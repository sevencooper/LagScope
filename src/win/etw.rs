//! Minimal real-time ETW consumer on top of windows-sys.
//!
//! Sessions use the QPC clock (ClientContext = 1) and are consumed with raw timestamps so
//! event times line up exactly with `clock::raw_now()`. Event payloads are decoded with
//! TDH once per (provider, event id, version) into a fixed-offset `Schema`, which keeps
//! the per-event cost to a hash lookup and a few slice reads.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows_sys::Win32::System::Diagnostics::Etw::*;
use windows_sys::core::GUID;

use super::wide;

pub struct ProviderSpec {
    pub name: &'static str,
    pub guid: GUID,
    pub level: u8,
    pub any_keyword: u64,
    /// If non-empty, ask ETW to deliver only these event ids (falls back to unfiltered).
    pub event_ids: Vec<u16>,
}

/// Borrowed view of one event inside the callback.
pub struct Event<'a> {
    pub rec: &'a EVENT_RECORD,
}

impl<'a> Event<'a> {
    pub fn id(&self) -> u16 {
        self.rec.EventHeader.EventDescriptor.Id
    }
    pub fn version(&self) -> u8 {
        self.rec.EventHeader.EventDescriptor.Version
    }
    pub fn opcode(&self) -> u8 {
        self.rec.EventHeader.EventDescriptor.Opcode
    }
    pub fn pid(&self) -> u32 {
        self.rec.EventHeader.ProcessId
    }
    pub fn tid(&self) -> u32 {
        self.rec.EventHeader.ThreadId
    }
    /// Raw QPC timestamp.
    pub fn timestamp(&self) -> i64 {
        self.rec.EventHeader.TimeStamp
    }
    pub fn provider(&self) -> GUID {
        self.rec.EventHeader.ProviderId
    }
    pub fn cpu(&self) -> u8 {
        unsafe { self.rec.BufferContext.Anonymous.Anonymous.ProcessorNumber }
    }
    pub fn data(&self) -> &'a [u8] {
        if self.rec.UserData.is_null() || self.rec.UserDataLength == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.rec.UserData as *const u8, self.rec.UserDataLength as usize) }
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub in_type: u16,
    pub out_type: u16,
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub event_name: String,
    pub task_name: String,
    pub message: String,
    pub fields: Vec<Field>,
    /// True if decoding stopped early at a variable-length field.
    pub truncated: bool,
    /// All top-level properties, for the slow sequential decoder (`values`).
    pub props: Vec<Prop>,
    pub ptr_size: usize,
}

#[derive(Debug, Clone)]
pub struct Prop {
    pub name: String,
    pub in_type: u16,
    pub flags: i32,
    pub count: usize,
    pub length: usize,
    pub param_index: [u16; 2], // [count index, length index]
}

fn fixed_size(in_type: u16, ptr_size: usize) -> Option<usize> {
    Some(match in_type {
        3 | 4 => 1,
        5 | 6 => 2,
        7 | 8 | 11 | 13 | 20 => 4,
        9 | 10 | 12 | 17 | 21 => 8,
        15 | 18 => 16,
        16 => ptr_size,
        _ => return None,
    })
}

impl Schema {
    /// Sequentially decodes all properties as display strings, including null-terminated
    /// and length-prefixed strings. Slower than `field` lookups; meant for rare events.
    pub fn values(&self, d: &[u8]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut nums: Vec<u64> = Vec::new();
        let mut off = 0usize;
        for p in &self.props {
            if p.in_type == 0xffff || off > d.len() {
                break;
            }
            let count = if p.flags & PropertyParamCount != 0 {
                nums.get(p.param_index[0] as usize).copied().unwrap_or(1) as usize
            } else {
                p.count.max(1)
            };
            let length = if p.flags & PropertyParamLength != 0 {
                nums.get(p.param_index[1] as usize).copied().unwrap_or(0) as usize
            } else {
                p.length
            };
            let rest = &d[off..];
            let (text, used, num) = match p.in_type {
                1 => {
                    // UTF-16, null-terminated unless a length is given
                    let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    let n = if length > 0 { length.min(units.len()) } else { units.iter().position(|&u| u == 0).unwrap_or(units.len()) };
                    let used = if length > 0 { n * 2 } else { (n + 1).min(units.len()) * 2 };
                    (String::from_utf16_lossy(&units[..n]), used, 0)
                }
                2 => {
                    let n = if length > 0 { length.min(rest.len()) } else { rest.iter().position(|&b| b == 0).unwrap_or(rest.len()) };
                    let used = if length > 0 { n } else { (n + 1).min(rest.len()) };
                    (String::from_utf8_lossy(&rest[..n]).to_string(), used, 0)
                }
                14 => {
                    let n = length.min(rest.len());
                    (rest[..n].iter().map(|b| format!("{b:02x}")).collect::<String>(), n, 0)
                }
                t => match fixed_size(t, self.ptr_size) {
                    Some(sz) => {
                        let total = (sz * count).min(rest.len());
                        let first = &rest[..sz.min(rest.len())];
                        let mut v = 0u64;
                        for (i, b) in first.iter().enumerate().take(8) {
                            v |= (*b as u64) << (8 * i);
                        }
                        let text = if t == 15 && first.len() == 16 {
                            first.iter().map(|b| format!("{b:02x}")).collect()
                        } else if t == 7 {
                            (v as u32 as i32).to_string()
                        } else if t == 9 {
                            (v as i64).to_string()
                        } else {
                            v.to_string()
                        };
                        (text, total, v)
                    }
                    None => break,
                },
            };
            nums.push(num);
            out.push((p.name.clone(), text));
            off += used;
        }
        out
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))
    }

    pub fn bytes<'d>(&self, data: &'d [u8], name: &str) -> Option<&'d [u8]> {
        let f = self.field(name)?;
        data.get(f.offset..f.offset + f.size)
    }

    pub fn uint(&self, data: &[u8], name: &str) -> Option<u64> {
        let b = self.bytes(data, name)?;
        Some(match b.len() {
            1 => b[0] as u64,
            2 => u16::from_le_bytes([b[0], b[1]]) as u64,
            4 => u32::from_le_bytes(b.try_into().ok()?) as u64,
            8 => u64::from_le_bytes(b.try_into().ok()?),
            _ => return None,
        })
    }
}

/// Caches TDH-derived schemas keyed by provider/event/version.
#[derive(Default)]
pub struct SchemaCache {
    map: HashMap<(u128, u16, u8), Option<Arc<Schema>>>,
}

impl SchemaCache {
    pub fn get(&mut self, ev: &Event) -> Option<Arc<Schema>> {
        let g = ev.provider();
        let key = (guid_u128(&g), ev.id(), ev.version());
        self.map
            .entry(key)
            .or_insert_with(|| decode_schema(ev.rec).map(Arc::new))
            .clone()
    }
}

fn guid_u128(g: &GUID) -> u128 {
    ((g.data1 as u128) << 96)
        | ((g.data2 as u128) << 80)
        | ((g.data3 as u128) << 64)
        | u64::from_be_bytes(g.data4) as u128
}

unsafe fn wstr_at(base: *const u8, off: u32) -> String {
    if off == 0 {
        return String::new();
    }
    unsafe {
        let mut p = base.add(off as usize) as *const u16;
        let mut v = Vec::new();
        while *p != 0 && v.len() < 512 {
            v.push(*p);
            p = p.add(1);
        }
        String::from_utf16_lossy(&v)
    }
}

fn decode_schema(rec: &EVENT_RECORD) -> Option<Schema> {
    unsafe {
        let mut size = 0u32;
        let r = TdhGetEventInformation(rec, 0, std::ptr::null(), std::ptr::null_mut(), &mut size);
        if r != ERROR_INSUFFICIENT_BUFFER || size == 0 {
            return None;
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let info = buf.as_mut_ptr() as *mut TRACE_EVENT_INFO;
        if TdhGetEventInformation(rec, 0, std::ptr::null(), info, &mut size) != ERROR_SUCCESS {
            return None;
        }
        let base = buf.as_ptr() as *const u8;
        let info = &*info;
        let ptr_size = if rec.EventHeader.Flags as u32 & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 { 4 } else { 8 };
        let props = std::slice::from_raw_parts(
            info.EventPropertyInfoArray.as_ptr(),
            info.PropertyCount as usize,
        );
        let mut schema = Schema {
            event_name: wstr_at(base, info.Anonymous1.EventNameOffset),
            task_name: wstr_at(base, info.TaskNameOffset),
            message: wstr_at(base, info.EventMessageOffset),
            ..Default::default()
        };
        schema.ptr_size = ptr_size;
        for p in props.iter().take(info.TopLevelPropertyCount as usize) {
            schema.props.push(Prop {
                name: wstr_at(base, p.NameOffset),
                in_type: if p.Flags & PropertyStruct != 0 { 0xffff } else { p.Anonymous1.nonStructType.InType },
                flags: p.Flags,
                count: p.Anonymous2.count as usize,
                length: p.Anonymous3.length as usize,
                param_index: [p.Anonymous2.countPropertyIndex, p.Anonymous3.lengthPropertyIndex],
            });
        }
        let mut off = 0usize;
        for p in props.iter().take(info.TopLevelPropertyCount as usize) {
            let flags = p.Flags;
            if flags & (PropertyStruct | PropertyParamCount | PropertyParamLength) != 0 {
                schema.truncated = true;
                break;
            }
            let nt = p.Anonymous1.nonStructType;
            let count = (p.Anonymous2.count as usize).max(1);
            let length = p.Anonymous3.length as usize;
            let elem = match nt.InType {
                3 | 4 => 1,                  // INT8/UINT8
                5 | 6 => 2,                  // INT16/UINT16
                7 | 8 | 11 | 13 | 20 => 4,   // INT32/UINT32/FLOAT/BOOLEAN/HEXINT32
                9 | 10 | 12 | 17 | 21 => 8,  // INT64/UINT64/DOUBLE/FILETIME/HEXINT64
                15 | 18 => 16,               // GUID/SYSTEMTIME
                16 => ptr_size,              // POINTER
                14 if length > 0 => length,  // BINARY with fixed length (e.g. IPv6 address)
                _ => {
                    schema.truncated = true;
                    break;
                }
            };
            let size = if nt.InType == 14 { elem } else { elem * count };
            schema.fields.push(Field {
                name: wstr_at(base, p.NameOffset),
                offset: off,
                size,
                in_type: nt.InType,
                out_type: nt.OutType,
            });
            off += size;
        }
        Some(schema)
    }
}

type Callback = Box<dyn FnMut(&Event) + Send>;

struct Ctx {
    cb: Callback,
}

unsafe extern "system" fn trampoline(rec: *mut EVENT_RECORD) {
    unsafe {
        let ctx = (*rec).UserContext as *mut Ctx;
        if ctx.is_null() {
            return;
        }
        let ev = Event { rec: &*rec };
        let cb = &mut (*ctx).cb;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&ev)));
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SessionStats {
    pub events_lost: u32,
    pub rt_buffers_lost: u32,
    pub buffers_written: u32,
}

pub struct RealtimeSession {
    name: Vec<u16>,
    props: Vec<u64>,
    handle: CONTROLTRACE_HANDLE,
    trace: PROCESSTRACE_HANDLE,
    thread: Option<JoinHandle<()>>,
    ctx: *mut Ctx,
    pub filtered: Vec<(&'static str, bool)>,
}

// The raw pointers are only touched from stop()/drop on the owning thread after the
// ProcessTrace thread has been joined.
unsafe impl Send for RealtimeSession {}

const NAME_SPACE: usize = 1024;

fn new_props(name: &[u16]) -> Vec<u64> {
    let base = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    let total = base + NAME_SPACE * 2 + 1024;
    let mut buf = vec![0u64; total.div_ceil(8)];
    unsafe {
        let p = &mut *(buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES);
        p.Wnode.BufferSize = (buf.len() * 8) as u32;
        p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        p.Wnode.ClientContext = 1; // QPC timestamps
        p.BufferSize = 64; // KB
        p.MinimumBuffers = 16;
        p.MaximumBuffers = 128;
        p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        p.FlushTimer = 1;
        p.LoggerNameOffset = base as u32;
        let dst = (buf.as_mut_ptr() as *mut u8).add(base) as *mut u16;
        std::ptr::copy_nonoverlapping(name.as_ptr(), dst, name.len().min(NAME_SPACE - 1));
    }
    buf
}

/// Stops a session by name if it exists (e.g. left over from a crashed run).
pub fn stop_by_name(name: &str) {
    let w = wide(name);
    let mut props = new_props(&w);
    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE { Value: 0 },
            w.as_ptr(),
            props.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

/// Kernel ("classic") event groups for `RealtimeSession::start_kernel`.
pub const KERNEL_FLAG_IMAGE_LOAD: u32 = EVENT_TRACE_FLAG_IMAGE_LOAD;
pub const KERNEL_FLAG_DPC: u32 = EVENT_TRACE_FLAG_DPC;
pub const KERNEL_FLAG_INTERRUPT: u32 = EVENT_TRACE_FLAG_INTERRUPT;
pub const KERNEL_FLAG_DISK_IO: u32 = EVENT_TRACE_FLAG_DISK_IO;
pub const KERNEL_FLAG_HARD_FAULTS: u32 = EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS;

impl RealtimeSession {
    /// A private kernel session (system logger mode, Windows 8+) delivering the kernel
    /// event groups in `flags` (DPC/ISR timings, image loads, ...). Several of these can
    /// run at once, unlike the single global "NT Kernel Logger".
    pub fn start_kernel(name: &str, flags: u32, cb: impl FnMut(&Event) + Send + 'static) -> Result<Self> {
        let wname = wide(name);
        let make = || {
            let mut p = new_props(&wname);
            unsafe {
                let pr = &mut *(p.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES);
                pr.LogFileMode |= EVENT_TRACE_SYSTEM_LOGGER_MODE;
                pr.EnableFlags = flags;
                // Kernel events can be very frequent: more and bigger buffers.
                pr.BufferSize = 256;
                pr.MinimumBuffers = 32;
                pr.MaximumBuffers = 256;
            }
            p
        };
        let (handle, props) = start_session(name, &wname, make)?;
        Self::consume(name, wname, props, handle, Vec::new(), cb)
    }

    pub fn start(name: &str, providers: &[ProviderSpec], cb: impl FnMut(&Event) + Send + 'static) -> Result<Self> {
        let wname = wide(name);
        let (handle, props) = start_session(name, &wname, || new_props(&wname))?;

        let mut filtered = Vec::new();
        for p in providers {
            let mut ok = false;
            if !p.event_ids.is_empty() {
                ok = enable_provider(handle, p, true) == ERROR_SUCCESS;
            }
            if !ok {
                let r = enable_provider(handle, p, false);
                if r != ERROR_SUCCESS {
                    let mut tmp = props.clone();
                    unsafe { ControlTraceW(handle, std::ptr::null(), tmp.as_mut_ptr() as *mut _, EVENT_TRACE_CONTROL_STOP) };
                    bail!("EnableTraceEx2({}) failed: error {r}", p.name);
                }
            }
            filtered.push((p.name, ok));
        }

        Self::consume(name, wname, props, handle, filtered, cb)
    }

    fn consume(
        name: &str,
        wname: Vec<u16>,
        props: Vec<u64>,
        handle: CONTROLTRACE_HANDLE,
        filtered: Vec<(&'static str, bool)>,
        cb: impl FnMut(&Event) + Send + 'static,
    ) -> Result<Self> {
        let ctx = Box::into_raw(Box::new(Ctx { cb: Box::new(cb) }));
        let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        logfile.LoggerName = wname.as_ptr() as *mut u16;
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(trampoline);
        logfile.Context = ctx as *mut c_void;
        let trace = unsafe { OpenTraceW(&mut logfile) };
        if trace.Value == u64::MAX || trace.Value == 0xFFFF_FFFF {
            unsafe {
                let mut tmp = props.clone();
                ControlTraceW(handle, std::ptr::null(), tmp.as_mut_ptr() as *mut _, EVENT_TRACE_CONTROL_STOP);
                drop(Box::from_raw(ctx));
            }
            bail!("OpenTrace({name}) failed: error {}", std::io::Error::last_os_error());
        }
        let tv = trace.Value;
        let thread = std::thread::Builder::new()
            .name(format!("etw-{name}"))
            .spawn(move || unsafe {
                let h = PROCESSTRACE_HANDLE { Value: tv };
                ProcessTrace(&h, 1, std::ptr::null(), std::ptr::null());
            })?;
        Ok(RealtimeSession { name: wname, props, handle, trace, thread: Some(thread), ctx, filtered })
    }

    pub fn stats(&mut self) -> SessionStats {
        let mut tmp = new_props(&self.name);
        let r = unsafe {
            ControlTraceW(self.handle, std::ptr::null(), tmp.as_mut_ptr() as *mut _, EVENT_TRACE_CONTROL_QUERY)
        };
        if r != ERROR_SUCCESS {
            return SessionStats::default();
        }
        let p = unsafe { &*(tmp.as_ptr() as *const EVENT_TRACE_PROPERTIES) };
        SessionStats { events_lost: p.EventsLost, rt_buffers_lost: p.RealTimeBuffersLost, buffers_written: p.BuffersWritten }
    }

    pub fn stop(&mut self) {
        if self.thread.is_none() {
            return;
        }
        unsafe {
            ControlTraceW(self.handle, std::ptr::null(), self.props.as_mut_ptr() as *mut _, EVENT_TRACE_CONTROL_STOP);
            CloseTrace(self.trace);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe { drop(Box::from_raw(self.ctx)) };
        self.ctx = std::ptr::null_mut();
    }
}

/// StartTrace with one retry after stopping a leftover session of the same name.
fn start_session(name: &str, wname: &[u16], make: impl Fn() -> Vec<u64>) -> Result<(CONTROLTRACE_HANDLE, Vec<u64>)> {
    stop_by_name(name);
    let mut props = make();
    let mut handle = CONTROLTRACE_HANDLE { Value: 0 };
    let mut r = unsafe { StartTraceW(&mut handle, wname.as_ptr(), props.as_mut_ptr() as *mut _) };
    if r == ERROR_ALREADY_EXISTS {
        stop_by_name(name);
        props = make();
        r = unsafe { StartTraceW(&mut handle, wname.as_ptr(), props.as_mut_ptr() as *mut _) };
    }
    if r != ERROR_SUCCESS {
        if r == 5 {
            bail!("StartTrace({name}) access denied - run as Administrator");
        }
        bail!("StartTrace({name}) failed: error {r}");
    }
    Ok((handle, props))
}

impl Drop for RealtimeSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn enable_provider(handle: CONTROLTRACE_HANDLE, p: &ProviderSpec, with_filter: bool) -> u32 {
    // EVENT_FILTER_EVENT_ID is a variable-length struct: header followed by u16 ids.
    let mut filter_buf: Vec<u16> = Vec::new();
    filter_buf.push(1); // FilterIn = TRUE (u8) + Reserved (u8) packed in one u16 (little endian)
    filter_buf.push(p.event_ids.len() as u16);
    filter_buf.extend_from_slice(&p.event_ids);
    let mut desc = EVENT_FILTER_DESCRIPTOR {
        Ptr: filter_buf.as_ptr() as u64,
        Size: (filter_buf.len() * 2) as u32,
        Type: EVENT_FILTER_TYPE_EVENT_ID,
    };
    let mut params: ENABLE_TRACE_PARAMETERS = unsafe { std::mem::zeroed() };
    params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
    if with_filter {
        params.EnableFilterDesc = &mut desc;
        params.FilterDescCount = 1;
    }
    unsafe {
        EnableTraceEx2(
            handle,
            &p.guid,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            p.level,
            p.any_keyword,
            0,
            0,
            &params,
        )
    }
}

pub const KERNEL_NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
pub const DXGI: GUID = GUID::from_u128(0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9);

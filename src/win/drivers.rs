//! Driver latency (LatencyMon-style): how long each driver's DPCs and interrupt handlers
//! (ISRs) run, from a private kernel ETW session. A long DPC/ISR holds up one CPU core
//! and everything queued behind it on that core - often network or audio processing -
//! so these are the classic "driver hiccups".
//!
//! Kernel events arrive at tens of thousands per second, so they are aggregated in the
//! callback: per driver and second (`drivers_1s.csv`), individually only when long
//! (`driver_long.csv`), and as a per-driver histogram for the whole session (meta.json).
//! Code addresses are mapped to drivers with the image-load events the session emits for
//! every loaded module at start (plus later loads).
//!
//! The same kernel session also records disk I/O (completion time and size) and hard page
//! faults of the game, aggregated per second into `disk_1s.csv` (slow I/Os individually in
//! `disk_slow.csv`), so frame hitches can be matched to asset streaming / paging.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use windows_sys::core::GUID;

use super::Running;
use super::etw::{self, Event, RealtimeSession};
use crate::app::App;
use crate::row;
use crate::session::Sink;


const PERFINFO: GUID = GUID::from_u128(0xce1dbfb4_137e_4da6_87b0_3f59aa102cbc);
const IMAGE: GUID = GUID::from_u128(0x2cb15d1d_5fc1_11d2_abe1_00a0c911f518);
const DISKIO: GUID = GUID::from_u128(0x3d6fa8d4_fe05_11d0_9dda_00c04fd7ba7c);
const PAGEFAULT: GUID = GUID::from_u128(0x3d6fa8d3_fe05_11d0_9dda_00c04fd7ba7c);
const OP_DISK_READ: u8 = 10;
const OP_DISK_WRITE: u8 = 11;
const OP_HARD_FAULT: u8 = 32;
/// Disk I/Os taking at least this long are written individually.
const SLOW_IO_MS: f64 = 50.0;
const OP_THREAD_DPC: u8 = 66;
const OP_ISR: u8 = 67;
const OP_DPC: u8 = 68;
const OP_TIMER_DPC: u8 = 69;

/// Histogram bucket upper bounds in microseconds (last bucket is open-ended).
pub const BUCKETS_US: [f64; 8] = [50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
/// DPC/ISR runs at least this long are written individually.
const LONG_US: f64 = 500.0;

#[derive(Default, Clone)]
struct Agg {
    dpc_n: u64,
    dpc_us: f64,
    dpc_max: f64,
    isr_n: u64,
    isr_us: f64,
    isr_max: f64,
    hist_dpc: [u64; 9],
    hist_isr: [u64; 9],
}

impl Agg {
    fn add(&mut self, isr: bool, us: f64) {
        let b = BUCKETS_US.iter().position(|&x| us < x).unwrap_or(BUCKETS_US.len());
        if isr {
            self.isr_n += 1;
            self.isr_us += us;
            self.isr_max = self.isr_max.max(us);
            self.hist_isr[b] += 1;
        } else {
            self.dpc_n += 1;
            self.dpc_us += us;
            self.dpc_max = self.dpc_max.max(us);
            self.hist_dpc[b] += 1;
        }
    }
}

#[derive(Default)]
struct DiskAgg {
    reads: u32,
    read_bytes: u64,
    writes: u32,
    write_bytes: u64,
    max_ms: f64,
    game_faults: u32,
    game_fault_bytes: u64,
}

struct Module {
    base: u64,
    end: u64,
    /// Index into `State::names`.
    name: usize,
}

struct State {
    modules: Vec<Module>,
    /// Routine address -> driver name index (0 = "unknown"). Hot path: one lookup per event.
    cache: HashMap<u64, usize>,
    names: Vec<String>,
    name_idx: HashMap<String, usize>,
    sec: i64,
    this_sec: HashMap<usize, Agg>,
    total: HashMap<usize, Agg>,
    /// Worst DPC/ISR in the last few seconds, for the console.
    recent: Vec<(i64, f64, usize)>,
    disk: DiskAgg,
    disk_sec: i64,
    /// Last flushed disk second: (second, read MB, write MB, slowest ms, game fault MB).
    disk_last: Option<(i64, f64, f64, f64, f64)>,
}

impl State {
    fn name_index(&mut self, name: &str) -> usize {
        if let Some(&i) = self.name_idx.get(name) {
            return i;
        }
        self.names.push(name.to_string());
        self.name_idx.insert(name.to_string(), self.names.len() - 1);
        self.names.len() - 1
    }

    fn driver_of(&mut self, routine: u64) -> usize {
        if let Some(&n) = self.cache.get(&routine) {
            return n;
        }
        let i = self.modules.partition_point(|m| m.base <= routine);
        let n = if i > 0 && routine < self.modules[i - 1].end { self.modules[i - 1].name } else { 0 };
        self.cache.insert(routine, n);
        n
    }

    fn add_module(&mut self, base: u64, size: u64, path: &str) {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_string();
        let i = self.modules.partition_point(|m| m.base < base);
        if self.modules.get(i).is_some_and(|m| m.base == base) {
            return;
        }
        let n = self.name_index(&name);
        self.modules.insert(i, Module { base, end: base.saturating_add(size.max(1)), name: n });
        // Addresses seen before this module was known may now resolve.
        self.cache.retain(|_, v| *v != 0);
    }
}


fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

fn u64_at(d: &[u8], off: usize) -> Option<u64> {
    d.get(off..off + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// FileName of an image event: a UTF-16 string after the fixed fields (offset differs a
/// little between event versions, so look for the one that starts like a path).
fn image_name(d: &[u8]) -> Option<String> {
    for off in [56usize, 48, 64, 40] {
        let Some(tail) = d.get(off..) else { continue };
        let w: Vec<u16> = tail.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        if w.len() > 3 && w[0] == b'\\' as u16 {
            return Some(String::from_utf16_lossy(&w));
        }
    }
    None
}

pub fn start(app: Arc<App>) -> Result<Running> {
    let per_sec = app.session.sink("drivers_1s", "t_us,driver,dpc_n,dpc_total_us,dpc_max_us,isr_n,isr_total_us,isr_max_us");
    let long = app.session.sink("driver_long", "t_us,kind,driver,dur_us,cpu");
    let disk_1s = app.session.sink("disk_1s", "t_us,reads,read_mb,writes,write_mb,max_io_ms,game_hard_faults,game_fault_mb");
    let disk_slow = app.session.sink("disk_slow", "t_us,kind,size_kb,ms,disk");
    let health = app.session.sink("etw_health_kernel", "t_us,session,events_lost,rt_buffers_lost,buffers_written");
    let state = Arc::new(Mutex::new(State {
        modules: Vec::new(),
        cache: HashMap::new(),
        names: vec!["unknown".to_string()],
        name_idx: HashMap::from([("unknown".to_string(), 0)]),
        sec: 0,
        this_sec: HashMap::new(),
        total: HashMap::new(),
        recent: Vec::new(),
        disk: DiskAgg::default(),
        disk_last: None,
        disk_sec: 0,
    }));
    let st = state.clone();
    let a = app.clone();
    let per_sec_cb = per_sec.clone();
    let disk_cb = disk_1s.clone();
    let mut schemas = super::etw::SchemaCache::default();
    let mut offsets: Vec<((bool, u8), [Option<(usize, usize)>; 3])> = Vec::new();
    let freq = app.session.clock.freq() as f64;
    let mut flags = etw::KERNEL_FLAG_DPC | etw::KERNEL_FLAG_INTERRUPT | etw::KERNEL_FLAG_IMAGE_LOAD | etw::KERNEL_FLAG_DISK_IO | etw::KERNEL_FLAG_HARD_FAULTS;
    if std::env::var_os("LAGSCOPE_NO_DISK").is_some() {
        flags &= !(etw::KERNEL_FLAG_DISK_IO | etw::KERNEL_FLAG_HARD_FAULTS);
    }
    let sess = RealtimeSession::start_kernel("LagScope-Kernel", flags, move |ev: &Event| {
        let g = ev.provider();
        let d = ev.data();
        // Cheap filtering first: most kernel events need no lock and no decoding.
        if same(&g, &DISKIO) || same(&g, &PAGEFAULT) {
            let op = ev.opcode();
            let disk = same(&g, &DISKIO) && matches!(op, OP_DISK_READ | OP_DISK_WRITE);
            let fault = !disk && same(&g, &PAGEFAULT) && op == OP_HARD_FAULT && a.is_game_pid(ev.pid());
            if !disk && !fault {
                return;
            }
            // Field offsets come from the event schema, resolved once per event layout.
            let key = (disk, ev.version());
            let off = match offsets.iter().find(|o| o.0 == key) {
                Some(o) => o.1,
                None => {
                    let Some(schema) = schemas.get(ev) else { return };
                    let f = |n: &str| schema.field(n).map(|f| (f.offset, f.size));
                    let o = if disk { [f("TransferSize"), f("HighResResponseTime"), f("DiskNumber")] } else { [f("ByteCount"), None, None] };
                    offsets.push((key, o));
                    o
                }
            };
            let read = |o: Option<(usize, usize)>| -> u64 {
                o.and_then(|(at, n)| d.get(at..at + n)).map(|b| b.iter().rev().fold(0u64, |acc, x| acc << 8 | *x as u64)).unwrap_or(0)
            };
            let t = a.session.clock.us_from_raw(ev.timestamp());
            let sec = t.div_euclid(1_000_000);
            let mut s = st.lock().unwrap();
            if sec != s.disk_sec {
                let prev = s.disk_sec;
                let last = flush_disk(&mut s.disk, prev, &disk_cb);
                s.disk_last = last.or(s.disk_last);
                s.disk_sec = sec;
            }
            if disk {
                let size = read(off[0]);
                let ms = read(off[1]) as f64 * 1000.0 / freq;
                if op == OP_DISK_READ {
                    s.disk.reads += 1;
                    s.disk.read_bytes += size;
                } else {
                    s.disk.writes += 1;
                    s.disk.write_bytes += size;
                }
                if ms < 60_000.0 {
                    s.disk.max_ms = s.disk.max_ms.max(ms);
                    if ms >= SLOW_IO_MS {
                        let start = t - (ms * 1000.0) as i64;
                        row!(disk_slow, "{start},{},{},{ms:.0},{}", if op == OP_DISK_READ { 'R' } else { 'W' }, size / 1024, read(off[2]));
                    }
                }
            } else {
                s.disk.game_faults += 1;
                s.disk.game_fault_bytes += read(off[0]);
            }
            return;
        }
        let mut s = st.lock().unwrap();
        if same(&g, &IMAGE) {
            // Load (10) and rundown of already loaded modules (3); kernel modules only.
            if matches!(ev.opcode(), 3 | 10) {
                if let (Some(base), Some(size), Some(name)) = (u64_at(d, 0), u64_at(d, 8), image_name(d)) {
                    if base >= 0xffff_8000_0000_0000 {
                        s.add_module(base, size, &name);
                    }
                }
            }
            return;
        }
        if !same(&g, &PERFINFO) {
            return;
        }
        let isr = match ev.opcode() {
            OP_ISR => true,
            OP_DPC | OP_THREAD_DPC | OP_TIMER_DPC => false,
            _ => return,
        };
        let (Some(initial), Some(routine)) = (u64_at(d, 0), u64_at(d, 8)) else { return };
        let end = ev.timestamp();
        let dur = end - initial as i64;
        if dur < 0 || dur as f64 > freq {
            return;
        }
        let us = dur as f64 * 1e6 / freq;
        let drv = s.driver_of(routine);
        let t_end = a.session.clock.us_from_raw(end);
        let t = a.session.clock.us_from_raw(initial as i64);
        let sec = t_end.div_euclid(1_000_000);
        if sec != s.sec {
            flush_second(&mut s, &per_sec_cb);
            s.sec = sec;
        }
        s.this_sec.entry(drv).or_default().add(isr, us);
        s.total.entry(drv).or_default().add(isr, us);
        if us >= LONG_US {
            row!(long, "{t},{},{},{us:.0},{}", if isr { 'I' } else { 'D' }, s.names[drv], ev.cpu());
        }
        if us >= 100.0 {
            s.recent.push((t_end, us, drv));
        }
    })?;
    let mut r = super::etw_running("drivers", app.clone(), sess, health);

    // Console line (worst run in the last 10 s) and the session summary at the end.
    let a = app.clone();
    let st = state.clone();
    let disk_on = flags & etw::KERNEL_FLAG_DISK_IO != 0;
    let live = super::spawn("drivers-live", move || {
        while a.sleep_ms(1000) {
            let now = a.session.clock.now_us();
            let mut s = st.lock().unwrap();
            s.recent.retain(|(t, _, _)| now - t < 10_000_000);
            let worst = s.recent.iter().copied().max_by(|x, y| x.1.partial_cmp(&y.1).unwrap());
            let known = s.modules.len();
            let disk = s.disk_last.filter(|d| now / 1_000_000 - d.0 <= 2);
            let txt = match worst {
                Some((_, us, d)) => format!("worst {:.2} ms ({})", us / 1000.0, s.names[d]),
                None if known == 0 => "waiting for driver list".into(),
                None => "all < 0.1 ms".into(),
            };
            drop(s);
            let mut l = a.live.lock().unwrap();
            l.drivers = Some(txt);
            l.drivers_worst_ms = worst.map(|w| w.1 / 1000.0).unwrap_or(0.0);
            if disk_on {
                l.disk = Some(match disk {
                    Some((_, r, w, ms, gm)) => format!(
                        "reads {r:.1} MB/s, writes {w:.1} MB/s, slowest {ms:.0} ms{}",
                        if gm >= 0.1 { format!(", game loading {gm:.1} MB/s") } else { String::new() }
                    ),
                    None => "idle".into(),
                });
            }
        }
    });
    let a = app.clone();
    r.add_thread(live);
    r.on_stopped(move || {
        let mut s = state.lock().unwrap();
        flush_second(&mut s, &per_sec);
        let ds = s.disk_sec;
        flush_disk(&mut s.disk, ds, &disk_1s);
        let mut rows: Vec<serde_json::Value> = s
            .total
            .iter()
            .map(|(&i, g)| {
                serde_json::json!({
                    "driver": s.names[i],
                    "dpc_n": g.dpc_n, "dpc_total_ms": g.dpc_us / 1000.0, "dpc_max_us": g.dpc_max,
                    "isr_n": g.isr_n, "isr_total_ms": g.isr_us / 1000.0, "isr_max_us": g.isr_max,
                    "hist_dpc": g.hist_dpc, "hist_isr": g.hist_isr,
                })
            })
            .collect();
        rows.sort_by(|x, y| {
            let m = |v: &serde_json::Value| v["dpc_max_us"].as_f64().unwrap_or(0.0).max(v["isr_max_us"].as_f64().unwrap_or(0.0));
            m(y).partial_cmp(&m(x)).unwrap()
        });
        a.session.meta_set("driver_latency", serde_json::json!({ "buckets_us": BUCKETS_US, "modules": s.modules.len(), "drivers": rows }));
    });
    Ok(r)
}

fn flush_disk(g: &mut DiskAgg, sec: i64, sink: &Sink) -> Option<(i64, f64, f64, f64, f64)> {
    if g.reads + g.writes + g.game_faults == 0 {
        return None;
    }
    let last = (sec, g.read_bytes as f64 / 1_048_576.0, g.write_bytes as f64 / 1_048_576.0, g.max_ms, g.game_fault_bytes as f64 / 1_048_576.0);
    row!(
        sink,
        "{},{},{:.2},{},{:.2},{:.1},{},{:.2}",
        sec * 1_000_000,
        g.reads,
        g.read_bytes as f64 / 1_048_576.0,
        g.writes,
        g.write_bytes as f64 / 1_048_576.0,
        g.max_ms,
        g.game_faults,
        g.game_fault_bytes as f64 / 1_048_576.0
    );
    *g = DiskAgg::default();
    Some(last)
}

/// Writes the finished second: a total row, plus drivers with a run of 100 us or more.
fn flush_second(s: &mut State, sink: &Sink) {
    if s.this_sec.is_empty() {
        return;
    }
    let t = s.sec * 1_000_000;
    let mut all = Agg::default();
    let mut rows: Vec<(usize, Agg)> = s.this_sec.drain().collect();
    rows.sort_by_key(|r| r.0);
    for (_, g) in &rows {
        all.dpc_n += g.dpc_n;
        all.dpc_us += g.dpc_us;
        all.dpc_max = all.dpc_max.max(g.dpc_max);
        all.isr_n += g.isr_n;
        all.isr_us += g.isr_us;
        all.isr_max = all.isr_max.max(g.isr_max);
    }
    let line = |name: &str, g: &Agg| {
        row!(sink, "{t},{name},{},{:.0},{:.0},{},{:.0},{:.0}", g.dpc_n, g.dpc_us, g.dpc_max, g.isr_n, g.isr_us, g.isr_max);
    };
    line("_all", &all);
    for (i, g) in &rows {
        if g.dpc_max >= 100.0 || g.isr_max >= 100.0 {
            line(&s.names[*i], g);
        }
    }
}

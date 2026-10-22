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
    }));
    let st = state.clone();
    let a = app.clone();
    let per_sec_cb = per_sec.clone();
    let freq = app.session.clock.freq() as f64;
    let flags = etw::KERNEL_FLAG_DPC | etw::KERNEL_FLAG_INTERRUPT | etw::KERNEL_FLAG_IMAGE_LOAD;
    let sess = RealtimeSession::start_kernel("LagScope-Kernel", flags, move |ev: &Event| {
        let g = ev.provider();
        let d = ev.data();
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
    let live = super::spawn("drivers-live", move || {
        while a.sleep_ms(1000) {
            let now = a.session.clock.now_us();
            let mut s = st.lock().unwrap();
            s.recent.retain(|(t, _, _)| now - t < 10_000_000);
            let worst = s.recent.iter().copied().max_by(|x, y| x.1.partial_cmp(&y.1).unwrap());
            let known = s.modules.len();
            let txt = match worst {
                Some((_, us, d)) => format!("worst {:.2} ms ({})", us / 1000.0, s.names[d]),
                None if known == 0 => "waiting for driver list".into(),
                None => "all < 0.1 ms".into(),
            };
            drop(s);
            let mut l = a.live.lock().unwrap();
            l.drivers = Some(txt);
            l.drivers_worst_ms = worst.map(|w| w.1 / 1000.0).unwrap_or(0.0);
        }
    });
    let a = app.clone();
    r.add_thread(live);
    r.on_stopped(move || {
        let mut s = state.lock().unwrap();
        flush_second(&mut s, &per_sec);
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

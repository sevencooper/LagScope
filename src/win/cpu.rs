//! CPU / memory / disk / per-process counters via PDH (English counter paths, so it works
//! on any Windows display language). Sampled at 4 Hz; GPU-engine counters (vendor
//! neutral, per process) at 1 Hz. Process counters come from the kernel's performance
//! data, no handle to the game process is opened.

use std::collections::BTreeMap;
use std::sync::Arc;

use windows_sys::Win32::System::Performance::*;

use super::{Running, from_wide, spawn, wide};
use crate::app::App;
use crate::row;

const PDH_FMT_NOCAP100: u32 = 0x8000;

struct Query {
    q: PDH_HQUERY,
}
unsafe impl Send for Query {}

impl Query {
    fn new() -> Option<Self> {
        let mut q: PDH_HQUERY = std::ptr::null_mut();
        (unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut q) } == 0).then_some(Query { q })
    }

    fn add(&self, path: &str) -> Option<PDH_HCOUNTER> {
        let mut c: PDH_HCOUNTER = std::ptr::null_mut();
        let w = wide(path);
        (unsafe { PdhAddEnglishCounterW(self.q, w.as_ptr(), 0, &mut c) } == 0).then_some(c)
    }

    fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.q) == 0 }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.q) };
    }
}

fn value(c: Option<PDH_HCOUNTER>) -> Option<f64> {
    let c = c?;
    let mut v: PDH_FMT_COUNTERVALUE = unsafe { std::mem::zeroed() };
    let r = unsafe { PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, std::ptr::null_mut(), &mut v) };
    (r == 0 && (v.CStatus == PDH_CSTATUS_VALID_DATA || v.CStatus == PDH_CSTATUS_NEW_DATA))
        .then(|| unsafe { v.Anonymous.doubleValue })
}

fn array(c: Option<PDH_HCOUNTER>) -> Vec<(String, f64)> {
    let Some(c) = c else { return vec![] };
    unsafe {
        let (mut size, mut count) = (0u32, 0u32);
        let r = PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &mut size, &mut count, std::ptr::null_mut());
        if r != PDH_MORE_DATA || size == 0 {
            return vec![];
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100, &mut size, &mut count, items) != 0 {
            return vec![];
        }
        std::slice::from_raw_parts(items, count as usize)
            .iter()
            .filter(|i| i.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA || i.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA)
            .map(|i| (from_wide(i.szName), i.FmtValue.Anonymous.doubleValue))
            .collect()
    }
}

fn f(v: Option<f64>, prec: usize) -> String {
    v.map(|x| format!("{x:.prec$}")).unwrap_or_default()
}

pub fn start(app: Arc<App>) -> Option<Running> {
    let a = app.clone();
    let t = spawn("cpu", move || sample_loop(a));
    Some(Running::threads("cpu", vec![t]))
}

fn sample_loop(app: Arc<App>) {
    let Some(q) = Query::new() else {
        app.session.event("collector", "cpu: PdhOpenQuery failed");
        return;
    };
    let pi = |c: &str| format!("\\Processor Information(_Total)\\{c}");
    let total = q.add(&pi("% Processor Time"));
    let util = q.add(&pi("% Processor Utility"));
    let dpc = q.add(&pi("% DPC Time"));
    let intr = q.add(&pi("% Interrupt Time"));
    let perf = q.add(&pi("% Processor Performance"));
    let freq = q.add(&pi("Processor Frequency"));
    let dpc_rate = q.add(&pi("DPCs Queued/sec"));
    let intr_rate = q.add(&pi("Interrupts/sec"));
    let cores = q.add("\\Processor Information(*)\\% Processor Time");
    let ctxsw = q.add("\\System\\Context Switches/sec");
    let queue = q.add("\\System\\Processor Queue Length");
    let avail = q.add("\\Memory\\Available MBytes");
    let pages_in = q.add("\\Memory\\Pages Input/sec");
    let disk_idle = q.add("\\PhysicalDisk(_Total)\\% Idle Time");
    let disk_lat = q.add("\\PhysicalDisk(_Total)\\Avg. Disk sec/Transfer");
    // How much the CPU is held back by power / thermal limits (0 = not at all).
    let perf_limit = q.add(&pi("% Performance Limit"));
    let commit = q.add("\\Memory\\% Committed Bytes In Use");

    let gq = Query::new();
    let gpu_eng = gq.as_ref().and_then(|g| g.add("\\GPU Engine(*)\\Utilization Percentage"));

    let sink = app.session.sink(
        "cpu",
        "t_us,total_pct,utility_pct,max_core_pct,dpc_pct,interrupt_pct,perf_pct,freq_mhz,dpc_rate,interrupt_rate,ctx_switch_rate,run_queue,avail_mb,pages_in_rate,disk_idle_pct,disk_ms_per_io,game_cpu_pct,game_private_mb,game_page_fault_rate,game_io_bps,game_threads,game_priority,perf_limit_pct,commit_pct",
    );
    let core_sink = app.session.sink("cpu_cores", "t_us,core,pct");
    let gpu_sink = app.session.sink("gpu_engines", "t_us,scope,engine,util_pct");
    let a = app;
    {
        q.collect();
        if let Some(g) = &gq {
            g.collect();
        }
        let mut tick = 0u64;
        while a.sleep_ms(250) {
            tick += 1;
            if !q.collect() {
                continue;
            }
            let t = a.session.clock.now_us();
            let per_core: Vec<(String, f64)> =
                array(cores).into_iter().filter(|(n, _)| !n.contains("_Total")).collect();
            let max_core = per_core.iter().map(|(_, v)| *v).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
            for (n, v) in &per_core {
                // Names are "group,number": quoted so the row keeps three columns.
                row!(core_sink, "{t},{},{v:.1}", crate::session::csv_escape(n));
            }
            // Game figures come from the process watcher (no per-process counters here:
            // PDH's Process object walks every process and was the costliest part).
            let gs = *a.game_stats.lock().unwrap();
            let game_cpu = gs.map(|g| g.cpu_pct);
            let avail_mb = value(avail);
            row!(
                sink,
                "{t},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                f(value(total), 1),
                f(value(util), 1),
                f(max_core, 1),
                f(value(dpc), 2),
                f(value(intr), 2),
                f(value(perf), 1),
                f(value(freq), 0),
                f(value(dpc_rate), 0),
                f(value(intr_rate), 0),
                f(value(ctxsw), 0),
                f(value(queue), 0),
                f(avail_mb, 0),
                f(value(pages_in), 1),
                f(value(disk_idle), 1),
                f(value(disk_lat).map(|s| s * 1000.0), 2),
                f(game_cpu, 1),
                f(gs.map(|g| g.private_mb), 0),
                f(gs.map(|g| g.page_faults_per_s), 0),
                f(gs.map(|g| g.io_bytes_per_s), 0),
                f(gs.map(|g| g.threads as f64), 0),
                f(gs.map(|g| g.priority as f64), 0),
                f(value(perf_limit), 1),
                f(value(commit), 1),
            );
            a.live.lock().unwrap().cpu = Some(format!(
                "CPU {}% (busiest core {}%), drivers {}%, {} MHz{}{}",
                f(value(total), 0),
                f(max_core, 0),
                f(value(dpc), 1),
                f(value(freq), 0),
                game_cpu.map(|g| format!(", game {g:.0}%")).unwrap_or_default(),
                avail_mb.map(|m| format!(", RAM free {:.1} GB", m / 1024.0)).unwrap_or_default()
            ));

            if tick % 4 == 0 {
                if let Some(g) = &gq {
                    if g.collect() {
                        gpu_engines(&a, &gpu_sink, t, array(gpu_eng));
                    }
                }
            }
        }
    }
}

/// Instance names look like `pid_1234_luid_0x00000000_0x0000D1E5_phys_0_eng_0_engtype_3D`.
fn gpu_engines(app: &App, sink: &crate::session::Sink, t: i64, items: Vec<(String, f64)>) {
    let mut per_adapter: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut game: BTreeMap<String, f64> = BTreeMap::new();
    for (name, v) in items {
        let parts: Vec<&str> = name.split('_').collect();
        let get = |k: &str| parts.iter().position(|p| *p == k).and_then(|i| parts.get(i + 1)).copied();
        let pid: u32 = get("pid").and_then(|p| p.parse().ok()).unwrap_or(0);
        let luid = parts.iter().position(|p| *p == "luid").map(|i| parts[i + 1..i + 3].join("_")).unwrap_or_default();
        let eng = name.rsplit("engtype_").next().unwrap_or("?").to_string();
        *per_adapter.entry((luid, eng.clone())).or_default() += v;
        if app.is_game_pid(pid) {
            *game.entry(eng).or_default() += v;
        }
    }
    // Busiest engine of the busiest adapter (and of the game), for cards NVML cannot read.
    let busiest = per_adapter.values().copied().fold(0.0, f64::max).min(100.0);
    let game_max = game.values().copied().fold(0.0, f64::max).min(100.0);
    app.live.lock().unwrap().gpu_engines = Some(format!("GPU {busiest:.0}%{}", if game.is_empty() { String::new() } else { format!(" (game {game_max:.0}%)") }));
    for ((luid, eng), v) in per_adapter {
        if v > 0.05 {
            row!(sink, "{t},adapter_{luid},{},{v:.1}", crate::session::csv_escape(&eng));
        }
    }
    for (eng, v) in game {
        row!(sink, "{t},game,{},{v:.1}", crate::session::csv_escape(&eng));
    }
}

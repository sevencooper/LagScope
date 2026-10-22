//! `lagscope` (default command): start every collector, show live status, and on exit
//! finalise the session (meta.json, event logs, report.html, zip).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{Running, console, cpu, frames, gpu, hotkey, net, probes, procwatch, spawn, stall, sysinfo, wifi};
use crate::app::{App, Config, LiveData};
use crate::session::Session;
use crate::ui::{self, BOLD, CYAN, DIM, GREEN, RED, RESET, YELLOW};

fn default_out_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .filter(|d| {
            // Make sure we can write next to the exe (it may live somewhere read-only).
            let probe = d.join(".lagscope_write_test");
            let ok = std::fs::write(&probe, b"x").is_ok();
            let _ = std::fs::remove_file(&probe);
            ok
        })
        .or_else(|| std::env::var("USERPROFILE").ok().map(|p| PathBuf::from(p).join("Documents")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Extra behaviour for special modes (the Wi-Fi check): a controller started after all
/// collectors (and stopped before them), and a step run after recording stops but before
/// the report is built.
pub struct Hooks {
    pub start: Box<dyn FnOnce(Arc<App>) -> Running + Send>,
    pub after_stop: Box<dyn FnOnce(&App) + Send>,
}

/// A running recording: all collectors started. Used by the console and the GUI.
pub struct Capture {
    pub app: Arc<App>,
    pub marker: Arc<hotkey::Marker>,
    running: Vec<Running>,
    flusher: Option<std::thread::JoinHandle<()>>,
    started: Instant,
    cpu_at_start: f64,
    after_stop: Option<Box<dyn FnOnce(&App) + Send>>,
}

/// What a finished recording produced.
#[derive(Debug, Clone, Default)]
pub struct Finished {
    pub dir: PathBuf,
    pub report: Option<PathBuf>,
    pub zip: Option<PathBuf>,
    pub headline: String,
    /// Too short: nothing was kept.
    pub discarded: bool,
}

pub fn default_out(cfg: &Config) -> PathBuf {
    cfg.out_dir.clone().unwrap_or_else(default_out_dir)
}

/// Creates the session and starts every collector. `console_input`: read M/Q keys from the
/// console window (console mode only).
pub fn start(cfg: Config, hooks: Option<Hooks>, console_input: bool) -> Result<Capture> {
    let base = default_out(&cfg);
    let session = Session::create(&base)?;
    let elevated = super::is_elevated();
    ui::log(format!("{BOLD}LagScope {}{RESET} - lag diagnostics recorder", env!("CARGO_PKG_VERSION")));
    ui::log(format!("Recording to {}", session.dir.display()));
    if !elevated {
        ui::log(format!("{RED}Not running as Administrator: network and frame tracing will be unavailable.{RESET}"));
        ui::log(format!("{RED}Right-click lagscope.exe > Run as administrator for full diagnostics.{RESET}"));
    }
    session.meta_set("elevated", elevated.into());
    session.meta_set(
        "config",
        serde_json::json!({
            "processes": cfg.processes, "hotkey": cfg.hotkey, "probe_hz": cfg.probe_hz,
            "probes": cfg.probes, "extra_targets": cfg.extra_targets, "duration_s": cfg.duration_s,
            "wifi_check": cfg.wifi_check, "kernel_trace": cfg.kernel_trace,
        }),
    );
    session.meta_set("profile", serde_json::to_value(&cfg.profile).unwrap_or_default());
    session.meta_set("logical_cpus", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0).into());
    session.meta_set("ui", (if console_input { "console" } else { "gui" }).into());

    let app = Arc::new(App::new(cfg, session));
    let cpu_at_start = process_cpu_secs();

    let mut running: Vec<Running> = Vec::new();
    let mut add = |name: &str, r: Result<Running>| match r {
        Ok(r) => {
            app.set_collector(name, "ok");
            running.push(r)
        }
        Err(e) => {
            app.set_collector(name, "FAILED");
            app.session.event("collector", format!("{name}: {e:#}"));
            app.session.meta_push("collector_errors", format!("{name}: {e:#}").into());
        }
    };

    add("procwatch", Ok(procwatch::start(app.clone())));
    add("net", net::start(app.clone()));
    add("frames", frames::start(app.clone()));
    if app.cfg.probes {
        add("probes", Ok(probes::start(app.clone())));
    }
    add("wifi", wifi::start(app.clone()).ok_or_else(|| anyhow::anyhow!("unavailable")));
    add("wlan_etw", super::wlan_etw::start(app.clone()));
    add("wifi_driver", super::wlan_etw::start_ndis(app.clone()));
    add("stall", Ok(stall::start(app.clone())));
    if app.cfg.kernel_trace {
        add("drivers", super::drivers::start(app.clone()));
    } else {
        app.session.event("collector", "drivers: driver & disk tracing turned off");
    }
    add("cpu", cpu::start(app.clone()).ok_or_else(|| anyhow::anyhow!("unavailable")));
    add("gpu", gpu::start(app.clone()).ok_or_else(|| anyhow::anyhow!("unavailable")));
    let marker = hotkey::Marker::new(app.clone());
    add("hotkey", hotkey::start(app.clone(), marker.clone()));
    if console_input {
        add("console", Ok(console::start_input(app.clone(), marker.clone())));
    }
    add("sysinfo", Ok(sysinfo::start(app.clone())));
    let after_stop = hooks.map(|h| {
        add("wifi_check", Ok((h.start)(app.clone())));
        h.after_stop
    });

    // Flusher: push CSV buffers to disk every second.
    let a = app.clone();
    let flusher = spawn("flusher", move || {
        while a.sleep_ms(1000) {
            a.session.flush_all();
        }
    });

    if app.cfg.wifi_check.is_none() {
        let how = if console_input { "press {} (or M in this window) at every lag spike; Q to stop and build the report" } else { "press {} at every lag spike" };
        app.session.event("start", how.replace("{}", &app.cfg.hotkey));
    }
    Ok(Capture { app, marker, running, flusher: Some(flusher), started: Instant::now(), cpu_at_start, after_stop })
}

impl Capture {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Applies the duration limit. Returns true while still recording.
    pub fn tick(&self) -> bool {
        if !self.app.stopping() && self.app.cfg.duration_s.is_some_and(|s| self.started.elapsed() >= Duration::from_secs(s)) {
            self.app.session.event("stop", "duration reached");
            self.app.stop.store(true, Ordering::SeqCst);
        }
        !self.app.stopping()
    }

    pub fn request_stop(&self, why: &str) {
        if !self.app.stopping() {
            self.app.session.event("user", why);
            self.app.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Stops the collectors, runs the after-stop step and builds the report and zip.
    pub fn finish(mut self) -> Finished {
        self.app.stop.store(true, Ordering::SeqCst);
        for r in self.running.iter_mut().rev() {
            r.stop();
        }
        if let Some(f) = self.flusher.take() {
            let _ = f.join();
        }
        if let Some(f) = self.after_stop.take() {
            f(&self.app);
        }
        let cpu = process_cpu_secs() - self.cpu_at_start;
        finalize(&self.app, self.started.elapsed(), cpu)
    }
}

pub fn run(cfg: Config) -> Result<()> {
    run_with(cfg, None)
}

/// Console mode: live status block, M/Q keys, then the report.
pub fn run_with(cfg: Config, hooks: Option<Hooks>) -> Result<()> {
    console::setup();
    ui::activate();
    let cap = match start(cfg, hooks, true) {
        Ok(c) => c,
        Err(e) => {
            ui::deactivate();
            return Err(e);
        }
    };
    console::install_ctrl_handler(cap.app.clone());
    while cap.tick() {
        ui::render(&status_lines(&cap.app, cap.elapsed()));
        std::thread::sleep(Duration::from_millis(500));
    }
    ui::log("Stopping collectors ...");
    ui::render(&[]);
    ui::deactivate();
    cap.finish();
    console::FINISHED.store(true, Ordering::SeqCst);
    println!("\nPress any key to close this window.");
    console::wait_key();
    Ok(())
}

/// CPU seconds (user + kernel) used by this process so far.
fn process_cpu_secs() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    ft(k) + ft(u)
}

fn finalize(app: &App, elapsed: Duration, cpu_secs: f64) -> Finished {
    let s = &app.session;
    let mut out = Finished { dir: s.dir.clone(), ..Default::default() };
    // Opened and closed right away (e.g. from the menu): nothing worth keeping.
    if app.cfg.wifi_check.is_none() && elapsed < Duration::from_secs(15) && app.live.lock().unwrap().marks == 0 {
        s.close_all();
        let _ = std::fs::remove_dir_all(&s.dir);
        ui::log(format!("Stopped after {:.0} s - too short to analyse, nothing was saved.", elapsed.as_secs_f64()));
        out.discarded = true;
        return out;
    }
    let one = cpu_secs / elapsed.as_secs_f64().max(1.0) * 100.0;
    let all = one / std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    s.meta_set("tool_cpu_pct_of_one_core", ((one * 100.0).round() / 100.0).into());
    s.meta_set("tool_cpu_pct_of_machine", ((all * 1000.0).round() / 1000.0).into());
    ui::log(format!("LagScope's own CPU use: {all:.2}% of the machine ({one:.1}% of one core)"));
    s.meta_set("duration_s", elapsed.as_secs_f64().into());
    s.meta_set("marks", app.live.lock().unwrap().marks.into());
    s.event("stop", format!("session ended after {:.0}s", elapsed.as_secs_f64()));
    s.flush_all();
    let _ = s.write_meta();
    ui::log("Exporting event logs ...");
    sysinfo::export_event_logs(app);
    ui::log("Analysing ...");
    match crate::analyze::run(&s.dir) {
        Ok(summary) => {
            for l in summary.lines() {
                ui::log(l.to_string());
            }
            out.headline = summary.lines().find(|l| l.starts_with("== ")).map(|l| l.trim_matches(['=', ' ']).to_string()).unwrap_or_default();
            out.report = Some(s.dir.join("report.html"));
            ui::log(format!("{GREEN}Report: {}{RESET}", s.dir.join("report.html").display()));
        }
        Err(e) => ui::log(format!("{RED}Analysis failed: {e:#}{RESET} (raw data is still saved)")),
    }
    if app.cfg.zip {
        s.close_all();
        match crate::session::zip_dir(&s.dir) {
            Ok(z) => {
                ui::log(format!("{GREEN}{BOLD}Send this file: {}{RESET}", z.display()));
                out.zip = Some(z);
            }
            Err(e) => ui::log(format!("{RED}Zip failed: {e:#}{RESET}")),
        }
    }
    if out.report.is_some() && app.cfg.open_report {
        super::open_path(&s.dir.join("report.html"));
    }
    out
}

fn status_lines(app: &App, elapsed: Duration) -> Vec<String> {
    let l: LiveData = app.live.lock().unwrap().clone();
    let now = app.session.clock.now_us();
    let secs = elapsed.as_secs();
    let mut out = Vec::new();
    out.push(format!(
        "{DIM}----------------------------------------------------------------------------------{RESET}"
    ));
    let check = app.cfg.wifi_check.is_some();
    if check {
        out.push(format!(
            "{BOLD}WI-FI CHECK{RESET} {:02}:{:02}   {DIM}leave the PC alone until it finishes; [Q] aborts (settings are put back){RESET}",
            secs / 60,
            secs % 60
        ));
        if let Some(st) = app.check_status.lock().unwrap().clone() {
            out.push(st);
        }
    } else {
        out.push(format!(
            "{BOLD}REC{RESET} {:02}:{:02}:{:02}   marks: {BOLD}{}{RESET}   [{}] / [M] mark lag spike   [Q] stop & build report",
            secs / 3600,
            (secs / 60) % 60,
            secs % 60,
            l.marks,
            app.cfg.hotkey
        ));
    }
    let pid = app.game_pid.load(Ordering::Relaxed);
    if check {
    } else if pid == 0 {
        out.push(format!("Game:   {YELLOW}waiting for {}{RESET}", app.game_name()));
    } else {
        let server = app.server.lock().unwrap().map(|s| s.to_string()).unwrap_or("detecting...".into());
        let fresh = now - l.game_net_updated_us < 2_500_000;
        let net = if fresh {
            let gap = l.game_max_gap_ms;
            let col = if gap > 150.0 { RED } else if gap > 60.0 { YELLOW } else { GREEN };
            format!(
                "in {:.0} pkt/s {:.0} kbit/s  out {:.0} pkt/s  max gap {col}{gap:.0} ms{RESET}  jitter {:.1} ms",
                l.game_rx_pps, l.game_rx_kbps, l.game_tx_pps, l.game_jitter_ms
            )
        } else {
            format!("{DIM}no game traffic{RESET}")
        };
        out.push(format!("Game:   {} pid {pid}  server {CYAN}{server}{RESET}  {net}", app.game_name()));
        let frames = if now - l.frames_updated_us < 2_500_000 {
            let col = if l.max_frame_ms > 50.0 { RED } else if l.max_frame_ms > 25.0 { YELLOW } else { GREEN };
            format!("{:.0} fps  worst frame {col}{:.1} ms{RESET}", l.fps, l.max_frame_ms)
        } else {
            format!("{DIM}no frames (menu/alt-tabbed?){RESET}")
        };
        out.push(format!("Frames: {frames}"));
    }
    let mut ping = Vec::new();
    for name in ["gateway", "hop2", "internet", "server"] {
        if let Some(p) = l.probes.get(name) {
            let loss = if p.sent_10s > 0 { p.lost_10s as f64 * 100.0 / p.sent_10s as f64 } else { 0.0 };
            let col = if loss > 5.0 || p.max_ms_10s > 100.0 { RED } else if loss > 0.0 || p.max_ms_10s > 40.0 { YELLOW } else { GREEN };
            ping.push(format!(
                "{name} {col}{:.0}/{:.0}ms{}{RESET}",
                p.avg_ms_10s,
                p.max_ms_10s,
                if loss > 0.0 { format!(" {loss:.0}%loss") } else { String::new() }
            ));
        }
    }
    if !ping.is_empty() {
        out.push(format!("Ping:   {} {DIM}(avg/max 10s){RESET}", ping.join("  ")));
    }
    let wifi = match (&l.wifi, l.wifi_location_blocked, l.wifi_etw_quality) {
        (_, true, q) => format!(
            "link quality {} {YELLOW}(details need Location services ON){RESET}",
            q.map(|q| q.to_string()).unwrap_or("?".into())
        ),
        (Some(w), _, _) => w.clone(),
        (None, _, Some(q)) => format!("link quality {q}"),
        _ => "-".into(),
    };
    out.push(format!(
        "Wi-Fi:  {wifi}   stall max {:.1} ms{}",
        l.stall_max_ms,
        l.top_talker.map(|t| format!("   other traffic: {YELLOW}{t}{RESET}")).unwrap_or_default()
    ));
    out.push(format!(
        "System: {}   {}",
        l.cpu.unwrap_or_else(|| "-".into()),
        l.gpu.unwrap_or_else(|| "-".into())
    ));
    if let Some(d) = &l.drivers {
        out.push(format!("Drivers: DPC/ISR {d} {DIM}(last 10 s){RESET}"));
    }
    let failed: Vec<&String> = l.collectors.iter().filter(|(_, v)| v.as_str() != "ok").map(|(k, _)| k).collect();
    if !failed.is_empty() {
        out.push(format!("{RED}Unavailable: {}{RESET}", failed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    out
}

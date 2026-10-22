//! Desktop GUI (egui/eframe, OpenGL). A thin front-end over the same engine as the
//! console: recordings and the Wi-Fi check run on a worker thread; this file only shows
//! their state. To stay out of the way of games it repaints at most once a second while
//! something is running and not at all when idle or minimised.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{self, Color32, RichText};

use super::{capture, console, fixes, hotkey, wificheck};
use crate::app::{App, Config};
use crate::profile::{self, Profile};

const LOG_LINES: usize = 400;
const HISTORY: usize = 120;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Record,
    WifiCheck,
    Network,
    Sessions,
}

/// A background job of the Network tab.
struct Job<T> {
    running: bool,
    status: String,
    result: Option<Result<T, String>>,
}

impl<T> Default for Job<T> {
    fn default() -> Self {
        Job { running: false, status: String::new(), result: None }
    }
}

type JobRef<T> = Arc<Mutex<Job<T>>>;

/// Runs `f` on a thread, publishing its status and result into `job`.
fn spawn_job<T: Send + 'static>(job: &JobRef<T>, ctx: &egui::Context, f: impl FnOnce(&dyn Fn(String)) -> Result<T> + Send + 'static) {
    {
        let mut j = job.lock().unwrap();
        j.running = true;
        j.status = "starting ...".into();
        j.result = None;
    }
    let (j, c) = (job.clone(), ctx.clone());
    std::thread::spawn(move || {
        let j2 = j.clone();
        let c2 = c.clone();
        let status = move |s: String| {
            j2.lock().unwrap().status = s;
            c2.request_repaint();
        };
        let r = f(&status).map_err(|e| format!("{e:#}"));
        let mut g = j.lock().unwrap();
        g.running = false;
        g.result = Some(r);
        drop(g);
        c.request_repaint();
    });
}

enum Task {
    Idle,
    Preparing(&'static str),
    Running { app: Arc<App>, marker: Arc<hotkey::Marker>, started: Instant },
    Finishing,
    Done(capture::Finished),
    Failed(String),
}

impl Task {
    fn busy(&self) -> bool {
        matches!(self, Task::Preparing(_) | Task::Running { .. } | Task::Finishing)
    }
}

/// State shared with the worker thread and the prompt hook. (The log has its own lock,
/// so logging can never block on - or deadlock with - code holding this one.)
struct Shared {
    question: Option<(String, bool, mpsc::Sender<bool>)>,
    task: Task,
    stop: bool,
    /// Which tab started the task (results are shown there).
    task_tab: Tab,
    /// The running/last recording was started by auto-record (for the game: profile).
    auto_game: Option<Profile>,
}

/// Scripted run for automated testing: actions at fixed times, screenshots, metrics.
struct SelfTest {
    scenario: String,
    out: PathBuf,
    t0: Instant,
    step: usize,
    shots: Vec<String>,
    cpu0: f64,
    idle_cpu: Option<(f64, f64)>,
    wait_until: f64,
    q_seen: Option<f64>,
}

struct GuiApp {
    shared: Arc<Mutex<Shared>>,
    /// Auto-record on/off (read by the watcher thread), the tray icon and its events.
    auto_on: Arc<std::sync::atomic::AtomicBool>,
    cfg_shared: Arc<Mutex<Config>>,
    tray: Arc<Mutex<Option<super::tray::Tray>>>,
    tray_rx: mpsc::Receiver<super::tray::TrayEvent>,
    tray_tx: mpsc::Sender<super::tray::TrayEvent>,
    startup_task: bool,
    hidden: bool,
    exiting: bool,
    told_about_tray: bool,
    log: Arc<Mutex<VecDeque<String>>>,
    cfg: Config,
    profiles: Vec<Profile>,
    tab: Tab,
    show_log: bool,
    sessions: Vec<SessionRow>,
    sessions_loaded: bool,
    kept: Vec<fixes::Change>,
    // Live history (1 sample per second while recording).
    last_sample: Option<Instant>,
    h_router: VecDeque<f32>,
    h_gap: VecDeque<f32>,
    h_frame: VecDeque<f32>,
    h_driver: VecDeque<f32>,
    close_when_done: bool,
    selftest: Option<SelfTest>,
    /// Findings and suggestions of the last finished session (read once from analysis.json).
    summary: Option<(PathBuf, Vec<String>, Vec<String>)>,
    /// Re-analysis running in the background (sessions list reloads when it finishes).
    reanalyzing: Arc<std::sync::atomic::AtomicUsize>,
    net_router: JobRef<super::router::RouterInfo>,
    net_load: JobRef<super::loadtest::LoadResult>,
    net_scan: JobRef<super::wifiscan::ScanResult>,
    /// Report folder of the last saved network test.
    net_saved: Arc<Mutex<Option<PathBuf>>>,
    /// Sessions ticked for comparison (oldest first) and the comparison being shown.
    compare_pick: Vec<PathBuf>,
    comparison: Option<Result<crate::analyze::compare::Comparison, String>>,
}

struct SessionRow {
    dir: PathBuf,
    name: String,
    when: String,
    headline: String,
    game: String,
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn run(cfg: Config, selftest: Option<(String, PathBuf)>, start_hidden: bool) -> Result<()> {
    let log = Arc::new(Mutex::new(VecDeque::new()));
    let shared = Arc::new(Mutex::new(Shared { question: None, task: Task::Idle, stop: false, task_tab: Tab::Record, auto_game: None }));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("LagScope {}", env!("CARGO_PKG_VERSION")))
            .with_inner_size([980.0, 720.0])
            .with_min_inner_size([720.0, 520.0])
            .with_visible(!start_hidden),
        centered: true,
        ..Default::default()
    };
    let options = eframe::NativeOptions { viewport: options.viewport.clone().with_icon(icon()), ..options };
    let (profiles, errors) = profile::load_all(&profile::user_dirs());
    let sh = shared.clone();
    eframe::run_native(
        "LagScope",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            ctx.set_visuals(egui::Visuals::dark());
            let l1 = log.clone();
            let c1 = ctx.clone();
            crate::ui::set_sink(move |line| {
                let mut lg = l1.lock().unwrap();
                for l in strip_ansi(&line).lines() {
                    if lg.len() >= LOG_LINES {
                        lg.pop_front();
                    }
                    lg.push_back(l.to_string());
                }
                drop(lg);
                c1.request_repaint_after(Duration::from_millis(250));
            });
            let s2 = sh.clone();
            let c2 = ctx.clone();
            console::set_prompter(move |q, def| {
                let (tx, rx) = mpsc::channel();
                s2.lock().unwrap().question = Some((q.to_string(), def, tx));
                c2.request_repaint();
                rx.recv().unwrap_or(def)
            });
            for e in errors {
                crate::ui::log(format!("problem in profile file {e}"));
            }
            for (what, r) in fixes::restore_pending() {
                crate::ui::log(match r {
                    Ok(()) => format!("Restored from an earlier, interrupted check: {what}"),
                    Err(e) => format!("Could not restore '{what}': {e:#}"),
                });
            }
            let auto_on = Arc::new(std::sync::atomic::AtomicBool::new(start_hidden || profile::setting("auto_record").and_then(|v| v.as_bool()).unwrap_or(false)));
            let cfg_shared = Arc::new(Mutex::new(cfg.clone()));
            let tray = Arc::new(Mutex::new(None));
            let (tray_tx, tray_rx) = mpsc::channel();
            {
                let (s, c, on, cf, t) = (sh.clone(), ctx.clone(), auto_on.clone(), cfg_shared.clone(), tray.clone());
                let profs = profiles.clone();
                std::thread::Builder::new().name("auto-record".into()).spawn(move || auto_record_loop(s, c, on, cf, profs, t)).ok();
            }
            let selftest = selftest.map(|(scenario, out)| {
                let _ = std::fs::create_dir_all(&out);
                SelfTest { scenario, out, t0: Instant::now(), step: 0, shots: Vec::new(), cpu0: 0.0, idle_cpu: None, wait_until: 0.0, q_seen: None }
            });
            let mut app = GuiApp {
                shared: sh,
                auto_on,
                cfg_shared,
                tray,
                tray_rx,
                tray_tx,
                startup_task: startup_task_exists(),
                hidden: start_hidden,
                exiting: false,
                told_about_tray: start_hidden,
                log,
                cfg,
                profiles,
                tab: Tab::Record,
                show_log: false,
                sessions: Vec::new(),
                sessions_loaded: false,
                kept: fixes::kept(),
                last_sample: None,
                h_router: VecDeque::new(),
                h_gap: VecDeque::new(),
                h_frame: VecDeque::new(),
                h_driver: VecDeque::new(),
                close_when_done: false,
                selftest,
                summary: None,
                reanalyzing: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                net_router: Arc::default(),
                net_load: Arc::default(),
                net_scan: Arc::default(),
                net_saved: Arc::default(),
                compare_pick: Vec::new(),
                comparison: None,
            };
            if app.auto_on.load(std::sync::atomic::Ordering::Relaxed) {
                app.ensure_tray(&ctx);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

/// Starts a recording on a worker thread (Start button or auto-record).
fn start_recording(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, mut cfg: Config, auto: Option<Profile>) {
    cfg.wifi_check = None;
    cfg.open_report = false;
    if let Some(p) = &auto {
        cfg.profile = p.clone();
        cfg.processes = p.processes.clone();
    }
    let sh = shared.clone();
    let c = ctx.clone();
    {
        let mut s = sh.lock().unwrap();
        s.stop = false;
        s.task = Task::Preparing("Starting the recording ...");
        s.task_tab = Tab::Record;
        s.auto_game = auto;
    }
    std::thread::spawn(move || match capture::start(cfg, None, false) {
        Ok(cap) => drive(&sh, &c, cap),
        Err(e) => {
            sh.lock().unwrap().task = Task::Failed(format!("{e:#}"));
            c.request_repaint();
        }
    });
}

/// Auto-record: every 2 s, start recording when a game with a profile is running, and stop
/// ~20 s after it exits. Runs on its own thread; costs one process snapshot per tick.
fn auto_record_loop(shared: Arc<Mutex<Shared>>, ctx: egui::Context, enabled: Arc<std::sync::atomic::AtomicBool>, cfg: Arc<Mutex<Config>>, profiles: Vec<Profile>, tray: Arc<Mutex<Option<super::tray::Tray>>>) {
    use std::sync::atomic::Ordering;
    let mut gone_since: Option<Instant> = None;
    let mut announced_done = true;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let on = enabled.load(Ordering::Relaxed);
        let (busy, auto, done_headline) = {
            let s = shared.lock().unwrap();
            let done = match &s.task {
                Task::Done(f) => Some(if f.discarded { "Stopped quickly - nothing saved.".to_string() } else { f.headline.clone() }),
                _ => None,
            };
            (s.task.busy(), s.auto_game.clone(), done)
        };
        // Tell the user when an automatic recording has its report.
        if let (Some(p), Some(h)) = (&auto, &done_headline) {
            if !announced_done {
                announced_done = true;
                if let Some(t) = tray.lock().unwrap().as_ref() {
                    t.notify(&format!("{} - report ready", p.name), h);
                }
            }
        }
        let names: Vec<String> = if on || auto.is_some() { super::procwatch::snapshot().into_values().map(|n| n.to_ascii_lowercase()).collect() } else { Vec::new() };
        let running = |p: &Profile| p.processes.iter().any(|x| names.contains(&x.to_ascii_lowercase()));
        let tip = match (&auto, busy, on) {
            (Some(p), true, _) => format!("LagScope - recording {}", p.name),
            (_, true, _) => "LagScope - recording".to_string(),
            (_, false, true) => "LagScope - auto-record on (waiting for a game)".to_string(),
            _ => "LagScope".to_string(),
        };
        if let Some(t) = tray.lock().unwrap().as_ref() {
            t.set_state(&tip, on);
        }
        if busy {
            // Stop an automatic recording once its game has been gone for 20 s.
            if let Some(p) = &auto {
                if running(p) {
                    gone_since = None;
                } else if gone_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(20) {
                    shared.lock().unwrap().stop = true;
                    gone_since = None;
                    ctx.request_repaint();
                }
            }
            continue;
        }
        if !on {
            continue;
        }
        if let Some(p) = profiles.iter().find(|p| !p.is_auto() && running(p)) {
            crate::ui::log(format!("Auto-record: {} started, recording.", p.name));
            if let Some(t) = tray.lock().unwrap().as_ref() {
                t.notify("LagScope is recording", &format!("{} is running. Press {} at every lag spike.", p.name, cfg.lock().unwrap().hotkey));
            }
            announced_done = false;
            start_recording(&shared, &ctx, cfg.lock().unwrap().clone(), Some(p.clone()));
            ctx.request_repaint();
        }
    }
}

/// "Start with Windows": a scheduled task at logon with highest privileges (the only way to
/// start an administrator tool at logon without a UAC prompt).
const TASK: &str = "LagScope auto-record";

pub fn startup_task_exists() -> bool {
    super::sysinfo::run_env("schtasks.exe", &["/Query", "/TN", TASK], None, Duration::from_secs(10), &[]).contains(TASK)
}

pub fn set_startup_task(on: bool) -> Result<(), String> {
    let out = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let tr = format!("\"{}\" --tray", exe.display());
        super::sysinfo::run_env("schtasks.exe", &["/Create", "/TN", TASK, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST", "/F"], None, Duration::from_secs(15), &[])
    } else {
        super::sysinfo::run_env("schtasks.exe", &["/Delete", "/TN", TASK, "/F"], None, Duration::from_secs(15), &[])
    };
    if startup_task_exists() == on { Ok(()) } else { Err(out.trim().to_string()) }
}

/// Polls the running capture until it stops, then builds the report.
fn drive(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, cap: capture::Capture) {
    {
        let mut s = shared.lock().unwrap();
        s.task = Task::Running { app: cap.app.clone(), marker: cap.marker.clone(), started: Instant::now() };
    }
    ctx.request_repaint();
    loop {
        if shared.lock().unwrap().stop {
            cap.request_stop("stop requested (GUI)");
        }
        if !cap.tick() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    shared.lock().unwrap().task = Task::Finishing;
    ctx.request_repaint();
    let f = cap.finish();
    shared.lock().unwrap().task = Task::Done(f);
    ctx.request_repaint();
}

impl GuiApp {
    fn start_record(&mut self, ctx: &egui::Context) {
        self.reset_history();
        start_recording(&self.shared, ctx, self.cfg.clone(), None);
    }

    fn start_check(&mut self, ctx: &egui::Context, unattended: bool, force: bool, secs: u64) {
        let mut cfg = self.cfg.clone();
        cfg.wifi_check = Some(secs);
        cfg.unattended = unattended;
        cfg.force_tests = force;
        cfg.open_report = false;
        self.reset_history();
        let sh = self.shared.clone();
        let c = ctx.clone();
        {
            let mut s = sh.lock().unwrap();
            s.stop = false;
            s.task = Task::Preparing("Reading the Wi-Fi settings ...");
            s.task_tab = Tab::WifiCheck;
        }
        std::thread::spawn(move || {
            let r = wificheck::prepare(cfg).and_then(|p| match p {
                Some((cfg, hooks)) => capture::start(cfg, Some(hooks), false).map(Some),
                None => Ok(None),
            });
            match r {
                Ok(Some(cap)) => drive(&sh, &c, cap),
                Ok(None) => sh.lock().unwrap().task = Task::Idle,
                Err(e) => sh.lock().unwrap().task = Task::Failed(format!("{e:#}")),
            }
            c.request_repaint();
        });
    }

    /// Creates the tray icon (once).
    fn ensure_tray(&mut self, ctx: &egui::Context) {
        let mut t = self.tray.lock().unwrap();
        if t.is_some() {
            return;
        }
        let (tx, c) = (self.tray_tx.clone(), ctx.clone());
        *t = super::tray::Tray::start("LagScope", self.auto_on.load(std::sync::atomic::Ordering::Relaxed), move |ev| {
            let _ = tx.send(ev);
            c.request_repaint();
        });
    }

    fn set_auto(&mut self, ctx: &egui::Context, on: bool) {
        self.auto_on.store(on, std::sync::atomic::Ordering::Relaxed);
        profile::set_setting("auto_record", on.into());
        if on {
            self.ensure_tray(ctx);
        }
        crate::ui::log(format!("Auto-record {}", if on { "on" } else { "off" }));
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Tray events, window closing and the "close after finishing" step. Runs every frame and
    /// also while the window is hidden (eframe calls `logic` then).
    fn housekeeping(&mut self, ctx: &egui::Context) {
        use super::tray::TrayEvent;
        while let Ok(ev) = self.tray_rx.try_recv() {
            match ev {
                TrayEvent::Open => self.show_window(ctx),
                TrayEvent::ToggleAuto => {
                    let on = !self.auto_on.load(std::sync::atomic::Ordering::Relaxed);
                    self.set_auto(ctx, on);
                }
                TrayEvent::Exit => {
                    self.exiting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let busy = self.shared.lock().unwrap().task.busy();
        if ctx.input(|i| i.viewport().close_requested()) {
            let tray = self.tray.lock().unwrap().is_some() && self.auto_on.load(std::sync::atomic::Ordering::Relaxed);
            if tray && !self.exiting {
                // With auto-record on, closing the window keeps LagScope running in the tray.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.hidden = true;
                if !self.told_about_tray {
                    self.told_about_tray = true;
                    if let Some(t) = self.tray.lock().unwrap().as_ref() {
                        t.notify("LagScope keeps running", "Auto-record is on, so LagScope stays here in the tray. Right-click the icon to exit.");
                    }
                }
            } else if busy {
                // Closing while something runs: stop it, build the report, then close.
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.shared.lock().unwrap().stop = true;
                self.close_when_done = true;
            } else if let Some(t) = self.tray.lock().unwrap().as_ref() {
                t.remove();
            }
        }
        if self.close_when_done && !busy {
            if let Some(t) = self.tray.lock().unwrap().as_ref() {
                t.remove();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if busy && self.close_when_done {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn reset_history(&mut self) {
        self.last_sample = None;
        for h in [&mut self.h_router, &mut self.h_gap, &mut self.h_frame, &mut self.h_driver] {
            h.clear();
        }
    }

    /// One history sample per second while recording.
    fn sample(&mut self, app: &App) {
        if self.last_sample.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_sample = Some(Instant::now());
        let now = app.session.clock.now_us();
        let router = app
            .gw_samples
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take_while(|(t, _)| now - t < 1_200_000)
            .filter_map(|(_, r)| *r)
            .fold(f64::NAN, f64::max);
        let l = app.live.lock().unwrap();
        let fresh_net = now - l.game_net_updated_us < 2_500_000;
        let fresh_frames = now - l.frames_updated_us < 2_500_000;
        let vals = [
            router as f32,
            if fresh_net { l.game_max_gap_ms as f32 } else { f32::NAN },
            if fresh_frames { l.max_frame_ms as f32 } else { f32::NAN },
            l.drivers_worst_ms as f32,
        ];
        drop(l);
        for (h, v) in [&mut self.h_router, &mut self.h_gap, &mut self.h_frame, &mut self.h_driver].into_iter().zip(vals) {
            if h.len() >= HISTORY {
                h.pop_front();
            }
            h.push_back(v);
        }
    }

    fn load_sessions(&mut self) {
        self.sessions_loaded = true;
        self.sessions.clear();
        let base = capture::default_out(&self.cfg);
        let Ok(rd) = std::fs::read_dir(&base) else { return };
        let mut dirs: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("LagScope_")))
            .collect();
        dirs.sort();
        dirs.reverse();
        for dir in dirs.into_iter().take(60) {
            let meta: serde_json::Value = std::fs::read_to_string(dir.join("meta.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            let analysis: serde_json::Value = std::fs::read_to_string(dir.join("analysis.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            let game = if meta["wifi_check_plan"].is_array() {
                "Wi-Fi check".to_string()
            } else if meta["kind"] == "network_test" {
                "Network test".to_string()
            } else {
                meta["detected_games"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| profile::of_session(&meta).name)
            };
            self.sessions.push(SessionRow {
                name: dir.file_name().unwrap().to_string_lossy().to_string(),
                when: meta["start_local"].as_str().unwrap_or("").chars().take(16).collect(),
                headline: analysis["headline"].as_str().unwrap_or("(no report)").to_string(),
                game,
                dir,
            });
        }
    }

    // ---------- drawing ----------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("LagScope").strong());
            ui.label(RichText::new(env!("CARGO_PKG_VERSION")).weak());
            ui.add_space(16.0);
            for (t, name) in [(Tab::Record, "Record"), (Tab::WifiCheck, "Wi-Fi check"), (Tab::Network, "Network"), (Tab::Sessions, "Sessions")] {
                if ui.selectable_label(self.tab == t, RichText::new(name).size(15.0)).clicked() {
                    self.tab = t;
                    if t == Tab::Sessions {
                        self.load_sessions();
                    }
                    if t == Tab::WifiCheck {
                        self.kept = fixes::kept();
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.show_log, "Log");
            });
        });
    }

    fn record_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (busy, mine, running): (bool, bool, Option<(Arc<App>, Arc<hotkey::Marker>, Instant)>) = {
            let s = self.shared.lock().unwrap();
            let running = match &s.task {
                Task::Running { app, marker, started } if s.task_tab == Tab::Record => Some((app.clone(), marker.clone(), *started)),
                _ => None,
            };
            (s.task.busy(), s.task_tab == Tab::Record, running)
        };
        if let Some((app, marker, started)) = running {
            self.sample(&app);
            self.live_view(ui, &app, &marker, started);
            return;
        }
        ui.heading("Record while playing");
        ui.label("LagScope watches the game's network traffic, your Wi-Fi, pings, frame times and drivers while you play. Press the mark key whenever it feels laggy; afterwards it builds a report that says where each hiccup came from.");
        ui.add_space(8.0);
        egui::Grid::new("recopts").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Game");
            let cur = self.cfg.profile.name.clone();
            egui::ComboBox::from_id_salt("game").selected_text(cur).width(260.0).show_ui(ui, |ui| {
                for p in &self.profiles {
                    if ui.selectable_label(p.id == self.cfg.profile.id, &p.name).clicked() {
                        self.cfg.profile = p.clone();
                        self.cfg.processes = p.processes.clone();
                        profile::save_last_game(&p.id);
                    }
                }
            });
            ui.end_row();
            ui.label("Mark key");
            egui::ComboBox::from_id_salt("hotkey").selected_text(self.cfg.hotkey.clone()).show_ui(ui, |ui| {
                for k in ["F8", "F9", "F10", "F7", "Pause", "Ctrl+Shift+M"] {
                    ui.selectable_value(&mut self.cfg.hotkey, k.to_string(), k);
                }
            });
            ui.end_row();
        });
        if !self.cfg.profile.hint.is_empty() {
            ui.label(RichText::new(&self.cfg.profile.hint).weak());
        }
        ui.add_space(10.0);
        if ui.add_enabled(!busy, egui::Button::new(RichText::new("  Start recording  ").size(18.0))).clicked() {
            self.start_record(&ctx);
        }
        if mine {
            self.task_result(ui);
        }
        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("Automatic recording").strong());
        let mut on = self.auto_on.load(std::sync::atomic::Ordering::Relaxed);
        let games: Vec<&str> = self.profiles.iter().filter(|p| !p.is_auto()).map(|p| p.name.as_str()).collect();
        if ui.checkbox(&mut on, format!("Record automatically whenever one of these games runs: {}", games.join(", "))).changed() {
            self.set_auto(&ctx, on);
        }
        ui.label(RichText::new("LagScope then stays in the notification area (tray) when you close this window, records each game session and builds the report when the game exits.").weak());
        let mut st = self.startup_task;
        if ui.add_enabled(on || st, egui::Checkbox::new(&mut st, "Start LagScope with Windows (hidden in the tray, auto-record on)")).changed() {
            match set_startup_task(st) {
                Ok(()) => {
                    self.startup_task = st;
                    crate::ui::log(format!("Start with Windows: {}", if st { "on" } else { "off" }));
                }
                Err(e) => {
                    crate::ui::log(format!("Could not change 'start with Windows': {e}"));
                    self.show_log = true;
                }
            }
        }
    }

    /// Status of the last task (preparing / finishing / done / failed) on its own tab.
    fn task_result(&mut self, ui: &mut egui::Ui) {
        let s = self.shared.lock().unwrap();
        ui.add_space(12.0);
        match &s.task {
            Task::Preparing(what) => {
                // No animated spinner: it would repaint ~60 times a second.
                ui.label(RichText::new(format!("{what}  (working)")).italics());
            }
            Task::Finishing => {
                ui.label(RichText::new("Stopping and building the report ...  (working)").italics());
            }
            Task::Failed(e) => {
                ui.colored_label(Color32::LIGHT_RED, format!("Failed: {e}"));
            }
            Task::Done(f) => {
                let f = f.clone();
                drop(s);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    if f.discarded {
                        ui.label("Stopped after a few seconds: too short to analyse, nothing was saved.");
                        return;
                    }
                    ui.label(RichText::new(&f.headline).size(16.0).strong());
                    ui.label(RichText::new(f.dir.display().to_string()).weak());
                    ui.horizontal(|ui| {
                        if let Some(r) = &f.report {
                            if ui.button("Open report").clicked() {
                                super::open_path(r);
                            }
                        }
                        if ui.button("Open folder").clicked() {
                            super::open_path(&f.dir);
                        }
                    });
                    if let Some(z) = &f.zip {
                        ui.label(format!("To share it, send: {}", z.file_name().unwrap_or_default().to_string_lossy()));
                    }
                });
                if self.summary.as_ref().is_none_or(|x| x.0 != f.dir) {
                    let a: serde_json::Value = std::fs::read_to_string(f.dir.join("analysis.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
                    let list = |k: &str| a[k].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                    self.summary = Some((f.dir.clone(), list("findings"), list("recommendations")));
                }
                if let Some((_, findings, recs)) = &self.summary {
                    if !recs.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new("Suggestions").strong());
                        for r in recs {
                            ui.add(egui::Label::new(format!("• {}", r.replace('`', ""))).wrap());
                        }
                    }
                    if !findings.is_empty() {
                        ui.add_space(6.0);
                        egui::CollapsingHeader::new(format!("Findings ({})", findings.len())).id_salt("findings").show(ui, |ui| {
                            for x in findings {
                                ui.add(egui::Label::new(format!("• {x}")).wrap());
                            }
                        });
                    }
                }
            }
            Task::Idle | Task::Running { .. } => {}
        }
    }

    fn live_view(&mut self, ui: &mut egui::Ui, app: &Arc<App>, marker: &Arc<hotkey::Marker>, started: Instant) {
        let l = app.live.lock().unwrap().clone();
        let secs = started.elapsed().as_secs();
        let check = app.cfg.wifi_check.is_some();
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            // Blinks once a second (the view repaints at 1 Hz anyway).
            let on = started.elapsed().as_secs() % 2 == 0;
            ui.painter().circle_filled(r.center(), 7.0, if on { Color32::from_rgb(230, 70, 70) } else { Color32::from_rgb(120, 40, 40) });
            ui.label(RichText::new(format!("{} {:02}:{:02}:{:02}", if check { "Wi-Fi check" } else { "Recording" }, secs / 3600, (secs / 60) % 60, secs % 60)).size(20.0).strong());
            ui.add_space(16.0);
            if !check {
                ui.label(RichText::new(format!("Marks: {}", l.marks)).size(16.0));
                if ui.button(RichText::new(format!("Mark lag spike ({})", app.cfg.hotkey)).size(15.0)).clicked() {
                    marker.mark("gui");
                }
            }
            if ui.button(RichText::new(if check { "Abort (settings are put back)" } else { "Stop & build report" }).size(15.0)).clicked() {
                self.shared.lock().unwrap().stop = true;
            }
        });
        if check {
            if let Some(st) = app.check_status.lock().unwrap().clone() {
                ui.label(RichText::new(strip_ansi(&st)).size(15.0));
            }
            ui.label(RichText::new("Leave the PC alone until the check finishes. Wi-Fi may drop for a few seconds when a setting changes.").weak());
        }
        ui.add_space(6.0);
        let now = app.session.clock.now_us();
        egui::Grid::new("live").num_columns(2).spacing([14.0, 6.0]).striped(true).show(ui, |ui| {
            if !check {
                ui.label(RichText::new("Game").strong());
                let pid = app.game_pid.load(std::sync::atomic::Ordering::Relaxed);
                if pid == 0 {
                    ui.colored_label(Color32::YELLOW, format!("waiting for {}", app.game_name()));
                } else {
                    let server = app.server.lock().unwrap().map(|s| s.to_string()).unwrap_or("detecting ...".into());
                    let net = if now - l.game_net_updated_us < 2_500_000 {
                        format!("in {:.0} pkt/s, out {:.0} pkt/s, longest gap {:.0} ms, jitter {:.1} ms", l.game_rx_pps, l.game_tx_pps, l.game_max_gap_ms, l.game_jitter_ms)
                    } else {
                        "no game traffic (menu?)".into()
                    };
                    ui.label(format!("{} (pid {pid}) - server {server} - {net}", app.game_name()));
                }
                ui.end_row();
                ui.label(RichText::new("Frames").strong());
                ui.label(if now - l.frames_updated_us < 2_500_000 { format!("{:.0} fps, worst frame {:.1} ms", l.fps, l.max_frame_ms) } else { "-".into() });
                ui.end_row();
            }
            ui.label(RichText::new("Ping").strong());
            let ping: Vec<String> = ["gateway", "hop2", "internet", "server"]
                .iter()
                .filter_map(|n| l.probes.get(*n).map(|p| format!("{n} {:.0}/{:.0} ms{}", p.avg_ms_10s, p.max_ms_10s, if p.lost_10s > 0 { format!(" ({} lost)", p.lost_10s) } else { String::new() })))
                .collect();
            ui.label(if ping.is_empty() { "-".into() } else { format!("{}   (avg/max, last 10 s)", ping.join("   ")) });
            ui.end_row();
            ui.label(RichText::new("Wi-Fi").strong());
            ui.label(l.wifi.clone().or(l.wifi_etw_quality.map(|q| format!("link quality {q}"))).unwrap_or_else(|| "-".into()));
            ui.end_row();
            ui.label(RichText::new("System").strong());
            ui.label(format!("{}   {}", l.cpu.clone().unwrap_or_else(|| "-".into()), l.gpu.clone().unwrap_or_else(|| "-".into())));
            ui.end_row();
            ui.label(RichText::new("Drivers").strong());
            ui.label(l.drivers.clone().map(|d| format!("DPC/ISR {d} (last 10 s)")).unwrap_or_else(|| "-".into()));
            ui.end_row();
        });
        ui.add_space(8.0);
        let w = ui.available_width();
        let cols = if w > 760.0 { 2 } else { 1 };
        let cw = (w - 12.0 * (cols as f32 - 1.0)) / cols as f32;
        let charts: Vec<(&str, &VecDeque<f32>, f32, Color32)> = vec![
            ("Router ping (ms)", &self.h_router, 20.0, Color32::from_rgb(90, 160, 255)),
            ("Game: longest gap between packets (ms)", &self.h_gap, 100.0, Color32::from_rgb(255, 160, 70)),
            ("Worst frame time (ms)", &self.h_frame, 33.0, Color32::from_rgb(120, 210, 140)),
            ("Worst driver DPC/ISR (ms)", &self.h_driver, 1.0, Color32::from_rgb(200, 140, 255)),
        ];
        egui::Grid::new("charts").num_columns(cols).spacing([12.0, 10.0]).show(ui, |ui| {
            for (i, (title, data, floor, color)) in charts.iter().enumerate() {
                if check && i == 1 {
                    continue;
                }
                sparkline(ui, title, data, *floor, *color, cw);
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
        ui.label(RichText::new(format!("Session folder: {}", app.session.dir.display())).weak());
    }

    fn wifi_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (busy, running) = {
            let s = self.shared.lock().unwrap();
            let r = match &s.task {
                Task::Running { app, marker, started } if s.task_tab == Tab::WifiCheck => Some((app.clone(), marker.clone(), *started)),
                _ => None,
            };
            (s.task.busy(), r)
        };
        if let Some((app, marker, started)) = running {
            self.sample(&app);
            self.live_view(ui, &app, &marker, started);
            return;
        }
        ui.heading("Wi-Fi check & fix");
        ui.label("Run this at the desktop with games closed (about 15 minutes). LagScope measures your Wi-Fi with nothing changed, then switches suspect settings off one at a time (each for a minute, then put back exactly as it was) and shows which one removes the freezes. You are asked before anything is changed, and can keep the change that helped.");
        ui.add_space(8.0);
        if ui.add_enabled(!busy, egui::Button::new(RichText::new("  Start Wi-Fi check  ").size(18.0))).clicked() {
            self.start_check(&ctx, false, false, 60);
        }
        if self.shared.lock().unwrap().task_tab == Tab::WifiCheck {
            self.task_result(ui);
        }
        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("Settings LagScope changed for you").strong());
        if self.kept.is_empty() {
            ui.label(RichText::new("None.").weak());
        } else {
            for c in &self.kept {
                ui.label(format!("• {}", c.describe()));
            }
            if ui.add_enabled(!busy, egui::Button::new("Undo all")).clicked() {
                for (what, r) in fixes::undo_kept() {
                    crate::ui::log(match r {
                        Ok(()) => format!("Undone: {what}"),
                        Err(e) => format!("Could not undo '{what}': {e:#}"),
                    });
                }
                self.kept = fixes::kept();
                self.show_log = true;
            }
        }
    }

    fn start_router_job(&self, ctx: &egui::Context) {
        spawn_job(&self.net_router, ctx, |status| {
            status("Looking up the router ...".into());
            let gw = super::probes::route_to(std::net::Ipv4Addr::new(1, 1, 1, 1)).and_then(|r| r.gateway).ok_or_else(|| anyhow::anyhow!("no internet connection"))?;
            let hops = super::probes::traceroute(std::net::Ipv4Addr::new(1, 1, 1, 1), 4);
            let hop2 = hops.iter().filter_map(|h| h.ip).find(|ip| *ip != gw && !ip.is_private());
            Ok(super::router::identify(gw, hop2))
        });
    }

    /// Saves the finished network tests as a session (called when a test completes).
    fn save_network(&self) {
        let base = capture::default_out(&self.cfg);
        let r = self.net_router.lock().unwrap().result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        let l = self.net_load.lock().unwrap().result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        let w = self.net_scan.lock().unwrap().result.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        match super::netreport::save(&base, r.as_ref(), l.as_ref(), w.as_ref()) {
            Ok(d) => {
                crate::ui::log(format!("Network test saved: {}", d.display()));
                *self.net_saved.lock().unwrap() = Some(d);
            }
            Err(e) => crate::ui::log(format!("Could not save the network test: {e:#}")),
        }
    }

    fn network_tab(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let recording = self.shared.lock().unwrap().task.busy();
        {
            let started = { let j = self.net_router.lock().unwrap(); j.running || j.result.is_some() };
            if !started {
                self.start_router_job(&ctx);
            }
        }
        let any_running = [self.net_router.lock().unwrap().running, self.net_load.lock().unwrap().running, self.net_scan.lock().unwrap().running];
        if any_running.iter().any(|x| *x) {
            ctx.request_repaint_after(Duration::from_secs(1));
        }

        ui.heading("Router");
        {
            let j = self.net_router.lock().unwrap();
            match &j.result {
                None => {
                    ui.label(RichText::new(&j.status).italics());
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
                Some(Ok(r)) => {
                    ui.label(RichText::new(if r.name.is_empty() { "Unknown router".to_string() } else { r.name.clone() }).size(16.0).strong());
                    ui.label(format!("Address {}{}", r.gateway, if r.isp_host.is_empty() { String::new() } else { format!("   ISP: {} ({})", if r.isp.is_empty() { "?" } else { &r.isp }, r.isp_host) }));
                    for n in &r.notes {
                        ui.add(egui::Label::new(format!("• {n}")).wrap());
                    }
                }
            }
        }
        ui.add_space(10.0);
        ui.separator();

        ui.heading("Latency under load (bufferbloat)");
        ui.add(egui::Label::new("Downloads and uploads as fast as possible for about 12 s each (to Cloudflare's public speed test) while pinging your router and the internet. If latency jumps when the connection is busy, everyone's downloads and uploads will make games lag. Takes about 40 s and can use 1-2 GB of data on a fast line.").wrap());
        let load_running = self.net_load.lock().unwrap().running;
        if ui.add_enabled(!load_running && !recording, egui::Button::new("Run latency-under-load test")).on_disabled_hover_text("Not while recording or another test runs").clicked() {
            spawn_job(&self.net_load, &ctx, |status| super::loadtest::run(status));
        }
        let mut save = false;
        {
            let j = self.net_load.lock().unwrap();
            if j.running {
                ui.label(RichText::new(&j.status).italics());
            }
            match &j.result {
                Some(Ok(l)) => {
                    let gc = |g: &str| match g {
                        "A+" | "A" => Color32::from_rgb(110, 210, 120),
                        "B" => Color32::from_rgb(230, 200, 90),
                        _ => Color32::from_rgb(240, 110, 100),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Download").strong());
                        ui.label(RichText::new(&l.grade_down).size(20.0).color(gc(&l.grade_down)).strong());
                        ui.add_space(14.0);
                        ui.label(RichText::new("Upload").strong());
                        ui.label(RichText::new(&l.grade_up).size(20.0).color(gc(&l.grade_up)).strong());
                    });
                    for x in l.summary.iter() {
                        ui.add(egui::Label::new(x).wrap());
                    }
                    for x in l.recommendations.iter() {
                        ui.add(egui::Label::new(format!("• {x}")).wrap());
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, format!("Test failed: {e}"));
                }
                None => {}
            }
            if !j.running && j.result.as_ref().is_some_and(|r| r.is_ok()) && !self.net_saved_has(&j.status) {
                save = true;
            }
        }
        ui.add_space(10.0);
        ui.separator();

        ui.heading("Wi-Fi neighbourhood");
        ui.add(egui::Label::new("Scans for nearby Wi-Fi networks and shows how crowded each channel is (names of other networks are not stored). The scan takes the Wi-Fi off its channel for a moment, so don't run it during a match.").wrap());
        let scan_running = self.net_scan.lock().unwrap().running;
        if ui.add_enabled(!scan_running && !recording, egui::Button::new("Scan Wi-Fi neighbourhood")).clicked() {
            spawn_job(&self.net_scan, &ctx, |status| {
                status("Scanning (about 5 s) ...".into());
                super::wifiscan::scan()
            });
        }
        {
            let j = self.net_scan.lock().unwrap();
            if j.running {
                ui.label(RichText::new(&j.status).italics());
            }
            match &j.result {
                Some(Ok(sc)) => {
                    for x in &sc.summary {
                        ui.add(egui::Label::new(x).wrap());
                    }
                    for b in sc.bands.iter().filter(|b| !b.channels.is_empty()) {
                        egui::CollapsingHeader::new(format!("{} - {} networks", b.band, b.networks)).id_salt(&b.band).default_open(b.band == sc.own_band).show(ui, |ui| {
                            egui::Grid::new(format!("ch{}", b.band)).striped(true).num_columns(4).show(ui, |ui| {
                                for h in ["channel", "networks", "strongest", "congestion"] {
                                    ui.label(RichText::new(h).weak());
                                }
                                ui.end_row();
                                for c in &b.channels {
                                    let mut name = c.channel.to_string();
                                    if b.band == sc.own_band && c.channel == sc.own_channel {
                                        name.push_str("  (yours)");
                                    }
                                    if b.best.is_some_and(|x| x.0 == c.channel) {
                                        name.push_str("  (quietest)");
                                    }
                                    ui.label(name);
                                    ui.label(c.networks.to_string());
                                    ui.label(if c.networks > 0 { format!("{} dBm", c.strongest_dbm) } else { "-".into() });
                                    ui.label(format!("{:.1}", c.score));
                                    ui.end_row();
                                }
                            });
                        });
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
                None => {}
            }
            if !j.running && j.result.as_ref().is_some_and(|r| r.is_ok()) && !self.net_saved_has(&j.status) {
                save = true;
            }
        }
        // Save once nothing is running any more (one session per batch of tests).
        let idle = !self.net_load.lock().unwrap().running && !self.net_scan.lock().unwrap().running;
        if save && idle {
            // Mark both finished jobs as saved, then write one session with everything.
            for st in [&mut self.net_load.lock().unwrap().status, &mut self.net_scan.lock().unwrap().status] {
                if !st.ends_with(" [saved]") {
                    st.push_str(" [saved]");
                }
            }
            self.save_network();
        }
        if let Some(d) = self.net_saved.lock().unwrap().clone() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Saved as {} (also in Sessions)", d.file_name().unwrap_or_default().to_string_lossy())).weak());
                if ui.button("Open report").clicked() {
                    super::open_path(&d.join("report.html"));
                }
            });
        }
    }

    fn net_saved_has(&self, status: &str) -> bool {
        status.ends_with(" [saved]")
    }

    fn sessions_tab(&mut self, ui: &mut egui::Ui) {
        if !self.sessions_loaded {
            self.load_sessions();
        }
        ui.horizontal(|ui| {
            ui.heading("Sessions");
            if ui.button("Refresh").clicked() {
                self.load_sessions();
            }
            if ui.button("Open folder").clicked() {
                super::open_path(&capture::default_out(&self.cfg));
            }
        });
        use std::sync::atomic::Ordering;
        if self.reanalyzing.load(Ordering::Relaxed) > 0 {
            ui.label(RichText::new("Re-analysing ...").italics());
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        } else if self.sessions.iter().any(|r| r.headline == "...") {
            self.load_sessions();
        }
        if self.sessions.is_empty() {
            ui.label("No recordings yet.");
            return;
        }
        if let Some(c) = &self.comparison {
            let mut back = false;
            match c {
                Ok(c) => {
                    ui.horizontal(|ui| {
                        ui.heading("Comparison");
                        if ui.button("Back to the list").clicked() {
                            back = true;
                        }
                    });
                    ui.add(egui::Label::new(RichText::new(&c.summary).size(15.0)).wrap());
                    ui.add_space(6.0);
                    egui::Grid::new("cmp").num_columns(3).striped(true).spacing([16.0, 5.0]).show(ui, |ui| {
                        ui.label("");
                        ui.label(RichText::new(format!("A  {}", c.a_name)).strong());
                        ui.label(RichText::new(format!("B  {}", c.b_name)).strong());
                        ui.end_row();
                        for r in &c.rows {
                            ui.label(r.label);
                            ui.add(egui::Label::new(&r.a).wrap());
                            let color = match r.change {
                                1 => Color32::from_rgb(110, 210, 120),
                                -1 => Color32::from_rgb(240, 110, 100),
                                _ => ui.visuals().text_color(),
                            };
                            ui.add(egui::Label::new(RichText::new(&r.b).color(color)).wrap());
                            ui.end_row();
                        }
                    });
                    ui.label(RichText::new("Green = B is better, red = B is worse.").weak());
                }
                Err(e) => {
                    ui.colored_label(Color32::LIGHT_RED, format!("Could not compare: {e}"));
                    if ui.button("Back").clicked() {
                        back = true;
                    }
                }
            }
            if back {
                self.comparison = None;
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tick two sessions to compare them (e.g. before and after a fix).").weak());
            if self.compare_pick.len() == 2 && ui.button("Compare").clicked() {
                let mut pick = self.compare_pick.clone();
                pick.sort();
                self.comparison = Some(crate::analyze::compare::compare(&pick[0], &pick[1]).map_err(|e| format!("{e:#}")));
            }
        });
        let mut redo: Option<PathBuf> = None;
        let mut toggle: Option<PathBuf> = None;
        for r in &self.sessions {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mut on = self.compare_pick.contains(&r.dir);
                    if ui.checkbox(&mut on, "").on_hover_text("Select for comparison").changed() {
                        toggle = Some(r.dir.clone());
                    }
                    ui.label(RichText::new(&r.when).strong());
                    ui.label(RichText::new(&r.game).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Folder").on_hover_text(&r.name).clicked() {
                            super::open_path(&r.dir);
                        }
                        let report = r.dir.join("report.html");
                        if ui.add_enabled(report.exists(), egui::Button::new("Report")).clicked() {
                            super::open_path(&report);
                        }
                        if ui
                            .add_enabled(r.dir.join("meta.json").exists(), egui::Button::new("Re-analyze"))
                            .on_hover_text("Rebuild the report with this version's analysis")
                            .clicked()
                        {
                            redo = Some(r.dir.clone());
                        }
                    });
                });
                ui.add(egui::Label::new(&r.headline).wrap());
            });
        }
        if let Some(dir) = toggle {
            if let Some(i) = self.compare_pick.iter().position(|d| *d == dir) {
                self.compare_pick.remove(i);
            } else {
                self.compare_pick.push(dir);
                if self.compare_pick.len() > 2 {
                    self.compare_pick.remove(0);
                }
            }
        }
        if let Some(dir) = redo {
            if let Some(r) = self.sessions.iter_mut().find(|r| r.dir == dir) {
                r.headline = "...".into();
            }
            let busy = self.reanalyzing.clone();
            busy.fetch_add(1, Ordering::Relaxed);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                if let Err(e) = crate::analyze::run(&dir) {
                    crate::ui::log(format!("Re-analysis of {} failed: {e:#}", dir.display()));
                }
                busy.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
    }

    fn question_window(&mut self, ctx: &egui::Context) {
        let q = self.shared.lock().unwrap().question.as_ref().map(|(q, d, _)| (q.clone(), *d));
        let Some((q, def)) = q else { return };
        let mut answer = None;
        egui::Window::new("LagScope asks")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(RichText::new(&q).size(16.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let (yes, no) = if def { ("Yes (recommended)", "No") } else { ("Yes", "No (recommended)") };
                    if ui.button(RichText::new(yes).size(15.0)).clicked() {
                        answer = Some(true);
                    }
                    if ui.button(RichText::new(no).size(15.0)).clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(a) = answer {
            if let Some((_, _, tx)) = self.shared.lock().unwrap().question.take() {
                let _ = tx.send(a);
            }
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        let lines: Vec<String> = self.log.lock().unwrap().iter().cloned().collect();
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
            for l in lines {
                ui.label(RichText::new(l).monospace().size(11.5));
            }
        });
    }

    // ---------- self-test ----------

    fn selftest_step(&mut self, ctx: &egui::Context) {
        // Taken out while the script runs so it can call methods on self.
        let Some(mut st_owned) = self.selftest.take() else { return };
        let st = &mut st_owned;
        let t = st.t0.elapsed().as_secs_f64();
        // Auto-answer questions: no driver installs, allow the temporary tests, keep nothing.
        let q = self.shared.lock().unwrap().question.as_ref().map(|(q, d, _)| (q.clone(), *d));
        if q.is_none() {
            st.q_seen = None;
        }
        if let Some((q, def)) = q {
            // Photograph each question once, then answer it two seconds later.
            let seen = *st.q_seen.get_or_insert_with(|| {
                st.shots.push(format!("q{}", st.shots.len()));
                let name = st.shots.last().unwrap().clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name)));
                t
            });
            if t > seen + 2.0 {
                let a = if q.contains("Windows Update") || q.contains("Install") || q.contains("Keep") { false } else if q.contains("temporary changes") { true } else { def };
                if let Some((_, _, tx)) = self.shared.lock().unwrap().question.take() {
                    let _ = tx.send(a);
                }
            }
        }
        let shot = |ctx: &egui::Context, st: &mut SelfTest, name: &str| {
            st.shots.push(name.to_string());
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(name.to_string())));
        };
        let done = matches!(self.shared.lock().unwrap().task, Task::Done(_) | Task::Failed(_));
        let scenario = st.scenario.clone();
        let step = st.step;
        // (time, action) per scenario; `step` counts completed actions.
        match (scenario.as_str(), step) {
            (_, 0) if t > 2.0 => {
                shot(ctx, st, "01_start");
                st.step += 1;
            }
            ("idle", 1) if t > 3.0 => {
                st.cpu0 = cpu_secs();
                st.step += 1;
            }
            ("idle", 2) if t > 33.0 => {
                st.idle_cpu = Some((cpu_secs() - st.cpu0, t - 3.0));
                self.tab = Tab::Sessions;
                self.load_sessions();
                st.step += 1;
            }
            ("idle", 3) if t > 35.0 => {
                shot(ctx, st, "02_sessions");
                st.step += 1;
            }
            ("idle", 4) if t > 37.0 => {
                self.tab = Tab::WifiCheck;
                st.step += 1;
            }
            ("idle", 5) if t > 39.0 => {
                shot(ctx, st, "03_wifi_tab");
                st.step = 100;
            }
            ("record", 1) => {
                self.cfg.profile = profile::find(&self.profiles, "auto").cloned().unwrap_or(self.cfg.profile.clone());
                self.cfg.processes.clear();
                self.start_record(ctx);
                st.cpu0 = cpu_secs();
                st.step += 1;
            }
            ("record", 2) if t > 25.0 => {
                shot(ctx, st, "02_recording");
                st.step += 1;
            }
            ("record", 3) if t > 28.0 => {
                // Clone first: marking logs a line, and the log hook takes the same lock.
                let marker = match &self.shared.lock().unwrap().task {
                    Task::Running { marker, .. } => Some(marker.clone()),
                    _ => None,
                };
                if let Some(m) = marker {
                    m.mark("selftest");
                }
                st.step += 1;
            }
            ("record", 4) if t > 62.0 => {
                st.idle_cpu = Some((cpu_secs() - st.cpu0, t - 2.0));
                self.shared.lock().unwrap().stop = true;
                st.step += 1;
            }
            ("record", 5) if done => {
                shot(ctx, st, "03_done");
                st.wait_until = t + 2.0;
                st.step += 1;
            }
            ("record", 6) if t > st.wait_until => {
                self.tab = Tab::Sessions;
                self.load_sessions();
                st.wait_until = t + 2.0;
                st.step += 1;
            }
            ("record", 7) if t > st.wait_until => {
                shot(ctx, st, "04_sessions");
                st.step = 100;
            }
            ("reanalyze", 1) => {
                self.tab = Tab::Sessions;
                self.load_sessions();
                if let Some(r) = self.sessions.first() {
                    let dir = r.dir.clone();
                    let busy = self.reanalyzing.clone();
                    busy.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    std::thread::spawn(move || {
                        let _ = crate::analyze::run(&dir);
                        busy.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    });
                    self.sessions[0].headline = "...".into();
                }
                st.step += 1;
            }
            ("reanalyze", 2) if self.reanalyzing.load(std::sync::atomic::Ordering::Relaxed) == 0 && t > 4.0 => {
                st.wait_until = t + 2.0;
                st.step += 1;
            }
            ("reanalyze", 3) if t > st.wait_until => {
                shot(ctx, st, "02_reanalyzed");
                st.step = 100;
            }
            ("compare", 1) => {
                self.tab = Tab::Sessions;
                self.load_sessions();
                let mut pick: Vec<PathBuf> = self.sessions.iter().take(2).map(|r| r.dir.clone()).collect();
                pick.sort();
                if pick.len() == 2 {
                    self.comparison = Some(crate::analyze::compare::compare(&pick[0], &pick[1]).map_err(|e| format!("{e:#}")));
                }
                st.wait_until = t + 2.0;
                st.step += 1;
            }
            ("compare", 2) if t > st.wait_until => {
                shot(ctx, st, "02_compare");
                st.step = 100;
            }
            ("network", 1) => {
                self.tab = Tab::Network;
                st.step += 1;
            }
            ("network", 2) if t > 3.0 => {
                spawn_job(&self.net_scan, ctx, |_| super::wifiscan::scan());
                spawn_job(&self.net_load, ctx, |status| super::loadtest::run(status));
                st.step += 1;
            }
            ("network", 3) if t > 8.0 => {
                shot(ctx, st, "02_network_running");
                st.step += 1;
            }
            ("network", 4) if !self.net_load.lock().unwrap().running && !self.net_scan.lock().unwrap().running && !self.net_router.lock().unwrap().running => {
                st.wait_until = t + 3.0;
                st.step += 1;
            }
            ("network", 5) if t > st.wait_until => {
                shot(ctx, st, "03_network_done");
                st.step = 100;
            }
            ("auto", 1) => {
                self.set_auto(ctx, true);
                st.step += 1;
            }
            ("auto", 2) if self.shared.lock().unwrap().auto_game.is_some() && matches!(self.shared.lock().unwrap().task, Task::Running { .. }) => {
                st.wait_until = t + 12.0;
                st.step += 1;
            }
            ("auto", 3) if t > st.wait_until => {
                shot(ctx, st, "02_auto_recording");
                st.step += 1;
            }
            ("auto", 4) if done => {
                st.wait_until = t + 3.0;
                st.step += 1;
            }
            ("auto", 5) if t > st.wait_until => {
                shot(ctx, st, "03_auto_done");
                // Like clicking X with auto-record on: should hide to the tray, not exit.
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                st.wait_until = t + 4.0;
                st.step += 1;
            }
            ("auto", 6) if t > st.wait_until => {
                crate::ui::log(format!("selftest: hidden after close = {}", self.hidden));
                // Restore the user's setting, then exit through the tray menu path.
                self.auto_on.store(false, std::sync::atomic::Ordering::Relaxed);
                profile::set_setting("auto_record", false.into());
                st.step = 101;
            }
            ("prompt", 1) => {
                std::thread::spawn(|| console::ask_yes_no("Check Windows Update for newer Wi-Fi / Bluetooth drivers? (takes a minute or two)", true));
                st.wait_until = t + 6.0;
                st.step += 1;
            }
            ("prompt", 2) if t > st.wait_until => {
                st.step = 100;
            }
            ("close", 1) => {
                self.start_record(ctx);
                st.step += 1;
            }
            ("close", 2) if t > 22.0 => {
                // Like clicking the window's X while recording.
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                st.step += 1;
            }
            ("close", 3) if t > 23.0 => {
                shot(ctx, st, "02_closing");
                st.step += 1;
            }
            ("close", 4) if done => {
                // The window closes by itself now; write the metrics first.
                st.step = 101;
            }
            ("wifi", 1) => {
                self.tab = Tab::WifiCheck;
                self.start_check(ctx, false, true, 12);
                st.step += 1;
            }
            ("wifi", 2) if t > 60.0 => {
                shot(ctx, st, "02_checking");
                st.step += 1;
            }
            ("wifi", 3) if done => {
                self.show_log = true;
                st.wait_until = t + 2.0;
                st.step += 1;
            }
            ("wifi", 4) if t > st.wait_until => {
                shot(ctx, st, "03_done");
                st.step = 100;
            }
            (_, 100) => {
                st.step = 101;
            }
            (_, 101) => {
                let (cpu, secs) = st.idle_cpu.unwrap_or((0.0, 1.0));
                let (mem, private) = memory_mb();
                let log: Vec<String> = self.log.lock().unwrap().iter().cloned().collect();
                let text = format!(
                    "scenario={}\ncpu_secs={cpu:.3}\nwindow_secs={secs:.1}\ncpu_pct_one_core={:.2}\nworking_set_mb={mem:.1}\nprivate_mb={private:.1}\nshots={}\n--- log ---\n{}\n",
                    st.scenario,
                    cpu / secs * 100.0,
                    st.shots.join(","),
                    log.join("\n")
                );
                let _ = std::fs::write(st.out.join("selftest.txt"), text);
                st.step = 102;
            }
            (_, 102) => {
                std::thread::sleep(Duration::from_millis(500));
                if self.tray.lock().unwrap().is_some() {
                    let _ = self.tray_tx.send(super::tray::TrayEvent::Exit);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                st.step = 103;
            }
            _ => {}
        }
        self.selftest = Some(st_owned);
        // Keep the script ticking at the GUI's own busy rate, so measurements stay honest.
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn save_screenshots(&mut self, ctx: &egui::Context) {
        let Some(st) = self.selftest.as_ref() else { return };
        let out = st.out.clone();
        ctx.input(|i| {
            for e in &i.raw.events {
                if let egui::Event::Screenshot { user_data, image, .. } = e {
                    let name = user_data.data.as_ref().and_then(|d| d.downcast_ref::<String>()).cloned().unwrap_or_else(|| "shot".into());
                    let _ = write_bmp(&out.join(format!("{name}.bmp")), image);
                }
            }
        });
    }
}

impl eframe::App for GuiApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.housekeeping(ctx);
        self.selftest_step(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.save_screenshots(&ctx);

        let busy = self.shared.lock().unwrap().task.busy();
        *self.cfg_shared.lock().unwrap() = self.cfg.clone();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        if self.show_log {
            egui::Panel::bottom("log").resizable(true).default_size(180.0).show(ui, |ui| self.log_panel(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if self.close_when_done {
                ui.label(RichText::new("Finishing the recording before closing ...").italics());
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Record => self.record_tab(ui),
                Tab::WifiCheck => self.wifi_tab(ui),
                Tab::Network => self.network_tab(ui),
                Tab::Sessions => self.sessions_tab(ui),
            });
        });
        self.question_window(&ctx);

        // Repaint policy: once a second while recording (live numbers), on demand otherwise.
        if busy {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}

/// Small line chart of the last two minutes; NaN = no data.
fn sparkline(ui: &mut egui::Ui, title: &str, data: &VecDeque<f32>, floor: f32, color: Color32, width: f32) {
    ui.vertical(|ui| {
        let last = data.iter().rev().find(|v| v.is_finite()).copied();
        let max = data.iter().copied().filter(|v| v.is_finite()).fold(floor, f32::max);
        ui.label(format!("{title}   {}", last.map(|v| format!("now {v:.1}")).unwrap_or_default()));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 56.0), egui::Sense::hover());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        let n = HISTORY as f32;
        let mut seg: Vec<egui::Pos2> = Vec::new();
        let flush = |seg: &mut Vec<egui::Pos2>| {
            if seg.len() >= 2 {
                p.add(egui::Shape::line(std::mem::take(seg), egui::Stroke::new(1.5, color)));
            }
            seg.clear();
        };
        let off = HISTORY - data.len();
        for (i, v) in data.iter().enumerate() {
            if !v.is_finite() {
                flush(&mut seg);
                continue;
            }
            let x = rect.left() + (off + i) as f32 / (n - 1.0) * rect.width();
            let y = rect.bottom() - 3.0 - (v.min(max) / max) * (rect.height() - 6.0);
            seg.push(egui::pos2(x, y));
        }
        flush(&mut seg);
        p.text(rect.right_top() + egui::vec2(-4.0, 2.0), egui::Align2::RIGHT_TOP, format!("{max:.0}"), egui::FontId::proportional(10.0), ui.visuals().weak_text_color());
    });
}

/// Window icon, drawn in code: a scope ring with a blip.
fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let blip = ((x as f32 - 42.0).powi(2) + (y as f32 - 22.0).powi(2)).sqrt();
            let px = if blip < 6.0 {
                [255, 90, 80, 255]
            } else if (d - 28.0).abs() < 3.0 || (d - 16.0).abs() < 1.5 || (dx.abs() < 1.2 && d < 28.0) || (dy.abs() < 1.2 && d < 28.0) {
                [90, 200, 255, 255]
            } else if d < 30.0 {
                [18, 30, 44, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba[(y * n + x) * 4..(y * n + x) * 4 + 4].copy_from_slice(&px);
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

fn cpu_secs() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    ft(k) + ft(u)
}

/// (working set, private bytes) in MB. The working set includes shared code pages of the
/// graphics driver; private bytes are what this process really costs.
fn memory_mb() -> (f64, f64) {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut pmc: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, pmc.cb) };
    (pmc.WorkingSetSize as f64 / 1_048_576.0, pmc.PrivateUsage as f64 / 1_048_576.0)
}

/// 24-bit BMP (no image crate needed) for self-test screenshots.
fn write_bmp(path: &std::path::Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let (w, h) = (img.size[0], img.size[1]);
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for y in (0..h).rev() {
        let start = b.len();
        for x in 0..w {
            let c = img.pixels[y * w + x];
            b.extend_from_slice(&[c.b(), c.g(), c.r()]);
        }
        b.resize(start + row, 0);
    }
    std::fs::write(path, b)
}

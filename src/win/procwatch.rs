//! Process list from the kernel's process information (NtQuerySystemInformation: names,
//! CPU time, memory and I/O counters of every process, no process handles are opened).
//! Maintains the PID -> name map used to label network traffic, detects game start/exit,
//! records which other programs use a lot of CPU, and once a second also checks the
//! power source and the network link (speed, drops, errors).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

use super::{Running, spawn};
use crate::app::{App, GameStats};
use crate::row;

const SYSTEM_PROCESS_INFORMATION: i32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
/// Programs worth knowing about when diagnosing lag: VPNs, "ping boosters", network
/// stacks with traffic shaping, overlays/recorders and heavy background syncers.
const NOTABLE: &[&str] = &[
    "killer", "exitlag", "gearup", "noping", "wtfast", "mudfish", "haste", "nordvpn", "expressvpn",
    "protonvpn", "surfshark", "openvpn", "wireguard", "tailscale", "zerotier", "hamachi", "radmin",
    "warp", "cloudflare", "obs64", "obs32", "medal", "overwolf", "outplayed", "nvcontainer",
    "nvidia share", "nvidia overlay", "nvidia app", "msiafterburner", "rtss", "icue", "armourycrate",
    "lghub", "razer", "synapse", "wallpaper", "onedrive", "dropbox", "googledrivefs", "steamwebhelper",
    "epicgameslauncher", "battle.net", "qbittorrent", "utorrent", "discord", "spotify", "teams",
    "msedgewebview2", "chrome", "firefox", "msedge", "smartbyte", "dragon center", "lenovo vantage",
    "asus", "wifi", "netlimiter", "glasswire", "mcafee", "norton", "avast", "avg", "kaspersky",
    "bitdefender", "malwarebytes", "eset",
];

pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    /// User + kernel time, 100 ns units.
    pub cpu: u64,
    pub private_bytes: u64,
    pub page_faults: u32,
    pub threads: u32,
    pub priority: i32,
    /// Bytes read + written + other I/O.
    pub io_bytes: u64,
}

/// All processes. `buf` is reused between calls.
pub fn processes(buf: &mut Vec<u64>) -> Vec<ProcInfo> {
    if buf.is_empty() {
        buf.resize(64 * 1024, 0);
    }
    loop {
        let mut ret = 0u32;
        let st = unsafe { NtQuerySystemInformation(SYSTEM_PROCESS_INFORMATION, buf.as_mut_ptr() as *mut _, (buf.len() * 8) as u32, &mut ret) };
        if st == STATUS_INFO_LENGTH_MISMATCH && buf.len() < 8 * 1024 * 1024 {
            let want = (ret as usize / 8 + 8192).max(buf.len() * 2);
            buf.resize(want, 0);
            continue;
        }
        if st != 0 {
            return Vec::new();
        }
        break;
    }
    // SYSTEM_PROCESS_INFORMATION (x64) field offsets.
    let base = buf.as_ptr() as *const u8;
    let mut out = Vec::with_capacity(400);
    let mut off = 0usize;
    unsafe {
        let rd32 = |o: usize| std::ptr::read_unaligned(base.add(o) as *const u32);
        let rd64 = |o: usize| std::ptr::read_unaligned(base.add(o) as *const u64);
        loop {
            let next = rd32(off) as usize;
            let len = std::ptr::read_unaligned(base.add(off + 56) as *const u16) as usize / 2;
            let ptr = rd64(off + 64) as *const u16;
            let pid = rd64(off + 80) as u32;
            let name = if ptr.is_null() || len == 0 {
                if pid == 0 { "Idle".to_string() } else { "System".to_string() }
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            };
            out.push(ProcInfo {
                pid,
                name,
                cpu: rd64(off + 40) + rd64(off + 48),
                private_bytes: rd64(off + 8),
                page_faults: rd32(off + 128),
                threads: rd32(off + 4),
                priority: rd32(off + 72) as i32,
                io_bytes: rd64(off + 232).wrapping_add(rd64(off + 240)).wrapping_add(rd64(off + 248)),
            });
            if next == 0 {
                break;
            }
            off += next;
        }
    }
    out
}

pub fn start(app: Arc<App>) -> Running {
    // Populate synchronously so the first network events already get names.
    let mut buf = Vec::new();
    let first = processes(&mut buf);
    update(&app, first.iter().map(|p| (p.pid, p.name.clone())).collect(), true);
    let a = app.clone();
    let t = spawn("procwatch", move || {
        let mut streak = HashMap::new();
        let mut cpu = CpuTracker::new(&a, first);
        let mut env = Environment::new(&a);
        while a.sleep_ms(1000) {
            let list = processes(&mut buf);
            let map: HashMap<u32, String> = list.iter().map(|p| (p.pid, p.name.clone())).collect();
            auto_detect(&a, &map, &mut streak);
            update(&a, map, false);
            cpu.sample(&a, list);
            env.sample(&a);
        }
    });
    Running::threads("procwatch", vec![t])
}

/// Per-process CPU use (from CPU-time deltas) and the game's own figures.
struct CpuTracker {
    prev: HashMap<u32, ProcInfo>,
    prev_t: i64,
    sink: Arc<crate::session::Sink>,
    me: u32,
    ncpu: f64,
}

impl CpuTracker {
    fn new(app: &App, first: Vec<ProcInfo>) -> Self {
        CpuTracker {
            prev: first.into_iter().map(|p| (p.pid, p)).collect(),
            prev_t: app.session.clock.now_us(),
            sink: app.session.sink("proc_cpu", "t_us,name,cpu_pct"),
            me: std::process::id(),
            ncpu: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64,
        }
    }

    fn sample(&mut self, app: &App, list: Vec<ProcInfo>) {
        let t = app.session.clock.now_us();
        let secs = ((t - self.prev_t) as f64 / 1e6).max(0.2);
        // Percent of one core per process.
        let mut busy: Vec<(f64, &str)> = Vec::new();
        let mut game = GameStats::default();
        let mut game_seen = false;
        for p in &list {
            let Some(old) = self.prev.get(&p.pid).filter(|o| o.name == p.name) else { continue };
            let pct = p.cpu.saturating_sub(old.cpu) as f64 / (secs * 1e5);
            if app.is_game_pid(p.pid) {
                game_seen = true;
                game.cpu_pct += pct / self.ncpu;
                game.private_mb += p.private_bytes as f64 / 1_048_576.0;
                game.page_faults_per_s += p.page_faults.saturating_sub(old.page_faults) as f64 / secs;
                game.io_bytes_per_s += p.io_bytes.saturating_sub(old.io_bytes) as f64 / secs;
                game.threads += p.threads;
                game.priority = game.priority.max(p.priority);
            } else if p.pid != 0 && p.pid != self.me && pct >= 10.0 {
                busy.push((pct, &p.name));
            }
        }
        busy.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (pct, name) in busy.iter().take(5) {
            row!(self.sink, "{t},{},{pct:.0}", crate::session::csv_escape(name));
        }
        {
            let mut l = app.live.lock().unwrap();
            l.top_cpu = busy.first().filter(|b| b.0 >= 25.0).map(|(pct, name)| format!("{name} {pct:.0}% of a core"));
        }
        *app.game_stats.lock().unwrap() = game_seen.then_some(game);
        self.prev = list.into_iter().map(|p| (p.pid, p)).collect();
        self.prev_t = t;
    }
}

/// Power source and network link state; changes are logged as events.
struct Environment {
    power: Option<(bool, bool)>,
    if_index: Option<u32>,
    is_wifi: bool,
    link: Option<(bool, u64)>,
    errors: [u64; 4],
    sink: Arc<crate::session::Sink>,
}

impl Environment {
    fn new(app: &App) -> Self {
        let route = super::probes::route_to(std::net::Ipv4Addr::new(1, 1, 1, 1));
        let mut e = Environment {
            power: None,
            if_index: route.as_ref().map(|r| r.if_index),
            is_wifi: route.as_ref().is_some_and(|r| r.is_wifi),
            link: None,
            errors: [0; 4],
            sink: app.session.sink("link", "t_us,up,rx_mbps,tx_mbps,in_errors,in_discards,out_errors,out_discards"),
        };
        e.sample(app);
        e
    }

    fn sample(&mut self, app: &App) {
        let t = app.session.clock.now_us();
        let mut ps: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { GetSystemPowerStatus(&mut ps) } != 0 {
            // ACLineStatus 0 = battery, 1 = mains, 255 = unknown (desktops: 1).
            let battery = ps.ACLineStatus == 0;
            let saver = ps.SystemStatusFlag == 1;
            if self.power.is_none() {
                app.session.meta_set(
                    "power",
                    serde_json::json!({ "on_battery": battery, "battery_saver": saver, "has_battery": ps.BatteryFlag != 128 && ps.BatteryFlag != 255, "battery_pct": ps.BatteryLifePercent }),
                );
            } else if self.power != Some((battery, saver)) {
                app.session.event(
                    "power",
                    format!("{}{}", if battery { format!("on battery ({}%)", ps.BatteryLifePercent) } else { "plugged in".to_string() }, if saver { ", battery saver on" } else { "" }),
                );
            }
            self.power = Some((battery, saver));
        }
        let Some(idx) = self.if_index else { return };
        let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
        row.InterfaceIndex = idx;
        if unsafe { GetIfEntry2(&mut row) } != 0 {
            return;
        }
        let up = row.OperStatus == 1;
        // Wi-Fi rates change all the time (rate adaptation): only wired speed changes count.
        let speed = if self.is_wifi { 0 } else { row.TransmitLinkSpeed };
        let errs = [row.InErrors, row.InDiscards, row.OutErrors, row.OutDiscards];
        let first = self.link.is_none();
        let d: Vec<u64> = errs.iter().zip(self.errors).map(|(n, o)| n.saturating_sub(o)).collect();
        let changed = self.link.is_some_and(|l| l != (up, speed));
        if first || changed || (!first && d.iter().any(|&x| x > 0)) {
            row!(self.sink, "{t},{},{:.0},{:.0},{},{},{},{}", up as u8, row.ReceiveLinkSpeed as f64 / 1e6, row.TransmitLinkSpeed as f64 / 1e6, if first { 0 } else { d[0] }, if first { 0 } else { d[1] }, if first { 0 } else { d[2] }, if first { 0 } else { d[3] });
        }
        if !self.is_wifi {
            app.live.lock().unwrap().wired = Some(if up { format!("cable, {:.0} Mbit/s", row.TransmitLinkSpeed as f64 / 1e6) } else { "cable unplugged / link down".to_string() });
        }
        if changed {
            app.session.event("link", if up { format!("network link up, {:.0} Mbit/s", row.TransmitLinkSpeed as f64 / 1e6) } else { "network link DOWN".to_string() });
        }
        self.link = Some((up, speed));
        self.errors = errs;
    }
}

/// Programs that present frames but are not games (ignored by auto-detect).
const NOT_GAMES: &[&str] = &[
    "dwm.exe", "explorer.exe", "chrome.exe", "msedge.exe", "firefox.exe", "opera.exe", "brave.exe", "vivaldi.exe",
    "msedgewebview2.exe", "discord.exe", "steamwebhelper.exe", "steam.exe", "epicgameslauncher.exe", "battle.net.exe",
    "riotclientux.exe", "eadesktop.exe", "ubisoftconnect.exe", "obs64.exe", "obs32.exe", "spotify.exe", "teams.exe",
    "ms-teams.exe", "slack.exe", "zoom.exe", "code.exe", "windowsterminal.exe", "conhost.exe", "lagscope.exe",
    "applicationframehost.exe", "searchhost.exe", "startmenuexperiencehost.exe", "shellexperiencehost.exe",
    "textinputhost.exe", "nvidia overlay.exe", "nvcontainer.exe", "medal.exe", "overwolf.exe", "photos.exe",
    "vlc.exe", "mpc-hc64.exe", "wallpaper64.exe", "wallpaper32.exe", "rtss.exe", "msiafterburner.exe",
    // Video and second-screen apps (a video on another monitor must not become "the game").
    "mpv.exe", "mpc-be64.exe", "potplayermini64.exe", "potplayermini.exe", "wmplayer.exe", "video.ui.exe",
    "microsoft.media.player.exe", "plex.exe", "kodi.exe", "wwahost.exe", "twitch.exe", "lively.exe",
    "rainmeter.exe", "streamlabs obs.exe", "xboxpcapp.exe", "gamebar.exe", "nvidia app.exe", "parsecd.exe",
    "whatsapp.exe", "telegram.exe",
];

/// Auto-detect: the process that keeps presenting the most frames (>= 30/s for 5 s) is the game.
fn auto_detect(app: &App, map: &HashMap<u32, String>, streak: &mut HashMap<String, u32>) {
    if !app.cfg.processes.is_empty() {
        return;
    }
    let counts: HashMap<u32, u32> = std::mem::take(&mut *app.auto_presents.lock().unwrap());
    let mut current = app.auto_game.lock().unwrap();
    if let Some(name) = current.clone() {
        // Forget a game that has exited so the next one can be picked up.
        if !map.values().any(|n| n.eq_ignore_ascii_case(&name)) {
            app.session.event("game", format!("{name} exited; waiting for the next game"));
            *current = None;
        }
        return;
    }
    let best = counts
        .iter()
        .filter_map(|(pid, n)| map.get(pid).map(|name| (name, *n)))
        .filter(|(name, _)| !NOT_GAMES.contains(&name.to_ascii_lowercase().as_str()))
        .max_by_key(|(_, n)| *n);
    let Some((name, n)) = best.filter(|(_, n)| *n >= 30) else {
        streak.clear();
        return;
    };
    let s = streak.entry(name.clone()).or_default();
    *s += 1;
    if *s >= 5 {
        app.session.event("game", format!("auto-detected game: {name} ({n} frames/s)"));
        app.session.meta_push("detected_games", name.clone().into());
        // A known game: analyse it with its own profile.
        let (all, _) = crate::profile::load_all(&crate::profile::user_dirs());
        if let Some(p) = crate::profile::find(&all, name).filter(|p| !p.is_auto()) {
            app.session.event("game", format!("using the {} profile", p.name));
            app.session.meta_set("profile", serde_json::to_value(p).unwrap_or_default());
        }
        *current = Some(name.clone());
        streak.clear();
    }
}

fn update(app: &App, map: HashMap<u32, String>, first: bool) {
    let mut wanted: Vec<String> = app.cfg.processes.iter().map(|s| s.to_ascii_lowercase()).collect();
    if let Some(g) = app.auto_game.lock().unwrap().as_ref() {
        wanted.push(g.to_ascii_lowercase());
    }
    let game: HashSet<u32> = map
        .iter()
        .filter(|(_, n)| wanted.contains(&n.to_ascii_lowercase()))
        .map(|(p, _)| *p)
        .collect();
    let old: HashSet<u32> = app.game_pids.read().unwrap().clone();
    if game != old {
        let started: Vec<String> = game.difference(&old).map(|p| p.to_string()).collect();
        let exited: Vec<String> = old.difference(&game).map(|p| p.to_string()).collect();
        let name = app.game_name();
        if !started.is_empty() {
            app.session.event("game", format!("{name} running (pid {})", started.join(", ")));
        }
        if !exited.is_empty() {
            app.session.event("game", format!("{name} exited (pid {})", exited.join(", ")));
        }
        let primary = game.iter().min().copied().unwrap_or(0);
        app.game_pid.store(primary, Ordering::Relaxed);
        *app.game_pids.write().unwrap() = game;
    }
    if first {
        let mut notable: Vec<String> = map
            .values()
            .filter(|n| {
                let l = n.to_ascii_lowercase();
                NOTABLE.iter().any(|k| l.contains(k))
            })
            .cloned()
            .collect();
        notable.sort();
        notable.dedup();
        app.session.meta_set("notable_processes", serde_json::json!(notable));
        let mut all: Vec<String> = map.values().cloned().collect();
        all.sort_by_key(|s| s.to_ascii_lowercase());
        all.dedup();
        app.session.meta_set("process_count", map.len().into());
        app.session.write_text("sysinfo/processes_at_start.txt", &all.join("\n"));
    }
    // Keep names of exited processes so late ETW events still resolve.
    let mut procs = app.procs.write().unwrap();
    procs.extend(map);
}

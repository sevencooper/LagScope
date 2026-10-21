//! Process list via Toolhelp snapshots (no process handles are opened). Maintains the
//! PID -> name map used to label network traffic and detects game start/exit.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

use super::{Running, from_wide_buf, spawn};
use crate::app::App;

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

pub fn snapshot() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return map;
        }
        let mut pe: PROCESSENTRY32W = std::mem::zeroed();
        pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut pe) != 0 {
            loop {
                map.insert(pe.th32ProcessID, from_wide_buf(&pe.szExeFile));
                if Process32NextW(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    map
}

pub fn start(app: Arc<App>) -> Running {
    // Populate synchronously so the first network events already get names.
    let first = snapshot();
    update(&app, first, true);
    let a = app.clone();
    let t = spawn("procwatch", move || {
        while a.sleep_ms(1000) {
            update(&a, snapshot(), false);
        }
    });
    Running::threads("procwatch", vec![t])
}

fn update(app: &App, map: HashMap<u32, String>, first: bool) {
    let wanted: Vec<String> = app.cfg.processes.iter().map(|s| s.to_ascii_lowercase()).collect();
    let game: HashSet<u32> = map
        .iter()
        .filter(|(_, n)| wanted.contains(&n.to_ascii_lowercase()))
        .map(|(p, _)| *p)
        .collect();
    let old: HashSet<u32> = app.game_pids.read().unwrap().clone();
    if game != old {
        let started: Vec<String> = game.difference(&old).map(|p| p.to_string()).collect();
        let exited: Vec<String> = old.difference(&game).map(|p| p.to_string()).collect();
        let name = app.cfg.processes.join("/");
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

//! State shared by all collectors during a capture.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, RwLock};

use crate::session::Session;

#[derive(Clone, Debug)]
pub struct Config {
    /// The game profile (what to watch, analysis settings).
    pub profile: crate::profile::Profile,
    /// Executable names (case-insensitive) of the game to watch; empty = auto-detect.
    pub processes: Vec<String>,
    pub hotkey: String,
    pub probe_hz: u32,
    pub probes: bool,
    pub out_dir: Option<PathBuf>,
    pub duration_s: Option<u64>,
    pub zip: bool,
    pub open_report: bool,
    /// Extra ICMP targets to probe continuously.
    pub extra_targets: Vec<String>,
    /// Wi-Fi check mode: seconds per test window (None = normal recording).
    pub wifi_check: Option<u64>,
    /// Kernel tracing of driver DPC/ISR times and disk I/O.
    pub kernel_trace: bool,
    /// Wi-Fi check without questions: run all tests, no driver updates, keep nothing.
    pub unattended: bool,
    /// Wi-Fi check: run the tests even if the baseline shows no freezes.
    pub force_tests: bool,
}

impl Default for Config {
    fn default() -> Self {
        let profile = crate::profile::builtin().remove(0);
        Config {
            processes: profile.processes.clone(),
            profile,
            hotkey: "F8".into(),
            probe_hz: 20,
            probes: true,
            out_dir: None,
            duration_s: None,
            zip: true,
            open_report: true,
            extra_targets: vec![],
            wifi_check: None,
            unattended: false,
            force_tests: false,
            kernel_trace: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProbeLive {
    pub sent_10s: u32,
    pub lost_10s: u32,
    pub max_ms_10s: f64,
    pub avg_ms_10s: f64,
}

/// Rolling numbers for the console status block. Collectors overwrite their own fields.
#[derive(Clone, Debug, Default)]
pub struct LiveData {
    pub game_rx_pps: f64,
    pub game_tx_pps: f64,
    pub game_rx_kbps: f64,
    pub game_tx_kbps: f64,
    pub game_max_gap_ms: f64,
    pub game_jitter_ms: f64,
    pub game_net_updated_us: i64,
    pub fps: f64,
    pub max_frame_ms: f64,
    pub frames_updated_us: i64,
    pub probes: BTreeMap<String, ProbeLive>,
    pub wifi: Option<String>,
    pub wifi_etw_quality: Option<u32>,
    pub wifi_location_blocked: bool,
    pub stall_max_ms: f64,
    pub drivers: Option<String>,
    /// Worst driver DPC/ISR run in the last 10 s (ms).
    pub drivers_worst_ms: f64,
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    /// Disk activity in the last second.
    pub disk: Option<String>,
    /// Busiest other program in the last second (if it used a lot of CPU).
    pub top_cpu: Option<String>,
    pub top_talker: Option<String>,
    /// Set when the connection is wired: link state and speed (replaces the Wi-Fi row).
    pub wired: Option<String>,
    /// GPU load from Windows' own counters, for cards without NVIDIA's interface.
    pub gpu_engines: Option<String>,
    pub marks: u32,
    pub collectors: BTreeMap<String, String>,
}

pub struct App {
    pub cfg: Config,
    pub session: Session,
    pub stop: AtomicBool,
    /// PID of the (first) running game process, 0 if none.
    pub game_pid: AtomicU32,
    pub game_pids: RwLock<HashSet<u32>>,
    pub procs: RwLock<HashMap<u32, String>>,
    pub server: Mutex<Option<SocketAddr>>,
    pub live: Mutex<LiveData>,
    /// Router ping results (send time, RTT ms or None if lost), for the Wi-Fi check.
    pub gw_samples: Mutex<Vec<(i64, Option<f64>)>>,
    /// Progress line shown in the status block during the Wi-Fi check.
    pub check_status: Mutex<Option<String>>,
    /// Auto-detect profile: frames presented per process since the last check, and the
    /// executable name chosen as the game.
    pub auto_presents: Mutex<HashMap<u32, u32>>,
    pub auto_game: Mutex<Option<String>>,
    /// Latest game process figures (from the process watcher, once a second).
    pub game_stats: Mutex<Option<GameStats>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GameStats {
    /// Percent of all logical CPUs.
    pub cpu_pct: f64,
    pub private_mb: f64,
    pub page_faults_per_s: f64,
    pub io_bytes_per_s: f64,
    pub threads: u32,
    pub priority: i32,
}

impl App {
    pub fn new(cfg: Config, session: Session) -> Self {
        App {
            cfg,
            session,
            stop: AtomicBool::new(false),
            game_pid: AtomicU32::new(0),
            game_pids: RwLock::new(HashSet::new()),
            procs: RwLock::new(HashMap::new()),
            server: Mutex::new(None),
            live: Mutex::new(LiveData::default()),
            gw_samples: Mutex::new(Vec::new()),
            check_status: Mutex::new(None),
            auto_presents: Mutex::new(HashMap::new()),
            auto_game: Mutex::new(None),
            game_stats: Mutex::new(None),
        }
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Name of the game being watched, for messages.
    pub fn game_name(&self) -> String {
        if self.cfg.processes.is_empty() {
            self.auto_game.lock().unwrap().clone().unwrap_or_else(|| "any game (auto-detect)".into())
        } else {
            self.cfg.profile.name.clone()
        }
    }

    pub fn is_game_pid(&self, pid: u32) -> bool {
        pid != 0
            && (self.game_pid.load(Ordering::Relaxed) == pid
                || self.game_pids.read().map(|s| s.contains(&pid)).unwrap_or(false))
    }

    pub fn proc_name(&self, pid: u32) -> String {
        self.procs
            .read()
            .ok()
            .and_then(|m| m.get(&pid).cloned())
            .unwrap_or_else(|| if pid == 0 { "Idle".into() } else if pid == 4 { "System".into() } else { format!("pid{pid}") })
    }

    pub fn set_collector(&self, name: &str, status: impl Into<String>) {
        self.live.lock().unwrap().collectors.insert(name.to_string(), status.into());
    }

    /// Sleeps in small slices so stop requests are honoured quickly. Returns false if stopping.
    pub fn sleep_ms(&self, ms: u64) -> bool {
        let mut left = ms;
        while left > 0 {
            if self.stopping() {
                return false;
            }
            let step = left.min(100);
            std::thread::sleep(std::time::Duration::from_millis(step));
            left -= step;
        }
        !self.stopping()
    }
}

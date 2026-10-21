//! State shared by all collectors during a capture.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, RwLock};

use crate::session::Session;

#[derive(Clone, Debug)]
pub struct Config {
    /// Executable names (case-insensitive) of the game to watch.
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            processes: vec!["Discovery.exe".into()],
            hotkey: "F8".into(),
            probe_hz: 20,
            probes: true,
            out_dir: None,
            duration_s: None,
            zip: true,
            open_report: true,
            extra_targets: vec![],
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
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    pub top_talker: Option<String>,
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
        }
    }

    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
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

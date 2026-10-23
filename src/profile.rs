//! Per-game profiles: which program to watch and a few game-specific analysis settings.
//! Built-in profiles are compiled in; more can be added as JSON files in a `profiles`
//! folder next to the exe (same fields, see README). A profile with no `processes` is the
//! auto-detect profile: the game is found at runtime (the process presenting frames).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchDetection {
    /// Match play = seconds in which the client sends at least half its usual peak rate.
    SendRate,
    /// Treat the whole session as play.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Short id used on the command line (`--game <id>`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Executable names (case-insensitive). Empty = auto-detect.
    #[serde(default)]
    pub processes: Vec<String>,
    /// Remote UDP ports that are never the game server (e.g. latency beacons).
    #[serde(default)]
    pub ignore_ports: Vec<u16>,
    #[serde(default = "send_rate")]
    pub match_detection: MatchDetection,
    /// Smallest silence in the server stream that counts as a stall.
    #[serde(default = "d_gap")]
    pub min_gap_ms: f64,
    /// Packets arriving within this many ms count as one server tick (for jitter).
    #[serde(default = "d_merge")]
    pub tick_merge_ms: f64,
    /// Shown with marks and in the instructions: what to look out for in this game.
    #[serde(default)]
    pub hint: String,
}

fn send_rate() -> MatchDetection {
    MatchDetection::SendRate
}
fn d_gap() -> f64 {
    80.0
}
fn d_merge() -> f64 {
    5.0
}

impl Profile {
    pub fn is_auto(&self) -> bool {
        self.processes.is_empty()
    }
}

const BUILTIN: &str = r#"[
  {
    "id": "the_finals",
    "name": "THE FINALS",
    "processes": ["Discovery.exe"],
    "ignore_ports": [7600],
    "hint": "Note which red icon (Packet Loss, High Latency, High Jitter, Server Performance, ...) shows in the top-right corner."
  },
  {
    "id": "cs2",
    "name": "Counter-Strike 2",
    "processes": ["cs2.exe"],
    "hint": "Turn on the in-game network graph (Settings > Game > Telemetry) and note what it shows at each hiccup."
  },
  {
    "id": "auto",
    "name": "Auto-detect (any game)",
    "processes": [],
    "hint": ""
  }
]"#;

pub fn builtin() -> Vec<Profile> {
    serde_json::from_str(BUILTIN).expect("built-in profiles are valid JSON")
}

/// Built-ins plus JSON files from `dirs` (a file with the same id replaces a built-in).
/// Returns the profiles and any problems with user files.
pub fn load_all(dirs: &[std::path::PathBuf]) -> (Vec<Profile>, Vec<String>) {
    let mut v = builtin();
    let mut errors = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for f in files {
            match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<Profile>(&s).map_err(|e| e.to_string())) {
                Ok(p) => {
                    v.retain(|x| x.id != p.id);
                    // Keep auto-detect last in menus.
                    let at = v.iter().position(|x| x.is_auto()).unwrap_or(v.len());
                    v.insert(at, p);
                }
                Err(e) => errors.push(format!("{}: {e}", f.display())),
            }
        }
    }
    (v, errors)
}

/// Folders with user profiles: `profiles` next to the exe, and %ProgramData%\LagScope\profiles.
pub fn user_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("profiles"))) {
        v.push(d);
    }
    if let Ok(pd) = std::env::var("ProgramData") {
        v.push(std::path::PathBuf::from(pd).join("LagScope").join("profiles"));
    }
    v
}

fn settings_path() -> Option<std::path::PathBuf> {
    std::env::var("ProgramData").ok().map(|p| std::path::PathBuf::from(p).join("LagScope").join("settings.json"))
}

/// One value from %ProgramData%\LagScope\settings.json.
pub fn setting(key: &str) -> Option<serde_json::Value> {
    let s = std::fs::read_to_string(settings_path()?).ok()?;
    serde_json::from_str::<serde_json::Value>(&s).ok()?.get(key).cloned()
}

/// Sets one value, keeping the others.
pub fn set_setting(key: &str, value: serde_json::Value) {
    let Some(p) = settings_path() else { return };
    let mut all: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    // Null removes the setting.
    if value.is_null() {
        all.remove(key);
    } else {
        all.insert(key.into(), value);
    }
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, serde_json::Value::Object(all).to_string());
}

pub fn last_game() -> Option<String> {
    setting("game")?.as_str().map(String::from)
}

pub fn save_last_game(id: &str) {
    set_setting("game", id.into());
}

pub fn find<'a>(all: &'a [Profile], key: &str) -> Option<&'a Profile> {
    let k = key.to_ascii_lowercase();
    all.iter().find(|p| p.id.eq_ignore_ascii_case(&k) || p.name.eq_ignore_ascii_case(&k) || p.processes.iter().any(|x| x.eq_ignore_ascii_case(&k)))
}

/// The profile a recorded session used (older sessions only stored the process list).
pub fn of_session(meta: &serde_json::Value) -> Profile {
    if let Ok(p) = serde_json::from_value::<Profile>(meta["profile"].clone()) {
        return p;
    }
    let procs: Vec<String> = meta["config"]["processes"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let all = builtin();
    procs
        .first()
        .and_then(|n| find(&all, n).cloned())
        .unwrap_or_else(|| Profile { id: "custom".into(), name: procs.join(", "), processes: procs, ..all.last().unwrap().clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_and_lookup() {
        let all = builtin();
        assert_eq!(find(&all, "discovery.exe").unwrap().id, "the_finals");
        assert_eq!(find(&all, "CS2").unwrap().name, "Counter-Strike 2");
        assert!(find(&all, "auto").unwrap().is_auto());
        let old = serde_json::json!({"config": {"processes": ["Discovery.exe"]}});
        assert_eq!(of_session(&old).ignore_ports, vec![7600]);
        let other = serde_json::json!({"config": {"processes": ["Foo.exe"]}});
        assert_eq!(of_session(&other).processes, vec!["Foo.exe".to_string()]);
    }

    #[test]
    fn user_profile_overrides() {
        let dir = std::env::temp_dir().join(format!("lagscope_prof_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("x.json"), r#"{"id":"apex","name":"Apex Legends","processes":["r5apex.exe"],"min_gap_ms":60}"#).unwrap();
        std::fs::write(dir.join("bad.json"), "{").unwrap();
        let (all, errs) = load_all(&[dir.clone()]);
        assert_eq!(errs.len(), 1);
        let p = find(&all, "apex").unwrap();
        assert_eq!((p.min_gap_ms, p.tick_merge_ms, p.match_detection.clone()), (60.0, 5.0, MatchDetection::SendRate));
        assert!(all.last().unwrap().is_auto());
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! Side-by-side comparison of two recorded sessions (e.g. before/after a fix, Wi-Fi vs
//! cable), from their analysis.json / meta.json.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Row {
    pub label: &'static str,
    pub a: String,
    pub b: String,
    /// +1: B is better, -1: B is worse, 0: same / not comparable.
    pub change: i8,
}

pub struct Comparison {
    pub a_name: String,
    pub b_name: String,
    pub rows: Vec<Row>,
    pub summary: String,
}

struct Side {
    meta: Value,
    an: Value,
}

fn load(dir: &Path) -> Result<Side> {
    if !dir.join("analysis.json").exists() {
        super::run(dir)?;
    }
    let read = |n: &str| -> Result<Value> {
        let t = std::fs::read_to_string(dir.join(n)).with_context(|| format!("reading {}", dir.join(n).display()))?;
        Ok(serde_json::from_str(&t)?)
    };
    Ok(Side { meta: read("meta.json")?, an: read("analysis.json")? })
}

impl Side {
    fn probe(&self, target: &str, field: &str) -> Option<f64> {
        self.an["probes"].as_array()?.iter().find(|p| p["target"] == target)?.get(field)?.as_f64()
    }
    fn loss_pct(&self, target: &str) -> Option<f64> {
        let p = self.an["probes"].as_array()?.iter().find(|p| p["target"] == target)?;
        let sent = p["sent"].as_f64()?;
        (sent > 0.0).then(|| p["lost"].as_f64().unwrap_or(0.0) * 100.0 / sent)
    }
    fn play_min(&self) -> Option<f64> {
        let s = &self.an["stream"];
        let m = s["match_s"].as_f64().filter(|&m| m > 0.0).or(s["active_s"].as_f64())?;
        (m > 0.0).then_some(m / 60.0)
    }
    fn stalls_per_min(&self) -> Option<f64> {
        if self.an["stream"]["in_packets"].as_u64().unwrap_or(0) == 0 {
            return None;
        }
        Some(self.an["stream"]["gaps_100ms"].as_f64()? / self.play_min()?.max(1.0))
    }
    fn game(&self) -> String {
        if self.meta["wifi_check_plan"].is_array() {
            return "Wi-Fi check".into();
        }
        self.meta["detected_games"][0].as_str().map(String::from).unwrap_or_else(|| crate::profile::of_session(&self.meta).name)
    }
    fn wifi(&self) -> String {
        let w = &self.meta["wlan_summary"];
        let band = w["Band"].as_str().unwrap_or("");
        let ch = w["Channel"].as_str().unwrap_or("");
        match (self.meta["route"]["is_wifi"].as_bool(), band.is_empty()) {
            (Some(false), _) => "wired".into(),
            (_, false) => format!("{band}, channel {ch}"),
            _ => "-".into(),
        }
    }
    fn router_pattern(&self) -> String {
        let dur = self.meta["duration_s"].as_f64().unwrap_or(f64::INFINITY);
        self.an["periodicity"]
            .as_array()
            .and_then(|v| v.iter().find(|p| p["what"].as_str().unwrap_or("").starts_with("gateway") && p["score"].as_f64().unwrap_or(0.0) >= 0.8 && p["events"].as_u64().unwrap_or(0) >= 10))
            .and_then(|p| p["period_s"].as_f64())
            .filter(|&s| s * 4.0 <= dur)
            .map(|s| format!("every {s:.1} s"))
            .unwrap_or_else(|| "none".into())
    }
    fn worst_driver(&self) -> Option<f64> {
        let d = self.meta["driver_latency"]["drivers"].as_array()?.first()?;
        Some(d["dpc_max_us"].as_f64().unwrap_or(0.0).max(d["isr_max_us"].as_f64().unwrap_or(0.0)) / 1000.0)
    }
    fn hitches_per_min(&self) -> Option<f64> {
        let n = self.an["frames"]["hitches_50ms"].as_f64()?;
        (self.an["frames"]["frames"].as_u64().unwrap_or(0) > 0).then(|| n / self.play_min().unwrap_or(1.0).max(1.0))
    }
}

/// `lower`: smaller is better. `tol`: changes smaller than this (absolute) count as equal.
fn num(label: &'static str, a: Option<f64>, b: Option<f64>, unit: &str, digits: usize, lower: bool, tol: f64) -> Row {
    let f = |v: Option<f64>| v.map(|x| format!("{x:.digits$}{unit}")).unwrap_or_else(|| "-".into());
    let change = match (a, b) {
        (Some(x), Some(y)) if (x - y).abs() > tol.max(x.abs() * 0.1) => {
            if (y < x) == lower { 1 } else { -1 }
        }
        _ => 0,
    };
    Row { label, a: f(a), b: f(b), change }
}

fn text(label: &'static str, a: String, b: String) -> Row {
    Row { label, a, b, change: 0 }
}

pub fn compare(a_dir: &Path, b_dir: &Path) -> Result<Comparison> {
    let (a, b) = (load(a_dir)?, load(b_dir)?);
    let name = |d: &Path| d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut rows = vec![
        text("Recorded", a.meta["start_local"].as_str().unwrap_or("?").chars().take(16).collect(), b.meta["start_local"].as_str().unwrap_or("?").chars().take(16).collect()),
        text("Game", a.game(), b.game()),
        num("Length (min)", a.meta["duration_s"].as_f64().map(|s| s / 60.0), b.meta["duration_s"].as_f64().map(|s| s / 60.0), "", 1, true, f64::INFINITY),
        num("Match play (min)", a.play_min(), b.play_min(), "", 1, true, f64::INFINITY),
        text("Wi-Fi", a.wifi(), b.wifi()),
        num("Game stalls (100 ms+) per min", a.stalls_per_min(), b.stalls_per_min(), "", 1, true, 0.3),
        num("Jitter", a.an["stream"]["jitter_ms"].as_f64(), b.an["stream"]["jitter_ms"].as_f64(), " ms", 1, true, 0.5),
        num("Router ping, typical", a.probe("gateway", "p50_ms"), b.probe("gateway", "p50_ms"), " ms", 1, true, 1.0),
        num("Router ping, worst 1%", a.probe("gateway", "p99_ms"), b.probe("gateway", "p99_ms"), " ms", 0, true, 5.0),
        num("Router ping lost", a.loss_pct("gateway"), b.loss_pct("gateway"), " %", 2, true, 0.1),
        text("Router freeze pattern", a.router_pattern(), b.router_pattern()),
        num("Internet ping, worst 1%", a.probe("internet", "p99_ms"), b.probe("internet", "p99_ms"), " ms", 0, true, 5.0),
        num("Game region ping", a.an["beacons"][0]["p50_ms"].as_f64(), b.an["beacons"][0]["p50_ms"].as_f64(), " ms", 0, true, 3.0),
        num("Average FPS", a.an["frames"]["avg_fps"].as_f64().filter(|&x| x > 0.0), b.an["frames"]["avg_fps"].as_f64().filter(|&x| x > 0.0), "", 0, false, 5.0),
        num("1% low FPS", a.an["frames"]["low1_fps"].as_f64().filter(|&x| x > 0.0), b.an["frames"]["low1_fps"].as_f64().filter(|&x| x > 0.0), "", 0, false, 5.0),
        num("Frame hitches (50 ms+) per min", a.hitches_per_min(), b.hitches_per_min(), "", 1, true, 0.3),
        num("Worst driver run", a.worst_driver(), b.worst_driver(), " ms", 2, true, 0.3),
        num("Marks", a.meta["marks"].as_f64(), b.meta["marks"].as_f64(), "", 0, true, f64::INFINITY),
        text("Verdict", a.an["headline"].as_str().unwrap_or("").into(), b.an["headline"].as_str().unwrap_or("").into()),
    ];
    // Router pattern: gone = better.
    if let Some(r) = rows.iter_mut().find(|r| r.label == "Router freeze pattern") {
        r.change = match (r.a == "none", r.b == "none") {
            (false, true) => 1,
            (true, false) => -1,
            _ => 0,
        };
    }
    let better: Vec<&Row> = rows.iter().filter(|r| r.change > 0).collect();
    let worse: Vec<&Row> = rows.iter().filter(|r| r.change < 0).collect();
    let list = |v: &[&Row]| v.iter().map(|r| format!("{} {} -> {}", r.label.to_lowercase(), r.a, r.b)).collect::<Vec<_>>().join("; ");
    let summary = match (better.is_empty(), worse.is_empty()) {
        (true, true) => "No meaningful difference between the two sessions.".to_string(),
        (false, true) => format!("The second session is better: {}.", list(&better)),
        (true, false) => format!("The second session is worse: {}.", list(&worse)),
        (false, false) => format!("Better: {}. Worse: {}.", list(&better), list(&worse)),
    };
    Ok(Comparison { a_name: name(a_dir), b_name: name(b_dir), rows, summary })
}

/// Plain-text table for the console.
pub fn to_text(c: &Comparison) -> String {
    let w = c.rows.iter().map(|r| r.a.chars().count()).max().unwrap_or(10).clamp(10, 40);
    let mut out = format!("{:<30} {:<w$}  {}\n", "", "A", "B");
    out.push_str(&format!("{:<30} {:<w$}  {}\n", "", c.a_name, c.b_name));
    for r in &c.rows {
        let mark = match r.change {
            1 => "  (better)",
            -1 => "  (worse)",
            _ => "",
        };
        let a: String = r.a.chars().take(w).collect();
        out.push_str(&format!("{:<30} {:<w$}  {}{mark}\n", r.label, a, r.b));
    }
    out.push('\n');
    out.push_str(&c.summary);
    out.push('\n');
    out
}

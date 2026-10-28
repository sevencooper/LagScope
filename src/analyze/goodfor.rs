//! "Good for" gaming, video calls and streaming: lagscope-core's grades (Cloudflare's network
//! quality steps plus the freeze cap), the same as LagScope Pulse shows. The words follow core's
//! design/wording.md ("Good for").
//!
//! A recording measures the idle round trip, jitter and loss itself (internet pings); ping
//! under load and download speed come from the busy-connection test (Network tab) on the same
//! router closest in time, within two weeks.

use std::path::Path;

use lagscope_core::quality::{Grade, Inputs, Note, Quality, Use, grade_inputs};
use serde::Serialize;
use serde_json::Value;

use super::detect::Analysis;
use super::load::Data;

/// A busy-connection test counts for recordings this close in time (ms).
const LOAD_TEST_MAX_AGE_MS: i64 = 14 * 24 * 3600 * 1000;

/// One grade, ready to show.
#[derive(Debug, Clone, Serialize)]
pub struct GoodFor {
    /// "gaming", "video_calls", "streaming".
    pub key: &'static str,
    /// "Gaming", "Video calls", "Streaming".
    pub name: &'static str,
    /// "bad" .. "great"; None = not measured yet.
    pub grade: Option<&'static str>,
    /// "Bad" .. "Great", or "Not measured yet".
    pub grade_text: &'static str,
    /// Why a grade is missing or capped.
    pub note: Option<String>,
}

/// Shown under the grades.
pub const FOOTNOTE: &str = "Graded like Cloudflare's network quality score.";

fn show(q: &[Quality], what: &str) -> Vec<GoodFor> {
    const RUN: &str = "Run the busy-connection test (Network tab)";
    q.iter()
        .map(|q| {
            let (key, name) = match q.use_ {
                Use::Gaming => ("gaming", "Gaming"),
                Use::VideoCalls => ("video_calls", "Video calls"),
                Use::Streaming => ("streaming", "Streaming"),
            };
            let (grade, grade_text) = match q.grade {
                Some(Grade::Bad) => (Some("bad"), "Bad"),
                Some(Grade::Poor) => (Some("poor"), "Poor"),
                Some(Grade::Average) => (Some("average"), "Average"),
                Some(Grade::Good) => (Some("good"), "Good"),
                Some(Grade::Great) => (Some("great"), "Great"),
                None => (None, "Not measured yet"),
            };
            let note = q.note.map(|n| match n {
                Note::MissingLoad => format!("{RUN} to include ping under load."),
                Note::MissingSpeed => format!("{RUN} to include download speed."),
                Note::FreezeCap => format!("Would be better without the freezes this {what} found."),
                Note::FreezesHurt => format!("The freezes this {what} found hurt games. {RUN} to include ping under load."),
            });
            GoodFor { key, name, grade, grade_text, note }
        })
        .collect()
}

/// What a busy-connection test adds: (idle round trip, jitter, loss %, increase under load,
/// download bits/s), from its saved form (`LoadResult` as JSON). The increase is None when no
/// internet ping was answered under load (a median of 0 there means "no answers").
pub fn load_test_inputs(l: &Value) -> Option<(f64, f64, f64, Option<f64>, f64)> {
    let f = |a: &str, b: &str| l[a][b].as_f64();
    let idle = f("idle", "internet_p50")?;
    let rise = |phase: &str| f(phase, "internet_p50").filter(|&p| p > 0.0).map(|p| (p - idle).max(0.0));
    let increase = match (rise("download"), rise("upload")) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    let samples = f("idle", "samples").unwrap_or(0.0);
    let loss = if samples > 0.0 { f("idle", "lost").unwrap_or(0.0) * 100.0 / samples } else { 0.0 };
    Some((idle, f("idle", "internet_jitter").unwrap_or(0.0), loss, increase, l["down_mbps"].as_f64()? * 1e6))
}

/// The grades of a busy-connection test on its own (Network tab).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn for_load_test(l: &Value) -> Vec<GoodFor> {
    let Some((latency_ms, jitter_ms, loss_pct, increase, down)) = load_test_inputs(l) else { return Vec::new() };
    let i = Inputs { latency_ms, jitter_ms, loss_pct, load_increase_ms: increase, download_bps: Some(down), froze: false };
    show(&grade_inputs(&i), "test")
}

/// The busy-connection test closest in time to this recording, on the same router: (its
/// saved result, when it ran).
fn nearest_load_test(d: &Data) -> Option<(Value, String)> {
    let gateway = d.meta["route"]["gateway"].as_str()?;
    let start = d.meta["start_unix_ms"].as_i64()?;
    let parent = d.dir.parent()?;
    let mut best: Option<(i64, Value, String)> = None;
    for e in std::fs::read_dir(parent).ok()?.flatten() {
        if !e.file_name().to_string_lossy().starts_with("LagScope_") || e.path() == d.dir {
            continue;
        }
        let Some(m) = read_json(&e.path().join("meta.json")) else { continue };
        // The test stores the router it ran behind; older ones only have the router lookup.
        let tested = m["load_test"]["gateway"].as_str().or(m["router"]["gateway"].as_str());
        if m["kind"] != "network_test" || m["load_test"].is_null() || tested != Some(gateway) {
            continue;
        }
        let dist = (m["start_unix_ms"].as_i64().unwrap_or(0) - start).abs();
        if dist <= LOAD_TEST_MAX_AGE_MS && best.as_ref().is_none_or(|b| dist < b.0) {
            best = Some((dist, m["load_test"].clone(), m["start_local"].as_str().unwrap_or("").chars().take(16).collect()));
        }
    }
    best.map(|(_, l, when)| (l, when))
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// The grades of a recording, from its internet pings, the freezes it found and the nearest
/// busy-connection test. Empty when the internet was not pinged (or for a Wi-Fi check).
pub fn grade(d: &Data, r: &mut Analysis, froze: bool) {
    if r.wifi_check.is_some() {
        return;
    }
    let Some(net) = r.probes.iter().find(|p| p.target == "internet" && p.sent > p.lost) else { return };
    let mut i = Inputs { latency_ms: net.p50_ms, jitter_ms: net.jitter_ms, loss_pct: net.lost as f64 * 100.0 / net.sent as f64, froze, ..Default::default() };
    if let Some((l, when)) = nearest_load_test(d) {
        if let Some((_, _, _, increase, down)) = load_test_inputs(&l) {
            i.load_increase_ms = increase;
            i.download_bps = Some(down);
            r.good_for_from = Some(if increase.is_some() {
                format!("Ping under load and speed from the busy-connection test of {when}.")
            } else {
                format!("Speed from the busy-connection test of {when} (its ping under load could not be measured).")
            });
        }
    }
    r.good_for = show(&grade_inputs(&i), "recording");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_test_grades() {
        let ph = |p50: f64| json!({ "internet_p50": p50, "internet_jitter": 2.0, "lost": 0, "samples": 100 });
        let l = json!({ "idle": ph(12.0), "download": ph(20.0), "upload": ph(90.0), "down_mbps": 300.0 });
        let (idle, _, loss, inc, down) = load_test_inputs(&l).unwrap();
        assert_eq!((idle, loss, inc, down), (12.0, 0.0, Some(78.0), 3e8));
        let g = for_load_test(&l);
        // 10 (latency) + 10 (loss) + 0 (increase 78 ms) = 20: Average for gaming.
        assert_eq!((g[0].name, g[0].grade_text), ("Gaming", "Average"));
        assert!(g.iter().all(|g| g.note.is_none()));
        // No answers while downloading (median 0): only the upload phase counts.
        let l = json!({ "idle": ph(12.0), "download": ph(0.0), "upload": ph(30.0), "down_mbps": 300.0 });
        assert_eq!(load_test_inputs(&l).unwrap().3, Some(18.0));
    }
}

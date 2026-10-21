//! Wi-Fi check: scores the router-ping test windows (baseline, then one change at a time).
//! Shared by the live check (to tell the user what helped) and the offline report.

use serde::Serialize;

use super::detect::percentile;
use super::load::Data;

#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub key: String,
    pub label: String,
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WindowStats {
    pub key: String,
    pub label: String,
    pub secs: f64,
    pub samples: usize,
    pub lost: usize,
    /// Freeze episodes (consecutive slow or lost pings).
    pub spikes: usize,
    pub per_min: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Change in freezes per minute vs. the baselines (negative = fewer).
    pub change_pct: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Outcome {
    pub windows: Vec<WindowStats>,
    pub threshold_ms: f64,
    /// Freezes per minute with nothing changed (mean of the baseline windows).
    pub baseline_per_min: f64,
    /// The baselines showed the problem (so the tests mean something).
    pub problem: bool,
    /// The problem was present in the first and the last baseline.
    pub stable: bool,
    /// Keys of tests that removed (nearly) all freezes.
    pub fixes: Vec<String>,
    /// Keys of tests that clearly reduced them.
    pub partial: Vec<String>,
    pub summary: Vec<String>,
}

pub fn is_baseline(key: &str) -> bool {
    key.starts_with("baseline")
}

/// A ping counts as a freeze if it is well above the normal router RTT.
pub fn threshold(base_median_ms: f64) -> f64 {
    (base_median_ms * 3.0).max(base_median_ms + 25.0).max(30.0)
}

pub fn stats(samples: &[(i64, Option<f64>)], w: &Window, thr: f64) -> WindowStats {
    let inside: Vec<&(i64, Option<f64>)> = samples.iter().filter(|(t, _)| *t >= w.start && *t < w.end).collect();
    let ok: Vec<f64> = {
        let mut v: Vec<f64> = inside.iter().filter_map(|(_, r)| *r).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let mut spikes = 0;
    let mut last_bad: Option<i64> = None;
    for (t, r) in &inside {
        if r.is_none_or(|r| r > thr) {
            if last_bad.is_none_or(|l| t - l > 400_000) {
                spikes += 1;
            }
            last_bad = Some(*t);
        }
    }
    let secs = (w.end - w.start) as f64 / 1e6;
    WindowStats {
        key: w.key.clone(),
        label: w.label.clone(),
        secs,
        samples: inside.len(),
        lost: inside.len() - ok.len(),
        spikes,
        per_min: if secs > 0.0 { spikes as f64 * 60.0 / secs } else { 0.0 },
        p50_ms: percentile(&ok, 0.5),
        p99_ms: percentile(&ok, 0.99),
        max_ms: ok.last().copied().unwrap_or(0.0),
        change_pct: None,
    }
}

pub fn evaluate(samples: &[(i64, Option<f64>)], windows: &[Window]) -> Outcome {
    let mut base: Vec<f64> = samples
        .iter()
        .filter(|(t, _)| windows.iter().any(|w| is_baseline(&w.key) && *t >= w.start && *t < w.end))
        .filter_map(|(_, r)| *r)
        .collect();
    base.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let thr = threshold(percentile(&base, 0.5));
    let mut ws: Vec<WindowStats> = windows.iter().map(|w| stats(samples, w, thr)).collect();
    let bases: Vec<&WindowStats> = ws.iter().filter(|s| is_baseline(&s.key)).collect();
    let baseline_per_min = if bases.is_empty() { 0.0 } else { bases.iter().map(|s| s.per_min).sum::<f64>() / bases.len() as f64 };
    let problem = baseline_per_min >= 2.0;
    let stable = bases.len() >= 2 && bases.iter().all(|s| s.per_min >= 1.5);
    let mut fixes = Vec::new();
    let mut partial = Vec::new();
    for s in ws.iter_mut() {
        if is_baseline(&s.key) || s.samples < 100 {
            continue;
        }
        if baseline_per_min > 0.0 {
            s.change_pct = Some((s.per_min / baseline_per_min - 1.0) * 100.0);
        }
        if !problem {
            continue;
        }
        if s.per_min <= (baseline_per_min * 0.2).max(1.0) {
            fixes.push(s.key.clone());
        } else if s.per_min <= baseline_per_min * 0.5 {
            partial.push(s.key.clone());
        }
    }
    let label = |k: &str| ws.iter().find(|s| s.key == k).map(|s| s.label.clone()).unwrap_or_default();
    let mut summary = Vec::new();
    if !problem {
        summary.push(format!(
            "No regular freezes on the Wi-Fi link during the check ({baseline_per_min:.1} per minute with nothing changed). The freeze may only happen while the game runs: start THE FINALS, leave it in the main menu, and run the check again. If that is clean too, record a match (menu option 2)."
        ));
    } else {
        summary.push(format!(
            "With nothing changed, the Wi-Fi link froze {baseline_per_min:.1} times per minute (router ping above {thr:.0} ms)."
        ));
        if !fixes.is_empty() {
            let names: Vec<String> = fixes.iter().map(|k| label(k)).collect();
            summary.push(format!("The freezes went away with: {}.", names.join("; ")));
        } else if !partial.is_empty() {
            let names: Vec<String> = partial.iter().map(|k| label(k)).collect();
            summary.push(format!("Fewer freezes (but not gone) with: {}.", names.join("; ")));
        } else {
            summary.push("None of the PC-side changes stopped the freezes, so the cause is most likely the router (or the Wi-Fi card's firmware): see the router steps.".into());
        }
        if !stable {
            summary.push("Note: the freezes were not present in every baseline window, so treat the result with care (it may come and go).".into());
        }
    }
    Outcome { windows: ws, threshold_ms: thr, baseline_per_min, problem, stable, fixes, partial, summary }
}

/// Rebuilds the test windows from the "exp" events of a recorded Wi-Fi check.
pub fn from_session(d: &Data) -> Option<Outcome> {
    let mut windows: Vec<Window> = Vec::new();
    let mut open: Option<(String, String, i64)> = None;
    for e in d.events.iter().filter(|e| e.kind == "exp") {
        let mut parts = e.detail.splitn(3, '|');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("start"), Some(key), label) => open = Some((key.into(), label.unwrap_or(key).into(), e.t)),
            (Some("end"), Some(key), _) => {
                if let Some((k, l, s)) = open.take().filter(|(k, _, _)| k == key) {
                    windows.push(Window { key: k, label: l, start: s, end: e.t });
                }
            }
            _ => {}
        }
    }
    if windows.is_empty() {
        return None;
    }
    let samples: Vec<(i64, Option<f64>)> = d
        .probes
        .get("gateway")
        .map(|v| v.iter().map(|p| (p.t, (p.rtt >= 0).then(|| p.rtt as f64 / 1000.0))).collect())
        .unwrap_or_default();
    Some(evaluate(&samples, &windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(windows: &[(&str, bool)]) -> (Vec<(i64, Option<f64>)>, Vec<Window>) {
        // 20 Hz pings at 8 ms; a 120 ms freeze every 10 s where `freezes` is set.
        let mut samples = Vec::new();
        let mut ws = Vec::new();
        let mut t0 = 0i64;
        for (key, freezes) in windows {
            let end = t0 + 60_000_000;
            let mut t = t0;
            while t < end {
                let in_freeze = *freezes && (t - t0) % 10_000_000 >= 2_000_000 && (t - t0) % 10_000_000 < 2_150_000;
                samples.push((t, Some(if in_freeze { 120.0 } else { 8.0 })));
                t += 50_000;
            }
            ws.push(Window { key: key.to_string(), label: key.to_string(), start: t0, end });
            t0 = end + 5_000_000;
        }
        (samples, ws)
    }

    #[test]
    fn finds_the_change_that_helps() {
        let (s, w) = synth(&[("baseline1", true), ("autoconfig", true), ("bluetooth", false), ("baseline2", true)]);
        let o = evaluate(&s, &w);
        assert!(o.problem && o.stable);
        assert_eq!(o.fixes, vec!["bluetooth".to_string()]);
        assert!((o.baseline_per_min - 6.0).abs() < 0.1, "{}", o.baseline_per_min);
    }

    #[test]
    fn clean_link_is_no_problem() {
        let (s, w) = synth(&[("baseline1", false), ("bluetooth", false), ("baseline2", false)]);
        let o = evaluate(&s, &w);
        assert!(!o.problem && o.fixes.is_empty());
    }
}

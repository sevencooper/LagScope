//! Saves the on-request network tests (router identification, latency under load, Wi-Fi
//! neighbourhood) as a small session folder - meta.json, analysis.json (headline, findings,
//! suggestions, read by the Sessions tab and `compare`) and a simple report.html.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;

use super::loadtest::LoadResult;
use super::router::RouterInfo;
use super::wifiscan::ScanResult;
use crate::clock;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn save(base: &Path, router: Option<&RouterInfo>, load: Option<&LoadResult>, scan: Option<&ScanResult>) -> Result<PathBuf> {
    let now = clock::unix_ms();
    let off = clock::local_offset_min();
    let dir = base.join(format!("LagScope_{}", clock::format_compact(now, off)));
    std::fs::create_dir_all(&dir)?;
    let mut findings: Vec<String> = Vec::new();
    let mut recs: Vec<String> = Vec::new();
    let mut headline = Vec::new();
    if let Some(l) = load {
        headline.push(format!("bufferbloat grade {} down / {} up, {:.0}/{:.0} Mbit/s", l.grade_down, l.grade_up, l.down_mbps, l.up_mbps));
        findings.extend(l.summary.iter().cloned());
        recs.extend(l.recommendations.iter().cloned());
    }
    if let Some(s) = scan {
        headline.push(format!("{} Wi-Fi networks nearby, {} on your channel", s.total_networks, s.own_channel_neighbours));
        findings.extend(s.summary.iter().cloned());
    }
    if let Some(r) = router {
        findings.push(format!("Router: {} at {}{}.", if r.name.is_empty() { "unknown" } else { &r.name }, r.gateway, if r.isp.is_empty() { String::new() } else { format!(", ISP {}", r.isp) }));
        recs.extend(r.notes.iter().cloned());
    }
    let headline = format!("Network test: {}", if headline.is_empty() { "router identified".to_string() } else { headline.join("; ") });
    let meta = json!({
        "kind": "network_test",
        "tool_version": env!("CARGO_PKG_VERSION"),
        "start_unix_ms": now,
        "start_local": clock::format_wall(now, off),
        "router": router,
        "load_test": load,
        "wifi_scan": scan,
    });
    let analysis = json!({ "headline": headline, "findings": findings, "recommendations": recs });
    std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;
    std::fs::write(dir.join("analysis.json"), serde_json::to_string_pretty(&analysis)?)?;

    // A plain, self-contained page.
    let mut h = String::from("<!doctype html><meta charset=utf-8><title>LagScope network test</title><style>body{font:15px system-ui,sans-serif;max-width:900px;margin:24px auto;padding:0 16px;background:#16181c;color:#dde}h1{font-size:22px}td,th{padding:3px 10px;text-align:left;border-bottom:1px solid #333}table{border-collapse:collapse;margin:8px 0}.w{color:#9ab}</style>");
    h.push_str(&format!("<h1>{}</h1><p class=w>{}</p>", esc(&headline), esc(meta["start_local"].as_str().unwrap_or(""))));
    h.push_str("<h2>Findings</h2><ul>");
    for f in &findings {
        h.push_str(&format!("<li>{}</li>", esc(f)));
    }
    h.push_str("</ul>");
    if !recs.is_empty() {
        h.push_str("<h2>Suggestions</h2><ul>");
        for r in &recs {
            h.push_str(&format!("<li>{}</li>", esc(r)));
        }
        h.push_str("</ul>");
    }
    if let Some(l) = load {
        h.push_str("<h2>Latency under load</h2><table><tr><th></th><th>router ping (median / 95%)</th><th>internet ping (median / 95%)</th><th>lost</th></tr>");
        for (n, p) in [("idle", &l.idle), ("downloading", &l.download), ("uploading", &l.upload)] {
            h.push_str(&format!("<tr><td>{n}</td><td>{:.0} / {:.0} ms</td><td>{:.0} / {:.0} ms</td><td>{} of {}</td></tr>", p.router_p50, p.router_p95, p.internet_p50, p.internet_p95, p.lost, p.samples));
        }
        h.push_str("</table>");
    }
    if let Some(s) = scan {
        h.push_str("<h2>Wi-Fi neighbourhood</h2>");
        for b in &s.bands {
            if b.channels.is_empty() {
                continue;
            }
            h.push_str(&format!("<h3>{} ({} networks)</h3><table><tr><th>channel</th><th>networks</th><th>strongest</th><th>congestion</th></tr>", b.band, b.networks));
            for c in &b.channels {
                let own = if b.band == s.own_band && c.channel == s.own_channel { " (yours)" } else { "" };
                let best = if b.best.is_some_and(|x| x.0 == c.channel) { " (quietest)" } else { "" };
                h.push_str(&format!("<tr><td>{}{own}{best}</td><td>{}</td><td>{}</td><td>{:.1}</td></tr>", c.channel, c.networks, if c.networks > 0 { format!("{} dBm", c.strongest_dbm) } else { "-".into() }, c.score));
            }
            h.push_str("</table>");
        }
    }
    h.push_str(&format!("<p class=w>LagScope {}</p>", env!("CARGO_PKG_VERSION")));
    std::fs::write(dir.join("report.html"), h)?;
    Ok(dir)
}

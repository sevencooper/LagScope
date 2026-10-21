//! Self-contained offline HTML report (uPlot is embedded).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::detect::{Analysis, server_endpoints};
use super::load::Data;

const UPLOT_JS: &str = include_str!("../report/uPlot.iife.min.js");
const UPLOT_CSS: &str = include_str!("../report/uPlot.min.css");
const TEMPLATE: &str = include_str!("../report/template.html");

struct Buckets {
    width: i64,
    n: usize,
}

impl Buckets {
    fn idx(&self, t: i64) -> Option<usize> {
        let i = t.div_euclid(self.width);
        (i >= 0 && (i as usize) < self.n).then_some(i as usize)
    }
}

fn max_into(v: &mut [Option<f64>], i: Option<usize>, x: f64) {
    if let Some(i) = i {
        v[i] = Some(v[i].map_or(x, |o: f64| o.max(x)));
    }
}

fn round(v: Vec<Option<f64>>, digits: i32) -> Value {
    let m = 10f64.powi(digits);
    Value::Array(v.into_iter().map(|x| x.map(|x| json!((x * m).round() / m)).unwrap_or(Value::Null)).collect())
}

fn series(d: &Data, a: &Analysis) -> Value {
    let width = if d.duration_us > 2 * 3_600_000_000 { 1_000_000 } else { 250_000 };
    let b = Buckets { width, n: (d.duration_us / width + 1).max(1) as usize };
    let n = b.n;
    let empty = || vec![None::<f64>; n];
    let t: Vec<f64> = (0..n).map(|i| (i as i64 * width) as f64 / 1e6).collect();

    let servers = server_endpoints(d);
    let (mut in_gap, mut out_gap, mut in_pps, mut game_kbps) = (empty(), empty(), empty(), empty());
    let (mut last_in, mut last_out) = (None::<i64>, None::<i64>);
    for p in d.packets.iter().filter(|p| servers.contains(&p.ep) && p.udp) {
        let i = b.idx(p.t);
        if p.send {
            if let Some(l) = last_out {
                if p.t - l < 5_000_000 {
                    max_into(&mut out_gap, i, (p.t - l) as f64 / 1000.0);
                }
            }
            last_out = Some(p.t);
        } else {
            if let Some(l) = last_in {
                if p.t - l < 5_000_000 {
                    max_into(&mut in_gap, i, (p.t - l) as f64 / 1000.0);
                }
            }
            last_in = Some(p.t);
            if let Some(i) = i {
                in_pps[i] = Some(in_pps[i].unwrap_or(0.0) + 1e6 / width as f64);
            }
        }
        if let Some(i) = i {
            game_kbps[i] = Some(game_kbps[i].unwrap_or(0.0) + p.size as f64 * 8.0 / 1000.0 * 1e6 / width as f64);
        }
    }

    let mut frame = empty();
    for w in d.frames.windows(2) {
        let g = w[1].0 - w[0].0;
        if g < 2_000_000 {
            max_into(&mut frame, b.idx(w[1].0), g as f64 / 1000.0);
        }
    }

    let mut probes = serde_json::Map::new();
    let mut loss = serde_json::Map::new();
    for (target, v) in &d.probes {
        let mut s = empty();
        let mut l = empty();
        for p in v {
            if p.rtt >= 0 {
                max_into(&mut s, b.idx(p.t), p.rtt as f64 / 1000.0);
            } else if let Some(i) = b.idx(p.t) {
                l[i] = Some(l[i].unwrap_or(0.0) + 1.0);
            }
        }
        probes.insert(target.clone(), round(s, 1));
        // A target that never answered (host ignores pings) would drown out real losses.
        if v.iter().any(|p| p.rtt >= 0) {
            loss.insert(target.clone(), round(l, 0));
        }
    }

    let mut quality = empty();
    for &(tq, q) in &d.link_quality {
        if let Some(i) = b.idx(tq) {
            quality[i] = Some(q);
        }
    }
    let mut rssi = empty();
    let mut rate = empty();
    for w in &d.wifi {
        if let (Some(i), Some(r)) = (b.idx(w.t), w.rssi) {
            rssi[i] = Some(r);
        }
        if let (Some(i), Some(r)) = (b.idx(w.t), w.tx_mbps) {
            rate[i] = Some(r);
        }
    }
    // Drivers that cache RSSI / rate report a constant: hide it so it doesn't flatten the chart.
    for v in [&mut rssi, &mut rate] {
        let vals: Vec<f64> = v.iter().flatten().copied().collect();
        if vals.len() > 10 && vals.iter().all(|&x| x == vals[0]) {
            v.iter_mut().for_each(|x| *x = None);
        }
    }
    // Carry quality forward (WLAN ETW only reports changes).
    let mut lastq = None;
    for q in quality.iter_mut() {
        if q.is_some() {
            lastq = *q;
        } else {
            *q = lastq;
        }
    }

    let mut stall = empty();
    for &(ts, m) in &d.stall_1s {
        max_into(&mut stall, b.idx(ts + 500_000), m as f64 / 1000.0);
    }
    for &(ts, l) in &d.stalls {
        max_into(&mut stall, b.idx(ts), l as f64 / 1000.0);
    }
    let (mut dpc, mut cpu, mut maxcore, mut gpu, mut gpupow) = (empty(), empty(), empty(), empty(), empty());
    for c in &d.cpu {
        let i = b.idx(c.t);
        if let Some(x) = c.dpc {
            max_into(&mut dpc, i, x + c.intr.unwrap_or(0.0));
        }
        if let Some(x) = c.total {
            max_into(&mut cpu, i, x);
        }
        if let Some(x) = c.max_core {
            max_into(&mut maxcore, i, x);
        }
    }
    for g in &d.gpu {
        let i = b.idx(g.t);
        if let Some(x) = g.util {
            max_into(&mut gpu, i, x);
        }
        if let Some(x) = g.power {
            max_into(&mut gpupow, i, x);
        }
    }
    let game = d.game_pids();
    let mut other = empty();
    let mut per_sec: BTreeMap<i64, u64> = BTreeMap::new();
    for p in d.proc_net.iter().filter(|p| !game.contains(&p.pid) && p.pid != 0) {
        *per_sec.entry(p.t).or_default() += p.tx + p.rx;
    }
    for (ts, bytes) in per_sec {
        let mbit = bytes as f64 * 8.0 / 1e6;
        let mut k = ts;
        while k < ts + 1_000_000 {
            if let Some(i) = b.idx(k) {
                other[i] = Some(mbit);
            }
            k += width;
        }
    }

    let mut region = empty();
    if let Some(bc) = a.beacons.first() {
        for &(ts, ms) in &bc.samples {
            max_into(&mut region, b.idx(ts), ms);
        }
    }

    json!({
        "t": t,
        "region": if a.beacons.is_empty() { Value::Null } else { round(region, 1) },
        "in_gap": round(in_gap, 1), "out_gap": round(out_gap, 1), "in_pps": round(in_pps, 0), "game_kbps": round(game_kbps, 0),
        "frame": round(frame, 1),
        "probes": probes, "loss": loss,
        "quality": round(quality, 0), "rssi": round(rssi, 0), "tx_rate": round(rate, 0),
        "stall": round(stall, 1), "dpc": round(dpc, 2), "cpu": round(cpu, 0), "maxcore": round(maxcore, 0),
        "gpu": round(gpu, 0), "gpu_power": round(gpupow, 0), "other_mbit": round(other, 2),
    })
}

pub fn render(d: &Data, a: &Analysis) -> String {
    // Keep the anomaly list bounded for very long/noisy sessions.
    let mut anomalies: Vec<&super::detect::Anomaly> = a.anomalies.iter().collect();
    if anomalies.len() > 5000 {
        anomalies.sort_by(|x, y| y.value.partial_cmp(&x.value).unwrap());
        anomalies.truncate(5000);
        anomalies.sort_by_key(|x| x.t);
    }
    let anomalies: Vec<Value> = anomalies
        .iter()
        .map(|x| json!({"t": x.t as f64 / 1e6, "dur": x.dur as f64 / 1e6, "kind": x.kind.label(), "target": x.target, "value": x.value, "detail": x.detail, "in_match": x.in_match}))
        .collect();
    let events: Vec<Value> = d.events.iter().map(|e| json!({"t": e.t as f64 / 1e6, "kind": e.kind, "detail": e.detail})).collect();
    let data = json!({
        "meta": d.meta,
        "duration_s": d.duration_us as f64 / 1e6,
        "headline": a.headline,
        "findings": a.findings,
        "recommendations": a.recommendations,
        "stream": a.stream,
        "frames": a.frames,
        "probes": a.probes,
        "verdicts": a.verdicts.iter().map(|v| json!({"t": v.t as f64 / 1e6, "n": v.n, "culprit": v.culprit, "category": v.category, "confidence": v.confidence, "evidence": v.evidence})).collect::<Vec<_>>(),
        "gap_correlation": a.gap_correlation,
        "gateway_spike_correlation": a.gateway_spike_correlation,
        "periodicity": a.periodicity,
        "anomalies": anomalies,
        "events": events,
        "series": series(d, a),
        "phases": a.phases.iter().map(|(x, y)| [*x as f64 / 1e6, *y as f64 / 1e6]).collect::<Vec<_>>(),
        "beacons": a.beacons.iter().take(10).collect::<Vec<_>>(),
        "marks": d.marks.iter().map(|m| *m as f64 / 1e6).collect::<Vec<_>>(),
    });
    // Escape "</" so embedded strings cannot terminate the script tag.
    let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into()).replace("</", "<\\/");
    TEMPLATE
        .replace("/*UPLOT_CSS*/", UPLOT_CSS)
        .replace("/*UPLOT_JS*/", UPLOT_JS)
        .replace("/*DATA*/", &json)
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
}

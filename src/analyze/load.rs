//! Loads a session directory into typed, time-sorted vectors. Missing files are fine
//! (collector unavailable); malformed rows are skipped.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};

/// Splits one CSV line, honouring double quotes.
pub fn split_csv(line: &str) -> Vec<String> {
    if !line.contains('"') {
        return line.split(',').map(str::to_string).collect();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, q) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => q = !q,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

pub struct Row<'a> {
    cols: Vec<String>,
    idx: &'a HashMap<String, usize>,
}

impl Row<'_> {
    pub fn s(&self, k: &str) -> &str {
        self.idx.get(k).and_then(|&i| self.cols.get(i)).map(String::as_str).unwrap_or("")
    }
    pub fn i(&self, k: &str) -> Option<i64> {
        self.s(k).trim().parse().ok()
    }
    pub fn f(&self, k: &str) -> Option<f64> {
        self.s(k).trim().parse().ok()
    }
}

/// Calls `f` for every data row of `<dir>/<name>.csv`. Returns false if the file is absent.
pub fn each(dir: &Path, name: &str, mut f: impl FnMut(&Row)) -> Result<bool> {
    let p = dir.join(format!("{name}.csv"));
    let Ok(file) = File::open(&p) else { return Ok(false) };
    let mut lines = BufReader::with_capacity(1 << 20, file).lines();
    let Some(header) = lines.next() else { return Ok(true) };
    let header = header.with_context(|| format!("reading {}", p.display()))?;
    let idx: HashMap<String, usize> = split_csv(header.trim_start_matches('\u{feff}'))
        .into_iter()
        .enumerate()
        .map(|(i, h)| (h, i))
        .collect();
    for line in lines {
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        let row = Row { cols: split_csv(&line), idx: &idx };
        f(&row);
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy)]
pub struct Pkt {
    pub t: i64,
    pub send: bool,
    pub udp: bool,
    pub size: u32,
    pub ep: u32, // index into Data::endpoints
    pub lport: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub t: i64,
    /// Round-trip in microseconds, or -1 for loss/timeout.
    pub rtt: i64,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // loaded for completeness; not all columns feed heuristics yet
pub struct WifiRow {
    pub t: i64,
    pub connected: bool,
    pub bssid: String,
    pub channel: Option<f64>,
    pub rssi: Option<f64>,
    pub quality: Option<f64>,
    pub rx_mbps: Option<f64>,
    pub tx_mbps: Option<f64>,
    pub d_tx: Option<f64>,
    pub d_retry: Option<f64>,
    pub d_failed: Option<f64>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // loaded for completeness; not all columns feed heuristics yet
pub struct CpuRow {
    pub t: i64,
    pub total: Option<f64>,
    pub max_core: Option<f64>,
    pub dpc: Option<f64>,
    pub intr: Option<f64>,
    pub perf: Option<f64>,
    pub freq: Option<f64>,
    pub game_cpu: Option<f64>,
    pub pages_in: Option<f64>,
    pub disk_ms: Option<f64>,
    pub avail_mb: Option<f64>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // loaded for completeness; not all columns feed heuristics yet
pub struct GpuRow {
    pub t: i64,
    pub util: Option<f64>,
    pub power: Option<f64>,
    pub limit: Option<f64>,
    pub temp: Option<f64>,
    pub gfx_mhz: Option<f64>,
    pub pstate: String,
    pub reasons: String,
    pub enc: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ProcNet {
    pub t: i64,
    pub pid: u32,
    pub name: String,
    pub tx: u64,
    pub rx: u64,
}

#[derive(Debug, Clone)]
pub struct Note {
    pub t: i64,
    pub kind: String,
    pub detail: String,
}

#[derive(Default)]
pub struct Data {
    pub meta: serde_json::Value,
    pub duration_us: i64,
    pub packets: Vec<Pkt>,
    pub endpoints: Vec<String>,
    pub frames: Vec<(i64, i64)>,
    pub probes: BTreeMap<String, Vec<Probe>>,
    pub probe_ips: BTreeMap<String, Vec<(i64, String)>>,
    /// Per target: addresses that never answered and how many probes to them were dropped
    /// (e.g. the game server before the tool switched to probing a hop on its path).
    pub probe_silent: BTreeMap<String, Vec<(String, usize)>>,
    pub wifi: Vec<WifiRow>,
    /// Wi-Fi notifications and WLAN ETW events, normalised as (t, name, detail).
    pub wifi_events: Vec<Note>,
    /// Link quality 0-100 from the WLAN API or WLAN ETW.
    pub link_quality: Vec<(i64, f64)>,
    pub stalls: Vec<(i64, i64)>,
    pub stall_1s: Vec<(i64, i64)>,
    pub cpu: Vec<CpuRow>,
    pub gpu: Vec<GpuRow>,
    pub proc_net: Vec<ProcNet>,
    pub marks: Vec<i64>,
    pub events: Vec<Note>,
    /// Commands Windows sent to the network drivers (NDIS OID requests, power requests):
    /// (t, what, pid).
    pub driver_cmds: Vec<(i64, String, u32)>,
    /// Long driver DPC/ISR runs: (start t, is ISR, driver, duration us).
    pub driver_long: Vec<(i64, bool, String, f64)>,
    /// Worst DPC/ISR run per second over all drivers: (t, us).
    pub driver_1s: Vec<(i64, f64)>,
    pub etw_lost: u64,
    pub files: Vec<String>,
}

impl Data {
    pub fn game_pids(&self) -> Vec<u32> {
        // PIDs are not in game_packets; infer from proc_net rows whose name matches.
        let names: Vec<String> = self.meta["config"]["processes"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(|s| s.to_ascii_lowercase()).collect())
            .unwrap_or_default();
        let mut pids: Vec<u32> =
            self.proc_net.iter().filter(|p| names.contains(&p.name.to_ascii_lowercase())).map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }
}

pub fn load(dir: &Path) -> Result<Data> {
    let mut d = Data::default();
    if let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) {
        d.meta = serde_json::from_str(&text).unwrap_or_default();
    }
    let mut ep_idx: HashMap<String, u32> = HashMap::new();
    let mut max_t = 0i64;
    let seen = |name: &str, present: bool, files: &mut Vec<String>| {
        if present {
            files.push(name.to_string());
        }
    };

    let mut files = Vec::new();
    let present = each(dir, "game_packets", |r| {
        let (Some(t), Some(size)) = (r.i("t_us"), r.i("size")) else { return };
        let key = format!("{}:{}", r.s("remote_ip"), r.s("remote_port"));
        let n = ep_idx.len() as u32;
        let ep = *ep_idx.entry(key).or_insert(n);
        d.packets.push(Pkt { t, send: r.s("dir") == "S", udp: r.s("proto") == "U", size: size as u32, ep, lport: r.i("local_port").unwrap_or(0) as u16 });
    })?;
    seen("game_packets", present, &mut files);
    let mut eps: Vec<(String, u32)> = ep_idx.into_iter().collect();
    eps.sort_by_key(|(_, i)| *i);
    d.endpoints = eps.into_iter().map(|(s, _)| s).collect();
    d.packets.sort_by_key(|p| p.t);

    let present = each(dir, "frames", |r| {
        if let (Some(t), Some(p)) = (r.i("t_us"), r.i("present_us")) {
            d.frames.push((t, p));
        }
    })?;
    seen("frames", present, &mut files);
    d.frames.sort_unstable();

    let mut probe_rows: BTreeMap<String, Vec<(Probe, String)>> = BTreeMap::new();
    let present = each(dir, "probes", |r| {
        let (Some(t), Some(status)) = (r.i("t_us"), r.i("status")) else { return };
        let target = r.s("target").to_string();
        // 11013 = TTL expired in transit: a valid reply when probing a hop with a limited TTL.
        let rtt = if status == 0 || status == 11013 { r.i("rtt_us").unwrap_or(-1) } else { -1 };
        // Status 99999 = local slot overflow (not a network loss): skip.
        if status == 99999 {
            return;
        }
        let ips = d.probe_ips.entry(target.clone()).or_default();
        // Unanswered TTL-limited probes carry the destination, not the hop: track replies only.
        if rtt >= 0 && ips.last().is_none_or(|(_, ip)| ip != r.s("ip")) {
            ips.push((t, r.s("ip").to_string()));
        }
        probe_rows.entry(target).or_default().push((Probe { t, rtt }, r.s("ip").to_string()));
    })?;
    seen("probes", present, &mut files);
    for (target, rows) in probe_rows {
        let answered: std::collections::HashSet<&str> = rows.iter().filter(|(p, _)| p.rtt >= 0).map(|(_, ip)| ip.as_str()).collect();
        let mut silent: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, ip) in rows.iter().filter(|(_, ip)| !answered.contains(ip.as_str())) {
            *silent.entry(ip).or_default() += 1;
        }
        if !silent.is_empty() {
            d.probe_silent.insert(target.clone(), silent.into_iter().map(|(ip, n)| (ip.to_string(), n)).collect());
        }
        let kept: Vec<Probe> = rows.iter().filter(|(_, ip)| answered.contains(ip.as_str())).map(|(p, _)| *p).collect();
        if !kept.is_empty() {
            d.probes.insert(target, kept);
        }
    }
    for v in d.probes.values_mut() {
        v.sort_by_key(|p| p.t);
    }

    let present = each(dir, "wifi", |r| {
        let Some(t) = r.i("t_us") else { return };
        let w = WifiRow {
            t,
            connected: r.s("connected") == "1",
            bssid: r.s("bssid_hash").to_string(),
            channel: r.f("channel"),
            rssi: r.f("rssi_dbm"),
            quality: r.f("quality"),
            rx_mbps: r.f("rx_mbps"),
            tx_mbps: r.f("tx_mbps"),
            d_tx: r.f("d_tx"),
            d_retry: r.f("d_retry"),
            d_failed: r.f("d_failed"),
        };
        if let Some(q) = w.quality.filter(|_| w.connected) {
            d.link_quality.push((t, q));
        }
        d.wifi.push(w);
    })?;
    seen("wifi", present, &mut files);

    let present = each(dir, "wifi_events", |r| {
        if let Some(t) = r.i("t_us") {
            d.wifi_events.push(Note { t, kind: r.s("name").to_string(), detail: r.s("data").to_string() });
        }
    })?;
    seen("wifi_events", present, &mut files);
    let api_quality = !d.link_quality.is_empty();
    let present = each(dir, "wlan_etw", |r| {
        let Some(t) = r.i("t_us") else { return };
        let ev = r.s("event").to_string();
        let vals = r.s("values").to_string();
        if ev.eq_ignore_ascii_case("Link Quality") {
            if !api_quality {
                if let Some(q) = vals.strip_prefix("Quality=").and_then(|q| q.trim().parse().ok()) {
                    d.link_quality.push((t, q));
                }
            }
            return;
        }
        // Security packets accompany every link-quality update (key exchange noise).
        if ev.starts_with("Received Security Packet") || ev.starts_with("Enable AutoConfig background scan") {
            return;
        }
        d.wifi_events.push(Note { t, kind: ev, detail: vals });
    })?;
    seen("wlan_etw", present, &mut files);
    d.wifi_events.sort_by_key(|n| n.t);
    d.link_quality.sort_by_key(|q| q.0);

    let present = each(dir, "stalls", |r| {
        if let (Some(t), Some(l)) = (r.i("t_us"), r.i("late_us")) {
            d.stalls.push((t, l));
        }
    })?;
    seen("stalls", present, &mut files);
    let present = each(dir, "stall_1s", |r| {
        if let (Some(t), Some(m)) = (r.i("t_us"), r.i("max_late_us")) {
            d.stall_1s.push((t, m));
        }
    })?;
    seen("stall_1s", present, &mut files);

    let present = each(dir, "cpu", |r| {
        let Some(t) = r.i("t_us") else { return };
        d.cpu.push(CpuRow {
            t,
            total: r.f("total_pct"),
            max_core: r.f("max_core_pct"),
            dpc: r.f("dpc_pct"),
            intr: r.f("interrupt_pct"),
            perf: r.f("perf_pct"),
            freq: r.f("freq_mhz"),
            game_cpu: r.f("game_cpu_pct"),
            pages_in: r.f("pages_in_rate"),
            disk_ms: r.f("disk_ms_per_io"),
            avail_mb: r.f("avail_mb"),
        });
    })?;
    seen("cpu", present, &mut files);

    let present = each(dir, "gpu", |r| {
        let Some(t) = r.i("t_us") else { return };
        if r.i("gpu").unwrap_or(0) != 0 {
            return; // first NVIDIA GPU only
        }
        d.gpu.push(GpuRow {
            t,
            util: r.f("util_pct"),
            power: r.f("power_w"),
            limit: r.f("power_limit_w"),
            temp: r.f("temp_c"),
            gfx_mhz: r.f("gfx_mhz"),
            pstate: r.s("pstate").to_string(),
            reasons: r.s("reasons").to_string(),
            enc: r.f("enc_pct"),
        });
    })?;
    seen("gpu", present, &mut files);

    let present = each(dir, "proc_net", |r| {
        let Some(t) = r.i("t_us") else { return };
        let g = |k: &str| r.i(k).unwrap_or(0).max(0) as u64;
        d.proc_net.push(ProcNet {
            t,
            pid: r.i("pid").unwrap_or(0) as u32,
            name: r.s("name").to_string(),
            tx: g("udp_tx_b") + g("tcp_tx_b"),
            rx: g("udp_rx_b") + g("tcp_rx_b"),
        });
    })?;
    seen("proc_net", present, &mut files);

    each(dir, "marks", |r| {
        if let Some(t) = r.i("t_us") {
            d.marks.push(t);
        }
    })?;
    d.marks.sort_unstable();
    each(dir, "events", |r| {
        if let Some(t) = r.i("t_us") {
            d.events.push(Note { t, kind: r.s("kind").to_string(), detail: r.s("detail").to_string() });
        }
    })?;
    let present = each(dir, "wifi_driver", |r| {
        if let Some(t) = r.i("t_us") {
            let what = format!("{} {}", r.s("event"), r.s("oid")).trim().to_string();
            d.driver_cmds.push((t, what, r.i("pid").unwrap_or(0) as u32));
        }
    })?;
    seen("wifi_driver", present, &mut files);
    let present = each(dir, "driver_long", |r| {
        if let (Some(t), Some(us)) = (r.i("t_us"), r.f("dur_us")) {
            d.driver_long.push((t, r.s("kind") == "I", r.s("driver").to_string(), us));
        }
    })?;
    seen("driver_long", present, &mut files);
    let present = each(dir, "drivers_1s", |r| {
        if r.s("driver") == "_all" {
            if let Some(t) = r.i("t_us") {
                let m = r.f("dpc_max_us").unwrap_or(0.0).max(r.f("isr_max_us").unwrap_or(0.0));
                d.driver_1s.push((t, m));
            }
        }
    })?;
    seen("drivers_1s", present, &mut files);
    for name in ["etw_health", "etw_health_frames", "etw_health_wlan", "etw_health_ndis", "etw_health_kernel"] {
        let mut lost = 0u64;
        each(dir, name, |r| lost = lost.max(r.i("events_lost").unwrap_or(0) as u64))?;
        d.etw_lost += lost;
    }

    let windows: Vec<(i64, i64)> = {
        let mut v = Vec::new();
        let mut open = None;
        for e in d.events.iter().filter(|e| e.kind == "exp") {
            if e.detail.starts_with("start|") {
                open = Some(e.t);
            } else if let (true, Some(s)) = (e.detail.starts_with("end|"), open.take()) {
                v.push((s, e.t));
            }
        }
        v
    };
    if !windows.is_empty() {
        // Wi-Fi check: between test windows the adapter is restarted on purpose, so those
        // pings would show up as loss / spikes in the general statistics.
        for v in d.probes.values_mut() {
            v.retain(|p| windows.iter().any(|(s, e)| p.t >= *s && p.t < *e));
        }
    }
    for t in d.packets.last().map(|p| p.t).into_iter()
        .chain(d.frames.last().map(|f| f.0))
        .chain(d.cpu.last().map(|c| c.t))
        .chain(d.probes.values().filter_map(|v| v.last().map(|p| p.t)))
        .chain(d.events.last().map(|e| e.t))
    {
        max_t = max_t.max(t);
    }
    let meta_dur = d.meta["duration_s"].as_f64().map(|s| (s * 1e6) as i64).unwrap_or(0);
    d.duration_us = max_t.max(meta_dur);
    d.files = files;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_split() {
        assert_eq!(split_csv("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(split_csv("1,\"x,y\",z"), vec!["1", "x,y", "z"]);
        assert_eq!(split_csv("\"say \"\"hi\"\"\",2"), vec!["say \"hi\"", "2"]);
    }
}

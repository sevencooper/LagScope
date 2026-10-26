//! Anomaly detection, cross-signal correlation and per-mark verdicts.

use std::collections::BTreeMap;

use serde::Serialize;

use super::load::{Data, Probe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Kind {
    InGap,
    OutGap,
    FrameHitch,
    Stall,
    ProbeSpike,
    ProbeLoss,
    RttPlateau,
    WifiScan,
    WifiRoam,
    WifiDisconnect,
    WifiQualityDrop,
    WifiRetries,
    TrafficBurst,
    DpcSpike,
    GpuLimit,
    Paging,
    GameBurst,
    /// One driver's DPC or interrupt handler ran >= 1 ms.
    DriverLatency,
    /// A single disk I/O took >= 100 ms.
    DiskSlow,
    /// The game read a lot from disk through page faults in one second (asset streaming
    /// or memory paging).
    GamePaging,
    /// The link froze in the upload direction: pings through the router stalled while the
    /// game's incoming stream kept arriving, so the PC's own packets were held back.
    UploadStall,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::InGap => "game inbound gap",
            Kind::OutGap => "game outbound gap",
            Kind::FrameHitch => "frame hitch",
            Kind::Stall => "system stall",
            Kind::ProbeSpike => "ping spike",
            Kind::ProbeLoss => "ping loss",
            Kind::RttPlateau => "latency plateau",
            Kind::WifiScan => "Wi-Fi scan",
            Kind::WifiRoam => "Wi-Fi roam",
            Kind::WifiDisconnect => "Wi-Fi disconnect",
            Kind::WifiQualityDrop => "Wi-Fi quality drop",
            Kind::WifiRetries => "Wi-Fi retries",
            Kind::TrafficBurst => "other traffic burst",
            Kind::DpcSpike => "DPC/interrupt spike",
            Kind::GpuLimit => "GPU clock limit",
            Kind::Paging => "memory pressure (paging)",
            Kind::GameBurst => "game region-ping burst",
            Kind::DriverLatency => "driver DPC/ISR",
            Kind::DiskSlow => "slow disk I/O",
            Kind::GamePaging => "game reading from disk",
            Kind::UploadStall => "upload freeze",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Anomaly {
    pub t: i64,
    pub dur: i64,
    pub kind: Kind,
    /// Probe target, process name, etc.
    pub target: String,
    /// Main magnitude (ms for gaps/spikes, % for CPU, ...).
    pub value: f64,
    pub detail: String,
    /// False when it happened outside match play (menus, loading screens, spectating).
    pub in_match: bool,
}

impl Anomaly {
    fn new(t: i64, dur: i64, kind: Kind, target: &str, value: f64, detail: String) -> Self {
        Anomaly { t, dur, kind, target: target.into(), value, detail, in_match: true }
    }
    /// Game-side symptoms (gaps, hitches) are only meaningful during match play.
    fn counts(&self) -> bool {
        self.in_match || !matches!(self.kind, Kind::InGap | Kind::OutGap | Kind::FrameHitch)
    }
    pub fn end(&self) -> i64 {
        self.t + self.dur
    }
    fn overlaps(&self, a: i64, b: i64) -> bool {
        self.t <= b && self.end() >= a
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeStats {
    pub target: String,
    pub ip: String,
    pub sent: usize,
    pub lost: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub spikes: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StreamStats {
    pub endpoints: Vec<String>,
    pub in_packets: usize,
    pub out_packets: usize,
    pub active_s: f64,
    pub in_rate_pps: f64,
    pub out_rate_pps: f64,
    pub in_median_ms: f64,
    pub in_p99_ms: f64,
    pub out_median_ms: f64,
    pub gaps_100ms: usize,
    pub gaps_250ms: usize,
    pub burst_gaps: usize,
    pub jitter_ms: f64,
    /// Seconds of match play (high client send rate); 0 if phases could not be told apart.
    pub match_s: f64,
}

/// One of the game's own UDP latency beacons (region pings, port 7600 in THE FINALS).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Beacon {
    pub endpoint: String,
    pub sent: usize,
    pub replies: usize,
    pub p50_ms: f64,
    pub max_ms: f64,
    /// (send time us, rtt ms) per reply.
    #[serde(skip)]
    pub samples: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FrameStats {
    pub frames: usize,
    pub avg_fps: f64,
    pub median_ms: f64,
    pub p99_ms: f64,
    pub low1_fps: f64,
    pub hitches_50ms: usize,
    pub hitches_100ms: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub t: i64,
    pub n: usize,
    pub culprit: String,
    pub category: &'static str,
    pub confidence: &'static str,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriverCmdCorr {
    pub what: String,
    pub process: String,
    /// Share of router-ping freezes preceded (within 1.5 s) by this command.
    pub hit: f64,
    /// Share expected by chance from how often the command occurs anyway.
    pub chance: f64,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Periodicity {
    pub what: String,
    pub period_s: f64,
    pub score: f64,
    pub events: usize,
    /// Typical size of the events on the beat (ms), where the series has one.
    pub typical_ms: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct Analysis {
    pub stream: StreamStats,
    /// Match-play intervals (us), from the client's send rate.
    pub phases: Vec<(i64, i64)>,
    /// Intervals around times the player switched away from the game (alt-tab etc.), when
    /// its frames are not judged.
    pub away: Vec<(i64, i64)>,
    /// How many times the player switched away.
    pub away_switches: usize,
    /// Router freezes that start at the same point within a second: (that many, of all).
    pub second_timer: Option<(usize, usize)>,
    /// The game's own region pings, nearest first.
    pub beacons: Vec<Beacon>,
    /// Wi-Fi driver retry counters were implausible and ignored.
    pub wifi_counters_bogus: bool,
    pub frames: FrameStats,
    pub probes: Vec<ProbeStats>,
    pub anomalies: Vec<Anomaly>,
    pub verdicts: Vec<Verdict>,
    /// Of game inbound gaps >= 100 ms: fraction coinciding with each other signal.
    pub gap_correlation: BTreeMap<String, f64>,
    pub gateway_spike_correlation: BTreeMap<String, f64>,
    pub periodicity: Vec<Periodicity>,
    /// Of in-match frame hitches >= 50 ms: fraction coinciding with each local cause.
    pub hitch_correlation: BTreeMap<String, f64>,
    /// Results of a Wi-Fi check session (test windows), if this was one.
    pub wifi_check: Option<super::experiments::Outcome>,
    /// Wi-Fi driver commands that repeatedly came right before router-ping freezes.
    pub driver_cmds: Vec<DriverCmdCorr>,
    /// Other programs' average and peak share of a CPU core while playing.
    pub background_cpu: Vec<(String, f64, f64)>,
    /// Programs busy during frame hitches: (name, hitches).
    pub hitch_procs: Vec<(String, usize)>,
    pub findings: Vec<String>,
    pub recommendations: Vec<String>,
    pub headline: String,
    /// One line per area (network, frames, CPU, ...) for the scorecard.
    pub health: Vec<Health>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub area: String,
    /// "ok", "warn" or "na" (not measured).
    pub status: String,
    pub text: String,
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// Endpoints that carry the real-time game stream (most inbound UDP packets).
pub fn server_endpoints(d: &Data) -> Vec<u32> {
    let ignore = crate::profile::of_session(&d.meta).ignore_ports;
    let ignored = |ep: &str| ep.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).is_some_and(|p| ignore.contains(&p));
    let mut counts = vec![0usize; d.endpoints.len()];
    for p in d.packets.iter().filter(|p| p.udp && !p.send) {
        if !ignored(&d.endpoints[p.ep as usize]) {
            counts[p.ep as usize] += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(0);
    let min = if max >= 2500 { (max / 5).max(500) } else { 100 };
    (0..counts.len() as u32).filter(|&i| counts[i as usize] >= min && counts[i as usize] * 5 >= max).collect()
}

const PAUSE_US: i64 = 5_000_000;

/// Correlation key for spikes / loss on the first hop (the router).
pub const GW_KEY: &str = "router hop spike/loss";

/// Gaps in a packet stream. Returns (median interval us, anomalies).
fn stream_gaps(times: &[i64], kind: Kind, label: &str, min_gap: i64) -> (f64, Vec<Anomaly>, Vec<f64>) {
    let gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let normal = sorted(gaps.iter().filter(|&&g| g < 1_000_000).map(|&g| g as f64).collect());
    let median = percentile(&normal, 0.5).max(1.0);
    let mut out = Vec::new();
    for (i, &g) in gaps.iter().enumerate() {
        if g <= min_gap || g > PAUSE_US {
            continue;
        }
        // Only a live real-time stream can stall: in lobbies and loading screens some games
        // send just a few packets per second (>= ~10 pkt/s over the last 3 s required).
        // The same after the gap: a real stall ends with the stream resuming, not with a
        // short burst fading into a lobby trickle.
        let since = times[i] - 3_000_000;
        let until = times[i + 1] + 3_000_000;
        if i + 1 - times[..=i].partition_point(|&x| x < since) < 30 || times[i + 1..].partition_point(|&x| x < until) < 30 {
            continue;
        }
        // Judge against the recent send rate: the stream slows down a lot in menus and
        // loading screens, which must not count as gaps.
        let recent = sorted(gaps[i.saturating_sub(64)..i].iter().filter(|&&x| x < 1_000_000).map(|&x| x as f64).collect());
        let local = if recent.len() >= 16 { percentile(&recent, 0.5) } else { median };
        if (g as f64) <= (4.0 * local).max(min_gap as f64) {
            continue;
        }
        let end = times[i + 1];
        // How many packets arrive within 20 ms after the gap: a large burst means the
        // packets were delayed and then released together (queueing/retransmission),
        // few means they were lost (or never sent).
        let burst = times[i + 1..].iter().take_while(|&&t| t - end <= 20_000).count();
        let expected = g as f64 / local;
        let buffered = burst >= 3 && burst as f64 >= 0.3 * expected;
        let detail = if kind == Kind::InGap {
            format!(
                "{:.0} ms without packets from the server, then {burst} arrived within 20 ms ({})",
                g as f64 / 1000.0,
                if buffered { "delayed & bunched: queueing / retransmissions" } else { "missing: lost or not sent" }
            )
        } else {
            format!("game sent nothing for {:.0} ms", g as f64 / 1000.0)
        };
        let mut a = Anomaly::new(times[i], g, kind, label, g as f64 / 1000.0, detail);
        if buffered {
            a.target = format!("{label}|burst");
        }
        out.push(a);
    }
    (median, out, normal)
}

fn probe_analysis(target: &str, v: &[Probe], ips: &[(i64, String)], out: &mut Vec<Anomaly>) -> ProbeStats {
    let ok = sorted(v.iter().filter(|p| p.rtt >= 0).map(|p| p.rtt as f64 / 1000.0).collect());
    let mut st = ProbeStats {
        target: target.into(),
        ip: ips.iter().map(|(_, ip)| ip.as_str()).collect::<Vec<_>>().join(" → "),
        sent: v.len(),
        lost: v.iter().filter(|p| p.rtt < 0).count(),
        p50_ms: percentile(&ok, 0.5),
        p95_ms: percentile(&ok, 0.95),
        p99_ms: percentile(&ok, 0.99),
        max_ms: ok.last().copied().unwrap_or(0.0),
        spikes: 0,
    };
    if ok.len() < 20 {
        return st;
    }
    let thr = (st.p50_ms * 3.0).max(st.p50_ms + 30.0);
    // Spikes, merged when closer than 300 ms.
    let mut cur: Option<Anomaly> = None;
    let flush = |cur: &mut Option<Anomaly>, out: &mut Vec<Anomaly>, st: &mut ProbeStats| {
        if let Some(a) = cur.take() {
            st.spikes += (a.kind == Kind::ProbeSpike) as usize;
            out.push(a);
        }
    };
    for p in v {
        let (is_spike, kind, val) = if p.rtt < 0 {
            (true, Kind::ProbeLoss, 1.0)
        } else {
            let ms = p.rtt as f64 / 1000.0;
            (ms > thr, Kind::ProbeSpike, ms)
        };
        if !is_spike {
            if cur.as_ref().is_some_and(|a| p.t - a.end() > 300_000) {
                flush(&mut cur, out, &mut st);
            }
            continue;
        }
        match &mut cur {
            Some(a) if a.kind == kind && p.t - a.end() <= 300_000 => {
                a.dur = p.t - a.t;
                if kind == Kind::ProbeLoss {
                    a.value += 1.0;
                } else {
                    a.value = a.value.max(val);
                }
            }
            _ => {
                flush(&mut cur, out, &mut st);
                cur = Some(Anomaly::new(p.t, 0, kind, target, val, String::new()));
            }
        }
    }
    flush(&mut cur, out, &mut st);
    for a in out.iter_mut().filter(|a| a.target == target && a.detail.is_empty()) {
        a.detail = match a.kind {
            Kind::ProbeLoss => format!("{} probe(s) to {target} lost", a.value),
            _ => format!("{target} RTT up to {:.0} ms (normal {:.0} ms)", a.value, st.p50_ms),
        };
    }
    // Plateau: 2 s rolling median well above normal for >= 3 s ("latency stuck high").
    let okv: Vec<(i64, f64)> = v.iter().filter(|p| p.rtt >= 0).map(|p| (p.t, p.rtt as f64 / 1000.0)).collect();
    let w = 40usize.min(okv.len());
    let mut start: Option<i64> = None;
    let mut peak = 0.0f64;
    for i in w..okv.len() {
        let med = percentile(&sorted(okv[i - w..i].iter().map(|x| x.1).collect()), 0.5);
        let high = med > st.p50_ms + 25.0 && med > st.p50_ms * 1.8;
        match (high, start) {
            (true, None) => {
                start = Some(okv[i - w].0);
                peak = med;
            }
            (true, Some(_)) => peak = peak.max(med),
            (false, Some(s)) => {
                let e = okv[i - 1].0;
                if e - s >= 3_000_000 {
                    out.push(Anomaly::new(
                        s,
                        e - s,
                        Kind::RttPlateau,
                        target,
                        peak,
                        format!("{target} latency stuck at ~{peak:.0} ms (normal {:.0}) for {:.1} s", st.p50_ms, (e - s) as f64 / 1e6),
                    ));
                }
                start = None;
            }
            _ => {}
        }
    }
    st
}

/// Some drivers (seen: MediaTek MT7925) report retry counters that exceed the frames sent.
fn wifi_counters_bogus(d: &Data) -> bool {
    let rows: Vec<(f64, f64)> = d.wifi.iter().filter_map(|w| Some((w.d_tx?, w.d_retry?))).filter(|r| r.0 > 0.0 || r.1 > 0.0).collect();
    rows.len() >= 10 && rows.iter().filter(|(tx, retry)| retry > tx).count() * 5 > rows.len()
}

fn wifi_anomalies(d: &Data, bogus: bool, out: &mut Vec<Anomaly>) {
    for e in &d.wifi_events {
        let k = e.kind.to_ascii_lowercase();
        let kind = if k.contains("discard") {
            continue;
        } else if k.contains("scan") || k.contains("unique wlan networks") {
            Kind::WifiScan
        } else if k.contains("roam") {
            Kind::WifiRoam
        } else if k.contains("disconnect") || k.contains("disassociat") {
            Kind::WifiDisconnect
        } else if k.contains("link_degraded") {
            Kind::WifiQualityDrop
        } else {
            continue;
        };
        out.push(Anomaly::new(e.t, 0, kind, "wifi", 0.0, format!("{} {}", e.kind, e.detail).trim().to_string()));
    }
    // Merge scan events closer than 3 s (a scan produces several notifications).
    let mut last_scan: Option<i64> = None;
    out.retain(|a| {
        if a.kind != Kind::WifiScan {
            return true;
        }
        let keep = last_scan.is_none_or(|l| a.t - l > 3_000_000);
        last_scan = Some(a.t);
        keep
    });
    let mut last_bssid = String::new();
    for w in d.wifi.iter().filter(|w| w.connected) {
        if !last_bssid.is_empty() && w.bssid != last_bssid {
            out.push(Anomaly::new(w.t, 0, Kind::WifiRoam, "wifi", 0.0, "BSSID changed (roamed to another AP or band)".into()));
        }
        last_bssid = w.bssid.clone();
        if let (Some(tx), Some(retry)) = (w.d_tx.filter(|_| !bogus), w.d_retry) {
            if tx >= 20.0 && retry / tx > 0.3 && retry <= tx {
                out.push(Anomaly::new(w.t, 250_000, Kind::WifiRetries, "wifi", retry / tx * 100.0, format!("{:.0}% of frames retried", retry / tx * 100.0)));
            }
        }
    }
    let q = &d.link_quality;
    let mut j = 0;
    let mut last_drop: Option<i64> = None;
    for i in 0..q.len() {
        while q[i].0 - q[j].0 > 3_000_000 {
            j += 1;
        }
        let maxprev = q[j..=i].iter().map(|x| x.1).fold(0.0, f64::max);
        if maxprev - q[i].1 >= 15.0 && last_drop.is_none_or(|l| q[i].0 - l > 5_000_000) {
            last_drop = Some(q[i].0);
            out.push(Anomaly::new(q[i].0, 0, Kind::WifiQualityDrop, "wifi", maxprev - q[i].1, format!("link quality fell {maxprev:.0} → {:.0}", q[i].1)));
        }
    }
}

/// Driver DPC/ISR runs of 1 ms or more (LatencyMon's "problem" threshold is about 1-2 ms;
/// shorter runs are normal).
pub const DRIVER_US: f64 = 1000.0;

/// Graphics drivers routinely run 1-2 ms DPCs under gaming load without visible effect;
/// they only count from 2 ms.
pub fn is_gpu_driver(name: &str) -> bool {
    const GPU: &[&str] = &["nvlddmkm.sys", "amdkmdag.sys", "atikmdag.sys", "atikmpag.sys", "igdkmd64.sys", "igdkmdn64.sys", "igdkmdnd64.sys", "dxgkrnl.sys", "dxgmms2.sys", "dxgmms1.sys"];
    GPU.iter().any(|g| name.eq_ignore_ascii_case(g))
}

pub fn driver_threshold_us(name: &str) -> f64 {
    if is_gpu_driver(name) { 2.0 * DRIVER_US } else { DRIVER_US }
}

fn driver_anomalies(d: &Data, out: &mut Vec<Anomaly>) {
    for (t, isr, drv, us) in d.driver_long.iter().filter(|x| x.3 >= driver_threshold_us(&x.2)) {
        out.push(Anomaly::new(
            *t,
            *us as i64,
            Kind::DriverLatency,
            drv,
            us / 1000.0,
            format!("{drv} {} ran {:.2} ms", if *isr { "interrupt handler" } else { "DPC" }, us / 1000.0),
        ));
    }
}

/// The game page-faulting this much data in from disk within one second counts as a
/// streaming/paging burst.
const GAME_FAULT_MB: f64 = 8.0;
const DISK_SLOW_MS: f64 = 100.0;

fn disk_anomalies(d: &Data, out: &mut Vec<Anomaly>) {
    // Several I/Os held up by the same disk stall finish together: one event, the slowest.
    let mut slow: Vec<Anomaly> = Vec::new();
    for (t, ms, kb) in d.disk_slow.iter().filter(|x| x.1 >= DISK_SLOW_MS) {
        match slow.last_mut() {
            Some(last) if *t - last.t <= 2_000_000 => {
                if *ms > last.value {
                    last.value = *ms;
                    last.dur = (*ms * 1000.0) as i64;
                }
                last.detail = format!("disk I/O held up for {:.0} ms", last.value);
            }
            _ => slow.push(Anomaly::new(*t, (*ms * 1000.0) as i64, Kind::DiskSlow, "disk", *ms, format!("one disk I/O ({kb:.0} KB) took {ms:.0} ms"))),
        }
    }
    out.extend(slow);
    for s in d.disk_1s.iter().filter(|s| s.game_fault_mb >= GAME_FAULT_MB) {
        out.push(Anomaly::new(
            s.t,
            1_000_000,
            Kind::GamePaging,
            "game",
            s.game_fault_mb,
            format!("the game read {:.0} MB from disk via page faults ({:.0} faults) in one second", s.game_fault_mb, s.game_faults),
        ));
    }
}

fn system_anomalies(d: &Data, out: &mut Vec<Anomaly>) {
    // DPC/interrupt time is a share of all CPUs, so judge it in cores, against this PC's
    // own normal: gaming keeps a few tenths of a core busy in drivers all the time (GPU,
    // network, USB, audio). A full extra core on top of that is a driver storm.
    let ncpu = d.meta["logical_cpus"].as_f64().filter(|&n| n > 0.0).unwrap_or(8.0);
    let cores = |c: &super::load::CpuRow| (c.dpc.unwrap_or(0.0) + c.intr.unwrap_or(0.0)) * ncpu / 100.0;
    let dpc_limit = percentile(&sorted(d.cpu.iter().map(cores).collect()), 0.5) + 1.0;
    for &(t, late) in &d.stalls {
        if late >= 8_000 {
            out.push(Anomaly::new(t, late, Kind::Stall, "system", late as f64 / 1000.0, format!("timer wake-up {:.1} ms late (whole-system stall)", late as f64 / 1000.0)));
        }
    }
    for c in &d.cpu {
        let dpc = c.dpc.unwrap_or(0.0) + c.intr.unwrap_or(0.0);
        if cores(c) >= dpc_limit {
            out.push(Anomaly::new(c.t - 250_000, 250_000, Kind::DpcSpike, "cpu", dpc, format!("DPC+interrupt time {:.1} CPU cores (normal {:.1})", cores(c), dpc_limit - 1.0)));
        }
        // Page-ins also count memory-mapped file reads (asset streaming), so they only mean
        // memory pressure when free RAM is actually low.
        if c.pages_in.unwrap_or(0.0) >= 1000.0 && c.avail_mb.is_some_and(|m| m < 1500.0) {
            out.push(Anomaly::new(c.t - 250_000, 250_000, Kind::Paging, "memory", c.pages_in.unwrap(), format!("{:.0} pages/s read from disk with only {:.0} MB RAM free", c.pages_in.unwrap(), c.avail_mb.unwrap_or(0.0))));
        }
    }
    let mut prev = String::new();
    for g in &d.gpu {
        let bad: Vec<&str> = g
            .reasons
            .split('|')
            .filter(|r| matches!(*r, "hw_slowdown" | "hw_thermal" | "sw_thermal" | "hw_power_brake"))
            .collect();
        let s = bad.join("|");
        if !s.is_empty() && s != prev {
            out.push(Anomaly::new(g.t, 250_000, Kind::GpuLimit, "gpu", g.temp.unwrap_or(0.0), format!("GPU clocks limited: {s} ({}°C)", g.temp.unwrap_or(0.0))));
        }
        prev = s;
    }
    // Non-game traffic bursts (possible bufferbloat on the shared link).
    let game: Vec<u32> = d.game_pids();
    let mut per_sec: BTreeMap<i64, (u64, String, u64)> = BTreeMap::new();
    for p in &d.proc_net {
        if game.contains(&p.pid) || p.pid == 0 {
            continue;
        }
        let e = per_sec.entry(p.t).or_insert((0, String::new(), 0));
        let b = p.tx + p.rx;
        e.0 += b;
        if b > e.2 {
            e.1 = p.name.clone();
            e.2 = b;
        }
    }
    let med = percentile(&sorted(per_sec.values().map(|v| v.0 as f64).collect()), 0.5);
    let thr = (med * 5.0).max(3e6 / 8.0);
    for (t, (b, name, _)) in per_sec {
        if b as f64 > thr {
            let mbit = b as f64 * 8.0 / 1e6;
            out.push(Anomaly::new(t, 1_000_000, Kind::TrafficBurst, &name, mbit, format!("{mbit:.1} Mbit/s of other traffic (top: {name})")));
        }
    }
}

/// Frames that are judged: in match play, not in its first seconds (loading into the map
/// hitches in every game) and not while the player was switched away.
fn frame_judged(phases: &[(i64, i64)], away: &[(i64, i64)], t: i64) -> bool {
    in_phases(phases, t) && !phases.iter().any(|&(a, _)| t >= a && t < a + 5_000_000) && !in_any(away, t)
}

fn frame_analysis(d: &Data, phases: &[(i64, i64)], away: &[(i64, i64)], out: &mut Vec<Anomaly>) -> FrameStats {
    let counted = |t: i64| frame_judged(phases, away, t);
    let times: Vec<i64> = d.frames.iter().map(|f| f.0).collect();
    let iv: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let normal = sorted(
        iv.iter()
            .zip(&times)
            .filter(|&(&g, &t)| g < 2_000_000 && counted(t))
            .map(|(&g, _)| g as f64 / 1000.0)
            .collect(),
    );
    let median = percentile(&normal, 0.5);
    let mut st = FrameStats {
        frames: times.len(),
        median_ms: median,
        p99_ms: percentile(&normal, 0.99),
        avg_fps: if normal.is_empty() { 0.0 } else { 1000.0 * normal.len() as f64 / normal.iter().sum::<f64>() },
        low1_fps: {
            let p = percentile(&normal, 0.99);
            if p > 0.0 { 1000.0 / p } else { 0.0 }
        },
        ..Default::default()
    };
    if normal.len() < 50 {
        return st;
    }
    let thr = (median * 3.0).max(40.0);
    for (i, &g) in iv.iter().enumerate() {
        let ms = g as f64 / 1000.0;
        if ms > thr && g < 2_000_000 {
            if counted(times[i]) {
                st.hitches_50ms += (ms >= 50.0) as usize;
                st.hitches_100ms += (ms >= 100.0) as usize;
            }
            let present = d.frames[i].1 as f64 / 1000.0;
            out.push(Anomaly::new(
                times[i],
                g,
                Kind::FrameHitch,
                "frames",
                ms,
                format!("{ms:.0} ms frame (normal {median:.1} ms){}", if present > ms * 0.5 { ", blocked inside Present (GPU/driver)" } else { "" }),
            ));
        }
    }
    st
}

/// True if `t` is inside match play (or if no phases could be determined).
fn in_phases(phases: &[(i64, i64)], t: i64) -> bool {
    phases.is_empty() || phases.iter().any(|&(a, b)| t >= a && t <= b)
}

fn in_any(iv: &[(i64, i64)], t: i64) -> bool {
    iv.iter().any(|&(a, b)| t >= a && t <= b)
}

/// While the game is not the foreground window (alt-tab, a click on another screen), plus
/// a moment before the switch and 2 s after coming back: switching itself makes the game
/// hitch, and in the background it may run at a different frame rate. Only once the game
/// has had focus (before that, nothing of it is recorded anyway).
pub fn away_intervals(focus: &[(i64, bool)], end: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut seen = false;
    let mut left: Option<i64> = None;
    for &(t, game) in focus {
        match (game, left) {
            (true, Some(l)) => {
                out.push((l - 500_000, t + 2_000_000));
                left = None;
            }
            (false, None) if seen => left = Some(t),
            _ => {}
        }
        seen |= game;
    }
    if let Some(l) = left {
        out.push((l - 500_000, end));
    }
    // Quick switches back and forth overlap: merge them.
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in out {
        match merged.last_mut() {
            Some(m) if a <= m.1 => m.1 = m.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// How many times the player switched away from the game (after it first had focus).
pub fn away_switches(focus: &[(i64, bool)]) -> usize {
    let mut seen = false;
    let mut n = 0;
    let mut prev = None;
    for &(_, game) in focus {
        if seen && !game && prev == Some(true) {
            n += 1;
        }
        seen |= game;
        prev = Some(game);
    }
    n
}

/// Match play = seconds where the client sends at a high rate (input/state updates). In
/// menus, loading screens and while spectating the client sends far less.
pub fn match_phases(outbound: &[i64], inbound: &[i64]) -> Vec<(i64, i64)> {
    let count = |v: &[i64]| {
        let mut m: BTreeMap<i64, usize> = BTreeMap::new();
        for &t in v {
            *m.entry(t.div_euclid(1_000_000)).or_default() += 1;
        }
        m
    };
    let (per_sec, per_sec_in) = (count(outbound), count(inbound));
    let p90 = percentile(&sorted(per_sec.values().map(|&n| n as f64).collect()), 0.9);
    if p90 < 20.0 {
        return vec![];
    }
    let thr = (p90 * 0.5) as usize;
    // Some games (e.g. CS2) keep sending at full rate while loading, with the server
    // sending only a trickle: match play needs both directions busy.
    let p90_in = percentile(&sorted(per_sec_in.values().map(|&n| n as f64).collect()), 0.9);
    let thr_in = (p90_in * 0.3) as usize;
    let active = |s: i64, n: usize| n >= thr && per_sec_in.get(&s).copied().unwrap_or(0) >= thr_in;
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (&s, _) in per_sec.iter().filter(|(s, n)| active(**s, **n)) {
        match out.last_mut() {
            Some(last) if s * 1_000_000 - last.1 <= 3_000_000 => last.1 = (s + 1) * 1_000_000,
            _ => out.push((s * 1_000_000, (s + 1) * 1_000_000)),
        }
    }
    out.retain(|&(a, b)| b - a >= 20_000_000);
    // Everything was "match": phases carry no information.
    if out.len() == 1 && per_sec.iter().filter(|(s, n)| !active(**s, **n)).count() < 5 {
        return vec![];
    }
    out
}

/// The game pings region beacons by itself (small UDP request/response pairs). Pairing
/// them gives a passive UDP latency measurement to the game's cloud regions.
pub fn beacons(d: &Data, servers: &[u32]) -> Vec<Beacon> {
    use std::collections::{HashMap, VecDeque};
    let mut small = vec![true; d.endpoints.len()];
    let mut sent = vec![0usize; d.endpoints.len()];
    let mut recv = vec![0usize; d.endpoints.len()];
    for p in d.packets.iter().filter(|p| p.udp) {
        let e = p.ep as usize;
        small[e] &= p.size <= 64;
        if p.send { sent[e] += 1 } else { recv[e] += 1 }
    }
    let cand: Vec<bool> = (0..d.endpoints.len())
        .map(|e| small[e] && sent[e] >= 5 && recv[e] * 2 >= sent[e] && recv[e] <= sent[e] * 2 && !servers.contains(&(e as u32)))
        .collect();
    let mut queue: HashMap<(u32, u16), VecDeque<i64>> = HashMap::new();
    let mut out: BTreeMap<u32, Beacon> = BTreeMap::new();
    for p in d.packets.iter().filter(|p| p.udp && cand[p.ep as usize]) {
        let q = queue.entry((p.ep, p.lport)).or_default();
        if p.send {
            q.push_back(p.t);
            continue;
        }
        while q.front().is_some_and(|&s| p.t - s > 1_000_000) {
            q.pop_front();
        }
        if let Some(s) = q.pop_front() {
            out.entry(p.ep).or_default().samples.push((s, (p.t - s) as f64 / 1000.0));
        }
    }
    let mut v: Vec<Beacon> = out
        .into_iter()
        .filter(|(_, b)| b.samples.len() >= 5)
        .map(|(e, mut b)| {
            let r = sorted(b.samples.iter().map(|x| x.1).collect());
            b.endpoint = d.endpoints[e as usize].clone();
            b.sent = sent[e as usize];
            b.replies = b.samples.len();
            b.p50_ms = percentile(&r, 0.5);
            b.max_ms = r.last().copied().unwrap_or(0.0);
            b
        })
        .collect();
    v.sort_by(|a, b| a.p50_ms.total_cmp(&b.p50_ms));
    v
}

/// Detects a dominant recurrence interval in event times.
pub fn periodicity(times: &[i64], end: i64) -> Option<(f64, f64, usize)> {
    if times.len() < 5 {
        return None;
    }
    let tol_of = |p: f64| (0.03 * p).max(0.75);
    // Share of events followed by another one about `p` seconds later.
    let score = |p: f64| -> Option<f64> {
        let pu = (p * 1e6) as i64;
        let tol = (tol_of(p) * 1e6) as i64;
        let eligible: Vec<i64> = times.iter().copied().filter(|&t| t + pu + tol <= end).collect();
        if eligible.len() < 4 {
            return None;
        }
        let hits = eligible
            .iter()
            .filter(|&&t| {
                let target = t + pu;
                let i = times.partition_point(|&x| x < target - tol);
                times.get(i).is_some_and(|&x| x <= target + tol)
            })
            .count();
        Some(hits as f64 / eligible.len() as f64)
    };
    let mut best: Vec<(f64, f64)> = Vec::new();
    let mut p = 3.0f64;
    // At least four full cycles in the recording, or a "period" is just two coincidences.
    let longest = (end as f64 / 1e6 / 4.0).min(180.0);
    while p <= longest {
        if let Some(s) = score(p) {
            best.push((p, s));
        }
        p += 0.25;
    }
    let top = best.iter().map(|b| b.1).fold(0.0, f64::max);
    if top < 0.5 {
        return None;
    }
    let rate = times.len() as f64 / (end as f64 / 1e6).max(1.0);
    // With many events, long periods match by chance (some event is always near).
    let chance_at = |p: f64| 1.0 - (-rate * 2.0 * tol_of(p)).exp();
    // Centre of the first plateau of top-scoring candidates (tolerance makes neighbours tie).
    let first = best.iter().position(|b| b.1 >= top - 1e-9)?;
    let last = first + best[first..].iter().take_while(|b| b.1 >= top - 1e-9).count() - 1;
    let mut p = (best[first].0 + best[last].0) / 2.0;
    // Prefer the fundamental over its multiples (an event every 10 s also matches 30 s and
    // 90 s): take the shortest p/k that scores nearly as well (missed events cost the fundamental
    // more) and clearly beats chance.
    for k in (2..=(p / 3.0) as usize).rev() {
        let q = p / k as f64;
        let chance = chance_at(q);
        if score(q).is_some_and(|s| s >= top * 0.75 && s - chance >= 0.5) {
            p = q;
            break;
        }
    }
    // The chosen period must beat chance clearly.
    if score(p).is_none_or(|sc| sc - chance_at(p) < 0.5) {
        return None;
    }
    // Refine on the actual spacing of events (the scan grid above is only 0.25 s fine).
    let tol = tol_of(p);
    let mut est: Vec<f64> = times
        .windows(2)
        .filter_map(|w| {
            let d = (w[1] - w[0]) as f64 / 1e6;
            let m = (d / p).round();
            (m >= 1.0 && m <= 3.0 && (d - m * p).abs() <= tol).then(|| d / m)
        })
        .collect();
    if est.len() >= 3 {
        est.sort_by(|a, b| a.total_cmp(b));
        p = est[est.len() / 2];
    }
    Some(((p * 100.0).round() / 100.0, top, times.len()))
}

/// A beat that may skip: a timer fires every p seconds but only some firings show up, so
/// events come p, 2p, 3p ... apart. Judged on the gaps between consecutive events (events
/// within 2 s merged: one freeze can cut a stream twice), against the chance that a random
/// gap lands near a whole multiple. Returns (period, share of gaps on the beat, events).
pub fn beat(times: &[i64]) -> Option<(f64, f64, usize)> {
    let mut sorted_t = times.to_vec();
    sorted_t.sort_unstable();
    let mut ev: Vec<f64> = Vec::new();
    for t in sorted_t {
        let s = t as f64 / 1e6;
        if ev.last().is_none_or(|&l| s - l > 2.0) {
            ev.push(s);
        }
    }
    if ev.len() < 8 {
        return None;
    }
    let gaps: Vec<f64> = ev.windows(2).map(|w| w[1] - w[0]).collect();
    let tol_of = |p: f64| (0.04 * p).max(0.4);
    // Whole multiples 1..=6 of p count; longer gaps count as off the beat.
    let on_beat = |p: f64, g: f64| {
        let k = (g / p).round();
        (1.0..=6.0).contains(&k) && (g - k * p).abs() <= tol_of(p)
    };
    let fit = |p: f64| {
        let score = gaps.iter().filter(|&&g| on_beat(p, g)).count() as f64 / gaps.len() as f64;
        (score, score - (2.0 * tol_of(p) / p).min(1.0))
    };
    let mut best: Option<(f64, f64, f64)> = None;
    let mut p = 2.0f64;
    while p <= 60.0 {
        let (score, margin) = fit(p);
        if best.is_none_or(|b| margin > b.2) {
            best = Some((p, score, margin));
        }
        p += 0.02;
    }
    let (mut p, mut score, margin) = best?;
    if score < 0.65 || margin < 0.45 {
        return None;
    }
    // A 10 s beat also fits 5 s and 3.33 s (every gap is a multiple of those too): take the
    // longest whole multiple that fits about as well.
    let base = p;
    for k in 2..=6 {
        let q = base * k as f64;
        let (s_q, _) = fit(q);
        if q <= 60.0 && s_q >= score * 0.9 {
            p = q;
            score = s_q;
        }
    }
    // Refine on the gaps themselves: total time over total periods.
    let (sum, k): (f64, f64) = gaps.iter().filter(|&&g| on_beat(p, g)).fold((0.0, 0.0), |(s, k), &g| (s + g, k + (g / p).round()));
    let period = if k > 0.0 { sum / k } else { p };
    Some(((period * 100.0).round() / 100.0, score, ev.len()))
}

/// Events that start at the same point within a second (+-75 ms), e.g. always at x.88 s: a
/// 1-second timer somewhere (router, adapter firmware, driver) sets them off. Returns (how
/// many share that point, total) when at least 60% do; by chance it would be about 15%.
pub fn second_timer(times: &[i64]) -> Option<(usize, usize)> {
    if times.len() < 8 {
        return None;
    }
    let mut bins = [0usize; 20];
    for &t in times {
        bins[(t.rem_euclid(1_000_000) / 50_000) as usize] += 1;
    }
    let best = (0..20).map(|i| bins[(i + 19) % 20] + bins[i] + bins[(i + 1) % 20]).max().unwrap_or(0);
    (best * 10 >= times.len() * 6).then_some((best, times.len()))
}

/// Driver commands (NDIS requests) that keep showing up just before router-ping freezes:
/// tells whether something on the PC asks the Wi-Fi driver to do something each time.
fn driver_cmd_correlation(d: &Data, an: &[Anomaly]) -> Vec<DriverCmdCorr> {
    let spikes: Vec<i64> = an.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike).map(|a| a.t).collect();
    if spikes.len() < 5 || d.driver_cmds.is_empty() {
        return vec![];
    }
    let mut by: BTreeMap<&str, (Vec<i64>, BTreeMap<u32, usize>)> = BTreeMap::new();
    for (t, what, pid) in &d.driver_cmds {
        let e = by.entry(what.as_str()).or_default();
        e.0.push(*t);
        *e.1.entry(*pid).or_default() += 1;
    }
    let names: BTreeMap<u32, &str> = d.proc_net.iter().map(|p| (p.pid, p.name.as_str())).collect();
    let dur = (d.duration_us as f64 / 1e6).max(1.0);
    let (before, after) = (1_500_000i64, 300_000i64);
    let mut out: Vec<DriverCmdCorr> = by
        .into_iter()
        .filter_map(|(what, (mut ts, pids))| {
            ts.sort_unstable();
            let hits = spikes
                .iter()
                .filter(|&&s| {
                    let i = ts.partition_point(|&x| x < s - before);
                    ts.get(i).is_some_and(|&x| x <= s + after)
                })
                .count();
            let hit = hits as f64 / spikes.len() as f64;
            let chance = 1.0 - (-(ts.len() as f64 / dur) * (before + after) as f64 / 1e6).exp();
            let pid = pids.into_iter().max_by_key(|p| p.1).map(|p| p.0).unwrap_or(0);
            let process = match pid {
                0 => "kernel".to_string(),
                4 => "System".to_string(),
                p => names.get(&p).map(|n| n.to_string()).unwrap_or(format!("pid {p}")),
            };
            (hit >= 0.6 && hit - chance >= 0.4).then(|| DriverCmdCorr { what: what.to_string(), process, hit, chance, count: ts.len() })
        })
        .collect();
    out.sort_by(|a, b| (b.hit - b.chance).total_cmp(&(a.hit - a.chance)));
    out.truncate(5);
    out
}

fn near(an: &[Anomaly], kinds: &[Kind], target: Option<&str>, a: i64, b: i64) -> Vec<Anomaly> {
    an.iter()
        .filter(|x| x.counts() && kinds.contains(&x.kind) && target.is_none_or(|t| x.target == t) && x.overlaps(a, b))
        .cloned()
        .collect()
}

fn corr(an: &[Anomaly], base: &[&Anomaly], kinds: &[Kind], target: Option<&str>, before: i64, after: i64) -> f64 {
    if base.is_empty() {
        return 0.0;
    }
    let n = base.iter().filter(|g| !near(an, kinds, target, g.t - before, g.end() + after).is_empty()).count();
    n as f64 / base.len() as f64
}

const PROBE: &[Kind] = &[Kind::ProbeSpike, Kind::ProbeLoss, Kind::RttPlateau];

fn verdict_for(an: &[Anomaly], t: i64, n: usize, wifi: bool, away: &[(i64, i64)]) -> Verdict {
    // People press the key a second or two after noticing: judge the preceding ~4 s first,
    // and only fall back to a wider window (with low confidence) if that is clean.
    let mut v = verdict_window(an, t, n, t - 4_000_000, t + 1_000_000, wifi);
    if v.category == "unknown" {
        let wide = verdict_window(an, t, n, t - 10_000_000, t + 2_000_000, wifi);
        if wide.category != "unknown" {
            v = Verdict { culprit: format!("Possibly (5-10 s before the key press): {}", wide.culprit), confidence: "low", ..wide };
        }
    }
    // Marked while switched away from the game: switching itself makes the game hitch, so
    // a local, unexplained or only loosely matched verdict says nothing. Network problems
    // right at the mark still count.
    if (matches!(v.category, "local" | "unknown") || v.confidence == "low") && in_any(away, t) {
        v.culprit = "You had switched away from the game (alt-tab or another window) at this moment; switching itself makes the game hitch, so this mark is not judged.".into();
        v.category = "away";
        v.confidence = "high";
    }
    v
}

fn verdict_window(an: &[Anomaly], t: i64, n: usize, a: i64, b: i64, is_wifi: bool) -> Verdict {
    let biggest = |kinds: &[Kind], target: Option<&str>| -> Option<Anomaly> {
        near(an, kinds, target, a, b).into_iter().max_by(|x, y| x.value.total_cmp(&y.value))
    };
    let in_gap = biggest(&[Kind::InGap], None);
    let out_gap = biggest(&[Kind::OutGap], None);
    let frame = biggest(&[Kind::FrameHitch], None);
    let stall = biggest(&[Kind::Stall], None);
    let gw = biggest(PROBE, Some("gateway"));
    let upload = biggest(&[Kind::UploadStall], None);
    let hop2 = biggest(PROBE, Some("hop2"));
    let inet = biggest(PROBE, Some("internet"));
    let server = biggest(PROBE, Some("server"));
    let wifi = near(an, &[Kind::WifiScan, Kind::WifiRoam, Kind::WifiDisconnect, Kind::WifiQualityDrop, Kind::WifiRetries], None, a, b);
    let traffic = biggest(&[Kind::TrafficBurst], None);
    let dpc = biggest(&[Kind::DpcSpike], None);
    let drv = biggest(&[Kind::DriverLatency], None);
    let disk = biggest(&[Kind::GamePaging, Kind::DiskSlow], None);
    let gpu = biggest(&[Kind::GpuLimit, Kind::Paging], None);

    let mut ev: Vec<String> = Vec::new();
    for x in [&in_gap, &out_gap, &frame, &stall, &upload, &gw, &hop2, &inet, &server, &traffic, &dpc, &drv, &disk, &gpu].into_iter().flatten() {
        ev.push(format!("{:+.1}s {}: {}", (x.t - t) as f64 / 1e6, x.kind.label(), x.detail));
    }
    for w in wifi.iter().take(4) {
        ev.push(format!("{:+.1}s {}: {}", (w.t - t) as f64 / 1e6, w.kind.label(), w.detail));
    }

    let gap_ms = in_gap.as_ref().map(|g| g.value).unwrap_or(0.0);
    let frame_ms = frame.as_ref().map(|f| f.value).unwrap_or(0.0);
    let overlaps = |x: &Option<Anomaly>, y: &Option<Anomaly>, slack: i64| match (x, y) {
        (Some(x), Some(y)) => x.overlaps(y.t - slack, y.end() + slack),
        _ => false,
    };
    let (culprit, category, confidence): (String, &'static str, &'static str) = if frame_ms >= 80.0 && gap_ms < 80.0 {
        let why = if let Some(x) = drv.as_ref().filter(|x| x.value >= 2.0) {
            format!("a driver held up a CPU core: {}", x.detail)
        } else if let Some(x) = disk.as_ref() {
            format!("the game waited for the disk: {}", x.detail)
        } else if stall.is_some() {
            "a whole-system stall (driver / DPC / firmware) froze the PC".to_string()
        } else if gpu.is_some() {
            "frame hitch with GPU limit / paging at the same time".to_string()
        } else {
            "the game itself hitched (shader compile, asset streaming, CPU spike)".to_string()
        };
        (format!("Local PC: {why}"), "local", if frame_ms >= 150.0 { "high" } else { "medium" })
    } else if gap_ms >= 80.0 || gw.is_some() || server.is_some() || inet.is_some() || hop2.is_some() {
        let local_side = out_gap.is_some() && overlaps(&out_gap, &in_gap, 300_000) && overlaps(&frame, &in_gap, 300_000);
        if local_side {
            ("Local PC: the game stopped sending and receiving at the same time as a frame hitch".into(), "local", "medium")
        } else if let Some(u) = upload.as_ref().filter(|_| in_gap.is_none()) {
            let link = if is_wifi { "Wi-Fi" } else { "Local network" };
            (
                format!("{link}: the PC's uploads froze for ~{:.0} ms while the game's incoming data kept flowing, so your actions reached the server late (this feels like rubber-banding or being pulled back)", u.value),
                if is_wifi { "wifi" } else { "lan" },
                "medium",
            )
        } else if gw.is_some() && (in_gap.is_none() || overlaps(&gw, &in_gap, 500_000)) {
            let extra = if !wifi.is_empty() { format!(" ({} nearby)", wifi[0].kind.label()) } else { String::new() };
            if is_wifi {
                (format!("Wi-Fi: the router hop itself spiked{extra}"), "wifi", if gap_ms >= 100.0 { "high" } else { "medium" })
            } else {
                ("Local network: the link to the router spiked".into(), "lan", if gap_ms >= 100.0 { "high" } else { "medium" })
            }
        } else if hop2.is_some() && (in_gap.is_none() || overlaps(&hop2, &in_gap, 500_000)) {
            ("ISP access line (modem / first ISP hop) spiked; the link to your router was clean".into(), "isp", "medium")
        } else if inet.is_some() && (in_gap.is_none() || overlaps(&inet, &in_gap, 500_000)) {
            ("Internet / ISP backbone: latency rose beyond your ISP's first hop".into(), "isp", "low")
        } else if server.is_some() && (in_gap.is_none() || overlaps(&server, &in_gap, 500_000)) {
            ("Route to the game server degraded (your own network looked clean)".into(), "route", "medium")
        } else if traffic.is_some() {
            ("Possibly local bandwidth contention: other traffic burst while game packets stalled".into(), "contention", "low")
        } else if !wifi.is_empty() {
            (format!("Probably Wi-Fi: game packets stalled around a {}", wifi[0].kind.label()), "wifi", "low")
        } else {
            ("Game server / game-specific route: game packets stalled while every ping stayed clean".into(), "server", if gap_ms >= 150.0 { "medium" } else { "low" })
        }
    } else if frame_ms >= 40.0 {
        ("Minor local frame hitch only; network looked clean".into(), "local", "low")
    } else {
        ("Nothing measurable around this mark: likely server-side (tick / hit registration) or a perceptual effect. Note which in-game icon was showing.".into(), "unknown", "low")
    };
    Verdict { t, n, culprit, category, confidence, evidence: ev }
}

pub fn analyze(d: &Data) -> Analysis {
    let mut an: Vec<Anomaly> = Vec::new();
    let mut res = Analysis::default();

    // Game stream.
    let profile = crate::profile::of_session(&d.meta);
    let min_gap = (profile.min_gap_ms * 1000.0) as i64;
    let servers = server_endpoints(d);
    let inbound: Vec<i64> = d.packets.iter().filter(|p| p.udp && !p.send && servers.contains(&p.ep)).map(|p| p.t).collect();
    let outbound: Vec<i64> = d.packets.iter().filter(|p| p.udp && p.send && servers.contains(&p.ep)).map(|p| p.t).collect();
    res.phases = if profile.match_detection == crate::profile::MatchDetection::SendRate { match_phases(&outbound, &inbound) } else { vec![] };
    let phases = res.phases.clone();
    if inbound.len() > 50 {
        let (med_in, mut gaps_in, normal_in) = stream_gaps(&inbound, Kind::InGap, "server", min_gap);
        let (med_out, mut gaps_out, _) = stream_gaps(&outbound, Kind::OutGap, "client", min_gap);
        for g in gaps_in.iter_mut().chain(gaps_out.iter_mut()) {
            g.in_match = in_phases(&phases, g.t);
        }
        let counted: Vec<&Anomaly> = gaps_in.iter().filter(|g| g.in_match).collect();
        let active: f64 = inbound.windows(2).map(|w| w[1] - w[0]).filter(|&g| g <= PAUSE_US).sum::<i64>() as f64 / 1e6;
        // Jitter: mean change between consecutive tick intervals (time order, match play).
        // Large updates arrive as several packets within a few ms: count them as one tick.
        let mut ticks: Vec<i64> = Vec::new();
        for &t in &inbound {
            if ticks.last().is_none_or(|&l| t - l >= (profile.tick_merge_ms * 1000.0) as i64) {
                ticks.push(t);
            }
        }
        let iv: Vec<(i64, f64)> = ticks.windows(2).map(|w| (w[1], (w[1] - w[0]) as f64)).filter(|x| x.1 < 1e6 && in_phases(&phases, x.0)).collect();
        let jit: Vec<f64> = iv.windows(2).map(|w| (w[1].1 - w[0].1).abs()).collect();
        res.stream = StreamStats {
            endpoints: servers.iter().map(|&i| d.endpoints[i as usize].clone()).collect(),
            in_packets: inbound.len(),
            out_packets: outbound.len(),
            active_s: active,
            in_rate_pps: inbound.len() as f64 / active.max(1.0),
            out_rate_pps: outbound.len() as f64 / active.max(1.0),
            in_median_ms: med_in / 1000.0,
            in_p99_ms: percentile(&normal_in, 0.99) / 1000.0,
            out_median_ms: med_out / 1000.0,
            gaps_100ms: counted.iter().filter(|g| g.value >= 100.0).count(),
            gaps_250ms: counted.iter().filter(|g| g.value >= 250.0).count(),
            burst_gaps: counted.iter().filter(|g| g.target.ends_with("|burst")).count(),
            jitter_ms: if jit.is_empty() { 0.0 } else { jit.iter().sum::<f64>() / jit.len() as f64 / 1000.0 },
            match_s: phases.iter().map(|(a, b)| (b - a) as f64 / 1e6).sum(),
        };
        an.extend(gaps_in);
        an.extend(gaps_out);
    }
    for a in an.iter_mut() {
        if let Some(s) = a.target.strip_suffix("|burst") {
            a.target = s.to_string();
        }
    }

    res.away = away_intervals(&d.focus, d.duration_us);
    res.away_switches = away_switches(&d.focus);
    res.frames = frame_analysis(d, &phases, &res.away, &mut an);
    for a in an.iter_mut().filter(|a| a.kind == Kind::FrameHitch) {
        a.in_match = frame_judged(&phases, &res.away, a.t);
    }
    res.beacons = beacons(d, &servers);
    // The game pings all regions at once (hundreds of packets within ~10 ms): mark those
    // bursts so they can be correlated with ping spikes on a weak uplink.
    let mut sends: Vec<i64> = res.beacons.iter().flat_map(|b| b.samples.iter().map(|x| x.0)).collect();
    sends.sort_unstable();
    let mut rounds: Vec<(i64, i64, usize)> = Vec::new();
    for t in sends {
        match rounds.last_mut() {
            Some(r) if t - r.1 <= 2_000_000 => {
                r.1 = t;
                r.2 += 1;
            }
            _ => rounds.push((t, t, 1)),
        }
    }
    for (a, b, n) in rounds.into_iter().filter(|r| r.2 >= 20) {
        an.push(Anomaly::new(a, b - a, Kind::GameBurst, "game", n as f64, format!("the game pinged its regions: {n} packets in {:.0} ms", (b - a) as f64 / 1000.0)));
    }
    for (target, v) in &d.probes {
        let ips = d.probe_ips.get(target).cloned().unwrap_or_default();
        res.probes.push(probe_analysis(target, v, &ips, &mut an));
    }
    // Upload freezes: the path through the router froze (router and farther pings stalled
    // together) while the game's incoming stream kept arriving.
    if inbound.len() > 50 {
        let ups: Vec<Anomaly> = an
            .iter()
            .filter(|g| g.target == "gateway" && g.kind == Kind::ProbeSpike && g.value >= 80.0)
            .filter(|g| an.iter().any(|o| o.kind == Kind::ProbeSpike && o.target != "gateway" && o.overlaps(g.t - 100_000, g.end() + 100_000)))
            .filter_map(|g| {
                let (a, b) = (g.t - 50_000, g.t + (g.value * 1000.0) as i64 + 100_000);
                let i = inbound.partition_point(|&x| x < a);
                let j = inbound.partition_point(|&x| x <= b);
                let inside = &inbound[i..j];
                let max_gap = inside.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(i64::MAX);
                let flowing = inside.len() >= 5 && max_gap < 60_000 && inside[0] - a < 60_000 && b - inside[inside.len() - 1] < 60_000;
                flowing.then(|| {
                    let mut u = Anomaly::new(g.t, (g.value * 1000.0) as i64, Kind::UploadStall, "gateway", g.value, format!("uploads froze for ~{:.0} ms (router and internet pings stalled, game data kept arriving)", g.value));
                    u.in_match = in_phases(&phases, g.t);
                    u
                })
            })
            .collect();
        an.extend(ups);
    }
    let order = ["gateway", "hop2", "internet", "server"];
    res.probes.sort_by_key(|p| order.iter().position(|o| *o == p.target).unwrap_or(9));
    res.wifi_counters_bogus = wifi_counters_bogus(d);
    // On a cable an idle Wi-Fi adapter still scans now and then: not a suspect.
    let wired = d.meta["route"]["is_wifi"].as_bool() == Some(false);
    if !wired {
        wifi_anomalies(d, res.wifi_counters_bogus, &mut an);
    }
    system_anomalies(d, &mut an);
    if !d.events.iter().any(|e| e.kind == "exp") {
        driver_anomalies(d, &mut an);
    }
    disk_anomalies(d, &mut an);
    // PC-side events count only during match play, like stalls and hitches of the game:
    // starting the game and loading keep drivers and the disk busy in every game.
    for a in an.iter_mut().filter(|a| matches!(a.kind, Kind::DriverLatency | Kind::Stall | Kind::DpcSpike | Kind::DiskSlow | Kind::GamePaging)) {
        a.in_match = in_phases(&phases, a.t);
    }
    an.sort_by_key(|a| (a.t, a.kind));

    // Correlations for noticeable game stalls.
    let gaps: Vec<&Anomaly> = an.iter().filter(|a| a.kind == Kind::InGap && a.in_match && a.value >= 100.0).collect();
    let mut c = BTreeMap::new();
    if !gaps.is_empty() {
        c.insert(GW_KEY.into(), corr(&an, &gaps, PROBE, Some("gateway"), 300_000, 300_000));
        c.insert("ISP hop2 spike/loss".into(), corr(&an, &gaps, PROBE, Some("hop2"), 300_000, 300_000));
        c.insert("internet (1.1.1.1) spike/loss".into(), corr(&an, &gaps, PROBE, Some("internet"), 300_000, 300_000));
        c.insert("server-path spike/loss".into(), corr(&an, &gaps, PROBE, Some("server"), 300_000, 300_000));
        c.insert("frame hitch".into(), corr(&an, &gaps, &[Kind::FrameHitch], None, 200_000, 200_000));
        c.insert("system stall".into(), corr(&an, &gaps, &[Kind::Stall], None, 200_000, 200_000));
        if !d.driver_1s.is_empty() {
            c.insert("driver DPC/ISR ≥1 ms".into(), corr(&an, &gaps, &[Kind::DriverLatency], None, 500_000, 200_000));
        }
        c.insert("Wi-Fi scan/roam/drop (±3 s)".into(), corr(&an, &gaps, &[Kind::WifiScan, Kind::WifiRoam, Kind::WifiDisconnect, Kind::WifiQualityDrop], None, 3_000_000, 1_000_000));
        c.insert("other traffic burst".into(), corr(&an, &gaps, &[Kind::TrafficBurst], None, 2_000_000, 1_000_000));
        if an.iter().any(|a| a.kind == Kind::GameBurst) {
            c.insert("game region-ping burst (≤1 s before)".into(), corr(&an, &gaps, &[Kind::GameBurst], None, 1_000_000, 200_000));
        }
    }
    res.gap_correlation = c;
    let gw_spikes: Vec<&Anomaly> = an.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike && a.value >= 30.0).collect();
    if !gw_spikes.is_empty() {
        let mut c = BTreeMap::new();
        c.insert("Wi-Fi scan/roam/drop (±3 s)".into(), corr(&an, &gw_spikes, &[Kind::WifiScan, Kind::WifiRoam, Kind::WifiDisconnect, Kind::WifiQualityDrop], None, 3_000_000, 1_000_000));
        c.insert("other traffic burst".into(), corr(&an, &gw_spikes, &[Kind::TrafficBurst], None, 2_000_000, 1_000_000));
        c.insert("system stall / DPC".into(), corr(&an, &gw_spikes, &[Kind::Stall, Kind::DpcSpike], None, 300_000, 300_000));
        if !d.driver_1s.is_empty() {
            c.insert("driver DPC/ISR ≥1 ms".into(), corr(&an, &gw_spikes, &[Kind::DriverLatency], None, 300_000, 300_000));
        }
        c.insert("game inbound gap".into(), corr(&an, &gw_spikes, &[Kind::InGap], None, 300_000, 300_000));
        if an.iter().any(|a| a.kind == Kind::GameBurst) {
            c.insert("game region-ping burst (≤1 s before)".into(), corr(&an, &gw_spikes, &[Kind::GameBurst], None, 1_000_000, 200_000));
        }
        res.gateway_spike_correlation = c;
    }

    // Periodicity.
    let end = d.duration_us;
    // Shorter pauses of the game's stream too (from 50 ms, or twice the usual spacing): a
    // timer that holds traffic for ~80 ms never makes a 100 ms stall but is still a beat.
    let pauses: Vec<(i64, f64)> = {
        let iv: Vec<f64> = inbound.windows(2).filter(|w| in_phases(&phases, w[0])).map(|w| (w[1] - w[0]) as f64).filter(|&g| g < 1e6).collect();
        let med = percentile(&sorted(iv), 0.5);
        let min = (2.0 * med).max(50_000.0) as i64;
        inbound.windows(2).filter(|w| w[1] - w[0] >= min && w[1] - w[0] < PAUSE_US && in_phases(&phases, w[0])).map(|w| (w[0], (w[1] - w[0]) as f64 / 1000.0)).collect()
    };
    let spikes = |target: &str| -> Vec<(i64, f64)> {
        let mut v: Vec<(i64, f64)> = an.iter().filter(|a| a.target == target && matches!(a.kind, Kind::ProbeSpike | Kind::ProbeLoss)).map(|a| (a.t, if a.kind == Kind::ProbeSpike { a.value } else { 0.0 })).collect();
        v.sort_by_key(|x| x.0);
        // One freeze = one event, even if it has both slow and lost pings.
        v.dedup_by(|b, a| b.0 - a.0 < 1_000_000);
        v
    };
    let typical = |v: &[(i64, f64)]| percentile(&sorted(v.iter().map(|x| x.1).filter(|&x| x > 0.0).collect()), 0.5);
    let sized: Vec<(&str, Vec<(i64, f64)>)> = vec![
        ("game stream pauses", pauses),
        ("ISP hop ping spikes", spikes("hop2")),
        ("server-path ping spikes", spikes("server")),
    ];
    for (what, v) in &sized {
        let times: Vec<i64> = v.iter().map(|x| x.0).collect();
        // Network timers often skip beats: the skip-tolerant search first (the strict one
        // otherwise settles on a long multiple of the real beat).
        if let Some((p, s, n)) = beat(&times).or_else(|| periodicity(&times, end)) {
            res.periodicity.push(Periodicity { what: what.to_string(), period_s: p, score: s, events: n, typical_ms: typical(v) });
        }
    }
    let series: Vec<(&str, Vec<i64>)> = vec![
        ("game inbound gaps ≥100 ms", gaps.iter().map(|g| g.t).collect()),
        // A freeze shows up as delayed pings or, with some drivers/routers, as lost ones.
        ("gateway ping spikes/loss", {
            let mut v: Vec<i64> = an
                .iter()
                .filter(|a| a.target == "gateway" && matches!(a.kind, Kind::ProbeSpike | Kind::ProbeLoss))
                .map(|a| a.t)
                .collect();
            v.sort_unstable();
            // One freeze = one event, even if it has both slow and lost pings.
            v.dedup_by(|b, a| *b - *a < 1_000_000);
            v
        }),
        ("internet ping spikes", spikes("internet").iter().map(|x| x.0).collect()),
        ("Wi-Fi scans", an.iter().filter(|a| a.kind == Kind::WifiScan).map(|a| a.t).collect()),
        ("frame hitches", an.iter().filter(|a| a.kind == Kind::FrameHitch && a.in_match).map(|a| a.t).collect()),
    ];
    for (what, times) in series {
        // Scans and hitches: only a strict beat; network series may skip beats.
        let skips = !matches!(what, "Wi-Fi scans" | "frame hitches");
        let found = if skips { beat(&times).or_else(|| periodicity(&times, end)) } else { periodicity(&times, end) };
        if let Some((p, s, n)) = found {
            let ms = match what {
                "game inbound gaps ≥100 ms" => percentile(&sorted(gaps.iter().map(|g| g.value).collect()), 0.5),
                "internet ping spikes" => typical(&spikes("internet")),
                "gateway ping spikes/loss" => typical(&spikes("gateway")),
                _ => 0.0,
            };
            res.periodicity.push(Periodicity { what: what.into(), period_s: p, score: s, events: n, typical_ms: ms });
        }
    }

    let hitches: Vec<&Anomaly> = an.iter().filter(|a| a.kind == Kind::FrameHitch && a.in_match && a.value >= 50.0).collect();
    if !hitches.is_empty() {
        let mut c = BTreeMap::new();
        if !d.disk_1s.is_empty() {
            c.insert("game reading from disk".into(), corr(&an, &hitches, &[Kind::GamePaging], None, 1_000_000, 200_000));
            c.insert("slow disk I/O".into(), corr(&an, &hitches, &[Kind::DiskSlow], None, 300_000, 100_000));
        }
        if !d.driver_1s.is_empty() {
            c.insert("driver DPC/ISR".into(), corr(&an, &hitches, &[Kind::DriverLatency], None, 300_000, 100_000));
        }
        c.insert("system stall".into(), corr(&an, &hitches, &[Kind::Stall], None, 300_000, 100_000));
        c.insert("GPU limit / memory pressure".into(), corr(&an, &hitches, &[Kind::GpuLimit, Kind::Paging], None, 1_000_000, 200_000));
        res.hitch_correlation = c;
    }
    let freezes: Vec<i64> = an.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike && a.value >= 80.0).map(|a| a.t).collect();
    res.second_timer = second_timer(&freezes);
    res.wifi_check = super::experiments::from_session(d);
    res.driver_cmds = driver_cmd_correlation(d, &an);
    res.verdicts = d.marks.iter().enumerate().map(|(i, &t)| verdict_for(&an, t, i + 1, !wired, &res.away)).collect();
    res.anomalies = an;
    super::findings::summarize(d, &mut res);
    res
}

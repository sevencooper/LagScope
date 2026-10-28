//! Latency under load ("bufferbloat" test), on request only. Pings the router and the
//! internet while idle, then while downloading and while uploading as fast as possible
//! (several HTTPS streams to Cloudflare's public speed-test service, via Windows' own
//! WinHTTP). If latency climbs a lot under load, a queue somewhere (Wi-Fi link, router,
//! modem / ISP) is oversized; whether the router ping climbs too tells which side.
//!
//! Judged like LagScope Pulse (lagscope-core): worth a look when the ping rises more than
//! 50 ms, with the shared wording, and graded for gaming, video calls and streaming.
//!
//! This is LagScope's only feature that sends real traffic to the internet, so it is
//! opt-in and capped (see `MAX_DOWN` / `MAX_UP`).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;
use windows_sys::Win32::Networking::WinHttp::*;

use super::probes::{Pinger, route_to};
use super::wide;

const HOST: &str = "speed.cloudflare.com";
const STREAMS: usize = 4;
const IDLE: Duration = Duration::from_secs(6);
/// Load phase length (LAGSCOPE_LOADTEST_SECS shortens it for automated tests).
fn loaded() -> Duration {
    Duration::from_secs(std::env::var("LAGSCOPE_LOADTEST_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(12).max(3))
}
/// Data caps per direction (a fast line would otherwise move a lot of data).
const MAX_DOWN: u64 = 1_500_000_000;
const MAX_UP: u64 = 400_000_000;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Phase {
    pub router_p50: f64,
    pub router_p95: f64,
    pub internet_p50: f64,
    pub internet_p95: f64,
    /// Jitter of the internet pings (lagscope-core's definition).
    pub internet_jitter: f64,
    pub lost: usize,
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LoadResult {
    /// The router it ran behind (its address), so recordings on the same network can use it.
    pub gateway: String,
    pub idle: Phase,
    pub download: Phase,
    pub upload: Phase,
    pub down_mbps: f64,
    pub up_mbps: f64,
    /// How much the internet ping rose while downloading / uploading (ms); None = no internet
    /// ping was answered in that phase (not measured, never "no rise").
    pub extra_down_ms: Option<f64>,
    pub extra_up_ms: Option<f64>,
    /// Worth a look: the ping rose more than `WARN_MS`.
    pub warn: bool,
    pub good_for: Vec<crate::analyze::goodfor::GoodFor>,
    pub summary: Vec<String>,
    pub recommendations: Vec<String>,
}

struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}
unsafe impl Send for Handle {}

fn session() -> Result<(Handle, Handle)> {
    unsafe {
        let s = Handle(WinHttpOpen(wide("LagScope").as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, std::ptr::null(), std::ptr::null(), 0));
        if s.0.is_null() {
            bail!("WinHttpOpen failed");
        }
        WinHttpSetTimeouts(s.0, 5000, 5000, 10000, 10000);
        let c = Handle(WinHttpConnect(s.0, wide(HOST).as_ptr(), 443, 0));
        if c.0.is_null() {
            bail!("cannot connect to {HOST}");
        }
        Ok((s, c))
    }
}

/// Downloads until `stop`, counting bytes. Each request asks for 25 MB.
fn download(stop: Arc<AtomicBool>, bytes: Arc<AtomicU64>) -> Result<()> {
    let (_s, c) = session()?;
    let mut buf = vec![0u8; 256 * 1024];
    while !stop.load(Ordering::Relaxed) {
        unsafe {
            let r = Handle(WinHttpOpenRequest(c.0, wide("GET").as_ptr(), wide("/__down?bytes=25000000").as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
            if r.0.is_null() || WinHttpSendRequest(r.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 || WinHttpReceiveResponse(r.0, std::ptr::null_mut()) == 0 {
                bail!("download request failed");
            }
            loop {
                let mut n = 0u32;
                if WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut n) == 0 || n == 0 {
                    break;
                }
                bytes.fetch_add(n as u64, Ordering::Relaxed);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Uploads until `stop` (10 MB request bodies).
fn upload(stop: Arc<AtomicBool>, bytes: Arc<AtomicU64>) -> Result<()> {
    let (_s, c) = session()?;
    let chunk = vec![0x5au8; 64 * 1024];
    const BODY: u32 = 10_000_000;
    let headers = wide("Content-Type: application/octet-stream\r\n");
    while !stop.load(Ordering::Relaxed) {
        unsafe {
            let r = Handle(WinHttpOpenRequest(c.0, wide("POST").as_ptr(), wide("/__up").as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
            if r.0.is_null() || WinHttpSendRequest(r.0, headers.as_ptr(), u32::MAX, std::ptr::null(), 0, BODY, 0) == 0 {
                bail!("upload request failed");
            }
            let mut sent = 0u32;
            while sent < BODY && !stop.load(Ordering::Relaxed) {
                let n = (BODY - sent).min(chunk.len() as u32);
                let mut w = 0u32;
                if WinHttpWriteData(r.0, chunk.as_ptr() as *const _, n, &mut w) == 0 {
                    bail!("upload write failed");
                }
                sent += w;
                bytes.fetch_add(w as u64, Ordering::Relaxed);
            }
            if sent >= BODY {
                WinHttpReceiveResponse(r.0, std::ptr::null_mut());
            }
        }
    }
    Ok(())
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn phase(samples: &[(Instant, &str, Option<f64>)], from: Instant, to: Instant) -> Phase {
    let pick = |target: &str| -> Vec<f64> { samples.iter().filter(|s| s.1 == target && s.0 >= from && s.0 < to).filter_map(|s| s.2).collect() };
    let (mut r, mut i) = (pick("router"), pick("internet"));
    let internet_jitter = lagscope_core::stream::jitter(&i);
    let all: Vec<&(Instant, &str, Option<f64>)> = samples.iter().filter(|s| s.0 >= from && s.0 < to).collect();
    Phase {
        router_p50: pct(&mut r, 0.5),
        router_p95: pct(&mut r, 0.95),
        internet_p50: pct(&mut i, 0.5),
        internet_p95: pct(&mut i, 0.95),
        internet_jitter,
        lost: all.iter().filter(|s| s.2.is_none()).count(),
        samples: all.len(),
    }
}

/// A rise above this under load is worth a look (as in LagScope Pulse).
pub const WARN_MS: f64 = 50.0;

/// Runs the whole test (about 40 s). `progress` gets short status lines.
pub fn run(progress: &dyn Fn(String)) -> Result<LoadResult> {
    let internet = Ipv4Addr::new(1, 1, 1, 1);
    let Some(gw) = route_to(internet).and_then(|r| r.gateway) else { bail!("no internet connection") };
    let samples = Arc::new(std::sync::Mutex::new(Vec::<(Instant, &'static str, Option<f64>)>::new()));
    let done = Arc::new(AtomicBool::new(false));
    let mut pingers = Vec::new();
    for (name, ip) in [("router", gw), ("internet", internet)] {
        let (s, d) = (samples.clone(), done.clone());
        pingers.push(std::thread::spawn(move || {
            let Some(p) = Pinger::new() else { return };
            while !d.load(Ordering::Relaxed) {
                let t = Instant::now();
                let r = p.ping(ip, 1000);
                s.lock().unwrap().push((t, name, r));
                let spent = t.elapsed();
                if spent < Duration::from_millis(100) {
                    std::thread::sleep(Duration::from_millis(100) - spent);
                }
            }
        }));
    }
    let t0 = Instant::now();
    progress("Measuring idle latency ...".into());
    std::thread::sleep(IDLE);
    let mut marks = vec![t0, Instant::now()];

    let mut mbps = [0.0f64; 2];
    let mut error = None;
    for (k, (label, cap)) in [("download", MAX_DOWN), ("upload", MAX_UP)].into_iter().enumerate() {
        progress(format!("Loading the connection: {label} ..."));
        let stop = Arc::new(AtomicBool::new(false));
        let bytes = Arc::new(AtomicU64::new(0));
        let workers: Vec<_> = (0..STREAMS)
            .map(|_| {
                let (s, b) = (stop.clone(), bytes.clone());
                std::thread::spawn(move || if k == 0 { download(s, b) } else { upload(s, b) })
            })
            .collect();
        let start = Instant::now();
        // Measure throughput after a 2 s ramp-up.
        let mut ramp: Option<(Instant, u64)> = None;
        while start.elapsed() < loaded() && bytes.load(Ordering::Relaxed) < cap {
            std::thread::sleep(Duration::from_millis(200));
            if ramp.is_none() && start.elapsed() >= Duration::from_secs(2) {
                ramp = Some((Instant::now(), bytes.load(Ordering::Relaxed)));
            }
            if workers.iter().all(|w| w.is_finished()) {
                break;
            }
        }
        let end = Instant::now();
        let total = bytes.load(Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        for w in workers {
            if let Ok(Err(e)) = w.join() {
                error.get_or_insert(format!("{label}: {e:#}"));
            }
        }
        if let Some((t, b)) = ramp {
            mbps[k] = (total - b) as f64 * 8.0 / 1e6 / end.duration_since(t).as_secs_f64().max(0.1);
        }
        // Latency window: from ramp-up to the end of the load.
        marks.push(start + Duration::from_secs(2));
        marks.push(end);
        progress("Letting the connection settle ...".into());
        std::thread::sleep(Duration::from_secs(3));
    }
    done.store(true, Ordering::Relaxed);
    for p in pingers {
        let _ = p.join();
    }
    if mbps[0] == 0.0 && mbps[1] == 0.0 {
        bail!("{}", error.unwrap_or_else(|| "could not reach the speed-test service".into()));
    }
    let s = samples.lock().unwrap();
    let mut res = LoadResult {
        gateway: gw.to_string(),
        idle: phase(&s, marks[0], marks[1]),
        download: phase(&s, marks[2], marks[3]),
        upload: phase(&s, marks[4], marks[5]),
        down_mbps: (mbps[0] * 10.0).round() / 10.0,
        up_mbps: (mbps[1] * 10.0).round() / 10.0,
        ..Default::default()
    };
    interpret(&mut res);
    Ok(res)
}

impl LoadResult {
    /// "+3 ms while downloading, +26 ms while uploading".
    pub fn rise_text(&self) -> String {
        let one = |x: Option<f64>, what: &str| match x {
            Some(ms) => format!("+{ms:.0} ms while {what}"),
            None => format!("no internet ping answered while {what}"),
        };
        format!("{}, {}", one(self.extra_down_ms, "downloading"), one(self.extra_up_ms, "uploading"))
    }
}

/// Pure part (unit-tested): judgement, grades and plain-language result. The sentences
/// follow lagscope-core's design/wording.md (`load`).
pub fn interpret(res: &mut LoadResult) {
    // A phase without a single answered internet ping says nothing about the rise.
    let inc = |p: &Phase| (p.internet_p50 > 0.0).then(|| (p.internet_p50 - res.idle.internet_p50).max(0.0));
    let rinc = |p: &Phase| (p.router_p50 - res.idle.router_p50).max(0.0);
    res.extra_down_ms = inc(&res.download);
    res.extra_up_ms = inc(&res.upload);
    let (dn, up) = (res.extra_down_ms.unwrap_or(0.0), res.extra_up_ms.unwrap_or(0.0));
    let worst = dn.max(up);
    res.warn = worst > WARN_MS;
    res.good_for = crate::analyze::goodfor::for_load_test(&serde_json::to_value(&*res).unwrap_or_default());
    res.summary = Vec::new();
    if res.warn {
        res.summary.push(format!(
            "Your ping jumps by about {worst:.0} ms while someone {}. When the line is full, packets wait in a queue, usually in the router or modem. Games feel that as lag whenever someone at home streams, downloads or backs up files. It is called bufferbloat, and it can often be fixed in the router.",
            if up > dn { "uploads" } else { "downloads" }
        ));
    } else if res.extra_down_ms.is_none() && res.extra_up_ms.is_none() {
        res.summary.push("Ping under load could not be measured: no internet ping was answered while the line was full. Run the test again.".into());
    } else {
        res.summary.push("No ping jumps under load: other people's downloads and uploads should not cause lag.".into());
    }
    res.summary.push(format!("Speed: {:.0} Mbit/s down, {:.0} Mbit/s up.", res.down_mbps, res.up_mbps));
    res.summary.push(format!(
        "Internet ping: {:.0} ms idle (jitter {:.1} ms), {} while downloading, {} while uploading. Router ping: {:.0} / {:.0} / {:.0} ms.",
        res.idle.internet_p50,
        res.idle.internet_jitter,
        answered(res.download.internet_p50),
        answered(res.upload.internet_p50),
        res.idle.router_p50,
        res.download.router_p50,
        res.upload.router_p50
    ));
    if !res.warn {
        return;
    }
    // Where does the queue build up? If the router ping rises about as much, the queue is
    // on the PC's own link (Wi-Fi airtime / adapter); otherwise in the router / modem / ISP.
    let rworst = if up > dn { rinc(&res.upload) } else { rinc(&res.download) };
    if rworst >= worst * 0.6 {
        res.summary.push("The router ping rises with it: the queue builds up on this PC's own link to the router (usually Wi-Fi).".into());
        res.recommendations.push("The delay builds up on the Wi-Fi link itself: move closer to the router, use 5 GHz, or use a cable; a heavy download on this PC will make games lag.".into());
    } else {
        res.summary.push("The router ping stays low: the queue is in the router, the modem or the provider's network.".into());
        res.recommendations.push("Look for \"Smart Queue\", \"SQM\", \"QoS\" or \"bufferbloat\" in your router's settings, and set it a little below your measured speed. If your router has none of these, a router with Smart Queue support can usually fix it. Until then, pause big downloads and uploads while you play.".into());
        if up > dn * 1.5 {
            res.recommendations.push("Uploads hurt most: pause cloud backups / photo sync / streaming uploads while playing.".into());
        }
    }
}

/// A phase's median ping, or "no answer" when none came back.
fn answered(p50: f64) -> String {
    if p50 > 0.0 { format!("{p50:.0} ms") } else { "no answer".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph(r: f64, i: f64) -> Phase {
        Phase { router_p50: r, internet_p50: i, ..Default::default() }
    }

    #[test]
    fn grades_and_location() {
        let mut r = LoadResult { idle: ph(3.0, 12.0), download: ph(4.0, 20.0), upload: ph(4.0, 250.0), down_mbps: 300.0, up_mbps: 20.0, ..Default::default() };
        interpret(&mut r);
        assert!(r.warn && r.summary[0].starts_with("Your ping jumps by about 238 ms while someone uploads"), "{:?}", r.summary);
        assert!(r.summary.iter().any(|s| s.contains("router, the modem or the provider")), "{:?}", r.summary);
        assert!(r.recommendations.iter().any(|s| s.contains("SQM")));
        assert_eq!(r.good_for.len(), 3);
        let mut w = LoadResult { idle: ph(3.0, 12.0), download: ph(150.0, 160.0), upload: ph(5.0, 14.0), ..Default::default() };
        interpret(&mut w);
        assert!(w.summary.iter().any(|s| s.contains("own link")), "{:?}", w.summary);
        let mut f = LoadResult { idle: ph(3.0, 12.0), download: ph(4.0, 40.0), upload: ph(4.0, 30.0), ..Default::default() };
        interpret(&mut f);
        assert!(!f.warn && f.recommendations.is_empty(), "{:?}", f.summary);
        // No internet ping answered while downloading: not measured, not "+0 ms".
        let mut n = LoadResult { idle: ph(3.0, 18.0), download: ph(70.0, 0.0), upload: ph(4.0, 44.0), down_mbps: 500.0, ..Default::default() };
        interpret(&mut n);
        assert_eq!((n.extra_down_ms, n.extra_up_ms), (None, Some(26.0)));
        assert!(n.rise_text().starts_with("no internet ping answered while downloading"), "{}", n.rise_text());
        assert!(n.summary.iter().any(|s| s.contains("no answer while downloading")), "{:?}", n.summary);
    }
}

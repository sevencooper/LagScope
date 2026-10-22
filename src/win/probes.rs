//! Continuous ICMP probing (IcmpSendEcho2, asynchronous, ~20 Hz per target) of every
//! segment of the path, so a spike can be localised:
//!   gateway  - the Wi-Fi/LAN hop (router)
//!   hop2     - first hop past the router (ISP edge)
//!   internet - 1.1.1.1 (stable anycast reference)
//!   server   - the game server endpoint detected from game traffic; if it does not answer
//!              ICMP, a TTL-limited echo toward it that expires at the last hop which
//!              answers reliably (routers often ignore echo but do send TTL-exceeded).
//! Up to 32 echo requests are in flight per target, so a stall does not stop sampling.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GetAdaptersAddresses, GetBestInterface, ICMP_ECHO_REPLY, IP_ADAPTER_ADDRESSES_LH,
    IP_OPTION_INFORMATION, IcmpCloseHandle, IcmpCreateFile, IcmpParseReplies, IcmpSendEcho2,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC, SOCKADDR_IN};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};

use super::{Running, from_wide, spawn};
use crate::app::{App, ProbeLive};
use crate::clock;
use crate::row;
use crate::session::Sink;

const SLOTS: usize = 32;
const TIMEOUT_MS: u32 = 1000;
const REPLY_BUF: usize = 512;
const IP_TTL_EXPIRED_TRANSIT: u32 = 11013;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RouteInfo {
    pub if_index: u32,
    pub adapter: String,
    pub description: String,
    pub if_type: u32,
    pub is_wifi: bool,
    pub gateway: Option<Ipv4Addr>,
    pub tx_link_mbps: u64,
    pub rx_link_mbps: u64,
    pub mtu: u32,
}

/// Finds the interface and gateway Windows uses to reach `dst`.
pub fn route_to(dst: Ipv4Addr) -> Option<RouteInfo> {
    unsafe {
        let mut idx = 0u32;
        if GetBestInterface(u32::from_ne_bytes(dst.octets()), &mut idx) != 0 {
            return None;
        }
        let mut size = 64 * 1024u32;
        let mut buf = vec![0u64; size as usize / 8];
        let mut r = GetAdaptersAddresses(AF_UNSPEC as u32, GAA_FLAG_INCLUDE_GATEWAYS, std::ptr::null(), buf.as_mut_ptr() as *mut _, &mut size);
        if r == 111 {
            // ERROR_BUFFER_OVERFLOW
            buf = vec![0u64; size as usize / 8 + 1];
            r = GetAdaptersAddresses(AF_UNSPEC as u32, GAA_FLAG_INCLUDE_GATEWAYS, std::ptr::null(), buf.as_mut_ptr() as *mut _, &mut size);
        }
        if r != 0 {
            return None;
        }
        let mut p = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !p.is_null() {
            let a = &*p;
            if a.Anonymous1.Anonymous.IfIndex == idx {
                let mut gw = None;
                let mut g = a.FirstGatewayAddress;
                while !g.is_null() {
                    let sa = (*g).Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        let sin = &*(sa as *const SOCKADDR_IN);
                        gw = Some(Ipv4Addr::from(sin.sin_addr.S_un.S_addr.to_ne_bytes()));
                        break;
                    }
                    g = (*g).Next;
                }
                return Some(RouteInfo {
                    if_index: idx,
                    adapter: from_wide(a.FriendlyName),
                    description: from_wide(a.Description),
                    if_type: a.IfType,
                    is_wifi: a.IfType == 71,
                    gateway: gw,
                    tx_link_mbps: a.TransmitLinkSpeed / 1_000_000,
                    rx_link_mbps: a.ReceiveLinkSpeed / 1_000_000,
                    mtu: a.Mtu,
                });
            }
            p = a.Next;
        }
        None
    }
}

struct Icmp(HANDLE);
unsafe impl Send for Icmp {}
impl Drop for Icmp {
    fn drop(&mut self) {
        unsafe { IcmpCloseHandle(self.0) };
    }
}

fn icmp() -> Option<Icmp> {
    let h = unsafe { IcmpCreateFile() };
    if h.is_null() || h as isize == -1 { None } else { Some(Icmp(h)) }
}

/// Simple blocking pinger (for one-off tests; the session prober is asynchronous).
pub struct Pinger(Icmp);

impl Pinger {
    pub fn new() -> Option<Self> {
        icmp().map(Pinger)
    }

    /// Round trip in ms, or None if lost / timed out.
    pub fn ping(&self, ip: Ipv4Addr, timeout_ms: u32) -> Option<f64> {
        let data = [0x4cu8; 32];
        let mut buf = vec![0u64; REPLY_BUF / 8];
        let t = std::time::Instant::now();
        let n = unsafe {
            IcmpSendEcho2(
                self.0.0,
                std::ptr::null_mut(),
                None,
                std::ptr::null(),
                u32::from_ne_bytes(ip.octets()),
                data.as_ptr() as *const _,
                data.len() as u16,
                std::ptr::null(),
                buf.as_mut_ptr() as *mut _,
                REPLY_BUF as u32,
                timeout_ms,
            )
        };
        let rep = unsafe { &*(buf.as_ptr() as *const ICMP_ECHO_REPLY) };
        (n > 0 && rep.Status == 0).then(|| t.elapsed().as_secs_f64() * 1000.0)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hop {
    pub ttl: u8,
    pub ip: Option<Ipv4Addr>,
    pub rtt_ms: Option<u32>,
}

/// Blocking ICMP traceroute (one probe per TTL, retried once on timeout; gives up after
/// 4 consecutive silent hops).
pub fn traceroute(dst: Ipv4Addr, max_ttl: u8) -> Vec<Hop> {
    let Some(h) = icmp() else { return vec![] };
    let data = [0x4cu8; 32];
    let mut hops = Vec::new();
    for ttl in 1..=max_ttl {
        let mut hop = Hop { ttl, ip: None, rtt_ms: None };
        let mut reached = false;
        for _try in 0..2 {
            let mut buf = vec![0u64; REPLY_BUF / 8];
            let opts = IP_OPTION_INFORMATION { Ttl: ttl, Tos: 0, Flags: 0, OptionsSize: 0, OptionsData: std::ptr::null_mut() };
            let n = unsafe {
                IcmpSendEcho2(
                    h.0,
                    std::ptr::null_mut(),
                    None,
                    std::ptr::null(),
                    u32::from_ne_bytes(dst.octets()),
                    data.as_ptr() as *const _,
                    data.len() as u16,
                    &opts,
                    buf.as_mut_ptr() as *mut _,
                    REPLY_BUF as u32,
                    800,
                )
            };
            if n > 0 {
                let rep = unsafe { &*(buf.as_ptr() as *const ICMP_ECHO_REPLY) };
                hop.ip = Some(Ipv4Addr::from(rep.Address.to_ne_bytes()));
                hop.rtt_ms = Some(rep.RoundTripTime);
                reached = rep.Status == 0;
                break;
            }
        }
        hops.push(hop);
        if reached {
            break;
        }
        // Cloud networks often hide their last hops: stop after 4 silent hops in a row
        // (each costs ~1.6 s, and a slow traceroute delays the server probing fallback).
        if hops.len() >= 6 && hops.iter().rev().take(4).all(|h| h.ip.is_none()) {
            break;
        }
    }
    hops
}

/// A probe target whose address may change at runtime (e.g. the game server).
pub struct Target {
    pub label: String,
    pub addr: Mutex<Option<Ipv4Addr>>,
    /// Some(n): send with TTL n and count "TTL expired" from hop n as the reply.
    pub ttl: Mutex<Option<u8>>,
    /// In TTL mode: the hop expected to answer (logged for unanswered probes too).
    pub via: Mutex<Option<Ipv4Addr>>,
    /// Replies / requests since the pathfinder last reset them.
    pub sent: AtomicU32,
    pub ok: AtomicU32,
}

impl Target {
    fn new(label: &str, addr: Option<Ipv4Addr>, ttl: Option<u8>) -> Arc<Target> {
        Arc::new(Target {
            label: label.into(),
            addr: Mutex::new(addr),
            ttl: Mutex::new(ttl),
            via: Mutex::new(None),
            sent: AtomicU32::new(0),
            ok: AtomicU32::new(0),
        })
    }

    fn set(&self, addr: Option<Ipv4Addr>, ttl: Option<u8>, via: Option<Ipv4Addr>) {
        *self.addr.lock().unwrap() = addr;
        *self.ttl.lock().unwrap() = ttl;
        *self.via.lock().unwrap() = via;
        self.sent.store(0, Ordering::Relaxed);
        self.ok.store(0, Ordering::Relaxed);
    }
}

struct Slot {
    event: HANDLE,
    buf: Vec<u64>,
    sent_raw: i64,
    ip: Ipv4Addr,
    ttl_mode: bool,
    busy: bool,
}

fn probe_loop(app: Arc<App>, target: Arc<Target>, sink: Arc<Sink>) {
    let Some(h) = icmp() else {
        app.session.event("probe", format!("{}: IcmpCreateFile failed", target.label));
        return;
    };
    let clock = app.session.clock;
    let hz = app.cfg.probe_hz.clamp(1, 100) as i64;
    let interval_raw = clock.freq() / hz;
    let data = [0x4cu8; 32];
    let mut slots: Vec<Slot> = (0..SLOTS)
        .map(|_| Slot {
            event: unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) },
            buf: vec![0u64; REPLY_BUF / 8],
            sent_raw: 0,
            ip: Ipv4Addr::UNSPECIFIED,
            ttl_mode: false,
            busy: false,
        })
        .collect();
    let mut recent: VecDeque<(i64, Option<f64>)> = VecDeque::new();
    let mut next_send = clock::raw_now();
    let mut last_live = 0i64;

    while !app.stopping() {
        let now = clock::raw_now();
        let addr = *target.addr.lock().unwrap();
        let ttl = *target.ttl.lock().unwrap();
        if now >= next_send {
            next_send += interval_raw;
            if next_send < now {
                next_send = now + interval_raw;
            }
            if let Some(ip) = addr {
                if let Some(s) = slots.iter_mut().find(|s| !s.busy) {
                    s.sent_raw = clock::raw_now();
                    s.ip = target.via.lock().unwrap().filter(|_| ttl.is_some()).unwrap_or(ip);
                    s.ttl_mode = ttl.is_some();
                    let opts = IP_OPTION_INFORMATION { Ttl: ttl.unwrap_or(128), Tos: 0, Flags: 0, OptionsSize: 0, OptionsData: std::ptr::null_mut() };
                    target.sent.fetch_add(1, Ordering::Relaxed);
                    let r = unsafe {
                        IcmpSendEcho2(
                            h.0,
                            s.event,
                            None,
                            std::ptr::null(),
                            u32::from_ne_bytes(ip.octets()),
                            data.as_ptr() as *const _,
                            data.len() as u16,
                            if ttl.is_some() { &opts } else { std::ptr::null() },
                            s.buf.as_mut_ptr() as *mut _,
                            REPLY_BUF as u32,
                            TIMEOUT_MS,
                        )
                    };
                    let err = unsafe { GetLastError() };
                    if r == 0 && err == 997 {
                        s.busy = true; // ERROR_IO_PENDING: completion signals the event
                    } else if r == 0 {
                        let t = clock.us_from_raw(s.sent_raw);
                        row!(sink, "{t},{},{ip},{err},-1,0", target.label);
                    }
                } else {
                    let t = clock.us_from_raw(now);
                    row!(sink, "{t},{},{},99999,-1,0", target.label, addr.unwrap()); // all slots busy
                }
            }
        }

        let busy: Vec<usize> = (0..SLOTS).filter(|&i| slots[i].busy).collect();
        // Round up and never wait 0 ms: rounding down made the thread spin through the last
        // fraction of a millisecond before every send (costly when the system timer is fine).
        let wait_ms = (((next_send - clock::raw_now()).max(0) * 1000 + clock.freq() - 1) / clock.freq()).clamp(1, 100) as u32;
        if busy.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
            continue;
        }
        let handles: Vec<HANDLE> = busy.iter().map(|&i| slots[i].event).collect();
        let w = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, wait_ms) };
        if w == WAIT_TIMEOUT || (w as usize) >= handles.len() {
            continue;
        }
        let recv_raw = clock::raw_now();
        let s = &mut slots[busy[w as usize]];
        s.busy = false;
        let n = unsafe { IcmpParseReplies(s.buf.as_mut_ptr() as *mut _, REPLY_BUF as u32) };
        let t = clock.us_from_raw(s.sent_raw);
        // Error replies (e.g. TTL expired) may come back with n == 0 and the status in
        // GetLastError, but the reply structure is still filled in.
        let rep = unsafe { &*(s.buf.as_ptr() as *const ICMP_ECHO_REPLY) };
        let status = if n > 0 { rep.Status } else { unsafe { GetLastError() } };
        let answered = status == 0 || (s.ttl_mode && status == IP_TTL_EXPIRED_TRANSIT);
        let (rtt_us, ttl, ip) = if answered {
            let from = Ipv4Addr::from(rep.Address.to_ne_bytes());
            (clock.raw_to_us_delta(recv_raw - s.sent_raw), rep.Options.Ttl, if from.is_unspecified() { s.ip } else { from })
        } else {
            (-1, 0, s.ip)
        };
        if answered {
            target.ok.fetch_add(1, Ordering::Relaxed);
        }
        row!(sink, "{t},{},{ip},{status},{rtt_us},{ttl}", target.label);

        let rtt = (rtt_us >= 0).then_some(rtt_us as f64 / 1000.0);
        if target.label == "gateway" {
            app.gw_samples.lock().unwrap().push((t, rtt));
        }
        recent.push_back((t, rtt));
        while recent.front().is_some_and(|(rt, _)| *rt < t - 10_000_000) {
            recent.pop_front();
        }
        if t - last_live > 500_000 {
            last_live = t;
            let ok: Vec<f64> = recent.iter().filter_map(|(_, r)| *r).collect();
            let pl = ProbeLive {
                sent_10s: recent.len() as u32,
                lost_10s: (recent.len() - ok.len()) as u32,
                max_ms_10s: ok.iter().copied().fold(0.0, f64::max),
                avg_ms_10s: if ok.is_empty() { 0.0 } else { ok.iter().sum::<f64>() / ok.len() as f64 },
            };
            app.live.lock().unwrap().probes.insert(target.label.clone(), pl);
        }
    }
    // Let outstanding requests complete before their buffers are freed.
    let busy: Vec<HANDLE> = slots.iter().filter(|s| s.busy).map(|s| s.event).collect();
    if !busy.is_empty() {
        unsafe { WaitForMultipleObjects(busy.len() as u32, busy.as_ptr(), 1, TIMEOUT_MS + 200) };
    }
    drop(h);
    for s in slots {
        unsafe { CloseHandle(s.event) };
    }
}

fn hops_csv(sink: &Sink, t: i64, dst: Ipv4Addr, hops: &[Hop]) {
    for h in hops {
        row!(
            sink,
            "{t},{dst},{},{},{}",
            h.ttl,
            h.ip.map(|i| i.to_string()).unwrap_or_default(),
            h.rtt_ms.map(|r| r.to_string()).unwrap_or_default()
        );
    }
}

pub fn start(app: Arc<App>) -> Running {
    let sink = app.session.sink("probes", "t_us,target,ip,status,rtt_us,ttl");
    let trace_sink = app.session.sink("traceroute", "t_us,dst,ttl,hop_ip,rtt_ms");
    let internet_ip = Ipv4Addr::new(1, 1, 1, 1);
    let route = route_to(internet_ip);
    if let Some(r) = &route {
        app.session.meta_set("route", serde_json::to_value(r).unwrap_or_default());
        app.session.event(
            "route",
            format!(
                "via '{}' ({}) {} gateway {} link {}/{} Mbit/s",
                r.adapter,
                r.description,
                if r.is_wifi { "[Wi-Fi]" } else { "[wired/other]" },
                r.gateway.map(|g| g.to_string()).unwrap_or("?".into()),
                r.tx_link_mbps,
                r.rx_link_mbps
            ),
        );
    } else {
        app.session.event("route", "could not determine route to 1.1.1.1 (offline?)");
    }

    let gateway = Target::new("gateway", route.as_ref().and_then(|r| r.gateway), None);
    let hop2 = Target::new("hop2", None, None);
    let internet = Target::new("internet", Some(internet_ip), None);
    let server = Target::new("server", None, None);
    let mut targets = vec![gateway.clone(), hop2.clone(), internet, server.clone()];
    for (i, extra) in app.cfg.extra_targets.iter().enumerate() {
        // "IP" or "IP@TTL" (probe the hop at that distance on the path to IP).
        let (ip, ttl) = match extra.split_once('@') {
            Some((ip, ttl)) => (ip.parse::<Ipv4Addr>().ok(), ttl.parse::<u8>().ok().filter(|t| *t > 0).map(Some)),
            None => (extra.parse::<Ipv4Addr>().ok(), Some(None)),
        };
        match (ip, ttl) {
            (Some(ip), Some(ttl)) => targets.push(Target::new(&format!("extra{}", i + 1), Some(ip), ttl)),
            _ => app.session.event("probe", format!("ignoring extra target '{extra}' (IPv4 or IPv4@TTL expected)")),
        }
    }

    let mut threads = Vec::new();
    for t in targets {
        let (a, s) = (app.clone(), sink.clone());
        threads.push(spawn(&format!("probe-{}", t.label), move || probe_loop(a, t, s)));
    }

    // Path discovery: traceroute to 1.1.1.1 for the ISP hop, then follow the game server.
    let a = app.clone();
    threads.push(spawn("pathfinder", move || {
        let t = a.session.clock.now_us();
        let hops = traceroute(internet_ip, 16);
        hops_csv(&trace_sink, t, internet_ip, &hops);
        let gw = *gateway.addr.lock().unwrap();
        let isp = hops.iter().filter_map(|h| h.ip).find(|ip| Some(*ip) != gw && *ip != internet_ip);
        if let Some(ip) = isp {
            *hop2.addr.lock().unwrap() = Some(ip);
            a.session.event("route", format!("ISP-side hop2 = {ip}"));
        }
        a.session.meta_set("traceroute_internet", serde_json::to_value(&hops).unwrap_or_default());

        let mut current: Option<SocketAddr> = None;
        while a.sleep_ms(1000) {
            let srv = *a.server.lock().unwrap();
            if srv != current {
                current = srv;
                if let Some(SocketAddr::V4(ep)) = srv {
                    server.set(Some(*ep.ip()), None, None);
                    let t = a.session.clock.now_us();
                    let hops = traceroute(*ep.ip(), 24);
                    hops_csv(&trace_sink, t, *ep.ip(), &hops);
                    let path: Vec<String> =
                        hops.iter().map(|h| h.ip.map(|i| i.to_string()).unwrap_or("*".into())).collect();
                    a.session.event("route", format!("path to server {}: {}", ep.ip(), path.join(" > ")));
                    a.session.meta_push("traceroute_server", serde_json::json!({ "dst": ep.ip().to_string(), "hops": hops }));
                    server_path_fallback(&a, &server, *ep.ip(), &hops, t);
                } else if let Some(SocketAddr::V6(ep)) = srv {
                    a.session.event("probe", format!("server {ep} is IPv6; ICMPv6 probing not implemented yet"));
                }
            }
        }
    }));
    Running::threads("probes", threads)
}

/// If the server never answers ICMP, find the farthest hop toward it that reliably answers a
/// TTL-limited echo ("TTL expired in transit") and keep probing that.
fn server_path_fallback(app: &Arc<App>, server: &Arc<Target>, ip: Ipv4Addr, hops: &[Hop], since_us: i64) {
    let reached = hops.last().is_some_and(|h| h.ip == Some(ip));
    if reached {
        return;
    }
    // Direct probing started before the traceroute; give it ~5 s in total.
    let waited_ms = (app.session.clock.now_us() - since_us) / 1000;
    if waited_ms < 5000 && !app.sleep_ms((5000 - waited_ms) as u64) {
        return;
    }
    if server.ok.load(Ordering::Relaxed) > 0 || *server.addr.lock().unwrap() != Some(ip) {
        return;
    }
    // Skip the first two hops: they are probed separately already.
    let candidates: Vec<&Hop> = hops.iter().rev().filter(|h| h.ip.is_some_and(|i| i != ip) && h.ttl > 2).take(4).collect();
    for h in candidates {
        server.set(Some(ip), Some(h.ttl), h.ip);
        if !app.sleep_ms(4000) {
            return;
        }
        let (sent, ok) = (server.sent.load(Ordering::Relaxed), server.ok.load(Ordering::Relaxed));
        if sent >= 20 && ok * 10 >= sent * 9 {
            app.session.event(
                "probe",
                format!(
                    "server {ip} does not answer ICMP; probing hop {} ({}) on its path instead via TTL-limited pings ({ok}/{sent} answered; since {:.1}s)",
                    h.ttl,
                    h.ip.unwrap(),
                    since_us as f64 / 1e6
                ),
            );
            return;
        }
    }
    server.set(None, None, None);
    app.session.event("probe", format!("server {ip} and the hops before it do not answer pings reliably; server-path probing stopped"));
}

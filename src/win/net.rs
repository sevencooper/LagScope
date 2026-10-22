//! Network activity from the Microsoft-Windows-Kernel-Network ETW provider.
//!
//! * Every TCP/UDP send/receive of the game process is written to `game_packets.csv`
//!   with a microsecond timestamp: inbound inter-arrival gaps and bursts are the
//!   primary network-stall signal.
//! * All processes are aggregated per second into `proc_net.csv` so traffic from other
//!   programs (downloads, cloud sync, streaming) can be correlated with spikes.
//! * The game server is identified as the remote UDP endpoint sending the game the most
//!   packets, and published to `App::server` for the ICMP prober.
//!
//! Verified on Windows 11 (event version 0): for send events the remote end is
//! daddr/dport, for receive events it is saddr/sport. Ports are big-endian. The PID in
//! the payload is the owning process (the header PID is often System for receives).

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::Running;
use super::etw::{self, Event, ProviderSpec, RealtimeSession, SchemaCache};
use crate::app::App;
use crate::row;
use crate::session::Sink;

const SESSION_NAME: &str = "LagScope-Net";

#[derive(Default, Clone, Copy)]
struct Counts {
    udp_tx: u64,
    udp_rx: u64,
    tcp_tx: u64,
    tcp_rx: u64,
    tx_pkts: u32,
    rx_pkts: u32,
}

struct LiveAcc {
    sec: i64,
    rx_pkts: u32,
    tx_pkts: u32,
    rx_bytes: u64,
    tx_bytes: u64,
    last_server_rx: Option<i64>,
    gaps: Vec<i64>,
}

struct NetState {
    app: Arc<App>,
    cache: SchemaCache,
    game: Arc<Sink>,
    procs: Arc<Sink>,
    buckets: BTreeMap<i64, HashMap<u32, Counts>>,
    newest_sec: i64,
    // Server detection over 3 s windows of game UDP receives.
    window_start: i64,
    endpoint_counts: HashMap<SocketAddr, u32>,
    server: Option<SocketAddr>,
    live: LiveAcc,
}

impl NetState {
    fn on_event(&mut self, ev: &Event) {
        let id = ev.id();
        let (is_send, is_udp) = match id {
            10 | 26 => (true, false),
            11 | 27 => (false, false),
            42 | 58 => (true, true),
            43 | 59 => (false, true),
            _ => return,
        };
        let Some(schema) = self.cache.get(ev) else { return };
        let d = ev.data();
        let (Some(pid), Some(size)) = (schema.uint(d, "PID"), schema.uint(d, "size")) else { return };
        let (pid, size) = (pid as u32, size as u64);
        let t = self.app.session.clock.us_from_raw(ev.timestamp());
        let sec = t.div_euclid(1_000_000);

        let c = self.buckets.entry(sec).or_default().entry(pid).or_default();
        match (is_udp, is_send) {
            (true, true) => c.udp_tx += size,
            (true, false) => c.udp_rx += size,
            (false, true) => c.tcp_tx += size,
            (false, false) => c.tcp_rx += size,
        }
        if is_send { c.tx_pkts += 1 } else { c.rx_pkts += 1 }
        if sec > self.newest_sec {
            self.newest_sec = sec;
            self.flush_buckets(sec - 3);
        }

        if !self.app.is_game_pid(pid) {
            return;
        }
        let (raddr, rport, lport) = if is_send {
            (addr(&schema, d, "daddr"), port(&schema, d, "dport"), port(&schema, d, "sport"))
        } else {
            (addr(&schema, d, "saddr"), port(&schema, d, "sport"), port(&schema, d, "dport"))
        };
        let Some(raddr) = raddr else { return };
        row!(
            self.game,
            "{t},{},{},{size},{raddr},{rport},{lport}",
            if is_send { 'S' } else { 'R' },
            if is_udp { 'U' } else { 'T' }
        );
        let remote = SocketAddr::new(raddr, rport);
        self.live_update(t, is_send, is_udp, size, remote);
    }

    fn live_update(&mut self, t: i64, is_send: bool, is_udp: bool, size: u64, remote: SocketAddr) {
        let sec = t.div_euclid(1_000_000);
        if sec != self.live.sec {
            self.publish_live();
            self.live.sec = sec;
        }
        if is_send {
            self.live.tx_pkts += 1;
            self.live.tx_bytes += size;
        } else {
            self.live.rx_pkts += 1;
            self.live.rx_bytes += size;
        }
        if is_udp && !is_send {
            if t - self.window_start > 3_000_000 {
                self.pick_server();
                self.window_start = t;
            }
            *self.endpoint_counts.entry(remote).or_default() += 1;
            if Some(remote) == self.server {
                if let Some(prev) = self.live.last_server_rx {
                    if t > prev {
                        self.live.gaps.push(t - prev);
                    }
                }
                self.live.last_server_rx = Some(t);
            }
        }
    }

    fn pick_server(&mut self) {
        let ignore = &self.app.cfg.profile.ignore_ports;
        let best = self.endpoint_counts.drain().filter(|(ep, _)| !ignore.contains(&ep.port())).max_by_key(|(_, n)| *n);
        if let Some((ep, n)) = best {
            // ~10 packets/s minimum: a real-time game stream, not a lobby/voice trickle.
            if n >= 30 && Some(ep) != self.server {
                self.server = Some(ep);
                *self.app.server.lock().unwrap() = Some(ep);
                self.app.session.event("server", format!("game server endpoint {ep} ({:.0} pkt/s inbound)", n as f64 / 3.0));
                self.app.session.meta_push("servers", serde_json::json!({
                    "t_us": self.app.session.clock.now_us(), "endpoint": ep.to_string()
                }));
            }
        }
    }

    fn publish_live(&mut self) {
        let l = &mut self.live;
        let mut live = self.app.live.lock().unwrap();
        live.game_rx_pps = l.rx_pkts as f64;
        live.game_tx_pps = l.tx_pkts as f64;
        live.game_rx_kbps = l.rx_bytes as f64 * 8.0 / 1000.0;
        live.game_tx_kbps = l.tx_bytes as f64 * 8.0 / 1000.0;
        live.game_max_gap_ms = l.gaps.iter().copied().max().unwrap_or(0) as f64 / 1000.0;
        live.game_jitter_ms = jitter_ms(&l.gaps);
        live.game_net_updated_us = self.app.session.clock.now_us();
        drop(live);
        l.rx_pkts = 0;
        l.tx_pkts = 0;
        l.rx_bytes = 0;
        l.tx_bytes = 0;
        l.gaps.clear();
    }

    fn flush_buckets(&mut self, upto_sec: i64) {
        let keys: Vec<i64> = self.buckets.range(..=upto_sec).map(|(k, _)| *k).collect();
        for k in keys {
            let Some(m) = self.buckets.remove(&k) else { continue };
            let mut top: Option<(u32, u64)> = None;
            for (pid, c) in &m {
                let name = self.app.proc_name(*pid);
                row!(
                    self.procs,
                    "{},{pid},{},{},{},{},{},{},{}",
                    k * 1_000_000,
                    crate::session::csv_escape(&name),
                    c.udp_tx,
                    c.udp_rx,
                    c.tcp_tx,
                    c.tcp_rx,
                    c.tx_pkts,
                    c.rx_pkts
                );
                let total = c.udp_tx + c.udp_rx + c.tcp_tx + c.tcp_rx;
                if !self.app.is_game_pid(*pid) && top.is_none_or(|(_, b)| total > b) {
                    top = Some((*pid, total));
                }
            }
            let talker = top
                .filter(|(_, b)| *b > 100_000)
                .map(|(pid, b)| format!("{} {:.1} Mbit/s", self.app.proc_name(pid), b as f64 * 8.0 / 1e6));
            self.app.live.lock().unwrap().top_talker = talker;
        }
    }
}

impl Drop for NetState {
    fn drop(&mut self) {
        self.flush_buckets(i64::MAX);
    }
}

/// Mean absolute deviation of inter-arrival times (RFC 3550-style jitter estimate).
fn jitter_ms(gaps: &[i64]) -> f64 {
    if gaps.len() < 2 {
        return 0.0;
    }
    let diffs: Vec<f64> = gaps.windows(2).map(|w| (w[1] - w[0]).abs() as f64).collect();
    diffs.iter().sum::<f64>() / diffs.len() as f64 / 1000.0
}

fn addr(schema: &etw::Schema, d: &[u8], name: &str) -> Option<IpAddr> {
    let b = schema.bytes(d, name)?;
    match b.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3]))),
        16 => {
            let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(b).ok()?);
            Some(v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)))
        }
        _ => None,
    }
}

fn port(schema: &etw::Schema, d: &[u8], name: &str) -> u16 {
    schema.bytes(d, name).filter(|b| b.len() == 2).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0)
}

pub fn start(app: Arc<App>) -> Result<Running> {
    let game = app.session.sink("game_packets", "t_us,dir,proto,size,remote_ip,remote_port,local_port");
    let procs = app.session.sink("proc_net", "t_us,pid,name,udp_tx_b,udp_rx_b,tcp_tx_b,tcp_rx_b,tx_pkts,rx_pkts");
    let health = app.session.sink("etw_health", "t_us,session,events_lost,rt_buffers_lost,buffers_written");
    let mut state = NetState {
        app: app.clone(),
        cache: SchemaCache::default(),
        game,
        procs,
        buckets: BTreeMap::new(),
        newest_sec: i64::MIN,
        window_start: 0,
        endpoint_counts: HashMap::new(),
        server: None,
        live: LiveAcc { sec: 0, rx_pkts: 0, tx_pkts: 0, rx_bytes: 0, tx_bytes: 0, last_server_rx: None, gaps: vec![] },
    };
    let spec = ProviderSpec {
        name: "Kernel-Network",
        guid: etw::KERNEL_NETWORK,
        level: 5,
        any_keyword: 0x10 | 0x20, // IPv4 | IPv6
        event_ids: vec![10, 11, 26, 27, 42, 43, 58, 59],
    };
    let sess = RealtimeSession::start(SESSION_NAME, &[spec], move |ev| state.on_event(ev))?;
    Ok(super::etw_running("net", app, sess, health))
}

pub(super) fn poll_health(app: &App, name: &str, sess: &Mutex<RealtimeSession>, health: &Sink, last_lost: &mut u32) {
    let st = sess.lock().unwrap().stats();
    let t = app.session.clock.now_us();
    row!(health, "{t},{name},{},{},{}", st.events_lost, st.rt_buffers_lost, st.buffers_written);
    if st.events_lost > *last_lost {
        app.session.event("etw_lost", format!("{name}: {} events lost so far", st.events_lost));
        *last_lost = st.events_lost;
    }
}

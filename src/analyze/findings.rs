//! Turns the numbers into plain-language findings, a headline and suggestions.

use std::collections::BTreeMap;

use super::detect::{Analysis, Kind};
use super::load::Data;

fn pct(x: f64) -> String {
    format!("{:.0}%", x * 100.0)
}

pub fn summarize(d: &Data, r: &mut Analysis) {
    let mut f: Vec<String> = Vec::new();
    let mut rec: Vec<String> = Vec::new();
    let hours = (d.duration_us as f64 / 3.6e9).max(1.0 / 60.0);
    let is_wifi = d.meta["route"]["is_wifi"].as_bool().unwrap_or(false);
    let count = |k: Kind| r.anomalies.iter().filter(|a| a.kind == k).count();

    // Game stream.
    let s = &r.stream;
    if s.in_packets > 0 {
        f.push(format!(
            "Game stream ({}): {:.0} pkt/s in (every {:.1} ms), {:.0} pkt/s out, {:.0} s active{}. Inbound gaps ≥100 ms during play: {} ({:.1}/min), ≥250 ms: {}. {} of the gaps ended in a burst of delayed packets.",
            s.endpoints.join(", "),
            s.in_rate_pps,
            s.in_median_ms,
            s.out_rate_pps,
            s.active_s,
            if s.match_s > 0.0 { format!(", {:.0} s of it in match play ({} segment(s))", s.match_s, r.phases.len()) } else { String::new() },
            s.gaps_100ms,
            s.gaps_100ms as f64 / ((if s.match_s > 0.0 { s.match_s } else { s.active_s }) / 60.0).max(1.0),
            s.gaps_250ms,
            s.burst_gaps
        ));
    } else if d.files.iter().any(|x| x == "game_packets") {
        f.push("No game network stream was recorded (game not running, or not in a match).".into());
    }

    // Probes.
    for p in &r.probes {
        if p.sent == 0 {
            continue;
        }
        let loss = p.lost as f64 / p.sent as f64;
        if p.lost == p.sent {
            f.push(format!(
                "Ping {} ({}): no replies at all - this host ignores pings, so this path could not be measured with ICMP.",
                p.target, p.ip
            ));
            continue;
        }
        f.push(format!(
            "Ping {} ({}): median {:.1} ms, p99 {:.1} ms, max {:.0} ms, loss {:.2}%, {} spikes ({:.0}/h).",
            p.target,
            p.ip,
            p.p50_ms,
            p.p99_ms,
            p.max_ms,
            loss * 100.0,
            p.spikes,
            p.spikes as f64 / hours
        ));
    }
    for (target, silent) in &d.probe_silent {
        for (ip, n) in silent {
            f.push(format!("Ping {target} ({ip}): never answered ({n} probes) - this host ignores pings, so those probes are not counted as loss."));
        }
    }
    if let Some(b) = r.beacons.first() {
        let lost: usize = r.beacons.iter().map(|b| b.sent.saturating_sub(b.replies)).sum();
        let sent: usize = r.beacons.iter().map(|b| b.sent).sum();
        f.push(format!(
            "The game's own region pings (UDP, {} datacenters): nearest {} median {:.1} ms, max {:.0} ms; {} of {} pings unanswered overall.",
            r.beacons.len(),
            b.endpoint,
            b.p50_ms,
            b.max_ms,
            lost,
            sent
        ));
    }
    let gw = r.probes.iter().find(|p| p.target == "gateway");
    if let Some(g) = gw {
        if g.sent > 100 && (g.p99_ms > 20.0 || g.lost as f64 / g.sent as f64 > 0.005) {
            f.push(format!(
                "The very first hop (your router) is unstable: p99 {:.0} ms / {:.2}% loss. A healthy {} link stays under ~5-10 ms with no loss.",
                g.p99_ms,
                g.lost as f64 * 100.0 / g.sent as f64,
                if is_wifi { "Wi-Fi" } else { "wired" }
            ));
            if is_wifi {
                rec.push("Your Wi-Fi hop itself is spiking. The decisive test: play the same session on an Ethernet cable (or powerline / USB phone tether) and compare the reports.".into());
                rec.push("Wi-Fi tweaks to try one at a time: use the 5 GHz/6 GHz band close to the router, pick a less crowded channel on the router, set the adapter's 'Roaming Aggressiveness' to Lowest and power saving off (Device Manager > adapter > Advanced), update the Wi-Fi driver from the chipset vendor.".into());
            }
        }
    }

    if let Some(gc) = r.gateway_spike_correlation.get("game inbound gap") {
        let n = r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike && a.value >= 30.0).count();
        if n >= 3 && *gc < 0.2 {
            f.push(format!(
                "{n} router-ping spikes ≥30 ms, but only {} of them coincided with a gap in the server → you game stream, so the download direction was unaffected. (Delays in your upload direction cannot be seen from the PC.)",
                pct(*gc)
            ));
        }
    }

    let burst = r.gateway_spike_correlation.get("game region-ping burst (≤1 s before)").copied().unwrap_or(0.0)
        .max(r.gap_correlation.get("game region-ping burst (≤1 s before)").copied().unwrap_or(0.0));
    if burst >= 0.4 {
        f.push(format!(
            "{} of the spikes started within 1 s after the game's own region-ping burst (hundreds of small packets at once): the link (usually the Wi-Fi uplink) struggles to absorb short bursts.",
            pct(burst)
        ));
    }

    // Correlations.
    if !r.gap_correlation.is_empty() {
        let mut parts: Vec<(String, f64)> = r.gap_correlation.iter().map(|(k, v)| (k.clone(), *v)).collect();
        parts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let txt: Vec<String> = parts.iter().filter(|(_, v)| *v > 0.0).map(|(k, v)| format!("{k} {}", pct(*v))).collect();
        f.push(format!(
            "Of {} game stalls ≥100 ms, coincided with: {}.",
            s.gaps_100ms,
            if txt.is_empty() { "nothing else we measure".into() } else { txt.join(", ") }
        ));
    }
    for p in &r.periodicity {
        f.push(format!("{} recur about every {:.1} s ({} events, {} match the period).", p.what, p.period_s, p.events, pct(p.score)));
    }
    let per = |what: &str| r.periodicity.iter().find(|p| p.what.starts_with(what));
    let same_period = |a: f64, b: f64| (a / b - 1.0).abs() < 0.05;
    let gap_per = per("game inbound").or(per("gateway"));
    let scan_corr = r.gap_correlation.get("Wi-Fi scan/roam/drop (±3 s)").copied().unwrap_or(0.0)
        .max(r.gateway_spike_correlation.get("Wi-Fi scan/roam/drop (±3 s)").copied().unwrap_or(0.0));
    let scan_aligned = per("Wi-Fi scans").zip(gap_per).is_some_and(|(a, b)| same_period(a.period_s, b.period_s));
    if is_wifi && (scan_corr >= 0.4 || (scan_aligned && scan_corr >= 0.2)) {
        f.push("Spikes line up with Wi-Fi background scanning: while scanning, the adapter leaves your channel for tens to hundreds of ms.".into());
        rec.push("To test the scan theory: in an admin terminal run `netsh wlan set autoconfig enabled=no interface=\"Wi-Fi\"` before playing (you stay connected but Windows stops background scans) and `netsh wlan set autoconfig enabled=yes interface=\"Wi-Fi\"` afterwards. Also close apps that ask for location/Wi-Fi lists.".into());
    }

    // A first-hop stall on an exact timer that also freezes the game stream.
    let mut clockwork = None;
    if let (Some(gwp), Some(gp)) = (per("gateway"), per("game inbound")) {
        let gwc = r.gap_correlation.get("gateway (Wi-Fi hop) spike/loss").copied().unwrap_or(0.0);
        if gwp.score >= 0.8 && gwp.events >= 10 && same_period(gwp.period_s, gp.period_s) && gwc >= 0.6 {
            let median = |mut v: Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v.get(v.len() / 2).copied().unwrap_or(0.0)
            };
            let gw_ms = median(r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike).map(|a| a.value).collect());
            let gap_ms = median(r.anomalies.iter().filter(|a| a.kind == Kind::InGap && a.in_match).map(|a| a.value).collect());
            let link = if is_wifi { "Wi-Fi link" } else { "link to the router" };
            f.push(format!(
                "Clockwork pattern: every {:.1} s the {link} goes dead for a moment - router ping jumps to ~{:.0} ms and the game stream freezes for ~{:.0} ms, then the held-back packets arrive in one burst ({} of {} game stalls, both directions affected). Interference or a weak signal is never this regular: something on a fixed timer takes the link away{}.",
                gwp.period_s,
                gw_ms,
                gap_ms,
                pct(gwc),
                s.gaps_100ms,
                if scan_corr < 0.2 { ", and it is not Windows' own Wi-Fi scanning (none happened at those moments)" } else { "" }
            ));
            f.push("Short freezes this regular make the game keep a bigger safety buffer for late packets, so shots, hit markers and movement can feel delayed all the time, not only at the freeze itself.".into());
            if is_wifi {
                rec.push(format!(
                    "Hunt the {:.0}-second timer, one change at a time: (1) update the Wi-Fi driver from the PC/motherboard maker or chipset vendor; (2) connect to the router's other band (5 GHz instead of 6 GHz, or the reverse); (3) temporarily turn off Bluetooth, Nearby sharing and Phone Link; (4) restart the router and check its app for features that probe the network periodically (band steering, mesh, motion sensing, auto channel). Each change can be checked in 2 minutes without playing: run LagScope at the desktop, press Q, and see whether the router ping still spikes every {:.0} s.",
                    gwp.period_s, gwp.period_s
                ));
            }
            clockwork = Some(format!(
                "Every {:.1} s the {link} freezes for ~{:.0} ms and the game stream stalls with it: {}",
                gwp.period_s,
                gap_ms,
                category_text(if is_wifi { "wifi" } else { "lan" })
            ));
        }
    }
    if r.gap_correlation.get("other traffic burst").copied().unwrap_or(0.0) >= 0.3 {
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        for a in r.anomalies.iter().filter(|a| a.kind == Kind::TrafficBurst) {
            *names.entry(a.target.clone()).or_default() += 1;
        }
        let mut v: Vec<_> = names.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        let top: Vec<String> = v.iter().take(3).map(|(n, c)| format!("{n} ({c}×)")).collect();
        rec.push(format!(
            "Other programs were moving a lot of data during stalls ({}). Pause downloads/sync while playing, or enable SQM/QoS (\"bufferbloat\" control) on the router.",
            top.join(", ")
        ));
    }

    // Frames / system.
    let fr = &r.frames;
    if fr.frames > 100 {
        f.push(format!(
            "Frames: {:.0} fps average, 1% low {:.0} fps, {} hitches ≥50 ms, {} ≥100 ms.",
            fr.avg_fps, fr.low1_fps, fr.hitches_50ms, fr.hitches_100ms
        ));
    }
    let stalls = count(Kind::Stall);
    if stalls > 0 {
        f.push(format!("{stalls} whole-system stalls (≥8 ms timer delay) detected - usually a driver (DPC/ISR) or firmware issue."));
        rec.push("System stalls were detected: run LatencyMon for 10 minutes to see which driver is responsible (Wi-Fi, audio and GPU drivers are the usual suspects).".into());
    }
    let dpc = count(Kind::DpcSpike);
    if dpc > 0 {
        f.push(format!("{dpc} samples with high DPC/interrupt load (≥5% of CPU time)."));
    }
    if count(Kind::GpuLimit) > 0 {
        f.push("The GPU hit thermal / hardware slowdown limits during the session.".into());
    }

    // Data-quality notes.
    let ignored = r.anomalies.iter().filter(|a| !a.in_match && matches!(a.kind, Kind::InGap | Kind::FrameHitch)).count();
    if ignored > 0 {
        f.push(format!("{ignored} stream gaps / frame hitches outside match play (menus, loading, spectating) are shown but not counted."));
    }
    if r.wifi_counters_bogus {
        f.push("The Wi-Fi driver's retry counters are not plausible (more retries than frames sent), so Wi-Fi retries were ignored.".into());
    }
    let rssi: Vec<f64> = d.wifi.iter().filter_map(|w| w.rssi).collect();
    if rssi.len() > 100 && rssi.iter().all(|&x| x == rssi[0]) {
        f.push(format!("The Wi-Fi driver reported a constant signal ({:.0} dBm) for the whole session - it caches it, so link quality from Windows' WLAN service is used instead.", rssi[0]));
    }
    if d.meta["elevated"].as_bool() == Some(false) {
        rec.push("LagScope was not run as administrator, so game traffic and frame data are missing. Right-click > Run as administrator.".into());
    }
    if d.meta["wifi_location_blocked"].as_bool() == Some(true) {
        rec.push("Windows Location services were off, so RSSI / PHY rate / roaming details were not available (link quality and scan events were still recorded). For richer Wi-Fi data, turn on Settings > Privacy & security > Location > Location services and 'Let desktop apps access your location' for the test session.".into());
    }
    if d.etw_lost > 0 {
        f.push(format!("Note: {} trace events were lost (very heavy system load); counts may be slightly low.", d.etw_lost));
    }
    if let Some(n) = d.meta["notable_processes"].as_array() {
        let names: Vec<&str> = n.iter().filter_map(|v| v.as_str()).collect();
        let boosters: Vec<&&str> = names
            .iter()
            .filter(|n| {
                let l = n.to_ascii_lowercase();
                ["killer", "exitlag", "gearup", "noping", "wtfast", "vpn", "wireguard", "tailscale", "zerotier", "warp", "smartbyte"].iter().any(|k| l.contains(k))
            })
            .collect();
        if !boosters.is_empty() {
            let adapter = format!("{} {}", d.meta["route"]["adapter"].as_str().unwrap_or(""), d.meta["route"]["description"].as_str().unwrap_or("")).to_ascii_lowercase();
            let tunnel = ["vpn", "nord", "tap", "tun", "wireguard", "tailscale", "zerotier", "warp", "wintun"].iter().any(|k| adapter.contains(k));
            rec.push(format!(
                "Network-altering software is running ({}){}. Try a session with it fully closed/disabled.",
                boosters.iter().map(|s| **s).collect::<Vec<_>>().join(", "),
                if tunnel { " and traffic is routed through it" } else { " (traffic was not routed through a tunnel, but such services can still install network filters)" }
            ));
        }
    }
    if d.marks.is_empty() {
        rec.push("No lag spikes were marked. Next time press the hotkey right when a hiccup happens - marks make the diagnosis far more precise.".into());
    }

    // Headline from mark verdicts, else from correlations.
    let mut cats: BTreeMap<&str, usize> = BTreeMap::new();
    for v in &r.verdicts {
        *cats.entry(v.category).or_default() += 1;
    }
    let headline = if let Some(c) = clockwork {
        c
    } else if let Some((cat, n)) = cats.iter().max_by_key(|x| *x.1) {
        format!("{n} of {} marked spikes point to: {}", r.verdicts.len(), category_text(cat))
    } else if s.gaps_100ms > 0 {
        let gwc = r.gap_correlation.get("gateway (Wi-Fi hop) spike/loss").copied().unwrap_or(0.0);
        let fc = r.gap_correlation.get("frame hitch").copied().unwrap_or(0.0);
        if gwc >= 0.4 {
            format!("Game stalls mostly coincide with spikes on your first network hop ({}): {}", pct(gwc), category_text(if is_wifi { "wifi" } else { "lan" }))
        } else if fc >= 0.5 {
            format!("Game stalls mostly coincide with frame hitches ({}): local PC", pct(fc))
        } else {
            "Game stalls occur while your own network looks clean: path to / the game server itself".into()
        }
    } else {
        "No significant problems detected in this session.".into()
    };

    if !r.verdicts.is_empty() || s.gaps_100ms > 0 {
        rec.push("Compare with a control session: run LagScope with `--process <other game>.exe` while playing a game that feels fine.".into());
    }
    r.headline = headline;
    r.findings = f;
    r.recommendations = rec;
}

pub fn category_text(cat: &str) -> &'static str {
    match cat {
        "wifi" => "the Wi-Fi link between the PC and the router",
        "lan" => "the local network link to the router",
        "isp" => "the internet connection / ISP",
        "route" => "the route to the game server",
        "server" => "the game server side (your network looked clean)",
        "contention" => "bandwidth contention from other traffic",
        "local" => "the PC itself (frame hitches / system stalls)",
        _ => "nothing measurable (server-side or perceptual)",
    }
}

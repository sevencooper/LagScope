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

    // Wi-Fi check (test windows).
    let mut check_headline = None;
    if let Some(o) = &r.wifi_check {
        f.extend(o.summary.iter().cloned());
        for w in &o.windows {
            f.push(format!(
                "  test '{}': {} freezes in {:.0} s ({:.1}/min{}), router ping median {:.1} ms, p99 {:.0} ms, {} lost.",
                w.label,
                w.spikes,
                w.secs,
                w.per_min,
                w.change_pct.map(|c| format!(", {c:+.0}% vs. baseline")).unwrap_or_default(),
                w.p50_ms,
                w.p99_ms,
                w.lost
            ));
        }
        if o.problem && o.fixes.is_empty() && o.partial.is_empty() {
            rec.push(ROUTER_STEPS.into());
        }
        check_headline = Some(match (o.problem, o.fixes.first()) {
            (false, _) => "Wi-Fi check: no regular freezes on the Wi-Fi link".to_string(),
            (true, Some(k)) => format!(
                "Wi-Fi check: the freezes stopped with \"{}\"",
                o.windows.iter().find(|w| &w.key == k).map(|w| w.label.as_str()).unwrap_or(k)
            ),
            (true, None) => "Wi-Fi check: the freezes are not caused by a PC setting - most likely the router".to_string(),
        });
    }
    for c in &r.driver_cmds {
        f.push(format!(
            "Windows driver command '{}' (from {}) came right before {} of the router-ping freezes ({} expected by chance, {} times in total): something on the PC asks the network driver to do this each time.",
            c.what,
            c.process,
            pct(c.hit),
            pct(c.chance),
            c.count
        ));
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
        // The server path is usually probed via a router that answers TTL-limited pings;
        // routers rate-limit those answers, so a few % "loss" there is normal.
        let via_router = p.target == "server" && p.ip.contains('→') || (p.target == "server" && d.events.iter().any(|e| e.detail.contains("TTL-limited")));
        f.push(format!(
            "Ping {} ({}): median {:.1} ms, p99 {:.1} ms, max {:.0} ms, loss {:.2}%{}, {} spikes ({:.0}/h).",
            p.target,
            p.ip,
            p.p50_ms,
            p.p99_ms,
            p.max_ms,
            loss * 100.0,
            // Routers answering on the server's behalf rate-limit those answers: unanswered
            // probes there are not packet loss (the game stream shows real loss).
            if via_router && loss > 0.0 { " (routers on the path answer on the server's behalf and limit those answers, so unanswered probes here are not real loss)" } else { "" },
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
    let gw_unstable = gw.is_some_and(|g| g.sent > 100 && (g.p99_ms > 20.0 || g.lost as f64 / g.sent as f64 > 0.005));
    if let Some(g) = gw {
        if gw_unstable {
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

    // Upload freezes: the game's data kept arriving, but the PC's own packets were held up.
    let ups: Vec<f64> = r.anomalies.iter().filter(|a| a.kind == Kind::UploadStall && a.in_match).map(|a| a.value).collect();
    let up_ms = {
        let mut v = ups.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let play_min_all = ((if s.match_s > 0.0 { s.match_s } else { s.active_s }) / 60.0).max(1.0);
    let upload_issue = ups.len() >= 3 && ups.len() as f64 / play_min_all >= 0.3;
    if !ups.is_empty() {
        f.push(format!(
            "{} time(s) during play the PC's uploads froze for about {up_ms:.0} ms while the game's incoming data kept arriving (pings to the router and beyond stalled together). Each time, your movement and shots reached the server late, which feels like rubber-banding or being pulled back.",
            ups.len()
        ));
    }
    if let Some((k, n)) = r.second_timer {
        f.push(format!(
            "{k} of {n} router freezes started at the same point within a second: something on a 1-second timer sets them off (typically the router or the network adapter's firmware). Interference or a weak signal would hit at random moments."
        ));
    }
    // The general Wi-Fi advice above already covers an unstable router hop.
    if upload_issue && is_wifi && !gw_unstable {
        rec.push(format!(
            "The PC's uploads keep freezing for ~{up_ms:.0} ms on the Wi-Fi ({} times). Test with an Ethernet cable if you can; otherwise try the router's other band, update the Wi-Fi driver from the PC or chipset maker, and restart the router.",
            ups.len()
        ));
    }

    if let Some(gc) = r.gateway_spike_correlation.get("game inbound gap") {
        let n = r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike && a.value >= 30.0).count();
        // With upload freezes found, the upload side is covered by their own finding.
        if n >= 3 && *gc < 0.2 && ups.is_empty() {
            f.push(format!(
                "{n} router-ping spikes ≥30 ms, but only {} of them coincided with a gap in the server → you game stream, so the download direction was unaffected; none of them was long enough to count as an upload freeze.",
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
        parts.sort_by(|a, b| b.1.total_cmp(&a.1));
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
        let gwc = r.gap_correlation.get(super::detect::GW_KEY).copied().unwrap_or(0.0);
        if gwp.score >= 0.8 && gwp.events >= 10 && same_period(gwp.period_s, gp.period_s) && gwc >= 0.6 {
            let median = |mut v: Vec<f64>| {
                v.sort_by(|a, b| a.total_cmp(b));
                v.get(v.len() / 2).copied().unwrap_or(0.0)
            };
            let gw_ms = median(r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike).map(|a| a.value).collect());
            let gw_lost = r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeLoss).count();
            let gw_slow = r.anomalies.iter().filter(|a| a.target == "gateway" && a.kind == Kind::ProbeSpike).count();
            let ping_txt = if gw_lost > gw_slow { "router pings get lost".to_string() } else { format!("router ping jumps to ~{gw_ms:.0} ms") };
            let gap_ms = median(r.anomalies.iter().filter(|a| a.kind == Kind::InGap && a.in_match).map(|a| a.value).collect());
            let link = if is_wifi { "Wi-Fi link" } else { "link to the router" };
            f.push(format!(
                "Clockwork pattern: every {:.1} s the {link} goes dead for a moment - {ping_txt} and the game stream freezes for ~{:.0} ms, then the held-back packets arrive in one burst ({} of {} game stalls, both directions affected). Interference or a weak signal is never this regular: something on a fixed timer takes the link away{}.",
                gwp.period_s,
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
    // Other traffic during game stalls: only with a few stalls to go on, naming the programs
    // that were busy around those stalls.
    let stalls: Vec<&super::detect::Anomaly> = r.anomalies.iter().filter(|a| a.kind == Kind::InGap && a.in_match && a.value >= 100.0).collect();
    if stalls.len() >= 3 && r.gap_correlation.get("other traffic burst").copied().unwrap_or(0.0) >= 0.3 {
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        let mut hit = 0;
        for g in &stalls {
            let near: Vec<&str> = r.anomalies.iter().filter(|a| a.kind == Kind::TrafficBurst && a.t <= g.end() + 1_000_000 && a.end() >= g.t - 2_000_000).map(|a| a.target.as_str()).collect();
            hit += !near.is_empty() as usize;
            for n in near {
                *names.entry(n.to_string()).or_default() += 1;
            }
        }
        let mut v: Vec<_> = names.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        let top: Vec<&str> = v.iter().take(3).map(|(n, _)| n.as_str()).collect();
        rec.push(format!(
            "{hit} of {} game stalls came while other programs were moving a lot of data ({}). Pausing downloads and sync while playing, or turning on SQM / QoS (\"bufferbloat\" control) on the router, may help.",
            stalls.len(),
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
    let playing = |k: Kind| r.anomalies.iter().filter(|a| a.kind == k && a.in_match).count();
    let elsewhere = |k: Kind| count(k) - playing(k);
    let stalls = playing(Kind::Stall);
    let stalls_out = elsewhere(Kind::Stall);
    if stalls > 0 {
        f.push(format!(
            "{stalls} whole-system stalls (≥8 ms timer delay) while playing - usually a driver (DPC/ISR) or firmware issue{}.",
            if stalls_out > 0 { format!("; {stalls_out} more outside match play are not judged") } else { String::new() }
        ));
    } else if stalls_out > 0 {
        f.push(format!("No whole-system stalls while playing ({stalls_out} outside match play, e.g. while the game was loading, are not judged)."));
    }
    if stalls >= 3 {
        rec.push("The whole PC stalled for a moment while playing: LatencyMon (10 minutes) can show which driver is responsible (Wi-Fi, audio and graphics drivers are the usual suspects).".into());
    }
    let dpc = playing(Kind::DpcSpike);
    if dpc > 0 {
        f.push(format!("{dpc} moments with very high driver load while playing (DPC/interrupt time a full CPU core above normal)."));
    }
    // Router identification and its advice (advice only when the network is implicated).
    if let Some(name) = d.meta["router"]["name"].as_str().filter(|n| !n.is_empty()) {
        f.push(format!("Router: {name} at {}.", d.meta["router"]["gateway"].as_str().unwrap_or("?")));
    }
    let drivers = driver_findings(d, r, &mut f, &mut rec);
    let disk = disk_findings(d, r, &mut f, &mut rec);
    let mut checks = super::health::system_findings(d, r, &mut f, &mut rec);
    checks.drivers = drivers;
    checks.disk = disk;
    if count(Kind::GpuLimit) > 0 {
        f.push(format!("The GPU's clocks were held back by heat or a power brake {:.0}% of the time while playing.", super::health::gpu_limit_share(d, r) * 100.0));
        if super::health::gpu_limited(d, r) {
            rec.push(if super::hardware::is_laptop(d) {
                "The graphics card kept slowing down from heat: raise the laptop's back for airflow, clean its vents, use its performance / fan mode, or cap the frame rate a little below the screen's refresh rate."
            } else {
                "The graphics card kept slowing down from heat: check that its fans spin up, clean dust from the card and case filters, improve case airflow, or cap the frame rate a little below the monitor's refresh rate."
            }
            .into());
        }
    }

    // Data-quality notes.
    if !r.away.is_empty() {
        let secs: f64 = r.away.iter().map(|(a, b)| (b - a) as f64 / 1e6).sum();
        f.push(format!(
            "You switched away from the game {} time(s) (alt-tab or another window, about {secs:.0} s in total with the moments around it): frames then are not judged.",
            r.away_switches.max(r.away.len())
        ));
    }
    let ignored = r.anomalies.iter().filter(|a| !a.in_match && matches!(a.kind, Kind::InGap | Kind::FrameHitch)).count();
    if ignored > 0 {
        f.push(format!("{ignored} stream gaps / frame hitches outside match play (menus, loading, spectating) or while you were switched away are shown but not counted."));
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
        f.push("Windows Location services were off, so RSSI / PHY rate / roaming details were not available (link quality and scan events were still recorded). For richer Wi-Fi data, turn on Settings > Privacy & security > Location > Location services and 'Let desktop apps access your location' for the test session.".into());
    }
    if let Ok(e) = std::fs::read_to_string(d.dir.join("errors.txt")) {
        f.push(format!("LagScope hit an internal error while recording, so some data may be missing: {}", e.lines().next().unwrap_or("")));
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
            let text = format!(
                "Network-altering software is running ({}){}.",
                boosters.iter().map(|s| **s).collect::<Vec<_>>().join(", "),
                if tunnel { " and traffic is routed through it" } else { " (traffic was not routed through it)" }
            );
            // Only advice when the network actually misbehaved or the game goes through it.
            let net_trouble = gw_unstable || upload_issue || s.gaps_100ms >= 3;
            if tunnel || net_trouble {
                rec.push(format!("{text} If the lag continues, try a session with it fully closed; such services can add delay even when traffic is not routed through them."));
            } else {
                f.push(text);
            }
        }
    }
    // A tip only for recordings with a game in them.
    if d.marks.is_empty() && (s.in_packets > 0 || r.frames.frames > 100) {
        rec.push("No lag spikes were marked. Next time, press the mark key when a hiccup happens: marks make the diagnosis much more precise.".into());
    }

    // Headline from mark verdicts, else from correlations.
    // Marks made while switched away from the game say nothing about the game.
    let mut cats: BTreeMap<&str, usize> = BTreeMap::new();
    for v in r.verdicts.iter().filter(|v| v.category != "away") {
        *cats.entry(v.category).or_default() += 1;
    }
    let judged = r.verdicts.iter().filter(|v| v.category != "away").count();
    let away_marks = r.verdicts.len() - judged;
    let headline = if let Some(c) = check_headline {
        c
    } else if let Some(c) = clockwork {
        c
    } else if let Some((cat, n)) = cats.iter().max_by_key(|x| *x.1) {
        format!(
            "{n} of {judged} marked spikes point to: {}{}",
            category_text(cat),
            if away_marks > 0 { format!(" ({away_marks} more marked while switched away from the game)") } else { String::new() }
        )
    } else if s.gaps_100ms >= 3 && s.gaps_100ms as f64 / ((if s.match_s > 0.0 { s.match_s } else { s.active_s }) / 60.0).max(1.0) >= 0.3 {
        let gwc = r.gap_correlation.get(super::detect::GW_KEY).copied().unwrap_or(0.0);
        let fc = r.gap_correlation.get("frame hitch").copied().unwrap_or(0.0);
        if gwc >= 0.4 {
            format!("Game stalls mostly coincide with spikes on your first network hop ({}): {}", pct(gwc), category_text(if is_wifi { "wifi" } else { "lan" }))
        } else if fc >= 0.5 {
            format!("Game stalls mostly coincide with frame hitches ({}): local PC", pct(fc))
        } else {
            "Game stalls occur while your own network looks clean: path to / the game server itself".into()
        }
    } else if upload_issue {
        format!("Your uploads froze {} times (~{up_ms:.0} ms each) while the game's data kept arriving: {}", ups.len(), category_text(if is_wifi { "wifi" } else { "lan" }))
    } else if s.gaps_100ms > 0 {
        format!("No significant problems: only {} brief game-stream stall(s) in the whole session.", s.gaps_100ms)
    } else {
        "No significant problems detected in this session.".into()
    };

    if !is_wifi && (gw_unstable || upload_issue || headline.contains("link to the router")) {
        rec.insert(0, "The link between the PC and the router is what stalls. If the PC connects through a powerline adapter, a mesh node or a switch, test with a cable straight to the router; otherwise try another cable and router port, update the network adapter's driver, and restart the router.".into());
    }
    let network_issue = headline.contains("Wi-Fi") || headline.contains("router") || headline.contains("network hop") || r.wifi_check.as_ref().is_some_and(|o| o.problem);
    if network_issue {
        for n in d.meta["router"]["notes"].as_array().into_iter().flatten().filter_map(|n| n.as_str()) {
            rec.insert(rec.len().min(2), n.to_string());
        }
    }
    r.findings = f;
    r.recommendations = rec;
    r.health = super::health::scorecard(d, r, &checks);
    let look: Vec<&str> = r.health.iter().filter(|h| h.status == "warn").map(|h| h.area.as_str()).collect();
    r.headline = if headline.starts_with("No significant problems") && !look.is_empty() {
        format!("No lag problems found, but worth a look: {}", look.join(", "))
    } else {
        headline
    };
    r.background_cpu = checks.background;
    r.hitch_procs = checks.hitch_procs;
}

/// Disk activity summary and whether frame hitches line up with the game loading from disk.
fn disk_findings(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>) -> Option<(bool, String)> {
    if d.disk_1s.is_empty() {
        return None;
    }
    let peak = d.disk_1s.iter().map(|s| s.read_mb).fold(0.0, f64::max);
    let peak_w = d.disk_1s.iter().map(|s| s.write_mb).fold(0.0, f64::max);
    let slowest = d.disk_1s.iter().map(|s| s.max_io_ms).fold(0.0, f64::max);
    let fault_mb: f64 = d.disk_1s.iter().map(|s| s.game_fault_mb).sum();
    // Moments the disk was held up (I/Os stalled together count once), while playing.
    let slow = r.anomalies.iter().filter(|a| a.kind == Kind::DiskSlow && a.in_match).count();
    let slow_ms = r.anomalies.iter().filter(|a| a.kind == Kind::DiskSlow && a.in_match).map(|a| a.value).fold(0.0, f64::max);
    f.push(format!(
        "Disk: peak reads {peak:.0} MB/s, writes {peak_w:.0} MB/s, slowest single I/O {slowest:.0} ms{}; the game read {fault_mb:.0} MB from disk through page faults.",
        if slow > 0 { format!(" ({slow} moment(s) while playing when the disk was held up for 100 ms or more)") } else { String::new() }
    ));
    let paging = r.hitch_correlation.get("game reading from disk").copied().unwrap_or(0.0);
    let slow_c = r.hitch_correlation.get("slow disk I/O").copied().unwrap_or(0.0);
    let n = r.anomalies.iter().filter(|a| a.kind == Kind::FrameHitch && a.in_match && a.value >= 50.0).count();
    if n >= 3 && (paging >= 0.4 || slow_c >= 0.3) {
        f.push(format!(
            "Frame hitches line up with disk activity: {} of {n} hitches came while the game was loading data from disk{}.",
            pct(paging),
            if slow_c > 0.0 { format!(", {} with a slow disk I/O", pct(slow_c)) } else { String::new() }
        ));
        rec.push("Stutter comes with the game loading data from disk: install the game on an SSD (ideally NVMe), keep some free space on it, close memory-hungry programs (browsers) while playing, and expect the first matches after a game or driver update to stutter more while shader caches rebuild.".into());
        return Some((true, format!("{} of hitches while the game loaded from disk", pct(paging))));
    } else if slow >= 3 {
        rec.push(format!(
            "The disk was held up for up to {slow_ms:.0} ms {slow} times while playing, which can cause hitches: usually something else was using it heavily (antivirus scan, Windows Update, downloads){}.",
            if slow >= 10 { "; this often, it is also worth checking the drive's health (e.g. CrystalDiskInfo)" } else { "" }
        ));
        return Some((true, format!("held up {slow} times while playing, up to {slow_ms:.0} ms")));
    }
    Some((false, format!("slowest I/O {slowest:.0} ms, game loaded {fault_mb:.0} MB")))
}

/// LatencyMon-style summary of driver DPC/ISR times, plus what they coincided with.
/// What to do about a driver with long DPC / interrupt runs, by what kind of driver it is.
fn driver_advice(n: &str) -> String {
    let l = n.to_ascii_lowercase();
    let is = |names: &[&str]| names.iter().any(|x| l == *x);
    if is(&["tcpip.sys", "netio.sys", "ndis.sys", "afd.sys", "wfplwfs.sys", "fwpkclnt.sys"]) {
        format!("'{n}' is Windows' own network stack, so there is nothing to update separately: long runs there usually come from heavy network traffic (downloads) or network filter software (VPNs, firewalls, \"ping boosters\"). Try a session with those closed.")
    } else if is(&["ntoskrnl.exe", "wdf01000.sys", "acpi.sys", "hal.dll", "intelppm.sys", "amdppm.sys"]) {
        format!("'{n}' is part of Windows itself and runs work for other drivers (USB devices, touchpad, power management), so it does not say which device is responsible. LatencyMon can narrow it down; updating the chipset drivers from the PC or motherboard maker may help.")
    } else if is(&["storport.sys", "stornvme.sys", "classpnp.sys", "disk.sys", "storahci.sys", "iastorvd.sys"]) {
        format!("'{n}' is a storage driver: long runs come with heavy disk activity. Updating the SSD's firmware and the storage / chipset drivers from the PC maker may help.")
    } else if super::detect::is_gpu_driver(n) {
        format!("'{n}' belongs to the graphics driver: installing the latest graphics driver may help.")
    } else {
        format!("Updating '{n}' from the PC, motherboard or chipset maker may help; also check that device's power-saving settings.")
    }
}

fn driver_findings(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>) -> Option<(bool, String)> {
    let meta = d.meta["driver_latency"]["drivers"].as_array()?;
    if r.wifi_check.is_some() {
        // The check restarts the Wi-Fi adapter on purpose, which makes drivers busy.
        f.push("Driver latency is not judged in a Wi-Fi check (the check restarts the Wi-Fi adapter on purpose); record a normal session for that.".into());
        return None;
    }
    // Worst run per driver while playing: game start-up, loading screens and menus keep
    // drivers busy in every game and are not judged. (driver_long.csv has every run of
    // 0.5 ms or more; recordings without it fall back to the whole-session figures.)
    let mut worst: BTreeMap<String, f64> = BTreeMap::new();
    if d.driver_long.is_empty() {
        for v in meta {
            let us = v["dpc_max_us"].as_f64().unwrap_or(0.0).max(v["isr_max_us"].as_f64().unwrap_or(0.0));
            worst.insert(v["driver"].as_str().unwrap_or("?").to_string(), us);
        }
    } else {
        for (t, _, drv, us) in &d.driver_long {
            if super::health::in_play(r, *t) {
                let e = worst.entry(drv.clone()).or_default();
                *e = e.max(*us);
            }
        }
    }
    let mut list: Vec<(String, f64)> = worst.into_iter().collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    // Judge on "effective" time: graphics drivers get 1 ms of extra allowance (their 1-2 ms
    // DPCs under gaming load are normal).
    let effective = |(n, us): &(String, f64)| (us - (super::detect::driver_threshold_us(n) - super::detect::DRIVER_US)).max(0.0);
    let long = r.anomalies.iter().filter(|a| a.kind == Kind::DriverLatency && a.in_match).count();
    let outside = r.anomalies.iter().filter(|a| a.kind == Kind::DriverLatency && !a.in_match).count();
    if outside > 0 {
        f.push(format!("{outside} long driver runs outside match play (game start-up, loading, menus) are not judged."));
    }
    let Some(top) = list.iter().max_by(|a, b| effective(a).total_cmp(&effective(b))).cloned() else {
        f.push("Driver latency while playing: no driver ran 0.5 ms or longer.".into());
        return Some((false, "no long runs while playing".into()));
    };
    let top_us = effective(&top);
    let names: Vec<String> = list
        .iter()
        .take(3)
        .filter(|v| v.1 >= 250.0)
        .map(|(n, us)| format!("{n} {:.2} ms{}", us / 1000.0, if super::detect::is_gpu_driver(n) { " (graphics)" } else { "" }))
        .collect();
    let gpu_note = if list.iter().take(3).any(|(n, us)| super::detect::is_gpu_driver(n) && *us >= 1000.0) {
        " (graphics drivers are judged from 2 ms: 1-2 ms is normal for them under gaming load)"
    } else {
        ""
    };
    // Severity depends on how often long runs happen, not just the single worst one: a few
    // isolated 2-4 ms runs in an hour of gaming are harmless; one every minute is not.
    let play_min = (if r.stream.match_s > 0.0 { r.stream.match_s } else { d.duration_us as f64 / 1e6 } / 60.0).max(1.0);
    let per_min = long as f64 / play_min;
    let rare = top_us < 8000.0 && per_min < 0.5;
    let verdict = if top_us < 500.0 {
        "all fine (under 0.5 ms)".to_string()
    } else if top_us < 1000.0 {
        "OK (under 1 ms)".to_string()
    } else if rare {
        format!("OK - only {long} isolated longer run(s), too rare to matter")
    } else if top_us < 2000.0 {
        "elevated (1-2 ms runs, often: can cause small hitches and audio crackle)".to_string()
    } else {
        "high (runs over 2 ms, often or very long: they hold up a CPU core long enough to delay network packets, frames and audio)".to_string()
    };
    f.push(format!(
        "Driver latency while playing (longest DPC/interrupt run per driver): {verdict}{gpu_note}{}.",
        if names.is_empty() { String::new() } else { format!(" - worst: {}", names.join(", ")) }
    ));
    if long > 0 {
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for a in r.anomalies.iter().filter(|a| a.kind == Kind::DriverLatency && a.in_match) {
            *per.entry(a.target.as_str()).or_default() += 1;
        }
        let mut v: Vec<_> = per.into_iter().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        let txt: Vec<String> = v.iter().take(4).map(|(n, c)| format!("{n} {c}x")).collect();
        f.push(format!("{long} long driver runs while playing (1 ms or more; graphics drivers 2 ms or more): {}.", txt.join(", ")));
    }
    let gc = r.gap_correlation.get("driver DPC/ISR ≥1 ms").copied().unwrap_or(0.0);
    let pc = r.gateway_spike_correlation.get("driver DPC/ISR ≥1 ms").copied().unwrap_or(0.0);
    if gc >= 0.3 || pc >= 0.3 {
        f.push(format!(
            "Long driver runs line up with network stalls ({} of game stalls, {} of router-ping spikes): a driver on this PC is delaying packets.",
            pct(gc),
            pct(pc)
        ));
    }
    if top_us >= 2000.0 && !rare {
        rec.push(format!("Driver '{}' had DPC/interrupt runs of up to {:.1} ms while playing ({per_min:.1} long runs per minute). {}", top.0, top.1 / 1000.0, driver_advice(&top.0)));
    }
    let warn = top_us >= 1000.0 && !rare;
    Some((warn, format!("worst {} {:.1} ms{}", top.0, top.1 / 1000.0, if long > 0 { format!(", {long} long runs") } else { String::new() })))
}

pub const ROUTER_STEPS: &str = "Router steps (the PC side is ruled out): 1) unplug the router/gateway for 30 s; 2) in its app or web page, give the 2.4 / 5 / 6 GHz bands separate network names and connect the PC to the 5 GHz one; 3) turn off extras that probe the network regularly (Wi-Fi motion sensing, band steering, 'smart' or auto channel, mesh/extender pods) one at a time; 4) set a fixed channel; 5) if it is an ISP gateway (e.g. Xfinity), ask for a replacement or use your own router.";

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

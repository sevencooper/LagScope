//! PC-side checks (other programs' CPU use, CPU power limits, power source, display refresh
//! rate, memory, network link; hardware set-up in hardware.rs) and the one-glance scorecard shown at the top of a report.

use std::collections::BTreeMap;

use super::detect::{Analysis, Health, Kind, percentile};
use super::load::Data;

/// Status of the checks that feed the scorecard (computed alongside the findings).
#[derive(Default)]
pub struct Checks {
    pub cpu: Vec<String>,
    pub memory: Option<(bool, String)>,
    pub setup: Vec<String>,
    pub setup_ok: Vec<String>,
    pub link: Vec<String>,
    pub drivers: Option<(bool, String)>,
    pub disk: Option<(bool, String)>,
    pub background: Vec<(String, f64, f64)>,
    pub hitch_procs: Vec<(String, usize)>,
    /// Hardware set up wrong, and a short description when all is well.
    pub hardware: Vec<String>,
    pub hardware_ok: Vec<String>,
}

fn in_play(r: &Analysis, t: i64) -> bool {
    r.phases.is_empty() || r.phases.iter().any(|&(a, b)| t >= a && t <= b)
}

/// What a busy background program usually is, and what to do about it.
fn explain(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let has = |keys: &[&str]| keys.iter().any(|k| n.contains(k));
    Some(if n == "system" || n == "interrupts" {
        "Windows' own kernel work (drivers, file system, memory management) was busy. It is not a program you can close: check the Drivers row of this report, and run LatencyMon to see which driver is responsible."
    } else if n == "memory compression" {
        "Windows was compressing memory because RAM was running short: close memory-hungry programs (browsers, launchers) while playing."
    } else if has(&["msmpeng", "mpdefendercoreservice", "nissrv"]) {
        "Windows Security (Defender) was scanning. Let a scan finish before playing, or add the game's folder as an exclusion (Windows Security > Virus & threat protection > Manage settings > Exclusions)."
    } else if has(&["tiworker", "trustedinstaller", "wuauclt", "mousocoreworker", "usoclient", "wuaucore"]) {
        "Windows Update was working in the background. Let it finish (Settings > Windows Update) before playing."
    } else if has(&["searchindexer", "searchprotocolhost", "searchfilterhost"]) {
        "Windows Search was indexing files. It calms down once indexing is done; pausing large file copies helps."
    } else if has(&["compattelrunner", "devicecensus"]) {
        "Windows telemetry (compatibility checks) was running. It is occasional and finishes by itself."
    } else if has(&["steam", "epicgameslauncher", "battle.net", "eadesktop", "ubisoftconnect", "riotclient"]) {
        "A game launcher was busy (downloads, updates or shader processing). Pause downloads while playing."
    } else if has(&["chrome", "msedge", "firefox", "opera", "brave"]) {
        "A browser was busy (video, many tabs). Close it or its heavy tabs while playing."
    } else if has(&["obs", "medal", "overwolf", "outplayed", "nvidia share", "nvcontainer"]) {
        "Recording / clipping software was busy. Lower its recording quality or turn it off to test."
    } else if has(&["wallpaper"]) {
        "Wallpaper Engine was rendering. Set it to pause while other apps are fullscreen."
    } else if has(&["icue", "armourycrate", "lghub", "synapse", "signalrgb", "openrgb", "dragon center"]) {
        "RGB / device software was busy. These are known to cause stutter; close it to test."
    } else {
        return None;
    })
}

fn short(name: &str) -> &str {
    name.strip_suffix(".exe").or(name.strip_suffix(".EXE")).unwrap_or(name)
}

/// Fills in the PC-side findings and suggestions; returns their status for the scorecard.
pub fn system_findings(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>) -> Checks {
    let mut c = Checks::default();
    background(d, r, f, rec, &mut c);
    cpu_limits(d, r, f, rec, &mut c);
    memory(d, r, f, rec, &mut c);
    setup(d, r, f, rec, &mut c);
    link(d, r, f, rec, &mut c);
    software(d, f);
    super::hardware::findings(d, f, rec, &mut c);
    c
}

/// Other programs that used a lot of CPU, and whether they were busy during frame hitches.
fn background(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    if d.proc_cpu.is_empty() && !d.files.iter().any(|x| x == "proc_cpu") {
        return;
    }
    // Average share of a core over play time, per program.
    let play_s = d.cpu.iter().filter(|x| in_play(r, x.t)).count() as f64 / 4.0;
    let mut sum: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for (t, name, pct) in &d.proc_cpu {
        if in_play(r, *t) {
            let e = sum.entry(name.as_str()).or_default();
            e.0 += pct;
            e.1 = e.1.max(*pct);
        }
    }
    let mut avg: Vec<(String, f64, f64)> = sum.into_iter().map(|(n, (s, m))| (n.to_string(), if play_s > 0.0 { s / play_s } else { 0.0 }, m)).collect();
    avg.sort_by(|a, b| b.1.total_cmp(&a.1));
    avg.truncate(5);
    c.background = avg.clone();

    // Programs busy (>= 30% of a core) within about a second of each frame hitch.
    let hitches: Vec<i64> = r.anomalies.iter().filter(|a| a.kind == Kind::FrameHitch && a.in_match && a.value >= 50.0).map(|a| a.t).collect();
    let mut hits: BTreeMap<String, usize> = BTreeMap::new();
    for &h in &hitches {
        let mut names: Vec<&str> = d.proc_cpu.iter().filter(|(t, _, p)| *t >= h - 200_000 && *t <= h + 1_200_000 && *p >= 30.0).map(|(_, n, _)| n.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for n in names {
            *hits.entry(n.to_string()).or_default() += 1;
        }
    }
    let mut hp: Vec<(String, usize)> = hits.into_iter().collect();
    hp.sort_by_key(|x| std::cmp::Reverse(x.1));
    hp.truncate(5);
    c.hitch_procs = hp.clone();
    // How often the same window around a random moment of play catches the program that
    // busy: a program busy much of the time is near many hitches by chance.
    let busy_share = |name: &str| {
        let busy: Vec<i64> = d.proc_cpu.iter().filter(|(t, n, p)| n == name && *p >= 30.0 && in_play(r, *t)).map(|x| x.0).collect();
        let moments: Vec<i64> = d.cpu.iter().step_by(4).filter(|x| in_play(r, x.t)).map(|x| x.t).collect();
        let hit = moments.iter().filter(|&&m| busy.iter().any(|&t| t >= m - 200_000 && t <= m + 1_200_000)).count();
        hit as f64 / moments.len().max(1) as f64
    };

    let busy: Vec<String> = avg.iter().filter(|x| x.1 >= 5.0).take(3).map(|(n, a, m)| format!("{} {a:.0}% (peak {m:.0}%)", short(n))).collect();
    if !busy.is_empty() {
        f.push(format!("Busiest other programs while playing (share of one CPU core): {}.", busy.join(", ")));
    }
    let mut blamed = false;
    if hitches.len() >= 3 {
        let share = |n: usize| n as f64 / hitches.len() as f64;
        if let Some((name, n)) = hp.iter().filter(|x| share(x.1) >= 0.3 && share(x.1) - busy_share(&x.0) >= 0.3).max_by(|a, b| (share(a.1) - busy_share(&a.0)).total_cmp(&(share(b.1) - busy_share(&b.0)))) {
            f.push(format!(
                "{n} of {} frame hitches came while {} was using a lot of CPU (it was that busy {:.0}% of the time otherwise).",
                hitches.len(),
                short(name),
                busy_share(name) * 100.0
            ));
            let mut advice = explain(name).map(String::from).unwrap_or_else(|| format!("{} was busy during the stutters: close it while playing and see if the stutter goes away.", short(name)));
            // Windows' memory manager runs as "System": with memory nearly full, that is the
            // likelier reason.
            if name == "System" && d.cpu.iter().filter_map(|x| x.commit_pct).fold(0.0, f64::max) >= 90.0 {
                advice.push_str(" Here RAM + page file were nearly full, which keeps Windows' memory management busy: freeing memory (see the Memory row) is the first thing to try.");
            }
            rec.push(advice);
            c.cpu.push(format!("{} busy during {n} hitches", short(name)));
            blamed = true;
        }
    }
    if !blamed {
        if let Some((name, a, _)) = avg.first().filter(|x| x.1 >= 50.0) {
            f.push(format!("{} used on average {a:.0}% of a CPU core while you played.", short(name)));
            if let Some(e) = explain(name) {
                rec.push(e.to_string());
            }
            c.cpu.push(format!("{} used {a:.0}% of a core", short(name)));
        }
    }
}

/// Time the CPU spent held back by power or temperature limits.
fn cpu_limits(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let rows: Vec<f64> = d.cpu.iter().filter(|x| in_play(r, x.t)).filter_map(|x| x.perf_limit).collect();
    if rows.len() < 40 {
        return;
    }
    let limited = rows.iter().filter(|&&p| p >= 15.0).count() as f64 / rows.len() as f64;
    if limited >= 0.2 {
        let typical = percentile(&{
            let mut v: Vec<f64> = rows.iter().copied().filter(|&p| p >= 15.0).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v
        }, 0.5);
        f.push(format!("The CPU was held back by power or heat limits {:.0}% of the time (typically {typical:.0}% below its full speed).", limited * 100.0));
        rec.push(if super::hardware::is_laptop(d) {
            "The CPU was throttled: keep the laptop plugged in, use the Best performance power mode, raise it for airflow and clean its vents."
        } else {
            "The CPU was throttled: check that the CPU cooler is seated and its fan spins up, and that the power plan is not Power saver."
        }
        .into());
        c.cpu.push(format!("held back {:.0}% of the time", limited * 100.0));
    }
}

fn memory(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let mut avail: Vec<f64> = d.cpu.iter().filter(|x| in_play(r, x.t)).filter_map(|x| x.avail_mb).collect();
    if avail.len() < 20 {
        return;
    }
    avail.sort_by(|a, b| a.total_cmp(b));
    let low = percentile(&avail, 0.02);
    let total = d.meta["ram_mb"].as_f64().unwrap_or(0.0);
    let paging = r.anomalies.iter().filter(|a| a.kind == Kind::Paging).count();
    let commit = d.cpu.iter().filter_map(|x| x.commit_pct).fold(0.0, f64::max);
    let of = if total > 0.0 { format!(" of {:.0} GB", total / 1024.0) } else { String::new() };
    let low_ram = low < 1024.0f64.max(total * 0.08) || paging >= 3;
    // The commit limit (RAM + page file) can run out while RAM still looks free.
    let commit_full = commit >= 90.0;
    f.push(format!(
        "Memory: lowest free RAM while playing {:.1} GB{of}{}{}.",
        low / 1024.0,
        if commit >= 80.0 { format!(", up to {commit:.0}% of the commit limit (RAM + page file) in use") } else { String::new() },
        if paging > 0 { format!(", {paging} moments of heavy paging to disk") } else { String::new() }
    ));
    if low_ram {
        rec.push(format!(
            "RAM ran low while playing: close browsers and other heavy programs before a match{}.",
            if total > 0.0 && total < 15_000.0 { "; 16 GB or more is recommended for current games" } else { "" }
        ));
    } else if commit_full {
        rec.push(format!(
            "Programs reserved {commit:.0}% of what Windows can hand out (RAM + page file), even though RAM itself was not full. When that limit is reached, Windows must grow the page file or programs crash. Close browsers and other heavy programs while playing, and keep the page file on \"System managed\" (Settings > System > About > Advanced system settings > Performance > Settings > Advanced > Virtual memory)."
        ));
    }
    let text = if commit_full && !low_ram { format!("{commit:.0}% of the commit limit in use (RAM + page file)") } else { format!("lowest free {:.1} GB{of}", low / 1024.0) };
    c.memory = Some((low_ram || commit_full, text));
}

/// Display refresh rate, power source and power plan.
fn setup(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    if let Some(ds) = d.meta["displays"].as_array() {
        for m in ds {
            let (hz, max) = (m["hz"].as_u64().unwrap_or(0), m["max_hz"].as_u64().unwrap_or(0));
            let name = m["monitor"].as_str().filter(|s| !s.is_empty() && !s.starts_with("Generic")).map(|s| format!(" ({s})")).unwrap_or_default();
            if max > hz + 4 && hz > 0 && m["primary"].as_bool() != Some(true) {
                // A second screen at a low rate is usually on purpose.
                f.push(format!("A second monitor{name} runs at {hz} Hz but supports {max} Hz."));
            } else if max > hz + 4 && hz > 0 {
                f.push(format!("A monitor{name} runs at {hz} Hz but supports {max} Hz."));
                rec.push(format!("Your monitor is set to {hz} Hz but can do {max} Hz: Settings > System > Display > Advanced display > Choose a refresh rate."));
                c.setup.push(format!("monitor at {hz} of {max} Hz"));
            } else if m["primary"].as_bool() == Some(true) && hz > 0 {
                c.setup_ok.push(format!("{hz} Hz"));
            }
        }
        if let Some(p) = ds.iter().find(|m| m["primary"].as_bool() == Some(true)) {
            let hz = p["hz"].as_f64().unwrap_or(0.0);
            f.push(format!("Main display: {}x{} at {hz:.0} Hz.", p["width"], p["height"]));
            if r.frames.frames > 100 && hz > 0.0 && r.frames.avg_fps > 0.0 && r.frames.avg_fps < hz * 0.8 {
                f.push(format!("The game averaged {:.0} fps, below the display's {hz:.0} Hz.", r.frames.avg_fps));
            }
        }
    }
    let power = &d.meta["power"];
    if power["on_battery"].as_bool() == Some(true) {
        rec.push("The laptop was on battery: plug it in for gaming, as most laptops slow the CPU and GPU down on battery.".into());
        c.setup.push("on battery".into());
    } else if power["has_battery"].as_bool() == Some(true) {
        c.setup_ok.push("plugged in".into());
    }
    if power["battery_saver"].as_bool() == Some(true) {
        rec.push("Battery saver was on: it throttles the PC. Turn it off while gaming.".into());
        c.setup.push("battery saver on".into());
    }
    let changes: Vec<&str> = d.events.iter().filter(|e| e.kind == "power").map(|e| e.detail.as_str()).collect();
    if !changes.is_empty() {
        f.push(format!("Power source changed during the session: {}.", changes.join("; ")));
    }
    if let Some(plan) = d.meta["power_plan"].as_str() {
        f.push(format!("Power plan: {plan}."));
        if plan.to_ascii_lowercase().contains("saver") {
            rec.push("The power plan is Power saver: switch to Balanced or High performance (Control Panel > Power Options).".into());
            c.setup.push("Power saver plan".into());
        }
    }
}

/// Wired link speed, drops and adapter errors; energy saving on Ethernet.
fn link(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let is_wifi = d.meta["route"]["is_wifi"].as_bool().unwrap_or(false);
    let drops = d.link.iter().skip(1).filter(|l| !l.up).count();
    if drops > 0 {
        f.push(format!("The network link went down {drops} time(s) during the session."));
        rec.push(if is_wifi { "The Wi-Fi connection dropped: see the Wi-Fi findings; move closer to the router or try another band." } else { "The Ethernet link dropped: replace the cable, try another port on the router, and check the cable's clips." }.into());
        c.link.push(format!("{drops} link drop(s)"));
    }
    if !is_wifi {
        if let Some(first) = d.link.first().filter(|l| l.up && l.tx_mbps > 1.0 && l.tx_mbps < 1000.0) {
            f.push(format!("The Ethernet link runs at {:.0} Mbit/s.", first.tx_mbps));
            if first.tx_mbps <= 100.0 {
                rec.push("The Ethernet link only runs at 100 Mbit/s: usually a damaged or old cable (or a 100 Mbit port). Try a Cat5e/Cat6 cable and another router port.".into());
                c.link.push("100 Mbit/s link".into());
            }
        }
    }
    if let Some(inner) = d.meta["inner_routers"].as_array().filter(|v| !v.is_empty()) {
        let ips: Vec<&str> = inner.iter().filter_map(|v| v.as_str()).collect();
        f.push(format!(
            "After your router, traffic passes another private address ({}): usually a second router in front of yours (double NAT). That does not cause lag by itself, but some games then report a strict NAT type.",
            ips.join(", ")
        ));
    }
    let errors: u64 = d.link.iter().skip(1).map(|l| l.errors).sum();
    let discards: u64 = d.link.iter().skip(1).map(|l| l.discards).sum();
    if errors + discards > 0 {
        f.push(format!("The network adapter reported {errors} errors and {discards} dropped packets during the session."));
        if errors >= 50 {
            rec.push("The network adapter counted many receive/send errors: update its driver; on Ethernet also replace the cable.".into());
            c.link.push(format!("{errors} adapter errors"));
        }
    }
    // Energy-saving Ethernet features can add delay on some adapters.
    if !is_wifi {
        let adapter = d.meta["route"]["adapter"].as_str().unwrap_or("");
        if !adapter.is_empty() {
            if let Ok(text) = std::fs::read_to_string(d.dir.join("sysinfo/system.txt")) {
                let on: Vec<String> = text
                    .lines()
                    .filter(|l| l.starts_with(adapter))
                    .filter(|l| ["Energy Efficient Ethernet", "Green Ethernet", "Advanced EEE", "Gigabit Lite", "Power Saving Mode"].iter().any(|k| l.contains(k)))
                    .filter(|l| l.trim_end().ends_with("Enabled") || l.trim_end().ends_with("On"))
                    .map(|l| l[adapter.len()..].trim().rsplit_once("  ").map(|(k, _)| k.trim().to_string()).unwrap_or_default())
                    .filter(|k| !k.is_empty())
                    .collect();
                if !on.is_empty() {
                    f.push(format!("Ethernet energy saving is on: {}.", on.join(", ")));
                    let gw_bad = r.probes.iter().any(|p| p.target == "gateway" && p.sent > 100 && (p.p99_ms > 10.0 || p.lost > 0));
                    if gw_bad {
                        rec.push(format!("Turn off {} for the Ethernet adapter (Device Manager > network adapter > Advanced): energy saving can delay packets.", on.join(" and ")));
                    }
                }
            }
        }
    }
}

/// Software known to affect games, listed for information.
fn software(d: &Data, f: &mut Vec<String>) {
    let Some(list) = d.meta["notable_processes"].as_array() else { return };
    let names: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
    let pick = |keys: &[&str]| -> Vec<&str> {
        let mut v: Vec<&str> = names.iter().copied().filter(|n| keys.iter().any(|k| n.to_ascii_lowercase().contains(k))).map(short).collect();
        v.dedup();
        v
    };
    let overlays = pick(&["obs64", "obs32", "medal", "overwolf", "outplayed", "rtss", "msiafterburner", "nvidia overlay", "nvidia share"]);
    let rgb = pick(&["icue", "armourycrate", "lghub", "synapse", "razer", "signalrgb", "dragon center", "lenovo vantage"]);
    let wallpaper = pick(&["wallpaper"]);
    let mut parts = Vec::new();
    if !overlays.is_empty() {
        parts.push(format!("overlays/recorders: {}", overlays.join(", ")));
    }
    if !rgb.is_empty() {
        parts.push(format!("device/RGB utilities: {}", rgb.join(", ")));
    }
    if !wallpaper.is_empty() {
        parts.push("Wallpaper Engine".to_string());
    }
    if !parts.is_empty() {
        f.push(format!("Also running: {}. Usually harmless, but worth closing to test if you get stutter.", parts.join("; ")));
    }
}

/// Share of play time the GPU's clocks were held back by heat or a power brake.
pub fn gpu_limit_share(d: &Data, r: &Analysis) -> f64 {
    let rows: Vec<&str> = d.gpu.iter().filter(|g| in_play(r, g.t)).map(|g| g.reasons.as_str()).collect();
    let limited = rows.iter().filter(|x| x.split('|').any(|k| matches!(k, "hw_slowdown" | "hw_thermal" | "sw_thermal" | "hw_power_brake"))).count();
    limited as f64 / rows.len().max(1) as f64
}

/// GPU clock limits (heat / power brake) that matter. Laptop GPUs run at their temperature
/// target by design, so for them only limits that line up with frame hitches count; a
/// desktop card at its limit for a fifth of the time or more is not cooled well enough.
pub fn gpu_limited(d: &Data, r: &Analysis) -> bool {
    let with_hitches = r.hitch_correlation.get("GPU limit / memory pressure").copied().unwrap_or(0.0) >= 0.3;
    with_hitches || (!super::hardware::is_laptop(d) && gpu_limit_share(d, r) >= 0.2)
}

/// One line per area: OK, needs a look, or not measured.
pub fn scorecard(d: &Data, r: &Analysis, c: &Checks) -> Vec<Health> {
    let is_wifi = d.meta["route"]["is_wifi"].as_bool().unwrap_or(false);
    let mut out = Vec::new();
    let mut add = |area: &str, warn: Option<bool>, text: String| {
        out.push(Health { area: area.into(), status: match warn { None => "na", Some(true) => "warn", Some(false) => "ok" }.into(), text });
    };
    let clockwork = r.headline.starts_with("Every ");

    // Link to the router.
    let area = if is_wifi { "Wi-Fi" } else { "Network link" };
    match r.probes.iter().find(|p| p.target == "gateway" && p.sent > 0) {
        Some(g) => {
            let loss = g.lost as f64 * 100.0 / g.sent as f64;
            let ups: Vec<f64> = r.anomalies.iter().filter(|a| a.kind == Kind::UploadStall && a.in_match).map(|a| a.value).collect();
            let bad = clockwork || g.p99_ms > 20.0 || loss > 0.5 || ups.len() >= 3 || !c.link.is_empty() || r.wifi_check.as_ref().is_some_and(|o| o.problem);
            let mut t = format!("router ping {:.0} ms, worst 1% {:.0} ms{}", g.p50_ms, g.p99_ms, if loss > 0.0 { format!(", {loss:.1}% lost") } else { String::new() });
            if !ups.is_empty() {
                t = format!("{} upload freezes of up to {:.0} ms; {t}", ups.len(), ups.iter().copied().fold(0.0, f64::max));
            }
            if clockwork {
                t = format!("freezes on a timer; {t}");
            }
            if !c.link.is_empty() {
                t = format!("{}; {t}", c.link.join(", "));
            }
            add(area, Some(bad), t);
        }
        None => add(area, None, "not measured".into()),
    }

    // Internet and the game's stream.
    let net = r.probes.iter().find(|p| p.target == "internet" && p.sent > 0);
    let s = &r.stream;
    let play_min = ((if s.match_s > 0.0 { s.match_s } else { s.active_s }) / 60.0).max(1.0);
    let stalls_pm = s.gaps_100ms as f64 / play_min;
    if net.is_some() || s.in_packets > 0 {
        let mut parts = Vec::new();
        let mut bad = false;
        // Losses and stalls caused by the first hop belong to the Wi-Fi / link line.
        let gw_loss = r.probes.iter().find(|p| p.target == "gateway" && p.sent > 0).map(|g| g.lost as f64 * 100.0 / g.sent as f64).unwrap_or(0.0);
        let first_hop = clockwork || r.gap_correlation.get(super::detect::GW_KEY).copied().unwrap_or(0.0) >= 0.4;
        if let Some(n) = net {
            let loss = n.lost as f64 * 100.0 / n.sent as f64;
            bad |= n.p99_ms > 100.0 || loss - gw_loss > 1.0;
            parts.push(format!("ping {:.0} ms{}", n.p50_ms, if loss - gw_loss > 0.2 { format!(", {:.1}% lost beyond the router", loss - gw_loss) } else { String::new() }));
        }
        if s.in_packets > 0 {
            let stalling = s.gaps_100ms >= 3 && stalls_pm >= 0.3;
            if stalling && first_hop {
                parts.push(format!("game stalls ({stalls_pm:.1}/min) come from the {}", if is_wifi { "Wi-Fi" } else { "link to the router" }));
            } else {
                bad |= stalling;
                parts.push(if s.gaps_100ms == 0 { "no game stalls".to_string() } else { format!("{stalls_pm:.1} game stalls/min") });
            }
        }
        add("Internet & game server", Some(bad), parts.join(", "));
    } else {
        add("Internet & game server", None, "not measured".into());
    }

    // Frames.
    let fr = &r.frames;
    if fr.frames > 100 {
        let hpm = r.anomalies.iter().filter(|a| a.kind == Kind::FrameHitch && a.in_match && a.value >= 50.0).count() as f64 / play_min;
        add("Frames", Some(hpm >= 1.0), format!("{:.0} fps, 1% low {:.0}, {hpm:.1} hitches/min", fr.avg_fps, fr.low1_fps));
    } else {
        add("Frames", None, "no game frames recorded".into());
    }

    // CPU.
    if d.cpu.is_empty() {
        add("CPU", None, "not measured".into());
    } else if !c.cpu.is_empty() {
        add("CPU", Some(true), c.cpu.join("; "));
    } else {
        let game = {
            let v: Vec<f64> = d.cpu.iter().filter(|x| in_play(r, x.t)).filter_map(|x| x.game_cpu).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        };
        let mut t = Vec::new();
        if let Some(g) = game {
            t.push(format!("game {g:.0}% of CPU"));
        }
        if d.cpu.iter().any(|x| x.perf_limit.is_some()) {
            t.push("no throttling".to_string());
        }
        match c.background.first().filter(|b| b.1 >= 5.0) {
            Some(b) => t.push(format!("{} {:.0}% of a core", short(&b.0), b.1)),
            None if t.is_empty() => t.push("no problems found".to_string()),
            None => {}
        }
        add("CPU", Some(false), t.join(", "));
    }

    // GPU.
    if d.gpu.is_empty() {
        // Other vendors: only how busy it was, from Windows' own counters.
        let mut busy: Vec<f64> = d.gpu_engines.iter().filter(|x| in_play(r, x.0)).map(|x| if x.2 > 0.0 { x.2 } else { x.1 }).collect();
        busy.sort_by(|a, b| a.total_cmp(b));
        if busy.len() >= 10 {
            add("GPU", Some(false), format!("{:.0}% busy (temperature and clock limits are only read on NVIDIA cards)", percentile(&busy, 0.5)));
        } else {
            add("GPU", None, "not measured".into());
        }
    } else {
        let share = gpu_limit_share(d, r);
        let temp = d.gpu.iter().filter_map(|g| g.temp).fold(0.0, f64::max);
        let t = if share >= 0.01 {
            format!("at its heat / power limit {:.0}% of the time{}, up to {temp:.0} °C", share * 100.0, if super::hardware::is_laptop(d) && !gpu_limited(d, r) { " (normal for a laptop)" } else { "" })
        } else {
            format!("no slowdowns, up to {temp:.0} °C")
        };
        add("GPU", Some(gpu_limited(d, r)), t);
    }

    match &c.memory {
        Some((bad, t)) => add("Memory", Some(*bad), t.clone()),
        None => add("Memory", None, "not measured".into()),
    }
    match &c.disk {
        Some((bad, t)) => add("Disk", Some(*bad), t.clone()),
        None => add("Disk", None, "not traced".into()),
    }
    match &c.drivers {
        Some((bad, t)) => add("Drivers", Some(*bad), t.clone()),
        None => add("Drivers", None, "not traced".into()),
    }
    if !c.setup.is_empty() {
        add("Display & power", Some(true), c.setup.join(", "));
    } else if !c.setup_ok.is_empty() {
        add("Display & power", Some(false), c.setup_ok.join(", "));
    } else {
        add("Display & power", None, "not measured".into());
    }
    if !c.hardware.is_empty() {
        add("Hardware setup", Some(true), c.hardware.join(", "));
    } else if !c.hardware_ok.is_empty() {
        add("Hardware setup", Some(false), c.hardware_ok.join(", "));
    } else {
        add("Hardware setup", None, "not measured".into());
    }
    out
}

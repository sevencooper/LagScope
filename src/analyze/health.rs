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
    Some(if has(&["msmpeng", "mpdefendercoreservice", "nissrv"]) {
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
    avg.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
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

    let busy: Vec<String> = avg.iter().filter(|x| x.1 >= 5.0).take(3).map(|(n, a, m)| format!("{} {a:.0}% (peak {m:.0}%)", short(n))).collect();
    if !busy.is_empty() {
        f.push(format!("Busiest other programs while playing (share of one CPU core): {}.", busy.join(", ")));
    }
    let mut blamed = false;
    if hitches.len() >= 3 {
        if let Some((name, n)) = hp.first().filter(|x| x.1 * 10 >= hitches.len() * 3) {
            f.push(format!("{n} of {} frame hitches came while {} was using a lot of CPU.", hitches.len(), short(name)));
            rec.push(explain(name).map(String::from).unwrap_or_else(|| format!("{} was busy during the stutters: close it while playing and see if the stutter goes away.", short(name))));
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
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v
        }, 0.5);
        f.push(format!("The CPU was held back by power or heat limits {:.0}% of the time (typically {typical:.0}% below its full speed).", limited * 100.0));
        let laptop = d.meta["power"]["has_battery"].as_bool() == Some(true);
        rec.push(if laptop {
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
    avail.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let low = percentile(&avail, 0.02);
    let total = d.meta["ram_mb"].as_f64().unwrap_or(0.0);
    let paging = r.anomalies.iter().filter(|a| a.kind == Kind::Paging).count();
    let commit = d.cpu.iter().filter_map(|x| x.commit_pct).fold(0.0, f64::max);
    let of = if total > 0.0 { format!(" of {:.0} GB", total / 1024.0) } else { String::new() };
    let tight = low < 1024.0f64.max(total * 0.08) || paging >= 3 || commit >= 90.0;
    f.push(format!(
        "Memory: lowest free RAM while playing {:.1} GB{of}{}{}.",
        low / 1024.0,
        if commit >= 80.0 { format!(", up to {commit:.0}% of the commit limit in use") } else { String::new() },
        if paging > 0 { format!(", {paging} moments of heavy paging to disk") } else { String::new() }
    ));
    if tight {
        rec.push(format!(
            "RAM ran low while playing: close browsers and other heavy programs before a match{}.",
            if total > 0.0 && total < 15_000.0 { "; 16 GB or more is recommended for current games" } else { "" }
        ));
    }
    c.memory = Some((tight, format!("lowest free {:.1} GB{of}", low / 1024.0)));
}

/// Display refresh rate, power source and power plan.
fn setup(d: &Data, r: &Analysis, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    if let Some(ds) = d.meta["displays"].as_array() {
        for m in ds {
            let (hz, max) = (m["hz"].as_u64().unwrap_or(0), m["max_hz"].as_u64().unwrap_or(0));
            let name = m["monitor"].as_str().filter(|s| !s.is_empty() && !s.starts_with("Generic")).map(|s| format!(" ({s})")).unwrap_or_default();
            if max > hz + 4 && hz > 0 {
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
            let bad = clockwork || g.p99_ms > 20.0 || loss > 0.5 || !c.link.is_empty() || r.wifi_check.as_ref().is_some_and(|o| o.problem);
            let mut t = format!("router ping {:.0} ms, worst 1% {:.0} ms{}", g.p50_ms, g.p99_ms, if loss > 0.0 { format!(", {loss:.1}% lost") } else { String::new() });
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
        let first_hop = clockwork || r.gap_correlation.get("gateway (Wi-Fi hop) spike/loss").copied().unwrap_or(0.0) >= 0.4;
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
        add("GPU", None, "no NVIDIA data".into());
    } else {
        let limits = r.anomalies.iter().filter(|a| a.kind == Kind::GpuLimit).count();
        let temp = d.gpu.iter().filter_map(|g| g.temp).fold(0.0, f64::max);
        add("GPU", Some(limits > 0), if limits > 0 { format!("slowed down by heat/power {limits}×, up to {temp:.0} °C") } else { format!("no slowdowns, up to {temp:.0} °C") });
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

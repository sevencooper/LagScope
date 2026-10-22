//! Hardware set up wrong: RAM below its rated speed, mixed or single sticks, the monitor on
//! the motherboard's output, the graphics card on a slow PCIe link or with a lowered power
//! limit, a full or spinning system drive, and a few Windows settings worth knowing about.
//! Everything comes from the one-time snapshot (meta "hardware", "displays", "nvidia") and
//! gpu.csv; nothing here costs anything while recording.

use serde_json::Value;

use super::health::Checks;
use super::load::Data;

pub fn findings(d: &Data, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let hw = &d.meta["hardware"];
    let laptop = d.meta["power"]["has_battery"].as_bool() == Some(true) || hw["pc_type"].as_i64() == Some(2);
    if hw.is_object() {
        memory(hw, laptop, f, rec, c);
        drives(hw, f, rec, c);
        software_age(d, hw, f, rec);
        if hw["background_recording"].as_i64() == Some(1) {
            f.push("Xbox Game Bar records in the background (\"Record what happened\").".into());
            rec.push("Xbox Game Bar keeps recording in the background, which costs some performance: turn it off in Settings > Gaming > Captures if you don't use it.".into());
        }
        if hw["memory_integrity"].as_bool() == Some(true) {
            f.push("Memory integrity (Core isolation) is on: a security feature that costs a few percent of performance in some games.".into());
        }
    }
    display_output(d, hw, laptop, rec, c);
    if !laptop {
        gpu_link(d, f, rec, c);
    }
    cpu_cap(d, f, rec, c);
}

/// DDR generation from SMBIOS memory type.
fn ddr(t: i64) -> &'static str {
    match t {
        24 => "DDR3",
        26 => "DDR4",
        34 => "DDR5",
        30 => "LPDDR4",
        35 => "LPDDR5",
        _ => "",
    }
}

/// Rated speed (MT/s) from the part number, for brands that put it there:
/// G.Skill F5-6000J3636F32G, Corsair CMK32GX5M2B6000C36, Kingston KF560C36BBEK2-32,
/// Team TF8D432G6000HC38ACD, Crucial BL2K8G32C16U4B / CP2K16G56C46U5.
pub fn rated_speed(part: &str) -> Option<u32> {
    let p = part.trim().to_ascii_uppercase();
    let num = |s: &str, n: usize| -> Option<u32> { s.get(..n).filter(|x| x.bytes().all(|b| b.is_ascii_digit())).and_then(|x| x.parse().ok()) };
    let v = if p.starts_with("F4-") || p.starts_with("F5-") {
        num(&p[3..], 4)
    } else if p.starts_with("CM") {
        let i = p.find("X4M").or_else(|| p.find("X5M"))?;
        // X5M + kit size digit + revision letter + speed.
        num(p.get(i + 5..)?, 4)
    } else if p.starts_with("KF") || p.starts_with("HX") {
        num(p.get(3..)?, 2).map(|x| x * 100)
    } else if p.starts_with("KVR") {
        num(&p[3..], 2).map(|x| x * 100)
    } else if p.starts_with("BL") || p.starts_with("CP") {
        let i = p.find('G')?;
        num(p.get(i + 1..)?, 2).filter(|_| p.get(i + 3..i + 4) == Some("C")).map(|x| x * 100)
    } else {
        let i = p.find("HC")?;
        num(p.get(i.checked_sub(4)?..)?, 4)
    };
    v.filter(|s| (1600..=10_000).contains(s))
}

/// Memory channel from a slot name such as DIMM_A2, DIMMB1 or DDR5_A2.
fn channel(slot: &str) -> Option<char> {
    let s = slot.trim().to_ascii_uppercase();
    let mut rev = s.chars().rev();
    let (digit, letter) = (rev.next()?, rev.next()?);
    let before: String = rev.collect::<Vec<_>>().into_iter().rev().collect();
    (digit.is_ascii_digit() && ('A'..='D').contains(&letter) && (before.is_empty() || before.ends_with("DIMM") || before.ends_with(['_', '-', ' '])))
        .then_some(letter)
}

fn memory(hw: &Value, laptop: bool, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let Some(sticks) = hw["memory"].as_array() else { return };
    let sticks: Vec<&Value> = sticks.iter().filter(|m| m["gb"].as_f64().unwrap_or(0.0) > 0.0).collect();
    if sticks.is_empty() {
        return;
    }
    let gb = |m: &Value| m["gb"].as_f64().unwrap_or(0.0);
    let part = |m: &Value| m["part"].as_str().unwrap_or("").trim().to_string();
    let speed = sticks.iter().filter_map(|m| m["configured"].as_u64()).filter(|&s| s > 0).min().unwrap_or(0) as u32;
    let kind = ddr(sticks[0]["type"].as_i64().unwrap_or(0));
    let n = sticks.len();
    let sizes: Vec<f64> = sticks.iter().map(|m| gb(m)).collect();
    let same_size = sizes.iter().all(|&s| s == sizes[0]);
    let summary = format!(
        "{}{} GB {kind}{}",
        if same_size { format!("{n}×") } else { String::new() },
        if same_size { format!("{:.0}", sizes[0]) } else { sizes.iter().map(|s| format!("{s:.0}")).collect::<Vec<_>>().join("+") },
        if speed > 0 { format!("{}{speed}", if kind.is_empty() { " at " } else { "-" }) } else { String::new() }
    );
    let mut parts: Vec<String> = sticks.iter().map(|m| part(m)).filter(|p| !p.is_empty()).collect();
    let mut slots: Vec<&str> = sticks.iter().filter_map(|m| m["slot"].as_str()).collect();
    parts.sort();
    slots.dedup();
    let mut grouped: Vec<String> = Vec::new();
    for p in parts.iter() {
        if !grouped.iter().any(|g| g.ends_with(p.as_str())) {
            grouped.push(format!("{}× {p}", parts.iter().filter(|x| *x == p).count()));
        }
    }
    f.push(format!("RAM: {summary} ({}; slots {}).", grouped.join(", "), slots.join(", ")));
    let mut bad = Vec::new();

    // Below the speed the sticks are sold for: XMP / EXPO is off.
    let rated = sticks.iter().filter_map(|m| rated_speed(&part(m))).min();
    if let Some(rt) = rated.filter(|&rt| speed > 0 && rt >= speed + 200) {
        let four_ddr5 = n >= 4 && kind == "DDR5";
        rec.push(format!(
            "Your RAM runs at {speed} MT/s but is rated for {rt}: turn on XMP (Intel) or EXPO (AMD) in the BIOS.{}",
            if four_ddr5 { " With four DDR5 sticks the full rated speed is often not stable, so pick the next lower speed if it crashes." } else { "" }
        ));
        bad.push(format!("RAM at {speed} of {rt} MT/s"));
    } else if rated.is_none() && !laptop && ((kind == "DDR4" && speed > 0 && speed <= 2666) || (kind == "DDR5" && speed > 0 && speed <= 4800)) {
        f.push(format!("RAM runs at {speed} MT/s, the {kind} default. If your sticks are rated faster (see their label), turn on XMP/EXPO in the BIOS."));
    }

    // Different kinds of sticks mixed together.
    let mut kinds: Vec<(String, u64)> = sticks.iter().map(|m| (part(m), gb(m) as u64)).collect();
    kinds.sort();
    kinds.dedup();
    if kinds.len() > 1 {
        rec.push(format!(
            "Your RAM sticks are not a matched set ({}): mixed sticks often have to run slower and can be unstable. A matched kit works best.",
            kinds.iter().map(|(p, g)| if p.is_empty() { format!("{g} GB") } else { format!("{g} GB {p}") }).collect::<Vec<_>>().join(" + ")
        ));
        bad.push("mixed RAM sticks".into());
    }

    // Channels: laptops often have soldered RAM that reports oddly, so desktops only.
    if !laptop {
        if n == 1 {
            rec.push("Only one RAM stick is installed, so memory runs in single-channel mode at half the speed: add a matching second stick (in the slots your motherboard manual names for two sticks, usually A2 and B2).".into());
            bad.push("one RAM stick (single channel)".into());
        } else if n == 3 {
            rec.push("Three RAM sticks are installed, so part of the memory runs single-channel: use two or four matching sticks.".into());
            bad.push("three RAM sticks".into());
        } else if n == 2 {
            let ch: Vec<Option<char>> = sticks.iter().map(|m| channel(m["slot"].as_str().unwrap_or(""))).collect();
            if let (Some(a), Some(b)) = (ch[0], ch[1]) {
                if a == b {
                    rec.push(format!("Both RAM sticks are in channel {a}, so memory runs single-channel: move one to the other channel (the motherboard manual names the slots for two sticks, usually A2 and B2)."));
                    bad.push("both RAM sticks in one channel".into());
                }
            }
        }
    }
    if bad.is_empty() {
        c.hardware_ok.push(summary);
    }
    c.hardware.extend(bad);
}

fn drives(hw: &Value, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let sys = &hw["system_drive"];
    let letter = sys["letter"].as_str().unwrap_or("C");
    if let (Some(free), Some(size)) = (sys["free_gb"].as_f64(), sys["size_gb"].as_f64()) {
        if size > 0.0 && (free < 10.0 || (free < size * 0.1 && free < 50.0)) {
            rec.push(format!("The system drive {letter}: has only {free:.0} GB free: Windows and games stutter when it is nearly full. Free up space (at least 10-15%)."));
            c.hardware.push(format!("{letter}: almost full ({free:.0} GB free)"));
        }
    }
    let Some(disks) = hw["disks"].as_array() else { return };
    for dk in disks.iter().filter(|d| d["media"].as_str() == Some("HDD")) {
        let letters: Vec<&str> = dk["letters"].as_array().map(|l| l.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let on = if letters.is_empty() { String::new() } else { format!(" ({})", letters.iter().map(|l| format!("{l}:")).collect::<Vec<_>>().join(" ")) };
        if letters.contains(&letter) {
            rec.push("Windows runs from a hard disk: moving to an SSD is the biggest single fix for stutter and slow loading.".into());
            c.hardware.push("Windows on a hard disk".into());
        } else {
            f.push(format!("A hard disk is installed{on}. Games on it load slowly and can stutter while streaming data; keep games on the SSD."));
        }
    }
}

/// Days since 1970 for a "YYYY-MM-DD..." string.
fn days(s: &str) -> Option<i64> {
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    Some(era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468)
}

fn software_age(d: &Data, hw: &Value, f: &mut Vec<String>, rec: &mut Vec<String>) {
    let Some(today) = d.meta["start_local"].as_str().and_then(days) else { return };
    let gpus: Vec<&Value> = hw["gpus"].as_array().map(|g| g.iter().collect()).unwrap_or_default();
    // The card that renders games: the discrete one if there is one.
    let main = gpus.iter().find(|g| discrete(g["name"].as_str().unwrap_or(""))).or(gpus.first());
    if let Some(g) = main {
        let date = g["driver_date"].as_str().unwrap_or("");
        if days(date).is_some_and(|x| today - x > 365) {
            f.push(format!("The graphics driver ({}) is from {date}.", g["name"].as_str().unwrap_or("")));
            rec.push(format!("Your graphics driver is over a year old ({}): updating it often fixes stutter in newer games.", &date[..7]));
        }
    }
    if let Some(date) = hw["bios_date"].as_str().filter(|b| days(b).is_some_and(|x| today - x > 3 * 365)) {
        f.push(format!("The BIOS is from {date}; a BIOS update from the motherboard maker can fix stability and performance problems."));
    }
}

fn integrated(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.contains("intel") && (n.contains("uhd") || n.contains("iris") || n.contains("hd graphics") || n.ends_with("arc(tm) graphics")))
        || (n.contains("radeon") && !n.contains(" rx") && !n.contains("pro w"))
}

fn discrete(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("nvidia") || (n.contains("radeon") && n.contains(" rx")) || n.contains("arc(tm) a") || n.contains("arc(tm) b") || n.contains("arc a") || n.contains("arc b")
}

/// The main monitor plugged into the motherboard (integrated graphics) while a graphics card is installed.
fn display_output(d: &Data, hw: &Value, laptop: bool, rec: &mut Vec<String>, c: &mut Checks) {
    if laptop {
        return;
    }
    let Some(primary) = d.meta["displays"].as_array().and_then(|ds| ds.iter().find(|m| m["primary"].as_bool() == Some(true))) else { return };
    let adapter = primary["adapter"].as_str().unwrap_or("");
    let mut names: Vec<String> = hw["gpus"].as_array().map(|g| g.iter().filter_map(|x| x["name"].as_str().map(String::from)).collect()).unwrap_or_default();
    names.extend(d.meta["nvidia"]["devices"].as_array().into_iter().flatten().filter_map(|x| x["name"].as_str().map(String::from)));
    if let Some(card) = names.iter().find(|n| discrete(n)).filter(|_| integrated(adapter)) {
        rec.push(format!("Your main monitor is plugged into the motherboard ({adapter}), not the graphics card ({card}): games then run slower or are copied across. Plug the cable into the graphics card's ports, lower on the back of the PC."));
        c.hardware.push("monitor plugged into the motherboard".into());
    }
}

/// The graphics card's PCIe link under load, and a lowered power limit.
fn gpu_link(d: &Data, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let dev = &d.meta["nvidia"]["devices"][0];
    if dev.is_null() {
        return;
    }
    let name = dev["name"].as_str().unwrap_or("the graphics card");
    // The link slows to gen 1 when idle, so only look at busy moments.
    let busy: Vec<(f64, f64)> = d.gpu.iter().filter(|g| g.util.unwrap_or(0.0) >= 60.0).filter_map(|g| Some((g.pcie_gen?, g.pcie_width?))).collect();
    if busy.len() >= 10 {
        let lg = busy.iter().map(|x| x.0).fold(0.0, f64::max);
        let width = busy.iter().map(|x| x.1).fold(0.0, f64::max);
        let max_gen = dev["max_pcie_gen"].as_f64().unwrap_or(lg);
        let max_width = dev["max_pcie_width"].as_f64().unwrap_or(width);
        let link = format!("PCIe {lg:.0}.0 x{width:.0}");
        f.push(format!("Graphics card link under load: {link} (card and slot support {max_gen:.0}.0 x{max_width:.0})."));
        if width < max_width || width <= 4.0 {
            rec.push(format!("The graphics card runs with only {width:.0} PCIe lanes{}: make sure it sits in the top long slot (the one nearest the CPU) and is pushed in fully; on some boards an M.2 drive in a shared slot halves the lanes.", if width < max_width { format!(" instead of {max_width:.0}") } else { String::new() }));
            c.hardware.push(format!("graphics card at x{width:.0}"));
        } else if lg < max_gen {
            rec.push(format!("The graphics card link runs at PCIe {lg:.0}.0 instead of {max_gen:.0}.0: if it is on a riser cable, the riser may be limiting it; otherwise set the slot's PCIe mode to Auto in the BIOS and reseat the card."));
            c.hardware.push(format!("graphics card at PCIe {lg:.0}.0"));
        } else {
            c.hardware_ok.push(format!("GPU {link}"));
        }
    }
    let default = dev["default_power_limit_w"].as_f64().unwrap_or(0.0);
    let limit = d.gpu.iter().filter_map(|g| g.limit).fold(0.0, f64::max);
    if default > 0.0 && limit > 0.0 && limit < default * 0.9 {
        rec.push(format!("{name}'s power limit is lowered ({limit:.0} of {default:.0} W): fine if you did that on purpose (e.g. in MSI Afterburner), otherwise reset it there to get full performance."));
        c.hardware.push(format!("GPU power limit {:.0}%", limit / default * 100.0));
    }
}

/// "Maximum processor state" below 100% in the power plan.
fn cpu_cap(d: &Data, f: &mut Vec<String>, rec: &mut Vec<String>, c: &mut Checks) {
    let Ok(text) = std::fs::read_to_string(d.dir.join("sysinfo/powercfg_processor.txt")) else { return };
    let Some(block) = text.split("bc5038f7-23e0-4960-96da-33abaf5935ec").nth(1).map(|b| b.split("GUID:").next().unwrap_or(b)) else { return };
    // Minimum, maximum and increment come first, then the AC and DC values.
    let vals: Vec<u32> = block.split_whitespace().filter_map(|w| w.strip_prefix("0x")).filter_map(|h| u32::from_str_radix(h, 16).ok()).collect();
    if vals.len() < 5 {
        return;
    }
    let on_battery = d.meta["power"]["on_battery"].as_bool() == Some(true);
    let cap = if on_battery { vals[vals.len() - 1] } else { vals[vals.len() - 2] };
    if cap < 100 {
        f.push(format!("The power plan limits the CPU to {cap}% (Maximum processor state)."));
        rec.push(format!("The power plan caps the CPU at {cap}% of its speed: set Control Panel > Power Options > Change plan settings > Change advanced power settings > Processor power management > Maximum processor state back to 100%."));
        c.setup.push(format!("CPU capped at {cap}%"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_numbers() {
        assert_eq!(rated_speed("F5-6000J3636F32G"), Some(6000));
        assert_eq!(rated_speed("F4-3200C16-8GVKB"), Some(3200));
        assert_eq!(rated_speed("CMK32GX5M2B6000C36"), Some(6000));
        assert_eq!(rated_speed("CMK16GX4M2B3200C16"), Some(3200));
        assert_eq!(rated_speed("KF560C36-32"), Some(6000));
        assert_eq!(rated_speed("KF432C16BBK2/32"), Some(3200));
        assert_eq!(rated_speed("TF8D432G6000HC38ACD"), Some(6000));
        assert_eq!(rated_speed("BL2K8G32C16U4B"), Some(3200));
        assert_eq!(rated_speed("CP2K16G56C46U5"), Some(5600));
        assert_eq!(rated_speed("M425R1GB4BB0-CQKOL"), None);
        assert_eq!(rated_speed(""), None);
    }

    #[test]
    fn slots() {
        assert_eq!(channel("DIMM_A2"), Some('A'));
        assert_eq!(channel("DIMMB2"), Some('B'));
        assert_eq!(channel("DDR5_A1"), Some('A'));
        assert_eq!(channel("ChannelA-DIMM0"), None);
        assert_eq!(channel("DIMM 1"), None);
        assert_eq!(channel("P0 CHANNEL A"), None);
    }

    #[test]
    fn gpu_names() {
        assert!(integrated("AMD Radeon(TM) Graphics"));
        assert!(integrated("Intel(R) UHD Graphics 770"));
        assert!(!integrated("AMD Radeon RX 7800 XT"));
        assert!(discrete("NVIDIA GeForce RTX 4080"));
        assert!(discrete("AMD Radeon RX 7800 XT"));
        assert!(!discrete("AMD Radeon(TM) Graphics"));
    }

    #[test]
    fn day_numbers() {
        assert_eq!(days("1970-01-01"), Some(0));
        assert_eq!(days("2026-10-22 18:00:00").unwrap() - days("2025-10-22").unwrap(), 365);
    }
}

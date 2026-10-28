//! Text for sharing a recording: a short plain-text summary (for a chat or forum post), a
//! prompt for an AI assistant, and `AI_GUIDE.md` (written into each recording, so the zip
//! explains itself to whoever reads it). Works from meta.json and analysis.json alone, so
//! older recordings can be shared the same way.

use std::path::Path;

use serde_json::Value;

/// The data dictionary, also in the repository as docs/SESSION_FORMAT.md.
const FORMAT: &str = include_str!("../../docs/SESSION_FORMAT.md");

fn read_json(dir: &Path, name: &str) -> Value {
    std::fs::read_to_string(dir.join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

fn mmss(us: f64) -> String {
    let s = (us / 1e6).max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn game(meta: &Value) -> String {
    let detected: Vec<&str> = meta["detected_games"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    let configured: Vec<&str> = meta["config"]["processes"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    match meta["profile"]["name"].as_str() {
        Some(n) if !n.is_empty() && !n.starts_with("Auto") => n.to_string(),
        _ if !detected.is_empty() => detected.join(", "),
        // Older recordings name the game only by its process.
        _ if !configured.is_empty() => configured.join(", "),
        _ => "no game detected".to_string(),
    }
}

/// One line about the session: version, game, start, length, connection.
fn header(meta: &Value) -> String {
    let start = meta["start_local"].as_str().map(|s| s.get(..16).unwrap_or(s)).unwrap_or("?");
    let mins = meta["duration_s"].as_f64().map(|s| format!("{:.0} min", (s / 60.0).max(1.0))).unwrap_or_else(|| "? min".into());
    let link = match meta["route"]["is_wifi"].as_bool() {
        Some(true) => "Wi-Fi",
        Some(false) => "wired",
        None => "connection unknown",
    };
    format!("LagScope {} · {} · {start} · {mins} · {link}", meta["tool_version"].as_str().unwrap_or("?"), game(meta))
}

fn marks(an: &Value) -> Vec<String> {
    an["verdicts"].as_array().into_iter().flatten().filter_map(|v| v["t"].as_f64()).map(mmss).collect()
}

/// Plain text for a chat or forum post: verdict, scorecard, top suggestions.
pub fn summary_text(dir: &Path) -> Option<String> {
    let (meta, an) = (read_json(dir, "meta.json"), read_json(dir, "analysis.json"));
    let headline = an["headline"].as_str()?;
    let mut t = format!("{}\nVerdict: {headline}\n", header(&meta));
    for h in an["health"].as_array().into_iter().flatten() {
        let mark = match h["status"].as_str() {
            Some("warn") => "⚠",
            Some("ok") => "✓",
            _ => "–",
        };
        t.push_str(&format!("{mark} {}: {}\n", h["area"].as_str().unwrap_or(""), h["text"].as_str().unwrap_or("")));
    }
    let good: Vec<String> = an["good_for"].as_array().into_iter().flatten().map(|g| format!("{} {}", g["name"].as_str().unwrap_or(""), g["grade_text"].as_str().unwrap_or(""))).collect();
    if !good.is_empty() {
        t.push_str(&format!("Good for: {}\n", good.join(", ")));
    }
    let recs: Vec<&str> = an["recommendations"].as_array().into_iter().flatten().filter_map(|r| r.as_str()).take(3).collect();
    if !recs.is_empty() {
        t.push_str("What to try (not guaranteed to help):\n");
        for (i, r) in recs.iter().enumerate() {
            t.push_str(&format!("{}. {r}\n", i + 1));
        }
    }
    Some(t)
}

/// A prompt to paste into an AI assistant together with the recording's zip.
pub fn ai_prompt(dir: &Path) -> Option<String> {
    let (meta, an) = (read_json(dir, "meta.json"), read_json(dir, "analysis.json"));
    let headline = an["headline"].as_str()?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let look: Vec<String> = an["health"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|h| h["status"] == "warn")
        .map(|h| format!("{} ({})", h["area"].as_str().unwrap_or(""), h["text"].as_str().unwrap_or("")))
        .collect();
    let m = marks(&an);
    Some(format!(
        "I recorded a gaming session with LagScope, a Windows tool that records network, frame, CPU/GPU, driver and disk timing while you play, and attached the recording ({name}.zip). Please analyse it.\n\
\n\
How to read it: unzip it and read AI_GUIDE.md first; it explains every file and column. Start with meta.json, analysis.json, events.csv and marks.csv, then use code (e.g. Python with pandas) for the large CSV files. If you cannot open zip files or run code, tell me and I will send the small files instead.\n\
\n\
The session: {}\n\
LagScope's verdict: {headline}\n\
{}\
{}\
\n\
What I noticed: [describe the lag you felt - when, what it looked like (rubber-banding, stutter, delayed hits, freezes), and any warning icons the game showed]\n\
\n\
Please:\n\
1. Check whether LagScope's verdict holds up, and look for anything it missed.\n\
2. {}\n\
3. Tell me the most likely cause and the next concrete steps, in order, and how sure you are about each.\n",
        header(&meta),
        if look.is_empty() { String::new() } else { format!("Areas it flagged: {}\n", look.join("; ")) },
        if m.is_empty() { "I did not mark any lag spikes.\n".to_string() } else { format!("I pressed the lag-spike key at {} (minutes:seconds into the recording).\n", m.join(", ")) },
        if m.is_empty() {
            "Look for moments where the game's packets stalled, frames hitched or pings spiked, and what else happened at the same time."
        } else {
            "For each marked moment, look at the 5 seconds before it across network, frames, CPU/GPU, drivers and disk, and say what happened."
        }
    ))
}

/// Writes AI_GUIDE.md: this session in brief, how to approach it, then the data dictionary.
pub fn write_ai_guide(dir: &Path) -> std::io::Result<()> {
    let (meta, an) = (read_json(dir, "meta.json"), read_json(dir, "analysis.json"));
    let m = marks(&an);
    let intro = format!(
        "# Guide for AI assistants\n\n\
This folder is one LagScope recording. The user wants to know where their lag comes from: the Wi-Fi or cable to the router, the internet / ISP, the game server, or the PC itself (frames, CPU, GPU, drivers, disk).\n\n\
**This session:** {}\n\n\
**LagScope's verdict:** {}\n\n\
**Lag spikes the user marked:** {}\n\n\
## How to approach it\n\n\
1. Read `meta.json`, `analysis.json`, `events.csv` and `marks.csv` (small). LagScope's own conclusions are in `analysis.json` (`headline`, `health`, `findings`, `verdicts`); treat them as a starting point to check, not as ground truth.\n\
2. Load the large CSVs with code. All files share one clock (`t_us`, microseconds since start), so join them on time windows, e.g. the 5 s before each mark.\n\
3. Typical questions: Did the game's inbound packets stall (`game_packets.csv`, received UDP from the server endpoint), and did the router ping (`probes.csv`, target `gateway`) spike at the same moment (local link) or only further out (ISP / route / server)? Did frames hitch (`frames.csv`) while the network was clean (PC side)? What did drivers, disk, CPU and other programs do at those times?\n\
4. Repeating patterns matter: events on a fixed interval point to something on a timer (router features, background scans, scheduled software).\n\n",
        header(&meta),
        an["headline"].as_str().unwrap_or("(not analysed)"),
        if m.is_empty() { "none".to_string() } else { format!("{} (minutes:seconds; `marks.csv` has exact times)", m.join(", ")) },
    );
    // The repository copy starts with its own title and intro; keep from the first section.
    let body = FORMAT.find("## Ground rules").map(|i| &FORMAT[i..]).unwrap_or(FORMAT);
    std::fs::write(dir.join("AI_GUIDE.md"), format!("{intro}{body}"))
}

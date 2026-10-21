mod analyze;
// Capture-side modules are only exercised by the Windows collectors.
#[cfg_attr(not(windows), allow(dead_code))]
mod app;
#[cfg_attr(not(windows), allow(dead_code))]
mod clock;
#[cfg_attr(not(windows), allow(dead_code))]
mod session;
#[cfg_attr(not(windows), allow(dead_code))]
mod ui;
#[cfg(windows)]
mod win;

use anyhow::{Result, bail};

use app::Config;

const HELP: &str = "\
LagScope - records network, frame-pacing, Wi-Fi and system data while you play,
so lag spikes can be traced to their cause.

USAGE:
    lagscope.exe [OPTIONS]            record a session (default; double-click works)
    lagscope.exe analyze <DIR>        (re)build report.html for a recorded session
    lagscope.exe dump-etw <net|dxgi> [SECS] [MAX]
                                      print raw decoded ETW events (debugging)

OPTIONS:
    --process <NAME.exe>   game executable to watch (repeatable; default Discovery.exe = THE FINALS)
    --hotkey <KEY>         lag-spike mark key, e.g. F8, F9, Pause, Ctrl+Shift+M (default F8)
    --probe-hz <N>         ICMP probes per second per target (default 20)
    --no-probes            disable ICMP probing
    --target <IPv4[@TTL]>  extra address to ping continuously (repeatable); @TTL pings
                           the router at that hop count on the path to the address
    --duration <SECS>      stop automatically after SECS seconds
    --out <DIR>            where to create the session folder (default: next to the exe)
    --no-zip               do not create the .zip at the end
    --no-open              do not open the report in the browser when done
";

fn parse_config(args: &[String]) -> Result<Config> {
    let mut cfg = Config::default();
    let mut custom_process = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| anyhow::anyhow!("{name} needs a value"));
        match a.as_str() {
            "--process" => {
                if !custom_process {
                    cfg.processes.clear();
                    custom_process = true;
                }
                cfg.processes.push(val("--process")?);
            }
            "--hotkey" => cfg.hotkey = val("--hotkey")?,
            "--probe-hz" => cfg.probe_hz = val("--probe-hz")?.parse()?,
            "--no-probes" => cfg.probes = false,
            "--target" => cfg.extra_targets.push(val("--target")?),
            "--duration" => cfg.duration_s = Some(val("--duration")?.parse()?),
            "--out" => cfg.out_dir = Some(val("--out")?.into()),
            "--no-zip" => cfg.zip = false,
            "--no-open" => cfg.open_report = false,
            other => bail!("unknown option '{other}'\n\n{HELP}"),
        }
    }
    Ok(cfg)
}

fn main() -> Result<()> {
    #[cfg(windows)]
    win::harden_dll_search();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("analyze") => {
            let Some(dir) = args.get(1) else { bail!("usage: lagscope analyze <session dir>") };
            let summary = analyze::run(std::path::Path::new(dir))?;
            println!("{summary}");
            Ok(())
        }
        #[cfg(windows)]
        Some("dump-etw") => {
            let kind = args.get(1).map(String::as_str).unwrap_or("net");
            let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
            let max = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(200);
            win::dump::run(kind, secs, max)
        }
        #[cfg(windows)]
        _ => {
            let cfg = parse_config(&args)?;
            let r = win::capture::run(cfg);
            if let Err(e) = &r {
                eprintln!("error: {e:#}");
                eprintln!("Press any key to close.");
                win::console::wait_key();
            }
            r
        }
        #[cfg(not(windows))]
        _ => {
            let _ = parse_config(&args)?;
            bail!("capture only runs on Windows; use `lagscope analyze <dir>` here")
        }
    }
}

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
    lagscope.exe                      menu (double-click): Wi-Fi check, record, undo
    lagscope.exe --wifi-check         Wi-Fi check & fix: test Wi-Fi settings one at a time
    lagscope.exe --record [OPTIONS]   record a session while playing
    lagscope.exe --undo               undo the settings changes LagScope was told to keep
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
    --check-secs <N>       Wi-Fi check: seconds per test (default 60)
    --unattended           Wi-Fi check without questions: runs all tests, installs no
                           drivers and keeps no changes (everything is put back)
    --force-tests          Wi-Fi check: run the tests even if the baseline looks clean
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
            "--record" => {}
            "--wifi-check" => cfg.wifi_check = Some(cfg.wifi_check.unwrap_or(60)),
            "--unattended" => cfg.unattended = true,
            "--force-tests" => cfg.force_tests = true,
            "--check-secs" => cfg.wifi_check = Some(val("--check-secs")?.parse()?),
            other => bail!("unknown option '{other}'\n\n{HELP}"),
        }
    }
    Ok(cfg)
}

#[cfg(windows)]
fn menu() -> char {
    use ui::{BOLD, DIM, RESET};
    win::console::setup();
    let kept = win::fixes::kept();
    println!("{BOLD}LagScope {}{RESET} - finds where lag in THE FINALS comes from\n", env!("CARGO_PKG_VERSION"));
    println!("  {BOLD}1{RESET}  Wi-Fi check & fix     {DIM}do this first: at the desktop, game closed, about 12 minutes{RESET}");
    println!("  {BOLD}2{RESET}  Record while playing  {DIM}start it, play, press F8 at every hiccup, Q when done{RESET}");
    if !kept.is_empty() {
        println!("  {BOLD}3{RESET}  Undo the {} setting change(s) LagScope made", kept.len());
    }
    println!("  {BOLD}Q{RESET}  Quit\n");
    print!("Choose: ");
    let c = win::console::read_choice(if kept.is_empty() { "12q" } else { "123q" }, None);
    println!("{c}\n");
    c
}

#[cfg(windows)]
fn undo() {
    let r = win::fixes::undo_kept();
    if r.is_empty() {
        println!("LagScope has not changed any settings on this PC.");
    }
    for (what, res) in r {
        match res {
            Ok(()) => println!("Undone: {what}"),
            Err(e) => println!("Could not undo '{what}': {e:#}"),
        }
    }
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
        Some("wifi-inventory") => {
            // Read-only: what the Wi-Fi check would test on this PC.
            let route = win::probes::route_to(std::net::Ipv4Addr::new(1, 1, 1, 1));
            let name = args.get(1).filter(|a| !a.starts_with("--")).cloned().or(route.map(|r| r.adapter)).unwrap_or_else(|| "Wi-Fi".into());
            let inv = win::fixes::inventory(&name)?;
            println!("{}", serde_json::to_string_pretty(&inv)?);
            for e in win::fixes::plan(&inv) {
                println!("test {:<11} {:<45} {}", e.key, e.label, e.change.describe());
            }
            if args.iter().any(|a| a == "--wu") {
                for u in win::fixes::search_driver_updates()? {
                    println!("update: {} [{} {} {}]", u.title, u.class, u.provider, u.date);
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        Some("--undo") => {
            win::console::setup();
            undo();
            println!("\nPress any key to close this window.");
            win::console::wait_key();
            Ok(())
        }
        #[cfg(windows)]
        _ => {
            let mut cfg = parse_config(&args)?;
            if args.is_empty() {
                match menu() {
                    'q' => return Ok(()),
                    '1' => cfg.wifi_check = Some(60),
                    '3' => {
                        undo();
                        println!("\nPress any key to close this window.");
                        win::console::wait_key();
                        return Ok(());
                    }
                    _ => {}
                }
            }
            let r = if cfg.wifi_check.is_some() {
                win::wificheck::run(cfg)
            } else {
                win::wificheck::report_restored(win::fixes::restore_pending());
                win::capture::run(cfg)
            };
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

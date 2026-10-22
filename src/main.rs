// A GUI program on Windows: double-click opens the window without a console; command-line
// use attaches to the terminal (see win::console::ensure).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod analyze;
#[cfg_attr(not(windows), allow(dead_code))]
mod profile;
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
    lagscope.exe                      the LagScope window (double-click)
    lagscope.exe --console            console menu: Wi-Fi check, record, undo
    lagscope.exe --tray               start hidden in the tray with auto-record on
    lagscope.exe startup <on|off>     start LagScope (tray, auto-record) at Windows logon
    lagscope.exe --wifi-check         Wi-Fi check & fix: test Wi-Fi settings one at a time
    lagscope.exe --record [OPTIONS]   record a session while playing
    lagscope.exe games                list the game profiles
    lagscope.exe --undo               undo the settings changes LagScope was told to keep
    lagscope.exe analyze <DIR>        (re)build report.html for a recorded session
    lagscope.exe compare <A> <B>      compare two recorded sessions side by side
    lagscope.exe dump-etw <net|dxgi> [SECS] [MAX]
                                      print raw decoded ETW events (debugging)

OPTIONS:
    --game <ID>            game profile: the_finals, cs2, auto (any game), or your own
                           (JSON files in a 'profiles' folder next to the exe; default: last used)
    --process <NAME.exe>   watch this executable instead (repeatable)
    --hotkey <KEY>         lag-spike mark key, e.g. F8, F9, Pause, Ctrl+Shift+M (default F8)
    --probe-hz <N>         ICMP probes per second per target (default 20)
    --no-probes            disable ICMP probing
    --no-kernel-trace      no driver (DPC/ISR) and disk tracing
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

fn set_profile(cfg: &mut Config, p: &profile::Profile) {
    cfg.profile = p.clone();
    cfg.processes = p.processes.clone();
}

fn parse_config(args: &[String]) -> Result<Config> {
    let mut cfg = Config::default();
    let (profiles, _) = profile::load_all(&profile::user_dirs());
    if let Some(p) = profile::last_game().and_then(|g| profile::find(&profiles, &g).cloned()) {
        set_profile(&mut cfg, &p);
    }
    if profile::setting("kernel_trace").and_then(|v| v.as_bool()) == Some(false) {
        cfg.kernel_trace = false;
    }
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
                let p = val("--process")?;
                cfg.processes.push(p.clone());
                cfg.profile = profile::find(&profiles, &p).cloned().unwrap_or_else(|| profile::Profile {
                    id: "custom".into(),
                    name: cfg.processes.join(", "),
                    processes: cfg.processes.clone(),
                    ..profile::builtin().last().unwrap().clone()
                });
            }
            "--game" => {
                let g = val("--game")?;
                let Some(p) = profile::find(&profiles, &g) else {
                    let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
                    bail!("unknown game '{g}' (known: {})", ids.join(", "));
                };
                set_profile(&mut cfg, p);
            }
            "--hotkey" => cfg.hotkey = val("--hotkey")?,
            "--probe-hz" => cfg.probe_hz = val("--probe-hz")?.parse()?,
            "--no-probes" => cfg.probes = false,
            "--no-kernel-trace" => cfg.kernel_trace = false,
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
    println!("{BOLD}LagScope {}{RESET} - finds where lag in online games comes from\n", env!("CARGO_PKG_VERSION"));
    println!("  {BOLD}1{RESET}  Wi-Fi check & fix     {DIM}do this first: at the desktop, game closed, about 15 minutes{RESET}");
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

/// Asks which game to record (Enter = the one used last time).
#[cfg(windows)]
fn pick_game(current: &str) -> profile::Profile {
    use ui::{BOLD, DIM, RESET};
    let (all, errors) = profile::load_all(&profile::user_dirs());
    for e in errors {
        println!("problem in profile file {e}");
    }
    let def = all.iter().position(|p| p.id == current).unwrap_or(0);
    println!("Which game?");
    let keys: String = (1..=all.len().min(9)).map(|i| char::from(b'0' + i as u8)).collect();
    for (i, p) in all.iter().take(9).enumerate() {
        println!("  {BOLD}{}{RESET}  {}{}", i + 1, p.name, if i == def { format!("  {DIM}(Enter){RESET}") } else { String::new() });
    }
    print!("Choose: ");
    let c = win::console::read_choice(&keys, Some(char::from(b'1' + def as u8)));
    let i = c.to_digit(10).map(|d| d as usize - 1).unwrap_or(def);
    println!("{}\n", all[i].name);
    all[i].clone()
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
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    {
        if args.is_empty() {
            return win::gui::run(parse_config(&[])?, None, false);
        }
        if args[0] == "--gui-selftest" {
            let scenario = args.get(1).cloned().unwrap_or_else(|| "idle".into());
            let out = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("lagscope_selftest"));
            let rest: Vec<String> = args.iter().skip(3).cloned().collect();
            return win::gui::run(parse_config(&rest)?, Some((scenario, out)), false);
        }
        if args[0] == "--tray" {
            // Started at logon: hidden in the tray with auto-record on.
            return win::gui::run(parse_config(&args[1..])?, None, true);
        }
        win::console::ensure();
        if args[0] == "--console" {
            args.remove(0);
        }
    }
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("games") => {
            let (all, errors) = profile::load_all(&profile::user_dirs());
            for p in &all {
                println!("{:<12} {:<28} {}", p.id, p.name, if p.is_auto() { "(finds the running game)".into() } else { p.processes.join(", ") });
            }
            for e in errors {
                println!("problem in profile file {e}");
            }
            Ok(())
        }
        Some("compare") => {
            let (Some(a), Some(b)) = (args.get(1), args.get(2)) else { bail!("usage: lagscope compare <session A> <session B>") };
            let c = analyze::compare::compare(std::path::Path::new(a), std::path::Path::new(b))?;
            print!("{}", analyze::compare::to_text(&c));
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
        Some("startup") => {
            match args.get(1).map(String::as_str) {
                Some("on") => win::gui::set_startup_task(true).map_err(|e| anyhow::anyhow!(e))?,
                Some("off") => win::gui::set_startup_task(false).map_err(|e| anyhow::anyhow!(e))?,
                _ => {}
            }
            println!("start with Windows (tray, auto-record): {}", if win::gui::startup_task_exists() { "on" } else { "off" });
            Ok(())
        }
        #[cfg(windows)]
        Some("load-test") => {
            let r = win::loadtest::run(&|s| println!("{s}"))?;
            for l in r.summary.iter().chain(&r.recommendations) {
                println!("{l}");
            }
            Ok(())
        }
        #[cfg(windows)]
        Some("wifi-scan") => {
            let r = win::wifiscan::scan()?;
            for l in &r.summary {
                println!("{l}");
            }
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&r)?);
            }
            Ok(())
        }
        #[cfg(windows)]
        Some("router") => {
            let route = win::probes::route_to(std::net::Ipv4Addr::new(1, 1, 1, 1));
            let Some(gw) = route.and_then(|r| r.gateway) else { bail!("no default gateway") };
            let hops = win::probes::traceroute(std::net::Ipv4Addr::new(1, 1, 1, 1), 4);
            let hop2 = hops.iter().filter_map(|h| h.ip).find(|ip| *ip != gw);
            println!("{}", serde_json::to_string_pretty(&win::router::identify(gw, hop2))?);
            Ok(())
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
                    '2' => {
                        let p = pick_game(&cfg.profile.id);
                        set_profile(&mut cfg, &p);
                        profile::save_last_game(&p.id);
                    }
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

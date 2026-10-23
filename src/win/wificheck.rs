//! Guided "Wi-Fi check & fix": at the desktop (no game needed), measure the router ping,
//! then switch suspects off one at a time (each for one test window, restored right after)
//! and see which one makes the regular freezes disappear. Offers driver updates via
//! Windows Update first and, at the end, to keep the change that fixed it.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::fixes::{self, Experiment, Inventory};
use super::{Running, capture, console, probes, spawn};
use crate::analyze::experiments::{self, Window};
use crate::analyze::findings::ROUTER_STEPS;
use crate::app::{App, Config};
use crate::ui::{BOLD, CYAN, DIM, GREEN, RED, RESET, YELLOW};

/// Like println!, but through the shared log (console or GUI).
macro_rules! say {
    ($($a:tt)*) => { crate::ui::log(format!($($a)*)) };
}

#[derive(Default)]
struct CheckState {
    windows: Vec<Window>,
    /// Tests whose change could not be applied: (key, error).
    failed: Vec<(String, String)>,
    skipped_no_problem: bool,
    /// Band the adapter actually used during the band test.
    band_after: Option<String>,
}

enum Step {
    Baseline(&'static str, &'static str, u64),
    Test(Experiment, u64),
}

pub fn report_restored(r: Vec<(String, Result<()>)>) {
    for (what, res) in r {
        match res {
            Ok(()) => say!("{GREEN}Restored from an earlier, interrupted check: {what}{RESET}"),
            Err(e) => say!("{RED}Could not restore '{what}': {e:#}{RESET}"),
        }
    }
}

fn wait_key_exit() {
    say!("\nPress any key to close this window.");
    console::wait_key();
}

/// Everything before recording starts: inventory, driver step, consent. Returns the
/// recording config and hooks, or None if there is nothing to record.
pub fn prepare(mut cfg: Config) -> Result<Option<(Config, capture::Hooks)>> {
    let secs = cfg.wifi_check.unwrap_or(60).max(10);
    say!("{BOLD}LagScope {} - Wi-Fi check & fix{RESET}\n", env!("CARGO_PKG_VERSION"));
    report_restored(fixes::restore_pending());

    let Some(route) = probes::route_to(Ipv4Addr::new(1, 1, 1, 1)) else {
        say!("{RED}This PC has no internet connection right now. Connect to your Wi-Fi and try again.{RESET}");
        return Ok(None);
    };
    if !route.is_wifi {
        say!("This PC is connected through '{}' ({}), not Wi-Fi, so there is nothing to check here.", route.adapter, route.description);
        return Ok(None);
    }
    say!("Reading the Wi-Fi settings of '{}' ...", route.adapter);
    let inv = match fixes::inventory(&route.adapter) {
        Ok(i) => i,
        Err(e) => {
            say!("{YELLOW}Could not read the adapter settings ({e:#}); the check will only measure, not test changes.{RESET}");
            Inventory { adapter: route.adapter.clone(), description: route.description.clone(), ..Default::default() }
        }
    };
    print_inventory(&inv);

    let mut driver_log = Vec::new();
    if !cfg.unattended && driver_step(&inv, &mut driver_log) {
        return Ok(None);
    }

    let plan = fixes::plan(&inv);
    let mut allowed = Vec::new();
    if plan.is_empty() {
        say!("\nThere are no settings to test on this PC; the check will measure the Wi-Fi link only.");
    } else {
        let mins = (secs * 2 + secs * 3 / 4 + secs * 3 / 2 + plan.len() as u64 * (secs + 20)) / 60 + 1;
        say!("\n{BOLD}The check takes about {mins} minutes.{RESET} It measures your Wi-Fi with nothing changed, then tests these one at a time");
        say!("(each for {secs} s, then put back exactly as it was):");
        for e in &plan {
            say!("  - {}", e.label);
        }
        say!("{DIM}Wi-Fi drops for a few seconds when an adapter setting is changed. Bluetooth devices disconnect during the Bluetooth test.{RESET}");
        say!("Close your games and anything that streams or downloads, and do not use the PC until the check is done.");
        if cfg.unattended || console::ask_yes_no("\nAllow these temporary changes?", true) {
            allowed = plan;
        } else {
            say!("OK - measuring only, nothing will be changed.");
        }
    }

    cfg.wifi_check = Some(secs);
    let state = Arc::new(Mutex::new(CheckState::default()));
    let (st1, st2) = (state.clone(), state.clone());
    let (plan1, plan2) = (allowed.clone(), allowed);
    let unattended = cfg.unattended;
    let inv_json = serde_json::to_string_pretty(&inv).unwrap_or_default();
    let driver_log: Vec<serde_json::Value> = driver_log.into_iter().map(Into::into).collect();
    let hooks = capture::Hooks {
        start: Box::new(move |app: Arc<App>| {
            app.session.write_text("sysinfo/wifi_inventory.json", &inv_json);
            app.session.meta_set("driver_check", driver_log.into());
            app.session.meta_set("wifi_check_plan", plan1.iter().map(|e| serde_json::json!({"key": e.key, "label": e.label, "change": e.change.describe()})).collect::<Vec<_>>().into());
            let a = app.clone();
            let t = spawn("wifi-check", move || controller(a, plan1, secs, st1));
            Running::threads("wifi-check", vec![t])
        }),
        after_stop: Box::new(move |app: &App| results(app, &plan2, &st2, unattended)),
    };
    Ok(Some((cfg, hooks)))
}

pub fn run(cfg: Config) -> Result<()> {
    console::setup();
    match prepare(cfg)? {
        Some((cfg, hooks)) => capture::run_with(cfg, Some(hooks)),
        None => {
            wait_key_exit();
            Ok(())
        }
    }
}

fn print_inventory(inv: &Inventory) {
    say!("  Adapter:   {}", inv.description);
    say!("  Driver:    {} ({})", if inv.driver_version.is_empty() { "?" } else { &inv.driver_version }, inv.driver_date);
    if !inv.band.is_empty() {
        say!("  Connected: {} band", inv.band);
    }
    for p in inv.adv.iter().filter(|p| {
        let d = p.display.to_ascii_lowercase();
        ["roam", "band", "power", "throughput", "bandwidth", "mlo", "multi-link"].iter().any(|w| d.contains(w))
    }) {
        say!("  {:<26} {}", p.display, p.value);
    }
    if !inv.selective_suspend.is_empty() {
        say!("  {:<26} {}", "Selective suspend", inv.selective_suspend);
    }
    say!("  Bluetooth: {}   Location services: {}   Windows Wi-Fi auto-config: {}", if inv.bluetooth.is_empty() { "none".into() } else { inv.bluetooth.join(", ") }, inv.location, if inv.autoconfig { "on" } else { "off" });
}

/// Driver version check and optional update through Windows Update. Returns true if the
/// PC must restart before the check makes sense.
fn driver_step(inv: &Inventory, log: &mut Vec<String>) -> bool {
    let known = fixes::KNOWN_DRIVERS.iter().find(|k| inv.description.contains(k.0));
    let outdated = known.filter(|k| !inv.driver_version.is_empty() && fixes::version_lt(&inv.driver_version, k.1));
    match (known, outdated) {
        (_, Some(k)) => say!("\n{YELLOW}Your Wi-Fi driver {} is older than the newest known one ({}, {}).{RESET}", inv.driver_version, k.1, k.2),
        (Some(k), None) => say!("\n{GREEN}Your Wi-Fi driver is up to date (newest known: {}).{RESET}", k.1),
        _ => {}
    }
    if !console::ask_yes_no("\nCheck Windows Update for newer Wi-Fi / Bluetooth drivers? (takes a minute or two)", true) {
        log.push("Windows Update search skipped by user".into());
        return false;
    }
    say!("Searching Windows Update ...");
    match fixes::search_driver_updates() {
        Ok(list) if !list.is_empty() => {
            log.push(format!("Windows Update offers: {}", list.iter().map(|u| u.title.as_str()).collect::<Vec<_>>().join("; ")));
            say!("Windows Update offers:");
            for u in &list {
                say!("  - {} {DIM}({} {}){RESET}", u.title, u.class, u.date);
            }
            if console::ask_yes_no("Install these now?", true) {
                say!("Downloading and installing (Wi-Fi may drop for a moment) ...");
                let ids: Vec<String> = list.iter().map(|u| u.id.clone()).collect();
                let r = fixes::install_driver_updates(&ids);
                log.push(format!("install: {r:?}"));
                match r {
                    Ok(true) => {
                        say!("{GREEN}{BOLD}Installed. Restart the PC now, then run the Wi-Fi check again.{RESET}");
                        return true;
                    }
                    Ok(false) => {
                        say!("{GREEN}Installed.{RESET} Waiting 20 s for Wi-Fi to reconnect ...");
                        std::thread::sleep(Duration::from_secs(20));
                    }
                    Err(e) => say!("{RED}Install failed: {e:#}{RESET}"),
                }
            }
        }
        Ok(_) => {
            log.push("Windows Update: no newer network/Bluetooth drivers".into());
            say!("Windows Update has no newer network or Bluetooth drivers for this PC.");
            if let Some(k) = outdated {
                say!("{YELLOW}To install {} by hand:{RESET}", k.1);
                say!("  1. Open https://www.catalog.update.microsoft.com and search for \"{}\".", k.0);
                say!("  2. Download \"... Net Driver Update ({})\" for your Windows version (a .cab file).", k.1);
                say!("  3. Open the .cab, select all files, drag them into a new folder.");
                say!("  4. Device Manager > Network adapters > right-click the {} adapter > Update driver >", k.0);
                say!("     Browse my computer > choose that folder. Restart, then run this check again.");
            }
        }
        Err(e) => {
            log.push(format!("Windows Update search failed: {e:#}"));
            say!("{YELLOW}Windows Update search failed: {e:#}{RESET}");
        }
    }
    false
}

/// Waits until the router answers pings again (after an adapter restart).
fn wait_reconnect(app: &App, max: Duration) -> bool {
    let start = Instant::now();
    std::thread::sleep(Duration::from_secs(3));
    while start.elapsed() < max && !app.stopping() {
        let since = app.session.clock.now_us() - 1_500_000;
        let ok = app.gw_samples.lock().unwrap().iter().rev().take_while(|(t, _)| *t >= since).filter(|(_, r)| r.is_some()).count();
        if ok >= 15 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn set_status(app: &App, s: String) {
    *app.check_status.lock().unwrap() = Some(s);
}

fn controller(app: Arc<App>, plan: Vec<Experiment>, secs: u64, st: Arc<Mutex<CheckState>>) {
    let mut steps = vec![Step::Baseline("baseline1", "Nothing changed (baseline)", if plan.is_empty() { secs * 3 } else { secs * 3 / 2 })];
    let half = plan.len().div_ceil(2);
    for (i, e) in plan.iter().enumerate() {
        if i == half && plan.len() >= 4 {
            steps.push(Step::Baseline("baseline2", "Nothing changed (baseline, middle)", secs * 3 / 4));
        }
        steps.push(Step::Test(e.clone(), secs));
    }
    if !plan.is_empty() {
        steps.push(Step::Baseline("baseline3", "Nothing changed (baseline, end)", secs));
    }
    let n = steps.len();
    set_status(&app, format!("{CYAN}Starting ...{RESET}"));
    if !wait_reconnect(&app, Duration::from_secs(20)) && app.stopping() {
        return;
    }
    let mut base_rate: Option<f64> = None;
    for (i, step) in steps.iter().enumerate() {
        if app.stopping() {
            break;
        }
        let (key, label, len, exp) = match step {
            Step::Baseline(k, l, s) => (k.to_string(), l.to_string(), *s, None),
            Step::Test(e, s) => (e.key.to_string(), e.label.clone(), *s, Some(e)),
        };
        if let Some(e) = exp {
            set_status(&app, format!("Step {}/{n}: {YELLOW}switching: {label}{RESET} ...", i + 1));
            app.session.event("exp_note", format!("applying {}: {}", e.key, e.change.describe()));
            if let Err(err) = fixes::apply_temp(&e.change) {
                app.session.event("exp_note", format!("could not apply {}: {err:#}", e.key));
                st.lock().unwrap().failed.push((e.key.to_string(), format!("{err:#}")));
                if e.change.restarts_adapter() {
                    wait_reconnect(&app, Duration::from_secs(45));
                }
                continue;
            }
            if e.change.restarts_adapter() {
                let ok = wait_reconnect(&app, Duration::from_secs(60));
                if e.key == "band" {
                    let now = match &e.change {
                        fixes::Change::Adapter { adapter, .. } => fixes::current_band(adapter),
                        _ => String::new(),
                    };
                    st.lock().unwrap().band_after = Some(now);
                }
                app.session.event("exp_note", format!("{}: Wi-Fi {}; band now {}", e.key, if ok { "reconnected" } else { "did NOT reconnect within 60 s" }, match &e.change {
                    fixes::Change::Adapter { adapter, .. } => fixes::current_band(adapter),
                    _ => String::new(),
                }));
            }
        }
        let start = app.session.clock.now_us();
        app.session.event("exp", format!("start|{key}|{label}"));
        let end = start + len as i64 * 1_000_000;
        while app.session.clock.now_us() < end && !app.stopping() {
            let now = app.session.clock.now_us();
            let w = Window { key: key.clone(), label: label.clone(), start, end: now };
            let samples = app.gw_samples.lock().unwrap().clone();
            let thr = experiments::threshold(base_median(&samples, &st.lock().unwrap().windows));
            let s = experiments::stats(&samples, &w, thr);
            set_status(
                &app,
                format!(
                    "Step {}/{n}: {BOLD}{label}{RESET}   {} s left   freezes: {}{}",
                    i + 1,
                    (end - now) / 1_000_000,
                    s.spikes,
                    base_rate.map(|r| format!("   {DIM}(baseline: {r:.1}/min){RESET}")).unwrap_or_default()
                ),
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        let stop = app.session.clock.now_us();
        app.session.event("exp", format!("end|{key}"));
        st.lock().unwrap().windows.push(Window { key: key.clone(), label: label.clone(), start, end: stop });
        if let Some(e) = exp {
            set_status(&app, format!("Step {}/{n}: {YELLOW}putting back: {label}{RESET} ...", i + 1));
            match fixes::revert_temp(&e.change) {
                Ok(()) => app.session.event("exp_note", format!("restored {}", e.key)),
                Err(err) => app.session.event("exp_note", format!("could not restore {}: {err:#} (will retry at next start)", e.key)),
            }
            // Even changes that do not restart the adapter can briefly drop the link
            // (re-enabling auto-config reconnects), so always wait for the router.
            wait_reconnect(&app, Duration::from_secs(if e.change.restarts_adapter() { 60 } else { 30 }));
            std::thread::sleep(Duration::from_secs(2));
        }
        if key == "baseline1" {
            let samples = app.gw_samples.lock().unwrap().clone();
            let o = experiments::evaluate(&samples, &st.lock().unwrap().windows);
            base_rate = Some(o.baseline_per_min);
            if !plan.is_empty() && o.baseline_per_min < 1.0 && !app.cfg.force_tests {
                app.session.event("exp_note", format!("no regular freezes in the baseline ({:.1}/min); tests skipped", o.baseline_per_min));
                st.lock().unwrap().skipped_no_problem = true;
                break;
            }
        }
    }
    app.session.event("exp_note", "check finished");
    app.stop.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn base_median(samples: &[(i64, Option<f64>)], windows: &[Window]) -> f64 {
    let mut v: Vec<f64> = samples
        .iter()
        .filter(|(t, _)| windows.iter().any(|w| experiments::is_baseline(&w.key) && *t >= w.start && *t < w.end))
        .filter_map(|(_, r)| *r)
        .collect();
    if v.is_empty() {
        // First baseline still running: use what we have so far.
        v = samples.iter().filter_map(|(_, r)| *r).collect();
    }
    v.sort_by(|a, b| a.total_cmp(b));
    crate::analyze::detect::percentile(&v, 0.5)
}

fn results(app: &App, plan: &[Experiment], st: &Mutex<CheckState>, unattended: bool) {
    let st = st.lock().unwrap();
    let samples = app.gw_samples.lock().unwrap().clone();
    let o = experiments::evaluate(&samples, &st.windows);
    say!("\n{BOLD}Wi-Fi check results{RESET} {DIM}(freeze = router ping above {:.0} ms or lost){RESET}", o.threshold_ms);
    for w in &o.windows {
        let tag = if o.fixes.contains(&w.key) {
            format!("  {GREEN}{BOLD}<- fixed{RESET}")
        } else if o.partial.contains(&w.key) {
            format!("  {YELLOW}<- fewer{RESET}")
        } else {
            String::new()
        };
        say!("  {:<46} {:>3} freezes in {:>3.0} s  ({:>4.1}/min){tag}", w.label, w.spikes, w.secs, w.per_min);
    }
    if let Some(b) = &st.band_after {
        say!("  {DIM}(during the band test the PC was connected on: {}){RESET}", if b.is_empty() { "?" } else { b });
    }
    for (k, e) in &st.failed {
        say!("  {RED}{k}: could not be changed ({e}){RESET}");
    }
    say!("");
    for s in &o.summary {
        say!("{s}");
    }
    let finals = "Next: record a match or two (Record), pressing F8 at every hiccup.";
    if st.skipped_no_problem || !o.problem {
        say!("\n{GREEN}{BOLD}No regular Wi-Fi freezes right now.{RESET}");
        if app.game_pid.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            say!("{YELLOW}The freeze may only happen while a game is running: start your game, stay in its main menu,");
            say!("and run this check again. If that is clean too:{RESET} {finals}");
        } else {
            say!("{finals}");
        }
        app.session.event("exp_result", "no problem found");
        return;
    }
    if let Some(e) = plan.iter().find(|e| o.fixes.iter().any(|k| k == e.key)) {
        say!("\n{GREEN}{BOLD}Fix found: {}{RESET}", e.label);
        if !e.keep_note.is_empty() {
            say!("{YELLOW}{}{RESET}", e.keep_note);
        }
        if !unattended && console::ask_yes_no("Keep this change permanently? (you can undo it later from the LagScope menu)", true) {
            match fixes::keep(&e.change) {
                Ok(()) => {
                    say!("{GREEN}Done: {}{RESET}", e.change.describe());
                    app.session.event("exp_result", format!("fix kept: {} ({})", e.key, e.change.describe()));
                    if e.change.restarts_adapter() {
                        say!("Waiting for Wi-Fi to reconnect ...");
                        wait_reconnect_blocking();
                    }
                }
                Err(err) => {
                    say!("{RED}Could not apply it: {err:#}{RESET}");
                    app.session.event("exp_result", format!("keeping {} failed: {err:#}", e.key));
                }
            }
        } else {
            app.session.event("exp_result", format!("fix found but not kept: {}", e.key));
        }
        say!("{finals}");
    } else if o.partial.is_empty() {
        say!("\n{YELLOW}{BOLD}The PC is not the cause.{RESET} {ROUTER_STEPS}");
        say!("After changing the router, run this check again to see whether the freezes are gone.");
        app.session.event("exp_result", "no PC-side change helped: router");
    } else {
        say!("\n{YELLOW}Some changes reduced the freezes but none removed them.{RESET} {ROUTER_STEPS}");
        app.session.event("exp_result", "only partial improvements");
    }
}

fn wait_reconnect_blocking() {
    for _ in 0..60 {
        if probes::route_to(Ipv4Addr::new(1, 1, 1, 1)).is_some_and(|r| r.gateway.is_some()) {
            std::thread::sleep(Duration::from_secs(2));
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

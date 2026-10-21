//! NVIDIA GPU telemetry through NVML (nvml.dll ships with every NVIDIA driver; it is
//! loaded dynamically, nothing is installed). Beyond utilisation this records clock-event
//! ("throttle") reasons, P-state, power vs. limit, PCIe traffic and encoder load
//! (ShadowPlay / Instant Replay). Sampled at 4 Hz while the game runs, every 5 s otherwise
//! (so a laptop's discrete GPU is not kept awake needlessly).

use std::ffi::OsStr;
use std::sync::Arc;

use nvml_wrapper::Nvml;
use nvml_wrapper::bitmasks::device::ThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor};

use super::{Running, spawn};
use crate::app::App;
use crate::row;

fn load() -> Option<Nvml> {
    let candidates = [
        super::system32("nvml.dll").to_string_lossy().into_owned(),
        std::env::var("ProgramW6432").map(|p| format!("{p}\\NVIDIA Corporation\\NVSMI\\nvml.dll")).unwrap_or_default(),
    ];
    for c in candidates.iter().filter(|c| !c.is_empty()) {
        if std::path::Path::new(c).exists() {
            if let Ok(n) = Nvml::builder().lib_path(OsStr::new(c)).init() {
                return Some(n);
            }
        }
    }
    // No search-path fallback: we run elevated, so only load NVML from known locations.
    None
}

fn reasons_text(r: ThrottleReasons) -> String {
    let names = [
        (ThrottleReasons::GPU_IDLE, "idle"),
        (ThrottleReasons::APPLICATIONS_CLOCKS_SETTING, "app_clocks"),
        (ThrottleReasons::SW_POWER_CAP, "sw_power_cap"),
        (ThrottleReasons::HW_SLOWDOWN, "hw_slowdown"),
        (ThrottleReasons::SYNC_BOOST, "sync_boost"),
        (ThrottleReasons::SW_THERMAL_SLOWDOWN, "sw_thermal"),
        (ThrottleReasons::HW_THERMAL_SLOWDOWN, "hw_thermal"),
        (ThrottleReasons::HW_POWER_BRAKE_SLOWDOWN, "hw_power_brake"),
        (ThrottleReasons::DISPLAY_CLOCK_SETTING, "display_clock"),
    ];
    names.iter().filter(|(f, _)| r.contains(*f)).map(|(_, n)| *n).collect::<Vec<_>>().join("|")
}

/// nvml-wrapper's PerformanceState debug names are words ("Zero".."Fifteen"); show "P0".."P15".
fn pstate_name(dbg: &str) -> String {
    const W: [&str; 16] = [
        "Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven", "Twelve",
        "Thirteen", "Fourteen", "Fifteen",
    ];
    W.iter().position(|w| *w == dbg).map(|i| format!("P{i}")).unwrap_or_else(|| dbg.to_string())
}

pub fn start(app: Arc<App>) -> Option<Running> {
    let a = app.clone();
    let t = spawn("gpu", move || {
        let Some(nvml) = load() else {
            a.session.event("collector", "gpu: NVML not available (no NVIDIA GPU/driver) - using GPU Engine counters only");
            return;
        };
        let count = nvml.device_count().unwrap_or(0);
        let mut devices = Vec::new();
        let mut info = Vec::new();
        for i in 0..count {
            if let Ok(d) = nvml.device_by_index(i) {
                info.push(serde_json::json!({
                    "index": i,
                    "name": d.name().unwrap_or_default(),
                    "power_limit_w": d.enforced_power_limit().ok().map(|p| p as f64 / 1000.0),
                    "pcie_gen": d.current_pcie_link_gen().ok(),
                    "pcie_width": d.current_pcie_link_width().ok(),
                }));
                devices.push(d);
            }
        }
        let driver = nvml.sys_driver_version().unwrap_or_default();
        a.session.meta_set("nvidia", serde_json::json!({ "driver": driver, "devices": info }));
        a.session.event("collector", format!("gpu: NVML driver {driver}, {} device(s)", devices.len()));
        let sink = a.session.sink(
            "gpu",
            "t_us,gpu,util_pct,mem_util_pct,gfx_mhz,mem_mhz,power_w,power_limit_w,temp_c,pstate,reasons_hex,reasons,pcie_tx_kbs,pcie_rx_kbs,vram_used_mb,enc_pct,dec_pct",
        );
        let mut tick = 0u64;
        loop {
            let fast = a.game_pid.load(std::sync::atomic::Ordering::Relaxed) != 0;
            if !a.sleep_ms(if fast { 250 } else { 5000 }) {
                break;
            }
            tick += 1;
            let t = a.session.clock.now_us();
            for (i, d) in devices.iter().enumerate() {
                let u = d.utilization_rates().ok();
                let r = d.current_throttle_reasons().ok();
                // PCIe counters block ~20 ms each, so sample them at 1 Hz.
                let (tx, rx) = if !fast || tick % 4 == 0 {
                    (d.pcie_throughput(PcieUtilCounter::Send).ok(), d.pcie_throughput(PcieUtilCounter::Receive).ok())
                } else {
                    (None, None)
                };
                let o = |v: Option<u32>| v.map(|x| x.to_string()).unwrap_or_default();
                let power = d.power_usage().ok().map(|p| p as f64 / 1000.0);
                let limit = d.enforced_power_limit().ok().map(|p| p as f64 / 1000.0);
                let temp = d.temperature(TemperatureSensor::Gpu).ok();
                let pstate = d.performance_state().ok().map(|p| pstate_name(&format!("{p:?}"))).unwrap_or_default();
                row!(
                    sink,
                    "{t},{i},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                    o(u.as_ref().map(|u| u.gpu)),
                    o(u.as_ref().map(|u| u.memory)),
                    o(d.clock_info(Clock::Graphics).ok()),
                    o(d.clock_info(Clock::Memory).ok()),
                    power.map(|p| format!("{p:.1}")).unwrap_or_default(),
                    limit.map(|p| format!("{p:.0}")).unwrap_or_default(),
                    o(temp),
                    pstate,
                    r.map(|r| format!("{:x}", r.bits())).unwrap_or_default(),
                    r.map(reasons_text).unwrap_or_default(),
                    o(tx),
                    o(rx),
                    d.memory_info().ok().map(|m| (m.used / 1048576).to_string()).unwrap_or_default(),
                    o(d.encoder_utilization().ok().map(|e| e.utilization)),
                    o(d.decoder_utilization().ok().map(|e| e.utilization)),
                );
                if i == 0 {
                    a.live.lock().unwrap().gpu = Some(format!(
                        "GPU {}%  {}  {}W/{}W  {}C  {}",
                        o(u.as_ref().map(|u| u.gpu)),
                        pstate,
                        power.map(|p| format!("{p:.0}")).unwrap_or("?".into()),
                        limit.map(|p| format!("{p:.0}")).unwrap_or("?".into()),
                        o(temp),
                        r.map(reasons_text).filter(|s| !s.is_empty()).unwrap_or_else(|| "no limits".into())
                    ));
                }
            }
        }
    });
    Some(Running::threads("gpu", vec![t]))
}

//! System stall detector: a time-critical thread asks for a 1 ms high-resolution timer
//! wake-up in a loop and measures how late each wake-up actually is. Lateness of several
//! milliseconds means the whole machine (or this core) was busy in drivers (DPC/ISR),
//! SMM firmware, power-state transitions etc. -- the kind of stall that also hits the game.

use std::sync::Arc;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, GetCurrentThread, SetThreadPriority,
    SetWaitableTimer, THREAD_PRIORITY_TIME_CRITICAL, TIMER_ALL_ACCESS, WaitForSingleObject,
};

use super::{Running, spawn};
use crate::app::App;
use crate::clock;
use crate::row;

/// Individual wake-ups later than this are logged to stalls.csv.
const EVENT_THRESHOLD_US: i64 = 4000;

pub fn start(app: Arc<App>) -> Running {
    let events = app.session.sink("stalls", "t_us,late_us");
    let summary = app.session.sink("stall_1s", "t_us,wakeups,max_late_us,p99_late_us,n_over_2ms,n_over_8ms");
    let a = app.clone();
    let t = spawn("stall", move || unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let timer = CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
        if timer.is_null() {
            a.session.event("collector", "stall: high-resolution timers not supported (needs Windows 10 1803+)");
            return;
        }
        let clock = a.session.clock;
        let due: i64 = -10_000; // 1 ms, relative, in 100 ns units
        let mut sec = clock.now_us() / 1_000_000;
        let mut lates: Vec<i64> = Vec::with_capacity(1100);
        while !a.stopping() {
            let before = clock::raw_now();
            SetWaitableTimer(timer, &due, 0, None, std::ptr::null(), 0);
            WaitForSingleObject(timer, 1000);
            let after = clock::raw_now();
            let late = clock.raw_to_us_delta(after - before) - 1000;
            let t = clock.us_from_raw(before);
            if late > EVENT_THRESHOLD_US {
                row!(events, "{t},{late}");
            }
            lates.push(late.max(0));
            let now_sec = clock.us_from_raw(after) / 1_000_000;
            if now_sec != sec {
                lates.sort_unstable();
                let n = lates.len();
                let max = lates.last().copied().unwrap_or(0);
                let p99 = if n > 0 { lates[(n * 99 / 100).min(n - 1)] } else { 0 };
                let o2 = lates.iter().filter(|&&l| l > 2000).count();
                let o8 = lates.iter().filter(|&&l| l > 8000).count();
                row!(summary, "{},{n},{max},{p99},{o2},{o8}", sec * 1_000_000);
                a.live.lock().unwrap().stall_max_ms = max as f64 / 1000.0;
                lates.clear();
                sec = now_sec;
            }
        }
        CloseHandle(timer);
    });
    Running::threads("stall", vec![t])
}

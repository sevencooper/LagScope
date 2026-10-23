//! Whether the game is the foreground window (alt-tab, clicks on a second screen, the lock
//! screen). Switching away and back makes the game hitch and change its frame rate, which
//! is normal and must not be counted as stutter. Polled at 4 Hz from the foreground window's
//! owning process ID; no handle to the game is opened. Changes go to `focus.csv`.

use std::sync::Arc;

use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::{Running, spawn};
use crate::app::App;
use crate::row;

fn foreground_pid() -> u32 {
    let mut pid = 0u32;
    let w = unsafe { GetForegroundWindow() };
    if !w.is_null() {
        unsafe { GetWindowThreadProcessId(w, &mut pid) };
    }
    pid
}

pub fn start(app: Arc<App>) -> Running {
    let sink = app.session.sink("focus", "t_us,game,process");
    let a = app.clone();
    let t = spawn("focus", move || {
        let mut last: Option<bool> = None;
        loop {
            let pid = foreground_pid();
            let game = pid != 0 && a.is_game_pid(pid);
            if last != Some(game) {
                let name = if pid == 0 { "none".to_string() } else { a.proc_name(pid) };
                row!(sink, "{},{},{}", a.session.clock.now_us(), game as u8, crate::session::csv_escape(&name));
                last = Some(game);
            }
            if !a.sleep_ms(250) {
                break;
            }
        }
    });
    Running::threads("focus", vec![t])
}

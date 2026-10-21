//! Console setup, keyboard commands (M = mark, Q = stop) and Ctrl+C / window-close handling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

use super::hotkey::Marker;
use super::{Running, spawn, wide};
use crate::app::App;

static APP: OnceLock<Arc<App>> = OnceLock::new();
/// Set once the session has been finalised; the close handler waits for it.
pub static FINISHED: AtomicBool = AtomicBool::new(false);

pub fn setup() {
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode: CONSOLE_MODE = 0;
        if GetConsoleMode(out, &mut mode) != 0 {
            SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | ENABLE_PROCESSED_OUTPUT);
        }
        // QuickEdit freezes console output while text is selected; turn it off so a stray
        // click in the window can never stall the UI.
        let inp = GetStdHandle(STD_INPUT_HANDLE);
        if GetConsoleMode(inp, &mut mode) != 0 {
            let m = (mode | ENABLE_EXTENDED_FLAGS) & !(ENABLE_QUICK_EDIT_MODE | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT);
            SetConsoleMode(inp, m);
        }
        let title = wide(&format!("LagScope {}", env!("CARGO_PKG_VERSION")));
        SetConsoleTitleW(title.as_ptr());
        SetConsoleOutputCP(65001);
    }
}

unsafe extern "system" fn on_ctrl(kind: u32) -> windows_sys::core::BOOL {
    if let Some(app) = APP.get() {
        app.stop.store(true, Ordering::SeqCst);
        if kind != CTRL_C_EVENT && kind != CTRL_BREAK_EVENT {
            // Window close / logoff / shutdown: Windows kills us when this returns (or after
            // ~5 s), so wait for the session to be finalised first.
            for _ in 0..45 {
                if FINISHED.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    1
}

pub fn install_ctrl_handler(app: Arc<App>) {
    let _ = APP.set(app);
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
}

/// Reads single key presses from the console window.
pub fn start_input(app: Arc<App>, marker: Arc<Marker>) -> Running {
    let t = spawn("console-input", move || unsafe {
        let inp: HANDLE = GetStdHandle(STD_INPUT_HANDLE);
        let mut rec: [INPUT_RECORD; 16] = std::mem::zeroed();
        while !app.stopping() {
            if WaitForSingleObject(inp, 200) != WAIT_OBJECT_0 {
                continue;
            }
            let mut n = 0u32;
            if ReadConsoleInputW(inp, rec.as_mut_ptr(), rec.len() as u32, &mut n) == 0 {
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
            for r in &rec[..n as usize] {
                if r.EventType as u32 != KEY_EVENT {
                    continue;
                }
                let k = r.Event.KeyEvent;
                if k.bKeyDown == 0 {
                    continue;
                }
                match char::from_u32(k.uChar.UnicodeChar as u32).map(|c| c.to_ascii_lowercase()) {
                    Some('q') => {
                        app.session.event("user", "stop requested (Q)");
                        app.stop.store(true, Ordering::SeqCst);
                    }
                    Some('m') | Some(' ') => marker.mark("console"),
                    _ if k.wVirtualKeyCode == 0x1b => {
                        app.session.event("user", "stop requested (Esc)");
                        app.stop.store(true, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
        }
    });
    Running::threads("console-input", vec![t])
}

/// Blocks until a key is pressed (used before exiting so a double-clicked console stays open).
pub fn wait_key() {
    unsafe {
        let inp = GetStdHandle(STD_INPUT_HANDLE);
        let mut rec: [INPUT_RECORD; 1] = std::mem::zeroed();
        loop {
            let mut n = 0u32;
            if ReadConsoleInputW(inp, rec.as_mut_ptr(), 1, &mut n) == 0 {
                return;
            }
            if n == 1 && rec[0].EventType as u32 == KEY_EVENT && rec[0].Event.KeyEvent.bKeyDown != 0 {
                return;
            }
        }
    }
}

/// Waits for one of `keys` (lower-case chars; Enter returns `enter` if given).
pub fn read_choice(keys: &str, enter: Option<char>) -> char {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    unsafe {
        let inp = GetStdHandle(STD_INPUT_HANDLE);
        let mut rec: [INPUT_RECORD; 1] = std::mem::zeroed();
        loop {
            let mut n = 0u32;
            if ReadConsoleInputW(inp, rec.as_mut_ptr(), 1, &mut n) == 0 {
                // No interactive console: never treat that as consent.
                return if keys.contains('n') { 'n' } else { 'q' };
            }
            if n != 1 || rec[0].EventType as u32 != KEY_EVENT || rec[0].Event.KeyEvent.bKeyDown == 0 {
                continue;
            }
            let k = rec[0].Event.KeyEvent;
            if k.wVirtualKeyCode == 0x0d {
                if let Some(c) = enter {
                    return c;
                }
            }
            if let Some(c) = char::from_u32(k.uChar.UnicodeChar as u32).map(|c| c.to_ascii_lowercase()) {
                if keys.contains(c) {
                    return c;
                }
            }
        }
    }
}

/// "Question? [Y/n]" (Enter = default). Prints the answer.
pub fn ask_yes_no(question: &str, default_yes: bool) -> bool {
    print!("{question} {} ", if default_yes { "[Y/n]" } else { "[y/N]" });
    let c = read_choice("yn", Some(if default_yes { 'y' } else { 'n' }));
    println!("{}", if c == 'y' { "yes" } else { "no" });
    c == 'y'
}

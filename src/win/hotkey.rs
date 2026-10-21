//! Global "lag spike" hotkey via RegisterHotKey (no keyboard hook). Each press writes a
//! mark and plays a short beep so the player knows it registered without alt-tabbing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Result, bail};
use windows_sys::Win32::System::Diagnostics::Debug::Beep;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PostThreadMessageW, WM_HOTKEY, WM_QUIT};

use super::{Running, spawn};
use crate::app::App;
use crate::row;
use crate::session::Sink;

/// Parses e.g. "F8", "Ctrl+Shift+M", "Pause", "ScrollLock", "Num5".
pub fn parse(spec: &str) -> Result<(u32, u32)> {
    let mut mods = 0;
    let mut vk = None;
    for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            "pause" | "break" => vk = Some(0x13),
            "scrolllock" | "scroll" => vk = Some(0x91),
            "insert" | "ins" => vk = Some(0x2d),
            "home" => vk = Some(0x24),
            "end" => vk = Some(0x23),
            "pageup" | "pgup" => vk = Some(0x21),
            "pagedown" | "pgdn" => vk = Some(0x22),
            p if p.starts_with('f') && p[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
                vk = Some(0x6f + p[1..].parse::<u32>().unwrap())
            }
            p if p.starts_with("num") && p[3..].parse::<u32>().is_ok_and(|n| n <= 9) => {
                vk = Some(0x60 + p[3..].parse::<u32>().unwrap())
            }
            p if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() => vk = Some(p.to_ascii_uppercase().as_bytes()[0] as u32),
            other => bail!("unknown key '{other}' in hotkey '{spec}'"),
        }
    }
    match vk {
        Some(vk) => Ok((mods, vk)),
        None => bail!("hotkey '{spec}' has no key"),
    }
}

pub struct Marker {
    app: Arc<App>,
    sink: Arc<Sink>,
}

impl Marker {
    pub fn new(app: Arc<App>) -> Arc<Self> {
        let sink = app.session.sink("marks", "t_us,source,n");
        Arc::new(Marker { app, sink })
    }

    pub fn mark(&self, source: &str) {
        let t = self.app.session.clock.now_us();
        let n = {
            let mut l = self.app.live.lock().unwrap();
            l.marks += 1;
            l.marks
        };
        row!(self.sink, "{t},{source},{n}");
        self.sink.flush();
        self.app.session.event("MARK", format!("#{n} lag spike marked ({source})"));
        spawn("beep", || unsafe {
            Beep(1400, 90);
        });
    }
}

pub fn start(app: Arc<App>, marker: Arc<Marker>) -> Result<Running> {
    let (mods, vk) = parse(&app.cfg.hotkey)?;
    let tid = Arc::new(AtomicU32::new(0));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let tid2 = tid.clone();
    let a = app.clone();
    let t = spawn("hotkey", move || unsafe {
        tid2.store(GetCurrentThreadId(), Ordering::SeqCst);
        if RegisterHotKey(std::ptr::null_mut(), 1, mods | MOD_NOREPEAT, vk) == 0 {
            let _ = tx.send(Err(format!(
                "RegisterHotKey({}) failed: {} (is another program using this key?)",
                a.cfg.hotkey,
                std::io::Error::last_os_error()
            )));
            return;
        }
        let _ = tx.send(Ok(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_HOTKEY {
                marker.mark("hotkey");
            }
        }
        UnregisterHotKey(std::ptr::null_mut(), 1);
    });
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = t.join();
            bail!(e)
        }
        Err(_) => bail!("hotkey thread died"),
    }
    let mut r = Running::threads("hotkey", vec![]);
    let tid3 = tid.clone();
    r.set_stop(move || unsafe {
        PostThreadMessageW(tid3.load(Ordering::SeqCst), WM_QUIT, 0, 0);
        let _ = t.join();
    });
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys() {
        assert_eq!(parse("F8").unwrap(), (0, 0x77));
        assert_eq!(parse("ctrl+shift+m").unwrap(), (MOD_CONTROL | MOD_SHIFT, b'M' as u32));
        assert_eq!(parse("Pause").unwrap(), (0, 0x13));
        assert!(parse("Ctrl").is_err());
    }
}

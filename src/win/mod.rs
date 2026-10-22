//! Windows collectors. Everything here is passive: no handles are ever opened to the game
//! process (it is protected by Easy Anti-Cheat); data comes from ETW, performance
//! counters, IP Helper, WLAN API and NVML.

pub mod capture;
pub mod console;
pub mod cpu;
pub mod drivers;
pub mod dump;
pub mod etw;
pub mod fixes;
pub mod frames;
pub mod gui;
pub mod gpu;
pub mod hotkey;
pub mod net;
pub mod probes;
pub mod procwatch;
pub mod stall;
pub mod sysinfo;
pub mod wifi;
pub mod wificheck;
pub mod wlan_etw;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::app::App;
use crate::session::Sink;

/// A started collector: an optional stop action plus the threads to join.
pub struct Running {
    stop: Option<Box<dyn FnOnce() + Send>>,
    /// Run `stop` after joining the threads (for resources the threads still use).
    stop_after_join: bool,
    threads: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn threads(_name: &'static str, threads: Vec<JoinHandle<()>>) -> Self {
        Running { stop: None, stop_after_join: false, threads }
    }

    /// Cleanup to run once the collector's threads have exited.
    pub fn set_stop(&mut self, f: impl FnOnce() + Send + 'static) {
        self.stop = Some(Box::new(f));
        self.stop_after_join = true;
    }

    pub fn add_thread(&mut self, t: JoinHandle<()>) {
        self.threads.push(t);
    }

    /// Runs `f` after the collector has fully stopped (after its existing stop action).
    pub fn on_stopped(&mut self, f: impl FnOnce() + Send + 'static) {
        let prev = self.stop.take();
        self.stop = Some(Box::new(move || {
            if let Some(p) = prev {
                p();
            }
            f();
        }));
    }

    pub fn stop(&mut self) {
        if !self.stop_after_join {
            if let Some(f) = self.stop.take() {
                f();
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Some(f) = self.stop.take() {
            f();
        }
    }
}

/// Wraps an ETW session: polls its loss counters every 5 s and stops it on shutdown.
fn etw_running(name: &'static str, app: Arc<App>, sess: etw::RealtimeSession, health: Arc<Sink>) -> Running {
    let filtered = sess.filtered.clone();
    app.session.event("collector", format!("{name}: ETW session running (event-id filter: {filtered:?})"));
    let sess = Arc::new(Mutex::new(sess));
    let s2 = sess.clone();
    let a2 = app.clone();
    let poller = spawn(&format!("{name}-health"), move || {
        let mut last_lost = 0;
        while a2.sleep_ms(5000) {
            net::poll_health(&a2, name, &s2, &health, &mut last_lost);
        }
    });
    Running {
        stop: Some(Box::new(move || sess.lock().unwrap().stop())),
        stop_after_join: false,
        threads: vec![poller],
    }
}

pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new().name(name.to_string()).spawn(f).expect("spawn thread")
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

pub fn from_wide_buf(b: &[u16]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..n])
}

/// The Windows directory (e.g. C:\Windows), from the OS rather than the environment.
pub fn windows_dir() -> std::path::PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return "C:\\Windows".into();
    }
    from_wide_buf(&buf[..n as usize]).into()
}

/// Full path of a program in System32. We run elevated, so never resolve tools through
/// the search path (a planted netsh.exe next to lagscope.exe would otherwise run as admin).
pub fn system32(rel: &str) -> std::path::PathBuf {
    windows_dir().join("System32").join(rel)
}

/// Restrict DLL loading for the rest of the process to System32 (no DLLs from the exe's
/// folder or the current directory).
/// Opens a file or folder with its default program (report in the browser, folder in
/// Explorer).
pub fn open_path(p: &std::path::Path) {
    let _ = std::process::Command::new(windows_dir().join("explorer.exe")).arg(p).spawn();
}

pub fn harden_dll_search() {
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut el = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            tok,
            TokenElevation,
            &mut el as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(tok);
        ok != 0 && el.TokenIsElevated != 0
    }
}

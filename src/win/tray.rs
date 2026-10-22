//! Notification-area (tray) icon: a hidden window on its own thread that owns the icon,
//! shows its menu (Open / Auto-record / Exit) and pops up notifications. Events go to the
//! GUI through a callback; the GUI talks to it through `Tray`.

use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::wide;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrayEvent {
    Open,
    ToggleAuto,
    Exit,
}

const WM_TRAY: u32 = WM_APP + 1;
const WM_UPDATE: u32 = WM_APP + 2;
const ID_OPEN: usize = 1;
const ID_AUTO: usize = 2;
const ID_EXIT: usize = 3;

#[derive(Default)]
struct Inner {
    hwnd: isize,
    auto: bool,
    tip: String,
    /// Pending balloon (title, text).
    balloon: Option<(String, String)>,
    remove: bool,
}

type Callback = Box<dyn Fn(TrayEvent) + Send + Sync>;
static STATE: OnceLock<(Arc<Mutex<Inner>>, Callback)> = OnceLock::new();
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

#[derive(Clone)]
pub struct Tray(Arc<Mutex<Inner>>);

fn copy_w(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// The same scope icon as the window, as a 32x32 HICON.
fn make_icon() -> HICON {
    let n = 32usize;
    let mut bgra = vec![0u8; n * n * 4];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let blip = ((x as f32 - 21.0).powi(2) + (y as f32 - 11.0).powi(2)).sqrt();
            let px: [u8; 4] = if blip < 3.2 {
                [80, 90, 255, 255]
            } else if (d - 14.0).abs() < 1.6 || (dx.abs() < 0.7 && d < 14.0) || (dy.abs() < 0.7 && d < 14.0) {
                [255, 200, 90, 255]
            } else if d < 15.0 {
                [44, 30, 18, 255]
            } else {
                [0, 0, 0, 0]
            };
            bgra[(y * n + x) * 4..(y * n + x) * 4 + 4].copy_from_slice(&px);
        }
    }
    let and = vec![0u8; n * n / 8];
    unsafe { CreateIcon(GetModuleHandleW(std::ptr::null()), n as i32, n as i32, 1, 32, and.as_ptr(), bgra.as_ptr()) }
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

fn add_icon(hwnd: HWND, tip: &str) {
    let mut d = nid(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = make_icon();
    copy_w(&mut d.szTip, tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &d) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some((state, cb)) = STATE.get() else { return unsafe { DefWindowProcW(hwnd, msg, wp, lp) } };
    if Some(&msg) == TASKBAR_CREATED.get() {
        // Explorer restarted: put the icon back.
        let tip = state.lock().unwrap().tip.clone();
        add_icon(hwnd, &tip);
        return 0;
    }
    match msg {
        WM_TRAY => {
            let ev = (lp & 0xffff) as u32;
            if ev == WM_LBUTTONUP || ev == WM_LBUTTONDBLCLK {
                cb(TrayEvent::Open);
            } else if ev == WM_RBUTTONUP {
                unsafe {
                    let menu = CreatePopupMenu();
                    let auto = state.lock().unwrap().auto;
                    AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Open LagScope").as_ptr());
                    AppendMenuW(menu, MF_STRING | if auto { MF_CHECKED } else { MF_UNCHECKED }, ID_AUTO, wide("Auto-record games").as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                    AppendMenuW(menu, MF_STRING, ID_EXIT, wide("Exit").as_ptr());
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    // Needed so the menu closes when clicking elsewhere.
                    SetForegroundWindow(hwnd);
                    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    DestroyMenu(menu);
                    match cmd as usize {
                        ID_OPEN => cb(TrayEvent::Open),
                        ID_AUTO => cb(TrayEvent::ToggleAuto),
                        ID_EXIT => cb(TrayEvent::Exit),
                        _ => {}
                    }
                }
            }
            0
        }
        WM_UPDATE => {
            let mut s = state.lock().unwrap();
            if s.remove {
                unsafe {
                    Shell_NotifyIconW(NIM_DELETE, &nid(hwnd));
                    DestroyWindow(hwnd);
                }
                return 0;
            }
            let mut d = nid(hwnd);
            d.uFlags = NIF_TIP;
            copy_w(&mut d.szTip, &s.tip);
            if let Some((title, text)) = s.balloon.take() {
                d.uFlags |= NIF_INFO;
                copy_w(&mut d.szInfoTitle, &title);
                copy_w(&mut d.szInfo, &text);
                d.dwInfoFlags = NIIF_INFO;
            }
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

impl Tray {
    /// Creates the icon (once per process). `on_event` runs on the tray thread.
    pub fn start(tip: &str, auto: bool, on_event: impl Fn(TrayEvent) + Send + Sync + 'static) -> Option<Tray> {
        let inner = Arc::new(Mutex::new(Inner { auto, tip: tip.into(), ..Default::default() }));
        STATE.set((inner.clone(), Box::new(on_event))).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let tip = tip.to_string();
        let i2 = inner.clone();
        std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || unsafe {
                let class = wide("LagScopeTray");
                let hinst = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
                RegisterClassW(&wc);
                let _ = TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
                let hwnd = CreateWindowExW(0, class.as_ptr(), wide("LagScope").as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null());
                if hwnd.is_null() {
                    let _ = tx.send(false);
                    return;
                }
                i2.lock().unwrap().hwnd = hwnd as isize;
                add_icon(hwnd, &tip);
                let _ = tx.send(true);
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .ok()?;
        rx.recv().ok().filter(|ok| *ok).map(|_| Tray(inner))
    }

    fn poke(&self) {
        let h = self.0.lock().unwrap().hwnd;
        if h != 0 {
            unsafe { PostMessageW(h as HWND, WM_UPDATE, 0, 0) };
        }
    }

    pub fn notify(&self, title: &str, text: &str) {
        self.0.lock().unwrap().balloon = Some((title.into(), text.into()));
        self.poke();
    }

    pub fn set_state(&self, tip: &str, auto: bool) {
        {
            let mut s = self.0.lock().unwrap();
            if s.tip == tip && s.auto == auto {
                return;
            }
            s.tip = tip.into();
            s.auto = auto;
        }
        self.poke();
    }

    pub fn remove(&self) {
        self.0.lock().unwrap().remove = true;
        self.poke();
    }
}

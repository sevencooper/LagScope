//! Console output. During a capture, collectors never write to stdout themselves: they
//! queue log lines and a single UI thread prints them above a status block that is
//! redrawn in place with VT escape sequences. That way a blocked console (e.g. a user
//! selecting text) can never stall a collector.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static PENDING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DRAWN: Mutex<usize> = Mutex::new(0);

type LogSink = Box<dyn Fn(String) + Send + Sync>;
static SINK: std::sync::OnceLock<LogSink> = std::sync::OnceLock::new();

/// Sends every log line to `f` instead of the console (the GUI).
pub fn set_sink(f: impl Fn(String) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(f));
}

pub fn log(line: impl Into<String>) {
    let line = line.into();
    if let Some(s) = SINK.get() {
        s(line);
        return;
    }
    if !ACTIVE.load(Ordering::Relaxed) {
        println!("{line}");
        return;
    }
    let mut q = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() < 2000 {
        q.push_back(line);
    }
}

/// Switches to queued mode (call from the UI thread owner before collectors start).
pub fn activate() {
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Prints queued log lines and redraws the status block. Called only by the UI thread.
pub fn render(status: &[String]) {
    let lines: Vec<String> = PENDING.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
    let mut drawn = DRAWN.lock().unwrap_or_else(|e| e.into_inner());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if *drawn > 0 {
        // Move to the start of the first status line, then clear to end of screen.
        let _ = write!(out, "\x1b[{}F\x1b[0J", *drawn);
    }
    for l in &lines {
        let _ = writeln!(out, "{l}");
    }
    for l in status {
        let _ = writeln!(out, "{l}");
    }
    *drawn = status.len();
    let _ = out.flush();
}

/// Flushes pending lines, removes the status block and returns to direct printing.
pub fn deactivate() {
    render(&[]);
    ACTIVE.store(false, Ordering::Relaxed);
}

pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const CYAN: &str = "\x1b[36m";
pub const RESET: &str = "\x1b[0m";

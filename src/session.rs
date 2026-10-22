//! Session directory and its CSV streams. Each collector owns one or more `Sink`s and
//! writes rows directly; a flusher thread pushes buffers to disk every second so a crash
//! or a closed console loses at most ~1 s of data.

use std::fmt::Display;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::clock::{self, Clock};

pub struct Sink {
    /// None once closed (late rows are dropped).
    w: Mutex<Option<BufWriter<File>>>,
}

impl Sink {
    pub fn row(&self, line: std::fmt::Arguments) {
        if let Ok(mut g) = self.w.lock() {
            if let Some(w) = g.as_mut() {
                let _ = w.write_fmt(line);
                let _ = w.write_all(b"\n");
            }
        }
    }

    pub fn flush(&self) {
        if let Ok(mut g) = self.w.lock() {
            if let Some(w) = g.as_mut() {
                let _ = w.flush();
            }
        }
    }

    fn close(&self) {
        if let Ok(mut g) = self.w.lock() {
            *g = None;
        }
    }
}

/// Writes a CSV row: `row!(sink, "{},{}", a, b)`.
#[macro_export]
macro_rules! row {
    ($sink:expr, $($arg:tt)*) => { $sink.row(format_args!($($arg)*)) };
}

pub struct Session {
    pub dir: PathBuf,
    pub clock: Clock,
    pub start_unix_ms: i64,
    sinks: Mutex<Vec<Arc<Sink>>>,
    events: Arc<Sink>,
    meta: Mutex<Map<String, Value>>,
}

impl Session {
    pub fn create(base: &Path) -> Result<Session> {
        let clock = Clock::new();
        let start_unix_ms = clock::unix_ms();
        let utc_offset_min = clock::local_offset_min();
        let name = format!("LagScope_{}", clock::format_compact(start_unix_ms, utc_offset_min));
        let dir = base.join(&name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let events = Arc::new(open_sink(&dir, "events", "t_us,kind,detail")?);
        let s = Session {
            dir,
            clock,
            start_unix_ms,
            sinks: Mutex::new(vec![events.clone()]),
            events,
            meta: Mutex::new(Map::new()),
        };
        s.meta_set("format_version", 1.into());
        s.meta_set("tool_version", env!("CARGO_PKG_VERSION").into());
        s.meta_set("start_unix_ms", start_unix_ms.into());
        s.meta_set("utc_offset_min", utc_offset_min.into());
        s.meta_set("start_local", clock::format_wall(start_unix_ms, utc_offset_min).into());
        s.meta_set("qpc_freq", clock.freq().into());
        Ok(s)
    }

    pub fn sink(&self, name: &str, header: &str) -> Arc<Sink> {
        let sink = match open_sink(&self.dir, name, header) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                crate::ui::log(format!("cannot create {name}.csv: {e:#}"));
                // Fall back to a sink that writes to the null device so collectors keep running.
                Arc::new(Sink {
                    w: Mutex::new(Some(BufWriter::new(File::create(null_path()).expect("null device")))),
                })
            }
        };
        self.sinks.lock().unwrap().push(sink.clone());
        sink
    }

    /// Records a timestamped tool event (also echoed to the console).
    pub fn event(&self, kind: &str, detail: impl Display) {
        let detail = detail.to_string();
        let t = self.clock.now_us();
        self.events.row(format_args!("{t},{kind},{}", csv_escape(&detail)));
        crate::ui::log(format!("[{:>7.1}s] {kind}: {detail}", t as f64 / 1e6));
    }

    pub fn meta_set(&self, key: &str, value: Value) {
        self.meta.lock().unwrap().insert(key.to_string(), value);
    }

    pub fn meta_push(&self, key: &str, value: Value) {
        let mut m = self.meta.lock().unwrap();
        match m.entry(key.to_string()).or_insert_with(|| Value::Array(vec![])) {
            Value::Array(a) => a.push(value),
            other => *other = Value::Array(vec![value]),
        }
    }

    pub fn flush_all(&self) {
        for s in self.sinks.lock().unwrap().iter() {
            s.flush();
        }
    }

    /// Closes every CSV file (so the session folder can be deleted).
    pub fn close_all(&self) {
        for s in self.sinks.lock().unwrap().iter() {
            s.close();
        }
    }

    pub fn write_meta(&self) -> Result<()> {
        let m = self.meta.lock().unwrap().clone();
        let text = serde_json::to_string_pretty(&Value::Object(m))?;
        std::fs::write(self.dir.join("meta.json"), text)?;
        Ok(())
    }

    pub fn write_text(&self, rel: &str, text: &str) {
        let p = self.dir.join(rel);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(p, text);
    }
}

fn open_sink(dir: &Path, name: &str, header: &str) -> Result<Sink> {
    let f = File::create(dir.join(format!("{name}.csv")))?;
    let mut w = BufWriter::with_capacity(256 * 1024, f);
    writeln!(w, "{header}")?;
    Ok(Sink { w: Mutex::new(Some(w)) })
}

fn null_path() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

pub fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\"").replace(['\n', '\r'], " "))
    } else {
        s.to_string()
    }
}

/// Zips every file in `dir` into `<dir>.zip` next to it.
pub fn zip_dir(dir: &Path) -> Result<PathBuf> {
    use zip::write::SimpleFileOptions;
    let zip_path = dir.with_extension("zip");
    let f = File::create(&zip_path)?;
    let mut zw = zip::ZipWriter::new(BufWriter::new(f));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let root_name = dir.file_name().unwrap().to_string_lossy().to_string();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(dir)?.to_string_lossy().replace('\\', "/");
            zw.start_file(format!("{root_name}/{rel}"), opts)?;
            let mut src = File::open(&p)?;
            std::io::copy(&mut src, &mut zw)?;
        }
    }
    zw.finish()?.flush()?;
    Ok(zip_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}

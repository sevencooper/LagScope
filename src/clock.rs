//! Session timeline. Every record in a session is stamped in microseconds since session
//! start, derived from QueryPerformanceCounter. ETW sessions are created with
//! ClientContext=1 and consumed with PROCESS_TRACE_MODE_RAW_TIMESTAMP, so ETW event
//! timestamps are raw QPC values on the very same clock.

#[derive(Clone, Copy, Debug)]
pub struct Clock {
    origin: i64,
    freq: i64,
}

impl Clock {
    pub fn new() -> Self {
        Clock { origin: raw_now(), freq: raw_freq() }
    }

    pub fn freq(&self) -> i64 {
        self.freq
    }

    /// Converts a raw QPC value (e.g. an ETW event timestamp) to session microseconds.
    pub fn us_from_raw(&self, raw: i64) -> i64 {
        ((raw - self.origin) as i128 * 1_000_000 / self.freq as i128) as i64
    }

    pub fn now_us(&self) -> i64 {
        self.us_from_raw(raw_now())
    }

    pub fn raw_to_us_delta(&self, delta: i64) -> i64 {
        (delta as i128 * 1_000_000 / self.freq as i128) as i64
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
pub fn raw_now() -> i64 {
    let mut v = 0i64;
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut v) };
    v
}

#[cfg(windows)]
fn raw_freq() -> i64 {
    let mut v = 0i64;
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut v) };
    v
}

#[cfg(not(windows))]
pub fn raw_now() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_nanos() as i64
}

#[cfg(not(windows))]
fn raw_freq() -> i64 {
    1_000_000_000
}

/// Unix time in milliseconds (wall clock), used only to anchor the session timeline.
pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Formats a unix-ms timestamp shifted by `offset_min` as `YYYY-MM-DD HH:MM:SS.mmm`.
pub fn format_wall(unix_ms: i64, offset_min: i32) -> String {
    let ms = unix_ms + offset_min as i64 * 60_000;
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{millis:03}",
        sod / 3600,
        (sod / 60) % 60,
        sod % 60
    )
}

/// Compact form for directory names: `YYYYMMDD_HHMMSS`.
pub fn format_compact(unix_ms: i64, offset_min: i32) -> String {
    let s = format_wall(unix_ms, offset_min);
    format!("{}{}{}_{}{}{}", &s[0..4], &s[5..7], &s[8..10], &s[11..13], &s[14..16], &s[17..19])
}

// Howard Hinnant's days-from-civil inverse.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Local UTC offset in minutes (Windows only; 0 elsewhere).
#[cfg(windows)]
pub fn local_offset_min() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let bias = match GetTimeZoneInformation(&mut tz) {
            1 => tz.Bias + tz.StandardBias, // TIME_ZONE_ID_STANDARD
            2 => tz.Bias + tz.DaylightBias, // TIME_ZONE_ID_DAYLIGHT
            _ => tz.Bias,
        };
        -bias
    }
}

#[cfg(not(windows))]
pub fn local_offset_min() -> i32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_format() {
        // 2026-10-20 19:25:00 UTC
        assert_eq!(format_wall(1_792_524_300_000, 0), "2026-10-20 19:25:00.000");
        assert_eq!(format_compact(1_792_524_300_000, 120), "20261020_212500");
    }
}

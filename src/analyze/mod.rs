//! Offline analysis of a recorded session (platform independent, so recordings from
//! a friend's PC can be re-analysed anywhere with `lagscope analyze <dir>`).

pub mod detect;
pub mod experiments;
pub mod findings;
pub mod load;
pub mod report;

use std::path::Path;

use anyhow::{Result, bail};

pub fn run(dir: &Path) -> Result<String> {
    if !dir.is_dir() {
        bail!("{} is not a session directory", dir.display());
    }
    let data = load::load(dir)?;
    let analysis = detect::analyze(&data);
    std::fs::write(dir.join("report.html"), report::render(&data, &analysis))?;
    std::fs::write(dir.join("analysis.json"), serde_json::to_string_pretty(&analysis)?)?;

    let mut out = String::new();
    out.push_str(&format!("\n== {} ==\n", analysis.headline));
    for v in &analysis.verdicts {
        out.push_str(&format!("  mark #{} at {:.1}s [{}]: {}\n", v.n, v.t as f64 / 1e6, v.confidence, v.culprit));
    }
    for f in &analysis.findings {
        out.push_str(&format!("  - {f}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::detect::{Kind, analyze, periodicity};
    use super::load;
    use std::fmt::Write as _;
    use std::path::PathBuf;

    /// Builds a synthetic session: 60 Hz server stream, client stream, 20 Hz probes, 144 fps
    /// frames, with injected events.
    struct Synth {
        dir: PathBuf,
        packets: String,
        probes: String,
        frames: String,
        marks: String,
        wlan: String,
        stalls: String,
    }

    impl Synth {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("lagscope_test_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Synth {
                dir,
                packets: "t_us,dir,proto,size,remote_ip,remote_port,local_port\n".into(),
                probes: "t_us,target,ip,status,rtt_us,ttl\n".into(),
                frames: "t_us,present_us,sync_interval,flags,swapchain,result\n".into(),
                marks: "t_us,source,n\n".into(),
                wlan: "t_us,provider,id,event,values\n".into(),
                stalls: "t_us,late_us\n".into(),
            }
        }

        /// `in_hole(t)` / `gw_rtt(t)` / `frame_hole(t)` customise the stream.
        fn fill(&mut self, secs: i64, in_hole: impl Fn(i64) -> bool, gw_rtt: impl Fn(i64) -> i64, frame_hole: impl Fn(i64) -> bool) {
            let end = secs * 1_000_000;
            let mut t = 0;
            let mut held = 0;
            while t < end {
                if in_hole(t) {
                    held += 1;
                } else {
                    // Delayed packets are released together at the end of the hole.
                    for _ in 0..=held {
                        writeln!(self.packets, "{t},R,U,300,203.0.113.5,7777,50000").unwrap();
                    }
                    held = 0;
                }
                writeln!(self.packets, "{},S,U,120,203.0.113.5,7777,50000", t + 3000).unwrap();
                t += 16_667;
            }
            let mut t = 0;
            while t < end {
                for (tg, base) in [("gateway", 2_000), ("hop2", 4_000), ("internet", 12_000), ("server", 30_000)] {
                    let rtt = if tg == "gateway" { gw_rtt(t) } else { base };
                    let rtt = if tg != "gateway" && gw_rtt(t) > 20_000 { rtt + gw_rtt(t) } else { rtt };
                    writeln!(self.probes, "{t},{tg},10.0.0.1,0,{rtt},64").unwrap();
                }
                t += 50_000;
            }
            let mut t = 0;
            while t < end {
                if !frame_hole(t) {
                    writeln!(self.frames, "{t},500,0,0,1,0").unwrap();
                }
                t += 6_944;
            }
        }

        fn write(&self) {
            let w = |n: &str, s: &str| std::fs::write(self.dir.join(n), s).unwrap();
            w("game_packets.csv", &self.packets);
            w("probes.csv", &self.probes);
            w("frames.csv", &self.frames);
            w("marks.csv", &self.marks);
            w("wlan_etw.csv", &self.wlan);
            w("stalls.csv", &self.stalls);
            w("meta.json", r#"{"route":{"is_wifi":true},"config":{"processes":["Discovery.exe"]},"elevated":true}"#);
        }
    }

    #[test]
    fn wifi_spikes_every_60s_are_attributed_to_wifi() {
        let mut s = Synth::new("wifi");
        // Every 60 s starting at 30 s: a 250 ms hole in server packets, gateway RTT spikes,
        // and a Wi-Fi scan event.
        let hole = |t: i64| t > 20_000_000 && (t - 30_000_000).rem_euclid(60_000_000) < 250_000;
        s.fill(300, hole, |t| if hole(t) { 180_000 } else { 2_000 }, |_| false);
        for k in 0..5 {
            let t = 30_000_000 + k * 60_000_000;
            writeln!(s.wlan, "{},autoconfig,20024,Number of Unique Wlan Networks,Count=12", t - 400_000).unwrap();
            writeln!(s.marks, "{},hotkey,{}", t + 1_200_000, k + 1).unwrap();
        }
        s.write();
        let d = load::load(&s.dir).unwrap();
        let a = analyze(&d);
        assert!(a.stream.gaps_100ms >= 4, "gaps: {}", a.stream.gaps_100ms);
        assert!(a.stream.burst_gaps >= 4, "delayed packets should be detected as bursts");
        assert!(a.verdicts.iter().all(|v| v.category == "wifi"), "{:#?}", a.verdicts);
        assert!(a.gap_correlation["gateway (Wi-Fi hop) spike/loss"] > 0.9);
        let p = a.periodicity.iter().find(|p| p.what.contains("inbound")).expect("periodicity");
        assert!((p.period_s - 60.0).abs() < 1.5, "period {}", p.period_s);
        assert!(a.headline.contains("Wi-Fi"), "{}", a.headline);
        super::run(&s.dir).unwrap();
        assert!(s.dir.join("report.html").exists());
    }

    #[test]
    fn server_side_stall_with_clean_pings() {
        let mut s = Synth::new("server");
        let hole = |t: i64| (t - 40_000_000).rem_euclid(45_000_000) < 300_000 && t > 30_000_000;
        // Packets are *lost* (not delayed): don't release them.
        s.fill(200, |_| false, |_| 2_000, |_| false);
        s.packets = s
            .packets
            .lines()
            .filter(|l| {
                let mut c = l.split(',');
                let t: i64 = c.next().unwrap().parse().unwrap_or(-1);
                !(c.next() == Some("R") && hole(t))
            })
            .map(|l| format!("{l}\n"))
            .collect();
        writeln!(s.marks, "{},hotkey,1", 41_000_000).unwrap();
        s.write();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert_eq!(a.verdicts[0].category, "server", "{:#?}", a.verdicts[0]);
        assert_eq!(a.stream.burst_gaps, 0);
    }

    #[test]
    fn local_frame_hitch() {
        let mut s = Synth::new("local");
        let fh = |t: i64| t > 50_000_000 && t < 50_400_000;
        s.fill(100, |_| false, |_| 2_000, fh);
        writeln!(s.stalls, "50100000,25000").unwrap();
        writeln!(s.marks, "{},hotkey,1", 51_500_000).unwrap();
        s.write();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert_eq!(a.verdicts[0].category, "local", "{:#?}", a.verdicts[0]);
        assert!(a.verdicts[0].culprit.contains("stall"));
        assert!(a.anomalies.iter().any(|x| x.kind == Kind::FrameHitch && x.value > 300.0));
    }

    #[test]
    fn latency_plateau_detected() {
        let mut s = Synth::new("plateau");
        s.fill(60, |_| false, |t| if t > 20_000_000 && t < 30_000_000 { 90_000 } else { 2_000 }, |_| false);
        s.write();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert!(a.anomalies.iter().any(|x| x.kind == Kind::RttPlateau && x.target == "gateway"));
    }

    #[test]
    fn periodicity_basic() {
        let t: Vec<i64> = (0..10).map(|k| 5_000_000 + k * 30_000_000).collect();
        let (p, s, _) = periodicity(&t, 320_000_000).unwrap();
        assert!((p - 30.0).abs() < 1.0 && s >= 0.85, "{p} {s}");
        let random = [1, 7, 8, 30, 31, 77, 140, 141].map(|x: i64| x * 1_000_000);
        assert!(periodicity(&random, 200_000_000).is_none_or(|(_, s, _)| s < 0.9));
        // An exact 10 s timer with a few events missing must not be reported as a multiple.
        let t: Vec<i64> = (0..56).filter(|k| k % 9 != 4).map(|k| 2_050_000 + k * 10_000_000 + (k * 7919 % 40_000)).collect();
        let (p, _, _) = periodicity(&t, 560_000_000).unwrap();
        assert!((p - 10.0).abs() < 0.1, "{p}");
    }
}

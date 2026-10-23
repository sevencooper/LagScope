//! Offline analysis of a recorded session (platform independent, so recordings from
//! a friend's PC can be re-analysed anywhere with `lagscope analyze <dir>`).

pub mod compare;
pub mod detect;
pub mod experiments;
pub mod findings;
pub mod hardware;
pub mod health;
pub mod load;
pub mod report;
pub mod share;

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
    share::write_ai_guide(dir)?;

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
        assert!(a.gap_correlation[super::detect::GW_KEY] > 0.9);
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
    fn driver_hiccups_are_tied_to_stalls_and_frames() {
        let mut s = Synth::new("driver");
        // Every 20 s: a 4 ms Wi-Fi driver DPC, a 150 ms frame hitch and a 150 ms packet hole.
        let hole = |t: i64| t > 10_000_000 && (t - 10_000_000).rem_euclid(20_000_000) < 150_000;
        s.fill(120, hole, |_| 2_000, hole);
        s.write();
        let mut long = String::from("t_us,kind,driver,dur_us,cpu\n");
        let mut per_s = String::from("t_us,driver,dpc_n,dpc_total_us,dpc_max_us,isr_n,isr_total_us,isr_max_us\n");
        for k in 0..6 {
            let t = 10_000_000 + k * 20_000_000;
            writeln!(long, "{},D,wifidrv.sys,4000,0", t + 20_000).unwrap();
        }
        for sec in 0..120 {
            writeln!(per_s, "{},_all,1000,50000,{},0,0,0", sec * 1_000_000, if (sec - 10) % 20 == 0 { 4000 } else { 200 }).unwrap();
        }
        std::fs::write(s.dir.join("driver_long.csv"), long).unwrap();
        std::fs::write(s.dir.join("drivers_1s.csv"), per_s).unwrap();
        let meta = r#"{"route":{"is_wifi":true},"config":{"processes":["Discovery.exe"]},"elevated":true,
            "driver_latency":{"drivers":[{"driver":"wifidrv.sys","dpc_n":100,"dpc_total_ms":30,"dpc_max_us":4000,"isr_n":0,"isr_total_ms":0,"isr_max_us":0,"hist_dpc":[0,0,0,0,0,0,6,0,0],"hist_isr":[0,0,0,0,0,0,0,0,0]}]}}"#;
        std::fs::write(s.dir.join("meta.json"), meta).unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert_eq!(a.anomalies.iter().filter(|x| x.kind == Kind::DriverLatency).count(), 6);
        assert!(a.gap_correlation["driver DPC/ISR ≥1 ms"] > 0.8, "{:?}", a.gap_correlation);
        assert!(a.findings.iter().any(|f| f.contains("high (") && f.contains("wifidrv.sys")), "{:#?}", a.findings);
        assert!(a.findings.iter().any(|f| f.contains("line up with network stalls")), "{:#?}", a.findings);
        assert!(a.recommendations.iter().any(|r| r.contains("wifidrv.sys")));
        super::run(&s.dir).unwrap();
    }

    #[test]
    fn frame_hitches_from_disk_streaming() {
        let mut s = Synth::new("disk");
        // 150 ms frame hitches every 15 s, each in a second where the game page-faults 30 MB.
        let hitch = |t: i64| t > 5_000_000 && (t - 5_000_000).rem_euclid(15_000_000) < 150_000;
        s.fill(120, |_| false, |_| 2_000, hitch);
        s.write();
        let mut disk = String::from("t_us,reads,read_mb,writes,write_mb,max_io_ms,game_hard_faults,game_fault_mb\n");
        for sec in 0..120i64 {
            let busy = sec >= 5 && (sec - 5) % 15 == 0;
            writeln!(disk, "{},{},{},5,1.0,{},{},{}", sec * 1_000_000, if busy { 900 } else { 20 }, if busy { 60.0 } else { 1.0 }, if busy { 40 } else { 2 }, if busy { 3000 } else { 0 }, if busy { 30.0 } else { 0.0 }).unwrap();
        }
        std::fs::write(s.dir.join("disk_1s.csv"), disk).unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert!(a.hitch_correlation["game reading from disk"] > 0.8, "{:?}", a.hitch_correlation);
        assert!(a.recommendations.iter().any(|r| r.contains("SSD")), "{:#?}", a.recommendations);
        super::run(&s.dir).unwrap();
    }

    #[test]
    fn pc_side_checks_and_scorecard() {
        let mut s = Synth::new("pcside");
        // 150 ms hitches every 15 s while Defender is busy; plenty of RAM; 60 Hz on a 144 Hz monitor.
        let hitch = |t: i64| t > 5_000_000 && (t - 5_000_000).rem_euclid(15_000_000) < 150_000;
        s.fill(120, |_| false, |_| 2_000, hitch);
        s.write();
        let mut pc = String::from("t_us,name,cpu_pct\n");
        for sec in 0..120i64 {
            writeln!(pc, "{},chrome.exe,12", sec * 1_000_000).unwrap();
            if sec >= 5 && (sec - 5) % 15 == 0 {
                writeln!(pc, "{},MsMpEng.exe,95", sec * 1_000_000 + 500_000).unwrap();
            }
        }
        std::fs::write(s.dir.join("proc_cpu.csv"), pc).unwrap();
        let mut cpu = String::from("t_us,total_pct,max_core_pct,avail_mb,perf_limit_pct,commit_pct\n");
        for q in 0..480i64 {
            // Throttled (30% below full speed) half the time.
            writeln!(cpu, "{},20,60,{},{},40", q * 250_000, 9000 - q, if q % 2 == 0 { 70 } else { 100 }).unwrap();
        }
        std::fs::write(s.dir.join("cpu.csv"), cpu).unwrap();
        std::fs::write(s.dir.join("link.csv"), "t_us,up,rx_mbps,tx_mbps,in_errors,in_discards,out_errors,out_discards\n0,1,866,866,0,0,0,0\n60000000,0,0,0,0,0,0,0\n61000000,1,866,866,0,0,0,0\n").unwrap();
        std::fs::write(
            s.dir.join("meta.json"),
            r#"{"route":{"is_wifi":true},"config":{"processes":["Discovery.exe"]},"elevated":true,"ram_mb":16384,
               "displays":[{"monitor":"","primary":true,"width":2560,"height":1440,"hz":60,"max_hz":144}],
               "power":{"on_battery":false,"battery_saver":false,"has_battery":true,"battery_pct":90},"power_plan":"Balanced"}"#,
        )
        .unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert_eq!(a.hitch_procs.first().map(|x| x.0.as_str()), Some("MsMpEng.exe"), "{:?}", a.hitch_procs);
        let rec = a.recommendations.join("\n");
        assert!(rec.contains("Defender"), "{rec}");
        assert!(rec.contains("144 Hz"), "{rec}");
        assert!(rec.contains("throttled"), "{rec}");
        assert!(rec.contains("Wi-Fi connection dropped"), "{rec}");
        let st = |area: &str| a.health.iter().find(|h| h.area == area).map(|h| h.status.clone()).unwrap_or_default();
        assert_eq!(st("CPU"), "warn", "{:#?}", a.health);
        assert_eq!(st("Display & power"), "warn");
        assert_eq!(st("Wi-Fi"), "warn");
        assert_eq!(st("Memory"), "ok");
        assert_eq!(st("Disk"), "na");
        assert!(a.background_cpu.iter().any(|b| b.0 == "chrome.exe" && (b.1 - 12.0).abs() < 1.5), "{:?}", a.background_cpu);
    }

    #[test]
    fn wired_pc_is_not_blamed_on_wifi() {
        // The same 60 s router freezes as above, on a cable, with an idle Wi-Fi adapter
        // scanning at those moments and a second router in front of the PC's.
        let mut s = Synth::new("wired");
        let hole = |t: i64| t > 20_000_000 && (t - 30_000_000).rem_euclid(60_000_000) < 250_000;
        s.fill(300, hole, |t| if hole(t) { 180_000 } else { 2_000 }, |_| false);
        for k in 0..5 {
            let t = 30_000_000 + k * 60_000_000;
            writeln!(s.wlan, "{},autoconfig,20024,Number of Unique Wlan Networks,Count=12", t - 400_000).unwrap();
            writeln!(s.marks, "{},hotkey,{}", t + 1_200_000, k + 1).unwrap();
        }
        s.write();
        std::fs::write(
            s.dir.join("meta.json"),
            r#"{"route":{"is_wifi":false},"config":{"processes":["Discovery.exe"]},"elevated":true,"logical_cpus":32,"inner_routers":["10.0.0.1"]}"#,
        )
        .unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert!(a.verdicts.iter().all(|v| v.category == "lan"), "{:#?}", a.verdicts);
        assert!(!a.anomalies.iter().any(|x| x.kind == Kind::WifiScan));
        assert!(!a.headline.contains("Wi-Fi"), "{}", a.headline);
        assert!(a.recommendations.iter().any(|r| r.contains("powerline")), "{:#?}", a.recommendations);
        assert!(a.findings.iter().any(|f| f.contains("double NAT")));
        assert_eq!(a.health[0].area, "Network link");
    }

    #[test]
    fn upload_freezes_on_a_second_timer() {
        // Every 10 s at x.88 the router (and everything behind it) answers 220 ms late, while
        // the game's incoming stream keeps flowing: the PC's uploads are held back. One mark
        // right after a freeze, one while switched away from the game.
        let mut s = Synth::new("upload");
        let frozen = |t: i64| t > 10_000_000 && (t - 880_000).rem_euclid(10_000_000) < 50_000;
        s.fill(120, |_| false, |t| if frozen(t) { 220_000 } else { 2_000 }, |_| false);
        writeln!(s.marks, "61300000,hotkey,1").unwrap();
        writeln!(s.marks, "96000000,hotkey,2").unwrap();
        s.write();
        std::fs::write(s.dir.join("focus.csv"), "t_us,game,process\n0,1,game.exe\n94000000,0,explorer.exe\n97000000,1,game.exe\n").unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        let ups = a.anomalies.iter().filter(|x| x.kind == Kind::UploadStall).count();
        assert!(ups >= 9, "upload freezes: {ups}");
        assert_eq!(a.stream.gaps_100ms, 0);
        assert!(a.second_timer.is_some_and(|(k, n)| k * 10 >= n * 9), "{:?}", a.second_timer);
        assert_eq!(a.verdicts[0].category, "wifi", "{:#?}", a.verdicts[0]);
        assert!(a.verdicts[0].culprit.contains("uploads froze"), "{}", a.verdicts[0].culprit);
        assert_eq!(a.verdicts[1].category, "away");
        assert!(a.headline.starts_with("1 of 1 marked") && a.headline.contains("1 more marked while switched away"), "{}", a.headline);
        assert!(a.health[0].text.contains("upload freezes"), "{:?}", a.health[0]);
        assert!(a.findings.iter().any(|f| f.contains("1-second timer")));
        // The 10 s beat itself is found, not a multiple of it.
        assert!(a.periodicity.iter().all(|p| (p.period_s - 10.0).abs() < 0.5), "{:?}", a.periodicity);
    }

    #[test]
    fn alt_tab_hitches_are_not_counted() {
        // 200 ms frame holes every 20 s; the ones at 40 s and 80 s are alt-tabs.
        let mut s = Synth::new("alttab");
        let hole = |t: i64| t > 10_000_000 && (t - 20_000_000).rem_euclid(20_000_000) < 200_000;
        s.fill(120, |_| false, |_| 2_000, hole);
        s.write();
        std::fs::write(s.dir.join("focus.csv"), "t_us,game,process\n0,1,Discovery.exe\n40000000,0,Discord.exe\n45000000,1,Discovery.exe\n80100000,0,explorer.exe\n81000000,1,Discovery.exe\n").unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        assert_eq!(a.away.len(), 2);
        let counted: Vec<i64> = a.anomalies.iter().filter(|x| x.kind == Kind::FrameHitch && x.in_match).map(|x| x.t / 1_000_000).collect();
        // A hitch is stamped at the last frame before the hole (19.99 s etc.).
        assert_eq!(counted, vec![19, 59, 99], "{counted:?}");
        assert!(a.findings.iter().any(|f| f.contains("switched away from the game 2 time")));
    }

    #[test]
    fn hardware_setup() {
        let mut s = Synth::new("hw");
        s.fill(60, |_| false, |_| 2_000, |_| false);
        s.write();
        // A desktop with one 6000-rated stick at 4800, the monitor on the integrated graphics,
        // the card at PCIe 3.0 x8 with a lowered power limit, a full C: and the CPU capped at 80%.
        let mut gpu = String::from("t_us,gpu,util_pct,power_limit_w,pcie_gen,pcie_width\n");
        for q in 0..60i64 {
            writeln!(gpu, "{},0,90,250,3,8", q * 1_000_000).unwrap();
        }
        std::fs::write(s.dir.join("gpu.csv"), gpu).unwrap();
        std::fs::create_dir_all(s.dir.join("sysinfo")).unwrap();
        std::fs::write(
            s.dir.join("sysinfo/powercfg_processor.txt"),
            "  Power Setting GUID: bc5038f7-23e0-4960-96da-33abaf5935ec  (Maximum processor state)\n    GUID Alias: PROCTHROTTLEMAX\n    Minimum Possible Setting: 0x00000000\n    Maximum Possible Setting: 0x00000064\n    Possible Settings increment: 0x00000001\n    Possible Settings units: %\n  Current AC Power Setting Index: 0x00000050\n  Current DC Power Setting Index: 0x00000064\n",
        )
        .unwrap();
        std::fs::write(
            s.dir.join("meta.json"),
            r#"{"route":{"is_wifi":false},"config":{"processes":["Discovery.exe"]},"elevated":true,"start_local":"2026-10-22 18:00:00",
               "power":{"has_battery":false,"on_battery":false},
               "displays":[{"monitor":"","primary":true,"width":2560,"height":1440,"hz":144,"max_hz":144,"adapter":"AMD Radeon(TM) Graphics"}],
               "nvidia":{"devices":[{"name":"NVIDIA GeForce RTX 4080","max_pcie_gen":4,"max_pcie_width":16,"default_power_limit_w":320}]},
               "hardware":{"pc_type":1,"bios_date":"2025-06-01",
                 "memory":[{"gb":32,"speed":4800,"configured":4800,"type":34,"part":"F5-6000J3636F32G","slot":"DIMM_A2"}],
                 "gpus":[{"name":"AMD Radeon(TM) Graphics","driver_date":"2026-01-01"},{"name":"NVIDIA GeForce RTX 4080","driver_date":"2024-05-01"}],
                 "disks":[{"name":"x","media":"SSD","letters":["C"]},{"name":"y","media":"HDD","letters":["D"]}],
                 "system_drive":{"letter":"C","free_gb":6,"size_gb":930},"background_recording":1}}"#,
        )
        .unwrap();
        let a = analyze(&load::load(&s.dir).unwrap());
        let rec = a.recommendations.join("\n");
        for k in ["XMP", "single-channel", "plugged into the motherboard", "PCIe 3.0 x8", "power limit", "only 6 GB free", "Maximum processor state", "over a year old", "Game Bar"] {
            assert!(rec.contains(k), "{k}: {rec}");
        }
        assert!(a.findings.iter().any(|f| f.contains("hard disk is installed (D:)")), "{:#?}", a.findings);
        let hw = a.health.iter().find(|h| h.area == "Hardware setup").unwrap();
        assert_eq!(hw.status, "warn");
        assert!(hw.text.contains("RAM at 4800 of 6000 MT/s"), "{}", hw.text);
        assert_eq!(a.health.iter().find(|h| h.area == "Display & power").unwrap().status, "warn");
    }

    #[test]
    fn compare_before_after() {
        let mut bad = Synth::new("cmp_a");
        let hole = |t: i64| t > 20_000_000 && (t - 30_000_000).rem_euclid(10_000_000) < 150_000;
        bad.fill(120, hole, |t| if hole(t) { 150_000 } else { 2_000 }, |_| false);
        bad.write();
        let mut good = Synth::new("cmp_b");
        good.fill(120, |_| false, |_| 2_000, |_| false);
        good.write();
        let c = super::compare::compare(&bad.dir, &good.dir).unwrap();
        let row = |l: &str| c.rows.iter().find(|r| r.label == l).unwrap();
        assert_eq!(row("Game stalls (100 ms+) per min").change, 1, "{:?}", row("Game stalls (100 ms+) per min"));
        assert_eq!(row("Router freeze pattern").change, 1, "{:?}", row("Router freeze pattern"));
        assert!(c.summary.starts_with("The second session is better"), "{}", c.summary);
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

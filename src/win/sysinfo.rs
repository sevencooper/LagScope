//! One-shot system snapshot (read-only commands, run once in the background at start)
//! and export of relevant Windows event logs at the end of the session.

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Running, spawn};
use crate::app::App;
use crate::clock;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) fn run(cmd: &str, args: &[&str], stdin: Option<&str>, timeout: Duration) -> String {
    run_env(cmd, args, stdin, timeout, &[])
}

/// Runs a System32 tool with extra environment variables (used to pass values to scripts
/// without quoting them into the script text).
pub(super) fn run_env(cmd: &str, args: &[&str], stdin: Option<&str>, timeout: Duration, env: &[(&str, &str)]) -> String {
    let mut c = Command::new(super::system32(cmd));
    c.envs(env.iter().copied());
    c.args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return format!("<failed to run {cmd}: {e}>"),
    };
    if let (Some(input), Some(mut si)) = (stdin, child.stdin.take()) {
        let _ = si.write_all(input.as_bytes());
    }
    // Read output on helper threads so a chatty command cannot block on a full pipe.
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let ho = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out, &mut v);
        v
    });
    let he = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = std::io::Read::read_to_end(&mut err, &mut v);
        v
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let o = ho.join().unwrap_or_default();
    let e = he.join().unwrap_or_default();
    let mut s = decode(&o);
    if !e.is_empty() {
        s.push_str("\n--- stderr ---\n");
        s.push_str(&decode(&e));
    }
    s
}

/// Removes things that identify the network or machine (SSID, BSSID, MAC addresses, host
/// name) so a session can be shared safely.
pub fn redact(text: &str) -> String {
    let names: Vec<String> = ["COMPUTERNAME", "USERNAME", "USERDOMAIN"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .filter(|n| n.len() >= 3)
        .collect();
    redact_with(text, &names)
}

fn redact_with(text: &str, names: &[String]) -> String {
    const KEYS: &[&str] = &["ssid", "bssid", "profile", "physical address", "host name", "network name", "peer mac", "mac address"];
    // Matched as the whole key only (e.g. event log "User:", "Computer:"), so that keys
    // such as "AllowComputerToTurnOffDevice" survive.
    const EXACT: &[&str] = &["user", "user name", "computer", "computer name", "user id", "domain"];
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut line = match line.split_once(':') {
            Some((k, _))
                if KEYS.iter().any(|w| k.to_ascii_lowercase().contains(w))
                    || EXACT.iter().any(|w| k.trim().eq_ignore_ascii_case(w)) =>
            {
                format!("{k}: <redacted>")
            }
            _ => line.to_string(),
        };
        for n in names {
            line = replace_ci(&line, n, "<redacted>");
        }
        out.push_str(&redact_macs(&line));
        out.push('\n');
    }
    out
}

/// Case-insensitive (ASCII) replacement of every occurrence of `pat`.
fn replace_ci(s: &str, pat: &str, with: &str) -> String {
    let (ls, lp) = (s.to_ascii_lowercase(), pat.to_ascii_lowercase());
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(j) = ls[i..].find(&lp) {
        out.push_str(&s[i..i + j]);
        out.push_str(with);
        i += j + lp.len();
    }
    out.push_str(&s[i..]);
    out
}

/// Replaces anything shaped like a MAC address (xx:xx:xx:xx:xx:xx or with dashes).
fn redact_macs(line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let (mut i, mut copied) = (0, 0);
    while i + 17 <= b.len() {
        let w = &b[i..i + 17];
        let sep = w[2];
        let is_mac = (sep == b':' || sep == b'-')
            && (0..6).all(|k| w[k * 3].is_ascii_hexdigit() && w[k * 3 + 1].is_ascii_hexdigit())
            && (0..5).all(|k| w[k * 3 + 2] == sep);
        if is_mac {
            // A match is pure ASCII, so both ends are char boundaries.
            out.push_str(&line[copied..i]);
            out.push_str("xx-xx-xx-xx-xx-xx");
            i += 17;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&line[copied..]);
    out
}

/// Console tools print in the OEM code page; we set UTF-8 for ourselves, but be lenient.
fn decode(b: &[u8]) -> String {
    String::from_utf8(b.to_vec()).unwrap_or_else(|_| b.iter().map(|&c| c as char).collect())
}

const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
# (not "H": that is a built-in alias of Get-History and would win)
function Section($t) { "`n===== $t =====" }
Section 'OS'; Get-CimInstance Win32_OperatingSystem | Format-List Caption,Version,BuildNumber,OSArchitecture,LastBootUpTime
Section 'Windows display version'; (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').DisplayVersion
Section 'Computer'; Get-CimInstance Win32_ComputerSystem | Format-List Manufacturer,Model,SystemType,TotalPhysicalMemory
Section 'CPU'; Get-CimInstance Win32_Processor | Format-List Name,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,CurrentClockSpeed
Section 'Board / BIOS'; Get-CimInstance Win32_BaseBoard | Format-List Manufacturer,Product,Version; Get-CimInstance Win32_BIOS | Format-List SMBIOSBIOSVersion,ReleaseDate
Section 'Memory modules'; Get-CimInstance Win32_PhysicalMemory | Format-Table -AutoSize Capacity,Speed,ConfiguredClockSpeed,Manufacturer,PartNumber
Section 'GPU'; Get-CimInstance Win32_VideoController | Format-List Name,DriverVersion,DriverDate,CurrentRefreshRate,CurrentHorizontalResolution,CurrentVerticalResolution
Section 'Network adapters'; Get-NetAdapter -IncludeHidden | Format-List Name,InterfaceDescription,Status,LinkSpeed,MediaType,PhysicalMediaType,DriverVersionString,DriverDate,DriverProvider,NdisVersion,Virtual,Hidden
Section 'Adapter advanced properties'; Get-NetAdapterAdvancedProperty | Format-Table -AutoSize Name,DisplayName,DisplayValue
Section 'Adapter power management'; Get-NetAdapterPowerManagement | Format-List Name,AllowComputerToTurnOffDevice,DeviceSleepOnDisconnect,SelectiveSuspend,WakeOnMagicPacket
Section 'Adapter RSC / RSS'; Get-NetAdapterRsc | Format-Table -AutoSize Name,IPv4Enabled,IPv6Enabled; Get-NetAdapterRss | Format-Table -AutoSize Name,Enabled,NumberOfReceiveQueues
Section 'IP interfaces'; Get-NetIPInterface | Sort-Object InterfaceMetric | Format-Table -AutoSize InterfaceAlias,AddressFamily,InterfaceMetric,NlMtu,ConnectionState
Section 'Routes (default)'; Get-NetRoute -DestinationPrefix '0.0.0.0/0','::/0' | Format-Table -AutoSize InterfaceAlias,NextHop,RouteMetric
Section 'TCP settings'; Get-NetTCPSetting -SettingName Internet | Format-List AutoTuningLevelLocal,CongestionProvider,EcnCapability,Timestamps
Section 'QoS policies'; Get-NetQosPolicy | Format-List
Section 'DNS servers'; Get-DnsClientServerAddress -AddressFamily IPv4 | Format-Table -AutoSize InterfaceAlias,ServerAddresses
Section 'Gaming-related registry'
$r = @{
 'GameDVR_Enabled'            = (Get-ItemProperty 'HKCU:\System\GameConfigStore').GameDVR_Enabled
 'AppCaptureEnabled'          = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\GameDVR').AppCaptureEnabled
 'AutoGameModeEnabled'        = (Get-ItemProperty 'HKCU:\Software\Microsoft\GameBar').AutoGameModeEnabled
 'HwSchMode (HAGS 2=on)'      = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\GraphicsDrivers').HwSchMode
 'NetworkThrottlingIndex'     = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile').NetworkThrottlingIndex
 'SystemResponsiveness'       = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile').SystemResponsiveness
 'GlobalTimerResolutionRequests' = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\kernel').GlobalTimerResolutionRequests
 'VBS running'                = (Get-CimInstance -ClassName Win32_DeviceGuard -Namespace root\Microsoft\Windows\DeviceGuard).VirtualizationBasedSecurityStatus
}
$r.GetEnumerator() | Sort-Object Name | Format-Table -AutoSize
Section 'Services of interest'; Get-Service | Where-Object { $_.Name -match 'Killer|ExitLag|GearUP|NoPing|WTFast|Rivet|SmartByte|Nahimic|Sonic|NordVPN|ProtonVPN|OpenVPN|WireGuard|Tailscale|ZeroTier|Hamachi|Radmin|WARP|Cloudflare|Asus|ROG|MSI|Lenovo|Razer|Corsair|Logi|Intel.*Connect|MediaTek|Realtek|Qualcomm' } | Format-Table -AutoSize Name,Status,StartType,DisplayName
Section 'Top processes by CPU time'; Get-Process | Sort-Object CPU -Descending | Select-Object -First 40 Name,Id,CPU,@{n='WS_MB';e={[int]($_.WorkingSet64/1MB)}},PriorityClass | Format-Table -AutoSize
Section 'Installed network-ish software'; Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*','HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*' | Where-Object { $_.DisplayName -match 'VPN|Killer|ExitLag|GearUP|NoPing|WTFast|Booster|Network|Wi-?Fi|Wireless|Bluetooth|Nahimic|SmartByte|Armoury|Dragon Center|Vantage|Synapse|iCUE|G HUB|Afterburner|RivaTuner|Overwolf|Medal|OBS' } | Select-Object DisplayName,DisplayVersion,Publisher | Sort-Object DisplayName | Format-Table -AutoSize
"#;

pub fn start(app: Arc<App>) -> Running {
    let a = app.clone();
    let t = spawn("sysinfo", move || {
        let t0 = Instant::now();
        let long = Duration::from_secs(60);
        let short = Duration::from_secs(15);
        let jobs: Vec<(&str, &str, Vec<&str>)> = vec![
            ("sysinfo/netsh_wlan_interfaces.txt", "netsh.exe", vec!["wlan", "show", "interfaces"]),
            ("sysinfo/netsh_wlan_drivers.txt", "netsh.exe", vec!["wlan", "show", "drivers"]),
            ("sysinfo/netsh_wlan_capabilities.txt", "netsh.exe", vec!["wlan", "show", "wirelesscapabilities"]),
            ("sysinfo/netsh_wlan_settings.txt", "netsh.exe", vec!["wlan", "show", "settings"]),
            ("sysinfo/netsh_tcp_global.txt", "netsh.exe", vec!["int", "tcp", "show", "global"]),
            ("sysinfo/ipconfig_all.txt", "ipconfig.exe", vec!["/all"]),
            ("sysinfo/powercfg_active.txt", "powercfg.exe", vec!["/getactivescheme"]),
            // Wireless Adapter Settings > Power Saving Mode, and processor power management.
            ("sysinfo/powercfg_wifi.txt", "powercfg.exe", vec!["/q", "SCHEME_CURRENT", "19cbb8fa-5279-450e-9fac-8a3d5fedd0c1"]),
            ("sysinfo/powercfg_processor.txt", "powercfg.exe", vec!["/q", "SCHEME_CURRENT", "54533251-82be-4824-96c1-47b60b740d00"]),
        ];
        for (file, cmd, args) in jobs {
            if a.stopping() {
                return;
            }
            let out = run(cmd, &args, None, short);
            a.session.write_text(file, &redact(&out));
        }
        if a.stopping() {
            return;
        }
        // -EncodedCommand: a script piped through stdin runs line by line, which breaks
        // multi-line statements.
        let enc = super::fixes::encode_command(PS_SCRIPT);
        let ps = run(
            "WindowsPowerShell\\v1.0\\powershell.exe",
            &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &enc],
            None,
            long,
        );
        a.session.write_text("sysinfo/system.txt", &redact(&ps));
        summarize_wlan(&a);
        a.session.event("collector", format!("sysinfo: snapshot written ({:.1}s)", t0.elapsed().as_secs_f64()));
    });
    Running::threads("sysinfo", vec![t])
}

/// Pulls a few key lines out of `netsh wlan show interfaces` into meta.json.
fn summarize_wlan(app: &App) {
    let Ok(text) = std::fs::read_to_string(app.session.dir.join("sysinfo/netsh_wlan_interfaces.txt")) else { return };
    let keys = ["Description", "Radio type", "Band", "Channel", "Receive rate (Mbps)", "Transmit rate (Mbps)", "Signal", "Authentication", "Connection mode"];
    let mut m = serde_json::Map::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            if keys.iter().any(|w| k.eq_ignore_ascii_case(w)) && !m.contains_key(k) {
                m.insert(k.to_string(), v.trim().into());
            }
        }
    }
    if !m.is_empty() {
        app.session.meta_set("wlan_summary", serde_json::Value::Object(m));
    }
}

/// Exports Windows event-log entries recorded since shortly before the session started.
pub fn export_event_logs(app: &App) {
    let since = app.session.start_unix_ms - 120_000;
    let iso = clock::format_wall(since, 0).replace(' ', "T") + "Z";
    let query = format!("*[System[TimeCreated[@SystemTime>='{iso}']]]");
    let logs = [
        ("System", "eventlog_system.txt"),
        ("Application", "eventlog_application.txt"),
        ("Microsoft-Windows-WLAN-AutoConfig/Operational", "eventlog_wlan.txt"),
        ("Microsoft-Windows-NetworkProfile/Operational", "eventlog_networkprofile.txt"),
    ];
    for (log, file) in logs {
        let q = format!("/q:{query}");
        let out = run("wevtutil.exe", &["qe", log, &q, "/f:text", "/c:2000"], None, Duration::from_secs(20));
        app.session.write_text(&format!("sysinfo/{file}"), &redact(&out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts() {
        let r = redact("    SSID                   : MyHome\n    AP BSSID               : 12:34:56:78:9a:bc\n    Channel                : 36\nMAC 00-11-22-33-44-55 seen");
        assert!(!r.contains("MyHome"));
        assert!(!r.contains("12:34"));
        assert!(r.contains("Channel                : 36"));
        assert!(r.contains("MAC xx-xx-xx-xx-xx-xx seen"));
        let names = vec!["Test-PC42".to_string(), "Some User".to_string()];
        let r = redact_with("  User Name: TEST-PC42\\Some User\n  Computer: Test-PC42\nAllowComputerToTurnOffDevice : Enabled\nC:\\Users\\Some User\\x é 00:11:22:33:44:55", &names);
        assert!(!r.to_ascii_lowercase().contains("test-pc42") && !r.contains("Some User"), "{r}");
        assert!(r.contains("AllowComputerToTurnOffDevice : Enabled"));
        assert!(r.contains("é xx-xx-xx-xx-xx-xx"));
    }
}

//! Reversible Wi-Fi settings changes for the Wi-Fi check (only ever made after the user
//! agreed), plus the read-only inventory they are planned from and driver updates via
//! Windows Update.
//!
//! Every temporary change is written to `%ProgramData%\LagScope\pending_restore.json`
//! before it is made, so if the tool is closed or crashes mid-test the next start puts the
//! setting back. Changes the user chooses to keep are listed in `kept_changes.json` and can
//! be undone from the menu.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::sysinfo::run_env;

const PS: &str = "WindowsPowerShell\\v1.0\\powershell.exe";
const PS_ARGS: &[&str] = &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"];

/// Base64 of the UTF-16LE script, as -EncodedCommand expects (a script fed through stdin
/// is run line by line, which breaks multi-line blocks).
pub(super) fn encode_command(script: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(b.len() * 4 / 3 + 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Newest driver known when this LagScope was built (adapter name fragment, version, date).
pub const KNOWN_DRIVERS: &[(&str, &str, &str)] = &[("FastConnect 7800", "3.1.0.1794", "July 2026")];

fn ps(script: &str, env: &[(&str, &str)], timeout_s: u64) -> String {
    let script = format!("$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n{script}");
    let enc = encode_command(&script);
    let mut args = PS_ARGS.to_vec();
    args.push(&enc);
    run_env(PS, &args, None, Duration::from_secs(timeout_s), env)
}

/// Runs a script whose last output line is JSON.
fn ps_json<T: for<'de> Deserialize<'de>>(script: &str, env: &[(&str, &str)], timeout_s: u64) -> Result<T> {
    let out = ps(script, env, timeout_s);
    let line = out.lines().rev().find(|l| l.trim_start().starts_with(['{', '['])).unwrap_or("");
    serde_json::from_str(line).map_err(|e| anyhow::anyhow!("{e}: {}", out.trim().chars().take(400).collect::<String>()))
}

const RADIOS: &str = r#"
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
function Await($op, [Type]$t) { $task = $asTask.MakeGenericMethod($t).Invoke($null, @($op)); $null = $task.Wait(-1); $task.Result }
$null = [Windows.Devices.Radios.Radio, Windows.System.Devices, ContentType = WindowsRuntime]
$null = Await ([Windows.Devices.Radios.Radio]::RequestAccessAsync()) ([Windows.Devices.Radios.RadioAccessStatus])
$radios = @(Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]]) | Where-Object { $_.Kind -eq 'Bluetooth' })
"#;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AdvProp {
    pub display: String,
    pub value: String,
    pub keyword: String,
    pub reg: String,
    #[serde(default)]
    pub valid: Vec<String>,
    #[serde(default)]
    pub valid_reg: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Inventory {
    pub adapter: String,
    pub description: String,
    pub driver_version: String,
    pub driver_date: String,
    #[serde(default)]
    pub adv: Vec<AdvProp>,
    pub selective_suspend: String,
    pub location: String,
    #[serde(default)]
    pub wifi_direct: Vec<String>,
    #[serde(default)]
    pub bluetooth: Vec<String>,
    #[serde(default)]
    pub autoconfig: bool,
    #[serde(default)]
    pub band: String,
}

pub fn inventory(adapter: &str) -> Result<Inventory> {
    let script = format!(
        r#"
$n = $env:LS_ADAPTER
$a = Get-NetAdapter -Name $n
$ErrorActionPreference = 'SilentlyContinue'
$adv = @(Get-NetAdapterAdvancedProperty -Name $n | ForEach-Object {{ [pscustomobject]@{{ display = [string]$_.DisplayName; value = [string]$_.DisplayValue; keyword = [string]$_.RegistryKeyword; reg = [string](@($_.RegistryValue) -join ','); valid = @($_.ValidDisplayValues | ForEach-Object {{ [string]$_ }}); valid_reg = @($_.ValidRegistryValues | ForEach-Object {{ [string]$_ }}) }} }})
$pm = Get-NetAdapterPowerManagement -Name $n
$loc = [string](Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\location').Value
$wfd = @(Get-PnpDevice -Class Net | Where-Object {{ $_.FriendlyName -like '*Wi-Fi Direct*' -and $_.Status -eq 'OK' }} | ForEach-Object {{ [string]$_.InstanceId }})
$bt = @()
try {{ {RADIOS}
$bt = @($radios | ForEach-Object {{ [string]$_.State }}) }} catch {{ $bt = @('error: ' + $_) }}
ConvertTo-Json -Depth 4 -Compress -InputObject ([pscustomobject]@{{ adapter = $n; description = [string]$a.InterfaceDescription; driver_version = [string]$a.DriverVersionString; driver_date = [string]$a.DriverDate; adv = $adv; selective_suspend = [string]$pm.SelectiveSuspend; location = $loc; wifi_direct = $wfd; bluetooth = $bt }})
"#
    );
    let mut inv: Inventory = ps_json(&script, &[("LS_ADAPTER", adapter)], 90)?;
    inv.autoconfig = autoconfig_enabled(adapter).unwrap_or(true);
    inv.band = current_band(adapter);
    Ok(inv)
}

/// `netsh wlan show settings` -> "Auto configuration logic is enabled on interface "Wi-Fi"".
pub fn autoconfig_enabled(iface: &str) -> Option<bool> {
    let out = run_env("netsh.exe", &["wlan", "show", "settings"], None, Duration::from_secs(15), &[]);
    out.lines().find(|l| l.contains(&format!("\"{iface}\"")) && l.contains("configuration")).map(|l| l.contains("enabled"))
}

/// Band the adapter is connected on, e.g. "6 GHz" (from `netsh wlan show interfaces`).
pub fn current_band(iface: &str) -> String {
    let out = run_env("netsh.exe", &["wlan", "show", "interfaces"], None, Duration::from_secs(15), &[]);
    let mut in_iface = false;
    for l in out.lines() {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if k == "Name" {
                in_iface = v == iface;
            } else if in_iface && k == "Band" {
                return v.to_string();
            }
        }
    }
    String::new()
}

/// Compares dotted version strings numerically ("3.1.0.1430" < "3.1.0.1794").
pub fn version_lt(a: &str, b: &str) -> bool {
    let p = |s: &str| s.split('.').map(|x| x.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    p(a) < p(b)
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct PropChange {
    pub keyword: String,
    pub display: String,
    pub from_reg: String,
    pub from_display: String,
    pub to_reg: String,
    pub to_display: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum Change {
    /// Windows' WLAN AutoConfig background activity for this interface (netsh).
    Autoconfig { iface: String },
    /// Bluetooth radio(s) off.
    Bluetooth,
    /// Location services off (prev = previous registry value, normally "Allow").
    Location { prev: String },
    /// Wi-Fi adapter advanced properties / selective suspend (applied together, then one
    /// adapter restart).
    Adapter { adapter: String, props: Vec<PropChange>, selective_suspend_off: bool },
    /// Wi-Fi Direct virtual adapters disabled.
    WifiDirect { ids: Vec<String> },
}

impl Change {
    pub fn restarts_adapter(&self) -> bool {
        matches!(self, Change::Adapter { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Change::Autoconfig { iface } => format!("Windows Wi-Fi background scanning off for '{iface}'"),
            Change::Bluetooth => "Bluetooth off".into(),
            Change::Location { .. } => "Location services off".into(),
            Change::Adapter { props, selective_suspend_off, .. } => {
                let mut v: Vec<String> = props.iter().map(|p| format!("{}: {} -> {}", p.display, p.from_display, p.to_display)).collect();
                if *selective_suspend_off {
                    v.push("Selective suspend: Enabled -> Disabled".into());
                }
                v.join(", ")
            }
            Change::WifiDirect { ids } => format!("{} Wi-Fi Direct adapter(s) disabled", ids.len()),
        }
    }

    pub fn apply(&self) -> Result<()> {
        self.set(true)
    }

    pub fn revert(&self) -> Result<()> {
        self.set(false)
    }

    fn set(&self, on: bool) -> Result<()> {
        let out = match self {
            Change::Autoconfig { iface } => {
                let en = if on { "enabled=no" } else { "enabled=yes" };
                let i = format!("interface={iface}");
                let out = run_env("netsh.exe", &["wlan", "set", "autoconfig", en, &i], None, Duration::from_secs(20), &[]);
                if autoconfig_enabled(iface) != Some(!on) {
                    bail!("netsh did not change the setting: {}", out.trim());
                }
                return Ok(());
            }
            Change::Bluetooth => ps(
                &format!("{RADIOS}\nforeach ($r in $radios) {{ $s = Await ($r.SetStateAsync($env:LS_STATE)) ([Windows.Devices.Radios.RadioAccessStatus]); \"status=$s\" }}\n'done'"),
                &[("LS_STATE", if on { "Off" } else { "On" })],
                60,
            ),
            Change::Location { prev } => ps(
                "Set-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\location' -Name Value -Value $env:LS_VAL\n'done'",
                &[("LS_VAL", if on { "Deny" } else { prev.as_str() })],
                30,
            ),
            Change::Adapter { adapter, props, selective_suspend_off } => {
                let list: Vec<serde_json::Value> = props
                    .iter()
                    .map(|p| serde_json::json!({"k": p.keyword, "v": if on { &p.to_reg } else { &p.from_reg }}))
                    .collect();
                let ss = match (selective_suspend_off, on) {
                    (false, _) => "",
                    (true, true) => "Disabled",
                    (true, false) => "Enabled",
                };
                ps(
                    r#"
$n = $env:LS_ADAPTER
# (Windows PowerShell turns "[]" into one empty item, hence the check for .k)
foreach ($p in @($env:LS_PROPS | ConvertFrom-Json)) { if ($p.k) { Set-NetAdapterAdvancedProperty -Name $n -RegistryKeyword $p.k -RegistryValue $p.v -NoRestart } }
if ($env:LS_SS) { Set-NetAdapterPowerManagement -Name $n -SelectiveSuspend $env:LS_SS -NoRestart }
Restart-NetAdapter -Name $n
'done'"#,
                    &[("LS_ADAPTER", adapter), ("LS_PROPS", &serde_json::Value::Array(list).to_string()), ("LS_SS", ss)],
                    90,
                )
            }
            Change::WifiDirect { ids } => ps(
                &format!(
                    "foreach ($id in @($env:LS_IDS | ConvertFrom-Json)) {{ if ($id) {{ {}-PnpDevice -InstanceId $id -Confirm:$false }} }}\n'done'",
                    if on { "Disable" } else { "Enable" }
                ),
                &[("LS_IDS", &serde_json::to_string(ids).unwrap_or_default())],
                60,
            ),
        };
        let last = out.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if last != "done" {
            bail!("{}", out.trim().chars().take(400).collect::<String>());
        }
        if let Change::Bluetooth = self {
            if let Some(bad) = out.lines().find(|l| l.starts_with("status=") && !l.ends_with("Allowed")) {
                bail!("Windows refused to switch Bluetooth ({bad})");
            }
        }
        Ok(())
    }
}

/// One test of the Wi-Fi check.
#[derive(Debug, Clone)]
pub struct Experiment {
    pub key: &'static str,
    pub label: String,
    /// Shown when offering to keep the change.
    pub keep_note: &'static str,
    pub change: Change,
}

fn find_prop<'a>(inv: &'a Inventory, name: &str) -> Option<&'a AdvProp> {
    inv.adv.iter().find(|p| p.display.to_ascii_lowercase().contains(name) && p.valid.len() == p.valid_reg.len())
}

/// The value whose display text matches one of `wants` (in order of preference).
fn pick(p: &AdvProp, wants: &[&str]) -> Option<(String, String)> {
    for w in wants {
        if let Some(i) = p.valid.iter().position(|v| v.to_ascii_lowercase().contains(w)) {
            return Some((p.valid_reg[i].clone(), p.valid[i].clone()));
        }
    }
    None
}

fn prop_change(p: &AdvProp, to: (String, String)) -> Option<PropChange> {
    (to.0 != p.reg).then(|| PropChange {
        keyword: p.keyword.clone(),
        display: p.display.clone(),
        from_reg: p.reg.clone(),
        from_display: p.value.clone(),
        to_reg: to.0,
        to_display: to.1,
    })
}

/// Which tests make sense on this PC, least invasive first.
pub fn plan(inv: &Inventory) -> Vec<Experiment> {
    let a = inv.adapter.clone();
    let mut v = Vec::new();
    let adapter = |props: Vec<PropChange>, ss: bool| Change::Adapter { adapter: a.clone(), props, selective_suspend_off: ss };

    if let Some(c) = find_prop(inv, "roam").and_then(|p| pick(p, &["lowest", "disable"]).and_then(|t| prop_change(p, t))) {
        v.push(Experiment { key: "roaming", label: format!("Roaming aggressiveness: {}", c.to_display), keep_note: "", change: adapter(vec![c], false) });
    }
    let mut power: Vec<PropChange> = inv
        .adv
        .iter()
        .filter(|p| {
            let d = p.display.to_ascii_lowercase();
            p.valid.len() == p.valid_reg.len() && (d.contains("power sav") || d.contains("mimo power") || d.contains("u-apsd") || d.contains("uapsd"))
        })
        .filter_map(|p| pick(p, &["disable", "off", "no smps", "maximum performance", "max"]).and_then(|t| prop_change(p, t)))
        .collect();
    power.truncate(4);
    let ss = inv.selective_suspend.eq_ignore_ascii_case("enabled");
    if !power.is_empty() || ss {
        v.push(Experiment { key: "powersave", label: "Wi-Fi power saving off".into(), keep_note: "", change: adapter(power, ss) });
    }
    let accel: Vec<PropChange> = inv
        .adv
        .iter()
        .filter(|p| {
            let d = p.display.to_ascii_lowercase();
            p.valid.len() == p.valid_reg.len() && d.contains("throughput") && (d.contains("accel") || d.contains("boost") || d.contains("enhanc"))
        })
        .filter_map(|p| pick(p, &["disable", "off"]).and_then(|t| prop_change(p, t)))
        .collect();
    if !accel.is_empty() {
        let name = accel[0].display.clone();
        v.push(Experiment { key: "throughput", label: format!("{name} off"), keep_note: "", change: adapter(accel, false) });
    }
    if !inv.wifi_direct.is_empty() {
        v.push(Experiment {
            key: "wifidirect",
            label: "Wi-Fi Direct adapters off".into(),
            keep_note: "Mobile hotspot, wireless display (Miracast) and some phone-link features use Wi-Fi Direct.",
            change: Change::WifiDirect { ids: inv.wifi_direct.clone() },
        });
    }
    if let Some(p) = find_prop(inv, "preferred band") {
        let on_6 = inv.band.starts_with('6');
        let want: &[&str] = if on_6 || inv.band.starts_with('2') { &["5ghz", "5 ghz", "5g"] } else { &["6ghz", "6 ghz", "6g"] };
        if let Some(c) = pick(p, want).and_then(|t| prop_change(p, t)) {
            v.push(Experiment {
                key: "band",
                label: format!("Other Wi-Fi band ({})", c.to_display),
                keep_note: "The PC will prefer this band from now on (it can still use the others).",
                change: adapter(vec![c], false),
            });
        }
    }
    if inv.location.eq_ignore_ascii_case("allow") {
        v.push(Experiment {
            key: "location",
            label: "Location services off".into(),
            keep_note: "Apps (maps, weather, Find my device) can no longer see your location.",
            change: Change::Location { prev: inv.location.clone() },
        });
    }
    if inv.bluetooth.iter().any(|s| s.eq_ignore_ascii_case("on")) {
        v.push(Experiment {
            key: "bluetooth",
            label: "Bluetooth off".into(),
            keep_note: "Bluetooth headsets, controllers, mice and keyboards will stop working until you turn Bluetooth back on.",
            change: Change::Bluetooth,
        });
    }
    if inv.autoconfig {
        v.push(Experiment {
            key: "autoconfig",
            label: "Windows Wi-Fi background scanning off".into(),
            keep_note: "Windows will no longer search for networks or connect on its own. After a restart you may have to connect manually, and other Wi-Fi networks will not show up - use LagScope's Undo (menu option 3) to turn it back on.",
            change: Change::Autoconfig { iface: a.clone() },
        });
    }
    v
}

fn state_dir() -> PathBuf {
    let base = std::env::var("ProgramData").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:\\ProgramData"));
    base.join("LagScope")
}

fn read_list(name: &str) -> Vec<Change> {
    std::fs::read_to_string(state_dir().join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_list(name: &str, v: &[Change]) {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(name);
    if v.is_empty() {
        let _ = std::fs::remove_file(p);
    } else {
        let _ = std::fs::write(p, serde_json::to_string_pretty(v).unwrap_or_default());
    }
}

/// Applies a change temporarily (remembered so it can be undone after a crash).
pub fn apply_temp(c: &Change) -> Result<()> {
    write_list("pending_restore.json", std::slice::from_ref(c));
    let r = c.apply();
    if r.is_err() {
        // A half-applied change is still reverted below by the caller / next start.
        let _ = c.revert();
        write_list("pending_restore.json", &[]);
    }
    r
}

pub fn revert_temp(c: &Change) -> Result<()> {
    let r = c.revert();
    if r.is_ok() {
        write_list("pending_restore.json", &[]);
    }
    r
}

/// Puts back anything a previous run left changed (crash / window closed mid-test).
pub fn restore_pending() -> Vec<(String, Result<()>)> {
    let v = read_list("pending_restore.json");
    let mut out = Vec::new();
    for c in &v {
        out.push((c.describe(), c.revert()));
    }
    if out.iter().all(|(_, r)| r.is_ok()) {
        write_list("pending_restore.json", &[]);
    }
    out
}

/// Makes a change permanent (it is already applied or will be applied now).
pub fn keep(c: &Change) -> Result<()> {
    c.apply()?;
    let mut v = read_list("kept_changes.json");
    v.retain(|x| x != c);
    v.push(c.clone());
    write_list("kept_changes.json", &v);
    Ok(())
}

pub fn kept() -> Vec<Change> {
    read_list("kept_changes.json")
}

/// Reverts every kept change (newest first).
pub fn undo_kept() -> Vec<(String, Result<()>)> {
    let v = read_list("kept_changes.json");
    let mut left = Vec::new();
    let mut out = Vec::new();
    for c in v.iter().rev() {
        let r = c.revert();
        if r.is_err() {
            left.push(c.clone());
        }
        out.push((c.describe(), r));
    }
    left.reverse();
    write_list("kept_changes.json", &left);
    out
}

#[derive(Debug, Clone, Deserialize)]
pub struct DriverUpdate {
    pub title: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub date: String,
    pub id: String,
}

const WU_SEARCH: &str = r#"
$s = New-Object -ComObject Microsoft.Update.Session
$s.ClientApplicationID = 'LagScope'
$r = $s.CreateUpdateSearcher().Search("IsInstalled=0 and Type='Driver'")
$list = @(foreach ($u in $r.Updates) { [pscustomobject]@{ title = [string]$u.Title; class = [string]$u.DriverClass; provider = [string]$u.DriverProvider; date = [string]$u.DriverVerDate; id = [string]$u.Identity.UpdateID } })
ConvertTo-Json -Depth 3 -Compress -InputObject $list
"#;

/// Driver updates Windows Update offers (including optional ones) for network/Bluetooth.
pub fn search_driver_updates() -> Result<Vec<DriverUpdate>> {
    let all: Vec<DriverUpdate> = ps_json(WU_SEARCH, &[], 300)?;
    Ok(all
        .into_iter()
        .filter(|u| {
            let t = format!("{} {} {}", u.title, u.class, u.provider).to_ascii_lowercase();
            ["net", "bluetooth", "wireless", "wi-fi", "wifi", "wlan", "qualcomm", "mediatek"].iter().any(|w| t.contains(w))
        })
        .collect())
}

/// Downloads and installs the given updates. Returns whether a restart is required.
pub fn install_driver_updates(ids: &[String]) -> Result<bool> {
    #[derive(Deserialize)]
    struct R {
        result: i64,
        reboot: bool,
    }
    let script = r#"
$ids = @($env:LS_IDS | ConvertFrom-Json)
$s = New-Object -ComObject Microsoft.Update.Session
$s.ClientApplicationID = 'LagScope'
$r = $s.CreateUpdateSearcher().Search("IsInstalled=0 and Type='Driver'")
$c = New-Object -ComObject Microsoft.Update.UpdateColl
foreach ($u in $r.Updates) { if ($ids -contains [string]$u.Identity.UpdateID) { if (-not $u.EulaAccepted) { $u.AcceptEula() }; $null = $c.Add($u) } }
if ($c.Count -eq 0) { throw 'update no longer offered' }
$d = $s.CreateUpdateDownloader(); $d.Updates = $c; $null = $d.Download()
$i = $s.CreateUpdateInstaller(); $i.Updates = $c; $res = $i.Install()
ConvertTo-Json -Compress -InputObject ([pscustomobject]@{ result = [int]$res.ResultCode; reboot = [bool]$res.RebootRequired })
"#;
    let r: R = ps_json(script, &[("LS_IDS", &serde_json::to_string(ids).unwrap_or_default())], 900)?;
    // OperationResultCode: 2 = succeeded, 3 = succeeded with errors.
    if r.result != 2 && r.result != 3 {
        bail!("Windows Update install result code {}", r.result);
    }
    Ok(r.reboot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(display: &str, value: &str, reg: &str, valid: &[&str], valid_reg: &[&str]) -> AdvProp {
        AdvProp {
            display: display.into(),
            value: value.into(),
            keyword: display.replace(' ', ""),
            reg: reg.into(),
            valid: valid.iter().map(|s| s.to_string()).collect(),
            valid_reg: valid_reg.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn plans_tests_for_a_qualcomm_card() {
        let inv = Inventory {
            adapter: "Wi-Fi".into(),
            adv: vec![
                prop("Roaming aggressiveness", "2.Medium-low", "2", &["1.Lowest", "2.Medium-low", "3.Medium"], &["1", "2", "3"]),
                prop("Throughput Acceleration", "Enable", "1", &["Disable", "Enable"], &["0", "1"]),
                prop("Preferred Band", "6Ghz Band Preferred", "3", &["No Preference", "2.4Ghz Band Preferred", "5Ghz Band Preferred", "6Ghz Band Preferred"], &["0", "1", "2", "3"]),
            ],
            selective_suspend: "Enabled".into(),
            location: "Allow".into(),
            bluetooth: vec!["On".into()],
            autoconfig: true,
            band: "6 GHz".into(),
            ..Default::default()
        };
        let keys: Vec<&str> = plan(&inv).iter().map(|e| e.key).collect();
        assert_eq!(keys, ["roaming", "powersave", "throughput", "band", "location", "bluetooth", "autoconfig"]);
        let p = plan(&inv);
        let Change::Adapter { props, .. } = &p[3].change else { panic!() };
        assert_eq!((props[0].to_reg.as_str(), props[0].from_reg.as_str()), ("2", "3"));
        let Change::Adapter { props, .. } = &p[0].change else { panic!() };
        assert_eq!(props[0].to_display, "1.Lowest");
    }

    #[test]
    fn base64() {
        assert_eq!(encode_command("hi"), "aABpAA==");
        assert_eq!(encode_command("abc"), "YQBiAGMA");
    }

    #[test]
    fn versions() {
        assert!(version_lt("3.1.0.1430", "3.1.0.1794"));
        assert!(!version_lt("3.1.0.1794", "3.1.0.1794"));
        assert!(!version_lt("3.2.0.1", "3.1.0.1794"));
    }
}

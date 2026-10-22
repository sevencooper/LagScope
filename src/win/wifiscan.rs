//! Wi-Fi neighbourhood scan (on request only - a scan briefly takes the adapter off its
//! channel): how many other networks share each channel and band, and which channels are
//! quieter. Network names and addresses of neighbours are never stored; the own network is
//! recognised by comparing names in memory only.

use std::ffi::c_void;

use anyhow::{Result, bail};
use serde::Serialize;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WiFi::*;
use windows_sys::core::GUID;

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub channel: u32,
    pub networks: u32,
    pub strongest_dbm: i32,
    /// Congestion score: neighbours weighted by signal strength (2.4 GHz includes
    /// overlapping channels).
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Band {
    pub band: String,
    pub networks: u32,
    pub channels: Vec<Channel>,
    /// Quietest recommended channel and its score.
    pub best: Option<(u32, f64)>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScanResult {
    pub own_band: String,
    pub own_channel: u32,
    pub own_rssi: i32,
    /// Neighbouring networks on the own channel (other networks only).
    pub own_channel_neighbours: u32,
    pub own_channel_score: f64,
    pub bands: Vec<Band>,
    pub total_networks: u32,
    pub summary: Vec<String>,
}

fn band_channel(khz: u32) -> (&'static str, u32) {
    let mhz = khz / 1000;
    match mhz {
        2412..=2472 => ("2.4 GHz", (mhz - 2407) / 5),
        2484 => ("2.4 GHz", 14),
        5150..=5925 => ("5 GHz", (mhz - 5000) / 5),
        5926..=7125 => ("6 GHz", (mhz - 5950) / 5),
        _ => ("other", 0),
    }
}

/// Signal-based weight of one neighbour: a strong neighbour counts ~1, a faint one ~0.
fn weight(rssi: i32) -> f64 {
    ((rssi as f64 + 90.0) / 30.0).clamp(0.0, 1.2)
}

fn round1(x: f64) -> f64 {
    ((x * 10.0).round() / 10.0).max(0.0)
}

struct Wlan(HANDLE);
impl Drop for Wlan {
    fn drop(&mut self) {
        unsafe { WlanCloseHandle(self.0, std::ptr::null()) };
    }
}

pub fn scan() -> Result<ScanResult> {
    let mut ver = 0u32;
    let mut h: HANDLE = std::ptr::null_mut();
    if unsafe { WlanOpenHandle(2, std::ptr::null(), &mut ver, &mut h) } != 0 {
        bail!("the WLAN service is not available (no Wi-Fi adapter?)");
    }
    let h = Wlan(h);
    // The connected interface.
    let mut guid: Option<GUID> = None;
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h.0, std::ptr::null(), &mut list) == 0 && !list.is_null() {
            let l = &*list;
            let items = std::slice::from_raw_parts(l.InterfaceInfo.as_ptr(), l.dwNumberOfItems as usize);
            guid = items.iter().find(|i| i.isState == wlan_interface_state_connected).or(items.first()).map(|i| i.InterfaceGuid);
            WlanFreeMemory(list as *const c_void);
        }
    }
    let Some(guid) = guid else { bail!("no Wi-Fi adapter found") };

    // Own connection: SSID (compared in memory only) and BSSID.
    let mut own_ssid: Vec<u8> = Vec::new();
    let mut own_bssid = [0u8; 6];
    unsafe {
        let mut size = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let mut vt = 0;
        let r = WlanQueryInterface(h.0, &guid, wlan_intf_opcode_current_connection, std::ptr::null(), &mut size, &mut data, &mut vt);
        if r == 5 {
            bail!("Windows blocks Wi-Fi scanning while Location services are off. Turn on Settings > Privacy & security > Location (and 'Let desktop apps access your location'), then scan again.");
        }
        if r == 0 && !data.is_null() {
            let c = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
            let a = &c.wlanAssociationAttributes;
            own_ssid = a.dot11Ssid.ucSSID[..a.dot11Ssid.uSSIDLength as usize].to_vec();
            own_bssid = a.dot11Bssid;
            WlanFreeMemory(data);
        }
    }

    unsafe { WlanScan(h.0, &guid, std::ptr::null(), std::ptr::null(), std::ptr::null()) };
    std::thread::sleep(std::time::Duration::from_secs(5));

    let mut entries: Vec<(Vec<u8>, [u8; 6], i32, u32)> = Vec::new();
    unsafe {
        let mut list: *mut WLAN_BSS_LIST = std::ptr::null_mut();
        let r = WlanGetNetworkBssList(h.0, &guid, std::ptr::null(), dot11_BSS_type_any, 0, std::ptr::null(), &mut list);
        if r == 5 {
            bail!("Windows blocks Wi-Fi scanning while Location services are off. Turn on Settings > Privacy & security > Location (and 'Let desktop apps access your location'), then scan again.");
        }
        if r != 0 || list.is_null() {
            bail!("Wi-Fi scan failed (error {r})");
        }
        let l = &*list;
        for e in std::slice::from_raw_parts(l.wlanBssEntries.as_ptr(), l.dwNumberOfItems as usize) {
            let ssid = e.dot11Ssid.ucSSID[..(e.dot11Ssid.uSSIDLength as usize).min(32)].to_vec();
            entries.push((ssid, e.dot11Bssid, e.lRssi, e.ulChCenterFrequency));
        }
        WlanFreeMemory(list as *const c_void);
    }
    Ok(summarise(&entries, &own_ssid, &own_bssid))
}

/// Pure part (unit-tested): entries are (SSID bytes, BSSID, RSSI dBm, centre frequency kHz).
pub fn summarise(entries: &[(Vec<u8>, [u8; 6], i32, u32)], own_ssid: &[u8], own_bssid: &[u8; 6]) -> ScanResult {
    let mut res = ScanResult::default();
    if let Some(own) = entries.iter().find(|e| &e.1 == own_bssid) {
        let (b, c) = band_channel(own.3);
        res.own_band = b.into();
        res.own_channel = c;
        res.own_rssi = own.2;
    }
    let others: Vec<(&str, u32, i32)> = entries
        .iter()
        .filter(|e| own_ssid.is_empty() || e.0 != own_ssid)
        .map(|e| {
            let (b, c) = band_channel(e.3);
            (b, c, e.2)
        })
        .filter(|(b, _, _)| *b != "other")
        .collect();
    res.total_networks = others.len() as u32;
    for band in ["2.4 GHz", "5 GHz", "6 GHz"] {
        let nets: Vec<&(&str, u32, i32)> = others.iter().filter(|o| o.0 == band).collect();
        let mut chans: Vec<u32> = nets.iter().map(|n| n.1).collect();
        // Candidates: the usual non-overlapping 2.4 GHz channels, common 5 GHz channels,
        // plus wherever networks are.
        match band {
            "2.4 GHz" => chans.extend([1, 6, 11]),
            "5 GHz" => chans.extend([36, 40, 44, 48, 149, 153, 157, 161]),
            _ => {}
        }
        if res.own_band == band {
            chans.push(res.own_channel);
        }
        chans.sort_unstable();
        chans.dedup();
        let score_at = |ch: u32| -> f64 {
            nets.iter()
                .filter(|n| if band == "2.4 GHz" { n.1.abs_diff(ch) <= 4 } else { n.1 == ch })
                .map(|n| weight(n.2) * if band == "2.4 GHz" && n.1 != ch { 0.5 } else { 1.0 })
                .sum()
        };
        let channels: Vec<Channel> = chans
            .iter()
            .map(|&ch| {
                let on: Vec<&&(&str, u32, i32)> = nets.iter().filter(|n| n.1 == ch).collect();
                Channel { channel: ch, networks: on.len() as u32, strongest_dbm: on.iter().map(|n| n.2).max().unwrap_or(-100), score: round1(score_at(ch)) }
            })
            .collect();
        let candidates: Vec<u32> = match band {
            "2.4 GHz" => vec![1, 6, 11],
            "5 GHz" => vec![36, 40, 44, 48, 149, 153, 157, 161],
            _ => chans.clone(),
        };
        let best = candidates.iter().map(|&c| (c, round1(score_at(c)))).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        if band == res.own_band {
            res.own_channel_neighbours = nets.iter().filter(|n| n.1 == res.own_channel).count() as u32;
            res.own_channel_score = round1(score_at(res.own_channel));
        }
        res.bands.push(Band { band: band.into(), networks: nets.len() as u32, channels, best: if nets.is_empty() && band == "6 GHz" { None } else { best } });
    }
    // Plain-language summary.
    let s = &mut res.summary;
    s.push(format!("{} other Wi-Fi networks in range: {}.", res.total_networks, res.bands.iter().map(|b| format!("{} on {}", b.networks, b.band)).collect::<Vec<_>>().join(", ")));
    if !res.own_band.is_empty() {
        s.push(format!(
            "Your network: {} channel {} (signal {} dBm), shared with {} other network(s) on the same channel (congestion score {:.1}).",
            res.own_band, res.own_channel, res.own_rssi, res.own_channel_neighbours, res.own_channel_score
        ));
        if res.own_band == "2.4 GHz" {
            s.push("You are on 2.4 GHz, the most crowded and slowest band: connect to the router's 5 GHz (or 6 GHz) network if it has one.".into());
        }
        if let Some(b) = res.bands.iter().find(|b| b.band == res.own_band) {
            if let Some((best, score)) = b.best {
                if best != res.own_channel && res.own_channel_score >= score + 1.0 {
                    s.push(format!("Channel {best} is quieter on {} (score {score:.1} vs {:.1}). Changing the router's channel needs access to its settings.", res.own_band, res.own_channel_score));
                } else {
                    s.push(format!("Your channel is among the quietest on {}.", res.own_band));
                }
            }
        }
        if res.own_rssi < -70 {
            s.push(format!("The signal is weak ({} dBm): move the PC or router closer, or remove obstacles between them.", res.own_rssi));
        }
    } else {
        s.push("Not connected to Wi-Fi (or the own network was not in the scan).".into());
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crowded_channel_gets_alternative() {
        let own = b"home".to_vec();
        let bssid = [1, 2, 3, 4, 5, 6];
        let mut e = vec![(own.clone(), bssid, -50, 5_220_000)]; // own: 5 GHz ch 44
        e.push((own.clone(), [9; 6], -55, 2_437_000)); // own 2.4 GHz radio: not a neighbour
        for i in 0..4 {
            e.push((format!("n{i}").into_bytes(), [i as u8; 6], -60, 5_220_000)); // 4 neighbours on 44
        }
        e.push((b"far".to_vec(), [7; 6], -85, 5_745_000)); // faint one on 149
        e.push((b"x".to_vec(), [8; 6], -50, 2_412_000)); // 2.4 GHz ch 1
        let r = summarise(&e, &own, &bssid);
        assert_eq!((r.own_band.as_str(), r.own_channel, r.own_channel_neighbours), ("5 GHz", 44, 4));
        assert_eq!(r.total_networks, 6);
        let b5 = r.bands.iter().find(|b| b.band == "5 GHz").unwrap();
        assert_ne!(b5.best.unwrap().0, 44);
        assert!(r.summary.iter().any(|s| s.contains("quieter")), "{:?}", r.summary);
        let b24 = r.bands.iter().find(|b| b.band == "2.4 GHz").unwrap();
        assert_ne!(b24.best.unwrap().0, 1, "channel 1 has a strong neighbour");
    }
}

//! WLAN AutoConfig service events via ETW (Microsoft-Windows-WLAN-AutoConfig and
//! Microsoft-Windows-WiFiNetworkManager). Unlike the WLAN API this is not gated by the
//! Windows Location privacy setting, so link quality, background-scan activity, roaming
//! and disconnects are still recorded when Location services are off.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use windows_sys::core::GUID;

use super::Running;
use super::etw::{ProviderSpec, RealtimeSession, SchemaCache};
use crate::app::App;
use crate::row;
use crate::session::csv_escape;

const WLAN_AUTOCONFIG: GUID = GUID::from_u128(0x9580d7dd_0379_4658_9870_d5be7d52d6de);
const WIFI_NETWORK_MANAGER: GUID = GUID::from_u128(0xe5c16d49_2464_4382_bb20_97a4b5465db9);
/// Microsoft-Windows-NDIS: requests (OIDs) Windows sends to network drivers and adapter
/// power transitions. Shows whether something on the PC tells the Wi-Fi driver to do
/// something (scan, change power state, ...) right before each freeze.
const NDIS: GUID = GUID::from_u128(0xcdead503_17f5_4a3e_b7ae_df8cc2902eb9);
const NDIS_EVENTS: &[(u16, &str)] = &[
    (10074, "power OID failed"),
    (10101, "OID completed by NDIS"),
    (10102, "OID completed by driver"),
    (10112, "OID completed by driver"),
    (10306, "power: working state requested"),
    (10307, "power: working state done"),
    (10308, "power: low-power state requested"),
    (10309, "power: low-power state done"),
];

/// Turns "Link Quality: %3 " into "Link Quality".
fn short(msg: &str) -> String {
    let m = msg.split(['%', '.', '\r', '\n']).next().unwrap_or("").trim().trim_end_matches([':', '=', ',']).trim();
    m.chars().take(90).collect()
}

pub fn start(app: Arc<App>) -> Result<Running> {
    let sink = app.session.sink("wlan_etw", "t_us,provider,id,event,values");
    let health = app.session.sink("etw_health_wlan", "t_us,session,events_lost,rt_buffers_lost,buffers_written");
    let mut cache = SchemaCache::default();
    let mut names: HashMap<(u32, u16), String> = HashMap::new();
    let a = app.clone();
    let specs = [
        ProviderSpec { name: "WLAN-AutoConfig", guid: WLAN_AUTOCONFIG, level: 5, any_keyword: u64::MAX, event_ids: vec![] },
        ProviderSpec { name: "WiFiNetworkManager", guid: WIFI_NETWORK_MANAGER, level: 5, any_keyword: u64::MAX, event_ids: vec![] },
    ];
    let sess = RealtimeSession::start("LagScope-WLAN", &specs, move |ev| {
        let t = a.session.clock.us_from_raw(ev.timestamp());
        let prov = if ev.provider().data1 == WLAN_AUTOCONFIG.data1 { "autoconfig" } else { "wifimgr" };
        let Some(schema) = cache.get(ev) else {
            row!(sink, "{t},{prov},{},,", ev.id());
            return;
        };
        let name = names
            .entry((ev.provider().data1, ev.id()))
            .or_insert_with(|| {
                let s = short(&schema.message);
                if s.is_empty() { format!("{} {}", schema.task_name, schema.event_name).trim().to_string() } else { s }
            })
            .clone();
        let vals: Vec<String> = schema
            .values(ev.data())
            .into_iter()
            .filter(|(k, _)| !matches!(k.as_str(), "InterfaceGuid" | "InterfaceDescription" | "SSID" | "BSSID" | "ProfileName" | "Ssid" | "Bssid"))
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let vals = vals.join(" ");
        row!(sink, "{t},{prov},{},{},{}", ev.id(), csv_escape(&name), csv_escape(&vals));
        let lname = name.to_ascii_lowercase();
        if lname.starts_with("link quality") {
            if let Some(q) = vals.strip_prefix("Quality=") {
                a.live.lock().unwrap().wifi_etw_quality = q.parse().ok();
            }
        } else if lname.contains("roam") || lname.contains("disconnect") {
            a.session.event("wifi", format!("{name} {vals}"));
        }
    })?;
    Ok(super::etw_running("wlan_etw", app, sess, health))
}

/// Separate session so a failure to enable NDIS tracing cannot cost the WLAN events.
pub fn start_ndis(app: Arc<App>) -> Result<Running> {
    let sink = app.session.sink("wifi_driver", "t_us,id,pid,event,oid,values");
    let health = app.session.sink("etw_health_ndis", "t_us,session,events_lost,rt_buffers_lost,buffers_written");
    let mut cache = SchemaCache::default();
    let a = app.clone();
    // Keywords: Request (0x1) | Power (0x10).
    let specs = [ProviderSpec { name: "NDIS", guid: NDIS, level: 5, any_keyword: 0x11, event_ids: NDIS_EVENTS.iter().map(|e| e.0).collect() }];
    let sess = RealtimeSession::start("LagScope-NDIS", &specs, move |ev| {
        let Some(&(_, name)) = NDIS_EVENTS.iter().find(|e| e.0 == ev.id()) else { return };
        let t = a.session.clock.us_from_raw(ev.timestamp());
        let vals = cache.get(ev).map(|s| s.values(ev.data())).unwrap_or_default();
        let oid = vals
            .iter()
            .find(|(k, _)| k == "Oid" || k == "RequestType")
            .and_then(|(_, v)| v.parse::<u64>().ok())
            .map(|o| format!("0x{o:08x}"))
            .unwrap_or_default();
        // OID_GEN_STATISTICS: Windows polls every adapter's counters many times a second.
        if oid == "0x00020106" {
            return;
        }
        let all: Vec<String> = vals.iter().map(|(k, v)| format!("{k}={v}")).collect();
        row!(sink, "{t},{},{},{name},{},{}", ev.id(), ev.pid(), csv_escape(&oid), csv_escape(&all.join(" ")));
    })?;
    Ok(super::etw_running("wifi_driver", app, sess, health))
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_names() {
        assert_eq!(super::short("Link Quality: %3 "), "Link Quality");
        assert_eq!(super::short("Enable AutoConfig background scan. Interface = %2, Enabled = %3. "), "Enable AutoConfig background scan");
    }
}

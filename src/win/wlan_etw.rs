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

#[cfg(test)]
mod tests {
    #[test]
    fn short_names() {
        assert_eq!(super::short("Link Quality: %3 "), "Link Quality");
        assert_eq!(super::short("Enable AutoConfig background scan. Interface = %2, Enabled = %3. "), "Enable AutoConfig background scan");
    }
}

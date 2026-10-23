//! Wi-Fi telemetry via the WLAN API (all passive: we never call WlanScan or
//! WlanGetNetworkBssList, which could trigger the very scans we want to observe).
//!
//! * `wifi.csv` (4 Hz): RSSI, signal quality, PHY rates, channel, BSSID hash and, when the
//!   driver supports it, per-interval frame / retry / failure counter deltas.
//! * `wifi_events.csv`: every ACM/MSM notification (scan complete, roaming, signal
//!   change, disconnects ...). Background scans are a classic cause of periodic spikes.
//!
//! The BSSID is hashed with a per-session salt so a shared session cannot be used to
//! geolocate the access point.

use std::ffi::c_void;
use std::sync::Arc;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::WiFi::*;
use windows_sys::core::GUID;

use super::{Running, from_wide_buf, spawn};
use crate::app::App;
use crate::row;
use crate::session::Sink;

struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

struct NotifyCtx {
    app: Arc<App>,
    sink: Arc<Sink>,
}

fn acm_name(code: u32) -> &'static str {
    match code {
        1 => "autoconf_enabled",
        2 => "autoconf_disabled",
        3 => "background_scan_enabled",
        4 => "background_scan_disabled",
        5 => "bss_type_change",
        6 => "power_setting_change",
        7 => "scan_complete",
        8 => "scan_fail",
        9 => "connection_start",
        10 => "connection_complete",
        11 => "connection_attempt_fail",
        12 => "filter_list_change",
        13 => "interface_arrival",
        14 => "interface_removal",
        15 => "profile_change",
        16 => "profile_name_change",
        17 => "profiles_exhausted",
        18 => "network_not_available",
        19 => "network_available",
        20 => "disconnecting",
        21 => "disconnected",
        22 => "adhoc_network_state_change",
        23 => "profile_unblocked",
        24 => "screen_power_change",
        25 => "profile_blocked",
        26 => "scan_list_refresh",
        27 => "operation_mode_change",
        _ => "acm_other",
    }
}

fn msm_name(code: u32) -> &'static str {
    match code {
        1 => "associating",
        2 => "associated",
        3 => "authenticating",
        4 => "connected",
        5 => "roaming_start",
        6 => "roaming_end",
        7 => "radio_state_change",
        8 => "signal_quality_change",
        9 => "disassociating",
        10 => "disconnected",
        11 => "peer_join",
        12 => "peer_leave",
        13 => "adapter_removal",
        14 => "adapter_operation_mode_change",
        15 => "link_degraded",
        16 => "link_improved",
        _ => "msm_other",
    }
}

unsafe extern "system" fn on_notify(data: *mut L2_NOTIFICATION_DATA, ctx: *mut c_void) {
    unsafe {
        if data.is_null() || ctx.is_null() {
            return;
        }
        let ctx = &*(ctx as *const NotifyCtx);
        let d = &*data;
        let t = ctx.app.session.clock.now_us();
        let (src, name) = match d.NotificationSource {
            WLAN_NOTIFICATION_SOURCE_ACM => ("acm", acm_name(d.NotificationCode)),
            WLAN_NOTIFICATION_SOURCE_MSM => ("msm", msm_name(d.NotificationCode)),
            WLAN_NOTIFICATION_SOURCE_ONEX => ("onex", "onex"),
            WLAN_NOTIFICATION_SOURCE_SECURITY => ("security", "security"),
            WLAN_NOTIFICATION_SOURCE_IHV => ("ihv", "ihv"),
            _ => ("other", "other"),
        };
        // signal_quality_change carries the new quality (0-100) as a ULONG.
        let extra = if src == "msm" && d.NotificationCode == 8 && d.dwDataSize >= 4 && !d.pData.is_null() {
            (*(d.pData as *const u32)).to_string()
        } else {
            String::new()
        };
        row!(ctx.sink, "{t},{src},{},{name},{extra}", d.NotificationCode);
        let important = matches!(
            (src, d.NotificationCode),
            ("acm", 8 | 11 | 20 | 21) | ("msm", 5 | 6 | 9 | 10 | 15)
        );
        if important {
            ctx.app.session.event("wifi", name);
        }
    }
}

fn query<T: Copy>(h: HANDLE, guid: &GUID, opcode: i32) -> Option<T> {
    unsafe {
        let mut size = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let mut vt = 0;
        if WlanQueryInterface(h, guid, opcode, std::ptr::null(), &mut size, &mut data, &mut vt) != 0 || data.is_null() {
            return None;
        }
        let v = if size as usize >= std::mem::size_of::<T>() { Some(*(data as *const T)) } else { None };
        WlanFreeMemory(data);
        v
    }
}

#[derive(Default, Clone, Copy)]
struct Counters {
    tx: u64,
    rx: u64,
    retry: u64,
    multi_retry: u64,
    failed: u64,
    ack_fail: u64,
    fcs_err: u64,
    rts_fail: u64,
    dup: u64,
    decrypt_fail: u64,
}

/// True if querying the connection fails with ERROR_ACCESS_DENIED (Location privacy gate).
fn last_error_denied(h: HANDLE, guid: &GUID) -> bool {
    unsafe {
        let mut size = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let mut vt = 0;
        let r = WlanQueryInterface(h, guid, wlan_intf_opcode_current_connection, std::ptr::null(), &mut size, &mut data, &mut vt);
        if !data.is_null() {
            WlanFreeMemory(data);
        }
        r == 5
    }
}

fn statistics(h: HANDLE, guid: &GUID) -> Option<Counters> {
    unsafe {
        let mut size = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let mut vt = 0;
        if WlanQueryInterface(h, guid, wlan_intf_opcode_statistics, std::ptr::null(), &mut size, &mut data, &mut vt) != 0
            || data.is_null()
        {
            return None;
        }
        let s = &*(data as *const WLAN_STATISTICS);
        let n = s.dwNumberOfPhys as usize;
        let max_n = (size as usize).saturating_sub(std::mem::offset_of!(WLAN_STATISTICS, PhyCounters))
            / std::mem::size_of::<WLAN_PHY_FRAME_STATISTICS>();
        let phys = std::slice::from_raw_parts(s.PhyCounters.as_ptr(), n.min(max_n));
        let mut c = Counters {
            decrypt_fail: s.MacUcastCounters.ullDecryptFailureCount,
            ..Default::default()
        };
        for p in phys {
            c.tx += p.ullTransmittedFrameCount;
            c.rx += p.ullReceivedFrameCount;
            c.retry += p.ullRetryCount;
            c.multi_retry += p.ullMultipleRetryCount;
            c.failed += p.ullFailedCount;
            c.ack_fail += p.ullACKFailureCount;
            c.fcs_err += p.ullFCSErrorCount;
            c.rts_fail += p.ullRTSFailureCount;
            c.dup += p.ullFrameDuplicateCount;
        }
        if phys.is_empty() {
            c.tx = s.MacUcastCounters.ullTransmittedFrameCount;
            c.rx = s.MacUcastCounters.ullReceivedFrameCount;
        }
        WlanFreeMemory(data);
        Some(c)
    }
}

fn fnv(salt: u64, bytes: &[u8]) -> u32 {
    let mut h: u64 = 0xcbf29ce484222325 ^ salt;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h ^ (h >> 32)) as u32
}

pub fn start(app: Arc<App>) -> Option<Running> {
    let mut ver = 0u32;
    let mut h: HANDLE = std::ptr::null_mut();
    if unsafe { WlanOpenHandle(2, std::ptr::null(), &mut ver, &mut h) } != 0 {
        app.session.event("collector", "wifi: WLAN service not available (no Wi-Fi adapter?)");
        return None;
    }
    let handle = Arc::new(Handle(h));

    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    let mut ifaces: Vec<(GUID, String, i32)> = vec![];
    unsafe {
        if WlanEnumInterfaces(h, std::ptr::null(), &mut list) == 0 && !list.is_null() {
            let l = &*list;
            let items = std::slice::from_raw_parts(l.InterfaceInfo.as_ptr(), l.dwNumberOfItems as usize);
            for i in items {
                ifaces.push((i.InterfaceGuid, from_wide_buf(&i.strInterfaceDescription), i.isState));
            }
            WlanFreeMemory(list as *const c_void);
        }
    }
    if ifaces.is_empty() {
        app.session.event("collector", "wifi: no wireless interfaces");
        unsafe { WlanCloseHandle(h, std::ptr::null()) };
        return None;
    }
    let (guid, desc, _) = ifaces
        .iter()
        .find(|(_, _, st)| *st == wlan_interface_state_connected)
        .cloned()
        .unwrap_or_else(|| ifaces[0].clone());

    let bg_scan: Option<u32> = query(h, &guid, wlan_intf_opcode_background_scan_enabled);
    let streaming: Option<u32> = query(h, &guid, wlan_intf_opcode_media_streaming_mode);
    app.session.meta_set(
        "wifi",
        serde_json::json!({
            "interface": desc,
            "interfaces": ifaces.len(),
            "background_scan_enabled": bg_scan,
            "media_streaming_mode": streaming,
        }),
    );
    app.session.event(
        "collector",
        format!("wifi: '{desc}' background_scan={bg_scan:?} media_streaming_mode={streaming:?}"),
    );

    let events = app.session.sink("wifi_events", "t_us,source,code,name,data");
    let ctx = Box::into_raw(Box::new(NotifyCtx { app: app.clone(), sink: events }));
    let mut prev = 0u32;
    let reg = unsafe {
        WlanRegisterNotification(
            h,
            WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
            0,
            Some(on_notify),
            ctx as *const c_void,
            std::ptr::null(),
            &mut prev,
        )
    };
    if reg == 5 {
        app.session.event("collector", "wifi: WLAN notifications blocked by Location privacy setting (using ETW instead)");
    } else if reg != 0 {
        app.session.event("collector", format!("wifi: notification registration failed ({reg})"));
    }

    let sink = app.session.sink(
        "wifi",
        "t_us,connected,bssid_hash,phy,channel,rssi_dbm,quality,rx_mbps,tx_mbps,d_tx,d_rx,d_retry,d_multi_retry,d_failed,d_ack_fail,d_fcs_err,d_rts_fail,d_dup,d_decrypt_fail",
    );
    let salt = (crate::clock::raw_now() as u64) ^ ((std::process::id() as u64) << 32);
    let a = app.clone();
    let hh = handle.clone();
    let poller = spawn("wifi", move || {
        let h = hh.0;
        let mut prev: Option<Counters> = None;
        let mut last_bssid = 0u32;
        let mut stats_supported = true;
        let mut blocked = false;
        while a.sleep_ms(if blocked { 5000 } else { 250 }) {
            let t = a.session.clock.now_us();
            let conn: Option<WLAN_CONNECTION_ATTRIBUTES> = query(h, &guid, wlan_intf_opcode_current_connection);
            let denied = conn.is_none() && last_error_denied(h, &guid);
            if denied != blocked {
                blocked = denied;
                a.live.lock().unwrap().wifi_location_blocked = denied;
                a.session.meta_set("wifi_location_blocked", denied.into());
                if denied {
                    a.session.event(
                        "wifi",
                        "Windows blocks Wi-Fi details (RSSI, PHY rate, roaming) because Location services are off. \
                         Optional: Settings > Privacy & security > Location > turn on 'Location services' and \
                         'Let desktop apps access your location'. Link quality and scan events are still recorded.",
                    );
                } else {
                    a.session.event("wifi", "Wi-Fi details available (Location permission granted)");
                }
            }
            if denied {
                continue;
            }
            let Some(c) = conn.filter(|c| c.isState == wlan_interface_state_connected) else {
                row!(sink, "{t},0,,,,,,,,,,,,,,,,,");
                a.live.lock().unwrap().wifi = Some("disconnected".into());
                continue;
            };
            let assoc = c.wlanAssociationAttributes;
            let bssid = fnv(salt, &assoc.dot11Bssid);
            if last_bssid != 0 && bssid != last_bssid {
                a.session.event("wifi", format!("BSSID changed (roamed to another AP/band): {last_bssid:08x} -> {bssid:08x}"));
            }
            last_bssid = bssid;
            let rssi: Option<i32> = query(h, &guid, wlan_intf_opcode_rssi);
            let ch: Option<u32> = query(h, &guid, wlan_intf_opcode_channel_number);
            let cur = if stats_supported { statistics(h, &guid) } else { None };
            if cur.is_none() {
                stats_supported = false;
            }
            let delta = match (prev, cur) {
                (Some(p), Some(c)) => format!(
                    "{},{},{},{},{},{},{},{},{},{}",
                    c.tx.wrapping_sub(p.tx),
                    c.rx.wrapping_sub(p.rx),
                    c.retry.wrapping_sub(p.retry),
                    c.multi_retry.wrapping_sub(p.multi_retry),
                    c.failed.wrapping_sub(p.failed),
                    c.ack_fail.wrapping_sub(p.ack_fail),
                    c.fcs_err.wrapping_sub(p.fcs_err),
                    c.rts_fail.wrapping_sub(p.rts_fail),
                    c.dup.wrapping_sub(p.dup),
                    c.decrypt_fail.wrapping_sub(p.decrypt_fail)
                ),
                _ => ",,,,,,,,,".into(),
            };
            prev = cur;
            let (rx, tx) = (assoc.ulRxRate / 1000, assoc.ulTxRate / 1000);
            row!(
                sink,
                "{t},1,{bssid:08x},{},{},{},{},{rx},{tx},{delta}",
                assoc.dot11PhyType,
                ch.map(|c| c.to_string()).unwrap_or_default(),
                rssi.map(|r| r.to_string()).unwrap_or_default(),
                assoc.wlanSignalQuality
            );
            a.live.lock().unwrap().wifi = Some(format!(
                "signal {} dBm ({}%), channel {}, link {rx}/{tx} Mbit/s",
                rssi.map(|r| r.to_string()).unwrap_or("?".into()),
                assoc.wlanSignalQuality,
                ch.map(|c| c.to_string()).unwrap_or("?".into()),
            ));
        }
    });

    let ctx = ctx as usize;
    let stopper = move || unsafe {
        let mut prev = 0u32;
        WlanRegisterNotification(handle.0, WLAN_NOTIFICATION_SOURCE_NONE, 0, None, std::ptr::null(), std::ptr::null(), &mut prev);
        WlanCloseHandle(handle.0, std::ptr::null());
        drop(Box::from_raw(ctx as *mut NotifyCtx));
    };
    let mut r = Running::threads("wifi", vec![poller]);
    r.set_stop(stopper);
    Some(r)
}

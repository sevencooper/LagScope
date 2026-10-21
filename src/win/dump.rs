//! `lagscope dump-etw <net|dxgi> [secs]`: prints decoded ETW events so field layouts can be
//! verified on a real machine (debugging aid; not used during capture).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use super::etw::{self, ProviderSpec, RealtimeSession, SchemaCache};

pub fn parse_guid(s: &str) -> Option<windows_sys::core::GUID> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return None;
    }
    Some(windows_sys::core::GUID::from_u128(u128::from_str_radix(&h, 16).ok()?))
}

pub fn run(kind: &str, secs: u64, max_print: u64) -> Result<()> {
    let specs: Vec<ProviderSpec> = if kind.contains('{') || kind.contains(',') || kind.len() >= 32 {
        kind.split(',')
            .map(|g| {
                parse_guid(g)
                    .map(|guid| ProviderSpec { name: "custom", guid, level: 5, any_keyword: u64::MAX, event_ids: vec![] })
                    .ok_or_else(|| anyhow::anyhow!("bad GUID '{g}'"))
            })
            .collect::<Result<_>>()?
    } else {
        vec![spec_for(kind)?]
    };
    run_specs(specs, secs, max_print)
}

fn spec_for(kind: &str) -> Result<ProviderSpec> {
    Ok(match kind {
        "net" => ProviderSpec {
            name: "Kernel-Network",
            guid: etw::KERNEL_NETWORK,
            level: 5,
            any_keyword: 0x30,
            event_ids: vec![10, 11, 26, 27, 42, 43, 58, 59],
        },
        "dxgi" => ProviderSpec {
            name: "DXGI",
            guid: etw::DXGI,
            level: 5,
            any_keyword: u64::MAX,
            event_ids: vec![42, 43],
        },
        other => anyhow::bail!("unknown provider '{other}' (use net, dxgi or a GUID list)"),
    })
}

fn run_specs(specs: Vec<ProviderSpec>, secs: u64, max_print: u64) -> Result<()> {
    let printed = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let (p2, t2) = (printed.clone(), total.clone());
    let mut cache = SchemaCache::default();
    let mut last_ts: std::collections::HashMap<u16, i64> = Default::default();
    let clock = crate::clock::Clock::new();
    let mut sess = RealtimeSession::start("LagScope-Dump", &specs, move |ev| {
        t2.fetch_add(1, Ordering::Relaxed);
        let n = p2.fetch_add(1, Ordering::Relaxed);
        let prev = last_ts.insert(ev.id(), ev.timestamp());
        if n >= max_print {
            return;
        }
        let Some(schema) = cache.get(ev) else {
            println!("id={} v={} (no schema) len={}", ev.id(), ev.version(), ev.data().len());
            return;
        };
        let d = ev.data();
        let mut parts = Vec::new();
        for f in &schema.fields {
            let b = &d[f.offset.min(d.len())..(f.offset + f.size).min(d.len())];
            let v = match (b.len(), f.out_type) {
                (_, _) if f.in_type == 1 || f.in_type == 2 => String::from("<str>"),
                (4, 20) => format!("{}", std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3])), // IPV4
                (2, 22) => format!("{}", u16::from_be_bytes([b[0], b[1]])), // PORT
                (16, 21) => format!("{}", std::net::Ipv6Addr::from(<[u8; 16]>::try_from(b).unwrap())),
                (1, _) => format!("{}", b[0]),
                (2, _) => format!("{}", u16::from_le_bytes([b[0], b[1]])),
                (4, _) => format!("{}", u32::from_le_bytes(b.try_into().unwrap())),
                (8, _) => format!("{:#x}", u64::from_le_bytes(b.try_into().unwrap())),
                _ => format!("{b:02x?}"),
            };
            parts.push(format!("{}[{}:{}/{}]={v}", f.name, f.in_type, f.out_type, f.size));
        }
        let dt = prev.map(|p| clock.raw_to_us_delta(ev.timestamp() - p)).unwrap_or(-1);
        let g = ev.provider();
        println!(
            "t={}us prov={:08x} dt_same_id={}us id={} v={} op={} {}/{} hdr_pid={} tid={} len={} trunc={} | {}",
            clock.us_from_raw(ev.timestamp()),
            g.data1,
            dt,
            ev.id(),
            ev.version(),
            ev.opcode(),
            schema.task_name,
            schema.event_name,
            ev.pid(),
            ev.tid(),
            d.len(),
            schema.truncated,
            parts.join(" ")
        );
        if schema.truncated {
            let vals: Vec<String> = schema.values(d).into_iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("    values: {}", vals.join(" "));
        }
        if !schema.message.is_empty() {
            println!("    msg: {}", schema.message.replace(['\r', '\n'], " "));
        }
    })?;
    println!("session started, filters: {:?}; capturing {secs}s ...", sess.filtered);
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let st = sess.stats();
    sess.stop();
    println!("total events: {}  lost: {}  rt_buffers_lost: {}", total.load(Ordering::Relaxed), st.events_lost, st.rt_buffers_lost);
    Ok(())
}

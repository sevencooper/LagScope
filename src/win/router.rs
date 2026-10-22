//! Router identification, all on the local network: the gateway's UPnP device description
//! (manufacturer / model), its admin page title / server header, and the ISP from the
//! reverse DNS name of the first ISP hop. Matched against a small table of known ISP
//! gateways for model-specific advice. Nothing leaves the local network except one
//! reverse DNS lookup.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct RouterInfo {
    pub gateway: String,
    pub manufacturer: String,
    pub model: String,
    pub model_number: String,
    pub description: String,
    pub web_title: String,
    pub web_server: String,
    pub isp_host: String,
    pub isp: String,
    /// The admin page only says to use the ISP's app (e.g. newer Xfinity gateways).
    pub app_only: bool,
    /// Best one-line name, e.g. "Technicolor CGM4981COM (Xfinity XB8)".
    pub name: String,
    /// Advice for this router / ISP.
    pub notes: Vec<String>,
}

fn http_get(addr: SocketAddr, host: &str, path: &str, cookie: &str) -> Option<(String, String)> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    // Some gateways take several seconds to render their status page.
    s.set_read_timeout(Some(Duration::from_secs(8))).ok()?;
    let cookie = if cookie.is_empty() { String::new() } else { format!("Cookie: {cookie}\r\n") };
    let req = format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: LagScope\r\n{cookie}Connection: close\r\n\r\n");
    if std::env::var_os("LAGSCOPE_DEBUG").is_some() {
        eprintln!("request: {req:?}");
    }
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let start = Instant::now();
    let mut chunk = [0u8; 8192];
    while buf.len() < 256 * 1024 && start.elapsed() < Duration::from_secs(10) {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Err(e) => {
                if std::env::var_os("LAGSCOPE_DEBUG").is_some() {
                    eprintln!("read error after {:?}: {e}", start.elapsed());
                }
                break;
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Some((head.to_string(), body.to_string()))
}

fn header(head: &str, name: &str) -> String {
    head.lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default()
}

fn tag(xml: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    xml.find(&open)
        .and_then(|i| xml[i + open.len()..].find(&close).map(|j| xml[i + open.len()..i + open.len() + j].trim().to_string()))
        .unwrap_or_default()
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(80).collect::<String>().trim().to_string()
}

/// UPnP: ask the gateway to describe itself (SSDP M-SEARCH, then its description XML).
fn upnp(gw: Ipv4Addr, info: &mut RouterInfo) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(400)));
    for st in ["urn:schemas-upnp-org:device:InternetGatewayDevice:1", "upnp:rootdevice"] {
        let msg = format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: {st}\r\n\r\n");
        let _ = sock.send_to(msg.as_bytes(), "239.255.255.250:1900");
    }
    let start = Instant::now();
    let mut buf = [0u8; 2048];
    let mut location = None;
    while start.elapsed() < Duration::from_millis(2500) && location.is_none() {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        if from.ip() != std::net::IpAddr::V4(gw) {
            continue;
        }
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let loc = header(&text, "location");
        if loc.starts_with("http://") {
            location = Some(loc);
        }
    }
    let Some(loc) = location else { return };
    // http://host:port/path
    let rest = &loc["http://".len()..];
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let addr: SocketAddr = if hostport.contains(':') { hostport.parse().ok() } else { format!("{hostport}:80").parse().ok() }.unwrap_or(SocketAddr::from((gw, 80)));
    if addr.ip() != std::net::IpAddr::V4(gw) {
        return;
    }
    if let Some((_, xml)) = http_get(addr, hostport, &path, "") {
        info.manufacturer = clean(&tag(&xml, "manufacturer"));
        info.model = clean(&tag(&xml, "modelName"));
        info.model_number = clean(&tag(&xml, "modelNumber"));
        info.description = clean(&tag(&xml, "modelDescription"));
    }
}

/// Router makers recognisable from the admin page (copyright lines, logos).
const VENDORS: &[&str] = &[
    "ARRIS", "Technicolor", "Vantiva", "Sagemcom", "Humax", "Nokia", "Pace", "Netgear", "NETGEAR", "ASUS", "TP-Link", "Linksys", "eero", "Google", "Ubiquiti", "MikroTik", "AVM", "FRITZ!Box", "Huawei", "ZTE", "Zyxel", "Actiontec", "Greenwave", "Askey", "Calix", "Adtran", "Hitron", "Ubee", "Motorola", "Xfinity",
];

fn web(gw: Ipv4Addr, info: &mut RouterInfo) {
    let host = gw.to_string();
    let mut path = "/".to_string();
    let mut cookie = String::new();
    for _ in 0..3 {
        let Some((head, body)) = http_get(SocketAddr::from((gw, 80)), &host, &path, &cookie) else { return };
        let server = clean(&header(&head, "server"));
        if !server.is_empty() {
            info.web_server = server;
        }
        let t = if tag(&body, "title").is_empty() { tag(&body, "TITLE") } else { tag(&body, "title") };
        if !t.is_empty() {
            info.web_title = clean(&t);
        }
        if body.to_lowercase().contains("download the xfinity app") {
            info.app_only = true;
        }
        if info.manufacturer.is_empty() {
            if let Some(v) = VENDORS.iter().find(|v| body.contains(*v)) {
                info.manufacturer = v.to_string();
            }
        }
        // Keep the session cookie and follow a same-host redirect (also <meta refresh>).
        let c = header(&head, "set-cookie");
        if let Some(first) = c.split(';').next().filter(|x| x.contains('=')) {
            cookie = first.trim().to_string();
        }
        let mut loc = header(&head, "location");
        if loc.is_empty() {
            if let Some(i) = body.to_lowercase().find("url=") {
                loc = body[i + 4..].split(['"', '\'', '>', ' ']).next().unwrap_or("").to_string();
            }
        }
        if std::env::var_os("LAGSCOPE_DEBUG").is_some() {
            eprintln!("GET {path} cookie={cookie:?} -> {} bytes head, {} bytes body, title={:?}, location={loc:?}", head.len(), body.len(), info.web_title);
        }
        if !info.web_title.is_empty() && info.web_title != "Redirect" {
            return;
        }
        let next = loc.strip_prefix(&format!("http://{host}")).map(String::from).or_else(|| loc.starts_with('/').then(|| loc.clone()));
        match next {
            Some(p) if !p.is_empty() && p != path => path = p,
            _ => return,
        }
    }
}

/// Reverse DNS name of an address (e.g. the first ISP hop), or "".
pub fn reverse_dns(ip: Ipv4Addr) -> String {
    use windows_sys::Win32::Networking::WinSock::{AF_INET, GetNameInfoW, IN_ADDR, NI_NAMEREQD, SOCKADDR, SOCKADDR_IN, WSADATA, WSAStartup};
    unsafe {
        let mut wsa: WSADATA = std::mem::zeroed();
        WSAStartup(0x0202, &mut wsa);
        let mut sa: SOCKADDR_IN = std::mem::zeroed();
        sa.sin_family = AF_INET;
        sa.sin_addr = IN_ADDR { S_un: std::mem::transmute::<u32, _>(u32::from_ne_bytes(ip.octets())) };
        let mut host = [0u16; 256];
        let r = GetNameInfoW(&sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN>() as i32, host.as_mut_ptr(), host.len() as u32, std::ptr::null_mut(), 0, NI_NAMEREQD as i32);
        if r != 0 {
            return String::new();
        }
        super::from_wide_buf(&host)
    }
}

/// Known ISPs by reverse-DNS domain.
const ISPS: &[(&str, &str)] = &[
    ("comcast.net", "Comcast (Xfinity)"),
    ("att.net", "AT&T"),
    ("sbcglobal.net", "AT&T"),
    ("verizon.net", "Verizon"),
    ("charter.com", "Spectrum"),
    ("rr.com", "Spectrum"),
    ("spectrum.com", "Spectrum"),
    ("cox.net", "Cox"),
    ("centurylink.net", "CenturyLink / Lumen"),
    ("frontiernet.net", "Frontier"),
    ("t-mobile.com", "T-Mobile"),
    ("googlefiber.net", "Google Fiber"),
    ("virginm.net", "Virgin Media"),
    ("btcentralplus.com", "BT"),
    ("telekom", "Deutsche Telekom"),
];

/// Model / ISP specific notes (kept to things we know to be true).
fn notes(info: &RouterInfo) -> Vec<String> {
    let all = format!("{} {} {} {} {} {} {}", info.manufacturer, info.model, info.model_number, info.description, info.web_title, info.web_server, info.isp).to_lowercase();
    let mut v = Vec::new();
    if all.contains("cgm4981") || all.contains("xb8") {
        v.push("Xfinity XB8 gateway: settings are only available in the Xfinity app (its local admin page is turned off). In LagScope recordings this model's 6 GHz Wi-Fi froze for ~140 ms every 10 s while 5 GHz was nearly clean - prefer 5 GHz; in the app, turn off WiFi Motion and restart the gateway; Xfinity can replace the unit.".into());
    } else if all.contains("xfinity") || all.contains("comcast") || info.app_only {
        v.push(format!(
            "Xfinity / Comcast gateway: Wi-Fi settings, restarts and WiFi Motion are in the Xfinity app (account holder){}. Xfinity replaces faulty gateways for free.",
            if info.app_only { "; this gateway's local admin page is turned off" } else { "" }
        ));
    }
    if all.contains("at&t") || all.contains("bgw") || all.contains("pace") || all.contains("arris") && info.isp.contains("AT&T") {
        v.push(format!("AT&T gateway: settings are at http://{} (the Device Access Code is on the gateway's sticker).", info.gateway));
    }
    if info.isp.contains("Verizon") {
        v.push(format!("Verizon: router settings at http://{} (admin password on the router's sticker) or in the My Verizon / My Fios app.", info.gateway));
    }
    if info.isp.contains("Spectrum") {
        v.push("Spectrum: Spectrum-supplied routers are managed in the My Spectrum app.".into());
    }
    if info.isp.contains("Cox") {
        v.push("Cox: Panoramic gateways are managed in the Panoramic Wifi app.".into());
    }
    if v.is_empty() && !info.manufacturer.is_empty() {
        v.push(format!("Router settings are usually at http://{} (login on the router's sticker or in its app).", info.gateway));
    }
    v
}

/// Identifies the gateway. `hop2` = first ISP hop (for the ISP name). Takes a few seconds.
pub fn identify(gw: Ipv4Addr, hop2: Option<Ipv4Addr>) -> RouterInfo {
    let mut info = RouterInfo { gateway: gw.to_string(), ..Default::default() };
    upnp(gw, &mut info);
    web(gw, &mut info);
    if info.web_title.is_empty() && info.manufacturer.is_empty() {
        // Busy or slow gateways sometimes drop the first page request.
        std::thread::sleep(Duration::from_secs(3));
        web(gw, &mut info);
    }
    if let Some(h) = hop2 {
        info.isp_host = reverse_dns(h);
        let lh = info.isp_host.to_lowercase();
        info.isp = ISPS.iter().find(|(d, _)| lh.contains(d)).map(|(_, n)| n.to_string()).unwrap_or_default();
    }
    let model = [info.model.as_str(), info.model_number.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
    info.name = match (info.manufacturer.is_empty(), model.is_empty()) {
        (false, false) => format!("{} {model}", info.manufacturer),
        (true, false) => model,
        (false, true) => format!("{} gateway", info.manufacturer),
        _ => "unknown router".into(),
    };
    if !info.isp.is_empty() && !info.name.contains(&info.isp) {
        info.name = format!("{} ({})", info.name, info.isp);
    }
    if info.model_number.to_lowercase().contains("cgm4981") || info.model.to_lowercase().contains("cgm4981") {
        info.name.push_str(" (Xfinity XB8)");
    }
    info.notes = notes(&info);
    info
}

//! 局域网设备发现：mDNS（Bonjour / Avahi）查询 + 本网段常用端口扫描。
//! 开发板拿到 DHCP 地址后不知道 IP 时很有用。

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// 扫描的端口和用途
pub const PORTS: &[(u16, &str)] = &[
    (22, "SSH"),
    (23, "Telnet"),
    (80, "HTTP"),
    (443, "HTTPS"),
    (502, "Modbus"),
    (554, "RTSP"),
    (1883, "MQTT"),
    (5555, "ADB"),
    (6053, "ESPHome"),
    (8000, "HTTP"),
    (8080, "HTTP"),
    (8266, "ESP OTA"),
    (8883, "MQTTS"),
];

#[derive(Clone)]
pub struct Host {
    pub ip: Ipv4Addr,
    /// mDNS 主机名（xxx.local）
    pub name: String,
    /// (服务类型, 实例名, 端口)
    pub services: Vec<(String, String, u16)>,
    pub ports: Vec<u16>,
}

impl Host {
    fn new(ip: Ipv4Addr) -> Host {
        Host {
            ip,
            name: String::new(),
            services: vec![],
            ports: vec![],
        }
    }
}

#[derive(Default)]
pub struct State {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub net: String,
    pub hosts: BTreeMap<Ipv4Addr, Host>,
    pub err: Option<String>,
}

pub type Shared = Arc<Mutex<State>>;

/// 开始扫描（mDNS 和端口扫描并行）
pub fn start(st: &Shared, ctx: egui::Context) {
    {
        let mut s = st.lock().unwrap();
        if s.running {
            return;
        }
        *s = State { running: true, ..Default::default() };
    }
    let Some(me) = super::local_ipv4() else {
        let mut s = st.lock().unwrap();
        s.running = false;
        s.err = Some(crate::i18n::t("没有找到局域网地址（网线 / Wi-Fi 是否已连接？）").into());
        return;
    };
    let o = me.octets();
    st.lock().unwrap().net = format!("{}.{}.{}.0/24", o[0], o[1], o[2]);
    let (st1, ctx1) = (st.clone(), ctx.clone());
    std::thread::spawn(move || mdns(&st1, &ctx1));
    let st2 = st.clone();
    std::thread::spawn(move || {
        sweep(me, &st2, &ctx);
        st2.lock().unwrap().running = false;
        ctx.request_repaint();
    });
}

fn sweep(me: Ipv4Addr, st: &Shared, ctx: &egui::Context) {
    let o = me.octets();
    // 本机也扫：顺便确认自己开的服务（比如工具箱里的 HTTP / TFTP）对外可见
    let jobs: Vec<(Ipv4Addr, u16)> = (1..=254u8).flat_map(|h| PORTS.iter().map(move |&(p, _)| (Ipv4Addr::new(o[0], o[1], o[2], h), p))).collect();
    st.lock().unwrap().total = jobs.len();
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..96)
        .map(|_| {
            let (jobs, next, st, ctx) = (jobs.clone(), next.clone(), st.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(ip, port)) = jobs.get(i) else { break };
                let open = TcpStream::connect_timeout(&SocketAddr::V4(SocketAddrV4::new(ip, port)), Duration::from_millis(350)).is_ok();
                let mut s = st.lock().unwrap();
                s.done += 1;
                if open {
                    let h = s.hosts.entry(ip).or_insert_with(|| Host::new(ip));
                    if ip == me && h.name.is_empty() {
                        h.name = crate::i18n::t("本机").to_string();
                    }
                    if !h.ports.contains(&port) {
                        h.ports.push(port);
                        h.ports.sort();
                    }
                    ctx.request_repaint();
                }
                if s.done % 64 == 0 {
                    ctx.request_repaint();
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
}

// ------------------------------------------------------------ mDNS

const MDNS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
const TYPES: &[&str] = &[
    "_services._dns-sd._udp.local",
    "_http._tcp.local",
    "_arduino._tcp.local",
    "_esphomelib._tcp.local",
    "_workstation._tcp.local",
    "_ssh._tcp.local",
    "_mqtt._tcp.local",
    "_ipp._tcp.local",
    "_googlecast._tcp.local",
    "_hap._tcp.local",
    "_airplay._tcp.local",
    "_smb._tcp.local",
];

pub fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut p = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.').filter(|l| !l.is_empty()) {
        p.push(label.len() as u8);
        p.extend_from_slice(label.as_bytes());
    }
    p.push(0);
    p.extend_from_slice(&qtype.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p
}

/// 读一个（可能压缩的）域名，返回 (名字, 名字之后的位置)
fn read_name(p: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut out: Vec<String> = vec![];
    let mut end = None;
    for _ in 0..64 {
        let len = *p.get(i)? as usize;
        if len == 0 {
            return Some((out.join("."), end.unwrap_or(i + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let off = ((len & 0x3F) << 8) | *p.get(i + 1)? as usize;
            end.get_or_insert(i + 2);
            i = off;
            continue;
        }
        out.push(String::from_utf8_lossy(p.get(i + 1..i + 1 + len)?).into_owned());
        i += 1 + len;
    }
    None
}

pub enum Rec {
    Ptr(String, String),
    Srv(String, String, u16),
    A(String, Ipv4Addr),
}

/// 解析回答、权威、附加三段里的 PTR / SRV / A 记录
pub fn parse(p: &[u8]) -> Vec<Rec> {
    let mut out = vec![];
    if p.len() < 12 {
        return out;
    }
    let qd = u16::from_be_bytes([p[4], p[5]]) as usize;
    let rr = [6, 8, 10].iter().map(|&k| u16::from_be_bytes([p[k], p[k + 1]]) as usize).sum::<usize>();
    let mut i = 12;
    for _ in 0..qd {
        let Some((_, j)) = read_name(p, i) else { return out };
        i = j + 4;
    }
    for _ in 0..rr {
        let Some((name, j)) = read_name(p, i) else { return out };
        if j + 10 > p.len() {
            return out;
        }
        let typ = u16::from_be_bytes([p[j], p[j + 1]]);
        let len = u16::from_be_bytes([p[j + 8], p[j + 9]]) as usize;
        let d = j + 10;
        if d + len > p.len() {
            return out;
        }
        match typ {
            12 => {
                if let Some((v, _)) = read_name(p, d) {
                    out.push(Rec::Ptr(name, v));
                }
            }
            33 if len >= 6 => {
                let port = u16::from_be_bytes([p[d + 4], p[d + 5]]);
                if let Some((target, _)) = read_name(p, d + 6) {
                    out.push(Rec::Srv(name, target, port));
                }
            }
            1 if len == 4 => out.push(Rec::A(name, Ipv4Addr::new(p[d], p[d + 1], p[d + 2], p[d + 3]))),
            _ => {}
        }
        i = d + len;
    }
    out
}

fn mdns(st: &Shared, ctx: &egui::Context) {
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
    sock.set_read_timeout(Some(Duration::from_millis(150))).ok();
    let mut asked: Vec<String> = vec![];
    let ask = |name: &str, asked: &mut Vec<String>| {
        if !asked.iter().any(|a| a == name) {
            asked.push(name.to_string());
            let _ = sock.send_to(&query(name, 12), MDNS);
        }
    };
    for t in TYPES {
        ask(t, &mut asked);
    }
    // 记录要等全了才知道「实例 → 主机 → IP」，先攒起来
    let mut srv: BTreeMap<String, (String, u16)> = BTreeMap::new();
    let mut inst: Vec<(String, String)> = vec![];
    let mut addr: BTreeMap<String, Ipv4Addr> = BTreeMap::new();
    let t0 = Instant::now();
    let mut buf = [0u8; 9000];
    while t0.elapsed() < Duration::from_secs(4) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        for r in parse(&buf[..n]) {
            match r {
                Rec::Ptr(name, v) if name == "_services._dns-sd._udp.local" => ask(&v, &mut asked),
                Rec::Ptr(typ, v) => {
                    if !inst.iter().any(|(_, i)| *i == v) {
                        inst.push((typ, v.clone()));
                        // 实例的 SRV 没跟着回来时再单独问
                        let _ = sock.send_to(&query(&v, 33), MDNS);
                    }
                }
                Rec::Srv(name, target, port) => {
                    srv.insert(name, (target, port));
                }
                Rec::A(name, ip) => {
                    addr.insert(name, ip);
                }
            }
        }
        // 没有 A 记录时用回包的来源地址
        if let SocketAddr::V4(f) = from {
            for (t, _) in srv.values() {
                addr.entry(t.clone()).or_insert(*f.ip());
            }
        }
        merge(st, &inst, &srv, &addr);
        ctx.request_repaint();
    }
}

fn merge(st: &Shared, inst: &[(String, String)], srv: &BTreeMap<String, (String, u16)>, addr: &BTreeMap<String, Ipv4Addr>) {
    let mut s = st.lock().unwrap();
    for (typ, name) in inst {
        let Some((target, port)) = srv.get(name) else { continue };
        let Some(&ip) = addr.get(target) else { continue };
        let h = s.hosts.entry(ip).or_insert_with(|| Host::new(ip));
        if h.name.is_empty() {
            h.name = target.clone();
        }
        let short_type = typ.trim_end_matches(".local").to_string();
        let short_name = name.split("._").next().unwrap_or(name).to_string();
        if !h.services.iter().any(|(t, n, _)| *t == short_type && *n == short_name) {
            h.services.push((short_type, short_name, *port));
        }
    }
}

pub fn port_name(p: u16) -> &'static str {
    PORTS.iter().find(|(x, _)| *x == p).map_or("", |(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mdns_answer() {
        // 手工拼一个回答：PTR _http._tcp.local -> dev._http._tcp.local，SRV -> esp.local:80，A esp.local
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 3, 0, 0, 0, 0];
        let name = |s: &str| {
            let mut v = vec![];
            for l in s.split('.') {
                v.push(l.len() as u8);
                v.extend_from_slice(l.as_bytes());
            }
            v.push(0);
            v
        };
        let ptr_name_off = p.len();
        p.extend(name("_http._tcp.local"));
        p.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 120]);
        let target = {
            let mut v = vec![3];
            v.extend_from_slice(b"dev");
            v.extend_from_slice(&[0xC0, ptr_name_off as u8]);
            v
        };
        p.extend_from_slice(&(target.len() as u16).to_be_bytes());
        let inst_off = p.len();
        p.extend(target);
        // SRV
        p.extend_from_slice(&[0xC0, inst_off as u8, 0, 33, 0, 1, 0, 0, 0, 120]);
        let host = name("esp.local");
        p.extend_from_slice(&((6 + host.len()) as u16).to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 0, 0, 80]);
        let host_off = p.len();
        p.extend(host);
        // A
        p.extend_from_slice(&[0xC0, host_off as u8, 0, 1, 0, 1, 0, 0, 0, 120, 0, 4, 192, 168, 1, 50]);
        let recs = parse(&p);
        assert!(matches!(&recs[0], Rec::Ptr(a, b) if a == "_http._tcp.local" && b == "dev._http._tcp.local"));
        assert!(matches!(&recs[1], Rec::Srv(a, b, 80) if a == "dev._http._tcp.local" && b == "esp.local"));
        assert!(matches!(&recs[2], Rec::A(a, ip) if a == "esp.local" && *ip == Ipv4Addr::new(192, 168, 1, 50)));
    }
}

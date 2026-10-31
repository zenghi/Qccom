//! 仿真里的联网：模块（ESP-01、4G……）和带 WiFi 的开发板发出的 TCP / UDP / HTTP / MQTT 请求，在后台线程里真的去连。
//! 连本机就是 QCCOM 自己的 TCP 服务器、MQTT 服务器、HTTP 服务器（工具箱里开），也能连外网。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::circuit::atdev::{NetCmd, NetEvt};
use crate::circuit::mcu::NetReq;
use crate::i18n::{t, tf};
use crate::net::mqtt;

pub enum Out {
    Evt(u32, NetEvt),
    Log(u32, String),
}

enum Op {
    Send(Vec<u8>),
    Close,
    Pub(String, Vec<u8>),
    Sub(String),
}

pub struct NetLink {
    tx: Sender<Out>,
    rx: Receiver<Out>,
    links: HashMap<(u32, u8), Sender<Op>>,
    mqtt: HashMap<u32, Sender<Op>>,
}

impl Default for NetLink {
    fn default() -> Self {
        let (tx, rx) = channel();
        NetLink {
            tx,
            rx,
            links: HashMap::new(),
            mqtt: HashMap::new(),
        }
    }
}

impl NetLink {
    /// 模块的联网指令
    pub fn cmd(&mut self, id: u32, c: NetCmd, ctx: &egui::Context) {
        let out = self.tx.clone();
        let ctx = ctx.clone();
        match c {
            NetCmd::Open { link, udp, host, port } => {
                let (otx, orx) = channel();
                if let Some(old) = self.links.insert((id, link), otx) {
                    old.send(Op::Close).ok();
                }
                std::thread::Builder::new().name("sim-net".into()).spawn(move || socket(id, link, udp, host, port, orx, out, ctx)).ok();
            }
            NetCmd::Send { link, data } => {
                if let Some(s) = self.links.get(&(id, link)) {
                    s.send(Op::Send(data)).ok();
                }
            }
            NetCmd::Close { link } => {
                if let Some(s) = self.links.remove(&(id, link)) {
                    s.send(Op::Close).ok();
                }
            }
            NetCmd::Http { method, url, body } => {
                std::thread::Builder::new()
                    .name("sim-http".into())
                    .spawn(move || {
                        let (status, body) = http(id, &method, &url, body, &out);
                        out.send(Out::Evt(id, NetEvt::Http { status, body })).ok();
                        ctx.request_repaint();
                    })
                    .ok();
            }
            NetCmd::MqttOpen { host, port, client, user, pass } => {
                let (otx, orx) = channel();
                if let Some(old) = self.mqtt.insert(id, otx) {
                    old.send(Op::Close).ok();
                }
                std::thread::Builder::new()
                    .name("sim-mqtt".into())
                    .spawn(move || mqtt_run(id, host, port, client, user, pass, orx, out, ctx))
                    .ok();
            }
            NetCmd::MqttPub { topic, payload } => {
                if let Some(s) = self.mqtt.get(&id) {
                    s.send(Op::Pub(topic, payload)).ok();
                }
            }
            NetCmd::MqttSub { topic } => {
                if let Some(s) = self.mqtt.get(&id) {
                    s.send(Op::Sub(topic)).ok();
                }
            }
            NetCmd::MqttClose => {
                if let Some(s) = self.mqtt.remove(&id) {
                    s.send(Op::Close).ok();
                }
            }
        }
    }

    /// 带 WiFi 的开发板规则里的「发 MQTT / HTTP / TCP」：发一次就断开
    pub fn oneshot(&mut self, id: u32, r: NetReq, ctx: &egui::Context) {
        let out = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("sim-net1".into())
            .spawn(move || {
                match r {
                    NetReq::Tcp { host, port, text } => match crate::net::connect_tcp(&host, port) {
                        Ok(mut s) => {
                            s.write_all(text.as_bytes()).ok();
                            s.set_read_timeout(Some(Duration::from_millis(300))).ok();
                            let mut buf = [0u8; 512];
                            let reply = match s.read(&mut buf) {
                                Ok(n) if n > 0 => format!(" ← {}", String::from_utf8_lossy(&buf[..n]).trim()),
                                _ => String::new(),
                            };
                            out.send(Out::Log(id, format!("☁ TCP {host}:{port} → {}{reply}", text.trim()))).ok();
                        }
                        Err(e) => {
                            out.send(Out::Log(id, format!("☁ TCP {host}:{port} {}：{e}", t("连接失败")))).ok();
                        }
                    },
                    NetReq::Http { url, body } => {
                        let method = if body.is_empty() { "GET" } else { "POST" };
                        let (status, _) = http(id, method, &url, body.into_bytes(), &out);
                        if status == 0 {
                            // http() 已经记了原因
                        }
                    }
                    NetReq::Mqtt { host, port, topic, payload } => {
                        let r = (|| -> Result<(), String> {
                            let mut s = crate::net::connect_tcp(&host, port)?;
                            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                            let cfg = crate::net::MqttConfig {
                                host: host.clone(),
                                port,
                                keepalive: 30,
                                clean: true,
                                ..Default::default()
                            };
                            s.write_all(&mqtt::connect(&cfg, "qccom-sim")).map_err(|e| e.to_string())?;
                            let mut buf = [0u8; 64];
                            let n = s.read(&mut buf).map_err(|e| e.to_string())?;
                            if n < 4 || buf[0] >> 4 != 2 || buf[3] != 0 {
                                return Err(t("服务器没有按 MQTT 协议回应").into());
                            }
                            s.write_all(&mqtt::publish(0, &topic, payload.as_bytes(), 0, false)).map_err(|e| e.to_string())?;
                            s.write_all(&[0xe0, 0]).ok();
                            Ok(())
                        })();
                        let m = match r {
                            Ok(()) => format!("☁ MQTT {host}:{port} {topic} ← {payload}"),
                            Err(e) => format!("☁ MQTT {host}:{port} {}：{e}", t("连接失败")),
                        };
                        out.send(Out::Log(id, m)).ok();
                    }
                }
                ctx.request_repaint();
            })
            .ok();
    }

    pub fn drain(&mut self) -> Vec<Out> {
        let mut v = vec![];
        while let Ok(o) = self.rx.try_recv() {
            // 线程自己断开的连接从表里去掉
            if let Out::Evt(id, NetEvt::Closed { link }) | Out::Evt(id, NetEvt::Opened { link, ok: false }) = &o {
                self.links.remove(&(*id, *link));
            }
            if let Out::Evt(id, NetEvt::MqttClosed) | Out::Evt(id, NetEvt::MqttOpened { ok: false }) = &o {
                self.mqtt.remove(id);
            }
            v.push(o);
        }
        v
    }

    /// 模块断电、删掉、仿真重来：关掉它的所有连接
    pub fn close_all(&mut self) {
        for (_, s) in self.links.drain() {
            s.send(Op::Close).ok();
        }
        for (_, s) in self.mqtt.drain() {
            s.send(Op::Close).ok();
        }
    }
}

fn http(id: u32, method: &str, url: &str, body: Vec<u8>, out: &Sender<Out>) -> (u16, Vec<u8>) {
    let req = crate::api::Request {
        method: method.into(),
        url: url.into(),
        body_kind: if body.is_empty() { crate::api::BodyKind::None } else { crate::api::BodyKind::Text },
        body: String::from_utf8_lossy(&body).to_string(),
        ..Default::default()
    };
    match crate::api::send(&req, &[], 10, true) {
        Ok(r) => {
            out.send(Out::Log(id, format!("☁ HTTP {method} {url} → {} ({} B)", r.status, r.body.len()))).ok();
            (r.status, r.body)
        }
        Err(e) => {
            out.send(Out::Log(id, format!("☁ HTTP {method} {url} {}：{e}", t("连接失败")))).ok();
            (0, vec![])
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn socket(id: u32, link: u8, udp: bool, host: String, port: u16, rx: Receiver<Op>, out: Sender<Out>, ctx: egui::Context) {
    enum S {
        Tcp(TcpStream),
        Udp(UdpSocket),
    }
    let proto = if udp { "UDP" } else { "TCP" };
    let opened = if udp {
        UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect((host.as_str(), port)).map(|_| s))
            .map(S::Udp)
            .map_err(|e| e.to_string())
    } else {
        crate::net::connect_tcp(&host, port).map(S::Tcp)
    };
    let mut s = match opened {
        Ok(s) => s,
        Err(e) => {
            out.send(Out::Log(id, format!("☁ {proto} {host}:{port} {}：{e}", t("连接失败")))).ok();
            out.send(Out::Evt(id, NetEvt::Opened { link, ok: false })).ok();
            ctx.request_repaint();
            return;
        }
    };
    match &s {
        S::Tcp(x) => x.set_read_timeout(Some(Duration::from_millis(20))).ok(),
        S::Udp(x) => x.set_read_timeout(Some(Duration::from_millis(20))).ok(),
    };
    out.send(Out::Log(id, tf("☁ {} {} 已连接", &[&proto, &format!("{host}:{port}")]))).ok();
    out.send(Out::Evt(id, NetEvt::Opened { link, ok: true })).ok();
    ctx.request_repaint();
    let mut buf = vec![0u8; 4096];
    loop {
        loop {
            match rx.try_recv() {
                Ok(Op::Send(d)) => {
                    let r = match &mut s {
                        S::Tcp(x) => x.write_all(&d),
                        S::Udp(x) => x.send(&d).map(|_| ()),
                    };
                    if r.is_err() {
                        out.send(Out::Evt(id, NetEvt::Closed { link })).ok();
                        return;
                    }
                }
                Ok(Op::Close) | Err(TryRecvError::Disconnected) => return,
                Ok(_) => {}
                Err(TryRecvError::Empty) => break,
            }
        }
        let r = match &mut s {
            S::Tcp(x) => x.read(&mut buf),
            S::Udp(x) => x.recv(&mut buf),
        };
        match r {
            Ok(0) if !udp => {
                out.send(Out::Log(id, tf("☁ {} {} 对方断开了", &[&proto, &format!("{host}:{port}")]))).ok();
                out.send(Out::Evt(id, NetEvt::Closed { link })).ok();
                ctx.request_repaint();
                return;
            }
            Ok(n) => {
                out.send(Out::Evt(id, NetEvt::Data { link, data: buf[..n].to_vec() })).ok();
                ctx.request_repaint();
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) if udp => {}
            Err(_) => {
                out.send(Out::Evt(id, NetEvt::Closed { link })).ok();
                ctx.request_repaint();
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn mqtt_run(id: u32, host: String, port: u16, client: String, user: String, pass: String, rx: Receiver<Op>, out: Sender<Out>, ctx: egui::Context) {
    let fail = |e: String| {
        out.send(Out::Log(id, format!("☁ MQTT {host}:{port} {}：{e}", t("连接失败")))).ok();
        out.send(Out::Evt(id, NetEvt::MqttOpened { ok: false })).ok();
        ctx.request_repaint();
    };
    let mut s = match crate::net::connect_tcp(&host, port) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let cfg = crate::net::MqttConfig {
        host: host.clone(),
        port,
        keepalive: 60,
        clean: true,
        user,
        pass,
        ..Default::default()
    };
    if let Err(e) = s.write_all(&mqtt::connect(&cfg, &client)) {
        return fail(e.to_string());
    }
    let mut buf: Vec<u8> = vec![];
    let mut tmp = vec![0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some((first, body, n)) = mqtt::parse(&buf) {
            if first >> 4 != 2 || body.len() < 2 || body[1] != 0 {
                return fail(t("服务器没有按 MQTT 协议回应").into());
            }
            buf.drain(..n);
            break;
        }
        if Instant::now() > deadline {
            return fail(t("等待服务器响应超时").into());
        }
        match s.read(&mut tmp) {
            Ok(0) => return fail(t("服务器关闭了连接").into()),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => {}
        }
    }
    s.set_read_timeout(Some(Duration::from_millis(20))).ok();
    out.send(Out::Log(id, tf("☁ MQTT {} 已连接", &[&format!("{host}:{port}")]))).ok();
    out.send(Out::Evt(id, NetEvt::MqttOpened { ok: true })).ok();
    ctx.request_repaint();
    let mut pid: u16 = 1;
    let mut last_ping = Instant::now();
    let closed = |out: &Sender<Out>| {
        out.send(Out::Evt(id, NetEvt::MqttClosed)).ok();
        ctx.request_repaint();
    };
    loop {
        loop {
            let r = match rx.try_recv() {
                Ok(Op::Pub(topic, payload)) => {
                    out.send(Out::Log(id, format!("☁ MQTT {topic} ← {}", String::from_utf8_lossy(&payload)))).ok();
                    s.write_all(&mqtt::publish(0, &topic, &payload, 0, false))
                }
                Ok(Op::Sub(topic)) => {
                    pid = pid.wrapping_add(1).max(1);
                    s.write_all(&mqtt::subscribe(pid, &topic, 0))
                }
                Ok(Op::Close) | Err(TryRecvError::Disconnected) => {
                    s.write_all(&[0xe0, 0]).ok();
                    return;
                }
                Ok(Op::Send(_)) => Ok(()),
                Err(TryRecvError::Empty) => break,
            };
            if r.is_err() {
                return closed(&out);
            }
        }
        if last_ping.elapsed() > Duration::from_secs(30) {
            last_ping = Instant::now();
            if s.write_all(&[0xc0, 0]).is_err() {
                return closed(&out);
            }
        }
        match s.read(&mut tmp) {
            Ok(0) => return closed(&out),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return closed(&out),
        }
        while let Some((first, body, n)) = mqtt::parse(&buf) {
            if first >> 4 == 3 && body.len() >= 2 {
                let tl = u16::from_be_bytes([body[0], body[1]]) as usize;
                let qos = (first >> 1) & 3;
                if body.len() >= 2 + tl {
                    let topic = String::from_utf8_lossy(&body[2..2 + tl]).to_string();
                    let start = 2 + tl + if qos > 0 { 2 } else { 0 };
                    let payload = body.get(start..).unwrap_or_default().to_vec();
                    if qos == 1 && body.len() >= 2 + tl + 2 {
                        s.write_all(&[0x40, 2, body[2 + tl], body[3 + tl]]).ok();
                    }
                    out.send(Out::Evt(id, NetEvt::MqttMsg { topic, payload })).ok();
                    ctx.request_repaint();
                }
            }
            buf.drain(..n);
        }
    }
}

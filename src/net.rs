//! 网络连接：TCP 客户端、TCP 服务器、UDP、MQTT。
//!
//! 和串口一样各开一个后台线程，通过同一套 Cmd / Evt 和界面通信，
//! 所以日志、终端、快捷命令、录制这些功能对网络连接全都能用。

use crate::i18n::{t, tf};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::serial::{Cmd, Connection, Evt};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Transport {
    #[default]
    Serial,
    TcpClient,
    TcpServer,
    Udp,
    Mqtt,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub client_id: String,
    pub user: String,
    pub pass: String,
    pub keepalive: u16,
    pub clean: bool,
    pub subs: Vec<String>,
    pub pub_topic: String,
    pub qos: u8,
    pub retain: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "broker.emqx.io".into(),
            port: 1883,
            tls: false,
            client_id: String::new(),
            user: String::new(),
            pass: String::new(),
            keepalive: 60,
            clean: true,
            subs: vec!["qccom/#".into()],
            pub_topic: "qccom/test".into(),
            qos: 0,
            retain: false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetConfig {
    pub tcp_host: String,
    pub tcp_port: u16,
    pub server_port: u16,
    pub udp_local: u16,
    pub udp_host: String,
    pub udp_port: u16,
    pub mqtt: MqttConfig,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            tcp_host: "192.168.1.100".into(),
            tcp_port: 8080,
            server_port: 8080,
            udp_local: 9000,
            udp_host: "192.168.1.100".into(),
            udp_port: 9000,
            mqtt: MqttConfig::default(),
        }
    }
}

impl NetConfig {
    /// 状态栏、日志里显示的连接名
    pub fn label(&self, t: Transport) -> String {
        match t {
            Transport::Serial => String::new(),
            Transport::TcpClient => format!("tcp://{}:{}", self.tcp_host.trim(), self.tcp_port),
            Transport::TcpServer => tf("tcp 监听 :{}", &[&self.server_port]),
            Transport::Udp => format!("udp :{} → {}:{}", self.udp_local, self.udp_host.trim(), self.udp_port),
            Transport::Mqtt => {
                let scheme = if self.mqtt.tls { "mqtts" } else { "mqtt" };
                format!("{scheme}://{}:{}", self.mqtt.host.trim(), self.mqtt.port)
            }
        }
    }
}

pub fn open(gen: u64, kind: Transport, cfg: NetConfig, tx: Sender<Evt>, ctx: egui::Context) -> Result<Connection, String> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let label = cfg.label(kind);
    let io = Io { gen, tx, ctx, cmd: cmd_rx };
    // 连接放到线程里做：DNS、握手都可能要几秒，界面不能等
    thread::Builder::new()
        .name("net-io".into())
        .spawn(move || {
            let r = match kind {
                Transport::TcpClient => tcp_client(&io, &cfg, &label),
                Transport::TcpServer => tcp_server(&io, &cfg, &label),
                Transport::Udp => udp(&io, &cfg, &label),
                Transport::Mqtt => mqtt::run(&io, &cfg.mqtt, &label),
                Transport::Serial => Err(t("内部错误").into()),
            };
            io.closed(r.err());
        })
        .map_err(|e| e.to_string())?;
    Ok(Connection { cmd: cmd_tx })
}

pub(crate) struct Io {
    gen: u64,
    tx: Sender<Evt>,
    ctx: egui::Context,
    cmd: Receiver<Cmd>,
}

pub(crate) enum Next {
    Cmd(Cmd),
    Idle,
    Quit,
}

impl Io {
    fn send(&self, e: Evt) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint_after(Duration::from_millis(33));
    }
    fn opened(&self, label: &str) {
        self.send(Evt::Opened(self.gen, label.to_string()));
    }
    fn data(&self, d: &[u8]) {
        self.send(Evt::Data(d.to_vec()));
    }
    fn packet(&self, tag: String, d: &[u8]) {
        self.send(Evt::Packet(tag, d.to_vec()));
    }
    fn sys(&self, s: impl Into<String>) {
        self.send(Evt::Sys(s.into()));
    }
    fn closed(&self, err: Option<String>) {
        let _ = self.tx.send(Evt::Closed(self.gen, err));
        self.ctx.request_repaint();
    }
    fn next(&self) -> Next {
        match self.cmd.try_recv() {
            Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => Next::Quit,
            Ok(c) => Next::Cmd(c),
            Err(TryRecvError::Empty) => Next::Idle,
        }
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err(t("请填写地址").into());
    }
    (host, port)
        .to_socket_addrs()
        .map_err(|e| tf("解析 {} 失败：{}", &[&host, &e]))?
        .next()
        .ok_or_else(|| tf("解析 {} 失败", &[&host]))
}

pub(crate) fn connect_tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addr = resolve(host, port)?;
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(6)).map_err(|e| match e.kind() {
        io::ErrorKind::TimedOut => tf("连接 {} 超时", &[&addr]),
        io::ErrorKind::ConnectionRefused => tf("{} 拒绝连接（端口没开？）", &[&addr]),
        _ => tf("连接 {} 失败：{}", &[&addr, &e]),
    })?;
    let _ = s.set_nodelay(true);
    Ok(s)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}

fn tcp_client(io: &Io, cfg: &NetConfig, label: &str) -> Result<(), String> {
    let mut s = connect_tcp(&cfg.tcp_host, cfg.tcp_port)?;
    s.set_read_timeout(Some(Duration::from_millis(20))).ok();
    io.opened(label);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return Err(t("对方关闭了连接").into()),
            Ok(n) => io.data(&buf[..n]),
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
        loop {
            match io.next() {
                Next::Cmd(Cmd::Send(d)) => s.write_all(&d).map_err(|e| e.to_string())?,
                Next::Cmd(_) => {}
                Next::Idle => break,
                Next::Quit => return Ok(()),
            }
        }
    }
}

fn tcp_server(io: &Io, cfg: &NetConfig, label: &str) -> Result<(), String> {
    let l = TcpListener::bind(("0.0.0.0", cfg.server_port)).map_err(|e| tf("监听端口 {} 失败：{}", &[&cfg.server_port, &e]))?;
    l.set_nonblocking(true).ok();
    io.opened(label);
    io.sys(tf("正在监听 0.0.0.0:{}，等待客户端连接…", &[&cfg.server_port]));
    io.send(Evt::Peers(0));
    let mut clients: Vec<(TcpStream, SocketAddr)> = vec![];
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut busy = false;
        while let Ok((s, a)) = l.accept() {
            s.set_nonblocking(true).ok();
            let _ = s.set_nodelay(true);
            io.sys(tf("客户端 {} 已连接", &[&a]));
            clients.push((s, a));
            io.send(Evt::Peers(clients.len()));
        }
        let mut gone = vec![];
        for (i, (s, a)) in clients.iter_mut().enumerate() {
            match s.read(&mut buf) {
                Ok(0) => gone.push((i, tf("客户端 {} 已断开", &[&a]))),
                Ok(n) => {
                    busy = true;
                    io.data(&buf[..n]);
                }
                Err(e) if is_timeout(&e) => {}
                Err(e) => gone.push((i, tf("客户端 {} 断开：{}", &[&a, &e]))),
            }
        }
        for (i, msg) in gone.into_iter().rev() {
            clients.remove(i);
            io.sys(msg);
            io.send(Evt::Peers(clients.len()));
        }
        loop {
            match io.next() {
                Next::Cmd(Cmd::Send(d)) => {
                    if clients.is_empty() {
                        io.sys(t("还没有客户端连上，数据没有发出"));
                    }
                    // 发给所有客户端（广播）
                    for (s, _) in clients.iter_mut() {
                        s.set_nonblocking(false).ok();
                        let _ = s.write_all(&d);
                        s.set_nonblocking(true).ok();
                    }
                }
                Next::Cmd(_) => {}
                Next::Idle => break,
                Next::Quit => return Ok(()),
            }
        }
        if !busy {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn udp(io: &Io, cfg: &NetConfig, label: &str) -> Result<(), String> {
    let sock = UdpSocket::bind(("0.0.0.0", cfg.udp_local)).map_err(|e| tf("绑定 UDP 端口 {} 失败：{}", &[&cfg.udp_local, &e]))?;
    sock.set_read_timeout(Some(Duration::from_millis(20))).ok();
    let _ = sock.set_broadcast(true);
    let mut target = if cfg.udp_host.trim().is_empty() { None } else { Some(resolve(&cfg.udp_host, cfg.udp_port)?) };
    io.opened(label);
    if let Ok(a) = sock.local_addr() {
        io.sys(tf(
            "UDP 已绑定 {}{}",
            &[&a, &target.map(|x| tf("，发送目标 {}", &[&x])).unwrap_or_else(|| t("，收到数据后回复给发送方").into())],
        ));
    }
    let mut buf = vec![0u8; 65536];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if cfg.udp_host.trim().is_empty() {
                    target = Some(from);
                }
                io.packet(from.to_string(), &buf[..n]);
            }
            Err(e) if is_timeout(&e) => {}
            // Windows 上对方端口没开会收到 ICMP 导致的 ConnectionReset，忽略
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => return Err(e.to_string()),
        }
        loop {
            match io.next() {
                Next::Cmd(Cmd::Send(d)) => match target {
                    Some(t) => {
                        sock.send_to(&d, t).map_err(|e| e.to_string())?;
                    }
                    None => io.sys(t("还不知道发给谁：请填写目标地址，或等对方先发数据过来")),
                },
                Next::Cmd(_) => {}
                Next::Idle => break,
                Next::Quit => return Ok(()),
            }
        }
    }
}

// ---------------------------------------------------------------- TLS

pub(crate) trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

#[cfg(windows)]
pub(crate) fn tls(host: &str, tcp: TcpStream) -> Result<Box<dyn Stream>, String> {
    let c = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let s = c.connect(host.trim(), tcp).map_err(|e| tf("TLS 握手失败：{}", &[&e]))?;
    Ok(Box::new(s))
}

#[cfg(not(windows))]
pub(crate) fn tls(host: &str, tcp: TcpStream) -> Result<Box<dyn Stream>, String> {
    use std::sync::Arc;
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.trim().to_string()).map_err(|e| e.to_string())?;
    let conn = rustls::ClientConnection::new(Arc::new(cfg), name).map_err(|e| e.to_string())?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    // 先把握手做完，错误能早点报出来
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock).map_err(|e| tf("TLS 握手失败：{}", &[&e]))?;
    }
    Ok(Box::new(s))
}

// ---------------------------------------------------------------- MQTT 3.1.1

pub mod mqtt {
    use super::*;

    pub fn encode_len(mut n: usize, out: &mut Vec<u8>) {
        loop {
            let mut b = (n % 128) as u8;
            n /= 128;
            if n > 0 {
                b |= 0x80;
            }
            out.push(b);
            if n == 0 {
                break;
            }
        }
    }

    fn str_field(s: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }

    pub fn packet(first: u8, body: &[u8]) -> Vec<u8> {
        let mut p = vec![first];
        encode_len(body.len(), &mut p);
        p.extend_from_slice(body);
        p
    }

    pub fn connect(c: &MqttConfig, client_id: &str) -> Vec<u8> {
        let mut b = vec![];
        str_field(b"MQTT", &mut b);
        b.push(4);
        let mut flags = 0u8;
        if c.clean {
            flags |= 0x02;
        }
        if !c.user.is_empty() {
            flags |= 0x80;
            if !c.pass.is_empty() {
                flags |= 0x40;
            }
        }
        b.push(flags);
        b.extend_from_slice(&c.keepalive.to_be_bytes());
        str_field(client_id.as_bytes(), &mut b);
        if !c.user.is_empty() {
            str_field(c.user.as_bytes(), &mut b);
            if !c.pass.is_empty() {
                str_field(c.pass.as_bytes(), &mut b);
            }
        }
        packet(0x10, &b)
    }

    pub fn subscribe(id: u16, topic: &str, qos: u8) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        str_field(topic.as_bytes(), &mut b);
        b.push(qos.min(2));
        packet(0x82, &b)
    }

    pub fn unsubscribe(id: u16, topic: &str) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        str_field(topic.as_bytes(), &mut b);
        packet(0xa2, &b)
    }

    pub fn publish(id: u16, topic: &str, payload: &[u8], qos: u8, retain: bool) -> Vec<u8> {
        let mut b = vec![];
        str_field(topic.as_bytes(), &mut b);
        if qos > 0 {
            b.extend_from_slice(&id.to_be_bytes());
        }
        b.extend_from_slice(payload);
        packet(0x30 | (qos.min(2) << 1) | retain as u8, &b)
    }

    /// 从缓冲区里拆出一个完整报文：(首字节, 报文体, 总长度)
    pub fn parse(buf: &[u8]) -> Option<(u8, &[u8], usize)> {
        let first = *buf.first()?;
        let (mut len, mut mul, mut i) = (0usize, 1usize, 1usize);
        loop {
            let b = *buf.get(i)?;
            len += (b & 0x7f) as usize * mul;
            mul *= 128;
            i += 1;
            if b & 0x80 == 0 {
                break;
            }
            if i > 4 {
                return None;
            }
        }
        let end = i + len;
        (buf.len() >= end).then(|| (first, &buf[i..end], end))
    }

    fn connack_err(rc: u8) -> String {
        match rc {
            1 => t("服务器不支持 MQTT 3.1.1").into(),
            2 => t("客户端 ID 被拒绝").into(),
            3 => t("服务器不可用").into(),
            4 => t("用户名或密码错误").into(),
            5 => t("未授权").into(),
            n => tf("连接被拒绝（代码 {}）", &[&n]),
        }
    }

    pub fn run(io: &Io, c: &MqttConfig, label: &str) -> Result<(), String> {
        let tcp = connect_tcp(&c.host, c.port)?;
        tcp.set_read_timeout(Some(Duration::from_secs(8))).ok();
        let raw = tcp.try_clone().map_err(|e| e.to_string())?;
        let mut s: Box<dyn Stream> = if c.tls { tls(&c.host, tcp)? } else { Box::new(tcp) };
        let client_id = if c.client_id.trim().is_empty() {
            format!(
                "qccom-{:06x}",
                (std::process::id() as u64 ^ Instant::now().elapsed().as_nanos() as u64 ^ chrono::Local::now().timestamp_subsec_nanos() as u64) & 0xffffff
            )
        } else {
            c.client_id.trim().to_string()
        };
        s.write_all(&connect(c, &client_id)).map_err(|e| e.to_string())?;

        let mut buf: Vec<u8> = vec![];
        let mut tmp = vec![0u8; 16 * 1024];
        // 等 CONNACK
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some((first, body, n)) = parse(&buf) {
                if first >> 4 != 2 || body.len() < 2 {
                    return Err(t("服务器没有按 MQTT 协议回应").into());
                }
                if body[1] != 0 {
                    return Err(connack_err(body[1]));
                }
                buf.drain(..n);
                break;
            }
            if Instant::now() > deadline {
                return Err(t("等待服务器响应超时").into());
            }
            match s.read(&mut tmp) {
                Ok(0) => return Err(t("服务器关闭了连接").into()),
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        raw.set_read_timeout(Some(Duration::from_millis(20))).ok();
        io.opened(label);
        io.sys(tf("MQTT 已连接，客户端 ID：{}", &[&client_id]));

        let mut next_id: u16 = 1;
        let mut id = || {
            next_id = next_id.wrapping_add(1).max(1);
            next_id
        };
        let mut pending_subs: Vec<(u16, String)> = vec![];
        for t in c.subs.iter().filter(|t| !t.trim().is_empty()) {
            let i = id();
            s.write_all(&subscribe(i, t.trim(), c.qos)).map_err(|e| e.to_string())?;
            pending_subs.push((i, t.trim().to_string()));
        }
        let (mut pub_topic, mut qos, mut retain) = (c.pub_topic.clone(), c.qos, c.retain);
        let keep = Duration::from_secs(c.keepalive.max(5) as u64 * 3 / 4);
        let mut last_tx = Instant::now();
        let mut last_rx = Instant::now();
        loop {
            match s.read(&mut tmp) {
                Ok(0) => return Err(t("服务器关闭了连接").into()),
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    last_rx = Instant::now();
                }
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e.to_string()),
            }
            while let Some((first, body, n)) = parse(&buf) {
                let body = body.to_vec();
                buf.drain(..n);
                match first >> 4 {
                    3 => {
                        // PUBLISH
                        let q = (first >> 1) & 3;
                        if body.len() < 2 {
                            continue;
                        }
                        let tl = u16::from_be_bytes([body[0], body[1]]) as usize;
                        let topic = String::from_utf8_lossy(body.get(2..2 + tl).unwrap_or_default()).to_string();
                        let mut p = 2 + tl;
                        if q > 0 && body.len() >= p + 2 {
                            let pid = [body[p], body[p + 1]];
                            p += 2;
                            let ack = if q == 1 { 0x40 } else { 0x50 };
                            s.write_all(&packet(ack, &pid)).map_err(|e| e.to_string())?;
                        }
                        io.packet(topic, body.get(p..).unwrap_or_default());
                    }
                    5 if body.len() >= 2 => s.write_all(&packet(0x62, &body[..2])).map_err(|e| e.to_string())?, // PUBREC → PUBREL
                    6 if body.len() >= 2 => s.write_all(&packet(0x70, &body[..2])).map_err(|e| e.to_string())?, // PUBREL → PUBCOMP
                    9 if body.len() >= 3 => {
                        // SUBACK
                        let pid = u16::from_be_bytes([body[0], body[1]]);
                        if let Some(i) = pending_subs.iter().position(|(p, _)| *p == pid) {
                            let (_, t) = pending_subs.remove(i);
                            if body[2] == 0x80 {
                                io.sys(tf("✗ 订阅 {} 被服务器拒绝", &[&t]));
                            } else {
                                io.sys(tf("✓ 已订阅 {}（QoS {}）", &[&t, &body[2]]));
                            }
                        }
                    }
                    _ => {}
                }
            }
            loop {
                match io.next() {
                    Next::Cmd(Cmd::Send(d)) => {
                        if pub_topic.trim().is_empty() {
                            io.sys(t("请先填写发布主题"));
                            continue;
                        }
                        let i = id();
                        s.write_all(&publish(i, pub_topic.trim(), &d, qos, retain)).map_err(|e| e.to_string())?;
                        last_tx = Instant::now();
                    }
                    Next::Cmd(Cmd::Subscribe(t)) => {
                        let i = id();
                        s.write_all(&subscribe(i, &t, qos)).map_err(|e| e.to_string())?;
                        pending_subs.push((i, t));
                        last_tx = Instant::now();
                    }
                    Next::Cmd(Cmd::Unsubscribe(t)) => {
                        let i = id();
                        s.write_all(&unsubscribe(i, &t)).map_err(|e| e.to_string())?;
                        io.sys(tf("已取消订阅 {}", &[&t]));
                        last_tx = Instant::now();
                    }
                    Next::Cmd(Cmd::PubOptions(t, q, r)) => {
                        pub_topic = t;
                        qos = q;
                        retain = r;
                    }
                    Next::Cmd(_) => {}
                    Next::Idle => break,
                    Next::Quit => {
                        let _ = s.write_all(&[0xe0, 0x00]);
                        return Ok(());
                    }
                }
            }
            if last_tx.elapsed() >= keep {
                s.write_all(&[0xc0, 0x00]).map_err(|e| e.to_string())?;
                last_tx = Instant::now();
            }
            if last_rx.elapsed() > keep * 2 + Duration::from_secs(10) {
                return Err(t("服务器长时间无响应（心跳超时）").into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mqtt::*;
    use super::*;

    fn wait_evt(rx: &Receiver<Evt>, f: impl Fn(&Evt) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            if let Ok(e) = rx.recv_timeout(Duration::from_millis(50)) {
                if f(&e) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn tcp_client_roundtrip() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let cfg = NetConfig {
            tcp_host: "127.0.0.1".into(),
            tcp_port: port,
            ..Default::default()
        };
        let c = open(1, Transport::TcpClient, cfg, tx, egui::Context::default()).unwrap();
        let (mut peer, _) = l.accept().unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Opened(1, _))));
        peer.write_all(b"hello").unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Data(d) if d == b"hello")));
        c.cmd.send(Cmd::Send(b"ping".to_vec())).unwrap();
        let mut b = [0u8; 4];
        peer.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        drop(peer);
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Closed(1, Some(_)))));
    }

    #[test]
    fn tcp_server_and_udp() {
        let (tx, rx) = mpsc::channel();
        let cfg = NetConfig {
            server_port: 47811,
            udp_local: 47812,
            udp_host: String::new(),
            ..Default::default()
        };
        let srv = open(2, Transport::TcpServer, cfg.clone(), tx.clone(), egui::Context::default()).unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Opened(2, _))));
        let mut cl = TcpStream::connect("127.0.0.1:47811").unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Peers(1))));
        cl.write_all(b"abc").unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Data(d) if d == b"abc")));
        srv.cmd.send(Cmd::Send(b"xyz".to_vec())).unwrap();
        let mut b = [0u8; 3];
        cl.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"xyz");

        let u = open(3, Transport::Udp, cfg, tx, egui::Context::default()).unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Opened(3, _))));
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.send_to(b"dgram", "127.0.0.1:47812").unwrap();
        assert!(wait_evt(&rx, |e| matches!(e, Evt::Packet(_, d) if d == b"dgram")));
        // 目标留空：回复给最后的发送方
        u.cmd.send(Cmd::Send(b"back".to_vec())).unwrap();
        let mut b = [0u8; 16];
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (n, _) = s.recv_from(&mut b).unwrap();
        assert_eq!(&b[..n], b"back");
    }

    #[test]
    fn varint_len() {
        for n in [0usize, 127, 128, 16383, 16384, 2_097_151] {
            let p = packet(0x30, &vec![0u8; n]);
            let (_, body, total) = parse(&p).unwrap();
            assert_eq!(body.len(), n);
            assert_eq!(total, p.len());
        }
        assert!(parse(&[0x30, 0x05, 1, 2]).is_none());
    }

    #[test]
    fn publish_layout() {
        let p = publish(7, "a/b", b"hi", 1, true);
        assert_eq!(p, vec![0x33, 9, 0, 3, b'a', b'/', b'b', 0, 7, b'h', b'i']);
    }
}

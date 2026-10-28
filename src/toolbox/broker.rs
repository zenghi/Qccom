//! 本地 MQTT 服务器（MQTT 3.1.1）：不用另外装 Mosquitto，设备和 QCCOM 自己的 MQTT 标签都能连上来。
//! 支持 QoS 0 / 1（QoS 2 按 1 处理）、通配符订阅、保留消息、遗嘱、心跳超时、可选的用户名密码。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::net::mqtt::{packet, parse, publish};

#[derive(Clone, Default)]
pub struct Config {
    pub port: u16,
    pub user: String,
    pub pass: String,
}

#[derive(Clone)]
pub struct ClientInfo {
    pub id: String,
    pub addr: SocketAddr,
    pub subs: Vec<(String, u8)>,
    pub since: String,
}

#[derive(Clone)]
pub struct MsgLog {
    pub at: String,
    pub from: String,
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    /// 投递给了几个订阅者
    pub delivered: usize,
}

struct Client {
    info: ClientInfo,
    tx: Sender<Vec<u8>>,
    next_id: u16,
}

#[derive(Default)]
pub struct State {
    clients: HashMap<u64, Client>,
    pub retained: BTreeMap<String, (Vec<u8>, u8)>,
    pub log: VecDeque<MsgLog>,
    pub events: VecDeque<String>,
    pub msgs: u64,
}

impl State {
    pub fn clients(&self) -> Vec<ClientInfo> {
        let mut v: Vec<ClientInfo> = self.clients.values().map(|c| c.info.clone()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn event(&mut self, s: String) {
        let at = chrono::Local::now().format("%H:%M:%S");
        self.events.push_back(format!("{at}  {s}"));
        while self.events.len() > 300 {
            self.events.pop_front();
        }
    }

    /// 把一条消息发给所有匹配的订阅者，返回投递数
    fn route(&mut self, from: &str, topic: &str, payload: &[u8], qos: u8, retain: bool) -> usize {
        if retain {
            if payload.is_empty() {
                self.retained.remove(topic);
            } else {
                self.retained.insert(topic.to_string(), (payload.to_vec(), qos));
            }
        }
        let mut n = 0;
        for c in self.clients.values_mut() {
            let Some(q) = c.info.subs.iter().filter(|(f, _)| matches(f, topic)).map(|(_, q)| *q).max() else {
                continue;
            };
            let q = q.min(qos).min(1);
            c.next_id = c.next_id.wrapping_add(1).max(1);
            if c.tx.send(publish(c.next_id, topic, payload, q, false)).is_ok() {
                n += 1;
            }
        }
        self.msgs += 1;
        self.log.push_back(MsgLog {
            at: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
            from: from.to_string(),
            topic: topic.to_string(),
            payload: payload.to_vec(),
            qos,
            retain,
            delivered: n,
        });
        while self.log.len() > 2000 {
            self.log.pop_front();
        }
        n
    }
}

pub struct Broker {
    pub stop: Arc<AtomicBool>,
    pub st: Arc<Mutex<State>>,
    pub port: u16,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Broker {
    /// 从界面发布一条消息
    pub fn publish(&self, topic: &str, payload: &[u8], retain: bool) -> usize {
        self.st.lock().unwrap().route("QCCOM", topic, payload, 1, retain)
    }
}

/// MQTT 主题过滤：+ 匹配一级，# 匹配剩下所有级；$ 开头的系统主题不被通配符第一级匹配
pub fn matches(filter: &str, topic: &str) -> bool {
    if topic.starts_with('$') && (filter.starts_with('+') || filter.starts_with('#')) {
        return false;
    }
    let mut f = filter.split('/');
    let mut t = topic.split('/');
    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

pub fn start(cfg: Config, ctx: egui::Context) -> Result<Broker, String> {
    let l = TcpListener::bind(("0.0.0.0", cfg.port)).map_err(|e| e.to_string())?;
    l.set_nonblocking(true).ok();
    let stop = Arc::new(AtomicBool::new(false));
    let st: Arc<Mutex<State>> = Default::default();
    let (stop2, st2) = (stop.clone(), st.clone());
    let port = cfg.port;
    thread::Builder::new()
        .name("mqtt-broker".into())
        .spawn(move || {
            let mut next = 1u64;
            while !stop2.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((s, addr)) => {
                        let (st, stop, ctx, cfg) = (st2.clone(), stop2.clone(), ctx.clone(), cfg.clone());
                        let key = next;
                        next += 1;
                        thread::spawn(move || {
                            let r = client(key, s, addr, &cfg, &st, &stop, &ctx);
                            let mut g = st.lock().unwrap();
                            if let Some(c) = g.clients.remove(&key) {
                                g.event(format!("- {} ({})  {}", c.info.id, addr, r.err().unwrap_or_default()));
                            }
                            ctx.request_repaint();
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Broker { stop, st, port })
}

fn read_str(b: &[u8], p: &mut usize) -> Option<String> {
    let n = u16::from_be_bytes([*b.get(*p)?, *b.get(*p + 1)?]) as usize;
    let s = b.get(*p + 2..*p + 2 + n)?;
    *p += 2 + n;
    Some(String::from_utf8_lossy(s).into_owned())
}

fn read_bytes(b: &[u8], p: &mut usize) -> Option<Vec<u8>> {
    let n = u16::from_be_bytes([*b.get(*p)?, *b.get(*p + 1)?]) as usize;
    let s = b.get(*p + 2..*p + 2 + n)?;
    *p += 2 + n;
    Some(s.to_vec())
}

struct Will {
    topic: String,
    payload: Vec<u8>,
    qos: u8,
    retain: bool,
}

fn client(key: u64, mut s: TcpStream, addr: SocketAddr, cfg: &Config, st: &Arc<Mutex<State>>, stop: &AtomicBool, ctx: &egui::Context) -> Result<(), String> {
    s.set_nonblocking(false).ok();
    s.set_read_timeout(Some(Duration::from_millis(30))).ok();
    let _ = s.set_nodelay(true);
    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::channel();
    let mut buf: Vec<u8> = vec![];
    let mut tmp = vec![0u8; 16 * 1024];
    let mut id = String::new();
    let mut will: Option<Will> = None;
    let mut keepalive = Duration::from_secs(60);
    let mut last_rx = Instant::now();
    let mut connected = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    let result: Result<(), String> = 'conn: loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match s.read(&mut tmp) {
            Ok(0) => break Err("closed".into()),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                last_rx = Instant::now();
            }
            Err(e) if crate::net::is_timeout(&e) => {}
            Err(e) => break Err(e.to_string()),
        }
        while let Some((first, body, n)) = parse(&buf) {
            let body = body.to_vec();
            buf.drain(..n);
            let kind = first >> 4;
            if !connected && kind != 1 {
                break 'conn Err("no CONNECT".into());
            }
            match kind {
                1 => {
                    // CONNECT
                    let mut p = 0;
                    let proto = read_str(&body, &mut p).unwrap_or_default();
                    let level = body.get(p).copied().unwrap_or(0);
                    let flags = body.get(p + 1).copied().unwrap_or(0);
                    let ka = u16::from_be_bytes([body.get(p + 2).copied().unwrap_or(0), body.get(p + 3).copied().unwrap_or(60)]);
                    p += 4;
                    if !(proto == "MQTT" || proto == "MQIsdp") || !(3..=4).contains(&level) {
                        let _ = s.write_all(&packet(0x20, &[0, 1]));
                        break 'conn Err(format!("protocol {proto} {level}"));
                    }
                    id = read_str(&body, &mut p).unwrap_or_default();
                    if id.is_empty() {
                        id = format!("anon-{key}");
                    }
                    if flags & 0x04 != 0 {
                        let topic = read_str(&body, &mut p).unwrap_or_default();
                        let payload = read_bytes(&body, &mut p).unwrap_or_default();
                        will = Some(Will {
                            topic,
                            payload,
                            qos: (flags >> 3) & 3,
                            retain: flags & 0x20 != 0,
                        });
                    }
                    let user = if flags & 0x80 != 0 { read_str(&body, &mut p).unwrap_or_default() } else { String::new() };
                    let pass = if flags & 0x40 != 0 { read_str(&body, &mut p).unwrap_or_default() } else { String::new() };
                    if !cfg.user.is_empty() && (user != cfg.user || pass != cfg.pass) {
                        let _ = s.write_all(&packet(0x20, &[0, 4]));
                        break 'conn Err("bad user name or password".into());
                    }
                    keepalive = Duration::from_secs(ka.max(1) as u64 * 3 / 2);
                    s.write_all(&packet(0x20, &[0, 0])).map_err(|e| e.to_string())?;
                    connected = true;
                    let mut g = st.lock().unwrap();
                    // 同一个客户端 ID 再连上来：踢掉旧的
                    let dup: Vec<u64> = g.clients.iter().filter(|(_, c)| c.info.id == id).map(|(k, _)| *k).collect();
                    for k in dup {
                        g.clients.remove(&k);
                    }
                    g.clients.insert(
                        key,
                        Client {
                            info: ClientInfo {
                                id: id.clone(),
                                addr,
                                subs: vec![],
                                since: chrono::Local::now().format("%H:%M:%S").to_string(),
                            },
                            tx: tx.clone(),
                            next_id: 0,
                        },
                    );
                    g.event(format!("+ {id} ({addr})"));
                    ctx.request_repaint();
                }
                3 => {
                    // PUBLISH
                    let qos = (first >> 1) & 3;
                    let retain = first & 1 != 0;
                    let mut p = 0;
                    let topic = read_str(&body, &mut p).unwrap_or_default();
                    if qos > 0 {
                        let pid = [body.get(p).copied().unwrap_or(0), body.get(p + 1).copied().unwrap_or(0)];
                        p += 2;
                        let ack = if qos == 1 { 0x40 } else { 0x50 };
                        s.write_all(&packet(ack, &pid)).map_err(|e| e.to_string())?;
                    }
                    let payload = body.get(p..).unwrap_or_default().to_vec();
                    st.lock().unwrap().route(&id, &topic, &payload, qos, retain);
                    ctx.request_repaint();
                }
                6 => s.write_all(&packet(0x70, body.get(..2).unwrap_or(&[0, 0]))).map_err(|e| e.to_string())?, // PUBREL → PUBCOMP
                4 | 5 | 7 => {}                                                                                // PUBACK / PUBREC / PUBCOMP
                8 => {
                    // SUBSCRIBE
                    let pid = body.get(..2).unwrap_or(&[0, 0]).to_vec();
                    let mut p = 2;
                    let mut granted = vec![];
                    let mut new = vec![];
                    while p < body.len() {
                        let Some(f) = read_str(&body, &mut p) else { break };
                        let q = body.get(p).copied().unwrap_or(0).min(1);
                        p += 1;
                        granted.push(q);
                        new.push((f, q));
                    }
                    let mut ack = pid;
                    ack.extend_from_slice(&granted);
                    s.write_all(&packet(0x90, &ack)).map_err(|e| e.to_string())?;
                    let mut g = st.lock().unwrap();
                    let retained: Vec<(String, Vec<u8>, u8)> = g
                        .retained
                        .iter()
                        .filter(|(t, _)| new.iter().any(|(f, _)| matches(f, t)))
                        .map(|(t, (d, q))| (t.clone(), d.clone(), *q))
                        .collect();
                    let names: Vec<String> = new.iter().map(|(f, _)| f.clone()).collect();
                    if let Some(c) = g.clients.get_mut(&key) {
                        for (f, q) in new {
                            c.info.subs.retain(|(x, _)| *x != f);
                            c.info.subs.push((f, q));
                        }
                    }
                    g.event(format!("  {id} SUB {}", names.join(", ")));
                    drop(g);
                    for (t, d, q) in retained {
                        s.write_all(&publish(1, &t, &d, q.min(1), true)).map_err(|e| e.to_string())?;
                    }
                    ctx.request_repaint();
                }
                10 => {
                    // UNSUBSCRIBE
                    let pid = body.get(..2).unwrap_or(&[0, 0]).to_vec();
                    let mut p = 2;
                    let mut g = st.lock().unwrap();
                    while p < body.len() {
                        let Some(f) = read_str(&body, &mut p) else { break };
                        if let Some(c) = g.clients.get_mut(&key) {
                            c.info.subs.retain(|(x, _)| *x != f);
                        }
                    }
                    drop(g);
                    s.write_all(&packet(0xb0, &pid)).map_err(|e| e.to_string())?;
                }
                12 => s.write_all(&packet(0xd0, &[])).map_err(|e| e.to_string())?, // PINGREQ
                14 => {
                    will = None; // 正常断开不发遗嘱
                    break 'conn Ok(());
                }
                _ => {}
            }
        }
        // 发给这个客户端的消息
        while let Ok(p) = rx.try_recv() {
            if let Err(e) = s.write_all(&p) {
                break 'conn Err(e.to_string());
            }
        }
        if !connected && Instant::now() > deadline {
            break Err("no CONNECT".into());
        }
        if connected && last_rx.elapsed() > keepalive {
            break Err("keepalive timeout".into());
        }
        // 被同 ID 的新连接踢掉了
        if connected && !st.lock().unwrap().clients.contains_key(&key) {
            break Ok(());
        }
    };
    if let Some(w) = will {
        st.lock().unwrap().route(&id, &w.topic, &w.payload, w.qos, w.retain);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_filters() {
        assert!(matches("a/+/c", "a/b/c"));
        assert!(!matches("a/+/c", "a/b/d"));
        assert!(matches("a/#", "a/b/c"));
        assert!(matches("a/#", "a"));
        assert!(matches("#", "x/y"));
        assert!(!matches("#", "$SYS/x"));
        assert!(matches("$SYS/#", "$SYS/x"));
        assert!(!matches("a/b", "a/b/c"));
    }

    #[test]
    fn pub_sub_roundtrip() {
        let ctx = egui::Context::default();
        let b = start(Config { port: 47833, ..Default::default() }, ctx).unwrap();
        thread::sleep(Duration::from_millis(100));
        let mut c = TcpStream::connect("127.0.0.1:47833").unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let cfg = crate::net::MqttConfig::default();
        c.write_all(&crate::net::mqtt::connect(&cfg, "t1")).unwrap();
        let mut r = [0u8; 4];
        c.read_exact(&mut r).unwrap();
        assert_eq!(r, [0x20, 2, 0, 0]);
        c.write_all(&crate::net::mqtt::subscribe(1, "dev/+/temp", 0)).unwrap();
        let mut r = [0u8; 5];
        c.read_exact(&mut r).unwrap();
        assert_eq!(r[0], 0x90);
        assert_eq!(b.publish("dev/7/temp", b"21.5", false), 1);
        let mut r = vec![0u8; 2 + 2 + 10 + 4];
        c.read_exact(&mut r).unwrap();
        assert_eq!(&r[r.len() - 4..], b"21.5");
    }
}

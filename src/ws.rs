//! WebSocket 客户端（RFC 6455）：ws:// 和 wss://，文本 / 二进制消息，自动回 Pong、定时 Ping。
//! 每条消息在日志里单独一行。

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::i18n::{t, tf};
use crate::net::{connect_tcp, is_timeout, tls, Io, Next, Stream};
use crate::serial::Cmd;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WsConfig {
    pub url: String,
    /// 额外的请求头，每行一个「名字: 值」
    pub headers: String,
    /// 发出去的是文本帧（否则二进制帧）
    pub text: bool,
    /// 每隔多少秒发一个 Ping（0 = 不发）
    pub ping_s: u16,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            url: "ws://192.168.1.100:81/".into(),
            headers: String::new(),
            text: true,
            ping_s: 30,
        }
    }
}

pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(s: &str) -> Result<Url, String> {
    let s = s.trim();
    let (tls, rest) = if let Some(r) = s.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = s.strip_prefix("ws://") {
        (false, r)
    } else if s.contains("://") {
        return Err(t("地址要以 ws:// 或 wss:// 开头").into());
    } else {
        (false, s)
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    if hostport.is_empty() {
        return Err(t("请填写地址").into());
    }
    let def = if tls { 443 } else { 80 };
    // [::1]:8080 这种 IPv6 写法
    let (host, port) = if let Some(h) = hostport.strip_prefix('[') {
        let (h, after) = h.split_once(']').ok_or_else(|| t("地址格式不对").to_string())?;
        (h.to_string(), after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(def))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().map_err(|_| tf("端口不对：{}", &[&p]))?),
            None => (hostport.to_string(), def),
        }
    };
    Ok(Url { tls, host, port, path })
}

/// 客户端发出的帧必须加掩码
pub fn frame(op: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0x80 | op];
    let n = payload.len();
    if n < 126 {
        f.push(0x80 | n as u8);
    } else if n < 65536 {
        f.push(0x80 | 126);
        f.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        f.push(0x80 | 127);
        f.extend_from_slice(&(n as u64).to_be_bytes());
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// 解析一帧：(FIN, 操作码, 内容, 占用字节数)。数据不够返回 None
pub fn parse_frame(b: &[u8]) -> Option<(bool, u8, Vec<u8>, usize)> {
    if b.len() < 2 {
        return None;
    }
    let fin = b[0] & 0x80 != 0;
    let op = b[0] & 0x0f;
    let masked = b[1] & 0x80 != 0;
    let mut n = (b[1] & 0x7f) as usize;
    let mut p = 2;
    if n == 126 {
        n = u16::from_be_bytes([*b.get(2)?, *b.get(3)?]) as usize;
        p = 4;
    } else if n == 127 {
        let mut a = [0u8; 8];
        a.copy_from_slice(b.get(2..10)?);
        n = u64::from_be_bytes(a) as usize;
        p = 10;
    }
    let mask = if masked {
        let m = [*b.get(p)?, *b.get(p + 1)?, *b.get(p + 2)?, *b.get(p + 3)?];
        p += 4;
        Some(m)
    } else {
        None
    };
    let body = b.get(p..p + n)?;
    let data = match mask {
        Some(m) => body.iter().enumerate().map(|(i, x)| x ^ m[i % 4]).collect(),
        None => body.to_vec(),
    };
    Some((fin, op, data, p + n))
}

pub fn accept_key(key: &str) -> String {
    crate::toolbox::conv::base64(&sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()))
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_be_bytes());
    for c in m.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c2, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c2) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c2 ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c2) | (b & d) | (c2 & d), 0x8F1BBCDC),
                _ => (b ^ c2 ^ d, 0xCA62C1D6),
            };
            let tmp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c2;
            c2 = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        for (x, y) in h.iter_mut().zip([a, b, c2, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// 不需要密码学强度：握手的 key 和帧掩码只要每次不一样
struct Rng(u64);
impl Rng {
    fn new() -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Rng(n ^ (std::process::id() as u64) << 32 | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn mask(&mut self) -> [u8; 4] {
        (self.next() as u32).to_le_bytes()
    }
}

fn close_reason(d: &[u8]) -> String {
    if d.len() >= 2 {
        let code = u16::from_be_bytes([d[0], d[1]]);
        let why = String::from_utf8_lossy(&d[2..]).to_string();
        if why.is_empty() {
            format!("{code}")
        } else {
            format!("{code} {why}")
        }
    } else {
        String::new()
    }
}

pub fn run(io: &Io, c: &WsConfig, label: &str) -> Result<(), String> {
    let u = parse_url(&c.url)?;
    let tcp = connect_tcp(&u.host, u.port)?;
    tcp.set_read_timeout(Some(Duration::from_secs(8))).ok();
    let raw = tcp.try_clone().map_err(|e| e.to_string())?;
    let mut s: Box<dyn Stream> = if u.tls { tls(&u.host, tcp)? } else { Box::new(tcp) };
    let mut rng = Rng::new();
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&rng.next().to_le_bytes());
    key[8..].copy_from_slice(&rng.next().to_le_bytes());
    let key = crate::toolbox::conv::base64(&key);
    let host_hdr = if (u.tls && u.port == 443) || (!u.tls && u.port == 80) {
        u.host.clone()
    } else {
        format!("{}:{}", u.host, u.port)
    };
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {host_hdr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: QCCOM/{}\r\n",
        u.path,
        env!("CARGO_PKG_VERSION")
    );
    for l in c.headers.lines().map(str::trim).filter(|l| l.contains(':')) {
        req.push_str(l);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    // 读握手响应
    let mut buf: Vec<u8> = vec![];
    let mut tmp = vec![0u8; 16 * 1024];
    let deadline = Instant::now() + Duration::from_secs(8);
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if Instant::now() > deadline || buf.len() > 64 * 1024 {
            return Err(t("等待服务器响应超时").into());
        }
        match s.read(&mut tmp) {
            Ok(0) => return Err(t("服务器关闭了连接").into()),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    buf.drain(..head_end);
    let status = head.lines().next().unwrap_or_default().to_string();
    if !status.split_whitespace().nth(1).is_some_and(|c| c == "101") {
        return Err(tf("服务器拒绝升级为 WebSocket：{}", &[&status.trim()]));
    }
    let accept = head
        .lines()
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("sec-websocket-accept"))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_default();
    if accept != accept_key(&key) {
        return Err(t("服务器的握手校验值不对").into());
    }
    raw.set_read_timeout(Some(Duration::from_millis(20))).ok();
    io.opened(label);

    let ping_every = (c.ping_s > 0).then(|| Duration::from_secs(c.ping_s as u64));
    let mut last_ping = Instant::now();
    // 分片消息：(操作码, 已收到的内容)
    let mut frag: Option<(u8, Vec<u8>)> = None;
    loop {
        match s.read(&mut tmp) {
            Ok(0) => return Err(t("服务器关闭了连接").into()),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
        while let Some((fin, op, data, n)) = parse_frame(&buf) {
            buf.drain(..n);
            match op {
                0x0 => {
                    if let Some((_, acc)) = frag.as_mut() {
                        acc.extend_from_slice(&data);
                    }
                    if fin {
                        if let Some((o, acc)) = frag.take() {
                            io.packet(if o == 1 { "txt".into() } else { "bin".into() }, &acc);
                        }
                    }
                }
                0x1 | 0x2 if !fin => frag = Some((op, data)),
                0x1 => io.packet("txt".into(), &data),
                0x2 => io.packet("bin".into(), &data),
                0x8 => {
                    let _ = s.write_all(&frame(0x8, &data[..data.len().min(2)], rng.mask()));
                    let r = close_reason(&data);
                    return Err(if r.is_empty() {
                        t("服务器关闭了连接").into()
                    } else {
                        tf("服务器关闭了连接（{}）", &[&r])
                    });
                }
                0x9 => s.write_all(&frame(0xA, &data, rng.mask())).map_err(|e| e.to_string())?,
                _ => {}
            }
        }
        if let Some(iv) = ping_every {
            if last_ping.elapsed() >= iv {
                last_ping = Instant::now();
                s.write_all(&frame(0x9, b"qccom", rng.mask())).map_err(|e| e.to_string())?;
            }
        }
        loop {
            match io.next() {
                Next::Cmd(Cmd::Send(d)) => {
                    let op = if c.text && std::str::from_utf8(&d).is_ok() { 0x1 } else { 0x2 };
                    s.write_all(&frame(op, &d, rng.mask())).map_err(|e| e.to_string())?;
                }
                Next::Cmd(_) => {}
                Next::Idle => break,
                Next::Quit => {
                    let _ = s.write_all(&frame(0x8, &1000u16.to_be_bytes(), rng.mask()));
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_key() {
        // RFC 6455 第 1.3 节的例子
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_roundtrip() {
        let f = frame(1, b"hello", [1, 2, 3, 4]);
        let (fin, op, d, n) = parse_frame(&f).unwrap();
        assert!(fin);
        assert_eq!((op, d.as_slice(), n), (1, &b"hello"[..], f.len()));
        let big = vec![7u8; 70000];
        let f = frame(2, &big, [9, 9, 9, 9]);
        assert_eq!(parse_frame(&f).unwrap().2, big);
        assert!(parse_frame(&f[..f.len() - 1]).is_none());
    }

    #[test]
    fn urls() {
        let u = parse_url("wss://echo.example.com/chat?x=1").unwrap();
        assert!(u.tls);
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("echo.example.com", 443, "/chat?x=1"));
        let u = parse_url("192.168.4.1:81").unwrap();
        assert_eq!((u.port, u.path.as_str()), (81, "/"));
        assert!(parse_url("http://x").is_err());
    }
}

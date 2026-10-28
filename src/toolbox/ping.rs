//! Ping 和端口检测。
//! Ping 调系统自带的 ping 命令（ICMP 需要管理员权限，系统命令不用），从输出里取延迟；
//! 端口检测直接 TCP 连接，同时测连接耗时。

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

#[derive(Clone)]
pub struct Reply {
    pub seq: u32,
    pub at: String,
    /// 毫秒；None = 超时 / 不通
    pub ms: Option<f64>,
}

#[derive(Default)]
pub struct PingState {
    pub running: bool,
    pub replies: Vec<Reply>,
    pub err: Option<String>,
}

impl PingState {
    pub fn summary(&self) -> (usize, usize, Option<(f64, f64, f64)>) {
        let sent = self.replies.len();
        let ok: Vec<f64> = self.replies.iter().filter_map(|r| r.ms).collect();
        let stats = (!ok.is_empty()).then(|| {
            let min = ok.iter().cloned().fold(f64::MAX, f64::min);
            let max = ok.iter().cloned().fold(0.0, f64::max);
            (min, ok.iter().sum::<f64>() / ok.len() as f64, max)
        });
        (sent, ok.len(), stats)
    }
}

pub type Shared = Arc<Mutex<PingState>>;

/// 从 ping 输出里取延迟：time=12.3 ms、时间=12ms、Zeit<1ms 都认
pub fn parse_ms(out: &str) -> Option<f64> {
    for line in out.lines() {
        let l = line.to_lowercase();
        if !(l.contains("ttl") || l.contains("icmp_seq")) {
            continue;
        }
        let b = l.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c != b'=' && c != b'<' {
                continue;
            }
            let rest = &l[i + 1..];
            let num: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if num.is_empty() {
                continue;
            }
            let after = rest.trim_start()[num.len()..].trim_start();
            if after.starts_with("ms") {
                let v: f64 = num.parse().ok()?;
                return Some(if c == b'<' { v.min(0.5) } else { v });
            }
        }
    }
    None
}

fn ping_once(host: &str, timeout_ms: u32) -> Result<Option<f64>, String> {
    let mut cmd = Command::new("ping");
    if cfg!(windows) {
        cmd.args(["-n", "1", "-w", &timeout_ms.to_string(), host]);
    } else {
        cmd.args(["-c", "1", "-W", &timeout_ms.div_ceil(1000).max(1).to_string(), host]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| e.to_string())?;
    // Windows 的 ping 输出是系统代码页，按 GBK 解一下不影响数字
    let text = if cfg!(windows) {
        encoding_rs::GBK.decode(&out.stdout).0.into_owned()
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    Ok(if out.status.success() { parse_ms(&text) } else { None })
}

/// count = 0 一直 ping，直到 stop
pub fn start(st: &Shared, stop: Arc<AtomicBool>, host: String, count: u32, interval_ms: u32, ctx: egui::Context) {
    {
        let mut s = st.lock().unwrap();
        s.running = true;
        s.replies.clear();
        s.err = None;
    }
    let st = st.clone();
    thread::spawn(move || {
        let mut seq = 0;
        while !stop.load(Ordering::Relaxed) && (count == 0 || seq < count) {
            seq += 1;
            let t0 = Instant::now();
            match ping_once(&host, 1000) {
                Ok(ms) => {
                    let at = chrono::Local::now().format("%H:%M:%S").to_string();
                    let mut s = st.lock().unwrap();
                    s.replies.push(Reply { seq, at, ms });
                    if s.replies.len() > 5000 {
                        s.replies.remove(0);
                    }
                }
                Err(e) => {
                    st.lock().unwrap().err = Some(e);
                    break;
                }
            }
            ctx.request_repaint();
            let wait = Duration::from_millis(interval_ms as u64).saturating_sub(t0.elapsed());
            let until = Instant::now() + wait;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(50));
            }
        }
        st.lock().unwrap().running = false;
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------- 端口检测

#[derive(Clone)]
pub struct PortResult {
    pub port: u16,
    /// Ok(毫秒) 开放；Err(原因) 关闭 / 超时
    pub r: Result<f64, String>,
}

#[derive(Default)]
pub struct ScanState {
    pub running: bool,
    pub total: usize,
    pub results: Vec<PortResult>,
    pub err: Option<String>,
}

pub type ScanShared = Arc<Mutex<ScanState>>;

/// "22, 80, 8000-8010" → 端口列表（最多 2000 个）
pub fn parse_ports(s: &str) -> Result<Vec<u16>, String> {
    let mut v = vec![];
    for part in s.split([',', ' ', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let a: u16 = a.trim().parse().map_err(|_| part.to_string())?;
            let b: u16 = b.trim().parse().map_err(|_| part.to_string())?;
            if a > b {
                return Err(part.to_string());
            }
            v.extend(a..=b);
        } else {
            v.push(part.parse().map_err(|_| part.to_string())?);
        }
        if v.len() > 2000 {
            return Err("> 2000".into());
        }
    }
    v.dedup();
    Ok(v)
}

pub fn scan(st: &ScanShared, host: String, ports: Vec<u16>, ctx: egui::Context) {
    {
        let mut s = st.lock().unwrap();
        s.running = true;
        s.results.clear();
        s.err = None;
        s.total = ports.len();
    }
    let st = st.clone();
    thread::spawn(move || {
        let ip = match (host.trim(), 0).to_socket_addrs().map(|mut a| a.next()) {
            Ok(Some(a)) => a.ip(),
            Ok(None) | Err(_) => {
                let mut s = st.lock().unwrap();
                s.err = Some(host.clone());
                s.running = false;
                ctx.request_repaint();
                return;
            }
        };
        // 64 个一批并发
        for chunk in ports.chunks(64) {
            let hs: Vec<_> = chunk
                .iter()
                .map(|&port| {
                    let st = st.clone();
                    thread::spawn(move || {
                        let t0 = Instant::now();
                        let r = TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(1200))
                            .map(|_| t0.elapsed().as_secs_f64() * 1000.0)
                            .map_err(|e| match e.kind() {
                                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timeout".to_string(),
                                std::io::ErrorKind::ConnectionRefused => "refused".to_string(),
                                _ => e.to_string(),
                            });
                        st.lock().unwrap().results.push(PortResult { port, r });
                    })
                })
                .collect();
            for h in hs {
                let _ = h.join();
            }
            ctx.request_repaint();
        }
        let mut s = st.lock().unwrap();
        s.results.sort_by_key(|r| r.port);
        s.running = false;
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_output() {
        assert_eq!(parse_ms("64 bytes from 1.1.1.1: icmp_seq=1 ttl=57 time=12.3 ms"), Some(12.3));
        assert_eq!(parse_ms("来自 192.168.1.1 的回复: 字节=32 时间=3ms TTL=64"), Some(3.0));
        assert_eq!(parse_ms("Reply from 10.0.0.1: bytes=32 time<1ms TTL=128"), Some(0.5));
        assert_eq!(parse_ms("Request timed out."), None);
    }

    #[test]
    fn ports() {
        assert_eq!(parse_ports("22, 80,8000-8002").unwrap(), vec![22, 80, 8000, 8001, 8002]);
        assert!(parse_ports("9-1").is_err());
        assert!(parse_ports("abc").is_err());
    }
}

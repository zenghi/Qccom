//! Syslog 接收器：UDP 监听，解析 RFC 3164 / RFC 5424 的级别、主机和程序名。
//! 路由器、交换机、Linux 开发板（busybox syslogd -R）都能把日志发过来。

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;

#[derive(Clone)]
pub struct Msg {
    pub at: String,
    pub from: String,
    /// 0 emerg … 7 debug；没有 PRI 时为 None
    pub severity: Option<u8>,
    pub facility: Option<u8>,
    pub host: String,
    pub app: String,
    pub text: String,
}

#[derive(Default)]
pub struct State {
    pub msgs: VecDeque<Msg>,
    pub total: u64,
}

pub struct Server {
    pub stop: Arc<AtomicBool>,
    pub st: Arc<Mutex<State>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub const SEVERITY: [&str; 8] = ["EMERG", "ALERT", "CRIT", "ERR", "WARN", "NOTICE", "INFO", "DEBUG"];

pub fn start(port: u16, ctx: egui::Context) -> Result<Server, String> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let stop = Arc::new(AtomicBool::new(false));
    let st: Arc<Mutex<State>> = Default::default();
    let (stop2, st2) = (stop.clone(), st.clone());
    thread::Builder::new()
        .name("syslog".into())
        .spawn(move || {
            let mut buf = vec![0u8; 8192];
            while !stop2.load(Ordering::Relaxed) {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    let m = parse(&String::from_utf8_lossy(&buf[..n]), from.ip().to_string());
                    let mut s = st2.lock().unwrap();
                    s.total += 1;
                    s.msgs.push_back(m);
                    while s.msgs.len() > 20_000 {
                        s.msgs.pop_front();
                    }
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Server { stop, st })
}

pub fn parse(raw: &str, from: String) -> Msg {
    let at = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
    let s = raw.trim_end_matches(['\r', '\n', '\0']);
    let mut m = Msg {
        at,
        from: from.clone(),
        severity: None,
        facility: None,
        host: from,
        app: String::new(),
        text: s.to_string(),
    };
    let Some(rest) = s.strip_prefix('<') else { return m };
    let Some(end) = rest.find('>') else { return m };
    let Ok(pri) = rest[..end].parse::<u16>() else { return m };
    m.severity = Some((pri % 8) as u8);
    m.facility = Some((pri / 8) as u8);
    let body = &rest[end + 1..];
    // RFC 5424：<PRI>1 时间 主机 程序 进程号 消息ID 结构化数据 消息
    if let Some(b) = body.strip_prefix("1 ") {
        let mut it = b.splitn(6, ' ');
        let _ts = it.next();
        let host = it.next().unwrap_or("-");
        let app = it.next().unwrap_or("-");
        let _pid = it.next();
        let _msgid = it.next();
        let mut msg = it.next().unwrap_or("");
        // 结构化数据：- 或 [ ... ]
        if let Some(r) = msg.strip_prefix("- ") {
            msg = r;
        } else if msg == "-" {
            msg = "";
        } else if msg.starts_with('[') {
            if let Some(i) = msg.find("] ") {
                msg = &msg[i + 2..];
            }
        }
        if host != "-" {
            m.host = host.to_string();
        }
        if app != "-" {
            m.app = app.to_string();
        }
        m.text = msg.trim_start_matches('\u{feff}').to_string();
        return m;
    }
    // RFC 3164：<PRI>Mmm dd hh:mm:ss 主机 程序[pid]: 消息（时间和主机常常省略）
    let mut b = body;
    if b.len() >= 16 && b.as_bytes()[3] == b' ' && b.as_bytes()[6] == b' ' && b.as_bytes()[9] == b':' {
        b = &b[16..];
        if let Some((h, r)) = b.split_once(' ') {
            if !h.ends_with(':') && !h.contains('[') {
                m.host = h.to_string();
                b = r;
            }
        }
    }
    if let Some(i) = b.find(": ") {
        let tag = &b[..i];
        if !tag.is_empty() && !tag.contains(' ') {
            m.app = tag.split('[').next().unwrap_or(tag).to_string();
            b = &b[i + 2..];
        }
    }
    m.text = b.to_string();
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let m = parse("<34>Oct 11 22:14:15 mymachine su[230]: 'su root' failed", "1.2.3.4".into());
        assert_eq!((m.severity, m.facility), (Some(2), Some(4)));
        assert_eq!((m.host.as_str(), m.app.as_str(), m.text.as_str()), ("mymachine", "su", "'su root' failed"));
        let m = parse("<165>1 2003-10-11T22:14:15.003Z host.example evntslog - ID47 [x@1 a=\"b\"] An application event", "x".into());
        assert_eq!(
            (m.severity, m.host.as_str(), m.app.as_str(), m.text.as_str()),
            (Some(5), "host.example", "evntslog", "An application event")
        );
        let m = parse("<13>kernel: usb 1-1: new device", "9.9.9.9".into());
        assert_eq!((m.app.as_str(), m.text.as_str(), m.host.as_str()), ("kernel", "usb 1-1: new device", "9.9.9.9"));
        assert_eq!(parse("plain", "h".into()).severity, None);
    }
}

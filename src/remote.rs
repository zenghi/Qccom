//! SSH / Telnet 远程终端（工具箱「通信调试」里的 SSH / Telnet 工具）。
//! SSH 直接调用系统的 ssh 命令（Windows 10 起自带 OpenSSH），跑在伪终端里：密钥、ssh-agent、
//! ~/.ssh/config、跳板机都和命令行里一样；上传下载走 scp。Telnet 自己实现（协商回显、窗口大小）。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::i18n::{t, tf};
use crate::shell::{Shell, ShellKind};
use crate::termview::TermView;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Proto {
    #[default]
    Ssh,
    Telnet,
}

impl Proto {
    pub fn name(self) -> &'static str {
        match self {
            Proto::Ssh => "SSH",
            Proto::Telnet => "Telnet",
        }
    }
    pub fn default_port(self) -> u16 {
        match self {
            Proto::Ssh => 22,
            Proto::Telnet => 23,
        }
    }
}

/// 保存的主机
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Host {
    pub name: String,
    pub proto: Proto,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 私钥文件（空 = 用 ssh 默认的 ~/.ssh/id_*、ssh-agent）
    pub key: String,
    /// 拖文件上传到哪个远程目录
    pub dir: String,
    /// 额外的 ssh 参数，如 -J 跳板机
    pub args: String,
    /// 连上后自动发送的命令（每行一条）
    pub init: String,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            name: String::new(),
            proto: Proto::Ssh,
            host: String::new(),
            port: 22,
            user: "root".into(),
            key: String::new(),
            dir: "~".into(),
            args: String::new(),
            init: String::new(),
        }
    }
}

impl Host {
    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        self.addr()
    }

    pub fn addr(&self) -> String {
        let port = if self.port == self.proto.default_port() { String::new() } else { format!(":{}", self.port) };
        if self.proto == Proto::Ssh && !self.user.is_empty() {
            format!("{}@{}{port}", self.user, self.host)
        } else {
            format!("{}{port}", self.host)
        }
    }

    /// 快速连接：`user@host:port`、`ssh://…`、`telnet://host:23`
    pub fn parse(s: &str) -> Option<Host> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (proto, rest) = if let Some(r) = s.strip_prefix("telnet://").or_else(|| s.strip_prefix("telnet ")) {
            (Proto::Telnet, r)
        } else if let Some(r) = s.strip_prefix("ssh://").or_else(|| s.strip_prefix("ssh ")) {
            (Proto::Ssh, r)
        } else {
            (Proto::Ssh, s)
        };
        let rest = rest.trim().trim_end_matches('/');
        let (user, hp) = match rest.rsplit_once('@') {
            Some((u, h)) => (u.to_string(), h),
            None => (if proto == Proto::Ssh { String::new() } else { String::new() }, rest),
        };
        // [v6]:port、host:port、host port
        let (host, port) = if let Some(h) = hp.strip_prefix('[') {
            let (h, p) = h.split_once(']')?;
            (h.to_string(), p.trim_start_matches(':').parse().ok())
        } else if let Some((h, p)) = hp.rsplit_once(':').filter(|(h, _)| !h.contains(':')) {
            (h.to_string(), p.parse().ok())
        } else if let Some((h, p)) = hp.split_once(' ') {
            (h.to_string(), p.trim().parse().ok())
        } else {
            (hp.to_string(), None)
        };
        if host.is_empty() || host.contains(char::is_whitespace) {
            return None;
        }
        Some(Host {
            proto,
            host,
            port: port.unwrap_or(proto.default_port()),
            user,
            ..Default::default()
        })
    }

    fn ssh_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-o".into(), "ServerAliveInterval=30".into()];
        if self.port != 22 {
            a.extend(["-p".into(), self.port.to_string()]);
        }
        if !self.key.trim().is_empty() {
            a.extend(["-i".into(), self.key.trim().into()]);
        }
        a.extend(split_args(&self.args));
        a.push(self.target());
        a
    }

    fn target(&self) -> String {
        let h = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.user.is_empty() {
            h
        } else {
            format!("{}@{h}", self.user)
        }
    }

    /// scp 的公共参数（BatchMode：要密码时直接失败，不会卡住）
    fn scp_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-o".into(), "BatchMode=yes".into(), "-r".into()];
        if self.port != 22 {
            a.extend(["-P".into(), self.port.to_string()]);
        }
        if !self.key.trim().is_empty() {
            a.extend(["-i".into(), self.key.trim().into()]);
        }
        a.extend(split_args(&self.args));
        a
    }
}

/// 按空格切参数，支持双引号
fn split_args(s: &str) -> Vec<String> {
    let mut v = vec![];
    let mut cur = String::new();
    let mut q = false;
    for c in s.chars() {
        match c {
            '"' => q = !q,
            c if c.is_whitespace() && !q => {
                if !cur.is_empty() {
                    v.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

pub fn ssh_program() -> Option<String> {
    let exe = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    if let Some(p) = crate::shell::which(exe) {
        return Some(p.to_string_lossy().into_owned());
    }
    if cfg!(windows) {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let p = PathBuf::from(sys).join("System32").join("OpenSSH").join("ssh.exe");
        if p.is_file() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

fn scp_program() -> String {
    ssh_program()
        .map(|s| {
            let p = Path::new(&s);
            p.with_file_name(if cfg!(windows) { "scp.exe" } else { "scp" }).to_string_lossy().into_owned()
        })
        .unwrap_or_else(|| "scp".into())
}

// ---------------------------------------------------------------- Telnet

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

enum TEvt {
    Data(Vec<u8>),
    Up(TcpStream),
    Closed(String),
}

pub struct Telnet {
    rx: Receiver<TEvt>,
    w: Option<TcpStream>,
    naws: Arc<AtomicBool>,
    size: (usize, usize),
}

/// 剥掉 IAC 协商，返回要显示的数据；需要回复的写进 reply
#[derive(Default)]
struct Iac {
    st: u8,
    verb: u8,
    sb: Vec<u8>,
}

impl Iac {
    fn feed(&mut self, d: &[u8], reply: &mut Vec<u8>, naws: &AtomicBool) -> Vec<u8> {
        let mut out = Vec::with_capacity(d.len());
        for &b in d {
            match self.st {
                0 => {
                    if b == IAC {
                        self.st = 1;
                    } else {
                        out.push(b);
                    }
                }
                1 => match b {
                    IAC => {
                        out.push(IAC);
                        self.st = 0;
                    }
                    DO | DONT | WILL | WONT => {
                        self.verb = b;
                        self.st = 2;
                    }
                    SB => {
                        self.sb.clear();
                        self.st = 3;
                    }
                    _ => self.st = 0,
                },
                2 => {
                    let opt = b;
                    match self.verb {
                        DO => {
                            if opt == NAWS || opt == TTYPE {
                                reply.extend([IAC, WILL, opt]);
                                if opt == NAWS {
                                    naws.store(true, Ordering::Relaxed);
                                }
                            } else {
                                reply.extend([IAC, WONT, opt]);
                            }
                        }
                        WILL => {
                            let ok = opt == ECHO || opt == SGA;
                            reply.extend([IAC, if ok { DO } else { DONT }, opt]);
                        }
                        _ => {}
                    }
                    self.st = 0;
                }
                3 => {
                    if b == IAC {
                        self.st = 4;
                    } else {
                        self.sb.push(b);
                    }
                }
                _ => {
                    if b == SE {
                        // 终端类型查询：SB TTYPE SEND
                        if self.sb.first() == Some(&TTYPE) && self.sb.get(1) == Some(&1) {
                            reply.extend([IAC, SB, TTYPE, 0]);
                            reply.extend(b"XTERM-256COLOR");
                            reply.extend([IAC, SE]);
                        }
                        self.st = 0;
                    } else {
                        self.sb.push(b);
                        self.st = 3;
                    }
                }
            }
        }
        out
    }
}

impl Telnet {
    fn connect(ctx: &egui::Context, host: &str, port: u16) -> Telnet {
        let (tx, rx) = mpsc::channel();
        let naws = Arc::new(AtomicBool::new(false));
        let (ctx, host, n2) = (ctx.clone(), host.to_string(), naws.clone());
        std::thread::spawn(move || telnet_thread(ctx, host, port, tx, n2));
        Telnet { rx, w: None, naws, size: (0, 0) }
    }

    fn write(&mut self, d: &[u8]) {
        let Some(w) = &mut self.w else { return };
        // 数据里的 0xFF 要转义；回车按协议发 CR NUL
        let mut out = Vec::with_capacity(d.len() + 4);
        for &b in d {
            match b {
                IAC => out.extend([IAC, IAC]),
                b'\r' => out.extend([b'\r', 0]),
                b => out.push(b),
            }
        }
        let _ = w.write_all(&out);
    }

    fn sync_size(&mut self, cols: usize, rows: usize) {
        if !self.naws.load(Ordering::Relaxed) || self.size == (cols, rows) {
            return;
        }
        if let Some(w) = &mut self.w {
            self.size = (cols, rows);
            let (c, r) = (cols as u16, rows as u16);
            let mut m = vec![IAC, SB, NAWS];
            for b in [(c >> 8) as u8, c as u8, (r >> 8) as u8, r as u8] {
                m.push(b);
                if b == IAC {
                    m.push(IAC);
                }
            }
            m.extend([IAC, SE]);
            let _ = w.write_all(&m);
        }
    }
}

fn telnet_thread(ctx: egui::Context, host: String, port: u16, tx: Sender<TEvt>, naws: Arc<AtomicBool>) {
    let fail = |e: String| {
        let _ = tx.send(TEvt::Closed(e));
        ctx.request_repaint();
    };
    let addrs: Vec<_> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => return fail(format!("{host}: {e}")),
    };
    let mut last = String::new();
    let mut s = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(6)) {
            Ok(c) => {
                s = Some(c);
                break;
            }
            Err(e) => last = e.to_string(),
        }
    }
    let Some(mut s) = s else { return fail(last) };
    let _ = s.set_nodelay(true);
    let Ok(w) = s.try_clone() else { return fail("clone".into()) };
    let _ = tx.send(TEvt::Up(w));
    let mut reply_w = s.try_clone().ok();
    let mut iac = Iac::default();
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) => return fail(String::new()),
            Ok(n) => {
                let mut reply = vec![];
                let data = iac.feed(&buf[..n], &mut reply, &naws);
                if !reply.is_empty() {
                    if let Some(w) = &mut reply_w {
                        let _ = w.write_all(&reply);
                    }
                }
                if !data.is_empty() && tx.send(TEvt::Data(data)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            Err(e) => return fail(e.to_string()),
        }
    }
}

// ---------------------------------------------------------------- 会话

pub enum Link {
    Ssh(Box<Shell>),
    Telnet(Telnet, TermView),
}

pub struct Session {
    pub host: Host,
    pub link: Link,
    pub up: bool,
    pub closed: Option<String>,
    pub since: Instant,
    init_sent: bool,
    /// 收到数据的时刻（标签上闪一下）
    pub rx_at: Option<Instant>,
}

impl Session {
    pub fn open(ctx: &egui::Context, host: &Host, n: usize) -> Result<Session, String> {
        let link = match host.proto {
            Proto::Ssh => {
                let Some(prog) = ssh_program() else {
                    return Err(t("没有找到 ssh 命令。Windows 10 及以上可以在「设置 → 应用 → 可选功能」里添加 OpenSSH 客户端").into());
                };
                let kind = ShellKind {
                    name: host.title(),
                    program: prog,
                    args: host.ssh_args(),
                };
                let home = PathBuf::from(std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_else(|_| ".".into()));
                let mut sh = Shell::spawn(ctx, &kind, &home, 100_000 + n)?;
                sh.remote = true;
                Link::Ssh(Box::new(sh))
            }
            Proto::Telnet => {
                let mut tv = TermView::new(("telnet", n), 100, 24);
                tv.focus_req = true;
                tv.bg = crate::bgimg::Slot::Shell;
                tv.blocks = true;
                let msg = format!("\x1b[2m{}\x1b[0m\r\n", tf("正在连接 {}:{} …", &[&host.host, &host.port]));
                tv.feed(msg.as_bytes());
                Link::Telnet(Telnet::connect(ctx, &host.host, host.port), tv)
            }
        };
        Ok(Session {
            host: host.clone(),
            up: host.proto == Proto::Ssh,
            link,
            closed: None,
            since: Instant::now(),
            init_sent: false,
            rx_at: None,
        })
    }

    pub fn tv(&mut self) -> &mut TermView {
        match &mut self.link {
            Link::Ssh(sh) => &mut sh.tv,
            Link::Telnet(_, tv) => tv,
        }
    }

    pub fn write(&mut self, d: &[u8]) {
        match &mut self.link {
            Link::Ssh(sh) => sh.write(d),
            Link::Telnet(tn, _) => tn.write(d),
        }
    }

    pub fn poll(&mut self) {
        match &mut self.link {
            Link::Ssh(sh) => {
                let before = sh.tv.term.s.scrollback.len() + sh.tv.term.s.y;
                sh.poll();
                if sh.tv.term.s.scrollback.len() + sh.tv.term.s.y != before {
                    self.rx_at = Some(Instant::now());
                }
                if sh.exited.is_some() && self.closed.is_none() {
                    self.closed = Some(String::new());
                }
            }
            Link::Telnet(tn, tv) => {
                while let Ok(e) = tn.rx.try_recv() {
                    match e {
                        TEvt::Up(w) => {
                            tn.w = Some(w);
                            self.up = true;
                        }
                        TEvt::Data(d) => {
                            let reply = tv.feed(&d);
                            if !reply.is_empty() {
                                tn.write(&reply);
                            }
                            self.rx_at = Some(Instant::now());
                        }
                        TEvt::Closed(e) => {
                            tn.w = None;
                            let why = if e.is_empty() { t("连接已关闭").to_string() } else { tf("连接失败：{}", &[&e]) };
                            let msg = format!("\r\n\x1b[2m[{why} · {}]\x1b[0m\r\n", t("按回车重新连接"));
                            tv.feed(msg.as_bytes());
                            self.closed = Some(e);
                        }
                    }
                }
            }
        }
        // 连上后发初始化命令（Telnet 等到收到第一段数据，SSH 等 1.5 秒让登录完成）
        if !self.init_sent && self.closed.is_none() && !self.host.init.trim().is_empty() {
            let ready = match self.link {
                Link::Ssh(_) => self.since.elapsed() > Duration::from_millis(1500) && self.rx_at.is_some(),
                Link::Telnet(..) => self.up && self.rx_at.is_some(),
            };
            if ready {
                self.init_sent = true;
                let cmds: String = self.host.init.lines().filter(|l| !l.trim().is_empty()).map(|l| format!("{l}\r")).collect();
                self.write(cmds.as_bytes());
            }
        }
    }

    pub fn sync_size(&mut self) {
        match &mut self.link {
            Link::Ssh(sh) => sh.sync_size(),
            Link::Telnet(tn, tv) => tn.sync_size(tv.term.s.cols, tv.term.s.rows),
        }
    }

    pub fn alive(&self) -> bool {
        self.closed.is_none()
    }
}

// ---------------------------------------------------------------- scp

pub struct Xfer {
    pub what: String,
    pub rx: Receiver<Result<(), String>>,
}

/// 上传本地文件 / 文件夹到 host 的 dir
pub fn upload(ctx: &egui::Context, host: &Host, files: &[PathBuf], dir: &str) -> Xfer {
    let mut args = host.scp_args();
    args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
    let dir = if dir.trim().is_empty() { "~" } else { dir.trim() };
    args.push(format!("{}:{}", host.target(), dir));
    let names: Vec<String> = files.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
    run_scp(ctx, args, format!("{} → {}:{dir}", names.join(", "), host.title()))
}

/// 下载远程文件到本地文件夹
pub fn download(ctx: &egui::Context, host: &Host, remote: &str, local: &Path) -> Xfer {
    let mut args = host.scp_args();
    args.push(format!("{}:{}", host.target(), remote.trim()));
    args.push(local.to_string_lossy().into_owned());
    run_scp(ctx, args, format!("{}:{} → {}", host.title(), remote.trim(), local.display()))
}

fn run_scp(ctx: &egui::Context, args: Vec<String>, what: String) -> Xfer {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let run = |legacy: bool| {
            let mut cmd = std::process::Command::new(scp_program());
            if legacy {
                cmd.arg("-O");
            }
            cmd.args(&args).stdin(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            cmd.output()
        };
        let mut out = run(false);
        // 新版 scp 走 SFTP；开发板上的 dropbear 等没有 sftp-server，退回老的 scp 协议再试
        if matches!(&out, Ok(o) if !o.status.success() && String::from_utf8_lossy(&o.stderr).contains("subsystem request failed")) {
            out = run(true);
        }
        let r = match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => {
                let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
                Err(if e.contains("Permission denied") || e.contains("publickey") {
                    t("scp 需要密钥登录（密码登录的主机请在终端里用 rz/sz 或手动 scp）").to_string()
                } else if e.is_empty() {
                    format!("exit {}", o.status.code().unwrap_or(-1))
                } else {
                    e.lines().last().unwrap_or("").to_string()
                })
            }
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    Xfer { what, rx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host() {
        let h = Host::parse("pi@192.168.1.5:2222").unwrap();
        assert_eq!((h.proto, h.user.as_str(), h.host.as_str(), h.port), (Proto::Ssh, "pi", "192.168.1.5", 2222));
        let h = Host::parse("telnet://10.0.0.1").unwrap();
        assert_eq!((h.proto, h.host.as_str(), h.port), (Proto::Telnet, "10.0.0.1", 23));
        let h = Host::parse("ssh root@[fe80::1]:22").unwrap();
        assert_eq!((h.host.as_str(), h.port), ("fe80::1", 22));
        assert_eq!(h.target(), "root@[fe80::1]");
        assert!(Host::parse("  ").is_none());
        assert_eq!(split_args(r#"-J jump -o "A=b c""#), vec!["-J", "jump", "-o", "A=b c"]);
    }

    #[test]
    fn telnet_iac() {
        let mut i = Iac::default();
        let mut r = vec![];
        let naws = AtomicBool::new(false);
        let d = [b'h', IAC, DO, NAWS, IAC, WILL, ECHO, b'i', IAC, IAC, IAC, SB, TTYPE, 1, IAC, SE, b'!'];
        let out = i.feed(&d, &mut r, &naws);
        assert_eq!(out, vec![b'h', b'i', IAC, b'!']);
        assert!(naws.load(Ordering::Relaxed));
        assert!(r.starts_with(&[IAC, WILL, NAWS, IAC, DO, ECHO, IAC, SB, TTYPE, 0]));
    }
}

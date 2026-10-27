//! TFTP 服务器（RFC 1350，支持 blksize / tsize / timeout 选项）。U-Boot 的 `tftpboot` 从这里下载内核、固件。

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;

use super::{now, safe_join, Shared, Xfer};
use crate::i18n::{t, tf};

const RRQ: u16 = 1;
const WRQ: u16 = 2;
const DATA: u16 = 3;
const ACK: u16 = 4;
const ERROR: u16 = 5;
const OACK: u16 = 6;

pub struct Config {
    pub root: PathBuf,
    pub port: u16,
    pub allow_write: bool,
}

/// 启动服务器。端口被占用 / 没权限时直接返回错误
pub fn start(cfg: Config, ctx: egui::Context) -> Result<Shared, String> {
    let sock = UdpSocket::bind(("0.0.0.0", cfg.port)).map_err(|e| bind_err(cfg.port, e))?;
    sock.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let sh = Shared::default();
    let sh2 = sh.clone();
    std::thread::Builder::new()
        .name("tftp".into())
        .spawn(move || {
            let mut buf = [0u8; 1024];
            while !sh2.stopped() {
                let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
                let pkt = buf[..n].to_vec();
                let (sh3, ctx3, root, w) = (sh2.clone(), ctx.clone(), cfg.root.clone(), cfg.allow_write);
                let _ = std::thread::Builder::new().name("tftp-xfer".into()).spawn(move || {
                    session(&pkt, peer, &root, w, &sh3, &ctx3);
                    ctx3.request_repaint();
                });
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(sh)
}

pub(crate) fn bind_err(port: u16, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        tf("端口 {} 需要管理员权限，换一个大于 1024 的端口试试", &[&port])
    } else if e.kind() == std::io::ErrorKind::AddrInUse {
        tf("端口 {} 已被占用", &[&port])
    } else {
        e.to_string()
    }
}

/// 请求包：文件名、模式、选项
struct Req {
    op: u16,
    file: String,
    opts: Vec<(String, String)>,
}

fn parse_req(p: &[u8]) -> Option<Req> {
    if p.len() < 4 {
        return None;
    }
    let op = u16::from_be_bytes([p[0], p[1]]);
    let mut parts = p[2..].split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    let file = parts.next()?;
    let _mode = parts.next()?;
    let mut opts = vec![];
    while let (Some(k), Some(v)) = (parts.next(), parts.next()) {
        if k.is_empty() {
            break;
        }
        opts.push((k.to_ascii_lowercase(), v));
    }
    Some(Req { op, file, opts })
}

/// 发给对方的错误文本用 RFC 里的英文（U-Boot 等只认 ASCII），界面上显示的是本地化的那句
fn err_pkt(code: u16, msg: &str) -> Vec<u8> {
    let msg = match code {
        1 => "File not found",
        2 => "Access violation",
        _ if msg.is_ascii() => msg,
        _ => "Undefined error",
    };
    let mut v = vec![0, ERROR as u8];
    v.extend_from_slice(&code.to_be_bytes());
    v.extend_from_slice(msg.as_bytes());
    v.push(0);
    v
}

fn session(p: &[u8], peer: SocketAddr, root: &std::path::Path, allow_write: bool, sh: &Shared, ctx: &egui::Context) {
    let Some(req) = parse_req(p) else { return };
    if req.op != RRQ && req.op != WRQ {
        return;
    }
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return };
    let _ = sock.connect(peer);
    let upload = req.op == WRQ;
    let i = sh.push(Xfer {
        at: now(),
        peer: peer.ip().to_string(),
        file: req.file.clone(),
        upload,
        bytes: 0,
        total: None,
        done: None,
        status: 0,
    });
    let fail = |code: u16, msg: String| {
        let _ = sock.send(&err_pkt(code, &msg));
        sh.update(i, |x| x.done = Some(Err(msg)));
    };
    let Some(path) = safe_join(root, &req.file) else {
        return fail(2, t("路径不允许").into());
    };
    // 协商选项
    let mut blksize = 512usize;
    let mut timeout = Duration::from_secs(3);
    let mut oack: Vec<(String, String)> = vec![];
    let mut tsize_req = false;
    for (k, v) in &req.opts {
        match k.as_str() {
            "blksize" => {
                if let Ok(n) = v.parse::<usize>() {
                    blksize = n.clamp(8, 65464).min(1468);
                    oack.push(("blksize".into(), blksize.to_string()));
                }
            }
            "timeout" => {
                if let Ok(n) = v.parse::<u64>() {
                    if (1..=255).contains(&n) {
                        timeout = Duration::from_secs(n);
                        oack.push(("timeout".into(), n.to_string()));
                    }
                }
            }
            "tsize" => {
                tsize_req = true;
                if upload {
                    if let Ok(n) = v.parse::<u64>() {
                        sh.update(i, |x| x.total = Some(n));
                    }
                    oack.push(("tsize".into(), v.clone()));
                }
            }
            _ => {}
        }
    }
    sock.set_read_timeout(Some(timeout)).ok();
    let r = if upload {
        if !allow_write {
            return fail(2, t("服务器不允许上传").into());
        }
        receive(&sock, &path, blksize, &oack, sh, i, ctx)
    } else {
        let f = match File::open(&path) {
            Ok(f) if path.is_file() => f,
            _ => return fail(1, t("文件不存在").into()),
        };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        sh.update(i, |x| x.total = Some(size));
        if tsize_req {
            oack.push(("tsize".into(), size.to_string()));
        }
        send(&sock, f, blksize, &oack, sh, i, ctx)
    };
    match r {
        Ok(()) => sh.update(i, |x| x.done = Some(Ok(()))),
        Err(e) => fail(0, e),
    }
}

fn oack_pkt(opts: &[(String, String)]) -> Vec<u8> {
    let mut v = vec![0, OACK as u8];
    for (k, val) in opts {
        v.extend_from_slice(k.as_bytes());
        v.push(0);
        v.extend_from_slice(val.as_bytes());
        v.push(0);
    }
    v
}

/// 发一个包并等对应块号的 ACK，超时重发 5 次
fn send_wait_ack(sock: &UdpSocket, pkt: &[u8], block: u16, sh: &Shared) -> Result<(), String> {
    let mut buf = [0u8; 516];
    for _ in 0..5 {
        sock.send(pkt).map_err(|e| e.to_string())?;
        loop {
            if sh.stopped() {
                return Err(t("服务器已停止").into());
            }
            match sock.recv(&mut buf) {
                Ok(n) if n >= 4 => {
                    let op = u16::from_be_bytes([buf[0], buf[1]]);
                    let b = u16::from_be_bytes([buf[2], buf[3]]);
                    if op == ACK && b == block {
                        return Ok(());
                    }
                    if op == ERROR {
                        return Err(tf("客户端取消：{}", &[&String::from_utf8_lossy(&buf[4..n]).trim_end_matches('\0')]));
                    }
                    // 旧块的重复 ACK：继续等
                }
                Ok(_) => {}
                Err(_) => break, // 超时，重发
            }
        }
    }
    Err(t("客户端没有响应（超时）").into())
}

fn send(sock: &UdpSocket, mut f: File, blksize: usize, oack: &[(String, String)], sh: &Shared, i: usize, ctx: &egui::Context) -> Result<(), String> {
    if !oack.is_empty() {
        send_wait_ack(sock, &oack_pkt(oack), 0, sh)?;
    }
    let mut block: u16 = 1;
    let mut sent = 0u64;
    let mut data = vec![0u8; blksize];
    loop {
        let mut n = 0;
        while n < blksize {
            let r = f.read(&mut data[n..]).map_err(|e| e.to_string())?;
            if r == 0 {
                break;
            }
            n += r;
        }
        let mut pkt = vec![0, DATA as u8];
        pkt.extend_from_slice(&block.to_be_bytes());
        pkt.extend_from_slice(&data[..n]);
        send_wait_ack(sock, &pkt, block, sh)?;
        sent += n as u64;
        if block % 64 == 0 || n < blksize {
            sh.update(i, |x| x.bytes = sent);
            ctx.request_repaint();
        }
        if n < blksize {
            return Ok(());
        }
        block = block.wrapping_add(1);
    }
}

fn receive(sock: &UdpSocket, path: &std::path::Path, blksize: usize, oack: &[(String, String)], sh: &Shared, i: usize, ctx: &egui::Context) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tftp-part");
    let mut f = File::create(&tmp).map_err(|e| e.to_string())?;
    let first = if oack.is_empty() { vec![0, ACK as u8, 0, 0] } else { oack_pkt(oack) };
    let mut ack = first;
    let mut expect: u16 = 1;
    let mut got = 0u64;
    let mut buf = vec![0u8; blksize + 4];
    let mut tries = 0;
    sock.send(&ack).map_err(|e| e.to_string())?;
    loop {
        if sh.stopped() {
            let _ = std::fs::remove_file(&tmp);
            return Err(t("服务器已停止").into());
        }
        match sock.recv(&mut buf) {
            Ok(n) if n >= 4 && u16::from_be_bytes([buf[0], buf[1]]) == DATA => {
                let b = u16::from_be_bytes([buf[2], buf[3]]);
                if b == expect {
                    f.write_all(&buf[4..n]).map_err(|e| e.to_string())?;
                    got += (n - 4) as u64;
                    ack = vec![0, ACK as u8];
                    ack.extend_from_slice(&b.to_be_bytes());
                    sock.send(&ack).map_err(|e| e.to_string())?;
                    tries = 0;
                    if b % 64 == 0 || n - 4 < blksize {
                        sh.update(i, |x| x.bytes = got);
                        ctx.request_repaint();
                    }
                    if n - 4 < blksize {
                        drop(f);
                        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    expect = expect.wrapping_add(1);
                } else {
                    // 重复的旧块：再回一次 ACK
                    sock.send(&ack).map_err(|e| e.to_string())?;
                }
            }
            Ok(n) if n >= 4 && u16::from_be_bytes([buf[0], buf[1]]) == ERROR => {
                let _ = std::fs::remove_file(&tmp);
                return Err(tf("客户端取消：{}", &[&String::from_utf8_lossy(&buf[4..n]).trim_end_matches('\0')]));
            }
            Ok(_) => {}
            Err(_) => {
                tries += 1;
                if tries > 5 {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(t("客户端没有响应（超时）").into());
                }
                sock.send(&ack).map_err(|e| e.to_string())?;
            }
        }
    }
}

/// 测试用的极简 TFTP 客户端：下载一个文件
#[cfg(test)]
pub fn client_get(addr: &str, file: &str, blksize: Option<usize>) -> Result<Vec<u8>, String> {
    let s = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut req = vec![0, RRQ as u8];
    req.extend_from_slice(file.as_bytes());
    req.extend_from_slice(b"\0octet\0");
    let bs = blksize.unwrap_or(512);
    if let Some(b) = blksize {
        req.extend_from_slice(format!("blksize\0{b}\0").as_bytes());
    }
    s.send_to(&req, addr).map_err(|e| e.to_string())?;
    let mut out = vec![];
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = s.recv_from(&mut buf).map_err(|e| e.to_string())?;
        let op = u16::from_be_bytes([buf[0], buf[1]]);
        match op {
            OACK => {
                s.send_to(&[0, ACK as u8, 0, 0], from).unwrap();
            }
            DATA => {
                out.extend_from_slice(&buf[4..n]);
                s.send_to(&[0, ACK as u8, buf[2], buf[3]], from).unwrap();
                if n - 4 < bs {
                    return Ok(out);
                }
            }
            _ => return Err(String::from_utf8_lossy(&buf[4..n]).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_files() {
        let dir = std::env::temp_dir().join(format!("qccom-tftp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        std::fs::write(dir.join("fw.bin"), &data).unwrap();
        std::fs::write(dir.join("exact.bin"), vec![1u8; 1024]).unwrap();
        let port = 46969;
        let sh = start(
            Config {
                root: dir.clone(),
                port,
                allow_write: false,
            },
            egui::Context::default(),
        )
        .unwrap();
        let addr = format!("127.0.0.1:{port}");
        assert_eq!(client_get(&addr, "fw.bin", None).unwrap(), data);
        assert_eq!(client_get(&addr, "fw.bin", Some(1024)).unwrap(), data);
        // 正好是块大小整数倍：最后要发一个空块
        assert_eq!(client_get(&addr, "exact.bin", None).unwrap().len(), 1024);
        assert!(client_get(&addr, "../secret", None).is_err());
        assert!(client_get(&addr, "nope.bin", None).is_err());
        sh.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

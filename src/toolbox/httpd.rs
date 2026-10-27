//! 简单的 HTTP 文件服务器：把一个文件夹共享给开发板下载（OTA 固件、配置文件）。
//! 支持 GET / HEAD、目录列表、Range（断点续传），可选允许 PUT 上传。

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;

use super::{now, safe_join, Shared, Xfer};
use crate::i18n::t;

pub struct Config {
    pub root: PathBuf,
    pub port: u16,
    pub allow_upload: bool,
}

pub fn start(cfg: Config, ctx: egui::Context) -> Result<Shared, String> {
    let l = TcpListener::bind(("0.0.0.0", cfg.port)).map_err(|e| super::tftp::bind_err(cfg.port, e))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    let sh = Shared::default();
    let sh2 = sh.clone();
    std::thread::Builder::new()
        .name("httpd".into())
        .spawn(move || {
            while !sh2.stopped() {
                match l.accept() {
                    Ok((s, _)) => {
                        let (sh3, ctx3, root, up) = (sh2.clone(), ctx.clone(), cfg.root.clone(), cfg.allow_upload);
                        let _ = std::thread::Builder::new().name("httpd-conn".into()).spawn(move || {
                            let _ = s.set_nonblocking(false);
                            let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
                            handle(s, &root, up, &sh3, &ctx3);
                            ctx3.request_repaint();
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(sh)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("txt" | "log" | "md" | "c" | "h" | "ini" | "cfg") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn respond(s: &mut TcpStream, code: u16, reason: &str, headers: &[(&str, String)], body: &[u8], head: bool) {
    let mut h = format!("HTTP/1.1 {code} {reason}\r\nServer: QCCOM\r\nConnection: close\r\n");
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    if !headers.iter().any(|(k, _)| *k == "Content-Length") {
        h.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    h.push_str("\r\n");
    let _ = s.write_all(h.as_bytes());
    if !head {
        let _ = s.write_all(body);
    }
}

fn handle(s: TcpStream, root: &Path, allow_upload: bool, sh: &Shared, ctx: &egui::Context) {
    let peer = s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let mut r = BufReader::new(s);
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut len = 0u64;
    let mut range: Option<String> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "range" => range = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    let path_part = target.split('?').next().unwrap_or("/");
    let rel = percent_decode(path_part);
    let i = sh.push(Xfer {
        at: now(),
        peer,
        file: format!("{method} {rel}"),
        upload: method == "PUT",
        bytes: 0,
        total: None,
        done: None,
        status: 0,
    });
    let mut s = r.get_ref().try_clone().expect("clone stream");
    let fin = |code: u16, bytes: u64, err: Option<String>| {
        sh.update(i, |x| {
            x.status = code;
            x.bytes = bytes;
            x.done = Some(match err {
                Some(e) => Err(e),
                None => Ok(()),
            });
        });
        ctx.request_repaint();
    };
    let Some(path) = safe_join(root, &rel) else {
        respond(&mut s, 403, "Forbidden", &[], b"forbidden", false);
        return fin(403, 0, Some(t("路径不允许").into()));
    };
    let head = method == "HEAD";
    match method.as_str() {
        "GET" | "HEAD" => {
            if path.is_dir() {
                if !path_part.ends_with('/') {
                    respond(&mut s, 301, "Moved Permanently", &[("Location", format!("{path_part}/"))], b"", head);
                    return fin(301, 0, None);
                }
                let body = listing(&path, &rel);
                respond(&mut s, 200, "OK", &[("Content-Type", "text/html; charset=utf-8".into())], body.as_bytes(), head);
                return fin(200, body.len() as u64, None);
            }
            let Ok(mut f) = File::open(&path) else {
                respond(&mut s, 404, "Not Found", &[("Content-Type", "text/plain".into())], b"404 not found", head);
                return fin(404, 0, Some(t("文件不存在").into()));
            };
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            sh.update(i, |x| x.total = Some(size));
            // 只支持单段 Range：bytes=a-b / bytes=a- / bytes=-n
            let (start, end, partial) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-')) {
                Some((a, b)) if size > 0 => {
                    let (st, en) = match (a.trim().parse::<u64>().ok(), b.trim().parse::<u64>().ok()) {
                        (Some(a), Some(b)) => (a, b.min(size - 1)),
                        (Some(a), None) => (a, size - 1),
                        (None, Some(n)) => (size.saturating_sub(n), size - 1),
                        _ => (0, size - 1),
                    };
                    if st > en || st >= size {
                        respond(&mut s, 416, "Range Not Satisfiable", &[("Content-Range", format!("bytes */{size}"))], b"", head);
                        return fin(416, 0, None);
                    }
                    (st, en, true)
                }
                _ => (0, size.saturating_sub(1), false),
            };
            let n = if size == 0 { 0 } else { end - start + 1 };
            let mut hs = vec![("Content-Type", mime(&path).to_string()), ("Content-Length", n.to_string()), ("Accept-Ranges", "bytes".to_string())];
            if partial {
                hs.push(("Content-Range", format!("bytes {start}-{end}/{size}")));
            }
            let code = if partial { 206 } else { 200 };
            respond(&mut s, code, if partial { "Partial Content" } else { "OK" }, &hs, b"", true);
            if head {
                return fin(code, 0, None);
            }
            let _ = f.seek(SeekFrom::Start(start));
            let mut left = n;
            let mut sent = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            while left > 0 {
                if sh.stopped() {
                    return fin(code, sent, Some(t("服务器已停止").into()));
                }
                let want = (left as usize).min(buf.len());
                let k = match f.read(&mut buf[..want]) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => k,
                };
                if let Err(e) = s.write_all(&buf[..k]) {
                    return fin(code, sent, Some(e.to_string()));
                }
                sent += k as u64;
                left -= k as u64;
                sh.update(i, |x| x.bytes = sent);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            fin(code, sent, None)
        }
        "PUT" if allow_upload => {
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let Ok(mut f) = File::create(&path) else {
                respond(&mut s, 500, "Internal Server Error", &[], b"cannot write", false);
                return fin(500, 0, Some(t("无法写入文件").into()));
            };
            sh.update(i, |x| x.total = Some(len));
            let mut got = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            while got < len {
                let want = ((len - got) as usize).min(buf.len());
                let k = match r.read(&mut buf[..want]) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => k,
                };
                if f.write_all(&buf[..k]).is_err() {
                    break;
                }
                got += k as u64;
                sh.update(i, |x| x.bytes = got);
            }
            let ok = got == len;
            respond(&mut s, if ok { 201 } else { 400 }, if ok { "Created" } else { "Bad Request" }, &[], b"", false);
            fin(if ok { 201 } else { 400 }, got, (!ok).then(|| t("上传不完整").into()))
        }
        _ => {
            respond(
                &mut s,
                405,
                "Method Not Allowed",
                &[("Allow", if allow_upload { "GET, HEAD, PUT" } else { "GET, HEAD" }.into())],
                b"",
                false,
            );
            fin(405, 0, None)
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn listing(dir: &Path, rel: &str) -> String {
    let mut items: Vec<(bool, String, u64)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let md = e.metadata().ok();
                    (md.as_ref().is_some_and(|m| m.is_dir()), e.file_name().to_string_lossy().into_owned(), md.map_or(0, |m| m.len()))
                })
                .collect()
        })
        .unwrap_or_default();
    items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let title = html_escape(if rel.is_empty() { "/" } else { rel });
    let mut h = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>{title}</title>\
         <style>body{{font:14px system-ui,sans-serif;margin:24px;color:#222}}a{{color:#0a7;text-decoration:none}}td{{padding:3px 16px 3px 0}}.s{{color:#888;text-align:right}}</style>\
         <h3>QCCOM · {title}</h3><table>"
    );
    if rel.trim_matches('/') != "" {
        h.push_str("<tr><td><a href='../'>../</a></td><td></td></tr>");
    }
    for (is_dir, name, size) in items {
        let href = percent_encode(&name) + if is_dir { "/" } else { "" };
        let shown = html_escape(&name) + if is_dir { "/" } else { "" };
        let sz = if is_dir { String::new() } else { crate::api::human_size(size as usize) };
        h.push_str(&format!("<tr><td><a href='{href}'>{shown}</a></td><td class=s>{sz}</td></tr>"));
    }
    h.push_str("</table>");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(port: u16, req: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    #[test]
    fn serves_and_ranges() {
        let dir = std::env::temp_dir().join(format!("qccom-httpd-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub dir")).unwrap();
        std::fs::write(dir.join("fw.bin"), b"0123456789").unwrap();
        let port = 48081;
        let sh = start(
            Config {
                root: dir.clone(),
                port,
                allow_upload: true,
            },
            egui::Context::default(),
        )
        .unwrap();
        let r = get(port, "GET /fw.bin HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("0123456789"), "{r}");
        let r = get(port, "GET /fw.bin HTTP/1.1\r\nRange: bytes=2-4\r\n\r\n");
        assert!(r.starts_with("HTTP/1.1 206") && r.ends_with("234") && r.contains("bytes 2-4/10"), "{r}");
        let r = get(port, "GET / HTTP/1.1\r\n\r\n");
        assert!(r.contains("sub%20dir/") && r.contains("fw.bin"), "{r}");
        assert!(get(port, "GET /../x HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
        assert!(get(port, "GET /nope HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        let r = get(port, "PUT /up/a.txt HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc");
        assert!(r.starts_with("HTTP/1.1 201"), "{r}");
        assert_eq!(std::fs::read(dir.join("up/a.txt")).unwrap(), b"abc");
        sh.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

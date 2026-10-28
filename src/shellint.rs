//! 系统右键菜单「用 QCCOM 打开」「用 QCCOM 比较」，以及把命令行参数转给已经开着的窗口。
//! Windows 写当前用户的注册表（HKCU，不需要管理员权限）；Linux 装 .desktop、Nautilus 脚本和 Dolphin 服务菜单。

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::i18n::t;

/// 第 1 个窗口占的端口，其他进程把要打开的文件转给它
pub const PORT: u16 = 29171;

/// 转发的一条命令：打开文件 / 文件夹，或者加入比较
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    Open(PathBuf),
    Project(PathBuf),
    Compare(PathBuf),
    /// 一次给了两个：直接比较
    Pair(PathBuf, PathBuf),
}

impl Req {
    fn line(&self) -> String {
        match self {
            Req::Open(p) => format!("open\t{}", p.display()),
            Req::Project(p) => format!("proj\t{}", p.display()),
            Req::Compare(p) => format!("cmp\t{}", p.display()),
            Req::Pair(a, b) => format!("pair\t{}\t{}", a.display(), b.display()),
        }
    }

    fn parse(l: &str) -> Option<Req> {
        let mut it = l.trim_end_matches(['\r', '\n']).split('\t');
        let k = it.next()?;
        let a = PathBuf::from(it.next()?);
        Some(match k {
            "open" => Req::Open(a),
            "proj" => Req::Project(a),
            "cmp" => Req::Compare(a),
            "pair" => Req::Pair(a, PathBuf::from(it.next()?)),
            _ => return None,
        })
    }
}

/// 解析 --file / --project / --compare 参数；普通的已有文件参数也当成打开
pub fn parse_args(args: &[String]) -> Vec<Req> {
    let mut out = vec![];
    let mut mode = "";
    let mut cmp = vec![];
    for a in args {
        match a.as_str() {
            "--file" | "--project" | "--compare" => {
                mode = a.as_str();
                continue;
            }
            _ if a.starts_with("--") => {
                mode = "";
                continue;
            }
            _ => {}
        }
        let p = absolute(Path::new(a));
        match mode {
            "--file" if p.is_dir() => out.push(Req::Project(p)),
            "--file" => out.push(Req::Open(p)),
            "--project" => out.push(Req::Project(p)),
            "--compare" => cmp.push(p),
            _ if p.is_file() => out.push(Req::Open(p)),
            _ => {}
        }
    }
    match cmp.len() {
        0 => {}
        2 => out.push(Req::Pair(cmp[0].clone(), cmp[1].clone())),
        _ => out.extend(cmp.into_iter().map(Req::Compare)),
    }
    out
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// 已经有窗口开着就把请求转过去，返回 true（这个进程直接退出）。
/// 同时选中两个文件时资源管理器会一下子启动两个进程，抢不到端口的那个再等一下重试连接。
pub fn forward(reqs: &[Req]) -> bool {
    if reqs.is_empty() {
        return false;
    }
    for _ in 0..20 {
        if let Ok(mut s) = TcpStream::connect_timeout(&([127, 0, 0, 1], PORT).into(), std::time::Duration::from_millis(300)) {
            let mut msg = String::from("QCCOM1\n");
            for r in reqs {
                msg.push_str(&r.line());
                msg.push('\n');
            }
            return s.write_all(msg.as_bytes()).is_ok();
        }
        // 没人在听：自己占住端口当第 1 个窗口
        if let Ok(l) = TcpListener::bind(("127.0.0.1", PORT)) {
            crate::claim_instance(l);
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// 第 1 个窗口：在后台收其他进程转来的请求
pub fn listen(l: &TcpListener, ctx: eframe::egui::Context) -> Arc<Mutex<Vec<Req>>> {
    let q = Arc::new(Mutex::new(vec![]));
    let Ok(l) = l.try_clone() else { return q };
    let q2 = q.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut lines = BufReader::new(s).lines();
            // 只认自己的握手，别的程序误连进来不理
            if lines.next().and_then(|l| l.ok()).as_deref() != Some("QCCOM1") {
                continue;
            }
            let reqs: Vec<Req> = lines.map_while(|l| l.ok()).filter_map(|l| Req::parse(&l)).collect();
            if !reqs.is_empty() {
                q2.lock().unwrap().extend(reqs);
                ctx.request_repaint();
            }
        }
    });
    q
}

// ------------------------------------------------------------------ 右键菜单

fn exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

#[cfg(windows)]
mod sys {
    use super::*;
    use std::os::windows::process::CommandExt;

    const KEYS: [&str; 5] = [
        r"HKCU\Software\Classes\*\shell\QCCOM",
        r"HKCU\Software\Classes\*\shell\QCCOMCompare",
        r"HKCU\Software\Classes\Directory\shell\QCCOM",
        r"HKCU\Software\Classes\Directory\shell\QCCOMCompare",
        r"HKCU\Software\Classes\Directory\Background\shell\QCCOM",
    ];

    fn reg(args: &[&str]) -> Result<(), String> {
        let out = std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn installed() -> bool {
        reg(&["query", KEYS[0]]).is_ok()
    }

    pub fn install() -> Result<(), String> {
        let exe = exe()?.display().to_string();
        let icon = format!("\"{exe}\",0");
        let items = [
            (KEYS[0], t("用 QCCOM 打开"), "--file", "%1"),
            (KEYS[1], t("用 QCCOM 比较"), "--compare", "%1"),
            (KEYS[2], t("用 QCCOM 打开"), "--project", "%1"),
            (KEYS[3], t("用 QCCOM 比较"), "--compare", "%1"),
            (KEYS[4], t("用 QCCOM 打开"), "--project", "%V"),
        ];
        for (key, label, flag, arg) in items {
            reg(&["add", key, "/ve", "/d", label, "/f"])?;
            reg(&["add", key, "/v", "Icon", "/d", &icon, "/f"])?;
            let cmd = format!("\"{exe}\" {flag} \"{arg}\"");
            reg(&["add", &format!(r"{key}\command"), "/ve", "/d", &cmd, "/f"])?;
        }
        Ok(())
    }

    pub fn uninstall() -> Result<(), String> {
        for k in KEYS {
            let _ = reg(&["delete", k, "/f"]);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod sys {
    use super::*;

    fn home() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }

    fn files() -> [PathBuf; 5] {
        let share = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"));
        [
            share.join("applications/qccom.desktop"),
            share.join("icons/hicolor/64x64/apps/qccom.png"),
            share.join("nautilus/scripts").join(t("用 QCCOM 打开")),
            share.join("nautilus/scripts").join(t("用 QCCOM 比较")),
            share.join("kio/servicemenus/qccom.desktop"),
        ]
    }

    pub fn installed() -> bool {
        files()[0].exists()
    }

    fn write(p: &Path, data: &[u8], exec: bool) -> Result<(), String> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(p, data).map_err(|e| format!("{}: {e}", p.display()))?;
        if exec {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
        }
        Ok(())
    }

    pub fn install() -> Result<(), String> {
        let exe = exe()?.display().to_string();
        let f = files();
        let (open, cmp) = (t("用 QCCOM 打开"), t("用 QCCOM 比较"));
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=QCCOM\nComment={}\nExec=\"{exe}\" --file %F\nIcon=qccom\nTerminal=false\nCategories=Development;Utility;\nMimeType=text/plain;text/x-csrc;text/x-chdr;text/x-c++src;text/x-log;application/octet-stream;inode/directory;\nActions=compare;\n\n[Desktop Action compare]\nName={cmp}\nExec=\"{exe}\" --compare %F\n",
            t("串口、网络调试和代码查看")
        );
        write(&f[0], desktop.as_bytes(), false)?;
        // 图标：从内置的 64×64 RGBA 编成 PNG
        let rgba = include_bytes!("../assets/icon64.rgba").to_vec();
        if let Some(img) = image::RgbaImage::from_raw(64, 64, rgba) {
            let mut png = std::io::Cursor::new(vec![]);
            if img.write_to(&mut png, image::ImageFormat::Png).is_ok() {
                write(&f[1], &png.into_inner(), false)?;
            }
        }
        write(&f[2], format!("#!/bin/sh\n# qccom-menu\nexec \"{exe}\" --file \"$@\"\n").as_bytes(), true)?;
        write(&f[3], format!("#!/bin/sh\n# qccom-menu\nexec \"{exe}\" --compare \"$@\"\n").as_bytes(), true)?;
        let svc = format!(
            "[Desktop Entry]\nType=Service\nMimeType=all/all;inode/directory;\nActions=qccomOpen;qccomCompare;\nX-KDE-Priority=TopLevel\n\n[Desktop Action qccomOpen]\nName={open}\nIcon=qccom\nExec=\"{exe}\" --file %F\n\n[Desktop Action qccomCompare]\nName={cmp}\nIcon=qccom\nExec=\"{exe}\" --compare %F\n"
        );
        write(&f[4], svc.as_bytes(), true)?;
        let _ = std::process::Command::new("update-desktop-database").arg(f[0].parent().unwrap()).output();
        Ok(())
    }

    pub fn uninstall() -> Result<(), String> {
        let f = files();
        // Nautilus 脚本的文件名跟着界面语言，按内容里的标记找
        if let Some(dir) = f[2].parent() {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if std::fs::read_to_string(e.path()).is_ok_and(|s| s.contains("# qccom-menu")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        for p in f {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
}

pub use sys::{install, installed, uninstall};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_roundtrip() {
        for r in [Req::Open("/a b/c.txt".into()), Req::Pair("/x".into(), "/y".into()), Req::Compare("C:\\fw\\a.c".into())] {
            assert_eq!(Req::parse(&r.line()), Some(r));
        }
        let args: Vec<String> = ["--compare", "/tmp", "/"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_args(&args), vec![Req::Pair("/tmp".into(), "/".into())]);
    }
}

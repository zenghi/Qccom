//! 系统右键菜单「用 QCCOM 打开」「用 QCCOM 比较」，以及把命令行参数转给已经开着的窗口。
//! Windows 写当前用户的注册表（HKCU，不需要管理员权限）；Linux 装 .desktop、Nautilus 脚本和 Dolphin 服务菜单。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::i18n::t;

/// 第 1 个窗口占的端口，其他进程把要打开的文件转给它
pub const PORT: u16 = 29171;

/// 转发的一条命令：打开文件 / 文件夹，或者加入比较
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Req {
    Activate,
    Open(PathBuf),
    Project(PathBuf),
    Compare(PathBuf),
    /// 一次给了两个：直接比较
    Pair(PathBuf, PathBuf),
}

impl Req {
    fn line(&self) -> String {
        match self {
            Req::Activate => "activate".into(),
            Req::Open(p) => format!("open\t{}", p.display()),
            Req::Project(p) => format!("proj\t{}", p.display()),
            Req::Compare(p) => format!("cmp\t{}", p.display()),
            Req::Pair(a, b) => format!("pair\t{}\t{}", a.display(), b.display()),
        }
    }

    fn parse(l: &str) -> Option<Req> {
        let mut it = l.trim_end_matches(['\r', '\n']).split('\t');
        let k = it.next()?;
        if k == "activate" && it.next().is_none() {
            return Some(Req::Activate);
        }
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
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--file" | "--project" | "--compare" => {
                mode = a;
                continue;
            }
            "--open" | "--replay" => {
                args.next(); // This value belongs to the log viewer, not the editor.
                mode = "";
                continue;
            }
            _ if a.starts_with("--") => {
                mode = "";
                continue;
            }
            _ => {}
        }
        let p = absolute(&shell_path(a));
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

fn shell_path(arg: &str) -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = arg.strip_suffix('"') {
        // Older Explorer registrations wrapped a drive root as "D:\". Windows
        // consumes the backslash as an escape and delivers D:" instead.
        return PathBuf::from(format!("{p}\\"));
    }
    PathBuf::from(arg)
}

pub fn should_forward(args: &[String], reqs: &[Req]) -> bool {
    !reqs.is_empty() && !args.iter().any(|a| matches!(a.as_str(), "--new-window" | "--open" | "--replay")) && !reqs.iter().any(|r| matches!(r, Req::Project(_)))
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
            // 这个进程是资源管理器刚启动的，有权把窗口切到前台：把这个权利让给已经开着的窗口
            allow_foreground();
            if s.write_all(msg.as_bytes()).is_err() {
                return false;
            }
            if reqs == [Req::Activate] {
                // An older running build ignores Activate. Open a window if it never acknowledges.
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(750)));
                let _ = s.shutdown(Shutdown::Write);
                let mut ack = [0];
                return s.read_exact(&mut ack).is_ok() && ack == [1];
            }
            return true;
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
        for mut s in l.incoming().flatten() {
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut lines = BufReader::new(&s).lines();
            // 只认自己的握手，别的程序误连进来不理
            if lines.next().and_then(|l| l.ok()).as_deref() != Some("QCCOM1") {
                continue;
            }
            let reqs: Vec<Req> = lines.map_while(|l| l.ok()).filter_map(|l| Req::parse(&l)).collect();
            if !reqs.is_empty() {
                let acknowledge = reqs == [Req::Activate];
                q2.lock().unwrap().extend(reqs);
                // 窗口最小化时界面不刷新，请求会一直排队：在这个线程里直接把窗口还原、提到前台
                raise_window(true);
                ctx.request_repaint();
                if acknowledge {
                    let _ = s.write_all(&[1]);
                }
            }
        }
    });
    q
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn AllowSetForegroundWindow(pid: u32) -> i32;
    fn IsIconic(hwnd: isize) -> i32;
    fn ShowWindowAsync(hwnd: isize, cmd: i32) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}

#[cfg(windows)]
static HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn register_window(_cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = _cc.window_handle() {
            if let RawWindowHandle::Win32(window) = handle.as_raw() {
                HWND.store(window.hwnd.get(), std::sync::atomic::Ordering::Release);
            }
        }
    }
}

fn allow_foreground() {
    #[cfg(windows)]
    unsafe {
        // ASFW_ANY
        AllowSetForegroundWindow(u32::MAX);
    }
}

fn raise_window(_background: bool) {
    #[cfg(windows)]
    {
        let hwnd = HWND.load(std::sync::atomic::Ordering::Acquire);
        if hwnd == 0 {
            return;
        }
        unsafe {
            if IsIconic(hwnd) != 0 {
                const SW_RESTORE: i32 = 9;
                if _background {
                    // Wake the UI thread even if it stopped drawing while minimized.
                    ShowWindowAsync(hwnd, SW_RESTORE);
                    return;
                }
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
}

pub fn show_forwarded_window(ctx: &eframe::egui::Context) {
    use eframe::egui::ViewportCommand;
    raise_window(false);
    #[cfg(not(windows))]
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
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

    const PROGID: &str = "QCCOM.Editor";

    fn classes() -> std::io::Result<winreg::RegKey> {
        winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .create_subkey(r"Software\Classes").map(|(k,_)| k)
    }

    fn associations(exe: &str) -> std::io::Result<()> {
        let classes = classes()?;
        let (prog, _) = classes.create_subkey(PROGID)?;
        prog.set_value("", &"QCCOM Editor")?;
        prog.create_subkey("DefaultIcon")?.0.set_value("", &format!("\"{exe}\",0"))?;
        prog.create_subkey(r"shell\open\command")?.0.set_value("", &format!("\"{exe}\" --file \"%1\""))?;
        for (_, extensions) in crate::highlight::EMBEDDED_TYPES {
            for ext in *extensions {
                let (list, _) = classes.create_subkey(format!(r".{ext}\OpenWithProgids"))?;
                list.set_value(PROGID, &"")?;
            }
        }
        prog.set_value("AssociationVersion", &1u32)?;
        Ok(())
    }

    // Upgrade the Explorer integration only for the executable the user already registered.
    pub fn upgrade_associations() {
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let Ok(classes) = hkcu.open_subkey(r"Software\Classes") else { return };
        let Ok(exe) = exe() else { return };
        let command = format!("\"{}\" --file \"%1\"", exe.display());
        let owned = classes.open_subkey(r"*\shell\QCCOM\command").and_then(|k| k.get_value::<String,_>("")).is_ok_and(|s| s == command);
        if owned && !classes.open_subkey(PROGID).and_then(|k| k.get_value::<u32,_>("AssociationVersion")).is_ok_and(|v| v == 1) {
            let _ = associations(&exe.to_string_lossy());
        }
    }

    fn remove_associations() -> std::io::Result<()> {
        let classes = classes()?;
        for (_, extensions) in crate::highlight::EMBEDDED_TYPES {
            for ext in *extensions {
                if let Ok(list) = classes.open_subkey_with_flags(format!(r".{ext}\OpenWithProgids"),winreg::enums::KEY_SET_VALUE) {
                    if let Err(e) = list.delete_value(PROGID) {
                        if e.kind() != std::io::ErrorKind::NotFound { return Err(e); }
                    }
                }
            }
        }
        match classes.delete_subkey_all(PROGID) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

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
            (KEYS[2], t("用 QCCOM 打开"), "--project", r"%1\."),
            (KEYS[3], t("用 QCCOM 比较"), "--compare", "%1"),
            (KEYS[4], t("用 QCCOM 打开"), "--project", r"%V\."),
        ];
        for (key, label, flag, arg) in items {
            reg(&["add", key, "/ve", "/d", label, "/f"])?;
            reg(&["add", key, "/v", "Icon", "/d", &icon, "/f"])?;
            let cmd = format!("\"{exe}\" {flag} \"{arg}\"");
            reg(&["add", &format!(r"{key}\command"), "/ve", "/d", &cmd, "/f"])?;
        }
        associations(&exe).map_err(|e| e.to_string())
    }

    pub fn uninstall() -> Result<(), String> {
        remove_associations().map_err(|e| e.to_string())?;
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
            "[Desktop Entry]\nType=Application\nName=QCCOM\nComment={}\nExec=\"{exe}\" --file %F\nIcon=qccom\nTerminal=false\nCategories=Development;Utility;\nMimeType=text/plain;text/x-csrc;text/x-chdr;text/x-c++src;text/x-c++hdr;text/rust;text/x-rust;application/json;application/toml;application/yaml;application/xml;text/x-asm;text/x-makefile;text/x-cmake;text/x-python;text/x-log;application/octet-stream;inode/directory;\nActions=compare;\n\n[Desktop Action compare]\nName={cmp}\nExec=\"{exe}\" --compare %F\n",
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

#[cfg(windows)]
pub use sys::upgrade_associations;
pub use sys::{install, installed, uninstall};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_roundtrip() {
        for r in [Req::Activate, Req::Open("/a b/c.txt".into()), Req::Pair("/x".into(), "/y".into()), Req::Compare("C:\\fw\\a.c".into())] {
            assert_eq!(Req::parse(&r.line()), Some(r));
        }
        let a = std::env::temp_dir();
        let b = std::env::current_dir().unwrap();
        let args = vec!["--compare".into(), a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned()];
        let reqs = parse_args(&args);
        assert_eq!(reqs, vec![Req::Pair(a, b)]);
        assert!(should_forward(&args, &reqs));
    }

    #[test]
    fn explorer_folder_opens_in_a_new_window() {
        assert!(!should_forward(&[], &parse_args(&[])));
        let folder = std::env::temp_dir();
        for flag in ["--project", "--file"] {
            let args = vec![flag.into(), folder.to_string_lossy().into_owned()];
            let reqs = parse_args(&args);
            assert_eq!(reqs, vec![Req::Project(folder.clone())]);
            assert!(!should_forward(&args, &reqs));
        }
        let file = folder.join("file with spaces.c");
        let mut args = vec!["--file".into(), file.to_string_lossy().into_owned()];
        let reqs = parse_args(&args);
        assert_eq!(reqs, vec![Req::Open(file)]);
        assert!(should_forward(&args, &reqs));
        args.push("--new-window".into());
        assert!(!should_forward(&args, &reqs));
    }

    #[cfg(windows)]
    #[test]
    fn legacy_explorer_drive_root_stays_absolute() {
        let args = vec!["--project".into(), "D:\"".into()];
        assert_eq!(parse_args(&args), vec![Req::Project(PathBuf::from("D:\\"))]);
    }
}

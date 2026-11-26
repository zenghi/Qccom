//! 下载并校验新版本，替换程序后重启。
//!
//! 版本文件格式（UTF-8 JSON）：
//! ```json
//! {
//!   "version": "0.5.1",
//!   "url": "https://order-1309900828.cos.ap-shanghai.myqcloud.com/qccom.exe",
//!   "sha256": "下载文件的 SHA-256（64 位十六进制）",
//!   "size": 3123456,
//!   "date": "2026-10-27",
//!   "notes": "1. 新增 xxx\n2. 修复 yyy",
//!   "force": false,
//!   "url_linux": "",
//!   "sha256_linux": ""
//! }
//! ```
//! 只有 version 必填；url 省略时用默认地址；sha256 强烈建议填，填了就必须一致才会替换。
//! Linux 版只认 url_linux / sha256_linux，没填就只提示有新版、不下载（避免下到 Windows 的 exe）。

use crate::i18n::{t, tf};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc::Sender, OnceLock};
use std::thread;
use std::time::Duration;
use tempfile::TempPath;

use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const VERSION_URL: &str = "https://www.zenghi.com/1.json";

/// 版本文件地址；环境变量 QCCOM_UPDATE_URL 可以临时指向测试服务器
pub fn version_url() -> String {
    std::env::var("QCCOM_UPDATE_URL").unwrap_or_else(|_| VERSION_URL.into())
}
pub const DEFAULT_EXE_URL: &str = "https://order-1309900828.cos.ap-shanghai.myqcloud.com/qccom.exe";

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub date: String,
    pub notes: String,
    pub force: bool,
    pub url_linux: String,
    pub sha256_linux: String,
}

pub enum UpdEvt {
    /// 检查结果：Some = 有新版
    Checked(Result<Option<Release>, String>),
        Progress(u64, u64),
    /// 下载并校验完成，准备好的新文件路径
        Ready(TempPath),
        Failed(String),
}

pub(crate) fn agent() -> Result<ureq::Agent, String> {
    Ok(agent_builder()?.build())
}

pub(crate) fn agent_builder() -> Result<ureq::AgentBuilder, String> {
    let b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .try_proxy_from_env(true)
        .user_agent(concat!("QCCOM/", env!("CARGO_PKG_VERSION")));
    #[cfg(windows)]
    let b = b.tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new().map_err(|e| e.to_string())?));
    Ok(b)
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => tf("服务器返回 {}", &[&code]),
        ureq::Error::Transport(t) => {
            // 证书问题给一句看得懂的说明（Windows 的原文是“证书中的 CN 名称与传递的值不匹配”之类）
            let raw = t.to_string();
            let low = raw.to_lowercase();
            let cert_name = ["cn", "principal name", "notvalidforname", "not valid for", "hostname mismatch", "0x800b010f", "主体名称"];
            let cert_any = ["certificate", "证书", "untrusted", "unknownissuer", "expired", "0x800b01"];
            if cert_name.iter().any(|k| low.contains(k)) && cert_any.iter().any(|k| low.contains(k)) || raw.contains("CN 名称") {
                tf("服务器的 HTTPS 证书和域名不匹配：证书要包含更新地址里的域名（{}）", &[&raw])
            } else if cert_any.iter().any(|k| low.contains(k)) {
                tf("服务器的 HTTPS 证书无效（过期、自签或缺少证书链）：{}", &[&raw])
            } else {
                tf("网络错误：{}", &[&raw])
            }
        }
    }
}

/// "0.10.2" > "0.9.9"；忽略前缀 v 和预发布后缀
pub fn newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(remote), parse(local));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

pub fn check(tx: Sender<UpdEvt>, ctx: egui::Context) {
    thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let r = (|| -> Result<Option<Release>, String> {
                let body = agent()?.get(&version_url()).call().map_err(http_err)?.into_string().map_err(|e| e.to_string())?;
                let mut r: Release = serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|e| tf("版本文件格式不对：{}", &[&e]))?;
                if r.version.trim().is_empty() {
                    return Err(t("版本文件里没有 version 字段").into());
                }
                if cfg!(windows) {
                    if r.url.trim().is_empty() {
                        r.url = DEFAULT_EXE_URL.into();
                    }
                } else {
                    // 本平台的下载地址；为空表示只提示、不自动下载
                    r.url = std::mem::take(&mut r.url_linux);
                    r.sha256 = std::mem::take(&mut r.sha256_linux);
                    r.size = 0; // size 记录的是 Windows 版文件大小
                }
                Ok(newer(&r.version, env!("CARGO_PKG_VERSION")).then_some(r))
            })();
            let _ = tx.send(UpdEvt::Checked(r));
            ctx.request_repaint();
        })
        .ok();
}

fn download_url_ok(url: &str) -> bool {
    ureq::get(url)
        .request_url()
        .ok()
        .is_some_and(|parsed| {
            let url = parsed.as_url();
            let local_http = url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            (url.scheme() == "https" || local_http) && url.username().is_empty() && url.password().is_none()
        })
}

fn current_executable() -> Result<&'static (PathBuf, Metadata), String> {
    static CURRENT: OnceLock<Result<(PathBuf, Metadata), String>> = OnceLock::new();
    // Remember the launch path. On Linux current_exe() follows the old file
    // if another QCCOM window has already renamed it during an update.
    CURRENT.get_or_init(|| {
        let path = std::env::current_exe().map_err(|e| e.to_string())?;
        let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
        Ok((path, metadata))
    }).as_ref().map_err(Clone::clone)
}

fn fetch_update(r: &Release, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<TempPath, String> {
    let url = r.url.trim();
    let invalid_url = || t("更新下载地址须使用 HTTPS，本机测试可使用 HTTP。").to_owned();
    if !download_url_ok(url) {
        return Err(invalid_url());
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".qccom-update-")
        .tempfile_in(dir)
        .map_err(|e| tf("无法写入 {}：{}", &[&dir.display(), &e]))?;
    let resp = agent()?.get(url).call().map_err(http_err)?;
    if !download_url_ok(resp.get_url()) {
        return Err(invalid_url());
    }
    let length = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
    let total = if r.size > 0 { r.size } else { length.unwrap_or(0) };
    if let Some(actual) = length.filter(|&size| r.size > 0 && size != r.size) {
        return Err(tf("文件不完整：{} / {} 字节", &[&actual, &total]));
    }
    let mut reader = resp.into_reader();
    let mut hash = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    let mut got = 0;
    let magic: &[u8] = if cfg!(windows) { b"MZ" } else { b"\x7fELF" };
    let mut head = Vec::with_capacity(magic.len());
    let mut tick = std::time::Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| tf("下载中断：{}", &[&e]))?;
        if n == 0 {
            break;
        }
        got += n as u64;
        if total > 0 && got > total {
            return Err(tf("文件不完整：{} / {} 字节", &[&got, &total]));
        }
        head.extend_from_slice(&buf[..n.min(magic.len() - head.len())]);
        hash.update(&buf[..n]);
        staged.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        if tick.elapsed() >= Duration::from_millis(50) {
            progress(got, total);
            tick = std::time::Instant::now();
        }
    }
    if total > 0 && got != total {
        return Err(tf("文件不完整：{} / {} 字节", &[&got, &total]));
    }
    if !head.starts_with(magic) {
        return Err(t("下载到的不是可执行程序（服务器可能返回了网页）").into());
    }
    let want = r.sha256.trim();
    if !want.is_empty() {
        let actual = format!("{:x}", hash.finalize());
        if !actual.eq_ignore_ascii_case(want) {
            return Err(t("SHA-256 校验不一致，文件可能损坏或被篡改，已删除").into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged.as_file().set_permissions(fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    staged.as_file().sync_all().map_err(|e| e.to_string())?;
    Ok(staged.into_temp_path())
}

pub fn download(r: Release, tx: Sender<UpdEvt>, ctx: egui::Context) {
    let result = thread::Builder::new().name("update-dl".into()).spawn({
        let tx = tx.clone();
        let ctx = ctx.clone();
        move || {
            let res = current_executable().and_then(|(exe, _)| {
                let dir = exe.parent().unwrap_or_else(|| Path::new("."));
                fetch_update(&r, dir, |got, total| {
                    let _ = tx.send(UpdEvt::Progress(got, total));
                    ctx.request_repaint();
                })
            });
            let _ = tx.send(match res {
                Ok(p) => UpdEvt::Ready(p),
                Err(e) => UpdEvt::Failed(e),
            });
            ctx.request_repaint();
        }
    });
    if let Err(e) = result {
        let _ = tx.send(UpdEvt::Failed(e.to_string()));
        ctx.request_repaint();
    }
}

fn lock_update(exe: &Path) -> Result<File, String> {
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(exe.with_extension("qccom-update-lock"))
        .map_err(|e| tf("没有权限替换程序（{}）。可以把程序放到有写权限的文件夹再试", &[&e]))?;
    lock.try_lock().map_err(|e| tf("替换失败：{}", &[&e]))?;
    Ok(lock)
}

fn install_into(exe: &Path, original: &Metadata, new: &Path, start: impl FnOnce() -> std::io::Result<()>) -> Result<(), String> {
    let _lock = lock_update(exe)?;
    let current = fs::metadata(exe).map_err(|e| tf("替换失败：{}", &[&e]))?;
    if current.len() != original.len() || current.modified().ok() != original.modified().ok() {
        return Err(t("程序文件已由另一个窗口更新，请重新打开 QCCOM。").into());
    }
    let old = exe.with_extension("qccom-update-backup");
    fs::rename(exe, &old).map_err(|e| tf("没有权限替换程序（{}）。可以把程序放到有写权限的文件夹再试", &[&e]))?;
    let rollback = |error: String| {
        if let Err(e) = fs::rename(&old, exe) {
            format!("{error}\n{}", tf("无法恢复旧程序，备份文件 {}：{}", &[&old.display(), &e]))
        } else {
            error
        }
    };
    fs::rename(new, exe).map_err(|e| rollback(tf("替换失败：{}", &[&e])))?;
    start().map_err(|e| rollback(tf("启动新版本失败：{}", &[&e])))
}

pub fn install(new: &Path) -> Result<(), String> {
    let (exe, original) = current_executable()?;
    install_into(exe, original, new, || {
        std::process::Command::new(exe)
            .arg("--updated")
            .arg(format!("--updated-slot={}", crate::instance()))
            .spawn()
            .map(|_| ())
    })
}

/// Clean up the prior executable once the replacement starts.
pub fn cleanup() {
    if let Ok((exe, _)) = current_executable() {
        let old = exe.with_extension("qccom-update-backup");
        if old.exists() {
            if let Ok(_lock) = lock_update(exe) {
                let _ = fs::remove_file(old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer("0.5.0", "0.4.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(!newer("0.4", "0.4.0"));
        assert!(!newer("0.3.9", "0.4.0"));
    }

    #[test]
    fn update_url_accepts_https_and_local_development() {
        assert!(download_url_ok(DEFAULT_EXE_URL));
        assert!(download_url_ok("http://127.0.0.1:8000/qccom.exe"));
        for url in ["file:///C:/setup.exe", "http://example.com/qccom.exe", "https://user@example.com/qccom.exe", "https://", ""] {
            assert!(!download_url_ok(url));
        }
    }

    #[test]
    fn update_rolls_back_if_relaunch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("qccom.exe");
        let new = dir.path().join("download");
        fs::write(&exe, b"original").unwrap();
        fs::write(&new, b"replacement").unwrap();
        let original = fs::metadata(&exe).unwrap();
        let result = install_into(&exe, &original, &new, || {
            assert_eq!(fs::read(&exe).unwrap(), b"replacement");
            Err(std::io::Error::other("could not launch"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"original");
    }

    #[test]
    fn stale_window_cannot_replace_a_newer_installation() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("qccom.exe");
        let new = dir.path().join("download");
        fs::write(&exe, b"original").unwrap();
        let original = fs::metadata(&exe).unwrap();
        fs::write(&exe, b"already updated").unwrap();
        fs::write(&new, b"stale download").unwrap();
        assert!(install_into(&exe, &original, &new, || panic!("stale update started")).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"already updated");
    }

    #[test]
    fn parse_release() {
        let r: Release = serde_json::from_str(r#"{"version":"0.5.1","notes":"x"}"#).unwrap();
        assert_eq!(r.version, "0.5.1");
        assert!(r.sha256.is_empty());
    }
}

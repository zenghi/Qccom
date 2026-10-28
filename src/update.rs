//! 自动更新：读服务器上的版本文件，有新版就提示，下载、校验后替换自己并重启。
//!
//! 版本文件格式（UTF-8 JSON）：
//! ```json
//! {
//!   "version": "0.5.1",
//!   "url": "https://www.zenghi.com/1.exe",
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
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use eframe::egui;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const VERSION_URL: &str = "https://www.zenghi.com/1.json";

/// 版本文件地址；环境变量 QCCOM_UPDATE_URL 可以临时指向测试服务器
pub fn version_url() -> String {
    std::env::var("QCCOM_UPDATE_URL").unwrap_or_else(|_| VERSION_URL.into())
}
pub const DEFAULT_EXE_URL: &str = "https://www.zenghi.com/1.exe";

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
    Ready(PathBuf),
    Failed(String),
}

fn agent() -> Result<ureq::Agent, String> {
    let b = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .try_proxy_from_env(true)
        .user_agent(concat!("QCCOM/", env!("CARGO_PKG_VERSION")));
    #[cfg(windows)]
    let b = b.tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new().map_err(|e| e.to_string())?));
    Ok(b.build())
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
                }
                Ok(newer(&r.version, env!("CARGO_PKG_VERSION")).then_some(r))
            })();
            let _ = tx.send(UpdEvt::Checked(r));
            ctx.request_repaint();
        })
        .ok();
}

/// 新文件先下载到 exe 旁边（同一个分区，替换时只需改名）；没有写权限就放到临时目录
fn staging_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let probe = dir.join(".qccom-write-test");
    if File::create(&probe).is_ok() {
        let _ = fs::remove_file(&probe);
        dir.join("qccom.update.tmp")
    } else {
        std::env::temp_dir().join("qccom.update.tmp")
    }
}

pub fn download(r: Release, tx: Sender<UpdEvt>, ctx: egui::Context) {
    thread::Builder::new()
        .name("update-dl".into())
        .spawn(move || {
            let res = (|| -> Result<PathBuf, String> {
                let resp = agent()?.get(r.url.trim()).call().map_err(http_err)?;
                let total = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(r.size);
                let path = staging_path();
                let mut f = File::create(&path).map_err(|e| tf("无法写入 {}：{}", &[&path.display(), &e]))?;
                let mut rd = resp.into_reader();
                let mut h = Sha256::new();
                let mut buf = vec![0u8; 64 * 1024];
                let mut got = 0u64;
                let mut head = Vec::new();
                loop {
                    let n = rd.read(&mut buf).map_err(|e| tf("下载中断：{}", &[&e]))?;
                    if n == 0 {
                        break;
                    }
                    if head.len() < 2 {
                        head.extend_from_slice(&buf[..n.min(2)]);
                    }
                    h.update(&buf[..n]);
                    f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                    got += n as u64;
                    let _ = tx.send(UpdEvt::Progress(got, total));
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                f.sync_all().ok();
                drop(f);
                let fail = |msg: String| {
                    let _ = fs::remove_file(&path);
                    Err(msg)
                };
                if total > 0 && got != total {
                    return fail(tf("文件不完整：{} / {} 字节", &[&got, &total]));
                }
                let magic: &[u8] = if cfg!(windows) { b"MZ" } else { b"\x7fE" };
                if !head.starts_with(magic) {
                    return fail(t("下载到的不是可执行程序（服务器可能返回了网页）").into());
                }
                let want = r.sha256.trim().to_ascii_lowercase();
                if !want.is_empty() {
                    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
                    if hex != want {
                        return fail(t("SHA-256 校验不一致，文件可能损坏或被篡改，已删除").into());
                    }
                }
                Ok(path)
            })();
            let _ = tx.send(match res {
                Ok(p) => UpdEvt::Ready(p),
                Err(e) => UpdEvt::Failed(e),
            });
            ctx.request_repaint();
        })
        .ok();
}

/// 用新文件替换正在运行的 exe 并启动新版本。成功后调用方应立即退出。
/// Windows 允许给运行中的 exe 改名，所以：当前 → .old，新文件 → 当前，下次启动时删掉 .old。
pub fn install(new: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = exe.with_extension("old");
    let _ = fs::remove_file(&old);
    fs::rename(&exe, &old).map_err(|e| tf("没有权限替换程序（{}）。可以把程序放到有写权限的文件夹再试", &[&e]))?;
    if let Err(e) = fs::rename(new, &exe).or_else(|_| fs::copy(new, &exe).map(|_| ())) {
        let _ = fs::rename(&old, &exe); // 还原
        return Err(tf("替换失败：{}", &[&e]));
    }
    let _ = fs::remove_file(new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
    }
    std::process::Command::new(&exe).arg("--updated").spawn().map_err(|e| tf("启动新版本失败：{}", &[&e]))?;
    Ok(())
}

/// 启动时清理上次更新留下的旧文件
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(exe.with_extension("old"));
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
    fn parse_release() {
        let r: Release = serde_json::from_str(r#"{"version":"0.5.1","notes":"x"}"#).unwrap();
        assert_eq!(r.version, "0.5.1");
        assert!(r.sha256.is_empty());
    }
}

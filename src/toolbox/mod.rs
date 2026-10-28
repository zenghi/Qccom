//! 工具箱的后台部分：TFTP / HTTP 文件服务器、Syslog、MQTT 服务器、Modbus 从站、局域网发现、Ping、
//! 数据转换、波特率 / CRC / 子网计算、取模、SLCAN、USB HID。
//! 界面在 app/toolbox.rs。

pub mod baud;
pub mod bitmap;
pub mod broker;
pub mod conv;
pub mod crc;
pub mod discover;
pub mod hid;
pub mod httpd;
pub mod mbslave;
pub mod netcalc;
pub mod ping;
pub mod slcan;
pub mod syslog;
pub mod tftp;

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 文件服务器的一条记录（一次传输 / 一个请求）
#[derive(Clone)]
pub struct Xfer {
    pub at: String,
    pub peer: String,
    pub file: String,
    /// true = 客户端上传
    pub upload: bool,
    pub bytes: u64,
    pub total: Option<u64>,
    /// None = 进行中；Some(Ok) 完成；Some(Err) 失败
    pub done: Option<Result<(), String>>,
    /// HTTP 状态码（TFTP 为 0）
    pub status: u16,
}

/// 后台服务共用的部分：停止标志和记录列表
#[derive(Clone, Default)]
pub struct Shared {
    pub stop: Arc<AtomicBool>,
    pub log: Arc<Mutex<Vec<Xfer>>>,
}

impl Shared {
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    /// 加一条记录，返回下标（之后用 update 改进度）
    pub fn push(&self, x: Xfer) -> usize {
        let mut l = self.log.lock().unwrap();
        if l.len() >= 500 {
            l.remove(0);
        }
        l.push(x);
        l.len() - 1
    }
    pub fn update(&self, i: usize, f: impl FnOnce(&mut Xfer)) {
        if let Some(x) = self.log.lock().unwrap().get_mut(i) {
            f(x);
        }
    }
}

pub fn now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// 把客户端给的相对路径接到根目录下；带 `..`、盘符、绝对路径的一律拒绝
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches(['/', '\\']);
    let mut out = root.to_path_buf();
    for c in Path::new(&rel.replace('\\', "/")).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// 本机局域网地址（不发包：UDP connect 只是选路由）
pub fn local_ipv4() -> Option<std::net::Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v) if !v.is_loopback() && !v.is_unspecified() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_is_safe() {
        let r = Path::new("/srv");
        assert_eq!(safe_join(r, "a/b.bin"), Some(PathBuf::from("/srv/a/b.bin")));
        assert_eq!(safe_join(r, "/a.bin"), Some(PathBuf::from("/srv/a.bin")));
        assert_eq!(safe_join(r, "../etc/passwd"), None);
        assert_eq!(safe_join(r, "a/../../x"), None);
        assert_eq!(safe_join(r, "a\\..\\..\\x"), None);
    }
}

//! v1.8 小工具：串口被谁占用（7.1）、设备插拔记录（7.2）、连接 / 录制时不让电脑睡眠（7.3）。

use std::io::Write;
use std::path::PathBuf;

use crate::i18n::tf;

// ---------------------------------------------------------------- 7.1 串口被谁占用

/// 串口打不开时补一句是谁占着。Linux 直接查 /proc/*/fd；Windows 查不到设备句柄，
/// 只能列出正在运行的常见串口软件，所以说“可能”。
pub fn busy_hint(port: &str, err: &str) -> String {
    let holders = holders(port);
    if !holders.is_empty() {
        return tf("{}（被 {} 占用）", &[&err, &holders.join("、")]);
    }
    let busy = ["busy", "denied", "拒绝访问", "占用", "in use"].iter().any(|k| err.to_lowercase().contains(k));
    if busy && cfg!(windows) {
        let cands = serial_apps();
        if !cands.is_empty() {
            return tf("{}（可能被这些程序占用：{}）", &[&err, &cands.join("、")]);
        }
    }
    err.to_string()
}

#[cfg(not(windows))]
fn holders(port: &str) -> Vec<String> {
    let Ok(target) = std::fs::canonicalize(port) else { return vec![] };
    let me = std::process::id().to_string();
    let mut out = vec![];
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let pid = e.file_name().to_string_lossy().to_string();
        if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == me {
            continue;
        }
        let hit = std::fs::read_dir(e.path().join("fd"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|l| l == target));
        if hit {
            let name = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
            out.push(format!("{} (PID {pid})", name.trim()));
        }
    }
    out
}

#[cfg(windows)]
fn holders(_port: &str) -> Vec<String> {
    vec![]
}

/// Windows：正在运行的常见串口软件（程序名 + PID）
#[cfg(windows)]
fn serial_apps() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const KNOWN: &[&str] = &[
        "putty",
        "kitty",
        "xshell",
        "securecrt",
        "mobaxterm",
        "ttermpro",
        "sscom",
        "sscom5",
        "sscom32",
        "uartassist",
        "comassistant",
        "commassistant",
        "xcom",
        "llcom",
        "cutecom",
        "realterm",
        "hterm",
        "coolterm",
        "arduino",
        "arduino ide",
        "uv4",
        "iaride",
        "stm32cubeide",
        "stm32cubeprogrammer",
        "flash_download_tool",
        "esptool",
        "serial port utility",
        "docklight",
        "accessport",
        "vofa+",
        "serialplot",
        "qcom",
        "atk-xcom",
        "mcuisp",
        "flymcu",
    ];
    let out = std::process::Command::new("tasklist").args(["/fo", "csv", "/nh"]).creation_flags(0x0800_0000).output();
    let Ok(o) = out else { return vec![] };
    let mut v = vec![];
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        let cols: Vec<&str> = l.split("\",\"").map(|s| s.trim_matches('"')).collect();
        let (Some(name), Some(pid)) = (cols.first(), cols.get(1)) else { continue };
        let stem = name.to_lowercase();
        let stem = stem.trim_end_matches(".exe");
        if KNOWN.iter().any(|k| stem == *k || stem.starts_with(k)) && !v.iter().any(|x: &String| x.starts_with(*name)) {
            v.push(format!("{name} (PID {pid})"));
        }
    }
    v
}

#[cfg(not(windows))]
fn serial_apps() -> Vec<String> {
    vec![]
}

// ---------------------------------------------------------------- 7.2 插拔记录

#[derive(Clone, Debug, PartialEq)]
pub struct PlugEvt {
    /// 本地时间 "2026-10-28 14:03:12"
    pub at: String,
    pub plugged: bool,
    pub port: String,
    /// 设备描述（带 VID:PID）
    pub desc: String,
}

const KEEP: usize = 500;

fn plug_file() -> PathBuf {
    crate::data_dir().join("plug-log.tsv")
}

pub fn load_plugs() -> Vec<PlugEvt> {
    let s = std::fs::read_to_string(plug_file()).unwrap_or_default();
    let mut v: Vec<PlugEvt> = s
        .lines()
        .filter_map(|l| {
            let c: Vec<&str> = l.splitn(4, '\t').collect();
            (c.len() == 4).then(|| PlugEvt {
                at: c[0].into(),
                plugged: c[1] == "+",
                port: c[2].into(),
                desc: c[3].into(),
            })
        })
        .collect();
    if v.len() > KEEP {
        v.drain(..v.len() - KEEP);
        save_all(&v);
    }
    v
}

fn save_all(v: &[PlugEvt]) {
    let s: String = v.iter().map(line).collect();
    let _ = std::fs::write(plug_file(), s);
}

fn line(e: &PlugEvt) -> String {
    format!("{}\t{}\t{}\t{}\n", e.at, if e.plugged { "+" } else { "-" }, e.port, e.desc.replace(['\t', '\n'], " "))
}

/// 追加一条记录（文件里也追加一行）
pub fn record(log: &mut Vec<PlugEvt>, plugged: bool, port: &str, desc: &str) {
    let e = PlugEvt {
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        plugged,
        port: port.into(),
        desc: desc.into(),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(plug_file()) {
        let _ = f.write_all(line(&e).as_bytes());
    }
    log.push(e);
    if log.len() > KEEP * 2 {
        log.drain(..log.len() - KEEP);
    }
}

/// 描述里的 VID:PID（"CP2102 (10c4:ea60)" → "10c4:ea60"）
pub fn vid_pid(desc: &str) -> Option<&str> {
    let s = desc.rfind('(')?;
    let inner = desc[s + 1..].strip_suffix(')')?;
    (inner.len() == 9 && inner.as_bytes()[4] == b':').then_some(inner)
}

pub fn clear_plugs(log: &mut Vec<PlugEvt>) {
    log.clear();
    let _ = std::fs::remove_file(plug_file());
}

// ---------------------------------------------------------------- 7.3 不休眠

/// 有连接或正在录制时阻止系统睡眠（屏幕可以照常关）。没有界面。
#[derive(Default)]
pub struct Awake {
    on: bool,
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
}

impl Awake {
    pub fn set(&mut self, want: bool) {
        if want == self.on {
            return;
        }
        self.on = want;
        self.apply(want);
    }

    #[cfg(windows)]
    fn apply(&mut self, want: bool) {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetThreadExecutionState(flags: u32) -> u32;
        }
        const ES_CONTINUOUS: u32 = 0x8000_0000;
        const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
        // UI 线程一直活着，设置一直有效；取消时只留 ES_CONTINUOUS
        unsafe {
            SetThreadExecutionState(if want { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
        }
    }

    #[cfg(not(windows))]
    fn apply(&mut self, want: bool) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if want {
            // systemd 系统用 systemd-inhibit 挂一个睡眠锁；没有就算了
            self.child = std::process::Command::new("systemd-inhibit")
                .args([
                    "--what=sleep:idle",
                    "--who=QCCOM",
                    &format!("--why={}", crate::i18n::t("正在连接设备或录制日志")),
                    "--mode=block",
                    "sleep",
                    "infinity",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok();
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        if self.on {
            self.apply(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_holder() {
        let p = std::env::temp_dir().join("qccom-holder-test");
        std::fs::write(&p, b"x").unwrap();
        let mut c = std::process::Command::new("sleep").arg("3").stdin(std::fs::File::open(&p).unwrap()).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let h = holders(p.to_str().unwrap());
        let hint = busy_hint(p.to_str().unwrap(), "Device or resource busy");
        let _ = c.kill();
        assert!(h.iter().any(|x| x.starts_with("sleep (PID")), "{h:?}");
        assert!(hint.contains("sleep"), "{hint}");
    }

    #[test]
    fn vidpid() {
        assert_eq!(vid_pid("CP2102 USB to UART (10c4:ea60)"), Some("10c4:ea60"));
        assert_eq!(vid_pid("PCI"), None);
    }
}

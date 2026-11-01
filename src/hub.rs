//! 和 qccom-hub 服务器通信：匿名设备身份、社区账号、使用统计上报。
//!
//! 服务器地址默认 https://www.zenghi.com/qccom/api/v1，环境变量 QCCOM_HUB_URL 可以指向测试服务器。
//! 设备 ID 和令牌、社区登录令牌存在数据目录的 hub.json（和设置分开，多窗口共用一份）。
//!
//! 统计只发：随机设备 ID、版本、系统、界面语言、使用时长、各页面/工具打开次数。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_URL: &str = "https://www.zenghi.com/qccom/api/v1";

pub fn base_url() -> String {
    std::env::var("QCCOM_HUB_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_URL.into())
        .trim_end_matches('/')
        .to_string()
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Creds {
    pub device_id: String,
    pub device_token: String,
    /// 社区登录令牌（空 = 没登录）
    pub session: String,
    /// 登录账号的缓存（显示用，启动后会用 /me 刷新）
    pub user: Option<Value>,
}

fn creds_path() -> std::path::PathBuf {
    crate::data_dir().join("hub.json")
}

fn store() -> &'static Mutex<Creds> {
    static S: OnceLock<Mutex<Creds>> = OnceLock::new();
    S.get_or_init(|| {
        let c = std::fs::read_to_string(creds_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Mutex::new(c)
    })
}

pub fn creds() -> Creds {
    store().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn update_creds(f: impl FnOnce(&mut Creds)) {
    let mut c = store().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut c);
    let _ = std::fs::create_dir_all(crate::data_dir());
    if let Ok(s) = serde_json::to_string_pretty(&*c) {
        let _ = std::fs::write(creds_path(), s);
    }
}

pub fn os_name() -> String {
    #[cfg(windows)]
    {
        // Windows 11 的内部版本号从 22000 起
        let build = windows_build();
        let name = match build {
            Some(b) if b >= 22000 => "Windows 11",
            Some(b) if b >= 10240 => "Windows 10",
            Some(b) if b >= 9200 => "Windows 8",
            Some(_) => "Windows 7",
            None => "Windows",
        };
        name.to_string()
    }
    #[cfg(not(windows))]
    {
        let rel = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let get = |k: &str| rel.lines().find_map(|l| l.strip_prefix(k)).map(|v| v.trim_matches('"').to_string());
        match (get("NAME="), get("VERSION_ID=")) {
            (Some(n), Some(v)) => format!("{} {v}", n.split_whitespace().next().unwrap_or(&n)),
            (Some(n), None) => n,
            _ => std::env::consts::OS.to_string(),
        }
    }
}

#[cfg(windows)]
fn windows_build() -> Option<u32> {
    let out = std::process::Command::new("cmd").args(["/c", "ver"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    // "Microsoft Windows [版本 10.0.22631.4317]"
    let v = s.split(|c: char| !(c.is_ascii_digit() || c == '.')).find(|p| p.matches('.').count() >= 2)?;
    v.split('.').nth(2)?.parse().ok()
}

fn os_cached() -> &'static str {
    static OS: OnceLock<String> = OnceLock::new();
    OS.get_or_init(os_name)
}

pub fn lang_code() -> &'static str {
    crate::i18n::LANGS[crate::i18n::current()].0
}

// ---------------------------------------------------------------- HTTP

#[derive(Debug, Clone)]
pub struct HubErr {
    /// 服务器给的错误代码（rate_limited / need_login / …），网络错误为 "network"
    pub code: String,
    pub msg: String,
}

impl std::fmt::Display for HubErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

pub fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        // 本机测试服务器不走系统代理（ureq 2 不认 NO_PROXY）
        let url = base_url();
        if url.contains("://127.0.0.1") || url.contains("://localhost") {
            return ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(Duration::from_secs(30)).build();
        }
        crate::update::agent().unwrap_or_else(|_| ureq::Agent::new())
    })
}

/// 发一个请求（阻塞，在后台线程里用）。body：None / Json / Bytes
pub enum Body {
    None,
    Json(Value),
    Bytes(Vec<u8>),
}

fn net_err(e: ureq::Error) -> HubErr {
    match e {
        ureq::Error::Status(code, r) => {
            let v: Value = serde_json::from_reader(r.into_reader()).unwrap_or(Value::Null);
            HubErr {
                code: v["error"].as_str().unwrap_or("http").to_string(),
                msg: v["message"].as_str().map(str::to_string).unwrap_or_else(|| match code {
                    404 => crate::i18n::tf("服务器返回 {}（社区地址或反向代理路径可能不对）", &[&code]),
                    502 | 503 | 504 => crate::i18n::tf("服务器返回 {}（qccom-hub 服务可能没在运行）", &[&code]),
                    _ => crate::i18n::tf("服务器返回 {}", &[&code]),
                }),
            }
        }
        ureq::Error::Transport(t) => HubErr {
            code: "network".into(),
            msg: crate::i18n::tf("连不上社区服务器：{}", &[&t]),
        },
    }
}

pub fn request(method: &str, path: &str, body: Body) -> Result<Value, HubErr> {
    let c = creds();
    let mut req = agent().request(method, &format!("{}{path}", base_url()));
    if !c.device_id.is_empty() {
        req = req.set("X-QC-Device", &c.device_id).set("X-QC-Token", &c.device_token);
    }
    if !c.session.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", c.session));
    }
    let r = match body {
        Body::None => req.call(),
        Body::Json(v) => req.set("Content-Type", "application/json").send_string(&v.to_string()),
        Body::Bytes(b) => req.set("Content-Type", "application/octet-stream").send_bytes(&b),
    };
    let out = match r {
        Ok(r) => {
            let ct = r.content_type().to_string();
            serde_json::from_reader::<_, Value>(r.into_reader()).map_err(|e| HubErr {
                code: "bad_reply".into(),
                msg: if ct.contains("html") {
                    crate::i18n::t("服务器返回了网页而不是数据，社区地址或反向代理配置可能不对").to_string()
                } else {
                    format!("{}: {e}", crate::i18n::t("服务器返回的数据无法解析"))
                },
            })
        }
        Err(e) => {
            let e = net_err(e);
            // 登录过期：清掉，界面回到未登录
            if e.code == "need_login" && !c.session.is_empty() && !path.starts_with("/auth/") {
                update_creds(|c| {
                    c.session.clear();
                    c.user = None;
                });
            }
            Err(e)
        }
    };
    match &out {
        Err(e) if matches!(e.code.as_str(), "network" | "http" | "bad_reply" | "internal" | "db") => note_error(method, path, &e.msg),
        _ => {}
    }
    out
}

static LAST_ERR: Mutex<Option<(i64, String)>> = Mutex::new(None);

/// 记下最近一次出错（设置页里显示，方便排查）
pub fn note_error(method: &str, path: &str, msg: &str) {
    let path = path.split('?').next().unwrap_or(path);
    *LAST_ERR.lock().unwrap() = Some((now_secs(), format!("{method} {path} · {msg}")));
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn last_error() -> Option<(i64, String)> {
    LAST_ERR.lock().unwrap().clone()
}

/// 没有设备身份就注册一个（发帖、投票、统计都要）
pub fn ensure_device() -> Result<(), HubErr> {
    if !creds().device_id.is_empty() {
        return Ok(());
    }
    let v = request(
        "POST",
        "/device/register",
        Body::Json(json!({ "version": env!("CARGO_PKG_VERSION"), "os": os_cached(), "lang": lang_code() })),
    )?;
    let (id, tok) = (v["device_id"].as_str().unwrap_or_default().to_string(), v["token"].as_str().unwrap_or_default().to_string());
    if id.is_empty() {
        return Err(HubErr {
            code: "bad".into(),
            msg: "register failed".into(),
        });
    }
    update_creds(|c| {
        c.device_id = id;
        c.device_token = tok;
    });
    Ok(())
}

// ---------------------------------------------------------------- 使用统计

static ENABLED: AtomicBool = AtomicBool::new(true);

fn counters() -> &'static Mutex<BTreeMap<String, i64>> {
    static C: OnceLock<Mutex<BTreeMap<String, i64>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 记一次「打开了某个页面 / 工具」。名字用英文短词，比如 serial、code、tool.modbus
pub fn count(feature: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    *counters().lock().unwrap_or_else(|e| e.into_inner()).entry(feature.to_string()).or_insert(0) += 1;
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on && std::env::var_os("QCCOM_NO_TELEMETRY").is_none(), Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 启动统计线程：30 秒后发第一次，之后每 6 小时一次；关闭开关后什么都不发
pub fn start_telemetry() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = last_beat();
    let first = std::env::var("QCCOM_HUB_FIRST_BEAT").ok().and_then(|s| s.parse().ok()).unwrap_or(30u64);
    std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(first));
            loop {
                if enabled() {
                    let _ = beat();
                }
                std::thread::sleep(Duration::from_secs(6 * 3600));
            }
        })
        .ok();
}

/// 上次成功上报的时间（统计「使用时长」用）
fn last_beat() -> &'static Mutex<Instant> {
    static L: OnceLock<Mutex<Instant>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Instant::now()))
}

/// 退出前补发一次本次的使用时长（最多等 2 秒）
pub fn final_beat() {
    if !enabled() || creds().device_id.is_empty() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(beat());
    });
    let _ = rx.recv_timeout(Duration::from_secs(2));
}

fn beat() -> Result<(), HubErr> {
    ensure_device()?;
    let secs = last_beat().lock().unwrap_or_else(|e| e.into_inner()).elapsed().as_secs();
    let feats: BTreeMap<String, i64> = std::mem::take(&mut *counters().lock().unwrap_or_else(|e| e.into_inner()));
    let r = request(
        "POST",
        "/heartbeat",
        Body::Json(json!({
            "version": env!("CARGO_PKG_VERSION"), "os": os_cached(), "lang": lang_code(),
            "secs": secs, "features": feats,
        })),
    );
    if r.is_err() {
        // 没发出去：计数放回去，下次一起发
        let mut c = counters().lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in feats {
            *c.entry(k).or_insert(0) += v;
        }
    }
    if r.is_ok() {
        *last_beat().lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
    r.map(|_| ())
}

/// 设置页「收集了什么」里显示的说明
pub fn what_we_send() -> String {
    let c = creds();
    let id = if c.device_id.is_empty() { "—".to_string() } else { c.device_id.clone() };
    format!(
        "device_id: {id}\nversion: {}\nos: {}\nlang: {}\nsecs: …\nfeatures: {{ serial: 3, code: 1, … }}",
        env!("CARGO_PKG_VERSION"),
        os_cached(),
        lang_code()
    )
}

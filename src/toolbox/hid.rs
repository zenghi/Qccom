//! USB HID 调试：列出 HID 设备，打开后接收输入报告，发送输出报告、读写特征报告。
//! 自定义 HID 的单片机（免驱通信）调试时用。

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use hidapi::{HidApi, HidDevice};

use crate::i18n::{t, tf};

#[derive(Clone)]
pub struct Info {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub serial: String,
    pub usage_page: u16,
    pub usage: u16,
    pub interface: i32,
}

impl Info {
    pub fn title(&self) -> String {
        let p = if self.product.is_empty() { t("（未知设备）").to_string() } else { self.product.clone() };
        if self.manufacturer.is_empty() {
            p
        } else {
            format!("{p} · {}", self.manufacturer)
        }
    }
    /// 键盘、鼠标这类系统独占的设备（打不开或读不到）
    pub fn is_system(&self) -> bool {
        self.usage_page == 0x01 && matches!(self.usage, 0x02 | 0x06)
    }
}

pub struct Entry {
    pub t: f64,
    /// 'I' 输入报告，'O' 输出报告，'F' 特征报告（读到的），'f' 特征报告（写的）
    pub kind: char,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct State {
    pub log: Vec<Entry>,
    pub err: Option<String>,
    pub rx: u64,
    pub tx: u64,
}

pub type Shared = Arc<Mutex<State>>;

pub struct Hid {
    api: Option<HidApi>,
    pub err: Option<String>,
}

impl Hid {
    pub fn new() -> Hid {
        match HidApi::new() {
            Ok(api) => Hid { api: Some(api), err: None },
            Err(e) => Hid { api: None, err: Some(e.to_string()) },
        }
    }

    pub fn list(&mut self) -> Vec<Info> {
        let Some(api) = self.api.as_mut() else { return vec![] };
        if let Err(e) = api.refresh_devices() {
            self.err = Some(e.to_string());
        }
        let mut v: Vec<Info> = api
            .device_list()
            .map(|d| Info {
                path: d.path().to_string_lossy().into_owned(),
                vid: d.vendor_id(),
                pid: d.product_id(),
                manufacturer: d.manufacturer_string().unwrap_or("").trim().to_string(),
                product: d.product_string().unwrap_or("").trim().to_string(),
                serial: d.serial_number().unwrap_or("").trim().to_string(),
                usage_page: d.usage_page(),
                usage: d.usage(),
                interface: d.interface_number(),
            })
            .collect();
        // 同一个物理接口在 Windows 上会按 usage 拆成好几项，这里保留全部，按 VID:PID 排序
        v.sort_by(|a, b| (a.is_system(), a.vid, a.pid, a.interface).cmp(&(b.is_system(), b.vid, b.pid, b.interface)));
        v
    }

    pub fn open(&self, info: &Info, st: &Shared, ctx: egui::Context) -> Result<Dev, String> {
        let api = self.api.as_ref().ok_or_else(|| self.err.clone().unwrap_or_default())?;
        let path = CString::new(info.path.clone()).map_err(|e| e.to_string())?;
        let dev = api.open_path(&path).map_err(|e| {
            if cfg!(target_os = "linux") {
                tf("打开失败：{}（Linux 上可能要给 /dev/hidraw* 加 udev 权限）", &[&e])
            } else {
                tf("打开失败：{}", &[&e])
            }
        })?;
        *st.lock().unwrap() = State::default();
        let dev = Arc::new(Mutex::new(dev));
        let stop = Arc::new(AtomicBool::new(false));
        let (d2, s2, st2) = (dev.clone(), stop.clone(), st.clone());
        let t0 = Instant::now();
        std::thread::Builder::new()
            .name("hid-read".into())
            .spawn(move || {
                let mut buf = [0u8; 1024];
                while !s2.load(Ordering::Relaxed) {
                    let r = d2.lock().unwrap().read_timeout(&mut buf, 20);
                    match r {
                        Ok(0) => std::thread::sleep(Duration::from_millis(2)),
                        Ok(n) => {
                            push(&st2, t0, 'I', buf[..n].to_vec());
                            ctx.request_repaint_after(Duration::from_millis(33));
                        }
                        Err(e) => {
                            st2.lock().unwrap().err = Some(tf("设备断开：{}", &[&e]));
                            ctx.request_repaint();
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Dev { dev, stop, t0, st: st.clone() })
    }
}

fn push(st: &Shared, t0: Instant, kind: char, data: Vec<u8>) {
    let mut s = st.lock().unwrap();
    if s.log.len() >= 10000 {
        s.log.drain(..2000);
    }
    if kind == 'I' || kind == 'F' {
        s.rx += 1;
    } else {
        s.tx += 1;
    }
    s.log.push(Entry {
        t: t0.elapsed().as_secs_f64(),
        kind,
        data,
    });
}

pub struct Dev {
    dev: Arc<Mutex<HidDevice>>,
    stop: Arc<AtomicBool>,
    t0: Instant,
    st: Shared,
}

impl Drop for Dev {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Dev {
    /// 发送输出报告；data 第一个字节是报告 ID（没有编号的设备用 0）
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        self.dev.lock().unwrap().write(data).map_err(|e| e.to_string())?;
        push(&self.st, self.t0, 'O', data.to_vec());
        Ok(())
    }
    pub fn set_feature(&self, data: &[u8]) -> Result<(), String> {
        self.dev.lock().unwrap().send_feature_report(data).map_err(|e| e.to_string())?;
        push(&self.st, self.t0, 'f', data.to_vec());
        Ok(())
    }
    pub fn get_feature(&self, id: u8, len: usize) -> Result<(), String> {
        let mut buf = vec![0u8; len.clamp(2, 4096)];
        buf[0] = id;
        let n = self.dev.lock().unwrap().get_feature_report(&mut buf).map_err(|e| e.to_string())?;
        push(&self.st, self.t0, 'F', buf[..n].to_vec());
        Ok(())
    }
}

//! 串口后台线程：端口扫描、收发、自动波特率识别。
//! UI 线程只通过 channel 和这些线程通信，界面永远不会被 IO 卡住。

use crate::i18n::{t, tf};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPortType, StopBits};

pub const COMMON_BAUDS: &[u32] = &[
    1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 74880, 115200, 230400, 256000, 460800, 500000, 921600, 1000000, 1500000, 2000000, 3000000,
];

/// 自动识别时的尝试顺序：最常见的放前面，命中高置信度可提前结束。
const AUTOBAUD_ORDER: &[u32] = &[
    115200, 9600, 921600, 57600, 460800, 230400, 38400, 19200, 1500000, 2000000, 74880, 4800, 1000000, 500000, 256000, 3000000, 2400, 1200,
];

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PortConfig {
    pub baud: u32,
    pub data_bits: u8,
    pub parity: u8, // 0 无 1 奇 2 偶
    pub stop_bits: u8,
    pub flow: u8, // 0 无 1 软件 2 硬件
}

impl Default for PortConfig {
    fn default() -> Self {
        Self {
            baud: 115200,
            data_bits: 8,
            parity: 0,
            stop_bits: 1,
            flow: 0,
        }
    }
}

impl PortConfig {
    pub fn short(&self) -> String {
        let p = ["N", "O", "E"][self.parity.min(2) as usize];
        format!("{}{}{}", self.data_bits, p, self.stop_bits)
    }

    fn builder(&self, name: &str) -> serialport::SerialPortBuilder {
        serialport::new(name, self.baud)
            .data_bits(match self.data_bits {
                5 => DataBits::Five,
                6 => DataBits::Six,
                7 => DataBits::Seven,
                _ => DataBits::Eight,
            })
            .parity(match self.parity {
                1 => Parity::Odd,
                2 => Parity::Even,
                _ => Parity::None,
            })
            .stop_bits(if self.stop_bits == 2 { StopBits::Two } else { StopBits::One })
            .flow_control(match self.flow {
                1 => FlowControl::Software,
                2 => FlowControl::Hardware,
                _ => FlowControl::None,
            })
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct PortEntry {
    pub name: String,
    /// 设备描述，比如 "CH340 (1a86:7523)"
    pub desc: String,
}

pub enum Cmd {
    Send(Vec<u8>),
    SetDtr(bool),
    SetRts(bool),
    /// MQTT：订阅 / 取消订阅 / 修改发布主题、QoS、保留标志
    Subscribe(String),
    Unsubscribe(String),
    PubOptions(String, u8, bool),
    /// 发送 Break 信号，持续毫秒数
    Break(u32),
    /// 依次设置 DTR / RTS 并保持一段时间（复位、进下载模式这类时序）；None = 不变
    Pulse(Vec<(Option<bool>, Option<bool>, u32)>),
    Close,
}

pub enum Evt {
    Ports(Vec<PortEntry>),
    /// (连接代号, 端口名)
    Opened(u64, String),
    Data(Vec<u8>),
    /// 一整包数据，带来源标签（MQTT 主题、UDP 对方地址）
    Packet(String, Vec<u8>),
    /// 网络连接的状态提示（客户端上线、订阅成功等）
    Sys(String),
    /// TCP 服务器当前连接的客户端数
    Peers(usize),
    /// None = 用户主动关闭；Some(err) = 出错/设备拔出
    Closed(u64, Option<String>),
    /// 输入信号线状态变化：(连接代号, [CTS, DSR, RI, CD])
    Signals(u64, [bool; 4]),
    /// 双口监听时第二个串口（B）收到的数据
    DataB(Vec<u8>),
    AutoBaudProgress(u32),
    AutoBaudDone(Result<(u32, f32), String>),
}

/// 每秒扫描一次端口，列表有变化才通知 UI（热插拔）。
pub fn spawn_port_scanner(tx: Sender<Evt>, ctx: egui::Context) {
    thread::Builder::new()
        .name("port-scan".into())
        .spawn(move || {
            let mut last: Option<Vec<PortEntry>> = None;
            loop {
                let list = scan_ports();
                if last.as_ref() != Some(&list) {
                    if tx.send(Evt::Ports(list.clone())).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                    last = Some(list);
                }
                thread::sleep(Duration::from_millis(1000));
            }
        })
        .ok();
}

fn scan_ports() -> Vec<PortEntry> {
    let mut out: Vec<PortEntry> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            // Linux 下 /dev/ttyS* 往往有几十个空壳端口，只保留真实设备
            !(cfg!(target_os = "linux") && p.port_name.starts_with("/dev/ttyS") && matches!(p.port_type, SerialPortType::Unknown))
        })
        .map(|p| {
            let desc = match &p.port_type {
                SerialPortType::UsbPort(u) => {
                    let name = u.product.clone().or_else(|| u.manufacturer.clone()).unwrap_or_else(|| "USB".into());
                    format!("{} ({:04x}:{:04x})", name, u.vid, u.pid)
                }
                SerialPortType::BluetoothPort => t("蓝牙").into(),
                SerialPortType::PciPort => "PCI".into(),
                SerialPortType::Unknown => String::new(),
            };
            PortEntry { name: p.port_name, desc }
        })
        .collect();
    // COM3 排在 COM10 前面
    out.sort_by(|a, b| natural_key(&a.name).cmp(&natural_key(&b.name)));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn natural_key(s: &str) -> (String, u64) {
    let digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let n: u64 = digits.chars().rev().collect::<String>().parse().unwrap_or(0);
    (s[..s.len() - digits.len()].to_string(), n)
}

pub struct Connection {
    pub cmd: Sender<Cmd>,
}

/// 打开串口并启动收发线程。打开失败直接返回错误。
pub fn open(gen: u64, name: &str, cfg: PortConfig, dtr: bool, rts: bool, tx: Sender<Evt>, ctx: egui::Context) -> Result<Connection, String> {
    open_side(gen, name, cfg, dtr, rts, false, tx, ctx)
}

/// side_b = 双口监听的第二个串口：数据用 DataB 送出
#[allow(clippy::too_many_arguments)]
pub fn open_side(gen: u64, name: &str, cfg: PortConfig, dtr: bool, rts: bool, side_b: bool, tx: Sender<Evt>, ctx: egui::Context) -> Result<Connection, String> {
    let mut port = cfg.builder(name).timeout(Duration::from_millis(20)).open().map_err(|e| e.to_string())?;
    let _ = port.write_data_terminal_ready(dtr);
    let _ = port.write_request_to_send(rts);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let name = name.to_string();
    thread::Builder::new()
        .name("serial-io".into())
        .spawn(move || io_loop(gen, port, name, side_b, cmd_rx, tx, ctx))
        .map_err(|e| e.to_string())?;
    Ok(Connection { cmd: cmd_tx })
}

fn read_signals(port: &mut Box<dyn serialport::SerialPort>) -> [bool; 4] {
    [
        port.read_clear_to_send().unwrap_or(false),
        port.read_data_set_ready().unwrap_or(false),
        port.read_ring_indicator().unwrap_or(false),
        port.read_carrier_detect().unwrap_or(false),
    ]
}

fn io_loop(gen: u64, mut port: Box<dyn serialport::SerialPort>, name: String, side_b: bool, cmd_rx: Receiver<Cmd>, tx: Sender<Evt>, ctx: egui::Context) {
    let _ = tx.send(Evt::Opened(gen, name));
    ctx.request_repaint();
    let mut buf = vec![0u8; 16 * 1024];
    let mut signals: Option<[bool; 4]> = None;
    let mut sig_at = Instant::now() - Duration::from_secs(1);
    let reason = loop {
        // 输入信号线每 250 ms 看一次，有变化才通知
        if !side_b && sig_at.elapsed() >= Duration::from_millis(250) {
            sig_at = Instant::now();
            let now = read_signals(&mut port);
            if signals != Some(now) {
                signals = Some(now);
                let _ = tx.send(Evt::Signals(gen, now));
                ctx.request_repaint();
            }
        }
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                let d = buf[..n].to_vec();
                if tx.send(if side_b { Evt::DataB(d) } else { Evt::Data(d) }).is_err() {
                    break None;
                }
                // 合并重绘：最多约 30 FPS，数据再快界面也不会把 CPU 吃满
                ctx.request_repaint_after(Duration::from_millis(33));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => break Some(e.to_string()),
        }
        loop {
            match cmd_rx.try_recv() {
                Ok(Cmd::Send(data)) => {
                    if let Err(e) = port.write_all(&data).and_then(|_| port.flush()) {
                        // 写超时不算断开，其他错误算
                        if e.kind() != std::io::ErrorKind::TimedOut {
                            let _ = tx.send(Evt::Closed(gen, Some(e.to_string())));
                            ctx.request_repaint();
                            return;
                        }
                    }
                }
                Ok(Cmd::SetDtr(v)) => {
                    let _ = port.write_data_terminal_ready(v);
                }
                Ok(Cmd::SetRts(v)) => {
                    let _ = port.write_request_to_send(v);
                }
                Ok(Cmd::Break(ms)) => {
                    let _ = port.set_break();
                    thread::sleep(Duration::from_millis(ms.clamp(1, 5000) as u64));
                    let _ = port.clear_break();
                }
                Ok(Cmd::Pulse(steps)) => {
                    for (dtr, rts, ms) in steps {
                        if let Some(v) = dtr {
                            let _ = port.write_data_terminal_ready(v);
                        }
                        if let Some(v) = rts {
                            let _ = port.write_request_to_send(v);
                        }
                        thread::sleep(Duration::from_millis(ms.min(5000) as u64));
                    }
                }
                Ok(Cmd::Subscribe(_) | Cmd::Unsubscribe(_) | Cmd::PubOptions(..)) => {}
                Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = tx.send(Evt::Closed(gen, None));
                    ctx.request_repaint();
                    return;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
    };
    let _ = tx.send(Evt::Closed(gen, reason));
    ctx.request_repaint();
}

/// 自动识别波特率：依次用候选波特率监听一小段时间，给收到的数据“可读性”打分，取最高分。
/// 需要设备在此期间持续有输出（大多数设备上电/运行时都会打日志）。
pub fn spawn_autobaud(name: String, cfg: PortConfig, tx: Sender<Evt>, ctx: egui::Context) {
    thread::Builder::new()
        .name("autobaud".into())
        .spawn(move || {
            let r = autobaud(&name, cfg, &tx, &ctx);
            let _ = tx.send(Evt::AutoBaudDone(r));
            ctx.request_repaint();
        })
        .ok();
}

fn autobaud(name: &str, cfg: PortConfig, tx: &Sender<Evt>, ctx: &egui::Context) -> Result<(u32, f32), String> {
    let mut best: Option<(u32, f32, usize)> = None;
    let mut any_data = false;
    for &baud in AUTOBAUD_ORDER {
        let _ = tx.send(Evt::AutoBaudProgress(baud));
        ctx.request_repaint();
        let mut c = cfg;
        c.baud = baud;
        // 刚断开的连接可能还没完全释放端口，失败时稍等重试
        let mut tries = 0;
        let mut port = loop {
            match c.builder(name).timeout(Duration::from_millis(30)).open() {
                Ok(p) => break p,
                Err(_) if tries < 10 => {
                    tries += 1;
                    thread::sleep(Duration::from_millis(60));
                }
                Err(e) => return Err(tf("无法打开 {}: {}", &[&name, &e])),
            }
        };
        let _ = port.clear(serialport::ClearBuffer::Input);
        let mut data = Vec::with_capacity(1024);
        let mut buf = [0u8; 1024];
        let start = Instant::now();
        // 丢掉切换瞬间的前几个字节，避免半个字节造成误判
        let mut skipped = false;
        while start.elapsed() < Duration::from_millis(350) && data.len() < 512 {
            if let Ok(n) = port.read(&mut buf) {
                if n > 0 {
                    if !skipped && n > 2 {
                        data.extend_from_slice(&buf[2..n]);
                    } else if skipped {
                        data.extend_from_slice(&buf[..n]);
                    }
                    skipped = true;
                }
            }
        }
        drop(port);
        if data.is_empty() {
            continue;
        }
        any_data = true;
        let s = score(&data);
        if best.map_or(true, |(_, bs, bl)| s > bs || (s == bs && data.len() > bl)) {
            best = Some((baud, s, data.len()));
        }
        if s >= 0.97 && data.len() >= 32 {
            break; // 高置信度，提前结束
        }
    }
    match best {
        Some((baud, s, len)) if s >= 0.80 && len >= 6 => Ok((baud, s)),
        Some((baud, s, _)) => Err(tf("数据可读性太低（最佳 {}，置信度 {}%），可能是二进制协议或非常用波特率", &[&baud, &format!("{:.0}", s * 100.0)])),
        None if !any_data => Err(t("识别期间没有收到任何数据，请让设备输出日志（如复位）后重试").into()),
        None => Err(t("识别失败").into()),
    }
}

/// 可读性评分 0..1：可打印 ASCII、常见控制符、合法 UTF-8/GBK 多字节都算“好字节”。
pub fn score(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let good_char = |c: char| {
        if c.is_ascii() {
            matches!(c as u8, 0x20..=0x7e | b'\r' | b'\n' | b'\t' | 0x1b)
        } else {
            !c.is_control() && c != '\u{fffd}'
        }
    };
    let ascii_good = |b: u8| matches!(b, 0x20..=0x7e | b'\r' | b'\n' | b'\t' | 0x1b);
    // UTF-8 视角
    let mut good = 0usize;
    let mut rest = data;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                good += s.chars().filter(|c| good_char(*c)).map(|c| c.len_utf8()).sum::<usize>();
                break;
            }
            Err(e) => {
                let (ok, bad) = rest.split_at(e.valid_up_to());
                let s = std::str::from_utf8(ok).unwrap_or("");
                good += s.chars().filter(|c| good_char(*c)).map(|c| c.len_utf8()).sum::<usize>();
                let skip = e.error_len().unwrap_or(bad.len()).max(1);
                rest = &bad[skip.min(bad.len())..];
            }
        }
    }
    let utf8 = good as f32 / data.len() as f32;
    // GBK 视角
    let mut gbk_good = 0usize;
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if ascii_good(b) {
            gbk_good += 1;
            i += 1;
        } else if (0xa1..=0xf7).contains(&b) && i + 1 < data.len() && (0xa1..=0xfe).contains(&data[i + 1]) {
            // 只认 GB2312 区（常用汉字和全角符号），随机字节很难凑出来
            gbk_good += 2;
            i += 2;
        } else {
            i += 1;
        }
    }
    let gbk = gbk_good as f32 / data.len() as f32;
    utf8.max(gbk * 0.95)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_text_scores_high() {
        assert!(score(b"I (123) wifi: connected\r\n[INFO] boot ok\r\n") > 0.99);
        assert!(score("温度=25.3℃ 正常\r\n".as_bytes()) > 0.99);
    }

    #[test]
    fn garbage_scores_low() {
        let g: Vec<u8> = (0..200u32).map(|i| (i.wrapping_mul(97).wrapping_add(13) % 256) as u8 | 0x80).collect();
        assert!(score(&g) < 0.8, "{}", score(&g));
        assert!(score(&[0u8; 64]) < 0.1);
        assert!(score(&[0xffu8, 0xfe, 0x00, 0xf0, 0x80, 0x00, 0xff, 0xe0].repeat(10)) < 0.5);
    }

    #[test]
    fn natural_sort() {
        assert!(natural_key("COM3") < natural_key("COM10"));
    }
}

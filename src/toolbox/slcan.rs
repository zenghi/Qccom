//! CAN 调试：通过 SLCAN（Lawicel）协议的 USB-CAN 适配器收发 CAN 帧。
//! CANable、USBtin、很多国产 USB-CAN 模块都支持这个串口协议。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::i18n::tf;

/// S0~S8 对应的位速率
pub const BITRATES: &[(u32, &str)] = &[
    (10_000, "S0"),
    (20_000, "S1"),
    (50_000, "S2"),
    (100_000, "S3"),
    (125_000, "S4"),
    (250_000, "S5"),
    (500_000, "S6"),
    (800_000, "S7"),
    (1_000_000, "S8"),
];

#[derive(Clone, PartialEq, Debug)]
pub struct Frame {
    pub id: u32,
    pub ext: bool,
    pub rtr: bool,
    pub data: Vec<u8>,
    pub dlc: u8,
}

impl Frame {
    /// 编码成 SLCAN 发送命令（不含 \r）
    pub fn encode(&self) -> String {
        let mut s = String::new();
        match (self.ext, self.rtr) {
            (false, false) => s.push_str(&format!("t{:03X}", self.id & 0x7FF)),
            (true, false) => s.push_str(&format!("T{:08X}", self.id & 0x1FFF_FFFF)),
            (false, true) => s.push_str(&format!("r{:03X}", self.id & 0x7FF)),
            (true, true) => s.push_str(&format!("R{:08X}", self.id & 0x1FFF_FFFF)),
        }
        let dlc = if self.rtr { self.dlc.min(8) } else { self.data.len().min(8) as u8 };
        s.push_str(&dlc.to_string());
        if !self.rtr {
            for b in self.data.iter().take(8) {
                s.push_str(&format!("{b:02X}"));
            }
        }
        s
    }

    /// 解析一行 SLCAN 收到的帧（可能带 4 位时间戳）
    pub fn decode(line: &str) -> Option<Frame> {
        let kind = line.chars().next()?;
        let (ext, rtr) = match kind {
            't' => (false, false),
            'T' => (true, false),
            'r' => (false, true),
            'R' => (true, true),
            _ => return None,
        };
        let idlen = if ext { 8 } else { 3 };
        let id = u32::from_str_radix(line.get(1..1 + idlen)?, 16).ok()?;
        let dlc = line.get(1 + idlen..2 + idlen)?.parse::<u8>().ok()?.min(8);
        let mut data = vec![];
        if !rtr {
            let d = line.get(2 + idlen..2 + idlen + dlc as usize * 2)?;
            for i in 0..dlc as usize {
                data.push(u8::from_str_radix(&d[i * 2..i * 2 + 2], 16).ok()?);
            }
        }
        Some(Frame { id, ext, rtr, data, dlc })
    }

    pub fn id_str(&self) -> String {
        if self.ext {
            format!("{:08X}", self.id)
        } else {
            format!("{:03X}", self.id)
        }
    }
}

pub struct TraceItem {
    pub t: f64,
    pub tx: bool,
    pub f: Frame,
}

/// 按 ID 汇总
pub struct IdStat {
    pub last: Frame,
    pub count: u64,
    pub period_ms: Option<f64>,
    pub last_at: Instant,
    /// 每个字节最后一次变化的时间（用来高亮刚变的字节）
    pub changed: [Option<Instant>; 8],
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    pub err: Option<String>,
    pub trace: Vec<TraceItem>,
    pub ids: BTreeMap<(bool, u32), IdStat>,
    pub rx: u64,
    pub tx: u64,
    pub errors: u64,
}

pub type Shared = Arc<Mutex<State>>;

pub enum Cmd {
    Send(Frame),
    Close,
}

pub struct Link {
    pub tx: Sender<Cmd>,
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Close);
    }
}

fn record(st: &Shared, f: Frame, tx: bool, t0: Instant) {
    let mut s = st.lock().unwrap();
    let now = Instant::now();
    if s.trace.len() >= 20000 {
        s.trace.drain(..5000);
    }
    s.trace.push(TraceItem {
        t: t0.elapsed().as_secs_f64(),
        tx,
        f: f.clone(),
    });
    if tx {
        s.tx += 1;
        return;
    }
    s.rx += 1;
    match s.ids.get_mut(&(f.ext, f.id)) {
        Some(x) => {
            let dt = now.duration_since(x.last_at).as_secs_f64() * 1000.0;
            // 周期取平滑值，偶发的抖动不至于让数字乱跳
            x.period_ms = Some(match x.period_ms {
                Some(p) => p * 0.8 + dt * 0.2,
                None => dt,
            });
            for i in 0..8 {
                if x.last.data.get(i) != f.data.get(i) {
                    x.changed[i] = Some(now);
                }
            }
            x.count += 1;
            x.last_at = now;
            x.last = f;
        }
        None => {
            s.ids.insert(
                (f.ext, f.id),
                IdStat {
                    last: f,
                    count: 1,
                    period_ms: None,
                    last_at: now,
                    changed: [None; 8],
                },
            );
        }
    }
}

/// 打开适配器：设位速率、打开通道（listen_only = 只听不应答）
pub fn open(port: &str, serial_baud: u32, bitrate: u32, listen_only: bool, st: &Shared, ctx: egui::Context) -> Result<Link, String> {
    let mut p = serialport::new(port, serial_baud).timeout(Duration::from_millis(20)).open().map_err(|e| e.to_string())?;
    let sx = BITRATES.iter().find(|(b, _)| *b == bitrate).map(|(_, s)| *s).unwrap_or("S6");
    // 先关一次（适配器可能还开着），再设速率、打开
    for c in ["\r\r\r", "C\r", &format!("{sx}\r"), if listen_only { "L\r" } else { "O\r" }] {
        p.write_all(c.as_bytes()).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(30));
    }
    let mut junk = [0u8; 256];
    let _ = p.read(&mut junk);
    {
        let mut s = st.lock().unwrap();
        *s = State { open: true, ..Default::default() };
    }
    let (tx, rx) = channel();
    let st = st.clone();
    std::thread::Builder::new().name("slcan".into()).spawn(move || run(p, rx, st, ctx)).map_err(|e| e.to_string())?;
    Ok(Link { tx })
}

fn run(mut p: Box<dyn serialport::SerialPort>, rx: Receiver<Cmd>, st: Shared, ctx: egui::Context) {
    let t0 = Instant::now();
    let mut line = String::new();
    let mut buf = [0u8; 1024];
    loop {
        match rx.try_recv() {
            Ok(Cmd::Send(f)) => {
                let s = f.encode() + "\r";
                if let Err(e) = p.write_all(s.as_bytes()) {
                    st.lock().unwrap().err = Some(e.to_string());
                } else {
                    record(&st, f, true, t0);
                }
                ctx.request_repaint();
            }
            Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => {
                let _ = p.write_all(b"C\r");
                break;
            }
            Err(TryRecvError::Empty) => {}
        }
        match p.read(&mut buf) {
            Ok(n) if n > 0 => {
                let mut got = false;
                for &b in &buf[..n] {
                    match b {
                        b'\r' | b'\n' => {
                            if let Some(f) = Frame::decode(&line) {
                                record(&st, f, false, t0);
                                got = true;
                            }
                            line.clear();
                        }
                        0x07 => {
                            st.lock().unwrap().errors += 1;
                            line.clear();
                        }
                        _ if line.len() < 64 => line.push(b as char),
                        _ => line.clear(),
                    }
                }
                if got {
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                st.lock().unwrap().err = Some(tf("适配器断开：{}", &[&e]));
                break;
            }
        }
    }
    st.lock().unwrap().open = false;
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode() {
        let f = Frame {
            id: 0x123,
            ext: false,
            rtr: false,
            data: vec![0x11, 0x22, 0xAB],
            dlc: 3,
        };
        assert_eq!(f.encode(), "t123311 22AB".replace(' ', ""));
        assert_eq!(Frame::decode("t123311 22AB".replace(' ', "").as_str()), Some(f));
        let e = Frame {
            id: 0x18FF50E5,
            ext: true,
            rtr: false,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
            dlc: 8,
        };
        assert_eq!(Frame::decode(&(e.encode() + "1A2B")).unwrap(), e); // 带时间戳
        let r = Frame {
            id: 0x7DF,
            ext: false,
            rtr: true,
            data: vec![],
            dlc: 2,
        };
        assert_eq!(r.encode(), "r7DF2");
        assert_eq!(Frame::decode("r7DF2"), Some(r));
        assert_eq!(Frame::decode("z"), None);
        assert_eq!(Frame::decode("t12"), None);
    }
}

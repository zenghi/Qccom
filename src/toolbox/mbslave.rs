//! Modbus 从站模拟：Modbus TCP 服务器，或者在一个串口上做 RTU 从站。
//! 线圈、离散输入、保持寄存器、输入寄存器各 65536 个，界面上可以直接改值；收到的请求都记下来。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::checksum::crc16_modbus;

pub struct Regs {
    pub coils: Vec<bool>,
    pub discrete: Vec<bool>,
    pub holding: Vec<u16>,
    pub input: Vec<u16>,
}

impl Default for Regs {
    fn default() -> Self {
        let mut r = Regs {
            coils: vec![false; 65536],
            discrete: vec![false; 65536],
            holding: vec![0; 65536],
            input: vec![0; 65536],
        };
        // 给前几个寄存器一些看得出来的初值，方便第一次读
        for i in 0..10 {
            r.holding[i] = (i as u16 + 1) * 100;
            r.input[i] = 1000 + i as u16;
        }
        r.discrete[0] = true;
        r
    }
}

#[derive(Clone)]
pub struct Req {
    pub at: String,
    pub peer: String,
    pub unit: u8,
    pub func: u8,
    pub addr: u16,
    pub count: u16,
    /// None = 正常；Some(异常码)
    pub exc: Option<u8>,
}

#[derive(Default)]
pub struct State {
    pub regs: Regs,
    pub log: VecDeque<Req>,
    pub total: u64,
    pub clients: usize,
    pub err: Option<String>,
}

pub type Shared = Arc<Mutex<State>>;

pub struct Slave {
    pub stop: Arc<AtomicBool>,
}

impl Drop for Slave {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn exc(func: u8, code: u8) -> Vec<u8> {
    vec![func | 0x80, code]
}

fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut b = vec![0u8; bits.len().div_ceil(8)];
    for (i, &v) in bits.iter().enumerate() {
        if v {
            b[i / 8] |= 1 << (i % 8);
        }
    }
    b
}

/// 处理一个请求 PDU，返回应答 PDU 和日志信息 (地址, 数量, 异常码)
pub fn handle(pdu: &[u8], r: &mut Regs) -> (Vec<u8>, u16, u16, Option<u8>) {
    let f = pdu.first().copied().unwrap_or(0);
    let u16_at = |i: usize| -> Option<u16> { Some(u16::from_be_bytes([*pdu.get(i)?, *pdu.get(i + 1)?])) };
    let (Some(addr), Some(arg)) = (u16_at(1), u16_at(3)) else {
        return (exc(f, 3), 0, 0, Some(3));
    };
    let range = |n: u16, max: u16| -> Result<std::ops::Range<usize>, u8> {
        if n == 0 || n > max {
            return Err(3);
        }
        let s = addr as usize;
        if s + n as usize > 65536 {
            return Err(2);
        }
        Ok(s..s + n as usize)
    };
    let out = match f {
        1 | 2 => range(arg, 2000).map(|rg| {
            let src = if f == 1 { &r.coils } else { &r.discrete };
            let b = pack_bits(&src[rg]);
            let mut o = vec![f, b.len() as u8];
            o.extend(b);
            o
        }),
        3 | 4 => range(arg, 125).map(|rg| {
            let src = if f == 3 { &r.holding } else { &r.input };
            let mut o = vec![f, (arg * 2) as u8];
            for v in &src[rg] {
                o.extend(v.to_be_bytes());
            }
            o
        }),
        5 => match arg {
            0xFF00 | 0x0000 => {
                r.coils[addr as usize] = arg == 0xFF00;
                Ok(pdu[..5].to_vec())
            }
            _ => Err(3),
        },
        6 => {
            r.holding[addr as usize] = arg;
            Ok(pdu[..5].to_vec())
        }
        15 => range(arg, 1968).and_then(|rg| {
            let data = pdu.get(6..6 + pdu.get(5).copied().unwrap_or(0) as usize).ok_or(3u8)?;
            if data.len() < (arg as usize).div_ceil(8) {
                return Err(3);
            }
            for (i, a) in rg.enumerate() {
                r.coils[a] = data[i / 8] >> (i % 8) & 1 != 0;
            }
            Ok(pdu[..5].to_vec())
        }),
        16 => range(arg, 123).and_then(|rg| {
            let data = pdu.get(6..6 + pdu.get(5).copied().unwrap_or(0) as usize).ok_or(3u8)?;
            if data.len() < arg as usize * 2 {
                return Err(3);
            }
            for (i, a) in rg.enumerate() {
                r.holding[a] = u16::from_be_bytes([data[i * 2], data[i * 2 + 1]]);
            }
            Ok(pdu[..5].to_vec())
        }),
        _ => Err(1),
    };
    let count = if matches!(f, 5 | 6) { 1 } else { arg };
    match out {
        Ok(o) => (o, addr, count, None),
        Err(c) => (exc(f, c), addr, count, Some(c)),
    }
}

fn record(st: &Shared, peer: &str, unit: u8, pdu: &[u8], addr: u16, count: u16, e: Option<u8>) {
    let mut s = st.lock().unwrap();
    s.total += 1;
    s.log.push_back(Req {
        at: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
        peer: peer.to_string(),
        unit,
        func: pdu.first().copied().unwrap_or(0),
        addr,
        count,
        exc: e,
    });
    while s.log.len() > 2000 {
        s.log.pop_front();
    }
}

pub fn start_tcp(port: u16, unit: u8, st: Shared, ctx: egui::Context) -> Result<Slave, String> {
    let l = TcpListener::bind(("0.0.0.0", port)).map_err(|e| e.to_string())?;
    l.set_nonblocking(true).ok();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    thread::Builder::new()
        .name("mb-slave".into())
        .spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match l.accept() {
                    Ok((s, a)) => {
                        let (st, stop, ctx) = (st.clone(), stop2.clone(), ctx.clone());
                        thread::spawn(move || {
                            st.lock().unwrap().clients += 1;
                            ctx.request_repaint();
                            let _ = tcp_client(s, &a.to_string(), unit, &st, &stop, &ctx);
                            st.lock().unwrap().clients -= 1;
                            ctx.request_repaint();
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Slave { stop })
}

fn tcp_client(mut s: TcpStream, peer: &str, unit: u8, st: &Shared, stop: &AtomicBool, ctx: &egui::Context) -> std::io::Result<()> {
    s.set_nonblocking(false)?;
    s.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buf: Vec<u8> = vec![];
    let mut tmp = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match s.read(&mut tmp) {
            Ok(0) => return Ok(()),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if crate::net::is_timeout(&e) => continue,
            Err(e) => return Err(e),
        }
        // MBAP：事务号 2 + 协议号 2 + 长度 2 + 单元号 1，然后是 PDU
        while buf.len() >= 7 {
            let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
            if len < 2 || len > 260 {
                return Ok(());
            }
            if buf.len() < 6 + len {
                break;
            }
            let frame: Vec<u8> = buf.drain(..6 + len).collect();
            let u = frame[6];
            if u != unit && u != 0 && u != 0xFF {
                continue;
            }
            let pdu = &frame[7..];
            let (resp, addr, count, e) = handle(pdu, &mut st.lock().unwrap().regs);
            record(st, peer, u, pdu, addr, count, e);
            let mut out = frame[..4].to_vec();
            out.extend(((resp.len() + 1) as u16).to_be_bytes());
            out.push(u);
            out.extend(resp);
            s.write_all(&out)?;
            ctx.request_repaint();
        }
    }
    Ok(())
}

/// RTU 从站：按字符间隔切帧（3.5 个字符时间，至少 2 ms）
pub fn start_rtu(port: &str, cfg: crate::serial::PortConfig, unit: u8, st: Shared, ctx: egui::Context) -> Result<Slave, String> {
    let mut p = cfg.builder(port).timeout(Duration::from_millis(2)).open().map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let gap = Duration::from_micros((38_500_000 / cfg.baud.max(1200) as u64).max(2000));
    let name = port.to_string();
    thread::Builder::new()
        .name("mb-rtu".into())
        .spawn(move || {
            let mut buf: Vec<u8> = vec![];
            let mut last = Instant::now();
            let mut tmp = [0u8; 512];
            while !stop2.load(Ordering::Relaxed) {
                match p.read(&mut tmp) {
                    Ok(n) if n > 0 => {
                        buf.extend_from_slice(&tmp[..n]);
                        last = Instant::now();
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) if crate::net::is_timeout(&e) => {}
                    Err(e) => {
                        st.lock().unwrap().err = Some(e.to_string());
                        ctx.request_repaint();
                        return;
                    }
                }
                if buf.is_empty() || last.elapsed() < gap {
                    continue;
                }
                let frame = std::mem::take(&mut buf);
                if frame.len() < 4 || crc16_modbus(&frame[..frame.len() - 2]).to_le_bytes() != frame[frame.len() - 2..] {
                    continue;
                }
                let u = frame[0];
                if u != unit && u != 0 {
                    continue;
                }
                let pdu = &frame[1..frame.len() - 2];
                let (resp, addr, count, e) = handle(pdu, &mut st.lock().unwrap().regs);
                record(&st, &name, u, pdu, addr, count, e);
                // 广播地址 0 只执行不应答
                if u != 0 {
                    let mut out = vec![u];
                    out.extend(resp);
                    out.extend(crc16_modbus(&out).to_le_bytes());
                    let _ = p.write_all(&out);
                }
                ctx.request_repaint();
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Slave { stop })
}

pub fn func_name(f: u8) -> &'static str {
    match f & 0x7f {
        1 => "01 Read Coils",
        2 => "02 Read Discrete",
        3 => "03 Read Holding",
        4 => "04 Read Input",
        5 => "05 Write Coil",
        6 => "06 Write Register",
        15 => "15 Write Coils",
        16 => "16 Write Registers",
        _ => "??",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::{self, Func};

    #[test]
    fn requests() {
        let mut r = Regs::default();
        let (o, ..) = handle(&modbus::pdu(Func::ReadHolding, 0, 2, &[]), &mut r);
        assert_eq!(o, vec![3, 4, 0, 100, 0, 200]);
        let (o, ..) = handle(&modbus::pdu(Func::WriteRegs, 10, 2, &[7, 8]), &mut r);
        assert_eq!(o, vec![16, 0, 10, 0, 2]);
        assert_eq!(&r.holding[10..12], &[7, 8]);
        handle(&modbus::pdu(Func::WriteCoils, 3, 3, &[1, 0, 1]), &mut r);
        assert_eq!(&r.coils[3..6], &[true, false, true]);
        let (o, ..) = handle(&modbus::pdu(Func::ReadCoils, 3, 3, &[]), &mut r);
        assert_eq!(o, vec![1, 1, 0b101]);
        let (o, _, _, e) = handle(&[0x2B, 0, 0, 0, 0], &mut r);
        assert_eq!((o, e), (vec![0xAB, 1], Some(1)));
        let (_, _, _, e) = handle(&modbus::pdu(Func::ReadHolding, 65535, 2, &[]), &mut r);
        assert_eq!(e, Some(2));
    }

    #[test]
    fn tcp_roundtrip() {
        let st: Shared = Default::default();
        let _s = start_tcp(47844, 1, st.clone(), egui::Context::default()).unwrap();
        thread::sleep(Duration::from_millis(100));
        let mut c = TcpStream::connect("127.0.0.1:47844").unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        c.write_all(&modbus::tcp(9, 1, &modbus::pdu(Func::ReadInput, 0, 1, &[]))).unwrap();
        let mut r = [0u8; 11];
        c.read_exact(&mut r).unwrap();
        assert_eq!(r, [0, 9, 0, 0, 0, 5, 1, 4, 2, 0x03, 0xE8]);
        assert_eq!(st.lock().unwrap().total, 1);
    }
}

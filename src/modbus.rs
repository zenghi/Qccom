//! Modbus 主站：组请求帧、解析应答，RTU（串口）和 TCP（MBAP 头）两种封装。
//! 界面在 tools_dbg.rs 的 Modbus 窗口里，这里只管协议。

use serde::{Deserialize, Serialize};

use crate::checksum::crc16_modbus;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Func {
    ReadCoils = 1,
    ReadDiscrete = 2,
    ReadHolding = 3,
    ReadInput = 4,
    WriteCoil = 5,
    WriteReg = 6,
    WriteCoils = 15,
    WriteRegs = 16,
}

pub const FUNCS: [Func; 8] = [
    Func::ReadHolding,
    Func::ReadInput,
    Func::ReadCoils,
    Func::ReadDiscrete,
    Func::WriteReg,
    Func::WriteRegs,
    Func::WriteCoil,
    Func::WriteCoils,
];

impl Func {
    pub fn code(self) -> u8 {
        self as u8
    }
    pub fn is_read(self) -> bool {
        (self as u8) <= 4
    }
    /// 位（线圈 / 离散输入）还是 16 位寄存器
    pub fn is_bits(self) -> bool {
        matches!(self, Func::ReadCoils | Func::ReadDiscrete | Func::WriteCoil | Func::WriteCoils)
    }
}

/// 一次请求的 PDU（功能码 + 数据）
pub fn pdu(f: Func, addr: u16, count: u16, values: &[u16]) -> Vec<u8> {
    let mut p = vec![f.code()];
    p.extend(addr.to_be_bytes());
    match f {
        Func::ReadCoils | Func::ReadDiscrete | Func::ReadHolding | Func::ReadInput => p.extend(count.to_be_bytes()),
        Func::WriteCoil => p.extend(if values.first().copied().unwrap_or(0) != 0 { [0xFF, 0x00] } else { [0x00, 0x00] }),
        Func::WriteReg => p.extend(values.first().copied().unwrap_or(0).to_be_bytes()),
        Func::WriteCoils => {
            let n = values.len() as u16;
            p.extend(n.to_be_bytes());
            let bytes = n.div_ceil(8) as usize;
            p.push(bytes as u8);
            let mut b = vec![0u8; bytes];
            for (i, &v) in values.iter().enumerate() {
                if v != 0 {
                    b[i / 8] |= 1 << (i % 8);
                }
            }
            p.extend(b);
        }
        Func::WriteRegs => {
            p.extend((values.len() as u16).to_be_bytes());
            p.push((values.len() * 2) as u8);
            for v in values {
                p.extend(v.to_be_bytes());
            }
        }
    }
    p
}

pub fn rtu(unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = vec![unit];
    f.extend_from_slice(pdu);
    let c = crc16_modbus(&f);
    f.extend(c.to_le_bytes());
    f
}

pub fn tcp(tid: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(pdu.len() + 7);
    f.extend(tid.to_be_bytes());
    f.extend([0, 0]);
    f.extend(((pdu.len() + 1) as u16).to_be_bytes());
    f.push(unit);
    f.extend_from_slice(pdu);
    f
}

#[derive(Debug, PartialEq)]
pub enum Reply {
    /// 读到的值（寄存器或位）
    Values(Vec<u16>),
    /// 写入成功
    Written,
    /// 异常码
    Exception(u8),
}

/// 应答 PDU 期望的长度（不含地址和 CRC）；还判断不出来时返回 None
fn pdu_len(f: Func, p: &[u8]) -> Option<usize> {
    let code = *p.first()?;
    if code & 0x80 != 0 {
        return Some(2);
    }
    if f.is_read() {
        Some(2 + *p.get(1)? as usize)
    } else {
        Some(5)
    }
}

fn parse_pdu(f: Func, count: u16, p: &[u8]) -> Result<Reply, String> {
    let code = p[0];
    if code == f.code() | 0x80 {
        return Ok(Reply::Exception(p[1]));
    }
    if code != f.code() {
        return Err(format!("function {code:#04x}"));
    }
    if !f.is_read() {
        return Ok(Reply::Written);
    }
    let data = &p[2..];
    if f.is_bits() {
        Ok(Reply::Values((0..count as usize).map(|i| data.get(i / 8).map_or(0, |b| (b >> (i % 8)) as u16 & 1)).collect()))
    } else {
        Ok(Reply::Values(data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()))
    }
}

/// 从收到的字节里找 RTU 应答。Ok(None) = 还没收全；返回 (结果, 用掉的字节数)
pub fn parse_rtu(unit: u8, f: Func, count: u16, buf: &[u8]) -> Result<Option<(Reply, usize)>, String> {
    // 跳过开头不属于这个从站的杂字节
    let Some(start) = buf.iter().position(|&b| b == unit) else { return Ok(None) };
    let b = &buf[start..];
    let Some(n) = b.get(1..).and_then(|p| pdu_len(f, p)) else { return Ok(None) };
    if b.len() < 1 + n + 2 {
        return Ok(None);
    }
    let frame = &b[..1 + n + 2];
    if crc16_modbus(&frame[..1 + n]).to_le_bytes() != frame[1 + n..] {
        return Err("CRC".into());
    }
    Ok(Some((parse_pdu(f, count, &frame[1..1 + n])?, start + frame.len())))
}

pub fn parse_tcp(tid: u16, f: Func, count: u16, buf: &[u8]) -> Result<Option<(Reply, usize)>, String> {
    if buf.len() < 8 {
        return Ok(None);
    }
    let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    if buf.len() < 6 + len {
        return Ok(None);
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != tid {
        // 不是这次请求的应答：丢掉这一帧
        return Ok(Some((Reply::Exception(0xFF), 6 + len)));
    }
    Ok(Some((parse_pdu(f, count, &buf[7..6 + len])?, 6 + len)))
}

pub fn exception_name(c: u8) -> &'static str {
    match c {
        1 => "Illegal function",
        2 => "Illegal data address",
        3 => "Illegal data value",
        4 => "Server device failure",
        5 => "Acknowledge",
        6 => "Server device busy",
        8 => "Memory parity error",
        10 => "Gateway path unavailable",
        11 => "Gateway target failed to respond",
        _ => "Unknown exception",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtu_roundtrip() {
        let req = rtu(1, &pdu(Func::ReadHolding, 0, 10, &[]));
        assert_eq!(req, vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD]);
        // 应答：2 个寄存器 0x1234 0xABCD，前面有一个杂字节
        let mut resp = vec![0x00];
        resp.extend(rtu(1, &[0x03, 0x04, 0x12, 0x34, 0xAB, 0xCD]));
        assert_eq!(parse_rtu(1, Func::ReadHolding, 2, &resp[..5]).unwrap(), None);
        let (r, used) = parse_rtu(1, Func::ReadHolding, 2, &resp).unwrap().unwrap();
        assert_eq!(r, Reply::Values(vec![0x1234, 0xABCD]));
        assert_eq!(used, resp.len());
        let ex = rtu(1, &[0x83, 0x02]);
        assert_eq!(parse_rtu(1, Func::ReadHolding, 2, &ex).unwrap().unwrap().0, Reply::Exception(2));
        let coils = rtu(1, &[0x01, 0x01, 0b0000_0101]);
        assert_eq!(parse_rtu(1, Func::ReadCoils, 3, &coils).unwrap().unwrap().0, Reply::Values(vec![1, 0, 1]));
        assert_eq!(pdu(Func::WriteRegs, 1, 0, &[1, 2]), vec![0x10, 0, 1, 0, 2, 4, 0, 1, 0, 2]);
    }

    #[test]
    fn tcp_frame() {
        let req = tcp(7, 1, &pdu(Func::ReadInput, 0x10, 1, &[]));
        assert_eq!(req, vec![0, 7, 0, 0, 0, 6, 1, 4, 0, 0x10, 0, 1]);
        let resp = [0, 7, 0, 0, 0, 5, 1, 4, 2, 0x00, 0x2A];
        assert_eq!(parse_tcp(7, Func::ReadInput, 1, &resp).unwrap().unwrap().0, Reply::Values(vec![42]));
    }
}

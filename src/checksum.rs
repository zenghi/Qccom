//! 常用校验：发送时自动追加、校验计算器、帧解析和 Modbus / XMODEM 都用这里的实现。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Check {
    #[default]
    None,
    Sum8,
    Xor8,
    Crc8,
    /// CRC-16/MODBUS，低字节在前
    Crc16Modbus,
    /// CRC-16/CCITT-FALSE（初值 0xFFFF），高字节在前
    Crc16Ccitt,
    /// CRC-16/XMODEM（初值 0），高字节在前
    Crc16Xmodem,
    /// CRC-32（以太网 / zip），低字节在前
    Crc32,
}

pub const ALL: [Check; 7] = [Check::Sum8, Check::Xor8, Check::Crc8, Check::Crc16Modbus, Check::Crc16Ccitt, Check::Crc16Xmodem, Check::Crc32];

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Check::None => "—",
            Check::Sum8 => "SUM8",
            Check::Xor8 => "XOR8",
            Check::Crc8 => "CRC-8",
            Check::Crc16Modbus => "CRC-16/MODBUS",
            Check::Crc16Ccitt => "CRC-16/CCITT",
            Check::Crc16Xmodem => "CRC-16/XMODEM",
            Check::Crc32 => "CRC-32",
        }
    }

    pub fn len(self) -> usize {
        match self {
            Check::None => 0,
            Check::Sum8 | Check::Xor8 | Check::Crc8 => 1,
            Check::Crc16Modbus | Check::Crc16Ccitt | Check::Crc16Xmodem => 2,
            Check::Crc32 => 4,
        }
    }

    pub fn value(self, d: &[u8]) -> u32 {
        match self {
            Check::None => 0,
            Check::Sum8 => d.iter().fold(0u8, |a, &b| a.wrapping_add(b)) as u32,
            Check::Xor8 => d.iter().fold(0u8, |a, &b| a ^ b) as u32,
            Check::Crc8 => crc8(d) as u32,
            Check::Crc16Modbus => crc16_modbus(d) as u32,
            Check::Crc16Ccitt => crc16_ccitt(d, 0xFFFF) as u32,
            Check::Crc16Xmodem => crc16_ccitt(d, 0) as u32,
            Check::Crc32 => crc32(d),
        }
    }

    /// 按这种校验在线路上的字节顺序输出
    pub fn bytes(self, d: &[u8]) -> Vec<u8> {
        let v = self.value(d);
        match self {
            Check::None => vec![],
            Check::Sum8 | Check::Xor8 | Check::Crc8 => vec![v as u8],
            Check::Crc16Modbus => (v as u16).to_le_bytes().to_vec(),
            Check::Crc16Ccitt | Check::Crc16Xmodem => (v as u16).to_be_bytes().to_vec(),
            Check::Crc32 => v.to_le_bytes().to_vec(),
        }
    }

    /// 末尾的校验是否正确
    pub fn verify(self, frame: &[u8]) -> bool {
        let n = self.len();
        n == 0 || (frame.len() > n && self.bytes(&frame[..frame.len() - n]) == frame[frame.len() - n..])
    }
}

/// CRC-8（多项式 0x07，初值 0）
pub fn crc8(d: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in d {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
        }
    }
    c
}

pub fn crc16_modbus(d: &[u8]) -> u16 {
    let mut c = 0xFFFFu16;
    for &b in d {
        c ^= b as u16;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xA001 } else { c >> 1 };
        }
    }
    c
}

pub fn crc16_ccitt(d: &[u8], init: u16) -> u16 {
    let mut c = init;
    for &b in d {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

pub fn crc32(d: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        let d = b"123456789";
        assert_eq!(Check::Crc8.value(d), 0xF4);
        assert_eq!(Check::Crc16Modbus.value(d), 0x4B37);
        assert_eq!(Check::Crc16Ccitt.value(d), 0x29B1);
        assert_eq!(Check::Crc16Xmodem.value(d), 0x31C3);
        assert_eq!(Check::Crc32.value(d), 0xCBF4_3926);
        assert_eq!(Check::Sum8.value(&[0xFF, 0x02]), 0x01);
        // Modbus 读保持寄存器：01 03 00 00 00 0A → C5 CD
        let f = [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A];
        assert_eq!(Check::Crc16Modbus.bytes(&f), vec![0xC5, 0xCD]);
        let mut full = f.to_vec();
        full.extend(Check::Crc16Modbus.bytes(&f));
        assert!(Check::Crc16Modbus.verify(&full));
        full[2] = 1;
        assert!(!Check::Crc16Modbus.verify(&full));
    }
}

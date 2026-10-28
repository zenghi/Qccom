//! 参数化 CRC（Rocksoft 模型：宽度、多项式、初值、输入 / 输出反转、结果异或），以及常用预设。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Crc {
    pub width: u8,
    pub poly: u32,
    pub init: u32,
    pub refin: bool,
    pub refout: bool,
    pub xorout: u32,
}

impl Default for Crc {
    fn default() -> Self {
        PRESETS[3].1
    }
}

const fn c(width: u8, poly: u32, init: u32, refin: bool, refout: bool, xorout: u32) -> Crc {
    Crc {
        width,
        poly,
        init,
        refin,
        refout,
        xorout,
    }
}

/// (名字, 参数)。校验值（"123456789"）见测试
pub const PRESETS: &[(&str, Crc)] = &[
    ("CRC-8", c(8, 0x07, 0x00, false, false, 0x00)),
    ("CRC-8/MAXIM", c(8, 0x31, 0x00, true, true, 0x00)),
    ("CRC-8/ROHC", c(8, 0x07, 0xFF, true, true, 0x00)),
    ("CRC-16/MODBUS", c(16, 0x8005, 0xFFFF, true, true, 0x0000)),
    ("CRC-16/CCITT-FALSE", c(16, 0x1021, 0xFFFF, false, false, 0x0000)),
    ("CRC-16/XMODEM", c(16, 0x1021, 0x0000, false, false, 0x0000)),
    ("CRC-16/KERMIT", c(16, 0x1021, 0x0000, true, true, 0x0000)),
    ("CRC-16/ARC", c(16, 0x8005, 0x0000, true, true, 0x0000)),
    ("CRC-16/USB", c(16, 0x8005, 0xFFFF, true, true, 0xFFFF)),
    ("CRC-16/X25", c(16, 0x1021, 0xFFFF, true, true, 0xFFFF)),
    ("CRC-32", c(32, 0x04C1_1DB7, 0xFFFF_FFFF, true, true, 0xFFFF_FFFF)),
    ("CRC-32/MPEG-2", c(32, 0x04C1_1DB7, 0xFFFF_FFFF, false, false, 0x0000_0000)),
    ("CRC-32C", c(32, 0x1EDC_6F41, 0xFFFF_FFFF, true, true, 0xFFFF_FFFF)),
    ("CRC-32/BZIP2", c(32, 0x04C1_1DB7, 0xFFFF_FFFF, false, false, 0xFFFF_FFFF)),
];

fn reflect(v: u32, bits: u8) -> u32 {
    let mut r = 0;
    for i in 0..bits {
        if v & (1 << i) != 0 {
            r |= 1 << (bits - 1 - i);
        }
    }
    r
}

impl Crc {
    pub fn mask(&self) -> u32 {
        if self.width >= 32 {
            u32::MAX
        } else {
            (1u32 << self.width) - 1
        }
    }

    pub fn compute(&self, data: &[u8]) -> u32 {
        let w = self.width.clamp(1, 32);
        let top = 1u64 << (w - 1);
        let mask = self.mask() as u64;
        let mut crc = self.init as u64 & mask;
        for &b in data {
            let b = if self.refin { reflect(b as u32, 8) as u8 } else { b };
            for i in (0..8).rev() {
                let bit = (b >> i) & 1 != 0;
                let hi = crc & top != 0;
                crc = (crc << 1) & mask;
                if hi ^ bit {
                    crc ^= self.poly as u64 & mask;
                }
            }
        }
        let mut r = crc as u32;
        if self.refout {
            r = reflect(r, w);
        }
        (r ^ self.xorout) & self.mask()
    }

    /// 结果按字节：小端（反转输入的算法通常低字节先发）和大端
    pub fn bytes(&self, v: u32, little: bool) -> Vec<u8> {
        let n = self.width.div_ceil(8) as usize;
        let b = v.to_le_bytes();
        let mut out: Vec<u8> = b[..n].to_vec();
        if !little {
            out.reverse();
        }
        out
    }

    pub fn hex_digits(&self) -> usize {
        self.width.div_ceil(4) as usize
    }

    pub fn preset_name(&self) -> Option<&'static str> {
        PRESETS.iter().find(|p| p.1 == *self).map(|p| p.0)
    }
}

/// 简单的累加和 / 异或，和 CRC 一起列出来
pub fn sums(d: &[u8]) -> Vec<(&'static str, u32, usize)> {
    let sum8 = d.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    let sum16 = d.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
    let xor = d.iter().fold(0u8, |a, &b| a ^ b);
    let lrc = sum8.wrapping_neg();
    vec![("SUM8", sum8 as u32, 2), ("SUM16", sum16 as u32, 4), ("XOR8", xor as u32, 2), ("LRC", lrc as u32, 2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_values() {
        let want: &[(&str, u32)] = &[
            ("CRC-8", 0xF4),
            ("CRC-8/MAXIM", 0xA1),
            ("CRC-8/ROHC", 0xD0),
            ("CRC-16/MODBUS", 0x4B37),
            ("CRC-16/CCITT-FALSE", 0x29B1),
            ("CRC-16/XMODEM", 0x31C3),
            ("CRC-16/KERMIT", 0x2189),
            ("CRC-16/ARC", 0xBB3D),
            ("CRC-16/USB", 0xB4C8),
            ("CRC-16/X25", 0x906E),
            ("CRC-32", 0xCBF4_3926),
            ("CRC-32/MPEG-2", 0x0376_E6E7),
            ("CRC-32C", 0xE306_9283),
            ("CRC-32/BZIP2", 0xFC89_1918),
        ];
        for (name, v) in want {
            let p = PRESETS.iter().find(|p| p.0 == *name).unwrap().1;
            assert_eq!(p.compute(b"123456789"), *v, "{name}");
        }
        // 和发送时追加校验用的实现一致
        assert_eq!(PRESETS[3].1.compute(b"\x01\x03\x00\x00\x00\x0A"), crate::checksum::crc16_modbus(b"\x01\x03\x00\x00\x00\x0A") as u32);
    }

    #[test]
    fn byte_order() {
        let c = PRESETS[3].1;
        assert_eq!(c.bytes(0x4B37, true), vec![0x37, 0x4B]);
        assert_eq!(c.bytes(0x4B37, false), vec![0x4B, 0x37]);
    }
}

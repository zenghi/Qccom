//! v1.9 电路仿真：数据模型（元件、导线、注释）、数值单位、坐标变换。
//!
//! 坐标单位是「格」：1 格 = 0.1 英寸（2.54 mm），和面包板、排针的孔距一样。元件引脚都落在整数格点上，
//! 引脚、导线端点落在同一个格点上就算接通（面包板的孔也是元件引脚）。
// 仿真模型里有些字段 / 方法留给后续版本（v1.9.1 进阶实验室）用
#![allow(dead_code)]

pub mod atdev;
pub mod check;
pub mod content;
pub mod examples;
pub mod gen;
pub mod lib;
pub mod mcu;
pub mod models;
pub mod net;
pub mod sim;
#[cfg(test)]
mod sim_tests;
pub mod simport;
pub mod solver;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 一个格点
pub type Pt = (i32, i32);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Circuit {
    pub title: String,
    pub parts: Vec<Part>,
    pub wires: Vec<Wire>,
    pub notes: Vec<Note>,
    pub next_id: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Part {
    pub id: u32,
    /// 元件库里的键，如 res、led、uno
    pub kind: String,
    /// 位置：第一个引脚（或元件原点）所在格点
    pub x: i32,
    pub y: i32,
    /// 旋转：0..3，每档 90°（顺时针）
    pub rot: u8,
    pub flip: bool,
    /// 参数（全部用字符串存，读的时候再解析：「10k」「4.7u」「red」）
    pub props: BTreeMap<String, String>,
    /// 用户起的名字（R1、LED1……），空的话显示自动编号
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Wire {
    pub id: u32,
    /// 折线：第一个和最后一个点是两端
    pub pts: Vec<Pt>,
    /// 杜邦线颜色（WIRE_COLORS 下标）
    pub color: u8,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Note {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub text: String,
}

/// 杜邦线颜色：名字、RGB。顺序就是习惯：红正黑地，其它信号
pub const WIRE_COLORS: [(&str, [u8; 3]); 10] = [
    ("红", [0xe5, 0x39, 0x35]),
    ("黑", [0x30, 0x30, 0x34]),
    ("黄", [0xfd, 0xd8, 0x35]),
    ("绿", [0x43, 0xa0, 0x47]),
    ("蓝", [0x1e, 0x88, 0xe5]),
    ("橙", [0xfb, 0x8c, 0x00]),
    ("白", [0xee, 0xee, 0xee]),
    ("紫", [0x8e, 0x24, 0xaa]),
    ("灰", [0x90, 0x90, 0x98]),
    ("棕", [0x79, 0x55, 0x48]),
];

impl Circuit {
    pub fn alloc_id(&mut self) -> u32 {
        self.next_id = self.next_id.max(
            self.parts
                .iter()
                .map(|p| p.id)
                .chain(self.wires.iter().map(|w| w.id))
                .chain(self.notes.iter().map(|n| n.id))
                .max()
                .unwrap_or(0),
        ) + 1;
        self.next_id
    }

    pub fn add_part(&mut self, kind: &str, x: i32, y: i32) -> u32 {
        let id = self.alloc_id();
        let mut p = Part {
            id,
            kind: kind.into(),
            x,
            y,
            ..Default::default()
        };
        if let Some(d) = lib::def(kind) {
            for (k, v) in d.props.iter() {
                p.props.insert(k.to_string(), v.default.to_string());
            }
        }
        self.parts.push(p);
        id
    }

    pub fn add_wire(&mut self, pts: Vec<Pt>, color: u8) -> u32 {
        let id = self.alloc_id();
        self.wires.push(Wire { id, pts, color });
        id
    }

    pub fn part(&self, id: u32) -> Option<&Part> {
        self.parts.iter().find(|p| p.id == id)
    }

    pub fn part_mut(&mut self, id: u32) -> Option<&mut Part> {
        self.parts.iter_mut().find(|p| p.id == id)
    }

    /// 显示用的名字：用户起的，或者「前缀 + 同类序号」（R1、LED2）
    pub fn label(&self, id: u32) -> String {
        let Some(p) = self.part(id) else { return String::new() };
        if !p.label.is_empty() {
            return p.label.clone();
        }
        let pre = lib::def(&p.kind).map_or("U", |d| d.prefix);
        let n = self.parts.iter().filter(|q| lib::def(&q.kind).map_or("U", |d| d.prefix) == pre && q.id <= id).count();
        format!("{pre}{n}")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Result<Circuit, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

impl Part {
    pub fn prop(&self, k: &str) -> &str {
        if let Some(v) = self.props.get(k) {
            return v;
        }
        lib::def(&self.kind).and_then(|d| d.props.iter().find(|(n, _)| *n == k)).map_or("", |(_, p)| p.default)
    }

    /// 数值参数（带单位前缀）。解析不了时用元件库默认值
    pub fn num(&self, k: &str) -> f64 {
        parse_si(self.prop(k))
            .or_else(|| lib::def(&self.kind).and_then(|d| d.props.iter().find(|(n, _)| *n == k)).and_then(|(_, p)| parse_si(p.default)))
            .unwrap_or(0.0)
    }

    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        self.props.insert(k.to_string(), v.into());
    }

    /// 局部坐标 → 画布格点（先翻转再旋转，再平移）
    pub fn to_world(&self, (lx, ly): Pt) -> Pt {
        let (x, y) = rot_pt((if self.flip { -lx } else { lx }, ly), self.rot);
        (self.x + x, self.y + y)
    }

    pub fn to_worldf(&self, (lx, ly): (f32, f32)) -> (f32, f32) {
        let lx = if self.flip { -lx } else { lx };
        let (x, y) = match self.rot % 4 {
            0 => (lx, ly),
            1 => (-ly, lx),
            2 => (-lx, -ly),
            _ => (ly, -lx),
        };
        (self.x as f32 + x, self.y as f32 + y)
    }

    /// 各引脚的画布位置（两脚元件第二脚可以拉长，见 lib::Leads::Stretch）
    pub fn pins(&self) -> Vec<Pt> {
        let Some(d) = lib::def(&self.kind) else { return vec![] };
        d.pins.iter().enumerate().map(|(i, p)| self.to_world(self.local_pin(d, i, p.at))).collect()
    }

    pub fn local_pin(&self, d: &lib::PartDef, i: usize, at: Pt) -> Pt {
        if d.stretch && i == 1 {
            return self.span();
        }
        at
    }

    /// 可拉长两脚元件：第二脚相对第一脚的位置（局部坐标）
    pub fn span(&self) -> Pt {
        if let Some((a, b)) = self.prop("span").split_once(',') {
            if let (Ok(a), Ok(b)) = (a.trim().parse(), b.trim().parse()) {
                return (a, b);
            }
        }
        lib::def(&self.kind).and_then(|d| d.pins.get(1)).map_or((4, 0), |p| p.at)
    }
}

pub fn rot_pt((x, y): Pt, rot: u8) -> Pt {
    match rot % 4 {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    }
}

/// 解析带单位前缀的数：10k、4.7u、4u7、100n、1M、2.2m、330、0.5、47µ、3V3
pub fn parse_si(s: &str) -> Option<f64> {
    let s = s.trim().replace('µ', "u").replace('μ', "u").replace('Ω', "").replace("ohm", "");
    let s = s.trim_end_matches(|c: char| matches!(c, 'F' | 'H' | 'V' | 'A' | 'W' | 's' | 'z')).trim_end_matches("Hz").trim();
    if s.is_empty() {
        return None;
    }
    let mult = |c: char| -> Option<f64> {
        Some(match c {
            'p' => 1e-12,
            'n' => 1e-9,
            'u' => 1e-6,
            'm' => 1e-3,
            'k' | 'K' => 1e3,
            'M' => 1e6,
            'G' => 1e9,
            'R' | 'r' => 1.0,
            'V' => 1.0,
            _ => return None,
        })
    };
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    // 4k7 / 3V3 这种：前缀字母夹在中间当小数点
    if let Some(pos) = s.find(|c: char| c.is_ascii_alphabetic()) {
        let (a, rest) = s.split_at(pos);
        let mut ch = rest.chars();
        let m = mult(ch.next()?)?;
        let b = ch.as_str();
        let a: f64 = if a.is_empty() { 0.0 } else { a.parse().ok()? };
        if b.is_empty() {
            return Some(a * m);
        }
        if b.chars().all(|c| c.is_ascii_digit()) {
            let frac: f64 = format!("0.{b}").parse().ok()?;
            return Some((a + frac) * m);
        }
    }
    None
}

/// 数值 → 带单位前缀的短文本：4700 → 4.7k
pub fn fmt_si(v: f64, unit: &str) -> String {
    if !v.is_finite() {
        return format!("— {unit}");
    }
    let a = v.abs();
    let (m, p) = if a == 0.0 {
        (1.0, "")
    } else if a >= 1e9 {
        (1e9, "G")
    } else if a >= 1e6 {
        (1e6, "M")
    } else if a >= 1e3 {
        (1e3, "k")
    } else if a >= 1.0 {
        (1.0, "")
    } else if a >= 1e-3 {
        (1e-3, "m")
    } else if a >= 1e-6 {
        (1e-6, "µ")
    } else if a >= 1e-9 {
        (1e-9, "n")
    } else {
        (1e-12, "p")
    };
    let x = v / m;
    let s = if x.abs() >= 100.0 {
        format!("{x:.0}")
    } else if x.abs() >= 10.0 {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    };
    // 去掉多余的 0：4.70 → 4.7，10.0 → 10
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    format!("{s} {p}{unit}").trim_end().to_string()
}

/// 电阻色环（4 环）：数字 → 颜色下标 0..9，乘数 −2..9（金 −1，银 −2）
pub fn color_bands(r: f64) -> Option<[i8; 4]> {
    if r <= 0.0 || !r.is_finite() {
        return None;
    }
    let mut exp = r.log10().floor() as i32 - 1;
    let mut sig = (r / 10f64.powi(exp)).round() as i32;
    if sig >= 100 {
        sig /= 10;
        exp += 1;
    }
    if !(-2..=9).contains(&exp) {
        return None;
    }
    Some([(sig / 10) as i8, (sig % 10) as i8, exp as i8, -1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si() {
        assert_eq!(parse_si("10k"), Some(10_000.0));
        assert_eq!(parse_si("4k7"), Some(4700.0));
        assert!((parse_si("4.7u").unwrap() - 4.7e-6).abs() < 1e-12);
        assert!((parse_si("100nF").unwrap() - 1e-7).abs() < 1e-15);
        assert_eq!(parse_si("3V3"), Some(3.3));
        assert_eq!(parse_si("220Ω"), Some(220.0));
        assert_eq!(parse_si("1M"), Some(1e6));
        assert_eq!(parse_si("2.2m"), Some(2.2e-3));
        assert_eq!(parse_si("abc"), None);
        assert_eq!(fmt_si(4700.0, "Ω"), "4.7 kΩ");
        assert_eq!(fmt_si(0.0136, "A"), "13.6 mA");
        assert_eq!(fmt_si(5.0, "V"), "5 V");
        assert_eq!(color_bands(220.0), Some([2, 2, 1, -1]));
        assert_eq!(color_bands(10_000.0), Some([1, 0, 3, -1]));
        assert_eq!(color_bands(4.7), Some([4, 7, -1, -1]));
    }

    #[test]
    fn transform() {
        let p = Part {
            x: 10,
            y: 5,
            rot: 1,
            ..Default::default()
        };
        assert_eq!(p.to_world((4, 0)), (10, 9));
        let p = Part {
            x: 10,
            y: 5,
            rot: 0,
            flip: true,
            ..Default::default()
        };
        assert_eq!(p.to_world((4, 1)), (6, 6));
    }
}

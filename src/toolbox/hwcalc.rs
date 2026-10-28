//! 硬件计算：E 系列电阻、分压、LED 限流、色环，ADC / NTC，定时器 PWM，I2C 上拉。

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ESeries {
    E12,
    #[default]
    E24,
    E96,
}

const E12: [f64; 12] = [1.0, 1.2, 1.5, 1.8, 2.2, 2.7, 3.3, 3.9, 4.7, 5.6, 6.8, 8.2];
const E24: [f64; 24] = [1.0, 1.1, 1.2, 1.3, 1.5, 1.6, 1.8, 2.0, 2.2, 2.4, 2.7, 3.0, 3.3, 3.6, 3.9, 4.3, 4.7, 5.1, 5.6, 6.2, 6.8, 7.5, 8.2, 9.1];
const E96: [f64; 96] = [
    1.00, 1.02, 1.05, 1.07, 1.10, 1.13, 1.15, 1.18, 1.21, 1.24, 1.27, 1.30, 1.33, 1.37, 1.40, 1.43, 1.47, 1.50, 1.54, 1.58, 1.62, 1.65, 1.69, 1.74, 1.78, 1.82, 1.87, 1.91, 1.96, 2.00, 2.05, 2.10,
    2.15, 2.21, 2.26, 2.32, 2.37, 2.43, 2.49, 2.55, 2.61, 2.67, 2.74, 2.80, 2.87, 2.94, 3.01, 3.09, 3.16, 3.24, 3.32, 3.40, 3.48, 3.57, 3.65, 3.74, 3.83, 3.92, 4.02, 4.12, 4.22, 4.32, 4.42, 4.53,
    4.64, 4.75, 4.87, 4.99, 5.11, 5.23, 5.36, 5.49, 5.62, 5.76, 5.90, 6.04, 6.19, 6.34, 6.49, 6.65, 6.81, 6.98, 7.15, 7.32, 7.50, 7.68, 7.87, 8.06, 8.25, 8.45, 8.66, 8.87, 9.09, 9.31, 9.53, 9.76,
];

impl ESeries {
    pub fn base(self) -> &'static [f64] {
        match self {
            ESeries::E12 => &E12,
            ESeries::E24 => &E24,
            ESeries::E96 => &E96,
        }
    }
    pub fn tolerance(self) -> &'static str {
        match self {
            ESeries::E12 => "±10%",
            ESeries::E24 => "±5%",
            ESeries::E96 => "±1%",
        }
    }
    /// lo..=hi 之间的所有标准值
    pub fn values(self, lo: f64, hi: f64) -> Vec<f64> {
        let mut v = vec![];
        for dec in -1..=8 {
            let m = 10f64.powi(dec);
            for b in self.base() {
                let x = round_sig(b * m);
                if x >= lo * 0.999 && x <= hi * 1.001 {
                    v.push(x);
                }
            }
        }
        v
    }
    /// 最接近的标准值；up = true 时取不小于 v 的
    pub fn nearest(self, v: f64, up: bool) -> f64 {
        let vals = self.values(v / 20.0, v * 20.0);
        if up {
            vals.into_iter().find(|x| *x >= v * 0.9999).unwrap_or(v)
        } else {
            vals.into_iter().min_by(|a, b| ((a / v).ln().abs()).total_cmp(&(b / v).ln().abs())).unwrap_or(v)
        }
    }
}

fn round_sig(x: f64) -> f64 {
    let m = 10f64.powi(x.abs().log10().floor() as i32 - 2);
    (x / m).round() * m
}

/// 4.7k、4k7、10M、100m、2.2u、1e3 都认；unit 是允许的单位后缀（Ω、V、Hz 等，忽略）
pub fn parse_si(s: &str) -> Option<f64> {
    let s: String = s.trim().chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.trim_end_matches(['Ω', 'Ω', 'V', 'v', 'A', 'F', 'z', 'H', 'R', 's']).replace("Hz", "");
    if s.is_empty() {
        return None;
    }
    let mul = |c: char| -> Option<f64> {
        Some(match c {
            'p' => 1e-12,
            'n' => 1e-9,
            'u' | 'µ' | 'μ' => 1e-6,
            'm' => 1e-3,
            'k' | 'K' => 1e3,
            'M' => 1e6,
            'G' => 1e9,
            'R' | 'r' => 1.0,
            _ => return None,
        })
    };
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    // 4k7 这种中间带倍率的
    if let Some((i, c)) = s.char_indices().find(|(_, c)| mul(*c).is_some()) {
        let (a, b) = (&s[..i], &s[i + c.len_utf8()..]);
        let num: f64 = if b.is_empty() { a.parse().ok()? } else { format!("{a}.{b}").parse().ok()? };
        return Some(num * mul(c)?);
    }
    None
}

/// 4700 → 4.7k
pub fn fmt_si(v: f64, unit: &str) -> String {
    let a = v.abs();
    let (d, p) = if a == 0.0 {
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
    let x = v / d;
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s} {p}{unit}")
}

#[derive(Clone, Debug)]
pub struct DivPick {
    pub r1: f64,
    pub r2: f64,
    pub vout: f64,
    pub err: f64,
    pub current: f64,
}

/// 分压：Vout = Vin × R2 / (R1 + R2)，R1 R2 都在 [lo, hi] 里挑标准值
pub fn divider(vin: f64, vout: f64, e: ESeries, lo: f64, hi: f64, n: usize) -> Vec<DivPick> {
    if vin <= 0.0 || vout <= 0.0 || vout >= vin {
        return vec![];
    }
    let vals = e.values(lo, hi);
    let mut out = vec![];
    for &r1 in &vals {
        // 对每个 R1 找最合适的 R2
        let ideal = vout * r1 / (vin - vout);
        let r2 = vals.iter().copied().min_by(|a, b| (a - ideal).abs().total_cmp(&(b - ideal).abs()));
        if let Some(r2) = r2 {
            let v = vin * r2 / (r1 + r2);
            out.push(DivPick {
                r1,
                r2,
                vout: v,
                err: (v - vout) / vout * 100.0,
                current: vin / (r1 + r2),
            });
        }
    }
    // 误差小的在前；误差差不多（0.01%）时选总阻值接近 10k–100k 的
    out.sort_by(|a, b| {
        let ea = (a.err.abs() * 100.0).round();
        let eb = (b.err.abs() * 100.0).round();
        ea.total_cmp(&eb).then(((a.r1 + a.r2) / 30e3).ln().abs().total_cmp(&((b.r1 + b.r2) / 30e3).ln().abs()))
    });
    out.dedup_by(|a, b| a.r1 == b.r1 && a.r2 == b.r2);
    out.truncate(n);
    out
}

pub struct Led {
    pub exact: f64,
    pub std: f64,
    pub current: f64,
    pub power: f64,
}

/// LED 限流电阻：(Vs − Vf) / I，取不小于它的标准值
pub fn led(vs: f64, vf: f64, i: f64, e: ESeries) -> Option<Led> {
    if vs <= vf || i <= 0.0 {
        return None;
    }
    let exact = (vs - vf) / i;
    let std = e.nearest(exact, true);
    let current = (vs - vf) / std;
    Some(Led {
        exact,
        std,
        current,
        power: current * current * std,
    })
}

/// 色环颜色名（0–9），金、银是 -1、-2 倍率
pub const COLORS: [&str; 10] = ["黑", "棕", "红", "橙", "黄", "绿", "蓝", "紫", "灰", "白"];
pub const RGB: [[u8; 3]; 12] = [
    [0x20, 0x20, 0x20],
    [0x8B, 0x45, 0x13],
    [0xE5, 0x39, 0x35],
    [0xFB, 0x8C, 0x00],
    [0xFD, 0xD8, 0x35],
    [0x43, 0xA0, 0x47],
    [0x1E, 0x88, 0xE5],
    [0x8E, 0x24, 0xAA],
    [0x9E, 0x9E, 0x9E],
    [0xF5, 0xF5, 0xF5],
    [0xD4, 0xAF, 0x37],
    [0xC0, 0xC0, 0xC0],
];

/// 阻值 → 色环：返回 (有效数字, 倍率指数)；四环 2 位有效数字，五环 3 位
pub fn bands(r: f64, five: bool) -> Option<(Vec<u8>, i32)> {
    if r < 0.1 {
        return None;
    }
    let digits = if five { 3 } else { 2 };
    let mut exp = r.log10().floor() as i32 - (digits - 1);
    let mut sig = (r / 10f64.powi(exp)).round() as u64;
    if sig >= 10u64.pow(digits as u32) {
        sig /= 10;
        exp += 1;
    }
    if !(-2..=9).contains(&exp) {
        return None;
    }
    let d: Vec<u8> = sig.to_string().bytes().map(|b| b - b'0').collect();
    Some((d, exp))
}

// ------------------------------------------------------------------ ADC / NTC

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Ntc {
    pub r25: f64,
    pub beta: f64,
    /// 串联的固定电阻
    pub rs: f64,
    /// true：NTC 接地、固定电阻接 Vref（温度越高电压越低）
    pub ntc_low: bool,
    pub vref: f64,
    pub bits: u32,
    /// 不用 B 值时的 R-T 表（每行 “温度, 阻值”）
    pub table: String,
    pub use_table: bool,
}

impl Default for Ntc {
    fn default() -> Self {
        Ntc {
            r25: 10e3,
            beta: 3950.0,
            rs: 10e3,
            ntc_low: true,
            vref: 3.3,
            bits: 12,
            table: String::new(),
            use_table: false,
        }
    }
}

const K0: f64 = 273.15;

impl Ntc {
    fn rt_table(&self) -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = self
            .table
            .lines()
            .filter_map(|l| {
                let mut it = l.split([',', '\t', ' ', ';']).filter(|s| !s.is_empty());
                Some((it.next()?.parse().ok()?, parse_si(it.next()?)?))
            })
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    }

    /// 温度（°C）→ 阻值
    pub fn r_at(&self, t: f64) -> f64 {
        if self.use_table {
            let tb = self.rt_table();
            if tb.len() >= 2 {
                // 在 ln(R) 上线性插值
                let i = tb.partition_point(|p| p.0 < t).clamp(1, tb.len() - 1);
                let (a, b) = (tb[i - 1], tb[i]);
                let f = (t - a.0) / (b.0 - a.0);
                return (a.1.ln() + f * (b.1.ln() - a.1.ln())).exp();
            }
        }
        self.r25 * (self.beta * (1.0 / (t + K0) - 1.0 / (25.0 + K0))).exp()
    }

    /// 阻值 → 温度（°C）
    pub fn t_at(&self, r: f64) -> f64 {
        if self.use_table {
            let tb = self.rt_table();
            if tb.len() >= 2 {
                // 阻值随温度单调下降
                let i = tb.partition_point(|p| p.1 > r).clamp(1, tb.len() - 1);
                let (a, b) = (tb[i - 1], tb[i]);
                let f = (r.ln() - a.1.ln()) / (b.1.ln() - a.1.ln());
                return a.0 + f * (b.0 - a.0);
            }
        }
        1.0 / (1.0 / (25.0 + K0) + (r / self.r25).ln() / self.beta) - K0
    }

    pub fn full(&self) -> f64 {
        ((1u64 << self.bits.min(24)) - 1) as f64
    }

    pub fn v_of_r(&self, r: f64) -> f64 {
        if self.ntc_low {
            self.vref * r / (self.rs + r)
        } else {
            self.vref * self.rs / (self.rs + r)
        }
    }

    pub fn r_of_v(&self, v: f64) -> f64 {
        let v = v.clamp(1e-9, self.vref - 1e-9);
        if self.ntc_low {
            self.rs * v / (self.vref - v)
        } else {
            self.rs * (self.vref - v) / v
        }
    }

    pub fn raw_at(&self, t: f64) -> f64 {
        self.v_of_r(self.r_at(t)) / self.vref * self.full()
    }

    pub fn t_of_raw(&self, raw: f64) -> f64 {
        self.t_at(self.r_of_v(raw / self.full() * self.vref))
    }

    /// C 查找表：温度从 lo 到 hi，每 step 度一个 ADC 值
    pub fn c_table(&self, lo: i32, hi: i32, step: i32) -> String {
        let step = step.max(1);
        let vals: Vec<(i32, u32)> = (lo..=hi).step_by(step as usize).map(|t| (t, self.raw_at(t as f64).round() as u32)).collect();
        let ty = if self.bits > 16 { "uint32_t" } else { "uint16_t" };
        let mut s = format!(
            "/* NTC R25={} B={} Rs={} {} Vref={}V {}-bit\n * temperature = NTC_T_MIN + index * NTC_T_STEP */\n#define NTC_T_MIN  ({lo})\n#define NTC_T_STEP ({step})\n#define NTC_N      ({})\n\nstatic const {ty} ntc_adc[NTC_N] = {{\n",
            fmt_si(self.r25, "Ω").replace(' ', ""),
            if self.use_table { "table".to_string() } else { format!("{}", self.beta) },
            fmt_si(self.rs, "Ω").replace(' ', ""),
            if self.ntc_low { "NTC-to-GND" } else { "NTC-to-Vref" },
            self.vref,
            self.bits,
            vals.len()
        );
        for ch in vals.chunks(8) {
            s.push_str("    ");
            s.push_str(&ch.iter().map(|(_, v)| format!("{v:5}")).collect::<Vec<_>>().join(","));
            s.push_str(&format!(", /* {}..{} °C */\n", ch[0].0, ch[ch.len() - 1].0));
        }
        s.push_str("};\n");
        s
    }
}

// ------------------------------------------------------------------ 定时器 PWM

#[derive(Clone, Debug, PartialEq)]
pub struct Pwm {
    pub psc: u64,
    pub arr: u64,
    pub freq: f64,
    pub err: f64,
    pub ccr: u64,
    /// 占空比能调多细（ARR + 1 级）
    pub steps: u64,
}

/// f = clk / ((PSC + 1) × (ARR + 1))；返回误差最小的几组，同样误差时分辨率高的在前
pub fn pwm(clk: f64, f: f64, duty: f64, psc_max: u64, arr_max: u64, n: usize) -> Vec<Pwm> {
    if clk <= 0.0 || f <= 0.0 || f > clk {
        return vec![];
    }
    let total = clk / f;
    let mut out = vec![];
    let lo = ((total / (arr_max + 1) as f64).ceil() as u64).saturating_sub(1);
    for psc in lo..=psc_max.min(lo + 200_000) {
        let arr1 = (total / (psc + 1) as f64).round().max(1.0) as u64;
        if arr1 < 2 || arr1 - 1 > arr_max {
            continue;
        }
        let freq = clk / ((psc + 1) * arr1) as f64;
        out.push(Pwm {
            psc,
            arr: arr1 - 1,
            freq,
            err: (freq - f) / f * 100.0,
            ccr: (duty / 100.0 * arr1 as f64).round() as u64,
            steps: arr1,
        });
        if arr1 < 4 {
            break;
        }
    }
    out.sort_by(|a, b| (a.err.abs() * 1e6).round().total_cmp(&(b.err.abs() * 1e6).round()).then(b.steps.cmp(&a.steps)));
    out.truncate(n);
    out
}

// ------------------------------------------------------------------ I2C 上拉

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum I2cMode {
    Standard,
    #[default]
    Fast,
    FastPlus,
}

impl I2cMode {
    pub const ALL: [I2cMode; 3] = [I2cMode::Standard, I2cMode::Fast, I2cMode::FastPlus];
    pub fn label(self) -> &'static str {
        match self {
            I2cMode::Standard => "100 kHz",
            I2cMode::Fast => "400 kHz",
            I2cMode::FastPlus => "1 MHz",
        }
    }
    /// 最大上升时间（秒）
    pub fn tr(self) -> f64 {
        match self {
            I2cMode::Standard => 1000e-9,
            I2cMode::Fast => 300e-9,
            I2cMode::FastPlus => 120e-9,
        }
    }
    /// 低电平灌电流（A）
    pub fn iol(self) -> f64 {
        match self {
            I2cMode::FastPlus => 20e-3,
            _ => 3e-3,
        }
    }
}

/// NXP UM10204：Rp(min) = (Vdd − Vol) / Iol，Rp(max) = tr / (0.8473 × Cb)
pub fn i2c_pullup(vdd: f64, cb: f64, mode: I2cMode) -> (f64, f64) {
    let vol = if vdd > 2.0 { 0.4 } else { 0.2 * vdd };
    ((vdd - vol) / mode.iol(), mode.tr() / (0.8473 * cb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn si() {
        assert_eq!(parse_si("4.7k"), Some(4700.0));
        assert_eq!(parse_si("4k7"), Some(4700.0));
        assert_eq!(parse_si("10MHz"), Some(10e6));
        assert_eq!(parse_si("3.3V"), Some(3.3));
        assert!((parse_si("100n").unwrap() - 1e-7).abs() < 1e-15);
        assert_eq!(parse_si("2 mA"), Some(0.002));
        assert_eq!(fmt_si(4700.0, "Ω"), "4.7 kΩ");
        assert_eq!(fmt_si(0.02, "A"), "20 mA");
    }

    #[test]
    fn resistors() {
        assert_eq!(ESeries::E24.nearest(4600.0, false), 4700.0);
        assert_eq!(ESeries::E24.nearest(4800.0, true), 5100.0);
        assert_eq!(ESeries::E96.nearest(10_050.0, false), 10_000.0);
        let d = divider(12.0, 3.3, ESeries::E24, 1e3, 1e6, 5);
        assert!(d[0].err.abs() < 0.5, "{d:?}");
        let l = led(5.0, 2.0, 0.02, ESeries::E24).unwrap();
        assert_eq!(l.exact, 150.0);
        assert_eq!(l.std, 150.0);
        assert_eq!(bands(4700.0, false), Some((vec![4, 7], 2)));
        assert_eq!(bands(10_000.0, true), Some((vec![1, 0, 0], 2)));
        assert_eq!(bands(0.47, false), Some((vec![4, 7], -2)));
    }

    #[test]
    fn ntc() {
        let n = Ntc::default();
        assert!((n.r_at(25.0) - 10e3).abs() < 1e-6);
        assert!((n.t_at(n.r_at(60.0)) - 60.0).abs() < 1e-6);
        // 25 °C 时正好一半
        assert!((n.raw_at(25.0) - 4095.0 / 2.0).abs() < 0.01);
        assert!((n.t_of_raw(n.raw_at(-10.0)) + 10.0).abs() < 1e-6);
        let tb = Ntc {
            use_table: true,
            table: "0, 32.65k\n25, 10k\n50, 3.6k".into(),
            ..Default::default()
        };
        assert!((tb.r_at(25.0) - 10e3).abs() < 1e-6);
        assert!((tb.t_at(10e3) - 25.0).abs() < 1e-6);
        assert!(n.c_table(-20, 100, 5).contains("ntc_adc[NTC_N]"));
    }

    #[test]
    fn timer() {
        let r = pwm(72e6, 1000.0, 50.0, 65535, 65535, 3);
        assert_eq!(r[0].err, 0.0);
        assert_eq!((r[0].psc + 1) * (r[0].arr + 1), 72_000);
        assert_eq!(r[0].ccr, (r[0].arr + 1) / 2);
        // 分辨率最高的排第一
        assert_eq!(r[0].psc, 1);
    }

    #[test]
    fn i2c() {
        let (lo, hi) = i2c_pullup(3.3, 200e-12, I2cMode::Fast);
        assert!((lo - 966.7).abs() < 1.0);
        assert!((hi - 1770.3).abs() < 1.0);
    }
}

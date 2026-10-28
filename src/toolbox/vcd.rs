//! VCD（Value Change Dump）波形文件：逻辑分析仪、PulseView / sigrok、仿真器都能导出。

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Val {
    pub v: u64,
    /// 有 x / z 位
    pub xz: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Signal {
    pub name: String,
    pub width: u32,
    pub real: bool,
    pub changes: Vec<(u64, Val)>,
    /// real 类型的值（和 changes 对应）
    pub reals: Vec<f64>,
}

impl Signal {
    /// 某时刻的值（下标）
    pub fn at(&self, t: u64) -> Option<usize> {
        let i = self.changes.partition_point(|c| c.0 <= t);
        i.checked_sub(1)
    }

    pub fn fmt(&self, i: usize) -> String {
        let (_, v) = self.changes[i];
        if self.real {
            return format!("{}", self.reals.get(i).copied().unwrap_or(0.0));
        }
        if v.xz {
            return if self.width == 1 { "x".into() } else { "X".into() };
        }
        if self.width == 1 {
            v.v.to_string()
        } else {
            format!("{:X}", v.v)
        }
    }

    /// 下一个 / 上一个变化的时刻
    pub fn edge(&self, t: u64, forward: bool) -> Option<u64> {
        if forward {
            let i = self.changes.partition_point(|c| c.0 <= t);
            self.changes.get(i).map(|c| c.0)
        } else {
            let i = self.changes.partition_point(|c| c.0 < t);
            i.checked_sub(1).map(|i| self.changes[i].0)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Vcd {
    /// 一个时间单位是多少秒
    pub unit: f64,
    pub signals: Vec<Signal>,
    pub end: u64,
    pub start: u64,
    pub date: String,
}

fn timescale(s: &str) -> f64 {
    let s: String = s.split_whitespace().collect();
    let num: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: f64 = num.parse().unwrap_or(1.0);
    let unit = &s[num.len()..];
    n * match unit {
        "s" => 1.0,
        "ms" => 1e-3,
        "us" => 1e-6,
        "ns" => 1e-9,
        "ps" => 1e-12,
        "fs" => 1e-15,
        _ => 1e-9,
    }
}

fn bits(s: &str) -> Val {
    let mut v = 0u64;
    let mut xz = false;
    for c in s.chars() {
        v = (v << 1) | (c == '1') as u64;
        xz |= matches!(c, 'x' | 'X' | 'z' | 'Z');
    }
    Val { v, xz }
}

pub fn parse(text: &str) -> Result<Vcd, String> {
    let mut d = Vcd { unit: 1e-9, ..Default::default() };
    let mut ids: HashMap<String, Vec<usize>> = HashMap::new();
    let mut scope: Vec<String> = vec![];
    let mut toks = text.split_whitespace().peekable();
    let mut t: u64 = 0;
    let mut first_time = true;
    let mut in_defs = true;
    while let Some(tok) = toks.next() {
        if in_defs {
            match tok {
                "$timescale" => {
                    let mut s = String::new();
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                        s.push_str(x);
                    }
                    d.unit = timescale(&s);
                }
                "$date" => {
                    let mut s = vec![];
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                        s.push(x);
                    }
                    d.date = s.join(" ");
                }
                "$scope" => {
                    let mut v = vec![];
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                        v.push(x);
                    }
                    scope.push(v.get(1).copied().unwrap_or("").to_string());
                }
                "$upscope" => {
                    scope.pop();
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                    }
                }
                "$var" => {
                    let mut v = vec![];
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                        v.push(x);
                    }
                    if v.len() < 4 {
                        continue;
                    }
                    let real = v[0] == "real";
                    let width: u32 = v[1].parse().unwrap_or(1);
                    let mut name = v[3].to_string();
                    if let Some(r) = v.get(4) {
                        if r.starts_with('[') {
                            name.push_str(r);
                        }
                    }
                    d.signals.push(Signal {
                        name,
                        width: if real { 64 } else { width.clamp(1, 64) },
                        real,
                        ..Default::default()
                    });
                    ids.entry(v[2].to_string()).or_default().push(d.signals.len() - 1);
                }
                "$enddefinitions" => {
                    in_defs = false;
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                    }
                }
                _ if tok.starts_with('$') => {
                    for x in toks.by_ref() {
                        if x == "$end" {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let first = tok.as_bytes()[0];
        match first {
            b'#' => {
                t = tok[1..].parse().map_err(|_| format!("bad time {tok}"))?;
                if first_time {
                    d.start = t;
                    first_time = false;
                }
                d.end = d.end.max(t);
            }
            b'$' => {
                // $dumpvars / $end 等，内容照常按值变化处理
            }
            b'0' | b'1' | b'x' | b'X' | b'z' | b'Z' => {
                let id = &tok[1..];
                let v = Val {
                    v: (first == b'1') as u64,
                    xz: !matches!(first, b'0' | b'1'),
                };
                for &i in ids.get(id).map(|v| v.as_slice()).unwrap_or(&[]) {
                    push(&mut d.signals[i], t, v, 0.0);
                }
            }
            b'b' | b'B' => {
                let v = bits(&tok[1..]);
                let id = toks.next().unwrap_or("");
                for &i in ids.get(id).map(|v| v.as_slice()).unwrap_or(&[]) {
                    push(&mut d.signals[i], t, v, 0.0);
                }
            }
            b'r' | b'R' => {
                let f: f64 = tok[1..].parse().unwrap_or(0.0);
                let id = toks.next().unwrap_or("");
                for &i in ids.get(id).map(|v| v.as_slice()).unwrap_or(&[]) {
                    push(&mut d.signals[i], t, Val { v: f.to_bits(), xz: false }, f);
                }
            }
            _ => {}
        }
    }
    if d.signals.is_empty() {
        return Err(crate::i18n::t("不是 VCD 文件（没有 $var 信号定义）").into());
    }
    Ok(d)
}

fn push(s: &mut Signal, t: u64, v: Val, f: f64) {
    // 同一时刻多次赋值只留最后一次；值没变不记
    if let Some(last) = s.changes.last_mut() {
        if last.0 == t {
            last.1 = v;
            if s.real {
                if let Some(r) = s.reals.last_mut() {
                    *r = f;
                }
            }
            return;
        }
        if last.1 == v {
            return;
        }
    }
    s.changes.push((t, v));
    if s.real {
        s.reals.push(f);
    }
}

/// 把秒数写成 12.5 µs 这样
pub fn fmt_time(s: f64) -> String {
    let a = s.abs();
    if a == 0.0 {
        return "0".into();
    }
    let (d, u) = if a >= 1.0 {
        (1.0, "s")
    } else if a >= 1e-3 {
        (1e-3, "ms")
    } else if a >= 1e-6 {
        (1e-6, "µs")
    } else if a >= 1e-9 {
        (1e-9, "ns")
    } else {
        (1e-12, "ps")
    };
    let x = format!("{:.3}", s / d);
    format!("{} {u}", x.trim_end_matches('0').trim_end_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vcd() {
        let v = "$date today $end\n$timescale 10 ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n$var wire 8 \" data [7:0] $end\n$upscope $end\n$enddefinitions $end\n$dumpvars\n0!\nbxxxxxxxx \"\n$end\n#0\n1!\n#5\n0!\nb1010 \"\n#10\n1!\n#15\n0!\n";
        let d = parse(v).unwrap();
        assert_eq!(d.unit, 10e-9);
        assert_eq!(d.signals.len(), 2);
        assert_eq!(d.signals[1].name, "data[7:0]");
        let clk = &d.signals[0];
        assert_eq!(clk.changes.len(), 4);
        assert_eq!(clk.fmt(clk.at(7).unwrap()), "0");
        let data = &d.signals[1];
        assert_eq!(data.fmt(data.at(0).unwrap()), "X");
        assert_eq!(data.fmt(data.at(6).unwrap()), "A");
        assert_eq!(clk.edge(5, true), Some(10));
        assert_eq!(clk.edge(10, false), Some(5));
        assert_eq!(d.end, 15);
        assert_eq!(fmt_time(1.25e-5), "12.5 µs");
    }
}

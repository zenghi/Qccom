//! 十六进制比较：BIN 按偏移逐字节对照（可选自动对齐插入 / 删除的字节），
//! HEX / SREC 按地址对照，不管记录顺序和分段方式。

use crate::binview::parse::{load_image, Image};
use std::path::Path;

pub const W: usize = 16;

/// 一行里的一格：左字节、右字节
pub type Cell = (Option<u8>, Option<u8>);

pub struct RowData {
    pub la: Option<u64>,
    pub ra: Option<u64>,
    pub cells: [Cell; W],
}

enum Mode {
    /// 同一偏移对照
    Offset { a: Vec<u8>, b: Vec<u8> },
    /// 对齐后的字节对；每格记下左右的偏移
    Aligned { pairs: Vec<(Option<u32>, Option<u32>)>, a: Vec<u8>, b: Vec<u8> },
    /// 按地址：每行的起始地址，None 是段之间的空隙
    Addr { a: Image, b: Image, rows: Vec<Option<u64>> },
}

pub struct HexCmp {
    mode: Mode,
    /// 有不同的行
    pub diff_rows: Vec<usize>,
    pub diff_bytes: u64,
    pub kind_l: &'static str,
    pub kind_r: &'static str,
    pub size_l: u64,
    pub size_r: u64,
}

/// 对齐模式最多处理的大小
pub const ALIGN_MAX: usize = 8 << 20;

pub fn open(a: &Path, b: &Path, align: bool) -> Result<HexCmp, String> {
    let ba = std::fs::read(a).map_err(|e| format!("{}: {e}", a.display()))?;
    let bb = std::fs::read(b).map_err(|e| format!("{}: {e}", b.display()))?;
    let ia = load_image(a, &ba);
    let ib = load_image(b, &bb);
    let is_text_image = |i: &Option<Image>| i.as_ref().is_some_and(|i| i.kind != "BIN");
    let mode = if is_text_image(&ia) || is_text_image(&ib) {
        let a = ia.unwrap_or_else(|| Image::bin(ba));
        let b = ib.unwrap_or_else(|| Image::bin(bb));
        let rows = addr_rows(&a, &b);
        Mode::Addr { a, b, rows }
    } else if align && ba.len().max(bb.len()) <= ALIGN_MAX && ba != bb {
        let pairs = align_bytes(&ba, &bb);
        Mode::Aligned { pairs, a: ba, b: bb }
    } else {
        Mode::Offset { a: ba, b: bb }
    };
    let mut h = HexCmp {
        kind_l: "",
        kind_r: "",
        size_l: 0,
        size_r: 0,
        mode,
        diff_rows: vec![],
        diff_bytes: 0,
    };
    (h.kind_l, h.kind_r, h.size_l, h.size_r) = match &h.mode {
        Mode::Offset { a, b } | Mode::Aligned { a, b, .. } => ("BIN", "BIN", a.len() as u64, b.len() as u64),
        Mode::Addr { a, b, .. } => (a.kind, b.kind, a.size(), b.size()),
    };
    h.scan();
    Ok(h)
}

fn addr_rows(a: &Image, b: &Image) -> Vec<Option<u64>> {
    let mut starts: Vec<u64> = a.rows().into_iter().chain(b.rows()).flatten().collect();
    starts.sort();
    starts.dedup();
    let mut out = vec![];
    let mut prev: Option<u64> = None;
    for s in starts {
        if prev.is_some_and(|p| p + W as u64 != s) {
            out.push(None);
        }
        out.push(Some(s));
        prev = Some(s);
    }
    out
}

/// 字节级差分对齐；差得太多的段落按位置配对
fn align_bytes(a: &[u8], b: &[u8]) -> Vec<(Option<u32>, Option<u32>)> {
    let ka: Vec<u64> = a.iter().map(|&x| x as u64).collect();
    let kb: Vec<u64> = b.iter().map(|&x| x as u64).collect();
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let (mut x, mut y) = (0usize, 0usize);
    for h in super::diff::hunks(&ka, &kb) {
        while x < h.a {
            out.push((Some(x as u32), Some(y as u32)));
            x += 1;
            y += 1;
        }
        let n = h.an.max(h.bn);
        for k in 0..n {
            out.push(((k < h.an).then(|| (h.a + k) as u32), (k < h.bn).then(|| (h.b + k) as u32)));
        }
        x = h.a + h.an;
        y = h.b + h.bn;
    }
    while x < a.len() {
        out.push((Some(x as u32), Some(y as u32)));
        x += 1;
        y += 1;
    }
    out
}

impl HexCmp {
    pub fn rows(&self) -> usize {
        match &self.mode {
            Mode::Offset { a, b } => a.len().max(b.len()).div_ceil(W),
            Mode::Aligned { pairs, .. } => pairs.len().div_ceil(W),
            Mode::Addr { rows, .. } => rows.len(),
        }
    }

    /// 是否是段之间的分隔行
    pub fn is_gap(&self, i: usize) -> bool {
        matches!(&self.mode, Mode::Addr { rows, .. } if rows[i].is_none())
    }

    pub fn row(&self, i: usize) -> RowData {
        let mut cells = [(None, None); W];
        match &self.mode {
            Mode::Offset { a, b } => {
                let base = i * W;
                for (k, c) in cells.iter_mut().enumerate() {
                    *c = (a.get(base + k).copied(), b.get(base + k).copied());
                }
                RowData {
                    la: (base < a.len()).then_some(base as u64),
                    ra: (base < b.len()).then_some(base as u64),
                    cells,
                }
            }
            Mode::Aligned { pairs, a, b } => {
                let base = i * W;
                let (mut la, mut ra) = (None, None);
                for (k, c) in cells.iter_mut().enumerate() {
                    if let Some(&(x, y)) = pairs.get(base + k) {
                        *c = (x.map(|x| a[x as usize]), y.map(|y| b[y as usize]));
                        la = la.or(x.map(|x| x as u64));
                        ra = ra.or(y.map(|y| y as u64));
                    }
                }
                RowData { la, ra, cells }
            }
            Mode::Addr { a, b, rows } => {
                let Some(s) = rows[i] else {
                    return RowData { la: None, ra: None, cells };
                };
                for (k, c) in cells.iter_mut().enumerate() {
                    *c = (a.get(s + k as u64), b.get(s + k as u64));
                }
                RowData { la: Some(s), ra: Some(s), cells }
            }
        }
    }

    fn scan(&mut self) {
        let n = self.rows();
        let mut rows = vec![];
        let mut bytes = 0u64;
        for i in 0..n {
            let r = self.row(i);
            let d = r.cells.iter().filter(|(x, y)| x != y).count();
            if d > 0 {
                rows.push(i);
                bytes += d as u64;
            }
        }
        self.diff_rows = rows;
        self.diff_bytes = bytes;
    }

    /// 连续不同的段（起始行, 结束行）用来逐处跳转
    pub fn blocks(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = vec![];
        for &r in &self.diff_rows {
            match v.last_mut() {
                Some(b) if b.1 == r => b.1 = r + 1,
                _ => v.push((r, r + 1)),
            }
        }
        v
    }

    /// 差异列表（导出用）：每段连续不同的字节一行
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str("# left_addr  right_addr  len  left_bytes  right_bytes\n");
        let mut run: Option<(Option<u64>, Option<u64>, Vec<String>, Vec<String>)> = None;
        let flush = |out: &mut String, run: &mut Option<(Option<u64>, Option<u64>, Vec<String>, Vec<String>)>| {
            if let Some((la, ra, l, r)) = run.take() {
                let f = |a: Option<u64>| a.map_or("-".to_string(), |a| format!("0x{a:08X}"));
                let n = l.len().max(r.len());
                let show = |v: &Vec<String>| if v.len() > 32 { format!("{} …", v[..32].join(" ")) } else { v.join(" ") };
                out.push_str(&format!("{}  {}  {}  {}  {}\n", f(la), f(ra), n, show(&l), show(&r)));
            }
        };
        for &i in &self.diff_rows {
            let row = self.row(i);
            for (k, (x, y)) in row.cells.iter().enumerate() {
                let la = row.la.map(|a| a + k as u64);
                let ra = row.ra.map(|a| a + k as u64);
                if x == y {
                    flush(&mut out, &mut run);
                    continue;
                }
                let hx = |v: &Option<u8>| v.map_or("--".to_string(), |b| format!("{b:02X}"));
                match &mut run {
                    Some((_, _, l, r)) => {
                        l.push(hx(x));
                        r.push(hx(y));
                    }
                    None => run = Some((la, ra, vec![hx(x)], vec![hx(y)])),
                }
            }
            flush(&mut out, &mut run);
        }
        flush(&mut out, &mut run);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("qccom-hex-{}-{name}", std::process::id()));
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn offset_and_aligned() {
        let a: Vec<u8> = (0..64u8).collect();
        let mut b = a.clone();
        b.insert(10, 0xEE);
        let pa = tmp("a.bin", &a);
        let pb = tmp("b.bin", &b);
        let off = open(&pa, &pb, false).unwrap();
        assert!(off.diff_bytes > 50);
        let al = open(&pa, &pb, true).unwrap();
        assert_eq!(al.diff_bytes, 1);
        assert_eq!(al.blocks().len(), 1);
        assert!(al.report().contains("--  EE"));
    }

    #[test]
    fn hex_by_address() {
        // 同样的数据，记录顺序不同
        let h1 = ":0400000001020304F2\n:0400100005060708CA\n:00000001FF\n";
        let h2 = ":0400100005060708CA\n:0400000001020304F2\n:00000001FF\n";
        let pa = tmp("a.hex", h1.as_bytes());
        let pb = tmp("b.hex", h2.as_bytes());
        let c = open(&pa, &pb, true).unwrap();
        assert_eq!(c.diff_bytes, 0);
        assert_eq!(c.kind_l, "Intel HEX");
        let h3 = ":04000000010203995D\n:00000001FF\n";
        let pc = tmp("c.hex", h3.as_bytes());
        let c = open(&pa, &pc, true).unwrap();
        // 0x0003 不同，0x0010..0x0014 只在左边
        assert_eq!(c.diff_bytes, 5);
    }
}

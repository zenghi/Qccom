//! 栈用量分析：读 GCC -fstack-usage 生成的 .su 文件，调用关系来自 -fcallgraph-info 的 .ci 文件，
//! 或者直接扫 Cortex-M ELF 里的 BL 指令。算出每个入口（main、任务函数、中断）的最大栈深度和最深调用链。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::binview::parse;

#[derive(Clone, Debug, Default)]
pub struct Func {
    pub name: String,
    pub file: String,
    pub line: u32,
    pub own: u64,
    /// static / dynamic / dynamic,bounded
    pub qual: String,
}

impl Func {
    pub fn dynamic(&self) -> bool {
        self.qual.starts_with("dynamic") && !self.qual.contains("bounded")
    }
}

#[derive(Default)]
pub struct Graph {
    pub funcs: HashMap<String, Func>,
    pub calls: HashMap<String, Vec<String>>,
    /// 有函数指针调用的函数
    pub indirect: HashSet<String>,
    pub su_files: usize,
    pub ci_files: usize,
    /// 调用关系来自 ELF
    pub from_elf: bool,
}

/// 解析 .su 的一行：file.c:12:5:main<TAB>48<TAB>static
pub fn parse_su_line(l: &str) -> Option<Func> {
    let mut it = l.split('\t');
    let loc = it.next()?;
    let own = it.next()?.trim().parse().ok()?;
    let qual = it.next().unwrap_or("static").trim().to_string();
    let parts: Vec<&str> = loc.split(':').collect();
    let num = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    // 从左往右找第一个 “行号:列号” 或者只有行号（老版本 GCC）
    let i = (1..parts.len()).find(|&i| num(parts[i]))?;
    let name_at = if i + 1 < parts.len() && num(parts[i + 1]) { i + 2 } else { i + 1 };
    let name = parts.get(name_at..)?.join(":");
    if name.is_empty() {
        return None;
    }
    Some(Func {
        name,
        file: parts[..i].join(":"),
        line: parts[i].parse().unwrap_or(0),
        own,
        qual,
    })
}

fn quoted<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// -fcallgraph-info=su 生成的 VCG 文件
pub fn parse_ci(text: &str, g: &mut Graph) {
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with("node:") {
            let (Some(title), Some(label)) = (quoted(l, "title:"), quoted(l, "label:")) else { continue };
            let parts: Vec<&str> = label.split("\\n").collect();
            let name = parts.first().copied().unwrap_or(title).to_string();
            // label 里有 “48 bytes (static)”
            if let Some(p) = parts.iter().find(|p| p.contains(" bytes (")) {
                let own = p.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
                let qual = p.split('(').nth(1).unwrap_or("static").trim_end_matches(')').to_string();
                let loc = parts.get(1).copied().unwrap_or("");
                let mut lp = loc.rsplitn(3, ':');
                let (_, line, file) = (lp.next(), lp.next(), lp.next());
                g.funcs.entry(title.to_string()).or_insert(Func {
                    name: name.clone(),
                    file: file.unwrap_or("").to_string(),
                    line: line.and_then(|x| x.parse().ok()).unwrap_or(0),
                    own,
                    qual,
                });
            }
        } else if l.starts_with("edge:") {
            let (Some(a), Some(b)) = (quoted(l, "sourcename:"), quoted(l, "targetname:")) else { continue };
            if b == "__indirect_call" {
                g.indirect.insert(a.to_string());
                continue;
            }
            let v = g.calls.entry(a.to_string()).or_default();
            if !v.iter().any(|x| x == b) {
                v.push(b.to_string());
            }
        }
    }
}

/// 递归找文件夹里的 .su 和 .ci（最多 2 万个文件）
pub fn load_dir(dir: &Path) -> Graph {
    let mut g = Graph::default();
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            seen += 1;
            if seen > 20_000 {
                return g;
            }
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !name.starts_with('.') && name != "node_modules" {
                    stack.push(p);
                }
                continue;
            }
            match p.extension().and_then(|e| e.to_str()) {
                Some("su") => {
                    g.su_files += 1;
                    for f in std::fs::read_to_string(&p).unwrap_or_default().lines().filter_map(parse_su_line) {
                        g.funcs.insert(f.name.clone(), f);
                    }
                }
                Some("ci") => {
                    g.ci_files += 1;
                    parse_ci(&std::fs::read_to_string(&p).unwrap_or_default(), &mut g);
                }
                _ => {}
            }
        }
    }
    g
}

/// 解码 Thumb-2 的 BL / B.W：返回 (目标地址, 是否 BL)
fn thumb_branch(pc: u64, h1: u16, h2: u16) -> Option<(u64, bool)> {
    if h1 & 0xF800 != 0xF000 {
        return None;
    }
    let bl = h2 & 0xD000 == 0xD000;
    let bw = h2 & 0xD000 == 0x9000;
    if !bl && !bw {
        return None;
    }
    let s = ((h1 >> 10) & 1) as u32;
    let j1 = ((h2 >> 13) & 1) as u32;
    let j2 = ((h2 >> 11) & 1) as u32;
    let i1 = 1 ^ (j1 ^ s);
    let i2 = 1 ^ (j2 ^ s);
    let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (((h1 & 0x3FF) as u32) << 12) | (((h2 & 0x7FF) as u32) << 1);
    let off = ((imm << 7) as i32) >> 7;
    Some(((pc as i64 + 4 + off as i64) as u64, bl))
}

/// 从 Cortex-M ELF 里扫 BL / 尾调用 B.W，补上调用关系
pub fn calls_from_elf(b: &[u8], g: &mut Graph) -> Result<usize, String> {
    let e = parse::parse_elf(b)?;
    if e.machine != 40 {
        return Err(format!("{} (Cortex-M only)", parse::machine_name(e.machine)));
    }
    let funcs: Vec<&parse::ESym> = e.syms.iter().filter(|s| s.kind == 2 && s.size > 0 && s.value & 1 == 1).collect();
    let by_addr: HashMap<u64, &str> = funcs.iter().map(|s| (s.value & !1, s.name.as_str())).collect();
    let mut n = 0;
    for f in &funcs {
        let a = f.value & !1;
        let Some(code) = e.read(b, a, f.size as usize) else { continue };
        let hw = |i: usize| u16::from_le_bytes([code[i], code[i + 1]]);
        let mut i = 0;
        while i + 4 <= code.len() {
            let h1 = hw(i);
            if let Some((t, _)) = thumb_branch(a + i as u64, h1, hw(i + 2)) {
                if let Some(callee) = by_addr.get(&t) {
                    if *callee != f.name {
                        let v = g.calls.entry(f.name.clone()).or_default();
                        if !v.iter().any(|x| x == callee) {
                            v.push(callee.to_string());
                            n += 1;
                        }
                    }
                }
                i += 4;
                continue;
            }
            // BLX Rm：函数指针
            if h1 & 0xFF87 == 0x4780 {
                g.indirect.insert(f.name.clone());
            }
            // 32 位指令的前半个字
            i += if h1 >> 11 >= 0x1D { 4 } else { 2 };
        }
    }
    g.from_elf = true;
    Ok(n)
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub total: u64,
    /// 最深的那条调用链
    pub chain: Vec<String>,
    pub recursive: bool,
    /// 链上有函数指针调用（实际可能更深）
    pub indirect: bool,
    /// 链上有不知道栈大小的函数（库函数等）
    pub unknown: Vec<String>,
    pub dynamic: bool,
}

pub fn is_isr(name: &str) -> bool {
    name.ends_with("_IRQHandler") || name.ends_with("_Handler") || name.ends_with("_IRQn") || name.ends_with("_isr") || name.ends_with("_ISR")
}

struct Walk<'a> {
    g: &'a Graph,
    memo: HashMap<String, Entry>,
    on: Vec<String>,
}

impl Walk<'_> {
    fn go(&mut self, f: &str) -> Entry {
        if let Some(e) = self.memo.get(f) {
            return e.clone();
        }
        let func = self.g.funcs.get(f);
        let own = func.map_or(0, |x| x.own);
        let mut best: Option<Entry> = None;
        let (mut rec, mut ind, mut unk, mut dynm) = (false, self.g.indirect.contains(f), vec![], func.is_some_and(|x| x.dynamic()));
        if func.is_none() {
            unk.push(f.to_string());
        }
        self.on.push(f.to_string());
        for c in self.g.calls.get(f).map(|v| v.as_slice()).unwrap_or(&[]) {
            if self.on.iter().any(|x| x == c) {
                rec = true;
                continue;
            }
            let e = self.go(c);
            rec |= e.recursive;
            ind |= e.indirect;
            dynm |= e.dynamic;
            for u in &e.unknown {
                if !unk.contains(u) && unk.len() < 20 {
                    unk.push(u.clone());
                }
            }
            if best.as_ref().is_none_or(|b| e.total > b.total) {
                best = Some(e);
            }
        }
        self.on.pop();
        let mut chain = vec![f.to_string()];
        let mut total = own;
        if let Some(b) = best {
            total += b.total;
            chain.extend(b.chain);
        }
        let e = Entry {
            name: f.to_string(),
            total,
            chain,
            recursive: rec,
            indirect: ind,
            unknown: unk,
            dynamic: dynm,
        };
        // 有递归时结果和进入路径有关，不缓存
        if !rec {
            self.memo.insert(f.to_string(), e.clone());
        }
        e
    }
}

/// 入口：没人调用的函数 + main + 中断处理函数，按最大栈深度排序
pub fn analyze(g: &Graph) -> Vec<Entry> {
    // 自己调自己不算被调用，不然递归函数就不是入口了
    let called: HashSet<&str> = g.calls.iter().flat_map(|(k, v)| v.iter().filter(move |c| *c != k)).map(|s| s.as_str()).collect();
    let mut roots: Vec<&str> = g
        .funcs
        .keys()
        .chain(g.calls.keys())
        .map(|s| s.as_str())
        .filter(|n| !called.contains(n) || *n == "main" || is_isr(n))
        .collect();
    roots.sort();
    roots.dedup();
    let mut w = Walk { g, memo: HashMap::new(), on: vec![] };
    let mut out: Vec<Entry> = roots.into_iter().map(|r| w.go(r)).collect();
    out.sort_by(|a, b| b.total.cmp(&a.total).then(a.name.cmp(&b.name)));
    out
}

/// 某个函数往下的最大深度（给 “查看调用链” 用）
pub fn entry_of(g: &Graph, f: &str) -> Entry {
    Walk { g, memo: HashMap::new(), on: vec![] }.go(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn su_lines() {
        let f = parse_su_line("Core/Src/main.c:68:5:main\t24\tstatic").unwrap();
        assert_eq!((f.name.as_str(), f.file.as_str(), f.line, f.own), ("main", "Core/Src/main.c", 68, 24));
        let f = parse_su_line("C:/fw/app.c:12:6:foo\t8\tdynamic,bounded").unwrap();
        assert_eq!(f.file, "C:/fw/app.c");
        assert!(!f.dynamic());
        let f = parse_su_line("x.cpp:3:6:void ns::bar(int)\t16\tdynamic").unwrap();
        assert_eq!(f.name, "void ns::bar(int)");
        assert!(f.dynamic());
        assert!(parse_su_line("garbage").is_none());
    }

    #[test]
    fn ci_and_depth() {
        let ci = r#"graph: { title: "main.c"
node: { title: "main" label: "main\nmain.c:5:5\n16 bytes (static)\n0 dynamic objects" }
node: { title: "a" label: "a\nmain.c:9:6\n32 bytes (static)" }
node: { title: "b" label: "b\nmain.c:12:6\n8 bytes (static)" }
node: { title: "r" label: "r\nmain.c:15:6\n4 bytes (static)" }
node: { title: "USART1_IRQHandler" label: "USART1_IRQHandler\nit.c:1:6\n8 bytes (static)" }
edge: { sourcename: "main" targetname: "a" label: "main.c:6:3" }
edge: { sourcename: "main" targetname: "b" label: "main.c:7:3" }
edge: { sourcename: "a" targetname: "b" label: "main.c:10:3" }
edge: { sourcename: "b" targetname: "printf" label: "main.c:13:3" }
edge: { sourcename: "r" targetname: "r" label: "main.c:16:3" }
edge: { sourcename: "USART1_IRQHandler" targetname: "b" }
edge: { sourcename: "USART1_IRQHandler" targetname: "__indirect_call" }
}"#;
        let mut g = Graph::default();
        parse_ci(ci, &mut g);
        assert_eq!(g.funcs["a"].own, 32);
        assert_eq!(g.funcs["a"].file, "main.c");
        let es = analyze(&g);
        let main = es.iter().find(|e| e.name == "main").unwrap();
        assert_eq!(main.total, 16 + 32 + 8);
        assert_eq!(main.chain, vec!["main", "a", "b", "printf"]);
        assert_eq!(main.unknown, vec!["printf"]);
        let r = es.iter().find(|e| e.name == "r").unwrap();
        assert!(r.recursive);
        let isr = es.iter().find(|e| e.name == "USART1_IRQHandler").unwrap();
        assert!(isr.indirect);
        assert_eq!(isr.total, 16);
        assert!(!es.iter().any(|e| e.name == "a"));
    }

    #[test]
    fn thumb_bl() {
        // BL 向后 0x100：pc=0x1000，目标 0x1104
        let (t, bl) = thumb_branch(0x1000, 0xF000, 0xF880).unwrap();
        assert_eq!((t, bl), (0x1104, true));
        // BL 向前 4：F7FF FFFC → pc + 4 - 8
        let (t, _) = thumb_branch(0x2000, 0xF7FF, 0xFFFC).unwrap();
        assert_eq!(t, 0x2000 + 4 - 8);
        assert!(thumb_branch(0, 0x4770, 0).is_none());
    }
}

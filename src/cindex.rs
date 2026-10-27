//! 工作区符号索引：后台扫描整个文件夹，找出函数、宏、结构体、枚举、typedef、全局变量的定义位置。
//!
//! 不是编译器，用的是"够用"的启发式规则，但对嵌入式 C 工程（HAL 库、寄存器宏）很实用：
//! 跳转定义、悬浮看宏展开和计算后的数值、查找引用、文件大纲、Ctrl+P 快速打开。

use crate::i18n::t;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::highlight::{is_ident, is_ident_start, Lang};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Func,
    Proto,
    Macro,
    Struct,
    Enum,
    EnumConst,
    Typedef,
    Var,
    Class,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Func => t("函数"),
            Kind::Proto => t("声明"),
            Kind::Macro => t("宏"),
            Kind::Struct => t("结构体"),
            Kind::Enum => t("枚举"),
            Kind::EnumConst => t("枚举值"),
            Kind::Typedef => t("类型"),
            Kind::Var => t("变量"),
            Kind::Class => t("类"),
        }
    }
    /// 跳转时优先级：定义 > 声明
    fn rank(self) -> u8 {
        match self {
            Kind::Proto => 1,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sym {
    pub name: String,
    pub kind: Kind,
    pub file: u32,
    /// 从 0 开始
    pub line: u32,
    /// 定义所在的那一行（或几行）原文
    pub sig: String,
    /// 宏的参数（函数式宏）和值
    pub params: Option<String>,
    pub value: Option<String>,
}

#[derive(Default)]
pub struct Index {
    pub root: PathBuf,
    /// 工作区里所有文件（相对路径），给 Ctrl+P 用
    pub files: Vec<PathBuf>,
    pub syms: Vec<Sym>,
    pub by_name: HashMap<String, Vec<u32>>,
    /// syms 里引用的文件路径表
    pub sym_files: Vec<PathBuf>,
}

pub struct Progress {
    pub files: AtomicUsize,
    pub cancel: AtomicBool,
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    ".idea",
    ".vscode",
    ".vs",
    "dist",
    ".cache",
    ".gradle",
    "venv",
    ".venv",
];
const MAX_FILES: usize = 60_000;
const MAX_PARSE: u64 = 2 * 1024 * 1024;

pub fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

pub fn build(root: &Path, prog: &Progress) -> Index {
    let mut idx = Index {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if prog.cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !skip_dir(&name) {
                    stack.push(p);
                }
                continue;
            }
            if idx.files.len() >= MAX_FILES {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
                idx.files.push(rel.to_path_buf());
            }
            let lang = Lang::from_path(&p);
            if !lang.is_code() || lang == Lang::Make || lang == Lang::Shell {
                continue;
            }
            if e.metadata().map(|m| m.len() > MAX_PARSE).unwrap_or(true) {
                continue;
            }
            let Some(text) = read_text(&p) else { continue };
            let fi = idx.sym_files.len() as u32;
            let before = idx.syms.len();
            if lang == Lang::C || lang == Lang::Asm {
                parse_c(&text, fi, &mut idx.syms);
            } else {
                parse_generic(&text, fi, &mut idx.syms);
            }
            if idx.syms.len() > before {
                idx.sym_files.push(p);
            }
            prog.files.fetch_add(1, Ordering::Relaxed);
        }
    }
    idx.files.sort();
    for (i, s) in idx.syms.iter().enumerate() {
        idx.by_name.entry(s.name.clone()).or_default().push(i as u32);
    }
    idx
}

/// 读文本：UTF-8 优先，不是就按 GBK（国内很多 C 工程是 GBK 编码）；二进制文件返回 None
pub fn read_text(p: &Path) -> Option<String> {
    let bytes = fs::read(p).ok()?;
    decode(&bytes).map(|(s, _)| s)
}

/// 返回 (文本, 是否 GBK)
pub fn decode(bytes: &[u8]) -> Option<(String, bool)> {
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return None;
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => Some((s.to_string(), false)),
        Err(_) => {
            let (s, _, _) = encoding_rs::GBK.decode(bytes);
            Some((s.into_owned(), true))
        }
    }
}

// ---------------------------------------------------------------- C 解析

/// 去掉字符串、字符常量里的内容和注释，保留长度不变（方便按位置对应）
fn scrub(line: &str, in_block: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_str: Option<char> = None;
    while let Some(c) = chars.next() {
        if *in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block = false;
                out.push_str("  ");
            } else {
                out.push(' ');
            }
            continue;
        }
        if let Some(q) = in_str {
            if c == '\\' {
                chars.next();
                out.push_str("  ");
                continue;
            }
            if c == q {
                in_str = None;
                out.push(c);
            } else {
                out.push(' ');
            }
            continue;
        }
        match c {
            '/' if chars.peek() == Some(&'/') => break,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                *in_block = true;
                out.push_str("  ");
            }
            '"' | '\'' => {
                in_str = Some(c);
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn idents(s: &str) -> Vec<(usize, &str)> {
    let mut v = vec![];
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_ident_start(c) {
            let mut end = i + c.len_utf8();
            while let Some(&(j, d)) = it.peek() {
                if is_ident(d) {
                    end = j + d.len_utf8();
                    it.next();
                } else {
                    break;
                }
            }
            v.push((i, &s[i..end]));
        } else if c.is_ascii_digit() {
            while let Some(&(_, d)) = it.peek() {
                if is_ident(d) {
                    it.next();
                } else {
                    break;
                }
            }
        }
    }
    v
}

const NOT_FUNC: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "sizeof",
    "do",
    "else",
    "case",
    "defined",
    "__attribute__",
    "typeof",
    "alignof",
    "_Static_assert",
    "static_assert",
    "__declspec",
    "decltype",
];

pub fn parse_c(text: &str, fi: u32, out: &mut Vec<Sym>) {
    let raw: Vec<&str> = text.lines().collect();
    let mut in_block = false;
    let clean: Vec<String> = raw.iter().map(|l| scrub(l, &mut in_block)).collect();
    let mut depth: i32 = 0;
    let mut enum_depth: Option<i32> = None;
    // 枚举成员的下一个值（遇到无法计算的表达式就变成 None）
    let mut enum_next: Option<i128> = Some(0);
    let mut typedef_open: Option<i32> = None;
    let mut i = 0;
    let n = clean.len();
    while i < n {
        let line = clean[i].as_str();
        let t = line.trim_start();
        // ---- 预处理
        if let Some(rest) = t.strip_prefix('#') {
            let rest = rest.trim_start();
            if let Some(def) = rest.strip_prefix("define") {
                // 拼上续行
                let mut full = raw[i].trim_end().to_string();
                let mut body = def.to_string();
                let start = i;
                while body.trim_end().ends_with('\\') && i + 1 < n {
                    body = body.trim_end().trim_end_matches('\\').to_string() + " " + clean[i + 1].trim();
                    full.push('\n');
                    full.push_str(raw[i + 1].trim_end());
                    i += 1;
                }
                let body = body.trim_start();
                let name_len = body.find(|c: char| !is_ident(c)).unwrap_or(body.len());
                if name_len > 0 {
                    let name = &body[..name_len];
                    let after = &body[name_len..];
                    let (params, value) = if let Some(p) = after.strip_prefix('(') {
                        let close = p.find(')').unwrap_or(p.len());
                        (Some(p[..close].trim().to_string()), p.get(close + 1..).unwrap_or("").trim().to_string())
                    } else {
                        (None, after.trim().to_string())
                    };
                    out.push(Sym {
                        name: name.to_string(),
                        kind: Kind::Macro,
                        file: fi,
                        line: start as u32,
                        // 对齐用的连续空白压成一个，悬浮卡片里更紧凑
                        sig: clip(&full.split_whitespace().collect::<Vec<_>>().join(" "), 600),
                        params,
                        value: Some(clip(&value, 600)),
                    });
                }
            }
            i += 1;
            continue;
        }
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        // ---- 枚举成员
        if let Some(d) = enum_depth {
            if depth == d {
                for part in line.split(',') {
                    let p = part.trim().trim_start_matches('{').trim();
                    if let Some((_, w)) = idents(p).first() {
                        if p.starts_with(w) {
                            let value = enum_value(p, &mut enum_next);
                            out.push(Sym {
                                name: w.to_string(),
                                kind: Kind::EnumConst,
                                file: fi,
                                line: i as u32,
                                sig: raw[i].trim().to_string(),
                                params: None,
                                value,
                            });
                        }
                    }
                }
            }
        }

        if depth == 0 && !t.is_empty() {
            let ids = idents(t);
            let first = ids.first().map(|x| x.1).unwrap_or("");
            // ---- struct / union / enum / class 定义
            for (k, kw) in [(Kind::Struct, "struct"), (Kind::Struct, "union"), (Kind::Enum, "enum"), (Kind::Class, "class")] {
                if let Some(pos) = ids.iter().position(|x| x.1 == kw) {
                    let next = ids.get(pos + 1).map(|x| x.1);
                    let has_body = t.contains('{') || clean.get(i + 1).is_some_and(|l| l.trim_start().starts_with('{'));
                    let brace = t.find('{').unwrap_or(t.len());
                    // typedef enum { A, B } x; 这种匿名的，enum 后面的第一个词是成员不是名字
                    if let Some(name) = next.filter(|_| ids[pos + 1].0 < brace) {
                        if has_body && !t[..t.find('{').unwrap_or(t.len())].contains(';') {
                            out.push(Sym {
                                name: name.to_string(),
                                kind: k,
                                file: fi,
                                line: i as u32,
                                sig: raw[i].trim().to_string(),
                                params: None,
                                value: None,
                            });
                        }
                    }
                    if kw == "enum" && has_body {
                        enum_next = Some(0);
                        if let Some(ob) = t.find('{') {
                            // 同一行里的成员：enum s { A, B = 3, C };
                            let inner = &t[ob + 1..t.rfind('}').unwrap_or(t.len())];
                            for part in inner.split(',') {
                                let p = part.trim();
                                if let Some(&(0, w)) = idents(p).first() {
                                    let value = enum_value(p, &mut enum_next);
                                    out.push(Sym {
                                        name: w.to_string(),
                                        kind: Kind::EnumConst,
                                        file: fi,
                                        line: i as u32,
                                        sig: raw[i].trim().to_string(),
                                        params: None,
                                        value,
                                    });
                                }
                            }
                        }
                        if !t.contains('}') {
                            enum_depth = Some(1);
                        }
                    }
                    break;
                }
            }
            if first == "typedef" {
                if t.contains('{') || (!t.contains(';') && clean.get(i + 1).is_some_and(|l| l.trim_start().starts_with('{'))) {
                    typedef_open = Some(0);
                } else if let Some(semi) = t.rfind(';') {
                    // typedef unsigned int u32;   typedef void (*cb_t)(int);
                    let before = &t[..semi];
                    let name = if let Some(p) = before.find("(*") {
                        idents(&before[p..]).first().map(|x| x.1)
                    } else {
                        idents(before).last().map(|x| x.1)
                    };
                    if let Some(name) = name {
                        out.push(Sym {
                            name: name.to_string(),
                            kind: Kind::Typedef,
                            file: fi,
                            line: i as u32,
                            sig: raw[i].trim().to_string(),
                            params: None,
                            value: None,
                        });
                    }
                }
            } else if !matches!(first, "extern" | "return" | "using" | "namespace" | "template" | "") {
                // ---- 函数：名字后面跟 (，参数结束后是 { 就是定义，是 ; 就是声明
                if let Some(pp) = t.find('(') {
                    let head = &t[..pp];
                    if let Some(&(_, name)) = idents(head).last() {
                        let at_end = head.trim_end().ends_with(name);
                        if at_end && !NOT_FUNC.contains(&name) && !head.contains('=') {
                            // 找到匹配的右括号后第一个有效字符（最多往下看 20 行）
                            let mut lvl = 0i32;
                            let mut found: Option<char> = None;
                            let mut started = false;
                            'scan: for (k, l) in clean.iter().enumerate().skip(i).take(20) {
                                let s = if k == i { &l[l.len() - t.len() + pp..] } else { l.as_str() };
                                for c in s.chars() {
                                    if lvl == 0 && started {
                                        if !c.is_whitespace() {
                                            // 跳过 __attribute__ 之类的修饰
                                            if c == '{' || c == ';' || c == ',' || c == '=' {
                                                found = Some(c);
                                                break 'scan;
                                            }
                                            if is_ident_start(c) || c == ')' || c == '(' {
                                                if c == '(' {
                                                    lvl += 1;
                                                }
                                                continue;
                                            }
                                        }
                                        continue;
                                    }
                                    if c == '(' {
                                        lvl += 1;
                                        started = true;
                                    } else if c == ')' {
                                        lvl -= 1;
                                    }
                                }
                            }
                            // 函数名前面得有类型（或者 GNU 风格类型在上一行）
                            let typed = idents(head).len() >= 2
                                || head.contains('*')
                                || (i > 0 && !clean[i - 1].trim().is_empty() && !clean[i - 1].trim_end().ends_with([';', '}', '{', ')']) && !clean[i - 1].trim_start().starts_with('#'));
                            match found {
                                Some('{') if typed => out.push(Sym {
                                    name: name.to_string(),
                                    kind: Kind::Func,
                                    file: fi,
                                    line: i as u32,
                                    sig: sig_lines(&raw, i),
                                    params: None,
                                    value: None,
                                }),
                                Some(';') if typed => out.push(Sym {
                                    name: name.to_string(),
                                    kind: Kind::Proto,
                                    file: fi,
                                    line: i as u32,
                                    sig: sig_lines(&raw, i),
                                    params: None,
                                    value: None,
                                }),
                                _ => {}
                            }
                        }
                    }
                } else if t.trim_end().ends_with(';') && ids.len() >= 2 && !t.contains("struct") || (t.contains('=') && ids.len() >= 2 && !t.contains('(')) {
                    // ---- 全局变量：int count = 0;  static uint8_t buf[64];
                    let cut = t.find(['=', '[', ';']).unwrap_or(t.len());
                    if let Some(&(_, name)) = idents(&t[..cut]).last() {
                        if idents(&t[..cut]).len() >= 2 && !NOT_FUNC.contains(&name) {
                            out.push(Sym {
                                name: name.to_string(),
                                kind: Kind::Var,
                                file: fi,
                                line: i as u32,
                                sig: raw[i].trim().to_string(),
                                params: None,
                                value: None,
                            });
                        }
                    }
                }
            }
        }
        // typedef struct { ... } Name;
        if typedef_open.is_some() && depth + opens - closes == 0 && closes > 0 {
            if let Some(after) = t.rsplit('}').next() {
                for &(_, w) in idents(after).iter() {
                    out.push(Sym {
                        name: w.to_string(),
                        kind: Kind::Typedef,
                        file: fi,
                        line: i as u32,
                        sig: raw[i].trim().to_string(),
                        params: None,
                        value: None,
                    });
                }
            }
            typedef_open = None;
        }
        depth = (depth + opens - closes).max(0);
        if enum_depth.is_some() && depth == 0 {
            enum_depth = None;
        }
        i += 1;
    }
}

/// "B = 0x10" → Some("16")，没写值就沿用上一个 + 1
fn enum_value(member: &str, next: &mut Option<i128>) -> Option<String> {
    let member = member.trim_end_matches(['}', ';', ' ']);
    let v = match member.split_once('=') {
        Some((_, e)) => eval(e.trim()),
        None => *next,
    };
    *next = v.map(|x| x + 1);
    v.map(|x| x.to_string())
}

fn sig_lines(raw: &[&str], i: usize) -> String {
    let mut s = String::new();
    for l in raw.iter().skip(i).take(6) {
        if !s.is_empty() {
            s.push('\n');
        }
        let cut = l.find('{').map_or(l.len(), |k| k);
        s.push_str(l[..cut].trim_end());
        if l.contains('{') || l.contains(';') || l.contains(')') && !l.trim_end().ends_with(',') {
            break;
        }
    }
    clip(s.trim(), 400)
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut k = n;
        while !s.is_char_boundary(k) {
            k -= 1;
        }
        format!("{}…", &s[..k])
    }
}

/// 其他语言：关键字后面跟名字
pub fn parse_generic(text: &str, fi: u32, out: &mut Vec<Sym>) {
    for (i, l) in text.lines().enumerate() {
        let t = l.trim_start();
        let ids = idents(t);
        for (k, &(pos, w)) in ids.iter().enumerate() {
            let kind = match w {
                "fn" | "def" | "function" | "func" => Kind::Func,
                "class" | "interface" | "trait" | "object" => Kind::Class,
                "struct" => Kind::Struct,
                "enum" => Kind::Enum,
                "type" if k == 0 || ids[0].1 == "pub" || ids[0].1 == "export" => Kind::Typedef,
                "const" | "static" if t.starts_with(w) || t.starts_with("pub") => Kind::Var,
                "macro_rules" => Kind::Macro,
                _ => continue,
            };
            // 关键字前只能是修饰词
            if !ids[..k].iter().all(|x| {
                matches!(
                    x.1,
                    "pub"
                        | "export"
                        | "async"
                        | "default"
                        | "public"
                        | "private"
                        | "static"
                        | "abstract"
                        | "final"
                        | "unsafe"
                        | "data"
                        | "open"
                        | "crate"
                        | "extern"
                        | "inline"
                        | "override"
                        | "protected"
                        | "sealed"
                )
            }) {
                break;
            }
            let rest = &t[pos + w.len()..];
            // Go 的方法：func (r *T) Name(
            let rest = if w == "func" && rest.trim_start().starts_with('(') {
                rest.find(')').map_or(rest, |p| &rest[p + 1..])
            } else {
                rest
            };
            let rest = rest.trim_start_matches(['!', ' ', '\t', '*']);
            let name_len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
            if name_len > 0 && rest.starts_with(is_ident_start) {
                let name = &rest[..name_len];
                if !matches!(name, "mut" | "self") {
                    out.push(Sym {
                        name: name.to_string(),
                        kind,
                        file: fi,
                        line: i as u32,
                        sig: clip(t.trim_end().trim_end_matches('{').trim_end(), 300),
                        params: None,
                        value: None,
                    });
                }
            }
            break;
        }
    }
}

// ---------------------------------------------------------------- 查询

impl Index {
    pub fn lookup(&self, name: &str) -> Vec<&Sym> {
        let mut v: Vec<&Sym> = self.by_name.get(name).map(|ids| ids.iter().map(|&i| &self.syms[i as usize]).collect()).unwrap_or_default();
        v.sort_by_key(|s| s.kind.rank());
        v
    }

    pub fn path_of(&self, s: &Sym) -> &Path {
        &self.sym_files[s.file as usize]
    }

    fn macro_value(&self, name: &str) -> Option<&Sym> {
        self.by_name.get(name)?.iter().map(|&i| &self.syms[i as usize]).find(|s| s.kind == Kind::Macro && s.params.is_none())
    }

    /// 宏展开：只展开无参数的宏，最多展开 12 层（防止互相引用死循环）
    pub fn expand(&self, value: &str) -> String {
        let mut cur = value.to_string();
        for _ in 0..12 {
            let mut changed = false;
            let mut next = String::with_capacity(cur.len());
            let mut last = 0;
            for (pos, w) in idents(&cur) {
                if let Some(m) = self.macro_value(w) {
                    let v = m.value.as_deref().unwrap_or("").trim();
                    if !v.is_empty() && v != w {
                        next.push_str(&cur[last..pos]);
                        next.push_str(v);
                        last = pos + w.len();
                        changed = true;
                    }
                }
            }
            next.push_str(&cur[last..]);
            if !changed || next.len() > 4000 {
                break;
            }
            cur = next;
        }
        cur.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

// ---------------------------------------------------------------- 整数表达式求值（给宏算出最终数值）

/// 能算就算：支持 + - * / % << >> & | ^ ~ ! 括号、十六进制/八进制/二进制、U/L 后缀、类型强转
pub fn eval(expr: &str) -> Option<i128> {
    let toks = lex(expr)?;
    let mut p = Parser { t: toks, i: 0 };
    let v = p.expr(0)?;
    (p.i == p.t.len()).then_some(v)
}

#[derive(Clone, Debug, PartialEq)]
enum T {
    Num(i128),
    Op(&'static str),
    L,
    R,
}

fn lex(s: &str) -> Option<Vec<T>> {
    const OPS: &[&str] = &["<<", ">>", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!"];
    let b = s.as_bytes();
    let mut v = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let lit = s[i..j].trim_end_matches(['u', 'U', 'l', 'L']);
            let n = if let Some(h) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
                i128::from_str_radix(&h.replace('_', ""), 16).ok()?
            } else if let Some(bn) = lit.strip_prefix("0b").or_else(|| lit.strip_prefix("0B")) {
                i128::from_str_radix(bn, 2).ok()?
            } else if lit.len() > 1 && lit.starts_with('0') {
                i128::from_str_radix(&lit[1..], 8).ok()?
            } else {
                lit.parse().ok()?
            };
            v.push(T::Num(n));
            i = j;
            continue;
        }
        if c == b'(' {
            // 类型强转 (uint32_t) (volatile GPIO_TypeDef *)：括号里全是标识符和 * 就跳过
            let close = s[i..].find(')')? + i;
            let inner = &s[i + 1..close];
            if !inner.trim().is_empty() && inner.chars().all(|c| is_ident(c) || c == '*' || c.is_whitespace()) && inner.chars().any(is_ident_start) {
                i = close + 1;
                continue;
            }
            v.push(T::L);
            i += 1;
            continue;
        }
        if c == b')' {
            v.push(T::R);
            i += 1;
            continue;
        }
        let op = OPS.iter().find(|o| s[i..].starts_with(**o))?;
        v.push(T::Op(op));
        i += op.len();
    }
    Some(v)
}

struct Parser {
    t: Vec<T>,
    i: usize,
}

impl Parser {
    fn prec(op: &str) -> u8 {
        match op {
            "|" => 1,
            "^" => 2,
            "&" => 3,
            "<<" | ">>" => 4,
            "+" | "-" => 5,
            "*" | "/" | "%" => 6,
            _ => 0,
        }
    }
    fn atom(&mut self) -> Option<i128> {
        let t = self.t.get(self.i)?.clone();
        self.i += 1;
        match t {
            T::Num(n) => Some(n),
            T::L => {
                let v = self.expr(0)?;
                (self.t.get(self.i) == Some(&T::R)).then(|| self.i += 1)?;
                Some(v)
            }
            T::Op("-") => Some(-self.atom()?),
            T::Op("+") => self.atom(),
            T::Op("~") => Some(!self.atom()? & 0xffff_ffff_ffff_ffff),
            T::Op("!") => Some((self.atom()? == 0) as i128),
            _ => None,
        }
    }
    fn expr(&mut self, min: u8) -> Option<i128> {
        let mut lhs = self.atom()?;
        while let Some(T::Op(op)) = self.t.get(self.i).cloned() {
            let p = Self::prec(op);
            if p == 0 || p <= min {
                break;
            }
            self.i += 1;
            let rhs = self.expr(p)?;
            lhs = match op {
                "+" => lhs.checked_add(rhs)?,
                "-" => lhs.checked_sub(rhs)?,
                "*" => lhs.checked_mul(rhs)?,
                "/" => lhs.checked_div(rhs)?,
                "%" => lhs.checked_rem(rhs)?,
                "<<" => lhs.checked_shl(rhs.try_into().ok()?)?,
                ">>" => lhs.checked_shr(rhs.try_into().ok()?)?,
                "&" => lhs & rhs,
                "|" => lhs | rhs,
                "^" => lhs ^ rhs,
                _ => return None,
            };
        }
        Some(lhs)
    }
}

// ---------------------------------------------------------------- 全局搜索

#[derive(Clone)]
pub struct Hit {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
}

pub struct SearchOpts {
    pub case: bool,
    pub word: bool,
}

pub fn search(root: &Path, files: &[PathBuf], q: &str, o: &SearchOpts, cancel: &AtomicBool, limit: usize) -> Vec<Hit> {
    let mut out = vec![];
    if q.is_empty() {
        return out;
    }
    let needle = if o.case { q.to_string() } else { q.to_lowercase() };
    for rel in files {
        if cancel.load(Ordering::Relaxed) || out.len() >= limit {
            break;
        }
        let p = root.join(rel);
        if fs::metadata(&p).map(|m| m.len() > MAX_PARSE * 2).unwrap_or(true) {
            continue;
        }
        let Some(text) = read_text(&p) else { continue };
        for (i, line) in text.lines().enumerate() {
            let hay = if o.case { std::borrow::Cow::Borrowed(line) } else { std::borrow::Cow::Owned(line.to_lowercase()) };
            let mut from = 0;
            while let Some(k) = hay[from..].find(&needle) {
                let at = from + k;
                let end = at + needle.len();
                let ok = !o.word || (!hay[..at].chars().next_back().is_some_and(is_ident) && !hay[end..].chars().next().is_some_and(is_ident));
                if ok {
                    out.push(Hit {
                        path: rel.clone(),
                        line: i as u32,
                        text: clip(line.trim(), 200),
                    });
                    break;
                }
                from = end;
            }
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Ctrl+P 模糊匹配打分：字符按顺序出现即可，连续、在文件名里、在词首的加分
pub fn fuzzy(path: &str, q: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let p: Vec<char> = path.to_lowercase().chars().collect();
    let name_start = path.rfind(['/', '\\']).map_or(0, |i| path[..=i].chars().count());
    let mut score = 0;
    let mut pi = 0;
    let mut last: Option<usize> = None;
    for qc in q.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (pi..p.len()).find(|&k| p[k] == qc)?;
        score += 1;
        if last == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        if found >= name_start {
            score += 3;
        }
        if found == 0 || matches!(p[found - 1], '/' | '\\' | '_' | '-' | '.') {
            score += 4;
        }
        last = Some(found);
        pi = found + 1;
    }
    Some(score * 100 - p.len() as i32)
}

pub fn shared_progress() -> Arc<Progress> {
    Arc::new(Progress {
        files: AtomicUsize::new(0),
        cancel: AtomicBool::new(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
#include "stm32.h"
#define PERIPH_BASE   0x40000000UL
#define AHB1PERIPH_BASE (PERIPH_BASE + 0x00020000UL)
#define GPIOA_BASE    (AHB1PERIPH_BASE + 0x0000UL)
#define GPIOA         ((GPIO_TypeDef *) GPIOA_BASE)
#define MAX(a, b) ((a) > (b) ? \
                   (a) : (b))
typedef struct {
    volatile uint32_t MODER;
} GPIO_TypeDef;
typedef unsigned int u32;
typedef void (*irq_cb_t)(int);
enum state { IDLE, RUNNING = 3, DONE };
struct pkt {
    int len;
};
static uint8_t rx_buf[64];
int counter = 0;
void led_init(void);
static void
led_toggle(int pin)
{
    if (pin) { return; }
}
int main(void) {
    led_init();
    return 0;
}
"#;

    #[test]
    fn c_symbols() {
        let mut v = vec![];
        parse_c(SRC, 0, &mut v);
        let has = |n: &str, k: Kind| v.iter().any(|s| s.name == n && s.kind == k);
        assert!(has("PERIPH_BASE", Kind::Macro));
        assert!(has("MAX", Kind::Macro));
        assert!(has("GPIO_TypeDef", Kind::Typedef));
        assert!(has("u32", Kind::Typedef));
        assert!(has("irq_cb_t", Kind::Typedef));
        assert!(has("state", Kind::Enum));
        assert!(has("RUNNING", Kind::EnumConst));
        assert!(has("DONE", Kind::EnumConst));
        assert!(has("pkt", Kind::Struct));
        assert!(has("rx_buf", Kind::Var));
        assert!(has("counter", Kind::Var));
        assert!(has("led_init", Kind::Proto));
        assert!(has("led_toggle", Kind::Func));
        assert!(has("main", Kind::Func));
        assert!(!v.iter().any(|s| s.name == "if" || s.name == "len"));
        let m = v.iter().find(|s| s.name == "MAX").unwrap();
        assert_eq!(m.params.as_deref(), Some("a, b"));

        // 匿名 typedef enum：成员带值，且不把第一个成员当成枚举名
        let mut v = vec![];
        parse_c("typedef enum { ST_IDLE, ST_RUN, ST_ERROR = 0x10, ST_X } state_t;\nenum m {\n  A = 3,\n  B,\n};", 0, &mut v);
        let val = |n: &str| v.iter().find(|s| s.name == n && s.kind == Kind::EnumConst).and_then(|s| s.value.clone());
        assert!(!v.iter().any(|s| s.name == "ST_IDLE" && s.kind == Kind::Enum));
        assert_eq!(val("ST_RUN").as_deref(), Some("1"));
        assert_eq!(val("ST_X").as_deref(), Some("17"));
        assert_eq!(val("B").as_deref(), Some("4"));
    }

    #[test]
    fn macro_expand_and_eval() {
        let mut idx = Index::default();
        parse_c(SRC, 0, &mut idx.syms);
        for (i, s) in idx.syms.iter().enumerate() {
            idx.by_name.entry(s.name.clone()).or_default().push(i as u32);
        }
        let e = idx.expand("GPIOA");
        assert_eq!(eval(&e), Some(0x4002_0000));
        assert_eq!(eval("(1UL << 5) | 0x3"), Some(0x23));
        assert_eq!(eval("~0x0F & 0xFF"), Some(0xF0));
        assert_eq!(eval("foo + 1"), None);
    }

    #[test]
    fn generic_symbols() {
        let mut v = vec![];
        parse_generic("pub fn run() {}\nstruct A;\ndef hello(x):\nclass Foo:\nfunc (s *Srv) Serve() {}\nexport function go() {}", 0, &mut v);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["run", "A", "hello", "Foo", "Serve", "go"]);
    }

    #[test]
    fn fuzzy_rank() {
        let a = fuzzy("src/drivers/uart.c", "uart").unwrap();
        let b = fuzzy("src/usb/audio_rt.c", "uart").unwrap();
        assert!(a > b);
        assert!(fuzzy("main.c", "xyz").is_none());
    }
}

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
            if !lang.is_code() || matches!(lang, Lang::Make | Lang::Shell | Lang::Kconfig | Lang::Dts | Lang::Ld) {
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
    const OPS: &[&str] = &["<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "?", ":"];
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
            "?" => 1,
            "||" => 2,
            "&&" => 3,
            "|" => 4,
            "^" => 5,
            "&" => 6,
            "==" | "!=" => 7,
            "<" | ">" | "<=" | ">=" => 8,
            "<<" | ">>" => 9,
            "+" | "-" => 10,
            "*" | "/" | "%" => 11,
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
            if op == "?" {
                // 条件运算：右结合
                let a = self.expr(0)?;
                (self.t.get(self.i) == Some(&T::Op(":"))).then(|| self.i += 1)?;
                let b = self.expr(p - 1)?;
                lhs = if lhs != 0 { a } else { b };
                continue;
            }
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
                "&&" => (lhs != 0 && rhs != 0) as i128,
                "||" => (lhs != 0 || rhs != 0) as i128,
                "==" => (lhs == rhs) as i128,
                "!=" => (lhs != rhs) as i128,
                "<" => (lhs < rhs) as i128,
                ">" => (lhs > rhs) as i128,
                "<=" => (lhs <= rhs) as i128,
                ">=" => (lhs >= rhs) as i128,
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
    /// 去掉首尾空白、截断后的这一行
    pub text: String,
    /// 第一处匹配在 text 里的字节范围
    pub m: (usize, usize),
}

/// 全工程搜索：每行最多记一条（第一处匹配）。一次性返回全部结果（给调用关系、TODO 这类后台用）
pub fn search(root: &Path, files: &[PathBuf], re: &regex_lite::Regex, cancel: &AtomicBool, limit: usize) -> Vec<Hit> {
    let job = SearchJob {
        root: root.to_path_buf(),
        files: files.to_vec(),
        re: re.clone(),
        lit: None,
        overrides: HashMap::new(),
        limit,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let done = AtomicUsize::new(0);
    job.run(cancel, &done, &tx);
    drop(tx);
    let mut v: Vec<(usize, Vec<Hit>)> = rx.into_iter().collect();
    v.sort_by_key(|(i, _)| *i);
    let mut out: Vec<Hit> = v.into_iter().flat_map(|(_, h)| h).collect();
    out.truncate(limit);
    out
}

/// 不用正则也能快速判断「这个文件里有没有」的字面量
#[derive(Clone)]
pub struct Lit {
    pub text: String,
    pub case: bool,
}

impl Lit {
    /// 非正则查询直接用查询本身做预筛
    pub fn of(pat: &crate::pattern::Pat) -> Option<Lit> {
        (!pat.regex && !pat.q.is_empty() && (pat.case || pat.q.is_ascii())).then(|| Lit {
            text: pat.q.clone(),
            case: pat.case,
        })
    }

    fn found_in(&self, hay: &str) -> bool {
        let (h, n) = (hay.as_bytes(), self.text.as_bytes());
        if self.case {
            return memchr::memmem::find(h, n).is_some();
        }
        // 不区分大小写：按首字母的大小写两种找候选位置，再逐字节比较
        let (lo, up) = (n[0].to_ascii_lowercase(), n[0].to_ascii_uppercase());
        memchr::memchr2_iter(lo, up, h).any(|i| h.len() - i >= n.len() && h[i..i + n.len()].eq_ignore_ascii_case(n))
    }
}

/// 一次全工程搜索的参数
pub struct SearchJob {
    pub root: PathBuf,
    /// 相对工程根的路径（已经按排除规则过滤好）
    pub files: Vec<PathBuf>,
    pub re: regex_lite::Regex,
    pub lit: Option<Lit>,
    /// 编辑器里已打开、有未保存修改的文件：搜编辑器里的内容
    pub overrides: HashMap<PathBuf, Arc<str>>,
    pub limit: usize,
}

impl SearchJob {
    /// 多线程搜索。每搜完一个有结果的文件就发一批 (文件序号, 结果)；`done` 记已搜完的文件数
    pub fn run(&self, cancel: &AtomicBool, done: &AtomicUsize, tx: &std::sync::mpsc::Sender<(usize, Vec<Hit>)>) {
        let next = AtomicUsize::new(0);
        let found = AtomicUsize::new(0);
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
        std::thread::scope(|sc| {
            for _ in 0..n {
                let tx = tx.clone();
                sc.spawn(|| {
                    let tx = tx;
                    loop {
                        if cancel.load(Ordering::Relaxed) || found.load(Ordering::Relaxed) >= self.limit {
                            break;
                        }
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = self.files.get(i) else { break };
                        let text = match self.overrides.get(rel) {
                            Some(t) => Some(t.clone()),
                            None => cached_text(&self.root.join(rel)),
                        };
                        done.fetch_add(1, Ordering::Relaxed);
                        let Some(text) = text else { continue };
                        if self.lit.as_ref().is_some_and(|l| !l.found_in(&text)) {
                            continue;
                        }
                        let hits = hits_in(rel, &text, &self.re, self.limit);
                        if !hits.is_empty() {
                            found.fetch_add(hits.len(), Ordering::Relaxed);
                            if tx.send((i, hits)).is_err() {
                                break;
                            }
                        }
                    }
                });
            }
        });
    }
}

/// 在整段文字上跑正则，再换算成行（每行只记第一处）
fn hits_in(rel: &Path, text: &str, re: &regex_lite::Regex, limit: usize) -> Vec<Hit> {
    let mut out = vec![];
    let bytes = text.as_bytes();
    let (mut line, mut line_start, mut scanned) = (0u32, 0usize, 0usize);
    let mut last_line: Option<u32> = None;
    for m in re.find_iter(text) {
        if m.is_empty() {
            continue;
        }
        // 数到匹配所在行
        for nl in memchr::memchr_iter(b'\n', &bytes[scanned..m.start()]) {
            line += 1;
            line_start = scanned + nl + 1;
        }
        scanned = m.start();
        if last_line == Some(line) {
            continue;
        }
        last_line = Some(line);
        let line_end = memchr::memchr(b'\n', &bytes[line_start..]).map_or(text.len(), |e| line_start + e);
        let full = text[line_start..line_end].trim_end_matches('\r');
        let lead = full.len() - full.trim_start().len();
        let shown = clip(full.trim(), 200);
        let a = (m.start() - line_start).saturating_sub(lead).min(shown.len());
        let mut b = (m.end().min(line_end) - line_start).saturating_sub(lead).min(shown.len());
        while !shown.is_char_boundary(b) {
            b -= 1;
        }
        let mut a = a.min(b);
        while !shown.is_char_boundary(a) {
            a -= 1;
        }
        out.push(Hit {
            path: rel.to_path_buf(),
            line,
            text: shown,
            m: (a, b),
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// 搜索用的文件内容缓存：按修改时间和大小判断是否过期，总量超过上限就清掉一半
struct CacheEnt {
    mtime: std::time::SystemTime,
    len: u64,
    text: Option<Arc<str>>,
}

static TEXT_CACHE: std::sync::LazyLock<std::sync::Mutex<(HashMap<PathBuf, CacheEnt>, usize)>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new((HashMap::new(), 0)));
const CACHE_MAX: usize = 256 * 1024 * 1024;

/// 读文件文字（二进制、超大文件返回 None），命中缓存时不碰磁盘内容
pub fn cached_text(p: &Path) -> Option<Arc<str>> {
    let md = fs::metadata(p).ok()?;
    let (mtime, len) = (md.modified().ok()?, md.len());
    if let Some(e) = TEXT_CACHE.lock().ok()?.0.get(p) {
        if e.mtime == mtime && e.len == len {
            return e.text.clone();
        }
    }
    let text: Option<Arc<str>> = if len > MAX_PARSE * 2 { None } else { read_text(p).map(Arc::from) };
    let mut g = TEXT_CACHE.lock().ok()?;
    let add = text.as_ref().map_or(0, |t| t.len());
    if g.1 + add > CACHE_MAX {
        // 简单处理：超了就整个清空（一次全工程搜索通常远小于上限）
        g.0.clear();
        g.1 = 0;
    }
    if let Some(old) = g.0.insert(p.to_path_buf(), CacheEnt { mtime, len, text: text.clone() }) {
        g.1 -= old.text.map_or(0, |t| t.len());
    }
    g.1 += add;
    text
}

/// 搜索范围：包含 / 排除规则（.gitignore 语法，逗号或换行分隔）和几个快捷开关
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Scope {
    pub include: String,
    pub exclude: String,
    /// 按工程的 .gitignore 排除
    pub gitignore: bool,
    /// 只搜 C/C++/汇编源文件和头文件
    pub only_c: bool,
    /// 排除厂商库（Drivers/、CMSIS/、Middlewares/ 等）
    pub no_vendor: bool,
    /// 排除编译产物（.o、.map、build/、Objects/ 等）
    pub no_build: bool,
    /// 只搜编辑器里已打开的文件（不存）
    #[serde(skip)]
    pub open_only: bool,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            include: String::new(),
            exclude: String::new(),
            gitignore: true,
            only_c: false,
            no_vendor: false,
            no_build: true,
            open_only: false,
        }
    }
}

pub const C_FILES: &[&str] = &["*.c", "*.h", "*.cpp", "*.hpp", "*.cc", "*.hh", "*.cxx", "*.s", "*.S", "*.asm", "*.inc"];
pub const VENDOR_DIRS: &[&str] = &[
    "Drivers/",
    "CMSIS/",
    "Middlewares/",
    "Libraries/",
    "ThirdParty/",
    "Third_Party/",
    "third_party/",
    "managed_components/",
    "**/*_HAL_Driver/",
    "**/*_StdPeriph_Driver/",
];

/// 把「a, b」或多行写法拆成一条条规则
pub fn split_globs(s: &str) -> Vec<String> {
    s.split([',', '\n', ';']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

impl Scope {
    /// 有没有任何会缩小范围的设置（.gitignore 和编译产物除外，那两项是默认的）
    pub fn narrowed(&self) -> bool {
        !self.include.trim().is_empty() || !self.exclude.trim().is_empty() || self.only_c || self.no_vendor || self.open_only
    }

    /// 按范围过滤文件列表。`open` 是已打开的文件（相对路径），open_only 时用
    pub fn filter(&self, root: &Path, files: &[PathBuf], open: &[PathBuf]) -> Vec<PathBuf> {
        use crate::history::ignore::Ignore;
        let mut ex = Ignore::default();
        if self.no_build {
            ex.extend(&Ignore::defaults());
        }
        let mut lines: Vec<String> = vec![];
        if self.no_vendor {
            lines.extend(VENDOR_DIRS.iter().map(|s| s.to_string()));
        }
        // 排除框里写 !xxx 表示「不排除」，和 .gitignore 一样
        lines.extend(split_globs(&self.exclude));
        ex.add_text("", &lines.join("\n"));
        let mut inc_lines = split_globs(&self.include);
        if self.only_c {
            inc_lines.extend(C_FILES.iter().map(|s| s.to_string()));
        }
        let mut inc = Ignore::default();
        inc.add_text("", &inc_lines.join("\n"));
        // .gitignore：根目录 + 各级子目录里的
        let mut gi = if self.gitignore { Some(Ignore::load(root)) } else { None };
        if let Some(g) = gi.as_mut() {
            let mut dirs: Vec<String> = files
                .iter()
                .filter_map(|f| f.parent())
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| d.to_string_lossy().replace('\\', "/"))
                .collect();
            dirs.sort();
            dirs.dedup();
            // 每个出现过的目录及其上级
            let mut all: std::collections::BTreeSet<String> = Default::default();
            for d in dirs {
                let mut end = 0;
                while let Some(i) = d[end..].find('/') {
                    all.insert(d[..end + i].to_string());
                    end += i + 1;
                }
                all.insert(d);
            }
            for d in all {
                g.push_dir(root, &d);
            }
        }
        let open: std::collections::HashSet<&PathBuf> = open.iter().collect();
        files
            .iter()
            .filter(|f| {
                if self.open_only && !open.contains(f) {
                    return false;
                }
                let rel = f.to_string_lossy().replace('\\', "/");
                if !inc_lines.is_empty() && !inc.is_ignored(&rel, false) {
                    return false;
                }
                if ex.is_ignored(&rel, false) {
                    return false;
                }
                !gi.as_ref().is_some_and(|g| g.is_ignored(&rel, false))
            })
            .cloned()
            .collect()
    }
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
        assert_eq!(eval("1 && (2 > 1) || 0"), Some(1));
        assert_eq!(eval("3 == 4"), Some(0));
        assert_eq!(eval("1 + 2 * 3 >= 7 && 5 != 4"), Some(1));
        assert_eq!(eval("0 ? 5 : 1 ? 6 : 7"), Some(6));
        assert_eq!(eval("!0 && 1 << 2 == 4"), Some(1));
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

#[cfg(test)]
mod bench {
    use super::*;
    /// 手动跑：QCCOM_BENCH=/path cargo test --release bench_search -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_search() {
        let Some(root) = std::env::var_os("QCCOM_BENCH").map(PathBuf::from) else { return };
        let idx = build(&root, &shared_progress());
        let pat = crate::pattern::Pat { q: "foo_special_marker".into(), case: false, word: false, regex: false };
        let re = pat.compile().unwrap();
        let never = AtomicBool::new(false);
        // 旧算法：单线程逐行
        let t = std::time::Instant::now();
        let mut n_old = 0;
        for rel in &idx.files {
            let Some(text) = read_text(&root.join(rel)) else { continue };
            for line in text.lines() {
                if re.find_iter(line).any(|m| !m.is_empty()) {
                    n_old += 1;
                }
            }
        }
        println!("old: {} hits, {:?}", n_old, t.elapsed());
        for round in 0..2 {
            let t = std::time::Instant::now();
            let job = SearchJob { root: root.clone(), files: idx.files.clone(), re: re.clone(), lit: Lit::of(&pat), overrides: HashMap::new(), limit: 5000 };
            let (tx, rx) = std::sync::mpsc::channel();
            let done = AtomicUsize::new(0);
            job.run(&never, &done, &tx);
            drop(tx);
            let n: usize = rx.into_iter().map(|(_, h)| h.len()).sum();
            println!("new round {round}: {} hits, {:?}", n, t.elapsed());
        }
        let t = std::time::Instant::now();
        let r = search(&root, &idx.files, &crate::pattern::Pat { q: r"HAL_\w+Pin".into(), case: true, word: false, regex: true }.compile().unwrap(), &never, 5000);
        println!("regex (cached): {} hits, {:?}", r.len(), t.elapsed());
    }
}

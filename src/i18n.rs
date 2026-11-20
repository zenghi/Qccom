//! 多语言：界面文字以简体中文为原文，翻译表在 assets/i18n.tsv（每行：原文 \t 各语言译文）。
//! 启动时按系统语言自动选择，设置里可以手动切换。缺翻译时退回英文，再退回中文原文。

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// (代码, 本地名称)。顺序和 i18n.tsv 的列一致
pub const LANGS: &[(&str, &str)] = &[
    ("zh-CN", "简体中文"),
    ("en", "English"),
    ("zh-TW", "繁體中文"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("ru", "Русский"),
    ("pt", "Português"),
];

const EN: usize = 1;
static CUR: AtomicUsize = AtomicUsize::new(0);
static TABLE: OnceLock<HashMap<&'static str, Vec<&'static str>>> = OnceLock::new();

fn table() -> &'static HashMap<&'static str, Vec<&'static str>> {
    TABLE.get_or_init(|| {
        let mut m = HashMap::new();
        for line in include_str!("../assets/i18n.tsv").lines() {
            if line.starts_with("# ") || line.trim().is_empty() {
                continue;
            }
            // 第 0 列是原文本身，译文从第 1 列（English）开始；\n 等转义还原成真正的字符
            let v: Vec<&'static str> = line.split('\t').map(unescape).collect();
            m.insert(v[0], v);
        }
        m
    })
}

/// 表格里写的是 Rust 字面量形式（\n、\\），还原后才能和代码里的字符串对上。
/// 只在启动时跑一次，所以直接 leak 成 'static
fn unescape(s: &'static str) -> &'static str {
    if !s.contains('\\') {
        return s;
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    Box::leak(out.into_boxed_str())
}

/// 翻译。参数必须是代码里的字面量（原文）
/// 中文的拼音首字母（工具搜索用，如「固件打包」→ gjdb）。用 GB2312 一级汉字按拼音排序的特点查表，
/// 不认识的字（二级汉字、非中文）原样保留
pub fn py_initials(s: &str) -> String {
    const B: &[(u16, char)] = &[
        (0xB0A1, 'a'),
        (0xB0C5, 'b'),
        (0xB2C1, 'c'),
        (0xB4EE, 'd'),
        (0xB6EA, 'e'),
        (0xB7A2, 'f'),
        (0xB8C1, 'g'),
        (0xB9FE, 'h'),
        (0xBBF7, 'j'),
        (0xBFA6, 'k'),
        (0xC0AC, 'l'),
        (0xC2E8, 'm'),
        (0xC4C3, 'n'),
        (0xC5B6, 'o'),
        (0xC5BE, 'p'),
        (0xC6DA, 'q'),
        (0xC8BB, 'r'),
        (0xC8F6, 's'),
        (0xCBFA, 't'),
        (0xCDDA, 'w'),
        (0xCEF4, 'x'),
        (0xD1B9, 'y'),
        (0xD4D1, 'z'),
    ];
    let mut out = String::new();
    let mut buf = [0u8; 4];
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        let (g, _, bad) = encoding_rs::GBK.encode(c.encode_utf8(&mut buf));
        if bad || g.len() != 2 {
            continue;
        }
        let code = u16::from_be_bytes([g[0], g[1]]);
        if (0xB0A1..0xD7FA).contains(&code) {
            if let Some((_, l)) = B.iter().rev().find(|(b, _)| code >= *b) {
                out.push(*l);
            }
        }
    }
    out
}

pub fn t(s: &'static str) -> &'static str {
    let lang = CUR.load(Ordering::Relaxed);
    if lang == 0 {
        return s;
    }
    let Some(v) = table().get(s) else { return s };
    let pick = |i: usize| v.get(i).copied().filter(|x| !x.is_empty());
    pick(lang).or_else(|| pick(EN)).unwrap_or(s)
}

/// 带参数的翻译：原文里用 {} 占位，按顺序替换
pub fn tf(s: &'static str, args: &[&dyn Display]) -> String {
    let mut out = String::new();
    let mut it = args.iter();
    let mut rest = t(s);
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

pub fn current() -> usize {
    CUR.load(Ordering::Relaxed)
}

pub fn set(i: usize) {
    CUR.store(i.min(LANGS.len() - 1), Ordering::Relaxed);
}

/// 按语言代码（"auto" 表示跟随系统）设置，返回实际使用的语言序号
pub fn apply(code: &str) -> usize {
    let i = if code == "auto" || code.is_empty() {
        detect()
    } else {
        LANGS.iter().position(|l| l.0 == code).unwrap_or_else(detect)
    };
    set(i);
    i
}

pub fn detect() -> usize {
    let loc = sys_locale::get_locale().unwrap_or_default();
    from_locale(&loc)
}

pub fn from_locale(loc: &str) -> usize {
    let l = loc.replace('_', "-").to_ascii_lowercase();
    if l.starts_with("zh") {
        return if l.contains("tw") || l.contains("hk") || l.contains("mo") || l.contains("hant") { 2 } else { 0 };
    }
    let prim = l.split('-').next().unwrap_or("");
    LANGS.iter().position(|(c, _)| *c == prim).unwrap_or(EN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinyin() {
        assert_eq!(py_initials("固件打包"), "gjdb");
        assert_eq!(py_initials("串口占用"), "ckzy");
        assert_eq!(py_initials("SVD 寄存器"), "svd jcq");
    }

    #[test]
    fn table_is_consistent() {
        let t = table();
        assert!(t.len() > 400);
        for (k, v) in t {
            assert_eq!(v.len(), LANGS.len(), "列数不对：{k}");
            // 空格子 = 这种语言暂时显示英文
            for x in v.iter().filter(|x| !x.is_empty()) {
                assert_eq!(x.matches("{}").count(), k.matches("{}").count(), "占位符数量不一致：{k} → {x}");
            }
        }
        assert!(t["自动识别波特率：依次尝试 18 种常用波特率，选出能解出正常文字的那个。\n需要设备此时在输出日志。"][1].contains('\n'));
        assert_eq!(t["解析终端颜色转义码（\\x1b[31m 等）"][1], "Parse terminal color escapes (\\x1b[31m etc.)");
        set(1);
        assert_eq!(super::t("连接"), "Connect");
        assert_eq!(tf("{} 分钟前", &[&5]), "5 min ago");
        set(0);
    }

    /// 代码里每一处 t("…") / tf("…") 都要在翻译表里有一行，漏了就提醒
    #[test]
    fn every_string_is_translated() {
        // src 下所有源文件（新文件不用再手动加进列表）
        fn walk(d: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("i18n.rs") {
                    out.push(std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        let mut sources = vec![];
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
        let mut missing = vec![];
        for src in &sources {
            let src = src.split("#[cfg(test)]").next().unwrap_or(src);
            for pat in ["t(\"", "tf(\"", "tr(\"", "c(\""] {
                for (i, _) in src.match_indices(pat) {
                    if src[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let rest = &src[i + pat.len()..];
                    let mut end = 0;
                    let b = rest.as_bytes();
                    while end < b.len() && b[end] != b'"' {
                        end += if b[end] == b'\\' { 2 } else { 1 };
                    }
                    let lit = unescape(Box::leak(rest[..end].to_string().into_boxed_str()));
                    if lit.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) && !table().contains_key(lit) {
                        missing.push(lit);
                    }
                }
            }
        }
        for d in crate::keymap::ACTIONS {
            if !table().contains_key(d.label) {
                missing.push(d.label);
            }
        }
        assert!(missing.is_empty(), "翻译表 assets/i18n.tsv 缺少：{missing:#?}");
    }

    #[test]
    fn locale_detect() {
        assert_eq!(from_locale("zh-CN"), 0);
        assert_eq!(from_locale("zh_TW"), 2);
        assert_eq!(from_locale("zh-Hant-HK"), 2);
        assert_eq!(from_locale("en-US"), 1);
        assert_eq!(from_locale("de-DE"), 5);
        assert_eq!(from_locale("pt-BR"), 9);
        assert_eq!(from_locale("sv-SE"), 1);
    }
}

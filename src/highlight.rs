//! 轻量语法高亮：一个通用的词法扫描器 + 每种语言一张关键字表。
//! 不追求编译器级精确，追求快和够用（C/C++、Rust、Python、JS/TS、Go、Java、Shell、JSON、Makefile…）。

use crate::i18n::t;
use eframe::egui::{text::LayoutJob, Color32, FontId, TextFormat};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Rust,
    Python,
    Js,
    Go,
    Java,
    Shell,
    Json,
    Make,
    Ini,
    Markdown,
    Asm,
    Plain,
}

impl Lang {
    pub fn from_path(p: &std::path::Path) -> Lang {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        if name == "makefile" || name == "gnumakefile" || name.ends_with(".mk") || name == "kconfig" || name == "cmakelists.txt" || name.ends_with(".cmake") {
            return Lang::Make;
        }
        let ext = name.rsplit('.').next().unwrap_or("");
        match ext {
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ino" | "cs" | "m" | "ld" | "lds" | "dts" | "dtsi" => Lang::C,
            "rs" => Lang::Rust,
            "py" | "pyw" => Lang::Python,
            "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "vue" => Lang::Js,
            "go" => Lang::Go,
            "java" | "kt" | "kts" | "scala" | "groovy" | "gradle" => Lang::Java,
            "sh" | "bash" | "zsh" | "bat" | "cmd" | "ps1" => Lang::Shell,
            "json" | "jsonc" => Lang::Json,
            "ini" | "toml" | "cfg" | "conf" | "yaml" | "yml" | "properties" | "env" => Lang::Ini,
            "md" | "markdown" => Lang::Markdown,
            "s" | "asm" | "inc" => Lang::Asm,
            _ => Lang::Plain,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "C / C++",
            Lang::Rust => "Rust",
            Lang::Python => "Python",
            Lang::Js => "JavaScript / TS",
            Lang::Go => "Go",
            Lang::Java => "Java / Kotlin",
            Lang::Shell => "Shell",
            Lang::Json => "JSON",
            Lang::Make => "Makefile / CMake",
            Lang::Ini => t("配置文件"),
            Lang::Markdown => "Markdown",
            Lang::Asm => t("汇编"),
            Lang::Plain => t("纯文本"),
        }
    }

    fn spec(self) -> Spec {
        let c_like = Spec {
            line: &["//"],
            block: Some(("/*", "*/")),
            strings: &['"', '\''],
            hash_pre: false,
            kw: &[],
            ty: &[],
            consts: &["true", "false", "NULL", "nullptr"],
        };
        match self {
            Lang::C => Spec {
                hash_pre: true,
                kw: C_KW,
                ty: C_TY,
                ..c_like
            },
            Lang::Rust => Spec {
                kw: RUST_KW,
                ty: RUST_TY,
                consts: &["true", "false", "None", "Some", "Ok", "Err", "Self"],
                strings: &['"'],
                ..c_like
            },
            Lang::Js => Spec {
                kw: JS_KW,
                ty: JS_TY,
                strings: &['"', '\'', '`'],
                consts: &["true", "false", "null", "undefined", "this", "NaN"],
                ..c_like
            },
            Lang::Go => Spec {
                kw: GO_KW,
                ty: GO_TY,
                strings: &['"', '\'', '`'],
                consts: &["true", "false", "nil", "iota"],
                ..c_like
            },
            Lang::Java => Spec {
                kw: JAVA_KW,
                ty: JAVA_TY,
                consts: &["true", "false", "null", "this", "super"],
                ..c_like
            },
            Lang::Python => Spec {
                line: &["#"],
                block: None,
                strings: &['"', '\''],
                hash_pre: false,
                kw: PY_KW,
                ty: PY_TY,
                consts: &["True", "False", "None", "self", "cls"],
            },
            Lang::Shell => Spec {
                line: &["#", "REM ", "rem ", "::"],
                block: None,
                strings: &['"', '\''],
                hash_pre: false,
                kw: SH_KW,
                ty: &[],
                consts: &["true", "false"],
            },
            Lang::Make => Spec {
                line: &["#"],
                block: None,
                strings: &['"', '\''],
                hash_pre: false,
                kw: MAKE_KW,
                ty: &[],
                consts: &[],
            },
            Lang::Json => Spec {
                line: &["//"],
                block: Some(("/*", "*/")),
                strings: &['"'],
                hash_pre: false,
                kw: &[],
                ty: &[],
                consts: &["true", "false", "null"],
            },
            Lang::Ini => Spec {
                line: &["#", ";"],
                block: None,
                strings: &['"', '\''],
                hash_pre: false,
                kw: &[],
                ty: &[],
                consts: &["true", "false", "yes", "no", "on", "off"],
            },
            Lang::Asm => Spec {
                line: &[";", "@", "//"],
                block: Some(("/*", "*/")),
                strings: &['"', '\''],
                hash_pre: true,
                kw: &[],
                ty: &[],
                consts: &[],
            },
            Lang::Markdown | Lang::Plain => Spec {
                line: &[],
                block: None,
                strings: &[],
                hash_pre: false,
                kw: &[],
                ty: &[],
                consts: &[],
            },
        }
    }

    /// 支持跳转定义 / 宏悬浮的语言
    pub fn is_code(self) -> bool {
        !matches!(self, Lang::Plain | Lang::Markdown | Lang::Json | Lang::Ini)
    }
}

struct Spec {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    strings: &'static [char],
    hash_pre: bool,
    kw: &'static [&'static str],
    ty: &'static [&'static str],
    consts: &'static [&'static str],
}

const C_KW: &[&str] = &[
    "auto",
    "break",
    "case",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extern",
    "for",
    "goto",
    "if",
    "inline",
    "register",
    "restrict",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "volatile",
    "while",
    "_Bool",
    "_Static_assert",
    "class",
    "namespace",
    "template",
    "typename",
    "public",
    "private",
    "protected",
    "virtual",
    "override",
    "new",
    "delete",
    "try",
    "catch",
    "throw",
    "using",
    "operator",
    "constexpr",
    "noexcept",
    "explicit",
    "friend",
    "mutable",
    "static_cast",
    "reinterpret_cast",
    "const_cast",
    "dynamic_cast",
    "__attribute__",
    "__asm",
    "__asm__",
    "asm",
    "__IO",
    "__I",
    "__O",
    "__STATIC_INLINE",
    "__weak",
    "__packed",
    "__inline",
    "__irq",
];
const C_TY: &[&str] = &[
    "void",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "signed",
    "unsigned",
    "bool",
    "size_t",
    "ssize_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uintptr_t",
    "intptr_t",
    "u8",
    "u16",
    "u32",
    "u64",
    "s8",
    "s16",
    "s32",
    "s64",
    "BOOL",
    "BYTE",
    "WORD",
    "DWORD",
    "UINT",
    "INT",
    "CHAR",
    "FILE",
    "string",
    "std",
    "vector",
    "map",
    "auto",
    "wchar_t",
    "ptrdiff_t",
    "off_t",
];
const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while", "self",
];
const RUST_TY: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "HashMap",
    "HashSet", "VecDeque",
];
const JS_KW: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "static",
    "switch",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "yield",
    "interface",
    "type",
    "enum",
    "implements",
    "public",
    "private",
    "protected",
    "readonly",
    "as",
];
const JS_TY: &[&str] = &["string", "number", "boolean", "any", "unknown", "never", "object", "Array", "Promise", "Map", "Set", "Record"];
const GO_KW: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const GO_TY: &[&str] = &[
    "bool", "byte", "error", "float32", "float64", "int", "int8", "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr",
];
const JAVA_KW: &[&str] = &[
    "abstract",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "for",
    "if",
    "implements",
    "import",
    "instanceof",
    "interface",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "switch",
    "synchronized",
    "throw",
    "throws",
    "try",
    "while",
    "fun",
    "val",
    "var",
    "when",
    "object",
    "override",
    "data",
    "companion",
];
const JAVA_TY: &[&str] = &[
    "boolean", "byte", "char", "double", "float", "int", "long", "short", "void", "String", "Integer", "List", "Map", "Int", "Unit", "Any",
];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];
const PY_TY: &[&str] = &["int", "float", "str", "bytes", "bool", "list", "dict", "set", "tuple", "object", "print", "len", "range", "open"];
const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in", "function", "return", "export", "local", "echo", "set", "call", "goto", "exit", "cd", "source",
];
const MAKE_KW: &[&str] = &[
    "ifeq",
    "ifneq",
    "ifdef",
    "ifndef",
    "else",
    "endif",
    "include",
    "define",
    "endef",
    "export",
    "override",
    "PHONY",
    "add_executable",
    "add_library",
    "target_link_libraries",
    "set",
    "project",
    "cmake_minimum_required",
    "include_directories",
    "target_include_directories",
    "if",
    "endif",
    "foreach",
    "endforeach",
    "config",
    "menu",
    "endmenu",
    "bool",
    "tristate",
    "depends",
    "select",
    "help",
    "default",
];

/// 高亮配色（接近 VS Code Dark+，但和本程序的青色主题协调）
pub mod pal {
    use eframe::egui::Color32;
    pub const TEXT: Color32 = Color32::from_rgb(0xd4, 0xd8, 0xe0);
    pub const KW: Color32 = Color32::from_rgb(0xc5, 0x86, 0xc0);
    pub const TY: Color32 = Color32::from_rgb(0x4e, 0xc9, 0xb0);
    pub const FUNC: Color32 = Color32::from_rgb(0xdc, 0xdc, 0xaa);
    pub const STR: Color32 = Color32::from_rgb(0xce, 0x91, 0x78);
    pub const NUM: Color32 = Color32::from_rgb(0xb5, 0xce, 0xa8);
    pub const COMMENT: Color32 = Color32::from_rgb(0x6a, 0x99, 0x55);
    pub const PRE: Color32 = Color32::from_rgb(0xc5, 0x86, 0xc0);
    pub const MACRO: Color32 = Color32::from_rgb(0x4f, 0xc1, 0xff);
    pub const CONST: Color32 = Color32::from_rgb(0x56, 0x9c, 0xd6);
    pub const PUNCT: Color32 = Color32::from_rgb(0x9d, 0xa5, 0xb4);
    pub const HEAD: Color32 = Color32::from_rgb(0x2d, 0xe2, 0xe6);
}

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Text,
    Kw,
    Ty,
    Func,
    Str,
    Num,
    Comment,
    Pre,
    Macro,
    Const,
    Punct,
    Head,
}

fn color(t: Tok) -> Color32 {
    match t {
        Tok::Text => pal::TEXT,
        Tok::Kw => pal::KW,
        Tok::Ty => pal::TY,
        Tok::Func => pal::FUNC,
        Tok::Str => pal::STR,
        Tok::Num => pal::NUM,
        Tok::Comment => pal::COMMENT,
        Tok::Pre => pal::PRE,
        Tok::Macro => pal::MACRO,
        Tok::Const => pal::CONST,
        Tok::Punct => pal::PUNCT,
        Tok::Head => pal::HEAD,
    }
}

pub fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic() || c == '$'
}
pub fn is_ident(c: char) -> bool {
    c == '_' || c.is_alphanumeric() || c == '$'
}

/// 把整份文本切成 (字节范围, 记号类型)
fn tokenize(text: &str, lang: Lang) -> Vec<(usize, usize, Tok)> {
    let sp = lang.spec();
    let b = text.as_bytes();
    let mut out: Vec<(usize, usize, Tok)> = Vec::with_capacity(text.len() / 4);
    let mut push = |s: usize, e: usize, t: Tok| {
        if e > s {
            if let Some(last) = out.last_mut() {
                if last.2 == t && last.1 == s {
                    last.1 = e;
                    return;
                }
            }
            out.push((s, e, t));
        }
    };
    let mut i = 0;
    let mut line_start = true;
    let mut in_pre = false;
    let n = b.len();
    while i < n {
        let c = b[i];
        if c == b'\n' {
            // 预处理行以反斜杠续行
            if in_pre && !(i > 0 && (b[i - 1] == b'\\' || (b[i - 1] == b'\r' && i > 1 && b[i - 2] == b'\\'))) {
                in_pre = false;
            }
            push(i, i + 1, Tok::Text);
            i += 1;
            line_start = true;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            push(i, i + 1, Tok::Text);
            i += 1;
            continue;
        }
        let rest = &text[i..];
        // Markdown 标题、列表
        if lang == Lang::Markdown {
            let end = rest.find('\n').map_or(n, |k| i + k);
            let t = if line_start && c == b'#' {
                Tok::Head
            } else if line_start && rest.starts_with("```") {
                Tok::Str
            } else if line_start && (rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("> ")) {
                Tok::Kw
            } else {
                Tok::Text
            };
            push(i, end, t);
            i = end;
            line_start = false;
            continue;
        }
        if lang == Lang::Ini && line_start && c == b'[' {
            let end = rest.find('\n').map_or(n, |k| i + k);
            push(i, end, Tok::Head);
            i = end;
            continue;
        }
        // 块注释
        if let Some((open, close)) = sp.block {
            if rest.starts_with(open) {
                let end = rest[open.len()..].find(close).map_or(n, |k| i + open.len() + k + close.len());
                push(i, end, Tok::Comment);
                i = end;
                line_start = false;
                continue;
            }
        }
        // 行注释
        if sp.line.iter().any(|p| rest.starts_with(p)) && !(sp.hash_pre && c == b'#' && line_start) {
            let end = rest.find('\n').map_or(n, |k| i + k);
            push(i, end, Tok::Comment);
            i = end;
            continue;
        }
        // 预处理指令 #include #define …
        if sp.hash_pre && c == b'#' && line_start {
            let mut j = i + 1;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            while j < n && (b[j] as char).is_ascii_alphabetic() {
                j += 1;
            }
            push(i, j, Tok::Pre);
            let word = text[i + 1..j].trim();
            if word == "include" || word == "import" {
                // <stdio.h> 当字符串
                let end = text[j..].find('\n').map_or(n, |k| j + k);
                let seg = &text[j..end];
                if let Some(a) = seg.find(['<', '"']) {
                    let close = if seg.as_bytes()[a] == b'<' { '>' } else { '"' };
                    let z = seg[a + 1..].find(close).map_or(seg.len(), |k| a + k + 2);
                    push(j, j + a, Tok::Text);
                    push(j + a, j + z, Tok::Str);
                    i = j + z;
                    continue;
                }
            }
            in_pre = true;
            i = j;
            line_start = false;
            continue;
        }
        line_start = false;
        // 字符串
        if sp.strings.contains(&(c as char)) {
            // Rust 的生命周期 'a 不是字符串
            if lang == Lang::Rust && c == b'\'' && i + 2 < n && b[i + 2] != b'\'' && !(b[i + 1] == b'\\') {
                push(i, i + 1, Tok::Punct);
                i += 1;
                continue;
            }
            let q = c;
            let triple = lang == Lang::Python && rest.len() >= 3 && rest.as_bytes()[1] == q && rest.as_bytes()[2] == q;
            let mut j = i + if triple { 3 } else { 1 };
            while j < n {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if triple {
                    if b[j] == q && j + 2 < n && b[j + 1] == q && b[j + 2] == q {
                        j += 3;
                        break;
                    }
                } else if b[j] == q {
                    j += 1;
                    break;
                } else if b[j] == b'\n' && q != b'`' {
                    break;
                }
                j += 1;
            }
            let j = j.min(n);
            push(i, j, Tok::Str);
            i = j;
            continue;
        }
        // 数字
        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_' || ((b[j] == b'+' || b[j] == b'-') && matches!(b[j - 1], b'e' | b'E'))) {
                j += 1;
            }
            push(i, j, Tok::Num);
            i = j;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if is_ident_start(ch) {
            let len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
            let w = &rest[..len];
            let j = i + len;
            // 后面紧跟 ( 的是函数
            let mut k = j;
            while k < n && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            let call = k < n && b[k] == b'(';
            let t = if in_pre && lang == Lang::C {
                if w == "defined" {
                    Tok::Pre
                } else {
                    Tok::Macro
                }
            } else if sp.kw.contains(&w) {
                Tok::Kw
            } else if sp.ty.contains(&w) || (lang == Lang::C && w.ends_with("_t") && w.len() > 2) {
                Tok::Ty
            } else if sp.consts.contains(&w) {
                Tok::Const
            } else if lang == Lang::Rust && k < n && b[k] == b'!' {
                Tok::Macro
            } else if lang.is_code() && w.len() > 1 && w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && w.chars().any(|c| c.is_ascii_uppercase()) {
                Tok::Macro
            } else if call {
                Tok::Func
            } else if lang == Lang::Make && k < n && (b[k] == b'=' || b[k] == b':') {
                Tok::Ty
            } else if (lang == Lang::Rust || lang == Lang::Go || lang == Lang::Java || lang == Lang::Js) && ch.is_uppercase() {
                Tok::Ty
            } else {
                Tok::Text
            };
            push(i, j, t);
            i = j;
            continue;
        }
        // Makefile 变量 $(VAR) ${VAR}
        if (lang == Lang::Make || lang == Lang::Shell) && c == b'$' {
            let mut j = i + 1;
            if j < n && (b[j] == b'(' || b[j] == b'{') {
                let close = if b[j] == b'(' { b')' } else { b'}' };
                while j < n && b[j] != close && b[j] != b'\n' {
                    j += 1;
                }
                j = (j + 1).min(n);
            } else {
                while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
            }
            push(i, j, Tok::Macro);
            i = j;
            continue;
        }
        let l = ch.len_utf8();
        push(i, i + l, if ch.is_ascii_punctuation() { Tok::Punct } else { Tok::Text });
        i += l;
    }
    out
}

pub fn layout(text: &str, lang: Lang, font: FontId) -> LayoutJob {
    let mut job = LayoutJob {
        text: text.to_string(),
        ..Default::default()
    };
    job.wrap.max_width = f32::INFINITY;
    for (s, e, t) in tokenize(text, lang) {
        let fmt = TextFormat {
            font_id: font.clone(),
            color: color(t),
            italics: t == Tok::Comment,
            ..Default::default()
        };
        job.sections.push(eframe::egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: s..e,
            format: fmt,
        });
    }
    if job.sections.is_empty() {
        job.sections.push(eframe::egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: 0..text.len(),
            format: TextFormat {
                font_id: font,
                color: pal::TEXT,
                ..Default::default()
            },
        });
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, lang: Lang) -> Vec<(String, Tok)> {
        tokenize(src, lang)
            .into_iter()
            .filter(|t| t.2 != Tok::Text && t.2 != Tok::Punct)
            .map(|(s, e, t)| (src[s..e].to_string(), t))
            .collect()
    }

    #[test]
    fn c_tokens() {
        let k = kinds("#define LED_PIN 13\nstatic int foo(void) { return 0x1F; } // hi\n/* a */ char *s = \"x\\\"y\";", Lang::C);
        assert!(k.contains(&("#define".into(), Tok::Pre)));
        assert!(k.contains(&("LED_PIN".into(), Tok::Macro)));
        assert!(k.contains(&("foo".into(), Tok::Func)));
        assert!(k.contains(&("0x1F".into(), Tok::Num)));
        assert!(k.contains(&("// hi".into(), Tok::Comment)));
        assert!(k.contains(&("\"x\\\"y\"".into(), Tok::Str)));
        assert!(k.contains(&("static".into(), Tok::Kw)));
        assert!(k.contains(&("int".into(), Tok::Ty)));
    }

    #[test]
    fn include_and_continuation() {
        let k = kinds("#include <stdio.h>\n#define A(x) \\\n  ((x) + 1)\nint b;", Lang::C);
        assert!(k.contains(&("<stdio.h>".into(), Tok::Str)));
        assert!(k.contains(&("int".into(), Tok::Ty)));
    }
}

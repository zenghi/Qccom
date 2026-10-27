//! 代码片段：输入前缀按 Tab 展开，或用快速打开里的 ! 搜索插入。
//! 片段存在数据目录的 snippets.json 里（格式和 VS Code 的差不多），用户直接在编辑器里改。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::highlight::Lang;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Snippet {
    pub prefix: String,
    pub name: String,
    /// 只在这种语言里生效：c / asm / kconfig / dts / ld / make / shell / python / rust；空 = 全部
    #[serde(default)]
    pub lang: String,
    pub body: Vec<String>,
}

pub fn path() -> PathBuf {
    crate::data_dir().join("snippets.json")
}

pub fn load() -> Vec<Snippet> {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(defaults)
}

/// 返回片段文件的路径；还没有就先把默认片段写进去，方便用户照着改
pub fn ensure_file() -> std::io::Result<PathBuf> {
    let p = path();
    if !p.exists() {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(&defaults()).unwrap_or_default())?;
    }
    Ok(p)
}

pub fn lang_id(l: Lang) -> &'static str {
    match l {
        Lang::C => "c",
        Lang::Asm => "asm",
        Lang::Kconfig => "kconfig",
        Lang::Dts => "dts",
        Lang::Ld => "ld",
        Lang::Make => "make",
        Lang::Shell => "shell",
        Lang::Python => "python",
        Lang::Rust => "rust",
        Lang::Js => "js",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::Json => "json",
        Lang::Ini => "ini",
        Lang::Markdown => "markdown",
        Lang::Plain => "text",
    }
}

pub fn applies(s: &Snippet, l: Lang) -> bool {
    s.lang.is_empty() || s.lang.split(',').any(|x| x.trim().eq_ignore_ascii_case(lang_id(l)))
}

/// 展开片段：续行加上当前行的缩进，\t 换成文件的缩进单位，$FILE 换成大写文件名。
/// 占位符 ${1:默认}、$1 换成默认文字；返回 (文字, 展开后要选中的范围)：第一个占位符，没有就是 $0
pub fn expand(body: &[String], indent: &str, unit: &str, file: &str) -> (String, (usize, usize)) {
    let guard: String = file.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    let src = body.join("\n");
    let mut out = String::new();
    // 编号 → 默认文字（同一个编号第一次出现时定义）
    let mut defaults: std::collections::HashMap<u32, String> = Default::default();
    let mut stops: Vec<(u32, usize, usize)> = vec![];
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            out.push('\n');
            out.push_str(indent);
            i += 1;
        } else if c == b'\t' {
            out.push_str(unit);
            i += 1;
        } else if c == b'\\' && i + 1 < b.len() && b[i + 1] == b'$' {
            out.push('$');
            i += 2;
        } else if c == b'$' && src[i..].starts_with("$FILE") {
            out.push_str(&guard);
            i += 5;
        } else if c == b'$' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let n: u32 = src[i + 1..j].parse().unwrap_or(0);
            let text = defaults.get(&n).cloned().unwrap_or_default();
            let at = out.len();
            out.push_str(&text);
            stops.push((n, at, out.len()));
            i = j;
        } else if c == b'$' && src[i..].starts_with("${") {
            let close = src[i..].find('}').map(|k| i + k);
            let inner = close.map(|e| &src[i + 2..e]);
            match inner.and_then(|s| s.split_once(':').map(|(n, d)| (n, d)).or(Some((s, "")))) {
                Some((n, d)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                    let n: u32 = n.parse().unwrap_or(0);
                    let text = defaults.entry(n).or_insert_with(|| d.to_string()).clone();
                    let at = out.len();
                    out.push_str(&text);
                    stops.push((n, at, out.len()));
                    i = close.unwrap() + 1;
                }
                _ => {
                    out.push('$');
                    i += 1;
                }
            }
        } else {
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    let first = stops.iter().filter(|s| s.0 > 0).min_by_key(|s| (s.0, s.1)).or_else(|| stops.iter().find(|s| s.0 == 0));
    let sel = first.map_or((out.len(), out.len()), |s| (s.1, s.2));
    (out, sel)
}

fn sn(prefix: &str, name: &str, lang: &str, body: &[&str]) -> Snippet {
    Snippet {
        prefix: prefix.into(),
        name: name.into(),
        lang: lang.into(),
        body: body.iter().map(|s| s.to_string()).collect(),
    }
}

pub fn defaults() -> Vec<Snippet> {
    vec![
        sn("for", "for loop", "c", &["for (int ${1:i} = 0; $1 < ${2:n}; $1++) {", "\t$0", "}"]),
        sn("if", "if", "c", &["if (${1:cond}) {", "\t$0", "}"]),
        sn("ife", "if / else", "c", &["if (${1:cond}) {", "\t$0", "} else {", "\t", "}"]),
        sn("while", "while loop", "c", &["while (${1:1}) {", "\t$0", "}"]),
        sn("switch", "switch", "c", &["switch (${1:x}) {", "case ${2:0}:", "\t$0", "\tbreak;", "default:", "\tbreak;", "}"]),
        sn("struct", "typedef struct", "c", &["typedef struct {", "\t$0", "} ${1:name}_t;"]),
        sn("enum", "typedef enum", "c", &["typedef enum {", "\t${2:A},", "} ${1:name}_t;"]),
        sn("main", "main()", "c", &["int main(void)", "{", "\t$0", "\treturn 0;", "}"]),
        sn("guard", "header guard", "c", &["#ifndef $FILE", "#define $FILE", "", "$0", "", "#endif /* $FILE */"]),
        sn(
            "cpp",
            "extern \"C\" guard",
            "c",
            &["#ifdef __cplusplus", "extern \"C\" {", "#endif", "", "$0", "", "#ifdef __cplusplus", "}", "#endif"],
        ),
        sn("ifdef", "#ifdef", "c", &["#ifdef ${1:MACRO}", "$0", "#endif"]),
        sn("isr", "interrupt handler", "c", &["void ${1:USART1}_IRQHandler(void)", "{", "\t$0", "}"]),
        sn("reg", "register #define", "c", &["#define ${1:REG_NAME} (*(volatile uint32_t *)${2:0x40000000}UL)"]),
        sn("bit", "bit mask #define", "c", &["#define ${1:FLAG} (1U << ${2:0})"]),
        sn("setbit", "set bit", "c", &["${1:reg} |= (1U << ${2:n});"]),
        sn("clrbit", "clear bit", "c", &["${1:reg} &= ~(1U << ${2:n});"]),
        sn("field", "write a bit field", "c", &["${1:reg} = ($1 & ~(${2:0xF}U << ${3:4})) | ((${4:val} & $2U) << $3);"]),
        sn("printf", "printf", "c", &["printf(\"${1:%d}\\n\", $0);"]),
        sn("todo", "TODO comment", "", &["/* TODO: $0 */"]),
        sn(
            "config",
            "Kconfig option",
            "kconfig",
            &["config ${1:NAME}", "\tbool \"${2:Prompt}\"", "\tdefault ${3:n}", "\thelp", "\t  $0"],
        ),
        sn(
            "node",
            "device tree node",
            "dts",
            &[
                "${1:label}: ${2:name}@${3:0} {",
                "\tcompatible = \"${4:vendor,device}\";",
                "\treg = <0x$3 0x${5:1000}>;",
                "\tstatus = \"okay\";",
                "};",
            ],
        ),
        sn(
            "section",
            "linker output section",
            "ld",
            &[".${1:name} :", "{", "\t. = ALIGN(4);", "\t*(.$1*)", "\t. = ALIGN(4);", "} > ${2:FLASH}"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_placeholders() {
        let (s, sel) = expand(&["for (int ${1:i} = 0; $1 < ${2:n}; $1++) {".into(), "\t$0".into(), "}".into()], "  ", "    ", "a.c");
        assert_eq!(s, "for (int i = 0; i < n; i++) {\n      \n  }");
        assert_eq!(&s[sel.0..sel.1], "i");
        let (s, sel) = expand(&["#ifndef $FILE".into(), "$0".into()], "", "\t", "uart.h");
        assert_eq!(s, "#ifndef UART_H\n");
        assert_eq!(sel, (s.len(), s.len()));
        assert!(defaults().iter().all(|d| !d.body.is_empty()));
    }
}

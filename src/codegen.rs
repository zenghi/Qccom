//! 代码生成：可自定义的宏动作模板（#ifdef 包裹、插入 printf、Doxygen 说明、文件头……），
//! 以及 printf 格式推断、函数签名解析、命名风格转换、按 = / , 对齐、枚举生成 switch 等纯文本处理。
//!
//! 模板变量：
//! `${SELECTION}` 选中的文字（没选中时是当前行）  `${WORD}` 光标处的词  `${FILE}` 文件名
//! `${FUNC}` 所在函数  `${DATE}` `${TIME}` `${YEAR}`  `${AUTHOR}` `${COPYRIGHT}`
//! `${PRINT}` 打印函数（默认 printf）  `${FMT}` `${ARG}` 按变量类型推断的格式和参数
//! `${PARAMS}` `${RETURN}` 函数的 @param / @return 行  `${GUARD}` 头文件保护宏  `${MARK}` 调试打印标记
//! `${INPUT:提示}` 或 `${INPUT:提示=默认值}` 执行时询问  `${CURSOR}` 插入后光标停在这里

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::highlight::{is_ident, is_ident_start};
use crate::i18n::t;

/// 调试打印的标记，「删除插入的调试打印」靠它找
pub const MARK: &str = "/* QCDBG */";

/// 生成的文字放在哪
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// 替换选中的行（没选中时是当前行）
    Selection,
    /// 光标处
    Cursor,
    /// 当前行下面新起一行
    LineBelow,
    /// 当前行上面
    LineAbove,
    /// 当前函数（或下一个函数）的上面
    FuncAbove,
    /// 文件开头
    FileTop,
    /// 包住整个文件
    WholeFile,
}

impl Place {
    pub const ALL: [Place; 7] = [Place::Selection, Place::Cursor, Place::LineBelow, Place::LineAbove, Place::FuncAbove, Place::FileTop, Place::WholeFile];

    pub fn label(self) -> &'static str {
        match self {
            Place::Selection => t("包裹选中的行"),
            Place::Cursor => t("插入到光标处"),
            Place::LineBelow => t("插入到下一行"),
            Place::LineAbove => t("插入到上一行"),
            Place::FuncAbove => t("插入到函数上方"),
            Place::FileTop => t("插入到文件开头"),
            Place::WholeFile => t("包裹整个文件"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct MacroDef {
    pub name: String,
    pub place: Place,
    pub template: String,
    /// 内置动作的 id（恢复默认、版本升级补齐用）；自己加的为空
    pub builtin: String,
}

impl Default for MacroDef {
    fn default() -> Self {
        MacroDef {
            name: String::new(),
            place: Place::Cursor,
            template: String::new(),
            builtin: String::new(),
        }
    }
}

fn b(id: &str, name: &str, place: Place, template: &str) -> MacroDef {
    MacroDef {
        name: name.to_string(),
        place,
        template: template.to_string(),
        builtin: id.to_string(),
    }
}

/// 内置动作。顺序对应快捷键槽位：1 #ifdef（Ctrl+Alt+I）、3 printf（Ctrl+Alt+P）、5 函数说明（Ctrl+Alt+D）、6 文件头（Ctrl+Alt+H）
pub fn builtins() -> Vec<MacroDef> {
    vec![
        b(
            "ifdef",
            t("#ifdef 包裹"),
            Place::Selection,
            &format!("#ifdef ${{INPUT:{m}}}\n${{SELECTION}}\n#endif /* ${{INPUT:{m}}} */", m = t("宏名")),
        ),
        b(
            "ifelse",
            t("#if / #else 包裹"),
            Place::Selection,
            &format!("#if ${{INPUT:{}}}\n${{SELECTION}}\n#else\n${{CURSOR}}\n#endif", t("条件")),
        ),
        b(
            "printf",
            t("插入 printf 打印变量"),
            Place::LineBelow,
            "${PRINT}(\"[%s:%d] ${WORD} = ${FMT}\\n\", __func__, __LINE__, ${ARG}); ${MARK}",
        ),
        b(
            "hexdump",
            t("十六进制打印缓冲区"),
            Place::LineBelow,
            &format!(
                "${{PRINT}}(\"${{WORD}}:\"); ${{MARK}}\nfor (unsigned int _i = 0; _i < (unsigned int)(${{INPUT:{}=sizeof(${{WORD}})}}); _i++) ${{PRINT}}(\" %02X\", ((const unsigned char *)(${{WORD}}))[_i]); ${{MARK}}\n${{PRINT}}(\"\\n\"); ${{MARK}}",
                t("长度")
            ),
        ),
        b("doxfunc", t("函数说明（Doxygen）"), Place::FuncAbove, "/**\n * @brief  ${CURSOR}\n${PARAMS}${RETURN} */"),
        b(
            "fileheader",
            t("文件头说明"),
            Place::FileTop,
            "/**\n * @file    ${FILE}\n * @author  ${AUTHOR}\n * @date    ${DATE}\n * @brief   ${CURSOR}\n *\n * ${COPYRIGHT}\n */\n",
        ),
        b(
            "guard",
            t("头文件保护 + extern \"C\""),
            Place::WholeFile,
            "#ifndef ${GUARD}\n#define ${GUARD}\n\n#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n${SELECTION}${CURSOR}\n\n#ifdef __cplusplus\n}\n#endif\n\n#endif /* ${GUARD} */\n",
        ),
        b("todo", t("带署名的 TODO"), Place::LineAbove, "/* TODO(${AUTHOR} ${DATE}): ${CURSOR} */"),
        b(
            "separator",
            t("分隔注释"),
            Place::LineAbove,
            &format!("/* ${{INPUT:{}}} ---------------------------------------------------------- */", t("标题")),
        ),
    ]
}

/// 设置里保存的列表：补上新版本加的内置动作，最多 MACRO_SLOTS 个
pub fn merge(saved: &mut Vec<MacroDef>) {
    if saved.is_empty() {
        *saved = builtins();
        return;
    }
    for (i, d) in builtins().into_iter().enumerate() {
        if !saved.iter().any(|m| m.builtin == d.builtin) {
            saved.insert(i.min(saved.len()), d);
        }
    }
    saved.truncate(crate::keymap::MACRO_SLOTS);
}

/// 模板里要问用户的输入：(提示, 默认值模板)，按出现顺序去重
pub fn inputs_of(template: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![];
    for (name, _) in vars(template) {
        if let Some(rest) = name.strip_prefix("INPUT:") {
            let (label, def) = rest.split_once('=').unwrap_or((rest, ""));
            if !v.iter().any(|x| x.0 == label) {
                v.push((label.to_string(), def.to_string()));
            }
        }
    }
    v
}

/// 找出模板里的 ${...}：(里面的文字, 在模板里的字节范围)，支持默认值里再嵌套 ${WORD}
fn vars(s: &str) -> Vec<(&str, std::ops::Range<usize>)> {
    let mut out = vec![];
    let mut i = 0;
    while let Some(p) = s[i..].find("${") {
        let start = i + p;
        let mut depth = 0;
        let mut end = None;
        let bytes = s.as_bytes();
        let mut j = start;
        while j < s.len() {
            if bytes[j] == b'$' && bytes.get(j + 1) == Some(&b'{') {
                depth += 1;
                j += 2;
                continue;
            }
            if bytes[j] == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = Some(j);
                    break;
                }
            }
            j += 1;
        }
        let Some(e) = end else { break };
        out.push((&s[start + 2..e], start..e + 1));
        i = e + 1;
    }
    out
}

/// 展开时用到的上下文
#[derive(Default, Clone)]
pub struct Ctx {
    pub selection: String,
    pub word: String,
    pub file: String,
    pub func: String,
    pub author: String,
    pub copyright: String,
    pub print: String,
    /// 变量的类型（推断不出来时为空）
    pub ty: String,
    /// 函数签名（${PARAMS} ${RETURN} 用）
    pub sig: String,
    pub inputs: BTreeMap<String, String>,
}

impl Ctx {
    fn var(&self, name: &str) -> String {
        let now = chrono::Local::now();
        match name {
            "SELECTION" => self.selection.clone(),
            "WORD" => self.word.clone(),
            "FILE" => self.file.clone(),
            "FUNC" => self.func.clone(),
            "DATE" => now.format("%Y-%m-%d").to_string(),
            "TIME" => now.format("%H:%M").to_string(),
            "YEAR" => now.format("%Y").to_string(),
            "AUTHOR" => self.author.clone(),
            "COPYRIGHT" => self.copyright.replace("${YEAR}", &now.format("%Y").to_string()),
            "PRINT" => {
                if self.print.trim().is_empty() {
                    "printf".into()
                } else {
                    self.print.trim().to_string()
                }
            }
            "FMT" => printf_fmt(&self.ty).0.to_string(),
            "ARG" => format!("{}{}", printf_fmt(&self.ty).1, self.word),
            "GUARD" => guard_name(&self.file),
            "MARK" => MARK.to_string(),
            "PARAMS" => {
                let (_, _, ps) = parse_sig(&self.sig).unwrap_or_default();
                ps.iter().map(|p| format!(" * @param  {p}  \n")).collect()
            }
            "RETURN" => match parse_sig(&self.sig) {
                Some((ret, _, _)) if !is_void(&ret) => " * @return \n".to_string(),
                _ => String::new(),
            },
            "CURSOR" => String::new(),
            _ => {
                if let Some(rest) = name.strip_prefix("INPUT:") {
                    let label = rest.split_once('=').map_or(rest, |x| x.0);
                    return self.inputs.get(label).cloned().unwrap_or_default();
                }
                format!("${{{name}}}")
            }
        }
    }

    /// 展开默认值这类简单文字里的变量（不处理 CURSOR、不删行）
    pub fn simple(&self, s: &str) -> String {
        let mut out = String::new();
        let mut last = 0;
        for (name, r) in vars(s) {
            out.push_str(&s[last..r.start]);
            out.push_str(&self.var(name));
            last = r.end;
        }
        out.push_str(&s[last..]);
        out
    }
}

/// 展开模板：返回文字和 ${CURSOR} 的位置（字节）。
/// 某行里的变量展开成空、这一行只剩空白或注释符号时，整行去掉（比如没有版权声明、void 函数没有 @return）
pub fn expand(template: &str, cx: &Ctx) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut cursor = None;
    let tl: Vec<&str> = template.split('\n').collect();
    for (li, line) in tl.iter().enumerate() {
        let mut s = String::new();
        let mut last = 0;
        let mut had_empty = false;
        let mut cur_here = None;
        for (name, r) in vars(line) {
            s.push_str(&line[last..r.start]);
            if name == "CURSOR" {
                cur_here = Some(s.len());
            } else {
                let v = cx.var(name);
                if v.is_empty() {
                    had_empty = true;
                }
                s.push_str(&v);
            }
            last = r.end;
        }
        s.push_str(&line[last..]);
        let bare = s.trim();
        if had_empty && cur_here.is_none() && matches!(bare, "" | "*" | "//" | "#" | ";") {
            continue;
        }
        if let Some(c) = cur_here {
            cursor = Some(out.len() + c);
        }
        out.push_str(&s);
        if li + 1 < tl.len() {
            out.push('\n');
        }
    }
    (out, cursor)
}

/// 头文件保护宏：uart_drv.h → UART_DRV_H_
pub fn guard_name(file: &str) -> String {
    let mut g: String = file.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    if g.starts_with(|c: char| c.is_ascii_digit()) {
        g.insert(0, '_');
    }
    g.push('_');
    g
}

fn is_void(ret: &str) -> bool {
    let r: Vec<&str> = ret
        .split_whitespace()
        .filter(|w| !matches!(*w, "static" | "inline" | "extern" | "__inline" | "__STATIC_INLINE"))
        .collect();
    r.is_empty() || r == ["void"]
}

// ---------------------------------------------------------------- printf 类型推断

/// 按类型给出 printf 格式和参数前面要加的强转
pub fn printf_fmt(ty: &str) -> (&'static str, &'static str) {
    let ty = ty.trim();
    let ptr = ty.contains('*');
    let arr = ty.ends_with("[]");
    let words: Vec<&str> = ty
        .split(|c: char| !is_ident(c))
        .filter(|w| !w.is_empty() && !matches!(*w, "const" | "volatile" | "static" | "register" | "extern" | "signed"))
        .collect();
    let has = |w: &str| words.contains(&w);
    let last = words.last().copied().unwrap_or("");
    if (has("char") || last == "int8_t" || last == "uint8_t") && (ptr || arr) && !has("unsigned") && last != "uint8_t" {
        return ("\\\"%s\\\"", "");
    }
    if ptr || arr {
        return ("%p", "(void *)");
    }
    match last {
        "float" | "double" => return ("%f", ""),
        "size_t" => return ("%zu", ""),
        "ssize_t" | "ptrdiff_t" => return ("%zd", ""),
        "uint64_t" | "u64" => return ("%llu", "(unsigned long long)"),
        "int64_t" | "s64" | "i64" => return ("%lld", "(long long)"),
        "uint32_t" | "u32" | "uintptr_t" => return ("%lu", "(unsigned long)"),
        "int32_t" | "s32" | "i32" => return ("%ld", "(long)"),
        "uint8_t" | "uint16_t" | "u8" | "u16" | "uint_fast8_t" | "uint_least8_t" => return ("%u", "(unsigned int)"),
        "int8_t" | "int16_t" | "s8" | "s16" | "bool" | "_Bool" | "BOOL" => return ("%d", "(int)"),
        "char" if !has("unsigned") => return ("'%c'", ""),
        _ => {}
    }
    let longs = words.iter().filter(|w| **w == "long").count();
    let unsigned = has("unsigned");
    match (longs, unsigned) {
        (2, true) => ("%llu", ""),
        (2, false) => ("%lld", ""),
        (1, true) => ("%lu", ""),
        (1, false) => ("%ld", ""),
        (_, true) => ("%u", ""),
        _ => ("%d", ""),
    }
}

/// 在光标前面的代码里找变量的声明，返回类型文字（数组在后面加 []）。
/// `expr` 可以是 a.b、p->x、arr[i] 这样的表达式，按最后一个成员名找
pub fn infer_type(before: &str, after: &str, expr: &str) -> Option<String> {
    let indexed = expr.trim_end().ends_with(']');
    let base = expr.trim();
    let base = if indexed { base.rsplit_once('[').map_or(base, |x| x.0) } else { base };
    let name = base.rsplit(['.', '>', ' ', '*', '&', '(']).next().unwrap_or(base).trim();
    if name.is_empty() || !name.starts_with(is_ident_start) {
        return None;
    }
    let found = find_decl(before, name).or_else(|| find_decl(after, name))?;
    if indexed {
        // a[i]：去掉一层数组或指针
        if let Some(t) = found.strip_suffix("[]") {
            return Some(t.to_string());
        }
        if let Some(i) = found.rfind('*') {
            return Some(format!("{}{}", &found[..i], &found[i + 1..]).trim().to_string());
        }
    }
    Some(found)
}

const NOT_TYPES: &[&str] = &["return", "if", "while", "for", "switch", "case", "else", "do", "goto", "sizeof", "break", "continue", "default"];

fn find_decl(text: &str, name: &str) -> Option<String> {
    // 从后往前找（离光标近的优先），最多看 3000 行
    for line in text.lines().rev().take(3000) {
        let mut from = line.len();
        while let Some(p) = line[..from].rfind(name) {
            from = p;
            let ok_l = !line[..p].chars().next_back().is_some_and(is_ident);
            let after = &line[p + name.len()..];
            let ok_r = !after.chars().next().is_some_and(is_ident);
            if !ok_l || !ok_r {
                continue;
            }
            let rest = after.trim_start();
            // 声明后面应该是 ; , = [ ) 之类
            if !rest.is_empty() && !rest.starts_with([';', ',', '=', '[', ')', ':']) {
                continue;
            }
            let arr = rest.starts_with('[');
            let mut head = &line[..p];
            // 同一行多个声明：int a, b; 取逗号前那段的类型
            let cut = head.rfind([';', '{', '(', ',']);
            let sep = cut.map(|i| head.as_bytes()[i]);
            if let Some(i) = cut {
                head = &head[i + 1..];
            }
            let mut ty = head.trim().to_string();
            if ty.is_empty() && sep == Some(b',') {
                // int a, b → 往前找到语句开头，去掉第一个变量名
                let stmt_start = line[..p].rfind([';', '{', '(']).map_or(0, |i| i + 1);
                let seg = line[stmt_start..p].split(',').next().unwrap_or("");
                let seg = seg.split('=').next().unwrap_or("").trim_end();
                let seg = seg.trim_end_matches(|c: char| c == ']' || c == '[' || c.is_ascii_digit());
                let n = seg.chars().rev().take_while(|c| is_ident(*c)).count();
                ty = seg[..seg.len() - n].trim().trim_end_matches('*').trim().to_string();
            }
            if ty.is_empty() || ty.contains(['=', '+', '-', '/', '<', '>', '!', '"', '\'', '?', '|', '&', '.', '#']) {
                continue;
            }
            let first = ty.split_whitespace().next().unwrap_or("");
            if NOT_TYPES.contains(&first) || ty.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            if !ty.chars().all(|c| is_ident(c) || c == '*' || c == ' ' || c == '\t') {
                continue;
            }
            if arr {
                ty.push_str("[]");
            }
            return Some(ty);
        }
    }
    None
}

// ---------------------------------------------------------------- 函数签名

/// 解析函数签名：(返回类型, 函数名, 参数名列表)
pub fn parse_sig(sig: &str) -> Option<(String, String, Vec<String>)> {
    let open = sig.find('(')?;
    let head = sig[..open].trim_end();
    let n = head.chars().rev().take_while(|c| is_ident(*c)).map(char::len_utf8).sum::<usize>();
    let name = &head[head.len() - n..];
    if name.is_empty() {
        return None;
    }
    let ret = head[..head.len() - n].trim().to_string();
    // 找配对的 )
    let mut depth = 0;
    let mut close = None;
    for (i, c) in sig[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &sig[open + 1..close?];
    let mut params = vec![];
    for p in split_top(inner, ',') {
        let p = p.trim();
        if p.is_empty() || p == "void" {
            continue;
        }
        if p == "..." {
            params.push("...".to_string());
            continue;
        }
        // 函数指针 int (*cb)(int)
        if let Some(i) = p.find("(*") {
            let r = &p[i + 2..];
            let nm: String = r.trim_start().chars().take_while(|c| is_ident(*c)).collect();
            if !nm.is_empty() {
                params.push(nm);
                continue;
            }
        }
        let p = p.split('=').next().unwrap_or(p).trim_end();
        let p = match p.find('[') {
            Some(i) => p[..i].trim_end(),
            None => p,
        };
        let n = p.chars().rev().take_while(|c| is_ident(*c)).map(char::len_utf8).sum::<usize>();
        let nm = &p[p.len() - n..];
        // 只有类型没有名字（声明里常见）：int, char *
        let only_type = p[..p.len() - n].trim().is_empty();
        if !nm.is_empty() && !only_type {
            params.push(nm.to_string());
        }
    }
    Some((ret, name.to_string(), params))
}

/// 按顶层的分隔符切开（括号、尖括号、引号里的不算）
pub fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut v = vec![];
    let mut depth = 0i32;
    let mut q: Option<char> = None;
    let mut last = 0;
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        if let Some(qc) = q {
            if c == qc && prev != '\\' {
                q = None;
            }
        } else {
            match c {
                '"' | '\'' => q = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    v.push(&s[last..i]);
                    last = i + c.len_utf8();
                }
                _ => {}
            }
        }
        prev = c;
    }
    v.push(&s[last..]);
    v
}

/// 定义 → 声明：int foo(int a) { → int foo(int a);
pub fn decl_of(def: &str) -> String {
    let s = def.split('{').next().unwrap_or(def);
    let s: Vec<&str> = s.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    format!("{};", s.join("\n").trim_end())
}

/// 声明 → 空的定义
pub fn def_of(decl: &str) -> String {
    let s = decl.trim().trim_end_matches(';').trim_end();
    let s = s.strip_prefix("extern ").unwrap_or(s);
    format!("{s}\n{{\n    \n}}\n")
}

// ---------------------------------------------------------------- 命名风格

fn split_words(s: &str) -> Vec<String> {
    let mut words: Vec<String> = vec![];
    let mut cur = String::new();
    let cs: Vec<char> = s.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if c == '_' || c == '-' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let boundary = c.is_uppercase() && !cur.is_empty() && (cs[i - 1].is_lowercase() || cs[i - 1].is_ascii_digit() || cs.get(i + 1).is_some_and(|n| n.is_lowercase()) && cs[i - 1].is_uppercase());
        if boundary {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.into_iter().map(|w| w.to_lowercase()).collect()
}

/// snake_case → camelCase → PascalCase → UPPER_CASE → snake_case
pub fn cycle_case(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let words = split_words(s);
    let cap = |w: &String| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    let has_lower = s.chars().any(|c| c.is_lowercase());
    let has_upper = s.chars().any(|c| c.is_uppercase());
    let snake = words.join("_");
    let camel: String = words.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { cap(w) }).collect();
    let pascal: String = words.iter().map(cap).collect();
    let upper = snake.to_uppercase();
    if !has_upper {
        // snake（或单个小写词）
        if words.len() > 1 {
            camel
        } else {
            pascal
        }
    } else if !has_lower {
        snake
    } else if s.starts_with(|c: char| c.is_lowercase()) {
        pascal
    } else {
        upper
    }
}

// ---------------------------------------------------------------- 对齐

/// 把几行按第一个 =（含 += |= 等）或者顶层逗号对齐；没法对齐时返回 None
pub fn align(lines: &[&str]) -> Option<Vec<String>> {
    let code: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if code.len() < 2 {
        return None;
    }
    let eq_pos = |l: &str| -> Option<usize> {
        let b = l.as_bytes();
        let mut q = false;
        for i in 0..b.len() {
            match b[i] {
                b'"' => q = !q,
                b'=' if !q => {
                    let next = b.get(i + 1).copied();
                    let prev = if i > 0 { b[i - 1] } else { b' ' };
                    if next == Some(b'=') || prev == b'=' || prev == b'!' || (prev == b'<' || prev == b'>') && !(i > 1 && b[i - 2] == prev) {
                        continue;
                    }
                    // 复合赋值：运算符的起点
                    let mut s = i;
                    while s > 0 && b"+-*/%&|^<>".contains(&b[s - 1]) {
                        s -= 1;
                    }
                    return Some(s);
                }
                _ => {}
            }
        }
        None
    };
    if code.iter().all(|l| eq_pos(l).is_some()) {
        let heads: Vec<Option<(String, &str)>> = lines.iter().map(|l| eq_pos(l).map(|p| (l[..p].trim_end().to_string(), &l[p..]))).collect();
        let w = heads.iter().flatten().map(|(h, _)| h.chars().count()).max()?;
        return Some(
            lines
                .iter()
                .zip(heads)
                .map(|(l, h)| match h {
                    Some((h, rest)) => format!("{h}{} {}", " ".repeat(w - h.chars().count()), rest),
                    None => l.to_string(),
                })
                .collect(),
        );
    }
    // 逗号：按列补空格（逗号后面补）
    let cells: Vec<Vec<String>> = lines
        .iter()
        .map(|l| {
            let ind = l.len() - l.trim_start().len();
            let body = l.trim();
            // { a, b, c }, 这样的行：按大括号里面的逗号切
            let (pre, inner, post) = match (body.find('{'), body.rfind('}')) {
                (Some(a), Some(z)) if a < z => (&body[..=a], &body[a + 1..z], &body[z..]),
                _ => ("", body, ""),
            };
            let mut v: Vec<String> = split_top(inner, ',').iter().map(|c| c.trim().to_string()).collect();
            if let Some(f) = v.first_mut() {
                *f = format!("{}{pre}{f}", &l[..ind]);
            }
            if let Some(z) = v.last_mut() {
                z.push_str(post);
            }
            v
        })
        .collect();
    let cols = cells.iter().map(Vec::len).max()?;
    if cols < 2 {
        return None;
    }
    let mut widths = vec![0usize; cols];
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            if i + 1 < row.len() {
                widths[i] = widths[i].max(c.chars().count());
            }
        }
    }
    Some(
        cells
            .iter()
            .zip(lines)
            .map(|(row, l)| {
                if l.trim().is_empty() || row.len() < 2 {
                    return l.to_string();
                }
                let mut s = String::new();
                for (i, c) in row.iter().enumerate() {
                    s.push_str(c);
                    if i + 1 < row.len() {
                        s.push(',');
                        s.push_str(&" ".repeat(widths[i] - c.chars().count() + 1));
                    }
                }
                s.trim_end().to_string()
            })
            .collect(),
    )
}

/// 枚举常量 → switch 语句
pub fn enum_switch(var: &str, consts: &[String], indent: &str, unit: &str) -> String {
    let mut s = format!("switch ({var}) {{\n");
    for c in consts {
        s.push_str(&format!("{indent}case {c}:\n{indent}{unit}break;\n"));
    }
    s.push_str(&format!("{indent}default:\n{indent}{unit}break;\n{indent}}}"));
    s
}

/// 枚举定义的原文 → 常量名
pub fn enum_consts(body: &str) -> Vec<String> {
    let Some(a) = body.find('{') else { return vec![] };
    let z = body.rfind('}').unwrap_or(body.len());
    let inner: String = body[a + 1..z].lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n");
    // 去掉 /* */ 注释
    let mut clean = String::new();
    let mut rest = inner.as_str();
    while let Some(p) = rest.find("/*") {
        clean.push_str(&rest[..p]);
        rest = rest[p..].find("*/").map_or("", |e| &rest[p + e + 2..]);
    }
    clean.push_str(rest);
    split_top(&clean, ',')
        .iter()
        .filter_map(|c| {
            let n: String = c.trim().chars().take_while(|c| is_ident(*c)).collect();
            (!n.is_empty()).then_some(n)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_template() {
        let mut cx = Ctx {
            selection: "foo();".into(),
            ..Default::default()
        };
        cx.inputs.insert("宏名".into(), "DEBUG".into());
        let (s, c) = expand("#ifdef ${INPUT:宏名}\n${SELECTION}\n#endif /* ${INPUT:宏名} */", &cx);
        assert_eq!(s, "#ifdef DEBUG\nfoo();\n#endif /* DEBUG */");
        assert_eq!(c, None);
        // 空变量所在的行去掉
        let (s, c) = expand("/**\n * @brief ${CURSOR}\n * ${COPYRIGHT}\n */", &cx);
        assert_eq!(s, "/**\n * @brief \n */");
        assert_eq!(c, Some(14));
        assert_eq!(inputs_of("${INPUT:长度=sizeof(${WORD})} ${INPUT:长度}"), vec![("长度".to_string(), "sizeof(${WORD})".to_string())]);
        cx.word = "buf".into();
        assert_eq!(cx.simple("sizeof(${WORD})"), "sizeof(buf)");
    }

    #[test]
    fn doxygen() {
        let cx = Ctx {
            sig: "static int uart_send(uart_t *u, const uint8_t *buf, size_t len)".into(),
            ..Default::default()
        };
        let (s, _) = expand(&builtins()[4].template, &cx);
        assert_eq!(s, "/**\n * @brief  \n * @param  u  \n * @param  buf  \n * @param  len  \n * @return \n */");
        let cx = Ctx {
            sig: "void init(void)".into(),
            ..Default::default()
        };
        let (s, _) = expand(&builtins()[4].template, &cx);
        assert_eq!(s, "/**\n * @brief  \n */");
        assert_eq!(parse_sig("void reg(int (*cb)(int), int n[4])").unwrap().2, vec!["cb", "n"]);
    }

    #[test]
    fn printf_inference() {
        let before = "void f(const char *name, uint8_t n)\n{\n    int a, b;\n    float t = 1.0f;\n    uint32_t regs[4];\n    struct cfg *p;\n";
        assert_eq!(infer_type(before, "", "name").as_deref(), Some("const char *"));
        assert_eq!(printf_fmt("const char *").0, "\\\"%s\\\"");
        assert_eq!(infer_type(before, "", "b").as_deref(), Some("int"));
        assert_eq!(infer_type(before, "", "t").as_deref(), Some("float"));
        assert_eq!(infer_type(before, "", "regs[2]").as_deref(), Some("uint32_t"));
        assert_eq!(printf_fmt("uint32_t"), ("%lu", "(unsigned long)"));
        assert_eq!(printf_fmt("uint8_t"), ("%u", "(unsigned int)"));
        assert_eq!(infer_type(before, "", "p").as_deref(), Some("struct cfg *"));
        assert_eq!(printf_fmt("struct cfg *").0, "%p");
        assert_eq!(printf_fmt("unsigned long long").0, "%llu");
        assert_eq!(infer_type(before, "", "zzz"), None);
        let cx = Ctx {
            word: "n".into(),
            ty: "uint8_t".into(),
            ..Default::default()
        };
        let (s, _) = expand(&builtins()[2].template, &cx);
        assert_eq!(s, "printf(\"[%s:%d] n = %u\\n\", __func__, __LINE__, (unsigned int)n); /* QCDBG */");
    }

    #[test]
    fn cases() {
        assert_eq!(cycle_case("uart_rx_buf"), "uartRxBuf");
        assert_eq!(cycle_case("uartRxBuf"), "UartRxBuf");
        assert_eq!(cycle_case("UartRxBuf"), "UART_RX_BUF");
        assert_eq!(cycle_case("UART_RX_BUF"), "uart_rx_buf");
        assert_eq!(cycle_case("HTTPServer"), "HTTP_SERVER");
        assert_eq!(guard_name("uart_drv.h"), "UART_DRV_H_");
    }

    #[test]
    fn aligning() {
        let v = align(&["    a = 1;", "    long_name |= 2;", "    x == y;"]);
        assert_eq!(v, None);
        let v = align(&["    a = 1;", "    long_name |= 2;"]).unwrap();
        assert_eq!(v, vec!["    a         = 1;", "    long_name |= 2;"]);
        let v = align(&["{1, 22, 3},", "{100, 2, 3},"]).unwrap();
        assert_eq!(v, vec!["{1,   22, 3},", "{100, 2,  3},"]);
    }

    #[test]
    fn enums_and_decls() {
        let c = enum_consts("typedef enum {\n  ST_IDLE = 0, /* idle */\n  ST_RUN, // run\n  ST_ERR\n} state_t;");
        assert_eq!(c, vec!["ST_IDLE", "ST_RUN", "ST_ERR"]);
        let s = enum_switch("st", &c[..1], "", "    ");
        assert_eq!(s, "switch (st) {\ncase ST_IDLE:\n    break;\ndefault:\n    break;\n}");
        assert_eq!(decl_of("int foo(int a)\n{"), "int foo(int a);");
        assert_eq!(def_of("extern int foo(int a);"), "int foo(int a)\n{\n    \n}\n");
    }
}

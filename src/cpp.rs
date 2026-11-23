//! 预处理条件分析：找出 #if / #ifdef 里不生效的分支（编辑器里灰显）、#if 配对跳转、光标所在的 #if 块。
//!
//! 只做编辑器需要的近似：按编译参数、当前文件及包含的头文件顺序追踪宏。
//! 算不出来的条件（用了未知的宏、函数式宏、__has_include 等）一律当作生效，宁可不灰也不灰错。

use std::collections::HashMap;

use crate::highlight::{is_ident, is_ident_start};

/// 一行是预处理指令时返回 (关键字, 后面的内容，去掉了注释)
fn directive(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start().strip_prefix('#')?.trim_start();
    let n = t.find(|c: char| !is_ident(c)).unwrap_or(t.len());
    let (kw, rest) = t.split_at(n);
    Some((kw, rest.trim()))
}

/// 一组条件编译：#if… 起头，中间的 #elif / #else，最后的 #endif（没写完时可能缺）
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub lines: Vec<usize>,
}

impl Group {
    pub fn first(&self) -> usize {
        self.lines[0]
    }
    pub fn last(&self) -> usize {
        *self.lines.last().unwrap()
    }
}

// Drop C comments without interpreting comment markers inside string literals.
fn clean<'a>(line: &'a str, block: &mut bool) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    if !*block && !line.contains('/') {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let (mut start, mut i) = (0, 0);
    let mut quote = 0;
    let mut result: Option<String> = None;
    while i < bytes.len() {
        if *block {
            let Some(end) = line[i..].find("*/") else { return Cow::Owned(result.unwrap_or_default()) };
            i += end + 2;
            start = i;
            *block = false;
        } else if quote != 0 {
            if bytes[i] == b'\\' {
                i = (i + 2).min(bytes.len());
                continue;
            }
            if bytes[i] == quote {
                quote = 0;
            }
            i += 1;
        } else if matches!(bytes[i], b'"' | b'\'') {
            quote = bytes[i];
            i += 1;
        } else if bytes[i..].starts_with(b"//") {
            return match result {
                Some(mut r) => {
                    r.push_str(&line[start..i]);
                    Cow::Owned(r)
                }
                None => Cow::Borrowed(&line[start..i]),
            };
        } else if bytes[i..].starts_with(b"/*") {
            let r = result.get_or_insert_with(String::new);
            r.push_str(&line[start..i]);
            r.push(' ');
            *block = true;
            i += 2;
        } else {
            i += 1;
        }
    }
    match result {
        Some(mut r) => {
            if !*block {
                r.push_str(&line[start..]);
            }
            Cow::Owned(r)
        }
        None => Cow::Borrowed(&line[start..]),
    }
}

/// Report a logical directive at its first physical line.
fn scan(text: &str, mut f: impl FnMut(usize, &str, &str)) {
    let mut block = false;
    let mut lines = text.split('\n').enumerate();
    while let Some((i, raw)) = lines.next() {
        let mut line = clean(raw, &mut block);
        if !line.trim_start().starts_with('#') {
            continue;
        }
        while line.trim_end().ends_with('\\') {
            let Some((_, next)) = lines.next() else { break };
            let n = line.trim_end().len() - 1;
            let dst = line.to_mut();
            dst.truncate(n);
            dst.push_str(&clean(next, &mut block));
        }
        if let Some((kw, rest)) = directive(&line) {
            f(i, kw, rest);
        }
    }
}

/// Included files only contribute preprocessor directives; their source lines are never rendered here.
pub fn header_directives(text: &str) -> String {
    let mut out = String::new();
    scan(text, |_, kw, rest| {
        out.push('#');
        out.push_str(kw);
        out.push(' ');
        out.push_str(rest);
        out.push('\n');
    });
    out
}

pub fn groups(text: &str) -> Vec<Group> {
    let mut out: Vec<Group> = vec![];
    let mut stack: Vec<usize> = vec![];
    scan(text, |i, kw, _| match kw {
        "if" | "ifdef" | "ifndef" => {
            stack.push(out.len());
            out.push(Group { lines: vec![i] });
        }
        "elif" | "else" | "elifdef" | "elifndef" => {
            if let Some(&g) = stack.last() {
                out[g].lines.push(i);
            }
        }
        "endif" => {
            if let Some(g) = stack.pop() {
                out[g].lines.push(i);
            }
        }
        _ => {}
    });
    out
}

/// 配对跳转：在指令行上跳到同组的下一条（#endif 回到 #if）；不在指令行上就跳到所在组里光标后面那一条
pub fn matching(text: &str, line: usize) -> Option<usize> {
    let gs = groups(text);
    if let Some(g) = gs.iter().find(|g| g.lines.contains(&line)) {
        let i = g.lines.iter().position(|&l| l == line)?;
        return Some(*g.lines.get(i + 1).unwrap_or(&g.lines[0]));
    }
    let g = innermost(&gs, line)?;
    g.lines.iter().copied().find(|&l| l > line)
}

fn innermost(gs: &[Group], line: usize) -> Option<&Group> {
    gs.iter().filter(|g| g.lines.len() > 1 && g.first() < line && line < g.last()).max_by_key(|g| g.first())
}

/// 光标所在的最内层 #if 块：(#if 所在行, #endif 所在行)
pub fn enclosing(text: &str, line: usize) -> Option<(usize, usize)> {
    let gs = groups(text);
    innermost(&gs, line).map(|g| (g.first(), g.last()))
}

struct Frame {
    /// 外层整体是否生效
    parent: bool,
    /// 条件算得出来
    known: bool,
    /// 已经有分支生效了
    taken: bool,
    /// 当前分支生效
    active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Defined(String),
    Undefined,
    Unknown,
}

impl Binding {
    fn defined(&self) -> Option<bool> {
        match self {
            Self::Defined(_) => Some(true),
            Self::Undefined => Some(false),
            Self::Unknown => None,
        }
    }
}

pub type Include = (std::path::PathBuf, std::sync::Arc<str>);
type Resolver<'a> = dyn FnMut(&std::path::Path, &str, bool) -> Option<Include> + 'a;

pub struct Unit {
    pub macros: HashMap<String, Binding>,
    once: std::collections::HashSet<std::path::PathBuf>,
    stack: Vec<std::path::PathBuf>,
    files: usize,
}

impl Unit {
    pub fn new(macros: HashMap<String, Binding>) -> Self {
        Self {
            macros,
            once: Default::default(),
            stack: Vec::new(),
            files: 0,
        }
    }

    fn lookup(&self, name: &str) -> Binding {
        // Preview the macros visible in this translation unit. An SDK header that
        // could not be read does not make every application switch indeterminate.
        self.macros
            .get(name)
            .cloned()
            .unwrap_or_else(|| if name.starts_with("__") { Binding::Unknown } else { Binding::Undefined })
    }

    fn include_name(&self, spec: &str) -> Option<(String, bool)> {
        let mut token = spec.trim().to_owned();
        for _ in 0..16 {
            if let Some(name) = token.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                return Some((name.into(), true));
            }
            if let Some(name) = token.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
                return Some((name.into(), false));
            }
            if token.is_empty() || first_ident(&token) != token {
                return None;
            }
            let Binding::Defined(value) = self.macros.get(&token)? else { return None };
            token = value.trim().to_owned();
        }
        None
    }

    pub fn include(&mut self, from: &std::path::Path, name: &str, quoted: bool, resolve: &mut Resolver<'_>, certain: bool) {
        // Bound pathological include graphs, even if files include themselves through aliases.
        if self.stack.len() >= 48 || self.files >= 1024 {
            return;
        }
        if let Some((path, text)) = resolve(from, name, quoted) {
            if !self.once.contains(&path) && !self.stack.contains(&path) {
                self.files += 1;
                self.process(&path, &text, resolve, certain, false);
            }
        }
    }

    pub fn ranges(&mut self, path: &std::path::Path, text: &str, resolve: &mut Resolver<'_>) -> Vec<(usize, usize)> {
        self.process(path, text, resolve, true, true)
    }

    fn process(&mut self, path: &std::path::Path, text: &str, resolve: &mut Resolver<'_>, certain: bool, mark: bool) -> Vec<(usize, usize)> {
        self.stack.push(path.to_path_buf());
        let mut stack: Vec<Frame> = vec![];
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut dim = |start: usize, end: usize| {
            if !mark || start >= end {
                return;
            }
            if let Some(r) = ranges.last_mut().filter(|r| r.1 + 1 == start) {
                r.1 = end - 1;
            } else {
                ranges.push((start, end - 1));
            }
        };
        let mut from = 0;
        let mut on = true;
        // Recognize a fresh include guard even when it uses a reserved name.
        let mut block = false;
        let mut directives = text.lines().filter_map(|l| {
            let line = clean(l, &mut block);
            let (kw, rest) = directive(&line)?;
            (kw != "pragma" || rest != "once").then(|| (kw.to_owned(), rest.to_owned()))
        });
        let guard = match (directives.next(), directives.next()) {
            (Some((kw, g)), Some((define, d))) if define == "define" => {
                let n = first_ident(&d);
                let normalized: String = g.split_whitespace().collect();
                ((kw == "ifndef" && g == n) || (kw == "if" && normalized == format!("!defined({n})"))).then(|| n.to_owned())
            }
            _ => None,
        };
        scan(text, |i, kw, rest| {
            if !on {
                dim(from, i);
            }
            let mut line_on = on;
            let condition = || {
                let name = first_ident(rest);
                let def = || self.lookup(name).defined();
                if stack.is_empty()
                    && matches!(kw, "if" | "ifndef")
                    && guard
                        .as_ref()
                        .is_some_and(|g| !self.macros.contains_key(g) && (kw == "ifndef" && name == g || kw == "if" && rest.split_whitespace().collect::<String>() == format!("!defined({g})")))
                {
                    return Some(true);
                }
                match kw {
                    "ifdef" | "elifdef" => def(),
                    "ifndef" | "elifndef" => def().map(|v| !v),
                    _ => eval_cond(rest, &|n| self.lookup(n)),
                }
            };
            match kw {
                "if" | "ifdef" | "ifndef" => {
                    let cond = if on { condition() } else { Some(false) };
                    stack.push(Frame {
                        parent: on,
                        known: cond.is_some(),
                        taken: cond == Some(true),
                        active: cond != Some(false),
                    });
                }
                "elif" | "elifdef" | "elifndef" => {
                    let cond = if stack.last().is_some_and(|f| f.parent && !f.taken) { condition() } else { None };
                    if let Some(f) = stack.last_mut() {
                        line_on = f.parent;
                        f.active = !f.taken && cond != Some(false);
                        if !f.taken {
                            f.known &= cond.is_some();
                        }
                        f.taken |= cond == Some(true);
                    }
                }
                "else" => {
                    if let Some(f) = stack.last_mut() {
                        line_on = f.parent;
                        f.active = !f.taken;
                        f.taken = true;
                    }
                }
                "endif" => {
                    if let Some(f) = stack.pop() {
                        line_on = f.parent;
                    }
                }
                _ if on => {
                    let definite = certain && stack.iter().all(|f| f.known);
                    match kw {
                        "define" | "undef" => {
                            let name = first_ident(rest);
                            if !name.is_empty() {
                                let val = if kw == "undef" {
                                    Binding::Undefined
                                } else {
                                    let body = &rest[name.len()..];
                                    Binding::Defined(if body.starts_with('(') { "\0".into() } else { body.trim().into() })
                                };
                                let val = if definite || self.lookup(name) == val { val } else { Binding::Unknown };
                                self.macros.insert(name.into(), val);
                            }
                        }
                        "include" => {
                            if let Some((name, quoted)) = self.include_name(rest) {
                                self.include(path, &name, quoted, resolve, definite);
                            }
                        }
                        "pragma" if definite && rest == "once" => {
                            self.once.insert(path.to_owned());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            if !line_on {
                dim(i, i + 1);
            }
            from = i + 1;
            on = stack.last().is_none_or(|f| f.parent && f.active);
        });
        if mark && !on {
            dim(from, text.split('\n').count());
        }
        self.stack.pop();
        ranges
    }
}

fn first_ident(s: &str) -> &str {
    let s = s.trim_start();
    let n = s.find(|c: char| !is_ident(c)).unwrap_or(s.len());
    &s[..n]
}

/// 算 #if 后面的条件；算不出来返回 None
fn eval_cond(expr: &str, lookup: &dyn Fn(&str) -> Binding) -> Option<bool> {
    let e = subst(expr, lookup, 0)?;
    crate::cindex::eval(&e).map(|v| v != 0)
}

fn subst(expr: &str, lookup: &dyn Fn(&str) -> Binding, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let mut out = String::with_capacity(expr.len());
    let mut i = 0;
    while i < expr.len() {
        let c = expr[i..].chars().next()?;
        if is_ident_start(c) {
            let n = expr[i..].find(|c: char| !is_ident(c)).unwrap_or(expr.len() - i);
            let w = &expr[i..i + n];
            i += n;
            if w == "defined" {
                // defined(X) 或 defined X
                let rest = expr[i..].trim_start();
                let skipped = expr.len() - i - rest.len();
                let (name, used) = if let Some(r) = rest.strip_prefix('(') {
                    let r2 = r.trim_start();
                    let name = first_ident(r2);
                    let after = r2[name.len()..].trim_start();
                    if !after.starts_with(')') {
                        return None;
                    }
                    let used = rest.len() - after.len() + 1;
                    (name, used)
                } else {
                    let name = first_ident(rest);
                    (name, name.len())
                };
                if name.is_empty() {
                    return None;
                }
                out.push_str(if lookup(name).defined()? { " 1 " } else { " 0 " });
                i += skipped + used;
                continue;
            }
            // 整数后缀（如 10UL）已经在数字里处理；这里是真正的标识符
            if expr[i..].trim_start().starts_with('(') {
                return None; // 函数式宏
            }
            let Binding::Defined(v) = lookup(w) else { return None };
            if v.contains('\u{0}') || v.trim().is_empty() {
                return None;
            }
            out.push('(');
            out.push_str(&subst(&v, lookup, depth + 1)?);
            out.push(')');
            continue;
        }
        if c.is_ascii_digit() {
            // 数字连同后缀一起拷过去
            let n = expr[i..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(expr.len() - i);
            out.push_str(&expr[i..i + n]);
            i += n;
            continue;
        }
        if c == '\'' {
            // 字符常量
            let rest = &expr[i + 1..];
            let close = rest.find('\'')?;
            let inner = &rest[..close];
            let v = match inner {
                "\\n" => 10,
                "\\r" => 13,
                "\\t" => 9,
                "\\0" => 0,
                _ if inner.chars().count() == 1 => inner.chars().next()? as u32,
                _ => return None,
            };
            out.push_str(&v.to_string());
            i += close + 2;
            continue;
        }
        out.push(c);
        i += c.len_utf8();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inactive(text: &str, defs: &[(&str, &str)]) -> Vec<(usize, usize)> {
        let macros = defs.iter().map(|(n, v)| (n.to_string(), Binding::Defined(v.to_string()))).collect();
        Unit::new(macros).ranges(std::path::Path::new("main.c"), text, &mut |_, _, _| None)
    }

    #[test]
    fn if0_and_else() {
        let t = "a\n#if 0\nb\nc\n#else\nd\n#endif\ne";
        assert_eq!(inactive(t, &[]), vec![(2, 3)]);
        let t = "#if 1\na\n#else\nb\n#endif";
        assert_eq!(inactive(t, &[]), vec![(3, 3)]);
    }

    #[test]
    fn ifdef_and_guard() {
        let t = "#ifndef FOO_H\n#define FOO_H\nint x;\n#ifdef DEBUG\nlog();\n#endif\n#endif";
        assert_eq!(inactive(t, &[]), vec![(4, 4)]);
        assert_eq!(inactive(t, &[("DEBUG", "")]), vec![]);
    }

    #[test]
    fn unknown_stays_active() {
        let t = "#if FOO > 3\na\n#else\nb\n#endif";
        assert_eq!(inactive(t, &[]), vec![]);
        assert_eq!(inactive(t, &[("FOO", "2")]), vec![(1, 1)]);
    }

    #[test]
    fn unknown_condition_does_not_activate_later_false_elif() {
        let t = "#if __SDK_TEST(1)\na\n#elifdef ZNDD\nb\n#elifndef THM\nc\n#else\nd\n#endif";
        assert_eq!(inactive(t, &[]), vec![(3, 3), (7, 7)]);
    }

    #[test]
    fn nested_and_elif() {
        let t = "#if 0\n#if 1\na\n#endif\nb\n#elif defined(X)\nc\n#elif 1\nd\n#else\ne\n#endif";
        // X 未定义：#elif defined(X) 不生效，#elif 1 生效，#else 不生效
        assert_eq!(inactive(t, &[]), vec![(1, 4), (6, 6), (10, 10)]);
    }

    #[test]
    fn local_define() {
        let t = "#define MODE 2\n#if MODE == 2\na\n#else\nb\n#endif\n#undef MODE\n#ifdef MODE\nc\n#endif";
        assert_eq!(inactive(t, &[]), vec![(4, 4), (8, 8)]);
    }

    #[test]
    fn match_and_enclosing() {
        let t = "#if A\nx\n#else\ny\n#endif";
        assert_eq!(matching(t, 0), Some(2));
        assert_eq!(matching(t, 2), Some(4));
        assert_eq!(matching(t, 4), Some(0));
        assert_eq!(matching(t, 1), Some(2));
        assert_eq!(enclosing(t, 3), Some((0, 4)));
        assert_eq!(enclosing("/*\n#if 0\n*/\nx", 3), None);
    }
}

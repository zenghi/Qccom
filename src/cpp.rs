//! 预处理条件分析：找出 #if / #ifdef 里不生效的分支（编辑器里灰显）、#if 配对跳转、光标所在的 #if 块。
//!
//! 只做编辑器需要的近似：宏是否定义、宏的值来自本文件前面的 #define 和外部（工程索引、设置里的额外宏）；
//! 算不出来的条件（用了未知的宏、函数式宏、__has_include 等）一律当作生效，宁可不灰也不灰错。

use std::collections::HashMap;

use crate::highlight::{is_ident, is_ident_start};

/// 一行是预处理指令时返回 (关键字, 后面的内容，去掉了注释)
fn directive(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start().strip_prefix('#')?.trim_start();
    let n = t.find(|c: char| !is_ident(c)).unwrap_or(t.len());
    let (kw, rest) = t.split_at(n);
    let rest = rest.split("//").next().unwrap_or("");
    let rest = rest.split("/*").next().unwrap_or("");
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

/// 逐行，跳过块注释里的内容，回调 (行号, 关键字, 条件)
fn scan<'a>(text: &'a str, mut f: impl FnMut(usize, &'a str, &'a str)) {
    let mut in_comment = false;
    for (i, line) in text.split('\n').enumerate() {
        if in_comment {
            if let Some(p) = line.find("*/") {
                in_comment = false;
                if let Some(d) = directive(&line[p + 2..]) {
                    f(i, d.0, d.1);
                }
            }
            continue;
        }
        if let Some((kw, rest)) = directive(line) {
            f(i, kw, rest);
        }
        // 只看行内最后一个 /* 有没有闭合，足够判断下一行是不是在注释里
        if let Some(p) = line.rfind("/*") {
            let tail = &line[p + 2..];
            let quoted = line[..p].matches('"').count() % 2 == 1 || line[..p].contains("//");
            if !quoted && !tail.contains("*/") {
                in_comment = true;
            }
        }
    }
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

/// 不生效的行范围（含首尾，按行号排序）。`ext` 查外部定义的宏：定义了返回 Some(值)
pub fn inactive(text: &str, ext: &dyn Fn(&str) -> Option<String>) -> Vec<(usize, usize)> {
    // 本文件里的 #define / #undef：None 表示被 #undef 了
    let mut local: HashMap<String, Option<String>> = HashMap::new();
    let mut stack: Vec<Frame> = vec![];
    let is_on = |s: &[Frame]| s.last().is_none_or(|f| f.parent && f.active);
    let n_lines = text.split('\n').count();
    let mut dead = vec![false; n_lines];
    let mut events: Vec<(usize, &str, &str)> = vec![];
    scan(text, |i, kw, rest| events.push((i, kw, rest)));
    // 上一条指令之后的行从哪开始、那时是否生效
    let mut from = 0usize;
    let mut on = true;
    for (i, kw, rest) in events {
        if !on {
            dead[from..i].iter_mut().for_each(|d| *d = true);
        }
        let on_before = is_on(&stack);
        let lookup = |name: &str| -> Option<String> {
            match local.get(name) {
                Some(v) => v.clone(),
                None => ext(name),
            }
        };
        // 指令行本身：所在组的外层生效就正常显示
        let mut line_on = on_before;
        match kw {
            "if" | "ifdef" | "ifndef" => {
                let cond = if !on_before {
                    Some(false)
                } else {
                    match kw {
                        "ifdef" => Some(lookup(first_ident(rest)).is_some()),
                        "ifndef" => Some(lookup(first_ident(rest)).is_none()),
                        _ => eval_cond(rest, &lookup),
                    }
                };
                stack.push(Frame {
                    parent: on_before,
                    known: cond.is_some(),
                    taken: cond != Some(false),
                    active: cond != Some(false),
                });
            }
            "elif" | "elifdef" | "elifndef" => {
                if let Some(f) = stack.last_mut() {
                    line_on = f.parent;
                    if !f.known {
                        f.active = true;
                    } else if f.taken {
                        f.active = false;
                    } else if f.parent {
                        let cond = match kw {
                            "elifdef" => Some(lookup(first_ident(rest)).is_some()),
                            "elifndef" => Some(lookup(first_ident(rest)).is_none()),
                            _ => eval_cond(rest, &lookup),
                        };
                        match cond {
                            Some(c) => {
                                f.active = c;
                                f.taken = c;
                            }
                            None => {
                                // 后面的分支也说不准了
                                f.known = false;
                                f.active = true;
                            }
                        }
                    }
                }
            }
            "else" => {
                if let Some(f) = stack.last_mut() {
                    line_on = f.parent;
                    f.active = !f.known || !f.taken;
                    f.taken = true;
                }
            }
            "endif" => {
                if let Some(f) = stack.pop() {
                    line_on = f.parent;
                }
            }
            "define" if on_before => {
                let name = first_ident(rest).to_string();
                if !name.is_empty() {
                    let after = &rest[rest.find(name.as_str()).unwrap_or(0) + name.len()..];
                    // 函数式宏不参与条件计算
                    let val = if after.starts_with('(') { String::from("\u{0}") } else { after.trim().to_string() };
                    local.insert(name, Some(val));
                }
            }
            "undef" if on_before => {
                local.insert(first_ident(rest).to_string(), None);
            }
            _ => {}
        }
        dead[i] = !line_on;
        from = i + 1;
        on = is_on(&stack);
    }
    if !on && from < n_lines {
        dead[from..].iter_mut().for_each(|d| *d = true);
    }
    let mut out: Vec<(usize, usize)> = vec![];
    for (i, d) in dead.iter().enumerate() {
        if *d {
            match out.last_mut() {
                Some(l) if l.1 + 1 == i => l.1 = i,
                _ => out.push((i, i)),
            }
        }
    }
    out
}

fn first_ident(s: &str) -> &str {
    let s = s.trim_start();
    let n = s.find(|c: char| !is_ident(c)).unwrap_or(s.len());
    &s[..n]
}

/// 算 #if 后面的条件；算不出来返回 None
pub fn eval_cond(expr: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<bool> {
    let e = subst(expr, lookup, 0)?;
    crate::cindex::eval(&e).map(|v| v != 0)
}

fn subst(expr: &str, lookup: &dyn Fn(&str) -> Option<String>, depth: usize) -> Option<String> {
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
                out.push_str(if lookup(name).is_some() { " 1 " } else { " 0 " });
                i += skipped + used;
                continue;
            }
            // 整数后缀（如 10UL）已经在数字里处理；这里是真正的标识符
            if expr[i..].trim_start().starts_with('(') {
                return None; // 函数式宏
            }
            let v = lookup(w)?;
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

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn if0_and_else() {
        let t = "a\n#if 0\nb\nc\n#else\nd\n#endif\ne";
        assert_eq!(inactive(t, &none), vec![(2, 3)]);
        let t = "#if 1\na\n#else\nb\n#endif";
        assert_eq!(inactive(t, &none), vec![(3, 3)]);
    }

    #[test]
    fn ifdef_and_guard() {
        let t = "#ifndef FOO_H\n#define FOO_H\nint x;\n#ifdef DEBUG\nlog();\n#endif\n#endif";
        assert_eq!(inactive(t, &none), vec![(4, 4)]);
        let dbg = |n: &str| (n == "DEBUG").then(String::new);
        assert_eq!(inactive(t, &dbg), vec![]);
    }

    #[test]
    fn unknown_stays_active() {
        let t = "#if FOO > 3\na\n#else\nb\n#endif";
        assert_eq!(inactive(t, &none), vec![]);
        let foo = |n: &str| (n == "FOO").then(|| "2".to_string());
        assert_eq!(inactive(t, &foo), vec![(1, 1)]);
    }

    #[test]
    fn nested_and_elif() {
        let t = "#if 0\n#if 1\na\n#endif\nb\n#elif defined(X)\nc\n#elif 1\nd\n#else\ne\n#endif";
        // X 未定义：#elif defined(X) 不生效，#elif 1 生效，#else 不生效
        assert_eq!(inactive(t, &none), vec![(1, 4), (6, 6), (10, 10)]);
    }

    #[test]
    fn local_define() {
        let t = "#define MODE 2\n#if MODE == 2\na\n#else\nb\n#endif\n#undef MODE\n#ifdef MODE\nc\n#endif";
        assert_eq!(inactive(t, &none), vec![(4, 4), (8, 8)]);
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

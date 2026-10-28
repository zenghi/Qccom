//! JSON / YAML / TOML 树形查看（v1.8）：折叠展开、复制路径和值、点节点跳到原文那一行；
//! 格式错误标出行号。三种格式都用自己写的小解析器（记得每个值在第几行），YAML 只支持常见的缩进写法。

use eframe::egui::{self, vec2, FontId, RichText, Sense, Ui};

use crate::i18n::{t, tf};
use crate::theme::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    /// 数字原样保留
    Num(String),
    Str(String),
    Arr(Vec<Node>),
    Obj(Vec<(String, Node)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub v: Val,
    /// 从 0 开始的行号
    pub line: usize,
}

fn node(v: Val, line: usize) -> Node {
    Node { v, line }
}

// ---------------------------------------------------------------- JSON

struct Json<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl Json<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\n' => self.line += 1,
                b' ' | b'\t' | b'\r' => {}
                // JSONC 的注释也认（VS Code 的配置文件常用）
                b'/' if self.s.get(self.i + 1) == Some(&b'/') => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                    continue;
                }
                b'/' if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    while self.i + 1 < self.s.len() && !(self.s[self.i] == b'*' && self.s[self.i + 1] == b'/') {
                        if self.s[self.i] == b'\n' {
                            self.line += 1;
                        }
                        self.i += 1;
                    }
                    self.i += 2;
                    continue;
                }
                _ => return,
            }
            self.i += 1;
        }
    }

    fn err<T>(&self, m: &str) -> Result<T, (usize, String)> {
        Err((self.line, m.to_string()))
    }

    fn string(&mut self) -> Result<String, (usize, String)> {
        self.i += 1;
        let mut out = Vec::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' => {
                    let e = self.s.get(self.i + 1).copied().unwrap_or(b'"');
                    self.i += 2;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'u' => {
                            let h = std::str::from_utf8(self.s.get(self.i..self.i + 4).unwrap_or(b"003f")).unwrap_or("003f");
                            let ch = u32::from_str_radix(h, 16).ok().and_then(char::from_u32).unwrap_or('?');
                            out.extend(ch.to_string().as_bytes());
                            self.i += 4;
                        }
                        x => out.push(x),
                    }
                    continue;
                }
                b'\n' => return self.err(t("字符串没有结束")),
                _ => out.push(c),
            }
            self.i += 1;
        }
        self.err(t("字符串没有结束"))
    }

    fn value(&mut self) -> Result<Node, (usize, String)> {
        self.ws();
        let line = self.line;
        let Some(&c) = self.s.get(self.i) else { return self.err(t("内容不完整")) };
        match c {
            b'{' => {
                self.i += 1;
                let mut items = vec![];
                loop {
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        Some(b'"') => {}
                        _ => return self.err(t("这里应该是 \"键\" 或 }")),
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return self.err(t("键后面少了冒号"));
                    }
                    self.i += 1;
                    let v = self.value()?;
                    items.push((k, v));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err(t("少了逗号或 }")),
                    }
                }
                Ok(node(Val::Obj(items), line))
            }
            b'[' => {
                self.i += 1;
                let mut items = vec![];
                loop {
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        break;
                    }
                    items.push(self.value()?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err(t("少了逗号或 ]")),
                    }
                }
                Ok(node(Val::Arr(items), line))
            }
            b'"' => Ok(node(Val::Str(self.string()?), line)),
            _ => {
                let st = self.i;
                while self.i < self.s.len() && !matches!(self.s[self.i], b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                    self.i += 1;
                }
                let w = String::from_utf8_lossy(&self.s[st..self.i]).into_owned();
                match w.as_str() {
                    "true" => Ok(node(Val::Bool(true), line)),
                    "false" => Ok(node(Val::Bool(false), line)),
                    "null" => Ok(node(Val::Null, line)),
                    _ if w.parse::<f64>().is_ok() => Ok(node(Val::Num(w), line)),
                    _ => self.err(&tf("看不懂的值：{}", &[&w])),
                }
            }
        }
    }
}

pub fn parse_json(text: &str) -> Result<Node, (usize, String)> {
    let mut p = Json { s: text.as_bytes(), i: 0, line: 0 };
    let v = p.value()?;
    p.ws();
    // 结尾多余的逗号之类
    if p.i < p.s.len() {
        return p.err(t("值后面还有多余的内容"));
    }
    Ok(v)
}

// ---------------------------------------------------------------- 标量（YAML / TOML 共用）

fn scalar(s: &str, line: usize) -> Node {
    let s = s.trim();
    let v = if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2) || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2) {
        Val::Str(s[1..s.len() - 1].replace("\\n", "\n").replace("\\\"", "\""))
    } else if matches!(s, "true" | "True" | "yes") {
        Val::Bool(true)
    } else if matches!(s, "false" | "False" | "no") {
        Val::Bool(false)
    } else if matches!(s, "null" | "~" | "") {
        Val::Null
    } else if s.replace('_', "").parse::<f64>().is_ok() || (s.starts_with("0x") && i64::from_str_radix(&s[2..].replace('_', ""), 16).is_ok()) {
        Val::Num(s.to_string())
    } else {
        Val::Str(s.to_string())
    };
    node(v, line)
}

/// 去掉 # 注释（引号里的不算）
fn strip_comment(l: &str) -> &str {
    let mut q: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in l.char_indices() {
        match q {
            Some(x) if c == x => q = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => q = Some(c),
            None if c == '#' && (prev == ' ' || prev == '\t' || i == 0) => return &l[..i],
            None => {}
        }
        prev = c;
    }
    l
}

/// [a, b, "c"] 或 { k = v } 这类一行里的复合值
fn inline_val(s: &str, line: usize, toml: bool) -> Result<Node, (usize, String)> {
    let s = s.trim();
    let split = |inner: &str| -> Vec<String> {
        let mut parts = vec![];
        let (mut depth, mut q, mut cur) = (0i32, None::<char>, String::new());
        for c in inner.chars() {
            match q {
                Some(x) if c == x => q = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => q = Some(c),
                None if c == '[' || c == '{' => depth += 1,
                None if c == ']' || c == '}' => depth -= 1,
                None if c == ',' && depth == 0 => {
                    parts.push(std::mem::take(&mut cur));
                    continue;
                }
                None => {}
            }
            cur.push(c);
        }
        if !cur.trim().is_empty() {
            parts.push(cur);
        }
        parts
    };
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let items: Result<Vec<Node>, _> = split(inner).iter().map(|p| inline_val(p, line, toml)).collect();
        return Ok(node(Val::Arr(items?), line));
    }
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        let mut items = vec![];
        for p in split(inner) {
            let sep = if toml { '=' } else { ':' };
            let (k, v) = p.split_once(sep).ok_or((line, tf("少了 {}", &[&sep])))?;
            items.push((k.trim().trim_matches('"').to_string(), inline_val(v, line, toml)?));
        }
        return Ok(node(Val::Obj(items), line));
    }
    if s.starts_with('[') || s.starts_with('{') {
        return Err((line, t("括号没有配对").to_string()));
    }
    Ok(scalar(s, line))
}

// ---------------------------------------------------------------- TOML

fn obj_mut<'a>(root: &'a mut Node, path: &[String], line: usize, array_last: bool) -> Result<&'a mut Vec<(String, Node)>, (usize, String)> {
    let mut cur = root;
    for (i, k) in path.iter().enumerate() {
        let Val::Obj(items) = &mut cur.v else { return Err((line, tf("{} 不是表", &[k]))) };
        let pos = match items.iter().position(|(x, _)| x == k) {
            Some(p) => p,
            None => {
                items.push((k.clone(), node(Val::Obj(vec![]), line)));
                items.len() - 1
            }
        };
        let next = &mut items[pos].1;
        let into_arr = matches!(next.v, Val::Arr(_)) && (i + 1 < path.len() || array_last);
        cur = if into_arr {
            match &mut next.v {
                Val::Arr(a) => a.last_mut().ok_or((line, t("数组是空的").to_string()))?,
                _ => unreachable!(),
            }
        } else {
            next
        };
    }
    match &mut cur.v {
        Val::Obj(items) => Ok(items),
        _ => Err((line, t("这里应该是表").to_string())),
    }
}

fn key_path(s: &str) -> Vec<String> {
    s.split('.').map(|k| k.trim().trim_matches('"').to_string()).collect()
}

pub fn parse_toml(text: &str) -> Result<Node, (usize, String)> {
    let mut root = node(Val::Obj(vec![]), 0);
    let mut table: Vec<String> = vec![];
    let mut arr_table = false;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = i;
        let l = strip_comment(lines[i]).trim().to_string();
        i += 1;
        if l.is_empty() {
            continue;
        }
        if let Some(name) = l.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
            let path = key_path(name);
            let (last, parent) = path.split_last().ok_or((line, t("表名是空的").to_string()))?;
            let items = obj_mut(&mut root, parent, line, true)?;
            match items.iter_mut().find(|(k, _)| k == last) {
                Some((_, n)) => match &mut n.v {
                    Val::Arr(a) => a.push(node(Val::Obj(vec![]), line)),
                    _ => return Err((line, tf("{} 已经定义过，不是数组", &[last]))),
                },
                None => items.push((last.clone(), node(Val::Arr(vec![node(Val::Obj(vec![]), line)]), line))),
            }
            table = path;
            arr_table = true;
            continue;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            table = key_path(name);
            arr_table = false;
            obj_mut(&mut root, &table, line, false)?;
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            return Err((line, t("这一行应该是 键 = 值").to_string()));
        };
        let mut v = v.trim().to_string();
        // 跨行的数组和三引号字符串
        while (v.matches('[').count() > v.matches(']').count() || (v.starts_with("\"\"\"") && (v.len() < 6 || !v[3..].contains("\"\"\"")))) && i < lines.len() {
            v.push(if v.starts_with("\"\"\"") { '\n' } else { ' ' });
            v.push_str(strip_comment(lines[i]).trim());
            i += 1;
        }
        if let Some(inner) = v.strip_prefix("\"\"\"").and_then(|r| r.strip_suffix("\"\"\"")) {
            v = format!("\"{}\"", inner.trim_start_matches('\n'));
        }
        let val = inline_val(&v, line, true)?;
        let mut kp = key_path(k);
        let last = kp.pop().unwrap_or_default();
        let mut full = table.clone();
        full.extend(kp);
        let items = obj_mut(&mut root, &full, line, arr_table)?;
        if items.iter().any(|(x, _)| x == &last) {
            return Err((line, tf("键 {} 重复了", &[&last])));
        }
        items.push((last, val));
    }
    Ok(root)
}

// ---------------------------------------------------------------- YAML（缩进写法）

struct YLine {
    ind: usize,
    text: String,
    line: usize,
}

fn yaml_block(ls: &[YLine], i: &mut usize, ind: usize) -> Result<Node, (usize, String)> {
    let Some(first) = ls.get(*i) else { return Ok(node(Val::Null, 0)) };
    let line0 = first.line;
    if first.text.starts_with("- ") || first.text == "-" {
        let mut items = vec![];
        while *i < ls.len() && ls[*i].ind == ind && (ls[*i].text.starts_with("- ") || ls[*i].text == "-") {
            let l = &ls[*i];
            let rest = l.text[1..].trim().to_string();
            let line = l.line;
            *i += 1;
            if rest.is_empty() {
                let child_ind = ls.get(*i).map_or(ind + 2, |n| n.ind);
                items.push(if child_ind > ind { yaml_block(ls, i, child_ind)? } else { node(Val::Null, line) });
            } else if rest.contains(": ") || rest.ends_with(':') {
                // - key: v 开头的对象：后面同缩进 + 2 的行也属于它
                let item_ind = ind + 2;
                let mut sub = vec![YLine { ind: item_ind, text: rest, line }];
                while *i < ls.len() && ls[*i].ind >= item_ind {
                    sub.push(YLine {
                        ind: ls[*i].ind,
                        text: ls[*i].text.clone(),
                        line: ls[*i].line,
                    });
                    *i += 1;
                }
                let mut k = 0;
                items.push(yaml_block(&sub, &mut k, item_ind)?);
            } else {
                items.push(inline_val(&rest, line, false)?);
            }
        }
        return Ok(node(Val::Arr(items), line0));
    }
    let mut items: Vec<(String, Node)> = vec![];
    while *i < ls.len() && ls[*i].ind == ind {
        let l = &ls[*i];
        let line = l.line;
        let (k, v) = match l.text.split_once(": ") {
            Some((k, v)) => (k.to_string(), v.trim().to_string()),
            None if l.text.ends_with(':') => (l.text.trim_end_matches(':').to_string(), String::new()),
            None => return Err((line, t("这一行应该是 键: 值").to_string())),
        };
        *i += 1;
        let k = k.trim().trim_matches(['"', '\'']).to_string();
        let val = if v.is_empty() {
            match ls.get(*i) {
                Some(n) if n.ind > ind || (n.ind == ind && n.text.starts_with("- ")) => {
                    let ci = n.ind;
                    yaml_block(ls, i, ci)?
                }
                _ => node(Val::Null, line),
            }
        } else if v == "|" || v == ">" || v.starts_with("|-") || v.starts_with(">-") {
            let mut body = vec![];
            while *i < ls.len() && ls[*i].ind > ind {
                body.push(ls[*i].text.clone());
                *i += 1;
            }
            node(Val::Str(body.join(if v.starts_with('|') { "\n" } else { " " })), line)
        } else {
            inline_val(&v, line, false)?
        };
        items.push((k, val));
    }
    if *i < ls.len() && ls[*i].ind > ind {
        return Err((ls[*i].line, t("缩进不对").to_string()));
    }
    Ok(node(Val::Obj(items), line0))
}

pub fn parse_yaml(text: &str) -> Result<Node, (usize, String)> {
    let mut ls = vec![];
    for (n, raw) in text.lines().enumerate() {
        if raw.trim_start().starts_with('\t') || raw.starts_with('\t') {
            return Err((n, t("YAML 不能用 Tab 缩进").to_string()));
        }
        let l = strip_comment(raw);
        let tr = l.trim();
        if tr.is_empty() || tr == "---" || tr == "..." || tr.starts_with('%') {
            continue;
        }
        ls.push(YLine {
            ind: l.len() - l.trim_start().len(),
            text: tr.to_string(),
            line: n,
        });
    }
    if ls.is_empty() {
        return Ok(node(Val::Null, 0));
    }
    let mut i = 0;
    let ind = ls[0].ind;
    let v = yaml_block(&ls, &mut i, ind)?;
    if i < ls.len() {
        return Err((ls[i].line, t("缩进不对").to_string()));
    }
    Ok(v)
}

// ---------------------------------------------------------------- 界面

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Json,
    Yaml,
    Toml,
}

impl Kind {
    pub fn of(ext: &str) -> Option<Kind> {
        match ext {
            "json" | "jsonc" | "json5" => Some(Kind::Json),
            "yaml" | "yml" => Some(Kind::Yaml),
            "toml" => Some(Kind::Toml),
            _ => None,
        }
    }
}

pub enum TreeEvt {
    /// 跳到原文第几行（从 0 开始）
    Line(usize),
    Toast(String),
}

pub struct TreeView {
    kind: Kind,
    root: Result<Node, (usize, String)>,
    ver: u64,
    /// 展开的路径
    open: std::collections::HashSet<String>,
    filter: String,
}

impl TreeView {
    pub fn new(kind: Kind, text: &str) -> TreeView {
        let mut v = TreeView {
            kind,
            root: Ok(node(Val::Null, 0)),
            ver: u64::MAX,
            open: Default::default(),
            filter: String::new(),
        };
        v.update(text, 0);
        v.open.insert(String::new());
        v
    }

    pub fn update(&mut self, text: &str, ver: u64) {
        if ver == self.ver {
            return;
        }
        self.ver = ver;
        self.root = match self.kind {
            Kind::Json => parse_json(text),
            Kind::Yaml => parse_yaml(text),
            Kind::Toml => parse_toml(text),
        };
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<TreeEvt> {
        let mut evt = None;
        ui.add_space(8.0);
        let root = match &self.root {
            Ok(r) => r.clone(),
            Err((l, m)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tf("第 {} 行：{}", &[&(l + 1), m])).color(ERR()));
                    if ui.button(t("跳到这一行")).clicked() {
                        evt = Some(TreeEvt::Line(*l));
                    }
                });
                return evt;
            }
        };
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t("筛选键或值…")).desired_width(240.0));
            if ui.small_button(t("全部展开")).clicked() {
                let mut all = vec![];
                collect_paths(&root, "", &mut all);
                self.open.extend(all);
            }
            if ui.small_button(t("全部折叠")).clicked() {
                self.open.clear();
                self.open.insert(String::new());
            }
        });
        ui.add_space(4.0);
        let f = self.filter.to_lowercase();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.rows(ui, &root, "", t("根"), 0, &f, &mut evt);
        });
        evt
    }

    fn rows(&mut self, ui: &mut Ui, n: &Node, path: &str, key: &str, depth: usize, f: &str, evt: &mut Option<TreeEvt>) {
        let kids: Option<Vec<(String, String, &Node)>> = match &n.v {
            Val::Obj(items) => Some(items.iter().map(|(k, v)| (k.clone(), if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, v)).collect()),
            Val::Arr(items) => Some(items.iter().enumerate().map(|(i, v)| (format!("[{i}]"), format!("{path}[{i}]"), v)).collect()),
            _ => None,
        };
        if !f.is_empty() && !matches_filter(key, n, f) {
            return;
        }
        let open = self.open.contains(path) || !f.is_empty();
        let (val_text, col) = match &n.v {
            Val::Null => ("null".to_string(), TEXT3()),
            Val::Bool(b) => (b.to_string(), VIOLET()),
            Val::Num(s) => (s.clone(), crate::highlight::pal::NUM()),
            Val::Str(s) => (format!("\"{}\"", crate::app::truncate(s, 120)), crate::highlight::pal::STR()),
            Val::Arr(a) => (if open { String::new() } else { tf("[{} 项]", &[&a.len()]) }, TEXT3()),
            Val::Obj(o) => (if open { String::new() } else { format!("{{{}}}", tf("{} 个键", &[&o.len()])) }, TEXT3()),
        };
        let h = 22.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(400.0), h), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, tint(6));
        }
        let x = rect.left() + 8.0 + depth as f32 * 16.0;
        let p = ui.painter();
        if kids.is_some() {
            crate::codeview::chevron(p, egui::pos2(x + 5.0, rect.center().y), open, TEXT3());
        }
        let mono = FontId::monospace(12.5);
        let g = p.layout_no_wrap(key.to_string(), mono.clone(), if depth == 0 { TEXT3() } else { ACCENT() });
        let kw = g.size().x;
        p.galley(egui::pos2(x + 14.0, rect.center().y - g.size().y / 2.0), g, TEXT());
        if !val_text.is_empty() {
            p.text(egui::pos2(x + 14.0 + kw + 10.0, rect.center().y), egui::Align2::LEFT_CENTER, val_text, mono, col);
        }
        if resp.clicked() && kids.is_some() {
            if !self.open.remove(path) {
                self.open.insert(path.to_string());
            }
        }
        if resp.double_clicked() {
            *evt = Some(TreeEvt::Line(n.line));
        }
        let path_s = path.to_string();
        resp.context_menu(|ui| {
            if ui.button(t("复制路径")).clicked() {
                ui.ctx().copy_text(path_s.clone());
                *evt = Some(TreeEvt::Toast(tf("已复制 {}", &[&path_s])));
                ui.close_menu();
            }
            if ui.button(t("复制值")).clicked() {
                ui.ctx().copy_text(match &n.v {
                    Val::Str(s) => s.clone(),
                    Val::Num(s) => s.clone(),
                    Val::Bool(b) => b.to_string(),
                    Val::Null => "null".into(),
                    _ => to_json(n),
                });
                ui.close_menu();
            }
            if ui.button(t("跳到原文")).clicked() {
                *evt = Some(TreeEvt::Line(n.line));
                ui.close_menu();
            }
        });
        if open {
            if let Some(k) = kids {
                for (key, p, child) in k.into_iter().take(5000) {
                    self.rows(ui, child, &p, &key, depth + 1, f, evt);
                }
            }
        }
    }
}

fn matches_filter(key: &str, n: &Node, f: &str) -> bool {
    if key.to_lowercase().contains(f) {
        return true;
    }
    match &n.v {
        Val::Str(s) | Val::Num(s) => s.to_lowercase().contains(f),
        Val::Obj(items) => items.iter().any(|(k, v)| matches_filter(k, v, f)),
        Val::Arr(items) => items.iter().any(|v| matches_filter("", v, f)),
        _ => false,
    }
}

fn collect_paths(n: &Node, path: &str, out: &mut Vec<String>) {
    match &n.v {
        Val::Obj(items) => {
            out.push(path.to_string());
            for (k, v) in items {
                collect_paths(v, &if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, out);
            }
        }
        Val::Arr(items) => {
            out.push(path.to_string());
            for (i, v) in items.iter().enumerate() {
                collect_paths(v, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn to_json(n: &Node) -> String {
    match &n.v {
        Val::Null => "null".into(),
        Val::Bool(b) => b.to_string(),
        Val::Num(s) => s.clone(),
        Val::Str(s) => serde_json::to_string(s).unwrap_or_default(),
        Val::Arr(a) => format!("[{}]", a.iter().map(to_json).collect::<Vec<_>>().join(", ")),
        Val::Obj(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", serde_json::to_string(k).unwrap_or_default(), to_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(n: &'a Node, k: &str) -> &'a Node {
        match &n.v {
            Val::Obj(items) => &items.iter().find(|(x, _)| x == k).unwrap().1,
            _ => panic!("not obj"),
        }
    }

    #[test]
    fn parsers() {
        let j = parse_json("{\n  \"a\": [1, 2.5, true],\n  // c\n  \"b\": {\"c\": null}\n}").unwrap();
        assert_eq!(get(&j, "b").line, 3);
        assert_eq!(parse_json("{\n\"a\": 1,\n\"b\" 2}").unwrap_err().0, 2);
        let tm = parse_toml("title = \"x\"\n[server]\nport = 8080 # c\nhosts = [\n \"a\",\n \"b\"]\n[[bin]]\nname = \"q\"\n[[bin]]\nname = \"r\"\n").unwrap();
        assert_eq!(get(get(&tm, "server"), "port").v, Val::Num("8080".into()));
        match &get(&tm, "bin").v {
            Val::Arr(a) => assert_eq!(a.len(), 2),
            _ => panic!(),
        }
        assert!(parse_toml("a = 1\na = 2").is_err());
        let y = parse_yaml("name: test\ndeps:\n  - idf: \">=5.0\"\n    public: true\n  - lvgl\nmap:\n  k: [1, 2]\n  s: |\n    line1\n    line2\n").unwrap();
        match &get(&y, "deps").v {
            Val::Arr(a) => {
                assert_eq!(a.len(), 2);
                assert_eq!(get(&a[0], "public").v, Val::Bool(true));
            }
            _ => panic!(),
        }
        assert_eq!(get(get(&y, "map"), "s").v, Val::Str("line1\nline2".into()));
        assert!(parse_yaml("a: 1\n\tb: 2").is_err());
    }
}

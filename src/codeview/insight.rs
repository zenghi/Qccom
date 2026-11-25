//! 借鉴 Source Insight 的两个辅助窗口：
//! - 上下文窗口：光标停在哪个符号上，底部就显示它的定义，不用跳过去也能看；
//! - 调用关系：当前函数调用了谁、被谁调用，可以一层层展开，点一下跳过去。

use super::*;
use crate::app::tab_strip;
use crate::highlight::{tokenize, Tok};

#[derive(Clone)]
pub(super) struct Call {
    name: String,
    path: PathBuf,
    line: u32,
    /// 被调用：同一个函数里调用了几次
    count: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum RelTab {
    Callees,
    Callers,
}

struct CtxView {
    word: String,
    path: PathBuf,
    line: u32,
    kind: Kind,
    local: Option<locals::Local>,
    version: Option<u64>,
    first: u32,
    lines: Vec<String>,
    lang: Lang,
}

pub(super) struct Insight {
    ctx_source: Option<(PathBuf, u64, usize, String)>,
    ctx_dirty: bool,
    ctx_retry: Option<(String, PathBuf, u32, bool, Kind)>,
    ctx_since: f64,
    ctx: Option<CtxView>,
    tab: RelTab,
    root: Option<String>,
    pinned: bool,
    /// 上下文窗口固定：不随光标变化（单击预览别的符号时自动固定）
    ctx_pinned: bool,
    callees: HashMap<String, Vec<Call>>,
    callers: HashMap<String, Option<Vec<Call>>>,
    callers_rx: Vec<(String, Receiver<Vec<Call>>)>,
    open: HashSet<String>,
}

impl Default for Insight {
    fn default() -> Self {
        Insight {
            ctx_source: None,
            ctx_dirty: false,
            ctx_retry: None,
            ctx_since: 0.0,
            ctx: None,
            tab: RelTab::Callees,
            root: None,
            pinned: false,
            ctx_pinned: false,
            callees: HashMap::new(),
            callers: HashMap::new(),
            callers_rx: vec![],
            open: HashSet::new(),
        }
    }
}

impl Insight {
    pub(super) fn context_changed(&mut self) {
        self.ctx_dirty = true;
    }
}

const C_NOT_CALLS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "sizeof",
    "defined",
    "do",
    "else",
    "case",
    "typeof",
    "__attribute__",
    "_Static_assert",
    "alignof",
    "offsetof",
];

impl CodeView {
    /// 索引变了（重建）时清掉算好的关系
    pub(super) fn insight_reset(&mut self) {
        self.ins.callees.clear();
        self.ins.callers.clear();
        self.ins.callers_rx.clear();
        self.ins.context_changed();
    }

    fn file_text(&self, p: &Path) -> Option<String> {
        if let Some(d) = self.tabs.iter().find(|d| d.path == p) {
            return Some(d.buf.text.clone());
        }
        cindex::read_text(p)
    }

    /// 函数（或函数式宏）的定义位置
    fn func_def(&self, name: &str) -> Option<(PathBuf, u32, Kind)> {
        self.symbol_sites(name)
            .into_iter()
            .find(|(_, s)| matches!(s.kind, Kind::Func | Kind::Macro))
            .map(|(p, s)| (p.to_path_buf(), s.line, s.kind))
    }

    /// 光标所在的函数（光标停在函数名上时就是那个函数）
    fn func_at_cursor(&self) -> Option<String> {
        let d = self.tabs.get(self.active)?;
        if let Some(w) = self.cursor_word() {
            if self.func_def(&w).is_some_and(|f| f.2 == Kind::Func) {
                return Some(w);
            }
        }
        let l = d.cursor.0 as u32;
        let f = d.syms.iter().filter(|s| s.kind == Kind::Func && s.line <= l).max_by_key(|s| s.line)?;
        // 光标要在函数体里（简单判断：函数之后、下一个函数之前）
        Some(f.name.clone())
    }

    fn callees_of(&mut self, name: &str) -> Vec<Call> {
        if let Some(v) = self.ins.callees.get(name) {
            return v.clone();
        }
        let mut out: Vec<Call> = vec![];
        if let Some((path, line, Kind::Func)) = self.func_def(name) {
            if let Some(text) = self.file_text(&path) {
                let lang = Lang::from_path(&path);
                let toks = tokenize(&text, lang);
                // 定义行之后第一个 { 到配对的 }
                let line_start = text.split('\n').take(line as usize).map(|l| l.len() + 1).sum::<usize>();
                let mut depth = 0i32;
                let mut body: Option<(usize, usize)> = None;
                let mut open_at = None;
                'outer: for &(s, e, k) in toks.iter().filter(|t| t.0 >= line_start) {
                    if k != Tok::Punct {
                        continue;
                    }
                    for (i, c) in text[s..e].char_indices() {
                        match c {
                            '{' => {
                                if depth == 0 {
                                    open_at = Some(s + i);
                                }
                                depth += 1;
                            }
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    body = open_at.map(|o| (o, s + i));
                                    break 'outer;
                                }
                            }
                            ';' if depth == 0 => break 'outer,
                            _ => {}
                        }
                    }
                }
                if let Some((a, b)) = body {
                    let sem = self.sem.clone();
                    let mut seen = HashSet::new();
                    for (i, &(s, e, k)) in toks.iter().enumerate() {
                        if s < a || e > b || !matches!(k, Tok::Func | Tok::Text | Tok::Macro) {
                            continue;
                        }
                        let w = &text[s..e];
                        if !w.starts_with(|c: char| is_ident(c) && !c.is_ascii_digit()) || C_NOT_CALLS.contains(&w) {
                            continue;
                        }
                        // 后面紧跟 (
                        let next = toks[i + 1..].iter().find(|t| !text[t.0..t.1].trim().is_empty());
                        if !next.is_some_and(|t| text[t.0..t.1].trim_start().starts_with('(')) {
                            continue;
                        }
                        let known = sem.as_ref().and_then(|m| m.get(w)).copied();
                        if !matches!(known, Some(Kind::Func | Kind::Proto | Kind::Macro)) {
                            continue;
                        }
                        if !seen.insert(w.to_string()) {
                            if let Some(c) = out.iter_mut().find(|c| c.name == w) {
                                c.count += 1;
                            }
                            continue;
                        }
                        let (p, l) = self.func_def(w).map(|f| (f.0, f.1)).unwrap_or((path.clone(), text[..s].matches('\n').count() as u32));
                        out.push(Call {
                            name: w.to_string(),
                            path: p,
                            line: l,
                            count: 1,
                        });
                    }
                }
            }
        }
        self.ins.callees.insert(name.to_string(), out.clone());
        out
    }

    /// 被谁调用：后台搜索全工程，第一次请求时返回 None
    fn callers_of(&mut self, name: &str, ctx: &egui::Context) -> Option<Vec<Call>> {
        if let Some(v) = self.ins.callers.get(name) {
            return v.clone();
        }
        let (Some(root), Some(idx)) = (self.root.clone(), self.index.clone()) else { return Some(vec![]) };
        let (tx, rx) = mpsc::channel();
        let n = name.to_string();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let re = crate::pattern::Pat {
                q: n.clone(),
                case: true,
                word: true,
                regex: false,
            }
            .compile();
            let hits = re.map(|re| cindex::search(&root, &idx.files, &re, &cancel, 5000)).unwrap_or_default();
            // 每个文件里的函数（按行排好），用来找调用点所在的函数
            let mut funcs: HashMap<PathBuf, Vec<(u32, String)>> = HashMap::new();
            for s in idx.syms.iter().filter(|s| s.kind == Kind::Func) {
                funcs.entry(idx.path_of(s).to_path_buf()).or_default().push((s.line, s.name.clone()));
            }
            for v in funcs.values_mut() {
                v.sort();
            }
            let mut out: Vec<Call> = vec![];
            for h in hits {
                let path = root.join(&h.path);
                // 只要 name( 这种调用形式，排除定义、声明本身
                let Some(at) = find_word(&h.text, &n) else { continue };
                if !h.text[at + n.len()..].trim_start().starts_with('(') {
                    continue;
                }
                let list = funcs.get(&path).map(|v| v.as_slice()).unwrap_or(&[]);
                if list.iter().any(|(l, f)| *l == h.line && *f == n) {
                    continue;
                }
                if is_decl(&h.text[..at]) {
                    continue;
                }
                let caller = list.iter().filter(|(l, _)| *l <= h.line).last().map(|(_, f)| f.clone()).unwrap_or_else(|| t_file_scope());
                match out.iter_mut().find(|c| c.name == caller && c.path == path) {
                    Some(c) => c.count += 1,
                    None => out.push(Call {
                        name: caller,
                        path,
                        line: h.line,
                        count: 1,
                    }),
                }
            }
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.ins.callers.insert(name.to_string(), None);
        self.ins.callers_rx.push((name.to_string(), rx));
        None
    }

    pub(super) fn poll_insight(&mut self) {
        let mut done = vec![];
        for (i, (n, rx)) in self.ins.callers_rx.iter().enumerate() {
            if let Ok(v) = rx.try_recv() {
                self.ins.callers.insert(n.clone(), Some(v));
                done.push(i);
            }
        }
        for i in done.into_iter().rev() {
            self.ins.callers_rx.remove(i);
        }
    }

    // ------------------------------------------------------------ 上下文窗口

    fn update_context(&mut self, now: f64) {
        let dirty = std::mem::take(&mut self.ins.ctx_dirty);
        if self.ins.ctx_pinned {
            let target = self
                .ins
                .ctx
                .as_ref()
                .and_then(|c| {
                    let version = self.tabs.iter().find(|d| d.path == c.path).map(|d| d.buf.ver);
                    (dirty || version != c.version).then(|| (c.word.clone(), c.path.clone(), c.line, c.local.is_some(), c.kind))
                })
                .or_else(|| if dirty { self.ins.ctx_retry.take() } else { None });
            if let Some((word, path, line, local, kind)) = target {
                self.ins.ctx = if local {
                    self.refresh_local_context(&word, &path, line)
                } else {
                    self.symbol_sites(&word)
                        .into_iter()
                        .find(|(p, s)| *p == path && kind == s.kind)
                        .map(|(_, s)| (path.clone(), s.line, s.kind))
                        .or_else(|| self.ctx_def(&word))
                        .and_then(|site| self.build_ctx(&word, site, None))
                };
                self.ins.ctx_retry = self.ins.ctx.is_none().then_some((word, path, line, local, kind));
            }
            return;
        }
        let source = self.tabs.get(self.active).and_then(|d| {
            let (byte, _, word) = d.word_at(d.ed.head())?;
            Some((&d.path, d.buf.ver, byte, word))
        });
        let same = source.is_none() && self.ins.ctx_source.is_none()
            || source
                .as_ref()
                .is_some_and(|(path, ver, at, word)| self.ins.ctx_source.as_ref().is_some_and(|(p, v, b, w)| p == *path && *v == *ver && *b == *at && w == *word));
        if !same {
            self.ins.ctx_source = source.map(|(p, v, b, w)| (p.clone(), v, b, w.to_owned()));
            self.ins.ctx = None;
            self.ins.ctx_since = now;
            return;
        }
        if now - self.ins.ctx_since < 0.25 || self.ins.ctx.is_some() && !dirty {
            return;
        }
        if let Some((path, _, byte, word)) = self.ins.ctx_source.clone() {
            self.ins.ctx = self.context_at(&word, &path, byte);
        }
    }

    fn context_at(&self, word: &str, path: &Path, byte: usize) -> Option<CtxView> {
        if let Some(d) = self.tabs.iter().find(|d| d.path == path) {
            if let Some(local) = d.locals.find(d, word, byte) {
                return self.build_ctx(word, (d.path.clone(), local.line, Kind::Var), Some(local));
            }
        }
        self.ctx_def(word).and_then(|site| self.build_ctx(word, site, None))
    }

    fn refresh_local_context(&self, word: &str, path: &Path, line: u32) -> Option<CtxView> {
        let d = self.tabs.iter().find(|d| d.path == path)?;
        let last = d.buf.lines().saturating_sub(1);
        let line = (line as usize).min(last);
        let lines = (0..=64).flat_map(|n| [line.checked_sub(n), (line + n <= last).then_some(line + n)]);
        for i in lines.flatten() {
            for (offset, _) in d.buf.line(i).match_indices(word) {
                let byte = d.buf.start(i) + offset;
                if let Some(local) = d.locals.find(d, word, byte).filter(|local| local.span.start == byte) {
                    return self.build_ctx(word, (d.path.clone(), local.line, Kind::Var), Some(local));
                }
            }
        }
        None
    }

    /// Pin an explicit definition selected from the call hierarchy.
    pub(super) fn preview_ctx(&mut self, w: &str, at: Option<(PathBuf, u32)>) {
        let site = match at {
            Some((p, l)) => Some((p, l, self.ctx_def(w).map_or(Kind::Func, |s| s.2))),
            None => self.ctx_def(w),
        };
        if let Some(c) = site.and_then(|site| self.build_ctx(w, site, None)) {
            self.ins.ctx = Some(c);
            self.ins.ctx_pinned = true;
            self.ins.ctx_retry = None;
            self.s.context_win = true;
        }
    }

    fn build_ctx(&self, w: &str, (path, line, kind): (PathBuf, u32, Kind), local: Option<locals::Local>) -> Option<CtxView> {
        let text = self.file_text(&path)?;
        let lang = Lang::from_path(&path);
        let all: Vec<&str> = text.split('\n').collect();
        let l = line as usize;
        if l >= all.len() {
            return None;
        }
        // 函数、结构体、枚举显示到配对的 }；宏显示续行；其他显示前后几行
        let end = match kind {
            Kind::Func | Kind::Struct | Kind::Enum | Kind::Class | Kind::Typedef => {
                let mut depth = 0i32;
                let mut seen = false;
                let mut e = l;
                for (i, ln) in all.iter().enumerate().skip(l).take(400) {
                    for c in ln.chars() {
                        if c == '{' {
                            depth += 1;
                            seen = true;
                        } else if c == '}' {
                            depth -= 1;
                        }
                    }
                    e = i;
                    if (seen && depth <= 0) || (!seen && ln.trim_end().ends_with(';')) {
                        break;
                    }
                }
                e
            }
            Kind::Macro => {
                let mut e = l;
                while e + 1 < all.len() && all[e].trim_end().ends_with('\\') && e - l < 60 {
                    e += 1;
                }
                e
            }
            _ => (l + 3).min(all.len().saturating_sub(1)),
        };
        // 定义前面的注释也带上
        let mut first = l;
        while first > 0 && l - first < 12 {
            let t = all[first - 1].trim_start();
            if t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
                first -= 1;
            } else {
                break;
            }
        }
        let lines = all[first..=end.min(all.len() - 1)].iter().map(|s| s.to_string()).collect();
        let version = self.tabs.iter().find(|d| d.path == path).map(|d| d.buf.ver);
        Some(CtxView {
            word: w.to_owned(),
            path,
            line,
            kind,
            local,
            version,
            first: first as u32,
            lines,
            lang,
        })
    }

    fn ctx_def(&self, w: &str) -> Option<(PathBuf, u32, Kind)> {
        self.symbol_sites(w).first().map(|(p, s)| (p.to_path_buf(), s.line, s.kind))
    }

    pub(super) fn context_ui(&mut self, ui: &mut Ui) {
        ui.set_min_height(ui.available_height());
        let now = ui.input(|i| i.time);
        self.update_context(now);
        if self.ins.ctx_since + 0.3 > now {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        }
        let mut go = None;
        let mut close = false;
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new(t("上下文")).size(12.0).color(TEXT3()));
            if let Some(c) = &self.ins.ctx {
                ui.add_space(4.0);
                if c.local.is_some() {
                    ui.label(RichText::new(t("局部变量")).small().color(ACCENT()));
                } else {
                    kind_pill(ui, c.kind);
                }
                ui.label(RichText::new(&c.word).font(FontId::new(12.5, FontFamily::Monospace)).color(TEXT()));
                let rel = self.root.as_ref().and_then(|r| c.path.strip_prefix(r).ok()).unwrap_or(&c.path).to_string_lossy().replace('\\', "/");
                let r = ui.add(egui::Label::new(RichText::new(format!("{rel}:{}", c.line + 1)).size(11.5).color(ACCENT())).sense(Sense::click()));
                if r.on_hover_text(t("打开定义")).clicked() {
                    go = Some((c.path.clone(), c.line, c.word.clone(), c.local.clone()));
                }
            } else {
                ui.label(RichText::new(t("把光标放在变量、函数、宏或类型上，这里显示其定义")).size(11.5).color(TEXT3()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if icon_button(ui, Icon::Close, t("关闭"), false).clicked() {
                    close = true;
                }
                let pin_tip = if self.ins.ctx_pinned {
                    t("已固定：不随光标变化（点击取消）")
                } else {
                    t("固定当前内容")
                };
                if icon_button(ui, Icon::Bookmark, pin_tip, self.ins.ctx_pinned).clicked() {
                    self.ins.ctx_pinned = !self.ins.ctx_pinned;
                    if !self.ins.ctx_pinned {
                        self.ins.ctx_source = None;
                        self.ins.ctx_retry = None;
                        self.ins.ctx = None;
                    }
                }
                ui.label(
                    RichText::new(if self.ins.ctx_pinned { t("已固定") } else { t("跟随光标") })
                        .size(11.0)
                        .color(if self.ins.ctx_pinned { ACCENT() } else { TEXT2() }),
                );
            });
        });
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, BG0());
        let mut preview = None;
        if let Some(c) = &self.ins.ctx {
            let font = FontId::new((self.s.font_size - 1.0).max(9.0), FontFamily::Monospace);
            let text = c.lines.join("\n").replace('\t', "    ");
            let g = ui.fonts(|f| f.layout_job(highlight::layout(&text, c.lang, font.clone())));
            let row_h = g.size().y / g.rows.len().max(1) as f32;
            egui::ScrollArea::both().id_salt(("ctx-sa", &c.word)).auto_shrink([false, false]).show(ui, |ui| {
                let gutter = 48.0;
                let (r, resp) = ui.allocate_exact_size(vec2((g.size().x + gutter + 40.0).max(ui.available_width()), g.size().y + 12.0), Sense::click());
                let top = r.top() + 6.0;
                // 单击符号：在这里预览它的定义；双击：跳到这一行
                if let Some(pp) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.double_clicked()) {
                    let row = (((pp.y - top) / row_h).max(0.0) as usize).min(c.lines.len().saturating_sub(1));
                    let cur = g.cursor_from_pos(pp - pos2(r.left() + gutter, top));
                    let chars: Vec<char> = text.chars().collect();
                    let ci = cur.ccursor.index.min(chars.len());
                    let mut a = ci;
                    while a > 0 && is_ident(chars[a - 1]) {
                        a -= 1;
                    }
                    let mut b = ci;
                    while b < chars.len() && is_ident(chars[b]) {
                        b += 1;
                    }
                    let word: String = chars[a..b].iter().collect();
                    if resp.double_clicked() {
                        go = Some((c.path.clone(), c.first + row as u32, word, None));
                    } else if !word.is_empty() && word.starts_with(|ch: char| !ch.is_ascii_digit()) && word != c.word {
                        // Map the display column back to the original source (tabs expand to four spaces).
                        let display = &chars[..a];
                        let column = display.iter().rev().take_while(|&&ch| ch != '\n').count();
                        let src = &c.lines[row];
                        let mut cells = 0;
                        let column_byte = src.char_indices().find_map(|(byte, ch)| {
                            let previous = cells;
                            cells += if ch == '\t' { 4 } else { 1 };
                            (previous == column).then_some(byte)
                        });
                        preview = Some((word, c.path.clone(), c.first as usize + row, column_byte));
                    }
                }
                let p = ui.painter();
                let hl = (c.line - c.first) as f32;
                p.rect_filled(Rect::from_min_size(pos2(r.left(), top + hl * row_h), vec2(r.width(), row_h)), 0.0, ACCENT().gamma_multiply(0.09));
                for i in 0..c.lines.len() {
                    p.text(
                        pos2(r.left() + gutter - 12.0, top + i as f32 * row_h + row_h / 2.0),
                        Align2::RIGHT_CENTER,
                        (c.first as usize + i + 1).to_string(),
                        FontId::new(font.size * 0.9, FontFamily::Monospace),
                        TEXT3(),
                    );
                }
                p.galley(pos2(r.left() + gutter, top), g, TEXT());
            });
        }
        if close {
            self.s.context_win = false;
        }
        if let Some((word, path, line, column)) = preview {
            let byte = self.tabs.iter().find(|d| d.path == path).and_then(|d| Some(d.buf.start(line) + column?));
            if let Some(c) = byte.and_then(|b| self.context_at(&word, &path, b)) {
                self.ins.ctx = Some(c);
                self.ins.ctx_pinned = true;
            } else {
                self.preview_ctx(&word, None);
            }
        }
        if let Some((p, l, w, local)) = go {
            if let Some(local) = local {
                self.open_file(&p, None);
                if let Some(d) = self.tabs.get_mut(self.active).filter(|d| d.path == p) {
                    d.goto = Some((l as usize, Some((local.span.start, local.span.end))));
                }
            } else {
                self.open_at_word(&p, l as usize, &w);
            }
        }
    }

    // ------------------------------------------------------------ 调用关系

    pub(super) fn relations_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.ins.pinned {
            if let Some(f) = self.func_at_cursor() {
                self.ins.root = Some(f);
            }
        }
        let mut close = false;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(t("调用关系")).size(13.0).color(TEXT()).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if icon_button(ui, Icon::Close, t("关闭"), false).clicked() {
                    close = true;
                }
                let pin_tip = if self.ins.pinned { t("已固定：不随光标变化") } else { t("固定当前函数") };
                if icon_button(ui, Icon::Bookmark, pin_tip, self.ins.pinned).clicked() {
                    self.ins.pinned = !self.ins.pinned;
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.allocate_ui(vec2(ui.available_width() - 12.0, 26.0), |ui| {
                tab_strip(ui, &mut self.ins.tab, &[(RelTab::Callees, t("调用了")), (RelTab::Callers, t("被调用"))], 26.0);
            });
        });
        ui.add_space(6.0);
        let Some(root) = self.ins.root.clone() else {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(t("把光标放进一个函数里")).size(12.0).color(TEXT3()));
            });
            if close {
                self.relations = false;
            }
            return;
        };
        if self.index.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(t("正在建立索引…")).size(12.0).color(TEXT3()));
            });
        }
        let mut go: Option<(PathBuf, u32, String)> = None;
        egui::ScrollArea::vertical().id_salt("rel-sa").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            // 根：当前函数
            let (r, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
            let p = ui.painter();
            if resp.hovered() {
                p.rect_filled(r, 0.0, BG3());
            }
            kind_badge(p, pos2(r.left() + 22.0, r.center().y), Kind::Func);
            p.text(pos2(r.left() + 36.0, r.center().y), Align2::LEFT_CENTER, &root, FontId::new(12.5, FontFamily::Monospace), TEXT());
            if resp.double_clicked() {
                if let Some((pp, l, _)) = self.func_def(&root) {
                    go = Some((pp, l, root.clone()));
                }
            } else if resp.clicked() {
                let def = self.func_def(&root).map(|f| (f.0, f.1));
                self.preview_ctx(&root, def);
            }
            let mut stack = vec![root.clone()];
            self.rel_rows(ui, &ctx, &root, 1, &mut stack, &mut go);
        });
        if close {
            self.relations = false;
        }
        if let Some((p, l, w)) = go {
            self.open_at_word(&p, l as usize, &w);
        }
    }

    fn rel_rows(&mut self, ui: &mut Ui, ctx: &egui::Context, name: &str, depth: usize, stack: &mut Vec<String>, go: &mut Option<(PathBuf, u32, String)>) {
        let items = match self.ins.tab {
            RelTab::Callees => Some(self.callees_of(name)),
            RelTab::Callers => self.callers_of(name, ctx),
        };
        let w = ui.available_width();
        let indent = 14.0 * depth as f32 + 8.0;
        let Some(items) = items else {
            let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
            ui.painter()
                .text(pos2(r.left() + indent + 18.0, r.center().y), Align2::LEFT_CENTER, t("正在搜索…"), FontId::proportional(11.5), TEXT3());
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        if items.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
            let msg = if self.ins.tab == RelTab::Callees {
                t("没有调用其他函数")
            } else {
                t("没有找到调用者")
            };
            ui.painter()
                .text(pos2(r.left() + indent + 18.0, r.center().y), Align2::LEFT_CENTER, msg, FontId::proportional(11.5), TEXT3());
            return;
        }
        for c in items {
            let key = format!("{}/{}", stack.join("/"), c.name);
            let recursive = stack.contains(&c.name);
            let expandable = depth < 8 && !recursive && (self.ins.tab == RelTab::Callers || self.func_def(&c.name).is_some_and(|f| f.2 == Kind::Func));
            let open = self.ins.open.contains(&key) && expandable;
            let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
            let p = ui.painter();
            if resp.hovered() {
                p.rect_filled(r, 0.0, BG3());
            }
            let x = r.left() + indent;
            if expandable {
                let cr = Rect::from_center_size(pos2(x + 6.0, r.center().y), vec2(14.0, 14.0));
                let s = 3.0;
                let cc = cr.center();
                let pts = if open {
                    vec![cc + vec2(-s, -s * 0.5), cc + vec2(s, -s * 0.5), cc + vec2(0.0, s * 0.7)]
                } else {
                    vec![cc + vec2(-s * 0.5, -s), cc + vec2(s * 0.7, 0.0), cc + vec2(-s * 0.5, s)]
                };
                p.add(Shape::line(pts, st(1.3, TEXT3())));
            }
            let kind = self.sem.as_ref().and_then(|m| m.get(&c.name)).copied().unwrap_or(Kind::Func);
            kind_badge(p, pos2(x + 22.0, r.center().y), if kind == Kind::Proto { Kind::Func } else { kind });
            let g = p.layout_no_wrap(c.name.clone(), FontId::new(12.0, FontFamily::Monospace), if recursive { TEXT3() } else { TEXT() });
            let nw = g.size().x;
            p.galley(pos2(x + 34.0, r.center().y - g.size().y / 2.0), g, TEXT());
            let file = c.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let extra = if c.count > 1 {
                format!("{file}:{}  ×{}", c.line + 1, c.count)
            } else {
                format!("{file}:{}", c.line + 1)
            };
            let ex = (x + 34.0 + nw + 10.0).min(r.right() - 20.0);
            p.with_clip_rect(r).text(pos2(ex, r.center().y), Align2::LEFT_CENTER, extra, FontId::proportional(11.0), TEXT3());
            let chevron_hit = resp.interact_pointer_pos().is_some_and(|pp| pp.x < x + 14.0);
            if resp.clicked() {
                if chevron_hit && expandable {
                    if !self.ins.open.remove(&key) {
                        self.ins.open.insert(key.clone());
                    }
                } else {
                    // 单击：在上下文窗口预览；双击：跳过去
                    let def = self.func_def(&c.name).map(|f| (f.0, f.1));
                    self.preview_ctx(&c.name, def);
                }
            }
            if resp.double_clicked() && !chevron_hit {
                *go = Some((c.path.clone(), c.line, c.name.clone()));
            }
            if open {
                stack.push(c.name.clone());
                self.rel_rows(ui, ctx, &c.name, depth + 1, stack, go);
                stack.pop();
            }
        }
    }
}

/// 调用点前面只有类型名、修饰符（没有运算符、括号、return），说明这一行是声明或定义本身
fn is_decl(before: &str) -> bool {
    let b = before.trim();
    if b.is_empty() {
        return false;
    }
    let words: Vec<&str> = b.split(|c: char| c.is_whitespace() || c == '*').filter(|w| !w.is_empty()).collect();
    !words.is_empty() && words.iter().all(|w| w.chars().all(|c| is_ident(c)) && !matches!(*w, "return" | "else" | "case" | "do" | "goto"))
}

fn t_file_scope() -> String {
    t("(文件作用域)").to_string()
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn same_word_in_separate_functions_refreshes_context_and_f12() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("setup.c");
        let text = "void a(void) { int count = 1; send(count); }\nvoid b(void) { long count = 2; send(count); }\n";
        std::fs::write(&path, text).unwrap();
        let mut view = CodeView::new(CodeSettings::default());
        view.tabs.push(Doc::load(&path, tmp.path()).unwrap());
        for (time, byte, expected) in [(0.0, text.find("send(count").unwrap() + 5, 0), (1.0, text.rfind("send(count").unwrap() + 5, 1)] {
            view.tabs[0].ed.select(byte, byte);
            view.update_context(time);
            view.update_context(time + 0.4);
            let context = view.ins.ctx.as_ref().unwrap();
            assert!(context.local.is_some());
            assert_eq!(context.line, expected);
            view.goto_definition("count", pos2(0., 0.), Some(byte), time + 0.5);
            let (line, span) = view.tabs[0].goto.unwrap();
            assert_eq!(line, expected as usize);
            let start = text[..byte].rfind("count").unwrap();
            assert_eq!(span, Some((start, start + "count".len())));
        }
        std::fs::write(&path, format!("// external\n\n{text}")).unwrap();
        view.tabs[0].reload(2.0).unwrap();
        view.update_context(2.0);
        view.update_context(2.4);
        assert_eq!(view.ins.ctx.as_ref().unwrap().line, 3);
    }

    #[test]
    fn fresh_tabs_and_rebuilt_index_replace_old_locations_and_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device.c");
        std::fs::write(&path, "int device(void) { return 1; }\n").unwrap();
        let mut view = CodeView::new(CodeSettings::default());
        view.index = Some(Arc::new(cindex::build(tmp.path(), &cindex::shared_progress())));
        view.preview_ctx("device", None);
        assert_eq!(view.ins.ctx.as_ref().unwrap().line, 0);
        std::fs::write(&path, "\n\nint device(void) { return 2; }\n").unwrap();
        view.index = Some(Arc::new(cindex::build(tmp.path(), &cindex::shared_progress())));
        view.insight_reset();
        view.update_context(1.0);
        assert_eq!(view.ins.ctx.as_ref().unwrap().line, 2);
        assert!(view.ins.ctx.as_ref().unwrap().lines[0].contains("return 2"));
        std::fs::write(&path, "\n\n\nint device(void) { return 3; }\n").unwrap();
        view.tabs.push(Doc::load(&path, tmp.path()).unwrap());
        let defs = view.definitions("device");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].1, 3);
        assert!(view.build_ctx("device", (path, 500, Kind::Func), None).is_none());
    }
}

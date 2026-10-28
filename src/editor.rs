//! 代码编辑器本体：自己画、只排版看得见的行。
//! 多光标、列选择、撤销重做、括号补全与匹配、自动缩进、缩进参考线、同名单词高亮、
//! 代码折叠、顶部粘住所在函数、书签、小地图。
//!
//! 坐标约定：位置一律用字节偏移；屏幕上按"格"排列（等宽字体一格，中文等全角字符两格，Tab 对齐到 4 格），
//! 所以光标、选区、参考线的位置都能直接算出来，不依赖字体排版结果。

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use eframe::egui::{self, pos2, vec2, Align2, Color32, Event, FontFamily, FontId, Key, Modifiers, Pos2, Rect, Sense, Shape, Ui, Vec2};

use crate::cindex::Kind;
use crate::highlight::{self, is_ident, pal, Lang, Tok};
use crate::theme::*;

pub const TAB: usize = 4;

// ------------------------------------------------------------ 文本

pub struct Buf {
    pub text: String,
    /// 每行起始字节
    pub starts: Vec<usize>,
    /// 每改一次加一，缓存靠它判断是否过期
    pub ver: u64,
}

impl Buf {
    pub fn new(text: String) -> Buf {
        let mut b = Buf { text, starts: vec![], ver: 0 };
        b.rebuild();
        b
    }

    fn rebuild(&mut self) {
        self.starts.clear();
        self.starts.push(0);
        self.starts.extend(self.text.bytes().enumerate().filter(|(_, c)| *c == b'\n').map(|(i, _)| i + 1));
    }

    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    pub fn line_of(&self, b: usize) -> usize {
        self.starts.partition_point(|&s| s <= b).saturating_sub(1)
    }

    pub fn start(&self, l: usize) -> usize {
        self.starts.get(l).copied().unwrap_or(self.text.len())
    }

    /// 行尾（不含换行符）
    pub fn end(&self, l: usize) -> usize {
        self.starts.get(l + 1).map_or(self.text.len(), |e| e - 1)
    }

    pub fn line(&self, l: usize) -> &str {
        let (s, e) = (self.start(l), self.end(l));
        &self.text[s..e.max(s)]
    }

    pub(crate) fn prev(&self, b: usize) -> usize {
        self.text[..b].chars().next_back().map_or(0, |c| b - c.len_utf8())
    }

    pub(crate) fn next(&self, b: usize) -> usize {
        self.text[b..].chars().next().map_or(b, |c| b + c.len_utf8())
    }

    fn char_before(&self, b: usize) -> Option<char> {
        self.text[..b].chars().next_back()
    }

    pub(crate) fn char_at(&self, b: usize) -> Option<char> {
        self.text.get(b..)?.chars().next()
    }

    /// 字节位置所在的标识符范围
    pub fn word_at(&self, b: usize) -> Option<(usize, usize)> {
        let t = &self.text;
        let mut s = b;
        while let Some(c) = t[..s].chars().next_back().filter(|c| is_ident(*c)) {
            s -= c.len_utf8();
        }
        let mut e = b;
        while let Some(c) = t[e..].chars().next().filter(|c| is_ident(*c)) {
            e += c.len_utf8();
        }
        (e > s && !t[s..].starts_with(|c: char| c.is_ascii_digit())).then_some((s, e))
    }

    /// 标识符或数字（悬停用）
    pub fn token_at(&self, b: usize) -> Option<(usize, usize)> {
        let t = &self.text;
        let mut s = b;
        while let Some(c) = t[..s].chars().next_back().filter(|c| is_ident(*c)) {
            s -= c.len_utf8();
        }
        let mut e = b;
        while let Some(c) = t[e..].chars().next().filter(|c| is_ident(*c)) {
            e += c.len_utf8();
        }
        (e > s).then_some((s, e))
    }

    /// 文件用 Tab 还是空格缩进（看前 2000 行哪种多）
    pub fn indent_unit(&self) -> String {
        let (mut tabs, mut spaces) = (0, 0);
        for l in self.text.lines().take(2000) {
            if l.starts_with('\t') {
                tabs += 1;
            } else if l.starts_with("  ") {
                spaces += 1;
            }
        }
        if tabs > spaces {
            "\t".into()
        } else {
            " ".repeat(TAB)
        }
    }
}

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF |
        0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

fn char_cols(c: char, col: usize) -> usize {
    if c == '\t' {
        TAB - col % TAB
    } else if is_wide(c) {
        2
    } else {
        1
    }
}

/// 行内字节偏移 → 显示列
pub fn vcol(line: &str, b: usize) -> usize {
    let mut col = 0;
    for c in line[..b.min(line.len())].chars() {
        col += char_cols(c, col);
    }
    col
}

/// 屏幕上的字宽：半角字符一格，全角字符用 CJK 字体的实际宽度（不硬凑两格，免得字距稀疏）
#[derive(Clone, Copy)]
pub struct Metrics {
    pub cw: f32,
    pub wide: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics { cw: 1.0, wide: 2.0 }
    }
}

impl Metrics {
    fn adv(&self, c: char, x: f32) -> f32 {
        if c == '\t' {
            let stop = TAB as f32 * self.cw;
            ((x / stop).floor() + 1.0) * stop - x
        } else if is_wide(c) {
            self.wide
        } else {
            self.cw
        }
    }
}

/// 行内字节偏移 → 横向像素位置（相对行首）
pub fn xpos(line: &str, b: usize, m: Metrics) -> f32 {
    let mut x = 0.0;
    for c in line[..b.min(line.len())].chars() {
        x += m.adv(c, x);
    }
    x
}

/// 横向位置 → 行内字节偏移（落在字中间时取较近的一边）
fn byte_at_x(line: &str, want: f32, m: Metrics) -> usize {
    let mut x = 0.0;
    for (i, c) in line.char_indices() {
        let w = m.adv(c, x);
        if want < x + w / 2.0 {
            return i;
        }
        x += w;
    }
    line.len()
}

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

// ------------------------------------------------------------ 选区与撤销

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Sel {
    /// 锚点（拖选开始的一端）
    pub a: usize,
    /// 光标所在的一端
    pub h: usize,
    /// 上下移动时想保持的列
    goal: Option<f32>,
}

impl Sel {
    pub fn at(b: usize) -> Sel {
        Sel { a: b, h: b, goal: None }
    }
    pub fn new(a: usize, h: usize) -> Sel {
        Sel { a, h, goal: None }
    }
    pub fn min(&self) -> usize {
        self.a.min(self.h)
    }
    pub fn max(&self) -> usize {
        self.a.max(self.h)
    }
    pub fn empty(&self) -> bool {
        self.a == self.h
    }
    pub fn range(&self) -> Range<usize> {
        self.min()..self.max()
    }
}

#[derive(Clone)]
struct Edit {
    at: usize,
    del: String,
    ins: String,
}

#[derive(Clone, Copy, PartialEq)]
enum EK {
    Type,
    Delete,
    Other,
}

struct Step {
    edits: Vec<Edit>,
    before: Vec<Sel>,
    after: Vec<Sel>,
    t: f64,
    kind: EK,
}

/// 一处替换：把 `r` 换成 `ins`，替换后的选区相对替换起点给出
struct Rep {
    r: Range<usize>,
    ins: String,
    sels: Vec<(usize, usize)>,
}

impl Rep {
    fn new(r: Range<usize>, ins: impl Into<String>, cur: usize) -> Rep {
        Rep {
            r,
            ins: ins.into(),
            sels: vec![(cur, cur)],
        }
    }
}

// ------------------------------------------------------------ 编辑器

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Char,
    Word,
    Line,
    Column,
}

struct Drag {
    mode: DragMode,
    /// 拖选开始时被操作的那个选区（Word/Line 模式下是整词/整行）
    origin: Sel,
    idx: usize,
    /// 列选择的起点（行, 列）
    col_from: (usize, f32),
}

#[derive(Default)]
struct Cache {
    ver: u64,
    lang: Option<Lang>,
    tokens: Vec<(usize, usize, Tok)>,
    /// 可折叠范围：(起始行, 最后一行被隐藏的行, 是否是大括号块)
    folds: Vec<(usize, usize, bool)>,
}

/// 编辑器一帧的结果，交给外层做悬浮提示、跳转定义等
#[derive(Default)]
pub struct Out {
    pub changed: bool,
    /// 指针下的标识符（字节范围）和它在屏幕上的矩形
    pub hover: Option<(usize, usize, Rect)>,
    /// Ctrl+单击的字节位置
    pub ctrl_click: Option<(usize, Pos2)>,
    pub focused: bool,
    /// Vim 的 : 命令、/ 搜索、n / N，交给外层处理
    pub vim_cmd: Option<String>,
    /// 要放进系统剪贴板的文字（Vim 的 y）
    pub copy: Option<String>,
    /// 右键：外层在这个响应上挂菜单
    pub menu: Option<egui::Response>,
    /// 在只有 /** 的行上按了回车：外层生成函数说明（行号）
    pub doc_comment: Option<usize>,
}

/// 外层传进来的显示参数
pub struct View<'a> {
    pub lang: Lang,
    pub font_size: f32,
    pub minimap: bool,
    /// 查找结果：[起点, 终点) 字节范围（按起点排序），当前项
    pub find: &'a [(usize, usize)],
    pub find_cur: Option<usize>,
    /// 索引里的符号种类，用来给函数、宏、类型、全局变量上色
    pub sem: Option<&'a HashMap<String, Kind>>,
    /// 行的 Git 改动标记：(起始行, 结束行, 'A' 新增 / 'M' 修改 / 'D' 删除)
    pub git: &'a [(usize, usize, char)],
    pub big: bool,
    /// 某一行末尾的淡色附注（行内 blame）
    pub ghost: Option<(usize, &'a str)>,
    /// 用 Vim 键位
    pub vim: bool,
    /// F8 高亮的词和颜色
    pub marks: &'a [(String, Color32)],
    /// 不生效的 #if 分支（行范围，含首尾）
    pub inactive: &'a [(usize, usize)],
    /// 光标所在函数体、#if 块的行范围（画淡色底）
    pub scope: Option<(usize, usize)>,
    pub if_block: Option<(usize, usize)>,
}

pub struct Editor {
    pub sels: Vec<Sel>,
    /// 主光标在 sels 里的下标（最后一次添加的那个）
    pub primary: usize,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// 存盘时 undo 栈的深度；None 表示存盘状态已经被覆盖
    clean: Option<usize>,
    pub folds: BTreeSet<usize>,
    pub bookmarks: BTreeSet<usize>,
    pub scroll: Vec2,
    reveal: Option<f32>,
    drag: Option<Drag>,
    clicks: (f64, Pos2, u8),
    blink: f64,
    cache: Cache,
    disp: (u64, Vec<usize>, Vec<usize>),
    unit: String,
    sb_grab: Option<(bool, f32)>,
    ctrl_down_at: Option<usize>,
    pub id: egui::Id,
    pub focus_req: bool,
    /// 上一帧可见的行数（翻页用）
    pub(crate) page_rows: usize,
    /// Vim 键位的状态（设置里打开时才有）
    pub vim: Option<crate::vim::Vim>,
    /// 小地图拖动中
    mm_drag: bool,
    m: Metrics,
    /// 在 /** 行上回车时交给外层生成说明（C 系语言打开）
    pub doc_hook: bool,
    doc_req: Option<usize>,
    /// 高亮词在整个文件里的位置（滚动条标记用）：(文本版本, 高亮表的指纹, 行和颜色)
    mark_cache: (u64, u64, Vec<(usize, Color32)>),
}

impl Editor {
    pub fn new(id: egui::Id, buf: &Buf) -> Editor {
        Editor {
            sels: vec![Sel::at(0)],
            primary: 0,
            undo: vec![],
            redo: vec![],
            clean: Some(0),
            folds: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            scroll: Vec2::ZERO,
            reveal: None,
            drag: None,
            clicks: (0.0, Pos2::ZERO, 0),
            blink: 0.0,
            cache: Cache::default(),
            disp: (u64::MAX, vec![], vec![]),
            unit: buf.indent_unit(),
            sb_grab: None,
            ctrl_down_at: None,
            id,
            focus_req: false,
            page_rows: 30,
            vim: None,
            mm_drag: false,
            m: Metrics::default(),
            doc_hook: false,
            doc_req: None,
            mark_cache: (u64::MAX, 0, vec![]),
        }
    }

    pub fn dirty(&self) -> bool {
        self.clean != Some(self.undo.len())
    }

    pub fn mark_clean(&mut self) {
        self.clean = Some(self.undo.len());
    }

    pub fn head(&self) -> usize {
        self.sels.get(self.primary).map_or(0, |s| s.h)
    }

    pub fn primary_sel(&self) -> Sel {
        self.sels.get(self.primary).copied().unwrap_or_default()
    }

    /// 只保留一个光标/选区，并滚到它（放在视图上方三分之一处）
    pub fn select(&mut self, a: usize, h: usize) {
        self.sels = vec![Sel::new(a, h)];
        self.primary = 0;
        self.reveal = Some(0.33);
    }

    pub fn reveal_cursor(&mut self) {
        self.reveal = Some(-1.0);
    }

    pub fn selected_text(&self, buf: &Buf) -> String {
        let mut v: Vec<&str> = self.sels.iter().filter(|s| !s.empty()).map(|s| &buf.text[s.range()]).collect();
        if v.is_empty() {
            return String::new();
        }
        if v.len() == 1 {
            return v.remove(0).to_string();
        }
        v.join("\n")
    }

    fn normalize(&mut self) {
        let prim = self.sels.get(self.primary).copied();
        self.sels.sort_by_key(|s| (s.min(), s.max()));
        let mut out: Vec<Sel> = vec![];
        for s in self.sels.drain(..) {
            if let Some(l) = out.last_mut() {
                if s.min() < l.max() || (s.min() == l.max() && (s.empty() || l.empty()) && s.min() == l.min()) || (s.empty() && l.empty() && s.h == l.h) {
                    // 合并重叠的选区，保持方向
                    let (lo, hi) = (l.min(), l.max().max(s.max()));
                    *l = if l.h >= l.a { Sel::new(lo, hi) } else { Sel::new(hi, lo) };
                    continue;
                }
            }
            out.push(s);
        }
        self.sels = out;
        self.primary = prim
            .and_then(|p| self.sels.iter().position(|s| s.min() <= p.h && p.h <= s.max()))
            .unwrap_or(self.sels.len().saturating_sub(1));
        if self.sels.is_empty() {
            self.sels.push(Sel::at(0));
            self.primary = 0;
        }
    }

    // ---------------------------------------------------- 修改与撤销

    fn apply(&mut self, buf: &mut Buf, mut reps: Vec<Rep>, kind: EK, now: f64) {
        reps.sort_by_key(|r| (r.r.start, r.r.end));
        let mut list: Vec<Rep> = vec![];
        for r in reps {
            if list.last().is_some_and(|l| r.r.start < l.r.end || (r.r.start == l.r.start && r.r.is_empty() && l.r.is_empty())) {
                continue;
            }
            list.push(r);
        }
        if list.iter().all(|r| r.r.is_empty() && r.ins.is_empty()) {
            return;
        }
        let before = self.sels.clone();
        let prim_at = self.head();
        let mut delta: isize = 0;
        let mut edits = vec![];
        let mut sels = vec![];
        let mut primary = 0;
        let mut line_before = 0usize;
        let mut last_s = 0usize;
        let mut marks: Vec<(usize, usize, usize)> = vec![];
        for r in list {
            let s = (r.r.start as isize + delta) as usize;
            let e = (r.r.end as isize + delta) as usize;
            line_before += buf.text[last_s..s].matches('\n').count();
            last_s = s;
            let del = buf.text[s..e].to_string();
            marks.push((line_before, del.matches('\n').count(), r.ins.matches('\n').count()));
            buf.text.replace_range(s..e, &r.ins);
            if r.r.start <= prim_at && prim_at <= r.r.end {
                primary = sels.len();
            }
            for (a, h) in &r.sels {
                sels.push(Sel::new(s + a, s + h));
            }
            delta += r.ins.len() as isize - (e - s) as isize;
            edits.push(Edit { at: s, del, ins: r.ins });
        }
        buf.rebuild();
        buf.ver += 1;
        // 书签、折叠跟着行号移动
        for (l, removed, added) in marks.into_iter().rev() {
            shift_lines(&mut self.bookmarks, l, removed, added);
            shift_lines(&mut self.folds, l, removed, added);
        }
        self.sels = sels;
        self.primary = primary.min(self.sels.len().saturating_sub(1));
        self.normalize();
        let after = self.sels.clone();
        self.redo.clear();
        if self.clean.is_some_and(|c| c > self.undo.len()) {
            self.clean = None;
        }
        let merge = kind != EK::Other && self.clean != Some(self.undo.len());
        match self.undo.last_mut() {
            Some(last) if merge && last.kind == kind && now - last.t < 1.0 && same_pos(&last.after, &before) => {
                last.edits.extend(edits);
                last.after = after;
                last.t = now;
            }
            _ => {
                self.undo.push(Step { edits, before, after, t: now, kind });
                if self.undo.len() > 2000 {
                    self.undo.remove(0);
                    self.clean = self.clean.and_then(|c| c.checked_sub(1));
                }
            }
        }
        self.reveal_cursor();
    }

    pub fn undo(&mut self, buf: &mut Buf) -> bool {
        let Some(st) = self.undo.pop() else { return false };
        for e in st.edits.iter().rev() {
            buf.text.replace_range(e.at..e.at + e.ins.len(), &e.del);
        }
        buf.rebuild();
        buf.ver += 1;
        self.sels = st.before.clone();
        self.primary = self.sels.len() - 1;
        self.redo.push(st);
        self.clamp(buf);
        self.reveal_cursor();
        true
    }

    pub fn redo(&mut self, buf: &mut Buf) -> bool {
        let Some(st) = self.redo.pop() else { return false };
        for e in &st.edits {
            buf.text.replace_range(e.at..e.at + e.del.len(), &e.ins);
        }
        buf.rebuild();
        buf.ver += 1;
        self.sels = st.after.clone();
        self.primary = self.sels.len() - 1;
        self.undo.push(st);
        self.clamp(buf);
        self.reveal_cursor();
        true
    }

    /// 外部改了文本（重新加载）后把选区收回到合法范围
    pub fn clamp(&mut self, buf: &Buf) {
        let n = buf.text.len();
        let fix = |b: usize| {
            let mut b = b.min(n);
            while !buf.text.is_char_boundary(b) {
                b -= 1;
            }
            b
        };
        for s in &mut self.sels {
            s.a = fix(s.a);
            s.h = fix(s.h);
        }
        self.normalize();
    }

    /// 一次替换多处（查找替换用）：可以整体撤销；光标停在第一处替换的末尾
    pub fn replace_ranges(&mut self, buf: &mut Buf, items: Vec<(Range<usize>, String)>, now: f64) {
        let reps = items
            .into_iter()
            .enumerate()
            .map(|(i, (r, ins))| {
                let n = ins.len();
                Rep {
                    r,
                    ins,
                    sels: if i == 0 { vec![(n, n)] } else { vec![] },
                }
            })
            .collect();
        self.apply(buf, reps, EK::Other, now);
    }

    /// 把一段换成新文字，光标放在新文字里的 `cursor` 处（None = 末尾）；可以撤销
    pub fn replace_at(&mut self, buf: &mut Buf, r: Range<usize>, ins: String, cursor: Option<usize>, now: f64) {
        let c = cursor.unwrap_or(ins.len()).min(ins.len());
        self.apply(buf, vec![Rep::new(r, ins, c)], EK::Other, now);
        self.reveal = Some(-1.0);
    }

    /// 选区覆盖的行（第一个选区），没有选中时是光标所在行：(首行, 末行)
    pub fn sel_line_span(&self, buf: &Buf) -> (usize, usize) {
        let s = self.primary_sel();
        let a = buf.line_of(s.min());
        let mut b = buf.line_of(s.max());
        if b > a && s.max() == buf.start(b) {
            b -= 1;
        }
        (a, b)
    }

    /// 切换块注释：选区已经被 /* */ 包住就去掉，否则包上（没选中时包当前行的内容）
    pub fn toggle_block_comment(&mut self, buf: &mut Buf, lang: Lang, now: f64) {
        let Some((open, close)) = lang.block_comment() else {
            return self.toggle_comment(buf, lang, now);
        };
        let mut reps = vec![];
        for s in &self.sels {
            let (mut a, mut b) = (s.min(), s.max());
            if s.empty() {
                // 光标在某个 /* */ 里：去掉它
                let l = buf.line_of(a);
                let (ls, le) = (buf.start(l), buf.end(l));
                let line = &buf.text[ls..le];
                let col = a - ls;
                let o = line[..col.min(line.len())].rfind(open).or_else(|| line.find(open));
                if let Some(o) = o {
                    if let Some(c) = line[o + open.len()..].find(close).map(|c| c + o + open.len()) {
                        let inner = line[o + open.len()..c].trim_matches(' ').to_string();
                        let n = inner.len();
                        reps.push(Rep::new(ls + o..ls + c + close.len(), inner, n));
                        continue;
                    }
                }
                let ind = leading_ws(line).len();
                a = ls + ind;
                b = le;
                if a >= b {
                    continue;
                }
            }
            let text = &buf.text[a..b];
            let t = text.trim();
            if t.starts_with(open) && t.ends_with(close) && t.len() >= open.len() + close.len() {
                let inner = t[open.len()..t.len() - close.len()].trim_matches(' ').to_string();
                let lead = text.len() - text.trim_start().len();
                let n = inner.len();
                reps.push(Rep {
                    r: a + lead..a + lead + t.len(),
                    ins: inner,
                    sels: vec![(0, n)],
                });
            } else {
                let ins = format!("{open} {text} {close}");
                let n = ins.len();
                reps.push(Rep { r: a..b, ins, sels: vec![(0, n)] });
            }
        }
        self.apply(buf, reps, EK::Other, now);
    }

    /// 用 #if 0 / #endif 包住选中的行；光标所在块已经是 #if 0 时去掉这两行
    pub fn toggle_if0(&mut self, buf: &mut Buf, now: f64) {
        let (a, b) = self.sel_line_span(buf);
        let p = self.primary_sel();
        let groups = crate::cpp::groups(&buf.text);
        let is_if0 = |l: usize| {
            let head = buf.line(l).trim_start().trim_start_matches('#').trim_start();
            head == "if 0" || head.starts_with("if 0 ") || head.starts_with("if 0/")
        };
        // 选中的正好是一整个 #if 0 … #endif，或者光标在一个 #if 0 块里：去掉
        let target = groups
            .iter()
            .find(|g| g.lines.len() == 2 && g.first() == a && g.last() == b && is_if0(a))
            .map(|g| (g.first(), g.last()))
            .or_else(|| {
                if !(p.empty() || a == b) {
                    return None;
                }
                let l = buf.line_of(p.h);
                let probe = if buf.line(l).trim_start().starts_with('#') { l + 1 } else { l };
                let (ia, ib) = crate::cpp::enclosing(&buf.text, probe.min(buf.lines().saturating_sub(1)))?;
                let simple = groups.iter().find(|g| g.first() == ia).is_some_and(|g| g.lines.len() == 2);
                (simple && is_if0(ia) && ia <= l && l <= ib).then_some((ia, ib))
            });
        if let Some((ia, ib)) = target {
            // #if 0 那一行和 #endif 那一行一起删掉（#endif 在最后一行时连同前面的换行）
            let end_r = if ib + 1 < buf.lines() {
                buf.start(ib)..buf.start(ib + 1)
            } else {
                buf.end(ib - 1)..buf.text.len()
            };
            let head_r = buf.start(ia)..buf.start(ia + 1);
            self.apply(
                buf,
                vec![
                    Rep {
                        r: head_r,
                        ins: String::new(),
                        sels: vec![(0, 0)],
                    },
                    Rep {
                        r: end_r,
                        ins: String::new(),
                        sels: vec![],
                    },
                ],
                EK::Other,
                now,
            );
            return;
        }
        let (s, e) = (buf.start(a), buf.end(b));
        let body = buf.text[s..e].to_string();
        let ins = format!("#if 0\n{body}\n#endif");
        let n = ins.len();
        self.apply(buf, vec![Rep { r: s..e, ins, sels: vec![(0, n)] }], EK::Other, now);
    }

    /// 把 (首行, 末行) 换成新的几行
    pub fn replace_lines(&mut self, buf: &mut Buf, a: usize, b: usize, lines: &[String], now: f64) {
        let (s, e) = (buf.start(a), buf.end(b));
        let ins = lines.join("\n");
        let n = ins.len();
        self.apply(buf, vec![Rep { r: s..e, ins, sels: vec![(0, n)] }], EK::Other, now);
    }

    /// 删掉含某个标记的所有行，返回删了几行
    pub fn delete_marked_lines(&mut self, buf: &mut Buf, mark: &str, now: f64) -> usize {
        let n = buf.lines();
        let mut reps = vec![];
        for l in 0..n {
            if buf.line(l).contains(mark) {
                let s = buf.start(l);
                let e = if l + 1 < n { buf.start(l + 1) } else { buf.text.len() };
                let (s, e) = if l + 1 >= n && l > 0 { (buf.end(l - 1), e) } else { (s, e) };
                reps.push(Rep {
                    r: s..e,
                    ins: String::new(),
                    sels: vec![],
                });
            }
        }
        let k = reps.len();
        if k > 0 {
            let keep = self.sels.clone();
            self.apply(buf, reps, EK::Other, now);
            if self.sels.is_empty() {
                self.sels = keep;
            }
            self.clamp(buf);
        }
        k
    }

    /// 对当前的每个光标/选区执行文字替换（命名风格转换用）：选区为空时作用于光标处的词
    pub fn map_words(&mut self, buf: &mut Buf, f: impl Fn(&str) -> String, now: f64) {
        let mut reps = vec![];
        for s in &self.sels {
            let r = if s.empty() {
                match buf.word_at(s.h) {
                    Some((a, b)) => a..b,
                    None => continue,
                }
            } else {
                s.range()
            };
            let ins = f(&buf.text[r.clone()]);
            let n = ins.len();
            reps.push(Rep { r, ins, sels: vec![(0, n)] });
        }
        self.apply(buf, reps, EK::Other, now);
    }

    // ---------------------------------------------------- 输入

    pub(crate) fn insert_text(&mut self, buf: &mut Buf, s: &str, lang: Lang, now: f64) {
        if s == "\n" || s == "\r\n" {
            return self.newline(buf, now);
        }
        let ch = {
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Some(c),
                _ => None,
            }
        };
        let toks = &self.cache.tokens;
        let in_str_or_comment = |b: usize| {
            let i = toks.partition_point(|t| t.1 <= b);
            toks.get(i).is_some_and(|t| t.0 < b && matches!(t.2, Tok::Str | Tok::Comment))
        };
        let mut reps = vec![];
        for sel in &self.sels {
            let r = sel.range();
            let next = buf.char_at(r.end);
            let prev = buf.char_before(r.start);
            let rep = match ch {
                Some(c @ (')' | ']' | '}' | '"' | '\'')) if sel.empty() && next == Some(c) => Rep::new(r.end..r.end + 1, c.to_string(), 1),
                Some(c) if lang != Lang::Plain && !sel.empty() && pair_of(c).is_some() && !buf.text[r.clone()].contains('\n') => {
                    let close = pair_of(c).unwrap();
                    let inner = buf.text[r.clone()].to_string();
                    let n = inner.len();
                    Rep {
                        r: r.clone(),
                        ins: format!("{c}{inner}{close}"),
                        sels: vec![(1, 1 + n)],
                    }
                }
                Some(c) if lang != Lang::Plain && sel.empty() && pair_of(c).is_some() => {
                    let close = pair_of(c).unwrap();
                    let free_after = next.is_none_or(|n| n.is_whitespace() || ")]};,".contains(n));
                    let quote = c == '"' || c == '\'';
                    let ok = free_after && !in_str_or_comment(r.start) && !(quote && prev.is_some_and(|p| is_ident(p) || p == c || p == '\\'));
                    if ok {
                        Rep::new(r.clone(), format!("{c}{close}"), 1)
                    } else {
                        Rep::new(r.clone(), c.to_string(), 1)
                    }
                }
                Some('}') if sel.empty() => {
                    // 在只有空白的行上敲 } 时自动往回缩一级
                    let l = buf.line_of(r.start);
                    let ls = buf.start(l);
                    let before = &buf.text[ls..r.start];
                    if !before.is_empty() && before.chars().all(|c| c == ' ' || c == '\t') {
                        let ind = outdent_str(before, &self.unit);
                        Rep::new(ls..r.start, format!("{ind}}}"), ind.len() + 1)
                    } else {
                        Rep::new(r.clone(), "}", 1)
                    }
                }
                _ => Rep::new(r.clone(), s, s.len()),
            };
            reps.push(rep);
        }
        self.apply(buf, reps, EK::Type, now);
    }

    pub(crate) fn newline(&mut self, buf: &mut Buf, now: f64) {
        if self.doc_hook && self.sels.len() == 1 && self.sels[0].empty() {
            let h = self.sels[0].h;
            let l = buf.line_of(h);
            let (ls, le) = (buf.start(l), buf.end(l));
            if buf.text[ls..h].trim() == "/**" && buf.text[h..le].trim().is_empty() {
                self.doc_req = Some(l);
                return;
            }
        }
        let mut reps = vec![];
        for sel in &self.sels {
            let r = sel.range();
            let l = buf.line_of(r.start);
            let line_start = buf.start(l);
            let head = &buf.text[line_start..r.start];
            let indent = leading_ws(head).to_string();
            let last = head.trim_end().chars().last();
            let next = buf.text[r.end..].chars().find(|c| *c != ' ' && *c != '\t');
            let opener = matches!(last, Some('{' | '(' | '[')) || (last == Some(':') && self.unit.len() > 1 && head.trim_start().starts_with(|c: char| c.is_alphabetic()) && is_py_block(head));
            // 删掉光标后面紧跟的空白，避免新行开头留空格
            let mut end = r.end;
            while buf.char_at(end).is_some_and(|c| c == ' ' || c == '\t') {
                end += 1;
            }
            if opener && last.and_then(pair_of) == next && next.is_some() {
                let ins = format!("\n{indent}{}\n{indent}", self.unit);
                let cur = 1 + indent.len() + self.unit.len();
                reps.push(Rep::new(r.start..end, ins, cur));
            } else if opener {
                let ins = format!("\n{indent}{}", self.unit);
                let n = ins.len();
                reps.push(Rep::new(r.start..end, ins, n));
            } else {
                let ins = format!("\n{indent}");
                let n = ins.len();
                reps.push(Rep::new(r.start..end, ins, n));
            }
        }
        self.apply(buf, reps, EK::Other, now);
    }

    pub(crate) fn backspace(&mut self, buf: &mut Buf, word: bool, now: f64) {
        let mut reps = vec![];
        for sel in &self.sels {
            if !sel.empty() {
                reps.push(Rep::new(sel.range(), "", 0));
                continue;
            }
            let b = sel.h;
            if b == 0 {
                continue;
            }
            let start = if word {
                word_left(buf, b)
            } else {
                let ls = buf.start(buf.line_of(b));
                let before = &buf.text[ls..b];
                let prev = buf.char_before(b);
                let next = buf.char_at(b);
                if prev.and_then(pair_of).is_some() && prev.and_then(pair_of) == next {
                    reps.push(Rep::new(buf.prev(b)..b + 1, "", 0));
                    continue;
                }
                if !before.is_empty() && before.bytes().all(|c| c == b' ') && self.unit.starts_with(' ') {
                    // 在缩进里退格：退到上一个制表位
                    let col = before.len();
                    let to = (col - 1) / TAB * TAB;
                    ls + to
                } else {
                    buf.prev(b)
                }
            };
            reps.push(Rep::new(start..b, "", 0));
        }
        self.apply(buf, reps, EK::Delete, now);
    }

    fn delete(&mut self, buf: &mut Buf, word: bool, now: f64) {
        let mut reps = vec![];
        for sel in &self.sels {
            if !sel.empty() {
                reps.push(Rep::new(sel.range(), "", 0));
                continue;
            }
            let b = sel.h;
            let end = if word { word_right(buf, b) } else { buf.next(b) };
            if end > b {
                reps.push(Rep::new(b..end, "", 0));
            }
        }
        self.apply(buf, reps, EK::Delete, now);
    }

    /// 选区覆盖的行（每个选区一段，已合并相邻的）
    fn sel_lines(&self, buf: &Buf) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = vec![];
        for s in &self.sels {
            let a = buf.line_of(s.min());
            let mut b = buf.line_of(s.max());
            // 选到下一行行首时不算那一行
            if b > a && s.max() == buf.start(b) {
                b -= 1;
            }
            match v.last_mut() {
                Some(l) if a <= l.1 + 1 => l.1 = l.1.max(b),
                _ => v.push((a, b)),
            }
        }
        v
    }

    fn tab(&mut self, buf: &mut Buf, now: f64) {
        if self.sels.iter().any(|s| buf.line_of(s.min()) != buf.line_of(s.max())) {
            return self.indent_lines(buf, true, now);
        }
        let mut reps = vec![];
        for sel in &self.sels {
            let ins = if self.unit == "\t" {
                "\t".to_string()
            } else {
                let l = buf.line_of(sel.min());
                let col = vcol(buf.line(l), sel.min() - buf.start(l));
                " ".repeat(TAB - col % TAB)
            };
            let n = ins.len();
            reps.push(Rep::new(sel.range(), ins, n));
        }
        self.apply(buf, reps, EK::Type, now);
    }

    pub(crate) fn indent_lines(&mut self, buf: &mut Buf, inc: bool, now: f64) {
        let blocks = self.sel_lines(buf);
        let old = self.sels.clone();
        let mut reps = vec![];
        // (行首位置, 改动量) 用来把选区挪到改动后的位置
        let mut shifts: Vec<(usize, isize)> = vec![];
        for (a, b) in blocks {
            for l in a..=b {
                let line = buf.line(l);
                let ls = buf.start(l);
                if inc {
                    if line.trim().is_empty() {
                        continue;
                    }
                    reps.push(Rep {
                        r: ls..ls,
                        ins: self.unit.clone(),
                        sels: vec![],
                    });
                    shifts.push((ls, self.unit.len() as isize));
                } else {
                    let ws = leading_ws(line);
                    let cut = if ws.starts_with('\t') { 1 } else { ws.bytes().take(TAB).take_while(|c| *c == b' ').count() };
                    if cut > 0 {
                        reps.push(Rep {
                            r: ls..ls + cut,
                            ins: String::new(),
                            sels: vec![],
                        });
                        shifts.push((ls, -(cut as isize)));
                    }
                }
            }
        }
        if reps.is_empty() {
            return;
        }
        let map = |b: usize| -> usize {
            let mut d: isize = 0;
            for &(ls, sh) in &shifts {
                if ls < b || (ls == b && sh > 0) {
                    if sh < 0 && b < ls + (-sh) as usize {
                        d -= (b - ls) as isize;
                    } else {
                        d += sh;
                    }
                }
            }
            (b as isize + d).max(0) as usize
        };
        let new_sels: Vec<Sel> = old.iter().map(|s| Sel::new(map(s.a), map(s.h))).collect();
        self.apply(buf, reps, EK::Other, now);
        self.sels = new_sels;
        self.clamp(buf);
        if let Some(st) = self.undo.last_mut() {
            st.after = self.sels.clone();
        }
    }

    pub fn toggle_comment(&mut self, buf: &mut Buf, lang: Lang, now: f64) {
        let Some(pre) = lang.line_comment() else { return };
        let blocks = self.sel_lines(buf);
        let old = self.sels.clone();
        let lines: Vec<usize> = blocks.iter().flat_map(|(a, b)| *a..=*b).filter(|l| !buf.line(*l).trim().is_empty()).collect();
        if lines.is_empty() {
            return;
        }
        let all = lines.iter().all(|l| buf.line(*l).trim_start().starts_with(pre));
        let min_ind = lines.iter().map(|l| leading_ws(buf.line(*l)).len()).min().unwrap_or(0);
        let mut reps = vec![];
        let mut shifts: Vec<(usize, isize)> = vec![];
        for l in lines {
            let ls = buf.start(l);
            let line = buf.line(l);
            if all {
                let i = leading_ws(line).len();
                let mut n = pre.len();
                if line[i + n..].starts_with(' ') {
                    n += 1;
                }
                reps.push(Rep {
                    r: ls + i..ls + i + n,
                    ins: String::new(),
                    sels: vec![],
                });
                shifts.push((ls + i, -(n as isize)));
            } else {
                let ins = format!("{pre} ");
                shifts.push((ls + min_ind, ins.len() as isize));
                reps.push(Rep {
                    r: ls + min_ind..ls + min_ind,
                    ins,
                    sels: vec![],
                });
            }
        }
        let map = |b: usize| -> usize {
            let mut d: isize = 0;
            for &(p, sh) in &shifts {
                if p < b {
                    if sh < 0 && b < p + (-sh) as usize {
                        d -= (b - p) as isize;
                    } else {
                        d += sh;
                    }
                }
            }
            (b as isize + d).max(0) as usize
        };
        let new_sels: Vec<Sel> = old.iter().map(|s| Sel::new(map(s.a), map(s.h))).collect();
        self.apply(buf, reps, EK::Other, now);
        self.sels = new_sels;
        self.clamp(buf);
        if let Some(st) = self.undo.last_mut() {
            st.after = self.sels.clone();
        }
    }

    /// 上下移动（copy=false）或复制（copy=true）选中的行
    pub fn move_lines(&mut self, buf: &mut Buf, up: bool, copy: bool, now: f64) {
        let blocks = self.sel_lines(buf);
        let n = buf.lines();
        if !copy && ((up && blocks[0].0 == 0) || (!up && blocks.last().unwrap().1 + 1 >= n)) {
            return;
        }
        let old = self.sels.clone();
        let mut reps = vec![];
        let mut moves: Vec<(usize, usize, isize)> = vec![];
        for (a, b) in blocks {
            let (s, e) = (buf.start(a), buf.end(b));
            let block = buf.text[s..e].to_string();
            if copy {
                if up {
                    reps.push(Rep {
                        r: s..s,
                        ins: format!("{block}\n"),
                        sels: vec![],
                    });
                    moves.push((s, e + 1, 0));
                } else {
                    reps.push(Rep {
                        r: e..e,
                        ins: format!("\n{block}"),
                        sels: vec![],
                    });
                    moves.push((s, e + 1, (block.len() + 1) as isize));
                }
            } else if up {
                let ps = buf.start(a - 1);
                let prev = buf.text[ps..s - 1].to_string();
                reps.push(Rep {
                    r: ps..e,
                    ins: format!("{block}\n{prev}"),
                    sels: vec![],
                });
                moves.push((s, e + 1, -((prev.len() + 1) as isize)));
            } else {
                let ne = buf.end(b + 1);
                let next = buf.text[e + 1..ne].to_string();
                reps.push(Rep {
                    r: s..ne,
                    ins: format!("{next}\n{block}"),
                    sels: vec![],
                });
                moves.push((s, e + 1, (next.len() + 1) as isize));
            }
        }
        let map = |x: usize| -> usize {
            let mut d = 0isize;
            for &(s, e, sh) in &moves {
                if x >= s && x < e.max(s + 1) {
                    return (x as isize + sh + d) as usize;
                }
                if x >= e {
                    d += if copy { (e - s) as isize } else { 0 };
                }
            }
            (x as isize + d) as usize
        };
        let new_sels: Vec<Sel> = old.iter().map(|s| Sel::new(map(s.a), map(s.h))).collect();
        self.apply(buf, reps, EK::Other, now);
        self.sels = new_sels;
        self.clamp(buf);
        if let Some(st) = self.undo.last_mut() {
            st.after = self.sels.clone();
        }
    }

    pub fn delete_lines(&mut self, buf: &mut Buf, now: f64) {
        let blocks = self.sel_lines(buf);
        let n = buf.lines();
        let mut reps = vec![];
        for (a, b) in blocks {
            let (s, e) = if b + 1 < n {
                (buf.start(a), buf.start(b + 1))
            } else if a > 0 {
                (buf.end(a - 1), buf.text.len())
            } else {
                (0, buf.text.len())
            };
            reps.push(Rep::new(s..e, "", 0));
        }
        self.apply(buf, reps, EK::Other, now);
        // 光标放到新行的同一列附近
        for s in &mut self.sels {
            let l = buf.line_of(s.h);
            let ind = leading_ws(buf.line(l)).len();
            *s = Sel::at(buf.start(l) + ind);
        }
    }

    pub fn insert_line(&mut self, buf: &mut Buf, above: bool, now: f64) {
        let mut reps = vec![];
        for s in &self.sels {
            let l = buf.line_of(s.h);
            let ind = leading_ws(buf.line(l)).to_string();
            if above {
                let ls = buf.start(l);
                let n = ind.len();
                reps.push(Rep::new(ls..ls, format!("{ind}\n"), n));
            } else {
                let le = buf.end(l);
                let ins = format!("\n{ind}");
                let n = ins.len();
                reps.push(Rep::new(le..le, ins, n));
            }
        }
        self.apply(buf, reps, EK::Other, now);
    }

    fn paste(&mut self, buf: &mut Buf, text: &str, now: f64) {
        let text = text.replace("\r\n", "\n");
        let parts: Vec<&str> = text.split('\n').collect();
        let spread = self.sels.len() > 1 && parts.len() == self.sels.len();
        let reps = self
            .sels
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let ins = if spread { parts[i].to_string() } else { text.clone() };
                let n = ins.len();
                Rep::new(s.range(), ins, n)
            })
            .collect();
        self.apply(buf, reps, EK::Other, now);
    }

    /// 复制：没有选中内容时复制整行（和 VS Code 一样）
    pub fn copy_text(&self, buf: &Buf) -> String {
        if self.sels.iter().all(|s| s.empty()) {
            let mut out = String::new();
            for (a, b) in self.sel_lines(buf) {
                out.push_str(&buf.text[buf.start(a)..buf.end(b)]);
                out.push('\n');
            }
            return out;
        }
        self.selected_text(buf)
    }

    fn cut(&mut self, buf: &mut Buf, now: f64) -> String {
        let t = self.copy_text(buf);
        if self.sels.iter().all(|s| s.empty()) {
            self.delete_lines(buf, now);
        } else {
            let reps = self.sels.iter().map(|s| Rep::new(s.range(), "", 0)).collect();
            self.apply(buf, reps, EK::Other, now);
        }
        t
    }

    // ---------------------------------------------------- 多光标

    /// Ctrl+D：先选中光标处的词，再按就加选下一个相同的
    pub fn add_next_occurrence(&mut self, buf: &Buf) {
        let p = self.primary_sel();
        if p.empty() {
            if let Some((s, e)) = buf.word_at(p.h) {
                self.sels[self.primary] = Sel::new(s, e);
            }
            return;
        }
        let needle = &buf.text[p.range()];
        let from = self.sels.iter().map(|s| s.max()).max().unwrap_or(0);
        let hit = buf.text[from..].find(needle).map(|i| from + i).or_else(|| buf.text.find(needle));
        if let Some(i) = hit {
            if !self.sels.iter().any(|s| s.min() == i) {
                self.sels.push(Sel::new(i, i + needle.len()));
                self.primary = self.sels.len() - 1;
                self.normalize();
                self.reveal_cursor();
            }
        }
    }

    pub fn select_all_occurrences(&mut self, buf: &Buf) {
        let mut p = self.primary_sel();
        if p.empty() {
            match buf.word_at(p.h) {
                Some((s, e)) => p = Sel::new(s, e),
                None => return,
            }
        }
        let needle = buf.text[p.range()].to_string();
        let whole = buf.word_at(p.min()) == Some((p.min(), p.max()));
        let mut v = vec![];
        let mut from = 0;
        while let Some(i) = buf.text[from..].find(&needle) {
            let s = from + i;
            from = s + needle.len().max(1);
            if whole && buf.word_at(s) != Some((s, s + needle.len())) {
                continue;
            }
            v.push(Sel::new(s, s + needle.len()));
            if v.len() >= 5000 {
                break;
            }
        }
        if !v.is_empty() {
            self.sels = v;
            self.primary = self.sels.iter().position(|s| s.min() == p.min()).unwrap_or(0);
        }
    }

    /// Ctrl+Alt+↑/↓：在上一行/下一行同一列加光标
    pub fn add_cursor_vert(&mut self, buf: &Buf, up: bool) {
        let p = self.primary_sel();
        let l = buf.line_of(p.h);
        let col = p.goal.unwrap_or_else(|| xpos(buf.line(l), p.h - buf.start(l), self.m));
        let Some(nl) = (if up { l.checked_sub(1) } else { (l + 1 < buf.lines()).then_some(l + 1) }) else {
            return;
        };
        let b = buf.start(nl) + byte_at_x(buf.line(nl), col, self.m);
        let mut s = Sel::at(b);
        s.goal = Some(col);
        self.sels.push(s);
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.reveal_cursor();
    }

    pub fn select_line(&mut self, buf: &Buf) {
        for s in &mut self.sels {
            let a = buf.line_of(s.min());
            let b = buf.line_of(s.max());
            let end = if b + 1 < buf.lines() { buf.start(b + 1) } else { buf.text.len() };
            // 已经整行选中时再按就多选一行
            let end = if s.min() == buf.start(a) && s.max() == end && b + 2 <= buf.lines() {
                buf.start((b + 2).min(buf.lines() - 1)).max(end)
            } else {
                end
            };
            *s = Sel::new(buf.start(a), end);
        }
        self.normalize();
        self.reveal_cursor();
    }

    pub fn goto_bracket(&mut self, buf: &Buf) {
        if let Some((a, b)) = self.bracket_pair(buf) {
            let h = self.head();
            let to = if h == a || h == a + 1 { b } else { a };
            self.sels = vec![Sel::at(to)];
            self.primary = 0;
            self.reveal_cursor();
        }
    }

    // ---------------------------------------------------- 光标移动

    fn move_sels(&mut self, buf: &Buf, extend: bool, f: impl Fn(&Self, &Buf, Sel) -> (usize, Option<f32>)) {
        let sels = self.sels.clone();
        self.sels = sels
            .into_iter()
            .map(|s| {
                let (b, goal) = f(self, buf, s);
                if extend {
                    Sel { a: s.a, h: b, goal }
                } else {
                    Sel { a: b, h: b, goal }
                }
            })
            .collect();
        self.normalize();
        self.reveal_cursor();
    }

    /// 显示行（考虑折叠）之间上下移动
    fn vert(&self, buf: &Buf, s: Sel, rows: isize) -> (usize, Option<f32>) {
        let l = buf.line_of(s.h);
        let col = s.goal.unwrap_or_else(|| xpos(buf.line(l), s.h - buf.start(l), self.m));
        let disp = &self.disp.1;
        let di = disp.partition_point(|&x| x < l) as isize;
        let ti = di + rows;
        if ti < 0 {
            return (0, None);
        }
        if ti as usize >= disp.len() {
            return (buf.text.len(), None);
        }
        let nl = disp[ti as usize];
        (buf.start(nl) + byte_at_x(buf.line(nl), col, self.m), Some(col))
    }

    fn key(&mut self, ui: &Ui, buf: &mut Buf, key: Key, m: Modifiers, lang: Lang, now: f64, out: &mut Out) -> bool {
        let cmd = m.command;
        let shift = m.shift;
        let word = cmd || m.alt;
        match key {
            Key::ArrowLeft => {
                let collapse = !shift && self.sels.iter().any(|s| !s.empty()) && !word;
                if collapse {
                    for s in &mut self.sels {
                        *s = Sel::at(s.min());
                    }
                    self.normalize();
                } else {
                    self.move_sels(buf, shift, |_, b, s| (if word { word_left(b, s.h) } else { b.prev(s.h) }, None));
                }
            }
            Key::ArrowRight => {
                let collapse = !shift && self.sels.iter().any(|s| !s.empty()) && !word;
                if collapse {
                    for s in &mut self.sels {
                        *s = Sel::at(s.max());
                    }
                    self.normalize();
                } else {
                    self.move_sels(buf, shift, |_, b, s| (if word { word_right(b, s.h) } else { b.next(s.h) }, None));
                }
            }
            Key::ArrowUp if !m.alt => {
                if cmd {
                    self.scroll.y -= 20.0;
                } else {
                    self.move_sels(buf, shift, |e, b, s| e.vert(b, s, -1));
                }
            }
            Key::ArrowDown if !m.alt => {
                if cmd {
                    self.scroll.y += 20.0;
                } else {
                    self.move_sels(buf, shift, |e, b, s| e.vert(b, s, 1));
                }
            }
            Key::PageUp => {
                let n = self.page_rows as isize;
                self.move_sels(buf, shift, |e, b, s| e.vert(b, s, -n));
            }
            Key::PageDown => {
                let n = self.page_rows as isize;
                self.move_sels(buf, shift, |e, b, s| e.vert(b, s, n));
            }
            Key::Home => {
                if cmd {
                    self.move_sels(buf, shift, |_, _, _| (0, None));
                } else {
                    self.move_sels(buf, shift, |_, b, s| {
                        let l = b.line_of(s.h);
                        let ls = b.start(l);
                        let ind = ls + leading_ws(b.line(l)).len();
                        (if s.h == ind { ls } else { ind }, None)
                    });
                }
            }
            Key::End => {
                if cmd {
                    self.move_sels(buf, shift, |_, b, _| (b.text.len(), None));
                } else {
                    self.move_sels(buf, shift, |_, b, s| (b.end(b.line_of(s.h)), None));
                }
            }
            Key::Backspace => self.backspace(buf, word, now),
            Key::Delete => self.delete(buf, word, now),
            Key::Enter if !cmd => self.newline(buf, now),
            Key::Tab => {
                if shift {
                    self.indent_lines(buf, false, now)
                } else {
                    self.tab(buf, now)
                }
            }
            Key::Escape => {
                if self.sels.len() > 1 {
                    let p = self.primary_sel();
                    self.sels = vec![p];
                    self.primary = 0;
                } else if !self.sels[0].empty() {
                    self.sels[0] = Sel::at(self.sels[0].h);
                } else {
                    return false;
                }
            }
            Key::A if cmd => {
                self.sels = vec![Sel::new(0, buf.text.len())];
                self.primary = 0;
            }
            Key::Z if cmd && !shift => {
                out.changed |= self.undo(buf);
            }
            Key::Z if cmd && shift => {
                out.changed |= self.redo(buf);
            }
            Key::Y if cmd => {
                out.changed |= self.redo(buf);
            }
            _ => return false,
        }
        let _ = (ui, lang);
        true
    }

    // ---------------------------------------------------- 折叠

    fn fold_at(&self, l: usize) -> Option<(usize, usize, bool)> {
        let i = self.cache.folds.partition_point(|f| f.0 < l);
        self.cache.folds.get(i).filter(|f| f.0 == l).copied()
    }

    /// 包含这一行的最内层可折叠范围
    fn enclosing(&self, l: usize) -> Option<(usize, usize, bool)> {
        self.cache.folds.iter().filter(|f| f.0 < l && l <= f.1 + 1 && f.2).max_by_key(|f| f.0).copied()
    }

    pub fn fold_cursor(&mut self, buf: &Buf, fold: bool) {
        let l = buf.line_of(self.head());
        if fold {
            let r = self
                .fold_at(l)
                .filter(|f| !self.folds.contains(&f.0))
                .or_else(|| self.cache.folds.iter().filter(|f| f.0 <= l && l <= f.1 + 1 && !self.folds.contains(&f.0)).max_by_key(|f| f.0).copied());
            if let Some(r) = r {
                self.folds.insert(r.0);
                // 光标在被藏起来的行里就挪到折叠起始行
                if l > r.0 && l <= r.1 {
                    let e = buf.end(r.0);
                    self.sels = vec![Sel::at(e)];
                    self.primary = 0;
                }
            }
        } else {
            let hit = self.folds.iter().copied().filter(|&s| self.fold_at(s).is_some_and(|f| f.0 <= l && l <= f.1 + 1)).max();
            if let Some(s) = hit {
                self.folds.remove(&s);
            }
        }
    }

    pub fn fold_all(&mut self, fold: bool) {
        if fold {
            // 只折叠最外层，打开时再一层层展开
            let mut last_end = 0;
            let mut any = false;
            for f in &self.cache.folds {
                if !any || f.0 > last_end {
                    self.folds.insert(f.0);
                    last_end = f.1;
                    any = true;
                }
            }
        } else {
            self.folds.clear();
        }
    }

    pub fn toggle_bookmark(&mut self, buf: &Buf) {
        let l = buf.line_of(self.head());
        if !self.bookmarks.remove(&l) {
            self.bookmarks.insert(l);
        }
    }

    pub fn next_bookmark(&mut self, buf: &Buf, back: bool) -> bool {
        let l = buf.line_of(self.head());
        let to = if back {
            self.bookmarks.range(..l).next_back().or_else(|| self.bookmarks.iter().next_back())
        } else {
            self.bookmarks.range(l + 1..).next().or_else(|| self.bookmarks.iter().next())
        };
        if let Some(&t) = to {
            let b = buf.start(t);
            self.select(b, b);
            return true;
        }
        false
    }

    /// 光标旁边的括号和它的配对（跳过字符串和注释里的）
    fn bracket_pair(&self, buf: &Buf) -> Option<(usize, usize)> {
        let h = self.head();
        let toks = &self.cache.tokens;
        let code = |b: usize| {
            let i = toks.partition_point(|t| t.1 <= b);
            !toks.get(i).is_some_and(|t| t.0 <= b && matches!(t.2, Tok::Str | Tok::Comment))
        };
        let cands = [(buf.char_at(h), h), (buf.char_before(h), buf.prev(h))];
        for (c, at) in cands {
            let Some(c) = c else { continue };
            if !code(at) {
                continue;
            }
            let (open, close, fwd) = match c {
                '(' => ('(', ')', true),
                '[' => ('[', ']', true),
                '{' => ('{', '}', true),
                ')' => ('(', ')', false),
                ']' => ('[', ']', false),
                '}' => ('{', '}', false),
                _ => continue,
            };
            let t = buf.text.as_bytes();
            let mut depth = 0i32;
            if fwd {
                let lim = (at + 400_000).min(t.len());
                for i in at..lim {
                    let ch = t[i] as char;
                    if ch == open && code(i) {
                        depth += 1;
                    } else if ch == close && code(i) {
                        depth -= 1;
                        if depth == 0 {
                            return Some((at, i));
                        }
                    }
                }
            } else {
                let lim = at.saturating_sub(400_000);
                for i in (lim..=at).rev() {
                    let ch = t[i] as char;
                    if ch == close && code(i) {
                        depth += 1;
                    } else if ch == open && code(i) {
                        depth -= 1;
                        if depth == 0 {
                            return Some((i, at));
                        }
                    }
                }
            }
        }
        None
    }

    fn refresh_cache(&mut self, buf: &Buf, lang: Lang, big: bool) {
        if self.cache.ver == buf.ver && self.cache.lang == Some(lang) && !self.cache.tokens.is_empty() == (!big && !buf.text.is_empty()) {
            return;
        }
        self.cache.ver = buf.ver;
        self.cache.lang = Some(lang);
        self.cache.tokens = if big { vec![] } else { highlight::tokenize(&buf.text, lang) };
        // 可折叠范围：跨行的 {}、#if…#endif、多行块注释
        let mut folds = vec![];
        let mut stack: Vec<usize> = vec![];
        let mut pre: Vec<usize> = vec![];
        let t = buf.text.as_bytes();
        for &(s, e, k) in &self.cache.tokens {
            match k {
                Tok::Punct => {
                    for i in s..e {
                        match t[i] {
                            b'{' => stack.push(i),
                            b'}' => {
                                if let Some(o) = stack.pop() {
                                    let (a, b) = (buf.line_of(o), buf.line_of(i));
                                    if b >= a + 2 {
                                        folds.push((a, b - 1, true));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Tok::Comment => {
                    let (a, b) = (buf.line_of(s), buf.line_of(e.saturating_sub(1)));
                    if b > a {
                        folds.push((a, b, false));
                    }
                }
                Tok::Pre if lang == Lang::C => {
                    let w = &buf.text[s..e];
                    let w = w.trim_start_matches('#').trim_start();
                    if w.starts_with("if") {
                        pre.push(buf.line_of(s));
                    } else if w.starts_with("endif") {
                        if let Some(a) = pre.pop() {
                            let b = buf.line_of(s);
                            if b >= a + 2 {
                                folds.push((a, b - 1, false));
                            }
                        }
                    } else if w.starts_with("el") {
                        if let Some(a) = pre.pop() {
                            let b = buf.line_of(s);
                            if b >= a + 2 {
                                folds.push((a, b - 1, false));
                            }
                            pre.push(b);
                        }
                    }
                }
                _ => {}
            }
        }
        if lang == Lang::Python {
            folds.extend(indent_folds(buf));
        }
        folds.sort_by_key(|f| (f.0, std::cmp::Reverse(f.1)));
        folds.dedup_by_key(|f| f.0);
        self.cache.folds = folds;
        let valid: BTreeSet<usize> = self.cache.folds.iter().map(|f| f.0).collect();
        self.folds.retain(|l| valid.contains(l));
    }

    fn refresh_disp(&mut self, buf: &Buf) {
        let key = buf
            .ver
            .wrapping_mul(31)
            .wrapping_add(self.folds.iter().fold(7u64, |h, l| h.wrapping_mul(1_000_003).wrapping_add(*l as u64)));
        if self.disp.0 == key && !self.disp.1.is_empty() {
            return;
        }
        let n = buf.lines();
        let mut hidden: Vec<(usize, usize)> = self.folds.iter().filter_map(|&s| self.fold_at(s)).map(|f| (f.0 + 1, f.1)).collect();
        hidden.sort();
        let mut v = Vec::with_capacity(n);
        let mut hi = 0;
        let mut l = 0;
        while l < n {
            while hi < hidden.len() && hidden[hi].1 < l {
                hi += 1;
            }
            if hi < hidden.len() && hidden[hi].0 <= l && l <= hidden[hi].1 {
                l = hidden[hi].1 + 1;
                continue;
            }
            v.push(l);
            l += 1;
        }
        self.disp = (key, v, vec![]);
    }

    /// 让被折叠藏起来的行露出来
    fn unfold_line(&mut self, l: usize) {
        let hide: Vec<usize> = self.folds.iter().copied().filter(|&s| self.fold_at(s).is_some_and(|f| f.0 < l && l <= f.1)).collect();
        for s in hide {
            self.folds.remove(&s);
        }
    }

    // ---------------------------------------------------- 绘制与交互

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, buf: &mut Buf, v: &View) -> Out {
        let mut out = Out::default();
        let now = ui.input(|i| i.time);
        if v.vim != self.vim.is_some() {
            self.vim = v.vim.then(crate::vim::Vim::default);
        }
        let (psel, nsels, dragging) = (self.primary_sel(), self.sels.len(), self.drag.is_some());
        if let Some(vm) = self.vim.as_mut() {
            let was_drag = std::mem::replace(&mut vm.was_drag, dragging);
            if vm.mode == crate::vim::Mode::Normal && !dragging {
                let s = psel;
                if !s.empty() && was_drag {
                    // 鼠标拖选了一段：进入可视模式
                    vm.enter_visual(nsels, s.a);
                } else if !s.empty() {
                    // 查找、跳转留下的选区：普通模式下只留光标在开头
                    self.sels = vec![Sel::at(s.min())];
                    self.primary = 0;
                } else {
                    let l = buf.line_of(s.h);
                    if s.h >= buf.end(l) && buf.end(l) > buf.start(l) && nsels == 1 {
                        let b = buf.prev(buf.end(l));
                        self.sels = vec![Sel::at(b)];
                        self.primary = 0;
                    }
                }
            }
        }
        self.refresh_cache(buf, v.lang, v.big);
        // 光标跳进了折叠区域：展开
        for s in self.sels.clone() {
            let l = buf.line_of(s.h);
            if self.folds.iter().any(|&f| self.fold_at(f).is_some_and(|r| r.0 < l && l <= r.1)) {
                self.unfold_line(l);
            }
        }
        self.refresh_disp(buf);
        // ---- 交互区域
        let n_lines = buf.lines();
        let digits = n_lines.to_string().len().max(3) as f32;
        let cw = ui.fonts(|f| f.glyph_width(&FontId::new(v.font_size, FontFamily::Monospace), '0'));
        let gutter_w = (digits * cw * 0.92 + 36.0).round();
        let mini_w = if v.minimap { 72.0 } else { 0.0 };
        let gutter = Rect::from_min_max(rect.min, pos2(rect.left() + gutter_w, rect.bottom()));
        let text_rect = Rect::from_min_max(pos2(gutter.right(), rect.top()), pos2(rect.right() - mini_w, rect.bottom()));
        let resp = ui.interact(Rect::from_min_max(gutter.min, text_rect.max), self.id, Sense::click_and_drag());
        if self.focus_req {
            resp.request_focus();
            self.focus_req = false;
        }
        let focused = resp.has_focus();
        out.focused = focused;
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        // Esc 留给编辑器自己处理（取消多光标、选区）
                        escape: true,
                    },
                )
            });
        }

        // ---- 滚轮
        if resp.hovered() || ui.rect_contains_pointer(rect) {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d != Vec2::ZERO {
                self.scroll -= d;
                ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            }
        }

        // ---- 键盘
        if focused {
            let events = ui.input(|i| i.events.clone());
            let mut changed_ver = buf.ver;
            for e in events {
                if let Some(mode) = self.vim.as_ref().map(|v| v.mode) {
                    let keys = vim_keys(&e);
                    let mut vm = self.vim.take().unwrap();
                    let mut skip = false;
                    if mode == crate::vim::Mode::Insert {
                        for k in &keys {
                            if vm.insert_key(self, buf, k) {
                                skip = true;
                            }
                        }
                    } else if !keys.is_empty() {
                        for k in keys {
                            vm.key(self, buf, k, v.lang, now, &mut out);
                        }
                        skip = true;
                    } else if matches!(e, Event::Key { modifiers, .. } if !modifiers.command && !modifiers.ctrl) || matches!(e, Event::Text(_)) {
                        // 普通模式下其他按键（字母的 Key 事件等）不进编辑器
                        skip = true;
                    }
                    self.vim = Some(vm);
                    if let Some(t) = out.copy.take() {
                        ui.ctx().copy_text(t);
                    }
                    if skip {
                        continue;
                    }
                }
                match e {
                    Event::Text(s) => {
                        if !s.chars().any(|c| c.is_control()) && !s.is_empty() {
                            self.insert_text(buf, &s, v.lang, now);
                        }
                    }
                    Event::Key { key, pressed: true, modifiers, .. } => {
                        self.key(ui, buf, key, modifiers, v.lang, now, &mut out);
                    }
                    Event::Copy => {
                        let t = self.copy_text(buf);
                        if !t.is_empty() {
                            ui.ctx().copy_text(t);
                        }
                    }
                    Event::Cut => {
                        let t = self.cut(buf, now);
                        if !t.is_empty() {
                            ui.ctx().copy_text(t);
                        }
                    }
                    Event::Paste(s) => self.paste(buf, &s, now),
                    _ => {}
                }
            }
            if buf.ver != changed_ver {
                out.changed = true;
                changed_ver = buf.ver;
                self.blink = now;
            }
            let _ = changed_ver;
        }
        if out.changed {
            self.refresh_cache(buf, v.lang, v.big);
            self.refresh_disp(buf);
        }
        self.finish_show(ui, rect, buf, v, out, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_show(&mut self, ui: &mut Ui, rect: Rect, buf: &mut Buf, v: &View, mut out: Out, now: f64) -> Out {
        let font = FontId::new(v.font_size, FontFamily::Monospace);
        // 用实际排版量字宽（单个字形宽度和排版后的步进会有细微差别，累积起来就错位了）
        let (cw, wide) = ui.fonts(|f| {
            let a = f.layout_no_wrap("0".repeat(64), font.clone(), Color32::WHITE).size().x / 64.0;
            let w = f.layout_no_wrap("中".repeat(16), font.clone(), Color32::WHITE).size().x / 16.0;
            (a, w)
        });
        self.m = Metrics { cw, wide };
        let m = self.m;
        let row_h = (v.font_size * 1.5).round();
        let n_lines = buf.lines();
        let digits = n_lines.to_string().len().max(3) as f32;
        let gutter_w = (digits * cw * 0.92 + 36.0).round();
        let mini_w = if v.minimap { 72.0 } else { 0.0 };
        let sb_w = 10.0;
        let gutter = Rect::from_min_max(rect.min, pos2(rect.left() + gutter_w, rect.bottom()));
        let text_rect = Rect::from_min_max(pos2(gutter.right(), rect.top()), pos2(rect.right() - mini_w, rect.bottom()));
        let pad_l = 8.0;
        let disp = std::mem::take(&mut self.disp.1);
        let total_h = disp.len() as f32 * row_h + text_rect.height() * 0.6;
        let a0 = (self.scroll.y / row_h).max(0.0) as usize;
        let max_cols = disp.iter().skip(a0.saturating_sub(200)).take(600).map(|&l| buf.line(l).len()).max().unwrap_or(0);
        let content_w = max_cols as f32 * cw * 1.1 + pad_l + 120.0;
        let resp = ui.interact(Rect::from_min_max(gutter.min, text_rect.max), self.id, Sense::click_and_drag());
        let focused = out.focused;
        self.page_rows = ((text_rect.height() / row_h) as usize).saturating_sub(2).max(1);

        // ---- 把光标滚进视野
        if let Some(frac) = self.reveal.take() {
            let h = self.head();
            let l = buf.line_of(h);
            let di = disp.partition_point(|&x| x < l) as f32;
            let y = di * row_h;
            if frac >= 0.0 {
                if y < self.scroll.y || y + row_h > self.scroll.y + text_rect.height() {
                    self.scroll.y = y - text_rect.height() * frac;
                }
            } else {
                let margin = row_h * 2.0;
                if y < self.scroll.y + margin {
                    self.scroll.y = y - margin;
                } else if y + row_h > self.scroll.y + text_rect.height() - margin - 12.0 {
                    self.scroll.y = y + row_h - text_rect.height() + margin + 12.0;
                }
            }
            let x = xpos(buf.line(l), h - buf.start(l), m) + pad_l;
            let vis_w = text_rect.width() - sb_w - 20.0;
            if x < self.scroll.x + 20.0 {
                self.scroll.x = (x - 40.0).max(0.0);
            } else if x > self.scroll.x + vis_w {
                self.scroll.x = x - vis_w + 40.0;
            }
            self.blink = now;
        }
        self.scroll.y = self.scroll.y.clamp(0.0, (total_h - text_rect.height()).max(0.0));
        self.scroll.x = self.scroll.x.clamp(0.0, (content_w - text_rect.width() + sb_w).max(0.0));

        let origin = pos2(text_rect.left() + pad_l - self.scroll.x, text_rect.top() - self.scroll.y);
        let first = ((self.scroll.y / row_h).floor() as usize).min(disp.len());
        let last = (((self.scroll.y + text_rect.height()) / row_h).ceil() as usize + 1).min(disp.len());
        let row_y = |di: usize| origin.y + di as f32 * row_h;
        let line_di = |l: usize| disp.partition_point(|&x| x < l);
        let x_of = |b: usize| {
            let l = buf.line_of(b);
            origin.x + xpos(buf.line(l), b - buf.start(l), m)
        };

        // 屏幕坐标 → 字节位置
        let pos_to_byte = |p: Pos2| -> (usize, usize) {
            let di = (((p.y - origin.y) / row_h).floor().max(0.0) as usize).min(disp.len().saturating_sub(1));
            let l = disp.get(di).copied().unwrap_or(0);
            (buf.start(l) + byte_at_x(buf.line(l), (p.x - origin.x).max(0.0), m), l)
        };

        // ---- 鼠标
        let mods = ui.input(|i| i.modifiers);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let pressed = ui.input(|i| i.pointer.primary_pressed() || i.pointer.button_pressed(egui::PointerButton::Middle));
        let middle = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
        let in_text = pointer.is_some_and(|p| text_rect.contains(p) && p.x < text_rect.right() - sb_w && p.y < text_rect.bottom() - if content_w > text_rect.width() { sb_w } else { 0.0 });
        let in_gutter = pointer.is_some_and(|p| gutter.contains(p));
        if resp.secondary_clicked() && in_text {
            // 右键：点在选区外时先把光标移过去，菜单由外层挂
            if let Some(p) = pointer {
                let (b, _) = pos_to_byte(p);
                if !self.sels.iter().any(|s| s.min() <= b && b <= s.max()) {
                    self.sels = vec![Sel::at(b)];
                    self.primary = 0;
                }
            }
            resp.request_focus();
        }
        out.menu = Some(resp.clone());
        if pressed && (in_text || in_gutter) && resp.hovered() {
            resp.request_focus();
            self.blink = now;
            let p = pointer.unwrap();
            let (b, l) = pos_to_byte(p);
            // 连击计数
            let (t0, p0, n0) = self.clicks;
            let n = if now - t0 < 0.45 && p0.distance(p) < 6.0 { (n0 % 3) + 1 } else { 1 };
            self.clicks = (now, p, n);
            if in_gutter {
                let chevron = p.x > gutter.right() - 18.0;
                if chevron && self.fold_at(l).is_some() {
                    if !self.folds.remove(&l) {
                        self.folds.insert(l);
                    }
                } else if p.x < gutter.left() + 10.0 {
                    if !self.bookmarks.remove(&l) {
                        self.bookmarks.insert(l);
                    }
                } else {
                    let end = if l + 1 < buf.lines() { buf.start(l + 1) } else { buf.text.len() };
                    let s = if mods.shift {
                        Sel::new(self.primary_sel().a.min(buf.start(l)), end)
                    } else {
                        Sel::new(buf.start(l), end)
                    };
                    self.sels = vec![s];
                    self.primary = 0;
                    self.drag = Some(Drag {
                        mode: DragMode::Line,
                        origin: Sel::new(buf.start(l), end),
                        idx: 0,
                        col_from: (l, 0.0),
                    });
                }
            } else if (mods.alt && mods.shift) || middle {
                let col = (p.x - origin.x).max(0.0);
                self.sels = vec![Sel::at(b)];
                self.primary = 0;
                self.drag = Some(Drag {
                    mode: DragMode::Column,
                    origin: Sel::at(b),
                    idx: 0,
                    col_from: (l, col),
                });
            } else if mods.alt {
                self.sels.push(Sel::at(b));
                self.primary = self.sels.len() - 1;
                self.normalize();
                self.drag = Some(Drag {
                    mode: DragMode::Char,
                    origin: Sel::at(b),
                    idx: self.primary,
                    col_from: (l, 0.0),
                });
            } else if mods.shift {
                let a = self.primary_sel().a;
                self.sels = vec![Sel::new(a, b)];
                self.primary = 0;
                self.drag = Some(Drag {
                    mode: DragMode::Char,
                    origin: Sel::at(a),
                    idx: 0,
                    col_from: (l, 0.0),
                });
            } else {
                let s = match n {
                    2 => buf.word_at(b).map(|(s, e)| Sel::new(s, e)).unwrap_or(Sel::new(b, buf.next(b))),
                    3 => Sel::new(buf.start(l), if l + 1 < buf.lines() { buf.start(l + 1) } else { buf.text.len() }),
                    _ => Sel::at(b),
                };
                self.sels = vec![s];
                self.primary = 0;
                let mode = match n {
                    2 => DragMode::Word,
                    3 => DragMode::Line,
                    _ => DragMode::Char,
                };
                self.drag = Some(Drag {
                    mode,
                    origin: s,
                    idx: 0,
                    col_from: (l, 0.0),
                });
                self.ctrl_down_at = mods.command.then_some(b);
            }
        }
        let down = ui.input(|i| i.pointer.primary_down() || i.pointer.button_down(egui::PointerButton::Middle));
        if let (Some(dr), true) = (&self.drag, down) {
            if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                // 拖到边缘外自动滚动
                let mut auto = Vec2::ZERO;
                if p.y < text_rect.top() + 4.0 {
                    auto.y = -((text_rect.top() + 4.0 - p.y) * 0.5).clamp(2.0, 40.0);
                } else if p.y > text_rect.bottom() - 4.0 {
                    auto.y = ((p.y - text_rect.bottom() + 4.0) * 0.5).clamp(2.0, 40.0);
                }
                if p.x > text_rect.right() - 4.0 && dr.mode != DragMode::Line {
                    auto.x = ((p.x - text_rect.right() + 4.0) * 0.5).clamp(2.0, 40.0);
                } else if p.x < text_rect.left() && self.scroll.x > 0.0 && dr.mode != DragMode::Line {
                    auto.x = -((text_rect.left() - p.x) * 0.5).clamp(2.0, 40.0);
                }
                if auto != Vec2::ZERO {
                    self.scroll += auto;
                    ui.ctx().request_repaint();
                }
                let moved = ui.input(|i| i.pointer.is_decidedly_dragging()) || auto != Vec2::ZERO;
                if moved {
                    let (b, l) = pos_to_byte(p);
                    let o = dr.origin;
                    let idx = dr.idx.min(self.sels.len().saturating_sub(1));
                    match dr.mode {
                        DragMode::Char => {
                            self.sels[idx] = Sel::new(o.a, b);
                            self.ctrl_down_at = None;
                        }
                        DragMode::Word => {
                            let (ws, we) = buf.word_at(b).unwrap_or((b, b));
                            self.sels[idx] = if b < o.min() { Sel::new(o.max(), ws) } else { Sel::new(o.min(), we.max(o.max())) };
                        }
                        DragMode::Line => {
                            let ls = buf.start(l);
                            let le = if l + 1 < buf.lines() { buf.start(l + 1) } else { buf.text.len() };
                            self.sels[idx] = if ls < o.min() { Sel::new(o.max(), ls) } else { Sel::new(o.min(), le) };
                        }
                        DragMode::Column => {
                            let (l0, c0) = dr.col_from;
                            let c1 = (p.x - origin.x).max(0.0);
                            let (la, lb) = (l0.min(l), l0.max(l));
                            let mut v = vec![];
                            for di in line_di(la)..=line_di(lb).min(disp.len().saturating_sub(1)) {
                                let ln = disp[di];
                                let line = buf.line(ln);
                                let s = buf.start(ln);
                                v.push(Sel::new(s + byte_at_x(line, c0, m), s + byte_at_x(line, c1, m)));
                            }
                            if !v.is_empty() {
                                self.sels = v;
                                self.primary = if l < l0 { 0 } else { self.sels.len() - 1 };
                            }
                        }
                    }
                    if dr.mode != DragMode::Column {
                        let keep = self.sels[idx];
                        self.normalize();
                        self.primary = self.sels.iter().position(|s| s.min() <= keep.h && keep.h <= s.max()).unwrap_or(self.primary);
                    }
                }
            }
        } else if !down {
            if self.drag.take().is_some() {
                if let (Some(b), true) = (self.ctrl_down_at.take(), mods.command) {
                    if self.sels.len() == 1 && self.sels[0].empty() {
                        out.ctrl_click = Some((b, pointer.unwrap_or_default()));
                    }
                }
            }
        }
        if resp.hovered() && !in_gutter {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, BG0());
        let tp = ui.painter_at(text_rect);
        let ph = self.primary_sel();
        let prim_line = buf.line_of(ph.h);

        // ---- 当前行
        if self.sels.iter().all(|s| s.empty()) {
            for s in &self.sels {
                let l = buf.line_of(s.h);
                let di = line_di(l);
                if di >= first && di < last {
                    let y = row_y(di);
                    tp.rect_filled(
                        Rect::from_min_max(pos2(text_rect.left(), y), pos2(text_rect.right(), y + row_h)),
                        0.0,
                        tint(if focused { 2 } else { 1 }),
                    );
                }
            }
        }

        // ---- 光标所在的函数体、#if 块：很淡的底色，左边一条细线
        for (range, c, x0) in [(v.scope, ACCENT(), 0.0), (v.if_block, VIOLET(), 3.0)] {
            let Some((a, b)) = range else { continue };
            let (da, db) = (line_di(a), line_di(b));
            if db < first || da >= last {
                continue;
            }
            let y0 = row_y(da.max(first));
            let y1 = row_y((db + 1).min(last));
            let r = Rect::from_min_max(pos2(text_rect.left(), y0), pos2(text_rect.right(), y1));
            tp.rect_filled(r, 0.0, c.gamma_multiply(if is_light() { 0.035 } else { 0.03 }));
            tp.rect_filled(Rect::from_min_max(pos2(text_rect.left() + x0, y0), pos2(text_rect.left() + x0 + 2.0, y1)), 0.0, c.gamma_multiply(0.35));
        }

        // ---- 行尾附注
        if let Some((l, g)) = v.ghost {
            let di = line_di(l);
            if di >= first && di < last && disp.get(di) == Some(&l) {
                let x = x_of(buf.end(l)) + m.cw * 4.0;
                tp.text(
                    pos2(x, row_y(di) + row_h / 2.0),
                    Align2::LEFT_CENTER,
                    g,
                    FontId::new(font.size * 0.88, FontFamily::Proportional),
                    TEXT3().gamma_multiply(0.85),
                );
            }
        }

        // ---- 查找结果
        if !v.find.is_empty() {
            let lo = disp.get(first).map_or(0, |&l| buf.start(l));
            let hi = disp.get(last.saturating_sub(1)).map_or(buf.text.len(), |&l| buf.end(l));
            let i0 = v.find.partition_point(|&(h, _)| h < lo);
            for (k, &(h, he)) in v.find.iter().enumerate().skip(i0) {
                if h > hi {
                    break;
                }
                let l = buf.line_of(h);
                let di = line_di(l);
                if disp.get(di) != Some(&l) {
                    continue;
                }
                let y = row_y(di);
                let r = Rect::from_min_max(pos2(x_of(h), y + 1.0), pos2(x_of(he.min(buf.end(l))), y + row_h - 1.0));
                let cur = v.find_cur == Some(k);
                tp.rect_filled(r, 2.0, WARN().gamma_multiply(if cur { 0.45 } else { 0.2 }));
                if cur {
                    tp.rect_stroke(r, 2.0, st(1.0, WARN()), egui::StrokeKind::Outside);
                }
            }
        }

        // ---- F8 高亮的词
        if !v.marks.is_empty() {
            for di in first..last {
                let l = disp[di];
                let line = buf.line(l);
                let ls = buf.start(l);
                for (w, c) in v.marks {
                    if w.is_empty() {
                        continue;
                    }
                    let mut from = 0;
                    while let Some(k) = line[from..].find(w.as_str()) {
                        let s = from + k;
                        from = s + w.len();
                        let ok = !line[..s].chars().next_back().is_some_and(is_ident) && !line[s + w.len()..].chars().next().is_some_and(is_ident);
                        if ok {
                            let y = row_y(di);
                            let r = Rect::from_min_max(pos2(x_of(ls + s) - 1.0, y + 1.0), pos2(x_of(ls + s + w.len()) + 1.0, y + row_h - 1.0));
                            tp.rect_filled(r, 3.0, c.gamma_multiply(if is_light() { 0.32 } else { 0.30 }));
                            tp.hline(r.x_range(), r.bottom() - 0.5, st(1.5, *c));
                        }
                    }
                }
            }
        }

        // ---- 同名单词 / 选中文字的其他出现位置
        let hl_word: Option<String> = if !v.big && self.sels.len() == 1 && v.find.is_empty() {
            if ph.empty() {
                buf.word_at(ph.h).map(|(s, e)| buf.text[s..e].to_string())
            } else if buf.word_at(ph.min()) == Some((ph.min(), ph.max())) {
                Some(buf.text[ph.range()].to_string())
            } else {
                None
            }
        } else {
            None
        };
        if let Some(w) = &hl_word {
            for di in first..last {
                let l = disp[di];
                let line = buf.line(l);
                let ls = buf.start(l);
                let mut from = 0;
                while let Some(k) = line[from..].find(w.as_str()) {
                    let s = from + k;
                    from = s + w.len();
                    let ok = !line[..s].chars().next_back().is_some_and(is_ident) && !line[s + w.len()..].chars().next().is_some_and(is_ident);
                    if ok {
                        let y = row_y(di);
                        let r = Rect::from_min_max(pos2(x_of(ls + s), y + 1.0), pos2(x_of(ls + s + w.len()), y + row_h - 1.0));
                        tp.rect_filled(r, 2.0, ACCENT().gamma_multiply(if is_light() { 0.16 } else { 0.14 }));
                    }
                }
            }
        }

        // ---- 选区
        let sel_fill = ui.visuals().selection.bg_fill.gamma_multiply(if focused { 1.0 } else { 0.6 });
        for s in &self.sels {
            if s.empty() {
                continue;
            }
            let (a, b) = (s.min(), s.max());
            let (la, lb) = (buf.line_of(a), buf.line_of(b));
            let (da, db) = (line_di(la), line_di(lb));
            for di in da.max(first)..=db.min(last.saturating_sub(1)) {
                let Some(&l) = disp.get(di) else { break };
                if l < la || l > lb {
                    continue;
                }
                let ls = buf.start(l);
                let le = buf.end(l);
                let x0 = x_of(a.max(ls));
                let mut x1 = x_of(b.min(le));
                if b > le {
                    x1 += cw * 0.6;
                }
                let y = row_y(di);
                tp.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1.max(x0 + 2.0), y + row_h)), 3.0, sel_fill);
            }
        }

        // ---- 括号配对
        let pair = if self.sels.len() == 1 && ph.empty() { self.bracket_pair(buf) } else { None };
        if let Some((a, b)) = pair {
            for at in [a, b] {
                let l = buf.line_of(at);
                let di = line_di(l);
                if disp.get(di) == Some(&l) && di >= first && di < last {
                    let y = row_y(di);
                    let r = Rect::from_min_max(pos2(x_of(at), y + 1.0), pos2(x_of(at) + cw, y + row_h - 1.0));
                    tp.rect(r, 2.0, pal::BRACKET().gamma_multiply(0.18), st(1.0, pal::BRACKET()), egui::StrokeKind::Inside);
                }
            }
        }

        // ---- 缩进参考线
        let active_guide = pair.and_then(|(a, b)| {
            let (la, lb) = (buf.line_of(a), buf.line_of(b));
            (lb > la + 1).then(|| (la, lb, vcol(buf.line(la), leading_ws(buf.line(la)).len())))
        });
        if !v.big {
            let indent_of = |l: usize| -> Option<usize> {
                let line = buf.line(l);
                (!line.trim().is_empty()).then(|| vcol(line, leading_ws(line).len()))
            };
            let mut prev_ind = 0usize;
            for di in first..last {
                let l = disp[di];
                let ind = match indent_of(l) {
                    Some(i) => i,
                    None => {
                        // 空行用上下非空行里较小的缩进
                        let next = disp[di..].iter().take(50).find_map(|&x| indent_of(x)).unwrap_or(0);
                        prev_ind.min(next)
                    }
                };
                if indent_of(l).is_some() {
                    prev_ind = ind;
                }
                let y = row_y(di);
                let mut c = TAB;
                while c <= ind.saturating_sub(1) + 1 && c <= ind {
                    let col = c - TAB;
                    let x = (origin.x + col as f32 * cw).round() + 0.5;
                    let on = active_guide.is_some_and(|(la, lb, gc)| gc == col && l > la && l < lb);
                    if x > text_rect.left() {
                        tp.vline(x, egui::Rangef::new(y, y + row_h), st(1.0, if on { pal::GUIDE_ON() } else { pal::GUIDE() }));
                    }
                    c += TAB;
                }
            }
        }

        // ---- 文字
        let toks = &self.cache.tokens;
        for di in first..last {
            let l = disp[di];
            let ls = buf.start(l);
            let line = buf.line(l);
            let y = row_y(di);
            if line.is_empty() {
                continue;
            }
            let ii = v.inactive.partition_point(|r| r.1 < l);
            let dim = v.inactive.get(ii).is_some_and(|r| r.0 <= l);
            // 只画横向可见部分附近的字
            let min_x = (self.scroll.x - 40.0).max(0.0);
            let max_x = self.scroll.x + text_rect.width() + 40.0;
            let mut ti = toks.partition_point(|t| t.1 <= ls);
            let mut col = 0.0f32;
            let mut run = String::new();
            let mut run_col = 0.0f32;
            let mut run_color = Color32::TRANSPARENT;
            let mut run_italic = false;
            let flush = |run: &mut String, run_col: f32, c: Color32, it: bool| {
                if run.is_empty() {
                    return;
                }
                let mut job = egui::text::LayoutJob::single_section(
                    std::mem::take(run),
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: c,
                        italics: it,
                        ..Default::default()
                    },
                );
                job.wrap.max_width = f32::INFINITY;
                let g = ui.fonts(|f| f.layout_job(job));
                let gy = y + (row_h - g.size().y) / 2.0;
                tp.galley(pos2(origin.x + run_col, gy), g, c);
            };
            for (i, ch) in line.char_indices() {
                if col > max_x {
                    break;
                }
                let b = ls + i;
                while ti < toks.len() && toks[ti].1 <= b {
                    ti += 1;
                }
                let tok = toks.get(ti).filter(|t| t.0 <= b).map_or(Tok::Text, |t| t.2);
                let mut color = highlight::color(tok);
                if tok == Tok::Text && is_ident(ch) {
                    if let (Some(sem), Some(t)) = (v.sem, toks.get(ti).filter(|t| t.0 <= b)) {
                        let w = &buf.text[t.0..t.1];
                        if let Some(k) = sem.get(w) {
                            color = kind_color(*k);
                        }
                    }
                }
                if dim {
                    color = color.gamma_multiply(0.42);
                }
                let italic = tok == Tok::Comment;
                let w = m.adv(ch, col);
                let wide = is_wide(ch);
                if ch == '\t' || wide || color != run_color || italic != run_italic || col < min_x {
                    flush(&mut run, run_col, run_color, run_italic);
                    run_col = col;
                    run_color = color;
                    run_italic = italic;
                }
                if col + w >= min_x {
                    if ch == '\t' {
                        run_col = col + w;
                    } else if wide {
                        run.push(ch);
                        flush(&mut run, col, color, italic);
                        run_col = col + w;
                    } else {
                        if run.is_empty() {
                            run_col = col;
                        }
                        run.push(ch);
                    }
                } else {
                    run_col = col + w;
                }
                col += w;
            }
            flush(&mut run, run_col, run_color, run_italic);
            // 折叠占位
            if self.folds.contains(&l) {
                let x = origin.x + xpos(line, line.len(), m) + 6.0;
                let r = Rect::from_min_size(pos2(x, y + 3.0), vec2(cw * 3.0 + 6.0, row_h - 6.0));
                tp.rect(r, 3.0, tint(10), st(1.0, LINE_HI()), egui::StrokeKind::Inside);
                tp.text(r.center(), Align2::CENTER_CENTER, "⋯", FontId::proportional(v.font_size), TEXT2());
            }
        }

        // ---- 光标
        let on = !focused || ((now - self.blink) % 1.06) < 0.62;
        if focused {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        for (i, s) in self.sels.iter().enumerate() {
            let l = buf.line_of(s.h);
            let di = line_di(l);
            if disp.get(di) != Some(&l) || di < first || di >= last {
                continue;
            }
            let x = x_of(s.h).round();
            let y = row_y(di);
            let c = if i == self.primary { ACCENT() } else { ACCENT().gamma_multiply(0.75) };
            if let Some(vm) = self.vim.as_ref().filter(|v| v.mode != crate::vim::Mode::Insert) {
                // Vim 普通 / 可视模式：块状光标，盖住光标下的字符
                let at = if vm.mode != crate::vim::Mode::Normal && s.h > s.a { buf.prev(s.h) } else { s.h };
                let l2 = buf.line_of(at);
                let di2 = line_di(l2);
                let x = x_of(at).round();
                let ch = buf.char_at(at).filter(|c| *c != '\n');
                let w = ch.map_or(m.cw, |c| m.adv(c, 0.0).max(m.cw * 0.6));
                let y = row_y(di2);
                let c = if focused { c.gamma_multiply(0.55) } else { TEXT3().gamma_multiply(0.4) };
                if focused {
                    tp.rect_filled(Rect::from_min_max(pos2(x, y + 1.0), pos2(x + w, y + row_h - 1.0)), 1.0, c);
                } else {
                    tp.rect_stroke(Rect::from_min_max(pos2(x, y + 1.0), pos2(x + w, y + row_h - 1.0)), 1.0, st(1.0, c), egui::StrokeKind::Inside);
                }
                continue;
            }
            if focused && on {
                tp.rect_filled(Rect::from_min_max(pos2(x - 1.0, y + 2.0), pos2(x + 1.0, y + row_h - 2.0)), 0.0, c);
            } else if !focused {
                tp.rect_filled(Rect::from_min_max(pos2(x - 0.5, y + 2.0), pos2(x + 0.5, y + row_h - 2.0)), 0.0, TEXT3().gamma_multiply(0.6));
            }
        }

        // ---- 顶部粘住的作用域（函数、结构体）
        let mut sticky_h = 0.0;
        if !v.big && self.scroll.y > 1.0 {
            if disp.get(first).is_some() {
                // 粘住 n 行时，第一行露出来的正文是 disp[first + n]；选让"它的外层作用域数 ≤ n"的最小 n
                let scopes = |l: usize| -> Vec<usize> {
                    let mut v = vec![];
                    let mut x = l;
                    while let Some(f) = self.enclosing(x) {
                        // K&R 以外的写法 `{` 单独一行，标题用上面的函数签名/条件行
                        let mut hd = f.0;
                        if buf.line(hd).trim() == "{" {
                            while hd > 0 && buf.line(hd - 1).trim().is_empty() {
                                hd -= 1;
                            }
                            hd = hd.saturating_sub(1);
                        }
                        v.push(hd);
                        x = f.0;
                    }
                    v.reverse();
                    v
                };
                let mut chain = vec![];
                for n in 0..=3 {
                    let Some(&l) = disp.get(first + n) else { break };
                    let sc = scopes(l);
                    if sc.len() <= n || n == 3 {
                        chain = sc.into_iter().take(3).collect();
                        break;
                    }
                }
                // 粘住的行下面那一行如果正好是作用域结尾附近就不显示，避免挡字
                if !chain.is_empty() {
                    let h = chain.len() as f32 * row_h;
                    sticky_h = h;
                    let sr = Rect::from_min_size(text_rect.min, vec2(text_rect.width() - sb_w, h));
                    tp.rect_filled(sr, 0.0, BG1());
                    tp.hline(sr.x_range(), sr.bottom(), st(1.0, LINE()));
                    for (k, &sl) in chain.iter().enumerate() {
                        let y = sr.top() + k as f32 * row_h;
                        let text = buf.line(sl).trim_end().to_string();
                        let ind = vcol(&text, leading_ws(&text).len());
                        let job = highlight::layout(text.trim_start(), v.lang, font.clone());
                        let g = ui.fonts(|f| f.layout_job(job));
                        tp.galley(
                            pos2(origin.x + self.scroll.x - 0.0 + ind as f32 * cw - self.scroll.x.min(ind as f32 * cw), y + (row_h - g.size().y) / 2.0),
                            g,
                            TEXT(),
                        );
                        let rr = Rect::from_min_size(pos2(sr.left(), y), vec2(sr.width(), row_h));
                        let r = ui.interact(rr, self.id.with(("sticky", k)), Sense::click());
                        if r.hovered() {
                            tp.rect_filled(rr, 0.0, tint(8));
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if r.clicked() {
                            let b = buf.start(sl) + leading_ws(buf.line(sl)).len();
                            self.select(b, b);
                            self.reveal = Some(0.2);
                        }
                    }
                    // 行号区同样遮住
                    let gp = ui.painter_at(gutter);
                    gp.rect_filled(Rect::from_min_size(gutter.min, vec2(gutter.width(), h)), 0.0, BG1());
                    for (k, &sl) in chain.iter().enumerate() {
                        let y = gutter.top() + k as f32 * row_h + row_h / 2.0;
                        gp.text(
                            pos2(gutter.right() - 20.0, y),
                            Align2::RIGHT_CENTER,
                            (sl + 1).to_string(),
                            FontId::new(v.font_size * 0.9, FontFamily::Monospace),
                            TEXT3(),
                        );
                    }
                    gp.hline(gutter.x_range(), gutter.top() + h, st(1.0, LINE()));
                }
            }
        }

        // ---- 行号、折叠箭头、书签、Git 标记
        let gp = ui.painter_at(Rect::from_min_max(pos2(gutter.left(), gutter.top() + sticky_h), gutter.max));
        gp.rect_filled(gutter, 0.0, BG0());
        let hover_gutter = pointer.is_some_and(|p| gutter.contains(p)) || ui.rect_contains_pointer(gutter);
        let sel_lines: Vec<(usize, usize)> = self.sels.iter().map(|s| (buf.line_of(s.min()), buf.line_of(s.max()))).collect();
        for di in first..last {
            let l = disp[di];
            let y = row_y(di);
            let cy = y + row_h / 2.0;
            let cur = sel_lines.iter().any(|&(a, b)| a <= l && l <= b);
            gp.text(
                pos2(gutter.right() - 20.0, cy),
                Align2::RIGHT_CENTER,
                (l + 1).to_string(),
                FontId::new(v.font_size * 0.9, FontFamily::Monospace),
                if cur { TEXT() } else { TEXT3().gamma_multiply(0.85) },
            );
            if let Some(_f) = self.fold_at(l) {
                let folded = self.folds.contains(&l);
                if folded || hover_gutter {
                    let c = pos2(gutter.right() - 10.0, cy);
                    let s = 3.5;
                    let pts = if folded {
                        vec![c + vec2(-s * 0.6, -s), c + vec2(s * 0.7, 0.0), c + vec2(-s * 0.6, s)]
                    } else {
                        vec![c + vec2(-s, -s * 0.5), c + vec2(s, -s * 0.5), c + vec2(0.0, s * 0.7)]
                    };
                    gp.add(Shape::line(pts, st(1.3, if folded { ACCENT() } else { TEXT3() })));
                }
            }
            if self.bookmarks.contains(&l) {
                gp.add(Shape::convex_polygon(
                    vec![
                        pos2(gutter.left() + 3.0, y + 3.0),
                        pos2(gutter.left() + 10.0, y + 3.0),
                        pos2(gutter.left() + 10.0, y + row_h - 3.0),
                        pos2(gutter.left() + 6.5, y + row_h - 6.0),
                        pos2(gutter.left() + 3.0, y + row_h - 3.0),
                    ],
                    VIOLET(),
                    egui::Stroke::NONE,
                ));
            }
        }
        for &(a, b, k) in v.git {
            let (da, db) = (line_di(a), line_di(b.max(a)));
            if db < first || da >= last {
                continue;
            }
            let x = gutter.right() - 3.0;
            let c = match k {
                'A' => OK(),
                'D' => ERR(),
                _ => DEBUG(),
            };
            if k == 'D' {
                let y = row_y(da);
                gp.add(Shape::convex_polygon(vec![pos2(x - 3.0, y - 4.0), pos2(x + 2.0, y), pos2(x - 3.0, y + 4.0)], c, egui::Stroke::NONE));
            } else {
                gp.rect_filled(Rect::from_min_max(pos2(x - 1.5, row_y(da)), pos2(x + 1.5, row_y(db) + row_h)), 0.0, c);
            }
        }

        // ---- 悬浮的标识符
        if let Some(pp) = pointer.filter(|_| in_text && resp.hovered() && self.drag.is_none()) {
            if pp.y > text_rect.top() + sticky_h {
                let (b, l) = pos_to_byte(pp);
                let line_end_x = x_of(buf.end(l));
                if pp.x <= line_end_x + 2.0 {
                    let w = buf.token_at(b).or_else(|| (b > 0).then(|| buf.token_at(buf.prev(b))).flatten());
                    if let Some((s, e)) = w {
                        let di = line_di(l);
                        let r = Rect::from_min_max(pos2(x_of(s), row_y(di)), pos2(x_of(e), row_y(di) + row_h));
                        if r.expand(1.0).contains(pp) {
                            out.hover = Some((s, e, r));
                            if mods.command && v.lang.is_code() && !buf.text[s..].starts_with(|c: char| c.is_ascii_digit()) {
                                tp.hline(r.x_range(), r.bottom() - 1.0, st(1.0, ACCENT()));
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                        }
                    }
                }
            }
        }

        // ---- 滚动条
        let track_v = Rect::from_min_max(pos2(text_rect.right() - sb_w, text_rect.top()), pos2(text_rect.right(), text_rect.bottom()));
        if total_h > text_rect.height() {
            let frac = text_rect.height() / total_h;
            let th = (track_v.height() * frac).max(24.0);
            let ty = track_v.top() + (track_v.height() - th) * (self.scroll.y / (total_h - text_rect.height()).max(1.0));
            let thumb = Rect::from_min_size(pos2(track_v.left() + 2.0, ty), vec2(sb_w - 4.0, th));
            let r = ui.interact(track_v, self.id.with("vsb"), Sense::click_and_drag());
            if r.drag_started() || r.clicked() {
                if let Some(pp) = r.interact_pointer_pos() {
                    let off = if thumb.contains(pp) { pp.y - thumb.top() } else { th / 2.0 };
                    self.sb_grab = Some((true, off));
                }
            }
            if let (Some((true, off)), Some(pp)) = (self.sb_grab, r.interact_pointer_pos()) {
                let t = ((pp.y - off - track_v.top()) / (track_v.height() - th).max(1.0)).clamp(0.0, 1.0);
                self.scroll.y = t * (total_h - text_rect.height());
            }
            if !r.dragged() && !r.drag_started() {
                if self.sb_grab.is_some_and(|g| g.0) {
                    self.sb_grab = None;
                }
            }
            let hot = r.hovered() || r.dragged();
            tp.rect_filled(thumb, 4.0, tint(if hot { 46 } else { 24 }));
            // 滚动条上标出查找结果、选区、书签的位置
            let mark = |l: usize, c: Color32, x0: f32, w: f32| {
                let y = track_v.top() + line_di(l) as f32 / disp.len().max(1) as f32 * track_v.height();
                tp.rect_filled(Rect::from_min_size(pos2(track_v.left() + x0, y), vec2(w, 2.0)), 0.0, c);
            };
            for &(h, _) in v.find.iter().take(3000) {
                mark(buf.line_of(h), WARN(), 5.0, 5.0);
            }
            for &bm in &self.bookmarks {
                mark(bm, VIOLET(), 0.0, 4.0);
            }
            if !v.marks.is_empty() {
                let key = v.marks.iter().fold(v.marks.len() as u64, |h, (w, c)| {
                    w.bytes().fold(h.wrapping_mul(31).wrapping_add(c.r() as u64), |h, b| h.wrapping_mul(131).wrapping_add(b as u64))
                });
                if self.mark_cache.0 != buf.ver || self.mark_cache.1 != key {
                    let mut hits = vec![];
                    for (w, c) in v.marks {
                        if w.is_empty() {
                            continue;
                        }
                        let mut from = 0;
                        while let Some(k) = buf.text[from..].find(w.as_str()) {
                            let s = from + k;
                            from = s + w.len();
                            let ok = !buf.text[..s].chars().next_back().is_some_and(is_ident) && !buf.text[s + w.len()..].chars().next().is_some_and(is_ident);
                            if ok {
                                hits.push((buf.line_of(s), *c));
                                if hits.len() > 5000 {
                                    break;
                                }
                            }
                        }
                    }
                    self.mark_cache = (buf.ver, key, hits);
                }
                for &(l, c) in &self.mark_cache.2 {
                    mark(l, c, sb_w - 4.0, 4.0);
                }
            }
            mark(prim_line, ACCENT(), 0.0, sb_w);
        }
        if content_w > text_rect.width() {
            let track_h = Rect::from_min_max(pos2(text_rect.left(), text_rect.bottom() - sb_w), pos2(text_rect.right() - sb_w, text_rect.bottom()));
            let frac = (text_rect.width() - sb_w) / content_w;
            let tw = (track_h.width() * frac).max(24.0);
            let tx = track_h.left() + (track_h.width() - tw) * (self.scroll.x / (content_w - text_rect.width() + sb_w).max(1.0));
            let thumb = Rect::from_min_size(pos2(tx, track_h.top() + 2.0), vec2(tw, sb_w - 4.0));
            let r = ui.interact(track_h, self.id.with("hsb"), Sense::click_and_drag());
            if r.drag_started() || r.clicked() {
                if let Some(pp) = r.interact_pointer_pos() {
                    let off = if thumb.contains(pp) { pp.x - thumb.left() } else { tw / 2.0 };
                    self.sb_grab = Some((false, off));
                }
            }
            if let (Some((false, off)), Some(pp)) = (self.sb_grab, r.interact_pointer_pos()) {
                let t = ((pp.x - off - track_h.left()) / (track_h.width() - tw).max(1.0)).clamp(0.0, 1.0);
                self.scroll.x = t * (content_w - text_rect.width() + sb_w);
            }
            if !r.dragged() && !r.drag_started() && self.sb_grab.is_some_and(|g| !g.0) {
                self.sb_grab = None;
            }
            tp.rect_filled(thumb, 4.0, tint(if r.hovered() || r.dragged() { 46 } else { 24 }));
        }

        // ---- 小地图
        if v.minimap {
            let mm = Rect::from_min_max(pos2(rect.right() - mini_w, rect.top()), rect.max);
            let mp = ui.painter_at(mm);
            mp.rect_filled(mm, 0.0, BG0());
            mp.vline(mm.left(), mm.y_range(), st(1.0, LINE()));
            let n = disp.len().max(1);
            let scale = (mm.height() / n as f32).min(2.5);
            let step = (1.0 / scale).ceil().max(1.0) as usize;
            let mut di = 0;
            while di < disp.len() {
                let l = disp[di];
                let line = buf.line(l);
                let t = line.trim_start();
                if !t.is_empty() {
                    let ind = vcol(line, line.len() - t.len());
                    let tok = self
                        .cache
                        .tokens
                        .get(self.cache.tokens.partition_point(|x| x.1 <= buf.start(l) + (line.len() - t.len())))
                        .map_or(Tok::Text, |x| x.2);
                    let c = match tok {
                        Tok::Comment => pal::COMMENT(),
                        Tok::Pre => pal::PRE(),
                        Tok::Kw | Tok::Ty => pal::KW(),
                        _ => TEXT3(),
                    }
                    .gamma_multiply(0.6);
                    let x0 = mm.left() + 6.0 + (ind as f32 * 0.8).min(28.0);
                    let x1 = (x0 + t.chars().count() as f32 * 0.8).min(mm.right() - 4.0);
                    let y = mm.top() + 4.0 + di as f32 * scale;
                    mp.rect_filled(Rect::from_min_max(pos2(x0, y), pos2(x1, y + (scale * 0.8).max(1.0))), 0.0, c);
                }
                di += step;
            }
            for &(h, _) in v.find.iter().take(2000) {
                let y = mm.top() + 4.0 + line_di(buf.line_of(h)) as f32 * scale;
                mp.rect_filled(Rect::from_min_size(pos2(mm.right() - 5.0, y), vec2(4.0, 2.0)), 0.0, WARN());
            }
            let vy0 = mm.top() + 4.0 + self.scroll.y / row_h * scale;
            let vh = (text_rect.height() / row_h * scale).max(8.0);
            let vr = Rect::from_min_size(pos2(mm.left() + 1.0, vy0), vec2(mm.width() - 1.0, vh));
            let mr = ui.interact(mm, self.id.with("minimap"), Sense::click_and_drag());
            mp.rect_filled(vr, 0.0, tint(if mr.hovered() || mr.dragged() { 18 } else { 10 }));
            if mr.drag_started() || mr.clicked() {
                self.mm_drag = true;
            }
            if !mr.dragged() && !mr.drag_started() && !mr.clicked() {
                self.mm_drag = false;
            }
            if let Some(pp) = mr.interact_pointer_pos().filter(|_| self.mm_drag) {
                let line = ((pp.y - mm.top() - 4.0) / scale).max(0.0);
                self.scroll.y = (line * row_h - text_rect.height() / 2.0).clamp(0.0, (total_h - text_rect.height()).max(0.0));
                ui.ctx().request_repaint();
            }
        }

        self.disp.1 = disp;
        out.doc_comment = self.doc_req.take();
        out
    }
}

fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Func | Kind::Proto => pal::FUNC(),
        Kind::Macro => pal::MACRO(),
        Kind::Struct | Kind::Enum | Kind::Typedef | Kind::Class => pal::TY(),
        Kind::EnumConst => pal::CONST(),
        Kind::Var => pal::VAR(),
    }
}

fn same_pos(a: &[Sel], b: &[Sel]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.a == y.a && x.h == y.h)
}

fn shift_lines(set: &mut BTreeSet<usize>, l: usize, removed: usize, added: usize) {
    if removed == 0 && added == 0 {
        return;
    }
    let moved: Vec<usize> = set.range(l + 1..).copied().collect();
    for m in &moved {
        set.remove(m);
    }
    for m in moved {
        if m <= l + removed {
            set.insert(l);
        } else {
            set.insert(m + added - removed);
        }
    }
}

fn pair_of(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

fn outdent_str(ws: &str, unit: &str) -> String {
    if let Some(s) = ws.strip_suffix('\t') {
        return s.to_string();
    }
    let n = ws.len() - ws.trim_end_matches(' ').len();
    ws[..ws.len() - n.min(unit.len().max(1)).min(n)].to_string()
}

fn is_py_block(head: &str) -> bool {
    let w = head.trim_start();
    ["if", "elif", "else", "for", "while", "def", "class", "try", "except", "finally", "with"]
        .iter()
        .any(|k| w.starts_with(k))
}

fn word_left(buf: &Buf, b: usize) -> usize {
    let mut i = b;
    while i > 0 && buf.char_before(i).is_some_and(|c| c == ' ' || c == '\t') {
        i = buf.prev(i);
    }
    let ident = buf.char_before(i).is_some_and(is_ident);
    if i > 0 && buf.char_before(i) == Some('\n') {
        return if i == b { buf.prev(i) } else { i };
    }
    while i > 0 && buf.char_before(i).is_some_and(|c| if ident { is_ident(c) } else { !is_ident(c) && !c.is_whitespace() }) {
        i = buf.prev(i);
    }
    if i == b {
        buf.prev(b)
    } else {
        i
    }
}

fn word_right(buf: &Buf, b: usize) -> usize {
    let n = buf.text.len();
    let mut i = b;
    while i < n && buf.char_at(i).is_some_and(|c| c == ' ' || c == '\t') {
        i = buf.next(i);
    }
    if i < n && buf.char_at(i) == Some('\n') {
        return if i == b { i + 1 } else { i };
    }
    let ident = buf.char_at(i).is_some_and(is_ident);
    while i < n && buf.char_at(i).is_some_and(|c| if ident { is_ident(c) } else { !is_ident(c) && !c.is_whitespace() }) {
        i = buf.next(i);
    }
    if i == b {
        buf.next(b)
    } else {
        i
    }
}

/// Python 这类靠缩进分块的语言：按缩进算折叠范围
fn indent_folds(buf: &Buf) -> Vec<(usize, usize, bool)> {
    let n = buf.lines();
    let ind = |l: usize| -> Option<usize> {
        let line = buf.line(l);
        (!line.trim().is_empty()).then(|| vcol(line, leading_ws(line).len()))
    };
    let mut out = vec![];
    let mut stack: Vec<(usize, usize)> = vec![];
    let mut last_nonblank = 0;
    for l in 0..n {
        let Some(i) = ind(l) else { continue };
        while let Some(&(sl, si)) = stack.last() {
            if i <= si {
                stack.pop();
                if last_nonblank > sl {
                    out.push((sl, last_nonblank, true));
                }
            } else {
                break;
            }
        }
        stack.push((l, i));
        last_nonblank = l;
    }
    while let Some((sl, _)) = stack.pop() {
        if last_nonblank > sl {
            out.push((sl, last_nonblank, true));
        }
    }
    out
}

/// 把输入事件翻译成 Vim 按键；不归 Vim 管的（Ctrl+S 之类）返回空
fn vim_keys(e: &Event) -> Vec<crate::vim::VKey> {
    use crate::vim::VKey;
    match e {
        Event::Text(s) => s.chars().filter(|c| !c.is_control()).map(VKey::Char).collect(),
        Event::Key { key, pressed: true, modifiers, .. } => {
            if modifiers.ctrl || modifiers.command {
                return match key {
                    Key::R => vec![VKey::Ctrl('r')],
                    Key::D => vec![VKey::Ctrl('d')],
                    Key::U => vec![VKey::Ctrl('u')],
                    Key::OpenBracket => vec![VKey::Esc],
                    _ => vec![],
                };
            }
            match key {
                Key::Escape => vec![VKey::Esc],
                Key::Enter => vec![VKey::Enter],
                Key::Backspace => vec![VKey::Back],
                Key::Delete => vec![VKey::Del],
                Key::ArrowLeft => vec![VKey::Left],
                Key::ArrowRight => vec![VKey::Right],
                Key::ArrowUp => vec![VKey::Up],
                Key::ArrowDown => vec![VKey::Down],
                Key::Home => vec![VKey::Home],
                Key::End => vec![VKey::End],
                Key::Tab => vec![VKey::Tab],
                _ => vec![],
            }
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str) -> (Editor, Buf) {
        let b = Buf::new(text.to_string());
        let e = Editor::new(egui::Id::new("t"), &b);
        (e, b)
    }

    #[test]
    fn type_undo_redo() {
        let (mut e, mut b) = ed("ab");
        e.sels = vec![Sel::at(1)];
        e.insert_text(&mut b, "x", Lang::C, 0.0);
        e.insert_text(&mut b, "y", Lang::C, 0.1);
        assert_eq!(b.text, "axyb");
        assert!(e.dirty());
        e.undo(&mut b);
        assert_eq!(b.text, "ab");
        assert!(!e.dirty());
        e.redo(&mut b);
        assert_eq!(b.text, "axyb");
    }

    #[test]
    fn auto_close_and_skip() {
        let (mut e, mut b) = ed("");
        e.insert_text(&mut b, "(", Lang::C, 0.0);
        assert_eq!(b.text, "()");
        e.insert_text(&mut b, ")", Lang::C, 0.1);
        assert_eq!(b.text, "()");
        assert_eq!(e.head(), 2);
    }

    #[test]
    fn enter_between_braces() {
        let (mut e, mut b) = ed("void f() {}");
        e.sels = vec![Sel::at(10)];
        e.newline(&mut b, 0.0);
        assert_eq!(b.text, "void f() {\n    \n}");
        assert_eq!(e.head(), 15);
    }

    #[test]
    fn multi_cursor_typing() {
        let (mut e, mut b) = ed("a\nb\nc");
        e.sels = vec![Sel::at(1), Sel::at(3), Sel::at(5)];
        e.insert_text(&mut b, ";", Lang::Plain, 0.0);
        assert_eq!(b.text, "a;\nb;\nc;");
        e.backspace(&mut b, false, 0.1);
        assert_eq!(b.text, "a\nb\nc");
    }

    #[test]
    fn comment_toggle() {
        let (mut e, mut b) = ed("  int a;\n  int b;");
        e.sels = vec![Sel::new(0, b.text.len())];
        e.toggle_comment(&mut b, Lang::C, 0.0);
        assert_eq!(b.text, "  // int a;\n  // int b;");
        e.toggle_comment(&mut b, Lang::C, 1.0);
        assert_eq!(b.text, "  int a;\n  int b;");
    }

    #[test]
    fn move_and_copy_lines() {
        let (mut e, mut b) = ed("one\ntwo\nthree");
        e.sels = vec![Sel::at(5)];
        e.move_lines(&mut b, true, false, 0.0);
        assert_eq!(b.text, "two\none\nthree");
        assert_eq!(b.line_of(e.head()), 0);
        e.move_lines(&mut b, false, true, 1.0);
        assert_eq!(b.text, "two\ntwo\none\nthree");
        assert_eq!(b.line_of(e.head()), 1);
    }

    #[test]
    fn indent_outdent() {
        let (mut e, mut b) = ed("a\nb");
        e.sels = vec![Sel::new(0, 3)];
        e.indent_lines(&mut b, true, 0.0);
        assert_eq!(b.text, "    a\n    b");
        e.indent_lines(&mut b, false, 1.0);
        assert_eq!(b.text, "a\nb");
    }

    #[test]
    fn next_occurrence() {
        let (mut e, b) = ed("foo bar foo foo");
        e.sels = vec![Sel::at(1)];
        e.add_next_occurrence(&b);
        assert_eq!(e.sels, vec![Sel::new(0, 3)]);
        e.add_next_occurrence(&b);
        assert_eq!(e.sels.len(), 2);
        e.select_all_occurrences(&b);
        assert_eq!(e.sels.len(), 3);
    }

    #[test]
    fn folds_and_bookmarks_shift() {
        let (mut e, mut b) = ed("int f() {\n  a;\n  b;\n}\nx");
        e.refresh_cache(&b, Lang::C, false);
        assert!(e.fold_at(0).is_some());
        e.bookmarks.insert(4);
        e.sels = vec![Sel::at(0)];
        e.insert_line(&mut b, true, 0.0);
        assert!(e.bookmarks.contains(&5));
    }

    #[test]
    fn if0_and_block_comment() {
        let (mut e, mut b) = ed("a();\nb();\nc();");
        e.select(5, 5);
        e.toggle_if0(&mut b, 0.0);
        assert_eq!(b.text, "a();\n#if 0\nb();\n#endif\nc();");
        // 选中整块再按一次：去掉
        e.toggle_if0(&mut b, 1.0);
        assert_eq!(b.text, "a();\nb();\nc();");
        // 光标在块里也能去掉
        e.select(5, 5);
        e.toggle_if0(&mut b, 2.0);
        e.select(12, 12);
        e.toggle_if0(&mut b, 3.0);
        assert_eq!(b.text, "a();\nb();\nc();");
        e.select(0, 0);
        e.toggle_block_comment(&mut b, Lang::C, 4.0);
        assert_eq!(b.text, "/* a(); */\nb();\nc();");
        e.select(3, 3);
        e.toggle_block_comment(&mut b, Lang::C, 5.0);
        assert_eq!(b.text, "a();\nb();\nc();");
        e.select(0, 0);
        e.map_words(&mut b, crate::codegen::cycle_case, 6.0);
        assert_eq!(b.text, "A();\nb();\nc();");
        let n = e.delete_marked_lines(&mut b, "b()", 7.0);
        assert_eq!((n, b.text.as_str()), (1, "A();\nc();"));
    }

    #[test]
    fn columns() {
        assert_eq!(vcol("\tab", 1), 4);
        assert_eq!(vcol("中a", 3), 2);
        let m = Metrics::default();
        assert_eq!(byte_at_x("中a", 2.2, m), 3);
        assert_eq!(byte_at_x("中a", 0.6, m), 0);
        assert_eq!(xpos("\tab", 2, m), 5.0);
    }
}

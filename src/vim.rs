//! 可选的 Vim 键位（设置里打开）：常用的普通模式移动和操作、可视模式、文本对象、计数、`.` 重复、
//! `:w` `:q` `:行号` 和 `/` 搜索（交给外层的查找栏）。不追求完整，覆盖日常编辑够用的部分。

use crate::editor::{Buf, Editor, Out};
use crate::highlight::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VKey {
    Char(char),
    Esc,
    Enter,
    Back,
    Del,
    Ctrl(char),
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
}

pub struct Vim {
    pub mode: Mode,
    /// 还没凑成完整命令的按键
    pending: Vec<VKey>,
    /// 无名寄存器：(内容, 是否整行)
    reg: (String, bool),
    last_find: Option<(char, bool, bool)>,
    /// 上下移动时想保持的列（字符数）和上次上下移动停在的位置；光标被别的操作挪走了就重新算
    goal: Option<(usize, usize)>,
    /// 可视模式的锚点和光标
    anchor: usize,
    /// 命令行（: 开头）
    pub cmdline: Option<String>,
    /// 上一帧鼠标在拖选：拖完留下的选区算可视模式，别的来源（查找、跳转）的选区只留光标
    pub was_drag: bool,
    /// 最近一次修改的按键（`.` 重放用）；recording = 正在记录（插入模式里打的字也算）
    last_change: Vec<VKey>,
    recording: Option<Vec<VKey>>,
    replaying: bool,
}

impl Default for Vim {
    fn default() -> Self {
        Vim {
            mode: Mode::Normal,
            pending: vec![],
            reg: (String::new(), false),
            last_find: None,
            goal: None,
            anchor: 0,
            cmdline: None,
            was_drag: false,
            last_change: vec![],
            recording: None,
            replaying: false,
        }
    }
}

enum Parse {
    More,
    Bad,
    Done,
}

fn cls(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn first_nonblank(buf: &Buf, l: usize) -> usize {
    let s = buf.start(l);
    s + buf.line(l).len() - buf.line(l).trim_start().len()
}

/// 普通模式下光标不能停在行尾（空行除外）
fn clamp_normal(buf: &Buf, b: usize) -> usize {
    let l = buf.line_of(b);
    let (s, e) = (buf.start(l), buf.end(l));
    if b >= e && e > s {
        buf.prev(e)
    } else {
        b.min(e)
    }
}

fn col_of(buf: &Buf, b: usize) -> usize {
    let s = buf.start(buf.line_of(b));
    buf.text[s..b].chars().count()
}

fn at_col(buf: &Buf, l: usize, col: usize) -> usize {
    let s = buf.start(l);
    let line = buf.line(l);
    s + line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

impl Vim {
    /// 鼠标拖选后进入可视模式
    pub fn enter_visual(&mut self, _n: usize, anchor: usize) {
        self.mode = Mode::Visual;
        self.anchor = anchor;
        self.pending.clear();
    }

    pub fn status(&self) -> String {
        if let Some(c) = &self.cmdline {
            return format!(":{c}");
        }
        let m = match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        };
        let p: String = self
            .pending
            .iter()
            .map(|k| match k {
                VKey::Char(c) => c.to_string(),
                VKey::Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
                _ => String::new(),
            })
            .collect();
        if p.is_empty() {
            m.to_string()
        } else {
            format!("{m}  {p}")
        }
    }

    /// 插入模式里的按键：只关心 Esc（回普通模式）和记录给 `.` 用；返回 true 表示已处理
    pub fn insert_key(&mut self, ed: &mut Editor, buf: &Buf, k: &VKey) -> bool {
        if let Some(r) = self.recording.as_mut() {
            r.push(k.clone());
        }
        if *k == VKey::Esc {
            self.mode = Mode::Normal;
            if let Some(r) = self.recording.take() {
                self.last_change = r;
            }
            let h = ed.head();
            let s = buf.start(buf.line_of(h));
            let b = if h > s { buf.prev(h) } else { h };
            ed.select(b, b);
            return true;
        }
        false
    }

    /// 普通 / 可视模式的一个按键
    pub fn key(&mut self, ed: &mut Editor, buf: &mut Buf, k: VKey, lang: Lang, now: f64, out: &mut Out) {
        if let Some(c) = self.cmdline.as_mut() {
            match k {
                VKey::Esc => self.cmdline = None,
                VKey::Back => {
                    if c.pop().is_none() {
                        self.cmdline = None;
                    }
                }
                VKey::Enter => {
                    out.vim_cmd = Some(format!(":{}", c.trim()));
                    self.cmdline = None;
                }
                VKey::Char(ch) => c.push(ch),
                _ => {}
            }
            return;
        }
        let k = match k {
            VKey::Del => VKey::Char('x'),
            VKey::Tab => return,
            k => k,
        };
        if k == VKey::Esc {
            self.pending.clear();
            if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                self.mode = Mode::Normal;
                let h = clamp_normal(buf, ed.head().min(buf.text.len()));
                ed.select(h, h);
                self.cursor_from_visual(ed, buf);
            }
            return;
        }
        self.pending.push(k);
        let keys = self.pending.clone();
        let before = buf.ver;
        let entered_insert;
        match self.run(ed, buf, &keys, lang, now, out) {
            Parse::More => return,
            Parse::Bad => {
                self.pending.clear();
                return;
            }
            Parse::Done => {
                self.pending.clear();
                entered_insert = self.mode == Mode::Insert;
            }
        }
        // 记录可以用 `.` 重复的修改
        if !self.replaying && keys.first() != Some(&VKey::Char('.')) && keys.first() != Some(&VKey::Char('u')) {
            if entered_insert {
                self.recording = Some(keys);
            } else if buf.ver != before && !matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                self.last_change = keys;
            }
        }
    }

    fn cursor_from_visual(&mut self, _ed: &mut Editor, _buf: &Buf) {
        self.goal = None;
    }

    /// 可视模式下把 (anchor, head) 画成编辑器的选区（包含光标下的字符）
    fn show_visual(&self, ed: &mut Editor, buf: &Buf, head: usize) {
        let a = self.anchor;
        match self.mode {
            Mode::VisualLine => {
                let (l1, l2) = (buf.line_of(a.min(head)), buf.line_of(a.max(head)));
                let (s, e) = (buf.start(l1), buf.end(l2));
                if head < a {
                    ed.select(e, s);
                } else {
                    ed.select(s, e);
                }
            }
            _ => {
                if head >= a {
                    ed.select(a, buf.next(head));
                } else {
                    ed.select(buf.next(a), head);
                }
            }
        }
    }

    fn visual_head(&self, ed: &Editor, buf: &Buf) -> usize {
        let s = ed.primary_sel();
        match self.mode {
            Mode::VisualLine => {
                if s.h < s.a {
                    s.h
                } else {
                    // 行选择时 head 在行尾，取回行首方便上下移动
                    let l = buf.line_of(s.h);
                    first_nonblank(buf, l).min(buf.end(l))
                }
            }
            _ => {
                if s.h > s.a {
                    buf.prev(s.h)
                } else {
                    s.h
                }
            }
        }
    }

    // ------------------------------------------------------------ 解析与执行

    fn run(&mut self, ed: &mut Editor, buf: &mut Buf, keys: &[VKey], lang: Lang, now: f64, out: &mut Out) -> Parse {
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        let mut i = 0;
        let mut count = 0usize;
        while let Some(VKey::Char(c)) = keys.get(i) {
            if c.is_ascii_digit() && (*c != '0' || count > 0) {
                count = count * 10 + c.to_digit(10).unwrap() as usize;
                i += 1;
            } else {
                break;
            }
        }
        let Some(k) = keys.get(i) else { return Parse::More };
        let n = count.max(1);
        let head = if visual { self.visual_head(ed, buf) } else { ed.head() };
        let cur_line = buf.line_of(head);

        // 可视模式里的操作
        if visual {
            if let VKey::Char(op @ ('d' | 'x' | 'c' | 'y' | '>' | '<' | '~' | 'J' | 's' | 'p')) = k {
                let line = self.mode == Mode::VisualLine;
                let (a, b) = (self.anchor.min(head), self.anchor.max(head));
                let (s, e) = if line { (buf.start(buf.line_of(a)), buf.end(buf.line_of(b))) } else { (a, buf.next(b)) };
                self.mode = Mode::Normal;
                ed.select(s, s);
                match op {
                    'd' | 'x' => self.delete(ed, buf, s, e, line, now),
                    'c' | 's' => {
                        self.change(ed, buf, s, e, line, now);
                    }
                    'y' => {
                        self.yank(buf, s, e, line, out);
                        ed.select(s, s);
                    }
                    '>' | '<' => self.indent(ed, buf, buf.line_of(s), buf.line_of(e), *op == '>', now),
                    '~' => {
                        let t: String = buf.text[s..e]
                            .chars()
                            .map(|c| if c.is_uppercase() { c.to_lowercase().next().unwrap() } else { c.to_uppercase().next().unwrap() })
                            .collect();
                        ed.replace_ranges(buf, vec![(s..e, t)], now);
                        ed.select(s, s);
                    }
                    'J' => self.join(ed, buf, buf.line_of(s), (buf.line_of(e) - buf.line_of(s)).max(1), now),
                    'p' => {
                        let (t, lw) = self.reg.clone();
                        let t = if lw && !line { format!("\n{}", t.trim_end_matches('\n')) } else { t };
                        ed.replace_ranges(buf, vec![(s..e, t)], now);
                        let h = clamp_normal(buf, ed.head().saturating_sub(1));
                        ed.select(h, h);
                    }
                    _ => {}
                }
                return Parse::Done;
            }
            if let VKey::Char('o') = k {
                let a = self.anchor;
                self.anchor = head;
                self.show_visual(ed, buf, a);
                return Parse::Done;
            }
            if let VKey::Char('v') = k {
                if self.mode == Mode::Visual {
                    self.mode = Mode::Normal;
                    ed.select(head, head);
                } else {
                    self.mode = Mode::Visual;
                    self.show_visual(ed, buf, head);
                }
                return Parse::Done;
            }
            if let VKey::Char('V') = k {
                if self.mode == Mode::VisualLine {
                    self.mode = Mode::Normal;
                    ed.select(head, head);
                } else {
                    self.mode = Mode::VisualLine;
                    self.show_visual(ed, buf, head);
                }
                return Parse::Done;
            }
            if let VKey::Char(c @ ('i' | 'a')) = k {
                // 可视模式的文本对象：viw、vi( …
                let Some(VKey::Char(o)) = keys.get(i + 1) else { return Parse::More };
                if let Some((s, e)) = text_object(buf, head, *o, *c == 'i') {
                    self.anchor = s;
                    self.show_visual(ed, buf, buf.prev(e).max(s));
                    return Parse::Done;
                }
                return Parse::Bad;
            }
        }

        // 不带移动的命令
        if let VKey::Char(c) = k {
            match c {
                'i' | 'a' | 'I' | 'A' if !visual => {
                    let b = match c {
                        'i' => head,
                        'a' => {
                            if head < buf.end(cur_line) {
                                buf.next(head)
                            } else {
                                head
                            }
                        }
                        'I' => first_nonblank(buf, cur_line),
                        _ => buf.end(cur_line),
                    };
                    ed.select(b, b);
                    self.mode = Mode::Insert;
                    return Parse::Done;
                }
                'o' | 'O' if !visual => {
                    ed.select(head, head);
                    ed.insert_line(buf, *c == 'O', now);
                    self.mode = Mode::Insert;
                    return Parse::Done;
                }
                'v' | 'V' if !visual => {
                    self.mode = if *c == 'v' { Mode::Visual } else { Mode::VisualLine };
                    self.anchor = head;
                    self.show_visual(ed, buf, head);
                    return Parse::Done;
                }
                'x' | 'X' if !visual => {
                    let (s, e) = (buf.start(cur_line), buf.end(cur_line));
                    let (a, b) = if *c == 'x' {
                        let mut b = head;
                        for _ in 0..n {
                            if b < e {
                                b = buf.next(b);
                            }
                        }
                        (head, b)
                    } else {
                        let mut a = head;
                        for _ in 0..n {
                            if a > s {
                                a = buf.prev(a);
                            }
                        }
                        (a, head)
                    };
                    if a < b {
                        self.delete(ed, buf, a, b, false, now);
                    }
                    return Parse::Done;
                }
                's' if !visual => {
                    let e = buf.end(cur_line);
                    let mut b = head;
                    for _ in 0..n {
                        if b < e {
                            b = buf.next(b);
                        }
                    }
                    self.change(ed, buf, head, b, false, now);
                    return Parse::Done;
                }
                'S' if !visual => {
                    let l2 = (cur_line + n - 1).min(buf.lines() - 1);
                    self.change(ed, buf, buf.start(cur_line), buf.end(l2), true, now);
                    return Parse::Done;
                }
                'D' | 'C' | 'Y' if !visual => {
                    let l2 = (cur_line + n - 1).min(buf.lines() - 1);
                    match c {
                        'D' => self.delete(ed, buf, head, buf.end(l2), false, now),
                        'C' => self.change(ed, buf, head, buf.end(l2), false, now),
                        _ => self.yank(buf, buf.start(cur_line), buf.end(l2), true, out),
                    }
                    return Parse::Done;
                }
                'p' | 'P' if !visual => {
                    self.put(ed, buf, head, *c == 'p', n, now);
                    return Parse::Done;
                }
                'J' if !visual => {
                    self.join(ed, buf, cur_line, n.max(2) - 1, now);
                    return Parse::Done;
                }
                '~' if !visual => {
                    let e = buf.end(cur_line);
                    let mut b = head;
                    for _ in 0..n {
                        if b < e {
                            b = buf.next(b);
                        }
                    }
                    if head < b {
                        let t: String = buf.text[head..b]
                            .chars()
                            .map(|c| if c.is_uppercase() { c.to_lowercase().next().unwrap() } else { c.to_uppercase().next().unwrap() })
                            .collect();
                        ed.replace_ranges(buf, vec![(head..b, t)], now);
                        let h = clamp_normal(buf, b);
                        ed.select(h, h);
                    }
                    return Parse::Done;
                }
                'r' if !visual => {
                    let Some(VKey::Char(ch)) = keys.get(i + 1) else {
                        return if keys.len() > i + 1 { Parse::Bad } else { Parse::More };
                    };
                    let e = buf.end(cur_line);
                    let mut b = head;
                    for _ in 0..n {
                        if b < e {
                            b = buf.next(b);
                        }
                    }
                    if head < b {
                        let cnt = buf.text[head..b].chars().count();
                        ed.replace_ranges(buf, vec![(head..b, ch.to_string().repeat(cnt))], now);
                        let h = clamp_normal(buf, b.saturating_sub(ch.len_utf8()).max(head));
                        let h = buf.prev(buf.next(h).min(buf.text.len()));
                        ed.select(h, h);
                    }
                    return Parse::Done;
                }
                'u' if !visual => {
                    for _ in 0..n {
                        ed.undo(buf);
                    }
                    let h = clamp_normal(buf, ed.primary_sel().min());
                    ed.select(h, h);
                    return Parse::Done;
                }
                '.' if !visual => {
                    let keys = self.last_change.clone();
                    if keys.is_empty() {
                        return Parse::Done;
                    }
                    self.replaying = true;
                    for _ in 0..n {
                        self.replay(ed, buf, &keys, lang, now, out);
                    }
                    self.replaying = false;
                    return Parse::Done;
                }
                ':' => {
                    self.cmdline = Some(String::new());
                    return Parse::Done;
                }
                '/' | '?' => {
                    out.vim_cmd = Some(c.to_string());
                    return Parse::Done;
                }
                'n' | 'N' => {
                    out.vim_cmd = Some(c.to_string());
                    return Parse::Done;
                }
                'z' => {
                    // zz / zt / zb：把光标行放到中间（交给编辑器的“显示光标”）
                    if keys.len() == i + 1 {
                        return Parse::More;
                    }
                    ed.reveal_cursor();
                    return Parse::Done;
                }
                _ => {}
            }
        }
        if let VKey::Ctrl('r') = k {
            for _ in 0..n {
                ed.redo(buf);
            }
            let h = clamp_normal(buf, ed.primary_sel().min());
            ed.select(h, h);
            return Parse::Done;
        }

        // 操作符 + 移动 / 文本对象
        let op = match k {
            VKey::Char(c @ ('d' | 'c' | 'y' | '>' | '<')) if !visual => Some(*c),
            _ => None,
        };
        let mut j = i + if op.is_some() { 1 } else { 0 };
        let mut count2 = 0usize;
        if op.is_some() {
            while let Some(VKey::Char(c)) = keys.get(j) {
                if c.is_ascii_digit() && (*c != '0' || count2 > 0) {
                    count2 = count2 * 10 + c.to_digit(10).unwrap() as usize;
                    j += 1;
                } else {
                    break;
                }
            }
        }
        let n = n * count2.max(1);
        let Some(mk) = keys.get(j) else { return Parse::More };
        if let Some(op) = op {
            // dd / cc / yy / >> / <<
            if *mk == VKey::Char(op) {
                let l2 = (cur_line + n - 1).min(buf.lines() - 1);
                match op {
                    'd' => self.delete(ed, buf, buf.start(cur_line), buf.end(l2), true, now),
                    'c' => self.change(ed, buf, buf.start(cur_line), buf.end(l2), true, now),
                    'y' => {
                        self.yank(buf, buf.start(cur_line), buf.end(l2), true, out);
                        ed.select(head, head);
                    }
                    _ => self.indent(ed, buf, cur_line, l2, op == '>', now),
                }
                return Parse::Done;
            }
            // 文本对象 iw / a( …
            if let VKey::Char(c @ ('i' | 'a')) = mk {
                let Some(VKey::Char(o)) = keys.get(j + 1) else { return Parse::More };
                let Some((s, e)) = text_object(buf, head, *o, *c == 'i') else { return Parse::Bad };
                self.operate(ed, buf, op, s, e, false, now, out);
                return Parse::Done;
            }
        }
        let mut stop_at_eol = false;
        if op.is_some() && matches!(mk, VKey::Char('w' | 'W')) {
            stop_at_eol = true;
        }
        let mk = if op == Some('c') {
            // cw 的行为和 ce 一样
            match mk {
                VKey::Char('w') if buf.char_at(head).is_some_and(|c| !c.is_whitespace()) => &VKey::Char('e'),
                VKey::Char('W') if buf.char_at(head).is_some_and(|c| !c.is_whitespace()) => &VKey::Char('E'),
                m => m,
            }
        } else {
            mk
        };
        let rest = &keys[j + 1..];
        let mv = match self.motion(ed, buf, head, mk, rest, count, n) {
            Some(Ok(m)) => m,
            Some(Err(())) => return Parse::Bad,
            None => return Parse::More,
        };
        let (mut to, linewise, inclusive) = mv;
        match op {
            None => {
                let to = if visual { to.min(buf.text.len()) } else { clamp_normal(buf, to) };
                if visual {
                    self.show_visual(ed, buf, to);
                } else {
                    ed.select(to, to);
                }
                ed.reveal_cursor();
            }
            Some(op) => {
                if stop_at_eol && buf.line_of(to) > cur_line {
                    // dw 在行尾最后一个词上：只删到行尾
                    to = buf.end(cur_line);
                }
                let (mut s, mut e) = (head.min(to), head.max(to));
                if inclusive {
                    e = buf.next(e);
                }
                if linewise {
                    s = buf.start(buf.line_of(s));
                    e = buf.end(buf.line_of(e));
                }
                self.operate(ed, buf, op, s, e, linewise, now, out);
            }
        }
        Parse::Done
    }

    fn replay(&mut self, ed: &mut Editor, buf: &mut Buf, keys: &[VKey], lang: Lang, now: f64, out: &mut Out) {
        let mut k = 0;
        // 先执行命令部分
        self.pending.clear();
        while k < keys.len() && self.mode != Mode::Insert {
            self.pending.push(keys[k].clone());
            let p = self.pending.clone();
            k += 1;
            match self.run(ed, buf, &p, lang, now, out) {
                Parse::More => continue,
                _ => self.pending.clear(),
            }
        }
        // 再把插入模式里打的字重放一遍
        for key in &keys[k..] {
            match key {
                VKey::Char(c) => ed.insert_text(buf, &c.to_string(), lang, now),
                VKey::Enter => ed.newline(buf, now),
                VKey::Back => ed.backspace(buf, false, now),
                VKey::Esc => {
                    self.insert_key(ed, buf, &VKey::Esc);
                    break;
                }
                _ => {}
            }
        }
    }

    /// 返回 Some(Ok((目标, 是否整行, 是否包含目标字符)))；None = 还要等按键；Some(Err) = 不认识
    #[allow(clippy::too_many_arguments)]
    fn motion(&mut self, ed: &Editor, buf: &Buf, head: usize, k: &VKey, rest: &[VKey], count: usize, n: usize) -> Option<Result<(usize, bool, bool), ()>> {
        let line = buf.line_of(head);
        let last = buf.lines() - 1;
        let vert = |me: &mut Self, l: usize| -> usize {
            let g = match me.goal {
                Some((g, at)) if at == head => g,
                _ => col_of(buf, head),
            };
            let to = at_col(buf, l, g);
            let shown = clamp_normal(buf, to);
            me.goal = Some((g, shown));
            to
        };
        let r = match k {
            VKey::Char('h') | VKey::Left | VKey::Back => {
                let s = buf.start(line);
                let mut b = head;
                for _ in 0..n {
                    if b > s {
                        b = buf.prev(b);
                    }
                }
                (b, false, false)
            }
            VKey::Char('l') | VKey::Right | VKey::Char(' ') => {
                let e = buf.end(line);
                let mut b = head;
                for _ in 0..n {
                    if b < e {
                        b = buf.next(b);
                    }
                }
                (b, false, false)
            }
            VKey::Char('j') | VKey::Down | VKey::Enter => {
                let l = (line + n).min(last);
                (vert(self, l), true, false)
            }
            VKey::Char('k') | VKey::Up => {
                let l = line.saturating_sub(n);
                (vert(self, l), true, false)
            }
            VKey::Ctrl('d') | VKey::Ctrl('u') => {
                let half = (ed.page_rows / 2).max(1);
                let l = if *k == VKey::Ctrl('d') { (line + half).min(last) } else { line.saturating_sub(half) };
                (vert(self, l), true, false)
            }
            VKey::Char('0') | VKey::Home => (buf.start(line), false, false),
            VKey::Char('^') => (first_nonblank(buf, line), false, false),
            VKey::Char('$') | VKey::End => {
                let l = (line + n - 1).min(last);
                let e = buf.end(l);
                (if e > buf.start(l) { buf.prev(e) } else { e }, false, true)
            }
            VKey::Char('G') => {
                let l = if count > 0 { (count - 1).min(last) } else { last };
                (first_nonblank(buf, l), true, false)
            }
            VKey::Char('g') => match rest.first() {
                None => return None,
                Some(VKey::Char('g')) => {
                    let l = if count > 0 { (count - 1).min(last) } else { 0 };
                    (first_nonblank(buf, l), true, false)
                }
                Some(VKey::Char('_')) => {
                    let e = buf.end(line);
                    let t = buf.line(line).trim_end().len();
                    ((buf.start(line) + t).min(e).saturating_sub(1).max(buf.start(line)), false, true)
                }
                _ => return Some(Err(())),
            },
            VKey::Char(c @ ('w' | 'W')) => {
                let mut b = head;
                for _ in 0..n {
                    b = word_fwd(buf, b, *c == 'W');
                }
                (b, false, false)
            }
            VKey::Char(c @ ('b' | 'B')) => {
                let mut b = head;
                for _ in 0..n {
                    b = word_back(buf, b, *c == 'B');
                }
                (b, false, false)
            }
            VKey::Char(c @ ('e' | 'E')) => {
                let mut b = head;
                for _ in 0..n {
                    b = word_end(buf, b, *c == 'E');
                }
                (b, false, true)
            }
            VKey::Char(c @ ('f' | 'F' | 't' | 'T')) => {
                let Some(t) = rest.first() else { return None };
                let VKey::Char(ch) = t else { return Some(Err(())) };
                let (fwd, till) = (c.is_lowercase(), *c == 't' || *c == 'T');
                self.last_find = Some((*ch, fwd, till));
                match find_char(buf, head, *ch, fwd, till, n) {
                    Some(b) => (b, false, fwd),
                    None => return Some(Err(())),
                }
            }
            VKey::Char(c @ (';' | ',')) => {
                let Some((ch, fwd, till)) = self.last_find else { return Some(Err(())) };
                let fwd = if *c == ',' { !fwd } else { fwd };
                // 重复 t/T 时先跳过紧挨着的那个字符
                let start = if till {
                    if fwd {
                        buf.next(head)
                    } else {
                        buf.prev(head)
                    }
                } else {
                    head
                };
                match find_char(buf, start, ch, fwd, till, n) {
                    Some(b) => (b, false, fwd),
                    None => return Some(Err(())),
                }
            }
            VKey::Char('%') => match match_bracket(buf, head) {
                Some(b) => (b, false, true),
                None => return Some(Err(())),
            },
            VKey::Char(c @ ('{' | '}')) => {
                let mut l = line;
                for _ in 0..n {
                    if *c == '}' {
                        l += 1;
                        while l < last && !buf.line(l).trim().is_empty() {
                            l += 1;
                        }
                        l = l.min(last);
                    } else {
                        l = l.saturating_sub(1);
                        while l > 0 && !buf.line(l).trim().is_empty() {
                            l -= 1;
                        }
                    }
                }
                (buf.start(l), false, false)
            }
            _ => return Some(Err(())),
        };
        Some(Ok(r))
    }

    // ------------------------------------------------------------ 操作

    #[allow(clippy::too_many_arguments)]
    fn operate(&mut self, ed: &mut Editor, buf: &mut Buf, op: char, s: usize, e: usize, linewise: bool, now: f64, out: &mut Out) {
        match op {
            'd' => self.delete(ed, buf, s, e, linewise, now),
            'c' => self.change(ed, buf, s, e, linewise, now),
            'y' => {
                self.yank(buf, s, e, linewise, out);
                ed.select(s, s);
            }
            '>' | '<' => self.indent(ed, buf, buf.line_of(s), buf.line_of(e), op == '>', now),
            _ => {}
        }
    }

    fn yank(&mut self, buf: &Buf, s: usize, e: usize, linewise: bool, out: &mut Out) {
        let mut t = buf.text[s..e].to_string();
        if linewise {
            t.push('\n');
        }
        out.copy = Some(t.clone());
        self.reg = (t, linewise);
    }

    fn delete(&mut self, ed: &mut Editor, buf: &mut Buf, s: usize, e: usize, linewise: bool, now: f64) {
        let mut t = buf.text[s..e].to_string();
        let (mut rs, mut re) = (s, e);
        if linewise {
            t.push('\n');
            // 连同换行一起删；最后一行就删掉前面的换行
            if re < buf.text.len() {
                re += 1;
            } else if rs > 0 {
                rs -= 1;
            }
        }
        self.reg = (t, linewise);
        ed.replace_ranges(buf, vec![(rs..re, String::new())], now);
        let b = if linewise {
            let l = buf.line_of(rs.min(buf.text.len()));
            let l = if rs > 0 && re >= buf.text.len() { buf.line_of(rs) } else { l };
            first_nonblank(buf, l)
        } else {
            clamp_normal(buf, s.min(buf.text.len()))
        };
        ed.select(b, b);
    }

    fn change(&mut self, ed: &mut Editor, buf: &mut Buf, s: usize, e: usize, linewise: bool, now: f64) {
        let (s2, ind) = if linewise {
            // cc 保留缩进
            let l = buf.line_of(s);
            let fnb = first_nonblank(buf, l);
            (fnb.min(e), String::new())
        } else {
            (s, String::new())
        };
        let mut t = buf.text[s..e].to_string();
        if linewise {
            t.push('\n');
        }
        self.reg = (t, linewise);
        ed.replace_ranges(buf, vec![(s2..e, ind)], now);
        ed.select(s2, s2);
        self.mode = Mode::Insert;
    }

    fn put(&mut self, ed: &mut Editor, buf: &mut Buf, head: usize, after: bool, n: usize, now: f64) {
        let (t, linewise) = self.reg.clone();
        if t.is_empty() {
            return;
        }
        let t = t.repeat(n);
        if linewise {
            let l = buf.line_of(head);
            let (at, ins) = if after {
                let e = buf.end(l);
                (e, format!("\n{}", t.trim_end_matches('\n')))
            } else {
                (buf.start(l), t.clone())
            };
            ed.replace_ranges(buf, vec![(at..at, ins)], now);
            let nl = if after { l + 1 } else { l };
            let b = first_nonblank(buf, nl.min(buf.lines() - 1));
            ed.select(b, b);
        } else {
            let at = if after && head < buf.end(buf.line_of(head)) { buf.next(head) } else { head };
            ed.replace_ranges(buf, vec![(at..at, t.clone())], now);
            let b = clamp_normal(buf, (at + t.len()).saturating_sub(1));
            let b = if buf.text.is_char_boundary(b) { b } else { buf.prev(b) };
            ed.select(b, b);
        }
    }

    fn join(&mut self, ed: &mut Editor, buf: &mut Buf, l: usize, times: usize, now: f64) {
        let mut items = vec![];
        let mut cur = 0;
        for k in 0..times {
            let a = l + k;
            if a + 1 >= buf.lines() {
                break;
            }
            let e = buf.end(a);
            let ns = buf.start(a + 1);
            let next = buf.line(a + 1);
            let skip = next.len() - next.trim_start().len();
            let left_trim = buf.line(a).len() - buf.line(a).trim_end().len();
            let sep = if next.trim().is_empty() || next.trim_start().starts_with(')') { "" } else { " " };
            items.push((e - left_trim..ns + skip, sep.to_string()));
            cur = e - left_trim;
        }
        if items.is_empty() {
            return;
        }
        ed.replace_ranges(buf, items, now);
        let b = clamp_normal(buf, cur.min(buf.text.len()));
        ed.select(b, b);
    }

    fn indent(&mut self, ed: &mut Editor, buf: &mut Buf, l1: usize, l2: usize, inc: bool, now: f64) {
        ed.select(buf.start(l1), buf.end(l2));
        ed.indent_lines(buf, inc, now);
        let b = first_nonblank(buf, l1);
        ed.select(b, b);
    }
}

// ---------------------------------------------------------------- 移动

fn word_fwd(buf: &Buf, b: usize, big: bool) -> usize {
    let t = &buf.text;
    let n = t.len();
    if b >= n {
        return n;
    }
    let mut p = b;
    let c0 = cls(buf.char_at(p).unwrap(), big);
    if c0 != 0 {
        while p < n && buf.char_at(p).is_some_and(|c| cls(c, big) == c0) {
            p = buf.next(p);
        }
    }
    // 跳过空白，但遇到空行停下
    while p < n {
        let c = buf.char_at(p).unwrap();
        if c == '\n' {
            p += 1;
            if buf.char_at(p) == Some('\n') {
                return p;
            }
            continue;
        }
        if !c.is_whitespace() {
            break;
        }
        p = buf.next(p);
    }
    p
}

fn word_end(buf: &Buf, b: usize, big: bool) -> usize {
    let n = buf.text.len();
    let mut p = buf.next(b);
    while p < n && buf.char_at(p).is_some_and(|c| c.is_whitespace()) {
        p = buf.next(p);
    }
    if p >= n {
        return buf.prev(n).max(b);
    }
    let c0 = cls(buf.char_at(p).unwrap(), big);
    loop {
        let q = buf.next(p);
        if q >= n || buf.char_at(q).is_none_or(|c| cls(c, big) != c0) {
            return p;
        }
        p = q;
    }
}

fn word_back(buf: &Buf, b: usize, big: bool) -> usize {
    if b == 0 {
        return 0;
    }
    let mut p = buf.prev(b);
    while p > 0 && buf.char_at(p).is_some_and(|c| c.is_whitespace()) {
        if buf.char_at(p) == Some('\n') && buf.char_at(buf.prev(p)) == Some('\n') {
            return p;
        }
        p = buf.prev(p);
    }
    let c0 = cls(buf.char_at(p).unwrap_or(' '), big);
    while p > 0 {
        let q = buf.prev(p);
        if buf.char_at(q).is_none_or(|c| cls(c, big) != c0) {
            break;
        }
        p = q;
    }
    p
}

fn find_char(buf: &Buf, head: usize, ch: char, fwd: bool, till: bool, n: usize) -> Option<usize> {
    let l = buf.line_of(head);
    let (s, e) = (buf.start(l), buf.end(l));
    let mut p = head;
    for _ in 0..n {
        if fwd {
            let from = buf.next(p).min(e);
            let off = buf.text[from..e].find(ch)?;
            p = from + off;
        } else {
            let off = buf.text[s..p].rfind(ch)?;
            p = s + off;
        }
    }
    Some(if till {
        if fwd {
            buf.prev(p)
        } else {
            buf.next(p)
        }
    } else {
        p
    })
}

fn match_bracket(buf: &Buf, head: usize) -> Option<usize> {
    let l = buf.line_of(head);
    let e = buf.end(l);
    // 光标处或右边第一个括号
    let (at, c) = buf.text[head..e].char_indices().find(|(_, c)| "(){}[]".contains(*c)).map(|(i, c)| (head + i, c))?;
    let (open, close, fwd) = match c {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        _ => ('{', '}', false),
    };
    let mut depth = 0i32;
    if fwd {
        for (i, ch) in buf.text[at..].char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(at + i);
                }
            }
        }
    } else {
        for (i, ch) in buf.text[..=at].char_indices().rev() {
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    }
    None
}

/// 文本对象的范围 [s, e)
fn text_object(buf: &Buf, head: usize, o: char, inner: bool) -> Option<(usize, usize)> {
    match o {
        'w' | 'W' => {
            let big = o == 'W';
            let c0 = cls(buf.char_at(head)?, big);
            let mut s = head;
            while s > 0 && buf.char_at(buf.prev(s)).is_some_and(|c| cls(c, big) == c0 && c != '\n') {
                s = buf.prev(s);
            }
            let mut e = head;
            while e < buf.text.len() && buf.char_at(e).is_some_and(|c| cls(c, big) == c0 && c != '\n') {
                e = buf.next(e);
            }
            if !inner {
                while e < buf.text.len() && buf.char_at(e).is_some_and(|c| c == ' ' || c == '\t') {
                    e = buf.next(e);
                }
            }
            Some((s, e))
        }
        '"' | '\'' | '`' => {
            let l = buf.line_of(head);
            let (ls, le) = (buf.start(l), buf.end(l));
            let line = &buf.text[ls..le];
            let qs: Vec<usize> = line.char_indices().filter(|(_, c)| *c == o).map(|(i, _)| ls + i).collect();
            let pair = qs
                .chunks(2)
                .find(|p| p.len() == 2 && head >= p[0] && head <= p[1])
                .or_else(|| qs.chunks(2).find(|p| p.len() == 2 && p[0] >= head))?;
            if inner {
                Some((pair[0] + 1, pair[1]))
            } else {
                Some((pair[0], pair[1] + 1))
            }
        }
        '(' | ')' | 'b' | '[' | ']' | '{' | '}' | 'B' | '<' | '>' => {
            let (open, close) = match o {
                '(' | ')' | 'b' => ('(', ')'),
                '[' | ']' => ('[', ']'),
                '<' | '>' => ('<', '>'),
                _ => ('{', '}'),
            };
            // 往左找没配对的开括号
            let mut depth = 0;
            let mut s = None;
            let start_at = if buf.char_at(head) == Some(open) { buf.next(head) } else { head };
            for (i, c) in buf.text[..start_at].char_indices().rev() {
                if c == close && i != head {
                    depth += 1;
                } else if c == open {
                    if depth == 0 {
                        s = Some(i);
                        break;
                    }
                    depth -= 1;
                }
            }
            let s = s?;
            let mut depth = 0;
            let mut e = None;
            for (i, c) in buf.text[s..].char_indices() {
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        e = Some(s + i);
                        break;
                    }
                }
            }
            let e = e?;
            if inner {
                let (mut a, mut b) = (s + 1, e);
                // 多行块：不含开括号后的换行和闭括号前的缩进
                if buf.text[a..b].starts_with('\n') {
                    a += 1;
                    let l = buf.line_of(b);
                    if buf.text[buf.start(l)..b].trim().is_empty() && buf.start(l) > a {
                        b = buf.start(l) - 1;
                    }
                }
                Some((a, b.max(a)))
            } else {
                Some((s, e + 1))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    fn setup(text: &str, at: usize) -> (Vim, Editor, Buf) {
        let buf = Buf::new(text.to_string());
        let mut ed = Editor::new(egui::Id::new("vim-test"), &buf);
        ed.select(at, at);
        (Vim::default(), ed, buf)
    }

    fn keys(v: &mut Vim, ed: &mut Editor, buf: &mut Buf, s: &str) {
        let mut out = Out::default();
        for c in s.chars() {
            let k = match c {
                '\u{1b}' => VKey::Esc,
                '\n' => VKey::Enter,
                c => VKey::Char(c),
            };
            if v.mode == Mode::Insert {
                if !v.insert_key(ed, buf, &k) {
                    match k {
                        VKey::Char(c) => ed.insert_text(buf, &c.to_string(), Lang::C, 0.0),
                        VKey::Enter => ed.newline(buf, 0.0),
                        _ => {}
                    }
                }
            } else {
                v.key(ed, buf, k, Lang::C, 0.0, &mut out);
            }
        }
    }

    #[test]
    fn motions_and_ops() {
        let (mut v, mut ed, mut buf) = setup("int foo = bar(1, 2);\nsecond line\n", 0);
        keys(&mut v, &mut ed, &mut buf, "w");
        assert_eq!(ed.head(), 4);
        keys(&mut v, &mut ed, &mut buf, "dw");
        assert_eq!(buf.text, "int = bar(1, 2);\nsecond line\n");
        keys(&mut v, &mut ed, &mut buf, "f(ci(x\u{1b}");
        assert_eq!(buf.text, "int = bar(x);\nsecond line\n");
        keys(&mut v, &mut ed, &mut buf, "ddp");
        assert_eq!(buf.text, "second line\nint = bar(x);\n");
        keys(&mut v, &mut ed, &mut buf, "u");
        assert_eq!(buf.text, "second line\n");
        keys(&mut v, &mut ed, &mut buf, "ggcwfirst\u{1b}");
        assert_eq!(buf.text, "first line\n");
        keys(&mut v, &mut ed, &mut buf, "w.");
        assert_eq!(buf.text, "first first\n");
        keys(&mut v, &mut ed, &mut buf, "0yiwP");
        assert_eq!(buf.text, "firstfirst first\n");
        keys(&mut v, &mut ed, &mut buf, "Vd");
        assert_eq!(buf.text, "");
    }

    #[test]
    fn counts_and_join() {
        let (mut v, mut ed, mut buf) = setup("a\nb\nc\nd\n", 0);
        keys(&mut v, &mut ed, &mut buf, "2dd");
        assert_eq!(buf.text, "c\nd\n");
        keys(&mut v, &mut ed, &mut buf, "J");
        assert_eq!(buf.text, "c d\n");
        keys(&mut v, &mut ed, &mut buf, "A;\u{1b}x");
        assert_eq!(buf.text, "c d\n");
        keys(&mut v, &mut ed, &mut buf, "oe\u{1b}");
        assert_eq!(buf.text, "c d\ne\n");
        keys(&mut v, &mut ed, &mut buf, "gg>>");
        assert!(buf.text.starts_with("    c d") || buf.text.starts_with("\tc d"));
    }
}

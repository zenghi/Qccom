//! 终端仿真（VT100 / xterm 常用子集）：让开发板的 Linux shell、U-Boot、vi、top 能直接在窗口里用。
//!
//! 只处理串口终端真正会遇到的控制序列：光标移动、清屏/清行、颜色、滚动区、插入/删除行和字符、
//! 备用屏（vi / top 用）、应用光标键模式、光标位置查询（ESC[6n）。

use std::collections::VecDeque;

use eframe::egui::Color32;
use vte::{Params, Parser, Perform};

use crate::logbuf::ANSI16;

#[derive(Clone, Copy, PartialEq)]
pub struct Style {
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub bold: bool,
    pub inverse: bool,
    pub underline: bool,
}

impl Style {
    const DEFAULT: Style = Style {
        fg: None,
        bg: None,
        bold: false,
        inverse: false,
        underline: false,
    };
}

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
    /// 宽字符（中文）占两格，第二格是占位
    pub wide_tail: bool,
}

impl Cell {
    const BLANK: Cell = Cell {
        ch: ' ',
        style: Style::DEFAULT,
        wide_tail: false,
    };
}

pub type Row = Vec<Cell>;

/// 中日韩文字、全角符号、emoji 按两格算
pub fn is_wide(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF |
        0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 |
        0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

struct Saved {
    x: usize,
    y: usize,
    pen: Style,
}

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub grid: Vec<Row>,
    pub scrollback: VecDeque<Row>,
    pub max_scrollback: usize,
    pub x: usize,
    pub y: usize,
    pen: Style,
    top: usize,
    bottom: usize,
    wrap_pending: bool,
    autowrap: bool,
    pub app_cursor: bool,
    pub cursor_visible: bool,
    /// 备用屏（vi / top 进入时切换，退出时恢复主屏）
    alt: Option<(Vec<Row>, usize, usize)>,
    saved: Saved,
    /// G0 / G1 是否为 DEC 线条字符集，gl 为当前使用哪个
    charset: [bool; 2],
    gl: usize,
    /// 需要回给设备的数据（光标位置报告等）
    pub reply: Vec<u8>,
    /// 内容有变化（界面据此决定要不要重绘）
    pub dirty: bool,
}

pub struct Term {
    parser: Parser,
    pub s: Screen,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            parser: Parser::new(),
            s: Screen::new(cols, rows),
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            self.parser.advance(&mut self.s, b);
        }
        self.s.dirty = true;
    }

    pub fn reset(&mut self) {
        let (c, r, m) = (self.s.cols, self.s.rows, self.s.max_scrollback);
        self.parser = Parser::new();
        self.s = Screen::new(c, r);
        self.s.max_scrollback = m;
    }
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(10);
        let rows = rows.max(3);
        Self {
            cols,
            rows,
            grid: vec![vec![Cell::BLANK; cols]; rows],
            scrollback: VecDeque::new(),
            max_scrollback: 3000,
            x: 0,
            y: 0,
            pen: Style::DEFAULT,
            top: 0,
            bottom: rows - 1,
            wrap_pending: false,
            autowrap: true,
            app_cursor: false,
            cursor_visible: true,
            alt: None,
            saved: Saved { x: 0, y: 0, pen: Style::DEFAULT },
            charset: [false; 2],
            gl: 0,
            reply: vec![],
            dirty: true,
        }
    }

    /// 窗口大小变了：不重排已有内容，只截断或补空白（和大多数终端一样）
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.clamp(10, 1000);
        let rows = rows.clamp(3, 500);
        if cols == self.cols && rows == self.rows {
            return;
        }
        for r in self.grid.iter_mut() {
            r.resize(cols, Cell::BLANK);
        }
        // 行数变少时把顶部的行推进回滚区，保证光标所在行还看得见
        while self.grid.len() > rows {
            if self.y > 0 {
                let r = self.grid.remove(0);
                self.push_scrollback(r);
                self.y -= 1;
            } else {
                self.grid.pop();
            }
        }
        while self.grid.len() < rows {
            self.grid.push(vec![Cell::BLANK; cols]);
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.dirty = true;
    }

    /// 是否在全屏程序（vi / top）的备用屏里
    pub fn in_alt(&self) -> bool {
        self.alt.is_some()
    }

    fn push_scrollback(&mut self, mut row: Row) {
        if self.alt.is_some() {
            return; // 备用屏的内容不进历史
        }
        while row.last().is_some_and(|c| *c == Cell::BLANK) {
            row.pop();
        }
        row.shrink_to_fit();
        self.scrollback.push_back(row);
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
    }

    fn blank(&self) -> Cell {
        // 擦除用当前背景色（xterm 行为）
        Cell {
            ch: ' ',
            style: Style { bg: self.pen.bg, ..Style::DEFAULT },
            wide_tail: false,
        }
    }

    fn scroll_up(&mut self, n: usize) {
        for _ in 0..n {
            let row = self.grid.remove(self.top);
            if self.top == 0 {
                self.push_scrollback(row);
            }
            self.grid.insert(self.bottom, vec![self.blank(); self.cols]);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            self.grid.remove(self.bottom);
            self.grid.insert(self.top, vec![self.blank(); self.cols]);
        }
    }

    fn newline(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(1);
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
    }

    fn put_char(&mut self, c: char) {
        let w = if is_wide(c) { 2 } else { 1 };
        if self.wrap_pending || self.x + w > self.cols {
            if self.autowrap {
                self.x = 0;
                self.newline();
            } else {
                self.x = self.cols - w;
            }
            self.wrap_pending = false;
        }
        let pen = self.pen;
        let row = &mut self.grid[self.y];
        row[self.x] = Cell { ch: c, style: pen, wide_tail: false };
        if w == 2 && self.x + 1 < self.cols {
            row[self.x + 1] = Cell { ch: ' ', style: pen, wide_tail: true };
        }
        if self.x + w >= self.cols {
            self.x = self.cols - 1;
            self.wrap_pending = true;
        } else {
            self.x += w;
        }
    }

    fn erase(&mut self, y: usize, from: usize, to: usize) {
        let b = self.blank();
        let row = &mut self.grid[y];
        for c in row.iter_mut().take(to.min(self.cols)).skip(from) {
            *c = b;
        }
    }

    fn sgr(&mut self, params: &Params) {
        let ps: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        if ps.is_empty() {
            self.pen = Style::DEFAULT;
            return;
        }
        let mut i = 0;
        while i < ps.len() {
            match ps[i] {
                0 => self.pen = Style::DEFAULT,
                1 => self.pen.bold = true,
                4 => self.pen.underline = true,
                7 => self.pen.inverse = true,
                22 => self.pen.bold = false,
                24 => self.pen.underline = false,
                27 => self.pen.inverse = false,
                n @ 30..=37 => self.pen.fg = Some(ANSI16[(n - 30) as usize]),
                39 => self.pen.fg = None,
                n @ 40..=47 => self.pen.bg = Some(ANSI16[(n - 40) as usize]),
                49 => self.pen.bg = None,
                n @ 90..=97 => self.pen.fg = Some(ANSI16[(n - 90 + 8) as usize]),
                n @ 100..=107 => self.pen.bg = Some(ANSI16[(n - 100 + 8) as usize]),
                n @ (38 | 48) => {
                    let col = match ps.get(i + 1) {
                        Some(5) => {
                            i += 2;
                            ps.get(i).map(|&v| xterm256(v.min(255) as u8))
                        }
                        Some(2) => {
                            i += 4;
                            let g = |k: usize| ps.get(i - 2 + k).copied().unwrap_or(0).min(255) as u8;
                            Some(Color32::from_rgb(g(0), g(1), g(2)))
                        }
                        _ => None,
                    };
                    if n == 38 {
                        self.pen.fg = col;
                    } else {
                        self.pen.bg = col;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_mode(&mut self, private: bool, ps: &[u16], on: bool) {
        for &p in ps {
            match (private, p) {
                (true, 1) => self.app_cursor = on,
                (true, 7) => self.autowrap = on,
                (true, 25) => self.cursor_visible = on,
                (true, 47 | 1047 | 1049) => {
                    if on && self.alt.is_none() {
                        let main = std::mem::replace(&mut self.grid, vec![vec![Cell::BLANK; self.cols]; self.rows]);
                        self.alt = Some((main, self.x, self.y));
                    } else if !on {
                        if let Some((main, x, y)) = self.alt.take() {
                            self.grid = main;
                            self.grid.resize(self.rows, vec![Cell::BLANK; self.cols]);
                            for r in self.grid.iter_mut() {
                                r.resize(self.cols, Cell::BLANK);
                            }
                            self.x = x.min(self.cols - 1);
                            self.y = y.min(self.rows - 1);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn xterm256(n: u8) -> Color32 {
    match n {
        0..=15 => ANSI16[n as usize],
        16..=231 => {
            let n = n - 16;
            let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(c(n / 36), c((n / 6) % 6), c(n % 6))
        }
        _ => {
            let g = 8 + (n - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        // DEC 线条字符集（menuconfig、dialog 等用 ESC(0 画框）
        let c = if self.charset[self.gl] && ('`'..='~').contains(&c) { dec_graphics(c) } else { c };
        self.put_char(c);
    }

    fn execute(&mut self, b: u8) {
        match b {
            b'\r' => {
                self.x = 0;
                self.wrap_pending = false;
            }
            b'\n' | 0x0b | 0x0c => {
                self.newline();
                self.wrap_pending = false;
            }
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => {
                self.x = ((self.x / 8 + 1) * 8).min(self.cols - 1);
            }
            0x0e => self.gl = 1,
            0x0f => self.gl = 0,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        let ps: Vec<u16> = params.iter().map(|p| p.first().copied().unwrap_or(0)).collect();
        let p = |i: usize, d: usize| ps.get(i).map(|&v| if v == 0 { d } else { v as usize }).unwrap_or(d);
        let private = inter.first() == Some(&b'?');
        self.wrap_pending = false;
        let (cols, rows) = (self.cols, self.rows);
        match action {
            'A' => self.y = self.y.saturating_sub(p(0, 1)).max(if self.y >= self.top { self.top } else { 0 }),
            'B' | 'e' => self.y = (self.y + p(0, 1)).min(if self.y <= self.bottom { self.bottom } else { rows - 1 }),
            'C' | 'a' => self.x = (self.x + p(0, 1)).min(cols - 1),
            'D' => self.x = self.x.saturating_sub(p(0, 1)),
            'E' => {
                self.y = (self.y + p(0, 1)).min(rows - 1);
                self.x = 0;
            }
            'F' => {
                self.y = self.y.saturating_sub(p(0, 1));
                self.x = 0;
            }
            'G' | '`' => self.x = (p(0, 1) - 1).min(cols - 1),
            'd' => self.y = (p(0, 1) - 1).min(rows - 1),
            'H' | 'f' => {
                self.y = (p(0, 1) - 1).min(rows - 1);
                self.x = (p(1, 1) - 1).min(cols - 1);
            }
            'J' => match ps.first().copied().unwrap_or(0) {
                0 => {
                    self.erase(self.y, self.x, cols);
                    for y in self.y + 1..rows {
                        self.erase(y, 0, cols);
                    }
                }
                1 => {
                    for y in 0..self.y {
                        self.erase(y, 0, cols);
                    }
                    self.erase(self.y, 0, self.x + 1);
                }
                2 => {
                    for y in 0..rows {
                        self.erase(y, 0, cols);
                    }
                }
                3 => self.scrollback.clear(),
                _ => {}
            },
            'K' => match ps.first().copied().unwrap_or(0) {
                0 => self.erase(self.y, self.x, cols),
                1 => self.erase(self.y, 0, self.x + 1),
                _ => self.erase(self.y, 0, cols),
            },
            'L' if (self.top..=self.bottom).contains(&self.y) => {
                for _ in 0..p(0, 1).min(self.bottom - self.y + 1) {
                    self.grid.remove(self.bottom);
                    self.grid.insert(self.y, vec![self.blank(); cols]);
                }
            }
            'M' if (self.top..=self.bottom).contains(&self.y) => {
                for _ in 0..p(0, 1).min(self.bottom - self.y + 1) {
                    self.grid.remove(self.y);
                    self.grid.insert(self.bottom, vec![self.blank(); cols]);
                }
            }
            'P' => {
                let n = p(0, 1).min(cols - self.x);
                let b = self.blank();
                let row = &mut self.grid[self.y];
                row.drain(self.x..self.x + n);
                row.extend(std::iter::repeat_n(b, n));
            }
            '@' => {
                let n = p(0, 1).min(cols - self.x);
                let b = self.blank();
                let row = &mut self.grid[self.y];
                for _ in 0..n {
                    row.insert(self.x, b);
                }
                row.truncate(cols);
            }
            'X' => {
                let n = p(0, 1);
                self.erase(self.y, self.x, self.x + n);
            }
            'S' => self.scroll_up(p(0, 1)),
            'T' => self.scroll_down(p(0, 1)),
            'm' => self.sgr(params),
            'r' => {
                let t = p(0, 1) - 1;
                let b = p(1, rows) - 1;
                if t < b && b < rows {
                    self.top = t;
                    self.bottom = b;
                    self.x = 0;
                    self.y = 0;
                }
            }
            's' => self.saved = Saved { x: self.x, y: self.y, pen: self.pen },
            'u' => {
                self.x = self.saved.x.min(cols - 1);
                self.y = self.saved.y.min(rows - 1);
            }
            'h' => self.set_mode(private, &ps, true),
            'l' => self.set_mode(private, &ps, false),
            'n' if ps.first() == Some(&6) => {
                // 光标位置报告：busybox、resize 等会用它探测终端大小
                self.reply.extend(format!("\x1b[{};{}R", self.y + 1, self.x + 1).bytes());
            }
            'n' if ps.first() == Some(&5) => self.reply.extend(b"\x1b[0n"),
            'c' if !private => self.reply.extend(b"\x1b[?1;2c"), // 设备属性：报告为 VT100
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, b: u8) {
        match inter {
            [] => {}
            [g @ (b'(' | b')')] => {
                self.charset[(*g == b')') as usize] = b == b'0';
                return;
            }
            _ => return,
        }
        match b {
            b'7' => self.saved = Saved { x: self.x, y: self.y, pen: self.pen },
            b'8' => {
                self.x = self.saved.x.min(self.cols - 1);
                self.y = self.saved.y.min(self.rows - 1);
                self.pen = self.saved.pen;
            }
            b'D' => self.newline(),
            b'E' => {
                self.x = 0;
                self.newline();
            }
            b'M' => {
                if self.y == self.top {
                    self.scroll_down(1);
                } else {
                    self.y = self.y.saturating_sub(1);
                }
            }
            b'c' => {
                let (c, r, m) = (self.cols, self.rows, self.max_scrollback);
                *self = Screen::new(c, r);
                self.max_scrollback = m;
            }
            _ => {}
        }
    }
}

fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'f' => '°',
        'g' => '±',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'q' => '─',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '~' => '·',
        _ => c,
    }
}

/// 把一行转成文本（去掉宽字符占位和行尾空白）
pub fn row_text(row: &[Cell], from: usize, to: usize) -> String {
    let s: String = row.iter().take(to).skip(from).filter(|c| !c.wide_tail).map(|c| c.ch).collect();
    s.trim_end().to_string()
}

/// 把一个按键翻译成发给设备的字节
pub fn key_bytes(key: eframe::egui::Key, mods: eframe::egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use eframe::egui::Key;
    let arrow = |c: u8| if app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let tilde = |n: &str| format!("\x1b[{n}~").into_bytes();
    if mods.ctrl && !mods.alt {
        // Ctrl+字母 → 控制字符（Ctrl+C = 0x03 等）
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(vec![c - b'@']);
            }
        }
        return match key {
            Key::OpenBracket => Some(vec![0x1b]),
            Key::Backslash => Some(vec![0x1c]),
            Key::CloseBracket => Some(vec![0x1d]),
            Key::Space => Some(vec![0x00]),
            _ => None,
        };
    }
    Some(match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => vec![0x7f],
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => arrow(b'A'),
        Key::ArrowDown => arrow(b'B'),
        Key::ArrowRight => arrow(b'C'),
        Key::ArrowLeft => arrow(b'D'),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::Insert => tilde("2"),
        Key::Delete => tilde("3"),
        Key::PageUp => tilde("5"),
        Key::PageDown => tilde("6"),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F6 => tilde("17"),
        Key::F7 => tilde("18"),
        Key::F8 => tilde("19"),
        Key::F9 => tilde("20"),
        Key::F10 => tilde("21"),
        Key::F11 => tilde("23"),
        Key::F12 => tilde("24"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &Term, y: usize) -> String {
        t.s.grid[y].iter().filter(|c| !c.wide_tail).map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn basic_print_and_newline() {
        let mut t = Term::new(20, 5);
        t.feed(b"hello\r\nworld");
        assert_eq!(text(&t, 0), "hello");
        assert_eq!(text(&t, 1), "world");
        assert_eq!((t.s.x, t.s.y), (5, 1));
    }

    #[test]
    fn cursor_and_erase() {
        let mut t = Term::new(20, 5);
        t.feed(b"abcdef\x1b[3D\x1b[K");
        assert_eq!(text(&t, 0), "abc");
        t.feed(b"\x1b[2J\x1b[3;5HX");
        assert_eq!(text(&t, 2), "    X");
    }

    #[test]
    fn scroll_into_history() {
        let mut t = Term::new(10, 3);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n");
        assert_eq!(t.s.scrollback.len(), 2);
        assert_eq!(text(&t, 0), "3");
    }

    #[test]
    fn backspace_line_edit() {
        // shell 删除字符的典型回显：BS 空格 BS
        let mut t = Term::new(20, 3);
        t.feed(b"# lsx\x08 \x08");
        assert_eq!(text(&t, 0), "# ls");
        assert_eq!(t.s.x, 4);
    }

    #[test]
    fn alt_screen_restores() {
        let mut t = Term::new(20, 3);
        t.feed(b"main");
        t.feed(b"\x1b[?1049h\x1b[H\x1b[2JVI");
        assert_eq!(text(&t, 0), "VI");
        t.feed(b"\x1b[?1049l");
        assert_eq!(text(&t, 0), "main");
    }

    #[test]
    fn cursor_report_and_wide() {
        let mut t = Term::new(20, 3);
        t.feed("中文".as_bytes());
        assert_eq!(t.s.x, 4);
        t.feed(b"\x1b[6n");
        assert_eq!(t.s.reply, b"\x1b[1;5R");
    }

    #[test]
    fn colors() {
        let mut t = Term::new(20, 3);
        t.feed(b"\x1b[1;31mE\x1b[0m");
        assert_eq!(t.s.grid[0][0].style.fg, Some(ANSI16[1]));
        assert!(t.s.grid[0][0].style.bold);
    }

    #[test]
    fn line_drawing_charset() {
        let mut t = Term::new(20, 3);
        t.feed(b"\x1b(0lqk\x1b(Bx");
        assert_eq!(text(&t, 0), "┌─┐x");
    }

    #[test]
    fn keys() {
        use eframe::egui::{Key, Modifiers};
        assert_eq!(key_bytes(Key::C, Modifiers::CTRL, false), Some(vec![3]));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, true), Some(b"\x1bOA".to_vec()));
    }
}

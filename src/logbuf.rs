//! 日志缓冲：按行切分、按编码解码、解析 ANSI 颜色、识别日志级别。

use std::collections::VecDeque;
use std::time::Instant;

use chrono::{DateTime, Local};
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Encoding {
    Utf8,
    Gbk,
    Ascii,
    Latin1,
}

impl Encoding {
    pub const ALL: [Encoding; 4] = [Encoding::Utf8, Encoding::Gbk, Encoding::Ascii, Encoding::Latin1];
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Gbk => "GBK / GB2312",
            Encoding::Ascii => "ASCII",
            Encoding::Latin1 => "Latin-1",
        }
    }
    pub fn encode(self, s: &str) -> Vec<u8> {
        match self {
            Encoding::Gbk => encoding_rs::GBK.encode(s).0.into_owned(),
            Encoding::Latin1 => encoding_rs::WINDOWS_1252.encode(s).0.into_owned(),
            _ => s.as_bytes().to_vec(),
        }
    }
    fn decode(self, raw: &[u8]) -> String {
        match self {
            Encoding::Utf8 => String::from_utf8_lossy(raw).into_owned(),
            Encoding::Gbk => encoding_rs::GBK.decode_without_bom_handling(raw).0.into_owned(),
            Encoding::Latin1 => encoding_rs::WINDOWS_1252.decode_without_bom_handling(raw).0.into_owned(),
            Encoding::Ascii => raw.iter().map(|&b| if b < 0x80 { b as char } else { '·' }).collect(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Level {
    None,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Rx,
    /// 双口监听时第二个串口（B）收到的
    Rx2,
    Tx,
    Sys,
}

/// ANSI 解析出的一段样式（作用于 plain 的字节区间）
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub bold: bool,
}

pub struct Line {
    pub ts: DateTime<Local>,
    pub dir: Dir,
    pub raw: Vec<u8>,
    /// 去掉 ANSI 转义后的可显示文本
    pub plain: String,
    pub spans: Vec<Span>,
    pub level: Level,
    /// 级别标记在 plain 里的位置（如 "[ERROR]"、"E (12)"），用来单独上色
    pub level_span: Option<(usize, usize)>,
    /// 按帧格式切出来的帧：校验对不对（没配置校验时是 Some(true)）
    pub check_ok: Option<bool>,
    /// 用户打的书签
    pub bookmark: bool,
}

impl Line {
    fn new(dir: Dir, raw: Vec<u8>, enc: Encoding) -> Self {
        let mut l = Line {
            ts: Local::now(),
            dir,
            raw,
            plain: String::new(),
            spans: vec![],
            level: Level::None,
            level_span: None,
            check_ok: None,
            bookmark: false,
        };
        l.render(enc);
        l
    }

    /// 行收完后释放多余的预留空间（几十万行时能省下一半内存）
    fn compact(&mut self) {
        self.raw.shrink_to_fit();
        self.plain.shrink_to_fit();
        self.spans.shrink_to_fit();
    }

    pub fn render(&mut self, enc: Encoding) {
        let mut raw: &[u8] = &self.raw;
        while let [rest @ .., b'\r' | b'\n'] = raw {
            raw = rest;
        }
        let text = enc.decode(raw);
        let (plain, spans) = parse_ansi(&text);
        (self.level, self.level_span) = detect_level(&plain);
        self.plain = plain;
        self.spans = spans;
    }
}

pub struct LogBuf {
    pub lines: VecDeque<Line>,
    /// 已经从队头丢弃的行数，用于把“绝对行号”换算成下标
    pub dropped: u64,
    /// 最后一行是否还是没收完的 RX 行
    open: bool,
    last_rx: Option<Instant>,
    pub max_lines: usize,
    /// 无换行时单行最大字节数，超过就强制换行
    pub max_line_bytes: usize,
    /// 空闲分帧（毫秒，0 = 关闭）：两次数据间隔超过它就另起一行，适合二进制协议
    pub frame_gap_ms: u32,
    /// 新收完的行（给录制到文件、过滤使用），由调用方取走
    pub committed: Vec<u64>,
    /// 缓冲区里已收完的 RX 行按级别计数（下标是 Level as usize）
    pub counts: [u64; 6],
    /// 按帧格式切分二进制数据（帧头 + 长度 + 校验），代替按换行分行
    pub framer: Option<Framer>,
}

impl LogBuf {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            dropped: 0,
            open: false,
            last_rx: None,
            max_lines,
            max_line_bytes: 2048,
            frame_gap_ms: 0,
            committed: vec![],
            counts: [0; 6],
            framer: None,
        }
    }

    pub fn clear(&mut self) {
        self.dropped += self.lines.len() as u64;
        self.lines.clear();
        self.open = false;
        self.committed.clear();
        self.counts = [0; 6];
    }

    pub fn total(&self) -> u64 {
        self.dropped + self.lines.len() as u64
    }

    pub fn get(&self, id: u64) -> Option<&Line> {
        id.checked_sub(self.dropped).and_then(|i| self.lines.get(i as usize))
    }

    pub fn is_open(&self, id: u64) -> bool {
        self.open && id + 1 == self.total()
    }

    pub fn close_open(&mut self) {
        if self.open {
            self.open = false;
            self.committed.push(self.total() - 1);
            if let Some(l) = self.lines.back_mut() {
                self.counts[l.level as usize] += 1;
                l.compact();
            }
        }
    }

    pub fn push_rx(&mut self, data: &[u8], enc: Encoding) {
        if self.framer.is_some() {
            self.push_frames(data, enc);
            return;
        }
        let now = Instant::now();
        if self.frame_gap_ms > 0 {
            if let Some(t) = self.last_rx {
                if now.duration_since(t).as_millis() as u32 >= self.frame_gap_ms {
                    self.close_open();
                }
            }
        }
        self.last_rx = Some(now);
        let mut touched = false;
        for chunk in data.split_inclusive(|&b| b == b'\n') {
            let mut chunk = chunk;
            while !chunk.is_empty() {
                if !self.open {
                    self.lines.push_back(Line::new(Dir::Rx, Vec::new(), enc));
                    self.open = true;
                }
                let line = self.lines.back_mut().unwrap();
                let room = self.max_line_bytes.saturating_sub(line.raw.len()).max(1);
                let take = chunk.len().min(room);
                line.raw.extend_from_slice(&chunk[..take]);
                chunk = &chunk[take..];
                touched = true;
                let full = line.raw.len() >= self.max_line_bytes;
                if line.raw.last() == Some(&b'\n') || full {
                    line.render(enc);
                    touched = false;
                    self.close_open();
                }
            }
        }
        if touched && self.open {
            self.lines.back_mut().unwrap().render(enc);
        }
        self.trim();
    }

    fn push_frames(&mut self, data: &[u8], enc: Encoding) {
        let now = Instant::now();
        let f = self.framer.as_mut().unwrap();
        // 隔了很久才来的数据：之前没收全的半帧丢掉（作为一行原样显示）
        let stale = self.last_rx.is_some_and(|t| now.duration_since(t).as_millis() > f.timeout_ms as u128);
        self.last_rx = Some(now);
        let mut out = vec![];
        if stale && !f.pending.is_empty() {
            out.push((std::mem::take(&mut f.pending), None));
        }
        f.pending.extend_from_slice(data);
        out.extend(f.drain());
        for (raw, ok) in out {
            self.close_open();
            let mut l = Line::new(Dir::Rx, raw, enc);
            l.check_ok = ok;
            if ok == Some(false) {
                l.level = Level::Error;
            }
            l.compact();
            self.counts[l.level as usize] += 1;
            self.lines.push_back(l);
            self.committed.push(self.total() - 1);
        }
        self.trim();
    }

    pub fn push_line(&mut self, dir: Dir, raw: Vec<u8>, enc: Encoding) {
        self.close_open();
        let mut l = Line::new(dir, raw, enc);
        l.compact();
        self.lines.push_back(l);
        self.committed.push(self.total() - 1);
        self.trim();
    }

    /// 打开日志文件时逐行载入：RX 行也要计入级别统计
    pub fn push_loaded(&mut self, dir: Dir, raw: Vec<u8>, enc: Encoding, ts: Option<DateTime<Local>>) {
        self.close_open();
        let mut l = Line::new(dir, raw, if dir == Dir::Sys { Encoding::Utf8 } else { enc });
        if let Some(ts) = ts {
            l.ts = ts;
        }
        l.compact();
        if dir == Dir::Rx {
            self.counts[l.level as usize] += 1;
        }
        // 不进 committed：载入的旧日志不触发应答、不重复录制
        self.lines.push_back(l);
        self.trim();
    }

    pub fn sys(&mut self, msg: impl Into<String>) {
        self.push_line(Dir::Sys, msg.into().into_bytes(), Encoding::Utf8);
    }

    fn trim(&mut self) {
        // 超出 10% 再批量删，避免每行都触发
        if self.lines.len() > self.max_lines + self.max_lines / 10 {
            let n = self.lines.len() - self.max_lines;
            for l in self.lines.drain(..n) {
                if l.dir == Dir::Rx {
                    self.counts[l.level as usize] = self.counts[l.level as usize].saturating_sub(1);
                }
            }
            self.dropped += n as u64;
        }
    }

    pub fn rerender(&mut self, enc: Encoding) {
        self.counts = [0; 6];
        let open = self.open;
        let n = self.lines.len();
        for (i, l) in self.lines.iter_mut().enumerate() {
            if l.dir != Dir::Sys {
                l.render(enc);
                l.compact();
            }
            if l.dir == Dir::Rx && !(open && i + 1 == n) {
                self.counts[l.level as usize] += 1;
            }
        }
    }
}

// ---------------------------------------------------------------- 帧解析

/// 帧长度怎么定
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum LenSpec {
    /// 每帧固定字节数
    Fixed(usize),
    /// 帧里有长度字段：从帧头开始第 offset 个字节，size 字节，be = 大端；整帧长度 = 字段值 + adjust
    Field { offset: usize, size: u8, be: bool, adjust: i32 },
}

pub struct Framer {
    pub header: Vec<u8>,
    pub len: LenSpec,
    pub check: crate::checksum::Check,
    /// 半帧超过这么久没收全就丢掉
    pub timeout_ms: u32,
    pending: Vec<u8>,
}

impl Framer {
    pub fn new(header: Vec<u8>, len: LenSpec, check: crate::checksum::Check, timeout_ms: u32) -> Self {
        Framer {
            header,
            len,
            check,
            timeout_ms,
            pending: vec![],
        }
    }

    /// 整帧长度；字节还不够判断时返回 None
    fn frame_len(&self, b: &[u8]) -> Option<usize> {
        match self.len {
            LenSpec::Fixed(n) => Some(n.max(self.header.len() + 1)),
            LenSpec::Field { offset, size, be, adjust } => {
                let f = b.get(offset..offset + size as usize)?;
                let v = f.iter().enumerate().fold(0u64, |a, (i, &x)| if be { (a << 8) | x as u64 } else { a | (x as u64) << (8 * i) });
                Some((v as i64 + adjust as i64).max((offset + size as usize) as i64) as usize)
            }
        }
    }

    /// 切出所有完整的帧：(字节, 校验是否通过)；帧头前面的杂数据单独成一行（校验 None）
    fn drain(&mut self) -> Vec<(Vec<u8>, Option<bool>)> {
        let mut out = vec![];
        loop {
            let b = &self.pending;
            let start = if self.header.is_empty() {
                Some(0)
            } else {
                b.windows(self.header.len()).position(|w| w == self.header.as_slice())
            };
            let Some(start) = start else {
                // 没找到帧头：保留可能是帧头开头的最后几个字节，其余算杂数据
                let keep = self.header.len().saturating_sub(1).min(b.len());
                if b.len() > keep + 256 {
                    let junk: Vec<u8> = self.pending.drain(..b.len() - keep).collect();
                    out.push((junk, None));
                }
                break;
            };
            if start > 0 {
                let junk: Vec<u8> = self.pending.drain(..start).collect();
                out.push((junk, None));
                continue;
            }
            let Some(n) = self.frame_len(b) else { break };
            if n > 65536 {
                // 长度字段明显不对：跳过这个帧头继续找
                let junk: Vec<u8> = self.pending.drain(..self.header.len().max(1)).collect();
                out.push((junk, Some(false)));
                continue;
            }
            if b.len() < n {
                break;
            }
            let frame: Vec<u8> = self.pending.drain(..n).collect();
            let ok = self.check.verify(&frame);
            out.push((frame, Some(ok)));
        }
        out
    }
}

// ---------------------------------------------------------------- ANSI

/// 适合暗色背景的 Linux 终端 16 色
pub const ANSI16: [Color32; 16] = [
    Color32::from_rgb(0x4a, 0x55, 0x66), // black（提亮，免得看不见）
    Color32::from_rgb(0xff, 0x55, 0x6e), // red
    Color32::from_rgb(0x3d, 0xf5, 0x9a), // green
    Color32::from_rgb(0xff, 0xd1, 0x4a), // yellow
    Color32::from_rgb(0x4d, 0x9d, 0xff), // blue
    Color32::from_rgb(0xd0, 0x6b, 0xff), // magenta
    Color32::from_rgb(0x00, 0xe5, 0xff), // cyan
    Color32::from_rgb(0xc8, 0xd3, 0xe0), // white
    Color32::from_rgb(0x6c, 0x7a, 0x8c),
    Color32::from_rgb(0xff, 0x7a, 0x8e),
    Color32::from_rgb(0x7d, 0xff, 0xb8),
    Color32::from_rgb(0xff, 0xe5, 0x7f),
    Color32::from_rgb(0x82, 0xbb, 0xff),
    Color32::from_rgb(0xe4, 0x9b, 0xff),
    Color32::from_rgb(0x7a, 0xf3, 0xff),
    Color32::from_rgb(0xff, 0xff, 0xff),
];

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

/// 解析 ANSI 转义序列（SGR 颜色/粗体），去掉其他控制序列。
/// 同时把 Tab 展开、其他控制字符替换成可见符号。
pub fn parse_ansi(s: &str) -> (String, Vec<Span>) {
    let mut plain = String::with_capacity(s.len());
    let mut spans = Vec::new();
    let (mut fg, mut bg, mut bold): (Option<Color32>, Option<Color32>, bool) = (None, None, false);
    let mut seg_start = 0usize;
    let mut fg_idx: Option<u8> = None;
    let mut chars = s.chars().peekable();

    macro_rules! flush {
        () => {
            if plain.len() > seg_start && (fg.is_some() || bg.is_some() || bold) {
                spans.push(Span {
                    start: seg_start,
                    end: plain.len(),
                    fg,
                    bg,
                    bold,
                });
            }
            seg_start = plain.len();
        };
    }

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut params = String::new();
                    let mut fin = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            fin = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if fin == Some('m') {
                        flush!();
                        let nums: Vec<i32> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
                        let mut i = 0;
                        while i < nums.len() {
                            match nums[i] {
                                0 => {
                                    fg = None;
                                    bg = None;
                                    bold = false;
                                    fg_idx = None;
                                }
                                1 => bold = true,
                                2 | 22 => bold = false,
                                n @ 30..=37 => {
                                    fg_idx = Some((n - 30) as u8);
                                    fg = Some(ANSI16[(n - 30) as usize + if bold { 8 } else { 0 }]);
                                }
                                n @ 90..=97 => fg = Some(ANSI16[(n - 90 + 8) as usize]),
                                n @ 40..=47 => bg = Some(ANSI16[(n - 40) as usize].gamma_multiply(0.45)),
                                n @ 100..=107 => bg = Some(ANSI16[(n - 100 + 8) as usize].gamma_multiply(0.45)),
                                39 => {
                                    fg = None;
                                    fg_idx = None;
                                }
                                49 => bg = None,
                                n @ (38 | 48) => {
                                    let col = match nums.get(i + 1) {
                                        Some(5) => {
                                            i += 2;
                                            nums.get(i).map(|&v| xterm256(v.clamp(0, 255) as u8))
                                        }
                                        Some(2) => {
                                            i += 4;
                                            let g = |k: usize| nums.get(i + k - 2).copied().unwrap_or(0).clamp(0, 255) as u8;
                                            Some(Color32::from_rgb(g(0), g(1), g(2)))
                                        }
                                        _ => None,
                                    };
                                    if n == 38 {
                                        fg = col;
                                    } else {
                                        bg = col.map(|c| c.gamma_multiply(0.45));
                                    }
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                        // 粗体时普通色提亮（终端的传统行为）
                        if bold {
                            if let Some(k) = fg_idx {
                                fg = Some(ANSI16[k as usize + 8]);
                            }
                        }
                    }
                } else {
                    // 其他 ESC 序列：跳过下一个字符；ESC ( 0 这类字符集切换再多跳一个
                    if matches!(chars.next(), Some('(' | ')' | '#')) {
                        chars.next();
                    }
                }
            }
            '\t' => plain.push_str("    "),
            '\r' => {}
            c if (c as u32) < 0x20 || c == '\x7f' => plain.push('·'),
            c => plain.push(c),
        }
    }
    flush!();
    let _ = seg_start;
    (plain, spans)
}

// ---------------------------------------------------------------- 日志级别

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 识别常见日志格式的级别：
/// `[ERROR]` `ERROR:` `<E>`、ESP-IDF `E (123) tag:`、Android `E/tag:`、Zephyr `<err>` 等。
pub fn detect_level(s: &str) -> (Level, Option<(usize, usize)>) {
    let b = s.as_bytes();
    // 行首单字母格式：E (123)  / E/tag / [E]
    let t = s.trim_start();
    let tb = t.as_bytes();
    let off = s.len() - t.len();
    let first_letter = |c: u8| match c {
        b'E' | b'F' => Some(Level::Error),
        b'W' => Some(Level::Warn),
        b'I' => Some(Level::Info),
        b'D' => Some(Level::Debug),
        b'V' | b'T' => Some(Level::Trace),
        _ => None,
    };
    if tb.len() >= 3 {
        if let Some(l) = first_letter(tb[0]) {
            if tb[1] == b'/' || (tb[1] == b' ' && tb[2] == b'(') || tb[1] == b':' {
                return (l, Some((off, off + 1)));
            }
        }
        if (tb[0] == b'[' && tb[2] == b']') || (tb[0] == b'<' && tb[2] == b'>') {
            if let Some(l) = first_letter(tb[1]) {
                return (l, Some((off, off + 3)));
            }
        }
    }
    // 关键词（整词匹配，只看前 120 字节，速度优先）
    const KW: &[(&str, Level)] = &[
        ("FATAL", Level::Error),
        ("PANIC", Level::Error),
        ("ERROR", Level::Error),
        ("ERR", Level::Error),
        ("FAIL", Level::Error),
        ("FAILED", Level::Error),
        ("WARNING", Level::Warn),
        ("WARN", Level::Warn),
        ("WRN", Level::Warn),
        ("INFO", Level::Info),
        ("INF", Level::Info),
        ("NOTICE", Level::Info),
        ("DEBUG", Level::Debug),
        ("DBG", Level::Debug),
        ("TRACE", Level::Trace),
        ("VERBOSE", Level::Trace),
    ];
    let scan = &b[..b.len().min(120)];
    let mut i = 0;
    while i < scan.len() {
        if scan[i].is_ascii_alphabetic() && (i == 0 || !is_word_byte(scan[i - 1])) {
            let mut j = i;
            while j < scan.len() && is_word_byte(scan[j]) {
                j += 1;
            }
            let w = &scan[i..j];
            if w.len() <= 7 {
                for (k, l) in KW {
                    if w.eq_ignore_ascii_case(k.as_bytes()) {
                        // 小写 “error” 只在被括号/冒号包围时算，减少误报
                        let upper = w.iter().all(|c| !c.is_ascii_lowercase());
                        let boxed = (i > 0 && matches!(scan[i - 1], b'[' | b'<' | b'(')) || matches!(scan.get(j), Some(b']' | b'>' | b':' | b')'));
                        if upper || boxed {
                            // 连同两侧的括号一起上色
                            let a = if i > 0 && matches!(scan[i - 1], b'[' | b'<' | b'(') { i - 1 } else { i };
                            let b = if matches!(scan.get(j), Some(b']' | b'>' | b')')) { j + 1 } else { j };
                            return (*l, Some((a, b)));
                        }
                    }
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    (Level::None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        use crate::checksum::Check;
        let mut b = LogBuf::new(100);
        // AA 55 | len | payload… | sum8（长度字段 = 负载字节数，整帧 = len + 4）
        b.framer = Some(Framer::new(
            vec![0xAA, 0x55],
            LenSpec::Field {
                offset: 2,
                size: 1,
                be: true,
                adjust: 4,
            },
            Check::Sum8,
            1000,
        ));
        let mut f1 = vec![0xAA, 0x55, 0x02, 0x10, 0x20];
        f1.push(Check::Sum8.value(&f1) as u8);
        let mut data = vec![0x00, 0x01];
        data.extend(&f1);
        data.extend(&f1[..3]);
        b.push_rx(&data, Encoding::Utf8);
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.lines[0].raw, vec![0x00, 0x01]);
        assert_eq!(b.lines[1].check_ok, Some(true));
        let mut rest = f1[3..].to_vec();
        rest[2] ^= 1;
        b.push_rx(&rest, Encoding::Utf8);
        assert_eq!(b.lines.len(), 3);
        assert_eq!(b.lines[2].check_ok, Some(false));
    }

    #[test]
    fn ansi_basic() {
        let (p, s) = parse_ansi("\x1b[0;31mE (12) boot: fail\x1b[0m ok");
        assert_eq!(p, "E (12) boot: fail ok");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].fg, Some(ANSI16[1]));
        assert_eq!(&p[s[0].start..s[0].end], "E (12) boot: fail");
    }

    #[test]
    fn ansi_256_and_rgb() {
        let (p, s) = parse_ansi("\x1b[38;5;196mA\x1b[38;2;1;2;3mB\x1b[0m");
        assert_eq!(p, "AB");
        assert_eq!(s[1].fg, Some(Color32::from_rgb(1, 2, 3)));
    }

    #[test]
    fn levels() {
        assert_eq!(detect_level("E (123) wifi: x").0, Level::Error);
        assert_eq!(detect_level("W/Tag: x").0, Level::Warn);
        assert_eq!(detect_level("[00:01] [INFO] started").0, Level::Info);
        assert_eq!(detect_level("<err> spi: timeout").0, Level::Error);
        assert_eq!(detect_level("debug: value=3").0, Level::Debug);
        assert_eq!(detect_level("no error here").0, Level::None);
        assert_eq!(detect_level("[00:01] [INFO] x").1, Some((8, 14)));
        assert_eq!(detect_level("Everything fine").0, Level::None);
    }

    #[test]
    fn line_split_and_partial() {
        let mut b = LogBuf::new(100);
        b.push_rx(b"hel", Encoding::Utf8);
        assert_eq!(b.lines.len(), 1);
        assert!(b.is_open(0));
        b.push_rx(b"lo\r\nwor", Encoding::Utf8);
        assert_eq!(b.lines[0].plain, "hello");
        assert_eq!(b.lines[1].plain, "wor");
        assert_eq!(b.committed, vec![0]);
        assert_eq!(b.counts[Level::None as usize], 1);
        b.push_line(Dir::Tx, b"AT".to_vec(), Encoding::Utf8);
        assert_eq!(b.lines.len(), 3);
        assert!(!b.is_open(1));
    }

    #[test]
    fn gbk_decode() {
        let raw = encoding_rs::GBK.encode("温度正常").0.into_owned();
        let mut b = LogBuf::new(10);
        b.push_rx(&raw, Encoding::Gbk);
        assert_eq!(b.lines[0].plain, "温度正常");
    }

    #[test]
    fn trims() {
        let mut b = LogBuf::new(10);
        for _ in 0..50 {
            b.push_rx(b"x\n", Encoding::Utf8);
        }
        assert!(b.lines.len() <= 11);
        assert_eq!(b.total(), 50);
        assert_eq!(b.get(49).unwrap().plain, "x");
    }
}

//! 日志增强：过滤 / 高亮规则、时间差、书签和标记、统计、打开和回放日志文件、分帧设置。
//! 平时不占界面：规则和帧格式在小窗口里改，其余在日志工具栏的「更多」菜单和命令面板里。

use super::*;
use crate::checksum::{self, Check};
use crate::logbuf::{Framer, LenSpec};
use chrono::{NaiveDateTime, NaiveTime};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::mpsc::{SyncSender, TryRecvError};

// ---------------------------------------------------------------- 规则

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum RuleAct {
    /// 只显示匹配的行（多条「只显示」规则之间是“或”）
    Show,
    /// 隐藏匹配的行
    Hide,
    /// 只高亮
    Mark,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Rule {
    pub on: bool,
    pub pat: String,
    pub regex: bool,
    pub act: RuleAct,
    /// 高亮预设的下标，顺序与已有配置兼容。
    pub color: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_color: Option<[u8; 3]>,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            on: true,
            pat: String::new(),
            regex: false,
            act: RuleAct::Mark,
            color: 0,
            custom_color: None,
        }
    }
}

impl Rule {
    fn highlight_color(&self) -> ManualColor {
        self.custom_color.map_or(ManualColor::Palette(self.color), ManualColor::Custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualColor {
    Palette(u8),
    Custom([u8; 3]),
}

const COLOR_NAMES: [&str; 14] = ["紫色", "黄色", "粉色", "蓝色", "橙色", "青色", "红色", "绿色", "蓝绿色", "靛蓝色", "黄绿色", "玫红色", "蓝灰色", "棕色"];

pub(super) const MANUAL_COLOR_COUNT: usize = COLOR_NAMES.len();

impl ManualColor {
    pub(super) fn resolve(self, palette: &[Color32; MANUAL_COLOR_COUNT]) -> Color32 {
        match self {
            Self::Palette(index) => palette[index as usize % palette.len()],
            Self::Custom([r, g, b]) => readable_highlight(Color32::from_rgb(r, g, b), BG0()),
        }
    }
}

pub(super) fn manual_palette() -> [Color32; MANUAL_COLOR_COUNT] {
    let [violet, yellow, pink, blue, orange, cyan] = kw_colors();
    [
        violet,
        yellow,
        pink,
        blue,
        orange,
        cyan,
        ERR(),
        Color32::from_rgb(0x4a, 0xde, 0x80),
        Color32::from_rgb(0x2d, 0xd4, 0xbf),
        Color32::from_rgb(0x81, 0x8c, 0xf8),
        Color32::from_rgb(0xa3, 0xe6, 0x35),
        Color32::from_rgb(0xe8, 0x79, 0xf9),
        Color32::from_rgb(0x94, 0xa3, 0xb8),
        Color32::from_rgb(0xd9, 0xa4, 0x7b),
    ]
    .map(|color| readable_highlight(color, BG0()))
}

fn readable_highlight(color: Color32, background: Color32) -> Color32 {
    let target = if luminance(background) < 0.5 { Color32::WHITE } else { Color32::BLACK };
    // Evaluate against the same tinted background used by the log renderer.
    (0..20)
        .map(|step| color.lerp_to_gamma(target, step as f32 * 0.05))
        .find(|&fg| contrast(luminance(fg), luminance(background.blend(fg.gamma_multiply(0.16)))) >= 4.5)
        .unwrap_or(target)
}

pub(super) fn highlight_color_popup(ui: &egui::Ui, anchor: &egui::Response, selected: &mut Option<ManualColor>, displayed: Color32, allow_auto: bool) -> bool {
    let id = anchor.id.with("highlight-color");
    if anchor.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    let before = *selected;
    egui::popup::popup_below_widget(ui, id, anchor, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        let colors = manual_palette();
        for (row, colors) in colors.chunks(7).enumerate() {
            ui.horizontal(|ui| {
                for (column, &color) in colors.iter().enumerate() {
                    let index = row * 7 + column;
                    let choice = ManualColor::Palette(index as u8);
                    let name = t(COLOR_NAMES[index]);
                    let active = before == Some(choice);
                    let outline = if active { st(2.0, TEXT()) } else { st(1.0, LINE_HI()) };
                    let button = egui::Button::new("").min_size(vec2(23.0, 23.0)).fill(color).stroke(outline);
                    let response = ui.add(button).on_hover_text(name);
                    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, name));
                    if response.clicked() {
                        *selected = Some(choice);
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
        }
        if allow_auto && ui.selectable_label(before.is_none(), t("自动")).clicked() {
            *selected = None;
            ui.memory_mut(|m| m.close_popup());
        }
        egui::CollapsingHeader::new(t("自定义颜色"))
            .default_open(matches!(before, Some(ManualColor::Custom(_))))
            .show(ui, |ui| {
                let mut color = match before {
                    Some(ManualColor::Custom([r, g, b])) => Color32::from_rgb(r, g, b),
                    _ => displayed,
                };
                if egui::color_picker::color_picker_color32(ui, &mut color, egui::color_picker::Alpha::Opaque) {
                    *selected = Some(ManualColor::Custom([color.r(), color.g(), color.b()]));
                }
            });
    });
    before != *selected
}

pub(super) enum Matcher {
    /// 不区分大小写的普通文字
    Plain(String),
    Re(regex_lite::Regex),
}

impl Matcher {
    pub(super) fn new(pat: &str, regex: bool) -> Result<Matcher, String> {
        if regex {
            regex_lite::Regex::new(pat).map(Matcher::Re).map_err(|e| e.to_string())
        } else {
            Ok(Matcher::Plain(pat.to_string()))
        }
    }
    pub(super) fn is_match(&self, s: &str) -> bool {
        match self {
            Matcher::Plain(p) => find_ci(s, p).is_some(),
            Matcher::Re(r) => r.is_match(s),
        }
    }
    pub(super) fn all(&self, s: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Plain(p) => find_all_ci(s, p),
            Matcher::Re(r) => r.find_iter(s).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).collect(),
        }
    }
}

pub(super) struct RuleRt {
    pub act: RuleAct,
    pub color: Color32,
    pub m: Matcher,
}

pub(super) fn log_preset_combo(ui: &mut egui::Ui, selected: &mut LogPreset) -> bool {
    let before = *selected;
    egui::ComboBox::from_id_salt("log-highlight-preset").selected_text(selected.name()).show_ui(ui, |ui| {
        for preset in LogPreset::ALL {
            ui.selectable_value(selected, preset, preset.name());
        }
    });
    before != *selected
}

// ---------------------------------------------------------------- 分帧

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct FrameCfg {
    pub on: bool,
    /// 帧头（HEX），可以为空
    pub header: String,
    /// 固定长度还是看长度字段
    pub fixed: bool,
    pub fixed_len: u16,
    /// 长度字段：从帧头第一个字节算起的偏移、字节数、大端
    pub len_off: u8,
    pub len_size: u8,
    pub len_be: bool,
    /// 整帧长度 = 长度字段的值 + adjust
    pub adjust: i16,
    pub check: Check,
    pub timeout_ms: u32,
}

impl Default for FrameCfg {
    fn default() -> Self {
        FrameCfg {
            on: false,
            header: "AA 55".into(),
            fixed: false,
            fixed_len: 8,
            len_off: 2,
            len_size: 1,
            len_be: true,
            adjust: 4,
            check: Check::Sum8,
            timeout_ms: 200,
        }
    }
}

impl FrameCfg {
    pub fn framer(&self) -> Result<Framer, String> {
        let header = parse_hex(&self.header)?;
        let len = if self.fixed {
            LenSpec::Fixed(self.fixed_len as usize)
        } else {
            LenSpec::Field {
                offset: self.len_off as usize,
                size: self.len_size.clamp(1, 4),
                be: self.len_be,
                adjust: self.adjust as i32,
            }
        };
        Ok(Framer::new(header, len, self.check, self.timeout_ms.max(10)))
    }

    /// 每个字节在帧里的角色：0 帧头、1 长度、2 数据、3 校验
    pub fn roles(&self, n: usize) -> Vec<u8> {
        let mut r = vec![2u8; n];
        let h = parse_hex(&self.header).map_or(0, |h| h.len());
        for x in r.iter_mut().take(h) {
            *x = 0;
        }
        if !self.fixed {
            let a = self.len_off as usize;
            for x in r.iter_mut().skip(a).take(self.len_size as usize) {
                *x = 1;
            }
        }
        let c = self.check.len();
        if n > c {
            for x in r.iter_mut().skip(n - c) {
                *x = 3;
            }
        }
        r
    }
}

// ---------------------------------------------------------------- 回放

pub(super) struct Replay {
    rx: Receiver<Result<ReplayLine, String>>,
    next: Option<ReplayLine>,
    total: u64,
    consumed: u64,
    pub speed: f32,
    /// 暂停时累计的“回放时间”（毫秒）
    played: f64,
    last: Instant,
    pub paused: bool,
    pub name: String,
}

impl Replay {
    pub(super) fn progress(&self) -> f32 {
        (self.consumed as f64 / self.total.max(1) as f64).min(1.0) as f32
    }

    pub(super) fn reset_clock(&mut self) {
        self.last = Instant::now();
    }
}

const LOG_WINDOW_MIB: u64 = 16;
const LOG_WINDOW_BYTES: u64 = LOG_WINDOW_MIB * 1024 * 1024;
const OPEN_LINE_BYTES: usize = 256 * 1024;
const REPLAY_LINE_MIB: usize = 4;
const REPLAY_LINE_BYTES: usize = REPLAY_LINE_MIB * 1024 * 1024;
const LOG_MAX_LINES: usize = 100_000;
const LOG_FRAME_BYTES: usize = OPEN_LINE_BYTES;
const LOG_FRAME_LINES: usize = 2000;
const LOG_PENDING_FILES: usize = 16;
const REPLAY_BUFFER_LINES: usize = 8;

type LoadedLine = (Option<NaiveDateTime>, Dir, Vec<u8>);

struct ReplayLine {
    at: i64,
    read: u64,
    line: LoadedLine,
}

#[derive(Default)]
pub(super) struct LogLoader {
    pending: VecDeque<(PathBuf, bool)>,
    active: Option<Receiver<Result<PreparedLog, String>>>,
    applying: VecDeque<LoadedLine>,
}

enum PreparedLog {
    Open {
        name: String,
        items: VecDeque<LoadedLine>,
        tail_only: bool,
        skipped_long: usize,
        max_lines: usize,
    },
    Replay {
        name: String,
        total: u64,
        rx: Receiver<Result<ReplayLine, String>>,
    },
}

/// Read a fixed-size snapshot of a regular file's tail. Discard a partial
/// first line when the window starts in the middle of a record.
fn read_log_window(mut file: impl Read + Seek, len: u64, max_bytes: u64) -> io::Result<(Vec<u8>, bool)> {
    let offset = len.saturating_sub(max_bytes);
    let mut starts_at_line = true;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset - 1))?;
        let mut prev = [0];
        file.read_exact(&mut prev)?;
        starts_at_line = prev[0] == b'\n';
    }
    let mut bytes = Vec::with_capacity((len - offset) as usize);
    file.take(len - offset).read_to_end(&mut bytes)?;
    if !starts_at_line {
        let skip = bytes.iter().position(|&c| c == b'\n').map_or(bytes.len(), |i| i + 1);
        bytes.drain(..skip);
    }
    Ok((bytes, offset > 0))
}

fn without_line_ending(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\r\n").or_else(|| bytes.strip_suffix(b"\n")).unwrap_or(bytes)
}

fn parse_loaded_line(raw: &[u8]) -> LoadedLine {
    let text = String::from_utf8_lossy(raw);
    let (ts, dir, body) = parse_log_line(&text);
    (ts, dir, body.as_bytes().to_vec())
}

fn parse_open_log(name: String, bytes: &[u8], tail_only: bool, max_lines: usize) -> PreparedLog {
    let lines = bytes.split_inclusive(|&c| c == b'\n');
    let skip = lines.clone().count().saturating_sub(max_lines);
    let mut items = VecDeque::new();
    let mut skipped_long = 0;
    for raw in lines.skip(skip).map(without_line_ending) {
        if raw.len() > OPEN_LINE_BYTES {
            skipped_long += 1;
            continue;
        }
        items.push_back(parse_loaded_line(raw));
    }
    PreparedLog::Open {
        name,
        items,
        tail_only: tail_only || skip != 0,
        skipped_long,
        max_lines,
    }
}

fn open_log_source(path: &std::path::Path) -> Result<(File, u64, String), String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err(t("选择文件").into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((file, len, name))
}

fn stream_replay(mut source: impl BufRead, tx: SyncSender<Result<ReplayLine, String>>) -> Result<(), String> {
    let mut bytes = Vec::new();
    let mut first = None;
    let mut last = 0i64;
    loop {
        bytes.clear();
        // Include room for CRLF, and one byte to distinguish a full line
        // from an oversized record without collecting it all into memory.
        let read = (&mut source).take((REPLAY_LINE_BYTES + 3) as u64).read_until(b'\n', &mut bytes).map_err(|e| e.to_string())?;
        if read == 0 {
            return Ok(());
        }
        let body = without_line_ending(&bytes);
        if body.len() > REPLAY_LINE_BYTES {
            return Err(tf("日志的单行超过 {} MiB，无法回放", &[&REPLAY_LINE_MIB]));
        }
        let line = parse_loaded_line(body);
        // Lines without timestamps use the preceding line's time.
        if let Some(ts) = line.0 {
            let start = *first.get_or_insert(ts);
            let mut elapsed = (ts - start).num_milliseconds();
            if elapsed < last - 43_200_000 {
                elapsed += 86_400_000; // time-only format crossing midnight
            }
            last = elapsed.max(last);
        }
        if tx.send(Ok(ReplayLine { at: last, read: read as u64, line })).is_err() {
            return Ok(()); // cancelled / window closed
        }
    }
}

fn prepare_log(path: PathBuf, replay: bool, max_lines: usize, tx: Sender<Result<PreparedLog, String>>, ctx: egui::Context) {
    let (file, len, name) = match open_log_source(&path) {
        Ok(source) => source,
        Err(err) => {
            let _ = tx.send(Err(err));
            ctx.request_repaint();
            return;
        }
    };
    if replay {
        let (out, rx) = mpsc::sync_channel(REPLAY_BUFFER_LINES);
        if tx.send(Ok(PreparedLog::Replay { name, total: len, rx })).is_err() {
            return;
        }
        ctx.request_repaint();
        if let Err(err) = stream_replay(BufReader::new(file.take(len)), out.clone()) {
            let _ = out.send(Err(err));
        }
    } else {
        let prepared = read_log_window(file, len, LOG_WINDOW_BYTES)
            .map(|(bytes, tail)| parse_open_log(name, &bytes, tail, max_lines))
            .map_err(|err| err.to_string());
        let _ = tx.send(prepared);
    }
    ctx.request_repaint();
}

/// 解析日志文件的一行。认得两种格式：
/// 保存 / 录制的 "2026-10-27 12:34:56.789 RX 内容"（RX / TX / RB / ## 表示方向），
/// 以及从界面复制的 "12:34:56.789  TX › 内容"（"◆ " 是系统消息，"B › " 是监听口 B）。
fn parse_log_line(line: &str) -> (Option<NaiveDateTime>, Dir, &str) {
    let mut rest = line;
    let mut ts = None;
    if line.len() >= 27 && line.is_char_boundary(23) {
        if let Ok(t) = NaiveDateTime::parse_from_str(&line[..23], "%Y-%m-%d %H:%M:%S%.3f") {
            let tail = &line[23..];
            let (dir, body) = match tail.get(..4) {
                Some(" RX ") => (Dir::Rx, &tail[4..]),
                Some(" TX ") => (Dir::Tx, &tail[4..]),
                Some(" RB ") => (Dir::Rx2, &tail[4..]),
                Some(" ## ") => (Dir::Sys, &tail[4..]),
                _ => (Dir::Rx, tail.trim_start()),
            };
            return (Some(t), dir, body);
        }
    }
    if line.len() >= 12 && line.is_char_boundary(12) {
        if let Ok(t) = NaiveTime::parse_from_str(&line[..12], "%H:%M:%S%.3f") {
            ts = Some(chrono::Local::now().date_naive().and_time(t));
            rest = line[12..].trim_start_matches(' ');
        }
    }
    if let Some(r) = rest.strip_prefix("TX › ") {
        (ts, Dir::Tx, r)
    } else if let Some(r) = rest.strip_prefix("B › ") {
        (ts, Dir::Rx2, r)
    } else if let Some(r) = rest.strip_prefix("◆ ") {
        (ts, Dir::Sys, r)
    } else {
        (ts, Dir::Rx, rest)
    }
}

/// 时间差：固定 8 个字符宽，对齐好看
pub(super) fn fmt_delta(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        format!("+{ms:>4}ms ")
    } else if ms < 60_000 {
        format!("+{:>5.2}s ", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("+{:>5.1}m ", ms as f64 / 60_000.0)
    } else {
        format!("+{:>5.1}h ", ms as f64 / 3_600_000.0)
    }
}

/// 日志统计（连接以来）
#[derive(Default)]
pub(super) struct Stats {
    pub peak_rate: f64,
    /// 最近 60 秒每秒的错误数
    pub err_hist: VecDeque<u32>,
    pub err_acc: u32,
    pub last_err_total: u64,
}

/// 多个调试工具窗口的开关
#[derive(Default)]
pub(super) struct Wins {
    pub rules: bool,
    pub frame: bool,
    pub stats: bool,
    pub merged: bool,
}

impl App {
    // ------------------------------------------------------------ 规则

    pub(super) fn compile_rules(&mut self) {
        let colors = manual_palette();
        self.rules_rt = self
            .s
            .rules
            .iter()
            .filter(|r| r.on && !r.pat.is_empty())
            .filter_map(|r| {
                Matcher::new(&r.pat, r.regex).ok().map(|m| RuleRt {
                    act: r.act,
                    color: r.highlight_color().resolve(&colors),
                    m,
                })
            })
            .collect();
        self.invalidate_log_views();
        self.repaint_log_windows(&self.ctx);
    }

    pub(super) fn rules_filter(&self) -> bool {
        self.rules_rt.iter().any(|r| r.act != RuleAct::Mark)
    }

    /// 按规则这一行要不要显示
    pub(super) fn rules_pass(&self, l: &Line) -> bool {
        if l.dir == Dir::Sys {
            return true;
        }
        let mut has_show = false;
        let mut shown = false;
        for r in &self.rules_rt {
            match r.act {
                RuleAct::Hide if r.m.is_match(&l.plain) => return false,
                RuleAct::Show => {
                    has_show = true;
                    shown = shown || r.m.is_match(&l.plain);
                }
                _ => {}
            }
        }
        !has_show || shown
    }

    // ------------------------------------------------------------ 书签和标记

    /// 烧录类任务：先给工程打一个存档点，日志里插一条「烧录 · 存档 #N」，以后能对上是哪份代码烧进去的
    pub(super) fn flash_checkpoint(&mut self, name: &str, cmd: &str) {
        const KEYS: &[&str] = &[
            "烧录",
            "下载",
            "flash",
            "download",
            "program",
            "burn",
            "upload",
            "openocd",
            "jlink",
            "st-flash",
            "esptool",
            "pyocd",
            "stm32_programmer",
        ];
        let s = format!("{name} {cmd}").to_lowercase();
        if !KEYS.iter().any(|k| s.contains(k)) {
            return;
        }
        self.flash_side();
        let Some(id) = self.code.checkpoint_now(&tf("烧录：{}", &[&name])) else { return };
        let text = format!("──────  {}  {}  ──────", tf("烧录 · 存档 #{}", &[&id]), chrono::Local::now().format("%H:%M:%S"));
        self.buf.sys(text);
        if let Some(l) = self.buf.lines.back_mut() {
            l.bookmark = true;
        }
        self.view_dirty = true;
    }

    pub(super) fn insert_marker(&mut self) {
        self.marker_n += 1;
        let text = format!("──────  {}  {}  ──────", tf("标记 {}", &[&self.marker_n]), chrono::Local::now().format("%H:%M:%S"));
        self.buf.sys(text);
        if let Some(l) = self.buf.lines.back_mut() {
            l.bookmark = true;
        }
        self.view_dirty = true;
    }

    pub(super) fn toggle_bookmark(&mut self, id: u64) {
        if let Some(i) = id.checked_sub(self.buf.dropped) {
            if let Some(l) = self.buf.lines.get_mut(i as usize) {
                l.bookmark = !l.bookmark;
            }
        }
    }

    pub(super) fn next_bookmark(&mut self, back: bool) {
        let total = self.buf.total();
        let from = self.search_hit.or(self.sel.map(|s| s.1)).unwrap_or(if back { total } else { self.buf.dropped.wrapping_sub(1) });
        let found = if back {
            (self.buf.dropped..from.min(total)).rev().find(|&id| self.buf.get(id).is_some_and(|l| l.bookmark && self.visible(l)))
        } else {
            (from.wrapping_add(1).max(self.buf.dropped)..total).find(|&id| self.buf.get(id).is_some_and(|l| l.bookmark && self.visible(l)))
        };
        match found {
            Some(id) => {
                self.sel = Some((id, id));
                self.tsel = None;
                self.search_hit = None;
                self.scroll_to = Some(id);
                self.auto_scroll = false;
            }
            None => self.toast(t("没有更多书签了"), TEXT2()),
        }
    }

    // ------------------------------------------------------------ 打开 / 回放日志文件

    pub(super) fn open_log_file(&mut self, replay: bool) {
        let Some(path) = crate::diagnostics::file_dialog().add_filter(t("日志"), &["log", "txt"]).add_filter(t("所有文件"), &["*"]).pick_file() else {
            return;
        };
        self.load_log_path(&path, replay);
    }

    /// 打开（或回放）一个日志文件：菜单、拖放、命令行 --open / --replay 都走这里
    pub(super) fn load_log_path(&mut self, path: &std::path::Path, replay: bool) {
        if self.log_load.pending.len() >= LOG_PENDING_FILES {
            self.toast(tf("最多可同时排队 {} 个日志文件", &[&LOG_PENDING_FILES]), WARN());
            return;
        }
        self.log_load.pending.push_back((path.to_owned(), replay));
        self.ctx.request_repaint();
    }

    fn log_file_status(&mut self, message: impl Into<String>) {
        // Do not write status or replay output into a live recording.
        self.buf.push_loaded(Dir::Sys, message.into().into_bytes(), Encoding::Utf8, None);
    }

    fn add_file_line(&mut self, (ts, dir, raw): LoadedLine) {
        let ts = ts.and_then(|t| t.and_local_timezone(chrono::Local).single());
        self.buf.push_loaded(dir, raw, Encoding::Utf8, ts);
    }

    fn tick_log_load(&mut self) {
        if let Some(loader) = self.log_load.active.take() {
            match loader.try_recv() {
                Ok(Ok(PreparedLog::Open {
                    name,
                    items,
                    tail_only,
                    skipped_long,
                    max_lines,
                })) => {
                    self.log_file_status(tf("打开日志文件 {}（{} 行）", &[&name, &items.len()]));
                    if tail_only {
                        self.log_file_status(tf("仅显示日志末尾（最多 {} MiB、{} 行）", &[&LOG_WINDOW_MIB, &max_lines]));
                    }
                    if skipped_long != 0 {
                        self.log_file_status(tf("已跳过 {} 个超过 {} KiB 的日志行", &[&skipped_long, &(OPEN_LINE_BYTES / 1024)]));
                    }
                    self.log_load.applying = items;
                    self.jump_bottom = true;
                }
                Ok(Ok(PreparedLog::Replay { name, total, rx })) => {
                    self.log_file_status(tf("开始回放 {}", &[&name]));
                    self.replay = Some(Replay {
                        rx,
                        next: None,
                        total,
                        consumed: 0,
                        speed: 1.0,
                        played: 0.0,
                        last: Instant::now(),
                        paused: false,
                        name,
                    });
                }
                Ok(Err(e)) => self.toast(tf("无法打开：{}", &[&e]), ERR()),
                Err(TryRecvError::Empty) => {
                    self.log_load.active = Some(loader);
                    return; // worker requests a repaint after sending its result
                }
                Err(TryRecvError::Disconnected) => self.toast(tf("无法打开：{}", &[&t("内部错误")]), ERR()),
            }
        }

        let mut bytes = 0usize;
        for _ in 0..LOG_FRAME_LINES {
            let Some(line) = self.log_load.applying.pop_front() else { break };
            bytes += line.2.len();
            self.add_file_line(line);
            self.jump_bottom = true;
            if bytes >= LOG_FRAME_BYTES {
                break;
            }
        }
        if !self.log_load.applying.is_empty() {
            self.ctx.request_repaint();
            return;
        }

        if let Some((path, replay)) = self.log_load.pending.pop_front() {
            let (tx, rx) = mpsc::channel();
            let ctx = self.ctx.clone();
            let max_lines = self.buf.max_lines.clamp(1, LOG_MAX_LINES);
            match std::thread::Builder::new().name("log-file".into()).spawn(move || prepare_log(path, replay, max_lines, tx, ctx)) {
                Ok(_) => self.log_load.active = Some(rx),
                Err(e) => {
                    self.toast(tf("无法打开：{}", &[&e]), ERR());
                    self.ctx.request_repaint();
                }
            }
        }
    }

    pub(super) fn tick_replay(&mut self) {
        self.tick_log_load();
        let Some(mut replay) = self.replay.take() else { return };
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(replay.last);
        replay.last = now;
        if replay.paused || self.paused {
            self.replay = Some(replay);
            return;
        }
        replay.played += elapsed.as_secs_f64() * 1000.0 * replay.speed as f64;
        let mut bytes = 0usize;
        let mut next_ms = 1;
        for _ in 0..LOG_FRAME_LINES {
            if replay.next.is_none() {
                match replay.rx.try_recv() {
                    Ok(Ok(line)) => replay.next = Some(line),
                    Ok(Err(e)) => {
                        self.log_file_status(tf("回放出错：{}", &[&e]));
                        self.toast(tf("回放出错：{}", &[&e]), ERR());
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        next_ms = 10;
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.log_file_status(tf("回放结束：{}", &[&replay.name]));
                        return;
                    }
                }
            }
            let Some(line) = replay.next.take() else { break };
            if replay.speed < 1000.0 && line.at as f64 > replay.played {
                next_ms = (((line.at as f64 - replay.played) / replay.speed as f64).clamp(1.0, 500.0)) as u64;
                replay.next = Some(line);
                break;
            }
            replay.consumed += line.read;
            bytes += line.line.2.len();
            let compare = matches!(line.line.1, Dir::Rx | Dir::Rx2) && (self.s.plot.on || self.dock.trig_rt.iter().any(Option::is_some));
            let id = self.buf.total();
            self.add_file_line(line.line);
            if compare {
                if let Some(text) = self.buf.get(id).map(|l| l.plain.clone()) {
                    self.on_line(&text, true);
                    self.plot_line(&text);
                }
            }
            if self.paused || bytes >= LOG_FRAME_BYTES {
                break;
            }
        }
        self.replay = Some(replay);
        self.ctx.request_repaint_after(Duration::from_millis(next_ms));
    }

    // ------------------------------------------------------------ 分帧

    pub(super) fn apply_frame(&mut self) {
        self.buf.close_open();
        self.buf.framer = if self.s.frame.on {
            match self.s.frame.framer() {
                Ok(f) => Some(f),
                Err(e) => {
                    self.toast(tf("帧头格式不对：{}", &[&e]), ERR());
                    None
                }
            }
        } else {
            None
        };
    }

    // ------------------------------------------------------------ 统计

    pub(super) fn tick_stats(&mut self) {
        let errs = self.buf.counts[Level::Error as usize];
        let new = errs.saturating_sub(self.stats.last_err_total);
        self.stats.last_err_total = errs;
        self.stats.err_acc += new as u32;
        self.stats.peak_rate = self.stats.peak_rate.max(self.rate);
    }

    /// 每秒调一次：错误数入历史
    pub(super) fn tick_stats_second(&mut self) {
        let n = std::mem::take(&mut self.stats.err_acc);
        self.stats.err_hist.push_back(n);
        while self.stats.err_hist.len() > 60 {
            self.stats.err_hist.pop_front();
        }
    }

    // ------------------------------------------------------------ 窗口

    pub(super) fn log_windows(&mut self, ctx: &egui::Context) {
        if self.wins.rules {
            self.rules_window(ctx);
        }
        if self.wins.frame {
            self.frame_window(ctx);
        }
        if self.wins.stats {
            self.stats_window(ctx);
        }
        self.merged_window(ctx);
    }

    fn rules_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut changed = false;
        tool_window(ctx, t("过滤与高亮规则"), "rules", &mut open, 520.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(t("高亮预设"));
                changed |= log_preset_combo(ui, &mut self.s.log_preset);
            });
            ui.separator();
            ui.label(RichText::new(t("「只显示」规则之间是“或”；「隐藏」优先。规则只影响显示，不影响录制。")).small().color(TEXT3()));
            ui.add_space(4.0);
            let colors = manual_palette();
            let mut del = None;
            for (i, r) in self.s.rules.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    changed |= switch(ui, &mut r.on).changed();
                    egui::ComboBox::from_id_salt(("ract", i)).width(76.0).selected_text(rule_act_name(r.act)).show_ui(ui, |ui| {
                        for a in [RuleAct::Mark, RuleAct::Show, RuleAct::Hide] {
                            changed |= ui.selectable_value(&mut r.act, a, rule_act_name(a)).changed();
                        }
                    });
                    let bad = r.regex && regex_lite::Regex::new(&r.pat).is_err();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut r.pat)
                                .desired_width(250.0)
                                .font(FontId::new(13.0, FontFamily::Monospace))
                                .hint_text(t("文字或正则"))
                                .text_color(if bad { ERR() } else { TEXT() }),
                        )
                        .changed();
                    changed |= small_chip(ui, &mut r.regex, ".*", t("正则表达式"));
                    if r.act != RuleAct::Hide {
                        let previous = r.highlight_color();
                        let displayed = previous.resolve(&colors);
                        let button = egui::Button::new("").min_size(vec2(24.0, 20.0)).fill(displayed).stroke(st(1.0, LINE_HI()));
                        let response = ui.add(button).on_hover_text(t("换颜色"));
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, t("换颜色")));
                        let mut selected = Some(previous);
                        if highlight_color_popup(ui, &response, &mut selected, displayed, false) {
                            match selected {
                                Some(ManualColor::Palette(index)) => {
                                    r.color = index;
                                    r.custom_color = None;
                                }
                                Some(ManualColor::Custom(rgb)) => r.custom_color = Some(rgb),
                                None => {}
                            }
                            changed = true;
                        }
                    } else {
                        ui.add_space(24.0);
                    }
                    if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                        del = Some(i);
                    }
                });
            }
            if let Some(i) = del {
                self.s.rules.remove(i);
                changed = true;
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui.button(t("＋ 添加规则")).clicked() {
                    let color = (self.s.rules.len() % MANUAL_COLOR_COUNT) as u8;
                    self.s.rules.push(Rule { color, ..Default::default() });
                }
                // 把搜索框里的词变成规则
                if !self.search.is_empty() && ui.button(tf("把“{}”加为规则", &[&self.search])).clicked() {
                    let color = (self.s.rules.len() % MANUAL_COLOR_COUNT) as u8;
                    self.s.rules.push(Rule {
                        pat: self.search.clone(),
                        color,
                        ..Default::default()
                    });
                    changed = true;
                }
            });
        });
        if changed {
            self.compile_rules();
        }
        self.wins.rules = open;
    }

    fn frame_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut changed = false;
        let mono = FontId::new(13.0, FontFamily::Monospace);
        tool_window(ctx, t("帧格式"), "frame", &mut open, 440.0, |ui| {
            ui.label(
                RichText::new(t("收到的数据按帧头 + 长度切成一帧一行，并检查校验；校验错的帧标红。适合看二进制协议。"))
                    .small()
                    .color(TEXT3()),
            );
            ui.add_space(4.0);
            let f = &mut self.s.frame;
            changed |= switch_row(ui, &mut f.on, t("按帧格式分帧"), t("关闭时按换行符分行"));
            ui.add_enabled_ui(f.on, |ui| {
                egui::Grid::new("framegrid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(RichText::new(t("帧头")).color(TEXT2()));
                    changed |= ui.add(egui::TextEdit::singleline(&mut f.header).font(mono.clone()).desired_width(160.0).hint_text("AA 55")).changed();
                    ui.end_row();
                    ui.label(RichText::new(t("帧长")).color(TEXT2()));
                    ui.horizontal(|ui| {
                        changed |= ui.selectable_value(&mut f.fixed, false, t("看长度字段")).changed();
                        changed |= ui.selectable_value(&mut f.fixed, true, t("固定长度")).changed();
                    });
                    ui.end_row();
                    if f.fixed {
                        ui.label(RichText::new(t("每帧字节数")).color(TEXT2()));
                        changed |= ui.add(egui::DragValue::new(&mut f.fixed_len).range(1..=4096)).changed();
                        ui.end_row();
                    } else {
                        ui.label(RichText::new(t("长度字段")).color(TEXT2()));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("偏移")).color(TEXT3()));
                            changed |= ui.add(egui::DragValue::new(&mut f.len_off).range(0..=64)).changed();
                            ui.label(RichText::new(t("字节数")).color(TEXT3()));
                            changed |= ui.add(egui::DragValue::new(&mut f.len_size).range(1..=4)).changed();
                            if f.len_size > 1 {
                                changed |= ui.selectable_value(&mut f.len_be, true, t("大端")).changed();
                                changed |= ui.selectable_value(&mut f.len_be, false, t("小端")).changed();
                            }
                        });
                        ui.end_row();
                        ui.label(RichText::new(t("整帧长度")).color(TEXT2()));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("长度字段的值 +")).color(TEXT3()));
                            changed |= ui.add(egui::DragValue::new(&mut f.adjust).range(-64..=1024)).changed();
                        });
                        ui.end_row();
                    }
                    ui.label(RichText::new(t("校验和")).color(TEXT2()));
                    egui::ComboBox::from_id_salt("fcheck").width(150.0).selected_text(check_name(f.check)).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut f.check, Check::None, check_name(Check::None)).changed();
                        for c in checksum::ALL {
                            changed |= ui.selectable_value(&mut f.check, c, c.name()).changed();
                        }
                    });
                    ui.end_row();
                    ui.label(RichText::new(t("半帧超时")).color(TEXT2()));
                    changed |= ui.add(egui::DragValue::new(&mut f.timeout_ms).range(10..=10_000).suffix(" ms")).changed();
                    ui.end_row();
                });
                // 示意：按当前设置画一帧
                ui.add_space(6.0);
                let n = if f.fixed {
                    f.fixed_len as usize
                } else {
                    (f.len_off as usize + f.len_size as usize + 3).max(parse_hex(&f.header).map_or(0, |h| h.len()) + 3)
                };
                let roles = f.roles(n.min(24));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let head = parse_hex(&f.header).unwrap_or_default();
                    for (i, r) in roles.iter().enumerate() {
                        let (txt, c) = match r {
                            0 => (format!("{:02X}", head.get(i).copied().unwrap_or(0)), ACCENT()),
                            1 => ("LL".to_string(), VIOLET()),
                            3 => ("CS".to_string(), OK()),
                            _ => ("··".to_string(), TEXT3()),
                        };
                        ui.label(RichText::new(txt).font(mono.clone()).color(c));
                    }
                });
                ui.horizontal(|ui| {
                    for (c, name) in [(ACCENT(), t("帧头")), (VIOLET(), t("长度")), (OK(), t("校验和"))] {
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 3.5, c);
                        ui.label(RichText::new(name).small().color(TEXT3()));
                    }
                });
            });
        });
        if changed {
            self.apply_frame();
        }
        self.wins.frame = open;
    }

    fn stats_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        tool_window(ctx, t("统计"), "stats", &mut open, 340.0, |ui| {
            let c = self.buf.counts;
            let rx_lines: u64 = c.iter().sum();
            let tx_lines = self.buf.lines.iter().filter(|l| l.dir == Dir::Tx).count();
            let up = self.connected_at.map(|t| t.elapsed().as_secs_f64());
            let row = |ui: &mut egui::Ui, k: &str, v: String, col: Color32| {
                ui.label(RichText::new(k).color(TEXT2()));
                ui.label(RichText::new(v).font(FontId::new(13.0, FontFamily::Monospace)).color(col));
                ui.end_row();
            };
            egui::Grid::new("statsg").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                row(ui, t("接收"), format!("{}  ·  {}", human_bytes(self.rx_bytes), tf("{} 行", &[&group_digits(rx_lines)])), ACCENT());
                row(
                    ui,
                    t("发送"),
                    format!("{}  ·  {}", human_bytes(self.tx_bytes), tf("{} 行", &[&group_digits(tx_lines as u64)])),
                    VIOLET(),
                );
                row(ui, t("当前速率"), format!("{}/s", human_bytes(self.rate as u64)), TEXT());
                row(ui, t("峰值速率"), format!("{}/s", human_bytes(self.stats.peak_rate as u64)), TEXT());
                if let Some(s) = up.filter(|&s| s > 1.0) {
                    row(ui, t("平均速率"), format!("{}/s", human_bytes((self.rx_bytes as f64 / s) as u64)), TEXT());
                }
                row(ui, t("连接时长"), self.uptime(), TEXT());
                for (lv, name, col) in [
                    (Level::Error, "ERROR", ERR()),
                    (Level::Warn, "WARN", WARN()),
                    (Level::Info, "INFO", INFO()),
                    (Level::Debug, "DEBUG", DEBUG()),
                ] {
                    let n = c[lv as usize];
                    let pct = if rx_lines > 0 { n as f64 * 100.0 / rx_lines as f64 } else { 0.0 };
                    row(ui, name, format!("{}  ({pct:.1}%)", group_digits(n)), if n > 0 { col } else { TEXT3() });
                }
                let per_min: u32 = self.stats.err_hist.iter().sum();
                row(ui, t("最近一分钟错误"), per_min.to_string(), if per_min > 0 { ERR() } else { TEXT3() });
            });
            // 错误数的小柱状图（最近 60 秒）
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
            let p = ui.painter();
            p.rect_filled(rect, 4.0, BG2());
            let max = self.stats.err_hist.iter().copied().max().unwrap_or(0).max(1) as f32;
            let w = rect.width() / 60.0;
            for (i, &n) in self.stats.err_hist.iter().enumerate() {
                if n > 0 {
                    let h = (n as f32 / max) * (rect.height() - 6.0);
                    let x = rect.right() - (self.stats.err_hist.len() - i) as f32 * w;
                    p.rect_filled(Rect::from_min_max(pos2(x + 0.5, rect.bottom() - 3.0 - h), pos2(x + w - 0.5, rect.bottom() - 3.0)), 1.0, ERR());
                }
            }
            ui.ctx().request_repaint_after(Duration::from_millis(1000));
        });
        self.wins.stats = open;
    }

    /// 日志工具栏「更多」菜单
    pub(super) fn more_menu(&mut self, ui: &mut egui::Ui) {
        let (k_mark, k_next, k_prev, k_rules) = (
            self.km.label(Act::LogMarker),
            self.km.label(Act::LogNextMark),
            self.km.label(Act::LogPrevMark),
            self.km.label(Act::LogRules),
        );
        ui.set_min_width(240.0);
        if menu_item(ui, t("插入标记"), &k_mark) {
            self.insert_marker();
            ui.close_menu();
        }
        if menu_item(ui, t("下一个书签"), &k_next) {
            self.next_bookmark(false);
            ui.close_menu();
        }
        if menu_item(ui, t("上一个书签"), &k_prev) {
            self.next_bookmark(true);
            ui.close_menu();
        }
        if menu_item(ui, t("复制全部日志"), "") {
            let text = self.export_text();
            ui.ctx().copy_text(text);
            self.toast(t("已复制到剪贴板"), OK());
            ui.close_menu();
        }
        if self.live_conns().len() > 1 && menu_item(ui, t("合并日志（所有连接）…"), "") {
            self.wins.merged = true;
            ui.close_menu();
        }
        ui.separator();
        if menu_item(ui, t("过滤与高亮规则…"), &k_rules) {
            self.wins.rules = true;
            ui.close_menu();
        }
        if menu_item(ui, t("帧格式…"), "") {
            self.wins.frame = true;
            ui.close_menu();
        }
        if menu_item(ui, t("统计…"), "") {
            self.wins.stats = true;
            ui.close_menu();
        }
        if menu_item(ui, t("两个关键字之间的耗时…"), "") {
            self.mini.timing = true;
            ui.close_menu();
        }
        ui.separator();
        if menu_item(ui, t("打开日志文件…"), "") {
            ui.close_menu();
            self.open_log_file(false);
        }
        if menu_item(ui, t("回放日志文件…"), "") {
            ui.close_menu();
            self.open_log_file(true);
        }
        self.tools_menu_items(ui);
    }
}

pub(super) fn rule_act_name(a: RuleAct) -> &'static str {
    match a {
        RuleAct::Show => t("只显示"),
        RuleAct::Hide => t("隐藏"),
        RuleAct::Mark => t("高亮"),
    }
}

pub(super) fn check_name(c: Check) -> &'static str {
    if c == Check::None {
        t("无")
    } else {
        c.name()
    }
}

/// 调试工具统一的小窗口样式
pub(crate) fn tool_window(ctx: &egui::Context, title: &str, id: &str, open: &mut bool, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(title)
        .id(egui::Id::new(("toolwin", id)))
        .open(open)
        .resizable(false)
        .collapsible(false)
        .default_pos(ctx.screen_rect().center() - vec2(width / 2.0, 240.0))
        .frame(egui::Frame::window(&ctx.style()).fill(BG1()).inner_margin(egui::Margin::same(14)))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            add(ui);
        });
}

/// 很小的开关字（如正则的 .*）
pub(crate) fn small_chip(ui: &mut egui::Ui, v: &mut bool, label: &str, tip: &str) -> bool {
    let r = ui
        .add(
            egui::Button::new(RichText::new(label).font(FontId::new(12.0, mono_bold())).color(if *v { ACCENT() } else { TEXT3() }))
                .fill(if *v { ACCENT().gamma_multiply(0.12) } else { Color32::TRANSPARENT })
                .stroke(st(1.0, if *v { ACCENT().gamma_multiply(0.4) } else { LINE() }))
                .min_size(vec2(26.0, 22.0)),
        )
        .on_hover_text(tip);
    if r.clicked() {
        *v = !*v;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::serialx::Profile;
    use std::io::Cursor;

    #[test]
    fn manual_highlight_colors_round_trip_with_legacy_rule_indices() {
        let legacy = r#"{"rules":[{"pat":"warn","color":4}],"keywords":"warn, boot"}"#;
        let mut profile: Profile = serde_json::from_str(legacy).unwrap();
        assert!(profile.keyword_colors.is_empty());
        assert_eq!(profile.rules[0].highlight_color(), ManualColor::Palette(4));
        profile.keyword_colors.insert("warn".to_owned(), ManualColor::Palette(11));
        profile.rules[0].custom_color = Some([12, 34, 56]);
        let saved = serde_json::to_string(&profile).unwrap();
        let restored: Profile = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.keyword_colors, profile.keyword_colors);
        assert_eq!(restored.rules, profile.rules);
    }

    #[test]
    fn custom_highlight_is_readable_on_light_and_dark_log_backgrounds() {
        for background in [LIGHT.bg0, DARK.bg0] {
            for color in [Color32::BLACK, Color32::WHITE, Color32::from_rgb(0x05, 0x12, 0x30)] {
                let fg = readable_highlight(color, background);
                let tinted = background.blend(fg.gamma_multiply(0.16));
                assert!(contrast(luminance(fg), luminance(tinted)) >= 4.5);
            }
        }
    }

    #[test]
    fn log_snapshot_reads_only_requested_bytes_and_whole_tail_lines() {
        let bytes = b"prefix\n\xE6\x97\xA5abc\nlast\r\n";
        let (tail, trimmed) = read_log_window(Cursor::new(bytes), bytes.len() as u64, 12).unwrap();
        assert!(trimmed);
        assert_eq!(tail, b"last\r\n");
        let (whole_lines, trimmed) = read_log_window(Cursor::new(bytes), bytes.len() as u64, 13).unwrap();
        assert!(trimmed);
        assert_eq!(whole_lines, b"\xE6\x97\xA5abc\nlast\r\n");
        let (snapshot, trimmed) = read_log_window(Cursor::new(bytes), 7, 15).unwrap();
        assert!(!trimmed);
        assert_eq!(snapshot, b"prefix\n");
    }

    #[test]
    fn open_keeps_latest_rows_and_skips_oversized_lines() {
        let mut raw = b"older\r\n".to_vec();
        raw.extend(std::iter::repeat_n(b'x', OPEN_LINE_BYTES + 1));
        raw.extend_from_slice(b"\n12:34:56.789  TX \xE2\x80\xBA last\r\n");
        let PreparedLog::Open { items, tail_only, skipped_long, .. } = parse_open_log("test".into(), &raw, false, 2) else {
            panic!("expected open result");
        };
        assert!(tail_only);
        assert_eq!(skipped_long, 1);
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].1, items[0].2.as_slice()), (Dir::Tx, &b"last"[..]));
    }

    #[test]
    fn replay_stream_preserves_time_direction_and_snapshot_progress() {
        let data = b"23:59:59.950  lead\r\n23:59:59.990  TX \xE2\x80\xBA cmd\n00:00:00.010  B \xE2\x80\xBA next\ncontinuation";
        let (tx, rx) = mpsc::sync_channel(REPLAY_BUFFER_LINES);
        stream_replay(Cursor::new(data), tx).unwrap();
        let rows: Vec<_> = rx.into_iter().map(Result::unwrap).collect();
        assert_eq!(rows.iter().map(|p| p.at).collect::<Vec<_>>(), vec![0, 40, 60, 60]);
        assert_eq!(rows.iter().map(|p| p.read).sum::<u64>(), data.len() as u64);
        assert_eq!(rows[2].line.1, Dir::Rx2);
        assert_eq!(rows[2].line.2, b"next");
    }

    #[test]
    fn replay_stops_reading_when_cancelled_or_a_line_is_too_long() {
        let (tx, rx) = mpsc::sync_channel(REPLAY_BUFFER_LINES);
        drop(rx);
        let mut cancelled = Cursor::new(b"one\ntwo\n");
        stream_replay(&mut cancelled, tx).unwrap();
        assert_eq!(cancelled.position(), 4);

        let (tx, _rx) = mpsc::sync_channel(REPLAY_BUFFER_LINES);
        let mut long = Cursor::new(vec![b'x'; REPLAY_LINE_BYTES + 100]);
        assert!(stream_replay(&mut long, tx).is_err());
        assert_eq!(long.position(), (REPLAY_LINE_BYTES + 3) as u64);
    }

    #[test]
    fn log_lines() {
        use chrono::Timelike;
        let (ts, d, s) = parse_log_line("12:34:56.789  TX › AT+GMR");
        assert_eq!(ts.unwrap().second(), 56);
        assert_eq!(d, Dir::Tx);
        assert_eq!(s, "AT+GMR");
        let (ts, d, s) = parse_log_line("2026-10-27 08:00:01.500 RB hello");
        assert_eq!(ts.unwrap().minute(), 0);
        assert_eq!((d, s), (Dir::Rx2, "hello"));
        let (ts, d, s) = parse_log_line("2026-10-27 08:00:01.500 TX AT+GMR");
        assert!(ts.is_some());
        assert_eq!(d, Dir::Tx);
        assert_eq!(s, "AT+GMR");
        let (ts, d, s) = parse_log_line("plain line");
        assert!(ts.is_none());
        assert_eq!((d, s), (Dir::Rx, "plain line"));
        assert_eq!(fmt_delta(12), "+  12ms ");
        assert_eq!(fmt_delta(1500), "+ 1.50s ");
        assert_eq!(fmt_delta(1500).chars().count(), fmt_delta(12).chars().count());
        let f = FrameCfg::default();
        assert_eq!(f.roles(6), vec![0, 0, 1, 2, 2, 3]);
    }

    #[test]
    fn recording_parts_round_trip_through_replay() {
        let dir = crate::record::tests::TempDir::new();
        let base = dir.0.join("session.log");
        let next = dir.0.join("session_002.log");
        let mut buf = LogBuf::new(10);
        buf.recording_options(ViewMode::Text, 1, 0);
        buf.start_recording(base.clone(), Encoding::Utf8).unwrap();
        let expected = [
            (Dir::Rx, "温".repeat(200 * 1024)),
            (Dir::Tx, "AT".to_owned()),
            (Dir::Rx2, "B".repeat(600 * 1024)),
            (Dir::Sys, "event".to_owned()),
        ];
        for (dir, s) in &expected {
            buf.push_line(*dir, s.as_bytes().to_vec(), Encoding::Utf8);
        }
        assert_eq!(buf.stop_recording(Encoding::Utf8).unwrap(), Some(next.clone()));

        let mut replayed = Vec::new();
        for part in [base, next] {
            let (file, len, _) = open_log_source(&part).unwrap();
            let (tx, rx) = mpsc::sync_channel(REPLAY_BUFFER_LINES);
            stream_replay(BufReader::new(file.take(len)), tx).unwrap();
            let mut read = 0;
            for event in rx {
                let event = event.unwrap();
                let (ts, dir, body) = event.line;
                assert!(ts.is_some());
                read += event.read;
                replayed.push((dir, String::from_utf8(body).unwrap()));
            }
            assert_eq!(read, len);
        }
        assert_eq!(replayed, expected);
    }
}

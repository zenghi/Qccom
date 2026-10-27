//! 日志增强：过滤 / 高亮规则、时间差、书签和标记、统计、打开和回放日志文件、分帧设置。
//! 平时不占界面：规则和帧格式在小窗口里改，其余在日志工具栏的「更多」菜单和命令面板里。

use super::*;
use crate::checksum::{self, Check};
use crate::logbuf::{Framer, LenSpec};
use chrono::{NaiveDateTime, NaiveTime};

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
    /// 高亮颜色（kw_colors 的下标）
    pub color: u8,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            on: true,
            pat: String::new(),
            regex: false,
            act: RuleAct::Mark,
            color: 0,
        }
    }
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
    /// (相对第一行的毫秒, 方向, 内容)
    items: VecDeque<(i64, Dir, Vec<u8>)>,
    total: usize,
    start: Instant,
    pub speed: f32,
    /// 暂停时累计的“回放时间”（毫秒）
    played: f64,
    last: Instant,
    pub paused: bool,
    pub name: String,
}

impl Replay {
    pub(super) fn progress(&self) -> f32 {
        1.0 - self.items.len() as f32 / self.total.max(1) as f32
    }
}

/// 解析日志文件的一行。认得两种格式：
/// 保存 / 录制的 "2026-10-27 12:34:56.789 RX 内容"（RX / TX / RB / ## 表示方向），
/// 以及从界面复制的 "12:34:56.789  TX › 内容"（"◆ " 是系统消息，"B › " 是监听口 B）。
fn parse_log_line(line: &str) -> (Option<NaiveDateTime>, Dir, &str) {
    let today = chrono::Local::now().date_naive();
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
            ts = Some(today.and_time(t));
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
}

impl App {
    // ------------------------------------------------------------ 规则

    pub(super) fn compile_rules(&mut self) {
        let cols = kw_colors();
        self.rules_rt = self
            .s
            .rules
            .iter()
            .filter(|r| r.on && !r.pat.is_empty())
            .filter_map(|r| {
                Matcher::new(&r.pat, r.regex).ok().map(|m| RuleRt {
                    act: r.act,
                    color: cols[r.color as usize % cols.len()],
                    m,
                })
            })
            .collect();
        self.view_dirty = true;
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
        let Some(path) = rfd::FileDialog::new().add_filter(t("日志"), &["log", "txt"]).add_filter(t("所有文件"), &["*"]).pick_file() else {
            return;
        };
        self.load_log_path(&path, replay);
    }

    /// 打开（或回放）一个日志文件：菜单、拖放、命令行 --open / --replay 都走这里
    pub(super) fn load_log_path(&mut self, path: &std::path::Path, replay: bool) {
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                self.toast(tf("无法打开：{}", &[&e]), ERR());
                return;
            }
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let parsed: Vec<(Option<NaiveDateTime>, Dir, &str)> = text.lines().map(parse_log_line).collect();
        if replay {
            let first = parsed.iter().find_map(|p| p.0);
            let mut items = VecDeque::new();
            let mut last = 0i64;
            for (ts, dir, s) in &parsed {
                // 没有时间戳的行沿用上一行的时间
                let at = match (ts, first) {
                    (Some(t), Some(f)) => {
                        let mut d = (*t - f).num_milliseconds();
                        if d < last - 43_200_000 {
                            d += 86_400_000; // 跨过午夜
                        }
                        d.max(last)
                    }
                    _ => last,
                };
                last = at;
                items.push_back((at, *dir, s.as_bytes().to_vec()));
            }
            let total = items.len();
            self.buf.sys(tf("开始回放 {}（{} 行）", &[&name, &total]));
            self.replay = Some(Replay {
                items,
                total,
                start: Instant::now(),
                speed: 1.0,
                played: 0.0,
                last: Instant::now(),
                paused: false,
                name,
            });
            return;
        }
        self.buf.sys(tf("打开日志文件 {}（{} 行）", &[&name, &parsed.len()]));
        for (ts, dir, s) in parsed {
            let ts = ts.and_then(|t| t.and_local_timezone(chrono::Local).single());
            self.buf.push_loaded(dir, s.as_bytes().to_vec(), self.s.encoding, ts);
        }
        self.view_dirty = true;
        self.jump_bottom = true;
    }

    pub(super) fn tick_replay(&mut self) {
        let Some(r) = self.replay.as_mut() else { return };
        let now = Instant::now();
        if !r.paused {
            r.played += now.duration_since(r.last).as_secs_f64() * 1000.0 * r.speed as f64;
        }
        r.last = now;
        let _ = r.start;
        let mut due = vec![];
        // 每帧最多放 2000 行，免得最高速时卡住界面
        while due.len() < 2000 && r.items.front().is_some_and(|(at, _, _)| (*at as f64) <= r.played || r.speed >= 1000.0) {
            due.push(r.items.pop_front().unwrap());
        }
        let next = r.items.front().map(|(at, _, _)| ((*at as f64 - r.played) / r.speed as f64).max(1.0));
        let done = r.items.is_empty();
        for (_, dir, d) in due {
            match dir {
                Dir::Rx => {
                    // 只进日志，不算进收发统计、不转发给网络共享和工具
                    let mut d = d;
                    d.push(b'\n');
                    if self.paused {
                        self.pause_buf.extend_from_slice(&d);
                    } else {
                        self.buf.push_rx(&d, self.s.encoding);
                    }
                }
                Dir::Rx2 => self.on_sniff_data([d, b"\n".to_vec()].concat()),
                Dir::Tx => self.buf.push_line(Dir::Tx, d, self.s.encoding),
                Dir::Sys => self.buf.sys(String::from_utf8_lossy(&d).into_owned()),
            }
        }
        if done {
            let name = self.replay.take().map(|r| r.name).unwrap_or_default();
            self.buf.sys(tf("回放结束：{}", &[&name]));
        } else if let Some(ms) = next {
            self.ctx.request_repaint_after(Duration::from_millis(ms.min(500.0) as u64));
        }
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
    }

    fn rules_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut changed = false;
        tool_window(ctx, t("过滤与高亮规则"), "rules", &mut open, 520.0, |ui| {
            ui.label(RichText::new(t("「只显示」规则之间是“或”；「隐藏」优先。规则只影响显示，不影响录制。")).small().color(TEXT3()));
            ui.add_space(4.0);
            let cols = kw_colors();
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
                    // 颜色：点一下换下一个
                    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    ui.painter().circle_filled(rect.center(), 6.0, cols[r.color as usize % cols.len()]);
                    if resp.on_hover_text(t("换颜色")).clicked() {
                        r.color = (r.color + 1) % cols.len() as u8;
                        changed = true;
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
                    let n = self.s.rules.len() as u8;
                    self.s.rules.push(Rule { color: n % 6, ..Default::default() });
                }
                // 把搜索框里的词变成规则
                if !self.search.is_empty() && ui.button(tf("把“{}”加为规则", &[&self.search])).clicked() {
                    let n = self.s.rules.len() as u8;
                    self.s.rules.push(Rule {
                        pat: self.search.clone(),
                        color: n % 6,
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
            ui.label(RichText::new(t("收到的数据按帧头 + 长度切成一帧一行，并检查校验；校验错的帧标红。适合看二进制协议。")).small().color(TEXT3()));
            ui.add_space(4.0);
            let f = &mut self.s.frame;
            changed |= switch_row(ui, &mut f.on, t("按帧格式分帧"), t("关闭时按换行符分行"));
            ui.add_enabled_ui(f.on, |ui| {
                egui::Grid::new("framegrid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(RichText::new(t("帧头")).color(TEXT2()));
                    changed |= ui
                        .add(egui::TextEdit::singleline(&mut f.header).font(mono.clone()).desired_width(160.0).hint_text("AA 55"))
                        .changed();
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
                let n = if f.fixed { f.fixed_len as usize } else { (f.len_off as usize + f.len_size as usize + 3).max(parse_hex(&f.header).map_or(0, |h| h.len()) + 3) };
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
                row(ui, t("发送"), format!("{}  ·  {}", human_bytes(self.tx_bytes), tf("{} 行", &[&group_digits(tx_lines as u64)])), VIOLET());
                row(ui, t("当前速率"), format!("{}/s", human_bytes(self.rate as u64)), TEXT());
                row(ui, t("峰值速率"), format!("{}/s", human_bytes(self.stats.peak_rate as u64)), TEXT());
                if let Some(s) = up.filter(|&s| s > 1.0) {
                    row(ui, t("平均速率"), format!("{}/s", human_bytes((self.rx_bytes as f64 / s) as u64)), TEXT());
                }
                row(ui, t("连接时长"), self.uptime(), TEXT());
                for (lv, name, col) in [(Level::Error, "ERROR", ERR()), (Level::Warn, "WARN", WARN()), (Level::Info, "INFO", INFO()), (Level::Debug, "DEBUG", DEBUG())] {
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
        let (k_mark, k_next, k_prev, k_rules) = (self.km.label(Act::LogMarker), self.km.label(Act::LogNextMark), self.km.label(Act::LogPrevMark), self.km.label(Act::LogRules));
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
}

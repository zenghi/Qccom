//! 可复用的终端视图：串口的终端模式和内置命令行终端共用。
//! 负责画屏幕、回滚、拖选复制、键盘输入转字节；数据从哪来、发到哪去由调用方决定。

use std::time::{Duration, Instant};

use eframe::egui::{self, epaint, pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Shape, TextStyle};

use crate::app::{hgrad_line, hud_corners};
use crate::i18n::t;
use crate::term::{self, Term};
use crate::theme::*;

#[derive(Default)]
pub struct TermOut {
    /// 要发给对端的字节
    pub input: Vec<u8>,
    pub toast: Option<&'static str>,
    pub focused: bool,
    /// Ctrl+点击的 `文件:行号`
    pub open: Option<(String, usize)>,
}

pub struct TermView {
    pub term: Term,
    /// 往回翻了多少行（0 = 最新）
    pub scroll: usize,
    pub sel: Option<((usize, usize), (usize, usize))>,
    wheel: f32,
    pub blink_at: f64,
    pub focus_req: bool,
    id: egui::Id,
    /// 用哪一处的背景图
    pub bg: crate::bgimg::Slot,
    /// 字号倍数（Ctrl+滚轮）
    pub zoom: f32,
    /// 命令行终端（本地 shell / SSH）：Ctrl+F 查找、Ctrl+↑↓ 在命令之间跳、记录命令块
    pub blocks: bool,
    /// 输入法组字状态
    ime: crate::ime::Ime,
    /// 命令块：每次回车时光标所在的行（稳定行号，见 Screen::dropped）
    pub marks: Vec<u64>,
    /// 最近一条命令什么时候发出、最后一次有输出是什么时候（判断长命令跑完）
    cmd_at: Option<Instant>,
    last_out: Option<Instant>,
    find: Option<Find>,
}

#[derive(Default)]
struct Find {
    q: String,
    focus: bool,
    /// (稳定行号, 起始列, 占几列)
    hits: Vec<(u64, usize, usize)>,
    cur: usize,
    key: (String, u64, usize),
    at: Option<Instant>,
    /// 下一帧要滚到当前命中
    jump: bool,
}

impl TermView {
    pub fn new(id: impl std::hash::Hash, cols: usize, rows: usize) -> Self {
        Self {
            term: Term::new(cols, rows),
            scroll: 0,
            sel: None,
            wheel: 0.0,
            blink_at: 0.0,
            focus_req: false,
            id: egui::Id::new(id),
            bg: crate::bgimg::Slot::Serial,
            zoom: 1.0,
            blocks: false,
            ime: Default::default(),
            marks: vec![],
            cmd_at: None,
            last_out: None,
            find: None,
        }
    }

    /// 长命令跑完了（发出后超过 8 秒，且已经 2.5 秒没有新输出）：返回用时，只报一次
    pub fn take_done(&mut self) -> Option<Duration> {
        let (at, last) = (self.cmd_at?, self.last_out?);
        if last > at && last.elapsed() > Duration::from_millis(2500) {
            self.cmd_at = None;
            let d = last - at;
            return (d > Duration::from_secs(8)).then_some(d);
        }
        None
    }

    /// 全部内容（回滚 + 屏幕），保存输出用
    pub fn all_text(&self) -> String {
        let s = &self.term.s;
        let mut lines: Vec<String> = s.scrollback.iter().chain(s.grid.iter()).map(|r| term::row_text(r, 0, r.len())).collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// 光标所在行的稳定行号
    pub fn cursor_row(&self) -> u64 {
        let s = &self.term.s;
        s.dropped + (s.scrollback.len() + s.y) as u64
    }

    /// 从某一行（稳定行号）到最后的文字
    pub fn text_since(&self, row: u64) -> String {
        let s = &self.term.s;
        let from = row.saturating_sub(s.dropped) as usize;
        let mut lines: Vec<String> = s.scrollback.iter().chain(s.grid.iter()).skip(from).map(|r| term::row_text(r, 0, r.len())).collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn stable(&self, abs: usize) -> u64 {
        self.term.s.dropped + abs as u64
    }

    /// 某个稳定行号对应的绝对行（已经被挤出回滚区就是 None）
    fn abs_of(&self, row: u64) -> Option<usize> {
        row.checked_sub(self.term.s.dropped).map(|r| r as usize)
    }

    fn scroll_to_abs(&mut self, abs: usize) {
        let sbl = self.term.s.scrollback.len();
        let top = abs.saturating_sub(self.term.s.rows / 2);
        self.scroll = sbl.saturating_sub(top).min(sbl);
    }

    /// Ctrl+↑ / Ctrl+↓：跳到上一条 / 下一条命令
    fn jump_mark(&mut self, up: bool) {
        let sbl = self.term.s.scrollback.len();
        let top = self.stable(sbl - self.scroll.min(sbl));
        let target = if up {
            self.marks.iter().rev().find(|&&m| m < top).copied()
        } else {
            self.marks.iter().find(|&&m| m > top).copied()
        };
        match target.and_then(|m| self.abs_of(m)) {
            Some(abs) => {
                let sbl = self.term.s.scrollback.len();
                self.scroll = sbl.saturating_sub(abs).min(sbl);
            }
            None if !up => self.scroll = 0,
            None => {}
        }
    }

    /// 某条命令块的输出（从这条命令到下一条命令之前）
    fn block_text(&self, mark: u64) -> String {
        let s = &self.term.s;
        let total = s.scrollback.len() + s.rows;
        let Some(a) = self.abs_of(mark) else { return String::new() };
        let next = self.marks.iter().find(|&&m| m > mark).and_then(|&m| self.abs_of(m)).unwrap_or(total);
        let mut out = vec![];
        for abs in a..next.min(total) {
            if let Some(r) = self.row(abs) {
                out.push(term::row_text(r, 0, r.len()));
            }
        }
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        out.join("\n")
    }

    fn refresh_find(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let s = &self.term.s;
        let key = (f.q.clone(), s.dropped, s.scrollback.len());
        if key == f.key && f.at.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        let first = f.key.0 != f.q;
        f.key = key;
        f.at = Some(Instant::now());
        let q: Vec<char> = f.q.to_lowercase().chars().collect();
        let mut hits = vec![];
        if !q.is_empty() {
            for (abs, row) in s.scrollback.iter().chain(s.grid.iter()).enumerate() {
                let mut text = Vec::with_capacity(row.len());
                let mut cols = Vec::with_capacity(row.len());
                for (c, cell) in row.iter().enumerate() {
                    if !cell.wide_tail {
                        text.extend(cell.ch.to_lowercase().next());
                        cols.push(c);
                    }
                }
                if text.len() < q.len() {
                    continue;
                }
                let mut i = 0;
                while i + q.len() <= text.len() {
                    if text[i..i + q.len()] == q[..] {
                        let last = cols[i + q.len() - 1];
                        let w = if term::is_wide(row[last].ch) { 2 } else { 1 };
                        hits.push((s.dropped + abs as u64, cols[i], last + w - cols[i]));
                        i += q.len();
                    } else {
                        i += 1;
                    }
                }
            }
        }
        // 新的查找词：从最新（最下面）的命中开始
        if first {
            f.cur = hits.len().saturating_sub(1);
            f.jump = !hits.is_empty();
        } else {
            f.cur = f.cur.min(hits.len().saturating_sub(1));
        }
        f.hits = hits;
    }

    pub fn open_find(&mut self) {
        let f = self.find.get_or_insert_with(Find::default);
        f.focus = true;
        if let Some(sel) = self.sel_text().filter(|t| !t.contains('\n') && !t.trim().is_empty()) {
            let f = self.find.as_mut().unwrap();
            f.q = sel;
        }
    }

    pub fn id(&self) -> egui::Id {
        self.id
    }

    /// 喂数据；正在往回翻时保持视图不动。返回终端要回给对端的应答（如光标位置查询）
    pub fn feed(&mut self, d: &[u8]) -> Vec<u8> {
        if self.cmd_at.is_some() {
            self.last_out = Some(Instant::now());
        }
        let before = self.term.s.scrollback.len();
        self.term.feed(d);
        if self.scroll > 0 {
            let grown = self.term.s.scrollback.len().saturating_sub(before);
            self.scroll = (self.scroll + grown).min(self.term.s.scrollback.len());
        }
        std::mem::take(&mut self.term.s.reply)
    }

    pub fn reset(&mut self) {
        self.term.reset();
        self.scroll = 0;
        self.sel = None;
        self.marks.clear();
        self.find = None;
    }

    fn row(&self, abs: usize) -> Option<&term::Row> {
        let sb = &self.term.s.scrollback;
        if abs < sb.len() {
            sb.get(abs)
        } else {
            self.term.s.grid.get(abs - sb.len())
        }
    }

    pub fn screen_text(&self) -> String {
        let s = &self.term.s;
        let mut lines: Vec<String> = s.grid.iter().map(|r| term::row_text(r, 0, s.cols)).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    pub fn sel_text(&self) -> Option<String> {
        let (a, b) = self.sel?;
        let (a, b) = (a.min(b), a.max(b));
        let mut out = vec![];
        for abs in a.0..=b.0 {
            let row = self.row(abs)?;
            let from = if abs == a.0 { a.1 } else { 0 };
            let to = if abs == b.0 { b.1 + 1 } else { row.len() };
            out.push(term::row_text(row, from, to));
        }
        Some(out.join("\n"))
    }

    /// 键盘、粘贴 → 发给设备
    fn input(&mut self, ui: &egui::Ui, enc: &dyn Fn(&str) -> Vec<u8>, o: &mut TermOut) {
        let (events, shift, time) = ui.input(|i| (i.events.clone(), i.modifiers.shift, i.time));
        let events = self.ime.filter(events);
        let app_cursor = self.term.s.app_cursor;
        let mut out = Vec::new();
        for e in events {
            match e {
                egui::Event::Text(t) => out.extend(enc(&t)),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if self.blocks && modifiers.command && !modifiers.shift && !modifiers.alt {
                        match key {
                            egui::Key::F => {
                                self.open_find();
                                continue;
                            }
                            egui::Key::ArrowUp | egui::Key::ArrowDown => {
                                self.jump_mark(key == egui::Key::ArrowUp);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if let Some(b) = term::key_bytes(key, modifiers, app_cursor) {
                        out.extend(b);
                    }
                }
                egui::Event::Copy => {
                    // Ctrl+Shift+C 复制选中；Ctrl+C 照常发 ^C 给设备
                    match self.sel_text().filter(|_| shift) {
                        Some(tv) => {
                            ui.ctx().copy_text(tv);
                            o.toast = Some(t("已复制"));
                        }
                        None => out.push(0x03),
                    }
                }
                egui::Event::Cut => out.push(0x18),
                egui::Event::Paste(p) => out.extend(enc(&p.replace('\n', "\r"))),
                _ => {}
            }
        }
        if !out.is_empty() {
            // 命令块：回车时记下光标所在行（全屏程序里的回车不算）
            if self.blocks && out.contains(&b'\r') && !self.term.s.in_alt() {
                let s = &self.term.s;
                let row = s.dropped + (s.scrollback.len() + s.y) as u64;
                if self.marks.last() != Some(&row) {
                    self.marks.push(row);
                }
                let dropped = s.dropped;
                self.marks.retain(|&m| m >= dropped);
                self.cmd_at = Some(std::time::Instant::now());
                self.last_out = None;
            }
            self.scroll = 0;
            self.sel = None;
            self.blink_at = time;
            o.input.extend(out);
        }
    }

    /// 画终端并处理输入。enc 把文字编成发给对端的字节；empty 是空屏时（未聚焦, 已聚焦）的提示；
    /// links 打开后按住 Ctrl 悬停 `文件:行号` 会加下划线，点击后通过 TermOut::open 交给调用方
    pub fn show(&mut self, ui: &mut egui::Ui, enc: &dyn Fn(&str) -> Vec<u8>, empty: Option<[(&str, &str); 2]>, links: bool) -> TermOut {
        let mut o = TermOut::default();
        let card_rect = ui.max_rect();
        let id = self.id;
        let resp = ui.interact(card_rect, id, Sense::click_and_drag());
        if self.focus_req {
            resp.request_focus();
            self.focus_req = false;
        }
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        if focused {
            // Tab、方向键、Esc 也交给终端，不用来切换焦点
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.input(ui, enc, &mut o);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }

        let painter = ui.painter_at(card_rect);
        painter.rect(
            card_rect,
            R_CARD,
            BG0(),
            st(1.0, if focused { ACCENT().gamma_multiply(0.35) } else { LINE() }),
            egui::StrokeKind::Inside,
        );
        crate::bgimg::paint(&painter, card_rect.shrink(1.0), self.bg);
        hgrad_line(&painter, card_rect.top() + 0.5, card_rect.x_range(), ACCENT().gamma_multiply(if focused { 0.8 } else { 0.4 }));

        // 按可用空间算出行列数
        if resp.hovered() {
            let z = ui.input(|i| i.zoom_delta());
            if z != 1.0 {
                self.zoom = (self.zoom * z).clamp(0.6, 2.5);
            }
        }
        let mut font = TextStyle::Monospace.resolve(ui.style());
        font.size = (font.size * self.zoom).round().max(7.0);
        let bold_font = FontId::new(font.size, mono_bold());
        // 用一长串字符量出实际步进（排版时会按像素取整，单个字宽乘列数会越画越偏）
        let cw = ui.fonts(|f| f.layout_no_wrap("0".repeat(100), font.clone(), TEXT()).size().x / 100.0);
        let rh = (ui.fonts(|f| f.row_height(&font)) + 2.0).round();
        let inner = card_rect.shrink2(vec2(12.0, 10.0));
        let cols = ((inner.width() - 10.0) / cw).floor().max(0.0) as usize;
        let rows = (inner.height() / rh).floor().max(0.0) as usize;
        if cols >= 10 && rows >= 3 && (cols, rows) != (self.term.s.cols, self.term.s.rows) {
            self.term.s.resize(cols, rows);
        }

        // 滚轮翻看历史
        let sbl = self.term.s.scrollback.len();
        if resp.hovered() {
            self.wheel += ui.input(|i| i.smooth_scroll_delta.y);
            let n = (self.wheel / rh).trunc();
            if n != 0.0 {
                self.wheel -= n * rh;
                self.scroll = (self.scroll as i64 + n as i64 * 3).clamp(0, sbl as i64) as usize;
            }
        }
        self.scroll = self.scroll.min(sbl);
        let top_abs = sbl - self.scroll;
        let (tcols, trows) = (self.term.s.cols, self.term.s.rows);
        let origin = inner.min;
        let cell_at = |p: egui::Pos2| {
            let c = ((p.x - origin.x) / cw).floor().clamp(0.0, (tcols - 1) as f32) as usize;
            let r = ((p.y - origin.y) / rh).floor().clamp(0.0, (trows - 1) as f32) as usize;
            (top_abs + r, c)
        };

        // Ctrl+悬停 `文件:行号`：下划线，点击打开
        if resp.hovered() && ui.input(|i| i.modifiers.command) {
            if let Some(p) = resp.hover_pos() {
                let (abs, c) = cell_at(p);
                if let Some(row) = self.row(abs) {
                    let chars: Vec<char> = row.iter().map(|c| if c.wide_tail { '\u{0}' } else { c.ch }).collect();
                    if let Some((a, b, url)) = find_url(&chars, c) {
                        let y = origin.y + (abs - top_abs) as f32 * rh + rh - 2.0;
                        ui.painter().hline(origin.x + a as f32 * cw..=origin.x + b as f32 * cw, y, st(1.0, ACCENT()));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            crate::app::open_url(&url);
                        }
                    } else if let Some((a, b, file, line)) = find_link(&chars, c).filter(|_| links) {
                        let y = origin.y + (abs - top_abs) as f32 * rh + rh - 2.0;
                        ui.painter().hline(origin.x + a as f32 * cw..=origin.x + b as f32 * cw, y, st(1.0, ACCENT()));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        if resp.clicked() {
                            o.open = Some((file, line));
                        }
                    }
                }
            }
        }

        // 鼠标拖动选择，松开自动复制
        if resp.drag_started() {
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                let c = cell_at(p);
                self.sel = Some((c, c));
            }
        }
        if resp.dragged() {
            if let (Some((a, _)), Some(p)) = (self.sel, resp.interact_pointer_pos()) {
                self.sel = Some((a, cell_at(p)));
            }
        }
        if resp.drag_stopped() {
            if let Some(tv) = self.sel_text().filter(|tv| !tv.trim().is_empty()) {
                ui.ctx().copy_text(tv);
                o.toast = Some(t("已复制选中文本"));
            }
        }
        if resp.clicked() {
            self.sel = None;
        }
        let mut paste = None;
        let mut reset = false;
        let mut find_open = false;
        let mut save = false;
        resp.context_menu(|ui| {
            if self.sel.is_some() && ui.button(t("复制选中")).clicked() {
                if let Some(t) = self.sel_text() {
                    ui.ctx().copy_text(t);
                }
                ui.close_menu();
            }
            if ui.button(t("粘贴")).clicked() {
                paste = Some(());
                ui.close_menu();
            }
            if ui.button(t("复制整屏")).clicked() {
                ui.ctx().copy_text(self.screen_text());
                ui.close_menu();
            }
            if ui.button(t("查找…")).clicked() {
                find_open = true;
                ui.close_menu();
            }
            if ui.button(t("保存输出…")).clicked() {
                save = true;
                ui.close_menu();
            }
            if self.zoom != 1.0 && ui.button(t("恢复默认字号")).clicked() {
                self.zoom = 1.0;
                ui.close_menu();
            }
            ui.separator();
            if ui.button(t("清屏并重置终端")).clicked() {
                reset = true;
                ui.close_menu();
            }
        });
        if paste.is_some() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.focus_req = true;
        }
        if reset {
            self.reset();
        }
        if find_open {
            self.open_find();
        }
        if save {
            let name = chrono::Local::now().format("terminal-%Y%m%d-%H%M%S.txt").to_string();
            if let Some(p) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                o.toast = Some(match std::fs::write(&p, self.all_text()) {
                    Ok(_) => t("已保存"),
                    Err(_) => t("保存失败"),
                });
            }
        }
        self.refresh_find();
        if let Some(f) = self.find.as_mut() {
            if f.jump {
                f.jump = false;
                if let Some(&(row, _, _)) = f.hits.get(f.cur) {
                    if let Some(abs) = row.checked_sub(self.term.s.dropped) {
                        let abs = abs as usize;
                        let sbl = self.term.s.scrollback.len();
                        let top = sbl - self.scroll.min(sbl);
                        if abs < top || abs >= top + trows {
                            self.scroll_to_abs(abs);
                        }
                    }
                }
            }
        }
        self.scroll = self.scroll.min(sbl);
        let top_abs = sbl - self.scroll;

        // 画屏幕
        let sel = self.sel.map(|(a, b)| (a.min(b), a.max(b)));
        let sel_bg = ACCENT().gamma_multiply(0.28);
        let mut blank = sbl == 0;
        let mut mark_rows: Vec<(u64, Rect)> = vec![];
        for r in 0..trows {
            let abs = top_abs + r;
            let Some(row) = self.row(abs) else { continue };
            let y = origin.y + r as f32 * rh;
            let stable = self.term.s.dropped + abs as u64;
            if let Some(f) = &self.find {
                let i0 = f.hits.partition_point(|h| h.0 < stable);
                for (k, h) in f.hits.iter().enumerate().skip(i0).take_while(|(_, h)| h.0 == stable) {
                    let hr = Rect::from_min_size(pos2(origin.x + h.1 as f32 * cw, y + 1.0), vec2(h.2 as f32 * cw, rh - 2.0));
                    let cur = k == f.cur;
                    painter.rect_filled(hr, 2.0, if cur { WARN().gamma_multiply(0.55) } else { WARN().gamma_multiply(0.22) });
                    if cur {
                        painter.rect_stroke(hr, 2.0, st(1.0, WARN()), egui::StrokeKind::Outside);
                    }
                }
            }
            // 命令块：每条命令上方一道细线，左边一个小标记
            if self.blocks && self.marks.binary_search(&stable).is_ok() {
                if r > 0 {
                    painter.hline(card_rect.x_range().shrink(8.0), y - 0.5, st(1.0, LINE()));
                }
                mark_rows.push((stable, Rect::from_min_size(pos2(card_rect.left() + 3.0, y + 3.0), vec2(4.0, rh - 6.0))));
            }
            let mut run = String::new();
            let mut run_x = 0usize;
            let mut run_key: Option<(Color32, bool)> = None;
            let flush = |run: &mut String, run_x: usize, key: Option<(Color32, bool)>| {
                if let Some((c, b)) = key {
                    if !run.trim().is_empty() {
                        let f = if b { &bold_font } else { &font };
                        painter.text(pos2(origin.x + run_x as f32 * cw, y + rh / 2.0), Align2::LEFT_CENTER, run.as_str(), f.clone(), c);
                    }
                }
                run.clear();
            };
            for (c, cell) in row.iter().enumerate() {
                if cell.wide_tail {
                    continue;
                }
                let wide = term::is_wide(cell.ch);
                let w = if wide { 2.0 } else { 1.0 };
                let st_ = cell.style;
                let (mut fg, mut bg) = (st_.fg.unwrap_or(if st_.bold { TEXT() } else { LOG_TEXT() }), st_.bg);
                if st_.inverse {
                    (fg, bg) = (bg.unwrap_or(BG0()), Some(fg));
                }
                if bg.is_none() {
                    fg = ink(fg);
                }
                let in_sel = sel.is_some_and(|(a, b)| (abs, c) >= a && (abs, c) <= b);
                let cell_rect = Rect::from_min_size(pos2(origin.x + c as f32 * cw, y), vec2(cw * w, rh));
                if let Some(b) = bg {
                    painter.rect_filled(cell_rect, 0.0, b);
                }
                if in_sel {
                    painter.rect_filled(cell_rect, 0.0, sel_bg);
                }
                if cell.ch != ' ' {
                    blank = false;
                }
                if st_.underline {
                    painter.hline(cell_rect.x_range(), cell_rect.bottom() - 1.5, st(1.0, fg));
                }
                // ASCII 连成一段画；中文、线框等非 ASCII 字符按格子单独放，保证对齐
                if cell.ch.is_ascii() {
                    let key = Some((fg, st_.bold));
                    if key != run_key {
                        flush(&mut run, run_x, run_key);
                        run_key = key;
                        run_x = c;
                    }
                    run.push(cell.ch);
                } else {
                    flush(&mut run, run_x, run_key);
                    run_key = None;
                    if !draw_block_char(&painter, cell_rect, cell.ch, fg) {
                        let f = if st_.bold { &bold_font } else { &font };
                        painter.text(cell_rect.center(), Align2::CENTER_CENTER, cell.ch, f.clone(), fg);
                    }
                }
            }
            flush(&mut run, run_x, run_key);
        }

        // 光标：获得焦点时闪烁的实心块，失去焦点时空心框
        let s = &self.term.s;
        if self.scroll == 0 && s.cursor_visible {
            let wide = s.grid.get(s.y).and_then(|r| r.get(s.x)).is_some_and(|c| term::is_wide(c.ch));
            let cr = Rect::from_min_size(pos2(origin.x + s.x as f32 * cw, origin.y + s.y as f32 * rh), vec2(cw * if wide { 2.0 } else { 1.0 }, rh));
            if focused {
                // 输入法：候选框跟着光标，正在输入的拼音画在光标处
                crate::ime::Ime::allow(ui, card_rect, cr);
            }
            if focused && !self.ime.preedit.is_empty() {
                self.ime.paint(&painter, cr.min, rh, font.clone(), TEXT(), BG2());
            } else if focused {
                let t = ui.input(|i| i.time) - self.blink_at;
                let phase = t % 1.06;
                if phase < 0.6 {
                    painter.rect_filled(cr, 1.0, ACCENT().gamma_multiply(0.85));
                    if let Some(c) = s.grid.get(s.y).and_then(|r| r.get(s.x)).filter(|c| c.ch != ' ') {
                        painter.text(cr.center(), Align2::CENTER_CENTER, c.ch, font.clone(), ON_ACCENT());
                    }
                }
                let next = if phase < 0.6 { 0.6 - phase } else { 1.06 - phase };
                ui.ctx().request_repaint_after(Duration::from_secs_f64(next.max(0.02)));
            } else {
                painter.rect_stroke(cr.shrink(0.5), 1.0, st(1.0, ACCENT().gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
        }

        // 空屏提示
        if let (true, Some(e)) = (blank, empty) {
            let (t1, t2) = e[focused as usize];
            let c = card_rect.center();
            painter.text(c - vec2(0.0, 12.0), Align2::CENTER_CENTER, t1, FontId::new(15.0, bold()), TEXT2());
            painter.text(c + vec2(0.0, 14.0), Align2::CENTER_CENTER, t2, FontId::proportional(12.5), TEXT3());
        }

        // 回滚条
        if sbl > 0 {
            let total = (sbl + trows) as f32;
            let track = Rect::from_min_max(pos2(card_rect.right() - 7.0, inner.top()), pos2(card_rect.right() - 4.0, inner.bottom()));
            let h = (trows as f32 / total * track.height()).max(16.0);
            let top = track.top() + top_abs as f32 / sbl as f32 * (track.height() - h);
            let thumb = Rect::from_min_size(pos2(track.left(), top), vec2(track.width(), h));
            painter.rect_filled(thumb, 1.5, if self.scroll > 0 { ACCENT().gamma_multiply(0.6) } else { LINE_HI() });
        }
        if self.scroll > 0 {
            let r = Rect::from_center_size(pos2(card_rect.center().x, card_rect.bottom() - 26.0), vec2(116.0, 30.0));
            let jr = ui.interact(r, id.with("jump"), Sense::click());
            let p = ui.painter();
            p.add(
                epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: shade(170),
                }
                .as_shape(r, CornerRadius::same(15)),
            );
            p.rect(
                r,
                CornerRadius::same(15),
                if jr.hovered() { BG3() } else { BG2() },
                st(1.0, ACCENT().gamma_multiply(0.5)),
                egui::StrokeKind::Inside,
            );
            let a = pos2(r.left() + 20.0, r.center().y);
            p.add(Shape::line(vec![a + vec2(-4.0, -2.0), a + vec2(0.0, 2.5), a + vec2(4.0, -2.0)], st(1.5, ACCENT())));
            p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, t("回到最新"), FontId::new(12.5, bold()), TEXT());
            if jr.clicked() {
                self.scroll = 0;
                self.focus_req = true;
            }
        }
        // 命令块标记：悬停高亮，单击复制这条命令和它的输出
        for (k, (row, r)) in mark_rows.into_iter().enumerate() {
            let mr = ui.interact(r.expand2(vec2(3.0, 0.0)), id.with(("mark", k)), Sense::click());
            let c = if mr.hovered() { ACCENT() } else { ACCENT().gamma_multiply(0.45) };
            painter.rect_filled(r, 2.0, c);
            if mr.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if mr.on_hover_text(t("单击复制这条命令和输出 · Ctrl+↑↓ 在命令之间跳")).clicked() {
                ui.ctx().copy_text(self.block_text(row));
                o.toast = Some(t("已复制这条命令的输出"));
            }
        }
        self.find_bar(ui, card_rect);
        hud_corners(&painter, card_rect.shrink(5.0), ACCENT().gamma_multiply(0.35));
        o.focused = focused;
        o
    }

    /// 查找栏（右上角）：回车上一个、Shift+回车下一个、Esc 关闭
    fn find_bar(&mut self, ui: &mut egui::Ui, card: Rect) {
        let Some(f) = self.find.as_mut() else { return };
        let r = Rect::from_min_size(pos2(card.right() - 336.0, card.top() + 8.0), vec2(324.0, 32.0));
        let p = ui.painter();
        p.add(
            epaint::Shadow {
                offset: [0, 4],
                blur: 14,
                spread: 0,
                color: shade(140),
            }
            .as_shape(r, CornerRadius::same(7)),
        );
        p.rect(r, CornerRadius::same(7), BG2(), st(1.0, ACCENT().gamma_multiply(0.45)), egui::StrokeKind::Inside);
        let mut close = false;
        let mut step: i32 = 0;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(8.0, 3.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let ui = &mut child;
        ui.spacing_mut().item_spacing.x = 4.0;
        let te = ui.add(egui::TextEdit::singleline(&mut f.q).hint_text(t("查找")).desired_width(170.0).frame(false));
        if f.focus {
            te.request_focus();
            f.focus = false;
        }
        if te.has_focus() || te.lost_focus() {
            let (enter, shift, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
            if esc {
                close = true;
            } else if enter {
                step = if shift { 1 } else { -1 };
                te.request_focus();
            }
        }
        let n = f.hits.len();
        let label = if f.q.is_empty() {
            String::new()
        } else if n == 0 {
            t("无结果").to_string()
        } else {
            format!("{}/{}", f.cur + 1, n)
        };
        ui.label(egui::RichText::new(label).size(11.5).color(if n == 0 && !f.q.is_empty() { ERR() } else { TEXT3() }));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = |ui: &mut egui::Ui, s: &str, tip: &str| ui.add(egui::Button::new(egui::RichText::new(s).size(13.0)).frame(false)).on_hover_text(tip).clicked();
            if b(ui, "✕", "Esc") {
                close = true;
            }
            if b(ui, "↓", t("下一个  Shift+Enter")) {
                step = 1;
            }
            if b(ui, "↑", t("上一个  Enter")) {
                step = -1;
            }
        });
        if step != 0 && n > 0 {
            f.cur = ((f.cur as i64 + step as i64).rem_euclid(n as i64)) as usize;
            f.jump = true;
            ui.ctx().request_repaint();
        }
        if close {
            self.find = None;
            self.focus_req = true;
        }
    }
}

/// 线框和方块字符自己画：字体里的线框字宽度不一，画出来对不齐
pub fn draw_block_char(p: &egui::Painter, r: Rect, ch: char, c: Color32) -> bool {
    let (l, rt, u, d) = match ch {
        '─' | '━' | '═' => (1, 1, 0, 0),
        '│' | '┃' | '║' => (0, 0, 1, 1),
        '┌' | '┏' | '╔' | '╭' => (0, 1, 0, 1),
        '┐' | '┓' | '╗' | '╮' => (1, 0, 0, 1),
        '└' | '┗' | '╚' | '╰' => (0, 1, 1, 0),
        '┘' | '┛' | '╝' | '╯' => (1, 0, 1, 0),
        '├' | '┣' | '╠' => (0, 1, 1, 1),
        '┤' | '┫' | '╣' => (1, 0, 1, 1),
        '┬' | '┳' | '╦' => (1, 1, 0, 1),
        '┴' | '┻' | '╩' => (1, 1, 1, 0),
        '┼' | '╋' | '╬' => (1, 1, 1, 1),
        '█' => return fill(p, r, c),
        '▀' => return fill(p, Rect::from_min_max(r.min, pos2(r.right(), r.center().y)), c),
        '▄' => return fill(p, Rect::from_min_max(pos2(r.left(), r.center().y), r.max), c),
        '▌' => return fill(p, Rect::from_min_max(r.min, pos2(r.center().x, r.bottom())), c),
        '▐' => return fill(p, Rect::from_min_max(pos2(r.center().x, r.top()), r.max), c),
        '░' => return fill(p, r, c.gamma_multiply(0.25)),
        '▒' => return fill(p, r, c.gamma_multiply(0.5)),
        '▓' => return fill(p, r, c.gamma_multiply(0.75)),
        _ => return false,
    };
    let m = pos2(r.center().x.round() + 0.5, r.center().y.round() + 0.5);
    let s = st(1.0, c);
    if l == 1 {
        p.hline(r.left()..=m.x, m.y, s);
    }
    if rt == 1 {
        p.hline(m.x..=r.right(), m.y, s);
    }
    if u == 1 {
        p.vline(m.x, r.top()..=m.y, s);
    }
    if d == 1 {
        p.vline(m.x, m.y..=r.bottom(), s);
    }
    true
}

fn fill(p: &egui::Painter, r: Rect, c: Color32) -> bool {
    p.rect_filled(r, 0.0, c);
    true
}

/// 找 col 所在的 `文件:行号`（也认 `文件:行:列` 和 Keil / MSVC 的 `文件(行)`），返回 [起, 止) 列和解析结果
pub fn find_link(chars: &[char], col: usize) -> Option<(usize, usize, String, usize)> {
    let delim = |c: char| c.is_whitespace() || c == '\u{0}' || "\"'`<>[]{},;|".contains(c);
    if col >= chars.len() || delim(chars[col]) {
        return None;
    }
    let mut a = col;
    while a > 0 && !delim(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b < chars.len() && !delim(chars[b]) {
        b += 1;
    }
    let tok: String = chars[a..b].iter().collect();
    let tok = tok.trim_end_matches([':', '.', ')', ',']).to_string();
    let tok = if tok.contains('(') && !tok.ends_with(')') { format!("{tok})") } else { tok };
    // 文件(行) 或 文件(行,列)
    if let Some(i) = tok.find('(') {
        let inner = tok[i + 1..].trim_end_matches(')');
        let line = inner.split(',').next()?.trim().parse::<usize>().ok()?;
        let file = &tok[..i];
        return (looks_like_file(file) && line > 0).then(|| (a, a + tok.chars().count(), file.to_string(), line));
    }
    // 文件:行[:列]，后面可以跟别的内容（grep -n 的 `文件:行:内容`）；Windows 盘符的冒号算在文件名里
    let parts: Vec<&str> = tok.split(':').collect();
    for i in 1..parts.len() {
        let num = parts[i];
        if num.is_empty() || !num.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let file = parts[..i].join(":");
        if !looks_like_file(&file) {
            continue;
        }
        let line = num.parse::<usize>().ok().filter(|&l| l > 0)?;
        let end = a + file.chars().count() + 1 + num.len();
        return (col < end).then_some((a, end, file, line));
    }
    None
}

/// 光标下的网址：(起始列, 结束列, 网址)
pub fn find_url(chars: &[char], col: usize) -> Option<(usize, usize, String)> {
    let delim = |c: char| c.is_whitespace() || c == '\u{0}' || "\"'`<>[]{}|".contains(c);
    if col >= chars.len() || delim(chars[col]) {
        return None;
    }
    let mut a = col;
    while a > 0 && !delim(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b < chars.len() && !delim(chars[b]) {
        b += 1;
    }
    let tok: String = chars[a..b].iter().collect();
    let start = tok.find("https://").or_else(|| tok.find("http://"))?;
    let a = a + tok[..start].chars().count();
    let url = tok[start..].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']).to_string();
    let b = a + url.chars().count();
    (col >= a && col < b && url.len() > 8).then_some((a, b, url))
}

fn looks_like_file(f: &str) -> bool {
    let name = f.rsplit(['/', '\\']).next().unwrap_or(f);
    name.contains('.') && !name.starts_with('.') || f.contains('/') || f.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::find_link;

    fn link(s: &str, col: usize) -> Option<(String, usize)> {
        let v: Vec<char> = s.chars().collect();
        find_link(&v, col).map(|(_, _, f, l)| (f, l))
    }

    #[test]
    fn links() {
        assert_eq!(link("src/main.c:12:5: error: x", 3), Some(("src/main.c".into(), 12)));
        assert_eq!(link("main.c(42): error C2065", 2), Some(("main.c".into(), 42)));
        assert_eq!(link("at C:\\fw\\uart.c:7", 6), Some(("C:\\fw\\uart.c".into(), 7)));
        assert_eq!(link("time 12:30:01 ok", 7), None);
        assert_eq!(link("src/uart.c:5:void uart_init", 1), Some(("src/uart.c".into(), 5)));
        assert_eq!(link("src/uart.c:5:void uart_init", 14), None);
        assert_eq!(link("hello world", 2), None);
        let v: Vec<char> = "see (https://zenghi.com/a?b=1).".chars().collect();
        assert_eq!(super::find_url(&v, 10).map(|u| u.2), Some("https://zenghi.com/a?b=1".into()));
        assert_eq!(super::find_url(&v, 1), None);
    }
}

//! 文本比较：左右对照、行内字符差异、两边都能直接改，中间的箭头把一段改动推到另一边。

use std::path::Path;

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Painter, Pos2, Rect, RichText, Sense, Ui, UiBuilder};

use super::{mono, nav, opts_chips, overview, save_report, side_title, step, CompareSettings, Out, Side};
use crate::app::{icon_button, menu_item, toggle_chip, Icon};
use crate::compare::diff::{self, CommentStyle, Kind, Opts, Row};
use crate::highlight::{self, Lang};
use crate::i18n::{t, tf};
use crate::theme::*;

const RH: f32 = 19.0;
/// 中间放箭头的一栏
const GUT: f32 = 40.0;
const OV: f32 = 12.0;
const NUM: f32 = 46.0;

struct Doc {
    lines: Vec<String>,
    eol: &'static str,
    gbk: bool,
    bom: bool,
    /// 文件以换行结尾
    trailing: bool,
    dirty: bool,
    lang: Lang,
}

impl Doc {
    fn empty() -> Doc {
        Doc {
            lines: vec![],
            eol: "\n",
            gbk: false,
            bom: false,
            trailing: true,
            dirty: false,
            lang: Lang::Plain,
        }
    }

    fn load(p: &Path) -> Result<Doc, String> {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let (text, gbk) = crate::cindex::decode(&bytes).ok_or_else(|| t("这是二进制文件，请用十六进制比较").to_string())?;
        let (mut lines, eol) = diff::split_lines(&text);
        if text.is_empty() {
            lines.clear();
        }
        Ok(Doc {
            lines,
            eol,
            gbk,
            bom: bytes.starts_with(b"\xef\xbb\xbf"),
            trailing: text.is_empty() || text.ends_with('\n'),
            dirty: false,
            lang: Lang::from_path(p),
        })
    }

    /// 按原来的编码、换行风格、BOM 和结尾换行写回去
    fn save(&mut self, p: &Path) -> Result<(), String> {
        let mut s = self.lines.join(self.eol);
        if self.trailing && !self.lines.is_empty() {
            s.push_str(self.eol);
        }
        let mut bytes = if self.gbk { encoding_rs::GBK.encode(&s).0.into_owned() } else { s.into_bytes() };
        if self.bom && !self.gbk {
            bytes.splice(0..0, *b"\xef\xbb\xbf");
        }
        std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
        self.dirty = false;
        Ok(())
    }

    fn info(&self) -> String {
        format!(
            "{} · {}",
            if self.gbk {
                "GBK"
            } else if self.bom {
                "UTF-8 BOM"
            } else {
                "UTF-8"
            },
            if self.eol == "\r\n" { "CRLF" } else { "LF" }
        )
    }
}

fn at(r: &Row, side: usize) -> Option<usize> {
    if side == 0 {
        r.l
    } else {
        r.r
    }
}

/// 一次界面操作，画完再统一处理（画的时候 self 是只读借用）
enum Act {
    Push(usize, bool),
    Edit(usize, usize),
    Insert(usize, usize),
    Delete(usize, usize),
    Anchor(usize, usize),
    OpenCode(usize, usize),
    Copy(String),
    Select(usize, usize),
}

pub struct TextCmp {
    pub a: Side,
    pub b: Side,
    d: [Doc; 2],
    err: Option<String>,
    opts: Opts,
    style: CommentStyle,
    rows: Vec<Row>,
    blocks: Vec<(usize, usize)>,
    marks: Vec<(usize, Color32)>,
    /// 当前这处改动（blocks 下标）
    cur: Option<usize>,
    /// 手动对齐点（左行, 右行）
    anchors: Vec<(usize, usize)>,
    /// 设对齐点时先选的左边一行
    pick: Option<usize>,
    /// 正在编辑：(哪边, 第几行, 内容, 已经给了焦点)
    edit: Option<(usize, usize, String, bool)>,
    scroll_to: Option<usize>,
    /// 选中的 (行, 哪边)
    sel: Option<(usize, usize)>,
    only_diff: bool,
    undo: Vec<([Vec<String>; 2], [bool; 2])>,
    /// 横向滚动
    hx: f32,
}

impl TextCmp {
    pub fn new(a: Side, b: Side, opts: &Opts) -> Self {
        let mut err = None;
        let mut ld = |s: &Side| {
            Doc::load(&s.path).unwrap_or_else(|e| {
                err.get_or_insert(e);
                Doc::empty()
            })
        };
        let d = [ld(&a), ld(&b)];
        let style = CommentStyle::of(&a.path.display().to_string());
        let mut x = Self {
            a,
            b,
            d,
            err,
            opts: *opts,
            style,
            rows: vec![],
            blocks: vec![],
            marks: vec![],
            cur: None,
            anchors: vec![],
            pick: None,
            edit: None,
            scroll_to: None,
            sel: None,
            only_diff: false,
            undo: vec![],
            hx: 0.0,
        };
        x.recompute();
        // 打开时停在第一处改动
        if !x.blocks.is_empty() {
            x.goto(Some(0));
        }
        x
    }

    pub fn dirty(&self) -> bool {
        self.d[0].dirty || self.d[1].dirty
    }

    pub fn save_all(&mut self) -> Result<(), String> {
        for i in 0..2 {
            if self.d[i].dirty && !self.side(i).readonly {
                let p = self.side(i).path.clone();
                self.d[i].save(&p)?;
            }
        }
        Ok(())
    }

    fn side(&self, i: usize) -> &Side {
        if i == 0 {
            &self.a
        } else {
            &self.b
        }
    }

    fn recompute(&mut self) {
        self.rows = diff::align(&self.d[0].lines, &self.d[1].lines, &self.opts, self.style, &self.anchors);
        self.blocks = diff::blocks(&self.rows);
        self.marks = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r.kind {
                Kind::Changed => Some((i, WARN())),
                Kind::LeftOnly => Some((i, ERR())),
                Kind::RightOnly => Some((i, OK())),
                _ => None,
            })
            .collect();
        if self.cur.is_some_and(|c| c >= self.blocks.len()) {
            self.cur = self.blocks.len().checked_sub(1);
        }
    }

    fn reload(&mut self) {
        let opts = self.opts;
        let anchors = std::mem::take(&mut self.anchors);
        *self = TextCmp::new(self.a.clone(), self.b.clone(), &opts);
        self.anchors = anchors;
        self.recompute();
    }

    fn goto(&mut self, b: Option<usize>) {
        self.cur = b;
        if let Some(&(s, _)) = b.and_then(|b| self.blocks.get(b)) {
            self.scroll_to = Some(s);
        }
    }

    fn snapshot(&mut self) {
        self.undo.push(([self.d[0].lines.clone(), self.d[1].lines.clone()], [self.d[0].dirty, self.d[1].dirty]));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some(([l, r], [dl, dr])) = self.undo.pop() {
            self.d[0].lines = l;
            self.d[1].lines = r;
            self.d[0].dirty = dl;
            self.d[1].dirty = dr;
            self.recompute();
        }
    }

    /// 某一行在这一边对应的行号；这边是空行时返回新行应插入的位置
    fn pos(&self, row: usize, side: usize) -> usize {
        if let Some(x) = self.rows.get(row).and_then(|r| at(r, side)) {
            return x;
        }
        self.rows[..row.min(self.rows.len())].iter().rev().find_map(|r| at(r, side)).map_or(0, |x| x + 1)
    }

    /// 把一处改动推到另一边：另一边的这一段换成这一边的
    fn push(&mut self, bi: usize, to_right: bool) {
        let Some(&(s, e)) = self.blocks.get(bi) else { return };
        let (from, to) = if to_right { (0, 1) } else { (1, 0) };
        if self.side(to).readonly {
            return;
        }
        let src: Vec<String> = self.rows[s..e].iter().filter_map(|r| at(r, from)).map(|i| self.d[from].lines[i].clone()).collect();
        let tl: Vec<usize> = self.rows[s..e].iter().filter_map(|r| at(r, to)).collect();
        let start = tl.first().copied().unwrap_or_else(|| self.pos(s, to));
        let end = tl.last().map_or(start, |x| x + 1);
        self.snapshot();
        self.d[to].lines.splice(start..end, src);
        self.d[to].dirty = true;
        self.recompute();
    }

    fn block_of(&self, row: usize) -> Option<usize> {
        let i = self.blocks.partition_point(|b| b.0 <= row);
        (i > 0 && row < self.blocks[i - 1].1).then(|| i - 1)
    }

    fn commit(&mut self, side: usize, line: usize, text: String) {
        if self.d[side].lines.get(line).is_some_and(|l| *l != text) {
            self.snapshot();
            self.d[side].lines[line] = text;
            self.d[side].dirty = true;
            self.recompute();
        }
    }

    // ------------------------------------------------------------ 界面

    pub fn ui(&mut self, ui: &mut Ui, s: &mut CompareSettings) -> Vec<Out> {
        let mut outs = vec![];
        self.toolbar(ui, s, &mut outs);
        if let Some(e) = &self.err {
            egui::Frame::new().fill(ERR().gamma_multiply(0.10)).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(e).color(ERR()));
            });
        }
        self.header(ui, &mut outs);
        let mut acts = vec![];
        self.body(ui, &mut acts);
        self.keys(ui, &mut outs);
        for a in acts {
            self.act(a, &mut outs, ui.ctx());
        }
        outs
    }

    fn toolbar(&mut self, ui: &mut Ui, s: &mut CompareSettings, outs: &mut Vec<Out>) {
        egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(next) = nav(ui, self.blocks.len(), self.cur) {
                    let c = step(self.cur, self.blocks.len(), next);
                    self.goto(c);
                }
                ui.add_space(6.0);
                if opts_chips(ui, &mut self.opts) {
                    s.opts = self.opts;
                    self.recompute();
                }
                toggle_chip(ui, &mut self.only_diff, t("只看不同"), t("隐藏相同的行，只留改动前后 3 行"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Download, &t("导出 HTML 报告"), false).clicked() {
                        let html = crate::compare::report::text(&self.a.label, &self.b.label, &self.d[0].lines, &self.d[1].lines, &self.rows, Some(3));
                        save_report(html, &format!("{}-compare", self.a.label), outs);
                    }
                    let dirty = self.dirty();
                    let r = ui.add_enabled_ui(!dirty, |ui| icon_button(ui, Icon::Refresh, &t("重新读取两边的文件  F5"), false)).inner;
                    if r.clicked() {
                        self.reload();
                    }
                    if dirty {
                        r.on_disabled_hover_text(t("有未保存的修改，先保存或撤销"));
                    }
                    if icon_button(ui, Icon::Swap, &t("交换左右"), false).clicked() {
                        self.a = std::mem::replace(&mut self.b, self.a.clone());
                        self.d.swap(0, 1);
                        self.anchors = self.anchors.iter().map(|&(x, y)| (y, x)).collect();
                        self.recompute();
                    }
                    if !self.anchors.is_empty() && ui.small_button(tf("清除 {} 个对齐点", &[&self.anchors.len()])).clicked() {
                        self.anchors.clear();
                        self.recompute();
                    }
                    if self.pick.is_some() {
                        ui.label(RichText::new(t("再在右边选一行作为对齐点")).small().color(VIOLET()));
                    }
                });
            });
        });
    }

    fn header(&mut self, ui: &mut Ui, outs: &mut Vec<Out>) {
        let full = ui.available_rect_before_wrap();
        let r = Rect::from_min_size(full.min, vec2(full.width(), 32.0));
        ui.allocate_rect(r, Sense::hover());
        ui.painter().rect_filled(r, 0.0, BG1());
        ui.painter().hline(r.x_range(), r.bottom() - 0.5, st(1.0, LINE()));
        let sw = (r.width() - OV - GUT) / 2.0;
        for side in 0..2 {
            let x = r.left() + side as f32 * (sw + GUT);
            let sr = Rect::from_min_size(pos2(x + 6.0, r.top() + 3.0), vec2(sw - 10.0, 26.0));
            let mut c = ui.new_child(UiBuilder::new().max_rect(sr).layout(Layout::left_to_right(Align::Center)));
            let info = self.d[side].info();
            let dirty = self.d[side].dirty;
            let ro = self.side(side).readonly;
            side_title(&mut c, self.side(side), dirty, (sw - 190.0).max(80.0));
            c.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let b = egui::Button::new(RichText::new(t("保存")).size(12.0)).min_size(vec2(0.0, 22.0));
                if ui.add_enabled(dirty && !ro, b).on_hover_text("Ctrl+S").clicked() {
                    let p = self.side(side).path.clone();
                    match self.d[side].save(&p) {
                        Ok(_) => outs.push(Out::Toast(tf("已保存 {}", &[&self.side(side).label]), OK())),
                        Err(e) => outs.push(Out::Toast(e, ERR())),
                    }
                }
                ui.label(RichText::new(info).font(FontId::monospace(10.5)).color(TEXT3()));
            });
        }
    }

    fn view(&self) -> Vec<usize> {
        if !self.only_diff {
            return (0..self.rows.len()).collect();
        }
        let mut keep = vec![false; self.rows.len()];
        for &(s, e) in &self.blocks {
            for k in keep.iter_mut().take((e + 3).min(self.rows.len())).skip(s.saturating_sub(3)) {
                *k = true;
            }
        }
        keep.iter().enumerate().filter(|x| *x.1).map(|x| x.0).collect()
    }

    fn body(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let ov = Rect::from_min_max(pos2(full.right() - OV, full.top()), full.max);
        let main = Rect::from_min_max(full.min, pos2(ov.left(), full.bottom()));
        let view = self.view();
        if self.rows.is_empty() || (self.only_diff && view.is_empty()) {
            let msg = if self.rows.is_empty() { t("两边都是空文件") } else { t("两边内容相同") };
            ui.painter().text(main.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), TEXT3());
            return;
        }
        if self.blocks.is_empty() && self.err.is_none() {
            // 一样的时候在顶上提示一下
            ui.painter()
                .text(pos2(main.right() - 12.0, main.top() + 10.0), Align2::RIGHT_CENTER, t("两边内容相同"), FontId::proportional(11.5), OK());
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(main));
        child.spacing_mut().item_spacing.y = 0.0;
        let mut sa = egui::ScrollArea::vertical().id_salt("txt-cmp").auto_shrink([false, false]);
        if let Some(row) = self.scroll_to.take() {
            let vi = view.partition_point(|&x| x < row);
            sa = sa.vertical_scroll_offset((vi as f32 - 6.0).max(0.0) * RH);
        }
        // Shift+滚轮横向滚动长行
        if child.rect_contains_pointer(main) {
            let dx = ui.input(|i| i.smooth_scroll_delta.x);
            self.hx = (self.hx - dx).max(0.0);
        }
        let mut edit = self.edit.take();
        let mut shown = (0, 0);
        sa.show_rows(&mut child, RH, view.len(), |ui, range| {
            shown = (view.get(range.start).copied().unwrap_or(0), view.get(range.end.saturating_sub(1)).copied().unwrap_or(0) + 1);
            let mut prev: Option<usize> = range.start.checked_sub(1).map(|i| view[i]);
            for vi in range {
                let ri = view[vi];
                let gap = prev.is_some_and(|p| p + 1 != ri);
                prev = Some(ri);
                self.row_ui(ui, ri, gap, &mut edit, acts);
            }
        });
        self.edit = edit;
        if let Some(i) = overview(ui, ov, self.rows.len(), &self.marks, Some(shown)) {
            self.scroll_to = Some(i);
            self.cur = self.block_of(i).or(self.cur);
        }
    }

    fn row_ui(&self, ui: &mut Ui, ri: usize, gap: bool, edit: &mut Option<(usize, usize, String, bool)>, acts: &mut Vec<Act>) {
        let row = self.rows[ri];
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), RH), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let sw = (rect.width() - GUT) / 2.0;
        let p = ui.painter().clone();
        let blk = self.block_of(ri);
        let in_cur = blk.is_some() && blk == self.cur;
        let (ml, mr) = match (row.kind, row.l, row.r) {
            (Kind::Changed, Some(x), Some(y)) => diff::inline(&self.d[0].lines[x], &self.d[1].lines[y]),
            _ => (vec![], vec![]),
        };
        for side in 0..2 {
            let x0 = rect.left() + side as f32 * (sw + GUT);
            let r = Rect::from_min_size(pos2(x0, rect.top()), vec2(sw, RH));
            let line = at(&row, side);
            // 底色
            let bg = match (row.kind, line.is_some()) {
                (Kind::Same, _) => None,
                (Kind::Minor, _) => Some(VIOLET().gamma_multiply(0.07)),
                (_, false) => Some(BG1()),
                (Kind::Changed, true) => Some(if side == 0 { ERR() } else { OK() }.gamma_multiply(0.10)),
                (Kind::LeftOnly, true) => Some(ERR().gamma_multiply(0.13)),
                (Kind::RightOnly, true) => Some(OK().gamma_multiply(0.13)),
            };
            if let Some(c) = bg {
                p.rect_filled(r, 0.0, c);
            }
            if line.is_none() && !matches!(row.kind, Kind::Same | Kind::Minor) {
                // 这边没有这一行：斜线底纹
                let pp = p.with_clip_rect(r);
                let mut x = r.left() - RH;
                while x < r.right() {
                    pp.line_segment([pos2(x, r.bottom()), pos2(x + RH, r.top())], st(1.0, LINE()));
                    x += 8.0;
                }
            }
            if self.sel == Some((ri, side)) {
                p.rect_stroke(r.shrink(0.5), 0.0, st(1.0, ACCENT().gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
            if self.pick.is_some() && side == 0 && line == self.pick {
                p.rect_stroke(r.shrink(0.5), 0.0, st(1.5, VIOLET()), egui::StrokeKind::Inside);
            }
            if gap {
                p.hline(r.x_range(), r.top(), st(1.0, TEXT3().gamma_multiply(0.5)));
            }
            if in_cur && side == 0 {
                p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, RH)), 0.0, ACCENT());
            }
            let Some(ln) = line else {
                let resp = ui.interact(r, ui.id().with(("cell", ri, side)), Sense::click());
                self.cell_menu(resp, ri, side, None, acts);
                continue;
            };
            p.text(pos2(r.left() + NUM - 8.0, r.center().y), Align2::RIGHT_CENTER, (ln + 1).to_string(), FontId::monospace(11.0), TEXT3());
            let tr = Rect::from_min_max(pos2(r.left() + NUM, r.top()), r.max);
            let editing = edit.as_ref().is_some_and(|e| e.0 == side && e.1 == ln);
            if editing {
                let e = edit.as_mut().unwrap();
                let te = egui::TextEdit::singleline(&mut e.2).font(mono()).frame(false).margin(vec2(0.0, 1.0)).desired_width(tr.width() - 4.0);
                p.rect_filled(tr, 0.0, BG2());
                let resp = ui.put(tr, te);
                if !e.3 {
                    resp.request_focus();
                    e.3 = true;
                }
                continue;
            }
            let text = &self.d[side].lines[ln];
            let marks = if side == 0 { &ml } else { &mr };
            let mc = if side == 0 { ERR() } else { OK() }.gamma_multiply(0.35);
            draw_text(ui, &p.with_clip_rect(tr), pos2(tr.left() + 2.0 - self.hx, tr.top() + 2.0), text, self.d[side].lang, marks, mc);
            let resp = ui.interact(r, ui.id().with(("cell", ri, side)), Sense::click());
            if resp.double_clicked() && !self.side(side).readonly {
                acts.push(Act::Edit(side, ln));
            } else if resp.clicked() {
                acts.push(Act::Select(ri, side));
            }
            self.cell_menu(resp, ri, side, Some(ln), acts);
        }
        // 中间一栏：改动的连线和推送箭头
        let g = Rect::from_min_size(pos2(rect.left() + sw, rect.top()), vec2(GUT, RH));
        if !matches!(row.kind, Kind::Same | Kind::Minor) {
            let c = match row.kind {
                Kind::LeftOnly => ERR(),
                Kind::RightOnly => OK(),
                _ => WARN(),
            };
            p.rect_filled(g.shrink2(vec2(GUT / 2.0 - 1.5, 0.0)), 0.0, c.gamma_multiply(0.45));
        }
        if self.anchors.iter().any(|&(x, y)| Some(x) == row.l && Some(y) == row.r) {
            p.circle_filled(g.center(), 4.0, VIOLET());
        }
        if let Some(b) = blk.filter(|&b| self.blocks[b].0 == ri) {
            let half = |right: bool| Rect::from_min_size(pos2(g.left() + if right { GUT / 2.0 } else { 0.0 }, g.top()), vec2(GUT / 2.0, RH));
            for (to_right, ro) in [(true, self.b.readonly), (false, self.a.readonly)] {
                if ro {
                    continue;
                }
                let hr = if to_right { half(false) } else { half(true) };
                let resp = ui.interact(hr, ui.id().with(("push", ri, to_right)), Sense::click());
                let col = if resp.hovered() { ACCENT() } else { TEXT2() };
                if resp.hovered() {
                    p.rect_filled(hr.shrink(1.0), CornerRadius::same(3), BG3());
                }
                arrow(&p, hr.center(), to_right, col);
                let tip = if to_right {
                    t("用左边这段替换右边  Alt+→")
                } else {
                    t("用右边这段替换左边  Alt+←")
                };
                if resp.on_hover_text(tip).clicked() {
                    acts.push(Act::Push(b, to_right));
                }
            }
        }
    }

    fn cell_menu(&self, resp: egui::Response, ri: usize, side: usize, line: Option<usize>, acts: &mut Vec<Act>) {
        let ro = self.side(side).readonly;
        let other_ro = self.side(1 - side).readonly;
        let blk = self.block_of(ri);
        resp.context_menu(|ui| {
            ui.set_min_width(220.0);
            if let Some(ln) = line {
                if !ro && menu_item(ui, t("编辑这一行"), t("双击")) {
                    acts.push(Act::Edit(side, ln));
                    ui.close_menu();
                }
                if menu_item(ui, t("复制这一行"), "") {
                    acts.push(Act::Copy(self.d[side].lines[ln].clone()));
                    ui.close_menu();
                }
            }
            if !ro && menu_item(ui, t("在这里插入一行"), "") {
                acts.push(Act::Insert(side, line.map_or_else(|| self.pos(ri, side), |l| l + 1)));
                ui.close_menu();
            }
            if let Some(ln) = line.filter(|_| !ro) {
                if menu_item(ui, t("删除这一行"), "") {
                    acts.push(Act::Delete(side, ln));
                    ui.close_menu();
                }
            }
            if let Some(b) = blk.filter(|_| !other_ro) {
                ui.separator();
                let k = if side == 0 { "Alt+→" } else { "Alt+←" };
                if menu_item(ui, t("把这段改动复制到另一边"), k) {
                    acts.push(Act::Push(b, side == 0));
                    ui.close_menu();
                }
            }
            if let Some(ln) = line {
                ui.separator();
                let label = if side == 0 { t("设为对齐点（左）") } else { t("设为对齐点（右）") };
                if menu_item(ui, label, "") {
                    acts.push(Act::Anchor(side, ln));
                    ui.close_menu();
                }
                if !self.side(side).readonly && menu_item(ui, t("在代码页打开"), "") {
                    acts.push(Act::OpenCode(side, ln));
                    ui.close_menu();
                }
            }
        });
    }

    fn act(&mut self, a: Act, outs: &mut Vec<Out>, ctx: &egui::Context) {
        match a {
            Act::Push(b, r) => {
                self.cur = Some(b);
                self.push(b, r);
            }
            Act::Edit(side, ln) => {
                if let Some((s, l, txt, _)) = self.edit.take() {
                    self.commit(s, l, txt);
                }
                self.edit = Some((side, ln, self.d[side].lines[ln].clone(), false));
            }
            Act::Insert(side, at) => {
                self.snapshot();
                let at = at.min(self.d[side].lines.len());
                self.d[side].lines.insert(at, String::new());
                self.d[side].dirty = true;
                self.recompute();
                self.edit = Some((side, at, String::new(), false));
            }
            Act::Delete(side, ln) => {
                self.snapshot();
                self.d[side].lines.remove(ln);
                self.d[side].dirty = true;
                self.recompute();
            }
            Act::Anchor(side, ln) => {
                if side == 0 {
                    self.pick = Some(ln);
                } else if let Some(l) = self.pick.take() {
                    // 对齐点两边都要递增，冲突的旧点去掉
                    self.anchors.retain(|&(x, y)| (x < l) == (y < ln) && x != l && y != ln);
                    self.anchors.push((l, ln));
                    self.anchors.sort();
                    self.recompute();
                    outs.push(Out::Toast(tf("左边第 {} 行和右边第 {} 行已对齐", &[&(l + 1), &(ln + 1)]), OK()));
                } else {
                    outs.push(Out::Toast(t("先在左边选一行设为对齐点").into(), WARN()));
                }
            }
            Act::OpenCode(side, ln) => outs.push(Out::OpenFile(self.side(side).path.clone(), ln)),
            Act::Copy(s) => ctx.copy_text(s),
            Act::Select(ri, side) => {
                self.sel = Some((ri, side));
                if let Some(b) = self.block_of(ri) {
                    self.cur = Some(b);
                }
            }
        }
    }

    fn keys(&mut self, ui: &mut Ui, outs: &mut Vec<Out>) {
        if let Some((side, ln, txt, _)) = &self.edit {
            let (side, ln) = (*side, *ln);
            let (enter, esc, up, down) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                )
            });
            if esc || enter || up || down {
                // 编辑框没了，焦点也要放掉，不然后面的快捷键都被当成在输入
                ui.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
            if esc {
                self.edit = None;
            } else if enter || up || down {
                let txt = txt.clone();
                self.edit = None;
                self.commit(side, ln, txt);
                let n = self.d[side].lines.len();
                let next = if up {
                    ln.checked_sub(1)
                } else if down {
                    (ln + 1 < n).then_some(ln + 1)
                } else {
                    None
                };
                if let Some(nl) = next {
                    self.edit = Some((side, nl, self.d[side].lines[nl].clone(), false));
                }
            }
            return;
        }
        if ui.memory(|m| m.focused().is_some()) {
            return;
        }
        let (save, undo, push_r, push_l, f5) = ui.input(|i| {
            let c = i.modifiers.command;
            (
                c && i.key_pressed(egui::Key::S),
                c && i.key_pressed(egui::Key::Z),
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::F5),
            )
        });
        if save && self.dirty() {
            match self.save_all() {
                Ok(_) => outs.push(Out::Toast(t("已保存").into(), OK())),
                Err(e) => outs.push(Out::Toast(e, ERR())),
            }
        }
        if undo {
            self.undo();
        }
        if let Some(b) = self.cur {
            if push_r {
                self.push(b, true);
            }
            if push_l {
                self.push(b, false);
            }
        }
        if f5 && !self.dirty() {
            self.reload();
        }
    }
}

/// 中间栏的小箭头
pub(crate) fn arrow(p: &Painter, c: Pos2, right: bool, col: Color32) {
    let s = if right { 1.0 } else { -1.0 };
    let stroke = st(1.5, col);
    p.line_segment([c + vec2(-5.0 * s, 0.0), c + vec2(5.0 * s, 0.0)], stroke);
    p.line_segment([c + vec2(5.0 * s, 0.0), c + vec2(1.0 * s, -4.0)], stroke);
    p.line_segment([c + vec2(5.0 * s, 0.0), c + vec2(1.0 * s, 4.0)], stroke);
}

/// 显示时 Tab 展开成 4 格；byte 偏移换成显示列
fn col(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// 画一行：语法高亮 + 行内差异底色
pub(crate) fn draw_text(ui: &Ui, p: &Painter, pos: Pos2, s: &str, lang: Lang, marks: &[(usize, usize)], mc: Color32) {
    let disp = if s.contains('\t') { s.replace('\t', "    ") } else { s.to_string() };
    let job = highlight::layout(&disp, lang, mono());
    let g = ui.fonts(|f| f.layout_job(job));
    for &(a, b) in marks {
        if !s.is_char_boundary(a.min(s.len())) || !s.is_char_boundary(b.min(s.len())) {
            continue;
        }
        let xa = g.pos_from_ccursor(egui::text::CCursor::new(col(s, a))).min.x;
        let xb = g.pos_from_ccursor(egui::text::CCursor::new(col(s, b))).min.x;
        let w = (xb - xa).max(3.0);
        p.rect_filled(Rect::from_min_size(pos2(pos.x + xa, pos.y - 1.0), vec2(w, RH - 2.0)), CornerRadius::same(2), mc);
    }
    p.galley(pos, g, TEXT());
}

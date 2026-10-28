//! 十六进制比较的界面：两边各一列 地址 / 十六进制 / ASCII，不同的字节标红，可以导出差异列表。

use eframe::egui::{self, pos2, vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense, Ui};

use super::{nav, step, CompareSettings, Out, Side};
use crate::app::{icon_button, toggle_chip, Icon};
use crate::compare::hexcmp::{self, HexCmp, W};
use crate::i18n::{t, tf};
use crate::theme::*;

const RH: f32 = 19.0;

pub struct HexTab {
    pub a: Side,
    pub b: Side,
    h: Result<HexCmp, String>,
    align: bool,
    cur: Option<usize>,
    scroll_to: Option<usize>,
}

impl HexTab {
    pub fn new(a: Side, b: Side, align: bool) -> Self {
        let h = hexcmp::open(&a.path, &b.path, align);
        let mut x = Self {
            a,
            b,
            h,
            align,
            cur: None,
            scroll_to: None,
        };
        if x.h.as_ref().is_ok_and(|h| !h.diff_rows.is_empty()) {
            x.goto(Some(0));
        }
        x
    }

    fn reload(&mut self) {
        self.h = hexcmp::open(&self.a.path, &self.b.path, self.align);
        self.cur = None;
    }

    fn goto(&mut self, b: Option<usize>) {
        self.cur = b;
        if let (Ok(h), Some(b)) = (&self.h, b) {
            self.scroll_to = h.blocks().get(b).map(|x| x.0);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, s: &mut CompareSettings) -> Vec<Out> {
        let mut outs = vec![];
        let blocks = self.h.as_ref().map(|h| h.blocks()).unwrap_or_default();
        egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(next) = nav(ui, blocks.len(), self.cur) {
                    let c = step(self.cur, blocks.len(), next);
                    self.goto(c);
                }
                ui.add_space(6.0);
                if let Ok(h) = &self.h {
                    let same = h.diff_bytes == 0;
                    let msg = if same { t("内容相同").to_string() } else { tf("{} 个字节不同", &[&h.diff_bytes]) };
                    ui.label(RichText::new(msg).color(if same { OK() } else { WARN() }));
                    ui.label(
                        RichText::new(format!("{} {} · {} {}", h.kind_l, crate::app::human_bytes(h.size_l), h.kind_r, crate::app::human_bytes(h.size_r)))
                            .small()
                            .color(TEXT3()),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Refresh, &t("重新读取"), false).clicked() {
                        self.reload();
                    }
                    if let Ok(h) = &self.h {
                        if icon_button(ui, Icon::Download, &t("导出差异列表"), false).clicked() {
                            let name = format!("{}-diff.txt", self.a.label);
                            if let Some(p) = rfd::FileDialog::new().set_file_name(name).save_file() {
                                let head = format!("# {}\n# {}\n", self.a.path.display(), self.b.path.display());
                                match std::fs::write(&p, format!("{head}{}", h.report())) {
                                    Ok(_) => outs.push(Out::Toast(tf("已保存：{}", &[&p.display()]), OK())),
                                    Err(e) => outs.push(Out::Toast(e.to_string(), ERR())),
                                }
                            }
                        }
                        if ui.small_button(t("复制差异列表")).clicked() {
                            ui.ctx().copy_text(h.report());
                            outs.push(Out::Toast(t("已复制").into(), OK()));
                        }
                    }
                    if toggle_chip(ui, &mut self.align, t("自动对齐"), t("BIN 文件中间插入或删除了字节时自动对齐，后面的内容不会全部错开")) {
                        s.align_hex = self.align;
                        self.reload();
                    }
                });
            });
        });
        let h = match &self.h {
            Ok(h) => h,
            Err(e) => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(e).color(ERR())));
                return outs;
            }
        };
        // 标题
        let full = ui.available_rect_before_wrap();
        let hr = Rect::from_min_size(full.min, vec2(full.width(), 28.0));
        ui.allocate_rect(hr, Sense::hover());
        ui.painter().rect_filled(hr, 0.0, BG1());
        ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, st(1.0, LINE()));
        let cw = ui.fonts(|f| f.glyph_width(&super::mono(), '0'));
        let side_w = cw * (10.0 + 3.0 * W as f32 + 2.0 + W as f32) + 24.0;
        let left_x = hr.left() + 10.0;
        let right_x = left_x + side_w + 20.0;
        for (x, side) in [(left_x, &self.a), (right_x, &self.b)] {
            ui.painter().text(pos2(x, hr.center().y), Align2::LEFT_CENTER, &side.label, FontId::new(12.5, bold()), TEXT());
        }
        let mut sa = egui::ScrollArea::both().id_salt("hex-cmp").auto_shrink([false, false]);
        if let Some(r) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset((r as f32 - 6.0).max(0.0) * RH);
        }
        let cur_rows = self.cur.and_then(|c| blocks.get(c).copied());
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, RH, h.rows(), |ui, range| {
            for i in range {
                let (rect, _) = ui.allocate_exact_size(vec2((side_w * 2.0 + 40.0).max(ui.available_width()), RH), Sense::hover());
                let p = ui.painter();
                if h.is_gap(i) {
                    p.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, "⋯", FontId::proportional(13.0), TEXT3());
                    p.hline(rect.x_range(), rect.center().y, st(1.0, LINE()));
                    continue;
                }
                let row = h.row(i);
                let diff = row.cells.iter().any(|(x, y)| x != y);
                if cur_rows.is_some_and(|(s, e)| i >= s && i < e) {
                    p.rect_filled(Rect::from_min_size(rect.min, vec2(2.0, RH)), 0.0, ACCENT());
                }
                if diff {
                    p.rect_filled(rect, 0.0, WARN().gamma_multiply(0.05));
                }
                for (side, x0) in [(0, left_x), (1, right_x)] {
                    let addr = if side == 0 { row.la } else { row.ra };
                    let y = rect.center().y;
                    let at = |col: f32| pos2(x0 + col * cw, y);
                    if let Some(a) = addr {
                        p.text(at(0.0), Align2::LEFT_CENTER, format!("{a:08X}"), super::mono(), TEXT3());
                    }
                    for (k, (l, r)) in row.cells.iter().enumerate() {
                        let (me, other) = if side == 0 { (l, r) } else { (r, l) };
                        let differs = me != other;
                        let hx = at(10.0 + 3.0 * k as f32 + if k >= 8 { 1.0 } else { 0.0 });
                        let ax = at(10.0 + 3.0 * W as f32 + 2.0 + k as f32);
                        let c = if differs {
                            if side == 0 {
                                ERR()
                            } else {
                                OK()
                            }
                        } else {
                            TEXT2()
                        };
                        match me {
                            Some(b) => {
                                if differs {
                                    let bg = if side == 0 { ERR() } else { OK() }.gamma_multiply(0.18);
                                    p.rect_filled(Rect::from_min_size(hx - vec2(1.0, RH / 2.0 - 1.0), vec2(cw * 2.0 + 2.0, RH - 2.0)), 2.0, bg);
                                    p.rect_filled(Rect::from_min_size(ax - vec2(0.0, RH / 2.0 - 1.0), vec2(cw, RH - 2.0)), 1.0, bg);
                                }
                                p.text(hx, Align2::LEFT_CENTER, format!("{b:02X}"), super::mono(), c);
                                let ch = if (0x20..0x7f).contains(b) { *b as char } else { '·' };
                                p.text(ax, Align2::LEFT_CENTER, ch.to_string(), super::mono(), if differs { c } else { TEXT3() });
                            }
                            None => {
                                if differs {
                                    p.text(hx, Align2::LEFT_CENTER, "--", super::mono(), TEXT3().gamma_multiply(0.6));
                                }
                            }
                        }
                    }
                }
            }
        });
        outs
    }
}

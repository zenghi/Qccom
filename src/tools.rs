//! 嵌入式小工具：位域计算器（寄存器值拆位、改位、取字段）

use eframe::egui::{self, pos2, vec2, Color32, FontFamily, FontId, Rect, RichText, Sense};

use crate::i18n::{t, tf};
use crate::theme::*;

pub struct BitCalc {
    pub open: bool,
    expr: String,
    value: u64,
    width: u32,
    /// 选中的字段 [hi:lo]
    field: Option<(u32, u32)>,
    err: bool,
    focus: bool,
    drag_from: Option<u32>,
}

impl Default for BitCalc {
    fn default() -> Self {
        BitCalc {
            open: false,
            expr: String::new(),
            value: 0,
            width: 32,
            field: None,
            err: false,
            focus: false,
            drag_from: None,
        }
    }
}

fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

fn fit_width(v: u64) -> u32 {
    match v {
        0..=0xFF => 8,
        0x100..=0xFFFF => 16,
        0x1_0000..=0xFFFF_FFFF => 32,
        _ => 64,
    }
}

/// 0b0000_0001 这样按 4 位分组
pub fn bin_grouped(v: u64, width: u32) -> String {
    let mut s = String::new();
    for i in (0..width).rev() {
        s.push(if v >> i & 1 == 1 { '1' } else { '0' });
        if i % 4 == 0 && i != 0 {
            s.push('_');
        }
    }
    s
}

pub fn hex_width(v: u64, width: u32) -> String {
    format!("0x{:0w$X}", v, w = (width / 4) as usize)
}

impl BitCalc {
    /// 从别处（代码里的数字、宏的值）打开
    pub fn show(&mut self, expr: &str, v: Option<i128>) {
        self.open = true;
        self.expr = expr.to_string();
        if let Some(v) = v {
            let u = v as u64;
            let need = if v < 0 { fit_width((!u) << 1).max(32) } else { fit_width(u) };
            self.width = self.width.max(need);
            self.value = u & mask(self.width);
            self.err = false;
        }
        self.field = None;
        self.focus = true;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.focus = self.open;
    }

    /// eval：表达式求值（可以展开工程里的宏）
    pub fn ui(&mut self, ctx: &egui::Context, eval: &dyn Fn(&str) -> Option<i128>) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mono = FontId::new(13.0, FontFamily::Monospace);
        egui::Window::new(t("位域计算器"))
            .id(egui::Id::new("bitcalc"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos(ctx.screen_rect().center() - vec2(280.0, 220.0))
            .frame(egui::Frame::window(&ctx.style()).fill(BG1()).inner_margin(egui::Margin::same(14)))
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
                // ---- 表达式
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.expr)
                        .font(mono.clone())
                        .desired_width(f32::INFINITY)
                        .hint_text(t("数值、C 表达式或宏名，如 0x40021000 | (1 << 5)"))
                        .text_color(if self.err { ERR() } else { TEXT() }),
                );
                if self.focus {
                    r.request_focus();
                    self.focus = false;
                }
                if r.changed() {
                    match eval(self.expr.trim()) {
                        Some(v) => {
                            let u = v as u64;
                            if fit_width(u) > self.width {
                                self.width = fit_width(u);
                            }
                            self.value = u & mask(self.width);
                            self.err = false;
                        }
                        None => self.err = !self.expr.trim().is_empty(),
                    }
                }
                // ---- 位宽
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("位宽")).color(TEXT2()));
                    for w in [8u32, 16, 32, 64] {
                        if ui.selectable_label(self.width == w, format!("{w}")).clicked() {
                            self.width = w;
                            self.value &= mask(w);
                            if self.field.is_some_and(|(hi, _)| hi >= w) {
                                self.field = None;
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(t("清零")).clicked() {
                            self.set(0);
                        }
                        if ui.button(t("取反")).clicked() {
                            self.set(!self.value & mask(self.width));
                        }
                        if ui.button("<<").on_hover_text(t("左移 1 位")).clicked() {
                            self.set((self.value << 1) & mask(self.width));
                        }
                        if ui.button(">>").on_hover_text(t("右移 1 位")).clicked() {
                            self.set(self.value >> 1);
                        }
                    });
                });
                // ---- 位格子
                self.bit_grid(ui);
                ui.label(RichText::new(t("点击切换某一位；拖动选择一段字段")).small().color(TEXT3()));
                // ---- 各种进制
                let v = self.value;
                let signed = if self.width >= 64 { v as i64 } else { ((v << (64 - self.width)) as i64) >> (64 - self.width) };
                let mut rows: Vec<(&str, String)> = vec![
                    ("HEX", hex_width(v, self.width)),
                    ("DEC", v.to_string()),
                    (t("有符号"), signed.to_string()),
                    ("OCT", format!("0{v:o}")),
                    ("BIN", format!("0b{}", bin_grouped(v, self.width))),
                ];
                if self.width <= 32 && (0x20..0x7f).contains(&v) {
                    rows.push((t("字符"), format!("'{}'", v as u8 as char)));
                }
                if self.width == 32 {
                    let f = f32::from_bits(v as u32);
                    let fs = if f == 0.0 || (1e-4..1e9).contains(&f.abs()) { format!("{f}") } else { format!("{f:e}") };
                    rows.push(("float", fs));
                }
                let mut copied = false;
                egui::Grid::new("bitcalc-vals").num_columns(2).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                    for (k, s) in &rows {
                        ui.label(RichText::new(*k).color(TEXT3()));
                        let r = ui.add(egui::Label::new(RichText::new(s).font(mono.clone()).color(TEXT())).sense(Sense::click()));
                        if r.on_hover_text(t("点击复制")).clicked() {
                            ui.ctx().copy_text(s.clone());
                            copied = true;
                        }
                        ui.end_row();
                    }
                });
                // ---- 字段
                if let Some((hi, lo)) = self.field {
                    ui.separator();
                    let n = hi - lo + 1;
                    let fm = mask(n);
                    let fv = (v >> lo) & fm;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(tf("字段 [{}:{}]", &[&hi, &lo])).color(VIOLET()).strong());
                        ui.label(RichText::new(format!("= {}  ({})", fv, hex_width(fv, n.div_ceil(4) * 4))).font(mono.clone()).color(TEXT()));
                    });
                    let ex = format!("(x >> {lo}) & 0x{fm:X}");
                    let mk = format!("0x{:X}", fm << lo);
                    egui::Grid::new("bitcalc-field").num_columns(2).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                        for (k, s) in [(t("读取"), ex), (t("掩码"), mk)] {
                            ui.label(RichText::new(k).color(TEXT3()));
                            let r = ui.add(egui::Label::new(RichText::new(&s).font(mono.clone()).color(TEXT2())).sense(Sense::click()));
                            if r.on_hover_text(t("点击复制")).clicked() {
                                ui.ctx().copy_text(s.clone());
                                copied = true;
                            }
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("改成")).color(TEXT3()));
                        let mut s = fv.to_string();
                        let r = ui.add(egui::TextEdit::singleline(&mut s).font(mono.clone()).desired_width(120.0));
                        if r.changed() {
                            let nv = s.trim().strip_prefix("0x").map_or_else(|| s.trim().parse::<u64>().ok(), |h| u64::from_str_radix(h, 16).ok());
                            if let Some(nv) = nv {
                                self.set((v & !(fm << lo)) | ((nv & fm) << lo));
                            }
                        }
                        if ui.small_button(t("取消选择")).clicked() {
                            self.field = None;
                        }
                    });
                }
                if copied {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("bitcalc-copied"), ui.input(|i| i.time)));
                }
                let at: Option<f64> = ui.ctx().data(|d| d.get_temp(egui::Id::new("bitcalc-copied")));
                if at.is_some_and(|a| ui.input(|i| i.time) - a < 1.2) {
                    ui.label(RichText::new(t("已复制")).small().color(OK()));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                }
            });
        if !open {
            self.open = false;
        }
    }

    fn set(&mut self, v: u64) {
        self.value = v & mask(self.width);
        self.expr = hex_width(self.value, self.width);
        self.err = false;
    }

    fn bit_grid(&mut self, ui: &mut egui::Ui) {
        let per_row = 16u32.min(self.width);
        let rows = self.width.div_ceil(per_row);
        let cell = vec2(32.0, 34.0);
        let gap = 8.0; // 每 4 位之间多留一点
        let w = per_row as f32 * cell.x + (per_row / 4 - 1) as f32 * gap;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, rows as f32 * (cell.y + 6.0)), Sense::click_and_drag());
        let p = ui.painter_at(rect.expand(2.0));
        let mut cells = vec![];
        for r in 0..rows {
            for c in 0..per_row {
                let bit = self.width - 1 - (r * per_row + c);
                let x = rect.left() + c as f32 * cell.x + (c / 4) as f32 * gap;
                let y = rect.top() + r as f32 * (cell.y + 6.0);
                cells.push((bit, Rect::from_min_size(pos2(x, y), cell - vec2(3.0, 0.0))));
            }
        }
        let hover = resp.hover_pos().and_then(|hp| cells.iter().find(|(_, r)| r.contains(hp)).map(|(b, _)| *b));
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let under = pointer.and_then(|pp| cells.iter().find(|(_, r)| r.expand(2.0).contains(pp)).map(|(b, _)| *b));
        if resp.drag_started() {
            self.drag_from = under;
        }
        let live_field = match (self.drag_from, under) {
            (Some(a), Some(b)) if resp.dragged() && a != b => Some((a.max(b), a.min(b))),
            _ => None,
        };
        if resp.drag_stopped() {
            if let (Some(a), Some(b)) = (self.drag_from, under) {
                if a != b {
                    self.field = Some((a.max(b), a.min(b)));
                } else {
                    self.set(self.value ^ (1 << a));
                }
            }
            self.drag_from = None;
        }
        if resp.clicked() {
            if let Some(b) = hover {
                self.set(self.value ^ (1 << b));
            }
        }
        let field = live_field.or(self.field);
        for (bit, r) in &cells {
            let on = self.value >> bit & 1 == 1;
            let in_field = field.is_some_and(|(hi, lo)| (lo..=hi).contains(bit));
            let fill = if on { ACCENT().gamma_multiply(0.85) } else { BG2() };
            p.rect_filled(*r, 4.0, fill);
            let border = if in_field {
                VIOLET()
            } else if hover == Some(*bit) {
                LINE_HI()
            } else {
                LINE()
            };
            p.rect_stroke(*r, 4.0, egui::Stroke::new(if in_field { 1.5f32 } else { 1.0 }, border), egui::StrokeKind::Inside);
            p.text(
                pos2(r.center().x, r.top() + 9.0),
                egui::Align2::CENTER_CENTER,
                bit.to_string(),
                FontId::proportional(9.5),
                if on { ON_ACCENT().gamma_multiply(0.75) } else { TEXT3() },
            );
            p.text(
                pos2(r.center().x, r.bottom() - 11.0),
                egui::Align2::CENTER_CENTER,
                if on { "1" } else { "0" },
                FontId::monospace(14.0),
                if on { ON_ACCENT() } else { TEXT2() },
            );
        }
        if let Some(b) = hover.filter(|_| !resp.dragged()) {
            resp.on_hover_text(format!("bit {b}   1 << {b} = 0x{:X}", 1u64 << b));
        }
    }
}

/// 代码里悬停数字时的小卡片内容：各进制 + 置位的位号
pub fn number_card(ui: &mut egui::Ui, v: i128) {
    let u = v as u64;
    let width = fit_width(u).max(8);
    let mono = FontId::new(12.5, FontFamily::Monospace);
    egui::Grid::new("num-card").num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        let mut row = |k: &str, s: String, c: Color32| {
            ui.label(RichText::new(k).small().color(TEXT3()));
            ui.label(RichText::new(s).font(mono.clone()).color(c));
            ui.end_row();
        };
        row("HEX", hex_width(u, width), crate::highlight::pal::NUM());
        row("DEC", v.to_string(), TEXT());
        row("BIN", bin_grouped(u, width), TEXT2());
        if (0x20..0x7f).contains(&v) {
            row(t("字符"), format!("'{}'", v as u8 as char), TEXT2());
        }
        let bits: Vec<String> = (0..64).filter(|b| u >> b & 1 == 1).map(|b| b.to_string()).collect();
        if !bits.is_empty() && bits.len() <= 12 {
            row(t("置位"), bits.join(", "), TEXT2());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bin_grouped(0xA5, 8), "1010_0101");
        assert_eq!(hex_width(0x271, 16), "0x0271");
        assert_eq!(fit_width(0x1_0000), 32);
        let mut b = BitCalc::default();
        b.show("-1", Some(-1));
        assert_eq!(b.width, 32);
        assert_eq!(b.value, 0xFFFF_FFFF);
    }
}

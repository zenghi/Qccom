//! 表格比较的界面：按关键列对齐两张表，改了的格子显示 旧值 → 新值，数值列多一栏变化量。

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense, Ui};

use super::Out;
use crate::app::{icon_button, toggle_chip, Icon};
use crate::compare::table::{self, Kind, TRow, Table};
use crate::i18n::t;
use crate::theme::*;

const RH: f32 = 22.0;

pub struct TableTab {
    pub a: PathBuf,
    pub b: PathBuf,
    ta: Table,
    tb: Table,
    err: Option<String>,
    /// 关键列；空 = 按行顺序
    key: Vec<usize>,
    rows: Vec<TRow>,
    only_diff: bool,
    /// 按数值变化量排序
    by_delta: bool,
    /// 数值列（算变化量）
    num: Option<usize>,
}

fn num(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|v| v as f64);
    }
    s.parse().ok()
}

impl TableTab {
    pub fn new(a: &Path, b: &Path) -> Self {
        let mut err = None;
        let mut ld = |p: &Path| {
            table::load(p).unwrap_or_else(|e| {
                err.get_or_insert(e);
                Table::default()
            })
        };
        let (ta, tb) = (ld(a), ld(b));
        let key = ta.key.clone();
        // 最后一个大部分是数字的列
        let ncol = ta.header.len();
        let num_col = (0..ncol).rev().find(|&c| {
            let vals: Vec<&String> = ta.rows.iter().take(200).filter_map(|r| r.get(c)).collect();
            !vals.is_empty() && vals.iter().filter(|v| num(v).is_some()).count() * 10 >= vals.len() * 8 && !key.contains(&c)
        });
        let mut x = Self {
            a: a.to_path_buf(),
            b: b.to_path_buf(),
            ta,
            tb,
            err,
            key,
            rows: vec![],
            only_diff: true,
            by_delta: false,
            num: num_col,
        };
        x.recompute();
        x
    }

    fn delta(&self, r: &TRow) -> Option<f64> {
        let c = self.num?;
        let v = |t: &Table, i: Option<usize>| i.and_then(|i| t.rows[i].get(c)).and_then(|s| num(s));
        let (a, b) = (v(&self.ta, r.l), v(&self.tb, r.r));
        match (a, b) {
            (None, None) => None,
            _ => Some(b.unwrap_or(0.0) - a.unwrap_or(0.0)),
        }
    }

    fn recompute(&mut self) {
        self.rows = table::compare(&self.ta, &self.tb, &self.key);
        if self.by_delta {
            let mut rows = std::mem::take(&mut self.rows);
            rows.sort_by(|x, y| {
                let dx = self.delta(x).unwrap_or(0.0).abs();
                let dy = self.delta(y).unwrap_or(0.0).abs();
                dy.partial_cmp(&dx).unwrap_or(std::cmp::Ordering::Equal)
            });
            self.rows = rows;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Out> {
        let mut outs = vec![];
        let header: Vec<String> = if self.ta.header.len() >= self.tb.header.len() {
            self.ta.header.clone()
        } else {
            self.tb.header.clone()
        };
        let counts = self.rows.iter().fold((0, 0, 0), |mut c, r| {
            match r.kind {
                Kind::Changed => c.0 += 1,
                Kind::LeftOnly => c.1 += 1,
                Kind::RightOnly => c.2 += 1,
                Kind::Same => {}
            }
            c
        });
        egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("关键列")).color(TEXT2()));
                let cur = if self.key.is_empty() {
                    t("按行顺序").to_string()
                } else {
                    self.key.iter().filter_map(|&k| header.get(k)).cloned().collect::<Vec<_>>().join(" + ")
                };
                let mut new_key: Option<Vec<usize>> = None;
                egui::ComboBox::from_id_salt("tbl-key").selected_text(cur).width(160.0).show_ui(ui, |ui| {
                    if ui.selectable_label(self.key.is_empty(), t("按行顺序")).clicked() {
                        new_key = Some(vec![]);
                    }
                    if self.ta.key.len() > 1
                        && ui
                            .selectable_label(self.key == self.ta.key, self.ta.key.iter().filter_map(|&k| header.get(k)).cloned().collect::<Vec<_>>().join(" + "))
                            .clicked()
                    {
                        new_key = Some(self.ta.key.clone());
                    }
                    for (i, h) in header.iter().enumerate() {
                        if ui.selectable_label(self.key == [i], h).clicked() {
                            new_key = Some(vec![i]);
                        }
                    }
                });
                if let Some(k) = new_key {
                    self.key = k;
                    self.recompute();
                }
                ui.add_space(6.0);
                crate::app::pill(ui, &format!("{} {}", t("不同"), counts.0), WARN());
                crate::app::pill(ui, &format!("{} {}", t("只在左侧"), counts.1), ERR());
                crate::app::pill(ui, &format!("{} {}", t("只在右侧"), counts.2), OK());
                if let Some(c) = self.num {
                    let total: f64 = self.rows.iter().filter_map(|r| self.delta(r)).sum();
                    if total != 0.0 {
                        let name = header.get(c).cloned().unwrap_or_default();
                        ui.label(
                            RichText::new(format!("Δ {name} {}", fmt_delta(total)))
                                .font(FontId::monospace(11.5))
                                .color(if total > 0.0 { ERR() } else { OK() }),
                        );
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Refresh, &t("重新读取"), false).clicked() {
                        *self = TableTab::new(&self.a.clone(), &self.b.clone());
                    }
                    if icon_button(ui, Icon::Copy, &t("复制差异（CSV）"), false).clicked() {
                        ui.ctx().copy_text(self.csv(&header));
                        outs.push(Out::Toast(t("已复制").into(), OK()));
                    }
                    if self.num.is_some() && toggle_chip(ui, &mut self.by_delta, t("按变化排序"), t("变化量最大的排在前面")) {
                        self.recompute();
                    }
                    toggle_chip(ui, &mut self.only_diff, t("只看不同"), t("隐藏相同的行"));
                });
            });
        });
        if let Some(e) = &self.err {
            ui.label(RichText::new(e).color(ERR()));
        }
        let vis: Vec<usize> = (0..self.rows.len()).filter(|&i| !self.only_diff || self.rows[i].kind != Kind::Same).collect();
        // 列宽：按内容估算
        let cw = ui.fonts(|f| f.glyph_width(&FontId::monospace(12.0), '0'));
        let ncol = header.len();
        let mut widths: Vec<f32> = header.iter().map(|h| h.chars().count() as f32).collect();
        for &i in vis.iter().take(300) {
            let r = &self.rows[i];
            for (c, w) in widths.iter_mut().enumerate() {
                let l = r.l.and_then(|x| self.ta.rows[x].get(c)).map_or(0, |s| s.chars().count());
                let rr = r.r.and_then(|x| self.tb.rows[x].get(c)).map_or(0, |s| s.chars().count());
                let n = if r.kind == Kind::Changed && r.cells.get(c) == Some(&true) { l + rr + 3 } else { l.max(rr) };
                *w = w.max(n as f32);
            }
        }
        let widths: Vec<f32> = widths.iter().map(|w| (w * cw + 20.0).clamp(56.0, 360.0)).collect();
        let delta_w = if self.num.is_some() { 90.0 } else { 0.0 };
        let total_w = 28.0 + widths.iter().sum::<f32>() + delta_w;
        // 表头
        let full = ui.available_rect_before_wrap();
        let hr = Rect::from_min_size(full.min, vec2(full.width(), 26.0));
        ui.allocate_rect(hr, Sense::hover());
        ui.painter().rect_filled(hr, 0.0, BG1());
        ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, st(1.0, LINE()));
        let hx = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("tbl-hx"))).unwrap_or(0.0);
        {
            let p = ui.painter().with_clip_rect(hr);
            let mut x = hr.left() + 28.0 - hx;
            for (c, h) in header.iter().enumerate() {
                let k = self.key.contains(&c);
                p.text(pos2(x + 8.0, hr.center().y), Align2::LEFT_CENTER, h, FontId::new(11.5, bold()), if k { ACCENT() } else { TEXT2() });
                x += widths[c];
            }
            if self.num.is_some() {
                p.text(pos2(x + 8.0, hr.center().y), Align2::LEFT_CENTER, "Δ", FontId::new(11.5, bold()), TEXT2());
            }
        }
        if vis.is_empty() {
            let r = ui.available_rect_before_wrap();
            ui.painter().text(r.center(), Align2::CENTER_CENTER, t("两张表内容相同"), FontId::proportional(14.0), OK());
            return outs;
        }
        ui.spacing_mut().item_spacing.y = 0.0;
        let out = egui::ScrollArea::both().id_salt("tbl-cmp").auto_shrink([false, false]).show_rows(ui, RH, vis.len(), |ui, range| {
            for k in range {
                let r = &self.rows[vis[k]];
                let (rect, _) = ui.allocate_exact_size(vec2(total_w.max(ui.available_width()), RH), Sense::hover());
                let p = ui.painter();
                let (bg, mark, mc) = match r.kind {
                    Kind::Same => (None, "", TEXT3()),
                    Kind::Changed => (Some(WARN().gamma_multiply(0.06)), "~", WARN()),
                    Kind::LeftOnly => (Some(ERR().gamma_multiply(0.10)), "−", ERR()),
                    Kind::RightOnly => (Some(OK().gamma_multiply(0.10)), "+", OK()),
                };
                if let Some(c) = bg {
                    p.rect_filled(rect, 0.0, c);
                }
                p.text(pos2(rect.left() + 14.0, rect.center().y), Align2::CENTER_CENTER, mark, FontId::new(12.0, mono_bold()), mc);
                let mut x = rect.left() + 28.0;
                let font = FontId::monospace(12.0);
                for c in 0..ncol {
                    let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(widths[c], RH));
                    let pc = p.with_clip_rect(cell.shrink2(vec2(4.0, 0.0)).intersect(ui.clip_rect()));
                    let l = r.l.and_then(|i| self.ta.rows[i].get(c)).map(String::as_str);
                    let rr = r.r.and_then(|i| self.tb.rows[i].get(c)).map(String::as_str);
                    let pos = pos2(x + 8.0, rect.center().y);
                    match r.kind {
                        Kind::Changed if r.cells.get(c) == Some(&true) => {
                            let g = pc.layout_no_wrap(l.unwrap_or("").to_string(), font.clone(), ERR());
                            let w = g.size().x;
                            let y = rect.center().y;
                            pc.galley(pos2(pos.x, y - g.size().y / 2.0), g, ERR());
                            pc.hline(pos.x..=pos.x + w, y, st(1.0, ERR().gamma_multiply(0.7)));
                            pc.text(pos2(pos.x + w + 4.0, y), Align2::LEFT_CENTER, "→", font.clone(), TEXT3());
                            pc.text(pos2(pos.x + w + 4.0 + cw * 2.0, y), Align2::LEFT_CENTER, rr.unwrap_or(""), font.clone(), OK());
                        }
                        _ => {
                            let s = l.or(rr).unwrap_or("");
                            let col = if r.kind == Kind::Same || r.kind == Kind::Changed { TEXT2() } else { mc };
                            pc.text(pos, Align2::LEFT_CENTER, s, font.clone(), col);
                        }
                    }
                    x += widths[c];
                }
                if let Some(d) = self.delta(r).filter(|d| *d != 0.0) {
                    p.text(
                        pos2(x + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        fmt_delta(d),
                        FontId::monospace(11.5),
                        if d > 0.0 { ERR() } else { OK() },
                    );
                }
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("tbl-hx"), out.state.offset.x));
        outs
    }

    fn csv(&self, header: &[String]) -> String {
        let q = |s: &str| if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
        let mut o = String::from("status");
        for h in header {
            o.push(',');
            o.push_str(&q(&format!("{h} (L)")));
            o.push(',');
            o.push_str(&q(&format!("{h} (R)")));
        }
        o.push('\n');
        for r in self.rows.iter().filter(|r| r.kind != Kind::Same) {
            o.push_str(match r.kind {
                Kind::Changed => "changed",
                Kind::LeftOnly => "left-only",
                Kind::RightOnly => "right-only",
                Kind::Same => "same",
            });
            for c in 0..header.len() {
                let l = r.l.and_then(|i| self.ta.rows[i].get(c)).map(String::as_str).unwrap_or("");
                let rr = r.r.and_then(|i| self.tb.rows[i].get(c)).map(String::as_str).unwrap_or("");
                o.push(',');
                o.push_str(&q(l));
                o.push(',');
                o.push_str(&q(rr));
            }
            o.push('\n');
        }
        o
    }
}

fn fmt_delta(d: f64) -> String {
    if d.fract() == 0.0 {
        format!("{:+}", d as i64)
    } else {
        format!("{d:+.3}")
    }
}

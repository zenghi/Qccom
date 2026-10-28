//! CSV 查看（v1.8）：表格（只画看得见的行，几十 MB 也不卡）、点列标题选列画曲线，显示最小 / 最大 / 平均值。
//! 分隔符自动认逗号、分号、Tab；第一行不是数字就当表头。

use eframe::egui::{self, pos2, vec2, Color32, FontId, Rect, RichText, Sense, Ui};

use crate::i18n::{t, tf};
use crate::theme::*;

pub fn split_line(l: &str, sep: char) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut q = false;
    let mut it = l.chars().peekable();
    while let Some(c) = it.next() {
        if q {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    cur.push('"');
                    it.next();
                } else {
                    q = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() {
            q = true;
        } else if c == sep {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn guess_sep(sample: &str) -> char {
    let first: Vec<&str> = sample.lines().take(5).collect();
    [',', ';', '\t', '|']
        .into_iter()
        .max_by_key(|c| {
            let n: Vec<usize> = first.iter().map(|l| l.matches(*c).count()).collect();
            // 每行个数一样的更像分隔符
            let same = n.windows(2).all(|w| w[0] == w[1]);
            n.first().copied().unwrap_or(0) * if same { 2 } else { 1 }
        })
        .unwrap_or(',')
}

fn num(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|v| v as f64);
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

pub struct CsvView {
    head: Vec<String>,
    rows: Vec<Vec<Box<str>>>,
    /// 选中画曲线的列
    plot: Vec<usize>,
    /// 每列能当数字的比例高不高
    numeric: Vec<bool>,
    sep: char,
    plot_h: f32,
}

const COLORS: [fn() -> Color32; 4] = [ACCENT, VIOLET, WARN, OK];

impl CsvView {
    pub fn new(text: &str) -> CsvView {
        let sep = guess_sep(&text[..text.len().min(4000)]);
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let first = lines.next().map(|l| split_line(l, sep)).unwrap_or_default();
        let header = first.iter().any(|c| !c.trim().is_empty() && num(c).is_none());
        let mut rows: Vec<Vec<Box<str>>> = vec![];
        if !header {
            rows.push(first.iter().map(|s| s.as_str().into()).collect());
        }
        for l in lines {
            rows.push(split_line(l, sep).into_iter().map(|s| s.into_boxed_str()).collect());
        }
        let ncol = rows.iter().take(1000).map(|r| r.len()).max().unwrap_or(0).max(first.len());
        let head: Vec<String> = if header {
            (0..ncol).map(|i| first.get(i).cloned().unwrap_or_default()).collect()
        } else {
            (0..ncol).map(|i| format!("#{}", i + 1)).collect()
        };
        let numeric = (0..ncol)
            .map(|c| {
                let sample: Vec<&Vec<Box<str>>> = rows.iter().take(200).collect();
                let ok = sample.iter().filter(|r| r.get(c).is_some_and(|v| num(v).is_some())).count();
                ok * 10 >= sample.len().max(1) * 8
            })
            .collect::<Vec<_>>();
        // 默认画第一个数字列（第一列像时间、序号时跳过）
        let plot = numeric.iter().enumerate().skip(1).find(|(_, n)| **n).map(|(i, _)| vec![i]).unwrap_or_default();
        CsvView {
            head,
            rows,
            plot,
            numeric,
            sep,
            plot_h: 180.0,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let sep = match self.sep {
            '\t' => "Tab".to_string(),
            c => format!("'{c}'"),
        };
        ui.label(
            RichText::new(tf("{} 行 × {} 列 · 分隔符 {} · 点列标题选择要画曲线的列（最多 4 列）", &[&self.rows.len(), &self.head.len(), &sep]))
                .small()
                .color(TEXT3()),
        );
        if !self.plot.is_empty() {
            self.plot_ui(ui);
        }
        ui.add_space(6.0);
        let mono = FontId::monospace(12.0);
        let widths: Vec<f32> = (0..self.head.len())
            .map(|c| {
                let n = self
                    .rows
                    .iter()
                    .take(300)
                    .map(|r| r.get(c).map_or(0, |s| s.chars().count()))
                    .max()
                    .unwrap_or(0)
                    .max(self.head[c].chars().count() + 2);
                (n.min(40) as f32 * 7.6 + 18.0).max(60.0)
            })
            .collect();
        let total_w: f32 = widths.iter().sum::<f32>() + 60.0;
        egui::ScrollArea::horizontal().id_salt("csv-h").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_min_width(total_w);
            // 表头
            let (hr, _) = ui.allocate_exact_size(vec2(total_w, 26.0), Sense::hover());
            ui.painter().rect_filled(hr, 0.0, BG2());
            let mut x = hr.left() + 60.0;
            let mut clicked = None;
            for (c, w) in widths.iter().enumerate() {
                let cr = Rect::from_min_size(pos2(x, hr.top()), vec2(*w, hr.height()));
                let r = ui.interact(cr, egui::Id::new(("csv-head", c)), Sense::click());
                let on = self.plot.iter().position(|p| *p == c);
                let col = on.map_or(if self.numeric[c] { TEXT() } else { TEXT3() }, |k| COLORS[k % 4]());
                if r.hovered() && self.numeric[c] {
                    ui.painter().rect_filled(cr, 0.0, tint(8));
                }
                ui.painter().text(
                    cr.left_center() + vec2(8.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    crate::app::truncate(&self.head[c], 40),
                    FontId::proportional(12.0),
                    col,
                );
                if on.is_some() {
                    ui.painter().hline(cr.x_range(), cr.bottom() - 1.0, st(2.0, col));
                }
                if r.clicked() && self.numeric[c] {
                    clicked = Some(c);
                }
                x += w;
            }
            if let Some(c) = clicked {
                if let Some(k) = self.plot.iter().position(|p| *p == c) {
                    self.plot.remove(k);
                } else if self.plot.len() < 4 {
                    self.plot.push(c);
                }
            }
            let row_h = 20.0;
            egui::ScrollArea::vertical()
                .id_salt("csv-v")
                .auto_shrink([false, false])
                .show_rows(ui, row_h, self.rows.len(), |ui, range| {
                    for i in range {
                        let (rr, _) = ui.allocate_exact_size(vec2(total_w, row_h), Sense::hover());
                        if i % 2 == 1 {
                            ui.painter().rect_filled(rr, 0.0, tint(3));
                        }
                        ui.painter()
                            .text(pos2(rr.left() + 50.0, rr.center().y), egui::Align2::RIGHT_CENTER, (i + 1).to_string(), mono.clone(), TEXT3());
                        let mut x = rr.left() + 60.0;
                        for (c, w) in widths.iter().enumerate() {
                            if let Some(v) = self.rows[i].get(c) {
                                let col = if self.numeric[c] { TEXT() } else { TEXT2() };
                                let cr = Rect::from_min_size(pos2(x, rr.top()), vec2(*w - 6.0, row_h));
                                ui.painter()
                                    .with_clip_rect(cr)
                                    .text(cr.left_center() + vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, v.as_ref(), mono.clone(), col);
                            }
                            x += w;
                        }
                    }
                });
        });
    }

    fn plot_ui(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, self.plot_h), Sense::hover());
        let p = ui.painter_at(rect);
        p.rect(rect, R_CARD, BG2(), st(1.0, LINE()), egui::StrokeKind::Inside);
        let inner = rect.shrink2(vec2(10.0, 12.0));
        let n = self.rows.len().max(1);
        let mut stats = vec![];
        for (k, &c) in self.plot.iter().enumerate() {
            let vals: Vec<Option<f64>> = self.rows.iter().map(|r| r.get(c).and_then(|v| num(v))).collect();
            let good: Vec<f64> = vals.iter().flatten().copied().collect();
            if good.is_empty() {
                continue;
            }
            let (lo, hi) = good.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let avg = good.iter().sum::<f64>() / good.len() as f64;
            stats.push((k, c, lo, hi, avg));
            let span = (hi - lo).max(1e-12);
            let col = COLORS[k % 4]();
            // 每个像素一列：取这一列里的最小、最大值画竖线，几十万行也快
            let px = inner.width().max(1.0) as usize;
            let mut pts = vec![];
            for xi in 0..px {
                let a = xi * n / px;
                let b = ((xi + 1) * n / px).max(a + 1).min(n);
                let mut mm: Option<(f64, f64)> = None;
                for v in vals[a..b].iter().flatten() {
                    mm = Some(mm.map_or((*v, *v), |(x, y)| (x.min(*v), y.max(*v))));
                }
                if let Some((x0, x1)) = mm {
                    let y = |v: f64| inner.bottom() - ((v - lo) / span) as f32 * inner.height();
                    let x = inner.left() + xi as f32;
                    if x1 > x0 {
                        p.vline(x, egui::Rangef::new(y(x1), y(x0)), st(1.0, col));
                    }
                    pts.push(pos2(x, y((x0 + x1) / 2.0)));
                }
            }
            p.add(egui::Shape::line(pts, st(1.4, col)));
        }
        if let Some(pp) = resp.hover_pos() {
            let i = (((pp.x - inner.left()) / inner.width()).clamp(0.0, 1.0) * (n - 1) as f32) as usize;
            p.vline(pp.x, inner.y_range(), st(1.0, TEXT3()));
            let txt: Vec<String> = self
                .plot
                .iter()
                .map(|&c| format!("{} = {}", self.head[c], self.rows.get(i).and_then(|r| r.get(c)).map_or("", |v| v.as_ref())))
                .collect();
            resp.on_hover_text(format!("{} {}\n{}", t("第"), i + 1, txt.join("\n")));
        }
        ui.horizontal_wrapped(|ui| {
            for (k, c, lo, hi, avg) in stats {
                ui.label(RichText::new(format!("━ {}", self.head[c])).color(COLORS[k % 4]()));
                ui.label(
                    RichText::new(format!("{} {}  {} {}  {} {}", t("最小"), fmt(lo), t("最大"), fmt(hi), t("平均"), fmt(avg)))
                        .small()
                        .color(TEXT2()),
                );
                ui.add_space(12.0);
            }
        });
    }
}

fn fmt(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        format!("{}", v as i64)
    } else {
        format!("{v:.4}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_parse() {
        assert_eq!(split_line("a,\"b,c\",\"d\"\"e\"", ','), vec!["a", "b,c", "d\"e"]);
        let v = CsvView::new("time;temp;note\n1;20.5;ok\n2;21;\n");
        assert_eq!(v.sep, ';');
        assert_eq!(v.rows.len(), 2);
        assert_eq!(v.plot, vec![1]);
        assert!(!v.numeric[2]);
        let v = CsvView::new("1,2\n3,4\n");
        assert_eq!(v.head[0], "#1");
        assert_eq!(v.rows.len(), 2);
    }
}

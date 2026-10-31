//! 底部的示波器和逻辑分析仪：画仿真里探针 / 逻辑分析仪通道记下的波形，逻辑分析仪可以导出 VCD。

use std::fmt::Write as _;
use std::path::PathBuf;

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke};

use super::draw;
use crate::circuit::sim::{Overlay, Sim, Trace};
use crate::circuit::{fmt_si, Circuit};
use crate::i18n::t;
use crate::theme::*;

/// 1-2-5 档位
const TDIVS: [f64; 19] = [1e-6, 2e-6, 5e-6, 1e-5, 2e-5, 5e-5, 1e-4, 2e-4, 5e-4, 1e-3, 2e-3, 5e-3, 1e-2, 2e-2, 5e-2, 0.1, 0.2, 0.5, 1.0];

#[derive(Default)]
pub struct ScopeUi {
    /// 每格时间，0 = 自动
    pub tdiv: f64,
    /// 用 CH1 上升沿触发（波形稳定不滚动）
    pub trig: bool,
    /// 停住：记下当时的结束时刻
    pub hold: Option<f64>,
    pub la_tdiv: f64,
    pub la_hold: Option<f64>,
}

fn nice_tdiv(x: f64) -> f64 {
    TDIVS.iter().copied().find(|v| *v >= x).unwrap_or(1.0)
}

/// 电压读数：很小的当 0
fn fv(v: f32) -> String {
    if v.abs() < 1e-3 {
        "0 V".into()
    } else {
        fmt_si(v as f64, "V")
    }
}

fn fmt_tdiv(v: f64) -> String {
    fmt_si(v, "s").replace(' ', "")
}

/// 一个通道最近的上升沿（过 level），返回时刻列表（最多看最近 2 秒）
fn rising(tr: &Trace, level: f32, from: f64) -> Vec<f64> {
    let mut out = vec![];
    let i0 = tr.s.partition_point(|x| x.0 < from);
    for k in i0.max(1)..tr.s.len() {
        let (a, b) = (tr.s[k - 1], tr.s[k]);
        if a.1 < level && b.1 >= level {
            let f = ((level - a.1) / (b.1 - a.1).max(1e-9)) as f64;
            out.push(a.0 + (b.0 - a.0) * f);
        }
    }
    out
}

/// 周期估计：最近几个上升沿的平均间隔
fn period(tr: &Trace, now: f64) -> Option<(f64, f32, f32)> {
    let (lo, hi) = tr.range(now - 1.0, now)?;
    if hi - lo < 0.05 {
        return None;
    }
    let mid = (lo + hi) / 2.0;
    let r = rising(tr, mid, now - 1.0);
    if r.len() < 3 {
        return None;
    }
    let n = r.len().min(20);
    let p = (r[r.len() - 1] - r[r.len() - n]) / (n - 1) as f64;
    (p > 0.0).then_some((p, lo, hi))
}

/// 逻辑电平：有协议波形补充（DHT、串口）的时间段用补充的边沿，其它时候按 1.5 V 门限
fn level_at(tr: &Trace, ov: Option<&Overlay>, t: f64) -> bool {
    if let Some(o) = ov {
        if let (Some(f), Some(l)) = (o.edges.first(), o.edges.last()) {
            if t >= f.0 && t <= l.0 {
                let i = o.edges.partition_point(|e| e.0 <= t);
                return o.edges[i - 1].1;
            }
        }
    }
    tr.at(t).unwrap_or(0.0) >= 1.5
}

pub fn scope_ui(ui: &mut egui::Ui, st: &mut ScopeUi, c: &Circuit, s: &Sim) {
    let chans: Vec<(&Trace, String, Color32)> = s
        .traces
        .iter()
        .filter_map(|tr| {
            let p = c.part(tr.part)?;
            (p.kind == "probe").then(|| {
                let ch = p.prop("ch").to_string();
                (tr, format!("CH{ch}"), draw::probe_color(&ch))
            })
        })
        .collect();
    if chans.is_empty() {
        ui.label(RichText::new(t("从元件库「仪器」里放一个示波器探针，探针尖接到要看的地方。")).color(TEXT3()));
        return;
    }
    let now = st.hold.unwrap_or(s.t);
    let auto = period(chans[0].0, now);
    let tdiv = if st.tdiv > 0.0 { st.tdiv } else { auto.map_or(0.1, |(p, ..)| nice_tdiv(p * 2.5 / 10.0)) };
    let span = tdiv * 10.0;
    ui.horizontal(|ui| {
        let txt = if st.tdiv > 0.0 { fmt_tdiv(st.tdiv) } else { format!("{} {}", t("自动"), fmt_tdiv(tdiv)) };
        egui::ComboBox::from_id_salt("cv-tdiv").width(110.0).selected_text(format!("{txt}/div")).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.tdiv, 0.0, t("自动"));
            for v in TDIVS {
                ui.selectable_value(&mut st.tdiv, v, format!("{}/div", fmt_tdiv(v)));
            }
        });
        crate::app::toggle_chip(ui, &mut st.trig, t("触发"), t("用第一个通道的上升沿触发，波形停在屏幕上不滚动"));
        let mut held = st.hold.is_some();
        if crate::app::toggle_chip(ui, &mut held, t("停住"), t("冻结画面仔细看")) {
            st.hold = held.then_some(s.t);
        }
        for (tr, name, col) in &chans {
            let mut m = name.clone();
            if let Some((lo, hi)) = tr.range(now - span, now) {
                let _ = write!(m, "  {}–{}", fv(lo), fv(hi));
            }
            if let Some((p, ..)) = period(tr, now) {
                let _ = write!(m, "  {}", fmt_si(1.0 / p, "Hz"));
            }
            ui.label(RichText::new(m).font(FontId::monospace(11.5)).color(*col));
        }
    });
    // 时间窗口
    let (t0, t1) = if st.trig {
        let (lo, hi) = chans[0].0.range(now - 1.0, now).unwrap_or((0.0, 1.0));
        let r = rising(chans[0].0, (lo + hi) / 2.0, now - (span * 4.0).max(0.05));
        match r.iter().rev().find(|x| **x <= now - span / 2.0) {
            Some(tc) => (tc - span / 2.0, tc + span / 2.0),
            None => (now - span, now),
        }
    } else {
        (now - span, now)
    };
    // 电压范围：所有通道一起，含 0 V
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for (tr, ..) in &chans {
        if let Some((a, b)) = tr.range(t0, t1) {
            lo = lo.min(a);
            hi = hi.max(b);
        }
    }
    let pad = (hi - lo) * 0.1;
    let (lo, hi) = (lo - pad, hi + pad);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ui.available_height().max(120.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_rgb(0x0c, 0x10, 0x14));
    let grid = Color32::from_white_alpha(18);
    for k in 0..=10 {
        let x = rect.left() + rect.width() * k as f32 / 10.0;
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, grid));
    }
    for k in 0..=6 {
        let y = rect.top() + rect.height() * k as f32 / 6.0;
        p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, grid));
    }
    let ymap = |v: f32| rect.bottom() - (v - lo) / (hi - lo).max(1e-6) * rect.height();
    // 0 V 线
    p.line_segment([pos2(rect.left(), ymap(0.0)), pos2(rect.right(), ymap(0.0))], Stroke::new(1.0, Color32::from_white_alpha(45)));
    let w = rect.width().max(1.0) as usize;
    for (tr, _, col) in &chans {
        let mut prev: Option<(f32, f32)> = None;
        let mut line = vec![];
        for x in 0..w {
            let ta = t0 + span * x as f64 / w as f64;
            let tb = t0 + span * (x + 1) as f64 / w as f64;
            if ta > s.t {
                break;
            }
            let Some((a, b)) = tr.range(ta, tb.min(s.t)) else { continue };
            let px = rect.left() + x as f32;
            if (b - a).abs() * rect.height() / (hi - lo) > 1.5 {
                p.line_segment([pos2(px, ymap(a)), pos2(px, ymap(b))], Stroke::new(1.4, *col));
            }
            line.push(pos2(px, ymap((a + b) / 2.0)));
            prev = Some((a, b));
        }
        let _ = prev;
        p.add(egui::Shape::line(line, Stroke::new(1.6, *col)));
    }
    let lab = FontId::monospace(10.5);
    p.text(rect.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, fv(hi), lab.clone(), TEXT3());
    p.text(rect.left_bottom() + vec2(6.0, -4.0), Align2::LEFT_BOTTOM, fv(lo), lab.clone(), TEXT3());
    p.text(rect.right_bottom() + vec2(-6.0, -4.0), Align2::RIGHT_BOTTOM, format!("{}/div", fmt_tdiv(tdiv)), lab, TEXT3());
}

/// 逻辑分析仪：返回要导出的 VCD 文件（点了导出时）
pub fn logic_ui(ui: &mut egui::Ui, st: &mut ScopeUi, c: &Circuit, s: &Sim) -> Option<PathBuf> {
    // 只显示接了线的通道
    let chans: Vec<(&Trace, String)> = s
        .traces
        .iter()
        .filter(|tr| c.part(tr.part).is_some_and(|p| p.kind == "logic"))
        .filter(|tr| s.nl.members.get(tr.net).is_some_and(|m| m.len() > 1))
        .map(|tr| {
            let name = c.part(tr.part).and_then(|p| crate::circuit::lib::def(&p.kind)).map_or(String::new(), |d| d.pins[tr.pin].name.clone());
            (tr, name)
        })
        .collect();
    let mut export = None;
    let now = st.la_hold.unwrap_or(s.t);
    let tdiv = if st.la_tdiv > 0.0 { st.la_tdiv } else { 1e-3 };
    let span = tdiv * 10.0;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("cv-la-tdiv")
            .width(110.0)
            .selected_text(format!("{}/div", fmt_tdiv(tdiv)))
            .show_ui(ui, |ui| {
                for v in TDIVS {
                    ui.selectable_value(&mut st.la_tdiv, v, format!("{}/div", fmt_tdiv(v)));
                }
            });
        let mut held = st.la_hold.is_some();
        if crate::app::toggle_chip(ui, &mut held, t("停住"), t("冻结画面仔细看")) {
            st.la_hold = held.then_some(s.t);
        }
        if ui
            .add_enabled(!chans.is_empty(), egui::Button::new(t("导出 VCD…")))
            .on_hover_text(t("导出最近 10 秒的波形，可以在工具箱的「VCD 波形」或 PulseView 里看"))
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().set_title(t("导出 VCD")).add_filter("VCD", &["vcd"]).set_file_name("circuit.vcd").save_file() {
                let txt = vcd(&chans, s);
                if std::fs::write(&path, txt).is_ok() {
                    export = Some(path);
                }
            }
        }
    });
    if chans.is_empty() {
        ui.label(RichText::new(t("从元件库「仪器」里放一个逻辑分析仪，CH0–CH7 接信号，GND 接地。")).color(TEXT3()));
        return export;
    }
    let row_h = 24.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), (row_h * chans.len() as f32 + 8.0).max(40.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_rgb(0x0c, 0x10, 0x14));
    let (t0, _) = (now - span, now);
    let label_w = 44.0;
    let plot = Rect::from_min_max(pos2(rect.left() + label_w, rect.top() + 4.0), pos2(rect.right() - 6.0, rect.bottom() - 4.0));
    for k in 0..=10 {
        let x = plot.left() + plot.width() * k as f32 / 10.0;
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, Color32::from_white_alpha(14)));
    }
    let col = Color32::from_rgb(0x4d, 0xe0, 0x8a);
    let w = plot.width().max(1.0) as usize;
    for (k, (tr, name)) in chans.iter().enumerate() {
        let y0 = plot.top() + k as f32 * row_h;
        let (yh, yl) = (y0 + 5.0, y0 + row_h - 5.0);
        p.text(pos2(rect.left() + 6.0, y0 + row_h / 2.0), Align2::LEFT_CENTER, name, FontId::monospace(11.0), TEXT2());
        let ov = s.overlay.get(&tr.net);
        let y = |v: bool| if v { yh } else { yl };
        let mut line: Vec<egui::Pos2> = vec![];
        for x in 0..w {
            let ta = t0 + span * x as f64 / w as f64;
            if ta > s.t {
                break;
            }
            let tb = t0 + span * (x + 1) as f64 / w as f64;
            let a = level_at(tr, ov, ta);
            let b = level_at(tr, ov, tb.min(s.t));
            let px = plot.left() + x as f32;
            if line.last().is_none_or(|q| q.y != y(a)) {
                line.push(pos2(px, y(a)));
            }
            if a != b {
                // 一个像素里有翻转：画竖线
                line.push(pos2(px, y(a)));
                line.push(pos2(px, y(!a)));
                if !a != b {
                    line.push(pos2(px, y(b)));
                }
            }
            line.push(pos2(px + 1.0, y(b)));
        }
        p.add(egui::Shape::line(line, Stroke::new(1.4, col)));
    }
    p.text(
        rect.right_bottom() + vec2(-6.0, -2.0),
        Align2::RIGHT_BOTTOM,
        format!("{}/div", fmt_tdiv(tdiv)),
        FontId::monospace(10.5),
        TEXT3(),
    );
    export
}

/// 生成 VCD：每个通道按 1.5 V 门限转成边沿，再并上协议补充的边沿
fn vcd(chans: &[(&Trace, String)], s: &Sim) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "$date QCCOM circuit simulation $end\n$version QCCOM $end\n$timescale 1ns $end\n$scope module circuit $end");
    let ids: Vec<char> = (0..chans.len()).map(|k| (b'!' + k as u8) as char).collect();
    for (k, (_, name)) in chans.iter().enumerate() {
        let _ = writeln!(out, "$var wire 1 {} {} $end", ids[k], name);
    }
    let _ = writeln!(out, "$upscope $end\n$enddefinitions $end");
    let t_start = chans.iter().filter_map(|(tr, _)| tr.s.front().map(|f| f.0)).fold(f64::MAX, f64::min);
    if t_start == f64::MAX {
        return out;
    }
    // 所有边沿：(时刻, 通道, 电平)
    let mut ev: Vec<(f64, usize, bool)> = vec![];
    for (k, (tr, _)) in chans.iter().enumerate() {
        let ov = s.overlay.get(&tr.net);
        let mut last = level_at(tr, ov, t_start);
        ev.push((t_start, k, last));
        let mut times: Vec<f64> = tr.s.iter().map(|x| x.0).collect();
        if let Some(o) = ov {
            times.extend(o.edges.iter().map(|e| e.0).filter(|x| *x >= t_start));
        }
        times.sort_by(|a, b| a.total_cmp(b));
        for x in times {
            let v = level_at(tr, ov, x);
            if v != last {
                ev.push((x, k, v));
                last = v;
            }
        }
    }
    ev.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cur_t = u64::MAX;
    for (x, k, v) in ev {
        let ns = ((x - t_start) * 1e9).round() as u64;
        if ns != cur_t {
            let _ = writeln!(out, "#{ns}");
            cur_t = ns;
        }
        let _ = writeln!(out, "{}{}", if v { 1 } else { 0 }, ids[k]);
    }
    out
}

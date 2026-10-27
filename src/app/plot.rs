//! 波形图：从日志里提取数值实时画曲线。
//! 自动识别 `temp=25.3`、`vbat: 3.71`、`x:1,y:2` 这类“名字 = 数字”，或整行就是逗号 / 空格分隔的数字；
//! 也可以写正则，每个括号是一条曲线（命名分组用组名）。

use std::collections::BTreeMap;

use super::*;

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct PlotCfg {
    pub on: bool,
    pub height: f32,
    /// 自定义正则；空 = 自动识别
    pub regex: String,
    /// 横轴显示最近多少秒
    pub window_s: f32,
}

impl Default for PlotCfg {
    fn default() -> Self {
        PlotCfg {
            on: false,
            height: 220.0,
            regex: String::new(),
            window_s: 30.0,
        }
    }
}

const MAX_POINTS: usize = 20_000;

pub(super) struct Series {
    pub pts: VecDeque<(f64, f64)>,
    pub color: Color32,
    pub hidden: bool,
    /// 数值变过（一直不变的量如版本号、地址默认不画，免得把刻度拉偏）
    pub varies: bool,
    /// 用户手动点开的不变量
    pub pinned: bool,
}

impl Series {
    pub fn shown(&self) -> bool {
        !self.hidden && (self.varies || self.pinned)
    }
}

pub(super) struct Plot {
    t0: Instant,
    pub series: BTreeMap<String, Series>,
    re: Option<regex_lite::Regex>,
    re_src: String,
    re_err: bool,
    /// 冻结：不再滚动（数据照收）
    frozen: Option<f64>,
    auto: regex_lite::Regex,
    editing: bool,
}

impl Default for Plot {
    fn default() -> Self {
        Plot {
            t0: Instant::now(),
            series: BTreeMap::new(),
            re: None,
            re_src: String::new(),
            re_err: false,
            frozen: None,
            auto: regex_lite::Regex::new(r"([A-Za-z_][\w.\[\]]*)\s*[:=]\s*(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)").unwrap(),
            editing: false,
        }
    }
}

fn series_color(i: usize) -> Color32 {
    const C: [Color32; 8] = [
        Color32::from_rgb(0x22, 0xd3, 0xee),
        Color32::from_rgb(0xa7, 0x8b, 0xfa),
        Color32::from_rgb(0x4a, 0xde, 0x80),
        Color32::from_rgb(0xfb, 0xbf, 0x24),
        Color32::from_rgb(0xf8, 0x71, 0x71),
        Color32::from_rgb(0xf4, 0x72, 0xb6),
        Color32::from_rgb(0x60, 0xa5, 0xfa),
        Color32::from_rgb(0xa3, 0xe6, 0x35),
    ];
    ink(C[i % C.len()])
}

/// 从一行里提取 (名字, 数值)
pub(super) fn extract(auto: &regex_lite::Regex, re: Option<&regex_lite::Regex>, line: &str) -> Vec<(String, f64)> {
    if let Some(re) = re {
        let Some(c) = re.captures(line) else { return vec![] };
        let names: Vec<Option<&str>> = re.capture_names().collect();
        return (1..c.len())
            .filter_map(|i| {
                let v = c.get(i)?.as_str().trim().parse::<f64>().ok()?;
                let name = names.get(i).copied().flatten().map_or_else(|| format!("${i}"), str::to_string);
                Some((name, v))
            })
            .collect();
    }
    let kv: Vec<(String, f64)> = auto.captures_iter(line).filter_map(|c| Some((c[1].to_string(), c[2].parse().ok()?))).collect();
    if !kv.is_empty() {
        return kv;
    }
    // 整行都是数字（CSV / 空格分隔）：按列编号
    let parts: Vec<&str> = line.split([',', ' ', '\t', ';']).filter(|s| !s.is_empty()).collect();
    if !parts.is_empty() && parts.len() <= 16 {
        let nums: Vec<f64> = parts.iter().filter_map(|s| s.parse().ok()).collect();
        if nums.len() == parts.len() {
            return nums.into_iter().enumerate().map(|(i, v)| (format!("#{}", i + 1), v)).collect();
        }
    }
    vec![]
}

impl App {
    /// 新收完一行时调用
    pub(super) fn plot_line(&mut self, text: &str) {
        if !self.s.plot.on {
            return;
        }
        let p = &mut self.plot;
        if p.re_src != self.s.plot.regex {
            p.re_src = self.s.plot.regex.clone();
            p.re = None;
            p.re_err = false;
            if !p.re_src.is_empty() {
                match regex_lite::Regex::new(&p.re_src) {
                    Ok(r) => p.re = Some(r),
                    Err(_) => p.re_err = true,
                }
            }
        }
        if p.re_err {
            return;
        }
        let vals = extract(&p.auto, p.re.as_ref(), text);
        if vals.is_empty() {
            return;
        }
        let now = p.t0.elapsed().as_secs_f64();
        for (name, v) in vals {
            let n = p.series.len();
            if n >= 16 && !p.series.contains_key(&name) {
                continue;
            }
            let s = p.series.entry(name).or_insert_with(|| Series {
                pts: VecDeque::new(),
                color: series_color(n),
                hidden: false,
                varies: false,
                pinned: false,
            });
            if s.pts.front().is_some_and(|p| p.1 != v) {
                s.varies = true;
            }
            s.pts.push_back((now, v));
            if s.pts.len() > MAX_POINTS {
                s.pts.pop_front();
            }
        }
    }

    pub(super) fn plot_pane(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter_at(rect);
        p.rect(rect, R_CARD, BG0(), st(1.0, LINE()), egui::StrokeKind::Inside);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))));
        let ui = &mut child;
        // ---- 顶部：曲线开关、设置
        let mut close = false;
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(t("波形")).font(FontId::new(12.0, bold())).color(TEXT2()));
            if self.plot.series.is_empty() {
                ui.label(RichText::new(t("等待数据：自动识别 temp=25.3、x:1,y:2 或逗号分隔的数字")).small().color(TEXT3()));
            }
            // 图例：正在画的排前面；放不下的收进「+N」
            let room = ui.available_width() - 250.0;
            let mut names: Vec<String> = self.plot.series.keys().cloned().collect();
            names.sort_by_key(|n| !self.plot.series[n].shown());
            let mut used = 0.0;
            let mut rest = vec![];
            for name in names {
                let s = &self.plot.series[&name];
                let last = s.pts.back().map_or(String::new(), |p| fmt_num(p.1));
                let label = format!("● {name} {last}");
                let w = ui.fonts(|f| f.layout_no_wrap(label.clone(), FontId::new(11.5, FontFamily::Monospace), TEXT()).size().x) + 16.0;
                if used + w > room {
                    rest.push(name);
                    continue;
                }
                used += w + 6.0;
                legend_chip(ui, &label, self.plot.series.get_mut(&name).unwrap());
            }
            if !rest.is_empty() {
                let r = ui.add(
                    egui::Button::new(RichText::new(format!("+{}", rest.len())).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT2()))
                        .fill(Color32::TRANSPARENT)
                        .stroke(st(1.0, LINE()))
                        .min_size(vec2(0.0, 22.0)),
                );
                let pop = egui::Id::new("plot_more");
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pop));
                }
                egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                    ui.set_min_width(160.0);
                    for name in rest {
                        let s = self.plot.series.get_mut(&name).unwrap();
                        let last = s.pts.back().map_or(String::new(), |p| fmt_num(p.1));
                        legend_chip(ui, &format!("● {name} {last}"), s);
                    }
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Close, t("关闭波形图"), false).clicked() {
                    close = true;
                }
                if icon_button(ui, Icon::Trash, t("清空曲线"), false).clicked() {
                    clear = true;
                }
                let frozen = self.plot.frozen.is_some();
                if icon_button(
                    ui,
                    if frozen { Icon::Play } else { Icon::Pause },
                    if frozen { t("继续滚动") } else { t("冻结画面（数据照收）") },
                    frozen,
                )
                .clicked()
                {
                    self.plot.frozen = if frozen { None } else { Some(self.plot.t0.elapsed().as_secs_f64()) };
                }
                if icon_button(ui, Icon::Edit, t("自定义提取规则"), self.plot.editing).clicked() {
                    self.plot.editing = !self.plot.editing;
                }
                egui::ComboBox::from_id_salt("plotwin")
                    .width(58.0)
                    .selected_text(format!("{}s", self.s.plot.window_s))
                    .show_ui(ui, |ui| {
                        for w in [5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0] {
                            ui.selectable_value(&mut self.s.plot.window_s, w, format!("{w}s"));
                        }
                    });
            });
        });
        if self.plot.editing {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("正则")).small().color(TEXT3()));
                ui.add(
                    egui::TextEdit::singleline(&mut self.s.plot.regex)
                        .font(FontId::new(12.0, FontFamily::Monospace))
                        .desired_width(360.0)
                        .hint_text(r"T=(?P<temp>[\d.]+) H=(?P<hum>[\d.]+)")
                        .text_color(if self.plot.re_err { ERR() } else { TEXT() }),
                );
                ui.label(RichText::new(t("留空 = 自动识别；每个括号一条曲线")).small().color(TEXT3()));
            });
        }
        if close {
            self.s.plot.on = false;
        }
        if clear {
            self.plot.series.clear();
            self.plot.t0 = Instant::now();
        }
        // ---- 画图区
        let top = ui.min_rect().bottom() + 4.0;
        let area = Rect::from_min_max(pos2(rect.left() + 52.0, top), pos2(rect.right() - 12.0, rect.bottom() - 20.0));
        if area.height() < 20.0 || area.width() < 40.0 {
            return;
        }
        let resp = ui.interact(area, ui.id().with("plotarea"), Sense::hover());
        // 滚轮缩放时间窗口
        if resp.hovered() {
            let dy = ui.input(|i| i.raw_scroll_delta.y);
            if dy != 0.0 {
                let w = (self.s.plot.window_s * if dy > 0.0 { 0.8 } else { 1.25 }).clamp(1.0, 3600.0);
                self.s.plot.window_s = (w * 10.0).round() / 10.0;
            }
        }
        let now = self.plot.frozen.unwrap_or_else(|| self.plot.t0.elapsed().as_secs_f64());
        let span = self.s.plot.window_s as f64;
        let x0 = now - span;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for s in self.plot.series.values().filter(|s| s.shown()) {
            for &(t, v) in s.pts.iter().rev().take_while(|p| p.0 >= x0 - 1.0) {
                let _ = t;
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if !lo.is_finite() {
            (lo, hi) = (0.0, 1.0);
        }
        if (hi - lo).abs() < 1e-9 {
            lo -= 1.0;
            hi += 1.0;
        }
        let pad = (hi - lo) * 0.08;
        let (lo, hi) = (lo - pad, hi + pad);
        let p = ui.painter_at(rect);
        let to_screen = |t: f64, v: f64| pos2(area.left() + ((t - x0) / span) as f32 * area.width(), area.bottom() - ((v - lo) / (hi - lo)) as f32 * area.height());
        // 网格和刻度
        let font = FontId::new(10.5, FontFamily::Monospace);
        for k in 0..=4 {
            let v = lo + (hi - lo) * k as f64 / 4.0;
            let y = to_screen(x0, v).y;
            p.line_segment([pos2(area.left(), y), pos2(area.right(), y)], st(1.0, LINE().gamma_multiply(0.6)));
            p.text(pos2(area.left() - 6.0, y), Align2::RIGHT_CENTER, fmt_num(v), font.clone(), TEXT3());
        }
        for k in 0..=5 {
            let tt = x0 + span * k as f64 / 5.0;
            let x = to_screen(tt, lo).x;
            p.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], st(1.0, LINE().gamma_multiply(0.35)));
            p.text(pos2(x, area.bottom() + 3.0), Align2::CENTER_TOP, format!("{:.0}s", (tt - now).round() + 0.0), font.clone(), TEXT3());
        }
        // 曲线
        let clip = p.with_clip_rect(area.expand(1.0));
        for s in self.plot.series.values().filter(|s| s.shown()) {
            let pts: Vec<egui::Pos2> = s.pts.iter().skip_while(|p| p.0 < x0 - span * 0.05).map(|&(t, v)| to_screen(t, v)).collect();
            if pts.len() >= 2 {
                clip.add(Shape::line(pts.clone(), st(1.6, s.color)));
            }
            if let Some(&last) = pts.last() {
                clip.circle_filled(last, 2.5, s.color);
            }
        }
        // 悬停：竖线 + 各曲线最近的值
        if let Some(h) = resp.hover_pos() {
            let tt = x0 + ((h.x - area.left()) / area.width()) as f64 * span;
            p.line_segment([pos2(h.x, area.top()), pos2(h.x, area.bottom())], st(1.0, TEXT3()));
            let mut lines = vec![format!("{:.2}s", tt - now)];
            for (name, s) in self.plot.series.iter().filter(|(_, s)| s.shown()) {
                let i = s.pts.partition_point(|p| p.0 < tt);
                let near = [i.checked_sub(1), Some(i)]
                    .into_iter()
                    .flatten()
                    .filter_map(|k| s.pts.get(k))
                    .min_by(|a, b| (a.0 - tt).abs().total_cmp(&(b.0 - tt).abs()));
                if let Some(&(pt, v)) = near {
                    clip.circle_stroke(to_screen(pt, v), 4.0, st(1.5, s.color));
                    lines.push(format!("{name} = {}", fmt_num(v)));
                }
            }
            egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), egui::Id::new("plottip"), |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for l in lines {
                    ui.label(RichText::new(l).font(FontId::new(12.0, FontFamily::Monospace)));
                }
            });
        }
        if self.plot.frozen.is_none() && !self.plot.series.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn legend_chip(ui: &mut egui::Ui, label: &str, s: &mut Series) {
    let on = s.shown();
    let col = if on { s.color } else { TEXT3() };
    let r = ui.add(
        egui::Button::new(RichText::new(label).font(FontId::new(11.5, FontFamily::Monospace)).color(col))
            .fill(Color32::TRANSPARENT)
            .stroke(st(1.0, if on { s.color.gamma_multiply(0.4) } else { LINE() }))
            .min_size(vec2(0.0, 22.0)),
    );
    let tip = if !s.varies && !s.hidden && !s.pinned {
        t("数值一直没变，默认不画；点击显示")
    } else {
        t("点击显示 / 隐藏")
    };
    if r.on_hover_text(tip).clicked() {
        if on {
            s.hidden = true;
            s.pinned = false;
        } else {
            s.hidden = false;
            s.pinned = true;
        }
    }
}

pub(super) fn fmt_num(v: f64) -> String {
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e7).contains(&a) {
        format!("{v:.3e}")
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_values() {
        let p = Plot::default();
        let v = extract(&p.auto, None, "I (123) sensor: temp=25.5 hum: 60 vbat=3.71V");
        assert_eq!(v, vec![("temp".into(), 25.5), ("hum".into(), 60.0), ("vbat".into(), 3.71)]);
        assert_eq!(extract(&p.auto, None, "1.5, -2, 3"), vec![("#1".into(), 1.5), ("#2".into(), -2.0), ("#3".into(), 3.0)]);
        assert!(extract(&p.auto, None, "boot ok").is_empty());
        let re = regex_lite::Regex::new(r"T=(?P<t>[\d.]+) (\d+)").unwrap();
        assert_eq!(extract(&p.auto, Some(&re), "T=21.5 7"), vec![("t".into(), 21.5), ("$2".into(), 7.0)]);
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(3.25), "3.25");
    }
}

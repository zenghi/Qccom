//! v1.8 几个没有常驻界面的小工具：设备插拔记录窗口（端口右键）、两个关键字之间的耗时（日志 ⋯ 菜单）、
//! 连接 / 录制时不让电脑睡眠。

use super::logx::{tool_window, Matcher};
use super::*;
use crate::portuse::{self, PlugEvt};

#[derive(Default)]
pub(super) struct Mini {
    pub plugs: Vec<PlugEvt>,
    plugs_loaded: bool,
    /// 插拔记录窗口：Some(端口) 只看这个口，Some("") 看全部
    pub plug_win: Option<String>,
    pub awake: portuse::Awake,
    pub timing: bool,
    /// 耗时结果缓存：(日志总行数, 关键字) → 每次的 (开始时间, 毫秒)
    cache: (u64, String, Vec<(String, f64)>),
}

impl App {
    /// 端口列表变化：记进插拔记录
    pub(super) fn note_plugs(&mut self, old: &[PortEntry], new: &[PortEntry]) {
        if !self.mini.plugs_loaded {
            self.mini.plugs = portuse::load_plugs();
            self.mini.plugs_loaded = true;
        }
        let extra = self.extra_port.clone();
        for p in new.iter().filter(|p| !old.iter().any(|o| o.name == p.name) && Some(&p.name) != extra.as_ref()) {
            portuse::record(&mut self.mini.plugs, true, &p.name, &p.desc);
        }
        for p in old.iter().filter(|p| !new.iter().any(|o| o.name == p.name) && Some(&p.name) != extra.as_ref()) {
            portuse::record(&mut self.mini.plugs, false, &p.name, &p.desc);
        }
    }

    /// 每帧：有连接或正在录制就不让系统睡眠
    pub(super) fn tick_awake(&mut self) {
        let busy = (0..sess::SLOTS).any(|k| self.slot_online(k)) || self.record.is_some() || self.sessions.iter().flatten().any(|s| s.recording());
        self.mini.awake.set(busy);
    }

    pub(super) fn open_plug_log(&mut self, port: &str) {
        if !self.mini.plugs_loaded {
            self.mini.plugs = portuse::load_plugs();
            self.mini.plugs_loaded = true;
        }
        self.mini.plug_win = Some(port.to_string());
    }

    pub(super) fn mini_windows(&mut self, ctx: &egui::Context) {
        if self.mini.plug_win.is_some() {
            self.plug_window(ctx);
        }
        if self.mini.timing {
            self.timing_window(ctx);
        }
    }

    fn plug_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut filter = self.mini.plug_win.clone().unwrap_or_default();
        let mut clear = false;
        tool_window(ctx, t("设备插拔记录"), "plugs", &mut open, 520.0, |ui| {
            ui.horizontal(|ui| {
                let all = filter.is_empty();
                if ui.selectable_label(all, t("全部设备")).clicked() {
                    filter.clear();
                }
                if !all {
                    let _ = ui.selectable_label(true, &filter);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(t("清空")).clicked() {
                        clear = true;
                    }
                });
            });
            ui.add_space(4.0);
            let rows: Vec<&PlugEvt> = self.mini.plugs.iter().rev().filter(|e| filter.is_empty() || e.port == filter).collect();
            if rows.is_empty() {
                ui.label(RichText::new(t("还没有记录。插拔 USB 串口时会自动记下时间和 VID:PID。")).color(TEXT3()));
                return;
            }
            let ins = rows.iter().filter(|e| e.plugged).count();
            ui.label(RichText::new(tf("插入 {} 次 · 拔出 {} 次", &[&ins, &(rows.len() - ins)])).small().color(TEXT3()));
            ui.add_space(4.0);
            let mono = FontId::new(12.0, FontFamily::Monospace);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                egui::Grid::new("plug-grid").num_columns(4).spacing([14.0, 4.0]).striped(true).show(ui, |ui| {
                    for e in rows {
                        ui.label(RichText::new(&e.at).font(mono.clone()).color(TEXT2()));
                        let (txt, col) = if e.plugged { (t("插入"), OK()) } else { (t("拔出"), WARN()) };
                        ui.label(RichText::new(txt).color(col));
                        ui.label(RichText::new(&e.port).font(mono.clone()).color(TEXT()));
                        let name = e.desc.rsplit_once(" (").map_or(e.desc.as_str(), |(a, _)| a);
                        let vp = portuse::vid_pid(&e.desc).unwrap_or("");
                        ui.label(RichText::new(format!("{name}  {vp}")).color(TEXT3()));
                        ui.end_row();
                    }
                });
            });
        });
        if clear {
            portuse::clear_plugs(&mut self.mini.plugs);
        }
        self.mini.plug_win = open.then_some(filter);
    }

    fn timing_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        // 关键字变了或日志有新行才重新算
        let key = format!("{}\u{1}{}\u{1}{}", self.s.timing_a, self.s.timing_b, self.s.timing_re);
        let total = self.buf.total();
        if self.mini.cache.0 != total || self.mini.cache.1 != key {
            let res = timing_pairs(&self.buf, &self.s.timing_a, &self.s.timing_b, self.s.timing_re);
            self.mini.cache = (total, key, res.unwrap_or_default());
        }
        let err = (!self.s.timing_a.is_empty() && !self.s.timing_b.is_empty())
            .then(|| timing_check(&self.s.timing_a, &self.s.timing_b, self.s.timing_re))
            .flatten();
        let res = self.mini.cache.2.clone();
        tool_window(ctx, t("两个关键字之间的耗时"), "timing", &mut open, 420.0, |ui| {
            ui.label(RichText::new(t("每次出现「开始」后，到下一次出现「结束」算一次。比如上电到联网、按键到响应。")).small().color(TEXT3()));
            ui.add_space(6.0);
            egui::Grid::new("timing-in").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new(t("开始")).color(TEXT2()));
                ui.add(egui::TextEdit::singleline(&mut self.s.timing_a).hint_text(t("如：boot")).desired_width(280.0));
                ui.end_row();
                ui.label(RichText::new(t("结束")).color(TEXT2()));
                ui.add(egui::TextEdit::singleline(&mut self.s.timing_b).hint_text(t("如：WiFi connected")).desired_width(280.0));
                ui.end_row();
            });
            ui.checkbox(&mut self.s.timing_re, t("正则表达式"));
            if let Some(e) = &err {
                ui.label(RichText::new(e).small().color(ERR()));
            }
            ui.add_space(6.0);
            if res.is_empty() {
                ui.label(RichText::new(t("日志里还没有成对出现的开始和结束")).color(TEXT3()));
                return;
            }
            let v: Vec<f64> = res.iter().map(|r| r.1).collect();
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(*x), b.max(*x)));
            let avg = v.iter().sum::<f64>() / v.len() as f64;
            let mono = FontId::new(13.0, FontFamily::Monospace);
            ui.horizontal_wrapped(|ui| {
                for (k, val, col) in [
                    (t("次数"), v.len().to_string(), TEXT()),
                    (t("最小"), ms(lo), OK()),
                    (t("最大"), ms(hi), WARN()),
                    (t("平均"), ms(avg), ACCENT()),
                ] {
                    ui.label(RichText::new(k).small().color(TEXT3()));
                    ui.label(RichText::new(val).font(mono.clone()).color(col));
                    ui.add_space(10.0);
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                egui::Grid::new("timing-rows").num_columns(3).spacing([14.0, 3.0]).striped(true).show(ui, |ui| {
                    for (i, (at, d)) in res.iter().enumerate().rev() {
                        ui.label(RichText::new(format!("#{}", i + 1)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT3()));
                        ui.label(RichText::new(at).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
                        // 和平均差得多的标出来
                        let col = if v.len() > 2 && (*d - avg).abs() > avg * 0.3 { WARN() } else { TEXT() };
                        ui.label(RichText::new(ms(*d)).font(FontId::new(12.0, FontFamily::Monospace)).color(col));
                        ui.end_row();
                    }
                });
            });
            if ui.small_button(t("复制结果")).clicked() {
                let mut s = format!("#\t{}\tms\n", t("开始时间"));
                for (i, (at, d)) in res.iter().enumerate() {
                    s += &format!("{}\t{at}\t{d:.1}\n", i + 1);
                }
                s += &format!("min\t{lo:.1}\nmax\t{hi:.1}\navg\t{avg:.1}\n");
                ui.ctx().copy_text(s);
            }
        });
        self.mini.timing = open;
    }
}

fn ms(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.2} s", v / 1000.0)
    } else {
        format!("{v:.1} ms")
    }
}

fn timing_check(a: &str, b: &str, re: bool) -> Option<String> {
    Matcher::new(a, re).and(Matcher::new(b, re)).err()
}

/// 按日志时间戳算每一对「开始 → 结束」的耗时
fn timing_pairs(buf: &crate::logbuf::LogBuf, a: &str, b: &str, re: bool) -> Option<Vec<(String, f64)>> {
    if a.trim().is_empty() || b.trim().is_empty() {
        return None;
    }
    let (ma, mb) = (Matcher::new(a, re).ok()?, Matcher::new(b, re).ok()?);
    let mut out = vec![];
    let mut start: Option<chrono::DateTime<chrono::Local>> = None;
    for l in &buf.lines {
        // 同一行既是结束又是开始时先算结束
        if let Some(s) = start {
            if mb.is_match(&l.plain) {
                out.push((s.format("%H:%M:%S%.3f").to_string(), (l.ts - s).num_microseconds().unwrap_or(0) as f64 / 1000.0));
                start = None;
            }
        }
        if ma.is_match(&l.plain) {
            start = Some(l.ts);
        }
    }
    Some(out)
}

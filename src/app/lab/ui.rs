use super::*;

impl App {
    pub(in crate::app) fn instrument_pane(&mut self, ui: &mut egui::Ui) {
        let lab = &mut self.plot.lab;
        let mut send = None;
        let mut build = None;
        let mut commands = false;
        ui.horizontal_wrapped(|ui| {
            ui.strong(t("采集分析"));
            ui.separator();
            ui.selectable_value(&mut lab.view, View::Wave, t("波形"));
            ui.selectable_value(&mut lab.view, View::Fft, "FFT");
            ui.selectable_value(&mut lab.view, View::Compare, t("采集对比"));
            ui.menu_button(t("配置"), |ui| {
                if ui.button(t("采样与通道")).clicked() {
                    lab.panel = (lab.panel != Some(Panel::Channels)).then_some(Panel::Channels);
                    ui.close_menu();
                }
                if ui.button(t("在线调参")).clicked() {
                    lab.panel = (lab.panel != Some(Panel::Tuning)).then_some(Panel::Tuning);
                    ui.close_menu();
                }
                if ui.button(t("触发录制")).clicked() {
                    lab.panel = (lab.panel != Some(Panel::Trigger)).then_some(Panel::Trigger);
                    ui.close_menu();
                }
                if ui.button(t("自定义命令")).clicked() {
                    commands = true;
                    ui.close_menu();
                }
                ui.checkbox(&mut lab.visible_events, t("事件时间线"));
            });
            ui.menu_button(t("文件"), |ui| {
                ui.checkbox(&mut lab.file_range, t("按时间段载入"));
                if lab.file_range {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut lab.file_from).range(0.0..=1e9).suffix(" s"));
                        ui.label("–");
                        ui.add(egui::DragValue::new(&mut lab.file_to).range(0.0..=1e9).suffix(" s"));
                    });
                }
                if ui.button(t("打开采集")).clicked() {
                    lab.input_file(false);
                    ui.close_menu();
                }
                if ui.button(t("载入对比采集")).clicked() {
                    lab.input_file(true);
                    ui.close_menu();
                }
                if lab.stream.is_some() {
                    if ui.button(t("结束写盘")).clicked() {
                        lab.stop_stream();
                        ui.close_menu();
                    }
                } else if ui.button(t("持续写盘…")).clicked() {
                    lab.file_choice(false);
                    ui.close_menu();
                }
                if ui.button(t("关联固件文件…")).clicked() {
                    lab.file_choice(true);
                    ui.close_menu();
                }
                if ui.button(t("载入 DBC")).clicked() {
                    lab.load_dbc();
                    ui.close_menu();
                }
                for (label, ext) in [("保存会话", "qcap"), ("导出 CSV", "csv"), ("导出 MDF4", "mf4")] {
                    if ui.button(t(label)).clicked() {
                        lab.output_file(ext);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("MCU", |ui| {
                for (label, flash) in [("编译", false), ("烧录", true)] {
                    if ui.button(t(label)).clicked() {
                        build = Some(flash);
                        ui.close_menu();
                    }
                }
            });
            if ui.small_button(t("返回日志")).clicked() {
                lab.visible = false;
            }
            ui.menu_button("⋯", |ui| {
                if ui.add_enabled(lab.stream.is_none() && lab.job.is_none(), egui::Button::new(t("停止采集"))).clicked() {
                    lab.active = false;
                    lab.visible = false;
                    lab.reset_decoder();
                    lab.rec = Recording::Idle;
                    ui.close_menu();
                }
            });
        });
        let mut persist = false;
        if lab.panel == Some(Panel::Channels) {
            ui.horizontal_wrapped(|ui| {
                ui.label(t("协议"));
                let old = lab.cfg.protocol;
                for (kind, name) in [(Protocol::FireWater, "FireWater"), (Protocol::JustFloat, "JustFloat"), (Protocol::RawData, "RawData")] {
                    ui.selectable_value(&mut lab.cfg.protocol, kind, name);
                }
                if old != lab.cfg.protocol {
                    lab.decoder = Default::default();
                    persist = true;
                }
                persist |= ui.checkbox(&mut lab.cfg.can, "SLCAN").changed();
                if lab.cfg.protocol == Protocol::FireWater {
                    persist |= ui.checkbox(&mut lab.cfg.filter_samples, t("隐藏采样行")).changed();
                }
                if !lab.serial.is_empty() {
                    let Some(i) = lab.serial.iter().position(|&id| id == lab.selected) else { return };
                    let id = lab.selected;
                    lab.cfg.aliases.resize_with(lab.serial.len(), String::new);
                    ui.label(format!("CH{}", i + 1));
                    if ui.add(egui::TextEdit::singleline(&mut lab.cfg.aliases[i]).desired_width(100.)).changed() {
                        let name = lab.cfg.aliases[i].trim();
                        lab.live.channels[id].name = if name.is_empty() { format!("CH{}", i + 1) } else { name.into() };
                        if let Some(s) = &mut lab.stream {
                            s.channel(id, &lab.live.channels[id].name, "");
                        }
                        persist = true;
                    }
                }
            });
        }
        if lab.panel == Some(Panel::Tuning) {
            ui.horizontal_wrapped(|ui| {
                ui.menu_button(t("调参模板"), |ui| {
                    for (name, (lo, hi, val)) in &lab.cfg.presets {
                        if ui.button(name).clicked() {
                            lab.cfg.template = name.clone();
                            lab.cfg.low = *lo;
                            lab.cfg.high = *hi;
                            lab.cfg.value = *val;
                            persist = true;
                            ui.close_menu();
                        }
                    }
                    if lab.cfg.template.contains("{value}") && lab.cfg.low < lab.cfg.high && ui.button(t("保存当前模板")).clicked() {
                        lab.cfg.presets.insert(lab.cfg.template.clone(), (lab.cfg.low, lab.cfg.high, lab.cfg.value));
                        persist = true;
                        ui.close_menu();
                    }
                });
                persist |= ui.add(egui::TextEdit::singleline(&mut lab.cfg.template).desired_width(110.)).changed();
                persist |= ui.add(egui::DragValue::new(&mut lab.cfg.low).prefix("min ")).changed();
                persist |= ui.add(egui::DragValue::new(&mut lab.cfg.high).prefix("max ")).changed();
                if lab.cfg.low < lab.cfg.high {
                    persist |= ui.add(egui::Slider::new(&mut lab.cfg.value, lab.cfg.low..=lab.cfg.high)).changed();
                }
                if ui.button(t("发送")).clicked() {
                    send = Some(lab.cfg.value);
                }
            });
        }
        if lab.panel == Some(Panel::Trigger) {
            ui.horizontal_wrapped(|ui| {
                ui.label(t("触发通道"));
                let mut n = lab.cfg.trigger_ch + 1;
                if ui.add(egui::DragValue::new(&mut n).range(1..=CHANNELS)).changed() {
                    lab.cfg.trigger_ch = n - 1;
                    persist = true;
                }
                persist |= ui.add(egui::DragValue::new(&mut lab.cfg.threshold).prefix("> ")).changed();
                persist |= ui.add(egui::DragValue::new(&mut lab.cfg.pre).range(0.0..=300.).prefix(t("前 ")).suffix(" s")).changed();
                persist |= ui.add(egui::DragValue::new(&mut lab.cfg.post).range(0.0..=300.).prefix(t("后 ")).suffix(" s")).changed();
                if matches!(lab.rec, Recording::Idle) && ui.button(t("布防")).clicked() {
                    lab.prev = None;
                    lab.rec = Recording::Armed;
                    lab.review = false;
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            match lab.rec {
                Recording::Idle => {
                    if ui.small_button(t("录制")).clicked() {
                        let _ = lab.record("start");
                    }
                }
                _ => {
                    let label = match lab.rec {
                        Recording::Manual(_) => "停止录制",
                        Recording::Armed => "撤防",
                        _ => "采集中",
                    };
                    if ui.small_button(t(label)).clicked() {
                        let _ = lab.record("stop");
                    }
                }
            }
            if lab.saved.is_some() {
                ui.selectable_value(&mut lab.review, false, t("实时"));
                ui.selectable_value(&mut lab.review, true, t("回放"));
            }
            if lab.review {
                let end = lab.data().end();
                if ui.small_button(if lab.playing { "⏸" } else { "▶" }).clicked() {
                    if lab.cursor >= end {
                        lab.cursor = 0.;
                    }
                    lab.playing = !lab.playing;
                    lab.play_at = Instant::now();
                }
                ui.add(egui::Slider::new(&mut lab.cursor, 0.0..=end.max(0.001)).suffix(" s"));
                ui.add(egui::DragValue::new(&mut lab.speed).range(0.1..=16.).suffix("×"));
                if lab.playing {
                    let now = Instant::now();
                    lab.cursor = (lab.cursor + (now - lab.play_at).as_secs_f64() * lab.speed).min(end);
                    lab.play_at = now;
                    lab.playing = lab.cursor < end;
                }
            }
            ui.add(egui::DragValue::new(&mut lab.span).range(0.01..=3600.).suffix(" s"));
            if lab.view == View::Fft {
                persist |= ui
                    .add(egui::DragValue::new(&mut lab.cfg.hz).range(1.0..=1e8).suffix(" Hz"))
                    .on_hover_text(t("固件的采样频率"))
                    .changed();
            }
            let count = lab.data().channels.len();
            if count > 0 {
                lab.selected = lab.selected.min(count - 1);
                egui::ComboBox::from_id_salt("capture_channel")
                    .selected_text(&lab.data().channels[lab.selected].name)
                    .show_ui(ui, |ui| {
                        for i in 0..count {
                            let name = lab.data().channels[i].name.clone();
                            ui.selectable_value(&mut lab.selected, i, name);
                        }
                    });
                ui.menu_button(t("显示通道"), |ui| {
                    for i in 0..count {
                        let mut show = !lab.cfg.hidden.contains(&i);
                        if ui.checkbox(&mut show, &lab.data().channels[i].name).changed() {
                            lab.cfg.hidden.retain(|v| *v != i);
                            if !show {
                                lab.cfg.hidden.push(i)
                            }
                            persist = true;
                        }
                    }
                });
            }
        });
        if lab.view == View::Compare && lab.reference.is_none() {
            if ui.button(t("选择参考采集文件…")).clicked() {
                lab.input_file(true);
            }
        }
        let end = if lab.review { lab.cursor } else { lab.live.end() };
        let chart_h = (ui.available_height() * 0.52).clamp(80., 560.);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), chart_h), Sense::click());
        let area = rect.shrink2(vec2(10., 10.));
        let paint = ui.painter_at(rect);
        paint.rect_filled(rect, 4., BG0());
        let from = (end - lab.span).max(0.);
        let to = end.max(from + 0.000001);
        for k in 0..=5 {
            let x = area.left() + area.width() * k as f32 / 5.;
            paint.vline(x, area.y_range(), st(1., LINE().gamma_multiply(0.6)));
        }
        let selected = lab.selected;
        let c = lab.data();
        if lab.view == View::Fft {
            if let Some(ch) = c.channels.get(selected) {
                let f = capture::spectrum(&ch.points, to, lab.cfg.hz);
                let max = f.iter().map(|p| p.1).fold(1e-10, f64::max);
                draw(&paint, area, &f, 0., lab.cfg.hz / 2., 0., max, ACCENT());
                if let Some(peak) = f.iter().skip(1).max_by(|a, b| a.1.total_cmp(&b.1)) {
                    paint.text(
                        area.left_top(),
                        Align2::LEFT_TOP,
                        format!("{} · {:.2} Hz · {:.3}", ch.name, peak.0, peak.1),
                        FontId::monospace(13.),
                        TEXT(),
                    );
                }
            }
        } else {
            let shown: Vec<_> = c
                .channels
                .iter()
                .enumerate()
                .filter(|(i, _)| !lab.cfg.hidden.contains(i) && (c.channels.len() <= 8 || *i / 8 == selected / 8) && (lab.view != View::Compare || *i == selected))
                .collect();
            for (lane, (_, ch)) in shown.iter().enumerate() {
                let h = area.height() / shown.len() as f32;
                let band = Rect::from_min_size(pos2(area.left(), area.top() + h * lane as f32), vec2(area.width(), h));
                let pts = ch.envelope(from, to, band.width() as usize);
                let other = if lab.view == View::Compare {
                    lab.reference
                        .as_deref()
                        .and_then(|r| r.channels.iter().find(|s| s.name == ch.name))
                        .map(|refch| {
                            let delta = refch.points.front().map_or(0., |p| p.0) - ch.points.front().map_or(0., |p| p.0);
                            let mut data = refch.envelope(from + delta, to + delta, band.width() as usize);
                            for p in &mut data {
                                p.0 -= delta;
                            }
                            data
                        })
                        .unwrap_or_default()
                } else {
                    vec![]
                };
                let lo = pts.iter().chain(&other).map(|p| p.1).fold(f64::INFINITY, f64::min);
                let hi = pts.iter().chain(&other).map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
                if lo.is_finite() {
                    let pad = ((hi - lo) * 0.08).max(1e-6);
                    draw(&paint, band, &other, from, to, lo - pad, hi + pad, Color32::from_rgb(174, 145, 244));
                    draw(&paint, band, &pts, from, to, lo - pad, hi + pad, ACCENT());
                    paint.text(
                        band.left_top(),
                        Align2::LEFT_TOP,
                        format!("{}   {:.3} … {:.3} {}", ch.name, lo, hi, ch.unit),
                        FontId::monospace(11.),
                        TEXT2(),
                    );
                }
            }
            if shown.is_empty() {
                paint.text(area.center(), Align2::CENTER_CENTER, t("等待采样数据"), FontId::proportional(14.), TEXT3());
            }
            for e in c.events.iter().rev().filter(|e| e.t >= from && e.t <= to && matches!(e.source.as_str(), "PARAM" | "TRIG")).take(12) {
                let x = area.left() + ((e.t - from) / (to - from)) as f32 * area.width();
                paint.vline(x, area.y_range(), st(1., WARN()));
                paint.text(pos2(x, area.top()), Align2::LEFT_TOP, &e.source, FontId::monospace(11.), WARN());
            }
            if response.clicked() {
                if let Some(p) = response.interact_pointer_pos() {
                    lab.mark[lab.next_mark] = Some(from + ((p.x - area.left()) / area.width()).clamp(0., 1.) as f64 * (to - from));
                    lab.next_mark ^= 1;
                }
            }
            for (i, tm) in lab.mark.iter().enumerate() {
                if let Some(v) = tm.filter(|v| *v >= from && *v <= to) {
                    let x = area.left() + ((v - from) / (to - from)) as f32 * area.width();
                    paint.vline(x, area.y_range(), st(1., TEXT2()));
                    paint.text(pos2(x, area.bottom()), Align2::LEFT_BOTTOM, if i == 0 { "A" } else { "B" }, FontId::monospace(12.), TEXT());
                }
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.small(format!("{from:.3} … {to:.3} s"));
            if let [Some(a), Some(b)] = lab.mark {
                ui.small(format!("Δt {:.6} s", (a - b).abs()));
            }
            ui.small(format!("{} frames · {} dropped · {} invalid", lab.data().frames, lab.data().dropped, lab.decoder.rejected));
            if !lab.status.is_empty() {
                ui.label(&lab.status);
            }
        });
        if lab.visible_events {
            let events = &lab.data().events;
            let total = if lab.review { events.partition_point(|e| e.t <= end) } else { events.len() };
            egui::ScrollArea::vertical()
                .id_salt("capture_events")
                .max_height(100.)
                .stick_to_bottom(true)
                .show_rows(ui, 16., total, |ui, range| {
                    for i in range {
                        let e = &events[i];
                        ui.monospace(format!("{:9.3}  {:7}  {}", e.t, e.source, e.text));
                    }
                });
        }
        if lab.job.is_some() || lab.playing || !matches!(lab.rec, Recording::Idle) {
            ui.ctx().request_repaint_after(Duration::from_millis(40));
        }
        if persist {
            self.s.labs.insert(lab.key.clone(), lab.cfg.clone());
        }
        if let Some(value) = send {
            if let Err(e) = self.send_tune(value) {
                self.toast(e, WARN());
            }
        }
        if commands {
            self.s.cmd_panel = true;
            self.s.dock_tab = 0;
            self.debug_drawer = Some(DebugDrawer::Commands);
        }
        if let Some(flash) = build {
            if let Err(e) = self.instrument_task(flash) {
                self.toast(e, WARN());
            }
        }
    }
}

fn draw(p: &egui::Painter, rect: Rect, pts: &[Point], a: f64, b: f64, lo: f64, hi: f64, color: Color32) {
    if pts.len() < 2 || b <= a || hi <= lo {
        return;
    }
    let pts = pts
        .iter()
        .map(|(x, y)| pos2(rect.left() + ((x - a) / (b - a)) as f32 * rect.width(), rect.bottom() - ((y - lo) / (hi - lo)) as f32 * rect.height()))
        .collect();
    p.with_clip_rect(rect).add(Shape::line(pts, st(1.5, color)));
}

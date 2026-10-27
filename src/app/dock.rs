//! 右侧面板：快捷命令 / 自动应答 / 脚本 三个标签。

use super::logx::{small_chip, Matcher};
use super::*;
use crate::script::{ScriptOut, ScriptRun};

// ---------------------------------------------------------------- 自动应答

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Trigger {
    pub on: bool,
    pub pat: String,
    pub regex: bool,
    /// 回复内容；空 = 不回复
    pub reply: String,
    pub hex: bool,
    pub delay_ms: u32,
    /// 弹提示并让任务栏闪一下
    pub notify: bool,
    /// 暂停日志滚动，方便看现场
    pub pause: bool,
    /// 运行同名脚本
    pub script: String,
}

impl Default for Trigger {
    fn default() -> Self {
        Trigger {
            on: true,
            pat: String::new(),
            regex: false,
            reply: String::new(),
            hex: false,
            delay_ms: 0,
            notify: false,
            pause: false,
            script: String::new(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Script {
    pub name: String,
    pub code: String,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            name: t("新脚本").into(),
            code: crate::script::EXAMPLE.into(),
        }
    }
}

#[derive(Default)]
pub(super) struct Dock {
    /// 每条应答规则编译好的匹配器（下标对应 s.triggers；None = 关闭或无效）
    pub trig_rt: Vec<Option<Matcher>>,
    pub trig_hits: Vec<u64>,
    /// 等待延时发送的回复
    pub pending: VecDeque<(Instant, Vec<u8>)>,
    pub trig_open: Option<usize>,
    pub script: Option<ScriptRun>,
    pub cur_script: usize,
    pub renaming: Option<String>,
}

impl App {
    pub(super) fn compile_triggers(&mut self) {
        self.dock.trig_rt = self
            .s
            .triggers
            .iter()
            .map(|t| if t.on && !t.pat.is_empty() { Matcher::new(&t.pat, t.regex).ok() } else { None })
            .collect();
        self.dock.trig_hits.resize(self.s.triggers.len(), 0);
    }

    /// 新收完一行：检查应答规则、交给脚本
    pub(super) fn on_line(&mut self, text: &str) {
        if let Some(sc) = &self.dock.script {
            sc.feed_line(text);
        }
        let mut fire = vec![];
        for (i, m) in self.dock.trig_rt.iter().enumerate() {
            if m.as_ref().is_some_and(|m| m.is_match(text)) {
                fire.push(i);
            }
        }
        for i in fire {
            if let Some(h) = self.dock.trig_hits.get_mut(i) {
                *h += 1;
            }
            let tr = self.s.triggers[i].clone();
            // 回放旧日志时不真的回复设备
            if !tr.reply.is_empty() && self.replay.is_none() {
                let data = if tr.hex {
                    parse_hex(&tr.reply).unwrap_or_default()
                } else {
                    let mut d = self.s.encoding.encode(&tr.reply);
                    d.extend_from_slice(self.line_ending().bytes());
                    d
                };
                self.dock.pending.push_back((Instant::now() + Duration::from_millis(tr.delay_ms as u64), data));
            }
            if tr.notify {
                self.toast(tf("应答规则命中：{}", &[&tr.pat]), WARN());
                self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
            }
            if tr.pause && !self.paused {
                self.toggle_pause();
            }
            if !tr.script.is_empty() && self.dock.script.is_none() {
                if let Some(k) = self.s.scripts.iter().position(|s| s.name == tr.script) {
                    self.run_script(k);
                }
            }
        }
    }

    pub(super) fn tick_dock(&mut self) {
        // 延时回复
        while self.dock.pending.front().is_some_and(|(at, _)| *at <= Instant::now()) {
            let (_, d) = self.dock.pending.pop_front().unwrap();
            self.send_bytes(d);
        }
        if let Some((at, _)) = self.dock.pending.front() {
            self.ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
        // 脚本输出
        let mut done = None;
        if let Some(sc) = &self.dock.script {
            let mut outs = vec![];
            while let Ok(o) = sc.rx.try_recv() {
                outs.push(o);
            }
            let name = sc.name.clone();
            for o in outs {
                match o {
                    ScriptOut::Send(d) => self.send_bytes(d),
                    ScriptOut::Log(s) => self.buf.sys(format!("[{name}] {s}")),
                    ScriptOut::Dtr(v) => {
                        self.s.dtr = v;
                        if let Some(c) = &self.conn {
                            let _ = c.cmd.send(Cmd::SetDtr(v));
                        }
                    }
                    ScriptOut::Rts(v) => {
                        self.s.rts = v;
                        if let Some(c) = &self.conn {
                            let _ = c.cmd.send(Cmd::SetRts(v));
                        }
                    }
                    ScriptOut::Done(r) => done = Some((name.clone(), r)),
                }
            }
        }
        if let Some((name, r)) = done {
            self.dock.script = None;
            match r {
                Ok(()) => self.buf.sys(tf("脚本「{}」运行结束", &[&name])),
                Err(e) if e == "stopped" => self.buf.sys(tf("脚本「{}」已停止", &[&name])),
                Err(e) => {
                    self.buf.sys(tf("✗ 脚本「{}」出错：{}", &[&name, &e]));
                    self.toast(tf("脚本出错：{}", &[&e]), ERR());
                }
            }
        }
    }

    pub(super) fn run_script(&mut self, k: usize) {
        if let Some(sc) = self.dock.script.take() {
            sc.stop();
        }
        let Some(s) = self.s.scripts.get(k).cloned() else { return };
        if self.conn.is_none() {
            self.toast(t("未连接：脚本发送的数据不会发出去"), WARN());
        }
        self.buf.sys(tf("运行脚本「{}」", &[&s.name]));
        self.dock.script = Some(ScriptRun::start(&s.name, s.code.clone(), self.line_ending().bytes().to_vec(), self.s.encoding, self.ctx.clone()));
    }

    // ------------------------------------------------------------ 面板

    pub(super) fn dock_ui(&mut self, ui: &mut egui::Ui) -> Option<CmdAction> {
        let mut tab = self.s.dock_tab;
        let running = self.dock.script.is_some();
        let n_trig = self.s.triggers.iter().filter(|t| t.on).count();
        let l_trig = if n_trig > 0 { format!("{} {}", t("应答"), n_trig) } else { t("应答").to_string() };
        let l_script = if running { format!("{} ●", t("脚本")) } else { t("脚本").to_string() };
        tab_strip(ui, &mut tab, &[(0u8, t("命令")), (1, l_trig.as_str()), (2, l_script.as_str())], 28.0);
        self.s.dock_tab = tab;
        ui.add_space(10.0);
        match tab {
            1 => {
                self.triggers_ui(ui);
                None
            }
            2 => {
                self.scripts_ui(ui);
                None
            }
            _ => {
                let connected = self.conn.is_some();
                let seq = self.seq.as_ref().map(|(g, q, _)| (*g, q.len()));
                self.cmd_ui.ui(ui, &mut self.s.groups, &mut self.s.cur_group, connected, seq)
            }
        }
    }

    fn triggers_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("收到匹配的行时自动回复、提醒或运行脚本。")).small().color(TEXT3()));
        ui.add_space(6.0);
        let mut changed = false;
        let mut del = None;
        let scripts: Vec<String> = self.s.scripts.iter().map(|s| s.name.clone()).collect();
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for i in 0..self.s.triggers.len() {
                let open = self.dock.trig_open == Some(i);
                let hits = self.dock.trig_hits.get(i).copied().unwrap_or(0);
                let tr = &mut self.s.triggers[i];
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, if open { ACCENT().gamma_multiply(0.4) } else { LINE() }))
                    .corner_radius(R_CARD)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            changed |= switch(ui, &mut tr.on).changed();
                            let title = if tr.pat.is_empty() { t("（未设置）").to_string() } else { tr.pat.clone() };
                            let r = ui.add(
                                egui::Label::new(RichText::new(title).font(FontId::new(12.5, FontFamily::Monospace)).color(if tr.on { TEXT() } else { TEXT3() }))
                                    .truncate()
                                    .sense(Sense::click()),
                            );
                            if r.clicked() {
                                self.dock.trig_open = if open { None } else { Some(i) };
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if hits > 0 {
                                    ui.label(RichText::new(compact(hits)).font(FontId::new(11.0, FontFamily::Monospace)).color(ACCENT()))
                                        .on_hover_text(t("命中次数"));
                                }
                            });
                        });
                        // 摘要
                        if !open {
                            let mut what = vec![];
                            if !tr.reply.is_empty() {
                                what.push(format!("→ {}", tr.reply));
                            }
                            if tr.notify {
                                what.push(t("提醒").into());
                            }
                            if tr.pause {
                                what.push(t("暂停").into());
                            }
                            if !tr.script.is_empty() {
                                what.push(format!("▶ {}", tr.script));
                            }
                            if !what.is_empty() {
                                ui.add(egui::Label::new(RichText::new(what.join("  ·  ")).small().color(TEXT3())).truncate());
                            }
                            return;
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(t("收到")).small().color(TEXT3()));
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut tr.pat).font(FontId::new(12.5, FontFamily::Monospace)).desired_width(ui.available_width() - 42.0).hint_text(t("文字或正则")))
                                .changed();
                            changed |= small_chip(ui, &mut tr.regex, ".*", t("正则表达式"));
                        });
                        ui.label(RichText::new(t("回复")).small().color(TEXT3()));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut tr.reply)
                                    .font(FontId::new(12.5, FontFamily::Monospace))
                                    .desired_width(ui.available_width() - 60.0)
                                    .hint_text(if tr.hex { "AA 55 01" } else { t("留空 = 不回复") }),
                            );
                            small_chip(ui, &mut tr.hex, "HEX", t("按十六进制字节发送"));
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("延时")).small().color(TEXT3()));
                            ui.add(egui::DragValue::new(&mut tr.delay_ms).range(0..=60_000).suffix(" ms").speed(10));
                        });
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut tr.notify, t("提醒"));
                            ui.checkbox(&mut tr.pause, t("暂停日志"));
                        });
                        if !scripts.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(t("运行脚本")).small().color(TEXT3()));
                                egui::ComboBox::from_id_salt(("trs", i))
                                    .width(130.0)
                                    .selected_text(if tr.script.is_empty() { t("无") } else { tr.script.as_str() })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut tr.script, String::new(), t("无"));
                                        for s in &scripts {
                                            ui.selectable_value(&mut tr.script, s.clone(), s);
                                        }
                                    });
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui.button(t("完成")).clicked() {
                                self.dock.trig_open = None;
                            }
                            if ui.button(RichText::new(t("删除")).color(ERR())).clicked() {
                                del = Some(i);
                            }
                        });
                    });
                ui.add_space(6.0);
            }
        });
        if let Some(i) = del {
            self.s.triggers.remove(i);
            if i < self.dock.trig_hits.len() {
                self.dock.trig_hits.remove(i);
            }
            self.dock.trig_open = None;
            changed = true;
        }
        if dashed_button(ui, t("＋ 添加应答规则")).clicked() {
            self.s.triggers.push(Trigger::default());
            self.dock.trig_open = Some(self.s.triggers.len() - 1);
            changed = true;
        }
        if changed {
            self.compile_triggers();
        }
    }

    fn scripts_ui(&mut self, ui: &mut egui::Ui) {
        if self.s.scripts.is_empty() {
            self.s.scripts.push(Script::default());
        }
        let k = self.dock.cur_script.min(self.s.scripts.len() - 1);
        self.dock.cur_script = k;
        // 脚本选择 + 运行
        ui.horizontal(|ui| {
            if let Some(name) = &mut self.dock.renaming {
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(140.0));
                r.request_focus();
                if r.lost_focus() {
                    if !name.trim().is_empty() {
                        self.s.scripts[k].name = name.trim().to_string();
                    }
                    self.dock.renaming = None;
                }
            } else {
                let r = egui::ComboBox::from_id_salt("scripts").width(140.0).selected_text(self.s.scripts[k].name.clone()).show_ui(ui, |ui| {
                    for (i, s) in self.s.scripts.iter().enumerate() {
                        if ui.selectable_label(i == k, &s.name).clicked() {
                            self.dock.cur_script = i;
                        }
                    }
                    ui.separator();
                    if ui.selectable_label(false, t("＋ 新建脚本")).clicked() {
                        self.s.scripts.push(Script::default());
                        self.dock.cur_script = self.s.scripts.len() - 1;
                    }
                });
                r.response.on_hover_text(t("右键：重命名 / 删除"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let running = self.dock.script.is_some();
                let b = egui::Button::new(RichText::new(if running { t("■ 停止") } else { t("▶ 运行") }).color(if running { ERR() } else { ON_ACCENT() }))
                    .fill(if running { ERR().gamma_multiply(0.15) } else { ACCENT() })
                    .min_size(vec2(64.0, 26.0));
                if ui.add(b).on_hover_text("Ctrl+Enter").clicked() {
                    if let Some(sc) = &self.dock.script {
                        sc.stop();
                    } else {
                        self.run_script(k);
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.small_button(t("重命名")).clicked() {
                self.dock.renaming = Some(self.s.scripts[k].name.clone());
            }
            if self.s.scripts.len() > 1 && ui.small_button(RichText::new(t("删除")).color(ERR())).clicked() {
                self.s.scripts.remove(k);
                self.dock.cur_script = 0;
            }
            if let Some(sc) = &self.dock.script {
                ui.label(RichText::new(tf("运行中 {}s", &[&sc.started.elapsed().as_secs()])).small().color(ACCENT()));
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        });
        let k = self.dock.cur_script.min(self.s.scripts.len() - 1);
        ui.add_space(4.0);
        let h = (ui.available_height() - 110.0).max(120.0);
        let code = &mut self.s.scripts[k].code;
        let mut run = false;
        egui::Frame::new().fill(BG0()).stroke(st(1.0, LINE())).corner_radius(R_WIDGET).show(ui, |ui| {
            egui::ScrollArea::both().max_height(h).min_scrolled_height(h).auto_shrink([false, false]).show(ui, |ui| {
                // 代码不自动折行，长行横向滚动
                let mut layouter = |ui: &egui::Ui, text: &str, _wrap: f32| {
                    let job = LayoutJob::simple(text.to_owned(), FontId::new(12.5, FontFamily::Monospace), TEXT(), f32::INFINITY);
                    ui.fonts(|f| f.layout_job(job))
                };
                let r = ui.add(
                    egui::TextEdit::multiline(code)
                        .font(FontId::new(12.5, FontFamily::Monospace))
                        .layouter(&mut layouter)
                        .code_editor()
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .desired_rows(((h / 17.0) as usize).max(6))
                        .margin(vec2(8.0, 6.0)),
                );
                if r.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
                    run = true;
                }
            });
        });
        if run && self.dock.script.is_none() {
            self.run_script(k);
        }
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new(t("可用函数")).small().color(TEXT2())).id_salt("script-help").show(ui, |ui| {
            for (f, d) in [
                ("sendln(\"AT\")", t("发送并加换行")),
                ("send(\"x\")  send_hex(\"AA 55\")", t("发送文字 / 字节")),
                ("wait(\"OK\", 1000)", t("等待包含文字的一行，返回 true / false")),
                ("wait_re(\"v(\\\\d+)\", 1000)", t("等待匹配正则的一行，返回括号里的内容")),
                ("read_line(1000)", t("读下一行，超时返回空")),
                ("sleep(100)  now()", t("等待 / 当前毫秒")),
                ("log(\"msg\")", t("在日志里显示")),
                ("dtr(true)  rts(false)", t("设置信号线")),
            ] {
                ui.label(RichText::new(f).font(FontId::new(11.5, FontFamily::Monospace)).color(ACCENT()));
                ui.label(RichText::new(d).small().color(TEXT3()));
            }
        });
    }
}

/// 虚线框的“添加”按钮
pub(super) fn dashed_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    let p = ui.painter();
    let c = if resp.hovered() { ACCENT() } else { LINE_HI() };
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    for w in pts.windows(2) {
        p.add(Shape::dashed_line(&[w[0], w[1]], st(1.0, c), 4.0, 3.0));
    }
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(12.5), if resp.hovered() { ACCENT() } else { TEXT2() });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

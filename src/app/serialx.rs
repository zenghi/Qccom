//! 串口增强：信号线（DTR/RTS 开关、CTS/DSR/RI/CD 指示、复位脉冲、Break）、端口别名、
//! 双口监听（B 口）、串口转 TCP、配置方案。

use super::*;
use crate::bridge::{Bridge, BridgeEvt};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ResetKind {
    /// DTR 拉低 100 ms 再恢复
    Dtr,
    /// RTS 拉低 100 ms 再恢复
    Rts,
    /// ESP32 / ESP8266：复位运行
    EspRun,
    /// ESP32 / ESP8266：复位进下载模式
    EspBoot,
}

impl ResetKind {
    pub fn name(self) -> &'static str {
        match self {
            ResetKind::Dtr => t("DTR 脉冲复位"),
            ResetKind::Rts => t("RTS 脉冲复位"),
            ResetKind::EspRun => t("ESP32 复位运行"),
            ResetKind::EspBoot => t("ESP32 进下载模式"),
        }
    }
}

/// 双口监听：B 口没收完的一行
#[derive(Default)]
pub(super) struct Sniff {
    pub conn: Option<Connection>,
    pub gen: u64,
    pub pending: Vec<u8>,
    pub last: Option<Instant>,
}

/// 配置方案：连接参数、显示方式、快捷命令、规则、脚本等一起保存 / 切换
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Profile {
    pub transport: Option<Transport>,
    pub port: String,
    pub cfg: Option<PortConfig>,
    pub net: Option<NetConfig>,
    pub encoding: Option<Encoding>,
    pub view: Option<ViewMode>,
    pub line_ending: Option<LineEnding>,
    pub hex_send: bool,
    pub dtr: bool,
    pub rts: bool,
    pub groups: Vec<CmdGroup>,
    pub rules: Vec<super::logx::Rule>,
    pub frame: Option<super::logx::FrameCfg>,
    pub triggers: Vec<super::dock::Trigger>,
    pub scripts: Vec<super::dock::Script>,
    pub keywords: String,
}

fn profile_dir() -> PathBuf {
    crate::data_dir().join("profiles")
}

pub(super) fn list_profiles() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(profile_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.path().file_stem().filter(|_| e.path().extension().is_some_and(|x| x == "json")).map(|s| s.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn safe_name(s: &str) -> String {
    s.trim().chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect()
}

#[derive(Default)]
pub(super) struct SerialUi {
    /// 正在给哪个端口起别名
    pub alias_edit: Option<(String, String)>,
    pub profile_new: Option<String>,
    pub bridge_err: Option<String>,
}

impl App {
    pub(super) fn port_alias(&self, name: &str) -> Option<&String> {
        self.s.aliases.get(name).filter(|a| !a.is_empty())
    }

    // ------------------------------------------------------------ 信号线

    pub(super) fn reset_device(&mut self, k: ResetKind) {
        let Some(c) = &self.conn else {
            self.toast(t("未连接"), WARN());
            return;
        };
        let (dtr, rts) = (self.s.dtr, self.s.rts);
        let steps = match k {
            ResetKind::Dtr => vec![(Some(!dtr), None, 100), (Some(dtr), None, 0)],
            ResetKind::Rts => vec![(None, Some(!rts), 100), (None, Some(rts), 0)],
            // esptool 的时序：EN = RTS 取反，IO0 = DTR 取反
            ResetKind::EspRun => vec![(Some(false), Some(true), 100), (Some(false), Some(false), 0)],
            ResetKind::EspBoot => vec![(Some(false), Some(true), 100), (Some(true), Some(false), 50), (Some(false), Some(false), 0)],
        };
        let _ = c.cmd.send(Cmd::Pulse(steps));
        if matches!(k, ResetKind::EspRun | ResetKind::EspBoot) {
            self.s.dtr = false;
            self.s.rts = false;
        }
        self.s.reset = k;
        self.buf.sys(tf("已发送：{}", &[&k.name()]));
    }

    pub(super) fn send_break(&mut self) {
        if let Some(c) = &self.conn {
            let _ = c.cmd.send(Cmd::Break(250));
            self.buf.sys(t("已发送 Break（250 ms）"));
        }
    }

    /// 链路卡片下面的信号线一行（只在串口连接时显示）
    pub(super) fn signals_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (name, on, tip) in [("DTR", self.s.dtr, t("点击切换 DTR")), ("RTS", self.s.rts, t("点击切换 RTS"))] {
                let r = ui
                    .add(
                        egui::Button::new(RichText::new(name).font(FontId::new(10.5, mono_bold())).color(if on { ACCENT() } else { TEXT3() }))
                            .fill(if on { ACCENT().gamma_multiply(0.12) } else { Color32::TRANSPARENT })
                            .stroke(st(1.0, if on { ACCENT().gamma_multiply(0.4) } else { LINE() }))
                            .min_size(vec2(36.0, 20.0)),
                    )
                    .on_hover_text(tip);
                if r.clicked() {
                    let v = !on;
                    if name == "DTR" {
                        self.s.dtr = v;
                    } else {
                        self.s.rts = v;
                    }
                    if let Some(c) = &self.conn {
                        let _ = c.cmd.send(if name == "DTR" { Cmd::SetDtr(v) } else { Cmd::SetRts(v) });
                    }
                }
            }
            ui.add_space(4.0);
            // 输入线：亮 = 有效
            for (i, name) in ["CTS", "DSR", "RI", "CD"].iter().enumerate() {
                let on = self.signals.is_some_and(|s| s[i]);
                let (r, _) = ui.allocate_exact_size(vec2(6.0, 20.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 2.5, if on { OK() } else { LINE_HI() });
                ui.label(RichText::new(*name).font(FontId::new(10.0, mono_bold())).color(if on { OK() } else { TEXT3() }));
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let k = self.s.reset;
            let r = ui.add(egui::Button::new(RichText::new(t("复位")).size(12.0)).min_size(vec2(0.0, 24.0))).on_hover_text(k.name());
            if r.clicked() {
                self.reset_device(k);
            }
            r.context_menu(|ui| {
                for kk in [ResetKind::Dtr, ResetKind::Rts, ResetKind::EspRun, ResetKind::EspBoot] {
                    if menu_item(ui, kk.name(), if kk == k { "✓" } else { "" }) {
                        self.reset_device(kk);
                        ui.close_menu();
                    }
                }
            });
            egui::ComboBox::from_id_salt("resetk").width(16.0).selected_text("").show_ui(ui, |ui| {
                for kk in [ResetKind::Dtr, ResetKind::Rts, ResetKind::EspRun, ResetKind::EspBoot] {
                    if ui.selectable_label(kk == k, kk.name()).clicked() {
                        self.reset_device(kk);
                    }
                }
            });
            if ui.add(egui::Button::new(RichText::new("Break").size(12.0)).min_size(vec2(0.0, 24.0))).on_hover_text(t("发送 Break 信号（250 ms）")).clicked() {
                self.send_break();
            }
        });
    }

    // ------------------------------------------------------------ 双口监听

    pub(super) fn open_sniff(&mut self) {
        self.close_sniff();
        let name = self.s.sniff_port.clone();
        if name.is_empty() || name == self.s.port {
            return;
        }
        self.sniff.gen += 1;
        match serial::open_side(self.sniff.gen | 1 << 62, &name, self.s.cfg, false, false, true, self.evt_tx.clone(), self.ctx.clone()) {
            Ok(c) => {
                self.sniff.conn = Some(c);
                self.buf.sys(tf("监听口 B：{}（显示为 B ›）", &[&name]));
            }
            Err(e) => {
                self.buf.sys(tf("✗ 打开监听口 B {} 失败：{}", &[&name, &e]));
                self.toast(tf("监听口 B 打开失败：{}", &[&e]), ERR());
            }
        }
    }

    pub(super) fn close_sniff(&mut self) {
        if let Some(c) = self.sniff.conn.take() {
            let _ = c.cmd.send(Cmd::Close);
        }
        self.flush_sniff();
    }

    pub(super) fn is_sniff_gen(&self, gen: u64) -> bool {
        gen == self.sniff.gen | 1 << 62
    }

    pub(super) fn on_sniff_data(&mut self, d: Vec<u8>) {
        self.rx_bytes += d.len() as u64;
        self.last_rx_at = Some(Instant::now());
        // 两路数据交错到达：B 口按自己的行切分，收完一行再插进日志
        for &b in &d {
            self.sniff.pending.push(b);
            if b == b'\n' || self.sniff.pending.len() >= 1024 {
                self.flush_sniff();
            }
        }
        self.sniff.last = Some(Instant::now());
    }

    pub(super) fn flush_sniff(&mut self) {
        if self.sniff.pending.is_empty() {
            return;
        }
        let d = std::mem::take(&mut self.sniff.pending);
        self.buf.push_line(Dir::Rx2, d, self.s.encoding);
    }

    pub(super) fn tick_sniff(&mut self) {
        // B 口半行超过 100 ms 没下文：先显示出来
        if self.sniff.last.is_some_and(|t| t.elapsed() > Duration::from_millis(100)) && !self.sniff.pending.is_empty() {
            self.flush_sniff();
        }
        if !self.sniff.pending.is_empty() {
            self.ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    // ------------------------------------------------------------ 串口转 TCP

    pub(super) fn toggle_bridge(&mut self) {
        if let Some(b) = self.bridge.take() {
            self.buf.sys(tf("已停止共享（端口 {}）", &[&b.port]));
            return;
        }
        match Bridge::start(self.s.bridge_port, self.ctx.clone()) {
            Ok(b) => {
                self.buf.sys(tf("串口已共享到网络：{}:{}", &[&local_ip(), &b.port]));
                self.serial_ui.bridge_err = None;
                self.bridge = Some(b);
            }
            Err(e) => {
                self.serial_ui.bridge_err = Some(e.clone());
                self.toast(tf("无法监听端口 {}：{}", &[&self.s.bridge_port, &e]), ERR());
            }
        }
    }

    pub(super) fn poll_bridge(&mut self) {
        let Some(b) = &mut self.bridge else { return };
        let mut evts = vec![];
        while let Ok(e) = b.rx.try_recv() {
            evts.push(e);
        }
        for e in evts {
            match e {
                BridgeEvt::Data(d) => {
                    if self.conn.is_some() {
                        self.send_bytes(d);
                    }
                }
                BridgeEvt::Clients(n) => {
                    if let Some(b) = &mut self.bridge {
                        let old = b.clients;
                        b.clients = n;
                        if n > old {
                            self.buf.sys(tf("网络客户端已连接（共 {} 个）", &[&n]));
                        } else {
                            self.buf.sys(tf("网络客户端已断开（剩 {} 个）", &[&n]));
                        }
                    }
                }
                BridgeEvt::Error(e) => self.buf.sys(tf("✗ 共享出错：{}", &[&e])),
            }
        }
    }

    pub(super) fn bridge_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut on = self.bridge.is_some();
            if toggle_chip(ui, &mut on, t("共享到网络"), t("开一个 TCP 服务器：连上来的客户端和串口双向透传")) {
                self.toggle_bridge();
            }
            if self.bridge.is_none() {
                ui.add(egui::DragValue::new(&mut self.s.bridge_port).range(1..=65535).prefix(":"));
            } else {
                ui.label(RichText::new(format!(":{}", self.s.bridge_port)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
            }
        });
        if let Some(b) = &self.bridge {
            ui.label(RichText::new(tf("{} 个客户端", &[&b.clients])).small().color(if b.clients > 0 { OK() } else { TEXT3() }));
        }
    }

    // ------------------------------------------------------------ 端口卡片右键菜单

    pub(super) fn port_menu(&mut self, ui: &mut egui::Ui, p: &PortEntry) {
        if menu_item(ui, t("重命名…"), "") {
            self.serial_ui.alias_edit = Some((p.name.clone(), self.port_alias(&p.name).cloned().unwrap_or_default()));
            ui.close_menu();
        }
        if self.port_alias(&p.name).is_some() && menu_item(ui, t("清除别名"), "") {
            self.s.aliases.remove(&p.name);
            ui.close_menu();
        }
        ui.separator();
        let is_b = self.s.sniff_port == p.name;
        if p.name != self.s.port {
            if menu_item(ui, if is_b { t("取消监听口 B") } else { t("设为监听口 B") }, "") {
                self.s.sniff_port = if is_b { String::new() } else { p.name.clone() };
                if self.conn.is_some() && self.s.transport == Transport::Serial {
                    if is_b {
                        self.close_sniff();
                    } else {
                        self.open_sniff();
                    }
                }
                ui.close_menu();
            }
            ui.label(RichText::new(t("同时打开两个串口，比如监听一条链路的收发两根线")).small().color(TEXT3()));
        }
        if menu_item(ui, t("复制端口名"), "") {
            ui.ctx().copy_text(p.name.clone());
            ui.close_menu();
        }
    }

    pub(super) fn alias_window(&mut self, ctx: &egui::Context) {
        let Some((port, mut name)) = self.serial_ui.alias_edit.clone() else { return };
        let mut open = true;
        let mut win_open = true;
        let mut done = false;
        super::logx::tool_window(ctx, t("端口别名"), "alias", &mut win_open, 280.0, |ui| {
            ui.label(RichText::new(&port).small().color(TEXT3()));
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(t("如：主控板、GPS 模块")).desired_width(f32::INFINITY));
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                done = true;
            }
            ui.horizontal(|ui| {
                if ui.button(t("确定")).clicked() {
                    done = true;
                }
                if ui.button(t("取消")).clicked() {
                    open = false;
                }
            });
        });
        open &= win_open;
        if done {
            if name.trim().is_empty() {
                self.s.aliases.remove(&port);
            } else {
                self.s.aliases.insert(port.clone(), name.trim().to_string());
            }
            open = false;
        }
        self.serial_ui.alias_edit = if open { Some((port, name)) } else { None };
    }

    // ------------------------------------------------------------ 配置方案

    fn current_profile(&self) -> Profile {
        let s = &self.s;
        Profile {
            transport: Some(s.transport),
            port: s.port.clone(),
            cfg: Some(s.cfg),
            net: Some(s.net.clone()),
            encoding: Some(s.encoding),
            view: Some(s.view),
            line_ending: Some(s.line_ending),
            hex_send: s.hex_send,
            dtr: s.dtr,
            rts: s.rts,
            groups: s.groups.clone(),
            rules: s.rules.clone(),
            frame: Some(s.frame.clone()),
            triggers: s.triggers.clone(),
            scripts: s.scripts.clone(),
            keywords: s.keywords.clone(),
        }
    }

    pub(super) fn save_profile(&mut self, name: &str) {
        let name = safe_name(name);
        if name.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(profile_dir());
        let p = profile_dir().join(format!("{name}.json"));
        match serde_json::to_string_pretty(&self.current_profile()).map_err(|e| e.to_string()).and_then(|j| std::fs::write(&p, j).map_err(|e| e.to_string())) {
            Ok(()) => {
                self.s.profile = name.clone();
                self.toast(tf("已保存配置方案「{}」", &[&name]), OK());
            }
            Err(e) => self.toast(tf("保存失败：{}", &[&e]), ERR()),
        }
    }

    pub(super) fn load_profile(&mut self, name: &str) {
        let p = profile_dir().join(format!("{name}.json"));
        let pr: Profile = match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
            Ok(p) => p,
            Err(e) => {
                self.toast(tf("无法读取配置方案：{}", &[&e]), ERR());
                return;
            }
        };
        if self.conn.is_some() {
            self.disconnect();
        }
        let s = &mut self.s;
        if let Some(v) = pr.transport {
            s.transport = v;
        }
        if !pr.port.is_empty() {
            s.port = pr.port;
        }
        if let Some(v) = pr.cfg {
            s.cfg = v;
        }
        if let Some(v) = pr.net {
            s.net = v;
        }
        if let Some(v) = pr.encoding {
            s.encoding = v;
        }
        if let Some(v) = pr.view {
            s.view = v;
        }
        if let Some(v) = pr.line_ending {
            s.line_ending = v;
        }
        s.hex_send = pr.hex_send;
        s.dtr = pr.dtr;
        s.rts = pr.rts;
        if !pr.groups.is_empty() {
            s.groups = pr.groups;
            s.cur_group = 0;
        }
        s.rules = pr.rules;
        if let Some(v) = pr.frame {
            s.frame = v;
        }
        s.triggers = pr.triggers;
        if !pr.scripts.is_empty() {
            s.scripts = pr.scripts;
        }
        s.keywords = pr.keywords;
        s.profile = name.to_string();
        self.baud_text = self.s.cfg.baud.to_string();
        self.parse_keywords();
        self.compile_rules();
        self.compile_triggers();
        self.apply_frame();
        self.buf.rerender(self.s.encoding);
        self.toast(tf("已切换到配置方案「{}」", &[&name]), OK());
    }

    /// 品牌栏右边的配置方案按钮
    pub(super) fn profile_button(&mut self, ui: &mut egui::Ui) {
        let label = if self.s.profile.is_empty() { t("默认").to_string() } else { self.s.profile.clone() };
        let r = ui
            .add(egui::Button::new(RichText::new(format!("{label} ▾")).size(11.5).color(TEXT2())).fill(Color32::TRANSPARENT).stroke(st(1.0, LINE())).min_size(vec2(0.0, 22.0)))
            .on_hover_text(t("配置方案：连接参数、快捷命令、规则、脚本一起保存和切换"));
        let id = egui::Id::new("profiles");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(220.0);
            let list = list_profiles();
            if list.is_empty() {
                ui.label(RichText::new(t("还没有保存的配置方案")).small().color(TEXT3()));
            }
            for n in &list {
                if menu_item(ui, n, if *n == self.s.profile { "✓" } else { "" }) {
                    self.load_profile(n);
                    ui.memory_mut(|m| m.close_popup());
                }
            }
            ui.separator();
            match &mut self.serial_ui.profile_new {
                Some(name) => {
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(t("方案名称")).desired_width(f32::INFINITY));
                    r.request_focus();
                    if r.lost_focus() {
                        let n = name.clone();
                        self.serial_ui.profile_new = None;
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            self.save_profile(&n);
                        }
                    }
                }
                None => {
                    if menu_item(ui, t("保存当前为新方案…"), "") {
                        self.serial_ui.profile_new = Some(String::new());
                    }
                    if !self.s.profile.is_empty() && menu_item(ui, &tf("覆盖「{}」", &[&self.s.profile]), "") {
                        let n = self.s.profile.clone();
                        self.save_profile(&n);
                        ui.memory_mut(|m| m.close_popup());
                    }
                    if !self.s.profile.is_empty() && menu_item(ui, &tf("删除「{}」", &[&self.s.profile]), "") {
                        let _ = std::fs::remove_file(profile_dir().join(format!("{}.json", self.s.profile)));
                        self.s.profile.clear();
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            }
        });
    }
}

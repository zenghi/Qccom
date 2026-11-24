//! 串口增强：信号线（DTR/RTS 开关、CTS/DSR/RI/CD 指示、复位脉冲、Break）、端口别名、
//! 双口监听（B 口）、串口转 TCP、配置方案。

use super::*;

/// 让给终端命令的串口
pub(crate) struct Lent {
    port: String,
    shell: usize,
    prompt: String,
    row: u64,
    since: Instant,
    monitor: bool,
    reopen: bool,
}
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
    pub log_preset: Option<LogPreset>,
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keyword_colors: BTreeMap<String, logx::ManualColor>,
}

fn profile_dir() -> PathBuf {
    crate::data_dir().join("profiles")
}

pub(super) fn list_profiles() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(profile_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    e.path()
                        .file_stem()
                        .filter(|_| e.path().extension().is_some_and(|x| x == "json"))
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn safe_name(s: &str) -> String {
    s.trim().chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' }).collect()
}

struct ProfileDraft {
    name: String,
    focus_name: bool,
    error: Option<String>,
}

#[derive(Default)]
pub(super) struct SerialUi {
    /// 正在给哪个端口起别名
    pub alias_edit: Option<(String, String)>,
    profile_new: Option<ProfileDraft>,
    pub bridge_err: Option<String>,
    /// Windows：打开虚拟串口时发现没装 com0com，显示安装提示
    pub vc_need_install: bool,
    pub vc_installing: Option<(String, u64, std::sync::mpsc::Receiver<Result<(), String>>)>,
    pub vc_err: Option<String>,
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
            if ui
                .add(egui::Button::new(RichText::new("Break").size(12.0)).min_size(vec2(0.0, 24.0)))
                .on_hover_text(t("发送 Break 信号（250 ms）"))
                .clicked()
            {
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
        if self.serial_port_busy(&name) {
            self.toast(t("该串口正在使用"), WARN());
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
        if self.conn.is_none() {
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

    // ------------------------------------------------------------ 终端命令要用串口：先让出来

    /// 终端里执行的命令用到了 QCCOM 正开着的串口（烧录、stty、别的工具）：先关掉，命令跑完再打开
    pub(super) fn show_mcu_serial(&mut self, slot: usize) {
        self.shells.show_below_debug();
        if self.focus_docked_serial(slot) {
            return;
        }
        self.selected_serial = slot;
        self.reveal(Pane::Debug, "mcu.serial", work::Strength::Strong);
    }

    pub(super) fn monitor_mcu_serial(&mut self, slot: usize) {
        self.show_mcu_serial(slot);
        if self.serial_port_for(slot).is_empty() {
            self.toast(t("请先在「设备」中选择串口"), WARN());
            return;
        }
        let port = self.serial_port_for(slot).to_owned();
        if self.is_mapping_monitor(&port) {
            return;
        }
        self.with_session(slot, |app| {
            if app.lent.is_none() && app.conn.is_none() && app.reconnect.is_none() && app.autobaud.is_none() {
                app.connect();
            }
        });
    }

    pub(super) fn lend_port(&mut self, shell: usize, line: &str, row: u64, monitor: Option<usize>) -> Option<Vec<std::sync::mpsc::Receiver<()>>> {
        let mut releases = Vec::new();
        let prompt = crate::shell::prompt_of(line);
        let cmd = line.strip_prefix(prompt.as_str()).unwrap_or(line);
        let matching: Vec<_> = self
            .serial_tabs
            .iter()
            .copied()
            .filter(|&k| {
                let port = self.serial_port_for(k);
                !port.is_empty() && (monitor == Some(k) || crate::shell::uses_port(cmd, port))
            })
            .collect();
        if matching.iter().copied().any(|slot| self.with_session(slot, |app| app.lent.is_some() || app.autobaud.is_some())) {
            self.toast(t("设备串口正在被其他任务使用"), WARN());
            return None;
        }
        for k in matching {
            self.with_session(k, |app| {
                let follow = monitor == Some(k);
                if app.s.transport != Transport::Serial {
                    return;
                }
                let port = app.s.port.clone();
                let reopen = follow || app.conn.is_some() || app.reconnect.is_some();
                if app.conn.is_some() {
                    let (done, released) = std::sync::mpsc::channel();
                    app.disconnect_with(Cmd::Release(done));
                    releases.push(released);
                    app.buf.sys(if prompt.is_empty() {
                        tf("终端命令要用 {}，已先关闭串口", &[&port])
                    } else {
                        tf("终端命令要用 {}，已先关闭串口，命令结束后自动重新打开", &[&port])
                    });
                }
                app.reconnect = None;
                if !prompt.is_empty() {
                    app.lent = Some(Lent {
                        port,
                        shell,
                        prompt: prompt.clone(),
                        row,
                        since: Instant::now(),
                        monitor: follow,
                        reopen,
                    });
                }
            });
        }
        Some(releases)
    }

    /// 让出的串口：命令回到提示符后重新打开（设备烧录后重启要重新枚举，交给自动重连等它出现）
    pub(super) fn tick_lent(&mut self) {
        let Some(l) = &self.lent else { return };
        // 用户自己又打开了，或者等太久了：不管了
        if self.conn.is_some() || self.s.port != l.port || l.since.elapsed() > Duration::from_secs(1800) {
            self.lent = None;
            return;
        }
        match self.shells.back_at_prompt(l.shell, &l.prompt, l.row, !l.monitor) {
            Some(false) => self.ctx.request_repaint_after(Duration::from_millis(if l.monitor { 100 } else { 500 })),
            done => {
                let l = self.lent.take().unwrap();
                if l.reopen && (done.is_some() || l.monitor) {
                    self.buf.sys(tf("命令已结束，重新打开 {}", &[&l.port]));
                    if l.monitor && self.ports.iter().any(|p| p.name == l.port) {
                        self.connect_inner(true);
                    }
                    if self.conn.is_none() {
                        self.reconnect = Some((l.port, Instant::now()));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ 串口共享给其他软件（虚拟串口）

    pub(super) fn toggle_vcom(&mut self) {
        if self.vcom.take().is_some() {
            self.buf.sys(t("虚拟串口已关闭"));
            return;
        }
        if self.conn.is_none() {
            return;
        }
        self.serial_ui.vc_err = None;
        #[cfg(unix)]
        let r = crate::vcom::Vcom::start_pty(1, self.session_slot, self.ctx.clone());
        #[cfg(not(unix))]
        let r = {
            let pairs = crate::vcom::pairs();
            if pairs.is_empty() {
                // 没装 com0com：在侧边栏提示一键安装（用户点了才装）
                self.serial_ui.vc_need_install = true;
                return;
            }
            let busy = |port: &str| {
                #[cfg(windows)]
                let mapped = self.mappings.serial_view_port(port).is_some();
                #[cfg(not(windows))]
                let mapped = false;
                mapped || self.serial_port_busy(port) || self.local_vcom_owner(port).is_some()
            };
            let available = |pair: &crate::vcom::Pair| !busy(&pair.mine) && !busy(&pair.other);
            // 优先用上次的串口对，也可以用其他空闲的串口对。
            let Some(pair) = pairs
                .iter()
                .find(|pair| pair.mine == self.s.vcom_port && available(pair))
                .or_else(|| pairs.iter().find(|pair| available(pair)))
            else {
                self.serial_ui.vc_need_install = false;
                self.serial_ui.vc_err = Some(tf("虚拟串口打不开：{}", &[&t("该串口正在使用")]));
                return;
            };
            self.s.vcom_port = pair.mine.clone();
            crate::vcom::Vcom::start_port(&pair.mine, &pair.other, self.s.cfg.baud, self.ctx.clone())
        };
        match r {
            Ok(v) => {
                self.serial_ui.vc_need_install = false;
                for n in &v.names {
                    self.buf.sys(tf("虚拟串口已打开，其他软件请打开：{}（和 QCCOM 同时收发，不占用真实 COM 口）", &[n]));
                }
                self.vcom = Some(v);
            }
            Err(e) => self.serial_ui.vc_err = Some(tf("虚拟串口打不开：{}", &[&e])),
        }
    }

    #[cfg(windows)]
    fn start_com0com_install(&mut self) {
        let Some(port) = self.connected.clone() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::vcom::install_com0com());
            ctx.request_repaint();
        });
        self.serial_ui.vc_installing = Some((port, self.conn_gen, rx));
        self.serial_ui.vc_err = None;
    }

    pub(super) fn poll_vcom(&mut self) {
        if let Some((port, generation, rx)) = &self.serial_ui.vc_installing {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    let same_connection = self.conn.is_some() && self.conn_gen == *generation && self.connected.as_deref() == Some(port.as_str());
                    self.serial_ui.vc_installing = None;
                    self.serial_ui.vc_need_install = false;
                    self.toast(t("虚拟串口驱动装好了"), OK());
                    if same_connection && self.vcom.is_none() {
                        self.toggle_vcom();
                    }
                }
                Ok(Err(error)) => {
                    self.serial_ui.vc_installing = None;
                    self.serial_ui.vc_err = Some(error);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.serial_ui.vc_installing = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => self.ctx.request_repaint_after(Duration::from_millis(300)),
            }
        }
        let Some(v) = &mut self.vcom else { return };
        let mut evts = vec![];
        while let Ok(e) = v.rx.try_recv() {
            evts.push(e);
        }
        for e in evts {
            match e {
                crate::vcom::VcomEvt::Data(d) => {
                    if self.conn.is_some() {
                        self.send_bytes(d);
                    }
                }
                crate::vcom::VcomEvt::Error(e) => {
                    self.buf.sys(tf("✗ 共享出错：{}", &[&e]));
                    self.vcom = None;
                }
            }
        }
    }

    fn vcom_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut on = self.vcom.is_some();
            let tip = t("再给其他软件（烧录工具、别的串口助手）一个虚拟串口：大家同时收发同一个设备，不占用 COM 口，不用来回断开");
            if ui.add_enabled_ui(self.conn.is_some() || on, |ui| toggle_chip(ui, &mut on, t("共享给本机软件"), tip)).inner {
                self.toggle_vcom();
            }
        });
        let names: Vec<String> = self.vcom.as_ref().map(|v| v.names.clone()).unwrap_or_default();
        for n in names {
            // Linux「/dev/pts/5  →  …/vcom/ttyQCCOM0」：固定名字的链接更好记，显示短一点，复制完整路径
            let path = n.split("  →  ").last().unwrap_or(&n).to_string();
            let short = match n.split_once("  →  ") {
                Some((dev, link)) => format!("{} · {dev}", link.rsplit(['/', '\\']).next().unwrap_or(link)),
                None => n.clone(),
            };
            ui.horizontal(|ui| {
                if icon_button(ui, Icon::Copy, &tf("复制：{}", &[&path]), false).clicked() {
                    ui.ctx().copy_text(path.clone());
                }
                ui.label(RichText::new(t("其他软件打开")).size(11.5).color(TEXT3()));
                ui.add(egui::Label::new(RichText::new(&short).font(FontId::new(12.0, FontFamily::Monospace)).strong().color(OK())).truncate())
                    .on_hover_text(tf("在烧录工具或别的串口助手里打开 {}，和 QCCOM 同时用，不用断开", &[&path]));
            });
        }
        // Windows 没装驱动：用户点了「虚拟串口」才提示，点按钮一键装
        if self.vcom.is_none() && ((self.conn.is_some() && self.serial_ui.vc_need_install) || self.serial_ui.vc_installing.is_some()) {
            egui::Frame::new()
                .fill(BG2())
                .stroke(st(1.0, LINE()))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(t("Windows 要先装一次虚拟串口驱动")).size(12.5).strong().color(TEXT()));
                    ui.label(
                        RichText::new(t("用的是免费开源的 com0com，点安装时自动下载官方安装包（约 250 KB）并校验。装好后自动建一对串口，以后直接用"))
                            .size(11.5)
                            .color(TEXT2()),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if self.serial_ui.vc_installing.is_some() {
                            ui.spinner();
                            ui.label(RichText::new(t("正在下载并安装…请在弹出的窗口里点「是」")).size(12.0).color(TEXT2()));
                        } else {
                            if ui
                                .add(egui::Button::new(RichText::new(t("一键安装")).color(ON_ACCENT())).fill(ACCENT()))
                                .on_hover_text(t("会弹出一次 Windows 管理员确认"))
                                .clicked()
                            {
                                #[cfg(windows)]
                                self.start_com0com_install();
                            }
                            if ui.button(t("以后再说")).clicked() {
                                self.serial_ui.vc_need_install = false;
                                self.serial_ui.vc_err = None;
                            }
                        }
                    });
                });
        }
        if let Some(e) = self.serial_ui.vc_err.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(e).size(11.5).color(ERR()));
                if cfg!(windows) && self.serial_ui.vc_need_install {
                    ui.hyperlink_to(RichText::new(t("手动下载 com0com")).size(11.5), "https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/");
                }
            });
        }
    }

    pub(super) fn bridge_ui(&mut self, ui: &mut egui::Ui) {
        self.vcom_ui(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut on = self.bridge.is_some();
            if ui
                .add_enabled_ui(self.conn.is_some() || on, |ui| {
                    toggle_chip(ui, &mut on, t("局域网 TCP 透传"), t("开一个 TCP 服务器：连上来的客户端和串口双向透传"))
                })
                .inner
            {
                self.toggle_bridge();
            }
            if self.bridge.is_none() {
                ui.add_enabled(self.conn.is_some(), egui::DragValue::new(&mut self.s.bridge_port).range(1..=65535).prefix(":"));
            } else {
                ui.label(RichText::new(format!(":{}", self.s.bridge_port)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
            }
        });
        if let Some(b) = &self.bridge {
            ui.label(RichText::new(tf("{} 个客户端", &[&b.clients])).small().color(if b.clients > 0 { OK() } else { TEXT3() }));
        }
    }

    // ------------------------------------------------------------ 端口卡片右键菜单

    pub(super) fn port_menu(&mut self, ui: &mut egui::Ui, p: &PortEntry, can_listen: bool) {
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
        if p.name != self.s.port && (can_listen || is_b) {
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
        if menu_item(ui, t("插拔记录…"), "") {
            self.open_plug_log(&p.name);
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
            log_preset: Some(s.log_preset),
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
            keyword_colors: s.keyword_colors.clone(),
        }
    }

    fn save_profile(&mut self, name: &str) -> Result<(), String> {
        let name = safe_name(name);
        if name.is_empty() {
            return Err(t("请输入方案名称").into());
        }
        let p = profile_dir().join(format!("{name}.json"));
        let bytes = serde_json::to_vec_pretty(&self.current_profile()).map_err(|e| e.to_string())?;
        crate::history::save_file(&p, &bytes)?;
        self.toast(tf("已保存配置方案「{}」", &[&name]), OK());
        self.s.profile = name;
        Ok(())
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
        if let Some(v) = pr.transport {
            self.use_slot(sess::slot_of(v));
            self.s.transport = v;
        }
        let target = if self.s.transport == Transport::Serial && !pr.port.is_empty() {
            self.choose_serial_port(&pr.port, false);
            if self.work.focus_side {
                self.side_pane().and_then(|pane| pane.serial_slot(self.selected_serial)).unwrap_or(self.session_slot)
            } else {
                self.session_slot
            }
        } else {
            self.session_slot
        };
        self.with_session(target, |app| app.apply_profile(name, pr));
    }

    fn apply_profile(&mut self, name: &str, pr: Profile) {
        if self.conn.is_some() || self.reconnect.is_some() {
            self.disconnect();
        }
        let s = &mut self.s;
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
        if let Some(v) = pr.log_preset {
            s.log_preset = v;
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
        s.keyword_colors = pr.keyword_colors;
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
            .add(
                egui::Button::new(RichText::new(format!("{label} ▾")).size(11.5).color(TEXT2()))
                    .fill(Color32::TRANSPARENT)
                    .stroke(st(1.0, LINE()))
                    .min_size(vec2(0.0, 22.0)),
            )
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
            if menu_item(ui, t("保存当前为新方案…"), "") {
                self.serial_ui.profile_new = Some(ProfileDraft {
                    name: String::new(),
                    focus_name: true,
                    error: None,
                });
                ui.memory_mut(|m| m.close_popup());
            }
            if !self.s.profile.is_empty() && menu_item(ui, &tf("覆盖「{}」", &[&self.s.profile]), "") {
                let current = self.s.profile.clone();
                if let Err(e) = self.save_profile(&current) {
                    self.toast(tf("保存失败：{}", &[&e]), ERR());
                }
                ui.memory_mut(|m| m.close_popup());
            }
            if !self.s.profile.is_empty() && menu_item(ui, &tf("删除「{}」", &[&self.s.profile]), "") {
                let _ = std::fs::remove_file(profile_dir().join(format!("{}.json", self.s.profile)));
                self.s.profile.clear();
                ui.memory_mut(|m| m.close_popup());
            }
        });
        self.new_profile_dialog(ui.ctx());
    }

    fn new_profile_dialog(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.serial_ui.profile_new.as_mut() else { return };
        let mut save = false;
        let mut open = true;
        let mut cancel = false;
        egui::Window::new(t("保存当前为新方案…"))
            .id(egui::Id::new("serial-new-profile"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                let name = ui.add(egui::TextEdit::singleline(&mut draft.name).hint_text(t("方案名称")).desired_width(f32::INFINITY));
                if std::mem::take(&mut draft.focus_name) {
                    name.request_focus();
                }
                if name.changed() {
                    draft.error = None;
                }
                let filled = !draft.name.trim().is_empty();
                save = filled && name.lost_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
                ui.horizontal(|ui| {
                    save |= ui.add_enabled(filled, egui::Button::new(t("保存"))).clicked();
                    cancel = ui.button(t("取消")).clicked();
                });
                if let Some(error) = &draft.error {
                    ui.colored_label(ERR(), error);
                }
            });
        if !open || cancel {
            self.serial_ui.profile_new = None;
        } else if save {
            let name = draft.name.clone();
            match self.save_profile(&name) {
                Ok(()) => self.serial_ui.profile_new = None,
                Err(e) => {
                    self.serial_ui.profile_new.as_mut().unwrap().error = Some(tf("保存失败：{}", &[&e]));
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod mcu_serial_tests {
    use super::*;

    #[test]
    fn monitor_uses_the_selected_debug_device_and_its_baud_rate() {
        use portable_pty::{native_pty_system, PtySize};
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let port = pair.master.tty_name().unwrap().into_os_string().into_string().unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx);
        let settings = Settings {
            telemetry: false,
            auto_update: false,
            ..Default::default()
        };
        let mut app = App::with_settings(&cc, settings, vec![], vec![]);
        app.ports.push(PortEntry {
            name: port.clone(),
            desc: String::new(),
        });
        app.choose_serial_port(&port, false);
        let slot = app.serial_sidebar_slot();
        app.with_session(slot, |app| app.s.cfg.baud = 57_600);
        app.monitor_mcu_serial(slot);
        app.with_session(slot, |app| {
            assert_eq!(app.connected.as_deref(), Some(port.as_str()));
            assert_eq!(app.s.cfg.baud, 57_600);
        });
        assert!(app.shells.tabs.is_empty());
    }
}

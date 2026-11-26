//! Network sharing controls and connection status for the serial sidebar.
use crate::{
    i18n::{t, tf},
    theme::*,
    vcom::Pair,
};
use eframe::egui::{self, RichText};
use qccom_tunnel::{
    mapping::{check_share_server, valid_com, Mapping, MappingMessage, Node, Notify, SerialTraffic, SerialWriteError, SerialWriter, Service, State, TrafficObserver, DEFAULT_SERVER},
    sharing::ShareInvitation,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch, OnceCell},
    task::JoinHandle,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub server: String,
    shared: Vec<Mapping>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
            shared: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Mode {
    Share,
    Join,
}

type Key = (Mode, Service, u16);

impl Mode {
    fn key(self, map: &Mapping) -> Key {
        (self, map.service, map.number)
    }
}

struct CopyAddress {
    text: String,
    relay: String,
    expires_at: u64,
}

struct Progress {
    state: State,
    failure: Option<FailureStep>,
    address: Option<Arc<CopyAddress>>,
    ended: bool,
}

#[derive(Clone, Copy)]
enum FailureStep {
    Server,
    Peer,
}

struct JoinedMapping {
    key: Key,
    map: Mapping,
    invitation: ShareInvitation,
}

enum ServerTest {
    Running(std::sync::mpsc::Receiver<Result<(), String>>),
    Complete(Result<(), String>),
}

struct Active {
    task: JoinHandle<()>,
    progress: watch::Receiver<Progress>,
    serial: Option<SerialMonitor>,
}

struct SerialMonitor {
    port: String,
    driver_port: String,
    traffic: mpsc::Receiver<SerialTraffic>,
    writer: SerialWriter,
    announced: bool,
}

impl Active {
    fn running(&self) -> bool {
        !self.task.is_finished() && !self.progress.borrow().ended
    }

    fn serial_online(&self) -> bool {
        self.running() && matches!(self.progress.borrow().state, State::Online { .. })
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Editor {
    mode: Mode,
    map: Mapping,
    original: Option<Key>,
    address: String,
    parsed: Option<ShareInvitation>,
    focus_address: bool,
}

impl Editor {
    fn new(mode: Mode, map: Mapping) -> Self {
        Self {
            mode,
            map,
            original: None,
            address: String::new(),
            parsed: None,
            focus_address: mode == Mode::Join,
        }
    }
}

#[derive(Default)]
pub struct Panel {
    rt: Option<Runtime>,
    host_node: Option<Arc<OnceCell<Node>>>,
    server_test: Option<ServerTest>,
    active: HashMap<Key, Active>,
    joined: Vec<JoinedMapping>,
    editor: Option<Editor>,
    pairs: Vec<Pair>,
    message: String,
    copied: Option<Key>,
    log_selection: Option<String>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.active.clear();
        self.tick();
    }
}

fn com_map(name: &str) -> Mapping {
    let mut map = Mapping::new(Service::Com);
    map.serial = name.to_owned();
    map.label = map.serial.clone();
    map
}

impl Panel {
    pub fn tick(&mut self) {
        if let Some(ServerTest::Running(result)) = &self.server_test {
            match result.try_recv() {
                Ok(result) => self.server_test = Some(ServerTest::Complete(result)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.server_test = Some(ServerTest::Complete(Err(t("服务器测试未完成，请重试。").into())));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.rt.is_none() {
            return;
        }
        if !self.host_running() {
            self.host_node = None;
        }
        if !self.running() {
            if let Some(runtime) = self.rt.take() {
                runtime.shutdown_background();
            }
        }
    }

    fn running(&self) -> bool {
        self.active.values().any(Active::running)
    }

    fn host_running(&self) -> bool {
        self.active.iter().any(|(key, job)| key.0 == Mode::Share && job.running())
    }

    fn rows<'a>(&'a self, prefs: &'a Preferences) -> impl Iterator<Item = (Key, &'a Mapping)> {
        prefs
            .shared
            .iter()
            .map(|map| (Mode::Share.key(map), map))
            .chain(self.joined.iter().map(|joined| (joined.key, &joined.map)))
    }

    fn available_number(&self, mode: Mode, service: Service, prefs: &Preferences) -> Result<u16, String> {
        (service.port()..=u16::MAX)
            .chain(1..service.port())
            .find(|number| self.rows(prefs).all(|(key, _)| key != (mode, service, *number)))
            .ok_or_else(|| t("没有可用的映射编号").into())
    }

    fn start(&mut self, key: Key, map: &Mapping, prefs: &Preferences, invitation: Option<ShareInvitation>, ctx: &egui::Context) -> Result<(), String> {
        map.validate().map_err(|error| mapping_error(&error))?;
        if self.active.get(&key).is_some_and(Active::running) {
            return Err(t("映射已启动").into());
        }
        if map.service == Service::Com && (self.owns(&map.serial, prefs) || self.owns(&map.label, prefs)) {
            return Err(t("该端口已有映射").into());
        }
        if key.0 == Mode::Join && !invitation.as_ref().is_some_and(|invite| invite.voucher().expires_at() > unix_now()) {
            return Err(t("分享地址无效或已过期").into());
        }
        self.active.remove(&key);
        self.tick();
        if self.rt.is_none() {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().map_err(|e| e.to_string())?;
            self.rt = Some(runtime);
        }
        let map = map.clone();
        let node_cell = match key.0 {
            Mode::Share => Some(self.host_node.get_or_insert_with(Default::default).clone()),
            Mode::Join => None,
        };
        let server = prefs.server.clone();
        let ctx = ctx.clone();
        let (observer, serial) = if map.service == Service::Com {
            let repaint = ctx.clone();
            let (observer, traffic, writer) = TrafficObserver::channel(move || repaint.request_repaint());
            (
                Some(observer),
                Some(SerialMonitor {
                    port: map.label.clone(),
                    driver_port: map.serial.clone(),
                    traffic,
                    writer,
                    announced: false,
                }),
            )
        } else {
            (None, None)
        };
        let (tx, progress) = watch::channel(Progress {
            state: State::Registering,
            failure: None,
            address: None,
            ended: false,
        });
        let task = self.rt.as_ref().expect("mapping runtime exists").spawn(async move {
            let updates = tx.clone();
            let repaint = ctx.clone();
            let notify: Notify = Arc::new(move |state| {
                updates.send_modify(|status| status.state = state);
                repaint.request_repaint();
            });
            let result = async {
                match key.0 {
                    Mode::Share => {
                        let cell = node_cell.expect("host mapping has a shared node cache");
                        let node = cell.get_or_try_init(|| Node::bind_share_host(&server, false)).await?;
                        node.share_observed(map, notify, observer, |text| {
                            if let Ok(invite) = ShareInvitation::parse(&text) {
                                let address = CopyAddress {
                                    text,
                                    relay: invite.relay().as_str().trim_start_matches("https://").trim_end_matches('/').to_owned(),
                                    expires_at: invite.voucher().expires_at(),
                                };
                                tx.send_modify(|status| status.address = Some(Arc::new(address)));
                                ctx.request_repaint();
                            }
                        })
                        .await
                    }
                    Mode::Join => {
                        let invitation = invitation.expect("valid share address checked before starting");
                        let node = Node::bind_share_client(invitation, false).await?;
                        node.run_observed(map, notify, observer).await
                    }
                }
            }
            .await;
            tx.send_modify(|status| {
                if !matches!(status.state, State::Expired) {
                    status.failure = match status.state {
                        State::Registering => Some(FailureStep::Server),
                        State::Punching | State::Handshaking => Some(FailureStep::Peer),
                        _ => None,
                    };
                    status.state = State::Failed(Arc::new(match result {
                        Ok(()) => MappingMessage::new("连接已结束，请重新连接").into(),
                        Err(error) => error,
                    }));
                }
                status.ended = true;
                status.address = None;
            });
            ctx.request_repaint();
        });
        self.active.insert(key, Active { task, progress, serial });
        self.copied = self.copied.filter(|copied| *copied != key);
        self.message.clear();
        Ok(())
    }

    pub fn share_com_from_row(&mut self, ui: &egui::Ui, name: &str, prefs: &mut Preferences, busy: bool) {
        let result = (|| {
            if busy {
                return Err(t("该串口正在使用，请先断开本机连接").into());
            }
            if crate::vcom::pairs().iter().any(|pair| pair.mine == name || pair.other == name) {
                return Err(t("请选择实体串口").into());
            }
            let mut map = com_map(name);
            map.number = self.available_number(Mode::Share, Service::Com, prefs)?;
            self.start(Mode::Share.key(&map), &map, prefs, None, ui.ctx())?;
            prefs.shared.push(map);
            Ok(())
        })();
        if let Err(error) = result {
            self.message = error;
        }
    }

    pub fn advanced(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences) {
        ui.separator();
        ui.label(RichText::new(t("穿透服务器")).small().color(TEXT2()));
        let testing = matches!(self.server_test, Some(ServerTest::Running(_)));
        let editable = !self.host_running() && !testing;
        if ui
            .add_enabled(editable, egui::TextEdit::singleline(&mut prefs.server).desired_width(f32::INFINITY).hint_text(DEFAULT_SERVER))
            .changed()
        {
            self.server_test = None;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(editable, egui::Button::new(t("恢复默认")).small()).clicked() {
                prefs.server = DEFAULT_SERVER.into();
                self.server_test = None;
            }
            if ui.add_enabled(!testing && !prefs.server.trim().is_empty(), egui::Button::new(t("测试服务器")).small()).clicked() {
                self.test_server(prefs.server.trim().to_owned(), ui.ctx().clone());
            }
        });
        match &self.server_test {
            Some(ServerTest::Running(_)) => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(ACCENT()));
                    ui.small(t("正在测试服务器"));
                });
            }
            Some(ServerTest::Complete(Ok(()))) => {
                ui.colored_label(OK(), t("穿透服务器连接成功"));
            }
            Some(ServerTest::Complete(Err(error))) => {
                ui.colored_label(ERR(), error);
            }
            None => {}
        }
        if !self.message.is_empty() {
            ui.colored_label(ERR(), &self.message);
        }
    }

    fn test_server(&mut self, server: String, ctx: egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.server_test = Some(ServerTest::Running(rx));
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
                .and_then(|runtime| runtime.block_on(check_share_server(&server)).map_err(|error| mapping_error(&error)));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn open_editor(&mut self, mode: Mode, mut map: Mapping) {
        self.pairs = crate::vcom::pairs();
        if map.service == Service::Com && mode == Mode::Join && !self.pairs.iter().any(|pair| pair.mine == map.serial) {
            self.pick_virtual(&mut map);
        }
        self.editor = Some(Editor::new(mode, map));
        self.message.clear();
    }

    fn pick_virtual(&self, map: &mut Mapping) {
        if let Some(pair) = self.pairs.first() {
            map.serial.clone_from(&pair.mine);
            map.label.clone_from(&pair.other);
        } else {
            map.serial.clear();
            map.label.clear();
        }
    }

    pub fn owns(&self, port: &str, _prefs: &Preferences) -> bool {
        self.serial_view_port(port).is_some()
    }

    fn serial_mapping(&self, port: &str) -> Option<(&Active, &SerialMonitor)> {
        self.active.values().filter(|job| job.running()).find_map(|job| {
            let monitor = job.serial.as_ref()?;
            (monitor.port == port || monitor.driver_port == port).then_some((job, monitor))
        })
    }

    pub fn serial_view_port(&self, port: &str) -> Option<&str> {
        self.serial_mapping(port).map(|(_, monitor)| monitor.port.as_str())
    }

    pub fn stop_serial_view(&mut self, port: &str) -> bool {
        let key = self.active.iter().find_map(|(&key, job)| {
            let monitor = job.serial.as_ref()?;
            (job.running() && (monitor.port == port || monitor.driver_port == port)).then_some(key)
        });
        if let Some(key) = key {
            self.active.remove(&key);
            self.tick();
            true
        } else {
            false
        }
    }

    pub fn serial_view_ports(&self) -> impl Iterator<Item = &str> {
        self.active
            .values()
            .filter(|job| job.running())
            .filter_map(|job| job.serial.as_ref().map(|monitor| monitor.port.as_str()))
    }

    pub fn serial_online(&self, port: &str) -> bool {
        self.serial_direct(port).is_some()
    }

    pub fn serial_direct(&self, port: &str) -> Option<bool> {
        let (job, _) = self.serial_mapping(port)?;
        match &job.progress.borrow().state {
            State::Online { direct, .. } => Some(*direct),
            _ => None,
        }
    }

    pub fn send_serial(&self, port: &str, data: Vec<u8>) -> Result<(), &'static str> {
        let (_, monitor) = self.serial_mapping(port).ok_or_else(|| t("未连接"))?;
        monitor.writer.try_send(data).map_err(|error| match error {
            SerialWriteError::Offline => t("未连接"),
            SerialWriteError::Full => t("发送队列已满，请稍后再试"),
            SerialWriteError::TooLarge => t("单次网络串口发送最多 64 KiB"),
        })
    }

    pub fn take_log_selection(&mut self) -> Option<String> {
        self.log_selection.take()
    }

    pub fn poll_serial_traffic(&mut self, ctx: &egui::Context) -> Vec<(String, Vec<SerialTraffic>)> {
        let mut ready = Vec::new();
        for job in self.active.values_mut() {
            let online = job.serial_online();
            let Some(serial) = &mut job.serial else { continue };
            let packets: Vec<_> = (0..8).map_while(|_| serial.traffic.try_recv().ok()).collect();
            if !serial.traffic.is_empty() {
                ctx.request_repaint();
            }
            if !packets.is_empty() || (online && !serial.announced) {
                ready.push((serial.port.clone(), packets));
            }
            serial.announced = online;
        }
        ready
    }

    pub fn hidden_driver_end(&self, port: &str, prefs: &Preferences) -> bool {
        self.rows(prefs).any(|(_, map)| map.service == Service::Com && map.serial == port && map.label != port)
    }

    pub fn count(&self, prefs: &Preferences) -> usize {
        prefs.shared.len() + self.joined.len()
    }

    pub fn hint(&self, prefs: &Preferences) -> Option<String> {
        let (key, map) = self
            .rows(prefs)
            .filter(|(key, _)| self.active.get(key).is_some_and(|job| matches!(job.progress.borrow().state, State::Online { .. })))
            .min_by_key(|(_, map)| map.service != Service::Com)?;
        if key.0 == Mode::Share {
            let label = if map.service == Service::Com { map.label.clone() } else { format!("{}:{}", map.host, map.port) };
            return Some(tf("已与使用端连接，正在共享 {}", &[&label]));
        }
        Some(client_hint(map))
    }

    pub fn has_com_share(&self, port: &str, prefs: &Preferences) -> bool {
        prefs.shared.iter().any(|map| map.service == Service::Com && map.serial == port)
    }

    fn forget(&mut self, key: Key, prefs: &mut Preferences) {
        self.active.remove(&key);
        match key.0 {
            Mode::Share => prefs.shared.retain(|map| Mode::Share.key(map) != key),
            Mode::Join => self.joined.retain(|joined| joined.key != key),
        }
        self.tick();
    }

    fn row(&mut self, ui: &mut egui::Ui, key: Key, map: &Mapping, prefs: &mut Preferences, busy_ports: &[String]) {
        let active = self.active.get(&key);
        let running = active.is_some_and(Active::running);
        let (state, address, failure) = active.map_or((None, None, None), |job| {
            let progress = job.progress.borrow();
            (Some(progress.state.clone()), progress.address.clone(), progress.failure)
        });
        let local = if map.service == Service::Com {
            map.label.clone()
        } else {
            format!("{} :{}", map.service.title(), map.port)
        };
        let (status, color, peer) = match &state {
            Some(State::Online { direct, peer }) => (if *direct { t("已直连") } else { t("服务器中继") }, if *direct { OK() } else { WARN() }, peer.as_str()),
            Some(State::Punching) => (t("打洞中"), ACCENT(), ""),
            Some(State::Registering) => (t("连接服务器"), TEXT2(), ""),
            Some(State::Handshaking) => (t("正在握手"), ACCENT(), ""),
            Some(State::Waiting) => (t("等待对端"), ACCENT(), ""),
            Some(State::Expired) => (t("地址已过期"), WARN(), ""),
            Some(State::ServiceError(_)) => (t("目标服务异常"), WARN(), ""),
            Some(State::Failed(_)) if failure.is_some() => (t("网络分享失败"), ERR(), ""),
            Some(State::Failed(_)) => (t("已断开"), ERR(), ""),
            None => (t("已停止"), TEXT3(), ""),
        };
        let host = key.0 == Mode::Share;
        let pending = running && matches!(state, Some(State::Registering | State::Punching | State::Handshaking));
        let address_valid = self
            .joined
            .iter()
            .find(|joined| joined.key == key)
            .is_some_and(|joined| joined.invitation.voucher().expires_at() > unix_now());
        let replace_address = !running && !host && !address_valid;
        let kind = if map.service == Service::Com {
            if host {
                t("实体")
            } else {
                t("虚拟")
            }
        } else {
            "TCP"
        };
        let mut toggle = false;
        let mut remove = false;
        let mut edit = false;
        egui::Frame::new()
            .fill(BG2())
            .stroke(egui::Stroke::new(1.0_f32, LINE()))
            .corner_radius(6)
            .inner_margin(7)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&local).monospace().strong().color(ACCENT()));
                    if !peer.is_empty() {
                        ui.label(RichText::new("⇄").color(TEXT3()));
                        ui.add(egui::Label::new(RichText::new(peer).monospace()).truncate()).on_hover_text(peer);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(kind).small().color(TEXT3()));
                    if pending {
                        ui.add(egui::Spinner::new().size(12.0).color(ACCENT()));
                    }
                    ui.label(RichText::new(status).small().color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let action = if pending {
                            t("取消")
                        } else if running {
                            t("停止")
                        } else if replace_address {
                            t("更换分享地址")
                        } else if matches!(state, Some(State::Failed(_))) {
                            t("重试")
                        } else {
                            t("重新共享")
                        };
                        toggle = ui.small_button(action).clicked();
                        if !running {
                            ui.menu_button("⋯", |ui| {
                                let label = if host { t("编辑") } else { t("更换分享地址") };
                                if !replace_address && ui.button(label).clicked() {
                                    edit = true;
                                    ui.close_menu();
                                }
                                if ui.button(t("移除")).clicked() {
                                    remove = true;
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
                if running {
                    if map.service == Service::Com && ui.small_button(t("查看日志")).clicked() {
                        self.log_selection = Some(map.label.clone());
                    }
                    if let Some(address) = &address {
                        let remaining = address.expires_at.saturating_sub(unix_now());
                        if remaining > 0 {
                            ui.horizontal_wrapped(|ui| {
                                let label = if self.copied == Some(key) { t("已复制") } else { t("复制分享地址") };
                                if ui.button(label).on_hover_text(t("仅发给需要连接的人，地址有效期 15 分钟")).clicked() {
                                    ui.ctx().copy_text(address.text.clone());
                                    self.copied = Some(key);
                                }
                                ui.add(egui::Label::new(RichText::new(&address.relay).small().monospace()).truncate()).on_hover_text(&address.relay);
                            });
                            ui.ctx().request_repaint_after(Duration::from_secs(remaining));
                        } else {
                            ui.small(t("连接地址已过期，停止后可重新共享"));
                        }
                    }
                    if !host && matches!(state, Some(State::Online { .. })) {
                        ui.small(client_hint(map));
                    }
                }
                if let Some(State::Failed(error) | State::ServiceError(error)) = &state {
                    ui.label(RichText::new(mapping_error(error)).small().color(ERR()));
                }
                if let Some(failure) = failure {
                    let hint = match failure {
                        FailureStep::Server if host => t("请检查高级设置中的穿透服务器地址及连通性。"),
                        FailureStep::Server => t("请确认此设备能连接分享地址中的服务器。"),
                        FailureStep::Peer => t("请确认分享端在线，且分享地址仍有效。"),
                    };
                    ui.small(hint);
                }
            });
        if toggle {
            if running {
                self.active.remove(&key);
                if key.0 == Mode::Join {
                    self.forget(key, prefs);
                }
                self.tick();
            } else if replace_address {
                edit = true;
            } else {
                self.report_start(key, map, prefs, busy_ports, ui.ctx());
            }
        }
        if edit {
            self.open_editor(key.0, map.clone());
            if let Some(editor) = &mut self.editor {
                editor.original = Some(key);
            }
        }
        if remove {
            self.forget(key, prefs);
        }
    }

    fn report_start(&mut self, key: Key, map: &Mapping, prefs: &Preferences, busy_ports: &[String], ctx: &egui::Context) {
        if map.service == Service::Com && (busy_ports.contains(&map.serial) || busy_ports.contains(&map.label)) {
            self.message = t("该串口正在使用，请先断开本机连接").into();
        } else {
            let invitation = self.joined.iter().find(|joined| joined.key == key).map(|joined| joined.invitation.clone());
            if let Err(error) = self.start(key, map, prefs, invitation, ctx) {
                self.message = error;
            }
        }
    }

    pub fn network_ui(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, ports: &[crate::serial::PortEntry], selected: &str, busy_ports: &[String]) {
        if self.editor.is_none() {
            ui.horizontal_wrapped(|ui| {
                if ui.button(t("关联网络分享")).clicked() {
                    self.open_editor(Mode::Join, com_map(""));
                }
                if ui.small_button(t("分享到网络")).clicked() {
                    let map = if ports.iter().any(|port| valid_com(&port.name)) {
                        com_map(selected)
                    } else {
                        Mapping::new(Service::Mqtt)
                    };
                    self.open_editor(Mode::Share, map);
                }
            });
        }
        if let Some(mut editor) = self.editor.take() {
            let (close, submit) = self.editor_ui(ui, &mut editor, ports, selected);
            let complete = submit
                && match self.submit_editor(&mut editor, prefs, busy_ports, ui.ctx()) {
                    Ok(()) => true,
                    Err(error) => {
                        self.message = error;
                        false
                    }
                };
            if close {
                self.message.clear();
            } else if !complete {
                self.editor = Some(editor);
            }
        }
        if !self.message.is_empty() {
            ui.label(RichText::new(&self.message).small().color(ERR()));
        }
        let mappings: Vec<_> = self.rows(prefs).map(|(key, map)| (key, map.clone())).collect();
        for (key, map) in mappings {
            ui.push_id(key, |ui| self.row(ui, key, &map, prefs, busy_ports));
        }
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, editor: &mut Editor, ports: &[crate::serial::PortEntry], selected: &str) -> (bool, bool) {
        let mut close = false;
        let mut submit = false;
        egui::Frame::new()
            .fill(BG2())
            .stroke(egui::Stroke::new(1.0_f32, LINE()))
            .corner_radius(7)
            .inner_margin(8)
            .show(ui, |ui| {
                let host = editor.mode == Mode::Share;
                ui.horizontal(|ui| {
                    if editor.mode == Mode::Join {
                        ui.strong(t("关联网络分享"));
                    } else {
                        for service in [Service::Com, Service::Mqtt, Service::Ftp] {
                            let label = if service == Service::Com { t("串口") } else { service.title() };
                            if ui.selectable_label(service == editor.map.service, label).clicked() && service != editor.map.service {
                                editor.map = if service == Service::Com { com_map(selected) } else { Mapping::new(service) };
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = ui.small_button("×").on_hover_text(t("收起")).clicked();
                    });
                });
                let mut enter = false;
                if editor.mode == Mode::Join {
                    let input = ui.add(
                        egui::TextEdit::singleline(&mut editor.address)
                            .hint_text(t("粘贴分享地址"))
                            .desired_width(f32::INFINITY)
                            .char_limit(2048),
                    );
                    if editor.focus_address {
                        input.request_focus();
                        editor.focus_address = false;
                    }
                    enter = input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if input.changed() {
                        editor.parsed = ShareInvitation::parse(&editor.address).ok();
                        if let Some(invite) = &editor.parsed {
                            let service = Service::from(invite.voucher().service());
                            if service != editor.map.service {
                                editor.map = Mapping::new(service);
                                self.set_join_defaults(&mut editor.map);
                            }
                            editor.map.number = invite.voucher().number();
                        }
                    }
                    if editor.parsed.is_none() {
                        if !editor.address.trim().is_empty() {
                            ui.colored_label(ERR(), t("分享地址无效或已过期"));
                        }
                        return;
                    }
                    ui.small(editor.map.service.title());
                }
                self.mapping_fields(ui, &mut editor.map, ports, host);
                if host && editor.map.service == Service::Ftp {
                    ui.collapsing(t("映射选项"), |ui| {
                        ui.small(t("FTP 服务的被动端口范围"));
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut editor.map.passive.0).range(1024..=65535));
                            ui.label("–");
                            ui.add(egui::DragValue::new(&mut editor.map.passive.1).range(1024..=65535));
                        });
                    });
                }
                let valid = editor.map.validate().is_ok();
                let action = if host { t("分享到网络") } else { t("关联网络分享") };
                let button = egui::Button::new(RichText::new(action).strong().color(ON_ACCENT()))
                    .fill(ACCENT())
                    .min_size(egui::vec2(ui.available_width(), 29.0));
                submit = ui.add_enabled(valid, button).clicked() || (valid && enter);
            });
        (close, submit)
    }

    fn set_join_defaults(&self, map: &mut Mapping) {
        match map.service {
            Service::Com => self.pick_virtual(map),
            Service::Mqtt => {}
            Service::Ftp => map.port = 2121,
        }
    }

    fn mapping_fields(&mut self, ui: &mut egui::Ui, map: &mut Mapping, ports: &[crate::serial::PortEntry], host: bool) {
        if map.service != Service::Com {
            if host {
                ui.small(t("目标服务地址"));
                ui.text_edit_singleline(&mut map.host);
            }
            ui.horizontal(|ui| {
                ui.small(if host { t("目标端口") } else { t("本机监听端口") });
                ui.add(egui::DragValue::new(&mut map.port).range(1..=65535));
            });
            return;
        }
        if host {
            ui.small(t("共享实体串口"));
            let physical = |p: &crate::serial::PortEntry| valid_com(&p.name) && !self.pairs.iter().any(|pair| p.name == pair.mine || p.name == pair.other);
            if !ports.iter().any(|p| physical(p) && p.name == map.serial) {
                map.serial = ports.iter().find(|p| physical(p)).map(|p| p.name.clone()).unwrap_or_default();
            }
            egui::ComboBox::from_id_salt("publish-com").selected_text(&map.serial).show_ui(ui, |ui| {
                for port in ports.iter().filter(|p| physical(p)) {
                    ui.selectable_value(&mut map.serial, port.name.clone(), format!("{}  {}", port.name, port.desc));
                }
            });
            map.label.clone_from(&map.serial);
        } else {
            ui.small(t("烧录工具使用的虚拟串口"));
            egui::ComboBox::from_id_salt("virtual-com").selected_text(&map.label).show_ui(ui, |ui| {
                for pair in &self.pairs {
                    if ui.selectable_label(pair.mine == map.serial, &pair.other).clicked() {
                        map.serial.clone_from(&pair.mine);
                        map.label.clone_from(&pair.other);
                    }
                }
            });
            if self.pairs.is_empty() {
                ui.small(t("未检测到虚拟串口对"));
                if crate::vcom::com0com_setup_ui(ui) {
                    self.pairs = crate::vcom::pairs();
                    self.pick_virtual(map);
                    self.message = crate::vcom::check_com0com_signature().err().unwrap_or_default();
                }
            }
        }
    }

    fn submit_editor(&mut self, editor: &mut Editor, prefs: &mut Preferences, busy_ports: &[String], ctx: &egui::Context) -> Result<(), String> {
        let mut map = editor.map.clone();
        if map.service == Service::Com && busy_ports.contains(&map.serial) {
            return Err(t("该串口正在使用，请先断开本机连接").into());
        }
        let invitation = if editor.mode == Mode::Join {
            Some(ShareInvitation::parse(&editor.address).map_err(|_| t("分享地址无效或已过期"))?)
        } else {
            None
        };
        if editor.original.is_none() && editor.mode == Mode::Share && map.service == Service::Com {
            editor.original = prefs
                .shared
                .iter()
                .find(|saved| saved.service == Service::Com && saved.serial == map.serial)
                .map(|saved| Mode::Share.key(saved));
        }
        let key = match editor.mode {
            Mode::Share => {
                map.number = match editor.original {
                    Some(key) if key.1 == map.service => key.2,
                    _ => self.available_number(Mode::Share, map.service, prefs)?,
                };
                Mode::Share.key(&map)
            }
            Mode::Join => match editor.original {
                Some(key) if key.1 == map.service => key,
                _ => (Mode::Join, map.service, self.available_number(Mode::Join, map.service, prefs)?),
            },
        };
        if self.rows(prefs).any(|(id, _)| id == key && Some(id) != editor.original) {
            return Err(t("此映射端口已有配置，请编辑原映射或选择其他端口").into());
        }
        self.start(key, &map, prefs, invitation.clone(), ctx)?;
        if let Some(original) = editor.original {
            if original != key {
                self.forget(original, prefs);
            }
        }
        if let Some(invitation) = invitation {
            self.joined.retain(|joined| joined.key != key);
            self.joined.push(JoinedMapping { key, map, invitation });
        } else {
            prefs.shared.retain(|map| Mode::Share.key(map) != key);
            prefs.shared.push(map);
        }
        Ok(())
    }
}

fn mapping_error(error: &anyhow::Error) -> String {
    let mut details: Vec<String> = Vec::new();
    for cause in error.chain() {
        let detail = match cause.downcast_ref::<MappingMessage>() {
            Some(message) => match &message.value {
                Some(value) => tf(message.key, &[value]),
                None => t(message.key).into(),
            },
            None => cause.to_string(),
        };
        if details.last().is_none_or(|previous| previous.replace(' ', "").to_lowercase() != detail.replace(' ', "").to_lowercase()) {
            details.push(detail);
        }
    }
    details.join(": ")
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn client_hint(map: &Mapping) -> String {
    if map.service == Service::Com {
        tf("在烧录工具中选择 {}", &[&map.label])
    } else {
        tf("在 {} 客户端中连接 {}", &[&map.service.title(), &format!("127.0.0.1:{}", map.port)])
    }
}

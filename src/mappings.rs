//! Port sharing and joining actions in the device list.
use crate::{
    i18n::{t, tf},
    theme::*,
    vcom::Pair,
};
use eframe::egui::{self, RichText};
use qccom_tunnel::{
    mapping::{set_server, valid_com, Mapping, Node, Notify, Service, State, DEFAULT_SERVER},
    sharing::ShareInvitation,
    Invitation, Role,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    runtime::Runtime,
    sync::{watch, OnceCell},
    task::JoinHandle,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub server: String,
    saved: Vec<Mapping>,
    shared: Vec<Mapping>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
            saved: Vec::new(),
            shared: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Mode {
    Share,
    Join,
    Legacy,
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
    address: Option<Arc<CopyAddress>>,
    ended: bool,
}

struct Active {
    task: JoinHandle<()>,
    progress: watch::Receiver<Progress>,
}

impl Active {
    fn running(&self) -> bool {
        !self.task.is_finished() && !self.progress.borrow().ended
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
    invite: Option<Invitation>,
    loaded: bool,
    rt: Option<Runtime>,
    host_node: Option<Arc<OnceCell<Node>>>,
    legacy_node: Option<Arc<OnceCell<Node>>>,
    active: HashMap<Key, Active>,
    joined: Vec<(Key, Mapping)>,
    editor: Option<Editor>,
    pairs: Vec<Pair>,
    message: String,
    copied: Option<Key>,
    create: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.active.clear();
        self.tick();
    }
}

fn read_invite(path: &Path) -> Result<Invitation, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 16384 {
        return Err(t("凭据文件过大").into());
    }
    let invite: Invitation = serde_json::from_reader(file.take(16385)).map_err(|e| e.to_string())?;
    invite.validate().map_err(|e| e.to_string())?;
    Ok(invite)
}

fn com_map(name: &str) -> Mapping {
    let mut map = Mapping::new(Service::Com);
    map.serial = name.to_owned();
    map.label = map.serial.clone();
    map
}

impl Panel {
    pub fn tick(&mut self) {
        if self.rt.is_none() {
            return;
        }
        if !self.active.iter().any(|(key, job)| key.0 == Mode::Share && job.running()) {
            self.host_node = None;
        }
        if !self.active.iter().any(|(key, job)| key.0 == Mode::Legacy && job.running()) {
            self.legacy_node = None;
        }
        if !self.running() {
            if let Some(runtime) = self.rt.take() {
                runtime.shutdown_background();
            }
        }
    }

    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let path = crate::data_dir().join("access.json");
        if path.is_file() {
            match read_invite(&path) {
                Ok(invite) => self.invite = Some(invite),
                Err(error) => self.message = error,
            }
        }
    }

    fn import(&mut self, prefs: &mut Preferences) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new().set_title(t("选择本机部署凭据")).add_filter("QCCOM", &["json"]).pick_file() else {
            return Ok(());
        };
        let invite = read_invite(&path)?;
        let dir = crate::data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&invite).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        file.persist(dir.join("access.json")).map_err(|e| e.to_string())?;
        if self.invite.as_ref().is_some_and(|old| old.role != invite.role) {
            prefs.saved.clear();
        }
        self.active.retain(|key, _| key.0 != Mode::Legacy);
        self.invite = Some(invite);
        self.message.clear();
        Ok(())
    }

    fn running(&self) -> bool {
        self.active.values().any(Active::running)
    }

    fn is_host(&self, mode: Mode) -> bool {
        mode == Mode::Share || (mode == Mode::Legacy && self.invite.as_ref().is_some_and(|invite| invite.role == Role::Host))
    }

    fn rows<'a>(&'a self, prefs: &'a Preferences) -> impl Iterator<Item = (Key, &'a Mapping)> {
        prefs
            .shared
            .iter()
            .map(|map| (Mode::Share.key(map), map))
            .chain(prefs.saved.iter().map(|map| (Mode::Legacy.key(map), map)))
            .chain(self.joined.iter().map(|(key, map)| (*key, map)))
    }

    fn available_number(&self, mode: Mode, service: Service, prefs: &Preferences) -> Result<u16, String> {
        (service.port()..=u16::MAX)
            .chain(1..service.port())
            .find(|number| self.rows(prefs).all(|(key, _)| key != (mode, service, *number)))
            .ok_or_else(|| t("没有可用的映射编号").into())
    }

    fn start(&mut self, key: Key, map: &Mapping, prefs: &Preferences, invitation: Option<ShareInvitation>, ctx: &egui::Context) -> Result<(), String> {
        map.validate().map_err(|e| e.to_string())?;
        if self.active.get(&key).is_some_and(Active::running) {
            return Err(t("映射已启动").into());
        }
        if map.service == Service::Com && self.owns(&map.serial, prefs) {
            return Err(t("该端口已有映射").into());
        }
        let legacy_invite = if key.0 == Mode::Legacy {
            let mut invite = self.invite.clone().ok_or(t("请先导入本机凭据"))?;
            if prefs.server != DEFAULT_SERVER {
                set_server(&mut invite, prefs.server.trim()).map_err(|e| e.to_string())?;
            }
            Some(invite)
        } else {
            None
        };
        self.active.remove(&key);
        self.tick();
        if self.rt.is_none() {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().map_err(|e| e.to_string())?;
            self.rt = Some(runtime);
        }
        let map = map.clone();
        let node_cell = match key.0 {
            Mode::Share => Some(self.host_node.get_or_insert_with(Default::default).clone()),
            Mode::Legacy => Some(self.legacy_node.get_or_insert_with(Default::default).clone()),
            Mode::Join => None,
        };
        let server = prefs.server.clone();
        let ctx = ctx.clone();
        let (tx, progress) = watch::channel(Progress {
            state: State::Registering,
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
                        node.share(map, notify, |text| {
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
                        let invitation = invitation.ok_or_else(|| anyhow_message(t("请输入有效的连接地址")))?;
                        let node = Node::bind_share_client(invitation, false).await?;
                        node.run(map, notify).await
                    }
                    Mode::Legacy => {
                        let invite = legacy_invite.ok_or_else(|| anyhow_message(t("请先导入本机凭据")))?;
                        let cell = node_cell.expect("legacy mapping has a shared node cache");
                        let node = cell.get_or_try_init(|| Node::bind(invite, false)).await?;
                        node.run(map, notify).await
                    }
                }
            }
            .await;
            tx.send_modify(|status| {
                if !matches!(status.state, State::Expired) {
                    status.state = State::Failed(match result {
                        Ok(()) => t("连接已结束，请重新连接").into(),
                        Err(error) => format!("{error:#}"),
                    });
                }
                status.ended = true;
                status.address = None;
            });
            ctx.request_repaint();
        });
        self.active.insert(key, Active { task, progress });
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
        self.load();
        ui.separator();
        ui.label(RichText::new(t("穿透服务器")).small().color(TEXT2()));
        ui.add_enabled_ui(!self.running(), |ui| {
            ui.add(egui::TextEdit::singleline(&mut prefs.server).desired_width(f32::INFINITY).hint_text(DEFAULT_SERVER));
            if ui.small_button(t("恢复默认")).clicked() {
                prefs.server = DEFAULT_SERVER.into();
            }
            ui.collapsing(t("旧版映射凭据"), |ui| {
                if ui.small_button(t("导入本机凭据")).clicked() {
                    if let Err(error) = self.import(prefs) {
                        self.message = error;
                    }
                }
                if self.invite.is_some() && ui.small_button(t("新建旧版映射")).clicked() {
                    self.open_editor(Mode::Legacy, com_map(""));
                }
            });
        });
        if !self.message.is_empty() {
            ui.colored_label(ERR(), &self.message);
        }
    }

    fn open_editor(&mut self, mode: Mode, mut map: Mapping) {
        self.pairs = crate::vcom::pairs();
        if map.service == Service::Com && !self.is_host(mode) && !self.pairs.iter().any(|pair| pair.mine == map.serial) {
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

    pub fn owns(&self, port: &str, prefs: &Preferences) -> bool {
        self.rows(prefs)
            .any(|(key, map)| map.service == Service::Com && map.serial == port && self.active.get(&key).is_some_and(Active::running))
    }

    pub fn hidden_driver_end(&self, port: &str, prefs: &Preferences) -> bool {
        self.rows(prefs).any(|(_, map)| map.service == Service::Com && map.serial == port && map.label != port)
    }

    pub fn device_count(&self, ports: &[crate::serial::PortEntry], prefs: &Preferences) -> usize {
        let regular = ports.iter().filter(|p| !self.configured(&p.name, prefs) && !self.hidden_driver_end(&p.name, prefs)).count();
        regular + self.rows(prefs).count()
    }

    pub fn hint(&self, prefs: &Preferences) -> Option<String> {
        let (key, map) = self
            .rows(prefs)
            .filter(|(key, _)| self.active.get(key).is_some_and(|job| matches!(job.progress.borrow().state, State::Online { .. })))
            .min_by_key(|(_, map)| map.service != Service::Com)?;
        if self.is_host(key.0) {
            let label = if map.service == Service::Com { map.label.clone() } else { format!("{}:{}", map.host, map.port) };
            return Some(tf("已与使用端连接，正在共享 {}", &[&label]));
        }
        Some(client_hint(map))
    }

    pub fn configured(&self, port: &str, prefs: &Preferences) -> bool {
        self.rows(prefs).any(|(_, map)| map.service == Service::Com && map.label == port)
    }

    fn forget(&mut self, key: Key, prefs: &mut Preferences) {
        self.active.remove(&key);
        match key.0 {
            Mode::Share => prefs.shared.retain(|map| Mode::Share.key(map) != key),
            Mode::Legacy => prefs.saved.retain(|map| Mode::Legacy.key(map) != key),
            Mode::Join => self.joined.retain(|(id, _)| *id != key),
        }
        self.tick();
    }

    fn row(&mut self, ui: &mut egui::Ui, key: Key, map: &Mapping, prefs: &mut Preferences, busy_port: Option<&str>) -> bool {
        let active = self.active.get(&key);
        let running = active.is_some_and(Active::running);
        let (state, address) = active.map_or((None, None), |job| {
            let progress = job.progress.borrow();
            (Some(progress.state.clone()), progress.address.clone())
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
            Some(State::Waiting) => (t("等待对端"), ACCENT(), ""),
            Some(State::Expired) => (t("地址已过期"), WARN(), ""),
            Some(State::ServiceError(_)) => (t("目标服务异常"), WARN(), ""),
            Some(State::Failed(_)) => (t("已断开"), ERR(), ""),
            None => (t("已停止"), TEXT3(), ""),
        };
        let host = self.is_host(key.0);
        let kind = if map.service == Service::Com {
            if host {
                t("实体")
            } else {
                t("虚拟")
            }
        } else {
            "TCP"
        };
        let mut selected = false;
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
                    selected = ui
                        .add(egui::Label::new(RichText::new(&local).monospace().strong().color(ACCENT())).sense(egui::Sense::click()))
                        .clicked();
                    if !peer.is_empty() {
                        ui.label(RichText::new("⇄").color(TEXT3()));
                        ui.add(egui::Label::new(RichText::new(peer).monospace()).truncate()).on_hover_text(peer);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(kind).small().color(TEXT3()));
                    ui.label(RichText::new(status).small().color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if running || key.0 != Mode::Join {
                            let action = if running {
                                t("停止")
                            } else if key.0 == Mode::Share {
                                t("重新共享")
                            } else {
                                t("启动")
                            };
                            toggle = ui.small_button(action).clicked();
                        }
                        if !running {
                            ui.menu_button("⋯", |ui| {
                                if key.0 != Mode::Join && ui.button(t("编辑")).clicked() {
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
                    if let Some(address) = &address {
                        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                        let remaining = address.expires_at.saturating_sub(now);
                        if remaining > 0 {
                            ui.horizontal_wrapped(|ui| {
                                let label = if self.copied == Some(key) { t("已复制") } else { t("复制连接地址") };
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
                    ui.label(RichText::new(error).small().color(ERR()));
                }
            });
        if toggle {
            if running {
                self.active.remove(&key);
                if key.0 == Mode::Join {
                    self.forget(key, prefs);
                }
                self.tick();
            } else {
                self.report_start(key, map, prefs, busy_port, ui.ctx());
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
        selected
    }

    fn report_start(&mut self, key: Key, map: &Mapping, prefs: &Preferences, busy_port: Option<&str>, ctx: &egui::Context) {
        if map.service == Service::Com && busy_port == Some(map.serial.as_str()) {
            self.message = t("该串口正在使用，请先断开本机连接").into();
        } else if let Err(error) = self.start(key, map, prefs, None, ctx) {
            self.message = error;
        }
    }

    pub fn device_row(&mut self, ui: &mut egui::Ui, name: &str, prefs: &mut Preferences, busy_port: Option<&str>) -> bool {
        self.load();
        let matching: Vec<_> = self
            .rows(prefs)
            .filter(|(_, map)| map.service == Service::Com && map.label == name)
            .map(|(key, map)| (key, map.clone()))
            .collect();
        matching.into_iter().fold(false, |selected, (key, map)| self.row(ui, key, &map, prefs, busy_port) || selected)
    }

    pub fn list(&mut self, ui: &mut egui::Ui, prefs: &mut Preferences, ports: &[crate::serial::PortEntry], selected: &str, busy_port: Option<&str>) {
        self.load();
        let other: Vec<_> = self
            .rows(prefs)
            .filter(|(_, map)| map.service != Service::Com || !ports.iter().any(|p| p.name == map.label))
            .map(|(key, map)| (key, map.clone()))
            .collect();
        for (key, map) in other {
            self.row(ui, key, &map, prefs, busy_port);
        }
        if self.editor.is_none() {
            ui.horizontal_wrapped(|ui| {
                if ui.button(t("连接映射")).clicked() {
                    self.open_editor(Mode::Join, com_map(""));
                }
                if ui.small_button(t("共享服务")).clicked() {
                    self.open_editor(Mode::Share, Mapping::new(Service::Mqtt));
                }
            });
        }
        self.poll_driver(ui.ctx());
        if let Some(mut editor) = self.editor.take() {
            let (close, submit) = self.editor_ui(ui, &mut editor, ports, selected);
            let complete = submit
                && match self.submit_editor(&mut editor, prefs, busy_port, ui.ctx()) {
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
                let host = self.is_host(editor.mode);
                ui.horizontal(|ui| {
                    if editor.mode == Mode::Join {
                        ui.strong(t("连接映射"));
                    } else {
                        for service in [Service::Com, Service::Mqtt, Service::Ftp] {
                            let label = if service == Service::Com { t("串口") } else { service.title() };
                            if ui.selectable_label(service == editor.map.service, label).clicked() && service != editor.map.service {
                                editor.map = if service == Service::Com { com_map(selected) } else { Mapping::new(service) };
                                if !host {
                                    self.set_join_defaults(&mut editor.map);
                                }
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
                            .hint_text(t("粘贴连接地址"))
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
                            ui.colored_label(ERR(), t("连接地址无效或已过期"));
                        }
                        return;
                    }
                    ui.small(editor.map.service.title());
                }
                self.mapping_fields(ui, &mut editor.map, ports, host);
                if editor.mode != Mode::Join && (editor.mode == Mode::Legacy || editor.map.service == Service::Ftp) {
                    ui.collapsing(t("映射选项"), |ui| {
                        if editor.mode == Mode::Legacy {
                            ui.horizontal(|ui| {
                                ui.small(t("映射端口"));
                                ui.add(egui::DragValue::new(&mut editor.map.number).range(1..=65535));
                            });
                        }
                        if host && editor.map.service == Service::Ftp {
                            ui.small(t("FTP 服务的被动端口范围"));
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut editor.map.passive.0).range(1024..=65535));
                                ui.label("–");
                                ui.add(egui::DragValue::new(&mut editor.map.passive.1).range(1024..=65535));
                            });
                        }
                    });
                }
                let valid = editor.map.validate().is_ok();
                let action = if host { t("共享此端口") } else { t("连接设备端") };
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
            if self.pairs.is_empty()
                && ui
                    .add_enabled(self.create.is_none(), egui::Button::new(if self.create.is_some() { t("准备中…") } else { t("准备虚拟串口") }))
                    .clicked()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                self.create = Some(rx);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::vcom::install_com0com());
                    ctx.request_repaint();
                });
            }
        }
    }

    fn poll_driver(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.create else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.create = None;
                self.pairs = crate::vcom::pairs();
                if let Err(error) = result {
                    self.message = error;
                }
                if self.editor.as_ref().is_some_and(|editor| editor.map.service == Service::Com && !self.is_host(editor.mode)) {
                    if let Some(mut editor) = self.editor.take() {
                        self.pick_virtual(&mut editor.map);
                        self.editor = Some(editor);
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.create = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(200)),
        }
    }

    fn submit_editor(&mut self, editor: &mut Editor, prefs: &mut Preferences, busy_port: Option<&str>, ctx: &egui::Context) -> Result<(), String> {
        let mut map = editor.map.clone();
        if map.service == Service::Com && busy_port == Some(map.serial.as_str()) {
            return Err(t("该串口正在使用，请先断开本机连接").into());
        }
        let invitation = if editor.mode == Mode::Join {
            Some(ShareInvitation::parse(&editor.address).map_err(|_| t("连接地址无效或已过期"))?)
        } else {
            None
        };
        let key = match editor.mode {
            Mode::Share => {
                map.number = match editor.original {
                    Some(key) if key.1 == map.service => key.2,
                    _ => self.available_number(Mode::Share, map.service, prefs)?,
                };
                Mode::Share.key(&map)
            }
            Mode::Join => (Mode::Join, map.service, self.available_number(Mode::Join, map.service, prefs)?),
            Mode::Legacy => Mode::Legacy.key(&map),
        };
        if self.rows(prefs).any(|(id, _)| id == key && Some(id) != editor.original) {
            return Err(t("此映射端口已有配置，请编辑原映射或选择其他端口").into());
        }
        self.start(key, &map, prefs, invitation, ctx)?;
        if let Some(original) = editor.original {
            if original != key {
                self.forget(original, prefs);
            }
        }
        match editor.mode {
            Mode::Share => {
                prefs.shared.retain(|map| Mode::Share.key(map) != key);
                prefs.shared.push(map);
            }
            Mode::Legacy => {
                prefs.saved.retain(|map| Mode::Legacy.key(map) != key);
                prefs.saved.push(map);
            }
            Mode::Join => self.joined.push((key, map)),
        }
        Ok(())
    }
}

fn anyhow_message(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

fn client_hint(map: &Mapping) -> String {
    if map.service == Service::Com {
        tf("在烧录工具中选择 {}", &[&map.label])
    } else {
        tf("在 {} 客户端中连接 {}", &[&map.service.title(), &format!("127.0.0.1:{}", map.port)])
    }
}

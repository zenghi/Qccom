//! 工具箱「SSH / Telnet」：左边是保存的主机和快速连接，右边是会话标签和终端。
//! SSH 会话可在终端左侧打开远程文件，通过独立的 SFTP 或显式 SCP 操作传输。

use crate::cmds::{add_button, cmd_card, command_fields};
use crate::remote::{Host, Link, Proto, Session};
use std::collections::HashMap;

use super::sshfiles::FileBrowser;
use super::*;

const FILES_MIN_WIDTH: f32 = 172.0;
const FILES_DEFAULT_WIDTH: f32 = 320.0;
const TERMINAL_MIN_WIDTH: f32 = 210.0;

#[derive(Default)]
pub struct SshUi {
    pub tabs: Vec<Session>,
    pub active: usize,
    next: usize,
    /// 正在编辑的主机：(在列表里的位置, 内容)
    edit: Option<(Option<usize>, Host)>,
    quick: String,
    files: HashMap<usize, FileBrowser>,
    command_edit: Option<(Option<usize>, QuickCmd)>,
    command_window: bool,
    host_window: bool,
}

impl SshUi {
    fn files_visible(&self) -> bool {
        self.tabs.get(self.active).and_then(ssh_file_id).and_then(|id| self.files.get(&id)).is_some_and(|view| view.visible)
    }
}

impl App {
    /// 每帧读所有会话的输出（不在这个页面也要读）
    pub(super) fn ssh_poll(&mut self) {
        let ssh = &mut self.tb.ssh;
        for s in &mut ssh.tabs {
            s.poll();
        }
        ssh.files.retain(|id, view| {
            let present = ssh.tabs.iter().any(|session| ssh_file_id(session) == Some(*id));
            if present {
                view.poll(&self.ctx);
            }
            present
        });
    }

    pub(super) fn ssh_online(&self) -> bool {
        self.tb.ssh.tabs.iter().any(|s| s.alive())
    }

    pub(super) fn ssh_page(&mut self, ui: &mut egui::Ui, compact: bool) {
        let ctx = &ui.ctx().clone();
        let area = ui.available_rect_before_wrap();
        let files_visible = self.tb.ssh.files_visible();
        let hosts_docked = !compact && area.width() >= if files_visible { 900.0 } else { 500.0 };
        if hosts_docked {
            self.tb.ssh.host_window = false;
            egui::SidePanel::left(ui.id().with("ssh_hosts"))
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 14,
                    right: 12,
                    top: 16,
                    bottom: 10,
                }))
                .exact_width(250.0)
                .resizable(false)
                .show_separator_line(true)
                .show_inside(ui, |ui| self.ssh_hosts(ui));
        }
        let command_space = ui.available_width() - if files_visible { files_dock_width(ui.available_width()).unwrap_or(0.0) } else { 0.0 };
        let docked = command_space >= 480.0;
        if docked {
            self.tb.ssh.command_window = false;
            let max_width = (command_space - 270.0).min(420.0);
            egui::SidePanel::right(ui.id().with("ssh_commands"))
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 14,
                    bottom: 8,
                }))
                .default_width(max_width.min(272.0))
                .width_range(210.0..=max_width)
                .resizable(true)
                .show_animated_inside(ui, self.s.tb.ssh_commands_open, |ui| self.ssh_commands(ui, true));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 10,
                right: 12,
                top: 8,
                bottom: 10,
            }))
            .show_inside(ui, |ui| self.ssh_sessions(ui, docked, hosts_docked));
        if self.tb.ssh.host_window {
            let mut open = true;
            egui::Window::new("SSH / Telnet")
                .id(ui.id().with("ssh_host_window"))
                .open(&mut open)
                .collapsible(false)
                .anchor(Align2::LEFT_TOP, vec2(8.0, 36.0))
                .constrain_to(area)
                .default_width(232.0)
                .max_width((area.width() - 24.0).max(140.0))
                .show(ctx, |ui| {
                    ui.set_max_height((area.height() - 112.0).max(140.0));
                    self.ssh_hosts(ui);
                });
            self.tb.ssh.host_window = open;
        }
        if !docked && self.tb.ssh.command_window {
            let mut open = true;
            let mut sent = false;
            egui::Window::new(t("快捷命令"))
                .id(ui.id().with("ssh_command_window"))
                .open(&mut open)
                .collapsible(false)
                .anchor(Align2::RIGHT_TOP, vec2(-8.0, 36.0))
                .constrain_to(area)
                .default_width(252.0)
                .max_width((area.width() - 24.0).max(140.0))
                .show(ctx, |ui| {
                    ui.set_max_height((area.height() - 112.0).max(80.0));
                    sent = self.ssh_commands(ui, false);
                });
            self.tb.ssh.command_window = open && !sent;
        }
        self.ssh_editor(ctx);
    }

    fn ssh_commands(&mut self, ui: &mut egui::Ui, docked: bool) -> bool {
        if docked {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("快捷命令")).font(FontId::new(15.0, bold())).color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, t("收起"), false).clicked() {
                        self.s.tb.ssh_commands_open = false;
                    }
                });
            });
        }
        let active = self.tb.ssh.tabs.get(self.tb.ssh.active);
        let status = match active.filter(|session| session.host.proto == Proto::Ssh && session.alive()) {
            Some(session) if ssh_command_ready(session) => None,
            Some(_) => Some(t("等待 SSH 命令提示符")),
            None => Some(t("先连接一个 SSH 主机")),
        };
        let ready = status.is_none();
        if let Some(status) = status {
            ui.label(RichText::new(status).small().color(TEXT3()));
        } else if let Some(session) = active {
            ui.add(egui::Label::new(RichText::new(session.host.addr()).small().monospace().color(TEXT2())).truncate());
        }
        ui.add_space(8.0);

        let commands = &mut self.s.tb.ssh_commands;
        let edit = &mut self.tb.ssh.command_edit;
        let mut send = None;
        let mut remove = None;
        let mut swap = None;
        if edit.is_some() {
            ssh_command_editor(ui, commands, edit);
            ui.add_space(6.0);
        }
        egui::ScrollArea::vertical().id_salt("ssh_command_list").auto_shrink([false, !docked]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for (index, command) in commands.iter().enumerate() {
                let row = cmd_card(ui, command, None, ready, status.unwrap_or_default());
                if ready && row.clicked() {
                    send = Some(index);
                }
                row.context_menu(|ui| {
                    if ui.button(t("编辑")).clicked() {
                        *edit = Some((Some(index), command.clone()));
                        ui.close_menu();
                    }
                    if index > 0 && ui.button(t("上移")).clicked() {
                        swap = Some((index, index - 1));
                        ui.close_menu();
                    }
                    if index + 1 < commands.len() && ui.button(t("下移")).clicked() {
                        swap = Some((index, index + 1));
                        ui.close_menu();
                    }
                    if ui.button(RichText::new(t("删除")).color(ERR())).clicked() {
                        remove = Some(index);
                        ui.close_menu();
                    }
                });
            }
            if edit.is_none() && add_button(ui).clicked() {
                *edit = Some((None, QuickCmd::default()));
            }
        });
        if let Some(index) = remove {
            commands.remove(index);
            *edit = None;
        }
        if let Some((first, second)) = swap {
            commands.swap(first, second);
            *edit = None;
        }
        if let Some(bytes) = send.and_then(|index| commands.get(index)).and_then(ssh_command_bytes) {
            if let Some(session) = self.tb.ssh.tabs.get_mut(self.tb.ssh.active).filter(|session| ssh_command_ready(session)) {
                session.write(&bytes);
                session.tv().focus_req = true;
                return true;
            }
        }
        false
    }

    pub(super) fn ssh_connect(&mut self, host: &Host) {
        let ui = &mut self.tb.ssh;
        ui.next += 1;
        match Session::open(&self.ctx, host, ui.next) {
            Ok(s) => {
                ui.tabs.push(s);
                ui.active = ui.tabs.len() - 1;
            }
            Err(e) => self.toast(e, ERR()),
        }
    }

    fn ssh_hosts(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("SSH / Telnet").font(FontId::new(15.0, bold())).color(TEXT()));
        ui.add_space(10.0);
        let te = ui.add(
            egui::TextEdit::singleline(&mut self.tb.ssh.quick)
                .hint_text(t("user@host:端口，回车连接"))
                .desired_width(f32::INFINITY)
                .font(MONO_13())
                .margin(vec2(8.0, 5.0)),
        );
        let enter = te.lost_focus() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        te.on_hover_text(t("快速连接：pi@192.168.1.5、root@10.0.0.2:2222、telnet://192.168.1.1"));
        if enter {
            match Host::parse(&self.tb.ssh.quick) {
                Some(h) => {
                    // 新地址顺手存进列表，下次点一下就能连
                    let same = |x: &Host| x.proto == h.proto && x.host == h.host && x.port == h.port && x.user == h.user;
                    if !self.s.tb.ssh_hosts.iter().any(same) {
                        self.s.tb.ssh_hosts.push(h.clone());
                    }
                    let h = self.s.tb.ssh_hosts.iter().find(|x| same(x)).cloned().unwrap_or(h);
                    self.tb.ssh.quick.clear();
                    self.ssh_connect(&h);
                }
                None => self.toast(t("地址格式：user@host:端口"), WARN()),
            }
        }
        ui.add_space(8.0);
        if ui
            .add_sized(vec2(ui.available_width(), 28.0), egui::Button::new(RichText::new(format!("+  {}", t("新建主机"))).size(12.5)))
            .clicked()
        {
            self.tb.ssh.edit = Some((None, Host::default()));
        }
        ui.add_space(12.0);
        eyebrow(ui, t("保存的主机"), "");
        let mut connect: Option<Host> = None;
        let mut del = None;
        let mut dup = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(ui.available_height() - 54.0).show(ui, |ui| {
            if self.s.tb.ssh_hosts.is_empty() {
                ui.label(RichText::new(t("还没有保存的主机")).color(TEXT3()).size(12.0));
            }
            for (i, h) in self.s.tb.ssh_hosts.iter().enumerate() {
                let alive = self.tb.ssh.tabs.iter().filter(|s| s.host == *h && s.alive()).count();
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                let p = ui.painter();
                p.rect(
                    r,
                    R_WIDGET,
                    if resp.hovered() { BG3() } else { BG2() },
                    st(1.0, if alive > 0 { OK().gamma_multiply(0.45) } else { LINE() }),
                    egui::StrokeKind::Inside,
                );
                p.text(r.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, h.title(), FontId::new(13.0, bold()), TEXT());
                // 没起名字时标题就是地址，第二行改成写全的 用户@主机:端口
                let sub = if h.name.trim().is_empty() {
                    let u = if h.proto == Proto::Ssh && !h.user.is_empty() { format!("{}@", h.user) } else { String::new() };
                    format!("{u}{}:{}", h.host, h.port)
                } else {
                    h.addr()
                };
                p.text(r.left_bottom() + vec2(10.0, -8.0), Align2::LEFT_BOTTOM, sub, FontId::new(11.0, FontFamily::Monospace), TEXT3());
                let tag = h.proto.name();
                let tag_c = if h.proto == Proto::Ssh { ACCENT() } else { VIOLET() };
                let tw = ui.fonts(|f| f.layout_no_wrap(tag.into(), FontId::new(10.0, mono_bold()), TEXT()).size().x);
                let tr = Rect::from_min_size(pos2(r.right() - tw - 18.0, r.top() + 7.0), vec2(tw + 10.0, 16.0));
                p.rect_filled(tr, 4.0, tag_c.gamma_multiply(0.14));
                p.text(tr.center(), Align2::CENTER_CENTER, tag, FontId::new(10.0, mono_bold()), tag_c);
                if alive > 0 {
                    let c = pos2(r.right() - 12.0, r.bottom() - 13.0);
                    p.circle_filled(c, 3.0, OK());
                    if alive > 1 {
                        p.text(c - vec2(8.0, 0.0), Align2::RIGHT_CENTER, format!("×{alive}"), FontId::new(10.5, FontFamily::Monospace), TEXT3());
                    }
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let resp = resp.on_hover_text(t("单击连接；已经连着就切过去。右键编辑"));
                if resp.clicked() {
                    match self.tb.ssh.tabs.iter().position(|s| s.host == *h && s.alive()) {
                        Some(k) if !ui.input(|i| i.modifiers.command) => {
                            self.tb.ssh.active = k;
                            self.tb.ssh.tabs[k].tv().focus_req = true;
                        }
                        _ => connect = Some(h.clone()),
                    }
                }
                resp.context_menu(|ui| {
                    if ui.button(t("新开一个会话")).clicked() {
                        connect = Some(h.clone());
                        ui.close_menu();
                    }
                    if ui.button(t("编辑…")).clicked() {
                        self.tb.ssh.edit = Some((Some(i), h.clone()));
                        ui.close_menu();
                    }
                    if ui.button(t("复制一份")).clicked() {
                        dup = Some(i);
                        ui.close_menu();
                    }
                    if ui.button(t("复制地址")).clicked() {
                        ui.ctx().copy_text(h.addr());
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(RichText::new(t("删除")).color(ERR())).clicked() {
                        del = Some(i);
                        ui.close_menu();
                    }
                });
                ui.add_space(6.0);
            }
        });
        if let Some(i) = dup {
            let mut h = self.s.tb.ssh_hosts[i].clone();
            h.name = format!("{} (2)", h.title());
            self.s.tb.ssh_hosts.insert(i + 1, h);
        }
        if let Some(i) = del {
            self.s.tb.ssh_hosts.remove(i);
        }
        if let Some(h) = connect {
            self.ssh_connect(&h);
        }
    }

    fn ssh_sessions(&mut self, ui: &mut egui::Ui, commands_docked: bool, hosts_docked: bool) {
        let su = &mut self.tb.ssh;
        let commands_open = if commands_docked { &mut self.s.tb.ssh_commands_open } else { &mut su.command_window };
        if su.tabs.is_empty() {
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Commands, t("快捷命令面板"), *commands_open).clicked() {
                    *commands_open = !*commands_open;
                }
                if !hosts_docked && icon_button(ui, Icon::Sidebar, t("保存的主机"), su.host_window).clicked() {
                    su.host_window = !su.host_window;
                }
            });
            let r = ui.available_rect_before_wrap();
            let p = ui.painter();
            p.rect(r, R_CARD, BG0(), st(1.0, LINE()), egui::StrokeKind::Inside);
            draw_icon(p, Rect::from_center_size(r.center() - vec2(0.0, 44.0), vec2(34.0, 34.0)), Icon::Terminal, TEXT3());
            p.text(r.center(), Align2::CENTER_CENTER, t("还没有打开的会话"), FontId::new(15.0, bold()), TEXT2());
            if hosts_docked {
                p.text(
                    r.center() + vec2(0.0, 26.0),
                    Align2::CENTER_CENTER,
                    t("点左边保存的主机，或在上面输入 user@host 回车"),
                    FontId::proportional(12.5),
                    TEXT3(),
                );
            } else if ui
                .put(Rect::from_center_size(r.center() + vec2(0.0, 32.0), vec2(140.0, 26.0)), egui::Button::new(t("保存的主机")))
                .clicked()
            {
                su.host_window = true;
            }
            return;
        }
        su.active = su.active.min(su.tabs.len() - 1);
        let mut close = None;
        let mut reconnect = false;
        let mut toggle_files = false;
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            let file_id = ssh_file_id(&su.tabs[su.active]);
            let button_count = 2 + usize::from(file_id.is_some()) + usize::from(!hosts_docked);
            let tools_width = button_count as f32 * 32.0;
            let tabs_width = (ui.available_width() - tools_width - 4.0).max(0.0);
            egui::ScrollArea::horizontal()
                .id_salt("ssh_session_tabs")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .max_width(tabs_width)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (i, s) in su.tabs.iter().enumerate() {
                            let on = i == su.active;
                            let name = s.host.title();
                            let font = FontId::proportional(12.0);
                            let w = ui.fonts(|f| f.layout_no_wrap(name.clone(), font.clone(), TEXT()).size().x) + 50.0;
                            let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                            let p = ui.painter();
                            if on {
                                p.rect_filled(r, R_WIDGET, BG2());
                                p.hline(r.x_range().shrink(6.0), r.bottom() - 0.5, st(1.5, ACCENT()));
                            } else if resp.hovered() {
                                p.rect_filled(r, R_WIDGET, tint(6));
                            }
                            let dot = if !s.alive() {
                                TEXT3()
                            } else if s.rx_at.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
                                ACCENT()
                            } else {
                                OK()
                            };
                            p.circle_filled(pos2(r.left() + 12.0, r.center().y), 3.0, dot);
                            p.text(pos2(r.left() + 22.0, r.center().y), Align2::LEFT_CENTER, &name, font, if on { TEXT() } else { TEXT3() });
                            let xr = Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(14.0, 14.0));
                            let hover_x = resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| xr.contains(p));
                            if on || resp.hovered() {
                                draw_icon(p, xr.shrink(2.0), Icon::Close, if hover_x { TEXT() } else { TEXT3() });
                            }
                            if resp.clicked() {
                                if hover_x {
                                    close = Some(i);
                                } else {
                                    su.active = i;
                                }
                            }
                            if resp.middle_clicked() {
                                close = Some(i);
                            }
                        }
                    });
                });
            let cur = &su.tabs[su.active];
            let file_id = ssh_file_id(cur);
            let files = file_id.and_then(|id| su.files.get(&id));
            let viewing = files.is_some_and(|view| view.visible);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Commands, t("快捷命令面板"), *commands_open).clicked() {
                    *commands_open = !*commands_open;
                }
                if !hosts_docked && icon_button(ui, Icon::Sidebar, t("保存的主机"), su.host_window).clicked() {
                    su.host_window = !su.host_window;
                }
                if icon_button(ui, Icon::Refresh, t("重新连接"), false).clicked() {
                    reconnect = true;
                }
                if file_id.is_some() {
                    let label = if !viewing && files.is_some_and(FileBrowser::busy) { t("传输中") } else { t("远程文件") };
                    toggle_files = icon_button(ui, Icon::Folder, label, viewing || files.is_some_and(FileBrowser::busy)).clicked();
                }
            });
        });
        ui.add_space(4.0);
        let rect = ui.available_rect_before_wrap();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let session = &mut su.tabs[su.active];
        if let Some(id) = ssh_file_id(session) {
            if toggle_files {
                let view = su.files.entry(id).or_default();
                view.toggle(&session.host, ui.ctx());
                if !view.visible {
                    session.tv().focus_req = true;
                }
            }
            if let Some(view) = su.files.get_mut(&id).filter(|view| view.visible) {
                ssh_files_pane(&mut child, id, view, &session.host);
            }
        }
        let toast = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(&mut child, |ui| ssh_terminal(session, ui, &mut reconnect))
            .inner;
        if let Some(i) = close {
            if let Some(id) = ssh_file_id(&su.tabs.remove(i)) {
                su.files.remove(&id);
            }
            if su.active >= su.tabs.len() {
                su.active = su.tabs.len().saturating_sub(1);
            }
        } else if reconnect {
            let k = su.active;
            let host = su.tabs[k].host.clone();
            su.next += 1;
            match Session::open(&self.ctx, &host, su.next) {
                Ok(session) => {
                    if let Some(id) = ssh_file_id(&su.tabs[k]) {
                        su.files.remove(&id);
                    }
                    su.tabs[k] = session;
                }
                Err(error) => self.toast(error, ERR()),
            }
        }
        if let Some(message) = toast {
            self.toast(message, OK());
        }
    }

    fn ssh_editor(&mut self, ctx: &egui::Context) {
        let Some((idx, mut h)) = self.tb.ssh.edit.take() else { return };
        let mut open = true;
        let mut save = false;
        let mut save_go = false;
        egui::Window::new(if idx.is_some() { t("编辑主机") } else { t("新建主机") })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, -40.0))
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(78.0, 26.0), Sense::hover());
                        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), TEXT2());
                        add(ui);
                    });
                };
                row(ui, t("协议"), &mut |ui| {
                    for p in [Proto::Ssh, Proto::Telnet] {
                        let mut on = h.proto == p;
                        if toggle_chip(ui, &mut on, p.name(), "") {
                            if h.port == h.proto.default_port() {
                                h.port = p.default_port();
                            }
                            h.proto = p;
                        }
                    }
                });
                row(ui, t("名称"), &mut |ui| text_field(ui, &mut h.name, t("可选，例如「网关」")));
                row(ui, t("主机"), &mut |ui| {
                    ui.add_sized(
                        vec2(ui.available_width() - 90.0, 26.0),
                        egui::TextEdit::singleline(&mut h.host).hint_text("192.168.1.10").font(MONO_13()).margin(vec2(6.0, 4.0)),
                    );
                    ui.label(RichText::new(":").color(TEXT3()));
                    ui.allocate_ui(vec2(70.0, 26.0), |ui| num_field(ui, "ssh_port", &mut h.port));
                });
                if h.proto == Proto::Ssh {
                    row(ui, t("用户名"), &mut |ui| text_field(ui, &mut h.user, "root"));
                    row(ui, t("私钥"), &mut |ui| {
                        ui.add_sized(
                            vec2(ui.available_width() - 74.0, 26.0),
                            egui::TextEdit::singleline(&mut h.key).hint_text(t("默认 ~/.ssh/id_*")).font(MONO_13()).margin(vec2(6.0, 4.0)),
                        );
                        if ui.add(egui::Button::new(t("选择…")).min_size(vec2(62.0, 26.0))).clicked() {
                            if let Some(p) = crate::diagnostics::file_dialog().pick_file() {
                                h.key = p.to_string_lossy().into_owned();
                            }
                        }
                    });
                    row(ui, t("远程目录"), &mut |ui| text_field(ui, &mut h.dir, "~"));
                    row(ui, t("额外参数"), &mut |ui| text_field(ui, &mut h.args, "-J user@jump"));
                }
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(78.0, 26.0), Sense::hover());
                    ui.painter().text(r.left_center(), Align2::LEFT_CENTER, t("登录后执行"), FontId::proportional(12.5), TEXT2());
                    ui.add(
                        egui::TextEdit::multiline(&mut h.init)
                            .hint_text(t("每行一条命令，可选"))
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .font(MONO_13()),
                    );
                });
                if h.proto == Proto::Ssh {
                    ui.label(RichText::new(t("SSH 密码在终端或文件连接提示中输入，QCCOM 不保存。")).size(11.5).color(TEXT3()));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let ok = !h.host.trim().is_empty();
                    ui.add_enabled_ui(ok, |ui| {
                        if primary_button(ui, t("保存并连接")).clicked() {
                            save_go = true;
                        }
                        if ui.add(egui::Button::new(t("保存")).min_size(vec2(80.0, 30.0))).clicked() {
                            save = true;
                        }
                    });
                });
            });
        if save || save_go {
            h.host = h.host.trim().to_string();
            match idx {
                Some(i) if i < self.s.tb.ssh_hosts.len() => self.s.tb.ssh_hosts[i] = h.clone(),
                _ => self.s.tb.ssh_hosts.push(h.clone()),
            }
            if save_go {
                self.ssh_connect(&h);
            }
        } else if open {
            self.tb.ssh.edit = Some((idx, h));
        }
    }

    pub(super) fn ssh_drop(&mut self, files: Vec<PathBuf>) {
        let ssh = &mut self.tb.ssh;
        let view = ssh.tabs.get(ssh.active).and_then(ssh_file_id).and_then(|id| ssh.files.get_mut(&id));
        if let Some(view) = view.filter(|view| view.visible && drop_pointer(&self.ctx).is_some_and(|pos| view.rect.is_some_and(|rect| rect.contains(pos)))) {
            view.stage_upload(files);
        } else {
            self.toast(t("将文件拖到远程文件列表以准备上传"), WARN());
        }
    }
}

fn files_dock_width(available: f32) -> Option<f32> {
    let max = available - TERMINAL_MIN_WIDTH;
    let preferred = (available * 0.46).clamp(FILES_MIN_WIDTH, FILES_DEFAULT_WIDTH);
    (max >= FILES_MIN_WIDTH).then_some(preferred.min(max))
}

fn ssh_files_pane(ui: &mut egui::Ui, id: usize, view: &mut FileBrowser, host: &Host) {
    let area = ui.available_rect_before_wrap();
    let salt = ui.id().with(("ssh_files", id));
    let frame = egui::Frame::new().fill(BG0()).inner_margin(egui::Margin::symmetric(10, 8));
    if let Some(width) = files_dock_width(area.width()) {
        egui::SidePanel::left(salt)
            .default_width(width)
            .width_range(FILES_MIN_WIDTH..=area.width() - TERMINAL_MIN_WIDTH)
            .resizable(true)
            .show_separator_line(true)
            .frame(frame)
            .show_inside(ui, |ui| view.show(ui, host, false));
    } else {
        let height = (area.height() * 0.53).min(area.height() - 110.0).max(82.0);
        egui::TopBottomPanel::top(salt)
            .exact_height(height)
            .show_separator_line(true)
            .frame(frame)
            .show_inside(ui, |ui| view.show(ui, host, true));
    }
}

fn ssh_file_id(session: &Session) -> Option<usize> {
    if let Link::Ssh(shell) = &session.link {
        Some(shell.id)
    } else {
        None
    }
}

fn ssh_terminal(session: &mut Session, ui: &mut egui::Ui, reconnect: &mut bool) -> Option<&'static str> {
    let out = session.tv().show(ui, &|text: &str| text.as_bytes().to_vec(), None, true, None);
    session.sync_size();
    if !session.alive() {
        *reconnect |= out.input.contains(&b'\r');
    } else if !out.input.is_empty() {
        session.write(&out.input);
    }
    out.toast
}

fn ssh_command_editor(ui: &mut egui::Ui, commands: &mut Vec<QuickCmd>, edit: &mut Option<(Option<usize>, QuickCmd)>) {
    let Some((index, draft)) = edit.as_mut() else { return };
    let mut save = false;
    let mut cancel = false;
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, ACCENT().gamma_multiply(0.5)))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.label(RichText::new(if index.is_some() { t("编辑命令") } else { t("新命令") }).font(FontId::new(12.0, bold())).color(ACCENT()));
            command_fields(ui, draft, t("要发送的内容"));
            let valid = is_shell_command(&draft.payload);
            if !valid && !draft.payload.trim().is_empty() {
                ui.label(RichText::new(t("命令需为单行文字")).small().color(ERR()));
            }
            ui.horizontal(|ui| {
                let button = egui::Button::new(RichText::new(t("保存")).font(FontId::new(12.5, bold())).color(ON_ACCENT())).fill(ACCENT());
                save = ui.add_enabled(valid, button).clicked();
                cancel = ui.button(t("取消")).clicked();
            });
        });
    if save {
        if let Some((index, mut draft)) = edit.take() {
            if draft.name.trim().is_empty() {
                draft.name = draft.payload.chars().take(24).collect();
            }
            match index.and_then(|index| commands.get_mut(index)) {
                Some(command) => *command = draft,
                None => commands.push(draft),
            }
        }
    } else if cancel {
        *edit = None;
    }
}

fn ssh_command_ready(session: &Session) -> bool {
    let Link::Ssh(shell) = &session.link else { return false };
    // OpenSSH's PTY has no separate authentication event. Require a shell prompt before sending a shortcut.
    session.alive() && session.up && shell.tv.fed_at.is_some() && !shell.tv.term.s.in_alt() && shell_prompt_ready(&shell.tv.cursor_line())
}

fn shell_prompt_ready(line: &str) -> bool {
    let line = line.trim_end();
    line != ">" && line.ends_with(['$', '#', '%', '>', '❯', '➜', '»', '❱', 'λ'])
}

fn is_shell_command(payload: &str) -> bool {
    !payload.trim().is_empty() && !payload.chars().any(char::is_control)
}

fn ssh_command_bytes(command: &QuickCmd) -> Option<Vec<u8>> {
    if !is_shell_command(&command.payload) {
        return None;
    }
    let mut bytes = command.payload.as_bytes().to_vec();
    bytes.push(b'\r');
    Some(bytes)
}

#[allow(non_snake_case)]
fn MONO_13() -> FontId {
    FontId::new(13.0, FontFamily::Monospace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_waits_for_an_idle_shell_prompt() {
        for prompt in ["user@board:~$ ", "root@board:~#", "PS C:\\Users>", "~/project ❯"] {
            assert!(shell_prompt_ready(prompt));
        }
        for line in [
            "",
            "> ",
            "board's password:",
            "Enter passphrase for key:",
            "Are you sure you want to continue connecting (yes/no)?",
            "root@board:~# ls",
        ] {
            assert!(!shell_prompt_ready(line));
        }
    }

    #[test]
    fn shortcut_sends_one_utf8_line_and_terminal_enter() {
        let command = |payload: &str| QuickCmd {
            payload: payload.into(),
            ..QuickCmd::default()
        };
        assert_eq!(ssh_command_bytes(&command("echo '串口'")), Some("echo '串口'\r".as_bytes().to_vec()));
        for payload in ["  ", "first\nsecond", "text\r", "text\u{1b}"] {
            assert_eq!(ssh_command_bytes(&command(payload)), None);
        }
    }
}

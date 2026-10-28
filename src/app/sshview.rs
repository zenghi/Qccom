//! 工具箱「SSH / Telnet」：左边是保存的主机和快速连接，右边是会话标签和终端。
//! 多个会话同时在线；拖文件到 SSH 终端上传（scp），头部按钮也能上传 / 下载。

use crate::remote::{self, Host, Proto, Session, Xfer};

use super::*;

#[derive(Default)]
pub struct SshUi {
    pub tabs: Vec<Session>,
    pub active: usize,
    next: usize,
    /// 正在编辑的主机：(在列表里的位置, 内容)
    edit: Option<(Option<usize>, Host)>,
    quick: String,
    xfers: Vec<Xfer>,
    dl_path: String,
    /// 终端区域（拖文件时判断落在哪）
    term_rect: Option<Rect>,
}

impl App {
    /// 每帧读所有会话的输出（不在这个页面也要读）
    pub(super) fn ssh_poll(&mut self) {
        for s in &mut self.tb.ssh.tabs {
            s.poll();
        }
        let mut done = vec![];
        self.tb.ssh.xfers.retain(|x| match x.rx.try_recv() {
            Ok(r) => {
                done.push((x.what.clone(), r));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for (what, r) in done {
            match r {
                Ok(()) => self.toast(tf("传输完成：{}", &[&what]), OK()),
                Err(e) => self.toast(tf("传输失败：{}", &[&e]), ERR()),
            }
        }
    }

    pub(super) fn ssh_online(&self) -> bool {
        self.tb.ssh.tabs.iter().any(|s| s.alive())
    }

    pub(super) fn ssh_page(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("ssh_hosts")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 14,
                right: 12,
                top: 16,
                bottom: 10,
            }))
            .exact_width(250.0)
            .resizable(false)
            .show_separator_line(true)
            .show(ctx, |ui| self.ssh_hosts(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 10,
                right: 12,
                top: 8,
                bottom: 10,
            }))
            .show(ctx, |ui| self.ssh_sessions(ui));
        self.ssh_editor(ctx);
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
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
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
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(
                RichText::new(t("SSH 用系统自带的 OpenSSH：密钥、ssh-agent、~/.ssh/config 都能用；把文件拖到终端上就是上传"))
                    .size(11.0)
                    .color(TEXT3()),
            );
        });
    }

    fn ssh_sessions(&mut self, ui: &mut egui::Ui) {
        let su = &mut self.tb.ssh;
        if su.tabs.is_empty() {
            su.term_rect = None;
            let r = ui.max_rect();
            let p = ui.painter();
            p.rect(r, R_CARD, BG0(), st(1.0, LINE()), egui::StrokeKind::Inside);
            draw_icon(p, Rect::from_center_size(r.center() - vec2(0.0, 44.0), vec2(34.0, 34.0)), Icon::Terminal, TEXT3());
            p.text(r.center(), Align2::CENTER_CENTER, t("还没有打开的会话"), FontId::new(15.0, bold()), TEXT2());
            p.text(
                r.center() + vec2(0.0, 26.0),
                Align2::CENTER_CENTER,
                t("点左边保存的主机，或在上面输入 user@host 回车"),
                FontId::proportional(12.5),
                TEXT3(),
            );
            return;
        }
        su.active = su.active.min(su.tabs.len() - 1);
        let mut close = None;
        let mut reconnect = false;
        let mut upload = false;
        let mut download: Option<String> = None;
        ui.horizontal(|ui| {
            ui.set_height(32.0);
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
            let cur = &su.tabs[su.active];
            let is_ssh = cur.host.proto == Proto::Ssh;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Refresh, t("重新连接"), false).clicked() {
                    reconnect = true;
                }
                if is_ssh {
                    let dr = icon_button(ui, Icon::Download, t("下载文件…"), false);
                    let pop = ui.id().with("ssh-dl");
                    if dr.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(pop));
                    }
                    egui::popup::popup_below_widget(ui, pop, &dr, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                        ui.set_min_width(300.0);
                        ui.label(RichText::new(t("远程文件或文件夹路径")).size(12.0).color(TEXT2()));
                        let te = ui.add(egui::TextEdit::singleline(&mut su.dl_path).hint_text("/var/log/messages").desired_width(f32::INFINITY).font(MONO_13()));
                        let go = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.add_space(4.0);
                        if (ui.button(t("下载到…")).clicked() || go) && !su.dl_path.trim().is_empty() {
                            download = Some(su.dl_path.clone());
                            ui.memory_mut(|m| m.close_popup());
                        }
                    });
                    if icon_button(ui, Icon::Folder, t("上传文件…（也可以直接把文件拖到终端上）"), false).clicked() {
                        upload = true;
                    }
                }
                if !su.xfers.is_empty() {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT()));
                    ui.label(RichText::new(tf("{} 个传输进行中", &[&su.xfers.len()])).size(11.5).color(TEXT3()));
                }
            });
        });
        ui.add_space(4.0);
        let rect = ui.available_rect_before_wrap();
        su.term_rect = Some(rect);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let s = &mut su.tabs[su.active];
        let o = s.tv().show(&mut child, &|x: &str| x.as_bytes().to_vec(), None, true);
        s.sync_size();
        if !s.alive() {
            if o.input.contains(&b'\r') {
                reconnect = true;
            }
        } else if !o.input.is_empty() {
            s.write(&o.input);
        }
        if let Some(m) = o.toast {
            self.toast(m, OK());
        }
        if let Some(i) = close {
            self.tb.ssh.tabs.remove(i);
            if self.tb.ssh.active >= self.tb.ssh.tabs.len() {
                self.tb.ssh.active = self.tb.ssh.tabs.len().saturating_sub(1);
            }
            return;
        }
        if reconnect {
            let k = self.tb.ssh.active;
            let host = self.tb.ssh.tabs[k].host.clone();
            self.tb.ssh.next += 1;
            match Session::open(&self.ctx, &host, self.tb.ssh.next) {
                Ok(s) => self.tb.ssh.tabs[k] = s,
                Err(e) => self.toast(e, ERR()),
            }
        }
        if upload {
            if let Some(files) = rfd::FileDialog::new().pick_files() {
                self.ssh_upload(files);
            }
        }
        if let Some(remote) = download {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let host = self.tb.ssh.tabs[self.tb.ssh.active].host.clone();
                let x = remote::download(&self.ctx, &host, &remote, &dir);
                self.tb.ssh.xfers.push(x);
            }
        }
    }

    /// 上传到当前 SSH 会话的主机
    pub(super) fn ssh_upload(&mut self, files: Vec<PathBuf>) {
        let Some(s) = self.tb.ssh.tabs.get(self.tb.ssh.active) else {
            self.toast(t("先连接一个 SSH 主机"), WARN());
            return;
        };
        if s.host.proto != Proto::Ssh {
            self.toast(t("Telnet 不能传文件，请用 SSH"), WARN());
            return;
        }
        let host = s.host.clone();
        let x = remote::upload(&self.ctx, &host, &files, &host.dir);
        self.toast(tf("正在上传：{}", &[&x.what]), ACCENT());
        self.tb.ssh.xfers.push(x);
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
                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                h.key = p.to_string_lossy().into_owned();
                            }
                        }
                    });
                    row(ui, t("上传目录"), &mut |ui| text_field(ui, &mut h.dir, "~"));
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
                    ui.label(RichText::new(t("密码在终端里输入，QCCOM 不保存密码；用密钥登录可以免输密码，也能拖文件上传")).size(11.5).color(TEXT3()));
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

    /// 拖文件到工具箱的 SSH 页：落在终端上就上传
    pub(super) fn ssh_drop(&mut self, files: Vec<PathBuf>) {
        if self.tb.ssh.tabs.is_empty() {
            self.toast(t("先连接一个 SSH 主机"), WARN());
            return;
        }
        self.ssh_upload(files);
    }
}

#[allow(non_snake_case)]
fn MONO_13() -> FontId {
    FontId::new(13.0, FontFamily::Monospace)
}

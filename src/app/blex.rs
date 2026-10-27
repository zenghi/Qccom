//! 侧边栏「BLE」标签：扫描附近的低功耗蓝牙设备，连上后列出服务和特征（订阅通知、读取、选写入目标）。

use crate::ble::{self, Chr, HubCmd};

use std::sync::{Arc, Mutex};

use super::*;

impl App {
    fn ble_hub(&mut self) -> &ble::Hub {
        let ctx = self.ctx.clone();
        self.ble.get_or_insert_with(|| ble::Hub::start(ctx))
    }

    pub(super) fn ble_connect(&mut self) -> Option<Connection> {
        if self.s.net.ble.id.is_empty() {
            self.toast(t("先在列表里选一个设备"), WARN());
            return None;
        }
        let (gen, cfg, tx) = (self.conn_gen, self.s.net.ble.clone(), self.evt_tx.clone());
        Some(self.ble_hub().open(gen, &cfg, tx))
    }

    /// BLE 标签下的侧边栏：没连接时是设备列表，连上后是特征表
    pub(super) fn ble_sidebar(&mut self, ui: &mut egui::Ui) {
        self.ble_hub();
        let hub = self.ble.as_ref().unwrap();
        let st = hub.st.clone();
        let live = self.conn.is_some();
        if !live {
            self.ble_devices(ui, &st);
            ui.add_space(10.0);
        }
        let busy = st.lock().unwrap().busy.clone();
        match busy {
            Some(b) if self.conn.is_some() && self.connected.is_none() => {
                self.connect_button(ui);
                ui.add_space(4.0);
                ui.label(RichText::new(b).small().color(TEXT3()));
            }
            _ => self.connect_button(ui),
        }
        ui.add_space(16.0);
        eyebrow(ui, t("链路"), &self.uptime());
        self.link_card(ui);
        ui.add_space(16.0);
        if self.connected.is_some() {
            self.ble_chars(ui, &st);
            ui.add_space(16.0);
        }
    }

    fn ble_devices(&mut self, ui: &mut egui::Ui, shared: &Arc<Mutex<ble::Shared>>) {
        let (scanning, err, n) = {
            let s = shared.lock().unwrap();
            (s.scanning, s.err.clone(), s.devices.len())
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("附近设备")).font(FontId::new(11.0, bold())).color(TEXT3()).extra_letter_spacing(1.5));
            if n > 0 {
                ui.label(RichText::new(n.to_string()).font(FontId::new(10.5, FontFamily::Monospace)).color(TEXT3()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (label, col) = if scanning { (t("停止"), WARN()) } else { (t("扫描"), ACCENT()) };
                let b = egui::Button::new(RichText::new(label).size(12.0).color(col))
                    .fill(col.gamma_multiply(0.08))
                    .stroke(st(1.0, col.gamma_multiply(0.35)))
                    .min_size(vec2(52.0, 22.0));
                if ui.add_enabled(err.is_none() || !scanning, b).clicked() {
                    if !scanning {
                        self.ble_hub().send(HubCmd::Clear);
                    }
                    self.ble_hub().send(HubCmd::Scan(!scanning));
                }
                if scanning {
                    ui.add(egui::Spinner::new().size(12.0).color(ACCENT()));
                }
            });
        });
        ui.add_space(4.0);
        if let Some(e) = &err {
            card(ui, |ui| {
                ui.label(RichText::new(e).small().color(ERR()));
                ui.add_space(2.0);
                let tip = if cfg!(windows) {
                    t("请在 Windows 设置里打开蓝牙")
                } else if cfg!(target_os = "macos") {
                    t("请在系统设置里打开蓝牙，并允许 QCCOM 使用蓝牙")
                } else {
                    t("需要 BlueZ：确认 bluetooth 服务在运行（systemctl status bluetooth）")
                };
                ui.label(RichText::new(tip).small().color(TEXT3()));
            });
            ui.add_space(6.0);
        }
        // 过滤
        ui.horizontal(|ui| {
            let w = ui.available_width() - 30.0 - ui.spacing().item_spacing.x;
            ui.add_sized(vec2(w, 26.0), egui::TextEdit::singleline(&mut self.ble_filter).hint_text(t("按名字 / 地址过滤")));
            let mut named = self.s.net.ble.named_only;
            let r = icon_button(ui, Icon::Filter, t("只显示有名字的设备"), named);
            if r.clicked() {
                named = !named;
            }
            self.s.net.ble.named_only = named;
        });
        ui.add_space(6.0);

        let q = self.ble_filter.trim().to_lowercase();
        let named_only = self.s.net.ble.named_only;
        let mut pick = None;
        let mut connect = false;
        {
            let s = shared.lock().unwrap();
            let mut list: Vec<&ble::Dev> = s
                .devices
                .iter()
                .filter(|d| !named_only || !d.name.is_empty())
                .filter(|d| q.is_empty() || d.name.to_lowercase().contains(&q) || d.addr.to_lowercase().contains(&q))
                .collect();
            // 按信号格数排，同一格里保持发现顺序（列表不会一直跳）
            list.sort_by_key(|d| std::cmp::Reverse(ble::bars(d.rssi)));
            if list.is_empty() {
                let msg = if s.err.is_some() {
                    ""
                } else if !scanning && s.devices.is_empty() {
                    t("点「扫描」查找附近的蓝牙设备")
                } else if scanning && s.devices.is_empty() {
                    t("正在扫描…")
                } else {
                    t("没有符合条件的设备")
                };
                if !msg.is_empty() {
                    ui.label(RichText::new(msg).small().color(TEXT3()));
                }
            }
            for d in list.iter().take(80) {
                let selected = d.id == self.s.net.ble.id;
                let r = ble_card(ui, d, selected);
                if r.clicked() {
                    pick = Some((d.id.clone(), d.name.clone()));
                }
                if r.double_clicked() {
                    connect = true;
                }
            }
        }
        if let Some((id, name)) = pick {
            self.s.net.ble.id = id;
            self.s.net.ble.name = name;
        }
        if connect {
            self.connect();
        }
        if scanning {
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ble_chars(&mut self, ui: &mut egui::Ui, shared: &Arc<Mutex<ble::Shared>>) {
        let (chars, target, mtu) = {
            let s = shared.lock().unwrap();
            (s.chars.clone(), s.target, s.mtu)
        };
        eyebrow(ui, t("服务与特征"), &format!("MTU {mtu}"));
        let mut with_resp = self.s.net.ble.with_resp;
        if switch_row(ui, &mut with_resp, t("写入要求应答"), t("Write Request：设备确认收到才算成功，慢但可靠；关闭时用 Write Command")) {
            self.s.net.ble.with_resp = with_resp;
            self.ble_hub().send(HubCmd::WithResp(with_resp));
        }
        ui.add_space(6.0);
        let mut cmds = Vec::new();
        let mut svc = None;
        for c in &chars {
            if svc != Some(c.service) {
                svc = Some(c.service);
                ui.add_space(6.0);
                let name = ble::service_name(&c.service).map(str::to_string).unwrap_or_else(|| tf("服务 {}", &[&ble::uuid_short_str(&c.service)]));
                ui.label(RichText::new(name).font(FontId::new(11.5, bold())).color(TEXT2())).on_hover_text(c.service.to_string());
                ui.add_space(2.0);
            }
            char_row(ui, c, target == Some(c.uuid), &mut cmds);
        }
        for c in cmds {
            self.ble_hub().send(c);
        }
    }
}

/// 一行设备：名字、地址、信号格
fn ble_card(ui: &mut egui::Ui, d: &ble::Dev, selected: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 44.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    let fill = if selected {
        BG3()
    } else if hov {
        BG2()
    } else {
        BG1()
    };
    let stroke = if selected {
        LINE_HI()
    } else if hov {
        LINE()
    } else {
        Color32::TRANSPARENT
    };
    p.rect(rect, CornerRadius::same(7), fill, st(1.0, stroke), egui::StrokeKind::Inside);
    if selected {
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 10.0), vec2(2.5, rect.height() - 20.0)), 1.5, ACCENT());
    }
    // 30 秒没再看到的设备变暗
    let stale = d.seen.elapsed() > Duration::from_secs(30);
    let dim = |c: Color32| if stale { c.gamma_multiply(0.5) } else { c };
    let x = rect.left() + 12.0;
    let name = if d.name.is_empty() { t("（未命名）") } else { d.name.as_str() };
    p.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        truncate(name, 22),
        FontId::new(13.0, if d.name.is_empty() { FontFamily::Proportional } else { bold() }),
        dim(if d.name.is_empty() {
            TEXT3()
        } else if selected {
            TEXT()
        } else {
            TEXT2()
        }),
    );
    let mut sub = d.addr.clone();
    if let Some(n) = d.services.iter().find_map(ble::service_name) {
        sub = format!("{sub} · {n}");
    } else if let Some(c) = d.company.and_then(ble::company) {
        sub = format!("{sub} · {c}");
    }
    // 右边留给「-60 dBm」，按宽度截断
    let max = ((rect.width() - 24.0 - 52.0) / 6.4).max(8.0) as usize;
    p.text(
        pos2(x, rect.top() + 31.0),
        Align2::LEFT_CENTER,
        truncate(&sub, max),
        FontId::new(10.5, FontFamily::Monospace),
        dim(TEXT3()),
    );
    // 信号格
    let b = ble::bars(d.rssi);
    let col = match b {
        4 | 3 => OK(),
        2 => WARN(),
        _ => ERR(),
    };
    let base = pos2(rect.right() - 12.0 - 4.0 * 5.0, rect.top() + 21.0);
    for i in 0..4u8 {
        let h = 4.0 + i as f32 * 3.0;
        let r = Rect::from_min_size(pos2(base.x + i as f32 * 5.0, base.y - h), vec2(3.0, h));
        p.rect_filled(r, 1.0, dim(if i < b { col } else { LINE_HI() }));
    }
    if let Some(r) = d.rssi {
        p.text(
            pos2(rect.right() - 12.0, rect.top() + 31.0),
            Align2::RIGHT_CENTER,
            format!("{r} dBm"),
            FontId::new(10.0, FontFamily::Monospace),
            dim(TEXT3()),
        );
    }
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    resp.on_hover_text(format!("{}\n{}", d.id, t("双击连接")))
}

/// 一行特征：名字 / UUID、属性标记，右边是通知开关和读取按钮；点一下设为写入目标
fn char_row(ui: &mut egui::Ui, c: &Chr, target: bool, cmds: &mut Vec<HubCmd>) {
    let w = ui.available_width();
    let h = if c.last.is_some() { 46.0 } else { 32.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), if c.can_write() { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    let hov = resp.hovered() && c.can_write();
    p.rect(
        rect,
        CornerRadius::same(6),
        if target {
            ACCENT().gamma_multiply(0.08)
        } else if hov {
            BG2()
        } else {
            Color32::TRANSPARENT
        },
        st(1.0, if target { ACCENT().gamma_multiply(0.4) } else { Color32::TRANSPARENT }),
        egui::StrokeKind::Inside,
    );
    let x = rect.left() + 8.0;
    let name = ble::char_name(&c.uuid);
    p.text(
        pos2(x, rect.top() + 16.0),
        Align2::LEFT_CENTER,
        truncate(&name, 16),
        FontId::new(12.0, FontFamily::Monospace),
        if target { ACCENT() } else { TEXT() },
    );
    if let Some(v) = &c.last {
        let hex: String = v.iter().take(12).map(|b| format!("{b:02X} ")).collect();
        let txt = if v.len() > 12 { format!("{hex}…") } else { hex };
        p.text(pos2(x, rect.top() + 34.0), Align2::LEFT_CENTER, txt, FontId::new(10.5, FontFamily::Monospace), TEXT3());
    }
    // 右侧按钮
    let mut right = rect.right() - 6.0;
    let row = rect.top() + 16.0;
    if c.can_notify() {
        let r = Rect::from_min_size(pos2(right - 30.0, row - 8.5), vec2(30.0, 17.0));
        let mut on = c.on;
        let sr = ui.put(r, |ui: &mut egui::Ui| switch(ui, &mut on)).on_hover_text(t("订阅通知"));
        if sr.changed() || on != c.on {
            cmds.push(HubCmd::Notify(c.uuid, on));
        }
        right -= 38.0;
    }
    if c.can_read() {
        let bw = (text_w(ui, t("读"), FontId::proportional(11.5)) + 14.0).max(36.0);
        let r = Rect::from_min_size(pos2(right - bw, row - 10.0), vec2(bw, 20.0));
        let b = ui.put(r, egui::Button::new(RichText::new(t("读")).size(11.5)).wrap_mode(egui::TextWrapMode::Extend).min_size(vec2(bw, 20.0)));
        if b.on_hover_text(t("读取一次，结果显示在日志里")).clicked() {
            cmds.push(HubCmd::Read(c.uuid));
        }
        right -= bw + 8.0;
    }
    // 可写的特征：右边一个小标记（读 / 通知已经由按钮和开关表示）
    if c.can_write() {
        let label = if target { t("写入") } else { "W" };
        let font = FontId::new(9.5, mono_bold());
        let w = text_w(ui, label, font.clone()) + 8.0;
        let r = Rect::from_min_size(pos2(right - w, row - 8.0), vec2(w, 16.0));
        let col = VIOLET();
        let fill = if target { col.gamma_multiply(0.18) } else { col.gamma_multiply(0.08) };
        let p = ui.painter();
        p.rect(r, 3.0, fill, st(1.0, col.gamma_multiply(0.4)), egui::StrokeKind::Inside);
        p.text(r.center(), Align2::CENTER_CENTER, label, font, col);
    }
    if resp.clicked() && !target {
        cmds.push(HubCmd::Target(c.uuid));
    }
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let tip = if c.can_write() {
        format!("{}\n{}", c.uuid, if target { t("发送区的数据写到这里") } else { t("点击设为写入目标") })
    } else {
        c.uuid.to_string()
    };
    resp.on_hover_text(tip);
    ui.add_space(2.0);
}

fn text_w(ui: &egui::Ui, s: &str, font: FontId) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(s.to_string(), font, Color32::WHITE).size().x)
}

//! The circuit workspace chrome and the linked canvas/inspector workflows.
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, RichText, Stroke};

use super::{routing, BTab, CircuitView, CvEvt, Drag, Practice, RTab, Tool, SPEEDS};
use crate::app::{draw_icon, icon_button, Icon};
use crate::circuit::{check::Level, lib, Pt};
use crate::i18n::{t, tf};
use crate::theme::*;

const NARROW: f32 = 940.0;

pub(super) fn canvas_color() -> Color32 {
    if is_light() {
        Color32::from_rgb(246, 248, 252)
    } else {
        Color32::from_rgb(14, 21, 30)
    }
}

pub(super) fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, LINE()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui);
        });
}

pub(super) fn action(ui: &mut egui::Ui, label: &str, icon: Option<Icon>, active: bool, tone: Color32) -> egui::Response {
    let fill = if active {
        tone.gamma_multiply(if is_light() { 0.11 } else { 0.20 })
    } else {
        Color32::TRANSPARENT
    };
    let foreground = if active { tone } else { TEXT2() };
    let (rect, response) = ui.allocate_exact_size(
        vec2(
            ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(12.5), foreground).size().x + if icon.is_some() { 38.0 } else { 20.0 },
            30.0,
        ),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        if response.hovered() && !active { BG3() } else { fill },
        Stroke::new(1.0, if active { tone.gamma_multiply(0.27) } else { Color32::TRANSPARENT }),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 10.0;
    if let Some(icon) = icon {
        draw_icon(painter, egui::Rect::from_center_size(pos2(x + 7.0, rect.center().y), vec2(14.0, 14.0)), icon, foreground);
        x += 20.0;
    }
    painter.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, label, FontId::proportional(12.5), foreground);
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, label));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl CircuitView {
    pub(super) fn refresh_diagnostics(&mut self) {
        if self.need_check && matches!(self.drag, Drag::None) {
            let focused = self.focused_issue.and_then(|i| self.issues.get(i)).map(|issue| issue.msg.clone());
            self.netlist = crate::circuit::net::Netlist::build(&self.c);
            self.junctions = routing::junctions(&self.c);
            self.issues = crate::circuit::check::check_netlist(&self.c, &self.netlist);
            if let Some(msg) = focused {
                self.focused_issue = self.issues.iter().position(|i| i.msg == msg);
                if let Some(index) = self.focused_issue {
                    self.focus_points = self.issues[index].pins.iter().filter_map(|&(id, k)| self.pin_at(id, k)).collect();
                } else {
                    self.focus_points.clear();
                }
            }
            self.need_check = false;
        }
    }

    fn show_group(ui: &mut egui::Ui, frame: egui::Frame, add: impl FnOnce(&mut egui::Ui)) {
        frame.show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            add(ui);
        });
    }

    pub(super) fn workspace(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let narrow = width < NARROW;
        let overlay = width < 620.0;
        egui::TopBottomPanel::top("cv-top")
            .frame(egui::Frame::new().fill(BG1()).stroke(st(1.0, LINE())).inner_margin(egui::Margin::symmetric(12, 8)))
            .show_inside(ui, |ui| self.workspace_header(ui, width));

        if self.wiring_on {
            self.wiring.set_dir(super::wiring::dir_for(self.project.as_deref()));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG0()).inner_margin(egui::Margin::same(14)))
                .show_inside(ui, |ui| self.wiring.ui(ui, &self.c));
            for (m, c) in std::mem::take(&mut self.wiring.toasts) {
                self.toast(m, c);
            }
            return;
        }

        let frame = egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(12, 9));
        let library = self.library_open && (!narrow || !self.inspector_open);
        let inspector = self.inspector_open;
        if overlay {
            if inspector {
                self.workspace_overlay(ui, false);
            } else if self.compact_library {
                self.workspace_overlay(ui, true);
            }
        } else {
            if library && !self.compact {
                egui::SidePanel::left("cv-lib")
                    .frame(frame)
                    .default_width(204.0)
                    .width_range(170.0..=285.0)
                    .show_inside(ui, |ui| self.library(ui));
            } else if self.compact && self.compact_library {
                self.workspace_overlay(ui, true);
            }
            if self.inspector_open {
                egui::SidePanel::right("cv-side")
                    .frame(frame)
                    .default_width(296.0)
                    .width_range(240.0..=(width * 0.46).clamp(250.0, 410.0))
                    .show_inside(ui, |ui| self.side(ui));
            }
        }
        egui::TopBottomPanel::bottom("cv-status")
            .frame(egui::Frame::new().fill(BG1()).stroke(st(1.0, LINE())).inner_margin(egui::Margin::symmetric(11, 3)))
            .show_inside(ui, |ui| self.workspace_status(ui));
        if self.bottom {
            let max_height = (ui.available_height() * 0.49).clamp(95.0, 520.0);
            let min_height = if self.btab == BTab::Scope { 195.0_f32.min(max_height) } else { 95.0 };
            egui::TopBottomPanel::bottom("cv-bottom")
                .frame(frame)
                .default_height(if self.btab == BTab::Log { 150.0 } else { 210.0 })
                .height_range(min_height..=max_height)
                .resizable(true)
                .show_inside(ui, |ui| self.bottom_ui(ui));
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(canvas_color())).show_inside(ui, |ui| {
            self.canvas(ui);
        });
        self.show_power_dialog(ui.ctx(), width);
    }

    fn workspace_header(&mut self, ui: &mut egui::Ui, width: f32) {
        Self::show_group(ui, egui::Frame::new(), |ui| {
            ui.horizontal(|ui| {
                if !self.compact && width > 610.0 && icon_button(ui, Icon::Back, t("回到工具箱"), false).clicked() {
                    self.events.push(CvEvt::Back);
                }
                let (tile, _) = ui.allocate_exact_size(vec2(33.0, 33.0), egui::Sense::hover());
                ui.painter().rect_filled(tile, CornerRadius::same(9), ACCENT().gamma_multiply(0.17));
                draw_icon(ui.painter(), egui::Rect::from_center_size(tile.center(), vec2(18.0, 18.0)), Icon::Toolbox, ACCENT());
                ui.add_space(4.0);
                self.title_widget(ui, width);
                if self.has_unsaved_file_edits() {
                    ui.label(RichText::new("●").size(8.0).color(WARN())).on_hover_text(t("有未保存的文件修改"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(RichText::new(t("文件")).size(12.5), |ui| self.more_menu(ui));
                    if width > 650.0 || self.wiring_on {
                        let label = if self.wiring_on { t("返回电路") } else { t("接线记录") };
                        if action(ui, label, None, self.wiring_on, ACCENT()).clicked() {
                            self.toggle_wiring();
                        }
                    }
                    if self.practice.is_some() && action(ui, t("退出练习"), None, true, ACCENT()).clicked() {
                        self.end_practice();
                    }
                });
            });
        });
        if self.wiring_on {
            return;
        }
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(BG0())
            .stroke(st(1.0, LINE()))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(5, 3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                ui.horizontal_wrapped(|ui| {
                    let on = self.powered();
                    let caption = if on { t("断电") } else { t("通电") };
                    let icon = if on { Icon::Close } else { Icon::Play };
                    if action(ui, caption, Some(icon), true, if on { OK() } else { ACCENT() }).on_hover_text("F5").clicked() {
                        self.toggle_power();
                    }
                    if on {
                        let icon = if self.paused { Icon::Play } else { Icon::Pause };
                        if icon_button(ui, icon, if self.paused { t("继续") } else { t("暂停") }, self.paused).clicked() {
                            self.paused = !self.paused;
                        }
                    }
                    ui.separator();
                    for (tool, caption, icon, shortcut) in [
                        (Tool::Select, t("选择"), Icon::Edit, "V"),
                        (Tool::Wire, t("画线"), Icon::Relations, "W"),
                        (Tool::Probe, t("测量"), Icon::Pulse, "P"),
                    ] {
                        if action(ui, caption, Some(icon), self.tool == tool, ACCENT()).on_hover_text(shortcut).clicked() {
                            self.choose_tool(tool);
                        }
                    }
                    ui.separator();
                    let issues = self.issues.iter().filter(|i| i.level != Level::Tip).count();
                    let label = if issues == 0 { t("检查").into() } else { format!("{} {issues}", t("检查")) };
                    let tone = if self.issues.iter().any(|issue| issue.level == Level::Err) {
                        ERR()
                    } else if issues > 0 {
                        WARN()
                    } else {
                        ACCENT()
                    };
                    if action(ui, &label, Some(Icon::Todo), issues > 0 || self.inspector_open && self.tab == RTab::Checks, tone).clicked() {
                        self.open_inspector(RTab::Checks, width);
                    }
                    if action(ui, t("AI"), Some(Icon::Chat), self.inspector_open && self.tab == RTab::Assistant, VIOLET()).clicked() {
                        self.open_inspector(RTab::Assistant, width);
                    }
                    ui.separator();
                    if action(ui, t("仪表"), Some(Icon::Chart), self.bottom, ACCENT()).clicked() {
                        self.bottom = !self.bottom;
                    }
                    let n_new = self.sim.as_ref().map_or(0, |s| s.log.len().saturating_sub(self.seen_log));
                    if n_new > 0 && !self.bottom {
                        let at = ui.min_rect().right_top() + vec2(-3.0, 7.0);
                        ui.painter().circle_filled(at, 2.5, WARN());
                    }
                    if icon_button(ui, Icon::Sidebar, t("元件库"), self.library_open && !self.compact && width >= 620.0 || self.compact_library).clicked() {
                        if width < 620.0 || self.compact {
                            self.compact_library = !self.compact_library;
                            if self.compact_library {
                                self.inspector_open = false;
                            }
                        } else {
                            self.library_open = !self.library_open || width < NARROW && self.inspector_open;
                            if self.library_open && width < NARROW {
                                self.inspector_open = false;
                            }
                        }
                    }
                    if icon_button(ui, Icon::Info, t("属性"), self.inspector_open && self.tab == RTab::Props).clicked() {
                        if self.tab == RTab::Props && self.inspector_open {
                            self.inspector_open = false;
                        } else {
                            self.open_inspector(RTab::Props, width);
                        }
                    }
                    if icon_button(ui, Icon::Maximize, &format!("{} (Home)", t("看全部")), false).clicked() {
                        self.fit = true;
                    }
                    if width > 1000.0 {
                        if icon_button(ui, Icon::Back, t("撤销"), false).clicked() {
                            self.undo();
                        }
                        if icon_button(ui, Icon::Forward, t("重做"), false).clicked() {
                            self.redo();
                        }
                    }
                });
            });
    }

    fn title_widget(&mut self, ui: &mut egui::Ui, width: f32) {
        let id = ui.make_persistent_id("cv-title-edit");
        if ui.memory(|m| m.data.get_temp::<bool>(id)).unwrap_or_default() {
            let desired = (width * 0.48).min(380.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.c.title).id(id.with("input")).desired_width(desired));
            if r.changed() {
                self.changed();
            }
            if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                ui.memory_mut(|m| m.data.remove::<bool>(id));
            }
        } else {
            let title = if self.c.title.is_empty() { t("未命名电路") } else { &self.c.title };
            let r = ui
                .allocate_ui_with_layout(vec2((width * 0.44).max(70.0), 33.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(title).size(if self.compact { 15.0 } else { 19.0 }).strong().color(TEXT()))
                            .truncate()
                            .sense(egui::Sense::click()),
                    )
                })
                .inner;
            if r.on_hover_text(t("双击改名")).double_clicked() {
                ui.memory_mut(|m| {
                    m.data.insert_temp(id, true);
                    m.request_focus(id.with("input"));
                });
            }
        }
    }

    fn workspace_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(7.0, 2.0);
            let (status, tone) = match &self.sim {
                None => (t("待通电"), TEXT3()),
                Some(_) if self.paused => (t("已暂停"), WARN()),
                Some(s) if !s.ok => (t("不收敛"), ERR()),
                Some(_) => (t("仿真运行中"), OK()),
            };
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 16.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 2.8, tone);
            ui.label(RichText::new(status).size(11.5).color(TEXT2()));
            if let Some(s) = &self.sim {
                ui.label(RichText::new(super::fmt_time(s.t)).font(FontId::monospace(11.5)).color(TEXT3()));
                let cur = SPEEDS.iter().find(|s| s.0 == self.speed).map_or("?", |s| s.1);
                egui::ComboBox::from_id_salt("cv-speed").width(47.0).selected_text(t(cur)).show_ui(ui, |ui| {
                    for (v, name) in SPEEDS {
                        ui.selectable_value(&mut self.speed, v, t(name));
                    }
                });
            }
            ui.separator();
            for (on, label) in [(false, t("实物")), (true, t("原理图"))] {
                if ui.selectable_label(self.schematic == on, RichText::new(label).size(11.5)).clicked() {
                    self.schematic = on;
                }
            }
            ui.label(RichText::new(format!("{:.0}%", self.view.s / 14.0 * 100.0)).font(FontId::monospace(11.5)).color(TEXT3()));
            if ui.available_width() > 260.0 {
                ui.separator();
                let hint = if self.draft.is_some() {
                    t("点接点结束 · 空白处加拐点 · R 换拐向 · Esc 取消")
                } else if self.placing.is_some() {
                    t("点击放下 · R 旋转 · 按住 Shift 连续放 · Esc 取消")
                } else if self.tool == Tool::Probe {
                    t("点击引脚或导线放置电压探针")
                } else if self.tool == Tool::Wire {
                    t("从引脚、面包板孔或导线开始")
                } else {
                    t("滚轮缩放 · 空格拖动画布")
                };
                ui.label(RichText::new(hint).size(11.5).color(TEXT3()));
            }
        });
    }

    fn workspace_overlay(&mut self, ui: &egui::Ui, library: bool) {
        let mut open = true;
        let size = ui.available_size();
        let pos = ui.min_rect().min + vec2(10.0, 77.0);
        egui::Window::new(if library { t("元件库") } else { t("检查与属性") })
            .id(ui.make_persistent_id(if library { "cv-library-overlay" } else { "cv-inspector-overlay" }))
            .default_pos(pos)
            .default_width((size.x - 36.0).clamp(195.0, 304.0))
            .default_height((size.y - 115.0).clamp(130.0, 380.0))
            .max_height((size.y - 93.0).max(130.0))
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                if library {
                    self.library(ui);
                } else {
                    self.side(ui);
                }
            });
        if !open {
            if library {
                self.compact_library = false;
            } else {
                self.inspector_open = false;
            }
        }
    }

    pub(super) fn open_inspector(&mut self, tab: RTab, width: f32) {
        self.tab = tab;
        self.inspector_open = true;
        if width < NARROW {
            self.compact_library = false;
        }
    }

    pub(super) fn choose_tool(&mut self, tool: Tool) {
        self.draft = None;
        self.placing = None;
        self.tool = tool;
        if tool == Tool::Wire && self.powered() {
            self.paused = true;
        }
    }

    pub(super) fn toggle_wiring(&mut self) {
        self.wiring_on = !self.wiring_on;
        if !self.wiring_on {
            self.wiring.save_now();
        }
    }

    pub(super) fn toggle_power(&mut self) {
        if self.powered() {
            self.set_power(false);
        } else {
            self.refresh_diagnostics();
            if self.issues.iter().any(|i| i.level == Level::Err) {
                self.power_dialog = true;
            } else {
                self.set_power(true);
            }
        }
    }

    fn show_power_dialog(&mut self, ctx: &egui::Context, width: f32) {
        if !self.power_dialog {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("cv-power-confirm")).show(ctx, |ui| {
            ui.set_width(330.0);
            ui.heading(t("通电前发现接线问题"));
            if let Some(i) = self.issues.iter().find(|i| i.level == Level::Err) {
                ui.add_space(6.0);
                ui.label(&i.msg);
            }
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if action(ui, t("查看问题"), Some(Icon::Todo), true, ACCENT()).clicked() {
                    self.open_inspector(RTab::Checks, width);
                    if let Some(i) = self.issues.iter().position(|i| i.level == Level::Err) {
                        self.locate_issue(i);
                    }
                    self.power_dialog = false;
                }
                if action(ui, t("继续仿真"), Some(Icon::Play), false, ACCENT()).clicked() {
                    self.set_power(true);
                    self.power_dialog = false;
                }
            });
        });
        if modal.should_close() {
            self.power_dialog = false;
        }
    }

    pub(super) fn contact_name(&self, contact: routing::Contact) -> String {
        match contact {
            routing::Contact::Pin(id, k) => self
                .c
                .part(id)
                .and_then(|p| lib::def(&p.kind))
                .and_then(|d| d.pins.get(k))
                .map_or_else(|| t("引脚").into(), |pin| format!("{}  ·  {}", self.c.label(id), pin.name)),
            routing::Contact::Wire(_) => t("导线").into(),
        }
    }

    pub(super) fn pin_at(&self, id: u32, pin: usize) -> Option<Pt> {
        self.c.part(id)?.pins().get(pin).copied()
    }

    pub(super) fn locate_issue(&mut self, idx: usize) {
        let Some(issue) = self.issues.get(idx) else { return };
        let pins = issue.pins.clone();
        self.focused_issue = Some(idx);
        self.focus_points = pins.iter().filter_map(|&(id, k)| self.pin_at(id, k)).collect();
        self.net_origin = pins.first().map(|&(id, k)| routing::Contact::Pin(id, k));
        self.center_points(&self.focus_points.clone());
    }

    pub(super) fn center_points(&mut self, points: &[Pt]) {
        let Some(r) = self.canvas_rect else { return };
        let Some(&(x, y)) = points.first() else { return };
        let bounds = points.iter().fold((x, y, x, y), |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
        let x = (bounds.0 as f32 + bounds.2 as f32) / 2.0;
        let y = (bounds.1 as f32 + bounds.3 as f32) / 2.0;
        let scale = (r.width() / (bounds.2.saturating_sub(bounds.0) as f32 + 7.0))
            .min(r.height() / (bounds.3.saturating_sub(bounds.1) as f32 + 7.0))
            .clamp(3.0, 34.0);
        self.auto_frame = false;
        self.view.s = self.view.s.min(scale).max(3.0);
        self.view.org = r.center() - vec2(x, y) * self.view.s;
    }

    pub(super) fn start_practice(&mut self, width: f32) {
        if self.practice.is_some() {
            self.open_inspector(RTab::Example, width);
            return;
        }
        let Some(ex) = crate::circuit::examples::find("A2") else { return };
        let mut circuit = match ex.build_practice() {
            Ok(c) => c,
            Err(err) => {
                self.toast(err, ERR());
                return;
            }
        };
        let Some(led) = circuit.parts.iter().find(|p| p.kind == "led") else { return };
        let Some(power) = circuit.parts.iter().find(|p| p.kind == "usb5v") else { return };
        let targets = ((led.id, 1), (power.id, 1));
        let (a, b) = (led.pins()[1], power.pins()[1]);
        circuit
            .wires
            .retain(|w| !matches!((w.pts.first(), w.pts.last()), (Some(x), Some(y)) if (*x == a && *y == b) || (*x == b && *y == a)));
        self.practice = Some(Practice {
            previous: std::mem::replace(&mut self.c, circuit),
            path: self.path.take(),
            saved: self.save_source.take(),
            example: self.example.take(),
            view: self.view,
            canvas_rect: self.canvas_rect,
            schematic: self.schematic,
            dirty_at: self.dirty_at.take(),
            auto_frame: self.auto_frame,
            inspector_open: self.inspector_open,
            tab: self.tab,
            bottom: self.bottom,
            btab: self.btab,
            scope: std::mem::take(&mut self.scope),
            selection: std::mem::take(&mut self.sel),
            sim: self.sim.take(),
            paused: self.paused,
            undo: std::mem::take(&mut self.undo),
            redo: std::mem::take(&mut self.redo),
            target: targets,
        });
        self.reset_canvas_state();
        self.example = Some("A2".into());
        self.schematic = true;
        self.choose_tool(Tool::Wire);
        self.open_inspector(RTab::Example, width);
    }

    pub(super) fn practice_targets(&self) -> Option<[Pt; 2]> {
        let practice = self.practice.as_ref()?;
        let (a, b) = practice.target;
        if self.netlist.same(a, b) {
            return None;
        }
        Some([self.pin_at(a.0, a.1)?, self.pin_at(b.0, b.1)?])
    }

    pub(super) fn practice_ui(&mut self, ui: &mut egui::Ui) {
        let Some(practice) = self.practice.as_ref() else { return };
        let (a, b) = practice.target;
        let complete = self.netlist.same(a, b) && !self.issues.iter().any(|i| i.level == Level::Err);
        let measured = complete
            && self.powered()
            && self
                .c
                .parts
                .iter()
                .any(|p| p.kind == "probe" && (self.netlist.same((p.id, 0), (a.0, 0)) || self.netlist.same((p.id, 0), a)));
        ui.heading(t("接亮第一个 LED"));
        ui.add_space(8.0);
        for (title, desc, step_done, active) in [
            (
                t("接上回路线"),
                tf(
                    "把 {} 接到 {}",
                    &[&self.contact_name(routing::Contact::Pin(a.0, a.1)), &self.contact_name(routing::Contact::Pin(b.0, b.1))],
                ),
                complete,
                !complete,
            ),
            (t("通电观察"), t("点通电，确认 LED 点亮。").into(), complete && self.powered(), complete && !self.powered()),
            (t("测量电压"), t("选测量工具，在 LED 的任一引脚放一个探针。").into(), measured, complete && self.powered() && !measured),
        ] {
            card(ui, |ui| {
                ui.label(RichText::new(format!("{}  {title}", if step_done { "✓" } else { "○" })).strong().color(if step_done {
                    OK()
                } else if active {
                    ACCENT()
                } else {
                    TEXT2()
                }));
                if active {
                    ui.label(RichText::new(desc).color(TEXT2()));
                }
            });
            ui.add_space(7.0);
        }
        if measured {
            ui.label(RichText::new(t("已完成第一次接线和测量")).strong().color(OK()));
        }
        ui.add_space(4.0);
        if action(ui, t("回到原电路"), Some(Icon::Back), false, ACCENT()).clicked() {
            self.end_practice();
        }
    }

    pub(super) fn end_practice(&mut self) {
        let Some(p) = self.practice.take() else { return };
        self.c = p.previous;
        self.reset_canvas_state();
        self.path = p.path;
        self.save_source = p.saved;
        self.example = p.example;
        self.tab = p.tab;
        self.inspector_open = p.inspector_open;
        self.bottom = p.bottom;
        self.btab = p.btab;
        self.scope = p.scope;
        self.view = p.view;
        self.canvas_rect = p.canvas_rect;
        self.auto_frame = p.auto_frame;
        self.sel = p.selection;
        self.sim = p.sim;
        self.paused = p.paused;
        self.last_frame = None;
        self.schematic = p.schematic;
        self.dirty_at = p.dirty_at;
        self.undo = p.undo;
        self.redo = p.redo;
        self.fit = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_practice_disconnects_exactly_the_goal_and_restores_the_document() {
        let mut view = CircuitView::new();
        let original = crate::circuit::examples::find("C5").unwrap().build().unwrap();
        view.load(original.clone(), Some("original.qccirc".into()));
        view.start_practice(1200.0);
        view.refresh_diagnostics();
        let [a, b] = view.practice_targets().expect("the return wire should be disconnected");
        view.c.add_wire(vec![a, b], 0);
        view.changed();
        view.refresh_diagnostics();
        assert!(view.practice_targets().is_none());
        view.end_practice();
        assert_eq!(view.c, original);
        assert_eq!(view.path.unwrap().to_str(), Some("original.qccirc"));
        assert!(!view.bottom && !view.inspector_open);
    }
}

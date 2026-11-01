//! 副页面的界面：右侧 / 底部的停靠区、标题栏、分隔条、独立窗口、拖动停靠、
//! 来回切页的提示条、状态栏上的格局图标和提示。

use super::toolbox::Tool;
use super::work::{DockPos, DragPane, DropAt, Pane, Strength, DEFAULT_FRAC, MIN_SIDE};
use super::*;

const TITLE_H: f32 = 30.0;

impl App {
    /// 画副页面（在主页面之前调用：ctx 级面板先占位置，主页面填剩下的）
    pub(super) fn dock_panel(&mut self, ctx: &egui::Context, acts: &[Act]) {
        let Some(p) = self.side_pane() else {
            self.work.rect = None;
            return;
        };
        if self.work.detached {
            self.detached_window(ctx, p, acts);
            return;
        }
        let avail = ctx.available_rect();
        // 打开时滑入
        let k = self.work.opened_at.map_or(1.0, |t| (t.elapsed().as_secs_f32() / 0.15).min(1.0));
        if k < 1.0 {
            ctx.request_repaint();
        } else {
            self.work.opened_at = None;
        }
        let k = 1.0 - (1.0 - k).powi(3);
        let frame = egui::Frame::new().fill(BG0());
        let mut close = false;
        match self.work.pos {
            DockPos::Right => {
                let total = avail.width();
                let max = (total - 360.0).max(MIN_SIDE);
                let w = (self.work.frac * total).clamp(MIN_SIDE.min(max), max) * k;
                egui::SidePanel::right("work-dock-r")
                    .exact_width(w)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(frame)
                    .show(ctx, |ui| {
                        let rect = ui.max_rect();
                        self.work.rect = Some(rect);
                        close = self.splitter(ui, rect, total, true);
                        ui.push_id("work-side", |ui| self.side_frame(ui, p, acts));
                    });
            }
            DockPos::Bottom => {
                let total = avail.height();
                let max = (total - 200.0).max(160.0);
                let h = (self.work.frac * total).clamp(160.0f32.min(max), max) * k;
                egui::TopBottomPanel::bottom("work-dock-b")
                    .exact_height(h)
                    .resizable(false)
                    .show_separator_line(false)
                    .frame(frame)
                    .show(ctx, |ui| {
                        let rect = ui.max_rect();
                        self.work.rect = Some(rect);
                        close = self.splitter(ui, rect, total, false);
                        ui.push_id("work-side", |ui| self.side_frame(ui, p, acts));
                    });
            }
        }
        if close {
            self.close_side();
        }
    }

    /// 主副之间的分隔条：拖动调比例，双击回到默认，拖到太窄就收起。返回是否要收起
    fn splitter(&mut self, ui: &mut egui::Ui, rect: Rect, total: f32, right: bool) -> bool {
        let strip = if right {
            Rect::from_min_max(rect.min, pos2(rect.left() + 5.0, rect.bottom()))
        } else {
            Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 5.0))
        };
        let r = ui.interact(strip, ui.id().with("work-split"), Sense::click_and_drag());
        let hot = r.hovered() || r.dragged();
        let p = ui.painter();
        if right {
            p.vline(rect.left() + 0.5, rect.y_range(), st(1.0, if hot { ACCENT().gamma_multiply(0.7) } else { LINE() }));
        } else {
            p.hline(rect.x_range(), rect.top() + 0.5, st(1.0, if hot { ACCENT().gamma_multiply(0.7) } else { LINE() }));
        }
        if hot {
            ui.ctx().set_cursor_icon(if right { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
        }
        let r = r.on_hover_text(t("拖动调整大小，双击恢复默认"));
        if r.double_clicked() {
            self.work.frac = DEFAULT_FRAC;
        }
        if r.dragged() {
            if let Some(h) = ui.input(|i| i.pointer.interact_pos()) {
                let size = if right { rect.right() - h.x } else { rect.bottom() - h.y };
                self.work.frac = (size / total).clamp(0.12, 0.8);
                if size < MIN_SIDE * 0.6 {
                    // 拖得太窄：松手就收起，先给个暗示
                    ui.painter().rect_filled(rect, 0.0, shade(60));
                }
            }
        }
        if r.drag_stopped() {
            if let Some(h) = ui.input(|i| i.pointer.interact_pos()) {
                let size = if right { rect.right() - h.x } else { rect.bottom() - h.y };
                if size < MIN_SIDE * 0.6 {
                    self.work.frac = DEFAULT_FRAC;
                    return true;
                }
            }
        }
        false
    }

    /// 副页面：标题栏 + 内容
    fn side_frame(&mut self, ui: &mut egui::Ui, p: Pane, acts: &[Act]) {
        egui::TopBottomPanel::top("work-title")
            .exact_height(TITLE_H)
            .show_separator_line(true)
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 8, right: 6, top: 0, bottom: 0 }))
            .show_inside(ui, |ui| self.side_title(ui, false));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| self.render_pane(ui, p, acts));
    }

    /// 标题栏：标签（多个时）+ ⇄ ⬌ ↗ ×
    fn side_title(&mut self, ui: &mut egui::Ui, detached: bool) {
        let focus = self.work.focus_side;
        let mut pick = None;
        let mut close_tab = None;
        let mut full = None;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let tabs = self.work.tabs.clone();
            for (i, tab) in tabs.iter().enumerate() {
                let on = i == self.work.cur;
                let name = tab.label();
                let font = FontId::proportional(12.5);
                let gw = ui.painter().layout_no_wrap(name.clone(), font.clone(), TEXT()).size().x;
                let multi = tabs.len() > 1;
                let w = gw + 34.0 + if multi { 18.0 } else { 0.0 };
                let (r, resp) = ui.allocate_exact_size(vec2(w, TITLE_H - 6.0), Sense::click_and_drag());
                let pt = ui.painter();
                if on && multi {
                    pt.rect_filled(r, CornerRadius::same(5), BG3());
                } else if resp.hovered() {
                    pt.rect_filled(r, CornerRadius::same(5), tint(6));
                }
                let col = if on {
                    if focus {
                        ACCENT()
                    } else {
                        TEXT2()
                    }
                } else {
                    TEXT3()
                };
                draw_icon(pt, Rect::from_center_size(pos2(r.left() + 13.0, r.center().y), vec2(14.0, 14.0)), tab.icon(), col);
                pt.text(pos2(r.left() + 26.0, r.center().y), Align2::LEFT_CENTER, &name, font, if on { TEXT() } else { TEXT2() });
                if multi {
                    let xr = Rect::from_center_size(pos2(r.right() - 11.0, r.center().y), vec2(14.0, 14.0));
                    let over = ui.input(|i| i.pointer.hover_pos()).is_some_and(|h| xr.contains(h));
                    if resp.hovered() || on {
                        draw_icon(ui.painter(), xr.shrink(3.0), Icon::Close, if over { TEXT() } else { TEXT3() });
                    }
                    if resp.clicked() && over {
                        close_tab = Some(i);
                    } else if resp.clicked() {
                        pick = Some(i);
                    }
                } else if resp.clicked() {
                    pick = Some(i);
                }
                if resp.middle_clicked() {
                    close_tab = Some(i);
                }
                if resp.double_clicked() {
                    full = Some(*tab);
                }
                if !detached && resp.drag_started() {
                    if let Some(h) = ui.input(|i| i.pointer.press_origin()) {
                        self.work.drag = Some(DragPane {
                            pane: *tab,
                            from: h,
                            live: false,
                            file: None,
                            own: true,
                        });
                    }
                }
                resp.on_hover_text(t("双击放到整页；拖到中间和主页面互换"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let small = |ui: &mut egui::Ui, icon: Icon, tip: &str| {
                    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(5), BG3());
                    }
                    draw_icon(ui.painter(), r.shrink(5.0), icon, if resp.hovered() { TEXT() } else { TEXT3() });
                    resp.on_hover_text(tip)
                };
                let (k_toggle, k_bottom, k_right, k_swap) = (
                    self.km.label(Act::SideToggle),
                    self.km.label(Act::SideBottom),
                    self.km.label(Act::SideRight),
                    self.km.label(Act::SideSwap),
                );
                if small(ui, Icon::Close, &format!("{}  {}", t("收起副页面"), k_toggle)).clicked() {
                    if detached {
                        self.work.detached = false;
                    }
                    self.close_side();
                }
                if detached {
                    if small(ui, Icon::DockRight, t("放回主窗口")).clicked() {
                        self.work.detached = false;
                    }
                } else {
                    if small(ui, Icon::PopOut, t("弹出成独立窗口（放到第二块屏幕）")).clicked() {
                        self.work.detached = true;
                    }
                    let (icon, tip, to) = match self.work.pos {
                        DockPos::Right => (Icon::DockBottom, format!("{}  {}", t("放到下方"), k_bottom), DockPos::Bottom),
                        DockPos::Bottom => (Icon::DockRight, format!("{}  {}", t("放到右边"), k_right), DockPos::Right),
                    };
                    if small(ui, icon, &tip).clicked() {
                        self.work.pos = to;
                    }
                }
                if small(ui, Icon::Swap, &format!("{}  {}", t("和主页面互换"), k_swap)).clicked() {
                    self.swap_side();
                }
            });
        });
        if let Some(i) = pick {
            self.work.cur = i;
            self.work.focus_side = true;
        }
        if let Some(i) = close_tab {
            self.close_side_tab(i);
        }
        if let Some(p) = full {
            // 放到整页：副页面收起，「← 返回」能回到并排
            self.work.tabs.retain(|x| *x != p);
            self.work.cur = 0;
            self.goto_pane(p);
        }
    }

    /// 在一块区域里画某个页面（副页面、独立窗口）
    pub(super) fn render_pane(&mut self, ui: &mut egui::Ui, p: Pane, acts: &[Act]) {
        match p {
            Pane::Debug => self.with_slot(0, |app| app.debug_in(ui, true)),
            Pane::Code => {
                let km = &self.km;
                self.code.ui_in(ui, acts, km, true);
            }
            Pane::Files => {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(BG1())).show_inside(ui, |ui| {
                    self.code.files_ui(ui, false);
                });
            }
            Pane::Git => {
                self.gitv.unsaved = self.code.dirty_paths();
                self.gitv.sensitive.clone_from(&self.code.s.sensitive);
                self.gitv.ui_in(ui, acts, true);
            }
            Pane::Compare => self.cmpv.ui_in(ui),
            Pane::Community => self.comm.ui_in(ui, true),
            Pane::Tool(tool) => self.tool_pane(ui, tool),
        }
    }

    /// 副页面里的工具：临时把「当前工具」换成它来画，画完换回去
    fn tool_pane(&mut self, ui: &mut egui::Ui, tool: Tool) {
        let keep_tool = self.s.tb.tool;
        let keep_api = self.s.api_mode;
        let keep_page = self.page;
        self.s.tb.tool = tool;
        self.s.api_mode = tool == Tool::Api;
        let slot = Pane::Tool(tool).slot().or((tool == Tool::Api).then_some(0));
        match slot {
            Some(k) => self.with_slot(k, |app| app.toolbox_in(ui, true)),
            None => self.toolbox_in(ui, true),
        }
        let after = self.s.tb.tool;
        self.s.api_mode = keep_api;
        if after != tool || self.page != keep_page {
            // 工具里跳到了别的工具或页面（比如电路里导出 VCD）：照常切过去
            if after == tool {
                self.s.tb.tool = keep_tool;
            }
        } else {
            self.s.tb.tool = keep_tool;
        }
    }

    /// 临时换进第 k 个会话来画，画完换回去
    pub(super) fn with_slot(&mut self, k: usize, f: impl FnOnce(&mut Self)) {
        let cur = self.cur_slot();
        if k == cur {
            f(self);
            return;
        }
        self.quiet_slot(k);
        self.update_view();
        f(self);
        self.quiet_slot(cur);
    }

    /// 副页面弹出成独立窗口
    fn detached_window(&mut self, ctx: &egui::Context, p: Pane, acts: &[Act]) {
        self.work.rect = None;
        let id = egui::ViewportId::from_hash_of("qccom-side");
        let title = format!("QCCOM · {}", p.label());
        let builder = egui::ViewportBuilder::default().with_title(title).with_inner_size([760.0, 620.0]).with_min_inner_size([320.0, 240.0]);
        let ctx2 = ctx.clone();
        let mut back = false;
        ctx2.show_viewport_immediate(id, builder, |vctx, _class| {
            egui::TopBottomPanel::top("work-title-x")
                .exact_height(TITLE_H)
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 8, right: 6, top: 0, bottom: 0 }))
                .show(vctx, |ui| self.side_title(ui, true));
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(vctx, |ui| {
                if let Some(p) = self.side_pane() {
                    // 窗口里有焦点时键盘归它
                    let acts: &[Act] = if vctx.input(|i| i.focused) { acts } else { &[] };
                    ui.push_id("work-side", |ui| self.render_pane(ui, p, acts));
                }
            });
            if vctx.input(|i| i.viewport().close_requested()) {
                back = true;
            }
        });
        if back {
            // 关掉独立窗口 = 放回主窗口
            self.work.detached = false;
        }
    }

    // ------------------------------------------------------------ 拖动停靠

    /// 拖动源（活动栏图标、工具标签、文件标签）每帧调用：按住拖超过 8 像素就开始
    pub(super) fn drag_source(&mut self, resp: &egui::Response, p: Pane, file: Option<PathBuf>) {
        if resp.drag_started() {
            if let Some(h) = resp.ctx.input(|i| i.pointer.press_origin()) {
                self.work.drag = Some(DragPane {
                    pane: p,
                    from: h,
                    live: false,
                    file,
                    own: false,
                });
            }
        }
    }

    /// 正在拖：判断放在哪；松手执行；Esc 取消
    pub(super) fn drag_overlay(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.work.drag else { return };
        let (pos, down, esc) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.primary_down(), i.key_pressed(Key::Escape)));
        let Some(pos) = pos else { return };
        if !d.live && pos.distance(d.from) > 8.0 {
            d.live = true;
        }
        if esc {
            self.work.drag = None;
            return;
        }
        let live = d.live;
        let pane = d.pane;
        let own = d.own;
        let file = d.file.clone();
        if !live {
            if !down {
                self.work.drag = None;
            }
            return;
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let area = self.work.main_rect.unwrap_or_else(|| ctx.available_rect());
        let full = ctx.screen_rect();
        // 在活动栏上 = 取消
        let on_bar = pos.x < full.left() + 48.0;
        let at = if on_bar {
            None
        } else if pos.x > full.right() - 120.0 || (own && self.work.pos == DockPos::Right && self.work.rect.is_some_and(|r| r.contains(pos))) {
            Some(DropAt::Right)
        } else if pos.y > full.bottom() - 140.0 {
            Some(DropAt::Bottom)
        } else if area.contains(pos) {
            Some(DropAt::Center)
        } else {
            None
        };
        // 文件标签只能拖到右边（拆分编辑器）
        let at = if file.is_some() { at.filter(|a| *a == DropAt::Right) } else { at };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("work-drag")));
        if let Some(a) = at {
            let main = self.work.main_rect.unwrap_or(full);
            // 已经有副页面在那个位置：预览就是它
            let cur_side = self.work.rect.filter(|_| !self.work.tabs.is_empty());
            let (r, text) = match a {
                DropAt::Right if file.is_none() && cur_side.is_some() && self.work.pos == DockPos::Right => (cur_side.unwrap(), t("放在右边")),
                DropAt::Bottom if cur_side.is_some() && self.work.pos == DockPos::Bottom => (cur_side.unwrap(), t("放在下方")),
                DropAt::Right => (
                    Rect::from_min_max(pos2(main.right() - main.width() * 0.38, main.top()), main.max),
                    if file.is_some() { t("在右边拆分") } else { t("放在右边") },
                ),
                DropAt::Bottom => (Rect::from_min_max(pos2(main.left(), main.bottom() - main.height() * 0.38), main.max), t("放在下方")),
                DropAt::Center => (main.shrink(24.0), t("在这里打开")),
            };
            painter.rect(
                r.shrink(4.0),
                CornerRadius::same(10),
                ACCENT().gamma_multiply(0.10),
                st(1.5, ACCENT().gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
            painter.text(r.center(), Align2::CENTER_CENTER, text, FontId::proportional(15.0), ACCENT());
        }
        // 指针旁边的小卡片
        let name = match &file {
            Some(f) => f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            None => pane.label(),
        };
        let g = painter.layout_no_wrap(name, FontId::proportional(12.5), TEXT());
        let card = Rect::from_min_size(pos + vec2(14.0, 12.0), vec2(g.size().x + 36.0, 26.0));
        painter.rect(card, CornerRadius::same(6), BG2(), st(1.0, LINE_HI()), egui::StrokeKind::Inside);
        draw_icon(&painter, Rect::from_center_size(pos2(card.left() + 14.0, card.center().y), vec2(14.0, 14.0)), pane.icon(), ACCENT());
        painter.galley(pos2(card.left() + 26.0, card.center().y - g.size().y / 2.0), g, TEXT());
        ctx.request_repaint();
        if down {
            return;
        }
        // 松手
        self.work.drag = None;
        let Some(a) = at else { return };
        if let Some(f) = file {
            self.code.split_with(&f);
            return;
        }
        match a {
            DropAt::Right | DropAt::Bottom => {
                let pos = if a == DropAt::Right { DockPos::Right } else { DockPos::Bottom };
                if self.main_pane() == Some(pane) {
                    // 把主页面拖出去：上一个页面回到主区域
                    let prev = self.work.switches.iter().rev().map(|x| x.0).find(|p| *p != pane && *p != Pane::Files);
                    match prev {
                        Some(prev) => {
                            self.goto_pane(prev);
                            self.open_side(pane, Some(pos), true);
                        }
                        None => self.toast(t("先打开另一个页面，再把这个拖到旁边"), WARN()),
                    }
                } else {
                    self.open_side(pane, Some(pos), true);
                }
            }
            DropAt::Center => {
                if own {
                    self.swap_side();
                } else {
                    self.nav_manual();
                    self.goto_pane(pane);
                }
            }
        }
    }

    /// 活动栏 / 工具标签的右键菜单：在右边打开、在下方打开
    pub(super) fn side_menu(&mut self, ui: &mut egui::Ui, p: Pane) {
        let main = self.main_pane() == Some(p);
        ui.add_enabled_ui(!main, |ui| {
            if menu_item(ui, t("在右边打开"), "") {
                self.open_side(p, Some(DockPos::Right), true);
                ui.close_menu();
            }
            if menu_item(ui, t("在下方打开"), "") {
                self.open_side(p, Some(DockPos::Bottom), true);
                ui.close_menu();
            }
        });
        if main {
            ui.label(RichText::new(t("正在主区域显示")).small().color(TEXT3()));
        }
    }

    // ------------------------------------------------------------ 状态栏

    /// 状态栏左边：「↳ 日志里有 HardFault · 在代码旁看」
    pub(super) fn work_chip(&mut self, ui: &mut egui::Ui) {
        if self.work.focus {
            ui.add_space(8.0);
            ui.label(RichText::new(t("专注")).small().color(VIOLET()))
                .on_hover_text(format!("{}  {}", t("专注模式：暂停自动弹出副页面"), self.km.label(Act::FocusMode)));
        }
        let Some(c) = &self.work.chip else { return };
        ui.add_space(10.0);
        let text = format!("↳ {}", c.text);
        let r = ui.add(
            egui::Button::new(RichText::new(text).size(11.5).color(ACCENT()))
                .fill(ACCENT().gamma_multiply(0.10))
                .corner_radius(CornerRadius::same(9))
                .min_size(vec2(0.0, 18.0)),
        );
        let r = r.on_hover_text(t("点一下在旁边打开；右键忽略"));
        if r.clicked() {
            self.take_chip();
        }
        if r.secondary_clicked() {
            self.work.chip = None;
        }
    }

    /// 状态栏右边：格局图标，只在有副页面（或收起过）时出现
    pub(super) fn work_status(&mut self, ui: &mut egui::Ui) {
        if self.work.tabs.is_empty() && self.work.hidden.is_empty() {
            return;
        }
        let icon = match (self.work.tabs.is_empty(), self.work.pos) {
            (true, _) => Icon::Split,
            (false, DockPos::Right) => Icon::DockRight,
            (false, DockPos::Bottom) => Icon::DockBottom,
        };
        let (r, resp) = ui.allocate_exact_size(vec2(22.0, 18.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(4), BG3());
        }
        draw_icon(ui.painter(), r.shrink2(vec2(4.0, 2.0)), icon, if self.work.tabs.is_empty() { TEXT3() } else { ACCENT() });
        let pop = egui::Id::new("work-layout-pop");
        let resp = resp.on_hover_text(t("格局和布局方案"));
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pop));
        }
        egui::popup::popup_above_or_below_widget(ui, pop, &resp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(250.0);
            self.layout_menu(ui);
        });
    }

    /// 四种格局 + 方案
    pub(super) fn layout_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("格局")).size(11.5).color(TEXT3()));
        ui.add_space(4.0);
        let side = !self.work.tabs.is_empty();
        let term = self.shells.s.open;
        let cur = match (side, self.work.pos, term) {
            (false, _, _) => 0,
            (true, DockPos::Right, false) => 1,
            (true, DockPos::Bottom, _) => 2,
            (true, DockPos::Right, true) => 3,
        };
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (i, name) in [t("单页"), t("左右"), t("上下"), t("三区")].iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(54.0, 54.0), Sense::click());
                let p = ui.painter();
                let on = i == cur;
                p.rect_filled(
                    r,
                    CornerRadius::same(6),
                    if on {
                        ACCENT().gamma_multiply(0.14)
                    } else if resp.hovered() {
                        BG3()
                    } else {
                        BG1()
                    },
                );
                let b = Rect::from_min_size(r.min + vec2(9.0, 7.0), vec2(36.0, 26.0));
                let c = if on { ACCENT() } else { TEXT3() };
                p.rect_stroke(b, 3.0, st(1.0, c), egui::StrokeKind::Inside);
                let fill = c.gamma_multiply(0.45);
                match i {
                    1 => {
                        p.rect_filled(Rect::from_min_max(pos2(b.right() - 13.0, b.top() + 2.0), b.max - vec2(2.0, 2.0)), 1.0, fill);
                    }
                    2 => {
                        p.rect_filled(Rect::from_min_max(pos2(b.left() + 2.0, b.bottom() - 10.0), b.max - vec2(2.0, 2.0)), 1.0, fill);
                    }
                    3 => {
                        p.rect_filled(Rect::from_min_max(pos2(b.right() - 13.0, b.top() + 2.0), pos2(b.right() - 2.0, b.bottom() - 10.0)), 1.0, fill);
                        p.rect_filled(Rect::from_min_max(pos2(b.left() + 2.0, b.bottom() - 8.0), b.max - vec2(2.0, 2.0)), 1.0, c.gamma_multiply(0.25));
                    }
                    _ => {}
                }
                p.text(
                    pos2(r.center().x, r.bottom() - 9.0),
                    Align2::CENTER_CENTER,
                    *name,
                    FontId::proportional(11.0),
                    if on { TEXT() } else { TEXT2() },
                );
                if resp.clicked() {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = pick {
            match i {
                0 => self.close_side(),
                1 | 3 => {
                    self.set_dock_pos(DockPos::Right);
                    self.shells.s.open = i == 3;
                }
                _ => self.set_dock_pos(DockPos::Bottom),
            }
        }
        ui.add_space(8.0);
        ui.separator();
        ui.label(RichText::new(t("布局方案")).size(11.5).color(TEXT3()));
        let mut apply = None;
        let mut del = None;
        let proj = self.work.proj_key.clone();
        let proj_default = self.s.work.proj_scheme.get(&proj).cloned();
        let n_builtin = super::work::builtin_schemes().len();
        for (i, sc) in self.all_schemes().iter().enumerate() {
            ui.horizontal(|ui| {
                let star = proj_default.as_deref() == Some(sc.name.as_str());
                let label = if star { format!("{}  ★", sc.name) } else { sc.name.clone() };
                if ui.add(egui::Button::new(RichText::new(label).size(12.5)).frame(false)).clicked() {
                    apply = Some(sc.name.clone());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if i >= n_builtin && ui.add(egui::Button::new(RichText::new("×").color(TEXT3())).frame(false)).on_hover_text(t("删除")).clicked() {
                        del = Some(sc.name.clone());
                    }
                    if !proj.is_empty() {
                        let tip = if star {
                            t("取消：打开这个工程时用这个方案")
                        } else {
                            t("打开这个工程时用这个方案")
                        };
                        if ui
                            .add(egui::Button::new(RichText::new(if star { "★" } else { "☆" }).color(if star { WARN() } else { TEXT3() })).frame(false))
                            .on_hover_text(tip)
                            .clicked()
                        {
                            if star {
                                self.s.work.proj_scheme.remove(&proj);
                            } else {
                                self.s.work.proj_scheme.insert(proj.clone(), sc.name.clone());
                            }
                        }
                    }
                });
            });
        }
        if let Some(n) = apply {
            self.apply_scheme(&n);
            ui.memory_mut(|m| m.close_popup());
        }
        if let Some(n) = del {
            self.s.work.schemes.retain(|s| s.name != n);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let id = egui::Id::new("work-scheme-name");
            let mut name: String = ui.memory(|m| m.data.get_temp(id)).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(t("保存当前布局…")).desired_width(150.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.button(t("保存")).clicked() || enter {
                self.save_scheme(&name);
                name.clear();
            }
            ui.memory_mut(|m| m.data.insert_temp(id, name));
        });
        ui.add_space(4.0);
        ui.separator();
        if menu_item(ui, t("重置布局（回到单页）"), "") {
            self.reset_layout();
            ui.memory_mut(|m| m.close_popup());
        }
        let mut f = self.work.focus;
        if ui.checkbox(&mut f, t("专注模式：暂停自动弹出")).changed() {
            self.work.focus = f;
        }
    }

    // ------------------------------------------------------------ 来回切页提示

    pub(super) fn pair_toast(&mut self, ctx: &egui::Context) {
        let Some(age) = self.work.pair.as_ref().map(|h| h.at.elapsed().as_secs_f32()) else { return };
        let hover_id = egui::Id::new("work-pair-hover");
        let hovered: bool = ctx.memory(|m| m.data.get_temp(hover_id)).unwrap_or(false);
        if age > 8.0 && !hovered {
            self.work.pair = None;
            return;
        }
        if hovered {
            // 鼠标停在上面时不消失：重新计时
            if let Some(h) = &mut self.work.pair {
                h.at = Instant::now() - Duration::from_secs(2);
            }
        }
        let a = ((8.0 - age).min(1.0) * (age * 6.0).min(1.0)).clamp(0.0, 1.0);
        let a = if hovered { 1.0 } else { a };
        let Some((main, other, ask_off)) = self.work.pair.as_ref().map(|h| (h.main, h.other, h.ask_off)) else {
            return;
        };
        let screen = ctx.screen_rect();
        let mut yes = false;
        let mut no = false;
        let resp = egui::Area::new(egui::Id::new("work-pair"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -64.0))
            .show(ctx, |ui| {
                ui.multiply_opacity(a);
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE_HI()))
                    .corner_radius(CornerRadius::same(10))
                    .shadow(epaint::Shadow {
                        offset: [0, 8],
                        blur: 28,
                        spread: 0,
                        color: shade(140),
                    })
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 12,
                        top: 8,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width((screen.width() - 80.0).min(620.0));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            // 缩略图：并排后的样子
                            let (r, _) = ui.allocate_exact_size(vec2(44.0, 30.0), Sense::hover());
                            let p = ui.painter();
                            p.rect_stroke(r, 3.0, st(1.0, TEXT3()), egui::StrokeKind::Inside);
                            let split = r.left() + r.width() * 0.6;
                            p.rect_filled(Rect::from_min_max(pos2(split, r.top() + 2.0), r.max - vec2(2.0, 2.0)), 1.0, ACCENT().gamma_multiply(0.5));
                            draw_icon(p, Rect::from_center_size(pos2((r.left() + split) / 2.0, r.center().y), vec2(11.0, 11.0)), main.icon(), TEXT2());
                            if ask_off {
                                ui.label(RichText::new(t("以后都不再提示「放在一起看」？")).size(13.0).color(TEXT()));
                                if ui.add(egui::Button::new(RichText::new(t("都不提醒")).size(12.5))).clicked() {
                                    self.s.work.switch_hint = false;
                                    self.work.pair = None;
                                }
                                if ui.add(egui::Button::new(RichText::new(t("继续提醒")).size(12.5).color(TEXT2())).frame(false)).clicked() {
                                    self.work.pair = None;
                                }
                                return;
                            }
                            ui.label(RichText::new(tf("在{}和{}之间来回切？", &[&main.label(), &other.label()])).size(13.0).color(TEXT()));
                            let b = egui::Button::new(RichText::new(t("放在一起看")).size(12.5).color(BG0())).fill(ACCENT());
                            if ui.add(b).on_hover_text(self.km.label(Act::SideToggle)).clicked() {
                                yes = true;
                            }
                            if ui.add(egui::Button::new(RichText::new(t("不用")).size(12.5).color(TEXT2())).frame(false)).clicked() {
                                no = true;
                            }
                        });
                    });
            })
            .response;
        let over = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| resp.rect.contains(p));
        ctx.memory_mut(|m| m.data.insert_temp(hover_id, over));
        ctx.request_repaint_after(Duration::from_millis(60));
        if yes {
            self.accept_pair();
        } else if no {
            self.decline_pair();
        }
    }

    // ------------------------------------------------------------ 调试副页面的连接下拉

    /// 副页面里的串口：标题栏左边一个「端口 ▾」，点开选端口、连接 / 断开
    pub(super) fn conn_chip(&mut self, ui: &mut egui::Ui) {
        let live = self.conn.is_some();
        let name = if self.s.transport == Transport::Serial {
            if self.s.port.is_empty() {
                t("选择串口").to_string()
            } else {
                self.port_alias(&self.s.port).cloned().unwrap_or_else(|| self.s.port.clone())
            }
        } else {
            self.s.net.label(self.s.transport)
        };
        let text = format!("{}  ▾", truncate(&name, 22));
        let col = if self.connected.is_some() { OK() } else { TEXT2() };
        let r = ui.add(
            egui::Button::new(RichText::new(text).size(12.5).color(TEXT()))
                .min_size(vec2(0.0, 26.0))
                .corner_radius(CornerRadius::same(6)),
        );
        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, col);
        let pop = ui.id().with("conn-chip");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pop));
        }
        egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(240.0);
            if self.s.transport == Transport::Serial {
                if self.ports.is_empty() {
                    ui.label(RichText::new(t("没有找到串口")).color(TEXT3()));
                }
                for p in self.ports.clone() {
                    let on = p.name == self.s.port;
                    let label = match self.port_alias(&p.name) {
                        Some(a) => format!("{a}  ({})", p.name),
                        None => p.name.clone(),
                    };
                    if ui.selectable_label(on, RichText::new(label).monospace()).on_hover_text(&p.desc).clicked() && !on {
                        let was = live;
                        if was {
                            self.disconnect();
                        }
                        self.s.port = p.name.clone();
                        self.connect();
                    }
                }
                ui.separator();
                ui.label(RichText::new(format!("{} {}", self.s.cfg.baud, self.s.cfg.short())).small().color(TEXT3()));
            }
            if menu_item(ui, if live { t("断开") } else { t("连接") }, "") {
                self.toggle_connect();
            }
        });
    }

    /// 打开副页面的入口给其它地方用：自动弹出的封装（按住强度）
    pub(super) fn side_or_page(&mut self, p: Pane, trig: &str, hint: &str) {
        if !self.auto_side(p, trig, Strength::Strong, hint) {
            self.goto_pane(p);
        }
    }
}

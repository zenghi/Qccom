//! 页面之间的导航：全局「后退 / 前进」（Alt+← / Alt+→、鼠标侧键）、跳转后的「← 返回」胶囊、
//! Ctrl+Tab 最近使用切换器（页面、工具、比较、文件混在一起，按住 Ctrl 连按 Tab 选择，松开就切过去）。
//!
//! 每帧比较「现在在哪」：页面或工具变了就把上一帧的位置记进后退栈，所以任何入口（命令面板、
//! 代码页右键比较、日志里点文件…）跳过去都能退回来，不用每个入口单独处理。

use super::toolbox::{self, Tool};
use super::work::Pane;
use super::*;
use std::path::PathBuf;

/// 一个可以回去的位置
#[derive(Clone, PartialEq)]
pub(super) struct Loc {
    page: Page,
    tool: Tool,
    /// 代码页：文件和光标行
    file: Option<(PathBuf, usize)>,
    /// 比较页：标签（None = 首页）
    cmp: Option<usize>,
    /// 副页面（并排时）
    side: Option<Pane>,
    /// Ctrl+Tab 里的布局方案
    scheme: Option<String>,
}

impl Loc {
    /// 粗粒度：页面（工具箱再加上工具）。变了才记进后退栈
    fn coarse(&self) -> (Page, Option<Tool>, Option<Pane>) {
        (self.page, (self.page == Page::Tools).then_some(self.tool), self.side)
    }

    /// 细粒度：再加上文件、比较标签。最近使用列表按这个去重
    fn fine(&self) -> (Page, Option<Tool>, Option<&PathBuf>, Option<usize>, Option<Pane>, Option<&String>) {
        (
            self.page,
            (self.page == Page::Tools).then_some(self.tool),
            self.file.as_ref().map(|f| &f.0),
            self.cmp,
            self.side,
            self.scheme.as_ref(),
        )
    }
}

#[derive(Default)]
pub(super) struct Nav {
    back: Vec<Loc>,
    fwd: Vec<Loc>,
    /// 上一帧的位置
    last: Option<Loc>,
    /// 最近使用（第一个是当前位置）
    mru: Vec<Loc>,
    /// 「← 返回」胶囊指向的位置
    from: Option<Loc>,
    /// 这一次切换是用户自己点的（活动栏、快捷键），不显示返回胶囊
    manual: bool,
    /// 这一次切换是后退 / 前进造成的，不再记进后退栈
    replaying: bool,
    /// 进入代码页时编辑器后退 / 前进栈的长度：比它长的部分归编辑器自己
    code_mark: (usize, usize),
    /// Ctrl+Tab 切换器：选中的下标
    switcher: Option<usize>,
}

impl App {
    fn nav_here(&self) -> Loc {
        Loc {
            page: self.page,
            tool: self.s.tb.tool,
            file: (self.page == Page::Code).then(|| self.code.here()).flatten(),
            cmp: (self.page == Page::Compare).then(|| self.cmpv.active_tab()).flatten(),
            side: self.side_pane(),
            scheme: None,
        }
    }

    /// 副页面是系统自动打开的：当成没变过，不进后退记录
    pub(super) fn nav_sync_side(&mut self) {
        let side = self.side_pane();
        if let Some(l) = &mut self.nav.last {
            l.side = side;
        }
        if let Some(m) = self.nav.mru.first_mut() {
            m.side = side;
        }
    }

    /// 活动栏、页面快捷键这类「用户自己切的」调用前先标一下
    pub(super) fn nav_manual(&mut self) {
        self.nav.manual = true;
    }

    /// 每帧调用：记录位置变化，处理 Alt+←/→、鼠标侧键、Ctrl+Tab
    pub(super) fn nav_tick(&mut self, ctx: &egui::Context) {
        let here = self.nav_here();
        // 设置、社区有自己的「返回」，不进导航历史
        let tracked = |p: Page| !matches!(p, Page::Settings | Page::Community);
        if let Some(last) = self.nav.last.take() {
            let moved = last.coarse() != here.coarse();
            if moved && tracked(last.page) && tracked(here.page) {
                if !self.nav.replaying {
                    self.nav.back.push(last.clone());
                    if self.nav.back.len() > 50 {
                        self.nav.back.remove(0);
                    }
                    self.nav.fwd.clear();
                    self.nav.from = (!self.nav.manual).then_some(last);
                }
                if here.page == Page::Code {
                    self.nav.code_mark = self.code.nav_lens();
                }
            }
            if moved {
                self.nav.manual = false;
                self.nav.replaying = false;
            }
        }
        if tracked(here.page) && self.nav.mru.first().map(|m| m.fine()) != Some(here.fine()) {
            self.nav.mru.retain(|m| m.fine() != here.fine());
            self.nav.mru.insert(0, here.clone());
            self.nav.mru.truncate(20);
        } else if let Some(m) = self.nav.mru.first_mut() {
            // 同一个文件里移动光标：更新行号
            m.file = here.file.clone();
        }
        self.nav.last = Some(here);

        // 鼠标侧键
        let (b4, b5) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        if b4 {
            self.nav_go(true);
        }
        if b5 {
            self.nav_go(false);
        }
        self.switcher_keys(ctx);
    }

    /// Alt+← / Alt+→：代码页里先用编辑器自己的跳转历史，用完了再回到上一个页面
    pub(super) fn nav_go(&mut self, back: bool) {
        if self.page == Page::Code {
            let (b, f) = self.code.nav_lens();
            let own = if back { b > self.nav.code_mark.0 } else { f > self.nav.code_mark.1 };
            if own {
                self.code.nav_step(back);
                return;
            }
        }
        let target = if back { self.nav.back.pop() } else { self.nav.fwd.pop() };
        let Some(to) = target else {
            // 没有页面历史了：代码页里还可以继续用编辑器的
            if self.page == Page::Code {
                self.code.nav_step(back);
            }
            return;
        };
        let here = self.nav_here();
        if back {
            self.nav.fwd.push(here);
        } else {
            self.nav.back.push(here);
        }
        self.nav.from = None;
        self.nav.replaying = true;
        self.nav_restore(&to);
    }

    fn nav_restore(&mut self, to: &Loc) {
        match to.page {
            Page::Tools => self.open_tool(to.tool),
            p => self.set_page(p),
        }
        self.nav_restore_side(to.side);
        if let Some((f, l)) = &to.file {
            if f.is_file() {
                self.code.goto_loc(f, *l);
            }
        }
        if to.page == Page::Compare {
            self.cmpv.set_active_tab(to.cmp);
        }
    }

    /// 回到某个位置时副页面也跟着还原
    fn nav_restore_side(&mut self, side: Option<Pane>) {
        if self.side_pane() == side {
            return;
        }
        match side {
            Some(p) => {
                if self.main_pane() != Some(p) {
                    self.open_side(p, None, true);
                }
            }
            None => {
                self.work.hidden = std::mem::take(&mut self.work.tabs);
                self.work.auto = None;
                self.work.manual = false;
            }
        }
    }

    fn loc_label(&self, l: &Loc) -> (Icon, String, String) {
        if let Some(name) = &l.scheme {
            return (Icon::DockRight, name.clone(), t("布局方案").into());
        }
        let (icon, name, sub) = self.loc_label_page(l);
        match l.side {
            Some(p) => (icon, format!("{name} + {}", p.label()), sub),
            None => (icon, name, sub),
        }
    }

    fn loc_label_page(&self, l: &Loc) -> (Icon, String, String) {
        match l.page {
            Page::Debug => (Icon::Pulse, t("调试").into(), t("串口日志").into()),
            Page::Code => match &l.file {
                Some((f, line)) => {
                    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let root = self.code.root().map(|r| r.to_path_buf()).unwrap_or_default();
                    let dir = f.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
                    (
                        Icon::Code,
                        name,
                        if dir.is_empty() {
                            tf("第 {} 行", &[&(line + 1)])
                        } else {
                            format!("{dir} · {}", tf("第 {} 行", &[&(line + 1)]))
                        },
                    )
                }
                None => (Icon::Code, t("代码").into(), String::new()),
            },
            Page::Git => (Icon::Branch, "Git".into(), String::new()),
            Page::Compare => match l.cmp.and_then(|i| self.cmpv.tab_title(i)) {
                Some(title) => (Icon::Compare, title, t("比较").into()),
                None => (Icon::Compare, t("比较").into(), String::new()),
            },
            Page::Tools => (Icon::Toolbox, toolbox::tool_text(l.tool).0.to_string(), t("工具箱").into()),
            Page::Settings => (Icon::Gear, t("设置").into(), String::new()),
            Page::Community => (Icon::Gear, t("社区与反馈").into(), String::new()),
        }
    }

    /// 从别处跳过来时，左下角的「← 返回 xxx」。手动切页后消失
    pub(super) fn back_chip(&mut self, ctx: &egui::Context) {
        let Some(from) = self.nav.from.clone() else { return };
        if self.nav.switcher.is_some() {
            return;
        }
        let (_, name, _) = self.loc_label(&from);
        let screen = ctx.screen_rect();
        let keys = self.km.label(Act::NavBack);
        let mut go = false;
        let mut close = false;
        egui::Area::new(egui::Id::new("nav-back-chip"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.left() + 60.0, screen.bottom() - 64.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE_HI()))
                    .corner_radius(CornerRadius::same(16))
                    .shadow(epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: shade(90),
                    })
                    .inner_margin(egui::Margin { left: 6, right: 4, top: 3, bottom: 3 })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let txt = format!("←  {}", tf("返回 {}", &[&name]));
                            let b = egui::Button::new(RichText::new(txt).size(12.5).color(TEXT())).frame(false).min_size(vec2(0.0, 24.0));
                            let r = ui.add(b).on_hover_text(tf("回到跳转前的位置（{}，或鼠标侧键）", &[&keys]));
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if r.clicked() {
                                go = true;
                            }
                            if !keys.is_empty() {
                                ui.label(RichText::new(&keys).size(11.0).color(TEXT3()));
                            }
                            let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                            draw_icon(ui.painter(), r.shrink(5.0), Icon::Close, if resp.hovered() { TEXT() } else { TEXT3() });
                            if resp.clicked() {
                                close = true;
                            }
                        });
                    });
            });
        if go {
            self.nav_go(true);
        } else if close {
            self.nav.from = None;
        }
    }

    /// 切换器的候选：最近使用 + 还没出现的已打开文件
    fn switcher_items(&self) -> Vec<Loc> {
        let mut v: Vec<Loc> = self.nav.mru.clone();
        // 当前位置放第一个（Ctrl+Tab 一下就是「上一个」）
        let here = self.nav_here();
        v.retain(|l| l.fine() != here.fine());
        v.insert(0, here);
        for f in self.code.open_files() {
            if !v.iter().any(|l| l.page == Page::Code && l.file.as_ref().is_some_and(|x| x.0 == f)) {
                v.push(Loc {
                    page: Page::Code,
                    tool: self.s.tb.tool,
                    file: Some((f, 0)),
                    cmp: None,
                    side: None,
                    scheme: None,
                });
            }
        }
        // 有具体文件的代码页条目时，不再列「代码」空页
        if v.iter().any(|l| l.page == Page::Code && l.file.is_some()) {
            v.retain(|l| l.page != Page::Code || l.file.is_some() || l.side.is_some());
        }
        v.truncate(14);
        // 常用组合和布局方案
        for (a, b) in self.s.work.combos.clone() {
            let Some((page, tool)) = pane_page(a) else { continue };
            if v.iter().any(|l| l.page == page && l.side == Some(b)) {
                continue;
            }
            v.push(Loc {
                page,
                tool: tool.unwrap_or(self.s.tb.tool),
                file: None,
                cmp: None,
                side: Some(b),
                scheme: None,
            });
        }
        for sc in self.all_schemes() {
            v.push(Loc {
                page: self.page,
                tool: self.s.tb.tool,
                file: None,
                cmp: None,
                side: None,
                scheme: Some(sc.name),
            });
        }
        v.truncate(22);
        v
    }

    fn switcher_keys(&mut self, ctx: &egui::Context) {
        let (fwd, back, ctrl, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::CTRL, Key::Tab),
                i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab),
                i.modifiers.ctrl,
                i.key_pressed(Key::Escape),
            )
        });
        if fwd || back {
            let n = self.switcher_items().len();
            if n < 2 {
                return;
            }
            let cur = self.nav.switcher.unwrap_or(0) as isize;
            let next = (cur + if back { -1 } else { 1 }).rem_euclid(n as isize) as usize;
            self.nav.switcher = Some(next);
            ctx.request_repaint();
            return;
        }
        let Some(sel) = self.nav.switcher else { return };
        if esc {
            self.nav.switcher = None;
            return;
        }
        if !ctrl {
            // 松开 Ctrl：切过去
            self.nav.switcher = None;
            if let Some(to) = self.switcher_items().get(sel).cloned() {
                self.nav_open(&to);
            }
        }
    }

    /// 切换器选中的位置（普通跳转，会记进后退栈，但不显示返回胶囊）
    fn nav_open(&mut self, to: &Loc) {
        self.nav_manual();
        if let Some(name) = &to.scheme {
            self.apply_scheme(name);
            return;
        }
        match to.page {
            Page::Tools => self.open_tool(to.tool),
            p => self.set_page(p),
        }
        if to.side.is_some() {
            self.nav_restore_side(to.side);
        }
        if let Some((f, l)) = &to.file {
            // 已打开的文件只切标签、保留它自己的光标；关掉了的按当时的行重新打开
            if self.code.open_files().contains(f) {
                self.code.open_file(f, None);
            } else if f.is_file() {
                self.code.goto_loc(f, *l);
            }
        }
        if to.page == Page::Compare {
            self.cmpv.set_active_tab(to.cmp);
        }
    }

    /// Ctrl+Tab 弹出的列表（居中）
    pub(super) fn switcher_ui(&mut self, ctx: &egui::Context) {
        let Some(sel) = self.nav.switcher else { return };
        let items = self.switcher_items();
        let screen = ctx.screen_rect();
        let w = 460.0f32.min(screen.width() - 40.0);
        let mut click: Option<usize> = None;
        let mut hover: Option<usize> = None;
        egui::Area::new(egui::Id::new("nav-switcher"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - w / 2.0, screen.top() + 90.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE_HI()))
                    .corner_radius(CornerRadius::same(10))
                    .shadow(epaint::Shadow {
                        offset: [0, 12],
                        blur: 40,
                        spread: 0,
                        color: shade(200),
                    })
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            ui.label(RichText::new(t("最近使用")).size(11.5).color(TEXT3()));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(6.0);
                                ui.label(RichText::new(t("按住 Ctrl 连按 Tab 选择，松开切换")).size(11.0).color(TEXT3()));
                            });
                        });
                        ui.add_space(4.0);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, l) in items.iter().enumerate() {
                            let (icon, name, sub) = self.loc_label(l);
                            let (r, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
                            let on = i == sel;
                            let p = ui.painter();
                            if on {
                                p.rect_filled(r, CornerRadius::same(6), ACCENT().gamma_multiply(0.16));
                            } else if resp.hovered() {
                                p.rect_filled(r, CornerRadius::same(6), BG3());
                            }
                            let ic = Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(16.0, 16.0));
                            if l.page == Page::Code && l.file.is_some() {
                                crate::codeview::file_badge(p, ic.center(), &name);
                            } else {
                                draw_icon(p, ic, icon, if on { ACCENT() } else { TEXT2() });
                            }
                            let g = p.layout_no_wrap(name, FontId::proportional(13.5), TEXT());
                            let gw = g.size().x;
                            p.galley(pos2(r.left() + 40.0, r.center().y - g.size().y / 2.0), g, TEXT());
                            if !sub.is_empty() {
                                p.text(pos2(r.left() + 50.0 + gw, r.center().y), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), TEXT3());
                            }
                            if i == 0 {
                                p.text(pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, t("当前"), FontId::proportional(11.0), TEXT3());
                            }
                            if resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                                hover = Some(i);
                            }
                            if resp.clicked() {
                                click = Some(i);
                            }
                        }
                    });
            });
        if let Some(i) = hover {
            self.nav.switcher = Some(i);
        }
        if let Some(i) = click {
            self.nav.switcher = None;
            if let Some(to) = items.get(i).cloned() {
                self.nav_open(&to);
            }
        }
    }

    /// 工具箱图标的悬停提示：带上最近用过的工具
    pub(super) fn tools_tip(&self) -> String {
        let mut s = t("工具箱：文件服务、设备发现、数据转换、CAN、HID").to_string();
        let recent: Vec<&str> = self.s.tb.recent.iter().take(4).map(|t| toolbox::tool_text(*t).0).collect();
        if !recent.is_empty() {
            s.push_str(&format!("\n{}：{}", t("最近"), recent.join("、")));
        }
        s
    }
}

/// 页面对应的 (Page, 工具)；「文件」不是页面
fn pane_page(p: Pane) -> Option<(Page, Option<Tool>)> {
    Some(match p {
        Pane::Debug => (Page::Debug, None),
        Pane::Code => (Page::Code, None),
        Pane::Git => (Page::Git, None),
        Pane::Compare => (Page::Compare, None),
        Pane::Community => (Page::Community, None),
        Pane::Files => return None,
        Pane::Tool(t) => (Page::Tools, Some(t)),
    })
}

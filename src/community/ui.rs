//! 社区页界面：左边导航、中间帖子列表、右边详情 / 发帖 / 账号。窗口窄于 900 时变成两级（列表 ↔ 详情）。
//!
//! 键盘：J / K 上下选帖、Enter 打开、V 投票、R 回复、/ 搜索、N 发帖、Esc 返回；输入框里 Ctrl+Enter 发送。

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Layout, Rect, RichText, Sense, Ui};
use serde_json::json;

use super::widgets as w;
use super::*;
use crate::app::{draw_icon, icon_button, Icon};
use crate::theme::*;

const NAV_W: f32 = 176.0;
const NAV_W_SLIM: f32 = 54.0;

fn search_id() -> egui::Id {
    egui::Id::new("comm-search")
}
fn reply_id() -> egui::Id {
    egui::Id::new("comm-reply")
}
fn title_id() -> egui::Id {
    egui::Id::new("comm-title")
}

fn notif_text(n: &Notif) -> String {
    let who = if n.actor.is_empty() { t("有人").to_string() } else { n.actor.clone() };
    match n.kind.as_str() {
        "reply" => tf("{} 回复了你的帖子", &[&who]),
        "quote" => tf("{} 引用了你的回复", &[&who]),
        "solved" => tf("{} 采纳了你的回复", &[&who]),
        "status" => {
            let mut it = n.extra.splitn(2, '|');
            let s = it.next().unwrap_or_default();
            let v = it.next().unwrap_or_default();
            if s == "done" && !v.is_empty() {
                tf("你关注的需求已在 {} 完成", &[&v])
            } else {
                tf("你关注的需求状态变为「{}」", &[&status_name(s)])
            }
        }
        _ => t("新消息").to_string(),
    }
}

impl Community {
    /// 整个社区页
    pub fn ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| self.ui_in(ui, false));
    }

    /// compact = 副页面：不要左边的板块栏，列表和详情来回切
    pub fn ui_in(&mut self, ui: &mut egui::Ui, compact: bool) {
        let ctx = &ui.ctx().clone();
        self.activate(ctx);
        let total_w = ui.available_width();
        let narrow = total_w < 900.0;
        if !compact || total_w > 700.0 {
            self.keys(ctx, narrow);
        }

        if !compact {
            egui::SidePanel::left("comm-nav")
                .resizable(false)
                .exact_width(if narrow { NAV_W_SLIM } else { NAV_W })
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(if narrow { 7 } else { 10 }, 12)))
                .show_inside(ui, |ui| self.nav_ui(ui, narrow));
        }

        if narrow {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show_inside(ui, |ui| {
                // 窄窗口里列表和详情来回切：淡入一下，不至于“啪”地跳过去
                let open = self.pane != Pane::Empty;
                let k = ctx.animate_bool_with_time(egui::Id::new("comm-narrow-pane"), open, 0.16);
                ui.multiply_opacity(if open { 0.35 + 0.65 * k } else { 0.35 + 0.65 * (1.0 - k) });
                if open {
                    self.pane_ui(ui, true);
                } else {
                    self.list_ui(ui);
                }
            });
        } else {
            egui::SidePanel::left("comm-list")
                .resizable(true)
                .default_width(((total_w - NAV_W) * 0.40).clamp(320.0, 460.0))
                .width_range(300.0..=560.0)
                .frame(egui::Frame::new().fill(BG0()))
                .show_inside(ui, |ui| self.list_ui(ui));
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show_inside(ui, |ui| self.pane_ui(ui, false));
        }
        self.lightbox_ui(ctx);
    }

    // ------------------------------------------------------------ 键盘

    fn keys(&mut self, ctx: &egui::Context, narrow: bool) {
        if self.lightbox.is_some() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.lightbox = None;
            }
            return;
        }
        if ctx.wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                if ctx.memory(|m| m.has_focus(search_id())) && !self.query.is_empty() {
                    self.query.clear();
                } else {
                    ctx.memory_mut(|m| m.surrender_focus(m.focused().unwrap_or(egui::Id::NULL)));
                }
            }
            return;
        }
        let (j, k, enter, v, r, slash, n, esc) = ctx.input(|i| {
            let plain = !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.command;
            (
                plain && i.key_pressed(Key::J),
                plain && i.key_pressed(Key::K),
                i.key_pressed(Key::Enter),
                plain && i.key_pressed(Key::V),
                plain && i.key_pressed(Key::R),
                i.key_pressed(Key::Slash),
                plain && i.key_pressed(Key::N),
                i.key_pressed(Key::Escape),
            )
        });
        if j || k || v || r || slash || n {
            // 快捷键打出的字母不要再落进刚获得焦点的输入框
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
        }
        if self.nav == Nav::Notifs {
            return;
        }
        let len = self.items.len();
        if (j || k) && len > 0 {
            let cur = self.sel.or_else(|| match self.pane {
                Pane::Post(id) => self.items.iter().position(|p| p.id == id),
                _ => None,
            });
            let next = match cur {
                None => 0,
                Some(c) if j => (c + 1).min(len - 1),
                Some(c) => c.saturating_sub(1),
            };
            self.sel = Some(next);
            if !narrow || self.pane != Pane::Empty {
                let id = self.items[next].id;
                self.open_post(id);
            }
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("comm-sel-scroll"), true));
            if j && next + 3 >= len {
                self.load_more();
            }
        }
        if enter {
            if let Some(p) = self.sel.and_then(|i| self.items.get(i)) {
                let id = p.id;
                self.open_post(id);
            }
        }
        if v {
            let id = match self.pane {
                Pane::Post(id) => Some(id),
                _ => self.sel.and_then(|i| self.items.get(i)).map(|p| p.id),
            };
            if let Some(id) = id {
                let board = self.items.iter().find(|p| p.id == id).map(|p| p.board.clone()).or(self.detail.as_ref().map(|d| d.board.clone()));
                if board.as_deref() == Some("idea") {
                    self.vote(id);
                }
            }
        }
        if r && matches!(self.pane, Pane::Post(_)) {
            ctx.memory_mut(|m| m.request_focus(reply_id()));
        }
        if slash {
            ctx.memory_mut(|m| m.request_focus(search_id()));
        }
        if n {
            self.new_post(None);
            ctx.memory_mut(|m| m.request_focus(title_id()));
        }
        if esc && self.pane != Pane::Empty {
            self.close_pane();
        }
    }

    // ------------------------------------------------------------ 左边导航

    fn nav_ui(&mut self, ui: &mut Ui, slim: bool) {
        ui.spacing_mut().item_spacing.y = 2.0;
        if !slim {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(t("社区")).font(FontId::new(16.0, bold())).color(TEXT()));
                if self.offline {
                    ui.add_space(4.0);
                    w::tag(ui, t("离线"), WARN());
                }
            });
            ui.add_space(10.0);
            let r = ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(rect, CornerRadius::same(7), if resp.hovered() { accent_hover() } else { ACCENT() });
                let c = rect.center();
                p.line_segment([c + vec2(-34.0, 0.0), c + vec2(-26.0, 0.0)], st(1.8, ON_ACCENT()));
                p.line_segment([c + vec2(-30.0, -4.0), c + vec2(-30.0, 4.0)], st(1.8, ON_ACCENT()));
                p.text(c + vec2(6.0, 0.0), Align2::CENTER_CENTER, t("发帖"), FontId::new(13.0, bold()), ON_ACCENT());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                resp
            });
            if r.inner.on_hover_text(t("发帖（N）")).clicked() {
                self.new_post(None);
                ui.ctx().memory_mut(|m| m.request_focus(title_id()));
            }
            ui.add_space(14.0);
            section(ui, t("板块"));
        } else {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(rect, CornerRadius::same(7), if resp.hovered() { accent_hover() } else { ACCENT() });
            let c = rect.center();
            p.line_segment([c + vec2(-6.0, 0.0), c + vec2(6.0, 0.0)], st(2.0, ON_ACCENT()));
            p.line_segment([c + vec2(0.0, -6.0), c + vec2(0.0, 6.0)], st(2.0, ON_ACCENT()));
            if resp.on_hover_text(t("发帖（N）")).clicked() {
                self.new_post(None);
            }
            ui.add_space(10.0);
        }
        let notice_new = self.notice.as_ref().is_some_and(|n| n.id > self.seen_notice);
        for b in Board::ALL {
            let on = self.nav == Nav::Board(b);
            let dot = b == Board::Notice && notice_new;
            let resp = nav_row(ui, slim, on, b.name(), None, dot, |p, r, c| w::board_icon(p, r, b, c));
            if let (false, false, Some(n)) = (slim, dot, self.board_counts.get(&b).filter(|n| **n > 0)) {
                ui.painter().text(
                    pos2(resp.rect.right() - 10.0, resp.rect.center().y),
                    Align2::RIGHT_CENTER,
                    if *n > 999 { "999+".to_string() } else { n.to_string() },
                    FontId::new(11.0, FontFamily::Monospace),
                    TEXT3(),
                );
            }
            if resp.on_hover_text(b.desc()).clicked() {
                if b == Board::Notice {
                    if let Some(n) = &self.notice {
                        self.seen_notice = self.seen_notice.max(n.id);
                    }
                }
                self.go(Nav::Board(b));
                if slim {
                    self.pane = Pane::Empty;
                }
            }
        }
        ui.add_space(12.0);
        if !slim {
            section(ui, t("我的"));
        } else {
            ui.separator();
        }
        // 「我的」只占一行，进去后在列表顶部切换 我发的 / 我投的 / 我回复的
        let on = matches!(self.nav, Nav::Mine(_));
        let resp = nav_row(ui, slim, on, t("我的内容"), None, false, |p, r, c| w::person_icon(p, r, c));
        if resp.on_hover_text(t("我发的、我投的、我回复的")).clicked() {
            if !on {
                self.go(Nav::Mine(Mine::Posted));
            }
            if slim {
                self.pane = Pane::Empty;
            }
        }
        let unread = (self.unread > 0).then_some(self.unread);
        let resp = nav_row(ui, slim, self.nav == Nav::Notifs, t("提醒"), unread, false, |p, r, c| w::bell_icon(p, r, c));
        if resp.on_hover_text(t("提醒")).clicked() {
            self.go(Nav::Notifs);
            if slim {
                self.pane = Pane::Empty;
            }
        }

        // 底部：账号
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            let (seed, name, tex, sub) = match &self.me_user {
                Some(u) => (format!("u{}", u.id), u.nickname.clone(), u.avatar.clone(), format!("@{}", u.username)),
                None => (
                    hub::creds().device_id,
                    if self.device_name.is_empty() { t("匿名用户").to_string() } else { self.device_name.clone() },
                    None,
                    t("登录 / 注册").to_string(),
                ),
            };
            let tex = tex.and_then(|id| self.image(ui.ctx(), &id)).map(|x| x.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
            let on = self.pane == Pane::Account;
            let p = ui.painter();
            if on || resp.hovered() {
                p.rect_filled(rect, CornerRadius::same(7), if on { BG3() } else { BG2() });
            }
            let ar = Rect::from_center_size(pos2(rect.left() + if slim { rect.width() / 2.0 } else { 20.0 }, rect.center().y), vec2(26.0, 26.0));
            w::paint_avatar(p, ar, &seed, tex);
            if !slim {
                let x = ar.right() + 9.0;
                let tw = rect.right() - x - 6.0;
                let g = p.layout_no_wrap(crate::app::truncate(&name, 12), FontId::new(12.5, bold()), TEXT());
                p.galley(pos2(x, rect.center().y - 15.0), g, TEXT());
                let _ = tw;
                p.text(
                    pos2(x, rect.center().y + 7.0),
                    Align2::LEFT_CENTER,
                    sub,
                    FontId::proportional(11.0),
                    if self.logged_in() { TEXT3() } else { ACCENT() },
                );
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.on_hover_text(t("账号")).clicked() {
                self.pane = Pane::Account;
            }
            ui.add_space(4.0);
            ui.separator();
        });
    }

    // ------------------------------------------------------------ 中间列表

    fn list_ui(&mut self, ui: &mut Ui) {
        let (title, desc) = match self.nav {
            Nav::Board(b) => (b.name().to_string(), b.desc().to_string()),
            Nav::Mine(_) => (t("我的内容").to_string(), self.mine_hint()),
            Nav::Notifs => (t("提醒").to_string(), t("回复、引用、采纳和需求状态变化").to_string()),
        };
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 16,
                right: 14,
                top: 14,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&title).font(FontId::new(18.0, bold())).color(TEXT()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.nav == Nav::Notifs {
                            if self.unread > 0 && w::link(ui, t("全部已读"), ACCENT()).clicked() {
                                self.mark_all_read();
                            }
                        } else if icon_button(ui, Icon::Refresh, t("刷新"), false).clicked() {
                            self.reload();
                            self.refresh_me();
                        }
                    });
                });
                ui.label(RichText::new(desc).size(12.0).color(TEXT3()));
                if self.nav == Nav::Notifs {
                    return;
                }
                ui.add_space(10.0);
                // 搜索
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
                let focused = ui.memory(|m| m.has_focus(search_id()));
                ui.painter().rect(
                    rect,
                    CornerRadius::same(7),
                    BG1(),
                    st(1.0, if focused { ACCENT().gamma_multiply(0.6) } else { LINE() }),
                    egui::StrokeKind::Inside,
                );
                draw_icon(ui.painter(), Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), vec2(14.0, 14.0)), Icon::Search, TEXT3());
                let inner = Rect::from_min_max(pos2(rect.left() + 28.0, rect.top() + 1.0), pos2(rect.right() - 30.0, rect.bottom() - 1.0));
                ui.put(
                    inner,
                    egui::TextEdit::singleline(&mut self.query)
                        .id(search_id())
                        .hint_text(t("搜索标题和内容"))
                        .frame(false)
                        .margin(vec2(4.0, 6.0))
                        .font(FontId::proportional(13.0)),
                );
                let kr = Rect::from_center_size(pos2(rect.right() - 15.0, rect.center().y), vec2(16.0, 17.0));
                if self.query.is_empty() {
                    ui.scope_builder(egui::UiBuilder::new().max_rect(kr), |ui| w::kbd(ui, "/"));
                } else {
                    // 自己画 ×：按钮控件最小高度比这里大，会把下面一行顶下去
                    let xr = ui.interact(kr, ui.id().with("comm-q-clear"), Sense::click());
                    let c = if xr.hovered() { TEXT() } else { TEXT3() };
                    let m = kr.center();
                    ui.painter().line_segment([m + vec2(-3.5, -3.5), m + vec2(3.5, 3.5)], st(1.3, c));
                    ui.painter().line_segment([m + vec2(-3.5, 3.5), m + vec2(3.5, -3.5)], st(1.3, c));
                    if xr.on_hover_text(t("清空")).clicked() {
                        self.query.clear();
                    }
                }
                ui.add_space(8.0);
                if let Nav::Mine(cur) = self.nav {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (m, name) in [(Mine::Posted, t("我发的")), (Mine::Voted, t("我投的")), (Mine::Replied, t("我回复的"))] {
                            if seg(ui, name, cur == m).clicked() && cur != m {
                                self.go(Nav::Mine(m));
                            }
                        }
                    });
                }
                // 排序 / 筛选
                if let Nav::Board(b) = self.nav {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let opts: &[(Sort, &str)] = match b {
                            Board::Idea => &[(Sort::Hot, t("热门")), (Sort::Top, t("票数")), (Sort::New, t("最新"))],
                            Board::Notice => &[(Sort::New, t("最新"))],
                            _ => &[(Sort::Active, t("最近回复")), (Sort::New, t("最新")), (Sort::Hot, t("热门"))],
                        };
                        let cur = self.cur_sort();
                        for (s, name) in opts {
                            if seg(ui, name, cur == *s).clicked() && cur != *s {
                                self.set_sort(*s);
                            }
                        }
                        if b == Board::Idea {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let f = self.idea_filter;
                                let name = |f: IdeaFilter| match f {
                                    IdeaFilter::Open => t("进行中"),
                                    IdeaFilter::Planned => t("计划中"),
                                    IdeaFilter::Done => t("已完成"),
                                    IdeaFilter::All => t("全部"),
                                };
                                // 从右往左排，所以倒着放
                                for x in [IdeaFilter::All, IdeaFilter::Done, IdeaFilter::Planned, IdeaFilter::Open] {
                                    let r = seg(ui, name(x), f == x);
                                    let r = if x == IdeaFilter::Open { r.on_hover_text(t("待评估和计划中")) } else { r };
                                    if r.clicked() && f != x {
                                        self.idea_filter = x;
                                        let key = self.list_key();
                                        self.items = self.cache.get(&key).cloned().unwrap_or_default();
                                        self.reload();
                                    }
                                }
                            });
                        }
                    });
                }
            });
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), st(1.0, LINE()));

        if self.offline {
            egui::Frame::new().fill(WARN().gamma_multiply(0.10)).inner_margin(egui::Margin::symmetric(16, 7)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("连不上社区服务器，显示的是上次的内容")).size(12.0).color(WARN()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::link(ui, t("重试"), ACCENT()).clicked() {
                            self.reload();
                            self.refresh_me();
                        }
                    });
                });
            });
        }

        if self.nav == Nav::Notifs {
            self.notifs_ui(ui);
            return;
        }

        // 有内容时出错（刷新、加载更多失败）：顶部一条，不清掉已有内容
        let bar_err = if self.offline || self.items.is_empty() { None } else { self.list_err.clone() };
        let skip_note = (self.skipped > 0).then(|| (tf("有 {} 条内容没显示（客户端可能需要更新）", &[&self.skipped]), self.skip_err.clone()));
        if let Some((msg, tip)) = bar_err.map(|e| (e, String::new())).or(skip_note) {
            egui::Frame::new().fill(ERR().gamma_multiply(0.08)).inner_margin(egui::Margin::symmetric(16, 7)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let r = ui.add(egui::Label::new(RichText::new(&msg).size(12.0).color(ERR())).truncate());
                    if !tip.is_empty() {
                        r.on_hover_text(&tip);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::link(ui, t("重试"), ACCENT()).clicked() {
                            self.reload();
                        }
                    });
                });
            });
        }

        egui::ScrollArea::vertical().id_salt(("comm-list", self.list_key())).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.items.is_empty() {
                if self.loading {
                    for _ in 0..6 {
                        w::skel_card(ui);
                    }
                } else if let Some(e) = self.list_err.clone() {
                    if empty(ui, Art::Error, t("加载失败"), &e, Some(t("重试"))) {
                        self.reload();
                    }
                } else {
                    let art = match self.nav {
                        _ if !self.query.trim().is_empty() => Art::Search,
                        Nav::Board(b) => Art::Board(b),
                        _ => Art::Board(Board::Share),
                    };
                    let (h, d, a) = match self.nav {
                        _ if !self.query.trim().is_empty() => (t("没有找到相关内容"), t("换个关键词试试，或者直接发一个").to_string(), Some(t("用这个标题发帖"))),
                        Nav::Board(Board::Idea) => (t("还没有需求"), t("想要什么功能？提一个，让大家投票").to_string(), Some(t("提需求"))),
                        Nav::Board(Board::Notice) => (t("还没有公告"), String::new(), None),
                        Nav::Board(_) => (t("这里还空着"), t("来发第一个帖子").to_string(), Some(t("发帖"))),
                        _ => (t("还没有内容"), String::new(), None),
                    };
                    if self.nav == Nav::Board(Board::Idea) && self.idea_filter == IdeaFilter::Open && self.hidden_other > 0 && self.query.trim().is_empty() {
                        let d = tf("没有待评估或计划中的需求，已完成和不做的还有 {} 条", &[&self.hidden_other]);
                        if empty(ui, Art::Done, t("需求都处理完了"), &d, Some(t("查看全部"))) {
                            self.idea_filter = IdeaFilter::All;
                            self.reload();
                        }
                    } else if empty(ui, art, h, &d, a) {
                        let q = self.query.trim().to_string();
                        self.new_post(None);
                        if !q.is_empty() && self.draft.title.is_empty() {
                            self.draft.title = q;
                        }
                    }
                }
                return;
            }
            let scroll_sel = ui.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("comm-sel-scroll"))).unwrap_or(false);
            let items = self.items.clone();
            let open = match self.pane {
                Pane::Post(id) => Some(id),
                _ => None,
            };
            for (i, p) in items.iter().enumerate() {
                let selected = open == Some(p.id) || (open.is_none() && self.sel == Some(i));
                let resp = self.card(ui, p, selected);
                if selected && scroll_sel {
                    resp.scroll_to_me(None);
                }
                if resp.clicked() {
                    self.sel = Some(i);
                    self.open_post(p.id);
                }
                let admin = self.is_admin();
                resp.context_menu(|ui| {
                    ui.set_min_width(170.0);
                    if ui.button(t("打开")).clicked() {
                        self.sel = Some(i);
                        self.open_post(p.id);
                        ui.close_menu();
                    }
                    if Board::from_id(&p.board) == Board::Idea && ui.button(if p.voted { t("取消投票") } else { t("投一票") }).clicked() {
                        self.vote(p.id);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(t("复制标题")).clicked() {
                        ui.ctx().copy_text(p.title.clone());
                        ui.close_menu();
                    }
                    if ui.button(t("复制帖子编号和标题")).clicked() {
                        ui.ctx().copy_text(format!("{} #{} {}", t("QCCOM 社区"), p.id, p.title));
                        ui.close_menu();
                    }
                    if admin && ui.button(t("在后台打开")).clicked() {
                        let base = hub::base_url();
                        crate::app::open_url(&format!("{}/admin#posts", base.trim_end_matches("/api/v1")));
                        ui.close_menu();
                    }
                });
            }
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                if (self.items.len() as i64) < self.total {
                    let r = ui.label(RichText::new(if self.loading { t("加载中…") } else { t("加载更多") }).size(12.0).color(TEXT3()));
                    if ui.is_rect_visible(r.rect) {
                        self.load_more();
                    }
                } else {
                    ui.label(RichText::new(tf("共 {} 条", &[&self.total.max(self.items.len() as i64)])).size(11.5).color(TEXT3()));
                }
            });
            ui.add_space(12.0);
        });
    }

    fn mine_hint(&self) -> String {
        if self.logged_in() {
            t("你的账号在所有电脑上的内容").to_string()
        } else {
            t("这台电脑上匿名发的内容；登录后可以跨电脑查看").to_string()
        }
    }

    /// 列表里的一张帖子卡片。先占位、再在上面画内容，这样里面的投票按钮能单独点。
    /// 卡片之间靠留白分开，不画分隔线；悬停 / 选中时是圆角浅底
    fn card(&mut self, ui: &mut Ui, p: &Post, selected: bool) -> egui::Response {
        let full = ui.available_width();
        let hid = ui.id().with(("card-h", p.id));
        let h = ui.data(|d| d.get_temp::<f32>(hid)).unwrap_or(84.0);
        let (outer, _) = ui.allocate_exact_size(vec2(full, h + 4.0), Sense::hover());
        let rect = Rect::from_min_max(outer.min + vec2(8.0, 2.0), outer.max - vec2(8.0, 2.0));
        let resp = ui.interact(rect, ui.id().with(("card", p.id)), Sense::click());
        let hover = ui.rect_contains_pointer(rect);
        let painter = ui.painter().clone();
        let fade = ui.ctx().animate_bool_with_time(ui.id().with(("card-hov", p.id)), hover || selected, 0.10);
        if fade > 0.0 {
            let fill = if selected { BG2() } else { BG1().gamma_multiply(fade.max(0.01)) };
            painter.rect_filled(rect, 8.0, fill);
        }
        if selected {
            painter.rect_stroke(rect, 8.0, st(1.0, LINE()), egui::StrokeKind::Inside);
            painter.vline(rect.left() + 1.0, rect.y_range().shrink(12.0), st(2.5, ACCENT()));
        }
        let inner = rect.shrink2(vec2(14.0, 12.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(inner.min, vec2(inner.width(), 1000.0)))
                .layout(Layout::left_to_right(Align::Min)),
        );
        let board = Board::from_id(&p.board);
        let is_idea = board == Board::Idea;
        if is_idea {
            let id = egui::Id::new(("vote", p.id));
            if w::vote_capsule(&mut child, id, p.score, p.voted, !self.banned).on_hover_text(t("投一票（V）")).clicked() {
                self.vote(p.id);
            }
            child.add_space(14.0);
        }
        let q = self.query_sent.trim().to_string();
        let show_status = is_idea && (p.status != "open" || self.idea_filter == IdeaFilter::All);
        child.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            let tf_ = |c: Color32, f: FontId| egui::TextFormat {
                font_id: f,
                color: c,
                ..Default::default()
            };
            // 标题行：标题在左，状态在右上角对齐
            ui.horizontal_top(|ui| {
                let badge_w = if show_status { 64.0 } else { 0.0 };
                let mut job = egui::text::LayoutJob::default();
                let tag = |job: &mut egui::text::LayoutJob, s: &str, c: Color32| job.append(&format!("{s}  "), 0.0, tf_(c, FontId::new(12.5, bold())));
                if p.pinned {
                    tag(&mut job, t("置顶"), WARN());
                }
                if board == Board::Help && p.solved_reply.is_some() {
                    tag(&mut job, t("已解决"), OK());
                }
                if p.pending {
                    tag(&mut job, t("审核中"), WARN());
                }
                hl_append(&mut job, &p.title, &q, tf_(TEXT(), FontId::new(15.0, bold())));
                job.wrap.max_rows = 2;
                job.wrap.max_width = (ui.available_width() - badge_w).max(80.0);
                ui.label(job);
                if show_status {
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if p.status == "done" && !p.done_version.is_empty() {
                            ui.label(RichText::new(&p.done_version).size(11.0).monospace().color(OK()));
                        }
                        w::status_badge(ui, &p.status);
                    });
                }
            });
            let ex = plain_excerpt(&p.excerpt);
            if !ex.trim().is_empty() {
                let mut job = egui::text::LayoutJob::default();
                hl_append(&mut job, ex.trim(), &q, tf_(TEXT2(), FontId::proportional(12.5)));
                job.wrap.max_rows = 1;
                job.wrap.max_width = ui.available_width();
                ui.label(job);
            }
            // 信息行
            ui.add_space(1.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let tex = p.author.avatar.clone().and_then(|id| self.image(ui.ctx(), &id)).map(|x| x.0);
                w::avatar(ui, &p.author.seed(), tex, 16.0);
                ui.label(
                    RichText::new(crate::app::truncate(&p.author.name, 14))
                        .size(11.5)
                        .color(if p.author.staff { ACCENT() } else { TEXT2() }),
                );
                if p.author.staff {
                    w::tag(ui, t("作者"), ACCENT());
                }
                let dot = |ui: &mut Ui| ui.label(RichText::new("·").size(11.5).color(TEXT3()));
                dot(ui);
                ui.label(
                    RichText::new(ago(if p.last_reply_at > 0 && !is_idea { p.last_reply_at } else { p.created_at }))
                        .size(11.5)
                        .color(TEXT3()),
                );
                if p.reply_count > 0 {
                    dot(ui);
                    let (r, _) = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover());
                    w::chat_icon(ui.painter(), r, TEXT3());
                    ui.label(RichText::new(p.reply_count.to_string()).size(11.5).color(TEXT3()));
                }
                if matches!(self.nav, Nav::Mine(_)) || !q.is_empty() {
                    dot(ui);
                    ui.label(RichText::new(board.name()).size(11.5).color(TEXT3()));
                }
            });
        });
        let used = child.min_rect().height() + 24.0;
        let used = used.max(if is_idea { 70.0 } else { 0.0 });
        if (used - h).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(hid, used));
            ui.ctx().request_repaint();
        }
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    fn notifs_ui(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().id_salt("comm-notifs").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if self.notifs.is_empty() {
                if self.notifs_loading {
                    for _ in 0..4 {
                        w::skel_card(ui);
                    }
                } else {
                    empty(ui, Art::Bell, t("没有新提醒"), t("别人回复你、采纳你的回复，或者你投票的需求有进展时，会在这里告诉你"), None);
                }
                return;
            }
            for n in self.notifs.clone() {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
                let p = ui.painter();
                if resp.hovered() || self.pane == Pane::Post(n.post_id) {
                    p.rect_filled(rect, 0.0, if self.pane == Pane::Post(n.post_id) { BG2() } else { BG1() });
                }
                p.hline(rect.x_range().shrink(14.0), rect.bottom() - 0.5, st(1.0, LINE().gamma_multiply(0.6)));
                let ic = Rect::from_center_size(pos2(rect.left() + 28.0, rect.center().y), vec2(28.0, 28.0));
                let col = match n.kind.as_str() {
                    "status" => VIOLET(),
                    "solved" => OK(),
                    _ => ACCENT(),
                };
                p.circle_filled(ic.center(), 14.0, col.gamma_multiply(0.14));
                let ir = Rect::from_center_size(ic.center(), vec2(14.0, 14.0));
                match n.kind.as_str() {
                    "status" => w::board_icon(p, ir, Board::Idea, col),
                    "solved" => {
                        let c = ir.center();
                        p.add(egui::Shape::line(vec![c + vec2(-4.5, 0.0), c + vec2(-1.2, 3.5), c + vec2(5.0, -3.5)], st(1.6, col)));
                    }
                    _ => w::chat_icon(p, ir, col),
                }
                let x = ic.right() + 12.0;
                let maxw = rect.right() - x - 30.0;
                let g = p.layout(
                    notif_text(&n),
                    FontId::new(13.0, if n.read { FontFamily::Proportional } else { bold() }),
                    if n.read { TEXT2() } else { TEXT() },
                    maxw,
                );
                let g = if g.rows.len() > 1 {
                    p.layout_no_wrap(crate::app::truncate(&notif_text(&n), 26), FontId::proportional(13.0), TEXT())
                } else {
                    g
                };
                p.galley(pos2(x, rect.top() + 11.0), g, TEXT());
                p.text(
                    pos2(x, rect.top() + 39.0),
                    Align2::LEFT_CENTER,
                    format!("{} · {}", crate::app::truncate(&n.title, 30), ago(n.created_at)),
                    FontId::proportional(11.5),
                    TEXT3(),
                );
                if !n.read {
                    p.circle_filled(pos2(rect.right() - 18.0, rect.center().y), 4.0, ACCENT());
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    self.open_post(n.post_id);
                    if let Some(rid) = n.reply_id {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("comm-jump-reply"), rid));
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------ 右边

    fn pane_ui(&mut self, ui: &mut Ui, narrow: bool) {
        match self.pane.clone() {
            Pane::Empty => self.welcome_ui(ui),
            Pane::Post(id) => self.detail_ui(ui, id, narrow),
            Pane::Compose => self.compose_ui(ui, narrow),
            Pane::Account => self.account_ui(ui, narrow),
        }
    }

    fn back_bar(&mut self, ui: &mut Ui, narrow: bool, title: &str, right: impl FnOnce(&mut Self, &mut Ui)) {
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 10,
                right: 12,
                top: 8,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(28.0);
                    if narrow {
                        if icon_button(ui, Icon::Back, t("返回（Esc）"), false).clicked() {
                            self.close_pane();
                        }
                    } else {
                        ui.add_space(6.0);
                    }
                    ui.label(RichText::new(title).size(12.5).color(TEXT3()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| right(self, ui));
                });
            });
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top(), st(1.0, LINE()));
    }

    fn welcome_ui(&mut self, ui: &mut Ui) {
        let r = ui.available_rect_before_wrap();
        let c = r.center() - vec2(0.0, 40.0);
        let p = ui.painter();
        p.circle_filled(c, 34.0, ACCENT().gamma_multiply(0.08));
        p.circle_stroke(c, 34.0, st(1.0, ACCENT().gamma_multiply(0.3)));
        w::chat_icon(p, Rect::from_center_size(c, vec2(30.0, 30.0)), ACCENT());
        p.text(c + vec2(0.0, 58.0), Align2::CENTER_CENTER, t("选一个帖子查看"), FontId::new(15.0, bold()), TEXT2());
        let mut ui2 = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(c + vec2(0.0, 98.0), vec2(320.0, 40.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        ui2.spacing_mut().item_spacing.x = 4.0;
        let keys = [("J", t("下一个")), ("K", t("上一个")), ("V", t("投票")), ("N", t("发帖"))];
        let total: f32 = 4.0 * 62.0;
        ui2.add_space((320.0 - total) / 2.0);
        for (k, d) in keys {
            w::kbd(&mut ui2, k);
            ui2.label(RichText::new(d).size(11.5).color(TEXT3()));
            ui2.add_space(6.0);
        }
    }

    // ---------------------------------------------------- 详情

    fn detail_ui(&mut self, ui: &mut Ui, id: i64, narrow: bool) {
        let ctx = ui.ctx().clone();
        let Some(p) = self.detail.clone().filter(|d| d.id == id) else {
            self.back_bar(ui, narrow, "", |_, _| {});
            if let Some(e) = self.detail_err.clone() {
                if empty(ui, Art::Error, t("打不开这个帖子"), &e, Some(t("重试"))) {
                    self.open_post(id);
                }
            } else {
                egui::Frame::new().inner_margin(egui::Margin::same(24)).show(ui, |ui| {
                    let wd = ui.available_width();
                    w::skel(ui, wd * 0.6, 22.0);
                    ui.add_space(14.0);
                    w::skel(ui, 160.0, 14.0);
                    ui.add_space(24.0);
                    for f in [0.95, 0.9, 0.7, 0.85, 0.4] {
                        w::skel(ui, wd * f, 12.0);
                        ui.add_space(9.0);
                    }
                });
            }
            return;
        };
        let board = Board::from_id(&p.board);
        let admin = self.is_admin();
        let crumb = format!("{} · #{}", board.name(), p.id);
        self.back_bar(ui, narrow, &crumb, |s, ui| {
            if s.detail_loading {
                ui.add(egui::Spinner::new().size(14.0).color(TEXT3()));
            } else if icon_button(ui, Icon::Refresh, t("刷新"), false).clicked() {
                s.open_post(p.id);
            }
            if p.can_edit || p.can_delete || admin {
                let r = icon_button(ui, Icon::More, t("更多"), false);
                let pid = ui.make_persistent_id(("comm-more", p.id));
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pid));
                }
                egui::popup::popup_above_or_below_widget(ui, pid, &r, egui::AboveOrBelow::Below, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(160.0);
                    if p.can_edit && ui.button(t("编辑")).clicked() {
                        s.edit_post(&p);
                    }
                    if admin {
                        ui.separator();
                        if ui.button(if p.pinned { t("取消置顶") } else { t("置顶") }).clicked() {
                            s.admin_patch(p.id, json!({ "pinned": !p.pinned }), if p.pinned { "已取消置顶" } else { "已置顶" });
                        }
                        if ui.button(if p.locked { t("解除锁定") } else { t("锁定（禁止回复）") }).clicked() {
                            s.admin_patch(p.id, json!({ "locked": !p.locked }), if p.locked { "已解锁" } else { "已锁定" });
                        }
                        for b in Board::ALL {
                            if b != board && ui.button(tf("移到「{}」", &[&b.name()])).clicked() {
                                s.admin_patch(p.id, json!({ "board": b.id() }), "已移动");
                            }
                        }
                        if ui.button(t("在后台打开")).clicked() {
                            let base = hub::base_url();
                            crate::app::open_url(&format!("{}/admin#posts", base.trim_end_matches("/api/v1")));
                        }
                    }
                    if p.can_delete {
                        ui.separator();
                        if ui.button(RichText::new(t("删除帖子")).color(ERR())).clicked() {
                            s.confirm = Some((p.id, false, Instant::now()));
                        }
                    }
                });
            }
        });

        // 删除确认条
        if let Some((cid, false, at)) = self.confirm {
            if cid == p.id && at.elapsed() < Duration::from_secs(8) {
                egui::Frame::new().fill(ERR().gamma_multiply(0.10)).inner_margin(egui::Margin::symmetric(16, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("删除这个帖子？回复会一起删除，不能恢复。")).size(12.5).color(ERR()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if w::link(ui, t("删除"), ERR()).clicked() {
                                self.confirm = None;
                                self.delete(p.id, false);
                            }
                            if w::link(ui, t("取消"), TEXT2()).clicked() {
                                self.confirm = None;
                            }
                        });
                    });
                });
            }
        }

        // 底部回复框
        egui::TopBottomPanel::bottom(egui::Id::new("comm-reply-panel"))
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 20,
                right: 20,
                top: 10,
                bottom: 10,
            }))
            .show_separator_line(true)
            .show_inside(ui, |ui| self.reply_box(ui, &p));

        let mut sa = egui::ScrollArea::vertical().id_salt(("comm-detail", p.id)).auto_shrink([false, false]);
        if self.scroll_top {
            sa = sa.vertical_scroll_offset(0.0);
            self.scroll_top = false;
        }
        let jump = ctx.data(|d| d.get_temp::<i64>(egui::Id::new("comm-jump-reply")));
        let bottom = ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new("comm-scroll-bottom"))).unwrap_or(false);
        sa.show(ui, |ui| {
            let maxw = 720.0f32.min(ui.available_width() - 40.0);
            let pad = ((ui.available_width() - maxw) / 2.0).max(20.0);
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: pad as i8,
                    right: pad as i8,
                    top: 20,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    ui.set_max_width(maxw);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    // 标题
                    ui.horizontal_top(|ui| {
                        if board == Board::Idea {
                            let r = ui.add_space(0.0);
                            let _ = r;
                        }
                        ui.add(egui::Label::new(RichText::new(&p.title).font(FontId::new(20.0, bold())).color(TEXT())).wrap());
                    });
                    // 信息条：作者、时间、标记；需求再加投票和状态
                    ui.add_space(4.0);
                    egui::Frame::new()
                        .fill(BG1())
                        .stroke(st(1.0, LINE().gamma_multiply(0.7)))
                        .corner_radius(R_CARD)
                        .inner_margin(egui::Margin::symmetric(12, 9))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let tex = p.author.avatar.clone().and_then(|id| self.image(&ctx, &id)).map(|x| x.0);
                                w::avatar(ui, &p.author.seed(), tex, 22.0);
                                ui.label(RichText::new(&p.author.name).size(12.5).color(if p.author.staff { ACCENT() } else { TEXT() }));
                                if p.author.staff {
                                    w::tag(ui, t("作者"), ACCENT());
                                }
                                if p.author.me {
                                    w::tag(ui, t("我"), TEXT3());
                                }
                                let mut meta = ago(p.created_at);
                                if p.edited_at.is_some() {
                                    meta.push_str(&format!(" · {}", t("已编辑")));
                                }
                                ui.label(RichText::new(format!("· {meta}")).size(12.0).color(TEXT3()));
                                if p.pinned {
                                    w::tag(ui, t("置顶"), WARN());
                                }
                                if p.locked {
                                    w::tag(ui, t("已锁定"), TEXT3());
                                }
                                if p.pending {
                                    w::tag(ui, t("审核中"), WARN());
                                }
                                if board == Board::Help && p.solved_reply.is_some() {
                                    w::tag(ui, t("已解决"), OK());
                                }
                            });
                            // 需求：投票 + 状态
                            if board == Board::Idea {
                                ui.add_space(4.0);
                                let r = ui.available_rect_before_wrap();
                                ui.painter().hline(r.x_range(), r.top(), st(1.0, LINE().gamma_multiply(0.6)));
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    if w::vote_pill(ui, p.score, p.voted, !self.banned)
                                        .on_hover_text(if p.voted { t("取消投票（V）") } else { t("我也想要（V）") })
                                        .clicked()
                                    {
                                        self.vote(p.id);
                                    }
                                    if admin {
                                        let mut s = p.status.clone();
                                        egui::ComboBox::from_id_salt("comm-status").selected_text(status_name(&s)).width(90.0).show_ui(ui, |ui| {
                                            for x in ["open", "planned", "done", "declined"] {
                                                ui.selectable_value(&mut s, x.to_string(), status_name(x));
                                            }
                                        });
                                        if s != p.status {
                                            self.admin_patch(p.id, json!({ "status": s }), "状态已更新");
                                            if let Some(d) = self.detail.as_mut() {
                                                d.status = s;
                                            }
                                        }
                                    } else {
                                        w::status_badge(ui, &p.status);
                                    }
                                    if p.status == "done" && !p.done_version.is_empty() {
                                        ui.label(RichText::new(tf("已在 {} 完成", &[&p.done_version])).size(12.0).color(OK()));
                                    }
                                    if p.voted {
                                        ui.label(RichText::new(t("有进展会提醒你")).size(11.5).color(TEXT3()));
                                    }
                                });
                            }
                        });
                    if let (Some(_), Some(mt)) = (p.merged_into, &p.merged_title) {
                        ui.add_space(6.0);
                        let r = note_card(ui, VIOLET(), t("已合并"), &tf("这个需求和「{}」重复，票数已经合并过去", &[mt]));
                        if r.clicked() {
                            if let Some(m) = p.merged_into {
                                self.open_post(m);
                            }
                        }
                    }
                    // 作者回复（需求的官方说明）
                    if !p.admin_note.is_empty() || admin && board == Board::Idea {
                        ui.add_space(6.0);
                        self.admin_note_ui(ui, &p, admin);
                    }
                    ui.add_space(12.0);
                    // 正文
                    if p.body.trim().is_empty() {
                        ui.label(RichText::new(t("（没有补充说明）")).color(TEXT3()));
                    } else {
                        let clicked = super::md::body(ui, &p.body, &mut |id| self.image(&ctx, id));
                        if clicked.is_some() {
                            self.lightbox = clicked;
                        }
                    }
                    ui.add_space(22.0);
                    // 回复
                    let n = p.replies.iter().filter(|r| !r.deleted).count();
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if n == 0 { t("还没有回复").to_string() } else { tf("{} 条回复", &[&n]) })
                                .font(FontId::new(13.0, bold()))
                                .color(TEXT2()),
                        );
                    });
                    let r = ui.available_rect_before_wrap();
                    ui.painter().hline(r.x_range(), r.top() + 2.0, st(1.0, LINE()));
                    ui.add_space(8.0);
                    let last = p.replies.last().map(|r| r.id);
                    for r in &p.replies {
                        let resp = self.reply_ui(ui, &p, r, &ctx);
                        if jump == Some(r.id) {
                            resp.scroll_to_me(Some(Align::Center));
                            ctx.data_mut(|d| d.remove::<i64>(egui::Id::new("comm-jump-reply")));
                            ctx.data_mut(|d| d.insert_temp(egui::Id::new(("comm-flash", r.id)), ctx.input(|i| i.time)));
                        }
                        if bottom && last == Some(r.id) {
                            resp.scroll_to_me(Some(Align::Max));
                        }
                    }
                });
        });
    }

    fn admin_note_ui(&mut self, ui: &mut Ui, p: &Post, admin: bool) {
        let editing = self.admin_note.as_ref().is_some_and(|x| x.0 == p.id);
        if editing {
            let mut done = None;
            egui::Frame::new()
                .fill(ACCENT().gamma_multiply(0.06))
                .stroke(st(1.0, ACCENT().gamma_multiply(0.4)))
                .corner_radius(R_CARD)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let (_, note, ver) = self.admin_note.as_mut().unwrap();
                    ui.label(RichText::new(t("作者回复")).size(12.0).color(ACCENT()));
                    ui.add(egui::TextEdit::multiline(note).desired_rows(3).desired_width(f32::INFINITY).hint_text(t("说明打算怎么做、为什么不做…")));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("完成版本")).size(12.0).color(TEXT2()));
                        ui.add(egui::TextEdit::singleline(ver).desired_width(90.0).hint_text("v1.9.0"));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if w::primary(ui, t("保存"), true).clicked() {
                                done = Some(true);
                            }
                            if w::link(ui, t("取消"), TEXT2()).clicked() {
                                done = Some(false);
                            }
                        });
                    });
                });
            match done {
                Some(true) => {
                    let (_, note, ver) = self.admin_note.take().unwrap();
                    if let Some(d) = self.detail.as_mut() {
                        d.admin_note = note.clone();
                        d.done_version = ver.clone();
                    }
                    self.admin_patch(p.id, json!({ "admin_note": note, "done_version": ver }), "已保存");
                }
                Some(false) => self.admin_note = None,
                None => {}
            }
            return;
        }
        if p.admin_note.is_empty() {
            if admin && w::link(ui, &format!("+ {}", t("写作者回复")), ACCENT()).clicked() {
                self.admin_note = Some((p.id, String::new(), p.done_version.clone()));
            }
            return;
        }
        let ctx = ui.ctx().clone();
        let r = egui::Frame::new()
            .fill(ACCENT().gamma_multiply(0.06))
            .stroke(st(1.0, ACCENT().gamma_multiply(0.30)))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    w::avatar(ui, "qccom-staff", None, 26.0);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("作者回复")).font(FontId::new(12.5, bold())).color(ACCENT()));
                            if p.status == "done" && !p.done_version.is_empty() {
                                w::tag(ui, &p.done_version, OK());
                            }
                        });
                        let _ = super::md::body(ui, &p.admin_note, &mut |id| self.image(&ctx, id));
                    });
                });
            })
            .response
            .interact(Sense::click());
        if admin && r.on_hover_text(t("点击修改")).clicked() {
            self.admin_note = Some((p.id, p.admin_note.clone(), p.done_version.clone()));
        }
    }

    fn reply_ui(&mut self, ui: &mut Ui, p: &Post, r: &Reply, ctx: &egui::Context) -> egui::Response {
        let solved = p.solved_reply == Some(r.id);
        let op = r.author.seed() == p.author.seed();
        let flash = ctx
            .data(|d| d.get_temp::<f64>(egui::Id::new(("comm-flash", r.id))))
            .map(|t0| ctx.input(|i| i.time) - t0)
            .filter(|d| *d < 1.6);
        let fill = if solved {
            OK().gamma_multiply(0.06)
        } else if let Some(d) = flash {
            ctx.request_repaint();
            ACCENT().gamma_multiply(0.14 * (1.0 - d as f32 / 1.6))
        } else if r.author.staff {
            ACCENT().gamma_multiply(0.045)
        } else if op {
            BG1()
        } else {
            Color32::TRANSPARENT
        };
        let frame = egui::Frame::new()
            .fill(fill)
            .stroke(if solved { st(1.0, OK().gamma_multiply(0.4)) } else { egui::Stroke::NONE })
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::symmetric(10, 10));
        let resp = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if r.deleted {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("#{}", r.floor)).size(11.5).monospace().color(TEXT3()));
                        ui.label(RichText::new(t("这条回复已删除")).size(12.5).italics().color(TEXT3()));
                    });
                    return;
                }
                ui.horizontal_top(|ui| {
                    let tex = r.author.avatar.clone().and_then(|id| self.image(ctx, &id)).map(|x| x.0);
                    w::avatar(ui, &r.author.seed(), tex, 30.0);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new(&r.author.name).size(12.5).strong().color(if r.author.staff { ACCENT() } else { TEXT() }));
                            if r.author.staff {
                                w::tag(ui, t("作者"), ACCENT());
                            }
                            if op {
                                w::tag(ui, t("楼主"), TEXT3());
                            }
                            if solved {
                                w::tag(ui, t("已采纳"), OK());
                            }
                            if r.pending {
                                w::tag(ui, t("审核中"), WARN());
                            }
                            let mut meta = ago(r.created_at);
                            if r.edited_at.is_some() {
                                meta.push_str(&format!(" · {}", t("已编辑")));
                            }
                            ui.label(RichText::new(meta).size(11.5).color(TEXT3()));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(format!("#{}", r.floor)).size(11.5).monospace().color(TEXT3()));
                            });
                        });
                        if let Some(q) = &r.quote {
                            let qa = q.author.as_ref().map(|a| a.name.clone()).unwrap_or_default();
                            let qr = egui::Frame::new()
                                .fill(BG1())
                                .corner_radius(R_WIDGET)
                                .inner_margin(egui::Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(format!("#{} {qa}", q.floor)).size(11.5).color(TEXT3()));
                                    let text = if q.deleted { t("（已删除）").to_string() } else { q.excerpt.clone() };
                                    let mut job = egui::text::LayoutJob::single_section(
                                        text,
                                        egui::TextFormat {
                                            font_id: FontId::proportional(12.5),
                                            color: TEXT2(),
                                            ..Default::default()
                                        },
                                    );
                                    job.wrap.max_rows = 3;
                                    job.wrap.max_width = ui.available_width();
                                    ui.label(job);
                                })
                                .response;
                            let x = qr.rect.left() + 1.0;
                            ui.painter().vline(x, qr.rect.y_range().shrink(4.0), st(2.5, LINE_HI()));
                            if ui
                                .interact(qr.rect, ui.id().with(("q", r.id)), Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .clicked()
                            {
                                ctx.data_mut(|d| d.insert_temp(egui::Id::new("comm-jump-reply"), q.id));
                            }
                        }
                        let clicked = super::md::body(ui, &r.body, &mut |id| self.image(ctx, id));
                        if clicked.is_some() {
                            self.lightbox = clicked;
                        }
                        // 操作栏
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let (hr, hresp) = ui.allocate_exact_size(vec2(r.likes.to_string().len() as f32 * 7.0 + 30.0, 24.0), Sense::click());
                            let pnt = ui.painter();
                            if hresp.hovered() {
                                pnt.rect_filled(hr, CornerRadius::same(4), BG3());
                                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            let hc = if r.liked { ERR() } else { TEXT3() };
                            w::heart(pnt, pos2(hr.left() + 12.0, hr.center().y), hc, r.liked);
                            if r.likes > 0 {
                                pnt.text(pos2(hr.left() + 22.0, hr.center().y), Align2::LEFT_CENTER, r.likes.to_string(), FontId::proportional(12.0), hc);
                            }
                            if hresp.on_hover_text(t("赞")).clicked() {
                                self.like(r.id);
                            }
                            if !p.locked && w::link(ui, t("引用"), TEXT3()).clicked() {
                                let ex: String = r.body.lines().find(|l| !l.trim().is_empty() && !l.contains("](img:")).unwrap_or("").chars().take(60).collect();
                                self.reply_quote = Some((r.id, r.floor, format!("{} : {ex}", r.author.name)));
                                self.reply_edit = None;
                                ctx.memory_mut(|m| m.request_focus(reply_id()));
                            }
                            if p.board == "help" && (p.author.me || self.is_admin()) && !r.author.me {
                                if solved {
                                    if w::link(ui, t("取消采纳"), TEXT3()).clicked() {
                                        self.solve(p.id, None);
                                    }
                                } else if w::link(ui, t("采纳为解决方法"), OK()).clicked() {
                                    self.solve(p.id, Some(r.id));
                                }
                            }
                            if r.can_edit && w::link(ui, t("编辑"), TEXT3()).clicked() {
                                self.reply_edit = Some(r.id);
                                self.reply_quote = None;
                                self.reply = r.body.clone();
                                ctx.memory_mut(|m| m.request_focus(reply_id()));
                            }
                            if r.can_delete {
                                let armed = self.confirm.is_some_and(|(id, rep, at)| rep && id == r.id && at.elapsed() < Duration::from_secs(4));
                                if armed {
                                    ctx.request_repaint_after(Duration::from_millis(500));
                                }
                                if w::link(ui, if armed { t("再点一次删除") } else { t("删除") }, if armed { ERR() } else { TEXT3() }).clicked() {
                                    if armed {
                                        self.confirm = None;
                                        self.delete(r.id, true);
                                    } else {
                                        self.confirm = Some((r.id, true, Instant::now()));
                                    }
                                }
                            }
                        });
                    });
                });
            })
            .response;
        ui.add_space(2.0);
        resp
    }

    fn reply_box(&mut self, ui: &mut Ui, p: &Post) {
        let maxw = 760.0f32.min(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.set_max_width(maxw);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                if p.locked && !self.is_admin() {
                    ui.label(RichText::new(t("这个帖子已锁定，不能再回复")).size(12.5).color(TEXT3()));
                    return;
                }
                if self.banned {
                    ui.label(RichText::new(t("你暂时不能发言")).size(12.5).color(WARN()));
                    return;
                }
                // 引用 / 编辑提示
                let chip = if let Some(eid) = self.reply_edit {
                    Some(tf("正在修改 #{} 的回复", &[&p.replies.iter().find(|r| r.id == eid).map(|r| r.floor).unwrap_or(0)]))
                } else {
                    self.reply_quote.as_ref().map(|(_, f, ex)| format!("{} #{f}  {}", t("回复"), crate::app::truncate(ex, 48)))
                };
                if let Some(c) = chip {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(c).size(11.5).color(ACCENT()));
                        if ui.add(egui::Button::new(RichText::new("×").color(TEXT3())).frame(false)).clicked() {
                            if self.reply_edit.take().is_some() {
                                self.reply.clear();
                            }
                            self.reply_quote = None;
                        }
                    });
                }
                let focused = ui.memory(|m| m.has_focus(reply_id()));
                let frame = egui::Frame::new()
                    .fill(BG0())
                    .stroke(st(1.0, if focused { ACCENT().gamma_multiply(0.6) } else { LINE() }))
                    .corner_radius(R_CARD)
                    .inner_margin(egui::Margin::symmetric(8, 6));
                let mut send = false;
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().id_salt("comm-reply-scroll").max_height(160.0).show(ui, |ui| {
                        let rows = if focused || !self.reply.is_empty() { 3 } else { 1 };
                        let te = egui::TextEdit::multiline(&mut self.reply)
                            .id(reply_id())
                            .frame(false)
                            .desired_rows(rows)
                            .desired_width(f32::INFINITY)
                            .hint_text(if p.board == "help" {
                                t("知道怎么解决？说说看…（R）")
                            } else {
                                t("写下你的回复…（R）")
                            });
                        let r = ui.add(te);
                        let _ = r;
                        if focused && ctrl_enter(ui) {
                            send = true;
                        }
                    });
                    if focused || !self.reply.is_empty() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if self.uploading == Some(Target::Reply) {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(RichText::new(t("上传中…")).size(11.5).color(TEXT3()));
                            } else {
                                if w::link(ui, t("图片"), TEXT2()).on_hover_text(t("也可以把图片拖进窗口")).clicked() {
                                    if let Some(f) = pick_image() {
                                        self.upload_file(Target::Reply, &f);
                                    }
                                }
                                if w::link(ui, t("粘贴截图"), TEXT2()).clicked() {
                                    self.paste_image(Target::Reply);
                                }
                                if w::link(ui, t("代码"), TEXT2()).clicked() {
                                    self.reply.push_str("\n```\n\n```\n");
                                }
                            }
                            if self.logged_in() && self.reply_edit.is_none() {
                                ui.add_space(6.0);
                                crate::app::toggle_chip(ui, &mut self.reply_anon, t("匿名"), t("这条回复不显示你的昵称"));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let ok = !self.reply.trim().is_empty() && !self.replying;
                                if w::primary(ui, if self.reply_edit.is_some() { t("保存") } else { t("回复") }, ok).clicked() && ok {
                                    send = true;
                                }
                                ui.label(RichText::new("Ctrl+Enter").size(11.0).color(TEXT3()));
                            });
                        });
                    }
                });
                if send {
                    self.submit_reply();
                }
            });
        });
    }

    // ---------------------------------------------------- 发帖

    fn compose_ui(&mut self, ui: &mut Ui, narrow: bool) {
        let editing = self.draft.edit.is_some();
        let title = if editing { t("编辑帖子") } else { t("发帖") };
        self.back_bar(ui, narrow, title, |s, ui| {
            if !editing && (!s.draft.title.is_empty() || !s.draft.body.is_empty()) {
                if w::link(ui, t("丢弃草稿"), TEXT3()).clicked() {
                    s.discard_draft();
                }
                ui.label(RichText::new(t("草稿已自动保存")).size(11.5).color(TEXT3()));
            } else if editing && w::link(ui, t("取消编辑"), TEXT3()).clicked() {
                s.discard_draft();
            }
        });
        let mut changed = false;
        let mut submit = false;
        egui::ScrollArea::vertical().id_salt("comm-compose").auto_shrink([false, false]).show(ui, |ui| {
            let maxw = 720.0f32.min(ui.available_width() - 40.0);
            let pad = ((ui.available_width() - maxw) / 2.0).max(20.0);
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: pad as i8,
                    right: pad as i8,
                    top: 18,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    ui.set_max_width(maxw);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    if self.banned {
                        note_card(ui, WARN(), t("暂时不能发言"), t("你的账号或这台电脑被作者限制了发言，有疑问可以联系作者"));
                    }
                    // 板块
                    if !editing {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for b in Board::ALL {
                                if b == Board::Notice && !self.is_admin() {
                                    continue;
                                }
                                let on = self.draft.board == b.id();
                                if board_chip(ui, b, on).clicked() && !on {
                                    self.draft.board = b.id().into();
                                    changed = true;
                                }
                            }
                        });
                        let b = Board::from_id(&self.draft.board);
                        ui.label(RichText::new(b.desc()).size(12.0).color(TEXT3()));
                    }
                    // 标题
                    let n = self.draft.title.chars().count();
                    let te = egui::TextEdit::singleline(&mut self.draft.title)
                        .id(title_id())
                        .hint_text(match Board::from_id(&self.draft.board) {
                            Board::Idea => t("一句话说清楚想要什么功能"),
                            Board::Help => t("一句话描述遇到的问题"),
                            _ => t("标题"),
                        })
                        .font(FontId::new(17.0, bold()))
                        .margin(vec2(10.0, 8.0))
                        .desired_width(f32::INFINITY);
                    if ui.add(te).changed() {
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("{n} / 80")).size(11.0).color(if n > 80 { ERR() } else { TEXT3() }));
                        });
                    });
                    // 相似帖
                    if !editing && !self.similar.is_empty() {
                        let sim = self.similar.clone();
                        egui::Frame::new()
                            .fill(VIOLET().gamma_multiply(0.07))
                            .stroke(st(1.0, VIOLET().gamma_multiply(0.3)))
                            .corner_radius(R_CARD)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(t("看看是不是已经有人提过，有的话投一票就好")).size(12.0).color(VIOLET()));
                                for s in sim.iter().take(4) {
                                    ui.horizontal(|ui| {
                                        if s.board == "idea" {
                                            ui.label(RichText::new(format!("▲ {}", s.score)).size(11.5).monospace().color(if s.voted { ACCENT() } else { TEXT3() }));
                                        }
                                        let r = ui.add(egui::Label::new(RichText::new(&s.title).size(13.0).color(TEXT())).sense(Sense::click()).truncate());
                                        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                            self.save_draft();
                                            self.open_post(s.id);
                                        }
                                    });
                                }
                            });
                    }
                    // 正文：编辑 / 预览
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if seg(ui, t("编辑"), !self.preview).clicked() {
                            self.preview = false;
                        }
                        if seg(ui, t("预览"), self.preview).clicked() {
                            self.preview = true;
                        }
                        ui.add_space(8.0);
                        if !self.preview {
                            if self.uploading == Some(Target::Compose) {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.label(RichText::new(t("上传中…")).size(11.5).color(TEXT3()));
                            } else {
                                if w::link(ui, t("图片"), TEXT2()).on_hover_text(t("最多 4 张，每张 2 MB 以内；也可以直接拖进窗口")).clicked() {
                                    if let Some(f) = pick_image() {
                                        self.upload_file(Target::Compose, &f);
                                    }
                                }
                                if w::link(ui, t("粘贴截图"), TEXT2()).clicked() {
                                    self.paste_image(Target::Compose);
                                }
                                if w::link(ui, t("代码"), TEXT2()).on_hover_text(t("插入代码块")).clicked() {
                                    self.draft.body.push_str("\n```c\n\n```\n");
                                    changed = true;
                                }
                            }
                        }
                    });
                    let imgs = super::md::image_ids(&self.draft.body).len();
                    if self.preview {
                        egui::Frame::new()
                            .fill(BG1())
                            .stroke(st(1.0, LINE()))
                            .corner_radius(R_CARD)
                            .inner_margin(egui::Margin::same(14))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.set_min_height(220.0);
                                if self.draft.body.trim().is_empty() {
                                    ui.label(RichText::new(t("（没有内容）")).color(TEXT3()));
                                } else {
                                    let ctx = ui.ctx().clone();
                                    let body = self.draft.body.clone();
                                    let c = super::md::body(ui, &body, &mut |id| self.image(&ctx, id));
                                    if c.is_some() {
                                        self.lightbox = c;
                                    }
                                }
                            });
                    } else {
                        let hint = match Board::from_id(&self.draft.board) {
                            Board::Idea => t("（选填）用在什么场景、希望怎么用。写清楚更容易被做出来"),
                            Board::Help => t("说说做了什么、期望什么、实际看到什么。可以贴日志和截图"),
                            _ => t("支持 Markdown：**粗体**、`代码`、```代码块```、> 引用、- 列表"),
                        };
                        let body_id = egui::Id::new("comm-body");
                        let had = ui.memory(|m| m.has_focus(body_id) || m.has_focus(title_id()));
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut self.draft.body)
                                .id(body_id)
                                .desired_rows(12)
                                .desired_width(f32::INFINITY)
                                .hint_text(hint)
                                .margin(vec2(10.0, 8.0))
                                .font(FontId::proportional(13.5)),
                        );
                        if r.changed() {
                            changed = true;
                        }
                        if had && ctrl_enter(ui) {
                            submit = true;
                        }
                    }
                    if imgs > 4 {
                        ui.label(RichText::new(t("图片最多 4 张")).size(12.0).color(ERR()));
                    }
                    ui.add_space(4.0);
                    // 底栏：身份 + 发布
                    ui.horizontal(|ui| {
                        if editing {
                        } else if self.logged_in() {
                            let mut anon = self.draft.anonymous;
                            if crate::app::switch(ui, &mut anon).changed() {
                                self.draft.anonymous = anon;
                                changed = true;
                            }
                            let name = self.me_user.as_ref().map(|u| u.nickname.clone()).unwrap_or_default();
                            ui.label(RichText::new(if anon { t("匿名发布").to_string() } else { tf("以「{}」发布", &[&name]) }).size(12.5).color(TEXT2()));
                        } else {
                            let name = if self.device_name.is_empty() { t("匿名用户").to_string() } else { self.device_name.clone() };
                            ui.label(RichText::new(tf("将以「{}」发布", &[&name])).size(12.5).color(TEXT2()));
                            if w::link(ui, t("登录"), ACCENT()).on_hover_text(t("登录后可以在别的电脑上看到自己的帖子")).clicked() {
                                self.save_draft();
                                self.pane = Pane::Account;
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let n = self.draft.title.trim().chars().count();
                            let ok = (4..=80).contains(&n) && !self.posting && !self.banned && imgs <= 4;
                            let label = if self.posting {
                                t("发布中…")
                            } else if editing {
                                t("保存修改")
                            } else {
                                t("发布")
                            };
                            let r = w::primary(ui, label, ok);
                            let r = if !ok && n < 4 { r.on_hover_text(t("标题 4–80 个字")) } else { r };
                            if r.clicked() && ok {
                                submit = true;
                            }
                            ui.label(RichText::new("Ctrl+Enter").size(11.0).color(TEXT3()));
                        });
                    });
                });
        });
        if changed {
            self.save_draft();
        }
        if submit {
            self.submit_post();
        }
    }

    // ---------------------------------------------------- 账号

    fn account_ui(&mut self, ui: &mut Ui, narrow: bool) {
        self.back_bar(ui, narrow, t("账号"), |_, _| {});
        egui::ScrollArea::vertical().id_salt("comm-account").auto_shrink([false, false]).show(ui, |ui| {
            let maxw = 380.0f32.min(ui.available_width() - 40.0);
            let pad = ((ui.available_width() - maxw) / 2.0).max(20.0);
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: pad as i8,
                    right: pad as i8,
                    top: 36,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    ui.set_max_width(maxw);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    if let Some(u) = self.me_user.clone() {
                        self.profile_ui(ui, &u);
                    } else {
                        self.login_ui(ui);
                    }
                });
        });
    }

    fn login_ui(&mut self, ui: &mut Ui) {
        let reg = self.auth.register;
        ui.vertical_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 26.0, ACCENT().gamma_multiply(0.10));
            w::person_icon(ui.painter(), Rect::from_center_size(r.center(), vec2(22.0, 22.0)), ACCENT());
            ui.add_space(4.0);
            ui.label(RichText::new(if reg { t("注册社区账号") } else { t("登录社区") }).font(FontId::new(18.0, bold())).color(TEXT()));
            ui.label(RichText::new(t("不登录也能匿名发帖、投票。登录后换电脑也能看到自己的帖子和提醒")).size(12.0).color(TEXT3()));
        });
        ui.add_space(10.0);
        let mut go = false;
        let field = |ui: &mut Ui, label: &str, v: &mut String, pw: bool, hint: &str| -> egui::Response {
            ui.label(RichText::new(label).size(12.0).color(TEXT2()));
            let r = ui.add(egui::TextEdit::singleline(v).password(pw).hint_text(hint).margin(vec2(10.0, 7.0)).desired_width(f32::INFINITY));
            r
        };
        let r1 = field(ui, t("用户名"), &mut self.auth.username, false, t("字母、数字、下划线，3–20 位"));
        let r2 = field(ui, t("密码"), &mut self.auth.password, true, if reg { t("至少 6 位") } else { "" });
        let mut enter = (r1.lost_focus() || r2.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
        if reg {
            let r3 = field(ui, t("昵称"), &mut self.auth.nickname, false, t("显示给别人看的名字，可以用中文"));
            enter |= r3.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        }
        if !self.auth.error.is_empty() {
            ui.label(RichText::new(&self.auth.error).size(12.0).color(ERR()));
        }
        ui.add_space(6.0);
        let ok = self.auth.username.trim().len() >= 3 && self.auth.password.len() >= if reg { 6 } else { 1 } && !self.auth.busy;
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 32.0), Layout::right_to_left(Align::Center), |ui| {
            let label = if self.auth.busy {
                t("请稍候…")
            } else if reg {
                t("注册并登录")
            } else {
                t("登录")
            };
            if w::primary(ui, label, ok).clicked() && ok {
                go = true;
            }
            if w::link(ui, if reg { t("已有账号？登录") } else { t("没有账号？注册") }, ACCENT()).clicked() {
                self.auth.register = !reg;
                self.auth.error.clear();
            }
        });
        if enter && ok {
            go = true;
        }
        if go {
            self.submit_auth();
        }
        ui.add_space(16.0);
        ui.label(RichText::new(t("忘记密码可以在社区发帖或联系作者重置。")).size(11.5).color(TEXT3()));
    }

    fn profile_ui(&mut self, ui: &mut Ui, u: &User) {
        let ctx = ui.ctx().clone();
        ui.vertical_centered(|ui| {
            let tex = u.avatar.clone().and_then(|id| self.image(&ctx, &id)).map(|x| x.0);
            let r = w::avatar(ui, &format!("u{}", u.id), tex, 64.0);
            if r.hovered() {
                let p = ui.painter();
                p.circle_filled(r.rect.center(), 32.0, Color32::from_black_alpha(120));
                p.text(r.rect.center(), Align2::CENTER_CENTER, t("更换"), FontId::proportional(12.0), Color32::WHITE);
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if r.on_hover_text(t("更换头像")).clicked() {
                if let Some(f) = pick_image() {
                    self.upload_file(Target::Avatar, &f);
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - 160.0) / 2.0).max(0.0));
                ui.label(RichText::new(&u.nickname).font(FontId::new(18.0, bold())).color(TEXT()));
                if u.is_admin {
                    w::tag(ui, t("作者"), ACCENT());
                }
            });
            ui.label(RichText::new(format!("@{}", u.username)).size(12.0).color(TEXT3()));
        });
        ui.add_space(8.0);
        if self.claimable > 0 {
            egui::Frame::new()
                .fill(ACCENT().gamma_multiply(0.07))
                .stroke(st(1.0, ACCENT().gamma_multiply(0.35)))
                .corner_radius(R_CARD)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(tf("这台电脑之前匿名发过 {} 条内容", &[&self.claimable])).size(13.0).color(TEXT()));
                    ui.label(RichText::new(t("归到账号下以后，换电脑也能看到，也能收到回复提醒。")).size(12.0).color(TEXT2()));
                    if w::primary(ui, t("归到我的账号"), true).clicked() {
                        self.claim();
                    }
                });
        }
        crate::app::card(ui, |ui| {
            ui.label(RichText::new(t("昵称")).size(12.0).color(TEXT2()));
            ui.add(egui::TextEdit::singleline(&mut self.auth.new_nick).margin(vec2(10.0, 7.0)).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            ui.label(RichText::new(t("修改密码")).size(12.0).color(TEXT2()));
            ui.add(
                egui::TextEdit::singleline(&mut self.auth.old_pw)
                    .password(true)
                    .hint_text(t("当前密码"))
                    .margin(vec2(10.0, 7.0))
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.auth.new_pw)
                    .password(true)
                    .hint_text(t("新密码，至少 6 位"))
                    .margin(vec2(10.0, 7.0))
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), 30.0), Layout::right_to_left(Align::Center), |ui| {
                let dirty = self.auth.new_nick.trim() != u.nickname || (!self.auth.new_pw.is_empty() && self.auth.new_pw.len() >= 6);
                if w::primary(ui, t("保存"), dirty).clicked() && dirty {
                    self.save_profile();
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if w::ghost(ui, t("退出登录")).clicked() {
                self.logout();
            }
            if u.is_admin && w::ghost(ui, t("打开后台")).clicked() {
                crate::app::open_url(&format!("{}/admin", hub::base_url().trim_end_matches("/api/v1")));
            }
        });
    }

    // ---------------------------------------------------- 大图

    fn lightbox_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.lightbox.clone() else { return };
        let screen = ctx.screen_rect();
        let mut close = false;
        egui::Area::new(egui::Id::new("comm-lightbox")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(200));
            if let Some((tex, [w0, h0])) = self.image(ctx, &id) {
                let (w0, h0) = (w0 as f32, h0 as f32);
                let s = ((r.width() - 80.0) / w0).min((r.height() - 80.0) / h0).min(2.0);
                let ir = Rect::from_center_size(r.center(), vec2(w0 * s, h0 * s));
                ui.painter().image(tex, ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            ui.painter().text(
                r.center_bottom() - vec2(0.0, 22.0),
                Align2::CENTER_CENTER,
                t("点击任意处或按 Esc 关闭"),
                FontId::proportional(12.0),
                Color32::from_gray(170),
            );
            if resp.clicked() {
                close = true;
            }
        });
        if close {
            self.lightbox = None;
        }
    }
}

// ---------------------------------------------------------------- 小工具

fn section(ui: &mut Ui, s: &str) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(s).size(11.0).color(TEXT3()));
    });
    ui.add_space(2.0);
}

/// 导航里的一行：图标 + 名字 + 未读数 / 小圆点
fn nav_row(ui: &mut Ui, slim: bool, on: bool, name: &str, count: Option<i64>, dot: bool, icon: impl FnOnce(&egui::Painter, Rect, Color32)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), if slim { 36.0 } else { 30.0 }), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, CornerRadius::same(6), BG3());
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), BG2());
    }
    let c = if on {
        ACCENT()
    } else if resp.hovered() {
        TEXT()
    } else {
        TEXT2()
    };
    let ic = if slim {
        Rect::from_center_size(rect.center(), vec2(16.0, 16.0))
    } else {
        Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(16.0, 16.0))
    };
    icon(p, ic, c);
    if !slim {
        p.text(
            pos2(rect.left() + 34.0, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::new(13.0, if on { bold() } else { FontFamily::Proportional }),
            if on { TEXT() } else { TEXT2() },
        );
    }
    if let Some(n) = count {
        let s = if n > 99 { "99+".to_string() } else { n.to_string() };
        if slim {
            p.circle_filled(ic.right_top() + vec2(1.0, 1.0), 4.0, ACCENT());
        } else {
            let g = p.layout_no_wrap(s, FontId::new(10.5, mono_bold()), ON_ACCENT());
            let br = Rect::from_center_size(pos2(rect.right() - 8.0 - (g.size().x + 10.0) / 2.0, rect.center().y), vec2(g.size().x + 10.0, 16.0));
            p.rect_filled(br, CornerRadius::same(8), ACCENT());
            p.galley(br.center() - g.size() / 2.0, g, ON_ACCENT());
        }
    } else if dot {
        let at = if slim { ic.right_top() + vec2(1.0, 1.0) } else { pos2(rect.right() - 12.0, rect.center().y) };
        p.circle_filled(at, 3.5, ACCENT());
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 分段按钮（排序、编辑/预览）
fn seg(ui: &mut Ui, name: &str, on: bool) -> egui::Response {
    let font = FontId::new(12.0, if on { bold() } else { FontFamily::Proportional });
    let g = ui.painter().layout_no_wrap(name.to_string(), font, TEXT());
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 18.0, 24.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect(r, CornerRadius::same(5), BG3(), st(1.0, LINE_HI()), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(5), BG2());
    }
    p.galley(
        r.center() - g.size() / 2.0,
        g,
        if on {
            TEXT()
        } else if resp.hovered() {
            TEXT2()
        } else {
            TEXT3()
        },
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn board_chip(ui: &mut Ui, b: Board, on: bool) -> egui::Response {
    let font = FontId::new(12.5, if on { bold() } else { FontFamily::Proportional });
    let g = ui.painter().layout_no_wrap(b.name().to_string(), font, TEXT());
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 40.0, 30.0), Sense::click());
    let p = ui.painter();
    let (fill, stroke, c) = if on {
        (ACCENT().gamma_multiply(0.12), ACCENT().gamma_multiply(0.55), ACCENT())
    } else if resp.hovered() {
        (BG3(), LINE_HI(), TEXT())
    } else {
        (BG2(), LINE(), TEXT2())
    };
    p.rect(r, CornerRadius::same(15), fill, st(1.0, stroke), egui::StrokeKind::Inside);
    w::board_icon(p, Rect::from_center_size(pos2(r.left() + 17.0, r.center().y), vec2(14.0, 14.0)), b, c);
    p.galley(pos2(r.left() + 29.0, r.center().y - g.size().y / 2.0), g, c);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 左边有色条的说明卡片（作者回复、已合并…）
fn note_card(ui: &mut Ui, c: Color32, label: &str, text: &str) -> egui::Response {
    let r = egui::Frame::new()
        .fill(c.gamma_multiply(0.07))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin {
            left: 14,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(label).size(11.5).strong().color(c));
            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(TEXT())).wrap());
        })
        .response;
    ui.painter().vline(r.rect.left() + 1.5, r.rect.y_range().shrink(6.0), st(3.0, c));
    ui.interact(r.rect, ui.id().with(("note", label)), Sense::click())
}

/// 空状态：图标 + 标题 + 说明 + 可选按钮；返回按钮是否被点
/// 空状态的小插图
#[derive(Clone, Copy)]
enum Art {
    Board(Board),
    Search,
    Bell,
    Error,
    Done,
}

fn empty(ui: &mut Ui, art: Art, title: &str, desc: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    ui.add_space(56.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(120.0, 84.0), Sense::hover());
        let p = ui.painter();
        let c = r.center();
        let col = match art {
            Art::Error => ERR(),
            Art::Done => OK(),
            Art::Board(Board::Help) => WARN(),
            Art::Board(Board::Share) => OK(),
            Art::Board(Board::Notice) => VIOLET(),
            _ => ACCENT(),
        };
        // 底下一团柔光 + 两张错开的卡片，中间是图标
        p.circle_filled(c + vec2(0.0, 4.0), 40.0, col.gamma_multiply(0.07));
        let card = |dx: f32, dy: f32, a: f32| {
            let rr = Rect::from_center_size(c + vec2(dx, dy), vec2(58.0, 40.0));
            p.rect(rr, CornerRadius::same(8), BG1(), st(1.0, LINE().gamma_multiply(a)), egui::StrokeKind::Inside);
            rr
        };
        card(-14.0, -8.0, 0.6);
        let front = card(10.0, 6.0, 1.0);
        for k in 0..2 {
            let y = front.top() + 22.0 + k as f32 * 7.0;
            p.line_segment([pos2(front.left() + 10.0, y), pos2(front.right() - if k == 0 { 12.0 } else { 24.0 }, y)], st(2.0, LINE()));
        }
        let ic = Rect::from_center_size(pos2(front.left() + 16.0, front.top() + 11.0), vec2(13.0, 13.0));
        match art {
            Art::Board(b) => w::board_icon(p, ic, b, col),
            Art::Bell => w::bell_icon(p, ic, col),
            Art::Search => draw_icon(p, ic, Icon::Search, col),
            Art::Error => {
                let m = ic.center();
                p.line_segment([m + vec2(0.0, -5.0), m + vec2(0.0, 1.5)], st(1.8, col));
                p.circle_filled(m + vec2(0.0, 5.0), 1.2, col);
            }
            Art::Done => {
                let m = ic.center();
                p.add(egui::Shape::line(vec![m + vec2(-5.0, 0.0), m + vec2(-1.5, 4.0), m + vec2(5.5, -4.0)], st(1.8, col)));
            }
        }
        ui.add_space(10.0);
        ui.label(RichText::new(title).font(FontId::new(14.0, bold())).color(TEXT2()));
        if !desc.is_empty() {
            ui.add_space(2.0);
            ui.set_max_width(280.0);
            ui.label(RichText::new(desc).size(12.0).color(TEXT3()));
        }
        if let Some(a) = action {
            ui.add_space(10.0);
            if w::primary(ui, a, true).clicked() {
                clicked = true;
            }
        }
    });
    clicked
}

fn pick_image() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title(t("选择图片"))
        .add_filter(t("图片"), &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
        .pick_file()
}

/// 把文字加进 LayoutJob，搜索关键词（不分大小写）标成强调色底
fn hl_append(job: &mut egui::text::LayoutJob, text: &str, q: &str, f: egui::TextFormat) {
    let mut hf = f.clone();
    hf.background = ACCENT().gamma_multiply(0.22);
    hf.color = TEXT();
    if q.is_empty() {
        job.append(text, 0.0, f);
        return;
    }
    let low = text.to_lowercase();
    let ql = q.to_lowercase();
    // 小写后字节位置可能变，只有长度不变时才高亮
    if low.len() != text.len() {
        job.append(text, 0.0, f);
        return;
    }
    let mut i = 0;
    while let Some(k) = low[i..].find(&ql) {
        let (a, b) = (i + k, i + k + ql.len());
        if !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            break;
        }
        job.append(&text[i..a], 0.0, f.clone());
        job.append(&text[a..b], 0.0, hf.clone());
        i = b;
    }
    job.append(&text[i..], 0.0, f);
}

/// 摘要里去掉 Markdown 记号（反斜杠转义、反引号、粗体星号、图片）
fn plain_excerpt(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                if let Some(n) = it.next() {
                    o.push(n);
                }
            }
            '`' | '*' => {}
            '\n' => o.push(' '),
            _ => o.push(c),
        }
    }
    // ![](img:xxx) → [图片]
    while let Some(a) = o.find("![](img:") {
        let end = o[a..].find(')').map(|e| a + e + 1).unwrap_or(o.len());
        o.replace_range(a..end, "▣");
    }
    o
}

/// 这一帧按了 Ctrl+Enter（看按键事件自带的修饰键，按得很快时 i.modifiers 可能已经松开）
fn ctrl_enter(ui: &Ui) -> bool {
    ui.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, modifiers, .. } if modifiers.command))
    })
}

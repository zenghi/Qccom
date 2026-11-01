//! 命令面板（Ctrl+Shift+P）：所有功能按名字搜索、回车执行。
//! 不常用的功能平时不占界面，需要时在这里一搜就有。

use super::*;
use crate::cindex;
use crate::keymap::{self, chord_str, ACTIONS};
use egui::Pos2;

/// 面板里的一项
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Item {
    Act(Act),
    Theme(&'static str),
    Lang(usize),
    LangAuto,
    Prefs(prefs::Cat),
    Tool(toolbox::Tool),
    /// 连接保存的 SSH / Telnet 主机
    Host(usize),
    /// 并排：把某个页面放到副页面
    Side(super::work::Pane),
    /// 布局方案（下标：内置在前）
    Scheme(usize),
    SaveLayout,
    ResetLayout,
}

#[derive(Default)]
pub struct Palette {
    q: String,
    sel: usize,
    focus: bool,
}

impl App {
    pub(super) fn open_palette(&mut self) {
        self.palette = Some(Palette { focus: true, ..Default::default() });
    }

    /// (分类, 名称, 快捷键)
    fn item_text(&self, it: Item) -> (String, String, String) {
        match it {
            Item::Act(a) => {
                let d = keymap::def(a);
                (d.scope.label().to_string(), keymap::act_label(a), self.km.bindings(a).first().map(|c| chord_str(c)).unwrap_or_default())
            }
            Item::Theme(m) => (
                t("界面主题").to_string(),
                match m {
                    "dark" => t("深色"),
                    "light" => t("浅色"),
                    _ => t("跟随系统"),
                }
                .to_string(),
                String::new(),
            ),
            Item::Lang(i) => (t("语言").to_string(), i18n::LANGS[i].1.to_string(), String::new()),
            Item::LangAuto => (t("语言").to_string(), t("跟随系统").to_string(), String::new()),
            Item::Prefs(c) => (t("设置").to_string(), c.label().to_string(), String::new()),
            Item::Tool(tool) => (t("工具箱").to_string(), toolbox::tool_text(tool).0.to_string(), String::new()),
            Item::Host(i) => {
                let h = self.s.tb.ssh_hosts.get(i).cloned().unwrap_or_default();
                (h.proto.name().to_string(), format!("{} {}  {}", t("连接"), h.title(), h.addr()), String::new())
            }
            Item::Side(p) => (t("并排").to_string(), p.label(), String::new()),
            Item::Scheme(i) => (t("布局").to_string(), self.all_schemes().get(i).map(|s| s.name.clone()).unwrap_or_default(), String::new()),
            Item::SaveLayout => (t("布局").to_string(), t("保存当前布局").to_string(), String::new()),
            Item::ResetLayout => (t("布局").to_string(), t("重置布局（回到单页）").to_string(), String::new()),
        }
    }

    fn palette_items(&self) -> Vec<Item> {
        let n_macros = self.code.s.macros.len();
        let mut v: Vec<Item> = ACTIONS
            .iter()
            .filter(|d| d.act != Act::Palette && !matches!(d.act, Act::Macro(i) if i as usize >= n_macros))
            .map(|d| Item::Act(d.act))
            .collect();
        v.extend(["dark", "light", "system"].map(Item::Theme));
        v.push(Item::LangAuto);
        v.extend((0..i18n::LANGS.len()).map(Item::Lang));
        v.extend(prefs::Cat::ALL.iter().map(|c| Item::Prefs(*c)));
        v.extend(toolbox::groups().iter().flat_map(|g| g.1.iter()).map(|t| Item::Tool(*t)));
        v.extend((0..self.s.tb.ssh_hosts.len()).map(Item::Host));
        use super::work::Pane;
        let main = self.main_pane();
        v.extend(
            [Pane::Debug, Pane::Code, Pane::Git, Pane::Compare, Pane::Files, Pane::Community]
                .into_iter()
                .chain(toolbox::groups().iter().flat_map(|g| g.1.iter()).map(|t| Pane::Tool(*t)))
                .filter(|p| Some(*p) != main)
                .map(Item::Side),
        );
        v.extend((0..self.all_schemes().len()).map(Item::Scheme));
        v.push(Item::SaveLayout);
        v.push(Item::ResetLayout);
        v
    }

    pub(super) fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(pal) = self.palette.as_ref() else { return };
        let q = pal.q.trim().to_string();
        let mut items: Vec<(i32, Item, (String, String, String))> = self
            .palette_items()
            .into_iter()
            .filter_map(|it| {
                let tx = self.item_text(it);
                let hay = format!("{}: {}", tx.0, tx.1);
                cindex::fuzzy(&hay, &q).map(|sc| (sc, it, tx))
            })
            .collect();
        if !q.is_empty() {
            items.sort_by(|a, b| b.0.cmp(&a.0));
        } else {
            // 没输入时，最近用过的排最前
            let rank = |it: &Item| self.s.recent_cmds.iter().position(|r| *r == item_key(*it)).unwrap_or(usize::MAX);
            items.sort_by_key(|(_, it, _)| rank(it));
        }
        let pal = self.palette.as_mut().unwrap();
        pal.sel = pal.sel.min(items.len().saturating_sub(1));
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            pal.sel = pal.sel.saturating_sub(1);
        }
        if down && pal.sel + 1 < items.len() {
            pal.sel += 1;
        }
        let mut chosen = if enter { items.get(pal.sel).map(|x| x.1) } else { None };
        let mut close = esc;
        let screen = ctx.screen_rect();
        let w = 600.0f32.min(screen.width() - 40.0);
        // 背后压暗，点面板外面关闭
        ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("palette-dim")))
            .rect_filled(screen, 0.0, shade(if is_light() { 40 } else { 110 }));
        let area = egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - w / 2.0, screen.top() + 56.0))
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
                        let te = ui.add_sized(
                            vec2(w, 34.0),
                            egui::TextEdit::singleline(&mut pal.q)
                                .hint_text(t("输入命令名称，例如「主题」「清屏」「快捷键」"))
                                .font(FontId::proportional(15.0))
                                .margin(vec2(10.0, 7.0)),
                        );
                        if pal.focus {
                            te.request_focus();
                            pal.focus = false;
                        }
                        if te.changed() {
                            pal.sel = 0;
                        }
                        ui.add_space(6.0);
                        if items.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(t("没有匹配项")).color(TEXT3()));
                            ui.add_space(6.0);
                        }
                        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, (_, it, (cat, name, keys))) in items.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
                                let on = i == pal.sel;
                                if on && (up || down) {
                                    ui.scroll_to_rect(r, None);
                                }
                                let p = ui.painter();
                                if on || resp.hovered() {
                                    p.rect_filled(r, CornerRadius::same(6), if on { ACCENT().gamma_multiply(0.14) } else { BG3() });
                                }
                                let g = p.layout_no_wrap(format!("{cat}: "), FontId::proportional(13.0), TEXT3());
                                let gw = g.size().x;
                                p.galley(pos2(r.left() + 12.0, r.center().y - g.size().y / 2.0), g, TEXT3());
                                p.text(pos2(r.left() + 12.0 + gw, r.center().y), Align2::LEFT_CENTER, name, FontId::proportional(13.5), TEXT());
                                if !keys.is_empty() {
                                    key_caps(p, pos2(r.right() - 10.0, r.center().y), keys);
                                }
                                if resp.clicked() {
                                    chosen = Some(*it);
                                }
                            }
                        });
                    });
            });
        let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
        if outside {
            close = true;
        }
        if let Some(it) = chosen {
            self.palette = None;
            let key = item_key(it);
            self.s.recent_cmds.retain(|r| *r != key);
            self.s.recent_cmds.insert(0, key);
            self.s.recent_cmds.truncate(12);
            self.run_item(ctx, it);
        } else if close {
            self.palette = None;
        }
    }

    pub(super) fn run_item(&mut self, ctx: &egui::Context, it: Item) {
        match it {
            Item::Act(a) => {
                // 页面命令先切到对应页面再执行
                let scope = keymap::def(a).scope;
                match scope {
                    Scope::Debug if self.page != Page::Debug => self.set_page(Page::Debug),
                    Scope::Code if self.page != Page::Code => self.set_page(Page::Code),
                    Scope::Git if self.page != Page::Git => self.set_page(Page::Git),
                    _ => {}
                }
                let rest = self.run_global(ctx, vec![a]);
                if !rest.is_empty() {
                    self.pending_acts.extend(rest);
                }
            }
            Item::Theme(m) => self.set_theme(ctx, m),
            Item::Lang(i) => {
                let code = i18n::LANGS[i].0;
                self.s.lang = code.to_string();
                i18n::apply(code);
            }
            Item::LangAuto => {
                self.s.lang = "auto".into();
                i18n::apply("auto");
            }
            Item::Prefs(c) => self.open_settings(c),
            Item::Tool(tool) => self.open_tool(tool),
            Item::Host(i) => {
                if let Some(h) = self.s.tb.ssh_hosts.get(i).cloned() {
                    self.open_tool(toolbox::Tool::Ssh);
                    self.ssh_connect(&h);
                }
            }
            Item::Side(p) => {
                let pos = self.fit_pos(None).or(Some(super::work::DockPos::Bottom));
                self.open_side(p, pos, true);
            }
            Item::Scheme(i) => {
                if let Some(sc) = self.all_schemes().get(i) {
                    let name = sc.name.clone();
                    self.apply_scheme(&name);
                }
            }
            Item::SaveLayout => {
                let n = self.s.work.schemes.len() + 1;
                let name = tf("我的布局 {}", &[&n]);
                self.save_scheme(&name);
            }
            Item::ResetLayout => self.reset_layout(),
        }
    }
}

fn item_key(it: Item) -> String {
    match it {
        Item::Act(a) => keymap::def(a).id.to_string(),
        Item::Theme(m) => format!("theme.{m}"),
        Item::Lang(i) => format!("lang.{}", i18n::LANGS[i].0),
        Item::LangAuto => "lang.auto".into(),
        Item::Prefs(c) => format!("prefs.{}", c.id()),
        Item::Tool(tool) => format!("tool.{tool:?}"),
        Item::Host(i) => format!("host.{i}"),
        Item::Side(p) => format!("side.{p:?}"),
        Item::Scheme(i) => format!("scheme.{i}"),
        Item::SaveLayout => "layout.save".into(),
        Item::ResetLayout => "layout.reset".into(),
    }
}

/// 右对齐画一组按键帽，例如「Ctrl」「Shift」「P」
pub(crate) fn key_caps(p: &egui::Painter, right: Pos2, keys: &str) -> f32 {
    let font = FontId::proportional(11.5);
    let mut x = right.x;
    let parts: Vec<String> = keys
        .split(' ')
        .flat_map(|combo| {
            let mut v: Vec<String> = combo.split('+').filter(|s| !s.is_empty()).map(str::to_string).collect();
            if combo.ends_with("++") {
                v.push("+".into());
            }
            v.push(" ".into());
            v
        })
        .collect();
    for part in parts.iter().rev().skip(1) {
        if part == " " {
            x -= 6.0;
            continue;
        }
        let g = p.layout_no_wrap(part.clone(), font.clone(), TEXT2());
        let w = (g.size().x + 10.0).max(20.0);
        let r = Rect::from_min_size(pos2(x - w, right.y - 10.0), vec2(w, 20.0));
        p.rect(r, CornerRadius::same(4), BG1(), st(1.0, LINE_HI()), egui::StrokeKind::Inside);
        p.galley(pos2(r.center().x - g.size().x / 2.0, r.center().y - g.size().y / 2.0), g, TEXT2());
        x -= w + 3.0;
    }
    right.x - x
}

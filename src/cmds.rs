//! 快捷命令面板：多组自定义命令，一键发送，可整组依次发送。

use crate::i18n::{t, tf};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Rect, RichText, Sense, Shape};
use serde::{Deserialize, Serialize};

use crate::app::{parse_hex, LineEnding};
use crate::logbuf::Encoding;
use crate::theme::*;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct QuickCmd {
    pub name: String,
    pub payload: String,
    pub hex: bool,
    /// None = 跟随命令栏的换行设置
    pub ending: Option<LineEnding>,
}

impl Default for QuickCmd {
    fn default() -> Self {
        Self {
            name: String::new(),
            payload: String::new(),
            hex: false,
            ending: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CmdGroup {
    pub name: String,
    pub cmds: Vec<QuickCmd>,
    /// 整组依次发送时的间隔
    pub interval_ms: u32,
}

impl Default for CmdGroup {
    fn default() -> Self {
        Self {
            name: t("新分组").into(),
            cmds: vec![],
            interval_ms: 200,
        }
    }
}

fn cmd(name: &str, payload: &str, hex: bool) -> QuickCmd {
    QuickCmd {
        name: name.into(),
        payload: payload.into(),
        hex,
        ending: None,
    }
}

/// 第一次使用时给几组示例，用户可以直接改
pub fn default_groups() -> Vec<CmdGroup> {
    vec![
        CmdGroup {
            name: t("AT 指令").into(),
            cmds: vec![
                cmd(t("测试"), "AT", false),
                cmd(t("版本"), "AT+GMR", false),
                cmd(t("复位"), "AT+RST", false),
                cmd(t("扫描 WiFi"), "AT+CWLAP", false),
                cmd(t("查询 IP"), "AT+CIFSR", false),
            ],
            interval_ms: 300,
        },
        CmdGroup {
            name: "Shell".into(),
            cmds: vec![cmd(t("帮助"), "help", false), cmd(t("版本"), "version", false), cmd(t("重启"), "reboot", false)],
            interval_ms: 200,
        },
        CmdGroup {
            name: t("HEX 示例").into(),
            cmds: vec![cmd(t("握手"), "AA 55 01 00 FF", true), cmd(t("读状态"), "AA 55 02 00 FE", true)],
            interval_ms: 100,
        },
    ]
}

impl QuickCmd {
    pub fn bytes(&self, enc: Encoding, global: LineEnding) -> Result<Vec<u8>, String> {
        if self.hex {
            parse_hex(&self.payload)
        } else {
            let mut d = enc.encode(&self.payload);
            d.extend_from_slice(self.ending.unwrap_or(global).bytes());
            Ok(d)
        }
    }
}

pub enum CmdAction {
    Send(QuickCmd),
    SendAll(usize),
}

#[derive(Default)]
pub struct CmdPanel {
    /// 正在编辑：(组, 命令下标；None 表示新建, 草稿)
    editing: Option<(usize, Option<usize>, QuickCmd)>,
    renaming: Option<(usize, String)>,
}

impl CmdPanel {
    pub fn ui(&mut self, ui: &mut egui::Ui, groups: &mut Vec<CmdGroup>, cur: &mut usize, connected: bool, sending_seq: Option<(usize, usize)>) -> Option<CmdAction> {
        let mut action = None;
        if groups.is_empty() {
            groups.push(CmdGroup::default());
        }
        *cur = (*cur).min(groups.len() - 1);

        // 标题
        // 分组标签
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            let mut remove: Option<usize> = None;
            for i in 0..groups.len() {
                if let Some((ri, name)) = &mut self.renaming {
                    if *ri == i {
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(90.0).margin(vec2(6.0, 3.0)));
                        r.request_focus();
                        if r.lost_focus() {
                            if !name.trim().is_empty() {
                                groups[i].name = name.trim().to_string();
                            }
                            self.renaming = None;
                        }
                        continue;
                    }
                }
                let on = *cur == i;
                let r = tab(ui, &groups[i].name, groups[i].cmds.len(), on);
                if r.clicked() {
                    *cur = i;
                    self.editing = None;
                }
                if r.double_clicked() {
                    self.renaming = Some((i, groups[i].name.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button(t("重命名")).clicked() {
                        self.renaming = Some((i, groups[i].name.clone()));
                        ui.close_menu();
                    }
                    if ui.button(t("复制此分组")).clicked() {
                        let mut g = groups[i].clone();
                        g.name = tf("{} 副本", &[&g.name]);
                        groups.insert(i + 1, g);
                        ui.close_menu();
                    }
                    if groups.len() > 1 && ui.button(RichText::new(t("删除分组")).color(ERR())).clicked() {
                        remove = Some(i);
                        ui.close_menu();
                    }
                });
            }
            if let Some(i) = remove {
                groups.remove(i);
                *cur = (*cur).min(groups.len() - 1);
                self.editing = None;
            }
            let r = ui.add(
                egui::Button::new(RichText::new("+").font(FontId::new(14.0, bold())).color(TEXT2()))
                    .fill(BG2())
                    .stroke(st(1.0, LINE()))
                    .corner_radius(CornerRadius::same(6))
                    .min_size(vec2(28.0, 26.0)),
            );
            if r.on_hover_text(t("新建分组")).clicked() {
                groups.push(CmdGroup::default());
                *cur = groups.len() - 1;
                self.renaming = Some((*cur, groups[*cur].name.clone()));
            }
        });
        ui.label(RichText::new(t("双击分组名可重命名，右键更多操作")).small().color(TEXT3()));
        ui.add_space(8.0);

        let gi = *cur;
        // 整组依次发送
        ui.horizontal(|ui| {
            let busy = sending_seq.is_some_and(|(g, _)| g == gi);
            let label = match sending_seq {
                Some((g, left)) if g == gi => tf("发送中… 剩 {} 条", &[&left]),
                _ => t("▶ 依次发送全组").into(),
            };
            let b = egui::Button::new(RichText::new(label).size(12.0).color(if busy { WARN() } else { VIOLET() }))
                .fill(VIOLET().gamma_multiply(0.08))
                .stroke(st(1.0, VIOLET().gamma_multiply(0.35)))
                .corner_radius(CornerRadius::same(6))
                .min_size(vec2(0.0, 26.0));
            if ui.add_enabled(connected && !groups[gi].cmds.is_empty(), b).on_hover_text(t("按顺序发送本组全部命令")).clicked() {
                action = Some(CmdAction::SendAll(gi));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::DragValue::new(&mut groups[gi].interval_ms).range(0..=60_000).suffix(" ms").speed(10))
                    .on_hover_text(t("依次发送时每条之间的间隔"));
                // 地方不够（比如英文按钮更长）时就不显示“间隔”两个字，悬浮提示里有说明
                let g = ui.fonts(|f| f.layout_no_wrap(t("间隔").into(), egui::TextStyle::Small.resolve(ui.style()), TEXT3()));
                if ui.available_width() > g.size().x + 8.0 {
                    ui.label(RichText::new(t("间隔")).small().color(TEXT3()));
                }
            });
        });
        ui.add_space(6.0);

        // 命令列表
        let mut del: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let n = groups[gi].cmds.len();
            for ci in 0..n {
                if matches!(&self.editing, Some((g, Some(c), _)) if *g == gi && *c == ci) {
                    self.editor(ui, groups);
                    continue;
                }
                let c = &groups[gi].cmds[ci];
                let r = cmd_card(ui, c, ci, connected);
                if r.clicked() {
                    if connected {
                        action = Some(CmdAction::Send(c.clone()));
                    }
                }
                let c = c.clone();
                r.context_menu(|ui| {
                    if ui.button(t("编辑")).clicked() {
                        self.editing = Some((gi, Some(ci), c.clone()));
                        ui.close_menu();
                    }
                    if ci > 0 && ui.button(t("上移")).clicked() {
                        swap = Some((ci, ci - 1));
                        ui.close_menu();
                    }
                    if ci + 1 < n && ui.button(t("下移")).clicked() {
                        swap = Some((ci, ci + 1));
                        ui.close_menu();
                    }
                    if ui.button(RichText::new(t("删除")).color(ERR())).clicked() {
                        del = Some(ci);
                        ui.close_menu();
                    }
                });
            }
            if matches!(&self.editing, Some((g, None, _)) if *g == gi) {
                self.editor(ui, groups);
            } else if add_button(ui).clicked() {
                self.editing = Some((gi, None, QuickCmd::default()));
            }
            ui.add_space(4.0);
            ui.label(RichText::new(t("点击发送 · 右键编辑 / 排序 / 删除")).small().color(TEXT3()));
        });
        if let Some(i) = del {
            groups[gi].cmds.remove(i);
        }
        if let Some((a, b)) = swap {
            groups[gi].cmds.swap(a, b);
        }
        action
    }

    fn editor(&mut self, ui: &mut egui::Ui, groups: &mut [CmdGroup]) {
        let Some((gi, ci, draft)) = &mut self.editing else { return };
        let mut done: Option<bool> = None;
        egui::Frame::new()
            .fill(BG2())
            .stroke(st(1.0, ACCENT().gamma_multiply(0.5)))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(if ci.is_some() { t("编辑命令") } else { t("新命令") }).font(FontId::new(12.0, bold())).color(ACCENT()));
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .hint_text(t("名称（可留空）"))
                        .desired_width(f32::INFINITY)
                        .margin(vec2(8.0, 4.0)),
                );
                let hint = if draft.hex { t("HEX，例如 AA 55 01") } else { t("要发送的内容") };
                ui.add(
                    egui::TextEdit::singleline(&mut draft.payload)
                        .hint_text(hint)
                        .desired_width(f32::INFINITY)
                        .font(FontId::new(13.0, FontFamily::Monospace))
                        .margin(vec2(8.0, 4.0)),
                );
                ui.horizontal(|ui| {
                    ui.checkbox(&mut draft.hex, "HEX");
                    ui.add_enabled_ui(!draft.hex, |ui| {
                        let name = draft.ending.map_or(t("跟随命令栏"), |e| e.name());
                        egui::ComboBox::from_id_salt("qc_le").width(96.0).selected_text(name).show_ui(ui, |ui| {
                            ui.selectable_value(&mut draft.ending, None, t("跟随命令栏"));
                            for le in [LineEnding::None, LineEnding::Lf, LineEnding::Cr, LineEnding::CrLf] {
                                ui.selectable_value(&mut draft.ending, Some(le), le.name());
                            }
                        });
                    });
                });
                let err = if draft.hex && !draft.payload.trim().is_empty() { parse_hex(&draft.payload).err() } else { None };
                if let Some(e) = &err {
                    ui.label(RichText::new(e).small().color(ERR()));
                }
                ui.horizontal(|ui| {
                    let ok = egui::Button::new(RichText::new(t("保存")).color(ON_ACCENT()).font(FontId::new(12.5, bold())))
                        .fill(ACCENT())
                        .corner_radius(CornerRadius::same(5));
                    if ui.add_enabled(!draft.payload.trim().is_empty() && err.is_none(), ok).clicked() {
                        done = Some(true);
                    }
                    if ui.button(t("取消")).clicked() {
                        done = Some(false);
                    }
                });
            });
        match done {
            Some(true) => {
                let mut c = draft.clone();
                if c.name.trim().is_empty() {
                    c.name = c.payload.chars().take(24).collect();
                }
                match ci {
                    Some(i) => groups[*gi].cmds[*i] = c,
                    None => groups[*gi].cmds.push(c),
                }
                self.editing = None;
            }
            Some(false) => self.editing = None,
            None => {}
        }
    }
}

fn tab(ui: &mut egui::Ui, name: &str, n: usize, on: bool) -> egui::Response {
    let font = FontId::new(12.0, if on { bold() } else { FontFamily::Proportional });
    let num = FontId::new(10.5, FontFamily::Monospace);
    let tw = ui.fonts(|f| f.layout_no_wrap(name.to_string(), font.clone(), TEXT()).size().x);
    let nw = ui.fonts(|f| f.layout_no_wrap(n.to_string(), num.clone(), TEXT()).size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2(tw + nw + 26.0, 26.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    let fill = if on {
        ACCENT().gamma_multiply(0.12)
    } else if hov {
        BG3()
    } else {
        BG2()
    };
    p.rect(
        rect,
        CornerRadius::same(6),
        fill,
        st(1.0, if on { ACCENT().gamma_multiply(0.5) } else { LINE() }),
        egui::StrokeKind::Inside,
    );
    p.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, name, font, if on { ACCENT() } else { TEXT2() });
    p.text(pos2(rect.right() - 9.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), num, TEXT3());
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn cmd_card(ui: &mut egui::Ui, c: &QuickCmd, idx: usize, connected: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 46.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    let pressed = resp.is_pointer_button_down_on();
    let fill = if pressed && connected {
        VIOLET().gamma_multiply(0.18)
    } else if hov {
        BG3()
    } else {
        BG2()
    };
    p.rect(
        rect,
        CornerRadius::same(7),
        fill,
        st(1.0, if hov && connected { VIOLET().gamma_multiply(0.6) } else { LINE() }),
        egui::StrokeKind::Inside,
    );
    // 序号徽标（Alt+数字快捷键）
    let badge = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(20.0, 20.0));
    p.rect_filled(badge, CornerRadius::same(5), if idx < 9 { BG3() } else { BG2() });
    if idx < 9 {
        p.text(badge.center(), Align2::CENTER_CENTER, (idx + 1).to_string(), FontId::new(11.0, mono_bold()), TEXT2());
    }
    let x = rect.left() + 36.0;
    let maxw = rect.width() - 36.0 - 52.0;
    let name = clip(ui, &c.name, FontId::new(12.5, bold()), maxw);
    p.text(
        pos2(x, rect.top() + 15.0),
        Align2::LEFT_CENTER,
        name,
        FontId::new(12.5, bold()),
        if connected { TEXT() } else { TEXT2() },
    );
    let payload = clip(ui, &c.payload, FontId::new(11.0, FontFamily::Monospace), maxw);
    p.text(pos2(x, rect.top() + 32.0), Align2::LEFT_CENTER, payload, FontId::new(11.0, FontFamily::Monospace), TEXT3());
    // 右侧：HEX 标记 / 发送箭头
    if c.hex {
        let r = Rect::from_min_size(pos2(rect.right() - 46.0, rect.top() + 8.0), vec2(32.0, 15.0));
        p.rect_filled(r, CornerRadius::same(3), WARN().gamma_multiply(0.14));
        p.text(r.center(), Align2::CENTER_CENTER, "HEX", FontId::new(9.5, mono_bold()), WARN());
    }
    if hov && connected {
        let c0 = pos2(rect.right() - 20.0, rect.bottom() - 14.0);
        p.add(Shape::convex_polygon(
            vec![c0 + vec2(-4.0, -5.0), c0 + vec2(5.0, 0.0), c0 + vec2(-4.0, 5.0)],
            VIOLET(),
            egui::Stroke::NONE,
        ));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let tip = if connected {
        t("点击发送，右键编辑")
    } else {
        t("连接串口后可发送；右键编辑")
    };
    resp.on_hover_text(tip)
}

fn add_button(ui: &mut egui::Ui) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
    let p = ui.painter();
    let c = if resp.hovered() { ACCENT() } else { TEXT3() };
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    for seg in pts.windows(2) {
        p.extend(Shape::dashed_line(seg, st(1.0, if resp.hovered() { ACCENT().gamma_multiply(0.6) } else { LINE_HI() }), 4.0, 4.0));
    }
    p.text(rect.center(), Align2::CENTER_CENTER, t("+  添加命令"), FontId::new(12.5, FontFamily::Proportional), c);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 文字超宽时截断加省略号
fn clip(ui: &egui::Ui, s: &str, font: FontId, maxw: f32) -> String {
    let w = |t: &str| ui.fonts(|f| f.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x);
    if w(s) <= maxw {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let t: String = chars[..mid].iter().collect::<String>() + "…";
        if w(&t) <= maxw {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "…"
}

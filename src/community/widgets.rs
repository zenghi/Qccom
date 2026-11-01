//! 社区页的小控件：头像、状态标签、投票胶囊、板块图标、骨架屏、按钮。

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Response, Sense, Stroke, Ui};

use super::Board;
use crate::theme::*;

/// 和后台网页同一个算法（FNV-1a），同一个人在网页和客户端里头像一样
fn hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for c in s.chars() {
        h ^= c as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color32::from_rgb(((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8)
}

/// 头像：有上传的图片用图片，否则按种子画对称像素图案。圆形。
pub fn avatar(ui: &mut Ui, seed: &str, tex: Option<egui::TextureId>, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    paint_avatar(ui.painter(), rect, seed, tex);
    resp
}

pub fn paint_avatar(p: &egui::Painter, rect: Rect, seed: &str, tex: Option<egui::TextureId>) {
    let r = rect.width() / 2.0;
    if let Some(t) = tex {
        // 圆形裁剪：用带纹理的圆形网格
        let mut mesh = egui::Mesh::with_texture(t);
        let n = 32;
        let c = rect.center();
        mesh.colored_vertex(c, Color32::WHITE);
        mesh.vertices[0].uv = pos2(0.5, 0.5);
        for i in 0..=n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            let d = vec2(a.cos(), a.sin());
            mesh.vertices.push(egui::epaint::Vertex { pos: c + d * r, uv: pos2(0.5 + d.x * 0.5, 0.5 + d.y * 0.5), color: Color32::WHITE });
            if i > 0 {
                mesh.add_triangle(0, i as u32, i as u32 + 1);
            }
        }
        p.add(mesh);
        p.circle_stroke(c, r, st(1.0, LINE()));
        return;
    }
    let h = hash(seed);
    let hue = (h % 360) as f32;
    let (fg, bg) = if is_light() { (hsl(hue, 0.55, 0.48), hsl(hue, 0.45, 0.90)) } else { (hsl(hue, 0.62, 0.58), hsl(hue, 0.30, 0.18)) };
    p.circle_filled(rect.center(), r, bg);
    // 7×7 的格子里画中间 5×5，左右对称
    let cell = rect.width() / 7.0 * 0.92;
    let o = rect.center() - vec2(cell * 2.5, cell * 2.5);
    for y in 0..5 {
        for x in 0..3 {
            if (h >> (y * 3 + x)) & 1 == 1 {
                for xx in [x, 4 - x] {
                    let rr = Rect::from_min_size(o + vec2(xx as f32 * cell, y as f32 * cell), vec2(cell + 0.3, cell + 0.3));
                    p.rect_filled(rr, 0.0, fg);
                }
            }
        }
    }
}

pub fn status_color(s: &str) -> Color32 {
    match s {
        "planned" => VIOLET(),
        "done" => OK(),
        "declined" => TEXT3(),
        _ => ACCENT(),
    }
}

/// 状态小标签：圆点 + 文字
pub fn status_badge(ui: &mut Ui, s: &str) {
    let c = status_color(s);
    let text = super::status_name(s);
    let font = FontId::new(11.0, FontFamily::Proportional);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, c);
    let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 18.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(9), c.gamma_multiply(0.13));
    p.circle_filled(pos2(r.left() + 8.0, r.center().y), 2.6, c);
    p.galley(pos2(r.left() + 14.0, r.center().y - g.size().y / 2.0), g, c);
}

/// 小标签（置顶、已解决、作者…）
pub fn tag(ui: &mut Ui, text: &str, c: Color32) {
    let font = FontId::new(10.5, FontFamily::Proportional);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, c);
    let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 16.0), Sense::hover());
    let p = ui.painter();
    p.rect(r, CornerRadius::same(4), c.gamma_multiply(0.10), st(1.0, c.gamma_multiply(0.35)), egui::StrokeKind::Inside);
    p.galley(pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, c);
}

/// 竖向投票胶囊：▲ + 票数。点了的高亮青色，按下时有一点缩放
pub fn vote_capsule(ui: &mut Ui, id: egui::Id, score: i64, voted: bool, enabled: bool) -> Response {
    let size = vec2(40.0, 46.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let k = bounce(ui, id, voted);
    let t = ui.ctx().animate_bool_with_time(id.with("v"), voted, 0.15);
    let press = ui.ctx().animate_bool_with_time(id.with("p"), resp.is_pointer_button_down_on(), 0.06);
    let rect = Rect::from_center_size(rect.center(), rect.size() * (1.0 + 0.10 * k - 0.04 * press));
    let p = ui.painter();
    let hov = resp.hovered() && enabled;
    // 投过：实心强调色；没投：浅底
    let fill = lerp(if hov { BG3() } else { BG2() }, ACCENT(), t);
    let stroke = lerp(if hov { LINE_HI() } else { LINE() }, ACCENT(), t);
    p.rect(rect, CornerRadius::same(9), fill, st(1.0, stroke), egui::StrokeKind::Inside);
    let on = Color32::WHITE;
    let c = lerp(if hov { ACCENT() } else { TEXT2() }, on, t);
    let a = pos2(rect.center().x, rect.top() + 14.0 - 2.0 * k);
    p.add(egui::Shape::convex_polygon(vec![a + vec2(0.0, -4.0), a + vec2(5.0, 2.5), a + vec2(-5.0, 2.5)], c, Stroke::NONE));
    p.text(pos2(rect.center().x, rect.bottom() - 13.5), Align2::CENTER_CENTER, score.to_string(), FontId::new(13.0, mono_bold()), lerp(TEXT(), on, t));
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 刚投上票时弹一下：返回 0..1 的弹跳量
pub fn bounce(ui: &Ui, id: egui::Id, on: bool) -> f32 {
    let key = id.with("bounce");
    let (prev, at) = ui.data(|d| d.get_temp::<(bool, f64)>(key)).unwrap_or((on, -10.0));
    let now = ui.input(|i| i.time);
    let at = if on && !prev { now } else { at };
    ui.data_mut(|d| d.insert_temp(key, (on, at)));
    let x = ((now - at) / 0.32) as f32;
    if (0.0..1.0).contains(&x) {
        ui.ctx().request_repaint();
        (x * std::f32::consts::PI).sin() * (1.0 - x)
    } else {
        0.0
    }
}

/// 横向的小投票按钮（详情页标题旁）
pub fn vote_pill(ui: &mut Ui, score: i64, voted: bool, enabled: bool) -> Response {
    let k = bounce(ui, ui.id().with("pill"), voted);
    let text = score.to_string();
    let font = FontId::new(13.0, mono_bold());
    let g = ui.painter().layout_no_wrap(text, font, TEXT());
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 54.0, 30.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    let hov = resp.hovered() && enabled;
    let rect = Rect::from_center_size(rect.center(), rect.size() * (1.0 + 0.08 * k));
    let (fill, stroke, c) = if voted {
        (ACCENT(), ACCENT(), Color32::WHITE)
    } else if hov {
        (BG3(), LINE_HI(), TEXT())
    } else {
        (BG2(), LINE(), TEXT2())
    };
    p.rect(rect, CornerRadius::same(15), fill, st(1.0, stroke), egui::StrokeKind::Inside);
    let a = pos2(rect.left() + 16.0, rect.center().y);
    p.add(egui::Shape::convex_polygon(vec![a + vec2(0.0, -5.0), a + vec2(5.5, 2.5), a + vec2(-5.5, 2.5)], c, Stroke::NONE));
    p.text(pos2(rect.left() + 28.0, rect.center().y), Align2::LEFT_CENTER, super::t("票"), FontId::proportional(12.0), c);
    p.galley(pos2(rect.right() - 12.0 - g.size().x, rect.center().y - g.size().y / 2.0), g, if voted { Color32::WHITE } else { TEXT() });
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), f(a.a(), b.a()))
}

/// 对话气泡图标（回复数、状态栏入口）
pub fn chat_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let s = st(1.4, c);
    let b = Rect::from_min_max(r.min + vec2(1.0, 2.0), r.max - vec2(1.0, 4.0));
    p.rect_stroke(b, CornerRadius::same(4), s, egui::StrokeKind::Middle);
    let tail = pos2(b.left() + b.width() * 0.32, b.bottom());
    p.line_segment([tail, tail + vec2(-2.5, 3.5)], s);
    p.line_segment([tail + vec2(-2.5, 3.5), tail + vec2(3.5, 0.0)], s);
    for i in 0..3 {
        p.circle_filled(pos2(b.center().x + (i as f32 - 1.0) * 3.4, b.center().y), 0.95, c);
    }
}

/// 板块图标：灯泡 / 问号 / 分享 / 喇叭；我的、提醒用通用图标
pub fn board_icon(p: &egui::Painter, r: Rect, b: Board, c: Color32) {
    let s = st(1.4, c);
    let o = r.center();
    match b {
        Board::Idea => {
            p.circle_stroke(o - vec2(0.0, 1.8), 4.6, s);
            p.line_segment([o + vec2(-2.2, 4.0), o + vec2(2.2, 4.0)], s);
            p.line_segment([o + vec2(-1.6, 6.2), o + vec2(1.6, 6.2)], s);
        }
        Board::Help => {
            p.circle_stroke(o, 6.4, s);
            let pts = [o + vec2(-2.2, -1.6), o + vec2(-1.6, -3.4), o + vec2(0.4, -3.8), o + vec2(2.2, -2.6), o + vec2(1.8, -0.8), o + vec2(0.0, 0.4), o + vec2(0.0, 1.6)];
            p.add(egui::Shape::line(pts.to_vec(), s));
            p.circle_filled(o + vec2(0.0, 3.6), 0.9, c);
        }
        Board::Share => {
            let a = o + vec2(-4.0, 0.0);
            let b1 = o + vec2(4.0, -4.0);
            let b2 = o + vec2(4.0, 4.0);
            p.line_segment([a, b1], s);
            p.line_segment([a, b2], s);
            for q in [a, b1, b2] {
                p.circle_filled(q, 2.3, c);
            }
        }
        Board::Notice => {
            let pts = vec![o + vec2(-5.5, -2.0), o + vec2(-2.0, -2.0), o + vec2(4.5, -5.5), o + vec2(4.5, 5.5), o + vec2(-2.0, 2.0), o + vec2(-5.5, 2.0)];
            p.add(egui::Shape::closed_line(pts, s));
            p.line_segment([o + vec2(-3.0, 2.0), o + vec2(-1.5, 6.0)], s);
        }
    }
}

pub fn person_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let s = st(1.4, c);
    let o = r.center();
    p.circle_stroke(o - vec2(0.0, 2.8), 3.0, s);
    let pts: Vec<_> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
            o + vec2(0.0, 6.5) + vec2(a.cos() * 5.6, a.sin() * 4.2)
        })
        .collect();
    p.add(egui::Shape::line(pts, s));
}

pub fn bell_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let s = st(1.4, c);
    let o = r.center();
    let pts = vec![
        o + vec2(-5.0, 3.5),
        o + vec2(-4.0, 1.5),
        o + vec2(-4.0, -1.5),
        o + vec2(-2.6, -4.4),
        o + vec2(0.0, -5.4),
        o + vec2(2.6, -4.4),
        o + vec2(4.0, -1.5),
        o + vec2(4.0, 1.5),
        o + vec2(5.0, 3.5),
    ];
    p.add(egui::Shape::closed_line(pts, s));
    p.line_segment([o + vec2(-1.6, 5.8), o + vec2(1.6, 5.8)], s);
}

pub fn heart(p: &egui::Painter, o: egui::Pos2, c: Color32, filled: bool) {
    let pts: Vec<_> = (0..40)
        .map(|i| {
            let t = i as f32 / 40.0 * std::f32::consts::TAU;
            let x = 16.0 * t.sin().powi(3);
            let y = -(13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos());
            o + vec2(x, y) * 0.34
        })
        .collect();
    if filled {
        // 心形对中心点是星形的，用扇形三角网填充
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(o + vec2(0.0, 0.5), c);
        for q in &pts {
            mesh.colored_vertex(*q, c);
        }
        let n = pts.len() as u32;
        for i in 0..n {
            mesh.add_triangle(0, i + 1, (i + 1) % n + 1);
        }
        p.add(mesh);
    }
    p.add(egui::Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill: Color32::TRANSPARENT, stroke: st(1.3, c).into() }));
}

/// 骨架屏：一条会微微闪的圆角条
pub fn skel(ui: &mut Ui, w: f32, h: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let a = 0.5 + 0.5 * (t * 2.6).sin();
    let c = lerp(BG2(), BG3(), a);
    ui.painter().rect_filled(r, CornerRadius::same((h / 2.0).min(5.0) as u8), c);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
}

/// 列表卡片的骨架
pub fn skel_card(ui: &mut Ui) {
    let w = ui.available_width();
    egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 12)).show(ui, |ui| {
        ui.horizontal(|ui| {
            skel(ui, 44.0, 50.0);
            ui.add_space(10.0);
            ui.vertical(|ui| {
                skel(ui, (w * 0.55).max(80.0), 14.0);
                ui.add_space(8.0);
                skel(ui, (w * 0.7).max(80.0), 10.0);
                ui.add_space(6.0);
                skel(ui, (w * 0.35).max(60.0), 10.0);
            });
        });
    });
}

/// 主按钮（青色实底）
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let font = FontId::new(13.0, bold());
    let g = ui.painter().layout_no_wrap(text.to_string(), font, ON_ACCENT());
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 28.0, 30.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    let fill = if !enabled {
        ACCENT().gamma_multiply(0.35)
    } else if resp.hovered() {
        accent_hover()
    } else {
        ACCENT()
    };
    p.rect_filled(r, CornerRadius::same(6), fill);
    p.galley(r.center() - g.size() / 2.0, g, ON_ACCENT());
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 次要按钮（描边）
pub fn ghost(ui: &mut Ui, text: &str) -> Response {
    let font = FontId::new(12.5, FontFamily::Proportional);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, TEXT());
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 22.0, 28.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect(r, CornerRadius::same(6), if hov { BG3() } else { BG2() }, st(1.0, if hov { LINE_HI() } else { LINE() }), egui::StrokeKind::Inside);
    p.galley(r.center() - g.size() / 2.0, g, if hov { TEXT() } else { TEXT2() });
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 文字按钮（操作栏：引用、编辑、删除…）
pub fn link(ui: &mut Ui, text: &str, c: Color32) -> Response {
    let font = FontId::new(12.0, FontFamily::Proportional);
    let g = ui.painter().layout_no_wrap(text.to_string(), font, c);
    let (r, resp) = ui.allocate_exact_size(g.size() + vec2(10.0, 8.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), BG3());
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    p.galley(r.center() - g.size() / 2.0, g, if resp.hovered() { TEXT() } else { c });
    resp
}

/// 键盘快捷键提示
pub fn kbd(ui: &mut Ui, k: &str) {
    let font = FontId::new(10.5, FontFamily::Monospace);
    let g = ui.painter().layout_no_wrap(k.to_string(), font, TEXT3());
    let (r, _) = ui.allocate_exact_size(vec2(g.size().x.max(8.0) + 8.0, 17.0), Sense::hover());
    let p = ui.painter();
    p.rect(r, CornerRadius::same(4), BG2(), st(1.0, LINE()), egui::StrokeKind::Inside);
    p.galley(r.center() - g.size() / 2.0, g, TEXT3());
}

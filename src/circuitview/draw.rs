//! 元件画法：实物视图（面包板上看到的样子）和原理图符号。全部矢量绘制，元件本身的颜色是实物颜色，
//! 不随深浅主题变化；文字和描边用主题色。坐标都是元件局部格坐标，经旋转 / 翻转后换算到屏幕。

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Painter, Pos2, Shape, Stroke};

use crate::circuit::lib::{self, Feature, Look, PartDef};
use crate::circuit::sim::Vis;
use crate::circuit::{color_bands, Part};
use crate::theme::*;

/// 画布视图：世界（格）坐标 → 屏幕
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub org: Pos2,
    /// 每格多少像素
    pub s: f32,
}

impl View {
    pub fn w2s(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.org.x + x * self.s, self.org.y + y * self.s)
    }
    pub fn s2w(&self, p: Pos2) -> (f32, f32) {
        ((p.x - self.org.x) / self.s, (p.y - self.org.y) / self.s)
    }
}

pub struct Ctx<'a> {
    pub p: &'a Painter,
    pub v: View,
    pub part: &'a Part,
    pub def: &'static PartDef,
    pub vis: Option<&'a Vis>,
    pub burnt: bool,
    /// 动画时间（秒）
    pub t: f64,
    pub sel: bool,
    pub label: String,
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// 色环颜色：黑棕红橙黄绿蓝紫灰白，-1 金，-2 银
pub fn band_color(d: i8) -> Color32 {
    match d {
        0 => Color32::from_rgb(0x20, 0x20, 0x20),
        1 => Color32::from_rgb(0x7a, 0x4a, 0x24),
        2 => Color32::from_rgb(0xd3, 0x2f, 0x2f),
        3 => Color32::from_rgb(0xf5, 0x7c, 0x00),
        4 => Color32::from_rgb(0xfb, 0xc0, 0x2d),
        5 => Color32::from_rgb(0x38, 0x8e, 0x3c),
        6 => Color32::from_rgb(0x19, 0x76, 0xd2),
        7 => Color32::from_rgb(0x7b, 0x1f, 0xa2),
        8 => Color32::from_rgb(0x9e, 0x9e, 0x9e),
        9 => Color32::from_rgb(0xfa, 0xfa, 0xfa),
        -1 => Color32::from_rgb(0xc8, 0xa4, 0x3a),
        _ => Color32::from_rgb(0xb8, 0xb8, 0xc0),
    }
}

impl Ctx<'_> {
    /// 局部坐标 → 屏幕
    pub fn l(&self, x: f32, y: f32) -> Pos2 {
        let (wx, wy) = self.part.to_worldf((x, y));
        self.v.w2s(wx, wy)
    }
    fn d(&self, len: f32) -> f32 {
        len * self.v.s
    }
    fn poly(&self, pts: &[(f32, f32)], fill: Color32, stroke: Stroke) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.l(*x, *y)).collect();
        self.p.add(Shape::convex_polygon(v, fill, stroke));
    }
    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, fill: Color32, stroke: Stroke) {
        self.poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], fill, stroke);
    }
    /// 圆角矩形（近似：四角切一点）
    fn rrect(&self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, fill: Color32, stroke: Stroke) {
        let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
        let k = r * 0.3;
        self.poly(
            &[
                (x0 + r, y0),
                (x1 - r, y0),
                (x1 - k, y0 + k),
                (x1, y0 + r),
                (x1, y1 - r),
                (x1 - k, y1 - k),
                (x1 - r, y1),
                (x0 + r, y1),
                (x0 + k, y1 - k),
                (x0, y1 - r),
                (x0, y0 + r),
                (x0 + k, y0 + k),
            ],
            fill,
            stroke,
        );
    }
    fn line(&self, a: (f32, f32), b: (f32, f32), w: f32, c: Color32) {
        self.p.line_segment([self.l(a.0, a.1), self.l(b.0, b.1)], Stroke::new(self.d(w).max(1.0), c));
    }
    fn path(&self, pts: &[(f32, f32)], w: f32, c: Color32) {
        let v: Vec<Pos2> = pts.iter().map(|(x, y)| self.l(*x, *y)).collect();
        self.p.add(Shape::line(v, Stroke::new(self.d(w).max(1.0), c)));
    }
    fn circle(&self, x: f32, y: f32, r: f32, fill: Color32, stroke: Stroke) {
        self.p.circle(self.l(x, y), self.d(r), fill, stroke);
    }
    fn text(&self, x: f32, y: f32, s: &str, size: f32, c: Color32) {
        if self.v.s * size < 5.0 {
            return;
        }
        self.p.text(self.l(x, y), Align2::CENTER_CENTER, s, FontId::new(self.v.s * size, FontFamily::Proportional), c);
    }
    fn mono(&self, x: f32, y: f32, s: &str, size: f32, c: Color32, align: Align2) {
        if self.v.s * size < 5.0 {
            return;
        }
        self.p.text(self.l(x, y), align, s, FontId::new(self.v.s * size, FontFamily::Monospace), c);
    }
    /// 引脚（金属脚）：从 (x,y) 往 (tx,ty) 画一小段
    fn leg(&self, x: f32, y: f32, tx: f32, ty: f32) {
        self.line((x, y), (tx, ty), 0.12, Color32::from_rgb(0xb0, 0xb4, 0xbc));
    }
    fn glow(&self, x: f32, y: f32, r: f32, c: Color32, amount: f32) {
        let a = amount.clamp(0.0, 1.6);
        if a < 0.02 {
            return;
        }
        for k in 0..5 {
            let rr = r * (1.0 + k as f32 * 0.55 * a);
            let alpha = (60.0 * a / (k as f32 + 1.0)) as u8;
            self.p.circle_filled(self.l(x, y), self.d(rr), Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha));
        }
    }
}

const BODY_STROKE: Color32 = Color32::from_rgb(0x10, 0x10, 0x14);

/// 实物视图
pub fn draw_part(c: &Ctx) {
    let k = c.def.key;
    let s1 = Stroke::new(1.0, BODY_STROKE);
    match &c.def.look {
        Look::Breadboard { cols } => draw_breadboard(c, *cols),
        Look::Resistor | Look::Diode | Look::Zener | Look::Inductor | Look::Bead | Look::Fuse => draw_axial(c),
        Look::Cap => {
            c.leg(0.0, 0.0, 0.2, -0.9);
            c.leg(1.0, 0.0, 0.8, -0.9);
            c.circle(0.5, -1.3, 0.75, Color32::from_rgb(0xe0, 0x9a, 0x2a), s1);
            let val = crate::circuit::fmt_si(c.part.num("c"), "F");
            c.text(0.5, -1.3, &val.replace(' ', ""), 0.36, Color32::from_rgb(0x30, 0x20, 0x08));
        }
        Look::ECap => {
            c.leg(0.0, 0.0, 0.0, -0.7);
            c.leg(1.0, 0.0, 1.0, -0.7);
            let blown = c.burnt;
            c.rrect(
                -0.6,
                -3.3,
                1.6,
                -0.7,
                0.3,
                if blown { Color32::from_rgb(0x55, 0x40, 0x40) } else { Color32::from_rgb(0x1f, 0x3e, 0x8c) },
                s1,
            );
            // 负极标记条
            c.rect(1.0, -3.3, 1.6, -0.7, Color32::from_rgb(0xc9, 0xd2, 0xe6), Stroke::NONE);
            c.text(1.3, -2.0, "−", 0.6, Color32::from_rgb(0x1f, 0x3e, 0x8c));
            c.text(0.2, -2.5, &crate::circuit::fmt_si(c.part.num("c"), "F").replace(' ', ""), 0.34, Color32::WHITE);
            c.text(0.2, -1.6, &format!("{}V", c.part.num("vmax")), 0.3, Color32::from_rgb(0xdd, 0xe3, 0xf0));
        }
        Look::Led => {
            let (_, col) = crate::circuit::models::led(c.part.prop("color"));
            let col = rgb(col);
            c.leg(0.0, 0.0, 0.0, -0.9);
            c.leg(1.0, 0.0, 1.0, -0.6);
            let g = c.vis.map_or(0.0, |v| v.glow);
            let lit = if c.burnt {
                Color32::from_rgb(0x50, 0x48, 0x48)
            } else {
                lerp_col(col.gamma_multiply(0.45), col, g.min(1.0))
            };
            c.rrect(-0.4, -2.5, 1.4, -0.8, 0.8, lit, Stroke::new(1.0, col.gamma_multiply(0.6)));
            c.rect(-0.55, -1.0, 1.55, -0.75, lit.gamma_multiply(0.9), Stroke::NONE);
            if !c.burnt {
                c.glow(0.5, -1.7, 0.6, col, g);
            }
        }
        Look::LedRgb => {
            for (i, x) in [0.0, 1.0, 2.0, 3.0].iter().enumerate() {
                c.leg(*x, 0.0, 1.5, -0.9 + if i == 1 { -0.2 } else { 0.0 });
            }
            let v = c.vis.map(|v| v.rgb).unwrap_or_default();
            let col = Color32::from_rgb((v[0].min(1.0) * 255.0) as u8, (v[1].min(1.0) * 255.0) as u8, (v[2].min(1.0) * 255.0) as u8);
            let base = Color32::from_rgb(0xe8, 0xea, 0xee);
            c.rrect(0.5, -2.9, 2.5, -0.9, 0.9, lerp_col(base.gamma_multiply(0.7), col, (v[0] + v[1] + v[2]).min(1.0)), s1);
            c.glow(1.5, -1.9, 0.7, col, (v[0] + v[1] + v[2]).min(1.2));
        }
        Look::Bridge => {
            for x in 0..4 {
                c.leg(x as f32, 0.0, x as f32, -0.6);
            }
            c.rect(-0.5, -2.5, 3.5, -0.6, Color32::from_rgb(0x22, 0x22, 0x26), s1);
            c.text(0.0, -1.5, "+", 0.5, Color32::WHITE);
            c.text(1.5, -1.5, "~ ~", 0.4, Color32::WHITE);
            c.text(3.0, -1.5, "−", 0.5, Color32::WHITE);
        }
        Look::To92 => {
            for x in 0..3 {
                c.leg(x as f32, 0.0, 1.0 + (x as f32 - 1.0) * 0.5, -0.8);
            }
            let body = if c.burnt { Color32::from_rgb(0x55, 0x4a, 0x4a) } else { Color32::from_rgb(0x24, 0x24, 0x28) };
            c.poly(&[(-0.2, -0.8), (2.2, -0.8), (2.4, -1.4), (2.0, -2.0), (1.0, -2.25), (0.0, -2.0), (-0.4, -1.4)], body, s1);
            c.text(1.0, -1.45, c.part.prop("model"), 0.32, Color32::from_rgb(0xd0, 0xd0, 0xd0));
        }
        Look::To220 => {
            for x in 0..3 {
                c.leg(x as f32, 0.0, x as f32, -0.8);
            }
            let body = if c.burnt { Color32::from_rgb(0x55, 0x4a, 0x4a) } else { Color32::from_rgb(0x22, 0x22, 0x26) };
            c.rect(-0.55, -2.4, 2.55, -0.8, body, s1);
            // 散热片
            c.rect(-0.55, -3.2, 2.55, -2.4, Color32::from_rgb(0xa8, 0xae, 0xb8), s1);
            c.circle(1.0, -2.8, 0.22, Color32::from_rgb(0x50, 0x54, 0x5c), Stroke::NONE);
            c.text(1.0, -1.6, c.part.prop("model"), 0.3, Color32::from_rgb(0xd0, 0xd0, 0xd0));
            if k == "ldo" {
                if let Some(v) = c.vis {
                    if !v.reading.is_empty() {
                        let hot = c.part.num("temp") > 0.0;
                        let _ = hot;
                    }
                }
            }
        }
        Look::Sot23 => {
            c.rect(-0.3, -0.6, 2.3, 0.0, Color32::from_rgb(0x24, 0x24, 0x28), s1);
        }
        Look::Dip { label } => {
            let n = c.def.pins.len() / 2;
            let w = n as f32 - 1.0;
            for (i, pd) in c.def.pins.iter().enumerate() {
                let (x, y) = (pd.at.0 as f32, pd.at.1 as f32);
                let ty = if y > 1.0 { y - 0.5 } else { y + 0.5 };
                c.rect(x - 0.15, y.min(ty), x + 0.15, y.max(ty), Color32::from_rgb(0xb0, 0xb4, 0xbc), Stroke::NONE);
                let _ = i;
            }
            let body = if c.burnt { Color32::from_rgb(0x50, 0x46, 0x46) } else { Color32::from_rgb(0x26, 0x26, 0x2a) };
            c.rect(-0.45, 0.45, w + 0.45, 2.55, body, s1);
            // 缺口和 1 脚标记
            c.circle(-0.45, 1.5, 0.25, Color32::from_rgb(0x14, 0x14, 0x18), Stroke::NONE);
            c.circle(0.0, 2.15, 0.12, Color32::from_rgb(0x90, 0x90, 0x98), Stroke::NONE);
            c.text(w / 2.0, 1.5, label, 0.45, Color32::from_rgb(0xdd, 0xdd, 0xdd));
        }
        Look::Battery => {
            let kind = c.part.prop("type");
            let (body, band) = match kind {
                "9v" => (Color32::from_rgb(0x2e, 0x2e, 0x34), Color32::from_rgb(0xd4, 0xa0, 0x17)),
                "cr2032" => (Color32::from_rgb(0xb8, 0xbc, 0xc4), Color32::from_rgb(0x90, 0x94, 0x9c)),
                "li" | "18650x2" => (Color32::from_rgb(0x2a, 0x6f, 0x4f), Color32::from_rgb(0x1a, 0x4a, 0x34)),
                _ => (Color32::from_rgb(0x30, 0x30, 0x34), Color32::from_rgb(0xc6, 0x8a, 0x10)),
            };
            c.rrect(-4.3, -0.6, -0.6, 3.6, 0.4, body, s1);
            c.rect(-4.3, -0.6, -3.2, 3.6, band, Stroke::NONE);
            c.line((-0.6, 0.0), (0.0, 0.0), 0.15, Color32::from_rgb(0xd3, 0x2f, 0x2f));
            c.line((-0.6, 3.0), (0.0, 3.0), 0.15, Color32::from_rgb(0x30, 0x30, 0x30));
            c.text(-1.0, 0.0, "+", 0.6, Color32::WHITE);
            c.text(-1.0, 3.0, "−", 0.6, Color32::WHITE);
            let spec = crate::circuit::models::battery(kind);
            c.text(-2.2, 1.0, &format!("{}V", spec.vnom), 0.55, Color32::WHITE);
            let soc = c.part.num("soc");
            c.text(-2.2, 2.0, &format!("{soc:.0}%"), 0.4, Color32::from_rgb(0xcc, 0xcc, 0xcc));
        }
        Look::UsbPower => {
            c.rrect(-4.0, -0.9, -0.6, 2.9, 0.3, Color32::from_rgb(0xec, 0xee, 0xf2), s1);
            c.rect(-3.6, -0.4, -2.2, 2.4, Color32::from_rgb(0x9a, 0xa0, 0xa8), Stroke::NONE);
            c.text(-1.4, 0.0, "+5V", 0.4, Color32::from_rgb(0xc0, 0x20, 0x20));
            c.text(-1.4, 2.0, "GND", 0.4, Color32::from_rgb(0x30, 0x30, 0x30));
            c.line((-0.6, 0.0), (0.0, 0.0), 0.15, Color32::from_rgb(0xd3, 0x2f, 0x2f));
            c.line((-0.6, 2.0), (0.0, 2.0), 0.15, Color32::from_rgb(0x30, 0x30, 0x30));
            c.text(-2.9, 1.0, "USB", 0.45, Color32::from_rgb(0x30, 0x30, 0x30));
        }
        Look::Psu => {
            c.rrect(-7.0, -1.4, -0.5, 3.4, 0.3, Color32::from_rgb(0x3a, 0x3e, 0x46), s1);
            c.rect(-6.4, -0.8, -2.0, 1.6, Color32::from_rgb(0x0a, 0x1a, 0x10), Stroke::NONE);
            c.mono(-4.2, -0.1, &format!("{:05.2}V", c.part.num("v")), 0.6, Color32::from_rgb(0x4a, 0xf0, 0x7a), Align2::CENTER_CENTER);
            let i = c.vis.map_or(String::new(), |v| v.reading.clone());
            c.mono(-4.2, 0.9, &i, 0.45, Color32::from_rgb(0xf0, 0xc0, 0x40), Align2::CENTER_CENTER);
            c.circle(-1.2, 0.0, 0.35, Color32::from_rgb(0xd3, 0x2f, 0x2f), Stroke::NONE);
            c.circle(-1.2, 2.0, 0.35, Color32::from_rgb(0x20, 0x20, 0x20), Stroke::new(1.0, Color32::GRAY));
            c.line((-1.2, 0.0), (0.0, 0.0), 0.15, Color32::from_rgb(0xd3, 0x2f, 0x2f));
            c.line((-1.2, 2.0), (0.0, 2.0), 0.15, Color32::from_rgb(0x30, 0x30, 0x30));
        }
        Look::SigGen => {
            c.rrect(-5.0, -0.9, -0.5, 2.9, 0.3, Color32::from_rgb(0x3a, 0x3e, 0x46), s1);
            let wave: Vec<(f32, f32)> = if k == "acsrc" {
                (0..=20).map(|i| (-4.4 + i as f32 * 0.17, 1.0 - 0.6 * (i as f32 / 20.0 * std::f32::consts::TAU).sin())).collect()
            } else {
                vec![(-4.4, 1.5), (-3.6, 1.5), (-3.6, 0.5), (-2.8, 0.5), (-2.8, 1.5), (-2.0, 1.5), (-2.0, 0.5), (-1.2, 0.5)]
            };
            c.path(&wave, 0.1, Color32::from_rgb(0x4a, 0xf0, 0x7a));
            c.line((-0.5, 0.0), (0.0, 0.0), 0.15, Color32::from_rgb(0xd3, 0x2f, 0x2f));
            c.line((-0.5, 2.0), (0.0, 2.0), 0.15, Color32::from_rgb(0x30, 0x30, 0x30));
        }
        Look::Ground => {
            c.line((0.0, 0.0), (0.0, 0.6), 0.1, TEXT2());
            for (i, w) in [0.7, 0.45, 0.2].iter().enumerate() {
                let y = 0.6 + i as f32 * 0.3;
                c.line((-w, y), (*w, y), 0.1, TEXT2());
            }
        }
        Look::Button => {
            c.rect(-0.3, -0.3, 2.3, 3.3, Color32::from_rgb(0x2a, 0x2a, 0x2e), s1);
            let down = c.vis.is_some_and(|v| v.on);
            c.circle(
                1.0,
                1.5,
                0.85,
                if down { Color32::from_rgb(0x55, 0x55, 0x5c) } else { Color32::from_rgb(0x1a, 0x1a, 0x1c) },
                Stroke::new(1.0, Color32::from_rgb(0x60, 0x60, 0x66)),
            );
        }
        Look::Slide => {
            for x in 0..3 {
                c.leg(x as f32, 0.0, x as f32, -0.6);
            }
            c.rect(-0.6, -1.9, 2.6, -0.6, Color32::from_rgb(0x2c, 0x2c, 0x30), s1);
            let on = c.part.prop("on") != "false";
            let x = if on { 0.0 } else { 1.2 };
            c.rect(x - 0.1, -1.7, x + 0.9, -0.8, Color32::from_rgb(0xd8, 0xd8, 0xdc), Stroke::NONE);
        }
        Look::DipSwitch => {
            c.rect(-0.5, 0.4, 3.5, 2.6, Color32::from_rgb(0xc6, 0x28, 0x28), s1);
            let bits: Vec<char> = c.part.prop("bits").chars().collect();
            for i in 0..4 {
                let on = bits.get(i) == Some(&'1');
                let x = i as f32;
                c.rect(x - 0.25, 0.8, x + 0.25, 2.2, Color32::from_rgb(0x30, 0x10, 0x10), Stroke::NONE);
                let y = if on { 0.85 } else { 1.55 };
                c.rect(x - 0.22, y, x + 0.22, y + 0.6, Color32::WHITE, Stroke::NONE);
            }
        }
        Look::Pot => {
            for x in 0..3 {
                c.leg(x as f32, 0.0, x as f32, -0.5);
            }
            c.rect(-0.7, -1.2, 2.7, -0.5, Color32::from_rgb(0x1f, 0x4e, 0x99), s1);
            c.circle(1.0, -2.0, 1.0, Color32::from_rgb(0x2a, 0x2e, 0x36), s1);
            let pos = c.part.num("pos") as f32 / 100.0;
            let a = (-135.0 + 270.0 * pos).to_radians();
            c.line((1.0, -2.0), (1.0 + 0.8 * a.sin(), -2.0 - 0.8 * a.cos()), 0.15, Color32::from_rgb(0xe0, 0xe0, 0xe0));
        }
        Look::Ldr => {
            c.leg(0.0, 0.0, 0.2, -0.9);
            c.leg(1.0, 0.0, 0.8, -0.9);
            c.circle(0.5, -1.5, 0.7, Color32::from_rgb(0xe8, 0xd8, 0xb0), s1);
            c.path(
                &[(0.1, -1.8), (0.9, -1.8), (0.1, -1.5), (0.9, -1.5), (0.1, -1.2), (0.9, -1.2)],
                0.08,
                Color32::from_rgb(0xb0, 0x40, 0x20),
            );
        }
        Look::Ntc => {
            c.leg(0.0, 0.0, 0.3, -0.9);
            c.leg(1.0, 0.0, 0.7, -0.9);
            c.circle(0.5, -1.4, 0.55, Color32::from_rgb(0x1a, 0x1a, 0x1e), s1);
            c.text(0.5, -1.4, "NTC", 0.28, Color32::WHITE);
        }
        Look::Encoder => {}
        Look::Buzzer => {
            c.leg(0.0, 0.0, 0.3, -0.6);
            c.leg(2.0, 0.0, 1.7, -0.6);
            c.circle(1.0, -1.8, 1.2, Color32::from_rgb(0x1a, 0x1a, 0x1e), s1);
            c.circle(1.0, -1.8, 0.25, Color32::from_rgb(0x0a, 0x0a, 0x0a), Stroke::NONE);
            c.text(0.2, -2.8, "+", 0.45, TEXT2());
            if let Some(v) = c.vis {
                if v.on && !c.burnt {
                    // 声波
                    let ph = (c.t * 6.0).fract() as f32;
                    for i in 0..3 {
                        let r = 1.4 + (i as f32 + ph) * 0.5;
                        let a = 200.0 * (1.0 - (i as f32 + ph) / 3.0);
                        c.p.circle_stroke(c.l(1.0, -1.8), c.d(r), Stroke::new(1.5, Color32::from_rgba_unmultiplied(0x2d, 0xe2, 0xe6, a as u8)));
                    }
                    if v.freq > 0.0 {
                        c.text(1.0, -3.6, &format!("{:.0} Hz", v.freq), 0.4, ACCENT());
                    }
                }
            }
        }
        Look::Relay => {
            c.rect(-0.5, -0.5, 4.5, 4.5, Color32::from_rgb(0x1e, 0x5f, 0xc8), s1);
            let on = c.vis.is_some_and(|v| v.on);
            c.text(2.0, 1.2, &format!("{}V", c.part.prop("v")), 0.45, Color32::WHITE);
            c.text(
                2.0,
                2.4,
                if on { "ON" } else { "OFF" },
                0.5,
                if on { Color32::from_rgb(0x9a, 0xff, 0xb0) } else { Color32::from_rgb(0xcc, 0xd6, 0xf0) },
            );
            c.text(3.6, 0.0, "COM", 0.26, Color32::WHITE);
            c.text(3.6, 2.0, "NO", 0.26, Color32::WHITE);
            c.text(3.6, 4.0, "NC", 0.26, Color32::WHITE);
        }
        Look::Motor => {
            c.leg(0.0, 0.0, 0.5, -1.0);
            c.leg(2.0, 0.0, 1.5, -1.0);
            c.rrect(-1.2, -3.8, 3.2, -1.0, 0.5, Color32::from_rgb(0xb4, 0xb8, 0xc0), s1);
            let ang = c.vis.map_or(0.0, |v| v.angle).to_radians();
            // 轴上的扇叶
            for k in 0..3 {
                let a = ang + k as f32 * std::f32::consts::TAU / 3.0;
                c.line((1.0, -2.4), (1.0 + 1.0 * a.cos(), -2.4 + 1.0 * a.sin()), 0.25, Color32::from_rgb(0xe5, 0x39, 0x35));
            }
            c.circle(1.0, -2.4, 0.25, Color32::from_rgb(0x40, 0x40, 0x44), Stroke::NONE);
            if let Some(v) = c.vis {
                if v.rpm.abs() > 1.0 {
                    c.text(1.0, -4.3, &format!("{:.0} rpm", v.rpm), 0.4, ACCENT());
                }
            }
        }
        Look::Seg7 => draw_seg7(c),
        Look::Servo => {}
        Look::Lamp => {
            c.leg(0.0, 0.0, 0.6, -0.8);
            c.leg(2.0, 0.0, 1.4, -0.8);
            let g = c.vis.map_or(0.0, |v| v.glow);
            let col = Color32::from_rgb(0xff, 0xe0, 0x80);
            c.circle(1.0, -1.9, 1.0, lerp_col(Color32::from_rgba_unmultiplied(0xe0, 0xe8, 0xf0, 160), col, g.min(1.0)), s1);
            c.path(&[(0.6, -1.4), (0.8, -2.2), (1.2, -2.2), (1.4, -1.4)], 0.06, Color32::from_rgb(0x80, 0x60, 0x20));
            if !c.burnt {
                c.glow(1.0, -1.9, 0.8, col, g);
            }
        }
        Look::Multimeter => {
            c.rrect(-1.3, -8.8, 4.3, -0.6, 0.5, Color32::from_rgb(0xf2, 0xb8, 0x10), s1);
            c.rect(-0.8, -8.2, 3.8, -6.2, Color32::from_rgb(0xa8, 0xb8, 0x90), Stroke::NONE);
            let r = c.vis.map_or(String::new(), |v| v.reading.clone());
            c.mono(1.5, -7.2, if r.is_empty() { "----" } else { &r }, 0.6, Color32::from_rgb(0x10, 0x18, 0x10), Align2::CENTER_CENTER);
            c.circle(1.5, -4.0, 1.4, Color32::from_rgb(0x30, 0x30, 0x34), s1);
            let mode = c.part.prop("mode");
            let txt = match mode {
                "a" => "A",
                "ohm" => "Ω",
                "beep" => "♪",
                _ => "V",
            };
            c.text(1.5, -4.0, txt, 0.8, Color32::WHITE);
            c.circle(0.0, -1.3, 0.35, Color32::from_rgb(0x10, 0x10, 0x10), Stroke::NONE);
            c.circle(3.0, -1.3, 0.35, Color32::from_rgb(0xd3, 0x2f, 0x2f), Stroke::NONE);
            c.leg(0.0, -1.3, 0.0, 0.0);
            c.leg(3.0, -1.3, 3.0, 0.0);
            c.text(0.0, -2.0, "COM", 0.3, Color32::from_rgb(0x20, 0x20, 0x20));
            c.text(3.0, -2.0, "VΩ", 0.3, Color32::from_rgb(0x20, 0x20, 0x20));
        }
        Look::Ammeter | Look::PowerMeter => {
            let x1 = if matches!(c.def.look, Look::PowerMeter) { 6.0 } else { 4.0 };
            c.rrect(-0.4, -3.3, x1 + 0.4, -0.5, 0.3, Color32::from_rgb(0x30, 0x34, 0x3c), s1);
            c.rect(0.2, -2.8, x1 - 0.2, -1.2, Color32::from_rgb(0x08, 0x10, 0x18), Stroke::NONE);
            let r = c.vis.map_or(String::new(), |v| v.reading.clone());
            c.mono(
                x1 / 2.0,
                -2.0,
                if r.is_empty() { "--- A" } else { &r },
                0.62,
                Color32::from_rgb(0x5a, 0xd8, 0xff),
                Align2::CENTER_CENTER,
            );
            c.leg(0.0, -0.5, 0.0, 0.0);
            c.leg(x1, -0.5, x1, 0.0);
            if x1 > 4.0 {
                c.leg(3.0, -0.5, 3.0, 0.0);
            }
        }
        Look::Probe => {
            let ch = c.part.prop("ch");
            let col = probe_color(ch);
            c.rrect(-0.45, -3.0, 0.45, -0.6, 0.3, col, s1);
            c.leg(0.0, -0.6, 0.0, 0.0);
            c.text(0.0, -1.8, &format!("CH{ch}"), 0.3, Color32::BLACK);
        }
        Look::Logic => {
            c.rrect(-0.5, -3.8, 8.5, -0.6, 0.3, Color32::from_rgb(0x2a, 0x2e, 0x36), s1);
            c.text(4.0, -2.4, "Logic 8CH", 0.5, Color32::WHITE);
            for i in 0..9 {
                c.leg(i as f32, -0.6, i as f32, 0.0);
            }
        }
        Look::Pcb { color, title, feature } => draw_pcb(c, rgb(*color), title, *feature),
    }
    if c.burnt {
        draw_smoke(c);
    }
}

fn lerp_col(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

pub fn probe_color(ch: &str) -> Color32 {
    match ch {
        "2" => Color32::from_rgb(0x4f, 0xc3, 0xf7),
        "3" => Color32::from_rgb(0xf0, 0x62, 0x92),
        "4" => Color32::from_rgb(0x81, 0xc7, 0x84),
        _ => Color32::from_rgb(0xff, 0xd5, 0x4f),
    }
}

fn draw_smoke(c: &Ctx) {
    let [x0, y0, x1, _] = c.def.body;
    let cx = (x0 + x1) / 2.0;
    for i in 0..4 {
        let ph = ((c.t * 0.7 + i as f64 * 0.25).fract()) as f32;
        let y = y0 - ph * 2.0;
        let a = (120.0 * (1.0 - ph)) as u8;
        c.p.circle_filled(
            c.l(cx + (ph * 6.0 + i as f32).sin() * 0.4, y),
            c.d(0.35 + ph * 0.6),
            Color32::from_rgba_unmultiplied(0x90, 0x90, 0x90, a),
        );
    }
}

fn draw_axial(c: &Ctx) {
    let (sx, sy) = c.part.span();
    let (sx, sy) = (sx as f32, sy as f32);
    let len = (sx * sx + sy * sy).sqrt().max(0.01);
    let (ux, uy) = (sx / len, sy / len);
    let (nx, ny) = (-uy, ux);
    let body_len = 2.4_f32.min(len - 0.4).max(0.6);
    let a = (len - body_len) / 2.0;
    let pt = |t: f32, n: f32| (ux * t + nx * n, uy * t + ny * n);
    let lead = Color32::from_rgb(0xb0, 0xb4, 0xbc);
    c.line((0.0, 0.0), pt(a, 0.0), 0.12, lead);
    c.line(pt(len - a, 0.0), (sx, sy), 0.12, lead);
    let h = 0.42;
    let quad = |t0: f32, t1: f32, hh: f32| [pt(t0, -hh), pt(t1, -hh), pt(t1, hh), pt(t0, hh)];
    let s1 = Stroke::new(1.0, BODY_STROKE);
    match c.def.look {
        Look::Resistor => {
            let body = if c.burnt { Color32::from_rgb(0x4a, 0x3a, 0x30) } else { Color32::from_rgb(0xd9, 0xc3, 0x9a) };
            c.poly(&quad(a, len - a, h), body, s1);
            if let Some(b) = color_bands(c.part.num("r")) {
                let span = len - 2.0 * a;
                for (i, d) in b.iter().enumerate() {
                    let t = a + span * [0.18, 0.32, 0.46, 0.8][i];
                    c.poly(&quad(t - 0.09, t + 0.09, h), band_color(*d), Stroke::NONE);
                }
            }
        }
        Look::Diode | Look::Zener => {
            let model = c.part.prop("model");
            let glass = model == "1N4148" || c.def.look == Look::Zener;
            let body = if glass { Color32::from_rgb(0xd8, 0x6a, 0x3a) } else { Color32::from_rgb(0x22, 0x22, 0x26) };
            c.poly(&quad(a, len - a, h * 0.85), body, s1);
            let band = if glass { Color32::from_rgb(0x20, 0x20, 0x20) } else { Color32::from_rgb(0xd0, 0xd0, 0xd0) };
            let t = len - a - 0.35;
            c.poly(&quad(t - 0.12, t + 0.12, h * 0.85), band, Stroke::NONE);
        }
        Look::Inductor => {
            c.poly(&quad(a, len - a, h * 1.2), Color32::from_rgb(0x2a, 0x6e, 0x3a), s1);
            let n = 6;
            for i in 0..n {
                let t = a + (len - 2.0 * a) * (i as f32 + 0.5) / n as f32;
                c.line(pt(t, -h * 1.2), pt(t, h * 1.2), 0.07, Color32::from_rgb(0xc0, 0x80, 0x40));
            }
        }
        Look::Bead => {
            c.poly(&quad(a, len - a, h * 1.1), Color32::from_rgb(0x3a, 0x3a, 0x40), s1);
        }
        Look::Fuse => {
            c.poly(&quad(a, len - a, h), Color32::from_rgba_unmultiplied(0xe0, 0xe8, 0xf0, 120), s1);
            c.poly(&quad(a - 0.1, a + 0.3, h + 0.05), Color32::from_rgb(0xb0, 0xb4, 0xbc), Stroke::NONE);
            c.poly(&quad(len - a - 0.3, len - a + 0.1, h + 0.05), Color32::from_rgb(0xb0, 0xb4, 0xbc), Stroke::NONE);
            let blown = c.burnt;
            if !blown {
                c.line(pt(a + 0.3, 0.0), pt(len - a - 0.3, 0.0), 0.05, Color32::from_rgb(0x90, 0x90, 0x90));
            }
        }
        _ => {}
    }
}

fn draw_seg7(c: &Ctx) {
    c.rect(-0.5, -0.3, 4.5, 5.3, Color32::from_rgb(0x18, 0x18, 0x1c), Stroke::new(1.0, BODY_STROKE));
    let segs = c.vis.map(|v| v.segs).unwrap_or_default();
    let on = Color32::from_rgb(0xff, 0x30, 0x20);
    let off = Color32::from_rgb(0x38, 0x1a, 0x18);
    seg_digit(c, 2.0, 2.5, 1.0, &segs, on, off);
}

/// 画一位七段数码管：中心 (cx, cy)、高度一半 hh
fn seg_digit(c: &Ctx, cx: f32, cy: f32, hh: f32, segs: &[f32; 8], on: Color32, off: Color32) {
    let w = hh * 0.9;
    let pts = [
        ((cx - w / 2.0, cy - hh), (cx + w / 2.0, cy - hh)),
        ((cx + w / 2.0, cy - hh), (cx + w / 2.0, cy)),
        ((cx + w / 2.0, cy), (cx + w / 2.0, cy + hh)),
        ((cx - w / 2.0, cy + hh), (cx + w / 2.0, cy + hh)),
        ((cx - w / 2.0, cy), (cx - w / 2.0, cy + hh)),
        ((cx - w / 2.0, cy - hh), (cx - w / 2.0, cy)),
        ((cx - w / 2.0, cy), (cx + w / 2.0, cy)),
    ];
    for (i, (a, b)) in pts.iter().enumerate() {
        let lv = segs[i];
        c.line(*a, *b, hh * 0.22, if lv > 0.05 { lerp_col(off, on, lv.min(1.0)) } else { off });
    }
    let lv = segs[7];
    c.circle(cx + w / 2.0 + hh * 0.3, cy + hh, hh * 0.12, if lv > 0.05 { on } else { off }, Stroke::NONE);
}

fn segs_of(ch: char) -> [f32; 8] {
    const SEG: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];
    let bits = match ch {
        '0'..='9' => SEG[ch as usize - '0' as usize],
        '-' => 0x40,
        'A' | 'a' => 0x77,
        'b' => 0x7c,
        'C' | 'c' => 0x39,
        'd' => 0x5e,
        'E' | 'e' => 0x79,
        'F' | 'f' => 0x71,
        _ => 0,
    };
    let mut v = [0.0; 8];
    for (i, x) in v.iter_mut().enumerate().take(7) {
        *x = if bits >> i & 1 == 1 { 1.0 } else { 0.0 };
    }
    v
}

fn draw_pcb(c: &Ctx, color: Color32, title: &str, feature: Feature) {
    let [x0, y0, x1, y1] = c.def.body;
    let dim = if c.burnt { color.gamma_multiply(0.5) } else { color };
    c.rrect(x0, y0, x1, y1, 0.4, dim, Stroke::new(1.0, BODY_STROKE));
    let silk = Color32::from_rgb(0xf0, 0xf2, 0xf4);
    // 排针：金色方焊盘 + 标签
    let board = c.def.is_board();
    for (i, pd) in c.def.pins.iter().enumerate() {
        let (x, y) = (pd.at.0 as f32, pd.at.1 as f32);
        let _ = i;
        c.rect(x - 0.28, y - 0.28, x + 0.28, y + 0.28, Color32::from_rgb(0x15, 0x15, 0x15), Stroke::NONE);
        c.circle(x, y, 0.2, Color32::from_rgb(0xd4, 0xb0, 0x4c), Stroke::NONE);
        if c.v.s >= 9.0 {
            // 标签往板子里面写
            let inward = if y <= y0 + 1.5 && board { 1.0 } else { -1.0 };
            let name = short_pin(&pd.name);
            let fs = 0.3_f32.min(0.95 / (name.chars().count() as f32 * 0.33).max(1.0));
            let p = c.l(x, y + inward * 0.75);
            if c.v.s * fs >= 4.5 {
                c.p.text(p, Align2::CENTER_CENTER, name, FontId::new(c.v.s * fs, FontFamily::Proportional), silk);
            }
        }
    }
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let vis = c.vis;
    match feature {
        Feature::Screen { w, h, px, py, color: col } => {
            let (sx0, sy0) = (cx - w / 2.0, y0 + 0.8);
            let (sx1, sy1) = (cx + w / 2.0, (y0 + 0.8 + h).min(y1 - 1.6));
            let lit = vis.is_some_and(|v| v.lit || !v.lines.is_empty() || !v.image.is_empty());
            c.rect(sx0 - 0.2, sy0 - 0.2, sx1 + 0.2, sy1 + 0.2, Color32::from_rgb(0x08, 0x08, 0x0a), Stroke::NONE);
            c.rect(
                sx0,
                sy0,
                sx1,
                sy1,
                if lit && col { Color32::from_rgb(0x10, 0x18, 0x30) } else { Color32::from_rgb(0x02, 0x02, 0x04) },
                Stroke::NONE,
            );
            if let Some(v) = vis {
                if !v.image.is_empty() {
                    draw_sample_image(c, sx0, sy0, sx1, sy1);
                } else if !v.lines.is_empty() {
                    let fg = if col {
                        Color32::WHITE
                    } else if c.def.key == "epaper" {
                        Color32::BLACK
                    } else {
                        Color32::from_rgb(0x9c, 0xe8, 0xff)
                    };
                    if c.def.key == "epaper" {
                        c.rect(sx0, sy0, sx1, sy1, Color32::from_rgb(0xe8, 0xe6, 0xdc), Stroke::NONE);
                    }
                    let n = v.lines.len().max(4) as f32;
                    let lh = ((sy1 - sy0) / n).min(1.2);
                    for (i, l) in v.lines.iter().enumerate() {
                        c.mono(sx0 + 0.3, sy0 + 0.2 + lh * (i as f32 + 0.5), l, lh * 0.8, fg, Align2::LEFT_CENTER);
                    }
                }
            }
            c.text(cx, sy1 + 0.6, &format!("{title}  {px}×{py}"), 0.32, silk);
        }
        Feature::Lcd1602 => {
            c.rect(x0 + 1.0, y0 + 1.2, x1 - 1.0, y0 + 6.5, Color32::from_rgb(0x20, 0x30, 0x10), Stroke::NONE);
            c.rect(x0 + 1.4, y0 + 1.8, x1 - 1.4, y0 + 5.9, Color32::from_rgb(0x7a, 0xc0, 0x30), Stroke::NONE);
            if let Some(v) = vis {
                for (i, l) in v.lines.iter().take(2).enumerate() {
                    let s: String = l.chars().take(16).collect();
                    c.mono(x0 + 1.8, y0 + 2.8 + i as f32 * 2.0, &s, 1.2, Color32::from_rgb(0x10, 0x20, 0x08), Align2::LEFT_CENTER);
                }
            }
            c.text(cx, y1 - 3.0, title, 0.45, silk);
        }
        Feature::Seg4 => {
            c.rect(x0 + 0.6, y0 + 0.6, x1 - 0.6, y1 - 1.4, Color32::from_rgb(0x14, 0x14, 0x16), Stroke::NONE);
            let digits: Vec<char> = vis.map(|v| v.digits.chars().collect()).unwrap_or_default();
            let n = 4;
            let span = (x1 - x0 - 1.6) / n as f32;
            for i in 0..n {
                let ch = digits.get(i).copied().unwrap_or(' ');
                seg_digit(
                    c,
                    x0 + 0.8 + span * (i as f32 + 0.5),
                    (y0 + y1 - 1.6) / 2.0,
                    1.1,
                    &segs_of(ch),
                    Color32::from_rgb(0xff, 0x30, 0x20),
                    Color32::from_rgb(0x30, 0x14, 0x12),
                );
            }
        }
        Feature::Camera => {
            c.circle(cx, cy, 1.6, Color32::from_rgb(0x10, 0x10, 0x12), Stroke::new(1.0, Color32::from_rgb(0x50, 0x50, 0x58)));
            c.circle(cx, cy, 0.8, Color32::from_rgb(0x1a, 0x2a, 0x50), Stroke::NONE);
            c.circle(cx - 0.3, cy - 0.3, 0.2, Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 120), Stroke::NONE);
            c.text(cx, y0 + 0.8, title, 0.4, silk);
        }
        Feature::Antenna => {
            let y = y0 + 0.8;
            c.path(
                &[
                    (x0 + 0.6, y),
                    (x0 + 0.6, y + 0.6),
                    (x0 + 1.4, y + 0.6),
                    (x0 + 1.4, y),
                    (x0 + 2.2, y),
                    (x0 + 2.2, y + 0.6),
                    (x0 + 3.0, y + 0.6),
                    (x0 + 3.0, y),
                ],
                0.12,
                Color32::from_rgb(0xd4, 0xb0, 0x4c),
            );
            c.text(cx, cy, title, 0.5, silk);
        }
        Feature::Shield => {
            c.rect(
                cx - 2.6,
                y0 + 0.8,
                cx + 2.6,
                y0 + 4.6,
                Color32::from_rgb(0xb8, 0xbe, 0xc6),
                Stroke::new(1.0, Color32::from_rgb(0x70, 0x74, 0x7c)),
            );
            c.text(cx, y0 + 2.7, title, 0.42, Color32::from_rgb(0x20, 0x20, 0x24));
            if c.def.key == "cat1" {
                c.text(cx, y0 + 5.6, c.part.prop("model"), 0.4, silk);
            }
        }
        Feature::Usb => {
            c.rect(
                x0 - 0.4,
                cy - 1.0,
                x0 + 1.4,
                cy + 1.0,
                Color32::from_rgb(0xb8, 0xbe, 0xc6),
                Stroke::new(1.0, Color32::from_rgb(0x70, 0x74, 0x7c)),
            );
            c.text(cx + 0.6, cy, title, 0.45, silk);
        }
        Feature::Chip => {
            c.rect(cx - 1.2, cy - 1.2, cx + 1.2, cy + 1.2, Color32::from_rgb(0x18, 0x18, 0x1c), Stroke::NONE);
            c.text(cx, y0 + 0.6, title, 0.38, silk);
        }
        Feature::Sensor => {
            c.rect(cx - 1.6, y0 + 0.6, cx + 1.6, y0 + 3.6, Color32::from_rgb(0x4c, 0x8e, 0xd8), Stroke::NONE);
            for i in 0..4 {
                let y = y0 + 1.1 + i as f32 * 0.65;
                c.line((cx - 1.2, y), (cx + 1.2, y), 0.18, Color32::from_rgb(0x1a, 0x3a, 0x6a));
            }
            c.text(cx, y1 - 1.3, title, 0.38, silk);
        }
        Feature::Ultrasonic => {
            for dx in [-2.2, 2.2] {
                c.circle(cx + dx, cy - 0.5, 1.6, Color32::from_rgb(0xb8, 0xbe, 0xc6), Stroke::new(1.0, Color32::from_rgb(0x70, 0x74, 0x7c)));
                c.circle(cx + dx, cy - 0.5, 1.0, Color32::from_rgb(0x30, 0x30, 0x34), Stroke::NONE);
            }
            c.text(cx, y0 + 0.6, title, 0.38, silk);
        }
        Feature::Relay => {
            c.rect(cx - 2.0, y0 + 0.6, cx + 2.0, y0 + 3.8, Color32::from_rgb(0x1e, 0x5f, 0xc8), Stroke::new(1.0, BODY_STROKE));
            let on = vis.is_some_and(|v| v.on);
            c.circle(
                x0 + 0.7,
                y0 + 0.7,
                0.25,
                if on { Color32::from_rgb(0xff, 0x30, 0x20) } else { Color32::from_rgb(0x40, 0x14, 0x12) },
                Stroke::NONE,
            );
            c.text(cx, y0 + 2.2, if on { "ON" } else { "OFF" }, 0.5, Color32::WHITE);
        }
        Feature::Terminal => {
            c.rect(x0 + 0.4, y0 + 0.4, x0 + 3.0, y0 + 1.6, Color32::from_rgb(0x2c, 0x8a, 0x3c), Stroke::NONE);
            c.text(cx, cy, title, 0.45, silk);
        }
        Feature::Rj45 => {
            c.rect(
                cx - 2.0,
                y0 + 0.5,
                cx + 2.0,
                y0 + 4.0,
                Color32::from_rgb(0xb8, 0xbe, 0xc6),
                Stroke::new(1.0, Color32::from_rgb(0x70, 0x74, 0x7c)),
            );
            c.rect(cx - 1.2, y0 + 1.2, cx + 1.2, y0 + 3.2, Color32::from_rgb(0x20, 0x20, 0x24), Stroke::NONE);
            c.text(cx, y1 - 1.6, title, 0.4, silk);
        }
        Feature::None => {
            c.text(cx, cy, title, 0.45, silk);
        }
    }
    // 开发板的运行状态
    if c.def.is_board() {
        if let Some(v) = vis {
            if !v.status.is_empty() {
                c.text(cx, y1 - 0.35, &v.status, 0.36, silk);
            }
        }
    }
    // 舵机摆臂
    if c.def.key == "servo" {
        let a = vis.map_or(90.0, |v| v.angle).to_radians();
        let (hx, hy) = (cx, y0 + 2.0);
        c.circle(hx, hy, 0.7, Color32::WHITE, Stroke::new(1.0, BODY_STROKE));
        c.line((hx, hy), (hx - 2.2 * a.cos(), hy - 2.2 * a.sin()), 0.45, Color32::WHITE);
        c.text(hx, y1 - 1.0, &format!("{:.0}°", vis.map_or(90.0, |v| v.angle)), 0.4, silk);
    }
}

/// 摄像头 / 屏幕上显示的内置示例图：渐变天空 + 太阳 + 山
fn draw_sample_image(c: &Ctx, x0: f32, y0: f32, x1: f32, y1: f32) {
    let n = 8;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let col = lerp_col(Color32::from_rgb(0x2a, 0x6c, 0xd8), Color32::from_rgb(0xf4, 0xb0, 0x60), t);
        c.rect(x0, y0 + (y1 - y0) * t, x1, y0 + (y1 - y0) * (t + 1.0 / n as f32), col, Stroke::NONE);
    }
    let w = x1 - x0;
    let h = y1 - y0;
    c.circle(x0 + w * 0.7, y0 + h * 0.35, h * 0.12, Color32::from_rgb(0xff, 0xf0, 0xa0), Stroke::NONE);
    c.poly(&[(x0, y1), (x0 + w * 0.35, y0 + h * 0.5), (x0 + w * 0.7, y1)], Color32::from_rgb(0x2a, 0x5a, 0x3a), Stroke::NONE);
    c.poly(&[(x0 + w * 0.4, y1), (x0 + w * 0.75, y0 + h * 0.6), (x1, y1)], Color32::from_rgb(0x1e, 0x46, 0x2c), Stroke::NONE);
}

/// 引脚名缩短（板子上丝印放不下长名字）
fn short_pin(n: &str) -> &str {
    n
}

fn draw_breadboard(c: &Ctx, cols: i32) {
    let [x0, y0, x1, y1] = c.def.body;
    c.rrect(x0, y0, x1, y1, 0.5, Color32::from_rgb(0xf3, 0xf1, 0xea), Stroke::new(1.0, Color32::from_rgb(0xc9, 0xc5, 0xb8)));
    // 中缝
    c.rect(x0, 8.0, x1, 9.0, Color32::from_rgb(0xe2, 0xdf, 0xd4), Stroke::NONE);
    // 电源轨标线
    let red = Color32::from_rgb(0xe0, 0x40, 0x40);
    let blue = Color32::from_rgb(0x30, 0x70, 0xd0);
    c.line((x0 + 0.6, -0.6), (x1 - 0.6, -0.6), 0.08, blue);
    c.line((x0 + 0.6, 1.6), (x1 - 0.6, 1.6), 0.08, red);
    c.line((x0 + 0.6, 15.4), (x1 - 0.6, 15.4), 0.08, red);
    c.line((x0 + 0.6, 17.6), (x1 - 0.6, 17.6), 0.08, blue);
    let hole = Color32::from_rgb(0x55, 0x55, 0x55);
    let hs = 0.17;
    let draw_holes = c.v.s >= 5.0;
    if draw_holes {
        for pd in &c.def.pins {
            let (x, y) = (pd.at.0 as f32, pd.at.1 as f32);
            let p = c.l(x, y);
            let r = c.d(hs);
            c.p.rect_filled(egui::Rect::from_center_size(p, vec2(r * 2.0, r * 2.0)), 1.0, hole);
        }
    }
    // 行字母、列号
    let txt = Color32::from_rgb(0x90, 0x8c, 0x80);
    for (i, y) in lib::BB_ROWS.iter().enumerate() {
        c.text(-0.9, *y as f32, &lib::BB_ROW_NAMES[i].to_string(), 0.4, txt);
        c.text(cols as f32 - 0.1, *y as f32, &lib::BB_ROW_NAMES[i].to_string(), 0.4, txt);
    }
    for x in (0..cols).filter(|x| (x + 1) % 5 == 0 || *x == 0) {
        c.text(x as f32, 2.2, &(x + 1).to_string(), 0.36, txt);
        c.text(x as f32, 14.8, &(x + 1).to_string(), 0.36, txt);
    }
    c.text(x0 + 0.5, 0.0, "−", 0.5, blue);
    c.text(x0 + 0.5, 1.0, "+", 0.5, red);
    c.text(x0 + 0.5, 16.0, "+", 0.5, red);
    c.text(x0 + 0.5, 17.0, "−", 0.5, blue);
    let _ = y1;
}

// ───────────────────────── 原理图符号 ─────────────────────────

/// 原理图视图：符号画在元件范围中间，用短线连到各引脚
pub fn draw_symbol(c: &Ctx) {
    let ink = TEXT();
    let w = 0.12;
    let k = c.def.key;
    let pins: Vec<(f32, f32)> = c
        .def
        .pins
        .iter()
        .enumerate()
        .map(|(i, pd)| {
            let (x, y) = c.part.local_pin(c.def, i, pd.at);
            (x as f32, y as f32)
        })
        .collect();
    let burnt = c.burnt;
    let col = if burnt { ERR() } else { ink };
    match &c.def.look {
        Look::Breadboard { .. } => {}
        Look::Resistor | Look::Inductor | Look::Fuse | Look::Bead | Look::Diode | Look::Zener => {
            let (a, b) = (pins[0], pins[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt().max(0.01);
            let (ux, uy) = (dx / len, dy / len);
            let (nx, ny) = (-uy, ux);
            let m = (a.0 + dx / 2.0, a.1 + dy / 2.0);
            let pt = |t: f32, n: f32| (m.0 + ux * t + nx * n, m.1 + uy * t + ny * n);
            let half = (len / 2.0 - 0.3).min(1.0).max(0.4);
            c.line(a, pt(-half, 0.0), w, col);
            c.line(pt(half, 0.0), b, w, col);
            match c.def.look {
                Look::Resistor => c.path(&[pt(-half, -0.35), pt(half, -0.35), pt(half, 0.35), pt(-half, 0.35), pt(-half, -0.35)], w, col),
                Look::Fuse => {
                    c.path(&[pt(-half, -0.25), pt(half, -0.25), pt(half, 0.25), pt(-half, 0.25), pt(-half, -0.25)], w, col);
                    c.line(pt(-half, 0.0), pt(half, 0.0), w, col);
                }
                Look::Bead => c.path(&[pt(-half, -0.3), pt(half, -0.3), pt(half, 0.3), pt(-half, 0.3), pt(-half, -0.3)], w * 2.0, col),
                Look::Inductor => {
                    let n = 4;
                    let mut v = vec![];
                    for i in 0..=n * 8 {
                        let t = -half + 2.0 * half * i as f32 / (n * 8) as f32;
                        let ph = (i % 8) as f32 / 8.0 * std::f32::consts::PI;
                        v.push(pt(t, -0.35 * ph.sin()));
                    }
                    c.path(&v, w, col);
                }
                _ => {
                    // 二极管：三角 + 竖线
                    let s = 0.45;
                    let tri = [pt(-s, -s), pt(-s, s), pt(s, 0.0)];
                    c.poly(&tri, Color32::TRANSPARENT, Stroke::new(c.d(w).max(1.0), col));
                    c.line(pt(-half, 0.0), pt(-s, 0.0), w, col);
                    c.line(pt(s, 0.0), pt(half, 0.0), w, col);
                    c.line(pt(s, -s), pt(s, s), w, col);
                    if c.def.look == Look::Zener {
                        c.line(pt(s, s), pt(s + 0.2, s + 0.15), w, col);
                        c.line(pt(s, -s), pt(s - 0.2, -s - 0.15), w, col);
                    }
                }
            }
        }
        Look::Cap | Look::ECap => {
            let (a, b) = (pins[0], pins[1]);
            let mid = ((a.0 + b.0) / 2.0, a.1 - 1.2);
            c.line(a, (a.0, mid.1), w, col);
            c.line(b, (b.0, mid.1), w, col);
            c.line((a.0, mid.1), (mid.0 - 0.15, mid.1), w, col);
            c.line((b.0, mid.1), (mid.0 + 0.15, mid.1), w, col);
            c.line((mid.0 - 0.15, mid.1 - 0.6), (mid.0 - 0.15, mid.1 + 0.6), w * 1.6, col);
            if c.def.look == Look::ECap {
                c.path(&[(mid.0 + 0.35, mid.1 - 0.6), (mid.0 + 0.15, mid.1), (mid.0 + 0.35, mid.1 + 0.6)], w * 1.6, col);
                c.text(mid.0 - 0.6, mid.1 - 0.6, "+", 0.5, col);
            } else {
                c.line((mid.0 + 0.15, mid.1 - 0.6), (mid.0 + 0.15, mid.1 + 0.6), w * 1.6, col);
            }
        }
        Look::Led => {
            let (a, b) = (pins[0], pins[1]);
            let y = a.1 - 1.4;
            c.line(a, (a.0, y), w, col);
            c.line(b, (b.0, y), w, col);
            let (x0, x1) = (a.0 - 0.1, b.0 + 0.1);
            let m = (x0 + x1) / 2.0;
            c.poly(&[(m - 0.4, y - 0.4), (m - 0.4, y + 0.4), (m + 0.3, y)], Color32::TRANSPARENT, Stroke::new(c.d(w).max(1.0), col));
            c.line((m + 0.3, y - 0.4), (m + 0.3, y + 0.4), w, col);
            c.line((a.0, y), (m - 0.4, y), w, col);
            c.line((m + 0.3, y), (b.0, y), w, col);
            c.line((m, y - 0.6), (m + 0.4, y - 1.0), w * 0.8, col);
            c.line((m + 0.3, y - 0.5), (m + 0.7, y - 0.9), w * 0.8, col);
            let (_, rgbc) = crate::circuit::models::led(c.part.prop("color"));
            c.glow(m, y, 0.3, rgb(rgbc), c.vis.map_or(0.0, |v| v.glow));
        }
        Look::Ground => {
            c.line((0.0, 0.0), (0.0, 0.6), w, ink);
            for (i, ww) in [0.7, 0.45, 0.2].iter().enumerate() {
                let y = 0.6 + i as f32 * 0.3;
                c.line((-ww, y), (*ww, y), w, ink);
            }
        }
        Look::Battery | Look::UsbPower | Look::Psu => {
            let (p, n) = (pins[0], pins[1]);
            let x = p.0 - 1.5;
            let my = (p.1 + n.1) / 2.0;
            c.path(&[p, (x, p.1), (x, my - 0.3)], w, col);
            c.path(&[n, (x, n.1), (x, my + 0.3)], w, col);
            c.line((x - 0.7, my - 0.3), (x + 0.7, my - 0.3), w, col);
            c.line((x - 0.35, my + 0.3), (x + 0.35, my + 0.3), w * 2.0, col);
            c.text(x - 1.0, my - 0.8, "+", 0.5, col);
            let v = match k {
                "battery" => format!("{}V", crate::circuit::models::battery(c.part.prop("type")).vnom),
                "usb5v" => "5V".into(),
                _ => format!("{}V", c.part.num("v")),
            };
            c.text(x - 1.6, my + 0.2, &v, 0.42, TEXT2());
        }
        Look::SigGen => {
            let (p, n) = (pins[0], pins[1]);
            let x = p.0 - 1.6;
            let my = (p.1 + n.1) / 2.0;
            c.path(&[p, (x, p.1), (x, my - 0.8)], w, col);
            c.path(&[n, (x, n.1), (x, my + 0.8)], w, col);
            c.p.circle_stroke(c.l(x, my), c.d(0.8), Stroke::new(c.d(w).max(1.0), col));
            if k == "acsrc" {
                c.text(x, my, "~", 0.8, col);
            } else {
                c.path(
                    &[
                        (x - 0.45, my + 0.25),
                        (x - 0.45, my - 0.25),
                        (x, my - 0.25),
                        (x, my + 0.25),
                        (x + 0.45, my + 0.25),
                        (x + 0.45, my - 0.25),
                    ],
                    w * 0.8,
                    col,
                );
            }
        }
        Look::To92 | Look::To220 if k == "npn" || k == "pnp" => {
            // E B C → 符号
            let (e, b, cc) = (pins[0], pins[1], pins[2]);
            let (cx, cy) = (b.0, b.1 - 2.0);
            c.p.circle_stroke(c.l(cx, cy), c.d(0.95), Stroke::new(c.d(w).max(1.0), col));
            c.path(&[b, (b.0, cy + 0.9), (cx - 0.4, cy)], w, col);
            c.line((cx - 0.4, cy - 0.5), (cx - 0.4, cy + 0.5), w * 1.6, col);
            c.path(&[(cx - 0.4, cy - 0.2), (cx + 0.35, cy - 0.65), (cc.0, cy - 0.65), cc], w, col);
            c.path(&[(cx - 0.4, cy + 0.2), (cx + 0.35, cy + 0.65)], w, col);
            c.path(&[(cx + 0.35, cy + 0.65), (e.0, cy + 0.65), e], w, col);
            // 箭头
            if k == "npn" {
                c.poly(&[(cx + 0.35, cy + 0.65), (cx + 0.05, cy + 0.6), (cx + 0.25, cy + 0.4)], col, Stroke::NONE);
            } else {
                c.poly(&[(cx - 0.35, cy + 0.15), (cx - 0.05, cy + 0.15), (cx - 0.15, cy + 0.4)], col, Stroke::NONE);
            }
            c.text(cx + 1.5, cy, c.part.prop("model"), 0.35, TEXT2());
        }
        Look::To220 if k == "nmos" || k == "pmos" => {
            let (g, d, s) = (pins[0], pins[1], pins[2]);
            let (cx, cy) = (d.0, d.1 - 2.2);
            c.p.circle_stroke(c.l(cx, cy), c.d(1.0), Stroke::new(c.d(w).max(1.0), col));
            c.path(&[g, (g.0, cy), (cx - 0.5, cy)], w, col);
            c.line((cx - 0.5, cy - 0.5), (cx - 0.5, cy + 0.5), w, col);
            for dy in [-0.5, 0.0, 0.5] {
                c.line((cx - 0.3, cy + dy - 0.15), (cx - 0.3, cy + dy + 0.15), w * 1.4, col);
            }
            c.path(&[(cx - 0.3, cy - 0.5), (cx + 0.4, cy - 0.5), (cx + 0.4, cy - 1.2), (d.0 + 0.0, cy - 1.2), d], w, col);
            c.path(&[(cx - 0.3, cy + 0.5), (cx + 0.4, cy + 0.5), (cx + 0.4, cy + 1.1), (s.0, cy + 1.1), s], w, col);
            c.line((cx - 0.3, cy), (cx + 0.4, cy), w, col);
            c.line((cx + 0.4, cy), (cx + 0.4, cy + 0.5), w, col);
            if k == "nmos" {
                c.poly(&[(cx - 0.25, cy), (cx + 0.05, cy - 0.15), (cx + 0.05, cy + 0.15)], col, Stroke::NONE);
            } else {
                c.poly(&[(cx + 0.35, cy), (cx + 0.05, cy - 0.15), (cx + 0.05, cy + 0.15)], col, Stroke::NONE);
            }
            c.text(cx + 1.6, cy, c.part.prop("model"), 0.35, TEXT2());
        }
        Look::Button | Look::Slide => {
            let (a, b) = if k == "button" { (pins[0], pins[2]) } else { (pins[1], pins[0]) };
            let y = a.1.min(b.1) - 1.0;
            c.line(a, (a.0, y), w, col);
            c.line(b, (b.0, y), w, col);
            let on = c.vis.is_some_and(|v| v.on) || (k == "slide" && c.part.prop("on") != "false");
            c.circle(a.0, y, 0.12, col, Stroke::NONE);
            c.circle(b.0, y, 0.12, col, Stroke::NONE);
            if on {
                c.line((a.0, y), (b.0, y), w, col);
            } else {
                c.line((a.0, y), (b.0 - 0.1, y - 0.7), w, col);
            }
            if k == "button" {
                c.line(((a.0 + b.0) / 2.0, y - 0.5), ((a.0 + b.0) / 2.0, y - 1.0), w, col);
            }
        }
        Look::Lamp | Look::Motor | Look::Buzzer => {
            let (a, b) = (pins[0], pins[1]);
            let (cx, cy) = ((a.0 + b.0) / 2.0, a.1 - 1.8);
            c.path(&[a, (a.0, cy), (cx - 0.9, cy)], w, col);
            c.path(&[b, (b.0, cy), (cx + 0.9, cy)], w, col);
            c.p.circle_stroke(c.l(cx, cy), c.d(0.9), Stroke::new(c.d(w).max(1.0), col));
            match c.def.look {
                Look::Lamp => {
                    c.line((cx - 0.6, cy - 0.6), (cx + 0.6, cy + 0.6), w, col);
                    c.line((cx - 0.6, cy + 0.6), (cx + 0.6, cy - 0.6), w, col);
                    c.glow(cx, cy, 0.5, Color32::from_rgb(0xff, 0xe0, 0x80), c.vis.map_or(0.0, |v| v.glow));
                }
                Look::Motor => c.text(cx, cy, "M", 0.8, col),
                _ => c.text(cx, cy, "BZ", 0.5, col),
            }
        }
        _ => {
            // 通用：方框 + 引脚名
            let [x0, y0, x1, y1] = c.def.body;
            let fill = if c.def.is_board() { BG3() } else { BG2() };
            c.rect(x0 + 0.2, y0 + 0.2, x1 - 0.2, y1 - 0.2, fill, Stroke::new(c.d(w).max(1.0), col));
            let name = crate::i18n::t(c.def.zh);
            c.text((x0 + x1) / 2.0, (y0 + y1) / 2.0, name, 0.5, col);
            for (i, (x, y)) in pins.iter().enumerate() {
                let n = &c.def.pins[i].name;
                if c.v.s >= 9.0 {
                    let up = *y <= y0 + 1.0;
                    c.p.text(
                        c.l(*x, *y + if up { 0.7 } else { -0.7 }),
                        Align2::CENTER_CENTER,
                        n,
                        FontId::new(c.v.s * 0.3, FontFamily::Proportional),
                        TEXT2(),
                    );
                }
            }
            if let Some(v) = c.vis {
                if !v.lines.is_empty() {
                    c.mono(x0 + 0.6, y1 - 1.2, &v.lines.join(" / "), 0.4, ACCENT(), Align2::LEFT_CENTER);
                }
            }
        }
    }
    // 引脚小圆点
    for (x, y) in &pins {
        if !matches!(c.def.look, Look::Breadboard { .. }) {
            c.circle(*x, *y, 0.1, col, Stroke::NONE);
        }
    }
    if c.v.s >= 8.0 && !matches!(c.def.look, Look::Breadboard { .. } | Look::Ground) {
        let [x0, y0, _, _] = c.def.body;
        let p = c.l(x0, y0 - 0.3);
        c.p.text(p, Align2::LEFT_BOTTOM, &c.label, FontId::new(c.v.s * 0.38, FontFamily::Proportional), ACCENT());
    }
}

/// 元件的值（显示在原理图标签后面、库列表里）
pub fn value_text(p: &Part) -> String {
    match p.kind.as_str() {
        "res" | "pot" => crate::circuit::fmt_si(p.num("r"), "Ω").replace(' ', ""),
        "cap" | "ecap" => crate::circuit::fmt_si(p.num("c"), "F").replace(' ', ""),
        "ind" => crate::circuit::fmt_si(p.num("l"), "H").replace(' ', ""),
        "led" => crate::i18n::t(color_zh(p.prop("color"))).to_string(),
        "npn" | "pnp" | "nmos" | "pmos" | "diode" | "zener" | "ldo" => p.prop("model").to_string(),
        "battery" => format!("{}V", crate::circuit::models::battery(p.prop("type")).vnom),
        "psu" => format!("{}V", p.num("v")),
        _ => String::new(),
    }
}

pub fn color_zh(c: &str) -> &'static str {
    match c {
        "yellow" => "黄",
        "green" => "绿",
        "blue" => "蓝",
        "white" => "白",
        _ => "红",
    }
}

/// 只要外框不要细节（缩得很小时）
pub fn draw_outline(c: &Ctx) {
    let [x0, y0, x1, y1] = c.def.body;
    c.rect(x0, y0, x1, y1, BG3(), Stroke::new(1.0, LINE_HI()));
    let _ = vec2(0.0, 0.0);
}

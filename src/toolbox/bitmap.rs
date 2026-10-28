//! 取模：文字（任意 TTF / TTC 字体）和图片转成单色点阵或 RGB565，再按 LCD / OLED 驱动要的方式排成 C 数组。

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};

/// 单色点阵
#[derive(Clone, Default)]
pub struct Mono {
    pub w: usize,
    pub h: usize,
    pub px: Vec<bool>,
}

impl Mono {
    pub fn new(w: usize, h: usize) -> Self {
        Mono { w, h, px: vec![false; w * h] }
    }
    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.w && y < self.h && self.px[y * self.w + x]
    }
}

/// 取模方式
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Scan {
    /// 逐行式：每行从左到右，8 个横向像素一个字节
    #[default]
    Rows,
    /// 逐列式：每列从上到下，8 个纵向像素一个字节
    Cols,
    /// 行列式：每 8 行一页，页内从左到右每列一个字节（SSD1306 等 OLED）
    PageCols,
    /// 列行式：每列从上到下，每 8 行一个字节，再到下一列
    ColPages,
}

impl Scan {
    pub const ALL: [Scan; 4] = [Scan::Rows, Scan::Cols, Scan::PageCols, Scan::ColPages];
}

/// 按取模方式把点阵编码成字节
pub fn encode(m: &Mono, scan: Scan, msb_first: bool, invert: bool) -> Vec<u8> {
    let mut out = vec![];
    let mut push8 = |bits: &mut dyn Iterator<Item = bool>| {
        let mut b = 0u8;
        for (i, on) in bits.enumerate() {
            if on != invert {
                b |= if msb_first { 0x80 >> i } else { 1 << i };
            }
        }
        out.push(b);
    };
    let pages = m.h.div_ceil(8);
    let bytes_w = m.w.div_ceil(8);
    match scan {
        Scan::Rows => {
            for y in 0..m.h {
                for bx in 0..bytes_w {
                    push8(&mut (0..8).map(|i| m.get(bx * 8 + i, y)));
                }
            }
        }
        Scan::Cols => {
            for x in 0..m.w {
                for by in 0..pages {
                    push8(&mut (0..8).map(|i| m.get(x, by * 8 + i)));
                }
            }
        }
        Scan::PageCols => {
            for p in 0..pages {
                for x in 0..m.w {
                    push8(&mut (0..8).map(|i| m.get(x, p * 8 + i)));
                }
            }
        }
        Scan::ColPages => {
            for x in 0..m.w {
                for p in 0..pages {
                    push8(&mut (0..8).map(|i| m.get(x, p * 8 + i)));
                }
            }
        }
    }
    out
}

/// 把一个字符画进 w×h 的格子；字号 size 像素，(dx, dy) 微调位置，阈值 0..255
pub fn glyph(font: &FontRef, c: char, size: f32, w: usize, h: usize, dx: i32, dy: i32, threshold: u8) -> Mono {
    let mut m = Mono::new(w, h);
    let sf = font.as_scaled(PxScale::from(size));
    let id = font.glyph_id(c);
    let adv = sf.h_advance(id);
    // 水平居中，基线放在 (格子高 - 字高) / 2 + ascent
    let line_h = sf.ascent() - sf.descent();
    let x0 = ((w as f32 - adv) / 2.0).round() + dx as f32;
    let y0 = ((h as f32 - line_h) / 2.0 + sf.ascent()).round() + dy as f32;
    let g = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x0, y0));
    if let Some(o) = font.outline_glyph(g) {
        let b = o.px_bounds();
        o.draw(|x, y, cov| {
            let px = b.min.x as i32 + x as i32;
            let py = b.min.y as i32 + y as i32;
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h && cov * 255.0 >= threshold as f32 {
                m.px[py as usize * w + px as usize] = true;
            }
        });
    }
    m
}

/// 图片：缩放到 w×h，灰度后按阈值（或抖动）变成点阵
pub fn image_mono(img: &image::RgbaImage, threshold: u8, dither: bool, invert_src: bool) -> Mono {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut gray: Vec<f32> = img
        .pixels()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            // 透明当白底
            let l = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) * a + 255.0 * (1.0 - a);
            if invert_src {
                255.0 - l
            } else {
                l
            }
        })
        .collect();
    let mut m = Mono::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let old = gray[i];
            // 暗的像素算「亮点」（黑字白底的图取出来是字）
            let on = old < threshold as f32;
            m.px[i] = on;
            if dither {
                let err = old - if on { 0.0 } else { 255.0 };
                let mut add = |xx: isize, yy: usize, f: f32| {
                    if xx >= 0 && (xx as usize) < w && yy < h {
                        gray[yy * w + xx as usize] += err * f;
                    }
                };
                add(x as isize + 1, y, 7.0 / 16.0);
                add(x as isize - 1, y + 1, 3.0 / 16.0);
                add(x as isize, y + 1, 5.0 / 16.0);
                add(x as isize + 1, y + 1, 1.0 / 16.0);
            }
        }
    }
    m
}

pub fn rgb565(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 & 0xF8) << 8) | ((g as u16 & 0xFC) << 3) | (b as u16 >> 3)
}

pub fn from565(v: u16) -> (u8, u8, u8) {
    let r = ((v >> 11) & 0x1F) as u8;
    let g = ((v >> 5) & 0x3F) as u8;
    let b = (v & 0x1F) as u8;
    ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
}

/// 彩色输出格式
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum ColorFmt {
    #[default]
    Rgb565,
    /// RGB565 按字节输出，高字节在前（SPI 屏直接发）
    Rgb565Be,
    Rgb888,
    Gray8,
}

pub fn image_color(img: &image::RgbaImage, fmt: ColorFmt) -> (Vec<u8>, Vec<u16>) {
    let mut bytes = vec![];
    let mut words = vec![];
    for p in img.pixels() {
        let a = p[3] as u16;
        // 透明当白底
        let mix = |c: u8| ((c as u16 * a + 255 * (255 - a)) / 255) as u8;
        let (r, g, b) = (mix(p[0]), mix(p[1]), mix(p[2]));
        match fmt {
            ColorFmt::Rgb565 => words.push(rgb565(r, g, b)),
            ColorFmt::Rgb565Be => bytes.extend(rgb565(r, g, b).to_be_bytes()),
            ColorFmt::Rgb888 => bytes.extend([r, g, b]),
            ColorFmt::Gray8 => bytes.push((0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8),
        }
    }
    (bytes, words)
}

/// 排成 C 源码：每行 per_line 个，十六进制
pub fn c_bytes(out: &mut String, data: &[u8], per_line: usize) {
    for chunk in data.chunks(per_line.max(1)) {
        out.push_str("    ");
        for b in chunk {
            out.push_str(&format!("0x{b:02X},"));
        }
        out.push('\n');
    }
}

pub fn c_words(out: &mut String, data: &[u16], per_line: usize) {
    for chunk in data.chunks(per_line.max(1)) {
        out.push_str("    ");
        for b in chunk {
            out.push_str(&format!("0x{b:04X},"));
        }
        out.push('\n');
    }
}

/// 合法的 C 标识符
pub fn c_ident(s: &str) -> String {
    let mut v: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if v.is_empty() || v.starts_with(|c: char| c.is_ascii_digit()) {
        v.insert(0, '_');
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Mono {
        // 10×9：左上角一个点、第 8 行最右一个点
        let mut m = Mono::new(10, 9);
        m.px[0] = true;
        m.px[8 * 10 + 9] = true;
        m
    }

    #[test]
    fn scans() {
        let m = sample();
        let r = encode(&m, Scan::Rows, true, false);
        assert_eq!(r.len(), 2 * 9);
        assert_eq!(r[0], 0x80);
        assert_eq!(r[17], 0x40);
        let p = encode(&m, Scan::PageCols, false, false);
        assert_eq!(p.len(), 10 * 2);
        assert_eq!(p[0], 0x01);
        assert_eq!(p[10 + 9], 0x01);
        let c = encode(&m, Scan::ColPages, false, false);
        assert_eq!((c[0], c[19]), (0x01, 0x01));
        assert_eq!(encode(&m, Scan::Rows, true, true)[0], 0x7F);
    }

    #[test]
    fn colors() {
        assert_eq!(rgb565(255, 0, 0), 0xF800);
        assert_eq!(rgb565(255, 255, 255), 0xFFFF);
        assert_eq!(from565(0x07E0), (0, 255, 0));
        assert_eq!(c_ident("1 logo.png"), "_1_logo_png");
    }

    #[test]
    fn glyph_from_builtin_font() {
        let f = FontRef::try_from_slice(include_bytes!("../../assets/JetBrainsMono-Regular.ttf")).unwrap();
        let m = glyph(&f, 'H', 16.0, 8, 16, 0, 0, 128);
        assert!(m.px.iter().filter(|p| **p).count() > 10);
        // 'H' 左右两竖：同一行左右两半都有点，中间有空
        assert!((0..16).any(|y| (0..3).any(|x| m.get(x, y)) && (5..8).any(|x| m.get(x, y)) && !m.get(4, y)));
    }
}

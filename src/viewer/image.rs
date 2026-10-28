//! 图片查看（v1.8）：PNG、JPG、BMP、GIF、ICO、WebP、TGA。缩放、像素网格、取色（含 RGB565）、转 C 数组；
//! GIF 自动播放、逐帧、导出单帧、全部帧转 C 数组；和另一张图并排 / 叠加 / 差异对比。
//! 原始像素（.bin 或粘贴的 HEX 按 RGB565 / RGB888 / 灰度 / 单色显示）也在这里。

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Color32, ColorImage, FontId, Rect, RichText, Sense, TextureHandle, TextureOptions, Ui};

use crate::i18n::{t, tf};
use crate::theme::*;

pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "ico", "webp", "tga"];

pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// 一帧：像素和显示时长（毫秒）
pub struct Frame {
    pub img: ColorImage,
    pub delay: u32,
}

/// 解码图片；GIF 拆成全部帧
pub fn decode(bytes: &[u8]) -> Result<Vec<Frame>, String> {
    use image::AnimationDecoder;
    let fmt = image::guess_format(bytes).map_err(|e| e.to_string())?;
    if fmt == image::ImageFormat::Gif {
        let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let mut out = vec![];
        for f in dec.into_frames().take(2000) {
            let f = f.map_err(|e| e.to_string())?;
            let (n, d) = f.delay().numer_denom_ms();
            let delay = if d == 0 { 100 } else { (n / d).max(20) };
            let buf = f.into_buffer();
            let size = [buf.width() as usize, buf.height() as usize];
            out.push(Frame {
                img: ColorImage::from_rgba_unmultiplied(size, buf.as_raw()),
                delay,
            });
        }
        if out.is_empty() {
            return Err(t("GIF 里没有帧").to_string());
        }
        return Ok(out);
    }
    let img = image::load_from_memory_with_format(bytes, fmt).map_err(|e| e.to_string())?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Ok(vec![Frame {
        img: ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
        delay: 0,
    }])
}

fn rgb565(c: Color32) -> u16 {
    ((c.r() as u16 >> 3) << 11) | ((c.g() as u16 >> 2) << 5) | (c.b() as u16 >> 3)
}

/// C 数组的像素格式
#[derive(Clone, Copy, PartialEq)]
pub enum CFmt {
    Rgb565,
    Rgb888,
    Argb8888,
    Mono,
}

impl CFmt {
    const ALL: [CFmt; 4] = [CFmt::Rgb565, CFmt::Rgb888, CFmt::Argb8888, CFmt::Mono];
    fn label(self) -> &'static str {
        match self {
            CFmt::Rgb565 => "RGB565",
            CFmt::Rgb888 => "RGB888",
            CFmt::Argb8888 => "ARGB8888",
            CFmt::Mono => t("单色 1 位"),
        }
    }
}

/// 一帧转成 C 数组的内容（不含声明）
fn c_body(img: &ColorImage, fmt: CFmt) -> (String, usize, &'static str) {
    let [w, h] = img.size;
    let mut items: Vec<String> = vec![];
    let ty = match fmt {
        CFmt::Rgb565 => {
            items.extend(img.pixels.iter().map(|c| format!("0x{:04X}", rgb565(*c))));
            "uint16_t"
        }
        CFmt::Rgb888 => {
            for c in &img.pixels {
                items.extend([c.r(), c.g(), c.b()].iter().map(|v| format!("0x{v:02X}")));
            }
            "uint8_t"
        }
        CFmt::Argb8888 => {
            items.extend(img.pixels.iter().map(|c| format!("0x{:02X}{:02X}{:02X}{:02X}", c.a(), c.r(), c.g(), c.b())));
            "uint32_t"
        }
        CFmt::Mono => {
            // 横向取模，高位在前，每行补齐到整字节
            for y in 0..h {
                for bx in 0..w.div_ceil(8) {
                    let mut b = 0u8;
                    for k in 0..8 {
                        let x = bx * 8 + k;
                        if x < w {
                            let c = img.pixels[y * w + x];
                            let lum = (c.r() as u32 * 30 + c.g() as u32 * 59 + c.b() as u32 * 11) / 100;
                            if lum < 128 && c.a() > 127 {
                                b |= 0x80 >> k;
                            }
                        }
                    }
                    items.push(format!("0x{b:02X}"));
                }
            }
            "uint8_t"
        }
    };
    let n = items.len();
    let per = if ty == "uint32_t" {
        8
    } else if ty == "uint16_t" {
        12
    } else {
        16
    };
    let body = items.chunks(per).map(|c| format!("    {},", c.join(", "))).collect::<Vec<_>>().join("\n");
    (body, n, ty)
}

pub fn c_array(name: &str, frames: &[&ColorImage], fmt: CFmt) -> String {
    let Some(first) = frames.first() else { return String::new() };
    let [w, h] = first.size;
    let mut s = format!(
        "// {name}: {w}x{h} {} ({})\n",
        fmt.label(),
        if frames.len() > 1 { tf("{} 帧", &[&frames.len()]) } else { t("1 帧").to_string() }
    );
    if frames.len() == 1 {
        let (body, n, ty) = c_body(first, fmt);
        s += &format!("const {ty} {name}[{n}] = {{\n{body}\n}};\n");
    } else {
        let (_, n, ty) = c_body(first, fmt);
        s += &format!("const {ty} {name}[{}][{n}] = {{\n", frames.len());
        for (i, f) in frames.iter().enumerate() {
            let (body, _, _) = c_body(f, fmt);
            s += &format!("  {{ // {}\n{body}\n  }},\n", tf("第 {} 帧", &[&(i + 1)]));
        }
        s += "};\n";
    }
    s
}

/// 变量名：文件名里的字母数字
fn ident(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    let mut s: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// 缩放和平移；返回图片在屏幕上的矩形
#[derive(Default)]
struct ViewXf {
    /// 0 = 适应窗口
    zoom: f32,
    pan: egui::Vec2,
}

impl ViewXf {
    fn place(&mut self, ui: &Ui, area: Rect, size: [usize; 2], resp: &egui::Response) -> Rect {
        let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
        let fit = ((area.width() - 20.0) / w).min((area.height() - 20.0) / h).min(8.0).max(0.01);
        let mut z = if self.zoom <= 0.0 { fit } else { self.zoom };
        if resp.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                let old = z;
                z = (z * if scroll > 0.0 { 1.25 } else { 0.8 }).clamp(0.05, 64.0);
                // 以指针为中心缩放
                if let Some(p) = resp.hover_pos() {
                    let c = area.center() + self.pan;
                    self.pan += (p - c) * (1.0 - z / old);
                }
                self.zoom = z;
            }
        }
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            self.zoom = 0.0;
            self.pan = egui::Vec2::ZERO;
            z = fit;
        }
        Rect::from_center_size(area.center() + self.pan, vec2(w * z, h * z))
    }
}

/// 棋盘格背景（透明像素看得出来）
fn checker(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, 0.0, Color32::from_gray(if is_light() { 235 } else { 40 }));
    let s = 8.0;
    let c = Color32::from_gray(if is_light() { 215 } else { 52 });
    let clip = p.clip_rect().intersect(r);
    let (x0, y0) = (((clip.left() - r.left()) / s).floor().max(0.0) as i32, ((clip.top() - r.top()) / s).floor().max(0.0) as i32);
    let (x1, y1) = (((clip.right() - r.left()) / s).ceil() as i32, ((clip.bottom() - r.top()) / s).ceil() as i32);
    if (x1 - x0) * (y1 - y0) > 40_000 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            if (x + y) % 2 == 0 {
                let a = r.min + vec2(x as f32 * s, y as f32 * s);
                p.rect_filled(Rect::from_min_size(a, vec2(s, s)).intersect(r), 0.0, c);
            }
        }
    }
}

pub struct ImageView {
    path: PathBuf,
    bytes: usize,
    frames: Vec<Frame>,
    tex: Vec<Option<TextureHandle>>,
    cur: usize,
    playing: bool,
    next_at: f64,
    xf: ViewXf,
    grid: bool,
    cfmt: CFmt,
    /// 对比：(另一张, 模式 0 并排 1 叠加 2 差异, 叠加透明度)
    cmp: Option<(PathBuf, ColorImage, Option<TextureHandle>, u8, f32)>,
    diff_tex: Option<(TextureHandle, usize)>,
    err: Option<String>,
}

pub enum ImgEvt {
    Toast(String, Color32),
}

impl ImageView {
    pub fn new(path: &Path, bytes: &[u8]) -> ImageView {
        let (frames, err) = match decode(bytes) {
            Ok(f) => (f, None),
            Err(e) => (vec![], Some(e)),
        };
        ImageView {
            path: path.to_path_buf(),
            bytes: bytes.len(),
            tex: frames.iter().map(|_| None).collect(),
            playing: frames.len() > 1,
            frames,
            cur: 0,
            next_at: 0.0,
            xf: ViewXf::default(),
            grid: true,
            cfmt: CFmt::Rgb565,
            cmp: None,
            diff_tex: None,
            err,
        }
    }

    fn texture(&mut self, ctx: &egui::Context, i: usize) -> Option<TextureHandle> {
        let f = self.frames.get(i)?;
        if self.tex[i].is_none() {
            // 放大看像素时用最近邻，不要糊
            self.tex[i] = Some(ctx.load_texture(format!("img-{}-{i}", self.path.display()), f.img.clone(), TextureOptions::NEAREST));
        }
        self.tex[i].clone()
    }

    /// 和另一张图对比（菜单选或者拖进来）
    pub fn compare_with(&mut self, p: PathBuf) -> Result<(), String> {
        let mut f = std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| decode(&b))?;
        self.cmp = Some((p, f.swap_remove(0).img, None, 0, 0.5));
        self.diff_tex = None;
        Ok(())
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<ImgEvt> {
        let mut evt = None;
        if let Some(e) = &self.err {
            ui.add_space(20.0);
            ui.label(RichText::new(tf("图片打不开：{}", &[e])).color(ERR()));
            return None;
        }
        let n = self.frames.len();
        let size = self.frames[0].img.size;
        let now = ui.input(|i| i.time);
        // 播放 GIF
        if n > 1 && self.playing {
            if now >= self.next_at {
                if self.next_at > 0.0 {
                    self.cur = (self.cur + 1) % n;
                }
                self.next_at = now + self.frames[self.cur].delay as f64 / 1000.0;
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((self.next_at - now).max(0.01)));
        }
        // ---- 工具栏
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} × {}", size[0], size[1])).font(FontId::monospace(12.0)).color(TEXT()));
            ui.label(RichText::new(crate::app::human_bytes(self.bytes as u64)).small().color(TEXT3()));
            if n > 1 {
                ui.separator();
                use crate::app::{icon_button, Icon};
                if icon_button(ui, if self.playing { Icon::Pause } else { Icon::Play }, if self.playing { t("暂停") } else { t("播放") }, false).clicked() {
                    self.playing = !self.playing;
                    self.next_at = 0.0;
                }
                if icon_button(ui, Icon::Back, t("上一帧"), false).clicked() {
                    self.playing = false;
                    self.cur = (self.cur + n - 1) % n;
                }
                if icon_button(ui, Icon::Forward, t("下一帧"), false).clicked() {
                    self.playing = false;
                    self.cur = (self.cur + 1) % n;
                }
                ui.label(RichText::new(tf("第 {} / {} 帧 · {} ms", &[&(self.cur + 1), &n, &self.frames[self.cur].delay])).small().color(TEXT2()));
            }
            ui.separator();
            let z = if self.xf.zoom <= 0.0 { t("适应").to_string() } else { format!("{:.0}%", self.xf.zoom * 100.0) };
            if ui.small_button(z).on_hover_text(t("滚轮缩放，拖动平移，双击适应窗口；点这里切换 100%")).clicked() {
                self.xf.zoom = if self.xf.zoom == 1.0 { 0.0 } else { 1.0 };
                self.xf.pan = egui::Vec2::ZERO;
            }
            ui.checkbox(&mut self.grid, t("像素网格"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(t("更多"), |ui| {
                    ui.label(RichText::new(t("转 C 数组的格式")).small().color(TEXT3()));
                    for f in CFmt::ALL {
                        ui.radio_value(&mut self.cfmt, f, f.label());
                    }
                    ui.separator();
                    if ui.button(t("复制当前帧为 C 数组")).clicked() {
                        let s = c_array(&ident(&self.path), &[&self.frames[self.cur].img], self.cfmt);
                        ui.ctx().copy_text(s);
                        evt = Some(ImgEvt::Toast(t("已复制 C 数组").into(), OK()));
                        ui.close_menu();
                    }
                    if n > 1 && ui.button(tf("复制全部 {} 帧为 C 数组（LCD 动画）", &[&n])).clicked() {
                        let all: Vec<&ColorImage> = self.frames.iter().map(|f| &f.img).collect();
                        let s = c_array(&ident(&self.path), &all, self.cfmt);
                        ui.ctx().copy_text(s);
                        evt = Some(ImgEvt::Toast(t("已复制 C 数组").into(), OK()));
                        ui.close_menu();
                    }
                    if ui.button(t("导出当前帧为 PNG…")).clicked() {
                        ui.close_menu();
                        let name = format!("{}-{}.png", ident(&self.path), self.cur + 1);
                        if let Some(p) = rfd::FileDialog::new().set_file_name(&name).add_filter("PNG", &["png"]).save_file() {
                            let f = &self.frames[self.cur].img;
                            let raw: Vec<u8> = f.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                            evt = Some(match image::save_buffer(&p, &raw, f.size[0] as u32, f.size[1] as u32, image::ColorType::Rgba8) {
                                Ok(()) => ImgEvt::Toast(tf("已导出 {}", &[&p.display()]), OK()),
                                Err(e) => ImgEvt::Toast(e.to_string(), ERR()),
                            });
                        }
                    }
                    ui.separator();
                    if ui.button(t("和另一张图对比…")).on_hover_text(t("也可以直接把另一张图拖到这张图上")).clicked() {
                        ui.close_menu();
                        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
                        if let Some(p) = rfd::FileDialog::new().set_directory(dir).add_filter(t("图片"), IMAGE_EXT).pick_file() {
                            if let Err(e) = self.compare_with(p) {
                                evt = Some(ImgEvt::Toast(e, ERR()));
                            }
                        }
                    }
                    if self.cmp.is_some() && ui.button(t("关闭对比")).clicked() {
                        self.cmp = None;
                        ui.close_menu();
                    }
                });
            });
        });
        if self.cmp.is_some() {
            self.compare_bar(ui);
        }
        ui.add_space(4.0);
        // ---- 画布
        let area = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(area, Sense::click_and_drag());
        let p = ui.painter_at(area);
        p.rect_filled(area, 0.0, BG0());
        let Some(tex) = self.texture(ui.ctx(), self.cur) else { return evt };
        if let Some((_, other, otex, mode, alpha)) = self.cmp.as_mut() {
            let otex = otex.get_or_insert_with(|| ui.ctx().load_texture("img-cmp", other.clone(), TextureOptions::NEAREST)).clone();
            let mode = *mode;
            let alpha = *alpha;
            let osize = other.size;
            if mode == 0 {
                // 并排：两张共用同一缩放，左右各一半
                let half = Rect::from_min_size(area.min, vec2(area.width() / 2.0 - 4.0, area.height()));
                let r = self.xf.place(ui, half, size, &resp);
                let z = r.width() / size[0].max(1) as f32;
                checker(&p, r);
                p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                let r2 = Rect::from_min_size(r.min + vec2(area.width() / 2.0, 0.0), vec2(osize[0] as f32 * z, osize[1] as f32 * z));
                checker(&p, r2);
                p.image(otex.id(), r2, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                p.vline(area.center().x, area.y_range(), st(1.0, LINE()));
            } else {
                let r = self.xf.place(ui, area, size, &resp);
                checker(&p, r);
                if mode == 1 {
                    p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    let z = r.width() / size[0].max(1) as f32;
                    let r2 = Rect::from_min_size(r.min, vec2(osize[0] as f32 * z, osize[1] as f32 * z));
                    p.image(otex.id(), r2, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha((alpha * 255.0) as u8));
                } else {
                    let dt = self.diff_texture(ui.ctx());
                    p.image(dt.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
            return evt;
        }
        let r = self.xf.place(ui, area, size, &resp);
        checker(&p, r);
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let z = r.width() / size[0].max(1) as f32;
        if self.grid && z >= 8.0 {
            let c = Color32::from_black_alpha(60);
            let vis = r.intersect(area);
            let x0 = ((vis.left() - r.left()) / z).floor() as usize;
            let x1 = ((vis.right() - r.left()) / z).ceil() as usize;
            for x in x0..=x1.min(size[0]) {
                p.vline(r.left() + x as f32 * z, vis.y_range(), st(1.0, c));
            }
            let y0 = ((vis.top() - r.top()) / z).floor() as usize;
            let y1 = ((vis.bottom() - r.top()) / z).ceil() as usize;
            for y in y0..=y1.min(size[1]) {
                p.hline(vis.x_range(), r.top() + y as f32 * z, st(1.0, c));
            }
        }
        // 取色
        if let Some(pp) = resp.hover_pos().filter(|pp| r.contains(*pp)) {
            let x = ((pp.x - r.left()) / z) as usize;
            let y = ((pp.y - r.top()) / z) as usize;
            if x < size[0] && y < size[1] {
                let c = self.frames[self.cur].img.pixels[y * size[0] + x];
                let [rr, g, b, a] = c.to_srgba_unmultiplied();
                let info = format!("({x}, {y})  #{rr:02X}{g:02X}{b:02X}  A{a}  RGB565 0x{:04X}", rgb565(Color32::from_rgb(rr, g, b)));
                let tr = Rect::from_min_size(area.left_bottom() + vec2(8.0, -30.0), vec2(360.0, 22.0));
                p.rect_filled(tr, 4.0, BG2());
                p.rect_filled(Rect::from_min_size(tr.min + vec2(6.0, 5.0), vec2(12.0, 12.0)), 2.0, Color32::from_rgb(rr, g, b));
                p.text(tr.min + vec2(24.0, 11.0), egui::Align2::LEFT_CENTER, &info, FontId::monospace(11.5), TEXT());
                if resp.clicked() {
                    ui.ctx().copy_text(format!("#{rr:02X}{g:02X}{b:02X}"));
                    evt = Some(ImgEvt::Toast(tf("已复制颜色 #{}", &[&format!("{rr:02X}{g:02X}{b:02X}")]), OK()));
                }
            }
        }
        evt
    }

    fn compare_bar(&mut self, ui: &mut Ui) {
        let Some((p, other, _, mode, alpha)) = self.cmp.as_mut() else { return };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tf("对比：{}", &[&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()]))
                    .small()
                    .color(TEXT2()),
            );
            for (i, l) in [t("并排"), t("叠加"), t("差异")].iter().enumerate() {
                if ui.selectable_label(*mode == i as u8, *l).clicked() {
                    *mode = i as u8;
                }
            }
            if *mode == 1 {
                ui.add(egui::Slider::new(alpha, 0.0..=1.0).show_value(false));
            }
            let a = &self.frames[self.cur].img;
            if a.size != other.size {
                ui.label(
                    RichText::new(tf("尺寸不同：{}×{} 和 {}×{}", &[&a.size[0], &a.size[1], &other.size[0], &other.size[1]]))
                        .small()
                        .color(WARN()),
                );
            } else if *mode == 2 {
                let n = a.pixels.iter().zip(&other.pixels).filter(|(x, y)| x != y).count();
                ui.label(RichText::new(tf("{} 个像素不同", &[&n])).small().color(if n == 0 { OK() } else { WARN() }));
            }
        });
    }

    /// 差异图：相同的像素变暗，不同的标红
    fn diff_texture(&mut self, ctx: &egui::Context) -> TextureHandle {
        if let Some((t, f)) = &self.diff_tex {
            if *f == self.cur {
                return t.clone();
            }
        }
        let a = &self.frames[self.cur].img;
        let b = &self.cmp.as_ref().unwrap().1;
        let [w, h] = a.size;
        let px: Vec<Color32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let pa = a.pixels[i];
                let pb = (x < b.size[0] && y < b.size[1]).then(|| b.pixels[y * b.size[0] + x]);
                if pb == Some(pa) {
                    Color32::from_rgba_unmultiplied(pa.r() / 3, pa.g() / 3, pa.b() / 3, 255)
                } else {
                    Color32::from_rgb(255, 40, 60)
                }
            })
            .collect();
        let t = ctx.load_texture("img-diff", ColorImage { size: [w, h], pixels: px }, TextureOptions::NEAREST);
        self.diff_tex = Some((t.clone(), self.cur));
        t
    }
}

// ================================================================== 原始像素

#[derive(Clone, Copy, PartialEq)]
pub enum PixFmt {
    Rgb565Le,
    Rgb565Be,
    Rgb888,
    Gray8,
    Mono,
}

impl PixFmt {
    const ALL: [PixFmt; 5] = [PixFmt::Rgb565Le, PixFmt::Rgb565Be, PixFmt::Rgb888, PixFmt::Gray8, PixFmt::Mono];
    fn label(self) -> &'static str {
        match self {
            PixFmt::Rgb565Le => "RGB565 LE",
            PixFmt::Rgb565Be => "RGB565 BE",
            PixFmt::Rgb888 => "RGB888",
            PixFmt::Gray8 => t("灰度 8 位"),
            PixFmt::Mono => t("单色 1 位"),
        }
    }
    /// 每像素的位数
    fn bits(self) -> usize {
        match self {
            PixFmt::Rgb565Le | PixFmt::Rgb565Be => 16,
            PixFmt::Rgb888 => 24,
            PixFmt::Gray8 => 8,
            PixFmt::Mono => 1,
        }
    }
}

/// 字节按格式画成图
pub fn render_raw(data: &[u8], fmt: PixFmt, w: usize, max_h: usize) -> ColorImage {
    let w = w.max(1);
    let bits = fmt.bits();
    let total = data.len() * 8 / bits;
    let h = total.div_ceil(w).clamp(1, max_h);
    let mut px = vec![Color32::BLACK; w * h];
    for (i, p) in px.iter_mut().enumerate() {
        let c = match fmt {
            PixFmt::Rgb565Le | PixFmt::Rgb565Be => {
                let (Some(&a), Some(&b)) = (data.get(i * 2), data.get(i * 2 + 1)) else { break };
                let v = if fmt == PixFmt::Rgb565Le { u16::from_le_bytes([a, b]) } else { u16::from_be_bytes([a, b]) };
                let (r, g, b) = ((v >> 11) & 0x1F, (v >> 5) & 0x3F, v & 0x1F);
                Color32::from_rgb((r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8)
            }
            PixFmt::Rgb888 => {
                let Some(s) = data.get(i * 3..i * 3 + 3) else { break };
                Color32::from_rgb(s[0], s[1], s[2])
            }
            PixFmt::Gray8 => {
                let Some(&v) = data.get(i) else { break };
                Color32::from_gray(v)
            }
            PixFmt::Mono => {
                // 每行按整字节对齐（和取模工具一致），高位在前
                let (x, y) = (i % w, i / w);
                let stride = w.div_ceil(8);
                let Some(&b) = data.get(y * stride + x / 8) else { break };
                if b & (0x80 >> (x % 8)) != 0 {
                    Color32::WHITE
                } else {
                    Color32::BLACK
                }
            }
        };
        *p = c;
    }
    ColorImage { size: [w, h], pixels: px }
}

/// 文字里的 HEX 字节（日志复制来的 "A5 5A 00 ..." 或 0x12,0x34）
pub fn parse_hex(s: &str) -> Vec<u8> {
    let mut out = vec![];
    for tok in s.split(|c: char| !c.is_ascii_hexdigit() && c != 'x' && c != 'X') {
        let tok = tok.trim_start_matches("0x").trim_start_matches("0X");
        if tok.is_empty() || !tok.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let tok = if tok.len() % 2 == 1 { format!("0{tok}") } else { tok.to_string() };
        for i in (0..tok.len()).step_by(2) {
            if let Ok(b) = u8::from_str_radix(&tok[i..i + 2], 16) {
                out.push(b);
            }
        }
    }
    out
}

/// 按像素个数猜宽度：常见屏幕分辨率优先，其次找最接近 4:3 或方形的整除宽度
fn guess_width(px: usize) -> usize {
    const SCREENS: [(usize, usize); 12] = [
        (240, 320),
        (320, 240),
        (240, 240),
        (128, 160),
        (160, 128),
        (128, 128),
        (480, 320),
        (320, 480),
        (128, 64),
        (800, 480),
        (480, 272),
        (135, 240),
    ];
    if let Some((w, _)) = SCREENS.iter().find(|(w, h)| w * h == px) {
        return *w;
    }
    (8..=1024)
        .filter(|w| px % w == 0 && px / w >= 4)
        .min_by_key(|w| {
            let r = *w as f32 / (px / w) as f32;
            ((r - 4.0 / 3.0).abs().min((r - 1.0).abs()) * 1000.0) as u32
        })
        .unwrap_or(240)
}

pub struct PixelView {
    data: Vec<u8>,
    /// 粘贴的 HEX 文字（有内容时用它，不用文件）
    paste: String,
    fmt: PixFmt,
    width: usize,
    offset: usize,
    tex: Option<(TextureHandle, u64)>,
    xf: ViewXf,
}

impl PixelView {
    pub fn new(data: Vec<u8>) -> PixelView {
        let width = guess_width(data.len() / 2);
        PixelView {
            data,
            paste: String::new(),
            fmt: PixFmt::Rgb565Le,
            width,
            offset: 0,
            tex: None,
            xf: ViewXf::default(),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("pix-fmt").selected_text(self.fmt.label()).show_ui(ui, |ui| {
                for f in PixFmt::ALL {
                    changed |= ui.selectable_value(&mut self.fmt, f, f.label()).changed();
                }
            });
            ui.label(RichText::new(t("宽")).color(TEXT2()));
            changed |= ui.add(egui::DragValue::new(&mut self.width).range(1..=4096).speed(1.0)).changed();
            changed |= ui.add(egui::Slider::new(&mut self.width, 8..=1024).show_value(false)).changed();
            for w in [128usize, 160, 240, 320, 480] {
                if ui.small_button(w.to_string()).clicked() {
                    self.width = w;
                    changed = true;
                }
            }
            ui.label(RichText::new(t("偏移")).color(TEXT2()));
            changed |= ui.add(egui::DragValue::new(&mut self.offset).range(0..=self.data.len().max(1)).hexadecimal(1, false, true)).changed();
        });
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.paste)
                .hint_text(t("也可以粘贴日志里的 HEX（A5 5A 00 …），按图像显示"))
                .desired_width(f32::INFINITY),
        );
        changed |= r.changed();
        let src: Vec<u8> = if self.paste.trim().is_empty() {
            self.data.get(self.offset..).unwrap_or(&[]).to_vec()
        } else {
            parse_hex(&self.paste)
        };
        let key = (self.fmt as u64) << 56 ^ (self.width as u64) << 32 ^ self.offset as u64 ^ (self.paste.len() as u64) << 20;
        if changed || self.tex.as_ref().is_none_or(|t| t.1 != key) {
            let img = render_raw(&src, self.fmt, self.width, 4096);
            self.tex = Some((ui.ctx().load_texture("raw-pixels", img, TextureOptions::NEAREST), key));
        }
        let bits = self.fmt.bits();
        let h = (src.len() * 8 / bits).div_ceil(self.width.max(1));
        ui.label(RichText::new(tf("{} 字节 → {} × {}", &[&src.len(), &self.width, &h])).small().color(TEXT3()));
        let Some((tex, _)) = &self.tex else { return };
        let area = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(area, Sense::click_and_drag());
        let p = ui.painter_at(area);
        p.rect_filled(area, 0.0, BG0());
        let size = tex.size();
        let r = self.xf.place(ui, area, size, &resp);
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        p.rect_stroke(r, 0.0, st(1.0, LINE()), egui::StrokeKind::Outside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_guess() {
        assert_eq!(guess_width(64 * 48), 64);
        assert_eq!(guess_width(240 * 320), 240);
    }

    #[test]
    fn c_and_raw() {
        let img = ColorImage {
            size: [2, 1],
            pixels: vec![Color32::from_rgb(255, 0, 0), Color32::from_rgb(0, 0, 0)],
        };
        let s = c_array("x", &[&img], CFmt::Rgb565);
        assert!(s.contains("const uint16_t x[2]"));
        assert!(s.contains("0xF800, 0x0000"));
        let m = c_array("m", &[&img, &img], CFmt::Mono);
        assert!(m.contains("[2][1]"));
        assert!(m.contains("0xC0"), "{m}");
        let r = render_raw(&[0x00, 0xF8, 0xE0, 0x07], PixFmt::Rgb565Le, 2, 10);
        assert_eq!(r.pixels[0], Color32::from_rgb(255, 0, 0));
        assert_eq!(r.pixels[1], Color32::from_rgb(0, 255, 0));
        assert_eq!(parse_hex("A5 5a 0x12,0x3"), vec![0xA5, 0x5A, 0x12, 0x03]);
    }
}

//! 代码编辑器、底部终端、串口终端的背景图片。设置放在一个全局变量里，画的地方只管调用 paint。
//! 图片导入时复制到数据目录（原图删了也不影响），按路径缓存成纹理。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use eframe::egui::{self, pos2, vec2, Color32, Rect, TextureHandle, TextureOptions};
use serde::{Deserialize, Serialize};

use crate::theme::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Editor,
    Shell,
    Serial,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum Fit {
    /// 等比铺满，多出来的裁掉
    #[default]
    Cover,
    /// 等比放进去，居中
    Contain,
    /// 原尺寸平铺
    Tile,
    /// 原尺寸靠右下角（适合角落里的插画 / logo）
    Corner,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct BgCfg {
    pub on: bool,
    /// 三处共用 editor 那张
    pub same: bool,
    pub editor: String,
    pub shell: String,
    pub serial: String,
    /// 图片不透明度 0.03 – 0.6
    pub opacity: f32,
    pub fit: Fit,
    /// 图片上再盖一层背景色的渐变（从左到右），让左侧代码区更干净
    pub fade: bool,
}

impl Default for BgCfg {
    fn default() -> Self {
        BgCfg {
            on: true,
            same: true,
            editor: String::new(),
            shell: String::new(),
            serial: String::new(),
            opacity: 0.15,
            fit: Fit::Cover,
            fade: false,
        }
    }
}

impl BgCfg {
    pub fn path(&self, slot: Slot) -> &str {
        if self.same {
            return &self.editor;
        }
        match slot {
            Slot::Editor => &self.editor,
            Slot::Shell => &self.shell,
            Slot::Serial => &self.serial,
        }
    }
}

static CFG: RwLock<Option<BgCfg>> = RwLock::new(None);
/// 路径 → 纹理（None = 读失败，不再重试）
static TEX: Mutex<Option<HashMap<String, Option<TextureHandle>>>> = Mutex::new(None);

pub fn set(cfg: &BgCfg) {
    if let Ok(mut c) = CFG.write() {
        if c.as_ref() != Some(cfg) {
            *c = Some(cfg.clone());
        }
    }
}

/// 把用户选的图片复制到数据目录，返回新路径
pub fn import(src: &Path) -> Result<PathBuf, String> {
    let dir = crate::data_dir().join("backgrounds");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 先确认能解码，免得存一张打不开的
    image::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bg.png".into());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dst = dir.join(format!("{stamp}-{name}"));
    std::fs::copy(src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

fn texture(ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
    let mut g = TEX.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(t) = map.get(path) {
        return t.clone();
    }
    // 大图缩到长边 2560，省显存
    let t = image::open(path).ok().map(|img| {
        let img = if img.width().max(img.height()) > 2560 {
            img.resize(2560, 2560, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
        ctx.load_texture(
            format!("bg:{path}"),
            ci,
            TextureOptions {
                wrap_mode: egui::TextureWrapMode::Repeat,
                ..TextureOptions::LINEAR
            },
        )
    });
    map.insert(path.to_string(), t.clone());
    t
}

/// 在 rect 里画背景图（在底色之后、文字之前调用）
pub fn paint(painter: &egui::Painter, rect: Rect, slot: Slot) {
    let Some(cfg) = CFG.read().ok().and_then(|c| c.clone()) else { return };
    let path = cfg.path(slot);
    if !cfg.on || path.is_empty() {
        return;
    }
    let Some(tex) = texture(painter.ctx(), path) else { return };
    let size = tex.size_vec2();
    if size.x < 1.0 || size.y < 1.0 {
        return;
    }
    let tint = Color32::WHITE.gamma_multiply(cfg.opacity.clamp(0.03, 0.6));
    let p = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    match cfg.fit {
        Fit::Cover | Fit::Contain => {
            let s = if cfg.fit == Fit::Cover {
                (rect.width() / size.x).max(rect.height() / size.y)
            } else {
                (rect.width() / size.x).min(rect.height() / size.y)
            };
            let r = Rect::from_center_size(rect.center(), size * s);
            p.image(tex.id(), r, full, tint);
        }
        Fit::Tile => {
            // UV 超过 1 配合 Repeat 就是平铺
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(rect.width() / size.x, rect.height() / size.y));
            p.image(tex.id(), rect, uv, tint);
        }
        Fit::Corner => {
            // 太大就缩到区域的一半
            let s = (rect.width() * 0.5 / size.x).min(rect.height() * 0.6 / size.y).min(1.0);
            let sz = size * s;
            let r = Rect::from_min_size(rect.max - sz - vec2(16.0, 16.0), sz);
            p.image(tex.id(), r, full, tint);
        }
    }
    if cfg.fade {
        // 左边盖住、右边露出：代码大多在左边
        let mut mesh = egui::Mesh::default();
        let (a, b) = (BG0(), BG0().gamma_multiply(0.0));
        mesh.colored_vertex(rect.left_top(), a);
        mesh.colored_vertex(rect.right_top(), b);
        mesh.colored_vertex(rect.right_bottom(), b);
        mesh.colored_vertex(rect.left_bottom(), a);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(egui::Shape::mesh(mesh));
    }
}

/// 有没有任何一处设置了背景图（命令面板「隐藏 / 显示背景图」用）
pub fn any(cfg: &BgCfg) -> bool {
    !cfg.editor.is_empty() || !cfg.same && (!cfg.shell.is_empty() || !cfg.serial.is_empty())
}

//! 主题：冷调深色、靠底色深浅和细分隔线分层，青色只用在焦点上。
//! 字体内置 JetBrains Mono（日志）和 Inter（界面），中文回退到系统字体。

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

// 底色层级：越往上越亮
pub const BG0: Color32 = Color32::from_rgb(0x08, 0x09, 0x0c); // 日志区
pub const BG1: Color32 = Color32::from_rgb(0x0e, 0x10, 0x15); // 侧边栏、面板
pub const BG2: Color32 = Color32::from_rgb(0x15, 0x18, 0x1f); // 卡片、输入框
pub const BG3: Color32 = Color32::from_rgb(0x1c, 0x20, 0x29); // 悬停
pub const LINE: Color32 = Color32::from_rgb(0x23, 0x28, 0x33); // 细分隔线
pub const LINE_HI: Color32 = Color32::from_rgb(0x33, 0x3a, 0x48);

pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xef);
pub const TEXT2: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xb2);
pub const TEXT3: Color32 = Color32::from_rgb(0x5d, 0x66, 0x75);
pub const LOG_TEXT: Color32 = Color32::from_rgb(0xc9, 0xd1, 0xdc);

pub const ACCENT: Color32 = Color32::from_rgb(0x2d, 0xe2, 0xe6);
pub const VIOLET: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
pub const OK: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const WARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const ERR: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub const INFO: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const DEBUG: Color32 = Color32::from_rgb(0x60, 0xa5, 0xfa);
pub const TRACE: Color32 = Color32::from_rgb(0x6b, 0x72, 0x80);

pub const R_CARD: CornerRadius = CornerRadius::same(8);
pub const R_WIDGET: CornerRadius = CornerRadius::same(5);

pub fn st(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn mono_bold() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

pub fn apply(ctx: &egui::Context, font_size: f32) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG1;
    v.window_fill = BG2;
    v.extreme_bg_color = BG2; // 输入框底色
    v.faint_bg_color = BG2;
    v.code_bg_color = BG2;
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.28);
    v.selection.stroke = st(1.0, ACCENT);
    v.window_stroke = st(1.0, LINE);
    v.window_corner_radius = R_CARD;
    v.menu_corner_radius = R_CARD;
    v.popup_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(180),
    };
    v.window_shadow = v.popup_shadow;
    v.text_cursor.stroke = st(2.0, ACCENT);
    v.striped = false;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG1;
    w.noninteractive.weak_bg_fill = BG1;
    w.noninteractive.bg_stroke = st(1.0, LINE);
    w.noninteractive.fg_stroke = st(1.0, TEXT2);
    w.inactive.bg_fill = BG2;
    w.inactive.weak_bg_fill = BG2;
    w.inactive.bg_stroke = st(1.0, LINE);
    w.inactive.fg_stroke = st(1.0, TEXT);
    w.hovered.bg_fill = BG3;
    w.hovered.weak_bg_fill = BG3;
    w.hovered.bg_stroke = st(1.0, LINE_HI);
    w.hovered.fg_stroke = st(1.0, TEXT);
    w.hovered.expansion = 0.0;
    w.active.bg_fill = BG3;
    w.active.weak_bg_fill = BG3;
    w.active.bg_stroke = st(1.0, ACCENT.gamma_multiply(0.8));
    w.active.fg_stroke = st(1.0, TEXT);
    w.active.expansion = 0.0;
    w.open.bg_fill = BG3;
    w.open.weak_bg_fill = BG3;
    w.open.bg_stroke = st(1.0, LINE_HI);
    w.open.fg_stroke = st(1.0, TEXT);
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = R_WIDGET;
    }
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.combo_height = 320.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.interaction.selectable_labels = false;
        s.text_styles = [
            (TextStyle::Heading, FontId::new(15.0, bold())),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(font_size, FontFamily::Monospace)),
        ]
        .into();
    });
}

pub fn load_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add("inter", include_bytes!("../assets/Inter-Regular.ttf"));
    add("inter-semibold", include_bytes!("../assets/Inter-SemiBold.ttf"));
    add("jbmono", include_bytes!("../assets/JetBrainsMono-Regular.ttf"));
    add("jbmono-bold", include_bytes!("../assets/JetBrainsMono-Bold.ttf"));

    // 回退顺序：界面字体 → 等宽字体（符号多）→ 系统中文字体
    let fam = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Proportional, fam(&["inter", "jbmono"]));
    fonts.families.insert(bold(), fam(&["inter-semibold", "jbmono-bold"]));
    fonts.families.insert(FontFamily::Monospace, fam(&["jbmono", "inter"]));
    fonts.families.insert(mono_bold(), fam(&["jbmono-bold", "jbmono"]));

    // 中文：用系统字体做后备（不内置，保持体积小）。
    // 用内存映射而不是整个读进内存：只有真正用到的字形所在的页才会占内存，
    // 微软雅黑这类 20 MB 的字体实际只占几 MB。
    let candidates: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        "/usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    // 中文字体里没有的韩文、日文假名再各找一个系统字体兜底（同样是内存映射，没用到就几乎不占内存）
    let korean: &[&str] = &[
        "C:\\Windows\\Fonts\\malgun.ttf",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
        "/usr/share/fonts/nanum/NanumGothic.ttf",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    ];
    let japanese: &[&str] = &["C:\\Windows\\Fonts\\YuGothM.ttc", "C:\\Windows\\Fonts\\meiryo.ttc", "C:\\Windows\\Fonts\\msgothic.ttc"];
    for (name, list) in [("cjk", candidates), ("kr", korean), ("jp", japanese)] {
        if let Some(bytes) = list.iter().find_map(|p| map_font(p)) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
            for fam in [FontFamily::Proportional, FontFamily::Monospace, bold(), mono_bold()] {
                fonts.families.entry(fam).or_default().push(name.into());
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// 把字体文件映射进内存。映射在程序整个运行期间都要用，所以有意不释放。
fn map_font(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // 安全性：系统字体文件在运行期间不会被改写
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let map: &'static memmap2::Mmap = Box::leak(Box::new(map));
    Some(&map[..])
}

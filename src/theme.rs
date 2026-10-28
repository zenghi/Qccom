//! 主题：冷调深色、靠底色深浅和细分隔线分层，青色只用在焦点上。
//! 字体内置 JetBrains Mono（日志）和 Inter（界面），中文回退到系统字体。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

/// 一套界面配色。深色靠底色深浅和细分隔线分层，浅色反过来用白卡片 + 浅灰面板。
pub struct Palette {
    pub bg0: Color32,
    pub bg1: Color32,
    pub bg2: Color32,
    pub bg3: Color32,
    pub line: Color32,
    pub line_hi: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub log_text: Color32,
    pub accent: Color32,
    /// 强调色底上的文字
    pub on_accent: Color32,
    pub violet: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub debug: Color32,
    pub trace: Color32,
    pub shadow: Color32,
    /// 搜索命中的底色和文字
    pub find_bg: Color32,
    pub find_fg: Color32,
}

pub const DARK: Palette = Palette {
    bg0: Color32::from_rgb(0x08, 0x09, 0x0c),
    bg1: Color32::from_rgb(0x0e, 0x10, 0x15),
    bg2: Color32::from_rgb(0x15, 0x18, 0x1f),
    bg3: Color32::from_rgb(0x1c, 0x20, 0x29),
    line: Color32::from_rgb(0x23, 0x28, 0x33),
    line_hi: Color32::from_rgb(0x33, 0x3a, 0x48),
    text: Color32::from_rgb(0xe6, 0xe9, 0xef),
    text2: Color32::from_rgb(0xa3, 0xab, 0xb9),
    text3: Color32::from_rgb(0x76, 0x80, 0x90),
    log_text: Color32::from_rgb(0xc9, 0xd1, 0xdc),
    accent: Color32::from_rgb(0x2d, 0xe2, 0xe6),
    on_accent: Color32::from_rgb(0x06, 0x1a, 0x1c),
    violet: Color32::from_rgb(0xa7, 0x8b, 0xfa),
    ok: Color32::from_rgb(0x34, 0xd3, 0x99),
    warn: Color32::from_rgb(0xfb, 0xbf, 0x24),
    err: Color32::from_rgb(0xf8, 0x71, 0x71),
    debug: Color32::from_rgb(0x60, 0xa5, 0xfa),
    trace: Color32::from_rgb(0x7b, 0x83, 0x91),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 180),
    find_bg: Color32::from_rgb(0xfb, 0xbf, 0x24),
    find_fg: Color32::from_rgb(0x08, 0x09, 0x0c),
};

pub const LIGHT: Palette = Palette {
    bg0: Color32::from_rgb(0xff, 0xff, 0xff),
    bg1: Color32::from_rgb(0xf5, 0xf6, 0xf8),
    bg2: Color32::from_rgb(0xff, 0xff, 0xff),
    bg3: Color32::from_rgb(0xec, 0xee, 0xf2),
    line: Color32::from_rgb(0xe1, 0xe4, 0xea),
    line_hi: Color32::from_rgb(0xc9, 0xcf, 0xd8),
    text: Color32::from_rgb(0x1a, 0x1e, 0x25),
    text2: Color32::from_rgb(0x4f, 0x58, 0x66),
    text3: Color32::from_rgb(0x6b, 0x74, 0x80),
    log_text: Color32::from_rgb(0x24, 0x29, 0x31),
    accent: Color32::from_rgb(0x08, 0x7f, 0x8c),
    on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
    violet: Color32::from_rgb(0x6d, 0x3a, 0xd8),
    ok: Color32::from_rgb(0x1a, 0x7f, 0x37),
    warn: Color32::from_rgb(0x9a, 0x67, 0x00),
    err: Color32::from_rgb(0xcf, 0x22, 0x2e),
    debug: Color32::from_rgb(0x09, 0x69, 0xda),
    trace: Color32::from_rgb(0x6e, 0x77, 0x81),
    shadow: Color32::from_rgba_premultiplied(0x10, 0x18, 0x28, 40),
    find_bg: Color32::from_rgb(0xff, 0xdc, 0x5c),
    find_fg: Color32::from_rgb(0x1a, 0x1e, 0x25),
};

static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

pub fn pal() -> &'static Palette {
    if is_light() {
        &LIGHT
    } else {
        &DARK
    }
}

// 下面这些名字保留成大写，读起来和以前的常量一样；现在会随主题变化
macro_rules! colors {
    ($($name:ident => $field:ident),* $(,)?) => {
        $(
            #[allow(non_snake_case)]
            #[inline]
            pub fn $name() -> Color32 {
                pal().$field
            }
        )*
    };
}
colors! {
    BG0 => bg0, BG1 => bg1, BG2 => bg2, BG3 => bg3, LINE => line, LINE_HI => line_hi,
    TEXT => text, TEXT2 => text2, TEXT3 => text3, LOG_TEXT => log_text,
    ACCENT => accent, ON_ACCENT => on_accent, VIOLET => violet,
    OK => ok, WARN => warn, ERR => err, INFO => ok, DEBUG => debug, TRACE => trace,
    FIND_BG => find_bg, FIND_FG => find_fg,
}

/// 内容颜色（ANSI、关键词等为深色背景设计的颜色）在浅色主题下自动压暗，保证和背景至少 4.5:1 的对比度
pub fn ink(c: Color32) -> Color32 {
    if !is_light() {
        return c;
    }
    let bg = luminance(pal().bg0);
    let mut x = c;
    for _ in 0..12 {
        if contrast(luminance(x), bg) >= 4.5 {
            break;
        }
        x = Color32::from_rgb((x.r() as f32 * 0.85) as u8, (x.g() as f32 * 0.85) as u8, (x.b() as f32 * 0.85) as u8);
    }
    x
}

pub fn luminance(c: Color32) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
}

pub fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// 主题设置："dark" / "light" / "system"
pub fn resolve(mode: &str, ctx: &egui::Context) -> bool {
    match mode {
        "light" => true,
        "dark" => false,
        _ => ctx.system_theme() == Some(egui::Theme::Light),
    }
}

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

pub fn apply(ctx: &egui::Context, font_size: f32, light: bool) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = BG1();
    v.window_fill = BG2();
    v.extreme_bg_color = BG2(); // 输入框底色
    v.faint_bg_color = BG2();
    v.code_bg_color = BG2();
    v.hyperlink_color = ACCENT();
    v.selection.bg_fill = ACCENT().gamma_multiply(0.28);
    v.selection.stroke = st(1.0, ACCENT());
    v.window_stroke = st(1.0, LINE());
    v.window_corner_radius = R_CARD;
    v.menu_corner_radius = R_CARD;
    v.popup_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: pal().shadow,
    };
    v.window_shadow = v.popup_shadow;
    v.text_cursor.stroke = st(2.0, ACCENT());
    v.striped = false;
    v.slider_trailing_fill = true;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG1();
    w.noninteractive.weak_bg_fill = BG1();
    w.noninteractive.bg_stroke = st(1.0, LINE());
    w.noninteractive.fg_stroke = st(1.0, TEXT2());
    w.inactive.bg_fill = BG2();
    w.inactive.weak_bg_fill = BG2();
    w.inactive.bg_stroke = st(1.0, LINE());
    w.inactive.fg_stroke = st(1.0, TEXT());
    w.hovered.bg_fill = BG3();
    w.hovered.weak_bg_fill = BG3();
    w.hovered.bg_stroke = st(1.0, LINE_HI());
    w.hovered.fg_stroke = st(1.0, TEXT());
    w.hovered.expansion = 0.0;
    w.active.bg_fill = BG3();
    w.active.weak_bg_fill = BG3();
    w.active.bg_stroke = st(1.0, ACCENT().gamma_multiply(0.8));
    w.active.fg_stroke = st(1.0, TEXT());
    w.active.expansion = 0.0;
    w.open.bg_fill = BG3();
    w.open.weak_bg_fill = BG3();
    w.open.bg_stroke = st(1.0, LINE_HI());
    w.open.fg_stroke = st(1.0, TEXT());
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = R_WIDGET;
    }
    // 两套 egui 主题都设成同一份样式，并固定主题偏好，
    // 否则系统切换深浅色时 egui 会换回它自带的样式（弹窗变成白底，文字看不清）
    let pref = if light { egui::ThemePreference::Light } else { egui::ThemePreference::Dark };
    ctx.options_mut(|o| o.theme_preference = pref);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);

    ctx.all_styles_mut(|s| {
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
    // egui 子菜单的箭头 ⏵：从 JetBrains Mono 里取 ▸ 做成的 1 KB 小字体
    add("symbols", include_bytes!("../assets/qccom-symbols.ttf"));

    // 回退顺序：界面字体 → 等宽字体（符号多）→ 系统中文字体
    let fam = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    fonts.families.insert(FontFamily::Proportional, fam(&["inter", "jbmono", "symbols"]));
    fonts.families.insert(bold(), fam(&["inter-semibold", "jbmono-bold", "symbols"]));
    fonts.families.insert(FontFamily::Monospace, fam(&["jbmono", "inter", "symbols"]));
    fonts.families.insert(mono_bold(), fam(&["jbmono-bold", "jbmono", "symbols"]));

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

/// 很淡的叠加色：深色主题提亮一点，浅色主题压暗一点（行悬停、当前行等）
pub fn tint(alpha: u8) -> Color32 {
    if is_light() {
        Color32::from_black_alpha((alpha as f32 * 0.7) as u8)
    } else {
        Color32::from_white_alpha(alpha)
    }
}

/// 阴影：浅色主题下要轻得多，否则显脏
pub fn shade(alpha: u8) -> Color32 {
    if is_light() {
        Color32::from_rgba_unmultiplied(0x10, 0x18, 0x28, (alpha as f32 * 0.22) as u8)
    } else {
        Color32::from_black_alpha(alpha)
    }
}

/// 主按钮悬停色
pub fn accent_hover() -> Color32 {
    if is_light() {
        Color32::from_rgb(0x06, 0x6c, 0x77)
    } else {
        Color32::from_rgb(0x67, 0xe8, 0xf9)
    }
}

/// 原子图标是亮色线条，浅色背景上压暗成主题青色才看得清
pub fn logo_tint() -> Color32 {
    if is_light() {
        Color32::from_rgb(0x0a, 0x5c, 0x66)
    } else {
        Color32::WHITE
    }
}

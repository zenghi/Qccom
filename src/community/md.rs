//! 帖子正文的显示：一个精简的 Markdown。和文档预览不同，这里保留换行（大家写帖子习惯直接回车换行）。
//! 支持：代码块（带高亮）、引用、列表、标题、行内粗体 / 斜体 / 代码 / 链接、`![](img:ID)` 图片。

use eframe::egui::{self, vec2, FontFamily, FontId, RichText, Sense, Ui};

use crate::highlight::{self, Lang};
use crate::theme::*;
use crate::viewer::md::inline_spans;

/// 图片 ID 列表：一行里只有图片时返回它们
fn images_only(line: &str) -> Option<Vec<String>> {
    let mut rest = line.trim();
    let mut ids = vec![];
    while !rest.is_empty() {
        let r = rest.strip_prefix("![")?;
        let close = r.find("](")?;
        let r = &r[close + 2..];
        let end = r.find(')')?;
        let target = &r[..end];
        ids.push(target.strip_prefix("img:")?.to_string());
        rest = r[end + 1..].trim_start();
    }
    (!ids.is_empty()).then_some(ids)
}

pub fn image_ids(text: &str) -> Vec<String> {
    text.lines().filter_map(images_only).flatten().collect()
}

fn lang_of(s: &str) -> Lang {
    let s = s.trim();
    if s.is_empty() || s == "text" || s == "log" {
        Lang::Plain
    } else {
        Lang::from_path(std::path::Path::new(&format!("x.{s}")))
    }
}

/// 画正文。img(id) 返回已经下载好的纹理；返回被点击的图片 ID（打开大图）
pub fn body(ui: &mut Ui, text: &str, img: &mut dyn FnMut(&str) -> Option<(egui::TextureId, [usize; 2])>) -> Option<String> {
    let mut clicked = None;
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut gap = false;
    ui.spacing_mut().item_spacing.y = 3.0;
    while i < lines.len() {
        let l = lines[i];
        let tr = l.trim();
        if tr.is_empty() {
            gap = true;
            i += 1;
            continue;
        }
        if gap {
            ui.add_space(6.0);
            gap = false;
        }
        // 代码块
        if let Some(rest) = tr.strip_prefix("```") {
            let lang = lang_of(rest);
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            let code = code.join("\n");
            ui.add_space(2.0);
            let resp = egui::Frame::new()
                .fill(BG1())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_WIDGET)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::horizontal().id_salt(("comm-code", i)).show(ui, |ui| {
                        let job = highlight::layout(&code, lang, FontId::new(12.5, FontFamily::Monospace));
                        ui.add(egui::Label::new(job).extend());
                    });
                })
                .response;
            // 右上角复制
            let r = egui::Rect::from_min_size(resp.rect.right_top() + vec2(-52.0, 4.0), vec2(48.0, 20.0));
            let h = ui.interact(r, ui.id().with(("copy", i)), Sense::click());
            if resp.hovered() || h.hovered() {
                let p = ui.painter();
                p.rect(r, egui::CornerRadius::same(4), if h.hovered() { BG3() } else { BG2() }, st(1.0, LINE()), egui::StrokeKind::Inside);
                p.text(r.center(), egui::Align2::CENTER_CENTER, crate::i18n::t("复制"), FontId::proportional(11.0), TEXT2());
            }
            if h.clicked() {
                ui.ctx().copy_text(code);
            }
            ui.add_space(2.0);
            continue;
        }
        // 引用（连续的 > 行）
        if tr.starts_with('>') {
            let mut q = vec![];
            while i < lines.len() && lines[i].trim().starts_with('>') {
                q.push(lines[i].trim().trim_start_matches('>').trim_start());
                i += 1;
            }
            let top = ui.cursor().top();
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 12,
                    right: 0,
                    top: 1,
                    bottom: 1,
                })
                .show(ui, |ui| {
                    for l in q {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(l).color(TEXT2()));
                        });
                    }
                });
            let x = ui.min_rect().left() + 2.0;
            ui.painter().vline(x, egui::Rangef::new(top, ui.cursor().top() - 3.0), st(2.5, LINE_HI()));
            continue;
        }
        // 图片
        if let Some(ids) = images_only(tr) {
            ui.horizontal_wrapped(|ui| {
                for id in ids {
                    let max_w = (ui.available_width()).min(460.0);
                    match img(&id) {
                        Some((tex, [w, h])) => {
                            let (w, h) = (w as f32, h as f32);
                            let s = (max_w / w).min(320.0 / h).min(1.0);
                            let size = vec2(w * s, h * s);
                            let r = ui.add(egui::Image::new((tex, size)).corner_radius(6).sense(Sense::click()));
                            ui.painter().rect_stroke(r.rect, 6.0, st(1.0, LINE()), egui::StrokeKind::Inside);
                            if r.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
                            }
                            if r.clicked() {
                                clicked = Some(id.clone());
                            }
                        }
                        None => super::widgets::skel(ui, 200.0, 120.0),
                    }
                }
            });
            i += 1;
            continue;
        }
        // 标题
        if tr.starts_with('#') {
            let n = tr.chars().take_while(|c| *c == '#').count();
            if n <= 6 && tr[n..].starts_with(' ') {
                ui.add_space(2.0);
                ui.label(RichText::new(tr[n..].trim()).font(FontId::new(if n <= 1 { 17.0 } else { 15.0 }, bold())).color(TEXT()));
                i += 1;
                continue;
            }
        }
        // 列表
        let item = tr.strip_prefix("- ").or_else(|| tr.strip_prefix("* ")).map(|s| ("•".to_string(), s)).or_else(|| {
            let d = tr.chars().take_while(|c| c.is_ascii_digit()).count();
            (d > 0 && d < 4 && tr[d..].starts_with(". ")).then(|| (format!("{}.", &tr[..d]), &tr[d + 2..]))
        });
        if let Some((mark, s)) = item {
            let indent = (l.len() - l.trim_start().len()).min(8) as f32 * 6.0;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(4.0 + indent);
                ui.label(RichText::new(format!("{mark} ")).color(TEXT3()));
                inline_spans(ui, s, false);
            });
            i += 1;
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            inline_spans(ui, l.trim_end(), false);
        });
        i += 1;
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn img_lines() {
        assert_eq!(images_only("![](img:ab12)"), Some(vec!["ab12".into()]));
        assert_eq!(images_only("![a](img:x) ![](img:y)"), Some(vec!["x".into(), "y".into()]));
        assert_eq!(images_only("看图 ![](img:x)"), None);
        assert_eq!(images_only("![](http://x)"), None);
        assert_eq!(image_ids("a\n![](img:1)\n```\n```"), vec!["1".to_string()]);
    }
}

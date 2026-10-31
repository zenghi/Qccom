//! 输入法（中文、日文、韩文……）：自绘的输入控件（代码编辑器、终端）要自己告诉窗口「这里能输入」，
//! 否则系统不会打开输入法，只能敲英文。组字过程中显示正在输入的拼音，提交的文字当普通输入处理。

use eframe::egui::{self, Event, ImeEvent, Key, Rect};

#[derive(Default, Clone, Debug)]
pub struct Ime {
    /// 正在组字的内容（拼音 / 假名），空表示没在组字
    pub preedit: String,
}

impl Ime {
    /// 把输入法事件翻译掉：提交的文字变成 Text；组字期间去掉会打架的按键（退格、方向键、回车由输入法自己用）
    pub fn filter(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            match e {
                Event::Ime(ImeEvent::Preedit(s)) => self.preedit = s,
                Event::Ime(ImeEvent::Commit(s)) => {
                    self.preedit.clear();
                    if !s.is_empty() {
                        out.push(Event::Text(s));
                    }
                }
                Event::Ime(ImeEvent::Disabled) => self.preedit.clear(),
                Event::Ime(_) => {}
                Event::Key { key, repeat, .. } if !self.preedit.is_empty() => {
                    let clash = repeat
                        || matches!(
                            key,
                            Key::Backspace | Key::Delete | Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight | Key::Enter | Key::Escape | Key::Tab | Key::Space
                        );
                    if !clash {
                        out.push(e);
                    }
                }
                Event::Text(_) if !self.preedit.is_empty() => {}
                e => out.push(e),
            }
        }
        out
    }

    /// 控件有焦点时每帧调用：打开输入法，候选框跟着光标
    pub fn allow(ui: &egui::Ui, rect: Rect, cursor: Rect) {
        let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
        ui.ctx().output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                rect: to_global * rect,
                cursor_rect: to_global * cursor,
            });
        });
    }

    /// 在光标处画出正在组字的内容（带下划线），返回画出来的宽度
    pub fn paint(&self, p: &egui::Painter, at: egui::Pos2, h: f32, font: egui::FontId, fg: egui::Color32, bg: egui::Color32) -> f32 {
        if self.preedit.is_empty() {
            return 0.0;
        }
        let g = p.layout_no_wrap(self.preedit.clone(), font, fg);
        let w = g.size().x;
        let r = Rect::from_min_size(at, egui::vec2(w + 2.0, h));
        p.rect_filled(r, 2.0, bg);
        let y = at.y + (h - g.size().y) / 2.0;
        p.galley(egui::pos2(at.x + 1.0, y), g, fg);
        p.hline(at.x + 1.0..=at.x + 1.0 + w, at.y + h - 2.0, egui::Stroke::new(1.0, fg));
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_becomes_text_and_preedit_swallows_keys() {
        let mut ime = Ime::default();
        let key = |k| Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let out = ime.filter(vec![Event::Ime(ImeEvent::Preedit("ni".into())), key(Key::Backspace), key(Key::Enter)]);
        assert!(out.is_empty());
        assert_eq!(ime.preedit, "ni");
        let out = ime.filter(vec![Event::Ime(ImeEvent::Commit("你".into())), key(Key::Enter)]);
        assert!(ime.preedit.is_empty());
        assert!(matches!(&out[0], Event::Text(s) if s == "你"));
        assert!(matches!(out[1], Event::Key { key: Key::Enter, .. }));
    }
}

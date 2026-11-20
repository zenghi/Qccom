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
    pub fn filter(&mut self, mut events: Vec<Event>) -> Vec<Event> {
        drop_ime_keys(&mut events);
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

/// Windows：输入法组字时按的键，系统发来的是 VK_PROCESSKEY（「输入法处理过的键」）。
/// egui-winit 认不出它，会退回用物理键，于是「拼音 + 回车上屏字母」被当成普通回车换行，
/// 退格、方向键也会动到正文。这里在消息循环里记下这些键，窗口收到输入时把对应的按键事件删掉。
pub fn install() {
    #[cfg(windows)]
    win::install();
}

/// 删掉被输入法处理过的按键（每帧拿到原始输入时调用一次即可，重复调用无害）
pub fn drop_ime_keys(events: &mut Vec<Event>) {
    #[cfg(windows)]
    win::drop_keys(events);
    #[cfg(not(windows))]
    let _ = events;
}

#[cfg(windows)]
mod win {
    use super::{Event, Key};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// 输入法吃掉的按键：(哪个键, 什么时候)
    static PENDING: Mutex<Vec<(Key, Instant)>> = Mutex::new(Vec::new());

    #[repr(C)]
    struct Msg {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
    }

    type HookProc = unsafe extern "system" fn(i32, usize, isize) -> isize;

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowsHookExW(id: i32, f: HookProc, hmod: isize, thread: u32) -> isize;
        fn CallNextHookEx(hook: isize, code: i32, w: usize, l: isize) -> isize;
        fn MapVirtualKeyW(code: u32, map: u32) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    const WH_GETMESSAGE: i32 = 3;
    const PM_REMOVE: usize = 1;
    const WM_KEYDOWN: u32 = 0x0100;
    const VK_PROCESSKEY: usize = 0xE5;
    const MAPVK_VSC_TO_VK_EX: u32 = 3;

    unsafe extern "system" fn hook(code: i32, w: usize, l: isize) -> isize {
        if code >= 0 && w == PM_REMOVE && l != 0 {
            let m = unsafe { &*(l as *const Msg) };
            if m.message == WM_KEYDOWN && m.wparam == VK_PROCESSKEY {
                let sc = ((m.lparam >> 16) & 0xff) as u32;
                let ext = (m.lparam >> 24) & 1 == 1;
                let vk = unsafe { MapVirtualKeyW(if ext { 0xE000 | sc } else { sc }, MAPVK_VSC_TO_VK_EX) };
                if let Some(k) = key_of(vk) {
                    if let Ok(mut p) = PENDING.lock() {
                        p.retain(|(_, t)| t.elapsed() < Duration::from_secs(2));
                        p.push((k, Instant::now()));
                    }
                }
            }
        }
        unsafe { CallNextHookEx(0, code, w, l) }
    }

    fn key_of(vk: u32) -> Option<Key> {
        Some(match vk {
            0x0D => Key::Enter,
            0x08 => Key::Backspace,
            0x09 => Key::Tab,
            0x1B => Key::Escape,
            0x20 => Key::Space,
            0x21 => Key::PageUp,
            0x22 => Key::PageDown,
            0x23 => Key::End,
            0x24 => Key::Home,
            0x25 => Key::ArrowLeft,
            0x26 => Key::ArrowUp,
            0x27 => Key::ArrowRight,
            0x28 => Key::ArrowDown,
            0x2E => Key::Delete,
            0x30..=0x39 | 0x41..=0x5A => return Key::from_name(&char::from(vk as u8).to_string()),
            0xBA => Key::Semicolon,
            0xBB => Key::Equals,
            0xBC => Key::Comma,
            0xBD => Key::Minus,
            0xBE => Key::Period,
            0xBF => Key::Slash,
            0xDB => Key::OpenBracket,
            0xDC => Key::Backslash,
            0xDD => Key::CloseBracket,
            0xDE => Key::Quote,
            _ => return None,
        })
    }

    pub fn install() {
        unsafe {
            SetWindowsHookExW(WH_GETMESSAGE, hook, 0, GetCurrentThreadId());
        }
    }

    pub fn drop_keys(events: &mut Vec<Event>) {
        let Ok(mut p) = PENDING.lock() else { return };
        p.retain(|(_, t)| t.elapsed() < Duration::from_secs(2));
        if p.is_empty() {
            return;
        }
        events.retain(|e| match e {
            Event::Key { key, pressed: true, .. } => match p.iter().position(|(k, _)| k == key) {
                Some(i) => {
                    p.remove(i);
                    false
                }
                None => true,
            },
            _ => true,
        });
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

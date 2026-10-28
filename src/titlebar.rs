//! 系统标题栏跟随界面主题。Windows 10/11 用 DWM 切深色标题栏，Win11 再把标题栏底色、文字和边框
//! 改成界面的颜色；Linux 的标题栏由桌面环境决定，这里不处理。

use eframe::egui::Color32;

#[cfg(windows)]
pub fn apply(frame: &eframe::Frame, dark: bool, caption: Color32, text: Color32, border: Color32) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    }
    // 每帧都会调用，颜色没变就不重复设置
    static LAST: std::sync::Mutex<Option<(bool, Color32, Color32, Color32)>> = std::sync::Mutex::new(None);
    let key = Some((dark, caption, text, border));
    if let Ok(mut last) = LAST.lock() {
        if *last == key {
            return;
        }
        *last = key;
    }
    let Ok(h) = frame.window_handle() else { return };
    let RawWindowHandle::Win32(w) = h.as_raw() else { return };
    let hwnd = w.hwnd.get();
    // COLORREF 是 0x00BBGGRR
    let rgb = |c: Color32| c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16;
    let set = |attr: u32, v: u32| unsafe { DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const _, 4) };
    // 20 = DWMWA_USE_IMMERSIVE_DARK_MODE；Win10 2004 以前的版本是 19
    if set(20, dark as u32) != 0 {
        set(19, dark as u32);
    }
    // 35 / 36 / 34 = 标题栏底色 / 文字 / 边框，只有 Win11 支持，旧系统返回错误，忽略即可
    set(35, rgb(caption));
    set(36, rgb(text));
    set(34, rgb(border));
}

#[cfg(not(windows))]
pub fn apply(_frame: &eframe::Frame, _dark: bool, _caption: Color32, _text: Color32, _border: Color32) {}

//! Opt-in Windows desktop shortcuts. Register only the combinations chosen by the user.
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub right_paste: bool,
    pub commands: Vec<Command>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Command {
    pub keys: String,
    pub text: String,
    pub enter: bool,
}

#[cfg(any(windows, test))]
impl Config {
    pub fn remove_empty_rows(&mut self) {
        self.commands.retain(|cmd| !cmd.keys.trim().is_empty() || !cmd.text.trim().is_empty());
    }

    fn bindings(&self) -> Result<Vec<(u32, u32)>, String> {
        use crate::i18n::{t, tf};
        let mut found = std::collections::HashSet::new();
        self.commands
            .iter()
            .enumerate()
            .map(|(index, cmd)| {
                let error = |message| tf("第 {} 条：{}", &[&(index + 1), &message]);
                if cmd.keys.trim().is_empty() {
                    return Err(error(t("请先设置快捷键").to_owned()));
                }
                let key = binding(&cmd.keys).ok_or_else(|| error(tf("快捷键格式无效：{}", &[&cmd.keys])))?;
                if !found.insert(key) {
                    return Err(error(tf("快捷键重复：{}", &[&cmd.keys])));
                }
                if cmd.text.trim().is_empty() || cmd.text.chars().any(char::is_control) || cmd.text.len() > 8192 {
                    return Err(error(t("命令须为非空的单行文本，最多 8192 字节").into()));
                }
                Ok(key)
            })
            .collect()
    }
}

/// Capture the same keys accepted by the Windows registration parser.
#[cfg(any(windows, test))]
pub fn recorded_key(key: eframe::egui::Key, modifiers: eframe::egui::Modifiers) -> Option<String> {
    let mut chord = String::new();
    if modifiers.ctrl {
        chord.push_str("Ctrl+");
    }
    if modifiers.shift {
        chord.push_str("Shift+");
    }
    if modifiers.alt {
        chord.push_str("Alt+");
    }
    chord.push_str(key.name());
    binding(&chord).map(|_| chord)
}

#[cfg(any(windows, test))]
fn binding(keys: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in keys.split('+') {
        let s = part.trim().to_ascii_uppercase();
        let modifier = match s.as_str() {
            "ALT" => 1,
            "CTRL" | "CONTROL" => 2,
            "SHIFT" => 4,
            "WIN" => 8,
            _ => {
                if key.is_some() {
                    return None;
                }
                key = Some(if s.len() == 1 && s.as_bytes()[0].is_ascii_alphanumeric() {
                    s.as_bytes()[0] as u32
                } else {
                    match s.as_str() {
                        "BACKSPACE" => 0x08,
                        "TAB" => 0x09,
                        "ENTER" => 0x0d,
                        "ESCAPE" => 0x1b,
                        "SPACE" => 0x20,
                        "PAGEUP" => 0x21,
                        "PAGEDOWN" => 0x22,
                        "END" => 0x23,
                        "HOME" => 0x24,
                        "LEFT" => 0x25,
                        "UP" => 0x26,
                        "RIGHT" => 0x27,
                        "DOWN" => 0x28,
                        "INSERT" => 0x2d,
                        "DELETE" => 0x2e,
                        _ => 0x6f + s.strip_prefix('F')?.parse::<u32>().ok().filter(|n| (1..=24).contains(n))?,
                    }
                });
                continue;
            }
        };
        if mods & modifier != 0 {
            return None;
        }
        mods |= modifier;
    }
    let key = key?;
    (mods & (1 | 2 | 8) != 0 || key >= 0x70).then_some((mods, key))
}

#[cfg(windows)]
pub mod win {
    use super::*;
    use crate::i18n::{t, tf};
    use std::{
        cell::Cell,
        io,
        ptr::null_mut,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateMutexW, GetCurrentThreadId},
        },
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };

    const PASTE: u32 = WM_APP + 79;
    thread_local! { static BUTTON: Cell<Option<HWND>> = const { Cell::new(None) }; }

    unsafe extern "system" fn mouse(code: i32, msg: WPARAM, data: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && matches!(msg as u32, WM_RBUTTONDOWN | WM_RBUTTONUP) {
            let event = &*(data as *const MSLLHOOKSTRUCT);
            if event.flags & LLMHF_INJECTED == 0 {
                if msg as u32 == WM_RBUTTONUP {
                    if let Some(target) = BUTTON.take() {
                        if target == GetForegroundWindow() && GetAncestor(WindowFromPoint(event.pt), GA_ROOT) == target {
                            PostThreadMessageW(GetCurrentThreadId(), PASTE, target as usize, 0);
                        }
                        return 1;
                    }
                } else if !modifiers_down() {
                    let target = GetForegroundWindow();
                    if !target.is_null() && GetAncestor(WindowFromPoint(event.pt), GA_ROOT) == target {
                        BUTTON.set(Some(target));
                        return 1;
                    }
                }
            }
        }
        CallNextHookEx(null_mut(), code, msg, data)
    }

    unsafe fn modifiers_down() -> bool {
        [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN].iter().any(|k| GetAsyncKeyState(*k as i32) < 0)
    }

    fn key(buf: &mut Vec<INPUT>, vk: u16, scan: u16, flags: u32) {
        buf.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        });
    }

    fn deliver(command: Option<&Command>) -> io::Result<()> {
        let mut inputs = Vec::new();
        if let Some(cmd) = command {
            for unit in cmd.text.encode_utf16() {
                key(&mut inputs, 0, unit, KEYEVENTF_UNICODE);
                key(&mut inputs, 0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
            }
            if cmd.enter {
                key(&mut inputs, VK_RETURN, 0, 0);
                key(&mut inputs, VK_RETURN, 0, KEYEVENTF_KEYUP);
            }
        } else {
            key(&mut inputs, VK_CONTROL, 0, 0);
            key(&mut inputs, b'V' as u16, 0, 0);
            key(&mut inputs, b'V' as u16, 0, KEYEVENTF_KEYUP);
            key(&mut inputs, VK_CONTROL, 0, KEYEVENTF_KEYUP);
        }
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[derive(Default)]
    struct Registrations {
        keys: Vec<i32>,
        hook: HHOOK,
        mutex: HANDLE,
    }

    impl Drop for Registrations {
        fn drop(&mut self) {
            unsafe {
                if !self.hook.is_null() {
                    UnhookWindowsHookEx(self.hook);
                }
                if !self.mutex.is_null() {
                    CloseHandle(self.mutex);
                }
                for id in self.keys.drain(..) {
                    UnregisterHotKey(null_mut(), id);
                }
            }
        }
    }

    impl Registrations {
        unsafe fn open(config: &Config, bindings: &[(u32, u32)]) -> Result<Self, String> {
            let mut set = Self::default();
            for (i, &(mods, k)) in bindings.iter().enumerate() {
                let id = i as i32 + 1;
                if RegisterHotKey(null_mut(), id, mods | MOD_NOREPEAT, k) == 0 {
                    return Err(tf("无法注册快捷键 {}（系统或其他程序可能已占用）", &[&config.commands[i].keys]));
                }
                set.keys.push(id);
            }
            if config.right_paste {
                let name: Vec<_> = "Local\\QCCOM.RightPaste.1\0".encode_utf16().collect();
                set.mutex = CreateMutexW(null_mut(), 0, name.as_ptr());
                if set.mutex.is_null() {
                    return Err(io::Error::last_os_error().to_string());
                }
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    return Err(t("另一个 QCCOM 实例正在使用右键粘贴").into());
                }
                set.hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), GetModuleHandleW(null_mut()), 0);
                if set.hook.is_null() {
                    return Err(io::Error::last_os_error().to_string());
                }
            }
            Ok(set)
        }
    }

    struct Worker {
        id: u32,
        handle: thread::JoinHandle<()>,
    }
    impl Worker {
        fn start(config: Config, bindings: Vec<(u32, u32)>, ctx: eframe::egui::Context, errors: mpsc::Sender<String>) -> Result<Self, String> {
            let (tx, rx) = mpsc::sync_channel(1);
            let handle = thread::Builder::new()
                .name("qccom-desktop-keys".into())
                .spawn(move || unsafe {
                    let mut msg = MSG::default();
                    PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
                    let _registrations = match Registrations::open(&config, &bindings) {
                        Ok(set) => set,
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    };
                    if tx.send(Ok(GetCurrentThreadId())).is_err() {
                        return;
                    }
                    let mut pending: Option<(usize, HWND, Instant)> = None;
                    let mut timer = 0;
                    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                        match msg.message {
                            WM_HOTKEY => {
                                let id = msg.wParam;
                                if id == 0 || id > config.commands.len() {
                                    continue;
                                }
                                pending = Some((id - 1, GetForegroundWindow(), Instant::now()));
                                if timer == 0 {
                                    timer = SetTimer(null_mut(), 0, 15, None);
                                }
                            }
                            WM_TIMER if timer != 0 && msg.wParam == timer => {
                                if let Some((index, window, start)) = pending {
                                    if start.elapsed() < Duration::from_secs(2) && window == GetForegroundWindow() && !window.is_null() {
                                        if modifiers_down() {
                                            continue;
                                        }
                                        if deliver(config.commands.get(index)).is_err() {
                                            let _ = errors.send(t("目标窗口未接受模拟输入").into());
                                            ctx.request_repaint();
                                        }
                                    }
                                }
                                KillTimer(null_mut(), timer);
                                timer = 0;
                                pending = None;
                            }
                            PASTE if msg.wParam != 0 && GetForegroundWindow() as usize == msg.wParam && !modifiers_down() => {
                                if deliver(None).is_err() {
                                    let _ = errors.send(t("目标窗口未接受模拟输入").into());
                                    ctx.request_repaint();
                                }
                            }
                            _ => {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            let id = rx.recv().map_err(|e| e.to_string())??;
            Ok(Self { id, handle })
        }
    }

    pub struct Manager {
        worker: Option<Worker>,
        pub errors: mpsc::Receiver<String>,
        tx: mpsc::Sender<String>,
    }
    impl Default for Manager {
        fn default() -> Self {
            let (tx, errors) = mpsc::channel();
            Self { worker: None, errors, tx }
        }
    }
    impl Manager {
        pub fn suspend(&mut self) {
            if let Some(w) = self.worker.take() {
                unsafe {
                    PostThreadMessageW(w.id, WM_QUIT, 0, 0);
                }
                let _ = w.handle.join();
            }
        }
        pub fn set(&mut self, cfg: &Config, ctx: &eframe::egui::Context) -> Result<(), String> {
            let mut cfg = cfg.clone();
            cfg.remove_empty_rows();
            let keys = cfg.bindings()?;
            self.suspend();
            if !keys.is_empty() || cfg.right_paste {
                self.worker = Some(Worker::start(cfg, keys, ctx.clone(), self.tx.clone())?);
            }
            Ok(())
        }
    }
    impl Drop for Manager {
        fn drop(&mut self) {
            self.suspend();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{binding, recorded_key, Command, Config};
    use eframe::egui::{Key, Modifiers};

    #[test]
    fn recording_produces_a_chord_windows_can_register() {
        assert_eq!(recorded_key(Key::J, Modifiers::CTRL | Modifiers::ALT).as_deref(), Some("Ctrl+Alt+J"));
        assert_eq!(recorded_key(Key::Enter, Modifiers::CTRL).as_deref(), Some("Ctrl+Enter"));
        assert!(recorded_key(Key::J, Modifiers::NONE).is_none());
    }

    #[test]
    fn unused_rows_do_not_block_apply_but_partial_rows_and_conflicts_do() {
        let good = Command {
            keys: "Ctrl+Alt+J".into(),
            text: "echo QCCOM_TEST".into(),
            enter: false,
        };
        let mut config = Config {
            right_paste: true,
            commands: vec![Command::default(), good.clone(), Command::default()],
        };
        config.remove_empty_rows();
        assert_eq!(config.commands.len(), 1);
        assert_eq!(config.bindings().unwrap(), vec![(3, 74)]);
        config.commands.push(Command {
            text: "second".into(),
            ..Default::default()
        });
        config.remove_empty_rows();
        assert!(config.bindings().is_err());
        config.commands[1].keys = "alt+ctrl+j".into();
        assert!(config.bindings().is_err());
    }
    #[test]
    fn desktop_chords_are_unambiguous() {
        assert_eq!(binding("Alt+Ctrl+j"), Some((3, 74)));
        assert_eq!(binding("F8"), Some((0, 0x77)));
        for s in ["Ctrl+", "a", "Shift+A", "Ctrl+Ctrl+K", "Ctrl+X+K", "F25"] {
            assert_eq!(binding(s), None);
        }
    }
}

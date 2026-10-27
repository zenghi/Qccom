//! 内置命令行终端：底部面板，多个标签，每个标签是一个真正的 shell（Windows 用 ConPTY，
//! Linux 用 pty）。cmd / PowerShell / Git Bash / bash / zsh … 自动检测。
//! 屏幕和按键处理复用 TermView，所以 vi、top、颜色、回滚都和串口终端一样。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use eframe::egui::{self, pos2, vec2, Align, Align2, FontId, Layout, Rect, RichText, Sense};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

use crate::app::{draw_icon, icon_button, Icon};
use crate::i18n::{t, tf};
use crate::termview::TermView;
use crate::theme::*;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellKind {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

/// 这台电脑上能用的 shell，第一个是默认
pub fn detect() -> Vec<ShellKind> {
    let k = |name: &str, program: &str, args: &[&str]| ShellKind {
        name: name.into(),
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
    };
    let mut v = vec![];
    if cfg!(windows) {
        if which("pwsh.exe").is_some() {
            v.push(k("PowerShell 7", "pwsh.exe", &["-NoLogo"]));
        }
        v.push(k("PowerShell", "powershell.exe", &["-NoLogo"]));
        v.push(k("cmd", "cmd.exe", &[]));
        let git_bash = ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files (x86)\\Git\\bin\\bash.exe"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .or_else(|| which("git.exe").and_then(|g| g.parent()?.parent().map(|r| r.join("bin").join("bash.exe"))).filter(|p| p.is_file()));
        if let Some(p) = git_bash {
            v.push(k("Git Bash", &p.to_string_lossy(), &["--login", "-i"]));
        }
        if which("wsl.exe").is_some() {
            v.push(k("WSL", "wsl.exe", &[]));
        }
    } else {
        if let Ok(sh) = std::env::var("SHELL") {
            let name = Path::new(&sh).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| sh.clone());
            v.push(k(&name, &sh, &[]));
        }
        for sh in ["bash", "zsh", "fish", "sh"] {
            if let Some(p) = which(sh) {
                if !v.iter().any(|x| Path::new(&x.program).file_name() == p.file_name()) {
                    v.push(k(sh, &p.to_string_lossy(), &[]));
                }
            }
        }
    }
    v
}

fn which(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}

pub struct Shell {
    pub title: String,
    pub tv: TermView,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    pub exited: Option<u32>,
    pub cwd: PathBuf,
}

impl Shell {
    pub fn spawn(ctx: &egui::Context, kind: &ShellKind, cwd: &Path, n: usize) -> Result<Shell, String> {
        let (cols, rows) = (100u16, 24u16);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(&kind.program);
        cmd.args(&kind.args);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("shell-read".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                            ctx.request_repaint();
                        }
                    }
                }
                ctx.request_repaint();
            })
            .map_err(|e| e.to_string())?;
        let mut tv = TermView::new(("shell", n), cols as usize, rows as usize);
        tv.focus_req = true;
        Ok(Shell {
            title: kind.name.clone(),
            tv,
            master: pair.master,
            writer,
            child,
            rx,
            exited: None,
            cwd: cwd.to_path_buf(),
        })
    }

    fn poll(&mut self) {
        while let Ok(d) = self.rx.try_recv() {
            let reply = self.tv.feed(&d);
            if !reply.is_empty() {
                self.write(&reply);
            }
        }
        if self.exited.is_none() {
            if let Ok(Some(st)) = self.child.try_wait() {
                self.exited = Some(st.exit_code());
                // 把剩下的输出读完再提示
                while let Ok(d) = self.rx.try_recv() {
                    self.tv.feed(&d);
                }
                let msg = format!("\r\n\x1b[2m[{}]\x1b[0m\r\n", tf("进程已退出，代码 {}。按回车重新启动", &[&st.exit_code()]));
                self.tv.feed(msg.as_bytes());
            }
        }
    }

    fn write(&mut self, d: &[u8]) {
        let _ = self.writer.write_all(d);
        let _ = self.writer.flush();
    }

    fn sync_size(&mut self) {
        let s = &self.tv.term.s;
        let want = (s.cols as u16, s.rows as u16);
        if let Ok(cur) = self.master.get_size() {
            if (cur.cols, cur.rows) != want {
                let _ = self.master.resize(PtySize {
                    rows: want.1,
                    cols: want.0,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if self.exited.is_none() {
            let _ = self.child.kill();
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct ShellSettings {
    pub open: bool,
    pub height: f32,
    /// 默认 shell 的名字（空 = 检测到的第一个）
    pub default: String,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            open: false,
            height: 260.0,
            default: String::new(),
        }
    }
}

pub enum ShellEvt {
    Toast(&'static str),
    /// Ctrl+点击了 `文件:行号`
    Open(PathBuf, usize),
}

pub struct Shells {
    pub s: ShellSettings,
    pub tabs: Vec<Shell>,
    pub active: usize,
    kinds: Option<Vec<ShellKind>>,
    next_id: usize,
    pub events: Vec<ShellEvt>,
    maximized: bool,
}

impl Shells {
    pub fn new(s: ShellSettings) -> Self {
        Self {
            s,
            tabs: vec![],
            active: 0,
            kinds: None,
            next_id: 0,
            events: vec![],
            maximized: false,
        }
    }

    pub fn kinds(&mut self) -> &[ShellKind] {
        self.kinds.get_or_insert_with(detect)
    }

    pub fn focused(&self, ctx: &egui::Context) -> bool {
        self.s.open && self.tabs.get(self.active).is_some_and(|sh| ctx.memory(|m| m.has_focus(sh.tv.id())))
    }

    /// 打开 / 关闭面板；第一次打开时自动开一个默认 shell
    pub fn toggle(&mut self, ctx: &egui::Context, cwd: &Path) {
        if self.s.open && !self.focused(ctx) && !self.tabs.is_empty() {
            // 面板开着但焦点不在终端：先把焦点给它，再按一次才关闭
            self.tabs[self.active].tv.focus_req = true;
            return;
        }
        self.s.open = !self.s.open;
        if self.s.open {
            if self.tabs.is_empty() {
                self.new_shell(ctx, None, cwd);
            } else {
                self.tabs[self.active].tv.focus_req = true;
            }
        }
    }

    pub fn new_shell(&mut self, ctx: &egui::Context, kind: Option<ShellKind>, cwd: &Path) {
        let default = self.s.default.clone();
        let kinds = self.kinds();
        let Some(kind) = kind.or_else(|| kinds.iter().find(|k| k.name == default).or(kinds.first()).cloned()) else {
            self.events.push(ShellEvt::Toast(t("没有找到可用的 shell")));
            return;
        };
        self.next_id += 1;
        match Shell::spawn(ctx, &kind, cwd, self.next_id) {
            Ok(sh) => {
                self.tabs.push(sh);
                self.active = self.tabs.len() - 1;
                self.s.open = true;
            }
            Err(e) => {
                let msg: &'static str = Box::leak(tf("启动 {} 失败：{}", &[&kind.name, &e]).into_boxed_str());
                self.events.push(ShellEvt::Toast(msg));
            }
        }
    }

    fn close(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            if self.tabs.is_empty() {
                self.s.open = false;
            }
        }
    }

    /// 每帧调用：读 shell 输出（面板收起时也要读，避免缓冲区堆满）
    pub fn poll(&mut self) {
        for sh in &mut self.tabs {
            sh.poll();
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, cwd: &Path) {
        if !self.s.open {
            return;
        }
        let avail = ctx.available_rect().height();
        let limit = (avail - 120.0).max(120.0);
        let want = if self.maximized { limit } else { self.s.height.clamp(120.0, limit) };
        let dragging = ctx.input(|i| i.pointer.any_down());
        let r = egui::TopBottomPanel::bottom("shell")
            .frame(egui::Frame::new().fill(BG0()))
            .default_height(want)
            .height_range(if dragging && !self.maximized { 120.0 } else { want }..=limit)
            .resizable(!self.maximized)
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.set_min_height(ui.available_height());
                self.header(ui, cwd);
                let mut restart = false;
                if let Some(sh) = self.tabs.get_mut(self.active) {
                    let rect = ui.available_rect_before_wrap().shrink2(vec2(6.0, 4.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    let o = sh.tv.show(&mut child, &|s: &str| s.as_bytes().to_vec(), None, true);
                    sh.sync_size();
                    if sh.exited.is_some() {
                        // 进程退出后按回车重新启动同一个 shell
                        restart = o.input.contains(&b'\r');
                    } else if !o.input.is_empty() {
                        sh.write(&o.input);
                    }
                    if let Some(m) = o.toast {
                        self.events.push(ShellEvt::Toast(m));
                    }
                    if let Some((f, line)) = o.open {
                        let base = self.tabs.get(self.active).map(|s| s.cwd.clone()).unwrap_or_default();
                        let p = if Path::new(&f).is_absolute() { PathBuf::from(&f) } else { base.join(&f) };
                        if p.is_file() {
                            self.events.push(ShellEvt::Open(p, line.saturating_sub(1)));
                        } else {
                            self.events.push(ShellEvt::Toast(t("找不到这个文件")));
                        }
                    }
                }
                if restart {
                    let (title, cwd) = (self.tabs[self.active].title.clone(), self.tabs[self.active].cwd.clone());
                    if let Some(kind) = self.kinds().iter().find(|k| k.name == title).cloned() {
                        self.next_id += 1;
                        match Shell::spawn(ui.ctx(), &kind, &cwd, self.next_id) {
                            Ok(new) => self.tabs[self.active] = new,
                            Err(_) => self.events.push(ShellEvt::Toast(t("启动失败"))),
                        }
                    }
                }
            });
        if dragging && !self.maximized && (r.response.rect.height() - want).abs() > 0.5 {
            self.s.height = r.response.rect.height();
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, cwd: &Path) {
        let ctx = ui.ctx().clone();
        let mut close = None;
        let mut pick = None;
        let mut new_kind: Option<Option<ShellKind>> = None;
        ui.horizontal(|ui| {
            ui.set_height(30.0);
            ui.add_space(10.0);
            draw_icon(
                ui.painter(),
                Rect::from_center_size(pos2(ui.cursor().left() + 7.0, ui.max_rect().center().y), vec2(14.0, 14.0)),
                Icon::Terminal,
                TEXT3(),
            );
            ui.add_space(20.0);
            ui.label(RichText::new(t("终端")).size(12.0).color(TEXT2()).strong());
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            for (i, sh) in self.tabs.iter().enumerate() {
                let on = i == self.active;
                let name = if sh.exited.is_some() { format!("{} ✕", sh.title) } else { sh.title.clone() };
                let font = FontId::proportional(12.0);
                let w = ui.fonts(|f| f.layout_no_wrap(name.clone(), font.clone(), TEXT()).size().x) + 40.0;
                let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
                let p = ui.painter();
                if on {
                    p.rect_filled(r, R_WIDGET, BG2());
                    p.hline(r.x_range().shrink(6.0), r.bottom() - 0.5, st(1.5, ACCENT()));
                } else if resp.hovered() {
                    p.rect_filled(r, R_WIDGET, tint(6));
                }
                p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, &name, font, if on { TEXT() } else { TEXT3() });
                let xr = Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(14.0, 14.0));
                let hover_x = resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| xr.contains(p));
                if on || resp.hovered() {
                    draw_icon(p, xr.shrink(2.0), Icon::Close, if hover_x { TEXT() } else { TEXT3() });
                }
                if resp.clicked() {
                    if hover_x {
                        close = Some(i);
                    } else {
                        pick = Some(i);
                    }
                }
                if resp.middle_clicked() {
                    close = Some(i);
                }
            }
            ui.add_space(4.0);
            if icon_button(ui, Icon::Plus, t("新建终端"), false).clicked() {
                new_kind = Some(None);
            }
            // 下拉箭头：选择 shell 类型
            let (ar, aresp) = ui.allocate_exact_size(vec2(16.0, 24.0), Sense::click());
            let c = if aresp.hovered() { TEXT() } else { TEXT3() };
            let m = ar.center();
            ui.painter().add(egui::Shape::line(vec![m + vec2(-3.5, -1.5), m + vec2(0.0, 2.0), m + vec2(3.5, -1.5)], st(1.3, c)));
            let pop = ui.id().with("shell-kinds");
            let aresp = aresp.on_hover_text(t("选择 shell"));
            if aresp.clicked() {
                ui.memory_mut(|m| m.toggle_popup(pop));
            }
            let kinds = self.kinds().to_vec();
            let default = self.s.default.clone();
            egui::popup::popup_below_widget(ui, pop, &aresp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(200.0);
                for (i, k) in kinds.iter().enumerate() {
                    let is_def = if default.is_empty() { i == 0 } else { k.name == default };
                    let label = if is_def { format!("{}  ·  {}", k.name, t("默认")) } else { k.name.clone() };
                    if ui.button(label).clicked() {
                        new_kind = Some(Some(k.clone()));
                    }
                }
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if icon_button(ui, Icon::Close, t("隐藏终端面板"), false).clicked() {
                    self.s.open = false;
                }
                let (ic, tip) = if self.maximized {
                    (Icon::Restore, t("还原面板大小"))
                } else {
                    (Icon::Maximize, t("最大化面板"))
                };
                if icon_button(ui, ic, tip, false).clicked() {
                    self.maximized = !self.maximized;
                }
                if !self.tabs.is_empty() {
                    if icon_button(ui, Icon::Trash, t("清屏"), false).clicked() {
                        let sh = &mut self.tabs[self.active];
                        sh.tv.reset();
                        sh.write(b"\x0c");
                        sh.tv.focus_req = true;
                    }
                    if let Some(sh) = self.tabs.get(self.active) {
                        let d = sh.cwd.to_string_lossy().to_string();
                        ui.label(RichText::new(d).size(11.0).color(TEXT3()));
                    }
                }
            });
        });
        if let Some(i) = pick {
            self.active = i;
            self.tabs[i].tv.focus_req = true;
        }
        if let Some(i) = close {
            self.close(i);
        }
        if let Some(k) = new_kind {
            self.new_shell(&ctx, k, cwd);
        }
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, st(1.0, LINE()));
    }
}

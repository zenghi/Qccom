//! 内置命令行终端：底部面板，多个标签，每个标签是一个真正的 shell（Windows 用 ConPTY，
//! Linux 用 pty）。cmd / PowerShell / Git Bash / bash / zsh … 自动检测。
//! 屏幕和按键处理复用 TermView，所以 vi、top、颜色、回滚都和串口终端一样。

mod environment;
mod mcu;
use environment::{EnvDialog, Environment};
use mcu::{Action, Syntax};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

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

/// 终端按 UTF-8 显示，shell 和里面的程序也要输出 UTF-8，否则 less（git log 的分页器）
/// 会把中文显示成 <E4><B8><AD>，git 会把中文文件名转义成 \346\226\207
fn utf8_env(cmd: &mut CommandBuilder) {
    let utf8 = |k: &str| std::env::var(k).map(|v| v.to_ascii_uppercase().replace('-', "").contains("UTF8")).unwrap_or(false);
    // LC_ALL 优先级最高，设了但不是 UTF-8 时要盖掉
    if std::env::var_os("LC_ALL").is_some() && !utf8("LC_ALL") {
        cmd.env("LC_ALL", "C.UTF-8");
    }
    if !utf8("LANG") {
        // Git Bash（MSYS）和大多数 Linux 发行版都认 C.UTF-8；中文环境用 zh_CN.UTF-8 让程序输出中文提示
        let lang = match crate::i18n::current() {
            0 if cfg!(windows) => "zh_CN.UTF-8",
            2 if cfg!(windows) => "zh_TW.UTF-8",
            _ => "C.UTF-8",
        };
        cmd.env("LANG", lang);
    }
    if std::env::var_os("LC_CTYPE").is_some() && !utf8("LC_CTYPE") {
        cmd.env("LC_CTYPE", "C.UTF-8");
    }
    cmd.env("LESSCHARSET", "utf-8");
    // git 的中文文件名不转义（不改用户的 git 配置，只对这个终端生效；git 2.31 起支持）
    if std::env::var_os("GIT_CONFIG_COUNT").is_none() {
        cmd.env("GIT_CONFIG_COUNT", "1");
        cmd.env("GIT_CONFIG_KEY_0", "core.quotepath");
        cmd.env("GIT_CONFIG_VALUE_0", "false");
    }
}

/// Windows 的 cmd / PowerShell 默认用系统代码页（中文系统是 936），先切到 UTF-8（65001）
fn utf8_args(cmd: &mut CommandBuilder, program: &str) {
    let exe = Path::new(program).file_name().map(|f| f.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match exe.as_str() {
        "cmd.exe" | "cmd" => {
            cmd.args(["/K", "chcp 65001 >nul"]);
        }
        "powershell.exe" | "pwsh.exe" | "powershell" | "pwsh" => {
            cmd.args([
                "-NoExit",
                "-Command",
                "chcp 65001 > $null; [Console]::InputEncoding = [Console]::OutputEncoding = $OutputEncoding = New-Object System.Text.UTF8Encoding $false",
            ]);
        }
        _ => {}
    }
}

pub fn which(exe: &str) -> Option<PathBuf> {
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
    /// SSH 会话：退出提示改成「重新连接」
    pub remote: bool,
    /// 标签颜色（TAB_COLORS 的下标，0 = 不着色）
    pub color: usize,
    /// 后台标签里的长命令跑完了，标签上亮一个点
    pub attn: bool,
    /// 启动用的 shell 类型名（标签可以改名，重启时按这个找）
    kind: String,
    environment: Option<Environment>,
    syntax: Option<Syntax>,
    mcu: Option<Arc<mcu::Session>>,
    pending_write: Option<(Vec<u8>, Vec<Receiver<()>>)>,
    pending_shortcut: Option<PendingShortcut>,
    discovery: Option<Discovery>,
    used: bool,
    /// 编号（标签会重排，用它认出是哪一个）
    pub id: usize,
}

impl Shell {
    fn at_prompt(&self) -> bool {
        let line = self.tv.cursor_line();
        self.exited.is_none() && self.pending_write.is_none() && self.pending_shortcut.is_none() && !line.is_empty() && prompt_of(&line).trim_end() == line
    }

    pub fn spawn(ctx: &egui::Context, kind: &ShellKind, cwd: &Path, n: usize) -> Result<Shell, String> {
        Self::spawn_with_env(ctx, kind, cwd, n, None, None)
    }

    fn spawn_with_env(ctx: &egui::Context, kind: &ShellKind, cwd: &Path, n: usize, env: Option<&Environment>, mcu: Option<Arc<mcu::Session>>) -> Result<Shell, String> {
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
        let syntax = Syntax::for_shell(kind);
        if !mcu.as_ref().zip(syntax).is_some_and(|(m, syntax)| m.starts_shell(syntax)) {
            utf8_args(&mut cmd, &kind.program);
        }
        let cwd = mcu.as_ref().map_or(cwd, |session| session.root.as_path());
        cmd.cwd(cwd);
        if cfg!(windows) && kind.name == "Git Bash" {
            // Git for Windows' login profile otherwise changes to $HOME.
            cmd.env("CHERE_INVOKING", "1");
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        utf8_env(&mut cmd);
        if let Some(env) = env {
            env.apply(&mut cmd)?;
        }
        if let Some((mcu, syntax)) = mcu.as_ref().zip(syntax) {
            mcu.apply(&mut cmd, syntax);
        }
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
        tv.bg = crate::bgimg::Slot::Shell;
        tv.blocks = true;
        Ok(Shell {
            title: kind.name.clone(),
            tv,
            master: pair.master,
            writer,
            child,
            rx,
            exited: None,
            cwd: cwd.to_path_buf(),
            remote: false,
            color: 0,
            attn: false,
            kind: kind.name.clone(),
            environment: env.cloned(),
            syntax,
            mcu,
            pending_write: None,
            pending_shortcut: None,
            discovery: None,
            used: false,
            id: n,
        })
    }

    pub fn poll(&mut self) {
        while let Ok(d) = self.rx.try_recv() {
            let reply = self.tv.feed(&d);
            if !reply.is_empty() {
                let _ = self.writer.write_all(&reply);
                let _ = self.writer.flush();
            }
        }
        if self.exited.is_none() {
            if let Ok(Some(st)) = self.child.try_wait() {
                self.exited = Some(st.exit_code());
                // 把剩下的输出读完再提示
                while let Ok(d) = self.rx.try_recv() {
                    self.tv.feed(&d);
                }
                let msg = if self.remote {
                    tf("连接已断开（代码 {}）。按回车重新连接", &[&st.exit_code()])
                } else {
                    tf("进程已退出，代码 {}。按回车重新启动", &[&st.exit_code()])
                };
                let msg = format!("\r\n\x1b[2m[{msg}]\x1b[0m\r\n");
                self.tv.feed(msg.as_bytes());
            }
        }
        if let Some((data, mut releases)) = self.pending_write.take().filter(|_| self.exited.is_none()) {
            releases.retain(|rx| matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
            if releases.is_empty() {
                self.write(&data);
            } else {
                self.pending_write = Some((data, releases));
            }
        }
    }

    pub fn write(&mut self, d: &[u8]) {
        self.used = true;
        let _ = self.writer.write_all(d);
        let _ = self.writer.flush();
    }

    fn command_event(&self, line: String, data: Vec<u8>, monitor: Option<String>) -> ShellEvt {
        ShellEvt::Command(self.id, line, self.tv.cursor_row(), data, monitor)
    }

    fn serial_shortcut(&mut self, line: &str, port: &str) -> Option<Option<ShellEvt>> {
        let prompt = prompt_of(line);
        if prompt.is_empty() {
            return None;
        }
        let (mcu, syntax) = self.mcu.as_ref().zip(self.syntax)?;
        let shortcut = mcu.shortcut(line.strip_prefix(prompt.as_str())?)?;
        let monitor = shortcut.action.monitor_after().then(|| port.to_owned());
        let result = mcu.shortcut_command(syntax, &shortcut, port);
        self.write(b"\x03"); // Clear the typed line before dispatching the application action.
        Some(match result {
            Ok(Some(task)) => {
                self.pending_shortcut = Some(PendingShortcut {
                    task,
                    prompt,
                    monitor,
                    at: std::time::Instant::now(),
                });
                None
            }
            Ok(None) => Some(ShellEvt::MonitorSerial),
            Err(e) => Some(ShellEvt::Toast(e)),
        })
    }

    pub fn sync_size(&mut self) {
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
    /// 面板头部的任务按钮（编译、烧录、清理…）
    pub tasks: Vec<Task>,
    pub environments: Vec<Environment>,
    pub environment: String,
    mcu: BTreeMap<PathBuf, mcu::Options>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Task {
    pub name: String,
    pub cmd: String,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            open: false,
            height: 260.0,
            default: String::new(),
            environments: vec![],
            environment: String::new(),
            mcu: BTreeMap::new(),
            tasks: DEFAULT_TASKS.iter().map(|(n, c)| Task { name: (*n).into(), cmd: (*c).into() }).collect(),
        }
    }
}

const DEFAULT_TASKS: &[(&str, &str)] = &[("编译", "make -j8"), ("烧录", "make flash"), ("清理", "make clean")];

fn default_task_label(task: &Task) -> &'static str {
    match task.name.as_str() {
        "编译" => t("编译"),
        "烧录" => t("烧录"),
        _ => t("清理"),
    }
}

fn primary_action(action: Action) -> bool {
    matches!(action, Action::Build | Action::Flash | Action::Monitor | Action::Clean)
}

/// 编译输出里的一条错误 / 警告
#[derive(Clone, PartialEq, Debug)]
pub struct Problem {
    pub file: String,
    pub line: usize,
    pub error: bool,
    pub msg: String,
}

/// 从编译输出里找错误和警告：GCC / Clang / armclang、Keil armcc、IAR、MSVC
/// 命令里有没有用到这个串口（COM3 不算 COM31；/dev/ttyUSB0 也认 ttyUSB0）
pub fn uses_port(cmd: &str, port: &str) -> bool {
    if port.is_empty() {
        return false;
    }
    let cmd = cmd.to_lowercase();
    let mut names = vec![port.to_lowercase()];
    if let Some(base) = port.rsplit(['/', '\\']).next().filter(|b| b.len() < port.len() && b.len() >= 3) {
        names.push(base.to_lowercase());
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    names.iter().any(|n| {
        cmd.match_indices(n.as_str()).any(|(i, _)| {
            let before = cmd[..i].chars().next_back();
            let after = cmd[i + n.len()..].chars().next();
            !word(before) && !word(after)
        })
    })
}

/// 回车时那一行里的提示符部分（到第一个 > $ # % 为止，再带上后面的空格）；认不出返回空
pub fn prompt_of(line: &str) -> String {
    let Some(i) = line.char_indices().take(160).find(|(_, c)| matches!(c, '>' | '$' | '#' | '%')).map(|(i, _)| i) else {
        return String::new();
    };
    let rest = &line[i + 1..];
    let sp = rest.len() - rest.trim_start_matches(' ').len();
    line[..i + 1 + sp].to_string()
}

pub fn parse_problems(text: &str) -> Vec<Problem> {
    use std::sync::OnceLock;
    static RE: OnceLock<[regex_lite::Regex; 3]> = OnceLock::new();
    let re = RE.get_or_init(|| {
        [
            // src/main.c:12:5: error: ...
            regex_lite::Regex::new(r"^(.+?):(\d+):(?:\d+:)?\s*(fatal error|error|warning):\s*(.*)$").unwrap(),
            // main.c(42): error: #20: ...   /  main.c(42,7): error C2065: ...
            regex_lite::Regex::new(r#"^"?(.+?)"?\((\d+)(?:,\d+)?\)\s*:\s*(fatal error|error|warning)\s*[A-Z]*\d*\s*:?\s*(.*)$"#).unwrap(),
            // "main.c",42  Error[Pe020]: ...
            regex_lite::Regex::new(r#"^"(.+?)",(\d+)\s+(Fatal error|Error|Warning)\[\w+\]:\s*(.*)$"#).unwrap(),
        ]
    });
    let mut out: Vec<Problem> = vec![];
    for l in text.lines() {
        let l = l.trim();
        for r in re.iter() {
            if let Some(c) = r.captures(l) {
                let file = c[1].trim().to_string();
                // 命令行回显（提示符 + 命令）里也会出现「文件:行」，文件名带空白的不算
                if file.contains(char::is_whitespace) {
                    break;
                }
                let line = c[2].parse().unwrap_or(0);
                let p = Problem {
                    file,
                    line,
                    error: !c[3].to_ascii_lowercase().starts_with("warning"),
                    msg: c[4].trim().to_string(),
                };
                if line > 0 && !out.contains(&p) {
                    out.push(p);
                }
                break;
            }
        }
    }
    out
}

/// 标签可选的颜色
pub const TAB_COLORS: [(u8, u8, u8); 7] = [
    (0, 0, 0),
    (0xf8, 0x71, 0x71),
    (0xfb, 0x92, 0x3c),
    (0xfa, 0xcc, 0x15),
    (0x4a, 0xde, 0x80),
    (0x60, 0xa5, 0xfa),
    (0xc0, 0x84, 0xfc),
];

fn tab_color(i: usize) -> Option<egui::Color32> {
    let (r, g, b) = *TAB_COLORS.get(i).filter(|_| i > 0)?;
    Some(egui::Color32::from_rgb(r, g, b))
}

pub enum ShellEvt {
    Toast(String),
    /// 后台的长命令跑完了：(标签名, 用时)
    Done(String, std::time::Duration),
    /// Ctrl+点击了 `文件:行号`
    Open(PathBuf, usize),
    /// 点了任务按钮：(任务名, 命令)
    Task(String, String),
    /// 在本机终端里执行了一条命令：(终端编号, 提示符 + 命令, 回车时光标所在行)
    Command(usize, String, u64, Vec<u8>, Option<String>),
    SelectSerial(String),
    ShowSerialDevices,
    MonitorSerial,
}

struct PendingShortcut {
    task: Task,
    prompt: String,
    monitor: Option<String>,
    at: std::time::Instant,
}

struct Discovery {
    kind: ShellKind,
    state: DiscoveryState,
    task: Option<Task>,
}

enum DiscoveryState {
    AwaitPaint(mcu::Options),
    Painted(mcu::Options, std::time::Instant),
    Scanning(Receiver<mcu::Session>),
}

struct PendingInstrument {
    root: PathBuf,
    id: usize,
    flash: bool,
    port: String,
    since: std::time::Instant,
}

pub struct Shells {
    pub s: ShellSettings,
    pub tabs: Vec<Shell>,
    pub active: usize,
    kinds: Option<Vec<ShellKind>>,
    next_id: usize,
    pub events: Vec<ShellEvt>,
    maximized: bool,
    /// 右半边显示的标签
    split: Option<usize>,
    rename: Option<(usize, String)>,
    project_shell_pending: bool,
    /// 当前标签的编译问题，以及从哪一行开始找（按任务按钮时记下）
    pub problems: Vec<Problem>,
    prob_from: Option<(usize, u64)>,
    prob_at: Option<std::time::Instant>,
    /// 面板在屏幕上的位置（拖文件进来时判断）
    pub rect: Option<Rect>,
    edit_tasks: bool,
    env_dialog: EnvDialog,
    instrument_pending: Option<PendingInstrument>,
}

impl Shells {
    pub fn instrument_context(&self) -> serde_json::Value {
        let mcu = self.tabs.get(self.active).and_then(|sh| sh.mcu.as_ref());
        serde_json::json!({"profile":mcu.map(|m|m.profile.status_label()),"sdk":mcu.and_then(|m|m.location.as_ref()),
            "firmware":mcu.map(|m|&m.options.firmware),"tools":mcu.map(|m|m.tool_paths()),"issues":self.problems.iter().take(50).map(|p|serde_json::json!({"file":p.file,"line":p.line,"error":p.error,"message":p.msg})).collect::<Vec<_>>()})
    }
    pub fn instrument_task(&mut self, ctx: &egui::Context, cwd: &Path, flash: bool, port: &str) -> Result<(), String> {
        if self.instrument_pending.is_some() {
            return Err(t("请等待当前工程的终端就绪").into());
        }
        if !self.tabs.get(self.active).is_some_and(|s| s.exited.is_none() && !s.remote && s.mcu.as_ref().map_or(s.cwd.as_path(), |m| m.workspace.as_path()) == cwd) {
            let id = self.next_id + 1;
            self.launch(ctx, None, cwd, None);
            if !self.tabs.iter().any(|s| s.id == id) {
                return Err(t("启动失败").into());
            }
            self.instrument_pending = Some(PendingInstrument {
                root: cwd.to_owned(),
                id,
                flash,
                port: port.into(),
                since: std::time::Instant::now(),
            });
            return Ok(());
        }
        if let Some(sh) = self.tabs.get(self.active).filter(|s| s.discovery.is_some()) {
            self.instrument_pending = Some(PendingInstrument {
                root: cwd.into(),
                id: sh.id,
                flash,
                port: port.into(),
                since: std::time::Instant::now(),
            });
            return Ok(());
        }
        self.submit_instrument(self.active, cwd, flash, port)
    }
    fn submit_instrument(&mut self, index: usize, cwd: &Path, flash: bool, port: &str) -> Result<(), String> {
        let sh = self.tabs.get(index).ok_or_else(|| t("请先打开工程的 MCU 终端").to_owned())?;
        let (m, syntax) = sh.mcu.as_ref().zip(sh.syntax).ok_or_else(|| t("请选择已识别工具链的 MCU 终端").to_owned())?;
        if m.workspace != cwd || sh.remote || !sh.at_prompt() {
            return Err(t("请等待当前工程的终端就绪").into());
        }
        let action = if flash { Action::Flash } else { Action::Build };
        let cmd = m.command(syntax, action, port)?;
        self.submit_task(index, Task { name: action.label().into(), cmd }, flash, (flash && !port.is_empty()).then(|| port.to_owned()));
        Ok(())
    }

    pub fn new(s: ShellSettings) -> Self {
        Self {
            // Layout restoration can reveal the panel after construction. The first
            // visible frame needs a fresh PTY even when the workspace is unchanged.
            project_shell_pending: true,
            s,
            tabs: vec![],
            active: 0,
            kinds: None,
            next_id: 0,
            events: vec![],
            maximized: false,
            split: None,
            rename: None,
            problems: vec![],
            prob_from: None,
            prob_at: None,
            rect: None,
            edit_tasks: false,
            env_dialog: Default::default(),
            instrument_pending: None,
        }
    }

    /// 拖文件到终端面板上：把路径插到命令行里（有空格的加引号）
    pub fn insert_paths(&mut self, paths: &[PathBuf]) {
        let Some(sh) = self.tabs.get_mut(self.active) else { return };
        let text: Vec<String> = paths
            .iter()
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                if s.contains(' ') {
                    format!("\"{s}\"")
                } else {
                    s
                }
            })
            .collect();
        sh.write(format!("{} ", text.join(" ")).as_bytes());
        sh.tv.focus_req = true;
    }

    /// 运行任务按钮：在当前终端（没有就新开一个）里输入命令
    fn run_task(&mut self, ctx: &egui::Context, cwd: &Path, task: Task, port: &str) {
        if let Some((mcu, syntax)) = self.tabs.get(self.active).and_then(|sh| sh.mcu.as_ref().zip(sh.syntax)) {
            if let Some(shortcut) = mcu.shortcut(&task.cmd) {
                match mcu.shortcut_command(syntax, &shortcut, port) {
                    Ok(Some(command)) => self.submit_task(self.active, command, mcu.workspace == cwd, shortcut.action.monitor_after().then(|| port.to_owned())),
                    Ok(None) => self.events.push(ShellEvt::MonitorSerial),
                    Err(e) => self.events.push(ShellEvt::Toast(e)),
                }
                return;
            }
        }
        if self.tabs.get(self.active).is_none_or(|s| s.exited.is_some()) {
            self.launch(ctx, None, cwd, Some(task));
        } else {
            self.submit_task(self.active, task, true, None);
        }
    }

    fn submit_task(&mut self, index: usize, task: Task, check_flash: bool, monitor: Option<String>) {
        let Some(sh) = self.tabs.get_mut(index) else { return };
        if sh.pending_write.is_some() {
            return;
        }
        self.prob_from = Some((index, sh.tv.cursor_row()));
        self.prob_at = None;
        self.problems.clear();
        let data = format!("{}\r", task.cmd).into_bytes();
        if check_flash {
            self.events.push(ShellEvt::Task(task.name, task.cmd.clone()));
        }
        if !sh.remote {
            self.events.push(sh.command_event(format!("{} {}", sh.tv.cursor_line(), task.cmd), data, monitor));
        } else {
            sh.write(&data);
        }
        sh.tv.focus_req = true;
        self.s.open = true;
    }

    pub fn submit(&mut self, id: usize, data: Vec<u8>, releases: Vec<Receiver<()>>) {
        if let Some(sh) = self.tabs.iter_mut().find(|s| s.id == id && s.exited.is_none()) {
            if releases.is_empty() {
                sh.write(&data);
            } else {
                sh.pending_write = Some((data, releases));
            }
        }
    }

    fn refresh_problems(&mut self) {
        if self.prob_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(800)) {
            return;
        }
        self.prob_at = Some(std::time::Instant::now());
        let Some(sh) = self.tabs.get(self.active) else {
            self.problems.clear();
            return;
        };
        let from = match self.prob_from {
            Some((i, row)) if i == self.active => row,
            _ => 0,
        };
        self.problems = parse_problems(&sh.tv.text_since(from));
    }

    /// 某个终端里的命令跑完、回到提示符了吗：None = 终端已经关了 / 进程退出了
    pub fn back_at_prompt(&self, id: usize, prompt: &str, row: u64, settle: bool) -> Option<bool> {
        let sh = self.tabs.iter().find(|s| s.id == id && s.exited.is_none())?;
        let quiet = !settle || sh.tv.fed_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(700));
        Some(quiet && sh.tv.cursor_row() > row && sh.tv.cursor_line() == prompt.trim_end())
    }

    pub fn kinds(&mut self) -> &[ShellKind] {
        self.kinds.get_or_insert_with(detect)
    }

    pub fn focused(&self, ctx: &egui::Context) -> bool {
        self.s.open && self.tabs.get(self.active).is_some_and(|sh| ctx.memory(|m| m.has_focus(sh.tv.id())))
    }

    pub fn project_changed(&mut self) {
        self.project_shell_pending = true;
    }

    pub fn show_below_debug(&mut self) {
        self.maximized = false;
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
        self.launch(ctx, kind, cwd, None);
    }

    fn launch(&mut self, ctx: &egui::Context, kind: Option<ShellKind>, cwd: &Path, task: Option<Task>) {
        self.project_shell_pending = false;
        let default = self.s.default.clone();
        let kinds = self.kinds();
        let Some(kind) = kind.or_else(|| kinds.iter().find(|k| k.name == default).or(kinds.first()).cloned()) else {
            self.events.push(ShellEvt::Toast(t("没有找到可用的 shell").into()));
            return;
        };
        let env = self.s.environments.iter().find(|e| e.name == self.s.environment).cloned();
        let options = self.s.mcu.get(cwd).cloned().unwrap_or_default();
        self.next_id += 1;
        match Shell::spawn_with_env(ctx, &kind, cwd, self.next_id, env.as_ref(), None) {
            Ok(mut sh) => {
                let scan = env.is_none() && options.profile != mcu::Profile::System && sh.syntax.is_some();
                let mut task = task;
                if scan {
                    sh.discovery = Some(Discovery {
                        kind,
                        state: DiscoveryState::AwaitPaint(options),
                        task: task.take(),
                    });
                }
                self.tabs.push(sh);
                self.active = self.tabs.len() - 1;
                self.s.open = true;
                if let Some(task) = task {
                    self.submit_task(self.active, task, true, None);
                }
            }
            Err(e) => self.events.push(ShellEvt::Toast(tf("启动 {} 失败：{}", &[&kind.name, &e]))),
        }
    }

    fn poll_launch(&mut self, ctx: &egui::Context) {
        for i in 0..self.tabs.len() {
            let sh = &mut self.tabs[i];
            let Some(mut job) = sh.discovery.take() else { continue };
            if sh.exited.is_some() {
                continue;
            }
            match job.state {
                DiscoveryState::Painted(ref options, at) if sh.tv.fed_at.is_some() || at.elapsed() >= std::time::Duration::from_millis(500) => {
                    let (tx, rx) = mpsc::channel();
                    let root = sh.cwd.clone();
                    let options = options.clone();
                    let wake = ctx.clone();
                    match std::thread::Builder::new().name("mcu-discover".into()).spawn(move || {
                        let _ = tx.send(mcu::Session::discover(root, options));
                        wake.request_repaint();
                    }) {
                        Ok(_) => job.state = DiscoveryState::Scanning(rx),
                        Err(e) => {
                            self.events.push(ShellEvt::Toast(e.to_string()));
                            continue;
                        }
                    }
                }
                DiscoveryState::Scanning(ref rx) => match rx.try_recv() {
                    Ok(session) => {
                        if let Some(err) = &session.issue {
                            self.events.push(ShellEvt::Toast(err.clone()));
                        }
                        let pending = session.projects.pending;
                        let mut dest = i;
                        if !session.actions().is_empty() {
                            // Environment and shell functions must be inherited at process creation.
                            // Keep a PTY once the user has typed or started work in it.
                            let preserve = sh.used;
                            let id = if preserve {
                                self.next_id += 1;
                                self.next_id
                            } else {
                                sh.id
                            };
                            match Shell::spawn_with_env(ctx, &job.kind, &sh.cwd, id, None, Some(Arc::new(session))) {
                                Ok(mut configured) => {
                                    configured.tv.focus_req = !preserve && i == self.active && sh.tv.focus_req;
                                    configured.color = sh.color;
                                    if preserve {
                                        configured.title = format!("MCU · {}", sh.title);
                                        self.tabs.push(configured);
                                        dest = self.tabs.len() - 1;
                                        if let Some(pending) = self.instrument_pending.as_mut().filter(|pending| pending.id == self.tabs[i].id) {
                                            pending.id = id;
                                        }
                                        self.events.push(ShellEvt::Toast(t("MCU 环境已就绪，可切换到 MCU 标签").into()));
                                    } else {
                                        configured.title = sh.title.clone();
                                        *sh = configured;
                                    }
                                }
                                Err(e) => {
                                    self.events.push(ShellEvt::Toast(e));
                                    continue;
                                }
                            }
                        } else {
                            sh.mcu = Some(Arc::new(session));
                        }
                        if !pending {
                            if let Some(task) = job.task {
                                self.submit_task(dest, task, true, None);
                            }
                        }
                        continue;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.events.push(ShellEvt::Toast(t("检测 MCU 工具链失败").into()));
                        continue;
                    }
                },
                _ => {}
            }
            sh.discovery = Some(job);
        }
    }

    fn close(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            self.split = match self.split {
                Some(j) if j == i => None,
                Some(j) if j > i => Some(j - 1),
                x => x,
            };
            self.prob_from = None;
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            if self.tabs.is_empty() {
                self.s.open = false;
            }
        }
    }

    /// 每帧调用：读 shell 输出（面板收起时也要读，避免缓冲区堆满）
    pub fn poll(&mut self, window_focused: bool) {
        let mut ready = Vec::new();
        for (i, sh) in self.tabs.iter_mut().enumerate() {
            sh.poll();
            if let Some(shortcut) = sh.pending_shortcut.take().filter(|_| sh.exited.is_none()) {
                if sh.tv.cursor_line() == shortcut.prompt.trim_end() {
                    ready.push((i, shortcut));
                } else if shortcut.at.elapsed() < std::time::Duration::from_secs(2) {
                    sh.pending_shortcut = Some(shortcut);
                } else {
                    self.events.push(ShellEvt::Toast(t("终端未回到提示符，请用工具栏执行 MCU 命令").into()));
                }
            }
            if let Some(d) = sh.tv.take_done() {
                let visible = self.s.open && (i == self.active || self.split == Some(i));
                if !visible || !window_focused {
                    sh.attn = !visible;
                    self.events.push(ShellEvt::Done(sh.title.clone(), d));
                }
            }
        }
        for (index, shortcut) in ready {
            self.submit_task(index, shortcut.task, false, shortcut.monitor);
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, cwd: &Path, port: &str, ports: &[crate::serial::PortEntry]) {
        if let Some(job) = self.instrument_pending.take() {
            let index = self.tabs.iter().position(|sh| sh.id == job.id && sh.exited.is_none());
            if job.root != cwd || job.since.elapsed() > std::time::Duration::from_secs(30) {
                self.events.push(ShellEvt::Toast(t("请等待当前工程的终端就绪").into()));
            } else if let Some(index) = index {
                let sh = &self.tabs[index];
                if sh.discovery.is_none() && (sh.mcu.is_none() || sh.at_prompt()) {
                    if let Err(e) = self.submit_instrument(index, cwd, job.flash, &job.port) {
                        self.events.push(ShellEvt::Toast(e));
                    }
                } else {
                    self.instrument_pending = Some(job);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            } else {
                self.events.push(ShellEvt::Toast(t("请等待当前工程的终端就绪").into()));
            }
        }
        if !self.s.open {
            self.poll_launch(ctx);
            self.rect = None;
            return;
        }
        if std::mem::take(&mut self.project_shell_pending) {
            // An existing PTY may be busy, and its launch directory isn't
            // necessarily its current directory. Keep it open and start fresh.
            self.new_shell(ctx, None, cwd);
        }
        self.poll_launch(ctx);
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
                self.header(ui, cwd, port, ports);
                let rect = ui.available_rect_before_wrap().shrink2(vec2(6.0, 4.0));
                self.rect = Some(ui.max_rect());
                if self.split.is_some_and(|j| j == self.active || j >= self.tabs.len()) {
                    self.split = None;
                }
                let panes: Vec<(usize, Rect)> = match self.split {
                    Some(j) => {
                        let mid = rect.center().x;
                        vec![
                            (self.active, Rect::from_min_max(rect.min, pos2(mid - 3.0, rect.max.y))),
                            (j, Rect::from_min_max(pos2(mid + 3.0, rect.min.y), rect.max)),
                        ]
                    }
                    None => vec![(self.active, rect)],
                };
                for (i, r) in panes {
                    if self.show_tab(ui, i, r, port) {
                        let old = &self.tabs[i];
                        let env = old.environment.clone();
                        let mcu = old.mcu.clone();
                        let (kname, title, color, cwd) = (old.kind.clone(), old.title.clone(), old.color, old.cwd.clone());
                        if let Some(kind) = self.kinds().iter().find(|k| k.name == kname).cloned() {
                            self.next_id += 1;
                            match Shell::spawn_with_env(ui.ctx(), &kind, &cwd, self.next_id, env.as_ref(), mcu) {
                                Ok(mut new) => {
                                    new.title = title;
                                    new.color = color;
                                    self.tabs[i] = new;
                                }
                                Err(_) => self.events.push(ShellEvt::Toast(t("启动失败").into())),
                            }
                        }
                    }
                }
            });
        if dragging && !self.maximized && (r.response.rect.height() - want).abs() > 0.5 {
            self.s.height = r.response.rect.height();
        }
    }

    fn tasks_window(&mut self, ctx: &egui::Context) {
        if !self.edit_tasks {
            return;
        }
        let mut open = true;
        let mut del = None;
        egui::Window::new(t("任务按钮"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.label(
                    RichText::new(t("终端面板头部的按钮：点一下就在当前终端里执行命令，输出里的错误和警告会列在「问题」里"))
                        .size(11.5)
                        .color(TEXT3()),
                );
                ui.add_space(6.0);
                for (k, tk) in self.s.tasks.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(80.0, 24.0), egui::TextEdit::singleline(&mut tk.name).hint_text(t("名称")));
                        ui.add_sized(vec2(320.0, 24.0), egui::TextEdit::singleline(&mut tk.cmd).hint_text("make -j8").font(egui::TextStyle::Monospace));
                        if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                            del = Some(k);
                        }
                    });
                }
                if ui.button(t("+ 添加任务")).clicked() {
                    self.s.tasks.push(Task::default());
                }
            });
        if let Some(k) = del {
            self.s.tasks.remove(k);
        }
        if !open {
            self.s.tasks.retain(|t| !t.name.trim().is_empty() && !t.cmd.trim().is_empty());
            self.edit_tasks = false;
        }
    }

    /// 画一个标签的终端；返回 true 表示进程已退出、用户按了回车要重启
    fn show_tab(&mut self, ui: &mut egui::Ui, i: usize, rect: Rect, port: &str) -> bool {
        let Some(sh) = self.tabs.get_mut(i) else { return false };
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let o = sh.tv.show(&mut child, &|s: &str| s.as_bytes().to_vec(), None, true);
        sh.sync_size();
        if let Some(job) = &mut sh.discovery {
            if let DiscoveryState::AwaitPaint(options) = &job.state {
                job.state = DiscoveryState::Painted(options.clone(), std::time::Instant::now());
                ui.ctx().request_repaint();
            } else if matches!(job.state, DiscoveryState::Painted(..)) {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
            }
        }
        if o.focused {
            sh.attn = false;
            if i != self.active {
                // 点了右半边：它变成当前标签，原来的当前标签挪到右边
                self.split = Some(self.active);
                self.active = i;
            }
        }
        let sh = &mut self.tabs[i];
        let mut restart = false;
        if sh.exited.is_some() {
            restart = o.input.contains(&b'\r');
        } else if !o.input.is_empty() && sh.pending_write.is_none() && sh.pending_shortcut.is_none() {
            if let Some(line) = o.enter_line.filter(|_| !sh.remote) {
                let shortcut = (o.input == b"\r").then(|| sh.serial_shortcut(&line, port)).flatten();
                if let Some(event) = shortcut {
                    self.events.extend(event);
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
                } else {
                    self.events.push(sh.command_event(line, o.input, None));
                }
            } else {
                sh.write(&o.input);
            }
        }
        if let Some(m) = o.toast {
            self.events.push(ShellEvt::Toast(m.into()));
        }
        if let Some((f, line)) = o.open {
            let base = self.tabs.get(i).map(|s| s.cwd.clone()).unwrap_or_default();
            let p = if Path::new(&f).is_absolute() { PathBuf::from(&f) } else { base.join(&f) };
            if p.is_file() {
                self.events.push(ShellEvt::Open(p, line.saturating_sub(1)));
            } else {
                self.events.push(ShellEvt::Toast(t("找不到这个文件").into()));
            }
        }
        restart
    }

    fn header(&mut self, ui: &mut egui::Ui, cwd: &Path, port: &str, ports: &[crate::serial::PortEntry]) {
        let ctx = ui.ctx().clone();
        let mut close = None;
        let mut pick = None;
        let mut new_kind: Option<Option<ShellKind>> = None;
        let mut task_run = None;
        let mut action_run = None;
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
            let mut rename_start = None;
            let mut set_color = None;
            let mut set_split: Option<Option<usize>> = None;
            let mut rename_done = None;
            for (i, sh) in self.tabs.iter().enumerate() {
                let on = i == self.active;
                let font = FontId::proportional(12.0);
                if let Some((ri, text)) = self.rename.as_mut().filter(|(ri, _)| *ri == i) {
                    let te = ui.add_sized(vec2(120.0, 24.0), egui::TextEdit::singleline(text).font(font.clone()));
                    if !te.has_focus() && !te.lost_focus() {
                        te.request_focus();
                    }
                    if te.lost_focus() {
                        let ok = !ui.input(|i| i.key_pressed(egui::Key::Escape));
                        rename_done = Some((*ri, ok.then(|| text.trim().to_string())));
                    }
                    continue;
                }
                let name = if sh.exited.is_some() { format!("{} ✕", sh.title) } else { sh.title.clone() };
                let bar = tab_color(sh.color);
                let pad = if bar.is_some() { 16.0 } else { 10.0 };
                let w = ui.fonts(|f| f.layout_no_wrap(name.clone(), font.clone(), TEXT()).size().x) + 30.0 + pad;
                let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
                let p = ui.painter();
                let in_split = self.split == Some(i);
                if on || in_split {
                    p.rect_filled(r, R_WIDGET, BG2());
                    p.hline(r.x_range().shrink(6.0), r.bottom() - 0.5, st(1.5, if on { ACCENT() } else { ACCENT().gamma_multiply(0.4) }));
                } else if resp.hovered() {
                    p.rect_filled(r, R_WIDGET, tint(6));
                }
                if let Some(c) = bar {
                    p.rect_filled(Rect::from_min_size(pos2(r.left() + 6.0, r.top() + 6.0), vec2(3.0, 12.0)), 1.5, c);
                }
                p.text(pos2(r.left() + pad, r.center().y), Align2::LEFT_CENTER, &name, font, if on { TEXT() } else { TEXT3() });
                let xr = Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(14.0, 14.0));
                let hover_x = resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| xr.contains(p));
                if on || resp.hovered() {
                    draw_icon(p, xr.shrink(2.0), Icon::Close, if hover_x { TEXT() } else { TEXT3() });
                } else if sh.attn {
                    p.circle_filled(xr.center(), 3.5, ACCENT());
                }
                if resp.double_clicked() {
                    rename_start = Some(i);
                } else if resp.clicked() {
                    if hover_x {
                        close = Some(i);
                    } else {
                        pick = Some(i);
                    }
                }
                if resp.middle_clicked() {
                    close = Some(i);
                }
                let resp = resp.on_hover_text(format!("{}\n{}", t("双击改名，右键设置颜色、拆分"), sh.cwd.display()));
                resp.context_menu(|ui| {
                    if ui.button(t("重命名…")).clicked() {
                        rename_start = Some(i);
                        ui.close_menu();
                    }
                    ui.horizontal(|ui| {
                        for k in 0..TAB_COLORS.len() {
                            let (cr, cresp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                            match tab_color(k) {
                                Some(c) => {
                                    ui.painter().circle_filled(cr.center(), 6.0, c);
                                }
                                None => {
                                    ui.painter().circle_stroke(cr.center(), 5.5, st(1.0, TEXT3()));
                                }
                            }
                            if sh.color == k {
                                ui.painter().circle_stroke(cr.center(), 8.5, st(1.0, TEXT()));
                            }
                            if cresp.clicked() {
                                set_color = Some((i, k));
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    if !on {
                        if ui.button(if in_split { t("取消拆分") } else { t("在右侧并排显示") }).clicked() {
                            set_split = Some(if in_split { None } else { Some(i) });
                            ui.close_menu();
                        }
                    } else if self.split.is_some() && ui.button(t("取消拆分")).clicked() {
                        set_split = Some(None);
                        ui.close_menu();
                    }
                    if ui.button(t("关闭")).clicked() {
                        close = Some(i);
                        ui.close_menu();
                    }
                });
            }
            if let Some(i) = rename_start {
                self.rename = Some((i, self.tabs[i].title.clone()));
            }
            if let Some((i, v)) = rename_done {
                if let (Some(v), Some(sh)) = (v.filter(|v| !v.is_empty()), self.tabs.get_mut(i)) {
                    sh.title = v;
                }
                self.rename = None;
            }
            if let Some((i, k)) = set_color {
                self.tabs[i].color = k;
            }
            if let Some(v) = set_split {
                self.split = v;
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
                }
                // 问题列表：最近一次任务（或整个终端）输出里的错误和警告
                self.refresh_problems();
                let (ne, nw) = (self.problems.iter().filter(|p| p.error).count(), self.problems.iter().filter(|p| !p.error).count());
                if ne + nw > 0 {
                    let label = format!("● {ne}  ▲ {nw}");
                    let col = if ne > 0 { ERR() } else { WARN() };
                    let btn = ui
                        .add(egui::Button::new(RichText::new(label).size(11.5).color(col)).fill(col.gamma_multiply(0.10)).min_size(vec2(0.0, 22.0)))
                        .on_hover_text(t("编译错误和警告，点一下列出，单击跳到代码"));
                    let pop = ui.id().with("shell-problems");
                    if btn.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(pop));
                    }
                    let mut open_p = None;
                    egui::popup::popup_above_or_below_widget(ui, pop, &btn, egui::AboveOrBelow::Below, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                        ui.set_min_width(520.0);
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for p in &self.problems {
                                let name = Path::new(&p.file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.file.clone());
                                let r = ui.add(
                                    egui::Button::new(
                                        RichText::new(format!("{}  {}:{}  {}", if p.error { "●" } else { "▲" }, name, p.line, p.msg))
                                            .size(12.0)
                                            .color(if p.error { ERR() } else { WARN() }),
                                    )
                                    .frame(false)
                                    .wrap_mode(egui::TextWrapMode::Truncate),
                                );
                                if r.on_hover_text(&p.file).clicked() {
                                    open_p = Some((p.file.clone(), p.line));
                                }
                            }
                        });
                    });
                    if let Some((f, line)) = open_p {
                        let base = self.tabs.get(self.active).map(|s| s.cwd.clone()).unwrap_or_default();
                        let path = if Path::new(&f).is_absolute() { PathBuf::from(&f) } else { base.join(&f) };
                        if path.is_file() {
                            self.events.push(ShellEvt::Open(path, line.saturating_sub(1)));
                        } else {
                            self.events.push(ShellEvt::Toast(t("找不到这个文件").into()));
                        }
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
                let sh = self.tabs.get(self.active);
                let detected = sh.and_then(|sh| sh.mcu.as_ref());
                let pending = detected.is_some_and(|m| m.projects.pending);
                let mcu = detected.filter(|m| !m.actions().is_empty());
                let name = if sh.is_some_and(|s| s.discovery.is_some()) {
                    t("检测工具链…").into()
                } else if pending {
                    t("选择沁恒子工程").into()
                } else if let Some(env) = sh.and_then(|sh| sh.environment.as_ref()) {
                    crate::app::truncate(&env.name, 22).into()
                } else {
                    mcu.map_or_else(|| t("MCU 工具链").into(), |m| m.profile.status_label())
                };
                if ui.small_button(name).on_hover_text(t("MCU 工具链")).clicked() {
                    self.env_dialog.open = true;
                }
                if mcu.is_some() {
                    ui.menu_button(if port.is_empty() { t("设备串口") } else { port }, |ui| {
                        for entry in ports {
                            if ui.selectable_label(entry.name == port, &entry.name).on_hover_text(&entry.desc).clicked() {
                                self.events.push(ShellEvt::SelectSerial(entry.name.clone()));
                                ui.close_menu();
                            }
                        }
                        if ui.button(t("打开调试设备列表")).clicked() {
                            self.events.push(ShellEvt::ShowSerialDevices);
                            ui.close_menu();
                        }
                    })
                    .response
                    .on_hover_text(t("烧录和监视跟随调试区选中的设备串口"));
                }
                if !pending && sh.is_none_or(|s| s.discovery.is_none()) {
                    let syntax = sh.and_then(|sh| sh.syntax);
                    let ready = sh.is_some_and(Shell::at_prompt);
                    ui.menu_button(t("自定义命令"), |ui| {
                        if let Some((m, syntax)) = mcu.zip(syntax) {
                            for &action in m.actions().iter().filter(|&&a| !primary_action(a)) {
                                if action_button(ui, m, syntax, action, ready, port) {
                                    action_run = Some(action);
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                        }
                        for task in &self.s.tasks {
                            if mcu.is_some() && DEFAULT_TASKS.contains(&(task.name.as_str(), task.cmd.as_str())) {
                                continue;
                            }
                            if ui.add_enabled(mcu.is_none() || ready, egui::Button::new(&task.name)).on_hover_text(&task.cmd).clicked() {
                                task_run = Some(task.clone());
                                ui.close_menu();
                            }
                        }
                        if ui.button(t("编辑任务…")).clicked() {
                            self.edit_tasks = true;
                            ui.close_menu();
                        }
                    });
                    if let Some((m, syntax)) = mcu.zip(syntax) {
                        for &action in m.actions().iter().rev().filter(|&&a| primary_action(a)) {
                            if action_button(ui, m, syntax, action, ready, port) {
                                action_run = Some(action);
                            }
                        }
                    } else {
                        for task in self.s.tasks.iter().rev().filter(|task| DEFAULT_TASKS.contains(&(task.name.as_str(), task.cmd.as_str()))) {
                            if ui.small_button(default_task_label(task)).on_hover_text(&task.cmd).clicked() {
                                task_run = Some(task.clone());
                            }
                        }
                    }
                    for task in self
                        .s
                        .tasks
                        .iter()
                        .rev()
                        .filter(|task| !task.name.trim().is_empty() && !task.cmd.trim().is_empty() && !DEFAULT_TASKS.contains(&(task.name.as_str(), task.cmd.as_str())))
                    {
                        let button = ui
                            .add_enabled(mcu.is_none() || ready, egui::Button::new(crate::app::truncate(&task.name, 20)).small())
                            .on_hover_text(format!("{}\n{}", task.cmd, t("右键编辑任务")));
                        if button.clicked() {
                            task_run = Some(task.clone());
                        }
                        if button.secondary_clicked() {
                            self.edit_tasks = true;
                        }
                    }
                }
            });
        });
        if action_run == Some(Action::Monitor) {
            self.events.push(ShellEvt::MonitorSerial);
        } else if let Some(action) = action_run {
            if let Some((m, syntax)) = self.tabs.get(self.active).and_then(|sh| sh.mcu.as_ref().zip(sh.syntax)) {
                match m.command(syntax, action, port) {
                    Ok(cmd) => self.submit_task(
                        self.active,
                        Task { name: action.label().into(), cmd },
                        m.workspace == cwd && matches!(action, Action::Flash | Action::AppFlash | Action::AppFlashMonitor),
                        action.monitor_after().then(|| port.to_owned()),
                    ),
                    Err(e) => self.events.push(ShellEvt::Toast(e)),
                }
            }
        }
        if let Some(task) = task_run {
            self.run_task(&ctx, cwd, task, port);
        }
        self.tasks_window(&ctx);
        self.environment_dialog(&ctx, cwd);
        if let Some(i) = pick {
            self.active = i;
            self.tabs[i].attn = false;
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

fn action_button(ui: &mut egui::Ui, mcu: &mcu::Session, syntax: Syntax, action: Action, ready: bool, port: &str) -> bool {
    if action == Action::Monitor {
        return ui.small_button(action.label()).on_hover_text(t("请在调试页查看串口日志")).clicked();
    }
    ui.add_enabled(ready, egui::Button::new(action.label()).small())
        .on_disabled_hover_text(t("请等待终端回到命令提示符"))
        .on_hover_ui(|ui| match mcu.command(syntax, action, port) {
            Ok(cmd) => {
                ui.monospace(cmd);
            }
            Err(e) => {
                ui.label(e);
            }
        })
        .clicked()
}

#[cfg(test)]
mod tests {
    use super::{parse_problems, prompt_of, uses_port};

    #[cfg(unix)]
    #[test]
    fn restored_open_panel_launches_once_in_the_workspace() {
        use super::*;
        let ctx = egui::Context::default();
        let tmp = tempfile::tempdir().unwrap();
        let mut shells = Shells::new(Default::default());
        let _ = ctx.run(Default::default(), |ctx| shells.ui(ctx, tmp.path(), "", &[]));
        assert!(shells.tabs.is_empty());
        shells.s.open = true;
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| shells.ui(ctx, tmp.path(), "", &[]));
        }
        assert_eq!(shells.tabs.len(), 1);
        assert_eq!(shells.tabs[0].cwd, tmp.path());
        assert!(shells.tabs[0].discovery.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn local_shell_accepts_input_before_discovery_and_keeps_busy_tab() {
        use super::*;
        let ctx = egui::Context::default();
        let tmp = tempfile::tempdir().unwrap();
        let mut shells = Shells::new(Default::default());
        let kind = ShellKind {
            name: "sh".into(),
            program: "/bin/sh".into(),
            args: vec!["-i".into()],
        };
        shells.new_shell(&ctx, Some(kind), tmp.path());
        let sh = &mut shells.tabs[0];
        assert!(matches!(sh.discovery.as_ref().unwrap().state, DiscoveryState::AwaitPaint(_)));
        let id = sh.id;
        sh.write(b"echo QCCOM_FIRST_PROMPT > ready\r");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !tmp.path().join("ready").exists() && std::time::Instant::now() < until {
            shells.poll_launch(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(tmp.path().join("ready")).unwrap().trim(), "QCCOM_FIRST_PROMPT");
        let mut job = shells.tabs[0].discovery.take().unwrap();
        let options = mcu::Options {
            profile: mcu::Profile::Make,
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        tx.send(mcu::Session::discover(tmp.path().into(), options)).ok().unwrap();
        job.state = DiscoveryState::Scanning(rx);
        shells.tabs[0].discovery = Some(job);
        shells.poll_launch(&ctx);
        assert_eq!(shells.tabs.len(), 2);
        assert_eq!(shells.tabs[shells.active].id, id);
        assert!(shells.tabs[1].mcu.is_some() && !shells.tabs[1].tv.focus_req);
    }

    #[test]
    fn port_in_command() {
        assert!(uses_port("esptool.py -p COM3 write_flash 0x0 a.bin", "COM3"));
        assert!(uses_port("idf.py -p com3 flash", "COM3"));
        assert!(uses_port("mode \\\\.\\COM3", "COM3"));
        assert!(!uses_port("esptool.py -p COM31 flash", "COM3"));
        assert!(!uses_port("echo COMMAND", "COM3"));
        assert!(uses_port("idf.py -p /dev/ttyUSB0 flash", "/dev/ttyUSB0"));
        assert!(uses_port("stty -F ttyUSB0 115200", "/dev/ttyUSB0"));
        assert!(!uses_port("stty -F ttyUSB01", "/dev/ttyUSB0"));
        assert!(!uses_port("ls", ""));
    }

    #[test]
    fn prompt_part() {
        assert_eq!(prompt_of(r"PS C:\fw> idf.py -p COM3 flash"), r"PS C:\fw> ");
        assert_eq!(prompt_of("zenghi@pc:~/fw$ make flash"), "zenghi@pc:~/fw$ ");
        assert_eq!(prompt_of(r"C:\fw>flash.bat"), r"C:\fw>");
        assert_eq!(prompt_of("flash"), "");
    }

    #[test]
    fn problems() {
        let out = "gcc -c main.c\nsrc/main.c:12:5: error: 'x' undeclared\nsrc/main.c:20:1: warning: unused variable 'y'\n\
main.c(42): error:  #20: identifier \"foo\" is undefined\n\"uart.c\",7  Error[Pe020]: identifier \"bar\" is undefined\n\
C:\\fw\\a.c(3,9): warning C4101: 'z': unreferenced\ntime 12:30:01 ok\nsrc/main.c:12:5: error: 'x' undeclared\n";
        let p = parse_problems(out);
        assert_eq!(p.len(), 5, "{p:?}");
        assert_eq!((p[0].file.as_str(), p[0].line, p[0].error), ("src/main.c", 12, true));
        assert!(!p[1].error);
        assert_eq!((p[2].file.as_str(), p[2].line), ("main.c", 42));
        assert_eq!((p[3].file.as_str(), p[3].line), ("uart.c", 7));
        assert_eq!((p[4].file.as_str(), p[4].line, p[4].error), ("C:\\fw\\a.c", 3, false));
    }
}

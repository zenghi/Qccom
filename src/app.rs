//! 主界面。
//!
//! 布局：左侧边栏（设备、连接、链路状态、显示设置）| 右侧（搜索与筛选、日志、命令栏）| 底部状态栏。
//! 设计上靠底色深浅和细分隔线分层，青色只留给焦点元素（连接按钮、当前端口、速率曲线）。

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, epaint, pos2, text::LayoutJob, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Layout, Modifiers, Rect, RichText, Sense, Shape, TextFormat, TextStyle};
use serde::{Deserialize, Serialize};

use crate::cmds::{default_groups, CmdAction, CmdGroup, CmdPanel, QuickCmd};
use crate::codeview::{CodeEvt, CodeSettings, CodeView};
use crate::gitview::{GitEvt, GitSettings, GitView};
use crate::i18n::{self, t, tf};
use crate::keymap::{self, Act, Keymap, Overrides, Scope};
use crate::logbuf::{Dir, Encoding, Level, Line, LogBuf};
use crate::net::{self, NetConfig, Transport};
use crate::serial::{self, Cmd, Connection, Evt, PortConfig, PortEntry, COMMON_BAUDS};
use crate::term::{self, Term};
use crate::theme::*;
use crate::update::{self, Release, UpdEvt};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Text,
    Hex,
    Mixed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineEnding {
    None,
    Lf,
    Cr,
    CrLf,
}

impl LineEnding {
    pub fn name(self) -> &'static str {
        match self {
            LineEnding::None => t("无换行"),
            LineEnding::Lf => "LF",
            LineEnding::Cr => "CR",
            LineEnding::CrLf => "CRLF",
        }
    }
    pub fn bytes(self) -> &'static [u8] {
        match self {
            LineEnding::None => b"",
            LineEnding::Lf => b"\n",
            LineEnding::Cr => b"\r",
            LineEnding::CrLf => b"\r\n",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: String,
    pub cfg: PortConfig,
    pub encoding: Encoding,
    pub view: ViewMode,
    pub show_ts: bool,
    pub show_ln: bool,
    pub show_tx: bool,
    pub ansi: bool,
    pub level_color: bool,
    pub min_level: Level,
    pub auto_reconnect: bool,
    pub follow_new_port: bool,
    pub line_ending: LineEnding,
    pub hex_send: bool,
    pub history: Vec<String>,
    pub keywords: String,
    pub max_lines: usize,
    pub font_size: f32,
    pub frame_gap_ms: u32,
    pub dtr: bool,
    pub rts: bool,
    pub repeat_ms: u32,
    pub sidebar: bool,
    pub advanced_open: bool,
    /// 设置的版本号，用来迁移旧默认值
    pub rev: u32,
    /// 是否已经按黄金比例摆过窗口（只在第一次启动时做）
    pub window_placed: bool,
    pub groups: Vec<CmdGroup>,
    pub cur_group: usize,
    pub cmd_panel: bool,
    /// 终端模式（键盘直通设备）还是日志模式
    pub terminal: bool,
    /// 连接方式：串口 / TCP / UDP / MQTT
    pub transport: Transport,
    pub net: NetConfig,
    pub code: CodeSettings,
    pub git: GitSettings,
    /// 界面语言："auto" 跟随系统
    pub lang: String,
    pub auto_update: bool,
    /// 用户选择跳过的版本，不再提示
    pub skip_version: String,
    /// 主题："system" 跟随系统 / "dark" / "light"
    pub theme: String,
    /// 用户改过的快捷键
    pub keys: Overrides,
    /// 复制日志时带上时间戳
    pub copy_ts: bool,
    /// 界面缩放（1.0 = 100%）
    pub ui_scale: f32,
    /// 命令面板最近用过的命令
    pub recent_cmds: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: String::new(),
            cfg: PortConfig::default(),
            encoding: Encoding::Utf8,
            view: ViewMode::Text,
            show_ts: true,
            show_ln: true,
            show_tx: true,
            ansi: true,
            level_color: true,
            min_level: Level::None,
            auto_reconnect: true,
            follow_new_port: true,
            line_ending: LineEnding::CrLf,
            hex_send: false,
            history: vec![],
            keywords: String::new(),
            max_lines: 100_000,
            font_size: 13.0,
            frame_gap_ms: 0,
            dtr: true,
            rts: false,
            repeat_ms: 1000,
            sidebar: true,
            advanced_open: false,
            rev: SETTINGS_REV,
            window_placed: false,
            groups: default_groups(),
            cur_group: 0,
            cmd_panel: true,
            terminal: false,
            transport: Transport::Serial,
            net: NetConfig::default(),
            code: CodeSettings::default(),
            git: GitSettings::default(),
            lang: "auto".into(),
            auto_update: true,
            skip_version: String::new(),
            theme: "system".into(),
            keys: Overrides::new(),
            copy_ts: false,
            ui_scale: 1.0,
            recent_cmds: Vec::new(),
        }
    }
}

fn kw_colors() -> [Color32; 6] {
    [VIOLET(), WARN(), ink(Color32::from_rgb(0xf4, 0x72, 0xb6)), DEBUG(), ink(Color32::from_rgb(0xfb, 0x92, 0x3c)), ACCENT()]
}

const RATE_HISTORY: usize = 60;

/// 左侧活动栏切换的页面：调试（串口/网络）、代码查看、Git
#[derive(Clone, Copy, PartialEq)]
enum Page {
    Debug,
    Code,
    Git,
    Settings,
}

/// 自动更新的界面状态
enum Upd {
    Idle,
    Checking { manual: bool },
    Available(Release),
    Downloading(Release, u64, u64),
    Ready(Release, PathBuf),
    Failed(Release, String),
}
const SETTINGS_REV: u32 = 2;

mod palette;
mod prefs;

/// 日志里按字选择的范围：(行号, 字符位置)，字符位置按这一行显示出来的文字计（含时间戳等前缀）
#[derive(Clone, Copy, PartialEq)]
struct TSel {
    a: (u64, usize),
    b: (u64, usize),
    drag: bool,
}

impl TSel {
    fn ordered(&self) -> ((u64, usize), (u64, usize)) {
        if self.a <= self.b {
            (self.a, self.b)
        } else {
            (self.b, self.a)
        }
    }
}

pub struct App {
    s: Settings,
    buf: LogBuf,
    ports: Vec<PortEntry>,
    ports_seen: bool,
    /// 命令行指定的端口（比如 Linux 的 /dev/pts/3 虚拟串口，系统扫描不到）
    extra_port: Option<String>,
    evt_tx: Sender<Evt>,
    evt_rx: Receiver<Evt>,
    ctx: egui::Context,

    conn: Option<Connection>,
    conn_gen: u64,
    connected: Option<String>,
    connected_at: Option<Instant>,
    autobaud: Option<u32>,
    reconnect: Option<(String, Instant)>,
    last_error: Option<String>,

    paused: bool,
    pause_buf: Vec<u8>,
    auto_scroll: bool,

    search: String,
    filter_only: bool,
    keywords: Vec<(String, Color32)>,
    view_ids: VecDeque<u64>,
    view_checked: u64,
    view_dirty: bool,
    search_hit: Option<u64>,
    scroll_to: Option<u64>,
    focus_search: bool,
    focus_send: bool,
    /// 选中的行（锚点, 终点），绝对行号
    sel: Option<(u64, u64)>,
    /// 按字选中的文字
    tsel: Option<TSel>,
    /// 拖选到日志区边缘时每帧滚动的距离
    drag_scroll: f32,
    log_scroll_y: f32,

    send_text: String,
    hist_pos: Option<usize>,
    repeat: bool,
    last_repeat: Instant,
    baud_text: String,

    rx_bytes: u64,
    tx_bytes: u64,
    rate: f64,
    rate_acc: u64,
    rate_t: Instant,
    rate_hist: VecDeque<f32>,

    record: Option<(BufWriter<File>, PathBuf)>,
    toast: Option<(String, Color32, Instant)>,
    logo: egui::TextureHandle,
    cmd_ui: CmdPanel,
    /// 整组依次发送的队列：(组, 剩余数据, 下一条的时间)
    seq: Option<(usize, VecDeque<Vec<u8>>, Instant)>,
    last_rx_at: Option<Instant>,
    last_tx_at: Option<Instant>,
    at_bottom: bool,
    jump_bottom: bool,

    term: Term,
    /// 终端往上翻了多少行（0 = 看最新）
    term_scroll: usize,
    term_wheel: f32,
    /// 终端里鼠标选中的范围：（绝对行, 列）锚点和终点
    term_sel: Option<((usize, usize), (usize, usize))>,
    term_focus_req: bool,
    /// 光标闪烁从这个时间重新计时（打字时光标保持常亮）
    term_blink_at: f64,

    /// TCP 服务器当前的客户端数
    peers: Option<usize>,
    /// 网络连接的自动重连是静默尝试（失败不刷屏）
    net_quiet: bool,
    sub_input: String,

    page: Page,
    code: CodeView,
    gitv: GitView,
    upd: Upd,
    upd_tx: Sender<UpdEvt>,
    upd_rx: Receiver<UpdEvt>,
    /// 启动后多久自动检查更新
    upd_check_at: Option<Instant>,
    upd_card: bool,

    km: Keymap,
    palette: Option<palette::Palette>,
    prefs: prefs::Prefs,
    /// 打开设置前所在的页面（关闭设置时回去）
    prev_page: Page,
    /// 命令面板选中后、需要下一帧在对应页面执行的命令
    pending_acts: Vec<Act>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 先按系统语言，这样第一次启动生成的默认快捷命令等也是对应语言
        i18n::apply("auto");
        let mut s: Settings = cc.storage.and_then(|st| eframe::get_value(st, eframe::APP_KEY)).unwrap_or_default();
        if s.rev < 2 {
            // v0.2 之前默认保留 20 万行，现在改为 10 万行以节省内存
            if s.max_lines == 200_000 {
                s.max_lines = 100_000;
            }
            s.rev = SETTINGS_REV;
        }
        i18n::apply(&s.lang);
        update::cleanup();
        let args: Vec<String> = std::env::args().skip(1).collect();
        let just_updated = args.iter().any(|a| a == "--updated");
        let extra_port = args.into_iter().find(|a| !a.starts_with("--"));
        if let Some(p) = &extra_port {
            s.port = p.clone();
        }
        let (upd_tx, upd_rx) = mpsc::channel();
        let code = CodeView::new(std::mem::take(&mut s.code));
        let gitv = GitView::new(std::mem::take(&mut s.git), cc.egui_ctx.clone());
        let auto_update = s.auto_update;
        let km = Keymap::new(&s.keys);
        if (s.ui_scale - 1.0).abs() > 0.01 {
            cc.egui_ctx.set_zoom_factor(s.ui_scale);
        }
        load_fonts(&cc.egui_ctx);
        apply(&cc.egui_ctx, s.font_size, resolve(&s.theme, &cc.egui_ctx));
        let (evt_tx, evt_rx) = mpsc::channel();
        serial::spawn_port_scanner(evt_tx.clone(), cc.egui_ctx.clone());
        let mut buf = LogBuf::new(s.max_lines);
        buf.frame_gap_ms = s.frame_gap_ms;
        let mut app = Self {
            baud_text: s.cfg.baud.to_string(),
            s,
            buf,
            ports: vec![],
            ports_seen: false,
            extra_port,
            evt_tx,
            evt_rx,
            ctx: cc.egui_ctx.clone(),
            conn: None,
            conn_gen: 0,
            connected: None,
            connected_at: None,
            autobaud: None,
            reconnect: None,
            last_error: None,
            paused: false,
            pause_buf: vec![],
            auto_scroll: true,
            search: String::new(),
            filter_only: false,
            keywords: vec![],
            view_ids: VecDeque::new(),
            view_checked: 0,
            view_dirty: true,
            search_hit: None,
            scroll_to: None,
            focus_search: false,
            focus_send: true,
            sel: None,
            tsel: None,
            drag_scroll: 0.0,
            log_scroll_y: 0.0,
            send_text: String::new(),
            hist_pos: None,
            repeat: false,
            last_repeat: Instant::now(),
            rx_bytes: 0,
            tx_bytes: 0,
            rate: 0.0,
            rate_acc: 0,
            rate_t: Instant::now(),
            rate_hist: VecDeque::from(vec![0.0; RATE_HISTORY]),
            record: None,
            toast: None,
            logo: cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied([96, 96], include_bytes!("../assets/logo96.rgba")),
                egui::TextureOptions::LINEAR,
            ),
            cmd_ui: CmdPanel::default(),
            seq: None,
            last_rx_at: None,
            last_tx_at: None,
            at_bottom: true,
            jump_bottom: false,
            term: Term::new(100, 30),
            term_scroll: 0,
            term_wheel: 0.0,
            term_sel: None,
            term_focus_req: true,
            term_blink_at: 0.0,
            peers: None,
            net_quiet: false,
            sub_input: String::new(),
            page: Page::Debug,
            code,
            gitv,
            upd: Upd::Idle,
            upd_tx,
            upd_rx,
            // 刚更新完的这次启动不再检查，避免服务器版本号没改时反复提示
            upd_check_at: (auto_update && !just_updated).then(|| Instant::now() + Duration::from_secs(3)),
            upd_card: false,
            km,
            palette: None,
            prefs: prefs::Prefs::default(),
            prev_page: Page::Debug,
            pending_acts: Vec::new(),
        };
        app.parse_keywords();
        if just_updated {
            app.toast(tf("已更新到 v{}", &[&env!("CARGO_PKG_VERSION")]), OK());
        }
        app
    }

    fn toast(&mut self, msg: impl Into<String>, color: Color32) {
        self.toast = Some((msg.into(), color, Instant::now()));
    }

    fn parse_keywords(&mut self) {
        self.keywords = self
            .s
            .keywords
            .split([',', '，', ';', ' '])
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .enumerate()
            .map(|(i, k)| (k.to_string(), kw_colors()[i % 6]))
            .collect();
    }

    // ------------------------------------------------------------ 串口

    fn connect(&mut self) {
        self.connect_inner(false);
    }

    /// quiet = 自动重连时的尝试，失败不刷屏
    fn connect_inner(&mut self, quiet: bool) {
        if self.s.transport != Transport::Serial {
            self.disconnect();
            self.conn_gen += 1;
            self.net_quiet = quiet;
            self.peers = None;
            if !quiet {
                self.buf.sys(tf("正在连接 {} …", &[&self.s.net.label(self.s.transport)]));
            }
            match net::open(self.conn_gen, self.s.transport, self.s.net.clone(), self.evt_tx.clone(), self.ctx.clone()) {
                Ok(c) => self.conn = Some(c),
                Err(e) => self.toast(e, ERR()),
            }
            return;
        }
        if self.s.port.is_empty() {
            self.toast(t("没有可用的串口"), WARN());
            return;
        }
        self.disconnect();
        self.conn_gen += 1;
        match serial::open(self.conn_gen, &self.s.port, self.s.cfg, self.s.dtr, self.s.rts, self.evt_tx.clone(), self.ctx.clone()) {
            Ok(c) => {
                self.conn = Some(c);
                self.connected = Some(self.s.port.clone());
                self.connected_at = Some(Instant::now());
                self.last_error = None;
            }
            Err(_) if quiet => {}
            Err(e) => {
                let msg = tf("打开 {} 失败：{}", &[&self.s.port, &e]);
                self.buf.sys(format!("✗ {msg}"));
                self.last_error = Some(msg.clone());
                self.toast(msg, ERR());
            }
        }
    }

    fn disconnect(&mut self) {
        self.reconnect = None;
        if let Some(c) = self.conn.take() {
            let _ = c.cmd.send(Cmd::Close);
        }
        self.connected = None;
        self.connected_at = None;
        self.repeat = false;
    }

    fn toggle_connect(&mut self) {
        if self.conn.is_some() || self.reconnect.is_some() {
            self.disconnect();
        } else {
            self.connect();
        }
    }

    fn start_autobaud(&mut self) {
        if self.s.port.is_empty() || self.autobaud.is_some() {
            return;
        }
        self.disconnect();
        self.autobaud = Some(0);
        self.buf.sys(tf("正在自动识别 {} 的波特率，请让设备持续输出日志…", &[&self.s.port]));
        serial::spawn_autobaud(self.s.port.clone(), self.s.cfg, self.evt_tx.clone(), self.ctx.clone());
    }

    fn send_bytes(&mut self, data: Vec<u8>) {
        let Some(c) = &self.conn else {
            self.toast(t("未连接"), WARN());
            return;
        };
        if data.is_empty() {
            return;
        }
        let _ = c.cmd.send(Cmd::Send(data.clone()));
        self.tx_bytes += data.len() as u64;
        self.last_tx_at = Some(Instant::now());
        self.buf.push_line(Dir::Tx, data, self.s.encoding);
    }

    /// 终端按键：直接发给设备，不在日志里记一行
    fn send_raw(&mut self, data: Vec<u8>) {
        let Some(c) = &self.conn else {
            self.toast(t("未连接"), WARN());
            return;
        };
        if data.is_empty() {
            return;
        }
        self.tx_bytes += data.len() as u64;
        self.last_tx_at = Some(Instant::now());
        let _ = c.cmd.send(Cmd::Send(data));
    }

    fn send_input(&mut self, from_timer: bool) {
        let text = self.send_text.clone();
        let data = if self.s.hex_send {
            match parse_hex(&text) {
                Ok(d) => d,
                Err(e) => {
                    self.repeat = false;
                    self.toast(e, ERR());
                    return;
                }
            }
        } else {
            let mut d = self.s.encoding.encode(&text);
            d.extend_from_slice(self.line_ending().bytes());
            d
        };
        if !from_timer && !text.is_empty() {
            self.s.history.retain(|h| h != &text);
            self.s.history.push(text);
            if self.s.history.len() > 100 {
                self.s.history.remove(0);
            }
        }
        self.hist_pos = None;
        self.send_bytes(data);
    }

    /// MQTT 每条消息本身就是一包，默认不加换行
    fn line_ending(&self) -> LineEnding {
        if self.s.transport == Transport::Mqtt {
            LineEnding::None
        } else {
            self.s.line_ending
        }
    }

    fn send_quick(&mut self, c: &QuickCmd) {
        match c.bytes(self.s.encoding, self.line_ending()) {
            Ok(d) => self.send_bytes(d),
            Err(e) => self.toast(e, ERR()),
        }
    }

    fn send_group(&mut self, gi: usize) {
        let Some(g) = self.s.groups.get(gi) else { return };
        let mut q = VecDeque::new();
        for c in &g.cmds {
            match c.bytes(self.s.encoding, self.line_ending()) {
                Ok(d) => q.push_back(d),
                Err(e) => {
                    self.toast(tf("「{}」{}", &[&c.name, &e]), ERR());
                    return;
                }
            }
        }
        self.seq = Some((gi, q, Instant::now()));
    }

    fn handle_events(&mut self) {
        while let Ok(e) = self.evt_rx.try_recv() {
            match e {
                Evt::Ports(list) => self.on_ports(list),
                Evt::Opened(gen, name) if gen == self.conn_gen => {
                    if self.s.transport == Transport::Serial {
                        let c = self.s.cfg;
                        self.buf.sys(tf("已连接 {}  {} {}", &[&name, &c.baud, &c.short()]));
                    } else {
                        self.buf.sys(tf("✓ 已连接 {}", &[&name]));
                        self.connected = Some(name);
                        self.connected_at = Some(Instant::now());
                        self.last_error = None;
                        self.reconnect = None;
                    }
                }
                Evt::Sys(m) => self.buf.sys(m),
                Evt::Peers(n) => self.peers = Some(n),
                Evt::Packet(tag, d) => {
                    self.rx_bytes += d.len() as u64;
                    self.last_rx_at = Some(Instant::now());
                    self.rate_acc += d.len() as u64;
                    // 每包一行，前面带上来源（MQTT 主题 / UDP 对方地址）
                    let mut line = self.s.encoding.encode(&format!("\x1b[36m[{tag}]\x1b[0m "));
                    let end = d.iter().rposition(|&b| b != b'\r' && b != b'\n').map_or(0, |i| i + 1);
                    line.extend_from_slice(&d[..end]);
                    line.push(b'\n');
                    self.term.feed(&line);
                    self.term.feed(b"\r");
                    if self.paused {
                        self.pause_buf.extend_from_slice(&line);
                    } else {
                        self.buf.close_open();
                        self.buf.push_rx(&line, self.s.encoding);
                    }
                }
                Evt::Opened(..) => {}
                Evt::Data(d) => {
                    self.rx_bytes += d.len() as u64;
                    self.last_rx_at = Some(Instant::now());
                    self.rate_acc += d.len() as u64;
                    let before = self.term.s.scrollback.len();
                    self.term.feed(&d);
                    if self.term_scroll > 0 {
                        // 正在往上翻时，新数据进来画面保持不动
                        let grown = self.term.s.scrollback.len().saturating_sub(before);
                        self.term_scroll = (self.term_scroll + grown).min(self.term.s.scrollback.len());
                    }
                    if !self.term.s.reply.is_empty() {
                        let r = std::mem::take(&mut self.term.s.reply);
                        self.send_raw(r);
                    }
                    if self.paused {
                        self.pause_buf.extend_from_slice(&d);
                    } else {
                        self.buf.push_rx(&d, self.s.encoding);
                    }
                }
                Evt::Closed(gen, reason) => {
                    if gen != self.conn_gen {
                        continue;
                    }
                    let was_open = self.connected.is_some();
                    let net = self.s.transport != Transport::Serial;
                    let name = self.connected.take().unwrap_or_else(|| if net { self.s.net.label(self.s.transport) } else { self.s.port.clone() });
                    self.conn = None;
                    self.connected_at = None;
                    self.repeat = false;
                    self.peers = None;
                    match reason {
                        Some(err) if net && !was_open => {
                            if self.net_quiet {
                                self.reconnect = Some((name, Instant::now()));
                            } else {
                                self.buf.sys(format!("✗ {err}"));
                                self.toast(err.clone(), ERR());
                                self.last_error = Some(err);
                            }
                        }
                        Some(err) if net => {
                            self.buf.sys(tf("✗ {} 连接中断：{}", &[&name, &err]));
                            self.last_error = Some(err);
                            if self.s.auto_reconnect {
                                self.buf.sys(t("3 秒后自动重连…"));
                                self.reconnect = Some((name, Instant::now()));
                            }
                        }
                        None => self.buf.sys(tf("已断开 {}", &[&name])),
                        Some(err) => {
                            self.buf.sys(tf("✗ {} 连接中断：{}", &[&name, &err]));
                            self.last_error = Some(err);
                            if self.s.auto_reconnect {
                                self.buf.sys(t("等待设备重新出现，自动重连中…"));
                                self.reconnect = Some((name, Instant::now()));
                            }
                        }
                    }
                }
                Evt::AutoBaudProgress(b) => self.autobaud = Some(b),
                Evt::AutoBaudDone(r) => {
                    self.autobaud = None;
                    match r {
                        Ok((baud, score)) => {
                            self.s.cfg.baud = baud;
                            self.baud_text = baud.to_string();
                            self.buf.sys(tf("✓ 识别到波特率 {}（置信度 {}%）", &[&baud, &format!("{:.0}", score * 100.0)]));
                            self.toast(tf("波特率 {}", &[&baud]), OK());
                            self.connect();
                        }
                        Err(e) => {
                            self.buf.sys(tf("✗ 自动识别失败：{}", &[&e]));
                            self.toast(t("自动识别失败"), ERR());
                        }
                    }
                }
            }
        }
    }

    fn on_ports(&mut self, mut list: Vec<PortEntry>) {
        if let Some(p) = &self.extra_port {
            if !list.iter().any(|e| &e.name == p) {
                list.push(PortEntry {
                    name: p.clone(),
                    desc: t("命令行指定").into(),
                });
            }
        }
        let new: Vec<String> = list.iter().filter(|p| !self.ports.iter().any(|o| o.name == p.name)).map(|p| p.name.clone()).collect();
        let first_scan = !self.ports_seen;
        self.ports_seen = true;
        self.ports = list;
        if !first_scan {
            for n in &new {
                self.buf.sys(tf("检测到串口 {}", &[&n]));
            }
        }
        if self.conn.is_none() && self.autobaud.is_none() {
            let has = |n: &str| self.ports.iter().any(|p| p.name == n);
            if !first_scan && self.s.follow_new_port && !new.is_empty() && self.reconnect.is_none() {
                self.s.port = new[0].clone();
                self.toast(tf("新设备 {}", &[&new[0]]), ACCENT());
            } else if !has(&self.s.port) {
                if let Some(p) = self.ports.first() {
                    self.s.port = p.name.clone();
                }
            }
        }
    }

    fn tick(&mut self) {
        // 自动重连：设备重新出现后稍等一下（等驱动就绪）再连
        if let Some((_, since)) = &self.reconnect {
            if self.s.transport != Transport::Serial && self.conn.is_none() && since.elapsed() > Duration::from_secs(3) {
                self.reconnect = None;
                self.connect_inner(true);
            }
        }
        if let Some((name, since)) = self.reconnect.as_ref().filter(|_| self.s.transport == Transport::Serial) {
            if self.conn.is_none() && since.elapsed() > Duration::from_millis(600) && self.ports.iter().any(|p| &p.name == name) {
                self.s.port = name.clone();
                let n = name.clone();
                self.reconnect = None;
                self.connect_inner(true);
                if self.conn.is_none() {
                    self.reconnect = Some((n, Instant::now()));
                }
            }
            self.ctx.request_repaint_after(Duration::from_millis(300));
        }
        // 速率统计（每秒一个点，给侧边栏曲线用）
        let dt = self.rate_t.elapsed().as_secs_f64();
        if dt >= 1.0 {
            self.rate = self.rate_acc as f64 / dt;
            self.rate_acc = 0;
            self.rate_t = Instant::now();
            self.rate_hist.push_back(self.rate as f32);
            while self.rate_hist.len() > RATE_HISTORY {
                self.rate_hist.pop_front();
            }
        }
        if self.conn.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(1000));
        }
        // 定时发送
        if self.repeat && self.conn.is_some() {
            let iv = Duration::from_millis(self.s.repeat_ms.max(10) as u64);
            if self.last_repeat.elapsed() >= iv {
                self.last_repeat = Instant::now();
                self.send_input(true);
            }
            self.ctx.request_repaint_after(iv.saturating_sub(self.last_repeat.elapsed()));
        }
        // 整组依次发送
        if let Some((gi, mut q, next)) = self.seq.take() {
            if self.conn.is_some() {
                if Instant::now() >= next {
                    if let Some(d) = q.pop_front() {
                        self.send_bytes(d);
                    }
                    let iv = Duration::from_millis(self.s.groups.get(gi).map_or(200, |g| g.interval_ms) as u64);
                    if !q.is_empty() {
                        self.seq = Some((gi, q, Instant::now() + iv));
                        self.ctx.request_repaint_after(iv);
                    }
                } else {
                    self.ctx.request_repaint_after(next - Instant::now());
                    self.seq = Some((gi, q, next));
                }
            }
        }
        // 录制
        let committed = std::mem::take(&mut self.buf.committed);
        if let Some((w, _)) = &mut self.record {
            let mut err = None;
            for id in &committed {
                if let Some(l) = self.buf.get(*id) {
                    if let Err(e) = writeln!(w, "{}", format_line(l, self.s.view)) {
                        err = Some(e);
                        break;
                    }
                }
            }
            if !committed.is_empty() {
                let _ = w.flush();
            }
            if let Some(e) = err {
                self.record = None;
                self.toast(tf("录制写入失败：{}", &[&e]), ERR());
            }
        }
    }

    // ------------------------------------------------------------ 过滤

    fn filtering(&self) -> bool {
        !self.s.show_tx || self.s.min_level != Level::None || (self.filter_only && !self.search.is_empty())
    }

    fn visible(&self, l: &Line) -> bool {
        if !self.s.show_tx && l.dir == Dir::Tx {
            return false;
        }
        if self.s.min_level != Level::None && l.dir == Dir::Rx && l.level < self.s.min_level {
            return false;
        }
        if self.filter_only && !self.search.is_empty() && find_ci(&l.plain, &self.search).is_none() {
            return false;
        }
        true
    }

    fn update_view(&mut self) {
        if !self.filtering() {
            return;
        }
        if self.view_dirty {
            self.view_ids.clear();
            self.view_checked = self.buf.dropped;
            self.view_dirty = false;
        }
        while self.view_ids.front().is_some_and(|&id| id < self.buf.dropped) {
            self.view_ids.pop_front();
        }
        self.view_checked = self.view_checked.max(self.buf.dropped);
        let total = self.buf.total();
        let upto = if total > 0 && self.buf.is_open(total - 1) { total - 1 } else { total };
        for id in self.view_checked..upto {
            if self.buf.get(id).is_some_and(|l| self.visible(l)) {
                self.view_ids.push_back(id);
            }
        }
        self.view_checked = self.view_checked.max(upto);
    }

    /// 当前显示的行数，以及（过滤时）未收完的最后一行
    fn rows(&self) -> (usize, Option<u64>) {
        if !self.filtering() {
            return (self.buf.lines.len(), None);
        }
        let total = self.buf.total();
        let open = (total > 0 && self.buf.is_open(total - 1) && self.buf.get(total - 1).is_some_and(|l| self.visible(l))).then_some(total - 1);
        (self.view_ids.len() + open.is_some() as usize, open)
    }

    fn row_id(&self, i: usize, open: Option<u64>) -> u64 {
        if !self.filtering() {
            self.buf.dropped + i as u64
        } else if i < self.view_ids.len() {
            self.view_ids[i]
        } else {
            open.unwrap_or(0)
        }
    }

    fn row_index(&self, id: u64) -> Option<usize> {
        if !self.filtering() {
            id.checked_sub(self.buf.dropped).map(|i| i as usize)
        } else {
            self.view_ids.binary_search(&id).ok()
        }
    }

    fn find_next(&mut self, back: bool) {
        if self.search.is_empty() || self.buf.lines.is_empty() {
            return;
        }
        let total = self.buf.total();
        let start = self.search_hit.unwrap_or(if back { total } else { self.buf.dropped.wrapping_sub(1) });
        let n = self.buf.lines.len() as u64;
        for k in 1..=n {
            let id = if back {
                self.buf.dropped + (start + n * 2 - self.buf.dropped - k) % n
            } else {
                self.buf.dropped + (start.wrapping_add(k).wrapping_sub(self.buf.dropped)) % n
            };
            if let Some(l) = self.buf.get(id) {
                if self.visible(l) && find_ci(&l.plain, &self.search).is_some() {
                    self.search_hit = Some(id);
                    self.scroll_to = Some(id);
                    self.auto_scroll = false;
                    return;
                }
            }
        }
        self.toast(t("没有匹配"), WARN());
    }

    fn selected_text(&self, with_ts: bool) -> Option<String> {
        let (a, b) = self.sel?;
        let (a, b) = (a.min(b), a.max(b));
        let mut out = String::new();
        for id in a..=b {
            if let Some(l) = self.buf.get(id) {
                if self.visible(l) {
                    if with_ts {
                        out.push_str(&format_line(l, self.s.view));
                    } else {
                        let (txt, pre) = self.display_text(l);
                        out.extend(txt.chars().skip(pre));
                    }
                    out.push('\n');
                }
            }
        }
        Some(out)
    }

    /// 一行显示出来的文字，以及开头时间戳、方向标记占几个字符
    fn display_text(&self, l: &Line) -> (String, usize) {
        let job = self.line_job(l, &FontId::monospace(12.0));
        let mut pre = if self.s.show_ts { 14 } else { 0 };
        pre += match l.dir {
            Dir::Tx => 5,
            Dir::Sys => 2,
            Dir::Rx => 0,
        };
        (job.text, pre)
    }

    /// 按字选中的文字；不带时间戳时，每行开头落在时间戳里的部分去掉
    fn tsel_text(&self, with_ts: bool) -> Option<String> {
        let (a, b) = self.tsel?.ordered();
        let mut out = String::new();
        for id in a.0..=b.0 {
            let Some(l) = self.buf.get(id) else { continue };
            if !self.visible(l) {
                continue;
            }
            let (txt, pre) = self.display_text(l);
            let n = txt.chars().count();
            let mut from = if id == a.0 { a.1 } else { 0 };
            let to = if id == b.0 { b.1.min(n) } else { n };
            if !with_ts {
                from = from.max(pre);
            }
            if from < to {
                out.extend(txt.chars().skip(from).take(to - from));
            }
            if id != b.0 {
                out.push('\n');
            }
        }
        Some(out)
    }

    fn copy_selection(&mut self, ctx: &egui::Context, with_ts: bool) {
        let txt = if self.tsel.is_some() { self.tsel_text(with_ts) } else { self.selected_text(with_ts) };
        if let Some(tv) = txt.filter(|x| !x.is_empty()) {
            ctx.copy_text(tv);
            self.toast(t("已复制"), OK());
        }
    }

    fn select_all(&mut self) {
        let (n, open) = self.rows();
        if n > 0 {
            self.tsel = None;
            self.sel = Some((self.row_id(0, open), self.row_id(n - 1, open)));
        }
    }

    // ------------------------------------------------------------ 渲染一行

    fn text_color(&self, l: &Line) -> Color32 {
        match l.dir {
            Dir::Sys => ACCENT().gamma_multiply(0.9),
            Dir::Tx => VIOLET(),
            Dir::Rx => {
                if !self.s.level_color {
                    return LOG_TEXT();
                }
                match l.level {
                    Level::Error => ink(Color32::from_rgb(0xfc, 0xa5, 0xa5)),
                    Level::Warn => ink(Color32::from_rgb(0xfc, 0xd3, 0x4d)),
                    Level::Debug => TEXT2(),
                    Level::Trace => TRACE(),
                    _ => LOG_TEXT(),
                }
            }
        }
    }

    fn line_job(&self, l: &Line, font: &FontId) -> LayoutJob {
        let mut job = LayoutJob::default();
        let bold_font = FontId::new(font.size, mono_bold());
        let fmt = |fg: Color32, bg: Color32| TextFormat {
            font_id: font.clone(),
            color: fg,
            background: bg,
            ..Default::default()
        };
        if self.s.show_ts {
            job.append(&l.ts.format("%H:%M:%S%.3f").to_string(), 0.0, fmt(TEXT3(), Color32::TRANSPARENT));
            job.append("  ", 0.0, fmt(TEXT3(), Color32::TRANSPARENT));
        }
        match l.dir {
            Dir::Tx => job.append(
                "TX › ",
                0.0,
                TextFormat {
                    font_id: bold_font.clone(),
                    color: VIOLET(),
                    ..Default::default()
                },
            ),
            Dir::Sys => job.append("◆ ", 0.0, fmt(ACCENT(), Color32::TRANSPARENT)),
            Dir::Rx => {}
        }
        let hex = || {
            let mut h = String::with_capacity(l.raw.len() * 3);
            for b in &l.raw {
                h.push_str(&format!("{b:02X} "));
            }
            h
        };
        let hex_color = if l.dir == Dir::Tx { VIOLET() } else { ink(Color32::from_rgb(0x93, 0xc5, 0xfd)) };
        if l.dir != Dir::Sys && self.s.view == ViewMode::Hex {
            job.append(&hex(), 0.0, fmt(hex_color, Color32::TRANSPARENT));
            return job;
        }
        self.text_sections(&mut job, l, font, &bold_font);
        if l.dir != Dir::Sys && self.s.view == ViewMode::Mixed {
            // 混合视图：先文本，后面跟着暗色的 HEX，长行也一眼能看到文字
            job.append("   ", 0.0, fmt(TEXT3(), Color32::TRANSPARENT));
            job.append(&hex(), 0.0, fmt(hex_color.gamma_multiply(0.55), Color32::TRANSPARENT));
        }
        job
    }

    fn text_sections(&self, job: &mut LayoutJob, l: &Line, font: &FontId, bold_font: &FontId) {
        let text = &l.plain;
        let n = text.len();
        if n == 0 {
            return;
        }
        let base = self.text_color(l);
        let mut fg = vec![base; n];
        let mut bg = vec![Color32::TRANSPARENT; n];
        let mut strong = vec![false; n];
        let ansi_colored = self.s.ansi && l.spans.iter().any(|s| s.fg.is_some());
        if self.s.ansi {
            for s in &l.spans {
                for i in s.start..s.end.min(n) {
                    if let Some(c) = s.fg {
                        fg[i] = ink(c);
                    }
                    if let Some(c) = s.bg {
                        bg[i] = c;
                    }
                    strong[i] |= s.bold;
                }
            }
        }
        // 级别标记本身（[ERROR]、E (12)、<inf>）用级别色加粗
        if self.s.level_color && !ansi_colored && l.dir == Dir::Rx {
            if let (Some((a, b)), Some(c)) = (l.level_span, level_color(l.level)) {
                if b <= n && text.is_char_boundary(a) && text.is_char_boundary(b) {
                    fg[a..b].fill(c);
                    strong[a..b].fill(true);
                }
            }
        }
        for (kw, col) in &self.keywords {
            for (a, b) in find_all_ci(text, kw) {
                fg[a..b].fill(*col);
                bg[a..b].fill(col.gamma_multiply(0.16));
            }
        }
        if !self.search.is_empty() {
            for (a, b) in find_all_ci(text, &self.search) {
                fg[a..b].fill(FIND_FG());
                bg[a..b].fill(FIND_BG());
            }
        }
        let mut start = 0;
        for (i, _) in text.char_indices().skip(1).chain(std::iter::once((n, ' '))) {
            if i == n || fg[i] != fg[start] || bg[i] != bg[start] || strong[i] != strong[start] {
                let f = if strong[start] { bold_font.clone() } else { font.clone() };
                job.append(
                    &text[start..i],
                    0.0,
                    TextFormat {
                        font_id: f,
                        color: fg[start],
                        background: bg[start],
                        ..Default::default()
                    },
                );
                start = i;
            }
        }
    }

    // ------------------------------------------------------------ 侧边栏

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        brand(ui, &self.logo);
        ui.add_space(14.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 0,
                    right: 10,
                    top: 0,
                    bottom: 0,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let busy = self.conn.is_some() || self.autobaud.is_some();

                    // ---- 连接方式
                    let mut kind = if self.s.transport == Transport::TcpServer { Transport::TcpClient } else { self.s.transport };
                    ui.add_enabled_ui(!busy, |ui| {
                        tab_strip(
                            ui,
                            &mut kind,
                            &[(Transport::Serial, t("串口")), (Transport::TcpClient, "TCP"), (Transport::Udp, "UDP"), (Transport::Mqtt, "MQTT")],
                            30.0,
                        );
                    });
                    if kind != Transport::TcpClient || self.s.transport != Transport::TcpServer {
                        if kind != self.s.transport {
                            self.reconnect = None;
                            self.last_error = None;
                        }
                        self.s.transport = kind;
                    }
                    ui.add_space(12.0);
                    if self.s.transport != Transport::Serial {
                        self.net_form(ui, busy);
                        ui.add_space(10.0);
                        self.connect_button(ui);
                        ui.add_space(16.0);
                        eyebrow(ui, t("链路"), &self.uptime());
                        self.link_card(ui);
                        ui.add_space(16.0);
                    } else {
                        // ---- 设备
                        eyebrow(ui, t("设备"), &tf("{} 个", &[&self.ports.len()]));
                        if self.ports.is_empty() {
                            empty_port_card(ui);
                        }
                        for p in self.ports.clone() {
                            let selected = p.name == self.s.port;
                            let live = self.connected.as_deref() == Some(p.name.as_str());
                            if port_card(ui, &p, selected, live, !busy).clicked() && !busy {
                                self.s.port = p.name.clone();
                            }
                        }
                        ui.add_space(12.0);

                        // ---- 波特率
                        eyebrow(ui, t("波特率"), "");
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal(|ui| {
                                let w = ui.available_width() - 80.0;
                                let r = ui.add(
                                    egui::TextEdit::singleline(&mut self.baud_text)
                                        .desired_width(w - 34.0)
                                        .font(FontId::new(14.0, FontFamily::Monospace))
                                        .margin(vec2(8.0, 5.0)),
                                );
                                if r.changed() {
                                    if let Ok(b) = self.baud_text.trim().parse::<u32>() {
                                        if b > 0 {
                                            self.s.cfg.baud = b;
                                        }
                                    }
                                }
                                if r.lost_focus() {
                                    self.baud_text = self.s.cfg.baud.to_string();
                                }
                                ui.spacing_mut().item_spacing.x = 4.0;
                                egui::ComboBox::from_id_salt("baud").width(22.0).height(400.0).selected_text("").show_ui(ui, |ui| {
                                    for &b in COMMON_BAUDS {
                                        if ui.selectable_label(self.s.cfg.baud == b, RichText::new(b.to_string()).monospace()).clicked() {
                                            self.s.cfg.baud = b;
                                            self.baud_text = b.to_string();
                                        }
                                    }
                                });
                                ui.spacing_mut().item_spacing.x = 8.0;
                                let auto = egui::Button::new(RichText::new(t("⚡ 自动")).color(ACCENT()))
                                    .fill(ACCENT().gamma_multiply(0.08))
                                    .stroke(st(1.0, ACCENT().gamma_multiply(0.35)))
                                    .min_size(vec2(ui.available_width(), 28.0));
                                if ui
                                    .add_enabled(!self.s.port.is_empty() && self.autobaud.is_none(), auto)
                                    .on_hover_text(t("自动识别波特率：依次尝试 18 种常用波特率，选出能解出正常文字的那个。\n需要设备此时在输出日志。"))
                                    .clicked()
                                {
                                    self.start_autobaud();
                                }
                            });
                        });
                        ui.label(
                            RichText::new(tf("{}  ·  流控 {}", &[&self.s.cfg.short(), &[t("无"), t("软件"), t("硬件")][self.s.cfg.flow.min(2) as usize]]))
                                .small()
                                .color(TEXT3()),
                        );
                        ui.add_space(10.0);

                        // ---- 连接按钮
                        self.connect_button(ui);
                        ui.add_space(16.0);

                        // ---- 链路
                        eyebrow(ui, t("链路"), &self.uptime());
                        self.link_card(ui);
                        ui.add_space(16.0);
                    }

                    // ---- 显示
                    eyebrow(ui, t("显示"), "");
                    let old = self.s.encoding;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("编码")).color(TEXT2()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            egui::ComboBox::from_id_salt("enc").width(130.0).selected_text(self.s.encoding.name()).show_ui(ui, |ui| {
                                for e in Encoding::ALL {
                                    ui.selectable_value(&mut self.s.encoding, e, e.name());
                                }
                            });
                        });
                    });
                    if old != self.s.encoding {
                        self.buf.rerender(self.s.encoding);
                        self.view_dirty = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("视图")).color(TEXT2()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            segmented(ui, &mut self.s.view, &[(ViewMode::Text, t("文本")), (ViewMode::Hex, "HEX"), (ViewMode::Mixed, t("混合"))]);
                        });
                    });
                    switch_row(ui, &mut self.s.show_ts, t("时间戳"), t("每行前显示接收时间（毫秒）"));
                    switch_row(ui, &mut self.s.show_ln, t("行号"), t("左侧显示行号"));
                    if switch_row(ui, &mut self.s.show_tx, t("发送回显"), t("在日志里显示自己发送的数据")) {
                        self.view_dirty = true;
                    }
                    switch_row(ui, &mut self.s.ansi, t("ANSI 颜色"), t("解析终端颜色转义码（\\x1b[31m 等）"));
                    switch_row(ui, &mut self.s.level_color, t("级别着色"), t("ERROR / WARN 整行上色，级别标记高亮"));
                    switch_row(ui, &mut self.auto_scroll, t("自动滚动"), t("新数据到来时自动滚到底部；往上翻时自动停住"));
                    ui.add_space(4.0);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.s.keywords)
                            .hint_text(t("高亮关键词，逗号分隔"))
                            .desired_width(f32::INFINITY)
                            .margin(vec2(8.0, 5.0)),
                    );
                    if r.changed() {
                        self.parse_keywords();
                    }
                    if !self.keywords.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                            for (k, c) in &self.keywords {
                                pill(ui, k, *c);
                            }
                        });
                    }
                    ui.add_space(12.0);

                    // ---- 高级
                    let id = ui.make_persistent_id("adv");
                    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, self.s.advanced_open);
                    let open = state.is_open();
                    let r = ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        let c = rect.center();
                        let pts = if open {
                            vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.5), c + vec2(4.0, -2.0)]
                        } else {
                            vec![c + vec2(-2.0, -4.0), c + vec2(2.5, 0.0), c + vec2(-2.0, 4.0)]
                        };
                        ui.painter().add(Shape::line(pts, st(1.4, TEXT2())));
                        ui.add(egui::Label::new(RichText::new(t("高级设置")).color(TEXT2())).sense(Sense::click()))
                    });
                    let r = r.inner | r.response.interact(Sense::click());
                    if r.clicked() {
                        state.toggle(ui);
                        self.s.advanced_open = !open;
                    }
                    state.store(ui.ctx());
                    if open {
                        self.advanced(ui);
                    }
                    ui.add_space(8.0);
                });
        });
    }

    // ------------------------------------------------------------ 网络连接表单

    fn net_form(&mut self, ui: &mut egui::Ui, busy: bool) {
        let connected = self.connected.is_some();
        match self.s.transport {
            Transport::TcpClient | Transport::TcpServer => {
                ui.add_enabled_ui(!busy, |ui| {
                    tab_strip(ui, &mut self.s.transport, &[(Transport::TcpClient, t("客户端")), (Transport::TcpServer, t("服务器"))], 26.0);
                    ui.add_space(8.0);
                    let n = &mut self.s.net;
                    card(ui, |ui| {
                        if self.s.transport == Transport::TcpClient {
                            form_row(ui, t("地址"), |ui| text_field(ui, &mut n.tcp_host, t("IP 或域名")));
                            form_row(ui, t("端口"), |ui| num_field(ui, "tcp_port", &mut n.tcp_port));
                        } else {
                            form_row(ui, t("监听端口"), |ui| num_field(ui, "srv_port", &mut n.server_port));
                            let ip = local_ip();
                            form_row(ui, t("本机地址"), |ui| {
                                let tv = format!("{ip}:{}", n.server_port);
                                ui.label(RichText::new(&tv).font(FontId::new(12.5, FontFamily::Monospace)).color(ACCENT()));
                                if ui.small_button(t("复制")).on_hover_text(t("复制给设备端填写")).clicked() {
                                    ui.ctx().copy_text(tv);
                                }
                            });
                        }
                    });
                });
                let hint = if self.s.transport == Transport::TcpClient {
                    t("连接设备或网络调试助手开的 TCP 服务")
                } else {
                    t("等设备连上来；多个客户端时发送内容会广播给所有人")
                };
                ui.label(RichText::new(hint).small().color(TEXT3()));
            }
            Transport::Udp => {
                ui.add_enabled_ui(!busy, |ui| {
                    let n = &mut self.s.net;
                    card(ui, |ui| {
                        form_row(ui, t("本地端口"), |ui| num_field(ui, "udp_local", &mut n.udp_local));
                        form_row(ui, t("目标地址"), |ui| text_field(ui, &mut n.udp_host, t("留空 = 回复给发送方")));
                        form_row(ui, t("目标端口"), |ui| num_field(ui, "udp_port", &mut n.udp_port));
                    });
                });
                ui.label(RichText::new(tf("本机 {}  ·  每个数据包单独一行，前面标出来源", &[&local_ip()])).small().color(TEXT3()));
            }
            Transport::Mqtt => {
                ui.add_enabled_ui(!busy, |ui| {
                    let m = &mut self.s.net.mqtt;
                    card(ui, |ui| {
                        form_row(ui, t("服务器"), |ui| {
                            let w = ui.available_width() - 26.0;
                            ui.add_sized(
                                vec2(w, 26.0),
                                egui::TextEdit::singleline(&mut m.host)
                                    .hint_text(t("broker 地址"))
                                    .font(FontId::new(13.0, FontFamily::Monospace))
                                    .margin(vec2(6.0, 4.0)),
                            );
                            ui.menu_button(RichText::new("⋯").color(TEXT2()), |ui| {
                                ui.label(RichText::new(t("公共测试服务器")).small().color(TEXT3()));
                                for (h, port) in [("broker.emqx.io", 1883), ("test.mosquitto.org", 1883), ("broker.hivemq.com", 1883), ("127.0.0.1", 1883)] {
                                    if ui.button(RichText::new(h).monospace()).clicked() {
                                        m.host = h.into();
                                        m.port = if m.tls { 8883 } else { port };
                                        ui.close_menu();
                                    }
                                }
                            });
                        });
                        form_row(ui, t("端口"), |ui| num_field(ui, "mq_port", &mut m.port));
                        form_row(ui, t("客户端 ID"), |ui| text_field(ui, &mut m.client_id, t("留空自动生成")));
                        form_row(ui, t("用户名"), |ui| text_field(ui, &mut m.user, t("可选")));
                        form_row(ui, t("密码"), |ui| {
                            ui.add_sized(
                                vec2(ui.available_width(), 26.0),
                                egui::TextEdit::singleline(&mut m.pass).password(true).hint_text(t("可选")).margin(vec2(6.0, 4.0)),
                            );
                        });
                        form_row(ui, t("心跳"), |ui| {
                            let mut k = m.keepalive;
                            num_field(ui, "mq_keep", &mut k);
                            m.keepalive = k.clamp(5, 3600);
                        });
                        let was = m.tls;
                        switch_row(ui, &mut m.tls, t("TLS 加密（mqtts）"), t("用 TLS 加密连接，端口一般是 8883"));
                        if m.tls != was {
                            if m.tls && m.port == 1883 {
                                m.port = 8883;
                            } else if !m.tls && m.port == 8883 {
                                m.port = 1883;
                            }
                        }
                    });
                });
                ui.add_space(10.0);

                // 订阅：连接后也能随时增删
                eyebrow(ui, t("订阅主题"), &tf("{} 个", &[&self.s.net.mqtt.subs.len()]));
                let mut remove = None;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for (i, t) in self.s.net.mqtt.subs.iter().enumerate() {
                        if topic_chip(ui, t, kw_colors()[i % 6]).clicked() {
                            remove = Some(i);
                        }
                    }
                });
                if let Some(i) = remove {
                    let t = self.s.net.mqtt.subs.remove(i);
                    if connected {
                        if let Some(c) = &self.conn {
                            let _ = c.cmd.send(Cmd::Unsubscribe(t));
                        }
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let label = t("订阅");
                    let bw = (ui.fonts(|f| f.layout_no_wrap(label.into(), FontId::proportional(13.0), TEXT()).size().x) + 20.0).max(50.0);
                    let w = ui.available_width() - bw - 8.0;
                    let r = ui.add_sized(
                        vec2(w, 26.0),
                        egui::TextEdit::singleline(&mut self.sub_input)
                            .hint_text(t("主题，如 sensor/+/temp"))
                            .font(FontId::new(12.5, FontFamily::Monospace))
                            .margin(vec2(6.0, 4.0)),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let add = ui.add_sized(vec2(bw, 26.0), egui::Button::new(label)).clicked();
                    let t = self.sub_input.trim().to_string();
                    if (enter || add) && !t.is_empty() {
                        if !self.s.net.mqtt.subs.contains(&t) {
                            self.s.net.mqtt.subs.push(t.clone());
                            if connected {
                                if let Some(c) = &self.conn {
                                    let _ = c.cmd.send(Cmd::Subscribe(t));
                                }
                            }
                        }
                        self.sub_input.clear();
                    }
                });
                ui.label(RichText::new(t("点主题标签可取消订阅；+ 匹配一层，# 匹配多层")).small().color(TEXT3()));
                ui.add_space(10.0);

                // 发布
                eyebrow(ui, t("发布"), "");
                let before = (self.s.net.mqtt.pub_topic.clone(), self.s.net.mqtt.qos, self.s.net.mqtt.retain);
                let m = &mut self.s.net.mqtt;
                card(ui, |ui| {
                    form_row(ui, t("主题"), |ui| text_field(ui, &mut m.pub_topic, t("发布到哪个主题")));
                    form_row(ui, "QoS", |ui| tab_strip(ui, &mut m.qos, &[(0, "0"), (1, "1"), (2, "2")], 24.0));
                    switch_row(ui, &mut m.retain, t("保留消息"), t("服务器保存最后一条，新订阅者一连上就能收到"));
                });
                ui.label(RichText::new(t("命令栏、快捷命令发出的内容都会发布到这个主题")).small().color(TEXT3()));
                let after = (m.pub_topic.clone(), m.qos, m.retain);
                if before != after && connected {
                    if let Some(c) = &self.conn {
                        let _ = c.cmd.send(Cmd::PubOptions(after.0, after.1, after.2));
                    }
                }
            }
            Transport::Serial => {}
        }
    }

    /// 第一次启动：窗口宽 = 屏幕宽 × 0.618，高 = 宽 × 0.618（黄金矩形），居中
    fn place_window(&mut self, ctx: &egui::Context) {
        let Some(screen) = ctx.input(|i| i.viewport().monitor_size) else {
            ctx.request_repaint(); // 屏幕尺寸下一帧才拿得到
            return;
        };
        let phi = 0.618;
        let mut w = screen.x * phi;
        let mut h = w * phi;
        if h > screen.y * 0.85 {
            h = screen.y * 0.85;
            w = h / phi;
        }
        let size = vec2(w.max(900.0).min(screen.x), h.max(560.0).min(screen.y));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2((screen.x - size.x) / 2.0, (screen.y - size.y) / 2.0)));
        self.s.window_placed = true;
    }

    fn uptime(&self) -> String {
        match self.connected_at {
            Some(t) => {
                let s = t.elapsed().as_secs();
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            }
            None => String::new(),
        }
    }

    fn connect_button(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
        let net = self.s.transport != Transport::Serial;
        let enabled = self.autobaud.is_none() && (net || !self.s.port.is_empty() || self.conn.is_some());
        let connecting = net && self.conn.is_some() && self.connected.is_none();
        let hov = resp.hovered() && enabled;
        let p = ui.painter();
        let tv = ui.input(|i| i.time) as f32;
        let r = CornerRadius::same(7);
        if let Some(b) = self.autobaud {
            p.rect(rect, r, BG2(), st(1.0, ACCENT().gamma_multiply(0.5)), egui::StrokeKind::Inside);
            // 扫描进度条动画
            let x = rect.left() + (tv * 0.8).fract() * rect.width();
            let bar = Rect::from_min_max(pos2((x - 40.0).max(rect.left()), rect.bottom() - 2.0), pos2(x.min(rect.right()), rect.bottom()));
            p.rect_filled(bar, 1.0, ACCENT());
            let txt = if b > 0 { tf("正在识别 {} …", &[&b]) } else { t("正在识别 …").into() };
            p.text(rect.center(), Align2::CENTER_CENTER, txt, FontId::new(13.5, bold()), ACCENT());
            ui.ctx().request_repaint();
        } else if connecting {
            p.rect(rect, r, BG2(), st(1.0, ACCENT().gamma_multiply(0.5)), egui::StrokeKind::Inside);
            let x = rect.left() + (tv * 0.8).fract() * rect.width();
            let bar = Rect::from_min_max(pos2((x - 40.0).max(rect.left()), rect.bottom() - 2.0), pos2(x.min(rect.right()), rect.bottom()));
            p.rect_filled(bar, 1.0, ACCENT());
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                if hov { t("取消") } else { t("正在连接 …") },
                FontId::new(13.5, bold()),
                if hov { ERR() } else { ACCENT() },
            );
            ui.ctx().request_repaint();
        } else if self.conn.is_some() {
            // 已连接：绿色呼吸光晕 + 描边按钮
            let glow = 0.5 + 0.5 * (tv * 2.0).sin();
            p.add(
                epaint::Shadow {
                    offset: [0, 0],
                    blur: 18,
                    spread: 0,
                    color: OK().gamma_multiply(0.10 + 0.08 * glow),
                }
                .as_shape(rect, r),
            );
            p.rect(
                rect,
                r,
                if hov { ERR().gamma_multiply(0.12) } else { BG2() },
                st(1.0, if hov { ERR() } else { OK().gamma_multiply(0.6) }),
                egui::StrokeKind::Inside,
            );
            let (txt, col) = if hov { (t("断开连接"), ERR()) } else { (t("● 已连接"), OK()) };
            p.text(rect.center(), Align2::CENTER_CENTER, txt, FontId::new(13.5, bold()), col);
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        } else if self.reconnect.is_some() {
            p.rect(rect, r, BG2(), st(1.0, WARN().gamma_multiply(0.6)), egui::StrokeKind::Inside);
            let wait = if net { t("等待自动重连…") } else { t("等待设备重新插入…") };
            p.text(rect.center(), Align2::CENTER_CENTER, if hov { t("取消重连") } else { wait }, FontId::new(13.5, bold()), WARN());
        } else {
            let fill = if !enabled {
                BG3()
            } else if hov {
                accent_hover()
            } else {
                ACCENT()
            };
            if enabled {
                p.add(
                    epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: ACCENT().gamma_multiply(if hov { 0.35 } else { 0.2 }),
                    }
                    .as_shape(rect, r),
                );
            }
            p.rect_filled(rect, r, fill);
            let tc = if enabled { ON_ACCENT() } else { TEXT3() };
            p.text(rect.center() - vec2(10.0, 0.0), Align2::CENTER_CENTER, t("连接"), FontId::new(14.0, bold()), tc);
            p.text(
                rect.right_center() - vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                "F5",
                FontId::new(11.0, FontFamily::Monospace),
                tc.gamma_multiply(0.6),
            );
        }
        if resp.clicked() && enabled {
            self.toggle_connect();
        }
        if hov {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    fn link_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                stat(ui, "RX", &human_bytes(self.rx_bytes), ACCENT());
                stat(ui, "TX", &human_bytes(self.tx_bytes), VIOLET());
                if let Some(n) = self.peers {
                    stat(ui, t("客户端"), &n.to_string(), OK());
                }
            });
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            sparkline(ui.painter(), rect, &self.rate_hist, ACCENT());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{}/s", human_bytes(self.rate as u64)))
                        .font(FontId::new(12.0, FontFamily::Monospace))
                        .color(TEXT()),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("60s").small().color(TEXT3()));
                });
            });
        });
    }

    fn advanced(&mut self, ui: &mut egui::Ui) {
        let busy = self.conn.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("pg").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new(t("数据位")).color(TEXT2()));
                combo_u8(ui, "db", &mut self.s.cfg.data_bits, &[(5, "5"), (6, "6"), (7, "7"), (8, "8")]);
                ui.end_row();
                ui.label(RichText::new(t("校验")).color(TEXT2()));
                combo_u8(ui, "par", &mut self.s.cfg.parity, &[(0, t("无 None")), (1, t("奇 Odd")), (2, t("偶 Even"))]);
                ui.end_row();
                ui.label(RichText::new(t("停止位")).color(TEXT2()));
                combo_u8(ui, "sb", &mut self.s.cfg.stop_bits, &[(1, "1"), (2, "2")]);
                ui.end_row();
                ui.label(RichText::new(t("流控")).color(TEXT2()));
                combo_u8(ui, "fc", &mut self.s.cfg.flow, &[(0, t("无")), (1, t("软件 XON/XOFF")), (2, t("硬件 RTS/CTS"))]);
                ui.end_row();
            });
        });
        if busy {
            ui.label(RichText::new(t("断开后可修改串口参数")).small().color(TEXT3()));
        }
        ui.add_space(4.0);
        if switch_row(ui, &mut self.s.dtr, "DTR", t("部分开发板（如 ESP32）用 DTR/RTS 控制复位和下载模式")) {
            if let Some(c) = &self.conn {
                let _ = c.cmd.send(Cmd::SetDtr(self.s.dtr));
            }
        }
        if switch_row(ui, &mut self.s.rts, "RTS", t("部分开发板（如 ESP32）用 DTR/RTS 控制复位和下载模式")) {
            if let Some(c) = &self.conn {
                let _ = c.cmd.send(Cmd::SetRts(self.s.rts));
            }
        }
        switch_row(ui, &mut self.s.auto_reconnect, t("拔出后自动重连"), t("设备断开后等它重新出现，自动连回去"));
        switch_row(ui, &mut self.s.follow_new_port, t("自动选中新设备"), t("插入新串口时自动切换过去"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("字号")).color(TEXT2()));
            if ui.add(egui::Slider::new(&mut self.s.font_size, 10.0..=22.0).step_by(0.5)).changed() {
                let fs = self.s.font_size;
                ui.ctx().all_styles_mut(|s| {
                    s.text_styles.insert(TextStyle::Monospace, FontId::new(fs, FontFamily::Monospace));
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("保留")).color(TEXT2()));
            if ui.add(egui::DragValue::new(&mut self.s.max_lines).range(1000..=2_000_000).speed(1000).suffix(t(" 行"))).changed() {
                self.buf.max_lines = self.s.max_lines;
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("空闲分帧")).color(TEXT2()));
            if ui
                .add(egui::DragValue::new(&mut self.s.frame_gap_ms).range(0..=5000).suffix(" ms"))
                .on_hover_text(t("两次数据间隔超过这个时间就另起一行（0 = 只按换行符分行）。\n看二进制协议时很好用。"))
                .changed()
            {
                self.buf.frame_gap_ms = self.s.frame_gap_ms;
            }
        });
    }

    // ------------------------------------------------------------ 顶部：搜索与筛选

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if icon_button(ui, Icon::Sidebar, t("侧边栏  Ctrl+B"), !self.s.sidebar).clicked() {
                self.s.sidebar = !self.s.sidebar;
            }
            let was = self.s.terminal;
            segmented(ui, &mut self.s.terminal, &[(true, t("终端")), (false, t("日志"))]);
            if self.s.terminal != was {
                self.term_focus_req = self.s.terminal;
            }
            ui.add_space(4.0);
            if self.s.terminal {
                self.term_header(ui);
                return;
            }
            // 搜索框
            let search_w = (ui.available_width() * 0.2).clamp(130.0, 260.0);
            egui::Frame::new()
                .fill(BG2())
                .stroke(st(
                    1.0,
                    if ui.memory(|m| m.has_focus(egui::Id::new("search"))) {
                        ACCENT().gamma_multiply(0.6)
                    } else {
                        LINE()
                    },
                ))
                .corner_radius(R_WIDGET)
                .inner_margin(egui::Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        draw_icon(ui.painter(), r, Icon::Search, TEXT3());
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .id(egui::Id::new("search"))
                                .frame(false)
                                .hint_text(t("搜索日志   Ctrl+F"))
                                .desired_width(search_w),
                        );
                        if self.focus_search {
                            r.request_focus();
                            self.focus_search = false;
                        }
                        if r.changed() {
                            self.search_hit = None;
                            if self.filter_only {
                                self.view_dirty = true;
                            }
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            let back = ui.input(|i| i.modifiers.shift);
                            self.find_next(back);
                            r.request_focus();
                        }
                        r.on_hover_text(t("回车：下一个    Shift+回车：上一个"));
                        if !self.search.is_empty() {
                            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                            draw_icon(ui.painter(), r, Icon::Close, if resp.hovered() { TEXT() } else { TEXT3() });
                            if resp.on_hover_text(t("清空搜索")).clicked() {
                                self.search.clear();
                                self.search_hit = None;
                                self.view_dirty = true;
                            }
                        }
                    });
                });
            if toggle_chip(ui, &mut self.filter_only, t("只看匹配"), t("只显示包含搜索词的行")) {
                self.view_dirty = true;
            }
            ui.add_space(6.0);

            // 级别计数，点一下按级别筛选
            let c = self.buf.counts;
            let total: u64 = c.iter().sum();
            let lv = [
                (Level::None, t("全部"), total, TEXT2()),
                (Level::Error, "ERR", c[Level::Error as usize], ERR()),
                (Level::Warn, "WRN", c[Level::Warn as usize], WARN()),
                (Level::Info, "INF", c[Level::Info as usize], INFO()),
                (Level::Debug, "DBG", c[Level::Debug as usize], DEBUG()),
            ];
            ui.spacing_mut().item_spacing.x = 4.0;
            // 窗口窄时从右往左少显示几个级别，给右侧按钮留位置
            let room = ui.available_width() - 6.0 * 30.0 - 40.0;
            let fits = ((room / 78.0).floor() as usize).clamp(1, lv.len());
            for (l, name, n, col) in lv.into_iter().take(fits) {
                let on = self.s.min_level == l;
                let tip = if l == Level::None {
                    t("显示全部").to_string()
                } else {
                    tf("只看 {} 及更严重的级别", &[&name])
                };
                if level_pill(ui, name, n, col, on).on_hover_text(tip).clicked() {
                    self.s.min_level = if on && l != Level::None { Level::None } else { l };
                    self.view_dirty = true;
                }
            }
            ui.spacing_mut().item_spacing.x = 8.0;

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 右侧：操作按钮（从右往左）
                if icon_button(ui, Icon::Commands, t("快捷命令面板  Ctrl+J"), self.s.cmd_panel).clicked() {
                    self.s.cmd_panel = !self.s.cmd_panel;
                }
                ui.add_space(2.0);
                let (r, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
                ui.painter().vline(r.center().x, r.y_range(), st(1.0, LINE()));
                ui.add_space(2.0);
                let rec = self.record.is_some();
                if icon_button(ui, Icon::Record, if rec { t("停止录制") } else { t("录制：把之后收到的每一行实时写入文件") }, rec).clicked() {
                    self.toggle_record();
                }
                if icon_button(ui, Icon::Save, t("保存日志  Ctrl+S"), false).clicked() {
                    self.save();
                }
                if icon_button(ui, Icon::Copy, t("复制当前显示的全部日志"), false).clicked() {
                    let text = self.export_text();
                    ui.ctx().copy_text(text);
                    self.toast(t("已复制到剪贴板"), OK());
                }
                if icon_button(ui, Icon::Trash, t("清屏  Ctrl+L"), false).clicked() {
                    self.clear();
                }
                let paused = self.paused;
                if icon_button(
                    ui,
                    if paused { Icon::Play } else { Icon::Pause },
                    if paused {
                        t("继续  Ctrl+P")
                    } else {
                        t("暂停（后台继续接收，不丢数据）  Ctrl+P")
                    },
                    paused,
                )
                .clicked()
                {
                    self.toggle_pause();
                }
            });
        });
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused && !self.pause_buf.is_empty() {
            let d = std::mem::take(&mut self.pause_buf);
            self.buf.push_rx(&d, self.s.encoding);
        }
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.view_dirty = true;
        self.search_hit = None;
        self.sel = None;
        self.rx_bytes = 0;
        self.tx_bytes = 0;
    }

    fn export_text(&self) -> String {
        let (n, open) = self.rows();
        let mut out = String::new();
        for i in 0..n {
            if let Some(l) = self.buf.get(self.row_id(i, open)) {
                out.push_str(&format_line(l, self.s.view));
                out.push('\n');
            }
        }
        out
    }

    fn save(&mut self) {
        let name = chrono::Local::now().format("uart_%Y%m%d_%H%M%S.log").to_string();
        if let Some(path) = rfd::FileDialog::new().set_file_name(&name).add_filter(t("日志"), &["log", "txt"]).save_file() {
            match std::fs::write(&path, self.export_text()) {
                Ok(_) => self.toast(tf("已保存 {}", &[&path.display()]), OK()),
                Err(e) => self.toast(tf("保存失败：{}", &[&e]), ERR()),
            }
        }
    }

    fn toggle_record(&mut self) {
        if let Some((mut w, p)) = self.record.take() {
            let _ = w.flush();
            self.buf.sys(tf("录制结束：{}", &[&p.display()]));
            return;
        }
        let name = chrono::Local::now().format("uart_rec_%Y%m%d_%H%M%S.log").to_string();
        if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
            match File::create(&path) {
                Ok(f) => {
                    self.buf.sys(tf("开始录制到 {}", &[&path.display()]));
                    self.record = Some((BufWriter::new(f), path));
                }
                Err(e) => self.toast(tf("无法创建文件：{}", &[&e]), ERR()),
            }
        }
    }

    // ------------------------------------------------------------ 命令栏

    fn command_bar(&mut self, ui: &mut egui::Ui) {
        let focused = ui.memory(|m| m.has_focus(egui::Id::new("send")));
        egui::Frame::new()
            .fill(BG2())
            .stroke(st(1.0, if focused { VIOLET().gamma_multiply(0.7) } else { LINE() }))
            .corner_radius(R_CARD)
            .inner_margin(egui::Margin {
                left: 12,
                right: 6,
                top: 5,
                bottom: 5,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("›").font(FontId::new(18.0, mono_bold())).color(if self.conn.is_some() { VIOLET() } else { TEXT3() }));
                    let right_w = 360.0;
                    let w = (ui.available_width() - right_w).max(120.0);
                    let full = if self.s.hex_send {
                        t("HEX，例如 AA 55 01 0D 0A")
                    } else {
                        t("输入命令，回车发送    ↑↓ 历史    Ctrl+K 聚焦")
                    };
                    // 提示按段（四个空格分隔）截断，放不下的段整段去掉，不显示半截
                    let font = FontId::new(13.5, FontFamily::Monospace);
                    let mut hint = String::new();
                    for seg in full.split("    ") {
                        let cand = if hint.is_empty() { seg.to_string() } else { format!("{hint}    {seg}") };
                        if !hint.is_empty() && ui.fonts(|f| f.layout_no_wrap(cand.clone(), font.clone(), TEXT3()).size().x) > w - 8.0 {
                            break;
                        }
                        hint = cand;
                    }
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.send_text)
                            .id(egui::Id::new("send"))
                            .frame(false)
                            .hint_text(RichText::new(hint).color(TEXT3()))
                            .desired_width(w)
                            .font(FontId::new(13.5, FontFamily::Monospace)),
                    );
                    if self.focus_send {
                        r.request_focus();
                        self.focus_send = false;
                    }
                    if r.has_focus() {
                        let (up, down) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
                        let h = &self.s.history;
                        if up && !h.is_empty() {
                            let p = self.hist_pos.map_or(h.len() - 1, |p| p.saturating_sub(1));
                            self.hist_pos = Some(p);
                            self.send_text = h[p].clone();
                        } else if down {
                            if let Some(p) = self.hist_pos {
                                if p + 1 < h.len() {
                                    self.hist_pos = Some(p + 1);
                                    self.send_text = h[p + 1].clone();
                                } else {
                                    self.hist_pos = None;
                                    self.send_text.clear();
                                }
                            }
                        }
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        self.send_input(false);
                        self.send_text.clear();
                        r.request_focus();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let send = egui::Button::new(RichText::new(t("发送")).font(FontId::new(13.0, bold())).color(ON_ACCENT()))
                            .fill(if self.conn.is_some() { VIOLET() } else { BG3() })
                            .corner_radius(CornerRadius::same(6))
                            .min_size(vec2(64.0, 28.0));
                        let glow = ui.painter().add(Shape::Noop);
                        let r = ui.add_enabled(self.conn.is_some(), send);
                        if self.conn.is_some() {
                            let a = if r.hovered() { 0.45 } else { 0.25 };
                            ui.painter().set(
                                glow,
                                epaint::Shadow {
                                    offset: [0, 2],
                                    blur: 14,
                                    spread: 0,
                                    color: VIOLET().gamma_multiply(a),
                                }
                                .as_shape(r.rect, CornerRadius::same(6)),
                            );
                        }
                        if r.clicked() {
                            self.send_input(false);
                            self.send_text.clear();
                        }
                        ui.add(egui::DragValue::new(&mut self.s.repeat_ms).range(10..=3_600_000).suffix(" ms").speed(10));
                        if toggle_chip(ui, &mut self.repeat, t("定时"), t("按间隔重复发送输入框内容")) {
                            self.last_repeat = Instant::now() - Duration::from_secs(3600);
                        }
                        toggle_chip(ui, &mut self.s.hex_send, "HEX", t("按十六进制字节发送"));
                        ui.add_enabled_ui(!self.s.hex_send, |ui| {
                            egui::ComboBox::from_id_salt("le").width(64.0).selected_text(self.s.line_ending.name()).show_ui(ui, |ui| {
                                for le in [LineEnding::None, LineEnding::Lf, LineEnding::Cr, LineEnding::CrLf] {
                                    ui.selectable_value(&mut self.s.line_ending, le, le.name());
                                }
                            });
                        });
                    });
                });
            });
    }

    // ------------------------------------------------------------ 状态栏

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (col, text) = if self.autobaud.is_some() {
                (ACCENT(), t("正在识别波特率").to_string())
            } else if let Some(p) = &self.connected {
                if self.s.transport == Transport::Serial {
                    (OK(), format!("{p}  ·  {}  ·  {}", self.s.cfg.baud, self.s.cfg.short()))
                } else if let Some(n) = self.peers {
                    (OK(), tf("{}  ·  {} 个客户端", &[&p, &n]))
                } else {
                    (OK(), p.clone())
                }
            } else if self.conn.is_some() {
                (ACCENT(), tf("正在连接 {}", &[&self.s.net.label(self.s.transport)]))
            } else if let Some((p, _)) = &self.reconnect {
                if self.s.transport == Transport::Serial {
                    (WARN(), tf("等待 {} 重新插入", &[&p]))
                } else {
                    (WARN(), tf("等待重连 {}", &[&p]))
                }
            } else if self.last_error.is_some() {
                (ERR(), t("未连接").to_string())
            } else {
                (TEXT3(), t("未连接").to_string())
            };
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 5.0, col.gamma_multiply(0.2));
            ui.painter().circle_filled(rect.center(), 2.5, col);
            ui.label(RichText::new(text).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT2()));
            if self.conn.is_some() {
                // 收发指示灯：有数据时亮 120 ms，像硬件上的 RX/TX 灯
                ui.add_space(6.0);
                for (name, at, c) in [("RX", self.last_rx_at, ACCENT()), ("TX", self.last_tx_at, VIOLET())] {
                    let lit = at.is_some_and(|t| t.elapsed() < Duration::from_millis(120));
                    let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    if lit {
                        ui.painter().circle_filled(r.center(), 5.0, c.gamma_multiply(0.25));
                        ui.ctx().request_repaint_after(Duration::from_millis(120));
                    }
                    ui.painter().circle_filled(r.center(), 2.5, if lit { c } else { LINE_HI() });
                    ui.label(RichText::new(name).font(FontId::new(10.0, mono_bold())).color(if lit { c } else { TEXT3() }));
                }
            }
            if self.paused {
                ui.label(RichText::new(tf("已暂停，缓冲 {}", &[&human_bytes(self.pause_buf.len() as u64)])).small().color(WARN()));
            }
            if let Some((_, p)) = &self.record {
                let t = ui.input(|i| i.time);
                let blink = (t * 2.0) as i64 % 2 == 0;
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                ui.label(RichText::new("● REC").font(FontId::new(11.0, mono_bold())).color(if blink { ERR() } else { ERR().gamma_multiply(0.4) }))
                    .on_hover_text(p.display().to_string());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(TEXT3()));
                let r = ui
                    .add(egui::Label::new(RichText::new(t("⌨ 快捷键")).small().color(TEXT3())).sense(Sense::click()))
                    .on_hover_ui(|ui| shortcuts_help(ui, &self.km));
                if r.clicked() {
                    self.open_settings(prefs::Cat::Keys);
                }
                if let Some(p) = self.km.pending() {
                    ui.label(RichText::new(tf("已按下 {}，等待第二个按键…", &[&p])).small().color(ACCENT()));
                }
                ui.label(RichText::new(tf("{} 行", &[&group_digits(self.buf.lines.len() as u64)])).small().color(TEXT3()));
                ui.label(RichText::new(self.s.encoding.name()).small().color(TEXT3()));
            });
        });
    }

    // ------------------------------------------------------------ 日志区

    fn log_view(&mut self, ui: &mut egui::Ui) {
        let card_rect = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect(card_rect, R_CARD, BG0(), st(1.0, LINE()), egui::StrokeKind::Inside);
        // 顶部一道渐隐的青色细线，点到为止的科技感
        hgrad_line(&painter, card_rect.top() + 0.5, card_rect.x_range(), ACCENT().gamma_multiply(0.55));

        let font = TextStyle::Monospace.resolve(ui.style());
        let row_h = (ui.fonts(|f| f.row_height(&font)) + 4.0).round();
        let (n, open) = self.rows();

        if n == 0 {
            empty_state(ui, card_rect, self.ports.is_empty() && self.s.transport == Transport::Serial, self.conn.is_some(), &self.logo);
        }

        let inner = card_rect.shrink2(vec2(1.0, 6.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        child.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let digits = (self.buf.total().max(1) as f64).log10().floor() as usize + 1;
        let char_w = ui.fonts(|f| f.glyph_width(&font, '0'));
        let gutter = if self.s.show_ln { (digits.max(4) as f32) * char_w + 22.0 } else { 10.0 };

        let dragging = self.tsel.is_some_and(|s| s.drag);
        let mut sa = egui::ScrollArea::both()
            .auto_shrink([false, false])
            .stick_to_bottom(self.auto_scroll && self.scroll_to.is_none() && !dragging);
        if dragging && self.drag_scroll != 0.0 {
            sa = sa.vertical_scroll_offset((self.log_scroll_y + self.drag_scroll).max(0.0));
        }
        if let Some(id) = self.scroll_to.take() {
            if let Some(i) = self.row_index(id) {
                let y = (i as f32 * row_h - inner.height() / 3.0).max(0.0);
                sa = sa.vertical_scroll_offset(y);
            }
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let (pressed, down, ptr) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos()));
        let mut clicked: Option<u64> = None;
        let mut ctx_copy: Option<String> = None;
        let mut ctx_cmd: Option<u8> = None;
        let sel = self.sel.map(|(a, b)| (a.min(b), a.max(b)));
        let tsel = self.tsel.map(|s| s.ordered());
        let sel_fill = ui.visuals().selection.bg_fill;
        // 按下 / 拖动 / 双击落在哪一行哪个字
        let mut press_at: Option<(u64, usize)> = None;
        let mut drag_at: Option<(u64, usize)> = None;
        let mut word_at: Option<(u64, usize, usize)> = None;
        let mut vis_first: Option<(u64, usize)> = None;
        let mut vis_last: Option<(u64, usize)> = None;
        if self.jump_bottom {
            sa = sa.vertical_scroll_offset(n as f32 * row_h + inner.height());
            self.jump_bottom = false;
        }
        let now = chrono::Local::now();
        let mut fading = false;
        let out = sa.show_rows(&mut child, row_h, n, |ui, range| {
            let min_w = ui.available_width();
            for i in range {
                let id = self.row_id(i, open);
                let Some(l) = self.buf.get(id) else { continue };
                let job = self.line_job(l, &font);
                let galley = ui.fonts(|f| f.layout_job(job));
                let w = min_w.max(gutter + galley.size().x + 24.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click_and_drag());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let text_pos = pos2(rect.left() + gutter, rect.center().y - galley.size().y / 2.0);
                let nchars = galley.job.text.chars().count();
                let char_at = |x: f32| galley.cursor_from_pos(vec2(x - text_pos.x, galley.size().y / 2.0)).ccursor.index.min(nchars);
                if inner.y_range().contains(rect.center().y) {
                    vis_first.get_or_insert((id, 0));
                    vis_last = Some((id, nchars));
                }
                let in_text = ptr.is_some_and(|pp| pp.x >= text_pos.x - 4.0);
                if let Some(pp) = ptr {
                    if resp.hovered() && in_text {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                    }
                    if pressed && resp.hovered() && in_text {
                        press_at = Some((id, char_at(pp.x)));
                    }
                    if down && (rect.top()..rect.bottom()).contains(&pp.y) {
                        drag_at = Some((id, char_at(pp.x)));
                    }
                    if resp.double_clicked() && in_text {
                        let c = char_at(pp.x);
                        let chars: Vec<char> = galley.job.text.chars().collect();
                        let is_w = |ch: char| ch.is_alphanumeric() || ch == '_';
                        let (mut a, mut b) = (c.min(chars.len()), c.min(chars.len()));
                        while a > 0 && is_w(chars[a - 1]) {
                            a -= 1;
                        }
                        while b < chars.len() && is_w(chars[b]) {
                            b += 1;
                        }
                        if a < b {
                            word_at = Some((id, a, b));
                        }
                    }
                    if resp.triple_clicked() && in_text {
                        word_at = Some((id, 0, nchars));
                    }
                }
                let p = ui.painter();
                let hit = self.search_hit == Some(id);
                let selected = sel.is_some_and(|(a, b)| id >= a && id <= b);
                // 行底色：选中 > 搜索命中 > 错误/警告 > 悬停
                let bg = if selected {
                    Some(ACCENT().gamma_multiply(0.13))
                } else if hit {
                    Some(WARN().gamma_multiply(0.14))
                } else if self.s.level_color && l.dir == Dir::Rx && l.level == Level::Error {
                    Some(ERR().gamma_multiply(0.07))
                } else if self.s.level_color && l.dir == Dir::Rx && l.level == Level::Warn {
                    Some(WARN().gamma_multiply(0.045))
                } else if resp.hovered() {
                    Some(tint(6))
                } else {
                    // 新到的行淡入一下青色，看得出“正在来数据”
                    let age = (now - l.ts).num_milliseconds();
                    if l.dir == Dir::Rx && (0..500).contains(&age) {
                        fading = true;
                        Some(ACCENT().gamma_multiply(0.09 * (1.0 - age as f32 / 500.0)))
                    } else {
                        None
                    }
                };
                if let Some(c) = bg {
                    p.rect_filled(rect, 0.0, c);
                }
                // 左侧色条
                let bar = if selected {
                    Some(ACCENT())
                } else if hit {
                    Some(WARN())
                } else {
                    match l.dir {
                        Dir::Tx => Some(VIOLET().gamma_multiply(0.8)),
                        Dir::Sys => Some(ACCENT().gamma_multiply(0.5)),
                        Dir::Rx if self.s.level_color => match l.level {
                            Level::Error => Some(ERR()),
                            Level::Warn => Some(WARN()),
                            _ => None,
                        },
                        Dir::Rx => None,
                    }
                };
                if let Some(c) = bar {
                    p.rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), 0.0, c);
                }
                if self.s.show_ln {
                    let lc = if selected || resp.hovered() { TEXT2() } else { TEXT3().gamma_multiply(0.7) };
                    p.text(pos2(rect.left() + gutter - 12.0, rect.center().y), Align2::RIGHT_CENTER, (id + 1).to_string(), font.clone(), lc);
                }
                if let Some(((sa_id, sa_i), (sb_id, sb_i))) = tsel {
                    if id >= sa_id && id <= sb_id {
                        let a = if id == sa_id { sa_i.min(nchars) } else { 0 };
                        let b = if id == sb_id { sb_i.min(nchars) } else { nchars };
                        let xa = galley.pos_from_ccursor(egui::text::CCursor::new(a)).min.x;
                        let mut xb = galley.pos_from_ccursor(egui::text::CCursor::new(b)).min.x;
                        if id != sb_id {
                            xb += char_w * 0.6; // 换行也算选中
                        }
                        if xb > xa {
                            let r = Rect::from_min_max(pos2(text_pos.x + xa, rect.top() + 1.0), pos2(text_pos.x + xb, rect.bottom() - 1.0));
                            p.rect_filled(r, 2.0, sel_fill);
                        }
                    }
                }
                p.galley(text_pos, galley, LOG_TEXT());
                if resp.clicked() && !in_text {
                    clicked = Some(id);
                }
                resp.context_menu(|ui| {
                    let has_sel = tsel.is_some() || sel.is_some();
                    if has_sel && menu_item(ui, t("复制"), "Ctrl+C") {
                        ctx_cmd = Some(1);
                        ui.close_menu();
                    }
                    if has_sel && menu_item(ui, t("复制（含时间戳）"), "") {
                        ctx_cmd = Some(2);
                        ui.close_menu();
                    }
                    if menu_item(ui, t("复制此行"), "") {
                        ctx_copy = Some(if self.s.copy_ts { format_line(l, self.s.view) } else { l.plain.clone() });
                        ui.close_menu();
                    }
                    ui.separator();
                    if menu_item(ui, t("全选"), "Ctrl+A") {
                        ctx_cmd = Some(3);
                        ui.close_menu();
                    }
                });
            }
        });
        // 行号区：整行选择（Shift 扩展）
        if let Some(id) = clicked {
            self.tsel = None;
            self.sel = match self.sel {
                Some((a, _)) if shift => Some((a, id)),
                Some((a, b)) if a == id && b == id => None,
                _ => Some((id, id)),
            };
        }
        // 文字区：按字拖选
        if let Some(at) = press_at {
            self.sel = None;
            match self.tsel.as_mut() {
                Some(ts) if shift => {
                    ts.b = at;
                    ts.drag = true;
                }
                _ => self.tsel = Some(TSel { a: at, b: at, drag: true }),
            }
        }
        self.drag_scroll = 0.0;
        if let Some(ts) = self.tsel.as_mut().filter(|s| s.drag) {
            if down {
                if let Some(at) = drag_at {
                    ts.b = at;
                }
                // 拖出日志区上下边缘：自动滚动，离得越远滚得越快
                if let Some(pp) = ptr {
                    if pp.y < inner.top() {
                        self.drag_scroll = -((inner.top() - pp.y) * 0.5).clamp(4.0, 60.0);
                        if let Some(f) = vis_first {
                            ts.b = f;
                        }
                    } else if pp.y > inner.bottom() {
                        self.drag_scroll = ((pp.y - inner.bottom()) * 0.5).clamp(4.0, 60.0);
                        if let Some(l) = vis_last {
                            ts.b = l;
                        }
                    }
                }
                ui.ctx().request_repaint();
            } else {
                ts.drag = false;
                if ts.a == ts.b {
                    self.tsel = None;
                }
            }
        }
        if let Some((id, a, b)) = word_at {
            self.sel = None;
            self.tsel = Some(TSel { a: (id, a), b: (id, b), drag: false });
        }
        match ctx_cmd {
            Some(1) => self.copy_selection(ui.ctx(), false),
            Some(2) => self.copy_selection(ui.ctx(), true),
            Some(3) => self.select_all(),
            _ => {}
        }
        if let Some(tv) = ctx_copy {
            ui.ctx().copy_text(tv);
            self.toast(t("已复制"), OK());
        }
        if fading {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        self.log_scroll_y = out.state.offset.y;
        self.at_bottom = out.state.offset.y + out.inner_rect.height() >= out.content_size.y - row_h * 1.5;
        // 往上翻时出现「回到最新」浮动按钮
        if !self.at_bottom && n > 0 {
            let r = Rect::from_center_size(pos2(card_rect.center().x, card_rect.bottom() - 26.0), vec2(116.0, 30.0));
            let resp = ui.interact(r, ui.id().with("jump"), Sense::click());
            let p = ui.painter();
            p.add(
                epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: shade(170),
                }
                .as_shape(r, CornerRadius::same(15)),
            );
            p.rect(
                r,
                CornerRadius::same(15),
                if resp.hovered() { BG3() } else { BG2() },
                st(1.0, ACCENT().gamma_multiply(0.5)),
                egui::StrokeKind::Inside,
            );
            let a = pos2(r.left() + 20.0, r.center().y);
            p.add(Shape::line(vec![a + vec2(-4.0, -2.0), a + vec2(0.0, 2.5), a + vec2(4.0, -2.0)], st(1.5, ACCENT())));
            p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, t("回到最新"), FontId::new(12.5, bold()), TEXT());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.jump_bottom = true;
                self.auto_scroll = true;
                ui.ctx().request_repaint();
            }
        }
        hud_corners(&painter, card_rect.shrink(5.0), ACCENT().gamma_multiply(0.35));
    }
}

// ---------------------------------------------------------------- 终端

fn term_id() -> egui::Id {
    egui::Id::new("term-view")
}

impl App {
    fn term_header(&mut self, ui: &mut egui::Ui) {
        let s = &self.term.s;
        let mono = FontId::new(11.5, FontFamily::Monospace);
        ui.label(RichText::new(format!("{}×{}", s.cols, s.rows)).font(mono.clone()).color(TEXT3()))
            .on_hover_text(t("终端当前的列数 × 行数（随窗口大小变化）"));
        if s.in_alt() {
            pill(ui, t("全屏程序"), VIOLET());
        }
        if self.term_scroll > 0 {
            pill(ui, &tf("回滚 {} 行", &[&self.term_scroll]), WARN());
        }
        let hint = if self.conn.is_none() {
            t("先在左侧连接串口")
        } else if ui.memory(|m| m.has_focus(term_id())) {
            t("键盘直接发给设备  ·  Ctrl+` 回到日志")
        } else {
            t("点击下方终端区域开始输入")
        };
        ui.label(RichText::new(hint).size(12.0).color(TEXT3()));

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(ui, Icon::Commands, t("快捷命令面板"), self.s.cmd_panel).clicked() {
                self.s.cmd_panel = !self.s.cmd_panel;
            }
            ui.add_space(2.0);
            let (r, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), st(1.0, LINE()));
            ui.add_space(2.0);
            if icon_button(ui, Icon::Copy, t("复制整屏文本"), false).clicked() {
                let tv = self.term_screen_text();
                ui.ctx().copy_text(tv);
                self.toast(t("已复制整屏"), OK());
            }
            if icon_button(ui, Icon::Trash, t("清屏并重置终端（不影响设备）"), false).clicked() {
                self.term.reset();
                self.term_scroll = 0;
                self.term_sel = None;
                self.term_focus_req = true;
            }
            let b = egui::Button::new(RichText::new(t("同步窗口大小")).size(12.0).color(TEXT2()))
                .fill(BG2())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_WIDGET)
                .min_size(vec2(0.0, 24.0));
            let tip = t("把当前行列数告诉设备（发送 stty rows R cols C），\nvi、top、less 等全屏程序就能铺满窗口。\n只适用于 Linux shell，请在提示符下点击。");
            if ui.add_enabled(self.conn.is_some(), b).on_hover_text(tip).clicked() {
                let (c, r) = (self.term.s.cols, self.term.s.rows);
                self.send_raw(format!("stty rows {r} cols {c}\r").into_bytes());
                self.term_focus_req = true;
            }
        });
    }

    fn term_row(&self, abs: usize) -> Option<&term::Row> {
        let sb = &self.term.s.scrollback;
        if abs < sb.len() {
            sb.get(abs)
        } else {
            self.term.s.grid.get(abs - sb.len())
        }
    }

    fn term_screen_text(&self) -> String {
        let s = &self.term.s;
        let mut lines: Vec<String> = s.grid.iter().map(|r| term::row_text(r, 0, s.cols)).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn term_sel_text(&self) -> Option<String> {
        let (a, b) = self.term_sel?;
        let (a, b) = (a.min(b), a.max(b));
        let mut out = vec![];
        for abs in a.0..=b.0 {
            let row = self.term_row(abs)?;
            let from = if abs == a.0 { a.1 } else { 0 };
            let to = if abs == b.0 { b.1 + 1 } else { row.len() };
            out.push(term::row_text(row, from, to));
        }
        Some(out.join("\n"))
    }

    /// 键盘、粘贴 → 发给设备
    fn term_input(&mut self, ui: &egui::Ui) {
        let (events, shift, time) = ui.input(|i| (i.events.clone(), i.modifiers.shift, i.time));
        let app_cursor = self.term.s.app_cursor;
        let mut out = Vec::new();
        for e in events {
            match e {
                egui::Event::Text(t) => out.extend(self.s.encoding.encode(&t)),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(b) = term::key_bytes(key, modifiers, app_cursor) {
                        out.extend(b);
                    }
                }
                egui::Event::Copy => {
                    // Ctrl+Shift+C 复制选中；Ctrl+C 照常发 ^C 给设备
                    match self.term_sel_text().filter(|_| shift) {
                        Some(tv) => {
                            ui.ctx().copy_text(tv);
                            self.toast(t("已复制"), OK());
                        }
                        None => out.push(0x03),
                    }
                }
                egui::Event::Cut => out.push(0x18),
                egui::Event::Paste(p) => out.extend(self.s.encoding.encode(&p.replace('\n', "\r"))),
                _ => {}
            }
        }
        if !out.is_empty() {
            self.term_scroll = 0;
            self.term_sel = None;
            self.term_blink_at = time;
            self.send_raw(out);
        }
    }

    fn term_view(&mut self, ui: &mut egui::Ui) {
        let card_rect = ui.max_rect();
        let id = term_id();
        let resp = ui.interact(card_rect, id, Sense::click_and_drag());
        if self.term_focus_req {
            resp.request_focus();
            self.term_focus_req = false;
        }
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        if focused {
            // Tab、方向键、Esc 也交给终端，不用来切换焦点
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.term_input(ui);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }

        let painter = ui.painter_at(card_rect);
        painter.rect(
            card_rect,
            R_CARD,
            BG0(),
            st(1.0, if focused { ACCENT().gamma_multiply(0.35) } else { LINE() }),
            egui::StrokeKind::Inside,
        );
        hgrad_line(&painter, card_rect.top() + 0.5, card_rect.x_range(), ACCENT().gamma_multiply(if focused { 0.8 } else { 0.4 }));

        // 按可用空间算出行列数
        let font = TextStyle::Monospace.resolve(ui.style());
        let bold_font = FontId::new(font.size, mono_bold());
        // 用一长串字符量出实际步进（排版时会按像素取整，单个字宽乘列数会越画越偏）
        let cw = ui.fonts(|f| f.layout_no_wrap("0".repeat(100), font.clone(), TEXT()).size().x / 100.0);
        let rh = (ui.fonts(|f| f.row_height(&font)) + 2.0).round();
        let inner = card_rect.shrink2(vec2(12.0, 10.0));
        let cols = ((inner.width() - 10.0) / cw).floor().max(0.0) as usize;
        let rows = (inner.height() / rh).floor().max(0.0) as usize;
        if cols >= 10 && rows >= 3 && (cols, rows) != (self.term.s.cols, self.term.s.rows) {
            self.term.s.resize(cols, rows);
        }

        // 滚轮翻看历史
        let sbl = self.term.s.scrollback.len();
        if resp.hovered() {
            self.term_wheel += ui.input(|i| i.smooth_scroll_delta.y);
            let n = (self.term_wheel / rh).trunc();
            if n != 0.0 {
                self.term_wheel -= n * rh;
                self.term_scroll = (self.term_scroll as i64 + n as i64 * 3).clamp(0, sbl as i64) as usize;
            }
        }
        self.term_scroll = self.term_scroll.min(sbl);
        let top_abs = sbl - self.term_scroll;
        let (tcols, trows) = (self.term.s.cols, self.term.s.rows);
        let origin = inner.min;
        let cell_at = |p: egui::Pos2| {
            let c = ((p.x - origin.x) / cw).floor().clamp(0.0, (tcols - 1) as f32) as usize;
            let r = ((p.y - origin.y) / rh).floor().clamp(0.0, (trows - 1) as f32) as usize;
            (top_abs + r, c)
        };

        // 鼠标拖动选择，松开自动复制
        if resp.drag_started() {
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                let c = cell_at(p);
                self.term_sel = Some((c, c));
            }
        }
        if resp.dragged() {
            if let (Some((a, _)), Some(p)) = (self.term_sel, resp.interact_pointer_pos()) {
                self.term_sel = Some((a, cell_at(p)));
            }
        }
        if resp.drag_stopped() {
            if let Some(tv) = self.term_sel_text().filter(|tv| !tv.trim().is_empty()) {
                ui.ctx().copy_text(tv);
                self.toast(t("已复制选中文本"), OK());
            }
        }
        if resp.clicked() {
            self.term_sel = None;
        }
        let mut paste = None;
        let mut reset = false;
        resp.context_menu(|ui| {
            if self.term_sel.is_some() && ui.button(t("复制选中")).clicked() {
                if let Some(t) = self.term_sel_text() {
                    ui.ctx().copy_text(t);
                }
                ui.close_menu();
            }
            if ui.button(t("粘贴")).clicked() {
                paste = Some(());
                ui.close_menu();
            }
            if ui.button(t("复制整屏")).clicked() {
                ui.ctx().copy_text(self.term_screen_text());
                ui.close_menu();
            }
            ui.separator();
            if ui.button(t("清屏并重置终端")).clicked() {
                reset = true;
                ui.close_menu();
            }
        });
        if paste.is_some() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            self.term_focus_req = true;
        }
        if reset {
            self.term.reset();
            self.term_scroll = 0;
            self.term_sel = None;
        }

        // 画屏幕
        let sel = self.term_sel.map(|(a, b)| (a.min(b), a.max(b)));
        let sel_bg = ACCENT().gamma_multiply(0.28);
        let mut blank = sbl == 0;
        for r in 0..trows {
            let abs = top_abs + r;
            let Some(row) = self.term_row(abs) else { continue };
            let y = origin.y + r as f32 * rh;
            let mut run = String::new();
            let mut run_x = 0usize;
            let mut run_key: Option<(Color32, bool)> = None;
            let flush = |run: &mut String, run_x: usize, key: Option<(Color32, bool)>| {
                if let Some((c, b)) = key {
                    if !run.trim().is_empty() {
                        let f = if b { &bold_font } else { &font };
                        painter.text(pos2(origin.x + run_x as f32 * cw, y + rh / 2.0), Align2::LEFT_CENTER, run.as_str(), f.clone(), c);
                    }
                }
                run.clear();
            };
            for (c, cell) in row.iter().enumerate() {
                if cell.wide_tail {
                    continue;
                }
                let wide = term::is_wide(cell.ch);
                let w = if wide { 2.0 } else { 1.0 };
                let st_ = cell.style;
                let (mut fg, mut bg) = (st_.fg.unwrap_or(if st_.bold { TEXT() } else { LOG_TEXT() }), st_.bg);
                if st_.inverse {
                    (fg, bg) = (bg.unwrap_or(BG0()), Some(fg));
                }
                if bg.is_none() {
                    fg = ink(fg);
                }
                let in_sel = sel.is_some_and(|(a, b)| (abs, c) >= a && (abs, c) <= b);
                let cell_rect = Rect::from_min_size(pos2(origin.x + c as f32 * cw, y), vec2(cw * w, rh));
                if let Some(b) = bg {
                    painter.rect_filled(cell_rect, 0.0, b);
                }
                if in_sel {
                    painter.rect_filled(cell_rect, 0.0, sel_bg);
                }
                if cell.ch != ' ' {
                    blank = false;
                }
                if st_.underline {
                    painter.hline(cell_rect.x_range(), cell_rect.bottom() - 1.5, st(1.0, fg));
                }
                // ASCII 连成一段画；中文、线框等非 ASCII 字符按格子单独放，保证对齐
                if cell.ch.is_ascii() {
                    let key = Some((fg, st_.bold));
                    if key != run_key {
                        flush(&mut run, run_x, run_key);
                        run_key = key;
                        run_x = c;
                    }
                    run.push(cell.ch);
                } else {
                    flush(&mut run, run_x, run_key);
                    run_key = None;
                    if !draw_block_char(&painter, cell_rect, cell.ch, fg) {
                        let f = if st_.bold { &bold_font } else { &font };
                        painter.text(cell_rect.center(), Align2::CENTER_CENTER, cell.ch, f.clone(), fg);
                    }
                }
            }
            flush(&mut run, run_x, run_key);
        }

        // 光标：获得焦点时闪烁的实心块，失去焦点时空心框
        let s = &self.term.s;
        if self.term_scroll == 0 && s.cursor_visible {
            let wide = s.grid.get(s.y).and_then(|r| r.get(s.x)).is_some_and(|c| term::is_wide(c.ch));
            let cr = Rect::from_min_size(pos2(origin.x + s.x as f32 * cw, origin.y + s.y as f32 * rh), vec2(cw * if wide { 2.0 } else { 1.0 }, rh));
            if focused {
                let t = ui.input(|i| i.time) - self.term_blink_at;
                let phase = t % 1.06;
                if phase < 0.6 {
                    painter.rect_filled(cr, 1.0, ACCENT().gamma_multiply(0.85));
                    if let Some(c) = s.grid.get(s.y).and_then(|r| r.get(s.x)).filter(|c| c.ch != ' ') {
                        painter.text(cr.center(), Align2::CENTER_CENTER, c.ch, font.clone(), ON_ACCENT());
                    }
                }
                let next = if phase < 0.6 { 0.6 - phase } else { 1.06 - phase };
                ui.ctx().request_repaint_after(Duration::from_secs_f64(next.max(0.02)));
            } else {
                painter.rect_stroke(cr.shrink(0.5), 1.0, st(1.0, ACCENT().gamma_multiply(0.6)), egui::StrokeKind::Inside);
            }
        }

        // 空屏提示
        if blank {
            let (t1, t2) = if self.conn.is_none() {
                (t("终端模式"), t("先在左侧连接串口，然后在这里直接和设备交互"))
            } else if focused {
                (t("已连接"), t("直接打字，按回车唤出设备的提示符"))
            } else {
                (t("已连接"), t("点击这里，键盘输入会直接发给设备"))
            };
            let c = card_rect.center();
            painter.text(c - vec2(0.0, 12.0), Align2::CENTER_CENTER, t1, FontId::new(15.0, bold()), TEXT2());
            painter.text(c + vec2(0.0, 14.0), Align2::CENTER_CENTER, t2, FontId::proportional(12.5), TEXT3());
        }

        // 回滚条
        if sbl > 0 {
            let total = (sbl + trows) as f32;
            let track = Rect::from_min_max(pos2(card_rect.right() - 7.0, inner.top()), pos2(card_rect.right() - 4.0, inner.bottom()));
            let h = (trows as f32 / total * track.height()).max(16.0);
            let top = track.top() + top_abs as f32 / sbl as f32 * (track.height() - h);
            let thumb = Rect::from_min_size(pos2(track.left(), top), vec2(track.width(), h));
            painter.rect_filled(thumb, 1.5, if self.term_scroll > 0 { ACCENT().gamma_multiply(0.6) } else { LINE_HI() });
        }
        if self.term_scroll > 0 {
            let r = Rect::from_center_size(pos2(card_rect.center().x, card_rect.bottom() - 26.0), vec2(116.0, 30.0));
            let jr = ui.interact(r, id.with("jump"), Sense::click());
            let p = ui.painter();
            p.add(
                epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: shade(170),
                }
                .as_shape(r, CornerRadius::same(15)),
            );
            p.rect(
                r,
                CornerRadius::same(15),
                if jr.hovered() { BG3() } else { BG2() },
                st(1.0, ACCENT().gamma_multiply(0.5)),
                egui::StrokeKind::Inside,
            );
            let a = pos2(r.left() + 20.0, r.center().y);
            p.add(Shape::line(vec![a + vec2(-4.0, -2.0), a + vec2(0.0, 2.5), a + vec2(4.0, -2.0)], st(1.5, ACCENT())));
            p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, t("回到最新"), FontId::new(12.5, bold()), TEXT());
            if jr.clicked() {
                self.term_scroll = 0;
                self.term_focus_req = true;
            }
        }
        hud_corners(&painter, card_rect.shrink(5.0), ACCENT().gamma_multiply(0.35));
    }
}

/// 线框和方块字符自己画：字体里的线框字宽度不一，画出来对不齐
fn draw_block_char(p: &egui::Painter, r: Rect, ch: char, c: Color32) -> bool {
    let (l, rt, u, d) = match ch {
        '─' | '━' | '═' => (1, 1, 0, 0),
        '│' | '┃' | '║' => (0, 0, 1, 1),
        '┌' | '┏' | '╔' | '╭' => (0, 1, 0, 1),
        '┐' | '┓' | '╗' | '╮' => (1, 0, 0, 1),
        '└' | '┗' | '╚' | '╰' => (0, 1, 1, 0),
        '┘' | '┛' | '╝' | '╯' => (1, 0, 1, 0),
        '├' | '┣' | '╠' => (0, 1, 1, 1),
        '┤' | '┫' | '╣' => (1, 0, 1, 1),
        '┬' | '┳' | '╦' => (1, 1, 0, 1),
        '┴' | '┻' | '╩' => (1, 1, 1, 0),
        '┼' | '╋' | '╬' => (1, 1, 1, 1),
        '█' => return fill(p, r, c),
        '▀' => return fill(p, Rect::from_min_max(r.min, pos2(r.right(), r.center().y)), c),
        '▄' => return fill(p, Rect::from_min_max(pos2(r.left(), r.center().y), r.max), c),
        '▌' => return fill(p, Rect::from_min_max(r.min, pos2(r.center().x, r.bottom())), c),
        '▐' => return fill(p, Rect::from_min_max(pos2(r.center().x, r.top()), r.max), c),
        '░' => return fill(p, r, c.gamma_multiply(0.25)),
        '▒' => return fill(p, r, c.gamma_multiply(0.5)),
        '▓' => return fill(p, r, c.gamma_multiply(0.75)),
        _ => return false,
    };
    let m = pos2(r.center().x.round() + 0.5, r.center().y.round() + 0.5);
    let s = st(1.0, c);
    if l == 1 {
        p.hline(r.left()..=m.x, m.y, s);
    }
    if rt == 1 {
        p.hline(m.x..=r.right(), m.y, s);
    }
    if u == 1 {
        p.vline(m.x, r.top()..=m.y, s);
    }
    if d == 1 {
        p.vline(m.x, m.y..=r.bottom(), s);
    }
    true
}

fn fill(p: &egui::Painter, r: Rect, c: Color32) -> bool {
    p.rect_filled(r, 0.0, c);
    true
}

impl App {
    /// 调试页的命令（按键已经由 Keymap 翻译好）
    fn shortcuts(&mut self, ctx: &egui::Context, acts: &[Act], term_focused: bool) {
        for a in acts {
            match *a {
                Act::ToggleTerminal => {
                    self.s.terminal = !self.s.terminal;
                    self.term_focus_req = self.s.terminal;
                }
                Act::Connect => {
                    if self.autobaud.is_none() {
                        self.toggle_connect();
                    }
                }
                Act::LogFind => {
                    if !self.s.terminal {
                        self.focus_search = true;
                    }
                }
                Act::LogClear => self.clear(),
                Act::LogSave => self.save(),
                Act::LogPause => self.toggle_pause(),
                Act::LogSelectAll => self.select_all(),
                Act::FocusCmdBar => self.focus_send = true,
                Act::ToggleSidebar => self.s.sidebar = !self.s.sidebar,
                Act::ToggleCmdPanel => self.s.cmd_panel = !self.s.cmd_panel,
                Act::Quick(n) => {
                    let c = self.s.groups.get(self.s.cur_group).and_then(|g| g.cmds.get(n as usize)).cloned();
                    if let Some(c) = c {
                        self.send_quick(&c);
                    }
                }
                _ => {}
            }
        }
        if term_focused {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) && !ctx.wants_keyboard_input() {
            self.sel = None;
            self.tsel = None;
        }
        // 没有输入框聚焦时，Ctrl+C 复制选中的内容
        let nothing_focused = ctx.memory(|m| m.focused().is_none());
        if nothing_focused && (self.sel.is_some() || self.tsel.is_some()) && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
            self.copy_selection(ctx, self.s.copy_ts);
        }
    }

    /// 和页面无关的命令：命令面板、设置、切换页面、主题…；返回剩下的交给页面处理
    fn run_global(&mut self, ctx: &egui::Context, acts: Vec<Act>) -> Vec<Act> {
        let mut rest = vec![];
        for a in acts {
            match a {
                Act::Palette => self.open_palette(),
                Act::Settings => self.open_settings(prefs::Cat::General),
                Act::Keymap => self.open_settings(prefs::Cat::Keys),
                Act::PageDebug => self.set_page(Page::Debug),
                Act::PageCode if self.page != Page::Code => self.set_page(Page::Code),
                Act::PageGit => self.set_page(Page::Git),
                Act::ThemeToggle => {
                    let light = !is_light();
                    self.set_theme(ctx, if light { "light" } else { "dark" });
                }
                Act::CheckUpdate => self.check_update(true),
                _ => rest.push(a),
            }
        }
        rest
    }

    pub(crate) fn set_theme(&mut self, ctx: &egui::Context, mode: &str) {
        self.s.theme = mode.to_string();
        apply(ctx, self.s.font_size, resolve(mode, ctx));
        self.parse_keywords();
    }

    /// 主题设成跟随系统时，系统切换深浅色后跟着变
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if self.s.theme == "system" && resolve("system", ctx) != is_light() {
            apply(ctx, self.s.font_size, !is_light());
            self.parse_keywords();
        }
    }

    fn open_settings(&mut self, cat: prefs::Cat) {
        self.prefs.cat = cat;
        self.set_page(Page::Settings);
    }

    fn debug_page(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("side")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 16,
                right: 14,
                top: 14,
                bottom: 8,
            }))
            .exact_width(276.0)
            .resizable(false)
            .show_animated(ctx, self.s.sidebar, |ui| self.sidebar(ui));
        let connected = self.conn.is_some();
        let seq = self.seq.as_ref().map(|(g, q, _)| (*g, q.len()));
        let act = egui::SidePanel::right("cmds")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 14,
                right: 14,
                top: 14,
                bottom: 8,
            }))
            .default_width(272.0)
            .width_range(220.0..=420.0)
            .resizable(true)
            .show_animated(ctx, self.s.cmd_panel, |ui| self.cmd_ui.ui(ui, &mut self.s.groups, &mut self.s.cur_group, connected, seq))
            .and_then(|r| r.inner);
        let acted = act.is_some();
        match act {
            Some(CmdAction::Send(c)) => self.send_quick(&c),
            Some(CmdAction::SendAll(gi)) => self.send_group(gi),
            None => {}
        }
        if acted && self.s.terminal {
            self.term_focus_req = true; // 点完快捷命令，键盘回到终端
        }
        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 12,
                right: 14,
                top: 10,
                bottom: 8,
            }))
            .show_separator_line(false)
            .show(ctx, |ui| self.header(ui));
        if !self.s.terminal {
            egui::TopBottomPanel::bottom("cmd")
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 12,
                    right: 14,
                    top: 8,
                    bottom: 10,
                }))
                .show_separator_line(false)
                .show(ctx, |ui| self.command_bar(ui));
        }
        let bottom = if self.s.terminal { 10 } else { 0 };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 12, right: 14, top: 0, bottom }))
            .show(ctx, |ui| if self.s.terminal { self.term_view(ui) } else { self.log_view(ui) });
    }
}

impl App {
    fn set_page(&mut self, p: Page) {
        if self.page == p {
            return;
        }
        if p == Page::Settings {
            self.prev_page = self.page;
        }
        self.page = p;
        match p {
            Page::Code => self.code.activate(),
            Page::Git => {
                let root = self.code.root().map(|r| r.to_path_buf());
                self.gitv.activate(root.as_deref());
            }
            Page::Debug => self.term_focus_req = self.s.terminal,
            Page::Settings => {}
        }
    }

    /// 最左侧的活动栏（类似 VS Code）：调试 / 代码 / Git，底部是语言和更新
    fn activity_bar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("activity")
            .frame(egui::Frame::new().fill(BG0()).inner_margin(egui::Margin { left: 0, right: 0, top: 8, bottom: 8 }))
            .exact_width(48.0)
            .resizable(false)
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                let live = self.conn.is_some();
                for (pg, icon, tip) in [
                    (Page::Debug, Icon::Pulse, t("调试：串口 / TCP / UDP / MQTT")),
                    (Page::Code, Icon::Code, t("代码查看：打开工程、跳转定义、宏展开")),
                    (Page::Git, Icon::Branch, t("Git：分支、提交图谱、差异")),
                ] {
                    let dot = (pg == Page::Debug && live).then_some(OK());
                    if activity_button(ui, icon, tip, self.page == pg, dot).clicked() {
                        self.set_page(pg);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    // 齿轮：设置、命令面板、主题、语言等不常用入口都收在这里
                    let r = activity_button(ui, Icon::Gear, t("管理"), self.page == Page::Settings, None);
                    let id = egui::Id::new("gear-menu");
                    if r.clicked() {
                        ui.memory_mut(|m| m.toggle_popup(id));
                    }
                    let mut pick: Option<Act> = None;
                    let mut theme: Option<&'static str> = None;
                    egui::popup::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                        ui.set_min_width(230.0);
                        for a in [Act::Palette, Act::Settings, Act::Keymap] {
                            let keys = self.km.label(a);
                            if menu_item(ui, t(keymap::def(a).label), &keys) {
                                pick = Some(a);
                            }
                        }
                        ui.separator();
                        ui.menu_button(t("界面主题"), |ui| {
                            for (m, name) in [("system", t("跟随系统")), ("dark", t("深色")), ("light", t("浅色"))] {
                                if lang_item(ui, name, self.s.theme == m) {
                                    theme = Some(m);
                                }
                            }
                        });
                        ui.menu_button(format!("{}  ·  Language", t("界面语言")), |ui| {
                            ui.set_min_width(180.0);
                            let cur = i18n::current();
                            let auto = self.s.lang == "auto";
                            let det = i18n::LANGS[i18n::detect()].1;
                            if lang_item(ui, &format!("{}  ({det})", t("跟随系统")), auto) {
                                self.s.lang = "auto".into();
                                i18n::apply("auto");
                                ui.close_menu();
                            }
                            ui.separator();
                            for (i, (code, name)) in i18n::LANGS.iter().enumerate() {
                                if lang_item(ui, name, !auto && cur == i) {
                                    self.s.lang = code.to_string();
                                    i18n::apply(code);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.separator();
                        if menu_item(ui, t("检查更新"), "") {
                            pick = Some(Act::CheckUpdate);
                        }
                        if menu_item(ui, t("关于"), "") {
                            self.open_settings(prefs::Cat::About);
                            ui.memory_mut(|m| m.close_popup());
                        }
                    });
                    if pick.is_some() || theme.is_some() {
                        ui.memory_mut(|m| m.close_popup());
                    }
                    if let Some(m) = theme {
                        self.set_theme(ui.ctx(), m);
                    }
                    if let Some(a) = pick {
                        self.pending_acts.push(a);
                    }
                    // 更新：只有在有事要处理时才出现
                    let tip = match &self.upd {
                        Upd::Idle => None,
                        Upd::Checking { .. } => Some((t("正在检查更新…").to_string(), TEXT3())),
                        Upd::Available(r) => Some((tf("发现新版本 v{}", &[&r.version]), ACCENT())),
                        Upd::Downloading(..) => Some((t("正在下载更新…").to_string(), ACCENT())),
                        Upd::Ready(..) => Some((t("更新已下载，点击安装").to_string(), OK())),
                        Upd::Failed(..) => Some((t("更新失败").to_string(), ERR())),
                    };
                    if let Some((tip, dot)) = tip {
                        if activity_button(ui, Icon::Download, &tip, false, Some(dot)).clicked() && !matches!(self.upd, Upd::Checking { .. }) {
                            self.upd_card = true;
                        }
                    }
                });
            });
    }

    fn check_update(&mut self, manual: bool) {
        if matches!(self.upd, Upd::Checking { .. } | Upd::Downloading(..)) {
            return;
        }
        self.upd = Upd::Checking { manual };
        if manual {
            self.toast(t("正在检查更新…"), ACCENT());
        }
        update::check(self.upd_tx.clone(), self.ctx.clone());
    }

    fn poll_update(&mut self, ctx: &egui::Context) {
        if let Some(at) = self.upd_check_at {
            let now = Instant::now();
            if now >= at {
                self.upd_check_at = None;
                self.check_update(false);
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        while let Ok(e) = self.upd_rx.try_recv() {
            match e {
                UpdEvt::Checked(r) => {
                    let manual = matches!(self.upd, Upd::Checking { manual: true });
                    self.upd = Upd::Idle;
                    match r {
                        Ok(Some(rel)) => {
                            if manual || rel.force || rel.version != self.s.skip_version {
                                self.upd = Upd::Available(rel);
                                self.upd_card = true;
                            }
                        }
                        Ok(None) if manual => self.toast(tf("已是最新版本 v{}", &[&env!("CARGO_PKG_VERSION")]), OK()),
                        Err(e) if manual => self.toast(tf("检查更新失败：{}", &[&e]), ERR()),
                        _ => {}
                    }
                }
                UpdEvt::Progress(a, b) => {
                    if let Upd::Downloading(_, x, y) = &mut self.upd {
                        *x = a;
                        *y = b;
                    }
                }
                UpdEvt::Ready(p) => {
                    if let Upd::Downloading(r, ..) = std::mem::replace(&mut self.upd, Upd::Idle) {
                        self.upd = Upd::Ready(r, p);
                        self.upd_card = true;
                    }
                }
                UpdEvt::Failed(e) => {
                    if let Upd::Downloading(r, ..) = std::mem::replace(&mut self.upd, Upd::Idle) {
                        self.upd = Upd::Failed(r, e);
                        self.upd_card = true;
                    }
                }
            }
        }
    }

    fn start_download(&mut self) {
        let r = match std::mem::replace(&mut self.upd, Upd::Idle) {
            Upd::Available(r) | Upd::Failed(r, _) => r,
            other => {
                self.upd = other;
                return;
            }
        };
        update::download(r.clone(), self.upd_tx.clone(), self.ctx.clone());
        self.upd = Upd::Downloading(r, 0, 0);
    }

    /// 右下角的更新卡片
    fn update_card(&mut self, ctx: &egui::Context) {
        if !self.upd_card {
            return;
        }
        let rel = match &self.upd {
            Upd::Available(r) | Upd::Downloading(r, ..) | Upd::Ready(r, _) | Upd::Failed(r, _) => r.clone(),
            _ => return,
        };
        enum Act {
            Download,
            Later,
            Skip,
            Install,
            Home,
        }
        let mut act = None;
        egui::Area::new(egui::Id::new("upd-card"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-24.0, -44.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE_HI()))
                    .corner_radius(R_CARD)
                    .shadow(epaint::Shadow {
                        offset: [0, 10],
                        blur: 30,
                        spread: 0,
                        color: shade(190),
                    })
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(360.0);
                        let top = ui.max_rect().top();
                        hgrad_line(ui.painter(), top - 16.0 + 0.5, ui.max_rect().expand(16.0).x_range(), ACCENT().gamma_multiply(0.7));
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                            ui.painter()
                                .rect(r, CornerRadius::same(9), ACCENT().gamma_multiply(0.12), st(1.0, ACCENT().gamma_multiply(0.4)), egui::StrokeKind::Inside);
                            draw_icon(ui.painter(), r.shrink(9.0), Icon::Download, ACCENT());
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(t("发现新版本")).font(FontId::new(14.5, bold())).color(TEXT()));
                                let mut v = format!("v{}  →  v{}", env!("CARGO_PKG_VERSION"), rel.version.trim_start_matches(['v', 'V']));
                                if !rel.date.is_empty() {
                                    v = format!("{v}   ·   {}", rel.date);
                                }
                                ui.label(RichText::new(v).font(FontId::new(11.5, FontFamily::Monospace)).color(ACCENT()));
                            });
                            if !rel.force && !matches!(self.upd, Upd::Downloading(..)) {
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    if ui.add(egui::Button::new(RichText::new("×").size(16.0).color(TEXT3())).frame(false)).on_hover_text(t("稍后")).clicked() {
                                        act = Some(Act::Later);
                                    }
                                });
                            }
                        });
                        if !rel.notes.trim().is_empty() {
                            ui.add_space(10.0);
                            egui::Frame::new().fill(BG1()).corner_radius(R_WIDGET).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                                    ui.label(RichText::new(rel.notes.trim()).size(12.5).color(TEXT2()));
                                });
                            });
                        }
                        ui.add_space(12.0);
                        match &self.upd {
                            Upd::Available(_) if rel.url.trim().is_empty() => {
                                ui.label(RichText::new(t("当前平台没有自动更新包，请到项目主页下载新版本。")).size(12.0).color(TEXT3()));
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    if primary_button(ui, t("打开下载页")).clicked() {
                                        act = Some(Act::Home);
                                    }
                                    if !rel.force && ui.add(egui::Button::new(t("跳过此版本")).frame(false)).clicked() {
                                        act = Some(Act::Skip);
                                    }
                                });
                            }
                            Upd::Available(_) => {
                                ui.horizontal(|ui| {
                                    if primary_button(ui, t("立即更新")).clicked() {
                                        act = Some(Act::Download);
                                    }
                                    if !rel.force {
                                        if ui.add(egui::Button::new(t("稍后")).min_size(vec2(70.0, 30.0))).clicked() {
                                            act = Some(Act::Later);
                                        }
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if ui.add(egui::Button::new(RichText::new(t("跳过此版本")).size(12.0).color(TEXT3())).frame(false)).clicked() {
                                                act = Some(Act::Skip);
                                            }
                                        });
                                    }
                                });
                            }
                            Upd::Downloading(_, got, total) => {
                                let f = if *total > 0 { *got as f32 / *total as f32 } else { 0.0 };
                                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
                                ui.painter().rect_filled(r, 3.0, BG3());
                                let mut fr = r;
                                fr.set_width(r.width() * f.clamp(0.0, 1.0));
                                ui.painter().rect_filled(fr, 3.0, ACCENT());
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(t("正在下载…")).size(12.0).color(TEXT2()));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let s = if *total > 0 {
                                            format!("{} / {}  ·  {:.0}%", human_bytes(*got), human_bytes(*total), f * 100.0)
                                        } else {
                                            human_bytes(*got)
                                        };
                                        ui.label(RichText::new(s).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3()));
                                    });
                                });
                            }
                            Upd::Ready(..) => {
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                                    ui.painter().circle_filled(r.center(), 4.0, OK());
                                    ui.label(
                                        RichText::new(if rel.sha256.trim().is_empty() {
                                            t("下载完成")
                                        } else {
                                            t("下载完成，SHA-256 校验通过")
                                        })
                                        .size(12.0)
                                        .color(OK()),
                                    );
                                });
                                ui.add_space(8.0);
                                if primary_button(ui, t("重启并更新")).clicked() {
                                    act = Some(Act::Install);
                                }
                            }
                            Upd::Failed(_, e) => {
                                ui.label(RichText::new(e).size(12.0).color(ERR()));
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    if primary_button(ui, t("重试")).clicked() {
                                        act = Some(Act::Download);
                                    }
                                    if ui.add(egui::Button::new(t("关闭")).min_size(vec2(70.0, 30.0))).clicked() {
                                        act = Some(Act::Later);
                                    }
                                });
                            }
                            _ => {}
                        }
                        ui.add_space(10.0);
                        ui.separator();
                        switch_row(ui, &mut self.s.auto_update, t("启动时自动检查更新"), "");
                    });
            });
        match act {
            Some(Act::Download) => self.start_download(),
            Some(Act::Later) => {
                self.upd_card = false;
                if matches!(self.upd, Upd::Failed(..)) {
                    self.upd = Upd::Idle;
                }
            }
            Some(Act::Skip) => {
                self.s.skip_version = rel.version.clone();
                self.upd_card = false;
                self.upd = Upd::Idle;
            }
            Some(Act::Home) => open_url("https://github.com/zenghi/QCCOM/releases"),
            Some(Act::Install) => {
                if let Upd::Ready(_, p) = &self.upd {
                    match update::install(p) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.upd = Upd::Failed(rel, e),
                    }
                }
            }
            None => {}
        }
    }
}

fn activity_button(ui: &mut egui::Ui, icon: Icon, tip: &str, active: bool, dot: Option<Color32>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(48.0, 42.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(2.0, 26.0)), 1.0, ACCENT());
        p.rect_filled(Rect::from_center_size(rect.center(), vec2(36.0, 36.0)), CornerRadius::same(8), ACCENT().gamma_multiply(0.10));
    } else if resp.hovered() {
        p.rect_filled(Rect::from_center_size(rect.center(), vec2(36.0, 36.0)), CornerRadius::same(8), BG2());
    }
    let c = if active {
        ACCENT()
    } else if resp.hovered() {
        TEXT()
    } else {
        TEXT3()
    };
    draw_icon(p, Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), icon, c);
    if let Some(d) = dot {
        let at = rect.center() + vec2(9.0, -9.0);
        p.circle(at, 3.5, d, st(2.0, BG0()));
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip)
}

fn lang_item(ui: &mut egui::Ui, name: &str, on: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, R_WIDGET, BG3());
    }
    if on {
        p.circle_filled(pos2(r.left() + 12.0, r.center().y), 3.0, ACCENT());
    }
    p.text(
        pos2(r.left() + 24.0, r.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(13.0),
        if on { ACCENT() } else { TEXT() },
    );
    resp.clicked()
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(ON_ACCENT()).strong())
            .fill(ACCENT())
            .corner_radius(R_WIDGET)
            .min_size(vec2(110.0, 30.0)),
    )
}

pub(crate) fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd").args(["/c", "start", "", url]).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

impl eframe::App for App {
    /// 面板之间没盖住的缝隙露出的是清屏色，要跟着主题走，否则浅色下会露出黑边
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG1().to_normalized_gamma_f32()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.s.code = self.code.s.clone();
        self.s.git = self.gitv.s.clone();
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some((mut w, _)) = self.record.take() {
            let _ = w.flush();
        }
        self.disconnect();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.s.window_placed {
            self.place_window(ctx);
        }
        self.handle_events();
        self.tick();

        self.poll_update(ctx);
        self.follow_system_theme(ctx);
        let scope = match self.page {
            Page::Debug | Page::Settings => Scope::Debug,
            Page::Code => Scope::Code,
            Page::Git => Scope::Git,
        };
        let term_focused = self.page == Page::Debug && self.s.terminal && ctx.memory(|m| m.has_focus(term_id()));
        let acts = if self.prefs.recording.is_some() || self.palette.is_some() {
            vec![]
        } else {
            self.km.poll(ctx, scope, term_focused)
        };
        let mut acts = acts;
        acts.extend(self.pending_acts.drain(..));
        let acts = self.run_global(ctx, acts);
        if self.page == Page::Debug {
            self.shortcuts(ctx, &acts, term_focused);
        }
        self.update_view();

        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(BG0()).inner_margin(egui::Margin::symmetric(14, 4)))
            .show_separator_line(true)
            .show(ctx, |ui| self.status_bar(ui));
        self.activity_bar(ctx);
        match self.page {
            Page::Code => {
                self.code.ui(ctx, &acts);
                for e in std::mem::take(&mut self.code.events) {
                    match e {
                        CodeEvt::Toast(m, c) => self.toast(m, c),
                    }
                }
            }
            Page::Git => {
                self.gitv.ui(ctx, &acts);
                for e in std::mem::take(&mut self.gitv.events) {
                    match e {
                        GitEvt::Toast(m, c) => self.toast(m, c),
                        GitEvt::OpenFile(p, line) => {
                            self.set_page(Page::Code);
                            self.code.open_file(&p, Some((line, None)));
                        }
                    }
                }
            }
            Page::Debug => self.debug_page(ctx),
            Page::Settings => self.settings_page(ctx),
        }
        self.palette_ui(ctx);

        // 提示气泡
        self.update_card(ctx);
        if let Some((msg, col, at)) = self.toast.clone() {
            let age = at.elapsed().as_secs_f32();
            if age > 3.0 {
                self.toast = None;
            } else {
                let a = (3.0 - age).min(1.0) * (age * 6.0).min(1.0);
                egui::Area::new(egui::Id::new("toast"))
                    .anchor(Align2::RIGHT_BOTTOM, vec2(-28.0, -86.0 - (1.0 - (age * 6.0).min(1.0)) * 8.0))
                    .interactable(false)
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        let r = egui::Frame::new()
                            .fill(BG2().gamma_multiply(a))
                            .stroke(st(1.0, LINE_HI().gamma_multiply(a)))
                            .corner_radius(R_CARD)
                            .shadow(epaint::Shadow {
                                offset: [0, 6],
                                blur: 20,
                                spread: 0,
                                color: shade((160.0 * a) as u8),
                            })
                            .inner_margin(egui::Margin {
                                left: 16,
                                right: 16,
                                top: 9,
                                bottom: 9,
                            })
                            .show(ui, |ui| {
                                ui.label(RichText::new(msg).color(TEXT().gamma_multiply(a)));
                            })
                            .response
                            .rect;
                        ui.painter()
                            .rect_filled(Rect::from_min_size(r.min + vec2(0.0, 8.0), vec2(3.0, r.height() - 16.0)), 1.5, col.gamma_multiply(a));
                    });
                ctx.request_repaint_after(Duration::from_millis(30));
            }
        }
    }
}

// ---------------------------------------------------------------- 小部件

pub(crate) fn brand(ui: &mut egui::Ui, logo: &egui::TextureHandle) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        let p = ui.painter();
        p.add(
            epaint::Shadow {
                offset: [0, 0],
                blur: 16,
                spread: 0,
                color: ACCENT().gamma_multiply(0.18),
            }
            .as_shape(rect.shrink(8.0), 6.0),
        );
        p.image(logo.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), logo_tint());
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.x = 0.0;
        let f = FontId::new(16.0, mono_bold());
        ui.label(RichText::new("QC").font(f.clone()).color(ACCENT()));
        ui.label(RichText::new("COM").font(f).color(TEXT()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // 版本号点开是“关于”：作者、仓库、协议（开源协议要求保留这些署名）
            let r = egui::Frame::new()
                .fill(TEXT3().gamma_multiply(0.12))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(6, 1))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).font(FontId::new(10.5, FontFamily::Monospace)).color(TEXT3()));
                })
                .response
                .interact(Sense::click())
                .on_hover_text(t("关于 QCCOM"));
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let id = egui::Id::new("about");
            if r.clicked() {
                ui.memory_mut(|m| m.toggle_popup(id));
            }
            egui::popup::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(260.0);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                    ui.painter().image(logo.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), logo_tint());
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(format!("QCCOM  v{}", env!("CARGO_PKG_VERSION"))).font(FontId::new(14.0, mono_bold())).color(TEXT()));
                        ui.label(RichText::new(t("串口 / 网络调试助手")).small().color(TEXT3()));
                    });
                });
                ui.add_space(8.0);
                ui.label(RichText::new("© 2026 zenghi").color(TEXT2()));
                if ui.link(RichText::new("github.com/zenghi/QCCOM").color(ACCENT())).clicked() {
                    open_url("https://github.com/zenghi/QCCOM");
                }
                ui.label(RichText::new("Apache License 2.0").small().color(TEXT3()));
                ui.add_space(2.0);
            });
        });
    });
}

/// 分区小标题：大写等宽小字 + 右侧附注
pub(crate) fn eyebrow(ui: &mut egui::Ui, title: &str, note: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(FontId::new(11.0, bold())).color(TEXT3()).extra_letter_spacing(1.5));
        if !note.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(note).font(FontId::new(10.5, FontFamily::Monospace)).color(TEXT3()));
            });
        }
    });
    ui.add_space(2.0);
}

pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, LINE()))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn port_card(ui: &mut egui::Ui, p: &PortEntry, selected: bool, live: bool, enabled: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 46.0), if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    let hov = resp.hovered() && enabled;
    let fill = if selected {
        BG3()
    } else if hov {
        BG2()
    } else {
        BG1()
    };
    let stroke = if live {
        OK().gamma_multiply(0.5)
    } else if selected {
        LINE_HI()
    } else if hov {
        LINE()
    } else {
        Color32::TRANSPARENT
    };
    painter.rect(rect, CornerRadius::same(7), fill, st(1.0, stroke), egui::StrokeKind::Inside);
    if selected {
        painter.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 10.0), vec2(2.5, rect.height() - 20.0)),
            1.5,
            if live { OK() } else { ACCENT() },
        );
    }
    let dot = pos2(rect.left() + 16.0, rect.center().y);
    let dc = if live {
        OK()
    } else if selected {
        ACCENT()
    } else {
        TEXT3()
    };
    if live || selected {
        painter.circle_filled(dot, 6.0, dc.gamma_multiply(0.18));
    }
    painter.circle_filled(dot, 3.0, dc);
    let x = rect.left() + 30.0;
    let name = p.name.rsplit(['/', '\\']).next().unwrap_or(&p.name);
    painter.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        name,
        FontId::new(13.5, mono_bold()),
        if selected { TEXT() } else { TEXT2() },
    );
    let desc = if p.desc.is_empty() { p.name.clone() } else { p.desc.clone() };
    painter.text(
        pos2(x, rect.top() + 32.0),
        Align2::LEFT_CENTER,
        truncate(&desc, 30),
        FontId::new(11.0, FontFamily::Proportional),
        TEXT3(),
    );
    if live {
        painter.text(rect.right_center() - vec2(12.0, 0.0), Align2::RIGHT_CENTER, "LIVE", FontId::new(10.0, mono_bold()), OK());
    }
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    resp.on_hover_text(&p.name)
}

fn empty_port_card(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 46.0), Sense::hover());
    let p = ui.painter();
    // 虚线框
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    for seg in pts.windows(2) {
        p.extend(Shape::dashed_line(seg, st(1.0, LINE_HI()), 4.0, 4.0));
    }
    let t = ui.input(|i| i.time);
    let dots = ".".repeat(((t * 2.0) as usize % 3) + 1);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        tf("等待设备插入{}", &[&dots]),
        FontId::new(12.0, FontFamily::Proportional),
        TEXT3(),
    );
    ui.ctx().request_repaint_after(Duration::from_millis(500));
}

fn stat(ui: &mut egui::Ui, k: &str, v: &str, c: Color32) {
    ui.vertical(|ui| {
        ui.set_width(100.0);
        ui.label(RichText::new(k).font(FontId::new(10.0, bold())).color(TEXT3()).extra_letter_spacing(1.2));
        ui.label(RichText::new(v).font(FontId::new(15.0, mono_bold())).color(c));
    });
}

/// 速率曲线：折线 + 渐隐填充
fn sparkline(p: &egui::Painter, rect: Rect, data: &VecDeque<f32>, color: Color32) {
    // 背景刻度线
    for k in 1..3 {
        let y = rect.top() + rect.height() * k as f32 / 3.0;
        p.extend(Shape::dashed_line(&[pos2(rect.left(), y), pos2(rect.right(), y)], st(1.0, LINE()), 2.0, 4.0));
    }
    if data.len() < 2 {
        return;
    }
    let max = data.iter().cloned().fold(1.0f32, f32::max) * 1.15;
    let n = data.len();
    let pts: Vec<_> = data
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(rect.left() + rect.width() * i as f32 / (n - 1) as f32, rect.bottom() - rect.height() * (v / max)))
        .collect();
    let mut mesh = epaint::Mesh::default();
    let top = color.gamma_multiply(0.28);
    let bot = color.gamma_multiply(0.0);
    for w in pts.windows(2) {
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(w[0], top);
        mesh.colored_vertex(w[1], top);
        mesh.colored_vertex(pos2(w[1].x, rect.bottom()), bot);
        mesh.colored_vertex(pos2(w[0].x, rect.bottom()), bot);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::line(pts.clone(), st(1.5, color)));
    if let Some(last) = pts.last() {
        p.circle_filled(*last, 5.0, color.gamma_multiply(0.25));
        p.circle_filled(*last, 2.5, color);
    }
}

/// 一道两端渐隐的水平细线
pub(crate) fn hgrad_line(p: &egui::Painter, y: f32, x: egui::Rangef, color: Color32) {
    let mut mesh = epaint::Mesh::default();
    let mid = (x.min + x.max) / 2.0;
    let segs = [(x.min + 20.0, Color32::TRANSPARENT), (mid, color), (x.max - 20.0, Color32::TRANSPARENT)];
    for w in segs.windows(2) {
        let i = mesh.vertices.len() as u32;
        mesh.colored_vertex(pos2(w[0].0, y - 0.5), w[0].1);
        mesh.colored_vertex(pos2(w[1].0, y - 0.5), w[1].1);
        mesh.colored_vertex(pos2(w[1].0, y + 0.5), w[1].1);
        mesh.colored_vertex(pos2(w[0].0, y + 0.5), w[0].1);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    p.add(Shape::mesh(mesh));
}

pub(crate) fn hud_corners(p: &egui::Painter, r: Rect, color: Color32) {
    let l = 10.0;
    let s = st(1.0, color);
    for (corner, dx, dy) in [(r.left_top(), 1.0, 1.0), (r.right_top(), -1.0, 1.0), (r.left_bottom(), 1.0, -1.0), (r.right_bottom(), -1.0, -1.0)] {
        p.line_segment([corner, corner + vec2(l * dx, 0.0)], s);
        p.line_segment([corner, corner + vec2(0.0, l * dy)], s);
    }
}

fn empty_state(ui: &egui::Ui, rect: Rect, no_ports: bool, connected: bool, logo: &egui::TextureHandle) {
    let p = ui.painter();
    // 点阵背景
    let step = 22.0;
    let dot = LINE();
    let mut y = rect.top() + step;
    while y < rect.bottom() - 4.0 {
        let mut x = rect.left() + step;
        while x < rect.right() - 4.0 {
            p.circle_filled(pos2(x, y), 0.8, dot);
            x += step;
        }
        y += step;
    }
    let c = rect.center() - vec2(0.0, 30.0);
    p.add(
        epaint::Shadow {
            offset: [0, 0],
            blur: 60,
            spread: 0,
            color: ACCENT().gamma_multiply(0.12),
        }
        .as_shape(Rect::from_center_size(c, vec2(40.0, 40.0)), 20.0),
    );
    p.image(logo.id(), Rect::from_center_size(c, vec2(72.0, 72.0)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), logo_tint());
    let (title, sub) = if connected {
        (t("已连接，等待数据"), t("设备有输出时会实时显示在这里"))
    } else if no_ports {
        (t("未发现串口"), t("插入 USB 转串口设备后会自动出现在左侧"))
    } else {
        (t("准备就绪"), t("左侧选好端口，点「连接」；不知道波特率就点「⚡ 自动」"))
    };
    p.text(c + vec2(0.0, 52.0), Align2::CENTER_CENTER, title, FontId::new(17.0, bold()), TEXT());
    p.text(c + vec2(0.0, 76.0), Align2::CENTER_CENTER, sub, FontId::new(13.0, FontFamily::Proportional), TEXT2());
    let keys = [("F5", t("连接")), ("Ctrl+F", t("搜索")), ("Ctrl+L", t("清屏")), ("Ctrl+S", t("保存"))];
    let font = FontId::new(11.0, FontFamily::Monospace);
    let widths: Vec<f32> = keys
        .iter()
        .map(|(k, v)| ui.fonts(|f| f.layout_no_wrap(format!("{k}  {v}"), font.clone(), TEXT()).size().x) + 20.0)
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 8.0 * (keys.len() - 1) as f32;
    let mut x = c.x - total / 2.0;
    let y = c.y + 112.0;
    for ((k, v), w) in keys.iter().zip(widths) {
        let r = Rect::from_min_size(pos2(x, y - 12.0), vec2(w, 24.0));
        p.rect(r, CornerRadius::same(5), BG2(), st(1.0, LINE()), egui::StrokeKind::Inside);
        let g = ui.fonts(|f| f.layout_no_wrap(k.to_string(), font.clone(), TEXT()));
        let kx = r.left() + 10.0;
        p.text(pos2(kx, r.center().y), Align2::LEFT_CENTER, *k, font.clone(), TEXT());
        p.text(pos2(kx + g.size().x + 8.0, r.center().y), Align2::LEFT_CENTER, *v, font.clone(), TEXT3());
        x += w + 8.0;
    }
}

/// iOS 风格开关；返回是否被切换
pub(crate) fn switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = vec2(30.0, 17.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let p = ui.painter();
    let bg = lerp_color(BG3(), ACCENT().gamma_multiply(0.85), t);
    p.rect(rect, CornerRadius::same(9), bg, st(1.0, if *on { Color32::TRANSPARENT } else { LINE_HI() }), egui::StrokeKind::Inside);
    let x = egui::lerp(rect.left() + 8.5..=rect.right() - 8.5, t);
    p.circle_filled(pos2(x, rect.center().y), 6.0, lerp_color(TEXT2(), BG0(), t));
    resp
}

pub(crate) fn switch_row(ui: &mut egui::Ui, v: &mut bool, label: &str, tip: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        let r = ui.label(RichText::new(label).color(if *v { TEXT() } else { TEXT2() }));
        r.on_hover_text(tip);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed = switch(ui, v).on_hover_text(tip).changed();
        });
    });
    changed
}

pub(crate) fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Icon {
    Sidebar,
    Search,
    Pause,
    Play,
    Trash,
    Copy,
    Save,
    Record,
    Close,
    Commands,
    Pulse,
    Code,
    Branch,
    Folder,
    Refresh,
    Collapse,
    Download,
    Back,
    Forward,
    Edit,
    Files,
    Outline,
    Commit,
    Gear,
    Palette,
    Lines,
    Plug,
    Keyboard,
    Info,
    Plus,
}

/// 统一线宽的矢量小图标，不依赖字体里有没有这个符号
pub(crate) fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, c: Color32) {
    let s = st(1.4, c);
    let c0 = r.center();
    let u = r.width() / 16.0;
    let pt = |x: f32, y: f32| c0 + vec2(x * u, y * u);
    match icon {
        Icon::Sidebar => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(14.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-2.0, -6.0), pt(-2.0, 6.0)], s);
        }
        Icon::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.8 * u, s);
            p.line_segment([pt(2.2, 2.2), pt(6.0, 6.0)], s);
        }
        Icon::Pause => {
            p.rect_filled(Rect::from_min_max(pt(-4.5, -5.5), pt(-1.5, 5.5)), 1.0, c);
            p.rect_filled(Rect::from_min_max(pt(1.5, -5.5), pt(4.5, 5.5)), 1.0, c);
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![pt(-4.0, -6.0), pt(6.0, 0.0), pt(-4.0, 6.0)], c, egui::Stroke::NONE));
        }
        Icon::Trash => {
            p.line_segment([pt(-6.0, -4.0), pt(6.0, -4.0)], s);
            p.line_segment([pt(-2.0, -6.5), pt(2.0, -6.5)], s);
            p.add(Shape::line(vec![pt(-4.5, -4.0), pt(-3.5, 6.5), pt(3.5, 6.5), pt(4.5, -4.0)], s));
        }
        Icon::Copy => {
            p.rect_stroke(Rect::from_min_max(pt(-2.5, -2.5), pt(6.0, 6.5)), 2.0, s, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![pt(-6.0, 3.0), pt(-6.0, -6.0), pt(3.0, -6.0)], s));
        }
        Icon::Save => {
            p.line_segment([pt(0.0, -6.5), pt(0.0, 3.0)], s);
            p.add(Shape::line(vec![pt(-3.8, -0.5), pt(0.0, 3.3), pt(3.8, -0.5)], s));
            p.add(Shape::line(vec![pt(-6.0, 2.5), pt(-6.0, 6.5), pt(6.0, 6.5), pt(6.0, 2.5)], s));
        }
        Icon::Record => {
            p.circle_stroke(c0, 6.0 * u, s);
            p.circle_filled(c0, 3.2 * u, c);
        }
        Icon::Commands => {
            // 面板 + 三条命令
            p.rect_stroke(Rect::from_center_size(c0, vec2(14.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(2.5, -6.0), pt(2.5, 6.0)], s);
            for y in [-2.5, 0.5, 3.5] {
                p.line_segment([pt(4.5, y), pt(5.5, y)], s);
            }
            p.add(Shape::line(vec![pt(-4.5, -2.0), pt(-2.0, 0.0), pt(-4.5, 2.0)], s));
        }
        Icon::Close => {
            p.line_segment([pt(-4.5, -4.5), pt(4.5, 4.5)], s);
            p.line_segment([pt(4.5, -4.5), pt(-4.5, 4.5)], s);
        }
        Icon::Pulse => {
            // 示波器波形：串口调试
            p.add(Shape::line(
                vec![pt(-7.0, 1.0), pt(-4.0, 1.0), pt(-2.0, -5.0), pt(1.0, 6.0), pt(3.0, -1.0), pt(4.5, 1.0), pt(7.0, 1.0)],
                s,
            ));
        }
        Icon::Code => {
            p.add(Shape::line(vec![pt(-3.0, -5.0), pt(-7.0, 0.0), pt(-3.0, 5.0)], s));
            p.add(Shape::line(vec![pt(3.0, -5.0), pt(7.0, 0.0), pt(3.0, 5.0)], s));
            p.line_segment([pt(1.2, -6.5), pt(-1.2, 6.5)], s);
        }
        Icon::Branch => {
            p.circle_stroke(pt(-3.5, -5.0), 1.8 * u, s);
            p.circle_stroke(pt(-3.5, 5.5), 1.8 * u, s);
            p.circle_stroke(pt(4.5, -3.0), 1.8 * u, s);
            p.line_segment([pt(-3.5, -3.2), pt(-3.5, 3.7)], s);
            p.add(Shape::line(vec![pt(4.5, -1.2), pt(4.5, 0.5), pt(-3.5, 3.0)], s));
        }
        Icon::Folder => {
            p.add(Shape::line(
                vec![pt(-7.0, 5.5), pt(-7.0, -5.5), pt(-2.5, -5.5), pt(-0.5, -3.5), pt(7.0, -3.5), pt(7.0, 5.5), pt(-7.0, 5.5)],
                s,
            ));
        }
        Icon::Refresh => {
            let pts: Vec<_> = (0..=20)
                .map(|k| {
                    let a = 0.6 + k as f32 / 20.0 * 4.9;
                    c0 + vec2(a.cos(), a.sin()) * 5.5 * u
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, s));
            p.add(Shape::line(vec![end + vec2(-3.5, -0.5) * u, end, end + vec2(0.5, 3.5) * u], s));
        }
        Icon::Collapse => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(12.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-3.0, 0.0), pt(3.0, 0.0)], s);
        }
        Icon::Download => {
            p.line_segment([pt(0.0, -6.5), pt(0.0, 3.0)], s);
            p.add(Shape::line(vec![pt(-3.8, -0.5), pt(0.0, 3.3), pt(3.8, -0.5)], s));
            p.line_segment([pt(-6.0, 6.5), pt(6.0, 6.5)], s);
        }
        Icon::Back => {
            p.add(Shape::line(vec![pt(2.0, -5.0), pt(-3.0, 0.0), pt(2.0, 5.0)], s));
        }
        Icon::Forward => {
            p.add(Shape::line(vec![pt(-2.0, -5.0), pt(3.0, 0.0), pt(-2.0, 5.0)], s));
        }
        Icon::Edit => {
            p.add(Shape::line(vec![pt(-5.5, 5.5), pt(-5.5, 2.5), pt(3.0, -6.0), pt(6.0, -3.0), pt(-2.5, 5.5), pt(-5.5, 5.5)], s));
        }
        Icon::Files => {
            p.add(Shape::line(vec![pt(-4.0, -7.0), pt(2.0, -7.0), pt(5.5, -3.5), pt(5.5, 5.0), pt(-4.0, 5.0), pt(-4.0, -7.0)], s));
            p.add(Shape::line(vec![pt(-6.5, -4.0), pt(-6.5, 7.5), pt(2.5, 7.5)], s));
        }
        Icon::Outline => {
            for (x, y) in [(-6.0, -4.5), (-3.0, 0.0), (-3.0, 4.5)] {
                p.circle_filled(pt(x, y), 1.1 * u, c);
                p.line_segment([pt(x + 2.5, y), pt(6.5, y)], s);
            }
        }
        Icon::Commit => {
            p.circle_stroke(c0, 3.2 * u, s);
            p.line_segment([pt(-7.5, 0.0), pt(-3.2, 0.0)], s);
            p.line_segment([pt(3.2, 0.0), pt(7.5, 0.0)], s);
        }
        Icon::Gear => {
            p.circle_stroke(c0, 2.4 * u, s);
            let teeth: Vec<egui::Pos2> = (0..16)
                .map(|i| {
                    let a = i as f32 * std::f32::consts::TAU / 16.0 - 0.1;
                    let rr = if (i / 2) % 2 == 0 { 7.0 } else { 5.2 };
                    pt(a.cos() * rr, a.sin() * rr)
                })
                .collect();
            p.add(Shape::closed_line(teeth, s));
        }
        Icon::Palette => {
            p.circle_stroke(c0, 6.5 * u, s);
            for (x, y) in [(-3.0, -2.0), (0.5, -3.8), (3.6, -1.4)] {
                p.circle_filled(pt(x, y), 1.2 * u, c);
            }
            p.circle_stroke(pt(1.5, 3.2), 1.6 * u, s);
        }
        Icon::Lines => {
            for (y, w) in [(-4.5, 6.0), (-1.5, 3.0), (1.5, 6.0), (4.5, 1.5)] {
                p.line_segment([pt(-6.0, y), pt(w, y)], s);
            }
        }
        Icon::Plug => {
            p.line_segment([pt(-2.5, -7.0), pt(-2.5, -3.5)], s);
            p.line_segment([pt(2.5, -7.0), pt(2.5, -3.5)], s);
            p.add(Shape::line(
                vec![pt(-5.0, -3.5), pt(5.0, -3.5), pt(5.0, 0.0), pt(2.0, 3.5), pt(-2.0, 3.5), pt(-5.0, 0.0), pt(-5.0, -3.5)],
                s,
            ));
            p.line_segment([pt(0.0, 3.5), pt(0.0, 7.0)], s);
        }
        Icon::Keyboard => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(15.0 * u, 10.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            for x in [-4.0, -1.3, 1.3, 4.0] {
                p.circle_filled(pt(x, -1.8), 0.8 * u, c);
            }
            p.line_segment([pt(-3.5, 2.0), pt(3.5, 2.0)], s);
        }
        Icon::Info => {
            p.circle_stroke(c0, 6.5 * u, s);
            p.circle_filled(pt(0.0, -3.0), 0.9 * u, c);
            p.line_segment([pt(0.0, -0.5), pt(0.0, 4.0)], s);
        }
        Icon::Plus => {
            p.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], s);
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], s);
        }
    }
}

/// 无边框图标按钮，悬停时出底色；active 时青色（录制时红色）
/// 右键菜单的一项：左边文字，右边淡色快捷键
pub(crate) fn menu_item(ui: &mut egui::Ui, text: &str, keys: &str) -> bool {
    let w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(4), BG3());
    }
    p.text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.0), TEXT());
    if !keys.is_empty() {
        p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, keys, FontId::proportional(12.0), TEXT3());
    }
    resp.clicked()
}

pub(crate) fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str, active: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 28.0), Sense::click());
    let p = ui.painter();
    let on = if icon == Icon::Record { ERR() } else { ACCENT() };
    if resp.hovered() || active {
        p.rect_filled(rect, CornerRadius::same(6), if active { on.gamma_multiply(0.14) } else { BG3() });
    }
    let c = if active {
        on
    } else if resp.hovered() {
        TEXT()
    } else {
        TEXT2()
    };
    draw_icon(p, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), icon, c);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tip)
}

/// 小开关标签：开时青色描边
pub(crate) fn toggle_chip(ui: &mut egui::Ui, v: &mut bool, text: &str, tip: &str) -> bool {
    let b = egui::Button::new(RichText::new(text).size(12.0).color(if *v { ACCENT() } else { TEXT2() }))
        .fill(if *v { ACCENT().gamma_multiply(0.10) } else { BG2() })
        .stroke(st(1.0, if *v { ACCENT().gamma_multiply(0.45) } else { LINE() }))
        .corner_radius(CornerRadius::same(5))
        .min_size(vec2(0.0, 26.0));
    let r = ui.add(b);
    let clicked = r.clicked();
    r.on_hover_text(tip);
    if clicked {
        *v = !*v;
    }
    clicked
}

fn level_pill(ui: &mut egui::Ui, name: &str, n: u64, col: Color32, on: bool) -> egui::Response {
    let font = FontId::new(11.0, mono_bold());
    let num = FontId::new(11.0, FontFamily::Monospace);
    let ns = compact(n);
    let gw = ui.fonts(|f| f.layout_no_wrap(name.to_string(), font.clone(), TEXT()).size().x);
    let nw = ui.fonts(|f| f.layout_no_wrap(ns.clone(), num.clone(), TEXT()).size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2(gw + nw + 28.0, 26.0), Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    let fill = if on {
        col.gamma_multiply(0.14)
    } else if hov {
        BG3()
    } else {
        BG2()
    };
    p.rect(rect, CornerRadius::same(5), fill, st(1.0, if on { col.gamma_multiply(0.5) } else { LINE() }), egui::StrokeKind::Inside);
    p.circle_filled(pos2(rect.left() + 10.0, rect.center().y), 3.0, col);
    p.text(pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, name, font, if on { col } else { TEXT2() });
    p.text(pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, ns, num, if n > 0 { TEXT() } else { TEXT3() });
    if hov {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub(crate) fn pill(ui: &mut egui::Ui, text: &str, col: Color32) {
    egui::Frame::new()
        .fill(col.gamma_multiply(0.12))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(FontId::new(10.5, FontFamily::Monospace)).color(col));
        });
}

fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, v: &mut T, opts: &[(T, &str)]) {
    // 注意：在 right_to_left 布局里调用，所以反向添加
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, LINE()))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (o, name) in opts.iter().rev() {
                let on = *v == *o;
                let b = egui::Button::new(RichText::new(*name).size(12.0).color(if on { TEXT() } else { TEXT3() }))
                    .fill(if on { BG3() } else { Color32::TRANSPARENT })
                    .stroke(st(1.0, if on { LINE_HI() } else { Color32::TRANSPARENT }))
                    .corner_radius(CornerRadius::same(4))
                    .min_size(vec2(44.0, 22.0));
                if ui.add(b).clicked() {
                    *v = *o;
                }
            }
        });
}

fn combo_u8(ui: &mut egui::Ui, id: &str, v: &mut u8, opts: &[(u8, &str)]) {
    let cur = opts.iter().find(|o| o.0 == *v).map(|o| o.1).unwrap_or("?");
    egui::ComboBox::from_id_salt(id).width(130.0).selected_text(cur).show_ui(ui, |ui| {
        for (k, name) in opts {
            ui.selectable_value(v, *k, *name);
        }
    });
}

fn shortcuts_help(ui: &mut egui::Ui, km: &Keymap) {
    egui::Grid::new("keys").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        let mut rows: Vec<(String, String)> = [
            Act::Palette,
            Act::Connect,
            Act::LogFind,
            Act::LogClear,
            Act::LogSave,
            Act::LogPause,
            Act::FocusCmdBar,
            Act::ToggleSidebar,
            Act::ToggleCmdPanel,
            Act::ToggleTerminal,
        ]
        .iter()
        .map(|a| (km.label(*a), t(keymap::def(*a).label).to_string()))
        .collect();
        for (k, v) in [
            ("Alt+1…9", t("发送第 1–9 条快捷命令")),
            (t("拖动 / 双击 / 三击"), t("选择文字 / 单词 / 整行")),
            (t("点击行号"), t("选中整行，Shift 多选")),
            ("Ctrl+C", t("复制选中内容")),
            ("↑ / ↓", t("发送历史")),
            (t("终端里"), t("按键直接发给设备，Ctrl+C 中断")),
        ] {
            rows.push((k.to_string(), v.to_string()));
        }
        for (k, v) in rows {
            ui.label(RichText::new(k).font(FontId::new(12.0, FontFamily::Monospace)).color(ACCENT()));
            ui.label(RichText::new(v).color(TEXT2()));
            ui.end_row();
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new(t("点击可自定义快捷键")).small().color(TEXT3()));
}

/// 等宽分段选择条（侧边栏用），选中项有青色底线
pub(crate) fn tab_strip<T: PartialEq + Copy>(ui: &mut egui::Ui, v: &mut T, opts: &[(T, &str)], h: f32) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let p = ui.painter();
    p.rect(rect, R_WIDGET, BG2(), st(1.0, LINE()), egui::StrokeKind::Inside);
    let seg = (w - 4.0) / opts.len() as f32;
    let enabled = ui.is_enabled();
    for (i, (o, name)) in opts.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + 2.0 + i as f32 * seg, rect.top() + 2.0), vec2(seg, h - 4.0));
        let resp = ui.interact(r, ui.id().with(("tab", i, name)), Sense::click());
        let on = *v == *o;
        let p = ui.painter();
        if on {
            p.rect(r, CornerRadius::same(5), BG3(), st(1.0, LINE_HI()), egui::StrokeKind::Inside);
            p.hline(
                egui::Rangef::new(r.center().x - 10.0, r.center().x + 10.0),
                r.bottom() - 1.5,
                st(2.0, if enabled { ACCENT() } else { TEXT3() }),
            );
        } else if resp.hovered() && enabled {
            p.rect_filled(r, CornerRadius::same(5), tint(6));
        }
        let c = if on {
            TEXT()
        } else if resp.hovered() {
            TEXT2()
        } else {
            TEXT3()
        };
        p.text(
            r.center() - vec2(0.0, if on { 1.0 } else { 0.0 }),
            Align2::CENTER_CENTER,
            *name,
            FontId::new(12.5, if on { bold() } else { FontFamily::Proportional }),
            c,
        );
        if resp.clicked() && enabled {
            *v = *o;
        }
        if resp.hovered() && enabled {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

fn form_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(62.0, 26.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), TEXT2());
        add(ui);
    });
}

pub(crate) fn text_field(ui: &mut egui::Ui, v: &mut String, hint: &str) {
    ui.add_sized(
        vec2(ui.available_width(), 26.0),
        egui::TextEdit::singleline(v).hint_text(hint).font(FontId::new(13.0, FontFamily::Monospace)).margin(vec2(6.0, 4.0)),
    );
}

/// 数字输入框：编辑过程中允许临时为空，失焦后再规范化
fn num_field(ui: &mut egui::Ui, id: &str, v: &mut u16) {
    let id = egui::Id::new(("num", id));
    let mut s = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| v.to_string());
    let r = ui.add_sized(
        vec2(ui.available_width(), 26.0),
        egui::TextEdit::singleline(&mut s).font(FontId::new(13.0, FontFamily::Monospace)).margin(vec2(6.0, 4.0)),
    );
    if r.changed() {
        if let Ok(n) = s.trim().parse::<u16>() {
            *v = n;
        }
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, s));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
}

fn topic_chip(ui: &mut egui::Ui, tv: &str, c: Color32) -> egui::Response {
    let font = FontId::new(12.0, FontFamily::Monospace);
    let g = ui.painter().layout_no_wrap(tv.to_string(), font, TEXT());
    let size = vec2(g.size().x + 34.0, 24.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let hov = resp.hovered();
    p.rect(
        r,
        CornerRadius::same(12),
        c.gamma_multiply(if hov { 0.16 } else { 0.10 }),
        st(1.0, c.gamma_multiply(0.45)),
        egui::StrokeKind::Inside,
    );
    p.circle_filled(pos2(r.left() + 11.0, r.center().y), 3.0, c);
    p.galley(pos2(r.left() + 19.0, r.center().y - g.size().y / 2.0), g, TEXT());
    if hov {
        let x = pos2(r.right() - 10.0, r.center().y);
        p.line_segment([x + vec2(-3.0, -3.0), x + vec2(3.0, 3.0)], st(1.3, ERR()));
        p.line_segment([x + vec2(-3.0, 3.0), x + vec2(3.0, -3.0)], st(1.3, ERR()));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(t("点击取消订阅"))
}

/// 本机局域网 IP（不发任何数据包，只是让系统选一下出口网卡）
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

fn level_color(l: Level) -> Option<Color32> {
    match l {
        Level::Error => Some(ERR()),
        Level::Warn => Some(WARN()),
        Level::Info => Some(INFO()),
        Level::Debug => Some(DEBUG()),
        Level::Trace => Some(TRACE()),
        Level::None => None,
    }
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

pub(crate) fn human_bytes(b: u64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.1} KB", b as f64 / 1024.0),
        _ => format!("{:.2} MB", b as f64 / 1_048_576.0),
    }
}

fn compact(n: u64) -> String {
    match n {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_line(l: &Line, view: ViewMode) -> String {
    let tag = match l.dir {
        Dir::Rx => "RX",
        Dir::Tx => "TX",
        Dir::Sys => "##",
    };
    let hex = || l.raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
    let body = match (view, l.dir) {
        (_, Dir::Sys) | (ViewMode::Text, _) => l.plain.clone(),
        (ViewMode::Hex, _) => hex(),
        (ViewMode::Mixed, _) => format!("{} | {}", l.plain, hex()),
    };
    format!("{} {} {}", l.ts.format("%Y-%m-%d %H:%M:%S%.3f"), tag, body)
}

/// ASCII 大小写不敏感查找（不分配内存，返回的位置总在字符边界上）
pub fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn find_all_ci(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = find_ci(&hay[from..], needle) {
        out.push((from + i, from + i + needle.len()));
        from += i + needle.len();
    }
    out
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .replace("0x", " ")
        .replace("0X", " ")
        .chars()
        .map(|c| if c == ',' || c == ';' || c == '-' { ' ' } else { c })
        .collect();
    let mut out = vec![];
    for tok in cleaned.split_whitespace() {
        if tok.len() % 2 != 0 && tok.len() > 2 {
            return Err(tf("HEX 格式错误：“{}” 长度不是偶数", &[&tok]));
        }
        if tok.len() <= 2 {
            out.push(u8::from_str_radix(tok, 16).map_err(|_| tf("HEX 格式错误：“{}”", &[&tok]))?);
        } else {
            for i in (0..tok.len()).step_by(2) {
                out.push(u8::from_str_radix(&tok[i..i + 2], 16).map_err(|_| tf("HEX 格式错误：“{}”", &[&tok]))?);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex("AA 55 0x01,0d0A").unwrap(), vec![0xaa, 0x55, 0x01, 0x0d, 0x0a]);
        assert_eq!(parse_hex("f").unwrap(), vec![0x0f]);
        assert!(parse_hex("zz").is_err());
        assert!(parse_hex("abc").is_err());
    }

    #[test]
    fn ci_find() {
        assert_eq!(find_ci("Hello WiFi", "wifi"), Some(6));
        assert_eq!(find_all_ci("温度 err ERR", "err"), vec![(7, 10), (11, 14)]);
    }

    #[test]
    fn digits() {
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(compact(12345), "12.3k");
    }
}

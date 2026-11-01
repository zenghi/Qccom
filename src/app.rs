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

use crate::atcmds;
use crate::cmds::{default_groups, CmdAction, CmdGroup, CmdPanel, QuickCmd};
use crate::codeview::{CodeEvt, CodeSettings, CodeView};
use crate::gitview::{GitEvt, GitSettings, GitView};
use crate::i18n::{self, t, tf};
use crate::keymap::{self, Act, Keymap, Overrides, Scope};
use crate::logbuf::{Dir, Encoding, Level, Line, LogBuf};
use crate::net::{self, NetConfig, Transport};
use crate::serial::{self, Cmd, Connection, Evt, PortConfig, PortEntry, COMMON_BAUDS};
use crate::shell::{ShellEvt, ShellSettings, Shells};
use crate::termview::TermView;
use crate::theme::*;
use crate::update::{self, Release, UpdEvt};
use dbgtools::check_combo;

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
    /// 编辑器 / 终端背景图
    pub bg: crate::bgimg::BgCfg,
    /// 侧边栏里折叠起来的分组（"dev" / "baud" / "link" / "view"）
    pub folded: Vec<String>,
    /// 设置的版本号，用来迁移旧默认值
    pub rev: u32,
    /// 是否已经按黄金比例摆过窗口（只在第一次启动时做）
    pub window_placed: bool,
    /// 发送匿名使用统计（设备随机 ID、版本、系统、语言、使用时长、页面打开次数）
    pub telemetry: bool,
    /// 已经提示过一次「会发送匿名统计」
    pub telemetry_noticed: bool,
    /// 社区里已经看过的最新公告
    pub seen_notice: i64,
    pub groups: Vec<CmdGroup>,
    pub cur_group: usize,
    pub cmd_panel: bool,
    /// 终端模式（键盘直通设备）还是日志模式
    pub terminal: bool,
    /// 当前显示的连接方式（串口 / TCP / UDP / MQTT / BLE / WebSocket）
    pub transport: Transport,
    /// TCP 工具是客户端还是服务器
    pub tcp_mode: Transport,
    pub net: NetConfig,
    pub code: CodeSettings,
    pub git: GitSettings,
    pub shell: ShellSettings,
    /// 界面语言："auto" 跟随系统
    pub lang: String,
    pub auto_update: bool,
    /// 用户选择跳过的版本，不再提示
    pub skip_version: String,
    /// 主题："system" 跟随系统 / "dark" / "light"
    pub theme: String,
    /// 用户改过的快捷键
    pub keys: Overrides,
    /// 快捷键方案
    pub key_scheme: crate::keymap::Scheme,
    /// 复制日志时带上时间戳
    pub copy_ts: bool,
    /// 界面缩放（1.0 = 100%）
    pub ui_scale: f32,
    /// 命令面板最近用过的命令
    pub recent_cmds: Vec<String>,
    /// 时间戳后面显示和上一行的时间差
    pub show_delta: bool,
    /// 日志计时的两个关键字
    pub timing_a: String,
    pub timing_b: String,
    pub timing_re: bool,
    pub rules: Vec<logx::Rule>,
    pub frame: logx::FrameCfg,
    /// 录制分段：超过多少 MB / 多少分钟换一个文件（0 = 不分段）
    pub rec_split_mb: u32,
    pub rec_split_min: u32,
    /// 端口别名：端口名 → 显示的名字
    pub aliases: std::collections::BTreeMap<String, String>,
    /// 双口监听的第二个串口（空 = 不监听）
    pub sniff_port: String,
    /// 串口转 TCP 的端口
    pub bridge_port: u16,
    /// Windows：共享给其他软件时 QCCOM 占用的虚拟串口（com0com 一对里的一头）
    pub vcom_port: String,
    pub reset: serialx::ResetKind,
    /// 当前配置方案的名字
    pub profile: String,
    pub triggers: Vec<dock::Trigger>,
    pub scripts: Vec<dock::Script>,
    /// 右侧面板的标签：0 命令 1 应答 2 脚本
    pub dock_tab: u8,
    pub plot: plot::PlotCfg,
    /// 侧边栏选了「API」标签（HTTP 接口调试），这时主区域换成请求编辑器
    pub api_mode: bool,
    pub api: crate::api::ApiSettings,
    pub mb: dbgtools::MbCfg,
    /// 工具箱页的配置
    pub tb: toolbox::ToolboxCfg,
    /// 比较页
    pub cmp: crate::compareview::CompareSettings,
    /// HEX 发送时自动追加的校验
    pub send_check: crate::checksum::Check,
    /// 命令栏多行模式；逐行发送时每行之间的间隔
    pub multiline: bool,
    pub line_by_line: bool,
    pub line_gap_ms: u32,
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
            folded: vec![],
            bg: Default::default(),
            rev: SETTINGS_REV,
            window_placed: false,
            telemetry: true,
            telemetry_noticed: false,
            seen_notice: 0,
            groups: default_groups(),
            cur_group: 0,
            cmd_panel: true,
            terminal: false,
            transport: Transport::Serial,
            tcp_mode: Transport::TcpClient,
            net: NetConfig::default(),
            code: CodeSettings::default(),
            git: GitSettings::default(),
            shell: ShellSettings::default(),
            lang: "auto".into(),
            auto_update: true,
            skip_version: String::new(),
            theme: "system".into(),
            keys: Overrides::new(),
            key_scheme: Default::default(),
            copy_ts: false,
            ui_scale: 1.0,
            recent_cmds: Vec::new(),
            show_delta: false,
            timing_a: String::new(),
            timing_b: String::new(),
            timing_re: false,
            rules: vec![],
            frame: Default::default(),
            rec_split_mb: 0,
            rec_split_min: 0,
            aliases: Default::default(),
            sniff_port: String::new(),
            bridge_port: 7000,
            vcom_port: String::new(),
            reset: serialx::ResetKind::Dtr,
            profile: String::new(),
            triggers: vec![],
            scripts: vec![],
            dock_tab: 0,
            plot: Default::default(),
            api_mode: false,
            api: Default::default(),
            mb: Default::default(),
            tb: Default::default(),
            cmp: Default::default(),
            send_check: Default::default(),
            multiline: false,
            line_by_line: false,
            line_gap_ms: 100,
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
    Compare,
    Tools,
    Settings,
    /// 社区：需求投票、求助、分享、公告
    Community,
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

mod apiview;
mod blex;
mod dbgtools;
mod dock;
mod logx;
mod minitools;
mod nav;
mod palette;
mod plot;
mod prefs;
mod serialx;
mod sess;
mod sshview;
mod toolbox;
mod toolbox2;
mod toolbox3;
mod toolbox4;
mod toolbox5;

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

    record: Option<Rec>,
    toast: Option<(String, Color32, Instant)>,
    logo: egui::TextureHandle,
    cmd_ui: CmdPanel,
    api: apiview::ApiUi,
    /// 蓝牙后台线程，第一次切到 BLE 标签时才启动
    ble: Option<crate::ble::Hub>,
    ble_filter: String,
    /// 整组依次发送的队列：(组, 剩余数据, 下一条的时间)
    seq: Option<(usize, VecDeque<Vec<u8>>, Instant)>,
    last_rx_at: Option<Instant>,
    last_tx_at: Option<Instant>,
    at_bottom: bool,
    jump_bottom: bool,
    /// 滚动条旁的概览标记：(缓存键, 算的时刻, 每格的标记位)
    ov: (u64, Instant, Vec<u8>),
    /// 中键自动滚动的锚点
    auto_anchor: Option<egui::Pos2>,
    /// 离开底部那一刻的总行数（「回到最新」上显示新增多少行）
    bottom_total: Option<u64>,
    /// Ctrl+G 跳到时间的输入框
    goto_time: Option<String>,

    /// 串口终端模式
    tv: TermView,

    /// TCP 服务器当前的客户端数
    peers: Option<usize>,
    /// 网络连接的自动重连是静默尝试（失败不刷屏）
    net_quiet: bool,
    sub_input: String,

    page: Page,
    code: CodeView,
    gitv: GitView,
    cmpv: crate::compareview::CompareView,
    /// 电路页（第一次打开时才创建）
    circ: Option<Box<crate::circuitview::CircuitView>>,
    /// 社区页
    comm: crate::community::Community,
    /// 其他进程转来的打开 / 比较请求（系统右键菜单）
    fwd: Option<std::sync::Arc<std::sync::Mutex<Vec<crate::shellint::Req>>>>,
    /// 比较页有未保存修改时第一次关窗口被拦下，再关一次就直接退出
    close_anyway: bool,
    /// 底部的命令行终端面板
    shells: Shells,
    upd: Upd,
    upd_tx: Sender<UpdEvt>,
    upd_rx: Receiver<UpdEvt>,
    /// 启动后多久自动检查更新
    upd_check_at: Option<Instant>,
    upd_card: bool,

    km: Keymap,
    palette: Option<palette::Palette>,
    prefs: prefs::Prefs,
    /// 上一次的工程文件夹（共享目录是否跟着换用）
    last_project: String,
    /// 打开设置前所在的页面（关闭设置时回去）
    prev_page: Page,
    nav: nav::Nav,
    /// 第一次启动：再过几帧最大化
    maximize_in: u8,
    /// 命令面板选中后、需要下一帧在对应页面执行的命令
    pending_acts: Vec<Act>,
    bitcalc: crate::tools::BitCalc,
    tb: toolbox::ToolboxUi,

    // ---- v1.0 调试增强
    rules_rt: Vec<logx::RuleRt>,
    marker_n: u32,
    replay: Option<logx::Replay>,
    stats: logx::Stats,
    wins: logx::Wins,
    mini: minitools::Mini,
    dock: dock::Dock,
    mb: dbgtools::Mb,
    xf: dbgtools::Xf,
    calc: dbgtools::Calc,
    plot: plot::Plot,
    /// 串口输入信号线 [CTS, DSR, RI, CD]
    signals: Option<[bool; 4]>,
    sniff: serialx::Sniff,
    bridge: Option<crate::bridge::Bridge>,
    vcom: Option<crate::vcom::Vcom>,
    serial_ui: serialx::SerialUi,
    /// AT 助手提示里选中的一项
    at_sel: usize,
    /// 命令行指定、启动后要打开 / 回放的日志
    open_log: Option<(PathBuf, bool)>,
    /// 停在后台的其它连接（当前连接所在的槽位为 None）
    sessions: Vec<Option<sess::Sess>>,
    /// 正在处理后台连接（不驱动 Modbus / 文件传输这些只属于当前连接的工具）
    bg: bool,
}

/// 正在录制：当前文件、已写字节、开始时间、第几段
struct Rec {
    w: BufWriter<File>,
    base: PathBuf,
    path: PathBuf,
    bytes: u64,
    since: Instant,
    part: u32,
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
        // --open 文件 / --replay 文件：启动后打开或回放日志
        let mut open_log = None;
        let mut extra_port = None;
        // 参数是文件夹：在代码页打开这个工程（qccom ~/fw）
        let mut open_dir = None;
        // --file / --project / --compare 和已有文件的参数交给 shellint 处理（系统右键菜单）
        let startup = crate::shellint::parse_args(&args);
        let mut it = args.into_iter();
        let mut in_req = false;
        while let Some(a) = it.next() {
            match a.as_str() {
                "--open" | "--replay" => open_log = it.next().map(|p| (PathBuf::from(p), a == "--replay")),
                "--file" | "--project" | "--compare" => in_req = true,
                _ if a.starts_with("--") => in_req = false,
                _ if in_req || std::path::Path::new(&a).is_file() => {}
                _ if std::path::Path::new(&a).is_dir() => open_dir = Some(PathBuf::from(&a)),
                _ if extra_port.is_none() => extra_port = Some(a),
                _ => {}
            }
        }
        if let Some(p) = &extra_port {
            s.port = p.clone();
        }
        // 调试页只放串口，网络连接都在工具箱里；启动时先显示串口
        if s.transport == Transport::TcpServer {
            s.tcp_mode = Transport::TcpServer;
        }
        s.transport = Transport::Serial;
        s.api_mode = false;
        let (upd_tx, upd_rx) = mpsc::channel();
        let code = CodeView::new(std::mem::take(&mut s.code));
        let gitv = GitView::new(std::mem::take(&mut s.git), cc.egui_ctx.clone());
        let cmpv = crate::compareview::CompareView::new(std::mem::take(&mut s.cmp));
        crate::hub::set_enabled(s.telemetry);
        crate::hub::start_telemetry();
        let mut comm = crate::community::Community::default();
        comm.seen_notice = s.seen_notice;
        let mut shell_s = std::mem::take(&mut s.shell);
        shell_s.open = false; // 启动时不自动开 shell
        let shells = Shells::new(shell_s);
        let auto_update = s.auto_update;
        let km = Keymap::with_scheme(&s.keys, s.key_scheme);
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
            api: Default::default(),
            ble: None,
            ble_filter: String::new(),
            seq: None,
            last_rx_at: None,
            last_tx_at: None,
            at_bottom: true,
            jump_bottom: false,
            ov: (0, Instant::now(), vec![]),
            auto_anchor: None,
            bottom_total: None,
            goto_time: None,
            tv: {
                let mut tv = TermView::new("term-view", 100, 30);
                tv.focus_req = true;
                tv
            },
            peers: None,
            net_quiet: false,
            sub_input: String::new(),
            page: Page::Debug,
            code,
            gitv,
            cmpv,
            circ: None,
            comm,
            close_anyway: false,
            fwd: None,
            shells,
            upd: Upd::Idle,
            upd_tx,
            upd_rx,
            // 刚更新完的这次启动不再检查，避免服务器版本号没改时反复提示
            upd_check_at: (auto_update && !just_updated).then(|| Instant::now() + Duration::from_secs(3)),
            upd_card: false,
            km,
            palette: None,
            prefs: prefs::Prefs::default(),
            last_project: String::new(),
            prev_page: Page::Debug,
            nav: Default::default(),
            maximize_in: 0,
            pending_acts: Vec::new(),
            bitcalc: Default::default(),
            tb: Default::default(),
            rules_rt: vec![],
            marker_n: 0,
            replay: None,
            stats: Default::default(),
            wins: Default::default(),
            mini: Default::default(),
            dock: Default::default(),
            mb: Default::default(),
            xf: Default::default(),
            calc: Default::default(),
            plot: Default::default(),
            signals: None,
            sniff: Default::default(),
            bridge: None,
            vcom: None,
            serial_ui: Default::default(),
            at_sel: 0,
            open_log,
            sessions: (0..sess::SLOTS).map(|_| None).collect(),
            bg: false,
        };
        app.parse_keywords();
        app.compile_rules();
        app.compile_triggers();
        app.apply_frame();
        if let Some(d) = open_dir {
            app.code.open_root(std::fs::canonicalize(&d).unwrap_or(d));
            app.set_page(Page::Code);
        }
        if let Some(l) = crate::instance_listener() {
            app.fwd = Some(crate::shellint::listen(l, cc.egui_ctx.clone()));
        }
        for r in startup {
            app.handle_req(r, false);
        }
        if just_updated {
            app.toast(tf("已更新到 v{}", &[&env!("CARGO_PKG_VERSION")]), OK());
        } else if !app.code.recovery.is_empty() && app.page != Page::Code {
            app.toast(tf("有 {} 个上次没保存的文件可以恢复，到代码页处理", &[&app.code.recovery.len()]), WARN());
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
            if self.s.transport == Transport::Ble {
                self.conn = self.ble_connect();
                return;
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
                self.signals = None;
                self.stats = Default::default();
                if !self.s.sniff_port.is_empty() {
                    self.open_sniff();
                }
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
        self.signals = None;
        self.close_sniff();
    }

    /// 收到数据（串口、TCP、回放都走这里）
    fn on_rx(&mut self, d: Vec<u8>) {
        self.rx_bytes += d.len() as u64;
        self.last_rx_at = Some(Instant::now());
        self.rate_acc += d.len() as u64;
        let r = self.tv.feed(&d);
        if !r.is_empty() {
            self.send_raw(r);
        }
        if !self.bg {
            self.tools_rx(&d);
        }
        if let Some(b) = self.bridge.as_ref().filter(|_| self.s.transport == Transport::Serial) {
            b.forward(&d);
        }
        if let Some(v) = self.vcom.as_ref().filter(|_| self.s.transport == Transport::Serial) {
            v.forward(&d);
        }
        if self.paused {
            self.pause_buf.extend_from_slice(&d);
        } else {
            self.buf.push_rx(&d, self.s.encoding);
        }
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
        let multi = self.s.multiline && text.contains('\n');
        // 多行逐行发送：每行一包，按间隔排队
        if multi && self.s.line_by_line {
            let mut q = VecDeque::new();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match self.encode_input(line) {
                    Ok(d) => q.push_back(d),
                    Err(e) => {
                        self.repeat = false;
                        self.toast(e, ERR());
                        return;
                    }
                }
            }
            if !from_timer {
                self.remember(text);
            }
            self.hist_pos = None;
            if self.conn.is_none() {
                self.toast(t("未连接"), WARN());
                return;
            }
            self.seq = Some((usize::MAX, q, Instant::now()));
            return;
        }
        let data = if multi && !self.s.hex_send {
            // 整段一起发：行与行之间用所选的换行符
            let le = self.line_ending();
            let sep = if le == LineEnding::None {
                "\n".to_string()
            } else {
                String::from_utf8_lossy(le.bytes()).into_owned()
            };
            let joined = text.lines().collect::<Vec<_>>().join(&sep);
            self.encode_input(&joined)
        } else {
            self.encode_input(&text)
        };
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                self.repeat = false;
                self.toast(e, ERR());
                return;
            }
        };
        if !from_timer && !text.is_empty() {
            self.remember(text);
        }
        self.hist_pos = None;
        self.send_bytes(data);
    }

    /// 命令栏的一段文字 → 要发的字节（HEX 模式自动加校验，文本模式加换行符）
    fn encode_input(&self, text: &str) -> Result<Vec<u8>, String> {
        if self.s.hex_send {
            let mut d = parse_hex(text)?;
            let c = self.s.send_check.bytes(&d);
            d.extend_from_slice(&c);
            Ok(d)
        } else {
            let mut d = self.s.encoding.encode(text);
            d.extend_from_slice(self.line_ending().bytes());
            Ok(d)
        }
    }

    fn remember(&mut self, text: String) {
        self.s.history.retain(|h| h != &text);
        self.s.history.push(text);
        if self.s.history.len() > 100 {
            self.s.history.remove(0);
        }
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
            Ok(d) => match c.target {
                Some(k) if k as usize != self.cur_slot() => self.send_to_slot(k as usize, d),
                _ => self.send_bytes(d),
            },
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
                    self.tv.feed(&line);
                    self.tv.feed(b"\r");
                    if self.paused {
                        self.pause_buf.extend_from_slice(&line);
                    } else {
                        self.buf.close_open();
                        self.buf.push_rx(&line, self.s.encoding);
                    }
                }
                Evt::Opened(..) => {}
                Evt::Data(d) => self.on_rx(d),
                Evt::DataB(d) => self.on_sniff_data(d),
                Evt::Signals(gen, sig) => {
                    if gen == self.conn_gen {
                        self.signals = Some(sig);
                    }
                }
                Evt::Closed(gen, reason) if self.is_sniff_gen(gen) => {
                    if self.sniff.conn.take().is_some() {
                        self.flush_sniff();
                        if let Some(e) = reason {
                            self.buf.sys(tf("✗ 监听口 B 断开：{}", &[&e]));
                        }
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
        if !first_scan {
            let old = std::mem::take(&mut self.ports);
            self.note_plugs(&old, &list);
        }
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
        self.tick_awake();
        self.tick_replay();
        self.tick_dock();
        self.tick_tools();
        if self.s.transport == Transport::Serial {
            self.tick_sniff();
            self.poll_bridge();
            self.poll_vcom();
        }
        self.tick_conn();
    }

    /// 每个连接自己的定时工作：自动重连、速率、定时 / 整组发送、录制（后台连接也跑）
    fn tick_conn(&mut self) {
        self.tick_stats();
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
            self.tick_stats_second();
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
        // 整组依次发送（gi = usize::MAX 是命令栏多行逐行发送）
        if let Some((gi, mut q, next)) = self.seq.take() {
            if self.conn.is_some() {
                if Instant::now() >= next {
                    if let Some(d) = q.pop_front() {
                        self.send_bytes(d);
                    }
                    let ms = if gi == usize::MAX {
                        self.s.line_gap_ms
                    } else {
                        self.s.groups.get(gi).map_or(200, |g| g.interval_ms)
                    };
                    let iv = Duration::from_millis(ms as u64);
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
        let committed = std::mem::take(&mut self.buf.committed);
        // 新收完的行：应答规则、脚本、波形图
        // 自动应答、脚本、波形图只跟当前显示的连接
        let want = !self.bg && (!self.dock.trig_rt.iter().all(Option::is_none) || self.dock.script.is_some() || self.s.plot.on);
        if want {
            let texts: Vec<String> = committed
                .iter()
                .filter_map(|id| self.buf.get(*id))
                .filter(|l| matches!(l.dir, Dir::Rx | Dir::Rx2))
                .map(|l| l.plain.clone())
                .collect();
            for tx in texts {
                self.on_line(&tx);
                self.plot_line(&tx);
            }
        }
        // 录制
        if let Some(r) = &mut self.record {
            let mut err = None;
            for id in &committed {
                if let Some(l) = self.buf.get(*id) {
                    let line = format_line(l, self.s.view);
                    r.bytes += line.len() as u64 + 1;
                    if let Err(e) = writeln!(r.w, "{line}") {
                        err = Some(e);
                        break;
                    }
                }
            }
            if !committed.is_empty() {
                let _ = r.w.flush();
            }
            if let Some(e) = err {
                self.record = None;
                self.toast(tf("录制写入失败：{}", &[&e]), ERR());
            } else {
                self.split_record();
            }
        }
    }

    /// 录制分段：文件太大或时间太长就换下一个文件（name_002.log …）
    fn split_record(&mut self) {
        let Some(r) = &mut self.record else { return };
        let by_size = self.s.rec_split_mb > 0 && r.bytes >= self.s.rec_split_mb as u64 * 1024 * 1024;
        let by_time = self.s.rec_split_min > 0 && r.since.elapsed() >= Duration::from_secs(self.s.rec_split_min as u64 * 60);
        if !(by_size || by_time) {
            return;
        }
        let part = r.part + 1;
        let stem = r.base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = r.base.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let next = r.base.with_file_name(format!("{stem}_{part:03}{ext}"));
        match File::create(&next) {
            Ok(f) => {
                let _ = r.w.flush();
                r.w = BufWriter::new(f);
                r.path = next.clone();
                r.bytes = 0;
                r.since = Instant::now();
                r.part = part;
                self.buf.sys(tf("录制换到新文件：{}", &[&next.display()]));
            }
            Err(e) => {
                self.record = None;
                self.toast(tf("录制写入失败：{}", &[&e]), ERR());
            }
        }
    }

    // ------------------------------------------------------------ 过滤

    fn filtering(&self) -> bool {
        !self.s.show_tx || self.s.min_level != Level::None || (self.filter_only && !self.search.is_empty()) || self.rules_filter()
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
        self.rules_pass(l)
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
                        let (txt, pre) = self.display_text(id, l);
                        out.extend(txt.chars().skip(pre));
                    }
                    out.push('\n');
                }
            }
        }
        Some(out)
    }

    /// 一行显示出来的文字，以及开头时间戳、方向标记占几个字符
    fn display_text(&self, id: u64, l: &Line) -> (String, usize) {
        let job = self.line_job(id, l, &FontId::monospace(12.0));
        let mut pre = if self.s.show_ts { 14 } else { 0 };
        if self.s.show_delta {
            pre += 8;
        }
        pre += match l.dir {
            Dir::Tx => 5,
            Dir::Rx2 => 4,
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
            let (txt, pre) = self.display_text(id, l);
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
            Dir::Rx2 => ink(Color32::from_rgb(0xfb, 0xbf, 0x24)),
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

    fn line_job(&self, id: u64, l: &Line, font: &FontId) -> LayoutJob {
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
        if self.s.show_delta {
            // 和上一行（不管显示没显示）的时间差；超过 1 秒的用黄色，一眼看出卡在哪
            let ms = id.checked_sub(1).and_then(|p| self.buf.get(p)).map_or(0, |p| (l.ts - p.ts).num_milliseconds());
            let c = if ms >= 1000 { WARN() } else { TEXT3() };
            job.append(&logx::fmt_delta(ms), 0.0, fmt(c, Color32::TRANSPARENT));
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
            Dir::Rx2 => job.append(
                "B › ",
                0.0,
                TextFormat {
                    font_id: bold_font.clone(),
                    color: ink(Color32::from_rgb(0xfb, 0xbf, 0x24)),
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
        // 按帧格式切出来的帧（二进制，所以文本视图也按 HEX 显示）：帧头 / 长度 / 校验分别上色，校验错标红
        if let Some(ok) = l.check_ok {
            let roles = self.s.frame.roles(l.raw.len());
            for (b, r) in l.raw.iter().zip(roles) {
                let c = match r {
                    0 => ACCENT(),
                    1 => VIOLET(),
                    3 if ok => OK(),
                    3 => ERR(),
                    _ if !ok => ink(Color32::from_rgb(0xfc, 0xa5, 0xa5)),
                    _ => hex_color,
                };
                job.append(&format!("{b:02X} "), 0.0, fmt(c, Color32::TRANSPARENT));
            }
            if !ok {
                job.append(t("  校验错误"), 0.0, fmt(ERR(), Color32::TRANSPARENT));
            }
            if self.s.view == ViewMode::Mixed {
                job.append("   ", 0.0, fmt(TEXT3(), Color32::TRANSPARENT));
                let txt: String = l.raw.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '·' }).collect();
                job.append(&txt, 0.0, fmt(TEXT3(), Color32::TRANSPARENT));
            }
            return job;
        }
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
        if l.dir != Dir::Sys {
            for r in self.rules_rt.iter().filter(|r| r.act != logx::RuleAct::Hide) {
                for (a, b) in r.m.all(text) {
                    if b <= n && text.is_char_boundary(a) && text.is_char_boundary(b) {
                        fg[a..b].fill(r.color);
                        bg[a..b].fill(r.color.gamma_multiply(0.16));
                    }
                }
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
        if self.page == Page::Tools {
            // 工具箱里的网络工具：标题换成工具名，配置方案只属于串口
            let (title, desc) = toolbox::tool_text(self.s.tb.tool);
            ui.label(RichText::new(title).font(FontId::new(17.0, bold())).color(TEXT()));
            ui.label(RichText::new(desc).small().color(TEXT3()));
        } else {
            let logo = self.logo.clone();
            brand(ui, &logo, |ui| self.profile_button(ui));
        }
        ui.add_space(14.0);

        // 限宽：哪个控件多出几像素也不会把侧边栏撑宽（切换标签时宽度保持一致）
        let w = ui.available_width();
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_width(w).show(ui, |ui| {
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

                    if self.s.api_mode {
                        self.api_sidebar(ui);
                        return;
                    }
                    if self.s.transport == Transport::Ble {
                        self.ble_sidebar(ui);
                    } else if self.s.transport != Transport::Serial {
                        self.net_form(ui, busy);
                        ui.add_space(10.0);
                        self.connect_button(ui);
                        ui.add_space(16.0);
                        if self.fold_head(ui, "link", t("链路"), &self.uptime(), &self.link_summary()) {
                            self.link_card(ui);
                        }
                        ui.add_space(16.0);
                    } else {
                        // ---- 设备
                        let dev_sum = if self.s.port.is_empty() { tf("{} 个", &[&self.ports.len()]) } else { self.s.port.clone() };
                        let dev_open = self.fold_head(ui, "dev", t("设备"), &tf("{} 个", &[&self.ports.len()]), &dev_sum);
                        if dev_open && self.ports.is_empty() {
                            empty_port_card(ui);
                        }
                        for p in self.ports.clone().into_iter().filter(|_| dev_open) {
                            let selected = p.name == self.s.port;
                            let live = self.connected.as_deref() == Some(p.name.as_str());
                            let alias = self.port_alias(&p.name).cloned();
                            let b = !self.s.sniff_port.is_empty() && self.s.sniff_port == p.name;
                            let r = port_card(ui, &p, alias.as_deref(), b, selected, live, !busy);
                            if r.clicked() && !busy {
                                self.s.port = p.name.clone();
                            }
                            r.context_menu(|ui| self.port_menu(ui, &p));
                        }
                        ui.add_space(12.0);

                        // ---- 波特率
                        let baud_sum = format!("{} {}", self.s.cfg.baud, self.s.cfg.short());
                        let baud_open = self.fold_head(ui, "baud", t("波特率"), "", &baud_sum);
                        ui.add_enabled_ui(!busy, |ui| {
                            if !baud_open {
                                return;
                            }
                            ui.horizontal(|ui| {
                                // 右边留给下拉箭头和「自动」按钮，别让这一行把侧边栏撑宽
                                let w = ui.available_width() - 94.0;
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
                        if baud_open {
                            ui.label(
                                RichText::new(tf("{}  ·  流控 {}", &[&self.s.cfg.short(), &[t("无"), t("软件"), t("硬件")][self.s.cfg.flow.min(2) as usize]]))
                                    .small()
                                    .color(TEXT3()),
                            );
                        }
                        ui.add_space(10.0);

                        // ---- 连接按钮
                        self.connect_button(ui);
                        ui.add_space(16.0);

                        // ---- 链路
                        if self.fold_head(ui, "link", t("链路"), &self.uptime(), &self.link_summary()) {
                            self.link_card(ui);
                        }
                        ui.add_space(16.0);
                    }

                    // ---- 显示
                    let view_sum = format!("{} · {}", self.s.encoding.name(), [t("文本"), "HEX", t("混合")][self.s.view as usize]);
                    if !self.fold_head(ui, "view", t("显示"), "", &view_sum) {
                        ui.add_space(12.0);
                        self.advanced_fold(ui);
                        return;
                    }
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
                    switch_row(ui, &mut self.s.show_delta, t("时间差"), t("每行前显示和上一行相隔的时间，看时序很方便"));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("协议分帧")).color(TEXT2()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = if self.s.frame.on { t("已开启") } else { t("设置…") };
                            let b = egui::Button::new(RichText::new(label).size(12.0).color(if self.s.frame.on { ACCENT() } else { TEXT2() }))
                                .fill(BG2())
                                .stroke(st(1.0, if self.s.frame.on { ACCENT().gamma_multiply(0.4) } else { LINE() }))
                                .min_size(vec2(0.0, 22.0));
                            if ui.add(b).on_hover_text(t("按帧头 + 长度 + 校验切分二进制数据，校验错误标红")).clicked() {
                                self.wins.frame = !self.wins.frame;
                            }
                        });
                    });
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

                    self.advanced_fold(ui);
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
            Transport::Ws => {
                ui.add_enabled_ui(!busy, |ui| {
                    let w = &mut self.s.net.ws;
                    card(ui, |ui| {
                        form_row(ui, "URL", |ui| text_field(ui, &mut w.url, "ws://192.168.4.1:81/"));
                        form_row(ui, t("发送"), |ui| {
                            let mut txt = w.text;
                            tab_strip(ui, &mut txt, &[(true, t("文本")), (false, t("二进制"))], 24.0);
                            w.text = txt;
                        });
                        form_row(ui, "Ping", |ui| {
                            ui.add(egui::DragValue::new(&mut w.ping_s).range(0..=600).suffix(" s"))
                                .on_hover_text(t("每隔多少秒发一次 Ping 保活，0 = 不发"));
                        });
                        ui.add_space(4.0);
                        ui.label(RichText::new(t("额外请求头（每行一个，比如 Authorization: Bearer …）")).small().color(TEXT3()));
                        ui.add(
                            egui::TextEdit::multiline(&mut w.headers)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .font(FontId::new(12.0, FontFamily::Monospace)),
                        );
                    });
                });
                ui.label(RichText::new(t("每条消息单独一行，前面标出 txt / bin")).small().color(TEXT3()));
            }
            Transport::Serial | Transport::Ble => {}
        }
    }

    /// 第一次启动：直接最大化。先把「还原」尺寸设成屏幕的 80% 居中，取消最大化时不会缩成小窗口。
    /// 以后由 eframe 记住上次关闭时的大小、位置和是否最大化
    fn place_window(&mut self, ctx: &egui::Context) {
        let Some(screen) = ctx.input(|i| i.viewport().monitor_size) else {
            ctx.request_repaint(); // 屏幕尺寸下一帧才拿得到
            return;
        };
        let size = vec2((screen.x * 0.8).max(900.0).min(screen.x), (screen.y * 0.8).max(560.0).min(screen.y));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2((screen.x - size.x) / 2.0, (screen.y - size.y) / 2.0)));
        // 窗口管理器处理完改大小再最大化，否则最大化会被上面的改大小覆盖
        self.maximize_in = 3;
        self.s.window_placed = true;
    }

    /// 外部文件拖在窗口上时，提示松手会做什么
    fn drop_hint(&mut self, ctx: &egui::Context, pos: Option<egui::Pos2>) {
        let files: Vec<PathBuf> = ctx.input(|i| i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect());
        let n = ctx.input(|i| i.raw.hovered_files.len());
        if n == 0 {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        let Some(pos) = pos else { return };
        let what = if files.len() == 1 { files[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { tf("{} 个文件", &[&n]) };
        let shift = ctx.input(|i| i.modifiers.shift);
        let msg = if self.shells.s.open && self.shells.rect.is_some_and(|r| r.contains(pos)) {
            t("插入路径到终端").to_string()
        } else {
            match self.page {
                Page::Code => match self.code.drop_dir_at(pos) {
                    Some(d) => {
                        let root = self.code.root().map(|r| r.to_path_buf()).unwrap_or_default();
                        let rel = d.strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                        let dir = if rel.is_empty() { root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { rel };
                        if shift { tf("移动到 {}/", &[&dir]) } else { tf("复制到 {}/（按住 Shift 移动）", &[&dir]) }
                    }
                    None if files.len() == 1 && files[0].is_dir() => t("作为工程打开").to_string(),
                    None => t("在编辑器中打开").to_string(),
                },
                Page::Compare => t("添加到比较").to_string(),
                Page::Tools => t("交给当前工具").to_string(),
                _ => t("作为日志打开").to_string(),
            }
        };
        egui::Area::new(egui::Id::new("drop-hint")).order(egui::Order::Tooltip).fixed_pos(pos + vec2(16.0, 14.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(format!("{what}  →  {msg}")).size(12.5).color(TEXT())).extend());
            });
        });
    }

    /// 侧边栏最下面的「高级设置」折叠块
    fn advanced_fold(&mut self, ui: &mut egui::Ui) {
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
    }

    /// 侧边栏分组标题：点击折叠 / 展开，折叠后在右边显示一句摘要；右键可以全部折叠或展开。返回是否展开
    fn fold_head(&mut self, ui: &mut egui::Ui, key: &str, title: &str, note: &str, summary: &str) -> bool {
        let open = !self.s.folded.iter().any(|k| k == key);
        let r = ui
            .horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(10.0, 12.0), Sense::hover());
                let c = rect.center();
                let pts = if open {
                    vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)]
                } else {
                    vec![c + vec2(-1.5, -3.5), c + vec2(2.0, 0.0), c + vec2(-1.5, 3.5)]
                };
                ui.painter().add(Shape::line(pts, st(1.3, TEXT3())));
                ui.label(RichText::new(title).font(FontId::new(11.0, bold())).color(TEXT3()).extra_letter_spacing(1.5));
                let right = if open { note } else { summary };
                if !right.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let c = if open { TEXT3() } else { TEXT2() };
                        ui.add(egui::Label::new(RichText::new(right).font(FontId::new(10.5, FontFamily::Monospace)).color(c)).truncate());
                    });
                }
            })
            .response
            .interact(Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if r.clicked() {
            if open {
                self.s.folded.push(key.to_string());
            } else {
                self.s.folded.retain(|k| k != key);
            }
        }
        r.context_menu(|ui| {
            if ui.button(t("全部折叠")).clicked() {
                self.s.folded = ["dev", "baud", "link", "view"].iter().map(|s| s.to_string()).collect();
                ui.close_menu();
            }
            if ui.button(t("全部展开")).clicked() {
                self.s.folded.clear();
                ui.close_menu();
            }
        });
        ui.add_space(2.0);
        open
    }

    /// 链路分组折叠时的摘要：收发字节数
    fn link_summary(&self) -> String {
        format!("RX {}  TX {}", human_bytes(self.rx_bytes), human_bytes(self.tx_bytes))
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
        let ble_ok = self.s.transport != Transport::Ble || !self.s.net.ble.id.is_empty();
        let enabled = self.autobaud.is_none() && ble_ok && (net || !self.s.port.is_empty() || self.conn.is_some());
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
            // 每列最宽 100，列多时平分，别撑宽侧边栏
            let cols = if self.peers.is_some() { 3.0 } else { 2.0 };
            let sw = ((ui.available_width() - 8.0 * (cols - 1.0)) / cols).min(100.0);
            ui.horizontal(|ui| {
                stat(ui, sw, "RX", &human_bytes(self.rx_bytes), ACCENT());
                stat(ui, sw, "TX", &human_bytes(self.tx_bytes), VIOLET());
                if let Some(n) = self.peers {
                    stat(ui, sw, t("客户端"), &n.to_string(), OK());
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
            if self.s.transport == Transport::Serial && self.conn.is_some() {
                self.signals_ui(ui);
                self.bridge_ui(ui);
            }
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
                self.tv.focus_req = self.s.terminal;
            }
            ui.add_space(4.0);
            if self.s.terminal {
                self.term_header(ui);
                return;
            }
            // 先从右往左放按钮，剩下的宽度给搜索框和级别标签
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.header_right(ui);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.header_left(ui));
            });
        });
    }

    fn header_left(&mut self, ui: &mut egui::Ui) {
        // 搜索框
        // 至少留一个级别标签的位置，窄窗口时搜索框让位
        let avail = ui.available_width();
        let all_pills = avail >= 160.0 + 70.0 + 5.0 * 70.0;
        let search_w = if all_pills {
            (avail - 70.0 - 5.0 * 70.0).min(260.0)
        } else {
            (avail - 70.0 - 140.0).clamp(60.0, 260.0)
        };
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
                    // 搜索框里的「只看匹配」开关，不占工具栏位置
                    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
                    let on = self.filter_only;
                    if on {
                        ui.painter().rect_filled(r, 4.0, ACCENT().gamma_multiply(0.15));
                    }
                    draw_icon(
                        ui.painter(),
                        r.shrink(2.0),
                        Icon::Lines,
                        if on {
                            ACCENT()
                        } else if resp.hovered() {
                            TEXT()
                        } else {
                            TEXT3()
                        },
                    );
                    if resp.on_hover_text(format!("{}\n{}", t("只看匹配"), t("只显示包含搜索词的行"))).clicked() {
                        self.filter_only = !self.filter_only;
                        self.view_dirty = true;
                    }
                });
            });
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
        let fits = if all_pills { lv.len() } else { 1 };
        if fits < lv.len() {
            // 放不下全部级别时收成一个下拉：显示当前级别，点开选
            let (_, name, n, col) = lv.iter().find(|x| x.0 == self.s.min_level).copied().unwrap_or(lv[0]);
            let r = level_pill(ui, name, n, col, self.s.min_level != Level::None);
            let (cr, _) = ui.allocate_exact_size(vec2(8.0, 26.0), Sense::hover());
            let c = cr.center() - vec2(4.0, 0.0);
            ui.painter().add(Shape::line(vec![c + vec2(-3.0, -1.5), c + vec2(0.0, 1.5), c + vec2(3.0, -1.5)], st(1.3, TEXT3())));
            let r = r.on_hover_text(t("按级别筛选"));
            let pop = egui::Id::new("level_pop");
            if r.clicked() {
                ui.memory_mut(|m| m.toggle_popup(pop));
            }
            egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                ui.set_min_width(120.0);
                for (l, name, n, col) in lv {
                    if level_pill(ui, name, n, col, self.s.min_level == l).clicked() {
                        self.s.min_level = l;
                        self.view_dirty = true;
                    }
                }
            });
        } else {
            for (l, name, n, col) in lv {
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
        }
        ui.spacing_mut().item_spacing.x = 8.0;
    }

    fn header_right(&mut self, ui: &mut egui::Ui) {
        // 右侧：操作按钮（从右往左）
        if icon_button(ui, Icon::Commands, t("快捷命令面板  Ctrl+J"), self.s.cmd_panel).clicked() {
            self.s.cmd_panel = !self.s.cmd_panel;
        }
        ui.add_space(2.0);
        let (r, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
        ui.painter().vline(r.center().x, r.y_range(), st(1.0, LINE()));
        ui.add_space(2.0);
        let more = icon_button(ui, Icon::More, t("更多：标记、帧格式、统计、回放、Modbus、发送文件…"), false);
        let pop = egui::Id::new("log_more");
        if more.clicked() {
            ui.memory_mut(|m| m.toggle_popup(pop));
        }
        egui::popup::popup_below_widget(ui, pop, &more, egui::PopupCloseBehavior::CloseOnClick, |ui| self.more_menu(ui));
        let rules_on = self.s.rules.iter().any(|r| r.on);
        if icon_button(ui, Icon::Filter, &format!("{}  {}", t("过滤与高亮规则"), self.km.label(Act::LogRules)), rules_on || self.wins.rules).clicked() {
            self.wins.rules = !self.wins.rules;
        }
        if icon_button(ui, Icon::Chart, &format!("{}  {}", t("波形图：把日志里的数字画成曲线"), self.km.label(Act::TogglePlot)), self.s.plot.on).clicked() {
            self.s.plot.on = !self.s.plot.on;
        }
        let rec = self.record.is_some();
        if icon_button(ui, Icon::Record, if rec { t("停止录制") } else { t("录制：把之后收到的每一行实时写入文件") }, rec).clicked() {
            self.toggle_record();
        }
        if icon_button(ui, Icon::Save, t("保存日志  Ctrl+S"), false).clicked() {
            self.save();
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
        ui.add_space(8.0);
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
        if let Some(mut r) = self.record.take() {
            let _ = r.w.flush();
            self.buf.sys(tf("录制结束：{}", &[&r.path.display()]));
            return;
        }
        let name = chrono::Local::now().format("uart_rec_%Y%m%d_%H%M%S.log").to_string();
        if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
            match File::create(&path) {
                Ok(f) => {
                    self.buf.sys(tf("开始录制到 {}", &[&path.display()]));
                    self.record = Some(Rec {
                        w: BufWriter::new(f),
                        base: path.clone(),
                        path,
                        bytes: 0,
                        since: Instant::now(),
                        part: 1,
                    });
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
                    let id = egui::Id::new("send");
                    // AT 指令提示：输入 AT 开头时弹出候选，↑↓ 选择、Tab 补全
                    let at = if focused && !self.s.hex_send && !self.s.multiline {
                        atcmds::matches(&self.send_text, 7)
                    } else {
                        vec![]
                    };
                    if at.is_empty() {
                        self.at_sel = 0;
                    } else {
                        self.at_sel = self.at_sel.min(at.len() - 1);
                    }
                    let mut completed = false;
                    if !at.is_empty() {
                        let (tab, up, down) = ui.input_mut(|i| {
                            (
                                i.consume_key(egui::Modifiers::NONE, Key::Tab),
                                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                            )
                        });
                        if up {
                            self.at_sel = self.at_sel.checked_sub(1).unwrap_or(at.len() - 1);
                        }
                        if down {
                            self.at_sel = (self.at_sel + 1) % at.len();
                        }
                        if tab {
                            self.send_text = at[self.at_sel].0.to_string();
                            completed = true;
                        }
                    }
                    let font = FontId::new(13.5, FontFamily::Monospace);
                    let r = if self.s.multiline {
                        // Enter 换行，Ctrl+Enter 发送
                        let send_now = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Enter));
                        let r = ui.add(
                            egui::TextEdit::multiline(&mut self.send_text)
                                .id(id)
                                .frame(false)
                                .hint_text(RichText::new(t("多行输入：Enter 换行，Ctrl+Enter 发送")).color(TEXT3()))
                                .desired_width(w)
                                .desired_rows(3)
                                .font(font),
                        );
                        if send_now {
                            self.send_input(false);
                            if !self.repeat {
                                self.send_text.clear();
                            }
                        }
                        r
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.send_text)
                                .id(id)
                                .frame(false)
                                .hint_text(RichText::new(hint).color(TEXT3()))
                                .desired_width(w)
                                .font(font),
                        )
                    };
                    if completed {
                        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                            let n = self.send_text.chars().count();
                            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(n))));
                            state.store(ui.ctx(), id);
                        }
                    }
                    if self.focus_send {
                        r.request_focus();
                        self.focus_send = false;
                    }
                    if !at.is_empty() {
                        self.at_popup(ui, r.rect, &at);
                    } else if focused && !self.s.hex_send {
                        if let Some(d) = atcmds::describe(&self.send_text) {
                            self.at_popup(ui, r.rect, &[("", d)]);
                        }
                    }
                    if r.has_focus() && !self.s.multiline && at.is_empty() {
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
                    if !self.s.multiline && r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
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
                        if self.s.hex_send {
                            check_combo(ui, &mut self.s.send_check);
                        }
                        toggle_chip(ui, &mut self.s.hex_send, "HEX", t("按十六进制字节发送"));
                        if toggle_chip(ui, &mut self.s.multiline, "¶", t("多行输入：Enter 换行，Ctrl+Enter 发送")) {
                            self.focus_send = true;
                        }
                        if self.s.multiline {
                            toggle_chip(ui, &mut self.s.line_by_line, t("逐行"), t("每行单独发一包，间隔可在设置里调整"));
                        }
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

    /// 命令栏上方的 AT 指令候选列表
    fn at_popup(&self, ui: &egui::Ui, anchor: Rect, items: &[(&str, &str)]) {
        egui::Area::new(egui::Id::new("at_popup"))
            .order(egui::Order::Foreground)
            .pivot(Align2::LEFT_BOTTOM)
            .fixed_pos(anchor.left_top() - vec2(4.0, 10.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(300.0);
                    for (i, (cmd, desc)) in items.iter().enumerate() {
                        let on = i == self.at_sel && !cmd.is_empty();
                        egui::Frame::new()
                            .fill(if on { ACCENT().gamma_multiply(0.12) } else { Color32::TRANSPARENT })
                            .corner_radius(4.0)
                            .inner_margin(egui::Margin::symmetric(6, 2))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if !cmd.is_empty() {
                                        ui.label(RichText::new(*cmd).font(FontId::new(12.5, FontFamily::Monospace)).color(if on { ACCENT() } else { TEXT() }));
                                    }
                                    ui.label(RichText::new(*desc).size(12.0).color(TEXT2()));
                                });
                            });
                    }
                    if !items[0].0.is_empty() {
                        ui.label(RichText::new(t("↑↓ 选择    Tab 补全")).small().color(TEXT3()));
                    }
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
            if let Some(Rec { path: p, .. }) = &self.record {
                let t = ui.input(|i| i.time);
                let blink = (t * 2.0) as i64 % 2 == 0;
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                ui.label(RichText::new("● REC").font(FontId::new(11.0, mono_bold())).color(if blink { ERR() } else { ERR().gamma_multiply(0.4) }))
                    .on_hover_text(p.display().to_string());
            }
            self.status_extras(ui);
            self.status_conns(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().color(TEXT3()));
                let br = self.code.branch();
                if !br.is_empty() {
                    let r = ui
                        .add(egui::Label::new(RichText::new(br).small().color(TEXT3())).sense(Sense::click()))
                        .on_hover_text(t("当前 Git 分支，点击打开 Git 页"));
                    let (ir, _) = ui.allocate_exact_size(vec2(11.0, 11.0), Sense::hover());
                    draw_icon(ui.painter(), ir, Icon::Branch, TEXT3());
                    if r.clicked() {
                        self.set_page(Page::Git);
                    }
                }
                let r = ui
                    .add(egui::Label::new(RichText::new(t("⌨ 快捷键")).small().color(TEXT3())).sense(Sense::click()))
                    .on_hover_ui(|ui| shortcuts_help(ui, &self.km));
                if r.clicked() {
                    self.open_settings(prefs::Cat::Keys);
                }
                if let Some(p) = self.km.pending() {
                    ui.label(RichText::new(tf("已按下 {}，等待第二个按键…", &[&p])).small().color(ACCENT()));
                }
                let r = ui
                    .add(egui::Label::new(RichText::new(tf("{} 行", &[&group_digits(self.buf.lines.len() as u64)])).small().color(TEXT3())).sense(Sense::click()))
                    .on_hover_text(t("点击查看统计"));
                if r.clicked() {
                    self.wins.stats = !self.wins.stats;
                }
                ui.label(RichText::new(self.s.encoding.name()).small().color(TEXT3()));
            });
        });
    }

    /// 状态栏里其它在线的连接：点一下切过去
    fn status_conns(&mut self, ui: &mut egui::Ui) {
        let shown = matches!(self.page, Page::Debug | Page::Tools);
        let mut go = None;
        for c in self.live_conns() {
            // 当前页面正显示着的连接左边已经有了
            if c.current && shown {
                continue;
            }
            ui.add_space(6.0);
            let kind = match c.transport {
                Transport::Serial => t("串口"),
                Transport::TcpClient | Transport::TcpServer => "TCP",
                Transport::Udp => "UDP",
                Transport::Mqtt => "MQTT",
                Transport::Ble => "BLE",
                Transport::Ws => "WS",
            };
            let col = if c.up { OK() } else { WARN() };
            let text = format!("● {kind} {}", truncate(&c.name, 22));
            let r = ui.add(egui::Label::new(RichText::new(text).size(11.5).color(if c.unread { col } else { col.gamma_multiply(0.75) })).sense(Sense::click()));
            if r.on_hover_text(t("后台连接：点击切过去")).clicked() {
                go = Some(c.slot);
            }
        }
        if let Some(k) = go {
            self.goto_slot(k);
        }
    }

    /// 跳到某个连接所在的页面
    fn goto_slot(&mut self, k: usize) {
        match toolbox::tool_of_slot(k) {
            Some(tool) => self.open_tool(tool),
            None => self.set_page(Page::Debug),
        }
        self.use_slot(k);
    }

    /// 状态栏里的回放、脚本、网络共享、监听口指示（点一下就能操作）
    fn status_extras(&mut self, ui: &mut egui::Ui) {
        let chip = |ui: &mut egui::Ui, text: String, col: Color32, tip: &str| ui.add(egui::Label::new(RichText::new(text).size(11.5).color(col)).sense(Sense::click())).on_hover_text(tip);
        if let Some(r) = &self.replay {
            let (name, pct, speed, paused) = (r.name.clone(), r.progress(), r.speed, r.paused);
            ui.add_space(6.0);
            let text = if paused {
                format!("▶ {} · {}", t("已暂停"), truncate(&name, 24))
            } else {
                format!("▶ {} {:.0}%", truncate(&name, 24), pct * 100.0)
            };
            if chip(ui, text, if paused { WARN() } else { ACCENT() }, t("回放中：点击暂停 / 继续")).clicked() {
                if let Some(r) = self.replay.as_mut() {
                    r.paused = !r.paused;
                }
            }
            let sp = if speed >= 1000.0 { t("最快").to_string() } else { format!("{speed}×") };
            if chip(ui, sp, TEXT2(), t("点击切换回放速度")).clicked() {
                if let Some(r) = self.replay.as_mut() {
                    r.speed = match r.speed {
                        s if s < 1.0 => 1.0,
                        s if s < 2.0 => 2.0,
                        s if s < 4.0 => 4.0,
                        s if s < 10.0 => 10.0,
                        s if s < 1000.0 => 1000.0,
                        _ => 0.5,
                    };
                }
            }
            if chip(ui, "■".into(), TEXT3(), t("停止回放")).clicked() {
                self.replay = None;
                self.buf.sys(t("回放已停止"));
            }
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        if self.s.frame.on {
            ui.add_space(6.0);
            if chip(ui, t("按帧格式分帧").to_string(), ACCENT(), t("收到的数据正按帧格式切分：点击修改或关闭")).clicked() {
                self.wins.frame = true;
            }
        }
        if let Some(sc) = &self.dock.script {
            let name = sc.name.clone();
            ui.add_space(6.0);
            if chip(ui, format!("▶ {}", truncate(&name, 20)), VIOLET(), t("脚本运行中：点击停止")).clicked() {
                if let Some(sc) = &self.dock.script {
                    sc.stop();
                }
            }
        }
        if self.vcom.is_some() {
            ui.add_space(6.0);
            let other = self
                .vcom
                .as_ref()
                .and_then(|v| v.names.first())
                .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string())
                .unwrap_or_default();
            if chip(ui, format!("{} {other}", t("虚拟串口")), OK(), t("其他软件正通过虚拟串口同时使用设备：点击关闭")).clicked() {
                self.toggle_vcom();
            }
        }
        if let Some(b) = &self.bridge {
            ui.add_space(6.0);
            let text = format!("TCP :{} · {}", b.port, b.clients);
            if chip(ui, text, if b.clients > 0 { OK() } else { TEXT2() }, t("串口已共享到网络：点击停止")).clicked() {
                self.toggle_bridge();
            }
        }
        if self.sniff.conn.is_some() {
            ui.add_space(6.0);
            let port = self.s.sniff_port.rsplit(['/', '\\']).next().unwrap_or("").to_string();
            chip(ui, format!("B {port}"), WARN(), t("监听口 B 已打开，收到的数据以 B › 标出"));
        }
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
            empty_state(
                ui,
                card_rect,
                self.ports.is_empty() && self.s.transport == Transport::Serial,
                self.conn.is_some(),
                self.s.transport != Transport::Serial,
                &self.logo,
            );
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
        let mut ctx_copy_line: Option<String> = None;
        let mut ctx_mark: Option<u64> = None;
        let mut ctx_trigger: Option<String> = None;
        let mut ctx_regs: Option<String> = None;
        let has_svd = self.code.svd.is_some();
        let sel = self.sel.map(|(a, b)| (a.min(b), a.max(b)));
        let tsel = self.tsel.map(|s| s.ordered());
        let sel_fill = ui.visuals().selection.bg_fill;
        // 按下 / 拖动 / 双击落在哪一行哪个字
        let mut press_at: Option<(u64, usize)> = None;
        let mut drag_at: Option<(u64, usize)> = None;
        let mut word_at: Option<(u64, usize, usize)> = None;
        let mut vis_first: Option<(u64, usize)> = None;
        let mut vis_last: Option<(u64, usize)> = None;
        // —— 快速滚动：Alt+滚轮 十倍速、翻页 / 首尾键、中键自动滚动 ——
        let hovered = ui.rect_contains_pointer(inner);
        let max_y = (n as f32 * row_h - inner.height()).max(0.0);
        let mut want: Option<f32> = None;
        if hovered {
            let (alt, dy) = ui.input(|i| (i.modifiers.alt, i.raw_scroll_delta.y));
            if alt && dy != 0.0 {
                want = Some(self.log_scroll_y - dy * 10.0);
                ui.input_mut(|i| {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    i.raw_scroll_delta = egui::Vec2::ZERO;
                });
            }
        }
        if ui.memory(|m| m.focused().is_none()) && self.goto_time.is_none() {
            let page = (inner.height() - row_h * 2.0).max(row_h);
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::PageUp) {
                    want = Some(self.log_scroll_y - page);
                }
                if i.consume_key(Modifiers::NONE, Key::PageDown) {
                    want = Some(self.log_scroll_y + page);
                }
                if i.consume_key(Modifiers::COMMAND, Key::Home) {
                    want = Some(0.0);
                }
                if i.consume_key(Modifiers::COMMAND, Key::End) {
                    self.jump_bottom = true;
                    self.auto_scroll = true;
                }
            });
        }
        let (mid_down, mid_up, other_click, esc, hover) = ui.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Middle),
                i.pointer.button_released(egui::PointerButton::Middle),
                i.pointer.primary_pressed() || i.pointer.secondary_pressed(),
                i.key_pressed(Key::Escape),
                i.pointer.hover_pos(),
            )
        });
        if let Some(a) = self.auto_anchor {
            // 再按一下中键 / 任意点击 / Esc 退出；按住拖动后松开也退出
            if mid_down || other_click || esc || mid_up && hover.is_some_and(|p| (p.y - a.y).abs() > 16.0) {
                self.auto_anchor = None;
            } else if let Some(p) = hover {
                let d = p.y - a.y;
                if d.abs() > 8.0 {
                    let dt = ui.input(|i| i.stable_dt).min(0.05);
                    want = Some(self.log_scroll_y + d.signum() * (d.abs() - 8.0).powf(1.25) * 4.0 * dt);
                }
                ui.ctx().set_cursor_icon(if d > 8.0 {
                    egui::CursorIcon::ResizeSouth
                } else if d < -8.0 {
                    egui::CursorIcon::ResizeNorth
                } else {
                    egui::CursorIcon::AllScroll
                });
                ui.ctx().request_repaint();
            }
        } else if mid_down && hovered && n > 0 {
            self.auto_anchor = hover;
        }
        if let Some(y) = want {
            sa = sa.vertical_scroll_offset(y.clamp(0.0, max_y));
        }
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
                let job = self.line_job(id, l, &font);
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
                        Dir::Rx2 => Some(ink(Color32::from_rgb(0xfb, 0xbf, 0x24)).gamma_multiply(0.7)),
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
                if l.bookmark {
                    // 书签：行号左边一个小旗
                    let x = rect.left() + 6.0;
                    let y = rect.center().y;
                    p.add(Shape::convex_polygon(
                        vec![pos2(x, y - 5.0), pos2(x + 6.0, y - 5.0), pos2(x + 6.0, y + 5.0), pos2(x + 3.0, y + 2.5), pos2(x, y + 5.0)],
                        ACCENT(),
                        egui::Stroke::NONE,
                    ));
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
                    if l.dir != Dir::Sys && menu_item(ui, t("复制为 HEX"), "") {
                        ctx_copy = Some(l.raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
                        ui.close_menu();
                    }
                    if menu_item(ui, if l.bookmark { t("移除书签") } else { t("添加书签") }, &self.km.label(Act::LogBookmark)) {
                        ctx_mark = Some(id);
                        ui.close_menu();
                    }
                    if has_svd && l.plain.contains(|c: char| c == '=' || c == ':') && menu_item(ui, t("按 SVD 解码寄存器值"), "") {
                        ctx_regs = Some(l.plain.clone());
                        ui.close_menu();
                    }
                    if menu_item(ui, t("发到社区求助"), "") {
                        ctx_cmd = Some(if has_sel { 4 } else { 5 });
                        ctx_copy_line = Some(l.plain.clone());
                        ui.close_menu();
                    }
                    if l.dir != Dir::Sys && !l.plain.trim().is_empty() && menu_item(ui, t("用此行新建应答规则"), "") {
                        ctx_trigger = Some(l.plain.trim().to_string());
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
            Some(4) | Some(5) => {
                let sel = if ctx_cmd == Some(5) {
                    None
                } else if self.tsel.is_some() {
                    self.tsel_text(true)
                } else {
                    self.selected_text(true)
                };
                if let Some(text) = sel.filter(|x| !x.trim().is_empty()).or(ctx_copy_line) {
                    self.comm.ask_with(&text, "log");
                    self.set_page(Page::Community);
                }
            }
            _ => {}
        }
        if let Some(tv) = ctx_copy {
            ui.ctx().copy_text(tv);
            self.toast(t("已复制"), OK());
        }
        if let Some(id) = ctx_mark {
            self.toggle_bookmark(id);
        }
        if let Some(text) = ctx_regs {
            if !self.code.decode_regs(&text) {
                self.toast(t("这一行里没认出寄存器值"), TEXT3());
            }
        }
        if let Some(pat) = ctx_trigger {
            self.s.triggers.push(dock::Trigger { pat, ..Default::default() });
            self.dock.trig_open = Some(self.s.triggers.len() - 1);
            self.compile_triggers();
            self.s.dock_tab = 1;
            self.s.cmd_panel = true;
        }
        if fading {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        self.log_scroll_y = out.state.offset.y;
        self.at_bottom = out.state.offset.y + out.inner_rect.height() >= out.content_size.y - row_h * 1.5;
        if self.at_bottom {
            self.bottom_total = None;
        } else if self.bottom_total.is_none() {
            self.bottom_total = Some(self.buf.total());
        }
        if n as f32 * row_h > out.inner_rect.height() {
            self.overview(ui, out.inner_rect, n, open);
        }
        if let Some(a) = self.auto_anchor {
            let p = ui.painter();
            p.circle(a, 13.0, BG2().gamma_multiply(0.92), st(1.0, ACCENT().gamma_multiply(0.7)));
            p.circle_filled(a, 2.0, ACCENT());
            for s in [-1.0f32, 1.0] {
                let c = a + vec2(0.0, s * 7.0);
                p.add(Shape::convex_polygon(
                    vec![c + vec2(-3.5, -s * 1.5), c + vec2(3.5, -s * 1.5), c + vec2(0.0, s * 2.5)],
                    ACCENT(),
                    egui::Stroke::NONE,
                ));
            }
        }
        self.goto_time_box(ui, card_rect, n, open);
        // 往上翻时出现「回到最新」浮动按钮，带上离开后新来了多少行
        if !self.at_bottom && n > 0 {
            let fresh = self.bottom_total.map(|b| self.buf.total().saturating_sub(b)).unwrap_or(0);
            let label = if fresh > 0 { tf("回到最新（+{} 行）", &[&fresh]) } else { t("回到最新").to_string() };
            let lw = ui.fonts(|f| f.layout_no_wrap(label.clone(), FontId::new(12.5, bold()), TEXT()).size().x);
            let r = Rect::from_center_size(pos2(card_rect.center().x, card_rect.bottom() - 26.0), vec2(lw + 48.0, 30.0));
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
            p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, label, FontId::new(12.5, bold()), TEXT());
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

impl App {
    /// 滚动条旁的概览：错误红、警告黄、书签青、搜索命中白，一眼看出整份日志哪里有事
    fn overview(&mut self, ui: &egui::Ui, rect: Rect, n: usize, open: Option<u64>) {
        use std::hash::{Hash, Hasher};
        let cells = ((rect.height() / 3.0) as usize).max(1);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (n, cells, self.buf.total(), self.buf.dropped, &self.search, self.s.min_level as u8, self.s.show_tx).hash(&mut h);
        let key = h.finish();
        let stale = self.ov.1.elapsed() > Duration::from_millis(if key == self.ov.0 { 2000 } else { 300 });
        if stale || self.ov.2.len() != cells {
            let mut v = vec![0u8; cells];
            let search = (!self.search.is_empty()).then_some(self.search.as_str());
            for i in 0..n {
                let Some(l) = self.buf.get(self.row_id(i, open)) else { continue };
                let mut m = match l.level {
                    Level::Error => 1,
                    Level::Warn => 2,
                    _ => 0,
                };
                if l.bookmark {
                    m |= 4;
                }
                if search.is_some_and(|q| find_ci(&l.plain, q).is_some()) {
                    m |= 8;
                }
                if m != 0 {
                    v[i * cells / n] |= m;
                }
            }
            self.ov = (key, Instant::now(), v);
        }
        let p = ui.painter();
        let ch = rect.height() / cells as f32;
        for (i, &m) in self.ov.2.iter().enumerate() {
            if m == 0 {
                continue;
            }
            let y = rect.top() + i as f32 * ch;
            let c = if m & 1 != 0 {
                ERR()
            } else if m & 2 != 0 {
                WARN()
            } else if m & 4 != 0 {
                ACCENT()
            } else {
                TEXT()
            };
            let r = Rect::from_min_max(pos2(rect.right() - 4.0, y), pos2(rect.right() - 1.0, y + ch.max(2.0)));
            p.rect_filled(r, 0.0, c.gamma_multiply(0.85));
            // 搜索命中同时有错误时，左边再补一道白
            if m & 8 != 0 && m & 7 != 0 {
                p.rect_filled(r.translate(vec2(-4.0, 0.0)), 0.0, TEXT().gamma_multiply(0.7));
            }
        }
    }

    /// Ctrl+G：输入时间，跳到那个时刻（或之后第一行）
    fn goto_time_box(&mut self, ui: &mut egui::Ui, card: Rect, n: usize, open: Option<u64>) {
        let Some(mut text) = self.goto_time.take() else { return };
        let mut keep = true;
        let mut go = false;
        let r = Rect::from_center_size(pos2(card.center().x, card.top() + 30.0), vec2(300.0, 36.0));
        let first = (n > 0).then(|| self.buf.get(self.row_id(0, open)).map(|l| l.ts)).flatten();
        let last = (n > 0).then(|| self.buf.get(self.row_id(n - 1, open)).map(|l| l.ts)).flatten();
        egui::Area::new(ui.id().with("goto_time")).fixed_pos(r.min).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(BG2())
                .stroke(st(1.0, ACCENT().gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .shadow(epaint::Shadow {
                    offset: [0, 6],
                    blur: 18,
                    spread: 0,
                    color: shade(170),
                })
                .show(ui, |ui| {
                    ui.set_width(r.width() - 20.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("跳到时间")).color(TEXT2()).size(12.5));
                        let hint = first.map(|f| f.format("%H:%M:%S").to_string()).unwrap_or_else(|| "12:30:05".into());
                        let te = ui.add(egui::TextEdit::singleline(&mut text).hint_text(hint).desired_width(120.0).font(TextStyle::Monospace));
                        if !te.has_focus() && !te.lost_focus() {
                            te.request_focus();
                        }
                        if te.lost_focus() {
                            if ui.input(|i| i.key_pressed(Key::Enter)) {
                                go = true;
                            } else {
                                keep = false;
                            }
                        }
                        if let (Some(f), Some(l)) = (first, last) {
                            ui.label(RichText::new(format!("{}–{}", f.format("%H:%M"), l.format("%H:%M"))).color(TEXT3()).size(11.5));
                        }
                    });
                });
        });
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            keep = false;
        }
        if go {
            match (parse_hms(&text), first, last) {
                (Some(tm), Some(f), Some(l)) => {
                    // 先按最后一行的日期拼；比最后一行还晚就往前一天（跨零点的日志）
                    let mut target = l.date_naive().and_time(tm);
                    if target > l.naive_local() && l.date_naive() > f.date_naive() {
                        target -= chrono::Duration::days(1);
                    }
                    let (mut lo, mut hi) = (0usize, n);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        let ts = self.buf.get(self.row_id(mid, open)).map(|l| l.ts.naive_local());
                        if ts.is_some_and(|t| t < target) {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    let i = lo.min(n - 1);
                    let id = self.row_id(i, open);
                    self.auto_scroll = false;
                    self.scroll_to = Some(id);
                    self.sel = Some((id, id));
                    keep = false;
                }
                (None, _, _) => {
                    self.toast(t("时间格式：时:分[:秒[.毫秒]]"), WARN());
                }
                _ => keep = false,
            }
        }
        if keep {
            self.goto_time = Some(text);
        }
    }
}

/// "12:30"、"12:30:05"、"12:30:05.250" → 时间
fn parse_hms(s: &str) -> Option<chrono::NaiveTime> {
    let s = s.trim().replace('：', ":");
    for f in ["%H:%M:%S%.f", "%H:%M:%S", "%H:%M"] {
        if let Ok(t) = chrono::NaiveTime::parse_from_str(&s, f) {
            return Some(t);
        }
    }
    None
}

// ---------------------------------------------------------------- 终端

impl App {
    fn term_view(&mut self, ui: &mut egui::Ui) {
        let enc = self.s.encoding;
        let empty = if self.conn.is_none() {
            let e = (t("终端模式"), t("先在左侧连接串口，然后在这里直接和设备交互"));
            [e, e]
        } else {
            [(t("已连接"), t("点击这里，键盘输入会直接发给设备")), (t("已连接"), t("直接打字，按回车唤出设备的提示符"))]
        };
        let o = self.tv.show(ui, &|s| enc.encode(s), Some(empty), false);
        if !o.input.is_empty() {
            self.send_raw(o.input);
        }
        if let Some(m) = o.toast {
            self.toast(m, OK());
        }
    }

    fn term_header(&mut self, ui: &mut egui::Ui) {
        let s = &self.tv.term.s;
        let mono = FontId::new(11.5, FontFamily::Monospace);
        ui.label(RichText::new(format!("{}×{}", s.cols, s.rows)).font(mono.clone()).color(TEXT3()))
            .on_hover_text(t("终端当前的列数 × 行数（随窗口大小变化）"));
        if s.in_alt() {
            pill(ui, t("全屏程序"), VIOLET());
        }
        if self.tv.scroll > 0 {
            pill(ui, &tf("回滚 {} 行", &[&self.tv.scroll]), WARN());
        }
        let hint = if self.conn.is_none() {
            t("先在左侧连接串口")
        } else if ui.memory(|m| m.has_focus(self.tv.id())) {
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
                let tv = self.tv.screen_text();
                ui.ctx().copy_text(tv);
                self.toast(t("已复制整屏"), OK());
            }
            if icon_button(ui, Icon::Trash, t("清屏并重置终端（不影响设备）"), false).clicked() {
                self.tv.reset();
                self.tv.scroll = 0;
                self.tv.sel = None;
                self.tv.focus_req = true;
            }
            let b = egui::Button::new(RichText::new(t("同步窗口大小")).size(12.0).color(TEXT2()))
                .fill(BG2())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_WIDGET)
                .min_size(vec2(0.0, 24.0));
            let tip = t("把当前行列数告诉设备（发送 stty rows R cols C），\nvi、top、less 等全屏程序就能铺满窗口。\n只适用于 Linux shell，请在提示符下点击。");
            if ui.add_enabled(self.conn.is_some(), b).on_hover_text(tip).clicked() {
                let (c, r) = (self.tv.term.s.cols, self.tv.term.s.rows);
                self.send_raw(format!("stty rows {r} cols {c}\r").into_bytes());
                self.tv.focus_req = true;
            }
        });
    }
}

impl App {
    /// 调试页的命令（按键已经由 Keymap 翻译好）
    fn shortcuts(&mut self, ctx: &egui::Context, acts: &[Act], term_focused: bool) {
        for a in acts {
            match *a {
                Act::ToggleTerminal => {
                    self.s.terminal = !self.s.terminal;
                    self.tv.focus_req = self.s.terminal;
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
                Act::LogGotoTime => {
                    if !self.s.terminal {
                        self.goto_time = Some(String::new());
                    }
                }
                Act::LogClear => self.clear(),
                Act::LogSave => self.save(),
                Act::LogPause => self.toggle_pause(),
                Act::LogSelectAll => self.select_all(),
                Act::FocusCmdBar => self.focus_send = true,
                Act::ToggleSidebar => self.s.sidebar = !self.s.sidebar,
                Act::ToggleCmdPanel => self.s.cmd_panel = !self.s.cmd_panel,
                Act::LogMarker => self.insert_marker(),
                Act::LogBookmark => {
                    let id = self.sel.map(|s| s.1).unwrap_or(self.buf.total().saturating_sub(1));
                    self.toggle_bookmark(id);
                }
                Act::LogNextMark => self.next_bookmark(false),
                Act::LogPrevMark => self.next_bookmark(true),
                Act::LogRules => self.wins.rules = !self.wins.rules,
                Act::LogStats => self.wins.stats = !self.wins.stats,
                Act::LogOpen => self.open_log_file(false),
                Act::TogglePlot => self.s.plot.on = !self.s.plot.on,
                Act::Modbus => self.mb.open = !self.mb.open,
                Act::SendFile => self.xf.open = !self.xf.open,
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
                Act::OpenProject => {
                    self.set_page(Page::Code);
                    self.code.pick_project();
                }
                Act::PageDebug | Act::PageCode | Act::PageGit | Act::PageTools | Act::PageCompare => {
                    self.nav_manual();
                    match a {
                        Act::PageDebug => self.set_page(Page::Debug),
                        Act::PageCode => self.set_page(Page::Code),
                        Act::PageGit => self.set_page(Page::Git),
                        Act::PageTools => self.set_page(Page::Tools),
                        _ => self.set_page(Page::Compare),
                    }
                }
                Act::NavBack => self.nav_go(true),
                Act::NavFwd => self.nav_go(false),
                Act::ThemeToggle => {
                    let light = !is_light();
                    self.set_theme(ctx, if light { "light" } else { "dark" });
                }
                Act::CheckUpdate => self.check_update(true),
                Act::BitCalc => self.bitcalc.toggle(),
                Act::ToggleBg => {
                    if crate::bgimg::any(&self.s.bg) {
                        self.s.bg.on = !self.s.bg.on;
                    } else {
                        self.open_settings(prefs::Cat::Appearance);
                    }
                }
                Act::Checksum => self.calc.open = !self.calc.open,
                Act::NewWindow => self.new_window(),
                Act::ToggleShell => {
                    let cwd = self.shell_cwd();
                    self.shells.toggle(ctx, &cwd);
                }
                Act::NewShell => {
                    let cwd = self.shell_cwd();
                    self.shells.new_shell(ctx, None, &cwd);
                }
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

    /// 代码页换了工程：Git 页、HTTP / TFTP 共享目录跟着换（终端新开时也在工程目录）
    fn project_changed(&mut self, p: &std::path::Path) {
        self.gitv.follow_project(p);
        let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
        let ps = p.to_string_lossy().to_string();
        let prev = std::mem::replace(&mut self.last_project, ps.clone());
        for r in [&mut self.s.tb.tftp_root, &mut self.s.tb.http_root] {
            if r.is_empty() || *r == home || (!prev.is_empty() && *r == prev) {
                *r = ps.clone();
            }
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
        if self.s.api_mode {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 16,
                    right: 16,
                    top: 14,
                    bottom: 10,
                }))
                .show(ctx, |ui| self.api_page(ui));
            return;
        }
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
            .show_animated(ctx, self.s.cmd_panel, |ui| self.dock_ui(ui))
            .and_then(|r| r.inner);
        let acted = act.is_some();
        match act {
            Some(CmdAction::Send(c)) => self.send_quick(&c),
            Some(CmdAction::SendAll(gi)) => self.send_group(gi),
            None => {}
        }
        if acted && self.s.terminal {
            self.tv.focus_req = true; // 点完快捷命令，键盘回到终端
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
            .show(ctx, |ui| {
                if self.s.terminal {
                    self.term_view(ui);
                    return;
                }
                if self.s.plot.on {
                    let rest = self.plot_split(ui);
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rest));
                    self.log_view(&mut child);
                } else {
                    self.log_view(ui);
                }
            });
    }

    /// 日志上方的波形图，和日志之间可以拖动分隔线调整高度
    fn plot_split(&mut self, ui: &mut egui::Ui) -> Rect {
        let full = ui.available_rect_before_wrap();
        let h = self.s.plot.height.clamp(90.0, (full.height() - 140.0).max(90.0));
        let r = Rect::from_min_size(full.min + vec2(0.0, 2.0), vec2(full.width(), h));
        self.plot_pane(ui, r);
        let split = Rect::from_min_size(pos2(full.left(), r.bottom()), vec2(full.width(), 8.0));
        let sr = ui.interact(split, egui::Id::new("plot_split"), Sense::drag());
        if sr.hovered() || sr.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().hline(split.x_range().shrink(full.width() * 0.4), split.center().y, st(2.0, ACCENT().gamma_multiply(0.6)));
        }
        if sr.dragged() {
            self.s.plot.height = h + sr.drag_delta().y;
        }
        Rect::from_min_max(pos2(full.left(), split.bottom()), full.max)
    }
}

impl App {
    fn set_page(&mut self, p: Page) {
        if self.page == p {
            return;
        }
        if p == Page::Settings || p == Page::Community {
            self.prev_page = self.page;
        }
        self.page = p;
        crate::hub::count(match p {
            Page::Debug => "page.debug",
            Page::Code => "page.code",
            Page::Git => "page.git",
            Page::Compare => "page.compare",
            Page::Tools => "page.tools",
            Page::Settings => "page.settings",
            Page::Community => "page.community",
        });
        match p {
            Page::Code => self.code.activate(),
            Page::Git => {
                let root = self.code.root().map(|r| r.to_path_buf());
                self.gitv.activate(root.as_deref());
            }
            Page::Debug => self.tv.focus_req = self.s.terminal,
            Page::Tools | Page::Settings | Page::Compare | Page::Community => {}
        }
    }

    /// 系统右键菜单 / 命令行的请求；forwarded = 从别的进程转来的，要把窗口提到前面
    fn handle_req(&mut self, r: crate::shellint::Req, forwarded: bool) {
        use crate::shellint::Req;
        match r {
            Req::Open(p) => {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext == "log" {
                    self.set_page(Page::Debug);
                    self.load_log_path(&p, false);
                } else {
                    self.set_page(Page::Code);
                    self.code.open_file(&p, None);
                }
            }
            Req::Project(p) => {
                self.code.open_root(std::fs::canonicalize(&p).unwrap_or(p));
                self.set_page(Page::Code);
            }
            Req::Compare(p) => {
                let first = self.cmpv.pending_left().is_none();
                self.cmpv.drop_files(vec![p.clone()]);
                if first {
                    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    self.toast(tf("已选为左侧：{}，再选一个「用 QCCOM 比较」", &[&name]), ACCENT());
                }
                self.set_page(Page::Compare);
            }
            Req::Pair(a, b) => {
                self.cmpv.open(&a, &b, None);
                self.set_page(Page::Compare);
            }
        }
        self.compare_events();
        if forwarded {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// 打开比较页比较两边；带名字的一边是只读的临时文件
    fn compare_sides(&mut self, a: PathBuf, la: Option<String>, b: PathBuf, lb: Option<String>) {
        use crate::compareview::Side;
        if la.is_none() && lb.is_none() {
            self.cmpv.open(&a, &b, None);
        } else {
            let mode = crate::compare::guess_mode(&a, &b);
            let side = |p: &PathBuf, l: Option<String>| match l {
                Some(l) => Side::labeled(p, &l),
                None => Side::file(p),
            };
            self.cmpv.open_sides(side(&a, la), side(&b, lb), mode);
        }
        self.set_page(Page::Compare);
        self.compare_events();
    }

    fn compare_events(&mut self) {
        for e in std::mem::take(&mut self.cmpv.events) {
            match e {
                crate::compareview::CmpEvt::Toast(m, c) => self.toast(m, c),
                crate::compareview::CmpEvt::OpenFile(p, line) => {
                    self.set_page(Page::Code);
                    self.code.open_file(&p, Some((line, None)));
                }
            }
        }
    }

    /// 最左侧的活动栏（类似 VS Code）：调试 / 代码 / Git，底部是语言和更新
    /// 新开 shell 的工作目录：代码页打开的工程，否则用户主目录
    fn shell_cwd(&self) -> PathBuf {
        self.code
            .root()
            .map(|p| p.to_path_buf())
            .or_else(|| Some(PathBuf::from(&self.code.s.root)).filter(|p| !p.as_os_str().is_empty() && p.is_dir()))
            .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    }

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
                    (Page::Compare, Icon::Compare, t("比较与合并：文件夹、文本、十六进制、三方合并")),
                    (Page::Tools, Icon::Toolbox, t("工具箱：文件服务、设备发现、数据转换、CAN、HID")),
                ] {
                    let dot = (pg == Page::Debug && live).then_some(OK());
                    let tools_tip;
                    let tip = if pg == Page::Tools {
                        tools_tip = self.tools_tip();
                        tools_tip.as_str()
                    } else {
                        tip
                    };
                    if activity_button(ui, icon, tip, self.page == pg, dot).clicked() {
                        self.nav_manual();
                        self.set_page(pg);
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    // 齿轮：设置、命令面板、主题、语言等不常用入口都收在这里
                    let r = activity_button(ui, Icon::Gear, t("管理"), self.page == Page::Settings, None);
                    let tip = format!("{}  {}", t("终端"), self.km.label(Act::ToggleShell));
                    if activity_button(ui, Icon::Terminal, &tip, self.shells.s.open, None).clicked() {
                        self.pending_acts.push(Act::ToggleShell);
                    }
                    let tip = self.community_tip();
                    let dot = self.comm.has_news().then_some(ACCENT());
                    if activity_button(ui, Icon::Chat, &tip, self.page == Page::Community, dot).clicked() {
                        self.toggle_community();
                    }
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
                            if menu_item(ui, &keymap::act_label(a), &keys) {
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
                        if menu_item(ui, t("社区与反馈"), "") {
                            self.set_page(Page::Community);
                            ui.memory_mut(|m| m.close_popup());
                        }
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
                        Err(e) if manual => {
                            // 顺手放进剪贴板，方便发给服务器管理员或贴到问题反馈里
                            ctx.copy_text(format!("QCCOM v{} {}: {e}", env!("CARGO_PKG_VERSION"), update::version_url()));
                            self.toast(tf("检查更新失败（错误信息已复制）：{}", &[&e]), ERR())
                        }
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

impl App {
    /// 活动栏社区图标的提示：有新回复 / 新公告时直接说是什么
    fn community_tip(&self) -> String {
        match (&self.comm.notice, self.comm.unread) {
            (_, n) if n > 0 => tf("社区与反馈：{} 条新提醒", &[&n]),
            (Some(nt), _) if nt.id > self.comm.seen_notice => tf("社区与反馈 · 新公告：{}", &[&nt.title]),
            _ => t("社区与反馈：提需求、投票、求助").to_string(),
        }
    }

    /// 点社区图标：已经在社区就回到原来的页面；有新提醒 / 新公告时直接打开它
    fn toggle_community(&mut self) {
        if self.page == Page::Community {
            self.set_page(self.prev_page_or_debug());
            return;
        }
        if self.comm.unread > 0 {
            self.comm.go(crate::community::Nav::Notifs);
        } else if let Some(nt) = self.comm.notice.clone().filter(|n| n.id > self.comm.seen_notice) {
            self.comm.seen_notice = nt.id;
            self.comm.go(crate::community::Nav::Board(crate::community::Board::Notice));
            self.comm.open_post(nt.id);
        }
        self.set_page(Page::Community);
    }

    fn prev_page_or_debug(&self) -> Page {
        if self.prev_page == Page::Community || self.prev_page == Page::Settings {
            Page::Debug
        } else {
            self.prev_page
        }
    }
}

impl eframe::App for App {
    /// 面板之间没盖住的缝隙露出的是清屏色，要跟着主题走，否则浅色下会露出黑边
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG1().to_normalized_gamma_f32()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.code.snapshot();
        self.s.code = self.code.s.clone();
        self.s.git = self.gitv.s.clone();
        self.s.cmp = self.cmpv.s.clone();
        self.s.shell = self.shells.s.clone();
        eframe::set_value(storage, eframe::APP_KEY, &self.s);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::hub::final_beat();
        if let Some(mut r) = self.record.take() {
            let _ = r.w.flush();
        }
        self.disconnect();
        self.close_all_sessions();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        crate::titlebar::apply(frame, !crate::theme::is_light(), BG0(), TEXT(), LINE());
        crate::bgimg::set(&self.s.bg);
        if !self.s.window_placed {
            self.place_window(ctx);
        }
        if self.maximize_in > 0 {
            self.maximize_in -= 1;
            if self.maximize_in == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        self.nav_tick(ctx);
        // 调试页显示串口；工具箱里选了网络工具就显示那个连接。其余页面保持不变
        let want = match self.page {
            Page::Debug => Some(0),
            Page::Tools => toolbox::comm_slot(self.s.tb.tool),
            _ => None,
        };
        if let Some(k) = want {
            self.use_slot(k);
        }
        if matches!(self.page, Page::Debug | Page::Tools) {
            self.s.api_mode = self.page == Page::Tools && self.s.tb.tool == toolbox::Tool::Api;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.cmpv.has_unsaved() && !self.close_anyway {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_anyway = true;
            self.set_page(Page::Compare);
            self.toast(t("比较页有未保存的修改；再关一次会直接退出"), WARN());
        }
        let reqs: Vec<crate::shellint::Req> = self.fwd.as_ref().map(|q| std::mem::take(&mut *q.lock().unwrap())).unwrap_or_default();
        for r in reqs {
            self.handle_req(r, true);
        }
        self.tick_background();
        self.handle_events();
        self.tick();

        self.poll_update(ctx);
        self.follow_system_theme(ctx);
        let scope = match self.page {
            Page::Debug | Page::Tools | Page::Settings | Page::Community => Scope::Debug,
            Page::Code => Scope::Code,
            Page::Git => Scope::Git,
            Page::Compare => Scope::Global,
        };
        let term_focused = (self.page == Page::Debug && self.s.terminal && ctx.memory(|m| m.has_focus(self.tv.id()))) || self.shells.focused(ctx);
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
        self.shells.poll(ctx.input(|i| i.focused));
        self.ssh_poll();
        let cwd = self.shell_cwd();
        self.shells.ui(ctx, &cwd);
        for e in std::mem::take(&mut self.shells.events) {
            match e {
                ShellEvt::Toast(m) => self.toast(m, WARN()),
                ShellEvt::Done(title, d) => {
                    self.toast(tf("「{}」的命令跑完了（用时 {} 秒）", &[&title, &d.as_secs()]), OK());
                    if !ctx.input(|i| i.focused) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
                    }
                }
                ShellEvt::Open(p, line) => {
                    self.set_page(Page::Code);
                    self.code.open_file(&p, Some((line, None)));
                }
                ShellEvt::Task(name, cmd) => self.flash_checkpoint(&name, &cmd),
            }
        }
        // 换了工程：不管在哪一页都要同步（命令行打开文件夹时代码页还没显示）
        let mut projs = vec![];
        self.code.events.retain(|e| match e {
            CodeEvt::Project(p) => {
                projs.push(p.clone());
                false
            }
            _ => true,
        });
        for p in projs {
            self.project_changed(&p);
        }
        // 设置里记着 SVD 路径时，启动后代码页的寄存器悬停不用先打开工具也能用（路径没变就只是比较一下字符串）
        self.svd_sync();
        self.code.reg_window(ctx);
        // 电路仿真在后台照常运行（切到调试页看仿真串口时也要跑）
        if let Some(cv) = &mut self.circ {
            cv.tick(ctx);
        }
        // 社区：收后台结果、定时拉提醒；提示语走统一的 toast
        self.comm.tick(ctx, self.page == Page::Community);
        for (m, err) in std::mem::take(&mut self.comm.toasts) {
            self.toast(m, if err { ERR() } else { OK() });
        }
        self.s.seen_notice = self.comm.seen_notice;
        match self.page {
            Page::Code => {
                self.code.ui(ctx, &acts, &self.km);
                for e in std::mem::take(&mut self.code.events) {
                    match e {
                        CodeEvt::Toast(m, c) => self.toast(m, c),
                        CodeEvt::Project(p) => self.project_changed(&p),
                        CodeEvt::OpenSettings => {
                            self.prefs.cat = prefs::Cat::Editor;
                            self.set_page(Page::Settings);
                        }
                        CodeEvt::BitCalc(lit, v) => self.bitcalc.show(&lit, v),
                        CodeEvt::Struct(src) => self.open_struct(src),
                        CodeEvt::Compare(a, la, b, lb) => self.compare_sides(a, la, b, lb),
                        CodeEvt::Ask(text, lang) => {
                            self.comm.ask_with(&text, &lang);
                            self.set_page(Page::Community);
                        }
                        CodeEvt::Merge(base, mine, out) => {
                            use crate::compareview::Side;
                            let disk = crate::compareview::temp_file("disk", &std::fs::read(&out).unwrap_or_default());
                            self.cmpv.open_merge(
                                Side::labeled(&base, &t("上次保存")),
                                Side::labeled(&mine, &t("我的修改")),
                                Side::labeled(&disk, &t("外部修改")),
                                &out,
                                None,
                            );
                            self.set_page(Page::Compare);
                            self.compare_events();
                        }
                    }
                }
            }
            Page::Git => {
                self.gitv.unsaved = self.code.dirty_paths();
                self.gitv.sensitive.clone_from(&self.code.s.sensitive);
                self.gitv.ui(ctx, &acts);
                for e in std::mem::take(&mut self.gitv.events) {
                    match e {
                        GitEvt::Toast(m, c) => self.toast(m, c),
                        GitEvt::Project(p) => {
                            self.code.open_root(p);
                        }
                        GitEvt::OpenFile(p, line) => {
                            self.set_page(Page::Code);
                            self.code.open_file(&p, Some((line, None)));
                        }
                        GitEvt::Compare(a, la, b, lb) => self.compare_sides(a, la, b, lb),
                        GitEvt::Conflict(root, rel) => {
                            self.cmpv.open_conflict(&root, &rel);
                            self.set_page(Page::Compare);
                            self.compare_events();
                        }
                    }
                }
            }
            Page::Debug => self.debug_page(ctx),
            Page::Tools => self.toolbox_page(ctx),
            Page::Compare => {
                self.cmpv.ui(ctx);
                self.compare_events();
            }
            Page::Settings => self.settings_page(ctx),
            Page::Community => self.comm.ui(ctx),
        }
        if let Some((p, replay)) = self.open_log.take() {
            self.load_log_path(&p, replay);
        }
        // 拖进窗口的文件：调试页当日志打开，代码页在编辑器里打开
        let mut dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        // 测试用：环境变量 QCCOM_DROP 指向一个文件，里面每行一个路径，当作拖进来的文件（xdotool 模拟不了拖放）
        if let Some(f) = std::env::var_os("QCCOM_DROP") {
            if let Ok(s) = std::fs::read_to_string(&f) {
                let _ = std::fs::remove_file(&f);
                dropped.extend(s.lines().filter(|l| !l.trim().is_empty()).map(|l| PathBuf::from(l.trim())));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        // 拖放位置：Windows 拖动过程中收不到鼠标移动，直接问系统光标位置
        let drop_pos = drop_pointer(ctx);
        // 拖到底部终端面板上：把路径插进命令行
        if !dropped.is_empty() && self.shells.s.open {
            if drop_pos.is_some_and(|h| self.shells.rect.is_some_and(|r| r.contains(h))) {
                self.shells.insert_paths(&std::mem::take(&mut dropped));
            }
        }
        // 拖到代码页的资源管理器上：复制进指针下面的文件夹（按住 Shift 移动）
        if !dropped.is_empty() && self.page == Page::Code {
            if let Some(dir) = drop_pos.and_then(|p| self.code.drop_dir_at(p)) {
                let mv = ctx.input(|i| i.modifiers.shift);
                let v = std::mem::take(&mut dropped);
                // 拖进一个文件夹（而工程还没打开）当工程打开
                self.code.drop_into(&v, &dir, mv);
            }
        }
        if !dropped.is_empty() && self.page == Page::Code && self.code.root().is_none() && dropped.len() == 1 && dropped[0].is_dir() {
            let d = dropped.remove(0);
            self.code.open_root(d);
        }
        for p in dropped {
            match self.page {
                Page::Code => {
                    if p.is_dir() {
                        self.code.open_root(p.clone());
                    } else if !self.code.drop_on_viewer(&p) {
                        self.code.open_file(&p, None)
                    }
                }
                Page::Compare => self.cmpv.drop_files(vec![p]),
                Page::Tools => self.tool_drop(p),
                _ => self.load_log_path(&p, false),
            }
        }
        self.drop_hint(ctx, drop_pos);
        let code = &self.code;
        self.bitcalc.ui(ctx, &|s| code.eval_expr(s));
        self.log_windows(ctx);
        self.mini_windows(ctx);
        self.tool_windows(ctx);
        self.alias_window(ctx);
        self.api_windows(ctx);
        self.palette_ui(ctx);
        self.back_chip(ctx);
        self.switcher_ui(ctx);

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

pub(crate) fn brand(ui: &mut egui::Ui, logo: &egui::TextureHandle, extra: impl FnOnce(&mut egui::Ui)) {
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
            ui.add_space(4.0);
            extra(ui);
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

fn port_card(ui: &mut egui::Ui, p: &PortEntry, alias: Option<&str>, side_b: bool, selected: bool, live: bool, enabled: bool) -> egui::Response {
    let w = ui.available_width();
    // 断开时才能点选，但右键菜单（重命名、监听口）始终可用
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 46.0), if enabled { Sense::click() } else { Sense::hover() | Sense::click() });
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
    let short = p.name.rsplit(['/', '\\']).next().unwrap_or(&p.name);
    painter.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        alias.unwrap_or(short),
        FontId::new(13.5, mono_bold()),
        if selected { TEXT() } else { TEXT2() },
    );
    let desc = match alias {
        Some(_) if p.desc.is_empty() => short.to_string(),
        Some(_) => format!("{short} · {}", p.desc),
        None if p.desc.is_empty() => p.name.clone(),
        None => p.desc.clone(),
    };
    if side_b {
        let r = Rect::from_center_size(rect.right_center() - vec2(if live { 50.0 } else { 18.0 }, 0.0), vec2(16.0, 16.0));
        painter.rect(r, 4.0, WARN().gamma_multiply(0.15), st(1.0, WARN().gamma_multiply(0.5)), egui::StrokeKind::Inside);
        painter.text(r.center(), Align2::CENTER_CENTER, "B", FontId::new(10.0, mono_bold()), WARN());
    }
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

fn stat(ui: &mut egui::Ui, w: f32, k: &str, v: &str, c: Color32) {
    ui.vertical(|ui| {
        ui.set_width(w);
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

fn empty_state(ui: &egui::Ui, rect: Rect, no_ports: bool, connected: bool, net: bool, logo: &egui::TextureHandle) {
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
    } else if net {
        (t("准备就绪"), t("左侧填好地址，点「连接」"))
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
    Relations,
    Context,
    Bookmark,
    Terminal,
    Replace,
    Split,
    Maximize,
    Restore,
    Todo,
    Chart,
    Filter,
    More,
    Toolbox,
    Compare,
    Swap,
    History,
    Chat,
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
        Icon::Compare => {
            // 比较：左右两页，中间一对箭头
            p.rect_stroke(Rect::from_min_max(pt(-7.0, -6.0), pt(-1.5, 6.0)), 1.2 * u, s, egui::StrokeKind::Middle);
            p.rect_stroke(Rect::from_min_max(pt(1.5, -6.0), pt(7.0, 6.0)), 1.2 * u, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-5.0, -2.5), pt(-3.5, -2.5)], s);
            p.line_segment([pt(-5.0, 0.5), pt(-3.5, 0.5)], s);
            p.line_segment([pt(3.5, -2.5), pt(5.0, -2.5)], s);
            p.line_segment([pt(3.5, 3.0), pt(5.0, 3.0)], s);
        }
        Icon::Swap => {
            p.line_segment([pt(-6.0, -3.0), pt(6.0, -3.0)], s);
            p.add(Shape::line(vec![pt(3.0, -6.0), pt(6.0, -3.0), pt(3.0, 0.0)], s));
            p.line_segment([pt(-6.0, 3.5), pt(6.0, 3.5)], s);
            p.add(Shape::line(vec![pt(-3.0, 0.5), pt(-6.0, 3.5), pt(-3.0, 6.5)], s));
        }
        Icon::Chat => crate::community::chat_icon(p, Rect::from_center_size(c0, vec2(15.0 * u, 15.0 * u)), c),
        Icon::History => {
            // 钟面 + 逆时针箭头
            let mut pts = vec![];
            for k in 0..=20 {
                let a = (-200.0f32 + k as f32 * 13.0).to_radians();
                pts.push(pt(6.0 * a.cos(), 6.0 * a.sin()));
            }
            p.add(Shape::line(pts, s));
            p.add(Shape::line(vec![pt(-7.6, -4.6), pt(-5.6, -2.0), pt(-3.0, -3.8)], s));
            p.add(Shape::line(vec![pt(0.0, -3.5), pt(0.0, 0.0), pt(2.6, 1.8)], s));
        }
        Icon::Toolbox => {
            // 工具箱：箱体 + 提手 + 中间的扣
            p.rect_stroke(Rect::from_min_max(pt(-7.0, -2.5), pt(7.0, 6.0)), 1.5 * u, s, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![pt(-3.0, -2.5), pt(-3.0, -5.5), pt(3.0, -5.5), pt(3.0, -2.5)], s));
            p.line_segment([pt(-7.0, 1.5), pt(7.0, 1.5)], s);
            p.rect_filled(Rect::from_center_size(pt(0.0, 1.5), vec2(3.0 * u, 2.4 * u)), 0.6 * u, c);
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
        Icon::Chart => {
            p.add(Shape::line(vec![pt(-6.5, -6.5), pt(-6.5, 6.5), pt(6.5, 6.5)], s));
            p.add(Shape::line(vec![pt(-4.5, 2.5), pt(-1.5, -1.5), pt(1.5, 1.5), pt(5.5, -4.5)], s));
        }
        Icon::Filter => {
            p.add(Shape::closed_line(vec![pt(-6.5, -5.5), pt(6.5, -5.5), pt(1.5, 0.5), pt(1.5, 5.5), pt(-1.5, 6.5), pt(-1.5, 0.5)], s));
        }
        Icon::More => {
            for x in [-5.0, 0.0, 5.0] {
                p.circle_filled(pt(x, 0.0), 1.3 * u, c);
            }
        }
        Icon::Todo => {
            p.rect_stroke(Rect::from_min_max(pt(-6.5, -6.0), pt(-1.5, -1.0)), 1.0, s, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![pt(-5.6, -3.6), pt(-4.2, -2.3), pt(-2.0, -5.0)], s));
            p.line_segment([pt(1.0, -3.5), pt(7.0, -3.5)], s);
            p.rect_stroke(Rect::from_min_max(pt(-6.5, 1.5), pt(-1.5, 6.5)), 1.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(1.0, 4.0), pt(7.0, 4.0)], s);
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
        Icon::Relations => {
            // 一个节点分出两个子节点：调用树
            p.rect_stroke(Rect::from_center_size(pt(-4.0, -4.5), vec2(5.0 * u, 4.0 * u)), 1.0, s, egui::StrokeKind::Middle);
            p.rect_stroke(Rect::from_center_size(pt(3.5, 0.5), vec2(5.0 * u, 4.0 * u)), 1.0, s, egui::StrokeKind::Middle);
            p.rect_stroke(Rect::from_center_size(pt(3.5, 5.5), vec2(5.0 * u, 4.0 * u)), 1.0, s, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![pt(-4.0, -2.5), pt(-4.0, 5.5), pt(1.0, 5.5)], s));
            p.line_segment([pt(-4.0, 0.5), pt(1.0, 0.5)], s);
        }
        Icon::Context => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(14.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(-7.0, 1.5), pt(7.0, 1.5)], s);
            p.line_segment([pt(-4.5, 4.0), pt(2.0, 4.0)], s);
        }
        Icon::Bookmark => {
            p.add(Shape::closed_line(vec![pt(-4.5, -6.5), pt(4.5, -6.5), pt(4.5, 6.5), pt(0.0, 3.0), pt(-4.5, 6.5)], s));
        }
        Icon::Terminal => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(15.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![pt(-4.5, -2.5), pt(-1.5, 0.0), pt(-4.5, 2.5)], s));
            p.line_segment([pt(0.5, 3.0), pt(4.5, 3.0)], s);
        }
        Icon::Replace => {
            p.add(Shape::line(vec![pt(-6.0, -1.0), pt(-6.0, -5.0), pt(3.0, -5.0)], s));
            p.add(Shape::line(vec![pt(1.0, -7.0), pt(3.0, -5.0), pt(1.0, -3.0)], s));
            p.add(Shape::line(vec![pt(6.0, 1.0), pt(6.0, 5.0), pt(-3.0, 5.0)], s));
            p.add(Shape::line(vec![pt(-1.0, 3.0), pt(-3.0, 5.0), pt(-1.0, 7.0)], s));
        }
        Icon::Split => {
            p.rect_stroke(Rect::from_center_size(c0, vec2(14.0 * u, 12.0 * u)), 2.0, s, egui::StrokeKind::Middle);
            p.line_segment([pt(0.0, -6.0), pt(0.0, 6.0)], s);
        }
        Icon::Maximize => {
            p.add(Shape::line(vec![pt(-5.0, 2.0), pt(0.0, -3.0), pt(5.0, 2.0)], s));
        }
        Icon::Restore => {
            p.add(Shape::line(vec![pt(-5.0, -2.0), pt(0.0, 3.0), pt(5.0, -2.0)], s));
        }
    }
}

/// 无边框图标按钮，悬停时出底色；active 时青色（录制时红色）
/// 右键菜单的一项：左边文字，右边淡色快捷键
/// 拖放时的指针位置（逻辑坐标）。Windows 在系统拖放过程中不发鼠标移动，egui 的位置是旧的，改问系统
fn drop_pointer(ctx: &egui::Context) -> Option<egui::Pos2> {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Point {
            x: i32,
            y: i32,
        }
        #[link(name = "user32")]
        extern "system" {
            fn GetCursorPos(p: *mut Point) -> i32;
        }
        let mut p = Point { x: 0, y: 0 };
        // SAFETY: 只写入本地结构体
        if unsafe { GetCursorPos(&mut p) } != 0 {
            let (inner, ppp) = ctx.input(|i| (i.viewport().inner_rect, i.pixels_per_point));
            if let Some(r) = inner {
                return Some(pos2(p.x as f32 / ppp, p.y as f32 / ppp) - r.min.to_vec2());
            }
        }
    }
    ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos()))
}

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
            Act::LogMarker,
            Act::LogNextMark,
            Act::TogglePlot,
        ]
        .iter()
        .map(|a| (km.label(*a), keymap::act_label(*a)))
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
            FontId::new(if seg < 44.0 { 11.5 } else { 12.5 }, if on { bold() } else { FontFamily::Proportional }),
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
        Dir::Rx2 => "RB",
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

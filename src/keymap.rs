//! 快捷键：所有可绑定的命令集中登记在这里，默认键位尽量和 VS Code 一致，用户可以改。
//!
//! 每帧开始时 `Keymap::poll` 把按键事件翻译成命令（并从输入里拿掉，免得再被别的控件处理），
//! 各页面只处理命令，不直接看按键。支持 VS Code 式的组合键序列（Ctrl+K Ctrl+S）。

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, Modifiers};

use crate::i18n::t;

/// 命令生效的范围
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Global,
    Debug,
    Code,
    Git,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => t("全局"),
            Scope::Debug => t("调试"),
            Scope::Code => t("代码"),
            Scope::Git => "Git",
        }
    }
}

/// 一个按键组合，如 Ctrl+Shift+P
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: Key,
}

impl Combo {
    pub fn from_event(key: Key, m: Modifiers) -> Combo {
        Combo {
            ctrl: m.command || m.ctrl,
            shift: m.shift,
            alt: m.alt,
            key,
        }
    }

    pub fn parse(s: &str) -> Option<Combo> {
        let mut c = Combo {
            ctrl: false,
            shift: false,
            alt: false,
            key: Key::Escape,
        };
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "cmd" | "control" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "option" => c.alt = true,
                "" => key = Some(Key::Plus), // "Ctrl++"
                _ => key = key_from_name(part),
            }
        }
        c.key = key?;
        Some(c)
    }

    /// 按修饰键数量判断：没有 Ctrl / Alt 的普通键（字母、数字）不能单独绑定，免得打字时触发
    pub fn usable(&self) -> bool {
        self.ctrl || self.alt || is_fkey(self.key) || matches!(self.key, Key::Escape)
    }
}

impl std::fmt::Display for Combo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ctrl {
            f.write_str(if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" })?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        f.write_str(key_name(self.key))
    }
}

fn is_fkey(k: Key) -> bool {
    matches!(
        k,
        Key::F1 | Key::F2 | Key::F3 | Key::F4 | Key::F5 | Key::F6 | Key::F7 | Key::F8 | Key::F9 | Key::F10 | Key::F11 | Key::F12
    )
}

fn key_name(k: Key) -> &'static str {
    match k {
        Key::Backtick => "`",
        Key::Plus => "+",
        Key::Equals => "=",
        Key::Minus => "-",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Backslash => "\\",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        _ => k.name(),
    }
}

fn key_from_name(s: &str) -> Option<Key> {
    let k = match s {
        "`" => Key::Backtick,
        "=" => Key::Equals,
        "-" => Key::Minus,
        "," => Key::Comma,
        "." => Key::Period,
        "/" => Key::Slash,
        "\\" => Key::Backslash,
        "[" => Key::OpenBracket,
        "]" => Key::CloseBracket,
        ";" => Key::Semicolon,
        "'" => Key::Quote,
        "←" | "Left" => Key::ArrowLeft,
        "→" | "Right" => Key::ArrowRight,
        "↑" | "Up" => Key::ArrowUp,
        "↓" | "Down" => Key::ArrowDown,
        "0" => Key::Num0,
        "1" => Key::Num1,
        "2" => Key::Num2,
        "3" => Key::Num3,
        "4" => Key::Num4,
        "5" => Key::Num5,
        "6" => Key::Num6,
        "7" => Key::Num7,
        "8" => Key::Num8,
        "9" => Key::Num9,
        _ => return Key::from_name(s).or_else(|| Key::from_name(&s.to_ascii_uppercase())),
    };
    Some(k)
}

/// 一个绑定：一个或两个组合键（序列）
pub type Chord = Vec<Combo>;

pub fn chord_str(c: &Chord) -> String {
    c.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")
}

pub fn parse_chord(s: &str) -> Option<Chord> {
    let v: Option<Vec<Combo>> = s.split_whitespace().map(Combo::parse).collect();
    v.filter(|v| !v.is_empty() && v.len() <= 2)
}

/// 命令。新增命令只需要在 `ACTIONS` 里加一行
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Act {
    Palette,
    Settings,
    Keymap,
    PageDebug,
    PageCode,
    PageGit,
    PageTools,
    PageCompare,
    ThemeToggle,
    CheckUpdate,
    ToggleShell,
    NewShell,
    // 调试页
    Connect,
    LogFind,
    LogGotoTime,
    LogClear,
    LogSave,
    LogPause,
    LogSelectAll,
    FocusCmdBar,
    ToggleSidebar,
    ToggleCmdPanel,
    ToggleTerminal,
    LogMarker,
    LogNextMark,
    LogPrevMark,
    LogBookmark,
    LogRules,
    LogStats,
    LogOpen,
    TogglePlot,
    Modbus,
    SendFile,
    Checksum,
    NewWindow,
    Quick(u8),
    // 代码页
    QuickOpen,
    GotoSymbolWs,
    GotoSymbol,
    GotoLine,
    OpenFile,
    SearchAll,
    FindInFile,
    ReplaceInFile,
    SplitEditor,
    ToggleBlame,
    BitCalc,
    ToggleBg,
    InsertSnippet,
    ToggleVim,
    CloseTab,
    NextTab,
    SaveFile,
    ZoomIn,
    ZoomOut,
    GotoDef,
    FindRefs,
    NavBack,
    NavFwd,
    // Git 页
    GitRefresh,
    GitFetch,
    GitFind,
    // 编辑器
    ToggleComment,
    MoveLineUp,
    MoveLineDown,
    CopyLineUp,
    CopyLineDown,
    DeleteLine,
    InsertLineBelow,
    InsertLineAbove,
    SelectLine,
    AddNextOcc,
    SelectAllOcc,
    CursorAbove,
    CursorBelow,
    GotoBracket,
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ToggleContext,
    ToggleRelations,
    // 导航与代码生成（v1.3）
    NavHistory,
    LastEdit,
    SwitchHeader,
    MatchIf,
    PrevFunc,
    NextFunc,
    GotoDecl,
    DefToSide,
    HlToggle,
    HlRemove,
    HlClear,
    HlNext,
    HlPrev,
    BlockComment,
    IfZero,
    AlignSel,
    CaseConvert,
    EnumSwitch,
    DeclToHeader,
    RemoveDebug,
    RecToggle,
    RecPlay,
    RecSave,
    OpenProject,
    // 本地历史（v1.8）
    Checkpoint,
    ReviewChanges,
    Timeline,
    FileHistory,
    // 编辑器（v1.8）
    ToggleWrap,
    ToggleWhitespace,
    TrimWhitespace,
    FormatDoc,
    ProjectUtf8,
    RegPanel,
    /// 代码生成动作（设置里可以编辑），下标对应动作列表
    Macro(u8),
}

pub struct ActDef {
    pub act: Act,
    pub id: &'static str,
    /// 代码里的中文原文，显示时经 t() 翻译
    pub label: &'static str,
    pub scope: Scope,
    pub keys: &'static [&'static str],
    /// 终端获得键盘焦点时也生效（其余快捷键都直接发给设备）
    pub in_term: bool,
}

macro_rules! acts {
    ($($act:expr, $id:literal, $label:literal, $scope:ident, [$($k:literal),*] $(, $term:literal)?;)*) => {
        pub const ACTIONS: &[ActDef] = &[
            $(ActDef { act: $act, id: $id, label: $label, scope: Scope::$scope, keys: &[$($k),*], in_term: false $(|| $term)? },)*
        ];
    };
}

acts! {
    Act::Palette, "palette", "命令面板", Global, ["Ctrl+Shift+P", "F1"], true;
    Act::Settings, "settings", "打开设置", Global, ["Ctrl+,"], true;
    Act::Keymap, "keymap", "快捷键设置", Global, ["Ctrl+K Ctrl+S"];
    Act::PageDebug, "page.debug", "切换到调试页", Global, ["Ctrl+Shift+D"], true;
    Act::PageCode, "page.code", "切换到代码页", Global, ["Ctrl+Shift+E"], true;
    Act::PageGit, "page.git", "切换到 Git 页", Global, ["Ctrl+Shift+G"], true;
    Act::PageTools, "page.tools", "切换到工具箱", Global, ["Ctrl+Shift+B"], true;
    Act::PageCompare, "page.compare", "切换到比较页", Global, ["Ctrl+Shift+M"], true;
    Act::ThemeToggle, "theme.toggle", "切换深色 / 浅色主题", Global, ["Ctrl+K Ctrl+T"];
    Act::CheckUpdate, "update.check", "检查更新", Global, [];
    Act::ToggleShell, "shell.toggle", "显示 / 隐藏终端面板", Global, ["Ctrl+Shift+`", "Alt+F12"], true;
    Act::NewShell, "shell.new", "新建终端", Global, ["Ctrl+Shift+5"], true;
    Act::Connect, "conn.toggle", "连接 / 断开", Debug, ["F5"], true;
    Act::LogFind, "log.find", "搜索日志", Debug, ["Ctrl+F"];
    Act::LogGotoTime, "log.goto_time", "跳到时间", Debug, ["Ctrl+G"];
    Act::LogClear, "log.clear", "清屏", Debug, ["Ctrl+L"];
    Act::LogSave, "log.save", "保存日志", Debug, ["Ctrl+S"];
    Act::LogPause, "log.pause", "暂停 / 继续", Debug, ["Ctrl+P"];
    Act::LogSelectAll, "log.select_all", "全选日志", Debug, ["Ctrl+A"];
    Act::FocusCmdBar, "cmdbar.focus", "聚焦命令栏", Debug, ["Ctrl+K"];
    Act::ToggleSidebar, "sidebar.toggle", "显示 / 隐藏侧边栏", Global, ["Ctrl+B"];
    Act::ToggleCmdPanel, "cmds.toggle", "展开 / 收起快捷命令", Debug, ["Ctrl+J"];
    Act::ToggleTerminal, "term.toggle", "切换 日志 / 终端", Debug, ["Ctrl+`"], true;
    Act::LogMarker, "log.marker", "插入分隔标记", Debug, ["Ctrl+M"];
    Act::LogBookmark, "log.bookmark", "添加 / 移除书签", Debug, ["Ctrl+F2"];
    Act::LogNextMark, "log.next_mark", "下一个书签", Debug, ["F2"];
    Act::LogPrevMark, "log.prev_mark", "上一个书签", Debug, ["Shift+F2"];
    Act::LogRules, "log.rules", "过滤与高亮规则", Debug, ["Ctrl+Shift+H"];
    Act::LogStats, "log.stats", "数据统计", Debug, [];
    Act::LogOpen, "log.open", "打开日志文件", Debug, ["Ctrl+O"];
    Act::TogglePlot, "plot.toggle", "显示 / 隐藏波形图", Debug, ["Ctrl+Shift+W"];
    Act::Modbus, "tools.modbus", "Modbus 主站", Debug, [];
    Act::SendFile, "tools.send_file", "发送文件（XMODEM / YMODEM）", Debug, [];
    Act::Checksum, "tools.checksum", "校验计算器", Global, [], true;
    Act::NewWindow, "window.new", "新建窗口", Global, ["Ctrl+Shift+N"], true;
    Act::Quick(0), "quick.1", "发送快捷命令 1", Debug, ["Alt+1"];
    Act::Quick(1), "quick.2", "发送快捷命令 2", Debug, ["Alt+2"];
    Act::Quick(2), "quick.3", "发送快捷命令 3", Debug, ["Alt+3"];
    Act::Quick(3), "quick.4", "发送快捷命令 4", Debug, ["Alt+4"];
    Act::Quick(4), "quick.5", "发送快捷命令 5", Debug, ["Alt+5"];
    Act::Quick(5), "quick.6", "发送快捷命令 6", Debug, ["Alt+6"];
    Act::Quick(6), "quick.7", "发送快捷命令 7", Debug, ["Alt+7"];
    Act::Quick(7), "quick.8", "发送快捷命令 8", Debug, ["Alt+8"];
    Act::Quick(8), "quick.9", "发送快捷命令 9", Debug, ["Alt+9"];
    Act::QuickOpen, "code.quick_open", "快速打开文件", Code, ["Ctrl+P"];
    Act::GotoSymbolWs, "code.symbol_ws", "跳到工作区符号", Code, ["Ctrl+T"];
    Act::GotoSymbol, "code.symbol", "跳到文件内符号", Code, ["Ctrl+Shift+O"];
    Act::GotoLine, "code.line", "跳到行", Code, ["Ctrl+G"];
    Act::OpenFile, "code.open_file", "打开文件", Code, ["Ctrl+O"];
    Act::SearchAll, "code.search", "全局搜索", Code, ["Ctrl+Shift+F"];
    Act::FindInFile, "code.find", "文件内查找", Code, ["Ctrl+F"];
    Act::ReplaceInFile, "code.replace", "文件内替换", Code, ["Ctrl+H"];
    Act::SplitEditor, "code.split", "拆分编辑器", Code, ["Ctrl+\\"];
    Act::ToggleBlame, "code.blame", "显示 / 隐藏行内 Blame", Code, [];
    Act::BitCalc, "tools.bitcalc", "位域计算器", Global, [], true;
    Act::ToggleBg, "view.background", "隐藏 / 显示背景图片", Global, [];
    Act::InsertSnippet, "code.snippet", "插入代码片段", Code, ["Ctrl+Alt+J"];
    Act::ToggleVim, "code.vim", "打开 / 关闭 Vim 键位", Code, [];
    Act::CloseTab, "code.close_tab", "关闭标签页", Code, ["Ctrl+W"];
    Act::NextTab, "code.next_tab", "下一个标签页", Code, ["Ctrl+Tab"];
    Act::SaveFile, "code.save", "保存文件", Code, ["Ctrl+S"];
    Act::ZoomIn, "code.zoom_in", "放大字号", Code, ["Ctrl+=", "Ctrl++"];
    Act::ZoomOut, "code.zoom_out", "缩小字号", Code, ["Ctrl+-"];
    Act::GotoDef, "code.goto_def", "跳转到定义", Code, ["F12"];
    Act::FindRefs, "code.refs", "查找引用", Code, ["Shift+F12"];
    Act::NavBack, "nav.back", "后退", Code, ["Alt+←"];
    Act::NavFwd, "nav.forward", "前进", Code, ["Alt+→"];
    Act::GitRefresh, "git.refresh", "刷新仓库", Git, ["F5"];
    Act::GitFetch, "git.fetch", "从所有远程拉取", Git, [];
    Act::GitFind, "git.find", "搜索提交", Git, ["Ctrl+F"];
    Act::ToggleComment, "edit.comment", "切换行注释", Code, ["Ctrl+/"];
    Act::MoveLineUp, "edit.move_up", "向上移动行", Code, ["Alt+↑"];
    Act::MoveLineDown, "edit.move_down", "向下移动行", Code, ["Alt+↓"];
    Act::CopyLineUp, "edit.copy_up", "向上复制行", Code, ["Shift+Alt+↑"];
    Act::CopyLineDown, "edit.copy_down", "向下复制行", Code, ["Shift+Alt+↓"];
    Act::DeleteLine, "edit.delete_line", "删除行", Code, ["Ctrl+Shift+K"];
    Act::InsertLineBelow, "edit.line_below", "在下方插入行", Code, ["Ctrl+Enter"];
    Act::InsertLineAbove, "edit.line_above", "在上方插入行", Code, ["Ctrl+Shift+Enter"];
    Act::SelectLine, "edit.select_line", "选中当前行", Code, ["Ctrl+L"];
    Act::AddNextOcc, "edit.next_occ", "把下一个相同内容加入选择", Code, ["Ctrl+D"];
    Act::SelectAllOcc, "edit.all_occ", "选中所有相同内容", Code, ["Ctrl+Shift+L"];
    Act::CursorAbove, "edit.cursor_above", "在上方添加光标", Code, ["Ctrl+Alt+↑"];
    Act::CursorBelow, "edit.cursor_below", "在下方添加光标", Code, ["Ctrl+Alt+↓"];
    Act::GotoBracket, "edit.goto_bracket", "跳到匹配的括号", Code, ["Ctrl+Shift+\\"];
    Act::Fold, "edit.fold", "折叠代码块", Code, ["Ctrl+Shift+[", "Ctrl+Shift+{"];
    Act::Unfold, "edit.unfold", "展开代码块", Code, ["Ctrl+Shift+]", "Ctrl+Shift+}"];
    Act::FoldAll, "edit.fold_all", "全部折叠", Code, ["Ctrl+K Ctrl+0"];
    Act::UnfoldAll, "edit.unfold_all", "全部展开", Code, ["Ctrl+K Ctrl+J"];
    Act::ToggleBookmark, "bookmark.toggle", "切换书签", Code, ["Ctrl+F2"];
    Act::NextBookmark, "bookmark.next", "下一个书签", Code, ["F2"];
    Act::PrevBookmark, "bookmark.prev", "上一个书签", Code, ["Shift+F2"];
    Act::ToggleContext, "code.context", "显示 / 隐藏上下文窗口", Code, ["Ctrl+Shift+Y"];
    Act::ToggleRelations, "code.relations", "显示 / 隐藏调用关系", Code, ["Ctrl+Shift+R"];
    Act::NavHistory, "nav.history", "导航历史", Code, ["Ctrl+Alt+-"];
    Act::LastEdit, "nav.last_edit", "跳到上次编辑的位置", Code, ["Ctrl+Q"];
    Act::SwitchHeader, "code.switch_header", "切换头文件 / 源文件", Code, ["Alt+O"];
    Act::MatchIf, "edit.match_if", "跳到配对的 #if / #else / #endif", Code, ["Ctrl+Alt+]"];
    Act::PrevFunc, "nav.prev_func", "上一个函数", Code, ["Alt+PageUp"];
    Act::NextFunc, "nav.next_func", "下一个函数", Code, ["Alt+PageDown"];
    Act::GotoDecl, "code.goto_decl", "跳转到声明", Code, ["Ctrl+F12"];
    Act::DefToSide, "code.def_side", "在侧边打开定义", Code, ["Ctrl+K F12"];
    Act::HlToggle, "hl.toggle", "高亮当前词（再按换颜色）", Code, ["F8"];
    Act::HlRemove, "hl.remove", "取消当前词的高亮", Code, ["Shift+F8"];
    Act::HlClear, "hl.clear", "清除全部高亮", Code, ["Ctrl+Shift+F8"];
    Act::HlNext, "hl.next", "下一个高亮词", Code, ["F3"];
    Act::HlPrev, "hl.prev", "上一个高亮词", Code, ["Shift+F3"];
    Act::BlockComment, "edit.block_comment", "切换块注释", Code, ["Ctrl+Shift+/", "Ctrl+Shift+?", "Shift+Alt+A"];
    Act::IfZero, "edit.if0", "用 #if 0 包裹 / 取消", Code, ["Ctrl+Alt+/"];
    Act::AlignSel, "edit.align", "按 = 或 , 对齐选中的行", Code, [];
    Act::CaseConvert, "edit.case", "切换命名风格（snake / camel / Pascal / 大写）", Code, ["Ctrl+Alt+U"];
    Act::EnumSwitch, "gen.enum_switch", "由枚举生成 switch", Code, [];
    Act::DeclToHeader, "gen.decl", "在头文件加声明 / 在源文件加定义", Code, [];
    Act::RemoveDebug, "gen.remove_debug", "删除插入的调试打印", Code, ["Ctrl+Shift+Alt+P"];
    Act::RecToggle, "rec.toggle", "开始 / 停止录制按键", Code, ["Shift+Alt+R"];
    Act::RecPlay, "rec.play", "回放录制的按键", Code, ["Shift+Alt+P"];
    Act::RecSave, "rec.save", "把录制的按键保存为宏", Code, [];
    Act::OpenProject, "project.open", "打开工程文件夹", Global, [], true;
    Act::Checkpoint, "history.checkpoint", "新建存档点", Code, [];
    Act::ReviewChanges, "history.review", "审阅外部修改", Code, [];
    Act::Timeline, "history.timeline", "本地历史时间线", Code, [];
    Act::FileHistory, "history.file", "当前文件的本地历史", Code, [];
    Act::ToggleWrap, "view.wrap", "切换自动换行", Code, ["Alt+Z"];
    Act::ToggleWhitespace, "view.whitespace", "显示 / 隐藏空白字符", Code, [];
    Act::TrimWhitespace, "edit.trim_ws", "删除行尾空格", Code, [];
    Act::FormatDoc, "edit.format", "格式化文件或选中部分（clang-format）", Code, ["Shift+Alt+F"];
    Act::ProjectUtf8, "encoding.project_utf8", "整个工程 GBK 转 UTF-8", Code, [];
    Act::RegPanel, "svd.panel", "寄存器面板（SVD）", Code, [];
    Act::Macro(0), "macro.1", "代码生成动作", Code, ["Ctrl+Alt+I"];
    Act::Macro(1), "macro.2", "代码生成动作", Code, [];
    Act::Macro(2), "macro.3", "代码生成动作", Code, ["Ctrl+Alt+P"];
    Act::Macro(3), "macro.4", "代码生成动作", Code, [];
    Act::Macro(4), "macro.5", "代码生成动作", Code, ["Ctrl+Alt+D"];
    Act::Macro(5), "macro.6", "代码生成动作", Code, ["Ctrl+Alt+H"];
    Act::Macro(6), "macro.7", "代码生成动作", Code, [];
    Act::Macro(7), "macro.8", "代码生成动作", Code, [];
    Act::Macro(8), "macro.9", "代码生成动作", Code, [];
    Act::Macro(9), "macro.10", "代码生成动作", Code, [];
    Act::Macro(10), "macro.11", "代码生成动作", Code, [];
    Act::Macro(11), "macro.12", "代码生成动作", Code, [];
    Act::Macro(12), "macro.13", "代码生成动作", Code, [];
    Act::Macro(13), "macro.14", "代码生成动作", Code, [];
    Act::Macro(14), "macro.15", "代码生成动作", Code, [];
    Act::Macro(15), "macro.16", "代码生成动作", Code, [];
}

/// 代码生成动作的槽位数
pub const MACRO_SLOTS: usize = 16;

static MACRO_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// 代码页把动作名字告诉这里，命令面板、快捷键设置就能显示真实名字
pub fn set_macro_names(v: Vec<String>) {
    if let Ok(mut m) = MACRO_NAMES.lock() {
        *m = v;
    }
}

/// 命令显示的名字（已翻译）
pub fn act_label(a: Act) -> String {
    if let Act::Macro(i) = a {
        let name = MACRO_NAMES.lock().ok().and_then(|m| m.get(i as usize).cloned()).filter(|n| !n.is_empty());
        return match name {
            Some(n) => format!("{}：{}", t("代码生成"), n),
            None => format!("{} {}", t("代码生成动作"), i + 1),
        };
    }
    t(def(a).label).to_string()
}

/// 快捷键方案：在默认键位上换掉一部分，用户的单独修改仍然优先
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Scheme {
    #[default]
    Qccom,
    VsCode,
    SourceInsight,
    Keil,
}

impl Scheme {
    pub const ALL: [Scheme; 4] = [Scheme::Qccom, Scheme::VsCode, Scheme::SourceInsight, Scheme::Keil];

    pub fn label(self) -> &'static str {
        match self {
            Scheme::Qccom => t("QCCOM 默认"),
            Scheme::VsCode => "VS Code",
            Scheme::SourceInsight => "Source Insight",
            Scheme::Keil => "Keil µVision",
        }
    }

    /// 这个方案里和默认不同的键位
    pub fn keys(self, id: &str) -> Option<&'static [&'static str]> {
        let table: &[(&str, &[&str])] = match self {
            Scheme::Qccom => &[],
            Scheme::VsCode => &[
                ("hl.toggle", &["Ctrl+K Ctrl+H"]),
                ("hl.remove", &[]),
                ("hl.clear", &["Ctrl+K Ctrl+Shift+H"]),
                ("hl.next", &["Ctrl+K Ctrl+N"]),
                ("hl.prev", &["Ctrl+K Ctrl+B"]),
                ("nav.last_edit", &["Ctrl+K Ctrl+Q"]),
                ("edit.block_comment", &["Shift+Alt+A"]),
                ("nav.history", &["Ctrl+Alt+-"]),
                ("code.goto_decl", &[]),
                ("edit.case", &[]),
            ],
            Scheme::SourceInsight => &[
                ("nav.back", &["Alt+,", "Alt+←"]),
                ("nav.forward", &["Alt+.", "Alt+→"]),
                ("code.goto_def", &["Ctrl+=", "F12"]),
                ("code.refs", &["Ctrl+/", "Shift+F12"]),
                ("edit.comment", &["Ctrl+K Ctrl+C"]),
                ("code.zoom_in", &["Ctrl++"]),
                ("hl.toggle", &["Shift+F8", "F8"]),
                ("hl.remove", &[]),
                ("hl.next", &["F4"]),
                ("hl.prev", &["F3"]),
                ("code.line", &["F5", "Ctrl+G"]),
                ("code.symbol", &["Alt+F8", "Ctrl+Shift+O"]),
                ("code.symbol_ws", &["F7", "Ctrl+T"]),
                ("code.context", &["Ctrl+Shift+Y", "Alt+F7"]),
                ("bookmark.toggle", &["Ctrl+M", "Ctrl+F2"]),
                ("edit.select_line", &["Shift+F6", "Ctrl+L"]),
            ],
            Scheme::Keil => &[
                ("nav.back", &["Ctrl+-", "Alt+←"]),
                ("nav.forward", &["Ctrl+Shift+-", "Alt+→"]),
                ("code.zoom_out", &["Ctrl+Alt+-"]),
                ("nav.history", &["Ctrl+Alt+H"]),
                ("macro.6", &[]),
                ("edit.goto_bracket", &["Ctrl+E", "Ctrl+Shift+\\"]),
                ("hl.next", &["F3"]),
                ("hl.prev", &["Shift+F3"]),
                ("code.goto_def", &["F12"]),
                ("code.refs", &["Shift+F12"]),
            ],
        };
        table.iter().find(|(i, _)| *i == id).map(|(_, k)| *k)
    }
}

pub fn def(a: Act) -> &'static ActDef {
    ACTIONS.iter().find(|d| d.act == a).expect("action registered")
}

/// 用户改过的绑定：命令 id → 键位字符串列表（空列表 = 删除了全部绑定）
pub type Overrides = BTreeMap<String, Vec<String>>;

pub struct Keymap {
    /// (命令, 绑定) 的当前生效表
    table: Vec<(Act, Chord)>,
    pending: Option<(Combo, Instant)>,
    pub scheme: Scheme,
}

impl Keymap {
    pub fn with_scheme(ov: &Overrides, scheme: Scheme) -> Keymap {
        let mut k = Keymap { table: vec![], pending: None, scheme };
        k.rebuild(ov);
        k
    }

    /// 当前方案下这个命令的默认键位
    pub fn defaults(&self, a: Act) -> &'static [&'static str] {
        let d = def(a);
        self.scheme.keys(d.id).unwrap_or(d.keys)
    }

    pub fn rebuild(&mut self, ov: &Overrides) {
        self.table.clear();
        for d in ACTIONS {
            match ov.get(d.id) {
                Some(list) => self.table.extend(list.iter().filter_map(|s| parse_chord(s)).map(|c| (d.act, c))),
                None => {
                    let keys = self.scheme.keys(d.id).unwrap_or(d.keys);
                    self.table.extend(keys.iter().filter_map(|s| parse_chord(s)).map(|c| (d.act, c)))
                }
            }
        }
    }

    pub fn bindings(&self, a: Act) -> Vec<&Chord> {
        self.table.iter().filter(|(x, _)| *x == a).map(|(_, c)| c).collect()
    }

    /// 第一个绑定的文字，用于菜单和提示
    pub fn label(&self, a: Act) -> String {
        self.bindings(a).first().map(|c| chord_str(c)).unwrap_or_default()
    }

    /// 等待序列第二个键时，状态栏显示的提示
    pub fn pending(&self) -> Option<String> {
        self.pending.map(|(c, _)| c.to_string())
    }

    /// 和这个绑定冲突（同一范围或全局重叠）的其他命令
    pub fn conflicts(&self, a: Act, chord: &Chord) -> Vec<Act> {
        let s = def(a).scope;
        self.table
            .iter()
            .filter(|(x, c)| *x != a && c == chord && (def(*x).scope == s || def(*x).scope == Scope::Global || s == Scope::Global))
            .map(|(x, _)| *x)
            .collect()
    }

    /// 把这一帧的按键翻译成命令。`scope` 是当前页面；`term` 表示终端拿着键盘焦点
    pub fn poll(&mut self, ctx: &egui::Context, scope: Scope, term: bool) -> Vec<Act> {
        if self.pending.is_some_and(|(_, at)| at.elapsed() > Duration::from_millis(1500)) {
            self.pending = None;
        }
        let active = |a: Act| {
            let d = def(a);
            (d.scope == scope || d.scope == Scope::Global) && (!term || d.in_term)
        };
        // 输入框有焦点时，把编辑用的按键留给输入框
        let typing = ctx.wants_keyboard_input();
        let mut out = vec![];
        let table = &self.table;
        let pending = &mut self.pending;
        // Alt / Shift+Alt 组合键在有些系统上还会跟一个文字事件（如 Shift+Alt+R 打出 R），一起吃掉
        let mut eat_text = false;
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                if let egui::Event::Text(t) = e {
                    if std::mem::take(&mut eat_text) && t.chars().count() == 1 {
                        return false;
                    }
                    return true;
                }
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { return true };
                if matches!(key, Key::Escape) && pending.is_some() {
                    *pending = None;
                    return false;
                }
                let combo = Combo::from_event(*key, *modifiers);
                let edit_key = match combo.key {
                    Key::A | Key::Y | Key::X | Key::C | Key::V => !combo.shift,
                    Key::Z | Key::ArrowLeft | Key::ArrowRight | Key::Backspace | Key::Delete | Key::Home | Key::End => true,
                    _ => false,
                };
                if typing && combo.ctrl && !combo.alt && edit_key {
                    return true;
                }
                // 序列的第二个键
                if let Some((first, _)) = *pending {
                    *pending = None;
                    if let Some((a, _)) = table.iter().find(|(a, c)| c.len() == 2 && c[0] == first && c[1] == combo && active(*a)) {
                        out.push(*a);
                        return false;
                    }
                }
                // 页面范围的绑定优先于全局
                let single = table
                    .iter()
                    .filter(|(a, c)| c.len() == 1 && c[0] == combo && active(*a))
                    .min_by_key(|(a, _)| (def(*a).scope == Scope::Global) as u8)
                    .map(|(a, _)| *a);
                let starts_chord = table.iter().any(|(a, c)| c.len() == 2 && c[0] == combo && active(*a));
                if starts_chord {
                    *pending = Some((combo, Instant::now()));
                }
                if let Some(a) = single {
                    out.push(a);
                    eat_text = combo.alt && !combo.ctrl;
                    return false;
                }
                !starts_chord
            });
        });
        // 序列等待期间别让粘在一起的文字事件（例如 Ctrl+K 后的字母）漏进输入框
        if self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(1600));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let c = Combo::parse("Ctrl+Shift+P").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && c.key == Key::P);
        assert_eq!(parse_chord("Ctrl+K Ctrl+S").unwrap().len(), 2);
        assert_eq!(Combo::parse("Ctrl+`").unwrap().key, Key::Backtick);
        assert_eq!(Combo::parse("Ctrl++").unwrap().key, Key::Plus);
        assert_eq!(Combo::parse("Alt+←").unwrap().key, Key::ArrowLeft);
        assert_eq!(Combo::parse("F12").unwrap().key, Key::F12);
        for d in ACTIONS {
            let mut all: Vec<&str> = d.keys.to_vec();
            for sc in Scheme::ALL {
                all.extend(sc.keys(d.id).unwrap_or(&[]));
            }
            for k in all {
                let c = parse_chord(k).unwrap_or_else(|| panic!("bad default {k}"));
                assert!(c.iter().all(|x| x.usable()), "{k}");
                if !cfg!(target_os = "macos") {
                    assert_eq!(parse_chord(&chord_str(&c)), Some(c.clone()), "{k}");
                }
            }
        }
    }

    #[test]
    fn ids_unique() {
        let mut ids: Vec<&str> = ACTIONS.iter().map(|d| d.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len());
    }

    #[test]
    fn scheme_ids_exist() {
        for sc in Scheme::ALL {
            let k = Keymap::with_scheme(&Overrides::new(), sc);
            assert!(!k.table.is_empty());
        }
        // 方案表里的 id 都要真实存在
        for id in ["hl.toggle", "nav.back", "macro.6", "edit.goto_bracket", "code.context", "edit.select_line", "bookmark.toggle"] {
            assert!(ACTIONS.iter().any(|d| d.id == id), "{id}");
        }
        let si = Keymap::with_scheme(&Overrides::new(), Scheme::SourceInsight);
        assert_eq!(si.label(Act::NavBack), "Alt+,");
        assert_eq!(si.label(Act::FindRefs), "Ctrl+/");
    }

    #[test]
    fn overrides() {
        let mut ov = Overrides::new();
        ov.insert("log.clear".into(), vec!["Ctrl+Shift+L".into()]);
        ov.insert("log.save".into(), vec![]);
        let k = Keymap::with_scheme(&ov, Scheme::Qccom);
        assert_eq!(k.label(Act::LogClear), "Ctrl+Shift+L");
        assert!(k.bindings(Act::LogSave).is_empty());
        assert_eq!(k.label(Act::Palette), "Ctrl+Shift+P");
    }
}

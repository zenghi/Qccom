//! 副页面和工作台：任何大页面都能当主页面，也能挂在右边或下方当副页面。
//!
//! - 副页面和整页是同一个实例（同一份日志、同一个编辑器、同一个仿真），只是画的位置不同；
//!   同一个页面同时只在一个位置出现，放到另一边就是互换。
//! - 三种打开方式：自动（发生需要两样一起看的事）、手动（拖动、右键、中键、命令面板）、
//!   来回切页时的提示。
//! - 自动弹出的防打扰：收起就降级为只提示、打字时不弹、不替换手动放的、同一提示 10 分钟一次、
//!   看窗口宽度、专注模式（F11）暂停、自动弹出不进后退记录。

use super::toolbox::{self, Tool};
use super::*;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// 能放进主区域或副页面的东西
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub(super) enum Pane {
    Debug,
    Code,
    Git,
    Compare,
    Community,
    /// 只有资源管理器（文件树）
    Files,
    Tool(Tool),
}

impl Pane {
    pub(super) fn of(page: Page, tool: Tool) -> Option<Pane> {
        Some(match page {
            Page::Debug => Pane::Debug,
            Page::Code => Pane::Code,
            Page::Git => Pane::Git,
            Page::Compare => Pane::Compare,
            Page::Community => Pane::Community,
            Page::Tools => Pane::Tool(tool),
            Page::Settings => return None,
        })
    }

    pub(super) fn label(self) -> String {
        match self {
            Pane::Debug => t("串口").into(),
            Pane::Code => t("代码").into(),
            Pane::Git => "Git".into(),
            Pane::Compare => t("比较").into(),
            Pane::Community => t("社区").into(),
            Pane::Files => t("文件").into(),
            Pane::Tool(tool) => toolbox::tool_text(tool).0.into(),
        }
    }

    pub(super) fn icon(self) -> Icon {
        match self {
            Pane::Debug => Icon::Pulse,
            Pane::Code => Icon::Code,
            Pane::Git => Icon::Branch,
            Pane::Compare => Icon::Compare,
            Pane::Community => Icon::Chat,
            Pane::Files => Icon::Files,
            Pane::Tool(Tool::Ssh) => Icon::Terminal,
            Pane::Tool(_) => Icon::Toolbox,
        }
    }

    /// 用到串口 / 网络会话的页面：画之前要把对应的会话换进来
    pub(super) fn slot(self) -> Option<usize> {
        match self {
            Pane::Debug => Some(0),
            Pane::Tool(tool) => toolbox::comm_slot(tool),
            _ => None,
        }
    }

    fn key(self) -> String {
        format!("{self:?}")
    }
}

/// 副页面放哪
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub(super) enum DockPos {
    #[default]
    Right,
    Bottom,
}

/// 一种布局：主页面 + 副页面（可以有几个标签）+ 位置 + 比例 + 底部终端开没开
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub(super) struct Layout {
    pub main: Option<Pane>,
    pub tabs: Vec<Pane>,
    pub cur: usize,
    pub pos: DockPos,
    pub frac: f32,
    pub term: bool,
    /// 比较页左边带文件树
    pub tree: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            main: None,
            tabs: vec![],
            cur: 0,
            pos: DockPos::Right,
            frac: DEFAULT_FRAC,
            term: false,
            tree: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(super) struct Scheme {
    pub name: String,
    pub lay: Layout,
}

/// 内置的三个方案（名字按当前语言翻译）
pub(super) fn builtin_schemes() -> Vec<Scheme> {
    vec![
        Scheme {
            name: t("调固件").into(),
            lay: Layout {
                main: Some(Pane::Code),
                tabs: vec![Pane::Debug],
                frac: 0.38,
                term: true,
                ..Default::default()
            },
        },
        Scheme {
            name: t("仿真联调").into(),
            lay: Layout {
                main: Some(Pane::Tool(Tool::Circuit)),
                tabs: vec![Pane::Debug],
                frac: 0.36,
                ..Default::default()
            },
        },
        Scheme {
            name: t("审阅").into(),
            lay: Layout {
                main: Some(Pane::Compare),
                tabs: vec![Pane::Code],
                frac: 0.38,
                tree: true,
                ..Default::default()
            },
        },
    ]
}

pub(super) const DEFAULT_FRAC: f32 = 0.35;
/// 副页面最窄（再窄就收起）
pub(super) const MIN_SIDE: f32 = 280.0;

/// 设置里存的部分
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct WorkCfg {
    /// 需要时自动打开副页面
    pub auto: bool,
    /// 来回切页时提示放在一起
    pub switch_hint: bool,
    /// 窄屏时把副页面放下方
    pub narrow_bottom: bool,
    /// 常用组合（点过「放在一起看」）：(主, 副)
    pub combos: Vec<(Pane, Pane)>,
    /// 点过「不用」的组合，不再提示
    pub muted: Vec<(Pane, Pane)>,
    /// 连续点「不用」的次数，到 3 次问一句「以后都不提醒？」
    pub mute_streak: u8,
    /// 「不再自动」：触发名@工程
    pub no_auto: Vec<String>,
    /// 用户保存的布局方案
    pub schemes: Vec<Scheme>,
    /// 每个工程上次的布局
    pub proj: BTreeMap<String, Layout>,
    /// 打开某个工程时用哪个方案
    pub proj_scheme: BTreeMap<String, String>,
    /// 比较页左边带文件树
    pub cmp_tree: bool,
    /// 当前布局（下次启动还原）
    pub last: Layout,
}

impl Default for WorkCfg {
    fn default() -> Self {
        Self {
            auto: true,
            switch_hint: true,
            narrow_bottom: true,
            combos: vec![],
            muted: vec![],
            mute_streak: 0,
            no_auto: vec![],
            schemes: vec![],
            proj: BTreeMap::new(),
            proj_scheme: BTreeMap::new(),
            cmp_tree: true,
            last: Layout::default(),
        }
    }
}

/// 状态栏上的小提示：「↳ 日志里有 HardFault · 在代码旁看」
pub(super) struct Chip {
    pub text: String,
    pub pane: Pane,
    pub key: String,
    pub at: Instant,
    pub go: Option<(PathBuf, usize)>,
}

/// 来回切页的提示条
pub(super) struct PairHint {
    pub main: Pane,
    pub other: Pane,
    pub at: Instant,
    /// 连续拒绝 3 次后改成问「以后都不提醒？」
    pub ask_off: bool,
}

/// 正在拖动的页面（活动栏图标、工具标签、文件标签、副页面标题栏）
pub(super) struct DragPane {
    pub pane: Pane,
    pub from: egui::Pos2,
    pub live: bool,
    /// 代码页的文件标签：放到右边 = 拆分编辑器
    pub file: Option<PathBuf>,
    /// 拖的是副页面自己
    pub own: bool,
}

/// 放下的位置
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum DropAt {
    Right,
    Bottom,
    Center,
}

/// 运行时状态
pub(super) struct Work {
    pub tabs: Vec<Pane>,
    pub cur: usize,
    pub pos: DockPos,
    pub frac: f32,
    /// Ctrl+Alt+\ 收起时记住，再按还原
    pub hidden: Vec<Pane>,
    /// 当前副页面是自动弹出的：触发名
    pub auto: Option<String>,
    /// 副页面里有用户自己放的
    pub manual: bool,
    /// 键盘焦点在副页面
    pub focus_side: bool,
    pub rect: Option<Rect>,
    pub main_rect: Option<Rect>,
    /// 专注模式：暂停自动弹出
    pub focus: bool,
    pub typed_at: Option<Instant>,
    pub chip: Option<Chip>,
    pub hinted: HashMap<String, Instant>,
    /// 打字时先放着、停下来再弹
    pub pending: Option<(Pane, String, Option<DockPos>)>,
    pub switches: VecDeque<(Pane, Instant)>,
    pub last_main: Option<Pane>,
    pub pair: Option<PairHint>,
    pub pair_day: HashMap<String, Instant>,
    pub drag: Option<DragPane>,
    pub detached: bool,
    pub proj_key: String,
    /// 打开动画
    pub opened_at: Option<Instant>,
    pub loaded: bool,
    /// 串口连上了仿真口（调试页 + 电路）
    pub sim_opened: bool,
    /// 电路生成的代码写在哪个文件（代码页里打开，规则改了自动更新）
    pub gen_file: Option<PathBuf>,
    pub gen_at: Option<Instant>,
    /// 代码里选中的文字，日志里高亮
    pub link: String,
    /// 日志扫描到哪一行（HardFault、文件:行号的提示）
    pub scan_upto: u64,
}

impl Default for Work {
    fn default() -> Self {
        Self {
            tabs: vec![],
            cur: 0,
            pos: DockPos::Right,
            frac: DEFAULT_FRAC,
            hidden: vec![],
            auto: None,
            manual: false,
            focus_side: false,
            rect: None,
            main_rect: None,
            focus: false,
            typed_at: None,
            chip: None,
            hinted: HashMap::new(),
            pending: None,
            switches: VecDeque::new(),
            last_main: None,
            pair: None,
            pair_day: HashMap::new(),
            drag: None,
            detached: false,
            proj_key: String::new(),
            opened_at: None,
            loaded: false,
            sim_opened: false,
            gen_file: None,
            gen_at: None,
            link: String::new(),
            scan_upto: 0,
        }
    }
}

/// 自动弹出的强度
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Strength {
    /// 用户自己发起的动作（右键比较、生成代码、点提交里的文件）：直接在副页面打开
    Strong,
    /// 状态变化（烧录、通电、连上仿真口）：第一次弹，收起后只提示
    Medium,
    /// 内容里发现的（HardFault、文件:行号）：只提示
    Weak,
}

impl App {
    pub(super) fn main_pane(&self) -> Option<Pane> {
        Pane::of(self.page, self.s.tb.tool)
    }

    pub(super) fn side_pane(&self) -> Option<Pane> {
        self.work.tabs.get(self.work.cur).copied()
    }

    /// 副页面能不能放这个（设置页不行；和主页面相同的不行）
    fn side_ok(&self, p: Pane) -> bool {
        self.main_pane() != Some(p)
    }

    /// 窗口宽度够不够并排；返回该放的位置（None = 太窄，只提示）
    pub(super) fn fit_pos(&self, want: Option<DockPos>) -> Option<DockPos> {
        let w = self.ctx.screen_rect().width();
        if let Some(p) = want {
            return Some(p);
        }
        if !self.work.tabs.is_empty() {
            return Some(self.work.pos);
        }
        if w >= 1400.0 {
            Some(DockPos::Right)
        } else if w >= 1000.0 {
            Some(if self.s.work.narrow_bottom { DockPos::Bottom } else { DockPos::Right })
        } else {
            None
        }
    }

    /// 在副页面打开。manual = 用户自己放的（拖动、右键、命令面板）
    pub(super) fn open_side(&mut self, p: Pane, pos: Option<DockPos>, manual: bool) {
        if !self.side_ok(p) {
            return;
        }
        if self.work.tabs.is_empty() {
            self.work.opened_at = Some(Instant::now());
            self.work.manual = false;
        }
        if let Some(i) = self.work.tabs.iter().position(|x| *x == p) {
            self.work.cur = i;
        } else {
            // 最多 3 个标签：挤掉最早的（不是当前的）
            if self.work.tabs.len() >= 3 {
                let drop = (0..self.work.tabs.len()).find(|i| *i != self.work.cur).unwrap_or(0);
                self.work.tabs.remove(drop);
            }
            self.work.tabs.push(p);
            self.work.cur = self.work.tabs.len() - 1;
        }
        if let Some(pos) = pos {
            self.work.pos = pos;
        }
        if manual {
            self.work.manual = true;
            self.work.auto = None;
            // 用户自己开的：进后退记录，但不出「← 返回」胶囊
            self.nav_manual();
        }
        self.work.hidden.clear();
        self.pane_activate(p);
        crate::hub::count("side.open");
    }

    /// 页面第一次显示时要做的事（和 set_page 里的一样）
    pub(super) fn pane_activate(&mut self, p: Pane) {
        match p {
            Pane::Code | Pane::Files => self.code.activate(),
            Pane::Git => {
                let root = self.code.root().map(|r| r.to_path_buf());
                self.gitv.activate(root.as_deref());
            }
            Pane::Tool(tool) => {
                if tool == Tool::Circuit && self.circ.is_none() {
                    self.circ = Some(Box::new(crate::circuitview::CircuitView::new()));
                }
            }
            _ => {}
        }
    }

    /// 收起副页面。自动弹出的被收起：这种触发在这个工程里以后只提示
    pub(super) fn close_side(&mut self) {
        if self.work.tabs.is_empty() {
            return;
        }
        if let Some(k) = self.work.auto.take() {
            let key = self.no_auto_key(&k);
            if !self.s.work.no_auto.contains(&key) {
                self.s.work.no_auto.push(key);
            }
        }
        self.work.hidden = std::mem::take(&mut self.work.tabs);
        self.work.cur = 0;
        self.work.manual = false;
        self.work.detached = false;
        self.work.focus_side = false;
        self.nav_manual();
    }

    /// 关掉副页面里的一个标签
    pub(super) fn close_side_tab(&mut self, i: usize) {
        if self.work.tabs.len() <= 1 {
            self.close_side();
            return;
        }
        self.work.tabs.remove(i);
        if self.work.cur >= self.work.tabs.len() {
            self.work.cur = self.work.tabs.len() - 1;
        }
    }

    /// Ctrl+Alt+\：有提示条时接受提示；有副页面就收起；收起过就还原
    pub(super) fn toggle_side(&mut self) {
        if self.work.pair.is_some() {
            self.accept_pair();
            return;
        }
        if !self.work.tabs.is_empty() {
            self.close_side();
            return;
        }
        let hidden = std::mem::take(&mut self.work.hidden);
        let hidden: Vec<Pane> = hidden.into_iter().filter(|p| self.side_ok(*p)).collect();
        if !hidden.is_empty() {
            for p in hidden {
                self.open_side(p, None, true);
            }
            return;
        }
        // 什么都没有：按常用组合，或者把上一个页面放旁边
        let main = self.main_pane();
        let other = self
            .s
            .work
            .combos
            .iter()
            .find(|(a, _)| Some(*a) == main)
            .map(|c| c.1)
            .or_else(|| self.work.switches.iter().rev().map(|x| x.0).find(|p| Some(*p) != main));
        match other {
            Some(p) => self.open_side(p, self.fit_pos(None).or(Some(DockPos::Bottom)), true),
            None => self.toast(t("把活动栏图标或工具标签拖到右边，就能并排看"), ACCENT()),
        }
    }

    /// 主页面和副页面互换
    pub(super) fn swap_side(&mut self) {
        let (Some(side), Some(main)) = (self.side_pane(), self.main_pane()) else { return };
        self.work.tabs[self.work.cur] = main;
        self.work.auto = None;
        self.nav_manual();
        self.goto_pane(side);
        self.work.focus_side = false;
    }

    /// 把某个页面放到主区域
    pub(super) fn goto_pane(&mut self, p: Pane) {
        // 要显示的页面正在副页面里：互换
        if let Some(i) = self.work.tabs.iter().position(|x| *x == p) {
            match self.main_pane() {
                Some(m) if m != Pane::Files => self.work.tabs[i] = m,
                _ => {
                    self.work.tabs.remove(i);
                    if self.work.cur >= self.work.tabs.len() {
                        self.work.cur = self.work.tabs.len().saturating_sub(1);
                    }
                }
            }
        }
        match p {
            Pane::Debug => self.set_page(Page::Debug),
            Pane::Code | Pane::Files => self.set_page(Page::Code),
            Pane::Git => self.set_page(Page::Git),
            Pane::Compare => self.set_page(Page::Compare),
            Pane::Community => self.set_page(Page::Community),
            Pane::Tool(tool) => self.open_tool(tool),
        }
    }

    /// 页面切到 p 之前：p 在副页面里就换出来（同一页面不同时出现在两处）
    pub(super) fn side_unclash(&mut self) {
        let Some(m) = self.main_pane() else { return };
        if let Some(i) = self.work.tabs.iter().position(|x| *x == m) {
            match self.work.last_main.filter(|l| *l != m && *l != Pane::Files) {
                Some(prev) if !self.work.tabs.contains(&prev) => self.work.tabs[i] = prev,
                _ => {
                    self.work.tabs.remove(i);
                    if self.work.cur >= self.work.tabs.len() {
                        self.work.cur = self.work.tabs.len().saturating_sub(1);
                    }
                }
            }
            if self.work.tabs.is_empty() {
                self.work.auto = None;
                self.work.manual = false;
            }
        }
    }

    pub(super) fn set_dock_pos(&mut self, pos: DockPos) {
        self.work.pos = pos;
        if self.work.tabs.is_empty() {
            self.toggle_side();
            self.work.pos = pos;
        }
    }

    fn no_auto_key(&self, trig: &str) -> String {
        format!("{trig}@{}", self.work.proj_key)
    }

    /// 系统判断该一起看时调用：按强度和防打扰规则决定弹出、加标签还是只提示。返回是否已在副页面显示
    pub(super) fn auto_side(&mut self, p: Pane, trig: &str, strength: Strength, hint: &str) -> bool {
        if !self.side_ok(p) {
            return false;
        }
        if self.side_pane() == Some(p) {
            return true;
        }
        let narrow = self.fit_pos(None).is_none();
        if strength == Strength::Strong {
            if narrow {
                return false;
            }
            // 已经有用户自己放的：作为新标签加进去，当前显示换成这个（用户刚刚点的）
            self.open_side(p, self.fit_pos(None), false);
            return true;
        }
        let blocked = strength == Strength::Weak || !self.s.work.auto || self.work.focus || narrow || self.s.work.no_auto.contains(&self.no_auto_key(trig)) || self.work.detached;
        if blocked {
            self.side_hint(p, trig, hint, None);
            return false;
        }
        // 打字时不弹：停下来再弹
        if self.work.typed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            self.work.pending = Some((p, trig.to_string(), self.fit_pos(None)));
            return false;
        }
        self.auto_open_now(p, trig);
        true
    }

    fn auto_open_now(&mut self, p: Pane, trig: &str) {
        if self.work.manual && !self.work.tabs.is_empty() {
            // 不替换用户自己放的：加成标签，不抢当前显示
            let cur = self.work.cur;
            self.open_side(p, None, false);
            self.work.cur = cur;
            self.toast(tf("「{}」放进了副页面的标签里", &[&p.label()]), ACCENT());
            return;
        }
        self.open_side(p, self.fit_pos(None), false);
        self.work.auto = Some(trig.to_string());
        // 自动弹出不进后退记录
        self.nav_absorb();
    }

    /// 状态栏小提示，同一种 10 分钟一次
    pub(super) fn side_hint(&mut self, p: Pane, trig: &str, text: &str, go: Option<(PathBuf, usize)>) {
        if self.side_pane() == Some(p) && go.is_none() {
            return;
        }
        if self.work.hinted.get(trig).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            return;
        }
        self.work.hinted.insert(trig.to_string(), Instant::now());
        self.work.chip = Some(Chip {
            text: text.to_string(),
            pane: p,
            key: trig.to_string(),
            at: Instant::now(),
            go,
        });
    }

    /// 点了状态栏提示
    pub(super) fn take_chip(&mut self) {
        let Some(c) = self.work.chip.take() else { return };
        let pos = self.fit_pos(None).or(Some(DockPos::Bottom));
        if self.main_pane() != Some(c.pane) {
            self.open_side(c.pane, pos, true);
        }
        if let Some((f, line)) = c.go {
            self.code.goto_loc(&f, line);
        }
        let _ = c.key;
    }

    /// 每帧：打字记录、延后的弹出、切页记录、提示过期
    pub(super) fn work_tick(&mut self, ctx: &egui::Context) {
        if !self.work.loaded {
            self.work.loaded = true;
            let l = self.s.work.last.clone();
            self.work.pos = l.pos;
            self.work.frac = l.frac;
            self.work.hidden = l.tabs.into_iter().filter(|p| !matches!(p, Pane::Files)).collect();
        }
        let typing = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_) | egui::Event::Key { pressed: true, .. })));
        if typing && ctx.wants_keyboard_input() {
            self.work.typed_at = Some(Instant::now());
        }
        if self.work.pending.is_some() && !self.work.typed_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            if let Some((p, trig, _)) = self.work.pending.take() {
                if self.side_ok(p) {
                    self.auto_open_now(p, &trig);
                }
            }
        } else if self.work.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.work.chip.as_ref().is_some_and(|c| c.at.elapsed() > Duration::from_secs(60)) {
            self.work.chip = None;
        }
        // 主页面变了
        let main = self.main_pane();
        if main != self.work.last_main {
            self.side_unclash();
            if let Some(m) = main {
                self.on_main_switch(m);
            }
            self.work.last_main = main;
        }
        // 焦点：在哪一边按下鼠标，键盘就归哪一边
        if let Some(p) = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }) {
            self.work.focus_side = self.work.rect.is_some_and(|r| r.contains(p)) && !self.work.tabs.is_empty();
        }
        if self.work.tabs.is_empty() {
            self.work.focus_side = false;
            self.work.rect = None;
        }
        // 记住当前布局（全局一份、当前工程一份）
        let lay = self.current_layout();
        if lay != self.s.work.last {
            if !self.work.proj_key.is_empty() {
                self.s.work.proj.insert(self.work.proj_key.clone(), lay.clone());
            }
            self.s.work.last = lay;
        }
    }

    pub(super) fn current_layout(&self) -> Layout {
        Layout {
            main: self.main_pane(),
            tabs: if self.work.tabs.is_empty() { self.work.hidden.clone() } else { self.work.tabs.clone() },
            cur: self.work.cur,
            pos: self.work.pos,
            frac: self.work.frac,
            term: self.shells.s.open,
            tree: self.s.work.cmp_tree,
        }
    }

    /// 套用一个布局（方案、按工程记住的）
    pub(super) fn apply_layout(&mut self, l: &Layout) {
        self.work.tabs.clear();
        self.work.auto = None;
        self.work.detached = false;
        if let Some(m) = l.main {
            self.goto_pane(m);
        }
        for p in &l.tabs {
            if self.side_ok(*p) {
                self.open_side(*p, Some(l.pos), true);
            }
        }
        self.work.cur = l.cur.min(self.work.tabs.len().saturating_sub(1));
        self.work.pos = l.pos;
        self.work.frac = if l.frac > 0.1 { l.frac } else { DEFAULT_FRAC };
        self.s.work.cmp_tree = l.tree;
        if l.term && !self.shells.s.open {
            self.shells.s.open = true;
        }
        if l.main == Some(Pane::Tool(Tool::Circuit)) {
            self.circ.get_or_insert_with(|| Box::new(crate::circuitview::CircuitView::new())).show_scope();
        }
    }

    pub(super) fn all_schemes(&self) -> Vec<Scheme> {
        let mut v = builtin_schemes();
        v.extend(self.s.work.schemes.iter().cloned());
        v
    }

    pub(super) fn apply_scheme(&mut self, name: &str) {
        if let Some(sc) = self.all_schemes().into_iter().find(|s| s.name == name) {
            self.apply_layout(&sc.lay);
            self.toast(tf("布局：{}", &[&sc.name]), ACCENT());
        }
    }

    pub(super) fn save_scheme(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let lay = self.current_layout();
        self.s.work.schemes.retain(|s| s.name != name);
        self.s.work.schemes.push(Scheme { name: name.to_string(), lay });
        self.toast(tf("已保存布局「{}」", &[&name]), OK());
    }

    /// 重置布局：回到单页
    pub(super) fn reset_layout(&mut self) {
        self.work.tabs.clear();
        self.work.hidden.clear();
        self.work.auto = None;
        self.work.manual = false;
        self.work.detached = false;
        self.work.frac = DEFAULT_FRAC;
        self.work.pos = DockPos::Right;
    }

    /// 换了工程：记下旧工程的布局，还原新工程的（或它指定的方案）
    pub(super) fn work_project_changed(&mut self, root: &std::path::Path) {
        let key = root.to_string_lossy().to_string();
        if key == self.work.proj_key {
            return;
        }
        let old = std::mem::replace(&mut self.work.proj_key, key.clone());
        if !old.is_empty() {
            let lay = self.current_layout();
            self.s.work.proj.insert(old, lay);
        }
        if let Some(name) = self.s.work.proj_scheme.get(&key).cloned() {
            self.apply_scheme(&name);
        } else if let Some(l) = self.s.work.proj.get(&key).cloned() {
            if !l.tabs.is_empty() {
                // 只还原副页面，不抢主页面（用户刚打开工程，应该看代码页）
                self.work.tabs.clear();
                for p in &l.tabs {
                    if self.side_ok(*p) {
                        self.open_side(*p, Some(l.pos), true);
                    }
                }
                self.work.frac = l.frac;
            }
        }
    }

    // ------------------------------------------------------------ 来回切页

    fn on_main_switch(&mut self, m: Pane) {
        let now = Instant::now();
        let prev = self.work.switches.back().map(|x| x.0);
        // 常用组合：从 A 切到 B 时直接并排
        if let Some(a) = prev {
            if self.work.tabs.is_empty() && self.s.work.combos.iter().any(|c| (c.0 == a && c.1 == m) || (c.0 == m && c.1 == a)) && self.fit_pos(None).is_some() {
                self.open_side(a, self.fit_pos(None), true);
                self.nav_absorb();
            }
        }
        // 两次切换间隔小于 1 秒：在翻找，不算
        if let Some((_, at)) = self.work.switches.back() {
            if at.elapsed() < Duration::from_secs(1) {
                self.work.switches.pop_back();
            }
        }
        self.work.switches.push_back((m, now));
        while self.work.switches.len() > 8 {
            self.work.switches.pop_front();
        }
        if !self.s.work.switch_hint || self.work.pair.is_some() || !self.work.tabs.is_empty() {
            return;
        }
        // 2 分钟内在同样两个页面之间来回 3 次（A→B→A→B→A）
        let v: Vec<(Pane, Instant)> = self.work.switches.iter().rev().take(5).copied().collect();
        if v.len() < 5 || v[4].1.elapsed() > Duration::from_secs(120) {
            return;
        }
        let (a, b) = (v[0].0, v[1].0);
        if a == b || !(v[2].0 == a && v[3].0 == b && v[4].0 == a) {
            return;
        }
        let both = |x: &(Pane, Pane)| (x.0 == a && x.1 == b) || (x.0 == b && x.1 == a);
        if self.s.work.muted.iter().any(both) || self.s.work.combos.iter().any(both) {
            return;
        }
        let k = pair_key(a, b);
        if self.work.pair_day.get(&k).is_some_and(|t| t.elapsed() < Duration::from_secs(24 * 3600)) {
            return;
        }
        self.work.pair_day.insert(k, now);
        self.work.pair = Some(PairHint {
            main: a,
            other: b,
            at: now,
            ask_off: false,
        });
    }

    /// 「放在一起看」
    pub(super) fn accept_pair(&mut self) {
        let Some(h) = self.work.pair.take() else { return };
        if h.ask_off {
            return;
        }
        self.s.work.mute_streak = 0;
        if !self.s.work.combos.contains(&(h.main, h.other)) {
            self.s.work.combos.push((h.main, h.other));
        }
        if self.main_pane() != Some(h.main) {
            self.goto_pane(h.main);
        }
        self.open_side(h.other, self.fit_pos(None).or(Some(DockPos::Bottom)), true);
    }

    /// 「不用」
    pub(super) fn decline_pair(&mut self) {
        let Some(h) = self.work.pair.take() else { return };
        if h.ask_off {
            return;
        }
        self.s.work.muted.push((h.main, h.other));
        self.s.work.mute_streak += 1;
        if self.s.work.mute_streak >= 3 {
            self.s.work.mute_streak = 0;
            self.work.pair = Some(PairHint {
                ask_off: true,
                at: Instant::now(),
                ..h
            });
        }
    }

    /// 自动弹出不进后退记录：把导航的「上一帧位置」更新成现在
    pub(super) fn nav_absorb(&mut self) {
        self.nav_sync_side();
    }

    /// 并排组合的名字：「代码 + 串口」
    pub(super) fn combo_label(a: Pane, b: Pane) -> String {
        format!("{} + {}", a.label(), b.label())
    }
}

pub(super) fn pair_key(a: Pane, b: Pane) -> String {
    let (x, y) = (a.key(), b.key());
    if x < y {
        format!("{x}|{y}")
    } else {
        format!("{y}|{x}")
    }
}

impl App {
    /// 让某个页面显示出来：已经在主区域或副页面就不动；否则按强度在副页面打开，
    /// 用户主动的动作在太窄放不下时退回整页切换
    pub(super) fn reveal(&mut self, p: Pane, trig: &str, strength: Strength) {
        if self.main_pane() == Some(p) {
            return;
        }
        if let Some(i) = self.work.tabs.iter().position(|x| *x == p) {
            self.work.cur = i;
            return;
        }
        // 设置页里点的：直接整页
        if self.page == Page::Settings {
            self.goto_pane(p);
            return;
        }
        let shown = self.auto_side(p, trig, strength, &p.label());
        if !shown && strength == Strength::Strong {
            self.goto_pane(p);
        }
    }

    /// 代码页发出的事件（不管代码页在主区域还是副页面）
    pub(super) fn code_events(&mut self) {
        for e in std::mem::take(&mut self.code.events) {
            match e {
                CodeEvt::Toast(m, c) => self.toast(m, c),
                CodeEvt::Project(p) => self.project_changed(&p),
                CodeEvt::OpenSettings => {
                    self.prefs.cat = super::prefs::Cat::Editor;
                    self.set_page(Page::Settings);
                }
                CodeEvt::BitCalc(lit, v) => self.bitcalc.show(&lit, v),
                CodeEvt::Struct(src) => self.open_struct(src),
                CodeEvt::Tool(kind, text) => self.code_to_tool(kind, text),
                CodeEvt::Compare(a, la, b, lb) => self.compare_sides(a, la, b, lb),
                CodeEvt::Ask(text, lang) => {
                    self.comm.ask_with(&text, &lang);
                    self.reveal(Pane::Community, "ask", Strength::Strong);
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
                    self.reveal(Pane::Compare, "merge", Strength::Strong);
                    self.compare_events();
                }
            }
        }
    }

    pub(super) fn git_events(&mut self) {
        for e in std::mem::take(&mut self.gitv.events) {
            match e {
                GitEvt::Toast(m, c) => self.toast(m, c),
                GitEvt::Project(p) => {
                    self.code.open_root(p);
                }
                GitEvt::OpenFile(p, line) => {
                    self.code.open_file(&p, Some((line, None)));
                    self.reveal(Pane::Code, "git.open", Strength::Strong);
                }
                GitEvt::Compare(a, la, b, lb) => self.compare_sides(a, la, b, lb),
                GitEvt::Conflict(root, rel) => {
                    self.cmpv.open_conflict(&root, &rel);
                    self.reveal(Pane::Compare, "conflict", Strength::Strong);
                    self.compare_events();
                }
            }
        }
    }

    /// 比较页左边的文件树：有工程时自动带上，点文件依次选为左、右两边
    pub(super) fn compare_tree(&mut self, ctx: &egui::Context) {
        if !self.s.work.cmp_tree || self.code.root().is_none() || ctx.screen_rect().width() < 900.0 {
            return;
        }
        let mut close = false;
        let picked = egui::SidePanel::left("cmp-tree")
            .frame(egui::Frame::new().fill(BG1()))
            .default_width(240.0)
            .width_range(180.0..=420.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    let tip = if self.cmpv.pending_left().is_some() {
                        t("再点一个文件作为右边")
                    } else {
                        t("点文件选为左边，再点一个作为右边")
                    };
                    ui.label(RichText::new(tip).size(11.5).color(TEXT3()));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, t("收起文件树"), false).clicked() {
                            close = true;
                        }
                    });
                });
                self.code.files_ui(ui, true)
            })
            .inner;
        if close {
            self.s.work.cmp_tree = false;
            self.toast(t("在格局菜单里可以重新打开文件树"), ACCENT());
        }
        if !picked.is_empty() {
            let first = self.cmpv.pending_left().is_none();
            self.cmpv.drop_files(picked.clone());
            if first && picked.len() == 1 {
                let name = picked[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.toast(tf("已选为左侧：{}，再点一个文件作为右侧", &[&name]), ACCENT());
            }
        }
    }
}

/// C 代码里的字节：0x12, 0x34… 或 "12 34 AB" 这样的十六进制串；不像就返回 None
pub(super) fn hex_bytes(text: &str) -> Option<String> {
    let toks: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric()))
        .filter_map(|w| {
            let w = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X"))?;
            (!w.is_empty() && w.len() <= 2 && w.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("{:0>2}", w.to_ascii_uppercase()))
        })
        .collect();
    if toks.len() >= 2 {
        return Some(toks.join(" "));
    }
    let bare: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    (bare.len() >= 2 && bare.len() % 2 == 0 && bare.chars().all(|c| c.is_ascii_hexdigit()) && text.contains(' ')).then(|| text.trim().to_string())
}

impl App {
    /// 代码页右键「用工具打开」
    pub(super) fn code_to_tool(&mut self, kind: &str, text: String) {
        use crate::toolbox::conv;
        let hex = hex_bytes(&text);
        match kind {
            "convert" => {
                let num = text.trim();
                let is_num = !num.is_empty() && (num.parse::<i64>().is_ok() || num.strip_prefix("0x").or_else(|| num.strip_prefix("0X")).is_some_and(|h| i64::from_str_radix(h, 16).is_ok()));
                let (k, v) = match hex {
                    Some(h) => (conv::Input::Hex, h),
                    None if is_num => (conv::Input::Number, num.to_string()),
                    None => (conv::Input::Text, text.trim_matches('"').to_string()),
                };
                self.s.tb.conv_in = k;
                self.s.tb.conv_text = v;
                self.reveal(Pane::Tool(Tool::Convert), "code.tool", Strength::Strong);
            }
            _ => {
                let c = &mut self.s.tb.x;
                match hex {
                    Some(h) => {
                        c.crc_hex = true;
                        c.crc_input = h;
                    }
                    None => {
                        c.crc_hex = false;
                        c.crc_input = text.trim_matches('"').to_string();
                    }
                }
                self.reveal(Pane::Tool(Tool::Checksum), "code.tool", Strength::Strong);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_from_c_arrays() {
        assert_eq!(hex_bytes("{0x01, 0x3, 0xAB}").as_deref(), Some("01 03 AB"));
        assert_eq!(hex_bytes("01 03 00 0A").as_deref(), Some("01 03 00 0A"));
        assert_eq!(hex_bytes("hello"), None);
        assert_eq!(hex_bytes("0x1234"), None);
    }

    #[test]
    fn pair_key_is_symmetric() {
        assert_eq!(pair_key(Pane::Code, Pane::Debug), pair_key(Pane::Debug, Pane::Code));
    }
}

impl App {
    /// 电路页发出的事件（电路在主区域或副页面都一样处理）
    pub(super) fn circuit_events(&mut self) {
        let Some(cv) = &mut self.circ else { return };
        let evs = std::mem::take(&mut cv.events);
        for e in evs {
            match e {
                crate::circuitview::CvEvt::Toast(m, c) => self.toast(m, c),
                crate::circuitview::CvEvt::OpenVcd(p) => {
                    self.toast(t("已导出，在「VCD 波形」里打开"), OK());
                    self.tool_drop5(Tool::Vcd, p);
                    self.open_tool(Tool::Vcd);
                }
                crate::circuitview::CvEvt::Back => {
                    // 回到工具箱：停在上一个用过的其它工具
                    let back = self.s.tb.recent.iter().copied().find(|t| *t != Tool::Circuit).unwrap_or(Tool::Tftp);
                    self.s.tb.tool = back;
                }
                crate::circuitview::CvEvt::Powered(ports) => self.on_circuit_power(ports),
                crate::circuitview::CvEvt::GenCode => self.on_gen_code(),
            }
        }
        self.gen_code_sync();
    }

    /// 电路通电、里面有仿真串口：串口空着就自动连上仿真口，并把串口放到旁边
    fn on_circuit_power(&mut self, ports: Vec<String>) {
        let mut connected = false;
        let mut busy = false;
        self.with_slot(0, |app| {
            if app.conn.is_some() {
                busy = !app.s.port.starts_with(crate::circuit::simport::PREFIX);
                connected = !busy;
                return;
            }
            app.s.transport = Transport::Serial;
            app.s.port = ports[0].clone();
            app.connect();
            connected = app.conn.is_some();
        });
        if busy {
            // 真实串口正在用：不抢，只提示
            self.side_hint(Pane::Debug, "circuit.power", &tf("仿真串口 {} 可以在调试页打开", &[&ports[0]]), None);
            return;
        }
        if connected && self.main_pane() == Some(Pane::Tool(Tool::Circuit)) {
            self.work.sim_opened = false;
            self.auto_side(Pane::Debug, "circuit.power", Strength::Medium, t("通电了 · 在旁边看仿真串口"));
        }
    }

    /// 调试页连上了仿真口：把电路放到旁边（能点按键、调电位器）
    pub(super) fn sim_opened_tick(&mut self) {
        if !std::mem::take(&mut self.work.sim_opened) {
            return;
        }
        if self.main_pane() == Some(Pane::Debug) && self.circ.is_some() {
            self.auto_side(Pane::Tool(Tool::Circuit), "sim.open", Strength::Medium, t("连上了仿真串口 · 在旁边看电路"));
        }
    }

    /// 「生成代码」：写到文件、在代码页旁边打开；改了引脚规则自动更新
    fn on_gen_code(&mut self) {
        let Some((name, ext, code)) = self.circ.as_ref().and_then(|c| c.gen_code()) else { return };
        let dir = crate::data_dir().join("circuit").join("gen");
        let _ = std::fs::create_dir_all(&dir);
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let f = dir.join(format!("{safe}.{ext}"));
        if let Err(e) = std::fs::write(&f, &code) {
            self.toast(e.to_string(), ERR());
            return;
        }
        if self.fit_pos(None).is_none() {
            return; // 太窄：留在电路底部的「代码」标签里看
        }
        self.code.open_file(&f, None);
        self.work.gen_file = Some(f);
        self.reveal(Pane::Code, "circuit.gen", Strength::Strong);
        if let Some(cv) = &mut self.circ {
            cv.code_in_side();
        }
        self.toast(t("生成的代码在旁边打开；改了引脚和规则会自动更新"), ACCENT());
    }

    /// 生成的代码跟着电路实时更新（半秒查一次）
    fn gen_code_sync(&mut self) {
        let Some(f) = self.work.gen_file.clone() else { return };
        if self.work.gen_at.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
            return;
        }
        self.work.gen_at = Some(Instant::now());
        if !self.code.open_files().contains(&f) {
            self.work.gen_file = None;
            return;
        }
        let Some((_, _, code)) = self.circ.as_ref().and_then(|c| c.gen_code()) else { return };
        if std::fs::read_to_string(&f).ok().as_deref() != Some(code.as_str()) {
            let _ = std::fs::write(&f, code);
        }
    }
}

const SRC_EXT: &[&str] = &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "s", "asm", "rs", "py", "go", "java", "js", "ts", "ino", "inc", "ld"];

/// 日志里的源码位置：`main.c:42`、`src/drv/uart.c:120:7`、`app.rs:10` → (起, 止, 文件, 行号)
pub(super) fn src_ref(text: &str) -> Option<(usize, usize, String, usize)> {
    let b = text.as_bytes();
    let path_ch = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'/' | b'\\' | b'-');
    let mut i = 0;
    while let Some(k) = text[i..].find(':') {
        let colon = i + k;
        i = colon + 1;
        let digits = b[colon + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > 7 {
            continue;
        }
        let mut s = colon;
        while s > 0 && path_ch(b[s - 1]) {
            s -= 1;
        }
        // Windows 盘符 C:\…
        if s >= 2 && b[s - 1] == b':' && b[s - 2].is_ascii_alphabetic() && (s < 3 || !path_ch(b[s - 3])) {
            s -= 2;
        }
        let path = &text[s..colon];
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !path.contains('.') || !SRC_EXT.contains(&ext.as_str()) || path.starts_with('.') && path.len() < 3 {
            continue;
        }
        let line: usize = text[colon + 1..colon + 1 + digits].parse().ok()?;
        if line == 0 {
            continue;
        }
        return Some((s, colon + 1 + digits, path.to_string(), line));
    }
    None
}

/// 日志里的异常：HardFault、assert、看门狗复位…
pub(super) fn is_fault(text: &str) -> bool {
    const KEYS: &[&str] = &[
        "hardfault",
        "hard fault",
        "assert",
        "panic",
        "guru meditation",
        "abort()",
        "busfault",
        "usagefault",
        "memmanage",
        "stack overflow",
        "watchdog",
        "wdt reset",
        "rst:0x",
    ];
    let l = text.to_ascii_lowercase();
    KEYS.iter().any(|k| l.contains(k))
}

impl App {
    /// 打开日志里的 `文件:行号`：在副页面（或整页）打开代码并跳到那一行
    pub(super) fn open_src_ref(&mut self, f: &str, line: usize) {
        match self.code.find_src(f) {
            Some(p) => {
                self.code.goto_loc(&p, line.saturating_sub(1));
                self.reveal(Pane::Code, "log.src", Strength::Strong);
            }
            None => self.toast(tf("工程里找不到 {}", &[&f]), WARN()),
        }
    }

    /// 代码和串口都在显示时，代码里选中的文字在日志里高亮
    pub(super) fn link_tick(&mut self) {
        let main = self.main_pane();
        let side = self.side_pane();
        let shown = |p: Pane| main == Some(p) || side == Some(p);
        self.work.link = if shown(Pane::Code) && shown(Pane::Debug) {
            self.code.link_term().unwrap_or_default()
        } else {
            String::new()
        };
    }

    /// 看新来的日志：HardFault、`文件:行号` → 状态栏提示（只提示，不自动弹）
    pub(super) fn scan_log_tick(&mut self) {
        if self.cur_slot() != 0 {
            return;
        }
        let total = self.buf.total();
        let from = self.work.scan_upto.max(self.buf.dropped).max(total.saturating_sub(200));
        let upto = if total > 0 && self.buf.is_open(total - 1) { total - 1 } else { total };
        if from >= upto {
            self.work.scan_upto = self.work.scan_upto.max(from);
            return;
        }
        let main = self.main_pane();
        let side = self.side_pane();
        let shown = |p: Pane| main == Some(p) || side == Some(p);
        let has_proj = self.code.root().is_some();
        let mut fault = None;
        let mut src = None;
        for id in from..upto {
            let Some(l) = self.buf.get(id) else { continue };
            if l.dir != crate::logbuf::Dir::Rx {
                continue;
            }
            if fault.is_none() && is_fault(&l.plain) {
                fault = Some(l.plain.clone());
            }
            if src.is_none() && has_proj {
                src = src_ref(&l.plain).map(|(_, _, f, n)| (f, n));
            }
        }
        self.work.scan_upto = upto;
        // 在代码页、串口没在显示：来了 HardFault 就提示把串口放旁边
        if fault.is_some() && main == Some(Pane::Code) && !shown(Pane::Debug) {
            self.side_hint(Pane::Debug, "log.fault", t("日志里有异常（HardFault / assert）· 在代码旁看"), None);
        }
        // 在调试页、代码没在显示：日志里有 文件:行号 → 提示在旁边看代码
        if let Some((f, n)) = src {
            if main == Some(Pane::Debug) && !shown(Pane::Code) {
                if let Some(p) = self.code.find_src(&f) {
                    let name = p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or(f);
                    self.side_hint(Pane::Code, "log.src", &tf("日志里有 {}:{} · 在旁边看代码", &[&name, &n]), Some((p, n.saturating_sub(1))));
                }
            }
        }
    }

    /// 点了「烧录」任务：串口连着、正在代码页 → 把串口放到旁边看启动日志
    pub(super) fn flash_side(&mut self) {
        let mut live = false;
        self.with_slot(0, |app| live = app.conn.is_some() || app.reconnect.is_some());
        if live && self.main_pane() == Some(Pane::Code) {
            self.auto_side(Pane::Debug, "flash", Strength::Medium, t("烧录了 · 在旁边看串口"));
        }
    }
}

#[cfg(test)]
mod src_tests {
    use super::*;

    #[test]
    fn finds_source_refs() {
        let r = src_ref("assert failed: src/drv/uart.c:120 (len > 0)").unwrap();
        assert_eq!((r.2.as_str(), r.3), ("src/drv/uart.c", 120));
        assert_eq!(src_ref("E (123) main.c:42: boom").map(|r| r.3), Some(42));
        assert_eq!(src_ref("time 12:30:45"), None);
        assert_eq!(src_ref("ip 192.168.1.1:8080"), None);
        assert_eq!(src_ref(r"C:\fw\main.c:7 oops").map(|r| r.2), Some(r"C:\fw\main.c".to_string()));
        assert!(is_fault("[E] HardFault_Handler"));
    }
}

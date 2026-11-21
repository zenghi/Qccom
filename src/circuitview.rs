//! 「电路和接线」页：左边元件库，中间画布（面包板实物视图 / 原理图视图），右边属性和知识卡，底部输出。
//! 电路数据和仿真在 crate::circuit，这里只管界面和交互。

mod canvas;
mod assistant;
pub mod draw;
mod flow;
mod mcuedit;
mod netlink;
mod panels;
mod power;
mod routing;
mod scope;
mod wiring;
mod workbench;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Modifiers, RichText, Sense};

use crate::app::{draw_icon, icon_button, Icon};
use crate::circuit::lib::{self, Cat};
use crate::circuit::sim::Sim;
use crate::circuit::{Circuit, Pt};
use crate::i18n::t;
use crate::theme::*;

use draw::View;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Sel {
    Part(u32),
    Wire(u32),
    Note(u32),
}

/// 页面发给 App 的事件
pub enum CvEvt {
    Toast(String, Color32),
    /// 回工具箱
    Back,
    /// 导出了 VCD，在「VCD 波形」工具里打开
    OpenVcd(PathBuf),
    /// 通电了，电路里有仿真串口（端口名）
    Powered(Vec<String>),
    /// 点了「生成代码」
    GenCode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BTab {
    Log,
    Scope,
    Logic,
    Code,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RTab {
    Props,
    Know,
    Checks,
    Nets,
    Assistant,
    /// 打开的示例：原理、步骤、试一试
    Example,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tool {
    #[default]
    Select,
    Wire,
    Probe,
}

/// 鼠标拖动的状态
enum Drag {
    None,
    Pan,
    /// 移动选中的东西：按下时的世界坐标、各对象原位置
    Move {
        start: (f32, f32),
        original: Box<Circuit>,
        parts: HashSet<u32>,
        wires: HashSet<u32>,
        notes: HashSet<u32>,
        delta: Pt,
    },
    /// 拖导线的一个端点（或中间拐点）
    WirePt {
        id: u32,
        k: usize,
        original: Vec<Pt>,
        moved: bool,
    },
    /// 拉长两脚元件（拖第二脚）
    Stretch {
        id: u32,
        moved: bool,
    },
    Box {
        start: egui::Pos2,
    },
    /// 仿真时按住按钮
    Press(u32),
}

/// 正在画的导线
struct Draft {
    start: routing::Contact,
    pts: Vec<Pt>,
    marks: Vec<usize>,
    /// 按住拖出来的（松手就结束），否则是一下一下点的
    dragging: bool,
}

const SPEEDS: [(f64, &str); 4] = [(1.0, "实时"), (0.1, "1/10"), (0.01, "1/100"), (0.001, "1/1000")];

pub struct CircuitView {
    pub c: Circuit,
    pub sim: Option<Sim>,
    paused: bool,
    speed: f64,
    view: View,
    schematic: bool,
    tool: Tool,
    vertical_first: bool,
    library_open: bool,
    compact_library: bool,
    power_dialog: bool,
    inspector_open: bool,
    canvas_rect: Option<egui::Rect>,
    focus_points: Vec<Pt>,
    focused_issue: Option<usize>,
    issue_filter: Option<crate::circuit::check::Level>,
    net_origin: Option<routing::Contact>,
    netlist: crate::circuit::net::Netlist,
    junctions: Vec<Pt>,
    assistant: assistant::Assistant,
    practice: Option<Practice>,
    save_source: Option<Circuit>,
    sel: Vec<Sel>,
    drag: Drag,
    draft: Option<Draft>,
    /// 正在放置的元件（库里点了一下 / 拖出来）：种类、是否拖出来的
    placing: Option<(&'static str, bool)>,
    /// 放置时的旋转
    place_rot: u8,
    undo: Vec<String>,
    redo: Vec<String>,
    clip: Option<String>,
    lib_filter: String,
    lib_open: HashSet<Cat>,
    /// 左栏：false 元件库，true 示例
    lib_examples: bool,
    ex_open: HashSet<char>,
    /// 当前打开的示例编号
    example: Option<String>,
    tab: RTab,
    bottom: bool,
    wire_color: u8,
    path: Option<PathBuf>,
    dirty_at: Option<Instant>,
    last_frame: Option<Instant>,
    /// 首帧把视图对准电路
    fit: bool,
    auto_frame: bool,
    pub events: Vec<CvEvt>,
    /// 新日志条数（底部面板关着时在按钮上显示）
    seen_log: usize,
    /// 通电时显示电流小点
    show_flow: bool,
    btab: BTab,
    scope: scope::ScopeUi,
    power: power::PowerStats,
    net: netlink::NetLink,
    meter: power::Meter,
    /// 接错检查的结果（电路改了就重算）
    issues: Vec<crate::circuit::check::Issue>,
    need_check: bool,
    /// 生成代码：哪块板子、哪种写法
    code_board: Option<u32>,
    code_dialect: crate::circuit::gen::Dialect,
    /// 连续编辑合并成一个撤销点
    ck_at: Option<Instant>,
    /// 工程文件夹（接线记录存在工程里）
    pub project: Option<PathBuf>,
    /// 接线记录模式
    wiring_on: bool,
    wiring: wiring::WiringUi,
    /// 画在副页面里（精简版）
    compact: bool,
}

struct Practice {
    previous: Circuit,
    path: Option<PathBuf>,
    saved: Option<Circuit>,
    example: Option<String>,
    view: View,
    canvas_rect: Option<egui::Rect>,
    schematic: bool,
    dirty_at: Option<Instant>,
    auto_frame: bool,
    inspector_open: bool,
    tab: RTab,
    bottom: bool,
    btab: BTab,
    scope: scope::ScopeUi,
    selection: Vec<Sel>,
    sim: Option<Sim>,
    paused: bool,
    undo: Vec<String>,
    redo: Vec<String>,
    target: ((u32, usize), (u32, usize)),
}

fn autosave_path() -> PathBuf {
    crate::data_dir().join("circuit").join("current.json")
}

impl Default for CircuitView {
    fn default() -> Self {
        Self::new()
    }
}

impl CircuitView {
    pub fn new() -> Self {
        let c = std::fs::read_to_string(autosave_path()).ok().and_then(|s| Circuit::from_json(&s).ok()).unwrap_or_else(starter);
        CircuitView {
            c,
            sim: None,
            paused: false,
            speed: 1.0,
            view: View { org: pos2(0.0, 0.0), s: 14.0 },
            schematic: false,
            tool: Tool::Select,
            vertical_first: false,
            library_open: true,
            compact_library: false,
            power_dialog: false,
            inspector_open: false,
            canvas_rect: None,
            focus_points: vec![],
            focused_issue: None,
            issue_filter: None,
            net_origin: None,
            netlist: Default::default(),
            junctions: vec![],
            assistant: Default::default(),
            practice: None,
            save_source: None,
            sel: vec![],
            drag: Drag::None,
            draft: None,
            placing: None,
            place_rot: 0,
            undo: vec![],
            redo: vec![],
            clip: None,
            lib_filter: String::new(),
            lib_open: [Cat::Board, Cat::Power, Cat::Passive, Cat::Diode].into_iter().collect(),
            lib_examples: false,
            ex_open: ['A'].into_iter().collect(),
            example: None,
            tab: RTab::Props,
            bottom: false,
            wire_color: 0,
            path: None,
            dirty_at: None,
            last_frame: None,
            fit: true,
            auto_frame: true,
            events: vec![],
            seen_log: 0,
            show_flow: true,
            btab: BTab::Scope,
            scope: Default::default(),
            power: Default::default(),
            net: Default::default(),
            meter: Default::default(),
            issues: vec![],
            need_check: true,
            code_board: None,
            code_dialect: crate::circuit::gen::Dialect::Arduino,
            ck_at: None,
            project: None,
            wiring_on: false,
            wiring: Default::default(),
            compact: false,
        }
    }

    pub fn powered(&self) -> bool {
        self.sim.is_some()
    }

    /// 拖进来的文件：接线记录模式下图片当实物照片，.qccirc 当电路打开
    pub fn drop_file(&mut self, p: &std::path::Path) {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ["jpg", "jpeg", "png", "bmp", "webp"].contains(&ext.as_str()) {
            if !self.wiring_on {
                self.wiring_on = true;
                self.wiring.set_dir(wiring::dir_for(self.project.as_deref()));
            }
            self.wiring.add_photo(p);
        } else if ext == "qccirc" || ext == "json" {
            match std::fs::read_to_string(p).ok().and_then(|s| Circuit::from_json(&s).ok()) {
                Some(c) => self.load(c, Some(p.to_path_buf())),
                None => self.toast(t("不是电路文件"), ERR()),
            }
        }
    }

    fn toast(&mut self, m: impl Into<String>, c: Color32) {
        self.events.push(CvEvt::Toast(m.into(), c));
    }

    /// 改动之前调用：记撤销点、标记要保存、通知仿真重建
    fn checkpoint(&mut self) {
        self.undo.push(self.c.to_json());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.changed();
    }

    /// 连续的小改动（拖数值、打字）只记一个撤销点
    fn checkpoint_soft(&mut self) {
        if self.ck_at.is_none_or(|t| t.elapsed() > Duration::from_millis(1200)) {
            self.checkpoint();
        } else {
            self.changed();
        }
        self.ck_at = Some(Instant::now());
    }

    /// 已经改了（拖动过程中多次调用）
    fn changed(&mut self) {
        self.dirty_at = Some(Instant::now());
        self.need_check = true;
        if let Some(s) = &mut self.sim {
            s.rebuild = true;
        }
    }

    fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.c.to_json());
            self.restore(&s);
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.c.to_json());
            self.restore(&s);
        }
    }

    fn restore(&mut self, s: &str) {
        if let Ok(c) = Circuit::from_json(s) {
            self.c = c;
            self.sel.retain(|x| match x {
                Sel::Part(id) => self.c.part(*id).is_some(),
                Sel::Wire(id) => self.c.wires.iter().any(|w| w.id == *id),
                Sel::Note(id) => self.c.notes.iter().any(|n| n.id == *id),
            });
            self.changed();
        }
    }

    /// 打开示例：搭好的电路 + 右边的说明
    pub fn load_example(&mut self, id: &str) {
        let Some(e) = crate::circuit::examples::find(id) else { return };
        match e.build() {
            Ok(c) => {
                self.load(c, None);
                self.example = Some(id.to_string());
                self.tab = RTab::Example;
                self.set_power(false);
                if e.power {
                    self.set_power(true);
                }
            }
            Err(err) => self.toast(err, ERR()),
        }
    }

    /// 换一个电路（新建、打开、示例）
    pub fn load(&mut self, c: Circuit, path: Option<PathBuf>) {
        if self.practice.is_some() {
            self.end_practice();
        }
        self.save_source = path.as_ref().map(|_| c.clone());
        self.undo.clear();
        self.redo.clear();
        self.c = c;
        self.path = path;
        self.example = None;
        self.reset_canvas_state();
        self.dirty_at = Some(Instant::now());
    }

    fn reset_canvas_state(&mut self) {
        self.sel.clear();
        self.draft = None;
        self.placing = None;
        self.tool = Tool::Select;
        self.scope = Default::default();
        self.set_power(false);
        self.drag = Drag::None;
        self.fit = true;
        self.focus_points.clear();
        self.focused_issue = None;
        self.net_origin = None;
        self.need_check = true;
        self.issues.clear();
        self.netlist = Default::default();
        self.junctions.clear();
        self.power_dialog = false;
        self.assistant.reset_circuit();
    }

    fn has_unsaved_file_edits(&self) -> bool {
        self.save_source.as_ref().is_some_and(|saved| *saved != self.c)
    }

    fn autosave_draft(&mut self) {
        if self.practice.is_none() {
            let _ = crate::history::save_file(&autosave_path(), self.c.to_json().as_bytes());
        }
        self.dirty_at = None;
    }

    fn save_to(&mut self, path: PathBuf) {
        if let Err(e) = crate::history::save_file(&path, self.c.to_json().as_bytes()) {
            self.toast(format!("{}：{e}", t("保存失败")), ERR());
            return;
        }
        self.path = Some(path);
        self.save_source = Some(self.c.clone());
        self.autosave_draft();
        self.toast(t("已保存"), OK());
    }

    fn restore_saved(&mut self) {
        if let Some(saved) = self.save_source.clone() {
            self.checkpoint();
            self.restore(&saved.to_json());
        }
    }

    fn open_file(&mut self) {
        let Some(p) = crate::diagnostics::file_dialog().set_title(t("打开电路")).add_filter(t("QCCOM 电路"), &["qccirc", "json"]).pick_file() else {
            return;
        };
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Circuit::from_json(&s)) {
            Ok(c) => self.load(c, Some(p)),
            Err(e) => self.toast(format!("{}：{e}", t("打不开")), ERR()),
        }
    }

    fn save_as(&mut self) {
        let name = if self.c.title.is_empty() { "circuit".to_string() } else { self.c.title.clone() };
        if let Some(p) = crate::diagnostics::file_dialog()
            .set_title(t("电路另存为"))
            .add_filter(t("QCCOM 电路"), &["qccirc"])
            .set_file_name(format!("{name}.qccirc"))
            .save_file()
        {
            self.save_to(p);
        }
    }

    /// 显示底部的示波器（「仿真」布局方案）
    pub fn show_scope(&mut self) {
        self.bottom = true;
        self.btab = BTab::Scope;
    }

    /// 生成的代码：(板子名, 扩展名, 代码)。没点过「生成代码」时为 None
    pub fn gen_code(&self) -> Option<(String, &'static str, String)> {
        use crate::circuit::gen;
        let id = self.code_board?;
        let p = self.c.part(id)?;
        let ds = gen::dialects(&p.kind);
        let d = if ds.contains(&self.code_dialect) { self.code_dialect } else { *ds.first()? };
        Some((self.c.label(id), d.ext(), gen::generate(&self.c, id, d)))
    }

    /// 生成的代码改在副页面看了：底部不再显示「代码」标签
    pub fn code_in_side(&mut self) {
        if self.btab == BTab::Code {
            self.btab = BTab::Log;
            self.bottom = false;
        }
    }

    /// 电路里的仿真串口名
    fn sim_port_names(&self) -> Vec<String> {
        use crate::circuit::simport;
        self.c
            .parts
            .iter()
            .filter(|p| p.kind == "usbttl" || lib::def(&p.kind).is_some_and(|d| d.is_board()) && p.prop("power") == "usb")
            .map(|p| format!("{}{}", simport::PREFIX, self.c.label(p.id)))
            .collect()
    }

    fn set_power(&mut self, on: bool) {
        if on {
            let mut s = Sim::new(&self.c);
            s.real_speed = self.speed;
            self.scope.hold = None;
            self.scope.la_hold = None;
            self.sim = Some(s);
            self.paused = false;
            self.seen_log = 0;
            self.last_frame = None;
            self.power.reset();
            let ports = self.sim_port_names();
            if !ports.is_empty() {
                self.bridge();
                self.events.push(CvEvt::Powered(ports));
            }
        } else {
            self.sim = None;
        }
    }

    /// 仿真串口：USB 转 TTL、USB 供电的开发板登记到串口列表；通电时双向转发数据
    fn bridge(&mut self) {
        use crate::circuit::simport;
        let list: Vec<(String, String, u32)> = self
            .c
            .parts
            .iter()
            .filter(|p| p.kind == "usbttl" || crate::circuit::atdev::is_bt(&p.kind) || lib::def(&p.kind).is_some_and(|d| d.is_board()) && p.prop("power") == "usb")
            .map(|p| {
                let name = t(lib::def(&p.kind).unwrap().zh);
                // 蓝牙模块：打开这个串口就相当于手机连上了它
                let desc = if crate::circuit::atdev::is_bt(&p.kind) {
                    format!("{} · {} · {name}", t("仿真"), t("手机蓝牙"))
                } else {
                    format!("{} · {name}", t("仿真"))
                };
                (format!("{}{}", simport::PREFIX, self.c.label(p.id)), desc, p.id)
            })
            .collect();
        simport::sync(&list);
        let Some(s) = &mut self.sim else { return };
        for (_, _, id) in &list {
            match simport::baud_of(*id) {
                Some(b) => {
                    s.host_baud.insert(*id, b);
                }
                None => {
                    s.host_baud.remove(id);
                }
            }
            if let Some(d) = s.host_rx.remove(id) {
                simport::to_host(*id, &d);
            }
            let d = simport::from_host(*id);
            if !d.is_empty() {
                s.host_send(&self.c, *id, &d);
            }
        }
        s.host_rx.clear();
    }

    /// 每帧推进仿真（不管当前在哪个页面，App 每帧都调用，切到调试页看串口时仿真照样在跑）
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.bridge();
        let now = Instant::now();
        let wall = self.last_frame.map_or(0.0, |t| now.duration_since(t).as_secs_f64()).min(0.1);
        self.last_frame = Some(now);
        let Some(s) = &mut self.sim else {
            self.net.close_all();
            return;
        };
        if !self.paused {
            s.advance(&self.c, wall, self.speed);
            self.power.update(&self.c, s);
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        // 出事了（烧坏、保险丝断）自动打开底部输出
        if s.log.len() > self.seen_log && !self.bottom && s.log[self.seen_log..].iter().any(|l| l.2.starts_with('🔥') || l.2.contains(t("烧坏"))) {
            self.bottom = true;
            self.btab = BTab::Log;
        }
        // 联网：模块的 AT 指令、开发板规则里的「发 MQTT / HTTP / TCP」在后台真的去连
        for (id, c) in std::mem::take(&mut s.at_net) {
            self.net.cmd(id, c, ctx);
        }
        for r in std::mem::take(&mut s.net_out) {
            self.net.oneshot(0, r, ctx);
        }
        for o in self.net.drain() {
            match o {
                netlink::Out::Evt(id, e) => s.at_event(id, e),
                netlink::Out::Log(id, m) => s.log.push((s.t, id, m)),
            }
        }
        s.photos.clear();
    }

    /// compact = 副页面：不要元件库和属性栏，只看画布（还能点按键、调电位器）
    pub fn ui_in(&mut self, ui: &mut egui::Ui, compact: bool) {
        self.compact = compact;
        self.refresh_diagnostics();
        self.workspace(ui);
        if let Some(dirty) = self.dirty_at {
            if dirty.elapsed() > Duration::from_millis(800) && matches!(self.drag, Drag::None) {
                self.autosave_draft();
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(250));
            }
        }
    }

    fn more_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(200.0);
        if crate::app::menu_item(ui, t("新建电路"), "") {
            self.load(Circuit::default(), None);
        }
        if crate::app::menu_item(ui, t("打开电路…"), "") {
            self.open_file();
        }
        if crate::app::menu_item(ui, t("保存"), "Ctrl+S") {
            if let Some(path) = self.path.clone() { self.save_to(path); } else { self.save_as(); }
        }
        if crate::app::menu_item(ui, t("另存为…"), "") {
            self.save_as();
        }
        if self.has_unsaved_file_edits() && crate::app::menu_item(ui, t("恢复上次保存"), "") {
            self.restore_saved();
        }
        ui.separator();
        if crate::app::menu_item(ui, t("接亮第一个 LED"), "") {
            self.start_practice(ui.ctx().screen_rect().width());
        }
        if crate::app::menu_item(ui, if self.wiring_on { t("返回电路") } else { t("接线记录") }, "") {
            self.toggle_wiring();
        }
        if self.wiring_on { return; }
        ui.separator();
        if crate::app::menu_item(ui, t("撤销"), "Ctrl+Z") { self.undo(); }
        if crate::app::menu_item(ui, t("重做"), "Ctrl+Y") { self.redo(); }
        if crate::app::menu_item(ui, if self.show_flow { t("隐藏电流动画") } else { t("显示电流动画") }, "") {
            self.show_flow = !self.show_flow;
        }
        if crate::app::menu_item(ui, t("全选"), "Ctrl+A") {
            self.select_all();
        }
        if crate::app::menu_item(ui, t("清空画布"), "") {
            self.checkpoint();
            self.c.parts.clear();
            self.c.wires.clear();
            self.c.notes.clear();
            self.sel.clear();
        }
    }

    fn select_all(&mut self) {
        self.sel = self
            .c
            .parts
            .iter()
            .map(|p| Sel::Part(p.id))
            .chain(self.c.wires.iter().map(|w| Sel::Wire(w.id)))
            .chain(self.c.notes.iter().map(|n| Sel::Note(n.id)))
            .collect();
    }

    fn delete_sel(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint();
        let sel = std::mem::take(&mut self.sel);
        self.c.parts.retain(|p| !sel.contains(&Sel::Part(p.id)));
        self.c.wires.retain(|w| !sel.contains(&Sel::Wire(w.id)));
        self.c.notes.retain(|n| !sel.contains(&Sel::Note(n.id)));
    }

    fn rotate_sel(&mut self, flip: bool) {
        let ids: Vec<u32> = self.sel.iter().filter_map(|s| if let Sel::Part(id) = s { Some(*id) } else { None }).collect();
        if ids.is_empty() {
            return;
        }
        let original = self.c.clone();
        self.checkpoint();
        for id in ids {
            if let Some(p) = self.c.part_mut(id) {
                if flip {
                    p.flip = !p.flip;
                } else {
                    p.rot = (p.rot + 1) % 4;
                }
            }
        }
        routing::carry_wires(&original, &mut self.c, &HashSet::new(), (0, 0), self.schematic);
        self.paused = self.powered();
    }

    /// 复制选中的元件和导线（JSON 形式的小电路）
    fn copy_sel(&mut self) -> bool {
        let mut sub = Circuit::default();
        for s in &self.sel {
            match s {
                Sel::Part(id) => sub.parts.extend(self.c.part(*id).cloned()),
                Sel::Wire(id) => sub.wires.extend(self.c.wires.iter().find(|w| w.id == *id).cloned()),
                Sel::Note(id) => sub.notes.extend(self.c.notes.iter().find(|n| n.id == *id).cloned()),
            }
        }
        if sub.parts.is_empty() && sub.wires.is_empty() && sub.notes.is_empty() {
            return false;
        }
        self.clip = Some(sub.to_json());
        true
    }

    /// 粘贴：整体偏移 (dx, dy)，新对象成为选中
    fn paste(&mut self, dx: i32, dy: i32) {
        let Some(sub) = self.clip.as_ref().and_then(|s| Circuit::from_json(s).ok()) else { return };
        self.checkpoint();
        self.sel.clear();
        for mut p in sub.parts {
            p.id = self.c.alloc_id();
            p.x += dx;
            p.y += dy;
            p.label.clear();
            self.sel.push(Sel::Part(p.id));
            self.c.parts.push(p);
        }
        for mut w in sub.wires {
            w.id = self.c.alloc_id();
            for q in &mut w.pts {
                q.0 += dx;
                q.1 += dy;
            }
            self.sel.push(Sel::Wire(w.id));
            self.c.wires.push(w);
        }
        for mut n in sub.notes {
            n.id = self.c.alloc_id();
            n.x += dx;
            n.y += dy;
            self.sel.push(Sel::Note(n.id));
            self.c.notes.push(n);
        }
    }

    /// 画布有焦点时的快捷键
    fn keys(&mut self, ui: &egui::Ui) {
        if ui.ctx().wants_keyboard_input() {
            return;
        }
        let i = ui.input(|i| i.clone());
        let cmd = i.modifiers.command;
        if i.key_pressed(Key::Escape) {
            if self.draft.is_some() || self.placing.is_some() || self.tool != Tool::Select {
                self.choose_tool(Tool::Select);
            } else {
                self.sel.clear();
                self.net_origin = None;
                self.focus_points.clear();
            }
        }
        if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
            if let Some(d) = &mut self.draft {
                if let Some(mark) = d.marks.pop() {
                    d.pts.truncate(mark);
                } else {
                    self.draft = None;
                }
            } else {
                self.delete_sel();
            }
        }
        if i.key_pressed(Key::F5) {
            self.toggle_power();
        }
        if !cmd && i.modifiers == Modifiers::NONE {
            for (key, tool) in [(Key::V, Tool::Select), (Key::W, Tool::Wire), (Key::P, Tool::Probe)] {
                if i.key_pressed(key) { self.choose_tool(tool); }
            }
        }
        if i.key_pressed(Key::Home) {
            self.fit = true;
        }
        if cmd && i.key_pressed(Key::Z) {
            if i.modifiers.shift {
                self.redo();
            } else {
                self.undo();
            }
        } else if cmd && i.key_pressed(Key::Y) {
            self.redo();
        } else if cmd && i.key_pressed(Key::A) {
            self.select_all();
        } else if cmd && i.key_pressed(Key::S) {
            if let Some(path) = self.path.clone() {
                self.save_to(path);
            } else {
                self.save_as();
            }
        } else if cmd && i.key_pressed(Key::D) {
            if self.copy_sel() {
                self.paste(2, 2);
            }
        } else if !cmd && i.key_pressed(Key::R) {
            if self.draft.is_some() {
                self.vertical_first = !self.vertical_first;
            } else if self.placing.is_some() {
                self.place_rot = (self.place_rot + 1) % 4;
            } else {
                self.rotate_sel(false);
            }
        } else if !cmd && i.key_pressed(Key::X) && !i.modifiers.shift {
            self.rotate_sel(true);
        }
        for e in &i.events {
            match e {
                egui::Event::Copy => {
                    self.copy_sel();
                }
                egui::Event::Cut => {
                    if self.copy_sel() {
                        self.delete_sel();
                    }
                }
                egui::Event::Paste(_) => self.paste(2, 2),
                _ => {}
            }
        }
        // 数字键换导线颜色：选中的导线和接下来要画的线
        if !cmd && i.modifiers == Modifiers::NONE {
            let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
            if let Some(k) = keys.iter().position(|k| i.key_pressed(*k)) {
                self.set_wire_color(k as u8);
            }
        }
    }

    fn set_wire_color(&mut self, k: u8) {
        self.wire_color = k;
        let ids: Vec<u32> = self.sel.iter().filter_map(|s| if let Sel::Wire(id) = s { Some(*id) } else { None }).collect();
        if !ids.is_empty() {
            self.checkpoint();
            for w in self.c.wires.iter_mut().filter(|w| ids.contains(&w.id)) {
                w.color = k;
            }
        }
    }

    fn bottom_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let tabs: Vec<(BTab, &str)> = [(BTab::Log, t("输出")), (BTab::Scope, t("示波器")), (BTab::Logic, t("逻辑分析仪"))]
                .into_iter()
                .chain(self.code_board.map(|_| (BTab::Code, t("代码"))))
                .collect();
            for (tab, name) in tabs {
                let on = self.btab == tab;
                let r = ui.add(egui::Label::new(RichText::new(name).size(12.5).strong().color(if on { TEXT() } else { TEXT3() })).sense(Sense::click()));
                if on {
                    let y = r.rect.bottom() + 2.0;
                    ui.painter().line_segment([pos2(r.rect.left(), y), pos2(r.rect.right(), y)], egui::Stroke::new(2.0, ACCENT()));
                }
                if r.clicked() {
                    self.btab = tab;
                }
                ui.add_space(10.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Close, t("关闭"), false).clicked() {
                    self.bottom = false;
                }
                if self.btab == BTab::Log && icon_button(ui, Icon::Trash, t("清空"), false).clicked() {
                    if let Some(s) = &mut self.sim {
                        s.log.clear();
                    }
                    self.seen_log = 0;
                }
            });
        });
        ui.add_space(4.0);
        if self.btab == BTab::Code {
            self.code_ui(ui);
            return;
        }
        if self.btab == BTab::Scope {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.scope.current, false, t("电压"));
                ui.selectable_value(&mut self.scope.current, true, t("电流"));
                ui.separator();
                for (kind, label) in [("probe", t("＋ 电压探针")), ("ammeter", t("＋ 电流表"))] {
                    if ui.selectable_label(self.placing.is_some_and(|p| p.0 == kind), label).clicked() {
                        self.placing = Some((kind, false));
                        self.place_rot = 0;
                        self.draft = None;
                        self.scope.current = kind == "ammeter";
                    }
                }
            });
        }
        if self.btab != BTab::Log {
            let Some(s) = &self.sim else {
                if self.btab == BTab::Scope {
                    scope::idle_scope(ui);
                } else {
                    ui.label(RichText::new(t("通电后这里显示波形。")).color(TEXT3()));
                }
                return;
            };
            if self.btab == BTab::Scope {
                scope::scope_ui(ui, &mut self.scope, &self.c, s);
            } else if let Some(p) = scope::logic_ui(ui, &mut self.scope, &self.c, s) {
                self.events.push(CvEvt::OpenVcd(p));
            }
            return;
        }
        let Some(s) = &self.sim else {
            ui.label(RichText::new(t("通电后，元件烧坏、单片机打印、保险丝熔断等事件显示在这里。")).color(TEXT3()));
            return;
        };
        self.seen_log = s.log.len();
        let rows: Vec<_> = s.log.iter().map(|(tt, id, m)| (*tt, *id, self.c.label(*id), m.clone())).collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if rows.is_empty() {
                ui.label(RichText::new(t("暂时没有事件")).color(TEXT3()));
            }
            for (tt, id, who, m) in rows {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fmt_time(tt)).font(FontId::monospace(11.5)).color(TEXT3()));
                    if !who.is_empty() && ui.link(RichText::new(who).font(FontId::monospace(11.5)).color(ACCENT())).clicked() {
                        if let Some(part) = self.c.part(id) {
                            let points = part.pins();
                            self.sel = vec![Sel::Part(id)];
                            self.center_points(&points);
                        }
                    }
                    let bad = m.contains(t("烧坏")) || m.contains(t("熔断"));
                    ui.label(RichText::new(m).size(12.0).color(if bad { ERR() } else { TEXT() }));
                });
            }
        });
    }
}

impl CircuitView {
    fn code_ui(&mut self, ui: &mut egui::Ui) {
        use crate::circuit::gen;
        let Some(id) = self.code_board else { return };
        let Some(p) = self.c.part(id) else {
            self.code_board = None;
            return;
        };
        let ds = gen::dialects(&p.kind);
        if !ds.contains(&self.code_dialect) {
            self.code_dialect = ds[0];
        }
        let code = gen::generate(&self.c, id, self.code_dialect);
        ui.horizontal(|ui| {
            ui.label(RichText::new(self.c.label(id)).size(12.0).color(ACCENT()));
            for d in &ds {
                let mut on = self.code_dialect == *d;
                if crate::app::toggle_chip(ui, &mut on, d.name(), "") {
                    self.code_dialect = *d;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("另存为…")).clicked() {
                    let ext = self.code_dialect.ext();
                    if let Some(f) = crate::diagnostics::file_dialog().set_file_name(format!("main.{ext}")).save_file() {
                        match std::fs::write(&f, &code) {
                            Ok(_) => self.toast(t("已保存"), OK()),
                            Err(e) => self.toast(e.to_string(), ERR()),
                        }
                    }
                }
                if ui.button(t("复制")).clicked() {
                    ui.ctx().copy_text(code.clone());
                    self.toast(t("已复制"), OK());
                }
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let job = crate::highlight::layout(&code, crate::highlight::Lang::C, FontId::monospace(12.5));
            ui.add(egui::Label::new(job).selectable(true));
        });
    }
}

/// 第一次打开：一块面包板、一个 USB 电源、一个 LED 加电阻，通电就亮
fn starter() -> Circuit {
    let mut c = Circuit {
        title: t("第一个电路：点亮 LED").into(),
        ..Default::default()
    };
    c.add_part("bb_half", 0, 0);
    // USB 电源：+ 接上方 + 轨，− 接 − 轨
    c.add_part("usb5v", -3, -2);
    c.add_part("ammeter", -1, -6);
    c.add_wire(vec![(-3, -2), (-3, -6), (-1, -6)], 0);
    c.add_wire(vec![(3, -6), (3, 1)], 0);
    let probe = c.add_part("probe", 5, 6);
    c.part_mut(probe).unwrap().rot = 1;
    c.add_wire(vec![(-3, 0), (2, 0)], 1);
    // + 轨 → 电阻（竖着插，下端在第 6 列）→ LED 正极；LED 倒过来插，负极在第 5 列，用黑线回 − 轨
    let r = c.add_part("res", 5, 1);
    c.part_mut(r).unwrap().set("span", "0,4");
    let l = c.add_part("led", 5, 7);
    c.part_mut(l).unwrap().rot = 2;
    c.add_wire(vec![(4, 3), (4, 0)], 1);
    c
}

pub fn fmt_time(t: f64) -> String {
    if t < 1e-3 {
        format!("{:.1} µs", t * 1e6)
    } else if t < 1.0 {
        format!("{:.2} ms", t * 1e3)
    } else if t < 60.0 {
        format!("{:.3} s", t)
    } else {
        format!("{}:{:06.3}", (t / 60.0) as u64, t % 60.0)
    }
}

/// 库列表上的一行：小图标 + 名字
fn lib_row(ui: &mut egui::Ui, name: &str, hint: &str, on: bool) -> egui::Response {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click_and_drag());
    let p = ui.painter();
    if on {
        p.rect_filled(r, CornerRadius::same(5), ACCENT().gamma_multiply(0.14));
    } else if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(5), BG3());
    }
    p.text(
        pos2(r.left() + 8.0, r.center().y),
        Align2::LEFT_CENTER,
        crate::app::truncate(name, 22),
        FontId::proportional(12.5),
        if on { ACCENT() } else { TEXT() },
    );
    if !hint.is_empty() && resp.hovered() {
        p.text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, hint, FontId::proportional(11.0), TEXT3());
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    resp
}

fn cat_header(ui: &mut egui::Ui, title: &str, n: usize, open: bool) -> egui::Response {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), tint(6));
    }
    let ir = egui::Rect::from_center_size(pos2(r.left() + 8.0, r.center().y), vec2(10.0, 10.0));
    draw_icon(p, ir, if open { Icon::Collapse } else { Icon::Forward }, TEXT3());
    p.text(pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, title, FontId::proportional(12.0), TEXT2());
    p.text(pos2(r.right() - 4.0, r.center().y), Align2::RIGHT_CENTER, n.to_string(), FontId::proportional(11.0), TEXT3());
    resp
}

#[cfg(test)]
mod tests {
    #[test]
    fn starter_led_lights() {
        let c = super::starter();
        let mut s = crate::circuit::sim::Sim::new(&c);
        for _ in 0..30 {
            s.advance(&c, 0.01, 1.0);
        }
        let amps = s.traces.iter().find(|t| t.current_part.is_some()).unwrap().at(s.t).unwrap();
        assert!((amps - 0.0136).abs() < 0.002, "current trace: {amps} A");
        let voltage = s.traces.iter().find(|t| t.current_part.is_none()).unwrap().at(s.t).unwrap();
        assert!(voltage > 1.0 && voltage < 4.0, "LED voltage: {voltage} V");
        let led = c.parts.iter().position(|p| p.kind == "led").unwrap();
        // 导线电流：串联回路里每段都约 13.6 mA
        let segs = super::flow::wire_currents(&c, &s);
        let shown: Vec<f64> = segs.iter().filter(|x| !x.hidden).map(|x| x.i.abs()).collect();
        assert!(shown.iter().all(|i| (*i - 0.0136).abs() < 0.002), "{shown:?}");
        assert!(s.ps[led].burnt.is_none());
        assert!(s.ps[led].vis.glow > 0.3, "glow {}", s.ps[led].vis.glow);
    }
}

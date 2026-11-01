//! 「电路和接线」页：左边元件库，中间画布（面包板实物视图 / 原理图视图），右边属性和知识卡，底部输出。
//! 电路数据和仿真在 crate::circuit，这里只管界面和交互。

mod canvas;
pub mod draw;
mod flow;
mod mcuedit;
mod netlink;
mod panels;
mod power;
mod scope;
mod wiring;

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
    /// 打开的示例：原理、步骤、试一试
    Example,
}

/// 鼠标拖动的状态
enum Drag {
    None,
    Pan,
    /// 移动选中的东西：按下时的世界坐标、各对象原位置
    Move {
        start: (f32, f32),
        parts: Vec<(u32, i32, i32)>,
        wires: Vec<(u32, Vec<Pt>)>,
        notes: Vec<(u32, i32, i32)>,
        moved: bool,
    },
    /// 拖导线的一个端点（或中间拐点）
    WirePt {
        id: u32,
        k: usize,
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
    pts: Vec<Pt>,
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
            events: vec![],
            seen_log: 0,
            show_flow: true,
            btab: BTab::Log,
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
        self.example = None;
        if !self.c.parts.is_empty() || !self.c.wires.is_empty() {
            self.undo.push(self.c.to_json());
        }
        self.c = c;
        self.path = path;
        self.sel.clear();
        self.draft = None;
        self.placing = None;
        self.sim = None;
        self.fit = true;
        self.dirty_at = Some(Instant::now());
        self.need_check = true;
    }

    fn save_now(&mut self) {
        let json = self.c.to_json();
        let p = autosave_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = crate::history::save_file(&p, json.as_bytes());
        if let Some(f) = &self.path {
            if let Err(e) = crate::history::save_file(f, json.as_bytes()) {
                let m = format!("{}：{e}", t("保存失败"));
                self.toast(m, ERR());
            }
        }
        self.dirty_at = None;
    }

    fn open_file(&mut self) {
        let Some(p) = rfd::FileDialog::new().set_title(t("打开电路")).add_filter(t("QCCOM 电路"), &["qccirc", "json"]).pick_file() else {
            return;
        };
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Circuit::from_json(&s)) {
            Ok(c) => self.load(c, Some(p)),
            Err(e) => self.toast(format!("{}：{e}", t("打不开")), ERR()),
        }
    }

    fn save_as(&mut self) {
        let name = if self.c.title.is_empty() { "circuit".to_string() } else { self.c.title.clone() };
        if let Some(p) = rfd::FileDialog::new()
            .set_title(t("电路另存为"))
            .add_filter(t("QCCOM 电路"), &["qccirc"])
            .set_file_name(format!("{name}.qccirc"))
            .save_file()
        {
            self.path = Some(p);
            self.save_now();
            self.toast(t("已保存"), OK());
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
            let ports = self.sim_port_names();
            if !ports.is_empty() {
                self.events.push(CvEvt::Powered(ports));
            }
            let mut s = Sim::new(&self.c);
            s.real_speed = self.speed;
            self.sim = Some(s);
            self.paused = false;
            self.seen_log = 0;
            self.last_frame = None;
            self.power.reset();
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
        // 拖动过程中不重算，松手再算
        if self.need_check && matches!(self.drag, Drag::None) {
            self.issues = crate::circuit::check::check(&self.c);
            self.need_check = false;
        }
        egui::TopBottomPanel::top("cv-top")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                left: 10,
                right: 10,
                top: 6,
                bottom: 6,
            }))
            .show_inside(ui, |ui| self.toolbar(ui));
        if self.wiring_on {
            self.wiring.set_dir(wiring::dir_for(self.project.as_deref()));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(BG0()).inner_margin(egui::Margin::same(14)))
                .show_inside(ui, |ui| self.wiring.ui(ui, &self.c));
            for (m, c) in std::mem::take(&mut self.wiring.toasts) {
                self.toast(m, c);
            }
            return;
        }
        if !compact {
            egui::SidePanel::left("cv-lib")
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 10,
                    right: 8,
                    top: 10,
                    bottom: 6,
                }))
                .default_width(220.0)
                .width_range(170.0..=340.0)
                .show_inside(ui, |ui| self.library(ui));
            egui::SidePanel::right("cv-side")
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 12,
                    right: 10,
                    top: 10,
                    bottom: 6,
                }))
                .default_width(270.0)
                .width_range(220.0..=420.0)
                .show_inside(ui, |ui| self.side(ui));
        }
        if self.bottom {
            egui::TopBottomPanel::bottom("cv-bottom")
                .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin {
                    left: 10,
                    right: 10,
                    top: 6,
                    bottom: 6,
                }))
                .default_height(if self.btab == BTab::Log { 150.0 } else { 260.0 })
                .height_range(80.0..=520.0)
                .resizable(true)
                .show_inside(ui, |ui| self.bottom_ui(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show_inside(ui, |ui| self.canvas(ui));
        if self.dirty_at.is_some_and(|t| t.elapsed() > Duration::from_millis(800)) && !matches!(self.drag, Drag::Move { .. } | Drag::WirePt { .. }) {
            self.save_now();
        }
        if self.dirty_at.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(900));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if !self.compact && icon_button(ui, Icon::Back, t("回到工具箱"), false).clicked() {
                self.events.push(CvEvt::Back);
            }
            let title = if self.c.title.is_empty() { t("未命名电路").to_string() } else { self.c.title.clone() };
            let r = ui.add(egui::Label::new(RichText::new(&title).size(14.0).strong().color(TEXT())).sense(Sense::click()));
            let r = r.on_hover_text(t("双击改名"));
            let id = ui.make_persistent_id("cv-title-edit");
            if r.double_clicked() {
                ui.memory_mut(|m| m.data.insert_temp(id, true));
            }
            if ui.memory(|m| m.data.get_temp::<bool>(id)).unwrap_or(false) {
                let mut s = self.c.title.clone();
                let e = ui.add(egui::TextEdit::singleline(&mut s).desired_width(180.0));
                e.request_focus();
                if s != self.c.title {
                    self.c.title = s;
                    self.changed();
                }
                if e.lost_focus() {
                    ui.memory_mut(|m| m.data.remove::<bool>(id));
                }
            }
            if self.dirty_at.is_some() && self.path.is_some() {
                ui.label(RichText::new("●").size(9.0).color(TEXT3()));
            }
            ui.add_space(12.0);
            // 电路 / 接线记录
            for (v, name) in [(false, t("电路")), (true, t("接线记录"))] {
                let on = self.wiring_on == v;
                let b = egui::Button::new(RichText::new(name).size(12.5).color(if on { TEXT() } else { TEXT2() }))
                    .fill(if on { BG3() } else { Color32::TRANSPARENT })
                    .stroke(st(1.0, if on { LINE() } else { Color32::TRANSPARENT }))
                    .corner_radius(CornerRadius::same(5))
                    .min_size(vec2(0.0, 26.0));
                if ui.add(b).clicked() && self.wiring_on != v {
                    self.wiring_on = v;
                    if !v {
                        self.wiring.save_now();
                    }
                }
            }
            if self.wiring_on {
                return;
            }
            ui.add_space(12.0);
            // 实物 / 原理图
            for (v, name) in [(false, t("实物")), (true, t("原理图"))] {
                let on = self.schematic == v;
                let b = egui::Button::new(RichText::new(name).size(12.5).color(if on { ACCENT() } else { TEXT2() }))
                    .fill(if on { ACCENT().gamma_multiply(0.12) } else { BG2() })
                    .stroke(st(1.0, if on { ACCENT().gamma_multiply(0.4) } else { LINE() }))
                    .corner_radius(CornerRadius::same(5))
                    .min_size(vec2(0.0, 26.0));
                if ui.add(b).clicked() {
                    self.schematic = v;
                }
            }
            ui.add_space(12.0);
            // 通电
            let on = self.powered();
            let (txt, col) = if on { (t("断电"), ERR()) } else { (t("通电"), OK()) };
            let b = egui::Button::new(RichText::new(format!("{}  {txt}", if on { "■" } else { "▶" })).size(12.5).strong().color(col))
                .fill(col.gamma_multiply(0.12))
                .stroke(st(1.0, col.gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(5))
                .min_size(vec2(72.0, 26.0));
            if ui.add(b).on_hover_text("F5").clicked() {
                self.set_power(!on);
            }
            if on {
                if icon_button(ui, if self.paused { Icon::Play } else { Icon::Pause }, if self.paused { t("继续") } else { t("暂停") }, self.paused).clicked() {
                    self.paused = !self.paused;
                }
                let cur = SPEEDS.iter().find(|s| s.0 == self.speed).map_or("?", |s| s.1);
                egui::ComboBox::from_id_salt("cv-speed").width(76.0).selected_text(t(cur)).show_ui(ui, |ui| {
                    for (v, n) in SPEEDS {
                        ui.selectable_value(&mut self.speed, v, t(n));
                    }
                });
                if let Some(s) = &self.sim {
                    let mut txt = format!("t = {}", fmt_time(s.t));
                    if !self.paused && s.real_speed < self.speed * 0.8 {
                        txt.push_str(&format!("  ({} {:.0}%)", t("实际速度"), s.real_speed / self.speed * 100.0));
                    }
                    if !s.ok {
                        txt.push_str(&format!("  {}", t("不收敛")));
                    }
                    ui.label(RichText::new(txt).font(FontId::monospace(12.0)).color(if s.ok { TEXT2() } else { WARN() }));
                }
            }
            {
                use crate::circuit::check::Level;
                let errs = self.issues.iter().filter(|i| i.level == Level::Err).count();
                let warns = self.issues.iter().filter(|i| i.level == Level::Warn).count();
                if errs + warns > 0 {
                    ui.add_space(8.0);
                    let col = if errs > 0 { ERR() } else { WARN() };
                    let txt = if errs > 0 {
                        format!("⚠ {} {}", errs + warns, t("处接错"))
                    } else {
                        format!("⚠ {} {}", warns, t("处提醒"))
                    };
                    let b = egui::Button::new(RichText::new(txt).size(12.0).color(col))
                        .fill(col.gamma_multiply(0.10))
                        .stroke(st(1.0, col.gamma_multiply(0.4)))
                        .corner_radius(CornerRadius::same(5))
                        .min_size(vec2(0.0, 26.0));
                    if ui.add(b).on_hover_text(t("看接错检查")).clicked() {
                        self.sel.clear();
                        self.tab = RTab::Props;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = icon_button(ui, Icon::More, t("更多"), false);
                let pop = egui::Id::new("cv-more");
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pop));
                }
                egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| self.more_menu(ui));
                let n_new = self.sim.as_ref().map_or(0, |s| s.log.len().saturating_sub(self.seen_log));
                let r = icon_button(ui, Icon::Terminal, t("输出"), self.bottom);
                if n_new > 0 && !self.bottom {
                    ui.painter().circle_filled(r.rect.right_top() + vec2(-6.0, 6.0), 3.5, WARN());
                }
                if r.clicked() {
                    self.bottom = !self.bottom;
                }
                if icon_button(ui, Icon::Maximize, &format!("{} (Home)", t("看全部")), false).clicked() {
                    self.fit = true;
                }
                if icon_button(ui, Icon::Forward, &format!("{} (Ctrl+Y)", t("重做")), false).clicked() {
                    self.redo();
                }
                if icon_button(ui, Icon::Back, &format!("{} (Ctrl+Z)", t("撤销")), false).clicked() {
                    self.undo();
                }
            });
        });
    }

    fn more_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(200.0);
        if crate::app::menu_item(ui, t("新建电路"), "") {
            self.load(Circuit::default(), None);
            self.c.add_part("bb_half", 0, 0);
        }
        if crate::app::menu_item(ui, t("打开电路…"), "") {
            self.open_file();
        }
        if crate::app::menu_item(ui, t("另存为…"), "Ctrl+S") {
            self.save_as();
        }
        ui.separator();
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
            if self.draft.is_some() || self.placing.is_some() {
                self.draft = None;
                self.placing = None;
            } else {
                self.sel.clear();
            }
        }
        if i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace) {
            if let Some(d) = &mut self.draft {
                if d.pts.len() > 1 {
                    d.pts.pop();
                } else {
                    self.draft = None;
                }
            } else {
                self.delete_sel();
            }
        }
        if i.key_pressed(Key::F5) {
            self.set_power(!self.powered());
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
            if self.path.is_some() {
                self.save_now();
                self.toast(t("已保存"), OK());
            } else {
                self.save_as();
            }
        } else if cmd && i.key_pressed(Key::D) {
            if self.copy_sel() {
                self.paste(2, 2);
            }
        } else if !cmd && i.key_pressed(Key::R) {
            if self.placing.is_some() {
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
        if self.btab != BTab::Log {
            let Some(s) = &self.sim else {
                ui.label(RichText::new(t("通电后这里显示波形。")).color(TEXT3()));
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
        let rows: Vec<(f64, String, String)> = s.log.iter().map(|(tt, id, m)| (*tt, self.c.label(*id), m.clone())).collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if rows.is_empty() {
                ui.label(RichText::new(t("暂时没有事件")).color(TEXT3()));
            }
            for (tt, who, m) in rows {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fmt_time(tt)).font(FontId::monospace(11.5)).color(TEXT3()));
                    ui.label(RichText::new(who).font(FontId::monospace(11.5)).color(ACCENT()));
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
                    if let Some(f) = rfd::FileDialog::new().set_file_name(format!("main.{ext}")).save_file() {
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
    c.add_wire(vec![(-3, -2), (3, 1)], 0);
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
        let led = c.parts.iter().position(|p| p.kind == "led").unwrap();
        // 导线电流：串联回路里每段都约 13.6 mA
        let segs = super::flow::wire_currents(&c, &s);
        let shown: Vec<f64> = segs.iter().filter(|x| !x.hidden).map(|x| x.i.abs()).collect();
        assert!(shown.iter().all(|i| (*i - 0.0136).abs() < 0.002), "{shown:?}");
        assert!(s.ps[led].burnt.is_none());
        assert!(s.ps[led].vis.glow > 0.3, "glow {}", s.ps[led].vis.glow);
    }
}

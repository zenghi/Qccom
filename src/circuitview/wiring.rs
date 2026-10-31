//! 接线记录：实物是怎么接的。一张接线表（逐行勾选）、实物照片上打编号、硬件改动记录。
//! 有工程时存在 工程/.qccom/wiring/ 里（跟着工程走、能进 Git），没有就放数据目录。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, RichText, Sense};
use serde::{Deserialize, Serialize};

use crate::app::{icon_button, Icon};
use crate::circuit::net::Netlist;
use crate::circuit::{lib, Circuit, WIRE_COLORS};
use crate::i18n::t;
use crate::theme::*;

use super::draw;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Doc {
    pub rows: Vec<Row>,
    pub photos: Vec<Photo>,
    pub log: Vec<Change>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Row {
    /// 从哪儿（设备.引脚）
    pub a: String,
    /// 接到哪儿
    pub b: String,
    /// 杜邦线颜色（WIRE_COLORS 下标）
    pub color: u8,
    pub note: String,
    pub done: bool,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Photo {
    /// photos/ 下的文件名
    pub file: String,
    pub marks: Vec<Mark>,
}

/// 照片上的编号：位置按图片宽高的比例存，row 是接线表的行号（从 0 开始）
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Mark {
    pub x: f32,
    pub y: f32,
    pub row: usize,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
#[serde(default)]
pub struct Change {
    pub date: String,
    pub text: String,
}

/// 常用设备：名字、(引脚, 一般接到哪, 颜色)
const TEMPLATES: &[(&str, &[(&str, &str, u8)])] = &[
    ("USB-TTL", &[("GND", "GND", 1), ("TXD", "RX", 2), ("RXD", "TX", 3), ("3V3", "3V3", 0)]),
    (
        "ST-Link",
        &[("GND", "GND", 1), ("SWDIO", "SWDIO (PA13)", 2), ("SWCLK", "SWCLK (PA14)", 3), ("3.3V", "3V3", 0), ("RST", "NRST", 6)],
    ),
    (
        "J-Link",
        &[("VTref", "3V3", 0), ("GND", "GND", 1), ("SWDIO", "SWDIO (PA13)", 2), ("SWCLK", "SWCLK (PA14)", 3), ("RESET", "NRST", 6)],
    ),
    (
        "ESP-Prog",
        &[("GND", "GND", 1), ("TXD", "U0RXD", 2), ("RXD", "U0TXD", 3), ("EN", "EN", 4), ("IO0", "GPIO0", 6), ("3V3", "3V3", 0)],
    ),
    (
        "CMSIS-DAP",
        &[("GND", "GND", 1), ("SWDIO", "SWDIO", 2), ("SWCLK", "SWCLK", 3), ("3V3", "3V3", 0), ("TX", "RX", 4), ("RX", "TX", 5)],
    ),
    ("逻辑分析仪", &[("GND", "GND", 1), ("CH0", "", 9), ("CH1", "", 0), ("CH2", "", 5), ("CH3", "", 2)]),
    (
        "INA219",
        &[("VCC", "3V3", 0), ("GND", "GND", 1), ("SDA", "SDA", 4), ("SCL", "SCL", 2), ("VIN+", "电源 +", 5), ("VIN-", "负载 +", 7)],
    ),
];

pub struct WiringUi {
    dir: PathBuf,
    /// 当前记录的名字（文件名去掉 .json）
    name: String,
    names: Vec<String>,
    pub doc: Doc,
    saved: Doc,
    dirty_at: Option<Instant>,
    photo: usize,
    /// 点照片时标记哪一行
    mark_row: usize,
    tex: HashMap<String, egui::TextureHandle>,
    new_log: String,
    new_name: Option<String>,
    pub toasts: Vec<(String, Color32)>,
}

impl Default for WiringUi {
    fn default() -> Self {
        WiringUi {
            dir: PathBuf::new(),
            name: String::new(),
            names: vec![],
            doc: Doc::default(),
            saved: Doc::default(),
            dirty_at: None,
            photo: 0,
            mark_row: 0,
            tex: HashMap::new(),
            new_log: String::new(),
            new_name: None,
            toasts: vec![],
        }
    }
}

/// 存放位置：工程里的 .qccom/wiring，没打开工程就放数据目录
pub fn dir_for(project: Option<&Path>) -> PathBuf {
    match project {
        Some(p) => p.join(".qccom").join("wiring"),
        None => crate::data_dir().join("wiring"),
    }
}

fn guess_color(pin: &str) -> u8 {
    let p = pin.to_ascii_uppercase();
    if p.contains("GND") || p == "VSS" || p == "-" {
        1
    } else if p.contains("VCC") || p.contains("5V") || p.contains("3V3") || p.contains("3.3V") || p.contains("VIN") || p == "+" || p == "VDD" {
        0
    } else if p.contains("TX") || p.contains("SDA") || p.contains("MOSI") {
        2
    } else if p.contains("RX") || p.contains("SCL") || p.contains("MISO") {
        3
    } else {
        4
    }
}

/// 从电路生成接线表：每个网络把第一个引脚和其余引脚各连一行（同一个元件只取一个引脚）
pub fn from_circuit(c: &Circuit) -> Vec<Row> {
    let nl = Netlist::build(c);
    let mut net_color: HashMap<usize, u8> = HashMap::new();
    for w in &c.wires {
        if let Some(n) = nl.wire.get(&w.id) {
            net_color.entry(*n).or_insert(w.color);
        }
    }
    let pin_name = |id: u32, k: usize| -> String {
        let kind = c.part(id).map(|p| p.kind.clone()).unwrap_or_default();
        let pn = lib::def(&kind).and_then(|d| d.pins.get(k)).map(|p| p.name.clone()).unwrap_or_else(|| k.to_string());
        format!("{}.{}", c.label(id), pn)
    };
    let mut rows = vec![];
    for (net, members) in nl.members.iter().enumerate() {
        let mut seen = vec![];
        let mut pins = vec![];
        for (id, k) in members {
            if !seen.contains(id) {
                seen.push(*id);
                pins.push((*id, *k));
            }
        }
        if pins.len() < 2 {
            continue;
        }
        // 开发板放前面当「从」，读起来像「板子的 PA9 → 模块的 RX」
        pins.sort_by_key(|(id, _)| {
            let board = c.part(*id).and_then(|p| lib::def(&p.kind)).is_some_and(|d| d.cat == lib::Cat::Board);
            (!board, *id)
        });
        let first = pin_name(pins[0].0, pins[0].1);
        for (id, k) in &pins[1..] {
            let b = pin_name(*id, *k);
            let color = net_color.get(&net).copied().unwrap_or_else(|| guess_color(first.rsplit('.').next().unwrap_or("")));
            rows.push(Row {
                a: first.clone(),
                b,
                color,
                ..Default::default()
            });
        }
    }
    rows
}

pub fn to_markdown(name: &str, d: &Doc) -> String {
    let mut s = format!("# {} · {name}\n\n", t("接线记录"));
    s.push_str(&format!(
        "| # | {} | {} | {} | {} | {} |\n|---|---|---|---|---|---|\n",
        t("从"),
        t("接到"),
        t("线色"),
        t("备注"),
        t("已接")
    ));
    for (i, r) in d.rows.iter().enumerate() {
        let esc = |x: &str| x.replace('|', "\\|");
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            esc(&r.a),
            esc(&r.b),
            t(WIRE_COLORS[r.color as usize % 10].0),
            esc(&r.note),
            if r.done { "✓" } else { "" }
        ));
    }
    if !d.photos.is_empty() {
        s.push_str(&format!("\n## {}\n\n", t("照片")));
        for p in &d.photos {
            s.push_str(&format!("![](photos/{})\n\n", p.file));
        }
    }
    if !d.log.is_empty() {
        s.push_str(&format!("\n## {}\n\n", t("硬件改动记录")));
        for c in &d.log {
            s.push_str(&format!("- {}  {}\n", c.date, c.text));
        }
    }
    s
}

/// 照片压缩：长边不超过 1600，存 JPEG
fn import_photo(src: &Path, dir: &Path, stem: &str) -> Result<String, String> {
    let img = image::open(src).map_err(|e| e.to_string())?;
    let img = if img.width().max(img.height()) > 1600 {
        img.resize(1600, 1600, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let pd = dir.join("photos");
    std::fs::create_dir_all(&pd).map_err(|e| e.to_string())?;
    let mut n = 1;
    let file = loop {
        let f = format!("{stem}-{n}.jpg");
        if !pd.join(&f).exists() {
            break f;
        }
        n += 1;
    };
    let mut out = std::io::BufWriter::new(std::fs::File::create(pd.join(&file)).map_err(|e| e.to_string())?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&img.to_rgb8())
        .map_err(|e| e.to_string())?;
    Ok(file)
}

fn safe_name(s: &str) -> String {
    let s: String = s.trim().chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    if s.is_empty() {
        t("接线").to_string()
    } else {
        s
    }
}

impl WiringUi {
    /// 每帧调用：工程换了就换目录
    pub fn set_dir(&mut self, dir: PathBuf) {
        if dir == self.dir {
            return;
        }
        self.save_now();
        self.dir = dir;
        self.tex.clear();
        self.refresh_names();
        let first = self.names.first().cloned().unwrap_or_else(|| t("接线").to_string());
        self.open(&first);
    }

    fn refresh_names(&mut self) {
        let mut v: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        (p.extension().is_some_and(|x| x == "json")).then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned())).flatten()
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        self.names = v;
    }

    fn open(&mut self, name: &str) {
        self.save_now();
        self.name = name.to_string();
        self.doc = std::fs::read_to_string(self.dir.join(format!("{name}.json")))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        self.saved = self.doc.clone();
        self.photo = 0;
        self.mark_row = 0;
    }

    pub fn save_now(&mut self) {
        self.dirty_at = None;
        if self.doc == self.saved || self.name.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&self.dir);
        let json = serde_json::to_string_pretty(&self.doc).unwrap_or_default();
        match crate::history::save_file(&self.dir.join(format!("{}.json", self.name)), json.as_bytes()) {
            Ok(()) => {
                self.saved = self.doc.clone();
                if !self.names.contains(&self.name) {
                    self.refresh_names();
                }
            }
            Err(e) => self.toasts.push((format!("{}：{e}", t("保存失败")), ERR())),
        }
    }

    pub fn add_photo(&mut self, p: &Path) {
        match import_photo(p, &self.dir, &self.name) {
            Ok(f) => {
                self.doc.photos.push(Photo { file: f, marks: vec![] });
                self.photo = self.doc.photos.len() - 1;
                self.save_now();
            }
            Err(e) => self.toasts.push((format!("{}：{e}", t("照片打不开")), ERR())),
        }
    }

    fn texture(&mut self, ctx: &egui::Context, file: &str) -> Option<egui::TextureHandle> {
        if let Some(h) = self.tex.get(file) {
            return Some(h.clone());
        }
        let img = image::open(self.dir.join("photos").join(file)).ok()?.to_rgba8();
        let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        let h = ctx.load_texture(format!("wiring-{file}"), ci, egui::TextureOptions::LINEAR);
        self.tex.insert(file.to_string(), h.clone());
        Some(h)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Circuit) {
        let before = self.doc.clone();
        self.header(ui, c);
        ui.add_space(8.0);
        let w = ui.available_width();
        let right_w = (w * 0.42).clamp(280.0, 620.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(w - right_w - 30.0, ui.available_height()), egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::ScrollArea::vertical().id_salt("wr-table").auto_shrink([false, false]).show(ui, |ui| self.table(ui));
            });
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(vec2(right_w, ui.available_height()), egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::ScrollArea::vertical().id_salt("wr-right").auto_shrink([false, false]).show(ui, |ui| {
                    self.photos_ui(ui);
                    ui.add_space(14.0);
                    self.log_ui(ui);
                });
            });
        });
        if self.doc != before {
            self.dirty_at = Some(Instant::now());
        }
        if self.dirty_at.is_some_and(|t| t.elapsed() > Duration::from_millis(600)) {
            self.save_now();
        }
        if self.dirty_at.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(700));
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, c: &Circuit) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut pick = None;
            egui::ComboBox::from_id_salt("wr-name").width(160.0).selected_text(&self.name).show_ui(ui, |ui| {
                for n in &self.names {
                    if ui.selectable_label(*n == self.name, n).clicked() {
                        pick = Some(n.clone());
                    }
                }
                ui.separator();
                if ui.selectable_label(false, format!("＋ {}", t("新建记录"))).clicked() {
                    self.new_name = Some(String::new());
                }
            });
            if let Some(n) = pick {
                self.open(&n);
            }
            if let Some(nn) = &mut self.new_name {
                let r = ui.add(egui::TextEdit::singleline(nn).hint_text(t("记录名，如 主板 V2")).desired_width(150.0));
                r.request_focus();
                if r.lost_focus() {
                    let name = safe_name(nn);
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    self.new_name = None;
                    if enter {
                        self.open(&name);
                        if !self.names.contains(&name) {
                            self.names.push(name);
                        }
                    }
                }
            }
            let done = self.doc.rows.iter().filter(|r| r.done).count();
            if !self.doc.rows.is_empty() {
                let col = if done == self.doc.rows.len() { OK() } else { TEXT2() };
                ui.label(RichText::new(format!("{} {done}/{}", t("已接"), self.doc.rows.len())).size(12.0).color(col));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("导出 Markdown")).on_hover_text(t("存成 .md 并复制到剪贴板，可以贴进 README 或提交说明")).clicked() {
                    let md = to_markdown(&self.name, &self.doc);
                    let _ = std::fs::create_dir_all(&self.dir);
                    let f = self.dir.join(format!("{}.md", self.name));
                    ui.ctx().copy_text(md.clone());
                    match crate::history::save_file(&f, md.as_bytes()) {
                        Ok(()) => self.toasts.push((format!("{} {}", t("已导出并复制："), f.display()), OK())),
                        Err(e) => self.toasts.push((e.to_string(), ERR())),
                    }
                }
                let r = ui.button(format!("{} ▾", t("设备模板")));
                let pop = egui::Id::new("wr-tpl");
                if r.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(pop));
                }
                egui::popup::popup_below_widget(ui, pop, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(160.0);
                    for (name, pins) in TEMPLATES {
                        if crate::app::menu_item(ui, t(name), "") {
                            for (p, to, col) in pins.iter() {
                                self.doc.rows.push(Row {
                                    a: format!("{}.{p}", t(name)),
                                    b: t(to).to_string(),
                                    color: *col,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                });
                let gen = ui.add_enabled(!c.parts.is_empty(), egui::Button::new(t("从电路生成")));
                if gen.on_hover_text(t("把画布上的连接列成接线表，已有的行不重复加")).clicked() {
                    let mut added = 0;
                    for r in from_circuit(c) {
                        if !self.doc.rows.iter().any(|x| x.a == r.a && x.b == r.b || x.a == r.b && x.b == r.a) {
                            self.doc.rows.push(r);
                            added += 1;
                        }
                    }
                    self.toasts.push((format!("{} {added}", t("接线表新增行数")), OK()));
                }
            });
        });
        ui.label(RichText::new(format!("{}  {}", t("保存在"), self.dir.display())).size(11.0).color(TEXT3()));
    }

    fn color_pick(ui: &mut egui::Ui, v: &mut u8, id: egui::Id) {
        let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
        let (name, rgb) = WIRE_COLORS[*v as usize % 10];
        ui.painter().rect(r.shrink(3.0), CornerRadius::same(4), draw::rgb(rgb), st(1.0, LINE()), egui::StrokeKind::Inside);
        let resp = resp.on_hover_text(t(name));
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup::popup_below_widget(ui, id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(110.0);
            for (k, (n, c)) in WIRE_COLORS.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                    ui.painter().rect(r, CornerRadius::same(3), draw::rgb(*c), st(1.0, LINE()), egui::StrokeKind::Inside);
                    if ui.selectable_label(*v as usize == k, t(n)).clicked() {
                        *v = k as u8;
                    }
                });
            }
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        if self.doc.rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(t("还没有接线记录")).size(15.0).color(TEXT2()));
                ui.add_space(4.0);
                ui.label(RichText::new(t("点「从电路生成」把画布上的连接列出来，或者用「设备模板」加常用设备（USB-TTL、ST-Link……），也可以一行一行手动加")).color(TEXT3()));
            });
            ui.add_space(12.0);
        }
        let w = ui.available_width();
        let mut del = None;
        let marked: Vec<usize> = self.doc.photos.iter().flat_map(|p| p.marks.iter().map(|m| m.row)).collect();
        for (i, r) in self.doc.rows.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // 编号（照片上打的就是这个号），线色做底
                let (nr, resp) = ui.allocate_exact_size(vec2(26.0, 22.0), Sense::click());
                let col = draw::rgb(WIRE_COLORS[r.color as usize % 10].1);
                ui.painter().rect_filled(nr, CornerRadius::same(11), col.gamma_multiply(if r.done { 0.35 } else { 0.9 }));
                let txt = if col.r() as u32 + col.g() as u32 + col.b() as u32 > 420 {
                    Color32::from_rgb(20, 20, 24)
                } else {
                    Color32::WHITE
                };
                ui.painter().text(nr.center(), egui::Align2::CENTER_CENTER, (i + 1).to_string(), FontId::proportional(11.5), txt);
                if resp
                    .on_hover_text(if marked.contains(&i) {
                        t("照片上已标记")
                    } else {
                        t("点一下，再点照片就能标记这一行")
                    })
                    .clicked()
                {
                    self.mark_row = i;
                }
                ui.checkbox(&mut r.done, "").on_hover_text(t("接好了打勾"));
                // 编号、勾、箭头、线色、删除和间距大约占 170
                let rest = (w - 170.0).max(240.0);
                let fw = rest * 0.34;
                ui.add(egui::TextEdit::singleline(&mut r.a).hint_text(t("从：设备.引脚")).desired_width(fw));
                ui.label(RichText::new("→").color(TEXT3()));
                ui.add(egui::TextEdit::singleline(&mut r.b).hint_text(t("接到")).desired_width(fw));
                Self::color_pick(ui, &mut r.color, egui::Id::new(("wr-col", i)));
                ui.add(egui::TextEdit::singleline(&mut r.note).hint_text(t("备注")).desired_width(rest - 2.0 * fw));
                if icon_button(ui, Icon::Close, t("删除这行"), false).clicked() {
                    del = Some(i);
                }
            });
        }
        if let Some(i) = del {
            self.doc.rows.remove(i);
            for p in &mut self.doc.photos {
                p.marks.retain(|m| m.row != i);
                for m in &mut p.marks {
                    if m.row > i {
                        m.row -= 1;
                    }
                }
            }
        }
        ui.add_space(6.0);
        if ui.button(format!("＋ {}", t("加一行"))).clicked() {
            let color = self.doc.rows.last().map_or(4, |r| (r.color + 1) % 10);
            self.doc.rows.push(Row { color, ..Default::default() });
        }
    }

    fn photos_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("实物照片")).strong().color(TEXT()));
            for i in 0..self.doc.photos.len() {
                if ui.selectable_label(self.photo == i, (i + 1).to_string()).clicked() {
                    self.photo = i;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.doc.photos.is_empty() && icon_button(ui, Icon::Trash, t("删掉这张照片"), false).clicked() {
                    let p = self.doc.photos.remove(self.photo.min(self.doc.photos.len() - 1));
                    let _ = std::fs::remove_file(self.dir.join("photos").join(&p.file));
                    self.tex.remove(&p.file);
                    self.photo = self.photo.saturating_sub(1);
                }
                if icon_button(ui, Icon::Plus, t("添加照片（也可以直接拖进来）"), false).clicked() {
                    if let Some(fs) = rfd::FileDialog::new().add_filter(t("图片"), &["jpg", "jpeg", "png", "bmp", "webp"]).pick_files() {
                        for f in fs {
                            self.add_photo(&f);
                        }
                    }
                }
            });
        });
        ui.add_space(4.0);
        if self.doc.photos.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::hover());
            ui.painter().rect(r, CornerRadius::same(8), BG2(), egui::Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                t("把实物照片拖进来，在上面给每根线标上编号"),
                FontId::proportional(12.5),
                TEXT3(),
            );
            return;
        }
        self.photo = self.photo.min(self.doc.photos.len() - 1);
        let file = self.doc.photos[self.photo].file.clone();
        let Some(tex) = self.texture(ui.ctx(), &file) else {
            ui.label(RichText::new(format!("{}：{file}", t("照片打不开"))).color(ERR()));
            return;
        };
        if !self.doc.rows.is_empty() {
            self.mark_row = self.mark_row.min(self.doc.rows.len() - 1);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("点照片标记第")).size(12.0).color(TEXT2()));
                let rows = &self.doc.rows;
                egui::ComboBox::from_id_salt("wr-markrow")
                    .width(150.0)
                    .selected_text(format!("{}  {}", self.mark_row + 1, crate::app::truncate(&rows[self.mark_row].a, 14)))
                    .show_ui(ui, |ui| {
                        for (i, r) in rows.iter().enumerate() {
                            ui.selectable_value(&mut self.mark_row, i, format!("{}  {} → {}", i + 1, r.a, r.b));
                        }
                    });
                ui.label(RichText::new(t("行；右键删除标记")).size(12.0).color(TEXT2()));
            });
        }
        let sz = tex.size_vec2();
        let w = ui.available_width();
        let h = (w * sz.y / sz.x).min(560.0);
        let w = h * sz.x / sz.y;
        let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        let p = ui.painter_at(r);
        p.image(tex.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let ph = &mut self.doc.photos[self.photo];
        let mut remove = None;
        let hover = resp.hover_pos();
        for (k, m) in ph.marks.iter().enumerate() {
            let c = pos2(r.min.x + m.x * r.width(), r.min.y + m.y * r.height());
            let row = self.doc.rows.get(m.row);
            let col = row.map_or(TEXT3(), |rw| draw::rgb(WIRE_COLORS[rw.color as usize % 10].1));
            p.circle(c, 11.0, col, egui::Stroke::new(2.0, Color32::WHITE));
            let txt = if col.r() as u32 + col.g() as u32 + col.b() as u32 > 420 {
                Color32::from_rgb(20, 20, 24)
            } else {
                Color32::WHITE
            };
            p.text(c, egui::Align2::CENTER_CENTER, (m.row + 1).to_string(), FontId::proportional(11.5), txt);
            if hover.is_some_and(|h| h.distance(c) < 12.0) {
                if let Some(rw) = row {
                    resp.clone().on_hover_text(format!("{} → {}", rw.a, rw.b));
                }
                if resp.secondary_clicked() {
                    remove = Some(k);
                }
            }
        }
        if let Some(k) = remove {
            ph.marks.remove(k);
        } else if resp.clicked() && !self.doc.rows.is_empty() {
            if let Some(h) = resp.interact_pointer_pos() {
                let (x, y) = ((h.x - r.min.x) / r.width(), (h.y - r.min.y) / r.height());
                ph.marks.retain(|m| m.row != self.mark_row);
                ph.marks.push(Mark { x, y, row: self.mark_row });
                // 自动跳到下一行还没标的
                let n = self.doc.rows.len();
                if let Some(next) = (1..n).map(|d| (self.mark_row + d) % n).find(|i| !ph.marks.iter().any(|m| m.row == *i)) {
                    self.mark_row = next;
                }
            }
        }
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("硬件改动记录")).strong().color(TEXT()));
        ui.label(RichText::new(t("飞线、换电阻、割线……记下来，过几个月还知道板子改过什么")).size(11.5).color(TEXT3()));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.new_log)
                    .hint_text(t("如：R12 换成 4.7k，PA3 飞线到 J2.5"))
                    .desired_width(ui.available_width() - 60.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button(t("记下")).clicked() || enter) && !self.new_log.trim().is_empty() {
                let date = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                self.doc.log.insert(
                    0,
                    Change {
                        date,
                        text: std::mem::take(&mut self.new_log).trim().to_string(),
                    },
                );
            }
        });
        let mut del = None;
        for (i, c) in self.doc.log.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&c.date).font(FontId::monospace(11.5)).color(TEXT3()));
                ui.label(RichText::new(&c.text).color(TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                        del = Some(i);
                    }
                });
            });
        }
        if let Some(i) = del {
            self.doc.log.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_rows_and_markdown() {
        let ex = crate::circuit::examples::find("A1").unwrap();
        let c = ex.build().unwrap();
        let rows = from_circuit(&c);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|r| r.a.contains('.') && r.b.contains('.')));
        let d = Doc { rows, ..Default::default() };
        let md = to_markdown("x", &d);
        assert!(md.contains("| 1 |"));
    }
}

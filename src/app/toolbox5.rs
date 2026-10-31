//! 工具箱 v1.6：数据和协议（结构体解析、CBOR / MessagePack / Protobuf 解码、VCD 波形、二维码）。

use std::collections::HashSet;

use crate::toolbox::conv;
use crate::toolbox::decode::{self, Node};
use crate::toolbox::structp::{self, Opts};
use crate::toolbox::vcd::{self, Vcd};

use super::logx::small_chip;
use super::toolbox::{cell, dense_rows, header_row, settings_card, Tool, MONO, ROW_H};
use super::toolbox3::{file_pick, series, stat};
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cfg5 {
    pub st_src: String,
    pub st_hex: String,
    pub st_pick: String,
    pub st_little: bool,
    pub st_packed: bool,
    pub st_ptr8: bool,
    pub dc_in: String,
    pub dc_b64: bool,
    pub dc_fmt: decode::Fmt,
    pub vcd_path: String,
    pub qr_wifi: bool,
    pub qr_text: String,
    pub qr_ssid: String,
    pub qr_sec: u8,
    pub qr_hidden: bool,
    pub qr_ec: u8,
}

impl Default for Cfg5 {
    fn default() -> Self {
        Cfg5 {
            st_src: "typedef struct __attribute__((packed)) {\n    uint8_t  head;      /* 0xAA */\n    uint8_t  cmd;\n    uint16_t len;\n    int16_t  temp;      /* 0.1 °C */\n    uint8_t  flags : 3;\n    uint8_t  mode  : 5;\n    float    voltage;\n    char     name[6];\n    uint16_t crc;\n} frame_t;\n".into(),
            st_hex: "AA 01 0E 00 F5 00 2D 66 66 56 40 51 43 43 4F 4D 00 34 12".into(),
            st_pick: String::new(),
            st_little: true,
            st_packed: false,
            st_ptr8: false,
            dc_in: "A3 64 74 65 6D 70 F9 4D 80 63 69 64 73 82 01 02 62 6F 6B F5".into(),
            dc_b64: false,
            dc_fmt: decode::Fmt::Auto,
            vcd_path: String::new(),
            qr_wifi: false,
            qr_text: "https://github.com/zenghi/QCCOM".into(),
            qr_ssid: String::new(),
            qr_sec: 0,
            qr_hidden: false,
            qr_ec: 1,
        }
    }
}

#[derive(Default)]
pub struct Ui5 {
    st_closed: HashSet<String>,
    vcd_key: String,
    vcd: Option<Result<Vcd, String>>,
    /// 显示的时间范围
    view: (f64, f64),
    cur_a: Option<u64>,
    cur_b: Option<u64>,
    vcd_sel: Option<usize>,
    vcd_filter: String,
    /// Wi-Fi 密码只放在内存里，不写进设置文件
    qr_pass: String,
    qr_key: String,
    qr_tex: Option<egui::TextureHandle>,
    qr_mod: Option<(usize, Vec<bool>)>,
    qr_err: Option<String>,
    dc_closed: HashSet<String>,
}

pub(super) fn tool_text5(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::Struct => (t("结构体解析"), t("贴上 C 结构体定义和一段 HEX，按对齐、packed、大小端、位域拆成每个字段")),
        Tool::Decode => (t("CBOR / MsgPack / Protobuf"), t("把二进制数据解码成树；Protobuf 不需要 .proto 文件")),
        Tool::Vcd => (t("VCD 波形"), t("打开逻辑分析仪或仿真导出的 VCD 文件，看数字波形、量时间")),
        Tool::Qr => (t("二维码"), t("文字、网址或 Wi-Fi 配网信息生成二维码，可以存成图片或 C 数组")),
        Tool::Circuit => (t("电路仿真"), t("在面包板上搭电路、通电看结果，接错会冒烟；单片机引脚不写代码也能动起来")),
        _ => ("", ""),
    }
}

impl App {
    pub(super) fn tb_v16c(&mut self, ui: &mut egui::Ui, tool: Tool) {
        match tool {
            Tool::Struct => self.tb_struct(ui),
            Tool::Decode => self.tb_decode(ui),
            Tool::Vcd => self.tb_vcd(ui),
            Tool::Qr => self.tb_qr(ui),
            _ => {}
        }
    }

    pub(super) fn tool_drop5(&mut self, tool: Tool, p: PathBuf) {
        let c = &mut self.s.tb.z.y;
        match tool {
            Tool::Vcd => c.vcd_path = p.to_string_lossy().into_owned(),
            Tool::Decode => {
                if let Ok(b) = std::fs::read(&p) {
                    c.dc_b64 = false;
                    c.dc_in = conv::hex(&b[..b.len().min(1 << 16)]);
                }
            }
            Tool::Struct => {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    c.st_src = s;
                }
            }
            Tool::Checksum => self.hash_file(p),
            _ => self.load_log_path(&p, false),
        }
    }

    /// 代码页右键「解析结构体」
    pub(super) fn open_struct(&mut self, src: String) {
        self.s.tb.z.y.st_src = src;
        self.s.tb.z.y.st_pick.clear();
        self.open_tool(Tool::Struct);
    }

    // ------------------------------------------------------------ 结构体

    fn tb_struct(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.y;
        let u = &mut self.tb.z.y;
        let o = Opts {
            little: c.st_little,
            ptr: if c.st_ptr8 { 8 } else { 4 },
            force_packed: c.st_packed,
        };
        let parsed = structp::parse(&c.st_src, o.ptr);
        let data = conv::parse_hex(&c.st_hex);
        let w = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width((w * 0.45).clamp(300.0, 560.0));
                eyebrow(ui, t("结构体定义"), "");
                let mut layouter = |ui: &egui::Ui, s: &str, wrap: f32| {
                    let mut job = crate::highlight::layout(s, crate::highlight::Lang::C, FontId::new(12.5, FontFamily::Monospace));
                    job.wrap.max_width = wrap;
                    ui.fonts(|f| f.layout_job(job))
                };
                egui::ScrollArea::vertical().id_salt("st_src").max_height(300.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut c.st_src)
                            .code_editor()
                            .desired_rows(12)
                            .desired_width(f32::INFINITY)
                            .layouter(&mut layouter),
                    );
                });
                ui.add_space(8.0);
                eyebrow(ui, t("数据（HEX）"), &data.as_ref().map(|d| tf("{} 字节", &[&d.len()])).unwrap_or_default());
                ui.add(
                    egui::TextEdit::multiline(&mut c.st_hex)
                        .font(MONO(12.5))
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("AA 01 0E 00 ..."),
                );
                if let Err(e) = &data {
                    ui.label(RichText::new(e).color(ERR()));
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let mut le = c.st_little;
                    if toggle_chip(ui, &mut le, if c.st_little { t("小端") } else { t("大端") }, t("ARM、x86、ESP32 都是小端")) {
                        c.st_little = le;
                    }
                    toggle_chip(ui, &mut c.st_packed, t("全部按 packed"), t("不管定义里写没写，都按 1 字节对齐"));
                    toggle_chip(ui, &mut c.st_ptr8, t("64 位指针"), t("指针、long、size_t 按 8 字节"));
                });
                ui.label(
                    RichText::new(t("按 GCC / ARM 的规则排布：自然对齐、#pragma pack、__attribute__((packed))、位域"))
                        .small()
                        .color(TEXT3()),
                );
            });
            ui.add_space(16.0);
            ui.vertical(|ui| {
                let defs = match &parsed {
                    Ok(d) => d,
                    Err(e) => {
                        ui.label(RichText::new(e).color(ERR()));
                        return;
                    }
                };
                let names: Vec<(usize, String)> = defs.defs.iter().enumerate().filter(|(_, d)| !d.name.is_empty()).map(|(i, d)| (i, d.name.clone())).collect();
                let idx = names.iter().find(|n| n.1 == c.st_pick).map(|n| n.0).unwrap_or_else(|| defs.main());
                let root = defs.layout(idx, &o);
                let size = root.bits / 8;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("st_pick").width(200.0).selected_text(&defs.defs[idx].name).show_ui(ui, |ui| {
                        for (_, n) in &names {
                            if ui.selectable_label(*n == defs.defs[idx].name, n).clicked() {
                                c.st_pick = n.clone();
                            }
                        }
                    });
                    pill(ui, &tf("sizeof = {}", &[&size]), ACCENT());
                    if let Ok(d) = &data {
                        let col = if d.len() as u64 == size { OK() } else { WARN() };
                        let note = if d.len() as u64 == size {
                            t("数据长度刚好").to_string()
                        } else if (d.len() as u64) < size {
                            tf("数据少 {} 字节", &[&(size - d.len() as u64)])
                        } else {
                            tf("数据多 {} 字节", &[&(d.len() as u64 - size)])
                        };
                        ui.label(RichText::new(note).small().color(col));
                    }
                });
                ui.add_space(6.0);
                let d = data.as_deref().unwrap_or(&[]);
                let rows = structp::rows(defs, &root, d, &o);
                let cols = [(t("字段"), 190.0), (t("类型"), 120.0), (t("偏移"), 54.0), ("HEX", 120.0), (t("值"), 200.0)];
                header_row(ui, &cols);
                let mut hidden: Option<String> = None;
                let visible: Vec<&structp::Row> = rows
                    .iter()
                    .filter(|r| {
                        if let Some(h) = &hidden {
                            if r.path.starts_with(&format!("{h}.")) {
                                return false;
                            }
                            hidden = None;
                        }
                        if r.group && u.st_closed.contains(&r.path) {
                            hidden = Some(r.path.clone());
                        }
                        true
                    })
                    .collect();
                egui::ScrollArea::vertical()
                    .id_salt("st_rows")
                    .auto_shrink([false, false])
                    .show_rows(ui, ROW_H, visible.len(), |ui, range| {
                        dense_rows(ui);
                        for r in &visible[range] {
                            let resp = ui
                                .horizontal(|ui| {
                                    let ind = r.depth as f32 * 14.0;
                                    let (rect, _) = ui.allocate_exact_size(vec2(190.0, 18.0), Sense::hover());
                                    let p = ui.painter();
                                    if r.group {
                                        crate::codeview::chevron(p, pos2(rect.left() + ind + 6.0, rect.center().y), !u.st_closed.contains(&r.path), TEXT3());
                                    }
                                    let col = if r.missing {
                                        TEXT3()
                                    } else if r.group {
                                        VIOLET()
                                    } else {
                                        TEXT()
                                    };
                                    p.text(
                                        pos2(rect.left() + ind + 16.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        &r.name,
                                        FontId::new(12.0, FontFamily::Monospace),
                                        col,
                                    );
                                    cell(ui, 120.0, RichText::new(&r.ty).small().color(TEXT3()));
                                    cell(ui, 54.0, RichText::new(&r.off).font(MONO(11.5)).color(TEXT3()));
                                    cell(ui, 120.0, RichText::new(&r.hex).font(MONO(11.5)).color(TEXT2()));
                                    let v = if r.missing && !r.group { t("（数据不够）").to_string() } else { r.value.clone() };
                                    cell(ui, 200.0, RichText::new(v).font(MONO(12.0)).color(if r.missing { TEXT3() } else { ACCENT() }));
                                })
                                .response
                                .interact(Sense::click());
                            if r.group && resp.clicked() {
                                if !u.st_closed.remove(&r.path) {
                                    u.st_closed.insert(r.path.clone());
                                }
                            }
                            if !r.group && !r.missing {
                                resp.on_hover_text(t("右键复制")).context_menu(|ui| {
                                    if ui.button(t("复制值")).clicked() {
                                        ui.ctx().copy_text(r.value.clone());
                                        ui.close_menu();
                                    }
                                    if ui.button(t("复制 HEX")).clicked() {
                                        ui.ctx().copy_text(r.hex.clone());
                                        ui.close_menu();
                                    }
                                });
                            }
                        }
                    });
            });
        });
    }

    // ------------------------------------------------------------ CBOR / MsgPack / Protobuf

    fn tb_decode(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.y;
        let u = &mut self.tb.z.y;
        ui.horizontal(|ui| {
            for f in decode::Fmt::ALL {
                let mut on = c.dc_fmt == f;
                if toggle_chip(ui, &mut on, if f == decode::Fmt::Auto { t("自动识别") } else { f.label() }, "") {
                    c.dc_fmt = f;
                }
            }
            ui.add_space(12.0);
            small_chip(ui, &mut c.dc_b64, "Base64", t("输入是 Base64 文本"));
        });
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(&mut c.dc_in)
                .font(MONO(12.5))
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(if c.dc_b64 { "o2R0ZW1w..." } else { t("HEX 字节，也可以把文件拖进来") }),
        );
        let bytes = if c.dc_b64 { conv::base64_decode(c.dc_in.trim()) } else { conv::parse_hex(&c.dc_in) };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        if bytes.is_empty() {
            return;
        }
        let (fmt, res) = decode::decode(&bytes, c.dc_fmt);
        ui.add_space(8.0);
        let nodes = match res {
            Ok(n) => n,
            Err(e) => {
                ui.label(RichText::new(format!("{}: {e}", fmt.label())).color(ERR()));
                return;
            }
        };
        ui.horizontal(|ui| {
            pill(ui, fmt.label(), VIOLET());
            ui.label(RichText::new(tf("{} 字节", &[&bytes.len()])).small().color(TEXT3()));
            if fmt == decode::Fmt::Protobuf {
                ui.label(RichText::new(t("没有 .proto：字段只有编号，类型是猜的")).small().color(TEXT3()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("复制为 JSON")).clicked() {
                    ui.ctx().copy_text(decode::to_json(&nodes, fmt == decode::Fmt::Protobuf));
                }
            });
        });
        ui.add_space(4.0);
        let cols = [(t("键"), 220.0), (t("类型"), 130.0), (t("偏移"), 70.0), (t("值"), 420.0)];
        header_row(ui, &cols);
        let mut flat = vec![];
        fn walk<'a>(ns: &'a [Node], depth: usize, path: &str, closed: &HashSet<String>, out: &mut Vec<(usize, String, &'a Node)>) {
            for (i, n) in ns.iter().enumerate() {
                let p = format!("{path}/{i}");
                out.push((depth, p.clone(), n));
                if !n.kids.is_empty() && !closed.contains(&p) {
                    walk(&n.kids, depth + 1, &p, closed, out);
                }
            }
        }
        walk(&nodes, 0, "", &u.dc_closed, &mut flat);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, flat.len(), |ui, range| {
            dense_rows(ui);
            for (depth, path, n) in &flat[range] {
                let resp = ui
                    .horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(220.0, 18.0), Sense::hover());
                        let p = ui.painter();
                        let ind = *depth as f32 * 14.0;
                        if !n.kids.is_empty() {
                            crate::codeview::chevron(p, pos2(rect.left() + ind + 6.0, rect.center().y), !u.dc_closed.contains(path), TEXT3());
                        }
                        let key = if n.key.is_empty() { "·".to_string() } else { n.key.clone() };
                        p.text(
                            pos2(rect.left() + ind + 16.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            truncate(&key, 28),
                            FontId::new(12.0, FontFamily::Monospace),
                            series(*depth),
                        );
                        cell(ui, 130.0, RichText::new(&n.ty).small().color(TEXT3()));
                        cell(ui, 70.0, RichText::new(format!("{}+{}", n.off, n.len)).font(MONO(11.0)).color(TEXT3()));
                        ui.add(egui::Label::new(RichText::new(&n.val).font(MONO(12.0)).color(TEXT())).truncate());
                    })
                    .response
                    .interact(Sense::click());
                if !n.kids.is_empty() && resp.clicked() && !u.dc_closed.remove(path) {
                    u.dc_closed.insert(path.clone());
                }
                if n.kids.is_empty() {
                    let v = n.val.clone();
                    resp.context_menu(|ui| {
                        if ui.button(t("复制值")).clicked() {
                            ui.ctx().copy_text(v.clone());
                            ui.close_menu();
                        }
                    });
                }
            }
        });
    }

    // ------------------------------------------------------------ VCD

    fn tb_vcd(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.y;
        let u = &mut self.tb.z.y;
        ui.horizontal(|ui| {
            ui.set_max_width(ui.available_width());
            ui.allocate_ui(vec2((ui.available_width() - 330.0).max(300.0), 30.0), |ui| file_pick(ui, "VCD", &mut c.vcd_path, &["vcd"], false));
            if let Some(Ok(d)) = &u.vcd {
                if icon_button(ui, Icon::Maximize, t("适应窗口"), false).clicked() {
                    u.view = (d.start as f64, d.end.max(d.start + 1) as f64);
                }
            }
            if let (Some(a), Some(b), Some(Ok(d))) = (u.cur_a, u.cur_b, &u.vcd) {
                let dt = (b as f64 - a as f64).abs() * d.unit;
                ui.label(RichText::new(format!("Δ {}", vcd::fmt_time(dt))).font(MONO(13.0)).color(TEXT()));
                if dt > 0.0 {
                    ui.label(RichText::new(format!("{}", hwcalc_freq(1.0 / dt))).font(MONO(12.0)).color(TEXT3()));
                }
            }
        });
        let key = c.vcd_path.trim().to_string();
        if key != u.vcd_key {
            u.vcd_key = key.clone();
            u.vcd = (!key.is_empty()).then(|| std::fs::read(&key).map_err(|e| e.to_string()).and_then(|b| vcd::parse(&String::from_utf8_lossy(&b))));
            if let Some(Ok(d)) = &u.vcd {
                u.view = (d.start as f64, d.end.max(d.start + 1) as f64);
            }
            u.cur_a = None;
            u.cur_b = None;
            u.vcd_sel = None;
        }
        let Some(d) = &u.vcd else {
            ui.add_space(8.0);
            ui.label(RichText::new(t("PulseView / sigrok、Saleae（导出 VCD）、Verilog 仿真都能生成 VCD 文件")).color(TEXT3()));
            return;
        };
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut u.vcd_filter).hint_text(t("筛选信号")).desired_width(180.0));
            ui.label(
                RichText::new(t("滚轮缩放，拖动平移；左键放光标 A，右键放光标 B；选中信号后按 ← → 跳到上一个 / 下一个边沿"))
                    .small()
                    .color(TEXT3()),
            );
        });
        ui.add_space(4.0);
        let q = u.vcd_filter.to_lowercase();
        let sigs: Vec<usize> = (0..d.signals.len()).filter(|i| q.is_empty() || d.signals[*i].name.to_lowercase().contains(&q)).collect();
        let row_h = 28.0;
        let name_w = 210.0;
        let ruler = 22.0;
        let avail = ui.available_size();
        let h = (ruler + sigs.len() as f32 * row_h).min(avail.y - 4.0).max(80.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(avail.x, h), Sense::click_and_drag());
        let wave = Rect::from_min_max(pos2(rect.left() + name_w, rect.top()), rect.max);
        let p = ui.painter_at(rect);
        p.rect_filled(rect, R_CARD, BG0());
        let (t0, t1) = u.view;
        let span = (t1 - t0).max(1e-9);
        let x_of = |t: f64| wave.left() + ((t - t0) / span) as f32 * wave.width();
        let t_of = |x: f32| t0 + ((x - wave.left()) / wave.width()) as f64 * span;
        // 缩放和平移
        if let Some(hp) = resp.hover_pos().filter(|h| wave.contains(*h)) {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.smooth_scroll_delta.x);
            if scroll != 0.0 {
                let k = (-scroll as f64 / 200.0).exp();
                let at = t_of(hp.x);
                let full = (d.end - d.start).max(1) as f64;
                let ns = (span * k).clamp(1.0, full * 4.0);
                let f = (at - t0) / span;
                u.view = (at - ns * f, at - ns * f + ns);
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            let dx = resp.drag_delta().x as f64;
            let dt = dx / wave.width() as f64 * span;
            u.view = (u.view.0 - dt, u.view.1 - dt);
        }
        let snap = |x: f32| -> u64 { t_of(x).round().max(0.0) as u64 };
        if let Some(pp) = resp.interact_pointer_pos().filter(|pp| wave.contains(*pp)) {
            if resp.clicked() {
                u.cur_a = Some(snap(pp.x));
            }
            if resp.secondary_clicked() {
                u.cur_b = Some(snap(pp.x));
            }
        }
        // 选中信号点名字
        if let Some(pp) = resp.interact_pointer_pos().filter(|pp| pp.x < wave.left() && pp.y > rect.top() + ruler) {
            if resp.clicked() {
                let k = ((pp.y - rect.top() - ruler) / row_h) as usize;
                u.vcd_sel = sigs.get(k).copied();
            }
        }
        if let Some(si) = u.vcd_sel {
            let (l, r) = ui.input(|i| (i.key_pressed(Key::ArrowLeft), i.key_pressed(Key::ArrowRight)));
            if l || r {
                let from = u.cur_a.unwrap_or(t0 as u64);
                if let Some(e) = d.signals[si].edge(from, r) {
                    u.cur_a = Some(e);
                    let ex = e as f64;
                    if ex < u.view.0 || ex > u.view.1 {
                        let half = span / 2.0;
                        u.view = (ex - half, ex + half);
                    }
                }
            }
        }
        // 时间刻度
        let target = span / (wave.width() as f64 / 110.0).max(1.0);
        let mag = 10f64.powf(target.log10().floor());
        let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= target).unwrap_or(mag * 10.0);
        let mut tt = (t0 / step).ceil() * step;
        let font = FontId::new(10.0, FontFamily::Monospace);
        while tt <= t1 {
            let x = x_of(tt);
            p.line_segment([pos2(x, rect.top() + ruler - 5.0), pos2(x, rect.bottom())], st(1.0, LINE().gamma_multiply(0.6)));
            p.text(pos2(x + 3.0, rect.top() + 4.0), Align2::LEFT_TOP, vcd::fmt_time(tt * d.unit), font.clone(), TEXT3());
            tt += step;
        }
        p.line_segment([pos2(rect.left(), rect.top() + ruler), pos2(rect.right(), rect.top() + ruler)], st(1.0, LINE()));
        p.line_segment([pos2(wave.left(), rect.top()), pos2(wave.left(), rect.bottom())], st(1.0, LINE()));
        let cur_val_t = u.cur_a.unwrap_or(t1 as u64);
        for (k, &si) in sigs.iter().enumerate() {
            let s = &d.signals[si];
            let top = rect.top() + ruler + k as f32 * row_h;
            if top > rect.bottom() {
                break;
            }
            let row = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), row_h));
            if u.vcd_sel == Some(si) {
                p.rect_filled(row, 0.0, ACCENT().gamma_multiply(0.07));
            }
            p.line_segment([pos2(rect.left(), row.bottom()), pos2(rect.right(), row.bottom())], st(1.0, LINE().gamma_multiply(0.4)));
            let col = series(si);
            p.text(
                pos2(rect.left() + 10.0, row.center().y),
                Align2::LEFT_CENTER,
                truncate(&s.name, 20),
                FontId::new(12.0, FontFamily::Monospace),
                TEXT(),
            );
            if let Some(i) = s.at(cur_val_t) {
                p.text(pos2(wave.left() - 8.0, row.center().y), Align2::RIGHT_CENTER, s.fmt(i), FontId::new(11.5, FontFamily::Monospace), col);
            }
            draw_signal(&p, s, row, wave, t0, t1, &x_of, col);
        }
        for (cur, col) in [(u.cur_a, ACCENT()), (u.cur_b, VIOLET())] {
            if let Some(cv) = cur {
                let x = x_of(cv as f64);
                if x >= wave.left() && x <= wave.right() {
                    p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], st(1.2, col));
                    // 标签带底色，盖住下面的刻度文字
                    let g = p.layout_no_wrap(vcd::fmt_time(cv as f64 * d.unit), font.clone(), col);
                    let at = pos2(x + 1.0, rect.top() + ruler - 2.0 - g.size().y);
                    p.rect_filled(Rect::from_min_size(at, g.size() + vec2(6.0, 0.0)), 2.0, BG2());
                    p.galley(at + vec2(3.0, 0.0), g, col);
                }
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Crosshair });
        }
    }

    // ------------------------------------------------------------ 二维码

    fn tb_qr(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.y;
        let u = &mut self.tb.z.y;
        ui.allocate_ui(vec2(260.0, 30.0), |ui| tab_strip(ui, &mut c.qr_wifi, &[(false, t("文字 / 网址")), (true, t("Wi-Fi"))], 28.0));
        ui.add_space(10.0);
        settings_card(ui, |ui| {
            if c.qr_wifi {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SSID").color(TEXT2()));
                    ui.add_sized(vec2(200.0, 24.0), egui::TextEdit::singleline(&mut c.qr_ssid));
                    ui.add_space(8.0);
                    ui.label(RichText::new(t("密码")).color(TEXT2()));
                    ui.add_sized(vec2(180.0, 24.0), egui::TextEdit::singleline(&mut u.qr_pass).password(true));
                });
                ui.horizontal(|ui| {
                    for (i, n) in [(0u8, "WPA/WPA2/WPA3"), (1, "WEP"), (2, t("不加密"))] {
                        let mut on = c.qr_sec == i;
                        if toggle_chip(ui, &mut on, n, "") {
                            c.qr_sec = i;
                        }
                    }
                    toggle_chip(ui, &mut c.qr_hidden, t("隐藏网络"), "");
                });
                ui.label(RichText::new(t("密码只在这次运行时保留，不会存进设置文件")).small().color(TEXT3()));
            } else {
                ui.add(egui::TextEdit::multiline(&mut c.qr_text).desired_rows(3).desired_width(f32::INFINITY));
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("纠错")).color(TEXT2()));
                for (i, n) in [(0u8, "L 7%"), (1, "M 15%"), (2, "Q 25%"), (3, "H 30%")] {
                    let mut on = c.qr_ec == i;
                    if toggle_chip(ui, &mut on, n, "") {
                        c.qr_ec = i;
                    }
                }
            });
        });
        let esc = |s: &str| {
            s.chars().fold(String::new(), |mut a, ch| {
                if matches!(ch, '\\' | ';' | ',' | ':' | '"') {
                    a.push('\\');
                }
                a.push(ch);
                a
            })
        };
        let payload = if c.qr_wifi {
            let sec = ["WPA", "WEP", "nopass"][c.qr_sec.min(2) as usize];
            let pass = if c.qr_sec == 2 { String::new() } else { format!("P:{};", esc(&u.qr_pass)) };
            format!("WIFI:T:{sec};S:{};{pass}{};", esc(&c.qr_ssid), if c.qr_hidden { "H:true;" } else { "" })
        } else {
            c.qr_text.clone()
        };
        let key = format!("{}|{payload}", c.qr_ec);
        if key != u.qr_key {
            u.qr_key = key;
            let ec = [qrcode::EcLevel::L, qrcode::EcLevel::M, qrcode::EcLevel::Q, qrcode::EcLevel::H][c.qr_ec.min(3) as usize];
            match qrcode::QrCode::with_error_correction_level(payload.as_bytes(), ec) {
                Ok(code) => {
                    let w = code.width();
                    let dark: Vec<bool> = code.to_colors().iter().map(|c| *c == qrcode::Color::Dark).collect();
                    // 四周留 4 格空白，每格放大 8 倍做纹理
                    let q = 4;
                    let n = w + 2 * q;
                    let scale = 8;
                    let mut img = egui::ColorImage::new([n * scale, n * scale], Color32::WHITE);
                    for y in 0..w {
                        for x in 0..w {
                            if dark[y * w + x] {
                                for dy in 0..scale {
                                    for dx in 0..scale {
                                        img.pixels[((y + q) * scale + dy) * n * scale + (x + q) * scale + dx] = Color32::BLACK;
                                    }
                                }
                            }
                        }
                    }
                    u.qr_tex = Some(ui.ctx().load_texture("qr", img, egui::TextureOptions::NEAREST));
                    u.qr_mod = Some((w, dark));
                    u.qr_err = None;
                }
                Err(e) => {
                    u.qr_tex = None;
                    u.qr_mod = None;
                    u.qr_err = Some(e.to_string());
                }
            }
        }
        ui.add_space(12.0);
        if let Some(e) = &u.qr_err {
            ui.label(RichText::new(e).color(ERR()));
            return;
        }
        let (Some(tex), Some((w, dark))) = (&u.qr_tex, &u.qr_mod) else { return };
        let mut msg = None;
        ui.horizontal_top(|ui| {
            let side = (ui.available_height() - 10.0).clamp(160.0, 300.0);
            ui.add(egui::Image::new((tex.id(), vec2(side, side))).corner_radius(R_CARD));
            ui.add_space(16.0);
            ui.vertical(|ui| {
                stat(ui, t("尺寸"), &format!("{w} × {w}"), Some(RichText::new(tf("版本 {}", &[&((w - 17) / 4)])).small().color(TEXT3())));
                ui.add_space(6.0);
                stat(ui, t("内容"), &tf("{} 字节", &[&payload.len()]), None);
                ui.add_space(10.0);
                if ui.button(t("保存 PNG…")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().set_file_name("qrcode.png").save_file() {
                        let q = 4;
                        let n = (w + 2 * q) as u32;
                        let scale = 10u32;
                        let img = image::GrayImage::from_fn(n * scale, n * scale, |x, y| {
                            let (mx, my) = ((x / scale) as i64 - q as i64, (y / scale) as i64 - q as i64);
                            let on = mx >= 0 && my >= 0 && (mx as usize) < *w && (my as usize) < *w && dark[my as usize * w + mx as usize];
                            image::Luma([if on { 0 } else { 255 }])
                        });
                        msg = Some(match img.save(&path) {
                            Ok(_) => (tf("已保存：{}", &[&path.display()]), OK()),
                            Err(e) => (e.to_string(), ERR()),
                        });
                    }
                }
                if ui.button(t("复制为 C 数组")).on_hover_text(t("每行按位打包，高位在左，1 = 黑；给 LCD / OLED 显示用")).clicked() {
                    let bpr = w.div_ceil(8);
                    let mut s = format!("/* QR {w}x{w}, {bpr} bytes per row, MSB first, 1 = dark */\nstatic const uint8_t qr_code[{}] = {{\n", bpr * w);
                    for y in 0..*w {
                        s.push_str("    ");
                        for b in 0..bpr {
                            let mut v = 0u8;
                            for k in 0..8 {
                                let x = b * 8 + k;
                                if x < *w && dark[y * w + x] {
                                    v |= 0x80 >> k;
                                }
                            }
                            s.push_str(&format!("0x{v:02X}, "));
                        }
                        s.push('\n');
                    }
                    s.push_str("};\n");
                    ui.ctx().copy_text(s);
                    msg = Some((t("已复制").to_string(), OK()));
                }
            });
        });
        if let Some((m, col)) = msg {
            self.toast(m, col);
        }
    }
}

fn hwcalc_freq(f: f64) -> String {
    crate::toolbox::hwcalc::fmt_si(f, "Hz")
}

/// 画一条信号：1 位画高低电平，多位画总线块并写值
#[allow(clippy::too_many_arguments)]
fn draw_signal(p: &egui::Painter, s: &vcd::Signal, row: Rect, wave: Rect, t0: f64, t1: f64, x_of: &dyn Fn(f64) -> f32, col: Color32) {
    let (hi, lo) = (row.top() + 6.0, row.bottom() - 6.0);
    let start = s.changes.partition_point(|c| (c.0 as f64) < t0).saturating_sub(1);
    let end = s.changes.partition_point(|c| (c.0 as f64) <= t1);
    if start >= s.changes.len() {
        return;
    }
    let font = FontId::new(10.5, FontFamily::Monospace);
    let mut busy_from: Option<f32> = None;
    let mut line: Vec<egui::Pos2> = vec![];
    let mut i = start;
    while i < end.max(start + 1).min(s.changes.len()) {
        let (t, v) = s.changes[i];
        let x0 = x_of(t as f64).max(wave.left());
        let x1 = s.changes.get(i + 1).map(|n| x_of(n.0 as f64)).unwrap_or(wave.right()).min(wave.right());
        i += 1;
        if x1 - x0 < 1.0 {
            // 太密：合成一块
            busy_from.get_or_insert(x0);
            continue;
        }
        if let Some(bx) = busy_from.take() {
            p.rect_filled(Rect::from_min_max(pos2(bx, hi), pos2(x0, lo)), 0.0, col.gamma_multiply(0.45));
            if s.width == 1 && !line.is_empty() {
                p.add(Shape::line(std::mem::take(&mut line), st(1.4, col)));
            }
        }
        if v.xz {
            p.rect_filled(Rect::from_min_max(pos2(x0, hi), pos2(x1, lo)), 0.0, ERR().gamma_multiply(0.25));
            if s.width == 1 && !line.is_empty() {
                p.add(Shape::line(std::mem::take(&mut line), st(1.4, col)));
            }
            continue;
        }
        if s.width == 1 && !s.real {
            let y = if v.v == 1 { hi } else { lo };
            if let Some(last) = line.last() {
                line.push(pos2(x0, last.y));
            }
            line.push(pos2(x0, y));
            line.push(pos2(x1, y));
            if v.v == 1 {
                p.rect_filled(Rect::from_min_max(pos2(x0, hi), pos2(x1, lo)), 0.0, col.gamma_multiply(0.08));
            }
        } else {
            let mid = row.center().y;
            let e = 3.0f32.min((x1 - x0) / 2.0);
            let pts = vec![pos2(x0, mid), pos2(x0 + e, hi), pos2(x1 - e, hi), pos2(x1, mid), pos2(x1 - e, lo), pos2(x0 + e, lo)];
            p.add(Shape::convex_polygon(pts, col.gamma_multiply(0.12), st(1.0, col)));
            let txt = s.fmt(i - 1);
            if (x1 - x0) > txt.len() as f32 * 6.5 + 8.0 {
                p.text(pos2((x0 + x1) / 2.0, mid), Align2::CENTER_CENTER, txt, font.clone(), TEXT());
            }
        }
    }
    if let Some(bx) = busy_from {
        p.rect_filled(Rect::from_min_max(pos2(bx, hi), pos2(wave.right(), lo)), 0.0, col.gamma_multiply(0.45));
    }
    if line.len() > 1 {
        p.add(Shape::line(line, st(1.4, col)));
    }
}

impl App {
    /// 「电路和接线」页
    pub(super) fn circuit_page(&mut self, ctx: &egui::Context) {
        let root = self.code.root().map(|r| r.to_path_buf());
        let cv = self.circ.get_or_insert_with(|| Box::new(crate::circuitview::CircuitView::new()));
        cv.project = root;
        cv.ui(ctx);
        for e in std::mem::take(&mut cv.events) {
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
            }
        }
    }
}

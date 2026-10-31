//! 工具箱 v1.6：固件处理（转换合并、打包头、体积对比、栈分析、向量表）。
//! 芯片 / 工程、硬件计算和数据工具在 toolbox4.rs、toolbox5.rs；三个文件共用这里的设置和状态。

use crate::toolbox::{fw, sizediff, stack};

use super::logx::small_chip;
use super::toolbox::{cell, copy_label, dense_rows, header_row, settings_card, Tool, MONO, ROW_H};
use super::*;

/// 记在设置里的部分
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cfg3 {
    pub fw_inputs: Vec<fw::Input>,
    pub fw_post: fw::Post,
    pub fw_fmt: fw::Fmt,
    pub fw_split: String,
    pub pk_input: String,
    pub pk_base: String,
    pub pk: fw::PackTpl,
    pub pk_tpls: Vec<fw::PackTpl>,
    pub pk_fmt: fw::Fmt,
    pub sz_old: String,
    pub sz_new: String,
    pub st_dir: String,
    pub st_elf: String,
    pub vt_elf: String,
    pub x: super::toolbox4::Cfg4,
    pub y: super::toolbox5::Cfg5,
}

impl Default for Cfg3 {
    fn default() -> Self {
        Cfg3 {
            fw_inputs: vec![],
            fw_post: Default::default(),
            fw_fmt: fw::Fmt::Hex,
            fw_split: String::new(),
            pk_input: String::new(),
            pk_base: "0x08000000".into(),
            pk: Default::default(),
            pk_tpls: fw::presets(),
            pk_fmt: fw::Fmt::Bin,
            sz_old: String::new(),
            sz_new: String::new(),
            st_dir: String::new(),
            st_elf: String::new(),
            vt_elf: String::new(),
            x: Default::default(),
            y: Default::default(),
        }
    }
}

/// 运行时状态
#[derive(Default)]
pub struct Ui3 {
    fw_key: String,
    fw: Option<fw::Merged>,
    pk_key: String,
    pk_img: Option<Result<(u64, Vec<u8>), String>>,
    sz_key: String,
    sz: Option<Result<sizediff::Diff, String>>,
    sz_files: bool,
    sz_filter: String,
    st: Option<(stack::Graph, Vec<stack::Entry>)>,
    st_note: String,
    st_sel: Option<String>,
    st_all: bool,
    st_filter: String,
    /// 拖入文件后自动重新分析
    st_auto: bool,
    vt_key: String,
    vt: Option<Result<fw::VecTable, String>>,
    vt_default_only: bool,
    post_key: String,
    post: Option<Result<Vec<(u64, Vec<u8>)>, String>>,
    pub(super) x: super::toolbox4::Ui4,
    pub(super) y: super::toolbox5::Ui5,
}

pub(super) fn tool_text3(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::FwConv => (t("固件转换 / 合并"), t("HEX、BIN、SREC、ELF 互相转换；把 Bootloader、App、参数区合成一个文件，或者按地址拆开")),
        Tool::FwPack => (t("升级包打包"), t("给固件加自定义包头（魔数、版本、长度、CRC32、时间），或者在指定地址写入 CRC")),
        Tool::SizeDiff => (t("体积对比"), t("比较两次编译的 .map 或 .elf：哪些函数、变量、文件变大了")),
        Tool::Stack => (t("栈用量分析"), t("读 GCC -fstack-usage 的 .su 文件和调用关系，算出每个任务和中断的最大栈深度")),
        Tool::Vectors => (t("中断向量表"), t("从 ELF 读出向量表，列出每个中断的处理函数，标出还指向默认处理函数的")),
        other => super::toolbox4::tool_text4(other),
    }
}

/// 一行：标签 + 路径输入框 + 选择按钮。返回是否改了
/// 文件行左边标签的宽度（德语、俄语的标签也放得下）
pub(super) const LABEL_W: f32 = 96.0;

pub(super) fn file_pick(ui: &mut egui::Ui, label: &str, path: &mut String, exts: &[&str], folder: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, 26.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), TEXT2());
        let w = ui.available_width() - 80.0;
        changed |= ui
            .add_sized(vec2(w, 26.0), egui::TextEdit::singleline(path).font(MONO(12.5)).margin(vec2(6.0, 4.0)).hint_text(t("选择或拖入文件")))
            .changed();
        if ui.add(egui::Button::new(RichText::new(t("选择…")).size(12.0)).min_size(vec2(70.0, 26.0))).clicked() {
            let mut d = rfd::FileDialog::new();
            if let Some(dir) = std::path::Path::new(path.as_str()).parent().filter(|d| d.is_dir()) {
                d = d.set_directory(dir);
            }
            let picked = if folder {
                d.pick_folder()
            } else {
                if !exts.is_empty() {
                    d = d.add_filter(label, exts);
                }
                d.pick_file()
            };
            if let Some(p) = picked {
                *path = p.to_string_lossy().into_owned();
                changed = true;
            }
        }
    });
    changed
}

pub(super) fn hx(v: u64) -> String {
    format!("0x{v:08X}")
}

/// 带正负号和颜色的变化量
fn delta_text(d: i64) -> RichText {
    let s = match d.cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{d}"),
        std::cmp::Ordering::Less => d.to_string(),
        _ => "0".into(),
    };
    RichText::new(s).font(MONO(12.0)).color(match d.cmp(&0) {
        std::cmp::Ordering::Greater => ERR(),
        std::cmp::Ordering::Less => OK(),
        _ => TEXT3(),
    })
}

/// 大字统计块
pub(super) fn stat(ui: &mut egui::Ui, label: &str, value: &str, note: Option<RichText>) {
    egui::Frame::new()
        .fill(BG2())
        .stroke(st(1.0, LINE()))
        .corner_radius(R_CARD)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_min_width(130.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(label).small().color(TEXT3()));
                ui.label(RichText::new(value).font(FontId::new(18.0, mono_bold())).color(TEXT()));
                if let Some(n) = note {
                    ui.label(n);
                }
            });
        });
}

/// 区段颜色
pub(super) fn series(i: usize) -> Color32 {
    [ACCENT(), VIOLET(), OK(), WARN(), Color32::from_rgb(0x60, 0xA5, 0xFA), Color32::from_rgb(0xF4, 0x72, 0xB6)][i % 6]
}

/// 地址分布图：几簇不相连的地址范围之间画断口，每个输入一行色条
fn addr_map(ui: &mut egui::Ui, clusters: &[(u64, u64)], rows: &[(String, Color32, u64, u64)]) {
    if clusters.is_empty() {
        return;
    }
    let w = ui.available_width();
    let gap = 18.0;
    let row_h = 14.0;
    let h = rows.len() as f32 * (row_h + 4.0) + 20.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let p = ui.painter();
    let usable = w - gap * (clusters.len() - 1) as f32;
    let total: f64 = clusters.iter().map(|c| (c.1 - c.0) as f64).sum();
    // 每簇至少 40 像素，剩下的按大小分
    let min_w = 40.0f32.min(usable / clusters.len() as f32);
    let spare = usable - min_w * clusters.len() as f32;
    let mut xs = vec![];
    let mut x = rect.left();
    for c in clusters {
        let cw = min_w + spare * ((c.1 - c.0) as f64 / total.max(1.0)) as f32;
        xs.push((x, cw));
        x += cw + gap;
    }
    let to_x = |a: u64| -> Option<f32> {
        let i = clusters.iter().position(|c| a >= c.0 && a <= c.1)?;
        let (x0, cw) = xs[i];
        let c = clusters[i];
        Some(x0 + cw * ((a - c.0) as f64 / (c.1 - c.0).max(1) as f64) as f32)
    };
    for (i, (x0, cw)) in xs.iter().enumerate() {
        let r = Rect::from_min_size(pos2(*x0, rect.top()), vec2(*cw, rows.len() as f32 * (row_h + 4.0)));
        p.rect_filled(r, 3.0, BG0());
        let font = FontId::new(10.0, FontFamily::Monospace);
        p.text(pos2(*x0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, format!("{:X}", clusters[i].0), font.clone(), TEXT3());
        if *cw > 110.0 {
            p.text(pos2(x0 + cw, rect.bottom() - 2.0), Align2::RIGHT_BOTTOM, format!("{:X}", clusters[i].1), font, TEXT3());
        }
        if i > 0 {
            let gx = x0 - gap / 2.0;
            p.line_segment([pos2(gx - 3.0, rect.top() + 4.0), pos2(gx + 3.0, rect.top() + 10.0)], st(1.0, TEXT3()));
            p.line_segment([pos2(gx - 3.0, rect.top() + 10.0), pos2(gx + 3.0, rect.top() + 16.0)], st(1.0, TEXT3()));
        }
    }
    for (k, (_, col, lo, hi)) in rows.iter().enumerate() {
        let y = rect.top() + k as f32 * (row_h + 4.0) + 2.0;
        if let (Some(a), Some(b)) = (to_x(*lo), to_x(*hi)) {
            let r = Rect::from_min_max(pos2(a, y), pos2((b).max(a + 3.0), y + row_h));
            p.rect_filled(r, 3.0, col.gamma_multiply(0.75));
        }
    }
}

impl App {
    pub(super) fn tb_v16(&mut self, ui: &mut egui::Ui, tool: Tool) {
        match tool {
            Tool::FwConv => self.tb_fwconv(ui),
            Tool::FwPack => self.tb_fwpack(ui),
            Tool::SizeDiff => self.tb_sizediff(ui),
            Tool::Stack => self.tb_stack(ui),
            Tool::Vectors => self.tb_vectors(ui),
            other => self.tb_v16b(ui, other),
        }
    }

    /// 拖进工具箱页的文件交给当前工具
    pub(super) fn tool_drop(&mut self, p: PathBuf) {
        if self.s.tb.tool == Tool::Ssh {
            return self.ssh_drop(vec![p]);
        }
        if self.s.tb.tool == Tool::Circuit {
            if let Some(cv) = &mut self.circ {
                cv.drop_file(&p);
            }
            return;
        }
        let s = p.to_string_lossy().into_owned();
        let c = &mut self.s.tb.z;
        match self.s.tb.tool {
            Tool::FwConv => {
                c.fw_inputs.push(fw::Input {
                    path: s,
                    addr: String::new(),
                    on: true,
                });
            }
            Tool::FwPack => c.pk_input = s,
            Tool::SizeDiff => {
                if c.sz_old.is_empty() || !c.sz_new.is_empty() && c.sz_old != s {
                    if c.sz_old.is_empty() {
                        c.sz_old = s;
                    } else {
                        c.sz_old = std::mem::replace(&mut c.sz_new, s);
                    }
                } else {
                    c.sz_new = s;
                }
            }
            Tool::Stack => {
                self.tb.z.st_auto = true;
                if p.is_dir() {
                    c.st_dir = s;
                } else {
                    c.st_elf = s;
                }
            }
            Tool::Vectors => c.vt_elf = s,
            other => self.tool_drop4(other, p),
        }
    }

    // ------------------------------------------------------------ 转换 / 合并

    fn tb_fwconv(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z;
        let u = &mut self.tb.z;
        let mut toast: Option<(String, Color32)> = None;
        let key = serde_json::to_string(&c.fw_inputs).unwrap_or_default();
        if key != u.fw_key {
            u.fw_key = key;
            u.fw = (!c.fw_inputs.is_empty()).then(|| fw::merge(&c.fw_inputs));
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            eyebrow(ui, t("输入文件"), t("后面的文件覆盖前面的"));
            let mut remove = None;
            let mut swap = None;
            let n = c.fw_inputs.len();
            settings_card(ui, |ui| {
                if c.fw_inputs.is_empty() {
                    ui.label(RichText::new(t("添加或拖入 HEX、BIN、SREC、ELF 文件。只放一个文件就是格式转换，放多个就是合并")).color(TEXT3()));
                }
                for (i, inp) in c.fw_inputs.iter_mut().enumerate() {
                    let info = u.fw.as_ref().and_then(|m| m.inputs.get(i));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut inp.on, "");
                        let (r, _) = ui.allocate_exact_size(vec2(4.0, 18.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, series(i));
                        let name = std::path::Path::new(&inp.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.allocate_ui_with_layout(vec2(170.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.set_min_width(170.0);
                            ui.add(egui::Label::new(RichText::new(name).font(MONO(12.5)).color(TEXT())).truncate()).on_hover_text(&inp.path);
                        });
                        match info {
                            Some(Ok(l)) => {
                                pill(ui, l.kind, VIOLET());
                                ui.label(RichText::new(format!("{} – {}  {}", hx(l.lo), hx(l.hi), human_bytes(l.size))).font(MONO(11.5)).color(TEXT2()));
                            }
                            Some(Err(e)) if !e.is_empty() => {
                                ui.label(RichText::new(truncate(e, 60)).color(ERR()));
                            }
                            _ => {}
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, Icon::Close, t("移除"), false).clicked() {
                                remove = Some(i);
                            }
                            if i > 0 && icon_button(ui, Icon::Swap, t("和上一个交换顺序"), false).clicked() {
                                swap = Some(i);
                            }
                            ui.add_sized(vec2(110.0, 24.0), egui::TextEdit::singleline(&mut inp.addr).font(MONO(12.0)).hint_text("0x0"));
                            let bin = fw::is_bin(&inp.path);
                            ui.label(RichText::new(if bin { t("放到地址") } else { t("地址偏移") }).small().color(TEXT3())).on_hover_text(if bin {
                                t("BIN 文件没有地址信息，写它放在哪个地址")
                            } else {
                                t("整体平移，比如 -0x08000000 或 0x4000")
                            });
                        });
                    });
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(t("添加文件…")).clicked() {
                        if let Some(ps) = rfd::FileDialog::new()
                            .add_filter(t("固件"), &["hex", "ihex", "bin", "srec", "s19", "s28", "s37", "mot", "elf", "axf", "out"])
                            .add_filter("*", &["*"])
                            .pick_files()
                        {
                            for p in ps {
                                c.fw_inputs.push(fw::Input {
                                    path: p.to_string_lossy().into_owned(),
                                    addr: String::new(),
                                    on: true,
                                });
                            }
                        }
                    }
                    if n > 0 && ui.button(t("清空")).clicked() {
                        c.fw_inputs.clear();
                    }
                    if n > 0 && icon_button(ui, Icon::Refresh, t("重新读取文件"), false).clicked() {
                        u.fw_key.clear();
                    }
                });
            });
            if let Some(i) = remove {
                c.fw_inputs.remove(i);
            }
            if let Some(i) = swap {
                c.fw_inputs.swap(i, i - 1);
            }
            let Some(m) = &u.fw else { return };
            if m.img.segs.is_empty() {
                return;
            }
            ui.add_space(12.0);
            // 地址分布
            let clusters: Vec<(u64, u64)> = m.img.segs.iter().map(|s| (s.0, s.0 + s.1.len() as u64)).collect();
            let rows: Vec<(String, Color32, u64, u64)> = m
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.as_ref().ok().map(|l| (i.to_string(), series(i), l.lo, l.hi)))
                .collect();
            eyebrow(ui, t("地址分布"), &tf("{} 段", &[&m.img.segs.len()]));
            addr_map(ui, &clusters, &rows);
            for (i, over) in &m.overlaps {
                ui.label(RichText::new(tf("第 {} 个文件和前面的重叠了 {} 字节（以后面的为准）", &[&(i + 1), &over])).color(WARN()));
            }
            ui.add_space(10.0);
            eyebrow(ui, t("输出"), "");
            let p = &mut c.fw_post;
            settings_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("格式")).color(TEXT2()));
                    for f in fw::Fmt::ALL {
                        let mut on = c.fw_fmt == f;
                        if toggle_chip(ui, &mut on, f.label(), "") {
                            c.fw_fmt = f;
                        }
                    }
                    ui.add_space(14.0);
                    toggle_chip(ui, &mut p.fill_gaps, t("填充空隙"), t("段和段之间的空白用填充字节补上，合成一整段"));
                    ui.label(RichText::new(t("填充")).color(TEXT2()));
                    ui.add(egui::DragValue::new(&mut p.fill).hexadecimal(2, false, true));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("只取范围")).color(TEXT2()));
                    ui.add_sized(vec2(110.0, 24.0), egui::TextEdit::singleline(&mut p.from).font(MONO(12.0)).hint_text(t("起始")));
                    ui.label("–");
                    ui.add_sized(vec2(110.0, 24.0), egui::TextEdit::singleline(&mut p.to).font(MONO(12.0)).hint_text(t("结束（不含）")));
                    ui.add_space(10.0);
                    ui.label(RichText::new(t("末尾对齐")).color(TEXT2()));
                    egui::ComboBox::from_id_salt("fw_align")
                        .width(80.0)
                        .selected_text(if p.align == 0 { t("不对齐").to_string() } else { p.align.to_string() })
                        .show_ui(ui, |ui| {
                            for a in [0u32, 4, 8, 16, 256, 1024, 2048, 4096] {
                                ui.selectable_value(&mut p.align, a, if a == 0 { t("不对齐").to_string() } else { a.to_string() });
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("按地址拆分")).color(TEXT2()));
                    ui.add_sized(vec2(260.0, 24.0), egui::TextEdit::singleline(&mut c.fw_split).font(MONO(12.0)).hint_text("0x08008000, 0x08040000"));
                    ui.label(RichText::new(t("留空就输出一个文件")).small().color(TEXT3()));
                });
            });
            // 填充 / 裁剪可能产生几 MB 数据，只在输入或选项变了时重算
            let pk = format!("{}|{}", u.fw_key, serde_json::to_string(&*p).unwrap_or_default());
            if pk != u.post_key || u.post.is_none() {
                u.post_key = pk;
                u.post = Some(fw::post(&m.img, p));
            }
            let out = u.post.as_ref().unwrap();
            ui.add_space(8.0);
            match &out {
                Err(e) => {
                    ui.label(RichText::new(e).color(ERR()));
                }
                Ok(segs) => {
                    let size: u64 = segs.iter().map(|s| s.1.len() as u64).sum();
                    let (lo, hi) = (segs.first().map_or(0, |s| s.0), segs.last().map_or(0, |s| s.0 + s.1.len() as u64));
                    ui.horizontal_wrapped(|ui| {
                        stat(ui, t("范围"), &format!("{:X}–{:X}", lo, hi), None);
                        stat(ui, t("数据"), &human_bytes(size), Some(RichText::new(tf("{} 字节", &[&size])).small().color(TEXT3())));
                        stat(
                            ui,
                            "CRC32",
                            &format!("{:08X}", crate::binview::parse::crc32(segs)),
                            Some(RichText::new(t("按地址顺序算")).small().color(TEXT3())),
                        );
                    });
                    ui.add_space(8.0);
                    let cuts: Vec<u64> = c
                        .fw_split
                        .split([',', ' ', ';'])
                        .filter(|s| !s.trim().is_empty())
                        .filter_map(|s| fw::parse_num(s).map(|v| v as u64))
                        .collect();
                    let start = m.img.start;
                    ui.horizontal(|ui| {
                        let label = if cuts.is_empty() { t("保存…") } else { t("拆分保存到文件夹…") };
                        if ui
                            .add(egui::Button::new(RichText::new(label).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(120.0, 30.0)))
                            .clicked()
                        {
                            let fmt = c.fw_fmt;
                            let stem = c
                                .fw_inputs
                                .iter()
                                .find(|i| i.on)
                                .and_then(|i| std::path::Path::new(&i.path).file_stem().map(|s| s.to_string_lossy().into_owned()))
                                .unwrap_or_else(|| "firmware".into());
                            let res = if cuts.is_empty() {
                                let name = if c.fw_inputs.iter().filter(|i| i.on).count() > 1 {
                                    format!("{stem}_merged.{}", fmt.ext())
                                } else {
                                    format!("{stem}.{}", fmt.ext())
                                };
                                rfd::FileDialog::new()
                                    .set_file_name(name)
                                    .save_file()
                                    .map(|path| std::fs::write(&path, fw::write(segs, fmt, p.fill, start)).map(|_| path.display().to_string()))
                            } else {
                                rfd::FileDialog::new().pick_folder().map(|dir| {
                                    let parts = fw::split(segs, &cuts);
                                    for (a, _, s) in &parts {
                                        std::fs::write(dir.join(format!("{stem}_{a:08X}.{}", fmt.ext())), fw::write(s, fmt, p.fill, None))?;
                                    }
                                    Ok(tf("已保存 {} 个文件", &[&parts.len()]))
                                })
                            };
                            match res {
                                Some(Ok(m)) => toast = Some((tf("已保存：{}", &[&m]), OK())),
                                Some(Err(e)) => toast = Some((e.to_string(), ERR())),
                                None => {}
                            }
                        }
                        if !cuts.is_empty() {
                            let parts = fw::split(segs, &cuts);
                            for (a, b, s) in &parts {
                                let n: usize = s.iter().map(|x| x.1.len()).sum();
                                let end = if *b == u64::MAX { String::from("…") } else { format!("{b:X}") };
                                pill(ui, &format!("{a:X}–{end} {}", human_bytes(n as u64)), TEXT2());
                            }
                        }
                    });
                }
            }
        });
        if let Some((m, col)) = toast {
            self.toast(m, col);
        }
    }

    // ------------------------------------------------------------ 打包

    fn tb_fwpack(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z;
        let u = &mut self.tb.z;
        let mut toast: Option<(String, Color32)> = None;
        let key = format!("{}|{}", c.pk_input, c.pk_base);
        if key != u.pk_key {
            u.pk_key = key;
            u.pk_img = (!c.pk_input.trim().is_empty()).then(|| {
                let mut img = fw::load_any(std::path::Path::new(c.pk_input.trim()))?;
                if img.kind == "BIN" {
                    let base = fw::parse_num(&c.pk_base).ok_or("base")?.max(0) as u64;
                    img.segs[0].0 = base;
                }
                let segs = fw::fill_gaps(&img.segs, c.pk.fill);
                segs.into_iter().next().ok_or_else(|| t("文件是空的").to_string())
            });
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            settings_card(ui, |ui| {
                file_pick(ui, t("固件"), &mut c.pk_input, &["bin", "hex", "srec", "s19", "elf", "axf"], false);
                if fw::is_bin(&c.pk_input) {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, 26.0), Sense::hover());
                        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, t("起始地址"), FontId::proportional(12.5), TEXT2());
                        ui.add_sized(vec2(140.0, 24.0), egui::TextEdit::singleline(&mut c.pk_base).font(MONO(12.0)));
                        ui.label(RichText::new(t("BIN 没有地址；只有写到指定地址时才用得上")).small().color(TEXT3()));
                    });
                }
            });
            ui.add_space(12.0);
            // 模板
            ui.horizontal(|ui| {
                eyebrow(ui, t("格式模板"), "");
                egui::ComboBox::from_id_salt("pk_tpl").width(220.0).selected_text(c.pk.name.clone()).show_ui(ui, |ui| {
                    for tp in &c.pk_tpls {
                        if ui.selectable_label(*tp == c.pk, &tp.name).clicked() {
                            c.pk = tp.clone();
                        }
                    }
                });
                if ui.button(t("存为模板")).on_hover_text(t("按名字保存；同名的会被覆盖")).clicked() {
                    match c.pk_tpls.iter_mut().find(|x| x.name == c.pk.name) {
                        Some(x) => *x = c.pk.clone(),
                        None => c.pk_tpls.push(c.pk.clone()),
                    }
                    toast = Some((tf("已保存模板：{}", &[&c.pk.name]), OK()));
                }
                if c.pk_tpls.iter().any(|x| x.name == c.pk.name) && ui.button(t("删除模板")).clicked() {
                    c.pk_tpls.retain(|x| x.name != c.pk.name);
                }
                if ui.button(t("恢复内置模板")).clicked() {
                    for p in fw::presets() {
                        if !c.pk_tpls.iter().any(|x| x.name == p.name) {
                            c.pk_tpls.push(p);
                        }
                    }
                }
            });
            let tp = &mut c.pk;
            settings_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("名字")).color(TEXT2()));
                    ui.add_sized(vec2(200.0, 24.0), egui::TextEdit::singleline(&mut tp.name));
                    ui.add_space(10.0);
                    let mut le = tp.little;
                    if toggle_chip(ui, &mut le, if tp.little { t("小端") } else { t("大端") }, t("多字节字段的字节序")) {
                        tp.little = le;
                    }
                    ui.add_space(10.0);
                    for (pl, name) in [(fw::Place::Head, t("放在前面")), (fw::Place::Tail, t("放在后面")), (fw::Place::At, t("写到地址"))] {
                        let mut on = tp.place == pl;
                        if toggle_chip(ui, &mut on, name, "") {
                            tp.place = pl;
                        }
                    }
                    if tp.place == fw::Place::At {
                        ui.add_sized(vec2(110.0, 24.0), egui::TextEdit::singleline(&mut tp.at).font(MONO(12.0)).hint_text("0x0800FFE0"));
                    }
                });
                ui.horizontal(|ui| {
                    if tp.place == fw::Place::Head {
                        ui.label(RichText::new(t("头补齐到")).color(TEXT2()));
                        ui.add(egui::DragValue::new(&mut tp.head_size).range(0..=65536).suffix(" B"));
                        ui.add_space(10.0);
                    }
                    if tp.place != fw::Place::At {
                        ui.label(RichText::new(t("固件先对齐到")).color(TEXT2()));
                        ui.add(egui::DragValue::new(&mut tp.align).range(0..=65536).suffix(" B"));
                        ui.add_space(10.0);
                    }
                    ui.label(RichText::new(t("填充")).color(TEXT2()));
                    ui.add(egui::DragValue::new(&mut tp.fill).hexadecimal(2, false, true));
                });
                ui.add_space(6.0);
                // 字段表
                let mut del = None;
                let mut up = None;
                header_row(ui, &[("", 22.0), (t("字段"), 130.0), (t("类型"), 150.0), (t("字节"), 70.0), (t("值"), 200.0)]);
                for (i, fd) in tp.fields.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(22.0, 22.0), egui::Label::new(RichText::new((i + 1).to_string()).small().color(TEXT3())));
                        ui.add_sized(vec2(126.0, 22.0), egui::TextEdit::singleline(&mut fd.name).font(MONO(12.0)));
                        egui::ComboBox::from_id_salt(("pk_kind", i)).width(140.0).selected_text(kind_label(fd.kind)).show_ui(ui, |ui| {
                            for k in fw::FieldKind::ALL {
                                if ui.selectable_value(&mut fd.kind, k, kind_label(k)).changed() {
                                    fd.size = match k {
                                        fw::FieldKind::Crc16 => 2,
                                        fw::FieldKind::Text => 16,
                                        _ => 4,
                                    };
                                }
                            }
                        });
                        ui.add_sized(vec2(66.0, 22.0), egui::DragValue::new(&mut fd.size).range(1..=256));
                        let needs = matches!(fd.kind, fw::FieldKind::Const | fw::FieldKind::Version | fw::FieldKind::Text);
                        if needs {
                            ui.add_sized(vec2(196.0, 22.0), egui::TextEdit::singleline(&mut fd.value).font(MONO(12.0)));
                        } else {
                            ui.add_sized(vec2(196.0, 22.0), egui::Label::new(RichText::new(t("自动")).small().color(TEXT3())));
                        }
                        if i > 0 && icon_button(ui, Icon::Swap, t("上移"), false).clicked() {
                            up = Some(i);
                        }
                        if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                            del = Some(i);
                        }
                    });
                }
                if let Some(i) = del {
                    tp.fields.remove(i);
                }
                if let Some(i) = up {
                    tp.fields.swap(i, i - 1);
                }
                if ui.button(t("添加字段")).clicked() {
                    tp.fields.push(fw::Field {
                        name: format!("field{}", tp.fields.len() + 1),
                        kind: fw::FieldKind::Const,
                        size: 4,
                        value: "0".into(),
                    });
                }
            });
            ui.add_space(12.0);
            let Some(img) = &u.pk_img else { return };
            let (base, data) = match img {
                Ok(x) => x,
                Err(e) => {
                    ui.label(RichText::new(e).color(ERR()));
                    return;
                }
            };
            let now = chrono::Utc::now().timestamp() as u64;
            let packed = match fw::pack(*base, data.clone(), tp, now) {
                Ok(p) => p,
                Err(e) => {
                    ui.label(RichText::new(e).color(ERR()));
                    return;
                }
            };
            eyebrow(ui, t("结果"), "");
            ui.horizontal_wrapped(|ui| {
                stat(ui, t("固件长度"), &packed.len.to_string(), Some(RichText::new(human_bytes(packed.len)).small().color(TEXT3())));
                stat(ui, "CRC32", &format!("{:08X}", packed.crc), None);
                let total: usize = packed.segs.iter().map(|s| s.1.len()).sum();
                stat(ui, t("输出大小"), &total.to_string(), Some(RichText::new(human_bytes(total as u64)).small().color(TEXT3())));
            });
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(BG0())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_CARD)
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Grid::new("pk_fields").num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
                        for h in [t("字段"), t("偏移"), t("字节")] {
                            ui.label(RichText::new(h).small().color(TEXT3()));
                        }
                        ui.end_row();
                        for (name, off, bytes) in &packed.fields {
                            ui.label(RichText::new(name).font(MONO(12.0)).color(VIOLET()));
                            ui.label(RichText::new(format!("+{off}")).font(MONO(12.0)).color(TEXT3()));
                            copy_label(ui, &bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
                            ui.end_row();
                        }
                    });
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("格式")).color(TEXT2()));
                for f in fw::Fmt::ALL {
                    let mut on = c.pk_fmt == f;
                    if toggle_chip(ui, &mut on, f.label(), "") {
                        c.pk_fmt = f;
                    }
                }
                ui.add_space(10.0);
                if ui
                    .add(egui::Button::new(RichText::new(t("保存…")).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(100.0, 30.0)))
                    .clicked()
                {
                    let stem = std::path::Path::new(&c.pk_input)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "firmware".into());
                    if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}_ota.{}", c.pk_fmt.ext())).save_file() {
                        match std::fs::write(&path, fw::write(&packed.segs, c.pk_fmt, tp.fill, None)) {
                            Ok(_) => toast = Some((tf("已保存：{}", &[&path.display()]), OK())),
                            Err(e) => toast = Some((e.to_string(), ERR())),
                        }
                    }
                }
                ui.label(RichText::new(t("时间字段用保存那一刻的时间")).small().color(TEXT3()));
            });
        });
        if let Some((m, col)) = toast {
            self.toast(m, col);
        }
    }

    // ------------------------------------------------------------ 体积对比

    fn tb_sizediff(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z;
        settings_card(ui, |ui| {
            file_pick(ui, t("旧版本"), &mut c.sz_old, &["map", "elf", "axf", "out"], false);
            file_pick(ui, t("新版本"), &mut c.sz_new, &["map", "elf", "axf", "out"], false);
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W);
                if icon_button(ui, Icon::Swap, t("交换新旧"), false).clicked() {
                    std::mem::swap(&mut c.sz_old, &mut c.sz_new);
                }
                if icon_button(ui, Icon::Refresh, t("重新读取文件"), false).clicked() {
                    self.tb.z.sz_key.clear();
                }
                ui.label(RichText::new(t("GCC、Keil、LLVM 的 .map（能按文件汇总）或者 .elf")).small().color(TEXT3()));
            });
        });
        let key = format!("{}|{}", c.sz_old, c.sz_new);
        if key != self.tb.z.sz_key {
            self.tb.z.sz_key = key;
            self.tb.z.sz = (!c.sz_old.trim().is_empty() && !c.sz_new.trim().is_empty()).then(|| {
                let a = sizediff::load(std::path::Path::new(c.sz_old.trim()))?;
                let b = sizediff::load(std::path::Path::new(c.sz_new.trim()))?;
                Ok(sizediff::diff(&a, &b))
            });
        }
        let u = &mut self.tb.z;
        let Some(d) = &u.sz else { return };
        let d = match d {
            Ok(d) => d,
            Err(e) => {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        let has_files = !d.files.is_empty() || d.syms.iter().any(|r| !r.file.is_empty());
        if !has_files {
            u.sz_files = false;
        }
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            let pct = |a: u64, b: u64| if a == 0 { String::new() } else { format!("  {:+.2}%", (b as f64 - a as f64) * 100.0 / a as f64) };
            for (label, a, b) in [("ROM / Flash", d.old.0, d.new.0), ("RAM", d.old.1, d.new.1)] {
                let delta = b as i64 - a as i64;
                let sign = if delta > 0 { format!("+{delta}") } else { delta.to_string() };
                let col = if delta > 0 {
                    ERR()
                } else if delta < 0 {
                    OK()
                } else {
                    TEXT3()
                };
                stat(ui, label, &human_bytes(b), Some(RichText::new(format!("{sign} B{}", pct(a, b))).font(MONO(12.0)).color(col)));
            }
            // ELF 里没有“属于哪个文件”的信息，只有 .map 才能按文件汇总
            if has_files {
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    let mut tab = u.sz_files;
                    tab_strip(ui, &mut tab, &[(false, t("符号")), (true, t("文件"))], 26.0);
                    u.sz_files = tab;
                });
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut u.sz_filter).hint_text(t("筛选名字")).desired_width(220.0));
            let rows = if u.sz_files { &d.files } else { &d.syms };
            ui.label(RichText::new(tf("{} 项有变化", &[&rows.len()])).small().color(TEXT3()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("复制为 CSV")).clicked() {
                    let mut s = String::from("name,file,kind,old_rom,old_ram,new_rom,new_ram,delta\n");
                    for r in rows {
                        s.push_str(&format!("{},{},{},{},{},{},{},{}\n", r.name, r.file, r.kind, r.old.0, r.old.1, r.new.0, r.new.1, r.delta()));
                    }
                    ui.ctx().copy_text(s);
                }
            });
        });
        let q = u.sz_filter.to_lowercase();
        let rows: Vec<&sizediff::Row> = (if u.sz_files { &d.files } else { &d.syms })
            .iter()
            .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q) || r.file.to_lowercase().contains(&q))
            .collect();
        let files = u.sz_files;
        let cols: Vec<(&str, f32)> = if files {
            vec![(t("文件"), 260.0), ("ROM Δ", 90.0), ("RAM Δ", 90.0), (t("旧"), 100.0), (t("新"), 100.0)]
        } else {
            let mut v = vec![(t("名字"), 260.0)];
            if has_files {
                v.push((t("文件"), 150.0));
            }
            v.extend([(t("类型"), 60.0), ("ROM Δ", 90.0), ("RAM Δ", 90.0), (t("旧"), 90.0), (t("新"), 90.0)]);
            v
        };
        ui.add_space(4.0);
        header_row(ui, &cols);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            dense_rows(ui);
            for r in &rows[range] {
                ui.horizontal(|ui| {
                    cell(ui, cols[0].1, RichText::new(&r.name).font(MONO(12.0)).color(TEXT()));
                    if !files {
                        if has_files {
                            cell(ui, 150.0, RichText::new(&r.file).font(MONO(11.5)).color(TEXT3()));
                        }
                        let k = match r.kind {
                            'F' => t("函数"),
                            'V' => t("变量"),
                            _ => t("段"),
                        };
                        cell(ui, 60.0, RichText::new(k).small().color(TEXT2()));
                    }
                    cell(ui, 90.0, delta_text(r.delta_rom()));
                    cell(ui, 90.0, delta_text(r.delta_ram()));
                    let w = cols.last().unwrap().1;
                    cell(ui, w, RichText::new(format!("{}", r.old.0.max(r.old.1))).font(MONO(11.5)).color(TEXT3()));
                    cell(ui, w, RichText::new(format!("{}", r.new.0.max(r.new.1))).font(MONO(11.5)).color(TEXT2()));
                });
            }
        });
    }

    // ------------------------------------------------------------ 栈分析

    fn tb_stack(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z;
        let mut run = false;
        settings_card(ui, |ui| {
            file_pick(ui, t("编译目录"), &mut c.st_dir, &[], true);
            file_pick(ui, "ELF", &mut c.st_elf, &["elf", "axf", "out"], false);
            ui.horizontal(|ui| {
                ui.add_space(LABEL_W);
                run = ui
                    .add(egui::Button::new(RichText::new(t("分析")).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(90.0, 28.0)))
                    .clicked();
                ui.label(
                    RichText::new(t("编译时加 -fstack-usage（必需）和 -fcallgraph-info=su；没有 .ci 文件时从 Cortex-M 的 ELF 里找调用关系"))
                        .small()
                        .color(TEXT3()),
                );
            });
        });
        // 第一次打开（上次的目录还在）或者刚拖入文件时自动分析
        run |= std::mem::take(&mut self.tb.z.st_auto) || self.tb.z.st.is_none() && !c.st_dir.trim().is_empty();
        if run {
            let mut g = stack::load_dir(std::path::Path::new(c.st_dir.trim()));
            let mut note = tf("{} 个函数，{} 个 .su 文件", &[&g.funcs.len(), &g.su_files]);
            if g.ci_files > 0 {
                note.push_str(&tf("，调用关系来自 {} 个 .ci 文件", &[&g.ci_files]));
            } else if !c.st_elf.trim().is_empty() {
                match std::fs::read(c.st_elf.trim()).map_err(|e| e.to_string()).and_then(|b| stack::calls_from_elf(&b, &mut g)) {
                    Ok(n) => note.push_str(&tf("，从 ELF 找到 {} 处调用", &[&n])),
                    Err(e) => note.push_str(&format!("，ELF: {e}")),
                }
            } else {
                note.push_str(&format!("，{}", t("没有调用关系，只能看单个函数")));
            }
            let es = stack::analyze(&g);
            self.tb.z.st = Some((g, es));
            self.tb.z.st_note = note;
            self.tb.z.st_sel = None;
        }
        let u = &mut self.tb.z;
        let Some((g, es)) = &u.st else { return };
        ui.add_space(10.0);
        ui.label(RichText::new(&u.st_note).color(TEXT2()));
        if g.funcs.is_empty() {
            ui.label(RichText::new(t("没找到 .su 文件。GCC 加 -fstack-usage 重新编译后再选编译输出目录")).color(WARN()));
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut all = u.st_all;
            tab_strip(ui, &mut all, &[(false, t("入口")), (true, t("所有函数"))], 26.0);
            u.st_all = all;
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut u.st_filter).hint_text(t("筛选名字")).desired_width(220.0));
            ui.label(RichText::new(t("Cortex-M 进中断时硬件还会压 32 字节（带 FPU 上下文 104 字节）")).small().color(TEXT3()));
        });
        ui.add_space(4.0);
        let q = u.st_filter.to_lowercase();
        let mut open: Option<(String, u32)> = None;
        if u.st_all {
            let mut fs: Vec<&stack::Func> = g.funcs.values().filter(|f| q.is_empty() || f.name.to_lowercase().contains(&q)).collect();
            fs.sort_by(|a, b| b.own.cmp(&a.own).then(a.name.cmp(&b.name)));
            let cols = [(t("函数"), 260.0), (t("自身"), 80.0), (t("类型"), 130.0), (t("位置"), 300.0)];
            header_row(ui, &cols);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, fs.len(), |ui, range| {
                dense_rows(ui);
                for f in &fs[range] {
                    let r = ui.horizontal(|ui| {
                        cell(ui, 260.0, RichText::new(&f.name).font(MONO(12.0)).color(TEXT()));
                        cell(ui, 80.0, RichText::new(f.own.to_string()).font(MONO(12.0)).color(TEXT()));
                        cell(ui, 130.0, RichText::new(&f.qual).small().color(if f.dynamic() { WARN() } else { TEXT3() }));
                        cell(ui, 300.0, RichText::new(format!("{}:{}", f.file, f.line)).font(MONO(11.5)).color(TEXT3()));
                    });
                    if r.response.interact(Sense::click()).on_hover_text(t("双击打开源文件")).double_clicked() {
                        open = Some((f.file.clone(), f.line));
                    }
                }
            });
        } else {
            let list: Vec<&stack::Entry> = es.iter().filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q)).collect();
            let max = es.first().map_or(1, |e| e.total.max(1));
            let sel = u.st_sel.clone();
            let detail_h = if sel.is_some() { 220.0 } else { 0.0 };
            let cols = [(t("入口"), 260.0), (t("最大栈"), 90.0), ("", 160.0), (t("提示"), 300.0)];
            header_row(ui, &cols);
            egui::ScrollArea::vertical()
                .id_salt("st_entries")
                .max_height((ui.available_height() - detail_h).max(120.0))
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, list.len(), |ui, range| {
                    dense_rows(ui);
                    for e in &list[range] {
                        let on = sel.as_deref() == Some(e.name.as_str());
                        let r = ui.horizontal(|ui| {
                            let col = if stack::is_isr(&e.name) {
                                WARN()
                            } else if e.name == "main" {
                                ACCENT()
                            } else {
                                TEXT()
                            };
                            cell(ui, 260.0, RichText::new(&e.name).font(MONO(12.0)).color(col));
                            cell(ui, 90.0, RichText::new(e.total.to_string()).font(FontId::new(12.0, mono_bold())).color(TEXT()));
                            let (br, _) = ui.allocate_exact_size(vec2(160.0, 18.0), Sense::hover());
                            ui.painter().rect_filled(Rect::from_min_size(br.left_center() - vec2(0.0, 4.0), vec2(150.0, 8.0)), 3.0, BG3());
                            ui.painter().rect_filled(
                                Rect::from_min_size(br.left_center() - vec2(0.0, 4.0), vec2(150.0 * e.total as f32 / max as f32, 8.0)),
                                3.0,
                                ACCENT().gamma_multiply(0.8),
                            );
                            let mut flags = vec![];
                            if e.recursive {
                                flags.push(t("有递归，实际可能更深"));
                            }
                            if e.indirect {
                                flags.push(t("有函数指针调用"));
                            }
                            if e.dynamic {
                                flags.push(t("有动态栈（alloca / 变长数组）"));
                            }
                            let unk = if e.unknown.is_empty() {
                                String::new()
                            } else {
                                tf("{} 个函数不知道栈大小", &[&e.unknown.len()])
                            };
                            if !unk.is_empty() {
                                flags.push(&unk);
                            }
                            cell(ui, 300.0, RichText::new(flags.join("；")).small().color(if e.recursive || e.dynamic { ERR() } else { WARN() }));
                        });
                        let resp = r.response.interact(Sense::click());
                        if on {
                            ui.painter().rect_filled(resp.rect, 2.0, ACCENT().gamma_multiply(0.08));
                        }
                        if resp.clicked() {
                            u.st_sel = if on { None } else { Some(e.name.clone()) };
                        }
                    }
                });
            if let Some(s) = &sel {
                let e = stack::entry_of(g, s);
                ui.add_space(6.0);
                eyebrow(ui, &tf("最深调用链：{}", &[s]), &tf("{} 字节", &[&e.total]));
                egui::ScrollArea::vertical().id_salt("st_chain").max_height(190.0).show(ui, |ui| {
                    let mut acc = 0;
                    for (depth, f) in e.chain.iter().enumerate() {
                        let fun = g.funcs.get(f);
                        let own = fun.map_or(0, |x| x.own);
                        acc += own;
                        let r = ui.horizontal(|ui| {
                            ui.add_space(depth.min(20) as f32 * 12.0);
                            ui.label(RichText::new(if depth == 0 { "●" } else { "└" }).color(TEXT3()));
                            ui.label(RichText::new(f).font(MONO(12.0)).color(if fun.is_some() { TEXT() } else { TEXT3() }));
                            ui.label(RichText::new(if fun.is_some() { format!("+{own}") } else { "?".into() }).font(MONO(11.5)).color(VIOLET()));
                            ui.label(RichText::new(format!("= {acc}")).font(MONO(11.5)).color(TEXT3()));
                        });
                        if let Some(fu) = fun {
                            if r.response.interact(Sense::click()).on_hover_text(format!("{}:{}", fu.file, fu.line)).double_clicked() {
                                open = Some((fu.file.clone(), fu.line));
                            }
                        }
                    }
                });
            }
        }
        if let Some((file, line)) = open {
            let mut p = PathBuf::from(&file);
            if p.is_relative() {
                p = PathBuf::from(self.s.tb.z.st_dir.trim()).join(&p);
            }
            if p.exists() {
                self.set_page(Page::Code);
                self.code.open_file(&p, Some((line.saturating_sub(1) as usize, None)));
            } else {
                self.toast(tf("找不到文件：{}", &[&file]), WARN());
            }
        }
    }

    // ------------------------------------------------------------ 向量表

    fn tb_vectors(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z;
        settings_card(ui, |ui| {
            file_pick(ui, "ELF", &mut c.vt_elf, &["elf", "axf", "out"], false);
        });
        let svd_key = self.tb.z.x.svd_name();
        let key = format!("{}|{svd_key}", c.vt_elf);
        if key != self.tb.z.vt_key {
            self.tb.z.vt_key = key;
            let irq = self.tb.z.x.irq_names();
            self.tb.z.vt = (!c.vt_elf.trim().is_empty()).then(|| {
                let b = std::fs::read(c.vt_elf.trim()).map_err(|e| e.to_string())?;
                fw::vectors(&b, &|n| irq.get(&(n as u32)).cloned())
            });
        }
        let u = &mut self.tb.z;
        let Some(v) = &u.vt else { return };
        let v = match v {
            Ok(v) => v,
            Err(e) => {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        ui.add_space(10.0);
        let unimpl = v.vectors.iter().filter(|x| x.default && x.index >= 16).count();
        ui.horizontal_wrapped(|ui| {
            stat(ui, t("位置"), &hx(v.base), Some(RichText::new(&v.section).small().color(TEXT3())));
            stat(ui, t("初始栈顶"), &hx(v.sp), None);
            stat(ui, t("向量数"), &v.vectors.len().to_string(), None);
            stat(
                ui,
                t("未实现的中断"),
                &unimpl.to_string(),
                Some(RichText::new(t("还指向 Default_Handler")).small().color(if unimpl > 0 { WARN() } else { TEXT3() })),
            );
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            small_chip(ui, &mut u.vt_default_only, t("只看未实现的"), "");
            if !svd_key.is_empty() {
                ui.label(RichText::new(tf("中断名来自 {}", &[&svd_key])).small().color(TEXT3()));
            } else {
                ui.label(RichText::new(t("在「SVD 寄存器」里载入芯片的 SVD 文件后，这里会显示 IRQ 的名字")).small().color(TEXT3()));
            }
        });
        let rows: Vec<&fw::Vector> = v.vectors.iter().filter(|x| !u.vt_default_only || x.default).collect();
        let cols = [("#", 44.0), (t("异常 / 中断"), 230.0), (t("入口地址"), 110.0), (t("处理函数"), 330.0)];
        header_row(ui, &cols);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            dense_rows(ui);
            for x in &rows[range] {
                ui.horizontal(|ui| {
                    cell(ui, 44.0, RichText::new(x.index.to_string()).font(MONO(11.5)).color(TEXT3()));
                    cell(ui, 230.0, RichText::new(&x.slot).font(MONO(12.0)).color(if x.index < 16 { VIOLET() } else { TEXT2() }));
                    cell(ui, 110.0, RichText::new(if x.addr == 0 { "0".into() } else { hx(x.addr) }).font(MONO(12.0)).color(TEXT2()));
                    // 同一个地址常有一串弱别名（NMI_Handler = EXTI0_IRQHandler = …），只显示和这个槽位对得上的那个
                    let key = x.slot.trim_start_matches("IRQ ").trim_start_matches(|c: char| c.is_ascii_digit()).trim().replace("SVCall", "SVC");
                    let key = key.to_lowercase();
                    let main = x.names.iter().find(|n| !key.is_empty() && key != "-" && n.to_lowercase().starts_with(&key)).or_else(|| {
                        if x.default {
                            None
                        } else {
                            x.names.iter().find(|n| !n.starts_with("Default")).or(x.names.first())
                        }
                    });
                    let mut job = egui::text::LayoutJob::default();
                    let f = |c: Color32| egui::TextFormat {
                        font_id: MONO(12.0),
                        color: c,
                        ..Default::default()
                    };
                    match main {
                        None if x.default => job.append("Default_Handler", 0.0, f(WARN())),
                        None => job.append("—", 0.0, f(TEXT3())),
                        Some(m) => {
                            job.append(m, 0.0, f(if x.default { WARN() } else { TEXT() }));
                            if x.default && !m.starts_with("Default") {
                                job.append("  → Default_Handler", 0.0, f(TEXT3()));
                            } else if x.names.len() > 1 && !x.default {
                                job.append(&tf("  +{} 个别名", &[&(x.names.len() - 1)]), 0.0, f(TEXT3()));
                            }
                        }
                    }
                    ui.allocate_ui_with_layout(vec2(330.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                        let r = ui.add(egui::Label::new(job).truncate());
                        if x.names.len() > 1 {
                            r.on_hover_text(x.names.join("\n"));
                        }
                    });
                });
            }
        });
    }
}

fn kind_label(k: fw::FieldKind) -> &'static str {
    use fw::FieldKind::*;
    match k {
        Const => t("固定值"),
        Version => t("版本号"),
        Length => t("固件长度"),
        Crc32 => "CRC32",
        Crc16 => "CRC16-CCITT",
        Sum => t("累加和"),
        Time => t("时间戳"),
        Text => t("文字"),
        Reserved => t("预留"),
        HeadCrc => t("头部 CRC32"),
    }
}

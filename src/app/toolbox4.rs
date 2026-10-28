//! 工具箱 v1.6：芯片和工程（SVD 寄存器、Keil / IAR 导入、CubeMX 引脚表）和硬件计算器。

use std::collections::HashMap;
use std::sync::Arc;

use crate::toolbox::hwcalc::{self, fmt_si, parse_si, ESeries, I2cMode, Ntc};
use crate::toolbox::{proj, svd};

use super::toolbox::{cell, copy_label, dense_rows, header_row, hint_line, settings_card, Tool, MONO, ROW_H};
use super::toolbox2::code_block;
use super::toolbox3::{file_pick, hx, series, stat};
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Cfg4 {
    pub svd_path: String,
    pub proj_path: String,
    pub proj_target: usize,
    pub ioc_path: String,
    pub res_tab: u8,
    pub div_vin: String,
    pub div_vout: String,
    pub div_e: ESeries,
    pub div_lo: String,
    pub div_hi: String,
    pub led_vs: String,
    pub led_vf: String,
    pub led_i: String,
    pub band_val: String,
    pub band_five: bool,
    pub adc_bits: u32,
    pub adc_vref: String,
    pub adc_raw: String,
    pub adc_r1: String,
    pub adc_r2: String,
    pub ntc: Ntc,
    pub ntc_raw: String,
    pub ntc_t: String,
    pub ntc_lo: i32,
    pub ntc_hi: i32,
    pub ntc_step: i32,
    pub pwm_clk: String,
    pub pwm_f: String,
    pub pwm_duty: f64,
    pub pwm_32: bool,
    pub i2c_vdd: String,
    pub i2c_cb: String,
    pub i2c_mode: I2cMode,
}

impl Default for Cfg4 {
    fn default() -> Self {
        Cfg4 {
            svd_path: String::new(),
            proj_path: String::new(),
            proj_target: 0,
            ioc_path: String::new(),
            res_tab: 0,
            div_vin: "12".into(),
            div_vout: "3.3".into(),
            div_e: ESeries::E24,
            div_lo: "1k".into(),
            div_hi: "1M".into(),
            led_vs: "3.3".into(),
            led_vf: "2.0".into(),
            led_i: "5m".into(),
            band_val: "4.7k".into(),
            band_five: false,
            adc_bits: 12,
            adc_vref: "3.3".into(),
            adc_raw: "2048".into(),
            adc_r1: String::new(),
            adc_r2: String::new(),
            ntc: Ntc::default(),
            ntc_raw: "2048".into(),
            ntc_t: "25".into(),
            ntc_lo: -20,
            ntc_hi: 100,
            ntc_step: 5,
            pwm_clk: "72M".into(),
            pwm_f: "20k".into(),
            pwm_duty: 50.0,
            pwm_32: false,
            i2c_vdd: "3.3".into(),
            i2c_cb: "100p".into(),
            i2c_mode: I2cMode::Fast,
        }
    }
}

#[derive(Default)]
pub struct Ui4 {
    svd_key: String,
    pub(super) svd: Option<Result<Arc<svd::Device>, String>>,
    svd_filter: String,
    svd_open: Option<usize>,
    /// (外设, 寄存器)
    svd_sel: Option<(usize, Option<usize>)>,
    svd_val: u64,
    svd_val_for: Option<(usize, usize)>,
    proj_key: String,
    proj: Option<Result<proj::Project, String>>,
    ioc_key: String,
    ioc: Option<Result<proj::Ioc, String>>,
    ioc_filter: String,
}

impl Ui4 {
    pub(super) fn device(&self) -> Option<&Arc<svd::Device>> {
        self.svd.as_ref().and_then(|r| r.as_ref().ok())
    }
    pub(super) fn svd_name(&self) -> String {
        self.device().map(|d| d.name.clone()).unwrap_or_default()
    }
    pub(super) fn irq_names(&self) -> HashMap<u32, String> {
        self.device().map(|d| d.irqs.iter().map(|(k, v)| (*k, v.0.clone())).collect()).unwrap_or_default()
    }
}

pub(super) fn tool_text4(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::Svd => (t("SVD 寄存器"), t("浏览芯片的外设、寄存器和位域；输入寄存器值拆成各个字段，代码里悬停 USART1->CR1 也能看到")),
        Tool::Project => (t("Keil / IAR 工程"), t("读出 Keil、IAR 工程的宏定义、头文件路径和源文件，生成 compile_commands.json")),
        Tool::Ioc => (t("CubeMX 引脚表"), t("导入 .ioc，列出每个引脚的功能和标签，可搜索、导出")),
        Tool::Resistor => (t("电阻计算"), t("分压电阻配对（E24 / E96）、LED 限流电阻、色环")),
        Tool::Adc => (t("ADC / NTC"), t("ADC 原始值和电压互算；NTC 用 B 值或阻值表算温度，生成 C 查找表")),
        Tool::Pwm => (t("定时器 / PWM"), t("按时钟、目标频率和占空比算预分频、重装值和比较值")),
        Tool::I2cPull => (t("I2C 上拉电阻"), t("按总线电容和速率算上拉电阻的取值范围")),
        other => super::toolbox5::tool_text5(other),
    }
}

/// 小输入框：标签 + 文本 + 单位
fn num_in(ui: &mut egui::Ui, label: &str, v: &mut String, unit: &str, w: f32) -> Option<f64> {
    ui.label(RichText::new(label).color(TEXT2()));
    let ok = parse_si(v);
    let r = ui.add_sized(vec2(w, 24.0), egui::TextEdit::singleline(v).font(MONO(12.5)).text_color(if ok.is_some() { TEXT() } else { ERR() }));
    if !unit.is_empty() {
        ui.label(RichText::new(unit).color(TEXT3()));
    }
    ui.add_space(8.0);
    let _ = r;
    ok
}

/// 数值存成 f64 的输入框：编辑时保留原文，失焦后显示成 4.7k 这种
fn si_edit(ui: &mut egui::Ui, id: &str, label: &str, v: &mut f64, unit: &str, w: f32) {
    ui.label(RichText::new(label).color(TEXT2()));
    let id = egui::Id::new(("si", id));
    let mut s = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| fmt_si(*v, "").replace(' ', ""));
    let ok = parse_si(&s);
    let r = ui.add_sized(vec2(w, 24.0), egui::TextEdit::singleline(&mut s).font(MONO(12.5)).text_color(if ok.is_some() { TEXT() } else { ERR() }));
    if let Some(x) = parse_si(&s).filter(|x| *x > 0.0) {
        *v = x;
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, s));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    ui.label(RichText::new(unit).color(TEXT3()));
    ui.add_space(8.0);
}

fn result_row(ui: &mut egui::Ui, k: &str, v: &str, note: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(110.0, 22.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, k, FontId::proportional(12.5), TEXT3());
        copy_label(ui, v);
        if !note.is_empty() {
            ui.label(RichText::new(note).small().color(TEXT3()));
        }
    });
}

impl App {
    pub(super) fn tb_v16b(&mut self, ui: &mut egui::Ui, tool: Tool) {
        match tool {
            Tool::Svd => self.tb_svd(ui),
            Tool::Project => self.tb_project(ui),
            Tool::Ioc => self.tb_ioc(ui),
            Tool::Resistor => self.tb_resistor(ui),
            Tool::Adc => self.tb_adc(ui),
            Tool::Pwm => self.tb_pwm(ui),
            Tool::I2cPull => self.tb_i2c(ui),
            other => self.tb_v16c(ui, other),
        }
    }

    pub(super) fn tool_drop4(&mut self, tool: Tool, p: PathBuf) {
        let s = p.to_string_lossy().into_owned();
        let c = &mut self.s.tb.z.x;
        match tool {
            Tool::Svd => c.svd_path = s,
            Tool::Project => c.proj_path = s,
            Tool::Ioc => c.ioc_path = s,
            other => self.tool_drop5(other, p),
        }
    }

    /// 启动时和换文件时载入 SVD（代码页悬停也要用）
    pub(super) fn svd_sync(&mut self) {
        let path = self.s.tb.z.x.svd_path.trim().to_string();
        if path == self.tb.z.x.svd_key {
            return;
        }
        self.tb.z.x.svd_key = path.clone();
        self.tb.z.x.svd = (!path.is_empty()).then(|| {
            let b = std::fs::read(&path).map_err(|e| e.to_string())?;
            svd::parse(&String::from_utf8_lossy(&b)).map(Arc::new)
        });
        self.tb.z.x.svd_sel = None;
        self.code.svd = self.tb.z.x.device().cloned();
    }

    // ------------------------------------------------------------ SVD

    fn tb_svd(&mut self, ui: &mut egui::Ui) {
        settings_card(ui, |ui| {
            file_pick(ui, "SVD", &mut self.s.tb.z.x.svd_path, &["svd", "xml"], false);
            if self.s.tb.z.x.svd_path.is_empty() {
                ui.label(
                    RichText::new(t("SVD 文件在芯片厂的 Keil 支持包（.pack 解压后的 SVD 目录）或者 cmsis-svd 仓库里"))
                        .small()
                        .color(TEXT3()),
                );
            }
        });
        self.svd_sync();
        let u = &mut self.tb.z.x;
        let Some(dev) = &u.svd else { return };
        let dev = match dev {
            Ok(d) => d.clone(),
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&dev.name).font(FontId::new(15.0, bold())).color(TEXT()));
            if !dev.cpu.is_empty() {
                pill(ui, &dev.cpu, VIOLET());
            }
            ui.label(RichText::new(tf("{} 个外设，{} 个中断", &[&dev.periphs.len(), &dev.irqs.len()])).small().color(TEXT3()));
        });
        ui.add_space(6.0);
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            // 左：外设和寄存器树
            ui.allocate_ui_with_layout(vec2(270.0, h), Layout::top_down(Align::Min), |ui| {
                ui.set_width(270.0);
                ui.add(egui::TextEdit::singleline(&mut u.svd_filter).hint_text(t("搜索外设、寄存器、位域")).desired_width(262.0));
                ui.add_space(4.0);
                let q = u.svd_filter.trim().to_lowercase();
                egui::ScrollArea::vertical().id_salt("svd_tree").auto_shrink([false, false]).show(ui, |ui| {
                    for (pi, p) in dev.periphs.iter().enumerate() {
                        let pm = q.is_empty() || p.name.to_lowercase().contains(&q) || p.group.to_lowercase().contains(&q);
                        let regs: Vec<usize> = p
                            .regs
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| q.is_empty() || pm || r.name.to_lowercase().contains(&q) || r.fields.iter().any(|f| f.name.to_lowercase().contains(&q)))
                            .map(|(i, _)| i)
                            .collect();
                        if !pm && regs.is_empty() {
                            continue;
                        }
                        let open = u.svd_open == Some(pi) || (!q.is_empty() && !pm);
                        let sel_p = u.svd_sel == Some((pi, None));
                        let r = crate::codeview::tree_row(ui, 0, &p.name, Some(&format!("{:08X}", p.base)), true, Some(open), sel_p, None);
                        if r.clicked() {
                            u.svd_open = if u.svd_open == Some(pi) { None } else { Some(pi) };
                            u.svd_sel = Some((pi, None));
                        }
                        if open {
                            for ri in regs {
                                let reg = &p.regs[ri];
                                let on = u.svd_sel == Some((pi, Some(ri)));
                                let rr = crate::codeview::tree_row(ui, 1, &reg.name, Some(&format!("+{:X}", reg.offset)), false, None, on, None);
                                if rr.clicked() {
                                    u.svd_sel = Some((pi, Some(ri)));
                                }
                            }
                        }
                    }
                });
            });
            ui.add_space(14.0);
            // 右：详情
            ui.vertical(|ui| {
                let Some((pi, ri)) = u.svd_sel else {
                    ui.label(RichText::new(t("在左边选一个外设或寄存器")).color(TEXT3()));
                    return;
                };
                let Some(p) = dev.periphs.get(pi) else { return };
                match ri.and_then(|ri| p.regs.get(ri).map(|r| (ri, r))) {
                    None => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&p.name).font(FontId::new(16.0, mono_bold())).color(TEXT()));
                            copy_label(ui, &hx(p.base));
                            if !p.group.is_empty() {
                                pill(ui, &p.group, TEXT2());
                            }
                            if let Some(d) = &p.derived {
                                ui.label(RichText::new(tf("同 {}", &[d])).small().color(TEXT3()));
                            }
                        });
                        ui.label(RichText::new(&p.desc).color(TEXT2()));
                        ui.add_space(6.0);
                        let cols = [(t("寄存器"), 150.0), (t("偏移"), 70.0), (t("绝对地址"), 110.0), (t("复位值"), 110.0), (t("说明"), 360.0)];
                        header_row(ui, &cols);
                        egui::ScrollArea::vertical()
                            .id_salt("svd_regs")
                            .auto_shrink([false, false])
                            .show_rows(ui, ROW_H, p.regs.len(), |ui, range| {
                                dense_rows(ui);
                                for ri in range {
                                    let r = &p.regs[ri];
                                    let row = ui.horizontal(|ui| {
                                        cell(ui, 150.0, RichText::new(&r.name).font(MONO(12.0)).color(ACCENT()));
                                        cell(ui, 70.0, RichText::new(format!("+0x{:X}", r.offset)).font(MONO(11.5)).color(TEXT3()));
                                        cell(ui, 110.0, RichText::new(hx(p.base + r.offset)).font(MONO(12.0)).color(TEXT2()));
                                        cell(
                                            ui,
                                            110.0,
                                            RichText::new(format!("0x{:0w$X}", r.reset, w = (r.size as usize).div_ceil(4))).font(MONO(12.0)).color(TEXT2()),
                                        );
                                        cell(ui, 360.0, RichText::new(&r.desc).small().color(TEXT3()));
                                    });
                                    if row.response.interact(Sense::click()).clicked() {
                                        u.svd_sel = Some((pi, Some(ri)));
                                    }
                                }
                            });
                    }
                    Some((ri, r)) => {
                        if u.svd_val_for != Some((pi, ri)) {
                            u.svd_val_for = Some((pi, ri));
                            u.svd_val = r.reset;
                        }
                        reg_detail(ui, p, r, &mut u.svd_val);
                    }
                }
            });
        });
    }

    // ------------------------------------------------------------ Keil / IAR

    fn tb_project(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        settings_card(ui, |ui| {
            file_pick(ui, t("工程文件"), &mut c.proj_path, &["uvprojx", "uvproj", "ewp"], false);
        });
        let key = c.proj_path.trim().to_string();
        if key != self.tb.z.x.proj_key {
            self.tb.z.x.proj_key = key.clone();
            self.tb.z.x.proj = (!key.is_empty()).then(|| proj::load(std::path::Path::new(&key)));
            c.proj_target = 0;
        }
        let Some(p) = &self.tb.z.x.proj else {
            ui.add_space(8.0);
            ui.label(RichText::new(t("支持 Keil MDK 的 .uvprojx / .uvproj 和 IAR EWARM 的 .ewp")).color(TEXT3()));
            return;
        };
        let p = match p {
            Ok(p) => p,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        if p.targets.is_empty() {
            ui.label(RichText::new(t("工程里没有目标（Target）")).color(WARN()));
            return;
        }
        ui.add_space(10.0);
        let ti = c.proj_target.min(p.targets.len() - 1);
        let tg = &p.targets[ti];
        let mut action = 0;
        let mut open: Option<PathBuf> = None;
        ui.horizontal(|ui| {
            pill(ui, p.kind, VIOLET());
            ui.label(RichText::new(t("目标")).color(TEXT2()));
            egui::ComboBox::from_id_salt("proj_target").width(200.0).selected_text(&tg.name).show_ui(ui, |ui| {
                for (i, x) in p.targets.iter().enumerate() {
                    ui.selectable_value(&mut c.proj_target, i, &x.name);
                }
            });
            if !tg.device.is_empty() {
                ui.label(RichText::new(&tg.device).font(MONO(12.5)).color(ACCENT()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("在代码页打开")).on_hover_text(t("生成 compile_commands.json 后打开工程目录")).clicked() {
                    action = 2;
                }
                if ui
                    .add(egui::Button::new(RichText::new(t("生成 compile_commands.json")).color(ON_ACCENT()).strong()).fill(ACCENT()))
                    .on_hover_text(t("写到所有源文件的公共上级目录，clangd 和代码页的跳转会用它"))
                    .clicked()
                {
                    action = 1;
                }
            });
        });
        ui.add_space(8.0);
        let srcs = tg.files.iter().filter(|f| matches!(f.1.extension().and_then(|e| e.to_str()), Some("c" | "cpp" | "cc" | "cxx"))).count();
        let missing = tg.includes.iter().filter(|i| !i.is_dir()).count() + tg.files.iter().filter(|f| !f.1.exists()).count();
        ui.horizontal_wrapped(|ui| {
            stat(ui, t("宏定义"), &tg.defines.len().to_string(), None);
            stat(ui, t("头文件目录"), &tg.includes.len().to_string(), None);
            stat(
                ui,
                t("C/C++ 源文件"),
                &srcs.to_string(),
                Some(RichText::new(tf("共 {} 个文件", &[&tg.files.len()])).small().color(TEXT3())),
            );
            stat(
                ui,
                t("找不到的路径"),
                &missing.to_string(),
                Some(
                    RichText::new(if missing > 0 { t("工程不在原来的位置？") } else { t("全部存在") })
                        .small()
                        .color(if missing > 0 { WARN() } else { OK() }),
                ),
            );
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            eyebrow(ui, t("宏定义"), "");
            ui.horizontal_wrapped(|ui| {
                for d in &tg.defines {
                    pill(ui, d, ACCENT());
                }
                if !tg.defines.is_empty() && ui.small_button(t("复制")).clicked() {
                    ui.ctx().copy_text(tg.defines.iter().map(|d| format!("-D{d}")).collect::<Vec<_>>().join(" "));
                }
            });
            if !tg.flags.is_empty() {
                ui.label(RichText::new(tf("其他编译选项：{}", &[&tg.flags])).small().color(TEXT3()));
            }
            ui.add_space(8.0);
            eyebrow(ui, t("头文件目录"), "");
            for i in &tg.includes {
                ui.horizontal(|ui| {
                    let ok = i.is_dir();
                    ui.label(RichText::new(if ok { "✓" } else { "✗" }).color(if ok { OK() } else { ERR() }));
                    ui.label(RichText::new(i.display().to_string()).font(MONO(12.0)).color(if ok { TEXT2() } else { TEXT3() }));
                });
            }
            for s in &tg.skipped {
                ui.label(RichText::new(tf("跳过（编译器自带的目录）：{}", &[s])).small().color(TEXT3()));
            }
            ui.add_space(8.0);
            eyebrow(ui, t("源文件"), "");
            let mut last = "";
            for (g, f) in &tg.files {
                if g != last {
                    ui.label(RichText::new(g).small().color(VIOLET()));
                    last = g;
                }
                let r = ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let ok = f.exists();
                    ui.label(
                        RichText::new(f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                            .font(MONO(12.0))
                            .color(if ok { TEXT() } else { ERR() }),
                    );
                    ui.label(RichText::new(f.parent().map(|d| d.display().to_string()).unwrap_or_default()).small().color(TEXT3()));
                });
                if r.response.interact(Sense::click()).on_hover_text(t("双击打开")).double_clicked() && f.exists() {
                    open = Some(f.clone());
                }
            }
        });
        if let Some(f) = open {
            self.set_page(Page::Code);
            self.code.open_file(&f, None);
            return;
        }
        if action == 0 {
            return;
        }
        let root = proj::common_root(p, tg);
        let out = root.join("compile_commands.json");
        match std::fs::write(&out, proj::compile_commands(p, tg)) {
            Ok(_) => {
                if action == 2 {
                    self.code.open_root(root);
                    self.set_page(Page::Code);
                }
                self.toast(tf("已生成：{}", &[&out.display()]), OK());
            }
            Err(e) => self.toast(format!("{}: {e}", out.display()), ERR()),
        }
    }

    // ------------------------------------------------------------ CubeMX

    fn tb_ioc(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        settings_card(ui, |ui| {
            file_pick(ui, ".ioc", &mut c.ioc_path, &["ioc"], false);
        });
        let key = c.ioc_path.trim().to_string();
        if key != self.tb.z.x.ioc_key {
            self.tb.z.x.ioc_key = key.clone();
            self.tb.z.x.ioc = (!key.is_empty()).then(|| std::fs::read_to_string(&key).map(|s| proj::parse_ioc(&s)).map_err(|e| e.to_string()));
        }
        let u = &mut self.tb.z.x;
        let Some(io) = &u.ioc else { return };
        let io = match io {
            Ok(i) => i,
            Err(e) => {
                ui.label(RichText::new(e).color(ERR()));
                return;
            }
        };
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&io.mcu).font(FontId::new(15.0, bold())).color(TEXT()));
            if !io.package.is_empty() {
                pill(ui, &io.package, VIOLET());
            }
            if !io.project.is_empty() {
                ui.label(RichText::new(&io.project).color(TEXT3()));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut u.ioc_filter).hint_text(t("搜索引脚、功能、标签")).desired_width(240.0));
            ui.label(RichText::new(tf("{} 个引脚已配置", &[&io.pins.len()])).small().color(TEXT3()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t("复制 #define")).on_hover_text(t("和 CubeMX 生成的 main.h 一样：LED_Pin / LED_GPIO_Port")).clicked() {
                    ui.ctx().copy_text(proj::ioc_defines(io));
                }
                if ui.button(t("导出 CSV…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name(format!("{}_pins.csv", if io.project.is_empty() { "pins" } else { &io.project }))
                        .save_file()
                    {
                        let _ = std::fs::write(p, proj::ioc_csv(io));
                    }
                }
                if ui.button(t("复制为 CSV")).clicked() {
                    ui.ctx().copy_text(proj::ioc_csv(io));
                }
            });
        });
        let q = u.ioc_filter.trim().to_lowercase();
        let rows: Vec<&proj::Pin> = io
            .pins
            .iter()
            .filter(|p| q.is_empty() || [&p.pin, &p.signal, &p.label, &p.mode].iter().any(|x| x.to_lowercase().contains(&q)))
            .collect();
        let cols = [
            (t("引脚"), 150.0),
            (t("功能"), 190.0),
            (t("用户标签"), 150.0),
            (t("模式"), 150.0),
            (t("上下拉"), 120.0),
            (t("速度"), 150.0),
        ];
        ui.add_space(4.0);
        header_row(ui, &cols);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            dense_rows(ui);
            for p in &rows[range] {
                ui.horizontal(|ui| {
                    cell(ui, 150.0, RichText::new(&p.pin).font(MONO(12.0)).color(ACCENT()));
                    cell(ui, 190.0, RichText::new(&p.signal).font(MONO(12.0)).color(TEXT()));
                    cell(ui, 150.0, RichText::new(&p.label).font(MONO(12.0)).color(VIOLET()));
                    cell(ui, 150.0, RichText::new(&p.mode).small().color(TEXT2()));
                    cell(ui, 120.0, RichText::new(p.pull.trim_start_matches("GPIO_")).small().color(TEXT3()));
                    cell(ui, 150.0, RichText::new(p.speed.trim_start_matches("GPIO_SPEED_FREQ_")).small().color(TEXT3()));
                });
            }
        });
    }

    // ------------------------------------------------------------ 电阻

    fn tb_resistor(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        ui.allocate_ui(vec2(360.0, 30.0), |ui| tab_strip(ui, &mut c.res_tab, &[(0, t("分压")), (1, t("LED 限流")), (2, t("色环"))], 28.0));
        ui.add_space(12.0);
        match c.res_tab {
            0 => {
                let mut vals = (None, None, None, None);
                settings_card(ui, |ui| {
                    ui.horizontal(|ui| {
                        vals.0 = num_in(ui, t("输入电压"), &mut c.div_vin, "V", 70.0);
                        vals.1 = num_in(ui, t("目标输出"), &mut c.div_vout, "V", 70.0);
                    });
                    ui.horizontal(|ui| {
                        vals.2 = num_in(ui, t("电阻范围"), &mut c.div_lo, "", 70.0);
                        ui.label("–");
                        vals.3 = num_in(ui, "", &mut c.div_hi, "Ω", 70.0);
                        for e in [ESeries::E12, ESeries::E24, ESeries::E96] {
                            let mut on = c.div_e == e;
                            if toggle_chip(ui, &mut on, &format!("{e:?}"), e.tolerance()) {
                                c.div_e = e;
                            }
                        }
                    });
                });
                ui.add_space(10.0);
                let (Some(vin), Some(vout), Some(lo), Some(hi)) = vals else { return };
                let res = hwcalc::divider(vin, vout, c.div_e, lo, hi, 8);
                if res.is_empty() {
                    ui.label(RichText::new(t("输出电压要小于输入电压")).color(WARN()));
                    return;
                }
                ui.horizontal_top(|ui| {
                    divider_sketch(ui, &res[0], vin);
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        let cols = [("R1", 90.0), ("R2", 90.0), (t("输出"), 100.0), (t("误差"), 80.0), (t("电流"), 90.0)];
                        header_row(ui, &cols);
                        for (i, d) in res.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let col = if i == 0 { ACCENT() } else { TEXT() };
                                cell(ui, 90.0, RichText::new(fmt_si(d.r1, "Ω")).font(MONO(12.5)).color(col));
                                cell(ui, 90.0, RichText::new(fmt_si(d.r2, "Ω")).font(MONO(12.5)).color(col));
                                cell(ui, 100.0, RichText::new(format!("{:.4} V", d.vout)).font(MONO(12.0)).color(TEXT2()));
                                cell(ui, 80.0, RichText::new(format!("{:+.2}%", d.err)).font(MONO(12.0)).color(if d.err.abs() < 1.0 { OK() } else { WARN() }));
                                cell(ui, 90.0, RichText::new(fmt_si(d.current, "A")).font(MONO(12.0)).color(TEXT3()));
                            });
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(t("Vout = Vin × R2 / (R1 + R2)；误差相近时优先总阻值适中的组合")).small().color(TEXT3()));
                    });
                });
            }
            1 => {
                let mut vals = (None, None, None);
                settings_card(ui, |ui| {
                    ui.horizontal(|ui| {
                        vals.0 = num_in(ui, t("电源"), &mut c.led_vs, "V", 60.0);
                        vals.1 = num_in(ui, t("LED 压降"), &mut c.led_vf, "V", 60.0);
                        vals.2 = num_in(ui, t("电流"), &mut c.led_i, "A", 60.0);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("常见压降")).small().color(TEXT3()));
                        for (n, v) in [(t("红"), "2.0"), (t("黄"), "2.1"), (t("绿"), "2.2"), (t("蓝 / 白"), "3.0"), (t("红外"), "1.3")] {
                            if ui.small_button(format!("{n} {v}V")).clicked() {
                                c.led_vf = v.into();
                            }
                        }
                    });
                });
                ui.add_space(10.0);
                let (Some(vs), Some(vf), Some(i)) = vals else { return };
                let Some(l) = hwcalc::led(vs, vf, i, ESeries::E24) else {
                    ui.label(RichText::new(t("电源电压要高于 LED 压降")).color(WARN()));
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    stat(ui, t("计算值"), &fmt_si(l.exact, "Ω"), None);
                    stat(
                        ui,
                        t("选用（E24）"),
                        &fmt_si(l.std, "Ω"),
                        Some(RichText::new(tf("实际 {}", &[&fmt_si(l.current, "A")])).small().color(TEXT3())),
                    );
                    let pkg = if l.power < 0.05 {
                        "0402 / 0603"
                    } else if l.power < 0.1 {
                        "0603"
                    } else if l.power < 0.125 {
                        "0805"
                    } else if l.power < 0.25 {
                        "1206"
                    } else {
                        "2512+"
                    };
                    stat(ui, t("电阻功耗"), &fmt_si(l.power, "W"), Some(RichText::new(tf("封装 {} 以上", &[&pkg])).small().color(TEXT3())));
                });
            }
            _ => {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("阻值")).color(TEXT2()));
                    changed |= ui.add_sized(vec2(120.0, 26.0), egui::TextEdit::singleline(&mut c.band_val).font(MONO(14.0))).changed();
                    ui.label(RichText::new("Ω").color(TEXT3()));
                    ui.add_space(10.0);
                    toggle_chip(ui, &mut c.band_five, t("五环"), t("精密电阻（±1%）用五环"));
                });
                let _ = changed;
                ui.add_space(14.0);
                let Some(r) = parse_si(&c.band_val) else {
                    ui.label(RichText::new(t("输入阻值，比如 4.7k、220、1M")).color(TEXT3()));
                    return;
                };
                match hwcalc::bands(r, c.band_five) {
                    None => {
                        ui.label(RichText::new(t("超出色环能表示的范围")).color(WARN()));
                    }
                    Some((digits, exp)) => {
                        let mut idx: Vec<usize> = digits.iter().map(|d| *d as usize).collect();
                        idx.push(match exp {
                            -1 => 10,
                            -2 => 11,
                            e => e as usize,
                        });
                        idx.push(if c.band_five { 1 } else { 10 });
                        resistor_bands(ui, &idx);
                        ui.add_space(8.0);
                        let names: Vec<String> = idx
                            .iter()
                            .map(|i| match i {
                                10 => t("金").to_string(),
                                11 => t("银").to_string(),
                                i => t(hwcalc::COLORS[*i]).to_string(),
                            })
                            .collect();
                        ui.label(RichText::new(names.join(" · ")).font(FontId::new(15.0, bold())).color(TEXT()));
                        let sig: u64 = digits.iter().fold(0, |a, d| a * 10 + *d as u64);
                        let actual = sig as f64 * 10f64.powi(exp);
                        ui.label(
                            RichText::new(format!("{}  {}", fmt_si(actual, "Ω"), if c.band_five { "±1%" } else { "±5%" }))
                                .font(MONO(13.0))
                                .color(TEXT2()),
                        );
                        if (actual - r).abs() / r > 0.001 {
                            ui.label(RichText::new(tf("{} 不是整数位，按 {} 显示", &[&c.band_val, &fmt_si(actual, "Ω")])).small().color(WARN()));
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ ADC / NTC

    fn tb_adc(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            eyebrow(ui, "ADC", "");
            settings_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("位数")).color(TEXT2()));
                    egui::ComboBox::from_id_salt("adc_bits").width(70.0).selected_text(format!("{}", c.adc_bits)).show_ui(ui, |ui| {
                        for b in [8u32, 10, 12, 14, 16, 24] {
                            ui.selectable_value(&mut c.adc_bits, b, b.to_string());
                        }
                    });
                    ui.add_space(8.0);
                    let vref = num_in(ui, t("参考电压"), &mut c.adc_vref, "V", 60.0);
                    let raw = num_in(ui, t("原始值"), &mut c.adc_raw, "", 80.0);
                    let r1 = parse_si(&c.adc_r1);
                    let r2 = parse_si(&c.adc_r2);
                    ui.label(RichText::new(t("前级分压")).color(TEXT2()))
                        .on_hover_text(t("被测电压经过 R1（上）R2（下）分压后接 ADC；不用就留空"));
                    ui.add_sized(vec2(60.0, 24.0), egui::TextEdit::singleline(&mut c.adc_r1).font(MONO(12.0)).hint_text("R1"));
                    ui.add_sized(vec2(60.0, 24.0), egui::TextEdit::singleline(&mut c.adc_r2).font(MONO(12.0)).hint_text("R2"));
                    if let (Some(vref), Some(raw)) = (vref, raw) {
                        let full = ((1u64 << c.adc_bits) - 1) as f64;
                        let v = raw / full * vref;
                        let k = match (r1, r2) {
                            (Some(a), Some(b)) if b > 0.0 => (a + b) / b,
                            _ => 1.0,
                        };
                        ui.end_row();
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            result_row(ui, t("ADC 引脚电压"), &format!("{v:.4} V"), "");
                            if k != 1.0 {
                                result_row(ui, t("被测电压"), &format!("{:.4} V", v * k), &format!("× {k:.4}"));
                            }
                            result_row(ui, t("1 LSB"), &fmt_si(vref / full * k, "V"), &tf("满量程 {}", &[&full]));
                        });
                    }
                });
            });
            ui.add_space(14.0);
            eyebrow(ui, "NTC", "");
            let n = &mut c.ntc;
            settings_card(ui, |ui| {
                ui.horizontal(|ui| {
                    si_edit(ui, "ntc_r25", "R25", &mut n.r25, "Ω", 70.0);
                    if !n.use_table {
                        ui.label(RichText::new(t("B 值")).color(TEXT2()));
                        ui.add(egui::DragValue::new(&mut n.beta).range(1000.0..=10000.0).speed(5.0));
                        ui.add_space(8.0);
                    }
                    si_edit(ui, "ntc_rs", t("串联电阻"), &mut n.rs, "Ω", 70.0);
                    let mut low = n.ntc_low;
                    if toggle_chip(ui, &mut low, if n.ntc_low { t("NTC 接地") } else { t("NTC 接电源") }, t("NTC 接地：温度越高 ADC 值越小")) {
                        n.ntc_low = low;
                    }
                    toggle_chip(ui, &mut n.use_table, t("用阻值表"), t("厂家给的 R-T 表比 B 值准；每行“温度, 阻值”"));
                });
                n.vref = parse_si(&c.adc_vref).unwrap_or(3.3);
                n.bits = c.adc_bits;
                if n.use_table {
                    ui.add(
                        egui::TextEdit::multiline(&mut n.table)
                            .font(MONO(12.0))
                            .desired_rows(4)
                            .desired_width(f32::INFINITY)
                            .hint_text("-20, 67.77k\n0, 27.22k\n25, 10k\n50, 4.16k\n100, 0.973k"),
                    );
                }
                ui.label(RichText::new(t("ADC 位数和参考电压用上面的设置")).small().color(TEXT3()));
            });
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    ui.horizontal(|ui| {
                        if let Some(raw) = num_in(ui, t("原始值"), &mut c.ntc_raw, "", 80.0) {
                            let tc = n.t_of_raw(raw);
                            ui.label(RichText::new(if tc.is_finite() { format!("= {tc:.2} °C") } else { "—".into() }).font(MONO(13.0)).color(ACCENT()));
                        }
                    });
                    ui.horizontal(|ui| {
                        if let Some(tc) = num_in(ui, t("温度"), &mut c.ntc_t, "°C", 60.0) {
                            ui.label(RichText::new(format!("= {:.0}", n.raw_at(tc))).font(MONO(13.0)).color(ACCENT()));
                            ui.label(RichText::new(fmt_si(n.r_at(tc), "Ω")).small().color(TEXT3()));
                        }
                    });
                });
                ntc_plot(ui, n, c.ntc_lo, c.ntc_hi);
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t("查找表")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.ntc_lo).range(-60..=200).suffix(" °C"));
                ui.label("–");
                ui.add(egui::DragValue::new(&mut c.ntc_hi).range(-60..=300).suffix(" °C"));
                ui.label(RichText::new(t("步长")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.ntc_step).range(1..=20).suffix(" °C"));
            });
            if c.ntc_hi > c.ntc_lo {
                code_block(ui, &n.c_table(c.ntc_lo, c.ntc_hi, c.ntc_step), t("C 查找表"), "ntc_table.h");
            }
        });
    }

    // ------------------------------------------------------------ PWM

    fn tb_pwm(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        let mut vals = (None, None);
        settings_card(ui, |ui| {
            ui.horizontal(|ui| {
                vals.0 = num_in(ui, t("定时器时钟"), &mut c.pwm_clk, "Hz", 80.0);
                vals.1 = num_in(ui, t("目标频率"), &mut c.pwm_f, "Hz", 80.0);
                ui.label(RichText::new(t("占空比")).color(TEXT2()));
                ui.add(egui::DragValue::new(&mut c.pwm_duty).range(0.0..=100.0).speed(0.5).suffix(" %"));
                ui.add_space(8.0);
                toggle_chip(ui, &mut c.pwm_32, t("32 位计数器"), t("STM32 的 TIM2 / TIM5 是 32 位"));
            });
        });
        let (Some(clk), Some(f)) = vals else { return };
        let arr_max = if c.pwm_32 { u32::MAX as u64 } else { 65535 };
        let res = hwcalc::pwm(clk, f, c.pwm_duty, 65535, arr_max, 6);
        ui.add_space(10.0);
        if res.is_empty() {
            ui.label(RichText::new(t("这个频率做不到")).color(WARN()));
            return;
        }
        let b = &res[0];
        ui.horizontal_wrapped(|ui| {
            stat(ui, "PSC", &b.psc.to_string(), None);
            stat(ui, "ARR", &b.arr.to_string(), None);
            stat(
                ui,
                "CCR",
                &b.ccr.to_string(),
                Some(RichText::new(format!("{:.2}%", b.ccr as f64 * 100.0 / b.steps as f64)).small().color(TEXT3())),
            );
            stat(
                ui,
                t("实际频率"),
                &fmt_si(b.freq, "Hz"),
                Some(RichText::new(format!("{:+.4}%", b.err)).small().color(if b.err.abs() < 0.01 { OK() } else { WARN() })),
            );
            stat(
                ui,
                t("占空比分辨率"),
                &format!("{:.1} bit", (b.steps as f64).log2()),
                Some(RichText::new(tf("{} 级", &[&b.steps])).small().color(TEXT3())),
            );
        });
        ui.add_space(6.0);
        pwm_wave(ui, b.ccr as f64 / b.steps as f64);
        ui.add_space(6.0);
        hint_line(
            ui,
            "STM32 HAL:",
            &format!("htim.Init.Prescaler = {}; htim.Init.Period = {}; __HAL_TIM_SET_COMPARE(&htim, TIM_CHANNEL_1, {});", b.psc, b.arr, b.ccr),
        );
        ui.add_space(10.0);
        eyebrow(ui, t("其他组合"), "");
        let cols = [("PSC", 80.0), ("ARR", 100.0), ("CCR", 90.0), (t("实际频率"), 120.0), (t("误差"), 90.0), (t("分辨率"), 90.0)];
        header_row(ui, &cols);
        for r in &res {
            ui.horizontal(|ui| {
                cell(ui, 80.0, RichText::new(r.psc.to_string()).font(MONO(12.0)).color(TEXT()));
                cell(ui, 100.0, RichText::new(r.arr.to_string()).font(MONO(12.0)).color(TEXT()));
                cell(ui, 90.0, RichText::new(r.ccr.to_string()).font(MONO(12.0)).color(TEXT2()));
                cell(ui, 120.0, RichText::new(fmt_si(r.freq, "Hz")).font(MONO(12.0)).color(TEXT2()));
                cell(
                    ui,
                    90.0,
                    RichText::new(format!("{:+.4}%", r.err)).font(MONO(12.0)).color(if r.err.abs() < 0.01 { OK() } else { WARN() }),
                );
                cell(ui, 90.0, RichText::new(format!("{:.1} bit", (r.steps as f64).log2())).font(MONO(12.0)).color(TEXT3()));
            });
        }
    }

    // ------------------------------------------------------------ I2C

    fn tb_i2c(&mut self, ui: &mut egui::Ui) {
        let c = &mut self.s.tb.z.x;
        let mut vals = (None, None);
        settings_card(ui, |ui| {
            ui.horizontal(|ui| {
                vals.0 = num_in(ui, t("电源"), &mut c.i2c_vdd, "V", 60.0);
                vals.1 = num_in(ui, t("总线电容"), &mut c.i2c_cb, "F", 70.0);
                for m in I2cMode::ALL {
                    let mut on = c.i2c_mode == m;
                    if toggle_chip(ui, &mut on, m.label(), "") {
                        c.i2c_mode = m;
                    }
                }
            });
            ui.label(RichText::new(t("总线电容 = 走线 + 每个器件的引脚（约 10 pF）；不知道时按 100–200 pF 估")).small().color(TEXT3()));
        });
        let (Some(vdd), Some(cb)) = vals else { return };
        let (lo, hi) = hwcalc::i2c_pullup(vdd, cb, c.i2c_mode);
        ui.add_space(10.0);
        if lo > hi {
            ui.label(RichText::new(t("总线电容太大，这个速率下找不到合适的上拉电阻；降低速率或者减少负载")).color(ERR()));
            return;
        }
        let std: Vec<f64> = ESeries::E24.values(lo, hi);
        let pick = std.iter().copied().find(|v| *v >= (lo * hi).sqrt()).or(std.last().copied());
        ui.horizontal_wrapped(|ui| {
            stat(ui, t("最小"), &fmt_si(lo, "Ω"), Some(RichText::new(t("再小灌电流超标")).small().color(TEXT3())));
            stat(ui, t("最大"), &fmt_si(hi, "Ω"), Some(RichText::new(t("再大上升沿太慢")).small().color(TEXT3())));
            if let Some(p) = pick {
                stat(ui, t("建议"), &fmt_si(p, "Ω"), Some(RichText::new(t("E24 标准值，留了余量")).small().color(OK())));
            }
        });
        ui.add_space(8.0);
        // 范围条：对数刻度上标出所有 E24 值
        let w = ui.available_width().min(900.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 46.0), Sense::hover());
        let p = ui.painter();
        let (a, b) = ((lo / 3.0).log10(), (hi * 3.0).log10());
        let x = |v: f64| rect.left() + ((v.log10() - a) / (b - a)) as f32 * rect.width();
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 14.0), vec2(rect.width(), 8.0)), 4.0, BG3());
        p.rect_filled(Rect::from_min_max(pos2(x(lo), rect.top() + 14.0), pos2(x(hi), rect.top() + 22.0)), 4.0, OK().gamma_multiply(0.6));
        // 标签互相不重叠：建议值一定标，其它的离已标的够远才标
        let mut used: Vec<f32> = pick.map(|v| vec![x(v)]).unwrap_or_default();
        for v in ESeries::E24.values(lo / 3.0, hi * 3.0) {
            let inr = v >= lo && v <= hi;
            p.line_segment(
                [pos2(x(v), rect.top() + 10.0), pos2(x(v), rect.top() + 26.0)],
                st(1.0, if inr { TEXT() } else { TEXT3().gamma_multiply(0.5) }),
            );
            let free = Some(v) == pick || inr && used.iter().all(|u| (u - x(v)).abs() > 40.0);
            if free {
                used.push(x(v));
                p.text(
                    pos2(x(v), rect.top() + 30.0),
                    Align2::CENTER_TOP,
                    fmt_si(v, "").trim(),
                    FontId::new(10.0, FontFamily::Monospace),
                    if Some(v) == pick { ACCENT() } else { TEXT2() },
                );
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(tf(
                "Rp(min) = (Vdd − Vol) / Iol，Rp(max) = tr / (0.8473 × Cb)；tr = {}，Iol = {}",
                &[&fmt_si(c.i2c_mode.tr(), "s"), &fmt_si(c.i2c_mode.iol(), "A")],
            ))
            .small()
            .color(TEXT3()),
        );
    }
}

/// 寄存器详情：地址、说明、位图、字段表；改值或点位都会更新
fn reg_detail(ui: &mut egui::Ui, p: &svd::Periph, r: &svd::Reg, val: &mut u64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}->{}", p.name, r.name)).font(FontId::new(16.0, mono_bold())).color(TEXT()));
        copy_label(ui, &hx(p.base + r.offset));
        pill(ui, &r.access, TEXT2());
    });
    if !r.desc.is_empty() {
        ui.label(RichText::new(&r.desc).color(TEXT2()));
    }
    ui.add_space(6.0);
    let digits = (r.size as usize).div_ceil(4);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("值")).color(TEXT2()));
        let id = egui::Id::new("svd_val_text");
        let mut s = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| format!("0x{:0digits$X}", *val));
        let resp = ui.add_sized(vec2(140.0, 26.0), egui::TextEdit::singleline(&mut s).font(MONO(14.0)));
        if resp.changed() {
            if let Some(v) = svd::num(&s.replace(['_', ' '], "")) {
                *val = v;
            }
        }
        if resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, s));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        ui.label(RichText::new(format!("= {}", *val)).font(MONO(12.0)).color(TEXT3()));
        if ui.small_button(t("复位值")).clicked() {
            *val = r.reset;
        }
        if ui.small_button("0").clicked() {
            *val = 0;
        }
    });
    ui.add_space(6.0);
    // 位图：一行 16 位，高位在左
    let bits = r.size.clamp(8, 32);
    let rows = bits.div_ceil(16);
    let cw = ((ui.available_width() - 4.0) / 16.0).clamp(22.0, 44.0);
    for row in 0..rows {
        let hi = bits - row * 16;
        let lo = hi.saturating_sub(16);
        let (rect, _) = ui.allocate_exact_size(vec2(cw * 16.0, 50.0), Sense::hover());
        let pnt = ui.painter().clone();
        for b in (lo..hi).rev() {
            let i = hi - 1 - b;
            let cr = Rect::from_min_size(rect.min + vec2(i as f32 * cw, 14.0), vec2(cw - 2.0, 20.0));
            let fi = r.fields.iter().position(|f| b >= f.lsb && b < f.lsb + f.width);
            let on = (*val >> b) & 1 == 1;
            let col = fi.map(series).unwrap_or(TEXT3());
            pnt.rect_filled(cr, 3.0, if on { col.gamma_multiply(0.85) } else { BG2() });
            pnt.rect_stroke(cr, 3.0, st(1.0, if fi.is_some() { col.gamma_multiply(0.5) } else { LINE() }), egui::StrokeKind::Inside);
            pnt.text(
                cr.center(),
                Align2::CENTER_CENTER,
                if on { "1" } else { "0" },
                FontId::new(11.0, FontFamily::Monospace),
                if on { ON_ACCENT() } else { TEXT3() },
            );
            pnt.text(
                pos2(cr.center().x, rect.top() + 6.0),
                Align2::CENTER_CENTER,
                b.to_string(),
                FontId::new(9.0, FontFamily::Monospace),
                TEXT3(),
            );
            let resp = ui.interact(cr, egui::Id::new(("svd_bit", b)), Sense::click());
            if resp.clicked() {
                *val ^= 1 << b;
            }
            if let Some(f) = fi.map(|i| &r.fields[i]) {
                resp.on_hover_text(format!("{}  [{}]\n{}", f.name, b, f.desc));
            }
        }
        // 字段名写在下面
        for (k, f) in r.fields.iter().enumerate() {
            let (fl, fh) = (f.lsb.max(lo), (f.lsb + f.width).min(hi));
            if fl >= fh {
                continue;
            }
            let x0 = rect.left() + (hi - fh) as f32 * cw;
            let x1 = rect.left() + (hi - fl) as f32 * cw - 2.0;
            let y = rect.top() + 40.0;
            pnt.line_segment([pos2(x0, y - 3.0), pos2(x1, y - 3.0)], st(1.5, series(k)));
            let name = if (x1 - x0) < f.name.len() as f32 * 6.0 {
                truncate(&f.name, ((x1 - x0) / 6.5).max(1.0) as usize)
            } else {
                f.name.clone()
            };
            pnt.text(pos2((x0 + x1) / 2.0, y + 3.0), Align2::CENTER_CENTER, name, FontId::new(9.5, FontFamily::Monospace), series(k));
        }
    }
    ui.add_space(8.0);
    let cols = [(t("位"), 60.0), (t("字段"), 140.0), (t("值"), 110.0), ("", 130.0), (t("说明"), 380.0)];
    header_row(ui, &cols);
    egui::ScrollArea::vertical().id_salt("svd_fields").auto_shrink([false, false]).show(ui, |ui| {
        for (k, f) in r.fields.iter().enumerate() {
            ui.horizontal(|ui| {
                let bits = if f.width == 1 { f.lsb.to_string() } else { format!("{}:{}", f.lsb + f.width - 1, f.lsb) };
                cell(ui, 60.0, RichText::new(bits).font(MONO(11.5)).color(TEXT3()));
                cell(ui, 140.0, RichText::new(&f.name).font(MONO(12.0)).color(series(k)));
                let mut x = f.get(*val);
                let max = if f.width >= 64 { u64::MAX } else { (1u64 << f.width) - 1 };
                ui.allocate_ui(vec2(110.0, 20.0), |ui| {
                    ui.set_width(110.0);
                    let dv = if f.width > 4 {
                        egui::DragValue::new(&mut x).range(0..=max).hexadecimal(1, false, true)
                    } else {
                        egui::DragValue::new(&mut x).range(0..=max)
                    };
                    if ui.add(dv).changed() {
                        *val = f.set(*val, x);
                    }
                });
                if f.enums.is_empty() {
                    cell(ui, 130.0, RichText::new(""));
                } else {
                    ui.allocate_ui(vec2(130.0, 20.0), |ui| {
                        ui.set_width(130.0);
                        let cur = f.enum_name(x).map(|e| e.name.clone()).unwrap_or_else(|| "?".into());
                        egui::ComboBox::from_id_salt(("svd_enum", k)).width(120.0).selected_text(cur).show_ui(ui, |ui| {
                            for e in &f.enums {
                                if ui.selectable_label(e.value == x, format!("{} = {}", e.name, e.value)).on_hover_text(&e.desc).clicked() {
                                    *val = f.set(*val, e.value);
                                }
                            }
                        });
                    });
                }
                let desc = f
                    .enum_name(x)
                    .filter(|e| !e.desc.is_empty())
                    .map(|e| format!("{} — {}", f.desc, e.desc))
                    .unwrap_or_else(|| f.desc.clone());
                ui.add(egui::Label::new(RichText::new(desc).small().color(TEXT2())).truncate());
            });
        }
    });
}

/// 分压示意图
fn divider_sketch(ui: &mut egui::Ui, d: &hwcalc::DivPick, vin: f64) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, 200.0), Sense::hover());
    let p = ui.painter();
    let x = rect.left() + 40.0;
    let s = st(1.4, TEXT2());
    let font = FontId::new(11.5, FontFamily::Monospace);
    p.text(pos2(x, rect.top() + 6.0), Align2::CENTER_CENTER, format!("{vin} V"), font.clone(), TEXT());
    p.line_segment([pos2(x, rect.top() + 14.0), pos2(x, rect.top() + 34.0)], s);
    let r1 = Rect::from_center_size(pos2(x, rect.top() + 60.0), vec2(16.0, 50.0));
    p.rect_stroke(r1, 2.0, st(1.4, ACCENT()), egui::StrokeKind::Middle);
    p.text(r1.right_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, format!("R1 {}", fmt_si(d.r1, "Ω")), font.clone(), ACCENT());
    p.line_segment([pos2(x, rect.top() + 85.0), pos2(x, rect.top() + 115.0)], s);
    p.circle_filled(pos2(x, rect.top() + 100.0), 3.0, TEXT());
    p.line_segment([pos2(x, rect.top() + 100.0), pos2(x + 30.0, rect.top() + 100.0)], s);
    p.text(pos2(x + 34.0, rect.top() + 100.0), Align2::LEFT_CENTER, format!("{:.3} V", d.vout), font.clone(), OK());
    let r2 = Rect::from_center_size(pos2(x, rect.top() + 140.0), vec2(16.0, 50.0));
    p.rect_stroke(r2, 2.0, st(1.4, VIOLET()), egui::StrokeKind::Middle);
    p.text(r2.right_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, format!("R2 {}", fmt_si(d.r2, "Ω")), font, VIOLET());
    p.line_segment([pos2(x, rect.top() + 165.0), pos2(x, rect.top() + 185.0)], s);
    for (i, w) in [14.0, 9.0, 4.0].iter().enumerate() {
        let y = rect.top() + 185.0 + i as f32 * 4.0;
        p.line_segment([pos2(x - w, y), pos2(x + w, y)], s);
    }
}

/// 画一个带色环的电阻
fn resistor_bands(ui: &mut egui::Ui, idx: &[usize]) {
    let (rect, _) = ui.allocate_exact_size(vec2(300.0, 60.0), Sense::hover());
    let p = ui.painter();
    let cy = rect.center().y;
    p.line_segment([pos2(rect.left(), cy), pos2(rect.right(), cy)], st(3.0, TEXT3()));
    let body = Rect::from_center_size(rect.center(), vec2(200.0, 44.0));
    p.rect_filled(body, 18.0, Color32::from_rgb(0xD9, 0xC2, 0x9A));
    let n = idx.len();
    for (i, c) in idx.iter().enumerate() {
        // 最后一环（误差）离开一点
        let x = if i + 1 == n { body.right() - 30.0 } else { body.left() + 36.0 + i as f32 * 24.0 };
        let [r, g, b] = hwcalc::RGB[*c];
        let band = Rect::from_center_size(pos2(x, cy), vec2(12.0, 44.0));
        p.rect_filled(band, 0.0, Color32::from_rgb(r, g, b));
    }
    p.rect_stroke(body, 18.0, st(1.0, shade(60)), egui::StrokeKind::Inside);
}

/// NTC 曲线：横轴温度，纵轴 ADC 值
fn ntc_plot(ui: &mut egui::Ui, n: &Ntc, lo: i32, hi: i32) {
    let w = (ui.available_width() - 10.0).clamp(200.0, 520.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 150.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, R_CARD, BG0());
    p.rect_stroke(rect, R_CARD, st(1.0, LINE()), egui::StrokeKind::Inside);
    if hi <= lo {
        return;
    }
    let inner = rect.shrink2(vec2(34.0, 14.0));
    let full = n.full();
    let x = |t: f64| inner.left() + ((t - lo as f64) / (hi - lo) as f64) as f32 * inner.width();
    let y = |v: f64| inner.bottom() - (v / full) as f32 * inner.height();
    let pts: Vec<egui::Pos2> = (0..=100)
        .map(|i| lo as f64 + (hi - lo) as f64 * i as f64 / 100.0)
        .map(|t| pos2(x(t), y(n.raw_at(t).clamp(0.0, full))))
        .collect();
    let font = FontId::new(9.5, FontFamily::Monospace);
    for t in [lo, (lo + hi) / 2, hi] {
        p.text(pos2(x(t as f64), rect.bottom() - 2.0), Align2::CENTER_BOTTOM, format!("{t}°"), font.clone(), TEXT3());
    }
    p.text(pos2(rect.left() + 4.0, inner.top()), Align2::LEFT_CENTER, format!("{full}"), font.clone(), TEXT3());
    p.text(pos2(rect.left() + 4.0, inner.bottom()), Align2::LEFT_CENTER, "0", font.clone(), TEXT3());
    p.add(Shape::line(pts, st(1.8, ACCENT())));
    if let Some(h) = resp.hover_pos().filter(|h| inner.contains(*h)) {
        let tc = lo as f64 + ((h.x - inner.left()) / inner.width()) as f64 * (hi - lo) as f64;
        let v = n.raw_at(tc);
        let pt = pos2(x(tc), y(v));
        p.circle_filled(pt, 3.5, ACCENT());
        p.text(
            pt + vec2(6.0, -10.0),
            Align2::LEFT_BOTTOM,
            format!("{tc:.1}°C  {v:.0}"),
            FontId::new(11.0, FontFamily::Monospace),
            TEXT(),
        );
    }
}

/// PWM 波形示意
fn pwm_wave(ui: &mut egui::Ui, duty: f64) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 40.0), Sense::hover());
    let p = ui.painter();
    let per = rect.width() / 3.0;
    let mut pts = vec![];
    for k in 0..3 {
        let x0 = rect.left() + k as f32 * per;
        let xh = x0 + per * duty as f32;
        pts.extend([
            pos2(x0, rect.bottom() - 4.0),
            pos2(x0, rect.top() + 4.0),
            pos2(xh, rect.top() + 4.0),
            pos2(xh, rect.bottom() - 4.0),
            pos2(x0 + per, rect.bottom() - 4.0),
        ]);
    }
    p.add(Shape::line(pts, st(1.6, ACCENT())));
}

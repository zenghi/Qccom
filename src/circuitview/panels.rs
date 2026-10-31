//! 左边元件库、右边属性 / 知识卡。

use eframe::egui::{self, vec2, Color32, CornerRadius, FontId, RichText, Sense, Stroke};

use super::{cat_header, draw, lib_row, CircuitView, RTab, Sel};
use crate::circuit::lib::{self, cap, Cat, PropKind};
use crate::circuit::{fmt_si, parse_si, WIRE_COLORS};
use crate::i18n::t;
use crate::theme::*;

/// 选项值的显示名
fn choice_label(label: &str, opt: &str) -> String {
    let named = match opt {
        "usb" => t("USB 供电"),
        "ext" => t("外部供电"),
        "red" => t("红"),
        "yellow" => t("黄"),
        "green" => t("绿"),
        "blue" => t("蓝"),
        "white" => t("白"),
        "v" => t("电压"),
        "a" => t("电流"),
        "ohm" => t("电阻"),
        "beep" => t("通断"),
        "cc" => t("共阴"),
        "ca" => t("共阳"),
        "pcb" => t("PCB 天线"),
        "ceramic" => t("陶瓷天线"),
        "ipex" => t("IPEX 外接天线"),
        "aa" => t("1 节 AA 碱性电池"),
        "2aa" => t("2 节 AA 碱性电池"),
        "3aa" => t("3 节 AA 碱性电池"),
        "li" => t("锂电池 3.7V"),
        "9v" => t("9V 电池"),
        "cr2032" => t("CR2032 纽扣电池"),
        "18650x2" => t("2 节 18650 串联"),
        "low" => t("低电平触发"),
        "high" => t("高电平触发"),
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    if opt.parse::<f64>().is_ok() {
        let unit = if label.contains("电压") || label.contains("电平") {
            " V"
        } else if label.contains("功率") {
            " W"
        } else if label.contains("电流") {
            " mA"
        } else if label.contains("尺寸") {
            "″"
        } else {
            ""
        };
        return format!("{opt}{unit}");
    }
    opt.to_string()
}

/// 引脚能力 → 简短文字
fn caps_text(c: u32) -> String {
    let names: [(u32, &str); 22] = [
        (cap::ADC, "ADC"),
        (cap::PWM, "PWM"),
        (cap::DAC, "DAC"),
        (cap::TX, "TX"),
        (cap::RX, "RX"),
        (cap::SDA, "SDA"),
        (cap::SCL, "SCL"),
        (cap::MOSI, "MOSI"),
        (cap::MISO, "MISO"),
        (cap::SCK, "SCK"),
        (cap::CS, "CS"),
        (cap::FT, t("耐 5V")),
        (cap::BOOT, t("启动脚")),
        (cap::INONLY, t("只能输入")),
        (cap::OD, t("开漏")),
        (cap::LED, t("板载 LED")),
        (cap::PWR, t("电源")),
        (cap::GND, t("地")),
        (cap::VIN, "VIN"),
        (cap::RST, t("复位")),
        (cap::SIN, t("输入")),
        (cap::SOUT, t("输出")),
    ];
    names.iter().filter(|(b, _)| c & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(" · ")
}

impl CircuitView {
    pub(super) fn library(&mut self, ui: &mut egui::Ui) {
        // 元件库 / 示例：同一块地方切换，不另加按钮
        ui.horizontal(|ui| {
            for (ex, name) in [(false, t("元件库")), (true, t("示例"))] {
                let on = self.lib_examples == ex;
                let r = ui.add(egui::Label::new(RichText::new(name).size(14.0).strong().color(if on { TEXT() } else { TEXT3() })).sense(Sense::click()));
                if on {
                    let y = r.rect.bottom() + 3.0;
                    ui.painter().line_segment([egui::pos2(r.rect.left(), y), egui::pos2(r.rect.right(), y)], Stroke::new(2.0, ACCENT()));
                }
                if r.clicked() {
                    self.lib_examples = ex;
                }
                ui.add_space(10.0);
            }
        });
        ui.add_space(8.0);
        let hint = if self.lib_examples { t("搜索示例") } else { t("搜索元件、型号") };
        ui.add(egui::TextEdit::singleline(&mut self.lib_filter).hint_text(hint).desired_width(f32::INFINITY).margin(vec2(8.0, 5.0)));
        ui.add_space(6.0);
        if self.lib_examples {
            self.examples_list(ui);
            return;
        }
        let q = self.lib_filter.trim().to_lowercase();
        let matches = |d: &lib::PartDef| {
            if q.is_empty() {
                return true;
            }
            let name = t(d.zh);
            let hay = format!("{} {} {} {} {} {}", d.zh, name, d.key, d.kw, crate::i18n::py_initials(d.zh), t(d.cat.zh())).to_lowercase();
            q.split_whitespace().all(|w| hay.contains(w))
        };
        let mut pick: Option<(&'static str, bool)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut any = false;
            for cat in Cat::ALL {
                let defs: Vec<&'static lib::PartDef> = lib::all().iter().filter(|d| d.cat == cat && matches(d)).collect();
                if defs.is_empty() {
                    continue;
                }
                any = true;
                let open = !q.is_empty() || self.lib_open.contains(&cat);
                if cat_header(ui, t(cat.zh()), defs.len(), open).clicked() && q.is_empty() {
                    if open {
                        self.lib_open.remove(&cat);
                    } else {
                        self.lib_open.insert(cat);
                    }
                }
                if !open {
                    continue;
                }
                for d in defs {
                    let on = self.placing.is_some_and(|p| p.0 == d.key);
                    let r = lib_row(ui, t(d.zh), d.prefix, on);
                    if r.drag_started() {
                        pick = Some((d.key, true));
                    } else if r.clicked() {
                        pick = Some((d.key, false));
                    }
                }
                ui.add_space(4.0);
            }
            if !any {
                ui.label(RichText::new(t("没有匹配的元件")).color(TEXT3()));
            }
        });
        if let Some((k, drag)) = pick {
            if !drag && self.placing.is_some_and(|p| p.0 == k) {
                self.placing = None;
            } else {
                self.placing = Some((k, drag));
                self.draft = None;
                self.place_rot = 0;
            }
        }
    }

    fn examples_list(&mut self, ui: &mut egui::Ui) {
        use crate::circuit::examples;
        let q = self.lib_filter.trim().to_lowercase();
        let mut pick: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut any = false;
            for g in ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J'] {
                let list: Vec<&examples::Example> = examples::all()
                    .iter()
                    .filter(|e| e.group == g)
                    .filter(|e| {
                        q.is_empty() || {
                            let hay = format!("{} {} {} {} {}", e.id, e.zh, e.en, e.title(), crate::i18n::py_initials(&e.zh)).to_lowercase();
                            q.split_whitespace().all(|w| hay.contains(w))
                        }
                    })
                    .collect();
                if list.is_empty() {
                    continue;
                }
                any = true;
                let open = !q.is_empty() || self.ex_open.contains(&g);
                if cat_header(ui, &format!("{g}  {}", examples::group_name(g)), list.len(), open).clicked() && q.is_empty() {
                    if open {
                        self.ex_open.remove(&g);
                    } else {
                        self.ex_open.insert(g);
                    }
                }
                if !open {
                    continue;
                }
                for e in list {
                    let on = self.example.as_deref() == Some(e.id.as_str());
                    let dots = ["●○○", "●●○", "●●●"][(e.level.clamp(1, 3) - 1) as usize];
                    let r = lib_row(ui, &format!("{}  {}", e.id, e.title()), dots, on);
                    if r.clicked() {
                        pick = Some(e.id.clone());
                    }
                }
                ui.add_space(4.0);
            }
            if !any {
                ui.label(RichText::new(t("没有匹配的示例")).color(TEXT3()));
            }
        });
        if let Some(id) = pick {
            self.load_example(&id);
        }
    }

    /// 右侧「示例」页：原理、搭建步骤、试一试、学到什么
    fn example_ui(&mut self, ui: &mut egui::Ui) {
        use crate::circuit::examples;
        let Some(e) = self.example.as_deref().and_then(examples::find) else {
            ui.label(RichText::new(t("从左侧「示例」里选一个")).color(TEXT3()));
            return;
        };
        ui.label(RichText::new(format!("{}  {}", e.id, e.title())).size(15.0).strong().color(TEXT()));
        ui.label(RichText::new(examples::group_name(e.group)).size(11.5).color(TEXT3()));
        ui.add_space(8.0);
        let section = |ui: &mut egui::Ui, title: &str, body: &str| {
            if body.is_empty() {
                return;
            }
            ui.label(RichText::new(title).size(12.0).strong().color(ACCENT()));
            ui.add_space(2.0);
            ui.label(RichText::new(body).size(12.5).color(TEXT2()));
            ui.add_space(10.0);
        };
        section(ui, t("原理"), e.text("intro"));
        section(ui, t("试一试"), e.text("try"));
        section(ui, t("学到什么"), e.text("learn"));
        egui::CollapsingHeader::new(RichText::new(t("搭建步骤")).size(12.0).strong().color(ACCENT()))
            .id_salt("ex-steps")
            .default_open(false)
            .show(ui, |ui| {
                for (i, s) in e.steps(&self.c).iter().enumerate() {
                    ui.label(RichText::new(format!("{}. {s}", i + 1)).size(12.0).color(TEXT2()));
                }
            });
        ui.add_space(8.0);
        if ui.button(t("恢复成示例原样")).clicked() {
            let id = e.id.clone();
            self.load_example(&id);
        }
    }

    pub(super) fn side(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut tabs = vec![(RTab::Props, t("属性")), (RTab::Know, t("认识它"))];
            if self.example.is_some() {
                tabs.insert(0, (RTab::Example, t("示例")));
            }
            for (tab, name) in tabs {
                let on = self.tab == tab;
                let r = ui.add(egui::Label::new(RichText::new(name).size(13.0).color(if on { TEXT() } else { TEXT3() }).strong()).sense(Sense::click()));
                if on {
                    let y = r.rect.bottom() + 3.0;
                    ui.painter().line_segment([egui::pos2(r.rect.left(), y), egui::pos2(r.rect.right(), y)], Stroke::new(2.0, ACCENT()));
                }
                if r.clicked() {
                    self.tab = tab;
                }
                ui.add_space(8.0);
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
            RTab::Props => self.props_ui(ui),
            RTab::Know => self.know_ui(ui),
            RTab::Example => self.example_ui(ui),
        });
    }

    fn props_ui(&mut self, ui: &mut egui::Ui) {
        match self.sel.clone().as_slice() {
            [] => self.summary_ui(ui),
            [Sel::Part(id)] => self.part_props(ui, *id),
            [Sel::Wire(id)] => self.wire_props(ui, *id),
            [Sel::Note(id)] => {
                let id = *id;
                ui.label(RichText::new(t("文字说明")).size(13.0).strong().color(TEXT()));
                ui.add_space(4.0);
                if let Some(i) = self.c.notes.iter().position(|n| n.id == id) {
                    let mut s = self.c.notes[i].text.clone();
                    let r = ui.add(egui::TextEdit::multiline(&mut s).desired_width(f32::INFINITY).desired_rows(3));
                    if r.gained_focus() {
                        self.checkpoint();
                    }
                    if s != self.c.notes[i].text {
                        self.c.notes[i].text = s;
                        self.changed();
                    }
                }
            }
            many => {
                ui.label(RichText::new(format!("{} {}", many.len(), t("个对象已选中"))).color(TEXT2()));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(t("旋转")).clicked() {
                        self.rotate_sel(false);
                    }
                    if ui.button(t("删除")).clicked() {
                        self.delete_sel();
                    }
                });
            }
        }
    }

    fn summary_ui(&mut self, ui: &mut egui::Ui) {
        let n_parts = self.c.parts.iter().filter(|p| lib::def(&p.kind).is_some_and(|d| d.cat != Cat::Board)).count();
        ui.label(RichText::new(t("电路")).size(13.0).strong().color(TEXT()));
        ui.add_space(4.0);
        ui.label(RichText::new(format!("{} {} · {} {}", n_parts, t("个元件"), self.c.wires.len(), t("根导线"))).color(TEXT2()));
        ui.add_space(10.0);
        self.issues_ui(ui);
        ui.add_space(8.0);
        if let Some(sim) = &self.sim {
            super::power::power_ui(ui, &self.power, &self.c, sim);
            ui.add_space(6.0);
        }
        egui::CollapsingHeader::new(RichText::new(t("接实物测电流（INA219 / INA226）")).size(12.5).color(TEXT2()))
            .default_open(self.meter.running())
            .show(ui, |ui| self.meter.ui(ui));
        if self.sim.is_some() {
            return;
        }
        ui.add_space(12.0);
        ui.label(RichText::new(t("怎么用")).size(12.0).strong().color(TEXT2()));
        ui.add_space(2.0);
        for line in [
            t("1. 从左边把面包板、电源、元件拖到画布上"),
            t("2. 从引脚或面包板孔拖出导线，松手在另一个孔上"),
            t("3. 同一竖列的 5 个孔是通的，两边的长条是电源轨"),
            t("4. 点「通电」看结果：LED 会亮，接错会冒烟"),
            t("5. 选中元件可以在这里改阻值、颜色、型号"),
        ] {
            ui.label(RichText::new(line).size(12.0).color(TEXT3()));
        }
    }

    fn issues_ui(&mut self, ui: &mut egui::Ui) {
        use crate::circuit::check::Level;
        ui.label(RichText::new(t("接错检查")).size(12.5).strong().color(TEXT()));
        ui.add_space(2.0);
        let real: Vec<_> = self.issues.iter().filter(|i| i.level != Level::Tip).cloned().collect();
        if real.is_empty() {
            ui.label(RichText::new(format!("✓ {}", t("没发现接错"))).size(12.0).color(OK()));
        }
        let tips = self.issues.len() - real.len();
        let list: Vec<_> = if tips > 0 && tips <= 6 { self.issues.clone() } else { real };
        for i in list {
            let (icon, col) = match i.level {
                Level::Err => ("●", ERR()),
                Level::Warn => ("▲", WARN()),
                Level::Tip => ("○", TEXT3()),
            };
            let r = ui
                .horizontal_top(|ui| {
                    ui.label(RichText::new(icon).size(10.0).color(col));
                    ui.add(
                        egui::Label::new(RichText::new(&i.msg).size(12.0).color(if i.level == Level::Tip { TEXT2() } else { TEXT() }))
                            .wrap()
                            .sense(Sense::click()),
                    )
                })
                .inner;
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                self.sel = i.parts.iter().map(|id| Sel::Part(*id)).collect();
            }
        }
        if tips > 6 {
            ui.label(RichText::new(format!("{} {}", tips, t("个引脚还没接"))).size(11.5).color(TEXT3()));
        }
    }

    fn wire_props(&mut self, ui: &mut egui::Ui, id: u32) {
        let Some(wi) = self.c.wires.iter().position(|w| w.id == id) else { return };
        ui.label(RichText::new(t("导线")).size(13.0).strong().color(TEXT()));
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for (k, (name, c)) in WIRE_COLORS.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                let on = self.c.wires[wi].color as usize == k;
                ui.painter()
                    .circle(r.center(), 8.0, draw::rgb(*c), Stroke::new(if on { 2.0 } else { 1.0 }, if on { ACCENT() } else { LINE_HI() }));
                if resp.on_hover_text(format!("{} ({})", t(name), (k + 1) % 10)).clicked() {
                    self.set_wire_color(k as u8);
                }
            }
        });
        let pts = self.c.wires[wi].pts.clone();
        let len: f64 = pts.windows(2).map(|s| (((s[1].0 - s[0].0).pow(2) + (s[1].1 - s[0].1).pow(2)) as f64).sqrt()).sum();
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} {:.1} cm", t("长度约"), len * 0.254)).color(TEXT2()));
        if let Some(s) = &self.sim {
            if let Some(v) = s.nl.wire.get(&id).and_then(|n| s.net_v(*n)) {
                ui.label(RichText::new(format!("{} {}", t("电压"), fmt_si(v, "V"))).font(FontId::monospace(12.5)).color(ACCENT()));
            }
        }
        ui.add_space(8.0);
        if ui.button(t("删除导线")).clicked() {
            self.delete_sel();
        }
    }

    fn part_props(&mut self, ui: &mut egui::Ui, id: u32) {
        let Some(pi) = self.c.parts.iter().position(|p| p.id == id) else { return };
        let Some(def) = lib::def(&self.c.parts[pi].kind) else { return };
        let label = self.c.label(id);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&label).size(15.0).strong().color(ACCENT()));
            ui.label(RichText::new(t(def.zh)).size(13.0).color(TEXT()));
        });
        ui.label(RichText::new(t(def.cat.zh())).size(11.5).color(TEXT3()));
        ui.add_space(8.0);
        let grid = |ui: &mut egui::Ui, name: &str| {
            ui.label(RichText::new(name).size(12.0).color(TEXT2()));
        };
        egui::Grid::new(("cv-props", id)).num_columns(2).spacing(vec2(10.0, 8.0)).min_col_width(64.0).show(ui, |ui| {
            grid(ui, t("名字"));
            let mut l = self.c.parts[pi].label.clone();
            let r = ui.add(egui::TextEdit::singleline(&mut l).hint_text(&label).desired_width(140.0));
            if r.gained_focus() {
                self.checkpoint();
            }
            if l != self.c.parts[pi].label {
                self.c.parts[pi].label = l;
                self.changed();
            }
            ui.end_row();
            for (key, pd) in def.props.iter() {
                grid(ui, t(pd.label));
                let cur = self.c.parts[pi].prop(key).to_string();
                let mut newv: Option<String> = None;
                match &pd.kind {
                    PropKind::Num(unit) => {
                        let mut s = cur.clone();
                        let bad = parse_si(&s).is_none();
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(90.0).text_color(if bad { ERR() } else { TEXT() }));
                            if r.gained_focus() {
                                self.checkpoint();
                            }
                            ui.label(RichText::new(*unit).color(TEXT3()));
                            if bad {
                                r.on_hover_text(t("写成 10k、4.7u、100n 这样的数值"));
                            }
                        });
                        if s != cur {
                            newv = Some(s);
                        }
                    }
                    PropKind::Choice(opts) => {
                        let mut s = cur.clone();
                        egui::ComboBox::from_id_salt(("cv-ch", id, *key))
                            .width(140.0)
                            .selected_text(choice_label(pd.label, &s))
                            .show_ui(ui, |ui| {
                                for o in opts.iter() {
                                    ui.selectable_value(&mut s, o.to_string(), choice_label(pd.label, o));
                                }
                            });
                        if s != cur {
                            self.checkpoint();
                            newv = Some(s);
                        }
                    }
                    PropKind::Slider(lo, hi, unit, log) => {
                        let mut v = parse_si(&cur).unwrap_or(*lo);
                        let r = ui.add(
                            egui::Slider::new(&mut v, *lo..=*hi)
                                .logarithmic(*log)
                                .suffix(format!(" {unit}"))
                                .max_decimals(if hi - lo > 50.0 { 0 } else { 2 }),
                        );
                        if r.drag_started() || r.gained_focus() {
                            self.checkpoint();
                        }
                        if r.changed() {
                            newv = Some(if v.abs() >= 100.0 || v == v.round() { format!("{v:.0}") } else { format!("{v:.2}") });
                        }
                    }
                    PropKind::Bool => {
                        let mut b = cur != "false";
                        if crate::app::switch(ui, &mut b).changed() {
                            self.checkpoint();
                            newv = Some(b.to_string());
                        }
                    }
                    PropKind::Text => {
                        let mut s = cur.clone();
                        let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(140.0));
                        if r.gained_focus() {
                            self.checkpoint();
                        }
                        if s != cur {
                            newv = Some(s);
                        }
                    }
                }
                if let Some(v) = newv {
                    self.c.parts[pi].set(key, v);
                    self.changed();
                }
                ui.end_row();
            }
        });
        // 电阻：色环
        if def.key == "res" {
            if let Some(bands) = crate::circuit::color_bands(self.c.parts[pi].num("r")) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("色环")).size(12.0).color(TEXT2()));
                    for b in bands {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 18.0), Sense::hover());
                        ui.painter().rect(r, CornerRadius::same(2), draw::band_color(b), Stroke::new(1.0, LINE_HI()), egui::StrokeKind::Inside);
                    }
                });
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(format!("{} (R)", t("旋转"))).clicked() {
                self.rotate_sel(false);
            }
            if ui.button(format!("{} (X)", t("镜像"))).clicked() {
                self.rotate_sel(true);
            }
            if ui.button(t("删除")).clicked() {
                self.delete_sel();
            }
        });
        // 开发板：引脚和规则
        if def.is_board() {
            ui.add_space(12.0);
            let mut cfg: crate::circuit::mcu::McuCfg = serde_json::from_str(self.c.parts[pi].prop("mcu")).unwrap_or_default();
            let (changed, gen) = super::mcuedit::editor(ui, def, &mut cfg);
            if changed {
                self.checkpoint_soft();
                self.c.parts[pi].set("mcu", serde_json::to_string(&cfg).unwrap_or_default());
                // 规则改了：单片机重新上电
                if let Some(s) = &mut self.sim {
                    s.rebuild = true;
                }
            }
            if gen {
                self.code_board = Some(id);
                self.btab = super::BTab::Code;
                self.bottom = true;
            }
        }
        // 通电时：读数
        let Some(s) = &self.sim else { return };
        let Some(st) = s.index_of(id).and_then(|i| s.ps.get(i)) else { return };
        ui.add_space(12.0);
        ui.label(RichText::new(t("实时读数")).size(12.5).strong().color(TEXT()));
        ui.add_space(4.0);
        if let Some(why) = &st.burnt {
            egui::Frame::new()
                .fill(ERR().gamma_multiply(0.12))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("🔥 {}", t("烧坏了"))).strong().color(ERR()));
                    ui.label(RichText::new(why).size(12.0).color(TEXT()));
                });
            ui.add_space(4.0);
            if ui.button(t("换个新的（修好）")).clicked() {
                if let Some(s) = &mut self.sim {
                    s.repair(id);
                }
            }
            return;
        }
        let pins: Vec<(String, Option<f64>, f64)> = def
            .pins
            .iter()
            .enumerate()
            .take(24)
            .map(|(i, pd)| (pd.name.clone(), s.pin_v(id, i), st.i.get(i).copied().unwrap_or(0.0)))
            .collect();
        egui::Grid::new(("cv-read", id)).num_columns(3).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            for (n, v, i) in pins {
                ui.label(RichText::new(n).font(FontId::monospace(12.0)).color(TEXT2()));
                ui.label(RichText::new(v.map_or("—".into(), |v| fmt_si(v, "V"))).font(FontId::monospace(12.0)).color(TEXT()));
                ui.label(RichText::new(if i.abs() > 1e-9 { fmt_si(i, "A") } else { String::new() }).font(FontId::monospace(12.0)).color(TEXT3()));
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        let mut extra = vec![format!("{} {}", t("功率"), fmt_si(st.p.abs(), "W"))];
        if st.temp > 30.0 {
            extra.push(format!("{} {:.0} °C", t("温度"), st.temp));
        }
        if !st.vis.status.is_empty() {
            extra.push(st.vis.status.clone());
        }
        for e in extra {
            ui.label(RichText::new(e).size(12.0).color(TEXT2()));
        }
    }

    fn know_ui(&mut self, ui: &mut egui::Ui) {
        let def = match self.sel.as_slice() {
            [Sel::Part(id)] => self.c.part(*id).and_then(|p| lib::def(&p.kind)),
            _ => self.placing.and_then(|p| lib::def(p.0)),
        };
        let Some(def) = def else {
            ui.label(RichText::new(t("选中一个元件，这里会讲它是什么、怎么接、常见的坑。")).color(TEXT3()));
            return;
        };
        ui.label(RichText::new(t(def.zh)).size(15.0).strong().color(TEXT()));
        ui.label(RichText::new(t(def.cat.zh())).size(11.5).color(TEXT3()));
        ui.add_space(8.0);
        let card = crate::circuit::content::card(def.key);
        if !card.is_empty() {
            for para in card.split("\\n") {
                ui.label(RichText::new(para).size(12.5).color(TEXT()));
                ui.add_space(4.0);
            }
            ui.add_space(6.0);
        }
        if def.cat != Cat::Board {
            ui.label(RichText::new(t("引脚")).size(12.5).strong().color(TEXT()));
            ui.add_space(2.0);
            egui::Grid::new(("cv-pins", def.key)).num_columns(2).spacing(vec2(12.0, 3.0)).show(ui, |ui| {
                for pd in def.pins.iter().take(64) {
                    ui.label(RichText::new(&pd.name).font(FontId::monospace(12.0)).color(ACCENT()));
                    ui.label(RichText::new(caps_text(pd.caps)).size(11.5).color(TEXT3()));
                    ui.end_row();
                }
            });
        }
        if !def.kw.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(def.kw).size(11.0).color(TEXT3()));
        }
        let _ = Color32::TRANSPARENT;
    }
}

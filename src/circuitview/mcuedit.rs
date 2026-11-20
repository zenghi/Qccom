//! 开发板的「引脚设置 + 如果…就…规则」编辑器。不写代码也能让板子动起来，需要时一键生成真代码。

use std::mem::discriminant;

use eframe::egui::{self, vec2, RichText};

use crate::app::{icon_button, Icon};
use crate::circuit::lib::{cap, PartDef};
use crate::circuit::mcu::{Act, Cond, McuCfg, PinCfg, PinMode, Rule};
use crate::i18n::t;
use crate::theme::*;

fn modes() -> Vec<(&'static str, PinMode)> {
    vec![
        (t("输出高电平"), PinMode::OutHigh),
        (t("输出低电平"), PinMode::OutLow),
        (t("闪烁"), PinMode::Blink { ms: 1000 }),
        ("PWM", PinMode::Pwm { duty: 0.5, freq: 1000 }),
        (t("呼吸灯"), PinMode::Breath { ms: 2000 }),
        (t("流水灯"), PinMode::Seq { group: 0, index: 0, ms: 200 }),
        (t("方波（蜂鸣器）"), PinMode::Tone { freq: 2000 }),
        (t("舵机"), PinMode::Servo { angle: 90.0 }),
        (t("上拉输入（接按键）"), PinMode::InPullUp),
        (t("下拉输入"), PinMode::InPullDown),
        (t("浮空输入"), PinMode::InFloat),
        (t("模拟输入（ADC）"), PinMode::Adc),
        (t("串口 TX"), PinMode::UartTx),
        (t("串口 RX"), PinMode::UartRx),
        ("I2C SDA", PinMode::I2cSda),
        ("I2C SCL", PinMode::I2cScl),
        ("SPI SCK", PinMode::SpiSck),
        ("SPI MOSI", PinMode::SpiMosi),
        ("SPI MISO", PinMode::SpiMiso),
        ("SPI CS", PinMode::SpiCs),
    ]
}

fn conds(pin: &str) -> Vec<(&'static str, Cond)> {
    let p = pin.to_string();
    vec![
        (t("上电时"), Cond::Start),
        (t("每隔一段时间"), Cond::Every { ms: 1000 }),
        (t("引脚变低（按下按键）"), Cond::Falling { pin: p.clone() }),
        (t("引脚变高"), Cond::Rising { pin: p.clone() }),
        (t("引脚一直是低"), Cond::WhileLow { pin: p.clone() }),
        (t("引脚一直是高"), Cond::WhileHigh { pin: p.clone() }),
        (t("电压高于"), Cond::Above { pin: p.clone(), v: 1.65 }),
        (t("电压低于"), Cond::Below { pin: p, v: 1.65 }),
        (t("串口收到"), Cond::Recv { text: "on".into() }),
        (t("温度高于"), Cond::TempAbove { c: 30.0 }),
        (t("温度低于"), Cond::TempBelow { c: 10.0 }),
        (t("距离小于"), Cond::DistBelow { cm: 20.0 }),
    ]
}

fn acts(pin: &str) -> Vec<(&'static str, Act)> {
    let p = pin.to_string();
    vec![
        (t("设置引脚"), Act::Set { pin: p.clone(), high: true }),
        (t("翻转引脚"), Act::Toggle { pin: p.clone() }),
        (t("设置 PWM"), Act::PwmSet { pin: p.clone(), duty: 0.5 }),
        (t("PWM 跟着电位器"), Act::PwmFromAdc { pin: p.clone(), adc: p.clone() }),
        (t("舵机转到"), Act::ServoSet { pin: p.clone(), angle: 90.0 }),
        (t("舵机跟着电位器"), Act::ServoFromAdc { pin: p.clone(), adc: p.clone() }),
        (t("串口打印"), Act::Print { text: "hello {count}".into() }),
        (t("蜂鸣器响"), Act::Tone { pin: p.clone(), freq: 2000, ms: 200 }),
        (t("计数 +1"), Act::Count),
        (t("屏幕显示"), Act::Show { text: "count {count}".into() }),
        (t("读温湿度（DHT11）"), Act::ReadDht { pin: p.clone() }),
        (t("读温湿度（SHT30）"), Act::ReadSht),
        (t("测距（HC-SR04）"), Act::Measure { trig: p.clone(), echo: p.clone() }),
        (
            t("74HC595 输出"),
            Act::Shift {
                data: p.clone(),
                clk: p.clone(),
                latch: p.clone(),
                value: "{count}".into(),
            },
        ),
        (
            t("数码管显示"),
            Act::Seg {
                pins: vec![],
                value: "{count}".into(),
                common_anode: false,
            },
        ),
        (
            t("TM1637 显示"),
            Act::Tm1637 {
                clk: p.clone(),
                dio: p.clone(),
                text: "{count}".into(),
            },
        ),
        (t("拍照"), Act::Photo),
        (t("显示照片"), Act::ShowImage),
        (
            t("发 MQTT"),
            Act::Mqtt {
                host: "127.0.0.1".into(),
                port: 1883,
                topic: "qccom/test".into(),
                payload: "{temp}".into(),
            },
        ),
        (
            t("HTTP 上报"),
            Act::Http {
                url: "http://127.0.0.1:8000/".into(),
                body: "{temp}".into(),
            },
        ),
        (
            t("TCP 发送"),
            Act::Tcp {
                host: "127.0.0.1".into(),
                port: 8888,
                text: "{count}".into(),
            },
        ),
        (t("等待"), Act::Sleep { ms: 500 }),
    ]
}

/// 可选的引脚：能当 IO / ADC 的
fn io_pins(def: &PartDef) -> Vec<String> {
    def.pins.iter().filter(|p| p.caps & (cap::IO | cap::ADC) != 0).map(|p| p.name.clone()).collect()
}

fn pin_pick(ui: &mut egui::Ui, id: impl std::hash::Hash, v: &mut String, pins: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .width(64.0)
        .selected_text(if v.is_empty() { "?" } else { v.as_str() })
        .show_ui(ui, |ui| {
            for p in pins {
                ui.selectable_value(v, p.clone(), p);
            }
        });
}

fn num<T: egui::emath::Numeric>(ui: &mut egui::Ui, v: &mut T, lo: f64, hi: f64, suffix: &str) {
    ui.add(egui::DragValue::new(v).range(lo..=hi).suffix(suffix).speed(((hi - lo) / 300.0).max(0.01)));
}

fn text(ui: &mut egui::Ui, v: &mut String, w: f32) {
    ui.add(egui::TextEdit::singleline(v).desired_width(w));
}

fn mode_params(ui: &mut egui::Ui, k: usize, m: &mut PinMode) {
    match m {
        PinMode::Blink { ms } | PinMode::Breath { ms } => num(ui, ms, 20.0, 10000.0, " ms"),
        PinMode::Pwm { duty, freq } => {
            let mut pct = *duty * 100.0;
            num(ui, &mut pct, 0.0, 100.0, "%");
            *duty = pct / 100.0;
            num(ui, freq, 1.0, 100000.0, " Hz");
        }
        PinMode::Seq { group, index, ms } => {
            ui.label(RichText::new(t("组")).size(11.0).color(TEXT3()));
            num(ui, group, 0.0, 9.0, "");
            ui.label(RichText::new(t("第")).size(11.0).color(TEXT3()));
            num(ui, index, 0.0, 31.0, "");
            num(ui, ms, 20.0, 5000.0, " ms");
        }
        PinMode::Tone { freq } => num(ui, freq, 20.0, 20000.0, " Hz"),
        PinMode::Servo { angle } => num(ui, angle, 0.0, 180.0, "°"),
        _ => {}
    }
    let _ = k;
}

fn cond_params(ui: &mut egui::Ui, k: usize, c: &mut Cond, pins: &[String]) {
    match c {
        Cond::Every { ms } => num(ui, ms, 1.0, 3_600_000.0, " ms"),
        Cond::Falling { pin } | Cond::Rising { pin } | Cond::WhileLow { pin } | Cond::WhileHigh { pin } => pin_pick(ui, ("cp", k), pin, pins),
        Cond::Above { pin, v } | Cond::Below { pin, v } => {
            pin_pick(ui, ("cp", k), pin, pins);
            num(ui, v, 0.0, 5.0, " V");
        }
        Cond::Recv { text: s } => text(ui, s, 90.0),
        Cond::TempAbove { c } | Cond::TempBelow { c } => num(ui, c, -40.0, 120.0, " °C"),
        Cond::DistBelow { cm } => num(ui, cm, 2.0, 400.0, " cm"),
        Cond::Start => {}
    }
}

fn act_params(ui: &mut egui::Ui, id: (usize, usize), a: &mut Act, pins: &[String]) {
    let pp = |ui: &mut egui::Ui, n: u8, v: &mut String| pin_pick(ui, (id, n), v, pins);
    match a {
        Act::Set { pin, high } => {
            pp(ui, 0, pin);
            egui::ComboBox::from_id_salt((id, 9))
                .width(44.0)
                .selected_text(if *high { t("高") } else { t("低") })
                .show_ui(ui, |ui| {
                    ui.selectable_value(high, true, t("高"));
                    ui.selectable_value(high, false, t("低"));
                });
        }
        Act::Toggle { pin } | Act::ReadDht { pin } => pp(ui, 0, pin),
        Act::PwmSet { pin, duty } => {
            pp(ui, 0, pin);
            let mut pct = *duty * 100.0;
            num(ui, &mut pct, 0.0, 100.0, "%");
            *duty = pct / 100.0;
        }
        Act::PwmFromAdc { pin, adc } | Act::ServoFromAdc { pin, adc } => {
            pp(ui, 0, pin);
            ui.label(RichText::new("←").color(TEXT3()));
            pp(ui, 1, adc);
        }
        Act::ServoSet { pin, angle } => {
            pp(ui, 0, pin);
            num(ui, angle, 0.0, 180.0, "°");
        }
        Act::Print { text: s } | Act::Show { text: s } => text(ui, s, 130.0),
        Act::Tone { pin, freq, ms } => {
            pp(ui, 0, pin);
            num(ui, freq, 20.0, 20000.0, " Hz");
            num(ui, ms, 10.0, 10000.0, " ms");
        }
        Act::Shift { data, clk, latch, value } => {
            pp(ui, 0, data);
            pp(ui, 1, clk);
            pp(ui, 2, latch);
            text(ui, value, 60.0);
        }
        Act::Seg { pins: sp, value, common_anode } => {
            let mut s = sp.join(",");
            ui.add(egui::TextEdit::singleline(&mut s).hint_text("a,b,c,d,e,f,g").desired_width(110.0));
            *sp = s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            text(ui, value, 60.0);
            ui.checkbox(common_anode, t("共阳"));
        }
        Act::Tm1637 { clk, dio, text: s } => {
            pp(ui, 0, clk);
            pp(ui, 1, dio);
            text(ui, s, 60.0);
        }
        Act::Measure { trig, echo } => {
            pp(ui, 0, trig);
            pp(ui, 1, echo);
        }
        Act::Mqtt { host, port, topic, payload } => {
            text(ui, host, 90.0);
            num(ui, port, 1.0, 65535.0, "");
            text(ui, topic, 90.0);
            text(ui, payload, 90.0);
        }
        Act::Http { url, body } => {
            text(ui, url, 150.0);
            text(ui, body, 80.0);
        }
        Act::Tcp { host, port, text: s } => {
            text(ui, host, 90.0);
            num(ui, port, 1.0, 65535.0, "");
            text(ui, s, 80.0);
        }
        Act::Sleep { ms } => num(ui, ms, 1.0, 60000.0, " ms"),
        Act::Count | Act::ReadSht | Act::Photo | Act::ShowImage => {}
    }
}

/// 返回 (配置是否改了, 是否点了「生成代码」)
pub fn editor(ui: &mut egui::Ui, def: &PartDef, cfg: &mut McuCfg) -> (bool, bool) {
    let before = cfg.clone();
    let pins = io_pins(def);
    let first = pins.first().cloned().unwrap_or_default();
    let mut gen = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("引脚")).size(12.5).strong().color(TEXT()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t("生成代码")).on_hover_text(t("Arduino / STM32 HAL / 51 / ESP-IDF")).clicked() {
                gen = true;
            }
        });
    });
    let mut del = None;
    for (k, pc) in cfg.pins.iter_mut().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            pin_pick(ui, ("mp", k), &mut pc.pin, &pins);
            let all = modes();
            let cur = all.iter().find(|m| discriminant(&m.1) == discriminant(&pc.mode)).map_or("?", |m| m.0);
            egui::ComboBox::from_id_salt(("mm", k)).width(110.0).selected_text(cur).show_ui(ui, |ui| {
                for (name, m) in all {
                    if ui.selectable_label(discriminant(&m) == discriminant(&pc.mode), name).clicked() {
                        pc.mode = m;
                    }
                }
            });
            mode_params(ui, k, &mut pc.mode);
            if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                del = Some(k);
            }
        });
    }
    if let Some(k) = del {
        cfg.pins.remove(k);
    }
    if ui.small_button(format!("+ {}", t("添加引脚"))).clicked() {
        let used: Vec<String> = cfg.pins.iter().map(|p| p.pin.clone()).collect();
        let pin = pins.iter().find(|p| !used.contains(p)).cloned().unwrap_or_else(|| first.clone());
        cfg.pins.push(PinCfg { pin, mode: PinMode::OutLow });
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("规则")).size(12.5).strong().color(TEXT()));
        ui.label(RichText::new(t("如果…就…")).size(11.5).color(TEXT3()));
    });
    let mut del_rule = None;
    for (k, r) in cfg.rules.iter_mut().enumerate() {
        egui::Frame::new().fill(BG2()).corner_radius(6).inner_margin(8.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ui.label(RichText::new(t("当")).color(ACCENT()));
                let all = conds(&first);
                let cur = all.iter().find(|c| discriminant(&c.1) == discriminant(&r.when)).map_or("?", |c| c.0);
                egui::ComboBox::from_id_salt(("rc", k)).width(120.0).selected_text(cur).show_ui(ui, |ui| {
                    for (name, c) in all {
                        if ui.selectable_label(discriminant(&c) == discriminant(&r.when), name).clicked() {
                            r.when = c;
                        }
                    }
                });
                cond_params(ui, k, &mut r.when, &pins);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if icon_button(ui, Icon::Close, t("删除这条规则"), false).clicked() {
                        del_rule = Some(k);
                    }
                });
            });
            let mut del_act = None;
            for (j, a) in r.then.iter_mut().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    ui.label(RichText::new(if j == 0 { t("就") } else { t("再") }).color(TEXT3()));
                    let all = acts(&first);
                    let cur = all.iter().find(|c| discriminant(&c.1) == discriminant(a)).map_or("?", |c| c.0);
                    egui::ComboBox::from_id_salt(("ra", k, j)).width(110.0).selected_text(cur).show_ui(ui, |ui| {
                        for (name, c) in all {
                            if ui.selectable_label(discriminant(&c) == discriminant(a), name).clicked() {
                                *a = c;
                            }
                        }
                    });
                    act_params(ui, (k, j), a, &pins);
                    if icon_button(ui, Icon::Close, t("删除"), false).clicked() {
                        del_act = Some(j);
                    }
                });
            }
            if let Some(j) = del_act {
                r.then.remove(j);
            }
            if ui.small_button(format!("+ {}", t("动作"))).clicked() {
                r.then.push(Act::Toggle { pin: first.clone() });
            }
        });
        ui.add_space(4.0);
    }
    if let Some(k) = del_rule {
        cfg.rules.remove(k);
    }
    if ui.small_button(format!("+ {}", t("添加规则"))).clicked() {
        cfg.rules.push(Rule {
            when: Cond::Every { ms: 1000 },
            then: vec![Act::Toggle { pin: first.clone() }],
        });
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t("串口波特率")).size(12.0).color(TEXT2()));
        if cfg.baud == 0 {
            cfg.baud = 115200;
        }
        egui::ComboBox::from_id_salt("mcu-baud").width(80.0).selected_text(cfg.baud.to_string()).show_ui(ui, |ui| {
            for b in [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600] {
                ui.selectable_value(&mut cfg.baud, b, b.to_string());
            }
        });
    });
    ui.label(RichText::new(t("文字里可以用 {count} {temp} {hum} {dist} {t} {adc:A0} {v:A0}")).size(11.0).color(TEXT3()));
    (*cfg != before, gen)
}

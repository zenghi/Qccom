//! 接错检查：不用通电，按连接关系找出新手最常见的错误。仿真本身也会「烧」，这里是事前提醒。

use std::collections::HashMap;

use super::lib::{self, cap, Cat, Look, PartDef};
use super::mcu::{McuCfg, PinMode};
use super::net::Netlist;
use super::{models, Circuit, Part};
use crate::i18n::{t, tf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Err,
    Warn,
    Tip,
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub level: Level,
    pub parts: Vec<u32>,
    pub msg: String,
}

/// 网络上的「已知电压」：电源正极、开发板 5V / 3V3、稳压芯片输出；地是 0
fn rail_volts(c: &Circuit, nl: &Netlist) -> HashMap<usize, f64> {
    let mut v: HashMap<usize, f64> = HashMap::new();
    for p in &c.parts {
        let Some(d) = lib::def(&p.kind) else { continue };
        let mut set = |pin: usize, volt: f64| {
            if let Some(n) = nl.net_of(p.id, pin) {
                v.entry(n).or_insert(volt);
            }
        };
        match d.key {
            "usb5v" | "psu" => {
                set(0, p.num("v"));
                set(1, 0.0);
            }
            "battery" => {
                set(0, models::battery(p.prop("type")).vnom);
                set(1, 0.0);
            }
            "gnd" => set(0, 0.0),
            "ldo" => {
                if let Some(k) = d.pin("OUT") {
                    set(k, models::ldo(p.prop("model")).vout);
                }
            }
            _ => {
                if d.is_board() || d.cat == Cat::Debug {
                    for (k, pd) in d.pins.iter().enumerate() {
                        if pd.caps & cap::GND != 0 {
                            set(k, 0.0);
                        } else if pd.caps & cap::V5 != 0 {
                            set(k, 5.0);
                        } else if pd.caps & cap::V33 != 0 {
                            set(k, 3.3);
                        }
                    }
                }
            }
        }
    }
    v
}

fn is_bb(d: &PartDef) -> bool {
    matches!(d.look, Look::Breadboard { .. })
}

/// 两个点之间只靠面包板连通（没有导线、没有别的元件）：说明两只脚插进了同一列
fn same_strip(c: &Circuit, p: &Part, a: usize, b: usize) -> bool {
    let pins = p.pins();
    let (pa, pb) = (pins[a], pins[b]);
    c.parts.iter().filter(|q| lib::def(&q.kind).is_some_and(is_bb)).any(|bb| {
        let d = lib::def(&bb.kind).unwrap();
        let holes: Vec<_> = d.pins.iter().map(|h| (bb.to_world(h.at), lib::bb_group(h.at))).collect();
        let ga = holes.iter().find(|h| h.0 == pa).map(|h| h.1);
        let gb = holes.iter().find(|h| h.0 == pb).map(|h| h.1);
        ga.is_some() && ga == gb
    })
}

fn out_mode(m: &PinMode) -> bool {
    matches!(
        m,
        PinMode::OutHigh
            | PinMode::OutLow
            | PinMode::Blink { .. }
            | PinMode::Pwm { .. }
            | PinMode::Breath { .. }
            | PinMode::Seq { .. }
            | PinMode::Tone { .. }
            | PinMode::Servo { .. }
            | PinMode::UartTx
    )
}

pub fn check(c: &Circuit) -> Vec<Issue> {
    let nl = Netlist::build(c);
    let rails = rail_volts(c, &nl);
    let mut out: Vec<Issue> = vec![];
    let mut push = |level, parts: Vec<u32>, msg: String| out.push(Issue { level, parts, msg });
    let label = |id: u32| c.label(id);
    // 网络里有哪些（元件, 引脚）
    let members = |net: usize| nl.members.get(net).cloned().unwrap_or_default();
    let pin_caps = |id: u32, k: usize| c.part(id).and_then(|p| lib::def(&p.kind)).and_then(|d| d.pins.get(k)).map_or(0, |p| p.caps);
    let gnd_of = |id: u32| -> Option<usize> {
        let p = c.part(id)?;
        let d = lib::def(&p.kind)?;
        let k = d.pins.iter().position(|x| x.caps & cap::GND != 0 || x.name == "GND" || x.name == "-")?;
        nl.net_of(id, k)
    };

    for p in &c.parts {
        let Some(d) = lib::def(&p.kind) else { continue };
        if is_bb(d) {
            continue;
        }
        let net = |k: usize| nl.net_of(p.id, k);
        // 电源正负极短路
        if matches!(d.key, "usb5v" | "psu" | "battery") && net(0).is_some() && net(0) == net(1) {
            push(Level::Err, vec![p.id], tf("{} 的正负极直接连在一起：短路，电源会发烫、电池会鼓包", &[&label(p.id)]));
            continue;
        }
        // 两只脚插进了面包板同一列（被短接）
        if d.pins.len() <= 3 && !matches!(d.cat, Cat::Meter | Cat::Mcu | Cat::Module) {
            for a in 0..d.pins.len() {
                for b in a + 1..d.pins.len() {
                    if d.pins[a].name != d.pins[b].name && net(a).is_some() && net(a) == net(b) && same_strip(c, p, a, b) {
                        push(
                            Level::Warn,
                            vec![p.id],
                            tf(
                                "{} 的 {} 脚和 {} 脚插在面包板同一列，被短接了；把它转个方向或跨到另一列",
                                &[&label(p.id), &d.pins[a].name, &d.pins[b].name],
                            ),
                        );
                    }
                }
            }
        }
        // 两脚元件有脚没接
        if matches!(d.cat, Cat::Passive | Cat::Diode | Cat::Bjt | Cat::Mos | Cat::Output | Cat::Input) && d.pins.len() <= 4 {
            let loose: Vec<&str> = (0..d.pins.len()).filter(|k| net(*k).is_none_or(|n| members(n).len() <= 1)).map(|k| d.pins[k].name.as_str()).collect();
            if !loose.is_empty() && loose.len() < d.pins.len() && d.key != "button" {
                push(Level::Tip, vec![p.id], tf("{} 的 {} 脚还没接", &[&label(p.id), &loose.join("、")]));
            }
        }
        // LED：没串电阻直接接在电源两端、接反
        if matches!(d.key, "led" | "diode") || d.key == "led" {
            let (a, k) = (net(0), net(1));
            let (va, vk) = (a.and_then(|n| rails.get(&n)), k.and_then(|n| rails.get(&n)));
            if let (Some(va), Some(vk)) = (va, vk) {
                if d.key == "led" && va - vk > 1.6 {
                    push(
                        Level::Err,
                        vec![p.id],
                        tf("{} 直接接在 {}V 电源上，没有串限流电阻，通电就会烧", &[&label(p.id), &format!("{:.1}", va - vk)]),
                    );
                } else if vk - va > 0.5 {
                    let what = if d.key == "led" {
                        t("LED 接反了：长脚（A）要接正，短脚（K）接负，现在不会亮")
                    } else {
                        t("二极管反接在电源上，不导通")
                    };
                    push(Level::Warn, vec![p.id], format!("{}：{what}", label(p.id)));
                }
            }
        }
        // 电解电容接反
        if d.key == "ecap" {
            let (pos, neg) = (net(0).and_then(|n| rails.get(&n)), net(1).and_then(|n| rails.get(&n)));
            if let (Some(a), Some(b)) = (pos, neg) {
                if *b > a + 0.5 {
                    push(Level::Err, vec![p.id], tf("{} 电解电容接反了：+ 极要接高电压，反接会鼓包甚至爆开", &[&label(p.id)]));
                }
            }
        }
        // 模块供电电压不对
        if matches!(d.cat, Cat::Module | Cat::Display | Cat::Camera | Cat::Iot | Cat::Chip | Cat::Output) {
            let (vcc, gnd) = super::sim::module_pwr_pins(d);
            if let (Some(vp), Some(gp)) = (vcc, gnd) {
                let (lo, hi, _) = models::module_power(d.key);
                let v = net(vp).and_then(|n| rails.get(&n)).copied();
                let g = net(gp).and_then(|n| rails.get(&n)).copied();
                match (v, g) {
                    (Some(v), Some(g)) if v - g > hi + 0.3 => push(Level::Err, vec![p.id], tf("{} 最高只能接 {}V，现在接的是 {}V，会烧坏", &[&label(p.id), &hi, &format!("{:.1}", v - g)])),
                    (Some(v), Some(g)) if v - g < lo - 0.2 && v - g > 0.5 => {
                        push(Level::Warn, vec![p.id], tf("{} 至少要 {}V 才能正常工作，现在只有 {}V", &[&label(p.id), &lo, &format!("{:.1}", v - g)]))
                    }
                    (Some(_), None) if net(gp).is_none_or(|n| members(n).len() <= 1) => push(Level::Warn, vec![p.id], tf("{} 的 GND 没接，模块不工作", &[&label(p.id)])),
                    _ => {}
                }
            }
        }
        // 单片机引脚
        if d.is_board() {
            let cfg: McuCfg = serde_json::from_str(p.prop("mcu")).unwrap_or_default();
            let logic = d.logic_v();
            for pc in &cfg.pins {
                let Some(k) = d.pin(&pc.pin) else { continue };
                let Some(n) = net(k) else { continue };
                if out_mode(&pc.mode) {
                    if let Some(v) = rails.get(&n) {
                        push(
                            Level::Err,
                            vec![p.id],
                            tf("{} 的 {} 设成输出，却直接接到了 {}V（电源 / 地）：输出时会短路烧引脚", &[&label(p.id), &pc.pin, &format!("{v:.1}")]),
                        );
                    }
                }
                if matches!(pc.mode, PinMode::InFloat) && members(n).iter().any(|(id, _)| c.part(*id).is_some_and(|q| q.kind == "button")) {
                    push(Level::Warn, vec![p.id], tf("{} 的 {} 接了按键但设成浮空输入，没按时读数乱跳；改成上拉输入", &[&label(p.id), &pc.pin]));
                }
            }
            // 5V 信号进 3.3V 板子（不耐 5V 的脚）
            if logic < 4.0 {
                for (k, pd) in d.pins.iter().enumerate() {
                    if pd.caps & cap::IO == 0 || pd.caps & cap::FT != 0 {
                        continue;
                    }
                    let Some(n) = net(k) else { continue };
                    if rails.get(&n).is_some_and(|v| *v > 4.0) {
                        push(Level::Err, vec![p.id], tf("{} 的 {} 是 3.3V 引脚（不耐 5V），直接接了 5V 会损坏", &[&label(p.id), &pd.name]));
                        continue;
                    }
                    for (oid, ok) in members(n) {
                        if oid == p.id {
                            continue;
                        }
                        let Some(q) = c.part(oid) else { continue };
                        let Some(qd) = lib::def(&q.kind) else { continue };
                        if pin_caps(oid, ok) & (cap::SOUT | cap::TX) == 0 {
                            continue;
                        }
                        let (vcc, _) = super::sim::module_pwr_pins(qd);
                        if vcc.and_then(|vp| nl.net_of(oid, vp)).and_then(|vn| rails.get(&vn)).is_some_and(|v| *v > 4.0) {
                            push(
                                Level::Warn,
                                vec![p.id, oid],
                                tf(
                                    "{} 用 5V 供电，它的 {} 输出 5V 信号接到 {} 的 {}（3.3V 不耐 5V）：加分压电阻或电平转换",
                                    &[&label(oid), &qd.pins[ok].name, &label(p.id), &pd.name],
                                ),
                            );
                        }
                    }
                }
            }
        }
        // 串口 TX 接 TX
        for (k, pd) in d.pins.iter().enumerate() {
            if pd.caps & cap::TX == 0 || pd.caps & cap::PWR != 0 {
                continue;
            }
            let Some(n) = net(k) else { continue };
            for (oid, ok) in members(n) {
                if oid > p.id && pin_caps(oid, ok) & cap::TX != 0 && pin_caps(oid, ok) & cap::RX == 0 && pd.caps & cap::RX == 0 {
                    push(
                        Level::Warn,
                        vec![p.id, oid],
                        tf("{} 的 {} 接到了 {} 的 TX：串口要交叉接，TX 接对方 RX", &[&label(p.id), &pd.name, &label(oid)]),
                    );
                }
            }
        }
    }
    // 没共地：两个有电源的东西之间有信号线，GND 却不通
    let powered: Vec<u32> = c.parts.iter().filter(|p| lib::def(&p.kind).is_some_and(|d| d.is_board() || d.key == "usbttl")).map(|p| p.id).collect();
    for p in &c.parts {
        let Some(d) = lib::def(&p.kind) else { continue };
        if !(d.is_board() || matches!(d.cat, Cat::Module | Cat::Iot | Cat::Display) || d.key == "usbttl") {
            continue;
        }
        let Some(g) = gnd_of(p.id) else { continue };
        for (k, pd) in d.pins.iter().enumerate() {
            if pd.caps & (cap::PWR | cap::GND) != 0 || pd.caps == 0 {
                continue;
            }
            let Some(n) = nl.net_of(p.id, k) else { continue };
            for (oid, _) in members(n) {
                if oid <= p.id || !powered.contains(&oid) && !powered.contains(&p.id) {
                    continue;
                }
                if let Some(og) = gnd_of(oid) {
                    if og != g && !out.iter().any(|i| i.parts.contains(&p.id) && i.parts.contains(&oid) && i.level == Level::Warn) {
                        out.push(Issue {
                            level: Level::Warn,
                            parts: vec![p.id, oid],
                            msg: tf("{} 和 {} 之间有信号线，但 GND 没连在一起（没共地），信号不可靠", &[&label(p.id), &label(oid)]),
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|i| i.level);
    out.dedup_by(|a, b| a.msg == b.msg);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(c: &mut Circuit, a: (i32, i32), b: (i32, i32)) {
        c.add_wire(vec![a, b], 0);
    }

    #[test]
    fn finds_common_mistakes() {
        // LED 直接接 5V
        let mut c = Circuit::default();
        c.add_part("usb5v", 0, 0); // + (0,0) - (0,2)
        c.add_part("led", 5, 0); // A (5,0) K (6,0)
        wire(&mut c, (0, 0), (5, 0));
        wire(&mut c, (0, 2), (6, 0));
        let v = check(&c);
        assert!(v.iter().any(|i| i.level == Level::Err && i.msg.contains("限流电阻")), "{v:?}");
        // 电源短路
        let mut c = Circuit::default();
        c.add_part("usb5v", 0, 0);
        wire(&mut c, (0, 0), (0, 2));
        assert!(check(&c).iter().any(|i| i.msg.contains("短路")));
        // 电阻两只脚插在面包板同一列
        let mut c = Circuit::default();
        c.add_part("bb_half", 0, 0);
        let r = c.add_part("res", 3, 3);
        c.part_mut(r).unwrap().set("span", "0,2");
        assert!(check(&c).iter().any(|i| i.msg.contains("同一列")), "{:?}", check(&c));
        // 起始电路没有问题
        let mut c = Circuit::default();
        c.add_part("bb_half", 0, 0);
        c.add_part("usb5v", -3, -2);
        wire(&mut c, (-3, -2), (3, 1));
        wire(&mut c, (-3, 0), (2, 0));
        let r = c.add_part("res", 5, 1);
        c.part_mut(r).unwrap().set("span", "0,4");
        let l = c.add_part("led", 5, 7);
        c.part_mut(l).unwrap().rot = 2;
        wire(&mut c, (4, 3), (4, 0));
        let v = check(&c);
        assert!(v.iter().all(|i| i.level == Level::Tip), "{v:?}");
    }
}

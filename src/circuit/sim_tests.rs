//! 整体仿真测试：搭几个典型电路，跑一段时间，看结果是不是符合常识。

use super::mcu::{Act, Cond, McuCfg, PinCfg, PinMode, Rule};
use super::sim::Sim;
use super::{lib, Circuit};

/// 按 1 倍速跑 secs 秒（每帧 1/60 秒）
fn run(s: &mut Sim, c: &Circuit, secs: f64) {
    let frames = (secs * 60.0).ceil() as usize;
    for _ in 0..frames {
        // 测试里不受 CPU 预算影响：一帧没走完就接着走
        let t_end = s.t + 1.0 / 60.0;
        while s.t < t_end - 1e-9 {
            s.advance(c, (t_end - s.t).min(1.0 / 60.0), 1.0);
        }
    }
}

fn led_circuit(r: &str) -> (Circuit, u32, u32) {
    let mut c = Circuit::default();
    let bat = c.add_part("usb5v", 0, 0);
    let res = c.add_part("res", 2, 0);
    c.part_mut(res).unwrap().set("r", r);
    let led = c.add_part("led", 6, 0);
    c.add_wire(vec![(0, 0), (2, 0)], 0);
    c.add_wire(vec![(7, 0), (7, 2), (0, 2)], 1);
    let _ = bat;
    (c, res, led)
}

#[test]
fn led_lights_with_resistor() {
    let (c, _, led) = led_circuit("220");
    let mut s = Sim::new(&c);
    run(&mut s, &c, 0.1);
    let st = &s.ps[s.index_of(led).unwrap()];
    assert!(st.burnt.is_none());
    assert!(st.vis.glow > 0.5 && st.vis.glow < 0.8, "glow {}", st.vis.glow);
}

#[test]
fn led_burns_without_resistor() {
    let (c, _, led) = led_circuit("0.001");
    let mut s = Sim::new(&c);
    run(&mut s, &c, 0.3);
    let st = &s.ps[s.index_of(led).unwrap()];
    assert!(st.burnt.is_some(), "LED 应该烧了，电流 {:?}", st.i);
    assert!(!s.log.is_empty());
}

#[test]
fn rc_on_breadboard() {
    // 面包板：电池 + 到上轨，10k 从轨到 a10，100µF 从 b10 到 - 轨
    let mut c = Circuit::default();
    c.add_part("bb_half", 0, 0);
    let bat = c.add_part("battery", -6, 0);
    c.part_mut(bat).unwrap().set("type", "3aa");
    c.add_wire(vec![(-6, 0), (-3, 0), (-3, 1), (2, 1)], 0); // + → 上 + 轨
    c.add_wire(vec![(-6, 3), (-4, 3), (-4, 0), (2, 0)], 1); // - → 上 - 轨
    let r = c.add_part("res", 9, 1);
    c.part_mut(r).unwrap().set("r", "10k");
    c.part_mut(r).unwrap().set("span", "0,2"); // 上 + 轨 → c10（y=3 是 a 行，这里落在 a10）
    let cap = c.add_part("ecap", 9, 4);
    c.part_mut(cap).unwrap().set("c", "100u");
    c.add_wire(vec![(10, 4), (10, 0)], 1); // b11 → - 轨
    let mut s = Sim::new(&c);
    run(&mut s, &c, 1.0);
    let vc = s.pin_v(cap, 0).unwrap() - s.pin_v(cap, 1).unwrap();
    let vb = s.pin_v(bat, 0).unwrap() - s.pin_v(bat, 1).unwrap();
    assert!((vc / vb - 0.632).abs() < 0.06, "vc = {vc}, vb = {vb}");
}

#[test]
fn uno_blinks_led() {
    let mut c = Circuit::default();
    let u = c.add_part("uno", 0, 0);
    let cfg = McuCfg {
        pins: vec![PinCfg {
            pin: "D2".into(),
            mode: PinMode::Blink { ms: 100 },
        }],
        rules: vec![],
        baud: 9600,
    };
    c.part_mut(u).unwrap().set("mcu", serde_json::to_string(&cfg).unwrap());
    // D2 在 (4,0)；GND 在 (17,0)
    let r = c.add_part("res", 4, -6);
    c.part_mut(r).unwrap().set("r", "330");
    c.add_wire(vec![(4, 0), (4, -6)], 2);
    let led = c.add_part("led", 8, -6);
    c.add_wire(vec![(9, -6), (17, -6), (17, 0)], 1);
    let mut s = Sim::new(&c);
    let mut seen_on = false;
    let mut seen_off = false;
    for _ in 0..30 {
        run(&mut s, &c, 1.0 / 60.0);
        let g = s.ps[s.index_of(led).unwrap()].vis.glow;
        if g > 0.3 {
            seen_on = true;
        }
        if g < 0.05 {
            seen_off = true;
        }
    }
    assert!(seen_on && seen_off, "LED 应该闪烁");
}

/// 用一根经过远处独特拐点的导线把两点连起来（避免意外的 T 形接头）
fn link(c: &mut Circuit, a: (i32, i32), b: (i32, i32)) {
    let k = c.wires.len() as i32;
    c.add_wire(vec![a, (500 + 37 * k, 300 + 101 * k), b], 4);
}

#[test]
fn timer555_astable() {
    // 经典无稳态：R1 = 10k、R2 = 47k、C = 10µF → f ≈ 1.44/((R1+2R2)C) ≈ 1.38 Hz
    let mut c = Circuit::default();
    let psu = c.add_part("psu", 0, 0);
    c.part_mut(psu).unwrap().set("v", "9");
    let t = c.add_part("ne555", 10, 0);
    // 1 GND (10,3) 2 TRIG (11,3) 3 OUT (12,3) 4 RST (13,3) 5 CTRL (13,0) 6 THR (12,0) 7 DIS (11,0) 8 VCC (10,0)
    let r1 = c.add_part("res", 30, 0);
    c.part_mut(r1).unwrap().set("r", "10k");
    let r2 = c.add_part("res", 30, 5);
    c.part_mut(r2).unwrap().set("r", "47k");
    let cap = c.add_part("ecap", 40, 0);
    c.part_mut(cap).unwrap().set("c", "10u");
    link(&mut c, (0, 0), (10, 0));
    link(&mut c, (10, 0), (13, 3));
    link(&mut c, (10, 0), (30, 0));
    link(&mut c, (34, 0), (11, 0));
    link(&mut c, (11, 0), (30, 5));
    link(&mut c, (34, 5), (12, 0));
    link(&mut c, (12, 0), (11, 3));
    link(&mut c, (12, 0), (40, 0));
    link(&mut c, (41, 0), (0, 2));
    link(&mut c, (10, 3), (0, 2));
    let mut s = Sim::new(&c);
    let mut edges = 0;
    let mut last = false;
    for _ in 0..(60 * 5) {
        run(&mut s, &c, 1.0 / 60.0);
        let v = s.pin_v(t, 2).unwrap_or(0.0) - s.pin_v(t, 0).unwrap_or(0.0);
        if std::env::var("DBG555").is_ok() {
            eprintln!(
                "t {:.3} out {:.2} thr {:.2} trig {:.2} vcc {:.2} ctrl {:.2} dis {:.2} ok {} dt {:e}",
                s.t,
                v,
                s.pin_v(t, 5).unwrap_or(-1.0),
                s.pin_v(t, 1).unwrap_or(-1.0),
                s.pin_v(t, 7).unwrap_or(-1.0),
                s.pin_v(t, 4).unwrap_or(-1.0),
                s.pin_v(t, 6).unwrap_or(-1.0),
                s.ok,
                s.dt
            );
        }
        let hi = v > 4.5;
        if hi && !last {
            edges += 1;
        }
        last = hi;
    }
    // 5 秒约 7 个周期（第一个周期长一些）
    assert!((5..=9).contains(&edges), "上升沿 {edges} 个");
}

#[test]
fn relay_flyback_kills_transistor() {
    // 5V 继电器线圈 + S8050，方波驱动基极；没有续流二极管 → 关断瞬间高压击穿三极管
    let build = |diode: bool| {
        let mut c = Circuit::default();
        let psu = c.add_part("psu", 0, 0);
        c.part_mut(psu).unwrap().set("v", "5");
        let sg = c.add_part("siggen", 0, 10);
        c.part_mut(sg).unwrap().set("freq", "2");
        let k = c.add_part("relay", 10, 0); // L1 (10,0) L2 (10,4)
        let q = c.add_part("npn", 20, 10); // E (20,10) B (21,10) C (22,10)
        let rb = c.add_part("res", 30, 20);
        c.part_mut(rb).unwrap().set("r", "1k");
        link(&mut c, (0, 0), (10, 0));
        link(&mut c, (10, 4), (22, 10));
        link(&mut c, (0, 10), (30, 20));
        link(&mut c, (34, 20), (21, 10));
        link(&mut c, (20, 10), (0, 2));
        link(&mut c, (0, 12), (0, 2));
        if diode {
            c.add_part("diode", 40, 0);
            link(&mut c, (40, 0), (10, 4));
            link(&mut c, (44, 0), (10, 0));
        }
        (c, q, k)
    };
    let (c, q, k) = build(false);
    let mut s = Sim::new(&c);
    let mut vmax: f64 = 0.0;
    for _ in 0..120 {
        run(&mut s, &c, 1.0 / 60.0);
        let vc = s.pin_v(q, 2).unwrap_or(0.0) - s.pin_v(q, 0).unwrap_or(0.0);
        vmax = vmax.max(vc);
        if std::env::var("DBGK").is_ok() {
            eprintln!(
                "t {:.3} vce {vc:.2} ic {:?} relay {} ok {} log {:?}",
                s.t,
                s.ps[s.index_of(q).unwrap()].i,
                s.ps[s.index_of(k).unwrap()].vis.on,
                s.ok,
                s.log.last()
            );
        }
    }
    eprintln!("vmax {vmax}");
    let st = &s.ps[s.index_of(q).unwrap()];
    assert!(st.burnt.is_some(), "没有续流二极管时三极管应该被击穿");
    let (c, q, k) = build(true);
    let mut s = Sim::new(&c);
    let mut closed = false;
    for _ in 0..120 {
        run(&mut s, &c, 1.0 / 60.0);
        closed |= s.ps[s.index_of(k).unwrap()].vis.on;
    }
    let st = &s.ps[s.index_of(q).unwrap()];
    assert!(st.burnt.is_none(), "{:?}", st.burnt);
    assert!(closed, "继电器应该吸合过");
}

#[test]
fn serial_print_reaches_host() {
    let mut c = Circuit::default();
    let u = c.add_part("uno", 0, 0);
    let cfg = McuCfg {
        pins: vec![],
        rules: vec![Rule {
            when: Cond::Every { ms: 200 },
            then: vec![Act::Print { text: "Hello {count}".into() }, Act::Count],
        }],
        baud: 9600,
    };
    c.part_mut(u).unwrap().set("mcu", serde_json::to_string(&cfg).unwrap());
    let mut s = Sim::new(&c);
    run(&mut s, &c, 1.05);
    let out = String::from_utf8_lossy(&s.host_rx[&u]).to_string();
    assert!(out.contains("Hello 0") && out.contains("Hello 3"), "{out:?}");
}

#[test]
fn battery_drains() {
    let mut c = Circuit::default();
    let b = c.add_part("battery", 0, 0);
    c.part_mut(b).unwrap().set("mah", "1");
    let r = c.add_part("res", 0, 0);
    c.part_mut(r).unwrap().set("r", "37");
    c.part_mut(r).unwrap().set("pmax", "2");
    c.part_mut(r).unwrap().set("span", "0,3");
    let mut s = Sim::new(&c);
    // 约 100 mA，1 mAh 的电池 36 秒用完；跑 10 秒应该用掉约 28%
    run(&mut s, &c, 10.0);
    match &s.ps[0].beh {
        super::sim::Beh::Battery { soc, .. } => assert!(*soc < 0.8 && *soc > 0.6, "soc {soc}"),
        _ => panic!(),
    }
}

/// 给界面测试导出一个 555 电路（带示波器探针、逻辑分析仪）：DUMP=路径 cargo test dump_555 -- --ignored
#[test]
#[ignore]
fn dump_555() {
    let mut c = Circuit::default();
    let psu = c.add_part("psu", 0, 0);
    c.part_mut(psu).unwrap().set("v", "9");
    c.add_part("ne555", 10, 0);
    let r1 = c.add_part("res", 30, 0);
    c.part_mut(r1).unwrap().set("r", "1k");
    let r2 = c.add_part("res", 30, 5);
    c.part_mut(r2).unwrap().set("r", "4.7k");
    let cap = c.add_part("cap", 40, 0);
    c.part_mut(cap).unwrap().set("c", "100n");
    for (a, b) in [
        ((0, 0), (10, 0)),
        ((10, 0), (13, 3)),
        ((10, 0), (30, 0)),
        ((34, 0), (11, 0)),
        ((11, 0), (30, 5)),
        ((34, 5), (12, 0)),
        ((12, 0), (11, 3)),
        ((12, 0), (40, 0)),
        ((41, 0), (0, 2)),
        ((10, 3), (0, 2)),
    ] {
        link(&mut c, a, b);
    }
    let p1 = c.add_part("probe", 12, 3);
    c.part_mut(p1).unwrap().set("ch", "1");
    let p2 = c.add_part("probe", 40, 0);
    c.part_mut(p2).unwrap().set("ch", "2");
    c.add_part("logic", 12, 8);
    link(&mut c, (12, 3), (12, 8));
    link(&mut c, (20, 8), (0, 2));
    std::fs::write(std::env::var("DUMP").unwrap(), c.to_json()).unwrap();
}

#[test]
fn usb_ttl_talks_to_uno() {
    for (host_baud, ok) in [(9600, true), (115200, false)] {
        let mut c = Circuit::default();
        let u = c.add_part("uno", 0, 0);
        c.part_mut(u).unwrap().set("power", "ext");
        let p = c.add_part("psu", -10, 30);
        c.part_mut(p).unwrap().set("v", "5");
        let cfg = McuCfg {
            pins: vec![PinCfg {
                pin: "D13".into(),
                mode: PinMode::OutLow,
            }],
            rules: vec![Rule {
                when: Cond::Recv { text: "on".into() },
                then: vec![Act::Set { pin: "D13".into(), high: true }, Act::Print { text: "ok".into() }],
            }],
            baud: 9600,
        };
        c.part_mut(u).unwrap().set("mcu", serde_json::to_string(&cfg).unwrap());
        let tt = c.add_part("usbttl", 40, 0);
        link(&mut c, (42, 0), (2, 0)); // TXD → D0 (RX)
        link(&mut c, (41, 0), (3, 0)); // RXD ← D1 (TX)
        link(&mut c, (40, 0), (17, 0)); // GND
        link(&mut c, (-10, 30), (10, 20)); // 外部 5V → 5V 脚
        link(&mut c, (-10, 32), (11, 20)); // 地
        let mut s = Sim::new(&c);
        s.host_baud.insert(tt, host_baud);
        run(&mut s, &c, 0.3);
        s.host_send(&c, tt, b"on\n");
        run(&mut s, &c, 0.5);
        let d13 = lib::def("uno").unwrap().pin("D13").unwrap();
        let v = s.pin_v(u, d13).unwrap_or(0.0) - s.pin_v(u, lib::def("uno").unwrap().pin("GND").unwrap()).unwrap_or(0.0);
        let got = String::from_utf8_lossy(s.host_rx.get(&tt).map_or(&[][..], |v| v)).to_string();
        assert_eq!(v > 4.0, ok, "baud {host_baud}: D13 = {v:.2} V");
        assert_eq!(got.contains("ok"), ok, "baud {host_baud}: {got:?}");
    }
}

/// USB 转 TTL 接 ESP-01 / 4G 模块：QCCOM 发 AT 指令，收到模块的回应；HC-05 的另一头是「手机」
/// 跑得很慢（串口波形边沿让步长变得很小），先不放进常规测试
#[test]
#[ignore]
fn modules_answer_at_commands() {
    for (kind, level, cmd, want) in [("esp01", "3", "AT+GMR\r\n", "AT version"), ("cat1", "5", "AT+CSQ\r\n", "+CSQ: 24,99"), ("hc05", "5", "hello", "")] {
        let mut c = Circuit::default();
        let tt = c.add_part("usbttl", 0, 0);
        c.part_mut(tt).unwrap().set("level", "3");
        let m = c.add_part(kind, 0, 20);
        let def = lib::def(kind).unwrap();
        let tp = c.part(tt).unwrap().pins();
        let mp = c.part(m).unwrap().pins();
        let pin = |n: &str| mp[def.pins.iter().position(|p| p.name == n).unwrap()];
        let tx = if def.pin("TX").is_some() { "TX" } else { "TXD" };
        let rx = if def.pin("RX").is_some() { "RX" } else { "RXD" };
        let vcc = ["3V3", "VCC", "VBAT"].into_iter().find(|n| def.pin(n).is_some()).unwrap();
        let mut w = |a: crate::circuit::Pt, b: crate::circuit::Pt| {
            c.add_wire(vec![a, b], 0);
        };
        w(tp[2], pin(rx)); // TXD → RX
        w(tp[1], pin(tx)); // RXD ← TX
        w(tp[0], pin("GND"));
        w(tp[if level == "3" { 3 } else { 4 }], pin(vcc));
        let mut s = Sim::new(&c);
        s.host_baud.insert(tt, if kind == "hc05" { 9600 } else { 115200 });
        if kind == "hc05" {
            // 手机连上
            s.host_baud.insert(m, 9600);
        }
        run(&mut s, &c, 3.0);
        s.host_rx.clear();
        s.host_send(&c, tt, cmd.as_bytes());
        run(&mut s, &c, 0.3);
        let got = String::from_utf8_lossy(s.host_rx.get(&tt).map_or(&[][..], |v| v)).to_string();
        if kind == "hc05" {
            let phone = String::from_utf8_lossy(s.host_rx.get(&m).map_or(&[][..], |v| v)).to_string();
            assert_eq!(phone, "hello", "{}", s.at[&m].status());
        } else {
            assert!(got.contains(want), "{kind}: {got:?} / {}", s.at[&m].status());
        }
    }
}

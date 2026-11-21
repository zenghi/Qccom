//! 示例电路：assets/circuit/examples.txt 里用几行文字描述（放哪些元件、哪两个脚相连），打开时自动布线。
//! 说明文字（原理、试一试、知识点）在 content.tsv 的 ex.<编号>.* 里。
//!
//! 格式：
//! ```text
//! == A1 | 欧姆定律 | Ohm's law | 1          编号 | 中文标题 | 英文标题 | 难度 1..3
//! part B1 battery 0 0 type=9v               标签 种类 x y [rot=1] [属性=值 …]
//! w B1.+ R1.1 [red]                         连线（颜色可省，按网络配色：电源红、地黑）
//! scope B1.+ R1.2 B1.-                      依次将 CH1、CH2、CH3 接到这些引脚上
//! mcu U1 {"pins":[…],"rules":[…]}           单片机的引脚和规则（JSON）
//! power on                                  打开就通电
//! ```

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::OnceLock;

use super::net::Netlist;
use super::{lib, Circuit, Pt};

const SRC: &str = include_str!("../../assets/circuit/examples.txt");

pub struct Example {
    pub id: String,
    pub group: char,
    pub zh: String,
    pub en: String,
    pub level: u8,
    pub power: bool,
    lines: Vec<String>,
}

pub fn all() -> &'static [Example] {
    static L: OnceLock<Vec<Example>> = OnceLock::new();
    L.get_or_init(|| {
        let mut out: Vec<Example> = vec![];
        for line in SRC.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(h) = l.strip_prefix("==") {
                let f: Vec<&str> = h.split('|').map(str::trim).collect();
                let id = f.first().copied().unwrap_or("").to_string();
                out.push(Example {
                    group: id.chars().next().unwrap_or('?'),
                    id,
                    zh: f.get(1).copied().unwrap_or("").into(),
                    en: f.get(2).copied().unwrap_or("").into(),
                    level: f.get(3).and_then(|s| s.parse().ok()).unwrap_or(1),
                    power: false,
                    lines: vec![],
                });
            } else if let Some(e) = out.last_mut() {
                if l == "power on" {
                    e.power = true;
                } else {
                    e.lines.push(l.to_string());
                }
            }
        }
        out
    })
}

pub fn find(id: &str) -> Option<&'static Example> {
    all().iter().find(|e| e.id == id)
}

/// 分组名
pub fn group_name(g: char) -> &'static str {
    use crate::i18n::t;
    match g {
        'A' => t("电学基础"),
        'B' => t("二极管、三极管和 MOS 管"),
        'C' => t("模拟和定时电路"),
        'D' => t("单片机入门"),
        'E' => t("模块和小项目"),
        'F' => t("常见接错（反面教材）"),
        'G' => t("显示、摄像头和物联网"),
        'H' => t("电源和功耗"),
        'I' => t("进阶：PCB、阻抗和天线"),
        'J' => t("器件进阶"),
        _ => "",
    }
}

impl Example {
    pub fn title(&self) -> String {
        match crate::i18n::current() {
            0 => self.zh.clone(),
            2 => {
                let s = super::content::get(&format!("ex.{}.title", self.id));
                if s.is_empty() {
                    self.zh.clone()
                } else {
                    s.to_string()
                }
            }
            _ => self.en.clone(),
        }
    }

    /// 原理说明 intro、试一试 try、学到什么 learn
    pub fn text(&self, field: &str) -> &'static str {
        super::content::get(&format!("ex.{}.{field}", self.id))
    }

    pub fn scope_targets(&self) -> impl Iterator<Item = (usize, &str)> {
        self.lines
            .iter()
            .filter_map(|line| line.strip_prefix("scope "))
            .flat_map(str::split_whitespace)
            .enumerate()
            .map(|(i, pin)| (i + 1, pin))
    }

    /// 搭好的电路
    pub fn build(&self) -> Result<Circuit, String> {
        self.build_circuit(true)
    }

    pub(crate) fn build_practice(&self) -> Result<Circuit, String> {
        self.build_circuit(false)
    }

    fn build_circuit(&self, with_scope: bool) -> Result<Circuit, String> {
        let mut c = Circuit {
            title: self.title(),
            ..Default::default()
        };
        let mut ids: HashMap<String, u32> = HashMap::new();
        let mut links: Vec<((u32, usize), (u32, usize), Option<u8>)> = vec![];
        for l in &self.lines {
            let mut it = l.split_whitespace();
            match it.next() {
                Some("part") => {
                    let (Some(label), Some(kind), Some(x), Some(y)) = (it.next(), it.next(), it.next(), it.next()) else {
                        return Err(format!("{}: {l}", self.id));
                    };
                    if lib::def(kind).is_none() {
                        return Err(format!("{}: unknown part {kind}", self.id));
                    }
                    let id = c.add_part(kind, x.parse().map_err(|_| l.clone())?, y.parse().map_err(|_| l.clone())?);
                    let p = c.part_mut(id).unwrap();
                    p.label = label.to_string();
                    for kv in it {
                        let Some((k, v)) = kv.split_once('=') else { continue };
                        match k {
                            "rot" => p.rot = v.parse().unwrap_or(0),
                            "flip" => p.flip = v == "1",
                            _ => p.set(k, v.replace('_', " ")),
                        }
                    }
                    ids.insert(label.to_string(), id);
                }
                Some("w") => {
                    let (Some(a), Some(b)) = (it.next(), it.next()) else { return Err(format!("{}: {l}", self.id)) };
                    let pa = pin_ref(&c, &ids, a).ok_or_else(|| format!("{}: {a}", self.id))?;
                    let pb = pin_ref(&c, &ids, b).ok_or_else(|| format!("{}: {b}", self.id))?;
                    let color = it.next().and_then(color_of);
                    links.push((pa, pb, color));
                }
                Some("scope") => {}
                Some("mcu") => {
                    let (Some(label), json) = (it.next(), it.collect::<Vec<_>>().join(" ")) else { continue };
                    let id = *ids.get(label).ok_or_else(|| format!("{}: {label}", self.id))?;
                    let cfg: super::mcu::McuCfg = serde_json::from_str(&json).map_err(|e| format!("{}: mcu {e}", self.id))?;
                    c.part_mut(id).unwrap().set("mcu", serde_json::to_string(&cfg).unwrap());
                }
                Some("note") => {
                    let (Some(x), Some(y)) = (it.next(), it.next()) else { continue };
                    let text = it.collect::<Vec<_>>().join(" ");
                    let id = c.alloc_id();
                    c.notes.push(super::Note {
                        id,
                        x: x.parse().unwrap_or(0),
                        y: y.parse().unwrap_or(0),
                        text,
                    });
                }
                _ => return Err(format!("{}: {l}", self.id)),
            }
        }
        route_all(&mut c, &links).map_err(|e| format!("{}: {e}", self.id))?;
        if with_scope {
            self.attach_probes(&mut c, &ids)?;
        }
        Ok(c)
    }

    fn attach_probes(&self, c: &mut Circuit, ids: &HashMap<String, u32>) -> Result<(), String> {
        let netlist = Netlist::build(c);
        let mut measured = HashSet::new();
        for (ch, name) in self.scope_targets() {
            let (id, pin) = pin_ref(c, ids, name).ok_or_else(|| format!("{}: unknown scope pin {name}", self.id))?;
            let net = netlist
                .net_of(id, pin)
                .filter(|&n| netlist.members[n].len() >= 2)
                .ok_or_else(|| format!("{}: scope pin {name} is unconnected", self.id))?;
            if ch > 8 || !measured.insert(net) {
                return Err(format!("{}: duplicate scope net or too many channels at {name}", self.id));
            }
            let at = c.part(id).unwrap().pins()[pin];
            let (rot, reach) = probe_placement(c, at);
            let probe = c.add_part("probe", at.0, at.1);
            let part = c.part_mut(probe).unwrap();
            part.rot = rot;
            part.label = format!("CH{ch}");
            part.set("ch", ch.to_string());
            part.set("reach", reach.to_string());
        }
        Ok(())
    }

    /// 搭建步骤：先放元件，再按顺序接线
    pub fn steps(&self, c: &Circuit) -> Vec<String> {
        use crate::i18n::{t, tf};
        let mut v = vec![];
        let names: Vec<String> = c
            .parts
            .iter()
            .filter(|p| p.kind != "probe")
            .map(|p| format!("{} {}", p.label, t(lib::def(&p.kind).map_or("", |d| d.zh))))
            .collect();
        v.push(tf("放好元件：{}", &[&names.join("、")]));
        for l in &self.lines {
            let mut it = l.split_whitespace();
            if it.next() == Some("w") {
                if let (Some(a), Some(b)) = (it.next(), it.next()) {
                    v.push(tf("连线：{} → {}", &[&pin_name(a), &pin_name(b)]));
                }
            }
        }
        v
    }
}

fn pin_name(r: &str) -> String {
    match r.split_once('.') {
        Some((p, n)) => format!("{p} {n}"),
        None => r.to_string(),
    }
}

fn pin_ref(c: &Circuit, ids: &HashMap<String, u32>, r: &str) -> Option<(u32, usize)> {
    let (label, pin) = r.split_once('.')?;
    let id = *ids.get(label)?;
    let d = lib::def(&c.part(id)?.kind)?;
    let i = d.pins.iter().position(|p| p.name == pin)?;
    Some((id, i))
}

fn color_of(s: &str) -> Option<u8> {
    Some(match s {
        "red" => 0,
        "black" => 1,
        "yellow" => 2,
        "green" => 3,
        "blue" => 4,
        "orange" => 5,
        "white" => 6,
        "purple" => 7,
        "gray" => 8,
        "brown" => 9,
        _ => return None,
    })
}

const SIGNAL_COLORS: [u8; 6] = [2, 3, 4, 5, 7, 6];

fn net_color(c: &Circuit, members: &[(u32, usize)]) -> Option<u8> {
    use lib::cap;
    let mut supply = false;
    for &(id, pin) in members {
        let Some(part) = c.part(id) else { continue };
        let Some(def) = lib::def(&part.kind) else { continue };
        let pd = &def.pins[pin];
        let source = matches!(def.key, "usb5v" | "psu" | "battery" | "siggen");
        if pd.caps & cap::GND != 0 || pd.name == "GND" || source && pd.name == "-" || matches!(pd.name.as_str(), "IN-" | "OUT-" | "B-") && matches!(def.key, "buck" | "boost" | "tp4056") {
            return Some(1);
        }
        supply |= pd.caps & (cap::V5 | cap::V33) != 0
            || source && pd.name == "+"
            || matches!(pd.name.as_str(), "VCC" | "VDD" | "VBAT" | "VBUS" | "VSYS" | "VIN" | "5V" | "3V3" | "VM" | "12V" | "B+" | "IN+" | "OUT+");
    }
    supply.then_some(0)
}

/// 元件外形在画布上的范围（格）
fn bounds(c: &Circuit, id: u32) -> (f32, f32, f32, f32) {
    let p = c.part(id).unwrap();
    let d = lib::def(&p.kind).unwrap();
    let [x0, y0, x1, y1] = d.body;
    let pts = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|corner| p.to_worldf(corner));
    let xs = pts.map(|p| p.0);
    let ys = pts.map(|p| p.1);
    (
        xs.iter().cloned().fold(f32::MAX, f32::min),
        ys.iter().cloned().fold(f32::MAX, f32::min),
        xs.iter().cloned().fold(f32::MIN, f32::max),
        ys.iter().cloned().fold(f32::MIN, f32::max),
    )
}

fn probe_placement(c: &Circuit, at: Pt) -> (u8, u8) {
    let bodies: Vec<_> = c.parts.iter().filter(|p| p.kind != "probe").map(|p| bounds(c, p.id)).collect();
    let badges: Vec<_> = c.parts.iter().filter(|p| p.kind == "probe").map(|p| p.to_worldf((0.0, -p.num("reach") as f32))).collect();
    let pins: HashSet<_> = c.parts.iter().flat_map(|p| p.pins()).collect();
    let score = |rot, reach: u8| {
        let mut cost = usize::from(reach - 1) * 2;
        for step in 1..=reach {
            let (dx, dy) = super::rot_pt((0, -i32::from(step)), rot);
            let (x, y) = (at.0 + dx, at.1 + dy);
            let (fx, fy) = (x as f32, y as f32);
            let covered = bodies.iter().filter(|&&(l, t, r, b)| (l - 0.3..=r + 0.3).contains(&fx) && (t - 0.3..=b + 0.3).contains(&fy)).count();
            let crosses = c.wires.iter().any(|w| w.pts.windows(2).any(|s| super::net::on_segment((x, y), s[0], s[1])));
            cost += 12 * covered + 10 * usize::from(pins.contains(&(x, y))) + 5 * usize::from(crosses);
            if step == reach && badges.iter().any(|&(bx, by)| (fx - bx).abs() < 1.65 && (fy - by).abs() < 0.95) {
                cost += 60;
            }
        }
        cost
    };
    [2, 1, 3]
        .into_iter()
        .flat_map(|reach| [0, 2, 1, 3].into_iter().map(move |rot| (rot, reach)))
        .min_by_key(|&(rot, reach)| score(rot, reach))
        .unwrap()
}

/// 引脚朝向：1 = 朝下（在外形底边），-1 = 朝上，0 = 两侧
fn facing(c: &Circuit, id: u32, at: Pt) -> i32 {
    let (_, y0, _, y1) = bounds(c, id);
    let y = at.1 as f32;
    if (y1 - y0) < 2.0 {
        0
    } else if (y - y1).abs() <= (y - y0).abs() {
        1
    } else {
        -1
    }
}

/// 自动布线：每根线从引脚垂直走出去，到元件外面的「走线通道」里横着走，再垂直回到另一个引脚。
/// 每根线放进去后都检查网表：只能接通想接的，不能碰到别的脚或别的线；不行就换一种走法。
fn route_all(c: &mut Circuit, links: &[((u32, usize), (u32, usize), Option<u8>)]) -> Result<(), String> {
    // 不连线时就已经接在一起的（同名 GND、面包板）
    let base = Netlist::build(c);
    let mut parent: HashMap<(u32, usize), (u32, usize)> = HashMap::new();
    fn find(p: &mut HashMap<(u32, usize), (u32, usize)>, x: (u32, usize)) -> (u32, usize) {
        let mut r = x;
        while let Some(&n) = p.get(&r) {
            if n == r {
                break;
            }
            r = n;
        }
        r
    }
    let mut join = |p: &mut HashMap<(u32, usize), (u32, usize)>, a: (u32, usize), b: (u32, usize)| {
        let (ra, rb) = (find(p, a), find(p, b));
        if ra != rb {
            p.insert(ra, rb);
        }
    };
    for m in &base.members {
        // 两个不同元件的脚压在同一格点上：摆放有问题（面包板除外）
        let parts: std::collections::HashSet<u32> = m.iter().map(|x| x.0).collect();
        if parts.len() > 1 {
            return Err(format!("pins overlap: {m:?}"));
        }
        for w in m.windows(2) {
            join(&mut parent, w[0], w[1]);
        }
    }
    let ymax = c.parts.iter().map(|p| bounds(c, p.id).3).fold(f32::MIN, f32::max).ceil() as i32;
    let ymin = c.parts.iter().map(|p| bounds(c, p.id).1).fold(f32::MAX, f32::min).floor() as i32;
    let (mut below, mut above) = (0, 0);
    for (a, b, color) in links {
        join(&mut parent, *a, *b);
        let pa = c.part(a.0).unwrap().pins()[a.1];
        let pb = c.part(b.0).unwrap().pins()[b.1];
        let fa = facing(c, a.0, pa);
        let fb = facing(c, b.0, pb);
        let name = lib::def(&c.part(a.0).unwrap().kind).unwrap().pins[a.1].name.clone();
        let name_b = lib::def(&c.part(b.0).unwrap().kind).unwrap().pins[b.1].name.clone();
        let col = color.unwrap_or(0);
        let mut cands: Vec<Vec<Pt>> = vec![];
        let lane_dn = |n: i32| ymax + 2 + n;
        let lane_up = |n: i32| ymin - 2 - n;
        // 按引脚朝向优先
        let prefer_up = fa < 0 && fb < 0 || (fa <= 0 && fb <= 0 && fa + fb < 0);
        for n in 0..6 {
            let dn = lane_dn(below + n);
            let up = lane_up(above + n);
            let lanes = if prefer_up { [up, dn] } else { [dn, up] };
            for y in lanes {
                cands.push(vec![pa, (pa.0, y), (pb.0, y), pb]);
            }
            if n == 0 {
                cands.insert(0, vec![pa, (pb.0, pa.1), pb]);
                cands.insert(1, vec![pa, (pa.0, pb.1), pb]);
                if fa == 0 && fb == 0 && pa.1 != pb.1 {
                    let mx = (pa.0 + pb.0) / 2;
                    cands.insert(0, vec![pa, (mx, pa.1), (mx, pb.1), pb]);
                }
                // 同一行 / 同一列直接连
                if pa.0 == pb.0 || pa.1 == pb.1 {
                    cands.insert(0, vec![pa, pb]);
                }
            }
        }
        // 迷宫布线优先：绕开引脚、尽量不压元件、不和别的线重叠、少拐弯
        // 终点可以是同一个网络里任意一个已经连上的引脚（比如板子的另一个 GND，或已经接地的 LED 负极）
        let now = Netlist::build(c);
        let nb = now.net_of(b.0, b.1);
        let na = now.net_of(a.0, a.1);
        let mut targets: Vec<Pt> = vec![pb];
        for p in &c.parts {
            for (i, pt) in p.pins().into_iter().enumerate() {
                let n = now.net_of(p.id, i);
                if pt != pa && n.is_some() && n == nb && n != na && !targets.contains(&pt) {
                    targets.push(pt);
                }
            }
        }
        let grid = RouteGrid::new(c);
        if let Some(m) = grid.maze(pa, &targets) {
            cands.push(m);
        }
        for pts in &mut cands {
            simplify(pts);
        }
        cands.sort_by_key(|pts| grid.cost(pts));
        let mut ok = false;
        for pts in cands {
            if pts.len() < 2 {
                continue;
            }
            let id = c.add_wire(pts.clone(), col);
            if std::env::var("EXDBG").is_ok() {
                eprintln!("try {pts:?}");
            }
            if check(c, &parent) {
                if pts.len() == 4 && pts[1].1 == pts[2].1 {
                    if pts[1].1 > ymax {
                        below = below.max(pts[1].1 - ymax - 1);
                    } else if pts[1].1 < ymin {
                        above = above.max(ymin - pts[1].1 - 1);
                    }
                }
                ok = true;
                break;
            }
            c.wires.retain(|w| w.id != id);
        }
        if !ok {
            return Err(format!("can't route {}.{} → {}.{}", c.part(a.0).unwrap().label, name, c.part(b.0).unwrap().label, name_b));
        }
    }
    improve_routes(c, &parent);
    if std::env::var_os("EXDBG").is_some() {
        for wire in &c.wires {
            eprintln!("wire {:?}", wire.pts);
        }
    }
    let netlist = Netlist::build(c);
    let rail_colors: Vec<_> = netlist.members.iter().map(|members| net_color(c, members)).collect();
    let mut assigned = HashMap::new();
    let mut next_signal = 0;
    for (wire, (_, _, explicit)) in c.wires.iter_mut().zip(links) {
        if let Some(color) = explicit {
            wire.color = *color;
        } else {
            let n = netlist.wire[&wire.id];
            wire.color = *assigned.entry(n).or_insert_with(|| {
                rail_colors[n].unwrap_or_else(|| {
                    let color = SIGNAL_COLORS[next_signal % SIGNAL_COLORS.len()];
                    next_signal += 1;
                    color
                })
            });
        }
    }
    Ok(())
}

/// Once all nets exist, remove avoidable detours around wires that were placed later.
fn improve_routes(c: &mut Circuit, parent: &HashMap<(u32, usize), (u32, usize)>) {
    for index in 0..c.wires.len() {
        let wire = c.wires.remove(index);
        let start = wire.pts[0];
        let end = *wire.pts.last().unwrap();
        let grid = RouteGrid::new(c);
        let current_cost = grid.cost(&wire.pts);
        let direct_cost = start.0.abs_diff(end.0) + start.1.abs_diff(end.1) + if start.0 != end.0 && start.1 != end.1 { TURN_COST } else { 0 };
        if current_cost <= direct_cost {
            c.wires.insert(index, wire);
            continue;
        }
        if let Some(points) = grid.maze(start, &[end]).filter(|points| grid.cost(points) < current_cost) {
            c.wires.push(super::Wire { pts: points, ..wire.clone() });
            let valid = check(c, parent);
            let revised = c.wires.pop().unwrap();
            c.wires.insert(index, if valid { revised } else { wire });
        } else {
            c.wires.insert(index, wire);
        }
    }
}

/// 网表里每个网络的引脚都属于同一个「应该连在一起」的组，且每根线的两头都接通了
fn check(c: &Circuit, parent: &HashMap<(u32, usize), (u32, usize)>) -> bool {
    fn find(p: &HashMap<(u32, usize), (u32, usize)>, x: (u32, usize)) -> (u32, usize) {
        let mut r = x;
        while let Some(&n) = p.get(&r) {
            if n == r {
                break;
            }
            r = n;
        }
        r
    }
    let nl = Netlist::build(c);
    // 新线不能从别的引脚上穿过去，也不能和别的线叠在一起（看不清，之后再接线也容易误连）
    if let Some(w) = c.wires.last() {
        let (e0, e1) = (w.pts[0], *w.pts.last().unwrap());
        for p in c.parts.iter().flat_map(|p| p.pins()) {
            if p != e0 && p != e1 && w.pts.windows(2).any(|s| super::net::on_segment(p, s[0], s[1])) {
                dbg_why(&format!("crosses pin {p:?}"));
                return false;
            }
        }
        for o in &c.wires[..c.wires.len() - 1] {
            for s in w.pts.windows(2) {
                for t in o.pts.windows(2) {
                    if overlap(s[0], s[1], t[0], t[1]) {
                        dbg_why("overlaps a wire");
                        return false;
                    }
                }
            }
        }
    }
    // 线的端点都落在引脚上（没有悬空的线头）
    for w in &c.wires {
        for e in [w.pts[0], *w.pts.last().unwrap()] {
            let on_pin = c.parts.iter().any(|p| p.pins().contains(&e));
            if !on_pin {
                dbg_why("dangling");
                return false;
            }
        }
    }
    for m in &nl.members {
        let r0 = find(parent, m[0]);
        if m.iter().any(|x| find(parent, *x) != r0) {
            dbg_why(&format!("merged {m:?}"));
            return false;
        }
    }
    // 每组应该连通的都在同一个网络
    let mut seen: HashMap<(u32, usize), usize> = HashMap::new();
    for &k in parent.keys() {
        let r = find(parent, k);
        let Some(n) = nl.net_of(k.0, k.1) else { return false };
        if let Some(&n0) = seen.get(&r) {
            if n0 != n {
                dbg_why(&format!("split {k:?}"));
                return false;
            }
        } else {
            seen.insert(r, n);
        }
    }
    true
}

fn simplify(points: &mut Vec<Pt>) {
    points.dedup();
    let mut out = Vec::with_capacity(points.len());
    for &point in points.iter() {
        if let [.., a, b] = out.as_slice() {
            if super::net::on_segment(*b, *a, point) {
                out.pop();
            }
        }
        out.push(point);
    }
    *points = out;
}

const TURN_COST: u32 = 8;
const CROSSING_COST: u32 = 17;
const BODY_COST: u32 = 10;
const DIRECTIONS: [Pt; 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

struct RouteGrid {
    pins: HashSet<Pt>,
    body: HashSet<Pt>,
    occupied: HashSet<(Pt, Pt)>,
    wire_axes: HashMap<Pt, u8>,
    area: (i32, i32, i32, i32),
}

fn cells(a: Pt, b: Pt) -> impl Iterator<Item = Pt> {
    let (dx, dy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
    let len = a.0.abs_diff(b.0).max(a.1.abs_diff(b.1)) as i32;
    (0..=len).map(move |i| (a.0 + i * dx, a.1 + i * dy))
}

fn axis(a: Pt, b: Pt) -> u8 {
    if a.1 == b.1 {
        1
    } else {
        2
    }
}

impl RouteGrid {
    fn new(c: &Circuit) -> Self {
        let mut grid = Self {
            pins: c.parts.iter().flat_map(|p| p.pins()).collect(),
            body: HashSet::new(),
            occupied: HashSet::new(),
            wire_axes: HashMap::new(),
            area: (i32::MAX, i32::MAX, i32::MIN, i32::MIN),
        };
        for p in &c.parts {
            let (x0, y0, x1, y1) = bounds(c, p.id);
            let (l, t, r, b) = (x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32);
            grid.area = (grid.area.0.min(l), grid.area.1.min(t), grid.area.2.max(r), grid.area.3.max(b));
            if !matches!(lib::def(&p.kind).unwrap().look, lib::Look::Breadboard { .. }) {
                for x in l + 1..r {
                    for y in t + 1..b {
                        if x as f32 > x0 && (x as f32) < x1 && y as f32 > y0 && (y as f32) < y1 {
                            grid.body.insert((x, y));
                        }
                    }
                }
            }
        }
        for w in &c.wires {
            for s in w.pts.windows(2) {
                let (a, b) = (s[0], s[1]);
                let mut previous = a;
                for p in cells(a, b).skip(1) {
                    grid.occupied.insert((p.min(previous), p.max(previous)));
                    *grid.wire_axes.entry(previous).or_default() |= axis(a, b);
                    *grid.wire_axes.entry(p).or_default() |= axis(a, b);
                    previous = p;
                }
            }
        }
        grid
    }

    fn step_cost(&self, from: Pt, to: Pt, at_target: bool) -> u32 {
        let touches_wire = self.wire_axes.get(&to).is_some_and(|&axes| axes & (3 - axis(from, to)) != 0);
        1 + u32::from(!at_target && self.body.contains(&to)) * BODY_COST + u32::from(!at_target && touches_wire) * CROSSING_COST
    }

    fn cost(&self, points: &[Pt]) -> u32 {
        let mut cost = points.len().saturating_sub(2) as u32 * TURN_COST;
        let target = points.last().copied();
        for pair in points.windows(2) {
            let mut from = pair[0];
            for to in cells(from, pair[1]).skip(1) {
                if self.occupied.contains(&(from.min(to), from.max(to))) || Some(to) != target && self.pins.contains(&to) {
                    return u32::MAX;
                }
                cost += self.step_cost(from, to, Some(to) == target);
                from = to;
            }
        }
        cost
    }

    fn maze(&self, a: Pt, targets: &[Pt]) -> Option<Vec<Pt>> {
        let (mut x0, mut y0, mut x1, mut y1) = self.area;
        for &(x, y) in targets.iter().chain(std::iter::once(&a)) {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let (x0, y0, x1, y1) = (x0 - 6, y0 - 6, x1 + 6, y1 + 6);
        let estimate = |(x, y): Pt| targets.iter().map(|&(tx, ty)| x.abs_diff(tx) + y.abs_diff(ty)).min().unwrap_or(0);
        let mut dist: HashMap<(Pt, u8), u32> = HashMap::new();
        let mut prev: HashMap<(Pt, u8), (Pt, u8)> = HashMap::new();
        let mut q = BinaryHeap::new();
        for d in 0..4u8 {
            dist.insert((a, d), 0);
            q.push(Reverse((estimate(a), 0u32, a, d)));
        }
        let mut end = None;
        while let Some(Reverse((_, cost, p, d))) = q.pop() {
            if dist.get(&(p, d)).is_some_and(|&x| x < cost) {
                continue;
            }
            if targets.contains(&p) {
                end = Some((p, d));
                break;
            }
            if cost > 4000 {
                continue;
            }
            for (nd, (dx, dy)) in DIRECTIONS.iter().enumerate() {
                let n = (p.0 + dx, p.1 + dy);
                let target = targets.contains(&n);
                if n.0 < x0 || n.0 > x1 || n.1 < y0 || n.1 > y1 || n == a || self.pins.contains(&n) && !target || self.occupied.contains(&(p.min(n), p.max(n))) {
                    continue;
                }
                let turn = if nd as u8 == d { 0 } else { TURN_COST };
                let nc = cost + self.step_cost(p, n, target) + turn;
                let key = (n, nd as u8);
                if dist.get(&key).is_none_or(|&x| nc < x) {
                    dist.insert(key, nc);
                    prev.insert(key, (p, d));
                    q.push(Reverse((nc + estimate(n), nc, n, nd as u8)));
                }
            }
        }
        let mut key = end?;
        let mut path = vec![key.0];
        while let Some(&previous) = prev.get(&key) {
            path.push(previous.0);
            key = previous;
        }
        path.reverse();
        simplify(&mut path);
        Some(path)
    }
}

/// 两段水平 / 竖直线段共线且重叠（不只是碰到一个点）
fn overlap(a0: Pt, a1: Pt, b0: Pt, b1: Pt) -> bool {
    let seg = |p: Pt, q: Pt| (p.0.min(q.0), p.0.max(q.0), p.1.min(q.1), p.1.max(q.1));
    let (ax0, ax1, ay0, ay1) = seg(a0, a1);
    let (bx0, bx1, by0, by1) = seg(b0, b1);
    if ay0 == ay1 && by0 == by1 && ay0 == by0 {
        return ax0.max(bx0) < ax1.min(bx1);
    }
    if ax0 == ax1 && bx0 == bx1 && ax0 == bx0 {
        return ay0.max(by0) < ay1.min(by1);
    }
    false
}

fn dbg_why(s: &str) {
    if std::env::var("EXDBG").is_ok() {
        eprintln!("  ✗ {s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个示例都能搭出来，通电跑一会儿不出错（反面教材 F 组本来就有问题，只要求能搭出来）
    #[test]
    fn every_example_builds() {
        let mut bad = vec![];
        let only = std::env::var("EXID").ok();
        for e in all().iter().filter(|e| only.as_ref().is_none_or(|o| *o == e.id)) {
            match e.build() {
                Ok(c) => {
                    let issues = super::super::check::check(&c);
                    let errs: Vec<String> = issues.iter().filter(|i| i.level == super::super::check::Level::Err).map(|i| i.msg.clone()).collect();
                    if e.group != 'F' && !errs.is_empty() {
                        bad.push(format!("{}: {errs:?}", e.id));
                    }
                }
                Err(err) => bad.push(err),
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn every_example_has_text() {
        let missing: Vec<&str> = all().iter().filter(|e| e.text("intro").is_empty()).map(|e| e.id.as_str()).collect();
        assert!(missing.is_empty(), "{missing:?}");
    }
}

#[cfg(test)]
mod run_tests {
    /// Every authored point is on a distinct net and survives serialization.
    #[test]
    fn every_example_has_scope_probes_on_the_declared_nets() {
        for e in super::all() {
            let built = e.build().unwrap();
            let c = super::Circuit::from_json(&built.to_json()).unwrap();
            let nl = super::Netlist::build(&c);
            let ids = c.parts.iter().map(|p| (p.label.clone(), p.id)).collect();
            let probes: Vec<_> = c.parts.iter().filter(|p| p.kind == "probe").collect();
            assert!((2..=4).contains(&probes.len()), "{}", e.id);
            assert_eq!(probes.len(), e.scope_targets().count(), "{}", e.id);
            let mut unmeasured = c.clone();
            unmeasured.parts.retain(|p| p.kind != "probe");
            let original = super::Netlist::build(&unmeasured);
            assert_eq!(nl.count, original.count, "{}", e.id);
            assert!(original.pin.iter().all(|(pin, net)| nl.pin.get(pin) == Some(net)), "{}", e.id);
            for (probe, (channel, reference)) in probes.into_iter().zip(e.scope_targets()) {
                assert_eq!(probe.prop("ch"), channel.to_string(), "{}", e.id);
                let target = super::pin_ref(&c, &ids, reference).unwrap();
                assert!(nl.same((probe.id, 0), target), "{} CH{channel} {reference}", e.id);
            }
        }
    }

    /// 每个示例通电跑一小会儿（像界面一样每帧有时间预算，不会卡住）不崩溃
    #[test]
    fn every_example_runs() {
        for e in super::all() {
            let c = e.build().unwrap();
            let mut s = super::super::sim::Sim::new(&c);
            for _ in 0..20 {
                s.advance(&c, 1.0 / 60.0, 1.0);
            }
            assert!(s.t > 0.0, "{} 没有往前走", e.id);
            for probe in c.parts.iter().filter(|p| p.kind == "probe") {
                let trace = s.traces.iter().find(|tr| tr.part == probe.id && tr.pin == 0).unwrap();
                assert!(s.pin_v(probe.id, 0).is_some_and(f64::is_finite), "{} {}", e.id, probe.label);
                assert!(trace.at(s.t).is_some_and(f32::is_finite), "{} {}", e.id, probe.label);
            }
            if e.id == "C5" {
                let swing = |ch: &str| {
                    let probe = c.parts.iter().find(|p| p.kind == "probe" && p.prop("ch") == ch).unwrap();
                    let trace = s.traces.iter().find(|tr| tr.part == probe.id).unwrap();
                    let (lo, hi) = trace.range((s.t - 0.02).max(0.0), s.t).unwrap();
                    hi - lo
                };
                assert!(swing("1") > 4.0, "CH1 should see the PWM amplitude");
                assert!(swing("2") < swing("1"), "CH2 should see the filtered output");
            }
        }
    }
}

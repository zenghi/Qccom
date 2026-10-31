//! 网表：哪些引脚接在一起。格点是基本单位——落在同一格点上的引脚、导线端点自动接通；
//! 面包板同一排的孔、元件内部相连的同名引脚（多个 GND）、按键两侧的引脚也算接通。

use std::collections::HashMap;

use super::lib::{self, Look};
use super::{Circuit, Pt};

#[derive(Default, Debug, Clone)]
pub struct Netlist {
    pub count: usize,
    /// (元件 id, 引脚下标) → 网络号
    pub pin: HashMap<(u32, usize), usize>,
    /// 导线 id → 网络号
    pub wire: HashMap<u32, usize>,
    /// 格点 → 网络号（有引脚或导线端点的格点）
    pub point: HashMap<Pt, usize>,
    /// 每个网络上的「真正的」引脚（不含面包板的孔）
    pub members: Vec<Vec<(u32, usize)>>,
}

struct Dsu(Vec<usize>);

impl Dsu {
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

/// 点 p 在线段 a-b 上（含端点）
pub fn on_segment(p: Pt, a: Pt, b: Pt) -> bool {
    let cross = (b.0 - a.0) as i64 * (p.1 - a.1) as i64 - (b.1 - a.1) as i64 * (p.0 - a.0) as i64;
    cross == 0 && p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

impl Netlist {
    pub fn build(c: &Circuit) -> Netlist {
        let mut idx: HashMap<Pt, usize> = HashMap::new();
        let mut pts: Vec<Pt> = vec![];
        let id_of = |p: Pt, idx: &mut HashMap<Pt, usize>, pts: &mut Vec<Pt>| -> usize {
            *idx.entry(p).or_insert_with(|| {
                pts.push(p);
                pts.len() - 1
            })
        };
        // 先登记所有点
        let mut part_pins: Vec<(u32, Vec<usize>)> = vec![];
        for part in &c.parts {
            let ids: Vec<usize> = part.pins().into_iter().map(|p| id_of(p, &mut idx, &mut pts)).collect();
            part_pins.push((part.id, ids));
        }
        let mut wire_ends: Vec<(u32, usize, usize)> = vec![];
        for w in &c.wires {
            if w.pts.len() < 2 {
                continue;
            }
            let a = id_of(w.pts[0], &mut idx, &mut pts);
            let b = id_of(*w.pts.last().unwrap(), &mut idx, &mut pts);
            wire_ends.push((w.id, a, b));
        }
        let mut d = Dsu((0..pts.len()).collect());
        // 元件内部的连接
        for (part, (_, ids)) in c.parts.iter().zip(&part_pins) {
            let Some(def) = lib::def(&part.kind) else { continue };
            if let Look::Breadboard { .. } = def.look {
                let mut groups: HashMap<i64, usize> = HashMap::new();
                for (k, pd) in def.pins.iter().enumerate() {
                    let g = lib::bb_group(pd.at);
                    match groups.get(&g) {
                        Some(first) => d.join(*first, ids[k]),
                        None => {
                            groups.insert(g, ids[k]);
                        }
                    }
                }
                continue;
            }
            // 同名引脚内部相连（多个 GND、数码管两个 COM……）
            for i in 0..def.pins.len() {
                for j in i + 1..def.pins.len() {
                    if def.pins[i].name == def.pins[j].name {
                        d.join(ids[i], ids[j]);
                    }
                }
            }
            if def.key == "button" {
                d.join(ids[0], ids[1]);
                d.join(ids[2], ids[3]);
            }
        }
        // 导线：两端相连；端点落在别的导线中间也接通（T 形接头）
        for (_, a, b) in &wire_ends {
            d.join(*a, *b);
        }
        for w in &c.wires {
            if w.pts.len() < 2 {
                continue;
            }
            for o in &c.wires {
                if o.id == w.id || o.pts.len() < 2 {
                    continue;
                }
                for end in [w.pts[0], *w.pts.last().unwrap()] {
                    if o.pts.windows(2).any(|s| on_segment(end, s[0], s[1])) {
                        let e = idx[&end];
                        let oe = idx[&o.pts[0]];
                        d.join(e, oe);
                    }
                }
            }
        }
        // 编号
        let mut root_net: HashMap<usize, usize> = HashMap::new();
        let mut nl = Netlist::default();
        for i in 0..pts.len() {
            let r = d.find(i);
            let n = root_net.len();
            let net = *root_net.entry(r).or_insert(n);
            nl.point.insert(pts[i], net);
        }
        nl.count = root_net.len();
        nl.members = vec![vec![]; nl.count];
        for (part, (pid, ids)) in c.parts.iter().zip(&part_pins) {
            let bb = lib::def(&part.kind).is_some_and(|d| matches!(d.look, Look::Breadboard { .. }));
            for (k, id) in ids.iter().enumerate() {
                let net = root_net[&d.find(*id)];
                nl.pin.insert((*pid, k), net);
                if !bb {
                    nl.members[net].push((*pid, k));
                }
            }
        }
        for (wid, a, _) in wire_ends {
            nl.wire.insert(wid, root_net[&d.find(a)]);
        }
        nl
    }

    pub fn net_of(&self, part: u32, pin: usize) -> Option<usize> {
        self.pin.get(&(part, pin)).copied()
    }

    /// 两个引脚是否接在一起
    pub fn same(&self, a: (u32, usize), b: (u32, usize)) -> bool {
        matches!((self.pin.get(&a), self.pin.get(&b)), (Some(x), Some(y)) if x == y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadboard_rows_connect() {
        let mut c = Circuit::default();
        c.add_part("bb_half", 0, 0);
        // 电阻插在 a1 和 a5，LED 插在 b5、b6
        let r = c.add_part("res", 0, 3);
        let led = c.add_part("led", 4, 4);
        let nl = Netlist::build(&c);
        assert!(nl.same((r, 1), (led, 0)), "a5 和 b5 同一列应该接通");
        assert!(!nl.same((r, 0), (led, 1)));
        // 跨中缝不通
        let r2 = c.add_part("res", 10, 7);
        c.part_mut(r2).unwrap().set("span", "0,3");
        let nl = Netlist::build(&c);
        assert_ne!(nl.net_of(r2, 0), nl.net_of(r2, 1));
        // 导线把 a1 接到上面的 + 轨
        c.add_wire(vec![(0, 3), (0, 2), (2, 1)], 0);
        let bat = c.add_part("battery", 20, 1);
        let nl = Netlist::build(&c);
        assert!(nl.same((r, 0), (bat, 0)));
    }

    #[test]
    fn same_name_pins_and_tee() {
        let mut c = Circuit::default();
        let u = c.add_part("uno", 0, 0);
        let nl = Netlist::build(&c);
        let d = lib::def("uno").unwrap();
        let gnds: Vec<usize> = d.pins.iter().enumerate().filter(|(_, p)| p.name == "GND").map(|(i, _)| i).collect();
        assert!(gnds.len() >= 2);
        assert!(nl.same((u, gnds[0]), (u, gnds[1])));
        let mut c = Circuit::default();
        let a = c.add_part("res", 0, 0);
        let b = c.add_part("res", 0, 10);
        c.add_wire(vec![(4, 0), (4, 20)], 0);
        c.add_wire(vec![(4, 10), (0, 10)], 1);
        let nl = Netlist::build(&c);
        assert!(nl.same((a, 1), (b, 0)));
    }
}

//! 导线里的电流：仿真只算出每个元件引脚流进 / 流出多少电流，导线本身是理想的。
//! 这里把每个网络的导线、面包板连通条看成一张图，找一棵生成树，按子树电流之和算出每段的电流，
//! 给画布上的「流动的小点」用。有环的地方（两根线并联）环上那根算 0，看起来照样合理。

use std::collections::HashMap;

use crate::circuit::lib::{self, Look};
use crate::circuit::net::on_segment;
use crate::circuit::sim::Sim;
use crate::circuit::{Circuit, Pt};

/// 一段导线：起点、终点、从 a 流向 b 的电流（安）。hidden = 面包板内部 / 元件内部的连接，不画
pub struct Seg {
    pub a: Pt,
    pub b: Pt,
    pub i: f64,
    pub hidden: bool,
}

pub fn wire_currents(c: &Circuit, s: &Sim) -> Vec<Seg> {
    // 点 → 编号
    let mut idx: HashMap<Pt, usize> = HashMap::new();
    let mut pts: Vec<Pt> = vec![];
    let mut id = |p: Pt| {
        *idx.entry(p).or_insert_with(|| {
            pts.push(p);
            pts.len() - 1
        })
    };
    // 边：(a, b, hidden)
    let mut edges: Vec<(usize, usize, bool)> = vec![];
    // 导线的端点和拐点，T 形接头把被接的那段拆开
    let ends: Vec<Pt> = c.wires.iter().filter(|w| w.pts.len() >= 2).flat_map(|w| [w.pts[0], *w.pts.last().unwrap()]).collect();
    for w in c.wires.iter().filter(|w| w.pts.len() >= 2) {
        for s2 in w.pts.windows(2) {
            let (a, b) = (s2[0], s2[1]);
            let mut cut: Vec<Pt> = ends.iter().copied().filter(|e| *e != a && *e != b && on_segment(*e, a, b)).collect();
            cut.sort_by_key(|e| (e.0 - a.0).abs() + (e.1 - a.1).abs());
            cut.dedup();
            let mut prev = a;
            for e in cut.into_iter().chain(std::iter::once(b)) {
                let (ia, ib) = (id(prev), id(e));
                edges.push((ia, ib, false));
                prev = e;
            }
        }
    }
    // 元件引脚注入的电流（流出元件、进入导线为正），面包板的连通条、元件内部同名脚作隐藏边
    let mut inj: HashMap<usize, f64> = HashMap::new();
    let occupied: std::collections::HashSet<Pt> = c
        .parts
        .iter()
        .filter(|p| lib::def(&p.kind).is_some_and(|d| !matches!(d.look, Look::Breadboard { .. })))
        .flat_map(|p| p.pins())
        .chain(ends.iter().copied())
        .collect();
    for (pi, part) in c.parts.iter().enumerate() {
        let Some(def) = lib::def(&part.kind) else { continue };
        let pins = part.pins();
        if let Look::Breadboard { .. } = def.look {
            let mut groups: HashMap<i64, Vec<Pt>> = HashMap::new();
            for (k, pd) in def.pins.iter().enumerate() {
                if occupied.contains(&pins[k]) {
                    groups.entry(lib::bb_group(pd.at)).or_default().push(pins[k]);
                }
            }
            for (_, mut g) in groups {
                g.sort();
                for w in g.windows(2) {
                    let (a, b) = (id(w[0]), id(w[1]));
                    edges.push((a, b, true));
                }
            }
            continue;
        }
        let Some(st) = s.ps.get(pi).filter(|st| st.id == part.id) else { continue };
        for (k, q) in pins.iter().enumerate() {
            let i = st.i.get(k).copied().unwrap_or(0.0);
            let n = id(*q);
            *inj.entry(n).or_default() -= i;
        }
        for i in 0..def.pins.len() {
            for j in i + 1..def.pins.len() {
                if def.pins[i].name == def.pins[j].name {
                    let (a, b) = (id(pins[i]), id(pins[j]));
                    edges.push((a, b, true));
                }
            }
        }
    }
    // 生成树：BFS，记下每个点的父边
    let n = pts.len();
    let mut adj: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
    for (k, (a, b, _)) in edges.iter().enumerate() {
        if a != b {
            adj[*a].push((*b, k));
            adj[*b].push((*a, k));
        }
    }
    let mut parent: Vec<Option<(usize, usize)>> = vec![None; n];
    let mut seen = vec![false; n];
    let mut order = vec![];
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut q = std::collections::VecDeque::from([root]);
        while let Some(v) = q.pop_front() {
            order.push(v);
            for &(u, e) in &adj[v] {
                if !seen[u] {
                    seen[u] = true;
                    parent[u] = Some((v, e));
                    q.push_back(u);
                }
            }
        }
    }
    // 子树和：从叶子往上累加。边上的电流 = 子节点那边流出的总电流，方向子 → 父
    let mut sub: Vec<f64> = (0..n).map(|v| inj.get(&v).copied().unwrap_or(0.0)).collect();
    let mut cur = vec![0.0; edges.len()];
    for &v in order.iter().rev() {
        if let Some((p, e)) = parent[v] {
            let flow = sub[v];
            // 边 (a,b) 从 a 到 b 为正
            cur[e] = if edges[e].0 == v { flow } else { -flow };
            sub[p] += flow;
        }
    }
    edges
        .iter()
        .zip(cur)
        .map(|((a, b, hidden), i)| Seg {
            a: pts[*a],
            b: pts[*b],
            i,
            hidden: *hidden,
        })
        .collect()
}

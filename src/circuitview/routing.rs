//! Canvas geometry. The snap target is the exact grid coordinate passed to the netlist.
use std::collections::{HashMap, HashSet};

use crate::circuit::net::{on_segment, Netlist};
use crate::circuit::{Circuit, Pt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Contact {
    Pin(u32, usize),
    Wire(u32),
}

impl Contact {
    pub(super) fn net(self, netlist: &Netlist) -> Option<usize> {
        match self {
            Self::Pin(id, pin) => netlist.net_of(id, pin),
            Self::Wire(id) => netlist.wire.get(&id).copied(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Snap {
    pub at: Pt,
    pub contact: Option<Contact>,
}

pub(super) fn grid((x, y): (f32, f32)) -> Pt {
    (x.round() as i32, y.round() as i32)
}

pub(super) fn distance((x, y): (f32, f32), (px, py): Pt) -> f32 {
    (x - px as f32).hypot(y - py as f32)
}

fn lattice_point(w: (f32, f32), a: Pt, b: Pt) -> Pt {
    let (dx, dy) = (i64::from(b.0) - i64::from(a.0), i64::from(b.1) - i64::from(a.1));
    let (mut steps, mut rem) = (dx.abs(), dy.abs());
    while rem != 0 {
        (steps, rem) = (rem, steps % rem);
    }
    if steps == 0 {
        return a;
    }
    let (sx, sy) = (dx / steps, dy / steps);
    let (fx, fy) = (sx as f64, sy as f64);
    let (x, y) = (f64::from(w.0) - f64::from(a.0), f64::from(w.1) - f64::from(a.1));
    let k = ((x * fx + y * fy) / (fx * fx + fy * fy)).round().clamp(0.0, steps as f64) as i64;
    ((i64::from(a.0) + sx * k) as i32, (i64::from(a.1) + sy * k) as i32)
}

pub(super) fn snap(c: &Circuit, w: (f32, f32), scale: f32, schematic: bool, skip_wire: Option<u32>) -> Snap {
    let mut best = Snap { at: grid(w), contact: None };
    let mut best_distance = 8.0 / scale;
    // A pin wins exact ties with an existing line. A normal pin wins ties with a breadboard hole.
    let mut consider = |at, contact, radius: f32| {
        let d = distance(w, at);
        if d < best_distance && d <= radius {
            best = Snap { at, contact: Some(contact) };
            best_distance = d;
        }
    };
    let is_board = |kind: &str| crate::circuit::lib::def(kind).is_some_and(|d| matches!(d.look, crate::circuit::lib::Look::Breadboard { .. }));
    let normal = c.parts.iter().rev().filter(|p| p.kind != "probe" && !is_board(&p.kind));
    let holes = c.parts.iter().rev().filter(|p| !schematic && is_board(&p.kind));
    let probes = c.parts.iter().rev().filter(|p| p.kind == "probe");
    for part in normal.chain(holes).chain(probes) {
        for (i, at) in part.pins().into_iter().enumerate() {
            consider(at, Contact::Pin(part.id, i), (8.0 / scale).min(0.76));
        }
    }
    for wire in c.wires.iter().rev().filter(|wire| Some(wire.id) != skip_wire) {
        for segment in wire.pts.windows(2) {
            let at = lattice_point(w, segment[0], segment[1]);
            consider(at, Contact::Wire(wire.id), (7.0 / scale).min(0.8));
        }
    }
    best
}

pub(super) fn append(points: &mut Vec<Pt>, to: Pt, orthogonal: bool, vertical_first: bool) {
    let Some(&from) = points.last() else {
        points.push(to);
        return;
    };
    if from == to {
        return;
    }
    if orthogonal && from.0 != to.0 && from.1 != to.1 {
        points.push(if vertical_first { (from.0, to.1) } else { (to.0, from.1) });
    }
    points.push(to);
}

/// Choose a bend that avoids unrelated wire endpoints. The solver connects
/// every existing endpoint on the new trace, even when it lies between two bends.
pub(super) fn draft_path(c: &Circuit, nl: &Netlist, from: Pt, start: Contact, to: Snap, orthogonal: bool, vertical_first: bool) -> Vec<Pt> {
    if from == to.at {
        return vec![from];
    }
    let allowed = [start.net(nl), to.contact.and_then(|contact| contact.net(nl))];
    let hazards: Vec<_> = c
        .wires
        .iter()
        .filter(|wire| !allowed.contains(&nl.wire.get(&wire.id).copied()))
        .flat_map(|wire| [wire.pts.first(), wire.pts.last()])
        .flatten()
        .copied()
        .collect();
    let score = |points: &[Pt]| hazards.iter().filter(|&&point| points.windows(2).any(|s| on_segment(point, s[0], s[1]))).count();
    let mut best = vec![from];
    append(&mut best, to.at, orthogonal, vertical_first);
    let mut best_score = score(&best);
    if best_score == 0 {
        return best;
    }

    let mut alternate = vec![from];
    append(&mut alternate, to.at, true, !vertical_first);
    let alternate_score = score(&alternate);
    if alternate_score < best_score {
        best = alternate;
        best_score = alternate_score;
    }
    if best_score == 0 {
        return best;
    }

    let (x, y) = from;
    let (tx, ty) = to.at;
    for offset in [1, -1, 2, -2, 3, -3] {
        let px = x.saturating_add(offset);
        let py = y.saturating_add(offset);
        let mut candidates = [vec![from, (px, y), (px, ty), to.at], vec![from, (x, py), (tx, py), to.at]];
        if vertical_first {
            candidates.swap(0, 1);
        }
        for mut points in candidates {
            points.dedup();
            let candidate_score = score(&points);
            if candidate_score < best_score {
                best = points;
                best_score = candidate_score;
            }
            if best_score == 0 {
                return best;
            }
        }
    }
    best
}

pub(super) fn connects_only(circuit: &Circuit, base: &Netlist, points: &[Pt], allowed: &[usize]) -> bool {
    let mut edited = circuit.clone();
    let wire = edited.add_wire(points.to_vec(), 0);
    let after = Netlist::build(&edited);
    let Some(&net) = after.wire.get(&wire) else { return false };
    after.members[net].iter().all(|p| base.pin.get(p).is_some_and(|n| allowed.contains(n)))
        && after
            .wire
            .iter()
            .filter(|(&id, &n)| id != wire && n == net)
            .all(|(id, _)| base.wire.get(id).is_some_and(|n| allowed.contains(n)))
}

pub(super) fn add_branch_vertices(points: &[Pt], endpoints: &HashSet<Pt>) -> Vec<Pt> {
    let mut out = Vec::with_capacity(points.len());
    for segment in points.windows(2) {
        let [a, b] = [segment[0], segment[1]];
        out.push(a);
        let mut middle: Vec<_> = endpoints.iter().copied().filter(|&p| p != a && p != b && on_segment(p, a, b)).collect();
        middle.sort_unstable_by_key(|p| (i64::from(p.0) - i64::from(a.0)).abs() + (i64::from(p.1) - i64::from(a.1)).abs());
        out.extend(middle);
    }
    out.extend(points.last().copied());
    out
}

/// Carry attached wire ends along with moved pins and keep branch points on their original net.
pub(super) fn carry_wires(original: &Circuit, edited: &mut Circuit, shifted: &HashSet<u32>, delta: Pt, orthogonal: bool) {
    let moved_pins: Vec<_> = original
        .parts
        .iter()
        .zip(&edited.parts)
        .filter(|(before, after)| (before.x, before.y, before.rot, before.flip) != (after.x, after.y, after.rot, after.flip))
        .flat_map(|(before, after)| before.pins().into_iter().zip(after.pins()))
        .collect();
    let endpoints: HashSet<_> = original.wires.iter().flat_map(|w| [w.pts.first(), w.pts.last()]).flatten().copied().collect();
    let stationary: HashSet<_> = original
        .parts
        .iter()
        .zip(&edited.parts)
        .filter(|(before, after)| (before.x, before.y, before.rot, before.flip) == (after.x, after.y, after.rot, after.flip))
        .flat_map(|(before, _)| before.pins())
        .collect();
    let mut destinations = HashMap::new();
    for (from, to) in moved_pins.into_iter().filter(|(from, _)| !stationary.contains(from)) {
        let entry = destinations.entry(from).or_insert(to);
        if *entry != to {
            *entry = from;
        }
    }
    if delta != (0, 0) {
        let shift = |(x, y): Pt| (x + delta.0, y + delta.1);
        for wire in original.wires.iter().filter(|w| shifted.contains(&w.id)) {
            for at in add_branch_vertices(&wire.pts, &endpoints) {
                if endpoints.contains(&at) && !stationary.contains(&at) {
                    destinations.entry(at).or_insert_with(|| shift(at));
                }
            }
        }
    }
    for (old, wire) in original.wires.iter().zip(&mut edited.wires) {
        let shifted = shifted.contains(&wire.id) && delta != (0, 0);
        let before = add_branch_vertices(&old.pts, &endpoints);
        let after: Vec<_> = before
            .iter()
            .map(|&at| {
                if let Some(&to) = destinations.get(&at) {
                    to
                } else if shifted && !endpoints.contains(&at) {
                    (at.0 + delta.0, at.1 + delta.1)
                } else {
                    at
                }
            })
            .collect();
        if shifted && after == before {
            if let [a, b] = after.as_slice() {
                let offset = if a.1 == b.1 {
                    (0, delta.1)
                } else if a.0 == b.0 {
                    (delta.0, 0)
                } else {
                    delta
                };
                if offset != (0, 0) {
                    wire.pts = vec![*a, (a.0 + offset.0, a.1 + offset.1), (b.0 + offset.0, b.1 + offset.1), *b];
                }
            }
        } else if after != before {
            wire.pts.clear();
            if let Some(&start) = after.first() {
                wire.pts.push(start);
                for (old, new) in before.windows(2).zip(after.windows(2)) {
                    let vertical = old[0].0 == old[1].0;
                    let keep_orthogonal = orthogonal || vertical || old[0].1 == old[1].1;
                    append(&mut wire.pts, new[1], keep_orthogonal, vertical);
                }
            }
        }
    }
}

pub(super) fn reshape_wire(c: &Circuit, id: u32, original: &[Pt], index: usize, at: Pt, schematic: bool) -> Option<Vec<Pt>> {
    let from = *original.get(index)?;
    if from == at {
        return Some(original.to_vec());
    }
    let branch: HashSet<_> = c.wires.iter().filter(|w| w.id != id).flat_map(|w| [w.pts.first(), w.pts.last()]).flatten().copied().collect();
    if branch.contains(&from) && index != 0 && index + 1 != original.len() {
        // Keep a junction pinned while the adjoining wire segments are edited.
        return None;
    }
    let before = add_branch_vertices(original, &branch);
    let replace = if index == 0 {
        0
    } else if index + 1 == original.len() {
        before.len() - 1
    } else {
        before.iter().position(|&p| p == from)?
    };
    let mut points = vec![if replace == 0 { at } else { before[0] }];
    for (i, pair) in before.windows(2).enumerate() {
        let to = if i + 1 == replace { at } else { pair[1] };
        let vertical = pair[0].0 == pair[1].0;
        append(&mut points, to, schematic || vertical || pair[0].1 == pair[1].1, vertical);
    }
    Some(points)
}

/// Junction dots represent wire endpoints joining another wire or several wire ends meeting.
/// A crossing with neither wire terminating there never enters this set.
pub(super) fn junctions(c: &Circuit) -> Vec<Pt> {
    let mut ends: HashMap<Pt, Vec<u32>> = HashMap::new();
    for w in &c.wires {
        if let [start, .., end] = w.pts.as_slice() {
            ends.entry(*start).or_default().push(w.id);
            ends.entry(*end).or_default().push(w.id);
        }
    }
    let pins: HashSet<_> = c
        .parts
        .iter()
        .filter(|p| crate::circuit::lib::def(&p.kind).is_some_and(|d| !matches!(d.look, crate::circuit::lib::Look::Breadboard { .. })))
        .flat_map(|p| p.pins())
        .collect();
    ends.into_iter()
        .filter_map(|(at, ids)| {
            let branch = c.wires.iter().any(|w| !ids.contains(&w.id) && w.pts.windows(2).any(|s| on_segment(at, s[0], s[1])));
            (branch || ids.len() + usize::from(pins.contains(&at)) >= 3).then_some(at)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_integer_point_on_diagonal_and_nearest_pin() {
        let mut c = Circuit::default();
        let wire = c.add_wire(vec![(0, 0), (6, 4)], 0);
        let target = snap(&c, (3.3, 2.06), 10.0, false, None);
        assert_eq!(target.at, (3, 2));
        assert_eq!(target.contact, Some(Contact::Wire(wire)));
        let id = c.add_part("led", 10, 10);
        assert_eq!(snap(&c, (10.49, 10.0), 10.0, false, None).contact, Some(Contact::Pin(id, 0)));
        assert_eq!(snap(&c, (10.51, 10.0), 10.0, false, None).contact, Some(Contact::Pin(id, 1)));
    }

    #[test]
    fn tutorial_return_wire_routes_around_the_positive_supply() {
        use crate::circuit::examples;
        let mut c = examples::find("A2").unwrap().build_practice().unwrap();
        let led = c.parts.iter().find(|p| p.kind == "led").unwrap();
        let power = c.parts.iter().find(|p| p.kind == "usb5v").unwrap();
        let (from, to) = (led.pins()[1], power.pins()[1]);
        let start = Contact::Pin(led.id, 1);
        let end = Contact::Pin(power.id, 1);
        c.wires.retain(|w| {
            !matches!((w.pts.first(), w.pts.last()), (Some(a), Some(b))
            if (*a == from && *b == to) || (*a == to && *b == from))
        });
        let nl = Netlist::build(&c);
        let allowed = [start.net(&nl).unwrap(), end.net(&nl).unwrap()];
        let mut direct = vec![from];
        append(&mut direct, to, true, false);
        assert!(!connects_only(&c, &nl, &direct, &allowed));
        let routed = draft_path(&c, &nl, from, start, Snap { at: to, contact: Some(end) }, true, false);
        assert!(connects_only(&c, &nl, &routed, &allowed));
        c.add_wire(routed, 0);
        assert!(!crate::circuit::check::check(&c).iter().any(|i| i.level == crate::circuit::check::Level::Err));
    }

    #[test]
    fn moving_an_attached_part_preserves_a_tee() {
        let mut c = Circuit::default();
        let r = c.add_part("res", 0, 0);
        let pin = c.part(r).unwrap().pins()[0];
        let wire = c.add_wire(vec![pin, (pin.0 + 10, pin.1)], 0);
        let point = (pin.0 + 5, pin.1);
        let branch = c.add_wire(vec![point, (point.0, point.1 + 8)], 0);
        let crossing = c.add_wire(vec![(point.0 + 2, point.1 - 4), (point.0 + 2, point.1 + 4)], 0);
        let mut edited = c.clone();
        edited.part_mut(r).unwrap().y += 3;
        carry_wires(&c, &mut edited, &HashSet::new(), (0, 3), true);
        let net = Netlist::build(&edited);
        assert_eq!(net.net_of(r, 0), net.wire.get(&wire).copied());
        assert_eq!(net.wire[&wire], net.wire[&branch]);
        assert_ne!(net.wire[&wire], net.wire[&crossing]);
        assert_eq!(junctions(&c), vec![point]);
    }
}

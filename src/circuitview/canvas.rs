//! 画布：绘制、命中测试、鼠标和键盘交互（摆放、移动、旋转、连线、框选、仿真时按按钮）。

use std::collections::HashMap;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke};

use super::draw::{self, Ctx, View};
use super::{CircuitView, Draft, Drag, Sel};
use crate::circuit::lib::{self, Look};
use crate::circuit::net::on_segment;
use crate::circuit::{fmt_si, rot_pt, Circuit, Part, Pt, WIRE_COLORS};
use crate::i18n::t;
use crate::theme::*;

/// 命中引脚的半径（格）
const PIN_R: f32 = 0.42;

/// 鼠标下面是什么
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    None,
    /// 元件引脚（面包板的孔也算）
    Pin(u32, usize),
    /// 导线上的点：导线、第几个点
    WirePt(u32, usize),
    /// 导线中间
    Wire(u32),
    Part(u32),
    Note(u32),
}

fn snap(w: (f32, f32)) -> Pt {
    (w.0.round() as i32, w.1.round() as i32)
}

fn dist(a: (f32, f32), b: Pt) -> f32 {
    ((a.0 - b.0 as f32).powi(2) + (a.1 - b.1 as f32).powi(2)).sqrt()
}

fn seg_dist(p: (f32, f32), a: Pt, b: Pt) -> f32 {
    let (ax, ay, bx, by) = (a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32);
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let k = if l2 < 1e-6 { 0.0 } else { (((p.0 - ax) * dx + (p.1 - ay) * dy) / l2).clamp(0.0, 1.0) };
    ((p.0 - ax - k * dx).powi(2) + (p.1 - ay - k * dy).powi(2)).sqrt()
}

/// 元件在画布上的范围（世界坐标）：外形 + 所有引脚
pub(super) fn part_bounds(p: &Part) -> Option<(f32, f32, f32, f32)> {
    let d = lib::def(&p.kind)?;
    let [x0, y0, x1, y1] = d.body;
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut add = |(x, y): (f32, f32)| {
        b.0 = b.0.min(x);
        b.1 = b.1.min(y);
        b.2 = b.2.max(x);
        b.3 = b.3.max(y);
    };
    for c in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
        add(p.to_worldf(c));
    }
    if d.stretch {
        for q in p.pins() {
            add((q.0 as f32, q.1 as f32));
        }
    }
    Some(b)
}

/// 两点之间的走线：原理图里走直角，实物里直接拉过去
fn route(a: Pt, b: Pt, schematic: bool) -> Vec<Pt> {
    if schematic && a.0 != b.0 && a.1 != b.1 {
        vec![a, (b.0, a.1), b]
    } else {
        vec![a, b]
    }
}

/// 世界方向 → 元件局部方向（旋转、翻转的逆变换）
fn to_local(p: &Part, d: Pt) -> Pt {
    let (x, y) = rot_pt(d, (4 - p.rot % 4) % 4);
    (if p.flip { -x } else { x }, y)
}

/// 面包板：某个孔所在连通组的所有孔
fn bb_strip(c: &Circuit, id: u32, pin: usize) -> Vec<Pt> {
    let Some(p) = c.part(id) else { return vec![] };
    let Some(d) = lib::def(&p.kind) else { return vec![] };
    let g = lib::bb_group(d.pins[pin].at);
    d.pins.iter().filter(|q| lib::bb_group(q.at) == g).map(|q| p.to_world(q.at)).collect()
}

fn is_bb(p: &Part) -> bool {
    lib::def(&p.kind).is_some_and(|d| matches!(d.look, Look::Breadboard { .. }))
}

impl CircuitView {
    fn hit(&self, w: (f32, f32), only_free: bool) -> Hit {
        // 选中导线的拐点 / 端点优先，方便拖
        for s in &self.sel {
            if let Sel::Wire(id) = s {
                if let Some(wr) = self.c.wires.iter().find(|x| x.id == *id) {
                    if let Some(k) = wr.pts.iter().position(|q| dist(w, *q) < PIN_R) {
                        return Hit::WirePt(*id, k);
                    }
                }
            }
        }
        // 引脚：非面包板元件的引脚优先，其次导线端点，最后面包板孔
        if !only_free {
            for p in self.c.parts.iter().rev().filter(|p| !is_bb(p)) {
                if let Some(i) = p.pins().iter().position(|q| dist(w, *q) < PIN_R) {
                    return Hit::Pin(p.id, i);
                }
            }
        }
        for wr in self.c.wires.iter().rev() {
            for k in [0, wr.pts.len().saturating_sub(1)] {
                if wr.pts.get(k).is_some_and(|q| dist(w, *q) < PIN_R) {
                    return Hit::WirePt(wr.id, k);
                }
            }
        }
        for wr in self.c.wires.iter().rev() {
            if wr.pts.windows(2).any(|s| seg_dist(w, s[0], s[1]) < 0.3) {
                return Hit::Wire(wr.id);
            }
        }
        for n in self.c.notes.iter().rev() {
            let len = n.text.chars().count().max(4) as f32 * 0.55;
            if w.0 >= n.x as f32 - 0.3 && w.0 <= n.x as f32 + len && w.1 >= n.y as f32 - 0.6 && w.1 <= n.y as f32 + 0.6 {
                return Hit::Note(n.id);
            }
        }
        for p in self.c.parts.iter().rev().filter(|p| !is_bb(p)) {
            if let Some((x0, y0, x1, y1)) = part_bounds(p) {
                if w.0 >= x0 && w.0 <= x1 && w.1 >= y0 && w.1 <= y1 {
                    return Hit::Part(p.id);
                }
            }
        }
        // 面包板：孔 → 引脚，其它地方 → 整块
        for p in self.c.parts.iter().rev().filter(|p| is_bb(p)) {
            if !only_free {
                if let Some(i) = p.pins().iter().position(|q| dist(w, *q) < PIN_R) {
                    return Hit::Pin(p.id, i);
                }
            }
            if let Some((x0, y0, x1, y1)) = part_bounds(p) {
                if w.0 >= x0 && w.0 <= x1 && w.1 >= y0 && w.1 <= y1 {
                    return Hit::Part(p.id);
                }
            }
        }
        Hit::None
    }

    fn fit_view(&mut self, r: Rect) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut add = |x: f32, y: f32| {
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        };
        for p in &self.c.parts {
            if let Some((x0, y0, x1, y1)) = part_bounds(p) {
                add(x0, y0);
                add(x1, y1);
            }
        }
        for w in &self.c.wires {
            for q in &w.pts {
                add(q.0 as f32, q.1 as f32);
            }
        }
        if b.0 > b.2 {
            b = (-10.0, -10.0, 30.0, 20.0);
        }
        let (w, h) = ((b.2 - b.0).max(4.0) + 4.0, (b.3 - b.1).max(4.0) + 4.0);
        let s = (r.width() / w).min(r.height() / h).clamp(4.0, 36.0);
        let cx = (b.0 + b.2) / 2.0;
        let cy = (b.1 + b.3) / 2.0;
        self.view = View {
            org: pos2(r.center().x - cx * s, r.center().y - cy * s),
            s,
        };
    }

    /// 开始拖动选中的东西（面包板带着插在上面的元件和导线一起走）
    fn begin_move(&mut self, w: (f32, f32)) {
        let mut parts = vec![];
        let mut wires = vec![];
        let mut notes = vec![];
        let mut ids: Vec<u32> = self.sel.iter().filter_map(|s| if let Sel::Part(id) = s { Some(*id) } else { None }).collect();
        let mut wids: Vec<u32> = self.sel.iter().filter_map(|s| if let Sel::Wire(id) = s { Some(*id) } else { None }).collect();
        // 面包板上插着的东西
        let bbs: Vec<u32> = self.c.parts.iter().filter(|p| ids.contains(&p.id) && is_bb(p)).map(|p| p.id).collect();
        for bb in bbs.iter().filter_map(|id| self.c.part(*id)) {
            let holes: std::collections::HashSet<Pt> = bb.pins().into_iter().collect();
            let on_board: Vec<u32> = self
                .c
                .parts
                .iter()
                .filter(|p| !is_bb(p) && !ids.contains(&p.id) && p.pins().iter().any(|q| holes.contains(q)))
                .map(|p| p.id)
                .collect();
            ids.extend(on_board);
            for wr in &self.c.wires {
                if !wids.contains(&wr.id) && wr.pts.first().is_some_and(|q| holes.contains(q)) && wr.pts.last().is_some_and(|q| holes.contains(q)) {
                    wids.push(wr.id);
                }
            }
        }
        for p in self.c.parts.iter().filter(|p| ids.contains(&p.id)) {
            parts.push((p.id, p.x, p.y));
        }
        for wr in self.c.wires.iter().filter(|x| wids.contains(&x.id)) {
            wires.push((wr.id, wr.pts.clone()));
        }
        for s in &self.sel {
            if let Sel::Note(id) = s {
                if let Some(n) = self.c.notes.iter().find(|n| n.id == *id) {
                    notes.push((n.id, n.x, n.y));
                }
            }
        }
        self.drag = Drag::Move {
            start: w,
            parts,
            wires,
            notes,
            moved: false,
        };
    }

    fn place(&mut self, kind: &'static str, at: Pt) {
        self.checkpoint();
        let id = self.c.add_part(kind, at.0, at.1);
        if let Some(p) = self.c.part_mut(id) {
            p.rot = self.place_rot;
        }
        self.sel = vec![Sel::Part(id)];
    }

    fn finish_draft(&mut self) {
        let Some(d) = self.draft.take() else { return };
        let mut pts = d.pts;
        pts.dedup();
        if pts.len() >= 2 {
            self.checkpoint();
            self.c.add_wire(pts, self.wire_color);
        }
    }

    /// 仿真时点元件：按钮按下、开关拨动
    fn interact(&mut self, id: u32, w: (f32, f32)) -> bool {
        let Some(p) = self.c.part(id) else { return false };
        match p.kind.as_str() {
            "button" => {
                if let Some(s) = &mut self.sim {
                    s.pressed.insert(id);
                }
                self.drag = Drag::Press(id);
                true
            }
            "slide" => {
                let on = p.prop("on") != "false";
                self.c.part_mut(id).unwrap().set("on", if on { "false" } else { "true" });
                self.dirty_at = Some(std::time::Instant::now());
                true
            }
            "dipsw" => {
                // 点到哪一位就拨哪一位：按局部 x 坐标
                let (lx, _) = {
                    let d = (w.0 - p.x as f32, w.1 - p.y as f32);
                    let d = (d.0.round() as i32, d.1.round() as i32);
                    to_local(p, d)
                };
                let mut bits: Vec<char> = p.prop("bits").chars().collect();
                bits.resize(4, '0');
                let k = lx.clamp(0, 3) as usize;
                bits[k] = if bits[k] == '1' { '0' } else { '1' };
                let s: String = bits.into_iter().collect();
                self.c.part_mut(id).unwrap().set("bits", s);
                self.dirty_at = Some(std::time::Instant::now());
                true
            }
            _ => false,
        }
    }

    /// 仿真时在可调元件上滚轮：调旋钮、光照、温度
    fn wheel_adjust(&mut self, id: u32, dy: f32) -> bool {
        let Some(p) = self.c.part(id) else { return false };
        let Some(d) = lib::def(&p.kind) else { return false };
        let Some((k, pd)) = d.props.iter().find(|(_, pd)| matches!(pd.kind, lib::PropKind::Slider(..))) else {
            return false;
        };
        let lib::PropKind::Slider(lo, hi, _, log) = pd.kind else { return false };
        let v = p.num(k);
        let step = dy.signum() as f64;
        let nv = if log {
            (v.max(lo) * 1.15f64.powf(step)).clamp(lo, hi)
        } else {
            (v + (hi - lo) / 50.0 * step).clamp(lo, hi)
        };
        let s = if nv.abs() >= 100.0 || nv == nv.round() { format!("{:.0}", nv) } else { format!("{:.2}", nv) };
        self.c.part_mut(id).unwrap().set(k, s);
        self.dirty_at = Some(std::time::Instant::now());
        true
    }

    pub(super) fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if self.fit {
            self.fit_view(rect);
            self.fit = false;
        }
        let painter = ui.painter_at(rect);
        let input = ui.input(|i| i.clone());
        let hover_pos = input
            .pointer
            .hover_pos()
            .filter(|p| rect.contains(*p) && resp.hovered() || self.placing.is_some_and(|x| x.1) && rect.contains(*p));
        let w = hover_pos.map(|p| self.view.s2w(p));
        if resp.hovered() || self.draft.is_some() {
            self.keys(ui);
        }

        // ── 缩放、平移
        if let Some(hp) = hover_pos {
            let scroll = input.smooth_scroll_delta;
            let mut z = input.zoom_delta();
            let target = w.map(|w| self.hit(w, true));
            let mut used = false;
            if self.powered() && scroll.y != 0.0 && !input.modifiers.command {
                if let Some(Hit::Part(id)) = target {
                    used = self.wheel_adjust(id, scroll.y);
                }
            }
            if !used {
                if input.modifiers.shift {
                    self.view.org += scroll;
                } else if scroll.y != 0.0 {
                    z *= (scroll.y * 0.0018).exp();
                }
                if (z - 1.0).abs() > 1e-4 {
                    let ns = (self.view.s * z).clamp(3.0, 60.0);
                    let k = ns / self.view.s;
                    self.view.org = hp - (hp - self.view.org) * k;
                    self.view.s = ns;
                }
            }
        }
        if resp.dragged_by(PointerButton::Middle) || (resp.dragged_by(PointerButton::Primary) && input.key_down(egui::Key::Space)) {
            self.view.org += resp.drag_delta();
            self.drag = Drag::Pan;
        }

        // ── 鼠标按下
        let pressed = resp.hovered() && input.pointer.primary_pressed() && !input.key_down(egui::Key::Space);
        if let (true, Some(w)) = (pressed, w) {
            let at = snap(w);
            if let Some((kind, false)) = self.placing {
                self.place(kind, at);
                if !input.modifiers.shift {
                    self.placing = None;
                }
            } else if let Some(d) = &mut self.draft {
                // 一下一下点着画：点到引脚 / 导线就结束
                let last = *d.pts.last().unwrap();
                let schem = self.schematic;
                if at != last {
                    let r = route(last, at, schem);
                    d.pts.extend(r.into_iter().skip(1));
                }
                let end = matches!(self.hit(w, false), Hit::Pin(..) | Hit::WirePt(..) | Hit::Wire(..));
                if end || input.pointer.button_double_clicked(PointerButton::Primary) {
                    self.finish_draft();
                }
            } else {
                let h = self.hit(w, false);
                let shift = input.modifiers.shift || input.modifiers.command;
                let handled = match h {
                    Hit::Part(id) | Hit::Pin(id, _) if self.powered() && !is_bb(self.c.part(id).unwrap()) => self.interact(id, w),
                    _ => false,
                };
                if !handled {
                    match h {
                        Hit::WirePt(id, k) if self.sel.contains(&Sel::Wire(id)) => {
                            self.undo.push(self.c.to_json());
                            self.redo.clear();
                            self.drag = Drag::WirePt { id, k, moved: false };
                        }
                        Hit::Pin(id, i) if self.sel.contains(&Sel::Part(id)) && i == 1 && lib::def(&self.c.part(id).unwrap().kind).is_some_and(|d| d.stretch) => {
                            self.undo.push(self.c.to_json());
                            self.redo.clear();
                            self.drag = Drag::Stretch { id, moved: false };
                        }
                        Hit::Pin(..) | Hit::WirePt(..) => {
                            let p = match h {
                                Hit::Pin(id, i) => self.c.part(id).unwrap().pins()[i],
                                Hit::WirePt(id, k) => self.c.wires.iter().find(|x| x.id == id).unwrap().pts[k],
                                _ => at,
                            };
                            self.sel.clear();
                            self.draft = Some(Draft { pts: vec![p], dragging: true });
                        }
                        Hit::Wire(id) | Hit::Part(id) | Hit::Note(id) => {
                            let s = match h {
                                Hit::Wire(_) => Sel::Wire(id),
                                Hit::Note(_) => Sel::Note(id),
                                _ => Sel::Part(id),
                            };
                            if shift {
                                if let Some(i) = self.sel.iter().position(|x| *x == s) {
                                    self.sel.remove(i);
                                } else {
                                    self.sel.push(s);
                                }
                            } else if !self.sel.contains(&s) {
                                self.sel = vec![s];
                            }
                            if self.sel.contains(&s) {
                                self.begin_move(w);
                            }
                        }
                        Hit::None => {
                            if !shift {
                                self.sel.clear();
                            }
                            self.drag = Drag::Box { start: hover_pos.unwrap() };
                        }
                        _ => {}
                    }
                }
            }
        }

        // ── 拖动中
        if input.pointer.primary_down() {
            if let Some(w) = input.pointer.hover_pos().map(|p| self.view.s2w(p)) {
                let at = snap(w);
                match &mut self.drag {
                    Drag::Move { start, parts, wires, notes, moved } => {
                        let d = ((w.0 - start.0).round() as i32, (w.1 - start.1).round() as i32);
                        if d != (0, 0) && !*moved {
                            *moved = true;
                            let snapshot = self.c.to_json();
                            self.undo.push(snapshot);
                            self.redo.clear();
                        }
                        if *moved {
                            let (parts, wires, notes) = (parts.clone(), wires.clone(), notes.clone());
                            for (id, x, y) in parts {
                                if let Some(p) = self.c.part_mut(id) {
                                    p.x = x + d.0;
                                    p.y = y + d.1;
                                }
                            }
                            for (id, pts) in wires {
                                if let Some(wr) = self.c.wires.iter_mut().find(|x| x.id == id) {
                                    wr.pts = pts.iter().map(|q| (q.0 + d.0, q.1 + d.1)).collect();
                                }
                            }
                            for (id, x, y) in notes {
                                if let Some(n) = self.c.notes.iter_mut().find(|n| n.id == id) {
                                    n.x = x + d.0;
                                    n.y = y + d.1;
                                }
                            }
                            self.changed();
                        }
                    }
                    Drag::WirePt { id, k, moved } => {
                        let (id, k) = (*id, *k);
                        *moved = true;
                        if let Some(wr) = self.c.wires.iter_mut().find(|x| x.id == id) {
                            if wr.pts[k] != at {
                                wr.pts[k] = at;
                                self.changed();
                            }
                        }
                    }
                    Drag::Stretch { id, moved } => {
                        let id = *id;
                        *moved = true;
                        if let Some(p) = self.c.part(id) {
                            let l = to_local(p, (at.0 - p.x, at.1 - p.y));
                            if l != (0, 0) && l != p.span() {
                                self.c.part_mut(id).unwrap().set("span", format!("{},{}", l.0, l.1));
                                self.changed();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── 松开
        if input.pointer.primary_released() {
            let rel_w = input.pointer.hover_pos().map(|p| self.view.s2w(p));
            match std::mem::replace(&mut self.drag, Drag::None) {
                Drag::Box { start } => {
                    if let Some(end) = input.pointer.hover_pos() {
                        let r = Rect::from_two_pos(start, end);
                        if r.width() > 3.0 || r.height() > 3.0 {
                            let (a, b) = (self.view.s2w(r.min), self.view.s2w(r.max));
                            let inside = |x: f32, y: f32| x >= a.0 && x <= b.0 && y >= a.1 && y <= b.1;
                            for p in &self.c.parts {
                                if let Some((x0, y0, x1, y1)) = part_bounds(p) {
                                    if inside(x0, y0) && inside(x1, y1) && !self.sel.contains(&Sel::Part(p.id)) {
                                        self.sel.push(Sel::Part(p.id));
                                    }
                                }
                            }
                            for wr in &self.c.wires {
                                if wr.pts.iter().all(|q| inside(q.0 as f32, q.1 as f32)) && !self.sel.contains(&Sel::Wire(wr.id)) {
                                    self.sel.push(Sel::Wire(wr.id));
                                }
                            }
                            for n in &self.c.notes {
                                if inside(n.x as f32, n.y as f32) && !self.sel.contains(&Sel::Note(n.id)) {
                                    self.sel.push(Sel::Note(n.id));
                                }
                            }
                        }
                    }
                }
                Drag::Press(id) => {
                    if let Some(s) = &mut self.sim {
                        s.pressed.remove(&id);
                    }
                }
                Drag::WirePt { id, moved, .. } => {
                    if !moved {
                        self.undo.pop();
                    }
                    // 端点拖回到起点 → 删掉这根线
                    if let Some(wr) = self.c.wires.iter_mut().find(|x| x.id == id) {
                        wr.pts.dedup();
                    }
                    self.c.wires.retain(|x| x.pts.len() >= 2);
                }
                Drag::Stretch { moved, .. } => {
                    if !moved {
                        self.undo.pop();
                    }
                }
                _ => {}
            }
            if let Some(d) = &mut self.draft {
                if d.dragging {
                    if let Some(w) = rel_w {
                        let at = snap(w);
                        let first = d.pts[0];
                        if at == first {
                            // 没拖动：变成一下一下点的模式
                            d.dragging = false;
                        } else {
                            d.pts = route(first, at, self.schematic);
                            self.finish_draft();
                        }
                    } else {
                        self.draft = None;
                    }
                }
            }
            // 从库里拖出来的元件：松开在画布上就放下
            if let Some((kind, true)) = self.placing {
                if let Some(w) = rel_w.filter(|_| input.pointer.hover_pos().is_some_and(|p| rect.contains(p))) {
                    self.place(kind, snap(w));
                }
                self.placing = None;
            }
        }
        if resp.double_clicked() {
            if let Some(Hit::Note(id)) = w.map(|w| self.hit(w, true)) {
                self.sel = vec![Sel::Note(id)];
                self.tab = super::RTab::Props;
            }
        }

        // 右键菜单：记下右键点在什么上
        if resp.secondary_clicked() {
            if let Some(w) = w {
                let h = self.hit(w, true);
                let s = match h {
                    Hit::Part(id) => Some(Sel::Part(id)),
                    Hit::Wire(id) | Hit::WirePt(id, _) => Some(Sel::Wire(id)),
                    Hit::Note(id) => Some(Sel::Note(id)),
                    _ => None,
                };
                match s {
                    Some(s) if !self.sel.contains(&s) => self.sel = vec![s],
                    None => self.sel.clear(),
                    _ => {}
                }
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("cv-ctx-at"), snap(w)));
            }
        }
        resp.context_menu(|ui| self.context_menu(ui));

        // ── 绘制
        self.paint(&painter, rect, w, &input);
    }

    fn context_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(190.0);
        let at: Pt = ui.memory(|m| m.data.get_temp(egui::Id::new("cv-ctx-at"))).unwrap_or((0, 0));
        let one_part = match self.sel.as_slice() {
            [Sel::Part(id)] => Some(*id),
            _ => None,
        };
        let wires: Vec<u32> = self.sel.iter().filter_map(|s| if let Sel::Wire(id) = s { Some(*id) } else { None }).collect();
        let any_part = self.sel.iter().any(|s| matches!(s, Sel::Part(_)));
        if any_part {
            if crate::app::menu_item(ui, t("旋转"), "R") {
                self.rotate_sel(false);
                ui.close_menu();
            }
            if crate::app::menu_item(ui, t("镜像"), "X") {
                self.rotate_sel(true);
                ui.close_menu();
            }
        }
        if let Some(id) = one_part {
            if self.sim.as_ref().is_some_and(|s| s.burnt_ids.contains(&id)) && crate::app::menu_item(ui, t("换个新的（修好）"), "") {
                if let Some(s) = &mut self.sim {
                    s.repair(id);
                }
                ui.close_menu();
            }
        }
        if !wires.is_empty() {
            ui.label(egui::RichText::new(t("导线颜色")).size(11.5).color(TEXT3()));
            ui.horizontal_wrapped(|ui| {
                for (k, (name, c)) in WIRE_COLORS.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    ui.painter().circle(r.center(), 7.0, draw::rgb(*c), Stroke::new(1.0, LINE_HI()));
                    if resp.on_hover_text(format!("{} ({})", t(name), (k + 1) % 10)).clicked() {
                        self.set_wire_color(k as u8);
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
        }
        if !self.sel.is_empty() {
            if crate::app::menu_item(ui, t("复制一份"), "Ctrl+D") {
                if self.copy_sel() {
                    self.paste(2, 2);
                }
                ui.close_menu();
            }
            if crate::app::menu_item(ui, t("删除"), "Del") {
                self.delete_sel();
                ui.close_menu();
            }
        } else {
            if self.clip.is_some() && crate::app::menu_item(ui, t("粘贴"), "Ctrl+V") {
                self.paste(2, 2);
                ui.close_menu();
            }
            if crate::app::menu_item(ui, t("添加文字说明"), "") {
                self.checkpoint();
                let id = self.c.alloc_id();
                self.c.notes.push(crate::circuit::Note {
                    id,
                    x: at.0,
                    y: at.1,
                    text: t("说明").into(),
                });
                self.sel = vec![Sel::Note(id)];
                self.tab = super::RTab::Props;
                ui.close_menu();
            }
            if crate::app::menu_item(ui, t("看全部"), "Home") {
                self.fit = true;
                ui.close_menu();
            }
        }
    }

    fn paint(&mut self, p: &egui::Painter, rect: Rect, w: Option<(f32, f32)>, input: &egui::InputState) {
        let v = self.view;
        p.rect_filled(rect, 0.0, BG0());
        // 格点
        if v.s >= 7.0 {
            let (a, b) = (v.s2w(rect.min), v.s2w(rect.max));
            let step = if v.s >= 12.0 { 1 } else { 2 };
            let dot = tint(if is_light() { 40 } else { 22 });
            let mut x = (a.0.floor() as i32) / step * step;
            while (x as f32) <= b.0 {
                let mut y = (a.1.floor() as i32) / step * step;
                while (y as f32) <= b.1 {
                    let big = x % 10 == 0 && y % 10 == 0;
                    p.circle_filled(v.w2s(x as f32, y as f32), if big { 1.5 } else { 0.9 }, dot);
                    y += step;
                }
                x += step;
            }
        }
        let t_anim = input.time;
        let idx: HashMap<u32, usize> = self.sim.as_ref().map(|s| s.ps.iter().enumerate().map(|(i, st)| (st.id, i)).collect()).unwrap_or_default();
        let visible = |b: (f32, f32, f32, f32)| {
            let r = Rect::from_min_max(v.w2s(b.0, b.1), v.w2s(b.2, b.3));
            r.expand(v.s * 2.0).intersects(rect)
        };
        let draw_part = |part: &Part, sel: bool, label: String, painter: &egui::Painter| {
            let Some(def) = lib::def(&part.kind) else { return };
            if part_bounds(part).is_some_and(|b| !visible(b)) {
                return;
            }
            let st = self.sim.as_ref().and_then(|s| idx.get(&part.id).and_then(|i| s.ps.get(*i)));
            let ctx = Ctx {
                p: painter,
                v,
                part,
                def,
                vis: st.map(|s| &s.vis),
                burnt: st.is_some_and(|s| s.burnt.is_some()),
                t: t_anim,
                sel,
                label,
            };
            if self.schematic {
                draw::draw_symbol(&ctx);
            } else if v.s < 4.0 {
                draw::draw_outline(&ctx);
            } else {
                draw::draw_part(&ctx);
            }
        };
        let labels: HashMap<u32, String> = self.c.parts.iter().map(|q| (q.id, self.c.label(q.id))).collect();
        let lab = |id: u32| {
            let q = self.c.part(id).unwrap();
            let val = draw::value_text(q);
            let l = labels.get(&id).cloned().unwrap_or_default();
            if val.is_empty() {
                l
            } else {
                format!("{l} {val}")
            }
        };

        // 面包板
        if !self.schematic {
            for part in self.c.parts.iter().filter(|q| is_bb(q)) {
                draw_part(part, self.sel.contains(&Sel::Part(part.id)), lab(part.id), p);
            }
        } else {
            self.paint_bb_links(p);
        }
        // 悬停的面包板孔：整条连通的孔高亮
        let hover = w.map(|w| self.hit(w, false)).unwrap_or(Hit::None);
        if let Hit::Pin(id, i) = hover {
            if self.c.part(id).is_some_and(is_bb) && !self.schematic {
                let strip = bb_strip(&self.c, id, i);
                if let (Some(mn), Some(mx)) = (strip.iter().min(), strip.iter().max()) {
                    let r = Rect::from_min_max(v.w2s(mn.0 as f32 - 0.4, mn.1 as f32 - 0.4), v.w2s(mx.0 as f32 + 0.4, mx.1 as f32 + 0.4));
                    p.rect(r, CornerRadius::same(3), ACCENT().gamma_multiply(0.18), Stroke::new(1.5, ACCENT()), egui::StrokeKind::Middle);
                }
            }
        }
        if self.schematic {
            self.paint_wires(p, rect);
        }
        for part in self.c.parts.iter().filter(|q| !is_bb(q)) {
            draw_part(part, self.sel.contains(&Sel::Part(part.id)), lab(part.id), p);
        }
        if !self.schematic {
            self.paint_wires(p, rect);
        }
        if self.sim.is_some() && self.show_flow {
            self.paint_flow(p, t_anim);
        }
        // 文字说明
        for n in &self.c.notes {
            let pos = v.w2s(n.x as f32, n.y as f32);
            let sel = self.sel.contains(&Sel::Note(n.id));
            let g = p.layout_no_wrap(n.text.clone(), FontId::proportional((v.s * 0.55).max(8.0)), TEXT());
            let r = Rect::from_min_size(pos - vec2(4.0, g.size().y / 2.0 + 2.0), g.size() + vec2(8.0, 4.0));
            p.rect(
                r,
                CornerRadius::same(3),
                BG1().gamma_multiply(0.85),
                Stroke::new(1.0, if sel { ACCENT() } else { LINE() }),
                egui::StrokeKind::Inside,
            );
            p.galley(r.min + vec2(4.0, 2.0), g, TEXT());
        }
        // 接错标记：元件右上角的小圆点
        for i in self.issues.iter().filter(|i| i.level != crate::circuit::check::Level::Tip) {
            let col = if i.level == crate::circuit::check::Level::Err { ERR() } else { WARN() };
            for id in &i.parts {
                if let Some(b) = self.c.part(*id).and_then(part_bounds) {
                    let at = v.w2s(b.2, b.1);
                    p.circle(at, 7.0, col, Stroke::new(1.5, BG0()));
                    p.text(at, Align2::CENTER_CENTER, "!", FontId::proportional(10.0), Color32::WHITE);
                }
            }
        }
        // 选中框
        for s in &self.sel {
            if let Sel::Part(id) = s {
                if let Some(b) = self.c.part(*id).and_then(part_bounds) {
                    let r = Rect::from_min_max(v.w2s(b.0, b.1), v.w2s(b.2, b.3)).expand(3.0);
                    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.5, ACCENT()), egui::StrokeKind::Outside);
                    // 可拉长的元件：第二脚画个拖动把手
                    if self.sel.len() == 1 {
                        let q = self.c.part(*id).unwrap();
                        if lib::def(&q.kind).is_some_and(|d| d.stretch) {
                            let e = q.pins()[1];
                            p.circle(v.w2s(e.0 as f32, e.1 as f32), 4.5, BG0(), Stroke::new(1.5, ACCENT()));
                        }
                    }
                }
            }
        }
        // 悬停的引脚
        if let (Hit::Pin(id, i), Some(w)) = (hover, w) {
            if let Some(q) = self.c.part(id) {
                let at = q.pins()[i];
                if !is_bb(q) {
                    p.circle_stroke(v.w2s(at.0 as f32, at.1 as f32), (v.s * 0.35).max(4.0), Stroke::new(1.5, ACCENT()));
                }
                let d = lib::def(&q.kind).unwrap();
                let mut lines = vec![if is_bb(q) {
                    lib::bb_hole_name(d.pins[i].at)
                } else {
                    format!("{} · {}", labels.get(&id).cloned().unwrap_or_default(), d.pins[i].name)
                }];
                if let Some(s) = &self.sim {
                    if let Some(volt) = s.pin_v(id, i) {
                        lines.push(fmt_si(volt, "V"));
                    }
                }
                tooltip(p, v.w2s(w.0, w.1), &lines);
            }
        } else if let (Hit::Part(id), Some(w), Some(s)) = (hover, w, &self.sim) {
            // 通电时悬停元件：电流、功率、温度
            if let Some(st) = idx.get(&id).and_then(|i| s.ps.get(*i)) {
                let q = self.c.part(id).unwrap();
                if !is_bb(q) {
                    let mut lines = vec![labels.get(&id).cloned().unwrap_or_default()];
                    if let Some(why) = &st.burnt {
                        lines.push(format!("🔥 {why}"));
                    } else {
                        let imax = st.i.iter().fold(0.0f64, |a, b| a.max(b.abs()));
                        if imax > 1e-7 {
                            lines.push(format!("{}  {}", fmt_si(imax, "A"), fmt_si(st.p.abs(), "W")));
                        }
                        if st.temp > 40.0 {
                            lines.push(format!("{:.0} °C", st.temp));
                        }
                    }
                    if lines.len() > 1 {
                        tooltip(p, v.w2s(w.0, w.1), &lines);
                    }
                }
            }
        }
        // 正在画的导线
        if let Some(d) = &self.draft {
            let mut pts = d.pts.clone();
            if let Some(w) = w {
                let last = *pts.last().unwrap();
                pts.extend(route(last, snap(w), self.schematic).into_iter().skip(1));
            }
            let col = draw::rgb(WIRE_COLORS[self.wire_color as usize % 10].1);
            let sp: Vec<Pos2> = pts.iter().map(|q| v.w2s(q.0 as f32, q.1 as f32)).collect();
            p.add(Shape::line(sp.clone(), Stroke::new(if self.schematic { 1.5 } else { (v.s * 0.26).max(2.0) }, col.gamma_multiply(0.8))));
            for q in sp {
                p.circle_filled(q, 3.0, ACCENT());
            }
        }
        // 正在放的元件：跟着鼠标的半透明预览
        if let (Some((kind, _)), Some(w)) = (self.placing, w) {
            let at = snap(w);
            let ghost = Part {
                id: 0,
                kind: kind.into(),
                x: at.0,
                y: at.1,
                rot: self.place_rot,
                ..Default::default()
            };
            let mut gp = p.clone();
            gp.set_opacity(0.6);
            if let Some(def) = lib::def(kind) {
                let ctx = Ctx {
                    p: &gp,
                    v,
                    part: &ghost,
                    def,
                    vis: None,
                    burnt: false,
                    t: t_anim,
                    sel: false,
                    label: String::new(),
                };
                if self.schematic {
                    draw::draw_symbol(&ctx);
                } else {
                    draw::draw_part(&ctx);
                }
            }
        }
        if let (Drag::Box { start }, Some(end)) = (&self.drag, input.pointer.hover_pos()) {
            let r = Rect::from_two_pos(*start, end);
            p.rect(r, 0.0, ACCENT().gamma_multiply(0.08), Stroke::new(1.0, ACCENT().gamma_multiply(0.7)), egui::StrokeKind::Inside);
        }
        // 空画布的提示
        if self.c.parts.is_empty() && self.c.wires.is_empty() {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                t("从左边元件库拖元件进来，或者点一下元件再点画布"),
                FontId::proportional(14.0),
                TEXT3(),
            );
        }
        // 左下角：操作提示
        let tip = if self.draft.is_some() {
            t("点击加拐点，点到引脚结束 · Esc 取消 · 数字键换颜色")
        } else if self.placing.is_some() {
            t("点击放下 · R 旋转 · 按住 Shift 连续放 · Esc 取消")
        } else if self.powered() {
            t("点按钮、拨开关；在电位器、光敏电阻上滚轮调节")
        } else {
            t("从引脚拖出导线 · 滚轮缩放 · 中键或空格拖动画布")
        };
        p.text(rect.left_bottom() + vec2(10.0, -10.0), Align2::LEFT_BOTTOM, tip, FontId::proportional(11.5), TEXT3());
    }

    fn paint_wires(&self, p: &egui::Painter, rect: Rect) {
        let v = self.view;
        let _ = rect;
        for wr in &self.c.wires {
            let sel = self.sel.contains(&Sel::Wire(wr.id));
            let pts: Vec<Pos2> = wr.pts.iter().map(|q| v.w2s(q.0 as f32, q.1 as f32)).collect();
            if pts.len() < 2 {
                continue;
            }
            let col = draw::rgb(WIRE_COLORS[wr.color as usize % 10].1);
            if self.schematic {
                if sel {
                    p.add(Shape::line(pts.clone(), Stroke::new(5.0, ACCENT().gamma_multiply(0.35))));
                }
                let vc = self.sim.as_ref().and_then(|s| s.nl.wire.get(&wr.id).and_then(|n| s.net_v(*n))).map(volt_color);
                p.add(Shape::line(pts.clone(), Stroke::new(1.6, vc.unwrap_or(TEXT2()))));
            } else {
                let wdt = (v.s * 0.26).max(2.0);
                if sel {
                    p.add(Shape::line(pts.clone(), Stroke::new(wdt + 5.0, ACCENT().gamma_multiply(0.5))));
                }
                p.add(Shape::line(pts.clone(), Stroke::new(wdt + 1.6, Color32::from_black_alpha(140))));
                p.add(Shape::line(pts.clone(), Stroke::new(wdt, col)));
                // 高光
                let hl = Color32::from_white_alpha(50);
                p.add(Shape::line(pts.iter().map(|q| *q - vec2(0.0, wdt * 0.22)).collect(), Stroke::new((wdt * 0.25).max(1.0), hl)));
                for e in [pts[0], *pts.last().unwrap()] {
                    p.circle(e, (wdt * 0.55).max(2.0), Color32::from_rgb(0xb8, 0xbc, 0xc4), Stroke::new(1.0, Color32::from_black_alpha(120)));
                }
            }
            if sel {
                for q in &pts {
                    p.circle(*q, 3.5, BG0(), Stroke::new(1.5, ACCENT()));
                }
            }
        }
        if self.schematic {
            // 连接点：三个及以上的线 / 引脚汇在一起画实心点
            let mut cnt: HashMap<Pt, u32> = HashMap::new();
            for wr in &self.c.wires {
                for q in [wr.pts[0], *wr.pts.last().unwrap()] {
                    *cnt.entry(q).or_default() += 1;
                    // T 形连接
                    for o in &self.c.wires {
                        if o.id != wr.id && o.pts.windows(2).any(|s| on_segment(q, s[0], s[1]) && q != s[0] && q != s[1]) {
                            *cnt.entry(q).or_default() += 2;
                        }
                    }
                }
            }
            for part in self.c.parts.iter().filter(|q| !is_bb(q)) {
                for q in part.pins() {
                    if let Some(c) = cnt.get_mut(&q) {
                        *c += 1;
                    }
                }
            }
            for (q, n) in cnt {
                if n >= 3 {
                    p.circle_filled(v.w2s(q.0 as f32, q.1 as f32), (v.s * 0.2).max(2.5), TEXT2());
                }
            }
        }
    }

    /// 电流：沿导线流动的小点，越大越快（对数），方向是正电流方向
    fn paint_flow(&self, p: &egui::Painter, t: f64) {
        let Some(s) = &self.sim else { return };
        let v = self.view;
        let col = if self.schematic {
            Color32::from_rgb(0xff, 0xc4, 0x3d)
        } else {
            Color32::from_rgb(0xff, 0xe0, 0x66)
        };
        let r = (v.s * 0.13).clamp(1.6, 4.0);
        for seg in super::flow::wire_currents(&self.c, s) {
            if seg.hidden || seg.i.abs() < 1e-6 {
                continue;
            }
            let (a, b) = if seg.i > 0.0 { (seg.a, seg.b) } else { (seg.b, seg.a) };
            let (ax, ay, bx, by) = (a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32);
            let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
            if len < 0.01 {
                continue;
            }
            // 1 µA → 0.6 格/秒，1 A → 约 9 格/秒
            let speed = (0.6 + 1.4 * (seg.i.abs() / 1e-6).log10()).clamp(0.4, 9.0);
            let spacing = 1.2f32;
            let off = ((t * speed).rem_euclid(spacing as f64)) as f32;
            let mut d = off;
            while d < len {
                let k = d / len;
                p.circle_filled(v.w2s(ax + (bx - ax) * k, ay + (by - ay) * k), r, col);
                d += spacing;
            }
        }
    }

    /// 原理图视图里面包板看不见：同一条连通孔里插着的东西用虚线连起来
    fn paint_bb_links(&self, p: &egui::Painter) {
        let v = self.view;
        let mut occupied: std::collections::HashSet<Pt> = std::collections::HashSet::new();
        for part in self.c.parts.iter().filter(|q| !is_bb(q)) {
            occupied.extend(part.pins());
        }
        for wr in &self.c.wires {
            occupied.insert(wr.pts[0]);
            occupied.insert(*wr.pts.last().unwrap());
        }
        for bb in self.c.parts.iter().filter(|q| is_bb(q)) {
            let d = lib::def(&bb.kind).unwrap();
            let mut groups: HashMap<i64, Vec<Pt>> = HashMap::new();
            for pd in &d.pins {
                let at = bb.to_world(pd.at);
                if occupied.contains(&at) {
                    groups.entry(lib::bb_group(pd.at)).or_default().push(at);
                }
            }
            for (_, mut pts) in groups {
                if pts.len() < 2 {
                    continue;
                }
                pts.sort();
                for s in pts.windows(2) {
                    let (a, b) = (v.w2s(s[0].0 as f32, s[0].1 as f32), v.w2s(s[1].0 as f32, s[1].1 as f32));
                    p.extend(Shape::dashed_line(&[a, b], Stroke::new(1.2, TEXT3()), 4.0, 3.0));
                }
                for q in pts {
                    p.circle_filled(v.w2s(q.0 as f32, q.1 as f32), 2.5, TEXT3());
                }
            }
        }
    }
}

fn tooltip(p: &egui::Painter, at: Pos2, lines: &[String]) {
    let font = FontId::proportional(12.0);
    let gs: Vec<_> = lines.iter().map(|l| p.layout_no_wrap(l.clone(), font.clone(), TEXT())).collect();
    let w = gs.iter().map(|g| g.size().x).fold(0.0, f32::max) + 14.0;
    let h = gs.iter().map(|g| g.size().y).sum::<f32>() + 10.0;
    let mut r = Rect::from_min_size(at + vec2(14.0, 14.0), vec2(w, h));
    let clip = p.clip_rect();
    if r.right() > clip.right() {
        r = r.translate(vec2(-(w + 28.0), 0.0));
    }
    if r.bottom() > clip.bottom() {
        r = r.translate(vec2(0.0, -(h + 28.0)));
    }
    p.rect(r, CornerRadius::same(5), BG2(), Stroke::new(1.0, LINE_HI()), egui::StrokeKind::Inside);
    let mut y = r.top() + 5.0;
    for (i, g) in gs.into_iter().enumerate() {
        let hgt = g.size().y;
        p.galley(pos2(r.left() + 7.0, y), g, if i == 0 { TEXT() } else { TEXT2() });
        y += hgt;
    }
}

/// 电压着色（原理图）：0 V 灰，正电压越高越绿到黄，负电压紫
pub(super) fn volt_color(v: f64) -> Color32 {
    let g = TEXT3();
    if v.abs() < 0.05 {
        return g;
    }
    let k = (v.abs() / 5.0).clamp(0.0, 1.0) as f32;
    let hi = if v > 0.0 { Color32::from_rgb(0x3d, 0xd6, 0x7a) } else { Color32::from_rgb(0xc0, 0x6c, 0xf0) };
    let top = if v > 5.5 { Color32::from_rgb(0xff, 0xb3, 0x30) } else { hi };
    crate::app::lerp_color(g, top, 0.35 + 0.65 * k)
}

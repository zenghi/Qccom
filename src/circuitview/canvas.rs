//! 画布：绘制、命中测试、鼠标和键盘交互（摆放、移动、旋转、连线、框选、仿真时按按钮）。

use std::collections::HashMap;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke};

use super::draw::{self, Ctx, View};
use super::routing::{self, distance as dist, grid as snap, Contact};
use super::{CircuitView, Draft, Drag, RTab, Sel, Tool};
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
        let mut parts: std::collections::HashSet<_> = self.sel.iter().filter_map(|s| match s {
            Sel::Part(id) => Some(*id),
            _ => None,
        }).collect();
        let mut wires: std::collections::HashSet<_> = self.sel.iter().filter_map(|s| match s {
            Sel::Wire(id) => Some(*id),
            _ => None,
        }).collect();
        let notes = self.sel.iter().filter_map(|s| match s {
            Sel::Note(id) => Some(*id),
            _ => None,
        }).collect();
        for board in self.c.parts.iter().filter(|p| is_bb(p) && self.sel.contains(&Sel::Part(p.id))) {
            let holes: std::collections::HashSet<_> = board.pins().into_iter().collect();
            parts.extend(self.c.parts.iter().filter(|p| !is_bb(p) && p.pins().iter().any(|pin| holes.contains(pin))).map(|p| p.id));
            wires.extend(self.c.wires.iter().filter(|w| {
                w.pts.first().is_some_and(|q| holes.contains(q)) && w.pts.last().is_some_and(|q| holes.contains(q))
            }).map(|w| w.id));
        }
        self.drag = Drag::Move {
            start: w,
            original: Box::new(self.c.clone()),
            parts,
            wires,
            notes,
            delta: (0, 0),
        };
    }

    fn place(&mut self, kind: &'static str, at: Pt) {
        self.checkpoint();
        let channel = if kind == "probe" {
            (1..=8).map(|n| n.to_string()).find(|ch| !self.c.parts.iter().any(|p| p.kind == "probe" && p.prop("ch") == ch))
        } else {
            None
        };
        self.place_part(kind, at, channel);
    }

    fn place_part(&mut self, kind: &str, at: Pt, channel: Option<String>) -> u32 {
        let id = self.c.add_part(kind, at.0, at.1);
        if let Some(p) = self.c.part_mut(id) {
            p.rot = self.place_rot;
            if let Some(ch) = channel {
                p.set("ch", ch);
            }
        }
        self.sel = vec![Sel::Part(id)];
        id
    }

    fn probe_at(&mut self, target: routing::Snap) {
        let Some(contact) = target.contact else { return };
        let Some(channel) = (1..=8).map(|n| n.to_string())
            .find(|ch| !self.c.parts.iter().any(|p| p.kind == "probe" && p.prop("ch") == ch)) else {
            self.toast(t("八个测量通道已用完"), WARN());
            return;
        };
        self.checkpoint();
        if let Contact::Wire(id) = contact {
            // A wire endpoint is electrically visible to a probe on an existing segment.
            if let Some(wire) = self.c.wires.iter_mut().find(|w| w.id == id) {
                if wire.pts.first() != Some(&target.at) && wire.pts.last() != Some(&target.at) {
                    if let Some(k) = wire.pts.windows(2).position(|s| on_segment(target.at, s[0], s[1])) {
                        let mut tail = wire.pts.split_off(k + 1);
                        if wire.pts.last() != Some(&target.at) { wire.pts.push(target.at); }
                        if tail.first() != Some(&target.at) { tail.insert(0, target.at); }
                        let color = wire.color;
                        self.c.add_wire(tail, color);
                    }
                }
            }
        }
        self.place_rot = if self.schematic { 2 } else { 0 };
        let id = self.place_part("probe", target.at, Some(channel));
        self.sel.clear();
        self.net_origin = Some(Contact::Pin(id, 0));
        self.scope.current = false;
        self.show_scope();
    }

    fn draft_points(&self, target: routing::Snap) -> Option<Vec<Pt>> {
        let draft = self.draft.as_ref()?;
        let mut points = draft.pts.clone();
        let from = *points.last()?;
        let next = routing::draft_path(&self.c, &self.netlist, from, draft.start, target, self.schematic, self.vertical_first);
        points.extend(next.into_iter().skip(1));
        Some(points)
    }

    fn finish_draft(&mut self, target: Option<routing::Snap>) {
        let Some(draft) = self.draft.as_ref() else { return };
        let points = target.and_then(|at| self.draft_points(at)).unwrap_or_else(|| draft.pts.clone());
        if points.len() < 2 || points.first() == points.last() {
            self.draft = None;
            return;
        }
        let end = target.and_then(|target| target.contact);
        let allowed: Vec<_> = [Some(draft.start), end].into_iter().flatten().filter_map(|contact| contact.net(&self.netlist)).collect();
        if !routing::connects_only(&self.c, &self.netlist, &points, &allowed) {
            self.toast(t("走线会接到其它网络，请移动落点或添加拐点"), WARN());
            return;
        }
        self.draft = None;
        self.checkpoint();
        let id = self.c.add_wire(points, self.wire_color);
        self.net_origin = Some(Contact::Wire(id));
    }

    fn extend_draft(&mut self, target: routing::Snap) {
        if let Some(points) = self.draft_points(target) {
            let draft = self.draft.as_mut().unwrap();
            if points.len() != draft.pts.len() {
                draft.marks.push(draft.pts.len());
                draft.pts = points;
            }
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
        if let Some(previous) = self.canvas_rect.replace(rect) {
            self.view.org += rect.center() - previous.center();
            if self.auto_frame && (rect.size() - previous.size()).length() > 2.0 {
                self.fit = true;
            }
        }
        if self.fit {
            self.auto_frame = true;
            self.fit_view(rect.shrink2(vec2(14.0, 18.0)));
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
                if input.modifiers.shift && scroll != egui::Vec2::ZERO {
                    self.auto_frame = false;
                    self.view.org += scroll;
                } else if scroll.y != 0.0 {
                    z *= (scroll.y * 0.0018).exp();
                }
                if (z - 1.0).abs() > 1e-4 {
                    self.auto_frame = false;
                    let ns = (self.view.s * z).clamp(3.0, 60.0);
                    let k = ns / self.view.s;
                    self.view.org = hp - (hp - self.view.org) * k;
                    self.view.s = ns;
                }
            }
        }
        if input.pointer.button_released(PointerButton::Middle) && matches!(self.drag, Drag::Pan) {
            self.drag = Drag::None;
        }
        if resp.dragged_by(PointerButton::Middle) || (resp.dragged_by(PointerButton::Primary) && input.key_down(egui::Key::Space)) {
            self.auto_frame = false;
            self.view.org += resp.drag_delta();
            self.drag = Drag::Pan;
        }

        let w = hover_pos.map(|p| self.view.s2w(p));
        let target = w.map(|at| routing::snap(&self.c, at, self.view.s, self.schematic, None));
        if self.draft.is_some() && !ui.ctx().wants_keyboard_input() && input.key_pressed(egui::Key::Enter) {
            self.finish_draft(target);
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
            } else if let Some(target) = target.filter(|_| self.draft.is_some()) {
                if target.contact.is_some() {
                    self.finish_draft(Some(target));
                } else {
                    self.extend_draft(target);
                }
            } else if self.tool == Tool::Probe {
                if let Some(target) = target { self.probe_at(target); }
            } else if self.tool == Tool::Wire {
                if let Some(target) = target.filter(|target| target.contact.is_some()) {
                    self.start_wire(target);
                }
            } else {
                let h = self.hit(w, false);
                let shift = input.modifiers.shift || input.modifiers.command;
                let handled = match h {
                    Hit::Part(id) | Hit::Pin(id, _) if self.powered() && !self.paused && !is_bb(self.c.part(id).unwrap()) => self.interact(id, w),
                    _ => false,
                };
                if !handled {
                    match h {
                        Hit::WirePt(id, k) if self.sel.contains(&Sel::Wire(id)) => {
                            if let Some(wire) = self.c.wires.iter().find(|w| w.id == id) {
                                self.drag = Drag::WirePt { id, k, original: wire.pts.clone(), moved: false };
                            }
                        }
                        Hit::Pin(id, i) if self.sel.contains(&Sel::Part(id)) && i == 1 && lib::def(&self.c.part(id).unwrap().kind).is_some_and(|d| d.stretch) => {
                            self.drag = Drag::Stretch { id, moved: false };
                        }
                        Hit::Pin(..) | Hit::WirePt(..) => {
                            if let Some(target) = target.filter(|target| target.contact.is_some()) {
                                self.start_wire(target);
                            }
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
                            self.net_origin = if let Sel::Wire(id) = s { Some(Contact::Wire(id)) } else { None };
                            if self.sel.contains(&s) {
                                self.begin_move(w);
                            }
                        }
                        Hit::None => {
                            if !shift {
                                self.sel.clear();
                                self.net_origin = None;
                                self.focus_points.clear();
                            }
                            self.drag = Drag::Box { start: hover_pos.unwrap() };
                        }
                    }
                }
            }
        }

        // ── 拖动中
        if input.pointer.primary_down() {
            if let Some(w) = input.pointer.hover_pos().map(|p| self.view.s2w(p)) {
                let at = snap(w);
                match &mut self.drag {
                    Drag::Move { start, original, parts, wires, notes, delta } => {
                        let next = snap((w.0 - start.0, w.1 - start.1));
                        if next != *delta {
                            *delta = next;
                            let mut edited = (**original).clone();
                            for p in edited.parts.iter_mut().filter(|p| parts.contains(&p.id)) {
                                p.x += next.0;
                                p.y += next.1;
                            }
                            for n in edited.notes.iter_mut().filter(|n| notes.contains(&n.id)) {
                                n.x += next.0;
                                n.y += next.1;
                            }
                            routing::carry_wires(original, &mut edited, wires, next, self.schematic);
                            self.c = edited;
                            self.paused = self.powered();
                        }
                    }
                    Drag::WirePt { id, k, original, moved } if resp.dragged() => {
                        let (id, k) = (*id, *k);
                        let at = if k == 0 || k + 1 == original.len() {
                            routing::snap(&self.c, w, self.view.s, self.schematic, Some(id)).at
                        } else { at };
                        if let Some(points) = routing::reshape_wire(&self.c, id, original, k, at, self.schematic) {
                            if self.c.wires.iter().any(|wire| wire.id == id && wire.pts != points) {
                                if !*moved {
                                    self.undo.push(self.c.to_json());
                                    self.redo.clear();
                                    *moved = true;
                                }
                                if let Some(wire) = self.c.wires.iter_mut().find(|w| w.id == id) {
                                    wire.pts = points;
                                }
                                self.paused = self.powered();
                            }
                        }
                    }
                    Drag::Stretch { id, moved } if resp.dragged() => {
                        let id = *id;
                        if let Some(p) = self.c.part(id) {
                            let l = to_local(p, (at.0 - p.x, at.1 - p.y));
                            if l != (0, 0) && l != p.span() {
                                if !*moved {
                                    self.undo.push(self.c.to_json());
                                    self.redo.clear();
                                    *moved = true;
                                }
                                self.c.part_mut(id).unwrap().set("span", format!("{},{}", l.0, l.1));
                                self.paused = self.powered();
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
                Drag::Move { original, delta, .. } => {
                    if delta != (0, 0) {
                        self.undo.push(original.to_json());
                        self.redo.clear();
                        self.changed();
                    }
                }
                Drag::WirePt { id, moved: true, .. } => {
                    if let Some(wr) = self.c.wires.iter_mut().find(|x| x.id == id) {
                        wr.pts.dedup();
                    }
                    self.c.wires.retain(|x| x.pts.len() >= 2);
                    self.changed();
                }
                Drag::Stretch { moved: true, .. } => self.changed(),
                _ => {}
            }
            if self.draft.as_ref().is_some_and(|d| d.dragging) {
                if let Some(w) = rel_w.filter(|_| input.pointer.hover_pos().is_some_and(|p| rect.contains(p))) {
                    let target = routing::snap(&self.c, w, self.view.s, self.schematic, None);
                    let first = self.draft.as_ref().unwrap().pts[0];
                    if target.contact.is_some() && target.at != first {
                        self.finish_draft(Some(target));
                        if let Some(d) = &mut self.draft { d.dragging = false; }
                    } else if let Some(d) = &mut self.draft {
                        d.dragging = false;
                    }
                } else {
                    self.draft = None;
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
        if self.tool == Tool::Select && self.draft.is_none() && resp.double_clicked() {
            let width = ui.ctx().screen_rect().width();
            if let Some(Hit::Note(id)) = w.map(|w| self.hit(w, true)) {
                self.sel = vec![Sel::Note(id)];
                self.open_inspector(RTab::Props, width);
            } else if let Some(Hit::Part(id)) = w.map(|w| self.hit(w, true)) {
                self.sel = vec![Sel::Part(id)];
                self.open_inspector(RTab::Props, width);
            } else if let Some(contact) = target.and_then(|target| target.contact) {
                self.net_origin = Some(contact);
                self.open_inspector(RTab::Nets, width);
            }
        }

        // 右键菜单：记下右键点在什么上
        if resp.secondary_clicked() {
            if let Some(w) = w {
                self.net_origin = target.and_then(|target| target.contact);
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
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("cv-ctx-at"), target.map_or_else(|| snap(w), |snap| snap.at)));
            }
        }
        resp.context_menu(|ui| self.context_menu(ui));

        // ── 绘制
        if self.tool != Tool::Select && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        self.paint(&painter, rect, w, target, &input);
    }

    fn start_wire(&mut self, target: routing::Snap) {
        self.sel.clear();
        self.focus_points.clear();
        self.paused = self.powered();
        if let Some(start) = target.contact {
            self.draft = Some(Draft { start, pts: vec![target.at], marks: vec![], dragging: true });
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui) {
        if !self.sel.is_empty() && crate::app::menu_item(ui, t("属性"), "") {
            self.open_inspector(RTab::Props, ui.ctx().screen_rect().width());
            ui.close_menu();
        }
        if self.net_origin.is_some() {
            if crate::app::menu_item(ui, t("查看连接网络"), "") {
                self.open_inspector(RTab::Nets, ui.ctx().screen_rect().width());
                ui.close_menu();
            }
            if crate::app::menu_item(ui, t("在此测量"), "") {
                if let Some(at) = ui.memory(|m| m.data.get_temp::<Pt>(egui::Id::new("cv-ctx-at"))) {
                    self.probe_at(routing::Snap { at, contact: self.net_origin });
                }
                ui.close_menu();
            }
            ui.separator();
        }
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

    fn paint(&mut self, p: &egui::Painter, rect: Rect, w: Option<(f32, f32)>, target: Option<routing::Snap>, input: &egui::InputState) {
        let v = self.view;
        p.rect_filled(rect, 0.0, super::workbench::canvas_color());
        let (a, b) = (v.s2w(rect.min), v.s2w(rect.max));
        let step = if v.s >= 19.0 { 1 } else if v.s >= 9.0 { 2 } else { 5 };
        let dot = if is_light() { Color32::from_rgb(208, 217, 229) } else { Color32::from_rgb(40, 53, 69) };
        let major = if is_light() { Color32::from_rgb(176, 192, 211) } else { Color32::from_rgb(61, 81, 103) };
        let mut x = (a.0.floor() as i32).div_euclid(step) * step;
        while (x as f32) <= b.0 {
            let mut y = (a.1.floor() as i32).div_euclid(step) * step;
            while (y as f32) <= b.1 {
                let big = x % 10 == 0 && y % 10 == 0;
                p.circle_filled(v.w2s(x as f32, y as f32), if big { 1.0 } else { 0.6 }, if big { major } else { dot });
                y += step;
            }
            x += step;
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
        let hover = match target.and_then(|target| target.contact) {
            Some(Contact::Pin(id, pin)) => Hit::Pin(id, pin),
            _ => w.map(|at| self.hit(at, true)).unwrap_or(Hit::None),
        };
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
        let active_net = self.net_origin.and_then(|contact| contact.net(&self.netlist));
        if let Some(net) = active_net {
            for part in self.c.parts.iter().filter(|part| !self.schematic || !is_bb(part)) {
                for (pin, at) in part.pins().into_iter().enumerate() {
                    if self.netlist.net_of(part.id, pin) == Some(net) {
                        p.circle_stroke(v.w2s(at.0 as f32, at.1 as f32), (v.s * 0.22).clamp(2.5, 5.0), Stroke::new(1.1, ACCENT()));
                    }
                }
            }
        }
        let practice = self.practice_targets();
        for &at in self.focus_points.iter().chain(practice.iter().flatten()) {
            let at = v.w2s(at.0 as f32, at.1 as f32);
            p.circle_filled(at, 11.0, WARN().gamma_multiply(0.15));
            p.circle_stroke(at, 9.0, Stroke::new(1.8, WARN()));
            p.circle_filled(at, 2.1, WARN());
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
                    if !is_bb(q) {
                        if let Some(amp) = idx.get(&id).and_then(|k| s.ps[*k].i.get(i)) {
                            lines.push(fmt_si(*amp, "A"));
                        }
                    }
                }
                if self.draft.is_none() && self.tool == Tool::Select {
                    tooltip(p, v.w2s(w.0, w.1), &lines);
                }
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
        if let Some(preview) = self.assistant_preview() {
            let points: Vec<_> = preview.iter().map(|&(x, y)| v.w2s(x as f32, y as f32)).collect();
            if points.len() >= 2 {
                p.extend(Shape::dashed_line(&points, Stroke::new(2.4, VIOLET()), 5.0, 4.0));
                for at in [points[0], points[points.len()-1]] {
                    p.circle(at, 5.5, BG0(), Stroke::new(1.5, VIOLET()));
                }
            }
        }
        if let Some(draft) = &self.draft {
            let points = target.and_then(|at| self.draft_points(at)).unwrap_or_else(|| draft.pts.clone());
            let trace: Vec<_> = points.iter().map(|q| v.w2s(q.0 as f32, q.1 as f32)).collect();
            let starts = draft.pts[0];
            let start = v.w2s(starts.0 as f32, starts.1 as f32);
            if trace.len() > 1 {
                p.add(Shape::line(trace.clone(), Stroke::new(6.0, ACCENT().gamma_multiply(0.2))));
                p.add(Shape::line(trace, Stroke::new(2.0, ACCENT())));
            }
            p.circle_filled(start, 3.1, ACCENT());
        }
        if let Some(target) = target.filter(|_| self.draft.is_some() || self.tool != Tool::Select) {
            let at = v.w2s(target.at.0 as f32, target.at.1 as f32);
            let color = if target.contact.is_some() { ACCENT() } else { TEXT3() };
            let radius = if target.contact.is_some() { 7.0 } else { 4.0 };
            p.circle(at, radius, color.gamma_multiply(0.17), Stroke::new(1.5, color));
            if let Some(contact) = target.contact {
                let mut lines = vec![self.contact_name(contact)];
                if let Some(draft) = &self.draft {
                    let a = draft.start.net(&self.netlist);
                    let b = contact.net(&self.netlist);
                    if let (Some(a), Some(b)) = (a, b) {
                        if a != b && self.netlist.members.get(a).is_some_and(|n| n.len() > 1)
                            && self.netlist.members.get(b).is_some_and(|n| n.len() > 1) {
                            lines.push(t("将合并两个网络").into());
                        }
                    }
                }
                tooltip(p, at, &lines);
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
            gp.set_opacity(0.78);
            for (x, y) in ghost.pins() {
                let pin = v.w2s(x as f32, y as f32);
                p.circle_stroke(pin, 5.5, Stroke::new(1.5, ACCENT()));
            }
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
        if self.c.parts.is_empty() && self.c.wires.is_empty() {
            p.text(rect.center(), Align2::CENTER_CENTER, t("从元件库添加元件，或打开一个示例"), FontId::proportional(15.0), TEXT2());
        }
    }

    fn paint_wires(&self, p: &egui::Painter, rect: Rect) {
        let v = self.view;
        let active_net = self.net_origin.and_then(|contact| contact.net(&self.netlist));
        for wr in &self.c.wires {
            let sel = self.sel.contains(&Sel::Wire(wr.id));
            let traced = active_net.is_some() && active_net == self.netlist.wire.get(&wr.id).copied();
            let pts: Vec<Pos2> = wr.pts.iter().map(|q| v.w2s(q.0 as f32, q.1 as f32)).collect();
            if pts.len() < 2 {
                continue;
            }
            let bounds = Rect::from_points(&pts);
            if !bounds.expand(12.0).intersects(rect) {
                continue;
            }
            let col = draw::rgb(WIRE_COLORS[wr.color as usize % 10].1);
            if traced {
                p.add(Shape::line(pts.clone(), Stroke::new((v.s * 0.26 + 6.0).max(6.0), ACCENT().gamma_multiply(0.30))));
            }
            if self.schematic {
                if sel {
                    p.add(Shape::line(pts.clone(), Stroke::new(5.0, ACCENT().gamma_multiply(0.35))));
                }
                let vc = self.sim.as_ref().and_then(|s| s.nl.wire.get(&wr.id).and_then(|n| s.net_v(*n))).map(volt_color);
                p.add(Shape::line(pts.clone(), Stroke::new(1.6, if traced { ACCENT() } else { vc.unwrap_or(TEXT2()) })));
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
        for &q in &self.junctions {
            let at = v.w2s(q.0 as f32, q.1 as f32);
            if rect.contains(at) {
                let traced = active_net.is_some() && active_net == self.netlist.point.get(&q).copied();
                p.circle(at, (v.s * 0.14).clamp(2.2, 4.3), if traced { ACCENT() } else { TEXT2() }, Stroke::new(1.0, super::workbench::canvas_color()));
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

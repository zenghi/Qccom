//! Acquisition and analysis are independent of UI frame rate. Samples are bounded per channel.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub mod dbc;
pub mod file;
pub mod stream;

pub const CHANNELS: usize = 64;
pub const HISTORY: usize = 65_536;
pub const EVENTS: usize = 4096;
pub type Point = (f64, f64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    #[default]
    FireWater,
    JustFloat,
    RawData,
}

#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    overflow: bool,
    pub rejected: u64,
    aligned: bool,
}
impl Decoder {
    pub fn feed(&mut self, protocol: Protocol, bytes: &[u8], mut frame: impl FnMut(&[f64]), mut ordinary: impl FnMut(&[u8])) {
        let mut values = [0.; CHANNELS];
        match protocol {
            Protocol::RawData => {}
            Protocol::FireWater => {
                for &b in bytes {
                    if b == b'\n' {
                        if self.overflow {
                            ordinary(&self.buf);
                            ordinary(b"\n");
                        } else {
                            let n = std::str::from_utf8(&self.buf).ok().and_then(|line| {
                                let (tag, body) = line.trim().split_once(':').unwrap_or(("", line.trim()));
                                if tag == "image" {
                                    return None;
                                }
                                let mut n = 0;
                                for word in body.split(',') {
                                    let v = word.trim().parse::<f64>().ok()?;
                                    if n == CHANNELS || !v.is_finite() {
                                        return None;
                                    }
                                    values[n] = v;
                                    n += 1;
                                }
                                Some(n)
                            });
                            if let Some(n) = n {
                                frame(&values[..n]);
                            } else {
                                ordinary(&self.buf);
                                ordinary(b"\n");
                            }
                        }
                        self.buf.clear();
                        self.overflow = false;
                    } else {
                        self.buf.push(b);
                        if self.buf.len() == 4096 {
                            ordinary(&self.buf);
                            self.buf.clear();
                            if !self.overflow {
                                self.overflow = true;
                                self.rejected += 1;
                            }
                        }
                    }
                }
            }
            Protocol::JustFloat => {
                for &b in bytes {
                    self.buf.push(b);
                    if self.buf.ends_with(&[0, 0, 128, 127]) {
                        let n = self.buf.len() - 4;
                        if self.aligned && !self.overflow && n % 4 != 0 {
                            continue;
                        }
                        if !self.overflow && n > 0 && n % 4 == 0 && n / 4 <= CHANNELS {
                            for (out, chunk) in values.iter_mut().zip(self.buf[..n].chunks_exact(4)) {
                                *out = f32::from_le_bytes(chunk.try_into().unwrap()) as f64;
                            }
                            self.aligned = true;
                            frame(&values[..n / 4]);
                        } else if !self.overflow {
                            self.rejected += 1;
                        }
                        self.buf.clear();
                        self.overflow = false;
                    } else if self.buf.len() >= CHANNELS * 4 + 4 {
                        if !self.overflow {
                            self.rejected += 1;
                        }
                        self.overflow = true;
                        self.aligned = false;
                        let end = self.buf.len();
                        self.buf.copy_within(end - 3..end, 0);
                        self.buf.truncate(3);
                    }
                }
            }
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub unit: String,
    pub points: VecDeque<Point>,
    #[serde(skip)]
    pub index: PeakIndex,
}
const BUCKET: u64 = 32;
#[derive(Default, Clone)]
pub struct PeakIndex {
    next: u64,
    first_bucket: u64,
    complete: VecDeque<(u64, u64)>,
    low: (u64, f64),
    high: (u64, f64),
}
impl PeakIndex {
    fn append(&mut self, v: f64) {
        let i = self.next;
        if i % BUCKET == 0 {
            self.low = (i, v);
            self.high = (i, v);
        } else {
            if v < self.low.1 {
                self.low = (i, v)
            }
            if v > self.high.1 {
                self.high = (i, v)
            }
        }
        self.next += 1;
        if self.next % BUCKET == 0 {
            if self.complete.len() >= HISTORY / (BUCKET as usize) + 1 {
                self.complete.pop_front();
                self.first_bucket += 1;
            }
            self.complete.push_back((self.low.0, self.high.0));
        }
    }
}
impl Channel {
    pub fn envelope(&self, from: f64, to: f64, width: usize) -> Vec<Point> {
        let pts = &self.points;
        let (a, b) = range(pts, from, to);
        if width == 0 || to <= from {
            return vec![];
        }
        if b - a <= width * 4 || self.index.next < pts.len() as u64 {
            return envelope(pts, from, to, width);
        }
        let base = self.index.next - pts.len() as u64;
        let scale = width as f64 / (to - from);
        let px = |i: usize| ((pts[i].0 - from) * scale) as usize;
        let mut out = Vec::with_capacity(2 * width + 2);
        let mut i = a;
        while i < b {
            let col = px(i);
            let (mut lo, mut hi) = (i, i);
            while i < b && px(i) == col {
                let global = base + i as u64;
                let peak = if global % BUCKET == 0 && b - i >= BUCKET as usize && px(i + BUCKET as usize - 1) == col {
                    (global / BUCKET).checked_sub(self.index.first_bucket).and_then(|k| self.index.complete.get(k as usize))
                } else {
                    None
                };
                let (l, h) = if let Some(&(l, h)) = peak {
                    i += BUCKET as usize;
                    ((l - base) as usize, (h - base) as usize)
                } else {
                    let at = i;
                    i += 1;
                    (at, at)
                };
                if pts[l].1 < pts[lo].1 {
                    lo = l
                }
                if pts[h].1 > pts[hi].1 {
                    hi = h
                }
            }
            out.push(pts[lo.min(hi)]);
            if lo != hi {
                out.push(pts[lo.max(hi)]);
            }
        }
        out
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub t: f64,
    pub source: String,
    pub text: String,
}
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Capture {
    pub channels: Vec<Channel>,
    pub events: VecDeque<Event>,
    pub frames: u64,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub port: String,
    #[serde(default)]
    pub firmware: String,
    #[serde(default)]
    pub dropped: u64,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, f64>,
}
impl Capture {
    pub fn point(&mut self, id: usize, t: f64, v: f64) {
        if !t.is_finite() || !v.is_finite() || id >= CHANNELS {
            return;
        }
        while self.channels.len() <= id {
            let name = format!("CH{}", self.channels.len() + 1);
            self.channels.push(Channel { name, ..Default::default() });
        }
        let channel = &mut self.channels[id];
        channel.index.append(v);
        let points = &mut channel.points;
        if points.len() == HISTORY {
            points.pop_front();
            self.dropped += 1;
        }
        points.push_back((t, v));
    }
    pub fn event(&mut self, t: f64, source: &str, text: String) {
        if self.events.len() == EVENTS {
            self.events.pop_front();
        }
        let e = Event { t, source: source.into(), text };
        if self.events.back().is_some_and(|last| last.t > t) {
            let at = self.events.partition_point(|p| p.t <= t);
            self.events.insert(at, e);
        } else {
            self.events.push_back(e);
        }
    }
    pub fn end(&self) -> f64 {
        self.channels
            .iter()
            .filter_map(|s| s.points.back())
            .map(|p| p.0)
            .chain(self.events.back().map(|e| e.t))
            .fold(0., f64::max)
    }
    pub fn slice(&self, from: f64, to: f64) -> Self {
        let mut out = Self {
            parameters: self.parameters.clone(),
            project: self.project.clone(),
            port: self.port.clone(),
            firmware: self.firmware.clone(),
            dropped: self.dropped,
            ..Self::default()
        };
        for c in &self.channels {
            let (a, b) = range(&c.points, from, to);
            out.channels.push(Channel {
                name: c.name.clone(),
                unit: c.unit.clone(),
                points: c.points.range(a..b).map(|&(t, v)| (t - from, v)).collect(),
                ..Default::default()
            });
        }
        out.events = self
            .events
            .iter()
            .filter(|e| (from..=to).contains(&e.t))
            .cloned()
            .map(|mut e| {
                e.t -= from;
                e
            })
            .collect();
        out.frames = out.channels.iter().map(|c| c.points.len() as u64).max().unwrap_or(0);
        out
    }
}
pub fn range(points: &VecDeque<Point>, from: f64, to: f64) -> (usize, usize) {
    let start = points.partition_point(|v| v.0 < from);
    (start, points.partition_point(|v| v.0 <= to).max(start))
}
/// Preserve the temporal order of minimum and maximum in each pixel column.
pub fn envelope(points: &VecDeque<Point>, from: f64, to: f64, width: usize) -> Vec<Point> {
    let (a, b) = range(points, from, to);
    if width == 0 || to <= from {
        return vec![];
    }
    if b - a <= 2 * width {
        return points.range(a..b).copied().collect();
    }
    let mut out = Vec::with_capacity(2 * width + 2);
    let scale = width as f64 / (to - from);
    let mut column = usize::MAX;
    let mut lo = a;
    let mut hi = a;
    for i in a..b {
        let px = ((points[i].0 - from) * scale) as usize;
        if px != column {
            if column != usize::MAX {
                out.push(points[lo.min(hi)]);
                if lo != hi {
                    out.push(points[lo.max(hi)]);
                }
            }
            column = px;
            lo = i;
            hi = i;
        } else {
            if points[i].1 < points[lo].1 {
                lo = i;
            }
            if points[i].1 > points[hi].1 {
                hi = i;
            }
        }
    }
    out.push(points[lo.min(hi)]);
    if lo != hi {
        out.push(points[lo.max(hi)]);
    }
    out
}
/// Settling time is measured from the first sample of the selected step.
pub fn step_metrics(points: &VecDeque<Point>, from: f64, to: f64, target: f64, tolerance: f64) -> Option<(f64, Option<f64>)> {
    let (a, b) = range(points, from, to);
    if b - a < 2 || !target.is_finite() || !tolerance.is_finite() || tolerance <= 0. {
        return None;
    }
    let (start, initial) = points[a];
    let step = target - initial;
    if step.abs() < f64::EPSILON {
        return None;
    }
    let mut overshoot = 0f64;
    let mut settled = None;
    for &(t, v) in points.range(a..b) {
        overshoot = overshoot.max((v - target) / step);
        if (target - v).abs() <= step.abs() * tolerance {
            settled.get_or_insert(t - start);
        } else {
            settled = None;
        }
    }
    Some((overshoot * 100., settled))
}
/// One-sided amplitude spectrum, Hann window, latest power-of-two block.
pub fn spectrum(points: &VecDeque<Point>, end: f64, hz: f64) -> Vec<Point> {
    let available = points.partition_point(|p| p.0 <= end);
    if available < 8 || !hz.is_finite() || hz <= 0. {
        return vec![];
    }
    let n = 1usize << available.min(8192).ilog2();
    let slice = points.range(available - n..available);
    let mean = slice.clone().map(|p| p.1).sum::<f64>() / n as f64;
    let mut data = vec![(0., 0.); n];
    let mut weight = 0.;
    for (i, p) in slice.enumerate() {
        let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos();
        data[i.reverse_bits() >> (usize::BITS - n.ilog2())] = ((p.1 - mean) * w, 0.);
        weight += w;
    }
    let mut len = 2;
    while len <= n {
        let omega = -std::f64::consts::TAU / len as f64;
        let (sin, cos) = omega.sin_cos();
        for block in data.chunks_exact_mut(len) {
            let (a, b) = block.split_at_mut(len / 2);
            let (mut wr, mut wi) = (1., 0.);
            for (u, v) in a.iter_mut().zip(b) {
                let (r, i) = (v.0 * wr - v.1 * wi, v.0 * wi + v.1 * wr);
                *v = (u.0 - r, u.1 - i);
                *u = (u.0 + r, u.1 + i);
                (wr, wi) = (wr * cos - wi * sin, wr * sin + wi * cos);
            }
        }
        len *= 2;
    }
    data[..=n / 2]
        .iter()
        .enumerate()
        .map(|(i, (x, y))| (i as f64 * hz / n as f64, x.hypot(*y) * if i == 0 || i == n / 2 { 1. } else { 2. } / weight))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_frames_and_recover() {
        let mut raw = vec![0x55; 600];
        raw.extend_from_slice(&[0, 0, 128, 127]);
        for _ in 0..3 {
            raw.extend(1.25f32.to_le_bytes());
            raw.extend((-8f32).to_le_bytes());
            raw.extend([0, 0, 128, 127]);
        }
        for chunk in 1..20 {
            let mut d = Decoder::default();
            let mut count = 0;
            for s in raw.chunks(chunk) {
                d.feed(
                    Protocol::JustFloat,
                    s,
                    |f| {
                        assert_eq!(f, &[1.25, -8.]);
                        count += 1
                    },
                    |_| {},
                );
            }
            assert_eq!(count, 3);
        }
        let mut d = Decoder::default();
        let mut out = vec![];
        for s in b"d:1,2\r\ntext\n3,4\n".chunks(2) {
            d.feed(Protocol::FireWater, s, |v| out.push(v.to_vec()), |_| {});
        }
        assert_eq!(out, vec![vec![1., 2.], vec![3., 4.]]);
    }
    #[test]
    fn spikes_and_fft() {
        let mut c = Capture::default();
        for i in 0..65536 {
            c.point(0, i as f64, if i == 1987 { 90. } else { 0. });
        }
        let reduced = c.channels[0].envelope(0., 65536., 800);
        assert_eq!(reduced, envelope(&c.channels[0].points, 0., 65536., 800));
        assert!(reduced.len() <= 1602);
        assert!(reduced.contains(&(1987., 90.)));
        for i in 65536..81765 {
            c.point(0, i as f64, ((i % 71) as f64).sin());
        }
        for width in [15, 120, 1000] {
            let ch = &c.channels[0];
            assert_eq!(ch.envelope(10000., 80000., width), envelope(&ch.points, 10000., 80000., width));
        }
        let p = (0..1024).map(|i| (i as f64 / 1024., (std::f64::consts::TAU * 64. * i as f64 / 1024.).sin())).collect();
        let f = spectrum(&p, 1., 1024.);
        assert_eq!(f.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0, 64.);
    }
}

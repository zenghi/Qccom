//! 耗电：电源输出电流、电池还能用多久、各元件耗电排行，以及接实物用 INA219 / INA226 或任意串口文本测电流。

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, RichText, Sense, Stroke};

use crate::circuit::lib::{self, Cat};
use crate::circuit::sim::{Beh, Sim};
use crate::circuit::{fmt_si, Circuit};
use crate::i18n::t;
use crate::theme::*;

/// 仿真里的统计：按元件累计电荷、能量（求长时间平均，单片机休眠、模块突发电流都能算准）
#[derive(Default)]
pub struct PowerStats {
    last_t: f64,
    /// 元件 id → (∑ I·dt, ∑ P·dt, ∑ dt)
    acc: HashMap<u32, (f64, f64, f64)>,
}

/// 电源类元件：正极引脚下标
fn source_pin(kind: &str) -> Option<usize> {
    match kind {
        "usb5v" | "battery" | "psu" => Some(0),
        _ => None,
    }
}

impl PowerStats {
    pub fn reset(&mut self) {
        *self = PowerStats::default();
    }

    /// 每帧调用一次
    pub fn update(&mut self, c: &Circuit, s: &Sim) {
        let dt = s.t - self.last_t;
        self.last_t = s.t;
        if !(dt > 0.0 && dt < 1.0) {
            return;
        }
        for (pi, part) in c.parts.iter().enumerate() {
            let Some(st) = s.ps.get(pi).filter(|x| x.id == part.id) else { continue };
            let i = match source_pin(&part.kind) {
                // 电源正极流出的电流（引脚电流以流进元件为正）
                Some(k) => -st.i.get(k).copied().unwrap_or(0.0),
                None => st.i.iter().fold(0.0f64, |a, b| a.max(b.abs())),
            };
            let e = self.acc.entry(part.id).or_default();
            e.0 += i * dt;
            e.1 += st.p * dt;
            e.2 += dt;
        }
    }

    fn avg(&self, id: u32) -> Option<(f64, f64)> {
        self.acc.get(&id).filter(|e| e.2 > 0.0).map(|e| (e.0 / e.2, e.1 / e.2))
    }
}

/// 小时数 → 「3 天 4 小时」
pub fn fmt_hours(h: f64) -> String {
    if !h.is_finite() || h > 24.0 * 365.0 * 5.0 {
        return t("很久（5 年以上）").into();
    }
    if h >= 48.0 {
        let d = (h / 24.0).floor();
        format!("{d:.0} {} {:.0} {}", t("天"), h - d * 24.0, t("小时"))
    } else if h >= 1.0 {
        format!("{h:.1} {}", t("小时"))
    } else {
        format!("{:.0} {}", (h * 60.0).max(1.0), t("分钟"))
    }
}

/// 电池能用多久：容量 × 可用比例（截止电压前约 85%），扣掉自放电
pub fn battery_hours(mah: f64, avg_ma: f64, self_dis_month: f64) -> f64 {
    if avg_ma <= 1e-6 {
        return f64::INFINITY;
    }
    let usable = mah * 0.85;
    // 自放电折算成等效电流：每月掉 self_dis × 容量
    let leak = mah * self_dis_month / (30.0 * 24.0);
    usable / (avg_ma + leak)
}

fn bar(ui: &mut egui::Ui, frac: f32, col: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(120.0), 6.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::same(3), BG3());
    let w = r.width() * frac.clamp(0.0, 1.0);
    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(w.max(1.0), r.height())), CornerRadius::same(3), col);
}

pub fn power_ui(ui: &mut egui::Ui, ps: &PowerStats, c: &Circuit, s: &Sim) {
    // 电源
    let mut any = false;
    for (pi, part) in c.parts.iter().enumerate() {
        if source_pin(&part.kind).is_none() {
            continue;
        }
        let Some(st) = s.ps.get(pi).filter(|x| x.id == part.id) else { continue };
        if !any {
            ui.label(RichText::new(t("电源")).size(12.5).strong().color(TEXT()));
            ui.add_space(2.0);
            any = true;
        }
        let name = format!("{} {}", c.label(part.id), t(lib::def(&part.kind).map_or("", |d| d.zh)));
        let now_i = -st.i[0];
        let (avg_i, avg_p) = ps.avg(part.id).unwrap_or((now_i, 0.0));
        ui.label(RichText::new(name).size(12.0).color(ACCENT()));
        ui.label(
            RichText::new(format!(
                "{} {}   {} {}   {} {}",
                t("现在"),
                fmt_si(now_i.max(0.0), "A"),
                t("平均"),
                fmt_si(avg_i.max(0.0), "A"),
                t("功率"),
                fmt_si((-avg_p).max(0.0), "W")
            ))
            .font(FontId::monospace(11.5))
            .color(TEXT2()),
        );
        if let Beh::Battery { soc, spec, .. } = &st.beh {
            let mah = part.num("mah");
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} {:.0}%", t("电量"), soc * 100.0)).size(12.0).color(TEXT2()));
                bar(ui, *soc as f32, if *soc > 0.2 { OK() } else { WARN() });
            });
            let h = battery_hours(mah * soc, avg_i.max(0.0) * 1000.0, spec.self_dis);
            ui.label(RichText::new(format!("{} {}", t("按现在的平均电流还能用"), fmt_hours(h))).size(12.0).color(TEXT()));
        }
        ui.add_space(6.0);
    }
    // 耗电排行（不含电源、面包板、仪器）
    let mut rank: Vec<(String, f64)> = c
        .parts
        .iter()
        .filter(|p| source_pin(&p.kind).is_none() && lib::def(&p.kind).is_some_and(|d| !matches!(d.cat, Cat::Board | Cat::Meter)))
        .filter_map(|p| ps.avg(p.id).map(|(_, pw)| (format!("{} {}", c.label(p.id), t(lib::def(&p.kind).unwrap().zh)), pw)))
        .filter(|x| x.1 > 1e-6)
        .collect();
    rank.sort_by(|a, b| b.1.total_cmp(&a.1));
    if !rank.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new(t("耗电排行")).size(12.5).strong().color(TEXT()));
        ui.add_space(2.0);
        let total: f64 = rank.iter().map(|x| x.1).sum();
        egui::Grid::new("cv-rank").num_columns(3).spacing(vec2(8.0, 3.0)).show(ui, |ui| {
            for (name, pw) in rank.iter().take(8) {
                ui.label(RichText::new(crate::app::truncate(name, 16)).size(11.5).color(TEXT2()));
                bar(ui, (pw / rank[0].1) as f32, ACCENT());
                ui.label(RichText::new(format!("{} {:.0}%", fmt_si(*pw, "W"), pw / total * 100.0)).font(FontId::monospace(11.0)).color(TEXT3()));
                ui.end_row();
            }
        });
    }
}

// ───────────────────────── 接实物测电流 ─────────────────────────

/// 串口电流计：读一行一行的文本，认出里面的电流值
pub struct Meter {
    pub port: String,
    pub baud: u32,
    pub cap_mah: String,
    run: Option<Arc<AtomicBool>>,
    data: Arc<Mutex<MeterData>>,
}

#[derive(Default)]
struct MeterData {
    /// (时刻, mA)
    pts: VecDeque<(Instant, f64)>,
    mah: f64,
    peak: f64,
    sum: f64,
    n: u64,
    err: Option<String>,
    last: Option<Instant>,
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            port: String::new(),
            baud: 115200,
            cap_mah: "2000".into(),
            run: None,
            data: Default::default(),
        }
    }
}

/// 从一行文本里认出电流（mA）。认得：「Current: 12.3 mA」「I=0.0123A」「12.3mA」「45uA」，
/// INA219 示例程序的「Current:       12.30 mA」，以及纯数字（当作 mA）或 CSV（取带 mA / current 的那列，否则第一列）
pub fn parse_current(line: &str) -> Option<f64> {
    let l = line.trim();
    if l.is_empty() {
        return None;
    }
    let low = l.to_lowercase();
    // 按「数字 + 单位」找
    let bytes: Vec<char> = low.chars().collect();
    let mut found: Vec<(f64, &str, usize)> = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() || (bytes[i] == '-' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit())) || (bytes[i] == '.' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit())) {
            let st = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.' || bytes[i] == 'e' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit() || *c == '-')) {
                i += 1;
            }
            let num: String = bytes[st..i].iter().collect();
            let Ok(v) = num.parse::<f64>() else { continue };
            let mut j = i;
            while j < bytes.len() && bytes[j] == ' ' {
                j += 1;
            }
            let rest: String = bytes[j..bytes.len().min(j + 2)].iter().collect();
            let unit = if rest.starts_with("ma") {
                "mA"
            } else if rest.starts_with("ua") || rest.starts_with("µa") {
                "uA"
            } else if rest.starts_with('a') && !rest.starts_with("ad") {
                "A"
            } else if rest.starts_with("mv") || rest.starts_with('v') || rest.starts_with("mw") || rest.starts_with('w') {
                "other"
            } else {
                ""
            };
            found.push((v, unit, st));
        } else {
            i += 1;
        }
    }
    let to_ma = |v: f64, u: &str| match u {
        "uA" => v / 1000.0,
        "A" => v * 1000.0,
        _ => v,
    };
    if let Some((v, u, _)) = found.iter().find(|x| matches!(x.1, "mA" | "uA" | "A")) {
        return Some(to_ma(*v, u));
    }
    // 「current」「i=」后面的第一个数
    for key in ["current", "i=", "i:", "电流"] {
        if let Some(k) = low.find(key) {
            if let Some((v, u, _)) = found.iter().find(|x| x.2 > k && x.1 != "other") {
                return Some(to_ma(*v, u));
            }
        }
    }
    found.iter().find(|x| x.1.is_empty()).map(|x| x.0)
}

impl Meter {
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    fn stop(&mut self) {
        if let Some(r) = self.run.take() {
            r.store(false, Ordering::Relaxed);
        }
    }

    fn start(&mut self) {
        self.stop();
        *self.data.lock().unwrap() = MeterData::default();
        let run = Arc::new(AtomicBool::new(true));
        self.run = Some(run.clone());
        let data = self.data.clone();
        let (port, baud) = (self.port.clone(), self.baud);
        std::thread::spawn(move || {
            let p = match serialport::new(&port, baud).timeout(Duration::from_millis(200)).open() {
                Ok(p) => p,
                Err(e) => {
                    data.lock().unwrap().err = Some(e.to_string());
                    return;
                }
            };
            let mut r = BufReader::new(p);
            let mut line = String::new();
            while run.load(Ordering::Relaxed) {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) => std::thread::sleep(Duration::from_millis(20)),
                    Ok(_) => {
                        if let Some(ma) = parse_current(&line) {
                            let now = Instant::now();
                            let mut d = data.lock().unwrap();
                            if let Some(last) = d.last {
                                d.mah += ma * now.duration_since(last).as_secs_f64() / 3600.0;
                            }
                            d.last = Some(now);
                            d.peak = d.peak.max(ma);
                            d.sum += ma;
                            d.n += 1;
                            d.pts.push_back((now, ma));
                            while d.pts.front().is_some_and(|f| now.duration_since(f.0) > Duration::from_secs(60)) {
                                d.pts.pop_front();
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        data.lock().unwrap().err = Some(e.to_string());
                        break;
                    }
                }
            }
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(t("把 INA219 / INA226 模块接在电池和电路之间，模块的读数用串口发过来（每行一个电流值，带不带单位都行）。"))
                .size(11.5)
                .color(TEXT3()),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ports = crate::serial::scan_ports();
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add(egui::TextEdit::singleline(&mut self.port).hint_text(t("串口")).desired_width(92.0));
            egui::ComboBox::from_id_salt("cv-meter-port").width(18.0).selected_text("").show_ui(ui, |ui| {
                if ports.is_empty() {
                    ui.label(RichText::new(t("没有找到串口")).color(TEXT3()));
                }
                for p in &ports {
                    ui.selectable_value(&mut self.port, p.name.clone(), format!("{}  {}", p.name, p.desc));
                }
            });
            ui.spacing_mut().item_spacing.x = 6.0;
            egui::ComboBox::from_id_salt("cv-meter-baud").width(64.0).selected_text(self.baud.to_string()).show_ui(ui, |ui| {
                for b in [9600, 38400, 57600, 115200, 230400, 460800, 921600] {
                    ui.selectable_value(&mut self.baud, b, b.to_string());
                }
            });
            let on = self.running();
            if ui.add_enabled(!self.port.is_empty() || on, egui::Button::new(if on { t("停止") } else { t("开始") })).clicked() {
                if on {
                    self.stop();
                } else {
                    self.start();
                }
            }
        });
        let d = self.data.lock().unwrap();
        if let Some(e) = &d.err {
            ui.label(RichText::new(e).size(12.0).color(ERR()));
            return;
        }
        if d.n == 0 {
            if self.run.is_some() {
                ui.label(RichText::new(t("等待数据…")).size(12.0).color(TEXT3()));
            }
            return;
        }
        let now_ma = d.pts.back().map_or(0.0, |x| x.1);
        let avg = d.sum / d.n as f64;
        ui.label(
            RichText::new(format!(
                "{} {}   {} {}   {} {}",
                t("现在"),
                fmt_si(now_ma / 1000.0, "A"),
                t("平均"),
                fmt_si(avg / 1000.0, "A"),
                t("峰值"),
                fmt_si(d.peak / 1000.0, "A")
            ))
            .font(FontId::monospace(11.5))
            .color(TEXT()),
        );
        ui.label(RichText::new(format!("{} {:.3} mAh", t("累计用掉"), d.mah)).font(FontId::monospace(11.5)).color(TEXT2()));
        // 最近 60 秒曲线
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 4.0, BG2());
        let maxv = d.pts.iter().map(|x| x.1).fold(0.0, f64::max).max(1e-3);
        let t_end = Instant::now();
        let pts: Vec<egui::Pos2> = d
            .pts
            .iter()
            .map(|(at, v)| {
                let age = t_end.duration_since(*at).as_secs_f32();
                pos2(r.right() - age / 60.0 * r.width(), r.bottom() - 4.0 - (*v / maxv) as f32 * (r.height() - 8.0))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.4, ACCENT())));
        drop(d);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("电池容量")).size(12.0).color(TEXT2()));
            ui.add(egui::TextEdit::singleline(&mut self.cap_mah).desired_width(60.0));
            ui.label(RichText::new("mAh").color(TEXT3()));
            if let Ok(cap) = self.cap_mah.trim().parse::<f64>() {
                ui.label(RichText::new(format!("→ {}", fmt_hours(battery_hours(cap, avg, 0.0)))).size(12.0).color(ACCENT()));
            }
        });
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines() {
        assert_eq!(parse_current("Current:       12.30 mA"), Some(12.3));
        assert_eq!(parse_current("I=0.0123A"), Some(12.3));
        assert_eq!(parse_current("45uA"), Some(0.045));
        assert_eq!(parse_current("Bus Voltage: 4.98 V"), None);
        assert_eq!(parse_current("3.3V,15.2mA,50mW"), Some(15.2));
        assert_eq!(parse_current("15.5"), Some(15.5));
        assert_eq!(parse_current("电流 20 mA"), Some(20.0));
        assert!((battery_hours(2000.0, 10.0, 0.0) - 170.0).abs() < 1e-9);
    }
}

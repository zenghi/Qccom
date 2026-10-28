//! 代码页寄存器预览（v1.8）：打开工程时按工程配置自动找 SVD；悬停 `USART1->CR1 = 0x200C` 里的数值按位域拆开，
//! 悬停 USART_CR1_UE 这类位宏显示是哪个寄存器的哪一位；寄存器面板（改位域算数值）和转储对比。
//! 面板是可以拖走的浮动窗口，从状态行的芯片名、命令面板、日志右键打开，不常驻。

use super::*;
use crate::toolbox::svd::{self, Device, Field, Periph, Reg};
use crate::toolbox::svdfind::{self, Found};

#[derive(Default)]
pub(super) struct RegUi {
    /// 自动找到的芯片和 SVD
    pub auto: Option<Found>,
    rx: Option<Receiver<Option<Found>>>,
    pub win: Option<RegWin>,
}

#[derive(Default)]
pub(super) struct RegWin {
    /// 0 寄存器，1 转储对比
    tab: u8,
    filter: String,
    sel: Option<(String, String)>,
    val: u64,
    val_text: String,
    dump_a: String,
    dump_b: String,
}

impl RegWin {
    fn pick(&mut self, p: &str, r: &str, v: u64) {
        self.sel = Some((p.to_string(), r.to_string()));
        self.val = v;
        self.val_text = format!("0x{v:X}");
        self.tab = 0;
    }
}

/// 位域的位范围文字：13 或 15:12
fn bits(f: &Field) -> String {
    if f.width == 1 {
        f.lsb.to_string()
    } else {
        format!("{}:{}", f.lsb + f.width - 1, f.lsb)
    }
}

/// 赋值语句里数值前面的寄存器：`USART1->CR1 = `、`USART1->CR1 |= (`、`GPIOA->ODR &= ~`
pub(super) fn reg_before(before: &str) -> Option<(String, String)> {
    let b = before.trim_end().trim_end_matches(['(', '~', ' ']);
    let b = b
        .strip_suffix("|=")
        .or_else(|| b.strip_suffix("&="))
        .or_else(|| b.strip_suffix("^="))
        .or_else(|| b.strip_suffix('=').filter(|x| !x.ends_with(['=', '!', '<', '>'])))?;
    let b = b.trim_end();
    let end = b.len();
    let start = b.rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '[' || c == ']')).map_or(0, |p| p + 1);
    let reg = &b[start..end];
    let b = b[..start].trim_end();
    let b = b.strip_suffix("->").or_else(|| b.strip_suffix('.'))?.trim_end();
    let ps = b.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |p| p + 1);
    let periph = &b[ps..];
    (!periph.is_empty() && !reg.is_empty()).then(|| (periph.to_string(), reg.to_string()))
}

/// 悬停卡片里的位域表：值不为 0 的位域醒目
fn fields_grid(ui: &mut Ui, r: &Reg, v: Option<u64>, id: &str) {
    egui::Grid::new(id).num_columns(4).spacing([12.0, 1.0]).show(ui, |ui| {
        for f in r.fields.iter().take(32) {
            let x = v.map(|v| f.get(v));
            let on = x.is_some_and(|x| x != 0);
            ui.label(RichText::new(bits(f)).font(FontId::new(11.0, FontFamily::Monospace)).color(TEXT3()));
            ui.label(
                RichText::new(&f.name)
                    .font(FontId::new(11.5, FontFamily::Monospace))
                    .color(if on || v.is_none() { VIOLET() } else { TEXT3() }),
            );
            match x {
                Some(x) => {
                    let e = f.enum_name(x).map(|e| format!("  {}", e.name)).unwrap_or_default();
                    ui.label(
                        RichText::new(format!("{x}{e}"))
                            .font(FontId::new(11.5, FontFamily::Monospace))
                            .color(if on { ACCENT() } else { TEXT3() }),
                    );
                }
                None => {
                    ui.label("");
                }
            }
            ui.label(RichText::new(truncate(&f.desc, 56)).small().color(TEXT2()));
            ui.end_row();
        }
    });
}

/// 悬停数值：按寄存器拆位域
pub(super) fn value_card(ui: &mut Ui, p: &Periph, r: &Reg, v: u64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}->{}", p.name, r.name)).font(FontId::new(12.5, FontFamily::Monospace)).color(TEXT()).strong());
        ui.label(RichText::new(format!("= 0x{v:X}")).font(FontId::new(12.5, FontFamily::Monospace)).color(ACCENT()));
    });
    ui.add_space(2.0);
    fields_grid(ui, r, Some(v), "svd-val-fields");
}

/// 悬停位宏：哪个寄存器的哪一位
pub(super) fn macro_card(ui: &mut Ui, p: &Periph, r: &Reg, f: &Field) {
    ui.set_max_width(520.0);
    let periph = if p.group.is_empty() { p.name.clone() } else { p.group.clone() };
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{periph} · {}", r.name)).font(FontId::new(12.5, FontFamily::Monospace)).color(TEXT()).strong());
        ui.label(RichText::new(tf("位 {}", &[&bits(f)])).font(FontId::new(12.0, FontFamily::Monospace)).color(ACCENT()));
        ui.label(RichText::new(&f.name).font(FontId::new(12.0, FontFamily::Monospace)).color(VIOLET()));
    });
    if !f.desc.is_empty() {
        ui.label(RichText::new(&f.desc).small().color(TEXT2()));
    }
    ui.label(RichText::new(format!("{}  0x{:X}", t("掩码"), f.mask())).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3()));
    for e in f.enums.iter().take(8) {
        ui.label(
            RichText::new(format!("{} = {}  {}", e.value, e.name, truncate(&e.desc, 40)))
                .font(FontId::new(11.0, FontFamily::Monospace))
                .color(TEXT3()),
        );
    }
}

impl CodeView {
    pub(super) fn regs_start(&mut self) {
        self.regs.auto = None;
        let Some(root) = self.root.clone() else { return };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("svd-find".into())
            .spawn(move || {
                let _ = tx.send(svdfind::find(&root));
            })
            .ok();
        self.regs.rx = Some(rx);
    }

    pub(super) fn regs_poll(&mut self) {
        if let Some(rx) = &self.regs.rx {
            if let Ok(f) = rx.try_recv() {
                self.regs.auto = f;
                self.regs.rx = None;
            }
        }
    }

    /// 自动找到的 SVD（工具箱里没手动指定时 App 用它）
    pub fn svd_auto(&self) -> Option<PathBuf> {
        self.regs.auto.as_ref().and_then(|f| f.svd.clone())
    }

    /// 状态行显示的芯片名
    pub(super) fn chip_label(&self) -> Option<String> {
        let f = self.regs.auto.as_ref();
        match (&self.svd, f) {
            (Some(d), Some(f)) if !f.chip.is_empty() => Some(f.chip.clone()).filter(|_| !d.periphs.is_empty()),
            (Some(d), _) => Some(d.name.clone()),
            (None, Some(f)) if !f.chip.is_empty() => Some(f.chip.clone()),
            _ => None,
        }
    }

    pub(super) fn chip_hint(&self) -> String {
        match (&self.svd, &self.regs.auto) {
            (Some(_), Some(f)) => tf("芯片来自 {} 工程配置，点击打开寄存器面板", &[&f.from]),
            (Some(_), None) => t("点击打开寄存器面板").to_string(),
            (None, Some(f)) => tf("{} 工程里的芯片；没找到 SVD 文件，可以在工具箱「SVD 寄存器」里指定", &[&f.from]),
            _ => String::new(),
        }
    }

    pub fn open_regs(&mut self) {
        if self.svd.is_none() {
            self.toast(t("还没有 SVD：工程里没找到，可以在工具箱「SVD 寄存器」里指定一次"), WARN());
            return;
        }
        self.regs.win.get_or_insert_with(RegWin::default);
    }

    /// 日志里的一行（或一段）：认出寄存器值就在面板里打开
    pub fn decode_regs(&mut self, text: &str) -> bool {
        let Some(dev) = self.svd.clone() else { return false };
        let found = dev.parse_dump(text);
        let w = self.regs.win.get_or_insert_with(RegWin::default);
        match found.as_slice() {
            [] => {
                self.regs.win = None;
                return false;
            }
            [(p, r, v)] => w.pick(p, r, *v),
            _ => {
                w.dump_a = text.to_string();
                w.tab = 1;
            }
        }
        true
    }

    /// 编辑器里悬停寄存器赋值里的数值、或者位宏时的卡片；返回 true 表示已经显示
    pub(super) fn reg_hover(&self, ctx: &egui::Context, d: &Doc, h: &Hover) -> bool {
        let Some(dev) = self.svd.as_ref() else { return false };
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hover-layer"));
        let at = h.rect.left_bottom() + vec2(0.0, 4.0);
        if h.word.starts_with(|c: char| c.is_ascii_digit()) {
            let ls = d.buf.start(d.buf.line_of(h.at));
            let Some((p, r)) = reg_before(&d.buf.text[ls..h.at]) else { return false };
            let Some((p, r)) = dev.find(&p, &r) else { return false };
            let Some(v) = cindex::eval(&h.word).map(|v| v as u64) else { return false };
            egui::show_tooltip_at(ctx, layer, egui::Id::new(("svd-val", h.at)), at, |ui| {
                value_card(ui, p, r, v);
                ui.add_space(2.0);
                ui.label(RichText::new(t("Ctrl+点击 在位域计算器中打开")).small().color(ACCENT().gamma_multiply(0.8)));
            });
            return true;
        }
        if h.word.contains('_') && h.word.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c.is_ascii_lowercase()) {
            if let Some((p, r, f)) = dev.bit_macro(&h.word) {
                egui::show_tooltip_at(ctx, layer, egui::Id::new(("svd-macro", h.at)), at, |ui| macro_card(ui, p, r, f));
                return true;
            }
        }
        false
    }

    /// 寄存器面板（浮动窗口，任何页面都画）
    pub fn reg_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.regs.win.take() else { return };
        let Some(dev) = self.svd.clone() else { return };
        // 当前文件里用到的寄存器
        let used: Vec<(String, String)> = self.tabs.get(self.active).filter(|d| d.lang.is_code()).map(|d| used_regs(&dev, &d.buf.text)).unwrap_or_default();
        let mut open = true;
        let mut copy: Option<String> = None;
        egui::Window::new(format!("{} · {}", t("寄存器"), dev.name))
            .id(egui::Id::new("reg-win"))
            .open(&mut open)
            .default_size(vec2(640.0, 460.0))
            .default_pos(ctx.screen_rect().right_top() + vec2(-680.0, 80.0))
            .collapsible(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (i, l) in [t("寄存器"), t("转储对比")].iter().enumerate() {
                        if ui.selectable_label(w.tab == i as u8, *l).clicked() {
                            w.tab = i as u8;
                        }
                    }
                });
                ui.separator();
                if w.tab == 1 {
                    dump_compare(ui, &dev, &mut w);
                    return;
                }
                egui::SidePanel::left("reg-list").resizable(true).default_width(200.0).show_inside(ui, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut w.filter).hint_text(t("搜索外设或寄存器…")).desired_width(f32::INFINITY));
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        let q = w.filter.to_uppercase();
                        if q.is_empty() {
                            if !used.is_empty() {
                                ui.label(RichText::new(t("当前文件用到的")).small().color(TEXT3()));
                                for (p, r) in &used {
                                    let on = w.sel.as_ref() == Some(&(p.clone(), r.clone()));
                                    if ui.selectable_label(on, RichText::new(format!("{p}->{r}")).font(FontId::new(12.0, FontFamily::Monospace))).clicked() {
                                        let reset = dev.find(p, r).map_or(0, |x| x.1.reset);
                                        w.pick(p, r, reset);
                                    }
                                }
                                ui.add_space(6.0);
                            }
                            ui.label(RichText::new(t("全部外设")).small().color(TEXT3()));
                        }
                        let mut n = 0;
                        for p in &dev.periphs {
                            let pm = p.name.to_uppercase().contains(&q);
                            for r in &p.regs {
                                if !(q.is_empty() || pm || r.name.to_uppercase().contains(&q)) {
                                    continue;
                                }
                                n += 1;
                                if n > 600 {
                                    break;
                                }
                                let on = w.sel.as_ref() == Some(&(p.name.clone(), r.name.clone()));
                                if ui
                                    .selectable_label(on, RichText::new(format!("{}->{}", p.name, r.name)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()))
                                    .clicked()
                                {
                                    w.pick(&p.name, &r.name, r.reset);
                                }
                            }
                        }
                    });
                });
                let Some((pn, rn)) = w.sel.clone() else {
                    ui.centered_and_justified(|ui| ui.label(RichText::new(t("左边选一个寄存器")).color(TEXT3())));
                    return;
                };
                let Some((p, r)) = dev.find(&pn, &rn) else { return };
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}->{}", p.name, r.name)).font(FontId::new(14.0, FontFamily::Monospace)).strong().color(TEXT()));
                        ui.label(RichText::new(format!("0x{:08X}", p.base + r.offset)).font(FontId::new(12.0, FontFamily::Monospace)).color(ACCENT()));
                        ui.label(RichText::new(&r.access).small().color(TEXT3()));
                    });
                    if !r.desc.is_empty() {
                        ui.label(RichText::new(&r.desc).small().color(TEXT2()));
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("值")).color(TEXT2()));
                        let te = ui.add(egui::TextEdit::singleline(&mut w.val_text).font(egui::TextStyle::Monospace).desired_width(120.0));
                        if te.changed() {
                            if let Some(v) = svd::num(&w.val_text) {
                                w.val = v;
                            }
                        }
                        if !te.has_focus() {
                            w.val_text = format!("0x{:X}", w.val);
                        }
                        if ui.small_button(t("复位值")).clicked() {
                            w.val = r.reset;
                            w.val_text = format!("0x{:X}", w.val);
                        }
                        if ui.small_button(t("复制数值")).clicked() {
                            copy = Some(format!("0x{:X}", w.val));
                        }
                        if ui.small_button(t("复制语句")).on_hover_text(format!("{}->{} = 0x{:X};", p.name, r.name, w.val)).clicked() {
                            copy = Some(format!("{}->{} = 0x{:X};", p.name, r.name, w.val));
                        }
                    });
                    ui.add_space(6.0);
                    egui::Grid::new("reg-fields").num_columns(4).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                        for f in &r.fields {
                            let x = f.get(w.val);
                            ui.label(RichText::new(bits(f)).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3()));
                            ui.label(RichText::new(&f.name).font(FontId::new(12.0, FontFamily::Monospace)).color(if x != 0 { VIOLET() } else { TEXT2() }));
                            let mut nx = x;
                            if f.width == 1 {
                                let mut b = x == 1;
                                if ui.checkbox(&mut b, "").changed() {
                                    nx = b as u64;
                                }
                            } else if !f.enums.is_empty() {
                                let cur = f.enum_name(x).map_or(x.to_string(), |e| format!("{x} {}", e.name));
                                egui::ComboBox::from_id_salt(("reg-enum", &f.name)).selected_text(cur).width(140.0).show_ui(ui, |ui| {
                                    for e in &f.enums {
                                        if ui.selectable_label(e.value == x, format!("{} {}", e.value, e.name)).clicked() {
                                            nx = e.value;
                                        }
                                    }
                                });
                            } else {
                                let max = if f.width >= 64 { u64::MAX } else { (1u64 << f.width) - 1 };
                                ui.add(egui::DragValue::new(&mut nx).range(0..=max).hexadecimal(1, false, true));
                            }
                            if nx != x {
                                w.val = f.set(w.val, nx);
                            }
                            ui.label(RichText::new(truncate(&f.desc, 60)).small().color(TEXT2())).on_hover_text(&f.desc);
                            ui.end_row();
                        }
                    });
                });
            });
        if let Some(c) = copy {
            ctx.copy_text(c);
            self.toast(t("已复制"), OK());
        }
        if open {
            self.regs.win = Some(w);
        }
    }
}

/// 文件里出现的 外设->寄存器（去重，按出现顺序，最多 60 个）
fn used_regs(dev: &Device, text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(k) = text[i..].find("->") {
        let at = i + k;
        i = at + 2;
        let ps = text[..at].rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |p| p + 1);
        let re = text[i..].find(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(text.len(), |p| i + p);
        if ps >= at || re <= i || !b[ps].is_ascii_alphabetic() {
            continue;
        }
        if let Some((p, r)) = dev.find(&text[ps..at], &text[i..re]) {
            let key = (p.name.clone(), r.name.clone());
            if !out.contains(&key) {
                out.push(key);
                if out.len() >= 60 {
                    break;
                }
            }
        }
    }
    out
}

/// 两段寄存器转储对比：只列出变了的寄存器和位域
fn dump_compare(ui: &mut Ui, dev: &Device, w: &mut RegWin) {
    ui.label(
        RichText::new(t("粘贴两段寄存器转储（日志里的 CR1=0x200C、0x40013800: 0x200C、USART1->CR1 = … 都认识），列出哪些位变了"))
            .small()
            .color(TEXT3()),
    );
    ui.columns(2, |c| {
        c[0].label(RichText::new("A").strong());
        c[0].add(egui::TextEdit::multiline(&mut w.dump_a).font(egui::TextStyle::Monospace).desired_rows(6).desired_width(f32::INFINITY));
        c[1].label(RichText::new("B").strong());
        c[1].add(egui::TextEdit::multiline(&mut w.dump_b).font(egui::TextStyle::Monospace).desired_rows(6).desired_width(f32::INFINITY));
    });
    let a = dev.parse_dump(&w.dump_a);
    let b = dev.parse_dump(&w.dump_b);
    ui.add_space(6.0);
    if b.is_empty() {
        // 只有一段：直接解码
        for (p, r, v) in &a {
            if let Some((p, r)) = dev.find(p, r) {
                ui.label(RichText::new(format!("{}->{} = 0x{v:X}", p.name, r.name)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT()));
                fields_grid(ui, r, Some(*v), &format!("dump-{}-{}", p.name, r.name));
                ui.add_space(4.0);
            }
        }
        return;
    }
    let mut any = false;
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        for (p, r, vb) in &b {
            let Some(va) = a.iter().find(|x| &x.0 == p && &x.1 == r).map(|x| x.2) else { continue };
            if va == *vb {
                continue;
            }
            any = true;
            let Some((pp, rr)) = dev.find(p, r) else { continue };
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}->{}", pp.name, rr.name)).font(FontId::new(12.5, FontFamily::Monospace)).strong().color(TEXT()));
                ui.label(RichText::new(format!("0x{va:X} → 0x{vb:X}")).font(FontId::new(12.0, FontFamily::Monospace)).color(ACCENT()));
            });
            for f in &rr.fields {
                let (x, y) = (f.get(va), f.get(*vb));
                if x != y {
                    ui.label(
                        RichText::new(format!("    {:<8} {:<10} {x} → {y}", bits(f), f.name))
                            .font(FontId::new(11.5, FontFamily::Monospace))
                            .color(WARN()),
                    );
                }
            }
            ui.add_space(4.0);
        }
    });
    if !any {
        ui.label(RichText::new(t("两段里同名寄存器的值都一样")).color(OK()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reg_context() {
        assert_eq!(reg_before("  USART1->CR1 = "), Some(("USART1".into(), "CR1".into())));
        assert_eq!(reg_before("GPIOA->ODR |= ("), Some(("GPIOA".into(), "ODR".into())));
        assert_eq!(reg_before("RCC->AFR[1] &= ~"), Some(("RCC".into(), "AFR[1]".into())));
        assert_eq!(reg_before("if (x == "), None);
        assert_eq!(reg_before("x = "), None);
    }
}

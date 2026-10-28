//! 代码页里的特殊查看器：ELF 段/符号、map 文件内存占用、HEX/BIN 十六进制查看和比较、文本差异。
//! 打开这些文件时代替文本编辑器显示（map / hex 可以切回原文）。

pub mod parse;

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Rect, RichText, Sense, Ui};

use crate::app::human_bytes;
use crate::highlight;
use crate::i18n::{t, tf};
use crate::theme::*;
use parse::*;

pub enum BinEvt {
    /// 跳到这个符号的定义
    Symbol(String),
    Toast(String, Color32),
}

pub enum Special {
    Hex(Box<HexView>),
    Elf(Box<ElfView>),
    Map(Box<MapView>),
}

const BIN_EXT: &[&str] = &["bin", "hex", "ihex", "ihx", "s19", "s28", "s37", "srec", "mot", "sx", "img", "dfu", "fw"];
const ELF_EXT: &[&str] = &["elf", "axf", "out", "o", "so", "ko", "a"];

impl Special {
    /// 按扩展名和内容决定要不要用特殊查看器；text 是能按文本解码时的内容
    pub fn detect(path: &Path, bytes: &[u8], text: Option<&str>) -> Option<Special> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if is_elf(bytes) {
            return match parse_elf(bytes) {
                Ok(e) => Some(Special::Elf(Box::new(ElfView::new(e)))),
                Err(_) => None,
            };
        }
        if ext == "map" {
            if let Some(m) = text.and_then(parse_map) {
                return Some(Special::Map(Box::new(MapView::new(m))));
            }
            return None;
        }
        if BIN_EXT.contains(&ext.as_str()) || (text.is_none() && !ELF_EXT.contains(&ext.as_str())) {
            return load_image(path, bytes).filter(|i| i.size() > 0 || text.is_none()).map(|i| Special::Hex(Box::new(HexView::new(i))));
        }
        None
    }

    /// 是否也能以文本方式查看（map、hex 文本）
    pub fn label(&self) -> &'static str {
        match self {
            Special::Hex(_) => t("十六进制"),
            Special::Elf(_) => "ELF",
            Special::Map(_) => t("内存占用"),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<BinEvt> {
        match self {
            Special::Hex(v) => v.ui(ui),
            Special::Elf(v) => v.ui(ui),
            Special::Map(v) => v.ui(ui),
        }
    }
}

// ================================================================== 通用表格

struct Col {
    title: &'static str,
    /// 0 = 占满剩余宽度
    w: f32,
    right: bool,
}

struct Row {
    cells: Vec<String>,
    nums: Vec<Option<u64>>,
    color: Option<Color32>,
    /// 点击跳转用的符号名
    sym: Option<String>,
}

struct Table {
    cols: Vec<Col>,
    rows: Vec<Row>,
    order: Vec<usize>,
    sort: usize,
    desc: bool,
    filter: String,
    /// 画占比条的列
    bar: Option<usize>,
    stale: bool,
}

impl Table {
    fn new(cols: Vec<Col>, rows: Vec<Row>, sort: usize, bar: Option<usize>) -> Table {
        Table {
            cols,
            rows,
            order: vec![],
            sort,
            desc: true,
            filter: String::new(),
            bar,
            stale: true,
        }
    }

    fn refresh(&mut self) {
        let f = self.filter.to_lowercase();
        self.order = (0..self.rows.len())
            .filter(|&i| f.is_empty() || self.rows[i].cells.iter().any(|c| c.to_lowercase().contains(&f)))
            .collect();
        let (s, rows) = (self.sort, &self.rows);
        self.order.sort_by(|&a, &b| {
            let o = match (rows[a].nums.get(s).copied().flatten(), rows[b].nums.get(s).copied().flatten()) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => rows[a].cells[s].to_lowercase().cmp(&rows[b].cells[s].to_lowercase()),
            };
            if self.desc {
                o.reverse()
            } else {
                o
            }
        });
        self.stale = false;
    }

    /// 返回被点击的行
    fn ui(&mut self, ui: &mut Ui, show_filter: bool) -> Option<usize> {
        if show_filter {
            ui.horizontal(|ui| {
                let r = ui.add_sized(vec2(260.0, 24.0), egui::TextEdit::singleline(&mut self.filter).hint_text(t("筛选…")).margin(vec2(6.0, 3.0)));
                if r.changed() {
                    self.stale = true;
                }
                ui.label(RichText::new(tf("{} 项", &[&self.order.len()])).small().color(TEXT3()));
            });
            ui.add_space(4.0);
        }
        if self.stale {
            self.refresh();
        }
        let full_w = ui.available_width();
        let fixed: f32 = self.cols.iter().map(|c| c.w).sum();
        let flex = (full_w - fixed - 16.0).max(120.0);
        let widths: Vec<f32> = self.cols.iter().map(|c| if c.w == 0.0 { flex } else { c.w }).collect();
        // 表头
        let (hr, _) = ui.allocate_exact_size(vec2(full_w, 24.0), Sense::hover());
        let p = ui.painter();
        p.hline(hr.x_range(), hr.bottom() - 0.5, st(1.0, LINE()));
        let mut x = hr.left() + 8.0;
        for (i, c) in self.cols.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(x, hr.top()), vec2(widths[i], hr.height()));
            let resp = ui.interact(cr, ui.id().with(("th", i)), Sense::click());
            let col = if resp.hovered() || self.sort == i { TEXT() } else { TEXT3() };
            let arrow = if self.sort == i {
                if self.desc {
                    " ↓"
                } else {
                    " ↑"
                }
            } else {
                ""
            };
            let (pos, al) = if c.right {
                (pos2(cr.right() - 10.0, cr.center().y), Align2::RIGHT_CENTER)
            } else {
                (pos2(cr.left(), cr.center().y), Align2::LEFT_CENTER)
            };
            ui.painter().text(pos, al, format!("{}{}", t(c.title), arrow), FontId::new(11.5, bold()), col);
            if resp.clicked() {
                if self.sort == i {
                    self.desc = !self.desc;
                } else {
                    self.sort = i;
                    self.desc = self.rows.first().and_then(|r| r.nums.get(i).copied().flatten()).is_some();
                }
                self.stale = true;
            }
            x += widths[i];
        }
        let max_bar = self.bar.map(|b| self.rows.iter().filter_map(|r| r.nums.get(b).copied().flatten()).max().unwrap_or(1).max(1));
        let mut clicked = None;
        let row_h = 22.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, self.order.len(), |ui, range| {
            for k in range {
                let ri = self.order[k];
                let row = &self.rows[ri];
                let (r, resp) = ui.allocate_exact_size(vec2(full_w, row_h), Sense::click());
                let p = ui.painter_at(r);
                if resp.hovered() {
                    p.rect_filled(r, 0.0, tint(8));
                }
                let mut x = r.left() + 8.0;
                for (i, c) in self.cols.iter().enumerate() {
                    let cr = Rect::from_min_size(pos2(x, r.top()), vec2(widths[i], r.height()));
                    if Some(i) == self.bar {
                        if let (Some(v), Some(m)) = (row.nums.get(i).copied().flatten(), max_bar) {
                            let w = (cr.width() - 12.0) * (v as f32 / m as f32);
                            let br = Rect::from_min_size(pos2(cr.right() - 6.0 - w, cr.top() + 4.0), vec2(w, cr.height() - 8.0));
                            p.rect_filled(br, 2.0, ACCENT().gamma_multiply(0.16));
                        }
                    }
                    let mono = row.nums.get(i).copied().flatten().is_some() || i > 0;
                    let font = if mono { FontId::new(12.0, FontFamily::Monospace) } else { FontId::proportional(12.5) };
                    let color = if i == 0 { row.color.unwrap_or(TEXT()) } else { TEXT2() };
                    let txt = &row.cells[i];
                    let g = p.layout_no_wrap(txt.clone(), font, color);
                    let gx = if c.right { cr.right() - 10.0 - g.size().x } else { cr.left() };
                    p.with_clip_rect(cr.shrink2(vec2(0.0, 0.0))).galley(pos2(gx, cr.center().y - g.size().y / 2.0), g, color);
                    x += widths[i];
                }
                if resp.clicked() {
                    clicked = Some(ri);
                }
                if row.sym.is_some() {
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            }
        });
        clicked
    }
}

fn hex(v: u64) -> String {
    if v > 0xFFFF_FFFF {
        format!("0x{v:016X}")
    } else {
        format!("0x{v:08X}")
    }
}

fn fmt_f(f: f64) -> String {
    if f == 0.0 || (1e-4..1e9).contains(&f.abs()) {
        format!("{f}")
    } else {
        format!("{f:e}")
    }
}

fn size_str(v: u64) -> String {
    if v < 1024 {
        format!("{v}")
    } else {
        format!("{v}  ({})", human_bytes(v))
    }
}

fn tabs(ui: &mut Ui, cur: &mut usize, names: &[String]) {
    ui.horizontal(|ui| {
        for (i, n) in names.iter().enumerate() {
            let on = *cur == i;
            let r = ui.add(
                egui::Button::new(RichText::new(n).color(if on { TEXT() } else { TEXT2() }))
                    .fill(if on { BG3() } else { Color32::TRANSPARENT })
                    .corner_radius(R_WIDGET),
            );
            if r.clicked() {
                *cur = i;
            }
        }
    });
}

/// 一条占用条：名字、已用/总量、百分比
fn usage_bar(ui: &mut Ui, name: &str, used: u64, total: u64, sub: &str) {
    let w = ui.available_width().min(760.0);
    let (r, _) = ui.allocate_exact_size(vec2(w, 40.0), Sense::hover());
    let p = ui.painter();
    let pct = if total > 0 { used as f32 / total as f32 } else { 0.0 };
    let c = if pct > 0.9 {
        ERR()
    } else if pct > 0.75 {
        WARN()
    } else {
        ACCENT()
    };
    p.text(r.left_top() + vec2(0.0, 2.0), Align2::LEFT_TOP, name, FontId::new(13.0, bold()), TEXT());
    let right = if total > 0 {
        format!("{} / {}   {:.1}%", human_bytes(used), human_bytes(total), pct * 100.0)
    } else {
        human_bytes(used)
    };
    p.text(r.right_top() + vec2(0.0, 2.0), Align2::RIGHT_TOP, right, FontId::new(12.0, FontFamily::Monospace), TEXT2());
    if !sub.is_empty() {
        let g = p.layout_no_wrap(name.to_string(), FontId::new(13.0, bold()), TEXT());
        p.text(r.left_top() + vec2(g.size().x + 10.0, 3.5), Align2::LEFT_TOP, sub, FontId::proportional(11.0), TEXT3());
    }
    let bar = Rect::from_min_size(pos2(r.left(), r.bottom() - 12.0), vec2(w, 7.0));
    p.rect_filled(bar, 3.5, BG3());
    if total > 0 {
        p.rect_filled(Rect::from_min_size(bar.min, vec2(w * pct.min(1.0), bar.height())), 3.5, c);
    }
}

// ================================================================== ELF

pub struct ElfView {
    elf: parse::Elf,
    tab: usize,
    secs: Table,
    syms: Table,
    segs: Table,
}

impl ElfView {
    fn new(elf: parse::Elf) -> ElfView {
        let secs = elf
            .sections
            .iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| {
                let mut fl = String::new();
                if s.flags & SHF_ALLOC != 0 {
                    fl.push('A');
                }
                if s.flags & SHF_WRITE != 0 {
                    fl.push('W');
                }
                if s.flags & SHF_EXEC != 0 {
                    fl.push('X');
                }
                let kind = if s.flags & SHF_ALLOC == 0 {
                    "—"
                } else if s.typ == SHT_NOBITS {
                    "bss"
                } else if s.flags & SHF_WRITE != 0 {
                    "data"
                } else {
                    "text"
                };
                Row {
                    cells: vec![s.name.clone(), hex(s.addr), size_str(s.size), fl, kind.into()],
                    nums: vec![None, Some(s.addr), Some(s.size), None, None],
                    color: Some(if s.flags & SHF_ALLOC != 0 { TEXT() } else { TEXT3() }),
                    sym: None,
                }
            })
            .collect();
        let syms = elf
            .syms
            .iter()
            .map(|s| Row {
                cells: vec![
                    s.name.clone(),
                    hex(s.value),
                    size_str(s.size),
                    if s.kind == 2 { "FUNC".into() } else { "OBJECT".into() },
                    s.sec.clone(),
                    if s.global { "global".into() } else { "local".into() },
                ],
                nums: vec![None, Some(s.value), Some(s.size), None, None, None],
                color: Some(if s.kind == 2 { highlight::pal::FUNC() } else { TEXT() }),
                sym: Some(s.name.clone()),
            })
            .collect();
        let segs = elf
            .segments
            .iter()
            .map(|s| {
                let ty = match s.typ {
                    1 => "LOAD".to_string(),
                    2 => "DYNAMIC".into(),
                    3 => "INTERP".into(),
                    4 => "NOTE".into(),
                    6 => "PHDR".into(),
                    7 => "TLS".into(),
                    0x6474e551 => "GNU_STACK".into(),
                    0x6474e552 => "GNU_RELRO".into(),
                    0x70000001 => "ARM_EXIDX".into(),
                    x => format!("0x{x:X}"),
                };
                let fl = format!(
                    "{}{}{}",
                    if s.flags & 4 != 0 { 'R' } else { '-' },
                    if s.flags & 2 != 0 { 'W' } else { '-' },
                    if s.flags & 1 != 0 { 'X' } else { '-' }
                );
                Row {
                    cells: vec![ty, hex(s.vaddr), hex(s.paddr), size_str(s.filesz), size_str(s.memsz), fl],
                    nums: vec![None, Some(s.vaddr), Some(s.paddr), Some(s.filesz), Some(s.memsz), None],
                    color: None,
                    sym: None,
                }
            })
            .collect();
        let c = |title, w, right| Col { title, w, right };
        let mut v = ElfView {
            tab: 0,
            secs: Table::new(
                vec![
                    c("名称", 0.0, false),
                    c("内存地址", 150.0, false),
                    c("大小", 170.0, true),
                    c("标志", 70.0, false),
                    c("类别", 70.0, false),
                ],
                secs,
                1,
                Some(2),
            ),
            syms: Table::new(
                vec![
                    c("名称", 0.0, false),
                    c("内存地址", 150.0, false),
                    c("大小", 150.0, true),
                    c("类型", 80.0, false),
                    c("所在段", 140.0, false),
                    c("范围", 70.0, false),
                ],
                syms,
                2,
                Some(2),
            ),
            segs: Table::new(
                vec![
                    c("类型", 0.0, false),
                    c("虚拟地址", 150.0, false),
                    c("物理地址", 150.0, false),
                    c("文件大小", 160.0, true),
                    c("内存大小", 160.0, true),
                    c("权限", 60.0, false),
                ],
                segs,
                1,
                None,
            ),
            elf,
        };
        // 段和程序头按地址从小到大
        v.secs.desc = false;
        v.segs.desc = false;
        v
    }

    fn ui(&mut self, ui: &mut Ui) -> Option<BinEvt> {
        let e = &self.elf;
        let (text, data, bss) = e.sizes();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "ELF{} · {} · {}",
                    if e.class64 { 64 } else { 32 },
                    machine_name(e.machine),
                    if e.le { "little-endian" } else { "big-endian" }
                ))
                .color(TEXT2()),
            );
            ui.label(RichText::new(tf("入口 {}", &[&hex(e.entry)])).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT3()));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (k, v, tip) in [
                ("text", text, t("代码和只读数据")),
                ("data", data, t("已初始化的全局变量（同时占 Flash 和 RAM）")),
                ("bss", bss, t("清零的全局变量（只占 RAM）")),
                ("Flash ≈", text + data, "text + data"),
                ("RAM ≈", data + bss, "data + bss"),
            ] {
                egui::Frame::new()
                    .fill(BG2())
                    .corner_radius(R_CARD)
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(k).small().color(TEXT3()));
                            ui.label(RichText::new(human_bytes(v)).size(16.0).color(TEXT()).strong());
                            ui.label(RichText::new(v.to_string()).font(FontId::new(11.0, FontFamily::Monospace)).color(TEXT3()));
                        });
                    })
                    .response
                    .on_hover_text(tip);
            }
        });
        ui.add_space(10.0);
        let names = [
            tf("段 ({})", &[&self.secs.rows.len()]),
            tf("符号 ({})", &[&self.syms.rows.len()]),
            tf("程序头 ({})", &[&self.segs.rows.len()]),
        ];
        tabs(ui, &mut self.tab, &names);
        ui.add_space(6.0);
        match self.tab {
            0 => {
                self.secs.ui(ui, false);
            }
            1 => {
                if let Some(i) = self.syms.ui(ui, true) {
                    return self.syms.rows[i].sym.clone().map(BinEvt::Symbol);
                }
            }
            _ => {
                self.segs.ui(ui, false);
            }
        }
        None
    }
}

// ================================================================== map

pub struct MapView {
    map: parse::MapInfo,
    tab: usize,
    syms: Table,
    objs: Table,
    outs: Table,
}

impl MapView {
    fn new(map: parse::MapInfo) -> MapView {
        let c = |title, w, right| Col { title, w, right };
        let syms = map
            .ins
            .iter()
            .map(|i| {
                let o = &map.outs[i.out];
                let (rom, ram) = map.out_cost(o);
                let place = match (rom > 0, ram > 0) {
                    (true, true) => "Flash+RAM",
                    (true, false) => "Flash",
                    (false, true) => "RAM",
                    _ => "—",
                };
                Row {
                    cells: vec![sym_name(&i.name).to_string(), size_str(i.size), hex(i.addr), o.name.clone(), place.into(), i.obj.clone()],
                    nums: vec![None, Some(i.size), Some(i.addr), None, None, None],
                    color: Some(if o.name.starts_with(".text") { highlight::pal::FUNC() } else { TEXT() }),
                    sym: Some(sym_name(&i.name).to_string()),
                }
            })
            .collect();
        let objs = map
            .by_object()
            .into_iter()
            .map(|(name, rom, ram)| Row {
                cells: vec![name, size_str(rom), size_str(ram), size_str(rom + ram)],
                nums: vec![None, Some(rom), Some(ram), Some(rom + ram)],
                color: None,
                sym: None,
            })
            .collect();
        let outs = map
            .outs
            .iter()
            .filter(|o| o.size > 0 && !o.name.starts_with(".debug") && !o.name.starts_with(".comment") && !o.name.starts_with(".ARM.attributes"))
            .map(|o| {
                let reg = map
                    .regions
                    .iter()
                    .find(|r| o.addr >= r.origin && o.addr < r.origin.saturating_add(r.len))
                    .map_or("—".to_string(), |r| r.name.clone());
                Row {
                    cells: vec![o.name.clone(), hex(o.addr), size_str(o.size), o.load.filter(|l| *l != o.addr).map(hex).unwrap_or_default(), reg],
                    nums: vec![None, Some(o.addr), Some(o.size), o.load, None],
                    color: None,
                    sym: None,
                }
            })
            .collect();
        MapView {
            tab: 0,
            syms: Table::new(
                vec![
                    c("符号", 0.0, false),
                    c("大小", 150.0, true),
                    c("内存地址", 120.0, false),
                    c("输出段", 110.0, false),
                    c("位置", 90.0, false),
                    c("目标文件", 180.0, false),
                ],
                syms,
                1,
                Some(1),
            ),
            objs: Table::new(
                vec![c("目标文件", 0.0, false), c("Flash", 160.0, true), c("RAM", 160.0, true), c("合计", 160.0, true)],
                objs,
                3,
                Some(3),
            ),
            outs: Table::new(
                vec![
                    c("输出段", 0.0, false),
                    c("内存地址", 150.0, false),
                    c("大小", 170.0, true),
                    c("加载地址", 150.0, false),
                    c("区域", 100.0, false),
                ],
                outs,
                1,
                Some(2),
            ),
            map,
        }
    }

    fn ui(&mut self, ui: &mut Ui) -> Option<BinEvt> {
        ui.add_space(12.0);
        if self.map.regions.is_empty() {
            ui.label(RichText::new(t("没有找到内存区域（Memory Configuration）")).color(TEXT3()));
        }
        for r in &self.map.regions {
            let sub = format!("{}  {}", hex(r.origin), if r.writable { "RAM" } else { "ROM" });
            usage_bar(ui, &r.name, r.used, r.len, &sub);
            ui.add_space(4.0);
        }
        ui.add_space(8.0);
        let names = [
            tf("符号 ({})", &[&self.syms.rows.len()]),
            tf("目标文件 ({})", &[&self.objs.rows.len()]),
            tf("输出段 ({})", &[&self.outs.rows.len()]),
        ];
        tabs(ui, &mut self.tab, &names);
        ui.add_space(6.0);
        match self.tab {
            0 => {
                if let Some(i) = self.syms.ui(ui, true) {
                    return self.syms.rows[i].sym.clone().map(BinEvt::Symbol);
                }
            }
            1 => {
                self.objs.ui(ui, true);
            }
            _ => {
                self.outs.ui(ui, false);
            }
        }
        None
    }
}

// ================================================================== HEX / BIN

struct Cmp {
    path: PathBuf,
    img: Image,
    rows: Vec<Option<u64>>,
    diff_rows: Vec<usize>,
    diff_bytes: u64,
    cur: Option<usize>,
}

pub struct HexView {
    img: Image,
    rows: Vec<Option<u64>>,
    crc: u32,
    /// 选中的地址范围（含两端）
    sel: Option<(u64, u64)>,
    goto: String,
    scroll_to: Option<usize>,
    cmp: Option<Cmp>,
}

impl HexView {
    fn new(img: Image) -> HexView {
        HexView {
            rows: img.rows(),
            crc: crc32(&img.segs),
            img,
            sel: None,
            goto: String::new(),
            scroll_to: None,
            cmp: None,
        }
    }

    pub fn compare_with(&mut self, path: &Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let img = load_image(path, &bytes).ok_or_else(|| t("无法解析这个文件").to_string())?;
        // 两边的行合并
        let mut all: Vec<u64> = self.rows.iter().chain(img.rows().iter()).flatten().copied().collect();
        all.sort_unstable();
        all.dedup();
        let mut rows = vec![];
        for (i, r) in all.iter().enumerate() {
            if i > 0 && *r != all[i - 1] + 16 {
                rows.push(None);
            }
            rows.push(Some(*r));
        }
        let (mut diff_rows, mut diff_bytes) = (vec![], 0u64);
        for (i, r) in rows.iter().enumerate() {
            let Some(r) = r else { continue };
            let n = (0..16).filter(|k| self.img.get(r + k) != img.get(r + k)).count();
            if n > 0 {
                diff_rows.push(i);
                diff_bytes += n as u64;
            }
        }
        self.cmp = Some(Cmp {
            path: path.to_path_buf(),
            img,
            rows,
            diff_rows,
            diff_bytes,
            cur: None,
        });
        self.step_diff(true);
        Ok(())
    }

    fn step_diff(&mut self, next: bool) {
        let Some(c) = self.cmp.as_mut() else { return };
        if c.diff_rows.is_empty() {
            return;
        }
        let n = c.diff_rows.len();
        let i = match (c.cur, next) {
            (None, _) => 0,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        c.cur = Some(i);
        self.scroll_to = Some(c.diff_rows[i]);
    }

    fn export_bin(&self) -> Result<Option<PathBuf>, String> {
        let Some(p) = rfd::FileDialog::new().set_title(t("导出为 BIN")).add_filter("BIN", &["bin"]).set_file_name("firmware.bin").save_file() else {
            return Ok(None);
        };
        let (lo, hi) = self.img.range();
        if hi - lo > 256 * 1024 * 1024 {
            return Err(t("地址范围太大（超过 256 MB），无法导出").into());
        }
        let mut out = vec![0xFFu8; (hi - lo) as usize];
        for (a, d) in &self.img.segs {
            let o = (a - lo) as usize;
            out[o..o + d.len()].copy_from_slice(d);
        }
        std::fs::write(&p, out).map_err(|e| e.to_string())?;
        Ok(Some(p))
    }

    fn ui(&mut self, ui: &mut Ui) -> Option<BinEvt> {
        let mut evt = None;
        let (lo, hi) = self.img.range();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let mut info = format!("{} · {} · {}–{}", self.img.kind, human_bytes(self.img.size()), hex(lo), hex(hi.saturating_sub(1)));
            if self.img.segs.len() > 1 {
                info += &format!(" · {}", tf("{} 段", &[&self.img.segs.len()]));
            }
            info += &format!(" · CRC32 {:08X}", self.crc);
            ui.label(RichText::new(info).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
            if let Some(s) = self.img.start {
                ui.label(RichText::new(tf("入口 {}", &[&hex(s)])).small().color(TEXT3()));
            }
            if self.img.errors > 0 {
                ui.label(RichText::new(tf("{} 行校验错误", &[&self.img.errors])).small().color(ERR()));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.goto)
                    .hint_text(t("跳到地址 0x…"))
                    .desired_width(150.0)
                    .font(FontId::new(12.5, FontFamily::Monospace)),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let s = self.goto.trim().trim_start_matches("0x").trim_start_matches("0X");
                if let Ok(a) = u64::from_str_radix(s, 16) {
                    let rows = self.cmp.as_ref().map_or(&self.rows, |c| &c.rows);
                    let i = rows.partition_point(|r| r.is_none_or(|r| r + 16 <= a));
                    self.scroll_to = Some(i.min(rows.len().saturating_sub(1)));
                    self.sel = Some((a, a));
                }
            }
            ui.add_space(8.0);
            if self.cmp.is_none() {
                if ui.button(t("与其他文件比较…")).clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_title(t("选择要比较的文件")).pick_file() {
                        if let Err(e) = self.compare_with(&p) {
                            evt = Some(BinEvt::Toast(e, ERR()));
                        }
                    }
                }
                if self.img.kind != "BIN" && ui.button(t("导出为 BIN…")).clicked() {
                    match self.export_bin() {
                        Ok(Some(p)) => evt = Some(BinEvt::Toast(tf("已导出到 {}", &[&p.display()]), OK())),
                        Ok(None) => {}
                        Err(e) => evt = Some(BinEvt::Toast(e, ERR())),
                    }
                }
            } else {
                let c = self.cmp.as_ref().unwrap();
                let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let (msg, col) = if c.diff_bytes == 0 {
                    (t("内容完全相同").to_string(), OK())
                } else {
                    (tf("{} 字节不同，{} 处", &[&c.diff_bytes, &c.diff_rows.len()]), WARN())
                };
                ui.label(RichText::new(format!("↔ {name}")).color(TEXT()));
                ui.label(RichText::new(msg).color(col));
                let has = c.diff_bytes > 0;
                if ui.add_enabled(has, egui::Button::new("↑")).on_hover_text(t("上一处不同")).clicked() {
                    self.step_diff(false);
                }
                if ui.add_enabled(has, egui::Button::new("↓")).on_hover_text(t("下一处不同")).clicked() {
                    self.step_diff(true);
                }
                if ui.button(t("结束比较")).clicked() {
                    self.cmp = None;
                }
            }
        });
        ui.add_space(6.0);
        let inspector_w = 240.0;
        let avail = ui.available_rect_before_wrap();
        let grid = Rect::from_min_max(avail.min, pos2(avail.right() - inspector_w - 8.0, avail.bottom()));
        let insp = Rect::from_min_max(pos2(grid.right() + 8.0, avail.top()), avail.max);
        ui.allocate_rect(avail, Sense::hover());
        let mut gui = ui.new_child(egui::UiBuilder::new().max_rect(grid));
        self.grid(&mut gui);
        let mut iui = ui.new_child(egui::UiBuilder::new().max_rect(insp));
        self.inspector(&mut iui);
        evt
    }

    fn grid(&mut self, ui: &mut Ui) {
        let mono = FontId::new(12.5, FontFamily::Monospace);
        let cw = ui.fonts(|f| f.glyph_width(&mono, '0'));
        let row_h = 20.0;
        let wide = self.img.range().1 > 0xFFFF_FFFF || self.cmp.as_ref().is_some_and(|c| c.img.range().1 > 0xFFFF_FFFF);
        let addr_w = cw * if wide { 17.0 } else { 9.0 } + 14.0;
        let bw = cw * 2.0 + 7.0;
        let half_gap = 8.0;
        let hex_w = bw * 16.0 + half_gap;
        let n_rows = self.cmp.as_ref().map_or(self.rows.len(), |c| c.rows.len());
        let mut sa = egui::ScrollArea::both().auto_shrink([false, false]).id_salt("hex-grid");
        if let Some(i) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset((i as f32 * row_h - 120.0).max(0.0));
        }
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let shift = ui.input(|i| i.modifiers.shift);
        let mut new_sel = None;
        // 表头：列号
        let head = |p: &egui::Painter, x0: f32, y: f32| {
            for k in 0..16u64 {
                let x = x0 + k as f32 * bw + if k >= 8 { half_gap } else { 0.0 };
                p.text(pos2(x + bw / 2.0, y), Align2::CENTER_CENTER, format!("{k:02X}"), FontId::new(10.5, FontFamily::Monospace), TEXT3());
            }
        };
        sa.show_rows(ui, row_h, n_rows + 1, |ui, range| {
            for ri in range {
                let total_w = addr_w + hex_w + if self.cmp.is_some() { 24.0 + hex_w } else { 16.0 + cw * 16.0 + 8.0 };
                let (r, resp) = ui.allocate_exact_size(vec2(total_w, row_h), Sense::click_and_drag());
                let p = ui.painter_at(r);
                let x0 = r.left() + addr_w;
                let x1 = x0 + hex_w + 24.0;
                if ri == 0 {
                    head(&p, x0, r.center().y);
                    if self.cmp.is_some() {
                        head(&p, x1, r.center().y);
                    }
                    continue;
                }
                let rows = self.cmp.as_ref().map_or(&self.rows, |c| &c.rows);
                let Some(base) = rows[ri - 1] else {
                    p.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, "⋯", mono.clone(), TEXT3());
                    p.hline(pos2(x0, 0.0).x..=r.right() - 8.0, r.center().y, st(1.0, LINE()));
                    continue;
                };
                let is_cur = self.cmp.as_ref().and_then(|c| c.cur.map(|k| c.diff_rows[k] == ri - 1)).unwrap_or(false);
                if is_cur {
                    p.rect_filled(r, 0.0, WARN().gamma_multiply(0.08));
                }
                let addr = if wide { format!("{base:016X}") } else { format!("{base:08X}") };
                p.text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, addr, mono.clone(), TEXT3());
                let byte_x = |x: f32, k: u64| x + k as f32 * bw + if k >= 8 { half_gap } else { 0.0 };
                let in_sel = |a: u64| self.sel.is_some_and(|(s, e)| a >= s.min(e) && a <= s.max(e));
                let draw = |p: &egui::Painter, x: f32, img: &Image, other: Option<&Image>| {
                    for k in 0..16u64 {
                        let a = base + k;
                        let bx = byte_x(x, k);
                        let cell = Rect::from_min_size(pos2(bx, r.top() + 1.0), vec2(bw - 2.0, row_h - 2.0));
                        let v = img.get(a);
                        let differs = other.is_some_and(|o| o.get(a) != v);
                        if differs {
                            p.rect_filled(cell, 3.0, ERR().gamma_multiply(0.22));
                        }
                        if in_sel(a) {
                            p.rect_filled(cell, 3.0, ACCENT().gamma_multiply(0.3));
                        }
                        let (s, c) = match v {
                            None => ("··".to_string(), TEXT3().gamma_multiply(0.6)),
                            Some(0) => ("00".into(), TEXT3()),
                            Some(0xFF) => ("FF".into(), TEXT3()),
                            Some(b) if (0x20..0x7f).contains(&b) => (format!("{b:02X}"), TEXT()),
                            Some(b) => (format!("{b:02X}"), highlight::pal::NUM()),
                        };
                        p.text(cell.center(), Align2::CENTER_CENTER, s, mono.clone(), if differs { ERR() } else { c });
                    }
                };
                match &self.cmp {
                    Some(c) => {
                        draw(&p, x0, &self.img, Some(&c.img));
                        p.vline(x1 - 12.0, r.y_range(), st(1.0, LINE()));
                        draw(&p, x1, &c.img, Some(&self.img));
                    }
                    None => {
                        draw(&p, x0, &self.img, None);
                        let ax = x0 + hex_w + 16.0;
                        for k in 0..16u64 {
                            let a = base + k;
                            let (ch, c) = match self.img.get(a) {
                                Some(b) if (0x20..0x7f).contains(&b) => (b as char, TEXT2()),
                                Some(_) => ('.', TEXT3()),
                                None => (' ', TEXT3()),
                            };
                            let cx = ax + k as f32 * cw;
                            if in_sel(a) {
                                p.rect_filled(Rect::from_min_size(pos2(cx, r.top() + 2.0), vec2(cw, row_h - 4.0)), 0.0, ACCENT().gamma_multiply(0.3));
                            }
                            p.text(pos2(cx, r.center().y), Align2::LEFT_CENTER, ch.to_string(), mono.clone(), c);
                        }
                    }
                }
                // 鼠标点选 / 拖选
                if let Some(pp) = resp.interact_pointer_pos().or_else(|| resp.hover_pos().filter(|_| pointer_down)) {
                    let rel = |x0: f32| -> Option<u64> {
                        let dx = pp.x - x0;
                        if dx < 0.0 || dx > hex_w {
                            return None;
                        }
                        let k = if dx >= 8.0 * bw + half_gap { ((dx - half_gap) / bw) as u64 } else { (dx / bw).min(7.99) as u64 };
                        Some(base + k.min(15))
                    };
                    let hit = rel(x0).or_else(|| self.cmp.as_ref().and_then(|_| rel(x1)));
                    if let Some(a) = hit {
                        if resp.clicked() || resp.drag_started() {
                            new_sel = Some(match (shift, self.sel) {
                                (true, Some((s, _))) => (s, a),
                                _ => (a, a),
                            });
                        } else if resp.dragged() || (pointer_down && resp.hovered()) {
                            if let Some((s, _)) = self.sel {
                                new_sel = Some((s, a));
                            }
                        }
                    }
                }
            }
        });
        if new_sel.is_some() {
            self.sel = new_sel;
        }
        // Ctrl+C 复制选中的字节（十六进制）
        if let Some((s, e)) = self.sel {
            if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) && ui.memory(|m| m.focused().is_none()) {
                let (s, e) = (s.min(e), s.max(e));
                let txt: Vec<String> = (s..=e.min(s + 65535)).filter_map(|a| self.img.get(a)).map(|b| format!("{b:02X}")).collect();
                ui.ctx().copy_text(txt.join(" "));
            }
        }
    }

    fn inspector(&self, ui: &mut Ui) {
        egui::Frame::new().fill(BG2()).corner_radius(R_CARD).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let Some((s, e)) = self.sel else {
                ui.label(RichText::new(t("点击字节查看它的各种解读；拖动选择一段，Ctrl+C 复制")).small().color(TEXT3()));
                return;
            };
            let (a, b) = (s.min(e), s.max(e));
            let mono = FontId::new(12.0, FontFamily::Monospace);
            ui.label(RichText::new(hex(a)).font(FontId::new(13.0, FontFamily::Monospace)).color(TEXT()).strong());
            if b > a {
                let n = b - a + 1;
                let sum: u64 = (a..=b.min(a + 1_000_000)).filter_map(|x| self.img.get(x)).map(u64::from).sum();
                ui.label(RichText::new(tf("选中 {} 字节 · 累加和 0x{}", &[&n, &format!("{:X}", sum & 0xFFFF_FFFF)])).small().color(TEXT2()));
            }
            ui.add_space(6.0);
            let bytes: Vec<u8> = (a..a + 8).map(|x| self.img.get(x).unwrap_or(0)).collect();
            let avail = |n: u64| (a..a + n).all(|x| self.img.get(x).is_some());
            let le = |n: usize| bytes[..n].iter().rev().fold(0u64, |s, x| (s << 8) | *x as u64);
            let be = |n: usize| bytes[..n].iter().fold(0u64, |s, x| (s << 8) | *x as u64);
            let mut rows: Vec<(&str, String)> = vec![];
            rows.push(("u8", format!("{}", bytes[0])));
            rows.push(("i8", format!("{}", bytes[0] as i8)));
            if avail(2) {
                rows.push(("u16 LE", format!("{}", le(2))));
                rows.push(("u16 BE", format!("{}", be(2))));
                rows.push(("i16 LE", format!("{}", le(2) as u16 as i16)));
            }
            if avail(4) {
                rows.push(("u32 LE", format!("{}  0x{:08X}", le(4), le(4))));
                rows.push(("u32 BE", format!("{}  0x{:08X}", be(4), be(4))));
                rows.push(("i32 LE", format!("{}", le(4) as u32 as i32)));
                rows.push(("f32 LE", fmt_f(f32::from_bits(le(4) as u32) as f64)));
            }
            if avail(8) {
                rows.push(("u64 LE", format!("{}", le(8))));
                rows.push(("f64 LE", fmt_f(f64::from_bits(le(8)))));
            }
            rows.push(("bin", format!("{:08b}", bytes[0])));
            if (0x20..0x7f).contains(&bytes[0]) {
                rows.push(("char", format!("'{}'", bytes[0] as char)));
            }
            egui::Grid::new("hex-insp").num_columns(2).spacing(vec2(10.0, 3.0)).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(RichText::new(k).small().color(TEXT3()));
                    ui.add(egui::Label::new(RichText::new(v).font(mono.clone()).color(TEXT())).truncate());
                    ui.end_row();
                }
            });
            if let Some(c) = &self.cmp {
                if let (Some(x), Some(y)) = (self.img.get(a), c.img.get(a)) {
                    if x != y {
                        ui.add_space(6.0);
                        ui.label(RichText::new(tf("这里：{} → 对方：{}", &[&format!("{x:02X}"), &format!("{y:02X}")])).small().color(ERR()));
                    }
                }
            }
        });
    }
}

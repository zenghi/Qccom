//! Markdown 预览（v1.8）：标题、段落（粗体 / 斜体 / 行内代码 / 链接 / 删除线）、列表和任务列表、代码块（带高亮）、
//! 引用、表格、分隔线、本地图片（GIF 会动）；左边标题大纲；和编辑器分栏时跟着编辑器滚动。
//! 不追求完整的 CommonMark，覆盖 README、文档、AI 写的说明这类常见写法。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, vec2, FontFamily, FontId, RichText, TextureHandle, TextureOptions, Ui};

use crate::highlight::{self, Lang};
use crate::i18n::t;
use crate::theme::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, String),
    Para(String),
    /// 列表项：(缩进层级, 序号（无序为 None）, 任务勾选, 文字)
    Item(u8, Option<String>, Option<bool>, String),
    Code(String, String),
    Quote(String),
    Table(Vec<String>, Vec<Vec<String>>),
    Image(String, String),
    Hr,
}

/// (起始行, 块)
pub fn parse(text: &str) -> Vec<(usize, Block)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = vec![];
    let mut i = 0;
    let row = |l: &str| -> Vec<String> { l.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect() };
    while i < lines.len() {
        let l = lines[i];
        let tr = l.trim();
        let start = i;
        if tr.is_empty() {
            i += 1;
            continue;
        }
        // 代码块
        if let Some(fence) = ["```", "~~~"].iter().find(|f| tr.starts_with(**f)) {
            let lang = tr[3..].trim().to_string();
            let mut body = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with(fence) {
                body.push(lines[i]);
                i += 1;
            }
            i += 1;
            out.push((start, Block::Code(lang, body.join("\n"))));
            continue;
        }
        if let Some(h) = tr
            .strip_prefix('#')
            .map(|_| tr.chars().take_while(|c| *c == '#').count())
            .filter(|n| *n <= 6 && tr[*n..].starts_with(' '))
        {
            out.push((start, Block::Heading(h as u8, tr[h..].trim().trim_end_matches('#').trim().to_string())));
            i += 1;
            continue;
        }
        if tr.len() >= 3
            && (tr.chars().all(|c| c == '-' || c == ' ') || tr.chars().all(|c| c == '*' || c == ' ') || tr.chars().all(|c| c == '_' || c == ' '))
            && tr.chars().filter(|c| !c.is_whitespace()).count() >= 3
        {
            out.push((start, Block::Hr));
            i += 1;
            continue;
        }
        // 表格：本行有 |，下一行是 |---|---|
        if tr.contains('|')
            && lines.get(i + 1).is_some_and(|n| {
                let n = n.trim();
                n.contains('-') && n.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
            })
        {
            let head = row(tr);
            i += 2;
            let mut rows = vec![];
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(row(lines[i]));
                i += 1;
            }
            out.push((start, Block::Table(head, rows)));
            continue;
        }
        if let Some(q) = tr.strip_prefix('>') {
            let mut body = vec![q.trim().to_string()];
            i += 1;
            while i < lines.len() && lines[i].trim().starts_with('>') {
                body.push(lines[i].trim()[1..].trim().to_string());
                i += 1;
            }
            out.push((start, Block::Quote(body.join("\n"))));
            continue;
        }
        // 列表
        let indent = (l.len() - l.trim_start().len()) / 2;
        let bullet = ["- ", "* ", "+ "].iter().find_map(|b| tr.strip_prefix(b)).map(|r| (None, r));
        let ordered = tr
            .find(['.', ')'])
            .filter(|&k| k > 0 && k <= 3 && tr[..k].chars().all(|c| c.is_ascii_digit()) && tr[k + 1..].starts_with(' '))
            .map(|k| (Some(tr[..k].to_string()), &tr[k + 2..]));
        if let Some((num, rest)) = bullet.or(ordered) {
            let (task, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
                (Some(false), r)
            } else if let Some(r) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
                (Some(true), r)
            } else {
                (None, rest)
            };
            out.push((start, Block::Item(indent.min(6) as u8, num, task, rest.to_string())));
            i += 1;
            continue;
        }
        // 单独一行的图片
        if tr.starts_with("![") && tr.ends_with(')') {
            if let Some(m) = tr.find("](") {
                out.push((start, Block::Image(tr[2..m].to_string(), tr[m + 2..tr.len() - 1].split_whitespace().next().unwrap_or("").to_string())));
                i += 1;
                continue;
            }
        }
        // 段落：到空行或别的块为止
        let mut body = vec![tr.to_string()];
        i += 1;
        while i < lines.len() {
            let n = lines[i].trim();
            if n.is_empty() || n.starts_with('#') || n.starts_with("```") || n.starts_with('>') || n.starts_with("- ") || n.starts_with("* ") || n.starts_with('|') {
                break;
            }
            body.push(n.to_string());
            i += 1;
        }
        out.push((start, Block::Para(body.join(" "))));
    }
    out
}

#[derive(Debug, PartialEq)]
enum Span {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Strike(String),
    Link(String, String),
}

fn spans(s: &str) -> Vec<Span> {
    let mut out = vec![];
    let mut buf = String::new();
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let find = |from: usize, pat: &[char]| -> Option<usize> { (from..cs.len().saturating_sub(pat.len() - 1)).find(|&k| cs[k..k + pat.len()] == *pat) };
    let flush = |buf: &mut String, out: &mut Vec<Span>| {
        if !buf.is_empty() {
            out.push(Span::Text(std::mem::take(buf)));
        }
    };
    while i < cs.len() {
        let c = cs[i];
        let rest: String = cs[i..].iter().collect();
        if c == '`' {
            if let Some(e) = find(i + 1, &['`']) {
                flush(&mut buf, &mut out);
                out.push(Span::Code(cs[i + 1..e].iter().collect()));
                i = e + 1;
                continue;
            }
        }
        if rest.starts_with("**") || rest.starts_with("__") {
            let m = [c, c];
            if let Some(e) = find(i + 2, &m) {
                flush(&mut buf, &mut out);
                out.push(Span::Bold(cs[i + 2..e].iter().collect()));
                i = e + 2;
                continue;
            }
        }
        if rest.starts_with("~~") {
            if let Some(e) = find(i + 2, &['~', '~']) {
                flush(&mut buf, &mut out);
                out.push(Span::Strike(cs[i + 2..e].iter().collect()));
                i = e + 2;
                continue;
            }
        }
        if (c == '*' || c == '_') && cs.get(i + 1).is_some_and(|n| !n.is_whitespace()) && (c == '*' || i == 0 || !cs[i - 1].is_alphanumeric()) {
            if let Some(e) = find(i + 1, &[c]) {
                if e > i + 1 {
                    flush(&mut buf, &mut out);
                    out.push(Span::Italic(cs[i + 1..e].iter().collect()));
                    i = e + 1;
                    continue;
                }
            }
        }
        if c == '[' || (c == '!' && cs.get(i + 1) == Some(&'[')) {
            let o = if c == '!' { i + 1 } else { i };
            if let Some(m) = find(o + 1, &[']', '(']) {
                if let Some(e) = find(m + 2, &[')']) {
                    flush(&mut buf, &mut out);
                    out.push(Span::Link(cs[o + 1..m].iter().collect(), cs[m + 2..e].iter().collect()));
                    i = e + 1;
                    continue;
                }
            }
        }
        // 裸网址
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let e = (i..cs.len()).find(|&k| cs[k].is_whitespace() || matches!(cs[k], ')' | '>' | '，' | '。')).unwrap_or(cs.len());
            flush(&mut buf, &mut out);
            let u: String = cs[i..e].iter().collect();
            out.push(Span::Link(u.clone(), u));
            i = e;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

/// 一张图（GIF 有多帧）
struct Pic {
    tex: Vec<(TextureHandle, u32)>,
    size: [usize; 2],
}

pub enum MdEvt {
    /// 打开工程里的文件（相对链接）
    Open(PathBuf),
}

pub struct MdView {
    dir: PathBuf,
    blocks: Vec<(usize, Block)>,
    ver: u64,
    pics: HashMap<String, Option<Pic>>,
    /// 每个块画出来的 y（相对内容顶部），大纲跳转和同步滚动用
    ys: Vec<f32>,
    scroll_to: Option<f32>,
    outline: bool,
    /// 同步滚动：上次对上的编辑器行
    synced: usize,
    /// Markdown 原文（右键「复制 Markdown 原文」）
    src: String,
    /// 代码块「复制」按钮刚点过的时间
    copied: Option<f64>,
    /// 右键菜单位置
    menu_at: Option<egui::Pos2>,
}

impl MdView {
    pub fn new(path: &Path, text: &str) -> MdView {
        MdView {
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            blocks: parse(text),
            ver: 0,
            pics: HashMap::new(),
            ys: vec![],
            scroll_to: None,
            outline: true,
            synced: usize::MAX,
            src: text.to_string(),
            copied: None,
            menu_at: None,
        }
    }

    /// 编辑器内容变了：重新解析
    pub fn update(&mut self, text: &str, ver: u64) {
        if ver != self.ver {
            self.ver = ver;
            self.blocks = parse(text);
            self.src = text.to_string();
        }
    }

    /// 编辑器滚到第 line 行：预览滚到对应的块
    pub fn sync_line(&mut self, line: usize) {
        if line == self.synced {
            return;
        }
        self.synced = line;
        let i = self.blocks.partition_point(|b| b.0 <= line).saturating_sub(1);
        if let Some(y) = self.ys.get(i) {
            self.scroll_to = Some((*y - 8.0).max(0.0));
        }
    }

    fn pic(&mut self, ctx: &egui::Context, src: &str) -> Option<&Pic> {
        if !self.pics.contains_key(src) {
            let p = if src.starts_with("http") {
                None
            } else {
                Some(self.dir.join(src.split(['?', '#']).next().unwrap_or(src)))
            };
            let pic = p.and_then(|p| std::fs::read(p).ok()).and_then(|b| super::image::decode(&b).ok()).map(|frames| Pic {
                size: frames[0].img.size,
                tex: frames
                    .into_iter()
                    .enumerate()
                    .map(|(i, f)| (ctx.load_texture(format!("md-{src}-{i}"), f.img, TextureOptions::LINEAR), f.delay))
                    .collect(),
            });
            self.pics.insert(src.to_string(), pic);
        }
        self.pics.get(src).and_then(|p| p.as_ref())
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Option<MdEvt> {
        let mut evt = None;
        let wide = ui.available_width() > 760.0;
        let heads: Vec<(usize, u8, String)> = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| if let Block::Heading(l, s) = &b.1 { Some((i, *l, s.clone())) } else { None })
            .collect();
        if wide && self.outline && heads.len() > 1 {
            egui::SidePanel::left("md-outline")
                .resizable(false)
                .exact_width(200.0)
                .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 10)))
                .show_inside(ui, |ui| {
                    ui.label(RichText::new(t("大纲")).small().color(TEXT3()));
                    egui::ScrollArea::vertical().id_salt("md-outline-scroll").show(ui, |ui| {
                        for (i, l, s) in &heads {
                            ui.horizontal(|ui| {
                                ui.add_space((*l as f32 - 1.0) * 10.0);
                                let txt = RichText::new(crate::app::truncate(&plain(s), 28)).size(12.0).color(if *l <= 2 { TEXT() } else { TEXT2() });
                                if ui.add(egui::Label::new(txt).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    if let Some(y) = self.ys.get(*i) {
                                        self.scroll_to = Some((*y - 8.0).max(0.0));
                                    }
                                }
                            });
                        }
                    });
                });
        }
        let mut sa = egui::ScrollArea::vertical().id_salt("md-body").auto_shrink([false, false]);
        if let Some(y) = self.scroll_to.take() {
            sa = sa.vertical_scroll_offset(y);
        }
        let blocks = std::mem::take(&mut self.blocks);
        let mut ys = Vec::with_capacity(blocks.len());
        let now = ui.input(|i| i.time);
        let out = sa.show(ui, |ui| {
            // 预览里的文字可以拖选、Ctrl+C 复制（全局默认关掉了标签选择，这里打开）
            ui.style_mut().interaction.selectable_labels = true;
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 24,
                    right: 24,
                    top: 16,
                    bottom: 40,
                })
                .show(ui, |ui| {
                    ui.set_max_width(900.0);
                    let top = ui.min_rect().top();
                    for (_, b) in &blocks {
                        ys.push(ui.cursor().top() - top);
                        match b {
                            Block::Heading(l, s) => {
                                ui.add_space(if *l <= 2 { 14.0 } else { 8.0 });
                                let size = [24.0, 20.0, 17.0, 15.0, 14.0, 13.0][(*l as usize - 1).min(5)];
                                ui.label(RichText::new(plain(s)).font(FontId::new(size, bold())).color(TEXT()));
                                if *l <= 2 {
                                    let r = ui.available_rect_before_wrap();
                                    ui.painter().hline(r.x_range(), r.top() + 2.0, st(1.0, LINE()));
                                    ui.add_space(6.0);
                                }
                            }
                            Block::Para(s) => {
                                if let Some(e) = inline(ui, s) {
                                    evt = self.link(e);
                                }
                                ui.add_space(6.0);
                            }
                            Block::Item(ind, num, task, s) => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add_space(*ind as f32 * 18.0 + 4.0);
                                    match (task, num) {
                                        (Some(done), _) => {
                                            let mut d = *done;
                                            ui.add_enabled(false, egui::Checkbox::without_text(&mut d));
                                        }
                                        (None, Some(n)) => {
                                            ui.label(RichText::new(format!("{n}.")).color(TEXT3()));
                                        }
                                        (None, None) => {
                                            ui.label(RichText::new(if *ind == 0 { "•" } else { "◦" }).color(TEXT3()));
                                        }
                                    }
                                    if let Some(e) = inline_spans(ui, s, *task == Some(true)) {
                                        evt = self.link(e);
                                    }
                                });
                            }
                            Block::Code(lang, code) => {
                                let l = if lang.is_empty() { Lang::Plain } else { Lang::from_path(Path::new(&format!("x.{lang}"))) };
                                let fr = egui::Frame::new().fill(BG2()).corner_radius(R_WIDGET).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    let job = highlight::layout(code, l, FontId::new(12.5, FontFamily::Monospace));
                                    ui.label(job);
                                });
                                // 悬停时右上角出现「复制」
                                let r = fr.response.rect;
                                if ui.rect_contains_pointer(r) {
                                    let br = egui::Rect::from_min_size(egui::pos2(r.right() - 58.0, r.top() + 6.0), vec2(52.0, 22.0));
                                    // 自己画，不占布局位置
                                    let b = ui.interact(br, ui.id().with(("md-copy", ys.len())), egui::Sense::click());
                                    ui.painter().rect_filled(br, R_WIDGET, if b.hovered() { LINE_HI() } else { BG3() });
                                    ui.painter().text(br.center(), egui::Align2::CENTER_CENTER, t("复制"), FontId::proportional(11.5), TEXT());
                                    if b.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if b.clicked() {
                                        ui.ctx().copy_text(code.clone());
                                        self.copied = Some(now);
                                    }
                                }
                                if self.copied.is_some_and(|t0| now - t0 < 1.2) && ui.rect_contains_pointer(r) {
                                    ui.painter().text(egui::pos2(r.right() - 66.0, r.top() + 17.0), egui::Align2::RIGHT_CENTER, t("已复制"), FontId::proportional(11.5), OK());
                                }
                                ui.add_space(6.0);
                            }
                            Block::Quote(s) => {
                                let r0 = ui.cursor().top();
                                egui::Frame::new()
                                    .inner_margin(egui::Margin {
                                        left: 14,
                                        right: 0,
                                        top: 2,
                                        bottom: 2,
                                    })
                                    .show(ui, |ui| {
                                        for line in s.lines() {
                                            ui.label(RichText::new(plain(line)).color(TEXT2()));
                                        }
                                    });
                                let r1 = ui.cursor().top();
                                let x = ui.min_rect().left() + 3.0;
                                ui.painter().vline(x, egui::Rangef::new(r0, r1), st(3.0, ACCENT().gamma_multiply(0.5)));
                                ui.add_space(6.0);
                            }
                            Block::Table(head, rows) => {
                                egui::ScrollArea::horizontal().id_salt(("md-table", ys.len())).show(ui, |ui| {
                                    egui::Grid::new(("md-grid", ys.len())).striped(true).spacing(vec2(18.0, 6.0)).show(ui, |ui| {
                                        for h in head {
                                            ui.label(RichText::new(plain(h)).strong().color(TEXT()));
                                        }
                                        ui.end_row();
                                        for r in rows {
                                            for c in r {
                                                ui.label(RichText::new(plain(c)).color(TEXT2()));
                                            }
                                            ui.end_row();
                                        }
                                    });
                                });
                                ui.add_space(8.0);
                            }
                            Block::Image(alt, src) => {
                                let maxw = ui.available_width();
                                match self.pic(ui.ctx(), src) {
                                    Some(p) => {
                                        // GIF：按每帧时长轮播
                                        let total: u32 = p.tex.iter().map(|f| f.1.max(20)).sum();
                                        let mut k = 0;
                                        if p.tex.len() > 1 {
                                            let mut ms = ((now * 1000.0) as u64 % total.max(1) as u64) as u32;
                                            while k + 1 < p.tex.len() && ms >= p.tex[k].1.max(20) {
                                                ms -= p.tex[k].1.max(20);
                                                k += 1;
                                            }
                                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
                                        }
                                        let sc = (maxw / p.size[0] as f32).min(1.0);
                                        let sz = vec2(p.size[0] as f32 * sc, p.size[1] as f32 * sc);
                                        ui.add(egui::Image::new((p.tex[k].0.id(), sz)));
                                    }
                                    None => {
                                        ui.label(RichText::new(format!("🖼 {alt} ({src})")).color(TEXT3()));
                                    }
                                }
                                ui.add_space(6.0);
                            }
                            Block::Hr => {
                                ui.add_space(6.0);
                                let r = ui.available_rect_before_wrap();
                                ui.painter().hline(r.x_range(), r.top(), st(1.0, LINE()));
                                ui.add_space(8.0);
                            }
                        }
                    }
                });
        });
        self.blocks = blocks;
        self.ys = ys;
        // 右键（文字上也行）：复制全部文字 / 原文
        let area = out.inner_rect;
        if ui.input(|i| i.pointer.secondary_clicked()) {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()).filter(|p| area.contains(*p)) {
                self.menu_at = Some(p);
            }
        }
        if let Some(at) = self.menu_at {
            let mut close = false;
            let r = egui::Area::new(ui.id().with("md-menu")).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_min_width(170.0);
                    ui.label(RichText::new(t("拖选文字后按 Ctrl+C 复制")).small().color(TEXT3()));
                    ui.separator();
                    if ui.button(t("复制全部文字")).clicked() {
                        ui.ctx().copy_text(plain_all(&self.blocks));
                        close = true;
                    }
                    if ui.button(t("复制 Markdown 原文")).clicked() {
                        ui.ctx().copy_text(self.src.clone());
                        close = true;
                    }
                });
            });
            let outside = ui.input(|i| (i.pointer.primary_pressed() || i.key_pressed(egui::Key::Escape)) && i.pointer.interact_pos().is_some_and(|p| !r.response.rect.contains(p)));
            if close || outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.menu_at = None;
            }
        }
        evt
    }

    fn link(&self, url: String) -> Option<MdEvt> {
        if url.starts_with("http://") || url.starts_with("https://") {
            None
        } else {
            let p = self.dir.join(url.split('#').next().unwrap_or(&url));
            p.exists().then_some(MdEvt::Open(p))
        }
    }
}

/// 整篇的纯文字（右键「复制全部文字」）
fn plain_all(blocks: &[(usize, Block)]) -> String {
    let mut out = String::new();
    for (_, b) in blocks {
        match b {
            Block::Heading(_, s) | Block::Para(s) => out.push_str(&plain(s)),
            Block::Item(ind, num, task, s) => {
                out.push_str(&"  ".repeat(*ind as usize));
                match (task, num) {
                    (Some(d), _) => out.push_str(if *d { "[x] " } else { "[ ] " }),
                    (None, Some(n)) => out.push_str(&format!("{n}. ")),
                    _ => out.push_str("- "),
                }
                out.push_str(&plain(s));
            }
            Block::Code(_, c) => out.push_str(c),
            Block::Quote(s) => out.push_str(&s.lines().map(plain).collect::<Vec<_>>().join("\n")),
            Block::Table(h, rows) => {
                out.push_str(&h.iter().map(|c| plain(c)).collect::<Vec<_>>().join("\t"));
                for r in rows {
                    out.push('\n');
                    out.push_str(&r.iter().map(|c| plain(c)).collect::<Vec<_>>().join("\t"));
                }
            }
            Block::Image(alt, _) => out.push_str(alt),
            Block::Hr => out.push_str("---"),
        }
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
}

/// 去掉行内标记，只留文字（标题、表格、大纲用）
fn plain(s: &str) -> String {
    spans(s)
        .into_iter()
        .map(|sp| match sp {
            Span::Text(x) | Span::Bold(x) | Span::Italic(x) | Span::Code(x) | Span::Strike(x) => x,
            Span::Link(x, _) => x,
        })
        .collect()
}

fn inline(ui: &mut Ui, s: &str) -> Option<String> {
    let mut out = None;
    ui.horizontal_wrapped(|ui| out = inline_spans(ui, s, false));
    out
}

/// 画一段带格式的文字；点了相对链接时返回链接
pub(crate) fn inline_spans(ui: &mut Ui, s: &str, done: bool) -> Option<String> {
    ui.spacing_mut().item_spacing.x = 0.0;
    let mut out = None;
    let base = if done { TEXT3() } else { TEXT() };
    for sp in spans(s) {
        match sp {
            Span::Text(x) => {
                let mut r = RichText::new(x).color(base);
                if done {
                    r = r.strikethrough();
                }
                ui.label(r);
            }
            Span::Bold(x) => {
                ui.label(RichText::new(x).strong().color(base));
            }
            Span::Italic(x) => {
                ui.label(RichText::new(x).italics().color(base));
            }
            Span::Strike(x) => {
                ui.label(RichText::new(x).strikethrough().color(TEXT3()));
            }
            Span::Code(x) => {
                ui.label(
                    RichText::new(x)
                        .font(FontId::new(12.5, FontFamily::Monospace))
                        .color(crate::highlight::pal::STR())
                        .background_color(BG2()),
                );
            }
            Span::Link(text, url) => {
                if url.starts_with("http://") || url.starts_with("https://") {
                    ui.hyperlink_to(RichText::new(text).color(ACCENT()), url);
                } else if ui
                    .add(egui::Label::new(RichText::new(text).color(ACCENT()).underline()).sense(egui::Sense::click()))
                    .on_hover_text(&url)
                    .clicked()
                {
                    out = Some(url);
                }
            }
        }
    }
    out
}

#[allow(dead_code)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_parse() {
        let b = parse("# T\n\nhello **b** `c`\nmore\n\n- [x] done\n  - sub\n1. one\n\n```c\nint a;\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n> q\n\n![p](x.gif)\n---\n");
        assert_eq!(b[0], (0, Block::Heading(1, "T".into())));
        assert_eq!(b[1].1, Block::Para("hello **b** `c` more".into()));
        assert_eq!(b[2].1, Block::Item(0, None, Some(true), "done".into()));
        assert_eq!(b[3].1, Block::Item(1, None, None, "sub".into()));
        assert_eq!(b[4].1, Block::Item(0, Some("1".into()), None, "one".into()));
        assert_eq!(b[5].1, Block::Code("c".into(), "int a;".into()));
        assert_eq!(b[6].1, Block::Table(vec!["a".into(), "b".into()], vec![vec!["1".into(), "2".into()]]));
        assert_eq!(b[7].1, Block::Quote("q".into()));
        assert_eq!(b[8].1, Block::Image("p".into(), "x.gif".into()));
        assert_eq!(b[9].1, Block::Hr);
        assert_eq!(
            spans("a **b** *c* `d` [e](f) ~~g~~"),
            vec![
                Span::Text("a ".into()),
                Span::Bold("b".into()),
                Span::Text(" ".into()),
                Span::Italic("c".into()),
                Span::Text(" ".into()),
                Span::Code("d".into()),
                Span::Text(" ".into()),
                Span::Link("e".into(), "f".into()),
                Span::Text(" ".into()),
                Span::Strike("g".into()),
            ]
        );
        assert_eq!(spans("snake_case_name"), vec![Span::Text("snake_case_name".into())]);
    }
}

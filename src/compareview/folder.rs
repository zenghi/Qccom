//! 文件夹比较：两棵树按相对路径对齐，后台比较内容；复制、删除、同步都在这里做。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Ui};

use super::{nav, save_report, step, CompareSettings, Out};
use crate::app::{draw_icon, human_bytes, icon_button, menu_item, toggle_chip, Icon};
use crate::codeview::{chevron, file_badge, reveal};
use crate::compare::folder::{self as fc, Entry, Filter, Op, St, Sync};
use crate::i18n::{t, tf};
use crate::theme::*;

const RH: f32 = 24.0;
const GUT: f32 = 30.0;

enum Msg {
    Tree(Vec<Entry>),
    St(Vec<(usize, St)>),
    Progress(usize, usize),
}

enum Ask {
    Sync(Vec<Op>),
    Delete(String, bool),
}

pub struct FolderCmp {
    pub a: PathBuf,
    pub b: PathBuf,
    es: Vec<Entry>,
    rx: Option<mpsc::Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    progress: Option<(usize, usize)>,
    /// 收起的文件夹（相对路径）
    closed: HashSet<String>,
    first: bool,
    sel: Option<String>,
    /// 当前这处不同（差异列表下标）
    cur: Option<usize>,
    filter_edit: String,
    ask: Option<Ask>,
    scroll_to: Option<String>,
}

impl Drop for FolderCmp {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl FolderCmp {
    pub fn new(a: &Path, b: &Path, s: &CompareSettings) -> Self {
        let mut x = Self {
            a: a.to_path_buf(),
            b: b.to_path_buf(),
            es: vec![],
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
            closed: HashSet::new(),
            first: true,
            sel: None,
            cur: None,
            filter_edit: s.filter.clone(),
            ask: None,
            scroll_to: None,
        };
        x.rescan(s);
        x
    }

    fn rescan(&mut self, s: &CompareSettings) {
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.progress = Some((0, 0));
        let (a, b) = (self.a.clone(), self.b.clone());
        let filter = Filter::parse(&s.filter);
        let content = s.content;
        std::thread::spawn(move || {
            let es = fc::scan(&a, &b, &filter);
            // 要看内容的文件：按内容比较时是两边都有的所有文件，否则只看大小一样、时间不同的
            let todo: Vec<usize> = es
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.dir && e.l.is_some() && e.r.is_some() && (e.st == St::Pending || (content && e.st == St::Same)))
                .map(|(i, _)| i)
                .collect();
            let rels: Vec<String> = todo.iter().map(|&i| es[i].rel.clone()).collect();
            if tx.send(Msg::Tree(es)).is_err() {
                return;
            }
            let n = todo.len();
            let mut batch = vec![];
            for (k, (i, rel)) in todo.into_iter().zip(rels).enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let st = match fc::same_content(&fc::join(&a, &rel), &fc::join(&b, &rel)) {
                    Ok(true) => St::Same,
                    Ok(false) => St::Diff,
                    Err(_) => St::Error,
                };
                batch.push((i, st));
                if batch.len() >= 64 {
                    let _ = tx.send(Msg::St(std::mem::take(&mut batch)));
                    let _ = tx.send(Msg::Progress(k + 1, n));
                }
            }
            let _ = tx.send(Msg::St(batch));
        });
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut changed = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Tree(es)) => {
                    self.es = es;
                    changed = true;
                }
                Ok(Msg::St(v)) => {
                    for (i, st) in v {
                        if let Some(e) = self.es.get_mut(i) {
                            e.st = st;
                        }
                    }
                    changed = true;
                }
                Ok(Msg::Progress(k, n)) => self.progress = Some((k, n)),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    self.progress = None;
                    break;
                }
            }
        }
        if changed {
            fc::rollup(&mut self.es);
            if self.first && !self.es.is_empty() {
                // 一开始只展开有不同的文件夹
                self.first = false;
                for e in &self.es {
                    if e.dir && (e.depth > 0 && e.counts == (0, 0, 0) || matches!(e.st, St::LeftOnly | St::RightOnly)) {
                        self.closed.insert(e.rel.clone());
                    }
                }
            }
        }
    }

    fn counts(&self) -> (usize, usize, usize, usize) {
        let mut c = (0, 0, 0, 0);
        for e in self.es.iter().filter(|e| !e.dir) {
            match e.st {
                St::Same => c.3 += 1,
                St::LeftOnly => c.1 += 1,
                St::RightOnly => c.2 += 1,
                _ => c.0 += 1,
            }
        }
        c
    }

    /// 看得见的条目：跳过收起的文件夹里面的，以及（只看不同时）相同的
    fn visible(&self, only_diff: bool) -> Vec<usize> {
        let mut v = vec![];
        let mut skip: Option<usize> = None;
        for (i, e) in self.es.iter().enumerate() {
            if let Some(d) = skip {
                if e.depth > d {
                    continue;
                }
                skip = None;
            }
            if only_diff && e.st == St::Same && (!e.dir || e.counts == (0, 0, 0)) {
                if e.dir {
                    skip = Some(e.depth);
                }
                continue;
            }
            v.push(i);
            if e.dir && self.closed.contains(&e.rel) {
                skip = Some(e.depth);
            }
        }
        v
    }

    /// 有不同的文件（跳转用）
    fn diffs(&self) -> Vec<usize> {
        self.es.iter().enumerate().filter(|(_, e)| !e.dir && e.st != St::Same).map(|(i, _)| i).collect()
    }

    fn reveal_entry(&mut self, i: usize) {
        // 展开它所在的文件夹
        let rel = self.es[i].rel.clone();
        let mut p = String::new();
        for part in rel.split('/') {
            if !p.is_empty() {
                self.closed.remove(&p);
                p.push('/');
            }
            p.push_str(part);
        }
        self.sel = Some(rel.clone());
        self.scroll_to = Some(rel);
    }

    fn run(&mut self, ops: &[Op], s: &CompareSettings, outs: &mut Vec<Out>) {
        let mut ok = 0;
        let mut errs = vec![];
        for op in ops {
            match fc::apply(&self.a, &self.b, op) {
                Ok(_) => ok += 1,
                Err(e) => errs.push(e.to_string()),
            }
        }
        if errs.is_empty() {
            outs.push(Out::Toast(tf("完成 {} 项操作", &[&ok]), OK()));
        } else {
            outs.push(Out::Toast(format!("{}  {}", tf("{} 项失败", &[&errs.len()]), errs[0]), ERR()));
        }
        self.rescan(s);
    }

    // ------------------------------------------------------------ 界面

    pub fn ui(&mut self, ui: &mut Ui, s: &mut CompareSettings) -> Vec<Out> {
        self.poll();
        if self.rx.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        }
        let mut outs = vec![];
        self.toolbar(ui, s, &mut outs);
        self.header(ui);
        self.tree(ui, s, &mut outs);
        self.modal(ui.ctx(), s, &mut outs);
        outs
    }

    fn toolbar(&mut self, ui: &mut Ui, s: &mut CompareSettings, outs: &mut Vec<Out>) {
        egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let d = self.diffs();
                if let Some(next) = nav(ui, d.len(), self.cur) {
                    self.cur = step(self.cur, d.len(), next);
                    if let Some(&i) = self.cur.and_then(|c| d.get(c)) {
                        self.reveal_entry(i);
                    }
                }
                ui.add_space(6.0);
                let (df, lo, ro, same) = self.counts();
                crate::app::pill(ui, &format!("{} {df}", t("不同")), WARN());
                crate::app::pill(ui, &format!("{} {lo}", t("只在左侧")), ERR());
                crate::app::pill(ui, &format!("{} {ro}", t("只在右侧")), OK());
                crate::app::pill(ui, &format!("{} {same}", t("相同")), TEXT3());
                if let Some((k, n)) = self.progress {
                    ui.add(egui::Spinner::new().size(14.0));
                    if n > 0 {
                        ui.label(RichText::new(tf("比较内容 {}/{}", &[&k, &n])).small().color(TEXT3()));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Download, &t("导出 HTML 报告"), false).clicked() {
                        let html = crate::compare::report::folder(&self.a.display().to_string(), &self.b.display().to_string(), &self.es, s.only_diff);
                        save_report(html, &format!("{}-compare", super::name_of(&self.a)), outs);
                    }
                    if icon_button(ui, Icon::Refresh, &t("重新比较  F5"), false).clicked() {
                        self.rescan(s);
                    }
                    self.sync_menu(ui);
                    self.filter_menu(ui, s);
                    // 一个按钮：有收起的就全部展开，否则全部收起
                    let any_closed = !self.closed.is_empty();
                    let tip = if any_closed { t("全部展开") } else { t("全部收起") };
                    if icon_button(ui, Icon::Collapse, tip, any_closed).clicked() {
                        if any_closed {
                            self.closed.clear();
                        } else {
                            for e in self.es.iter().filter(|e| e.dir) {
                                self.closed.insert(e.rel.clone());
                            }
                        }
                    }
                    if toggle_chip(ui, &mut s.content, t("按内容"), t("逐字节比较内容；关掉时按大小和修改时间判断（快）")) {
                        self.rescan(s);
                    }
                    toggle_chip(ui, &mut s.only_diff, t("只看不同"), t("隐藏相同的文件和文件夹"));
                });
            });
        });
        if ui.memory(|m| m.focused().is_none()) && ui.input(|i| i.key_pressed(egui::Key::F5)) {
            self.rescan(s);
        }
    }

    fn sync_menu(&mut self, ui: &mut Ui) {
        let r = ui.add(egui::Button::new(RichText::new(t("同步…")).size(12.0)).min_size(vec2(0.0, 26.0)));
        let id = egui::Id::new("fc-sync");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        let busy = self.rx.is_some();
        egui::popup::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(300.0);
            if busy {
                ui.label(RichText::new(t("还在比较，等比较完再同步")).small().color(WARN()));
            }
            for (m, name, note) in [
                (Sync::UpdateRight, t("更新右边"), t("左边新的、只在左边的复制到右边")),
                (Sync::UpdateLeft, t("更新左边"), t("右边新的、只在右边的复制到左边")),
                (Sync::Both, t("双向更新"), t("两边互相补齐，各取较新的")),
                (Sync::MirrorRight, t("镜像到右边"), t("右边变得和左边一样（会删除只在右边的）")),
                (Sync::MirrorLeft, t("镜像到左边"), t("左边变得和右边一样（会删除只在左边的）")),
            ] {
                let resp = ui.add_enabled_ui(!busy, |ui| {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(4), BG3());
                    }
                    ui.painter().text(pos2(r.left() + 8.0, r.top() + 12.0), Align2::LEFT_CENTER, name, FontId::proportional(13.0), TEXT());
                    ui.painter().text(pos2(r.left() + 8.0, r.top() + 29.0), Align2::LEFT_CENTER, note, FontId::proportional(11.0), TEXT3());
                    resp
                });
                if resp.inner.clicked() {
                    self.ask = Some(Ask::Sync(fc::plan(&self.es, m)));
                }
            }
        });
    }

    fn filter_menu(&mut self, ui: &mut Ui, s: &mut CompareSettings) {
        let r = icon_button(ui, Icon::Filter, &t("过滤规则"), !s.filter.trim().is_empty());
        let id = egui::Id::new("fc-filter");
        if r.clicked() {
            self.filter_edit = s.filter.clone();
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        egui::popup::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(t("不参与比较的文件和文件夹")).strong());
            ui.label(RichText::new(t("用分号或换行分开；以 / 结尾的是文件夹，支持 * 和 ?")).small().color(TEXT3()));
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::multiline(&mut self.filter_edit)
                    .font(FontId::monospace(12.0))
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new(t("应用")).color(ON_ACCENT())).fill(ACCENT())).clicked() {
                    s.filter = self.filter_edit.trim().to_string();
                    self.rescan(s);
                    ui.memory_mut(|m| m.close_popup());
                }
                if ui.button(t("恢复默认")).clicked() {
                    self.filter_edit = CompareSettings::default().filter;
                }
            });
        });
    }

    fn header(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let r = Rect::from_min_size(full.min, vec2(full.width(), 28.0));
        ui.allocate_rect(r, Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 0.0, BG1());
        p.hline(r.x_range(), r.bottom() - 0.5, st(1.0, LINE()));
        let sw = (r.width() - GUT) / 2.0;
        for (k, root) in [&self.a, &self.b].into_iter().enumerate() {
            let x = r.left() + k as f32 * (sw + GUT);
            let sr = Rect::from_min_size(pos2(x, r.top()), vec2(sw, r.height()));
            draw_icon(p, Rect::from_center_size(pos2(x + 16.0, sr.center().y), vec2(14.0, 14.0)), Icon::Folder, ACCENT());
            let g = p.layout_no_wrap(root.display().to_string(), FontId::monospace(11.5), TEXT());
            p.with_clip_rect(sr.shrink2(vec2(4.0, 0.0))).galley(pos2(x + 30.0, sr.center().y - g.size().y / 2.0), g, TEXT());
            ui.interact(sr, ui.id().with(("fc-root", k)), Sense::hover()).on_hover_text(root.display().to_string());
        }
    }

    fn tree(&mut self, ui: &mut Ui, s: &CompareSettings, outs: &mut Vec<Out>) {
        let vis = self.visible(s.only_diff);
        if self.es.is_empty() {
            let msg = if self.rx.is_some() { t("正在扫描…") } else { t("两边都是空文件夹") };
            let r = ui.available_rect_before_wrap();
            ui.painter().text(r.center(), Align2::CENTER_CENTER, msg, FontId::proportional(14.0), TEXT3());
            return;
        }
        if vis.is_empty() {
            let r = ui.available_rect_before_wrap();
            ui.painter().text(r.center(), Align2::CENTER_CENTER, t("两边内容相同"), FontId::proportional(14.0), OK());
            return;
        }
        // 键盘：上下选择，回车比较，左右收起 / 展开
        let pos = self.sel.as_ref().and_then(|r| vis.iter().position(|&i| &self.es[i].rel == r));
        if ui.memory(|m| m.focused().is_none()) {
            let (up, down, enter, left, right) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::ArrowDown) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::ArrowLeft) && !i.modifiers.alt,
                    i.key_pressed(egui::Key::ArrowRight) && !i.modifiers.alt,
                )
            });
            let mv = |d: isize| pos.map_or(0, |p| (p as isize + d).clamp(0, vis.len() as isize - 1) as usize);
            if up || down {
                let k = mv(if up { -1 } else { 1 });
                self.sel = Some(self.es[vis[k]].rel.clone());
                self.scroll_to = self.sel.clone();
            }
            if let Some(p) = pos {
                let e = &self.es[vis[p]];
                if enter {
                    self.activate(vis[p], outs);
                } else if e.dir && left {
                    self.closed.insert(e.rel.clone());
                } else if e.dir && right {
                    self.closed.remove(&e.rel);
                }
            }
        }
        let mut sa = egui::ScrollArea::vertical().id_salt("fc-tree").auto_shrink([false, false]);
        if let Some(rel) = self.scroll_to.take() {
            if let Some(k) = vis.iter().position(|&i| self.es[i].rel == rel) {
                let h = ui.available_height();
                sa = sa.vertical_scroll_offset((k as f32 * RH - h / 2.0).max(0.0));
            }
        }
        let mut click: Option<(usize, bool)> = None;
        let mut menu: Vec<Op> = vec![];
        let mut ask: Option<Ask> = None;
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show_rows(ui, RH, vis.len(), |ui, range| {
            for k in range {
                let i = vis[k];
                let resp = self.row_ui(ui, i);
                if resp.double_clicked() {
                    click = Some((i, true));
                } else if resp.clicked() {
                    click = Some((i, false));
                }
                let e = &self.es[i];
                resp.context_menu(|ui| {
                    ui.set_min_width(230.0);
                    let (l, r) = (fc::join(&self.a, &e.rel), fc::join(&self.b, &e.rel));
                    if e.l.is_some() && e.r.is_some() && menu_item(ui, t("比较"), "Enter") {
                        click = Some((i, true));
                        ui.close_menu();
                    }
                    if e.l.is_some() && menu_item(ui, t("复制到右边  →"), "") {
                        menu.push(Op::Copy(e.rel.clone(), true));
                        ui.close_menu();
                    }
                    if e.r.is_some() && menu_item(ui, t("复制到左边  ←"), "") {
                        menu.push(Op::Copy(e.rel.clone(), false));
                        ui.close_menu();
                    }
                    ui.separator();
                    if e.l.is_some() && menu_item(ui, t("删除左边的"), "") {
                        ask = Some(Ask::Delete(e.rel.clone(), true));
                        ui.close_menu();
                    }
                    if e.r.is_some() && menu_item(ui, t("删除右边的"), "") {
                        ask = Some(Ask::Delete(e.rel.clone(), false));
                        ui.close_menu();
                    }
                    ui.separator();
                    if menu_item(ui, t("在文件管理器中显示"), "") {
                        reveal(if e.l.is_some() { &l } else { &r });
                        ui.close_menu();
                    }
                    if !e.dir {
                        if e.l.is_some() && menu_item(ui, t("在代码页打开左边的"), "") {
                            outs.push(Out::OpenFile(l.clone(), 0));
                            ui.close_menu();
                        }
                        if e.r.is_some() && menu_item(ui, t("在代码页打开右边的"), "") {
                            outs.push(Out::OpenFile(r.clone(), 0));
                            ui.close_menu();
                        }
                    }
                    if menu_item(ui, t("复制相对路径"), "") {
                        ui.ctx().copy_text(e.rel.clone());
                        ui.close_menu();
                    }
                });
            }
        });
        if let Some((i, dbl)) = click {
            self.sel = Some(self.es[i].rel.clone());
            if self.es[i].dir && !dbl {
                // 单击文件夹就展开 / 收起
                let rel = self.es[i].rel.clone();
                if !self.closed.remove(&rel) {
                    self.closed.insert(rel);
                }
            } else if dbl {
                self.activate(i, outs);
            }
        }
        if !menu.is_empty() {
            self.run(&menu, s, outs);
        }
        if ask.is_some() {
            self.ask = ask;
        }
    }

    /// 回车 / 双击：文件打开比较；文件夹展开收起
    fn activate(&mut self, i: usize, outs: &mut Vec<Out>) {
        let e = &self.es[i];
        if e.dir {
            let rel = e.rel.clone();
            if !self.closed.remove(&rel) {
                self.closed.insert(rel);
            }
            return;
        }
        if e.l.is_some() && e.r.is_some() {
            let (l, r) = (fc::join(&self.a, &e.rel), fc::join(&self.b, &e.rel));
            let m = crate::compare::guess_mode(&l, &r);
            outs.push(Out::Compare(l, r, m));
        } else {
            let p = if e.l.is_some() { fc::join(&self.a, &e.rel) } else { fc::join(&self.b, &e.rel) };
            outs.push(Out::OpenFile(p, 0));
        }
    }

    fn row_ui(&self, ui: &mut Ui, i: usize) -> egui::Response {
        let e = &self.es[i];
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), RH), Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        let selected = self.sel.as_deref() == Some(e.rel.as_str());
        if selected {
            p.rect_filled(rect, 0.0, ACCENT().gamma_multiply(0.12));
        } else if resp.hovered() {
            p.rect_filled(rect, 0.0, tint(7));
        }
        let sw = (rect.width() - GUT) / 2.0;
        let (col, tag) = match e.st {
            St::Same => (TEXT2(), None),
            St::Diff => (WARN(), None),
            St::Pending => (TEXT2(), Some("…")),
            St::Error => (ERR(), Some("!")),
            St::LeftOnly => (ERR(), None),
            St::RightOnly => (OK(), None),
        };
        let newer = e.newer();
        for side in 0..2 {
            let x0 = rect.left() + side as f32 * (sw + GUT);
            let r = Rect::from_min_size(pos2(x0, rect.top()), vec2(sw, RH));
            let meta = if side == 0 { e.l } else { e.r };
            let Some(m) = meta else {
                // 这边没有：淡淡的斜线
                let pp = p.with_clip_rect(r.shrink2(vec2(0.0, 1.0)));
                let mut x = r.left() - RH;
                while x < r.right() {
                    pp.line_segment([pos2(x, r.bottom()), pos2(x + RH, r.top())], st(1.0, LINE().gamma_multiply(0.7)));
                    x += 9.0;
                }
                continue;
            };
            let show_time = sw > 420.0;
            let tw = if show_time { 128.0 } else { 0.0 };
            let name_r = Rect::from_min_max(r.min, pos2(r.right() - 76.0 - tw, r.bottom()));
            let x = r.left() + 10.0 + e.depth as f32 * 14.0;
            for k in 0..e.depth {
                let gx = r.left() + 16.0 + k as f32 * 14.0;
                p.vline(gx, r.y_range(), st(1.0, LINE().gamma_multiply(0.8)));
            }
            let pc = p.with_clip_rect(name_r);
            if e.dir {
                chevron(&pc, pos2(x + 6.0, r.center().y), !self.closed.contains(&e.rel), TEXT3());
                draw_icon(
                    &pc,
                    Rect::from_center_size(pos2(x + 22.0, r.center().y), vec2(14.0, 14.0)),
                    Icon::Folder,
                    if e.st == St::Same { TEXT3() } else { col },
                );
            } else {
                file_badge(&pc, pos2(x + 20.0, r.center().y), &e.name);
            }
            let g = pc.layout_no_wrap(e.name.clone(), FontId::proportional(12.5), col);
            let gw = g.size().x;
            pc.galley(pos2(x + 33.0, r.center().y - g.size().y / 2.0), g, col);
            if e.dir && e.counts != (0, 0, 0) {
                let (d, l, rr) = e.counts;
                let n = d + if side == 0 { l } else { rr };
                if n > 0 {
                    pc.text(pos2(x + 40.0 + gw, r.center().y), Align2::LEFT_CENTER, n.to_string(), FontId::monospace(10.5), TEXT3());
                }
            }
            if !e.dir {
                p.text(pos2(r.right() - 8.0 - tw, r.center().y), Align2::RIGHT_CENTER, human_bytes(m.size), FontId::monospace(11.0), TEXT3());
            }
            if show_time {
                let s = chrono::DateTime::from_timestamp(m.mtime, 0)
                    .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                let is_newer = (side == 0 && newer < 0) || (side == 1 && newer > 0);
                p.text(
                    pos2(r.right() - 8.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    s,
                    FontId::monospace(11.0),
                    if is_newer { ACCENT() } else { TEXT3() },
                );
            }
        }
        // 中间的状态
        let c = pos2(rect.left() + sw + GUT / 2.0, rect.center().y);
        match e.st {
            St::Same => {
                p.text(c, Align2::CENTER_CENTER, "=", FontId::monospace(12.0), TEXT3());
            }
            St::Diff => {
                p.text(c, Align2::CENTER_CENTER, "≠", FontId::proportional(13.0), WARN());
            }
            St::LeftOnly => super::text::arrow(p, c, false, ERR()),
            St::RightOnly => super::text::arrow(p, c, true, OK()),
            _ => {
                p.text(c, Align2::CENTER_CENTER, tag.unwrap_or(""), FontId::monospace(12.0), col);
            }
        }
        resp
    }

    fn modal(&mut self, ctx: &egui::Context, s: &CompareSettings, outs: &mut Vec<Out>) {
        let Some(ask) = &self.ask else { return };
        let mut close = false;
        let mut run: Option<Vec<Op>> = None;
        egui::Modal::new(egui::Id::new("fc-ask")).show(ctx, |ui| {
            ui.set_width(520.0);
            match ask {
                Ask::Sync(ops) => {
                    ui.label(RichText::new(t("确认同步")).strong().size(15.0));
                    ui.add_space(4.0);
                    if ops.is_empty() {
                        ui.label(RichText::new(t("没有需要同步的文件")).color(TEXT2()));
                    } else {
                        let dels = ops.iter().filter(|o| matches!(o, Op::Delete(..))).count();
                        ui.label(RichText::new(tf("{} 项操作，其中 {} 项删除", &[&ops.len(), &dels])).color(if dels > 0 { WARN() } else { TEXT2() }));
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            for op in ops.iter().take(500) {
                                let (txt, c) = match op {
                                    Op::Copy(r, true) => (format!("→  {r}"), ACCENT()),
                                    Op::Copy(r, false) => (format!("←  {r}"), ACCENT()),
                                    Op::Delete(r, true) => (format!("{}  {r}", t("删除左边")), ERR()),
                                    Op::Delete(r, false) => (format!("{}  {r}", t("删除右边")), ERR()),
                                };
                                ui.label(RichText::new(txt).font(FontId::monospace(11.5)).color(c));
                            }
                        });
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if !ops.is_empty() && ui.add(egui::Button::new(RichText::new(t("开始同步")).color(ON_ACCENT())).fill(ACCENT())).clicked() {
                            run = Some(ops.clone());
                        }
                        if ui.button(t("取消")).clicked() {
                            close = true;
                        }
                    });
                }
                Ask::Delete(rel, left) => {
                    ui.label(RichText::new(t("确认删除")).strong().size(15.0));
                    ui.add_space(4.0);
                    let root = if *left { &self.a } else { &self.b };
                    ui.label(RichText::new(fc::join(root, rel).display().to_string()).font(FontId::monospace(12.0)).color(ERR()));
                    ui.label(RichText::new(t("删除后无法恢复")).small().color(TEXT3()));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new(t("删除")).color(Color32::WHITE)).fill(ERR())).clicked() {
                            run = Some(vec![Op::Delete(rel.clone(), *left)]);
                        }
                        if ui.button(t("取消")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if let Some(ops) = run {
            self.ask = None;
            self.run(&ops, s, outs);
        } else if close {
            self.ask = None;
        }
    }
}

//! 比较页：文件夹、文本、十六进制、三方合并、表格比较，多个标签同时开着。
//! 首页选两边的路径（或拖文件进来），也列出最近和保存的比较会话。

mod folder;
mod hex;
mod merge;
mod table;
mod text;

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Rect, RichText, Sense, Ui};
use serde::{Deserialize, Serialize};

use crate::app::{draw_icon, hgrad_line, icon_button, menu_item, toggle_chip, Icon};
use crate::compare::{self, diff::Opts, Mode};
use crate::i18n::{t, tf};
use crate::theme::*;

/// 保存下来的一次比较
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Saved {
    pub name: String,
    pub mode: Mode,
    pub left: String,
    pub right: String,
    #[serde(default)]
    pub base: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct CompareSettings {
    pub opts: Opts,
    /// 文件夹比较的过滤规则
    pub filter: String,
    /// 文件夹按内容逐字节比较（否则按大小 + 修改时间）
    pub content: bool,
    /// 只看不同
    pub only_diff: bool,
    /// BIN 自动对齐插入 / 删除的字节
    pub align_hex: bool,
    pub recent: Vec<Saved>,
    pub saved: Vec<Saved>,
}

impl Default for CompareSettings {
    fn default() -> Self {
        Self {
            opts: Opts::default(),
            filter: ".git/; .svn/; .vs/; build/; *.o; *.obj; *.d".into(),
            content: false,
            only_diff: false,
            align_hex: true,
            recent: vec![],
            saved: vec![],
        }
    }
}

pub enum CmpEvt {
    Toast(String, Color32),
    /// 在代码页打开（行号从 0 开始）
    OpenFile(PathBuf, usize),
}

/// 子视图的一次请求
pub(crate) enum Out {
    Toast(String, Color32),
    OpenFile(PathBuf, usize),
    Compare(PathBuf, PathBuf, Mode),
}

/// 一边：文件或文件夹，显示名（比如 "main.c (HEAD)"），临时文件只读
#[derive(Clone)]
pub struct Side {
    pub path: PathBuf,
    pub label: String,
    pub readonly: bool,
}

impl Side {
    pub fn file(p: &Path) -> Side {
        Side {
            path: p.to_path_buf(),
            label: name_of(p),
            readonly: false,
        }
    }

    pub fn labeled(p: &Path, label: &str) -> Side {
        Side {
            path: p.to_path_buf(),
            label: label.to_string(),
            readonly: true,
        }
    }
}

pub(crate) fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

enum Tab {
    Text(text::TextCmp),
    Folder(folder::FolderCmp),
    Hex(hex::HexTab),
    Merge(merge::MergeTab),
    Table(table::TableTab),
}

impl Tab {
    fn title(&self) -> (String, &'static str, bool) {
        match self {
            Tab::Text(x) => (format!("{} ↔ {}", x.a.label, x.b.label), "T", x.dirty()),
            Tab::Folder(x) => (format!("{} ↔ {}", name_of(&x.a), name_of(&x.b)), "D", false),
            Tab::Hex(x) => (format!("{} ↔ {}", x.a.label, x.b.label), "0x", false),
            Tab::Merge(x) => (x.title(), "3", !x.saved),
            Tab::Table(x) => (format!("{} ↔ {}", name_of(&x.a), name_of(&x.b)), "▦", false),
        }
    }

    fn color(&self) -> Color32 {
        match self {
            Tab::Text(_) => ACCENT(),
            Tab::Folder(_) => WARN(),
            Tab::Hex(_) => VIOLET(),
            Tab::Merge(_) => ERR(),
            Tab::Table(_) => OK(),
        }
    }

    fn paths(&self) -> Option<(PathBuf, PathBuf)> {
        match self {
            Tab::Text(x) => Some((x.a.path.clone(), x.b.path.clone())),
            Tab::Folder(x) => Some((x.a.clone(), x.b.clone())),
            Tab::Hex(x) => Some((x.a.path.clone(), x.b.path.clone())),
            Tab::Table(x) => Some((x.a.clone(), x.b.clone())),
            Tab::Merge(_) => None,
        }
    }
}

#[derive(Default)]
struct Home {
    left: String,
    right: String,
    base: String,
    rename: Option<(usize, String)>,
}

pub struct CompareView {
    pub s: CompareSettings,
    tabs: Vec<Tab>,
    /// None = 首页
    active: Option<usize>,
    home: Home,
    pub events: Vec<CmpEvt>,
    ext: Option<Vec<(&'static str, PathBuf)>>,
    /// 关闭有未保存修改的标签前确认
    close_ask: Option<usize>,
}

impl CompareView {
    pub fn new(s: CompareSettings) -> Self {
        // 上次留下的临时文件
        let _ = std::fs::remove_dir_all(temp_dir());
        Self {
            s,
            tabs: vec![],
            active: None,
            home: Home::default(),
            events: vec![],
            ext: None,
            close_ask: None,
        }
    }

    /// 当前标签（None = 首页）
    pub fn active_tab(&self) -> Option<usize> {
        self.active
    }

    pub fn set_active_tab(&mut self, i: Option<usize>) {
        self.active = i.filter(|i| *i < self.tabs.len());
    }

    /// 标签标题（切换器用）
    pub fn tab_title(&self, i: usize) -> Option<String> {
        self.tabs.get(i).map(|t| t.title().0)
    }

    pub fn has_unsaved(&self) -> bool {
        self.tabs.iter().any(|t| t.title().2)
    }

    /// 比较两个文件或文件夹；mode 为 None 时自动判断
    pub fn open(&mut self, a: &Path, b: &Path, mode: Option<Mode>) {
        let mode = mode.unwrap_or_else(|| compare::guess_mode(a, b));
        self.remember(Saved {
            name: String::new(),
            mode,
            left: a.display().to_string(),
            right: b.display().to_string(),
            base: String::new(),
        });
        self.open_sides(Side::file(a), Side::file(b), mode);
    }

    /// 两边可以是带名字的临时文件（Git 的某个版本、编辑器里没保存的内容）
    pub fn open_sides(&mut self, a: Side, b: Side, mode: Mode) {
        // 已经开着同样的比较就切过去
        if let Some(i) = self.tabs.iter().position(|t| t.paths().as_ref() == Some(&(a.path.clone(), b.path.clone()))) {
            self.active = Some(i);
            return;
        }
        let tab = match mode {
            Mode::Folder => {
                if !(a.path.is_dir() && b.path.is_dir()) {
                    self.events.push(CmpEvt::Toast(t("文件夹只能和文件夹比较").into(), WARN()));
                    return;
                }
                Tab::Folder(folder::FolderCmp::new(&a.path, &b.path, &self.s))
            }
            Mode::Hex => Tab::Hex(hex::HexTab::new(a, b, self.s.align_hex)),
            Mode::Table => Tab::Table(table::TableTab::new(&a.path, &b.path)),
            Mode::Merge | Mode::Text => Tab::Text(text::TextCmp::new(a, b, &self.s.opts)),
        };
        self.tabs.push(tab);
        self.active = Some(self.tabs.len() - 1);
    }

    /// 三方合并：结果写到 out
    pub fn open_merge(&mut self, base: Side, l: Side, r: Side, out: &Path, git_root: Option<PathBuf>) {
        match merge::MergeTab::new(base, l, r, out, git_root) {
            Ok(m) => {
                self.tabs.push(Tab::Merge(m));
                self.active = Some(self.tabs.len() - 1);
            }
            Err(e) => self.events.push(CmpEvt::Toast(e, ERR())),
        }
    }

    /// Git 合并冲突的文件：取暂存区里的 :1 基准、:2 我们的、:3 他们的
    pub fn open_conflict(&mut self, root: &Path, rel: &str) {
        let stage = |n: u8| crate::git::git(root, &["show", &format!(":{n}:{rel}")]).ok();
        let name = name_of(Path::new(rel));
        let target = root.join(rel);
        let (base, ours, theirs) = match (stage(1), stage(2), stage(3)) {
            (b, Some(o), Some(th)) => (b.unwrap_or_default(), o, th),
            _ => {
                // 没有暂存区信息：从冲突标记里拆
                let text = std::fs::read(&target).ok().and_then(|b| crate::cindex::decode(&b)).map(|x| x.0).unwrap_or_default();
                let (lines, _) = compare::diff::split_lines(&text);
                match compare::merge::split_conflict_markers(&lines) {
                    Some((o, b, th)) => (b.join("\n"), o.join("\n"), th.join("\n")),
                    None => {
                        self.events.push(CmpEvt::Toast(t("这个文件里没有找到冲突").into(), WARN()));
                        return;
                    }
                }
            }
        };
        let tb = temp_file(&format!("BASE-{name}"), base.as_bytes());
        let to = temp_file(&format!("OURS-{name}"), ours.as_bytes());
        let tt = temp_file(&format!("THEIRS-{name}"), theirs.as_bytes());
        self.open_merge(
            Side::labeled(&tb, &format!("{name} ({})", t("基准"))),
            Side::labeled(&to, &format!("{name} ({})", t("我们的"))),
            Side::labeled(&tt, &format!("{name} ({})", t("他们的"))),
            &target,
            Some(root.to_path_buf()),
        );
    }

    fn remember(&mut self, mut s: Saved) {
        s.name = format!("{} ↔ {}", name_of(Path::new(&s.left)), name_of(Path::new(&s.right)));
        self.s.recent.retain(|x| !(x.left == s.left && x.right == s.right && x.mode == s.mode));
        self.s.recent.insert(0, s);
        self.s.recent.truncate(12);
    }

    /// 已选为左侧、还在等右侧的路径
    pub fn pending_left(&self) -> Option<&str> {
        (!self.home.left.is_empty() && self.home.right.is_empty()).then_some(self.home.left.as_str())
    }

    /// 拖进比较页的文件：一次拖两个直接比较，一个一个拖就先填左边再填右边
    pub fn drop_files(&mut self, paths: Vec<PathBuf>) {
        if paths.len() >= 2 {
            self.open(&paths[0], &paths[1], None);
            return;
        }
        let Some(p) = paths.into_iter().next() else { return };
        self.active = None;
        if self.home.left.is_empty() || !self.home.right.is_empty() {
            self.home.left = p.display().to_string();
            self.home.right.clear();
        } else {
            self.home.right = p.display().to_string();
            let (a, b) = (PathBuf::from(&self.home.left), PathBuf::from(&self.home.right));
            self.open(&a, &b, None);
        }
    }

    // ------------------------------------------------------------ 界面

    pub fn ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| self.ui_in(ui));
    }

    pub fn ui_in(&mut self, ui: &mut egui::Ui) {
        if let Some(i) = self.active {
            if i >= self.tabs.len() {
                self.active = self.tabs.len().checked_sub(1);
            }
        }
        egui::TopBottomPanel::top("cmp-tabs")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 0 }))
            .show_separator_line(true)
            .show_inside(ui, |ui| self.tab_strip(ui));
        let mut outs = vec![];
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG0()))
            .show_inside(ui, |ui| match self.active.and_then(|i| self.tabs.get_mut(i)) {
                None => self.home_ui(ui),
                Some(Tab::Text(x)) => outs = x.ui(ui, &mut self.s),
                Some(Tab::Folder(x)) => outs = x.ui(ui, &mut self.s),
                Some(Tab::Hex(x)) => outs = x.ui(ui, &mut self.s),
                Some(Tab::Merge(x)) => outs = x.ui(ui),
                Some(Tab::Table(x)) => outs = x.ui(ui),
            });
        for o in outs {
            match o {
                Out::Toast(m, c) => self.events.push(CmpEvt::Toast(m, c)),
                Out::OpenFile(p, l) => self.events.push(CmpEvt::OpenFile(p, l)),
                Out::Compare(a, b, m) => self.open_sides(Side::file(&a), Side::file(&b), m),
            }
        }
        self.close_modal(ui.ctx());
    }

    fn tab_strip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // 首页
            let home = self.active.is_none();
            let (r, resp) = ui.allocate_exact_size(vec2(34.0, 30.0), Sense::click());
            if home {
                ui.painter().rect_filled(r, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, BG0());
            } else if resp.hovered() {
                ui.painter().rect_filled(r.shrink(2.0), CornerRadius::same(5), BG3());
            }
            draw_icon(ui.painter(), Rect::from_center_size(r.center(), vec2(16.0, 16.0)), Icon::Plus, if home { ACCENT() } else { TEXT2() });
            if resp.on_hover_text(t("新的比较")).clicked() {
                self.active = None;
            }
            let mut close = None;
            let mut pick = None;
            egui::ScrollArea::horizontal().id_salt("cmp-tab-scroll").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.iter().enumerate() {
                        let (title, badge, dirty) = tab.title();
                        let on = self.active == Some(i);
                        let font = FontId::proportional(12.5);
                        let g = ui.painter().layout_no_wrap(crate::app::truncate(&title, 48), font, if on { TEXT() } else { TEXT2() });
                        let w = g.size().x + 64.0;
                        let (r, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                        let p = ui.painter();
                        if on {
                            p.rect_filled(r, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, BG0());
                            p.rect_filled(Rect::from_min_size(r.min, vec2(r.width(), 2.0)), 0.0, tab.color());
                        } else if resp.hovered() {
                            p.rect_filled(r.shrink2(vec2(0.0, 2.0)), CornerRadius::same(5), tint(8));
                        }
                        let bc = tab.color();
                        let br = Rect::from_center_size(pos2(r.left() + 16.0, r.center().y), vec2(18.0, 15.0));
                        p.rect_filled(br, CornerRadius::same(3), bc.gamma_multiply(0.16));
                        p.text(br.center(), Align2::CENTER_CENTER, badge, FontId::new(9.5, mono_bold()), bc);
                        p.galley(pos2(r.left() + 30.0, r.center().y - g.size().y / 2.0), g, TEXT());
                        let cr = Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(16.0, 16.0));
                        let over_close = resp.hover_pos().is_some_and(|p| cr.contains(p));
                        if dirty && !over_close {
                            p.circle_filled(cr.center(), 3.5, TEXT2());
                        } else if on || resp.hovered() {
                            if over_close {
                                p.rect_filled(cr, CornerRadius::same(3), BG3());
                            }
                            draw_icon(p, cr.shrink(3.0), Icon::Close, TEXT3());
                        }
                        if resp.clicked() {
                            if over_close {
                                close = Some(i);
                            } else {
                                pick = Some(i);
                            }
                        }
                        if resp.middle_clicked() {
                            close = Some(i);
                        }
                        resp.on_hover_text(&title);
                    }
                });
            });
            if let Some(i) = pick {
                self.active = Some(i);
            }
            if let Some(i) = close {
                if self.tabs[i].title().2 {
                    self.close_ask = Some(i);
                } else {
                    self.close_tab(i);
                }
            }
            // 右边：当前比较的更多操作
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(i) = self.active {
                    self.tab_menu(ui, i);
                }
            });
        });
    }

    fn close_tab(&mut self, i: usize) {
        self.tabs.remove(i);
        self.active = match self.active {
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => (!self.tabs.is_empty()).then(|| a.min(self.tabs.len() - 1)),
            a => a,
        };
    }

    fn close_modal(&mut self, ctx: &egui::Context) {
        let Some(i) = self.close_ask else { return };
        let mut keep = true;
        egui::Modal::new(egui::Id::new("cmp-close")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new(t("关闭前保存修改吗？")).strong().size(15.0));
            ui.add_space(4.0);
            ui.label(RichText::new(self.tabs[i].title().0).color(TEXT2()));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new(t("保存")).color(ON_ACCENT())).fill(ACCENT())).clicked() {
                    let ok = match &mut self.tabs[i] {
                        Tab::Text(x) => x.save_all().is_ok(),
                        Tab::Merge(x) => x.save().is_ok(),
                        _ => true,
                    };
                    if ok {
                        self.close_tab(i);
                    }
                    keep = false;
                }
                if ui.button(t("不保存")).clicked() {
                    self.close_tab(i);
                    keep = false;
                }
                if ui.button(t("取消")).clicked() {
                    keep = false;
                }
            });
        });
        if !keep {
            self.close_ask = None;
        }
    }

    fn tab_menu(&mut self, ui: &mut Ui, i: usize) {
        let r = icon_button(ui, Icon::More, &t("更多"), false);
        let id = egui::Id::new("cmp-tab-menu");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        let paths = self.tabs[i].paths();
        if self.ext.is_none() {
            self.ext = Some(compare::external_tools());
        }
        let ext = self.ext.clone().unwrap_or_default();
        let mut save_sess = false;
        egui::popup::popup_below_widget(ui, id, &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(240.0);
            if let Some((a, b)) = &paths {
                if menu_item(ui, &t("保存为会话"), "") {
                    save_sess = true;
                }
                if ext.is_empty() {
                    ui.add_enabled(false, egui::Label::new(RichText::new(t("没有找到外部比较工具")).small().color(TEXT3())));
                }
                for (name, exe) in &ext {
                    if menu_item(ui, &tf("用 {} 打开", &[name]), "") {
                        if let Err(e) = std::process::Command::new(exe).arg(a).arg(b).spawn() {
                            self.events.push(CmpEvt::Toast(e.to_string(), ERR()));
                        }
                    }
                }
            }
            if menu_item(ui, &t("关闭其他比较"), "") {
                let keep = self.tabs.remove(i);
                self.tabs.retain(|t| t.title().2);
                self.tabs.insert(0, keep);
                self.active = Some(0);
            }
        });
        if save_sess {
            if let Some((a, b)) = paths {
                let mode = match &self.tabs[i] {
                    Tab::Folder(_) => Mode::Folder,
                    Tab::Hex(_) => Mode::Hex,
                    Tab::Table(_) => Mode::Table,
                    _ => Mode::Text,
                };
                let s = Saved {
                    name: format!("{} ↔ {}", name_of(&a), name_of(&b)),
                    mode,
                    left: a.display().to_string(),
                    right: b.display().to_string(),
                    base: String::new(),
                };
                if !self.s.saved.iter().any(|x| x.left == s.left && x.right == s.right && x.mode == s.mode) {
                    self.s.saved.push(s);
                }
                self.events.push(CmpEvt::Toast(t("已保存，在比较页首页可以一键重新打开").into(), OK()));
            }
        }
    }

    // ------------------------------------------------------------ 首页

    fn home_ui(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        hgrad_line(ui.painter(), full.top() + 0.5, full.x_range(), ACCENT().gamma_multiply(0.35));
        // 窄的时候（副页面）贴着两边，不再保证最小宽度
        let w = (full.width() - 80.0).clamp(560.0, 880.0).min(full.width() - 24.0);
        let col = Rect::from_min_size(pos2(full.center().x - w / 2.0, full.top() + 36.0), vec2(w, full.height() - 48.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(col));
        let ui = &mut child;
        egui::ScrollArea::vertical().id_salt("cmp-home").auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new(t("比较与合并")).font(FontId::new(22.0, bold())).color(TEXT()));
            ui.add_space(4.0);
            ui.label(RichText::new(t("选两个文件或文件夹，也可以直接把文件拖进来；会自动选择文本、十六进制、表格或文件夹比较")).color(TEXT3()));
            ui.add_space(16.0);
            let mut go: Option<Option<Mode>> = None;
            egui::Frame::new()
                .fill(BG1())
                .stroke(st(1.0, LINE()))
                .corner_radius(R_CARD)
                .inner_margin(egui::Margin::same(16))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (label, v) in [(t("左侧"), &mut self.home.left), (t("右侧"), &mut self.home.right)] {
                        path_row(ui, label, v);
                        ui.add_space(6.0);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(64.0);
                        if icon_button(ui, Icon::Swap, t("交换左右"), false).clicked() {
                            std::mem::swap(&mut self.home.left, &mut self.home.right);
                        }
                        ui.label(RichText::new(t("三方合并时再填基准：")).small().color(TEXT3()));
                    });
                    ui.add_space(6.0);
                    path_row(ui, t("基准"), &mut self.home.base);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.add_space(64.0);
                        let ready = !self.home.left.trim().is_empty() && !self.home.right.trim().is_empty();
                        let main = egui::Button::new(RichText::new(t("比较")).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(110.0, 32.0));
                        if ui.add_enabled(ready, main).clicked() {
                            go = Some(None);
                        }
                        ui.add_space(8.0);
                        ui.label(RichText::new(t("指定方式：")).small().color(TEXT3()));
                        for (m, name) in [
                            (Mode::Folder, t("文件夹")),
                            (Mode::Text, t("文本")),
                            (Mode::Hex, t("十六进制")),
                            (Mode::Table, t("表格")),
                            (Mode::Merge, t("三方合并")),
                        ] {
                            let en = ready && (m != Mode::Merge || !self.home.base.trim().is_empty());
                            if ui.add_enabled(en, egui::Button::new(RichText::new(name).size(12.5))).clicked() {
                                go = Some(Some(m));
                            }
                        }
                    });
                });
            if let Some(mode) = go {
                let a = PathBuf::from(self.home.left.trim());
                let b = PathBuf::from(self.home.right.trim());
                if !a.exists() || !b.exists() {
                    let miss = if a.exists() { &b } else { &a };
                    self.events.push(CmpEvt::Toast(tf("找不到 {}", &[&miss.display()]), ERR()));
                } else if mode == Some(Mode::Merge) {
                    let base = PathBuf::from(self.home.base.trim());
                    // 结果默认写回左边
                    self.open_merge(Side::file(&base), Side::file(&a), Side::file(&b), &a, None);
                } else {
                    self.open(&a, &b, mode);
                }
            }
            ui.add_space(20.0);
            let mut open: Option<Saved> = None;
            let mut del: Option<usize> = None;
            if !self.s.saved.is_empty() {
                crate::app::eyebrow(ui, t("保存的会话"), &self.s.saved.len().to_string());
                for (i, s) in self.s.saved.iter_mut().enumerate() {
                    if let Some((k, name)) = &mut self.home.rename {
                        if *k == i {
                            let r = ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                            r.request_focus();
                            if r.lost_focus() {
                                if !name.trim().is_empty() {
                                    s.name = name.trim().to_string();
                                }
                                self.home.rename = None;
                            }
                            continue;
                        }
                    }
                    let r = session_row(ui, s, true);
                    if r.clicked() {
                        open = Some(s.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button(t("重命名")).clicked() {
                            self.home.rename = Some((i, s.name.clone()));
                            ui.close_menu();
                        }
                        if ui.button(t("删除")).clicked() {
                            del = Some(i);
                            ui.close_menu();
                        }
                    });
                }
                ui.add_space(14.0);
            }
            if let Some(i) = del {
                self.s.saved.remove(i);
            }
            if !self.s.recent.is_empty() {
                ui.horizontal(|ui| {
                    crate::app::eyebrow(ui, t("最近的比较"), "");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button(t("清空")).clicked() {
                            self.s.recent.clear();
                        }
                    });
                });
                for s in &self.s.recent {
                    if session_row(ui, s, false).clicked() {
                        open = Some(s.clone());
                    }
                }
            }
            if let Some(s) = open {
                let (a, b) = (PathBuf::from(&s.left), PathBuf::from(&s.right));
                if a.exists() && b.exists() {
                    self.open(&a, &b, Some(s.mode));
                } else {
                    self.events.push(CmpEvt::Toast(t("路径已经不存在了").into(), WARN()));
                }
            }
            ui.add_space(20.0);
            ui.label(
                RichText::new(t("也可以在代码页文件树、编辑器标签、Git 页的文件上右键比较；系统右键菜单在 设置 → 常规 里开启"))
                    .small()
                    .color(TEXT3()),
            );
        });
    }
}

fn path_row(ui: &mut Ui, label: &str, v: &mut String) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(56.0, 26.0), egui::Label::new(RichText::new(label).color(TEXT2())));
        let w = ui.available_width() - 72.0;
        ui.add(
            egui::TextEdit::singleline(v)
                .desired_width(w)
                .font(FontId::monospace(12.5))
                .margin(vec2(8.0, 5.0))
                .hint_text(t("路径，或把文件拖到这里")),
        );
        if icon_button(ui, Icon::Files, t("选择文件"), false).clicked() {
            if let Some(p) = rfd::FileDialog::new().pick_file() {
                *v = p.display().to_string();
            }
        }
        if icon_button(ui, Icon::Folder, t("选择文件夹"), false).clicked() {
            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                *v = p.display().to_string();
            }
        }
    });
}

fn mode_badge(m: Mode) -> (&'static str, Color32) {
    match m {
        Mode::Folder => ("D", WARN()),
        Mode::Text => ("T", ACCENT()),
        Mode::Hex => ("0x", VIOLET()),
        Mode::Merge => ("3", ERR()),
        Mode::Table => ("▦", OK()),
    }
}

fn session_row(ui: &mut Ui, s: &Saved, star: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(6), tint(8));
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (b, c) = mode_badge(s.mode);
    let br = Rect::from_center_size(pos2(r.left() + 18.0, r.center().y), vec2(22.0, 18.0));
    p.rect_filled(br, CornerRadius::same(4), c.gamma_multiply(0.16));
    p.text(br.center(), Align2::CENTER_CENTER, b, FontId::new(10.0, mono_bold()), c);
    let name = if star { format!("★ {}", s.name) } else { s.name.clone() };
    p.text(pos2(r.left() + 38.0, r.top() + 13.0), Align2::LEFT_CENTER, name, FontId::proportional(13.0), TEXT());
    let sub = format!("{}  ↔  {}", s.left, s.right);
    let g = p.layout_no_wrap(sub, FontId::monospace(11.0), TEXT3());
    p.with_clip_rect(r.shrink2(vec2(8.0, 0.0))).galley(pos2(r.left() + 38.0, r.top() + 22.0), g, TEXT3());
    resp
}

// ------------------------------------------------------------ 公共小部件

/// 临时文件目录（Git 的某个版本、未保存的编辑内容）
pub fn temp_dir() -> PathBuf {
    crate::data_dir().join("compare-tmp")
}

pub fn temp_file(name: &str, data: &[u8]) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = temp_dir().join(N.fetch_add(1, Ordering::Relaxed).to_string());
    let _ = std::fs::create_dir_all(&dir);
    let p = dir.join(name.replace(['/', '\\', ':'], "_"));
    let _ = std::fs::write(&p, data);
    p
}

/// 上一处 / 下一处按钮，返回 Some(true) = 下一处
pub(crate) fn nav(ui: &mut Ui, n: usize, cur: Option<usize>) -> Option<bool> {
    let mut r = None;
    let has = n > 0;
    if ui.add_enabled(has, egui::Button::new("↑")).on_hover_text(t("上一处  Shift+F7")).clicked() {
        r = Some(false);
    }
    if ui.add_enabled(has, egui::Button::new("↓")).on_hover_text(t("下一处  F7")).clicked() {
        r = Some(true);
    }
    let label = match cur {
        Some(c) if has => format!("{}/{}", c + 1, n),
        _ => tf("{} 处", &[&n]),
    };
    ui.label(RichText::new(label).font(FontId::monospace(11.5)).color(TEXT3()));
    if has && ui.memory(|m| m.focused().is_none()) {
        ui.input(|i| {
            if i.key_pressed(egui::Key::F7) || (i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown)) {
                r = Some(!i.modifiers.shift);
            }
            if i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp) {
                r = Some(false);
            }
        });
    }
    r
}

pub(crate) fn step(cur: Option<usize>, n: usize, next: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    Some(match (cur, next) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
    })
}

/// 一边的标题：文件名、所在目录、未保存的点
pub(crate) fn side_title(ui: &mut Ui, side: &Side, dirty: bool, width: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let p = ui.painter();
    crate::codeview::file_badge(p, pos2(r.left() + 10.0, r.center().y), &name_of(&side.path));
    let g = p.layout_no_wrap(side.label.clone(), FontId::new(13.0, bold()), TEXT());
    let gw = g.size().x;
    p.galley(pos2(r.left() + 22.0, r.center().y - g.size().y / 2.0), g, TEXT());
    let mut x = r.left() + 28.0 + gw;
    if dirty {
        p.circle_filled(pos2(x + 2.0, r.center().y), 3.5, WARN());
        x += 12.0;
    }
    if side.readonly {
        p.text(pos2(x, r.center().y), Align2::LEFT_CENTER, t("只读"), FontId::proportional(10.5), TEXT3());
        x += 30.0;
    }
    let dir = side.path.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let g = p.layout_no_wrap(dir, FontId::monospace(10.5), TEXT3());
    p.with_clip_rect(Rect::from_min_max(pos2(x, r.top()), r.max))
        .galley(pos2(x, r.center().y - g.size().y / 2.0), g, TEXT3());
    ui.interact(r, ui.id().with(("side-title", &side.path)), Sense::hover()).on_hover_text(side.path.display().to_string());
}

/// 忽略选项的一排开关；有变化返回 true
pub(crate) fn opts_chips(ui: &mut Ui, o: &mut Opts) -> bool {
    let before = *o;
    toggle_chip(ui, &mut o.ws, t("空白"), t("忽略空格和 Tab 的差别"));
    toggle_chip(ui, &mut o.case, t("大小写"), t("忽略大小写"));
    toggle_chip(ui, &mut o.comments, t("注释"), t("忽略注释（C 的 // /* */、脚本的 #、ini 的 ;）"));
    toggle_chip(ui, &mut o.blank, t("空行"), t("忽略空行"));
    *o != before
}

/// 右侧的概览条：每处差异的位置，点一下跳过去
pub(crate) fn overview(ui: &mut Ui, rect: Rect, n: usize, marks: &[(usize, Color32)], view: Option<(usize, usize)>) -> Option<usize> {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, BG1());
    let n = n.max(1) as f32;
    for &(i, c) in marks {
        let y = rect.top() + rect.height() * i as f32 / n;
        p.rect_filled(Rect::from_min_size(pos2(rect.left() + 2.0, y), vec2(rect.width() - 4.0, (rect.height() / n).max(2.0))), 0.0, c);
    }
    if let Some((a, b)) = view {
        let y0 = rect.top() + rect.height() * a as f32 / n;
        let y1 = rect.top() + rect.height() * b as f32 / n;
        p.rect_stroke(
            Rect::from_min_max(pos2(rect.left(), y0), pos2(rect.right(), y1.max(y0 + 4.0))),
            0.0,
            st(1.0, TEXT3()),
            egui::StrokeKind::Inside,
        );
    }
    let resp = ui.interact(rect, ui.id().with("overview"), Sense::click_and_drag());
    if resp.clicked() || resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            return Some(((pp.y - rect.top()) / rect.height() * n) as usize);
        }
    }
    None
}

pub(crate) fn mono() -> FontId {
    FontId::new(12.5, FontFamily::Monospace)
}

/// 生成报告并用浏览器打开
pub(crate) fn save_report(html: String, name: &str, outs: &mut Vec<Out>) {
    if let Some(p) = rfd::FileDialog::new().set_file_name(format!("{name}.html")).add_filter("HTML", &["html"]).save_file() {
        match std::fs::write(&p, html) {
            Ok(_) => {
                crate::app::open_url(&p.display().to_string());
                outs.push(Out::Toast(tf("报告已保存：{}", &[&p.display()]), OK()));
            }
            Err(e) => outs.push(Out::Toast(e.to_string(), ERR())),
        }
    }
}

/// 从 Git 里取某个版本的文件内容（原样字节，二进制也行），比如 "HEAD:./main.c"
pub fn git_blob(dir: &Path, spec: &str) -> Option<Vec<u8>> {
    let mut c = std::process::Command::new("git");
    c.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(dir).args(["show", spec]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().ok()?;
    out.status.success().then_some(out.stdout)
}

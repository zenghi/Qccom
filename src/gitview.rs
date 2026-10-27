//! Git 视图：本地 / 远程分支和标签、提交图谱、提交详情、文件差异、工作区更改，
//! 以及拉取、切换分支。所有 git 命令都在后台线程里跑。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, epaint::CubicBezierShape, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Rect, RichText, Sense, Ui};
use serde::{Deserialize, Serialize};

use crate::app::{draw_icon, hgrad_line, icon_button, Icon};
use crate::codeview::{chevron, file_badge, tree_row};
use crate::git::{self, Branch, Commit, CommitInfo, DiffKind, DiffLine, GraphRow, RefKind, Status, StatusFile};
use crate::highlight::{self, Lang};
use crate::i18n::{t as tr, tf};
use crate::theme::*;

const ROW_H: f32 = 28.0;
const LANE_W: f32 = 14.0;
const MAX_LANES: usize = 14;
const PAGE: usize = 1500;

const LANE_COLORS: [Color32; 8] = [
    ACCENT,
    VIOLET,
    OK,
    WARN,
    Color32::from_rgb(0xf4, 0x72, 0xb6),
    DEBUG,
    Color32::from_rgb(0xfb, 0x92, 0x3c),
    Color32::from_rgb(0x2d, 0xd4, 0xbf),
];

fn lane_col(l: usize) -> Color32 {
    LANE_COLORS[l % LANE_COLORS.len()]
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct GitSettings {
    pub root: String,
    pub side_width: f32,
    pub detail_width: f32,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            root: String::new(),
            side_width: 270.0,
            detail_width: 420.0,
        }
    }
}

pub enum GitEvt {
    Toast(String, Color32),
    /// 在代码查看里打开文件（行号从 0 开始）
    OpenFile(PathBuf, usize),
}

enum Msg {
    Branches(Result<Vec<Branch>, String>),
    Log(u64, Result<Vec<Commit>, String>),
    Info(String, Result<CommitInfo, String>),
    Diff(u64, Result<String, String>),
    Status(Result<Status, String>),
    Op(String, Result<String, String>),
}

#[derive(Clone, PartialEq)]
enum Sel {
    None,
    Work,
    Commit(String),
}

struct DiffView {
    title: String,
    path: String,
    lang: Lang,
    lines: Vec<DiffLine>,
    adds: usize,
    dels: usize,
    loading: bool,
    err: Option<String>,
    max_chars: usize,
}

struct Confirm {
    title: String,
    body: String,
    ok: String,
    args: Vec<String>,
    label: String,
}

pub struct GitView {
    pub s: GitSettings,
    ctx: egui::Context,
    root: Option<PathBuf>,
    err: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    branches: Vec<Branch>,
    status: Option<Status>,
    commits: Vec<Commit>,
    rows: Vec<GraphRow>,
    graph_w: usize,
    log_gen: u64,
    loading_log: bool,
    limit: usize,
    scope: Option<String>,
    query: String,
    query_sent: String,
    query_at: Option<Instant>,
    sel: Sel,
    info: Option<CommitInfo>,
    diff: Option<DiffView>,
    diff_gen: u64,
    file_sel: Option<String>,
    open_local: bool,
    open_remote: bool,
    open_tags: bool,
    branch_filter: String,
    confirm: Option<Confirm>,
    busy: Option<String>,
    scroll_to_sel: bool,
    activated: bool,
    pub events: Vec<GitEvt>,
}

impl GitView {
    pub fn new(s: GitSettings, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            s,
            ctx,
            root: None,
            err: None,
            tx,
            rx,
            branches: vec![],
            status: None,
            commits: vec![],
            rows: vec![],
            graph_w: 1,
            log_gen: 0,
            loading_log: false,
            limit: PAGE,
            scope: None,
            query: String::new(),
            query_sent: String::new(),
            query_at: None,
            sel: Sel::None,
            info: None,
            diff: None,
            diff_gen: 0,
            file_sel: None,
            open_local: true,
            open_remote: true,
            open_tags: false,
            branch_filter: String::new(),
            confirm: None,
            busy: None,
            scroll_to_sel: false,
            activated: false,
            events: vec![],
        }
    }

    /// 第一次进入 Git 页面：打开上次的仓库，没有的话用代码查看里的文件夹
    pub fn activate(&mut self, code_root: Option<&Path>) {
        if self.activated {
            // 再次进入时刷新一下工作区状态
            if self.root.is_some() {
                self.load_status();
                self.load_branches();
            }
            return;
        }
        self.activated = true;
        let saved = (!self.s.root.is_empty()).then(|| PathBuf::from(&self.s.root));
        if let Some(p) = saved.or_else(|| code_root.map(Path::to_path_buf)) {
            self.open_root(&p);
            if self.root.is_none() && code_root.is_some() && self.s.root.is_empty() {
                self.err = None; // 代码文件夹不是仓库时安静地停在欢迎页
            }
        }
    }

    pub fn open_root(&mut self, p: &Path) {
        match git::find_root(p) {
            Ok(r) => {
                self.s.root = r.to_string_lossy().into();
                self.root = Some(r);
                self.err = None;
                self.scope = None;
                self.query.clear();
                self.query_sent.clear();
                self.sel = Sel::None;
                self.info = None;
                self.diff = None;
                self.commits.clear();
                self.rows.clear();
                self.limit = PAGE;
                self.refresh();
            }
            Err(e) => {
                self.err = Some(if e.contains("not a git repository") {
                    tf("「{}」不是 Git 仓库", &[&p.display()])
                } else {
                    e
                });
            }
        }
    }

    fn pick_folder(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_title(t("打开 Git 仓库")).pick_folder() {
            self.open_root(&p);
        }
    }

    fn spawn(&self, f: impl FnOnce(PathBuf) -> Msg + Send + 'static) {
        let Some(root) = self.root.clone() else { return };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::Builder::new()
            .name("git".into())
            .spawn(move || {
                let _ = tx.send(f(root));
                ctx.request_repaint();
            })
            .ok();
    }

    fn refresh(&mut self) {
        self.load_branches();
        self.load_status();
        self.load_log();
        if let Sel::Commit(h) = self.sel.clone() {
            self.load_info(&h);
        }
    }

    fn load_branches(&self) {
        self.spawn(|r| Msg::Branches(git::branches(&r)));
    }

    fn load_status(&self) {
        self.spawn(|r| Msg::Status(git::status(&r)));
    }

    fn load_log(&mut self) {
        self.log_gen += 1;
        self.loading_log = true;
        let (g, scope, limit, q) = (self.log_gen, self.scope.clone(), self.limit, self.query_sent.clone());
        self.spawn(move |r| Msg::Log(g, git::log(&r, scope.as_deref(), limit, &q)));
    }

    fn load_info(&self, hash: &str) {
        let h = hash.to_string();
        self.spawn(move |r| Msg::Info(h.clone(), git::commit_info(&r, &h)));
    }

    fn select_commit(&mut self, hash: &str) {
        if self.sel == Sel::Commit(hash.into()) {
            return;
        }
        self.sel = Sel::Commit(hash.into());
        self.info = None;
        self.diff = None;
        self.file_sel = None;
        self.load_info(hash);
    }

    fn open_diff(&mut self, title: String, path: String, job: impl FnOnce(PathBuf) -> Result<String, String> + Send + 'static) {
        self.diff_gen += 1;
        let g = self.diff_gen;
        self.file_sel = Some(path.clone());
        self.diff = Some(DiffView {
            title,
            lang: Lang::from_path(Path::new(&path)),
            path,
            lines: vec![],
            adds: 0,
            dels: 0,
            loading: true,
            err: None,
            max_chars: 0,
        });
        self.spawn(move |r| Msg::Diff(g, job(r)));
    }

    fn run_op(&mut self, label: String, args: Vec<String>) {
        self.busy = Some(label.clone());
        self.spawn(move |r| {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            Msg::Op(label, git::git(&r, &a))
        });
    }

    fn toast(&mut self, m: impl Into<String>, c: Color32) {
        self.events.push(GitEvt::Toast(m.into(), c));
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Branches(Ok(b)) => self.branches = b,
                Msg::Branches(Err(e)) | Msg::Status(Err(e)) => self.err = Some(e),
                Msg::Status(Ok(s)) => self.status = Some(s),
                Msg::Log(g, r) if g == self.log_gen => {
                    self.loading_log = false;
                    match r {
                        Ok(c) => {
                            self.rows = git::graph(&c);
                            self.graph_w = self.rows.iter().map(|r| r.width).max().unwrap_or(1).clamp(1, MAX_LANES);
                            self.commits = c;
                            if self.sel == Sel::None {
                                if let Some(c) = self.commits.first() {
                                    let h = c.hash.clone();
                                    self.select_commit(&h);
                                }
                            }
                        }
                        Err(e) => self.err = Some(e),
                    }
                }
                Msg::Log(..) => {}
                Msg::Info(h, r) => {
                    if self.sel == Sel::Commit(h) {
                        match r {
                            Ok(i) => self.info = Some(i),
                            Err(e) => self.toast(e, ERR),
                        }
                    }
                }
                Msg::Diff(g, r) if g == self.diff_gen => {
                    if let Some(d) = self.diff.as_mut() {
                        d.loading = false;
                        match r {
                            Ok(text) => {
                                d.lines = git::parse_diff(&text)
                                    .into_iter()
                                    .filter(|l| {
                                        !(l.kind == DiffKind::Meta && (l.text.starts_with("diff --git") || l.text.starts_with("index ") || l.text.starts_with("--- ") || l.text.starts_with("+++ ")))
                                    })
                                    .collect();
                                d.adds = d.lines.iter().filter(|l| l.kind == DiffKind::Add).count();
                                d.dels = d.lines.iter().filter(|l| l.kind == DiffKind::Del).count();
                                d.max_chars = d.lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
                            }
                            Err(e) => d.err = Some(e),
                        }
                    }
                }
                Msg::Diff(..) => {}
                Msg::Op(label, r) => {
                    self.busy = None;
                    match r {
                        Ok(_) => self.toast(tf("{} 完成", &[&label]), OK),
                        Err(e) => self.toast(tf("{} 失败：{}", &[&label, &friendly_err(&e)]), ERR),
                    }
                    self.refresh();
                }
            }
        }
        // 搜索框停止输入 350ms 后再查
        if let Some(at) = self.query_at {
            if at.elapsed().as_millis() > 350 {
                self.query_at = None;
                if self.query != self.query_sent {
                    self.query_sent = self.query.clone();
                    self.limit = PAGE;
                    self.load_log();
                }
            } else {
                self.ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }

    fn head_name(&self) -> String {
        self.branches
            .iter()
            .find(|b| b.head)
            .map(|b| b.name.clone())
            .unwrap_or_else(|| self.status.as_ref().map(|s| s.branch.split("...").next().unwrap_or("").to_string()).unwrap_or_default())
    }

    fn dirty_count(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.files.len())
    }

    fn ask_checkout(&mut self, b: &Branch) {
        let (args, target): (Vec<String>, String) = match b.kind {
            RefKind::Local => (vec!["switch".into(), b.name.clone()], b.name.clone()),
            RefKind::Remote => {
                let local = b.name.split_once('/').map(|x| x.1).unwrap_or(&b.name).to_string();
                if self.branches.iter().any(|x| x.kind == RefKind::Local && x.name == local) {
                    (vec!["switch".into(), local.clone()], local)
                } else {
                    (vec!["switch".into(), "-c".into(), local.clone(), "--track".into(), b.name.clone()], local)
                }
            }
            RefKind::Tag => (vec!["switch".into(), "--detach".into(), b.name.clone()], b.name.clone()),
        };
        let mut body = tf("将当前分支切换到「{}」。", &[&target]);
        if b.kind == RefKind::Remote {
            body.push('\n');
            body.push_str(&tf("会创建（或使用已有的）本地分支 {} 并跟踪 {}。", &[&target, &b.name]));
        }
        if b.kind == RefKind::Tag {
            body.push('\n');
            body.push_str(&t("标签会以“分离头指针”方式检出，适合查看旧版本代码。"));
        }
        let n = self.dirty_count();
        if n > 0 {
            body.push_str("\n\n");
            body.push_str(&tf("工作区有 {} 个未提交的更改。git 会尽量把它们带过去；如果有冲突会拒绝切换，不会丢失修改。", &[&n]));
        }
        self.confirm = Some(Confirm {
            title: t("切换分支"),
            body,
            ok: t("切换"),
            args,
            label: tf("切换到 {}", &[&target]),
        });
    }

    // ------------------------------------------------------------ 界面

    pub fn ui(&mut self, ctx: &egui::Context) {
        self.poll();
        if self.root.is_none() {
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0)).show(ctx, |ui| self.welcome(ui));
            return;
        }
        egui::SidePanel::left("git-side")
            .frame(egui::Frame::new().fill(BG1))
            .default_width(self.s.side_width)
            .width_range(220.0..=460.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.s.side_width = ui.available_width();
                self.side_panel(ui);
            });
        if self.sel != Sel::None {
            egui::SidePanel::right("git-detail")
                .frame(egui::Frame::new().fill(BG1))
                .default_width(self.s.detail_width)
                .width_range(300.0..=760.0)
                .resizable(true)
                .show(ctx, |ui| {
                    self.s.detail_width = ui.available_width();
                    match self.sel.clone() {
                        Sel::Work => self.work_panel(ui),
                        Sel::Commit(h) => self.commit_panel(ui, &h),
                        Sel::None => {}
                    }
                });
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0)).show(ctx, |ui| {
            if self.diff.is_some() {
                self.diff_view(ui);
            } else {
                self.commit_list(ui);
            }
        });
        self.confirm_modal(ctx);
        if ctx.input(|i| i.key_pressed(Key::Escape)) && ctx.memory(|m| m.focused().is_none()) {
            if self.confirm.is_some() {
                self.confirm = None;
            } else if self.diff.is_some() {
                self.diff = None;
                self.file_sel = None;
            }
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        let r = ui.max_rect();
        let p = ui.painter();
        hgrad_line(p, r.top() + 0.5, r.x_range(), VIOLET.gamma_multiply(0.4));
        let c = r.center() - vec2(0.0, 90.0);
        let ic = Rect::from_center_size(c - vec2(0.0, 40.0), vec2(56.0, 56.0));
        p.rect(ic, CornerRadius::same(14), BG2, st(1.0, VIOLET.gamma_multiply(0.4)), egui::StrokeKind::Inside);
        draw_icon(p, ic.shrink(12.0), Icon::Branch, VIOLET);
        p.text(c + vec2(0.0, 14.0), Align2::CENTER_CENTER, t("Git 分支"), FontId::new(22.0, bold()), TEXT);
        p.text(
            c + vec2(0.0, 42.0),
            Align2::CENTER_CENTER,
            t("查看各分支的提交图谱、每次提交改了什么、工作区还有哪些没提交"),
            FontId::proportional(13.0),
            TEXT3,
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(r.center() + vec2(0.0, 80.0), vec2(420.0, 160.0))));
        let ui = &mut child;
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new(t("打开仓库文件夹")).color(BG0).strong())
                        .fill(VIOLET)
                        .corner_radius(R_WIDGET)
                        .min_size(vec2(200.0, 36.0)),
                )
                .clicked()
            {
                self.pick_folder();
            }
            if let Some(e) = &self.err {
                ui.add_space(14.0);
                ui.label(RichText::new(e).color(WARN));
            }
        });
    }

    // ------------------------------------------------------------ 左侧：分支

    fn side_panel(&mut self, ui: &mut Ui) {
        let root = self.root.clone().unwrap_or_default();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            ui.label(RichText::new(name.to_uppercase()).font(FontId::new(11.5, bold())).color(TEXT2))
                .on_hover_text(root.to_string_lossy());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if icon_button(ui, Icon::Folder, &t("打开其他仓库"), false).clicked() {
                    self.pick_folder();
                }
                if icon_button(ui, Icon::Refresh, &t("刷新"), false).clicked() {
                    self.refresh();
                }
                let busy = self.busy.is_some();
                let r = icon_button(ui, Icon::Download, &t("拉取远程（git fetch --all --prune）"), busy);
                if r.clicked() && !busy {
                    self.run_op(t("拉取"), vec!["fetch".into(), "--all".into(), "--prune".into()]);
                }
            });
        });
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top() + 3.0, st(1.0, LINE));
        ui.add_space(10.0);

        // 当前分支卡片
        let head = self.head_name();
        let hb = self.branches.iter().find(|b| b.head).cloned();
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let w = ui.available_width() - 10.0;
            let (rect, _) = ui.allocate_exact_size(vec2(w, 52.0), Sense::hover());
            let p = ui.painter();
            p.rect(rect, R_CARD, BG2, st(1.0, LINE), egui::StrokeKind::Inside);
            p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 10.0), vec2(2.0, 32.0)), 1.0, VIOLET);
            draw_icon(p, Rect::from_center_size(pos2(rect.left() + 20.0, rect.top() + 18.0), vec2(14.0, 14.0)), Icon::Branch, VIOLET);
            p.text(
                pos2(rect.left() + 34.0, rect.top() + 18.0),
                Align2::LEFT_CENTER,
                if head.is_empty() { "HEAD" } else { &head },
                FontId::new(13.5, mono_bold()),
                TEXT,
            );
            let mut note = match &hb {
                Some(b) if !b.upstream.is_empty() => {
                    if b.gone {
                        tf("上游 {} 已删除", &[&b.upstream])
                    } else if b.ahead == 0 && b.behind == 0 {
                        tf("与 {} 同步", &[&b.upstream])
                    } else {
                        format!("{}  ↑{} ↓{}", b.upstream, b.ahead, b.behind)
                    }
                }
                Some(_) => t("没有上游分支"),
                None => t("分离头指针"),
            };
            let n = self.dirty_count();
            if n > 0 {
                note = format!("{note}  ·  {}", tf("{} 处更改", &[&n]));
            }
            p.text(pos2(rect.left() + 34.0, rect.top() + 37.0), Align2::LEFT_CENTER, note, FontId::proportional(11.5), TEXT3);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.add_sized(
                vec2(ui.available_width() - 10.0, 24.0),
                egui::TextEdit::singleline(&mut self.branch_filter).hint_text(t("筛选分支…")).margin(vec2(6.0, 3.0)),
            );
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // 工作区更改 / 全部提交
            let n = self.dirty_count();
            let r = nav_row(
                ui,
                Icon::Edit,
                &t("工作区更改"),
                (n > 0).then(|| n.to_string()),
                self.sel == Sel::Work,
                if n > 0 { WARN } else { TEXT3 },
            );
            if r.clicked() {
                self.sel = Sel::Work;
                self.diff = None;
                self.file_sel = None;
                self.load_status();
            }
            let r = nav_row(ui, Icon::Commit, &t("全部分支的提交"), None, self.scope.is_none() && self.sel != Sel::Work, ACCENT);
            if r.clicked() && self.scope.is_some() {
                self.scope = None;
                self.limit = PAGE;
                self.load_log();
            }
            ui.add_space(6.0);

            let f = self.branch_filter.to_lowercase();
            let mut act: Option<(Branch, bool)> = None;
            for (kind, title) in [(RefKind::Local, t("本地分支")), (RefKind::Remote, t("远程分支")), (RefKind::Tag, t("标签"))] {
                let list: Vec<&Branch> = self.branches.iter().filter(|b| b.kind == kind && (f.is_empty() || b.name.to_lowercase().contains(&f))).collect();
                let open = match kind {
                    RefKind::Local => &mut self.open_local,
                    RefKind::Remote => &mut self.open_remote,
                    RefKind::Tag => &mut self.open_tags,
                };
                if section_header(ui, &title, list.len(), *open || !f.is_empty()).clicked() {
                    *open = !*open;
                }
                if !*open && f.is_empty() {
                    continue;
                }
                for b in list.iter().take(500) {
                    let r = branch_row(ui, b, self.scope.as_deref() == Some(b.name.as_str()));
                    if r.double_clicked() {
                        act = Some(((*b).clone(), true));
                    } else if r.clicked() {
                        act = Some(((*b).clone(), false));
                    }
                    r.context_menu(|ui| {
                        if !b.head && ui.button(t("切换到此分支…")).clicked() {
                            act = Some(((*b).clone(), true));
                            ui.close_menu();
                        }
                        if ui.button(t("只看此分支的提交")).clicked() {
                            act = Some(((*b).clone(), false));
                            ui.close_menu();
                        }
                        if ui.button(t("复制名称")).clicked() {
                            ui.ctx().copy_text(b.name.clone());
                            ui.close_menu();
                        }
                    });
                }
                ui.add_space(4.0);
            }
            if let Some((b, checkout)) = act {
                if checkout && !b.head {
                    self.ask_checkout(&b);
                } else {
                    self.scope = Some(b.name.clone());
                    self.limit = PAGE;
                    self.load_log();
                    self.select_commit_by_ref(&b);
                }
            }
        });
    }

    fn select_commit_by_ref(&mut self, b: &Branch) {
        // 分支短哈希 → 等日志加载后自动选中第一条
        let _ = b;
        self.sel = Sel::None;
        self.info = None;
        self.diff = None;
    }

    // ------------------------------------------------------------ 中间：提交图谱

    fn commit_list(&mut self, ui: &mut Ui) {
        // 顶栏
        let bar = ui.available_rect_before_wrap();
        let bar = Rect::from_min_size(bar.min, vec2(bar.width(), 44.0));
        ui.painter().rect_filled(bar, 0.0, BG1);
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, st(1.0, LINE));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(14.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        {
            let ui = &mut child;
            ui.label(RichText::new(t("提交历史")).font(FontId::new(14.0, bold())).color(TEXT));
            ui.add_space(6.0);
            if let Some(s) = self.scope.clone() {
                let r = egui::Frame::new()
                    .fill(VIOLET.gamma_multiply(0.14))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin { left: 8, right: 4, top: 2, bottom: 2 })
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(&s).font(FontId::new(11.5, FontFamily::Monospace)).color(VIOLET));
                        ui.add(egui::Button::new(RichText::new("×").color(VIOLET)).frame(false)).on_hover_text(t("显示全部分支")).clicked()
                    });
                if r.inner {
                    self.scope = None;
                    self.limit = PAGE;
                    self.load_log();
                }
            } else {
                ui.label(RichText::new(t("全部分支")).size(12.0).color(TEXT3));
            }
            let n = self.commits.len();
            ui.label(RichText::new(if self.loading_log { t("加载中…") } else { tf("{} 个提交", &[&n]) }).size(11.5).color(TEXT3));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if search_box(ui, &mut self.query, &t("搜索提交说明…"), 260.0) {
                    self.query_at = Some(Instant::now());
                }
            });
        }
        ui.add_space(44.0);

        if self.commits.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(if self.loading_log {
                        t("正在读取提交…")
                    } else if self.query_sent.is_empty() {
                        t("没有提交")
                    } else {
                        t("没有匹配的提交")
                    })
                    .color(TEXT3),
                );
            });
            return;
        }

        // 键盘上下切换提交
        let sel_idx = match &self.sel {
            Sel::Commit(h) => self.commits.iter().position(|c| &c.hash == h),
            _ => None,
        };
        if ui.ctx().memory(|m| m.focused().is_none()) && self.confirm.is_none() {
            let (up, down) = ui.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown)));
            let next = match (sel_idx, up, down) {
                (Some(i), true, _) if i > 0 => Some(i - 1),
                (Some(i), _, true) if i + 1 < self.commits.len() => Some(i + 1),
                (None, _, true) => Some(0),
                _ => None,
            };
            if let Some(k) = next {
                let h = self.commits[k].hash.clone();
                self.select_commit(&h);
                self.scroll_to_sel = true;
            }
        }

        let now = now_secs();
        let gw = 14.0 + self.graph_w as f32 * LANE_W;
        let mut clicked: Option<usize> = None;
        let mut menu: Option<(usize, u8)> = None;
        let head_hash = self.branches.iter().find(|b| b.head).map(|b| b.hash.clone()).unwrap_or_default();
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("git-log", self.log_gen));
        if self.scroll_to_sel {
            self.scroll_to_sel = false;
            if let Some(i) = match &self.sel {
                Sel::Commit(h) => self.commits.iter().position(|c| &c.hash == h),
                _ => None,
            } {
                let vh = ui.available_height();
                let off = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new("git-log-off"))).unwrap_or(0.0);
                let y = i as f32 * ROW_H;
                if y < off {
                    sa = sa.vertical_scroll_offset(y);
                } else if y + ROW_H > off + vh {
                    sa = sa.vertical_scroll_offset(y + ROW_H - vh);
                }
            }
        }
        let total = self.commits.len();
        ui.spacing_mut().item_spacing.y = 0.0; // 行之间不留缝，图谱的线才是连续的
        let out = sa.show_rows(ui, ROW_H, total, |ui, range| {
            let w = ui.available_width();
            for i in range.clone() {
                let c = &self.commits[i];
                let g = &self.rows[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
                let p = ui.painter();
                let selected = matches!(&self.sel, Sel::Commit(h) if h == &c.hash);
                if selected {
                    p.rect_filled(rect, 0.0, ACCENT.gamma_multiply(0.10));
                    p.rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), 0.0, ACCENT);
                } else if resp.hovered() {
                    p.rect_filled(rect, 0.0, Color32::from_white_alpha(6));
                }
                draw_graph_row(p, rect, g, c.parents.len() > 1, c.hash.starts_with(&head_hash) && !head_hash.is_empty());

                // 右侧三列：作者、时间、哈希
                let mono = FontId::new(11.5, FontFamily::Monospace);
                let hx = rect.right() - 12.0;
                p.text(pos2(hx, rect.center().y), Align2::RIGHT_CENTER, &c.hash[..7.min(c.hash.len())], mono.clone(), TEXT3);
                let tx = hx - 72.0;
                p.text(pos2(tx, rect.center().y), Align2::RIGHT_CENTER, relative(now, c.time), FontId::proportional(11.5), TEXT3);
                let ax = tx - 92.0;
                let author_r = Rect::from_min_max(pos2(ax - 110.0, rect.top()), pos2(ax, rect.bottom()));
                let ap = p.with_clip_rect(author_r);
                avatar(&ap, pos2(author_r.left() + 9.0, rect.center().y), &c.author, 8.0);
                ap.text(pos2(author_r.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, &c.author, FontId::proportional(12.0), TEXT2);

                // 引用标签 + 提交说明
                let sub_r = Rect::from_min_max(pos2(rect.left() + gw, rect.top()), pos2(author_r.left() - 12.0, rect.bottom()));
                let sp = p.with_clip_rect(sub_r);
                let mut x = sub_r.left();
                for r in &c.refs {
                    x += ref_pill(&sp, pos2(x, rect.center().y), r) + 5.0;
                }
                let col = if selected {
                    TEXT
                } else if c.parents.len() > 1 {
                    TEXT2
                } else {
                    LOG_TEXT
                };
                sp.text(pos2(x + 2.0, rect.center().y), Align2::LEFT_CENTER, &c.subject, FontId::proportional(13.0), col);

                if resp.clicked() {
                    clicked = Some(i);
                }
                resp.context_menu(|ui| {
                    if ui.button(t("复制提交哈希")).clicked() {
                        menu = Some((i, 0));
                        ui.close_menu();
                    }
                    if ui.button(t("复制提交说明")).clicked() {
                        menu = Some((i, 1));
                        ui.close_menu();
                    }
                    if ui.button(t("检出此提交（分离头指针）…")).clicked() {
                        menu = Some((i, 2));
                        ui.close_menu();
                    }
                });
            }
            // 滚到底自动加载更多
            if range.end >= total && total >= self.limit && !self.loading_log {
                self.limit += PAGE;
                self.load_log();
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("git-log-off"), out.state.offset.y));
        if let Some(i) = clicked {
            let h = self.commits[i].hash.clone();
            self.select_commit(&h);
        }
        if let Some((i, what)) = menu {
            let c = &self.commits[i];
            match what {
                0 => {
                    ui.ctx().copy_text(c.hash.clone());
                    self.toast(t("已复制"), OK);
                }
                1 => {
                    ui.ctx().copy_text(c.subject.clone());
                    self.toast(t("已复制"), OK);
                }
                _ => {
                    let short = c.hash[..8.min(c.hash.len())].to_string();
                    self.confirm = Some(Confirm {
                        title: t("检出提交"),
                        body: tf("把工作区切换到提交 {}（分离头指针），适合查看或调试旧版本。之后可以在左侧双击任意分支切回去。", &[&short]),
                        ok: t("检出"),
                        args: vec!["switch".into(), "--detach".into(), c.hash.clone()],
                        label: tf("检出 {}", &[&short]),
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------ 右侧：提交详情

    fn commit_panel(&mut self, ui: &mut Ui, hash: &str) {
        let Some(c) = self.commits.iter().find(|c| c.hash == hash).cloned() else {
            ui.add_space(20.0);
            ui.label(RichText::new(t("加载中…")).color(TEXT3));
            return;
        };
        let now = now_secs();
        let mut open: Option<(String, Option<String>)> = None;
        let mut jump: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: 16,
                    right: 14,
                    top: 14,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.label(RichText::new(&c.subject).font(FontId::new(15.0, bold())).color(TEXT));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                        avatar(ui.painter(), r.center(), &c.author, 16.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(RichText::new(&c.author).strong().color(TEXT));
                            ui.label(RichText::new(format!("{}  ·  {}", absolute(c.time), relative(now, c.time))).size(11.5).color(TEXT3))
                                .on_hover_text(&c.email);
                        });
                    });
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        let short = &c.hash[..10.min(c.hash.len())];
                        if hash_chip(ui, short, ACCENT).on_hover_text(t("点击复制完整哈希")).clicked() {
                            ui.ctx().copy_text(c.hash.clone());
                            self.events.push(GitEvt::Toast(t("已复制"), OK));
                        }
                        for p in &c.parents {
                            let r = hash_chip(ui, &format!("← {}", &p[..7.min(p.len())]), TEXT3).on_hover_text(t("父提交，点击跳转"));
                            if r.clicked() {
                                jump = Some(p.clone());
                            }
                        }
                    });
                    if !c.refs.is_empty() {
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            for r in &c.refs {
                                let (rect, _) = ui.allocate_exact_size(vec2(ref_width(ui, r), 20.0), Sense::hover());
                                ref_pill(ui.painter(), pos2(rect.left(), rect.center().y), r);
                            }
                        });
                    }
                    if let Some(info) = &self.info {
                        let body = info.body.trim();
                        let rest = body.strip_prefix(c.subject.trim()).unwrap_or(body).trim();
                        if !rest.is_empty() {
                            ui.add_space(10.0);
                            egui::Frame::new().fill(BG2).corner_radius(R_WIDGET).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new(rest).size(12.5).color(TEXT2));
                            });
                        }
                    }
                });
            let r = ui.available_rect_before_wrap();
            ui.painter().hline(r.x_range(), r.top(), st(1.0, LINE));
            ui.add_space(8.0);
            let Some(info) = &self.info else {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.spinner();
                    ui.label(RichText::new(t("读取改动的文件…")).color(TEXT3));
                });
                return;
            };
            let (adds, dels): (u32, u32) = info.files.iter().fold((0, 0), |a, f| (a.0 + f.adds.unwrap_or(0), a.1 + f.dels.unwrap_or(0)));
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(RichText::new(tf("改动的文件  {}", &[&info.files.len()])).font(FontId::new(11.5, bold())).color(TEXT2));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(format!("−{dels}")).font(FontId::new(11.5, FontFamily::Monospace)).color(ERR));
                    ui.label(RichText::new(format!("+{adds}")).font(FontId::new(11.5, FontFamily::Monospace)).color(OK));
                });
            });
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for f in &info.files {
                let active = self.file_sel.as_deref() == Some(f.path.as_str());
                let r = file_row(ui, f.status, &f.path, f.adds, f.dels, active);
                if r.clicked() {
                    open = Some((f.path.clone(), f.old_path.clone()));
                }
                if let Some(o) = &f.old_path {
                    r.on_hover_text(format!("{o} → {}", f.path));
                }
            }
        });
        if let Some(h) = jump {
            if self.commits.iter().any(|c| c.hash == h) {
                self.select_commit(&h);
                self.scroll_to_sel = true;
            } else {
                self.toast(t("父提交不在当前列表里（可能被筛选掉了）"), WARN);
            }
        }
        if let Some((path, _)) = open {
            let title = c.subject.clone();
            let h = c.hash.clone();
            let p2 = path.clone();
            self.open_diff(title, path, move |r| git::commit_diff(&r, &h, &p2));
        }
    }

    // ------------------------------------------------------------ 右侧：工作区更改

    fn work_panel(&mut self, ui: &mut Ui) {
        let mut open: Option<StatusFile> = None;
        let mut to_code: Option<String> = None;
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 16,
                right: 14,
                top: 14,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("工作区更改")).font(FontId::new(15.0, bold())).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Refresh, &t("刷新"), false).clicked() {
                            self.load_status();
                        }
                    });
                });
                if let Some(s) = &self.status {
                    ui.label(RichText::new(&s.branch).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3));
                }
            });
        let Some(st_) = self.status.clone() else { return };
        if st_.files.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("✓").size(28.0).color(OK));
                ui.label(RichText::new(t("工作区很干净，没有未提交的更改")).color(TEXT2));
            });
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let groups: [(String, Box<dyn Fn(&StatusFile) -> bool>); 3] = [
                (t("已暂存"), Box::new(|f: &StatusFile| f.x != ' ' && f.x != '?')),
                (t("未暂存"), Box::new(|f: &StatusFile| f.y != ' ' && f.x != '?')),
                (t("未跟踪"), Box::new(|f: &StatusFile| f.x == '?')),
            ];
            for (title, pred) in groups.iter() {
                let list: Vec<&StatusFile> = st_.files.iter().filter(|f| pred(f)).collect();
                if list.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                section_header(ui, title, list.len(), true);
                for f in list {
                    let code = if f.x == '?' {
                        'U'
                    } else if f.y != ' ' {
                        f.y
                    } else {
                        f.x
                    };
                    let active = self.file_sel.as_deref() == Some(f.path.as_str());
                    let r = file_row(ui, code, &f.path, None, None, active);
                    if r.clicked() {
                        open = Some(f.clone());
                    }
                    r.context_menu(|ui| {
                        if ui.button(t("在代码查看中打开")).clicked() {
                            to_code = Some(f.path.clone());
                            ui.close_menu();
                        }
                        if ui.button(t("复制路径")).clicked() {
                            ui.ctx().copy_text(f.path.clone());
                            ui.close_menu();
                        }
                    });
                }
            }
        });
        if let Some(f) = open {
            let path = f.path.clone();
            self.open_diff(t("工作区更改"), path, move |r| git::worktree_diff(&r, &f));
        }
        if let (Some(p), Some(root)) = (to_code, &self.root) {
            self.events.push(GitEvt::OpenFile(root.join(p), 0));
        }
    }

    // ------------------------------------------------------------ 差异

    fn diff_view(&mut self, ui: &mut Ui) {
        let Some(d) = self.diff.as_ref() else { return };
        let bar = ui.available_rect_before_wrap();
        let bar = Rect::from_min_size(bar.min, vec2(bar.width(), 44.0));
        ui.painter().rect_filled(bar, 0.0, BG1);
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, st(1.0, LINE));
        let mut back = false;
        let mut to_code: Option<usize> = None;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        {
            let ui = &mut child;
            if icon_button(ui, Icon::Back, &t("返回提交列表  Esc"), false).clicked() {
                back = true;
            }
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            file_badge(ui.painter(), r.center(), &d.path);
            let (dir, name) = match d.path.rsplit_once('/') {
                Some((a, b)) => (format!("{a}/"), b.to_string()),
                None => (String::new(), d.path.clone()),
            };
            ui.label(RichText::new(name).strong().color(TEXT));
            ui.label(RichText::new(dir).size(11.5).color(TEXT3));
            ui.add_space(6.0);
            ui.label(RichText::new(format!("+{}", d.adds)).font(FontId::new(12.0, FontFamily::Monospace)).color(OK));
            ui.label(RichText::new(format!("−{}", d.dels)).font(FontId::new(12.0, FontFamily::Monospace)).color(ERR));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Code, &t("在代码查看中打开"), false).clicked() {
                    to_code = Some(d.lines.iter().find_map(|l| (l.kind != DiffKind::Ctx).then_some(l.new).flatten()).unwrap_or(1) as usize);
                }
                ui.label(RichText::new(crate::app::truncate(&d.title, 60)).size(11.5).color(TEXT3));
            });
        }
        ui.add_space(44.0);
        if d.loading {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.spinner());
        } else if let Some(e) = &d.err {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(e).color(ERR)));
        } else if d.lines.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(t("没有文本差异（可能是二进制文件或只改了权限）")).color(TEXT3)));
        } else {
            let font = FontId::new(12.5, FontFamily::Monospace);
            let cw = ui.fonts(|f| f.layout_no_wrap("0".repeat(100), font.clone(), TEXT).size().x / 100.0);
            let lh = 19.0;
            let gutter = 92.0;
            let width = (gutter + 24.0 + d.max_chars as f32 * cw + 40.0).max(ui.available_width());
            let lang = d.lang;
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .id_salt(("git-diff", self.diff_gen))
                .show_rows(ui, lh, d.lines.len(), |ui, range| {
                    for i in range {
                        let l = &d.lines[i];
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, lh), Sense::click());
                        let p = ui.painter();
                        let (bg, mark, mcol) = match l.kind {
                            DiffKind::Add => (OK.gamma_multiply(0.10), "+", OK),
                            DiffKind::Del => (ERR.gamma_multiply(0.10), "−", ERR),
                            DiffKind::Hunk => (VIOLET.gamma_multiply(0.08), "", VIOLET),
                            _ => (Color32::TRANSPARENT, "", TEXT3),
                        };
                        p.rect_filled(rect, 0.0, bg);
                        let gr = Rect::from_min_size(rect.min, vec2(gutter, lh));
                        p.rect_filled(gr, 0.0, if matches!(l.kind, DiffKind::Add | DiffKind::Del) { bg } else { BG1 });
                        if matches!(l.kind, DiffKind::Add | DiffKind::Del) {
                            p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, lh)), 0.0, mcol);
                        }
                        let nf = FontId::new(11.0, FontFamily::Monospace);
                        if let Some(o) = l.old {
                            p.text(pos2(rect.left() + 40.0, rect.center().y), Align2::RIGHT_CENTER, o.to_string(), nf.clone(), TEXT3);
                        }
                        if let Some(n) = l.new {
                            p.text(pos2(rect.left() + 82.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), nf, TEXT3);
                        }
                        p.text(pos2(rect.left() + gutter + 8.0, rect.center().y), Align2::LEFT_CENTER, mark, font.clone(), mcol);
                        let tx = pos2(rect.left() + gutter + 24.0, rect.top() + 2.0);
                        match l.kind {
                            DiffKind::Hunk | DiffKind::Meta => {
                                p.text(
                                    pos2(tx.x, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    &l.text,
                                    font.clone(),
                                    if l.kind == DiffKind::Hunk { VIOLET.gamma_multiply(0.9) } else { TEXT3 },
                                );
                            }
                            _ => {
                                let text = l.text.replace('\t', "    ");
                                let g = ui.fonts(|f| f.layout_job(highlight::layout(&text, lang, font.clone())));
                                p.galley(tx, g, TEXT);
                            }
                        }
                        if resp.double_clicked() {
                            if let Some(n) = l.new {
                                to_code = Some(n as usize);
                            }
                        }
                        if resp.hovered() {
                            p.rect_stroke(rect, 0.0, st(1.0, Color32::from_white_alpha(10)), egui::StrokeKind::Inside);
                        }
                    }
                });
        }
        let path = d.path.clone();
        if back {
            self.diff = None;
            self.file_sel = None;
        }
        if let (Some(line), Some(root)) = (to_code, &self.root) {
            let p = root.join(&path);
            if p.exists() {
                self.events.push(GitEvt::OpenFile(p, line.saturating_sub(1)));
            } else {
                self.toast(t("文件在工作区里已不存在"), WARN);
            }
        }
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let mut done: Option<bool> = None;
        egui::Modal::new(egui::Id::new("git-confirm"))
            .frame(egui::Frame::popup(&ctx.style()).inner_margin(egui::Margin::same(20)).corner_radius(R_CARD))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    draw_icon(ui.painter(), r, Icon::Branch, VIOLET);
                    ui.label(RichText::new(&c.title).font(FontId::new(15.0, bold())).color(TEXT));
                });
                ui.add_space(10.0);
                ui.label(RichText::new(&c.body).color(TEXT2));
                ui.add_space(8.0);
                ui.label(RichText::new(format!("git {}", c.args.join(" "))).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3));
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new(&c.ok).color(BG0).strong()).fill(VIOLET).min_size(vec2(90.0, 30.0))).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                        done = Some(true);
                    }
                    if ui.add(egui::Button::new(t("取消")).min_size(vec2(80.0, 30.0))).clicked() {
                        done = Some(false);
                    }
                });
            });
        match done {
            Some(true) => {
                let c = self.confirm.take().unwrap();
                self.run_op(c.label, c.args);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

// ---------------------------------------------------------------- 绘制

fn draw_graph_row(p: &egui::Painter, rect: Rect, g: &GraphRow, merge: bool, head: bool) {
    let x = |l: usize| rect.left() + 14.0 + l.min(MAX_LANES - 1) as f32 * LANE_W;
    let (y0, yc, y1) = (rect.top(), rect.center().y, rect.bottom());
    let w = 1.6;
    for &l in &g.pass {
        if l < MAX_LANES {
            p.vline(x(l), egui::Rangef::new(y0, y1), st(w, lane_col(l).gamma_multiply(0.85)));
        }
    }
    let curve = |a: egui::Pos2, b: egui::Pos2, col: Color32| {
        if (a.x - b.x).abs() < 0.5 {
            p.line_segment([a, b], st(w, col));
        } else {
            let dy = (b.y - a.y) * 0.7;
            p.add(CubicBezierShape::from_points_stroke(
                [a, pos2(a.x, a.y + dy), pos2(b.x, b.y - dy), b],
                false,
                Color32::TRANSPARENT,
                st(w, col),
            ));
        }
    };
    for &l in &g.into {
        curve(pos2(x(l), y0), pos2(x(g.lane), yc), lane_col(l).gamma_multiply(0.85));
    }
    for &l in &g.out {
        let col = if l == g.lane { lane_col(g.lane) } else { lane_col(l) };
        curve(pos2(x(g.lane), yc), pos2(x(l), y1), col.gamma_multiply(0.85));
    }
    let c = pos2(x(g.lane), yc);
    let col = lane_col(g.lane);
    if head {
        p.circle(c, 6.5, BG0, st(2.0, col));
        p.circle_filled(c, 3.0, col);
    } else if merge {
        p.circle(c, 4.5, BG0, st(1.8, col));
    } else {
        p.circle(c, 4.5, col, st(2.0, BG0));
    }
}

/// 解析 git log 的 %D：HEAD -> main / origin/main / tag: v1.0
fn ref_style(r: &str) -> (String, Color32, bool) {
    if let Some(b) = r.strip_prefix("HEAD -> ") {
        (b.to_string(), ACCENT, true)
    } else if r == "HEAD" {
        ("HEAD".into(), ACCENT, true)
    } else if let Some(t) = r.strip_prefix("tag: ") {
        (t.to_string(), WARN, false)
    } else if r.contains('/') {
        (r.to_string(), VIOLET, false)
    } else {
        (r.to_string(), OK, false)
    }
}

fn ref_width(ui: &Ui, r: &str) -> f32 {
    let (s, ..) = ref_style(r);
    ui.fonts(|f| f.layout_no_wrap(s, FontId::new(11.0, FontFamily::Monospace), TEXT).size().x) + 18.0
}

/// 画一个引用标签，返回宽度
fn ref_pill(p: &egui::Painter, left_center: egui::Pos2, r: &str) -> f32 {
    let (s, col, head) = ref_style(r);
    let tag = r.starts_with("tag: ");
    let g = p.layout_no_wrap(s, FontId::new(11.0, FontFamily::Monospace), if head { BG0 } else { col });
    let w = g.size().x + if tag { 18.0 } else { 14.0 };
    let rect = Rect::from_min_size(pos2(left_center.x, left_center.y - 9.0), vec2(w, 18.0));
    if head {
        p.rect_filled(rect, CornerRadius::same(9), col);
    } else {
        p.rect(rect, CornerRadius::same(9), col.gamma_multiply(0.13), st(1.0, col.gamma_multiply(0.45)), egui::StrokeKind::Inside);
    }
    let mut tx = rect.left() + 7.0;
    if tag {
        // 小标签图标：一个菱形点
        let c = pos2(rect.left() + 8.0, rect.center().y);
        p.add(egui::Shape::convex_polygon(
            vec![c + vec2(0.0, -3.0), c + vec2(3.0, 0.0), c + vec2(0.0, 3.0), c + vec2(-3.0, 0.0)],
            col,
            egui::Stroke::NONE,
        ));
        tx += 5.0;
    }
    p.galley(pos2(tx, rect.center().y - g.size().y / 2.0), g, col);
    w
}

fn avatar(p: &egui::Painter, c: egui::Pos2, name: &str, r: f32) {
    let h = name.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let col = LANE_COLORS[(h as usize) % LANE_COLORS.len()];
    p.circle_filled(c, r, col.gamma_multiply(0.22));
    let ch = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    p.text(c, Align2::CENTER_CENTER, ch, FontId::new(r * 1.05, bold()), col);
}

/// 带放大镜图标的搜索框，返回内容是否改变
fn search_box(ui: &mut Ui, v: &mut String, hint: &str, w: f32) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 28.0), Sense::hover());
    let id = egui::Id::new(("search-box", hint));
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter()
        .rect(rect, R_WIDGET, BG2, st(1.0, if focused { ACCENT.gamma_multiply(0.7) } else { LINE }), egui::StrokeKind::Inside);
    draw_icon(ui.painter(), Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), vec2(13.0, 13.0)), Icon::Search, TEXT3);
    let inner = Rect::from_min_max(pos2(rect.left() + 28.0, rect.top()), pos2(rect.right() - 6.0, rect.bottom()));
    let r = ui.put(
        inner,
        egui::TextEdit::singleline(v)
            .id(id)
            .hint_text(hint)
            .frame(false)
            .vertical_align(egui::Align::Center)
            .margin(vec2(0.0, 5.0)),
    );
    r.changed()
}

fn hash_chip(ui: &mut Ui, s: &str, col: Color32) -> egui::Response {
    let b = egui::Button::new(RichText::new(s).font(FontId::new(11.5, FontFamily::Monospace)).color(col))
        .fill(BG2)
        .stroke(st(1.0, LINE))
        .corner_radius(CornerRadius::same(5))
        .min_size(vec2(0.0, 22.0));
    let r = ui.add(b);
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    r
}

fn nav_row(ui: &mut Ui, icon: Icon, label: &str, badge: Option<String>, active: bool, col: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(r, 0.0, ACCENT.gamma_multiply(0.10));
        p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(r, 0.0, Color32::from_white_alpha(7));
    }
    draw_icon(
        p,
        Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(14.0, 14.0)),
        icon,
        if active { ACCENT } else { TEXT2 },
    );
    p.text(
        pos2(r.left() + 36.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        if active { TEXT } else { TEXT2 },
    );
    if let Some(b) = badge {
        let g = p.layout_no_wrap(b, FontId::new(10.5, mono_bold()), col);
        let br = Rect::from_center_size(pos2(r.right() - 14.0 - g.size().x / 2.0, r.center().y), vec2(g.size().x + 12.0, 16.0));
        p.rect_filled(br, CornerRadius::same(8), col.gamma_multiply(0.16));
        p.galley(br.center() - g.size() / 2.0, g, col);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn section_header(ui: &mut Ui, title: &str, n: usize, open: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    chevron(p, pos2(r.left() + 14.0, r.center().y), open, TEXT3);
    p.text(
        pos2(r.left() + 26.0, r.center().y),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        FontId::new(11.0, bold()),
        if resp.hovered() { TEXT } else { TEXT2 },
    );
    p.text(
        pos2(r.right() - 14.0, r.center().y),
        Align2::RIGHT_CENTER,
        n.to_string(),
        FontId::new(11.0, FontFamily::Monospace),
        TEXT3,
    );
    resp
}

fn branch_row(ui: &mut Ui, b: &Branch, active: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(r, 0.0, VIOLET.gamma_multiply(0.12));
        p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, VIOLET);
    } else if resp.hovered() {
        p.rect_filled(r, 0.0, Color32::from_white_alpha(7));
    }
    let x = r.left() + 30.0;
    let ic = Rect::from_center_size(pos2(x, r.center().y), vec2(13.0, 13.0));
    let col = match b.kind {
        RefKind::Local => OK,
        RefKind::Remote => VIOLET,
        RefKind::Tag => WARN,
    };
    if b.head {
        p.circle_filled(ic.center(), 4.0, ACCENT);
        p.circle_stroke(ic.center(), 7.0, st(1.0, ACCENT.gamma_multiply(0.4)));
    } else if b.kind == RefKind::Tag {
        let c = ic.center();
        p.add(egui::Shape::convex_polygon(
            vec![c + vec2(0.0, -4.5), c + vec2(4.5, 0.0), c + vec2(0.0, 4.5), c + vec2(-4.5, 0.0)],
            col.gamma_multiply(0.25),
            st(1.0, col),
        ));
    } else {
        draw_icon(p, ic, Icon::Branch, col.gamma_multiply(0.85));
    }
    // 右侧：领先 / 落后，或者最后提交时间
    let mut right = r.right() - 12.0;
    let small = FontId::new(10.5, FontFamily::Monospace);
    if b.gone {
        let g = p.layout_no_wrap(t("上游已删"), FontId::proportional(10.5), ERR);
        right -= g.size().x;
        p.galley(pos2(right, r.center().y - g.size().y / 2.0), g, ERR);
        right -= 8.0;
    } else {
        for (n, arrow, c) in [(b.behind, "↓", WARN), (b.ahead, "↑", OK)] {
            if n > 0 {
                let g = p.layout_no_wrap(format!("{arrow}{n}"), small.clone(), c);
                let br = Rect::from_min_max(pos2(right - g.size().x - 10.0, r.center().y - 8.0), pos2(right, r.center().y + 8.0));
                p.rect_filled(br, CornerRadius::same(8), c.gamma_multiply(0.14));
                p.galley(br.center() - g.size() / 2.0, g, c);
                right = br.left() - 4.0;
            }
        }
        if b.ahead == 0 && b.behind == 0 {
            let g = p.layout_no_wrap(relative(now_secs(), b.time), FontId::proportional(10.5), TEXT3);
            if resp.hovered() || active {
                right -= g.size().x;
                p.galley(pos2(right, r.center().y - g.size().y / 2.0), g, TEXT3);
                right -= 8.0;
            }
        }
    }
    let name_r = Rect::from_min_max(pos2(x + 12.0, r.top()), pos2(right, r.bottom()));
    let np = p.with_clip_rect(name_r);
    let font = if b.head { FontId::new(12.5, mono_bold()) } else { FontId::new(12.5, FontFamily::Monospace) };
    np.text(pos2(name_r.left(), r.center().y), Align2::LEFT_CENTER, &b.name, font, if b.head { TEXT } else { TEXT2 });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let tip = format!("{}\n{} · {}\n{}", b.name, b.hash, absolute(b.time), b.subject);
    resp.on_hover_text(tip + "\n\n" + &t("单击：只看这个分支的提交  ·  双击：切换到此分支"))
}

fn file_row(ui: &mut Ui, status: char, path: &str, adds: Option<u32>, dels: Option<u32>, active: bool) -> egui::Response {
    let (name, dir) = match path.rsplit_once('/') {
        Some((a, b)) => (b, a),
        None => (path, ""),
    };
    let mark = match status {
        'A' | 'U' | '?' => 'A',
        'D' => 'D',
        'R' => 'R',
        _ => 'M',
    };
    let r = tree_row(ui, 0, name, (!dir.is_empty()).then_some(dir), false, None, active, None);
    let p = ui.painter();
    let col = match mark {
        'A' => OK,
        'D' => ERR,
        'R' => VIOLET,
        _ => WARN,
    };
    let mut right = r.rect.right() - 12.0;
    p.text(
        pos2(right, r.rect.center().y),
        Align2::RIGHT_CENTER,
        if status == 'U' || status == '?' { 'U' } else { mark }.to_string(),
        FontId::new(11.0, mono_bold()),
        col,
    );
    right -= 18.0;
    if let (Some(a), Some(d)) = (adds, dels) {
        // 迷你增删条
        let tot = (a + d).max(1) as f32;
        let w = 30.0;
        let bar = Rect::from_min_size(pos2(right - w, r.rect.center().y - 2.0), vec2(w, 4.0));
        p.rect_filled(bar, 2.0, BG3);
        let aw = w * a as f32 / tot;
        p.rect_filled(Rect::from_min_size(bar.min, vec2(aw, 4.0)), 2.0, OK.gamma_multiply(0.8));
        p.rect_filled(Rect::from_min_max(pos2(bar.left() + aw, bar.top()), bar.max), 2.0, ERR.gamma_multiply(0.8));
        let _ = right;
    }
    r
}

/// 本模块里的文字多半要存进 String，统一转成 String 省得到处 to_string
fn t(s: &'static str) -> String {
    tr(s).to_string()
}

/// git 的英文报错转成一句能看懂的话
fn friendly_err(e: &str) -> String {
    if e.contains("would be overwritten") {
        return t("工作区的修改会被覆盖，请先提交或暂存（git stash）后再切换");
    }
    if e.contains("Could not read from remote") || e.contains("Authentication failed") || e.contains("terminal prompts disabled") {
        return t("无法访问远程仓库（网络或登录凭据问题），请在命令行里先登录一次");
    }
    if e.contains("already exists") {
        return t("同名分支已存在");
    }
    e.lines()
        .map(str::trim)
        .find(|l| l.starts_with("error:") || l.starts_with("fatal:"))
        .map(|l| l.trim_start_matches("error:").trim_start_matches("fatal:").trim().to_string())
        .unwrap_or_else(|| e.lines().next().unwrap_or("").to_string())
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn relative(now: i64, ts: i64) -> String {
    let d = (now - ts).max(0);
    if d < 60 {
        t("刚刚")
    } else if d < 3600 {
        tf("{} 分钟前", &[&(d / 60)])
    } else if d < 86400 {
        tf("{} 小时前", &[&(d / 3600)])
    } else if d < 86400 * 30 {
        tf("{} 天前", &[&(d / 86400)])
    } else if d < 86400 * 365 {
        tf("{} 个月前", &[&(d / (86400 * 30))])
    } else {
        tf("{} 年前", &[&(d / (86400 * 365))])
    }
}

fn absolute(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

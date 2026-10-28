//! 本地历史在代码页里的界面（v1.8）：
//! - 外部修改审阅：别的程序（AI、Keil、别的编辑器）改了工程里的文件，侧边栏出现「改动」视图，逐个保留或还原；
//! - 时间线和存档点：整个工程的保存、外部修改、存档点按时间排列，可以回到任一存档点（回退本身也能撤销）；
//! - 单个文件的本地历史；
//! - .gitignore 没排除编译产物时提示一次。
//!
//! 界面原则：没事的时候什么都不显示，有外部修改才出现侧边栏图标和提示条。

use super::*;
use crate::history::{self, review, Event, Hash, History, Kind as HKind};

pub(super) struct Row {
    pub rel: String,
    base: Option<Hash>,
    cur: Option<Hash>,
    round: u64,
    ts: i64,
    info: review::Info,
}

#[derive(Default)]
pub(super) struct HistUi {
    pub h: Option<Arc<History>>,
    rev: u64,
    pub rows: Vec<Row>,
    /// 待审阅文件的相对路径（文件树标记用）
    pub set: HashSet<String>,
    events: Vec<Event>,
    deleted: Vec<(String, Hash, i64)>,
    tab: u8,
    /// 点了「稍后」时的待审阅数量，数量变了再显示提示条
    snooze: Option<usize>,
    armed: Option<String>,
    cp_note: String,
    /// 单个文件的历史窗口
    file_win: Option<String>,
    /// .gitignore 默认规则窗口：每组勾不勾
    rules_win: Option<Vec<bool>>,
    /// 工程的 .gitignore 排除了编译产物 / 工程里确实有编译产物
    covers: bool,
    has_outputs: bool,
    show_deleted: bool,
    sens: Option<(String, Arc<review::Sensitive>)>,
    storage: Option<(Instant, u64)>,
}

/// 工程里有没有编译产物（顶层和下一层找常见的输出目录和文件）
fn has_build_outputs(root: &Path) -> bool {
    const DIRS: &[&str] = &["build", "objects", "listings", "debug", "release", ".pio", "cmakefiles", "out"];
    const EXTS: &[&str] = &["o", "obj", "axf", "elf", "map", "lst", "d", "crf"];
    let hit = |p: &Path| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if p.is_dir() {
            DIRS.contains(&name.as_str()) || name.starts_with("cmake-build-")
        } else {
            name.rsplit_once('.').is_some_and(|(_, e)| EXTS.contains(&e))
        }
    };
    let Ok(rd) = std::fs::read_dir(root) else { return false };
    for e in rd.flatten() {
        let p = e.path();
        if hit(&p) {
            return true;
        }
        if p.is_dir() && !e.file_name().to_string_lossy().starts_with('.') {
            if let Ok(rd2) = std::fs::read_dir(&p) {
                if rd2.flatten().take(400).any(|e| hit(&e.path())) {
                    return true;
                }
            }
        }
    }
    false
}

fn fmt_ts(ms: i64, with_date: bool) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms).single() {
        Some(dt) if with_date => dt.format("%m-%d %H:%M").to_string(),
        Some(dt) => dt.format("%H:%M:%S").to_string(),
        None => String::new(),
    }
}

fn day_of(ms: i64) -> String {
    use chrono::TimeZone;
    let Some(dt) = chrono::Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let today = chrono::Local::now().date_naive();
    let d = dt.date_naive();
    if d == today {
        t("今天").to_string()
    } else if Some(d) == today.pred_opt() {
        t("昨天").to_string()
    } else {
        d.format("%Y-%m-%d").to_string()
    }
}

/// 按像素宽度截断（中文字符按两倍宽算）
fn fit(s: &str, px: f32, size: f32) -> String {
    let w = |c: char| if c.is_ascii() { size * 0.56 } else { size * 1.02 };
    let total: f32 = s.chars().map(w).sum();
    if total <= px {
        return s.to_string();
    }
    let mut out = String::new();
    let mut acc = size;
    for c in s.chars() {
        acc += w(c);
        if acc > px {
            break;
        }
        out.push(c);
    }
    out + "…"
}

fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// 事件里这个文件改之前的内容
fn before_of(e: &Event, rel: &str) -> Option<Hash> {
    let i = e.files.iter().position(|(r, _)| r == rel)?;
    e.before.get(i).cloned().flatten()
}

fn files_desc(files: &[(String, Option<Hash>)]) -> String {
    match files.len() {
        0 => String::new(),
        1 => file_name(&files[0].0).to_string(),
        n => tf("{} 等 {} 个文件", &[&file_name(&files[0].0), &n]),
    }
}

fn kind_style(k: HKind) -> (&'static str, Color32) {
    match k {
        HKind::Init => ("首次快照", TEXT3()),
        HKind::Save => ("保存", TEXT2()),
        HKind::External => ("外部修改", WARN()),
        HKind::Delete => ("删除", ERR()),
        HKind::Keep => ("保留", OK()),
        HKind::Revert => ("还原", VIOLET()),
        HKind::Checkpoint => ("存档点", ACCENT()),
        HKind::Restore => ("回到存档点", ACCENT()),
    }
}

impl CodeView {
    pub(super) fn hist_opts(&self) -> history::Opts {
        history::Opts {
            max_file: (self.s.hist_max_mb.max(1) as u64) << 20,
            days: self.s.hist_days.max(1),
            max_bytes: (self.s.hist_gb.max(1) as u64) << 30,
            quiet: self.s.hist_quiet.split([',', '，', ';', '\n']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        }
    }

    /// 打开工程时启动（或关闭设置后停止）后台历史
    pub(super) fn hist_start(&mut self, ctx: &egui::Context) {
        history::set_active(None);
        self.hist = HistUi {
            tab: self.hist.tab,
            ..Default::default()
        };
        let Some(root) = self.root.clone() else { return };
        if !self.s.hist_on {
            return;
        }
        let h = Arc::new(History::start(&root, self.hist_opts(), ctx.clone()));
        self.hist.covers = h.covers_build;
        self.hist.has_outputs = !h.covers_build && has_build_outputs(&root);
        history::set_active(Some(h.clone()));
        self.hist.h = Some(h);
        self.hist.rev = u64::MAX;
    }

    /// 设置页改了历史相关的选项
    pub fn hist_settings_changed(&mut self, ctx: &egui::Context) {
        match (&self.hist.h, self.s.hist_on) {
            (Some(h), true) => {
                h.set_opts(self.hist_opts());
                self.hist.sens = None;
                self.hist.rev = u64::MAX;
            }
            (None, true) | (Some(_), false) => self.hist_start(ctx),
            (None, false) => {}
        }
    }

    /// 历史占用的磁盘空间（几秒算一次）
    pub fn hist_storage(&mut self) -> Option<u64> {
        let h = self.hist.h.as_ref()?;
        if self.hist.storage.is_none_or(|(t, _)| t.elapsed().as_secs() > 5) {
            self.hist.storage = Some((Instant::now(), h.storage_bytes()));
        }
        self.hist.storage.map(|(_, n)| n)
    }

    fn sensitive(&mut self) -> Arc<review::Sensitive> {
        if self.hist.sens.as_ref().is_none_or(|(s, _)| *s != self.s.sensitive) {
            self.hist.sens = Some((self.s.sensitive.clone(), Arc::new(review::Sensitive::new(&self.s.sensitive))));
        }
        self.hist.sens.as_ref().map(|(_, s)| s.clone()).unwrap_or_else(|| Arc::new(review::Sensitive::new("")))
    }

    /// 每帧：历史有变化时刷新待审阅列表和时间线
    pub(super) fn hist_poll(&mut self) {
        let Some(h) = self.hist.h.clone() else { return };
        let rev = h.rev();
        if rev == self.hist.rev {
            return;
        }
        self.hist.rev = rev;
        let sens = self.sensitive();
        let pend = h.pending();
        let mut rows = Vec::with_capacity(pend.len());
        for (i, p) in pend.into_iter().enumerate() {
            let mut info = if i < 400 {
                let a = h.content(&p.base);
                let b = h.content(&p.cur);
                review::analyze(a.as_deref(), b.as_deref())
            } else {
                review::Info::default()
            };
            info.sensitive = sens.hit(&p.rel);
            rows.push(Row {
                rel: p.rel,
                base: p.base,
                cur: p.cur,
                round: p.round,
                ts: p.ts,
                info,
            });
        }
        self.hist.set = rows.iter().map(|r| r.rel.clone()).collect();
        self.hist.rows = rows;
        self.hist.events = h.events();
        self.hist.deleted = h.deleted();
        if self.hist.rows.is_empty() && self.side == Side::Changes && self.hist.tab == 0 {
            self.hist.tab = 1;
        }
    }

    pub(super) fn hist_pending(&self) -> bool {
        !self.hist.rows.is_empty()
    }

    // ------------------------------------------------------------ 提示条

    pub(super) fn hist_notice(&mut self, ui: &mut Ui) {
        let n = self.hist.rows.len();
        if n > 0 && self.side != Side::Changes && self.hist.snooze != Some(n) {
            let (a, d) = self.hist.rows.iter().fold((0, 0), |(a, d), r| (a + r.info.add, d + r.info.del));
            let sens = self.hist.rows.iter().filter(|r| r.info.sensitive).count();
            let mut msg = tf("外部修改了 {} 个文件（+{} −{}）", &[&n, &a, &d]);
            if sens > 0 {
                msg += &tf("，其中 {} 个是敏感文件", &[&sens]);
            }
            match notice_bar(ui, &msg, &[t("查看"), t("全部保留"), t("稍后")]) {
                Some(0) => {
                    self.side = Side::Changes;
                    self.hist.tab = 0;
                }
                Some(1) => self.keep_all(),
                Some(2) => self.hist.snooze = Some(n),
                _ => {}
            }
        }
        let root = self.root.as_ref().map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
        if self.hist.h.is_some() && !self.hist.covers && self.hist.has_outputs && !self.s.ig_hint_off.contains(&root) {
            match notice_bar(ui, t(".gitignore 没有排除编译产物，本地历史暂时按嵌入式默认规则跳过它们"), &[t("添加到 .gitignore…"), t("不再提示")]) {
                Some(0) => self.hist.rules_win = Some(vec![true; crate::history::ignore::DEFAULT_RULES.len()]),
                Some(1) => self.s.ig_hint_off.push(root),
                _ => {}
            }
        }
    }

    fn keep_all(&mut self) {
        let Some(h) = self.hist.h.clone() else { return };
        let n = self.hist.rows.len();
        let id = h.keep_all();
        self.toast(tf("已保留 {} 个文件的修改，存档点 #{}", &[&n, &id]), OK());
    }

    fn keep(&mut self, rels: Vec<String>) {
        if let Some(h) = &self.hist.h {
            h.keep(&rels);
        }
    }

    fn revert(&mut self, rels: Vec<String>) {
        let Some(h) = self.hist.h.clone() else { return };
        match h.revert(&rels) {
            Ok(n) => self.toast(tf("已还原 {} 个文件", &[&n]), OK()),
            Err(e) => self.toast(e, ERR()),
        }
        self.after_disk_change();
    }

    /// 历史把文件写回磁盘后：刷新文件树、已打开的标签、Git 标记
    fn after_disk_change(&mut self) {
        let dirs: Vec<PathBuf> = self.tree.keys().cloned().collect();
        for d in dirs {
            self.load_dir(&d);
        }
        self.last_stat = 0.0;
        self.refresh_git();
    }

    /// 历史里的两个版本比较（right 为 None：和磁盘上现在的文件比较，可以逐块还原）
    fn cmp_hist(&mut self, rel: &str, left: (Option<Hash>, String), right: Option<(Option<Hash>, String)>) {
        let Some(h) = self.hist.h.clone() else { return };
        let Some(root) = self.root.clone() else { return };
        let name = file_name(rel).to_string();
        let tmp = |hash: &Option<Hash>, label: &str| crate::compareview::temp_file(&name, &h.content(hash).unwrap_or_default()).pipe_label(&name, label);
        let a = tmp(&left.0, &left.1);
        let b = match right {
            Some((hash, label)) => tmp(&hash, &label),
            None => {
                let p = root.join(rel);
                if p.exists() {
                    (p, None)
                } else {
                    let r = crate::compareview::temp_file(&name, b"");
                    (r, Some(format!("{name} ({})", t("已删除"))))
                }
            }
        };
        self.events.push(CodeEvt::Compare(a.0, a.1, b.0, b.1));
    }

    // ------------------------------------------------------------ 侧边栏视图

    pub(super) fn changes_panel(&mut self, ui: &mut Ui) {
        if self.hist.h.is_none() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(t("本地历史已关闭（设置 → 代码）")).color(TEXT3()));
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 8.0);
                let n = self.hist.rows.len();
                let l0 = if n > 0 { tf("待审阅 {}", &[&n]) } else { t("待审阅").to_string() };
                crate::app::tab_strip(ui, &mut self.hist.tab, &[(0u8, l0.as_str()), (1u8, t("时间线"))], 26.0);
            });
        });
        ui.add_space(6.0);
        if self.hist.tab == 0 {
            self.review_list(ui);
        } else {
            self.timeline(ui);
        }
    }

    fn review_list(&mut self, ui: &mut Ui) {
        if self.hist.rows.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(t("没有待审阅的外部修改")).color(TEXT2()));
                ui.add_space(4.0);
                ui.label(RichText::new(t("别的程序（AI 工具、Keil 等）改了工程里的文件，会列在这里")).size(11.5).color(TEXT3()));
            });
            return;
        }
        let (a, d) = self.hist.rows.iter().fold((0, 0), |(a, d), r| (a + r.info.add, d + r.info.del));
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(tf("{} 个文件", &[&self.hist.rows.len()])).size(12.0).color(TEXT2()));
            ui.label(RichText::new(format!("+{a}")).size(12.0).color(OK()));
            ui.label(RichText::new(format!("−{d}")).size(12.0).color(ERR()));
        });
        ui.add_space(4.0);
        let mut keep_all = false;
        let mut revert_all = false;
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            if ui
                .add(egui::Button::new(RichText::new(t("全部保留")).color(ON_ACCENT())).fill(ACCENT()))
                .on_hover_text(t("接受这些修改，并打一个存档点"))
                .clicked()
            {
                keep_all = true;
            }
            let armed = self.hist.armed.as_deref() == Some("*");
            let label = if armed {
                RichText::new(t("确认全部还原？")).color(ERR())
            } else {
                RichText::new(t("全部还原"))
            };
            if ui.button(label).clicked() {
                if armed {
                    revert_all = true;
                } else {
                    self.hist.armed = Some("*".into());
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.checkbox(&mut self.s.hide_format, RichText::new(t("隐藏只改了空白 / 格式的文件")).size(11.5).color(TEXT2()));
        });
        if keep_all {
            self.keep_all();
            return;
        }
        if revert_all {
            self.hist.armed = None;
            let rels: Vec<String> = self.hist.rows.iter().map(|r| r.rel.clone()).collect();
            self.revert(rels);
            return;
        }
        ui.add_space(4.0);
        let mut act: Option<(u8, String)> = None;
        let mut round_act: Option<(bool, u64)> = None;
        let hidden = self.hist.rows.iter().filter(|r| self.s.hide_format && r.info.format_only).count();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut rounds: Vec<u64> = self.hist.rows.iter().map(|r| r.round).collect();
            rounds.sort_unstable_by(|a, b| b.cmp(a));
            rounds.dedup();
            let multi = rounds.len() > 1;
            for rd in rounds {
                let rows: Vec<&Row> = self.hist.rows.iter().filter(|r| r.round == rd && !(self.s.hide_format && r.info.format_only)).collect();
                if rows.is_empty() {
                    continue;
                }
                if multi {
                    let ts = rows.iter().map(|r| r.ts).max().unwrap_or(0);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new(tf("第 {} 轮 · {}", &[&rd, &fmt_ts(ts, true)])).size(11.0).color(TEXT3()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(8.0);
                            if ui.small_button(t("还原本轮")).clicked() {
                                round_act = Some((false, rd));
                            }
                            if ui.small_button(t("保留本轮")).clicked() {
                                round_act = Some((true, rd));
                            }
                        });
                    });
                    ui.add_space(2.0);
                }
                for r in rows {
                    if let Some(a) = review_row(ui, r, self.hist.armed.as_deref() == Some(r.rel.as_str())) {
                        act = Some((a, r.rel.clone()));
                    }
                }
            }
            if hidden > 0 {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(tf("另有 {} 个只改了格式的文件已隐藏", &[&hidden])).size(11.0).color(TEXT3()));
                });
            }
        });
        if let Some((keep, rd)) = round_act {
            let rels: Vec<String> = self.hist.rows.iter().filter(|r| r.round == rd).map(|r| r.rel.clone()).collect();
            if keep {
                self.keep(rels);
            } else {
                self.revert(rels);
            }
        }
        let Some((a, rel)) = act else { return };
        let Some(row) = self.hist.rows.iter().find(|r| r.rel == rel) else { return };
        let (base, cur) = (row.base.clone(), row.cur.clone());
        match a {
            // 单击：和修改前比较，可以逐块还原
            0 => {
                let _ = cur;
                self.cmp_hist(&rel, (base, t("外部修改前").to_string()), None);
            }
            1 => self.keep(vec![rel]),
            2 => {
                self.hist.armed = None;
                self.revert(vec![rel]);
            }
            3 => self.hist.armed = Some(rel),
            4 => {
                if let Some(root) = self.root.clone() {
                    let p = root.join(&rel);
                    if p.is_file() {
                        self.open_file(&p, None);
                    }
                }
            }
            5 => self.restore_enc(&rel, base),
            6 => self.hist.file_win = Some(rel),
            _ => {}
        }
    }

    /// 保留现在的内容，换回原来的编码和换行符
    fn restore_enc(&mut self, rel: &str, base: Option<Hash>) {
        let Some(h) = self.hist.h.clone() else { return };
        let Some(root) = self.root.clone() else { return };
        let p = root.join(rel);
        let (Some(a), Ok(b)) = (h.content(&base), std::fs::read(&p)) else { return };
        match review::restore_encoding(&a, &b).and_then(|data| history::save_tracked(&p, &data)) {
            Ok(()) => {
                self.toast(tf("{} 已换回原来的编码和换行符", &[&file_name(rel)]), OK());
                self.after_disk_change();
            }
            Err(e) => self.toast(e, ERR()),
        }
    }

    // ------------------------------------------------------------ 时间线

    fn timeline(&mut self, ui: &mut Ui) {
        let Some(h) = self.hist.h.clone() else { return };
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let te = ui.add_sized(
                vec2(ui.available_width() - 96.0, 24.0),
                egui::TextEdit::singleline(&mut self.hist.cp_note).hint_text(t("存档点说明（可不填）")).margin(vec2(6.0, 3.0)),
            );
            let enter = te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.button(t("新建存档点")).clicked() || enter {
                let note = std::mem::take(&mut self.hist.cp_note);
                let id = h.checkpoint(note.trim(), true);
                self.toast(tf("已新建存档点 #{}", &[&id]), OK());
            }
        });
        if h.busy.load(Ordering::Relaxed) {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(tf("正在建立首次快照… {} 个文件", &[&h.scanned.load(Ordering::Relaxed)])).size(11.5).color(TEXT3()));
            });
        }
        ui.add_space(6.0);
        let mut act: Option<(u8, u64)> = None;
        let mut restore_del: Option<(String, Hash)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if !self.hist.deleted.is_empty() {
                let open = self.hist.show_deleted;
                if tree_row(ui, 0, &tf("已删除的文件（{}）", &[&self.hist.deleted.len()]), None, true, Some(open), false, None).clicked() {
                    self.hist.show_deleted = !open;
                }
                if open {
                    for (rel, hash, ts) in self.hist.deleted.iter().take(200) {
                        let r = tree_row(ui, 1, file_name(rel), Some(&fmt_ts(*ts, true)), false, None, false, Some('D'));
                        let r = r.on_hover_text(format!("{rel}\n{}", t("点击恢复")));
                        if r.clicked() {
                            restore_del = Some((rel.clone(), hash.clone()));
                        }
                    }
                }
                ui.add_space(6.0);
            }
            let mut day = String::new();
            for e in self.hist.events.iter().rev().filter(|e| e.kind != HKind::Init).take(600) {
                let d = day_of(e.ts);
                if d != day {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new(&d).size(11.0).strong().color(TEXT3()));
                    });
                    ui.add_space(2.0);
                    day = d;
                }
                if let Some(a) = event_row(ui, e, self.hist.armed.as_deref() == Some(format!("cp{}", e.id).as_str())) {
                    act = Some((a, e.id));
                }
            }
            if self.hist.events.iter().all(|e| e.kind == HKind::Init) {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(t("还没有记录。保存文件、外部修改、存档点都会出现在这里")).size(11.5).color(TEXT3()));
                });
            }
        });
        if let Some((rel, hash)) = restore_del {
            match h.restore_file(&rel, Some(hash)) {
                Ok(()) => {
                    self.toast(tf("已恢复 {}", &[&rel]), OK());
                    self.after_disk_change();
                }
                Err(e) => self.toast(e, ERR()),
            }
        }
        let Some((a, id)) = act else { return };
        let Some(ev) = self.hist.events.iter().rev().find(|e| e.id == id).cloned() else { return };
        match a {
            // 单个文件的事件：打开这个文件的历史
            0 => {
                if ev.files.len() == 1 {
                    self.hist.file_win = Some(ev.files[0].0.clone());
                } else if ev.files.len() > 1 {
                    // 多个文件：和事件之前比较第一个，其它在列表里
                    self.hist.file_win = Some(ev.files[0].0.clone());
                }
            }
            1 => self.hist.armed = Some(format!("cp{id}")),
            2 => {
                self.hist.armed = None;
                match h.restore(id) {
                    Ok((n, undo)) => {
                        self.toast(tf("已回到存档点 #{}（改动 {} 个文件）。想撤销就回到存档点 #{}", &[&id, &n, &undo]), OK());
                    }
                    Err(e) => self.toast(e, ERR()),
                }
                self.after_disk_change();
            }
            3 => self.cmp_checkpoint(&ev),
            _ => {}
        }
    }

    /// 存档点和现在的工程比较：把存档点内容放进临时文件夹，交给比较页做文件夹比较
    fn cmp_checkpoint(&mut self, ev: &Event) {
        let Some(h) = self.hist.h.clone() else { return };
        let Some(root) = self.root.clone() else { return };
        let Some(m) = h.lock().manifest(ev) else {
            self.toast(t("存档点内容已被清理"), ERR());
            return;
        };
        let dir = crate::compareview::temp_dir().join(format!("checkpoint-{}", ev.id));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, hash) in &m {
            if let Some(data) = h.content(&Some(hash.clone())) {
                let p = dir.join(rel);
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::write(p, data);
            }
        }
        let _ = std::fs::create_dir_all(&dir);
        self.events.push(CodeEvt::Compare(dir, Some(tf("存档点 #{}", &[&ev.id])), root, None));
    }

    // ------------------------------------------------------------ 浮动窗口

    pub(super) fn hist_windows(&mut self, ctx: &egui::Context) {
        self.file_history_win(ctx);
        self.rules_window(ctx);
    }

    /// 文件树右键「本地历史…」
    pub(super) fn show_file_history(&mut self, path: &Path) {
        if self.hist.h.is_none() {
            self.toast(t("本地历史已关闭（设置 → 代码）"), WARN());
            return;
        }
        if let Some(rel) = self.hist.h.as_ref().and_then(|h| h.rel(path)) {
            self.hist.file_win = Some(rel);
        }
    }

    fn file_history_win(&mut self, ctx: &egui::Context) {
        let Some(rel) = self.hist.file_win.clone() else { return };
        let Some(h) = self.hist.h.clone() else { return };
        let vers = h.versions(&rel);
        let mut open = true;
        let mut act: Option<(u8, usize)> = None;
        egui::Window::new(tf("本地历史 · {}", &[&file_name(&rel)]))
            .id(egui::Id::new("file-history"))
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .default_height(420.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(&rel).size(11.5).color(TEXT3()));
                ui.add_space(4.0);
                if vers.is_empty() {
                    ui.label(RichText::new(t("这个文件还没有历史版本")).color(TEXT2()));
                    return;
                }
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (i, (e, hash)) in vers.iter().enumerate() {
                        let (label, c) = kind_style(e.kind);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(fmt_ts(e.ts, true)).font(FontId::new(12.0, FontFamily::Monospace)).color(TEXT2()));
                            ui.label(RichText::new(t(label)).size(12.0).color(c));
                            if e.kind == HKind::External && e.note == "quiet" {
                                ui.label(RichText::new(t("（后台程序）")).size(11.0).color(TEXT3()));
                            }
                            if hash.is_none() {
                                ui.label(RichText::new(t("（已删除）")).size(11.0).color(TEXT3()));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if hash.is_some() {
                                    if ui.small_button(t("恢复")).on_hover_text(t("把文件恢复成这个版本（这一步本身也会记进历史）")).clicked() {
                                        act = Some((2, i));
                                    }
                                    if ui.small_button(t("与现在比较")).clicked() {
                                        act = Some((0, i));
                                    }
                                    if i + 1 < vers.len() {
                                        if ui.small_button(t("与上一版比较")).clicked() {
                                            act = Some((1, i));
                                        }
                                    } else if before_of(e, &rel).is_some() && ui.small_button(t("与上一版比较")).clicked() {
                                        // 最早的一条：和它之前（首次快照里）的内容比较
                                        act = Some((3, i));
                                    }
                                }
                            });
                        });
                    }
                });
            });
        if !open {
            self.hist.file_win = None;
        }
        let Some((a, i)) = act else { return };
        let (e, hash) = vers[i].clone();
        let lbl = |e: &Event| format!("{} {}", fmt_ts(e.ts, true), t(kind_style(e.kind).0));
        match a {
            0 => self.cmp_hist(&rel, (hash, lbl(&e)), None),
            1 => {
                let (pe, ph) = vers[i + 1].clone();
                self.cmp_hist(&rel, (ph, lbl(&pe)), Some((hash, lbl(&e))));
            }
            3 => {
                let b = before_of(&e, &rel);
                self.cmp_hist(&rel, (b, t("首次快照").to_string()), Some((hash, lbl(&e))));
            }
            _ => match h.restore_file(&rel, hash) {
                Ok(()) => {
                    self.toast(tf("已恢复 {}", &[&file_name(&rel)]), OK());
                    self.after_disk_change();
                }
                Err(e) => self.toast(e, ERR()),
            },
        }
    }

    /// 往 .gitignore 加嵌入式默认规则
    fn rules_window(&mut self, ctx: &egui::Context) {
        let Some(mut picked) = self.hist.rules_win.take() else { return };
        let Some(root) = self.root.clone() else { return };
        let mut open = true;
        let mut add = false;
        egui::Window::new(t("添加编译产物规则到 .gitignore"))
            .id(egui::Id::new("gitignore-rules"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(
                    RichText::new(t("勾选要排除的类型。.bin / .hex 默认不排除（常要拿去烧录或发给别人），需要时自己加。"))
                        .size(11.5)
                        .color(TEXT3()),
                );
                ui.add_space(6.0);
                for ((title, rules), on) in crate::history::ignore::DEFAULT_RULES.iter().zip(picked.iter_mut()) {
                    ui.horizontal(|ui| {
                        ui.checkbox(on, RichText::new(t(title)).strong());
                        ui.label(RichText::new(rules.join("  ")).font(FontId::new(11.0, FontFamily::Monospace)).color(TEXT3()));
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let any = picked.iter().any(|b| *b);
                    if ui.add_enabled(any, egui::Button::new(RichText::new(t("添加")).color(ON_ACCENT())).fill(ACCENT())).clicked() {
                        add = true;
                    }
                });
            });
        if add {
            let p = root.join(".gitignore");
            let mut old = std::fs::read(&p).unwrap_or_default();
            if !old.is_empty() && !old.ends_with(b"\n") {
                old.push(b'\n');
            }
            old.extend_from_slice(crate::history::ignore::default_text(&picked).as_bytes());
            match history::save_tracked(&p, &old) {
                Ok(()) => {
                    self.hist.covers = true;
                    self.toast(t("已添加到 .gitignore"), OK());
                    self.after_disk_change();
                }
                Err(e) => self.toast(e, ERR()),
            }
            return;
        }
        if open {
            self.hist.rules_win = Some(picked);
        }
    }

    /// 外部调用（烧录任务）：打一个存档点，本地历史关闭时返回 None
    /// 还没保存的文件（Git 提交前检查用）
    pub fn dirty_paths(&self) -> Vec<PathBuf> {
        self.tabs.iter().filter(|d| d.dirty() && !d.virt).map(|d| d.path.clone()).collect()
    }

    /// 当前工程的 Git 分支名（不是仓库时为空）
    pub fn branch(&self) -> &str {
        &self.git_branch
    }

    pub fn checkpoint_now(&mut self, note: &str) -> Option<u64> {
        self.hist.h.as_ref().map(|h| h.checkpoint(note, true))
    }

    pub(super) fn hist_acts(&mut self, has: &dyn Fn(Act) -> bool) {
        if has(Act::Checkpoint) {
            match self.hist.h.clone() {
                Some(h) => {
                    let id = h.checkpoint("", true);
                    self.toast(tf("已新建存档点 #{}", &[&id]), OK());
                }
                None => self.toast(t("本地历史已关闭（设置 → 代码）"), WARN()),
            }
        }
        if has(Act::ReviewChanges) {
            self.side = Side::Changes;
            self.hist.tab = 0;
        }
        if has(Act::Timeline) {
            self.side = Side::Changes;
            self.hist.tab = 1;
        }
        if has(Act::FileHistory) {
            if let Some(p) = self.tabs.get(self.active).map(|d| d.path.clone()) {
                self.show_file_history(&p);
            }
        }
    }
}

trait PipeLabel {
    fn pipe_label(self, name: &str, label: &str) -> (PathBuf, Option<String>);
}

impl PipeLabel for PathBuf {
    fn pipe_label(self, name: &str, label: &str) -> (PathBuf, Option<String>) {
        (self, Some(format!("{name} ({label})")))
    }
}

/// 审阅列表的一行。返回：0 单击比较，1 保留，2 确认还原，3 还原（第一次点），4 打开，5 恢复编码，6 本地历史
fn review_row(ui: &mut Ui, r: &Row, armed: bool) -> Option<u8> {
    let i = &r.info;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let p = ui.painter();
    if ui.rect_contains_pointer(rect) || armed {
        p.rect_filled(rect, 0.0, tint(7));
    }
    let x = rect.left() + 14.0;
    let y1 = rect.top() + 13.0;
    let y2 = rect.top() + 29.0;
    file_badge(p, pos2(x + 6.0, y1), &r.rel);
    let name = file_name(&r.rel);
    let nc = if r.cur.is_none() {
        ERR()
    } else if r.base.is_none() {
        OK()
    } else {
        TEXT()
    };
    let g = p.layout_no_wrap(name.to_string(), FontId::proportional(12.5), nc);
    let nw = g.size().x;
    p.galley(pos2(x + 20.0, y1 - g.size().y / 2.0), g, TEXT());
    let dir = r.rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let right_w = 92.0;
    let room = rect.right() - right_w - (x + 26.0 + nw);
    if !dir.is_empty() && room > 30.0 {
        let d = fit(dir, room, 11.0);
        p.text(pos2(x + 26.0 + nw, y1), Align2::LEFT_CENTER, d, FontId::proportional(11.0), TEXT3());
    }
    // 第二行：小标签
    let mut tags: Vec<(String, Color32)> = vec![];
    if r.base.is_none() {
        tags.push((t("新增").into(), OK()));
    }
    if r.cur.is_none() {
        tags.push((t("删除").into(), ERR()));
    }
    if i.sensitive {
        tags.push((t("敏感").into(), WARN()));
    }
    if let Some((a, b)) = i.enc {
        tags.push((format!("{}→{}", a.label(), b.label()), VIOLET()));
    }
    if let Some((a, b)) = i.eol {
        let s = |c: bool| if c { "CRLF" } else { "LF" };
        tags.push((format!("{}→{}", s(a), s(b)), VIOLET()));
    }
    if i.format_only {
        tags.push((t("仅格式").into(), TEXT3()));
    }
    if i.binary {
        tags.push((t("二进制").into(), TEXT3()));
    }
    let mut tx = x + 20.0;
    for (s, c) in &tags {
        let g = p.layout_no_wrap(s.clone(), FontId::proportional(10.5), *c);
        let w = g.size().x + 10.0;
        let tr = Rect::from_min_size(pos2(tx, y2 - 8.0), vec2(w, 16.0));
        p.rect_filled(tr, 3.0, c.gamma_multiply(0.14));
        p.galley(pos2(tx + 5.0, y2 - g.size().y / 2.0), g, *c);
        tx += w + 4.0;
    }
    if !i.binary {
        let s = format!("+{} −{}", i.add, i.del);
        let g = p.layout_no_wrap(format!("+{}", i.add), FontId::new(11.0, FontFamily::Monospace), OK());
        let g2 = p.layout_no_wrap(format!(" −{}", i.del), FontId::new(11.0, FontFamily::Monospace), ERR());
        let _ = s;
        let w2 = g2.size().x;
        let gx = rect.right() - 12.0 - w2 - g.size().x;
        p.galley(pos2(gx, y1 - g.size().y / 2.0), g, OK());
        p.galley(pos2(rect.right() - 12.0 - w2, y1 - g2.size().y / 2.0), g2, ERR());
    }
    let mut out = None;
    // 悬停时第二行右侧：保留 / 还原（按钮盖在行上，用指针位置判断，免得移到按钮上就消失）
    let over = ui.rect_contains_pointer(rect);
    if over || armed {
        let mk = |ui: &mut Ui, r: Rect, s: &str, c: Color32, id: &str| {
            let resp = ui.interact(r, ui.id().with(("rv", id)), Sense::click());
            let p = ui.painter();
            if resp.hovered() {
                p.rect_filled(r, 3.0, c.gamma_multiply(0.18));
            }
            p.text(r.center(), Align2::CENTER_CENTER, s, FontId::proportional(11.0), c);
            resp.clicked()
        };
        let w_rv = if armed { 66.0 } else { 36.0 };
        let r_rv = Rect::from_min_size(pos2(rect.right() - 10.0 - w_rv, y2 - 8.0), vec2(w_rv, 16.0));
        let r_keep = Rect::from_min_size(pos2(r_rv.left() - 40.0, y2 - 8.0), vec2(36.0, 16.0));
        let rv_label = if armed { t("确认还原") } else { t("还原") };
        if mk(ui, r_rv, rv_label, if armed { ERR() } else { TEXT2() }, &r.rel) {
            out = Some(if armed { 2 } else { 3 });
        }
        if mk(ui, r_keep, t("保留"), OK(), &format!("k{}", r.rel)) {
            out = Some(1);
        }
    }
    if out.is_none() && resp.clicked() {
        out = Some(0);
    }
    resp.clone().on_hover_text(format!("{}\n{}", r.rel, t("点击：和修改前比较，可以逐块还原")));
    resp.context_menu(|ui| {
        ui.set_min_width(200.0);
        if ui.button(t("和修改前比较")).clicked() {
            out = Some(0);
            ui.close_menu();
        }
        if r.cur.is_some() && ui.button(t("打开文件")).clicked() {
            out = Some(4);
            ui.close_menu();
        }
        ui.separator();
        if ui.button(t("保留")).clicked() {
            out = Some(1);
            ui.close_menu();
        }
        if ui.button(t("还原成修改前")).clicked() {
            out = Some(2);
            ui.close_menu();
        }
        if (i.enc.is_some() || i.eol.is_some()) && r.base.is_some() && r.cur.is_some() {
            if ui.button(t("保留内容，换回原来的编码和换行符")).clicked() {
                out = Some(5);
                ui.close_menu();
            }
        }
        ui.separator();
        if ui.button(t("本地历史…")).clicked() {
            out = Some(6);
            ui.close_menu();
        }
    });
    out
}

/// 时间线的一行。返回：0 单击（看文件历史），1 回到存档点（第一次点），2 确认回到，3 与现在比较
fn event_row(ui: &mut Ui, e: &Event, armed: bool) -> Option<u8> {
    let cp = e.kind == HKind::Checkpoint;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() || armed {
        p.rect_filled(rect, 0.0, tint(7));
    }
    let (label, c) = kind_style(e.kind);
    let cy = rect.center().y;
    // 竖线 + 圆点，存档点用实心大点
    p.vline(rect.left() + 18.0, rect.y_range(), st(1.0, LINE()));
    if cp {
        p.circle_filled(pos2(rect.left() + 18.0, cy), 4.5, c);
    } else {
        p.circle_filled(pos2(rect.left() + 18.0, cy), 3.0, c.gamma_multiply(0.85));
    }
    let tg = p.layout_no_wrap(fmt_ts(e.ts, false), FontId::new(11.0, FontFamily::Monospace), TEXT3());
    let tw = tg.size().x;
    p.galley(pos2(rect.left() + 30.0, cy - tg.size().y / 2.0), tg, TEXT3());
    let mut desc = match e.kind {
        HKind::Restore => format!("{} {}", t(label), e.note),
        HKind::Checkpoint if !e.note.is_empty() => format!("{} #{} · {}", t(label), e.id, e.note),
        HKind::Checkpoint => format!("{} #{}", t(label), e.id),
        _ => format!("{} {}", t(label), files_desc(&e.files)),
    };
    if e.kind == HKind::External && e.note == "quiet" {
        desc += &format!(" {}", t("（后台程序）"));
    }
    let x = rect.left() + 38.0 + tw;
    let over = ui.rect_contains_pointer(rect);
    let right = if cp && (over || armed) {
        rect.right() - (if armed { 140.0 } else { 120.0 })
    } else {
        rect.right() - 8.0
    };
    let d = fit(&desc, right - x - 6.0, 12.0);
    p.text(pos2(x, cy), Align2::LEFT_CENTER, d, FontId::proportional(12.0), if cp { TEXT() } else { TEXT2() });
    if e.pinned && cp {
        // 手动存档点：不会被自动清理
        p.text(pos2(right, cy), Align2::RIGHT_CENTER, "●", FontId::proportional(7.0), ACCENT().gamma_multiply(0.7));
    }
    let mut out = None;
    if cp && (over || armed) {
        let mk = |ui: &mut Ui, r: Rect, s: &str, c: Color32, id: u8| {
            let resp = ui.interact(r, ui.id().with(("ev", e.id, id)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 3.0, c.gamma_multiply(0.18));
            }
            ui.painter().text(r.center(), Align2::CENTER_CENTER, s, FontId::proportional(11.0), c);
            resp.clicked()
        };
        let w1 = if armed { 72.0 } else { 52.0 };
        let r1 = Rect::from_min_size(pos2(rect.right() - 8.0 - w1, cy - 9.0), vec2(w1, 18.0));
        let r2 = Rect::from_min_size(pos2(r1.left() - 56.0, cy - 9.0), vec2(52.0, 18.0));
        if mk(ui, r1, if armed { t("确认回到") } else { t("回到这里") }, if armed { ERR() } else { ACCENT() }, 1) {
            out = Some(if armed { 2 } else { 1 });
        }
        if mk(ui, r2, t("比较"), TEXT2(), 2) {
            out = Some(3);
        }
    }
    let tip = if e.files.len() > 1 {
        e.files.iter().take(20).map(|(r, _)| r.as_str()).collect::<Vec<_>>().join("\n")
    } else {
        e.files.first().map(|(r, _)| r.clone()).unwrap_or_default()
    };
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if out.is_none() && resp.clicked() && !e.files.is_empty() {
        out = Some(0);
    }
    out
}

//! Git 页的写操作（v1.8）：暂存 / 取消暂存 / 放弃修改、提交（提交前检查、说明草稿）、拉取合并、推送、
//! 撤销上次提交、暂存修改（stash）、打标签、子模块、固件体积历史。
//! 不常用的都收在侧边栏顶部的「⋯」菜单里，常驻的只有提交框。

use super::*;
use crate::gitops::{self, Issue, SizePoint};

#[derive(Default)]
pub(super) struct Ops {
    pub msg: String,
    /// 提交前检查的结果：(问题, 提交后推送)
    pub issues: Option<(Vec<Issue>, bool)>,
    pub checking: bool,
    pub subs: Vec<(char, String, String)>,
    pub open_subs: bool,
    pub sizes: Vec<SizePoint>,
    /// 打标签窗口：(名字, 说明, 推送到远程)
    pub tag_win: Option<(String, String, bool)>,
}

pub(super) enum OpMsg {
    Checks(Vec<Issue>, bool),
    Subs(Vec<(char, String, String)>),
    Sizes(Vec<SizePoint>),
    Draft(String),
    /// 提交完成：成功时清空提交框
    Committed(String, Result<String, String>),
}

impl GitView {
    pub(super) fn load_extra(&self) {
        self.spawn(|r| Msg::Ops(OpMsg::Subs(gitops::submodules(&r))));
        self.spawn(|r| Msg::Ops(OpMsg::Sizes(gitops::load_sizes(&r))));
    }

    pub(super) fn on_ops(&mut self, m: OpMsg) {
        match m {
            OpMsg::Checks(issues, push) => {
                self.ops.checking = false;
                if issues.iter().all(|i| i.level == 0) && issues.is_empty() {
                    self.do_commit(push);
                } else {
                    self.ops.issues = Some((issues, push));
                }
            }
            OpMsg::Subs(s) => self.ops.subs = s,
            OpMsg::Sizes(s) => self.ops.sizes = s,
            OpMsg::Draft(d) => {
                if d.is_empty() {
                    self.toast(t("先暂存要提交的文件"), WARN());
                } else {
                    self.ops.msg = d;
                }
            }
            OpMsg::Committed(label, r) => {
                self.busy = None;
                match r {
                    Ok(_) => {
                        self.ops.msg.clear();
                        self.toast(tf("{} 完成", &[&label]), OK());
                    }
                    Err(e) => self.toast(tf("{} 失败：{}", &[&label, &friendly_err(&e)]), ERR()),
                }
                self.refresh();
            }
        }
    }

    fn staged_count(&self) -> usize {
        self.status.as_ref().map_or(0, |s| s.files.iter().filter(|f| f.x != ' ' && f.x != '?').count())
    }

    /// 点「提交」：先在后台做提交前检查，没问题就直接提交
    fn start_commit(&mut self, push: bool) {
        if self.ops.msg.trim().is_empty() {
            self.toast(t("先写一句提交说明（可以点「生成」）"), WARN());
            return;
        }
        self.ops.checking = true;
        self.ops.issues = None;
        let (unsaved, sens) = (self.unsaved.clone(), self.sensitive.clone());
        let stage_all = self.staged_count() == 0;
        self.spawn(move |r| {
            if stage_all {
                let _ = git::git(&r, &["add", "-A"]);
            }
            Msg::Ops(OpMsg::Checks(gitops::precommit(&r, &unsaved, &sens), push))
        });
    }

    fn do_commit(&mut self, push: bool) {
        self.ops.issues = None;
        let msg = self.ops.msg.trim().to_string();
        let label = if push { t("提交并推送") } else { t("提交") };
        self.busy = Some(label.clone());
        self.spawn(move |r| {
            let mut res = git::git(&r, &["commit", "-m", &msg]);
            if res.is_ok() {
                // 有编译产物就顺手记一笔固件体积（失败不影响提交）
                let _ = gitops::record_size(&r);
                if push {
                    res = push_cmd(&r);
                }
            }
            Msg::Ops(OpMsg::Committed(label, res))
        });
    }

    /// 推送；当前分支还没有上游时自动 -u origin
    fn push(&mut self) {
        self.busy = Some(t("推送"));
        self.spawn(|r| Msg::Op(t("推送"), push_cmd(&r)));
    }

    // ------------------------------------------------------------ 提交框

    pub(super) fn commit_box(&mut self, ui: &mut Ui) {
        let staged = self.staged_count();
        let n = self.dirty_count();
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 16,
                right: 14,
                top: 0,
                bottom: 8,
            })
            .show(ui, |ui| {
                let te = ui.add(
                    egui::TextEdit::multiline(&mut self.ops.msg)
                        .hint_text(t("提交说明（Ctrl+Enter 提交）"))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                let ctrl_enter = te.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                ui.add_space(4.0);
                let busy = self.busy.is_some() || self.ops.checking;
                ui.horizontal(|ui| {
                    let label = if staged == 0 { tf("提交全部 {} 处", &[&n]) } else { tf("提交 {} 个文件", &[&staged]) };
                    let go = ui.add_enabled(!busy && n > 0, egui::Button::new(RichText::new(label).color(ON_ACCENT()).strong()).fill(VIOLET()));
                    let go = go.on_hover_text(if staged == 0 {
                        t("没有暂存的文件时，提交全部更改")
                    } else {
                        t("只提交已暂存的文件")
                    });
                    if go.clicked() || (ctrl_enter && !busy && n > 0) {
                        self.start_commit(false);
                    }
                    if ui.add_enabled(!busy && n > 0, egui::Button::new(t("提交并推送"))).clicked() {
                        self.start_commit(true);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(t("生成")).on_hover_text(t("根据暂存的文件写一句提交说明")).clicked() {
                            let stage_all = staged == 0;
                            self.spawn(move |r| {
                                if stage_all {
                                    let _ = git::git(&r, &["add", "-A"]);
                                }
                                Msg::Ops(OpMsg::Draft(gitops::draft_message(&r)))
                            });
                            if stage_all {
                                self.load_status();
                            }
                        }
                        if self.ops.checking {
                            ui.spinner();
                        }
                    });
                });
                // 提交前检查发现的问题
                let mut act = None;
                if let Some((issues, push)) = &self.ops.issues {
                    ui.add_space(6.0);
                    egui::Frame::new().fill(BG2()).corner_radius(R_WIDGET).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(t("提交前检查")).strong().color(TEXT()));
                        for i in issues {
                            let (sym, c) = match i.level {
                                2 => ("●", ERR()),
                                1 => ("●", WARN()),
                                _ => ("●", TEXT3()),
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(sym).size(9.0).color(c));
                                ui.label(RichText::new(&i.text).size(12.0).color(TEXT2()));
                            });
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if ui.button(t("仍然提交")).clicked() {
                                act = Some(Some(*push));
                            }
                            if ui.button(t("取消")).clicked() {
                                act = Some(None);
                            }
                        });
                    });
                }
                match act {
                    Some(Some(push)) => self.do_commit(push),
                    Some(None) => self.ops.issues = None,
                    None => {}
                }
            });
    }

    /// 工作区文件的右键菜单里加的写操作
    pub(super) fn file_ops_menu(&mut self, ui: &mut Ui, f: &StatusFile) {
        let staged = f.x != ' ' && f.x != '?';
        let unstaged = f.y != ' ' || f.x == '?';
        if unstaged && ui.button(t("暂存")).clicked() {
            self.run_op(t("暂存"), vec!["add".into(), "--".into(), f.path.clone()]);
            ui.close_menu();
        }
        if staged && ui.button(t("取消暂存")).clicked() {
            self.run_op(t("取消暂存"), vec!["restore".into(), "--staged".into(), "--".into(), f.path.clone()]);
            ui.close_menu();
        }
        if ui.button(RichText::new(t("放弃修改…")).color(ERR())).clicked() {
            let args = if f.x == '?' {
                vec!["clean".into(), "-f".into(), "--".into(), f.path.clone()]
            } else {
                vec!["restore".into(), "--source=HEAD".into(), "--staged".into(), "--worktree".into(), "--".into(), f.path.clone()]
            };
            self.confirm = Some(Confirm {
                title: t("放弃修改"),
                body: tf("{} 会恢复成上次提交的样子，修改的内容在本地历史里还能找回。", &[&f.path]),
                ok: t("放弃"),
                args,
                label: t("放弃修改"),
            });
            ui.close_menu();
        }
        ui.separator();
    }

    pub(super) fn stage_all_button(&mut self, ui: &mut Ui) {
        if ui.small_button(t("全部暂存")).clicked() {
            self.run_op(t("全部暂存"), vec!["add".into(), "-A".into()]);
        }
    }

    // ------------------------------------------------------------ ⋯ 菜单

    pub(super) fn more_menu(&mut self, ui: &mut Ui) {
        let busy = self.busy.is_some();
        let r = icon_button(ui, Icon::More, &t("更多 Git 操作"), false);
        let hb = self.branches.iter().find(|b| b.head).cloned();
        r.context_menu(|ui| self.more_items(ui, busy, hb.as_ref()));
        if r.clicked() {
            r.ctx.memory_mut(|m| m.open_popup(egui::Id::new("git-more")));
        }
        egui::popup_below_widget(ui, egui::Id::new("git-more"), &r, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(200.0);
            self.more_items(ui, busy, hb.as_ref());
        });
    }

    fn more_items(&mut self, ui: &mut Ui, busy: bool, hb: Option<&Branch>) {
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button(t("拉取并合并（git pull）")).clicked() {
                self.run_op(t("拉取并合并"), vec!["pull".into(), "--ff-only".into()]);
                ui.close_menu();
            }
            let ahead = hb.map_or(0, |b| b.ahead);
            let push_label = if ahead > 0 { tf("推送（{} 个提交）", &[&ahead]) } else { t("推送") };
            if ui.button(push_label).clicked() {
                self.push();
                ui.close_menu();
            }
            ui.separator();
            if ui.button(t("撤销上次提交…")).clicked() {
                let pushed = hb.is_some_and(|b| !b.upstream.is_empty() && b.ahead == 0);
                let mut body = t("上次提交会撤销，但改动保留在暂存区，可以改完再提交。");
                if pushed {
                    body.push_str("\n\n");
                    body.push_str(&t("注意：这个提交已经推送到远程，撤销后再推送需要强制推送，会影响别人。"));
                }
                self.confirm = Some(Confirm {
                    title: t("撤销上次提交"),
                    body,
                    ok: t("撤销"),
                    args: vec!["reset".into(), "--soft".into(), "HEAD~1".into()],
                    label: t("撤销上次提交"),
                });
                ui.close_menu();
            }
            if ui.button(t("暂存当前修改（stash）")).on_hover_text(t("把没提交的修改先收起来，工作区变干净；之后可以恢复")).clicked() {
                self.run_op(t("暂存修改"), vec!["stash".into(), "push".into(), "-u".into(), "-m".into(), "QCCOM".into()]);
                ui.close_menu();
            }
            if ui.button(t("恢复暂存的修改")).clicked() {
                self.run_op(t("恢复暂存的修改"), vec!["stash".into(), "pop".into()]);
                ui.close_menu();
            }
            ui.separator();
            if ui.button(t("打标签…")).clicked() {
                let tags: Vec<String> = self.branches.iter().filter(|b| b.kind == RefKind::Tag).map(|b| b.name.clone()).collect();
                let name = gitops::next_tag(&tags).unwrap_or_else(|| "v1.0.0".into());
                self.ops.tag_win = Some((name, String::new(), false));
                ui.close_menu();
            }
            if ui
                .button(t("记录当前固件体积"))
                .on_hover_text(t("用工程里最新的 .map / .elf 给当前提交记一笔 ROM / RAM，提交时也会自动记"))
                .clicked()
            {
                self.spawn(|r| match gitops::record_size(&r) {
                    Ok(_) => Msg::Ops(OpMsg::Sizes(gitops::load_sizes(&r))),
                    Err(e) => Msg::Op(t("记录固件体积"), Err(e)),
                });
                ui.close_menu();
            }
            if !self.ops.subs.is_empty() && ui.button(t("更新子模块")).clicked() {
                self.run_op(t("更新子模块"), vec!["submodule".into(), "update".into(), "--init".into(), "--recursive".into()]);
                ui.close_menu();
            }
        });
    }

    pub(super) fn tag_window(&mut self, ctx: &egui::Context) {
        let Some((mut name, mut msg, mut push)) = self.ops.tag_win.take() else { return };
        let mut open = true;
        let mut go = false;
        egui::Window::new(t("打标签"))
            .id(egui::Id::new("git-tag"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new(t("给当前提交打一个版本标签，建议的版本号是在最新标签上加一")).size(11.5).color(TEXT3()));
                ui.add_space(6.0);
                ui.add(egui::TextEdit::singleline(&mut name).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(&mut msg).hint_text(t("说明（可不填）")).desired_width(f32::INFINITY));
                ui.checkbox(&mut push, t("同时推送到远程"));
                ui.add_space(6.0);
                if ui
                    .add_enabled(!name.trim().is_empty(), egui::Button::new(RichText::new(t("打标签")).color(ON_ACCENT())).fill(VIOLET()))
                    .clicked()
                {
                    go = true;
                }
            });
        if go {
            let (n, m) = (name.trim().to_string(), msg.trim().to_string());
            self.busy = Some(t("打标签"));
            self.spawn(move |r| {
                let mut res = if m.is_empty() { git::git(&r, &["tag", &n]) } else { git::git(&r, &["tag", "-a", &n, "-m", &m]) };
                if res.is_ok() && push {
                    res = git::git(&r, &["push", "origin", &n]);
                }
                Msg::Op(tf("打标签 {}", &[&n]), res)
            });
            return;
        }
        if open {
            self.ops.tag_win = Some((name, msg, push));
        }
    }

    // ------------------------------------------------------------ 侧边栏：子模块、体积历史

    pub(super) fn side_extra(&mut self, ui: &mut Ui) {
        if !self.ops.sizes.is_empty() {
            let last = self.ops.sizes.last().map(|p| format!("{} KB", (p.rom + 1023) / 1024));
            let r = nav_row(ui, Icon::Chart, &t("固件体积"), last, self.sel == Sel::Sizes, ACCENT());
            if r.clicked() {
                self.sel = Sel::Sizes;
                self.diff = None;
            }
        }
        if self.ops.subs.is_empty() {
            return;
        }
        let bad = self.ops.subs.iter().filter(|s| s.0 != ' ').count();
        ui.add_space(4.0);
        if section_header(ui, &t("子模块"), self.ops.subs.len(), self.ops.open_subs || bad > 0).clicked() {
            self.ops.open_subs = !self.ops.open_subs;
        }
        if !(self.ops.open_subs || bad > 0) {
            return;
        }
        let mut update = false;
        for (st, path, desc) in self.ops.subs.clone() {
            let (note, mark) = match st {
                '-' => (t("没有初始化"), Some('D')),
                '+' => (t("检出的版本和记录的不一样"), Some('M')),
                'U' => (t("有冲突"), Some('D')),
                _ => (desc.clone(), None),
            };
            let r = tree_row(ui, 0, &path, Some(&note), true, None, false, mark);
            r.context_menu(|ui| {
                if ui.button(t("更新子模块")).clicked() {
                    update = true;
                    ui.close_menu();
                }
            });
        }
        if update {
            self.run_op(t("更新子模块"), vec!["submodule".into(), "update".into(), "--init".into(), "--recursive".into()]);
        }
    }

    /// 右侧：固件体积曲线
    pub(super) fn sizes_panel(&mut self, ui: &mut Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin {
                left: 16,
                right: 14,
                top: 14,
                bottom: 10,
            })
            .show(ui, |ui| {
                ui.label(RichText::new(t("固件体积")).font(FontId::new(15.0, bold())).color(TEXT()));
                ui.label(RichText::new(t("每次在 QCCOM 里提交时自动记录（用工程里最新的 .map / .elf）")).size(11.5).color(TEXT3()));
                let pts = &self.ops.sizes;
                if pts.is_empty() {
                    return;
                }
                ui.add_space(10.0);
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 160.0), Sense::hover());
                let p = ui.painter();
                p.rect(rect, R_CARD, BG2(), st(1.0, LINE()), egui::StrokeKind::Inside);
                let inner = rect.shrink2(vec2(10.0, 14.0));
                let n = pts.len();
                let xs = |i: usize| if n == 1 { inner.center().x } else { inner.left() + inner.width() * i as f32 / (n - 1) as f32 };
                for (get, col) in [
                    (Box::new(|p: &SizePoint| p.rom) as Box<dyn Fn(&SizePoint) -> u64>, ACCENT()),
                    (Box::new(|p: &SizePoint| p.ram), VIOLET()),
                ] {
                    let (lo, hi) = pts.iter().map(&get).fold((u64::MAX, 0), |(a, b), v| (a.min(v), b.max(v)));
                    let span = (hi - lo).max(1) as f32;
                    let ys = |v: u64| inner.bottom() - inner.height() * (v - lo) as f32 / span * 0.9 - inner.height() * 0.05;
                    let line: Vec<egui::Pos2> = pts.iter().enumerate().map(|(i, pt)| pos2(xs(i), ys(get(pt)))).collect();
                    p.add(egui::Shape::line(line.clone(), st(1.6, col)));
                    for q in line {
                        p.circle_filled(q, 2.5, col);
                    }
                }
                if let Some(pos) = resp.hover_pos() {
                    let i = (((pos.x - inner.left()) / inner.width().max(1.0)) * (n.max(2) - 1) as f32).round().clamp(0.0, (n - 1) as f32) as usize;
                    p.vline(xs(i), inner.y_range(), st(1.0, TEXT3()));
                    let pt = &pts[i];
                    resp.on_hover_text(format!("{}  {}\nROM {} B   RAM {} B", &pt.hash[..8.min(pt.hash.len())], pt.subject, pt.rom, pt.ram));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("━ ROM").color(ACCENT()));
                    ui.label(RichText::new("━ RAM").color(VIOLET()));
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let mut prev: Option<&SizePoint> = None;
                    let mut rows = vec![];
                    for pt in pts.iter() {
                        let d = prev.map(|q| (pt.rom as i64 - q.rom as i64, pt.ram as i64 - q.ram as i64));
                        rows.push((pt, d));
                        prev = Some(pt);
                    }
                    for (pt, d) in rows.into_iter().rev() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&pt.hash[..7.min(pt.hash.len())]).font(FontId::new(11.5, FontFamily::Monospace)).color(ACCENT()));
                            ui.label(
                                RichText::new(format!("ROM {:>7}  RAM {:>6}", pt.rom, pt.ram))
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(TEXT2()),
                            );
                            if let Some((dr, dm)) = d {
                                let fmt = |v: i64| if v > 0 { format!("+{v}") } else { v.to_string() };
                                let c = |v: i64| {
                                    if v > 0 {
                                        WARN()
                                    } else if v < 0 {
                                        OK()
                                    } else {
                                        TEXT3()
                                    }
                                };
                                ui.label(RichText::new(fmt(dr)).font(FontId::new(11.0, FontFamily::Monospace)).color(c(dr)));
                                ui.label(RichText::new(fmt(dm)).font(FontId::new(11.0, FontFamily::Monospace)).color(c(dm)));
                            }
                        });
                        ui.label(RichText::new(&pt.subject).size(11.5).color(TEXT3()));
                        ui.add_space(4.0);
                    }
                });
            });
    }
}

fn push_cmd(r: &Path) -> Result<String, String> {
    match git::git(r, &["push"]) {
        Err(e) if e.contains("no upstream") || e.contains("has no upstream") => git::git(r, &["push", "-u", "origin", "HEAD"]),
        x => x,
    }
}

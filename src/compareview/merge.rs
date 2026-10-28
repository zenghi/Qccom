//! 三方合并：上面左 / 基准 / 右三栏对照，冲突处一键选用哪边；下面是可以直接改的合并结果。

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Ui};

use super::{mono, name_of, nav, step, Out, Side};
use crate::compare::diff::{self, CommentStyle, Opts};
use crate::compare::merge::{build, diff3, Chunk, ChunkKind, Pick};
use crate::highlight::Lang;
use crate::i18n::{t, tf};
use crate::theme::*;

const RH: f32 = 19.0;
const HEAD: f32 = 28.0;

fn read_lines(p: &Path) -> Result<(Vec<String>, &'static str, bool), String> {
    if p.as_os_str().is_empty() || !p.exists() {
        // 没有基准：当成空文件（两边都是新增）
        return Ok((vec![], "\n", false));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let (text, gbk) = crate::cindex::decode(&bytes).ok_or_else(|| tf("{} 是二进制文件，不能做文本合并", &[&p.display()]))?;
    let (mut v, eol) = diff::split_lines(&text);
    if text.is_empty() {
        v.clear();
    }
    Ok((v, eol, gbk))
}

pub struct MergeTab {
    sides: [Side; 3],
    out: PathBuf,
    git_root: Option<PathBuf>,
    /// 基准、左、右
    lines: [Vec<String>; 3],
    eol: &'static str,
    gbk: bool,
    lang: Lang,
    chunks: Vec<Chunk>,
    /// 每个冲突的选择
    picks: Vec<Option<Pick>>,
    result: String,
    /// 结果被手动改过
    manual: bool,
    pub saved: bool,
    cur: Option<usize>,
    scroll_to: Option<usize>,
}

impl MergeTab {
    pub fn new(base: Side, l: Side, r: Side, out: &Path, git_root: Option<PathBuf>) -> Result<Self, String> {
        let (lb, _, _) = read_lines(&base.path)?;
        let (ll, eol, gbk) = read_lines(&l.path)?;
        let (lr, _, _) = read_lines(&r.path)?;
        let k = |v: &[String]| diff::keys(v, &Opts::default(), CommentStyle::None).0;
        let chunks = diff3(&k(&lb), &k(&ll), &k(&lr));
        let n = chunks.iter().filter(|c| c.kind == ChunkKind::Conflict).count();
        let mut m = Self {
            lang: Lang::from_path(out),
            sides: [base, l, r],
            out: out.to_path_buf(),
            git_root,
            lines: [lb, ll, lr],
            eol,
            gbk,
            chunks,
            picks: vec![None; n],
            result: String::new(),
            manual: false,
            saved: false,
            cur: None,
            scroll_to: None,
        };
        m.rebuild();
        if n > 0 {
            m.goto(Some(0));
        }
        Ok(m)
    }

    pub fn title(&self) -> String {
        format!("{} ({})", name_of(&self.out), t("合并"))
    }

    fn conflicts(&self) -> Vec<usize> {
        self.chunks.iter().enumerate().filter(|(_, c)| c.kind == ChunkKind::Conflict).map(|(i, _)| i).collect()
    }

    fn unresolved(&self) -> usize {
        self.picks.iter().filter(|p| p.is_none()).count()
    }

    fn rebuild(&mut self) {
        let v = build(&self.lines[0], &self.lines[1], &self.lines[2], &self.chunks, &self.picks);
        self.result = v.join(self.eol);
        if !v.is_empty() {
            self.result.push_str(self.eol);
        }
        self.manual = false;
        self.saved = false;
    }

    fn pick(&mut self, ci: usize, p: Option<Pick>) {
        if let Some(x) = self.picks.get_mut(ci) {
            *x = p;
            self.rebuild();
        }
    }

    fn goto(&mut self, c: Option<usize>) {
        self.cur = c;
        if let Some(ch) = c.and_then(|c| self.conflicts().get(c).copied()) {
            self.scroll_to = Some(ch);
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        let bytes = if self.gbk {
            encoding_rs::GBK.encode(&self.result).0.into_owned()
        } else {
            self.result.clone().into_bytes()
        };
        crate::history::save_tracked(&self.out, &bytes).map_err(|e| format!("{}: {e}", self.out.display()))?;
        self.saved = true;
        Ok(())
    }

    fn has_markers(&self) -> bool {
        self.result.lines().any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
    }

    // ------------------------------------------------------------ 界面

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Out> {
        let mut outs = vec![];
        self.toolbar(ui, &mut outs);
        egui::TopBottomPanel::bottom(ui.id().with("merge-out"))
            .resizable(true)
            .default_height(ui.available_height() * 0.4)
            .min_height(120.0)
            .frame(egui::Frame::new().fill(BG0()))
            .show_inside(ui, |ui| self.output(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show_inside(ui, |ui| self.three(ui));
        if ui.memory(|m| m.focused().is_none()) || ui.input(|i| i.modifiers.command) {
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
                self.do_save(false, &mut outs);
            }
        }
        outs
    }

    fn do_save(&mut self, mark: bool, outs: &mut Vec<Out>) {
        if let Err(e) = self.save() {
            outs.push(Out::Toast(e, ERR()));
            return;
        }
        if self.has_markers() {
            outs.push(Out::Toast(t("已保存，但还有冲突标记没处理").into(), WARN()));
            return;
        }
        if mark {
            if let Some(root) = &self.git_root {
                let rel = self.out.strip_prefix(root).unwrap_or(&self.out).to_string_lossy().replace('\\', "/");
                match crate::git::git(root, &["add", "--", &rel]) {
                    Ok(_) => outs.push(Out::Toast(tf("{} 已标记为解决", &[&rel]), OK())),
                    Err(e) => outs.push(Out::Toast(e, ERR())),
                }
                return;
            }
        }
        outs.push(Out::Toast(tf("已保存 {}", &[&self.out.display()]), OK()));
    }

    fn toolbar(&mut self, ui: &mut Ui, outs: &mut Vec<Out>) {
        egui::Frame::new().fill(BG1()).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let n = self.picks.len();
                if let Some(next) = nav(ui, n, self.cur) {
                    let c = step(self.cur, n, next);
                    self.goto(c);
                }
                ui.add_space(6.0);
                let left = self.unresolved();
                let auto = self.chunks.iter().filter(|c| matches!(c.kind, ChunkKind::Left | ChunkKind::Right | ChunkKind::Both)).count();
                if n == 0 {
                    ui.label(RichText::new(tf("没有冲突，{} 处改动已自动合并", &[&auto])).color(OK()));
                } else {
                    let c = if left == 0 { OK() } else { ERR() };
                    ui.label(RichText::new(tf("{} 处冲突，还剩 {} 处", &[&n, &left])).color(c));
                    ui.label(RichText::new(tf("{} 处已自动合并", &[&auto])).small().color(TEXT3()));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.git_root.is_some() {
                        let b = egui::Button::new(RichText::new(t("保存并标记已解决")).color(ON_ACCENT())).fill(ACCENT());
                        if ui.add(b).on_hover_text("git add").clicked() {
                            self.do_save(true, outs);
                        }
                        if ui.button(t("保存")).on_hover_text("Ctrl+S").clicked() {
                            self.do_save(false, outs);
                        }
                    } else {
                        let b = egui::Button::new(RichText::new(t("保存")).color(ON_ACCENT())).fill(ACCENT());
                        if ui.add(b).on_hover_text("Ctrl+S").clicked() {
                            self.do_save(false, outs);
                        }
                    }
                    ui.label(RichText::new(format!("→ {}", name_of(&self.out))).small().color(TEXT3()))
                        .on_hover_text(self.out.display().to_string());
                    if n > 0 {
                        if ui.small_button(t("全部用右边")).clicked() {
                            self.picks.iter_mut().for_each(|p| *p = Some(Pick::Right));
                            self.rebuild();
                        }
                        if ui.small_button(t("全部用左边")).clicked() {
                            self.picks.iter_mut().for_each(|p| *p = Some(Pick::Left));
                            self.rebuild();
                        }
                    }
                });
            });
        });
    }

    fn three(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let gap = 8.0;
        let cw = (full.width() - gap * 2.0) / 3.0;
        // 栏标题：左、基准、右
        let hr = Rect::from_min_size(full.min, vec2(full.width(), 26.0));
        ui.allocate_rect(hr, Sense::hover());
        ui.painter().rect_filled(hr, 0.0, BG1());
        let order = [1usize, 0, 2];
        for (k, &si) in order.iter().enumerate() {
            let x = hr.left() + k as f32 * (cw + gap);
            let label = match si {
                1 => t("左边"),
                0 => t("基准"),
                _ => t("右边"),
            };
            let p = ui.painter();
            p.text(pos2(x + 10.0, hr.center().y), Align2::LEFT_CENTER, label, FontId::new(11.0, bold()), TEXT3());
            let g = p.layout_no_wrap(self.sides[si].label.clone(), FontId::proportional(12.5), TEXT());
            p.with_clip_rect(Rect::from_min_size(pos2(x, hr.top()), vec2(cw, hr.height())))
                .galley(pos2(x + 52.0, hr.center().y - g.size().y / 2.0), g, TEXT());
        }
        let conflicts = self.conflicts();
        let cur_chunk = self.cur.and_then(|c| conflicts.get(c).copied());
        // 每块的高度
        let heights: Vec<f32> = self
            .chunks
            .iter()
            .map(|c| {
                let n = (c.base.1 - c.base.0).max(c.l.1 - c.l.0).max(c.r.1 - c.r.0).max(1);
                n as f32 * RH + if c.kind == ChunkKind::Conflict { HEAD } else { 0.0 }
            })
            .collect();
        let mut sa = egui::ScrollArea::vertical().id_salt("merge-three").auto_shrink([false, false]);
        if let Some(ch) = self.scroll_to.take() {
            let y: f32 = heights[..ch.min(heights.len())].iter().sum();
            sa = sa.vertical_scroll_offset((y - 80.0).max(0.0));
        }
        let mut picks: Vec<(usize, Option<Pick>)> = vec![];
        ui.spacing_mut().item_spacing.y = 0.0;
        sa.show(ui, |ui| {
            let mut ci = 0;
            for (k, c) in self.chunks.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), heights[k]), Sense::hover());
                let is_conf = c.kind == ChunkKind::Conflict;
                let my_ci = ci;
                if is_conf {
                    ci += 1;
                }
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let p = ui.painter().clone();
                let mut body = rect;
                if is_conf {
                    let h = Rect::from_min_size(rect.min, vec2(rect.width(), HEAD));
                    body.min.y += HEAD;
                    let on = cur_chunk == Some(k);
                    p.rect_filled(h, 0.0, ERR().gamma_multiply(if on { 0.20 } else { 0.12 }));
                    let pick = self.picks[my_ci];
                    let label = match pick {
                        None => tf("冲突 {}", &[&(my_ci + 1)]),
                        Some(pk) => format!("{} {} · {}", t("冲突"), my_ci + 1, pick_name(pk)),
                    };
                    p.text(
                        pos2(h.left() + 10.0, h.center().y),
                        Align2::LEFT_CENTER,
                        label,
                        FontId::new(12.0, bold()),
                        if pick.is_some() { OK() } else { ERR() },
                    );
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(h.shrink2(vec2(8.0, 2.0))).layout(Layout::right_to_left(Align::Center)));
                    for pk in [Some(Pick::Base), Some(Pick::RightLeft), Some(Pick::LeftRight), Some(Pick::Right), Some(Pick::Left)] {
                        let sel = pick == pk;
                        let b = egui::Button::new(RichText::new(pick_name(pk.unwrap())).size(11.5).color(if sel { ON_ACCENT() } else { TEXT() }))
                            .fill(if sel { ACCENT() } else { BG2() })
                            .min_size(vec2(0.0, 22.0));
                        if child.add(b).clicked() {
                            picks.push((my_ci, if sel { None } else { pk }));
                        }
                    }
                }
                for (col, &si) in order.iter().enumerate() {
                    let x = rect.left() + col as f32 * (cw + gap);
                    let r = Rect::from_min_max(pos2(x, body.top()), pos2(x + cw, body.bottom()));
                    let span = match si {
                        0 => c.base,
                        1 => c.l,
                        _ => c.r,
                    };
                    let changed = match (c.kind, si) {
                        (ChunkKind::Conflict, 0) => Some(TEXT3().gamma_multiply(0.10)),
                        (ChunkKind::Conflict, _) => Some(ERR().gamma_multiply(0.12)),
                        (ChunkKind::Left, 1) | (ChunkKind::Right, 2) => Some(ACCENT().gamma_multiply(0.12)),
                        (ChunkKind::Both, 1 | 2) => Some(OK().gamma_multiply(0.12)),
                        _ => None,
                    };
                    if let Some(bg) = changed {
                        p.rect_filled(r, 0.0, bg);
                    }
                    let clip = p.with_clip_rect(r.intersect(ui.clip_rect()));
                    for (k, li) in (span.0..span.1).enumerate() {
                        let y = r.top() + k as f32 * RH;
                        if y + RH < ui.clip_rect().top() || y > ui.clip_rect().bottom() {
                            continue;
                        }
                        clip.text(pos2(r.left() + 36.0, y + RH / 2.0), Align2::RIGHT_CENTER, (li + 1).to_string(), FontId::monospace(10.5), TEXT3());
                        super::text::draw_text(ui, &clip, pos2(r.left() + 44.0, y + 2.0), &self.lines[si][li], self.lang, &[], Color32::TRANSPARENT);
                    }
                    if span.0 == span.1 && changed.is_some() {
                        clip.text(pos2(r.left() + 44.0, r.top() + RH / 2.0), Align2::LEFT_CENTER, t("（删除）"), FontId::proportional(11.0), TEXT3());
                    }
                }
                if c.kind != ChunkKind::Same {
                    p.hline(rect.x_range(), rect.bottom() - 0.5, st(1.0, LINE()));
                }
            }
        });
        for (ci, pk) in picks {
            self.cur = Some(ci);
            self.pick(ci, pk);
        }
    }

    fn output(&mut self, ui: &mut Ui) {
        let r = ui.available_rect_before_wrap();
        let hr = Rect::from_min_size(r.min, vec2(r.width(), 28.0));
        ui.allocate_rect(hr, Sense::hover());
        let p = ui.painter();
        p.rect_filled(hr, 0.0, BG1());
        p.hline(hr.x_range(), hr.top() + 0.5, st(1.0, LINE()));
        p.text(pos2(hr.left() + 10.0, hr.center().y), Align2::LEFT_CENTER, t("合并结果"), FontId::new(12.5, bold()), TEXT());
        let note = if self.manual {
            t("已手动修改；再点冲突按钮会重新生成")
        } else {
            t("可以直接在这里修改")
        };
        p.text(pos2(hr.left() + 90.0, hr.center().y), Align2::LEFT_CENTER, note, FontId::proportional(11.0), TEXT3());
        if self.saved {
            p.text(pos2(hr.right() - 10.0, hr.center().y), Align2::RIGHT_CENTER, t("已保存"), FontId::proportional(11.0), OK());
        }
        let lang = self.lang;
        let mut layouter = |ui: &Ui, s: &str, wrap: f32| {
            let mut job = crate::highlight::layout(s, lang, mono());
            job.wrap.max_width = wrap;
            // 冲突标记行标红
            let mut off = 0;
            for line in s.split_inclusive('\n') {
                if line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line.starts_with("=======") || line.starts_with("|||||||") {
                    for sec in job.sections.iter_mut() {
                        if sec.byte_range.start >= off && sec.byte_range.start < off + line.len() {
                            sec.format.color = ERR();
                            sec.format.background = ERR().gamma_multiply(0.12);
                        }
                    }
                }
                off += line.len();
            }
            ui.fonts(|f| f.layout_job(job))
        };
        egui::ScrollArea::vertical().id_salt("merge-out-scroll").auto_shrink([false, false]).show(ui, |ui| {
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.result)
                    .font(mono())
                    .code_editor()
                    .frame(false)
                    .margin(vec2(12.0, 6.0))
                    .desired_width(f32::INFINITY)
                    .layouter(&mut layouter),
            );
            if resp.changed() {
                self.manual = true;
                self.saved = false;
            }
        });
    }
}

fn pick_name(p: Pick) -> &'static str {
    match p {
        Pick::Left => t("用左边"),
        Pick::Right => t("用右边"),
        Pick::LeftRight => t("左 + 右"),
        Pick::RightLeft => t("右 + 左"),
        Pick::Base => t("用基准"),
    }
}

//! 代码页的 v1.3 增强：代码生成动作（模板 + 输入框）、F8 多色高亮、导航（历史、上次编辑处、
//! 头文件/源文件切换、上下函数、#if 配对、声明、侧边定义）、#if 灰显与当前块底色、
//! 按键录制回放、右键菜单、每个工程记住打开的文件和书签。

use std::collections::{BTreeMap, VecDeque};

use super::*;
use crate::app::menu_item;
use crate::codegen::{self, Ctx, Place};
use crate::keymap::{self, ACTIONS};

/// F8 高亮的 8 种颜色（深浅主题下都够醒目）
pub(super) const HL_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xF5, 0xB7, 0x00),
    Color32::from_rgb(0x3F, 0xB9, 0x50),
    Color32::from_rgb(0x22, 0xB8, 0xCF),
    Color32::from_rgb(0xF0, 0x62, 0x92),
    Color32::from_rgb(0xA3, 0x71, 0xF7),
    Color32::from_rgb(0xFF, 0x8C, 0x42),
    Color32::from_rgb(0x4C, 0x8D, 0xFF),
    Color32::from_rgb(0xA3, 0xD9, 0x32),
];

/// 录下来的一步
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RecItem {
    Text(String),
    Key { key: String, ctrl: bool, shift: bool, alt: bool },
    Act(String),
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct KeyMacro {
    pub name: String,
    pub items: Vec<RecItem>,
}

/// 每个工程记住的状态
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ProjState {
    pub tabs: Vec<String>,
    pub active: usize,
    /// 文件 → (行, 行内字节)
    pub cursors: BTreeMap<String, (usize, usize)>,
    pub bookmarks: BTreeMap<String, Vec<usize>>,
}

pub(super) enum PromptKind {
    Macro(usize),
    EnumSwitch(Vec<String>),
    SaveRec,
}

/// 执行动作前要问的几个输入
pub(super) struct Prompt {
    kind: PromptKind,
    title: String,
    fields: Vec<(String, String)>,
    cx: Ctx,
    focus: bool,
    /// 输入框出现的位置（光标附近）
    at: Option<egui::Pos2>,
}

/// 能被录进按键宏的编辑命令
const REC_ACTS: &[Act] = &[
    Act::ToggleComment,
    Act::BlockComment,
    Act::MoveLineUp,
    Act::MoveLineDown,
    Act::CopyLineUp,
    Act::CopyLineDown,
    Act::DeleteLine,
    Act::InsertLineBelow,
    Act::InsertLineAbove,
    Act::SelectLine,
    Act::AddNextOcc,
    Act::SelectAllOcc,
    Act::CursorAbove,
    Act::CursorBelow,
    Act::GotoBracket,
    Act::CaseConvert,
    Act::IfZero,
    Act::NextFunc,
    Act::PrevFunc,
    Act::HlNext,
    Act::HlPrev,
];

#[derive(Default)]
pub(super) struct GenState {
    /// F8 高亮：(词, 颜色下标)
    pub marks: Vec<(String, u8)>,
    next_color: u8,
    pub prompt: Option<Prompt>,
    /// 最后一次编辑的位置
    pub last_edit: Option<(PathBuf, usize)>,
    /// 上一帧光标所在（文件, 行），用来发现大跳转、记进后退历史
    last_pos: Option<(PathBuf, usize)>,
    /// 程序自己跳转后的几帧不记
    pub quiet: u8,
    /// 正在录制
    pub rec: Option<Vec<RecItem>>,
    pub last_rec: Vec<RecItem>,
    play: VecDeque<RecItem>,
    /// 回放开始前先让编辑器拿到焦点
    play_wait: u8,
    /// 右键菜单开着（这时不显示悬浮卡片）
    pub menu_open: bool,
}

fn act_by_id(id: &str) -> Option<Act> {
    ACTIONS.iter().find(|d| d.id == id).map(|d| d.act)
}

impl CodeView {
    // ------------------------------------------------------------ 每帧

    /// 在 shortcuts 之前调用：录制、回放、鼠标侧键。返回回放要执行的命令
    pub(super) fn gen_frame(&mut self, ctx: &egui::Context, acts: &[Act]) -> Vec<Act> {
        // 鼠标侧键：后退 / 前进
        let (b1, b2) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        if b1 {
            self.nav(true);
        }
        if b2 {
            self.nav(false);
        }
        let focused = self.tabs.get(self.active).is_some_and(|d| ctx.memory(|m| m.has_focus(d.ed.id)));
        if let Some(rec) = self.gen.rec.as_mut() {
            if focused {
                ctx.input(|i| {
                    for e in &i.events {
                        match e {
                            egui::Event::Text(s) => rec.push(RecItem::Text(s.clone())),
                            egui::Event::Paste(s) => rec.push(RecItem::Text(s.clone())),
                            // 普通字母、数字、空格会同时有文字事件，只录文字
                            egui::Event::Key { key, pressed: true, modifiers, .. }
                                if modifiers.command || modifiers.ctrl || modifiers.alt || (key.name().chars().count() > 1 && *key != Key::Space) =>
                            {
                                rec.push(RecItem::Key {
                                    key: key.name().to_string(),
                                    ctrl: modifiers.command || modifiers.ctrl,
                                    shift: modifiers.shift,
                                    alt: modifiers.alt,
                                })
                            }
                            _ => {}
                        }
                    }
                });
            }
            for a in acts {
                if REC_ACTS.contains(a) || matches!(a, Act::Macro(_)) {
                    rec.push(RecItem::Act(keymap::def(*a).id.to_string()));
                }
            }
        }
        // 回放：先执行开头的命令，再把文字和按键放进这一帧的输入，遇到下一个命令就停，下一帧接着
        let mut out = vec![];
        if !self.gen.play.is_empty() {
            if let Some(d) = self.tabs.get_mut(self.active) {
                d.ed.focus_req = true;
            }
            if self.gen.play_wait > 0 {
                self.gen.play_wait -= 1;
                ctx.request_repaint();
                return out;
            }
            while let Some(RecItem::Act(id)) = self.gen.play.front() {
                if let Some(a) = act_by_id(id) {
                    out.push(a);
                }
                self.gen.play.pop_front();
            }
            let mut evs = vec![];
            while let Some(it) = self.gen.play.front() {
                match it {
                    RecItem::Act(_) => break,
                    RecItem::Text(s) => evs.push(egui::Event::Text(s.clone())),
                    RecItem::Key { key, ctrl, shift, alt } => {
                        if let Some(k) = Key::from_name(key) {
                            let modifiers = Modifiers {
                                alt: *alt,
                                ctrl: *ctrl,
                                shift: *shift,
                                mac_cmd: false,
                                command: *ctrl,
                            };
                            evs.push(egui::Event::Key {
                                key: k,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers,
                            });
                        }
                    }
                }
                self.gen.play.pop_front();
            }
            ctx.input_mut(|i| i.events.extend(evs));
            ctx.request_repaint();
        }
        out
    }

    /// 编辑器显示之后：记录跳转历史和最后编辑的位置
    pub(super) fn track_position(&mut self, changed: bool) {
        let Some(d) = self.tabs.get(self.active) else { return };
        let cur = (d.path.clone(), d.cursor.0);
        if changed {
            self.gen.last_edit = Some((d.path.clone(), d.ed.head()));
        }
        if self.gen.quiet > 0 {
            self.gen.quiet -= 1;
        } else if let Some(prev) = &self.gen.last_pos {
            // 同一个文件里一下跳了 30 行以上（点击、Ctrl+End、搜索结果……）就记一笔，后退能回来
            if prev.0 == cur.0 && prev.1.abs_diff(cur.1) >= 30 && !changed && self.back.last() != Some(prev) {
                self.back.push(prev.clone());
                if self.back.len() > 100 {
                    self.back.remove(0);
                }
                self.fwd.clear();
            }
        }
        self.gen.last_pos = Some(cur);
    }

    // ------------------------------------------------------------ 显示参数

    pub(super) fn mark_colors(&self) -> Vec<(String, Color32)> {
        self.gen.marks.iter().map(|(w, c)| (w.clone(), HL_COLORS[*c as usize % HL_COLORS.len()])).collect()
    }

    /// 不生效的 #if 分支（有缓存）
    pub(super) fn inactive_of(&mut self, i: usize) -> Vec<(usize, usize)> {
        if !self.s.dim_inactive {
            return vec![];
        }
        let idx = self.index.clone();
        let key = idx.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize as u64) ^ hash_str(&self.s.defines);
        let extra = parse_defines(&self.s.defines);
        let d = &mut self.tabs[i];
        if !matches!(d.lang, Lang::C) || d.buf.text.len() > 3_000_000 {
            return vec![];
        }
        if d.inactive.0 == d.buf.ver && d.inactive.1 == key {
            return d.inactive.2.clone();
        }
        let path = d.path.clone();
        let lookup = |name: &str| -> Option<String> {
            if let Some(v) = extra.get(name) {
                return Some(v.clone());
            }
            let idx = idx.as_ref()?;
            idx.lookup(name)
                .into_iter()
                .find(|s| s.kind == Kind::Macro && idx.path_of(s) != path)
                .map(|s| if s.params.is_some() { "\u{0}".to_string() } else { s.value.clone().unwrap_or_default() })
        };
        let v = crate::cpp::inactive(&d.buf.text, &lookup);
        d.inactive = (d.buf.ver, key, v.clone());
        v
    }

    /// 光标所在函数体和 #if 块（有缓存）
    pub(super) fn scopes_of(&mut self, i: usize) -> (Option<(usize, usize)>, Option<(usize, usize)>) {
        if !self.s.scope_bg {
            return (None, None);
        }
        let d = &mut self.tabs[i];
        if d.lang != Lang::C || d.buf.text.len() > 3_000_000 {
            return (None, None);
        }
        let l = d.buf.line_of(d.ed.head());
        if d.scope.0 == d.buf.ver && d.scope.1 == l {
            return (d.scope.2, d.scope.3);
        }
        let func = enclosing_func(d, l).map(|(_, a, b)| (a, b));
        let blk = crate::cpp::enclosing(&d.buf.text, l);
        d.scope = (d.buf.ver, l, func, blk);
        (func, blk)
    }

    // ------------------------------------------------------------ 命令

    /// 处理 v1.3 加的命令；返回是否处理了
    pub(super) fn gen_act(&mut self, ctx: &egui::Context, a: Act) -> bool {
        let now = ctx.input(|i| i.time);
        match a {
            Act::HlToggle => self.hl_toggle(),
            Act::HlRemove => {
                if let Some(w) = self.hl_word() {
                    self.gen.marks.retain(|(x, _)| *x != w);
                }
            }
            Act::HlClear => self.gen.marks.clear(),
            Act::HlNext | Act::HlPrev => {
                // 查找框开着时 F3 是查找下一个
                if self.find.is_some() && a == Act::HlNext && !self.find.as_ref().is_some_and(|f| f.hits.is_empty()) {
                    self.find_step(false);
                } else if self.find.is_some() && a == Act::HlPrev {
                    self.find_step(true);
                } else {
                    self.hl_jump(a == Act::HlPrev);
                }
            }
            Act::NavHistory => {
                self.quick = Some(Quick { q: "<".into(), sel: 0, focus: true });
            }
            Act::LastEdit => {
                if let Some((p, b)) = self.gen.last_edit.clone() {
                    self.open_file(&p, None);
                    if let Some(d) = self.tabs.get_mut(self.active) {
                        let b = b.min(d.buf.text.len());
                        d.goto = Some((d.buf.line_of(b), Some((b, b))));
                    }
                } else {
                    self.toast(t("还没有编辑过"), TEXT3());
                }
            }
            Act::SwitchHeader => self.switch_header(),
            Act::MatchIf => {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    match crate::cpp::matching(&d.buf.text, d.cursor.0) {
                        Some(l) => {
                            let ls = d.buf.start(l);
                            let ind = d.buf.line(l).len() - d.buf.line(l).trim_start().len();
                            d.ed.select(ls + ind, ls + ind);
                            d.ed.focus_req = true;
                        }
                        None => self.toast(t("光标不在 #if 块里"), TEXT3()),
                    }
                }
            }
            Act::PrevFunc | Act::NextFunc => {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    let l = d.cursor.0 as u32;
                    let mut lines: Vec<u32> = d.syms.iter().filter(|s| matches!(s.kind, Kind::Func | Kind::Class)).map(|s| s.line).collect();
                    lines.sort();
                    let to = if a == Act::PrevFunc {
                        lines.iter().rev().find(|&&x| x < l)
                    } else {
                        lines.iter().find(|&&x| x > l)
                    };
                    if let Some(&x) = to {
                        let s = d.buf.start(x as usize);
                        d.ed.select(s, s);
                        d.ed.focus_req = true;
                    }
                }
            }
            Act::GotoDecl => {
                if let Some(w) = self.cursor_word() {
                    let at = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(pos2(400.0, 200.0));
                    self.goto_declaration(&w, at);
                }
            }
            Act::DefToSide => {
                if let Some(w) = self.cursor_word() {
                    self.def_to_side(&w);
                }
            }
            Act::BlockComment | Act::IfZero | Act::AlignSel | Act::CaseConvert => {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    let lang = d.lang;
                    match a {
                        Act::BlockComment => d.ed.toggle_block_comment(&mut d.buf, lang, now),
                        Act::IfZero => d.ed.toggle_if0(&mut d.buf, now),
                        Act::CaseConvert => d.ed.map_words(&mut d.buf, codegen::cycle_case, now),
                        _ => {
                            let (la, lb) = d.ed.sel_line_span(&d.buf);
                            let lines: Vec<&str> = (la..=lb).map(|l| d.buf.line(l)).collect();
                            match codegen::align(&lines) {
                                Some(v) => d.ed.replace_lines(&mut d.buf, la, lb, &v, now),
                                None => {
                                    self.toast(t("选中几行含 = 或 , 的代码再对齐"), TEXT3());
                                    return true;
                                }
                            }
                        }
                    }
                    d.ed.focus_req = true;
                    d.syms_stale = Some(now);
                    d.edited_at = Some(now);
                }
            }
            Act::RemoveDebug => {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    let n = d.ed.delete_marked_lines(&mut d.buf, codegen::MARK, now);
                    d.syms_stale = Some(now);
                    d.edited_at = Some(now);
                    self.toast(
                        if n == 0 {
                            t("当前文件里没有插入的调试打印").to_string()
                        } else {
                            tf("已删除 {} 行调试打印", &[&n])
                        },
                        if n == 0 { TEXT3() } else { OK() },
                    );
                }
            }
            Act::EnumSwitch => self.enum_switch(),
            Act::DeclToHeader => self.decl_to_header(now),
            Act::RecToggle => match self.gen.rec.take() {
                Some(v) => {
                    let n = v.len();
                    if n > 0 {
                        self.gen.last_rec = v;
                    }
                    self.toast(tf("录制结束：{} 步（{} 回放）", &[&n, &"Shift+Alt+P"]), OK());
                }
                None => {
                    self.gen.rec = Some(vec![]);
                    self.toast(t("开始录制按键…再按一次停止"), ACCENT());
                }
            },
            Act::RecPlay => {
                if self.gen.rec.is_some() {
                    self.toast(t("先停止录制"), WARN());
                } else if self.gen.last_rec.is_empty() {
                    self.toast(t("还没有录制按键（Shift+Alt+R 开始）"), TEXT3());
                } else {
                    let v = self.gen.last_rec.clone();
                    self.play_keys(v);
                }
            }
            Act::RecSave => {
                if self.gen.last_rec.is_empty() {
                    self.toast(t("还没有录制按键（Shift+Alt+R 开始）"), TEXT3());
                } else {
                    self.gen.prompt = Some(Prompt {
                        kind: PromptKind::SaveRec,
                        title: t("保存按键宏").to_string(),
                        fields: vec![(t("名字").to_string(), tf("按键宏 {}", &[&(self.s.recordings.len() + 1)]))],
                        cx: Ctx::default(),
                        focus: true,
                        at: None,
                    });
                }
            }
            Act::Macro(i) => self.run_macro(i as usize, ctx),
            _ => return false,
        }
        true
    }

    pub(super) fn play_keys(&mut self, v: Vec<RecItem>) {
        self.gen.play = v.into();
        self.gen.play_wait = 1;
    }

    // ------------------------------------------------------------ 高亮

    fn hl_word(&self) -> Option<String> {
        let d = self.tabs.get(self.active)?;
        let s = d.ed.primary_sel();
        if !s.empty() {
            let t = &d.buf.text[s.range()];
            return (!t.contains('\n') && t.len() < 200).then(|| t.to_string());
        }
        self.cursor_word()
    }

    fn hl_toggle(&mut self) {
        let Some(w) = self.hl_word() else { return };
        if let Some(m) = self.gen.marks.iter_mut().find(|(x, _)| *x == w) {
            // 已经高亮了：换下一种颜色
            m.1 = (m.1 + 1) % HL_COLORS.len() as u8;
            return;
        }
        let used: Vec<u8> = self.gen.marks.iter().map(|m| m.1).collect();
        let c = (0..HL_COLORS.len() as u8)
            .map(|k| (self.gen.next_color + k) % HL_COLORS.len() as u8)
            .find(|c| !used.contains(c))
            .unwrap_or(self.gen.next_color);
        self.gen.next_color = (c + 1) % HL_COLORS.len() as u8;
        self.gen.marks.push((w, c));
    }

    fn hl_jump(&mut self, back: bool) {
        let cur = self.hl_word().filter(|w| self.gen.marks.iter().any(|m| m.0 == *w));
        let words: Vec<String> = match cur {
            Some(w) => vec![w],
            None => self.gen.marks.iter().map(|m| m.0.clone()).collect(),
        };
        if words.is_empty() {
            self.toast(t("还没有高亮的词（F8 高亮光标处的词）"), TEXT3());
            return;
        }
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        let text = &d.buf.text;
        let mut hits: Vec<(usize, usize)> = vec![];
        for w in &words {
            let mut from = 0;
            while let Some(k) = text[from..].find(w.as_str()) {
                let s = from + k;
                from = s + w.len();
                if !text[..s].chars().next_back().is_some_and(is_ident) && !text[s + w.len()..].chars().next().is_some_and(is_ident) {
                    hits.push((s, s + w.len()));
                }
            }
        }
        hits.sort();
        let h = d.ed.primary_sel().min();
        let to = if back {
            hits.iter().rev().find(|x| x.0 < h).or(hits.last())
        } else {
            hits.iter().find(|x| x.0 > h).or(hits.first())
        };
        if let Some(&(a, b)) = to {
            d.ed.select(a, b);
            d.ed.focus_req = true;
        }
    }

    // ------------------------------------------------------------ 导航

    fn switch_header(&mut self) {
        let Some(d) = self.tabs.get(self.active) else { return };
        let p = d.path.clone();
        let ext = p.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let want: &[&str] = match ext.as_str() {
            "h" | "hpp" | "hh" | "hxx" => &["c", "cpp", "cc", "cxx", "s", "S"],
            "c" | "cpp" | "cc" | "cxx" | "s" => &["h", "hpp", "hh", "hxx"],
            _ => {
                self.toast(t("只有 C / C++ 文件可以切换"), TEXT3());
                return;
            }
        };
        let found = self.partner(&p, want);
        match found {
            Some(f) => self.open_file(&f, None),
            None => self.toast(tf("没有找到 {} 对应的文件", &[&p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()]), TEXT3()),
        }
    }

    /// 对应的头文件 / 源文件：先找同目录，再找工程里同名的文件（include/ 与 src/ 分开放的工程）
    fn partner(&self, p: &Path, want: &[&str]) -> Option<PathBuf> {
        if let Some(c) = want.iter().map(|e| p.with_extension(e)).find(|c| c.is_file()) {
            return Some(c);
        }
        let stem = p.file_stem()?.to_string_lossy().to_string();
        let (idx, root) = (self.index.as_ref()?, self.root.as_ref()?);
        idx.files
            .iter()
            .find(|f| f.file_stem().is_some_and(|s| s.to_string_lossy() == stem) && f.extension().is_some_and(|x| want.iter().any(|w| x.to_string_lossy().eq_ignore_ascii_case(w))))
            .map(|f| root.join(f))
    }

    fn goto_declaration(&mut self, word: &str, at: egui::Pos2) {
        let decls: Vec<(PathBuf, u32, &'static str, String)> = self.definitions(word).into_iter().filter(|x| x.3.trim_end().ends_with(';') || x.2 == Kind::Proto.label()).collect();
        match decls.len() {
            0 => {
                self.toast(tf("没有找到 {} 的单独声明，已跳到定义", &[&word]), TEXT3());
                self.goto_definition_index(word, at);
            }
            1 => {
                let (p, l, _, _) = decls[0].clone();
                self.open_at_word(&p, l as usize, word);
            }
            _ => self.pick = Some((word.to_string(), decls, at)),
        }
    }

    fn def_to_side(&mut self, word: &str) {
        let Some(cur) = self.tabs.get(self.active).map(|d| d.path.clone()) else { return };
        let defs = self.definitions(word);
        let Some((p, l, _, _)) = defs.into_iter().next() else {
            self.toast(tf("没找到 {} 的定义", &[&word]), WARN());
            return;
        };
        self.open_at_word(&p, l as usize, word);
        if p != cur {
            self.split = Some(Split {
                other: cur,
                active_left: false,
                last: p,
            });
        }
    }

    // ------------------------------------------------------------ 代码生成

    /// 光标处的变量 / 选中的表达式
    fn gen_word(&self) -> String {
        let Some(d) = self.tabs.get(self.active) else { return String::new() };
        let s = d.ed.primary_sel();
        if !s.empty() {
            let t = d.buf.text[s.range()].trim();
            if !t.contains('\n') && t.len() < 120 {
                return t.to_string();
            }
        }
        // 光标停在 a.b / p->x / arr[i] 的最后一段上时把整个表达式取出来
        let h = d.ed.head();
        let Some((a, b)) = d.buf.word_at(h) else { return String::new() };
        let line_start = d.buf.start(d.buf.line_of(a));
        let mut s = a;
        loop {
            let before = &d.buf.text[line_start..s];
            let pre = if before.ends_with("->") {
                2
            } else if before.ends_with('.') {
                1
            } else {
                break;
            };
            let head = &before[..before.len() - pre];
            let n: usize = head.chars().rev().take_while(|c| is_ident(*c)).map(char::len_utf8).sum();
            if n == 0 {
                break;
            }
            s -= pre + n;
        }
        let mut e = b;
        // 下标 arr[i]
        if d.buf.text[e..].starts_with('[') {
            if let Some(c) = d.buf.text[e..].find(']').filter(|c| *c < 40 && !d.buf.text[e..e + c].contains('\n')) {
                e += c + 1;
            }
        }
        d.buf.text[s..e].to_string()
    }

    fn base_ctx(&self) -> Ctx {
        let mut cx = Ctx {
            author: self.s.author.clone(),
            copyright: self.s.copyright.clone(),
            print: self.s.print_fn.clone(),
            ..Default::default()
        };
        if let Some(d) = self.tabs.get(self.active) {
            cx.file = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if let Some((name, _, _)) = enclosing_func(d, d.cursor.0) {
                cx.func = name;
            }
        }
        cx
    }

    pub(super) fn run_macro(&mut self, i: usize, ctx: &egui::Context) {
        let Some(def) = self.s.macros.get(i).cloned() else {
            self.toast(t("这个槽位还没有设置代码生成动作（设置 → 代码 → 代码生成）"), TEXT3());
            return;
        };
        if self.tabs.is_empty() || self.tabs[self.active].virt {
            return;
        }
        let mut cx = self.base_ctx();
        let tpl = &def.template;
        let uses = |v: &str| tpl.contains(&format!("${{{v}}}"));
        if uses("WORD") || uses("ARG") || uses("FMT") || tpl.contains("${WORD}") {
            cx.word = self.gen_word();
        }
        let mut fields: Vec<(String, String)> = codegen::inputs_of(tpl).into_iter().map(|(l, dft)| (l, dft)).collect();
        if (tpl.contains("${WORD}") || uses("ARG")) && cx.word.is_empty() {
            fields.insert(0, (t("变量 / 表达式").to_string(), String::new()));
        }
        // 输入框的默认值：上次输入的，或模板里写的默认值
        for f in &mut fields {
            if let Some(r) = self.s.macro_recent.get(&f.0).and_then(|v| v.first()) {
                if f.1.is_empty() || !f.1.contains("${") {
                    f.1 = r.clone();
                    continue;
                }
            }
            f.1 = cx.simple(&f.1);
        }
        if fields.is_empty() {
            self.finish_macro(i, cx, ctx.input(|i| i.time));
        } else {
            self.gen.prompt = Some(Prompt {
                kind: PromptKind::Macro(i),
                title: def.name.clone(),
                fields,
                cx,
                focus: true,
                at: None,
            });
        }
    }

    /// 新建的 .c / .h：写文件头，.h 再加头文件保护（设置里可以关）
    pub(super) fn auto_header_new_file(&mut self) {
        if !self.s.auto_header {
            return;
        }
        let Some(d) = self.tabs.get(self.active) else { return };
        if !d.buf.text.is_empty() {
            return;
        }
        let ext = d.path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !matches!(ext.as_str(), "c" | "h" | "cpp" | "hpp" | "cc") {
            return;
        }
        let now = 0.0;
        if ext.starts_with('h') {
            if let Some(i) = self.s.macros.iter().position(|m| m.builtin == "guard") {
                let cx = self.base_ctx();
                self.finish_macro(i, cx, now);
            }
        }
        if let Some(i) = self.s.macros.iter().position(|m| m.builtin == "fileheader") {
            let cx = self.base_ctx();
            self.finish_macro(i, cx, now);
        }
    }

    fn finish_macro(&mut self, i: usize, mut cx: Ctx, now: f64) {
        let Some(def) = self.s.macros.get(i).cloned() else { return };
        if let Some(w) = cx.inputs.get(t("变量 / 表达式")).cloned() {
            cx.word = w;
        }
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        let tpl = def.template.clone();
        if tpl.contains("${FMT}") || tpl.contains("${ARG}") {
            let h = d.ed.head();
            let (before, after) = d.buf.text.split_at(h.min(d.buf.text.len()));
            cx.ty = codegen::infer_type(before, after, &cx.word).unwrap_or_default();
        }
        let cur_line = d.buf.line_of(d.ed.head());
        let ind_of = |l: usize, b: &Buf| -> String {
            let s = b.line(l);
            s[..s.len() - s.trim_start().len()].to_string()
        };
        // 位置：(替换的范围, 缩进, 前缀, 后缀)
        let (range, indent, pre, post): (std::ops::Range<usize>, String, &str, &str) = match def.place {
            Place::Selection => {
                let (a, b) = d.ed.sel_line_span(&d.buf);
                let r = d.buf.start(a)..d.buf.end(b);
                cx.selection = d.buf.text[r.clone()].to_string();
                (r, String::new(), "", "")
            }
            Place::Cursor => {
                let s = d.ed.primary_sel();
                cx.selection = d.buf.text[s.range()].to_string();
                (s.range(), String::new(), "", "")
            }
            Place::LineBelow => {
                let (_, b) = d.ed.sel_line_span(&d.buf);
                let e = d.buf.end(b);
                // 当前行以 { 结尾时缩进多一级
                let mut ind = ind_of(b, &d.buf);
                if d.buf.line(b).trim_end().ends_with('{') {
                    ind.push_str(&d.buf.indent_unit());
                }
                (e..e, ind, "\n", "")
            }
            Place::LineAbove => {
                let (a, _) = d.ed.sel_line_span(&d.buf);
                let s = d.buf.start(a);
                (s..s, ind_of(a, &d.buf), "", "\n")
            }
            Place::FuncAbove => {
                let Some((name, fl, _)) = func_for_doc(d, cur_line) else {
                    self.toast(t("光标附近没有函数"), TEXT3());
                    return;
                };
                let _ = name;
                cx.sig = sig_text(&d.buf, fl);
                let s = d.buf.start(fl);
                (s..s, ind_of(fl, &d.buf), "", "\n")
            }
            Place::FileTop => (0..0, String::new(), "", ""),
            Place::WholeFile => {
                cx.selection = d.buf.text.trim_end().to_string();
                (0..d.buf.text.len(), String::new(), "", "")
            }
        };
        let (body, cur) = codegen::expand(&tpl, &cx);
        // 用一个私用区字符标出光标，加缩进后再找回来
        const CUR: char = '\u{E000}';
        let mut body = body;
        if let Some(c) = cur {
            body.insert(c, CUR);
        }
        let body: String = if indent.is_empty() {
            body
        } else {
            body.split('\n')
                .map(|l| if l.is_empty() { l.to_string() } else { format!("{indent}{l}") })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut text = format!("{pre}{body}{post}");
        let cursor = text.find(CUR);
        if let Some(c) = cursor {
            text.remove(c);
        }
        d.ed.replace_at(&mut d.buf, range, text, cursor, now);
        d.ed.focus_req = true;
        d.syms_stale = Some(now);
        d.edited_at = Some(now);
        // 记住输入过的值
        for (k, v) in cx.inputs {
            if v.trim().is_empty() {
                continue;
            }
            let list = self.s.macro_recent.entry(k).or_default();
            list.retain(|x| *x != v);
            list.insert(0, v);
            list.truncate(8);
        }
    }

    /// /** 回车：在这一行生成函数说明
    pub(super) fn doc_comment_at(&mut self, line: usize, now: f64) {
        let cx = self.base_ctx();
        let Some(i) = self.s.macros.iter().position(|m| m.builtin == "doxfunc") else { return };
        let tpl = self.s.macros[i].template.clone();
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        let mut cx = cx;
        // /** 下面的函数
        let next = d.syms.iter().filter(|s| matches!(s.kind, Kind::Func | Kind::Proto) && s.line as usize > line).min_by_key(|s| s.line);
        let fl = match next {
            Some(s) if (s.line as usize) <= line + 3 => s.line as usize,
            _ => {
                // 下面没有函数：普通的块注释
                let ind = {
                    let s = d.buf.line(line);
                    s[..s.len() - s.trim_start().len()].to_string()
                };
                let r = d.buf.start(line)..d.buf.end(line);
                let text = format!("{ind}/**\n{ind} * \n{ind} */");
                let c = ind.len() * 2 + 7;
                d.ed.replace_at(&mut d.buf, r, text, Some(c), now);
                return;
            }
        };
        cx.sig = sig_text(&d.buf, fl);
        let ind = {
            let s = d.buf.line(line);
            s[..s.len() - s.trim_start().len()].to_string()
        };
        let (body, cur) = codegen::expand(&tpl, &cx);
        const CUR: char = '\u{E000}';
        let mut body = body;
        if let Some(c) = cur {
            body.insert(c, CUR);
        }
        let mut text: String = body.split('\n').map(|l| format!("{ind}{l}")).collect::<Vec<_>>().join("\n");
        let c = text.find(CUR);
        if let Some(c) = c {
            text.remove(c);
        }
        let r = d.buf.start(line)..d.buf.end(line);
        d.ed.replace_at(&mut d.buf, r, text, c, now);
        d.syms_stale = Some(now);
    }

    fn enum_switch(&mut self) {
        let Some(w) = self.cursor_word() else {
            self.toast(t("把光标放在枚举类型名上"), TEXT3());
            return;
        };
        // 在索引里找这个名字对应的枚举（enum 名或 typedef 名）
        let mut consts = vec![];
        let mut found: Option<(PathBuf, u32)> = None;
        if let Some(idx) = &self.index {
            for s in idx.lookup(&w) {
                if matches!(s.kind, Kind::Enum | Kind::Typedef) {
                    found = Some((idx.path_of(s).to_path_buf(), s.line));
                    break;
                }
            }
        }
        if found.is_none() {
            if let Some(d) = self.tabs.get(self.active) {
                if let Some(s) = d.syms.iter().find(|s| s.name == w && matches!(s.kind, Kind::Enum | Kind::Typedef)) {
                    found = Some((d.path.clone(), s.line));
                }
            }
        }
        if let Some((p, l)) = found {
            if let Some(text) = self.file_text_of(&p) {
                // 从定义行往前后找 enum { ... }
                let lines: Vec<&str> = text.split('\n').collect();
                let l = l as usize;
                let from = (l.saturating_sub(60)..=l).rev().find(|&i| lines.get(i).is_some_and(|x| x.contains("enum"))).unwrap_or(l);
                let mut body = String::new();
                for x in lines.iter().skip(from).take(400) {
                    body.push_str(x);
                    body.push('\n');
                    if x.contains('}') {
                        break;
                    }
                }
                consts = codegen::enum_consts(&body);
            }
        }
        if consts.is_empty() {
            self.toast(tf("{} 不是枚举（把光标放在枚举类型名上）", &[&w]), TEXT3());
            return;
        }
        self.gen.prompt = Some(Prompt {
            kind: PromptKind::EnumSwitch(consts),
            title: t("由枚举生成 switch").to_string(),
            fields: vec![(t("变量").to_string(), String::new())],
            cx: Ctx::default(),
            focus: true,
            at: None,
        });
    }

    fn file_text_of(&self, p: &Path) -> Option<String> {
        if let Some(d) = self.tabs.iter().find(|d| d.path == p) {
            return Some(d.buf.text.clone());
        }
        cindex::read_text(p)
    }

    /// 在 .c 的函数定义上：把声明加进同名 .h；在 .h 的声明上：在同名 .c 里加一个空定义
    fn decl_to_header(&mut self, now: f64) {
        let Some(d) = self.tabs.get(self.active) else { return };
        let ext = d.path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let is_header = matches!(ext.as_str(), "h" | "hpp" | "hh");
        let l = d.cursor.0;
        let sym = d
            .syms
            .iter()
            .filter(|s| matches!(s.kind, Kind::Func | Kind::Proto) && (s.line as usize) <= l)
            .max_by_key(|s| s.line)
            .cloned();
        let Some(sym) = sym else {
            self.toast(t("把光标放在函数上"), TEXT3());
            return;
        };
        let sig = sig_text(&d.buf, sym.line as usize);
        if !is_header && sig.split_whitespace().next() == Some("static") {
            self.toast(t("static 函数不需要放进头文件"), TEXT3());
            return;
        }
        let src = d.path.clone();
        let wants: &[&str] = if is_header { &["c", "cpp", "cc"] } else { &["h", "hpp"] };
        let Some(target) = self.partner(&src, wants) else {
            self.toast(tf("工程里没有找到对应的 {} 文件", &[&wants[0]]), WARN());
            return;
        };
        let (snippet, name) = if is_header {
            (codegen::def_of(&sig), sym.name.clone())
        } else {
            (codegen::decl_of(&sig), sym.name.clone())
        };
        self.open_file(&target, None);
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        if d.path != target {
            return;
        }
        // 已经有了就不加
        let exists = d.syms.iter().any(|s| s.name == name && if is_header { s.kind == Kind::Func } else { s.kind == Kind::Proto });
        if exists {
            self.toast(tf("{} 里已经有 {}", &[&target.file_name().unwrap_or_default().to_string_lossy(), &name]), TEXT3());
            return;
        }
        // 头文件：放在 extern "C" 的 } 或者最后一个 #endif 之前；源文件：放在末尾
        let text = &d.buf.text;
        let at = if is_header {
            text.len()
        } else {
            let lines: Vec<&str> = text.split('\n').collect();
            let mut at_line = None;
            for (i, ln) in lines.iter().enumerate().rev() {
                let tl = ln.trim();
                if tl.starts_with("#endif") || tl == "}" && lines.get(i.saturating_sub(1)).is_some_and(|p| p.trim() == "#ifdef __cplusplus") {
                    at_line = Some(i);
                    if tl.starts_with("#endif") {
                        // 跳过 extern "C" 收尾那三行
                        if i >= 2 && lines[i - 1].trim() == "}" && lines[i - 2].trim() == "#ifdef __cplusplus" {
                            at_line = Some(i - 2);
                        }
                    }
                    break;
                }
                if !tl.is_empty() {
                    break;
                }
            }
            match at_line {
                Some(i) => d.buf.start(i),
                None => text.len(),
            }
        };
        let need_nl = at > 0 && !d.buf.text[..at].ends_with("\n\n");
        let ins = if is_header {
            format!("{}\n{snippet}", if d.buf.text.ends_with('\n') { "" } else { "\n" })
        } else {
            format!(
                "{}{snippet}\n\n",
                if need_nl && d.buf.text[..at].ends_with('\n') {
                    ""
                } else if need_nl {
                    "\n"
                } else {
                    ""
                }
            )
        };
        let c = if is_header { ins.len() - 3 } else { ins.len() - 2 };
        d.ed.replace_at(&mut d.buf, at..at, ins, Some(c), now);
        d.ed.focus_req = true;
        d.syms_stale = Some(now);
        d.edited_at = Some(now);
        self.toast(tf("已添加到 {}", &[&target.file_name().unwrap_or_default().to_string_lossy()]), OK());
    }

    // ------------------------------------------------------------ 输入框

    pub(super) fn prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(pr) = self.gen.prompt.as_mut() else { return };
        let mut done = false;
        let mut cancel = ctx.input(|i| i.key_pressed(Key::Escape));
        let screen = ctx.screen_rect();
        let w = 420.0f32.min(screen.width() - 40.0);
        let pos = pr.at.unwrap_or(pos2(screen.center().x - w / 2.0, screen.top() + 90.0));
        let recent = self.s.macro_recent.clone();
        egui::Area::new(egui::Id::new("gen-prompt")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::new()
                .fill(BG2())
                .stroke(st(1.0, ACCENT().gamma_multiply(0.4)))
                .corner_radius(CornerRadius::same(10))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 10],
                    blur: 40,
                    spread: 0,
                    color: shade(200),
                })
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(w);
                    ui.label(RichText::new(&pr.title).font(FontId::new(13.5, bold())).color(TEXT()));
                    ui.add_space(8.0);
                    let n = pr.fields.len();
                    for (k, (label, val)) in pr.fields.iter_mut().enumerate() {
                        ui.label(RichText::new(label.as_str()).size(12.0).color(TEXT2()));
                        let te = ui.add_sized(vec2(w, 30.0), egui::TextEdit::singleline(val).font(FontId::new(13.0, FontFamily::Monospace)).margin(vec2(8.0, 6.0)));
                        if pr.focus && k == 0 {
                            te.request_focus();
                            // 全选默认值，直接打字就替换
                            if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                                s.cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(val.chars().count()))));
                                s.store(ui.ctx(), te.id);
                            }
                        }
                        if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && k + 1 == n {
                            done = true;
                        }
                        // 最近用过的值
                        if let Some(list) = recent.get(label.as_str()).filter(|l| !l.is_empty()) {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                                for r in list.iter().take(6) {
                                    let b = egui::Button::new(RichText::new(r).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT2()))
                                        .fill(BG3())
                                        .corner_radius(R_WIDGET);
                                    if ui.add(b).clicked() {
                                        *val = r.clone();
                                    }
                                }
                            });
                        }
                        ui.add_space(6.0);
                    }
                    pr.focus = false;
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t("Enter 确定 · Esc 取消")).size(11.5).color(TEXT3()));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(RichText::new(t("确定")).color(ON_ACCENT())).fill(ACCENT()).corner_radius(R_WIDGET)).clicked() {
                                done = true;
                            }
                            if ui.button(t("取消")).clicked() {
                                cancel = true;
                            }
                        });
                    });
                });
        });
        if cancel {
            self.gen.prompt = None;
            if let Some(d) = self.tabs.get_mut(self.active) {
                d.ed.focus_req = true;
            }
            return;
        }
        if !done {
            return;
        }
        let pr = self.gen.prompt.take().unwrap();
        let now = ctx.input(|i| i.time);
        match pr.kind {
            PromptKind::Macro(i) => {
                let mut cx = pr.cx;
                for (k, v) in pr.fields {
                    cx.inputs.insert(k, v);
                }
                self.finish_macro(i, cx, now);
            }
            PromptKind::EnumSwitch(consts) => {
                let var = pr.fields.first().map(|f| f.1.trim().to_string()).unwrap_or_default();
                let var = if var.is_empty() { "/* 变量 */".to_string() } else { var };
                if let Some(d) = self.tabs.get_mut(self.active) {
                    let l = d.cursor.0;
                    let line = d.buf.line(l);
                    let ind = line[..line.len() - line.trim_start().len()].to_string();
                    let unit = d.buf.indent_unit();
                    let sw = codegen::enum_switch(&var, &consts, &ind, &unit);
                    let e = d.buf.end(l);
                    let text = format!("\n{ind}{sw}");
                    d.ed.replace_at(&mut d.buf, e..e, text, None, now);
                    d.ed.focus_req = true;
                    d.syms_stale = Some(now);
                    d.edited_at = Some(now);
                }
            }
            PromptKind::SaveRec => {
                let name = pr.fields.first().map(|f| f.1.trim().to_string()).unwrap_or_default();
                if !name.is_empty() {
                    self.s.recordings.retain(|r| r.name != name);
                    self.s.recordings.push(KeyMacro {
                        name: name.clone(),
                        items: self.gen.last_rec.clone(),
                    });
                    self.toast(tf("已保存按键宏「{}」，右键菜单 → 代码生成里可以回放", &[&name]), OK());
                }
            }
        }
    }

    // ------------------------------------------------------------ 右键菜单

    /// 光标所在（或选中）的结构体定义，连同文件里它前面的内容（里面可能有用到的其他类型）
    fn struct_source(&self) -> Option<String> {
        let d = self.tabs.get(self.active)?;
        let text = &d.buf.text;
        let sel = d.ed.primary_sel();
        let (a, b) = (sel.a.min(sel.h), sel.a.max(sel.h));
        if b > a && text[a..b].contains('{') {
            return Some(text[..b].to_string());
        }
        // 往外找包住光标的 struct / union 花括号，取最外层
        let bytes = text.as_bytes();
        let (mut depth, mut i, mut open) = (0, a.min(bytes.len()), None);
        while i > 0 {
            i -= 1;
            match bytes[i] {
                b'}' => depth += 1,
                b'{' if depth > 0 => depth -= 1,
                b'{' => {
                    let from = text[..i].rfind([';', '}', '{']).map_or(0, |p| p + 1);
                    let head = &text[from..i];
                    if head.contains("struct") || head.contains("union") {
                        open = Some(i);
                    } else {
                        break;
                    }
                }
                _ => {}
            }
        }
        let open = open?;
        let mut depth = 0;
        let mut close = None;
        for (k, c) in text[open..].bytes().enumerate() {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = close.and_then(|c| text[c..].find(';').map(|p| c + p + 1))?;
        Some(text[..end].to_string())
    }

    pub(super) fn editor_menu(&mut self, ui: &mut Ui, km: &dyn Fn(Act) -> String) -> Option<Act> {
        let mut pick = None;
        let mut item = |ui: &mut Ui, a: Act, label: &str| {
            if menu_item(ui, label, &km(a)) {
                pick = Some(a);
                ui.close_menu();
            }
        };
        ui.set_min_width(260.0);
        item(ui, Act::GotoDef, t("跳转到定义"));
        item(ui, Act::GotoDecl, t("跳转到声明"));
        item(ui, Act::DefToSide, t("在侧边打开定义"));
        item(ui, Act::FindRefs, t("查找引用"));
        ui.separator();
        item(ui, Act::HlToggle, t("高亮当前词"));
        if !self.gen.marks.is_empty() {
            item(ui, Act::HlClear, t("清除全部高亮"));
        }
        item(ui, Act::ToggleComment, t("切换行注释"));
        item(ui, Act::BlockComment, t("切换块注释"));
        item(ui, Act::IfZero, t("用 #if 0 包裹 / 取消"));
        if let Some(sel) = self.selected_text(ui.ctx()) {
            if menu_item(ui, t("发到社区求助"), "") {
                let lang = self.tabs.get(self.active).and_then(|d| d.path.extension()).and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                self.events.push(CodeEvt::Ask(sel, lang));
                ui.close_menu();
            }
        }
        ui.separator();
        let macros: Vec<(usize, String)> = self.s.macros.iter().enumerate().map(|(i, m)| (i, m.name.clone())).collect();
        let recs: Vec<(usize, String)> = self.s.recordings.iter().enumerate().map(|(i, m)| (i, m.name.clone())).collect();
        let mut play: Option<usize> = None;
        ui.menu_button(t("代码生成"), |ui| {
            ui.set_min_width(280.0);
            for (i, name) in &macros {
                item(ui, Act::Macro(*i as u8), name);
            }
            ui.separator();
            item(ui, Act::EnumSwitch, t("由枚举生成 switch"));
            item(ui, Act::DeclToHeader, t("在头文件加声明 / 在源文件加定义"));
            item(ui, Act::RemoveDebug, t("删除插入的调试打印"));
            ui.separator();
            if menu_item(ui, t("编辑代码生成动作…"), "") {
                self.events.push(CodeEvt::OpenSettings);
                ui.close_menu();
            }
        });
        ui.menu_button(t("编辑"), |ui| {
            ui.set_min_width(280.0);
            item(ui, Act::AlignSel, t("按 = 或 , 对齐选中的行"));
            item(ui, Act::CaseConvert, t("切换命名风格（snake / camel / Pascal / 大写）"));
            item(ui, Act::MatchIf, t("跳到配对的 #if / #else / #endif"));
            item(ui, Act::SwitchHeader, t("切换头文件 / 源文件"));
            ui.separator();
            if menu_item(ui, t("解析结构体（工具箱）"), "") {
                match self.struct_source() {
                    Some(s) => self.events.push(CodeEvt::Struct(s)),
                    None => self.toast(t("把光标放在结构体定义里，或者选中它"), TEXT3()),
                }
                ui.close_menu();
            }
        });
        ui.menu_button(t("按键宏"), |ui| {
            ui.set_min_width(260.0);
            let rec_label = if self.gen.rec.is_some() { t("停止录制") } else { t("开始录制") };
            item(ui, Act::RecToggle, rec_label);
            item(ui, Act::RecPlay, t("回放"));
            item(ui, Act::RecSave, t("保存为宏…"));
            if !recs.is_empty() {
                ui.separator();
                for (i, name) in &recs {
                    if menu_item(ui, name, "") {
                        play = Some(*i);
                        ui.close_menu();
                    }
                }
            }
        });
        if let Some(i) = play {
            if let Some(m) = self.s.recordings.get(i) {
                let v = m.items.clone();
                self.play_keys(v);
            }
        }
        pick
    }

    // ------------------------------------------------------------ 工程状态

    /// 把当前工程打开的文件、光标、书签记下来
    pub fn snapshot(&mut self) {
        let Some(root) = self.root.as_ref().map(|r| r.to_string_lossy().to_string()) else { return };
        let mut st = ProjState {
            active: self.active,
            ..Default::default()
        };
        for d in self.tabs.iter().filter(|d| !d.virt) {
            let p = d.path.to_string_lossy().to_string();
            st.tabs.push(p.clone());
            let h = d.ed.head();
            let l = d.buf.line_of(h);
            st.cursors.insert(p.clone(), (l, h - d.buf.start(l)));
            if !d.ed.bookmarks.is_empty() {
                st.bookmarks.insert(p, d.ed.bookmarks.iter().copied().collect());
            }
        }
        self.s.projects.insert(root, st);
        // 最多记 20 个工程
        while self.s.projects.len() > 20 {
            let keep: Vec<String> = self.s.recent.clone();
            let Some(k) = self.s.projects.keys().find(|k| !keep.contains(k)).cloned() else { break };
            self.s.projects.remove(&k);
        }
    }

    /// 打开工程后恢复它上次的文件
    pub(super) fn restore_project(&mut self, root: &Path) {
        let key = root.to_string_lossy().to_string();
        let Some(st) = self.s.projects.get(&key).cloned() else { return };
        for p in &st.tabs {
            let pb = PathBuf::from(p);
            if !pb.is_file() {
                continue;
            }
            self.open_file_nohist(&pb, None);
            if let Some(d) = self.tabs.iter_mut().find(|d| d.path == pb) {
                if let Some(&(l, col)) = st.cursors.get(p) {
                    let l = l.min(d.buf.lines().saturating_sub(1));
                    let b = (d.buf.start(l) + col).min(d.buf.end(l));
                    d.goto = Some((l, Some((b, b))));
                }
                if let Some(bm) = st.bookmarks.get(p) {
                    let n = d.buf.lines();
                    d.ed.bookmarks = bm.iter().copied().filter(|l| *l < n).collect();
                }
            }
        }
        if !self.tabs.is_empty() {
            self.active = st.active.min(self.tabs.len() - 1);
        }
        self.gen.quiet = 3;
    }
}

/// 光标所在的函数：(名字, 起始行, 函数体最后一行)
fn enclosing_func(d: &Doc, l: usize) -> Option<(String, usize, usize)> {
    let mut cands: Vec<&Sym> = d.syms.iter().filter(|s| s.kind == Kind::Func && (s.line as usize) <= l).collect();
    cands.sort_by_key(|s| std::cmp::Reverse(s.line));
    for s in cands.into_iter().take(3) {
        if let Some((_, end)) = brace_span(&d.buf, s.line as usize) {
            if l <= end {
                return Some((s.name.clone(), s.line as usize, end));
            }
        }
    }
    None
}

/// 生成函数说明时用哪个函数：光标所在行或下面三行内开始的函数，否则光标所在的函数
fn func_for_doc(d: &Doc, l: usize) -> Option<(String, usize, usize)> {
    if let Some(s) = d
        .syms
        .iter()
        .filter(|s| matches!(s.kind, Kind::Func | Kind::Proto) && (s.line as usize) >= l && (s.line as usize) <= l + 3)
        .min_by_key(|s| s.line)
    {
        return Some((s.name.clone(), s.line as usize, s.line as usize));
    }
    enclosing_func(d, l)
}

/// 从某行开始的 { … } 块：(左括号所在行, 右括号所在行)；跳过字符串和注释
fn brace_span(b: &Buf, from: usize) -> Option<(usize, usize)> {
    let start = b.start(from);
    let text = &b.text[start..];
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut open_at = None;
    let mut i = 0;
    let limit = bytes.len().min(2_000_000);
    while i < limit {
        let c = bytes[i];
        match c {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < limit && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < limit && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'"' | b'\'' => {
                i += 1;
                while i < limit && bytes[i] != c && bytes[i] != b'\n' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b';' if open_at.is_none() => return None,
            b'{' => {
                if open_at.is_none() {
                    open_at = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let o = open_at?;
                    return Some((b.line_of(start + o), b.line_of(start + i)));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 函数签名原文：从起始行到 { 或 ; 为止
fn sig_text(b: &Buf, l: usize) -> String {
    let mut s = String::new();
    for i in l..(l + 12).min(b.lines()) {
        let line = b.line(i);
        let cut = line.find(['{', ';']);
        s.push_str(&line[..cut.unwrap_or(line.len())]);
        if cut.is_some() {
            break;
        }
        s.push(' ');
    }
    s.trim().to_string()
}

fn hash_str(s: &str) -> u64 {
    s.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
}

/// 设置里额外定义的宏：DEBUG, USE_HAL=1, STM32F407xx
fn parse_defines(s: &str) -> HashMap<String, String> {
    s.split([',', ';', '\n', ' '])
        .filter(|x| !x.trim().is_empty())
        .map(|x| {
            let x = x.trim().trim_start_matches("-D");
            match x.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (x.to_string(), "1".to_string()),
            }
        })
        .collect()
}

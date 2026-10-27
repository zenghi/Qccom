//! 代码查看器（类 VS Code）：打开文件夹、文件树、多标签、语法高亮、跳转定义、宏展开悬浮、
//! 大纲、Ctrl+P 快速打开、全局搜索、文件内查找、前进后退、小地图、Git 状态标记。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Key, Modifiers, Rect, RichText, Sense, Shape, Ui};
use serde::{Deserialize, Serialize};

use crate::app::{draw_icon, hgrad_line, icon_button, Icon};
use crate::cindex::{self, Hit, Index, Kind, Progress, SearchOpts, Sym};
use crate::editor::{Buf, Editor, View};

mod insight;
use crate::git;
use crate::highlight::{self, is_ident, Lang};
use crate::i18n::{t, tf};
use crate::keymap::Act;
use crate::lsp;
use crate::theme::*;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct CodeSettings {
    pub root: String,
    pub recent: Vec<String>,
    pub font_size: f32,
    pub tabs: Vec<String>,
    pub side_width: f32,
    pub minimap: bool,
    pub ctx_height: f32,
    /// 有 clangd 和 compile_commands.json 时用 clangd 跳转定义
    pub clangd: bool,
    /// 空 = 自动查找
    pub clangd_path: String,
}

impl Default for CodeSettings {
    fn default() -> Self {
        Self {
            root: String::new(),
            recent: vec![],
            font_size: 13.5,
            tabs: vec![],
            side_width: 260.0,
            minimap: true,
            ctx_height: 220.0,
            clangd: true,
            clangd_path: String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Files,
    Search,
    Outline,
}

struct Node {
    path: PathBuf,
    name: String,
    dir: bool,
}

struct Doc {
    path: PathBuf,
    rel: String,
    buf: Buf,
    ed: Editor,
    gbk: bool,
    crlf: bool,
    lang: Lang,
    syms: Vec<Sym>,
    /// 改过文字后，符号表等一会儿再重新解析（连续打字时不用每个字都解析）
    syms_stale: Option<f64>,
    /// 待跳转：(行, 选中的字节范围)
    goto: Option<(usize, Option<(usize, usize)>)>,
    cursor: (usize, usize),
}

impl Doc {
    fn load(path: &Path, root: &Path) -> Result<Doc, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.len() > 8 * 1024 * 1024 {
            return Err(tf("文件太大（{}），超过 8 MB 不打开", &[&crate::app::human_bytes(meta.len())]));
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let (text, gbk) = cindex::decode(&bytes).ok_or_else(|| t("这是二进制文件，无法以文本方式查看").to_string())?;
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text };
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let buf = Buf::new(text);
        let ed = Editor::new(egui::Id::new(("code-ed", path)), &buf);
        let mut d = Doc {
            path: path.to_path_buf(),
            rel,
            buf,
            ed,
            gbk,
            crlf,
            lang: Lang::from_path(path),
            syms: vec![],
            syms_stale: None,
            goto: None,
            cursor: (0, 0),
        };
        d.reparse();
        Ok(d)
    }

    fn reparse(&mut self) {
        self.syms.clear();
        if self.lang == Lang::C || self.lang == Lang::Asm {
            cindex::parse_c(&self.buf.text, 0, &mut self.syms);
        } else if self.lang.is_code() {
            cindex::parse_generic(&self.buf.text, 0, &mut self.syms);
        }
        self.syms.retain(|s| s.kind != Kind::EnumConst);
        self.syms_stale = None;
    }

    fn dirty(&self) -> bool {
        self.ed.dirty()
    }

    fn line_of_byte(&self, b: usize) -> usize {
        self.buf.line_of(b)
    }

    fn line_text(&self, l: usize) -> &str {
        self.buf.line(l)
    }

    /// 字节位置所在的标识符
    fn word_at(&self, b: usize) -> Option<(usize, usize, &str)> {
        self.buf.word_at(b).map(|(s, e)| (s, e, &self.buf.text[s..e]))
    }

    fn save(&mut self) -> Result<(), String> {
        let text = if self.crlf { self.buf.text.replace('\n', "\r\n") } else { self.buf.text.clone() };
        let bytes = if self.gbk { encoding_rs::GBK.encode(&text).0.into_owned() } else { text.into_bytes() };
        std::fs::write(&self.path, bytes).map_err(|e| e.to_string())?;
        self.ed.mark_clean();
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum QuickMode {
    Files,
    Symbols,
    FileSymbols,
    Line,
}

struct Quick {
    q: String,
    sel: usize,
    focus: bool,
}

struct Find {
    q: String,
    case: bool,
    hits: Vec<usize>,
    cur: usize,
    focus: bool,
}

struct Hover {
    word: String,
    at: usize,
    since: f64,
    rect: Rect,
}

pub enum CodeEvt {
    Toast(String, Color32),
}

pub struct CodeView {
    pub s: CodeSettings,
    root: Option<PathBuf>,
    tree: HashMap<PathBuf, Vec<Node>>,
    expanded: HashSet<PathBuf>,
    tabs: Vec<Doc>,
    active: usize,
    side: Side,
    filter: String,

    index: Option<Arc<Index>>,
    index_rx: Option<Receiver<Index>>,
    index_prog: Option<Arc<Progress>>,
    index_t: Option<Instant>,

    git_marks: HashMap<String, char>,
    git_rx: Option<Receiver<HashMap<String, char>>>,
    git_branch: String,

    quick: Option<Quick>,
    find: Option<Find>,
    search_q: String,
    search_case: bool,
    search_word: bool,
    search_rx: Option<Receiver<Vec<Hit>>>,
    search_cancel: Option<Arc<Progress>>,
    results: Vec<Hit>,
    results_for: String,
    collapsed_results: HashSet<PathBuf>,
    focus_search: bool,

    back: Vec<(PathBuf, usize)>,
    fwd: Vec<(PathBuf, usize)>,
    pick: Option<(String, Vec<(PathBuf, u32, &'static str, String)>, egui::Pos2)>,
    hover: Option<Hover>,
    outline_filter: String,
    pub events: Vec<CodeEvt>,
    sem: Option<Arc<HashMap<String, Kind>>>,
    /// Source Insight 式的上下文窗口和调用关系窗口
    pub context_win: bool,
    pub relations: bool,
    ins: insight::Insight,
    /// clangd（可选）；lsp_for 记录它是为哪个工程、哪个设置启动的，变了就重启
    lsp: Option<lsp::Lsp>,
    lsp_for: Option<(PathBuf, bool, String)>,
    lsp_def: Option<(mpsc::Receiver<serde_json::Value>, String, egui::Pos2, f64)>,
    /// 设置页显示用：按 clangd_path 缓存的查找结果
    clangd_found: Option<(String, Option<PathBuf>)>,
}

impl CodeView {
    pub fn new(s: CodeSettings) -> Self {
        let mut v = CodeView {
            s,
            root: None,
            tree: HashMap::new(),
            expanded: HashSet::new(),
            tabs: vec![],
            active: 0,
            side: Side::Files,
            filter: String::new(),
            index: None,
            index_rx: None,
            index_prog: None,
            index_t: None,
            git_marks: HashMap::new(),
            git_rx: None,
            git_branch: String::new(),
            quick: None,
            find: None,
            search_q: String::new(),
            search_case: false,
            search_word: false,
            search_rx: None,
            search_cancel: None,
            results: vec![],
            results_for: String::new(),
            collapsed_results: HashSet::new(),
            focus_search: false,
            back: vec![],
            fwd: vec![],
            pick: None,
            hover: None,
            outline_filter: String::new(),
            events: vec![],
            sem: None,
            context_win: false,
            relations: false,
            ins: insight::Insight::default(),
            lsp: None,
            lsp_for: None,
            lsp_def: None,
            clangd_found: None,
        };
        v.s.font_size = v.s.font_size.clamp(9.0, 28.0);
        v
    }

    /// 第一次进入代码页时才真正打开上次的文件夹（不进入就不占资源）
    pub fn activate(&mut self) {
        if self.root.is_none() && !self.s.root.is_empty() {
            let p = PathBuf::from(&self.s.root);
            if p.is_dir() {
                let tabs = self.s.tabs.clone();
                self.open_root(p);
                for t in tabs {
                    let p = PathBuf::from(t);
                    if p.is_file() {
                        self.open_file(&p, None);
                    }
                }
                self.active = 0;
            }
        }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn toast(&mut self, m: impl Into<String>, c: Color32) {
        self.events.push(CodeEvt::Toast(m.into(), c));
    }

    pub fn open_root(&mut self, p: PathBuf) {
        let p = std::fs::canonicalize(&p).map(strip_unc).unwrap_or(p);
        self.tree.clear();
        self.expanded.clear();
        self.tabs.clear();
        self.results.clear();
        self.back.clear();
        self.fwd.clear();
        self.load_dir(&p);
        self.expanded.insert(p.clone());
        let ps = p.to_string_lossy().to_string();
        self.s.recent.retain(|r| r != &ps);
        self.s.recent.insert(0, ps.clone());
        self.s.recent.truncate(10);
        self.s.root = ps;
        self.root = Some(p.clone());
        self.reindex();
        self.refresh_git();
    }

    fn reindex(&mut self) {
        let Some(root) = self.root.clone() else { return };
        self.lsp_for = None; // 顺便重新检测 clangd / compile_commands.json
        if let Some(p) = &self.index_prog {
            p.cancel.store(true, Ordering::Relaxed);
        }
        let prog = cindex::shared_progress();
        let (tx, rx) = mpsc::channel();
        let pr = prog.clone();
        std::thread::Builder::new()
            .name("code-index".into())
            .spawn(move || {
                let idx = cindex::build(&root, &pr);
                let _ = tx.send(idx);
            })
            .ok();
        self.index_rx = Some(rx);
        self.index_prog = Some(prog);
        self.index_t = Some(Instant::now());
    }

    fn refresh_git(&mut self) {
        let Some(root) = self.root.clone() else { return };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut m = HashMap::new();
            if let Ok(repo) = git::find_root(&root) {
                if let Ok(st) = git::status(&repo) {
                    m.insert(String::new(), '\0');
                    m.insert("\u{1}branch".into(), '\0');
                    for f in st.files {
                        let full = repo.join(&f.path);
                        let Ok(rel) = full.strip_prefix(&root) else { continue };
                        let rel = rel.to_string_lossy().replace('\\', "/");
                        let c = if f.x == '?' {
                            'U'
                        } else if f.y == 'D' || f.x == 'D' {
                            'D'
                        } else if f.x == 'A' {
                            'A'
                        } else {
                            'M'
                        };
                        // 上级目录也标一下
                        let mut parent = rel.as_str();
                        while let Some(i) = parent.rfind('/') {
                            parent = &parent[..i];
                            m.entry(parent.to_string()).or_insert('•');
                        }
                        m.insert(rel, c);
                    }
                    m.insert(format!("\u{2}{}", st.branch.split("...").next().unwrap_or("")), '\0');
                }
            }
            let _ = tx.send(m);
        });
        self.git_rx = Some(rx);
    }

    fn load_dir(&mut self, dir: &Path) {
        let mut v: Vec<Node> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let dir = e.file_type().ok()?.is_dir();
                        (!(dir && (name == ".git" || name == ".svn"))).then_some(Node { path: e.path(), name, dir })
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        self.tree.insert(dir.to_path_buf(), v);
    }

    fn pick_folder(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_title(t("打开文件夹")).pick_folder() {
            self.open_root(p);
        }
    }

    fn pick_file(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_title(t("打开文件")).pick_file() {
            if self.root.is_none() {
                if let Some(d) = p.parent() {
                    self.open_root(d.to_path_buf());
                }
            }
            self.open_file(&p, None);
        }
    }

    pub fn open_file(&mut self, p: &Path, goto: Option<(usize, Option<(usize, usize)>)>) {
        // 记录跳转前的位置，给"后退"用
        if let Some(d) = self.tabs.get(self.active) {
            let cur = (d.path.clone(), d.cursor.0);
            if self.back.last() != Some(&cur) {
                self.back.push(cur);
                if self.back.len() > 100 {
                    self.back.remove(0);
                }
            }
            self.fwd.clear();
        }
        self.open_file_nohist(p, goto);
    }

    fn open_file_nohist(&mut self, p: &Path, goto: Option<(usize, Option<(usize, usize)>)>) {
        if let Some(i) = self.tabs.iter().position(|d| d.path == p) {
            self.active = i;
        } else {
            let root = self.root.clone().unwrap_or_default();
            match Doc::load(p, &root) {
                Ok(d) => {
                    self.tabs.insert((self.active + 1).min(self.tabs.len()), d);
                    self.active = (self.active + 1).min(self.tabs.len() - 1);
                    if self.tabs.len() == 1 {
                        self.active = 0;
                    }
                }
                Err(e) => {
                    self.toast(e, ERR());
                    return;
                }
            }
        }
        if let Some(g) = goto {
            self.tabs[self.active].goto = Some(g);
        }
        self.sync_tabs();
        self.hover = None;
        self.pick = None;
    }

    fn goto_line(&mut self, line: usize) {
        if let Some(d) = self.tabs.get_mut(self.active) {
            let l = line.min(d.buf.lines().saturating_sub(1));
            let s = d.buf.start(l);
            d.goto = Some((l, Some((s, s))));
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            if self.tabs[i].dirty() {
                let name = self.tabs[i].rel.clone();
                self.toast(tf("{} 有未保存的修改，已丢弃", &[&name]), WARN());
            }
            if let Some(l) = self.lsp.as_mut() {
                l.close(&self.tabs[i].path);
            }
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if i < self.active {
                self.active -= 1;
            }
            self.sync_tabs();
        }
    }

    fn sync_tabs(&mut self) {
        self.s.tabs = self.tabs.iter().map(|d| d.path.to_string_lossy().to_string()).collect();
    }

    fn nav(&mut self, back: bool) {
        let (from, to) = if back { (&mut self.back, &mut self.fwd) } else { (&mut self.fwd, &mut self.back) };
        let Some((p, l)) = from.pop() else { return };
        if let Some(d) = self.tabs.get(self.active) {
            to.push((d.path.clone(), d.cursor.0));
        }
        self.open_file_nohist(&p, None);
        self.goto_line(l);
    }

    // ------------------------------------------------------------ 跳转定义

    fn definitions(&self, word: &str) -> Vec<(PathBuf, u32, &'static str, String)> {
        let mut v = vec![];
        if let Some(idx) = &self.index {
            for s in idx.lookup(word) {
                v.push((idx.path_of(s).to_path_buf(), s.line, s.kind.label(), s.sig.clone()));
            }
        }
        // 当前文件里的（索引还没建好、或文件不在工作区里时也能跳）
        if let Some(d) = self.tabs.get(self.active) {
            for s in d.syms.iter().filter(|s| s.name == word) {
                if !v.iter().any(|x| x.0 == d.path && x.1 == s.line) {
                    v.push((d.path.clone(), s.line, s.kind.label(), s.sig.clone()));
                }
            }
        }
        v
    }

    /// 跳转定义：clangd 就绪时先问它（知道光标所在的字节位置才能问），否则用内置索引
    fn goto_definition(&mut self, word: &str, at: egui::Pos2, byte: Option<usize>, now: f64) {
        if let (Some(lsp), Some(b), Some(d)) = (self.lsp.as_mut(), byte, self.tabs.get(self.active)) {
            if lsp.ready() && d.lang == Lang::C {
                let l = d.buf.line_of(b);
                let col = lsp::utf16_col(d.buf.line(l), b - d.buf.start(l));
                lsp.sync(&d.path, d.buf.ver, &d.buf.text);
                self.lsp_def = Some((lsp.definition(&d.path, l, col), word.to_string(), at, now));
                return;
            }
        }
        self.goto_definition_index(word, at);
    }

    /// 设置页里 clangd 一项的状态说明
    pub fn clangd_status(&mut self) -> (String, bool) {
        if let Some(l) = &self.lsp {
            return if l.ready() {
                (tf("正在使用 {}", &[&l.exe.display()]), true)
            } else if l.dead() {
                (t("clangd 意外退出，已改用内置索引").into(), false)
            } else {
                (t("clangd 启动中…").into(), true)
            };
        }
        if !self.s.clangd {
            return (t("已关闭，使用内置索引").into(), false);
        }
        if self.clangd_found.as_ref().map_or(true, |c| c.0 != self.s.clangd_path) {
            self.clangd_found = Some((self.s.clangd_path.clone(), lsp::find_clangd(&self.s.clangd_path)));
        }
        match self.clangd_found.as_ref().and_then(|c| c.1.as_ref()) {
            None => (t("没找到 clangd，使用内置索引").into(), false),
            Some(p) if self.root.is_none() => (tf("找到 {}，打开工程后启用", &[&p.display()]), false),
            Some(p) => (tf("找到 {}，但工程里没有 compile_commands.json，使用内置索引", &[&p.display()]), false),
        }
    }

    fn poll_lsp(&mut self, ctx: &egui::Context) {
        // 工程或设置变了就（重新）启动 clangd；找不到 clangd 或没有编译数据库就不用它
        let want = self.root.as_ref().map(|r| (r.clone(), self.s.clangd, self.s.clangd_path.clone()));
        if want != self.lsp_for {
            self.lsp = None;
            self.lsp_def = None;
            if let Some((root, true, path)) = &want {
                if let (Some(exe), Some(db)) = (lsp::find_clangd(path), lsp::compile_db(root)) {
                    self.lsp = lsp::Lsp::start(&exe, root, Some(&db)).ok();
                }
            }
            self.lsp_for = want;
        }
        let Some((rx, word, at, since)) = &self.lsp_def else { return };
        let now = ctx.input(|i| i.time);
        let res = match rx.try_recv() {
            Ok(v) => Some(lsp::locations(&v)),
            Err(mpsc::TryRecvError::Empty) if now - since < 4.0 => {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
                return;
            }
            _ => None,
        };
        let (word, at) = (word.clone(), *at);
        self.lsp_def = None;
        let locs = res.unwrap_or_default();
        // clangd 的后台索引还没建好时，别的 .c 里的定义它不知道，只会给头文件里的声明；
        // 这时内置索引如果知道真正的定义，就用内置索引
        let is_header = |p: &Path| matches!(p.extension().and_then(|e| e.to_str()), Some("h" | "hh" | "hpp" | "hxx"));
        let only_decl = !locs.is_empty() && locs.iter().all(|l| is_header(&l.0)) && self.definitions(&word).iter().any(|d| !is_header(&d.0));
        match if only_decl { &[][..] } else { locs.as_slice() } {
            [] => self.goto_definition_index(&word, at),
            [(p, l, c)] => {
                let (p, l, c) = (p.clone(), *l, *c);
                self.open_file(&p, None);
                if let Some(d) = self.tabs.get_mut(self.active) {
                    let l = l.min(d.buf.lines().saturating_sub(1));
                    let b = d.buf.start(l) + lsp::byte_col(d.buf.line(l), c);
                    let e = d.buf.word_at(b).map_or(b, |w| w.1);
                    d.goto = Some((l, Some((b, e))));
                }
            }
            many => {
                let defs = many
                    .iter()
                    .map(|(p, l, _)| {
                        let sig = std::fs::read_to_string(p).ok().and_then(|t| t.lines().nth(*l).map(|s| s.trim().to_string())).unwrap_or_default();
                        (p.clone(), *l as u32, "clangd", sig)
                    })
                    .collect();
                self.pick = Some((word, defs, at));
            }
        }
    }

    fn goto_definition_index(&mut self, word: &str, at: egui::Pos2) {
        let defs = self.definitions(word);
        match defs.len() {
            0 => {
                // 没找到定义：直接做一次全局搜索
                self.toast(tf("没找到 {} 的定义，已在全工作区搜索", &[&word]), WARN());
                self.find_references(word);
            }
            1 => {
                let (p, l, _, _) = defs[0].clone();
                self.open_at_word(&p, l as usize, word);
            }
            _ => self.pick = Some((word.to_string(), defs, at)),
        }
    }

    fn open_at_word(&mut self, p: &Path, line: usize, word: &str) {
        self.open_file(p, None);
        if let Some(d) = self.tabs.get_mut(self.active) {
            let l = line.min(d.buf.lines().saturating_sub(1));
            let lt = d.line_text(l);
            let col = find_word(lt, word).unwrap_or(0);
            let s = d.buf.start(l) + col;
            d.goto = Some((l, Some((s, s + if col > 0 || lt.starts_with(word) { word.len() } else { 0 }))));
        }
    }

    fn find_references(&mut self, word: &str) {
        self.side = Side::Search;
        self.search_q = word.to_string();
        self.search_word = true;
        self.search_case = true;
        self.run_search();
    }

    fn run_search(&mut self) {
        let (Some(root), Some(idx)) = (self.root.clone(), self.index.clone()) else {
            self.toast(t("正在建立索引，稍等片刻再搜索"), WARN());
            return;
        };
        if let Some(c) = &self.search_cancel {
            c.cancel.store(true, Ordering::Relaxed);
        }
        let q = self.search_q.clone();
        let opts = SearchOpts {
            case: self.search_case,
            word: self.search_word,
        };
        let cancel = cindex::shared_progress();
        let c2 = cancel.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = cindex::search(&root, &idx.files, &q, &opts, &c2.cancel, 5000);
            let _ = tx.send(r);
        });
        self.search_rx = Some(rx);
        self.search_cancel = Some(cancel);
        self.results_for = self.search_q.clone();
        self.collapsed_results.clear();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_lsp(ctx);
        if let Some(rx) = &self.index_rx {
            match rx.try_recv() {
                Ok(idx) => {
                    let n = idx.syms.len();
                    let ms = self.index_t.map_or(0, |t| t.elapsed().as_millis());
                    // 给编辑器上色用：名字 → 符号种类（同名的取第一个定义）
                    let mut sem = HashMap::with_capacity(idx.by_name.len());
                    for (name, ids) in &idx.by_name {
                        if let Some(&i) = ids.first() {
                            sem.insert(name.clone(), idx.syms[i as usize].kind);
                        }
                    }
                    self.sem = Some(Arc::new(sem));
                    self.insight_reset();
                    self.index = Some(Arc::new(idx));
                    self.index_rx = None;
                    self.index_prog = None;
                    log_index(n, ms);
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(150)),
                Err(_) => self.index_rx = None,
            }
        }
        if let Some(rx) = &self.git_rx {
            if let Ok(mut m) = rx.try_recv() {
                self.git_branch = m.keys().find_map(|k| k.strip_prefix('\u{2}').map(String::from)).unwrap_or_default();
                m.retain(|k, _| !k.starts_with(['\u{1}', '\u{2}']) && !k.is_empty());
                self.git_marks = m;
                self.git_rx = None;
            }
        }
        if let Some(rx) = &self.search_rx {
            match rx.try_recv() {
                Ok(r) => {
                    self.results = r;
                    self.search_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
                Err(_) => self.search_rx = None,
            }
        }
    }

    // ------------------------------------------------------------ 主界面

    pub fn ui(&mut self, ctx: &egui::Context, acts: &[Act]) {
        self.poll(ctx);
        self.shortcuts(ctx, acts);

        egui::SidePanel::left("code-side")
            .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 0 }))
            .default_width(self.s.side_width)
            .width_range(200.0..=520.0)
            .resizable(true)
            .show(ctx, |ui| {
                self.s.side_width = ui.available_width();
                self.side_panel(ui);
            });
        self.poll_insight();
        if self.relations && !self.tabs.is_empty() {
            egui::SidePanel::right("code-rel")
                .frame(egui::Frame::new().fill(BG1()))
                .default_width(300.0)
                .width_range(220.0..=560.0)
                .resizable(true)
                .show(ctx, |ui| self.relations_ui(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG0())).show(ctx, |ui| {
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.tab_bar(ui);
                self.breadcrumbs(ui);
                if self.context_win {
                    // egui 会把面板高度夹到当时可用空间并记住；这里自己记住想要的高度，
                    // 窗口变大后能恢复，拖动时才放开下限
                    let limit = (ui.available_height() * 0.6).max(90.0);
                    let want = self.s.ctx_height.clamp(90.0, limit);
                    let dragging = ui.input(|i| i.pointer.any_down());
                    let r = egui::TopBottomPanel::bottom("code-ctx")
                        .frame(egui::Frame::new().fill(BG1()).inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 }))
                        .default_height(want)
                        .height_range(if dragging { 90.0 } else { want }..=limit)
                        .resizable(true)
                        .show_inside(ui, |ui| self.context_ui(ui));
                    if dragging && (r.response.rect.height() - want).abs() > 0.5 {
                        self.s.ctx_height = r.response.rect.height();
                    }
                }
                self.editor(ui);
            }
        });
        self.quick_open(ctx);
        self.pick_popup(ctx);
    }

    fn shortcuts(&mut self, ctx: &egui::Context, acts: &[Act]) {
        let has = |a: Act| acts.contains(&a);
        if has(Act::SearchAll) {
            self.side = Side::Search;
            self.focus_search = true;
        }
        if has(Act::GotoSymbol) {
            self.quick = Some(Quick { q: "@".into(), sel: 0, focus: true });
        }
        if has(Act::PageCode) {
            self.side = Side::Files;
        }
        if has(Act::QuickOpen) {
            self.quick = Some(Quick {
                q: String::new(),
                sel: 0,
                focus: true,
            });
        }
        if has(Act::GotoSymbolWs) {
            self.quick = Some(Quick { q: "#".into(), sel: 0, focus: true });
        }
        if has(Act::GotoLine) {
            self.quick = Some(Quick { q: ":".into(), sel: 0, focus: true });
        }
        if has(Act::OpenFile) {
            self.pick_file();
        }
        if !self.tabs.is_empty() {
            if has(Act::FindInFile) {
                let sel = self.selected_text(ctx).filter(|s| !s.contains('\n') && s.len() < 200);
                let q = sel.unwrap_or_else(|| self.find.as_ref().map(|f| f.q.clone()).unwrap_or_default());
                self.find = Some(Find {
                    q,
                    case: false,
                    hits: vec![],
                    cur: 0,
                    focus: true,
                });
                self.update_find();
            }
            if has(Act::CloseTab) {
                self.close_tab(self.active);
            }
            if has(Act::NextTab) {
                self.active = (self.active + 1) % self.tabs.len();
            }
            if has(Act::SaveFile) {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    if d.dirty() {
                        match d.save() {
                            Ok(()) => self.toast(t("已保存"), OK()),
                            Err(e) => self.toast(e, ERR()),
                        }
                    }
                }
            }
            if has(Act::ZoomIn) {
                self.s.font_size = (self.s.font_size + 1.0).min(28.0);
            }
            if has(Act::ZoomOut) {
                self.s.font_size = (self.s.font_size - 1.0).max(9.0);
            }
            if has(Act::GotoDef) {
                if let Some(w) = self.cursor_word() {
                    let at = ctx.input(|i| i.pointer.hover_pos()).unwrap_or(pos2(400.0, 200.0));
                    let b = self.tabs.get(self.active).map(|d| d.ed.head());
                    self.goto_definition(&w, at, b, ctx.input(|i| i.time));
                }
            }
            if has(Act::FindRefs) {
                if let Some(w) = self.cursor_word() {
                    self.find_references(&w);
                }
            }
        }
        if has(Act::ToggleContext) {
            self.context_win = !self.context_win;
        }
        if has(Act::ToggleRelations) {
            self.relations = !self.relations;
        }
        if let Some(d) = self.tabs.get_mut(self.active) {
            let now = ctx.input(|i| i.time);
            let before = d.buf.ver;
            let (ed, buf) = (&mut d.ed, &mut d.buf);
            for a in acts {
                match a {
                    Act::ToggleComment => ed.toggle_comment(buf, d.lang, now),
                    Act::MoveLineUp => ed.move_lines(buf, true, false, now),
                    Act::MoveLineDown => ed.move_lines(buf, false, false, now),
                    Act::CopyLineUp => ed.move_lines(buf, true, true, now),
                    Act::CopyLineDown => ed.move_lines(buf, false, true, now),
                    Act::DeleteLine => ed.delete_lines(buf, now),
                    Act::InsertLineBelow => ed.insert_line(buf, false, now),
                    Act::InsertLineAbove => ed.insert_line(buf, true, now),
                    Act::SelectLine => ed.select_line(buf),
                    Act::AddNextOcc => ed.add_next_occurrence(buf),
                    Act::SelectAllOcc => ed.select_all_occurrences(buf),
                    Act::CursorAbove => ed.add_cursor_vert(buf, true),
                    Act::CursorBelow => ed.add_cursor_vert(buf, false),
                    Act::GotoBracket => ed.goto_bracket(buf),
                    Act::Fold => ed.fold_cursor(buf, true),
                    Act::Unfold => ed.fold_cursor(buf, false),
                    Act::FoldAll => ed.fold_all(true),
                    Act::UnfoldAll => ed.fold_all(false),
                    Act::ToggleBookmark => ed.toggle_bookmark(buf),
                    Act::NextBookmark => {
                        ed.next_bookmark(buf, false);
                    }
                    Act::PrevBookmark => {
                        ed.next_bookmark(buf, true);
                    }
                    _ => continue,
                }
                ed.focus_req = true;
            }
            if d.buf.ver != before {
                d.syms_stale = Some(now);
            }
        }
        if has(Act::NavBack) {
            self.nav(true);
        }
        if has(Act::NavFwd) {
            self.nav(false);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.pick = None;
            self.hover = None;
            if self.quick.is_some() {
                self.quick = None;
            } else if self.find.is_some() {
                self.find = None;
            }
        }
    }

    fn selected_text(&self, _ctx: &egui::Context) -> Option<String> {
        let d = self.tabs.get(self.active)?;
        let t = d.ed.selected_text(&d.buf);
        (!t.is_empty()).then_some(t)
    }

    fn cursor_word(&self) -> Option<String> {
        let d = self.tabs.get(self.active)?;
        d.word_at(d.ed.head()).map(|(_, _, w)| w.to_string())
    }

    fn update_find(&mut self) {
        let (Some(f), Some(d)) = (self.find.as_mut(), self.tabs.get(self.active)) else { return };
        f.hits.clear();
        if f.q.is_empty() {
            return;
        }
        let (hay, needle) = if f.case {
            (d.buf.text.clone(), f.q.clone())
        } else {
            (d.buf.text.to_lowercase(), f.q.to_lowercase())
        };
        if hay.len() != d.buf.text.len() {
            return;
        }
        let mut from = 0;
        while let Some(k) = hay[from..].find(&needle) {
            f.hits.push(from + k);
            from += k + needle.len().max(1);
            if f.hits.len() >= 10_000 {
                break;
            }
        }
        // 从光标附近开始
        let cur_b = d.ed.head();
        f.cur = f.hits.iter().position(|&h| h >= cur_b).unwrap_or(0);
    }

    fn find_step(&mut self, back: bool) {
        let Some(f) = self.find.as_mut() else { return };
        if f.hits.is_empty() {
            return;
        }
        let n = f.hits.len();
        f.cur = if back { (f.cur + n - 1) % n } else { (f.cur + 1) % n };
        let (h, len) = (f.hits[f.cur], f.q.len());
        if let Some(d) = self.tabs.get_mut(self.active) {
            d.goto = Some((d.line_of_byte(h), Some((h, h + len))));
        }
    }

    // ------------------------------------------------------------ 侧边栏

    fn side_panel(&mut self, ui: &mut Ui) {
        // 顶部：三个视图切换 + 文件夹操作
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            for (s, icon, tip) in [
                (Side::Files, Icon::Files, t("资源管理器  Ctrl+Shift+E")),
                (Side::Search, Icon::Search, t("全局搜索  Ctrl+Shift+F")),
                (Side::Outline, Icon::Outline, t("大纲（当前文件的函数、宏…）")),
            ] {
                if icon_button(ui, icon, tip, self.side == s).clicked() {
                    self.side = s;
                    if s == Side::Search {
                        self.focus_search = true;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if icon_button(ui, Icon::Folder, t("打开文件夹"), false).clicked() {
                    self.pick_folder();
                }
                if self.root.is_some() && self.side == Side::Files {
                    if icon_button(ui, Icon::Collapse, t("全部折叠"), false).clicked() {
                        let r = self.root.clone();
                        self.expanded.retain(|p| Some(p) == r.as_ref());
                    }
                    if icon_button(ui, Icon::Refresh, t("刷新（重新读取文件和索引）"), false).clicked() {
                        let dirs: Vec<PathBuf> = self.tree.keys().cloned().collect();
                        for d in dirs {
                            self.load_dir(&d);
                        }
                        self.reindex();
                        self.refresh_git();
                    }
                }
            });
        });
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top() + 3.0, st(1.0, LINE()));
        ui.add_space(8.0);
        match self.side {
            Side::Files => self.files_panel(ui),
            Side::Search => self.search_panel(ui),
            Side::Outline => self.outline_panel(ui),
        }
    }

    fn files_panel(&mut self, ui: &mut Ui) {
        let Some(root) = self.root.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(t("还没有打开文件夹")).color(TEXT2()));
                ui.add_space(8.0);
                if ui
                    .add(egui::Button::new(RichText::new(t("打开文件夹")).color(ON_ACCENT()).strong()).fill(ACCENT()).min_size(vec2(160.0, 32.0)))
                    .clicked()
                {
                    self.pick_folder();
                }
            });
            return;
        };
        // 工作区名 + 分支 + 索引状态
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.to_string_lossy().to_string());
            ui.label(RichText::new(name.to_uppercase()).font(FontId::new(11.5, bold())).color(TEXT2()))
                .on_hover_text(root.to_string_lossy());
            if !self.git_branch.is_empty() {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                draw_icon(ui.painter(), r, Icon::Branch, VIOLET());
                ui.label(RichText::new(&self.git_branch).font(FontId::new(11.0, FontFamily::Monospace)).color(VIOLET()));
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.add_sized(
                vec2(ui.available_width() - 10.0, 24.0),
                egui::TextEdit::singleline(&mut self.filter).hint_text(t("筛选文件名…")).margin(vec2(6.0, 3.0)),
            );
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if !self.filter.trim().is_empty() {
                // 筛选：用索引里的文件列表做模糊匹配
                let q = self.filter.trim().to_string();
                let mut hits: Vec<(i32, &PathBuf)> = self
                    .index
                    .as_ref()
                    .map(|i| i.files.iter().filter_map(|f| cindex::fuzzy(&f.to_string_lossy(), &q).map(|s| (s, f))).collect())
                    .unwrap_or_default();
                hits.sort_by(|a, b| b.0.cmp(&a.0));
                let mut open = None;
                for (_, f) in hits.into_iter().take(300) {
                    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let parent = f.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    if tree_row(ui, 0, &name, Some(&parent), false, None, false, self.mark(&f.to_string_lossy())).clicked() {
                        open = Some(root.join(f));
                    }
                }
                if let Some(p) = open {
                    self.open_file(&p, None);
                }
                return;
            }
            self.tree_rows(ui, &root, 0);
        });
    }

    fn mark(&self, rel: &str) -> Option<char> {
        self.git_marks.get(&rel.replace('\\', "/")).copied()
    }

    fn tree_rows(&mut self, ui: &mut Ui, dir: &Path, depth: usize) {
        if !self.tree.contains_key(dir) {
            self.load_dir(dir);
        }
        let root = self.root.clone().unwrap_or_default();
        let active = self.tabs.get(self.active).map(|d| d.path.clone());
        let nodes: Vec<(PathBuf, String, bool)> = self.tree[dir].iter().map(|n| (n.path.clone(), n.name.clone(), n.dir)).collect();
        for (path, name, is_dir) in nodes {
            let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().to_string();
            let open = self.expanded.contains(&path);
            let r = tree_row(ui, depth, &name, None, is_dir, Some(open), active.as_ref() == Some(&path), self.mark(&rel));
            if r.clicked() {
                if is_dir {
                    if open {
                        self.expanded.remove(&path);
                    } else {
                        self.expanded.insert(path.clone());
                    }
                } else {
                    self.open_file(&path, None);
                }
            }
            r.context_menu(|ui| {
                if ui.button(t("复制路径")).clicked() {
                    ui.ctx().copy_text(path.to_string_lossy().to_string());
                    ui.close_menu();
                }
                if ui.button(t("复制相对路径")).clicked() {
                    ui.ctx().copy_text(rel.replace('\\', "/"));
                    ui.close_menu();
                }
                if ui.button(t("在文件管理器中显示")).clicked() {
                    reveal(&path);
                    ui.close_menu();
                }
            });
            if is_dir && open {
                self.tree_rows(ui, &path, depth + 1);
            }
        }
    }

    fn search_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let w = ui.available_width() - 70.0;
            let r = ui.add_sized(vec2(w, 26.0), egui::TextEdit::singleline(&mut self.search_q).hint_text(t("在工作区中搜索")).margin(vec2(6.0, 4.0)));
            if self.focus_search {
                r.request_focus();
                self.focus_search = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.run_search();
            }
            ui.spacing_mut().item_spacing.x = 2.0;
            if small_toggle(ui, &mut self.search_case, "Aa", t("区分大小写")) | small_toggle(ui, &mut self.search_word, "ab", t("全字匹配")) {
                if !self.search_q.is_empty() {
                    self.run_search();
                }
            }
        });
        ui.add_space(6.0);
        if self.search_rx.is_some() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.spinner();
                ui.label(RichText::new(t("搜索中…")).small().color(TEXT3()));
            });
        } else if !self.results_for.is_empty() {
            let files: HashSet<&PathBuf> = self.results.iter().map(|h| &h.path).collect();
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(tf("{} 个结果，{} 个文件", &[&self.results.len(), &files.len()])).small().color(TEXT3()));
            });
        }
        ui.add_space(4.0);
        let root = self.root.clone().unwrap_or_default();
        let mut open: Option<(PathBuf, usize)> = None;
        let q = self.results_for.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut last: Option<&PathBuf> = None;
            let mut collapsed = false;
            for h in &self.results {
                if last != Some(&h.path) {
                    last = Some(&h.path);
                    collapsed = self.collapsed_results.contains(&h.path);
                    let n = self.results.iter().filter(|x| x.path == h.path).count();
                    let name = h.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dir = h.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                    let p = ui.painter();
                    if resp.hovered() {
                        p.rect_filled(r, 0.0, tint(6));
                    }
                    chevron(p, pos2(r.left() + 14.0, r.center().y), !collapsed, TEXT3());
                    file_badge(p, pos2(r.left() + 30.0, r.center().y), &name);
                    let g = p.layout_no_wrap(name.clone(), FontId::proportional(12.5), TEXT());
                    let w = g.size().x;
                    p.galley(pos2(r.left() + 42.0, r.center().y - g.size().y / 2.0), g, TEXT());
                    p.text(pos2(r.left() + 48.0 + w, r.center().y), Align2::LEFT_CENTER, dir, FontId::proportional(11.0), TEXT3());
                    p.text(
                        pos2(r.right() - 10.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        n.to_string(),
                        FontId::new(10.5, FontFamily::Monospace),
                        TEXT3(),
                    );
                    if resp.clicked() {
                        if collapsed {
                            self.collapsed_results.remove(&h.path);
                        } else {
                            self.collapsed_results.insert(h.path.clone());
                        }
                    }
                }
                if collapsed {
                    continue;
                }
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                let p = ui.painter_at(r);
                if resp.hovered() {
                    p.rect_filled(r, 0.0, tint(8));
                }
                let mut job = egui::text::LayoutJob::default();
                let font = FontId::new(12.0, FontFamily::Monospace);
                let txt = &h.text;
                let lo = if self.search_case { txt.clone() } else { txt.to_lowercase() };
                let qq = if self.search_case { q.clone() } else { q.to_lowercase() };
                let at = if lo.len() == txt.len() { lo.find(&qq) } else { None };
                match at {
                    Some(a) => {
                        job.append(&txt[..a], 0.0, egui::TextFormat::simple(font.clone(), TEXT2()));
                        job.append(
                            &txt[a..a + qq.len()],
                            0.0,
                            egui::TextFormat {
                                font_id: font.clone(),
                                color: TEXT(),
                                background: WARN().gamma_multiply(0.35),
                                ..Default::default()
                            },
                        );
                        job.append(&txt[a + qq.len()..], 0.0, egui::TextFormat::simple(font.clone(), TEXT2()));
                    }
                    None => job.append(txt, 0.0, egui::TextFormat::simple(font.clone(), TEXT2())),
                }
                let g = ui.fonts(|f| f.layout_job(job));
                p.text(
                    pos2(r.left() + 30.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    (h.line + 1).to_string(),
                    FontId::new(10.5, FontFamily::Monospace),
                    TEXT3(),
                );
                p.galley(pos2(r.left() + 38.0, r.center().y - g.size().y / 2.0), g, TEXT());
                if resp.clicked() {
                    open = Some((root.join(&h.path), h.line as usize));
                }
            }
        });
        if let Some((p, l)) = open {
            let w = q.clone();
            self.open_at_word(&p, l, &w);
        }
    }

    fn outline_panel(&mut self, ui: &mut Ui) {
        if self.tabs.get(self.active).is_none() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(t("打开一个文件后显示它的函数、宏和类型")).small().color(TEXT3()));
            });
            return;
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.add_sized(
                vec2(ui.available_width() - 10.0, 24.0),
                egui::TextEdit::singleline(&mut self.outline_filter).hint_text(t("筛选符号…")).margin(vec2(6.0, 3.0)),
            );
        });
        ui.add_space(4.0);
        // 书签（所有打开的文件），没有书签时不占地方
        let marks: Vec<(usize, usize, String)> = self
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(ti, d)| d.ed.bookmarks.iter().map(move |&l| (ti, l, d.buf.line(l.min(d.buf.lines() - 1)).trim().to_string())))
            .collect();
        let mut jump = None;
        if !marks.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(t("书签")).size(11.0).color(TEXT3()).strong());
                ui.label(RichText::new(marks.len().to_string()).size(10.5).color(TEXT3()));
            });
            ui.spacing_mut().item_spacing.y = 0.0;
            for (ti, l, text) in &marks {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                let p = ui.painter();
                if resp.hovered() {
                    p.rect_filled(r, 0.0, tint(6));
                }
                draw_icon(p, Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(12.0, 12.0)), Icon::Bookmark, VIOLET());
                let name = format!("{}:{}", self.tabs[*ti].path.file_name().unwrap_or_default().to_string_lossy(), l + 1);
                let g = p.layout_no_wrap(name, FontId::proportional(12.0), TEXT2());
                let nw = g.size().x;
                p.galley(pos2(r.left() + 34.0, r.center().y - g.size().y / 2.0), g, TEXT2());
                let room = r.right() - 10.0 - (r.left() + 44.0 + nw);
                if room > 20.0 {
                    let g = p.layout(text.clone(), FontId::new(11.5, FontFamily::Monospace), TEXT3(), f32::INFINITY);
                    let clip = Rect::from_min_max(pos2(r.left() + 44.0 + nw, r.top()), pos2(r.right() - 10.0, r.bottom()));
                    p.with_clip_rect(clip).galley(pos2(clip.left(), r.center().y - g.size().y / 2.0), g, TEXT3());
                }
                if resp.clicked() {
                    jump = Some((*ti, *l));
                }
            }
            ui.add_space(6.0);
            let r = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover()).0;
            ui.painter().hline(r.x_range().shrink(10.0), r.center().y, st(1.0, LINE()));
            ui.add_space(6.0);
        }
        if let Some((ti, l)) = jump {
            self.active = ti;
            self.tabs[ti].goto = Some((l, None));
            return;
        }
        let d = &self.tabs[self.active];
        let cur_line = d.cursor.0;
        let f = self.outline_filter.to_lowercase();
        let syms: Vec<(String, Kind, u32)> = d
            .syms
            .iter()
            .filter(|s| f.is_empty() || s.name.to_lowercase().contains(&f))
            .map(|s| (s.name.clone(), s.kind, s.line))
            .collect();
        let current = syms.iter().rposition(|s| s.2 as usize <= cur_line);
        let mut go = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (name, kind, line)) in syms.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 23.0), Sense::click());
                let p = ui.painter();
                if Some(i) == current {
                    p.rect_filled(r, 0.0, ACCENT().gamma_multiply(0.08));
                    p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, ACCENT());
                } else if resp.hovered() {
                    p.rect_filled(r, 0.0, tint(6));
                }
                kind_badge(p, pos2(r.left() + 20.0, r.center().y), *kind);
                p.text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, name, FontId::new(12.5, FontFamily::Monospace), TEXT());
                p.text(
                    pos2(r.right() - 10.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    (line + 1).to_string(),
                    FontId::new(10.5, FontFamily::Monospace),
                    TEXT3(),
                );
                if resp.clicked() {
                    go = Some(name.clone());
                }
            }
        });
        if let Some(name) = go {
            if let Some(s) = self.tabs[self.active].syms.iter().find(|s| s.name == name).map(|s| s.line) {
                let p = self.tabs[self.active].path.clone();
                self.open_at_word(&p, s as usize, &name);
            }
        }
    }

    // ------------------------------------------------------------ 欢迎页

    fn welcome(&mut self, ui: &mut Ui) {
        let r = ui.max_rect();
        let p = ui.painter();
        hgrad_line(p, r.top() + 0.5, r.x_range(), ACCENT().gamma_multiply(0.4));
        let c = r.center() - vec2(0.0, 110.0);
        let mut code_icon = Rect::from_center_size(c - vec2(0.0, 40.0), vec2(56.0, 56.0));
        p.rect(code_icon, CornerRadius::same(14), BG2(), st(1.0, ACCENT().gamma_multiply(0.4)), egui::StrokeKind::Inside);
        code_icon = code_icon.shrink(12.0);
        draw_icon(p, code_icon, Icon::Code, ACCENT());
        p.text(c + vec2(0.0, 14.0), Align2::CENTER_CENTER, t("代码查看"), FontId::new(22.0, bold()), TEXT());
        p.text(
            c + vec2(0.0, 42.0),
            Align2::CENTER_CENTER,
            t("打开固件工程，边看日志边查代码：跳转定义、宏展开、全局搜索"),
            FontId::proportional(13.0),
            TEXT3(),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(r.center() + vec2(0.0, 110.0), vec2(520.0, 300.0))));
        let ui = &mut child;
        ui.horizontal(|ui| {
            ui.add_space(70.0);
            if ui
                .add(
                    egui::Button::new(RichText::new(t("打开文件夹")).color(ON_ACCENT()).strong())
                        .fill(ACCENT())
                        .corner_radius(R_WIDGET)
                        .min_size(vec2(180.0, 36.0)),
                )
                .clicked()
            {
                self.pick_folder();
            }
            ui.add_space(8.0);
            if ui
                .add(
                    egui::Button::new(RichText::new(t("打开文件")).color(TEXT()))
                        .fill(BG2())
                        .stroke(st(1.0, LINE_HI()))
                        .corner_radius(R_WIDGET)
                        .min_size(vec2(180.0, 36.0)),
                )
                .clicked()
            {
                self.pick_file();
            }
        });
        ui.add_space(18.0);
        if self.root.is_none() && !self.s.recent.is_empty() {
            ui.label(RichText::new(t("最近打开")).font(FontId::new(11.5, bold())).color(TEXT3()));
            ui.add_space(4.0);
            let mut go = None;
            for r in self.s.recent.iter().take(6) {
                let name = Path::new(r).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.clone());
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                let p = ui.painter();
                if resp.hovered() {
                    p.rect_filled(rect, R_WIDGET, BG2());
                }
                draw_icon(p, Rect::from_center_size(pos2(rect.left() + 12.0, rect.center().y), vec2(14.0, 14.0)), Icon::Folder, ACCENT());
                p.text(pos2(rect.left() + 28.0, rect.center().y), Align2::LEFT_CENTER, &name, FontId::proportional(13.0), ACCENT());
                p.text(
                    pos2(rect.left() + 40.0 + name.chars().count() as f32 * 8.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    r,
                    FontId::proportional(11.5),
                    TEXT3(),
                );
                if resp.clicked() {
                    go = Some(r.clone());
                }
            }
            if let Some(r) = go {
                self.open_root(PathBuf::from(r));
            }
        } else {
            egui::Grid::new("code-keys").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Ctrl+P", t("按文件名快速打开")),
                    ("Ctrl+T", t("搜索整个工作区的函数 / 宏")),
                    ("Ctrl+Shift+O", t("跳到当前文件里的符号")),
                    (t("F12 / Ctrl+点击"), t("跳转到定义")),
                    ("Shift+F12", t("查找所有引用")),
                    ("Ctrl+Shift+F", t("全局搜索")),
                    ("Alt+← / Alt+→", t("后退 / 前进")),
                ] {
                    ui.label(RichText::new(k).font(FontId::new(12.0, FontFamily::Monospace)).color(ACCENT()));
                    ui.label(RichText::new(v).color(TEXT2()));
                    ui.end_row();
                }
            });
        }
    }

    // ------------------------------------------------------------ 标签栏、面包屑

    fn tab_bar(&mut self, ui: &mut Ui) {
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
        ui.painter().rect_filled(bar, 0.0, BG1());
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, st(1.0, LINE()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar));
        let mut close = None;
        let mut activate = None;
        egui::ScrollArea::horizontal()
            .id_salt("code-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, d) in self.tabs.iter().enumerate() {
                        let name = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        let g = ui.painter().layout_no_wrap(name.clone(), FontId::proportional(12.5), TEXT());
                        let w = g.size().x + 64.0;
                        let (r, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
                        let on = i == self.active;
                        let p = ui.painter();
                        if on {
                            p.rect_filled(r, 0.0, BG0());
                            p.hline(r.x_range(), r.top() + 1.0, st(2.0, ACCENT()));
                        } else if resp.hovered() {
                            p.rect_filled(r, 0.0, BG2());
                        }
                        p.vline(r.right() - 0.5, r.y_range(), st(1.0, LINE()));
                        file_badge(p, pos2(r.left() + 16.0, r.center().y), &name);
                        p.galley(pos2(r.left() + 28.0, r.center().y - g.size().y / 2.0), g, if on { TEXT() } else { TEXT2() });
                        let xr = Rect::from_center_size(pos2(r.right() - 16.0, r.center().y), vec2(16.0, 16.0));
                        let xh = ui.rect_contains_pointer(xr);
                        if d.dirty() && !xh {
                            p.circle_filled(xr.center(), 4.0, TEXT2());
                        } else if on || resp.hovered() {
                            if xh {
                                p.rect_filled(xr, 3.0, BG3());
                            }
                            draw_icon(p, xr.shrink(2.0), Icon::Close, if xh { TEXT() } else { TEXT3() });
                        }
                        if resp.clicked() {
                            if xh {
                                close = Some(i);
                            } else {
                                activate = Some(i);
                            }
                        }
                        if resp.middle_clicked() {
                            close = Some(i);
                        }
                        resp.on_hover_text(&d.rel);
                    }
                });
            });
        if let Some(i) = activate {
            self.active = i;
            self.hover = None;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }

    fn breadcrumbs(&mut self, ui: &mut Ui) {
        let Some(d) = self.tabs.get(self.active) else { return };
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 0.0, BG0());
        let mut x = r.left() + 14.0;
        let font = FontId::proportional(12.0);
        let parts: Vec<&str> = d.rel.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            if last {
                file_badge(p, pos2(x + 5.0, r.center().y), part);
                x += 14.0;
            }
            let g = p.layout_no_wrap(part.to_string(), font.clone(), if last { TEXT() } else { TEXT3() });
            let w = g.size().x;
            p.galley(pos2(x, r.center().y - g.size().y / 2.0), g, TEXT());
            x += w + 6.0;
            if !last {
                chevron(p, pos2(x + 3.0, r.center().y), false, TEXT3());
                x += 14.0;
            }
        }
        // 当前所在的函数
        if let Some(s) = d
            .syms
            .iter()
            .filter(|s| matches!(s.kind, Kind::Func | Kind::Struct | Kind::Enum | Kind::Class))
            .rfind(|s| s.line as usize <= d.cursor.0)
        {
            chevron(p, pos2(x + 3.0, r.center().y), false, TEXT3());
            x += 14.0;
            kind_badge(p, pos2(x + 6.0, r.center().y), s.kind);
            p.text(pos2(x + 16.0, r.center().y), Align2::LEFT_CENTER, &s.name, FontId::new(12.0, FontFamily::Monospace), TEXT2());
        }
        // 右侧：后退前进、编辑开关
        let mut bx = r.right() - 10.0;
        let mut btn = |icon: Icon, tip: &str, active: bool, enabled: bool| -> bool {
            let br = Rect::from_center_size(pos2(bx - 11.0, r.center().y), vec2(22.0, 20.0));
            bx -= 24.0;
            let resp = ui.interact(br, ui.id().with(("bc", tip)), Sense::click());
            let p = ui.painter();
            if (resp.hovered() && enabled) || active {
                p.rect_filled(br, 4.0, if active { ACCENT().gamma_multiply(0.15) } else { BG3() });
            }
            draw_icon(
                p,
                br.shrink2(vec2(4.0, 3.0)),
                icon,
                if !enabled {
                    TEXT3().gamma_multiply(0.5)
                } else if active {
                    ACCENT()
                } else {
                    TEXT2()
                },
            );
            resp.on_hover_text(tip).clicked() && enabled
        };
        let (nb, nf) = (!self.back.is_empty(), !self.fwd.is_empty());
        let rel_on = self.relations;
        let ctx_on = self.context_win;
        let rel = btn(Icon::Relations, t("调用关系"), rel_on, true);
        let cx = btn(Icon::Context, t("上下文窗口"), ctx_on, true);
        let f = btn(Icon::Forward, t("前进  Alt+→"), false, nf);
        let b = btn(Icon::Back, t("后退  Alt+←"), false, nb);
        if rel {
            self.relations = !self.relations;
        }
        if cx {
            self.context_win = !self.context_win;
        }
        if b {
            self.nav(true);
        }
        if f {
            self.nav(false);
        }
    }

    // ------------------------------------------------------------ 编辑器

    fn editor(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let status_h = 24.0;
        let area = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - status_h));
        let status_rect = Rect::from_min_max(pos2(full.left(), area.bottom()), full.max);
        ui.allocate_rect(full, Sense::hover());
        let idx = self.index.clone();
        let sem = self.sem.clone();
        let active = self.active;
        let now = ui.input(|i| i.time);
        let d = &mut self.tabs[active];

        // 待跳转：设置光标/选区，并滚到那一行
        if let Some((line, sel)) = d.goto.take() {
            let (a, b) = sel.unwrap_or_else(|| {
                let s = d.buf.start(line);
                (s, s)
            });
            d.ed.select(a, b);
            d.ed.focus_req = true;
        }
        let (hits, find_len, find_cur) = match &self.find {
            Some(f) => (f.hits.clone(), f.q.len(), Some(f.cur)),
            None => (vec![], 0, None),
        };
        let view = View {
            lang: d.lang,
            font_size: self.s.font_size,
            minimap: self.s.minimap,
            find: &hits,
            find_len,
            find_cur,
            sem: sem.as_deref().filter(|_| d.lang == Lang::C || d.lang == Lang::Asm),
            git: &[],
            big: d.buf.text.len() > 1_500_000,
        };
        let out = d.ed.show(ui, area, &mut d.buf, &view);
        if out.changed {
            d.syms_stale = Some(now);
            self.find = self.find.take().map(|mut f| {
                f.focus = false;
                f
            });
        }
        if d.syms_stale.is_some_and(|t| now - t > 0.4) {
            d.reparse();
        } else if d.syms_stale.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(450));
        }
        let h = d.ed.head();
        let l = d.buf.line_of(h);
        d.cursor = (l, d.buf.line(l)[..h - d.buf.start(l)].chars().count());
        let lang = d.lang;
        if out.changed && self.find.is_some() {
            self.update_find();
        }

        // ---- 悬浮 / Ctrl+点击
        let mut want_def: Option<(String, egui::Pos2, Option<usize>)> = None;
        let d = &self.tabs[active];
        if let Some((b, at)) = out.ctrl_click {
            if let Some((_, _, w)) = d.word_at(b) {
                want_def = Some((w.to_string(), at, Some(b)));
            }
        }
        if let Some((s, e, r)) = out.hover {
            let w = d.buf.text[s..e].to_string();
            match &self.hover {
                Some(h) if h.at == s && h.word == w => {}
                _ => self.hover = Some(Hover { word: w, at: s, since: now, rect: r }),
            }
        } else {
            self.hover = None;
        }
        let mut hover_def: Option<(String, egui::Pos2, Option<usize>)> = None;
        if let Some(h) = &self.hover {
            if lang.is_code() && now - h.since > 0.35 && !ui.input(|i| i.pointer.any_down() || i.modifiers.command) {
                if let Some(go) = hover_card(ui.ctx(), &h.word, h.rect, idx.as_deref(), &self.tabs[active]) {
                    let b = (go == h.word).then_some(h.at);
                    hover_def = Some((go, h.rect.left_bottom(), b));
                }
            } else if lang.is_code() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // ---- 查找浮层
        let mini_w = if self.s.minimap { 72.0 } else { 0.0 };
        self.find_bar(ui, Rect::from_min_max(area.min, pos2(area.right() - mini_w - 10.0, area.bottom())));

        // ---- 状态行
        let d = &self.tabs[active];
        let n_lines = d.buf.lines();
        let sp = ui.painter_at(status_rect);
        sp.rect_filled(status_rect, 0.0, BG1());
        sp.hline(status_rect.x_range(), status_rect.top() + 0.5, st(1.0, LINE()));
        let mut items: Vec<(String, Color32)> = vec![
            (tf("行 {}，列 {}", &[&(d.cursor.0 + 1), &(d.cursor.1 + 1)]), TEXT2()),
            (if d.gbk { "GBK".into() } else { "UTF-8".into() }, TEXT3()),
            (if d.crlf { "CRLF".into() } else { "LF".into() }, TEXT3()),
            (t(d.lang.name()).to_string(), TEXT3()),
            (format!("{} {}", n_lines, t("行")), TEXT3()),
        ];
        let nsel = d.ed.sels.len();
        if nsel > 1 {
            items.insert(1, (tf("{} 个光标", &[&nsel]), ACCENT()));
        } else {
            let sel = d.ed.primary_sel();
            if !sel.empty() {
                let txt = &d.buf.text[sel.range()];
                items.insert(1, (tf("已选 {} 字", &[&txt.chars().count()]), TEXT2()));
            }
        }
        if d.dirty() {
            items.insert(0, (t("未保存").to_string(), WARN()));
        }
        let mut x = status_rect.right() - 12.0;
        for (s, c) in items.iter().rev() {
            let g = sp.layout_no_wrap(s.clone(), FontId::proportional(11.5), *c);
            x -= g.size().x;
            sp.galley(pos2(x, status_rect.center().y - g.size().y / 2.0), g, *c);
            x -= 18.0;
        }
        let idx_text = if let Some(pg) = &self.index_prog {
            (tf("正在建立索引… {} 个文件", &[&pg.files.load(Ordering::Relaxed)]), ACCENT())
        } else if let Some(i) = &self.index {
            let mut s = tf("索引：{} 个符号，{} 个文件", &[&i.syms.len(), &i.files.len()]);
            match &self.lsp {
                Some(l) if l.ready() => s += " · clangd",
                Some(l) if !l.dead() => s += &format!(" · {}", t("clangd 启动中…")),
                _ => {}
            }
            (s, TEXT3())
        } else {
            (String::new(), TEXT3())
        };
        sp.text(
            pos2(status_rect.left() + 12.0, status_rect.center().y),
            Align2::LEFT_CENTER,
            idx_text.0,
            FontId::proportional(11.5),
            idx_text.1,
        );

        if let Some((w, at, b)) = want_def.or(hover_def) {
            self.hover = None;
            self.goto_definition(&w, at, b, now);
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, text_rect: Rect) {
        let Some(f) = self.find.as_mut() else { return };
        let r = Rect::from_min_size(pos2(text_rect.right() - 380.0, text_rect.top() + 6.0), vec2(360.0, 36.0));
        let mut changed = false;
        let mut step: Option<bool> = None;
        let mut close = false;
        egui::Area::new(egui::Id::new("code-find")).fixed_pos(r.min).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(BG2())
                .stroke(st(1.0, LINE_HI()))
                .corner_radius(R_CARD)
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: shade(150),
                })
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let te = ui.add_sized(vec2(170.0, 24.0), egui::TextEdit::singleline(&mut f.q).hint_text(t("查找")).margin(vec2(6.0, 3.0)));
                        if f.focus {
                            te.request_focus();
                            f.focus = false;
                        }
                        if te.changed() {
                            changed = true;
                        }
                        if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            step = Some(ui.input(|i| i.modifiers.shift));
                            te.request_focus();
                        }
                        changed |= small_toggle(ui, &mut f.case, "Aa", t("区分大小写"));
                        let n = f.hits.len();
                        let label = if f.q.is_empty() {
                            String::new()
                        } else if n == 0 {
                            t("无结果").into()
                        } else {
                            format!("{}/{}", f.cur + 1, n)
                        };
                        ui.label(RichText::new(label).small().color(if n == 0 && !f.q.is_empty() { ERR() } else { TEXT2() }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if icon_button(ui, Icon::Close, t("关闭  Esc"), false).clicked() {
                                close = true;
                            }
                            if icon_button(ui, Icon::Forward, t("下一个  Enter"), false).clicked() {
                                step = Some(false);
                            }
                            if icon_button(ui, Icon::Back, t("上一个  Shift+Enter"), false).clicked() {
                                step = Some(true);
                            }
                        });
                    });
                });
        });
        if close {
            self.find = None;
            return;
        }
        if changed {
            self.update_find();
            if let Some(f) = &self.find {
                if let Some(&h) = f.hits.get(f.cur) {
                    let len = f.q.len();
                    if let Some(d) = self.tabs.get_mut(self.active) {
                        let l = d.line_of_byte(h);
                        d.goto = Some((l, None));
                        let _ = len;
                    }
                }
            }
        }
        if let Some(b) = step {
            self.find_step(b);
            if let Some(f) = self.find.as_mut() {
                f.focus = true;
            }
        }
    }

    // ------------------------------------------------------------ 快速打开 / 定义选择

    fn quick_open(&mut self, ctx: &egui::Context) {
        let Some(qk) = self.quick.as_mut() else { return };
        let mode = match qk.q.chars().next() {
            Some('@') => QuickMode::FileSymbols,
            Some('#') => QuickMode::Symbols,
            Some(':') => QuickMode::Line,
            _ => QuickMode::Files,
        };
        let q = qk.q.trim_start_matches(['@', '#', ':']).trim().to_string();
        // 候选项：(主文字, 副文字, 打开目标)
        let mut items: Vec<(String, String, Option<Kind>, PathBuf, Option<u32>)> = vec![];
        let root = self.root.clone().unwrap_or_default();
        match mode {
            QuickMode::Files => {
                if let Some(idx) = &self.index {
                    let mut v: Vec<(i32, &PathBuf)> = idx.files.iter().filter_map(|f| cindex::fuzzy(&f.to_string_lossy(), &q).map(|s| (s, f))).collect();
                    v.sort_by(|a, b| b.0.cmp(&a.0));
                    for (_, f) in v.into_iter().take(50) {
                        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        items.push((name, f.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(), None, root.join(f), None));
                    }
                }
            }
            QuickMode::Symbols => {
                if let Some(idx) = &self.index {
                    let ql = q.to_lowercase();
                    let mut v: Vec<(i32, &Sym)> = idx
                        .syms
                        .iter()
                        .filter(|_| !ql.is_empty())
                        .filter_map(|s| cindex::fuzzy(&s.name, &ql).map(|sc| (sc + if s.name.to_lowercase() == ql { 1000 } else { 0 }, s)))
                        .collect();
                    v.sort_by(|a, b| b.0.cmp(&a.0));
                    for (_, s) in v.into_iter().take(60) {
                        let p = idx.path_of(s);
                        let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string();
                        items.push((s.name.clone(), format!("{}:{}", rel, s.line + 1), Some(s.kind), p.to_path_buf(), Some(s.line)));
                    }
                }
            }
            QuickMode::FileSymbols => {
                if let Some(d) = self.tabs.get(self.active) {
                    let ql = q.to_lowercase();
                    for s in d.syms.iter().filter(|s| ql.is_empty() || s.name.to_lowercase().contains(&ql)).take(200) {
                        items.push((s.name.clone(), format!("{} {}", t("行"), s.line + 1), Some(s.kind), d.path.clone(), Some(s.line)));
                    }
                }
            }
            QuickMode::Line => {
                if let (Some(d), Ok(n)) = (self.tabs.get(self.active), q.parse::<usize>()) {
                    items.push((tf("跳到第 {} 行", &[&n]), String::new(), None, d.path.clone(), Some(n.saturating_sub(1) as u32)));
                }
            }
        }
        let qk = self.quick.as_mut().unwrap();
        qk.sel = qk.sel.min(items.len().saturating_sub(1));
        let (up, down, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if up {
            qk.sel = qk.sel.saturating_sub(1);
        }
        if down && qk.sel + 1 < items.len() {
            qk.sel += 1;
        }
        let mut chosen = if enter { items.get(qk.sel).cloned() } else { None };
        let screen = ctx.screen_rect();
        let w = 620.0f32.min(screen.width() - 40.0);
        egui::Area::new(egui::Id::new("quick-open"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - w / 2.0, screen.top() + 60.0))
            .show(ctx, |ui| {
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
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        let hint = match mode {
                            QuickMode::Files => t("输入文件名（@ 当前文件符号，# 全部符号，: 行号）"),
                            QuickMode::Symbols => t("搜索整个工作区的函数、宏、类型"),
                            QuickMode::FileSymbols => t("当前文件里的符号"),
                            QuickMode::Line => t("输入行号"),
                        };
                        let te = ui.add_sized(
                            vec2(w, 32.0),
                            egui::TextEdit::singleline(&mut qk.q).hint_text(hint).font(FontId::proportional(15.0)).margin(vec2(10.0, 6.0)),
                        );
                        if qk.focus {
                            te.request_focus();
                            qk.focus = false;
                        }
                        if te.changed() {
                            qk.sel = 0;
                        }
                        ui.add_space(6.0);
                        if items.is_empty() {
                            let msg = if self.index.is_none() && mode != QuickMode::Line && mode != QuickMode::FileSymbols {
                                t("正在建立索引…")
                            } else {
                                t("没有匹配项")
                            };
                            ui.label(RichText::new(msg).color(TEXT3()));
                        }
                        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, it) in items.iter().enumerate() {
                                let (r, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                                let on = i == qk.sel;
                                let p = ui.painter();
                                if on {
                                    p.rect_filled(r, CornerRadius::same(6), ACCENT().gamma_multiply(0.14));
                                    if up || down {
                                        ui.scroll_to_rect(r, None);
                                    }
                                } else if resp.hovered() {
                                    p.rect_filled(r, CornerRadius::same(6), BG3());
                                }
                                match it.2 {
                                    Some(k) => kind_badge(p, pos2(r.left() + 16.0, r.center().y), k),
                                    None => file_badge(p, pos2(r.left() + 16.0, r.center().y), &it.0),
                                }
                                let g = p.layout_no_wrap(it.0.clone(), FontId::new(13.5, if it.2.is_some() { FontFamily::Monospace } else { FontFamily::Proportional }), TEXT());
                                let gw = g.size().x;
                                p.galley(pos2(r.left() + 32.0, r.center().y - g.size().y / 2.0), g, TEXT());
                                p.text(pos2(r.left() + 44.0 + gw, r.center().y), Align2::LEFT_CENTER, &it.1, FontId::proportional(12.0), TEXT3());
                                if resp.clicked() {
                                    chosen = Some(it.clone());
                                }
                            }
                        });
                    });
            });
        if let Some((name, _, kind, path, line)) = chosen {
            self.quick = None;
            match (kind, line) {
                (Some(_), Some(l)) => self.open_at_word(&path, l as usize, &name),
                (None, Some(l)) => {
                    self.open_file(&path, None);
                    self.goto_line(l as usize);
                }
                _ => self.open_file(&path, None),
            }
        }
    }

    fn pick_popup(&mut self, ctx: &egui::Context) {
        let Some((word, defs, at)) = self.pick.clone() else { return };
        let root = self.root.clone().unwrap_or_default();
        let mut chosen = None;
        let r = egui::Area::new(egui::Id::new("def-pick"))
            .order(egui::Order::Foreground)
            .fixed_pos(at + vec2(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(BG2())
                    .stroke(st(1.0, LINE_HI()))
                    .corner_radius(R_CARD)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 6],
                        blur: 24,
                        spread: 0,
                        color: shade(180),
                    })
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(tf("{} 有 {} 处定义", &[&word, &defs.len()])).font(FontId::new(12.0, bold())).color(TEXT2()));
                        ui.add_space(4.0);
                        for (i, (p, l, k, sig)) in defs.iter().enumerate() {
                            let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string();
                            let resp = ui
                                .add(egui::Button::new(RichText::new(format!("{k}  {rel}:{}", l + 1)).color(ACCENT())).frame(false))
                                .on_hover_text(sig);
                            ui.label(RichText::new(sig.lines().next().unwrap_or("")).font(FontId::new(11.5, FontFamily::Monospace)).color(TEXT3()));
                            if resp.clicked() {
                                chosen = Some(i);
                            }
                            ui.add_space(2.0);
                        }
                    });
            });
        if r.response.clicked_elsewhere() {
            self.pick = None;
        }
        if let Some(i) = chosen {
            let (p, l, _, _) = defs[i].clone();
            self.pick = None;
            self.open_at_word(&p, l as usize, &word);
        }
    }
}

// ---------------------------------------------------------------- 悬浮卡片

/// 显示符号信息；宏会展开并尽量算出数值。返回 Some(word) 表示用户点了"跳转"
fn hover_card(ctx: &egui::Context, word: &str, at: Rect, idx: Option<&Index>, doc: &Doc) -> Option<String> {
    let mut syms: Vec<Sym> = idx.map(|i| i.lookup(word).into_iter().take(3).cloned().collect()).unwrap_or_default();
    if syms.is_empty() {
        syms = doc.syms.iter().filter(|s| s.name == word).take(3).cloned().collect();
    }
    let s = syms.first()?.clone();
    let file = idx
        .map(|i| i.path_of(&s).to_path_buf())
        .filter(|_| idx.is_some_and(|i| i.lookup(word).iter().any(|x| x.line == s.line)))
        .unwrap_or_else(|| doc.path.clone());
    let root = idx.map(|i| i.root.clone()).unwrap_or_default();
    let mut go = false;
    egui::show_tooltip_at(
        ctx,
        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hover-layer")),
        egui::Id::new(("hover", word, at.min.x as i32, at.min.y as i32)),
        at.left_bottom() + vec2(0.0, 4.0),
        |ui| {
            ui.set_max_width(640.0);
            ui.horizontal(|ui| {
                kind_pill(ui, s.kind);
                ui.label(RichText::new(&s.name).font(FontId::new(13.0, FontFamily::Monospace)).color(TEXT()).strong());
                if syms.len() > 1 {
                    ui.label(RichText::new(tf("等 {} 处", &[&syms.len()])).small().color(TEXT3()));
                }
            });
            ui.add_space(4.0);
            let lang = Lang::from_path(&file);
            let code_font = FontId::new(12.5, FontFamily::Monospace);
            let sig = if s.kind == Kind::Macro {
                s.sig.clone()
            } else {
                s.sig.lines().take(6).collect::<Vec<_>>().join("\n")
            };
            egui::Frame::new().fill(BG0()).corner_radius(CornerRadius::same(4)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.label(highlight::layout(&sig, if lang == Lang::Plain { Lang::C } else { lang }, code_font.clone()));
            });
            if (s.kind == Kind::Macro && s.params.is_none()) || s.kind == Kind::EnumConst {
                if let (Some(i), Some(v)) = (idx, s.value.as_deref()) {
                    let ex = i.expand(v);
                    if ex.trim() != v.trim() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(t("展开")).small().color(TEXT3()));
                        ui.label(highlight::layout(&ex, Lang::C, code_font.clone()));
                    }
                    if let Some(n) = cindex::eval(&ex) {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(t("值")).small().color(TEXT3()));
                            let txt = if n < 0 { format!("{n}") } else { format!("0x{:X}  ({n})", n) };
                            ui.label(RichText::new(txt).font(code_font.clone()).color(highlight::pal::NUM()));
                        });
                    }
                }
            }
            ui.add_space(4.0);
            let rel = file.strip_prefix(&root).unwrap_or(&file).to_string_lossy().to_string();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}:{}", rel, s.line + 1)).small().color(TEXT3()));
                ui.label(RichText::new(t("Ctrl+点击 或 F12 跳转")).small().color(ACCENT().gamma_multiply(0.8)));
            });
            go = false;
        },
    );
    go.then(|| word.to_string())
}

// ---------------------------------------------------------------- 小部件

pub(crate) fn tree_row(ui: &mut Ui, depth: usize, name: &str, note: Option<&str>, dir: bool, open: Option<bool>, active: bool, mark: Option<char>) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(r, 0.0, ACCENT().gamma_multiply(0.12));
        p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, ACCENT());
    } else if resp.hovered() {
        p.rect_filled(r, 0.0, tint(7));
    }
    // 缩进参考线
    for k in 0..depth {
        let x = r.left() + 18.0 + k as f32 * 14.0;
        p.vline(x, r.y_range(), st(1.0, LINE().gamma_multiply(0.8)));
    }
    let x = r.left() + 12.0 + depth as f32 * 14.0;
    if dir {
        chevron(p, pos2(x + 6.0, r.center().y), open == Some(true), TEXT3());
        draw_icon(
            p,
            Rect::from_center_size(pos2(x + 22.0, r.center().y), vec2(14.0, 14.0)),
            Icon::Folder,
            if open == Some(true) { ACCENT().gamma_multiply(0.85) } else { TEXT3() },
        );
    } else {
        file_badge(p, pos2(x + 20.0, r.center().y), name);
    }
    let mc = match mark {
        Some('M') => Some(WARN()),
        Some('U') | Some('A') => Some(OK()),
        Some('D') => Some(ERR()),
        Some('•') => Some(WARN().gamma_multiply(0.7)),
        _ => None,
    };
    let g = p.layout_no_wrap(name.to_string(), FontId::proportional(12.5), mc.unwrap_or(if active { TEXT() } else { TEXT2() }));
    let gw = g.size().x;
    p.galley(pos2(x + 33.0, r.center().y - g.size().y / 2.0), g, TEXT());
    if let Some(n) = note {
        p.text(pos2(x + 40.0 + gw, r.center().y), Align2::LEFT_CENTER, n, FontId::proportional(11.0), TEXT3());
    }
    if let (Some(c), Some(m)) = (mc, mark) {
        if m == '•' {
            p.circle_filled(pos2(r.right() - 14.0, r.center().y), 3.0, c);
        } else {
            p.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, m.to_string(), FontId::new(11.0, mono_bold()), c);
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

pub(crate) fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, col: Color32) {
    let pts = if open {
        vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)]
    } else {
        vec![c + vec2(-1.5, -3.5), c + vec2(2.0, 0.0), c + vec2(-1.5, 3.5)]
    };
    p.add(Shape::line(pts, st(1.3, col)));
}

/// 文件类型小标：C 蓝、H 紫、Rust 橙……一眼区分
pub fn file_badge(p: &egui::Painter, c: egui::Pos2, name: &str) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let (label, col) = match ext.as_str() {
        "c" => ("C", ink(Color32::from_rgb(0x55, 0x9c, 0xf5))),
        "h" | "hpp" | "hh" => ("H", ink(Color32::from_rgb(0xa0, 0x7c, 0xf0))),
        "cpp" | "cc" | "cxx" => ("C+", ink(Color32::from_rgb(0x4f, 0x8c, 0xd6))),
        "rs" => ("RS", ink(Color32::from_rgb(0xf0, 0x8a, 0x4b))),
        "py" => ("PY", ink(Color32::from_rgb(0xf2, 0xc9, 0x4c))),
        "js" | "mjs" | "ts" | "tsx" | "jsx" => ("JS", ink(Color32::from_rgb(0xe8, 0xd4, 0x4d))),
        "go" => ("GO", ink(Color32::from_rgb(0x4d, 0xc6, 0xe6))),
        "java" | "kt" => ("JV", ink(Color32::from_rgb(0xe0, 0x6c, 0x5a))),
        "json" => ("{}", ink(Color32::from_rgb(0xe8, 0xc0, 0x4d))),
        "md" => ("M↓", ink(Color32::from_rgb(0x4e, 0xc9, 0xb0))),
        "s" | "asm" => ("AS", ink(Color32::from_rgb(0x9a, 0xa5, 0xb4))),
        "sh" | "bat" | "cmd" | "ps1" => ("$", ink(Color32::from_rgb(0x8b, 0xd3, 0x6a))),
        "ld" | "lds" => ("LD", ink(Color32::from_rgb(0xc0, 0x8b, 0x6a))),
        "txt" | "log" => ("≡", ink(Color32::from_rgb(0x8a, 0x93, 0xa3))),
        "yml" | "yaml" | "toml" | "ini" | "cfg" | "conf" => ("⚙", ink(Color32::from_rgb(0x9a, 0xa5, 0xb4))),
        _ if name.eq_ignore_ascii_case("makefile") || ext == "mk" || ext == "cmake" => ("MK", ink(Color32::from_rgb(0x6c, 0xc0, 0x6a))),
        _ => ("", ink(Color32::from_rgb(0x6b, 0x74, 0x84))),
    };
    let r = Rect::from_center_size(c, vec2(14.0, 14.0));
    if label.is_empty() || label == "⚙" || label == "≡" {
        // 普通文件：画个文档轮廓
        let s = st(1.2, col);
        p.add(Shape::line(
            vec![
                r.left_top() + vec2(2.5, 0.5),
                r.right_top() + vec2(-5.0, 0.5),
                r.right_top() + vec2(-2.0, 3.5),
                r.right_bottom() + vec2(-2.0, -0.5),
                r.left_bottom() + vec2(2.5, -0.5),
                r.left_top() + vec2(2.5, 0.5),
            ],
            s,
        ));
        if label == "≡" || label == "⚙" {
            for y in [5.5, 8.0, 10.5] {
                p.hline(egui::Rangef::new(r.left() + 4.5, r.right() - 4.0), r.top() + y, st(1.0, col));
            }
        }
        return;
    }
    p.rect_filled(r, CornerRadius::same(3), col.gamma_multiply(0.18));
    p.text(c, Align2::CENTER_CENTER, label, FontId::new(if label.chars().count() > 1 { 8.0 } else { 9.5 }, mono_bold()), col);
}

fn kind_color(k: Kind) -> Color32 {
    match k {
        Kind::Func | Kind::Proto => highlight::pal::FUNC(),
        Kind::Macro => highlight::pal::MACRO(),
        Kind::Struct | Kind::Typedef | Kind::Class => highlight::pal::TY(),
        Kind::Enum | Kind::EnumConst => WARN(),
        Kind::Var => highlight::pal::CONST(),
    }
}

fn kind_letter(k: Kind) -> &'static str {
    match k {
        Kind::Func => "ƒ",
        Kind::Proto => "ƒ",
        Kind::Macro => "#",
        Kind::Struct => "S",
        Kind::Enum => "E",
        Kind::EnumConst => "e",
        Kind::Typedef => "T",
        Kind::Var => "v",
        Kind::Class => "C",
    }
}

fn kind_badge(p: &egui::Painter, c: egui::Pos2, k: Kind) {
    let col = kind_color(k);
    let r = Rect::from_center_size(c, vec2(15.0, 15.0));
    p.rect(r, CornerRadius::same(3), col.gamma_multiply(0.14), st(1.0, col.gamma_multiply(0.4)), egui::StrokeKind::Inside);
    p.text(c, Align2::CENTER_CENTER, kind_letter(k), FontId::new(10.0, mono_bold()), col);
}

fn kind_pill(ui: &mut Ui, k: Kind) {
    let col = kind_color(k);
    egui::Frame::new()
        .fill(col.gamma_multiply(0.14))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(t(k.label())).size(11.0).color(col));
        });
}

fn small_toggle(ui: &mut Ui, v: &mut bool, label: &str, tip: &str) -> bool {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    let p = ui.painter();
    if *v {
        p.rect(r, CornerRadius::same(4), ACCENT().gamma_multiply(0.15), st(1.0, ACCENT().gamma_multiply(0.5)), egui::StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), BG3());
    }
    let font = if label == "ab" { FontId::new(11.0, mono_bold()) } else { FontId::new(11.5, bold()) };
    p.text(r.center(), Align2::CENTER_CENTER, label, font, if *v { ACCENT() } else { TEXT2() });
    if label == "ab" {
        p.hline(egui::Rangef::new(r.left() + 6.0, r.right() - 6.0), r.bottom() - 6.0, st(1.0, if *v { ACCENT() } else { TEXT3() }));
    }
    let clicked = resp.on_hover_text(tip).clicked();
    if clicked {
        *v = !*v;
    }
    clicked
}

fn find_word(line: &str, w: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(k) = line[from..].find(w) {
        let a = from + k;
        let e = a + w.len();
        if !line[..a].chars().next_back().is_some_and(is_ident) && !line[e..].chars().next().is_some_and(is_ident) {
            return Some(a);
        }
        from = e;
    }
    None
}

/// Windows 的 canonicalize 会加 \\?\ 前缀，显示和传给其他程序都不方便
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(r) if !r.starts_with("UNC") => PathBuf::from(r),
        _ => p,
    }
}

pub fn reveal(p: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

fn log_index(_n: usize, _ms: u128) {
    #[cfg(debug_assertions)]
    eprintln!("index: {_n} symbols in {_ms} ms");
}

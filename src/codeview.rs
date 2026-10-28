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

use crate::app::{draw_icon, hgrad_line, icon_button, truncate, Icon};
use crate::binview::{BinEvt, Special};
use crate::cindex::{self, Hit, Index, Kind, Progress, Sym};
use crate::editor::{Buf, Editor, View};

mod gen;
mod insight;
use crate::git;
use crate::highlight::{self, is_ident, Lang};
use crate::i18n::{t, tf};
use crate::keymap::Act;
use crate::lsp;
use crate::pattern::Pat;
use crate::snippets;
use crate::theme::*;
pub use gen::{KeyMacro, ProjState};

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
    pub split_ratio: f32,
    /// 改完停一秒自动保存
    pub autosave: bool,
    /// 当前行末尾显示最后修改它的提交
    pub inline_blame: bool,
    /// Vim 键位
    #[serde(default)]
    pub vim: bool,
    /// 有 clangd 和 compile_commands.json 时用 clangd 跳转定义
    pub clangd: bool,
    /// 空 = 自动查找
    pub clangd_path: String,
    /// 代码生成动作（模板）
    pub macros: Vec<crate::codegen::MacroDef>,
    /// 文件头、TODO 里的作者和版权声明
    pub author: String,
    pub copyright: String,
    /// 插入 printf 用的打印函数
    pub print_fn: String,
    /// 新建 .c / .h 文件时自动加文件头（.h 再加头文件保护）
    pub auto_header: bool,
    /// 额外当作已定义的宏（判断 #ifdef 用），如 DEBUG, USE_HAL=1
    pub defines: String,
    /// 灰显不生效的 #if 分支
    pub dim_inactive: bool,
    /// 光标所在函数体、#if 块画淡色底
    pub scope_bg: bool,
    /// 代码生成输入框里最近输入过的值
    pub macro_recent: std::collections::BTreeMap<String, Vec<String>>,
    /// 保存的按键宏
    pub recordings: Vec<KeyMacro>,
    /// 每个工程打开的文件、光标、书签
    pub projects: std::collections::BTreeMap<String, ProjState>,
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
            split_ratio: 0.5,
            autosave: false,
            inline_blame: true,
            vim: false,
            clangd: true,
            clangd_path: String::new(),
            macros: crate::codegen::builtins(),
            author: String::new(),
            copyright: String::new(),
            print_fn: "printf".into(),
            auto_header: true,
            defines: String::new(),
            dim_inactive: true,
            scope_bg: true,
            macro_recent: Default::default(),
            recordings: vec![],
            projects: Default::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Files,
    Search,
    Outline,
    Todo,
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
    bom: bool,
    /// 磁盘上文件的修改时间（发现外部修改用）
    mtime: Option<std::time::SystemTime>,
    /// 外部改了文件，而编辑器里也有没保存的修改：等用户选
    ext_changed: bool,
    /// 最后一次编辑的时间（自动保存、崩溃恢复用）
    edited_at: Option<f64>,
    /// 和 Git HEAD 比的行改动：(起始行, 结束行, 'A' / 'M' / 'D')
    git_lines: Vec<(usize, usize, char)>,
    git_ver: Option<u64>,
    /// 磁盘版本的逐行 blame；None = 还没取
    blame: Option<Vec<Option<Arc<git::BlameLine>>>>,
    blame_req: bool,
    /// 已写进崩溃恢复备份的版本
    backup_ver: Option<u64>,
    /// ELF / map / HEX / 比较等特殊查看器；show_special = 当前显示它而不是文本
    special: Option<Special>,
    show_special: bool,
    /// 不是磁盘上的文件（比较视图）
    virt: bool,
    /// 不生效的 #if 分支缓存：(文本版本, 宏环境指纹, 行范围)
    inactive: (u64, u64, Vec<(usize, usize)>),
    /// 光标所在函数体、#if 块缓存：(文本版本, 光标行, 函数体, #if 块)
    scope: (u64, usize, Option<(usize, usize)>, Option<(usize, usize)>),
}

impl Doc {
    fn load(path: &Path, root: &Path) -> Result<Doc, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        let limit = if is_firmware(path) { 64 } else { 8 };
        if meta.len() > limit * 1024 * 1024 {
            return Err(tf("文件太大（{}），超过 {} MB 不打开", &[&crate::app::human_bytes(meta.len()), &limit]));
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let decoded = if meta.len() > 8 * 1024 * 1024 { None } else { cindex::decode(&bytes) };
        let special = Special::detect(path, &bytes, decoded.as_ref().map(|(t, _)| t.as_str()));
        let (text, gbk) = match (decoded, &special) {
            (Some(d), _) => d,
            (None, Some(_)) => (String::new(), false),
            (None, None) => return Err(t("这是二进制文件，无法以文本方式查看").to_string()),
        };
        let bom = bytes.starts_with(b"\xef\xbb\xbf");
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
            bom,
            mtime: meta.modified().ok(),
            ext_changed: false,
            edited_at: None,
            git_lines: vec![],
            git_ver: None,
            blame: None,
            blame_req: false,
            backup_ver: None,
            show_special: special.is_some(),
            special,
            virt: false,
            inactive: (u64::MAX, 0, vec![]),
            scope: (u64::MAX, 0, None, None),
        };
        d.reparse();
        Ok(d)
    }

    /// 有文本内容可以编辑（不是纯二进制、不是比较视图）
    fn textual(&self) -> bool {
        !self.virt && !(self.special.is_some() && self.buf.text.is_empty())
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

    fn line_text(&self, l: usize) -> &str {
        self.buf.line(l)
    }

    /// 字节位置所在的标识符
    fn word_at(&self, b: usize) -> Option<(usize, usize, &str)> {
        self.buf.word_at(b).map(|(s, e)| (s, e, &self.buf.text[s..e]))
    }

    /// 外部修改后重新读盘：只替换变了的那一段，光标和撤销历史都留着（Ctrl+Z 能回到旧内容）
    fn reload(&mut self, now: f64) -> Result<(), String> {
        if self.virt {
            return Ok(());
        }
        let bytes = std::fs::read(&self.path).map_err(|e| e.to_string())?;
        if self.special.is_some() {
            // 重新编译后 map / elf / hex 变了：查看器跟着刷新
            let decoded = cindex::decode(&bytes);
            self.special = Special::detect(&self.path, &bytes, decoded.as_ref().map(|(t, _)| t.as_str()));
            self.show_special &= self.special.is_some();
            if decoded.is_none() {
                self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
                return Ok(());
            }
        }
        let (text, gbk) = cindex::decode(&bytes).ok_or_else(|| t("这是二进制文件，无法以文本方式查看").to_string())?;
        self.bom = bytes.starts_with(b"\xef\xbb\xbf");
        self.crlf = text.contains("\r\n");
        self.gbk = gbk;
        let new = if self.crlf { text.replace("\r\n", "\n") } else { text };
        let old = self.buf.text.clone();
        if old != new {
            let (a, b) = (old.as_bytes(), new.as_bytes());
            let mut pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
            while !old.is_char_boundary(pre) {
                pre -= 1;
            }
            let max_suf = a.len().min(b.len()) - pre;
            let mut suf = a.iter().rev().zip(b.iter().rev()).take(max_suf).take_while(|(x, y)| x == y).count();
            while !old.is_char_boundary(a.len() - suf) || !new.is_char_boundary(b.len() - suf) {
                suf -= 1;
            }
            let keep = self.ed.head();
            let ins = new[pre..b.len() - suf].to_string();
            let delta = ins.len() as isize - (a.len() - suf - pre) as isize;
            self.ed.replace_ranges(&mut self.buf, vec![(pre..a.len() - suf, ins)], now);
            let h = if keep <= pre {
                keep
            } else if keep >= a.len() - suf {
                (keep as isize + delta) as usize
            } else {
                pre
            };
            self.ed.select(h, h);
            self.ed.clamp(&self.buf);
            self.reparse();
        }
        self.ed.mark_clean();
        self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        self.ext_changed = false;
        self.edited_at = None;
        self.git_ver = None;
        self.blame = None;
        self.blame_req = false;
        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        if !self.textual() {
            return Ok(());
        }
        let text = if self.crlf { self.buf.text.replace('\n', "\r\n") } else { self.buf.text.clone() };
        let mut bytes = if self.gbk { encoding_rs::GBK.encode(&text).0.into_owned() } else { text.into_bytes() };
        if self.bom {
            bytes.splice(0..0, *b"\xef\xbb\xbf");
        }
        std::fs::write(&self.path, bytes).map_err(|e| e.to_string())?;
        self.ed.mark_clean();
        self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        self.ext_changed = false;
        self.edited_at = None;
        self.git_ver = None;
        self.blame = None;
        self.blame_req = false;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TreeOp {
    NewFile,
    NewDir,
    Rename,
}

struct TreeEdit {
    op: TreeOp,
    /// 新建：所在目录；重命名：被改名的文件
    at: PathBuf,
    text: String,
    focus: bool,
}

enum GitJob {
    Marks(PathBuf, u64, String),
    Blame(PathBuf),
    /// HEAD 可能变了（提交、切分支），丢掉缓存的 HEAD 内容
    Reset,
}

enum GitRes {
    Marks(PathBuf, u64, Vec<(usize, usize, char)>),
    Blame(PathBuf, Vec<Option<Arc<git::BlameLine>>>),
}

fn git_worker(ctx: egui::Context) -> (mpsc::Sender<GitJob>, Receiver<GitRes>) {
    let (jtx, jrx) = mpsc::channel::<GitJob>();
    let (rtx, rrx) = mpsc::channel();
    std::thread::Builder::new()
        .name("git-lines".into())
        .spawn(move || {
            let mut heads: HashMap<PathBuf, Option<String>> = HashMap::new();
            while let Ok(job) = jrx.recv() {
                // 积压的同一文件的标记请求只算最新的
                let mut jobs = vec![job];
                jobs.extend(jrx.try_iter());
                let mut latest: HashMap<PathBuf, usize> = HashMap::new();
                for (i, j) in jobs.iter().enumerate() {
                    if let GitJob::Marks(p, ..) = j {
                        latest.insert(p.clone(), i);
                    }
                }
                for (i, j) in jobs.into_iter().enumerate() {
                    let r = match j {
                        GitJob::Reset => {
                            heads.clear();
                            continue;
                        }
                        GitJob::Marks(p, ver, text) => {
                            if latest.get(&p) != Some(&i) {
                                continue;
                            }
                            let head = heads.entry(p.clone()).or_insert_with(|| git::head_text(&p));
                            let marks = head.as_deref().map(|h| git::line_marks(h, &text)).unwrap_or_default();
                            GitRes::Marks(p, ver, marks)
                        }
                        GitJob::Blame(p) => GitRes::Blame(p.clone(), git::blame(&p).unwrap_or_default()),
                    };
                    if rtx.send(r).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
        })
        .ok();
    (jtx, rrx)
}

struct Split {
    other: PathBuf,
    /// 当前（有焦点的）一侧在左边
    active_left: bool,
    /// 上一帧的当前文件，用来发现“激活了另一侧的文件”
    last: PathBuf,
}

#[derive(Clone, Copy, PartialEq)]
enum QuickMode {
    Files,
    Symbols,
    FileSymbols,
    Line,
    Snippets,
    History,
}

struct Quick {
    q: String,
    sel: usize,
    focus: bool,
}

struct Find {
    pat: Pat,
    repl: String,
    /// 显示替换行
    replace: bool,
    hits: Vec<(usize, usize)>,
    cur: usize,
    focus: bool,
    focus_repl: bool,
    /// 正则写错时的提示
    err: Option<String>,
}

struct Hover {
    word: String,
    at: usize,
    since: f64,
    rect: Rect,
}

pub enum CodeEvt {
    Toast(String, Color32),
    /// 打开了另一个工程（Git 页、终端跟着换）
    Project(PathBuf),
    /// 打开设置页的代码部分
    OpenSettings,
    /// Ctrl+点击代码里的数字：在位域计算器里打开
    BitCalc(String, Option<i128>),
    /// 在比较页比较两个文件或文件夹；带名字的一边是只读的临时文件（已保存版本、Git HEAD、未保存的内容）
    Compare(PathBuf, Option<String>, PathBuf, Option<String>),
    /// 右键「解析结构体」：结构体定义的源码
    Struct(String),
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
    search_regex: bool,
    /// 全工程替换：显示替换框、替换内容、“确认替换”的截止时间
    search_replace: bool,
    search_repl: String,
    replace_confirm: Option<f64>,
    search_err: Option<String>,
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
    /// 工具箱里载入的 SVD：悬停 USART1->CR1 时显示寄存器
    pub svd: Option<Arc<crate::toolbox::svd::Device>>,
    outline_filter: String,
    pub events: Vec<CodeEvt>,
    sem: Option<Arc<HashMap<String, Kind>>>,
    /// Source Insight 式的上下文窗口和调用关系窗口
    pub context_win: bool,
    pub relations: bool,
    ins: insight::Insight,
    /// 分屏：另一侧显示的文件（用路径记，标签增删、拖动都不受影响）
    split: Option<Split>,
    /// 正在拖动的标签
    tab_drag: Option<usize>,
    /// 文件树里正在输入的新建 / 重命名
    tree_edit: Option<TreeEdit>,
    /// 右键菜单里“删除”点了一次，等确认
    delete_armed: Option<PathBuf>,
    /// 后台算 Git 行标记和 blame
    git_jobs: Option<(mpsc::Sender<GitJob>, Receiver<GitRes>)>,
    /// clangd（可选）；lsp_for 记录它是为哪个工程、哪个设置启动的，变了就重启
    lsp: Option<lsp::Lsp>,
    lsp_for: Option<(PathBuf, bool, String)>,
    lsp_def: Option<(mpsc::Receiver<serde_json::Value>, String, egui::Pos2, f64)>,
    /// 设置页显示用：按 clangd_path 缓存的查找结果
    clangd_found: Option<(String, Option<PathBuf>)>,
    /// 上次检查磁盘修改时间的时刻
    last_stat: f64,
    snippets: Vec<snippets::Snippet>,
    /// 文件树里“选择以进行比较”选中的第一个文件
    cmp_pick: Option<PathBuf>,
    /// 工程里的 TODO / FIXME；todo_for 记录是基于哪一版索引扫的
    todos: Vec<Hit>,
    todo_rx: Option<Receiver<Vec<Hit>>>,
    todo_for: Option<usize>,
    todo_filter: String,
    /// 启动时发现的崩溃恢复备份：(备份文件, 原文件)
    pub recovery: Vec<(PathBuf, PathBuf)>,
    gen: gen::GenState,
    /// 右键菜单里显示的快捷键
    key_labels: HashMap<Act, String>,
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
            search_regex: false,
            search_replace: false,
            search_repl: String::new(),
            replace_confirm: None,
            search_err: None,
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
            svd: None,
            outline_filter: String::new(),
            events: vec![],
            sem: None,
            context_win: false,
            relations: false,
            ins: insight::Insight::default(),
            split: None,
            tab_drag: None,
            git_jobs: None,
            tree_edit: None,
            delete_armed: None,
            lsp: None,
            lsp_for: None,
            lsp_def: None,
            clangd_found: None,
            last_stat: 0.0,
            snippets: snippets::load(),
            cmp_pick: None,
            todos: vec![],
            todo_rx: None,
            todo_for: None,
            todo_filter: String::new(),
            recovery: scan_recovery(),
            gen: Default::default(),
            key_labels: HashMap::new(),
        };
        v.s.font_size = v.s.font_size.clamp(9.0, 28.0);
        crate::codegen::merge(&mut v.s.macros);
        v.publish_macro_names();
        v
    }

    /// 让命令面板、快捷键设置显示动作的名字
    pub fn publish_macro_names(&self) {
        crate::keymap::set_macro_names(self.s.macros.iter().map(|m| m.name.clone()).collect());
    }

    /// 第一次进入代码页时才真正打开上次的文件夹（不进入就不占资源）
    pub fn activate(&mut self) {
        if self.root.is_none() && !self.s.root.is_empty() {
            let p = PathBuf::from(&self.s.root);
            if p.is_dir() {
                let tabs = self.s.tabs.clone();
                let known = self.s.projects.contains_key(&self.s.root);
                self.open_root(p);
                if !known {
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
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    fn toast(&mut self, m: impl Into<String>, c: Color32) {
        self.events.push(CodeEvt::Toast(m.into(), c));
    }

    pub fn open_root(&mut self, p: PathBuf) {
        let p = std::fs::canonicalize(&p).map(strip_unc).unwrap_or(p);
        if self.root.as_deref() == Some(p.as_path()) {
            return;
        }
        // 先记下当前工程的状态，换回来时恢复
        self.snapshot();
        for i in (0..self.tabs.len()).rev() {
            if self.tabs[i].dirty() && !self.tabs[i].virt {
                let _ = self.tabs[i].save();
            }
        }
        self.split = None;
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
        self.restore_project(&p);
        self.sync_tabs();
        self.events.push(CodeEvt::Project(p));
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
        if let Some((tx, _)) = &self.git_jobs {
            let _ = tx.send(GitJob::Reset);
        }
        for d in &mut self.tabs {
            d.git_ver = None;
            if !d.dirty() {
                d.blame_req = false;
            }
        }
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

    /// 选一个文件夹作为工程（代码页、Git 页、终端共用）
    pub fn pick_project(&mut self) {
        self.pick_folder();
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
            drop_backup(&mut self.tabs[i]);
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
        self.s.tabs = self.tabs.iter().filter(|d| !d.virt).map(|d| d.path.to_string_lossy().to_string()).collect();
    }

    fn nav(&mut self, back: bool) {
        self.gen.quiet = 3;
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
        self.search_regex = false;
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
        self.replace_confirm = None;
        self.search_err = None;
        if self.search_q.is_empty() {
            self.results.clear();
            self.results_for.clear();
            return;
        }
        let re = match self.search_pat().compile() {
            Ok(re) => re,
            Err(e) => {
                self.search_err = Some(e);
                return;
            }
        };
        let cancel = cindex::shared_progress();
        let c2 = cancel.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let r = cindex::search(&root, &idx.files, &re, &c2.cancel, 5000);
            let _ = tx.send(r);
        });
        self.search_rx = Some(rx);
        self.search_cancel = Some(cancel);
        self.results_for = self.search_q.clone();
        self.collapsed_results.clear();
    }

    fn search_pat(&self) -> Pat {
        Pat {
            q: self.search_q.clone(),
            case: self.search_case,
            word: self.search_word,
            regex: self.search_regex,
        }
    }

    /// 把全工程的匹配全部替换：已打开的文件在编辑器里改（可撤销、需保存），其他文件直接写回磁盘
    fn replace_in_files(&mut self, now: f64) {
        let Some(root) = self.root.clone() else { return };
        let pat = self.search_pat();
        let Ok(re) = pat.compile() else { return };
        let repl = self.search_repl.clone();
        let mut files: Vec<PathBuf> = vec![];
        for h in &self.results {
            if files.last() != Some(&h.path) && !files.contains(&h.path) {
                files.push(h.path.clone());
            }
        }
        let (mut n_files, mut n_hits, mut failed) = (0, 0, vec![]);
        for rel in files {
            let p = root.join(&rel);
            if let Some(d) = self.tabs.iter_mut().find(|d| d.path == p) {
                let hits = Pat::find_all(&re, &d.buf.text, 100_000);
                if hits.is_empty() {
                    continue;
                }
                let items: Vec<(std::ops::Range<usize>, String)> = hits.iter().map(|&(a, b)| (a..b, pat.replacement(&re, &d.buf.text, (a, b), &repl))).collect();
                n_hits += items.len();
                n_files += 1;
                d.ed.replace_ranges(&mut d.buf, items, now);
                d.syms_stale = Some(now);
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                failed.push(rel);
                continue;
            };
            let bom = bytes.starts_with(b"\xef\xbb\xbf");
            let Some((text, gbk)) = cindex::decode(&bytes) else { continue };
            let hits = Pat::find_all(&re, &text, 100_000);
            if hits.is_empty() {
                continue;
            }
            let mut out = String::with_capacity(text.len());
            let mut last = 0;
            for &(a, b) in &hits {
                out.push_str(&text[last..a]);
                out.push_str(&pat.replacement(&re, &text, (a, b), &repl));
                last = b;
            }
            out.push_str(&text[last..]);
            let mut data = if gbk { encoding_rs::GBK.encode(&out).0.into_owned() } else { out.into_bytes() };
            if bom {
                data.splice(0..0, *b"\xef\xbb\xbf");
            }
            match std::fs::write(&p, data) {
                Ok(()) => {
                    n_hits += hits.len();
                    n_files += 1;
                }
                Err(_) => failed.push(rel),
            }
        }
        if failed.is_empty() {
            self.toast(tf("已替换 {} 处，{} 个文件", &[&n_hits, &n_files]), OK());
        } else {
            self.toast(tf("已替换 {} 处；{} 个文件写入失败", &[&n_hits, &failed.len()]), WARN());
        }
        self.run_search();
        self.refresh_git();
    }

    /// 编辑停下来 0.6 秒后重新算 Git 行标记；打开 / 保存后取 blame；顺便做自动保存
    fn poll_git_lines(&mut self, ctx: &egui::Context) {
        if self.root.is_none() || self.tabs.is_empty() {
            return;
        }
        let now = ctx.input(|i| i.time);
        let (tx, _) = self.git_jobs.get_or_insert_with(|| git_worker(ctx.clone()));
        let tx = tx.clone();
        let blame_on = self.s.inline_blame;
        let mut saved = vec![];
        for d in self.tabs.iter_mut().filter(|d| d.textual()) {
            let idle = d.edited_at.map_or(true, |t| now - t > 0.6);
            if d.git_ver != Some(d.buf.ver) && idle {
                d.git_ver = Some(d.buf.ver);
                let _ = tx.send(GitJob::Marks(d.path.clone(), d.buf.ver, d.buf.text.clone()));
            } else if !idle {
                ctx.request_repaint_after(std::time::Duration::from_millis(300));
            }
            if blame_on && !d.blame_req {
                d.blame_req = true;
                let _ = tx.send(GitJob::Blame(d.path.clone()));
            }
            if self.s.autosave && d.dirty() && !d.ext_changed && d.edited_at.is_some_and(|t| now - t > 1.0) {
                match d.save() {
                    Ok(()) => saved.push(d.rel.clone()),
                    Err(e) => {
                        d.edited_at = None;
                        self.events.push(CodeEvt::Toast(e, ERR()));
                    }
                }
            } else if self.s.autosave && d.dirty() && d.edited_at.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(300));
            }
        }
        if !saved.is_empty() {
            self.refresh_git();
        }
        let Some((_, rx)) = &self.git_jobs else { return };
        for r in rx.try_iter() {
            match r {
                GitRes::Marks(p, ver, m) => {
                    if let Some(d) = self.tabs.iter_mut().find(|d| d.path == p && d.buf.ver == ver) {
                        d.git_lines = m;
                    }
                }
                GitRes::Blame(p, b) => {
                    if let Some(d) = self.tabs.iter_mut().find(|d| d.path == p) {
                        d.blame = Some(b);
                    }
                }
            }
        }
    }

    /// 没保存的文件停手 2 秒后备份一份（崩溃 / 断电后能找回）；每隔 1.5 秒看一眼打开的文件有没有被外部改过
    fn poll_disk(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for d in &mut self.tabs {
            if d.dirty() {
                if d.backup_ver != Some(d.buf.ver) {
                    if d.edited_at.is_none_or(|t| now - t > 2.0) {
                        d.backup_ver = Some(d.buf.ver);
                        write_backup(&d.path, &d.buf.text);
                    } else {
                        ctx.request_repaint_after(std::time::Duration::from_millis(600));
                    }
                }
            } else {
                drop_backup(d);
            }
        }
        if now - self.last_stat < 1.5 {
            return;
        }
        self.last_stat = now;
        let mut reloaded = vec![];
        for d in self.tabs.iter_mut().filter(|d| !d.virt) {
            let Ok(m) = std::fs::metadata(&d.path).and_then(|m| m.modified()) else { continue };
            if d.mtime == Some(m) {
                continue;
            }
            d.mtime = Some(m);
            if d.dirty() {
                d.ext_changed = true;
            } else if d.reload(now).is_ok() {
                reloaded.push(d.rel.clone());
            }
        }
        if !reloaded.is_empty() {
            self.refresh_git();
        }
    }

    /// 把备份恢复进编辑器（文件会处于“未保存”状态，用户自己决定存不存）
    pub fn restore_recovery(&mut self) {
        let items = std::mem::take(&mut self.recovery);
        let mut n = 0;
        for (bak, path) in items {
            let Some(text) = read_backup(&bak) else { continue };
            let _ = std::fs::remove_file(&bak);
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&path, "");
            }
            self.open_file(&path, None);
            if let Some(d) = self.tabs.iter_mut().find(|d| d.path == path) {
                if d.buf.text != text {
                    let len = d.buf.text.len();
                    d.ed.replace_ranges(&mut d.buf, vec![(0..len, text)], 0.0);
                    d.ed.select(0, 0);
                    d.reparse();
                }
                n += 1;
            }
        }
        if n > 0 {
            self.toast(tf("已恢复 {} 个文件，记得保存", &[&n]), OK());
        }
    }

    pub fn discard_recovery(&mut self) {
        for (bak, _) in std::mem::take(&mut self.recovery) {
            let _ = std::fs::remove_file(bak);
        }
    }

    /// 编辑区上方的提示条：崩溃恢复、外部修改
    fn notice_bars(&mut self, ui: &mut Ui) {
        if !self.recovery.is_empty() {
            let msg = tf("上次有 {} 个文件的修改没保存就退出了", &[&self.recovery.len()]);
            match notice_bar(ui, &msg, &[t("恢复"), t("丢弃")]) {
                Some(0) => self.restore_recovery(),
                Some(1) => self.discard_recovery(),
                _ => {}
            }
        }
        let Some(d) = self.tabs.get_mut(self.active) else { return };
        if d.ext_changed {
            let now = ui.input(|i| i.time);
            match notice_bar(ui, &tf("{} 已在外部修改", &[&d.rel]), &[t("重新加载"), t("保留我的修改")]) {
                Some(0) => {
                    if let Err(e) = d.reload(now) {
                        self.events.push(CodeEvt::Toast(e, ERR()));
                    }
                    self.refresh_git();
                }
                Some(1) => d.ext_changed = false,
                _ => {}
            }
        }
    }

    /// 比较两个文件或文件夹：交给比较页
    fn compare(&mut self, a: &Path, b: &Path) {
        self.events.push(CodeEvt::Compare(a.to_path_buf(), None, b.to_path_buf(), None));
    }

    /// 编辑器里的内容和磁盘上已保存的版本比较
    fn compare_saved(&mut self, i: usize) {
        let Some(d) = self.tabs.get(i) else { return };
        let name = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let buf = crate::compareview::temp_file(&name, d.buf.text.as_bytes());
        self.events.push(CodeEvt::Compare(
            d.path.clone(),
            Some(format!("{name} ({})", t("已保存"))),
            buf,
            Some(format!("{name} ({})", t("编辑中"))),
        ));
    }

    /// 和 Git 里最后一次提交的版本比较
    fn compare_head(&mut self, i: usize) {
        let Some(d) = self.tabs.get(i) else { return };
        let name = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let dir = d.path.parent().unwrap_or(Path::new("."));
        match crate::compareview::git_blob(dir, &format!("HEAD:./{name}")) {
            Some(bytes) => {
                let head = crate::compareview::temp_file(&name, &bytes);
                self.events.push(CodeEvt::Compare(head, Some(format!("{name} (HEAD)")), d.path.clone(), None));
            }
            None => self.toast(t("这个文件不在 Git 仓库里，或者还没提交过"), WARN()),
        }
    }

    fn handle_bin_evt(&mut self, e: Option<BinEvt>, at: egui::Pos2, now: f64) {
        match e {
            Some(BinEvt::Symbol(name)) => {
                if self.index.as_ref().is_some_and(|i| !i.lookup(&name).is_empty()) {
                    self.goto_definition_index(&name, at);
                } else {
                    self.toast(tf("工程里没有找到 {} 的定义", &[&name]), WARN());
                }
                let _ = now;
            }
            Some(BinEvt::Toast(m, c)) => self.toast(m, c),
            None => {}
        }
    }

    /// Vim 的 : 命令、/ 搜索、n / N
    fn vim_command(&mut self, c: &str, now: f64) {
        match c {
            "/" | "?" => {
                let old = self.find.take();
                self.find = Some(Find {
                    // 和 Vim 一样从空白开始输入，大小写 / 正则等选项保留
                    pat: crate::pattern::Pat {
                        q: String::new(),
                        ..old.as_ref().map(|f| f.pat.clone()).unwrap_or_default()
                    },
                    repl: old.as_ref().map(|f| f.repl.clone()).unwrap_or_default(),
                    replace: false,
                    hits: vec![],
                    cur: 0,
                    focus: true,
                    focus_repl: false,
                    err: None,
                });
                self.update_find();
            }
            "n" | "N" => {
                // 从光标位置找下一个 / 上一个匹配，到头了绕回去；光标停在匹配开头
                let (Some(f), Some(d)) = (self.find.as_mut(), self.tabs.get_mut(self.active)) else { return };
                if f.hits.is_empty() {
                    return;
                }
                let at = d.ed.primary_sel().min();
                let i = if c == "N" {
                    f.hits.iter().rposition(|&(h, _)| h < at).unwrap_or(f.hits.len() - 1)
                } else {
                    f.hits.iter().position(|&(h, _)| h > at).unwrap_or(0)
                };
                f.cur = i;
                let h = f.hits[i].0;
                d.ed.select(h, h);
                d.ed.focus_req = true;
            }
            _ => {
                let cmd = c.trim_start_matches(':').trim();
                if let Ok(n) = cmd.parse::<usize>() {
                    self.goto_line(n.saturating_sub(1));
                    return;
                }
                match cmd {
                    "w" | "wa" | "wq" | "x" | "wqa" | "xa" => {
                        let all = cmd.ends_with('a');
                        let mut saved = vec![];
                        for (i, d) in self.tabs.iter_mut().enumerate() {
                            if (all || i == self.active) && d.dirty() {
                                match d.save() {
                                    Ok(()) => saved.push(d.path.clone()),
                                    Err(e) => self.events.push(CodeEvt::Toast(e, ERR())),
                                }
                            }
                        }
                        for p in &saved {
                            self.after_save(p);
                        }
                        if !saved.is_empty() {
                            self.toast(t("已保存"), OK());
                            self.refresh_git();
                        }
                        if cmd.starts_with("wq") || cmd.starts_with('x') {
                            self.close_tab(self.active);
                        }
                    }
                    "q" | "q!" | "bd" => self.close_tab(self.active),
                    "noh" | "nohl" | "nohlsearch" => self.find = None,
                    "vs" | "vsp" | "vsplit" | "sp" | "split" => self.toggle_split(),
                    "bn" | "bnext" => {
                        if !self.tabs.is_empty() {
                            self.active = (self.active + 1) % self.tabs.len();
                        }
                    }
                    "bp" | "bprev" => {
                        if !self.tabs.is_empty() {
                            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                        }
                    }
                    _ => self.toast(tf("不支持的命令：{}", &[&cmd]), WARN()),
                }
                let _ = now;
            }
        }
    }

    /// 保存后的处理：改的是代码片段文件就重新读
    fn after_save(&mut self, p: &Path) {
        if p == snippets::path() {
            match std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
                Ok(v) => {
                    self.snippets = v;
                    self.toast(tf("已载入 {} 个代码片段", &[&self.snippets.len()]), OK());
                }
                Err(e) => self.toast(tf("代码片段文件格式有误：{}", &[&e]), ERR()),
            }
        }
    }

    /// 打开代码片段文件编辑（第一次会写入默认片段）
    pub fn edit_snippets(&mut self) {
        match snippets::ensure_file() {
            Ok(p) => self.open_file(&p, None),
            Err(e) => self.toast(e.to_string(), ERR()),
        }
    }

    /// 表达式求值，先展开工程里的宏（位域计算器用）
    pub fn eval_expr(&self, s: &str) -> Option<i128> {
        cindex::eval(s).or_else(|| self.index.as_ref().and_then(|i| cindex::eval(&i.expand(s))))
    }

    /// 当前行的行内 blame 文字
    fn blame_text(d: &Doc) -> Option<String> {
        let l = d.cursor.0;
        if d.dirty() {
            return d.git_lines.iter().any(|&(a, b, k)| k != 'D' && a <= l && l <= b).then(|| t("未提交的修改").to_string());
        }
        let b = d.blame.as_ref()?.get(l)?;
        let Some(b) = b else { return Some(t("未提交的修改").to_string()) };
        let when = crate::gitview::relative(crate::gitview::now_secs(), b.time);
        let mut summary = b.summary.clone();
        if summary.chars().count() > 60 {
            summary = summary.chars().take(60).collect::<String>() + "…";
        }
        Some(format!("{}, {} · {}", b.author, when, summary))
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_lsp(ctx);
        self.poll_git_lines(ctx);
        self.poll_disk(ctx);
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

    pub fn ui(&mut self, ctx: &egui::Context, acts: &[Act], km: &crate::keymap::Keymap) {
        self.poll(ctx);
        if self.key_labels.is_empty() || ctx.input(|i| i.pointer.secondary_pressed()) {
            self.key_labels = crate::keymap::ACTIONS.iter().map(|d| (d.act, km.label(d.act))).collect();
        }
        let mut all = self.gen_frame(ctx, acts);
        all.extend_from_slice(acts);
        self.shortcuts(ctx, &all);

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
                self.notice_bars(ui);
                self.welcome(ui);
            } else {
                self.tab_bar(ui);
                self.breadcrumbs(ui);
                self.notice_bars(ui);
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
                self.sync_split();
                match self.split.as_ref().and_then(|sp| self.tabs.iter().position(|d| d.path == sp.other)) {
                    Some(oi) => self.split_editors(ui, oi),
                    None => self.editor(ui),
                }
            }
        });
        self.quick_open(ctx);
        self.pick_popup(ctx);
        self.prompt_ui(ctx);
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
            if has(Act::FindInFile) || has(Act::ReplaceInFile) {
                let replace = has(Act::ReplaceInFile);
                let sel = self.selected_text(ctx).filter(|s| !s.contains('\n') && s.len() < 200);
                let old = self.find.take();
                let mut pat = old.as_ref().map(|f| f.pat.clone()).unwrap_or_default();
                if let Some(q) = sel {
                    pat.q = if pat.regex { regex_lite::escape(&q) } else { q };
                }
                self.find = Some(Find {
                    pat,
                    repl: old.as_ref().map(|f| f.repl.clone()).unwrap_or_default(),
                    replace: replace || old.as_ref().is_some_and(|f| f.replace),
                    hits: vec![],
                    cur: 0,
                    focus: true,
                    focus_repl: false,
                    err: None,
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
                            Ok(()) => {
                                let p = d.path.clone();
                                self.toast(t("已保存"), OK());
                                self.after_save(&p);
                            }
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
        if has(Act::SplitEditor) {
            self.toggle_split();
        }
        if has(Act::ToggleBlame) {
            self.s.inline_blame = !self.s.inline_blame;
        }
        if has(Act::ToggleVim) {
            self.s.vim = !self.s.vim;
            self.toast(if self.s.vim { t("已打开 Vim 键位") } else { t("已关闭 Vim 键位") }, OK());
        }
        if has(Act::InsertSnippet) && !self.tabs.is_empty() {
            self.quick = Some(Quick { q: "!".into(), sel: 0, focus: true });
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
        for a in acts {
            if !self.tabs.is_empty() || matches!(a, Act::NavHistory) {
                self.gen_act(ctx, *a);
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
        f.err = None;
        if f.pat.q.is_empty() {
            return;
        }
        match f.pat.compile() {
            Ok(re) => f.hits = Pat::find_all(&re, &d.buf.text, 20_000),
            Err(e) => f.err = Some(e),
        }
        // 从光标附近开始
        let cur_b = d.ed.primary_sel().min();
        f.cur = f.hits.iter().position(|&(h, _)| h >= cur_b).unwrap_or(0);
    }

    fn find_step(&mut self, back: bool) {
        let Some(f) = self.find.as_mut() else { return };
        if f.hits.is_empty() {
            return;
        }
        let n = f.hits.len();
        // 光标正停在当前项上才前进，否则先跳到当前项
        let at_cur = self.tabs.get(self.active).is_some_and(|d| {
            let s = d.ed.primary_sel();
            (s.min(), s.max()) == f.hits[f.cur]
        });
        if at_cur || back {
            f.cur = if back { (f.cur + n - 1) % n } else { (f.cur + 1) % n };
        }
        let (h, e) = f.hits[f.cur];
        if let Some(d) = self.tabs.get_mut(self.active) {
            // 只选中并滚过去，键盘焦点留在查找框
            d.ed.select(h, e);
        }
    }

    /// 替换当前项（all = 全部替换）；返回替换了几处
    fn replace(&mut self, all: bool, now: f64) -> usize {
        let (Some(f), Some(d)) = (self.find.as_ref(), self.tabs.get_mut(self.active)) else { return 0 };
        let Ok(re) = f.pat.compile() else { return 0 };
        let targets: Vec<(usize, usize)> = if all { f.hits.clone() } else { f.hits.get(f.cur).copied().into_iter().collect() };
        if targets.is_empty() {
            return 0;
        }
        let items: Vec<(std::ops::Range<usize>, String)> = targets.iter().map(|&(a, b)| (a..b, f.pat.replacement(&re, &d.buf.text, (a, b), &f.repl))).collect();
        let n = items.len();
        d.ed.replace_ranges(&mut d.buf, items, now);
        d.syms_stale = Some(now);
        self.update_find();
        if !all {
            // 跳到下一处，方便连续按“替换”
            if let (Some(f), Some(d)) = (self.find.as_ref(), self.tabs.get_mut(self.active)) {
                if let Some(&(h, e)) = f.hits.get(f.cur) {
                    d.ed.select(h, e);
                }
            }
        }
        n
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
                (Side::Todo, Icon::Todo, t("TODO / FIXME 列表")),
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
                let fb = icon_button(ui, Icon::Folder, t("打开文件夹（右键：最近的工程）"), false);
                if fb.clicked() {
                    self.pick_folder();
                }
                let mut open: Option<PathBuf> = None;
                fb.context_menu(|ui| {
                    ui.label(RichText::new(t("最近的工程")).size(11.5).color(TEXT3()));
                    for r in self.s.recent.iter().filter(|r| Some(Path::new(r.as_str())) != self.root.as_deref()) {
                        let name = Path::new(r).file_name().map_or(r.clone(), |n| n.to_string_lossy().to_string());
                        if ui.button(&name).on_hover_text(r).clicked() {
                            open = Some(PathBuf::from(r));
                            ui.close_menu();
                        }
                    }
                    if self.s.recent.len() > 1 {
                        ui.separator();
                        if ui.button(t("清空列表")).clicked() {
                            let cur = self.root.as_ref().map(|r| r.to_string_lossy().to_string());
                            self.s.recent.retain(|r| Some(r) == cur.as_ref());
                            ui.close_menu();
                        }
                    }
                });
                if let Some(p) = open {
                    self.open_root(p);
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
            Side::Todo => self.todo_panel(ui),
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
            // 空白处右键：在根目录新建
            let rest = ui.available_rect_before_wrap();
            let r = ui.allocate_rect(Rect::from_min_size(rest.min, vec2(rest.width(), rest.height().max(40.0))), Sense::click());
            let mut op = None;
            r.context_menu(|ui| {
                if ui.button(t("新建文件…")).clicked() {
                    op = Some(TreeOp::NewFile);
                    ui.close_menu();
                }
                if ui.button(t("新建文件夹…")).clicked() {
                    op = Some(TreeOp::NewDir);
                    ui.close_menu();
                }
            });
            if let Some(op) = op {
                self.tree_edit = Some(TreeEdit {
                    op,
                    at: root.clone(),
                    text: String::new(),
                    focus: true,
                });
            }
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
        if self.tree_edit.as_ref().is_some_and(|e| e.op != TreeOp::Rename && e.at == dir) {
            self.tree_edit_row(ui, depth);
        }
        for (path, name, is_dir) in nodes {
            if self.tree_edit.as_ref().is_some_and(|e| e.op == TreeOp::Rename && e.at == path) {
                self.tree_edit_row(ui, depth);
                continue;
            }
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
            let mut op: Option<TreeOp> = None;
            let mut delete = false;
            let mut compare_with: Option<PathBuf> = None;
            let mut head_of: Option<PathBuf> = None;
            r.context_menu(|ui| {
                ui.set_min_width(190.0);
                if ui.button(t("新建文件…")).clicked() {
                    op = Some(TreeOp::NewFile);
                    ui.close_menu();
                }
                if ui.button(t("新建文件夹…")).clicked() {
                    op = Some(TreeOp::NewDir);
                    ui.close_menu();
                }
                if ui.button(t("重命名…")).clicked() {
                    op = Some(TreeOp::Rename);
                    ui.close_menu();
                }
                // 删除要点两次
                let armed = self.delete_armed.as_ref() == Some(&path);
                let label = if armed {
                    RichText::new(tf("确认删除 {}？", &[&name])).color(ERR())
                } else {
                    RichText::new(t("删除"))
                };
                if ui.button(label).clicked() {
                    if armed {
                        delete = true;
                        ui.close_menu();
                    } else {
                        self.delete_armed = Some(path.clone());
                    }
                }
                ui.separator();
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
                ui.separator();
                if let Some(first) = self.cmp_pick.clone().filter(|p| *p != path && p.is_dir() == is_dir) {
                    let n = first.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    if ui.button(tf("与“{}”比较", &[&n])).clicked() {
                        compare_with = Some(first);
                        ui.close_menu();
                    }
                }
                if ui.button(t("选为比较左侧")).clicked() {
                    self.cmp_pick = Some(path.clone());
                    self.toast(t("再在另一个文件上右键「与…比较」"), ACCENT());
                    ui.close_menu();
                }
                if !is_dir && ui.button(t("与 Git HEAD 比较")).clicked() {
                    head_of = Some(path.clone());
                    ui.close_menu();
                }
            });
            if let Some(first) = compare_with.take() {
                self.compare(&first, &path);
            }
            if let Some(p) = head_of.take() {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                match crate::compareview::git_blob(p.parent().unwrap_or(Path::new(".")), &format!("HEAD:./{name}")) {
                    Some(bytes) => {
                        let head = crate::compareview::temp_file(&name, &bytes);
                        self.events.push(CodeEvt::Compare(head, Some(format!("{name} (HEAD)")), p, None));
                    }
                    None => self.toast(t("这个文件不在 Git 仓库里，或者还没提交过"), WARN()),
                }
            }
            if let Some(op) = op {
                // 在文件夹上新建就建在里面，在文件上新建就建在同一层
                let at = match op {
                    TreeOp::Rename => path.clone(),
                    _ if is_dir => path.clone(),
                    _ => dir.to_path_buf(),
                };
                if op != TreeOp::Rename {
                    self.expanded.insert(at.clone());
                }
                self.tree_edit = Some(TreeEdit {
                    op,
                    at,
                    text: if op == TreeOp::Rename { name.clone() } else { String::new() },
                    focus: true,
                });
            }
            if delete {
                self.delete_path(&path);
            }
            if is_dir && open {
                self.tree_rows(ui, &path, depth + 1);
            }
        }
    }

    /// 文件树里的输入框（新建 / 重命名）。回车确认，Esc 或点别处取消
    fn tree_edit_row(&mut self, ui: &mut Ui, depth: usize) {
        let Some(e) = self.tree_edit.as_mut() else { return };
        let x = 12.0 + depth as f32 * 14.0 + 30.0;
        let mut done: Option<bool> = None;
        ui.horizontal(|ui| {
            ui.set_height(26.0);
            ui.add_space(x - 18.0);
            let icon = if e.op == TreeOp::NewDir { Icon::Folder } else { Icon::Files };
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            draw_icon(ui.painter(), r, icon, ACCENT());
            let hint = match e.op {
                TreeOp::NewFile => t("文件名（可以带子目录，如 src/a.c）"),
                TreeOp::NewDir => t("文件夹名"),
                TreeOp::Rename => t("新名字"),
            };
            let te = ui.add_sized(vec2(ui.available_width() - 10.0, 22.0), egui::TextEdit::singleline(&mut e.text).hint_text(hint).margin(vec2(4.0, 2.0)));
            if e.focus {
                te.request_focus();
                if e.op == TreeOp::Rename {
                    // 重命名时先选中不带扩展名的部分
                    let stem = e.text.rfind('.').filter(|&i| i > 0).unwrap_or(e.text.len());
                    let n = e.text[..stem].chars().count();
                    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                        st.store(ui.ctx(), te.id);
                    }
                }
                e.focus = false;
            }
            if te.lost_focus() {
                done = Some(ui.input(|i| i.key_pressed(Key::Enter)));
            }
        });
        match done {
            Some(true) => self.commit_tree_edit(),
            Some(false) => self.tree_edit = None,
            None => {}
        }
    }

    fn commit_tree_edit(&mut self) {
        let Some(e) = self.tree_edit.take() else { return };
        let name = e.text.trim().trim_matches(['/', '\\']).to_string();
        if name.is_empty() || name.contains("..") {
            return;
        }
        let res: Result<PathBuf, String> = match e.op {
            TreeOp::NewFile => {
                let p = e.at.join(&name);
                if p.exists() {
                    Err(tf("{} 已经存在", &[&name]))
                } else {
                    p.parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|_| std::fs::write(&p, b""))
                        .map(|_| p)
                        .map_err(|e| e.to_string())
                }
            }
            TreeOp::NewDir => {
                let p = e.at.join(&name);
                std::fs::create_dir_all(&p).map(|_| p).map_err(|e| e.to_string())
            }
            TreeOp::Rename => {
                let p = e.at.with_file_name(&name);
                if p == e.at {
                    return;
                }
                if p.exists() {
                    Err(tf("{} 已经存在", &[&name]))
                } else {
                    std::fs::rename(&e.at, &p).map(|_| p).map_err(|e| e.to_string())
                }
            }
        };
        match res {
            Ok(p) => {
                if e.op == TreeOp::Rename {
                    // 打开着的标签跟着改名（包括改名的文件夹下面的文件）
                    let root = self.root.clone().unwrap_or_default();
                    for d in &mut self.tabs {
                        if let Ok(rest) = d.path.strip_prefix(&e.at) {
                            d.path = if rest.as_os_str().is_empty() { p.clone() } else { p.join(rest) };
                            d.rel = d.path.strip_prefix(&root).unwrap_or(&d.path).to_string_lossy().replace('\\', "/");
                            d.lang = Lang::from_path(&d.path);
                        }
                    }
                    self.sync_tabs();
                }
                self.reload_tree_for(&p);
                // 展开到新建 / 改名后的位置，让它在树里看得见
                let mut dir = p.parent();
                while let Some(d) = dir.filter(|d| self.root.as_deref().is_some_and(|r| d.starts_with(r))) {
                    self.expanded.insert(d.to_path_buf());
                    dir = d.parent();
                }
                if e.op == TreeOp::NewFile {
                    self.open_file(&p, None);
                    self.auto_header_new_file();
                }
                self.reindex();
                self.refresh_git();
            }
            Err(msg) => self.toast(msg, ERR()),
        }
    }

    fn delete_path(&mut self, p: &Path) {
        self.delete_armed = None;
        let r = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        match r {
            Ok(()) => {
                // 关掉已删除文件的标签（没保存的修改也一起丢弃，之前已经确认过）
                while let Some(i) = self.tabs.iter().position(|d| d.path.starts_with(p)) {
                    drop_backup(&mut self.tabs.remove(i));
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len().saturating_sub(1);
                    }
                }
                self.sync_tabs();
                self.reload_tree_for(p);
                self.reindex();
                self.refresh_git();
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.toast(tf("已删除 {}", &[&name]), OK());
            }
            Err(e) => self.toast(e.to_string(), ERR()),
        }
    }

    /// 重新读取某个路径所在的目录（以及它自己，如果是目录）
    fn reload_tree_for(&mut self, p: &Path) {
        let dirs: Vec<PathBuf> = self.tree.keys().filter(|d| p.starts_with(d) || d.starts_with(p)).cloned().collect();
        for d in dirs {
            if d.is_dir() {
                self.load_dir(&d);
            } else {
                self.tree.remove(&d);
            }
        }
    }

    fn search_panel(&mut self, ui: &mut Ui) {
        let now = ui.input(|i| i.time);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            let (r, resp) = ui.allocate_exact_size(vec2(16.0, 26.0), Sense::click());
            chevron(ui.painter(), r.center(), self.search_replace, if resp.hovered() { TEXT() } else { TEXT3() });
            if resp.on_hover_text(t("切换替换")).clicked() {
                self.search_replace = !self.search_replace;
            }
            let w = ui.available_width() - 6.0;
            let r = ui.add_sized(
                vec2(w, 26.0),
                egui::TextEdit::singleline(&mut self.search_q)
                    .hint_text(t("在工作区中搜索"))
                    .margin(vec2(6.0, 4.0))
                    .text_color(if self.search_err.is_some() { ERR() } else { TEXT() }),
            );
            if self.focus_search {
                r.request_focus();
                self.focus_search = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.run_search();
            }
        });
        if self.search_replace {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                let w = ui.available_width() - 6.0;
                let hint = if self.search_regex { t("替换（$1 引用分组）") } else { t("替换") };
                if ui
                    .add_sized(vec2(w, 26.0), egui::TextEdit::singleline(&mut self.search_repl).hint_text(hint).margin(vec2(6.0, 4.0)))
                    .changed()
                {
                    self.replace_confirm = None;
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            if small_toggle(ui, &mut self.search_case, "Aa", t("区分大小写"))
                | small_toggle(ui, &mut self.search_word, "ab", t("全字匹配"))
                | small_toggle(ui, &mut self.search_regex, ".*", t("正则表达式"))
            {
                self.run_search();
            }
            if self.search_replace && !self.results.is_empty() && self.search_rx.is_none() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    // 改磁盘上的文件，要点两次：第一次变成“确认”
                    let confirming = self.replace_confirm.is_some_and(|t0| now - t0 < 4.0);
                    let files: HashSet<&PathBuf> = self.results.iter().map(|h| &h.path).collect();
                    let (label, fill, fg) = if confirming {
                        (tf("确认替换 {} 个文件", &[&files.len()]), WARN(), ON_ACCENT())
                    } else {
                        (t("全部替换").to_string(), BG3(), TEXT2())
                    };
                    let b = egui::Button::new(RichText::new(label).size(12.0).color(fg))
                        .fill(fill)
                        .corner_radius(R_WIDGET)
                        .min_size(vec2(0.0, 24.0));
                    if ui.add(b).on_hover_text(t("未打开的文件会直接写回磁盘；已打开的文件在编辑器里修改，可撤销")).clicked() {
                        if confirming {
                            self.replace_confirm = None;
                            self.replace_in_files(now);
                        } else {
                            self.replace_confirm = Some(now);
                        }
                    }
                    if confirming {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    }
                });
            }
        });
        ui.add_space(6.0);
        if let Some(e) = &self.search_err {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(t("正则有误")).small().color(ERR())).on_hover_text(e.as_str());
            });
        }
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
        let preview = (self.search_replace && !self.search_repl.is_empty()).then(|| self.search_repl.clone());
        let pat = self.search_pat();
        let re = pat.compile().ok();
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
                let (a, e) = h.m;
                let plain = |c| egui::TextFormat::simple(font.clone(), c);
                job.append(&txt[..a], 0.0, plain(TEXT2()));
                if let Some(new) = &preview {
                    // 替换预览：旧的划掉，新的标绿
                    let nw = if self.search_regex {
                        re.as_ref().map(|re| pat.replacement(re, txt, (a, e), new)).unwrap_or_else(|| new.clone())
                    } else {
                        new.clone()
                    };
                    job.append(
                        &txt[a..e],
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: ERR(),
                            background: ERR().gamma_multiply(0.12),
                            strikethrough: st(1.0, ERR()),
                            ..Default::default()
                        },
                    );
                    job.append(
                        &nw,
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: OK(),
                            background: OK().gamma_multiply(0.14),
                            ..Default::default()
                        },
                    );
                } else {
                    job.append(
                        &txt[a..e],
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: TEXT(),
                            background: WARN().gamma_multiply(0.35),
                            ..Default::default()
                        },
                    );
                }
                job.append(&txt[e..], 0.0, plain(TEXT2()));
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

    fn scan_todos(&mut self) {
        let (Some(root), Some(idx)) = (self.root.clone(), self.index.clone()) else { return };
        self.todo_for = Some(Arc::as_ptr(&idx) as usize);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let re = regex_lite::Regex::new(TODO_RE).expect("todo regex");
            let never = std::sync::atomic::AtomicBool::new(false);
            let _ = tx.send(cindex::search(&root, &idx.files, &re, &never, 3000));
        });
        self.todo_rx = Some(rx);
    }

    fn todo_panel(&mut self, ui: &mut Ui) {
        if let Some(rx) = &self.todo_rx {
            if let Ok(v) = rx.try_recv() {
                self.todos = v;
                self.todo_rx = None;
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        // 索引重建过（打开工程、保存、刷新）就重新扫
        let cur = self.index.as_ref().map(|i| Arc::as_ptr(i) as usize);
        if cur.is_some() && cur != self.todo_for && self.todo_rx.is_none() {
            self.scan_todos();
        }
        if self.root.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(t("打开文件夹后列出代码里的 TODO、FIXME 等标记")).small().color(TEXT3()));
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.add_sized(
                vec2(ui.available_width() - 10.0, 24.0),
                egui::TextEdit::singleline(&mut self.todo_filter).hint_text(t("筛选…")).margin(vec2(6.0, 3.0)),
            );
        });
        ui.add_space(4.0);
        let fl = self.todo_filter.to_lowercase();
        let shown: Vec<&Hit> = self
            .todos
            .iter()
            .filter(|h| fl.is_empty() || h.text.to_lowercase().contains(&fl) || h.path.to_string_lossy().to_lowercase().contains(&fl))
            .collect();
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let msg = if self.todo_rx.is_some() {
                t("正在扫描…").to_string()
            } else {
                tf("{} 条", &[&shown.len()])
            };
            ui.label(RichText::new(msg).small().color(TEXT3()));
        });
        ui.add_space(2.0);
        let root = self.root.clone().unwrap_or_default();
        let mut open: Option<(PathBuf, usize, (usize, usize))> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut last: Option<&PathBuf> = None;
            for h in shown {
                if last != Some(&h.path) {
                    last = Some(&h.path);
                    let name = h.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let dir = h.path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
                    let p = ui.painter();
                    file_badge(p, pos2(r.left() + 18.0, r.center().y), &name);
                    let g = p.layout_no_wrap(name, FontId::proportional(12.5), TEXT());
                    let w = g.size().x;
                    p.galley(pos2(r.left() + 30.0, r.center().y - g.size().y / 2.0), g, TEXT());
                    p.text(pos2(r.left() + 36.0 + w, r.center().y), Align2::LEFT_CENTER, dir, FontId::proportional(11.0), TEXT3());
                }
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
                let p = ui.painter_at(r);
                if resp.hovered() {
                    p.rect_filled(r, 0.0, tint(8));
                }
                let tag = &h.text[h.m.0..h.m.1];
                let c = todo_color(tag);
                let pill = Rect::from_min_size(pos2(r.left() + 22.0, r.center().y - 8.0), vec2(42.0, 16.0));
                p.rect_filled(pill, 4.0, c.gamma_multiply(0.16));
                p.text(pill.center(), Align2::CENTER_CENTER, tag, FontId::new(10.0, bold()), c);
                let rest = h.text[h.m.1..].trim_start_matches([':', ' ', '(', ')']).trim_end_matches("*/").trim();
                let rest = if rest.is_empty() { h.text.as_str() } else { rest };
                p.text(pos2(pill.right() + 8.0, r.center().y), Align2::LEFT_CENTER, rest, FontId::proportional(12.0), TEXT2());
                if resp.on_hover_text(format!("{}:{}  {}", h.path.display(), h.line + 1, h.text)).clicked() {
                    open = Some((root.join(&h.path), h.line as usize, h.m));
                }
            }
        });
        if let Some((p, l, _)) = open {
            self.open_file(&p, None);
            self.goto_line(l);
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

    /// 分屏开 / 关。另一侧显示最近用过的另一个文件
    fn toggle_split(&mut self) {
        if self.split.take().is_some() {
            return;
        }
        let Some(cur) = self.tabs.get(self.active).map(|d| d.path.clone()) else { return };
        let other = self
            .back
            .iter()
            .rev()
            .map(|(p, _)| p)
            .find(|p| **p != cur && self.tabs.iter().any(|d| &d.path == *p))
            .cloned()
            .or_else(|| self.tabs.iter().map(|d| d.path.clone()).find(|p| *p != cur));
        match other {
            Some(other) => self.split = Some(Split { other, active_left: false, last: cur }),
            None => self.toast(t("再打开一个文件才能分屏"), WARN()),
        }
    }

    /// 每帧整理分屏状态：另一侧的文件被关了就退出分屏；激活了另一侧的文件就把焦点移过去
    fn sync_split(&mut self) {
        let Some(cur) = self.tabs.get(self.active).map(|d| d.path.clone()) else {
            self.split = None;
            return;
        };
        let Some(sp) = self.split.as_mut() else { return };
        if cur == sp.other {
            sp.other = std::mem::replace(&mut sp.last, cur.clone());
            sp.active_left = !sp.active_left;
        }
        if !self.tabs.iter().any(|d| d.path == sp.other) || sp.other == cur {
            self.split = None;
            return;
        }
        sp.last = cur;
    }

    fn split_editors(&mut self, ui: &mut Ui, oi: usize) {
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let ratio = self.s.split_ratio.clamp(0.2, 0.8);
        let mid = (full.left() + full.width() * ratio).round();
        let left = Rect::from_min_max(full.min, pos2(mid, full.bottom()));
        let right = Rect::from_min_max(pos2(mid + 1.0, full.top()), full.max);
        let active_left = self.split.as_ref().is_some_and(|s| s.active_left);
        let (ar, or) = if active_left { (left, right) } else { (right, left) };
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(ar));
        self.editor(&mut c);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(or));
        if self.side_editor(&mut c, oi) {
            self.active = oi;
        }
        // 当前一侧顶上一条细的强调色，分清键盘在哪边
        ui.painter().hline(ar.x_range(), ar.top() + 0.5, st(1.5, ACCENT().gamma_multiply(0.7)));
        // 中间的分隔线，可以拖动
        let handle = Rect::from_center_size(pos2(mid + 0.5, full.center().y), vec2(6.0, full.height()));
        let resp = ui.interact(handle, ui.id().with("split-handle"), Sense::drag());
        let hot = resp.hovered() || resp.dragged();
        ui.painter().vline(mid + 0.5, full.y_range(), st(1.0, if hot { ACCENT() } else { LINE() }));
        if hot {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
            self.s.split_ratio = ((p.x - full.left()) / full.width()).clamp(0.2, 0.8);
        }
    }

    /// 分屏另一侧：只画编辑器（可以编辑），点进去就成为当前一侧。返回是否获得了焦点
    fn side_editor(&mut self, ui: &mut Ui, i: usize) -> bool {
        let rect = ui.available_rect_before_wrap();
        let status_h = 24.0;
        let area = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - status_h));
        let now = ui.input(|i| i.time);
        let sem = self.sem.clone();
        let (font_size, minimap) = (self.s.font_size, self.s.minimap);
        let vim_on = self.s.vim;
        let marks = self.mark_colors();
        let inactive = self.inactive_of(i);
        let d = &mut self.tabs[i];
        let view = View {
            lang: d.lang,
            font_size,
            minimap,
            find: &[],
            find_cur: None,
            sem: sem.as_deref().filter(|_| d.lang == Lang::C || d.lang == Lang::Asm),
            git: &d.git_lines,
            big: d.buf.text.len() > 1_500_000,
            ghost: None,
            vim: vim_on,
            marks: &marks,
            inactive: &inactive,
            scope: None,
            if_block: None,
        };
        let out = d.ed.show(ui, area, &mut d.buf, &view);
        if out.changed {
            d.syms_stale = Some(now);
            d.edited_at = Some(now);
        }
        // 底部一条和主编辑器等高的文件名栏，两边状态栏对齐
        let sr = Rect::from_min_max(pos2(rect.left(), area.bottom()), rect.max);
        let p = ui.painter();
        p.rect_filled(sr, 0.0, BG1());
        p.hline(sr.x_range(), sr.top() + 0.5, st(1.0, LINE()));
        let name = format!("{}{}", d.rel, if d.dirty() { "  ●" } else { "" });
        p.text(pos2(sr.left() + 12.0, sr.center().y), Align2::LEFT_CENTER, name, FontId::proportional(11.5), TEXT3());
        out.focused
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
        ui.painter().rect_filled(bar, 0.0, BG1());
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, st(1.0, LINE()));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar));
        let mut close = None;
        let mut activate = None;
        let mut rects: Vec<Rect> = vec![];
        let mut drag_start = None;
        let mut menu: Option<(usize, u8)> = None;
        let mut cmp: Option<(usize, Option<usize>)> = None;
        let mut cmp_saved: Option<usize> = None;
        let mut cmp_head: Option<usize> = None;
        let others: Vec<(String, bool)> = self.tabs.iter().map(|d| (d.rel.clone(), d.virt)).collect();
        let split_other = self.split.as_ref().map(|s| s.other.clone());
        egui::ScrollArea::horizontal()
            .id_salt("code-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, d) in self.tabs.iter().enumerate() {
                        let name = if d.virt {
                            d.rel.clone()
                        } else {
                            d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                        };
                        let g = ui.painter().layout_no_wrap(name.clone(), FontId::proportional(12.5), TEXT());
                        let w = g.size().x + 64.0;
                        let (r, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click_and_drag());
                        rects.push(r);
                        if resp.drag_started() {
                            drag_start = Some(i);
                        }
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
                        if split_other.as_ref() == Some(&d.path) {
                            // 分屏另一侧正在显示
                            p.hline(r.x_range().shrink(10.0), r.top() + 1.0, st(1.5, TEXT3()));
                        }
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
                        resp.on_hover_text(&d.rel).context_menu(|ui| {
                            ui.set_min_width(180.0);
                            for (k, label) in [
                                (0u8, t("关闭")),
                                (1, t("关闭其他")),
                                (2, t("关闭右侧")),
                                (3, t("关闭已保存的")),
                                (4, t("向右拆分")),
                                (5, t("复制路径")),
                                (6, t("在文件管理器中显示")),
                            ] {
                                if k == 4 || k == 5 {
                                    ui.separator();
                                }
                                if ui.button(label).clicked() {
                                    menu = Some((i, k));
                                    ui.close_menu();
                                }
                                if k == 4 && !d.virt {
                                    ui.menu_button(t("与…比较"), |ui| {
                                        if d.dirty() && ui.button(t("已保存的版本")).clicked() {
                                            cmp_saved = Some(i);
                                            ui.close_menu();
                                        }
                                        if ui.button("Git HEAD").clicked() {
                                            cmp_head = Some(i);
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        for (j, o) in others.iter().enumerate() {
                                            if j != i && !o.1 {
                                                if ui.button(&o.0).clicked() {
                                                    cmp = Some((i, Some(j)));
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                        if others.len() > 1 {
                                            ui.separator();
                                        }
                                        if ui.button(t("选择文件…")).clicked() {
                                            cmp = Some((i, None));
                                            ui.close_menu();
                                        }
                                    });
                                }
                            }
                        });
                    }
                });
            });
        // 拖动标签换位置：松手时按指针位置落到对应的空位
        if let Some(i) = drag_start {
            self.tab_drag = Some(i);
        }
        if let Some(from) = self.tab_drag {
            let ptr = ui.input(|i| i.pointer.interact_pos());
            if let (Some(p), Some(r)) = (ptr, rects.get(from)) {
                let to = rects.iter().position(|r| p.x < r.center().x).unwrap_or(rects.len()).min(rects.len());
                let to = if to > from { to - 1 } else { to };
                // 落点指示线
                let x = if to >= from { rects.get(to).map_or(r.right(), |r| r.right()) } else { rects[to].left() };
                ui.painter().vline(x, bar.y_range(), st(2.0, ACCENT()));
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                if !ui.input(|i| i.pointer.any_down()) {
                    self.tab_drag = None;
                    if to != from {
                        let cur = self.tabs[self.active].path.clone();
                        let d = self.tabs.remove(from);
                        self.tabs.insert(to, d);
                        self.active = self.tabs.iter().position(|d| d.path == cur).unwrap_or(0);
                        self.sync_tabs();
                    }
                }
            } else if !ui.input(|i| i.pointer.any_down()) {
                self.tab_drag = None;
            }
        }
        if let Some(i) = activate {
            self.active = i;
            self.hover = None;
        }
        if let Some(i) = cmp_saved {
            self.compare_saved(i);
        }
        if let Some(i) = cmp_head {
            self.compare_head(i);
        }
        if let Some((i, j)) = cmp {
            let a = self.tabs[i].path.clone();
            let b = match j {
                Some(j) => Some(self.tabs[j].path.clone()),
                None => rfd::FileDialog::new().set_title(t("选择要比较的文件")).pick_file(),
            };
            if let Some(b) = b {
                self.compare(&a, &b);
            }
        }
        if let Some((i, k)) = menu {
            match k {
                0 => close = Some(i),
                1 | 2 | 3 => {
                    let keep = self.tabs[i].path.clone();
                    let paths: Vec<(usize, PathBuf, bool)> = self.tabs.iter().enumerate().map(|(j, d)| (j, d.path.clone(), d.dirty())).collect();
                    for (j, p, dirty) in paths.into_iter().rev() {
                        let drop = match k {
                            1 => p != keep && !dirty,
                            2 => j > i && !dirty,
                            _ => !dirty,
                        };
                        if drop {
                            if let Some(pos) = self.tabs.iter().position(|d| d.path == p) {
                                self.close_tab(pos);
                            }
                        }
                    }
                }
                4 => {
                    // 当前文件放到右边
                    if i != self.active {
                        let other = self.tabs[self.active].path.clone();
                        self.active = i;
                        self.split = Some(Split {
                            other,
                            active_left: false,
                            last: self.tabs[i].path.clone(),
                        });
                    } else {
                        self.split = None;
                        self.toggle_split();
                    }
                }
                5 => ui.ctx().copy_text(self.tabs[i].path.to_string_lossy().to_string()),
                _ => reveal(&self.tabs[i].path),
            }
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
        let split_on = self.split.is_some();
        let sp = btn(Icon::Split, t("拆分编辑器"), split_on, true);
        let rel = btn(Icon::Relations, t("调用关系"), rel_on, true);
        let cx = btn(Icon::Context, t("上下文窗口"), ctx_on, true);
        let f = btn(Icon::Forward, t("前进  Alt+→"), false, nf);
        let b = btn(Icon::Back, t("后退  Alt+←"), false, nb);
        // 特殊查看器和原文之间切换
        let toggle = match self.tabs.get(self.active).map(|d| (&d.special, d.buf.text.is_empty(), d.show_special)) {
            Some((Some(sp), false, show)) => Some(if show { t("原文") } else { sp.label() }),
            _ => None,
        };
        if let Some(label) = toggle {
            let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(11.5), ACCENT());
            let br = Rect::from_min_max(pos2(bx - g.size().x - 20.0, r.center().y - 10.0), pos2(bx - 4.0, r.center().y + 10.0));
            let resp = ui.interact(br, ui.id().with("bc-special"), Sense::click());
            ui.painter()
                .rect_filled(br, 10.0, if resp.hovered() { ACCENT().gamma_multiply(0.22) } else { ACCENT().gamma_multiply(0.12) });
            ui.painter().galley(br.center() - g.size() / 2.0, g, ACCENT());
            if resp.on_hover_text(t("切换显示方式")).clicked() {
                if let Some(d) = self.tabs.get_mut(self.active) {
                    d.show_special = !d.show_special;
                }
            }
        }
        if sp {
            self.toggle_split();
        }
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
        if self.tabs[active].show_special {
            if let Some(sp) = self.tabs[active].special.as_mut() {
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(full.shrink2(vec2(14.0, 0.0))));
                let evt = sp.ui(&mut c);
                self.handle_bin_evt(evt, ui.input(|i| i.pointer.hover_pos()).unwrap_or(full.center()), now);
                return;
            }
        }
        let marks = self.mark_colors();
        let inactive = self.inactive_of(active);
        let (scope, if_block) = self.scopes_of(active);
        let d = &mut self.tabs[active];
        d.ed.doc_hook = d.lang == Lang::C;

        // 待跳转：设置光标/选区，并滚到那一行
        if let Some((line, sel)) = d.goto.take() {
            let (a, b) = sel.unwrap_or_else(|| {
                let s = d.buf.start(line);
                (s, s)
            });
            d.ed.select(a, b);
            d.ed.focus_req = true;
        }
        // 输入片段前缀后按 Tab 展开
        if d.ed.sels.len() == 1 && d.ed.primary_sel().empty() && ui.memory(|m| m.has_focus(d.ed.id)) && ui.input(|i| i.key_pressed(Key::Tab) && i.modifiers.is_none()) {
            let h = d.ed.head();
            let ls = d.buf.start(d.buf.line_of(h));
            let n: usize = d.buf.text[ls..h].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
            let w = &d.buf.text[h - n..h];
            if let Some(sn) = self.snippets.iter().find(|s| !w.is_empty() && s.prefix == w && snippets::applies(s, d.lang)).cloned() {
                ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
                insert_snippet(d, h - n..h, &sn, now);
            }
        }
        let ghost = if self.s.inline_blame && d.ed.sels.len() == 1 && ui.memory(|m| m.has_focus(d.ed.id)) {
            Self::blame_text(d)
        } else {
            None
        };
        let (hits, find_cur) = match &self.find {
            Some(f) => (f.hits.clone(), Some(f.cur)),
            None => (vec![], None),
        };
        let view = View {
            lang: d.lang,
            font_size: self.s.font_size,
            minimap: self.s.minimap,
            find: &hits,
            find_cur,
            sem: sem.as_deref().filter(|_| d.lang == Lang::C || d.lang == Lang::Asm),
            git: &d.git_lines,
            big: d.buf.text.len() > 1_500_000,
            ghost: ghost.as_deref().map(|g| (d.cursor.0, g)),
            vim: self.s.vim,
            marks: &marks,
            inactive: &inactive,
            scope,
            if_block,
        };
        let out = d.ed.show(ui, area, &mut d.buf, &view);
        if out.changed {
            d.syms_stale = Some(now);
            d.edited_at = Some(now);
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
        if let Some(c) = out.vim_cmd.clone() {
            self.vim_command(&c, now);
        }

        // ---- 悬浮 / Ctrl+点击
        let mut want_def: Option<(String, egui::Pos2, Option<usize>)> = None;
        let d = &self.tabs[active];
        if let Some((b, at)) = out.ctrl_click {
            if let Some((_, _, w)) = d.word_at(b) {
                want_def = Some((w.to_string(), at, Some(b)));
            } else if let Some((s, e)) = d.buf.token_at(b).filter(|(s, _)| d.buf.text[*s..].starts_with(|c: char| c.is_ascii_digit())) {
                let lit = d.buf.text[s..e].to_string();
                let v = cindex::eval(&lit);
                self.events.push(CodeEvt::BitCalc(lit, v));
            }
        }
        if let Some((s, e, r)) = out.hover.filter(|_| !self.gen.menu_open) {
            let w = d.buf.text[s..e].to_string();
            match &self.hover {
                Some(h) if h.at == s && h.word == w => {}
                _ => self.hover = Some(Hover { word: w, at: s, since: now, rect: r }),
            }
        } else {
            self.hover = None;
        }
        let mut hover_def: Option<(String, egui::Pos2, Option<usize>)> = None;
        let num = self.hover.as_ref().filter(|h| h.word.starts_with(|c: char| c.is_ascii_digit()));
        if let Some(h) = num {
            if now - h.since > 0.35 && !ui.input(|i| i.pointer.any_down()) {
                if let Some(v) = cindex::eval(&h.word) {
                    egui::show_tooltip_at(
                        ui.ctx(),
                        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hover-layer")),
                        egui::Id::new(("num-hover", h.at)),
                        h.rect.left_bottom() + vec2(0.0, 4.0),
                        |ui| {
                            crate::tools::number_card(ui, v);
                            ui.add_space(2.0);
                            ui.label(RichText::new(t("Ctrl+点击 在位域计算器中打开")).small().color(ACCENT().gamma_multiply(0.8)));
                        },
                    );
                }
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
        } else if let Some(h) = &self.hover {
            let reg = self.svd.as_ref().filter(|_| lang.is_code()).and_then(|dev| {
                let before = &self.tabs[active].buf.text[..h.at];
                let b = before.trim_end();
                let b = b.strip_suffix("->").or_else(|| b.strip_suffix('.'))?.trim_end();
                let start = b.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |p| p + 1);
                let (p, r) = dev.find(&b[start..], &h.word)?;
                Some((p.clone(), r.clone()))
            });
            if let Some((p, r)) = reg {
                if now - h.since > 0.35 && !ui.input(|i| i.pointer.any_down()) {
                    egui::show_tooltip_at(
                        ui.ctx(),
                        egui::LayerId::new(egui::Order::Foreground, egui::Id::new("hover-layer")),
                        egui::Id::new(("svd-hover", h.at)),
                        h.rect.left_bottom() + vec2(0.0, 4.0),
                        |ui| svd_card(ui, &p, &r),
                    );
                } else {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
                }
            } else if lang.is_code() && now - h.since > 0.35 && !ui.input(|i| i.pointer.any_down() || i.modifiers.command) {
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
        if let Some(vm) = &d.ed.vim {
            let insert = vm.mode == crate::vim::Mode::Insert;
            items.insert(0, (vm.status(), if insert { OK() } else { ACCENT() }));
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
        self.track_position(out.changed);
        if let Some(l) = out.doc_comment {
            self.doc_comment_at(l, now);
        }
        // 右键菜单
        if let Some(resp) = out.menu {
            let labels = std::mem::take(&mut self.key_labels);
            let lab = |a: Act| labels.get(&a).cloned().unwrap_or_default();
            let mut picked = None;
            let mut shown = false;
            resp.context_menu(|ui| {
                shown = true;
                picked = self.editor_menu(ui, &lab);
            });
            self.gen.menu_open = shown;
            self.key_labels = labels;
            if let Some(a) = picked {
                self.shortcuts(ui.ctx(), &[a]);
            }
        }
    }

    fn find_bar(&mut self, ui: &mut Ui, text_rect: Rect) {
        let now = ui.input(|i| i.time);
        let preview = self.find_preview();
        let Some(f) = self.find.as_mut() else { return };
        let w = 470.0;
        let pos = pos2(text_rect.right() - w - 20.0, text_rect.top() + 6.0);
        let mut changed = false;
        let mut step: Option<bool> = None;
        let mut close = false;
        let mut do_replace: Option<bool> = None;
        let mut to_editor = false;
        let vim = self.s.vim;
        egui::Area::new(egui::Id::new("code-find")).fixed_pos(pos).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
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
                .inner_margin(egui::Margin::symmetric(6, 5))
                .show(ui, |ui| {
                    ui.set_width(w);
                    ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
                    ui.horizontal(|ui| {
                        // 展开 / 收起替换行
                        let (r, resp) = ui.allocate_exact_size(vec2(18.0, 24.0), Sense::click());
                        let c = if resp.hovered() { TEXT() } else { TEXT3() };
                        let m = r.center();
                        let pts = if f.replace {
                            vec![m + vec2(-3.5, -1.5), m + vec2(0.0, 2.0), m + vec2(3.5, -1.5)]
                        } else {
                            vec![m + vec2(-1.5, -3.5), m + vec2(2.0, 0.0), m + vec2(-1.5, 3.5)]
                        };
                        ui.painter().add(Shape::line(pts, st(1.3, c)));
                        if resp.on_hover_text(t("切换替换  Ctrl+H")).clicked() {
                            f.replace = !f.replace;
                            f.focus_repl = f.replace;
                        }
                        ui.add_space(2.0);
                        let te = ui.add_sized(
                            vec2(200.0, 24.0),
                            egui::TextEdit::singleline(&mut f.pat.q)
                                .hint_text(t("查找"))
                                .margin(vec2(6.0, 3.0))
                                .text_color(if f.err.is_some() { ERR() } else { TEXT() }),
                        );
                        if f.focus {
                            te.request_focus();
                            f.focus = false;
                        }
                        changed |= te.changed();
                        if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            if vim {
                                // Vim 的 /：回车就停在当前匹配，回到编辑器，之后用 n / N 跳
                                to_editor = true;
                            } else {
                                step = Some(ui.input(|i| i.modifiers.shift));
                                te.request_focus();
                            }
                        }
                        ui.add_space(2.0);
                        changed |= small_toggle(ui, &mut f.pat.case, "Aa", t("区分大小写"));
                        changed |= small_toggle(ui, &mut f.pat.word, "ab", t("全字匹配"));
                        changed |= small_toggle(ui, &mut f.pat.regex, ".*", t("正则表达式"));
                        ui.add_space(4.0);
                        let n = f.hits.len();
                        let (label, col) = if let Some(e) = &f.err {
                            let _ = e;
                            (t("正则有误").to_string(), ERR())
                        } else if f.pat.q.is_empty() {
                            (String::new(), TEXT2())
                        } else if n == 0 {
                            (t("无结果").to_string(), ERR())
                        } else {
                            (format!("{}/{}", f.cur + 1, n), TEXT2())
                        };
                        let lr = ui.add_sized(vec2(64.0, 24.0), egui::Label::new(RichText::new(label).small().color(col)).truncate());
                        if let Some(e) = &f.err {
                            lr.on_hover_text(e.as_str());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    if f.replace {
                        ui.horizontal(|ui| {
                            ui.add_space(20.0);
                            let te = ui.add_sized(
                                vec2(200.0, 24.0),
                                egui::TextEdit::singleline(&mut f.repl)
                                    .hint_text(if f.pat.regex { t("替换（$1 引用分组）") } else { t("替换") })
                                    .margin(vec2(6.0, 3.0)),
                            );
                            if f.focus_repl {
                                te.request_focus();
                                f.focus_repl = false;
                            }
                            if te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                do_replace = Some(ui.input(|i| i.modifiers.command));
                                te.request_focus();
                            }
                            ui.add_space(4.0);
                            let has = !f.hits.is_empty();
                            if ui.add_enabled_ui(has, |ui| icon_button(ui, Icon::Replace, t("替换当前  Enter"), false)).inner.clicked() {
                                do_replace = Some(false);
                            }
                            let b = egui::Button::new(RichText::new(t("全部替换")).size(12.0).color(TEXT2()))
                                .fill(BG3())
                                .corner_radius(R_WIDGET)
                                .min_size(vec2(0.0, 24.0));
                            if ui.add_enabled(has, b).on_hover_text(t("Ctrl+Enter；可以 Ctrl+Z 一次撤销")).clicked() {
                                do_replace = Some(true);
                            }
                        });
                        // 预览：当前项替换前后
                        if let Some((old, new)) = &preview {
                            ui.horizontal(|ui| {
                                ui.add_space(22.0);
                                let mono = FontId::new(11.5, FontFamily::Monospace);
                                ui.label(RichText::new(old).font(mono.clone()).color(ERR()).strikethrough());
                                ui.label(RichText::new("→").size(11.0).color(TEXT3()));
                                ui.label(RichText::new(new).font(mono).color(OK()));
                            });
                        }
                    }
                });
        });
        if close {
            self.find = None;
            return;
        }
        if changed {
            self.update_find();
            if let Some(f) = &self.find {
                if let Some(&(h, e)) = f.hits.get(f.cur) {
                    if let Some(d) = self.tabs.get_mut(self.active) {
                        d.ed.select(h, e);
                    }
                }
            }
        }
        if to_editor {
            if let Some(d) = self.tabs.get_mut(self.active) {
                let a = d.ed.primary_sel().min();
                d.ed.select(a, a);
                d.ed.focus_req = true;
            }
        }
        if let Some(b) = step {
            self.find_step(b);
            if let Some(f) = self.find.as_mut() {
                f.focus = true;
            }
        }
        if let Some(all) = do_replace {
            let n = self.replace(all, now);
            if all && n > 0 {
                self.toast(tf("已替换 {} 处", &[&n]), OK());
            }
        }
    }

    /// 当前匹配替换前后的文字（替换行打开、有替换内容时）
    fn find_preview(&self) -> Option<(String, String)> {
        let f = self.find.as_ref()?;
        let d = self.tabs.get(self.active)?;
        if !f.replace || f.repl.is_empty() {
            return None;
        }
        let &(a, b) = f.hits.get(f.cur)?;
        let re = f.pat.compile().ok()?;
        let new = f.pat.replacement(&re, &d.buf.text, (a, b), &f.repl);
        let clip = |s: &str| {
            let s = s.replace('\n', "⏎");
            if s.chars().count() > 40 {
                format!("{}…", s.chars().take(40).collect::<String>())
            } else {
                s
            }
        };
        Some((clip(&d.buf.text[a..b]), clip(&new)))
    }

    // ------------------------------------------------------------ 快速打开 / 定义选择

    fn quick_open(&mut self, ctx: &egui::Context) {
        let Some(qk) = self.quick.as_mut() else { return };
        let mode = match qk.q.chars().next() {
            Some('@') => QuickMode::FileSymbols,
            Some('#') => QuickMode::Symbols,
            Some(':') => QuickMode::Line,
            Some('!') => QuickMode::Snippets,
            Some('<') => QuickMode::History,
            _ => QuickMode::Files,
        };
        let q = qk.q.trim_start_matches(['@', '#', ':', '!', '<']).trim().to_string();
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
            QuickMode::Snippets => {
                if let Some(d) = self.tabs.get(self.active) {
                    let ql = q.to_lowercase();
                    for (i, s) in self.snippets.iter().enumerate() {
                        if snippets::applies(s, d.lang) && (ql.is_empty() || s.prefix.to_lowercase().contains(&ql) || s.name.to_lowercase().contains(&ql)) {
                            items.push((s.prefix.clone(), s.name.clone(), None, PathBuf::new(), Some(i as u32)));
                        }
                    }
                }
            }
            QuickMode::History => {
                // 后退栈（最近的在前）+ 前进栈
                let ql = q.to_lowercase();
                let mut seen = HashSet::new();
                for (p, l) in self.back.iter().rev().chain(self.fwd.iter().rev()) {
                    if !seen.insert((p.clone(), *l)) {
                        continue;
                    }
                    let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().replace('\\', "/");
                    let text = self
                        .tabs
                        .iter()
                        .find(|d| d.path == *p)
                        .map(|d| d.buf.line((*l).min(d.buf.lines().saturating_sub(1))).trim().to_string())
                        .unwrap_or_default();
                    let main = format!("{}:{}", rel, l + 1);
                    if !ql.is_empty() && !main.to_lowercase().contains(&ql) && !text.to_lowercase().contains(&ql) {
                        continue;
                    }
                    let text: String = text.chars().take(80).collect();
                    items.push((main, text, None, p.clone(), Some(*l as u32)));
                    if items.len() >= 60 {
                        break;
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
                            QuickMode::Snippets => t("搜索代码片段（输入前缀后按 Tab 也能展开）"),
                            QuickMode::History => t("导航历史：最近跳转过的位置"),
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
                            let msg = if self.index.is_none() && !matches!(mode, QuickMode::Line | QuickMode::FileSymbols | QuickMode::History) {
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
                                    None if mode == QuickMode::Snippets => {
                                        p.text(pos2(r.left() + 16.0, r.center().y), Align2::CENTER_CENTER, "{ }", FontId::monospace(11.0), VIOLET());
                                    }
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
            if mode == QuickMode::Snippets {
                let now = ctx.input(|i| i.time);
                if let (Some(sn), Some(d)) = (line.and_then(|i| self.snippets.get(i as usize)).cloned(), self.tabs.get_mut(self.active)) {
                    let s = d.ed.primary_sel();
                    insert_snippet(d, s.min()..s.max(), &sn, now);
                    d.ed.focus_req = true;
                }
                return;
            }
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
/// 悬停寄存器时的卡片：地址、复位值、说明和位域
fn svd_card(ui: &mut Ui, p: &crate::toolbox::svd::Periph, r: &crate::toolbox::svd::Reg) {
    ui.set_max_width(560.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{}->{}", p.name, r.name)).font(FontId::new(13.0, FontFamily::Monospace)).color(TEXT()).strong());
        ui.label(RichText::new(format!("0x{:08X}", p.base + r.offset)).font(FontId::new(12.5, FontFamily::Monospace)).color(ACCENT()));
        ui.label(RichText::new(&r.access).small().color(TEXT3()));
    });
    if !r.desc.is_empty() {
        ui.label(RichText::new(&r.desc).small().color(TEXT2()));
    }
    ui.label(RichText::new(tf("复位值 0x{}", &[&format!("{:X}", r.reset)])).small().color(TEXT3()));
    if !r.fields.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("svd_hover_fields").num_columns(3).spacing([12.0, 1.0]).show(ui, |ui| {
            for f in r.fields.iter().take(24) {
                let bits = if f.width == 1 { f.lsb.to_string() } else { format!("{}:{}", f.lsb + f.width - 1, f.lsb) };
                ui.label(RichText::new(bits).font(FontId::new(11.0, FontFamily::Monospace)).color(TEXT3()));
                ui.label(RichText::new(&f.name).font(FontId::new(11.5, FontFamily::Monospace)).color(VIOLET()));
                ui.label(RichText::new(truncate(&f.desc, 60)).small().color(TEXT2()));
                ui.end_row();
            }
        });
        if r.fields.len() > 24 {
            ui.label(RichText::new(tf("等 {} 个位域", &[&r.fields.len()])).small().color(TEXT3()));
        }
    }
}

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

// ---------------------------------------------------------------- 崩溃恢复

fn recovery_dir() -> PathBuf {
    match crate::instance() {
        1 => crate::data_dir().join("recovery"),
        n => crate::data_dir().join(format!("recovery-{n}")),
    }
}

fn backup_file(path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    recovery_dir().join(format!("{:016x}.txt", h.finish()))
}

/// 备份格式：第一行是原文件路径，后面是内容
fn write_backup(path: &Path, text: &str) {
    let _ = std::fs::create_dir_all(recovery_dir());
    let _ = std::fs::write(backup_file(path), format!("{}\n{}", path.display(), text));
}

fn read_backup(bak: &Path) -> Option<String> {
    let s = std::fs::read_to_string(bak).ok()?;
    s.split_once('\n').map(|(_, t)| t.to_string())
}

fn drop_backup(d: &mut Doc) {
    if d.backup_ver.take().is_some() {
        let _ = std::fs::remove_file(backup_file(&d.path));
    }
}

fn scan_recovery() -> Vec<(PathBuf, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(recovery_dir()) else { return vec![] };
    rd.flatten()
        .filter_map(|e| {
            let s = std::fs::read_to_string(e.path()).ok()?;
            let (p, _) = s.split_once('\n')?;
            Some((e.path(), PathBuf::from(p)))
        })
        .collect()
}

/// 一条横跨编辑区的提示条，返回点了第几个按钮
fn notice_bar(ui: &mut Ui, msg: &str, buttons: &[&str]) -> Option<usize> {
    let mut hit = None;
    egui::Frame::new().fill(BG2()).inner_margin(egui::Margin::symmetric(12, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").size(10.0).color(WARN()));
            ui.label(RichText::new(msg).size(12.5).color(TEXT()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (i, b) in buttons.iter().enumerate().rev() {
                    if ui.button(*b).clicked() {
                        hit = Some(i);
                    }
                }
            });
        });
    });
    let r = ui.min_rect();
    ui.painter().hline(r.x_range(), ui.cursor().top() - 0.5, st(1.0, LINE()));
    hit
}

/// 把片段展开到 r 这一段（通常是刚输入的前缀），选中第一个占位符
fn insert_snippet(d: &mut Doc, r: std::ops::Range<usize>, sn: &snippets::Snippet, now: f64) {
    let ls = d.buf.start(d.buf.line_of(r.start));
    let indent: String = d.buf.text[ls..r.start].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let unit = d.buf.indent_unit();
    let file = d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (text, (a, b)) = snippets::expand(&sn.body, &indent, &unit, &file);
    let start = r.start;
    d.ed.replace_ranges(&mut d.buf, vec![(r, text)], now);
    d.ed.select(start + a, start + b);
    d.edited_at = Some(now);
    d.syms_stale = Some(now);
}

const TODO_RE: &str = r"\b(TODO|FIXME|XXX|HACK|BUG|NOTE)\b";

fn todo_color(tag: &str) -> Color32 {
    match tag {
        "FIXME" | "BUG" | "XXX" => ERR(),
        "HACK" => VIOLET(),
        "NOTE" => ACCENT(),
        _ => WARN(),
    }
}

/// 固件 / 映像文件（按扩展名），这些文件允许更大，且默认用十六进制查看
fn is_firmware(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "bin" | "hex" | "ihex" | "ihx" | "s19" | "s28" | "s37" | "srec" | "mot" | "sx" | "img" | "dfu" | "fw" | "elf" | "axf" | "out"
    )
}

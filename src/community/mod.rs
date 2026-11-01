//! 社区页：需求投票 + 精简论坛（需求 / 求助 / 分享 / 公告），可匿名，也可以注册登录。
//!
//! 网络请求都在后台线程里做，结果经通道送回界面线程；投票、点赞先改界面再发请求，失败了再改回来。
//! 服务器接口见 qccom-hub 的 docs/API.md。

mod md;
mod ui;
mod widgets;

pub use widgets::chat_icon;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::hub::{self, Body, HubErr};
use crate::i18n::{t, tf};

// ---------------------------------------------------------------- 数据

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Author {
    pub name: String,
    pub avatar: Option<String>,
    pub user_id: Option<i64>,
    pub staff: bool,
    pub anonymous: bool,
    pub seed: String,
    pub me: bool,
}

impl Author {
    /// 头像图案的种子
    pub fn seed(&self) -> String {
        match self.user_id {
            Some(u) => format!("u{u}"),
            None if !self.seed.is_empty() => self.seed.clone(),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Quote {
    pub id: i64,
    pub floor: i64,
    pub excerpt: String,
    pub author: Option<Author>,
    pub deleted: bool,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Reply {
    pub id: i64,
    pub floor: i64,
    pub body: String,
    pub author: Author,
    pub quote: Option<Quote>,
    pub pending: bool,
    pub likes: i64,
    pub liked: bool,
    pub created_at: i64,
    pub edited_at: Option<i64>,
    pub can_edit: bool,
    pub can_delete: bool,
    pub deleted: bool,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Post {
    pub id: i64,
    pub board: String,
    pub title: String,
    pub excerpt: String,
    pub body: String,
    pub author: Author,
    pub status: String,
    pub done_version: String,
    pub admin_note: String,
    pub solved_reply: Option<i64>,
    pub pinned: bool,
    pub locked: bool,
    pub pending: bool,
    pub merged_into: Option<i64>,
    pub merged_title: Option<String>,
    pub score: i64,
    pub reply_count: i64,
    pub created_at: i64,
    pub edited_at: Option<i64>,
    pub last_reply_at: i64,
    pub voted: bool,
    pub can_edit: bool,
    pub can_delete: bool,
    pub replies: Vec<Reply>,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub is_admin: bool,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Notice {
    pub id: i64,
    pub title: String,
    pub created_at: i64,
}

#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Notif {
    pub id: i64,
    pub kind: String,
    pub post_id: i64,
    pub reply_id: Option<i64>,
    pub actor: String,
    pub extra: String,
    pub created_at: i64,
    pub read: bool,
    pub title: String,
}

// ---------------------------------------------------------------- 导航

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Board {
    Idea,
    Help,
    Share,
    Notice,
}

impl Board {
    pub const ALL: [Board; 4] = [Board::Idea, Board::Help, Board::Share, Board::Notice];
    pub fn id(self) -> &'static str {
        match self {
            Board::Idea => "idea",
            Board::Help => "help",
            Board::Share => "share",
            Board::Notice => "notice",
        }
    }
    pub fn from_id(s: &str) -> Board {
        match s {
            "help" => Board::Help,
            "share" => Board::Share,
            "notice" => Board::Notice,
            _ => Board::Idea,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Board::Idea => t("需求"),
            Board::Help => t("求助"),
            Board::Share => t("分享"),
            Board::Notice => t("公告"),
        }
    }
    pub fn desc(self) -> &'static str {
        match self {
            Board::Idea => t("想要的功能，大家投票，票多的先做"),
            Board::Help => t("使用中遇到的问题"),
            Board::Share => t("脚本、快捷命令、使用经验"),
            Board::Notice => t("作者发布的消息"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mine {
    Posted,
    Voted,
    Replied,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nav {
    Board(Board),
    Mine(Mine),
    Notifs,
}

/// 列表排序
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Hot,
    Top,
    New,
    Active,
}

impl Sort {
    pub fn id(self) -> &'static str {
        match self {
            Sort::Hot => "hot",
            Sort::Top => "top",
            Sort::New => "new",
            Sort::Active => "active",
        }
    }
}

/// 需求的状态筛选
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdeaFilter {
    /// 待评估 + 计划中
    Open,
    Planned,
    Done,
    All,
}

/// 右栏显示什么
#[derive(Clone, Debug, PartialEq)]
pub enum Pane {
    Empty,
    Post(i64),
    Compose,
    Account,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub board: String,
    pub title: String,
    pub body: String,
    pub anonymous: bool,
    /// 正在编辑哪条帖子（None = 新帖）
    pub edit: Option<i64>,
}

/// 后台任务的结果
pub enum Res {
    List(u64, Result<ListPage, HubErr>),
    /// 预取各板块第一页（切板块不闪），顺便拿到各板块条数
    Prefetch(Board, Result<ListPage, HubErr>),
    Detail(i64, Result<Post, HubErr>),
    Me(Result<Value, HubErr>),
    Similar(String, Vec<Post>),
    Notifs(Result<(Vec<Notif>, i64), HubErr>),
    Posted(Result<Post, HubErr>),
    Replied(i64, Result<Value, HubErr>),
    Voted(i64, bool, Result<Value, HubErr>),
    Liked(i64, bool, Result<Value, HubErr>),
    Auth(Result<Value, HubErr>),
    Img(String, Result<Vec<u8>, HubErr>),
    Uploaded(Target, Result<Value, HubErr>),
    /// 其它操作（删除、标记解决、认领、改昵称…）：成功提示语 + 之后刷新哪条帖子
    Done(String, Option<i64>, Result<Value, HubErr>),
}

/// 上传的图片插到哪里
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Target {
    Compose,
    Reply,
    Avatar,
}

pub enum ImgState {
    Loading,
    Ready(egui::TextureHandle, [usize; 2]),
    Failed,
}

/// 登录 / 注册表单
#[derive(Default)]
pub struct AuthForm {
    pub register: bool,
    pub username: String,
    pub password: String,
    pub nickname: String,
    pub busy: bool,
    pub error: String,
    /// 账号页：改昵称 / 改密码
    pub new_nick: String,
    pub old_pw: String,
    pub new_pw: String,
}

pub struct Community {
    pub nav: Nav,
    pub sort: HashMap<Board, Sort>,
    pub idea_filter: IdeaFilter,
    pub query: String,
    query_sent: String,
    query_at: Option<Instant>,
    pub items: Vec<Post>,
    pub total: i64,
    pub page: i64,
    pub loading: bool,
    pub list_err: Option<String>,
    /// 这一页里有几条解析不了（服务器和客户端版本不一致时）
    pub skipped: usize,
    pub skip_err: String,
    pub hidden_other: i64,
    /// 各板块条数（默认筛选下），导航里显示
    pub board_counts: HashMap<Board, i64>,
    list_seq: u64,
    /// 当前列表是否需要重新拉
    stale: bool,
    pub pane: Pane,
    compose_back: Option<Pane>,
    pub detail: Option<Post>,
    pub detail_loading: bool,
    pub detail_err: Option<String>,
    /// 列表里选中的帖子（键盘 J/K）
    pub sel: Option<usize>,
    pub draft: Draft,
    pub preview: bool,
    pub similar: Vec<Post>,
    similar_q: String,
    similar_at: Option<Instant>,
    pub posting: bool,
    pub reply: String,
    pub reply_quote: Option<(i64, i64, String)>,
    pub reply_anon: bool,
    pub replying: bool,
    /// 正在修改哪条回复（回复框里放的是它的内容）
    pub reply_edit: Option<i64>,
    /// 放大查看的图片
    pub lightbox: Option<String>,
    /// 作者（管理员）在详情页写的回复 / 完成版本（编辑中）
    pub admin_note: Option<(i64, String, String)>,
    pub uploading: Option<Target>,
    pub me_user: Option<User>,
    pub device_name: String,
    pub unread: i64,
    pub notice: Option<Notice>,
    pub claimable: i64,
    pub banned: bool,
    me_at: Option<Instant>,
    pub notifs: Vec<Notif>,
    pub notifs_loading: bool,
    pub auth: AuthForm,
    pub imgs: HashMap<String, ImgState>,
    pub toasts: Vec<(String, bool)>,
    /// 已看过的最新公告（状态栏只提示一次）
    pub seen_notice: i64,
    /// 打开过社区页（之后才开始定时拉提醒）
    pub active: bool,
    /// 删除确认：(帖子或回复 id, 是不是回复, 什么时候点的)
    pub confirm: Option<(i64, bool, Instant)>,
    /// 外部要求切到这个帖子（提醒、状态栏公告）
    pub scroll_top: bool,
    pub offline: bool,
    tx: Sender<Res>,
    rx: Receiver<Res>,
    ctx: Option<egui::Context>,
    /// 列表缓存：离线时显示上次的内容
    cache: HashMap<String, Vec<Post>>,
}

fn draft_path() -> PathBuf {
    crate::data_dir().join("community-draft.json")
}

fn cache_path() -> PathBuf {
    crate::data_dir().join("community-cache.json")
}

impl Default for Community {
    fn default() -> Self {
        let (tx, rx) = channel();
        let draft = std::fs::read_to_string(draft_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let cache: HashMap<String, Vec<Value>> =
            std::fs::read_to_string(cache_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let cache = cache
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().filter_map(|x| serde_json::from_value(x).ok()).collect()))
            .collect();
        let c = hub::creds();
        let me_user = c.user.and_then(|u| serde_json::from_value(u).ok());
        Community {
            nav: Nav::Board(Board::Idea),
            sort: HashMap::new(),
            idea_filter: IdeaFilter::Open,
            query: String::new(),
            query_sent: String::new(),
            query_at: None,
            items: vec![],
            total: 0,
            page: 0,
            loading: false,
            list_err: None,
            skipped: 0,
            skip_err: String::new(),
            hidden_other: 0,
            board_counts: HashMap::new(),
            list_seq: 0,
            stale: true,
            pane: Pane::Empty,
            compose_back: None,
            detail: None,
            detail_loading: false,
            detail_err: None,
            sel: None,
            draft,
            preview: false,
            similar: vec![],
            similar_q: String::new(),
            similar_at: None,
            posting: false,
            reply: String::new(),
            reply_quote: None,
            reply_anon: false,
            replying: false,
            reply_edit: None,
            lightbox: None,
            admin_note: None,
            uploading: None,
            me_user,
            device_name: String::new(),
            unread: 0,
            notice: None,
            claimable: 0,
            banned: false,
            me_at: None,
            notifs: vec![],
            notifs_loading: false,
            auth: AuthForm::default(),
            imgs: HashMap::new(),
            toasts: vec![],
            seen_notice: 0,
            active: false,
            confirm: None,
            scroll_top: false,
            offline: false,
            tx,
            rx,
            ctx: None,
            cache,
        }
    }
}

/// 一页列表。解析不了的条目不再悄悄丢掉：记下条数和第一条的原因，界面上提示
#[derive(Default)]
pub struct ListPage {
    pub items: Vec<Post>,
    pub total: i64,
    pub skipped: usize,
    pub skip_err: String,
    /// 需求默认只看「待评估 + 计划中」；这里为空时，另外查一下全部状态有多少条
    pub hidden_other: i64,
}

fn parse_page(v: Value) -> ListPage {
    let mut pg = ListPage { total: v["total"].as_i64().unwrap_or(0), ..Default::default() };
    for x in v["items"].as_array().cloned().unwrap_or_default() {
        match serde_json::from_value::<Post>(x) {
            Ok(p) => pg.items.push(p),
            Err(e) => {
                if pg.skipped == 0 {
                    pg.skip_err = e.to_string();
                    eprintln!("community: skip item: {e}");
                }
                pg.skipped += 1;
            }
        }
    }
    pg
}

fn parse_list(v: Value) -> (Vec<Post>, i64) {
    let pg = parse_page(v);
    (pg.items, pg.total)
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl Community {
    /// 后台线程跑一个任务，结果送回界面
    fn spawn(&self, job: impl FnOnce() -> Res + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = job();
            let _ = tx.send(r);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    pub fn logged_in(&self) -> bool {
        self.me_user.is_some()
    }

    pub fn is_admin(&self) -> bool {
        self.me_user.as_ref().is_some_and(|u| u.is_admin)
    }

    pub fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toasts.push((msg.into(), err));
    }

    fn list_key(&self) -> String {
        match self.nav {
            Nav::Board(b) => format!("{}:{}:{:?}", b.id(), self.cur_sort().id(), self.idea_filter),
            Nav::Mine(m) => format!("mine:{m:?}"),
            Nav::Notifs => "notifs".into(),
        }
    }

    fn board_sort(&self, b: Board) -> Sort {
        *self.sort.get(&b).unwrap_or(&match b {
            Board::Idea => Sort::Hot,
            Board::Notice => Sort::New,
            _ => Sort::Active,
        })
    }

    /// 后台把各板块第一页都拉一遍，存进缓存
    fn prefetch(&mut self) {
        for b in Board::ALL {
            let sort = self.board_sort(b);
            let path = format!("/posts?page=0&board={}&sort={}", b.id(), sort.id());
            self.spawn(move || Res::Prefetch(b, hub::ensure_device().and_then(|_| hub::request("GET", &path, Body::None)).map(parse_page)));
        }
    }

    pub fn cur_sort(&self) -> Sort {
        match self.nav {
            Nav::Board(b) => self.board_sort(b),
            _ => Sort::New,
        }
    }

    pub fn set_sort(&mut self, s: Sort) {
        if let Nav::Board(b) = self.nav {
            self.sort.insert(b, s);
            self.reload();
        }
    }

    pub fn go(&mut self, nav: Nav) {
        if self.nav != nav {
            self.nav = nav;
            self.sel = None;
            self.query.clear();
            self.query_sent.clear();
            let key = self.list_key();
            self.items = self.cache.get(&key).cloned().unwrap_or_default();
        }
        self.reload();
        if nav == Nav::Notifs {
            self.load_notifs();
        }
    }

    pub fn reload(&mut self) {
        self.page = 0;
        self.stale = true;
    }

    /// 第一次打开社区页：注册设备、拉账号信息和列表
    pub fn activate(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        if !self.active {
            self.active = true;
            self.refresh_me();
            self.prefetch();
            let key = self.list_key();
            if self.items.is_empty() {
                self.items = self.cache.get(&key).cloned().unwrap_or_default();
            }
            // 只在第一次打开时拉。以前每帧都判断「列表为空就重拉」，
            // 板块真没内容（或默认筛选下为空）时会无限重拉，一直显示骨架屏
            self.stale = true;
        }
    }

    pub fn refresh_me(&mut self) {
        self.me_at = Some(Instant::now());
        self.spawn(|| {
            let r = hub::ensure_device().and_then(|_| hub::request("GET", "/me", Body::None));
            Res::Me(r)
        });
    }

    fn fetch_list(&mut self, append: bool) {
        self.list_seq += 1;
        let seq = self.list_seq;
        self.loading = true;
        self.list_err = None;
        if !append {
            self.page = 0;
        }
        let mut q: Vec<(String, String)> = vec![("page".into(), self.page.to_string())];
        match self.nav {
            Nav::Board(b) => {
                q.push(("board".into(), b.id().into()));
                q.push(("sort".into(), self.cur_sort().id().into()));
                if b == Board::Idea {
                    match self.idea_filter {
                        IdeaFilter::Open => {}
                        IdeaFilter::Planned => q.push(("status".into(), "planned".into())),
                        IdeaFilter::Done => q.push(("status".into(), "done".into())),
                        IdeaFilter::All => q.push(("status".into(), "all".into())),
                    }
                }
            }
            Nav::Mine(m) => q.push((
                "mine".into(),
                match m {
                    Mine::Posted => "posted",
                    Mine::Voted => "voted",
                    Mine::Replied => "replied",
                }
                .into(),
            )),
            Nav::Notifs => {
                self.loading = false;
                return;
            }
        }
        let query = self.query.trim().to_string();
        if !query.is_empty() {
            q.push(("q".into(), query));
        }
        let qs: Vec<String> = q.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect();
        let path = format!("/posts?{}", qs.join("&"));
        let default_ideas = self.nav == Nav::Board(Board::Idea) && self.idea_filter == IdeaFilter::Open && !append;
        self.spawn(move || {
            let r = hub::ensure_device().and_then(|_| hub::request("GET", &path, Body::None)).map(parse_page);
            Res::List(
                seq,
                r.map(|mut pg| {
                    if default_ideas && pg.items.is_empty() && pg.skipped == 0 {
                        let all = path.replace("board=idea", "board=idea&status=all");
                        pg.hidden_other = hub::request("GET", &all, Body::None).map(|v| v["total"].as_i64().unwrap_or(0)).unwrap_or(0);
                    }
                    pg
                }),
            )
        });
    }

    pub fn load_more(&mut self) {
        if !self.loading && (self.items.len() as i64) < self.total {
            self.page += 1;
            self.fetch_list(true);
        }
    }

    pub fn open_post(&mut self, id: i64) {
        if self.pane != Pane::Post(id) {
            self.reply.clear();
            self.reply_quote = None;
            self.scroll_top = true;
        }
        self.pane = Pane::Post(id);
        if self.detail.as_ref().map(|d| d.id) != Some(id) {
            self.detail = self.items.iter().find(|p| p.id == id).cloned();
        }
        self.detail_loading = true;
        self.detail_err = None;
        self.spawn(move || Res::Detail(id, hub::request("GET", &format!("/posts/{id}"), Body::None).and_then(|v| serde_json::from_value(v).map_err(|e| HubErr { code: "bad".into(), msg: e.to_string() }))));
        // 这帖的提醒服务器那边会标已读，本地先减掉
        let n = self.notifs.iter().filter(|x| x.post_id == id && !x.read).count() as i64;
        if n > 0 {
            for x in self.notifs.iter_mut().filter(|x| x.post_id == id) {
                x.read = true;
            }
            self.unread = (self.unread - n).max(0);
        }
    }

    pub fn load_notifs(&mut self) {
        self.notifs_loading = true;
        self.spawn(|| {
            Res::Notifs(hub::request("GET", "/me/notifications", Body::None).map(|v| {
                let items = v["items"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|x| serde_json::from_value(x).ok()).collect();
                (items, v["unread"].as_i64().unwrap_or(0))
            }))
        });
    }

    pub fn mark_all_read(&mut self) {
        for n in &mut self.notifs {
            n.read = true;
        }
        self.unread = 0;
        self.spawn(|| Res::Done(String::new(), None, hub::request("POST", "/me/notifications/read", Body::Json(json!({ "ids": [] })))));
    }

    // ------------------------------------------------------------ 投票 / 点赞（先改界面）

    pub fn vote(&mut self, id: i64) {
        let cur = self.items.iter().find(|p| p.id == id).map(|p| p.voted).or(self.detail.as_ref().filter(|d| d.id == id).map(|d| d.voted));
        let Some(cur) = cur else { return };
        let on = !cur;
        self.apply_vote(id, on, None);
        self.spawn(move || {
            let r = hub::ensure_device().and_then(|_| hub::request(if on { "PUT" } else { "DELETE" }, &format!("/posts/{id}/vote"), Body::None));
            Res::Voted(id, on, r)
        });
    }

    fn apply_vote(&mut self, id: i64, on: bool, score: Option<i64>) {
        let f = |p: &mut Post| {
            if p.id == id {
                if p.voted != on {
                    p.score += if on { 1 } else { -1 };
                }
                p.voted = on;
                if let Some(s) = score {
                    p.score = s;
                }
            }
        };
        self.items.iter_mut().for_each(f);
        if let Some(d) = self.detail.as_mut() {
            f(d);
        }
    }

    pub fn like(&mut self, reply: i64) {
        let Some(d) = self.detail.as_mut() else { return };
        let Some(r) = d.replies.iter_mut().find(|r| r.id == reply) else { return };
        let on = !r.liked;
        r.liked = on;
        r.likes += if on { 1 } else { -1 };
        self.spawn(move || {
            let r = hub::request(if on { "PUT" } else { "DELETE" }, &format!("/replies/{reply}/like"), Body::None);
            Res::Liked(reply, on, r)
        });
    }

    // ------------------------------------------------------------ 发帖 / 回复

    pub fn new_post(&mut self, board: Option<Board>) {
        if self.draft.edit.is_some() {
            self.draft = Draft::default();
        }
        let b = board.unwrap_or(match self.nav {
            Nav::Board(Board::Notice) if !self.is_admin() => Board::Idea,
            Nav::Board(b) => b,
            _ => Board::Idea,
        });
        if self.draft.title.is_empty() && self.draft.body.is_empty() {
            self.draft.board = b.id().into();
        } else if self.draft.board.is_empty() {
            self.draft.board = b.id().into();
        }
        self.preview = false;
        self.enter_compose();
    }

    /// 打开发帖：记住原来在看什么，Esc / 取消后回到那里
    fn enter_compose(&mut self) {
        if self.pane != Pane::Compose {
            self.compose_back = Some(self.pane.clone());
        }
        self.pane = Pane::Compose;
    }

    /// 关掉右边当前的内容（Esc / 返回）。发帖页存草稿后回到打开前的帖子
    pub fn close_pane(&mut self) {
        if self.pane == Pane::Compose {
            self.save_draft();
            self.pane = self.compose_back.take().filter(|p| *p != Pane::Compose).unwrap_or(Pane::Empty);
        } else {
            self.pane = Pane::Empty;
        }
    }

    /// 从日志 / 编辑器带着选中的内容发求助帖
    pub fn ask_with(&mut self, snippet: &str, lang: &str) {
        self.draft = Draft {
            board: "help".into(),
            title: String::new(),
            body: format!(
                "{}\n\n```{lang}\n{}\n```\n\n> QCCOM v{} · {}",
                t("（在这里描述问题）"),
                snippet.trim_end(),
                env!("CARGO_PKG_VERSION"),
                hub::os_name()
            ),
            anonymous: false,
            edit: None,
        };
        self.nav = Nav::Board(Board::Help);
        self.stale = true;
        self.pane = Pane::Compose;
        self.save_draft();
    }

    pub fn edit_post(&mut self, p: &Post) {
        self.draft = Draft { board: p.board.clone(), title: p.title.clone(), body: p.body.clone(), anonymous: p.author.anonymous, edit: Some(p.id) };
        self.enter_compose();
    }

    pub fn save_draft(&self) {
        if self.draft.edit.is_none() {
            if let Ok(s) = serde_json::to_string(&self.draft) {
                let _ = std::fs::write(draft_path(), s);
            }
        }
    }

    pub fn discard_draft(&mut self) {
        self.draft = Draft::default();
        let _ = std::fs::remove_file(draft_path());
        self.pane = match self.detail.as_ref() {
            Some(d) => Pane::Post(d.id),
            None => Pane::Empty,
        };
    }

    pub fn submit_post(&mut self) {
        let d = self.draft.clone();
        let n = d.title.trim().chars().count();
        if !(4..=80).contains(&n) {
            self.toast(t("标题 4–80 个字"), true);
            return;
        }
        if d.board != "idea" && d.body.trim().is_empty() {
            self.toast(t("内容不能为空"), true);
            return;
        }
        self.posting = true;
        self.spawn(move || {
            let r = hub::ensure_device().and_then(|_| match d.edit {
                Some(id) => hub::request("PATCH", &format!("/posts/{id}"), Body::Json(json!({ "title": d.title, "body": d.body }))),
                None => hub::request("POST", "/posts", Body::Json(json!({ "board": d.board, "title": d.title, "body": d.body, "anonymous": d.anonymous }))),
            });
            Res::Posted(r.and_then(|v| serde_json::from_value(v).map_err(|e| HubErr { code: "bad".into(), msg: e.to_string() })))
        });
    }

    pub fn submit_reply(&mut self) {
        let Some(d) = self.detail.as_ref() else { return };
        let body = self.reply.trim().to_string();
        if body.is_empty() || self.replying {
            return;
        }
        let (id, quote, anon, edit) = (d.id, self.reply_quote.as_ref().map(|q| q.0), self.reply_anon, self.reply_edit);
        self.replying = true;
        self.spawn(move || {
            let r = hub::ensure_device().and_then(|_| match edit {
                Some(rid) => hub::request("PATCH", &format!("/replies/{rid}"), Body::Json(json!({ "body": body }))).map(|mut v| {
                    v["edited"] = json!(true);
                    v
                }),
                None => hub::request("POST", &format!("/posts/{id}/replies"), Body::Json(json!({ "body": body, "quote": quote, "anonymous": anon }))),
            });
            Res::Replied(id, r)
        });
    }

    pub fn delete(&mut self, id: i64, is_reply: bool) {
        let post = self.detail.as_ref().map(|d| d.id);
        let path = if is_reply { format!("/replies/{id}") } else { format!("/posts/{id}") };
        if !is_reply {
            self.items.retain(|p| p.id != id);
            self.pane = Pane::Empty;
            self.detail = None;
        }
        self.spawn(move || Res::Done(t("已删除").into(), if is_reply { post } else { None }, hub::request("DELETE", &path, Body::None)));
    }

    pub fn solve(&mut self, post: i64, reply: Option<i64>) {
        if let Some(d) = self.detail.as_mut() {
            d.solved_reply = reply;
        }
        let msg = if reply.is_some() { t("已标记为已解决") } else { t("已取消已解决") };
        self.spawn(move || Res::Done(msg.into(), Some(post), hub::request("POST", &format!("/posts/{post}/solve"), Body::Json(json!({ "reply_id": reply })))));
    }

    /// 作者（管理员）在客户端里直接改状态 / 置顶 / 锁帖
    pub fn admin_patch(&mut self, post: i64, v: Value, msg: &'static str) {
        self.spawn(move || Res::Done(t(msg).into(), Some(post), hub::request("PATCH", &format!("/posts/{post}"), Body::Json(v))));
    }

    // ------------------------------------------------------------ 账号

    pub fn submit_auth(&mut self) {
        let a = &mut self.auth;
        a.busy = true;
        a.error.clear();
        let (reg, u, p, n) = (a.register, a.username.trim().to_string(), a.password.clone(), a.nickname.trim().to_string());
        self.spawn(move || {
            let r = hub::ensure_device().and_then(|_| {
                if reg {
                    hub::request("POST", "/auth/register", Body::Json(json!({ "username": u, "password": p, "nickname": n })))
                } else {
                    hub::request("POST", "/auth/login", Body::Json(json!({ "username": u, "password": p })))
                }
            });
            if let Ok(v) = &r {
                let tok = v["token"].as_str().unwrap_or_default().to_string();
                let user = v["user"].clone();
                hub::update_creds(|c| {
                    c.session = tok;
                    c.user = Some(user);
                });
            }
            Res::Auth(r)
        });
    }

    pub fn logout(&mut self) {
        let _ = std::thread::spawn(|| {
            let _ = hub::request("POST", "/auth/logout", Body::None);
            hub::update_creds(|c| {
                c.session.clear();
                c.user = None;
            });
        })
        .join();
        self.me_user = None;
        self.unread = 0;
        self.notifs.clear();
        self.pane = Pane::Empty;
        self.toast(t("已退出登录"), false);
        self.refresh_me();
        self.reload();
    }

    pub fn claim(&mut self) {
        self.claimable = 0;
        self.spawn(|| Res::Done(t("已把这台电脑之前匿名发的内容归到你的账号下").into(), None, hub::request("POST", "/me/claim", Body::None)));
    }

    pub fn save_profile(&mut self) {
        let mut v = json!({});
        let nick = self.auth.new_nick.trim().to_string();
        if !nick.is_empty() && Some(&nick) != self.me_user.as_ref().map(|u| &u.nickname) {
            v["nickname"] = json!(nick);
        }
        if !self.auth.new_pw.is_empty() {
            v["old_password"] = json!(self.auth.old_pw);
            v["new_password"] = json!(self.auth.new_pw);
        }
        if v.as_object().is_some_and(|o| o.is_empty()) {
            return;
        }
        self.auth.old_pw.clear();
        self.auth.new_pw.clear();
        self.spawn(move || Res::Done(t("已保存").into(), None, hub::request("PATCH", "/me", Body::Json(v))));
    }

    // ------------------------------------------------------------ 图片

    pub fn upload(&mut self, target: Target, bytes: Vec<u8>) {
        if bytes.len() > 2 * 1024 * 1024 {
            // 太大的先在本地缩一下
            match shrink(&bytes) {
                Some(b) => return self.upload(target, b),
                None => return self.toast(t("图片不能超过 2 MB"), true),
            }
        }
        self.uploading = Some(target);
        self.spawn(move || Res::Uploaded(target, hub::ensure_device().and_then(|_| hub::request("POST", "/upload", Body::Bytes(bytes)))));
    }

    pub fn upload_file(&mut self, target: Target, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(b) => self.upload(target, b),
            Err(e) => self.toast(e.to_string(), true),
        }
    }

    /// 拖进窗口的文件：图片插到正在写的帖子 / 回复里
    pub fn drop_file(&mut self, p: &std::path::Path) {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str()) {
            self.toast(t("只能拖入图片"), true);
            return;
        }
        let target = match self.pane {
            Pane::Post(_) => Target::Reply,
            Pane::Account if self.logged_in() => Target::Avatar,
            _ => {
                if self.pane != Pane::Compose {
                    self.new_post(None);
                }
                Target::Compose
            }
        };
        self.upload_file(target, p);
    }

    /// 剪贴板里的截图
    pub fn paste_image(&mut self, target: Target) -> bool {
        let img = arboard::Clipboard::new().and_then(|mut c| c.get_image());
        match img {
            Ok(img) => {
                let Some(buf) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else { return false };
                let mut out = std::io::Cursor::new(Vec::new());
                if image::DynamicImage::ImageRgba8(buf).write_to(&mut out, image::ImageFormat::Png).is_ok() {
                    self.upload(target, out.into_inner());
                    return true;
                }
                false
            }
            Err(_) => {
                self.toast(t("剪贴板里没有图片"), true);
                false
            }
        }
    }

    /// 帖子里的图片 / 上传的头像：按需下载，缓存成纹理
    pub fn image(&mut self, ctx: &egui::Context, id: &str) -> Option<(egui::TextureId, [usize; 2])> {
        match self.imgs.get(id) {
            Some(ImgState::Ready(t, s)) => return Some((t.id(), *s)),
            Some(_) => return None,
            None => {}
        }
        self.imgs.insert(id.to_string(), ImgState::Loading);
        let id2 = id.to_string();
        let _ = ctx;
        self.spawn(move || {
            let url = format!("{}/img/{id2}", hub::base_url());
            let r = (|| -> Result<Vec<u8>, HubErr> {
                let resp = hub::agent()
                    .get(&url)
                    .call()
                    .map_err(|e| HubErr { code: "network".into(), msg: e.to_string() })?;
                let mut b = vec![];
                std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 8 * 1024 * 1024), &mut b).map_err(|e| HubErr { code: "network".into(), msg: e.to_string() })?;
                Ok(b)
            })();
            Res::Img(id2, r)
        });
        None
    }

    // ------------------------------------------------------------ 每帧

    /// 不管在不在社区页都调：收结果、定时拉提醒
    pub fn tick(&mut self, ctx: &egui::Context, page_open: bool) {
        self.ctx = Some(ctx.clone());
        while let Ok(r) = self.rx.try_recv() {
            self.on_result(ctx, r);
        }
        // 提醒：打开过社区或登录了才定时拉（每 5 分钟），只亮状态栏小圆点
        let due = self.me_at.is_none_or(|t| t.elapsed() > Duration::from_secs(300));
        if due && (self.active || self.logged_in() || !hub::creds().device_id.is_empty()) {
            let first = self.me_at.is_none();
            self.refresh_me();
            if self.active && !first {
                self.prefetch();
            }
        }
        if !page_open {
            return;
        }
        // 搜索框：停止输入 300 ms 后再查
        if self.query != self.query_sent {
            let at = *self.query_at.get_or_insert_with(Instant::now);
            if at.elapsed() > Duration::from_millis(300) {
                self.query_sent = self.query.clone();
                self.query_at = None;
                self.stale = true;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        if self.stale && self.nav != Nav::Notifs {
            self.stale = false;
            self.fetch_list(false);
        }
        // 写需求标题时找相似的
        if self.pane == Pane::Compose && self.draft.edit.is_none() {
            let q = self.draft.title.trim().to_string();
            if q != self.similar_q {
                let at = *self.similar_at.get_or_insert_with(Instant::now);
                if at.elapsed() > Duration::from_millis(400) {
                    self.similar_q = q.clone();
                    self.similar_at = None;
                    if q.chars().count() >= 2 {
                        let board = self.draft.board.clone();
                        self.spawn(move || {
                            let r = hub::request("GET", &format!("/posts/similar?q={}&board={}", enc(&q), enc(&board)), Body::None);
                            Res::Similar(q, r.map(parse_list).map(|x| x.0).unwrap_or_default())
                        });
                    } else {
                        self.similar.clear();
                    }
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }
        }
    }

    fn on_result(&mut self, ctx: &egui::Context, r: Res) {
        match r {
            Res::List(seq, r) => {
                if seq != self.list_seq {
                    return;
                }
                self.loading = false;
                match r {
                    Ok(pg) => {
                        let ListPage { items, total, skipped, skip_err, hidden_other } = pg;
                        self.offline = false;
                        self.total = total;
                        self.hidden_other = hidden_other;
                        if let Nav::Board(b) = self.nav {
                            if self.query_sent.trim().is_empty() && (b != Board::Idea || self.idea_filter == IdeaFilter::Open) {
                                self.board_counts.insert(b, total);
                            }
                        }
                        self.skipped = if self.page == 0 { skipped } else { self.skipped + skipped };
                        if skipped > 0 {
                            self.skip_err = skip_err;
                        }
                        if self.page == 0 {
                            self.items = items;
                            if self.query.trim().is_empty() {
                                let key = self.list_key();
                                self.cache.insert(key, self.items.iter().take(30).cloned().collect());
                                self.save_cache();
                            }
                        } else {
                            self.items.extend(items);
                        }
                    }
                    Err(e) => {
                        self.offline = e.code == "network";
                        self.list_err = Some(e.msg);
                    }
                }
            }
            Res::Prefetch(b, r) => {
                if let Ok(pg) = r {
                    self.board_counts.insert(b, pg.total);
                    let key = format!("{}:{}:{:?}", b.id(), self.board_sort(b).id(), IdeaFilter::Open);
                    let cur = self.nav == Nav::Board(b) && self.list_key() == key;
                    // 正在看的板块不动（避免和正在进行的请求打架），只更新缓存
                    if !cur || self.items.is_empty() && !self.loading {
                        self.cache.insert(key, pg.items.into_iter().take(30).collect());
                        if cur {
                            self.items = self.cache.get(&self.list_key()).cloned().unwrap_or_default();
                        }
                        self.save_cache();
                    }
                }
            }
            Res::Detail(id, r) => {
                if self.pane != Pane::Post(id) {
                    return;
                }
                self.detail_loading = false;
                match r {
                    Ok(p) => {
                        // 列表里的计数也同步一下
                        if let Some(x) = self.items.iter_mut().find(|x| x.id == p.id) {
                            x.score = p.score;
                            x.voted = p.voted;
                            x.reply_count = p.reply_count;
                            x.status = p.status.clone();
                            x.solved_reply = p.solved_reply;
                        }
                        self.detail = Some(p);
                    }
                    Err(e) => self.detail_err = Some(e.msg),
                }
            }
            Res::Me(r) => match r {
                Ok(v) => {
                    self.offline = false;
                    self.me_user = serde_json::from_value(v["user"].clone()).ok();
                    if let Some(u) = &self.me_user {
                        if self.auth.new_nick.is_empty() {
                            self.auth.new_nick = u.nickname.clone();
                        }
                    }
                    hub::update_creds(|c| c.user = if v["user"].is_null() { None } else { Some(v["user"].clone()) });
                    self.device_name = v["device"]["name"].as_str().unwrap_or_default().to_string();
                    self.unread = v["unread"].as_i64().unwrap_or(0);
                    self.notice = serde_json::from_value(v["notice"].clone()).ok();
                    self.claimable = v["claimable"].as_i64().unwrap_or(0);
                    self.banned = v["banned"].as_bool().unwrap_or(false);
                }
                Err(e) => {
                    if e.code == "network" {
                        self.offline = true;
                    }
                }
            },
            Res::Similar(q, items) => {
                if q == self.similar_q {
                    self.similar = items;
                }
            }
            Res::Notifs(r) => {
                self.notifs_loading = false;
                match r {
                    Ok((items, unread)) => {
                        self.notifs = items;
                        self.unread = unread;
                    }
                    Err(e) => self.toast(e.msg, true),
                }
            }
            Res::Posted(r) => {
                self.posting = false;
                match r {
                    Ok(p) => {
                        let edited = self.draft.edit.is_some();
                        if !edited {
                            let _ = std::fs::remove_file(draft_path());
                        }
                        self.draft = Draft::default();
                        self.similar.clear();
                        self.toast(
                            if p.pending {
                                t("已提交，内容需要审核后才会公开")
                            } else if edited {
                                t("已保存修改")
                            } else {
                                t("发布成功")
                            },
                            false,
                        );
                        let b = Board::from_id(&p.board);
                        self.nav = Nav::Board(b);
                        if !edited {
                            self.sort.insert(b, Sort::New);
                            self.idea_filter = IdeaFilter::Open;
                        }
                        self.reload();
                        let id = p.id;
                        self.detail = Some(p);
                        self.open_post(id);
                    }
                    Err(e) => self.toast(e.msg, true),
                }
            }
            Res::Replied(id, r) => {
                self.replying = false;
                match r {
                    Ok(v) => {
                        self.reply.clear();
                        self.reply_quote = None;
                        let edited = self.reply_edit.take().is_some() || v["edited"].as_bool() == Some(true);
                        if edited {
                            if self.pane == Pane::Post(id) {
                                self.open_post(id);
                            }
                            return;
                        }
                        if v["pending"].as_bool() == Some(true) {
                            self.toast(t("已提交，内容需要审核后才会公开"), false);
                        }
                        if let Some(x) = self.items.iter_mut().find(|x| x.id == id) {
                            x.reply_count += 1;
                        }
                        if self.pane == Pane::Post(id) {
                            self.open_post(id);
                            // 发完回复滚到最底下
                            ctx.data_mut(|d| d.insert_temp(egui::Id::new("comm-scroll-bottom"), true));
                        }
                    }
                    Err(e) => self.toast(e.msg, true),
                }
            }
            Res::Voted(id, on, r) => match r {
                Ok(v) => self.apply_vote(id, on, v["score"].as_i64()),
                Err(e) => {
                    self.apply_vote(id, !on, None);
                    self.toast(e.msg, true);
                }
            },
            Res::Liked(reply, on, r) => {
                let fix = |c: &mut Community, likes: Option<i64>, liked: bool| {
                    if let Some(d) = c.detail.as_mut() {
                        if let Some(x) = d.replies.iter_mut().find(|x| x.id == reply) {
                            if x.liked != liked {
                                x.likes += if liked { 1 } else { -1 };
                            }
                            x.liked = liked;
                            if let Some(l) = likes {
                                x.likes = l;
                            }
                        }
                    }
                };
                match r {
                    Ok(v) => fix(self, v["likes"].as_i64(), on),
                    Err(e) => {
                        fix(self, None, !on);
                        self.toast(e.msg, true);
                    }
                }
            }
            Res::Auth(r) => {
                self.auth.busy = false;
                match r {
                    Ok(v) => {
                        self.me_user = serde_json::from_value(v["user"].clone()).ok();
                        let name = self.me_user.as_ref().map(|u| u.nickname.clone()).unwrap_or_default();
                        self.auth.password.clear();
                        self.auth.new_nick = name.clone();
                        self.toast(tf("欢迎，{}", &[&name]), false);
                        self.pane = Pane::Account;
                        self.refresh_me();
                        self.reload();
                    }
                    Err(e) => self.auth.error = e.msg,
                }
            }
            Res::Img(id, r) => {
                let st = r
                    .ok()
                    .and_then(|b| image::load_from_memory(&b).ok())
                    .map(|img| {
                        let rgba = img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        ImgState::Ready(ctx.load_texture(format!("comm-img-{id}"), ci, egui::TextureOptions::LINEAR), size)
                    })
                    .unwrap_or(ImgState::Failed);
                self.imgs.insert(id, st);
            }
            Res::Uploaded(target, r) => {
                self.uploading = None;
                match r {
                    Ok(v) => {
                        let id = v["id"].as_str().unwrap_or_default().to_string();
                        let tag = format!("![]({}{id})", "img:");
                        match target {
                            Target::Compose => {
                                if !self.draft.body.is_empty() && !self.draft.body.ends_with('\n') {
                                    self.draft.body.push('\n');
                                }
                                self.draft.body.push_str(&tag);
                                self.draft.body.push('\n');
                                self.save_draft();
                            }
                            Target::Reply => {
                                if !self.reply.is_empty() && !self.reply.ends_with('\n') {
                                    self.reply.push('\n');
                                }
                                self.reply.push_str(&tag);
                                self.reply.push('\n');
                            }
                            Target::Avatar => {
                                self.spawn(move || Res::Done(t("头像已更新").into(), None, hub::request("PATCH", "/me", Body::Json(json!({ "avatar": id })))));
                            }
                        }
                    }
                    Err(e) => self.toast(e.msg, true),
                }
            }
            Res::Done(msg, post, r) => match r {
                Ok(v) => {
                    if !msg.is_empty() {
                        self.toast(msg, false);
                    }
                    if v.get("user").is_some_and(|u| !u.is_null()) {
                        self.me_user = serde_json::from_value(v["user"].clone()).ok();
                        hub::update_creds(|c| c.user = Some(v["user"].clone()));
                    }
                    if let Some(p) = post {
                        if self.pane == Pane::Post(p) {
                            self.open_post(p);
                        }
                    }
                    self.reload();
                }
                Err(e) => {
                    self.toast(e.msg, true);
                    if let Some(p) = post {
                        if self.pane == Pane::Post(p) {
                            self.open_post(p);
                        }
                    }
                }
            },
        }
    }

    fn save_cache(&self) {
        // 只存列表需要的字段（正文不存）
        let v: HashMap<&String, Vec<Value>> = self
            .cache
            .iter()
            .map(|(k, items)| {
                (
                    k,
                    items
                        .iter()
                        .map(|p| {
                            json!({ "id": p.id, "board": p.board, "title": p.title, "excerpt": p.excerpt, "status": p.status,
                                "done_version": p.done_version, "score": p.score, "reply_count": p.reply_count, "pinned": p.pinned,
                                "created_at": p.created_at, "last_reply_at": p.last_reply_at, "voted": p.voted,
                                "solved_reply": p.solved_reply, "author": { "name": p.author.name, "user_id": p.author.user_id,
                                "seed": p.author.seed, "staff": p.author.staff, "avatar": p.author.avatar } })
                        })
                        .collect(),
                )
            })
            .collect();
        if let Ok(s) = serde_json::to_string(&v) {
            let _ = std::fs::write(cache_path(), s);
        }
    }

    /// 状态栏图标要不要亮小圆点
    pub fn has_news(&self) -> bool {
        self.unread > 0 || self.notice.as_ref().is_some_and(|n| n.id > self.seen_notice)
    }
}

/// 太大的图片先在本地缩到最长边 2560 再上传
fn shrink(b: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(b).ok()?;
    let img = if img.width().max(img.height()) > 2560 { img.resize(2560, 2560, image::imageops::FilterType::Triangle) } else { img };
    let mut out = std::io::Cursor::new(Vec::new());
    img.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
    let v = out.into_inner();
    (v.len() <= 2 * 1024 * 1024).then_some(v)
}

/// 相对时间
pub fn ago(ts: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let s = (now - ts).max(0);
    if s < 60 {
        t("刚刚").into()
    } else if s < 3600 {
        tf("{} 分钟前", &[&(s / 60)])
    } else if s < 86400 {
        tf("{} 小时前", &[&(s / 3600)])
    } else if s < 86400 * 30 {
        tf("{} 天前", &[&(s / 86400)])
    } else {
        chrono::DateTime::from_timestamp(ts, 0).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string()).unwrap_or_default()
    }
}

pub fn status_name(s: &str) -> &'static str {
    match s {
        "planned" => t("计划中"),
        "done" => t("已完成"),
        "declined" => t("不做"),
        _ => t("待评估"),
    }
}

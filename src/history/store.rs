//! 本地历史的存储：内容按哈希去重压缩存一份（所有工程共用，同一个 SDK 在多个工程里只占一份），
//! 每个工程一个目录，记录事件流水（events.jsonl）和当前状态（state.json）。
//! 所有写入都先写临时文件再改名，写到一半断电也不会损坏已有记录。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub type Hash = String;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

pub fn hash_of(b: &[u8]) -> Hash {
    let d = Sha256::digest(b);
    d.iter().take(20).map(|x| format!("{x:02x}")).collect()
}

/// 先写 .tmp 再改名
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[derive(Clone, Debug)]
pub struct Objects {
    dir: PathBuf,
}

impl Objects {
    pub fn new(history_dir: &Path) -> Self {
        Objects { dir: history_dir.join("objects") }
    }

    fn path(&self, h: &str) -> PathBuf {
        let (a, b) = h.split_at(2.min(h.len()));
        self.dir.join(a).join(b)
    }

    pub fn put(&self, data: &[u8]) -> std::io::Result<Hash> {
        let h = hash_of(data);
        let p = self.path(&h);
        if !p.exists() {
            let z = miniz_oxide::deflate::compress_to_vec(data, 6);
            write_atomic(&p, &z)?;
        }
        Ok(h)
    }

    pub fn get(&self, h: &str) -> Option<Vec<u8>> {
        let z = std::fs::read(self.path(h)).ok()?;
        miniz_oxide::inflate::decompress_to_vec_with_limit(&z, 512 << 20).ok()
    }

    /// 总占用字节数
    pub fn size(&self) -> u64 {
        dir_size(&self.dir)
    }
}

pub fn dir_size(d: &Path) -> u64 {
    let mut n = 0;
    let Ok(rd) = std::fs::read_dir(d) else { return 0 };
    for e in rd.flatten() {
        let Ok(m) = e.metadata() else { continue };
        n += if m.is_dir() { dir_size(&e.path()) } else { m.len() };
    }
    n
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 第一次打开工程时的完整快照
    Init,
    /// 在 QCCOM 里保存
    Save,
    /// 外部程序（AI、其它编辑器）改动或新建
    External,
    /// 外部删除
    Delete,
    /// 审阅时「保留」
    Keep,
    /// 审阅时「还原」或从历史还原文件
    Revert,
    /// 存档点
    Checkpoint,
    /// 整个工程回到某个存档点
    Restore,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Event {
    pub id: u64,
    /// Unix 毫秒
    pub ts: i64,
    pub kind: Kind,
    /// 外部改动的回合号（连续一段时间内的改动算一个回合）
    #[serde(default)]
    pub round: u64,
    #[serde(default)]
    pub note: String,
    /// (相对路径, 改动后的内容；None 表示删除)
    #[serde(default)]
    pub files: Vec<(String, Option<Hash>)>,
    /// 改动前的内容，和 files 一一对应
    #[serde(default)]
    pub before: Vec<Option<Hash>>,
    /// 存档点：整个工程的清单（存成一个对象）
    #[serde(default)]
    pub manifest: Option<Hash>,
    /// 手动打的存档点：不会被自动清理
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FileState {
    pub hash: Hash,
    pub size: u64,
    /// 修改时间（Unix 纳秒）
    pub mtime: i64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct State {
    /// 当前磁盘上每个文件的状态
    pub known: BTreeMap<String, FileState>,
    /// 审阅基准：只记还没审阅的外部改动之前的内容；None 表示当时文件不存在。保留或还原后删掉该项
    pub baseline: BTreeMap<String, Option<Hash>>,
    pub next_id: u64,
    pub round: u64,
    pub last_ext: i64,
    /// 首次快照完成
    pub ready: bool,
    /// 上次清理时间
    #[serde(default)]
    pub pruned: i64,
}

/// 审阅清单里的一项
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub rel: String,
    /// 基准内容（None：基准时还没有这个文件）
    pub base: Option<Hash>,
    /// 当前内容（None：已被删除）
    pub cur: Option<Hash>,
    pub round: u64,
    pub ts: i64,
}

pub struct Project {
    dir: PathBuf,
    pub obj: Objects,
    pub state: State,
    pub events: Vec<Event>,
    /// 每次有新的外部改动加一，界面据此刷新
    pub rev: u64,
    dirty: bool,
}

pub fn history_dir() -> PathBuf {
    crate::data_dir().join("history")
}

fn project_key(root: &Path) -> String {
    let s = root.to_string_lossy().replace('\\', "/").to_lowercase();
    hash_of(s.as_bytes())[..16].to_string()
}

pub fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let r = p.strip_prefix(root).ok()?;
    let s = r.to_string_lossy().replace('\\', "/");
    (!s.is_empty()).then_some(s)
}

impl Project {
    pub fn open(history: &Path, root: &Path) -> Self {
        let dir = history.join("p").join(project_key(root));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("root.txt"), root.to_string_lossy().as_bytes());
        let state: State = std::fs::read(dir.join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let events = std::fs::read_to_string(dir.join("events.jsonl"))
            .map(|s| s.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()).collect())
            .unwrap_or_default();
        Project {
            dir,
            obj: Objects::new(history),
            state,
            events,
            rev: 0,
            dirty: false,
        }
    }

    pub fn flush(&mut self) {
        if self.dirty {
            if let Ok(b) = serde_json::to_vec(&self.state) {
                let _ = write_atomic(&self.dir.join("state.json"), &b);
            }
            self.dirty = false;
        }
    }

    fn append(&mut self, mut ev: Event) -> u64 {
        self.state.next_id += 1;
        ev.id = self.state.next_id;
        if let Ok(line) = serde_json::to_string(&ev) {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("events.jsonl")) {
                let _ = writeln!(f, "{line}");
            }
        }
        let id = ev.id;
        self.events.push(ev);
        self.dirty = true;
        id
    }

    pub fn record(&mut self, kind: Kind, files: Vec<(String, Option<Hash>)>, before: Vec<Option<Hash>>, note: &str) -> u64 {
        let round = if kind == Kind::External || kind == Kind::Delete { self.state.round } else { 0 };
        self.append(Event {
            id: 0,
            ts: now_ms(),
            kind,
            round,
            note: note.into(),
            files,
            before,
            manifest: None,
            pinned: false,
        })
    }

    /// 外部改动：90 秒内连续的改动算同一个回合
    pub fn bump_round(&mut self) {
        let now = now_ms();
        if now - self.state.last_ext > 90_000 {
            self.state.round += 1;
        }
        self.state.last_ext = now;
    }

    /// 打一个存档点，返回编号
    pub fn checkpoint(&mut self, note: &str, pinned: bool) -> u64 {
        let m: BTreeMap<&String, &Hash> = self.state.known.iter().map(|(k, v)| (k, &v.hash)).collect();
        let manifest = serde_json::to_vec(&m).ok().and_then(|b| self.obj.put(&b).ok());
        self.append(Event {
            id: 0,
            ts: now_ms(),
            kind: Kind::Checkpoint,
            round: 0,
            note: note.into(),
            files: vec![],
            before: vec![],
            manifest,
            pinned,
        })
    }

    pub fn manifest(&self, ev: &Event) -> Option<BTreeMap<String, Hash>> {
        let b = self.obj.get(ev.manifest.as_ref()?)?;
        serde_json::from_slice(&b).ok()
    }

    pub fn event(&self, id: u64) -> Option<&Event> {
        self.events.iter().rev().find(|e| e.id == id)
    }

    /// 和基准不一样的文件
    pub fn pending(&self) -> Vec<Pending> {
        let mut out = vec![];
        let mut last: std::collections::HashMap<&str, (u64, i64)> = Default::default();
        for e in &self.events {
            if matches!(e.kind, Kind::External | Kind::Delete) {
                for (r, _) in &e.files {
                    last.insert(r.as_str(), (e.round, e.ts));
                }
            }
        }
        for (rel, base) in &self.state.baseline {
            let cur = self.state.known.get(rel).map(|f| f.hash.clone());
            if &cur != base {
                let (round, ts) = last.get(rel.as_str()).copied().unwrap_or((0, 0));
                out.push(Pending {
                    rel: rel.clone(),
                    base: base.clone(),
                    cur,
                    round,
                    ts,
                });
            }
        }
        out.sort_by(|a, b| b.ts.cmp(&a.ts).then(a.rel.cmp(&b.rel)));
        out
    }

    /// 读一个文件在某个哈希下的内容
    pub fn content(&self, h: &Option<Hash>) -> Option<Vec<u8>> {
        self.obj.get(h.as_ref()?)
    }

    /// 内容变化登记：更新 known，返回旧哈希；new 为 None 表示删除
    pub fn set_known(&mut self, rel: &str, new: Option<FileState>) -> Option<Hash> {
        self.dirty = true;
        match new {
            Some(f) => self.state.known.insert(rel.to_string(), f).map(|f| f.hash),
            None => self.state.known.remove(rel).map(|f| f.hash),
        }
    }

    /// 新文件第一次出现时，审阅基准记为「当时不存在」
    pub fn ensure_baseline(&mut self, rel: &str, old: Option<Hash>) {
        if !self.state.baseline.contains_key(rel) {
            self.state.baseline.insert(rel.to_string(), old);
        }
    }

    /// 保留：基准跟上当前内容
    pub fn accept(&mut self, rel: &str) {
        self.state.baseline.remove(rel);
        self.dirty = true;
    }

    /// 基准里多余的项（和当前一致的）可以删掉，省空间
    pub fn compact_baseline(&mut self) {
        let known = &self.state.known;
        self.state.baseline.retain(|rel, h| known.get(rel).map(|f| &f.hash) != h.as_ref());
    }

    /// 清理：超过 days 天的普通版本删掉（手动存档点保留），总占用超过 max_bytes 时从最旧的开始删
    pub fn prune(&mut self, days: u32, max_bytes: u64) {
        let cut = now_ms() - days as i64 * 86_400_000;
        let before = self.events.len();
        self.events.retain(|e| e.pinned || e.ts >= cut);
        if max_bytes > 0 && self.obj.size() > max_bytes {
            let unpinned: Vec<u64> = self.events.iter().filter(|e| !e.pinned).map(|e| e.id).collect();
            let drop: HashSet<u64> = unpinned.iter().take(unpinned.len().div_ceil(4)).copied().collect();
            self.events.retain(|e| !drop.contains(&e.id));
        }
        if self.events.len() != before {
            let mut s = String::new();
            for e in &self.events {
                if let Ok(l) = serde_json::to_string(e) {
                    s += &l;
                    s.push('\n');
                }
            }
            let _ = write_atomic(&self.dir.join("events.jsonl"), s.as_bytes());
        }
        self.state.pruned = now_ms();
        self.dirty = true;
    }

    /// 本工程引用的所有对象
    pub fn refs(&self, out: &mut HashSet<Hash>) {
        for f in self.state.known.values() {
            out.insert(f.hash.clone());
        }
        for h in self.state.baseline.values().flatten() {
            out.insert(h.clone());
        }
        for e in &self.events {
            for h in e.files.iter().filter_map(|f| f.1.as_ref()).chain(e.before.iter().flatten()) {
                out.insert(h.clone());
            }
            if let Some(m) = &e.manifest {
                out.insert(m.clone());
                if let Some(map) = self.manifest(e) {
                    out.extend(map.into_values());
                }
            }
        }
    }
}

/// 删除所有工程都不再引用的对象（只删一小时以前写入的，避免和正在写的冲突）
pub fn gc(history: &Path) {
    let mut refs = HashSet::new();
    let Ok(rd) = std::fs::read_dir(history.join("p")) else { return };
    for e in rd.flatten() {
        let Ok(root) = std::fs::read_to_string(e.path().join("root.txt")) else { continue };
        let p = Project::open(history, Path::new(&root));
        p.refs(&mut refs);
    }
    let obj = history.join("objects");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for a in std::fs::read_dir(&obj).into_iter().flatten().flatten() {
        let name_a = a.file_name().to_string_lossy().into_owned();
        for b in std::fs::read_dir(a.path()).into_iter().flatten().flatten() {
            let name = format!("{name_a}{}", b.file_name().to_string_lossy());
            if name.contains(".tmp") || refs.contains(&name) {
                continue;
            }
            if b.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < old) {
                let _ = std::fs::remove_file(b.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_and_events() {
        let d = std::env::temp_dir().join(format!("qccom-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let o = Objects::new(&d);
        let h = o.put(b"hello world").unwrap();
        assert_eq!(o.get(&h).unwrap(), b"hello world");
        assert_eq!(o.put(b"hello world").unwrap(), h);

        let root = d.join("proj");
        let mut p = Project::open(&d, &root);
        p.set_known("a.c", Some(FileState { hash: h.clone(), size: 11, mtime: 1 }));
        p.ensure_baseline("a.c", Some(h.clone()));
        let h2 = o.put(b"changed").unwrap();
        p.bump_round();
        let old = p.set_known("a.c", Some(FileState { hash: h2.clone(), size: 7, mtime: 2 }));
        p.record(Kind::External, vec![("a.c".into(), Some(h2.clone()))], vec![old], "");
        let pend = p.pending();
        assert_eq!(pend.len(), 1);
        assert_eq!(pend[0].base.as_deref(), Some(h.as_str()));
        let cp = p.checkpoint("before", true);
        assert!(p.manifest(p.event(cp).unwrap()).unwrap().contains_key("a.c"));
        p.accept("a.c");
        assert!(p.pending().is_empty());
        p.flush();
        let p2 = Project::open(&d, &root);
        assert_eq!(p2.events.len(), 2);
        assert!(p2.state.known.contains_key("a.c"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

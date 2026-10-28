//! 本地历史、存档点、外部改动审阅的底层：后台线程定时扫描工程文件夹（跳过 .gitignore 排除的目录），
//! 发现内容变化就存一个版本。在 QCCOM 里保存的算「自己改的」，其它程序（AI、别的编辑器）改的算「外部修改」，
//! 外部修改进入审阅清单，直到保留或还原。

pub mod ignore;
pub mod review;
pub mod store;

use crate::i18n::{t, tf};
use ignore::Ignore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
pub use store::{Event, FileState, Hash, Kind, Pending, Project};

#[derive(Clone, Debug)]
pub struct Opts {
    /// 超过这个大小的文件不记版本
    pub max_file: u64,
    pub days: u32,
    pub max_bytes: u64,
    /// 这些程序运行时，外部修改照常记版本，但不进审阅清单（例如 Keil 自己保存文件）
    pub quiet: Vec<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            max_file: 20 << 20,
            days: 30,
            max_bytes: 10 << 30,
            quiet: vec![],
        }
    }
}

pub struct History {
    pub root: PathBuf,
    p: Arc<Mutex<Project>>,
    stop: Arc<AtomicBool>,
    /// 首次快照进行中
    pub busy: Arc<AtomicBool>,
    /// 已扫描的文件数（首次快照的进度）
    pub scanned: Arc<AtomicU64>,
    /// 工程的 .gitignore 有没有排除编译产物
    pub covers_build: bool,
    opts: Arc<Mutex<Opts>>,
}

impl Drop for History {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.lock().flush();
    }
}

impl History {
    pub fn start(root: &Path, opts: Opts, ctx: eframe::egui::Context) -> Self {
        let p = Arc::new(Mutex::new(Project::open(&store::history_dir(), root)));
        let stop = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let scanned = Arc::new(AtomicU64::new(0));
        let opts = Arc::new(Mutex::new(opts));
        let covers_build = Ignore::load(root).covers_build_outputs();
        let h = History {
            root: root.to_path_buf(),
            p: p.clone(),
            stop: stop.clone(),
            busy: busy.clone(),
            scanned: scanned.clone(),
            covers_build,
            opts: opts.clone(),
        };
        let root = root.to_path_buf();
        std::thread::Builder::new().name("history".into()).spawn(move || watch(root, p, stop, busy, scanned, opts, ctx)).ok();
        h
    }

    pub fn set_opts(&self, o: Opts) {
        if let Ok(mut g) = self.opts.lock() {
            *g = o;
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Project> {
        self.p.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn rel(&self, p: &Path) -> Option<String> {
        store::rel_of(&self.root, p)
    }

    /// 在 QCCOM 里保存了文件
    pub fn note_save(&self, path: &Path, bytes: &[u8]) {
        let Some(rel) = self.rel(path) else { return };
        if bytes.len() as u64 > self.opts.lock().map_or(20 << 20, |o| o.max_file) {
            return;
        }
        let mut p = self.lock();
        let Ok(h) = p.obj.put(bytes) else { return };
        let fs = file_state(path, h.clone());
        let old = p.set_known(&rel, Some(fs));
        if old.as_ref() != Some(&h) {
            p.record(Kind::Save, vec![(rel, Some(h))], vec![old], "");
        }
        p.flush();
    }

    /// 文件在 QCCOM 里被删除、改名（改名后新文件由扫描发现）
    pub fn note_delete(&self, path: &Path) {
        let Some(rel) = self.rel(path) else { return };
        let mut p = self.lock();
        let rels: Vec<String> = p.state.known.keys().filter(|k| *k == &rel || k.starts_with(&format!("{rel}/"))).cloned().collect();
        if rels.is_empty() {
            return;
        }
        let mut files = vec![];
        let mut before = vec![];
        for r in rels {
            before.push(p.set_known(&r, None));
            files.push((r, None));
        }
        p.record(Kind::Delete, files, before, "QCCOM");
        p.flush();
    }

    pub fn checkpoint(&self, note: &str, pinned: bool) -> u64 {
        let mut p = self.lock();
        let id = p.checkpoint(note, pinned);
        p.flush();
        id
    }

    pub fn pending(&self) -> Vec<Pending> {
        self.lock().pending()
    }

    pub fn rev(&self) -> u64 {
        self.lock().rev
    }

    /// 保留这些文件的外部修改
    pub fn keep(&self, rels: &[String]) {
        let mut p = self.lock();
        let mut files = vec![];
        for r in rels {
            p.accept(r);
            let cur = p.state.known.get(r).map(|f| f.hash.clone());
            files.push((r.clone(), cur));
        }
        if !files.is_empty() {
            p.record(Kind::Keep, files, vec![], "");
        }
        p.rev += 1;
        p.flush();
    }

    /// 全部保留，并打一个存档点作为下一轮的基准
    pub fn keep_all(&self) -> u64 {
        let rels: Vec<String> = self.pending().into_iter().map(|x| x.rel).collect();
        self.keep(&rels);
        self.checkpoint(t("全部保留"), false)
    }

    /// 还原这些文件到审阅基准（外部修改之前）
    pub fn revert(&self, rels: &[String]) -> Result<usize, String> {
        let mut p = self.lock();
        let mut files = vec![];
        let mut before = vec![];
        let mut errs = vec![];
        for r in rels {
            let Some(base) = p.state.baseline.get(r).cloned() else { continue };
            let data = match &base {
                Some(h) => match p.obj.get(h) {
                    Some(d) => Some(d),
                    None => {
                        errs.push(tf("{}：历史内容已被清理", &[r]));
                        continue;
                    }
                },
                None => None,
            };
            match write_file(&self.root, r, data.as_deref()) {
                Ok(()) => {
                    let fs = base.clone().map(|h| file_state(&self.root.join(r), h));
                    before.push(p.set_known(r, fs));
                    p.accept(r);
                    files.push((r.clone(), base));
                }
                Err(e) => errs.push(format!("{r}：{e}")),
            }
        }
        let n = files.len();
        if n > 0 {
            p.record(Kind::Revert, files, before, "");
        }
        p.rev += 1;
        p.flush();
        if errs.is_empty() {
            Ok(n)
        } else {
            Err(errs.join("\n"))
        }
    }

    /// 把一个文件恢复成历史里的某个版本（None：删除）
    pub fn restore_file(&self, rel: &str, h: Option<Hash>) -> Result<(), String> {
        let mut p = self.lock();
        let data = match &h {
            Some(h) => Some(p.obj.get(h).ok_or_else(|| t("历史内容已被清理").to_string())?),
            None => None,
        };
        write_file(&self.root, rel, data.as_deref())?;
        let fs = h.clone().map(|h| file_state(&self.root.join(rel), h));
        let old = p.set_known(rel, fs);
        p.record(Kind::Revert, vec![(rel.to_string(), h)], vec![old], "");
        p.rev += 1;
        p.flush();
        Ok(())
    }

    /// 整个工程回到某个存档点。回退前先自动存一个，回退本身也能撤销
    pub fn restore(&self, cp: u64) -> Result<(usize, u64), String> {
        let (manifest, label) = {
            let p = self.lock();
            let ev = p.event(cp).ok_or_else(|| t("找不到这个存档点").to_string())?;
            (p.manifest(ev).ok_or_else(|| t("存档点内容已被清理").to_string())?, format!("#{}", ev.id))
        };
        let undo = self.checkpoint(&tf("回到存档点 {} 之前", &[&label]), true);
        let mut p = self.lock();
        let mut n = 0;
        let mut errs = vec![];
        let cur: Vec<(String, Hash)> = p.state.known.iter().map(|(k, v)| (k.clone(), v.hash.clone())).collect();
        let mut files = vec![];
        let mut before = vec![];
        for (rel, h) in &manifest {
            if p.state.known.get(rel).map(|f| &f.hash) == Some(h) {
                continue;
            }
            let Some(data) = p.obj.get(h) else {
                errs.push(tf("{}：历史内容已被清理", &[rel]));
                continue;
            };
            match write_file(&self.root, rel, Some(&data)) {
                Ok(()) => {
                    before.push(p.set_known(rel, Some(file_state(&self.root.join(rel), h.clone()))));
                    files.push((rel.clone(), Some(h.clone())));
                    n += 1;
                }
                Err(e) => errs.push(format!("{rel}：{e}")),
            }
        }
        for (rel, h) in cur {
            if manifest.contains_key(&rel) {
                continue;
            }
            match write_file(&self.root, &rel, None) {
                Ok(()) => {
                    p.set_known(&rel, None);
                    before.push(Some(h));
                    files.push((rel, None));
                    n += 1;
                }
                Err(e) => errs.push(format!("{rel}：{e}")),
            }
        }
        p.state.baseline.clear();
        p.record(Kind::Restore, files, before, &label);
        p.rev += 1;
        p.flush();
        if errs.is_empty() {
            Ok((n, undo))
        } else {
            Err(errs.join("\n"))
        }
    }

    /// 历史里某个文件的所有版本（新的在前）：(事件, 该版本内容)
    pub fn versions(&self, rel: &str) -> Vec<(Event, Option<Hash>)> {
        let p = self.lock();
        let mut out = vec![];
        for e in p.events.iter().rev() {
            if let Some((_, h)) = e.files.iter().find(|(r, _)| r == rel) {
                out.push((e.clone(), h.clone()));
            }
        }
        out
    }

    /// 被删除、现在不存在的文件：(路径, 删除前的内容, 删除时间)
    pub fn deleted(&self) -> Vec<(String, Hash, i64)> {
        let p = self.lock();
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for e in p.events.iter().rev() {
            for (i, (rel, h)) in e.files.iter().enumerate() {
                if h.is_none() && !p.state.known.contains_key(rel) && seen.insert(rel.clone()) {
                    if let Some(Some(b)) = e.before.get(i) {
                        out.push((rel.clone(), b.clone(), e.ts));
                    }
                }
            }
        }
        out
    }

    pub fn content(&self, h: &Option<Hash>) -> Option<Vec<u8>> {
        self.lock().content(h)
    }

    pub fn events(&self) -> Vec<Event> {
        self.lock().events.clone()
    }

    pub fn storage_bytes(&self) -> u64 {
        self.lock().obj.size()
    }
}

fn file_state(path: &Path, hash: Hash) -> FileState {
    let m = std::fs::metadata(path).ok();
    FileState {
        hash,
        size: m.as_ref().map_or(0, |m| m.len()),
        mtime: m.and_then(|m| m.modified().ok()).map_or(0, nanos),
    }
}

fn nanos(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as i64)
}

/// 写回或删除工程里的文件；被别的程序占用时给出明确提示
pub fn write_file(root: &Path, rel: &str, data: Option<&[u8]>) -> Result<(), String> {
    let path = root.join(rel);
    match data {
        Some(d) => save_file(&path, d),
        None if path.exists() => std::fs::remove_file(&path).map_err(io_msg),
        None => Ok(()),
    }
}

/// 临时文件的后缀，扫描时跳过
const TMP_TAG: &str = ".qccom~";

/// 安全保存：先写同目录的临时文件并落盘，再改名替换，断电也不会只剩半个文件。
/// 改名失败（例如文件被别的程序以不允许删除的方式打开）时退回直接覆盖
pub fn save_file(path: &Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}{TMP_TAG}"));
    let staged = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        if let Ok(m) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, m.permissions());
        }
        Ok(())
    })();
    if staged.is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return Ok(());
    }
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(path, data).map_err(io_msg)
}

pub fn io_msg(e: std::io::Error) -> String {
    // Windows: 32 = ERROR_SHARING_VIOLATION，33 = ERROR_LOCK_VIOLATION
    if matches!(e.raw_os_error(), Some(32 | 33)) || e.kind() == std::io::ErrorKind::PermissionDenied {
        t("文件被占用（可能是 Keil 等程序打开着），请关闭后重试").to_string()
    } else {
        e.to_string()
    }
}

static ACTIVE: Mutex<Option<Arc<History>>> = Mutex::new(None);

/// 代码页打开工程时登记，其它页面保存文件也能记成「自己改的」
pub fn set_active(h: Option<Arc<History>>) {
    if let Ok(mut g) = ACTIVE.lock() {
        *g = h;
    }
}

fn active() -> Option<Arc<History>> {
    ACTIVE.lock().ok().and_then(|g| g.clone())
}

/// QCCOM 里任何地方保存工程文件后调用
pub fn saved(path: &Path, bytes: &[u8]) {
    if let Some(h) = active() {
        h.note_save(path, bytes);
    }
}

/// QCCOM 里删除了文件或文件夹
pub fn removed(path: &Path) {
    if let Some(h) = active() {
        h.note_delete(path);
    }
}

/// 保存并登记。先登记再写：后台扫描随时可能读到新内容，先登记它就会认出是自己保存的
pub fn save_tracked(path: &Path, bytes: &[u8]) -> Result<(), String> {
    saved(path, bytes);
    save_file(path, bytes)
}

/// 工程里的文件（按 .gitignore 和默认的编译产物规则排除），相对路径
pub fn project_files(root: &Path, max_file: u64) -> Vec<String> {
    let mut ig = Ignore::load(root);
    ig.extend(&Ignore::defaults());
    let mut out = vec![];
    walk(root, &mut ig, max_file, &mut out, &AtomicBool::new(false));
    out.into_iter().map(|f| f.0).collect()
}

/// 遍历工程里要跟踪的文件：(相对路径, 大小, 修改时间)
fn walk(root: &Path, ig: &mut Ignore, max_file: u64, out: &mut Vec<(String, u64, i64)>, stop: &AtomicBool) {
    fn rec(root: &Path, rel: &str, ig: &mut Ignore, max_file: u64, out: &mut Vec<(String, u64, i64)>, stop: &AtomicBool) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let n = ig.push_dir(root, rel);
        let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    if name == ".git" || ig.matched(&r, true) {
                        continue;
                    }
                    rec(root, &r, ig, max_file, out, stop);
                } else if ft.is_file() && !name.ends_with(TMP_TAG) && !ig.matched(&r, false) {
                    let Ok(m) = e.metadata() else { continue };
                    if m.len() <= max_file {
                        out.push((r, m.len(), m.modified().map_or(0, nanos)));
                    }
                }
            }
        }
        ig.truncate(n);
    }
    rec(root, "", ig, max_file, out, stop);
}

/// 这些程序有没有在运行（结果缓存几秒）
fn quiet_running(names: &[String], cache: &mut (Instant, bool, usize)) -> bool {
    if names.is_empty() {
        return false;
    }
    if cache.0.elapsed() < Duration::from_secs(5) && cache.2 == names.len() {
        return cache.1;
    }
    let list = process_names();
    let hit = names.iter().any(|n| {
        let n = n.trim().to_lowercase();
        !n.is_empty() && list.iter().any(|p| p == &n || p.trim_end_matches(".exe") == n.trim_end_matches(".exe"))
    });
    *cache = (Instant::now(), hit, names.len());
    hit
}

/// 正在运行的程序名（小写）
pub fn process_names() -> Vec<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("tasklist").args(["/fo", "csv", "/nh"]).creation_flags(0x0800_0000).output();
        out.map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split(',').next())
                .map(|s| s.trim_matches('"').to_lowercase())
                .collect()
        })
        .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let mut v = vec![];
        for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            if let Ok(s) = std::fs::read_to_string(e.path().join("comm")) {
                v.push(s.trim().to_lowercase());
            }
        }
        v
    }
}

fn watch(root: PathBuf, p: Arc<Mutex<Project>>, stop: Arc<AtomicBool>, busy: Arc<AtomicBool>, scanned: Arc<AtomicU64>, opts: Arc<Mutex<Opts>>, ctx: eframe::egui::Context) {
    let lock = || p.lock().unwrap_or_else(|e| e.into_inner());
    // 扫描用的缓存：路径 → (大小, 修改时间)
    let mut seen: HashMap<String, (u64, i64)> = lock().state.known.iter().map(|(k, f)| (k.clone(), (f.size, f.mtime))).collect();
    let mut initial = !lock().state.ready;
    let mut quiet_cache = (Instant::now() - Duration::from_secs(60), false, 0);
    let mut interval = Duration::from_millis(1200);
    {
        let o = opts.lock().map(|o| o.clone()).unwrap_or_default();
        let mut g = lock();
        if store::now_ms() - g.state.pruned > 86_400_000 {
            g.prune(o.days, o.max_bytes);
            g.compact_baseline();
            g.flush();
            drop(g);
            std::thread::spawn(|| store::gc(&store::history_dir()));
        }
    }
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        let o = opts.lock().map(|o| o.clone()).unwrap_or_default();
        let mut ig = Ignore::load(&root);
        if !ig.covers_build_outputs() {
            let mut d = Ignore::defaults();
            d.extend(&ig);
            ig = d;
        }
        busy.store(initial, Ordering::Relaxed);
        let mut files = Vec::with_capacity(seen.len() + 16);
        walk(&root, &mut ig, o.max_file, &mut files, &stop);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let present: std::collections::HashSet<&str> = files.iter().map(|f| f.0.as_str()).collect();
        let gone: Vec<String> = seen.keys().filter(|k| !present.contains(k.as_str())).cloned().collect();
        let mut changed = vec![];
        for (rel, size, mtime) in &files {
            if seen.get(rel) != Some(&(*size, *mtime)) {
                changed.push((rel.clone(), *size, *mtime));
            }
        }
        // 先在锁外读文件、存内容
        let obj = lock().obj.clone();
        let mut hashed = vec![];
        for (i, (rel, size, mtime)) in changed.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if initial {
                scanned.store(i as u64, Ordering::Relaxed);
            }
            let path = root.join(rel);
            let Ok(data) = std::fs::read(&path) else { continue };
            // 读的过程中又被改了：下一轮再说
            let Ok(m) = std::fs::metadata(&path) else { continue };
            if m.len() != *size || m.modified().map_or(0, nanos) != *mtime {
                continue;
            }
            let Ok(h) = obj.put(&data) else { continue };
            hashed.push((rel.clone(), FileState { hash: h, size: *size, mtime: *mtime }));
        }
        if !hashed.is_empty() || !gone.is_empty() {
            let quiet = !initial && quiet_running(&o.quiet, &mut quiet_cache);
            let mut g = lock();
            let mut files_ev = vec![];
            let mut before = vec![];
            let mut del_ev = vec![];
            let mut del_before = vec![];
            for (rel, fs) in hashed {
                seen.insert(rel.clone(), (fs.size, fs.mtime));
                let prev = g.state.known.get(&rel).map(|f| f.hash.clone());
                if prev.as_ref() == Some(&fs.hash) {
                    g.set_known(&rel, Some(fs)); // 只是时间变了（或是我们自己写的）
                    continue;
                }
                let h = fs.hash.clone();
                g.set_known(&rel, Some(fs));
                if initial {
                    continue;
                }
                if !quiet {
                    g.ensure_baseline(&rel, prev.clone());
                }
                files_ev.push((rel, Some(h)));
                before.push(prev);
            }
            for rel in gone {
                seen.remove(&rel);
                if let Some(prev) = g.set_known(&rel, None) {
                    if !initial {
                        if !quiet {
                            g.ensure_baseline(&rel, Some(prev.clone()));
                        }
                        del_ev.push((rel, None));
                        del_before.push(Some(prev));
                    }
                }
            }
            if !files_ev.is_empty() || !del_ev.is_empty() {
                g.bump_round();
                let note = if quiet { "quiet" } else { "" };
                if !files_ev.is_empty() {
                    g.record(Kind::External, files_ev, before, note);
                }
                if !del_ev.is_empty() {
                    g.record(Kind::Delete, del_ev, del_before, note);
                }
                g.compact_baseline();
                g.rev += 1;
                ctx.request_repaint();
            }
            g.flush();
        }
        if initial {
            initial = false;
            busy.store(false, Ordering::Relaxed);
            let mut g = lock();
            g.state.ready = true;
            g.record(Kind::Init, vec![], vec![], "");
            g.flush();
            ctx.request_repaint();
        }
        // 扫描越慢间隔越长，几万个文件的工程也不占 CPU
        let took = t0.elapsed();
        interval = (took * 5).clamp(Duration::from_millis(1200), Duration::from_secs(15));
        let until = Instant::now() + interval;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    let _ = interval;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_respects_gitignore() {
        let d = std::env::temp_dir().join(format!("qccom-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("build")).unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join("src/main.c"), "int main;").unwrap();
        std::fs::write(d.join("src/main.o"), "x").unwrap();
        std::fs::write(d.join("build/a.bin"), "x").unwrap();
        std::fs::write(d.join(".git/HEAD"), "x").unwrap();
        std::fs::write(d.join(".gitignore"), "build/\n*.o\n").unwrap();
        let mut ig = Ignore::load(&d);
        let mut out = vec![];
        walk(&d, &mut ig, 1 << 20, &mut out, &AtomicBool::new(false));
        let mut names: Vec<_> = out.into_iter().map(|f| f.0).collect();
        names.sort();
        assert_eq!(names, vec![".gitignore", "src/main.c"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// 工程里最新的编译产物（.elf / .axf / .out，有同名 .map 时优先用 .map，能按文件汇总体积）。
/// 在常见的输出目录里找，最多往下四层
pub fn build_output(root: &Path) -> Option<PathBuf> {
    const EXTS: &[&str] = &["elf", "axf", "out", "map"];
    fn rec(dir: &Path, depth: u32, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten().take(2000) {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if depth > 0 && !n.starts_with('.') || n == ".pio" {
                    if !matches!(n.as_str(), "node_modules" | "src" | "inc" | "include" | "drivers" | "middlewares" | ".git") {
                        rec(&p, depth.saturating_sub(1), best);
                    }
                }
            } else if let Some(ext) = p.extension().map(|x| x.to_string_lossy().to_lowercase()) {
                if EXTS.contains(&ext.as_str()) {
                    if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
                            *best = Some((t, p));
                        }
                    }
                }
            }
        }
    }
    let mut best = None;
    rec(root, 4, &mut best);
    let (_, p) = best?;
    // 有同名 .map 就用 .map
    let map = p.with_extension("map");
    Some(if map.is_file() { map } else { p })
}

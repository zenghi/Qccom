use super::*;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

pub(super) const WORK: u8 = 1;
pub(super) const REFS: u8 = 2;
pub(super) const EXTRA: u8 = 4;
pub(super) const ALL: u8 = WORK | REFS | EXTRA;

pub(super) struct RepoWatch {
    changed: Arc<AtomicU8>,
    active: Arc<AtomicBool>,
    _stop: Sender<()>,
}

impl RepoWatch {
    pub(super) fn start(root: PathBuf, ctx: egui::Context) -> Self {
        let (stop, closed) = mpsc::channel();
        let changed = Arc::new(AtomicU8::new(0));
        let active = Arc::new(AtomicBool::new(false));
        let dirty = changed.clone();
        let ready = active.clone();
        std::thread::spawn(move || {
            let Ok(git_dir) = git::git(&root, &["rev-parse", "--absolute-git-dir"]) else { return };
            let Ok(common_dir) = git::git(&root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { return };
            let git_dir = PathBuf::from(git_dir.trim_end_matches(['\r', '\n']));
            let common_dir = PathBuf::from(common_dir.trim_end_matches(['\r', '\n']));
            let dirs = [root.clone(), git_dir, common_dir];
            let watched = dirs.clone();
            let signal = dirty.clone();
            let repaint = ctx.clone();
            let health = ready.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |event: notify::Result<Event>| {
                    let changes = match event {
                        Ok(event) => classify(&watched, event),
                        Err(_) => {
                            health.store(false, Ordering::Release);
                            ALL
                        }
                    };
                    if changes != 0 && signal.fetch_or(changes, Ordering::AcqRel) & changes != changes {
                        repaint.request_repaint();
                    }
                },
                Config::default(),
            ) else {
                return;
            };
            // A linked worktree keeps its index and HEAD in a separate directory.
            let mut roots: Vec<PathBuf> = Vec::new();
            for dir in [&dirs[0], &dirs[2], &dirs[1]] {
                if !roots.iter().any(|p| dir.starts_with(p)) {
                    if watcher.watch(dir, RecursiveMode::Recursive).is_err() {
                        return;
                    }
                    roots.push(dir.clone());
                }
            }
            ready.store(true, Ordering::Release);
            dirty.fetch_or(ALL, Ordering::AcqRel);
            ctx.request_repaint();
            let _ = closed.recv();
        });
        Self { changed, active, _stop: stop }
    }

    pub(super) fn take(&self) -> u8 {
        self.changed.swap(0, Ordering::AcqRel)
    }

    pub(super) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

fn classify(dirs: &[PathBuf; 3], event: Event) -> u8 {
    if matches!(event.kind, EventKind::Access(_)) {
        return 0;
    }
    if event.need_rescan() {
        return ALL;
    }
    event.paths.iter().fold(0, |changes, path| {
        if let Some(p) = path.strip_prefix(&dirs[1]).ok().or_else(|| path.strip_prefix(&dirs[2]).ok()) {
            let first = p.components().next().and_then(|c| c.as_os_str().to_str()).unwrap_or("");
            if first == "refs" || first == "logs" || first == "reftable" || matches!(first, "" | "HEAD" | "packed-refs" | "shallow" | "config" | "config.worktree") {
                return changes | ALL;
            }
            if matches!(first, "index" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD" | "rebase-merge" | "rebase-apply" | "sequencer") {
                return changes | WORK;
            }
            return changes;
        }
        if let Ok(p) = path.strip_prefix(&dirs[0]) {
            return changes | if p == Path::new(".git") || p == Path::new(".gitmodules") { ALL } else { WORK };
        }
        changes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_edits_staging_and_commits_update_open_git_view() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let run = |args: &[&str]| git::git(root, args).unwrap();
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@example.com"]);
        let path = root.join("driver.c");
        std::fs::write(&path, "int value = 0;\n").unwrap();
        run(&["add", "--", "driver.c"]);
        run(&["commit", "-qm", "initial"]);
        let mut view = GitView::new(GitSettings::default(), egui::Context::default());
        view.set_repo(Some(root.to_owned()), false);
        fn wait(view: &mut GitView, predicate: impl Fn(&GitView) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                view.poll();
                if predicate(view) {
                    break;
                }
                assert!(Instant::now() < deadline, "repository data stayed stale");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        wait(&mut view, |v| v.watcher.as_ref().is_some_and(RepoWatch::active) && v.status.is_some());
        std::fs::write(&path, "int value = 11;\n").unwrap();
        wait(&mut view, |v| v.status.as_ref().is_some_and(|s| s.files.iter().any(|f| f.path == "driver.c" && f.y == 'M')));
        view.sel = Sel::Work;
        view.open_work_diff(view.status.as_ref().unwrap().files[0].clone());
        wait(&mut view, |v| v.diff.as_ref().is_some_and(|d| d.raw.contains("+int value = 11;")));
        let scroll_id = view.diff_scroll_id;
        std::fs::write(&path, "int value = 22;\n").unwrap();
        wait(&mut view, |v| v.diff.as_ref().is_some_and(|d| d.raw.contains("+int value = 22;")));
        run(&["add", "--", "driver.c"]);
        wait(&mut view, |v| v.diff.as_ref().is_some_and(|d| d.staged_hunks == 1 && d.work.as_ref().is_some_and(|f| f.x == 'M' && f.y == ' ')));
        assert_eq!(view.diff_scroll_id, scroll_id);
        run(&["commit", "-qm", "from terminal"]);
        wait(&mut view, |v| v.diff.is_none() && v.dirty_count() == 0 && v.commits.first().is_some_and(|c| c.subject == "from terminal"));
        run(&["switch", "-qc", "external"]);
        wait(&mut view, |v| v.head_name() == "external");
    }
}

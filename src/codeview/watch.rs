use super::*;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::sync::atomic::AtomicBool;

/// Registers recursive file watching outside the UI thread, including on large workspaces.
pub(super) struct ProjectWatch {
    changed: Arc<AtomicBool>,
    _stop: mpsc::Sender<()>,
}

impl ProjectWatch {
    pub(super) fn start(root: PathBuf, ctx: egui::Context) -> Self {
        let (stop, closed) = mpsc::channel();
        let changed = Arc::new(AtomicBool::new(false));
        let dirty = changed.clone();
        std::thread::spawn(move || {
            let root_for_watch = root.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |event: notify::Result<Event>| {
                    if event.is_ok_and(|e| relevant(&root, &e)) {
                        dirty.store(true, Ordering::Release);
                        ctx.request_repaint();
                    }
                },
                Config::default(),
            ) else {
                return;
            };
            if watcher.watch(&root_for_watch, RecursiveMode::Recursive).is_ok() {
                let _ = closed.recv();
            }
        });
        Self { changed, _stop: stop }
    }

    pub(super) fn changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

fn relevant(root: &Path, event: &Event) -> bool {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    if matches!(event.kind, EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))) {
        return false;
    }
    event.paths.iter().filter_map(|p| p.strip_prefix(root).ok()).any(|p| {
        if p.components().any(|c| c.as_os_str().to_str().is_some_and(cindex::skip_dir)) {
            return false;
        }
        match event.kind {
            EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => true,
            _ => Lang::from_path(p).is_code(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_writes_rebuild_symbols_even_for_closed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("driver.c");
        std::fs::write(&path, "int device(void) { return 1; }").unwrap();
        let ctx = egui::Context::default();
        let mut view = CodeView::new(CodeSettings::default());
        view.root = Some(dir.path().to_owned());
        view.index = Some(Arc::new(cindex::build(dir.path(), &cindex::shared_progress())));
        let poll = |view: &mut CodeView| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| view.poll(ctx));
        };
        poll(&mut view);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while view.index_dirty_at.is_none() {
            assert!(Instant::now() < deadline, "missing notification for a source file");
            std::fs::write(&path, "\n\nint device(void) { return 2; }").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(25));
            poll(&mut view);
        }
        while view.definitions("device").first().is_none_or(|d| d.1 != 2) {
            assert!(Instant::now() < deadline, "index did not refresh after an external edit");
            std::thread::sleep(std::time::Duration::from_millis(25));
            poll(&mut view);
        }
        assert_eq!(view.definitions("device").len(), 1);
        assert!(view.tabs.is_empty());
    }
}

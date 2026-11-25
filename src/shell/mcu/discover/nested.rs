//! Bounded directory searches performed by the MCU discovery worker.
use super::*;
use std::collections::VecDeque;
use std::ffi::OsString;

fn can_descend(entry: &fs::DirEntry) -> bool {
    if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return entry.metadata().is_ok_and(|meta| meta.file_attributes() & 0x400 == 0);
    }
    #[cfg(not(windows))]
    true
}

pub(super) fn searchable(root: &Path) -> bool {
    root.is_absolute() && root.parent().is_some() && Host::env_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).as_deref() != Some(root)
}

// Breadth first so SDK examples buried deep in one package cannot starve sibling packages.
// The visitor can prune an entire project after identifying its root.
pub(super) fn walk(root: &Path, skip: &[&str], mut visit: impl FnMut(&Path, &[OsString]) -> bool) -> bool {
    if !searchable(root) || !root.is_dir() {
        return true;
    }
    let mut queue = VecDeque::from([(root.to_owned(), 0)]);
    let mut dirs_left = 4096usize;
    let mut entries_left = 150_000usize;
    let mut complete = true;
    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            complete = false;
            continue;
        };
        let mut children = Vec::new();
        let mut files = Vec::new();
        for entry in entries.flatten() {
            if entries_left == 0 {
                return false;
            }
            entries_left -= 1;
            let name = entry.file_name();
            if can_descend(&entry) {
                let text = name.to_string_lossy();
                if !text.starts_with('.') && !skip.iter().any(|s| text.eq_ignore_ascii_case(s)) {
                    children.push(entry.path());
                }
            } else {
                files.push(name);
            }
        }
        if visit(&dir, &files) {
            children.sort();
            if depth == 20 || children.len() > dirs_left {
                complete &= children.is_empty();
                children.truncate(if depth == 20 { 0 } else { dirs_left });
            }
            dirs_left -= children.len();
            queue.extend(children.into_iter().map(|p| (p, depth + 1)));
        }
    }
    complete
}

#[derive(Default)]
pub(in crate::shell) struct Projects {
    pub paths: Vec<PathBuf>,
    pub pending: bool,
    pub complete: bool,
}

pub(super) fn projects(workspace: &Path, requested: &Path) -> (Projects, Option<PathBuf>) {
    let mut paths = Vec::new();
    let mut matched = Vec::new();
    let outer = workspace.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let matches = |name: &str| {
        let name = name.to_lowercase();
        !name.is_empty() && outer.strip_suffix(&name).is_some_and(|prefix| prefix.is_empty() || prefix.ends_with(['_', '-', ' ']))
    };
    let complete = walk(workspace, &["node_modules", "target", "obj", "CMakeFiles", "CMakeCache", "__pycache__"], |dir, files| {
        if dir != workspace && files.iter().any(|f| f == "CMakeCache.txt") {
            return false;
        }
        if dir == workspace {
            return true;
        }
        let mut descriptors = files
            .iter()
            .map(Path::new)
            .filter(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("wvproj")) && dir.join(p).is_file())
            .peekable();
        if descriptors.peek().is_none() {
            let has = |name: &str| files.iter().any(|f| f == name);
            let makefile = wch::MAKEFILES.iter().find(|name| has(name)).map(|name| dir.join(name));
            if makefile.is_none() && !has("CMakeLists.txt") && !has(".cproject") {
                return true;
            }
            let cmake = if has("CMakeLists.txt") {
                read_prefix(&dir.join("CMakeLists.txt")).to_ascii_lowercase()
            } else {
                String::new()
            };
            if !wch_project(dir, &read_prefix(&dir.join(".project")), &cmake, makefile.as_deref()) {
                return true;
            }
        }
        let relative = dir.strip_prefix(workspace).unwrap().to_owned();
        if matches(&dir.file_name().unwrap_or_default().to_string_lossy()) || descriptors.any(|p| matches(&p.file_stem().unwrap_or_default().to_string_lossy())) {
            matched.push(relative.clone());
        }
        paths.push(relative);
        false
    });
    paths.sort();
    let selected = if !requested.as_os_str().is_empty() {
        paths.iter().find(|p| *p == requested).cloned()
    } else if complete && paths.len() == 1 {
        paths.first().cloned()
    } else if complete && matched.len() == 1 {
        matched.pop()
    } else {
        None
    };
    let pending = selected.is_none() && (!paths.is_empty() || !requested.as_os_str().is_empty());
    (Projects { paths, pending, complete }, selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path, dir: &str, name: &str) -> PathBuf {
        let project = root.join(dir);
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(format!("{name}.wvproj")), []).unwrap();
        project
    }

    #[test]
    fn nested_workspace_matches_descriptor_and_keeps_an_explicit_target() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("CH582M_翻盖柜");
        let main = project(&workspace, "BLE/Peripheral", "翻盖柜");
        let example = project(&workspace, "USB/Host/Example", "Example");
        project(&main, "obj/cache", "ignored");
        let detected = Session::discover(workspace.clone(), Options::default());
        assert_eq!(detected.profile, Profile::Wch);
        assert_eq!(detected.root, main);
        assert_eq!(detected.projects.paths.len(), 2);
        assert_eq!(detected.workspace, workspace);
        let options = Options {
            wch_subproject: "USB/Host/Example".into(),
            ..Default::default()
        };
        let detected = Session::discover(workspace.clone(), options.clone());
        assert_eq!(detected.root, example);
        fs::remove_dir_all(&example).unwrap();
        let stale = Session::discover(workspace, options);
        assert!(stale.projects.pending && stale.actions().is_empty());
        assert!(stale.command(Syntax::PowerShell, Action::Flash, "COM30").is_err());
    }

    #[test]
    fn single_make_project_and_ambiguous_targets_under_plain_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("board/vendor/application");
        fs::create_dir_all(&first).unwrap();
        fs::write(first.join("GNUmakefile"), "CFLAGS += -DCH32V203\n").unwrap();
        assert_eq!(Session::discover(tmp.path().into(), Options::default()).root, first);
        project(tmp.path(), "other/board", "Demo");
        assert!(Session::discover(tmp.path().into(), Options::default()).projects.pending);
        fs::write(tmp.path().join("platformio.ini"), "").unwrap();
        let root_project = Session::discover(tmp.path().into(), Options::default());
        assert_eq!(root_project.profile, Profile::PlatformIo);
        assert_eq!(root_project.root, tmp.path());
    }

    #[cfg(unix)]
    #[test]
    fn skips_directory_links_and_generated_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let outside = project(tmp.path(), "outside", "Demo");
        std::os::unix::fs::symlink(outside, root.join("external")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        project(&root, ".git/objects", "ignore");
        project(&root, "build/cache", "ignore");
        fs::write(root.join("build/CMakeCache.txt"), "").unwrap();
        assert!(projects(&root, Path::new("")).0.paths.is_empty());
    }
}

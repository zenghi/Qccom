//! Read Eclipse CDT managed-build settings and resolve virtual folders in .project.
use super::*;
use roxmltree::{Document, Node};

fn xml(path: &Path) -> Option<String> {
    if path.metadata().ok()?.len() > 4 << 20 {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn child<'a>(node: Node<'a, '_>, tag: &str) -> Option<&'a str> {
    node.children().find(|c| c.has_tag_name(tag))?.text()
}

struct Project<'a> {
    root: &'a Path,
    name: String,
    links: Vec<(PathBuf, PathBuf)>,
}

impl<'a> Project<'a> {
    fn new(root: &'a Path) -> Self {
        let mut p = Self {
            root,
            name: root.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            links: vec![],
        };
        if let Some(s) = xml(&root.join(".project")) {
            if let Ok(doc) = Document::parse(&s) {
                let desc = doc.root_element();
                if let Some(name) = child(desc, "name") {
                    p.name = name.to_owned();
                }
                for link in desc.descendants().filter(|n| n.has_tag_name("link")) {
                    let Some(name) = child(link, "name") else { continue };
                    let dest = child(link, "location").map(PathBuf::from).or_else(|| {
                        let uri = child(link, "locationURI")?;
                        let (base, path) = uri.split_once('/').unwrap_or((uri, ""));
                        let up = if base == "PROJECT_LOC" {
                            0
                        } else {
                            base.strip_prefix("PARENT-")?.strip_suffix("-PROJECT_LOC")?.parse().ok()?
                        };
                        Some(root.ancestors().nth(up)?.join(path))
                    });
                    if let Some(dest) = dest {
                        p.links.push((PathBuf::from(name), dest));
                    }
                }
                p.links.sort_by_key(|(name, _)| std::cmp::Reverse(name.components().count()));
            }
        }
        p
    }

    fn option_path(&self, value: &str, build_dir: Option<&Path>, forced: bool) -> Option<String> {
        let raw = value.trim().trim_matches('"').replace('\\', "/");
        let mut resolved = self.path(&raw)?;
        if raw.starts_with("../") {
            // Generated build directories may not exist before the first build.
            let mut parts = Path::new(&raw).components().peekable();
            let depth = std::iter::from_fn(|| parts.next_if_eq(&std::path::Component::ParentDir)).count();
            if let Some(base) = build_dir.and_then(|dir| dir.ancestors().nth(depth)) {
                let candidate = base.join(parts.collect::<PathBuf>());
                if candidate.exists() {
                    resolved = candidate;
                }
            }
        }
        if forced && !resolved.is_file() && !raw.contains('/') {
            return Some(raw); // GCC also searches include directories for a forced header.
        }
        Some(resolved.to_string_lossy().into_owned())
    }

    fn path(&self, value: &str) -> Option<PathBuf> {
        let v = value.trim().trim_matches('"').replace('\\', "/");
        let rel = if let Some(s) = v.strip_prefix("${workspace_loc:/").and_then(|s| s.strip_suffix('}')) {
            let (name, sub) = s.split_once('/').unwrap_or((s, ""));
            if name != "${ProjName}" && name != self.name && Some(std::ffi::OsStr::new(name)) != self.root.file_name() {
                return None;
            }
            sub
        } else if let Some(s) = v.strip_prefix("${ProjDirPath}").or_else(|| v.strip_prefix("${project_loc}")) {
            s.trim_start_matches('/')
        } else if let Some(s) = v.strip_prefix("${project_loc:/").and_then(|s| s.strip_suffix('}')) {
            s
        } else {
            v.as_str()
        };
        if rel.contains("${") {
            return None;
        }
        let path = Path::new(rel);
        for (name, dest) in &self.links {
            if let Ok(sub) = path.strip_prefix(name) {
                return Some(dest.join(sub));
            }
        }
        Some(self.root.join(path))
    }
}

fn compiler_args(cfg: Node<'_, '_>, project: &Project<'_>, file: &Path, cpp: bool) -> Option<Vec<String>> {
    let mut scopes = cfg
        .descendants()
        .filter(|n| n.has_tag_name("folderInfo") || n.has_tag_name("fileInfo"))
        .filter_map(|n| {
            let scope = n.attribute("resourcePath")?;
            (scope.is_empty() || file.starts_with(project.path(scope)?)).then_some((scope.len(), n))
        })
        .collect::<Vec<_>>();
    scopes.sort_by_key(|(depth, _)| *depth);
    let config_name = cfg.descendants().find(|n| n.has_tag_name("configuration")).and_then(|n| n.attribute("name"));
    let build_dir = cfg
        .descendants()
        .find(|n| n.has_tag_name("builder"))
        .and_then(|n| n.attribute("buildPath"))
        .and_then(|v| project.path(v))
        .or_else(|| config_name.map(|s| project.root.join(s)));
    let mut groups: Vec<(&str, Vec<String>)> = vec![];
    let mut found = false;
    for (_, scope) in scopes {
        for tool in scope.descendants().filter(|n| n.has_tag_name("tool")) {
            let class = tool.attribute("superClass").unwrap_or("");
            let tag = if cpp { ".cpp.compiler" } else { ".c.compiler" };
            if !class.contains(tag) && !(class.contains("compiler") && class.ends_with(if cpp { ".cpp" } else { ".c" })) {
                continue;
            }
            found = true;
            for option in tool.children().filter(|n| n.has_tag_name("option")) {
                let flag = match option.attribute("valueType") {
                    Some("includePath") => "-I",
                    Some("includeFiles") => "-include",
                    Some("definedSymbols") => "-D",
                    Some("undefDefinedSymbols") => "-U",
                    _ => continue,
                };
                let mut values = vec![];
                for v in option.children().filter(|n| n.has_tag_name("listOptionValue")).filter_map(|n| n.attribute("value")) {
                    let value = if matches!(flag, "-I" | "-include") {
                        let Some(path) = project.option_path(v, build_dir.as_deref(), flag == "-include") else { continue };
                        path
                    } else {
                        v.trim().trim_matches('"').to_owned()
                    };
                    values.push(value);
                }
                // Resource-specific lists replace the inherited setting of the same type.
                if let Some((_, inherited)) = groups.iter_mut().find(|(kind, _)| *kind == flag) {
                    *inherited = values;
                } else {
                    groups.push((flag, values));
                }
            }
        }
    }
    found.then(|| groups.into_iter().flat_map(|(flag, values)| values.into_iter().flat_map(move |v| [flag.to_owned(), v])).collect())
}

pub(super) fn load(path: &Path, file: &Path, opt: &mut Options) -> Option<()> {
    let text = xml(path)?;
    let doc = Document::parse(&text).ok()?;
    let root = path.parent()?;
    let project = Project::new(root);
    let cpp = opt.macros.contains_key("__cplusplus");
    let mut configs = doc.descendants().filter(|n| n.has_tag_name("cconfiguration"));
    let args = compiler_args(configs.next()?, &project, file, cpp)?;
    // When configurations disagree, only an actual compilation database can select a target.
    if configs.any(|cfg| compiler_args(cfg, &project, file, cpp).as_ref() != Some(&args)) {
        return None;
    }
    opt.flags(root, file, &args);
    Some(())
}

// CDT projects shared from Windows often use different casing in includes and on disk.
// Keep exact matches first and reject ambiguous names on case-sensitive filesystems.
#[cfg(not(windows))]
pub(super) fn case_path(path: &Path, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    if path.exists() {
        return Some(path.to_owned());
    }
    if let Some(p) = cache.get(path) {
        return p.clone();
    }
    let result = (|| {
        let parent = case_path(path.parent()?, cache)?;
        let component = match path.components().next_back()? {
            std::path::Component::ParentDir => return Some(parent.join("..")),
            std::path::Component::Normal(name) => name,
            _ => return None,
        };
        let direct = parent.join(component);
        if direct.exists() {
            return Some(direct);
        }
        let mut entries = fs::read_dir(parent).ok()?.flatten().filter(|e| e.file_name().eq_ignore_ascii_case(component));
        let first = entries.next()?;
        entries.next().is_none().then(|| first.path())
    })();
    cache.insert(path.to_owned(), result.clone());
    result
}

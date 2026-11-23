//! Project-aware conditional shading. File and compilation database reads run off the UI thread.
use super::*;
use crate::cpp::{Binding, Unit};
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::{Duration, SystemTime};

type Stamp = Option<(SystemTime, u64)>;
fn stamp(path: &Path) -> Stamp {
    let m = path.metadata().ok()?;
    Some((m.modified().ok()?, m.len()))
}
fn absolute(p: &Path) -> PathBuf {
    fs::canonicalize(p).map(strip_unc).unwrap_or_else(|_| p.to_owned())
}

#[derive(Default)]
struct Options {
    db: Option<(PathBuf, Stamp)>,
    matched: bool,
    macros: HashMap<String, Binding>,
    includes: Vec<PathBuf>,
    quotes: Vec<PathBuf>,
    system: Vec<PathBuf>,
    forced: Vec<PathBuf>,
}

impl Options {
    fn flags(&mut self, directory: &Path, file: &Path, args: &[String]) {
        let mut args = args.iter().map(String::as_str);
        while let Some(arg) = args.next() {
            for opt in ["-isystem", "-iquote", "-include", "-D", "-U", "-I", "/D", "/U", "/I"] {
                let Some(tail) = arg.strip_prefix(opt) else { continue };
                let value = if tail.is_empty() { args.next().unwrap_or("") } else { tail };
                if value.is_empty() {
                    break;
                }
                match opt {
                    "-D" | "/D" => {
                        let (n, v) = value.split_once('=').unwrap_or((value, "1"));
                        self.macros.insert(n.into(), Binding::Defined(v.into()));
                    }
                    "-U" | "/U" => {
                        self.macros.insert(value.into(), Binding::Undefined);
                    }
                    "-iquote" => self.quotes.push(directory.join(value)),
                    "-isystem" => self.system.push(directory.join(value)),
                    "-include" => {
                        let local = directory.join(value);
                        self.forced.push(if local.is_file() { local } else { PathBuf::from(value) });
                    }
                    _ => self.includes.push(directory.join(value)),
                }
                break;
            }
        }
        self.matched = true;
        self.language(file);
    }

    fn language(&mut self, file: &Path) {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "C" || matches!(ext.to_ascii_lowercase().as_str(), "cpp" | "cc" | "cxx" | "c++" | "cppm" | "ixx" | "ino") {
            // The exact numerical value depends on the selected C++ standard.
            self.macros.entry("__cplusplus".into()).or_insert_with(|| Binding::Defined("\0".into()));
        }
    }
}

// Decode common compile_commands command quoting without executing a shell. Preserve Windows paths.
fn arguments(command: &str) -> Vec<String> {
    let mut out = vec![];
    let mut arg = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if Some(c) == quote => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(c),
            '\\' if quote != Some('\'') && chars.peek().is_some_and(|n| *n == '"' || (quote.is_none() && n.is_whitespace())) => arg.push(chars.next().unwrap()),
            c if c.is_whitespace() && quote.is_none() => {
                if !arg.is_empty() {
                    out.push(std::mem::take(&mut arg));
                }
            }
            c => arg.push(c),
        }
    }
    if !arg.is_empty() {
        out.push(arg);
    }
    out
}

fn options(root: &Path, file: &Path, previous: &Arc<Options>, watch: &mut HashMap<PathBuf, Stamp>) -> Arc<Options> {
    let db = crate::lsp::DB_DIRS.iter().map(|dir| root.join(dir).join("compile_commands.json")).find_map(|p| {
        let s = stamp(&p);
        watch.insert(p.clone(), s);
        s.map(|_| (p, s))
    });
    if previous.db == db && db.is_some() {
        return previous.clone();
    }
    let mut opt = Options { db: db.clone(), ..Default::default() };
    opt.language(file);
    if let Some((p, Some((_, size)))) = db {
        if size <= 48 << 20 {
            #[derive(Deserialize)]
            struct Command {
                file: PathBuf,
                directory: PathBuf,
                #[serde(default)]
                arguments: Vec<String>,
                #[serde(default)]
                command: String,
            }
            if let Some(entries) = fs::File::open(&p).ok().and_then(|f| serde_json::from_reader::<_, Vec<Command>>(std::io::BufReader::new(f)).ok()) {
                for entry in entries {
                    if entry.file.file_name() != file.file_name() {
                        continue;
                    }
                    let dir = p.parent().unwrap_or(root).join(entry.directory);
                    if absolute(&dir.join(entry.file)) == file {
                        let args = if entry.arguments.is_empty() { arguments(&entry.command) } else { entry.arguments };
                        opt.flags(&dir, file, &args);
                        break;
                    }
                }
            }
        }
    }
    Arc::new(opt)
}

#[derive(Default, Clone)]
struct Inputs {
    options: Arc<Options>,
    headers: Arc<HashMap<PathBuf, (Stamp, Arc<str>)>>,
}

#[derive(Default)]
struct Analysis {
    ranges: Vec<(usize, usize)>,
    watch: Arc<HashMap<PathBuf, Stamp>>,
    inputs: Inputs,
}

fn analyze(path: PathBuf, text: String, index: Option<Arc<Index>>, mut live: HashMap<PathBuf, Arc<str>>, extra: String, prev: Inputs) -> Analysis {
    let mut watch = HashMap::new();
    let path = absolute(&path);
    let root = index.as_ref().map(|i| i.root.as_path()).unwrap_or_else(|| path.parent().unwrap_or(&path));
    let opt = options(root, &path, &prev.options, &mut watch);
    let mut unit = Unit::new(opt.macros.clone());
    unit.incomplete = !opt.matched;
    for (k, v) in super::gen::parse_defines(&extra) {
        unit.macros.insert(k, Binding::Defined(v));
    }
    live = live.into_iter().map(|(k, v)| (absolute(&k), v)).collect();
    let mut cache = HashMap::<PathBuf, (Stamp, Arc<str>)>::new();
    let mut remaining = 16 * 1024 * 1024u64;
    let mut resolve = |parent: &Path, name: &str, quoted: bool| -> Option<crate::cpp::Include> {
        let inc = Path::new(name);
        let mut paths = vec![];
        if inc.is_absolute() {
            paths.push(inc.to_owned());
        }
        if quoted {
            paths.push(parent.parent()?.join(inc));
            paths.extend(opt.quotes.iter().map(|d| d.join(inc)));
        }
        paths.extend(opt.includes.iter().chain(&opt.system).map(|d| d.join(inc)));
        if !opt.matched {
            paths.push(root.join(inc));
            if let Some(candidates) = index.as_ref().and_then(|i| i.include_paths.get(inc.file_name()?)) {
                let mut matches = candidates.iter().filter(|p| p.ends_with(inc));
                if let Some(only) = matches.next().filter(|_| matches.next().is_none()) {
                    paths.push(only.clone());
                }
            }
        }
        for p in paths {
            if let Some((_, text)) = cache.get(&p) {
                return Some((p, text.clone()));
            }
            let meta = *watch.entry(p.clone()).or_insert_with(|| stamp(&p));
            let Some((_, len)) = meta else { continue };
            let full = absolute(&p);
            if let Some(t) = live.get(&full) {
                return Some((full, t.clone()));
            }
            if len > (3 << 20) || len > remaining {
                return None;
            }
            let t = if let Some((_, t)) = prev.headers.get(&full).filter(|(s, _)| *s == meta) {
                t.clone()
            } else {
                Arc::<str>::from(crate::cpp::header_directives(&cindex::read_text(&full)?))
            };
            remaining -= len;
            cache.insert(full.clone(), (meta, t.clone()));
            return Some((full, t));
        }
        None
    };
    for f in &opt.forced {
        unit.include(&path, &f.to_string_lossy(), false, &mut resolve, true);
    }
    let ranges = unit.ranges(&path, &text, &mut resolve);
    Analysis {
        ranges,
        watch: Arc::new(watch),
        inputs: Inputs {
            options: opt,
            headers: Arc::new(cache),
        },
    }
}

enum Update {
    Ready(Analysis),
    Unchanged,
    Stale,
}

#[derive(Default)]
pub(super) struct Shading {
    displayed: Option<u64>,
    result: Analysis,
    worker: Option<(u64, Receiver<Update>)>,
    edit: Option<(u64, Instant)>,
    last_check: Option<Instant>,
}

impl CodeView {
    pub(super) fn inactive_of(&mut self, i: usize, ctx: &egui::Context) -> Vec<(usize, usize)> {
        let d = &self.tabs[i];
        if !self.s.dim_inactive || d.lang != Lang::C || d.buf.text.len() > 3_000_000 {
            return vec![];
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.buf.ver.hash(&mut h);
        d.path.hash(&mut h);
        self.index.as_ref().map(Arc::as_ptr).hash(&mut h);
        self.s.defines.hash(&mut h);
        for header in self.tabs.iter().filter(|d| d.dirty()) {
            header.path.hash(&mut h);
            header.buf.ver.hash(&mut h);
        }
        let key = h.finish();
        let now = Instant::now();
        let cache = &mut self.tabs[i].inactive;
        if let Some((k, rx)) = &cache.worker {
            match rx.try_recv() {
                Ok(upd) if *k == key => {
                    match upd {
                        Update::Ready(r) => {
                            cache.result = r;
                            cache.displayed = Some(key);
                        }
                        Update::Stale => cache.displayed = None,
                        Update::Unchanged => {}
                    }
                    cache.last_check = Some(now);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(60));
                    return if cache.displayed == Some(key) { cache.result.ranges.clone() } else { vec![] };
                }
                _ => {}
            }
            cache.worker = None;
        }
        if cache.displayed == Some(key) {
            let interval = Duration::from_secs(2);
            if !cache.result.watch.is_empty() && cache.last_check.is_none_or(|t| now.duration_since(t) >= interval) {
                let (tx, rx) = mpsc::channel();
                let watch = cache.result.watch.clone();
                cache.worker = Some((key, rx));
                std::thread::spawn(move || {
                    let changed = watch.iter().any(|(p, s)| stamp(p) != *s);
                    let _ = tx.send(if changed { Update::Stale } else { Update::Unchanged });
                });
                ctx.request_repaint_after(Duration::from_millis(80));
            } else if !cache.result.watch.is_empty() {
                ctx.request_repaint_after(interval);
            }
            return cache.result.ranges.clone();
        }
        if cache.edit.is_none_or(|(k, _)| k != key) {
            cache.edit = Some((key, now));
        }
        let delay = Duration::from_millis(130).saturating_sub(now.duration_since(cache.edit.unwrap().1));
        if !delay.is_zero() {
            ctx.request_repaint_after(delay);
            return vec![];
        }
        let (tx, rx) = mpsc::channel();
        let prev = cache.result.inputs.clone();
        cache.worker = Some((key, rx));
        let d = &self.tabs[i];
        let (path, text) = (d.path.clone(), d.buf.text.clone());
        let index = self.index.clone();
        let live = self
            .tabs
            .iter()
            .filter(|h| h.path != path && h.dirty() && h.buf.text.len() <= 3_000_000)
            .map(|h| (h.path.clone(), Arc::from(h.buf.text.as_str())))
            .collect();
        let extra = self.s.defines.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Update::Ready(analyze(path, text, index, live, extra, prev)));
            ctx.request_repaint();
        });
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_headers_follow_translation_unit_flags_and_unsaved_includes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = absolute(tmp.path());
        let source = root.join("main.cpp");
        fs::create_dir(root.join("board A")).unwrap();
        fs::create_dir(root.join("boardB")).unwrap();
        let header = root.join("board A/config.hpp");
        fs::write(&header, "#ifndef CONFIG_H\n#define CONFIG_H\n#include \"pin.h\"\n#define ENABLE PIN\n#endif\n").unwrap();
        fs::write(root.join("board A/pin.h"), "#define PIN 1").unwrap();
        fs::write(root.join("boardB/config.hpp"), "#ifndef CONFIG_H\n#define CONFIG_H\n#define ENABLE 0\n#endif").unwrap();
        let main = "#include <config.hpp>\n#if ENABLE && defined(BOARD) && defined(__cplusplus)\nuart_on();\n#else\nuart_off();\n#endif";
        fs::write(&source, main).unwrap();
        let db = serde_json::json!([{"file":"main.cpp","directory":root,"command":"g++ -DBOARD -I\"board A\" -IboardB -c main.cpp"}]);
        fs::write(root.join("compile_commands.json"), db.to_string()).unwrap();
        let index = Some(Arc::new(cindex::build(&root, &cindex::shared_progress())));
        let read = |live| analyze(source.clone(), main.into(), index.clone(), live, String::new(), Inputs::default());
        let disk = read(HashMap::new());
        assert_eq!(disk.ranges, [(4, 4)]);
        let live = HashMap::from([(root.join("board A/pin.h"), Arc::from("#define PIN 0"))]);
        assert_eq!(read(live).ranges, [(2, 2)]);
        assert!(disk.watch.contains_key(&root.join("board A/pin.h")));
        assert!(!disk.watch.contains_key(&root.join("boardB/config.hpp")));
        let h = analyze(header.clone(), fs::read_to_string(header).unwrap(), index, HashMap::new(), String::new(), Inputs::default());
        assert!(h.ranges.is_empty(), "another board's include guard must not grey this header");
    }

    #[test]
    fn missing_sdk_is_unknown_but_local_undef_and_constant_false_are_definite() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("main.c");
        let source = "#include <vendor.h>\n#ifdef SDK_ENABLED\na\n#else\nb\n#endif\n#undef OFF\n#ifdef OFF\nc\n#endif\n#if 0\nd\n#endif";
        let r = analyze(file, source.into(), None, HashMap::new(), String::new(), Inputs::default());
        assert_eq!(r.ranges, [(8, 8), (11, 11)]);
    }

    #[test]
    fn forced_headers_and_relative_include_loops_obey_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = absolute(dir.path());
        let file = root.join("unit.c");
        fs::write(&file, "").unwrap();
        fs::write(root.join("setup.h"), "#include \"setup.h\"\n#define BASE 7").unwrap();
        let args = ["cc", "-D", "OLD=1", "-UOLD", "-include", "setup.h", "unit.c"];
        let db = serde_json::json!([{"file":"unit.c","directory":root,"arguments":args}]);
        fs::write(root.join("compile_commands.json"), db.to_string()).unwrap();
        let source = "#if defined(OLD) || \\\n BASE != /* expected */ 7\noff\n#else\non\n#endif";
        let r = analyze(file, source.into(), None, HashMap::new(), String::new(), Inputs::default());
        assert_eq!(r.ranges, [(1, 2)]);
    }
}

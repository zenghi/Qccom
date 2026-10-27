//! 可选的 clangd 支持。电脑上装了 clangd、工程里又有 compile_commands.json 时，
//! 跳转定义交给 clangd（能区分同名的 static 函数、看懂宏和类型）；
//! 否则继续用内置索引。这里只实现用到的几条 LSP 消息，全部异步，不卡界面。

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

type Pending = Arc<Mutex<HashMap<i64, Sender<Value>>>>;

pub struct Lsp {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next: AtomicI64,
    pending: Pending,
    ready: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
    /// 已经告诉 clangd 的文件 → 发过去的内容版本
    open: HashMap<PathBuf, (u64, i64)>,
    pub exe: PathBuf,
}

/// 找 clangd：用户指定的路径 → PATH 里的 clangd → clangd-NN（Debian/Ubuntu）→ Windows 下 LLVM 默认安装目录
pub fn find_clangd(custom: &str) -> Option<PathBuf> {
    let custom = custom.trim();
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        return p.is_file().then_some(p);
    }
    let exe = if cfg!(windows) { "clangd.exe" } else { "clangd" };
    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(p) = dirs.iter().map(|d| d.join(exe)).find(|p| p.is_file()) {
        return Some(p);
    }
    if cfg!(windows) {
        ["C:\\Program Files\\LLVM\\bin", "C:\\Program Files (x86)\\LLVM\\bin"]
            .iter()
            .map(|b| Path::new(b).join(exe))
            .find(|p| p.is_file())
    } else {
        (10..=30).rev().flat_map(|v| dirs.iter().map(move |d| d.join(format!("clangd-{v}")))).find(|p| p.is_file())
    }
}

/// compile_commands.json 所在目录（工程根或常见的构建目录）
pub fn compile_db(root: &Path) -> Option<PathBuf> {
    ["", "build", "out", "cmake-build-debug", "cmake-build-release", ".build", "builddir"]
        .iter()
        .map(|d| root.join(d))
        .find(|d| d.join("compile_commands.json").is_file())
}

pub fn to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn from_uri(u: &str) -> Option<PathBuf> {
    let rest = u.strip_prefix("file://")?;
    let b = rest.as_bytes();
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                v.push(x);
                i += 3;
                continue;
            }
        }
        v.push(b[i]);
        i += 1;
    }
    let s = String::from_utf8_lossy(&v).to_string();
    // Windows：/C:/x → C:/x
    let s = if s.len() > 2 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s[1..].to_string()
    } else {
        s
    };
    Some(PathBuf::from(s))
}

/// 一行里的字节偏移 ↔ LSP 的 UTF-16 列号
pub fn utf16_col(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())].encode_utf16().count()
}

pub fn byte_col(line: &str, col16: usize) -> usize {
    let mut n = 0;
    for (i, c) in line.char_indices() {
        if n >= col16 {
            return i;
        }
        n += c.len_utf16();
    }
    line.len()
}

fn lang_id(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("c") => "c",
        Some("h") => "c",
        _ => "cpp",
    }
}

impl Lsp {
    pub fn start(exe: &Path, root: &Path, db: Option<&Path>) -> std::io::Result<Lsp> {
        let mut c = Command::new(exe);
        c.args(["--background-index", "--log=error", "--header-insertion=never", "--pch-storage=memory"]);
        if let Some(d) = db {
            c.arg(format!("--compile-commands-dir={}", d.display()));
        }
        c.current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = c.spawn()?;
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Arc::default();
        let ready = Arc::new(AtomicBool::new(false));
        let dead = Arc::new(AtomicBool::new(false));
        {
            let (pending, dead) = (pending.clone(), dead.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(stdout);
                while let Some(msg) = read_msg(&mut r) {
                    if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                        if msg.get("method").is_none() {
                            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                                let _ = tx.send(msg.get("result").cloned().unwrap_or(Value::Null));
                            }
                        }
                    }
                }
                dead.store(true, Ordering::Relaxed);
                pending.lock().unwrap().clear();
            });
        }
        let lsp = Lsp {
            child,
            stdin,
            next: AtomicI64::new(1),
            pending,
            ready: ready.clone(),
            dead,
            open: HashMap::new(),
            exe: exe.to_path_buf(),
        };
        let rx = lsp.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": to_uri(root),
                "capabilities": {
                    "textDocument": { "definition": { "linkSupport": false }, "synchronization": { "didSave": false } },
                    "general": { "positionEncodings": ["utf-16"] }
                },
                "clientInfo": { "name": "QCCOM", "version": env!("CARGO_PKG_VERSION") }
            }),
        );
        let stdin = lsp.stdin.clone();
        std::thread::spawn(move || {
            if rx.recv_timeout(std::time::Duration::from_secs(20)).is_ok() {
                let _ = write_msg(&stdin, &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
                ready.store(true, Ordering::Relaxed);
            }
        });
        Ok(lsp)
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed) && !self.dead.load(Ordering::Relaxed)
    }

    pub fn dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = write_msg(&self.stdin, &json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn request(&self, method: &str, params: Value) -> Receiver<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _ = write_msg(&self.stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        rx
    }

    /// 把编辑器里的内容同步给 clangd（第一次 didOpen，之后内容变了就整份 didChange）
    pub fn sync(&mut self, path: &Path, ver: u64, text: &str) {
        let uri = to_uri(path);
        match self.open.get_mut(path) {
            None => {
                self.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": lang_id(path), "version": 1, "text": text}}));
                self.open.insert(path.to_path_buf(), (ver, 1));
            }
            Some((v, n)) if *v != ver => {
                *v = ver;
                *n += 1;
                let n = *n;
                self.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": n}, "contentChanges": [{"text": text}]}));
            }
            _ => {}
        }
    }

    pub fn close(&mut self, path: &Path) {
        if self.open.remove(path).is_some() {
            self.notify("textDocument/didClose", json!({"textDocument": {"uri": to_uri(path)}}));
        }
    }

    /// 跳转定义；line 从 0 开始，col 是 UTF-16 列
    pub fn definition(&self, path: &Path, line: usize, col16: usize) -> Receiver<Value> {
        self.request(
            "textDocument/definition",
            json!({"textDocument": {"uri": to_uri(path)}, "position": {"line": line, "character": col16}}),
        )
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        if !self.dead() {
            let _ = self.request("shutdown", Value::Null);
            self.notify("exit", Value::Null);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// definition 的回复（Location / Location[] / LocationLink[]）→ (文件, 行, UTF-16 列)
pub fn locations(v: &Value) -> Vec<(PathBuf, usize, usize)> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => vec![],
    };
    items
        .into_iter()
        .filter_map(|it| {
            let uri = it.get("uri").or_else(|| it.get("targetUri"))?.as_str()?;
            let range = it.get("range").or_else(|| it.get("targetSelectionRange"))?;
            let start = range.get("start")?;
            Some((from_uri(uri)?, start.get("line")?.as_u64()? as usize, start.get("character")?.as_u64()? as usize))
        })
        .collect()
}

fn write_msg(w: &Mutex<ChildStdin>, v: &Value) -> std::io::Result<()> {
    let body = v.to_string();
    let mut w = w.lock().unwrap();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()
}

fn read_msg(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip() {
        let p = Path::new("/tmp/固件 工程/src/main.c");
        let u = to_uri(p);
        assert!(u.starts_with("file:///tmp/"));
        assert!(!u.contains(' '));
        assert_eq!(from_uri(&u).unwrap(), p);
        assert_eq!(from_uri("file:///C:/fw/main.c").unwrap(), PathBuf::from("C:/fw/main.c"));
    }

    #[test]
    fn cols() {
        let l = "a中😀b";
        assert_eq!(utf16_col(l, l.find('b').unwrap()), 4);
        assert_eq!(byte_col(l, 4), l.find('b').unwrap());
    }

    #[test]
    fn parse_locations() {
        let v = json!([{"uri": "file:///x/a.c", "range": {"start": {"line": 3, "character": 5}, "end": {"line": 3, "character": 9}}}]);
        assert_eq!(locations(&v), vec![(PathBuf::from("/x/a.c"), 3, 5)]);
    }
}

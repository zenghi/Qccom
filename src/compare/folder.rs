//! 文件夹比较：两边一起遍历，按相对路径对齐；按大小 + 修改时间快速比较，或逐字节比较内容。
//! 也负责复制 / 删除 / 同步这些文件操作。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub size: u64,
    /// 修改时间，秒
    pub mtime: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum St {
    Same,
    Diff,
    LeftOnly,
    RightOnly,
    /// 大小一样、时间不同，等内容比较
    Pending,
    Error,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// 用 / 分隔的相对路径
    pub rel: String,
    pub name: String,
    pub depth: usize,
    pub dir: bool,
    pub l: Option<Meta>,
    pub r: Option<Meta>,
    pub st: St,
    /// 文件夹里各状态的数量：不同、只在左、只在右
    pub counts: (usize, usize, usize),
}

impl Entry {
    /// 哪边更新：-1 左边、1 右边、0 一样或不适用
    pub fn newer(&self) -> i8 {
        match (self.l, self.r) {
            (Some(a), Some(b)) if !self.dir && self.st != St::Same => {
                // FAT 文件系统时间精度 2 秒
                if a.mtime > b.mtime + 2 {
                    -1
                } else if b.mtime > a.mtime + 2 {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        }
    }
}

/// 过滤规则：`build/` 匹配文件夹名，`*.o` 匹配文件名，`-*.c`（减号开头）强制保留
#[derive(Clone, Default)]
pub struct Filter {
    dirs: Vec<String>,
    files: Vec<String>,
}

impl Filter {
    pub fn parse(s: &str) -> Filter {
        let mut f = Filter::default();
        for p in s.split([';', '\n', ',']).map(str::trim).filter(|p| !p.is_empty()) {
            if let Some(d) = p.strip_suffix('/').or_else(|| p.strip_suffix('\\')) {
                f.dirs.push(d.to_lowercase());
            } else {
                f.files.push(p.to_lowercase());
            }
        }
        f
    }

    pub fn skip_dir(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        self.dirs.iter().any(|d| glob(d, &n))
    }

    pub fn skip_file(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        self.files.iter().any(|d| glob(d, &n))
    }
}

/// 简单通配符：* 任意多个字符，? 一个字符
pub fn glob(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn meta(p: &Path) -> Option<(bool, Meta)> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Some((
        m.is_dir(),
        Meta {
            size: if m.is_dir() { 0 } else { m.len() },
            mtime,
        },
    ))
}

fn list(dir: &Path) -> BTreeMap<String, (String, bool, Meta)> {
    let mut m = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some((is_dir, me)) = meta(&e.path()) {
                // 键：文件夹在前，再按不分大小写的名字排
                let key = format!("{}{}", if is_dir { '0' } else { '1' }, name.to_lowercase());
                m.insert(key, (name, is_dir, me));
            }
        }
    }
    m
}

/// 两边一起遍历（深度优先，文件夹在前），得到对齐后的条目
pub fn scan(left: &Path, right: &Path, f: &Filter) -> Vec<Entry> {
    let mut out = vec![];
    walk(left, right, "", 0, f, &mut out, &mut 0);
    out
}

fn walk(left: &Path, right: &Path, rel: &str, depth: usize, f: &Filter, out: &mut Vec<Entry>, budget: &mut usize) {
    *budget += 1;
    if *budget > 200_000 || depth > 64 {
        return;
    }
    let la = list(&left.join(rel));
    let lb = list(&right.join(rel));
    let mut keys: Vec<&String> = la.keys().chain(lb.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let a = la.get(k);
        let b = lb.get(k);
        let (name, dir) = a.or(b).map(|(n, d, _)| (n.clone(), *d)).unwrap();
        if (dir && f.skip_dir(&name)) || (!dir && f.skip_file(&name)) {
            continue;
        }
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let idx = out.len();
        out.push(Entry {
            rel: r.clone(),
            name,
            depth,
            dir,
            l: a.map(|x| x.2),
            r: b.map(|x| x.2),
            st: St::Same,
            counts: (0, 0, 0),
        });
        if dir {
            walk(left, right, &r, depth + 1, f, out, budget);
        }
        let e = &mut out[idx];
        e.st = match (a, b) {
            (Some(_), None) => St::LeftOnly,
            (None, Some(_)) => St::RightOnly,
            _ if dir => St::Same,
            (Some(x), Some(y)) => {
                if x.2.size != y.2.size {
                    St::Diff
                } else if (x.2.mtime - y.2.mtime).abs() <= 2 {
                    St::Same
                } else {
                    St::Pending
                }
            }
            _ => St::Same,
        };
    }
}

/// 汇总文件夹状态：里面有任何不同就是不同
pub fn rollup(es: &mut [Entry]) {
    // 从后往前，子条目一定在父条目后面
    for i in (0..es.len()).rev() {
        if !es[i].dir {
            continue;
        }
        let depth = es[i].depth;
        let mut c = (0, 0, 0);
        let mut j = i + 1;
        while j < es.len() && es[j].depth > depth {
            if !es[j].dir {
                match es[j].st {
                    St::Diff | St::Pending | St::Error => c.0 += 1,
                    St::LeftOnly => c.1 += 1,
                    St::RightOnly => c.2 += 1,
                    St::Same => {}
                }
            }
            j += 1;
        }
        es[i].counts = c;
        if es[i].l.is_some() && es[i].r.is_some() {
            es[i].st = if c == (0, 0, 0) { St::Same } else { St::Diff };
        }
    }
}

/// 逐字节比较两个文件
pub fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    let mut fa = std::fs::File::open(a)?;
    let mut fb = std::fs::File::open(b)?;
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

// ------------------------------------------------------------------ 文件操作

/// 复制文件或整个文件夹（保留修改时间）；返回复制的文件数
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut n = 0;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            n += copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(n)
    } else {
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(from, to)?;
        if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
            if let Ok(f) = std::fs::File::options().write(true).open(to) {
                let _ = f.set_modified(t);
            }
        }
        Ok(1)
    }
}

pub fn remove(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// 同步方式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sync {
    /// 把左边新的、只在左边的复制到右边
    UpdateRight,
    UpdateLeft,
    /// 左右互相补齐，各取更新的
    Both,
    /// 右边变得和左边一模一样（会删除只在右边的）
    MirrorRight,
    MirrorLeft,
}

/// 一步同步操作
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// (相对路径, 从左到右)
    Copy(String, bool),
    /// (相对路径, 删左边)
    Delete(String, bool),
}

/// 根据比较结果算出同步要做的操作（只看文件和整个只在一边的文件夹）
pub fn plan(es: &[Entry], mode: Sync) -> Vec<Op> {
    let mut ops = vec![];
    let mut skip_under: Option<String> = None;
    for e in es {
        if let Some(p) = &skip_under {
            if e.rel.starts_with(p.as_str()) && e.rel.as_bytes().get(p.len()) == Some(&b'/') {
                continue;
            }
            skip_under = None;
        }
        let whole = e.dir && matches!(e.st, St::LeftOnly | St::RightOnly);
        if e.dir && !whole {
            continue;
        }
        let to_r = |ops: &mut Vec<Op>| ops.push(Op::Copy(e.rel.clone(), true));
        let to_l = |ops: &mut Vec<Op>| ops.push(Op::Copy(e.rel.clone(), false));
        match (mode, e.st) {
            (_, St::Same) => {}
            (Sync::UpdateRight | Sync::MirrorRight | Sync::Both, St::LeftOnly) => to_r(&mut ops),
            (Sync::UpdateLeft | Sync::MirrorLeft | Sync::Both, St::RightOnly) => to_l(&mut ops),
            (Sync::MirrorRight, St::RightOnly) => ops.push(Op::Delete(e.rel.clone(), false)),
            (Sync::MirrorLeft, St::LeftOnly) => ops.push(Op::Delete(e.rel.clone(), true)),
            (Sync::MirrorRight, St::Diff | St::Pending | St::Error) => to_r(&mut ops),
            (Sync::MirrorLeft, St::Diff | St::Pending | St::Error) => to_l(&mut ops),
            (Sync::UpdateRight, St::Diff | St::Pending) if e.newer() <= 0 => to_r(&mut ops),
            (Sync::UpdateLeft, St::Diff | St::Pending) if e.newer() >= 0 => to_l(&mut ops),
            (Sync::Both, St::Diff | St::Pending) => match e.newer() {
                -1 => to_r(&mut ops),
                1 => to_l(&mut ops),
                _ => {}
            },
            _ => {}
        }
        if whole {
            skip_under = Some(e.rel.clone());
        }
    }
    ops
}

pub fn apply(left: &Path, right: &Path, op: &Op) -> std::io::Result<()> {
    let rel = match op {
        Op::Copy(r, _) | Op::Delete(r, _) => r,
    };
    let (l, r) = (join(left, rel), join(right, rel));
    match op {
        Op::Copy(_, true) => copy_tree(&l, &r).map(|_| ()),
        Op::Copy(_, false) => copy_tree(&r, &l).map(|_| ()),
        Op::Delete(_, true) => remove(&l),
        Op::Delete(_, false) => remove(&r),
    }
}

pub fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qccom-fold-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn globbing() {
        assert!(glob("*.o", "main.o"));
        assert!(!glob("*.o", "main.c"));
        assert!(glob("build*", "build-debug"));
        assert!(glob("a?c", "abc"));
        let f = Filter::parse(".git/; build/ ;*.o");
        assert!(f.skip_dir(".git") && f.skip_dir("BUILD") && f.skip_file("x.O") && !f.skip_file("x.c"));
    }

    #[test]
    fn scan_and_sync() {
        let a = tmp("a");
        let b = tmp("b");
        std::fs::write(a.join("same.c"), "x").unwrap();
        std::fs::write(b.join("same.c"), "x").unwrap();
        std::fs::write(a.join("diff.c"), "left").unwrap();
        std::fs::write(b.join("diff.c"), "right!").unwrap();
        std::fs::create_dir_all(a.join("src/sub")).unwrap();
        std::fs::write(a.join("src/sub/only.h"), "h").unwrap();
        std::fs::create_dir_all(b.join("src")).unwrap();
        std::fs::write(b.join("src/r.txt"), "r").unwrap();
        std::fs::create_dir_all(a.join("build")).unwrap();
        std::fs::write(a.join("build/x.o"), "o").unwrap();
        let f = Filter::parse("build/");
        let mut es = scan(&a, &b, &f);
        rollup(&mut es);
        let st = |rel: &str| es.iter().find(|e| e.rel == rel).map(|e| e.st);
        assert_eq!(st("same.c"), Some(St::Same));
        assert_eq!(st("diff.c"), Some(St::Diff));
        assert_eq!(st("src/sub"), Some(St::LeftOnly));
        assert_eq!(st("src/r.txt"), Some(St::RightOnly));
        assert_eq!(st("src"), Some(St::Diff));
        assert_eq!(st("build"), None);
        // 文件夹排在文件前面
        assert_eq!(es[0].rel, "src");
        let ops = plan(&es, Sync::MirrorRight);
        assert!(ops.contains(&Op::Copy("src/sub".into(), true)));
        assert!(ops.contains(&Op::Delete("src/r.txt".into(), false)));
        assert!(!ops.iter().any(|o| matches!(o, Op::Copy(p, _) if p == "src/sub/only.h")));
        for o in &ops {
            apply(&a, &b, o).unwrap();
        }
        let mut es = scan(&a, &b, &f);
        rollup(&mut es);
        assert!(es.iter().all(|e| e.st == St::Same), "{es:?}");
        assert!(same_content(&a.join("diff.c"), &b.join("diff.c")).unwrap());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}

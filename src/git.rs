//! Git 数据：调用系统里的 git 命令行（体积为零、兼容所有仓库特性），解析成界面要的结构。
//! 全部在后台线程里跑，界面不会卡。

use crate::i18n::{t, tf};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("git");
    c.env("GIT_TERMINAL_PROMPT", "0"); // 需要密码时直接失败，不要卡住等输入
    c.arg("-C").arg(root).args(["-c", "core.quotepath=off", "-c", "color.ui=never"]).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：不弹黑框
    }
    let out = c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            t("没有找到 git 命令。请先安装 Git（https://git-scm.com），安装后重启本程序").to_string()
        } else {
            e.to_string()
        }
    })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if e.is_empty() { tf("git {} 失败", &[&args.first().unwrap_or(&"")]) } else { e })
    }
}

pub fn find_root(dir: &Path) -> Result<PathBuf, String> {
    let s = git(dir, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(s.trim()))
}

#[derive(Clone, Debug, Default)]
pub struct Branch {
    /// 显示名 main / origin/main / v1.0
    pub name: String,
    pub kind: RefKind,
    pub head: bool,
    pub hash: String,
    pub upstream: String,
    pub ahead: u32,
    pub behind: u32,
    pub gone: bool,
    pub time: i64,
    pub subject: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RefKind {
    #[default]
    Local,
    Remote,
    Tag,
}

pub fn branches(root: &Path) -> Result<Vec<Branch>, String> {
    let fmt = "%(refname)%00%(refname:short)%00%(HEAD)%00%(objectname:short)%00%(upstream:short)%00%(upstream:track)%00%(committerdate:unix)%00%(contents:subject)";
    let out = git(root, &["for-each-ref", &format!("--format={fmt}"), "--sort=-committerdate", "refs/heads", "refs/remotes", "refs/tags"])?;
    let mut v = vec![];
    for l in out.lines() {
        let f: Vec<&str> = l.split('\0').collect();
        if f.len() < 8 || f[0].ends_with("/HEAD") {
            continue;
        }
        let kind = if f[0].starts_with("refs/heads/") {
            RefKind::Local
        } else if f[0].starts_with("refs/tags/") {
            RefKind::Tag
        } else {
            RefKind::Remote
        };
        let (ahead, behind, gone) = parse_track(f[5]);
        v.push(Branch {
            name: f[1].into(),
            kind,
            head: f[2] == "*",
            hash: f[3].into(),
            upstream: f[4].into(),
            ahead,
            behind,
            gone,
            time: f[6].parse().unwrap_or(0),
            subject: f[7].into(),
        });
    }
    Ok(v)
}

/// "[ahead 2, behind 1]" / "[gone]"
fn parse_track(s: &str) -> (u32, u32, bool) {
    let num = |k: &str| s.find(k).and_then(|i| s[i + k.len()..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).unwrap_or(0);
    (num("ahead "), num("behind "), s.contains("gone"))
}

#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub refs: Vec<String>,
    pub subject: String,
}

/// scope 为 None 时看所有分支，否则只看某个引用能到达的提交
pub fn log(root: &Path, scope: Option<&str>, limit: usize, grep: &str) -> Result<Vec<Commit>, String> {
    let n = format!("-n{limit}");
    let mut args = vec!["log", "--date-order", "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%D%x00%s", n.as_str()];
    let g;
    if !grep.trim().is_empty() {
        g = format!("--grep={}", grep.trim());
        args.extend(["-i", g.as_str()]);
    }
    match scope {
        Some(s) => args.push(s),
        None => args.extend(["--branches", "--remotes", "--tags", "HEAD"]),
    }
    let out = git(root, &args)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(7, '\0').collect();
            (f.len() == 7).then(|| Commit {
                hash: f[0].into(),
                parents: f[1].split_whitespace().map(String::from).collect(),
                author: f[2].into(),
                email: f[3].into(),
                time: f[4].parse().unwrap_or(0),
                refs: f[5].split(", ").filter(|s| !s.is_empty()).map(String::from).collect(),
                subject: f[6].into(),
            })
        })
        .collect())
}

// ---------------------------------------------------------------- 提交图谱

#[derive(Clone, Debug, Default)]
pub struct GraphRow {
    /// 这个提交所在的车道
    pub lane: usize,
    /// 从上方穿过、不经过本提交的车道
    pub pass: Vec<usize>,
    /// 从上方汇入本提交的车道
    pub into: Vec<usize>,
    /// 从本提交往下连到父提交的车道
    pub out: Vec<usize>,
    pub width: usize,
}

pub fn graph(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<&str>> = vec![];
    let mut rows = Vec::with_capacity(commits.len());
    for c in commits {
        let mut row = GraphRow::default();
        let mine: Vec<usize> = lanes.iter().enumerate().filter(|(_, l)| **l == Some(c.hash.as_str())).map(|(i, _)| i).collect();
        let lane = match mine.first() {
            Some(&i) => i,
            None => match lanes.iter().position(Option::is_none) {
                Some(i) => i,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        row.lane = lane;
        row.into = mine.clone();
        for (i, l) in lanes.iter().enumerate() {
            if l.is_some() && !mine.contains(&i) {
                row.pass.push(i);
            }
        }
        for &i in &mine {
            lanes[i] = None;
        }
        // 父提交：第一个父亲沿用本车道，其余的找已有车道或开新车道
        for (k, p) in c.parents.iter().enumerate() {
            let target = if let Some(i) = lanes.iter().position(|l| *l == Some(p.as_str())) {
                i
            } else if k == 0 {
                lanes[lane] = Some(p.as_str());
                lane
            } else {
                match lanes.iter().position(Option::is_none) {
                    Some(i) => {
                        lanes[i] = Some(p.as_str());
                        i
                    }
                    None => {
                        lanes.push(Some(p.as_str()));
                        lanes.len() - 1
                    }
                }
            };
            row.out.push(target);
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        row.width = lanes.len().max(lane + 1).max(row.pass.iter().chain(&row.into).max().map_or(0, |m| m + 1));
        rows.push(row);
    }
    rows
}

// ---------------------------------------------------------------- 提交详情 / 差异

#[derive(Clone, Debug, Default)]
pub struct FileChange {
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
    pub adds: Option<u32>,
    pub dels: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct CommitInfo {
    pub body: String,
    pub files: Vec<FileChange>,
}

pub fn commit_info(root: &Path, hash: &str) -> Result<CommitInfo, String> {
    let body = git(root, &["show", "-s", "--format=%B", hash])?;
    // --root 让第一个提交也能列出文件；-m --first-parent 让合并提交和第一个父亲比较
    let ns = git(root, &["show", "--format=", "--numstat", "-M", "--first-parent", "-m", hash])?;
    let st = git(root, &["show", "--format=", "--name-status", "-M", "--first-parent", "-m", hash])?;
    let mut files: Vec<FileChange> = st
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let s = f.first()?.chars().next()?;
            Some(if f.len() >= 3 {
                FileChange {
                    status: s,
                    path: f[2].into(),
                    old_path: Some(f[1].into()),
                    ..Default::default()
                }
            } else {
                FileChange {
                    status: s,
                    path: f.get(1)?.to_string(),
                    ..Default::default()
                }
            })
        })
        .collect();
    for l in ns.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        // 改名时 numstat 的路径是 "a => b" 或 "dir/{a => b}"
        let p = f[2];
        if let Some(fc) = files.iter_mut().find(|fc| fc.path == p || (p.contains("=>") && p.ends_with(fc.path.rsplit('/').next().unwrap_or("")))) {
            fc.adds = f[0].parse().ok();
            fc.dels = f[1].parse().ok();
        }
    }
    Ok(CommitInfo { body: body.trim().into(), files })
}

pub fn commit_diff(root: &Path, hash: &str, path: &str) -> Result<String, String> {
    git(root, &["show", "--format=", "-M", "--first-parent", "-m", "-U3", hash, "--", path])
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub branch: String,
    pub files: Vec<StatusFile>,
}

#[derive(Clone, Debug)]
pub struct StatusFile {
    /// 暂存区状态、工作区状态（porcelain 的 XY）
    pub x: char,
    pub y: char,
    pub path: String,
}

pub fn status(root: &Path) -> Result<Status, String> {
    let out = git(root, &["status", "--porcelain=v1", "-b", "-uall"])?;
    let mut s = Status::default();
    for l in out.lines() {
        if let Some(b) = l.strip_prefix("## ") {
            s.branch = b.into();
            continue;
        }
        let mut ch = l.chars();
        let (Some(x), Some(y)) = (ch.next(), ch.next()) else { continue };
        let p = l.get(3..).unwrap_or("");
        let p = p.rsplit(" -> ").next().unwrap_or(p).trim_matches('"');
        s.files.push(StatusFile { x, y, path: p.into() });
    }
    Ok(s)
}

pub fn worktree_diff(root: &Path, f: &StatusFile) -> Result<String, String> {
    if f.x == '?' {
        // 未跟踪文件：显示全部内容为新增
        let text = crate::cindex::read_text(&root.join(&f.path)).unwrap_or_else(|| t("（二进制文件）").into());
        let mut d = tf("@@ -0,0 +1,{} @@ 新文件\n", &[&text.lines().count()]);
        for l in text.lines().take(5000) {
            d.push('+');
            d.push_str(l);
            d.push('\n');
        }
        return Ok(d);
    }
    let staged = git(root, &["diff", "--cached", "-M", "--", &f.path])?;
    let work = git(root, &["diff", "-M", "--", &f.path])?;
    Ok(format!("{staged}{work}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    Ctx,
    Add,
    Del,
    Hunk,
    Meta,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

pub fn parse_diff(d: &str) -> Vec<DiffLine> {
    let mut v = vec![];
    let (mut o, mut n) = (0u32, 0u32);
    for l in d.lines() {
        if l.starts_with("@@") {
            // @@ -12,7 +12,8 @@ fn foo
            let num = |pfx: char| {
                l.split_whitespace()
                    .find(|w| w.starts_with(pfx))
                    .and_then(|w| w[1..].split(',').next()?.parse::<u32>().ok())
                    .unwrap_or(1)
            };
            o = num('-');
            n = num('+');
            v.push(DiffLine {
                kind: DiffKind::Hunk,
                old: None,
                new: None,
                text: l.into(),
            });
        } else if l.starts_with("diff --git")
            || l.starts_with("index ")
            || l.starts_with("--- ")
            || l.starts_with("+++ ")
            || l.starts_with("new file")
            || l.starts_with("deleted file")
            || l.starts_with("similarity")
            || l.starts_with("rename ")
            || l.starts_with("old mode")
            || l.starts_with("new mode")
        {
            if l.starts_with("diff --git") && !v.is_empty() {
                v.push(DiffLine {
                    kind: DiffKind::Meta,
                    old: None,
                    new: None,
                    text: String::new(),
                });
            }
            if l.starts_with("rename") || l.starts_with("new file") || l.starts_with("deleted file") {
                v.push(DiffLine {
                    kind: DiffKind::Meta,
                    old: None,
                    new: None,
                    text: l.into(),
                });
            }
        } else if l.starts_with("Binary files") {
            v.push(DiffLine {
                kind: DiffKind::Meta,
                old: None,
                new: None,
                text: t("二进制文件，不显示差异").into(),
            });
        } else if let Some(t) = l.strip_prefix('+') {
            v.push(DiffLine {
                kind: DiffKind::Add,
                old: None,
                new: Some(n),
                text: t.into(),
            });
            n += 1;
        } else if let Some(t) = l.strip_prefix('-') {
            v.push(DiffLine {
                kind: DiffKind::Del,
                old: Some(o),
                new: None,
                text: t.into(),
            });
            o += 1;
        } else if l.starts_with('\\') {
            // \ No newline at end of file
        } else {
            v.push(DiffLine {
                kind: DiffKind::Ctx,
                old: Some(o),
                new: Some(n),
                text: l.strip_prefix(' ').unwrap_or(l).into(),
            });
            o += 1;
            n += 1;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(h: &str, p: &[&str]) -> Commit {
        Commit {
            hash: h.into(),
            parents: p.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn graph_merge() {
        // m 合并了 a 和 f；f 从 b 分出来
        let cs = vec![c("m", &["a", "f"]), c("f", &["b"]), c("a", &["b"]), c("b", &[])];
        let g = graph(&cs);
        assert_eq!(g[0].lane, 0);
        assert_eq!(g[0].out, vec![0, 1]);
        assert_eq!(g[1].lane, 1);
        assert_eq!(g[1].pass, vec![0]);
        assert_eq!(g[2].lane, 0);
        // a 的父亲 b 已经在车道 1 上等着，a 斜着连过去
        assert_eq!(g[2].out, vec![1]);
        assert_eq!(g[3].lane, 1);
    }

    #[test]
    fn track() {
        assert_eq!(parse_track("[ahead 2, behind 13]"), (2, 13, false));
        assert_eq!(parse_track("[behind 1]"), (0, 1, false));
        assert_eq!(parse_track("[gone]"), (0, 0, true));
    }

    #[test]
    fn diff_numbers() {
        let d = parse_diff("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn\n ctx\n-old\n+new\n+more\n ctx2\n");
        let add: Vec<_> = d.iter().filter(|l| l.kind == DiffKind::Add).map(|l| l.new.unwrap()).collect();
        assert_eq!(add, vec![11, 12]);
        assert_eq!(d.iter().find(|l| l.kind == DiffKind::Del).unwrap().old, Some(11));
    }

    #[test]
    fn real_repo() {
        // 在临时目录建一个小仓库，确认整套命令能跑通
        let dir = std::env::temp_dir().join(format!("qccom-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |a: &[&str]| git(&dir, a).unwrap();
        if git(&dir, &["init", "-q", "-b", "main"]).is_err() {
            return; // 没装 git 的环境跳过
        }
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "tester"]);
        std::fs::write(dir.join("a.c"), "int a;\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        run(&["checkout", "-q", "-b", "dev"]);
        std::fs::write(dir.join("a.c"), "int a;\nint b;\n").unwrap();
        run(&["commit", "-q", "-am", "second"]);
        std::fs::write(dir.join("new.txt"), "x\n").unwrap();
        let bs = branches(&dir).unwrap();
        assert!(bs.iter().any(|b| b.name == "dev" && b.head));
        let l = log(&dir, None, 100, "").unwrap();
        assert_eq!(l.len(), 2);
        assert!(l[0].refs.iter().any(|r| r.contains("dev")));
        let info = commit_info(&dir, &l[0].hash).unwrap();
        assert_eq!(info.files[0].path, "a.c");
        assert_eq!(info.files[0].adds, Some(1));
        let st = status(&dir).unwrap();
        assert!(st.files.iter().any(|f| f.path == "new.txt" && f.x == '?'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Git 写操作的辅助（v1.8）：提交前检查、提交说明草稿、下一个版本标签、子模块状态、固件体积历史。
//! 都是调用 git 命令行的纯函数，界面在 gitview.rs，后台线程里调用这里。

use std::path::{Path, PathBuf};

use crate::git::git;
use crate::i18n::{t, tf};

/// 提交前检查发现的问题。level：0 提示，1 警告，2 应该先处理
#[derive(Clone, Debug)]
pub struct Issue {
    pub level: u8,
    pub text: String,
}

const BUILD_EXTS: &[&str] = &["o", "obj", "elf", "axf", "out", "hex", "bin", "map", "lst", "d", "crf", "lnp"];

/// 暂存区里的文件：(状态字母, 路径)
pub fn staged(root: &Path) -> Vec<(char, String)> {
    let Ok(out) = git(root, &["diff", "--cached", "--name-status", "--no-renames"]) else {
        return vec![];
    };
    out.lines()
        .filter_map(|l| {
            let (s, p) = l.split_once('\t')?;
            Some((s.chars().next()?, p.trim_matches('"').to_string()))
        })
        .collect()
}

/// 看起来像密码、密钥的内容
fn secret_in(text: &str) -> Option<&'static str> {
    use std::sync::OnceLock;
    static RES: OnceLock<Vec<(regex_lite::Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            (r"-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----", "私钥"),
            (r#"(?i)(password|passwd|pwd|secret|token|api_?key|access_?key)\s*[:=]\s*["'][^"'\s]{6,}["']"#, "密码或密钥"),
            (r"AKIA[0-9A-Z]{16}", "AWS 密钥"),
            (r"gh[pousr]_[A-Za-z0-9]{30,}", "GitHub 令牌"),
            (r"(?i)wifi_?pass(word)?\s*[:=]?\s*\x22[^\x22]{6,}\x22", "WiFi 密码"),
        ]
        .into_iter()
        .filter_map(|(p, n)| regex_lite::Regex::new(p).ok().map(|r| (r, n)))
        .collect()
    });
    res.iter().find(|(r, _)| r.is_match(text)).map(|(_, n)| *n)
}

fn conflict_marker(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======")
}

/// 提交前检查。unsaved：编辑器里还没保存的文件；sensitive：敏感文件规则（.gitignore 语法）
pub fn precommit(root: &Path, unsaved: &[PathBuf], sensitive: &str) -> Vec<Issue> {
    let mut out = vec![];
    let files = staged(root);
    let in_repo: Vec<String> = unsaved.iter().filter_map(|p| p.strip_prefix(root).ok().map(|r| r.to_string_lossy().replace('\\', "/"))).collect();
    if !in_repo.is_empty() {
        out.push(Issue {
            level: 2,
            text: tf("编辑器里还有 {} 个文件没保存：{}", &[&in_repo.len(), &in_repo.join("、")]),
        });
    }
    let sens = crate::history::review::Sensitive::new(sensitive);
    for (st, rel) in &files {
        if *st == 'D' {
            continue;
        }
        let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let size: u64 = git(root, &["cat-file", "-s", &format!(":{rel}")]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if BUILD_EXTS.contains(&ext.as_str()) && !matches!(ext.as_str(), "hex" | "bin") {
            out.push(Issue {
                level: 1,
                text: tf("{} 看起来是编译产物，一般不提交", &[rel]),
            });
        } else if size > 5 << 20 {
            out.push(Issue {
                level: 1,
                text: tf("{} 有 {} MB，大文件会让仓库变慢", &[rel, &format!("{:.1}", size as f64 / 1048576.0)]),
            });
        }
        if size > 0 && size < 2 << 20 {
            if let Ok(text) = git(root, &["show", &format!(":{rel}")]) {
                if conflict_marker(&text) {
                    out.push(Issue {
                        level: 2,
                        text: tf("{} 里还有冲突标记（<<<<<<< / >>>>>>>）", &[rel]),
                    });
                }
                if let Some(kind) = secret_in(&text) {
                    out.push(Issue {
                        level: 1,
                        text: tf("{} 里可能有{}，确认一下再提交", &[rel, &t(kind)]),
                    });
                }
            }
        }
        if sens.hit(rel) {
            out.push(Issue {
                level: 0,
                text: tf("{} 是敏感文件（链接脚本、启动文件、配置等），确认改动是有意的", &[rel]),
            });
        }
    }
    out
}

/// 根据暂存的文件写一句提交说明草稿
pub fn draft_message(root: &Path) -> String {
    let files = staged(root);
    if files.is_empty() {
        return String::new();
    }
    let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let mut parts = vec![];
    for (code, label) in [('A', t("新增")), ('M', t("修改")), ('D', t("删除"))] {
        let v: Vec<String> = files.iter().filter(|(s, _)| *s == code || (code == 'M' && !"AD".contains(*s))).map(|(_, p)| name(p)).collect();
        if v.is_empty() {
            continue;
        }
        let list = if v.len() > 3 {
            tf("{} 等 {} 个文件", &[&v[..3].join("、"), &v.len()])
        } else {
            v.join("、")
        };
        parts.push(format!("{label} {list}"));
    }
    // 都在同一个目录下：加目录前缀，如「drivers/uart: 修改 uart.c」
    let dirs: std::collections::BTreeSet<&str> = files.iter().map(|(_, p)| p.rsplit_once('/').map_or("", |(d, _)| d)).collect();
    let prefix = match dirs.iter().next() {
        Some(d) if dirs.len() == 1 && !d.is_empty() => format!("{d}: "),
        _ => String::new(),
    };
    format!("{prefix}{}", parts.join("；"))
}

/// 已有标签里最大的版本号加一（v1.2.3 → v1.2.4），没有版本号形式的标签返回 None
pub fn next_tag(tags: &[String]) -> Option<String> {
    let mut best: Option<(Vec<u64>, String)> = None;
    for tg in tags {
        let (pre, rest) = match tg.find(|c: char| c.is_ascii_digit()) {
            Some(i) => tg.split_at(i),
            None => continue,
        };
        if pre.len() > 1 || (pre.len() == 1 && pre != "v" && pre != "V") {
            continue;
        }
        let nums: Option<Vec<u64>> = rest.split('.').map(|s| s.parse().ok()).collect();
        let Some(nums) = nums.filter(|n| (2..=4).contains(&n.len())) else { continue };
        if best.as_ref().is_none_or(|(b, _)| nums > *b) {
            best = Some((nums, pre.to_string()));
        }
    }
    let (mut n, pre) = best?;
    *n.last_mut()? += 1;
    Some(format!("{pre}{}", n.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(".")))
}

/// 子模块：(状态, 路径, 说明)。状态：' ' 正常，'-' 没初始化，'+' 检出的版本和记录的不一样，'U' 冲突
pub fn submodules(root: &Path) -> Vec<(char, String, String)> {
    if !root.join(".gitmodules").exists() {
        return vec![];
    }
    let Ok(out) = git(root, &["submodule", "status"]) else { return vec![] };
    out.lines()
        .filter_map(|l| {
            let st = l.chars().next()?;
            let mut it = l[1..].split_whitespace();
            let hash = it.next()?.chars().take(8).collect::<String>();
            let path = it.next()?.to_string();
            let desc = it.next().map(|d| d.trim_matches(|c| c == '(' || c == ')').to_string()).unwrap_or(hash);
            Some((st, path, desc))
        })
        .collect()
}

// ---------------------------------------------------------------- 固件体积历史

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SizePoint {
    pub hash: String,
    pub subject: String,
    /// 秒
    pub ts: i64,
    pub rom: u64,
    pub ram: u64,
}

fn size_log(root: &Path) -> Option<PathBuf> {
    let d = git(root, &["rev-parse", "--git-dir"]).ok()?;
    let d = PathBuf::from(d.trim());
    Some(if d.is_absolute() { d } else { root.join(d) }.join("qccom-sizes.jsonl"))
}

pub fn load_sizes(root: &Path) -> Vec<SizePoint> {
    let Some(p) = size_log(root) else { return vec![] };
    std::fs::read_to_string(p).map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

/// 给当前 HEAD 记一笔固件体积（用工程里最新的编译产物）。同一个提交只留最后一次
pub fn record_size(root: &Path) -> Result<SizePoint, String> {
    let out = crate::history::build_output(root).ok_or_else(|| t("工程里没找到编译产物（.elf / .axf / .map），先编译一次").to_string())?;
    let s = crate::toolbox::sizediff::load(&out)?;
    let head = git(root, &["log", "-1", "--format=%H%x1f%s%x1f%ct"])?;
    let mut it = head.trim().split('\x1f');
    let hash = it.next().unwrap_or("").to_string();
    let subject = it.next().unwrap_or("").to_string();
    let ts = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let pt = SizePoint {
        hash,
        subject,
        ts,
        rom: s.rom,
        ram: s.ram,
    };
    let path = size_log(root).ok_or_else(|| t("不是 Git 仓库").to_string())?;
    let mut all = load_sizes(root);
    all.retain(|p| p.hash != pt.hash);
    all.push(pt.clone());
    let text: String = all.iter().filter_map(|p| serde_json::to_string(p).ok()).map(|l| l + "\n").collect();
    std::fs::write(path, text).map_err(|e| e.to_string())?;
    Ok(pt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_checks() {
        let tags: Vec<String> = ["v1.2.3", "v1.10.0", "test", "release-2", "v1.9.9"].iter().map(|s| s.to_string()).collect();
        assert_eq!(next_tag(&tags).as_deref(), Some("v1.10.1"));
        assert_eq!(next_tag(&["1.0".to_string()]).as_deref(), Some("1.1"));
        assert_eq!(next_tag(&["abc".to_string()]), None);
        assert!(conflict_marker("a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> x\n"));
        assert!(!conflict_marker("a ======= b"));
        assert_eq!(secret_in("const char *password = \"hunter22\";"), Some("密码或密钥"));
        assert_eq!(secret_in("int password_len = 8;"), None);
        assert!(secret_in("-----BEGIN RSA PRIVATE KEY-----").is_some());
    }
}

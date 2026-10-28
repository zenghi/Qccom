//! .gitignore 规则：本地历史、外部改动审阅都按工程自己的 .gitignore 决定哪些文件不管。
//! 支持 Git 的常用语法：`*` `?` `[a-z]` `**`、`!` 取反、结尾 `/` 只匹配目录、带 `/` 的规则相对 .gitignore 所在目录。
//! 子目录里的 .gitignore 按目录逐层加载；.git/info/exclude 也算。

use std::path::Path;

#[derive(Clone, Debug)]
struct Rule {
    /// 规则所在 .gitignore 的目录（相对工程根，空串是根目录，否则以 / 结尾）
    base: String,
    pat: String,
    neg: bool,
    dir_only: bool,
    /// 带 / 的规则要从 base 开始整段匹配；不带 / 的只匹配最后一级名字
    anchored: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Ignore {
    rules: Vec<Rule>,
}

/// 嵌入式工程的默认排除规则（.gitignore 缺编译产物规则时建议添加）。每组：(标题, 规则)
pub const DEFAULT_RULES: &[(&str, &[&str])] = &[
    ("通用", &["*.o", "*.obj", "*.d", "*.elf", "*.axf", "*.out", "*.map", "*.lst", "build/"]),
    ("Keil", &["Objects/", "Listings/", "*.uvguix.*", "*.crf", "*.dep", "*.htm", "*.lnp", "*.bak", "JLinkLog.txt"]),
    ("IAR", &["**/Debug/Exe/", "**/Debug/Obj/", "**/Debug/List/", "*.dni", "*.wsdt", "*.dbgdt"]),
    ("ESP-IDF", &["sdkconfig.old", "managed_components/"]),
    ("STM32CubeIDE", &["Debug/", "Release/"]),
    ("PlatformIO / CMake", &[".pio/", "CMakeFiles/", "cmake-build-*/"]),
];

impl Ignore {
    /// 只有 .git/info/exclude 和根目录 .gitignore；子目录的规则在遍历时用 [`Ignore::push_dir`] 追加
    pub fn load(root: &Path) -> Self {
        let mut ig = Ignore::default();
        if let Ok(s) = std::fs::read_to_string(root.join(".git/info/exclude")) {
            ig.add_text("", &s);
        }
        if let Ok(s) = std::fs::read_to_string(root.join(".gitignore")) {
            ig.add_text("", &s);
        }
        ig
    }

    /// 默认规则（工程没写规则时内部也用它，免得编译一次就刷出几百个改动）
    pub fn defaults() -> Self {
        let mut ig = Ignore::default();
        for (_, rs) in DEFAULT_RULES {
            for r in *rs {
                ig.add_line("", r);
            }
        }
        ig
    }

    /// 合并另一组规则（排在后面，优先级更高）
    pub fn extend(&mut self, other: &Ignore) {
        self.rules.extend(other.rules.iter().cloned());
    }

    /// 进入子目录时，读取它的 .gitignore；返回追加前的规则数，出目录时 truncate 回去
    pub fn push_dir(&mut self, root: &Path, rel_dir: &str) -> usize {
        let n = self.rules.len();
        if !rel_dir.is_empty() {
            if let Ok(s) = std::fs::read_to_string(root.join(rel_dir).join(".gitignore")) {
                self.add_text(&format!("{rel_dir}/"), &s);
            }
        }
        n
    }

    pub fn truncate(&mut self, n: usize) {
        self.rules.truncate(n);
    }

    pub fn add_text(&mut self, base: &str, text: &str) {
        for l in text.lines() {
            self.add_line(base, l);
        }
    }

    fn add_line(&mut self, base: &str, line: &str) {
        let line = line.trim_end_matches('\r');
        // 行尾空格要用 \ 转义才算；这里简单地去掉没转义的
        let mut l = line.trim_end().to_string();
        if line.ends_with("\\ ") {
            l = line.to_string();
        }
        if l.is_empty() || l.starts_with('#') {
            return;
        }
        let neg = l.starts_with('!');
        if neg {
            l.remove(0);
        } else if l.starts_with("\\!") || l.starts_with("\\#") {
            l.remove(0);
        }
        let dir_only = l.ends_with('/');
        let l = l.trim_end_matches('/');
        if l.is_empty() {
            return;
        }
        let anchored = l.contains('/');
        let pat = l.trim_start_matches('/').to_string();
        self.rules.push(Rule {
            base: base.to_string(),
            pat,
            neg,
            dir_only,
            anchored,
        });
    }

    /// rel 是相对工程根、用 / 分隔的路径。调用者应保证上级目录没有被排除（遍历时整个目录直接跳过）
    pub fn matched(&self, rel: &str, is_dir: bool) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let mut ignored = false;
        for r in &self.rules {
            if r.neg != ignored {
                continue; // 结果不会改变的规则不用算
            }
            if r.dir_only && !is_dir {
                continue;
            }
            let Some(sub) = rel.strip_prefix(r.base.as_str()) else { continue };
            let hit = if r.anchored {
                glob(r.pat.as_bytes(), sub.as_bytes())
            } else {
                glob(r.pat.as_bytes(), name.as_bytes())
            };
            if hit {
                ignored = !r.neg;
            }
        }
        ignored
    }

    /// 完整判断：任何一级上级目录被排除，自己也算被排除
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        let mut end = 0;
        while let Some(i) = rel[end..].find('/') {
            let dir = &rel[..end + i];
            if self.matched(dir, true) {
                return true;
            }
            end += i + 1;
        }
        self.matched(rel, is_dir)
    }

    /// 规则里有没有排除编译产物（没有就提示添加默认规则）
    pub fn covers_build_outputs(&self) -> bool {
        let probes = [("main.o", false), ("app.elf", false), ("app.axf", false), ("build", true), ("Objects", true), (".pio", true)];
        probes.iter().filter(|(p, d)| self.matched(p, *d)).count() >= 2
    }
}

/// 生成写进 .gitignore 的文本，`picked` 与 [`DEFAULT_RULES`] 一一对应
pub fn default_text(picked: &[bool]) -> String {
    let mut s = String::from("\n# 嵌入式编译产物（由 QCCOM 添加）\n");
    for ((title, rules), on) in DEFAULT_RULES.iter().zip(picked) {
        if *on {
            s += &format!("# {title}\n");
            for r in *rules {
                s += r;
                s.push('\n');
            }
        }
    }
    s
}

/// Git 风格的通配符匹配：`*` 和 `?` 不跨 `/`，`**` 跨目录
pub fn glob(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p.starts_with(b"**") {
        let rest = &p[2..];
        if rest.is_empty() {
            return true;
        }
        // "**/" 匹配零级或多级目录
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        if glob(rest, s) {
            return true;
        }
        for (i, &c) in s.iter().enumerate() {
            if c == b'/' && glob(rest, &s[i + 1..]) {
                return true;
            }
        }
        return false;
    }
    match p[0] {
        b'*' => {
            for i in 0..=s.len() {
                if glob(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        b'?' => !s.is_empty() && s[0] != b'/' && glob(&p[1..], &s[1..]),
        b'[' => {
            let Some(end) = p[1..].iter().position(|&c| c == b']').map(|i| i + 1) else {
                return !s.is_empty() && s[0] == b'[' && glob(&p[1..], &s[1..]);
            };
            if s.is_empty() || s[0] == b'/' {
                return false;
            }
            let mut set = &p[1..end];
            let neg = matches!(set.first(), Some(b'!' | b'^'));
            if neg {
                set = &set[1..];
            }
            let c = s[0];
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == b'-' {
                    hit |= set[i] <= c && c <= set[i + 2];
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != neg && glob(&p[end + 1..], &s[1..])
        }
        b'\\' if p.len() > 1 => !s.is_empty() && s[0] == p[1] && glob(&p[2..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob(b"*.o", b"main.o"));
        assert!(!glob(b"*.o", b"src/main.o"));
        assert!(glob(b"src/**/*.c", b"src/a/b/x.c"));
        assert!(glob(b"src/**/*.c", b"src/x.c"));
        assert!(glob(b"**/build", b"a/b/build"));
        assert!(glob(b"doc/**", b"doc/x/y"));
        assert!(glob(b"*.uvguix.*", b"proj.uvguix.zenghi"));
        assert!(glob(b"[Dd]ebug", b"debug"));
        assert!(!glob(b"[!D]ebug", b"Debug"));
        assert!(glob(b"cmake-build-*", b"cmake-build-release"));
    }

    #[test]
    fn rules() {
        let mut ig = Ignore::default();
        ig.add_text("", "# c\n*.o\nbuild/\n/Objects\n!keep.o\nsrc/gen/*.c\n");
        assert!(ig.is_ignored("a/b/main.o", false));
        assert!(!ig.is_ignored("keep.o", false));
        assert!(ig.is_ignored("build", true));
        assert!(!ig.is_ignored("build", false));
        assert!(ig.is_ignored("x/build/out.bin", false));
        assert!(ig.is_ignored("Objects/a.axf", false));
        assert!(!ig.is_ignored("lib/Objects/a.h", false));
        assert!(ig.is_ignored("src/gen/a.c", false));
        assert!(!ig.is_ignored("src/gen/sub/a.c", false));
        ig.add_text("lib/", "*.h\n");
        assert!(ig.is_ignored("lib/x/a.h", false));
        assert!(!ig.is_ignored("inc/a.h", false));
        assert!(!ig.covers_build_outputs() || ig.matched("main.o", false));
        assert!(Ignore::defaults().covers_build_outputs());
        assert!(!Ignore::default().covers_build_outputs());
    }
}

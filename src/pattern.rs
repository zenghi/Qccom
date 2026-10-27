//! 查找 / 替换共用的匹配规则：区分大小写、全字匹配、正则。统一编译成 regex-lite 的正则，
//! 编辑器内查找和全工程搜索都用它。

use regex_lite::{Regex, RegexBuilder};

#[derive(Clone, Default, PartialEq)]
pub struct Pat {
    pub q: String,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

impl Pat {
    pub fn compile(&self) -> Result<Regex, String> {
        let body = if self.regex { self.q.clone() } else { regex_lite::escape(&self.q) };
        // 全字匹配：只在查询以标识符字符开头 / 结尾的那一侧加 \b（`->x` 这类查询前面不需要）
        let is_w = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let body = if self.word {
            let pre = if self.regex || is_w(self.q.chars().next()) { r"\b" } else { "" };
            let post = if self.regex || is_w(self.q.chars().last()) { r"\b" } else { "" };
            format!("{pre}(?:{body}){post}")
        } else {
            body
        };
        RegexBuilder::new(&body).case_insensitive(!self.case).multi_line(true).build().map_err(|e| e.to_string())
    }

    /// 所有匹配的字节范围（空匹配跳过），最多 limit 个
    pub fn find_all(re: &Regex, text: &str, limit: usize) -> Vec<(usize, usize)> {
        re.find_iter(text).filter(|m| !m.is_empty()).take(limit).map(|m| (m.start(), m.end())).collect()
    }

    /// 某个匹配替换后的文字：正则模式下支持 $1 / ${name}，否则原样
    pub fn replacement(&self, re: &Regex, text: &str, at: (usize, usize), repl: &str) -> String {
        if !self.regex {
            return repl.to_string();
        }
        match re.captures_at(text, at.0).filter(|c| c.get(0).is_some_and(|m| m.start() == at.0)) {
            Some(c) => {
                let mut out = String::new();
                c.expand(repl, &mut out);
                out
            }
            None => repl.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str, case: bool, word: bool, regex: bool) -> Pat {
        Pat { q: q.into(), case, word, regex }
    }

    #[test]
    fn plain_and_word() {
        let t = "crc crc16 CRC x.crc";
        let re = p("crc", false, false, false).compile().unwrap();
        assert_eq!(Pat::find_all(&re, t, 99).len(), 4);
        let re = p("crc", true, true, false).compile().unwrap();
        assert_eq!(Pat::find_all(&re, t, 99), vec![(0, 3), (16, 19)]);
        let re = p("a.b", true, false, false).compile().unwrap();
        assert!(Pat::find_all(&re, "axb", 9).is_empty());
    }

    #[test]
    fn regex_replace() {
        let pat = p(r"uart(\d)_init", true, false, true);
        let re = pat.compile().unwrap();
        let t = "uart2_init(); uart3_init();";
        let hits = Pat::find_all(&re, t, 9);
        assert_eq!(hits.len(), 2);
        assert_eq!(pat.replacement(&re, t, hits[1], "usart${1}_setup"), "usart3_setup");
        assert!(p("(", true, false, true).compile().is_err());
    }
}

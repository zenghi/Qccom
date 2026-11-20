//! 电路页的长文本：元件知识卡、示例教程。放在 assets/circuit/content.tsv（键、简体、繁体、英文），
//! 其它语言用英文。界面上的短文字仍然走 i18n.tsv。

use std::collections::HashMap;
use std::sync::OnceLock;

const SRC: &str = include_str!("../../assets/circuit/content.tsv");

fn table() -> &'static HashMap<&'static str, [&'static str; 3]> {
    static T: OnceLock<HashMap<&'static str, [&'static str; 3]>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for line in SRC.lines() {
            if line.starts_with("# ") || line.trim().is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(k), Some(zh)) = (it.next(), it.next()) else { continue };
            let tw = it.next().unwrap_or("");
            let en = it.next().unwrap_or("");
            m.insert(k, [zh, tw, en]);
        }
        m
    })
}

/// 按当前语言取一条：简体 0、英文 1、繁体 2，其它语言用英文，缺了退回简体
pub fn get(key: &str) -> &'static str {
    let Some(v) = table().get(key) else { return "" };
    let col = match crate::i18n::current() {
        0 => 0,
        2 => 1,
        _ => 2,
    };
    if v[col].is_empty() {
        v[0]
    } else {
        v[col]
    }
}

/// 元件知识卡（段落用 \n 分隔）
pub fn card(key: &str) -> &'static str {
    get(&format!("card.{key}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn rows_have_four_columns() {
        for (i, line) in super::SRC.lines().enumerate() {
            if line.starts_with("# ") || line.trim().is_empty() {
                continue;
            }
            assert_eq!(line.split('\t').count(), 4, "content.tsv 第 {} 行列数不对", i + 1);
        }
    }
}

//! 表格比较：CSV（逗号 / 分号 / Tab 自动识别）和链接器 .map 的符号表，按关键列对齐后逐格比较。

use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Default, Debug)]
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 默认的关键列
    pub key: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Same,
    Changed,
    LeftOnly,
    RightOnly,
}

#[derive(Clone, Debug)]
pub struct TRow {
    pub l: Option<usize>,
    pub r: Option<usize>,
    pub kind: Kind,
    /// 哪几列不同
    pub cells: Vec<bool>,
}

pub fn load(p: &Path) -> Result<Table, String> {
    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    let (text, _) = crate::cindex::decode(&bytes).ok_or_else(|| crate::i18n::t("这是二进制文件，无法以文本方式查看").to_string())?;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "map" {
        if let Some(m) = crate::binview::parse::parse_map(&text) {
            return Ok(from_map(&m));
        }
    }
    Ok(parse_csv(&text))
}

pub fn from_map(m: &crate::binview::parse::MapInfo) -> Table {
    let mut rows: Vec<Vec<String>> = m
        .ins
        .iter()
        .filter(|i| i.size > 0)
        .map(|i| {
            vec![
                crate::binview::parse::sym_name(&i.name).to_string(),
                i.obj.clone(),
                m.outs.get(i.out).map(|o| o.name.clone()).unwrap_or_default(),
                i.size.to_string(),
            ]
        })
        .collect();
    rows.sort();
    Table {
        header: vec!["Symbol".into(), "Object".into(), "Section".into(), "Size".into()],
        rows,
        key: vec![0, 1],
    }
}

pub fn parse_csv(text: &str) -> Table {
    let first = text.lines().next().unwrap_or("");
    let delim = [',', ';', '\t', '|'].into_iter().max_by_key(|d| first.matches(*d).count()).unwrap_or(',');
    let mut rows: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
            continue;
        }
        match c {
            '"' if cell.is_empty() => quoted = true,
            c if c == delim => row.push(std::mem::take(&mut cell)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    // 第一行全不是数字就当表头
    let header = if rows.first().is_some_and(|r| r.iter().all(|c| c.trim().parse::<f64>().is_err())) && rows.len() > 1 {
        rows.remove(0)
    } else {
        let n = rows.iter().map(Vec::len).max().unwrap_or(0);
        (1..=n).map(|i| format!("#{i}")).collect()
    };
    Table { header, rows, key: vec![0] }
}

fn key_of(r: &[String], key: &[usize]) -> String {
    key.iter().map(|&k| r.get(k).map(String::as_str).unwrap_or("")).collect::<Vec<_>>().join("\u{1}")
}

fn cells_diff(a: &[String], b: &[String], n: usize) -> Vec<bool> {
    (0..n).map(|i| a.get(i).map(|s| s.trim()) != b.get(i).map(|s| s.trim())).collect()
}

/// 对齐两张表。key 为空时按行顺序做差分
pub fn compare(a: &Table, b: &Table, key: &[usize]) -> Vec<TRow> {
    let n = a.header.len().max(b.header.len());
    let mk = |l: Option<usize>, r: Option<usize>| {
        let (kind, cells) = match (l, r) {
            (Some(i), Some(j)) => {
                let c = cells_diff(&a.rows[i], &b.rows[j], n);
                (if c.iter().any(|x| *x) { Kind::Changed } else { Kind::Same }, c)
            }
            (Some(_), None) => (Kind::LeftOnly, vec![true; n]),
            _ => (Kind::RightOnly, vec![true; n]),
        };
        TRow { l, r, kind, cells }
    };
    if key.is_empty() {
        let h = |t: &Table| -> Vec<u64> {
            t.rows
                .iter()
                .map(|r| {
                    use std::hash::{Hash, Hasher};
                    let mut s = std::collections::hash_map::DefaultHasher::new();
                    r.hash(&mut s);
                    s.finish()
                })
                .collect()
        };
        let mut out = vec![];
        let (mut x, mut y) = (0, 0);
        for hk in super::diff::hunks(&h(a), &h(b)) {
            while x < hk.a {
                out.push(mk(Some(x), Some(y)));
                x += 1;
                y += 1;
            }
            for k in 0..hk.an.max(hk.bn) {
                out.push(mk((k < hk.an).then(|| hk.a + k), (k < hk.bn).then(|| hk.b + k)));
            }
            x = hk.a + hk.an;
            y = hk.b + hk.bn;
        }
        while x < a.rows.len() {
            out.push(mk(Some(x), Some(y)));
            x += 1;
            y += 1;
        }
        return out;
    }
    // 重复的键按出现次序区分
    let keyed = |t: &Table| -> Vec<String> {
        let mut seen: HashMap<String, usize> = HashMap::new();
        t.rows
            .iter()
            .map(|r| {
                let k = key_of(r, key);
                let c = seen.entry(k.clone()).or_default();
                *c += 1;
                format!("{k}\u{2}{c}")
            })
            .collect()
    };
    let ka = keyed(a);
    let kb = keyed(b);
    let ia: HashMap<&str, usize> = ka.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let mut matched: Vec<Option<usize>> = vec![None; a.rows.len()];
    // 只在右边的行插在右边上一行对应的左行后面
    let mut after: Vec<Vec<usize>> = vec![vec![]; a.rows.len() + 1];
    let mut last_left: usize = 0;
    for (j, k) in kb.iter().enumerate() {
        match ia.get(k.as_str()) {
            Some(&i) => {
                matched[i] = Some(j);
                last_left = i + 1;
            }
            None => after[last_left].push(j),
        }
    }
    let mut out = vec![];
    for j in &after[0] {
        out.push(mk(None, Some(*j)));
    }
    for i in 0..a.rows.len() {
        out.push(mk(Some(i), matched[i]));
        for j in &after[i + 1] {
            out.push(mk(None, Some(*j)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv() {
        let t = parse_csv("name;value\r\n\"a;b\";1\nc;\"say \"\"hi\"\"\"\n");
        assert_eq!(t.header, vec!["name", "value"]);
        assert_eq!(t.rows, vec![vec!["a;b".to_string(), "1".into()], vec!["c".into(), "say \"hi\"".into()]]);
    }

    #[test]
    fn keyed_compare() {
        let a = parse_csv("sym,size\nmain,100\nfoo,20\nold,5\n");
        let b = parse_csv("sym,size\nnew,7\nmain,120\nfoo,20\n");
        let rows = compare(&a, &b, &[0]);
        let kinds: Vec<Kind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![Kind::RightOnly, Kind::Changed, Kind::Same, Kind::LeftOnly]);
        assert_eq!(rows[1].cells, vec![false, true]);
        let rows = compare(&a, &b, &[]);
        assert!(rows.iter().any(|r| r.kind == Kind::Same));
    }
}

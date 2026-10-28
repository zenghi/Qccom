//! 行差分：Myers 算法（改动太多时按两边都只出现一次的行切段再分别比较），
//! 忽略空白 / 大小写 / 注释 / 空行，行内字符级差异，以及手动对齐点。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// 一处改动：a 里从 a 开始的 an 行换成了 b 里从 b 开始的 bn 行
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub a: usize,
    pub an: usize,
    pub b: usize,
    pub bn: usize,
}

/// Myers 搜索的最大编辑距离；超过就改用唯一行切段，避免内存爆掉
const MAX_D: usize = 2000;

pub fn hunks(a: &[u64], b: &[u64]) -> Vec<Hunk> {
    let mut out = vec![];
    diff_into(a, b, 0, 0, &mut out, 0);
    // 相邻的块合并
    let mut merged: Vec<Hunk> = vec![];
    for h in out {
        if let Some(l) = merged.last_mut() {
            if l.a + l.an == h.a && l.b + l.bn == h.b {
                l.an += h.an;
                l.bn += h.bn;
                continue;
            }
        }
        merged.push(h);
    }
    merged
}

fn diff_into(a: &[u64], b: &[u64], oa: usize, ob: usize, out: &mut Vec<Hunk>, depth: u32) {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..].iter().rev().zip(b[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a2, b2) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (oa, ob) = (oa + pre, ob + pre);
    if a2.is_empty() && b2.is_empty() {
        return;
    }
    if a2.is_empty() || b2.is_empty() {
        out.push(Hunk {
            a: oa,
            an: a2.len(),
            b: ob,
            bn: b2.len(),
        });
        return;
    }
    if let Some(v) = myers(a2, b2) {
        out.extend(v.into_iter().map(|h| Hunk { a: h.a + oa, b: h.b + ob, ..h }));
        return;
    }
    // 改动太多：用两边都只出现一次的行做锚点（patience），锚点之间分别比较
    let anchors = if depth < 8 { unique_anchors(a2, b2) } else { vec![] };
    if anchors.is_empty() {
        out.push(Hunk {
            a: oa,
            an: a2.len(),
            b: ob,
            bn: b2.len(),
        });
        return;
    }
    let (mut pa, mut pb) = (0, 0);
    for (x, y) in anchors {
        diff_into(&a2[pa..x], &b2[pb..y], oa + pa, ob + pb, out, depth + 1);
        pa = x + 1;
        pb = y + 1;
    }
    diff_into(&a2[pa..], &b2[pb..], oa + pa, ob + pb, out, depth + 1);
}

/// 两边都恰好出现一次的行，按位置求最长递增子序列
fn unique_anchors(a: &[u64], b: &[u64]) -> Vec<(usize, usize)> {
    let mut ca: HashMap<u64, (u32, usize)> = HashMap::new();
    for (i, k) in a.iter().enumerate() {
        let e = ca.entry(*k).or_insert((0, i));
        e.0 += 1;
    }
    let mut cb: HashMap<u64, (u32, usize)> = HashMap::new();
    for (i, k) in b.iter().enumerate() {
        let e = cb.entry(*k).or_insert((0, i));
        e.0 += 1;
    }
    let mut pairs: Vec<(usize, usize)> = ca
        .iter()
        .filter(|(_, (n, _))| *n == 1)
        .filter_map(|(k, (_, i))| cb.get(k).filter(|(n, _)| *n == 1).map(|(_, j)| (*i, *j)))
        .collect();
    pairs.sort();
    // 按 b 的位置求 LIS（耐心排序）
    let mut tails: Vec<usize> = vec![]; // 每个长度结尾元素在 pairs 里的下标
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for (i, &(_, y)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < y);
        if pos > 0 {
            prev[i] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = vec![];
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(pairs[i]);
        cur = prev[i];
    }
    out.reverse();
    out
}

/// 经典 Myers O(ND)；每一步只存 2d+1 个值，d 超过 MAX_D 就放弃
fn myers(a: &[u64], b: &[u64]) -> Option<Vec<Hunk>> {
    let (n, m) = (a.len() as i64, b.len() as i64);
    let max = (n + m) as usize;
    let limit = max.min(MAX_D) as i64;
    let off = limit + 1;
    let mut v = vec![0i64; (2 * limit + 3) as usize];
    let mut trace: Vec<Vec<i32>> = vec![];
    let mut done = None;
    'outer: for d in 0..=limit {
        let mut k = -d;
        while k <= d {
            let i = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                trace.push(v[(off - d) as usize..=(off + d) as usize].iter().map(|&x| x as i32).collect());
                done = Some(d);
                break 'outer;
            }
            k += 2;
        }
        trace.push(v[(off - d) as usize..=(off + d) as usize].iter().map(|&x| x as i32).collect());
    }
    let dmax = done?;
    // 回溯：trace[d][k + d] 是第 d 步对角线 k 上走到的 x
    let get = |d: i64, k: i64| trace[d as usize][(k + d) as usize] as i64;
    let (mut x, mut y) = (n, m);
    let mut steps: Vec<(i64, i64, bool)> = vec![]; // (x, y, 是删除)
    for d in (1..=dmax).rev() {
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(d - 1, k - 1) < get(d - 1, k + 1)) { k + 1 } else { k - 1 };
        let px = get(d - 1, prev_k);
        let py = px - prev_k;
        steps.push((px, py, prev_k != k + 1));
        x = px;
        y = py;
    }
    steps.reverse();
    let mut hs: Vec<Hunk> = vec![];
    for (x, y, del) in steps {
        let (x, y) = (x as usize, y as usize);
        let (dl, il) = if del { (1, 0) } else { (0, 1) };
        if let Some(h) = hs.last_mut() {
            if h.a + h.an == x && h.b + h.bn == y {
                h.an += dl;
                h.bn += il;
                continue;
            }
        }
        hs.push(Hunk { a: x, an: dl, b: y, bn: il });
    }
    Some(hs)
}

// ------------------------------------------------------------------ 忽略选项

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Opts {
    /// 忽略空白（空格、Tab 的多少和有无）
    pub ws: bool,
    pub case: bool,
    /// 忽略注释（C 风格 // /* */，脚本 #，ini ;）
    pub comments: bool,
    /// 忽略空行
    pub blank: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
    None,
    C,
    Hash,
    Semi,
}

impl CommentStyle {
    pub fn of(path: &str) -> CommentStyle {
        let lower = path.to_ascii_lowercase();
        let name = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if matches!(name, "makefile" | "kconfig" | "cmakelists.txt" | "dockerfile") || name.starts_with("kconfig") {
            return CommentStyle::Hash;
        }
        match ext {
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "rs" | "js" | "ts" | "java" | "go" | "cs" | "swift" | "kt" | "dts" | "dtsi" | "ld" | "lds" | "proto" | "json5" | "jsonc" | "css"
            | "scss" | "v" | "sv" => CommentStyle::C,
            "py" | "sh" | "bash" | "cmake" | "mk" | "yml" | "yaml" | "toml" | "conf" | "cfg" | "pl" | "rb" | "ps1" | "r" => CommentStyle::Hash,
            "ini" | "s" | "asm" | "inf" => CommentStyle::Semi,
            _ => CommentStyle::None,
        }
    }
}

/// 去掉注释；in_block 跨行记住是否在 /* */ 里
fn strip_comment(line: &str, style: CommentStyle, in_block: &mut bool) -> String {
    match style {
        CommentStyle::None => line.to_string(),
        CommentStyle::Hash | CommentStyle::Semi => {
            let c = if style == CommentStyle::Hash { '#' } else { ';' };
            // 引号里的不算
            let mut quote: Option<char> = None;
            for (i, ch) in line.char_indices() {
                match quote {
                    Some(q) if ch == q => quote = None,
                    Some(_) => {}
                    None if ch == '"' || ch == '\'' => quote = Some(ch),
                    None if ch == c => return line[..i].to_string(),
                    None => {}
                }
            }
            line.to_string()
        }
        CommentStyle::C => {
            let b = line.as_bytes();
            let mut out = String::new();
            let mut i = 0;
            let mut quote: Option<u8> = None;
            let mut start = 0;
            while i < b.len() {
                if *in_block {
                    if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        *in_block = false;
                        i += 2;
                        start = i;
                        continue;
                    }
                    i += 1;
                    continue;
                }
                match quote {
                    Some(q) => {
                        if b[i] == b'\\' {
                            i += 2;
                            continue;
                        }
                        if b[i] == q {
                            quote = None;
                        }
                    }
                    None => {
                        if b[i] == b'"' || b[i] == b'\'' {
                            quote = Some(b[i]);
                        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
                            out.push_str(&line[start..i]);
                            return out;
                        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                            out.push_str(&line[start..i]);
                            *in_block = true;
                            i += 2;
                            continue;
                        }
                    }
                }
                i += 1;
            }
            if !*in_block {
                out.push_str(&line[start.min(line.len())..]);
            }
            out
        }
    }
}

/// 每行用来比较的键（按忽略选项归一化后的哈希），以及这一行在忽略选项下是否算"空行"
pub fn keys(lines: &[String], o: &Opts, style: CommentStyle) -> (Vec<u64>, Vec<bool>) {
    let mut in_block = false;
    let mut ks = Vec::with_capacity(lines.len());
    let mut blank = Vec::with_capacity(lines.len());
    for l in lines {
        let mut s = if o.comments { strip_comment(l, style, &mut in_block) } else { l.clone() };
        if o.ws {
            s.retain(|c| !c.is_whitespace());
        }
        if o.case {
            s = s.to_lowercase();
        }
        blank.push(o.blank && s.trim().is_empty());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        ks.push(h.finish());
    }
    (ks, blank)
}

// ------------------------------------------------------------------ 对齐成行

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Same,
    /// 只差在忽略掉的东西上（空白、大小写、注释、空行）
    Minor,
    Changed,
    LeftOnly,
    RightOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub l: Option<usize>,
    pub r: Option<usize>,
    pub kind: Kind,
}

/// 左右对照的行；anchors 是手动对齐点（左行, 右行），必须两边都递增
pub fn align(la: &[String], lb: &[String], o: &Opts, style: CommentStyle, anchors: &[(usize, usize)]) -> Vec<Row> {
    let (ka, ba) = keys(la, o, style);
    let (kb, bb) = keys(lb, o, style);
    let mut rows = vec![];
    let (mut pa, mut pb) = (0, 0);
    let mut anchors: Vec<(usize, usize)> = anchors.iter().copied().filter(|&(x, y)| x < la.len() && y < lb.len()).collect();
    anchors.sort();
    for (x, y) in anchors {
        if x < pa || y < pb {
            continue;
        }
        segment(la, lb, &ka, &kb, &ba, &bb, pa..x, pb..y, &mut rows);
        let kind = if la[x] == lb[y] {
            Kind::Same
        } else if ka[x] == kb[y] {
            Kind::Minor
        } else {
            Kind::Changed
        };
        rows.push(Row { l: Some(x), r: Some(y), kind });
        pa = x + 1;
        pb = y + 1;
    }
    segment(la, lb, &ka, &kb, &ba, &bb, pa..la.len(), pb..lb.len(), &mut rows);
    rows
}

#[allow(clippy::too_many_arguments)]
fn segment(la: &[String], lb: &[String], ka: &[u64], kb: &[u64], ba: &[bool], bb: &[bool], ra: std::ops::Range<usize>, rb: std::ops::Range<usize>, rows: &mut Vec<Row>) {
    // 忽略空行时，空行不参与差分，之后再插回去
    let ia: Vec<usize> = ra.clone().filter(|&i| !ba[i]).collect();
    let ib: Vec<usize> = rb.clone().filter(|&i| !bb[i]).collect();
    let sa: Vec<u64> = ia.iter().map(|&i| ka[i]).collect();
    let sb: Vec<u64> = ib.iter().map(|&i| kb[i]).collect();
    let hs = hunks(&sa, &sb);
    // 先得到非空行的配对序列
    let mut pairs: Vec<(Option<usize>, Option<usize>, bool)> = vec![]; // (左, 右, 是改动)
    let (mut x, mut y) = (0, 0);
    let push_same = |pairs: &mut Vec<_>, x: &mut usize, y: &mut usize, to_x: usize| {
        while *x < to_x {
            pairs.push((Some(ia[*x]), Some(ib[*y]), false));
            *x += 1;
            *y += 1;
        }
    };
    for h in &hs {
        push_same(&mut pairs, &mut x, &mut y, h.a);
        // 改动块里按相似度配对：插了一行时，后面相似的行仍然左右对齐
        let ta: Vec<&str> = (0..h.an).map(|k| la[ia[h.a + k]].as_str()).collect();
        let tb: Vec<&str> = (0..h.bn).map(|k| lb[ib[h.b + k]].as_str()).collect();
        for (l, r) in pair_lines(&ta, &tb) {
            pairs.push((l.map(|k| ia[h.a + k]), r.map(|k| ib[h.b + k]), true));
        }
        x = h.a + h.an;
        y = h.b + h.bn;
    }
    push_same(&mut pairs, &mut x, &mut y, ia.len());
    // 插回空行：在下一个非空行之前，把两边落下的空行成对放出来
    let (mut na, mut nb) = (ra.start, rb.start);
    let flush = |rows: &mut Vec<Row>, na: &mut usize, nb: &mut usize, to_a: usize, to_b: usize| {
        let mut pa: Vec<usize> = (*na..to_a).collect();
        let mut pb: Vec<usize> = (*nb..to_b).collect();
        *na = to_a.max(*na);
        *nb = to_b.max(*nb);
        let n = pa.len().max(pb.len());
        pa.resize(n, usize::MAX);
        pb.resize(n, usize::MAX);
        for (l, r) in pa.into_iter().zip(pb) {
            let l = (l != usize::MAX).then_some(l);
            let r = (r != usize::MAX).then_some(r);
            rows.push(Row { l, r, kind: Kind::Minor });
        }
    };
    for (l, r, changed) in pairs {
        let to_a = l.unwrap_or(na);
        let to_b = r.unwrap_or(nb);
        flush(rows, &mut na, &mut nb, to_a, to_b);
        let kind = match (l, r) {
            (Some(i), Some(j)) if !changed => {
                if la[i] == lb[j] {
                    Kind::Same
                } else {
                    Kind::Minor
                }
            }
            (Some(_), Some(_)) => Kind::Changed,
            (Some(_), None) => Kind::LeftOnly,
            _ => Kind::RightOnly,
        };
        rows.push(Row { l, r, kind });
        if let Some(i) = l {
            na = i + 1;
        }
        if let Some(j) = r {
            nb = j + 1;
        }
    }
    flush(rows, &mut na, &mut nb, ra.end, rb.end);
}

/// 两行的相似度（字符二元组的 Dice 系数，0..1）
fn similarity(a: &str, b: &str) -> f32 {
    let bg = |s: &str| -> Vec<(char, char)> {
        let c: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
        let mut v: Vec<(char, char)> = c.windows(2).map(|w| (w[0], w[1])).collect();
        v.sort_unstable();
        v
    };
    let (x, y) = (bg(a), bg(b));
    if x.is_empty() || y.is_empty() {
        return if a.trim() == b.trim() { 1.0 } else { 0.0 };
    }
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < x.len() && j < y.len() {
        match x[i].cmp(&y[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * n as f32 / (x.len() + y.len()) as f32
}

/// 改动块内的行配对：相似的行放在同一排，其余单独占一排；块太大或都不像时按位置配对
fn pair_lines(a: &[&str], b: &[&str]) -> Vec<(Option<usize>, Option<usize>)> {
    let (n, m) = (a.len(), b.len());
    let positional = || (0..n.max(m)).map(|k| ((k < n).then_some(k), (k < m).then_some(k))).collect();
    if n == 0 || m == 0 || n * m > 10_000 || (n == m && (0..n).all(|k| similarity(a[k], b[k]) >= 0.4)) {
        return positional();
    }
    const MIN: f32 = 0.4;
    let sim: Vec<Vec<f32>> = a.iter().map(|x| b.iter().map(|y| similarity(x, y)).collect()).collect();
    // dp[i][j]：a[i..] 和 b[j..] 能配上的最大相似度之和
    let mut dp = vec![vec![0f32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let take = if sim[i][j] >= MIN { sim[i][j] + dp[i + 1][j + 1] } else { 0.0 };
            dp[i][j] = take.max(dp[i + 1][j]).max(dp[i][j + 1]);
        }
    }
    if dp[0][0] == 0.0 {
        return positional();
    }
    let mut out = vec![];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if sim[i][j] >= MIN && (dp[i][j] - (sim[i][j] + dp[i + 1][j + 1])).abs() < 1e-5 {
            out.push((Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if dp[i][j] == dp[i + 1][j] {
            out.push((Some(i), None));
            i += 1;
        } else {
            out.push((None, Some(j)));
            j += 1;
        }
    }
    out.extend((i..n).map(|i| (Some(i), None)));
    out.extend((j..m).map(|j| (None, Some(j))));
    out
}

/// 每处改动（连续的非相同行）的起始行号
pub fn blocks(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut v = vec![];
    let mut i = 0;
    while i < rows.len() {
        if matches!(rows[i].kind, Kind::Same | Kind::Minor) {
            i += 1;
            continue;
        }
        let s = i;
        while i < rows.len() && !matches!(rows[i].kind, Kind::Same | Kind::Minor) {
            i += 1;
        }
        v.push((s, i));
    }
    v
}

// ------------------------------------------------------------------ 行内差异

/// 行内改动的字节范围（左, 右）。按"单词 / 单个符号"切分再比较，结果更好读
pub fn inline(a: &str, b: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let ta = tokens(a);
    let tb = tokens(b);
    let ha: Vec<u64> = ta.iter().map(|&(s, e)| hash_str(&a[s..e])).collect();
    let hb: Vec<u64> = tb.iter().map(|&(s, e)| hash_str(&b[s..e])).collect();
    let mut ra = vec![];
    let mut rb = vec![];
    for h in hunks(&ha, &hb) {
        if h.an > 0 {
            ra.push((ta[h.a].0, ta[h.a + h.an - 1].1));
        }
        if h.bn > 0 {
            rb.push((tb[h.b].0, tb[h.b + h.bn - 1].1));
        }
    }
    (ra, rb)
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn tokens(s: &str) -> Vec<(usize, usize)> {
    let mut v = vec![];
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let mut end = i + c.len_utf8();
        if word(c) {
            while let Some(&(j, d)) = it.peek() {
                if !word(d) {
                    break;
                }
                end = j + d.len_utf8();
                it.next();
            }
        } else if c.is_whitespace() {
            while let Some(&(j, d)) = it.peek() {
                if !d.is_whitespace() {
                    break;
                }
                end = j + d.len_utf8();
                it.next();
            }
        }
        v.push((i, end));
    }
    v
}

/// 文本按行切开，记住行尾风格
pub fn split_lines(text: &str) -> (Vec<String>, &'static str) {
    let crlf = text.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let mut v: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    // 末尾换行不算一行空行
    if v.len() > 1 && v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    (v, eol)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(s: &str) -> Vec<String> {
        s.split('|').map(str::to_string).collect()
    }
    fn k(v: &[&str]) -> Vec<u64> {
        v.iter().map(|s| hash_str(s)).collect()
    }

    #[test]
    fn similar_lines_pair_up() {
        let a = ls("x|    while (*s) {|y");
        let b = ls("x|    size_t n = strlen(s);|    while (n--) {|y");
        let rows = align(&a, &b, &Opts::default(), CommentStyle::None, &[]);
        assert_eq!(
            rows[1],
            Row {
                l: None,
                r: Some(1),
                kind: Kind::RightOnly
            }
        );
        assert_eq!(
            rows[2],
            Row {
                l: Some(1),
                r: Some(2),
                kind: Kind::Changed
            }
        );
    }

    #[test]
    fn basic_hunks() {
        let a = k(&["a", "b", "c", "d"]);
        let b = k(&["a", "x", "c", "d", "e"]);
        assert_eq!(hunks(&a, &b), vec![Hunk { a: 1, an: 1, b: 1, bn: 1 }, Hunk { a: 4, an: 0, b: 4, bn: 1 }]);
        assert!(hunks(&a, &a).is_empty());
        assert_eq!(hunks(&[], &a), vec![Hunk { a: 0, an: 0, b: 0, bn: 4 }]);
    }

    #[test]
    fn large_falls_back_to_anchors() {
        // 两边完全不同的 5000 行，中间有一行相同的锚点
        let mut a: Vec<u64> = (0..5000).collect();
        let mut b: Vec<u64> = (10000..15000).collect();
        a[2500] = 99999;
        b[2600] = 99999;
        let hs = hunks(&a, &b);
        assert_eq!(hs.len(), 2);
        assert_eq!((hs[0].a, hs[0].an, hs[0].b, hs[0].bn), (0, 2500, 0, 2600));
    }

    #[test]
    fn options() {
        let a = ls("int a = 1;|  foo();  // call|");
        let b = ls("int  a=1;|foo(); /* x */|");
        let o = Opts {
            ws: true,
            comments: true,
            ..Default::default()
        };
        let rows = align(&a, &b, &o, CommentStyle::C, &[]);
        assert!(rows.iter().all(|r| r.kind != Kind::Changed), "{rows:?}");
        let rows = align(&a, &b, &Opts::default(), CommentStyle::None, &[]);
        assert_eq!(rows.iter().filter(|r| r.kind == Kind::Changed).count(), 2);
        let c = ls("ABC");
        let d = ls("abc");
        assert_eq!(align(&c, &d, &Opts { case: true, ..Default::default() }, CommentStyle::None, &[])[0].kind, Kind::Minor);
    }

    #[test]
    fn blank_lines() {
        let a = ls("a||b|c");
        let b = ls("a|b|||c");
        let o = Opts { blank: true, ..Default::default() };
        let rows = align(&a, &b, &o, CommentStyle::None, &[]);
        assert!(rows.iter().all(|r| matches!(r.kind, Kind::Same | Kind::Minor)), "{rows:?}");
        // 每行都出现一次
        assert_eq!(rows.iter().filter(|r| r.l.is_some()).count(), 4);
        assert_eq!(rows.iter().filter(|r| r.r.is_some()).count(), 5);
    }

    #[test]
    fn anchors_force_pairs() {
        let a = ls("x|f1|y");
        let b = ls("f2|x|y");
        let rows = align(&a, &b, &Opts::default(), CommentStyle::None, &[(1, 0)]);
        assert_eq!(
            rows[0],
            Row {
                l: Some(0),
                r: None,
                kind: Kind::LeftOnly
            }
        );
        assert_eq!(
            rows[1],
            Row {
                l: Some(1),
                r: Some(0),
                kind: Kind::Changed
            }
        );
    }

    #[test]
    fn inline_words() {
        let (a, b) = inline("led_on(GPIOA, 5);", "led_on(GPIOB, 5);");
        assert_eq!(a, vec![(7, 12)]);
        assert_eq!(b, vec![(7, 12)]);
    }

    #[test]
    fn comments() {
        let mut blk = false;
        assert_eq!(strip_comment("a /* b */ c // d", CommentStyle::C, &mut blk), "a  c ");
        assert_eq!(strip_comment("x = \"//no\"; // yes", CommentStyle::C, &mut blk), "x = \"//no\"; ");
        assert_eq!(strip_comment("a /* open", CommentStyle::C, &mut blk), "a ");
        assert!(blk);
        assert_eq!(strip_comment("still */ b", CommentStyle::C, &mut blk), " b");
        assert_eq!(strip_comment("key = 1 # note", CommentStyle::Hash, &mut false), "key = 1 ");
        assert!(CommentStyle::of("src/main.c") == CommentStyle::C);
    }

    #[test]
    fn lines() {
        let (v, eol) = split_lines("a\r\nb\r\n");
        assert_eq!((v, eol), (vec!["a".to_string(), "b".to_string()], "\r\n"));
    }
}

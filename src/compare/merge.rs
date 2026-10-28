//! 三方合并（diff3）：基准分别和左、右比较，两边改的地方不重叠就自动合并，重叠且改得不一样就是冲突。

use super::diff::{hunks, Hunk};

/// 行范围 [s, e)
pub type Span = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    /// 三边一样
    Same,
    /// 只有左边改了
    Left,
    /// 只有右边改了
    Right,
    /// 两边改成了一样的
    Both,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub kind: ChunkKind,
    pub base: Span,
    pub l: Span,
    pub r: Span,
}

/// 冲突怎么解决
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Left,
    Right,
    /// 两边都要，左边在前
    LeftRight,
    RightLeft,
    Base,
}

pub fn diff3(base: &[u64], l: &[u64], r: &[u64]) -> Vec<Chunk> {
    let hl = hunks(base, l);
    let hr = hunks(base, r);
    let mut out = vec![];
    let (mut il, mut ir) = (0, 0);
    // 当前 base 位置，以及左右相对 base 的偏移
    let mut pos = 0usize;
    let (mut dl, mut dr) = (0isize, 0isize);
    let map = |p: usize, d: isize| (p as isize + d) as usize;
    let overlaps = |a: &Hunk, lo: usize, hi: usize| a.a <= hi && lo <= a.a + a.an;
    loop {
        let nl = hl.get(il);
        let nr = hr.get(ir);
        let start = match (nl, nr) {
            (None, None) => break,
            (Some(a), None) => a.a,
            (None, Some(b)) => b.a,
            (Some(a), Some(b)) => a.a.min(b.a),
        };
        if start > pos {
            out.push(Chunk {
                kind: ChunkKind::Same,
                base: (pos, start),
                l: (map(pos, dl), map(start, dl)),
                r: (map(pos, dr), map(start, dr)),
            });
        }
        // 把重叠的块都吸进来
        let (mut lo, mut hi) = (start, start);
        let (l0, r0) = (il, ir);
        loop {
            let mut grew = false;
            while let Some(h) = hl.get(il) {
                if overlaps(h, lo, hi) {
                    lo = lo.min(h.a);
                    hi = hi.max(h.a + h.an);
                    il += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            while let Some(h) = hr.get(ir) {
                if overlaps(h, lo, hi) {
                    lo = lo.min(h.a);
                    hi = hi.max(h.a + h.an);
                    ir += 1;
                    grew = true;
                } else {
                    break;
                }
            }
            if !grew {
                break;
            }
        }
        let take_l = &hl[l0..il];
        let take_r = &hr[r0..ir];
        let span = |hs: &[Hunk], d: isize| {
            let grow: isize = hs.iter().map(|h| h.bn as isize - h.an as isize).sum();
            ((map(lo, d)), (map(hi, d + grow)), d + grow)
        };
        let (ls, le, ndl) = span(take_l, dl);
        let (rs, re, ndr) = span(take_r, dr);
        let kind = match (take_l.is_empty(), take_r.is_empty()) {
            (false, true) => ChunkKind::Left,
            (true, false) => ChunkKind::Right,
            _ => {
                if l[ls..le] == r[rs..re] {
                    ChunkKind::Both
                } else {
                    ChunkKind::Conflict
                }
            }
        };
        out.push(Chunk {
            kind,
            base: (lo, hi),
            l: (ls, le),
            r: (rs, re),
        });
        pos = hi;
        dl = ndl;
        dr = ndr;
    }
    if pos < base.len() || out.is_empty() {
        out.push(Chunk {
            kind: ChunkKind::Same,
            base: (pos, base.len()),
            l: (map(pos, dl), l.len()),
            r: (map(pos, dr), r.len()),
        });
    }
    out.retain(|c| c.kind != ChunkKind::Same || c.base.1 > c.base.0 || c.l.1 > c.l.0);
    out
}

/// 按每个冲突的选择拼出结果；没选的冲突写成 Git 风格的冲突标记
pub fn build(base: &[String], l: &[String], r: &[String], chunks: &[Chunk], picks: &[Option<Pick>]) -> Vec<String> {
    let mut out = vec![];
    let mut ci = 0;
    for c in chunks {
        let lv = &l[c.l.0..c.l.1];
        let rv = &r[c.r.0..c.r.1];
        let bv = &base[c.base.0..c.base.1];
        match c.kind {
            ChunkKind::Same | ChunkKind::Left | ChunkKind::Both => out.extend_from_slice(lv),
            ChunkKind::Right => out.extend_from_slice(rv),
            ChunkKind::Conflict => {
                match picks.get(ci).copied().flatten() {
                    Some(Pick::Left) => out.extend_from_slice(lv),
                    Some(Pick::Right) => out.extend_from_slice(rv),
                    Some(Pick::LeftRight) => {
                        out.extend_from_slice(lv);
                        out.extend_from_slice(rv);
                    }
                    Some(Pick::RightLeft) => {
                        out.extend_from_slice(rv);
                        out.extend_from_slice(lv);
                    }
                    Some(Pick::Base) => out.extend_from_slice(bv),
                    None => {
                        out.push("<<<<<<< LEFT".into());
                        out.extend_from_slice(lv);
                        out.push("||||||| BASE".into());
                        out.extend_from_slice(bv);
                        out.push("=======".into());
                        out.extend_from_slice(rv);
                        out.push(">>>>>>> RIGHT".into());
                    }
                }
                ci += 1;
            }
        }
    }
    out
}

/// 从带 Git 冲突标记的文件里拆出 ours / base / theirs 三份（没有 diff3 风格的基准段时，基准取两边都没有的空段）
pub fn split_conflict_markers(text: &[String]) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let (mut ours, mut base, mut theirs) = (vec![], vec![], vec![]);
    let mut state = 0; // 0 正常，1 ours，2 base，3 theirs
    let mut found = false;
    for line in text {
        if line.starts_with("<<<<<<<") && state == 0 {
            state = 1;
            found = true;
            continue;
        }
        if line.starts_with("|||||||") && state == 1 {
            state = 2;
            continue;
        }
        if line.starts_with("=======") && (state == 1 || state == 2) {
            state = 3;
            continue;
        }
        if line.starts_with(">>>>>>>") && state == 3 {
            state = 0;
            continue;
        }
        match state {
            0 => {
                ours.push(line.clone());
                base.push(line.clone());
                theirs.push(line.clone());
            }
            1 => ours.push(line.clone()),
            2 => base.push(line.clone()),
            _ => theirs.push(line.clone()),
        }
    }
    (found && state == 0).then_some((ours, base, theirs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

    fn k(s: &str) -> (Vec<String>, Vec<u64>) {
        let v: Vec<String> = if s.is_empty() { vec![] } else { s.split('|').map(str::to_string).collect() };
        let h = v
            .iter()
            .map(|x| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                x.hash(&mut h);
                h.finish()
            })
            .collect();
        (v, h)
    }

    fn merged(b: &str, l: &str, r: &str) -> (Vec<ChunkKind>, String) {
        let (bs, bk) = k(b);
        let (ls, lk) = k(l);
        let (rs, rk) = k(r);
        let c = diff3(&bk, &lk, &rk);
        let kinds = c.iter().map(|c| c.kind).collect();
        (kinds, build(&bs, &ls, &rs, &c, &[]).join("|"))
    }

    #[test]
    fn auto_merge() {
        let (kinds, out) = merged("a|b|c|d|e", "a|B|c|d|e", "a|b|c|D|e");
        assert_eq!(kinds, vec![ChunkKind::Same, ChunkKind::Left, ChunkKind::Same, ChunkKind::Right, ChunkKind::Same]);
        assert_eq!(out, "a|B|c|D|e");
    }

    #[test]
    fn conflict() {
        let (kinds, out) = merged("a|b|c", "a|X|c", "a|Y|c");
        assert_eq!(kinds, vec![ChunkKind::Same, ChunkKind::Conflict, ChunkKind::Same]);
        assert_eq!(out, "a|<<<<<<< LEFT|X|||||||| BASE|b|=======|Y|>>>>>>> RIGHT|c");
        let (kinds, out) = merged("a|b|c", "a|X|c", "a|X|c");
        assert_eq!(kinds[1], ChunkKind::Both);
        assert_eq!(out, "a|X|c");
    }

    #[test]
    fn inserts_and_picks() {
        let (bs, bk) = k("a|c");
        let (ls, lk) = k("a|L|c");
        let (rs, rk) = k("a|R|c");
        let c = diff3(&bk, &lk, &rk);
        assert_eq!(c.iter().filter(|c| c.kind == ChunkKind::Conflict).count(), 1);
        assert_eq!(build(&bs, &ls, &rs, &c, &[Some(Pick::LeftRight)]).join("|"), "a|L|R|c");
        assert_eq!(build(&bs, &ls, &rs, &c, &[Some(Pick::Right)]).join("|"), "a|R|c");
        // 一边在末尾追加，一边改开头
        let (_, out) = merged("a|b", "A|b", "a|b|c");
        assert_eq!(out, "A|b|c");
        let (_, out) = merged("", "x", "");
        assert_eq!(out, "x");
    }

    #[test]
    fn markers() {
        let t: Vec<String> = "a|<<<<<<< HEAD|x|=======|y|>>>>>>> br|b".split('|').map(str::to_string).collect();
        let (o, b, th) = split_conflict_markers(&t).unwrap();
        assert_eq!((o.join(","), b.join(","), th.join(",")), ("a,x,b".into(), "a,b".into(), "a,y,b".into()));
    }
}

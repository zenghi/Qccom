//! 比较报告：生成一个独立的 HTML 文件，可以直接发给同事用浏览器打开。

use super::diff::{Kind, Row};
use super::folder::{Entry, St};
use crate::i18n::t;

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\t' => o.push_str("    "),
            c => o.push(c),
        }
    }
    o
}

const CSS: &str = r#"
:root{--bg:#fff;--fg:#1f2328;--mute:#656d76;--line:#d0d7de;--add:#dafbe1;--del:#ffebe9;--chg:#fff8c5;--hl:#ffd33d66}
@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--mute:#8d96a0;--line:#30363d;--add:#12361f;--del:#3d1418;--chg:#3a3112;--hl:#bb800966}}
body{background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,"Segoe UI",system-ui,sans-serif;margin:24px}
h1{font-size:18px;margin:0 0 4px}.sub{color:var(--mute);margin-bottom:16px}
table{border-collapse:collapse;width:100%;font:12.5px/1.45 ui-monospace,Consolas,"JetBrains Mono",monospace}
td,th{border-bottom:1px solid var(--line);padding:1px 8px;vertical-align:top;white-space:pre-wrap;word-break:break-all}
th{text-align:left;color:var(--mute);font-weight:600;position:sticky;top:0;background:var(--bg)}
td.n{color:var(--mute);text-align:right;width:1%;white-space:nowrap;user-select:none}
tr.chg td.t{background:var(--chg)}tr.del td.l,tr.del td.lt{background:var(--del)}tr.add td.r,tr.add td.rt{background:var(--add)}
tr.chg td.lt{background:var(--del)}tr.chg td.rt{background:var(--add)}
mark{background:var(--hl);color:inherit;border-radius:2px}
.gap td{color:var(--mute);text-align:center;background:transparent}
.st-diff{color:#cf222e}.st-left{color:#8250df}.st-right{color:#0969da}.st-same{color:var(--mute)}
"#;

fn page(title: &str, sub: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{}</title><style>{CSS}</style></head><body><h1>{}</h1><div class=\"sub\">{}</div>{body}<p class=\"sub\">QCCOM · {}</p></body></html>",
        esc(title),
        esc(title),
        esc(sub),
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

fn marked(s: &str, ranges: &[(usize, usize)]) -> String {
    let mut o = String::new();
    let mut p = 0;
    for &(a, b) in ranges {
        if a < p || b > s.len() || !s.is_char_boundary(a) || !s.is_char_boundary(b) {
            continue;
        }
        o.push_str(&esc(&s[p..a]));
        o.push_str("<mark>");
        o.push_str(&esc(&s[a..b]));
        o.push_str("</mark>");
        p = b;
    }
    o.push_str(&esc(&s[p..]));
    o
}

/// 文本比较报告；context = 改动前后保留几行（None = 全文）
pub fn text(name_l: &str, name_r: &str, la: &[String], lb: &[String], rows: &[Row], context: Option<usize>) -> String {
    let keep: Vec<bool> = match context {
        None => vec![true; rows.len()],
        Some(c) => {
            let mut k = vec![false; rows.len()];
            for (i, r) in rows.iter().enumerate() {
                if !matches!(r.kind, Kind::Same | Kind::Minor) {
                    for x in k.iter_mut().take((i + c + 1).min(rows.len())).skip(i.saturating_sub(c)) {
                        *x = true;
                    }
                }
            }
            k
        }
    };
    let changed = rows.iter().filter(|r| !matches!(r.kind, Kind::Same | Kind::Minor)).count();
    let mut b = format!("<table><tr><th></th><th>{}</th><th></th><th>{}</th></tr>", esc(name_l), esc(name_r));
    let mut skipped = false;
    for (i, r) in rows.iter().enumerate() {
        if !keep[i] {
            if !skipped {
                b.push_str("<tr class=\"gap\"><td colspan=\"4\">⋯</td></tr>");
            }
            skipped = true;
            continue;
        }
        skipped = false;
        let cls = match r.kind {
            Kind::Same | Kind::Minor => "",
            Kind::Changed => "chg",
            Kind::LeftOnly => "del",
            Kind::RightOnly => "add",
        };
        let (ml, mr) = match (r.kind, r.l, r.r) {
            (Kind::Changed, Some(x), Some(y)) => super::diff::inline(&la[x], &lb[y]),
            _ => (vec![], vec![]),
        };
        let n = |x: Option<usize>| x.map(|x| (x + 1).to_string()).unwrap_or_default();
        let txt = |x: Option<usize>, v: &[String], m: &[(usize, usize)]| x.map(|x| marked(&v[x], m)).unwrap_or_default();
        b.push_str(&format!(
            "<tr class=\"{cls}\"><td class=\"n l\">{}</td><td class=\"lt\">{}</td><td class=\"n r\">{}</td><td class=\"rt\">{}</td></tr>",
            n(r.l),
            txt(r.l, la, &ml),
            n(r.r),
            txt(r.r, lb, &mr)
        ));
    }
    b.push_str("</table>");
    let sub = format!("{}  ↔  {}  ·  {}", name_l, name_r, crate::i18n::tf("{} 行不同", &[&changed]));
    page(t("文本比较报告"), &sub, &b)
}

/// 文件夹比较报告；only_diff = 只列出不同的
pub fn folder(left: &str, right: &str, es: &[Entry], only_diff: bool) -> String {
    let mut b = format!("<table><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr>", t("路径"), t("状态"), t("左侧"), t("右侧"));
    let mut n = (0, 0, 0);
    for e in es {
        if e.dir {
            continue;
        }
        let (cls, label) = match e.st {
            St::Same => ("st-same", t("相同")),
            St::Diff | St::Pending | St::Error => {
                n.0 += 1;
                ("st-diff", t("不同"))
            }
            St::LeftOnly => {
                n.1 += 1;
                ("st-left", t("只在左侧"))
            }
            St::RightOnly => {
                n.2 += 1;
                ("st-right", t("只在右侧"))
            }
        };
        if only_diff && e.st == St::Same {
            continue;
        }
        let m = |x: Option<super::folder::Meta>| {
            x.map(|m| {
                let time = chrono::DateTime::from_timestamp(m.mtime, 0)
                    .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                format!("{}  {time}", m.size)
            })
            .unwrap_or_default()
        };
        b.push_str(&format!("<tr><td>{}</td><td class=\"{cls}\">{}</td><td>{}</td><td>{}</td></tr>", esc(&e.rel), label, m(e.l), m(e.r)));
    }
    b.push_str("</table>");
    let sub = format!("{left}  ↔  {right}  ·  {}", crate::i18n::tf("{} 个不同，{} 个只在左侧，{} 个只在右侧", &[&n.0, &n.1, &n.2]));
    page(t("文件夹比较报告"), &sub, &b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html() {
        let a: Vec<String> = vec!["a".into(), "x<1>".into(), "c".into()];
        let b: Vec<String> = vec!["a".into(), "x<2>".into(), "c".into()];
        let rows = super::super::diff::align(&a, &b, &Default::default(), super::super::diff::CommentStyle::None, &[]);
        let h = text("l.c", "r.c", &a, &b, &rows, Some(0));
        assert!(h.contains("x&lt;<mark>1</mark>&gt;"));
        assert!(h.contains("⋯"));
    }
}

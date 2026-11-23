//! Find property spellings even while the configuration is being edited.
use std::ops::Range;

fn strings(text: &str, mut visit: impl FnMut(Range<usize>, bool)) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n').unwrap_or(b.len() - i);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4);
            }
            quote @ (b'"' | b'\'') => {
                let start = i + 1;
                i = start;
                while i < b.len() && b[i] != quote && b[i] != b'\n' {
                    i += if b[i] == b'\\' && i + 1 < b.len() { 2 } else { 1 };
                }
                let closed = b.get(i) == Some(&quote);
                visit(start..i, closed);
                i += usize::from(closed);
            }
            _ => i += 1,
        }
    }
}

pub(crate) fn keys(text: &str) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    strings(text, |r, closed| {
        if closed && r.len() <= 256 && !r.is_empty() && text[r.end + 1..].trim_start().starts_with(':') {
            found.insert(&text[r]);
        }
    });
    found.into_iter().map(str::to_owned).collect()
}

pub(super) fn quoted_range(line: &str, caret: usize) -> Option<Range<usize>> {
    // Keys offered by the index are at most 256 bytes. Limit work on minified JSON lines.
    let left = line.ceil_char_boundary(caret.saturating_sub(512));
    let right = line.floor_char_boundary((caret + 512).min(line.len()));
    if left == 0 && right == line.len() {
        let mut range = None;
        strings(line, |r, _| {
            if r.contains(&caret) || r.end == caret {
                range = Some(r);
            }
        });
        return range;
    }
    let b = line.as_bytes();
    let delimiter = |i: usize| matches!(b[i], b'"' | b'\'') && b[left..i].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 0;
    let open = (left..caret).rev().find(|&i| delimiter(i))?;
    let prev = line[left..open].trim_end().chars().next_back();
    if prev.is_some_and(|c| !matches!(c, '{' | '[' | ':' | ',')) {
        return None;
    }
    let end = (caret..right).find(|&i| b[i] == b[open] && delimiter(i)).unwrap_or(right);
    Some(open + 1..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn punctuation_unicode_and_commented_out_keys() {
        let s = r#"{ // "ignore": 1
            "serial.port-name": "value", "波特率": 1, /* "unused": "" */
            "serial.port-name": "other", "escaped\"key": 4
        }"#;
        assert_eq!(keys(s), [r#"escaped\"key"#, "serial.port-name", "波特率"]);
        assert_eq!(quoted_range(r#"  "serial.po": 1"#, 12), Some(3..12));
    }
}

//! 数据转换：一段数据在 HEX、文字（UTF-8 / GBK）、Base64、C 数组、整数和浮点数之间互转。

use encoding_rs::GBK;

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum Input {
    #[default]
    Hex,
    Text,
    Base64,
    /// 十进制或 0x 开头的整数，按 8 字节小端放进字节里
    Number,
}

/// 把输入解析成字节
pub fn parse(kind: Input, s: &str, gbk: bool) -> Result<Vec<u8>, String> {
    match kind {
        Input::Hex => parse_hex(s),
        Input::Text => Ok(unescape(s, gbk)),
        Input::Base64 => base64_decode(s),
        Input::Number => {
            let t = s.trim().replace('_', "");
            let v: i128 = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i128::from_str_radix(h, 16).map_err(|e| e.to_string())?
            } else if let Some(b) = t.strip_prefix("0b") {
                i128::from_str_radix(b, 2).map_err(|e| e.to_string())?
            } else if t.contains(['.', 'e', 'E']) {
                // 浮点数：按 float32 / float64 都列出来，这里给 f64 的字节
                let f: f64 = t.parse().map_err(|_| crate::i18n::t("不是数字").to_string())?;
                return Ok(f.to_le_bytes().to_vec());
            } else {
                t.parse().map_err(|_| crate::i18n::t("不是数字").to_string())?
            };
            let b = (v as u64).to_le_bytes();
            // 去掉高位多余的 0 / FF，至少留 1 字节
            let n = if v >= 0 {
                8 - b.iter().rev().take_while(|&&x| x == 0).count()
            } else {
                8 - b.iter().rev().take_while(|&&x| x == 0xFF).count()
            };
            Ok(b[..n.max(1)].to_vec())
        }
    }
}

/// 文字里认 C 风格转义：\\n \\r \\t \\0 \\\\ \\xNN，其余原样按编码转成字节
fn unescape(s: &str, gbk: bool) -> Vec<u8> {
    let enc = |t: &str, out: &mut Vec<u8>| {
        if gbk {
            out.extend_from_slice(&GBK.encode(t).0)
        } else {
            out.extend_from_slice(t.as_bytes())
        }
    };
    let mut out = vec![];
    let mut it = s.char_indices().peekable();
    let mut start = 0;
    while let Some((i, c)) = it.next() {
        if c != '\\' {
            continue;
        }
        let Some(&(_, n)) = it.peek() else { break };
        let (b, len) = match n {
            'n' => (Some(b'\n'), 2),
            'r' => (Some(b'\r'), 2),
            't' => (Some(b'\t'), 2),
            '0' => (Some(0), 2),
            '\\' => (Some(b'\\'), 2),
            'x' => (s.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok()), 4),
            _ => (None, 0),
        };
        if let Some(b) = b {
            enc(&s[start..i], &mut out);
            out.push(b);
            start = i + len;
            for _ in 1..len {
                it.next();
            }
        }
    }
    enc(&s[start..], &mut out);
    out
}

/// 宽松的 HEX：允许空格、逗号、0x 前缀、换行
pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    let cleaned = s.replace("0x", " ").replace("0X", " ");
    for c in cleaned.chars() {
        if c.is_ascii_hexdigit() {
            digits.push(c);
        } else if !(c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '{' | '}' | '[' | ']')) {
            return Err(crate::i18n::tf("不是 HEX 字符：{}", &[&c]));
        }
    }
    if digits.len() % 2 == 1 {
        digits.insert(0, '0');
    }
    (0..digits.len()).step_by(2).map(|i| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

pub fn c_array(b: &[u8]) -> String {
    let body: Vec<String> = b.chunks(12).map(|c| c.iter().map(|x| format!("0x{x:02X}")).collect::<Vec<_>>().join(", ")).collect();
    format!("{{ {} }}", body.join(",\n  "))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    let mut acc = 0u32;
    let mut n = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return Err(crate::i18n::t("不是合法的 Base64").into()),
        };
        acc = (acc << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
        }
    }
    Ok(out)
}

pub fn base64(b: &[u8]) -> String {
    let mut s = String::new();
    for c in b.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(v >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// 文字显示：不可见字符写成 \xNN
pub fn text(b: &[u8], gbk: bool) -> String {
    let esc = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '\n' => "\\n".to_string(),
                '\r' => "\\r".to_string(),
                '\t' => "\\t".to_string(),
                '\\' => "\\\\".to_string(),
                c if c.is_control() => format!("\\x{:02X}", c as u32),
                c => c.to_string(),
            })
            .collect()
    };
    if gbk {
        return esc(&GBK.decode(b).0);
    }
    // 不是合法 UTF-8 的字节原样显示成 \xNN，比替换成 � 更有用
    let mut out = String::new();
    for ch in b.utf8_chunks() {
        out += &esc(ch.valid());
        for x in ch.invalid() {
            out += &format!("\\x{x:02X}");
        }
    }
    out
}

/// 按字节数能解释出的数值：(名字, 小端, 大端)
pub fn numbers(b: &[u8]) -> Vec<(&'static str, String, String)> {
    let mut out = vec![];
    let le = |n: usize| -> Option<[u8; 8]> {
        if b.len() < n {
            return None;
        }
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(&b[..n]);
        Some(a)
    };
    let be = |n: usize| -> Option<[u8; 8]> {
        if b.len() < n {
            return None;
        }
        let mut a = [0u8; 8];
        for i in 0..n {
            a[i] = b[n - 1 - i];
        }
        Some(a)
    };
    for (name, n, signed) in [
        ("u8", 1, false),
        ("i8", 1, true),
        ("u16", 2, false),
        ("i16", 2, true),
        ("u32", 4, false),
        ("i32", 4, true),
        ("u64", 8, false),
        ("i64", 8, true),
    ] {
        let (Some(l), Some(bb)) = (le(n), be(n)) else { continue };
        let fmt = |a: [u8; 8]| {
            let u = u64::from_le_bytes(a);
            if signed {
                let shift = 64 - n * 8;
                (((u << shift) as i64) >> shift).to_string()
            } else {
                u.to_string()
            }
        };
        out.push((name, fmt(l), fmt(bb)));
    }
    if b.len() >= 4 {
        let l = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let bb = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        out.push(("float", fmt_f(l as f64), fmt_f(bb as f64)));
    }
    if b.len() >= 8 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        out.push(("double", fmt_f(f64::from_le_bytes(a)), fmt_f(f64::from_be_bytes(a))));
    }
    out
}

fn fmt_f(v: f64) -> String {
    if v.is_nan() || v.is_infinite() || (v != 0.0 && (v.abs() > 1e12 || v.abs() < 1e-6)) {
        format!("{v:e}")
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// 输入一个浮点数时，float32 的字节（HEX，小端 / 大端）
pub fn float32_bytes(s: &str) -> Option<(String, String)> {
    let f: f32 = s.trim().parse().ok()?;
    Some((hex(&f.to_le_bytes()), hex(&f.to_be_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(parse_hex("0x12, 0xAB cd\n0f").unwrap(), vec![0x12, 0xAB, 0xCD, 0x0F]);
        assert_eq!(parse_hex("abc").unwrap(), vec![0x0A, 0xBC]);
        assert!(parse_hex("zz").is_err());
        assert_eq!(base64(b"hello!?"), "aGVsbG8hPw==");
        assert_eq!(base64_decode("aGVsbG8hPw==").unwrap(), b"hello!?");
        assert_eq!(parse(Input::Number, "0x1234", false).unwrap(), vec![0x34, 0x12]);
        assert_eq!(parse(Input::Number, "-2", false).unwrap(), vec![0xFE]);
        assert_eq!(parse(Input::Text, "中", true).unwrap(), vec![0xD6, 0xD0]);
        assert_eq!(text(&[0xD6, 0xD0, 0x0A], true), "中\\n");
        // 文字 ↔ 字节来回转不丢东西（含非法 UTF-8、反斜杠、控制字符）
        let raw = [b'A', 0x00, 0x80, b'\\', b'x', 0x0D, 0x0A, 0xE4, 0xB8, 0xAD];
        assert_eq!(text(&raw, false), "A\\x00\\x80\\\\x\\r\\n中");
        assert_eq!(parse(Input::Text, &text(&raw, false), false).unwrap(), raw);
        assert_eq!(parse(Input::Text, "a\\q\\x4", false).unwrap(), b"a\\q\\x4");
        let n = numbers(&[0x00, 0x00, 0x80, 0x3F]);
        assert!(n.iter().any(|(k, l, _)| *k == "float" && l == "1"));
        assert!(n.iter().any(|(k, l, b)| *k == "i16" && l == "0" && b == "0"));
        assert_eq!(c_array(&[1, 2]), "{ 0x01, 0x02 }");
        assert_eq!(float32_bytes("1.0").unwrap().0, "00 00 80 3F");
    }
}

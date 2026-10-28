//! 二进制格式解码成树：CBOR、MessagePack、Protobuf（没有 .proto 时按线格式猜）。

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub key: String,
    pub ty: String,
    pub val: String,
    /// JSON 里的写法（叶子）
    pub json: String,
    pub off: usize,
    pub len: usize,
    pub kids: Vec<Node>,
    /// true：子节点是 键 / 值 交替（map）
    pub map: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Fmt {
    #[default]
    Auto,
    Cbor,
    MsgPack,
    Protobuf,
}

impl Fmt {
    pub const ALL: [Fmt; 4] = [Fmt::Auto, Fmt::Cbor, Fmt::MsgPack, Fmt::Protobuf];
    pub fn label(self) -> &'static str {
        match self {
            Fmt::Auto => "Auto",
            Fmt::Cbor => "CBOR",
            Fmt::MsgPack => "MessagePack",
            Fmt::Protobuf => "Protobuf",
        }
    }
}

fn leaf(ty: &str, val: String, json: String, off: usize, len: usize) -> Node {
    Node {
        ty: ty.into(),
        val,
        json,
        off,
        len,
        ..Default::default()
    }
}

fn hexs(b: &[u8]) -> String {
    let s: String = b.iter().take(64).map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ");
    if b.len() > 64 {
        s + " …"
    } else {
        s
    }
}

fn text_node(ty: &str, b: &[u8], off: usize, len: usize) -> Node {
    match std::str::from_utf8(b) {
        Ok(s) => leaf(ty, format!("\"{s}\""), serde_json::to_string(s).unwrap_or_default(), off, len),
        Err(_) => leaf(ty, hexs(b), format!("\"{}\"", hexs(b)), off, len),
    }
}

fn float_json(f: f64) -> String {
    if f.is_finite() {
        format!("{f}")
    } else {
        "null".into()
    }
}

fn half(h: u16) -> f64 {
    let e = (h >> 10) & 0x1F;
    let m = (h & 0x3FF) as f64;
    let v = if e == 0 {
        m * 2f64.powi(-24)
    } else if e == 31 {
        if m == 0.0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        (1.0 + m / 1024.0) * 2f64.powi(e as i32 - 15)
    };
    if h & 0x8000 != 0 {
        -v
    } else {
        v
    }
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.i..self.i.checked_add(n).ok_or("len")?).ok_or_else(|| format!("truncated at {}", self.i))?;
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn be(&mut self, n: usize) -> Result<u64, String> {
        Ok(self.take(n)?.iter().fold(0u64, |a, b| (a << 8) | *b as u64))
    }
}

// ------------------------------------------------------------------ CBOR

fn cbor_item(r: &mut Rd, depth: u32) -> Result<Node, String> {
    if depth > 64 {
        return Err("too deep".into());
    }
    let off = r.i;
    let ib = r.u8()?;
    let (major, ai) = (ib >> 5, ib & 0x1F);
    let arg = |r: &mut Rd| -> Result<Option<u64>, String> {
        Ok(match ai {
            0..=23 => Some(ai as u64),
            24 => Some(r.be(1)?),
            25 => Some(r.be(2)?),
            26 => Some(r.be(4)?),
            27 => Some(r.be(8)?),
            31 => None,
            _ => return Err(format!("bad info {ai} at {off}")),
        })
    };
    let mut n = match major {
        0 => {
            let v = arg(r)?.ok_or("indefinite int")?;
            leaf("uint", v.to_string(), v.to_string(), off, 0)
        }
        1 => {
            let v = arg(r)?.ok_or("indefinite int")?;
            let s = format!("-{}", v as u128 + 1);
            leaf("int", s.clone(), s, off, 0)
        }
        2 | 3 => {
            let ty = if major == 2 { "bytes" } else { "text" };
            match arg(r)? {
                Some(len) => {
                    let b = r.take(len as usize)?.to_vec();
                    if major == 2 {
                        leaf(ty, hexs(&b), format!("\"{}\"", hexs(&b)), off, 0)
                    } else {
                        text_node(ty, &b, off, 0)
                    }
                }
                None => {
                    // 不定长：一串块，0xFF 结束
                    let mut all = vec![];
                    while r.b.get(r.i) != Some(&0xFF) {
                        let c = cbor_item(r, depth + 1)?;
                        all.push(c.val.trim_matches('"').to_string());
                    }
                    r.i += 1;
                    let s = all.join(if major == 2 { " " } else { "" });
                    leaf(ty, if major == 3 { format!("\"{s}\"") } else { s.clone() }, serde_json::to_string(&s).unwrap_or_default(), off, 0)
                }
            }
        }
        4 | 5 => {
            let count = arg(r)?;
            let mut kids = vec![];
            let mut k = 0u64;
            loop {
                match count {
                    Some(c) if k >= c => break,
                    None if r.b.get(r.i) == Some(&0xFF) => {
                        r.i += 1;
                        break;
                    }
                    _ => {}
                }
                if major == 4 {
                    let mut c = cbor_item(r, depth + 1)?;
                    c.key = format!("[{k}]");
                    kids.push(c);
                } else {
                    let key = cbor_item(r, depth + 1)?;
                    let mut v = cbor_item(r, depth + 1)?;
                    v.key = key.val.clone();
                    kids.push(v);
                }
                k += 1;
                if k > 1_000_000 {
                    return Err("too many items".into());
                }
            }
            Node {
                ty: if major == 4 { format!("array({})", kids.len()) } else { format!("map({})", kids.len()) },
                off,
                kids,
                map: major == 5,
                ..Default::default()
            }
        }
        6 => {
            let tag = arg(r)?.ok_or("indefinite tag")?;
            let mut inner = cbor_item(r, depth + 1)?;
            let name = match tag {
                0 => " datetime",
                1 => " epoch",
                2 => " bignum",
                3 => " -bignum",
                32 => " uri",
                55799 => " self-describe",
                _ => "",
            };
            inner.ty = format!("tag {tag}{name} · {}", inner.ty);
            inner.off = off;
            inner
        }
        _ => match ai {
            20 => leaf("bool", "false".into(), "false".into(), off, 0),
            21 => leaf("bool", "true".into(), "true".into(), off, 0),
            22 => leaf("null", "null".into(), "null".into(), off, 0),
            23 => leaf("undefined", "undefined".into(), "null".into(), off, 0),
            25 => {
                let f = half(r.be(2)? as u16);
                leaf("float16", format!("{f}"), float_json(f), off, 0)
            }
            26 => {
                let f = f32::from_bits(r.be(4)? as u32) as f64;
                leaf("float32", format!("{f}"), float_json(f), off, 0)
            }
            27 => {
                let f = f64::from_bits(r.be(8)?);
                leaf("float64", format!("{f}"), float_json(f), off, 0)
            }
            24 => {
                let v = r.u8()?;
                leaf("simple", v.to_string(), v.to_string(), off, 0)
            }
            31 => return Err(format!("unexpected break at {off}")),
            v => leaf("simple", v.to_string(), v.to_string(), off, 0),
        },
    };
    n.len = r.i - off;
    Ok(n)
}

pub fn cbor(b: &[u8]) -> Result<Vec<Node>, String> {
    let mut r = Rd { b, i: 0 };
    let mut out = vec![];
    while r.i < b.len() {
        out.push(cbor_item(&mut r, 0)?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ MessagePack

fn mp_item(r: &mut Rd, depth: u32) -> Result<Node, String> {
    if depth > 64 {
        return Err("too deep".into());
    }
    let off = r.i;
    let t = r.u8()?;
    let int = |v: i128, ty: &str| leaf(ty, v.to_string(), v.to_string(), off, 0);
    let mut n = match t {
        0x00..=0x7f => int(t as i128, "uint"),
        0xe0..=0xff => int(t as i8 as i128, "int"),
        0x80..=0x8f | 0xde | 0xdf | 0x90..=0x9f | 0xdc | 0xdd => {
            let is_map = matches!(t, 0x80..=0x8f | 0xde | 0xdf);
            let count = match t {
                0x80..=0x8f => (t & 0x0f) as u64,
                0x90..=0x9f => (t & 0x0f) as u64,
                0xdc | 0xde => r.be(2)?,
                _ => r.be(4)?,
            };
            if count > (r.b.len() - r.i) as u64 {
                return Err(format!("bad length at {off}"));
            }
            let mut kids = vec![];
            for k in 0..count {
                if is_map {
                    let key = mp_item(r, depth + 1)?;
                    let mut v = mp_item(r, depth + 1)?;
                    v.key = key.val.clone();
                    kids.push(v);
                } else {
                    let mut c = mp_item(r, depth + 1)?;
                    c.key = format!("[{k}]");
                    kids.push(c);
                }
            }
            Node {
                ty: if is_map { format!("map({count})") } else { format!("array({count})") },
                off,
                kids,
                map: is_map,
                ..Default::default()
            }
        }
        0xa0..=0xbf | 0xd9 | 0xda | 0xdb => {
            let len = match t {
                0xa0..=0xbf => (t & 0x1f) as usize,
                0xd9 => r.be(1)? as usize,
                0xda => r.be(2)? as usize,
                _ => r.be(4)? as usize,
            };
            let b = r.take(len)?.to_vec();
            text_node("str", &b, off, 0)
        }
        0xc0 => leaf("nil", "nil".into(), "null".into(), off, 0),
        0xc2 => leaf("bool", "false".into(), "false".into(), off, 0),
        0xc3 => leaf("bool", "true".into(), "true".into(), off, 0),
        0xc4..=0xc6 => {
            let len = r.be(1 << (t - 0xc4))? as usize;
            let b = r.take(len)?.to_vec();
            leaf("bin", hexs(&b), format!("\"{}\"", hexs(&b)), off, 0)
        }
        0xc7..=0xc9 | 0xd4..=0xd8 => {
            let len = match t {
                0xc7 => r.be(1)? as usize,
                0xc8 => r.be(2)? as usize,
                0xc9 => r.be(4)? as usize,
                _ => 1 << (t - 0xd4),
            };
            let ty = r.u8()? as i8;
            let b = r.take(len)?.to_vec();
            // -1 是时间戳扩展
            let v = if ty == -1 && (len == 4 || len == 8) {
                let s = if len == 4 {
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
                } else {
                    (u64::from_be_bytes(b[..8].try_into().unwrap()) & 0x3_FFFF_FFFF) as i64
                };
                chrono::DateTime::from_timestamp(s, 0).map(|d| d.to_rfc3339()).unwrap_or_else(|| s.to_string())
            } else {
                hexs(&b)
            };
            leaf(&format!("ext {ty}"), v.clone(), format!("\"{v}\""), off, 0)
        }
        0xca => {
            let f = f32::from_bits(r.be(4)? as u32) as f64;
            leaf("float32", format!("{f}"), float_json(f), off, 0)
        }
        0xcb => {
            let f = f64::from_bits(r.be(8)?);
            leaf("float64", format!("{f}"), float_json(f), off, 0)
        }
        0xcc..=0xcf => {
            let v = r.be(1 << (t - 0xcc))?;
            int(v as i128, "uint")
        }
        0xd0..=0xd3 => {
            let bytes = 1usize << (t - 0xd0);
            let v = r.be(bytes)?;
            let s = ((v << (64 - bytes * 8)) as i64) >> (64 - bytes * 8);
            int(s as i128, "int")
        }
        _ => return Err(format!("unused type 0x{t:02X} at {off}")),
    };
    n.len = r.i - off;
    Ok(n)
}

pub fn msgpack(b: &[u8]) -> Result<Vec<Node>, String> {
    let mut r = Rd { b, i: 0 };
    let mut out = vec![];
    while r.i < b.len() {
        out.push(mp_item(&mut r, 0)?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ Protobuf

fn varint(r: &mut Rd) -> Result<u64, String> {
    let mut v = 0u64;
    for k in 0..10 {
        let b = r.u8()?;
        v |= ((b & 0x7f) as u64) << (7 * k);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(format!("bad varint at {}", r.i))
}

fn printable(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_control() || c == '\n' || c == '\r' || c == '\t')
}

fn pb_msg(b: &[u8], base: usize, depth: u32) -> Result<Vec<Node>, String> {
    if depth > 32 {
        return Err("too deep".into());
    }
    let mut r = Rd { b, i: 0 };
    let mut out = vec![];
    while r.i < b.len() {
        let off = r.i;
        let tag = varint(&mut r)?;
        let (field, wire) = (tag >> 3, tag & 7);
        if field == 0 || field > 536_870_911 {
            return Err(format!("bad field at {}", base + off));
        }
        let mut n = match wire {
            0 => {
                let v = varint(&mut r)?;
                let z = ((v >> 1) as i64) ^ -((v & 1) as i64);
                let mut s = v.to_string();
                if v > i64::MAX as u64 {
                    s.push_str(&format!(" / {}", v as i64));
                } else if z != v as i64 {
                    s.push_str(&format!("  (zigzag {z})"));
                }
                leaf("varint", s, v.to_string(), base + off, 0)
            }
            1 => {
                let x = u64::from_le_bytes(r.take(8)?.try_into().unwrap());
                let f = f64::from_bits(x);
                let mut s = format!("{x}");
                if f.is_finite() && f != 0.0 && f.abs() > 1e-30 && f.abs() < 1e30 {
                    s.push_str(&format!("  (double {f})"));
                }
                leaf("fixed64", s, x.to_string(), base + off, 0)
            }
            5 => {
                let x = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
                let f = f32::from_bits(x);
                let mut s = format!("{x}");
                if f.is_finite() && f != 0.0 && f.abs() > 1e-20 && f.abs() < 1e20 {
                    s.push_str(&format!("  (float {f})"));
                }
                leaf("fixed32", s, x.to_string(), base + off, 0)
            }
            2 => {
                let len = varint(&mut r)? as usize;
                let start = r.i;
                let body = r.take(len)?;
                // 先试嵌套消息，再试字符串，最后当字节
                let sub = if len > 0 { pb_msg(body, base + start, depth + 1).ok() } else { None };
                let txt = std::str::from_utf8(body).ok().filter(|s| printable(s));
                match (sub, txt) {
                    (Some(kids), None) | (Some(kids), Some(_)) if !kids.is_empty() && txt.is_none_or(|t| t.len() < 2 || kids.len() > 1) => Node {
                        ty: "message".into(),
                        off: base + off,
                        kids,
                        ..Default::default()
                    },
                    (_, Some(s)) => leaf("string", format!("\"{s}\""), serde_json::to_string(s).unwrap_or_default(), base + off, 0),
                    _ => leaf("bytes", hexs(body), format!("\"{}\"", hexs(body)), base + off, 0),
                }
            }
            _ => return Err(format!("wire type {wire} at {}", base + off)),
        };
        n.key = field.to_string();
        n.len = r.i - off;
        out.push(n);
    }
    Ok(out)
}

pub fn protobuf(b: &[u8]) -> Result<Vec<Node>, String> {
    pb_msg(b, 0, 0)
}

/// 自动识别：哪个能把数据完整解析完用哪个，优先 Protobuf 以外的（它太宽松）
pub fn auto(b: &[u8]) -> (Fmt, Result<Vec<Node>, String>) {
    if let Ok(v) = cbor(b) {
        if v.len() == 1 && !v[0].kids.is_empty() {
            return (Fmt::Cbor, Ok(v));
        }
    }
    if let Ok(v) = msgpack(b) {
        if v.len() == 1 && !v[0].kids.is_empty() {
            return (Fmt::MsgPack, Ok(v));
        }
    }
    if let Ok(v) = protobuf(b) {
        return (Fmt::Protobuf, Ok(v));
    }
    if let Ok(v) = cbor(b) {
        return (Fmt::Cbor, Ok(v));
    }
    (Fmt::MsgPack, msgpack(b))
}

pub fn decode(b: &[u8], f: Fmt) -> (Fmt, Result<Vec<Node>, String>) {
    match f {
        Fmt::Auto => auto(b),
        Fmt::Cbor => (f, cbor(b)),
        Fmt::MsgPack => (f, msgpack(b)),
        Fmt::Protobuf => (f, protobuf(b)),
    }
}

/// 转成 JSON 文本（Protobuf 的重复字段合成数组）
pub fn to_json(nodes: &[Node], proto: bool) -> String {
    fn one(n: &Node, proto: bool) -> serde_json::Value {
        if n.kids.is_empty() && n.ty != "message" && !n.ty.starts_with("map") && !n.ty.starts_with("array") {
            return serde_json::from_str(&n.json).unwrap_or(serde_json::Value::String(n.val.clone()));
        }
        if n.map || proto {
            let mut m = serde_json::Map::new();
            for k in &n.kids {
                let key = k.key.trim_matches('"').to_string();
                let v = one(k, proto);
                match m.get_mut(&key) {
                    Some(serde_json::Value::Array(a)) if proto => a.push(v),
                    Some(old) if proto => *old = serde_json::Value::Array(vec![old.clone(), v]),
                    _ => {
                        m.insert(key, v);
                    }
                }
            }
            serde_json::Value::Object(m)
        } else {
            serde_json::Value::Array(n.kids.iter().map(|k| one(k, proto)).collect())
        }
    }
    let v: Vec<serde_json::Value> = if proto {
        vec![one(
            &Node {
                ty: "message".into(),
                kids: nodes.to_vec(),
                ..Default::default()
            },
            true,
        )]
    } else {
        nodes.iter().map(|n| one(n, false)).collect()
    };
    let v = if v.len() == 1 { v.into_iter().next().unwrap() } else { serde_json::Value::Array(v) };
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbor_map() {
        // {"a": 1, "b": [2, -3], "c": true, "d": 1.5}
        let b = [0xA4, 0x61, 0x61, 0x01, 0x61, 0x62, 0x82, 0x02, 0x22, 0x61, 0x63, 0xF5, 0x61, 0x64, 0xF9, 0x3E, 0x00];
        let v = cbor(&b).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kids.len(), 4);
        assert_eq!(v[0].kids[1].kids[1].val, "-3");
        assert_eq!(v[0].kids[3].val, "1.5");
        let j: serde_json::Value = serde_json::from_str(&to_json(&v, false)).unwrap();
        assert_eq!(j["b"][1], -3);
        assert_eq!(auto(&b).0, Fmt::Cbor);
    }

    #[test]
    fn msgpack_map() {
        // {"compact": true, "schema": 0}
        let b = [0x82, 0xA7, b'c', b'o', b'm', b'p', b'a', b'c', b't', 0xC3, 0xA6, b's', b'c', b'h', b'e', b'm', b'a', 0x00];
        let v = msgpack(&b).unwrap();
        assert_eq!(v[0].kids[0].key, "\"compact\"");
        assert_eq!(v[0].kids[0].val, "true");
        assert_eq!(auto(&b).0, Fmt::MsgPack);
        let j: serde_json::Value = serde_json::from_str(&to_json(&v, false)).unwrap();
        assert_eq!(j["schema"], 0);
    }

    #[test]
    fn proto() {
        // field 1 varint 150, field 2 string "testing", field 3 { field 1: 1 }
        let b = [0x08, 0x96, 0x01, 0x12, 0x07, b't', b'e', b's', b't', b'i', b'n', b'g', 0x1A, 0x02, 0x08, 0x01];
        let v = protobuf(&b).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].json, "150");
        assert_eq!(v[1].val, "\"testing\"");
        assert_eq!(v[2].ty, "message");
        assert_eq!(v[2].kids[0].json, "1");
        let j: serde_json::Value = serde_json::from_str(&to_json(&v, true)).unwrap();
        assert_eq!(j["2"], "testing");
        assert_eq!(j["3"]["1"], 1);
    }
}

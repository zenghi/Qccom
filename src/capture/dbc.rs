//! Classic CAN DBC numeric signals, Intel/Motorola byte order and simple multiplexing.
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Database(pub BTreeMap<u32, Vec<Signal>>);
pub struct Signal {
    pub name: String,
    pub unit: String,
    start: usize,
    len: usize,
    little: bool,
    signed: bool,
    scale: f64,
    offset: f64,
    mux: Option<u64>,
    selector: bool,
    float: bool,
}
impl Signal {
    fn raw(&self, data: &[u8]) -> Option<u64> {
        let mut bit = self.start;
        let mut raw = 0;
        for k in 0..self.len {
            let val = u64::from((*data.get(bit / 8)? >> (bit % 8)) & 1);
            if self.little {
                raw |= val << k;
                bit += 1;
            } else {
                raw = (raw << 1) | val;
                bit = if bit % 8 == 0 { bit + 15 } else { bit - 1 };
            }
        }
        Some(raw)
    }
}
impl Database {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut db = Self::default();
        let mut id = None;
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if let Some(body) = line.strip_prefix("SIG_VALTYPE_ ") {
                let mut w = body.trim_end_matches(';').split_whitespace();
                let id = w.next().and_then(|s| s.parse::<u32>().ok());
                let name = w.next();
                let kind = w.last();
                let signal = id
                    .and_then(|id| db.0.get_mut(&id))
                    .and_then(|all| all.iter_mut().find(|s| Some(s.name.as_str()) == name))
                    .ok_or_else(|| format!("DBC line {}", i + 1))?;
                match (kind, signal.len) {
                    (Some("0"), _) => {}
                    (Some("1"), 32) | (Some("2"), 64) => signal.float = true,
                    _ => return Err(format!("Unsupported floating type on DBC line {}", i + 1)),
                }
                continue;
            }
            if line.starts_with("SG_MUL_VAL_ ") {
                return Err(format!("Extended multiplexing on DBC line {}", i + 1));
            }
            if let Some(body) = line.strip_prefix("BO_ ") {
                id = body.split_whitespace().next().and_then(|s| s.parse::<u32>().ok());
            }
            if let Some(body) = line.strip_prefix("SG_ ") {
                let parsed = (|| {
                    let (head, layout) = body.split_once(':')?;
                    let mut words = head.split_whitespace();
                    let name = words.next()?.to_owned();
                    let mux = words.next();
                    let mut fields = layout.split_whitespace();
                    let bits = fields.next()?;
                    let (start, rest) = bits.split_once('|')?;
                    let (len, fmt) = rest.split_once('@')?;
                    let len: usize = len.parse().ok()?;
                    let start: usize = start.parse().ok()?;
                    if len == 0 || len > 64 || start >= 64 || !matches!(fmt, "1+" | "1-" | "0+" | "0-") {
                        return None;
                    }
                    let coef = fields.next()?.trim_matches(['(', ')']);
                    let (scale, offset) = coef.split_once(',')?;
                    let unit = layout.split('"').nth(1).unwrap_or("").into();
                    let branch = match mux {
                        Some("M") | None => None,
                        Some(m) => Some(m.strip_prefix('m')?.parse().ok()?),
                    };
                    Some((
                        id?,
                        Signal {
                            name,
                            unit,
                            start,
                            len,
                            little: fmt.starts_with('1'),
                            signed: fmt.ends_with('-'),
                            scale: scale.parse().ok()?,
                            offset: offset.parse().ok()?,
                            mux: branch,
                            float: false,
                            selector: mux == Some("M"),
                        },
                    ))
                })()
                .ok_or_else(|| format!("DBC line {}", i + 1))?;
                db.0.entry(parsed.0).or_default().push(parsed.1);
            }
        }
        if db.0.is_empty() {
            return Err("DBC contains no supported signals".into());
        }
        Ok(db)
    }
    pub fn decode(&self, id: u32, extended: bool, data: &[u8], mut emit: impl FnMut(&str, &str, f64)) {
        let Some(signals) = self.0.get(&(id | if extended { 1 << 31 } else { 0 })) else { return };
        let mux = signals.iter().find(|s| s.selector).and_then(|s| s.raw(data));
        for s in signals {
            if s.mux.is_some() && s.mux != mux {
                continue;
            }
            if let Some(bits) = s.raw(data) {
                let v = if s.float {
                    if s.len == 32 {
                        f32::from_bits(bits as u32) as f64
                    } else {
                        f64::from_bits(bits)
                    }
                } else if s.signed {
                    (((bits << (64 - s.len)) as i64) >> (64 - s.len)) as f64
                } else {
                    bits as f64
                };
                emit(&s.name, &s.unit, v * s.scale + s.offset);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decode_endian_mux() {
        let db = Database::parse("BO_ 12 Msg: 8 ECU\n SG_ Mode M : 0|8@1+ (1,0) [0|255] \"\" ECU\n SG_ Torque m1 : 15|16@0- (0.5,1) [-200|200] \"Nm\" ECU").unwrap();
        let mut r = vec![];
        db.decode(12, false, &[1, 255, 252], |n, _, v| r.push((n.to_owned(), v)));
        assert_eq!(r[1], ("Torque".into(), -1.));
    }
}

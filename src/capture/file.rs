//! Portable session files, CSV export and sorted MDF 4.10 numeric time series.
use super::*;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
fn extension(p: &Path) -> String {
    p.extension().unwrap_or_default().to_string_lossy().to_lowercase()
}
pub fn save(path: &Path, c: &Capture) -> io::Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))?;
    {
        let mut w = io::BufWriter::new(tmp.as_file_mut());
        match extension(path).as_str() {
            "mf4" => w.write_all(&mdf(c))?,
            "csv" => {
                writeln!(w, "time_s,channel,unit,value")?;
                for ch in &c.channels {
                    let name = ch.name.replace('"', "\"\"");
                    let unit = ch.unit.replace('"', "\"\"");
                    for &(t, v) in &ch.points {
                        writeln!(w, "{t},\"{name}\",\"{unit}\",{v}")?
                    }
                }
            }
            "qcap" => serde_json::to_writer(&mut w, c)?,
            _ => return Err(invalid("Choose .mf4, .csv or .qcap")),
        };
        w.flush()?;
    }
    tmp.persist(path)?;
    Ok(())
}
pub fn load_range(path: &Path, window: Option<(f64, f64)>) -> io::Result<Capture> {
    if extension(path) == "qrec" {
        return super::stream::load(path, window);
    }
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    const LIMIT: u64 = 128 << 20;
    if f.metadata()?.len() > LIMIT {
        return Err(invalid("Capture exceeds 128 MiB"));
    }
    let mut data = vec![];
    f.take(LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > LIMIT {
        return Err(invalid("Capture exceeds 128 MiB"));
    }
    let is_mdf = extension(path) == "mf4";
    let c: Capture = if is_mdf { read_mdf(&data, window)? } else { serde_json::from_slice(&data)? };
    if c.channels.len() > CHANNELS
        || c.events.len() > EVENTS
        || !c.events.iter().map(|e| e.t).is_sorted()
        || c.events.iter().any(|e| !e.t.is_finite())
        || c.channels
            .iter()
            .any(|ch| ch.points.len() > HISTORY || ch.points.iter().any(|(t, v)| !t.is_finite() || !v.is_finite()) || !ch.points.iter().map(|p| p.0).is_sorted())
    {
        return Err(invalid("Invalid or oversized capture"));
    }
    Ok(if let Some((a, b)) = window.filter(|_| !is_mdf) { c.slice(a, b) } else { c })
}
fn block(b: &mut Vec<u8>, id: &[u8; 4], links: &[usize], data: &[u8]) -> usize {
    b.resize(b.len().next_multiple_of(8), 0);
    let pos = b.len();
    b.extend(id);
    b.extend([0; 4]);
    b.extend(((24 + links.len() * 8 + data.len()) as u64).to_le_bytes());
    b.extend((links.len() as u64).to_le_bytes());
    for &l in links {
        b.extend((l as u64).to_le_bytes())
    }
    b.extend(data);
    pos
}
fn text(b: &mut Vec<u8>, t: &str) -> usize {
    let mut d = t.as_bytes().to_vec();
    d.push(0);
    block(b, b"##TX", &[], &d)
}
fn mdf(c: &Capture) -> Vec<u8> {
    let mut b = vec![0; 64];
    b[..24].copy_from_slice(b"MDF     4.10    QCCOM   ");
    b[28..30].copy_from_slice(&410u16.to_le_bytes());
    let mut hd = [0; 32];
    hd[..8].copy_from_slice(&std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos().to_le_bytes()[..8]);
    let header = block(&mut b, b"##HD", &[0; 6], &hd);
    let time = text(&mut b, "time");
    let sec = text(&mut b, "s");
    let mut next = 0;
    for ch in c.channels.iter().rev() {
        let mut data = Vec::with_capacity(ch.points.len() * 16);
        for &(t, v) in &ch.points {
            data.extend(t.to_le_bytes());
            data.extend(v.to_le_bytes());
        }
        let dt = block(&mut b, b"##DT", &[], &data);
        let name = text(&mut b, &ch.name);
        let unit = text(&mut b, &ch.unit);
        let mut fields = [0; 72];
        fields[2] = 4;
        fields[4] = 8;
        fields[8] = 64;
        let val = block(&mut b, b"##CN", &[0, 0, name, 0, 0, 0, unit, 0], &fields);
        fields[0] = 2;
        fields[1] = 1;
        fields[4] = 0;
        let master = block(&mut b, b"##CN", &[val, 0, time, 0, 0, 0, sec, 0], &fields);
        let mut group = [0; 32];
        group[8..16].copy_from_slice(&(ch.points.len() as u64).to_le_bytes());
        group[24] = 16;
        let cg = block(&mut b, b"##CG", &[0, master, 0, 0, 0, 0], &group);
        next = block(&mut b, b"##DG", &[next, cg, dt, 0], &[0; 8]);
    }
    b[header + 24..header + 32].copy_from_slice(&(next as u64).to_le_bytes());
    b
}
fn u64at(d: &[u8], i: usize) -> Option<usize> {
    usize::try_from(u64::from_le_bytes(d.get(i..i.checked_add(8)?)?.try_into().ok()?)).ok()
}
struct Block<'a> {
    links: &'a [u8],
    data: &'a [u8],
}
impl Block<'_> {
    fn link(&self, i: usize) -> io::Result<usize> {
        u64at(self.links, i * 8).ok_or_else(|| invalid("Missing MDF link"))
    }
}
fn read<'a>(b: &'a [u8], at: usize, id: &[u8; 4]) -> io::Result<Block<'a>> {
    let err = || invalid("Unsupported or damaged MDF4 block");
    let d = b.get(at..).ok_or_else(err)?;
    if d.get(..4) != Some(id) {
        return Err(err());
    }
    let len = u64at(d, 8).ok_or_else(err)?;
    let n = u64at(d, 16).and_then(|n| n.checked_mul(8)).and_then(|n| n.checked_add(24)).ok_or_else(err)?;
    let d = d.get(..len).ok_or_else(err)?;
    Ok(Block {
        links: d.get(24..n).ok_or_else(err)?,
        data: d.get(n..).ok_or_else(err)?,
    })
}
fn read_text(b: &[u8], at: usize) -> io::Result<String> {
    if at == 0 {
        return Ok(String::new());
    }
    let d = read(b, at, b"##TX")?.data;
    Ok(String::from_utf8_lossy(d.split(|x| *x == 0).next().unwrap_or_default()).into_owned())
}
const MDF_LIMIT: usize = 128 << 20;
fn record_bytes(b: &[u8], at: usize) -> io::Result<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    fn chunk(b: &[u8], at: usize) -> io::Result<Cow<'_, [u8]>> {
        if b.get(at..).is_some_and(|s| s.starts_with(b"##DT")) {
            return Ok(Cow::Borrowed(read(b, at, b"##DT")?.data));
        }
        let dz = read(b, at, b"##DZ")?.data;
        let err = || invalid("Invalid MDF compressed record block");
        if dz.len() < 24 || &dz[..2] != b"DT" {
            return Err(err());
        }
        let len = u64at(dz, 8).ok_or_else(err)?;
        let size = u64at(dz, 16).ok_or_else(err)?;
        if len > MDF_LIMIT || size > dz.len() - 24 {
            return Err(err());
        }
        let data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&dz[24..24 + size], len).map_err(|_| err())?;
        if data.len() != len {
            return Err(err());
        }
        if dz[2] == 0 {
            return Ok(Cow::Owned(data));
        }
        let w = u32::from_le_bytes(dz[4..8].try_into().unwrap()) as usize;
        if dz[2] != 1 || w == 0 || w > len {
            return Err(err());
        }
        let rows = len / w;
        let mut out = vec![0; len];
        for (i, v) in data[..rows * w].iter().enumerate() {
            out[(i % rows) * w + i / rows] = *v;
        }
        out[rows * w..].copy_from_slice(&data[rows * w..]);
        Ok(Cow::Owned(out))
    }
    let mut at = at;
    if b.get(at..).is_some_and(|s| s.starts_with(b"##HL")) {
        at = read(b, at, b"##HL")?.link(0)?
    }
    if !b.get(at..).is_some_and(|s| s.starts_with(b"##DL")) {
        return chunk(b, at);
    }
    let mut out = vec![];
    let mut visited = HashSet::new();
    while at != 0 {
        if !visited.insert(at) {
            return Err(invalid("Cyclic MDF data list"));
        }
        let dl = read(b, at, b"##DL")?;
        for i in 1..dl.links.len() / 8 {
            let bytes = chunk(b, dl.link(i)?)?;
            if bytes.len() > MDF_LIMIT - out.len() {
                return Err(invalid("Expanded MDF group exceeds 128 MiB"));
            }
            out.extend_from_slice(&bytes);
        }
        at = dl.link(0)?;
    }
    Ok(Cow::Owned(out))
}
#[derive(Clone, Copy)]
struct Field {
    offset: usize,
    kind: u8,
    bytes: usize,
    factor: f64,
    bias: f64,
}
impl Field {
    fn value(self, rec: &[u8]) -> f64 {
        let src = &rec[self.offset..self.offset + self.bytes];
        let mut raw = [0; 8];
        let u = if self.kind % 2 == 0 {
            raw[..self.bytes].copy_from_slice(src);
            u64::from_le_bytes(raw)
        } else {
            raw[8 - self.bytes..].copy_from_slice(src);
            u64::from_be_bytes(raw)
        };
        let v = match self.kind {
            0 | 1 => u as f64,
            2 | 3 => {
                let shift = (8 - self.bytes) * 8;
                (((u << shift) as i64) >> shift) as f64
            }
            _ if self.bytes == 4 => f32::from_bits(u as u32) as f64,
            _ => f64::from_bits(u),
        };
        v * self.factor + self.bias
    }
}
fn read_mdf(b: &[u8], window: Option<(f64, f64)>) -> io::Result<Capture> {
    let err = || invalid("Supported MDF4: sorted numeric groups with a time master, byte-aligned integer/float, identity or linear conversion");
    if b.get(..8) != Some(b"MDF     ") {
        return Err(err());
    }
    let mut dg = read(b, 64, b"##HD")?.link(0)?;
    let mut seen = HashSet::new();
    let mut cap = Capture::default();
    while dg != 0 {
        if !seen.insert(dg) {
            return Err(err());
        }
        let d = read(b, dg, b"##DG")?;
        let cg = read(b, d.link(1)?, b"##CG")?;
        let dt = record_bytes(b, d.link(2)?)?;
        if cg.link(0)? != 0 || d.data.first() != Some(&0) || cg.data.len() < 32 {
            return Err(err());
        }
        let count = u64at(cg.data, 8).ok_or_else(err)?;
        let row = u32::from_le_bytes(cg.data[24..28].try_into().unwrap()) as usize;
        if row == 0 || count.checked_mul(row).is_none_or(|n| n > dt.len()) || cg.data[28..32] != [0; 4] {
            return Err(err());
        }
        let mut cn = cg.link(1)?;
        let mut master = None;
        let mut fields = vec![];
        while cn != 0 {
            if !seen.insert(cn) {
                return Err(err());
            }
            let ch = read(b, cn, b"##CN")?;
            let d = ch.data;
            if d.len() < 24 || d[2] > 5 || d[3] != 0 || d[12] & 3 != 0 {
                return Err(err());
            }
            let conv = ch.link(4)?;
            let (bias, factor) = if conv == 0 {
                (0., 1.)
            } else {
                let cc = read(b, conv, b"##CC")?.data;
                match cc.first() {
                    Some(0) => (0., 1.),
                    Some(1) if cc.len() >= 40 => (f64::from_le_bytes(cc[24..32].try_into().unwrap()), f64::from_le_bytes(cc[32..40].try_into().unwrap())),
                    _ => return Err(err()),
                }
            };
            let start = u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize;
            let bits = u32::from_le_bytes(d[8..12].try_into().unwrap());
            if !matches!(bits, 8 | 16 | 32 | 64) || (d[2] >= 4 && bits < 32) || start > row || bits as usize / 8 > row - start {
                return Err(err());
            }
            let field = Field {
                offset: start,
                bytes: bits as usize / 8,
                kind: d[2],
                bias,
                factor,
            };
            if d[0] == 2 && d[1] == 1 {
                master = Some(field);
            } else if d[0] == 0 {
                if cap.channels.len() >= CHANNELS {
                    return Err(err());
                }
                fields.push((field, cap.channels.len()));
                cap.channels.push(Channel {
                    name: read_text(b, ch.link(2)?)?,
                    unit: read_text(b, ch.link(6)?)?,
                    ..Default::default()
                });
            } else {
                return Err(err());
            }
            cn = ch.link(0)?;
        }
        let master = master.ok_or_else(err)?;
        for rec in dt.chunks_exact(row).take(count) {
            let t = master.value(rec);
            if !t.is_finite() {
                return Err(err());
            }
            let (from, to) = window.unwrap_or((0., f64::MAX));
            if t < from || t > to {
                continue;
            }
            let t = t - from;
            for &(field, id) in &fields {
                cap.point(id, t, field.value(rec));
            }
        }
        dg = d.link(0)?;
    }
    Ok(cap)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mf4_interchange() {
        let mut c = Capture::default();
        for i in 0..50 {
            c.point(0, i as f64 / 10., i as f64);
        }
        c.channels[0].unit = "rpm".into();
        let b = mdf(&c);
        let load = read_mdf(&b, None).unwrap();
        assert_eq!(load.channels[0].points, c.channels[0].points);
        assert_eq!(load.channels[0].unit, "rpm");
        assert!(read_mdf(&b[..80], None).is_err());
    }
}

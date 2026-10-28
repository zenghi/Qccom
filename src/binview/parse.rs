//! 固件文件解析：ELF、Intel HEX / S-record / BIN、GNU ld 的 map 文件、文本差异。
//! 全部手写，不依赖额外的库；只取查看器需要的信息。

use std::collections::HashMap;

// ------------------------------------------------------------------ ELF

pub struct Elf {
    pub class64: bool,
    pub le: bool,
    pub machine: u16,
    pub entry: u64,
    pub sections: Vec<Sec>,
    pub segments: Vec<Seg>,
    pub syms: Vec<ESym>,
}

pub struct Sec {
    pub name: String,
    pub typ: u32,
    pub flags: u64,
    pub addr: u64,
    pub size: u64,
    /// 在文件里的偏移
    pub off: u64,
}

pub struct Seg {
    pub typ: u32,
    pub flags: u32,
    pub vaddr: u64,
    pub paddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub off: u64,
}

pub struct ESym {
    pub name: String,
    pub value: u64,
    pub size: u64,
    /// STT_*：1 对象，2 函数
    pub kind: u8,
    pub global: bool,
    pub sec: String,
}

pub const SHF_WRITE: u64 = 1;
pub const SHF_ALLOC: u64 = 2;
pub const SHF_EXEC: u64 = 4;
pub const SHT_NOBITS: u32 = 8;

struct Rd<'a> {
    b: &'a [u8],
    le: bool,
}

impl Rd<'_> {
    fn n(&self, at: usize, len: usize) -> Option<u64> {
        let s = self.b.get(at..at + len)?;
        let mut v = 0u64;
        if self.le {
            for (i, x) in s.iter().enumerate() {
                v |= (*x as u64) << (8 * i);
            }
        } else {
            for x in s {
                v = (v << 8) | *x as u64;
            }
        }
        Some(v)
    }
    fn u16(&self, at: usize) -> Option<u16> {
        self.n(at, 2).map(|v| v as u16)
    }
    fn u32(&self, at: usize) -> Option<u32> {
        self.n(at, 4).map(|v| v as u32)
    }
    fn word(&self, at: usize, w64: bool) -> Option<u64> {
        self.n(at, if w64 { 8 } else { 4 })
    }
    fn cstr(&self, at: usize) -> String {
        let s = self.b.get(at..).unwrap_or(&[]);
        let end = s.iter().position(|c| *c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    }
}

pub fn is_elf(b: &[u8]) -> bool {
    b.starts_with(b"\x7fELF")
}

pub fn parse_elf(b: &[u8]) -> Result<Elf, String> {
    if !is_elf(b) || b.len() < 52 {
        return Err("not an ELF file".into());
    }
    let w = b[4] == 2;
    let r = Rd { b, le: b[5] != 2 };
    let bad = || "truncated ELF".to_string();
    let machine = r.u16(18).ok_or_else(bad)?;
    let entry = r.word(24, w).ok_or_else(bad)?;
    let (phoff, shoff) = if w { (r.word(32, w), r.word(40, w)) } else { (r.word(28, w), r.word(32, w)) };
    let (phoff, shoff) = (phoff.ok_or_else(bad)? as usize, shoff.ok_or_else(bad)? as usize);
    let base = if w { 54 } else { 42 };
    let phentsize = r.u16(base).ok_or_else(bad)? as usize;
    let phnum = r.u16(base + 2).ok_or_else(bad)? as usize;
    let shentsize = r.u16(base + 4).ok_or_else(bad)? as usize;
    let shnum = r.u16(base + 6).ok_or_else(bad)? as usize;
    let shstrndx = r.u16(base + 8).ok_or_else(bad)? as usize;

    // 段头：(名字偏移, 类型, 标志, 地址, 文件偏移, 大小, link, entsize)
    let mut raw = vec![];
    for i in 0..shnum.min(4096) {
        let o = shoff + i * shentsize;
        let s = if w {
            (
                r.u32(o),
                r.u32(o + 4),
                r.word(o + 8, true),
                r.word(o + 16, true),
                r.word(o + 24, true),
                r.word(o + 32, true),
                r.u32(o + 40),
                r.word(o + 56, true),
            )
        } else {
            (
                r.u32(o),
                r.u32(o + 4),
                r.u32(o + 8).map(u64::from),
                r.u32(o + 12).map(u64::from),
                r.u32(o + 16).map(u64::from),
                r.u32(o + 20).map(u64::from),
                r.u32(o + 24),
                r.u32(o + 36).map(u64::from),
            )
        };
        match s {
            (Some(a), Some(b2), Some(c), Some(d), Some(e), Some(f), Some(g), Some(h)) => raw.push((a, b2, c, d, e, f, g, h)),
            _ => return Err(bad()),
        }
    }
    let strtab_off = raw.get(shstrndx).map(|s| s.4 as usize);
    let sections: Vec<Sec> = raw
        .iter()
        .map(|s| Sec {
            name: strtab_off.map(|o| r.cstr(o + s.0 as usize)).unwrap_or_default(),
            typ: s.1,
            flags: s.2,
            addr: s.3,
            size: s.5,
            off: s.4,
        })
        .collect();

    let mut segments = vec![];
    for i in 0..phnum.min(1024) {
        let o = phoff + i * phentsize;
        let seg = if w {
            (
                r.u32(o),
                r.u32(o + 4),
                r.word(o + 16, true),
                r.word(o + 24, true),
                r.word(o + 32, true),
                r.word(o + 40, true),
                r.word(o + 8, true),
            )
        } else {
            (
                r.u32(o),
                r.u32(o + 24),
                r.word(o + 8, false),
                r.word(o + 12, false),
                r.word(o + 16, false),
                r.word(o + 20, false),
                r.word(o + 4, false),
            )
        };
        if let (Some(typ), Some(flags), Some(vaddr), Some(paddr), Some(filesz), Some(memsz), Some(off)) = seg {
            segments.push(Seg {
                typ,
                flags,
                vaddr,
                paddr,
                filesz,
                memsz,
                off,
            });
        }
    }

    let mut syms = vec![];
    for s in raw.iter().filter(|s| s.1 == 2) {
        let Some(st) = raw.get(s.6 as usize) else { continue };
        let ent = if s.7 > 0 {
            s.7 as usize
        } else if w {
            24
        } else {
            16
        };
        let n = (s.5 as usize / ent).min(500_000);
        for i in 1..n {
            let o = s.4 as usize + i * ent;
            let (name, info, shndx, value, size) = if w {
                (r.u32(o), b.get(o + 4).copied(), r.u16(o + 6), r.word(o + 8, true), r.word(o + 16, true))
            } else {
                (r.u32(o), b.get(o + 12).copied(), r.u16(o + 14), r.word(o + 4, false), r.word(o + 8, false))
            };
            let (Some(name), Some(info), Some(shndx), Some(value), Some(size)) = (name, info, shndx, value, size) else {
                break;
            };
            let kind = info & 0xf;
            if kind != 1 && kind != 2 {
                continue;
            }
            let name = r.cstr(st.4 as usize + name as usize);
            if name.is_empty() {
                continue;
            }
            syms.push(ESym {
                name,
                value,
                size,
                kind,
                global: info >> 4 != 0,
                sec: sections.get(shndx as usize).map(|s| s.name.clone()).unwrap_or_default(),
            });
        }
    }
    Ok(Elf {
        class64: w,
        le: r.le,
        machine,
        entry,
        sections,
        segments,
        syms,
    })
}

pub fn machine_name(m: u16) -> String {
    match m {
        0x03 => "x86".into(),
        0x08 => "MIPS".into(),
        0x14 => "PowerPC".into(),
        0x28 => "ARM".into(),
        0x2A => "SuperH".into(),
        0x3E => "x86-64".into(),
        0x53 => "AVR".into(),
        0x5E => "Xtensa".into(),
        0x69 => "MSP430".into(),
        0xB7 => "AArch64".into(),
        0xF3 => "RISC-V".into(),
        0x102 => "LoongArch".into(),
        _ => format!("0x{m:X}"),
    }
}

impl Elf {
    /// 和 size 命令一样的 text / data / bss
    pub fn sizes(&self) -> (u64, u64, u64) {
        let (mut text, mut data, mut bss) = (0, 0, 0);
        for s in self.sections.iter().filter(|s| s.flags & SHF_ALLOC != 0) {
            if s.typ == SHT_NOBITS {
                bss += s.size;
            } else if s.flags & SHF_WRITE != 0 {
                data += s.size;
            } else {
                text += s.size;
            }
        }
        (text, data, bss)
    }

    /// 要烧进去的内容：PT_LOAD 段按加载地址（LMA）放，和 objcopy -O binary 一样
    pub fn image(&self, b: &[u8]) -> Image {
        let recs = self
            .segments
            .iter()
            .filter(|s| s.typ == 1 && s.filesz > 0)
            .filter_map(|s| Some((s.paddr, b.get(s.off as usize..(s.off + s.filesz) as usize)?.to_vec())))
            .collect();
        Image::from_records("ELF", recs, 0, Some(self.entry))
    }

    /// 读某个地址处的内容（按段的运行地址找）
    pub fn read<'a>(&self, b: &'a [u8], addr: u64, len: usize) -> Option<&'a [u8]> {
        let s = self
            .sections
            .iter()
            .find(|s| s.typ != SHT_NOBITS && s.flags & SHF_ALLOC != 0 && addr >= s.addr && addr + len as u64 <= s.addr + s.size)?;
        let o = (s.off + addr - s.addr) as usize;
        b.get(o..o + len)
    }
}

// ------------------------------------------------------------------ HEX / BIN

/// 一段段的内存映像（BIN 只有一段，从 0 开始）
pub struct Image {
    pub kind: &'static str,
    pub segs: Vec<(u64, Vec<u8>)>,
    pub errors: usize,
    pub start: Option<u64>,
}

impl Image {
    pub fn bin(b: Vec<u8>) -> Image {
        Image {
            kind: "BIN",
            segs: vec![(0, b)],
            errors: 0,
            start: None,
        }
    }

    pub fn size(&self) -> u64 {
        self.segs.iter().map(|s| s.1.len() as u64).sum()
    }

    pub fn range(&self) -> (u64, u64) {
        let lo = self.segs.first().map_or(0, |s| s.0);
        let hi = self.segs.last().map_or(0, |s| s.0 + s.1.len() as u64);
        (lo, hi)
    }

    pub fn get(&self, addr: u64) -> Option<u8> {
        let i = self.segs.partition_point(|s| s.0 <= addr);
        let (a, d) = self.segs.get(i.checked_sub(1)?)?;
        d.get((addr - a) as usize).copied()
    }

    /// 每行 16 字节的行起始地址；段之间的空洞用 None 表示一行分隔
    pub fn rows(&self) -> Vec<Option<u64>> {
        let mut v = vec![];
        let mut last_end: Option<u64> = None;
        for (a, d) in &self.segs {
            let start = a & !15;
            let end = (a + d.len() as u64 + 15) & !15;
            let mut r = start;
            if let Some(le) = last_end {
                if r < le {
                    r = le;
                } else if r > le {
                    v.push(None);
                }
            }
            while r < end {
                v.push(Some(r));
                r += 16;
            }
            last_end = Some(end);
        }
        v
    }

    /// 把零散的记录合并成连续的段
    pub fn from_records(kind: &'static str, mut recs: Vec<(u64, Vec<u8>)>, errors: usize, start: Option<u64>) -> Image {
        recs.sort_by_key(|r| r.0);
        let mut segs: Vec<(u64, Vec<u8>)> = vec![];
        for (a, d) in recs {
            match segs.last_mut() {
                Some((sa, sd)) if *sa + sd.len() as u64 >= a => {
                    let off = (a - *sa) as usize;
                    if off + d.len() > sd.len() {
                        sd.resize(off + d.len(), 0xFF);
                    }
                    sd[off..off + d.len()].copy_from_slice(&d);
                }
                _ => segs.push((a, d)),
            }
        }
        Image { kind, segs, errors, start }
    }
}

fn hexbytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn parse_ihex(text: &str) -> Option<Image> {
    let mut recs = vec![];
    let (mut base, mut errors, mut start, mut any) = (0u64, 0, None, false);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(b) = line.strip_prefix(':').and_then(hexbytes) else {
            errors += 1;
            continue;
        };
        if b.len() < 5 || b.len() != b[0] as usize + 5 {
            errors += 1;
            continue;
        }
        if b.iter().fold(0u8, |s, x| s.wrapping_add(*x)) != 0 {
            errors += 1;
        }
        any = true;
        let addr = ((b[1] as u64) << 8) | b[2] as u64;
        let data = &b[4..b.len() - 1];
        match b[3] {
            0 => recs.push((base + addr, data.to_vec())),
            1 => break,
            2 if data.len() == 2 => base = (((data[0] as u64) << 8) | data[1] as u64) << 4,
            4 if data.len() == 2 => base = (((data[0] as u64) << 8) | data[1] as u64) << 16,
            3 | 5 if data.len() == 4 => start = Some(data.iter().fold(0u64, |s, x| (s << 8) | *x as u64)),
            _ => {}
        }
    }
    any.then(|| Image::from_records("Intel HEX", recs, errors, start))
}

pub fn parse_srec(text: &str) -> Option<Image> {
    let mut recs = vec![];
    let (mut errors, mut start, mut any) = (0, None, false);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(rest) = line.strip_prefix('S') else {
            errors += 1;
            continue;
        };
        let typ = rest.as_bytes().first().copied().unwrap_or(b'?');
        let Some(b) = hexbytes(&rest[1..]) else {
            errors += 1;
            continue;
        };
        if b.is_empty() || b.len() != b[0] as usize + 1 {
            errors += 1;
            continue;
        }
        if b.iter().fold(0u8, |s, x| s.wrapping_add(*x)) != 0xFF {
            errors += 1;
        }
        any = true;
        let alen = match typ {
            b'1' | b'9' => 2,
            b'2' | b'8' => 3,
            b'3' | b'7' => 4,
            _ => continue,
        };
        if b.len() < 2 + alen {
            errors += 1;
            continue;
        }
        let addr = b[1..1 + alen].iter().fold(0u64, |s, x| (s << 8) | *x as u64);
        match typ {
            b'1' | b'2' | b'3' => recs.push((addr, b[1 + alen..b.len() - 1].to_vec())),
            _ => start = Some(addr),
        }
    }
    any.then(|| Image::from_records("S-Record", recs, errors, start))
}

/// 按扩展名 / 内容识别；返回 None 表示不是固件映像
pub fn load_image(path: &std::path::Path, bytes: &[u8]) -> Option<Image> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "hex" | "ihex" | "ihx" => std::str::from_utf8(bytes).ok().and_then(parse_ihex),
        "s19" | "s28" | "s37" | "srec" | "mot" | "sx" => std::str::from_utf8(bytes).ok().and_then(parse_srec),
        _ => Some(Image::bin(bytes.to_vec())),
    }
}

pub fn crc32(segs: &[(u64, Vec<u8>)]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for (_, d) in segs {
        for b in d {
            c ^= *b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
    }
    !c
}

// ------------------------------------------------------------------ map 文件

pub struct Region {
    pub name: String,
    pub origin: u64,
    pub len: u64,
    pub writable: bool,
    pub used: u64,
}

pub struct OutSec {
    pub name: String,
    pub addr: u64,
    pub size: u64,
    pub load: Option<u64>,
}

pub struct InSec {
    pub name: String,
    pub out: usize,
    pub addr: u64,
    pub size: u64,
    pub obj: String,
}

pub struct MapInfo {
    pub regions: Vec<Region>,
    pub outs: Vec<OutSec>,
    pub ins: Vec<InSec>,
}

fn hexnum(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?, 16).ok()
}

fn short_obj(p: &str) -> String {
    // libc.a(lib_a-memcpy.o) / C:\x\y\foo.o / ./build/foo.o
    let (path, member) = match p.find('(') {
        Some(i) => (&p[..i], &p[i..]),
        None => (p, ""),
    };
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    format!("{name}{member}")
}

pub fn parse_map(text: &str) -> Option<MapInfo> {
    let mut regions = vec![];
    let mut lines = text.lines();
    // 内存配置
    let mut found_mem = false;
    for l in lines.by_ref() {
        if l.starts_with("Memory Configuration") {
            found_mem = true;
            break;
        }
    }
    if !found_mem {
        return None;
    }
    for l in lines.by_ref() {
        if l.starts_with("Linker script and memory map") {
            break;
        }
        let t: Vec<&str> = l.split_whitespace().collect();
        if t.len() >= 3 && t[0] != "Name" && t[0] != "*default*" {
            if let (Some(origin), Some(len)) = (hexnum(t[1]), hexnum(t[2])) {
                let attr = t.get(3).copied().unwrap_or("");
                let up = t[0].to_ascii_uppercase();
                let writable = if attr.is_empty() {
                    up.contains("RAM") || up.contains("TCM")
                } else {
                    attr.contains('w') && !attr.contains("!w")
                };
                regions.push(Region {
                    name: t[0].to_string(),
                    origin,
                    len,
                    writable,
                    used: 0,
                });
            }
        }
    }
    let mut outs: Vec<OutSec> = vec![];
    let mut ins: Vec<InSec> = vec![];
    let mut pending_out: Option<String> = None;
    let mut pending_in: Option<String> = None;
    for l in lines {
        if l.starts_with("/DISCARD/") || l.starts_with("OUTPUT(") {
            pending_out = None;
            pending_in = None;
            continue;
        }
        let t: Vec<&str> = l.split_whitespace().collect();
        if t.is_empty() {
            continue;
        }
        let lead = l.len() - l.trim_start().len();
        let addr_size = |t: &[&str]| -> Option<(u64, u64)> { Some((hexnum(t.first()?)?, hexnum(t.get(1)?)?)) };
        let load_of = |t: &[&str]| t.windows(3).find(|w| w[0] == "load" && w[1] == "address").and_then(|w| hexnum(w[2]));
        if lead == 0 {
            pending_in = None;
            // 输出段：.text 0x... 0x... [load address 0x...]；名字太长时地址在下一行
            if t.len() == 1 && !t[0].starts_with("0x") {
                pending_out = Some(t[0].to_string());
                continue;
            }
            pending_out = None;
            if let Some((a, s)) = addr_size(&t[1..]) {
                outs.push(OutSec {
                    name: t[0].to_string(),
                    addr: a,
                    size: s,
                    load: load_of(&t),
                });
            }
            continue;
        }
        if let Some(name) = pending_out.take() {
            if let Some((a, s)) = addr_size(&t) {
                outs.push(OutSec {
                    name,
                    addr: a,
                    size: s,
                    load: load_of(&t),
                });
            }
            continue;
        }
        if outs.is_empty() {
            continue;
        }
        if lead == 1 && !t[0].starts_with('*') {
            // 输入段： .text.main 0x... 0x... foo.o
            if t.len() == 1 {
                pending_in = Some(t[0].to_string());
                continue;
            }
            pending_in = None;
            if let Some((a, s)) = addr_size(&t[1..]) {
                if t.len() >= 4 && s > 0 {
                    ins.push(InSec {
                        name: t[0].to_string(),
                        out: outs.len() - 1,
                        addr: a,
                        size: s,
                        obj: short_obj(&t[3..].join(" ")),
                    });
                }
            }
            continue;
        }
        if let Some(name) = pending_in.take() {
            if let Some((a, s)) = addr_size(&t) {
                if t.len() >= 3 && s > 0 {
                    ins.push(InSec {
                        name,
                        out: outs.len() - 1,
                        addr: a,
                        size: s,
                        obj: short_obj(&t[2..].join(" ")),
                    });
                }
            }
            continue;
        }
        // 符号行： 0x08000140  main —— 给不带函数名的输入段（.text、COMMON）起名字用
        if t.len() == 2 && t[0].starts_with("0x") && !t[1].starts_with("0x") && !l.contains('=') {
            if let (Some(last), Some(a)) = (ins.last_mut(), hexnum(t[0])) {
                let inside = a >= last.addr && a < last.addr + last.size;
                if inside && matches!(last.name.as_str(), ".text" | ".data" | ".bss" | ".rodata" | "COMMON" | ".sbss" | ".sdata") {
                    last.name = format!("{} {}", last.name, t[1]);
                }
            }
        }
    }
    // ld 给 NOLOAD 的 .bss 也打印 load address，但它并不占 Flash
    for o in outs.iter_mut() {
        let n = o.name.to_ascii_lowercase();
        if ["bss", "noinit", "heap", "stack", "uninit", "zero"].iter().any(|k| n.contains(k)) {
            o.load = None;
        }
    }
    let in_region = |a: u64, r: &Region| a >= r.origin && a < r.origin.saturating_add(r.len);
    for o in outs.iter().filter(|o| o.size > 0) {
        if let Some(r) = regions.iter_mut().find(|r| in_region(o.addr, r)) {
            r.used += o.size;
        }
        if let Some(la) = o.load.filter(|la| *la != o.addr) {
            if let Some(r) = regions.iter_mut().find(|r| in_region(la, r) && !in_region(o.addr, r)) {
                r.used += o.size;
            }
        }
    }
    Some(MapInfo { regions, outs, ins })
}

impl MapInfo {
    /// 输出段占用的 (ROM, RAM)：放在 ROM 区算 ROM；VMA 在 RAM 区算 RAM，且有 ROM 里的加载地址时 ROM 也算一份
    pub fn out_cost(&self, o: &OutSec) -> (u64, u64) {
        let reg = |a: u64| self.regions.iter().find(|r| a >= r.origin && a < r.origin.saturating_add(r.len));
        let (mut rom, mut ram) = (0, 0);
        match reg(o.addr) {
            Some(r) if r.writable => {
                ram = o.size;
                if o.load.filter(|l| *l != o.addr).and_then(reg).is_some_and(|r| !r.writable) {
                    rom = o.size;
                }
            }
            Some(_) => rom = o.size,
            None => {}
        }
        (rom, ram)
    }

    /// 按目标文件汇总：(文件, ROM, RAM)
    pub fn by_object(&self) -> Vec<(String, u64, u64)> {
        let mut m: HashMap<&str, (u64, u64)> = HashMap::new();
        for i in &self.ins {
            let o = &self.outs[i.out];
            if o.size == 0 {
                continue;
            }
            let (rom, ram) = self.out_cost(o);
            let e = m.entry(&i.obj).or_default();
            if rom > 0 {
                e.0 += i.size;
            }
            if ram > 0 {
                e.1 += i.size;
            }
        }
        let mut v: Vec<(String, u64, u64)> = m.into_iter().filter(|(_, (a, b))| a + b > 0).map(|(k, (a, b))| (k.to_string(), a, b)).collect();
        v.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)));
        v
    }
}

/// 输入段的显示名：.text.main → main，.bss.buf → buf
pub fn sym_name(sec: &str) -> &str {
    if let Some((_, sym)) = sec.split_once(' ') {
        return sym;
    }
    for p in [".text.", ".rodata.", ".data.", ".bss.", ".sdata.", ".sbss.", ".srodata.", ".iram1.", ".dram1."] {
        if let Some(s) = sec.strip_prefix(p) {
            return s;
        }
    }
    sec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ihex() {
        let t = ":020000040800F2\n:10000000000000200D010008110100081301000884\n:0400100001020304E2\n:00000001FF\n";
        let img = parse_ihex(t).unwrap();
        assert_eq!(img.segs.len(), 1);
        assert_eq!(img.segs[0].0, 0x0800_0000);
        assert_eq!(img.size(), 20);
        assert_eq!(img.get(0x0800_0010), Some(1));
        assert_eq!(img.errors, 0);
        assert_eq!(img.rows(), vec![Some(0x0800_0000), Some(0x0800_0010)]);
    }

    #[test]
    fn srec() {
        let t = "S00600004844521B\nS1130000285F245F2212226A000424290008237C2A\nS9030000FC\n";
        let img = parse_srec(t).unwrap();
        assert_eq!(img.size(), 16);
        assert_eq!(img.get(0), Some(0x28));
        assert_eq!(img.errors, 0);
    }

    #[test]
    fn gaps() {
        let img = Image::from_records("x", vec![(0x20, vec![1; 4]), (0x100, vec![2; 4])], 0, None);
        assert_eq!(img.rows(), vec![Some(0x20), None, Some(0x100)]);
        assert_eq!(img.get(0x101), Some(2));
        assert_eq!(img.get(0x30), None);
    }

    #[test]
    fn map() {
        let t = "Memory Configuration\n\nName             Origin             Length             Attributes\nFLASH            0x08000000         0x00010000         xr\nRAM              0x20000000         0x00005000         xrw\n*default*        0x00000000         0xffffffff\n\nLinker script and memory map\n\n.isr_vector     0x08000000       0x10\n .isr_vector    0x08000000       0x10 startup.o\n\n.text           0x08000010      0x120\n .text.main     0x08000010       0x20 build/main.o\n                0x08000010                main\n .text.uart_init\n                0x08000030      0x100 build/uart.o\n\n.data           0x20000000        0x8 load address 0x08000130\n .data          0x20000000        0x8 build/main.o\n                0x20000000                counter\n\n.bss            0x20000008       0x40\n COMMON         0x20000008       0x40 build/uart.o\n                0x20000008                rx_buf\n";
        let m = parse_map(t).unwrap();
        assert_eq!(m.regions.len(), 2);
        assert_eq!(m.regions[0].used, 0x10 + 0x120 + 8);
        assert_eq!(m.regions[1].used, 8 + 0x40);
        assert_eq!(m.ins.len(), 5);
        assert_eq!(sym_name(&m.ins[1].name), "main");
        assert_eq!(sym_name(&m.ins[2].name), "uart_init");
        assert_eq!(m.ins[2].obj, "uart.o");
        assert_eq!(sym_name(&m.ins[4].name), "rx_buf");
        let objs = m.by_object();
        assert_eq!(objs[0].0, "uart.o");
    }

    #[test]
    fn elf_self() {
        // 用测试程序自己（Linux 上是 ELF）验证解析不出错
        if let Ok(exe) = std::env::current_exe() {
            let b = std::fs::read(exe).unwrap();
            if is_elf(&b) {
                let e = parse_elf(&b).unwrap();
                assert!(e.sections.iter().any(|s| s.name == ".text"));
                assert!(e.sizes().0 > 0);
            }
        }
    }
}

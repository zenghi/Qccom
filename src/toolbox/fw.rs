//! 固件处理：HEX / BIN / SREC / ELF 互转、多文件合并、按地址拆分、填充空隙，
//! 加自定义升级包头（OTA 头）或在指定地址写 CRC，读 ELF 里的中断向量表。

use serde::{Deserialize, Serialize};

use crate::binview::parse::{self, Image};

/// 读任意固件文件：ELF 取加载段，其余交给 HEX / SREC / BIN 解析
pub fn load_any(path: &std::path::Path) -> Result<Image, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if parse::is_elf(&b) {
        return Ok(parse::parse_elf(&b)?.image(&b));
    }
    parse::load_image(path, &b).ok_or_else(|| format!("{}: ?", path.display()))
}

/// 输出格式
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Fmt {
    #[default]
    Hex,
    Bin,
    Srec,
}

impl Fmt {
    pub const ALL: [Fmt; 3] = [Fmt::Hex, Fmt::Bin, Fmt::Srec];
    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Hex => "hex",
            Fmt::Bin => "bin",
            Fmt::Srec => "srec",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Fmt::Hex => "Intel HEX",
            Fmt::Bin => "BIN",
            Fmt::Srec => "S-Record",
        }
    }
}

/// 把一段数据盖到映像上（后来的覆盖先来的），返回重叠的字节数
pub fn overlay(segs: &mut Vec<(u64, Vec<u8>)>, a: u64, d: &[u8]) -> u64 {
    if d.is_empty() {
        return 0;
    }
    let end = a + d.len() as u64;
    // 和 [a, end] 相交或相邻的段合成一段
    let i0 = segs.partition_point(|s| s.0 + (s.1.len() as u64) < a);
    let mut j = i0;
    let (mut lo, mut hi, mut over) = (a, end, 0u64);
    while j < segs.len() && segs[j].0 <= end {
        let (sa, se) = (segs[j].0, segs[j].0 + segs[j].1.len() as u64);
        lo = lo.min(sa);
        hi = hi.max(se);
        over += se.min(end).saturating_sub(sa.max(a));
        j += 1;
    }
    let mut buf = vec![0xFFu8; (hi - lo) as usize];
    for (sa, sd) in &segs[i0..j] {
        let o = (sa - lo) as usize;
        buf[o..o + sd.len()].copy_from_slice(sd);
    }
    let o = (a - lo) as usize;
    buf[o..o + d.len()].copy_from_slice(d);
    segs.splice(i0..j, [(lo, buf)]);
    over
}

/// 合并的一个输入
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Input {
    pub path: String,
    /// BIN 文件放到哪个地址；HEX / SREC / ELF 是整体偏移（可以是负数，写成 -0x1000）
    pub addr: String,
    pub on: bool,
}

pub fn is_bin(path: &str) -> bool {
    let e = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    !matches!(e.as_str(), "hex" | "ihex" | "ihx" | "s19" | "s28" | "s37" | "srec" | "mot" | "sx" | "elf" | "axf" | "out" | "o") && std::fs::read(path).map(|b| !parse::is_elf(&b)).unwrap_or(true)
}

/// 解析 0x0800_0000、134217728、-0x100、64K、1M 这类地址
pub fn parse_num(s: &str) -> Option<i64> {
    let s = s.trim().replace('_', "");
    if s.is_empty() {
        return Some(0);
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r.trim().to_string()),
        None => (false, s.strip_prefix('+').unwrap_or(&s).trim().to_string()),
    };
    let (body, mul) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1024),
        Some('m' | 'M') if !s.starts_with("0x") && !s.starts_with("0X") => (&s[..s.len() - 1], 1024 * 1024),
        _ => (s.as_str(), 1),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(h) = body.strip_suffix('h').or_else(|| body.strip_suffix('H')) {
        i64::from_str_radix(h, 16).ok()?
    } else {
        body.parse::<i64>().ok()?
    };
    Some(if neg { -v * mul } else { v * mul })
}

/// 一个输入载入后的信息
pub struct Loaded {
    pub kind: &'static str,
    pub lo: u64,
    pub hi: u64,
    pub size: u64,
}

/// 合并结果
pub struct Merged {
    pub img: Image,
    /// 每个输入的信息，或者出错原因
    pub inputs: Vec<Result<Loaded, String>>,
    /// (输入序号, 和前面的输入重叠的字节数)
    pub overlaps: Vec<(usize, u64)>,
}

pub fn merge(inputs: &[Input]) -> Merged {
    let mut segs = vec![];
    let mut infos = vec![];
    let mut overlaps = vec![];
    let mut start = None;
    for (i, inp) in inputs.iter().enumerate() {
        if !inp.on || inp.path.trim().is_empty() {
            infos.push(Err(String::new()));
            continue;
        }
        let Some(off) = parse_num(&inp.addr) else {
            infos.push(Err(format!("? {}", inp.addr)));
            continue;
        };
        let img = match load_any(std::path::Path::new(inp.path.trim())) {
            Ok(i) => i,
            Err(e) => {
                infos.push(Err(e));
                continue;
            }
        };
        let (lo, hi) = img.range();
        if (lo as i64 + off) < 0 {
            infos.push(Err("< 0".into()));
            continue;
        }
        let mut over = 0;
        for (a, d) in &img.segs {
            over += overlay(&mut segs, (*a as i64 + off) as u64, d);
        }
        if over > 0 {
            overlaps.push((i, over));
        }
        if start.is_none() {
            start = img.start;
        }
        infos.push(Ok(Loaded {
            kind: img.kind,
            lo: (lo as i64 + off) as u64,
            hi: (hi as i64 + off) as u64,
            size: img.size(),
        }));
    }
    Merged {
        img: Image {
            kind: "merged",
            segs,
            errors: 0,
            start,
        },
        inputs: infos,
        overlaps,
    }
}

/// 输出前的处理
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Post {
    /// 段之间的空隙填上 fill
    pub fill_gaps: bool,
    pub fill: u8,
    /// 只取这个范围（空 = 全部）
    pub from: String,
    pub to: String,
    /// 末尾补齐到这个字节数的整数倍（0 = 不补）
    pub align: u32,
}

impl Default for Post {
    fn default() -> Self {
        Post {
            fill_gaps: true,
            fill: 0xFF,
            from: String::new(),
            to: String::new(),
            align: 0,
        }
    }
}

/// 截取 [from, to) 的部分
pub fn crop(segs: &[(u64, Vec<u8>)], from: u64, to: u64) -> Vec<(u64, Vec<u8>)> {
    segs.iter()
        .filter_map(|(a, d)| {
            let (s, e) = ((*a).max(from), (a + d.len() as u64).min(to));
            (s < e).then(|| (s, d[(s - a) as usize..(e - a) as usize].to_vec()))
        })
        .collect()
}

pub fn fill_gaps(segs: &[(u64, Vec<u8>)], fill: u8) -> Vec<(u64, Vec<u8>)> {
    let (Some(first), Some(last)) = (segs.first(), segs.last()) else { return vec![] };
    let lo = first.0;
    let hi = last.0 + last.1.len() as u64;
    let mut buf = vec![fill; (hi - lo) as usize];
    for (a, d) in segs {
        let o = (a - lo) as usize;
        buf[o..o + d.len()].copy_from_slice(d);
    }
    vec![(lo, buf)]
}

/// 按设置处理一遍，空隙太大（超过 256 MB）时不填
pub fn post(img: &Image, p: &Post) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let mut segs = img.segs.clone();
    let bad = || crate::i18n::t("地址格式不对").to_string();
    let from = if p.from.trim().is_empty() {
        None
    } else {
        Some(parse_num(&p.from).ok_or_else(bad)?.max(0) as u64)
    };
    let to = if p.to.trim().is_empty() { None } else { Some(parse_num(&p.to).ok_or_else(bad)?.max(0) as u64) };
    if from.is_some() || to.is_some() {
        segs = crop(&segs, from.unwrap_or(0), to.unwrap_or(u64::MAX));
    }
    if p.fill_gaps && segs.len() > 1 {
        let span = segs.last().map(|s| s.0 + s.1.len() as u64).unwrap_or(0) - segs[0].0;
        // 16 MB 以上多半是两段离得太远（比如 BIN 没设加载地址），填出来没有意义
        if span > 16 << 20 {
            return Err(crate::i18n::tf(
                "填充后有 {}，段之间离得太远：检查 BIN 的加载地址，或者关掉“填充空隙”",
                &[&crate::app::human_bytes(span)],
            ));
        }
        segs = fill_gaps(&segs, p.fill);
    }
    if p.align > 1 {
        if let Some(last) = segs.last_mut() {
            let end = last.0 + last.1.len() as u64;
            let pad = (p.align as u64 - end % p.align as u64) % p.align as u64;
            last.1.extend(std::iter::repeat_n(p.fill, pad as usize));
        }
    }
    Ok(segs)
}

/// 按格式输出。BIN 从最低地址开始，中间的空隙用 fill 填
pub fn write(segs: &[(u64, Vec<u8>)], fmt: Fmt, fill: u8, start: Option<u64>) -> Vec<u8> {
    match fmt {
        Fmt::Bin => fill_gaps(segs, fill).pop().map(|s| s.1).unwrap_or_default(),
        Fmt::Hex => write_ihex(segs, start).into_bytes(),
        Fmt::Srec => write_srec(segs, start).into_bytes(),
    }
}

fn hex_rec(len: usize, addr: u16, typ: u8, data: &[u8]) -> String {
    let mut sum = len as u8;
    sum = sum.wrapping_add((addr >> 8) as u8).wrapping_add(addr as u8).wrapping_add(typ);
    let mut s = format!(":{len:02X}{addr:04X}{typ:02X}");
    for b in data {
        sum = sum.wrapping_add(*b);
        s.push_str(&format!("{b:02X}"));
    }
    s.push_str(&format!("{:02X}\n", sum.wrapping_neg()));
    s
}

pub fn write_ihex(segs: &[(u64, Vec<u8>)], start: Option<u64>) -> String {
    let mut out = String::new();
    let mut upper: Option<u64> = None;
    for (a, d) in segs {
        let mut addr = *a;
        let mut rest = &d[..];
        while !rest.is_empty() {
            if upper != Some(addr >> 16) {
                let u = (addr >> 16) as u16;
                out.push_str(&hex_rec(2, 0, 4, &u.to_be_bytes()));
                upper = Some(addr >> 16);
            }
            // 一行最多 16 字节，而且不跨 64K 边界
            let room = (0x1_0000 - (addr & 0xFFFF)) as usize;
            let n = rest.len().min(16).min(room);
            out.push_str(&hex_rec(n, addr as u16, 0, &rest[..n]));
            addr += n as u64;
            rest = &rest[n..];
        }
    }
    if let Some(s) = start {
        out.push_str(&hex_rec(4, 0, 5, &(s as u32).to_be_bytes()));
    }
    out.push_str(":00000001FF\n");
    out
}

fn srec_rec(typ: u8, addr: u32, data: &[u8]) -> String {
    let alen = match typ {
        0 | 1 | 9 | 5 => 2,
        2 | 8 => 3,
        _ => 4,
    };
    let count = alen + data.len() + 1;
    let mut sum = count as u8;
    let mut s = format!("S{typ}{count:02X}");
    for i in (0..alen).rev() {
        let b = (addr >> (8 * i)) as u8;
        sum = sum.wrapping_add(b);
        s.push_str(&format!("{b:02X}"));
    }
    for b in data {
        sum = sum.wrapping_add(*b);
        s.push_str(&format!("{b:02X}"));
    }
    s.push_str(&format!("{:02X}\n", !sum));
    s
}

pub fn write_srec(segs: &[(u64, Vec<u8>)], start: Option<u64>) -> String {
    let mut out = srec_rec(0, 0, b"QCCOM");
    let mut n = 0u32;
    for (a, d) in segs {
        for (i, ch) in d.chunks(16).enumerate() {
            out.push_str(&srec_rec(3, (*a as u32).wrapping_add(i as u32 * 16), ch));
            n += 1;
        }
    }
    if n <= 0xFFFF {
        out.push_str(&srec_rec(5, n, &[]));
    }
    out.push_str(&srec_rec(7, start.unwrap_or(0) as u32, &[]));
    out
}

/// 按地址切成几块：cuts 是切点（升序），返回每块的 (起始, 数据)
pub fn split(segs: &[(u64, Vec<u8>)], cuts: &[u64]) -> Vec<(u64, u64, Vec<(u64, Vec<u8>)>)> {
    let mut bounds = vec![0u64];
    bounds.extend(cuts.iter().copied());
    bounds.push(u64::MAX);
    bounds.windows(2).map(|w| (w[0], w[1], crop(segs, w[0], w[1]))).filter(|p| !p.2.is_empty()).collect()
}

// ------------------------------------------------------------------ 升级包头 / 尾

/// 头里的一个字段
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FieldKind {
    /// 固定数值（魔数等）
    Const,
    /// 版本号 1.2.3 → 每段一个字节，或者整个数值
    Version,
    /// 固件长度
    Length,
    Crc32,
    Crc16,
    /// 累加和（按字段宽度截断）
    Sum,
    /// 打包时的 Unix 时间（秒）
    Time,
    /// 文字，不够长补 0
    Text,
    /// 预留，填 fill
    Reserved,
    /// 前面所有头字段的 CRC32
    HeadCrc,
}

impl FieldKind {
    pub const ALL: [FieldKind; 10] = [
        FieldKind::Const,
        FieldKind::Version,
        FieldKind::Length,
        FieldKind::Crc32,
        FieldKind::Crc16,
        FieldKind::Sum,
        FieldKind::Time,
        FieldKind::Text,
        FieldKind::Reserved,
        FieldKind::HeadCrc,
    ];
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    /// 字节数
    pub size: u32,
    pub value: String,
}

/// 放在前面（头）还是后面（尾）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Place {
    #[default]
    Head,
    Tail,
    /// 写进固件里的指定地址（覆盖原来的内容）
    At,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct PackTpl {
    pub name: String,
    pub fields: Vec<Field>,
    pub little: bool,
    pub place: Place,
    /// Place::At 时的地址
    pub at: String,
    /// 头补齐到这么大（0 = 不补）
    pub head_size: u32,
    /// 固件先补齐到这个字节数的整数倍再算长度和 CRC
    pub align: u32,
    pub fill: u8,
}

impl Default for PackTpl {
    fn default() -> Self {
        presets().remove(0)
    }
}

fn f(name: &str, kind: FieldKind, size: u32, value: &str) -> Field {
    Field {
        name: name.into(),
        kind,
        size,
        value: value.into(),
    }
}

/// 内置的几种格式
pub fn presets() -> Vec<PackTpl> {
    vec![
        PackTpl {
            name: "OTA header 32B".into(),
            fields: vec![
                f("magic", FieldKind::Const, 4, "0x5AA5F00D"),
                f("version", FieldKind::Version, 4, "1.0.0"),
                f("length", FieldKind::Length, 4, ""),
                f("crc32", FieldKind::Crc32, 4, ""),
                f("time", FieldKind::Time, 4, ""),
                f("reserved", FieldKind::Reserved, 8, ""),
                f("head_crc", FieldKind::HeadCrc, 4, ""),
            ],
            little: true,
            place: Place::Head,
            at: String::new(),
            head_size: 32,
            align: 0,
            fill: 0xFF,
        },
        PackTpl {
            name: "CRC32 tail".into(),
            fields: vec![f("crc32", FieldKind::Crc32, 4, "")],
            little: true,
            place: Place::Tail,
            at: String::new(),
            head_size: 0,
            align: 4,
            fill: 0xFF,
        },
        PackTpl {
            name: "Bootloader info @ address".into(),
            fields: vec![
                f("magic", FieldKind::Const, 4, "0x424F4F54"),
                f("length", FieldKind::Length, 4, ""),
                f("crc32", FieldKind::Crc32, 4, ""),
                f("version", FieldKind::Text, 16, "v1.0.0"),
            ],
            little: true,
            place: Place::At,
            at: "0x0800FFE0".into(),
            head_size: 0,
            align: 0,
            fill: 0xFF,
        },
    ]
}

pub fn crc32(d: &[u8]) -> u32 {
    parse::crc32(&[(0, d.to_vec())])
}

/// CRC-16/CCITT-FALSE
pub fn crc16(d: &[u8]) -> u16 {
    let mut c = 0xFFFFu16;
    for b in d {
        c ^= (*b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

fn put(out: &mut Vec<u8>, v: u64, size: u32, little: bool) {
    let n = size.min(8) as usize;
    let mut b: Vec<u8> = (0..n).map(|i| (v >> (8 * i)) as u8).collect();
    if !little {
        b.reverse();
    }
    out.extend(b);
    out.extend(std::iter::repeat_n(0, size as usize - n));
}

/// 版本号：1.2.3 按字段宽度每段一个字节（4 字节时 major.minor.patch.build），纯数字直接当数值
fn version_bytes(s: &str, size: u32, little: bool) -> Vec<u8> {
    let parts: Vec<u64> = s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '_']).filter_map(|p| p.parse().ok()).collect();
    let mut out = vec![];
    if parts.len() <= 1 {
        put(&mut out, parse_num(s).unwrap_or(0) as u64, size, little);
        return out;
    }
    // 大端顺序：major 在最高字节
    let n = size as usize;
    let mut v = vec![0u8; n];
    for (i, p) in parts.iter().take(n).enumerate() {
        v[i] = *p as u8;
    }
    if little {
        v.reverse();
    }
    v
}

/// 打包后的结果和每个字段的值（给界面显示）
pub struct Packed {
    pub segs: Vec<(u64, Vec<u8>)>,
    pub fields: Vec<(String, u64, Vec<u8>)>,
    pub crc: u32,
    pub len: u64,
}

/// fw 是要打包的固件（已经合成一段）；now 是 Unix 时间
pub fn pack(base: u64, mut fw: Vec<u8>, tpl: &PackTpl, now: u64) -> Result<Packed, String> {
    // 写进固件里时，长度和 CRC 只算信息块前面那部分
    let at = if tpl.place == Place::At {
        let a = parse_num(&tpl.at).ok_or("address")? as u64;
        if a < base || a > base + fw.len() as u64 {
            return Err(format!("0x{a:08X} out of range"));
        }
        Some((a - base) as usize)
    } else {
        None
    };
    if tpl.align > 1 && at.is_none() {
        let pad = (tpl.align as usize - fw.len() % tpl.align as usize) % tpl.align as usize;
        fw.extend(std::iter::repeat_n(tpl.fill, pad));
    }
    let body = &fw[..at.unwrap_or(fw.len())];
    let crc = crc32(body);
    let mut head = vec![];
    let mut fields = vec![];
    for fd in &tpl.fields {
        let start = head.len();
        let v: u64 = match fd.kind {
            FieldKind::Const => parse_num(&fd.value).unwrap_or(0) as u64,
            FieldKind::Length => body.len() as u64,
            FieldKind::Crc32 => crc as u64,
            FieldKind::Crc16 => crc16(body) as u64,
            FieldKind::Sum => body.iter().map(|b| *b as u64).sum(),
            FieldKind::Time => now,
            FieldKind::HeadCrc => crc32(&head) as u64,
            _ => 0,
        };
        match fd.kind {
            FieldKind::Version => head.extend(version_bytes(&fd.value, fd.size, tpl.little)),
            FieldKind::Text => {
                let mut b = fd.value.as_bytes().to_vec();
                b.resize(fd.size as usize, 0);
                head.extend(b);
            }
            FieldKind::Reserved => head.extend(std::iter::repeat_n(tpl.fill, fd.size as usize)),
            _ => put(&mut head, v, fd.size, tpl.little),
        }
        fields.push((fd.name.clone(), start as u64, head[start..].to_vec()));
    }
    if tpl.head_size as usize > head.len() && tpl.place == Place::Head {
        head.resize(tpl.head_size as usize, tpl.fill);
    }
    let len = body.len() as u64;
    let segs = match (tpl.place, at) {
        (Place::Head, _) => {
            let mut v = head;
            v.extend(fw);
            vec![(base.saturating_sub(0), v)]
        }
        (Place::Tail, _) => {
            fw.extend(head);
            vec![(base, fw)]
        }
        (Place::At, Some(o)) => {
            if o + head.len() > fw.len() {
                fw.resize(o + head.len(), tpl.fill);
            }
            fw[o..o + head.len()].copy_from_slice(&head);
            vec![(base, fw)]
        }
        _ => unreachable!(),
    };
    Ok(Packed { segs, fields, crc, len })
}

// ------------------------------------------------------------------ 中断向量表

pub struct Vector {
    pub index: usize,
    /// 异常名 / IRQ 编号
    pub slot: String,
    pub addr: u64,
    /// 指向的函数（所有同地址的符号）
    pub names: Vec<String>,
    /// 还指着默认处理函数（没实现）
    pub default: bool,
}

pub struct VecTable {
    pub base: u64,
    pub sp: u64,
    pub vectors: Vec<Vector>,
    pub section: String,
}

const CORTEX_M: [&str; 16] = [
    "Initial SP",
    "Reset",
    "NMI",
    "HardFault",
    "MemManage",
    "BusFault",
    "UsageFault",
    "SecureFault",
    "",
    "",
    "",
    "SVCall",
    "DebugMon",
    "",
    "PendSV",
    "SysTick",
];

/// 从 Cortex-M 的 ELF 里读向量表：找 .isr_vector 之类的段，或者 g_pfnVectors / __Vectors 符号
pub fn vectors(b: &[u8], irq_names: &dyn Fn(usize) -> Option<String>) -> Result<VecTable, String> {
    let e = parse::parse_elf(b)?;
    if e.machine != 40 {
        return Err(format!("{} (Cortex-M only)", parse::machine_name(e.machine)));
    }
    let by_sym = ["g_pfnVectors", "__Vectors", "__isr_vector", "_vectors", "__vector_table", "vector_table", "_vector_table"]
        .iter()
        .find_map(|n| e.syms.iter().find(|s| s.name == *n).map(|s| (s.value, s.size, s.sec.clone())));
    let by_sec = [".isr_vector", ".vectors", ".vector_table", ".intvec", "RESET", ".isr_vectors", ".interrupt_vector"]
        .iter()
        .find_map(|n| e.sections.iter().find(|s| s.name == *n).map(|s| (s.addr, s.size, s.name.clone())));
    let (base, mut size, section) = by_sym.or(by_sec).ok_or("no vector table (.isr_vector / g_pfnVectors)")?;
    if size == 0 {
        size = e.sections.iter().find(|s| s.name == section).map(|s| s.size - (base - s.addr)).unwrap_or(0);
    }
    let n = (size / 4).clamp(2, 512) as usize;
    let data = e.read(b, base, n * 4).or_else(|| e.read(b, base, 64)).ok_or("vector table not in file")?;
    let word = |i: usize| -> u64 {
        let w = [data[i * 4], data[i * 4 + 1], data[i * 4 + 2], data[i * 4 + 3]];
        if e.le {
            u32::from_le_bytes(w) as u64
        } else {
            u32::from_be_bytes(w) as u64
        }
    };
    let n = data.len() / 4;
    let funcs: Vec<&parse::ESym> = e.syms.iter().filter(|s| s.kind == 2).collect();
    let default_addr = funcs.iter().find(|s| s.name == "Default_Handler" || s.name == "Default_IRQHandler").map(|s| s.value & !1);
    let mut vectors = vec![];
    for i in 1..n {
        let addr = word(i);
        let a = addr & !1;
        let mut names: Vec<String> = if addr == 0 {
            vec![]
        } else {
            funcs.iter().filter(|s| s.value & !1 == a).map(|s| s.name.clone()).collect()
        };
        names.sort();
        names.dedup();
        // 自己的名字排前面，Default_Handler 放最后
        names.sort_by_key(|n| n.starts_with("Default"));
        let slot = if i < 16 {
            if CORTEX_M[i].is_empty() {
                "-".to_string()
            } else {
                CORTEX_M[i].to_string()
            }
        } else {
            match irq_names(i - 16) {
                Some(nm) => format!("IRQ {} {nm}", i - 16),
                None => format!("IRQ {}", i - 16),
            }
        };
        let default = addr != 0 && (Some(a) == default_addr || names.iter().all(|n| n.starts_with("Default")) && !names.is_empty());
        vectors.push(Vector { index: i, slot, addr, names, default });
    }
    Ok(VecTable { base, sp: word(0), vectors, section })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_merges() {
        let mut s = vec![];
        assert_eq!(overlay(&mut s, 0x100, &[1; 4]), 0);
        assert_eq!(overlay(&mut s, 0x200, &[2; 4]), 0);
        assert_eq!(s.len(), 2);
        // 相邻的接起来
        assert_eq!(overlay(&mut s, 0x104, &[3; 2]), 0);
        assert_eq!(s[0].1, vec![1, 1, 1, 1, 3, 3]);
        // 跨两段，重叠 2 + 1 字节
        assert_eq!(overlay(&mut s, 0x104, &vec![9; 0x200 - 0x104 + 1]), 3);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, 0x100);
        assert_eq!(s[0].1.len(), 0x104);
        assert_eq!(s[0].1[0x101], 2);
        assert_eq!(s[0].1[0x100], 9);
    }

    #[test]
    fn hex_roundtrip() {
        let segs = vec![(0x0800_FFF8u64, (0..20u8).collect::<Vec<_>>()), (0x2000_0000, vec![0xAA; 3])];
        let h = write_ihex(&segs, Some(0x0800_0101));
        let img = parse::parse_ihex(&h).unwrap();
        assert_eq!(img.errors, 0);
        assert_eq!(img.segs, segs);
        assert_eq!(img.start, Some(0x0800_0101));
        let s = write_srec(&segs, None);
        let img = parse::parse_srec(&s).unwrap();
        assert_eq!(img.errors, 0);
        assert_eq!(img.segs, segs);
    }

    #[test]
    fn nums() {
        assert_eq!(parse_num("0x0800_0000"), Some(0x0800_0000));
        assert_eq!(parse_num("-0x10"), Some(-16));
        assert_eq!(parse_num("64K"), Some(65536));
        assert_eq!(parse_num("8000h"), Some(0x8000));
        assert_eq!(parse_num(""), Some(0));
        assert_eq!(parse_num("zz"), None);
    }

    #[test]
    fn post_and_split() {
        let img = Image::from_records("x", vec![(0x10, vec![1; 4]), (0x20, vec![2; 2])], 0, None);
        let p = Post { align: 8, ..Default::default() };
        let s = post(&img, &p).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].1.len(), 0x18);
        assert_eq!(s[0].1[4], 0xFF);
        let parts = split(&img.segs, &[0x20]);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].2, vec![(0x20, vec![2, 2])]);
        let bin = write(&img.segs, Fmt::Bin, 0, None);
        assert_eq!(bin.len(), 0x12);
    }

    #[test]
    fn packing() {
        let tpl = &presets()[0];
        let p = pack(0x0800_8000, b"123456789".to_vec(), tpl, 1_700_000_000).unwrap();
        let v = &p.segs[0].1;
        assert_eq!(v.len(), 32 + 9);
        assert_eq!(&v[0..4], &0x5AA5F00Du32.to_le_bytes());
        assert_eq!(&v[4..8], &[0, 0, 0, 1]);
        assert_eq!(&v[8..12], &9u32.to_le_bytes());
        assert_eq!(&v[12..16], &0xCBF43926u32.to_le_bytes());
        let hc = crc32(&v[..28]);
        assert_eq!(&v[28..32], &hc.to_le_bytes());
        assert_eq!(crc16(b"123456789"), 0x29B1);
        // 写到指定地址：长度只算前面
        let mut t = presets()[2].clone();
        t.at = "0x10".into();
        let p = pack(0, vec![0u8; 0x40], &t, 0).unwrap();
        assert_eq!(p.len, 0x10);
        assert_eq!(&p.segs[0].1[0x10..0x14], &0x424F4F54u32.to_le_bytes());
        assert_eq!(p.segs[0].1.len(), 0x40);
        // 尾部 CRC，先 4 字节对齐
        let p = pack(0, vec![1, 2, 3], &presets()[1], 0).unwrap();
        assert_eq!(p.segs[0].1.len(), 8);
        assert_eq!(&p.segs[0].1[4..], &crc32(&[1, 2, 3, 0xFF]).to_le_bytes());
    }
}

//! 固件体积对比：两个 .map 或 .elf，看哪些函数 / 变量 / 文件变大了。

use std::collections::HashMap;

use crate::binview::parse::{self, MapInfo, SHF_ALLOC, SHF_EXEC, SHF_WRITE, SHT_NOBITS};

/// 一个版本里每个符号占的 (ROM, RAM)
pub struct Sizes {
    /// (名字, 文件) → (类别, ROM, RAM)
    pub syms: HashMap<(String, String), (char, u64, u64)>,
    pub rom: u64,
    pub ram: u64,
}

/// 类别：F 函数，V 变量 / 常量，S 其他段
fn kind_of(sec: &str) -> char {
    if sec.starts_with(".text") || sec.starts_with(".iram") || sec.contains("ramfunc") {
        'F'
    } else if [".data", ".bss", ".rodata", ".sdata", ".sbss", ".srodata", ".dram", "COMMON", ".noinit"]
        .iter()
        .any(|p| sec.starts_with(p))
    {
        'V'
    } else {
        'S'
    }
}

pub fn from_map(m: &MapInfo) -> Sizes {
    let mut syms: HashMap<(String, String), (char, u64, u64)> = HashMap::new();
    for i in &m.ins {
        let o = &m.outs[i.out];
        let (r, a) = m.out_cost(o);
        if r + a == 0 || i.size == 0 {
            continue;
        }
        let name = parse::sym_name(&i.name).to_string();
        let kind = kind_of(&i.name);
        let e = syms.entry((name, i.obj.clone())).or_insert((kind, 0, 0));
        if r > 0 {
            e.1 += i.size;
        }
        if a > 0 {
            e.2 += i.size;
        }
    }
    let (mut rom, mut ram) = (0, 0);
    for o in &m.outs {
        let (r, a) = m.out_cost(o);
        rom += r;
        ram += a;
    }
    Sizes { syms, rom, ram }
}

pub fn from_elf(e: &parse::Elf) -> Sizes {
    let mut syms: HashMap<(String, String), (char, u64, u64)> = HashMap::new();
    let sec = |n: &str| e.sections.iter().find(|s| s.name == n);
    for s in e.syms.iter().filter(|s| s.size > 0) {
        let Some(sc) = sec(&s.sec) else { continue };
        if sc.flags & SHF_ALLOC == 0 {
            continue;
        }
        let (rom, ram) = if sc.typ == SHT_NOBITS {
            (0, s.size)
        } else if sc.flags & SHF_WRITE != 0 {
            (s.size, s.size)
        } else {
            (s.size, 0)
        };
        // 符号类型优先：常量数组也常放在可执行的 .text 输出段里
        let kind = match s.kind {
            2 => 'F',
            1 => 'V',
            _ if sc.flags & SHF_EXEC != 0 => 'F',
            _ => 'V',
        };
        let e = syms.entry((s.name.clone(), String::new())).or_insert((kind, 0, 0));
        e.1 += rom;
        e.2 += ram;
    }
    let (text, data, bss) = e.sizes();
    Sizes {
        syms,
        rom: text + data,
        ram: data + bss,
    }
}

pub fn load(path: &std::path::Path) -> Result<Sizes, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if parse::is_elf(&b) {
        return Ok(from_elf(&parse::parse_elf(&b)?));
    }
    let text = String::from_utf8_lossy(&b);
    from_lld(&text)
        .or_else(|| from_armlink(&text))
        .or_else(|| parse::parse_map(&text).map(|m| from_map(&m)))
        .ok_or_else(|| crate::i18n::tf("{}：认不出来，支持 GCC / Keil / LLVM 的 .map 和 .elf", &[&path.display()]))
}

/// 只放在 RAM 里的段（不占 Flash）
fn ram_only(sec: &str) -> bool {
    let s = sec.to_ascii_lowercase();
    ["bss", "noinit", "heap", "stack", "common", "zi"].iter().any(|k| s.contains(k))
}

/// LLVM lld 的 -Map：表头是 VMA LMA Size Align Out In Symbol，数字是十六进制，层级靠缩进区分
pub fn from_lld(text: &str) -> Option<Sizes> {
    let mut lines = text.lines();
    let head = lines.by_ref().find(|l| !l.trim().is_empty())?;
    if !(head.contains("VMA") && head.contains("Out") && head.contains("In") && head.contains("Symbol")) {
        return None;
    }
    let (col_out, col_in) = (head.find("Out")?, head.find(" In")? + 1);
    // 输入段：(名字, 文件, 类别, ROM, RAM, 段下面列出的符号)
    let mut ins: Vec<(String, String, char, u64, u64, Vec<String>)> = vec![];
    let (mut rom, mut ram) = (0u64, 0u64);
    // 当前输出段：(ROM?, RAM?)
    let mut cur = (false, false);
    for l in lines {
        let mut it = l.split_whitespace();
        let (Some(vma), Some(lma), Some(size)) = (it.next(), it.next(), it.next()) else { continue };
        let (Ok(vma), Ok(lma), Ok(size)) = (u64::from_str_radix(vma, 16), u64::from_str_radix(lma, 16), u64::from_str_radix(size, 16)) else {
            continue;
        };
        let _align = it.next();
        let Some(name) = it.next() else { continue };
        let col = l.find(name).unwrap_or(0);
        if col < col_in {
            // 输出段：地址为 0 的是调试信息之类，不占空间
            if col != col_out || vma == 0 || name.starts_with(".debug") || name.starts_with(".comment") || name == ".ARM.attributes" {
                cur = (false, false);
                continue;
            }
            cur = if ram_only(name) { (false, true) } else { (true, vma != lma) };
            rom += if cur.0 { size } else { 0 };
            ram += if cur.1 { size } else { 0 };
        } else if col < col_in + 8 && (cur.0 || cur.1) && size > 0 {
            // 输入段：file.o:(.text.foo)
            let Some((file, sec)) = name.split_once(":(") else { continue };
            let sec = sec.trim_end_matches(')');
            let sym = parse::sym_name(sec).to_string();
            let kind = kind_of(sec);
            let file = file.rsplit(['/', '\\']).next().unwrap_or(file).to_string();
            ins.push((sym, file, kind, if cur.0 { size } else { 0 }, if cur.1 { size } else { 0 }, vec![]));
        } else if col >= col_in + 8 && !name.starts_with('$') {
            if let Some(i) = ins.last_mut() {
                i.5.push(name.to_string());
            }
        }
    }
    let mut syms: HashMap<(String, String), (char, u64, u64)> = HashMap::new();
    for (name, file, kind, r, a, list) in ins {
        // 没加 -ffunction-sections 时段名就是 .text / .isr_vector，下面只有一个符号就用符号名
        let name = if name.starts_with('.') && list.len() == 1 { list[0].clone() } else { name };
        let e = syms.entry((name, file)).or_insert((kind, 0, 0));
        e.1 += r;
        e.2 += a;
    }
    Some(Sizes { syms, rom, ram })
}

/// Keil MDK（armlink）的 .map：读 Image Symbol Table 和最后的 Total ROM / RW Size
pub fn from_armlink(text: &str) -> Option<Sizes> {
    if !text.contains("Image Symbol Table") {
        return None;
    }
    let re = regex_lite::Regex::new(r"^\s+(\S+)\s+0x([0-9a-fA-F]+)\s+(Thumb Code|ARM Code|Data)\s+(\d+)\s+(\S+?)\((\S+)\)").ok()?;
    let total = |key: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(key))
            .and_then(|l| l.split(')').nth(1))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
    };
    let mut syms: HashMap<(String, String), (char, u64, u64)> = HashMap::new();
    for l in text.lines() {
        let Some(c) = re.captures(l) else { continue };
        let size: u64 = c[4].parse().unwrap_or(0);
        if size == 0 {
            continue;
        }
        let (name, file, sec) = (c[1].to_string(), c[5].to_string(), &c[6]);
        let code = &c[3] != "Data";
        let (kind, r, a) = if code {
            ('F', size, 0)
        } else if ram_only(sec) {
            ('V', 0, size)
        } else if sec.starts_with(".data") || sec.starts_with(".sdata") || sec.contains("RW") {
            ('V', size, size)
        } else {
            ('V', size, 0)
        };
        let e = syms.entry((name, file)).or_insert((kind, 0, 0));
        e.1 += r;
        e.2 += a;
    }
    Some(Sizes {
        rom: total("Total ROM Size").unwrap_or(0),
        ram: total("Total RW  Size").or_else(|| total("Total RW Size")).unwrap_or(0),
        syms,
    })
}

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub file: String,
    pub kind: char,
    pub old: (u64, u64),
    pub new: (u64, u64),
}

impl Row {
    pub fn delta(&self) -> i64 {
        (self.new.0 + self.new.1) as i64 - (self.old.0 + self.old.1) as i64
    }
    pub fn delta_rom(&self) -> i64 {
        self.new.0 as i64 - self.old.0 as i64
    }
    pub fn delta_ram(&self) -> i64 {
        self.new.1 as i64 - self.old.1 as i64
    }
}

pub struct Diff {
    pub syms: Vec<Row>,
    pub files: Vec<Row>,
    pub old: (u64, u64),
    pub new: (u64, u64),
}

pub fn diff(a: &Sizes, b: &Sizes) -> Diff {
    let mut rows: HashMap<(String, String), Row> = HashMap::new();
    for (k, v, old) in a.syms.iter().map(|(k, v)| (k, v, true)).chain(b.syms.iter().map(|(k, v)| (k, v, false))) {
        let r = rows.entry(k.clone()).or_insert_with(|| Row {
            name: k.0.clone(),
            file: k.1.clone(),
            kind: v.0,
            old: (0, 0),
            new: (0, 0),
        });
        if old {
            r.old = (v.1, v.2);
        } else {
            r.new = (v.1, v.2);
        }
    }
    // 文件改名或换了目录时同名符号对不上：两边各剩一个同名的就配成一对
    let mut only_old: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut only_new: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (k, r) in &rows {
        if r.new == (0, 0) {
            only_old.entry(k.0.clone()).or_default().push(k.clone());
        } else if r.old == (0, 0) {
            only_new.entry(k.0.clone()).or_default().push(k.clone());
        }
    }
    for (name, o) in only_old {
        if let (1, Some(n)) = (o.len(), only_new.get(&name).filter(|n| n.len() == 1)) {
            if let Some(old) = rows.remove(&o[0]) {
                if let Some(r) = rows.get_mut(&n[0]) {
                    r.old = old.old;
                }
            }
        }
    }
    let mut syms: Vec<Row> = rows.into_values().filter(|r| r.old != r.new).collect();
    syms.sort_by(|x, y| y.delta().abs().cmp(&x.delta().abs()).then(x.name.cmp(&y.name)));
    // 按文件汇总（ELF 没有文件信息时就是空的）
    let mut files: HashMap<String, Row> = HashMap::new();
    for (map, old) in [(&a.syms, true), (&b.syms, false)] {
        for (k, v) in map.iter().filter(|(k, _)| !k.1.is_empty()) {
            let r = files.entry(k.1.clone()).or_insert_with(|| Row {
                name: k.1.clone(),
                file: String::new(),
                kind: 'O',
                old: (0, 0),
                new: (0, 0),
            });
            let t = if old { &mut r.old } else { &mut r.new };
            t.0 += v.1;
            t.1 += v.2;
        }
    }
    let mut files: Vec<Row> = files.into_values().filter(|r| r.old != r.new).collect();
    files.sort_by(|x, y| y.delta().abs().cmp(&x.delta().abs()).then(x.name.cmp(&y.name)));
    Diff {
        syms,
        files,
        old: (a.rom, a.ram),
        new: (b.rom, b.ram),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lld_map() {
        let m = "     VMA      LMA     Size Align Out     In      Symbol\n       0        0        0     1 _estack = 0\n 8000000  8000000       50     4 .isr_vector\n 8000000  8000000       50     4         ../startup.o:(.isr_vector)\n 8000000  8000000       50     1                 g_pfnVectors\n 8000060  8000060       32     4 .text\n 8000060  8000060       32     4         main2.o:(.text.parse)\n 8000061  8000061       32     1                 parse\n20000000  8000092        8     4 .data\n20000000  8000092        8     4         main2.o:(.data.cfg)\n20000008 20000008      100     4 .bss\n20000008 20000008      100     4         main2.o:(.bss.rxbuf)\n       0        0      123     1 .debug_info\n       0        0      123     1         main2.o:(.debug_info)\n";
        let s = from_lld(m).unwrap();
        assert_eq!(s.rom, 0x50 + 0x32 + 8);
        assert_eq!(s.ram, 8 + 0x100);
        assert_eq!(s.syms[&("parse".to_string(), "main2.o".to_string())], ('F', 0x32, 0));
        assert_eq!(s.syms[&("rxbuf".to_string(), "main2.o".to_string())], ('V', 0, 0x100));
        assert_eq!(s.syms[&("cfg".to_string(), "main2.o".to_string())], ('V', 8, 8));
        assert_eq!(s.syms[&("g_pfnVectors".to_string(), "startup.o".to_string())].1, 0x50);
    }

    #[test]
    fn armlink_map() {
        let m = "==============================================================================\n\nImage Symbol Table\n\n    Local Symbols\n\n    Symbol Name                              Value     Ov Type        Size  Object(Section)\n\n    i.main                                   0x08000188   Section        0  main.o(i.main)\n    s_buf                                    0x20000010   Data          64  main.o(.bss)\n\n    Global Symbols\n\n    main                                     0x08000189   Thumb Code    36  main.o(i.main)\n    SystemCoreClock                          0x20000000   Data           4  system_stm32f10x.o(.data)\n    crc_table                                0x08000400   Data         256  crc.o(.constdata)\n\n    Total RO  Size (Code + RO Data)                 2000 (   1.95kB)\n    Total RW  Size (RW Data + ZI Data)              1100 (   1.07kB)\n    Total ROM Size (Code + RO Data + RW Data)       2004 (   1.96kB)\n";
        let s = from_armlink(m).unwrap();
        assert_eq!((s.rom, s.ram), (2004, 1100));
        assert_eq!(s.syms.len(), 4);
        assert_eq!(s.syms[&("main".to_string(), "main.o".to_string())], ('F', 36, 0));
        assert_eq!(s.syms[&("s_buf".to_string(), "main.o".to_string())], ('V', 0, 64));
        assert_eq!(s.syms[&("SystemCoreClock".to_string(), "system_stm32f10x.o".to_string())], ('V', 4, 4));
        assert_eq!(s.syms[&("crc_table".to_string(), "crc.o".to_string())], ('V', 256, 0));
    }

    #[test]
    fn map_diff() {
        let head = "Memory Configuration\n\nName             Origin             Length             Attributes\nFLASH            0x08000000         0x00010000         xr\nRAM              0x20000000         0x00005000         xrw\n*default*        0x00000000         0xffffffff\n\nLinker script and memory map\n\n";
        let a = format!("{head}.text           0x08000000      0x120\n .text.main     0x08000000       0x20 build/main.o\n .text.uart_init\n                0x08000020      0x100 build/uart.o\n\n.bss            0x20000000       0x40\n COMMON         0x20000000       0x40 build/uart.o\n");
        let b = format!("{head}.text           0x08000000      0x160\n .text.main     0x08000000       0x20 build/main.o\n .text.uart_init\n                0x08000020      0x140 build/uart.o\n\n.bss            0x20000000       0x80\n COMMON         0x20000000       0x80 build/uart.o\n");
        let sa = from_map(&parse::parse_map(&a).unwrap());
        let sb = from_map(&parse::parse_map(&b).unwrap());
        let d = diff(&sa, &sb);
        assert_eq!(d.old.0, 0x120);
        assert_eq!(d.new.0, 0x160);
        assert_eq!(d.syms.len(), 2);
        let u = d.syms.iter().find(|r| r.name == "uart_init").unwrap();
        assert_eq!(u.delta(), 0x40);
        assert_eq!(d.files.len(), 1);
        assert_eq!(d.files[0].delta(), 0x80);
    }
}

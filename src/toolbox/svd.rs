//! CMSIS-SVD 寄存器描述：外设 → 寄存器 → 位域，带地址、复位值和说明。
//! 支持 derivedFrom、dim 数组、cluster（展开成 “CLUSTER_REG” 这样的名字）。

use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Enum {
    pub value: u64,
    pub name: String,
    pub desc: String,
}

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub name: String,
    pub lsb: u32,
    pub width: u32,
    pub desc: String,
    pub enums: Vec<Enum>,
}

impl Field {
    pub fn mask(&self) -> u64 {
        if self.width >= 64 {
            u64::MAX
        } else {
            ((1u64 << self.width) - 1) << self.lsb
        }
    }
    pub fn get(&self, v: u64) -> u64 {
        (v & self.mask()) >> self.lsb
    }
    pub fn set(&self, v: u64, x: u64) -> u64 {
        (v & !self.mask()) | ((x << self.lsb) & self.mask())
    }
    pub fn enum_name(&self, x: u64) -> Option<&Enum> {
        self.enums.iter().find(|e| e.value == x)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Reg {
    pub name: String,
    pub offset: u64,
    pub size: u32,
    pub reset: u64,
    pub access: String,
    pub desc: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Default)]
pub struct Periph {
    pub name: String,
    pub base: u64,
    pub group: String,
    pub desc: String,
    pub regs: Vec<Reg>,
    pub derived: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub name: String,
    pub cpu: String,
    pub periphs: Vec<Periph>,
    /// IRQ 号 → (名字, 说明)
    pub irqs: HashMap<u32, (String, String)>,
}

pub fn num(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = s.strip_prefix('#').or_else(|| s.strip_prefix("0b")) {
        // SVD 的 #1x0 这种带 x 的不在乎位，按 0 处理
        u64::from_str_radix(&b.replace(['x', 'X'], "0"), 2).ok()
    } else {
        s.parse().ok()
    }
}

/// 说明文字里常有换行和多个空格，收成一行
fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn child<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(tag))
}

fn text(n: Node, tag: &str) -> Option<String> {
    child(n, tag).and_then(|c| c.text()).map(|s| s.trim().to_string())
}

fn number(n: Node, tag: &str) -> Option<u64> {
    text(n, tag).and_then(|s| num(&s))
}

/// 继承下来的默认值
#[derive(Clone, Copy)]
struct Props<'a> {
    size: u32,
    reset: u64,
    access: &'a str,
}

fn props<'a>(n: Node<'a, '_>, p: Props<'a>) -> Props<'a> {
    Props {
        size: number(n, "size").map(|v| v as u32).unwrap_or(p.size),
        reset: number(n, "resetValue").unwrap_or(p.reset),
        access: child(n, "access").and_then(|c| c.text()).map(str::trim).unwrap_or(p.access),
    }
}

/// dim 展开：返回 (名字, 偏移增量) 列表
fn dims(n: Node, name: &str) -> Vec<(String, u64)> {
    let Some(dim) = number(n, "dim") else {
        return vec![(name.to_string(), 0)];
    };
    let inc = number(n, "dimIncrement").unwrap_or(0);
    let idx: Vec<String> = match text(n, "dimIndex") {
        Some(s) if s.contains('-') && !s.contains(',') => {
            let (a, b) = s.split_once('-').unwrap();
            match (a.trim().parse::<u64>(), b.trim().parse::<u64>()) {
                (Ok(a), Ok(b)) => (a..=b).map(|i| i.to_string()).collect(),
                _ => {
                    // A-D 这种字母范围
                    let (a, b) = (a.trim().bytes().next().unwrap_or(b'A'), b.trim().bytes().next().unwrap_or(b'A'));
                    (a..=b).map(|c| (c as char).to_string()).collect()
                }
            }
        }
        Some(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        None => (0..dim).map(|i| i.to_string()).collect(),
    };
    idx.iter()
        .take(dim.min(1024) as usize)
        .enumerate()
        .map(|(i, x)| {
            let nm = if name.contains("[%s]") { name.replace("[%s]", x) } else { name.replace("%s", x) };
            (nm, i as u64 * inc)
        })
        .collect()
}

fn parse_fields(n: Node) -> Vec<Field> {
    let Some(fs) = child(n, "fields") else { return vec![] };
    let mut out = vec![];
    for f in fs.children().filter(|c| c.has_tag_name("field")) {
        let name = text(f, "name").unwrap_or_default();
        let (lsb, width) = if let (Some(o), Some(w)) = (number(f, "bitOffset"), number(f, "bitWidth")) {
            (o, w)
        } else if let (Some(l), Some(m)) = (number(f, "lsb"), number(f, "msb")) {
            (l, m.saturating_sub(l) + 1)
        } else if let Some(r) = text(f, "bitRange") {
            let r = r.trim_matches(['[', ']']);
            let (m, l) = r.split_once(':').unwrap_or((r, r));
            let (m, l) = (m.trim().parse().unwrap_or(0u64), l.trim().parse().unwrap_or(0u64));
            (l, m.saturating_sub(l) + 1)
        } else {
            continue;
        };
        let mut enums = vec![];
        for ev in f.children().filter(|c| c.has_tag_name("enumeratedValues")) {
            for e in ev.children().filter(|c| c.has_tag_name("enumeratedValue")) {
                if let Some(v) = number(e, "value") {
                    enums.push(Enum {
                        value: v,
                        name: text(e, "name").unwrap_or_default(),
                        desc: clean(&text(e, "description").unwrap_or_default()),
                    });
                }
            }
        }
        for (nm, k) in dims(f, &name) {
            out.push(Field {
                name: nm,
                lsb: (lsb + k) as u32,
                width: width as u32,
                desc: clean(&text(f, "description").unwrap_or_default()),
                enums: enums.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.lsb.cmp(&a.lsb));
    out
}

/// registers 或 cluster 下面的寄存器，cluster 展开成前缀
fn parse_regs(n: Node, base: u64, prefix: &str, p: Props, out: &mut Vec<Reg>) {
    for c in n.children() {
        if c.has_tag_name("register") {
            let pp = props(c, p);
            let name = text(c, "name").unwrap_or_default();
            let off = number(c, "addressOffset").unwrap_or(0);
            let fields = parse_fields(c);
            let desc = clean(&text(c, "description").unwrap_or_default());
            for (nm, k) in dims(c, &name) {
                out.push(Reg {
                    name: format!("{prefix}{nm}"),
                    offset: base + off + k,
                    size: pp.size,
                    reset: pp.reset,
                    access: pp.access.to_string(),
                    desc: desc.clone(),
                    fields: fields.clone(),
                });
            }
        } else if c.has_tag_name("cluster") {
            let pp = props(c, p);
            let name = text(c, "name").unwrap_or_default();
            let off = number(c, "addressOffset").unwrap_or(0);
            for (nm, k) in dims(c, &name) {
                parse_regs(c, base + off + k, &format!("{prefix}{nm}_"), pp, out);
            }
        }
    }
}

pub fn parse(xml: &str) -> Result<Device, String> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let dev = doc.root_element();
    if !dev.has_tag_name("device") {
        return Err("not an SVD file (<device>)".into());
    }
    let p0 = props(
        dev,
        Props {
            size: 32,
            reset: 0,
            access: "read-write",
        },
    );
    let mut d = Device {
        name: text(dev, "name").unwrap_or_default(),
        cpu: child(dev, "cpu").and_then(|c| text(c, "name")).unwrap_or_default(),
        ..Default::default()
    };
    let Some(ps) = child(dev, "peripherals") else {
        return Ok(d);
    };
    for p in ps.children().filter(|c| c.has_tag_name("peripheral")) {
        let pp = props(p, p0);
        let mut regs = vec![];
        if let Some(rs) = child(p, "registers") {
            parse_regs(rs, 0, "", pp, &mut regs);
        }
        regs.sort_by_key(|r| r.offset);
        for i in p.children().filter(|c| c.has_tag_name("interrupt")) {
            if let (Some(n), Some(v)) = (text(i, "name"), number(i, "value")) {
                d.irqs.entry(v as u32).or_insert((n, clean(&text(i, "description").unwrap_or_default())));
            }
        }
        let name = text(p, "name").unwrap_or_default();
        let base = number(p, "baseAddress").unwrap_or(0);
        let group = text(p, "groupName").unwrap_or_default();
        let desc = clean(&text(p, "description").unwrap_or_default());
        let derived = p.attribute("derivedFrom").map(str::to_string);
        for (nm, k) in dims(p, &name) {
            d.periphs.push(Periph {
                name: nm,
                base: base + k,
                group: group.clone(),
                desc: desc.clone(),
                regs: regs.clone(),
                derived: derived.clone(),
            });
        }
    }
    // derivedFrom：没有自己寄存器的从基础外设复制
    let snapshot: HashMap<String, (Vec<Reg>, String, String)> = d.periphs.iter().map(|p| (p.name.clone(), (p.regs.clone(), p.group.clone(), p.desc.clone()))).collect();
    for p in &mut d.periphs {
        if let Some((regs, group, desc)) = p.derived.as_ref().and_then(|b| snapshot.get(b)) {
            if p.regs.is_empty() {
                p.regs = regs.clone();
            }
            if p.group.is_empty() {
                p.group = group.clone();
            }
            if p.desc.is_empty() {
                p.desc = desc.clone();
            }
        }
    }
    d.periphs.sort_by_key(|p| p.base);
    Ok(d)
}

impl Device {
    pub fn find(&self, periph: &str, reg: &str) -> Option<(&Periph, &Reg)> {
        let p = self.periphs.iter().find(|p| p.name.eq_ignore_ascii_case(periph))?;
        // CMSIS 头文件里数组寄存器写成 BSRR 或者 AFR[0]
        let reg = reg.replace(['[', ']'], "");
        let r = p.regs.iter().find(|r| r.name.eq_ignore_ascii_case(&reg) || r.name.replace(['[', ']'], "").eq_ignore_ascii_case(&reg))?;
        Some((p, r))
    }

    /// 地址落在哪个寄存器上
    pub fn at(&self, addr: u64) -> Option<(&Periph, &Reg)> {
        self.periphs.iter().find_map(|p| p.regs.iter().find(|r| p.base + r.offset == addr).map(|r| (p, r)))
    }
}

/// 位域解码成一行行：(位, 名字, 值, 枚举名)
#[cfg(test)]
pub fn decode(r: &Reg, v: u64) -> Vec<(String, String, u64, String)> {
    r.fields
        .iter()
        .map(|f| {
            let bits = if f.width == 1 { f.lsb.to_string() } else { format!("{}:{}", f.lsb + f.width - 1, f.lsb) };
            let x = f.get(v);
            (bits, f.name.clone(), x, f.enum_name(x).map(|e| e.name.clone()).unwrap_or_default())
        })
        .collect()
}

impl Device {
    /// 外设名或分组名（USART → USART1、USART2…）对上的第一个外设
    fn periph_like(&self, name: &str) -> Option<&Periph> {
        self.periphs
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .or_else(|| self.periphs.iter().find(|p| p.group.eq_ignore_ascii_case(name)))
    }

    /// CMSIS 位宏：USART_CR1_UE、USART_CR1_UE_Pos、GPIO_ODR_ODR5 → (外设分组, 寄存器, 位域)
    pub fn bit_macro(&self, name: &str) -> Option<(&Periph, &Reg, &Field)> {
        let base = name.strip_suffix("_Pos").or_else(|| name.strip_suffix("_Msk")).unwrap_or(name);
        let parts: Vec<&str> = base.split('_').collect();
        for i in 1..parts.len() {
            let Some(p) = self.periph_like(&parts[..i].join("_")) else { continue };
            for j in i + 1..parts.len() {
                let reg = parts[i..j].join("_");
                let Some(r) = p.regs.iter().find(|r| r.name.replace(['[', ']'], "").eq_ignore_ascii_case(&reg)) else {
                    continue;
                };
                let field = parts[j..].join("_");
                // 多位位域里的单独一位：USART_CR1_M_0
                let f = r.fields.iter().find(|f| f.name.eq_ignore_ascii_case(&field)).or_else(|| {
                    let (head, tail) = field.rsplit_once('_')?;
                    tail.parse::<u32>().ok()?;
                    r.fields.iter().find(|f| f.name.eq_ignore_ascii_case(head))
                });
                if let Some(f) = f {
                    return Some((p, r, f));
                }
            }
        }
        None
    }

    /// 从一段文字（日志、粘贴的转储）里认出寄存器值：(外设, 寄存器, 值)
    /// 支持 USART1->CR1 = 0x200C、0x4001380C: 0x200C、USART1 CR1=0x200C、CR1=0x200C（寄存器名唯一时）
    pub fn parse_dump(&self, text: &str) -> Vec<(String, String, u64)> {
        let mut out: Vec<(String, String, u64)> = vec![];
        let mut ctx: Option<String> = None;
        for line in text.lines() {
            let toks: Vec<&str> = line
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '[' || c == ']'))
                .filter(|s| !s.is_empty())
                .collect();
            // 行里出现的外设名作为后面寄存器的上下文
            if let Some(p) = toks.iter().find(|t| self.periphs.iter().any(|p| p.name.eq_ignore_ascii_case(t))) {
                ctx = Some(p.to_string());
            }
            let mut i = 0;
            while i + 1 < toks.len() {
                let (a, b) = (toks[i], toks[i + 1]);
                let Some(v) = num(b).filter(|_| b.starts_with(|c: char| c.is_ascii_digit())) else {
                    i += 1;
                    continue;
                };
                // 地址: 值
                if let Some(addr) = num(a).filter(|_| a.starts_with("0x") || a.starts_with("0X")) {
                    if let Some((p, r)) = self.at(addr) {
                        out.push((p.name.clone(), r.name.clone(), v));
                        i += 2;
                        continue;
                    }
                }
                if a.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                    let hit = ctx.as_deref().and_then(|c| self.find(c, a)).map(|(p, r)| (p.name.clone(), r.name.clone())).or_else(|| {
                        let all: Vec<(&Periph, &Reg)> = self.periphs.iter().filter_map(|p| p.regs.iter().find(|r| r.name.eq_ignore_ascii_case(a)).map(|r| (p, r))).collect();
                        (all.len() == 1).then(|| (all[0].0.name.clone(), all[0].1.name.clone()))
                    });
                    if let Some((p, r)) = hit {
                        out.push((p, r, v));
                        i += 2;
                        continue;
                    }
                }
                i += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<device schemaVersion="1.1">
  <name>STM32F103</name>
  <cpu><name>CM3</name></cpu>
  <size>32</size><resetValue>0x0</resetValue>
  <peripherals>
    <peripheral>
      <name>USART1</name><groupName>USART</groupName><baseAddress>0x40013800</baseAddress>
      <description>Universal synchronous asynchronous
        receiver transmitter</description>
      <interrupt><name>USART1</name><description>USART1 global interrupt</description><value>37</value></interrupt>
      <registers>
        <register>
          <name>SR</name><addressOffset>0x0</addressOffset><resetValue>0x00C0</resetValue>
          <fields>
            <field><name>TXE</name><bitOffset>7</bitOffset><bitWidth>1</bitWidth></field>
            <field><name>RXNE</name><lsb>5</lsb><msb>5</msb></field>
          </fields>
        </register>
        <register>
          <name>CR1</name><addressOffset>0xC</addressOffset>
          <fields>
            <field><name>M</name><bitRange>[12:12]</bitRange>
              <enumeratedValues><enumeratedValue><name>Bits8</name><value>0</value></enumeratedValue><enumeratedValue><name>Bits9</name><value>1</value></enumeratedValue></enumeratedValues>
            </field>
            <field><name>UE</name><bitOffset>13</bitOffset><bitWidth>1</bitWidth></field>
          </fields>
        </register>
        <register>
          <dim>2</dim><dimIncrement>4</dimIncrement>
          <name>AFR[%s]</name><addressOffset>0x20</addressOffset>
        </register>
      </registers>
    </peripheral>
    <peripheral derivedFrom="USART1">
      <name>USART2</name><baseAddress>0x40004400</baseAddress>
      <interrupt><name>USART2</name><value>38</value></interrupt>
    </peripheral>
  </peripherals>
</device>"#;

    #[test]
    fn parse_svd() {
        let d = parse(SVD).unwrap();
        assert_eq!(d.name, "STM32F103");
        assert_eq!(d.periphs.len(), 2);
        let (p, r) = d.find("USART2", "CR1").unwrap();
        assert_eq!(p.base + r.offset, 0x4000_440C);
        assert_eq!(p.group, "USART");
        let (_, sr) = d.find("usart1", "sr").unwrap();
        assert_eq!(sr.reset, 0xC0);
        assert_eq!(sr.fields[0].name, "TXE");
        let dec = decode(sr, 0x80);
        assert_eq!(dec[0], ("7".into(), "TXE".into(), 1, String::new()));
        let (_, cr1) = d.find("USART1", "CR1").unwrap();
        assert_eq!(decode(cr1, 0x1000)[1].3, "Bits9");
        assert!(d.find("USART1", "AFR[1]").is_some());
        assert_eq!(d.find("USART1", "AFR1").unwrap().1.offset, 0x24);
        assert_eq!(d.irqs[&37].0, "USART1");
        assert_eq!(d.at(0x4001_3800).unwrap().1.name, "SR");
        assert!(p.desc.contains("asynchronous receiver"));
        let f = &cr1.fields[0];
        assert_eq!(f.set(0, 1), 1 << 13);
        let (bp, br, bf) = d.bit_macro("USART_CR1_UE_Pos").unwrap();
        assert_eq!((bp.group.as_str(), br.name.as_str(), bf.name.as_str()), ("USART", "CR1", "UE"));
        assert!(d.bit_macro("USART_CR1_M_0").is_some());
        assert!(d.bit_macro("GPIO_CR1_UE").is_none());
        let v = d.parse_dump("USART1->CR1 = 0x200C\n0x40013800: 0x80\nUSART2 SR=0xC0 CR1=0x2000\n");
        assert_eq!(v.len(), 4);
        assert_eq!(v[0], ("USART1".into(), "CR1".into(), 0x200C));
        assert_eq!(v[1], ("USART1".into(), "SR".into(), 0x80));
        assert_eq!(v[3], ("USART2".into(), "CR1".into(), 0x2000));
    }
}

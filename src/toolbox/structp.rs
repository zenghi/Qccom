//! C 结构体解析：把结构体定义和一段 HEX 数据对起来，拆成每个字段的值。
//! 按 GCC / ARM 的规则排布：自然对齐、#pragma pack、__attribute__((packed))、位域、联合体、嵌套结构体和数组。

use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    pub little: bool,
    /// 指针、long、size_t 的字节数（4 或 8）
    pub ptr: u32,
    /// 全部按 packed 排（不管定义里写没写）
    pub force_packed: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            little: true,
            ptr: 4,
            force_packed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Int {
        size: u32,
        signed: bool,
    },
    Float,
    Double,
    Bool,
    Char {
        signed: bool,
    },
    Ptr,
    /// 结构体 / 联合体（Defs 里的序号）
    Agg(usize),
    /// 枚举（Enums 里的序号）
    Enum(usize, u32),
    Array(Box<Ty>, u64),
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub ty: Ty,
    pub bits: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Def {
    pub name: String,
    pub union: bool,
    pub packed: bool,
    /// #pragma pack(n)，0 = 不限
    pub pack: u32,
    pub members: Vec<Member>,
}

#[derive(Default, Debug)]
pub struct Defs {
    pub defs: Vec<Def>,
    /// typedef 名字 / struct 标签 → 类型
    pub names: HashMap<String, Ty>,
    pub enums: Vec<Vec<(i64, String)>>,
    /// 定义的顺序里最后一个（默认解析它）
    pub order: Vec<String>,
}

// ------------------------------------------------------------------ 词法

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Id(String),
    Num(i64),
    P(char),
    /// #pragma pack 的参数：Some(n) 设置，None 恢复
    Pack(Option<u32>),
}

fn lex(src: &str, defines: &mut HashMap<String, i64>) -> Vec<Tok> {
    let mut out = vec![];
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut pack_stack: Vec<Option<u32>> = vec![];
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '#' && line_start {
            let mut j = i;
            let mut line = String::new();
            while j < b.len() && b[j] != '\n' {
                if b[j] == '\\' && b.get(j + 1) == Some(&'\n') {
                    j += 2;
                    continue;
                }
                line.push(b[j]);
                j += 1;
            }
            i = j;
            let l = line.trim_start_matches('#').trim();
            if let Some(rest) = l.strip_prefix("pragma").map(str::trim).and_then(|r| r.strip_prefix("pack")) {
                let args: String = rest.chars().filter(|c| !c.is_whitespace() && *c != '(' && *c != ')').collect();
                let n = args.split(',').filter_map(|a| a.parse::<u32>().ok()).next_back();
                if args.starts_with("pop") {
                    let prev = pack_stack.pop().flatten();
                    out.push(Tok::Pack(prev));
                } else if args.starts_with("push") {
                    pack_stack.push(out.iter().rev().find_map(|t| if let Tok::Pack(p) = t { Some(*p) } else { None }).flatten());
                    if n.is_some() {
                        out.push(Tok::Pack(n));
                    }
                } else {
                    out.push(Tok::Pack(n));
                }
            } else if let Some(rest) = l.strip_prefix("define") {
                let mut it = rest.split_whitespace();
                if let (Some(k), Some(v)) = (it.next(), it.next()) {
                    if let Some(n) = parse_int(v.trim_matches(['(', ')'])) {
                        defines.insert(k.to_string(), n);
                    }
                }
            }
            continue;
        }
        line_start = false;
        if c.is_alphabetic() || c == '_' {
            let mut s = String::new();
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                s.push(b[i]);
                i += 1;
            }
            out.push(Tok::Id(s));
            continue;
        }
        if c.is_ascii_digit() {
            let mut s = String::new();
            while i < b.len() && (b[i].is_ascii_alphanumeric()) {
                s.push(b[i]);
                i += 1;
            }
            out.push(Tok::Num(parse_int(&s).unwrap_or(0)));
            continue;
        }
        out.push(Tok::P(c));
        i += 1;
    }
    out
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(b) = s.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()
    } else {
        s.parse().ok()
    }
}

// ------------------------------------------------------------------ 语法

struct P<'a> {
    t: &'a [Tok],
    i: usize,
    pack: u32,
    defs: Defs,
    defines: HashMap<String, i64>,
    err: Option<String>,
}

const QUALS: [&str; 8] = ["const", "volatile", "static", "extern", "register", "__IO", "__I", "__O"];

fn builtin(words: &[String], ptr: u32) -> Option<Ty> {
    let w: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    let one = |s: &str| -> Option<Ty> {
        Some(match s {
            "uint8_t" | "u8" | "uint8" | "UINT8" | "BYTE" | "byte" | "uchar" => Ty::Int { size: 1, signed: false },
            "int8_t" | "s8" | "i8" | "int8" | "INT8" => Ty::Int { size: 1, signed: true },
            "uint16_t" | "u16" | "uint16" | "UINT16" | "WORD" | "ushort" => Ty::Int { size: 2, signed: false },
            "int16_t" | "s16" | "i16" | "int16" | "INT16" => Ty::Int { size: 2, signed: true },
            "uint32_t" | "u32" | "uint32" | "UINT32" | "DWORD" | "uint" => Ty::Int { size: 4, signed: false },
            "int32_t" | "s32" | "i32" | "int32" | "INT32" => Ty::Int { size: 4, signed: true },
            "uint64_t" | "u64" | "uint64" | "UINT64" | "QWORD" => Ty::Int { size: 8, signed: false },
            "int64_t" | "s64" | "i64" | "int64" | "INT64" => Ty::Int { size: 8, signed: true },
            "size_t" | "uintptr_t" => Ty::Int { size: ptr, signed: false },
            "ssize_t" | "intptr_t" | "ptrdiff_t" => Ty::Int { size: ptr, signed: true },
            "float" | "float32_t" | "f32" => Ty::Float,
            "double" | "float64_t" | "f64" => Ty::Double,
            "bool" | "_Bool" | "boolean" => Ty::Bool,
            "char" | "char_t" => Ty::Char { signed: true },
            "wchar_t" => Ty::Int { size: 4, signed: false },
            // 只会以 void * 出现
            "void" => Ty::Int { size: 1, signed: false },
            _ => return None,
        })
    };
    if w.len() == 1 {
        if let Some(t) = one(w[0]) {
            return Some(t);
        }
    }
    let unsigned = w.contains(&"unsigned");
    let longs = w.iter().filter(|x| **x == "long").count();
    if w.contains(&"char") {
        return Some(if unsigned { Ty::Int { size: 1, signed: false } } else { Ty::Char { signed: true } });
    }
    if w.contains(&"double") {
        return Some(Ty::Double);
    }
    let size = if w.contains(&"short") {
        2
    } else if longs >= 2 {
        8
    } else if longs == 1 {
        ptr
    } else if w.iter().any(|x| matches!(*x, "int" | "signed" | "unsigned")) {
        4
    } else {
        return None;
    };
    Some(Ty::Int { size, signed: !unsigned })
}

fn is_packed_word(s: &str) -> bool {
    s.to_ascii_lowercase().contains("packed")
}

impl P<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::P(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn id(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Id(s)) => Some(s),
            _ => None,
        }
    }
    fn skip_pack(&mut self) {
        while let Some(Tok::Pack(p)) = self.peek() {
            self.pack = p.unwrap_or(0);
            self.i += 1;
        }
    }
    /// 跳过 __attribute__((...))、__packed 之类，返回其中是否有 packed
    fn attrs(&mut self) -> bool {
        let mut packed = false;
        loop {
            self.skip_pack();
            match self.id() {
                Some("__attribute__") | Some("__attribute") | Some("__declspec") | Some("alignas") | Some("_Alignas") => {
                    self.i += 1;
                    let start = self.i;
                    self.skip_parens();
                    packed |= self.t[start..self.i].iter().any(|t| matches!(t, Tok::Id(s) if is_packed_word(s)));
                }
                Some(s) if is_packed_word(s) => {
                    packed = true;
                    self.i += 1;
                }
                Some(s) if QUALS.contains(&s) => self.i += 1,
                _ => return packed,
            }
        }
    }
    fn skip_parens(&mut self) {
        if !self.eat('(') {
            return;
        }
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Some(Tok::P('(')) => depth += 1,
                Some(Tok::P(')')) => depth -= 1,
                None => return,
                _ => {}
            }
        }
    }
    fn skip_braces(&mut self) {
        let mut depth = 1;
        while depth > 0 {
            match self.next() {
                Some(Tok::P('{')) => depth += 1,
                Some(Tok::P('}')) => depth -= 1,
                None => return,
                _ => {}
            }
        }
    }
    fn expr(&mut self) -> Result<i64, String> {
        // 数组长度：数字、#define 的名字、简单的 + - * 运算
        let mut v = self.atom()?;
        loop {
            match self.peek() {
                Some(Tok::P('+')) => {
                    self.i += 1;
                    v += self.atom()?;
                }
                Some(Tok::P('-')) => {
                    self.i += 1;
                    v -= self.atom()?;
                }
                Some(Tok::P('*')) => {
                    self.i += 1;
                    v *= self.atom()?;
                }
                Some(Tok::P('/')) => {
                    self.i += 1;
                    let d = self.atom()?;
                    v = if d == 0 { 0 } else { v / d };
                }
                _ => return Ok(v),
            }
        }
    }
    fn atom(&mut self) -> Result<i64, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(n),
            Some(Tok::Id(s)) => self.defines.get(&s).copied().or_else(|| self.enum_const(&s)).ok_or(format!("? {s}")),
            Some(Tok::P('(')) => {
                let v = self.expr()?;
                self.eat(')');
                Ok(v)
            }
            Some(Tok::P('-')) => Ok(-self.atom()?),
            t => Err(format!("? {t:?}")),
        }
    }
    fn enum_const(&self, s: &str) -> Option<i64> {
        self.defs.enums.iter().flatten().find(|e| e.1 == s).map(|e| e.0)
    }

    /// 类型说明：返回类型；内联的 struct / union / enum 定义顺便登记
    fn spec(&mut self, ptr: u32) -> Result<Ty, String> {
        let mut packed = self.attrs();
        match self.id() {
            Some("struct") | Some("union") => {
                let union = self.id() == Some("union");
                self.i += 1;
                packed |= self.attrs();
                let tag = match self.peek() {
                    Some(Tok::Id(s)) => {
                        let s = s.clone();
                        self.i += 1;
                        Some(s)
                    }
                    _ => None,
                };
                packed |= self.attrs();
                if self.eat('{') {
                    let idx = self.body(tag.clone().unwrap_or_default(), union, packed, ptr)?;
                    if let Some(t) = tag {
                        self.defs.names.insert(format!("struct {t}"), Ty::Agg(idx));
                    }
                    Ok(Ty::Agg(idx))
                } else {
                    let t = tag.ok_or("struct without name")?;
                    self.defs.names.get(&format!("struct {t}")).cloned().ok_or(format!("struct {t} ?"))
                }
            }
            Some("enum") => {
                self.i += 1;
                packed |= self.attrs();
                let tag = match self.peek() {
                    Some(Tok::Id(s)) => {
                        let s = s.clone();
                        self.i += 1;
                        Some(s)
                    }
                    _ => None,
                };
                // C23 / C++ 的 enum : uint8_t
                let mut size = 4;
                if self.eat(':') {
                    let mut words = vec![];
                    while let Some(Tok::Id(s)) = self.peek() {
                        words.push(s.clone());
                        self.i += 1;
                    }
                    if let Some(Ty::Int { size: s, .. }) = builtin(&words, ptr) {
                        size = s;
                    }
                }
                packed |= self.attrs();
                if self.eat('{') {
                    let mut vals = vec![];
                    let mut next = 0i64;
                    loop {
                        match self.next() {
                            Some(Tok::P('}')) | None => break,
                            Some(Tok::Id(name)) => {
                                if self.eat('=') {
                                    next = self.expr().unwrap_or(next);
                                }
                                vals.push((next, name));
                                next += 1;
                                self.eat(',');
                            }
                            _ => {}
                        }
                    }
                    packed |= self.attrs();
                    if packed {
                        let max = vals.iter().map(|v| v.0).max().unwrap_or(0);
                        let min = vals.iter().map(|v| v.0).min().unwrap_or(0);
                        size = if min >= 0 && max <= 255 || min >= -128 && max <= 127 {
                            1
                        } else if min >= -32768 && max <= 65535 {
                            2
                        } else {
                            4
                        };
                    }
                    self.defs.enums.push(vals);
                    let ty = Ty::Enum(self.defs.enums.len() - 1, size);
                    if let Some(t) = tag {
                        self.defs.names.insert(format!("struct {t}"), ty.clone());
                    }
                    Ok(ty)
                } else {
                    let t = tag.ok_or("enum without name")?;
                    Ok(self.defs.names.get(&format!("struct {t}")).cloned().unwrap_or(Ty::Int { size, signed: true }))
                }
            }
            _ => {
                let mut words: Vec<String> = vec![];
                while let Some(Tok::Id(s)) = self.peek() {
                    let s = s.clone();
                    if QUALS.contains(&s.as_str()) || is_packed_word(&s) {
                        self.i += 1;
                        continue;
                    }
                    let kw = matches!(s.as_str(), "unsigned" | "signed" | "short" | "long" | "int" | "char" | "double");
                    if !words.is_empty() && !kw {
                        break;
                    }
                    if words.len() == 1 && !kw && !matches!(words[0].as_str(), "unsigned" | "signed" | "short" | "long") {
                        break;
                    }
                    words.push(s);
                    self.i += 1;
                    if !kw {
                        break;
                    }
                }
                if words.is_empty() {
                    return Err(format!("? {:?}", self.peek()));
                }
                if let Some(t) = builtin(&words, ptr) {
                    return Ok(t);
                }
                let w = words.join(" ");
                self.defs.names.get(&w).cloned().ok_or(format!("{} {w}", "unknown type"))
            }
        }
    }

    /// 声明符：* name [N]... : bits；函数指针 (*name)(...) 当指针
    fn declarator(&mut self, base: &Ty) -> Result<(String, Ty, Option<u32>), String> {
        let mut ty = base.clone();
        let mut ptr = false;
        loop {
            if self.eat('*') {
                ptr = true;
                self.attrs();
            } else if matches!(self.id(), Some(s) if QUALS.contains(&s)) {
                self.i += 1;
            } else {
                break;
            }
        }
        let name;
        if self.eat('(') {
            // 函数指针
            while self.eat('*') {}
            name = match self.next() {
                Some(Tok::Id(s)) => s,
                t => return Err(format!("? {t:?}")),
            };
            let mut dims = vec![];
            while self.eat('[') {
                dims.push(self.expr()?);
                self.eat(']');
            }
            self.eat(')');
            self.skip_parens();
            let mut t = Ty::Ptr;
            for d in dims.into_iter().rev() {
                t = Ty::Array(Box::new(t), d.max(0) as u64);
            }
            return Ok((name, t, None));
        }
        name = match self.peek() {
            Some(Tok::Id(s)) => {
                let s = s.clone();
                self.i += 1;
                s
            }
            _ => String::new(),
        };
        if ptr {
            ty = Ty::Ptr;
        }
        let mut dims = vec![];
        while self.eat('[') {
            dims.push(if self.eat(']') { 0 } else { self.expr()? });
            self.eat(']');
        }
        for d in dims.into_iter().rev() {
            ty = Ty::Array(Box::new(ty), d.max(0) as u64);
        }
        let bits = if self.eat(':') { Some(self.expr()?.max(0) as u32) } else { None };
        self.attrs();
        Ok((name, ty, bits))
    }

    fn body(&mut self, name: String, union: bool, packed: bool, ptr: u32) -> Result<usize, String> {
        let idx = self.defs.defs.len();
        self.defs.defs.push(Def {
            name,
            union,
            packed,
            pack: self.pack,
            members: vec![],
        });
        let mut members = vec![];
        loop {
            self.skip_pack();
            if self.eat('}') {
                break;
            }
            if self.peek().is_none() {
                return Err("missing }".into());
            }
            if self.eat(';') {
                continue;
            }
            let base = self.spec(ptr)?;
            // 匿名的 struct / union 成员：字段直接属于外层
            if self.eat(';') {
                if let Ty::Agg(i) = base {
                    members.push(Member {
                        name: String::new(),
                        ty: Ty::Agg(i),
                        bits: None,
                    });
                }
                continue;
            }
            loop {
                let (n, t, bits) = self.declarator(&base)?;
                members.push(Member { name: n, ty: t, bits });
                if self.eat(',') {
                    continue;
                }
                break;
            }
            if !self.eat(';') {
                return Err(format!("expected ; got {:?}", self.peek()));
            }
        }
        let packed = self.attrs() || packed;
        let d = &mut self.defs.defs[idx];
        d.members = members;
        d.packed = packed;
        Ok(idx)
    }

    /// 跳到这一句结束（顶层的 ;），中间的花括号整体跳过
    fn recover(&mut self) {
        let mut depth = 0;
        while let Some(t) = self.next() {
            match t {
                Tok::P('{') => depth += 1,
                Tok::P('}') => depth -= 1,
                Tok::P(';') if depth <= 0 => return,
                _ => {}
            }
        }
    }

    fn top(&mut self, ptr: u32) -> Result<(), String> {
        while self.peek().is_some() {
            self.skip_pack();
            match self.id() {
                Some("typedef") => {
                    self.i += 1;
                    // 认不出的类型（别的头文件里定义的）跳过这一句，不影响后面的
                    let base = match self.spec(ptr) {
                        Ok(b) => b,
                        Err(e) => {
                            self.err = Some(e);
                            self.recover();
                            continue;
                        }
                    };
                    loop {
                        let Ok((n, t, _)) = self.declarator(&base) else {
                            self.recover();
                            break;
                        };
                        if let Ty::Agg(i) = &t {
                            if self.defs.defs[*i].name.is_empty() {
                                self.defs.defs[*i].name = n.clone();
                            }
                        }
                        if matches!(t, Ty::Agg(_)) {
                            self.defs.order.push(n.clone());
                        }
                        self.defs.names.insert(n, t);
                        if !self.eat(',') {
                            break;
                        }
                    }
                    self.eat(';');
                }
                Some("struct") | Some("union") | Some("enum") => {
                    let start = self.i;
                    let t = self.spec(ptr);
                    match t {
                        Ok(Ty::Agg(i)) => {
                            let n = self.defs.defs[i].name.clone();
                            if !n.is_empty() {
                                self.defs.order.push(format!("struct {n}"));
                            }
                            // struct x {...} var; 的变量名忽略
                            while !self.eat(';') && self.peek().is_some() {
                                self.i += 1;
                            }
                        }
                        Ok(_) => {
                            while !self.eat(';') && self.peek().is_some() {
                                self.i += 1;
                            }
                        }
                        Err(e) => {
                            self.err = Some(e);
                            self.i = start + 1;
                        }
                    }
                }
                _ => {
                    // 函数、变量等其他内容：跳到 ; 或者跳过 {...}
                    match self.next() {
                        Some(Tok::P('{')) => self.skip_braces(),
                        None => break,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn parse(src: &str, ptr: u32) -> Result<Defs, String> {
    let mut defines = HashMap::new();
    let toks = lex(src, &mut defines);
    let mut p = P {
        t: &toks,
        i: 0,
        pack: 0,
        defs: Defs::default(),
        defines,
        err: None,
    };
    p.top(ptr)?;
    if p.defs.defs.is_empty() || p.defs.defs[p.defs.main()].members.is_empty() {
        return Err(p.err.unwrap_or_else(|| "no struct".into()));
    }
    Ok(p.defs)
}

// ------------------------------------------------------------------ 排布和解码

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub ty: String,
    /// 位偏移（从结构体开头算）
    pub bit_off: u64,
    pub bits: u64,
    pub is_bitfield: bool,
    pub kind: Leaf,
    pub kids: Vec<Field>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Leaf {
    Int(bool),
    Float,
    Double,
    Bool,
    Char,
    Ptr,
    Enum(usize),
    Group,
    CharArray,
}

impl Defs {
    pub fn find(&self, name: &str) -> Option<usize> {
        match self.names.get(name).or_else(|| self.names.get(&format!("struct {name}"))) {
            Some(Ty::Agg(i)) => Some(*i),
            _ => self.defs.iter().position(|d| d.name == name),
        }
    }

    /// 默认解析最后定义的那个结构体（一般是最外层）
    pub fn main(&self) -> usize {
        self.order.last().and_then(|n| self.find(n)).unwrap_or(self.defs.len() - 1)
    }

    fn size_align(&self, ty: &Ty, o: &Opts, depth: u32) -> (u64, u64) {
        if depth > 32 {
            return (0, 1);
        }
        match ty {
            Ty::Int { size, .. } => (*size as u64, *size as u64),
            Ty::Enum(_, s) => (*s as u64, *s as u64),
            Ty::Float => (4, 4),
            Ty::Double => (8, 8),
            Ty::Bool | Ty::Char { .. } => (1, 1),
            Ty::Ptr => (o.ptr as u64, o.ptr as u64),
            Ty::Array(t, n) => {
                let (s, a) = self.size_align(t, o, depth + 1);
                (s * n, a)
            }
            Ty::Agg(i) => {
                let f = self.layout_agg(*i, o, depth + 1);
                (f.0.bits / 8, f.1)
            }
        }
    }

    fn type_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int { size, signed } => format!("{}int{}", if *signed { "" } else { "u" }, size * 8),
            Ty::Enum(_, s) => format!("enum({s})"),
            Ty::Float => "float".into(),
            Ty::Double => "double".into(),
            Ty::Bool => "bool".into(),
            Ty::Char { .. } => "char".into(),
            Ty::Ptr => "pointer".into(),
            Ty::Array(t, n) => format!("{}[{n}]", self.type_name(t)),
            Ty::Agg(i) => {
                let d = &self.defs[*i];
                let k = if d.union { "union" } else { "struct" };
                if d.name.is_empty() {
                    k.to_string()
                } else {
                    format!("{k} {}", d.name)
                }
            }
        }
    }

    fn field(&self, name: String, ty: &Ty, bit_off: u64, o: &Opts, depth: u32) -> Field {
        let (size, _) = self.size_align(ty, o, depth);
        let mut f = Field {
            name,
            ty: self.type_name(ty),
            bit_off,
            bits: size * 8,
            is_bitfield: false,
            kind: Leaf::Group,
            kids: vec![],
        };
        match ty {
            Ty::Int { signed, .. } => f.kind = Leaf::Int(*signed),
            Ty::Enum(e, _) => f.kind = Leaf::Enum(*e),
            Ty::Float => f.kind = Leaf::Float,
            Ty::Double => f.kind = Leaf::Double,
            Ty::Bool => f.kind = Leaf::Bool,
            Ty::Char { .. } => f.kind = Leaf::Char,
            Ty::Ptr => f.kind = Leaf::Ptr,
            Ty::Array(t, n) => {
                if matches!(**t, Ty::Char { .. }) {
                    f.kind = Leaf::CharArray;
                } else {
                    let (es, _) = self.size_align(t, o, depth);
                    for k in 0..(*n).min(4096) {
                        f.kids.push(self.field(format!("[{k}]"), t, bit_off + k * es * 8, o, depth + 1));
                    }
                }
            }
            Ty::Agg(i) => {
                let (sub, _) = self.layout_agg(*i, o, depth + 1);
                f.kids = sub
                    .kids
                    .into_iter()
                    .map(|mut k| {
                        shift(&mut k, bit_off);
                        k
                    })
                    .collect();
            }
        }
        f
    }

    /// 返回 (布局, 对齐)
    pub fn layout_agg(&self, idx: usize, o: &Opts, depth: u32) -> (Field, u64) {
        let d = &self.defs[idx];
        let packed = d.packed || o.force_packed;
        let cap = if packed {
            1
        } else if d.pack > 0 {
            d.pack as u64
        } else {
            u64::MAX
        };
        let mut kids = vec![];
        let mut bitpos: u64 = 0;
        let mut max_align = 1u64;
        let mut size_bits = 0u64;
        for m in &d.members {
            let (sz, al) = self.size_align(&m.ty, o, depth);
            let al = al.min(cap).max(1);
            if let Some(w) = m.bits {
                let w = w as u64;
                let unit = sz.max(1) * 8;
                if d.union {
                    bitpos = 0;
                }
                if w == 0 {
                    // :0 强制换到下一个存储单元
                    bitpos = bitpos.div_ceil(unit) * unit;
                    continue;
                }
                if !packed && bitpos % unit + w > unit {
                    bitpos = bitpos.div_ceil(unit) * unit;
                }
                // 位域本身不提高结构体对齐（ARM AAPCS 会，但 packed 时不会），这里取类型对齐
                max_align = max_align.max(al);
                let mut f = self.field(m.name.clone(), &m.ty, bitpos, o, depth);
                f.bits = w;
                f.is_bitfield = true;
                f.ty = format!("{}:{w}", f.ty);
                kids.push(f);
                bitpos += w;
                size_bits = size_bits.max(bitpos);
                if d.union {
                    bitpos = 0;
                }
                continue;
            }
            let byte = if d.union { 0 } else { bitpos.div_ceil(8).div_ceil(al) * al };
            max_align = max_align.max(al);
            let f = self.field(m.name.clone(), &m.ty, byte * 8, o, depth);
            if m.name.is_empty() {
                // 匿名成员：字段并到外层
                kids.extend(f.kids);
            } else {
                kids.push(f);
            }
            let end = (byte + sz) * 8;
            size_bits = size_bits.max(end);
            if !d.union {
                bitpos = end;
            }
        }
        let size = size_bits.div_ceil(8).div_ceil(max_align) * max_align;
        (
            Field {
                name: d.name.clone(),
                ty: self.type_name(&Ty::Agg(idx)),
                bit_off: 0,
                bits: size * 8,
                is_bitfield: false,
                kind: Leaf::Group,
                kids,
            },
            max_align,
        )
    }

    pub fn layout(&self, idx: usize, o: &Opts) -> Field {
        self.layout_agg(idx, o, 0).0
    }
}

fn shift(f: &mut Field, by: u64) {
    f.bit_off += by;
    for k in &mut f.kids {
        shift(k, by);
    }
}

/// 读 bit_off 开始的 bits 位。小端：字节流按低位在前编号；大端：高位在前
pub fn read_bits(data: &[u8], bit_off: u64, bits: u64, little: bool, bitfield: bool) -> Option<u64> {
    if bits == 0 || bits > 64 || (bit_off + bits).div_ceil(8) > data.len() as u64 {
        return None;
    }
    if !bitfield {
        let s = &data[(bit_off / 8) as usize..((bit_off + bits) / 8) as usize];
        return Some(if little {
            s.iter().rev().fold(0, |a, b| (a << 8) | *b as u64)
        } else {
            s.iter().fold(0, |a, b| (a << 8) | *b as u64)
        });
    }
    let mut v = 0u64;
    for k in 0..bits {
        let p = bit_off + k;
        let byte = data[(p / 8) as usize];
        if little {
            v |= (((byte >> (p % 8)) & 1) as u64) << k;
        } else {
            v = (v << 1) | ((byte >> (7 - p % 8)) & 1) as u64;
        }
    }
    Some(v)
}

/// 显示用的一行
#[derive(Clone, Debug)]
pub struct Row {
    pub depth: usize,
    pub path: String,
    pub name: String,
    pub ty: String,
    pub off: String,
    pub hex: String,
    pub value: String,
    pub group: bool,
    pub missing: bool,
}

fn sext(v: u64, bits: u64) -> i64 {
    if bits >= 64 {
        v as i64
    } else {
        ((v << (64 - bits)) as i64) >> (64 - bits)
    }
}

pub fn value(defs: &Defs, f: &Field, data: &[u8], o: &Opts) -> (String, String, bool) {
    if f.kind == Leaf::CharArray {
        let (a, b) = ((f.bit_off / 8) as usize, ((f.bit_off + f.bits) / 8) as usize);
        let Some(s) = data.get(a..b.min(data.len())).filter(|s| !s.is_empty()) else {
            return (String::new(), String::new(), true);
        };
        let end = s.iter().position(|c| *c == 0).unwrap_or(s.len());
        let txt: String = s[..end]
            .iter()
            .map(|c| if (0x20..0x7f).contains(c) { (*c as char).to_string() } else { format!("\\x{c:02X}") })
            .collect();
        let hex = s.iter().take(16).map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ") + if s.len() > 16 { " …" } else { "" };
        return (hex, format!("\"{txt}\""), b > data.len());
    }
    let Some(v) = read_bits(data, f.bit_off, f.bits, o.little, f.is_bitfield) else {
        return (String::new(), String::new(), true);
    };
    let digits = (f.bits as usize).div_ceil(4);
    let hex = format!("0x{v:0digits$X}");
    let val = match f.kind {
        Leaf::Int(true) => sext(v, f.bits).to_string(),
        Leaf::Int(false) | Leaf::Ptr => v.to_string(),
        Leaf::Float => format!("{}", f32::from_bits(v as u32)),
        Leaf::Double => format!("{}", f64::from_bits(v)),
        Leaf::Bool => (v != 0).to_string(),
        Leaf::Char => {
            if (0x20..0x7f).contains(&v) {
                format!("'{}'  {v}", v as u8 as char)
            } else {
                sext(v, f.bits).to_string()
            }
        }
        Leaf::Enum(e) => {
            let sv = sext(v, f.bits);
            match defs.enums.get(e).and_then(|vs| vs.iter().find(|x| x.0 == sv || x.0 as u64 == v)) {
                Some((_, n)) => format!("{n} ({sv})"),
                None => sv.to_string(),
            }
        }
        _ => String::new(),
    };
    (hex, val, false)
}

pub fn rows(defs: &Defs, root: &Field, data: &[u8], o: &Opts) -> Vec<Row> {
    let mut out = vec![];
    fn walk(defs: &Defs, f: &Field, data: &[u8], o: &Opts, depth: usize, path: &str, out: &mut Vec<Row>) {
        for k in &f.kids {
            let p = format!("{path}.{}", k.name);
            let off = if k.is_bitfield {
                format!("{}.{}", k.bit_off / 8, k.bit_off % 8)
            } else {
                (k.bit_off / 8).to_string()
            };
            let group = !k.kids.is_empty();
            let (hex, value, missing) = if group {
                (String::new(), String::new(), (k.bit_off + k.bits).div_ceil(8) > data.len() as u64)
            } else {
                self::value(defs, k, data, o)
            };
            out.push(Row {
                depth,
                path: p.clone(),
                name: k.name.clone(),
                ty: k.ty.clone(),
                off,
                hex,
                value,
                group,
                missing,
            });
            if group {
                walk(defs, k, data, o, depth + 1, &p, out);
            }
        }
    }
    walk(defs, root, data, o, 0, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(src: &str, o: &Opts) -> (Defs, Field) {
        let d = parse(src, o.ptr).unwrap();
        let f = d.layout(d.main(), o);
        (d, f)
    }

    #[test]
    fn natural_alignment() {
        let o = Opts::default();
        let (_, f) = lay("typedef struct { uint8_t a; uint32_t b; uint16_t c; } S;", &o);
        assert_eq!(f.bits / 8, 12);
        assert_eq!(f.kids[1].bit_off / 8, 4);
        assert_eq!(f.kids[2].bit_off / 8, 8);
        let (_, f) = lay("typedef struct __attribute__((packed)) { uint8_t a; uint32_t b; uint16_t c; } S;", &o);
        assert_eq!(f.bits / 8, 7);
        let (_, f) = lay("#pragma pack(push, 2)\nstruct s { char a; int b; };\n#pragma pack(pop)\n", &o);
        assert_eq!(f.bits / 8, 6);
        let (_, f) = lay("typedef struct { char a; double d; } S;", &Opts { force_packed: true, ..o });
        assert_eq!(f.bits / 8, 9);
    }

    #[test]
    fn nested_arrays_unions() {
        let src = "#define N 3\ntypedef enum { IDLE, RUN = 5 } state_t;\ntypedef struct { int16_t x, y; } pt_t;\ntypedef struct {\n  uint8_t id; // comment\n  pt_t pts[N];\n  union { uint32_t u; float f; } v;\n  state_t st;\n  char name[4];\n  void (*cb)(int);\n} msg_t;";
        let o = Opts::default();
        let (d, f) = lay(src, &o);
        assert_eq!(f.name, "msg_t");
        // id 1 + pad 1 + pts 12 + pad 2 + v 4 + st 4 + name 4 + cb 4 = 32
        assert_eq!(f.bits / 8, 32);
        assert_eq!(f.kids[1].kids.len(), 3);
        assert_eq!(f.kids[1].kids[2].kids[1].bit_off / 8, 2 + 8 + 2);
        let mut data = vec![0u8; 32];
        data[0] = 7;
        data[2] = 0xFF;
        data[3] = 0xFF;
        data[16..20].copy_from_slice(&1.5f32.to_le_bytes());
        data[20] = 5;
        data[24..28].copy_from_slice(b"ab\0\0");
        let r = rows(&d, &f, &data, &o);
        let get = |p: &str| r.iter().find(|x| x.path == p).unwrap().value.clone();
        assert_eq!(get(".id"), "7");
        assert_eq!(get(".pts.[0].x"), "-1");
        assert_eq!(get(".v.f"), "1.5");
        assert_eq!(get(".st"), "RUN (5)");
        assert_eq!(get(".name"), "\"ab\"");
        assert!(!r.iter().any(|x| x.missing));
        let short = rows(&d, &f, &data[..10], &o);
        assert!(short.iter().find(|x| x.path == ".st").unwrap().missing);
    }

    #[test]
    fn bitfields() {
        let o = Opts::default();
        let (d, f) = lay("struct flags { uint8_t a:3; uint8_t b:4; uint8_t c:3; uint16_t d; };", &o);
        // a,b 在第 0 字节；c 放不下，换到第 1 字节；d 对齐到 2
        assert_eq!(f.kids[2].bit_off, 8);
        assert_eq!(f.kids[3].bit_off, 16);
        assert_eq!(f.bits / 8, 4);
        let data = [0b1010_1101u8, 0b0000_0110, 0x34, 0x12];
        let r = rows(&d, &f, &data, &o);
        assert_eq!(r[0].value, "5");
        assert_eq!(r[1].value, "5");
        assert_eq!(r[2].value, "6");
        assert_eq!(r[3].value, "4660");
        let be = Opts { little: false, ..o };
        let r = rows(&d, &f, &data, &be);
        assert_eq!(r[0].value, "5");
        assert_eq!(r[3].value, "13330");
    }
}

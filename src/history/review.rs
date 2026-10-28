//! 审阅时对每个改动文件的分析：增删行数、是不是只改了格式、编码和换行符有没有被改、是不是敏感文件。

use super::ignore::Ignore;
use crate::compare::diff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enc {
    Utf8,
    Utf8Bom,
    Gbk,
    /// 二进制，或空文件
    Binary,
}

impl Enc {
    pub fn label(self) -> &'static str {
        match self {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 BOM",
            Enc::Gbk => "GBK",
            Enc::Binary => "BIN",
        }
    }
}

pub fn detect(b: &[u8]) -> Enc {
    if b.iter().take(8000).any(|&c| c == 0) {
        return Enc::Binary;
    }
    if b.starts_with(b"\xef\xbb\xbf") {
        return Enc::Utf8Bom;
    }
    if std::str::from_utf8(b).is_ok() {
        Enc::Utf8
    } else {
        Enc::Gbk
    }
}

pub fn decode(b: &[u8]) -> String {
    match detect(b) {
        Enc::Gbk => encoding_rs::GBK.decode(b).0.into_owned(),
        _ => String::from_utf8_lossy(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b)).into_owned(),
    }
}

pub fn encode(s: &str, e: Enc) -> Vec<u8> {
    match e {
        Enc::Gbk => encoding_rs::GBK.encode(s).0.into_owned(),
        Enc::Utf8Bom => {
            let mut v = b"\xef\xbb\xbf".to_vec();
            v.extend_from_slice(s.as_bytes());
            v
        }
        _ => s.as_bytes().to_vec(),
    }
}

/// 文本里是否只能用 UTF-8 表示（GBK 编不出来的字符）
pub fn gbk_lossless(s: &str) -> bool {
    !encoding_rs::GBK.encode(s).2
}

fn crlf(b: &[u8]) -> bool {
    b.windows(2).any(|w| w == b"\r\n")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub add: usize,
    pub del: usize,
    pub binary: bool,
    /// 去掉空白和空行后没有区别
    pub format_only: bool,
    /// 编码被改：(原来, 现在)
    pub enc: Option<(Enc, Enc)>,
    /// 换行符被改：(原来是 CRLF, 现在是 CRLF)
    pub eol: Option<(bool, bool)>,
    pub sensitive: bool,
}

impl Default for Enc {
    fn default() -> Self {
        Enc::Utf8
    }
}

pub fn analyze(base: Option<&[u8]>, cur: Option<&[u8]>) -> Info {
    let mut i = Info::default();
    let (eb, ec) = (base.map(detect), cur.map(detect));
    if eb == Some(Enc::Binary) || ec == Some(Enc::Binary) {
        i.binary = true;
        return i;
    }
    let (la, _) = diff::split_lines(&base.map(decode).unwrap_or_default());
    let (lb, _) = diff::split_lines(&cur.map(decode).unwrap_or_default());
    let la = if base.is_none() { vec![] } else { la };
    let lb = if cur.is_none() { vec![] } else { lb };
    let (ka, _) = diff::keys(&la, &diff::Opts::default(), diff::CommentStyle::None);
    let (kb, _) = diff::keys(&lb, &diff::Opts::default(), diff::CommentStyle::None);
    for h in diff::hunks(&ka, &kb) {
        i.add += h.bn;
        i.del += h.an;
    }
    if let (Some(a), Some(b)) = (base, cur) {
        let (a_, b_) = (eb.unwrap_or_default(), ec.unwrap_or_default());
        // 纯 ASCII 的文件在 GBK 和 UTF-8 下一样，不算编码被改
        let ascii = a.is_ascii() || b.is_ascii();
        if a_ != b_ && !(ascii && matches!((a_, b_), (Enc::Gbk, Enc::Utf8) | (Enc::Utf8, Enc::Gbk))) {
            i.enc = Some((a_, b_));
        }
        if !a.is_empty() && !b.is_empty() && crlf(a) != crlf(b) {
            i.eol = Some((crlf(a), crlf(b)));
        }
        if i.add + i.del > 0 {
            let o = diff::Opts {
                ws: true,
                blank: true,
                ..Default::default()
            };
            let (ka, ba) = diff::keys(&la, &o, diff::CommentStyle::None);
            let (kb, bb) = diff::keys(&lb, &o, diff::CommentStyle::None);
            let fa: Vec<u64> = ka.iter().zip(&ba).filter(|(_, b)| !**b).map(|(k, _)| *k).collect();
            let fb: Vec<u64> = kb.iter().zip(&bb).filter(|(_, b)| !**b).map(|(k, _)| *k).collect();
            i.format_only = fa == fb;
        }
    }
    i
}

/// 保留现在的内容，但换回原来的编码和换行符
pub fn restore_encoding(base: &[u8], cur: &[u8]) -> Result<Vec<u8>, String> {
    let e = detect(base);
    let mut s = decode(cur);
    if e == Enc::Gbk && !gbk_lossless(&s) {
        return Err(crate::i18n::t("新内容里有 GBK 表示不了的字符，不能改回 GBK").to_string());
    }
    let want_crlf = crlf(base);
    s = s.replace("\r\n", "\n");
    if want_crlf {
        s = s.replace('\n', "\r\n");
    }
    Ok(encode(&s, e))
}

/// 默认的敏感文件：被改时在审阅清单里醒目标出（只提醒，不拦截）
pub const SENSITIVE_DEFAULT: &str = "\
# 链接脚本
*.ld
*.lds
*.sct
*.icf
# 启动和时钟
startup_*.s
startup_*.S
system_*.c
# 配置
partitions*.csv
sdkconfig
sdkconfig.defaults
.config
prj.conf
Kconfig
FreeRTOSConfig.h
*.dts
*.dtsi
*.overlay
# 工程和构建文件
*.uvprojx
*.uvproj
*.ewp
.cproject
*.ioc
platformio.ini
CMakeLists.txt
Makefile
# 厂商库和第三方库
Drivers/
Middlewares/
RTE/
managed_components/
third_party/
vendor/
";

pub struct Sensitive(Ignore);

impl Sensitive {
    pub fn new(text: &str) -> Self {
        let mut ig = Ignore::default();
        ig.add_text("", text);
        Sensitive(ig)
    }

    pub fn hit(&self, rel: &str) -> bool {
        self.0.is_ignored(rel, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_changes() {
        let a = "int a;\r\nint b;\r\n".as_bytes();
        let b = "int a;\nint  b;\nint c;\n".as_bytes();
        let i = analyze(Some(a), Some(b));
        assert_eq!((i.add, i.del), (2, 1));
        assert_eq!(i.eol, Some((true, false)));
        assert!(!i.format_only);
        let i = analyze(Some(b"int a;\nint b;\n"), Some(b"int a;\n\n  int  b;\n"));
        assert!(i.format_only);
        let gbk = encoding_rs::GBK.encode("// 中文注释\n").0.into_owned();
        let i = analyze(Some(&gbk), Some("// 中文注释\n".as_bytes()));
        assert_eq!(i.enc, Some((Enc::Gbk, Enc::Utf8)));
        assert_eq!((i.add, i.del), (0, 0));
        let back = restore_encoding(&gbk, "// 中文注释\n".as_bytes()).unwrap();
        assert_eq!(back, gbk);
        let i = analyze(None, Some(b"x\ny\n"));
        assert_eq!(i.add, 2);
        let s = Sensitive::new(SENSITIVE_DEFAULT);
        assert!(s.hit("Core/STM32F103C8Tx_FLASH.ld"));
        assert!(s.hit("Drivers/STM32F1xx_HAL_Driver/Src/x.c"));
        assert!(s.hit("Core/Startup/startup_stm32f103c8tx.s"));
        assert!(!s.hit("Core/Src/main.c"));
    }
}

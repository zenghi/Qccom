//! 内置比较合并：文本、文件夹、十六进制、三方合并、表格，以及报告和外部比较工具。

pub mod diff;
pub mod folder;
pub mod hexcmp;
pub mod merge;
pub mod report;
pub mod table;

use std::path::{Path, PathBuf};

/// 本机装着的外部比较工具（名字, 程序路径）
pub fn external_tools() -> Vec<(&'static str, PathBuf)> {
    let mut v = vec![];
    let cands: &[(&str, &[&str])] = if cfg!(windows) {
        &[
            (
                "Beyond Compare",
                &[
                    r"C:\Program Files\Beyond Compare 5\BCompare.exe",
                    r"C:\Program Files\Beyond Compare 4\BCompare.exe",
                    r"C:\Program Files (x86)\Beyond Compare 4\BCompare.exe",
                    r"C:\Program Files (x86)\Beyond Compare 3\BCompare.exe",
                ],
            ),
            ("WinMerge", &[r"C:\Program Files\WinMerge\WinMergeU.exe", r"C:\Program Files (x86)\WinMerge\WinMergeU.exe"]),
            ("Meld", &[r"C:\Program Files\Meld\Meld.exe", r"C:\Program Files (x86)\Meld\Meld.exe"]),
        ]
    } else {
        &[("Beyond Compare", &["bcompare"]), ("Meld", &["meld"]), ("KDiff3", &["kdiff3"]), ("WinMerge", &[])]
    };
    for (name, paths) in cands {
        for p in *paths {
            let p = Path::new(p);
            let found = if p.is_absolute() { p.is_file().then(|| p.to_path_buf()) } else { which(p) };
            if let Some(f) = found {
                v.push((*name, f));
                break;
            }
        }
    }
    v
}

fn which(name: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

/// 识别比较方式：文件夹、固件 / 二进制、表格，否则文本
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Folder,
    Text,
    Hex,
    Merge,
    Table,
}

pub fn guess_mode(a: &Path, b: &Path) -> Mode {
    if a.is_dir() || b.is_dir() {
        return Mode::Folder;
    }
    let ext = |p: &Path| p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let (ea, eb) = (ext(a), ext(b));
    let bin = |e: &str| {
        matches!(
            e,
            "bin" | "hex" | "ihex" | "ihx" | "s19" | "s28" | "s37" | "srec" | "mot" | "elf" | "axf" | "out" | "img" | "dfu" | "o" | "a"
        )
    };
    if bin(&ea) || bin(&eb) {
        return Mode::Hex;
    }
    if (ea == "csv" || ea == "tsv") && (eb == "csv" || eb == "tsv") || (ea == "map" && eb == "map") {
        return Mode::Table;
    }
    // 内容里有 0 字节就是二进制
    let is_bin = |p: &Path| {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        std::fs::File::open(p).and_then(|mut f| f.read(&mut buf)).map(|n| buf[..n].contains(&0)).unwrap_or(false)
    };
    if is_bin(a) || is_bin(b) {
        Mode::Hex
    } else {
        Mode::Text
    }
}

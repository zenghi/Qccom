//! 从工程配置自动找芯片型号和 SVD 文件（v1.8）：
//! Keil（.uvprojx 里的 Device、SFDFile、PackID）、IAR（.ewp 的器件）、STM32CubeMX / CubeIDE（.ioc、.cproject）、
//! ESP-IDF（sdkconfig）、PlatformIO（board_build.mcu）；工程里直接放了 .svd 的优先用它。
//! 找 SVD 的地方：工程目录、Keil 支持包目录、STM32CubeIDE / CubeProgrammer、IAR、ESP-IDF 工具目录。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// 芯片型号（工程里写的原样）
    pub chip: String,
    /// 从哪里读到的：Keil / IAR / CubeMX / ESP-IDF …
    pub from: String,
    pub svd: Option<PathBuf>,
}

/// 浅层遍历，最多 depth 层、max 个条目；跳过 .git、node_modules 和编译目录
fn walk(dir: &Path, depth: usize, max: &mut usize, f: &mut dyn FnMut(&Path)) {
    if depth == 0 || *max == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if *max == 0 {
            return;
        }
        *max -= 1;
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().to_lowercase();
        if ft.is_dir() {
            if matches!(
                name.as_str(),
                ".git" | "node_modules" | ".pio" | "build" | "objects" | "listings" | "debug" | "release" | ".qccom" | ".cache"
            ) {
                continue;
            }
            walk(&p, depth - 1, max, f);
        } else if ft.is_file() {
            f(&p);
        }
    }
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let a = xml.find(&open)? + open.len();
    let b = xml[a..].find('<')? + a;
    let v = xml[a..b].trim();
    (!v.is_empty()).then_some(v)
}

/// 从工程文件里读芯片型号；Keil 的 SFDFile 能直接给出 SVD 在支持包里的位置
pub fn chip_of(root: &Path) -> Option<Found> {
    let mut files = vec![];
    let mut max = 4000;
    walk(root, 4, &mut max, &mut |p| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if n.ends_with(".uvprojx") || n.ends_with(".uvproj") || n.ends_with(".ewp") || n.ends_with(".ioc") || n == ".cproject" || n == "sdkconfig" || n == "platformio.ini" || n.ends_with(".svd") {
            files.push(p.to_path_buf());
        }
    });
    files.sort_by_key(|p| p.components().count());
    // 工程里自带的 SVD
    let direct = files.iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("svd"))).cloned();
    let mut found: Option<Found> = None;
    for p in &files {
        let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Ok(text) = std::fs::read_to_string(p).or_else(|_| std::fs::read(p).map(|b| String::from_utf8_lossy(&b).into_owned())) else {
            continue;
        };
        let f = if n.ends_with(".uvprojx") || n.ends_with(".uvproj") {
            tag(&text, "Device").map(|chip| {
                let svd = tag(&text, "SFDFile").and_then(|sfd| keil_sfd(sfd, tag(&text, "PackID")));
                Found {
                    chip: chip.into(),
                    from: "Keil".into(),
                    svd,
                }
            })
        } else if n.ends_with(".ewp") {
            // <name>OGChipSelectEditMenu</name><state>STM32F103C8	ST STM32F103C8</state>
            text.find("OGChipSelectEditMenu").and_then(|i| tag(&text[i..], "state")).map(|s| Found {
                chip: s.split(['\t', ' ']).next().unwrap_or(s).into(),
                from: "IAR".into(),
                svd: None,
            })
        } else if n.ends_with(".ioc") {
            let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()));
            get("Mcu.UserName=").or_else(|| get("Mcu.CPN=")).or_else(|| get("Mcu.Name=")).map(|chip| Found {
                chip,
                from: "CubeMX".into(),
                svd: None,
            })
        } else if n == ".cproject" {
            text.find("target_mcu").and_then(|i| {
                let s = &text[i..];
                let a = s.find("value=\"")? + 7;
                let b = s[a..].find('"')? + a;
                Some(Found {
                    chip: s[a..b].to_string(),
                    from: "CubeIDE".into(),
                    svd: None,
                })
            })
        } else if n == "sdkconfig" {
            text.lines().find_map(|l| l.strip_prefix("CONFIG_IDF_TARGET=")).map(|v| Found {
                chip: v.trim_matches('"').to_string(),
                from: "ESP-IDF".into(),
                svd: None,
            })
        } else if n == "platformio.ini" {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("board_build.mcu").map(|r| r.trim_start_matches([' ', '=']).trim().to_string()))
                .filter(|v| !v.is_empty())
                .map(|chip| Found {
                    chip,
                    from: "PlatformIO".into(),
                    svd: None,
                })
        } else {
            None
        };
        if let Some(f) = f {
            // Keil 能直接定位 SVD 的优先
            let better = found.as_ref().is_none_or(|old| old.svd.is_none() && f.svd.is_some());
            if better {
                found = Some(f);
            }
        }
    }
    match (found, direct) {
        (Some(mut f), Some(d)) => {
            f.svd = Some(d);
            Some(f)
        }
        (Some(f), None) => Some(f),
        (None, Some(d)) => Some(Found {
            chip: d.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            from: "SVD".into(),
            svd: Some(d),
        }),
        (None, None) => None,
    }
}

/// Keil 支持包放在哪：用户改过的路径猜不到，常见的几个都看看
fn pack_roots() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(l).join("Arm").join("Packs"));
    }
    for d in ["C:\\Keil_v5\\ARM\\PACK", "C:\\Keil\\ARM\\PACK", "D:\\Keil_v5\\ARM\\PACK", "D:\\Keil\\ARM\\PACK"] {
        v.push(PathBuf::from(d));
    }
    if let Some(h) = home() {
        v.push(h.join(".cache").join("arm").join("packs"));
        v.push(h.join("AppData").join("Local").join("Arm").join("Packs"));
    }
    if let Some(p) = std::env::var_os("CMSIS_PACK_ROOT") {
        v.insert(0, PathBuf::from(p));
    }
    v.into_iter().filter(|p| p.is_dir()).collect()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// $$Device:STM32F103C8$SVD\STM32F103xx.svd + Keil.STM32F1xx_DFP.2.4.0 → 支持包里的文件
fn keil_sfd(sfd: &str, pack: Option<&str>) -> Option<PathBuf> {
    let rel = sfd.rsplit('$').next()?.replace('\\', "/");
    if !rel.to_lowercase().ends_with(".svd") {
        return None;
    }
    let pack = pack?;
    let mut it = pack.splitn(3, '.');
    let (vendor, rest) = (it.next()?, it.next()?);
    let ver = it.next().unwrap_or("");
    for r in pack_roots() {
        let base = r.join(vendor).join(rest);
        let p = base.join(ver).join(&rel);
        if p.is_file() {
            return Some(p);
        }
        // 版本号对不上：用装着的任意一个版本
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path().join(&rel);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// 可能放着 SVD 的目录
fn svd_roots(root: &Path) -> Vec<(PathBuf, usize)> {
    let mut v: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 5)];
    for r in pack_roots() {
        v.push((r, 6));
    }
    let mut globs: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        for d in [
            "C:\\ST",
            "C:\\Program Files\\STMicroelectronics",
            "C:\\Program Files\\IAR Systems",
            "C:\\Program Files (x86)\\IAR Systems",
        ] {
            globs.push(PathBuf::from(d));
        }
    } else {
        globs.push(PathBuf::from("/opt/st"));
        globs.push(PathBuf::from("/usr/share/svd"));
    }
    if let Some(h) = home() {
        globs.push(h.join("STMicroelectronics"));
        globs.push(h.join(".espressif"));
        globs.push(h.join(".platformio").join("platforms"));
        globs.push(h.join("st"));
    }
    for g in globs.into_iter().filter(|p| p.is_dir()) {
        v.push((g, 10));
    }
    v
}

/// 文件名和芯片型号的匹配程度：SVD 名里的 x 当通配符；文件名必须整个对上芯片型号的开头
pub fn score(chip: &str, stem: &str) -> usize {
    let c: Vec<char> = chip.to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let s: Vec<char> = stem.to_uppercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if s.len() < 4 || s.len() > c.len() {
        return 0;
    }
    for (i, ch) in s.iter().enumerate() {
        if *ch != c[i] && *ch != 'X' {
            return 0;
        }
    }
    s.len()
}

/// 按芯片型号在常见目录里找 SVD
pub fn search(root: &Path, chip: &str) -> Option<PathBuf> {
    let mut best: Option<(usize, usize, PathBuf)> = None;
    for (dir, depth) in svd_roots(root) {
        let mut max = 60_000;
        walk(&dir, depth, &mut max, &mut |p| {
            if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("svd")) {
                return;
            }
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let sc = score(chip, &stem);
            let len = p.as_os_str().len();
            if sc > 0 && best.as_ref().is_none_or(|b| (sc, usize::MAX - len) > (b.0, usize::MAX - b.1)) {
                best = Some((sc, len, p.to_path_buf()));
            }
        });
    }
    best.map(|b| b.2)
}

/// 完整流程：读芯片型号，没有直接给出 SVD 时再去找
pub fn find(root: &Path) -> Option<Found> {
    let mut f = chip_of(root)?;
    if f.svd.is_none() {
        f.svd = search(root, &f.chip);
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_and_score() {
        assert_eq!(score("STM32F103C8Tx", "STM32F103xx"), 11);
        assert_eq!(score("STM32F103C8Tx", "STM32F103"), 9);
        assert_eq!(score("STM32F103C8Tx", "STM32F107xx"), 0);
        assert_eq!(score("esp32c3", "esp32c3"), 7);
        assert_eq!(score("esp32c3", "esp32"), 5);
        let dir = std::env::temp_dir().join(format!("qccom-svdfind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("MDK")).unwrap();
        std::fs::write(dir.join("board.ioc"), "Mcu.Name=STM32F103C(8-B)Tx\nMcu.UserName=STM32F103C8Tx\n").unwrap();
        let f = chip_of(&dir).unwrap();
        assert_eq!((f.chip.as_str(), f.from.as_str()), ("STM32F103C8Tx", "CubeMX"));
        std::fs::write(
            dir.join("MDK/fw.uvprojx"),
            "<Device>STM32F407VGTx</Device><SFDFile>$$Device:STM32F407VGTx$CMSIS\\SVD\\STM32F40x.svd</SFDFile>",
        )
        .unwrap();
        std::fs::write(dir.join("STM32F40x.svd"), "<device/>").unwrap();
        let f = find(&dir).unwrap();
        assert_eq!(f.svd.as_deref(), Some(dir.join("STM32F40x.svd").as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

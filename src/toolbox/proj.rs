//! 导入 Keil（.uvprojx / .uvproj）和 IAR（.ewp）工程：读出宏定义、头文件路径和源文件，
//! 生成 compile_commands.json，让 clangd 和代码页的跳转按工程配置工作。
//! 还有 CubeMX 的 .ioc 引脚表。

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Target {
    pub name: String,
    pub device: String,
    pub defines: Vec<String>,
    /// 已经换成绝对路径
    pub includes: Vec<PathBuf>,
    /// (分组, 绝对路径)
    pub files: Vec<(String, PathBuf)>,
    /// 解析不了的路径（$TOOLKIT_DIR$ 等）
    pub skipped: Vec<String>,
    pub flags: String,
}

#[derive(Clone, Debug, Default)]
pub struct Project {
    pub kind: &'static str,
    pub path: PathBuf,
    pub targets: Vec<Target>,
}

impl Project {
    pub fn dir(&self) -> PathBuf {
        self.path.parent().map(Path::to_path_buf).unwrap_or_default()
    }
}

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn child<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(tag))
}

fn path_of<'a, 'i>(n: Node<'a, 'i>, tags: &[&str]) -> Option<Node<'a, 'i>> {
    let mut cur = n;
    for t in tags {
        cur = child(cur, t)?;
    }
    Some(cur)
}

fn text_at(n: Node, tags: &[&str]) -> String {
    path_of(n, tags).and_then(|c| c.text()).unwrap_or("").trim().to_string()
}

/// 工程里的路径（反斜杠、相对路径、.\ 开头）换成本机的绝对路径
fn resolve(dir: &Path, p: &str) -> PathBuf {
    let p = p.trim().trim_matches('"').replace('\\', "/");
    let pb = PathBuf::from(&p);
    let abs = if pb.is_absolute() || p.chars().nth(1) == Some(':') { pb } else { dir.join(pb) };
    normalize(&abs)
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn split_defines(s: &str) -> Vec<String> {
    s.split([',', ' ', ';', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|x| seen.insert(x.clone()));
}

pub fn parse_keil(xml: &str, path: &Path) -> Result<Project, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let targets_node = child(root, "Targets").ok_or("no <Targets> (not a Keil project)")?;
    let mut targets = vec![];
    for tn in targets_node.children().filter(|c| c.has_tag_name("Target")) {
        let name = text_at(tn, &["TargetName"]);
        let opt = child(tn, "TargetOption");
        let device = opt.map(|o| text_at(o, &["TargetCommonOption", "Device"])).unwrap_or_default();
        let cads = opt.and_then(|o| path_of(o, &["TargetArmAds", "Cads", "VariousControls"]));
        let mut defines = cads.map(|c| split_defines(&text_at(c, &["Define"]))).unwrap_or_default();
        dedup(&mut defines);
        let mut includes: Vec<PathBuf> = cads
            .map(|c| text_at(c, &["IncludePath"]))
            .unwrap_or_default()
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .map(|s| resolve(&dir, s))
            .collect();
        // 用了 RTE（运行时环境）时 Keil 自动加的目录
        let rte = dir.join("RTE").join(format!("_{}", name.replace(' ', "_")));
        if rte.is_dir() {
            includes.push(rte);
        }
        let flags = cads.map(|c| text_at(c, &["MiscControls"])).unwrap_or_default();
        let mut files = vec![];
        if let Some(gs) = child(tn, "Groups") {
            for g in gs.children().filter(|c| c.has_tag_name("Group")) {
                let gname = text_at(g, &["GroupName"]);
                if let Some(fs) = child(g, "Files") {
                    for f in fs.children().filter(|c| c.has_tag_name("File")) {
                        let fp = text_at(f, &["FilePath"]);
                        if !fp.is_empty() {
                            files.push((gname.clone(), resolve(&dir, &fp)));
                        }
                    }
                }
            }
        }
        targets.push(Target {
            name,
            device,
            defines,
            includes,
            files,
            skipped: vec![],
            flags,
        });
    }
    Ok(Project {
        kind: "Keil MDK",
        path: path.to_path_buf(),
        targets,
    })
}

fn iar_path(dir: &Path, s: &str, skipped: &mut Vec<String>) -> Option<PathBuf> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let d = dir.to_string_lossy();
    let r = s.replace("$PROJ_DIR$", &d).replace("$PROJ_FNAME$", "");
    if r.contains('$') {
        skipped.push(s.to_string());
        return None;
    }
    Some(resolve(dir, &r))
}

fn iar_files(n: Node, group: &str, dir: &Path, out: &mut Vec<(String, PathBuf)>, skipped: &mut Vec<String>) {
    for c in n.children() {
        if c.has_tag_name("group") {
            let g = text_at(c, &["name"]);
            let name = if group.is_empty() { g } else { format!("{group}/{g}") };
            iar_files(c, &name, dir, out, skipped);
        } else if c.has_tag_name("file") {
            if let Some(p) = iar_path(dir, &text_at(c, &["name"]), skipped) {
                out.push((group.to_string(), p));
            }
        }
    }
}

pub fn parse_iar(xml: &str, path: &Path) -> Result<Project, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    if !root.has_tag_name("project") {
        return Err("not an IAR project (<project>)".into());
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut files = vec![];
    let mut skipped_files = vec![];
    iar_files(root, "", &dir, &mut files, &mut skipped_files);
    let mut targets = vec![];
    for cfg in root.children().filter(|c| c.has_tag_name("configuration")) {
        let mut t = Target {
            name: text_at(cfg, &["name"]),
            files: files.clone(),
            skipped: skipped_files.clone(),
            ..Default::default()
        };
        for s in cfg.children().filter(|c| c.has_tag_name("settings")) {
            let Some(data) = child(s, "data") else { continue };
            for o in data.children().filter(|c| c.has_tag_name("option")) {
                let name = text_at(o, &["name"]);
                let states: Vec<String> = o.children().filter(|c| c.has_tag_name("state")).filter_map(|c| c.text()).map(|x| x.trim().to_string()).collect();
                match name.as_str() {
                    "CCDefines" => t.defines.extend(states.into_iter().filter(|x| !x.is_empty())),
                    "CCIncludePath2" => {
                        for st in states {
                            if let Some(p) = iar_path(&dir, &st, &mut t.skipped) {
                                t.includes.push(p);
                            }
                        }
                    }
                    "OGChipSelectEditMenu" => {
                        if let Some(st) = states.first() {
                            t.device = st.split('\t').next().unwrap_or("").to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        dedup(&mut t.defines);
        targets.push(t);
    }
    Ok(Project {
        kind: "IAR EWARM",
        path: path.to_path_buf(),
        targets,
    })
}

pub fn load(path: &Path) -> Result<Project, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = crate::cindex::decode(&b).map(|d| d.0).unwrap_or_else(|| String::from_utf8_lossy(&b).into_owned());
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "ewp" => parse_iar(&text, path),
        _ => parse_keil(&text, path),
    }
}

fn is_source(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str(), "c" | "cc" | "cpp" | "cxx")
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// 每个 C / C++ 源文件一条，只带宏和头文件路径（clangd 用）
pub fn compile_commands(p: &Project, t: &Target) -> String {
    let dir = p.dir();
    let mut args = vec!["clang".to_string(), "--target=arm-none-eabi".to_string()];
    args.extend(t.defines.iter().map(|d| format!("-D{d}")));
    args.extend(t.includes.iter().map(|i| format!("-I{}", i.display())));
    let mut out = String::from("[\n");
    let srcs: Vec<&PathBuf> = t.files.iter().map(|f| &f.1).filter(|f| is_source(f)).collect();
    for (i, f) in srcs.iter().enumerate() {
        let mut a = args.clone();
        a.push("-c".into());
        a.push(f.display().to_string());
        let list = a.iter().map(|x| json_str(x)).collect::<Vec<_>>().join(", ");
        out.push_str(&format!(
            "  {{\n    \"directory\": {},\n    \"file\": {},\n    \"arguments\": [{}]\n  }}{}\n",
            json_str(&dir.display().to_string()),
            json_str(&f.display().to_string()),
            list,
            if i + 1 < srcs.len() { "," } else { "" }
        ));
    }
    out.push_str("]\n");
    out
}

/// 所有源文件和头文件目录的公共上级目录（在代码页里打开它）
pub fn common_root(p: &Project, t: &Target) -> PathBuf {
    let mut root: Option<PathBuf> = None;
    for f in t.files.iter().map(|f| f.1.parent().map(Path::to_path_buf).unwrap_or_default()).chain(std::iter::once(p.dir())) {
        root = Some(match root {
            None => f,
            Some(r) => {
                let mut out = PathBuf::new();
                for (a, b) in r.components().zip(f.components()) {
                    if a != b {
                        break;
                    }
                    out.push(a);
                }
                out
            }
        });
    }
    root.unwrap_or_default()
}

// ------------------------------------------------------------------ CubeMX .ioc

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pin {
    pub pin: String,
    pub signal: String,
    pub label: String,
    pub mode: String,
    pub pull: String,
    pub speed: String,
}

#[derive(Clone, Debug, Default)]
pub struct Ioc {
    pub mcu: String,
    pub package: String,
    pub project: String,
    pub pins: Vec<Pin>,
}

/// PA9 → (A, 9)；PC14-OSC32_IN → (C, 14)
pub fn pin_key(p: &str) -> (String, u32) {
    let base = p.split(['-', ' ']).next().unwrap_or(p);
    let port: String = base.chars().skip(1).take_while(|c| c.is_ascii_alphabetic()).collect();
    let n: String = base.chars().skip(1 + port.len()).take_while(|c| c.is_ascii_digit()).collect();
    (port, n.parse().unwrap_or(0))
}

pub fn parse_ioc(text: &str) -> Ioc {
    let mut kv = std::collections::HashMap::new();
    let mut order = vec![];
    for l in text.lines() {
        if l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            // .ioc 里冒号、等号会被转义成 \:、\=
            let v = v.replace("\\:", ":").replace("\\=", "=").replace("\\#", "#");
            if k.starts_with("Mcu.Pin") && !k.starts_with("Mcu.PinsNb") {
                order.push(v.clone());
            }
            kv.insert(k.to_string(), v);
        }
    }
    let get = |k: &str| kv.get(k).cloned().unwrap_or_default();
    let mut pins: Vec<Pin> = order
        .into_iter()
        .filter(|p| !p.starts_with("VP_"))
        .map(|p| Pin {
            signal: get(&format!("{p}.Signal")),
            label: get(&format!("{p}.GPIO_Label")),
            mode: get(&format!("{p}.Mode")),
            pull: get(&format!("{p}.GPIO_PuPd")),
            speed: get(&format!("{p}.GPIO_Speed")),
            pin: p,
        })
        .collect();
    pins.sort_by_key(|p| pin_key(&p.pin));
    pins.dedup_by(|a, b| a.pin == b.pin);
    Ioc {
        mcu: get("Mcu.UserName").chars().next().map(|_| get("Mcu.UserName")).unwrap_or_else(|| get("Mcu.Name")),
        package: get("Mcu.Package"),
        project: get("ProjectManager.ProjectName"),
        pins,
    }
}

pub fn ioc_csv(io: &Ioc) -> String {
    let mut s = String::from("Pin,Signal,Label,Mode,Pull,Speed\n");
    let q = |x: &str| if x.contains([',', '"']) { format!("\"{}\"", x.replace('"', "\"\"")) } else { x.to_string() };
    for p in &io.pins {
        s.push_str(&format!("{},{},{},{},{},{}\n", q(&p.pin), q(&p.signal), q(&p.label), q(&p.mode), q(&p.pull), q(&p.speed)));
    }
    s
}

/// 和 CubeMX 生成的 main.h 一样的 #define（只有带标签的引脚）
pub fn ioc_defines(io: &Ioc) -> String {
    let mut s = String::new();
    for p in io.pins.iter().filter(|p| !p.label.is_empty()) {
        let (port, n) = pin_key(&p.pin);
        let id: String = p.label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        s.push_str(&format!("#define {id}_Pin GPIO_PIN_{n}\n#define {id}_GPIO_Port GPIO{port}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keil() {
        let x = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<Project><SchemaVersion>2.1</SchemaVersion><Targets><Target><TargetName>Debug Build</TargetName>
<TargetOption><TargetCommonOption><Device>STM32F407VGTx</Device></TargetCommonOption>
<TargetArmAds><Cads><VariousControls><MiscControls>--c99</MiscControls><Define>USE_HAL_DRIVER,STM32F407xx</Define><Undefine></Undefine>
<IncludePath>../Core/Inc;..\Drivers\CMSIS\Include</IncludePath></VariousControls></Cads></TargetArmAds></TargetOption>
<Groups><Group><GroupName>Application/User</GroupName><Files><File><FileName>main.c</FileName><FileType>1</FileType><FilePath>../Core/Src/main.c</FilePath></File>
<File><FileName>startup.s</FileName><FileType>2</FileType><FilePath>startup_stm32f407xx.s</FilePath></File></Files></Group></Groups>
</Target></Targets></Project>"#;
        let p = parse_keil(x, Path::new("/w/fw/MDK-ARM/fw.uvprojx")).unwrap();
        let t = &p.targets[0];
        assert_eq!(t.name, "Debug Build");
        assert_eq!(t.device, "STM32F407VGTx");
        assert_eq!(t.defines, vec!["USE_HAL_DRIVER", "STM32F407xx"]);
        assert_eq!(t.includes[0], PathBuf::from("/w/fw/Core/Inc"));
        assert_eq!(t.includes[1], PathBuf::from("/w/fw/Drivers/CMSIS/Include"));
        assert_eq!(t.files[0].1, PathBuf::from("/w/fw/Core/Src/main.c"));
        let cc = compile_commands(&p, t);
        let v: serde_json::Value = serde_json::from_str(&cc).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert!(cc.contains("-DUSE_HAL_DRIVER"));
        assert_eq!(common_root(&p, t), PathBuf::from("/w/fw"));
    }

    #[test]
    fn iar() {
        let x = r#"<?xml version="1.0" encoding="UTF-8"?>
<project><fileVersion>3</fileVersion>
<configuration><name>Debug</name>
<settings><name>General</name><data><option><name>OGChipSelectEditMenu</name><state>STM32F407VG	ST STM32F407VG</state></option></data></settings>
<settings><name>ICCARM</name><data>
<option><name>CCDefines</name><state>USE_HAL_DRIVER</state><state>STM32F407xx</state></option>
<option><name>CCIncludePath2</name><state>$PROJ_DIR$/../Core/Inc</state><state>$TOOLKIT_DIR$\inc</state></option>
</data></settings></configuration>
<group><name>Application</name><group><name>User</name><file><name>$PROJ_DIR$\..\Core\Src\main.c</name></file></group></group>
</project>"#;
        let p = parse_iar(x, Path::new("/w/fw/EWARM/fw.ewp")).unwrap();
        let t = &p.targets[0];
        assert_eq!(t.device, "STM32F407VG");
        assert_eq!(t.defines.len(), 2);
        assert_eq!(t.includes, vec![PathBuf::from("/w/fw/Core/Inc")]);
        assert_eq!(t.skipped.len(), 1);
        assert_eq!(t.files[0], ("Application/User".to_string(), PathBuf::from("/w/fw/Core/Src/main.c")));
    }

    #[test]
    fn ioc() {
        let x = "#MicroXplorer Configuration settings - do not modify\nMcu.Name=STM32F103C(8-B)Tx\nMcu.UserName=STM32F103C8Tx\nMcu.Package=LQFP48\nMcu.Pin0=PC13-TAMPER-RTC\nMcu.Pin1=PA9\nMcu.Pin2=VP_SYS_VS_Systick\nMcu.Pin3=PA10\nMcu.PinsNb=4\nPC13-TAMPER-RTC.GPIO_Label=LED\nPC13-TAMPER-RTC.Signal=GPIO_Output\nPA9.Signal=USART1_TX\nPA9.Mode=Asynchronous\nPA10.Signal=USART1_RX\n";
        let io = parse_ioc(x);
        assert_eq!(io.mcu, "STM32F103C8Tx");
        assert_eq!(io.pins.len(), 3);
        assert_eq!(io.pins[0].pin, "PA9");
        assert_eq!(io.pins[1].pin, "PA10");
        assert_eq!(io.pins[2].label, "LED");
        assert_eq!(ioc_defines(&io), "#define LED_Pin GPIO_PIN_13\n#define LED_GPIO_Port GPIOC\n");
        assert!(ioc_csv(&io).contains("PA9,USART1_TX,,Asynchronous"));
    }
}

use super::*;
use std::fs;

pub(super) const MAKEFILES: &[&str] = &["GNUmakefile", "makefile", "Makefile"];
const GENERATED: &str = "Automatically-generated file. Do not edit!";
const CLEAN_RULES: &str = "qccom-clean.mk";

pub(super) struct Project {
    pub name: String,
    pub configuration: String,
    pub dir: PathBuf,
    pub workspace: Option<tempfile::TempDir>,
    clean_rules: Option<tempfile::TempDir>,
    #[cfg(unix)]
    pub shims: Option<tempfile::TempDir>,
}

impl Project {
    pub(super) fn read(root: &Path, name: String, requested: &str) -> Option<Self> {
        let xml = fs::read_to_string(root.join(".cproject")).ok()?;
        let doc = roxmltree::Document::parse(&xml).ok()?;
        let configurations = doc.descendants().filter(|n| n.has_tag_name("configuration") && n.attribute("artifactName").is_some());
        let mut configurations = configurations.filter_map(|node| {
            let config = node.attribute("name")?;
            let builder = node.descendants().find(|child| child.has_tag_name("builder"))?;
            let dir = builder.attribute("buildPath").and_then(|path| path.strip_prefix("${workspace_loc:/")?.split_once("}/").map(|(_, dir)| dir));
            let dir = PathBuf::from(dir.unwrap_or(config));
            dir.components().all(|p| matches!(p, std::path::Component::Normal(_))).then_some((config, dir))
        });
        let (configuration, dir) = if requested.trim().is_empty() {
            configurations.next()?
        } else {
            configurations.find(|(config, _)| *config == requested.trim())?
        };
        Some(Self {
            name,
            configuration: configuration.into(),
            dir,
            workspace: None,
            clean_rules: None,
            #[cfg(unix)]
            shims: None,
        })
    }

    pub(super) fn selection(&self) -> String {
        format!("{}/{}", regex_lite::escape(&self.name), regex_lite::escape(&self.configuration))
    }

    pub(super) fn prepare_clean_rules(&mut self) -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join(CLEAN_RULES), include_str!("wch-clean.mk"))?;
        self.clean_rules = Some(dir);
        Ok(())
    }

    pub(super) fn clean_rules_path(&self) -> Option<PathBuf> {
        self.clean_rules.as_ref().map(|dir| dir.path().join(CLEAN_RULES))
    }

    #[cfg(unix)]
    pub(super) fn make_shim(&mut self) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let make = dir.path().join("make");
        fs::write(&make, include_str!("wch-make.sh"))?;
        fs::set_permissions(&make, fs::Permissions::from_mode(0o700))?;
        self.shims = Some(dir);
        Ok(())
    }
}

pub(super) struct IspConfig {
    pub model: String,
    pub serial: Option<bool>,
}

impl IspConfig {
    pub(super) fn read(path: &Path) -> Option<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut bytes).ok()?;
        let text = if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
            String::from_utf16_lossy(&bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect::<Vec<_>>())
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let mut section = "";
        let mut model = None;
        let mut serial = None;
        for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
            if line.starts_with('[') {
                section = line;
            } else if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().trim_matches('"');
                if section.eq_ignore_ascii_case("[Public]") && key.trim().eq_ignore_ascii_case("MCUName") && !value.is_empty() {
                    model = Some(value.to_owned());
                }
                // The CH57x–59x family uses this setting to select the hardware
                // interface, even if a serial port is present on the command line.
                if section.eq_ignore_ascii_case("[CH57x-59xUICfg]") && key.trim().eq_ignore_ascii_case("bDnInterType") {
                    serial = match value {
                        "0" => Some(false),
                        "1" => Some(true),
                        _ => None,
                    };
                }
            }
        }
        Some(Self { model: model?, serial })
    }
}

pub(super) fn makefile_name(root: &Path) -> Option<&'static str> {
    MAKEFILES.iter().copied().find(|name| root.join(name).is_file())
}

pub(super) fn has_makefile(root: &Path) -> bool {
    makefile_name(root).is_some()
}

pub(super) fn is_managed_makefile(path: &Path) -> bool {
    use std::io::{BufRead, BufReader, Read};
    let Ok(file) = fs::File::open(path) else { return false };
    BufReader::new(file.take(4096))
        .split(b'\n')
        .take(5)
        .flatten()
        .any(|line| line.windows(GENERATED.len()).any(|token| token == GENERATED.as_bytes()))
}

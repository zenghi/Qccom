use super::*;
use std::fs;

pub(super) const MAKEFILES: &[&str] = &["GNUmakefile", "makefile", "Makefile"];

pub(super) struct Project {
    pub name: String,
    pub configuration: String,
    pub dir: PathBuf,
    pub workspace: Option<tempfile::TempDir>,
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
            #[cfg(unix)]
            shims: None,
        })
    }

    pub(super) fn selection(&self) -> String {
        format!("{}/{}", regex_lite::escape(&self.name), regex_lite::escape(&self.configuration))
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

pub(super) fn has_makefile(root: &Path) -> bool {
    MAKEFILES.iter().any(|name| root.join(name).is_file())
}

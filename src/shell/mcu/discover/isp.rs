//! Find the Windows Studio family programmer or the standalone cross-platform ISP CLI.
use super::*;

const STUDIO: &str = "WCHISPStudio";
const COMMAND: &str = "WCHISPTool_CMD";
const COMMAND_NAMES: &[&str] = &["WCHISPTool_CMD", "WCHISPTool_CMD_x64", "WCHISPTool_CMD_x86", "wchisptool_cmd"];
const FAMILY_NAMES: &[(&str, &[&str])] = &[
    ("WCHISPTool_CH57x-59x", &["ch57", "ch58", "ch59"]),
    ("WCHISPTool_CH54x-55x", &["ch54", "ch55"]),
    ("WCHISPTool_CH56x", &["ch56"]),
    ("WCHISPTool_CH32Fxxx", &["ch32f"]),
    ("WCHISPTool_CH32Vxxx", &["ch32v"]),
];

pub(super) fn find(host: &Host, session: &Session, sdk: Option<&Path>, tool_dirs: &[PathBuf]) -> Option<(&'static str, PathBuf)> {
    if !session.options.wch_isp_tool.as_os_str().is_empty() {
        let path = session.root.join(&session.options.wch_isp_tool);
        if !executable(&path) {
            return None;
        }
        let name = path.file_name()?;
        // Never pass command-line arguments to the Studio launcher itself.
        if tool_name(name, COMMAND_NAMES) {
            return Some((COMMAND, path));
        }
        if FAMILY_NAMES.iter().any(|(family, _)| tool_name(name, &[*family])) {
            return Some((STUDIO, path));
        }
        return None;
    }

    let mut roots = registered_dirs(&["wchispstudio", "wchisptool", "wch isp studio", "wch isp tool"]);
    #[cfg(windows)]
    roots.extend(installed::app_path_dirs(&["WchIspStudio", "WchIspTool", "WCHISPTool_CMD", "WCHISPTool_CH57x-59x"]));
    roots.extend(host.var("WCHISPSTUDIO_HOME"));
    roots.extend(host.var("WCHISPTOOL_HOME"));

    let mut parents = host.program_files.clone();
    parents.extend(host.home.iter().cloned());
    parents.extend(host.home.iter().map(|home| home.join("Downloads")));
    parents.extend(host.var("LOCALAPPDATA").map(|path| path.join("Programs")));
    #[cfg(windows)]
    for drive in installed::fixed_drives() {
        parents.extend(["", "Program Files", "Program Files (x86)", "Software", "Tools", "WCH.CN/App"].map(|folder| drive.join(folder)));
    }
    if !cfg!(windows) {
        parents.push(PathBuf::from("/opt"));
    }
    let mut seen = std::collections::HashSet::new();
    for parent in parents.into_iter().filter(|p| seen.insert(p.clone()) && p.is_dir()) {
        roots.extend(child_dirs_matching(&parent, isp_directory));
        roots.extend(child_dirs_matching(&parent.join("WCH"), isp_directory));
    }

    let family = if cfg!(windows) { project_family(session) } else { None };
    let mut command = None;
    let mut studio = None;
    let mut scanned = std::collections::HashSet::new();
    for root in roots.iter().map(PathBuf::as_path).chain(sdk).filter(|dir| scanned.insert(*dir)) {
        nested::walk(root, &["docs", "include", "lib", "src", "examples", "node_modules"], |dir, files| {
            if !platform_dir(dir) || (studio.is_some() && command.is_some()) {
                return false;
            }
            if let Some(family) = family.filter(|_| studio.is_none()) {
                studio = named_file(dir, files, &[family]);
            }
            if command.is_none() {
                command = named_file(dir, files, COMMAND_NAMES);
            }
            true
        });
        if studio.is_some() || family.is_none() && command.is_some() {
            break;
        }
    }
    if let Some(family) = family {
        studio = studio.or_else(|| locate(tool_dirs, &[family])).or_else(|| locate(&host.path, &[family]));
    }
    if let Some(path) = studio {
        return Some((STUDIO, path));
    }
    command
        .or_else(|| locate(tool_dirs, COMMAND_NAMES))
        .or_else(|| locate(&host.path, COMMAND_NAMES))
        .map(|path| (COMMAND, path))
}

fn platform_dir(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return true };
    match name.to_string_lossy().to_ascii_lowercase().as_str() {
        "windows" | "win32" | "win64" => cfg!(windows),
        "linux" => cfg!(target_os = "linux"),
        "macos" | "mac" | "darwin" => cfg!(target_os = "macos"),
        "aarch64" | "arm64" => cfg!(target_arch = "aarch64"),
        "arm-gnueabi" | "arm-gnueabihf" => cfg!(target_arch = "arm"),
        "mips32" => cfg!(target_arch = "mips"),
        "mips64" => cfg!(target_arch = "mips64"),
        "x64" | "x86_64" | "amd64" => cfg!(target_arch = "x86_64"),
        "x86" | "i386" => cfg!(target_arch = "x86") || cfg!(all(windows, target_arch = "x86_64")),
        _ => true,
    }
}

fn isp_directory(name: &str) -> bool {
    name.starts_with("wchisp") || name.starts_with("wch isp")
}

fn named_file(dir: &Path, files: &[OsString], names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .find_map(|name| files.iter().filter(|f| tool_name(f, &[*name])).map(|f| dir.join(f)).find(|path| executable(path)))
}

fn family_for_text(text: &str) -> Option<&'static str> {
    let text = text.to_ascii_lowercase();
    let mut matched = FAMILY_NAMES.iter().filter(|(_, prefixes)| {
        text.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| {
            prefixes
                .iter()
                .any(|prefix| word.strip_prefix(prefix).is_some_and(|suffix| suffix.starts_with(|first: char| first.is_ascii_digit() || first == 'x')))
        })
    });
    let family = matched.next()?.0;
    matched.next().is_none().then_some(family)
}

fn project_family(session: &Session) -> Option<&'static str> {
    if let Some(config) = session.wch_isp_config_path() {
        if let Some(family) = wch::IspConfig::read(&config).and_then(|config| family_for_text(&config.model)) {
            return Some(family);
        }
    }
    for path in [&session.root, &session.workspace] {
        if let Some(family) = path.file_name().and_then(|name| family_for_text(&name.to_string_lossy())) {
            return Some(family);
        }
    }
    for name in [".cproject", ".project", "CMakeLists.txt", "Makefile", "makefile", "GNUmakefile"] {
        if let Some(family) = family_for_text(&read_prefix(&session.root.join(name))) {
            return Some(family);
        }
    }
    let startup = child_names(&session.root.join("Startup")).chain(child_names(&session.workspace.join("SRC/Startup")));
    family_for_text(&startup.collect::<Vec<_>>().join(" "))
}

fn child_names(path: &Path) -> impl Iterator<Item = String> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_family_matching_uses_chip_tokens() {
        assert_eq!(family_for_text("CH582M_翻盖柜"), Some("WCHISPTool_CH57x-59x"));
        assert_eq!(family_for_text("ld: startup_ch32v203.S"), Some("WCHISPTool_CH32Vxxx"));
        assert_eq!(family_for_text("CH582 and CH32V203"), None);
        assert_eq!(family_for_text("ch58folder"), None);
    }

    #[cfg(windows)]
    #[test]
    fn finds_family_program_inside_studio_install() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("portable/WCH.CN/App/WCHISPTool");
        let program = install.join("WCHISPTool_CH57x-59x/WCHISPTool_CH57x-59x.exe");
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(&program, []).unwrap();
        fs::write(install.join("WchIspStudio.exe"), []).unwrap();

        let root = temp.path().join("CH582M_翻盖柜");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("board.wvproj"), []).unwrap();
        let session = Session::discover(root, Options { sdk: install, ..Default::default() });
        assert_eq!(session.tools.get(STUDIO), Some(&program));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn selects_x64_from_multi_arch_program_package() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("package");
        for arch in ["aarch64", "x64"] {
            let bin = install.join("Linux/bin").join(arch);
            fs::create_dir_all(&bin).unwrap();
            let tool = bin.join(COMMAND);
            fs::write(&tool, []).unwrap();
            fs::set_permissions(tool, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let root = temp.path().join("CH582M");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("board.wvproj"), []).unwrap();
        let tool = install.join("Linux/bin/x64").join(COMMAND);
        let session = Session::discover(root, Options { sdk: install, ..Default::default() });
        assert_eq!(session.tools.get(COMMAND), Some(&tool));
    }
}

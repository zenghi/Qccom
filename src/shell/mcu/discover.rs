use super::*;
use std::fs;
use std::io::Read;

mod esp;
mod isp;
mod nested;
pub(in crate::shell) use nested::Projects;
#[cfg(windows)]
mod installed;
pub(super) use esp::EspIdf;

pub(super) fn session(root: PathBuf, options: Options) -> Session {
    detect(root.clone(), root, options, None)
}

fn detect(workspace: PathBuf, root: PathBuf, options: Options, selected: Option<Projects>) -> Session {
    let makefile = wch::MAKEFILES.iter().map(|name| root.join(name)).find(|file| file.is_file());
    let make = makefile.is_some();
    let cmake = root.join("CMakeLists.txt").is_file();
    let cmake_text = read_prefix(&root.join("CMakeLists.txt")).to_ascii_lowercase();
    let eclipse = read_prefix(&root.join(".project"));
    let cube_project = eclipse.contains("com.st.stm32cube.ide.mcu.").then(|| eclipse_project_name(&eclipse)).flatten();
    let sketch = root.file_name().map(|n| root.join(n).with_extension("ino"));
    let sketch = sketch.filter(|path| path.is_file());
    let wch_make_dir = ["obj", "Debug", "Release"]
        .into_iter()
        .find(|dir| wch::has_makefile(&root.join(dir)))
        .map(PathBuf::from);
    let mut profile = match options.profile {
        _ if selected.is_some() => Profile::Wch,
        Profile::Auto if root.join("platformio.ini").is_file() => Profile::PlatformIo,
        Profile::Auto if cmake_text.contains("tools/cmake/project.cmake") || cmake_text.contains("idf_build_process(") => Profile::EspIdf,
        Profile::Auto if cmake_text.contains("find_package(zephyr") => Profile::Zephyr,
        Profile::Auto if cmake_text.contains("pico_sdk_init(") || cmake_text.contains("pico_sdk_import.cmake") => Profile::Pico,
        Profile::Auto if wch_project(&root, &eclipse, &cmake_text, makefile.as_deref()) => Profile::Wch,
        Profile::Auto if cube_project.is_some() || has_ioc(&root) || root.join("cmake/stm32cubemx").is_dir() => Profile::Stm32,
        Profile::Auto if sketch.is_some() => Profile::Arduino,
        Profile::Auto if make => Profile::Make,
        Profile::Auto if cmake => Profile::CMake,
        Profile::Auto => Profile::System,
        profile => profile,
    };
    let projects = if let Some(projects) = selected {
        projects
    } else if matches!(profile, Profile::System | Profile::Wch) && matches!(options.profile, Profile::Auto | Profile::Wch)
        && !make && !cmake && wch_make_dir.is_none() && !root.join(".cproject").is_file()
        && !fs::read_dir(&root).into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("wvproj")))
    {
        let (projects, selected) = nested::projects(&workspace, &options.wch_subproject);
        if let Some(relative) = selected {
            let project = workspace.join(relative);
            return detect(workspace, project, options, Some(projects));
        }
        if projects.pending {
            profile = Profile::Wch;
        }
        projects
    } else {
        Projects { complete: true, ..Default::default() }
    };
    let cube_toolchain = ["cmake/gcc-arm-none-eabi.cmake", "cmake/arm-none-eabi-gcc.cmake"]
        .iter()
        .map(PathBuf::from)
        .find(|file| profile == Profile::Stm32 && root.join(file).is_file());

    let wch_project = if profile == Profile::Wch && !make && !cmake {
        eclipse_project_name(&eclipse).and_then(|name| wch::Project::read(&root, name, &options.configuration))
    } else {
        None
    };
    let mut s = Session {
        profile,
        options,
        root,
        workspace,
        projects,
        location: None,
        issue: None,
        env: BTreeMap::new(),
        tools: BTreeMap::new(),
        esp: None,
        make,
        cmake,
        cube_toolchain,
        cube_ide: None,
        wch_make_dir,
        wch_project,
        sketch,
    };
    if s.projects.pending {
        s.issue = Some(t("请在 MCU 工具链中选择沁恒子工程。").into());
        return s;
    }
    if !s.projects.complete {
        s.issue = Some(t("子目录搜索未完成，请打开更接近目标的工程目录。").into());
    }
    if profile == Profile::System {
        return s;
    }

    let host = Host::new();
    let sdk = (!s.options.sdk.as_os_str().is_empty()).then(|| s.workspace.join(&s.options.sdk));
    let mut paths = Vec::new();
    if let Some(dir) = sdk.as_deref().filter(|dir| dir.is_dir()) {
        paths.extend([dir.join("bin"), dir.join("Scripts"), dir.to_owned()]);
        s.location = Some(dir.to_owned());
    } else if let Some(dir) = sdk.as_deref() {
        s.issue = Some(tf("工具链目录不存在：{}", &[&dir.display()]));
    }

    let tools: &[(&str, &[&str])] = match profile {
        Profile::EspIdf => {
            match esp::activate(&host, sdk.as_deref()) {
                Ok(activated) => {
                    s.location = Some(activated.root);
                    s.env = activated.env;
                    paths.extend(activated.paths);
                    s.esp = Some(activated.idf);
                }
                Err(e) => s.issue = Some(e),
            }
            &[("make", &["make", "mingw32-make", "gmake"])]
        }
        Profile::Stm32 => {
            stm32_paths(&host, sdk.as_deref(), &mut paths);
            if let Some(project) = cube_project.filter(|_| !make && !cmake) {
                match tempfile::tempdir() {
                    Ok(workspace) => s.cube_ide = Some(CubeIde { project, workspace }),
                    Err(e) => s.issue = Some(tf("无法创建 STM32CubeIDE 工作区：{}", &[&e])),
                }
            }
            &[
                ("cmake", &["cmake"]),
                ("ninja", &["ninja"]),
                ("make", &["make", "mingw32-make", "gmake"]),
                ("arm-none-eabi-gcc", &["arm-none-eabi-gcc"]),
                ("STM32_Programmer_CLI", &["STM32_Programmer_CLI"]),
                ("STM32CubeIDE", &["stm32cubeidec", "stm32cubeide"]),
            ]
        }
        Profile::Wch => {
            let (location, ide) = wch_paths(&host, sdk.as_deref(), &mut paths);
            s.location = location.or(s.location);
            if let Some(project) = s.wch_project.as_mut() {
                if let Some(ide) = ide {
                    match tempfile::tempdir() {
                        Ok(workspace) => {
                            project.workspace = Some(workspace);
                            s.tools.insert("MounRiver Studio", ide);
                        }
                        Err(e) => s.issue = Some(tf("无法创建 MounRiver Studio 工作区：{}", &[&e])),
                    }
                }
                #[cfg(unix)]
                if let Err(e) = project.make_shim() {
                    s.issue = Some(tf("无法初始化 WCH 的 make 命令：{}", &[&e]));
                }
            }
            if let Some((kind, executable)) = isp::find(&host, &s, sdk.as_deref(), &paths) {
                s.tools.insert(kind, executable);
            }
            &[
                ("cmake", &["cmake"]),
                ("ninja", &["ninja"]),
                ("make", &["make", "mingw32-make", "gmake"]),
                ("riscv-none-embed-gcc", &["riscv-none-embed-gcc"]),
                ("riscv-none-elf-gcc", &["riscv-none-elf-gcc"]),
                ("riscv-wch-elf-gcc", &["riscv-wch-elf-gcc"]),
                ("riscv32-wch-elf-gcc", &["riscv32-wch-elf-gcc"]),
                ("arm-none-eabi-gcc", &["arm-none-eabi-gcc"]),
                ("wlink", &["wlink"]),
                ("wchisp", &["wchisp"]),
                ("openocd", &["openocd"]),
            ]
        }
        Profile::PlatformIo => {
            for dir in host.var("PLATFORMIO_CORE_DIR").into_iter().chain(host.home.iter().map(|p| p.join(".platformio"))) {
                paths.push(python_bin(&dir.join("penv")));
            }
            &[("pio", &["pio", "platformio"])]
        }
        Profile::Zephyr => {
            if let Some(workspace) = s.root.ancestors().find(|dir| dir.join(".west/config").is_file()) {
                paths.push(python_bin(&workspace.join(".venv")));
            }
            paths.push(python_bin(&s.root.join(".venv")));
            if let Some(home) = &host.home {
                paths.push(home.join(".local/bin"));
                paths.push(python_bin(&home.join("zephyrproject/.venv")));
                paths.push(python_bin(&home.join("ncs/.venv")));
            }
            let zephyr_sdk = sdk
                .as_ref()
                .filter(|p| p.join("sdk_version").is_file())
                .cloned()
                .or_else(|| host.var("ZEPHYR_SDK_INSTALL_DIR"))
                .or_else(|| {
                    host.home
                        .iter()
                        .cloned()
                        .chain([PathBuf::from(if cfg!(windows) { r"C:\" } else { "/opt" })])
                        .flat_map(|p| child_dirs(&p, "zephyr-sdk"))
                        .find(|p| p.join("sdk_version").is_file())
                });
            if let Some(path) = zephyr_sdk {
                s.env.insert("ZEPHYR_SDK_INSTALL_DIR".into(), path.as_os_str().into());
                s.location.get_or_insert(path);
            }
            &[("west", &["west"]), ("cmake", &["cmake"]), ("ninja", &["ninja"])]
        }
        Profile::Pico => {
            let pico = host.home.as_ref().map(|p| p.join(".pico-sdk"));
            let pico_sdk = sdk
                .as_ref()
                .filter(|dir| dir.join("pico_sdk_init.cmake").is_file())
                .cloned()
                .or_else(|| host.var("PICO_SDK_PATH").filter(|dir| dir.join("pico_sdk_init.cmake").is_file()))
                .or_else(|| pico.iter().flat_map(|p| child_dirs(&p.join("sdk"), "")).find(|p| p.join("pico_sdk_init.cmake").is_file()))
                .or_else(|| host.home.as_ref().map(|p| p.join("pico/pico-sdk")).filter(|p| p.join("pico_sdk_init.cmake").is_file()));
            if let Some(path) = pico_sdk {
                s.env.insert("PICO_SDK_PATH".into(), path.as_os_str().into());
                s.location.get_or_insert(path);
            }
            if let Some(dir) = pico {
                for package in ["toolchain", "picotool", "ninja", "cmake"] {
                    for version in child_dirs(&dir.join(package), "") {
                        paths.extend([version.join("bin"), version]);
                    }
                }
            }
            &[("cmake", &["cmake"]), ("ninja", &["ninja"]), ("arm-none-eabi-gcc", &["arm-none-eabi-gcc"]), ("picotool", &["picotool"])]
        }
        Profile::Arduino => {
            if let Some(home) = &host.home {
                paths.push(home.join(".local/bin"));
            }
            for dir in host.program_files.iter().cloned().chain(host.var("LOCALAPPDATA").map(|p| p.join("Programs"))) {
                paths.push(dir.join("Arduino IDE/resources/app/lib/backend/resources"));
            }
            for dir in registered_dirs(&["arduino ide"]) {
                paths.extend([dir.join("resources/app/lib/backend/resources"), dir]);
            }
            &[("arduino-cli", &["arduino-cli"])]
        }
        Profile::CMake => &[("cmake", &["cmake"]), ("ninja", &["ninja"])],
        Profile::Make => &[("make", &["make", "mingw32-make", "gmake"])],
        Profile::Auto | Profile::System => unreachable!(),
    };
    if matches!(profile, Profile::Stm32 | Profile::Wch | Profile::Pico | Profile::Zephyr | Profile::CMake) {
        for dir in &host.program_files {
            paths.push(dir.join("CMake/bin"));
        }
    }
    if matches!(profile, Profile::Stm32 | Profile::Wch | Profile::Pico | Profile::Zephyr | Profile::CMake) {
        paths.extend(registered_dirs(&["cmake"]).into_iter().map(|p| p.join("bin")));
    }
    paths.extend(host.path);
    paths.retain(|path| path.is_absolute() && path.is_dir());
    let mut seen = std::collections::HashSet::new();
    paths.retain(|dir| seen.insert(dir.clone()));
    if let Ok(path) = std::env::join_paths(&paths) {
        s.env.insert("QCCOM_MCU_TOOL_DIRS".into(), path.clone());
        s.env.insert("PATH".into(), path);
    }
    for (key, names) in tools {
        if let Some(tool) = locate(&paths, names) {
            s.tools.insert(key, tool);
        }
    }
    if let Some(esp) = &s.esp {
        s.tools.insert("idf.py", esp.script.clone());
    }
    if profile == Profile::Wch && (s.make || !s.cmake) && !s.tools.contains_key("make") && s.issue.is_none() {
        s.issue = Some(t("未找到 GNU Make。请在 MCU 工具链选择 MounRiver Studio 安装目录，或安装 GNU Make。").into());
    }
    s
}

fn has_ioc(root: &Path) -> bool {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .any(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ioc")) && p.is_file())
}

const WCH_CHIP_PREFIXES: &[&str] = &["ch32v", "ch32f", "ch56", "ch57", "ch58", "ch59", "ch641", "ch643", "ch32l", "ch32x", "ch32h"];

fn wch_project(root: &Path, eclipse: &str, cmake: &str, makefile: Option<&Path>) -> bool {
    let chip = |text: &str| WCH_CHIP_PREFIXES.iter().any(|prefix| text.contains(prefix));
    let named_for_chip = root.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        WCH_CHIP_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
    });
    if named_for_chip
        || eclipse.to_ascii_lowercase().contains("mounriver")
        || chip(cmake)
        || chip(&read_prefix(&root.join(".cproject")).to_ascii_lowercase())
        || makefile.is_some_and(|file| chip(&read_prefix(file).to_ascii_lowercase()))
    {
        return true;
    }
    for dir in ["", "Startup", "Core", "User", "Peripheral/inc", "EVT/EXAM/SRC/Startup"] {
        if fs::read_dir(root.join(dir)).into_iter().flatten().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name.ends_with(".wvproj") || ((name.ends_with(".s") || name.ends_with(".h") || name.ends_with(".ld")) && chip(&name))
        }) {
            return true;
        }
    }
    false
}

fn registered_dirs(_names: &[&str]) -> Vec<PathBuf> {
    #[cfg(windows)]
    return installed::directories(_names);
    #[cfg(not(windows))]
    Vec::new()
}

fn wch_paths(host: &Host, sdk: Option<&Path>, paths: &mut Vec<PathBuf>) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut roots: Vec<_> = sdk.into_iter().map(Path::to_owned).collect();
    for key in ["MRS2_HOME", "MRS3_HOME", "MRS_PATH", "MOUNRIVER_HOME", "WCH_TOOLCHAIN_ROOT"] {
        roots.extend(host.var(key));
    }
    roots.extend(registered_dirs(&["mounriver", "mrs community", "mrs studio"]));
    if let Some(home) = &host.home {
        paths.extend([home.join(".cargo/bin"), home.join(".local/bin")]);
    }
    let mut parents = host.program_files.clone();
    parents.extend(host.home.iter().cloned());
    parents.extend(host.var("LOCALAPPDATA").map(|path| path.join("Programs")));
    if cfg!(windows) {
        let system_drive = host.var("SystemRoot").and_then(|p| p.parent().map(Path::to_owned)).unwrap_or_else(|| PathBuf::from(r"C:\"));
        parents.push(system_drive);
    } else {
        parents.push(PathBuf::from("/opt"));
        roots.extend(["/usr/share/MRS2/MRS-linux-x64", "/usr/share/MRS3/MRS-linux-x64", "/usr/share/MounRiverStudio", "/Applications/MounRiver Studio.app"].map(PathBuf::from));
    }
    #[cfg(windows)]
    for drive in installed::fixed_drives() {
        parents.extend(["", "Program Files", "Program Files (x86)", "Software", "Soft", "Tools"].map(|folder| drive.join(folder)));
    }
    let mut parents_seen = std::collections::HashSet::new();
    for parent in parents.into_iter().filter(|p| parents_seen.insert(p.clone()) && p.is_dir()) {
        roots.extend(child_dirs_matching(&parent, |name| ["mounriver", "mrs", "wch"].iter().any(|prefix| name.starts_with(prefix))));
    }
    let mut seen = std::collections::HashSet::new();
    let mut location = None;
    let mut ide = None;
    for root in roots.into_iter().filter(|dir| dir.is_dir() && seen.insert(dir.clone())) {
        let before = paths.len();
        collect_wch_paths(&root, paths);
        nested::walk(&root, &["node_modules", "include", "lib", "libexec", "share", "plugins", "docs", "examples"], |dir, files| {
            if ide.is_none() && files.iter().any(|f| tool_name(f, WCH_IDES)) && dir.join("plugins").is_dir() {
                ide = locate(std::slice::from_ref(&dir.to_owned()), WCH_IDES);
            }
            if files.iter().any(|f| tool_name(f, WCH_TOOLS) && executable(&dir.join(f))) {
                paths.push(dir.to_owned());
            }
            true
        });
        if paths.len() > before {
            location.get_or_insert(root);
        }
    }
    // A user may pick the compiler's bin folder inside an IDE instead of its install root.
    for ancestor in sdk.into_iter().flat_map(|p| p.ancestors().skip(1).take(7)).filter(|p| p.parent().is_some()) {
        if ancestor.join("plugins").is_dir() || ancestor.join("resources/app/resources").is_dir() || ancestor.join("toolchain").is_dir() {
            collect_wch_paths(ancestor, paths);
            if ide.is_none() && ancestor.join("plugins").is_dir() {
                ide = locate(&[ancestor.to_owned()], WCH_IDES);
            }
        }
    }
    (location, ide)
}

const WCH_IDES: &[&str] = &["MounRiver Studio_Communityc", "MounRiver Studio_Community", "MounRiver Studioc", "MounRiver Studio", "MounRiverStudio"];
const WCH_TOOLS: &[&str] = &[
    "make", "gmake", "mingw32-make", "riscv-none-embed-gcc", "riscv-none-elf-gcc", "riscv-wch-elf-gcc",
    "riscv32-wch-elf-gcc", "arm-none-eabi-gcc", "openocd", "wlink", "wchisp", "WCHISPTool_CMD", "WCHISPTool_CMD_x64", "WCHISPTool_CMD_x86",
];

fn tool_name(file: &std::ffi::OsStr, names: &[&str]) -> bool {
    let path = Path::new(file);
    let name = if cfg!(windows) {
        if !path.extension().is_some_and(|ext| ["exe", "com", "cmd", "bat"].iter().any(|s| ext.eq_ignore_ascii_case(s))) {
            return false;
        }
        path.file_stem().unwrap_or_default()
    } else {
        file
    };
    names.iter().any(|s| name.eq_ignore_ascii_case(s))
}

fn collect_wch_paths(install: &Path, paths: &mut Vec<PathBuf>) {
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let platform_dirs = [
        install.join("resources/app/resources").join(platform),
        install.join("Contents/Resources/app/resources").join(platform),
        install.to_owned(),
    ];
    let mut found = Vec::new();
    for layer in platform_dirs {
        let wch = layer.join("components/WCH");
        let bundled_make = layer.join("others/Build_Tools/Make/bin");
        if locate(std::slice::from_ref(&bundled_make), &["make"]).is_some() {
            found.push(bundled_make);
        }
        found.extend([wch.join("OpenOCD/OpenOCD/bin"), layer.join("OpenOCD/OpenOCD/bin")]);
        for base in [
            wch.join("Toolchain"),
            layer.join("Toolchain"),
            layer.join("toolchain"),
            layer.join("toolchains"),
            layer.join("resources/app/resources/toolchain"),
            layer.join("resources/toolchain"),
        ] {
            found.push(base.join("bin"));
            found.extend(child_dirs(&base, "").into_iter().map(|package| package.join("bin")));
        }
    }
    for relative in [
        "bin",
        "tools/bin",
        "build_tools/bin",
        "build_tools/make/bin",
        "toolchain/Build Tools/bin",
        "toolchain/Build Tools/Make/bin",
    ] {
        found.push(install.join(relative));
    }
    found.push(install.to_owned());
    // Keep build helper programs from the same directory as the detected tool.
    found.retain(|dir| locate(std::slice::from_ref(dir), WCH_TOOLS).is_some());
    paths.extend(found);
}

fn eclipse_project_name(text: &str) -> Option<String> {
    let xml = roxmltree::Document::parse(text).ok()?;
    let name = xml.root_element().children().find(|node| node.has_tag_name("name"))?.text()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

struct Host {
    home: Option<PathBuf>,
    program_files: Vec<PathBuf>,
    path: Vec<PathBuf>,
}

impl Host {
    fn new() -> Self {
        let path = std::env::var_os("PATH").map(|v| std::env::split_paths(&v).filter(|p| p.is_absolute()).collect()).unwrap_or_default();
        let home = Self::env_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        let mut program_files = Vec::new();
        if cfg!(windows) {
            for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(dir) = Self::env_path(key).filter(|p| !program_files.contains(p)) {
                    program_files.push(dir);
                }
            }
        }
        Self { home, program_files, path }
    }

    fn env_path(key: &str) -> Option<PathBuf> {
        std::env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute())
    }

    fn var(&self, key: &str) -> Option<PathBuf> {
        Self::env_path(key)
    }
}

fn stm32_paths(host: &Host, sdk: Option<&Path>, paths: &mut Vec<PathBuf>) {
    let mut roots: Vec<_> = if cfg!(windows) {
        vec![PathBuf::from(r"C:\ST")]
    } else {
        vec![PathBuf::from("/opt/st"), PathBuf::from("/opt")]
    };
    roots.extend(sdk.map(Path::to_owned));
    roots.extend(host.program_files.iter().map(|p| p.join("STMicroelectronics")));
    roots.extend(registered_dirs(&["stm32cubeide", "stm32cubeprogrammer", "stm32cubeclt"]));
    for root in roots {
        paths.extend([root.join("STM32Cube/STM32CubeProgrammer/bin"), root.join("STM32CubeProgrammer/bin")]);
        for clt in std::iter::once(root.clone()).chain(child_dirs(&root, "stm32cubeclt")) {
            paths.extend([clt.join("GNU-tools-for-STM32/bin"), clt.join("STM32CubeProgrammer/bin"), clt.join("CMake/bin"), clt.join("Ninja/bin")]);
        }
        for ide in std::iter::once(root.clone()).chain(child_dirs(&root, "stm32cubeide")) {
            paths.extend([ide.join("STM32CubeIDE"), ide.clone()]);
            for plugins in [ide.join("plugins"), ide.join("STM32CubeIDE/plugins")] {
                for plugin in child_dirs(&plugins, "com.st.stm32cube.ide.") {
                    if let Some(name) = plugin.file_name().and_then(|n| n.to_str()) {
                        if name.contains(".tools.") || name.contains(".externaltools.") {
                            paths.extend([plugin.join("tools/bin"), plugin.join("tools")]);
                        }
                    }
                }
            }
        }
    }
    if let Some(root) = host.var("ARMGCC_DIR") {
        paths.push(root.join("bin"));
    }
    for root in &host.program_files {
        for prefix in ["Arm GNU Toolchain arm-none-eabi", "GNU Arm Embedded Toolchain"] {
            let base = root.join(prefix);
            paths.push(base.join("bin"));
            paths.extend(child_dirs(&base, "").into_iter().map(|version| version.join("bin")));
        }
    }
    for base in ["/opt", "/opt/arm"] {
        paths.extend(child_dirs(Path::new(base), "arm-gnu-toolchain").into_iter().map(|version| version.join("bin")));
    }
}

fn read_prefix(path: &Path) -> String {
    let mut bytes = Vec::new();
    if !path.is_file() {
        return String::new();
    }
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(256 * 1024).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn child_dirs(base: &Path, prefix: &str) -> Vec<PathBuf> {
    child_dirs_matching(base, |name| name.starts_with(prefix))
}

fn child_dirs_matching(base: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<_> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|name| keep(&name.to_string_lossy().to_ascii_lowercase())) && p.is_dir())
        .collect();
    out.sort_by_cached_key(|p| std::cmp::Reverse((numbers(&p.file_name().unwrap_or_default().to_string_lossy()), p.clone())));
    out
}

fn numbers(text: &str) -> Vec<u32> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect()
}

fn python_bin(root: &Path) -> PathBuf {
    root.join(if cfg!(windows) { "Scripts" } else { "bin" })
}

fn locate(paths: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    let extensions: &[&str] = if cfg!(windows) { &[".exe", ".com", ".cmd", ".bat"] } else { &[""] };
    paths
        .iter()
        .flat_map(|d| names.iter().flat_map(move |n| extensions.iter().map(move |ext| d.join(format!("{n}{ext}")))))
        .find(|p| executable(p))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    metadata.is_file()
}

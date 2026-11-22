use super::*;
use std::fs;
use std::io::Read;

mod esp;
pub(super) use esp::EspIdf;

pub(super) fn session(root: PathBuf, options: Options) -> Session {
    let makefiles = ["GNUmakefile", "makefile", "Makefile"];
    let makefile = makefiles.iter().map(|name| root.join(name)).find(|file| file.is_file());
    let make = makefile.is_some();
    let cmake = root.join("CMakeLists.txt").is_file();
    let cmake_text = read_prefix(&root.join("CMakeLists.txt")).to_ascii_lowercase();
    let eclipse = read_prefix(&root.join(".project"));
    let cube_project = eclipse.contains("com.st.stm32cube.ide.mcu.").then(|| eclipse_project_name(&eclipse)).flatten();
    let sketch = root.file_name().map(|n| root.join(n).with_extension("ino"));
    let sketch = sketch.filter(|path| path.is_file());
    let wch_make_dir = ["obj", "Debug", "Release"]
        .into_iter()
        .find(|dir| makefiles.iter().any(|name| root.join(dir).join(name).is_file()))
        .map(PathBuf::from);
    let profile = match options.profile {
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
    let cube_toolchain = ["cmake/gcc-arm-none-eabi.cmake", "cmake/arm-none-eabi-gcc.cmake"]
        .iter()
        .map(PathBuf::from)
        .find(|file| profile == Profile::Stm32 && root.join(file).is_file());

    let mut s = Session {
        profile,
        options,
        root,
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
        sketch,
    };
    if profile == Profile::System {
        return s;
    }

    let host = Host::new();
    let sdk = (!s.options.sdk.as_os_str().is_empty()).then(|| s.root.join(&s.options.sdk));
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
            wch_paths(&host, sdk.as_deref(), &mut paths);
            &[
                ("cmake", &["cmake"]),
                ("ninja", &["ninja"]),
                ("make", &["make", "mingw32-make", "gmake"]),
                ("riscv-none-embed-gcc", &["riscv-none-embed-gcc"]),
                ("riscv-none-elf-gcc", &["riscv-none-elf-gcc"]),
                ("riscv-wch-elf-gcc", &["riscv-wch-elf-gcc"]),
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
    paths.extend(host.path);
    paths.retain(|path| path.is_absolute() && path.is_dir());
    let mut seen = std::collections::HashSet::new();
    paths.retain(|dir| seen.insert(dir.clone()));
    if let Ok(path) = std::env::join_paths(&paths) {
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

fn wch_project(root: &Path, eclipse: &str, cmake: &str, makefile: Option<&Path>) -> bool {
    let chip = |text: &str| {
        ["ch32v", "ch32f", "ch56", "ch57", "ch58", "ch59", "ch641", "ch643", "ch32l", "ch32x", "ch32h"]
            .iter()
            .any(|prefix| text.contains(prefix))
    };
    if eclipse.to_ascii_lowercase().contains("mounriver") || chip(cmake) || makefile.is_some_and(|file| chip(&read_prefix(file).to_ascii_lowercase())) {
        return true;
    }
    for dir in ["", "Startup", "Core", "User", "Peripheral/inc", "EVT/EXAM/SRC/Startup"] {
        let found = fs::read_dir(root.join(dir)).into_iter().flatten().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name.ends_with(".wvproj") || ((name.ends_with(".s") || name.ends_with(".h") || name.ends_with(".ld")) && chip(&name))
        });
        if found {
            return true;
        }
    }
    false
}

fn wch_paths(host: &Host, sdk: Option<&Path>, paths: &mut Vec<PathBuf>) {
    let mut roots: Vec<_> = sdk.into_iter().map(Path::to_owned).collect();
    roots.extend(host.var("MRS_PATH"));
    roots.extend(host.var("MOUNRIVER_HOME"));
    roots.extend(host.var("WCH_TOOLCHAIN_ROOT"));
    if let Some(home) = &host.home {
        paths.extend([home.join(".cargo/bin"), home.join(".local/bin")]);
        roots.extend([home.join("MounRiver_Studio"), home.join("MounRiver Studio")]);
    }
    let common = if cfg!(windows) { Path::new(r"C:\") } else { Path::new("/opt") };
    for parent in std::iter::once(common.to_owned()).chain(host.program_files.iter().cloned()) {
        roots.extend(child_dirs(&parent, "mounriver"));
    }
    for root in roots {
        for install in std::iter::once(root.clone()).chain(child_dirs(&root, "mounriver")) {
            paths.extend([install.clone(), install.join("bin"), install.join("tools/bin")]);
            for packages in ["toolchain", "toolchains", "resources/app/resources/toolchain", "resources/toolchain"] {
                let base = install.join(packages);
                paths.push(base.join("bin"));
                paths.extend(child_dirs(&base, "").into_iter().map(|package| package.join("bin")));
            }
        }
    }
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
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(256 * 1024).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn child_dirs(base: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out: Vec<_> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().starts_with(prefix)) && p.is_dir())
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

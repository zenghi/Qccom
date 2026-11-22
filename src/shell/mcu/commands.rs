use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::shell) enum Syntax {
    PowerShell,
    Cmd,
    Posix,
    Fish,
}

impl Syntax {
    pub(in crate::shell) fn for_shell(kind: &ShellKind) -> Option<Self> {
        let exe = Path::new(&kind.program).file_stem()?.to_str()?.to_ascii_lowercase();
        match exe.as_str() {
            "powershell" | "pwsh" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            "bash" | "sh" | "dash" | "zsh" | "ksh" => Some(Self::Posix),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    fn quote(self, arg: &str) -> Result<String, String> {
        if arg.chars().any(char::is_control) || self == Self::Cmd && arg.contains(['"', '%', '!']) {
            return Err(t("命令参数含有当前终端无法安全传递的字符").into());
        }
        if !arg.is_empty() && !arg.starts_with('~') && arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-./:=+".contains(&b)) {
            return Ok(arg.to_string());
        }
        Ok(match self {
            Self::PowerShell => format!("'{}'", arg.replace('\'', "''")),
            Self::Cmd => format!("\"{arg}\""),
            Self::Posix => format!("'{}'", arg.replace('\'', "'\\''")),
            Self::Fish => format!("'{}'", arg.replace('\\', "\\\\").replace('\'', "\\'")),
        })
    }

    fn render(self, root: &Path, steps: &[Invocation]) -> Result<String, String> {
        let quote_path = |p: &Path| p.to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").into()).and_then(|p| self.quote(p));
        let cwd = quote_path(root)?;
        let commands = steps
            .iter()
            .map(|step| {
                let program = quote_path(&step.program)?;
                let args = step.args.iter().map(|arg| self.quote(arg)).collect::<Result<Vec<_>, _>>()?.join(" ");
                Ok(format!("{}{program} {args}", if self == Self::PowerShell { "& " } else { "" }).trim_end().to_owned())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(match self {
            Self::PowerShell => {
                let mut chain = String::new();
                for cmd in commands.into_iter().rev() {
                    chain = if chain.is_empty() { cmd } else { format!("{cmd}; if ($?) {{ {chain} }}") };
                }
                format!("Push-Location -LiteralPath {cwd}; if ($?) {{ try {{ {chain} }} finally {{ Pop-Location }} }}")
            }
            Self::Cmd => format!("pushd {cwd} && ({} & popd)", commands.join(" && ")),
            Self::Posix => format!("(cd {cwd} && {})", commands.join(" && ")),
            Self::Fish => format!("begin; set -l qccom_task_cwd $PWD; cd {cwd}; and {}; cd $qccom_task_cwd; end", commands.join("; and ")),
        })
    }
}

pub(super) const POWERSHELL_INIT: &str = r#"
chcp 65001 > $null;
[Console]::InputEncoding = [Console]::OutputEncoding = $OutputEncoding = New-Object System.Text.UTF8Encoding $false;
function global:idf.py { & $env:QCCOM_IDF_PYTHON $env:QCCOM_IDF_SCRIPT @args }
function global:make {
    $target = if ($args.Count -eq 0) { 'build' } else { $args[0] };
    if ($PWD.ProviderPath -eq $env:QCCOM_MCU_PROJECT -and $args.Count -le 1 -and
        $target -cin @('build', 'all', 'clean') -and
        !(Test-Path -LiteralPath 'Makefile','makefile','GNUmakefile' -PathType Leaf | Where-Object { $_ })) {
        if ($target -eq 'all') { $target = 'build' }
        $idfArgs = @('-B', $env:QCCOM_MCU_BUILD_DIR);
        idf.py @idfArgs $target;
    } elseif ($env:QCCOM_REAL_MAKE) {
        & $env:QCCOM_REAL_MAKE @args;
    } else {
        Write-Error 'GNU Make was not found.';
    }
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::shell) enum Action {
    Build,
    Flash,
    Monitor,
    Clean,
    AppFlash,
    FlashMonitor,
    AppFlashMonitor,
    Configure,
    Size,
    Version,
}

impl Action {
    pub(in crate::shell) fn label(self) -> &'static str {
        match self {
            Self::Build => t("编译"),
            Self::Flash => t("烧录"),
            Self::Monitor => t("串口监视"),
            Self::Clean => t("清理"),
            Self::AppFlash => t("烧录应用"),
            Self::FlashMonitor => t("烧录并监视"),
            Self::AppFlashMonitor => t("烧录应用并监视"),
            Self::Configure => "menuconfig",
            Self::Size => t("固件大小"),
            Self::Version => t("工具版本"),
        }
    }

    pub(in crate::shell) fn monitor_after(self) -> bool {
        matches!(self, Self::FlashMonitor | Self::AppFlashMonitor)
    }
}

pub(in crate::shell) struct Shortcut<'a> {
    pub action: Action,
    pub build: Option<&'a str>,
    pub explicit_port: Option<&'a str>,
}

impl Session {
    pub(in crate::shell) fn shortcut<'a>(&self, cmd: &'a str) -> Option<Shortcut<'a>> {
        // Only rewrite recognized commands. Leave pipelines, expressions, and other shell input alone.
        let words = shortcut_words(cmd)?;
        let mut shortcut = Shortcut {
            action: Action::Monitor,
            build: None,
            explicit_port: None,
        };
        match words.as_slice() {
            [program, target] if self.has_esp() && !self.make && program.eq_ignore_ascii_case("make") => {
                shortcut.action = match *target {
                    "flash" => Action::Flash,
                    "app-flash" => Action::AppFlash,
                    "monitor" => Action::Monitor,
                    _ => return None,
                };
            }
            [program, args @ .., "monitor"] if self.profile == Profile::EspIdf && program.eq_ignore_ascii_case("idf.py") => {
                let mut args = args.iter().copied().peekable();
                while let Some(flag) = args.next() {
                    match flag {
                        "-B" | "--build-dir" => shortcut.build = Some(args.next()?),
                        "-p" | "--port" => shortcut.explicit_port = Some(args.next()?),
                        "flash" if args.peek().is_none() => shortcut.action = Action::FlashMonitor,
                        "app-flash" if args.peek().is_none() => shortcut.action = Action::AppFlashMonitor,
                        _ => return None,
                    }
                }
            }
            ["pio" | "platformio", "device", "monitor"] if self.profile == Profile::PlatformIo => {}
            ["arduino-cli", "monitor"] if self.profile == Profile::Arduino => {}
            _ => return None,
        }
        Some(shortcut)
    }

    pub(in crate::shell) fn shortcut_command(&self, syntax: Syntax, shortcut: &Shortcut<'_>, port: &str) -> Result<Option<Task>, String> {
        if port.is_empty() {
            return Err(t("请先在「设备」中选择串口").into());
        }
        if shortcut.explicit_port.is_some_and(|p| if cfg!(windows) { !p.eq_ignore_ascii_case(port) } else { p != port }) {
            return Err(tf("命令里的串口与当前设备 {} 不一致", &[&port]));
        }
        if shortcut.action == Action::Monitor {
            return Ok(None);
        }
        let steps = self.plan_with_build(shortcut.action, port, shortcut.build.unwrap_or_else(|| self.options.build_dir()))?;
        syntax.render(&self.root, &steps).map(|cmd| {
            Some(Task {
                name: shortcut.action.label().into(),
                cmd,
            })
        })
    }
}

fn shortcut_words(cmd: &str) -> Option<Vec<&str>> {
    static WORD: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    let token = WORD.get_or_init(|| regex_lite::Regex::new(r#"'[^'\r\n]*'|"[^"\r\n]*"|[A-Za-z0-9_./:\\-]+"#).unwrap());
    let mut end = 0;
    let mut words = Vec::new();
    for word in token.find_iter(cmd) {
        if !cmd[end..word.start()].chars().all(char::is_whitespace) || end == word.start() && end != 0 {
            return None;
        }
        words.push(word.as_str().trim_matches(['\'', '"']));
        end = word.end();
    }
    cmd[end..].trim().is_empty().then_some(words)
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    program: PathBuf,
    args: Vec<String>,
}

impl Session {
    pub(in crate::shell) fn actions(&self) -> &'static [Action] {
        use Action::*;
        match self.profile {
            Profile::Auto | Profile::System => &[],
            Profile::EspIdf => &[Build, Flash, Monitor, Clean, AppFlashMonitor, Configure, Size, Version],
            Profile::Arduino => &[Build, Flash, Monitor, Version],
            Profile::Stm32 if !self.make && !self.cmake => &[Build, Flash, Monitor, Version],
            _ => &[Build, Flash, Monitor, Clean, Version],
        }
    }

    pub(in crate::shell) fn command(&self, syntax: Syntax, action: Action, port: &str) -> Result<String, String> {
        let steps = self.plan(action, port)?;
        syntax.render(&self.root, &steps)
    }

    fn invoke(&self, tool: &'static str, args: &[&str]) -> Result<Invocation, String> {
        let program = self.tools.get(tool).ok_or_else(|| tf("找不到工具 {}。请安装后重新检测，或选择工具链目录。", &[&tool]))?;
        Ok(Invocation {
            program: program.clone(),
            args: args.iter().map(|a| (*a).into()).collect(),
        })
    }

    fn plan(&self, action: Action, port: &str) -> Result<Vec<Invocation>, String> {
        self.plan_with_build(action, port, self.options.build_dir())
    }

    fn plan_with_build(&self, action: Action, port: &str, build: &str) -> Result<Vec<Invocation>, String> {
        use Action::*;
        let port = port.trim();
        let needs_port = || {
            if port.is_empty() || port.starts_with('-') {
                Err(t("请先在「设备」中选择串口").to_owned())
            } else {
                Ok(port)
            }
        };
        if action == Monitor {
            return Err(t("请在调试页查看串口日志").into());
        }
        let board = self.options.board.trim();
        let preset = self.options.preset.trim();
        let target = self.options.flash_target.trim();
        let nv = |s: &str, error: &'static str| if s.is_empty() || s.starts_with('-') { Err(error.to_owned()) } else { Ok(()) };
        let mut steps = Vec::new();

        let main = match self.profile {
            Profile::EspIdf => {
                let esp = self.esp.as_ref().ok_or_else(|| self.issue.clone().unwrap_or_default())?;
                let script = esp.script.to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned())?;
                let mut args = vec![script];
                if action == Version {
                    args.push("--version");
                } else {
                    args.extend(["-B", build]);
                    if matches!(action, Flash | AppFlash | FlashMonitor | AppFlashMonitor) {
                        args.extend(["-p", needs_port()?]);
                    }
                    args.extend(match action {
                        Build => &["build"][..],
                        Flash | FlashMonitor => &["flash"],
                        Clean => &["clean"],
                        AppFlash | AppFlashMonitor => &["app-flash"],
                        Configure => &["menuconfig"],
                        Size => &["size"],
                        Version | Monitor => unreachable!(),
                    });
                }
                Invocation {
                    program: esp.python.clone(),
                    args: args.into_iter().map(str::to_owned).collect(),
                }
            }
            Profile::PlatformIo => {
                let mut args = match action {
                    Build => vec!["run"],
                    Flash => vec!["run", "-t", "upload"],
                    Clean => vec!["run", "-t", "clean"],
                    Version => vec!["--version"],
                    _ => unreachable!(),
                };
                if !board.is_empty() && action != Version {
                    args.extend(["-e", board]);
                }
                if action == Flash && !self.options.pio_project_upload && !port.is_empty() {
                    args.extend(["--upload-port", port]);
                }
                self.invoke("pio", &args)?
            }
            Profile::Zephyr => {
                let mut args = match action {
                    Build => vec!["build", "-d", build],
                    Flash => vec!["flash", "-d", build],
                    Clean => vec!["build", "-d", build, "-t", "clean"],
                    Version => vec!["--version"],
                    _ => unreachable!(),
                };
                if action == Build && !board.is_empty() {
                    args.extend(["-b", board]);
                } else if action == Build && !self.root.join(build).join("CMakeCache.txt").is_file() {
                    return Err(t("首次编译前请填写开发板名称").into());
                }
                self.invoke("west", &args)?
            }
            Profile::Arduino => {
                let sketch = self.sketch.as_deref().and_then(Path::to_str).ok_or_else(|| t("工程目录缺少 Arduino 草图").to_owned())?;
                match action {
                    Version => self.invoke("arduino-cli", &["version"])?,
                    Build => {
                        nv(board, t("请填写 Arduino 开发板的 FQBN"))?;
                        self.invoke("arduino-cli", &["compile", "--fqbn", board, "--output-dir", build, sketch])?
                    }
                    Flash => {
                        nv(board, t("请填写 Arduino 开发板的 FQBN"))?;
                        self.invoke("arduino-cli", &["upload", "--fqbn", board, "--port", needs_port()?, "--input-dir", build, sketch])?
                    }
                    _ => unreachable!(),
                }
            }
            Profile::Stm32 if action == Flash && target.is_empty() => {
                let file = self.firmware(&["elf", "hex", "bin", "srec", "s19"])?;
                let conn = format!("port={}", if self.options.stm32_serial { needs_port()? } else { "SWD" });
                let mut args = vec!["-c", &conn, "-w", file];
                if Path::new(file).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("bin")) {
                    let addr = self.options.address.trim();
                    if !addr.strip_prefix("0x").is_some_and(|hex| u32::from_str_radix(hex, 16).is_ok()) {
                        return Err(t("烧录 STM32 BIN 需要十六进制起始地址，例如 0x08000000").into());
                    }
                    args.push(addr);
                }
                args.extend(["-v", "-rst"]);
                self.invoke("STM32_Programmer_CLI", &args)?
            }
            Profile::Wch if action == Flash && target.is_empty() => {
                let artifact = if self.options.firmware.trim().is_empty() {
                    self.wch_firmware()?
                } else {
                    self.firmware(&["elf", "hex", "bin"])?.to_owned()
                };
                let file = artifact.as_str();
                match self.options.wch_programmer {
                    WchProgrammer::Link => {
                        let mut args = vec!["flash"];
                        let address = self.options.address.trim();
                        if !address.is_empty() && Path::new(file).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("bin")) {
                            if !address.strip_prefix("0x").is_some_and(|hex| u32::from_str_radix(hex, 16).is_ok()) {
                                return Err(t("请输入十六进制起始地址").into());
                            }
                            args.extend(["--address", address]);
                        }
                        args.push(file);
                        self.invoke("wlink", &args)?
                    }
                    WchProgrammer::UsbIsp => self.invoke("wchisp", &["flash", file])?,
                    WchProgrammer::SerialIsp => self.invoke("wchisp", &["--serial", "--port", needs_port()?, "flash", file])?,
                }
            }
            Profile::Pico if action == Flash && target.is_empty() => {
                let file = self.firmware(&["uf2"])?;
                self.invoke("picotool", &["load", "-v", "-x", file])?
            }
            Profile::Stm32 if !self.make && !self.cmake => {
                let ide = self
                    .cube_ide
                    .as_ref()
                    .ok_or_else(|| t("请用 STM32CubeMX 生成 Makefile 或 CMake 工程，或打开 STM32CubeIDE 工程。").to_owned())?;
                let workspace = ide.workspace.path().to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned())?;
                let project = self.root.to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned())?;
                let cfg = self.options.configuration.trim();
                let cfg = if cfg.is_empty() { "Debug" } else { cfg };
                // CDT interprets -build as a regular expression over project/configuration names.
                let selection = format!("{}/{}", regex_lite::escape(&ide.project), regex_lite::escape(cfg));
                let args = match action {
                    Build => vec![
                        "-nosplash",
                        "-application",
                        "org.eclipse.cdt.managedbuilder.core.headlessbuild",
                        "-data",
                        workspace,
                        "-import",
                        project,
                        "-build",
                        &selection,
                    ],
                    Version => vec!["--launcher.suppressErrors", "-version"],
                    Flash => return Err(t("请在 MCU 工具链中选择 CubeIDE 生成的固件文件").into()),
                    _ => unreachable!(),
                };
                self.invoke("STM32CubeIDE", &args)?
            }
            Profile::Make | Profile::Stm32 | Profile::Wch if self.profile == Profile::Make || !self.cmake || (self.make || self.wch_make_dir.is_some()) && preset.is_empty() => {
                let mut args = Vec::new();
                if !self.make && self.profile == Profile::Wch && action != Version {
                    let dir = self
                        .wch_make_dir
                        .as_deref()
                        .and_then(Path::to_str)
                        .ok_or_else(|| t("请先在 MounRiver Studio 生成工程的 Makefile，或提供 Make/CMake 工程。").to_owned())?;
                    args.extend(["-C", dir]);
                }
                let target_args = match action {
                    Build => vec!["-j8"],
                    Flash => vec![if target.is_empty() { "flash" } else { target }],
                    Clean => vec!["clean"],
                    Version => vec!["--version"],
                    _ => unreachable!(),
                };
                if action == Flash {
                    nv(target_args[0], t("请输入有效的烧录目标"))?;
                }
                args.extend(target_args);
                self.invoke("make", &args)?
            }
            Profile::CMake | Profile::Pico | Profile::Stm32 | Profile::Wch => {
                let mut args = if preset.is_empty() { vec!["--build", build] } else { vec!["--build", "--preset", preset] };
                match action {
                    Build => {
                        let mut cfg = if preset.is_empty() { vec!["-S", ".", "-B", build] } else { vec!["--preset", preset] };
                        if preset.is_empty()
                            && matches!(self.profile, Profile::Stm32 | Profile::Pico | Profile::Wch)
                            && self.tools.contains_key("ninja")
                            && !self.root.join(build).join("CMakeCache.txt").is_file()
                        {
                            cfg.extend(["-G", "Ninja"]);
                        }
                        let toolchain;
                        if let Some(file) = self.cube_toolchain.as_deref().and_then(Path::to_str).filter(|_| preset.is_empty()) {
                            toolchain = format!("-DCMAKE_TOOLCHAIN_FILE={file}");
                            cfg.push(&toolchain);
                        }
                        let pico_board;
                        if self.profile == Profile::Pico && !board.is_empty() {
                            pico_board = format!("-DPICO_BOARD={board}");
                            cfg.push(&pico_board);
                        }
                        steps.push(self.invoke("cmake", &cfg)?);
                    }
                    Flash => {
                        nv(target, t("请填写工程定义的烧录目标"))?;
                        args.extend(["--target", target]);
                    }
                    Clean => args.extend(["--target", "clean"]),
                    Version => args = vec!["--version"],
                    _ => unreachable!(),
                }
                self.invoke("cmake", &args)?
            }
            _ => return Err(t("当前工程没有可用的 MCU 命令").into()),
        };
        steps.push(main);
        Ok(steps)
    }

    fn wch_firmware(&self) -> Result<String, String> {
        let ambiguous = || tf("请选择已编译的固件文件（{}）", &[&"elf / hex / bin"]);
        let dirs = [self.wch_make_dir.as_deref().unwrap_or(Path::new("obj")), Path::new(self.options.build_dir())];
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(self.root.join(dir)) else { continue };
            let images: Vec<_> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_file()).collect();
            for ext in ["elf", "hex", "bin"] {
                let mut matching = images.iter().filter(|file| file.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)));
                if let Some(image) = matching.next() {
                    return if matching.next().is_none() {
                        image.to_str().map(str::to_owned).ok_or_else(ambiguous)
                    } else {
                        Err(ambiguous())
                    };
                }
            }
        }
        Err(ambiguous())
    }

    fn firmware(&self, extensions: &[&str]) -> Result<&str, String> {
        let file = self.options.firmware.trim();
        let path = Path::new(file);
        if file.starts_with('-') || !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e))) || !self.root.join(path).is_file() {
            return Err(tf("请选择已编译的固件文件（{}）", &[&extensions.join(" / ")]));
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn native_flash_commands_use_project_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("stm32.ioc"), "").unwrap();
        fs::write(root.join("Makefile"), "").unwrap();
        fs::write(root.join("App image.bin"), "").unwrap();
        let mut stm = Session::discover(
            root.to_owned(),
            Options {
                firmware: "App image.bin".into(),
                ..Default::default()
            },
        );
        stm.tools.insert("STM32_Programmer_CLI", root.join("STM32_Programmer_CLI"));
        assert_eq!(stm.profile, Profile::Stm32);
        assert!(stm.plan(Action::Flash, "").is_err());
        stm.options.address = "0x08004000".into();
        assert_eq!(stm.plan(Action::Flash, "").unwrap()[0].args, ["-c", "port=SWD", "-w", "App image.bin", "0x08004000", "-v", "-rst"]);

        fs::write(root.join("platformio.ini"), "[env:firmware]").unwrap();
        let mut pio = Session::discover(
            root.to_owned(),
            Options {
                board: "firmware".into(),
                ..Default::default()
            },
        );
        pio.tools.insert("pio", root.join("platformio"));
        assert_eq!(pio.profile, Profile::PlatformIo);
        assert_eq!(pio.plan(Action::Flash, "COM30").unwrap()[0].args, ["run", "-t", "upload", "-e", "firmware", "--upload-port", "COM30"]);
        assert_eq!(pio.plan(Action::Flash, "COM31").unwrap()[0].args.last().unwrap(), "COM31");
        assert!(pio.plan(Action::Monitor, "COM30").is_err());
    }

    #[test]
    fn mounriver_uses_generated_makefile_and_selected_isp_port() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("obj")).unwrap();
        fs::write(root.join("board.wvproj"), "").unwrap();
        fs::write(root.join("obj/makefile"), "").unwrap();
        let image = root.join("obj/board.elf");
        fs::write(&image, "").unwrap();
        let mut s = Session::discover(root.to_owned(), Options::default());
        for tool in ["make", "wlink", "wchisp"] {
            s.tools.insert(tool, root.join(tool));
        }
        assert_eq!(s.profile, Profile::Wch);
        assert_eq!(s.plan(Action::Build, "").unwrap()[0].args, ["-C", "obj", "-j8"]);
        assert_eq!(s.plan(Action::Flash, "COM31").unwrap()[0].args, ["flash", image.to_str().unwrap()]);
        s.options.wch_programmer = WchProgrammer::SerialIsp;
        assert!(s.plan(Action::Flash, "").is_err());
        assert_eq!(s.plan(Action::Flash, "COM31").unwrap()[0].args, ["--serial", "--port", "COM31", "flash", image.to_str().unwrap()]);
    }

    #[test]
    fn pico_cmake_presets_and_uf2() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("CMakeLists.txt"), "include(pico_sdk_import.cmake)\npico_sdk_init()\n").unwrap();
        fs::write(root.join("firmware.uf2"), "").unwrap();
        let mut pico = Session::discover(
            root.to_owned(),
            Options {
                preset: "debug".into(),
                board: "pico2".into(),
                firmware: "firmware.uf2".into(),
                ..Default::default()
            },
        );
        pico.tools.insert("cmake", root.join("cmake"));
        pico.tools.insert("picotool", root.join("picotool"));
        assert_eq!(pico.profile, Profile::Pico);
        let steps = pico.plan(Action::Build, "").unwrap();
        assert_eq!(steps[0].args, ["--preset", "debug", "-DPICO_BOARD=pico2"]);
        assert_eq!(steps[1].args, ["--build", "--preset", "debug"]);
        assert_eq!(pico.plan(Action::Flash, "").unwrap()[0].args, ["load", "-v", "-x", "firmware.uf2"]);
    }

    #[test]
    fn shell_metacharacters_stay_in_arguments() {
        let root = Path::new("/tmp/lab & tools");
        let steps = [Invocation {
            program: root.join("python"),
            args: vec!["firmware's $build&clean".into()],
        }];
        let powershell = Syntax::PowerShell.render(root, &steps).unwrap();
        assert!(powershell.contains("'firmware''s $build&clean'"), "{powershell}");
        let posix = Syntax::Posix.render(root, &steps).unwrap();
        assert!(posix.contains("'firmware'\\''s $build&clean'"), "{posix}");
        assert!(Syntax::Cmd.quote("%PATH%").is_err());
        assert!(Syntax::Posix.quote("build\nflash").is_err());
    }
}

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
                let prefix = match self {
                    Self::PowerShell if step.console != NativeConsole::Direct => "Invoke-QccomWchTool ",
                    Self::PowerShell => "& ",
                    Self::Cmd if step.console == NativeConsole::Gui => r#"start "" /b /wait "#,
                    _ => "",
                };
                Ok(format!("{prefix}{program} {args}").trim_end().to_owned())
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(match self {
            Self::PowerShell => {
                let mut chain = String::new();
                for (cmd, step) in commands.into_iter().zip(steps).rev() {
                    let success = if step.console != NativeConsole::Direct { "$LASTEXITCODE -eq 0" } else { "$?" };
                    chain = if chain.is_empty() { cmd } else { format!("{cmd}; if ({success}) {{ {chain} }}") };
                }
                format!("Push-Location -LiteralPath {cwd}; if ($?) {{ try {{ {chain} }} finally {{ Pop-Location }} }}")
            }
            Self::Cmd => format!("pushd {cwd} && ({} & popd)", commands.join(" && ")),
            Self::Posix => format!("(cd {cwd} && {})", commands.join(" && ")),
            Self::Fish => format!("begin; set -l qccom_task_cwd $PWD; cd {cwd}; and {}; cd $qccom_task_cwd; end", commands.join("; and ")),
        })
    }
}

pub(super) const POWERSHELL_ENV: &str = r#"
chcp 65001 > $null;
[Console]::InputEncoding = [Console]::OutputEncoding = $OutputEncoding = New-Object System.Text.UTF8Encoding $false;
if ($env:QCCOM_MCU_TOOL_DIRS -and $env:PATH -ne $env:QCCOM_MCU_TOOL_DIRS) {
    $env:PATH = $env:QCCOM_MCU_TOOL_DIRS + [IO.Path]::PathSeparator + $env:PATH;
}
"#;

pub(super) const POWERSHELL_ESP: &str = r#"
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

// Legacy MounRiver Make can report a short write on the UTF-8 Windows console.
// A native pipeline gives Make and its recursive children normal pipe handles.
pub(super) const POWERSHELL_WCH_OUTPUT: &str = r#"
function global:Invoke-QccomWchTool {
    $program = $args[0];
    $toolArgs = @($args | Select-Object -Skip 1);
    $global:LASTEXITCODE = 1;
    & $program @toolArgs 2>&1 | ForEach-Object { $_.ToString() };
}
"#;

pub(super) const POWERSHELL_MAKE: &str = r#"
function global:make {
    $projectArgs = @();
    $makeArgs = @($args);
    $buildOnly = !($args -cnotmatch '^(-j[0-9]*|--jobs(=[0-9]+)?|[0-9]+)$');
    if ($PWD.ProviderPath -eq $env:QCCOM_MCU_PROJECT -and
        !(Test-Path -LiteralPath 'Makefile','makefile','GNUmakefile' -PathType Leaf | Where-Object { $_ }) -and
        !($args -cmatch '^(-C|-f|--directory|--file|--makefile)')) {
        if ($env:QCCOM_MCU_MAKE_DIR -and
            (@('GNUmakefile','makefile','Makefile') | Where-Object {
                Test-Path -LiteralPath (Join-Path $env:QCCOM_MCU_MAKE_DIR $_) -PathType Leaf
            })) {
            $projectArgs = @('-C', $env:QCCOM_MCU_MAKE_DIR);
            if ($buildOnly) { $makeArgs += 'all'; }
        } elseif ($env:QCCOM_MRS_IDE -and !($args -cnotmatch '^(-j[0-9]*|--jobs(=[0-9]+)?|[0-9]+|all|clean)$')) {
            if ($args -ccontains 'clean') { return }
            Invoke-QccomWchTool $env:QCCOM_MRS_IDE -nosplash --launcher.suppressErrors -application org.eclipse.cdt.managedbuilder.core.headlessbuild -data $env:QCCOM_MRS_WORKSPACE -import $env:QCCOM_MCU_PROJECT -build $env:QCCOM_MRS_SELECTION;
            return;
        } elseif ($env:QCCOM_MCU_NO_MAKEFILE -and !($args -ccontains '--version')) {
            Write-Error $env:QCCOM_MCU_NO_MAKEFILE;
            return;
        }
    }
    if ($env:QCCOM_REAL_MAKE) {
        if (Test-Path Function:Invoke-QccomWchTool) {
            Invoke-QccomWchTool $env:QCCOM_REAL_MAKE @projectArgs @makeArgs;
        } else {
            & $env:QCCOM_REAL_MAKE @projectArgs @makeArgs;
        }
    } else {
        Write-Error 'GNU Make was not found.';
    }
}
"#;

pub(super) const POWERSHELL_PIO: &str = "function global:pio { & $env:QCCOM_REAL_PIO @args }";

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
            ["wch-flash"] if self.profile == Profile::Wch => shortcut.action = Action::Flash,
            ["wch-monitor"] if self.profile == Profile::Wch => {}
            [program, "flash"] if self.profile == Profile::Wch && !self.make && program.eq_ignore_ascii_case("make") => shortcut.action = Action::Flash,
            ["pio" | "platformio", "device", "monitor"] if self.profile == Profile::PlatformIo => {}
            ["arduino-cli", "monitor"] if self.profile == Profile::Arduino => {}
            _ => return None,
        }
        Some(shortcut)
    }

    pub(in crate::shell) fn shortcut_command(&self, syntax: Syntax, shortcut: &Shortcut<'_>, port: &str) -> Result<Option<Task>, String> {
        if port.is_empty() && (shortcut.action == Action::Monitor || shortcut.action.monitor_after() || self.profile == Profile::EspIdf) {
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
enum NativeConsole {
    Direct,
    Pipe,
    Gui,
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    program: PathBuf,
    args: Vec<String>,
    console: NativeConsole,
}

impl Session {
    pub(in crate::shell) fn actions(&self) -> &'static [Action] {
        use Action::*;
        if self.projects.pending {
            return &[];
        }
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
            console: match (self.profile, tool) {
                (Profile::Wch, "make" | "MounRiver Studio") => NativeConsole::Pipe,
                (Profile::Wch, "WCHISPStudio") => NativeConsole::Gui,
                _ => NativeConsole::Direct,
            },
        })
    }

    fn plan(&self, action: Action, port: &str) -> Result<Vec<Invocation>, String> {
        self.plan_with_build(action, port, self.options.build_dir())
    }

    fn plan_with_build(&self, action: Action, port: &str, build: &str) -> Result<Vec<Invocation>, String> {
        use Action::*;
        if self.projects.pending {
            return Err(t("请在 MCU 工具链中选择沁恒子工程。").into());
        }
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
                    console: NativeConsole::Direct,
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
                let programmer = self.wch_programmer();
                let extensions = if programmer == WchProgrammer::Link { &["elf", "hex", "bin"][..] } else { &["hex", "bin"][..] };
                let artifact = if self.options.firmware.trim().is_empty() {
                    self.wch_firmware(extensions)?
                } else {
                    self.firmware(extensions)?.to_owned()
                };
                let file = artifact.as_str();
                match programmer {
                    WchProgrammer::Auto => unreachable!(),
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
                    WchProgrammer::VendorSerialIsp => self.wch_vendor_flash(file, needs_port()?, true)?,
                    WchProgrammer::VendorUsbIsp => self.wch_vendor_flash(file, self.options.wch_isp_device.trim(), false)?,

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
            Profile::Wch if action == Build && !self.make && !self.cmake && self.wch_generated_make_dir().is_none() => {
                let project = self.wch_project.as_ref().ok_or_else(|| t("工程缺少可用的 Makefile、CMakeLists.txt 或 MounRiver .cproject 构建配置。").to_owned())?;
                let workspace = project.workspace.as_ref().ok_or_else(|| t("首次编译此 .cproject 需要 MounRiver Studio Community，或使用当前 MRS 导出 CMake 工程。").to_owned())?;
                let workspace = workspace.path().to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned())?;
                let root = self.root.to_str().ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned())?;
                self.invoke(
                    "MounRiver Studio",
                    &["-nosplash", "--launcher.suppressErrors", "-application", "org.eclipse.cdt.managedbuilder.core.headlessbuild", "-data", workspace, "-import", root, "-build", &project.selection()],
                )?
            }
            Profile::Make | Profile::Stm32 | Profile::Wch if self.profile == Profile::Make || !self.cmake || (self.make || self.wch_generated_make_dir().is_some()) && preset.is_empty() => {
                let mut args = Vec::new();
                if !self.make && self.profile == Profile::Wch && action != Version {
                    let dir = self
                        .wch_generated_make_dir()
                        .and_then(Path::to_str)
                        .ok_or_else(|| t("请先在 MounRiver Studio 生成工程的 Makefile，或提供 Make/CMake 工程。").to_owned())?;
                    args.extend(["-C", dir]);
                }
                let target_args = match action {
                    Build if self.profile == Profile::Wch && !self.make => vec!["-j8", "all"],
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

    fn wch_vendor_flash(&self, file: &str, port: &str, serial: bool) -> Result<Invocation, String> {
        let tool = self.wch_official_tool().ok_or_else(|| t("未找到官方 ISP 程序。请安装 WCHISPStudio 或在 MCU 工具链中选择命令行程序。").to_owned())?;
        let studio = tool == "WCHISPStudio";
        let config_error = || t("请在 MCU 工具链选择 WCHISPStudio 导出的芯片配置 INI。").to_owned();
        let config = self.wch_isp_config_path().ok_or_else(&config_error)?;
        let settings = wch::IspConfig::read(&config).ok_or_else(config_error)?;
        let path_str = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(|| t("工程或工具路径不是 UTF-8").to_owned());
        if studio {
            if settings.serial.is_some_and(|chosen| chosen != serial) {
                return Err(t("烧录接口与 WCHISPStudio 配置不一致，请导入对应接口的 INI。").into());
            }
            let config = path_str(&config)?;
            let file = path_str(&self.root.join(file))?;
            let mut args = vec!["-c", config.as_str(), "-o", "download", "-f", file.as_str()];
            let port = port.to_ascii_uppercase();
            if serial {
                if !port.strip_prefix("COM").and_then(|n| n.parse::<u16>().ok()).is_some_and(|n| n != 0) {
                    return Err(t("请先在「设备」中选择串口").into());
                }
                args.extend(["-p", port.as_str(), "-b", "115200"]);
            }
            return self.invoke(tool, &args);
        }

        if port.is_empty() || port.starts_with('-') {
            return Err(t("请输入 WCH ISP 的 USB 设备标识").into());
        }
        // The standalone tool also accepts a config relative to the working directory.
        let relative = config.strip_prefix(&self.root).ok().filter(|path| !path.to_string_lossy().starts_with('-'));
        let config = path_str(relative.unwrap_or(&config))?;
        let mut args = vec!["-p", port];
        if serial {
            args.extend(["-b", "115200"]);
        }
        args.extend(["-c", &config, "-o", "program", "-f", file]);
        self.invoke(tool, &args)
    }

    fn wch_firmware(&self, extensions: &[&str]) -> Result<String, String> {
        let ambiguous = || tf("请选择已编译的固件文件（{}）", &[&extensions.join(" / ")]);
        let dirs = [self.wch_output_dir().unwrap_or(Path::new("obj")), Path::new(self.options.build_dir()), Path::new(".")];
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(self.root.join(dir)) else { continue };
            let images: Vec<_> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_file()).collect();
            for ext in extensions {
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
        let image = root.join("obj").join("board.elf");
        let hex = root.join("obj").join("board.hex");
        fs::write(&image, "").unwrap();
        fs::write(&hex, "").unwrap();
        let mut s = Session::discover(root.to_owned(), Options { wch_programmer: WchProgrammer::Link, ..Options::default() });
        for tool in ["make", "wlink", "wchisp", "WCHISPTool_CMD"] {
            s.tools.insert(tool, root.join(tool));
        }
        assert_eq!(s.profile, Profile::Wch);
        assert_eq!(s.plan(Action::Build, "").unwrap()[0].args, ["-C", "obj", "-j8", "all"]);
        assert_eq!(s.plan(Action::Flash, "COM31").unwrap()[0].args, ["flash", image.to_str().unwrap()]);
        s.options.wch_programmer = WchProgrammer::SerialIsp;
        assert!(s.plan(Action::Flash, "").is_err());
        assert_eq!(s.plan(Action::Flash, "COM31").unwrap()[0].args, ["--serial", "--port", "COM31", "flash", hex.to_str().unwrap()]);
        s.options.wch_programmer = WchProgrammer::VendorSerialIsp;
        fs::write(root.join("Config.ini"), "[Public]\nMCUName=CH582\n").unwrap();
        assert!(s.plan(Action::Flash, "").is_err());
        assert_eq!(s.plan(Action::Flash, "COM31").unwrap()[0].args, ["-p", "COM31", "-b", "115200", "-c", "Config.ini", "-o", "program", "-f", hex.to_str().unwrap()]);
        let shortcut = s.shortcut("wch-flash").unwrap();
        assert!(s.shortcut_command(Syntax::PowerShell, &shortcut, "COM31").unwrap().unwrap().cmd.contains("-p COM31"));
    }

    #[test]
    fn wch_auto_prefers_studio_and_keeps_explicit_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("board.wvproj"), "").unwrap();
        let hex = root.join("翻盖柜 firmware.hex");
        fs::write(&hex, "").unwrap();
        let config = root.join("board.ini");
        fs::write(&config, "[Public]\nMCUName=CH582\n[CH57x-59xUICfg]\nbDnInterType=1\n").unwrap();
        let mut session = Session::discover(
            root.to_owned(),
            Options { wch_isp_config: "board.ini".into(), ..Default::default() },
        );
        let studio = root.join("WCHISPTool_CH57x-59x.exe");
        session.tools.insert("WCHISPStudio", studio.clone());
        session.tools.insert("wlink", root.join("wlink"));
        let flash = session.plan(Action::Flash, "com31").unwrap().pop().unwrap();
        let paths = ["-c", config.to_str().unwrap(), "-o", "download", "-f", flash.args[5].as_str()];
        assert_eq!(flash.program, studio);
        assert_eq!(Path::new(&flash.args[5]), hex);
        let powershell = session.command(Syntax::PowerShell, Action::Flash, "COM31").unwrap();
        assert!(powershell.contains("Invoke-QccomWchTool") && powershell.contains("-p COM31 -b 115200"));
        let cmd = session.command(Syntax::Cmd, Action::Flash, "COM31").unwrap();
        assert!(cmd.contains(r#"start "" /b /wait "#));
        assert_eq!(&flash.args[..6], &paths);
        assert_eq!(&flash.args[6..], ["-p", "COM31", "-b", "115200"]);
        assert!(session.plan(Action::Flash, "").is_err());

        session.options.wch_programmer = WchProgrammer::VendorUsbIsp;
        assert!(session.plan(Action::Flash, "").is_err());
        fs::write(&config, "[Public]\nMCUName=CH582\n[CH57x-59xUICfg]\nbDnInterType=0\n").unwrap();
        assert_eq!(session.plan(Action::Flash, "").unwrap()[0].args, paths);
        session.options.wch_programmer = WchProgrammer::Auto;
        assert_eq!(session.plan(Action::Flash, "COM31").unwrap()[0].args, paths);
        fs::write(config, "[Unrelated]\nMCUName=CH582\n").unwrap();
        assert!(session.plan(Action::Flash, "").is_err());
        session.options.wch_programmer = WchProgrammer::Link;
        assert_eq!(session.plan(Action::Flash, "").unwrap()[0].program, root.join("wlink"));
    }

    #[test]
    fn saved_legacy_wch_link_default_migrates_to_auto() {
        let options: Options = serde_json::from_str(r#"{"wch_programmer":"Link"}"#).unwrap();
        assert_eq!(options.wch_programmer, WchProgrammer::Auto);
        let options: Options = serde_json::from_str(r#"{"wch_programmer":"WchLink"}"#).unwrap();
        assert_eq!(options.wch_programmer, WchProgrammer::Link);
        let options: Options = serde_json::from_str(r#"{"wch_programmer":"VendorUsbIsp"}"#).unwrap();
        assert_eq!(options.wch_programmer, WchProgrammer::VendorUsbIsp);
    }

    #[test]
    fn wch_uses_cdt_on_first_build_then_the_selected_generated_makefile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("board.wvproj"), "").unwrap();
        fs::write(root.join(".project"), "<projectDescription><name>Board (test)</name></projectDescription>").unwrap();
        fs::write(
            root.join(".cproject"),
            r#"<cproject><configuration name="Debug" artifactName="board"><builder buildPath="${workspace_loc:/Board (test)}/obj-debug"/></configuration>
               <configuration name="Release" artifactName="board"><builder buildPath="${workspace_loc:/Board (test)}/obj-release"/></configuration></cproject>"#,
        )
        .unwrap();
        let mut session = Session::discover(
            root.to_owned(),
            Options { configuration: "Release".into(), ..Default::default() },
        );
        session.wch_project.as_mut().unwrap().workspace = Some(tempfile::tempdir().unwrap());
        let ide = root.join("MounRiver Studio_Community");
        session.tools.insert("MounRiver Studio", ide.clone());
        session.tools.insert("make", root.join("make"));
        let plan = session.plan(Action::Build, "").unwrap();
        assert_eq!(plan[0].program, ide);
        assert_eq!(&plan[0].args[plan[0].args.len() - 2..], ["-build", r"Board \(test\)/Release"]);
        fs::create_dir(root.join("obj-release")).unwrap();
        fs::write(root.join("obj-release/Makefile"), "all:").unwrap();
        assert_eq!(session.plan(Action::Build, "").unwrap()[0].args, ["-C", "obj-release", "-j8", "all"]);
    }

    #[test]
    fn discovers_mrs_bundled_make_and_gcc_for_plain_powershell() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("CH582M_LOCK");
        let sdk = dir.path().join("MounRiver Studio 3");
        let platform = if cfg!(windows) { "win32" } else { "linux" };
        let resources = sdk.join("resources/app/resources").join(platform);
        let make_bin = resources.join("others/Build_Tools/Make/bin");
        let gcc_bin = resources.join("components/WCH/Toolchain/RISC-V Embedded GCC15/bin");
        let fake_exe = |dir: &Path, name: &str| {
            fs::create_dir_all(dir).unwrap();
            let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            fs::write(&path, "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            path
        };
        let make = fake_exe(&make_bin, "make");
        let gcc = fake_exe(&gcc_bin, "riscv32-wch-elf-gcc");
        fs::create_dir_all(root.join("obj")).unwrap();
        fs::write(root.join("obj/makefile"), "").unwrap();
        let session = Session::discover(root, Options { sdk, ..Default::default() });
        assert_eq!(session.profile, Profile::Wch);
        assert_eq!(session.tools.get("make"), Some(&make));
        assert_eq!(session.tools.get("riscv32-wch-elf-gcc"), Some(&gcc));
        // Both tools can live below arbitrary versioned folders inside the install.
        let nested = dir.path().join("portable/releases/pack-7/host");
        let nested_make = fake_exe(&nested.join("utilities/make/v4.3/host/bin"), "make");
        let nested_gcc = fake_exe(&nested.join("compiler/embedded/15/riscv/bin"), "riscv32-wch-elf-gcc");
        let wrapped = Session::discover(dir.path().join("CH582M_LOCK"), Options { sdk: dir.path().join("portable"), ..Default::default() });
        assert_eq!(wrapped.tools.get("make"), Some(&nested_make));
        assert_eq!(wrapped.tools.get("riscv32-wch-elf-gcc"), Some(&nested_gcc));
        let mut builder = CommandBuilder::new("powershell");
        session.apply(&mut builder, Syntax::PowerShell);
        assert_eq!(builder.get_env("QCCOM_REAL_MAKE"), Some(make.as_os_str()));
        assert_eq!(builder.get_env("QCCOM_MCU_MAKE_DIR"), Some(Path::new("obj").as_os_str()));
        let path = builder.get_env("PATH").unwrap();
        assert!(std::env::split_paths(path).any(|p| p == gcc_bin));
        let script = builder.get_argv().last().unwrap().to_string_lossy();
        assert!(script.contains("function global:make"));
        assert!(script.contains("function global:Invoke-QccomWchTool"));
        let build = session.command(Syntax::PowerShell, Action::Build, "").unwrap();
        let clean = session.command(Syntax::PowerShell, Action::Clean, "").unwrap();
        assert!(build.contains("Invoke-QccomWchTool") && build.contains(" -C obj -j8 all"));
        assert!(clean.contains("Invoke-QccomWchTool") && clean.contains(" -C obj clean"));
        assert!(!session.command(Syntax::Posix, Action::Build, "").unwrap().contains("Invoke-QccomWchTool"));
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
            console: NativeConsole::Direct,
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

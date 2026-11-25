//! Detect a local MCU project and keep its toolchain scoped to one terminal.
mod commands;
mod discover;
mod wch;

use super::*;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::OsString;

pub(super) use commands::{Action, Syntax};
use discover::EspIdf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Profile {
    #[default]
    Auto,
    System,
    EspIdf,
    Stm32,
    Wch,
    PlatformIo,
    Zephyr,
    Pico,
    Arduino,
    CMake,
    Make,
}

impl Profile {
    pub(super) const ALL: &[Self] = &[
        Self::Auto,
        Self::System,
        Self::EspIdf,
        Self::Stm32,
        Self::Wch,
        Self::PlatformIo,
        Self::Zephyr,
        Self::Pico,
        Self::Arduino,
        Self::CMake,
        Self::Make,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Auto => t("自动识别工程"),
            Self::System => t("系统环境"),
            Self::EspIdf => "ESP-IDF",
            Self::Stm32 => "STM32",
            Self::Wch => t("沁恒微 / WCH"),
            Self::PlatformIo => "PlatformIO",
            Self::Zephyr => "Zephyr / west",
            Self::Pico => "Raspberry Pi Pico SDK",
            Self::Arduino => "Arduino CLI",
            Self::CMake => "CMake",
            Self::Make => "Make",
        }
    }

    pub(super) fn status_label(self) -> Cow<'static, str> {
        match self {
            Self::Auto | Self::System | Self::EspIdf => self.label().into(),
            _ => tf("{}（未测试）", &[&self.label()]).into(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Options {
    pub profile: Profile,
    pub sdk: PathBuf,
    pub build_dir: String,
    pub board: String,
    pub preset: String,
    pub configuration: String,
    pub firmware: String,
    pub address: String,
    pub flash_target: String,
    pub stm32_serial: bool,
    pub wch_programmer: WchProgrammer,
    pub wch_isp_config: String,
    pub wch_isp_device: String,
    pub pio_project_upload: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum WchProgrammer {
    #[default]
    Link,
    UsbIsp,
    SerialIsp,
    VendorSerialIsp,
    VendorUsbIsp,
}

impl Options {
    fn build_dir(&self) -> &str {
        if self.build_dir.trim().is_empty() {
            "build"
        } else {
            self.build_dir.trim()
        }
    }
}

pub(super) struct Session {
    pub profile: Profile,
    pub options: Options,
    pub root: PathBuf,
    pub location: Option<PathBuf>,
    pub issue: Option<String>,
    env: BTreeMap<String, OsString>,
    tools: BTreeMap<&'static str, PathBuf>,
    esp: Option<EspIdf>,
    make: bool,
    pub(super) cmake: bool,
    cube_toolchain: Option<PathBuf>,
    cube_ide: Option<CubeIde>,
    wch_make_dir: Option<PathBuf>,
    wch_project: Option<wch::Project>,
    sketch: Option<PathBuf>,
}

struct CubeIde {
    project: String,
    workspace: tempfile::TempDir,
}

impl Session {
    pub(super) fn tool_paths(&self) -> &BTreeMap<&'static str, PathBuf> {
        &self.tools
    }
    pub(super) fn discover(root: PathBuf, options: Options) -> Self {
        discover::session(root, options)
    }

    fn wch_output_dir(&self) -> Option<&Path> {
        self.wch_project.as_ref().map(|project| project.dir.as_path()).or(self.wch_make_dir.as_deref())
    }

    fn wch_generated_make_dir(&self) -> Option<&Path> {
        self.wch_output_dir().filter(|dir| wch::has_makefile(&self.root.join(dir)))
    }

    pub(super) fn apply(&self, cmd: &mut CommandBuilder, syntax: Syntax) {
        for (key, val) in &self.env {
            cmd.env(key, val);
        }
        if let Some(esp) = &self.esp {
            cmd.env("QCCOM_IDF_PYTHON", &esp.python);
            cmd.env("QCCOM_IDF_SCRIPT", &esp.script);
            cmd.env("QCCOM_MCU_PROJECT", &self.root);
            cmd.env("QCCOM_MCU_BUILD_DIR", self.options.build_dir());
            cmd.env_remove("QCCOM_MCU_PORT");
            if let Some(make) = self.tools.get("make") {
                cmd.env("QCCOM_REAL_MAKE", make);
            } else {
                cmd.env_remove("QCCOM_REAL_MAKE");
            }
            #[cfg(unix)]
            if let Some(shims) = esp.shims.as_ref() {
                let old = cmd.get_env("PATH").unwrap_or_default();
                if let Ok(path) = std::env::join_paths(std::iter::once(shims.path().to_owned()).chain(std::env::split_paths(old))) {
                    cmd.env("PATH", path);
                }
            }
            if syntax == Syntax::Cmd {
                cmd.args(["/K", r#"chcp 65001 >nul & doskey idf.py="%QCCOM_IDF_PYTHON%" "%QCCOM_IDF_SCRIPT%" $*"#]);
            }
        }
        if self.profile == Profile::Wch && !self.make && !self.cmake {
            cmd.env("QCCOM_MCU_PROJECT", &self.root);
            if let Some(make) = self.tools.get("make") {
                cmd.env("QCCOM_REAL_MAKE", make);
            }
            if let Some(dir) = self.wch_output_dir() {
                cmd.env("QCCOM_MCU_MAKE_DIR", dir);
            }
            if let Some(project) = &self.wch_project {
                if let (Some(ide), Some(workspace)) = (self.tools.get("MounRiver Studio"), project.workspace.as_ref()) {
                    cmd.env("QCCOM_MRS_IDE", ide);
                    cmd.env("QCCOM_MRS_WORKSPACE", workspace.path());
                    cmd.env("QCCOM_MRS_SELECTION", project.selection());
                }
                #[cfg(unix)]
                if let (Some(shims), Some(_)) = (project.shims.as_ref(), self.tools.get("make")) {
                    let old = cmd.get_env("PATH").unwrap_or_default();
                    if let Ok(path) = std::env::join_paths(std::iter::once(shims.path().to_owned()).chain(std::env::split_paths(old))) {
                        cmd.env("PATH", path);
                    }
                }
            }
        }
        if syntax == Syntax::PowerShell && self.starts_shell(syntax) {
            let mut script = String::from(commands::POWERSHELL_ENV);
            if self.has_esp() {
                script.push_str(commands::POWERSHELL_ESP);
            } else if self.profile == Profile::Wch && !self.make && !self.cmake {
                cmd.env("QCCOM_MCU_NO_MAKEFILE", t("未找到工程的 Makefile。请安装 MounRiver Studio Community，或从 MRS 导出 Makefile/CMake 工程。"));
                script.push_str(commands::POWERSHELL_MAKE);
            } else if let Some(make) = self.tools.get("make") {
                cmd.env("QCCOM_REAL_MAKE", make);
                cmd.env("QCCOM_MCU_PROJECT", &self.root);
                script.push_str(commands::POWERSHELL_MAKE);
            }
            if let Some(pio) = self.tools.get("pio") {
                cmd.env("QCCOM_REAL_PIO", pio);
                script.push_str(commands::POWERSHELL_PIO);
            }
            cmd.arg("-NoExit");
            cmd.arg("-Command");
            cmd.arg(script);
        }
    }

    pub(super) fn starts_shell(&self, syntax: Syntax) -> bool {
        match syntax {
            Syntax::PowerShell => !self.actions().is_empty(),
            Syntax::Cmd => self.has_esp(),
            _ => false,
        }
    }

    pub(super) fn has_esp(&self) -> bool {
        self.esp.is_some()
    }

    pub(super) fn tools_summary(&self) -> String {
        self.tools.keys().copied().collect::<Vec<_>>().join(", ")
    }
}

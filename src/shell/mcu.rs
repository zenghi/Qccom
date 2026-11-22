//! Detect a local MCU project and keep its toolchain scoped to one terminal.
mod commands;
mod discover;

use super::*;
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

    pub(super) fn label(self) -> &'static str {
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
    pub pio_project_upload: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum WchProgrammer {
    #[default]
    Link,
    UsbIsp,
    SerialIsp,
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
    sketch: Option<PathBuf>,
}

struct CubeIde {
    project: String,
    workspace: tempfile::TempDir,
}

impl Session {
    pub(super) fn discover(root: PathBuf, options: Options) -> Self {
        discover::session(root, options)
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
            match syntax {
                Syntax::PowerShell => cmd.args(["-NoExit", "-Command", commands::POWERSHELL_INIT]),
                Syntax::Cmd => cmd.args(["/K", r#"chcp 65001 >nul & doskey idf.py="%QCCOM_IDF_PYTHON%" "%QCCOM_IDF_SCRIPT%" $*"#]),
                _ => {}
            }
        }
    }

    pub(super) fn has_esp(&self) -> bool {
        self.esp.is_some()
    }

    pub(super) fn tools_summary(&self) -> String {
        self.tools.keys().copied().collect::<Vec<_>>().join(", ")
    }
}

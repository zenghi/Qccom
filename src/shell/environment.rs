//! Named local toolchains. Read variables when starting a PTY, so editing the file
//! takes effect on the next launch and secrets are never copied into settings.
use super::*;
use std::ffi::OsString;

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    pub name: String,
    pub path: PathBuf,
}

impl Environment {
    pub(super) fn apply(&self, cmd: &mut CommandBuilder) -> Result<(), String> {
        if self.path.is_dir() {
            return prepend(cmd, &self.path);
        }
        let input = std::fs::read_to_string(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let base = self.path.parent().unwrap_or(Path::new("."));
        for (i, line) in input.trim_start_matches('\u{feff}').lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let invalid = || tf("环境文件第 {} 行格式有误", &[&(i + 1)]);
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            let key = key.trim();
            if key.is_empty() || !key.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric()) || key.as_bytes()[0].is_ascii_digit() {
                return Err(invalid());
            }
            let raw = value.trim();
            let value = if raw.starts_with(['"', '\'']) {
                raw.get(1..).and_then(|s| s.strip_suffix(&raw[..1])).ok_or_else(invalid)?
            } else {
                raw
            };
            if value.contains('\0') {
                return Err(invalid());
            }
            if key == "PATH_PREPEND" {
                let dir = base.join(value);
                if value.is_empty() || !dir.is_dir() {
                    return Err(format!("{}: {}", invalid(), dir.display()));
                }
                prepend(cmd, &dir)?;
            } else {
                cmd.env(key, value);
            }
        }
        Ok(())
    }
}

fn prepend(cmd: &mut CommandBuilder, path: &Path) -> Result<(), String> {
    let old = cmd.get_env("PATH").map(OsString::from).unwrap_or_default();
    let paths = std::iter::once(path.to_owned()).chain(std::env::split_paths(&old));
    cmd.env("PATH", std::env::join_paths(paths).map_err(|e| e.to_string())?);
    Ok(())
}

#[derive(Default)]
pub(super) struct EnvDialog {
    pub open: bool,
    project: Option<(PathBuf, mcu::Options)>,
    draft: Environment,
    error: String,
}

impl Shells {
    pub(super) fn environment_dialog(&mut self, ctx: &egui::Context, cwd: &Path) {
        let mut open = self.env_dialog.open;
        if !open {
            return;
        }
        if self.env_dialog.project.as_ref().is_none_or(|(p, _)| p != cwd) {
            self.env_dialog.project = Some((cwd.to_owned(), self.s.mcu.get(cwd).cloned().unwrap_or_default()));
        }
        let mut launch = None;
        let mut apply = false;
        egui::Window::new(t("MCU 工具链"))
            .id(egui::Id::new("local-toolchains"))
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(cwd.display().to_string());
                let found = self.tabs.get(self.active).filter(|sh| sh.cwd == cwd).and_then(|sh| sh.mcu.as_deref());
                let (_, project) = self.env_dialog.project.as_mut().unwrap();
                let detected = found.map_or(mcu::Profile::Auto, |m| m.profile);
                egui::ComboBox::from_label(t("工程类型")).selected_text(project.profile.label()).show_ui(ui, |ui| {
                    for &profile in mcu::Profile::ALL {
                        ui.selectable_value(&mut project.profile, profile, profile.label());
                    }
                });
                let profile = if project.profile == mcu::Profile::Auto { detected } else { project.profile };
                if profile != mcu::Profile::System || project.profile == mcu::Profile::Auto {
                    ui.horizontal(|ui| {
                        ui.label(t("SDK / 工具链目录"));
                        let mut path = project.sdk.to_string_lossy().into_owned();
                        if ui.add(egui::TextEdit::singleline(&mut path).hint_text(t("留空自动搜索"))).changed() {
                            project.sdk = PathBuf::from(path);
                        }
                        if ui.small_button(t("浏览…")).clicked() {
                            if let Some(p) = crate::diagnostics::file_dialog().pick_folder() {
                                project.sdk = p;
                            }
                        }
                    });
                    project_fields(ui, project, profile, found.is_some_and(|s| s.cmake), cwd);
                }
                if self.pending_shell.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("检测工具链…"));
                    });
                } else if let Some(session) = found {
                    ui.label(tf("当前终端：{}", &[&session.profile.label()]));
                    if let Some(dir) = &session.location {
                        ui.label(dir.display().to_string());
                    }
                    let tools = session.tools_summary();
                    if !tools.is_empty() {
                        ui.label(tf("已发现：{}", &[&tools]));
                    }
                    if let Some(issue) = &session.issue {
                        ui.colored_label(ERR(), issue);
                    }
                }
                apply = ui.add_enabled(self.pending_shell.is_none(), egui::Button::new(t("检测并打开新终端"))).clicked();
                ui.separator();
                ui.collapsing(t("手动导入环境"), |ui| self.manual_environment(ui, &mut launch));
            });
        if apply {
            let (_, options) = self.env_dialog.project.as_ref().unwrap();
            let system = options.profile == mcu::Profile::System;
            if *options == mcu::Options::default() {
                self.s.mcu.remove(cwd);
            } else {
                self.s.mcu.insert(cwd.to_owned(), options.clone());
            }
            let old = std::mem::take(&mut self.s.environment);
            let count = self.tabs.len();
            let kind = if system {
                None
            } else {
                let preferred = self.tabs.get(self.active).map_or_else(|| self.s.default.clone(), |sh| sh.kind.clone());
                let mut local = self.kinds().iter().filter(|kind| Syntax::for_shell(kind).is_some());
                local.clone().find(|kind| kind.name == preferred).or_else(|| local.next()).cloned()
            };
            self.new_shell(ctx, kind, cwd);
            if self.pending_shell.is_none() && self.tabs.len() == count {
                self.s.environment = old;
            }
        }
        self.env_dialog.open = open;
        if !open {
            self.env_dialog.project = None;
        }
        if let Some(name) = launch {
            let old = std::mem::replace(&mut self.s.environment, name);
            let count = self.tabs.len();
            self.new_shell(ctx, None, cwd);
            if self.tabs.len() > count {
                self.env_dialog.open = false;
                self.env_dialog.project = None;
            } else {
                self.s.environment = old;
            }
        }
    }

    fn manual_environment(&mut self, ui: &mut egui::Ui, launch: &mut Option<String>) {
        ui.label(t("选择环境后打开新终端，编译、烧录、清理在该终端运行。"));
        let mut remove = None;
        for (i, env) in self.s.environments.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.selectable_label(env.name == self.s.environment, &env.name).on_hover_text(env.path.display().to_string()).clicked() {
                    *launch = Some(env.name.clone());
                }
                if ui.small_button(t("移除")).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            if self.s.environments.remove(i).name == self.s.environment {
                self.s.environment.clear();
            }
        }
        ui.separator();
        let draft = &mut self.env_dialog.draft;
        ui.horizontal(|ui| {
            if ui.button(t("选择工具链目录")).clicked() {
                if let Some(p) = crate::diagnostics::file_dialog().pick_folder() {
                    draft.path = p;
                }
            }
            if ui.button(t("导入环境文件")).clicked() {
                if let Some(p) = crate::diagnostics::file_dialog().pick_file() {
                    draft.path = p;
                }
            }
        });
        ui.label(t("环境文件每行写 KEY=VALUE；PATH_PREPEND=目录 会保留原有 PATH。"));
        let mut path = draft.path.to_string_lossy().into_owned();
        ui.horizontal(|ui| {
            ui.label(t("路径"));
            if ui.text_edit_singleline(&mut path).changed() {
                draft.path = PathBuf::from(&path);
            }
        });
        ui.horizontal(|ui| {
            ui.label(t("名称"));
            ui.text_edit_singleline(&mut draft.name);
        });
        if ui
            .add_enabled(!draft.name.trim().is_empty() && !draft.path.as_os_str().is_empty(), egui::Button::new(t("保存并启用")))
            .clicked()
        {
            draft.name = draft.name.trim().to_owned();
            if self.s.environments.iter().any(|e| e.name == draft.name) {
                self.env_dialog.error = t("环境名称已存在").into();
            } else {
                match draft.apply(&mut CommandBuilder::new("env")) {
                    Ok(()) => {
                        *launch = Some(draft.name.clone());
                        self.s.environments.push(std::mem::take(draft));
                        self.env_dialog.error.clear();
                    }
                    Err(e) => self.env_dialog.error = e,
                }
            }
        }
        if !self.env_dialog.error.is_empty() {
            ui.colored_label(ERR(), &self.env_dialog.error);
        }
    }
}

fn project_fields(ui: &mut egui::Ui, project: &mut mcu::Options, profile: mcu::Profile, cmake: bool, cwd: &Path) {
    use mcu::Profile::*;
    let field = |ui: &mut egui::Ui, label, value: &mut String, hint| {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(value).hint_text(hint));
        });
    };
    if profile == Stm32 {
        ui.horizontal(|ui| {
            ui.label(t("烧录接口"));
            ui.selectable_value(&mut project.stm32_serial, false, "SWD");
            ui.selectable_value(&mut project.stm32_serial, true, t("设备串口"));
        });
    }
    if profile == Wch {
        use mcu::WchProgrammer::*;
        ui.horizontal(|ui| {
            ui.label(t("烧录接口"));
            ui.selectable_value(&mut project.wch_programmer, Link, "WCH-Link");
            ui.selectable_value(&mut project.wch_programmer, UsbIsp, "USB ISP");
            ui.selectable_value(&mut project.wch_programmer, SerialIsp, t("串口 ISP"));
        });
    }
    if matches!(profile, EspIdf | Zephyr | Pico | Arduino | CMake | Wch) || profile == Stm32 && cmake {
        field(ui, t("构建目录"), &mut project.build_dir, "build");
    }
    match profile {
        PlatformIo => {
            field(ui, "PlatformIO env", &mut project.board, t("使用 platformio.ini 的默认环境"));
            ui.checkbox(&mut project.pio_project_upload, t("烧录接口由 platformio.ini 指定"));
        }
        Arduino => field(ui, "FQBN", &mut project.board, "arduino:avr:uno"),
        Zephyr => field(ui, t("开发板"), &mut project.board, "nrf52840dk/nrf52840"),
        Pico => field(ui, t("开发板"), &mut project.board, "pico / pico2"),
        _ => {}
    }
    if matches!(profile, CMake | Pico) || matches!(profile, Stm32 | Wch) && cmake {
        field(ui, "CMake preset", &mut project.preset, "Debug");
    }
    if profile == Stm32 && !cmake {
        field(ui, t("CubeIDE 构建配置"), &mut project.configuration, "Debug");
    }
    if matches!(profile, Stm32 | Pico | Wch) {
        ui.horizontal(|ui| {
            ui.label(t("固件文件"));
            ui.add(egui::TextEdit::singleline(&mut project.firmware).hint_text(if profile == Pico { ".uf2" } else { ".elf / .hex / .bin" }));
            if ui.small_button(t("浏览…")).clicked() {
                if let Some(path) = crate::diagnostics::file_dialog().set_directory(cwd).pick_file() {
                    project.firmware = path.strip_prefix(cwd).unwrap_or(&path).to_string_lossy().into_owned();
                }
            }
        });
        if matches!(profile, Stm32 | Wch) {
            field(ui, t("BIN 起始地址"), &mut project.address, "0x08000000");
        }
    }
    if matches!(profile, Stm32 | Pico | Wch | CMake | Make) {
        field(ui, t("烧录目标"), &mut project.flash_target, if profile == Make { "flash" } else { t("可选的工程目标") });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_literal_values_and_prepends_paths() {
        let dir = std::env::temp_dir().join(format!("qccom-toolchain-{}", std::process::id()));
        let bin = dir.join("tool bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = dir.join("board.env");
        std::fs::write(&path, "\u{feff}# board\r\nSDK=\u{22}a=b c\u{22}\r\nPATH_PREPEND=tool bin\r\n").unwrap();
        let env = Environment { name: "Board".into(), path };
        let mut cmd = CommandBuilder::new("test");
        env.apply(&mut cmd).unwrap();
        assert_eq!(cmd.get_env("SDK").unwrap(), "a=b c");
        assert_eq!(std::env::split_paths(cmd.get_env("PATH").unwrap()).next(), Some(bin));
        std::fs::write(&env.path, "bad line").unwrap();
        assert!(env.apply(&mut cmd).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

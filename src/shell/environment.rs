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
    draft: Environment,
    error: String,
}

impl Shells {
    pub(super) fn environment_dialog(&mut self, ctx: &egui::Context, cwd: &Path) {
        let mut open = self.env_dialog.open;
        if !open {
            return;
        }
        let mut launch = None;
        egui::Window::new(t("终端环境"))
            .id(egui::Id::new("local-toolchains"))
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(t("选择环境后打开新终端，编译、烧录、清理在该终端运行。"));
                if ui.selectable_label(self.s.environment.is_empty(), t("系统环境")).clicked() {
                    launch = Some(String::new());
                }
                let mut remove = None;
                for (i, env) in self.s.environments.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(env.name == self.s.environment, &env.name).on_hover_text(env.path.display().to_string()).clicked() {
                            launch = Some(env.name.clone());
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
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            draft.path = p;
                        }
                    }
                    if ui.button(t("导入环境文件")).clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_file() {
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
                                launch = Some(draft.name.clone());
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
            });
        self.env_dialog.open = open;
        if let Some(name) = launch {
            let old = std::mem::replace(&mut self.s.environment, name);
            let count = self.tabs.len();
            self.new_shell(ctx, None, cwd);
            if self.tabs.len() > count {
                self.env_dialog.open = false;
            } else {
                self.s.environment = old;
            }
        }
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

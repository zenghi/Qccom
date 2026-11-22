//! Working-copy inspection uses the installed SVN client. Write actions open TortoiseSVN.

use super::*;
use std::ffi::{OsStr, OsString};
use std::process::Command;

#[derive(Default)]
pub(super) struct WorkingCopy {
    pub status: Option<Result<Status, String>>,
    pub loading: bool,
    conflicts_only: bool,
    #[cfg(windows)]
    external_focus: Option<bool>,
}

pub(super) struct Status {
    revision: u64,
    url: String,
    relative_url: String,
    changes: Vec<Change>,
}

#[derive(Clone)]
struct Change {
    path: String,
    mark: char,
    has_base: bool,
    replacement_base: Option<u64>,
    removed: bool,
}

/// A nested Git checkout takes precedence over an enclosing SVN working copy.
pub(super) fn find_root(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors() {
        if dir.join(".git").exists() {
            return None;
        }
        if dir.join(".svn").is_dir() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

#[cfg(windows)]
fn installed(name: &str) -> Option<PathBuf> {
    let defaults = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|dir| PathBuf::from(dir).join("TortoiseSVN").join("bin"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(defaults)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|file| file.is_file())
}

fn svn(root: &Path, args: &[&str], target: &OsStr) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let program = installed("svn.exe").ok_or_else(|| t("需要安装 SVN 命令行工具才能查看状态和比较"))?;
        let mut command = Command::new(program);
        command.creation_flags(0x0800_0000);
        command
    };
    #[cfg(not(windows))]
    let mut command = Command::new("svn");

    let out = command
        .current_dir(root)
        .args(args)
        .args(["--non-interactive", "--"])
        .arg(target)
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => t("需要安装 SVN 命令行工具才能查看状态和比较"),
            _ => error.to_string(),
        })?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let detail = String::from_utf8_lossy(&out.stderr);
        Err(if detail.trim().is_empty() {
            format!("svn {} ({})", args[0], out.status)
        } else {
            detail.trim().to_owned()
        })
    }
}

fn parse_status(info: &[u8], status: &[u8]) -> Result<Status, String> {
    fn xml(bytes: &[u8]) -> Result<roxmltree::Document<'_>, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        roxmltree::Document::parse(text).map_err(|error| error.to_string())
    }

    let info = xml(info)?;
    let entry = info.root_element().children().find(|node| node.has_tag_name("entry")).ok_or_else(|| t("无法读取 SVN 修订号"))?;
    let revision = entry.attribute("revision").and_then(|value| value.parse().ok()).ok_or_else(|| t("无法读取 SVN 修订号"))?;
    let text = |tag| entry.children().find(|node| node.has_tag_name(tag)).and_then(|node| node.text()).unwrap_or_default();
    let status = xml(status)?;
    let mut changes: Vec<_> = status
        .descendants()
        .filter(|node| node.has_tag_name("entry"))
        .filter_map(|entry| {
            let path = entry.attribute("path")?;
            if path.is_empty() || !Path::new(path).components().all(|part| matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
                return None;
            }
            let wc = entry.children().find(|node| node.has_tag_name("wc-status"))?;
            let item = wc.attribute("item")?;
            let props = wc.attribute("props");
            let conflict = item == "conflicted" || props == Some("conflicted") || wc.attribute("tree-conflicted") == Some("true");
            let mark = if conflict {
                '!'
            } else {
                match item {
                    "added" => 'A',
                    "unversioned" => '?',
                    "deleted" | "missing" => 'D',
                    "modified" | "obstructed" | "incomplete" => 'M',
                    "replaced" => 'R',
                    "normal" if props == Some("modified") => 'P',
                    _ => return None,
                }
            };
            Some(Change {
                path: path.to_owned(),
                mark,
                has_base: !matches!(item, "added" | "unversioned"),
                replacement_base: wc.attribute("revision").filter(|_| item == "replaced").and_then(|value| value.parse().ok()),
                removed: matches!(item, "deleted" | "missing"),
            })
        })
        .collect();
    changes.sort_by(|a, b| (a.mark != '!', &a.path).cmp(&(b.mark != '!', &b.path)));
    Ok(Status {
        revision,
        url: text("url").into(),
        relative_url: text("relative-url").into(),
        changes,
    })
}

fn status(root: &Path) -> Result<Status, String> {
    let target = OsStr::new(".");
    let info = svn(root, &["info", "--xml"], target)?;
    let changes = svn(root, &["status", "--xml", "--ignore-externals"], target)?;
    parse_status(&info, &changes)
}

fn base_comparison(root: &Path, change: &Change) -> Result<GitEvt, String> {
    let rel = Path::new(&change.path);
    let current = root.join(rel);
    if current.is_dir() || !current.is_file() && !change.removed {
        return Err(t("请选择工作副本中的文件进行比较"));
    }
    let name = current.file_name().unwrap_or_default().to_string_lossy();
    let mut target = OsString::from(rel);
    target.push("@"); // An empty peg revision preserves literal '@' in the filename.
    let revision = change.replacement_base.map(|value| value.to_string());
    // SVN does not expose a local pristine file for replacements. Fetch its recorded base revision.
    if let Some(revision) = &revision {
        target.push(revision);
    }
    let data = if change.has_base {
        svn(root, &["cat", "-r", revision.as_deref().unwrap_or("BASE")], &target)?
    } else {
        Vec::new()
    };
    let base = crate::compareview::temp_file(&name, &data);
    let left_label = format!("{name} (BASE)");
    let (right, right_label) = if current.is_file() {
        (current, None)
    } else {
        (crate::compareview::temp_file(&name, &[]), Some(format!("{name} ({})", t("文件已删除"))))
    };
    Ok(GitEvt::Compare(base, Some(left_label), right, right_label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_nearest_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let firmware = root.join("firmware");
        std::fs::create_dir(root.join(".svn")).unwrap();
        std::fs::create_dir(&firmware).unwrap();
        assert_eq!(find_root(&firmware).as_deref(), Some(root));
        std::fs::write(firmware.join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(find_root(&firmware), None);
    }

    #[test]
    fn parses_properties_conflicts_and_file_changes_from_svn_xml() {
        let info = br#"<info><entry revision="42"><url>file:///src/trunk</url><relative-url>^/trunk</relative-url></entry></info>"#;
        let status = r#"<status><target path=".">
          <entry path="escaped &amp; 串口@2.c"><wc-status item="modified" props="none"/></entry>
          <entry path="props"><wc-status item="normal" props="modified"/></entry>
          <entry path="conflict"><wc-status item="normal" props="none" tree-conflicted="true"/></entry>
          <entry path="fresh.c"><wc-status item="added" props="none"/></entry>
          <entry path="gone.c"><wc-status item="missing" props="none"/></entry>
          <entry path="replaced.c"><wc-status item="replaced" props="none" revision="41"/></entry>
          <entry path="../ancestor"><wc-status item="modified" props="none"/></entry>
        </target></status>"#;
        let result = parse_status(info, status.as_bytes()).unwrap();
        assert_eq!(result.revision, 42);
        assert_eq!(result.relative_url, "^/trunk");
        let changes: Vec<_> = result
            .changes
            .iter()
            .map(|change| (change.path.as_str(), change.mark, change.has_base, change.removed, change.replacement_base))
            .collect();
        assert_eq!(
            changes,
            [
                ("conflict", '!', true, false, None),
                ("escaped & 串口@2.c", 'M', true, false, None),
                ("fresh.c", 'A', false, false, None),
                ("gone.c", 'D', true, true, None),
                ("props", 'P', true, false, None),
                ("replaced.c", 'R', true, false, Some(41)),
            ]
        );
    }
}

#[cfg(windows)]
fn launch_tortoise(program: &Path, action: &str, path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // TortoiseProc reads the raw command line, with the quote after '/path:'.
    let mut path_arg = OsString::from(r#"/path:""#);
    path_arg.push(path);
    path_arg.push("\"");
    Command::new(program)
        .arg(format!("/command:{action}"))
        .raw_arg(path_arg)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

impl GitView {
    pub(super) fn load_svn(&mut self) {
        if let Some(wc) = self.svn.as_mut().filter(|wc| !wc.loading) {
            wc.loading = true;
            self.spawn(|root| Msg::SvnStatus(status(&root)));
        }
    }

    pub(super) fn svn_ui(&mut self, ui: &mut Ui) {
        let Some(root) = self.root.clone() else { return };
        let Some(wc) = self.svn.as_mut() else { return };
        let mut refresh = false;
        #[cfg(windows)]
        if let Some(blurred) = &mut wc.external_focus {
            if !ui.input(|input| input.focused) {
                *blurred = true;
            } else if *blurred {
                wc.external_focus = None;
                refresh = true;
            }
        }
        let mut pick = false;
        let mut compare = None;
        #[cfg(windows)]
        let mut external = None;
        let project = root.file_name().unwrap_or(root.as_os_str()).to_string_lossy();
        let report = wc.status.as_ref().and_then(|status| status.as_ref().ok());
        egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("SVN").size(16.0).strong().color(ACCENT()));
                ui.add(egui::Label::new(RichText::new(project.as_ref()).strong()).truncate()).on_hover_text(root.to_string_lossy());
                pick = icon_button(ui, Icon::Folder, &t("打开其他仓库"), false).clicked();
                refresh |= icon_button(ui, Icon::Refresh, &t("刷新"), false).clicked();
                #[cfg(windows)]
                ui.menu_button("TortoiseSVN", |ui| {
                    for (label, action) in [("日志", "log"), ("更新工作副本", "update"), ("提交", "commit")] {
                        if ui.button(t(label)).clicked() {
                            external = Some((action, root.clone()));
                            ui.close_menu();
                        }
                    }
                });
                if wc.loading {
                    ui.spinner();
                }
            });
            if let Some(report) = report {
                ui.horizontal_wrapped(|ui| {
                    let revision = format!("r{}", report.revision);
                    ui.label(RichText::new(&revision).monospace().color(ACCENT()));
                    ui.add(egui::Label::new(&report.relative_url).truncate()).on_hover_text(&report.url);
                    if ui.button(t("复制版本信息")).clicked() {
                        ui.ctx()
                            .copy_text(format!("SVN · {project} · {revision} · {} · {}", report.relative_url, tf("{} 个改动", &[&report.changes.len()])));
                    }
                });
            }
        });
        ui.separator();
        if let Some(error) = &self.err {
            ui.label(RichText::new(error).color(WARN()));
        }
        match &wc.status {
            Some(Ok(status)) if status.changes.is_empty() => {
                ui.label(t("工作区很干净，没有未提交的更改"));
            }
            Some(Ok(status)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("工作区更改")).strong());
                    ui.label(status.changes.len().to_string());
                    ui.checkbox(&mut wc.conflicts_only, t("只看冲突"));
                });
                let rows: Vec<_> = status.changes.iter().filter(|f| !wc.conflicts_only || f.mark == '!').collect();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 24.0, rows.len(), |ui, range| {
                    for f in &rows[range] {
                        let name = f.path.replace('\\', "/");
                        let label = match f.mark {
                            'P' => t("仅属性改动"),
                            '!' => t("合并冲突"),
                            _ => t("双击对比 SVN BASE"),
                        };
                        let row = file_row(ui, f.mark, &name, None, None, false).on_hover_text(format!("{name}\n{label}"));
                        if row.double_clicked() {
                            compare = Some((*f).clone());
                        }
                        row.context_menu(|ui| {
                            if ui.button(t("在比较页中比较")).clicked() {
                                compare = Some((*f).clone());
                                ui.close_menu();
                            }
                            #[cfg(windows)]
                            if ui.button(format!("TortoiseSVN · {}", t("日志"))).clicked() {
                                external = Some(("log", root.join(&f.path)));
                                ui.close_menu();
                            }
                        });
                    }
                });
            }
            Some(Err(error)) => {
                ui.label(RichText::new(error).color(WARN()));
            }
            None if !wc.loading => {
                ui.label(t("加载中…"));
            }
            None => {}
        }

        #[cfg(windows)]
        if let Some((action, path)) = external {
            let result = installed("TortoiseProc.exe")
                .ok_or_else(|| t("未找到 TortoiseSVN"))
                .and_then(|program| launch_tortoise(&program, action, &path));
            match result {
                Ok(()) => wc.external_focus = Some(false),
                Err(error) => self.toast(error, WARN()),
            }
        }
        if refresh {
            self.load_svn();
        }
        if pick {
            self.pick_folder();
        } else if let Some(change) = compare {
            self.spawn(move |root| Msg::SvnCompare(base_comparison(&root, &change)));
        }
    }
}

//! Remote files within an SSH session's workspace.

use crate::askpass::AuthRequest;
use crate::remote::Host;
use crate::ssh_files::{FileClient, FileCommand, FileEntry, FileEvent, FileKind, ScpTransfer};
use std::sync::mpsc::TryRecvError;

use super::*;

enum Operation {
    Connect,
    List,
    Upload { name: String, directory: String },
    Download(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileProtocol {
    Sftp,
    Scp,
}

enum ScpStatus {
    Done,
    Cancelled,
    Failed(String),
}

impl Operation {
    fn transfer_name(&self) -> Option<&str> {
        match self {
            Self::Upload { name, .. } | Self::Download(name) => Some(name),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct FileBrowser {
    pub visible: bool,
    pub rect: Option<Rect>,
    client: Option<FileClient>,
    operation: Option<Operation>,
    authenticated: bool,
    directory: Option<String>,
    path_input: String,
    entries: Vec<FileEntry>,
    selected: Option<usize>,
    progress: Option<(u64, Option<u64>)>,
    cancelling: bool,
    uploads: VecDeque<PathBuf>,
    dropped: Vec<PathBuf>,
    auth: Option<(FileProtocol, AuthRequest)>,
    auth_input: String,
    auth_focus: bool,
    error: Option<String>,
    notice: Option<String>,
    scp: Option<ScpTransfer>,
    scp_status: Option<ScpStatus>,
    scp_download: Option<String>,
}

impl FileBrowser {
    pub fn toggle(&mut self, host: &Host, ctx: &egui::Context) {
        self.visible = !self.visible;
        if self.visible && self.client.is_none() && self.scp.is_none() {
            self.connect(host, ctx);
        } else if !self.visible {
            self.rect = None;
            if matches!(self.operation, Some(Operation::Connect)) {
                self.close();
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.operation.is_some() || self.scp.is_some()
    }

    pub fn stage_upload(&mut self, paths: Vec<PathBuf>) {
        self.dropped.extend(paths);
    }

    fn connect(&mut self, host: &Host, ctx: &egui::Context) {
        self.error = None;
        self.notice = None;
        if self.path_input.is_empty() {
            self.path_input = if host.dir.is_empty() { ".".into() } else { host.dir.clone() };
        }
        match FileClient::open(host, ctx.clone()) {
            Ok(client) => {
                self.client = Some(client);
                self.operation = Some(Operation::Connect);
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        self.poll_scp(ctx);
        for _ in 0..128 {
            let Some(client) = &self.client else { return };
            match client.rx.try_recv() {
                Ok(event) => self.update(event),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.unexpected_close();
                    return;
                }
            }
        }
        ctx.request_repaint();
    }

    fn update(&mut self, event: FileEvent) {
        match event {
            FileEvent::Authentication(request) => self.request_auth(FileProtocol::Sftp, request),
            FileEvent::Ready => {
                self.authenticated = true;
                self.list(self.path_input.clone());
            }
            FileEvent::Entries { path, items } => {
                self.path_input = path.clone();
                self.directory = Some(path);
                self.entries = items;
                self.selected = None;
                self.operation = None;
            }
            FileEvent::Progress { name, done, total } => {
                if self.operation.as_ref().and_then(Operation::transfer_name).is_some_and(|current| current == name) {
                    self.progress = Some((done, total));
                }
            }
            FileEvent::Transferred => {
                self.progress = None;
                let cancelled = std::mem::take(&mut self.cancelling);
                if let Some(operation) = self.operation.take() {
                    if let Some(name) = operation.transfer_name() {
                        self.notice = Some(tf("传输完成：{}", &[&display(name)]));
                    }
                    if let Operation::Upload { directory, .. } = operation {
                        if cancelled || self.uploads.is_empty() {
                            self.list(directory);
                        } else {
                            self.upload_next(directory);
                        }
                    }
                }
            }
            FileEvent::Error(error) => {
                let cancelled = std::mem::take(&mut self.cancelling);
                let refresh = match self.operation.take() {
                    Some(Operation::Upload { directory, .. }) => Some(directory),
                    _ => None,
                };
                self.uploads.clear();
                self.progress = None;
                if cancelled {
                    self.notice = Some(t("传输已取消").into());
                } else {
                    self.error = Some(error);
                }
                if let Some(directory) = refresh {
                    // Other uploads from this batch may have finished before the failure.
                    let previous_error = self.error.take();
                    if self.list(directory) && previous_error.is_some() {
                        self.error = previous_error;
                    }
                }
            }
            FileEvent::Closed => self.unexpected_close(),
        }
    }

    fn unexpected_close(&mut self) {
        self.close();
        self.error.get_or_insert_with(|| t("连接已关闭").into());
    }

    fn close(&mut self) {
        self.cancel_auth(FileProtocol::Sftp);
        self.client = None;
        self.operation = None;
        self.authenticated = false;
        self.progress = None;
        self.cancelling = false;
        self.uploads.clear();
    }

    fn request_auth(&mut self, protocol: FileProtocol, request: AuthRequest) {
        if let Some((_, prior)) = self.auth.replace((protocol, request)) {
            prior.answer(None);
        }
        self.auth_input.clear();
        self.auth_focus = true;
    }

    fn cancel_auth(&mut self, protocol: FileProtocol) {
        if self.auth.as_ref().is_some_and(|(current, _)| *current == protocol) {
            let Some((_, request)) = self.auth.take() else { return };
            request.answer(None);
            self.auth_input.clear();
        }
    }

    fn poll_scp(&mut self, ctx: &egui::Context) {
        for _ in 0..128 {
            let Some(transfer) = &self.scp else { return };
            let status = match transfer.rx.try_recv() {
                Ok(FileEvent::Authentication(request)) => {
                    self.request_auth(FileProtocol::Scp, request);
                    continue;
                }
                Ok(FileEvent::Transferred) => ScpStatus::Done,
                Ok(FileEvent::Error(error)) => ScpStatus::Failed(error),
                Ok(FileEvent::Closed) | Err(TryRecvError::Disconnected) => ScpStatus::Failed(t("连接已关闭").into()),
                Ok(_) => continue,
                Err(TryRecvError::Empty) => return,
            };
            self.finish_scp(status);
            return;
        }
        ctx.request_repaint();
    }

    fn finish_scp(&mut self, status: ScpStatus) {
        self.cancel_auth(FileProtocol::Scp);
        self.scp = None;
        self.scp_status = Some(status);
    }

    fn start_scp(&mut self, start: impl FnOnce() -> Result<ScpTransfer, String>) {
        if matches!(self.operation, Some(Operation::Connect)) {
            self.close();
        }
        self.scp_status = None;
        match start() {
            Ok(transfer) => self.scp = Some(transfer),
            Err(error) => self.scp_status = Some(ScpStatus::Failed(error)),
        }
    }

    fn cancel_scp(&mut self) {
        if let Some(transfer) = &self.scp {
            transfer.cancel();
        }
        self.finish_scp(ScpStatus::Cancelled);
    }

    fn send(&mut self, command: FileCommand, operation: Operation) -> bool {
        let Some(client) = &self.client else { return false };
        match client.send(command) {
            Ok(()) => {
                self.operation = Some(operation);
                self.error = None;
                self.progress = None;
                true
            }
            Err(error) => {
                self.close();
                self.error = Some(error);
                false
            }
        }
    }

    fn list(&mut self, path: String) -> bool {
        self.send(FileCommand::List(path), Operation::List)
    }

    fn current_directory(&self) -> Option<&str> {
        self.directory.as_deref().filter(|path| *path == self.path_input)
    }

    fn start_uploads(&mut self, files: Vec<PathBuf>) {
        let Some(directory) = self.current_directory().map(str::to_owned) else { return };
        if !files.iter().all(|path| path.is_file()) {
            self.error = Some(t("文件必须是普通文件").into());
            return;
        }
        self.notice = None;
        self.uploads = files.into();
        self.upload_next(directory);
    }

    fn upload_next(&mut self, directory: String) {
        if let Some(local) = self.uploads.pop_front() {
            let name = local.file_name().unwrap_or(local.as_os_str()).to_string_lossy().into_owned();
            self.send(FileCommand::Upload { local, directory: directory.clone() }, Operation::Upload { name, directory });
        }
    }

    fn download(&mut self, name: &str) {
        let (Some(directory), true) = (self.current_directory(), is_child_name(name)) else { return };
        let remote = remote_child(directory, name);
        let file_name: String = name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
        if let Some(local) = crate::diagnostics::file_dialog().set_file_name(file_name).save_file() {
            self.notice = None;
            self.send(FileCommand::Download { remote, local }, Operation::Download(name.into()));
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, host: &Host, stacked: bool) {
        self.rect = Some(ui.max_rect());
        if stacked {
            let height = ui.available_height();
            egui::ScrollArea::vertical()
                .id_salt("stacked_remote_files")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.show_content(ui, host, (height * 0.6).max(54.0));
                });
        } else {
            self.show_content(ui, host, f32::INFINITY);
        }
    }

    fn show_content(&mut self, ui: &mut egui::Ui, host: &Host, entries_height: f32) {
        let mut close = false;
        let narrow = ui.available_width() < 205.0;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = icon_button(ui, Icon::Close, t("关闭"), false).clicked();
                if !narrow {
                    self.scp_menu(ui, host);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(t("远程文件")).font(FontId::new(15.0, bold())).color(TEXT())).truncate())
                        .on_hover_text(host.addr());
                });
            });
        });
        if close {
            self.toggle(host, ui.ctx());
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if narrow {
                self.scp_menu(ui, host);
            }
            let (status, color) = if self.authenticated {
                (t("已连接"), OK())
            } else if matches!(self.operation, Some(Operation::Connect)) {
                (t("正在连接…"), ACCENT())
            } else {
                (t("未连接"), TEXT3())
            };
            ui.label(RichText::new(format!("SFTP · {status}")).small().color(color));
            if self.client.is_none() && self.scp.is_none() && ui.small_button(t("重新连接")).clicked() {
                self.connect(host, ui.ctx());
            }
        });
        self.scp_ui(ui, host);
        ui.add_space(8.0);
        if self.auth.is_some() {
            self.auth_ui(ui);
            return;
        }
        let can_list = self.authenticated && !self.busy();
        let path = ui.add_enabled(
            can_list,
            egui::TextEdit::singleline(&mut self.path_input)
                .hint_text(t("远程目录"))
                .desired_width(f32::INFINITY)
                .font(FontId::new(13.0, FontFamily::Monospace)),
        );
        if can_list && path.lost_focus() && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter)) && !self.path_input.is_empty() {
            self.notice = None;
            self.list(self.path_to_open());
        }
        let pending_path = self.current_directory().is_none();
        let can_transfer = can_list && !self.busy() && !pending_path;
        let parent = self.directory.as_deref().and_then(parent_path).map(str::to_owned);
        let downloadable = self
            .selected
            .and_then(|index| self.entries.get(index))
            .filter(|entry| matches!(entry.kind, FileKind::File | FileKind::Link))
            .map(|entry| entry.name.clone());
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(can_list && !self.busy() && pending_path && !self.path_input.is_empty(), egui::Button::new(t("打开")))
                .clicked()
            {
                self.notice = None;
                self.list(self.path_to_open());
            }
            if ui.add_enabled(can_list && parent.is_some(), egui::Button::new(t("上一级"))).clicked() {
                if let Some(parent) = parent {
                    self.notice = None;
                    self.list(parent);
                }
            }
            if ui.add_enabled(can_list, egui::Button::new(t("刷新"))).clicked() {
                self.notice = None;
                self.list(self.directory.clone().unwrap_or_else(|| self.path_input.clone()));
            }
            if ui.add_enabled(can_transfer, egui::Button::new(t("上传文件…"))).clicked() {
                if let Some(files) = crate::diagnostics::file_dialog().pick_files() {
                    self.start_uploads(files);
                }
            }
            if ui.add_enabled(can_transfer && downloadable.is_some(), egui::Button::new(t("下载文件…"))).clicked() {
                if let Some(name) = &downloadable {
                    self.download(name);
                }
            }
        });
        if !self.dropped.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(tf("{} 个文件待上传", &[&self.dropped.len()]));
                if ui.add_enabled(can_transfer, egui::Button::new(t("上传"))).clicked() {
                    let files = std::mem::take(&mut self.dropped);
                    self.start_uploads(files);
                }
                if ui.small_button(t("取消")).clicked() {
                    self.dropped.clear();
                }
            });
        }
        if let Some(error) = &self.error {
            transfer_error(ui, error);
        }
        if let Some(notice) = &self.notice {
            ui.small(notice);
        }
        self.operation_ui(ui, host);
        ui.add_space(7.0);
        self.entries_ui(ui, entries_height);
    }

    fn scp_menu(&mut self, ui: &mut egui::Ui, host: &Host) {
        let available = self.scp.is_none() && (self.operation.is_none() || matches!(self.operation, Some(Operation::Connect)));
        let mut upload = false;
        let mut download = false;
        ui.add_enabled_ui(available, |ui| {
            ui.menu_button("SCP", |ui| {
                if ui.button(t("上传文件…")).clicked() {
                    upload = true;
                    ui.close_menu();
                }
                if ui.button(t("下载文件…")).clicked() {
                    download = true;
                    ui.close_menu();
                }
            });
        });
        if download {
            if matches!(self.operation, Some(Operation::Connect)) {
                self.close();
            }
            self.scp_download = Some(String::new());
        }
        if upload {
            if let Some(paths) = crate::diagnostics::file_dialog().pick_files() {
                self.scp_download = None;
                self.start_scp(|| ScpTransfer::upload(host, paths, ui.ctx().clone()));
            }
        }
    }

    fn scp_ui(&mut self, ui: &mut egui::Ui, host: &Host) {
        if let Some(path) = &mut self.scp_download {
            let edit = ui.add(egui::TextEdit::singleline(path).hint_text(t("远程文件或文件夹路径")).desired_width(f32::INFINITY));
            let enter = edit.lost_focus() && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
            let mut download = false;
            let mut cancel = false;
            ui.horizontal_wrapped(|ui| {
                download = ui.add_enabled(!path.trim().is_empty(), egui::Button::new(t("下载到…"))).clicked();
                cancel = ui.button(t("取消")).clicked();
            });
            if cancel {
                self.scp_download = None;
            } else if (download || enter) && !path.trim().is_empty() {
                if let Some(folder) = crate::diagnostics::file_dialog().pick_folder() {
                    let remote = path.trim().to_owned();
                    self.scp_download = None;
                    self.start_scp(|| ScpTransfer::download(host, remote, folder, ui.ctx().clone()));
                }
            }
        }
        if self.scp.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Spinner::new().size(12.0).color(ACCENT()));
                ui.small(format!("SCP · {}", t("传输中")));
                if ui.small_button(t("取消")).clicked() {
                    self.cancel_scp();
                }
            });
        }
        match &self.scp_status {
            Some(ScpStatus::Done) => {
                ui.small(tf("传输完成：{}", &[&"SCP"]));
            }
            Some(ScpStatus::Cancelled) => {
                ui.small(format!("SCP · {}", t("传输已取消")));
            }
            Some(ScpStatus::Failed(error)) => transfer_error(ui, error),
            None => {}
        }
    }

    fn path_to_open(&self) -> String {
        let input = &self.path_input;
        let drive = input.as_bytes().get(1..3) == Some(b":/") || input.as_bytes().get(1..3) == Some(b":\\");
        if !drive && !input.starts_with(['/', '~']) {
            if let Some(directory) = &self.directory {
                return remote_child(directory, input);
            }
        }
        input.clone()
    }

    fn auth_ui(&mut self, ui: &mut egui::Ui) {
        let Some((protocol, request)) = &self.auth else { return };
        let protocol = *protocol;
        ui.strong(match protocol {
            FileProtocol::Sftp => t("SFTP 身份验证"),
            FileProtocol::Scp => t("SCP 身份验证"),
        });
        let prompt = display(&request.prompt);
        ui.label(&prompt);
        let verify = prompt.contains("yes/no");
        let mut confirm = false;
        if !verify {
            let mut output = egui::TextEdit::singleline(&mut self.auth_input).password(!request.echo).desired_width(f32::INFINITY).show(ui);
            output.state.clear_undoer();
            egui::TextEdit::store_state(ui.ctx(), output.response.id, output.state);
            let edit = output.response;
            if std::mem::take(&mut self.auth_focus) {
                edit.request_focus();
            }
            confirm = edit.lost_focus() && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
        }
        let mut cancel = false;
        ui.horizontal(|ui| {
            confirm |= primary_button(ui, t("确认")).clicked();
            cancel = ui.button(t("取消")).clicked();
        });
        if cancel {
            match protocol {
                FileProtocol::Sftp => self.close(),
                FileProtocol::Scp => self.cancel_scp(),
            }
        } else if confirm {
            if let Some((_, request)) = self.auth.take() {
                let answer = if verify { "yes".into() } else { std::mem::take(&mut self.auth_input) };
                request.answer(Some(answer));
            }
        }
    }

    fn operation_ui(&mut self, ui: &mut egui::Ui, host: &Host) {
        let Some(operation) = &self.operation else { return };
        let message = match operation {
            Operation::Connect => tf("正在连接 {} …", &[&host.addr()]),
            Operation::List => t("正在读取目录").into(),
            Operation::Upload { name, .. } => tf("正在上传：{}", &[&display(name)]),
            Operation::Download(name) => tf("正在下载：{}", &[&display(name)]),
        };
        let transfer = operation.transfer_name().is_some();
        let connecting = matches!(operation, Operation::Connect);
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Spinner::new().size(12.0).color(ACCENT()));
            ui.small(message);
            if connecting && ui.small_button(t("取消")).clicked() {
                self.close();
            } else if transfer && ui.add_enabled(!self.cancelling, egui::Button::new(t("取消")).small()).clicked() {
                self.uploads.clear();
                if let Some(client) = &self.client {
                    match client.send(FileCommand::Cancel) {
                        Ok(()) => self.cancelling = true,
                        Err(error) => {
                            self.close();
                            self.error = Some(error);
                        }
                    }
                }
            }
        });
        if let Some((done, total)) = self.progress {
            if let Some(total) = total.filter(|total| *total > 0) {
                ui.add(egui::ProgressBar::new((done as f64 / total as f64).min(1.0) as f32).text(format!("{} / {}", human_bytes(done), human_bytes(total))));
            } else {
                ui.small(human_bytes(done));
            }
        }
    }

    fn entries_ui(&mut self, ui: &mut egui::Ui, max_height: f32) {
        let width = ui.available_width();
        let kind_width = if width >= 445.0 { 93.0 } else { 0.0 };
        let size_width = if width >= 265.0 { 76.0 } else { 0.0 };
        let spacing = ui.spacing().item_spacing.x;
        let name_width = (width - kind_width - size_width - 2.0 * spacing - 12.0).max(20.0);
        let headers = [(t("名称"), name_width), (t("类型"), kind_width), (t("大小"), size_width)];
        ui.horizontal(|ui| {
            for (label, width) in headers {
                if width > 0.0 {
                    ui.add_sized(vec2(width, 19.0), egui::Label::new(RichText::new(label).small().color(TEXT3())).truncate());
                }
            }
        });
        ui.separator();
        if self.directory.is_some() && self.entries.is_empty() && !self.busy() {
            ui.label(RichText::new(t("目录为空")).color(TEXT3()));
        }
        let interactive = self.authenticated && !self.busy() && self.current_directory().is_some();
        let mut open = None;
        egui::ScrollArea::vertical()
            .id_salt("sftp_entries")
            .max_height(max_height)
            .auto_shrink([false, false])
            .show_rows(ui, 26.0, self.entries.len(), |ui, range| {
                for index in range {
                    let entry = &self.entries[index];
                    ui.horizontal(|ui| {
                        let name = display(&entry.name);
                        let directory = entry.kind == FileKind::Directory;
                        let selected = self.selected == Some(index);
                        let color = if directory { ACCENT() } else { TEXT() };
                        let (icon_rect, _) = ui.allocate_exact_size(vec2(16.0, 26.0), Sense::hover());
                        let icon = if directory { Icon::Folder } else { Icon::Files };
                        draw_icon(ui.painter(), Rect::from_center_size(icon_rect.center(), vec2(14.0, 14.0)), icon, color);
                        let button = egui::Button::new(RichText::new(&name).color(color)).truncate().frame(selected).selected(selected);
                        let row = ui.add_sized(vec2((name_width - 16.0 - spacing).max(10.0), 26.0), button).on_hover_text(name);
                        if row.clicked() {
                            self.selected = Some(index);
                        }
                        if interactive && row.double_clicked() {
                            open = Some(index);
                        }
                        if kind_width > 0.0 {
                            let kind = match entry.kind {
                                FileKind::Directory => t("文件夹"),
                                FileKind::File => t("文件"),
                                FileKind::Link => t("符号链接"),
                                FileKind::Other => t("其他文件"),
                            };
                            ui.add_sized(vec2(kind_width, 26.0), egui::Label::new(RichText::new(kind).small().color(TEXT3())).truncate());
                        }
                        if size_width > 0.0 {
                            let size = entry.size.filter(|_| !directory).map_or_else(|| "—".into(), human_bytes);
                            ui.add_sized(vec2(size_width, 26.0), egui::Label::new(RichText::new(size).monospace().small().color(TEXT3())).truncate());
                        }
                    });
                }
            });
        if let Some(index) = open {
            let entry = &self.entries[index];
            match entry.kind {
                FileKind::Directory => {
                    if let Some(directory) = &self.directory {
                        let path = remote_child(directory, &entry.name);
                        self.notice = None;
                        self.list(path);
                    }
                }
                FileKind::File | FileKind::Link => self.download(&entry.name.clone()),
                FileKind::Other => {}
            }
        }
    }
}

impl Drop for FileBrowser {
    fn drop(&mut self) {
        self.close();
        self.cancel_auth(FileProtocol::Scp);
    }
}

fn transfer_error(ui: &mut egui::Ui, error: &str) {
    egui::ScrollArea::vertical().id_salt(("transfer_error", error)).max_height(100.0).show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(error).color(ERR())).wrap());
    });
    if ui.small_button(t("复制")).clicked() {
        ui.ctx().copy_text(error.to_owned());
    }
}

fn is_child_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

fn parent_path(path: &str) -> Option<&str> {
    let (parent, _) = path.trim_end_matches('/').rsplit_once('/')?;
    Some(if parent.is_empty() {
        "/"
    } else if parent.trim_start_matches('/').len() == 2 && parent.ends_with(':') {
        &path[..parent.len() + 1]
    } else {
        parent
    })
}

fn remote_child(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

fn display(name: &str) -> String {
    name.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edited_paths_do_not_target_the_previous_listing() {
        let mut browser = FileBrowser::default();
        browser.directory = Some("/home/user".into());
        browser.path_input = "/home/user".into();
        assert_eq!(browser.current_directory(), Some("/home/user"));
        browser.path_input = "specs and logs".into();
        assert_eq!(browser.current_directory(), None);
        assert_eq!(browser.path_to_open(), "/home/user/specs and logs");
        browser.path_input = "C:/Users".into();
        assert_eq!(browser.path_to_open(), "C:/Users");
        assert_eq!(parent_path("C:/Users"), Some("C:/"));
    }
}

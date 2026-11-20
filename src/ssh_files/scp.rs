//! Manual copies over the explicit SCP protocol, for hosts without SFTP.
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use super::*;

pub struct ScpTransfer {
    pub rx: mpsc::Receiver<FileEvent>,
    cancel: watch::Sender<()>,
}

enum Copy {
    Upload(Vec<PathBuf>),
    Download { remote: String, folder: PathBuf },
}

impl ScpTransfer {
    pub fn upload(host: &Host, paths: Vec<PathBuf>, ctx: egui::Context) -> Result<Self, String> {
        if paths.is_empty() {
            return Err(t("请选择要上传的文件").into());
        }
        Self::start(host, Copy::Upload(paths), ctx)
    }

    pub fn download(host: &Host, remote: String, folder: PathBuf, ctx: egui::Context) -> Result<Self, String> {
        local_name(&remote).map_err(|error| error.to_string())?;
        Self::start(host, Copy::Download { remote, folder }, ctx)
    }

    pub fn cancel(&self) {
        let _ = self.cancel.send(());
    }

    fn start(host: &Host, copy: Copy, ctx: egui::Context) -> Result<Self, String> {
        if host.proto != Proto::Ssh {
            return Err(t("文件管理需要 SSH 连接").into());
        }
        let ssh = remote::ssh_program().ok_or_else(|| t("未找到 OpenSSH 客户端").to_owned())?;
        let name = if cfg!(windows) { "scp.exe" } else { "scp" };
        let beside_ssh = Path::new(&ssh).with_file_name(name);
        let program = if beside_ssh.is_file() { Some(beside_ssh) } else { crate::shell::which(name) }.ok_or_else(|| t("未找到 OpenSSH 的 SCP 客户端").to_owned())?;
        let host = host.clone();
        let (cancel, signal) = watch::channel(());
        let (tx, rx) = mpsc::channel();
        let events = Events(tx, ctx);
        std::thread::Builder::new()
            .name("qccom-scp".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| runtime.block_on(transfer(program, ssh, host, copy, signal, &events)));
                match result {
                    Ok(true) => events.send(FileEvent::Transferred),
                    Ok(false) => events.send(FileEvent::Error(t("已取消文件操作").into())),
                    Err(error) => events.send(FileEvent::Error(format!("{error:#}"))),
                }
                events.send(FileEvent::Closed);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { rx, cancel })
    }
}

async fn transfer(program: PathBuf, ssh: String, host: Host, copy: Copy, mut cancel: watch::Receiver<()>, events: &Events) -> Result<bool> {
    let mut command = Command::new(&program);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    match interrupt(&mut cancel, needs_legacy_flag(&program)).await {
        Outcome::Complete(Ok(true)) => {
            command.arg("-O");
        }
        Outcome::Complete(Ok(false)) => {}
        Outcome::Complete(Err(error)) => return Err(error),
        Outcome::Cancelled | Outcome::Closed => return Ok(false),
    }
    command.args(FILE_SSH_OPTIONS).args(host.ssh_options("-P")).args(["-S", &ssh, "-r", "--"]);
    let destination = match copy {
        Copy::Upload(paths) => {
            for path in paths {
                command.arg(std::path::absolute(path)?);
            }
            let directory = if host.dir.trim().is_empty() { "~" } else { host.dir.trim() };
            command.arg(format!("{}:{}", host.target(true), escape_path(directory)?));
            None
        }
        Copy::Download { remote, folder } => {
            let file = local_name(&remote)?;
            // Let SCP write only into a private staging directory in the selected folder.
            let stage = tempfile::Builder::new().prefix(".qccom-scp-").tempdir_in(&folder).context(t("无法创建本地临时文件"))?;
            command.arg(format!("{}:{}", host.target(true), escape_path(&remote)?)).arg(std::path::absolute(stage.path())?);
            Some((stage, folder, file))
        }
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let auth_events = events.clone();
    let _broker = Broker::attach(&mut command, move |request| auth_events.send(FileEvent::Authentication(request))).await?;
    let mut child = command.spawn().context(t("无法启动 SCP 传输"))?;
    let stderr = child.stderr.take().context("missing SCP error stream")?;
    let mut diagnostic = tokio::spawn(read_stderr(stderr));
    let result = match interrupt(&mut cancel, async { Ok(child.wait().await?) }).await {
        Outcome::Complete(result) => result,
        Outcome::Cancelled | Outcome::Closed => {
            let _ = child.kill().await;
            diagnostic.abort();
            return Ok(false);
        }
    };
    let stderr = match timeout(Duration::from_secs(1), &mut diagnostic).await {
        Ok(Ok(stderr)) => stderr,
        _ => {
            diagnostic.abort();
            String::new()
        }
    };
    let status = result?;
    if !status.success() {
        let error = anyhow::anyhow!("{} ({status})", t("SCP 传输失败"));
        return Err(with_ssh_diagnostics(error, &stderr, false));
    }
    if let Some((stage, folder, name)) = destination {
        // OpenSSH checks names in the legacy stream. Also verify the staged top-level entry.
        tokio::task::spawn_blocking(move || {
            let mut entries = fs::read_dir(stage.path())?;
            let entry = entries.next().context(t("SCP 未返回请求的文件"))??;
            ensure!(entry.file_name() == name && entries.next().is_none(), "{}", t("SCP 未返回请求的文件"));
            publish(&entry.path(), &folder.join(name)).context(t("无法保存本地文件"))
        })
        .await??;
    }
    Ok(true)
}

async fn needs_legacy_flag(program: &Path) -> Result<bool> {
    // Old scp clients already speak SCP and do not accept -O. Probe only CLI parsing.
    let mut probe = Command::new(program);
    probe.arg("-O").env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    probe.creation_flags(0x0800_0000);
    let output = timeout(Duration::from_secs(5), probe.output()).await.context(t("SCP 客户端检测超时"))??;
    let diagnostics = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    Ok(!diagnostics
        .lines()
        .any(|line| ["unknown option", "illegal option", "invalid option", "unrecognized option"].iter().any(|text| line.contains(text))))
}

fn local_name(remote: &str) -> Result<std::ffi::OsString> {
    let name = remote.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(local)), None) if valid_name(name) => Ok(local.to_owned()),
        _ => anyhow::bail!("{}", t("远程文件名无效")),
    }
}

fn escape_path(path: &str) -> Result<String> {
    ensure!(!path.is_empty() && !path.chars().any(char::is_control), "{}", t("远程文件名无效"));
    let mut escaped = String::with_capacity(path.len());
    if path.starts_with('-') {
        escaped.push_str("./");
    }
    // Backslash escapes protect the remote shell and preserve OpenSSH's filename checks.
    for (index, c) in path.chars().enumerate() {
        let home = index == 0 && c == '~' && (path == "~" || path.starts_with("~/"));
        if !home && c.is_ascii() && !c.is_ascii_alphanumeric() && !"._:/@+-".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    Ok(escaped)
}

fn publish(staged: &Path, target: &Path) -> Result<()> {
    let kind = fs::symlink_metadata(staged)?.file_type();
    if kind.is_file() {
        tempfile::TempPath::try_from_path(staged)?.persist_noclobber(target)?;
        return Ok(());
    }
    ensure!(kind.is_dir(), "{}", t("请选择普通文件"));
    let mut entries = fs::read_dir(staged)?;
    fs::create_dir(target)?;
    let result = entries.try_for_each(|entry| {
        let entry = entry?;
        publish(&entry.path(), &target.join(entry.file_name()))
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(target);
    }
    result
}

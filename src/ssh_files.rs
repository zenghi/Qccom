//! Persistent SFTP transport over the system OpenSSH client.
use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context, Result};
use eframe::egui;
use russh_sftp::{
    client::{Config, SftpSession},
    protocol::{FileType, OpenFlags},
};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, ChildStderr, Command},
    sync::{mpsc as queue, watch},
    time::timeout,
};

use crate::{
    askpass::{AuthRequest, Broker},
    i18n::t,
    remote::{self, Host, Proto},
};

mod scp;
pub use scp::ScpTransfer;

const START_TIMEOUT: u64 = 420;
const REQUEST_TIMEOUT: u64 = 30;
const IO_CHUNK: usize = 128 * 1024;
const STDERR_LIMIT: usize = 4096;
const FILE_SSH_OPTIONS: &[&str] = &["-o", "BatchMode=no", "-o", "ClearAllForwardings=yes", "-o", "RemoteCommand=none", "-o", "ConnectTimeout=15"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
    Link,
    Other,
}

pub struct FileEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: Option<u64>,
}

pub enum FileCommand {
    List(String),
    Upload { local: PathBuf, directory: String },
    Download { remote: String, local: PathBuf },
    Cancel,
}

pub enum FileEvent {
    Authentication(AuthRequest),
    Ready,
    Entries { path: String, items: Vec<FileEntry> },
    Progress { name: String, done: u64, total: Option<u64> },
    Transferred,
    Error(String),
    Closed,
}

pub struct FileClient {
    pub rx: mpsc::Receiver<FileEvent>,
    commands: queue::Sender<FileCommand>,
    cancel: watch::Sender<()>,
}

impl FileClient {
    pub fn open(host: &Host, ctx: egui::Context) -> Result<Self, String> {
        if host.proto != Proto::Ssh {
            return Err(t("文件管理需要 SSH 连接").into());
        }
        let program = remote::ssh_program().ok_or_else(|| t("未找到 OpenSSH 客户端").to_owned())?;
        let host = host.clone();
        let (commands, incoming) = queue::channel(16);
        let (cancel, signal) = watch::channel(());
        let (tx, rx) = mpsc::channel();
        let events = Events(tx, ctx);
        std::thread::Builder::new()
            .name("qccom-sftp".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(anyhow::Error::from)
                    .and_then(|runtime| runtime.block_on(worker(program, host, incoming, signal, &events)));
                if let Err(error) = result {
                    events.send(FileEvent::Error(format!("{error:#}")));
                }
                events.send(FileEvent::Closed);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { rx, commands, cancel })
    }

    pub fn send(&self, command: FileCommand) -> Result<(), String> {
        if matches!(command, FileCommand::Cancel) {
            return self.cancel.send(()).map_err(|_| t("文件连接已关闭").into());
        }
        self.commands.try_send(command).map_err(|error| match error {
            queue::error::TrySendError::Closed(_) => t("文件连接已关闭").into(),
            queue::error::TrySendError::Full(_) => t("文件操作队列已满").into(),
        })
    }
}

#[derive(Clone)]
struct Events(mpsc::Sender<FileEvent>, egui::Context);

impl Events {
    fn send(&self, event: FileEvent) {
        if self.0.send(event).is_ok() {
            self.1.request_repaint();
        }
    }
}

async fn worker(program: String, host: Host, mut commands: queue::Receiver<FileCommand>, mut cancel: watch::Receiver<()>, events: &Events) -> Result<()> {
    let mut args = host.ssh_args();
    let target = args.pop().context("missing SSH destination")?;
    let mut command = Command::new(program);
    command
        .args(FILE_SSH_OPTIONS)
        .args(args)
        .args(["-T", "-s", "--", &target, "sftp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let auth_events = events.clone();
    let _broker = Broker::attach(&mut command, move |request| auth_events.send(FileEvent::Authentication(request))).await?;
    let mut child = command.spawn().context(t("无法启动 SSH 文件连接"))?;
    let stderr = child.stderr.take().context("missing SSH error stream")?;
    let mut diagnostics = tokio::spawn(read_stderr(stderr));

    let result = run_client(&mut child, &mut commands, &mut cancel, events).await;
    if timeout(Duration::from_secs(1), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    let stderr = match timeout(Duration::from_secs(1), &mut diagnostics).await {
        Ok(Ok(stderr)) => stderr,
        _ => {
            diagnostics.abort();
            String::new()
        }
    };
    result.map_err(|error| with_ssh_diagnostics(error, &stderr, true))
}

fn with_ssh_diagnostics(error: anyhow::Error, stderr: &str, sftp: bool) -> anyhow::Error {
    let lower = stderr.to_ascii_lowercase();
    let mut error = if stderr.is_empty() { error } else { error.context(stderr.to_owned()) };
    let hint = if sftp && lower.contains("subsystem request failed") {
        Some(t("服务器拒绝了 SFTP 子系统请求。请检查服务端 SFTP 配置，或使用 SCP 传输。"))
    } else if lower.contains("host key verification failed") || lower.contains("host identification has changed") {
        Some(t("SSH 主机密钥验证失败。请在核对服务器指纹后检查 known_hosts。"))
    } else if lower.contains("permission denied (") || lower.contains("read_passphrase:") {
        Some(t("SSH 身份验证失败。请检查用户名和密钥。若密码提示未出现，请升级 OpenSSH 客户端，或使用密钥登录。"))
    } else if ["connection refused", "connection timed out", "could not resolve hostname", "no route to host"]
        .iter()
        .any(|text| lower.contains(text))
    {
        Some(t("无法连接 SSH 服务器。请检查主机、端口和网络。"))
    } else {
        None
    };
    if let Some(hint) = hint {
        error = error.context(hint);
    }
    error
}

async fn read_stderr(mut stream: ChildStderr) -> String {
    let mut tail = Vec::with_capacity(STDERR_LIMIT);
    let mut bytes = [0; 1024];
    while let Ok(n) = stream.read(&mut bytes).await {
        if n == 0 {
            break;
        }
        let discard = (tail.len() + n).saturating_sub(STDERR_LIMIT);
        tail.drain(..discard);
        tail.extend_from_slice(&bytes[..n]);
    }
    String::from_utf8_lossy(&tail).trim().chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

async fn run_client(child: &mut Child, commands: &mut queue::Receiver<FileCommand>, cancel: &mut watch::Receiver<()>, events: &Events) -> Result<()> {
    let read = child.stdout.take().context("missing SSH output stream")?;
    let write = child.stdin.take().context("missing SSH input stream")?;
    let startup = Config {
        request_timeout_secs: START_TIMEOUT,
        max_concurrent_reads: 4,
        max_concurrent_writes: 4,
        ..Config::default()
    };
    let session = tokio::select! {
        biased;
        _ = cancel.changed() => return Ok(()),
        status = child.wait() => return Err(anyhow::anyhow!("{} ({})", t("无法建立 SFTP 会话"), status?)),
        session = SftpSession::new_with_config(io::join(read, write), startup) => session.context(t("无法建立 SFTP 会话"))?,
    };
    session.set_timeout(REQUEST_TIMEOUT);
    events.send(FileEvent::Ready);
    let result = loop {
        let command = tokio::select! {
            biased;
            signal = cancel.changed() => {
                if signal.is_err() {
                    break Ok(());
                }
                while commands.try_recv().is_ok() {}
                events.send(FileEvent::Error(t("已取消文件操作").into()));
                continue;
            }
            status = child.wait() => break Err(anyhow::anyhow!("{} ({})", t("SSH 文件连接已断开"), status?)),
            command = commands.recv() => match command {
                Some(command) => command,
                None => break Ok(()),
            }
        };
        match execute(&session, command, cancel, events).await {
            Outcome::Complete(Ok(event)) => events.send(event),
            Outcome::Complete(Err(error)) => events.send(FileEvent::Error(format!("{error:#}"))),
            Outcome::Cancelled => {
                while commands.try_recv().is_ok() {}
                events.send(FileEvent::Error(t("已取消文件操作").into()));
            }
            Outcome::Closed => break Ok(()),
        }
    };
    let _ = session.close().await;
    result
}

enum Outcome<T> {
    Complete(Result<T>),
    Cancelled,
    Closed,
}

async fn interrupt<T>(cancel: &mut watch::Receiver<()>, operation: impl Future<Output = Result<T>>) -> Outcome<T> {
    tokio::select! {
        biased;
        signal = cancel.changed() => if signal.is_ok() { Outcome::Cancelled } else { Outcome::Closed },
        result = operation => Outcome::Complete(result),
    }
}

async fn execute(session: &SftpSession, command: FileCommand, cancel: &mut watch::Receiver<()>, events: &Events) -> Outcome<FileEvent> {
    match command {
        FileCommand::List(path) => interrupt(cancel, list(session, path)).await,
        FileCommand::Download { remote, local } => interrupt(cancel, download(session, &remote, &local, events)).await,
        FileCommand::Upload { local, directory } => {
            let directory = match interrupt(cancel, absolute_path(session, directory)).await {
                Outcome::Complete(Ok(directory)) => directory,
                Outcome::Complete(Err(error)) => return Outcome::Complete(Err(error)),
                Outcome::Cancelled => return Outcome::Cancelled,
                Outcome::Closed => return Outcome::Closed,
            };
            let staged = remote_join(&directory, &format!(".qccom-upload-{}.part", uuid::Uuid::new_v4().simple()));
            let result = match interrupt(cancel, upload_stage(session, &local, &directory, &staged, events)).await {
                Outcome::Complete(Ok(target)) => {
                    // The rename is the commit point. Observe its response before reporting the result.
                    Outcome::Complete(session.rename(&staged, target).await.context(t("无法保存远程文件")).map(|()| FileEvent::Transferred))
                }
                Outcome::Complete(Err(error)) => Outcome::Complete(Err(error)),
                Outcome::Cancelled => Outcome::Cancelled,
                Outcome::Closed => Outcome::Closed,
            };
            if !matches!(result, Outcome::Complete(Ok(_))) {
                let _ = timeout(Duration::from_secs(2), session.remove_file(staged)).await;
            }
            result
        }
        FileCommand::Cancel => Outcome::Cancelled,
    }
}

async fn absolute_path(session: &SftpSession, path: String) -> Result<String> {
    let fallback = match path.as_str() {
        "" | "~" => ".",
        path if path.starts_with("~/") => &path[2..],
        path => path,
    };
    if path.starts_with('~') {
        if let Some(path) = session.expand_path(path.clone()).await? {
            return Ok(path);
        }
    }
    Ok(session.canonicalize(fallback.to_owned()).await?)
}

async fn list(session: &SftpSession, path: String) -> Result<FileEvent> {
    let path = absolute_path(session, path).await.context(t("无法打开远程目录"))?;
    let entries = session.read_dir(path.clone()).await.context(t("无法读取远程目录"))?;
    let mut items = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        if !valid_name(&name) {
            continue;
        }
        let kind = match entry.file_type() {
            FileType::Dir => FileKind::Directory,
            FileType::File => FileKind::File,
            FileType::Symlink => FileKind::Link,
            FileType::Other => FileKind::Other,
        };
        items.push(FileEntry {
            name,
            kind,
            size: entry.metadata().size,
        });
    }
    items.sort_by(|a, b| (a.kind != FileKind::Directory, &a.name).cmp(&(b.kind != FileKind::Directory, &b.name)));
    Ok(FileEvent::Entries { path, items })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

fn remote_join(directory: &str, name: &str) -> String {
    format!("{}/{name}", directory.trim_end_matches('/'))
}

async fn upload_stage(session: &SftpSession, local: &Path, directory: &str, staged: &str, events: &Events) -> Result<String> {
    let name = local.file_name().and_then(|name| name.to_str()).filter(|name| valid_name(name)).context(t("文件名无效"))?;
    ensure!(tokio::fs::metadata(local).await?.is_file(), "{}", t("请选择普通文件"));
    let mut input = tokio::fs::File::open(local).await.context(t("无法打开本地文件"))?;
    let meta = input.metadata().await?;
    ensure!(meta.is_file(), "{}", t("请选择普通文件"));
    let target = remote_join(directory, name);
    ensure!(!session.try_exists(target.clone()).await?, "{}", t("远程同名文件已存在"));
    let mut output = session
        .open_with_flags(staged, OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE)
        .await
        .context(t("无法创建远程临时文件"))?;
    copy(&mut input, &mut output, name, Some(meta.len()), events).await.context(t("上传失败"))?;
    output.close().await.context(t("无法关闭远程文件"))?;
    Ok(target)
}

async fn download(session: &SftpSession, remote: &str, local: &Path, events: &Events) -> Result<FileEvent> {
    let name = remote.rsplit('/').next().unwrap_or_default();
    ensure!(valid_name(name), "{}", t("远程文件名无效"));
    let parent = local.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let metadata = session.metadata(remote).await.context(t("无法打开远程文件"))?;
    ensure!(metadata.permissions.is_none() || metadata.file_type().is_file(), "{}", t("请选择普通文件"));
    let mut input = session.open(remote).await.context(t("无法打开远程文件"))?;
    let (file, temporary) = tempfile::NamedTempFile::new_in(parent).context(t("无法创建本地临时文件"))?.into_parts();
    let mut output = tokio::fs::File::from_std(file);
    copy(&mut input, &mut output, name, metadata.size, events).await.context(t("下载失败"))?;
    input.close().await.context(t("无法关闭远程文件"))?;
    output.flush().await?;
    output.sync_all().await?;
    drop(output);
    temporary.persist_noclobber(local).context(t("无法保存本地文件"))?;
    Ok(FileEvent::Transferred)
}

async fn copy(input: &mut (impl AsyncRead + Unpin), output: &mut (impl AsyncWrite + Unpin), name: &str, total: Option<u64>, events: &Events) -> Result<()> {
    let mut bytes = vec![0; IO_CHUNK];
    let mut done = 0;
    let mut last_update = Instant::now();
    events.send(FileEvent::Progress { name: name.into(), done, total });
    loop {
        let n = input.read(&mut bytes).await?;
        if n == 0 {
            break;
        }
        output.write_all(&bytes[..n]).await?;
        done += n as u64;
        if last_update.elapsed() >= Duration::from_millis(100) {
            last_update = Instant::now();
            events.send(FileEvent::Progress { name: name.into(), done, total });
        }
    }
    events.send(FileEvent::Progress { name: name.into(), done, total });
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        fs,
        net::{Ipv4Addr, TcpListener, TcpStream},
        process::{Child, Command},
        thread,
    };

    use super::*;

    struct Server(Child);

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn server(root: &Path, directory: &Path, sftp: bool) -> Result<(Server, Host)> {
        for name in ["server", "client"] {
            let status = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(root.join(name)).status()?;
            ensure!(status.success(), "test key generation failed");
        }
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port();
        let config = root.join("sshd_config");
        let mut settings = format!(
            "ListenAddress 127.0.0.1\nPort {port}\nHostKey {0}/server\nPidFile {0}/sshd.pid\nAuthorizedKeysFile {0}/client.pub\nStrictModes no\nPasswordAuthentication no\nUsePAM no\n",
            root.display()
        );
        if sftp {
            settings.push_str(&format!("Subsystem sftp internal-sftp -d {}\n", directory.display()));
        }
        fs::write(&config, settings)?;
        let known_hosts = root.join("known_hosts");
        fs::write(&known_hosts, format!("[127.0.0.1]:{port} {}", fs::read_to_string(root.join("server.pub"))?))?;
        let mut server = Server(Command::new("/usr/sbin/sshd").args(["-D", "-f"]).arg(config).spawn()?);
        let address = (Ipv4Addr::LOCALHOST, port).into();
        let deadline = Instant::now() + Duration::from_secs(4);
        while TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            ensure!(server.0.try_wait()?.is_none() && Instant::now() < deadline, "test SSH server did not start");
            thread::sleep(Duration::from_millis(25));
        }
        let host = Host {
            host: Ipv4Addr::LOCALHOST.to_string(),
            port,
            user: std::env::var("USER")?,
            key: root.join("client").to_string_lossy().into_owned(),
            dir: directory.to_string_lossy().into_owned(),
            args: format!("-F /dev/null -o StrictHostKeyChecking=yes -o UserKnownHostsFile=\"{}\" -o IdentitiesOnly=yes", known_hosts.display()),
            ..Host::default()
        };
        Ok((server, host))
    }

    fn next(events: &mpsc::Receiver<FileEvent>) -> Result<FileEvent> {
        loop {
            match events.recv_timeout(Duration::from_secs(10))? {
                FileEvent::Authentication(request) => {
                    request.answer(None);
                    anyhow::bail!("unexpected authentication prompt in test");
                }
                FileEvent::Progress { .. } => {}
                event => return Ok(event),
            }
        }
    }

    #[test]
    #[ignore = "requires local OpenSSH server and client binaries"]
    fn transfers_through_a_persistent_ssh_connection_without_overwriting() -> Result<()> {
        let root = tempfile::tempdir()?;
        let device = root.path().join("remote");
        fs::create_dir(&device)?;
        fs::create_dir(device.join("子目录"))?;
        let original = root.path().join("设备 固件.bin");
        let downloaded = root.path().join("下载.bin");
        let bytes: Vec<_> = (0..=255).cycle().take(314159).collect();
        fs::write(&original, &bytes)?;
        let (_server, host) = server(root.path(), &device, true)?;
        let client = FileClient::open(&host, egui::Context::default()).map_err(anyhow::Error::msg)?;
        match next(&client.rx)? {
            FileEvent::Ready => {}
            FileEvent::Error(error) => anyhow::bail!("{error}"),
            _ => anyhow::bail!("SSH file connection did not start"),
        }
        let send = |command| client.send(command).map_err(anyhow::Error::msg);

        let upload = || FileCommand::Upload {
            local: original.clone(),
            directory: ".".into(),
        };
        send(upload())?;
        assert!(matches!(next(&client.rx)?, FileEvent::Transferred));
        send(upload())?;
        assert!(matches!(next(&client.rx)?, FileEvent::Error(_)));
        send(FileCommand::List(".".into()))?;
        let FileEvent::Entries { path, items } = next(&client.rx)? else {
            anyhow::bail!("directory entries missing");
        };
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].name.as_str(), items[0].kind), ("子目录", FileKind::Directory));
        assert_eq!((items[1].name.as_str(), items[1].kind, items[1].size), ("设备 固件.bin", FileKind::File, Some(bytes.len() as u64)));
        let remote = remote_join(&path, &items[1].name);
        let download = || FileCommand::Download {
            remote: remote.clone(),
            local: downloaded.clone(),
        };
        send(download())?;
        assert!(matches!(next(&client.rx)?, FileEvent::Transferred));
        assert_eq!(fs::read(&downloaded)?, bytes);
        fs::write(&downloaded, b"existing destination")?;
        send(download())?;
        assert!(matches!(next(&client.rx)?, FileEvent::Error(_)));
        assert_eq!(fs::read(&downloaded)?, b"existing destination");
        send(FileCommand::List(".".into()))?;
        assert!(matches!(next(&client.rx)?, FileEvent::Entries { .. }));
        Ok(())
    }

    #[test]
    #[ignore = "requires local OpenSSH server and client binaries"]
    fn scp_transfers_literal_paths_when_the_server_has_no_sftp() -> Result<()> {
        let root = tempfile::tempdir()?;
        let device = root.path().join("files 目录");
        let fixture = root.path().join("ssh");
        let output = root.path().join("downloads");
        for path in [&device, &fixture, &output] {
            fs::create_dir(path)?;
        }
        let (_server, host) = server(&fixture, &device, false)?;
        let ctx = egui::Context::default();
        let sftp = FileClient::open(&host, ctx.clone()).map_err(anyhow::Error::msg)?;
        let FileEvent::Error(error) = next(&sftp.rx)? else {
            anyhow::bail!("expected a rejected subsystem request");
        };
        assert!(error.contains("subsystem request failed"));
        assert!(error.contains(t("服务器拒绝了 SFTP 子系统请求。请检查服务端 SFTP 配置，或使用 SCP 传输。")));
        assert!(matches!(next(&sftp.rx)?, FileEvent::Closed));

        let name = "it's 设备 [1];$.bin";
        let original = root.path().join(name);
        let bytes: Vec<_> = (0..=255).cycle().take(74153).collect();
        fs::write(&original, &bytes)?;
        let upload = ScpTransfer::upload(&host, vec![original], ctx.clone()).map_err(anyhow::Error::msg)?;
        match next(&upload.rx)? {
            FileEvent::Transferred => {}
            FileEvent::Error(error) => anyhow::bail!("{error}"),
            _ => anyhow::bail!("missing SCP upload result"),
        }
        assert_eq!(fs::read(device.join(name))?, bytes);
        let download = || ScpTransfer::download(&host, device.join(name).to_string_lossy().into_owned(), output.clone(), ctx.clone()).map_err(anyhow::Error::msg);
        match next(&download()?.rx)? {
            FileEvent::Transferred => {}
            FileEvent::Error(error) => anyhow::bail!("{error}"),
            _ => anyhow::bail!("missing SCP download result"),
        }
        let file = output.join(name);
        assert_eq!(fs::read(&file)?, bytes);
        fs::write(&file, b"existing file")?;
        assert!(matches!(next(&download()?.rx)?, FileEvent::Error(_)));
        assert_eq!(fs::read(&file)?, b"existing file");
        Ok(())
    }
}

//! Windows remote COM. The application end of the com0com pair remains available to
//! QCCOM or a third-party flasher. hub4com consumes the driver's ordered line-change
//! notifications and frpc transports the resulting RFC 2217 stream.
use crate::i18n::{t, tf};
use crate::serial::PortEntry;
use eframe::egui;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::JobObjects::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Host,
    Client,
}

impl Role {
    fn profile(self) -> &'static str {
        match self {
            Self::Host => "host.json",
            Self::Client => "client.json",
        }
    }
}

fn com_port(name: &str) -> bool {
    name.strip_prefix("COM").and_then(|s| s.parse::<u16>().ok()).is_some_and(|n| n > 0 && name == format!("COM{n}"))
}

// Equivalent to the hub4com RFC 2217 filters. Use argv directly so port names and
// paths never enter cmd.exe. RI=ropen is wired on the bridge end by prepare-windows.
fn serial_command(exe: &Path, role: Role, name: &str, port: u16, follow_open: bool) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg("--create-filter=escparse,com,parse");
    if role == Role::Client && follow_open {
        cmd.arg("--create-filter=pin2con,com,connect:--connect=ring");
    }
    match role {
        Role::Host => cmd.args([
            "--share-mode=on",
            "--create-filter=purge,com,purge",
            "--create-filter=pinmap,com,pinmap:--rts=cts --dtr=dsr --break=break",
            "--create-filter=linectl,com,lc:--br=remote --lc=remote",
            "--create-filter=telnet,tcp,telnet:--comport=server --suppress-echo=yes --keep-active=10",
            "--create-filter=lsrmap,tcp,lsrmap",
            "--create-filter=pinmap,tcp,pinmap:--cts=cts --dsr=dsr --dcd=dcd --ring=ring",
            "--create-filter=linectl,tcp,lc:--br=local --lc=local",
        ]),
        Role::Client => cmd.args([
            "--create-filter=pinmap,com,pinmap:--rts=cts --dtr=dsr",
            "--create-filter=linectl,com,lc:--br=local --lc=local",
            "--create-filter=telnet,tcp,telnet:--comport=client --keep-active=10",
            "--create-filter=pinmap,tcp,pinmap:--rts=cts --dtr=dsr --break=break",
            "--create-filter=linectl,tcp,lc:--br=remote --lc=remote",
        ]),
    };
    cmd.args(["--add-filters=0:com", "--add-filters=1:tcp", "--octs=off"])
        .arg(format!(r"\\.\{name}"))
        .args(["--use-driver=tcp", "--interface=127.0.0.1"]);
    match role {
        Role::Host => cmd.arg(format!("*{port}")),
        Role::Client => {
            let prefix = if follow_open { "" } else { "*" };
            cmd.arg("--reconnect=no").arg(format!("{prefix}127.0.0.1:{port}"))
        }
    };
    cmd
}

fn new_job() -> io::Result<OwnedHandle> {
    // SAFETY: unnamed private job, owned exactly once. The initialized limits
    // structure is valid for the duration of SetInformationJobObject.
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(handle, JobObjectExtendedLimitInformation, std::ptr::from_ref(&limits).cast(), std::mem::size_of_val(&limits) as u32) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

type Line = (&'static str, String);

fn read_log(stream: impl Read + Send + 'static, name: &'static str, tx: SyncSender<Line>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line).trim().to_owned();
                    if tx.send((name, text)).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

struct Session {
    job: OwnedHandle,
    _config: tempfile::NamedTempFile,
    children: Vec<(&'static str, Child)>,
    tx: SyncSender<Line>,
    rx: Receiver<Line>,
    second: Option<(&'static str, Command)>,
    role: Role,
    started: Instant,
    registrations: usize,
    follow_open: bool,
    relayed: bool,
    negotiating_since: Option<Instant>,
}

impl Session {
    fn start(role: Role, profile: &Path, name: &str, follow_open: bool) -> Result<Self, String> {
        if !com_port(name) || profile.file_name().and_then(|s| s.to_str()) != Some(role.profile()) {
            return Err(t("请选择本机端口和对应的配对文件").into());
        }
        let dir = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("remote-com");
        for exe in ["hub4com.exe", "frpc.exe"] {
            if !dir.join(exe).is_file() {
                return Err(tf("缺少 {}，请先运行 prepare-windows.ps1", &[&dir.join(exe).display()]));
            }
        }
        let port = TcpListener::bind(("127.0.0.1", 0)).and_then(|s| s.local_addr()).map_err(|e| e.to_string())?.port();
        let profile = profile.canonicalize().map_err(|e| e.to_string())?;
        // Only the app supplies frpc configuration. An invitation contains data,
        // never plugins, commands or a choice of local services to publish.
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Invitation {
            server: String,
            stun: String,
            token: String,
            secret: String,
            channel: String,
        }
        let file = std::fs::File::open(&profile).map_err(|e| e.to_string())?;
        let invite: Invitation = serde_json::from_reader(file.take(8192)).map_err(|e| e.to_string())?;
        if ![&invite.token, &invite.secret].into_iter().all(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
            || !invite.channel.strip_prefix("qccom-").is_some_and(|s| s.len() == 16 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(t("请选择本机端口和对应的配对文件").into());
        }
        let ca = profile.with_file_name("server.crt").canonicalize().map_err(|e| format!("server.crt: {e}"))?;
        let template = match role {
            Role::Host => include_bytes!("remote_com/host.toml").as_slice(),
            Role::Client => include_bytes!("remote_com/client.toml").as_slice(),
        };
        let mut config = tempfile::Builder::new().suffix(".toml").tempfile().map_err(|e| e.to_string())?;
        config.write_all(template).map_err(|e| e.to_string())?;
        let mut tunnel = Command::new(dir.join("frpc.exe"));
        tunnel
            .current_dir(&dir)
            .arg("-c")
            .arg(config.path())
            .env("QCCOM_LOOP_PORT", port.to_string())
            .env("QCCOM_SERVER", invite.server)
            .env("QCCOM_STUN", invite.stun)
            .env("QCCOM_TOKEN", invite.token)
            .env("QCCOM_SECRET", invite.secret)
            .env("QCCOM_CHANNEL", invite.channel)
            .env("QCCOM_CA", ca);
        let serial = serial_command(&dir.join("hub4com.exe"), role, name, port, follow_open);
        let (serial, tunnel) = (("hub4com", serial), ("frpc", tunnel));
        let (first, second) = if role == Role::Host { (serial, tunnel) } else { (tunnel, serial) };
        let (tx, rx) = mpsc::sync_channel(256);
        let mut session = Self {
            job: new_job().map_err(|e| e.to_string())?,
            _config: config,
            children: vec![],
            tx,
            rx,
            second: Some(second),
            role,
            started: Instant::now(),
            registrations: 0,
            follow_open,
            relayed: false,
            negotiating_since: None,
        };
        session.launch(first)?;
        Ok(session)
    }

    fn launch(&mut self, (name, mut cmd): (&'static str, Command)) -> Result<(), String> {
        let mut child = cmd
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{name}: {e}"))?;
        // SAFETY: both live handles are owned for the duration of this call.
        if unsafe { AssignProcessToJobObject(self.job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            let e = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.to_string());
        }
        read_log(child.stdout.take().unwrap(), name, self.tx.clone());
        read_log(child.stderr.take().unwrap(), name, self.tx.clone());
        self.children.push((name, child));
        Ok(())
    }

    fn poll(&mut self, status: &mut String, log: &mut VecDeque<String>) -> Result<(), String> {
        for (name, line) in self.rx.try_iter().take(256).collect::<Vec<_>>() {
            if log.len() >= 40 {
                log.pop_front();
            }
            log.push_back(format!("[{name}] {line}"));
            if name == "frpc" && (line.contains("control message dispatcher exited") || self.registrations > 0 && line.contains("login to server success")) {
                return Err(t("服务器连接已中断，请重新启动映射").into());
            }
            if name == "frpc" && (line.contains("start visitor success") || line.contains("start proxy success")) {
                self.registrations += 1;
                if self.registrations == 2 {
                    *status = t("隧道已注册，等待应用打开串口").into();
                }
            }
            let ready = match self.role {
                Role::Host => name == "hub4com" && line.starts_with("Listen(") && line.ends_with(" - OK"),
                Role::Client => self.registrations == 2,
            };
            if ready {
                if let Some(command) = self.second.take() {
                    self.launch(command)?;
                }
            }
            if name == "hub4com" && line.ends_with(": Connected") {
                self.negotiating_since = Some(Instant::now());
                *status = t("正在协商远端串口").into();
            }
            if name == "hub4com" && line.ends_with(": Disconnected") {
                self.relayed = false;
                self.negotiating_since = None;
                if self.role == Role::Client && !self.follow_open {
                    return Err(t("远端连接已断开，请重新启动映射").into());
                }
                *status = t("连接已关闭，等待重新打开串口").into();
            }
            if name == "frpc" && line.contains("try to transfer connection to visitor: serial-stcp") {
                self.relayed = true;
                *status = t("正在通过服务器中继").into();
            }
            if name == "hub4com" && line.ends_with(" RECV: SB 44") {
                self.negotiating_since = None;
                *status = match self.role {
                    Role::Host => t("远端已协商串口参数"),
                    Role::Client if self.relayed => t("远程串口就绪 · 服务器中继"),
                    Role::Client => t("远程串口就绪 · P2P"),
                }
                .into();
            }
            let serial_failure = line.contains(" ERROR ")
                && ["CreateFile(", "ReadFileEx(", "WriteFileEx(", "SetCommState(", "EscapeCommFunction("]
                    .iter()
                    .any(|call| line.contains(call));
            if name == "hub4com" && (line.contains("Write lost ") || serial_failure) {
                return Err(line);
            }
        }
        for (name, child) in &mut self.children {
            if let Some(exit) = child.try_wait().map_err(|e| e.to_string())? {
                return Err(format!("{name}: {exit}"));
            }
        }
        let timeout = Duration::from_secs(30);
        if (self.second.is_some() || self.registrations < 2) && self.started.elapsed() > timeout || self.negotiating_since.is_some_and(|at| at.elapsed() > timeout) {
            return Err(t("启动超时，请检查端口、配对文件及服务器日志").into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Panel {
    profile: Option<PathBuf>,
    port: String,
    pairs: Vec<crate::vcom::Pair>,
    session: Option<Session>,
    follow_open: bool,
    status: String,
    log: VecDeque<String>,
}

impl Panel {
    pub fn tick(&mut self, ctx: &egui::Context) {
        if let Some(session) = &mut self.session {
            if let Err(e) = session.poll(&mut self.status, &mut self.log) {
                self.status = e;
                self.session = None;
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ports: &[PortEntry]) {
        ui.collapsing(t("远程 COM（P2P）"), |ui| {
            ui.add_enabled_ui(self.session.is_none(), |ui| {
                if ui.button(t("选择配对文件")).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("QCCOM", &["json"]).pick_file() {
                        self.profile = Some(path);
                        self.port.clear();
                        self.status.clear();
                        self.pairs = crate::vcom::pairs();
                    }
                }
            });
            let Some(profile) = self.profile.as_deref() else {
                ui.small(t("设备端打开 host.json，烧录端打开 client.json"));
                return;
            };
            let role = match profile.file_name().and_then(|s| s.to_str()) {
                Some("host.json") => Role::Host,
                Some("client.json") => Role::Client,
                _ => {
                    ui.label(t("请选择本机端口和对应的配对文件"));
                    return;
                }
            };
            ui.add_enabled_ui(self.session.is_none(), |ui| {
                let label = if role == Role::Host { t("设备端 · 物理串口") } else { t("烧录端 · 虚拟串口对") };
                ui.label(label);
                egui::ComboBox::from_id_salt("remote-com-port").selected_text(&self.port).show_ui(ui, |ui| {
                    if role == Role::Host {
                        for p in ports.iter().filter(|p| com_port(&p.name)) {
                            ui.selectable_value(&mut self.port, p.name.clone(), &p.name);
                        }
                    } else {
                        for pair in &self.pairs {
                            ui.selectable_value(&mut self.port, pair.mine.clone(), format!("{} ↔ {}", pair.mine, pair.other));
                        }
                    }
                });
            });
            if role == Role::Client {
                ui.add_enabled(self.session.is_none(), egui::Checkbox::new(&mut self.follow_open, t("随应用打开和关闭远端串口")))
                    .on_hover_text(t("默认提前连接，等串口就绪再烧录。需要靠打开串口触发复位时可勾选"));
                if let Some(p) = self.pairs.iter().find(|p| p.mine == self.port) {
                    ui.label(tf("QCCOM 或烧录工具打开 {}", &[&p.other]));
                } else {
                    ui.small(t("请先用 prepare-windows.ps1 -CreatePair 创建烧录口，再选择配对文件"));
                }
            }
            if self.session.is_some() {
                if ui.button(t("停止映射")).clicked() {
                    self.session = None;
                    self.status = t("已停止").into();
                }
            } else if ui.add_enabled(!self.port.is_empty(), egui::Button::new(t("启动映射"))).clicked() {
                self.log.clear();
                match Session::start(role, profile, &self.port, self.follow_open) {
                    Ok(session) => {
                        self.session = Some(session);
                        self.status = t("正在启动").into();
                    }
                    Err(e) => self.status = e,
                }
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            if !self.log.is_empty() {
                ui.collapsing(t("连接日志"), |ui| {
                    egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                        for line in &self.log {
                            ui.small(line);
                        }
                    });
                });
            }
        });
    }
}

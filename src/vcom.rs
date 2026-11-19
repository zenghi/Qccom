//! 串口共享给其他软件：QCCOM 打开真实串口，再「虚拟」出串口给别的工具（烧录、另一个串口助手）同时用，
//! 大家看到同样的数据、都能发，不用再抢 COM 口。
//!
//! - Linux / macOS：直接创建虚拟串口（伪终端），不用装任何东西。路径形如 /dev/pts/5，
//!   另外在 数据目录/vcom/ 下放一个固定名字的链接 ttyQCCOM0，别的工具每次填同一个路径就行。
//! - Windows：系统不允许软件凭空造 COM 口（要驱动）。装上免费的 com0com 后会有一对互通的口（如 COM10 ↔ COM11），
//!   QCCOM 占一头，别的软件打开另一头。也可以用「共享到网络」走 TCP。

use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;

pub enum VcomEvt {
    /// 别的软件发来的数据，要写到真实串口
    Data(Vec<u8>),
    Error(String),
}

pub struct Vcom {
    /// 给别的软件用的路径 / 端口名
    pub names: Vec<String>,
    to_ports: Sender<Vec<u8>>,
    pub rx: Receiver<VcomEvt>,
}

trait Endpoint: Send {
    /// 非阻塞读：没数据返回 Ok(0)
    fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all_to(&mut self, d: &[u8]) -> std::io::Result<()>;
}

impl Vcom {
    /// Linux / macOS：创建 n 个虚拟串口
    #[cfg(unix)]
    pub fn start_pty(n: usize, ctx: egui::Context) -> Result<Vcom, String> {
        let dir = crate::data_dir().join("vcom");
        let _ = std::fs::create_dir_all(&dir);
        let mut eps: Vec<Box<dyn Endpoint>> = vec![];
        let mut names = vec![];
        for i in 0..n.max(1) {
            let p = pty::Pty::open()?;
            let link = dir.join(format!("ttyQCCOM{i}"));
            let _ = std::fs::remove_file(&link);
            let shown = match std::os::unix::fs::symlink(&p.path, &link) {
                Ok(()) => format!("{}  →  {}", p.path, link.display()),
                Err(_) => p.path.clone(),
            };
            names.push(shown);
            eps.push(Box::new(p));
        }
        Ok(Self::spawn(eps, names, ctx))
    }

    #[cfg_attr(unix, allow(dead_code))]
    /// 打开一个现有的串口（com0com 等虚拟串口对的一头）当作共享口
    pub fn start_port(name: &str, baud: u32, ctx: egui::Context) -> Result<Vcom, String> {
        let p = serialport::new(name, baud).timeout(Duration::from_millis(5)).open().map_err(|e| e.to_string())?;
        let ep: Box<dyn Endpoint> = Box::new(SerialEp(p));
        Ok(Self::spawn(vec![ep], vec![name.to_string()], ctx))
    }

    fn spawn(eps: Vec<Box<dyn Endpoint>>, names: Vec<String>, ctx: egui::Context) -> Vcom {
        let (to_ports, from_app) = mpsc::channel::<Vec<u8>>();
        let (tx, rx) = mpsc::channel();
        let _ = thread::Builder::new().name("vcom".into()).spawn(move || run(eps, from_app, tx, ctx));
        Vcom { names, to_ports, rx }
    }

    /// 真实串口收到的数据转发给各个虚拟口
    pub fn forward(&self, d: &[u8]) {
        let _ = self.to_ports.send(d.to_vec());
    }
}

impl Drop for Vcom {
    fn drop(&mut self) {
        #[cfg(unix)]
        for i in 0..self.names.len() {
            let _ = std::fs::remove_file(crate::data_dir().join("vcom").join(format!("ttyQCCOM{i}")));
        }
    }
}

fn run(mut eps: Vec<Box<dyn Endpoint>>, from_app: Receiver<Vec<u8>>, tx: Sender<VcomEvt>, ctx: egui::Context) {
    let mut buf = [0u8; 4096];
    loop {
        loop {
            match from_app.try_recv() {
                Ok(d) => {
                    for e in eps.iter_mut() {
                        // 没有软件打开虚拟口时写不进去，丢掉就好
                        let _ = e.write_all_to(&d);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let mut got = false;
        for e in eps.iter_mut() {
            match e.read_some(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    got = true;
                    if tx.send(VcomEvt::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(e) => {
                    let _ = tx.send(VcomEvt::Error(e.to_string()));
                    ctx.request_repaint();
                    return;
                }
            }
        }
        if got {
            ctx.request_repaint();
        } else {
            thread::sleep(Duration::from_millis(3));
        }
    }
}

#[cfg_attr(unix, allow(dead_code))]
struct SerialEp(Box<dyn serialport::SerialPort>);

impl Endpoint for SerialEp {
    fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.0.read(buf) {
            Err(e) if e.kind() == ErrorKind::TimedOut => Ok(0),
            r => r,
        }
    }
    fn write_all_to(&mut self, d: &[u8]) -> std::io::Result<()> {
        self.0.write_all(d)
    }
}

#[cfg_attr(unix, allow(dead_code))]
/// com0com 的一对虚拟串口：QCCOM 打开 mine，其他软件打开 other
#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub mine: String,
    pub other: String,
}

#[cfg_attr(unix, allow(dead_code))]
/// 系统里现有的 com0com 串口对。读注册表 HARDWARE\DEVICEMAP\SERIALCOMM（不用管理员）：
/// A 头的设备名是 \Device\com0com1N，B 头是 \Device\com0com2N，N 是第几对
pub fn pairs() -> Vec<Pair> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let Ok(o) = std::process::Command::new("reg")
            .args(["query", r"HKLM\HARDWARE\DEVICEMAP\SERIALCOMM"])
            .creation_flags(0x0800_0000)
            .output()
        else {
            return vec![];
        };
        use qccom_tunnel::mapping::valid_com;
        parse_serialcomm(&String::from_utf8_lossy(&o.stdout))
            .into_iter()
            .filter(|pair| valid_com(&pair.mine) && valid_com(&pair.other) && pair.mine != pair.other)
            .collect()
    }
    #[cfg(not(windows))]
    vec![]
}

#[cfg_attr(unix, allow(dead_code))]
pub fn parse_serialcomm(out: &str) -> Vec<Pair> {
    let mut a = std::collections::BTreeMap::new();
    let mut b = std::collections::BTreeMap::new();
    for l in out.lines() {
        let mut it = l.split_whitespace();
        let (Some(dev), Some(_), Some(port)) = (it.next(), it.next(), it.next()) else { continue };
        let Some(rest) = dev.to_ascii_lowercase().strip_prefix(r"\device\com0com").map(|r| r.to_string()) else {
            continue;
        };
        let (side, n) = rest.split_at(1.min(rest.len()));
        match side {
            "1" => a.insert(n.to_string(), port.to_string()),
            "2" => b.insert(n.to_string(), port.to_string()),
            _ => None,
        };
    }
    a.into_iter().filter_map(|(n, mine)| b.get(&n).map(|other| Pair { mine, other: other.clone() })).collect()
}

/// com0com 3.0.0.0 的原版安装包（setup_x64.exe，带旧式交叉签名）。不打包进 exe：QCCOM 只包含自己源码编译出的代码，
/// 用户点「一键安装」时才下载，校验下面的 SHA256 一致才运行。环境变量 QCCOM_COM0COM_URL 可以临时换地址
pub const COM0COM_URL: &str = "https://www.zenghi.com/com0com/setup_x64.exe";
pub const COM0COM_SHA256: &str = "26486b28604b49a9008c54feb11b9ece0008a8287ee5caf0bcf2a62f4317128f";

/// 安装包内容是否就是 com0com 官方那一份
#[cfg_attr(not(windows), allow(dead_code))]
pub fn com0com_ok(data: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
    hex == COM0COM_SHA256
}

#[cfg(windows)]
pub(crate) fn check_com0com_signature() -> Result<(), String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW, CM_GETIDLIST_DONOTGENERATE, CM_GETIDLIST_FILTER_PRESENT, CM_GETIDLIST_FILTER_SERVICE,
        CM_LOCATE_DEVNODE_NORMAL, CM_PROB_UNSIGNED_DRIVER, CR_BUFFER_SMALL, CR_SUCCESS, DN_HAS_PROBLEM,
    };

    // All three com0com INFs use this service, including the bus node that can
    // be blocked before Windows enumerates any COM ports.
    let service = windows_sys::w!("com0com");
    let filter = CM_GETIDLIST_FILTER_SERVICE | CM_GETIDLIST_FILTER_PRESENT | CM_GETIDLIST_DONOTGENERATE;
    for _ in 0..3 {
        let mut size = 0;
        // SAFETY: the service string is NUL-terminated and the size is writable.
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut size, service, filter) } != CR_SUCCESS || size <= 1 {
            return Ok(());
        }
        let mut ids = vec![0u16; size as usize];
        // SAFETY: the mutable UTF-16 buffer holds the requested number of code units.
        match unsafe { CM_Get_Device_ID_ListW(service, ids.as_mut_ptr(), size, filter) } {
            CR_SUCCESS => {}
            CR_BUFFER_SMALL => continue,
            _ => return Ok(()),
        }
        for id in ids.split_inclusive(|&unit| unit == 0).take_while(|id| id.len() > 1) {
            if id.last() != Some(&0) {
                break;
            }
            let (mut node, mut status, mut problem) = (0, 0, 0);
            // SAFETY: id is NUL-terminated and the status outputs are writable.
            let blocked = unsafe {
                CM_Locate_DevNodeW(&mut node, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS
                    && CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) == CR_SUCCESS
                    && status & DN_HAS_PROBLEM != 0
                    && problem == CM_PROB_UNSIGNED_DRIVER
            };
            if blocked {
                return Err(crate::i18n::t("Windows 拒绝加载 com0com 驱动（代码 52）。请安装兼容当前 Windows 的已签名 com0com 驱动。").into());
            }
        }
        break;
    }
    Ok(())
}

#[cfg(windows)]
fn fetch_com0com() -> Result<Vec<u8>, String> {
    use std::io::Read;
    let url = std::env::var("QCCOM_COM0COM_URL").unwrap_or_else(|_| COM0COM_URL.into());
    let resp = crate::update::agent()?.get(&url).call().map_err(|e| crate::i18n::tf("下载 com0com 失败：{}", &[&e]))?;
    let mut data = Vec::new();
    resp.into_reader().take(16 << 20).read_to_end(&mut data).map_err(|e| crate::i18n::tf("下载 com0com 失败：{}", &[&e]))?;
    if !com0com_ok(&data) {
        return Err(crate::i18n::t("下载到的 com0com 安装包校验不通过，没有运行").to_string());
    }
    Ok(data)
}

/// Windows：一键安装 com0com（先下载官方安装包并校验），并建好一对 COM 口。会弹一次管理员确认。阻塞，放线程里跑
#[cfg(windows)]
pub fn install_com0com() -> Result<(), String> {
    use crate::i18n::tf;
    use std::os::windows::process::CommandExt;
    if !pairs().is_empty() {
        return Ok(());
    }
    if crate::remote_com::setupc().is_some() {
        return crate::remote_com::create_pair();
    }
    check_com0com_signature()?;
    let bytes = fetch_com0com()?;
    let dir = tempfile::Builder::new().prefix("qccom-driver-").tempdir().map_err(|e| e.to_string())?;
    let exe = dir.path().join("setup_com0com_x64.exe");
    std::fs::write(&exe, bytes).map_err(|e| e.to_string())?;
    // Pass the verified path in the environment so portable paths are not PowerShell source.
    let status = std::process::Command::new("powershell.exe")
        .creation_flags(0x0800_0000)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ErrorActionPreference='Stop'; $p=Start-Process -FilePath $env:QCCOM_DRIVER_INSTALLER -ArgumentList '/S' -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        ])
        .env("QCCOM_DRIVER_INSTALLER", exe)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        check_com0com_signature()?;
        return Err(tf("com0com 安装未完成（{}）", &[&status]));
    }
    if !pairs().is_empty() {
        return Ok(());
    }
    crate::remote_com::create_pair()
}

#[cfg(unix)]
mod pty {
    use super::Endpoint;
    use std::ffi::CStr;

    pub struct Pty {
        master: libc::c_int,
        /// 自己留着从端，没人打开时主端也不会报错
        slave: libc::c_int,
        pub path: String,
    }

    impl Pty {
        pub fn open() -> Result<Pty, String> {
            let (mut m, mut s) = (0, 0);
            let mut name = [0 as libc::c_char; 128];
            // SAFETY: openpty 写入两个 fd 和名字缓冲区（128 字节足够放 /dev/pts/N）
            let r = unsafe { libc::openpty(&mut m, &mut s, name.as_mut_ptr(), std::ptr::null(), std::ptr::null()) };
            if r != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // SAFETY: openpty 成功时 name 是以 0 结尾的路径
            let path = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().to_string();
            // 原始模式：不回显、不转换换行，字节原样过去
            // SAFETY: s 是刚打开的终端 fd，termios 由 tcgetattr 填好
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(s, &mut t) == 0 {
                    libc::cfmakeraw(&mut t);
                    libc::tcsetattr(s, libc::TCSANOW, &t);
                }
                let fl = libc::fcntl(m, libc::F_GETFL);
                libc::fcntl(m, libc::F_SETFL, fl | libc::O_NONBLOCK);
                libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            Ok(Pty { master: m, slave: s, path })
        }
    }

    impl Endpoint for Pty {
        fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // SAFETY: buf 可写，长度正确
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                // 对端关闭（EIO）在我们自己留着从端时不会出现；出现也当作没数据
                if e.raw_os_error() == Some(libc::EIO) {
                    return Ok(0);
                }
                return Err(e);
            }
            Ok(n as usize)
        }

        fn write_all_to(&mut self, mut d: &[u8]) -> std::io::Result<()> {
            let mut spins = 0;
            while !d.is_empty() {
                // SAFETY: d 是有效切片
                let n = unsafe { libc::write(self.master, d.as_ptr().cast(), d.len()) };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() == std::io::ErrorKind::WouldBlock && spins < 20 {
                        spins += 1;
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    return Err(e);
                }
                d = &d[n as usize..];
            }
            Ok(())
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // SAFETY: 两个 fd 都由 openpty 打开，只关一次
            unsafe {
                libc::close(self.master);
                libc::close(self.slave);
            }
        }
    }
}

#[cfg(test)]
mod pair_tests {
    #[test]
    fn com0com_hash_check() {
        assert!(!super::com0com_ok(b"<html>not found</html>"));
        assert_eq!(super::COM0COM_SHA256.len(), 64);
    }

    #[test]
    fn serialcomm_pairs() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\HARDWARE\\DEVICEMAP\\SERIALCOMM\r\n    \\Device\\Serial0    REG_SZ    COM1\r\n    \\Device\\com0com10    REG_SZ    COM20\r\n    \\Device\\com0com20    REG_SZ    COM21\r\n    \\Device\\com0com11    REG_SZ    CNCA1\r\n";
        let p = super::parse_serialcomm(out);
        assert_eq!(
            p,
            vec![super::Pair {
                mine: "COM20".into(),
                other: "COM21".into()
            }]
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn pty_passes_bytes_both_ways() {
        let v = Vcom::start_pty(1, egui::Context::default()).unwrap();
        let path = v.names[0].split_whitespace().next().unwrap().to_string();
        let mut f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open(&path).unwrap();
        v.forward(b"hello\r\n");
        let mut got = [0u8; 7];
        f.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello\r\n");
        f.write_all(b"AT\r").unwrap();
        let mut data = vec![];
        for _ in 0..200 {
            if let Ok(VcomEvt::Data(d)) = v.rx.try_recv() {
                data.extend(d);
                if data.len() >= 3 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(data, b"AT\r");
    }
}

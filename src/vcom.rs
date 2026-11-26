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
    external_ports: Vec<String>,
    to_ports: Sender<Vec<u8>>,
    pub rx: Receiver<VcomEvt>,
    #[cfg(unix)]
    links: Vec<(std::path::PathBuf, String)>,
}

trait Endpoint: Send {
    /// 非阻塞读：没数据返回 Ok(0)
    fn read_some(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all_to(&mut self, d: &[u8]) -> std::io::Result<()>;
}

impl Vcom {
    /// Linux / macOS：创建 n 个虚拟串口
    #[cfg(unix)]
    pub fn start_pty(n: usize, slot: usize, ctx: egui::Context) -> Result<Vcom, String> {
        let dir = crate::data_dir().join("vcom");
        let _ = std::fs::create_dir_all(&dir);
        let mut eps: Vec<Box<dyn Endpoint>> = vec![];
        let mut names = vec![];
        let mut external_ports = vec![];
        let ports: Vec<_> = (0..n.max(1)).map(|_| pty::Pty::open()).collect::<Result<_, _>>()?;
        let mut links = vec![];
        for (i, p) in ports.into_iter().enumerate() {
            let link = if slot == 0 { dir.join(format!("ttyQCCOM{i}")) } else { dir.join(format!("ttyQCCOM-{slot}-{i}")) };
            if std::fs::read_link(&link).is_ok_and(|previous| !previous.exists()) {
                let _ = std::fs::remove_file(&link);
            }
            let shown = match std::os::unix::fs::symlink(&p.path, &link) {
                Ok(()) => {
                    let shown = format!("{}  →  {}", p.path, link.display());
                    links.push((link, p.path.clone()));
                    shown
                }
                Err(_) => p.path.clone(),
            };
            names.push(shown);
            external_ports.push(p.path.clone());
            eps.push(Box::new(p));
        }
        let mut vcom = Self::spawn(eps, external_ports, ctx);
        vcom.names = names;
        vcom.links = links;
        Ok(vcom)
    }

    #[cfg_attr(unix, allow(dead_code))]
    /// 打开一个现有的串口（com0com 等虚拟串口对的一头）当作共享口
    pub fn start_port(bridge_end: &str, external_end: &str, baud: u32, ctx: egui::Context) -> Result<Vcom, String> {
        let p = serialport::new(bridge_end, baud).timeout(Duration::from_millis(5)).open().map_err(|e| e.to_string())?;
        let ep: Box<dyn Endpoint> = Box::new(SerialEp(p));
        Ok(Self::spawn(vec![ep], vec![external_end.to_owned()], ctx))
    }

    fn spawn(eps: Vec<Box<dyn Endpoint>>, external_ports: Vec<String>, ctx: egui::Context) -> Vcom {
        let (to_ports, from_app) = mpsc::channel::<Vec<u8>>();
        let (tx, rx) = mpsc::channel();
        let _ = thread::Builder::new().name("vcom".into()).spawn(move || run(eps, from_app, tx, ctx));
        Vcom {
            names: external_ports.clone(),
            external_ports,
            to_ports,
            rx,
            #[cfg(unix)]
            links: Vec::new(),
        }
    }

    /// 外部软件打开的端口名，和界面展示文字分开保存。
    pub fn external_ports(&self) -> &[String] {
        &self.external_ports
    }

    pub fn owns_port(&self, name: &str) -> bool {
        #[cfg(windows)]
        let name = name.strip_prefix(r"\\.\").unwrap_or(name);
        if self.external_ports.iter().any(|port| if cfg!(windows) { port.eq_ignore_ascii_case(name) } else { port == name }) {
            return true;
        }
        #[cfg(unix)]
        return self
            .links
            .iter()
            .any(|(link, target)| link == std::path::Path::new(name) && std::fs::read_link(link).is_ok_and(|current| current == std::path::Path::new(target)));
        #[cfg(not(unix))]
        false
    }

    /// 真实串口收到的数据转发给各个虚拟口
    pub fn forward(&self, d: &[u8]) {
        let _ = self.to_ports.send(d.to_vec());
    }
}

impl Drop for Vcom {
    fn drop(&mut self) {
        #[cfg(unix)]
        for (link, target) in &self.links {
            if std::fs::read_link(link).is_ok_and(|current| current == std::path::Path::new(target)) {
                let _ = std::fs::remove_file(link);
            }
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
        use qccom_tunnel::mapping::valid_com;
        collect_pairs(serialcomm_values())
            .into_iter()
            .filter(|pair| valid_com(&pair.mine) && valid_com(&pair.other) && pair.mine != pair.other)
            .collect()
    }
    #[cfg(not(windows))]
    vec![]
}

#[cfg(windows)]
fn serialcomm_values() -> Vec<(String, String)> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS},
        System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_SZ},
    };

    let mut key: HKEY = null_mut();
    // SAFETY: The key name is NUL-terminated and `key` is writable.
    let opened = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, windows_sys::w!(r"HARDWARE\DEVICEMAP\SERIALCOMM"), 0, KEY_QUERY_VALUE, &mut key) };
    if opened != ERROR_SUCCESS {
        return Vec::new();
    }

    let mut values = Vec::new();
    let mut name = [0u16; 512];
    let mut data = [0u16; 64];
    for index in 0..u32::MAX {
        let mut name_len = name.len() as u32;
        let mut data_len = std::mem::size_of_val(&data) as u32;
        let mut kind = 0;
        // SAFETY: `key` is open, and both buffers and their sizes are valid for the call.
        let result = unsafe { RegEnumValueW(key, index, name.as_mut_ptr(), &mut name_len, null(), &mut kind, data.as_mut_ptr().cast(), &mut data_len) };
        if result == ERROR_MORE_DATA {
            // Legitimate com0com device and COM names fit in the buffers.
            continue;
        }
        if result != ERROR_SUCCESS {
            break;
        }
        if kind != REG_SZ || data_len % 2 != 0 {
            continue;
        }
        let device = String::from_utf16_lossy(&name[..name_len as usize]);
        let value = &data[..data_len as usize / 2];
        let end = value.iter().position(|&unit| unit == 0).unwrap_or(value.len());
        let port = String::from_utf16_lossy(&value[..end]);
        values.push((device, port));
    }
    // SAFETY: Only this function owns the handle returned by RegOpenKeyExW.
    unsafe { RegCloseKey(key) };
    values
}

#[cfg(any(windows, test))]
fn collect_pairs(values: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>) -> Vec<Pair> {
    let mut a = std::collections::BTreeMap::new();
    let mut b = std::collections::BTreeMap::new();
    for (device, value) in values {
        let device = device.as_ref().to_ascii_lowercase();
        let Some(rest) = device.strip_prefix(r"\device\com0com") else { continue };
        let Some(port) = value.as_ref().split_whitespace().next() else {
            continue;
        };
        if let Some(n) = rest.strip_prefix('1') {
            a.insert(n.to_owned(), port.to_owned());
        } else if let Some(n) = rest.strip_prefix('2') {
            b.insert(n.to_owned(), port.to_owned());
        }
    }
    a.into_iter().filter_map(|(n, mine)| b.get(&n).map(|other| Pair { mine, other: other.clone() })).collect()
}

#[cfg(windows)]
pub(crate) fn com0com_setup_ui(ui: &mut egui::Ui) -> bool {
    use crate::i18n::t;
    ui.small(t("请在浏览器下载并安装兼容当前 Windows 的 com0com 驱动，再用它自带的工具创建一对虚拟 COM 口。"));
    ui.horizontal_wrapped(|ui| {
        ui.hyperlink_to(t("com0com 官方下载"), "https://sourceforge.net/projects/com0com/files/com0com/");
        ui.button(t("重新检测虚拟串口")).clicked()
    })
    .inner
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
    fn serialcomm_pairs() {
        let values = [
            (r"\Device\Serial0", "COM1"),
            (r"\Device\com0com10", "COM20"),
            (r"\Device\COM0COM20", "COM21"),
            (r"\Device\com0com11", "CNCA1"),
            (r"\Device\com0com串0", "COM22"),
        ];
        let p = super::collect_pairs(values);
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
        let v = Vcom::start_pty(1, 0, egui::Context::default()).unwrap();
        let path = &v.external_ports()[0];
        assert!(v.owns_port(path));
        for (link, _) in &v.links {
            assert!(v.owns_port(link.to_str().unwrap()));
        }
        let mut f = std::fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open(path).unwrap();
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
        assert!(matches!(v.rx.recv_timeout(Duration::from_millis(25)), Err(mpsc::RecvTimeoutError::Timeout)));
    }
}

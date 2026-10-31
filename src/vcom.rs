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

/// com0com 等虚拟串口：Windows 上按端口描述认出来
pub fn virtual_pairs() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            let n = p.port_name.to_ascii_uppercase();
            let desc = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => u.product.clone().unwrap_or_default(),
                _ => String::new(),
            }
            .to_ascii_lowercase();
            n.starts_with("CNC") || desc.contains("com0com") || desc.contains("virtual") || desc.contains("emulator")
        })
        .map(|p| p.port_name)
        .collect()
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

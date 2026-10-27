//! 串口转 TCP：把当前串口共享到网络上，别的电脑（或别的软件）连上这个端口就能收发同样的数据。
//! 一个后台线程负责监听和所有客户端；串口收到的数据由界面线程转发进来。

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;

pub enum BridgeEvt {
    /// 客户端发来的数据，要写到串口
    Data(Vec<u8>),
    Clients(usize),
    Error(String),
}

pub struct Bridge {
    pub port: u16,
    to_net: Sender<Vec<u8>>,
    pub rx: Receiver<BridgeEvt>,
    pub clients: usize,
}

impl Bridge {
    pub fn start(port: u16, ctx: egui::Context) -> Result<Bridge, String> {
        let l = TcpListener::bind(("0.0.0.0", port)).map_err(|e| e.to_string())?;
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (to_net, from_app) = mpsc::channel::<Vec<u8>>();
        let (evt_tx, rx) = mpsc::channel();
        thread::Builder::new().name("bridge".into()).spawn(move || run(l, from_app, evt_tx, ctx)).map_err(|e| e.to_string())?;
        Ok(Bridge { port, to_net, rx, clients: 0 })
    }

    /// 串口收到的数据转发给所有客户端
    pub fn forward(&self, d: &[u8]) {
        let _ = self.to_net.send(d.to_vec());
    }
}

fn run(l: TcpListener, from_app: Receiver<Vec<u8>>, tx: Sender<BridgeEvt>, ctx: egui::Context) {
    let mut clients: Vec<TcpStream> = vec![];
    let mut buf = [0u8; 4096];
    loop {
        let before = clients.len();
        // 新连接
        loop {
            match l.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(true);
                    let _ = s.set_nodelay(true);
                    clients.push(s);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    let _ = tx.send(BridgeEvt::Error(e.to_string()));
                    break;
                }
            }
        }
        // 串口 → 网络；界面那头关掉（Bridge 被 drop）就退出
        loop {
            match from_app.try_recv() {
                Ok(d) => clients.retain_mut(|c| write_all(c, &d)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // 网络 → 串口
        let mut got = false;
        clients.retain_mut(|c| match c.read(&mut buf) {
            Ok(0) => false,
            Ok(n) => {
                got = true;
                tx.send(BridgeEvt::Data(buf[..n].to_vec())).is_ok()
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => true,
            Err(_) => false,
        });
        if clients.len() != before || got {
            if clients.len() != before {
                let _ = tx.send(BridgeEvt::Clients(clients.len()));
            }
            ctx.request_repaint();
        }
        if !got {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn write_all(c: &mut TcpStream, mut d: &[u8]) -> bool {
    // 非阻塞写：对方太慢时稍等一下，等太久就当它掉线
    let mut spins = 0;
    while !d.is_empty() {
        match c.write(d) {
            Ok(0) => return false,
            Ok(n) => d = &d[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock && spins < 200 => {
                spins += 1;
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwards_both_ways() {
        let port = 47_000 + (std::process::id() % 1000) as u16;
        let b = Bridge::start(port, egui::Context::default()).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        // 等客户端被接受
        let mut n = 0;
        for _ in 0..100 {
            if let Ok(BridgeEvt::Clients(k)) = b.rx.try_recv() {
                n = k;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(n, 1);
        b.forward(b"hello");
        let mut got = [0u8; 5];
        c.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello");
        c.write_all(b"up").unwrap();
        let mut data = None;
        for _ in 0..100 {
            if let Ok(BridgeEvt::Data(d)) = b.rx.try_recv() {
                data = Some(d);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(data.as_deref(), Some(&b"up"[..]));
    }
}

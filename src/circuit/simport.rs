//! 仿真串口：电路里的 USB 转 TTL、USB 供电的开发板出现在 QCCOM 的串口列表里（名字以 SIM: 开头），
//! 打开后和真串口一样收发。电路页和串口线程通过这里的共享缓冲交换数据。

use std::collections::VecDeque;
use std::sync::Mutex;

pub const PREFIX: &str = "SIM:";

struct Port {
    name: String,
    desc: String,
    part: u32,
    to_host: VecDeque<u8>,
    from_host: Vec<u8>,
    /// QCCOM 打开时设的波特率；None = 没打开
    baud: Option<u32>,
}

static PORTS: Mutex<Vec<Port>> = Mutex::new(Vec::new());

/// 电路页每帧同步：现在有哪些仿真串口 (名字, 描述, 元件 id)
pub fn sync(list: &[(String, String, u32)]) {
    let mut v = PORTS.lock().unwrap();
    v.retain(|p| list.iter().any(|x| x.0 == p.name));
    for (name, desc, part) in list {
        match v.iter_mut().find(|p| &p.name == name) {
            Some(p) => {
                p.desc = desc.clone();
                p.part = *part;
            }
            None => v.push(Port {
                name: name.clone(),
                desc: desc.clone(),
                part: *part,
                to_host: VecDeque::new(),
                from_host: vec![],
                baud: None,
            }),
        }
    }
}

/// 串口列表用：(名字, 描述)
pub fn list() -> Vec<(String, String)> {
    PORTS.lock().unwrap().iter().map(|p| (p.name.clone(), p.desc.clone())).collect()
}

pub fn exists(name: &str) -> bool {
    PORTS.lock().unwrap().iter().any(|p| p.name == name)
}

/// 串口线程：打开 / 关闭
pub fn open(name: &str, baud: u32) -> bool {
    let mut v = PORTS.lock().unwrap();
    match v.iter_mut().find(|p| p.name == name) {
        Some(p) => {
            p.baud = Some(baud);
            p.to_host.clear();
            true
        }
        None => false,
    }
}

pub fn close(name: &str) {
    if let Some(p) = PORTS.lock().unwrap().iter_mut().find(|p| p.name == name) {
        p.baud = None;
        p.from_host.clear();
    }
}

/// 串口线程：取出给 QCCOM 的数据；None = 端口没了（电路里删掉了）
pub fn read(name: &str) -> Option<Vec<u8>> {
    let mut v = PORTS.lock().unwrap();
    let p = v.iter_mut().find(|p| p.name == name)?;
    Some(p.to_host.drain(..).collect())
}

pub fn write(name: &str, data: &[u8]) {
    if let Some(p) = PORTS.lock().unwrap().iter_mut().find(|p| p.name == name) {
        if p.baud.is_some() {
            p.from_host.extend_from_slice(data);
        }
    }
}

/// 电路页：元件 → 打开时的波特率
pub fn baud_of(part: u32) -> Option<u32> {
    PORTS.lock().unwrap().iter().find(|p| p.part == part).and_then(|p| p.baud)
}

/// 电路页：仿真发给 QCCOM 的数据（没打开就丢掉，和真串口一样）
pub fn to_host(part: u32, data: &[u8]) {
    if let Some(p) = PORTS.lock().unwrap().iter_mut().find(|p| p.part == part && p.baud.is_some()) {
        p.to_host.extend(data);
        let n = p.to_host.len();
        if n > 1 << 20 {
            p.to_host.drain(..n - (1 << 20));
        }
    }
}

/// 电路页：QCCOM 发来的数据
pub fn from_host(part: u32) -> Vec<u8> {
    PORTS.lock().unwrap().iter_mut().find(|p| p.part == part).map(|p| std::mem::take(&mut p.from_host)).unwrap_or_default()
}

//! XMODEM / XMODEM-1K / YMODEM 发送：很多 bootloader（U-Boot loady/loadx、STM32 IAP、各种串口升级）都用它们。
//! 纯状态机：收到的字节交给 on_rx，定时调 on_tick，返回要发出去的数据。

use std::time::{Duration, Instant};

use crate::checksum::crc16_ccitt;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const C: u8 = b'C';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Proto {
    Xmodem,
    Xmodem1k,
    Ymodem,
}

impl Proto {
    pub fn name(self) -> &'static str {
        match self {
            Proto::Xmodem => "XMODEM",
            Proto::Xmodem1k => "XMODEM-1K",
            Proto::Ymodem => "YMODEM",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// 等接收方发 'C'（CRC 模式）或 NAK（累加和模式）
    Start,
    /// YMODEM：第 0 块（文件名）已发，等 ACK
    Header,
    /// YMODEM：第 0 块收到 ACK 后再等一个 'C' 才开始发数据
    HeaderC,
    Data,
    /// 已发 EOT
    Eot,
    /// YMODEM：等接收方要下一个文件，发空的第 0 块结束会话
    Finish,
    FinishAck,
    Done,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    Send(Vec<u8>),
    Done,
    Fail(String),
}

pub struct Sender {
    pub proto: Proto,
    name: String,
    data: Vec<u8>,
    /// 当前块的起始字节
    pos: usize,
    blk: u8,
    crc: bool,
    state: State,
    retries: u32,
    last: Instant,
    eots: u32,
    pub started: Instant,
}

impl Sender {
    pub fn new(proto: Proto, name: &str, data: Vec<u8>) -> Self {
        Sender {
            proto,
            name: name.to_string(),
            data,
            pos: 0,
            blk: 1,
            crc: true,
            state: State::Start,
            retries: 0,
            last: Instant::now(),
            eots: 0,
            started: Instant::now(),
        }
    }

    pub fn total(&self) -> usize {
        self.data.len()
    }

    pub fn sent(&self) -> usize {
        match self.state {
            State::Start | State::Header | State::HeaderC => 0,
            State::Data => self.pos,
            _ => self.data.len(),
        }
    }

    /// 还在等接收方开始（提示用户在设备上执行 loady / loadx 之类的命令）
    pub fn waiting(&self) -> bool {
        self.state == State::Start
    }

    fn block_size(&self) -> usize {
        if self.proto == Proto::Xmodem {
            128
        } else {
            1024
        }
    }

    fn packet(&self, blk: u8, payload: &[u8], size: usize, pad: u8) -> Vec<u8> {
        let mut p = Vec::with_capacity(size + 5);
        p.push(if size == 1024 { STX } else { SOH });
        p.push(blk);
        p.push(!blk);
        p.extend_from_slice(payload);
        p.resize(3 + size, pad);
        if self.crc {
            p.extend(crc16_ccitt(&p[3..], 0).to_be_bytes());
        } else {
            p.push(p[3..].iter().fold(0u8, |a, &b| a.wrapping_add(b)));
        }
        p
    }

    fn header(&self, empty: bool) -> Vec<u8> {
        let mut h = Vec::new();
        if !empty {
            h.extend(self.name.as_bytes());
            h.push(0);
            h.extend(self.data.len().to_string().as_bytes());
        }
        let size = if h.len() > 128 { 1024 } else { 128 };
        self.packet(0, &h, size, 0)
    }

    fn data_packet(&self) -> Vec<u8> {
        let size = self.cur_size();
        let end = (self.pos + size).min(self.data.len());
        self.packet(self.blk, &self.data[self.pos..end], size, 0x1A)
    }

    /// 最后剩得不多时用 128 字节小块，少发填充
    fn cur_size(&self) -> usize {
        let rest = self.data.len() - self.pos;
        if self.block_size() == 1024 && rest <= 128 * 7 {
            128
        } else {
            self.block_size()
        }
    }

    fn send(&mut self, d: Vec<u8>) -> Vec<Out> {
        self.last = Instant::now();
        vec![Out::Send(d)]
    }

    fn next_data(&mut self) -> Vec<Out> {
        if self.pos >= self.data.len() {
            self.state = State::Eot;
            self.eots = 1;
            return self.send(vec![EOT]);
        }
        self.state = State::Data;
        let p = self.data_packet();
        self.send(p)
    }

    fn resend(&mut self) -> Vec<Out> {
        self.retries += 1;
        if self.retries > 10 {
            self.state = State::Done;
            return vec![Out::Send(vec![CAN, CAN, CAN]), Out::Fail("too many retries".into())];
        }
        match self.state {
            State::Header => {
                let h = self.header(false);
                self.send(h)
            }
            State::Data => {
                let p = self.data_packet();
                self.send(p)
            }
            State::Eot => self.send(vec![EOT]),
            State::FinishAck => {
                let h = self.header(true);
                self.send(h)
            }
            _ => vec![],
        }
    }

    pub fn on_rx(&mut self, bytes: &[u8]) -> Vec<Out> {
        let mut out = vec![];
        for &b in bytes {
            if self.state == State::Done {
                break;
            }
            out.extend(self.on_byte(b));
        }
        out
    }

    fn on_byte(&mut self, b: u8) -> Vec<Out> {
        if b == CAN && self.state != State::Start {
            self.state = State::Done;
            return vec![Out::Fail("cancelled by receiver".into())];
        }
        match (self.state, b) {
            (State::Start, C | NAK) => {
                self.crc = b == C;
                self.retries = 0;
                if self.proto == Proto::Ymodem {
                    self.state = State::Header;
                    let h = self.header(false);
                    self.send(h)
                } else {
                    self.next_data()
                }
            }
            (State::Header, ACK) => {
                self.state = State::HeaderC;
                vec![]
            }
            (State::HeaderC, C) => {
                self.retries = 0;
                self.next_data()
            }
            (State::Header, NAK | C) => self.resend(),
            (State::Data, ACK) => {
                self.retries = 0;
                self.pos = (self.pos + self.cur_size()).min(self.data.len());
                self.blk = self.blk.wrapping_add(1);
                self.next_data()
            }
            (State::Data, NAK) => self.resend(),
            (State::Eot, ACK) => {
                if self.proto == Proto::Ymodem {
                    self.state = State::Finish;
                    vec![]
                } else {
                    self.state = State::Done;
                    vec![Out::Done]
                }
            }
            // 有的接收方第一次 EOT 回 NAK，再发一次
            (State::Eot, NAK) => {
                self.eots += 1;
                self.send(vec![EOT])
            }
            (State::Finish, C) => {
                self.state = State::FinishAck;
                let h = self.header(true);
                self.send(h)
            }
            (State::FinishAck, ACK) => {
                self.state = State::Done;
                vec![Out::Done]
            }
            (State::FinishAck, NAK) => self.resend(),
            _ => vec![],
        }
    }

    pub fn on_tick(&mut self) -> Vec<Out> {
        let idle = self.last.elapsed();
        match self.state {
            State::Start if idle > Duration::from_secs(60) => {
                self.state = State::Done;
                vec![Out::Fail("receiver did not start within 60 s".into())]
            }
            State::Header | State::Data | State::Eot | State::FinishAck if idle > Duration::from_secs(10) => self.resend(),
            // YMODEM 收尾：有的接收方收完文件不再要下一个，等 3 秒算成功
            State::Finish if idle > Duration::from_secs(3) => {
                self.state = State::Done;
                vec![Out::Done]
            }
            State::HeaderC if idle > Duration::from_secs(10) => {
                self.retries = 0;
                self.next_data()
            }
            _ => vec![],
        }
    }

    pub fn cancel(&mut self) -> Vec<u8> {
        self.state = State::Done;
        vec![CAN, CAN, CAN, CAN, CAN, 8, 8, 8, 8, 8]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(o: &[Out]) -> Vec<u8> {
        match &o[0] {
            Out::Send(d) => d.clone(),
            x => panic!("{x:?}"),
        }
    }

    #[test]
    fn xmodem_crc() {
        let data: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
        let mut s = Sender::new(Proto::Xmodem, "a.bin", data);
        let p = sent(&s.on_rx(b"C"));
        assert_eq!(p.len(), 3 + 128 + 2);
        assert_eq!(&p[..3], &[SOH, 1, 0xFE]);
        assert_eq!(crc16_ccitt(&p[3..131], 0).to_be_bytes(), [p[131], p[132]]);
        // NAK：重发同一块
        assert_eq!(sent(&s.on_rx(&[NAK])), p);
        let p2 = sent(&s.on_rx(&[ACK]));
        assert_eq!(&p2[..3], &[SOH, 2, 0xFD]);
        assert_eq!(p2[3 + 72], 0x1A); // 200 - 128 = 72 字节之后是填充
        assert_eq!(sent(&s.on_rx(&[ACK])), vec![EOT]);
        assert_eq!(s.on_rx(&[ACK]), vec![Out::Done]);
    }

    #[test]
    fn ymodem_session() {
        let mut s = Sender::new(Proto::Ymodem, "fw.bin", vec![7u8; 2000]);
        let h = sent(&s.on_rx(b"C"));
        assert_eq!(&h[..3], &[SOH, 0, 0xFF]);
        assert!(h[3..].starts_with(b"fw.bin\x002000"));
        assert!(s.on_rx(&[ACK]).is_empty());
        let d1 = sent(&s.on_rx(b"C"));
        assert_eq!(d1[0], STX);
        assert_eq!(d1.len(), 3 + 1024 + 2);
        // 剩 976 字节 > 896：还是 1K 块
        let d2 = sent(&s.on_rx(&[ACK]));
        assert_eq!(d2[0], STX);
        assert_eq!(sent(&s.on_rx(&[ACK])), vec![EOT]);
        assert_eq!(sent(&s.on_rx(&[NAK])), vec![EOT]);
        assert!(s.on_rx(&[ACK]).is_empty());
        let end = sent(&s.on_rx(b"C"));
        assert!(end[3..131].iter().all(|&b| b == 0));
        assert_eq!(s.on_rx(&[ACK]), vec![Out::Done]);
    }
}

use super::line_events::{Event, ESC};
use super::{
    traffic::{escape_controller_data, publish_host_tx, receive_host_tx, ControllerTraffic},
    MappingMessage, SerialDirection, TrafficObserver,
};
use anyhow::{ensure, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serialport::{COMPort, SerialPort};
use std::{
    io::{Read, Write},
    os::windows::io::AsRawHandle,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::mpsc;
use windows_sys::Win32::{
    Devices::Communication::{GetCommState, SetCommState, SetCommTimeouts, COMMTIMEOUTS, DCB},
    System::{Ioctl::IOCTL_SERIAL_LSRMST_INSERT, IO::DeviceIoControl},
};

pub(super) struct Port {
    com: COMPort,
    virtual_end: bool,
    initial: Vec<u8>,
}

enum UartWrite {
    Peer(Vec<u8>),
    Local(Vec<u8>),
}

async fn local_input(commands: &mut Option<mpsc::Receiver<Vec<u8>>>) -> Vec<u8> {
    if let Some(commands) = commands {
        if let Some(bytes) = commands.recv().await {
            return bytes;
        }
    }
    std::future::pending().await
}

impl Port {
    pub fn open(name: &str, virtual_end: bool) -> Result<Self> {
        ensure!(super::valid_com(name), MappingMessage::new("请选择有效 COM 口"));
        let com = serialport::new(name, 115200)
            .timeout(Duration::from_millis(15))
            .open_native()
            .map_err(|error| MappingMessage::new("打不开 {}，请检查占用和驱动").with_value(name).caused_by(error))?;
        // A fast idle read must not impose the same deadline on a full UART transmit queue.
        let io_time = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: 15,
            WriteTotalTimeoutMultiplier: 2,
            WriteTotalTimeoutConstant: 3000,
        };
        // SAFETY: the synchronous COM handle and initialized struct live for this call.
        if unsafe { SetCommTimeouts(com.as_raw_handle(), &io_time) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut initial = vec![];
        if virtual_end {
            // GET + break, modem status, remote baud and line control. The driver
            // returns the current paired settings before delivering later insertions.
            let mut request = [ESC, b'c', b'0', b'c', 0, 0, 0, 0, 0];
            request[5..].copy_from_slice(&0x0300_0702u32.to_le_bytes());
            let mut out = [0u8; 64];
            let mut used = 0;
            // SAFETY: a live synchronous port handle, non-overlapping input/output buffers.
            let ok = unsafe {
                DeviceIoControl(
                    com.as_raw_handle(),
                    IOCTL_SERIAL_LSRMST_INSERT,
                    request.as_ptr().cast(),
                    request.len() as u32,
                    out.as_mut_ptr().cast(),
                    out.len() as u32,
                    &mut used,
                    std::ptr::null_mut(),
                )
            };
            ensure!(ok != 0 && used == 17, MappingMessage::new("该端口不支持 com0com 控制线同步，请选择虚拟串口对"));
            initial.extend_from_slice(&out[..used as usize]);
        }
        Ok(Self { com, virtual_end, initial })
    }
    pub async fn bridge(mut self, connection: &Connection, tx: &mut SendStream, rx: &mut RecvStream, observer: Option<TrafficObserver>, observe_host_tx: bool) -> Result<()> {
        let mut reader = self.com.try_clone_native()?;
        let stop = Arc::new(AtomicBool::new(false));
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _stop = Stop(stop.clone());
        let (out, mut outgoing) = mpsc::channel::<Vec<u8>>(8);
        let (incoming, mut input) = mpsc::channel::<UartWrite>(8);
        let virtual_end = self.virtual_end;
        let (_attached_writer, commands) = observer.as_ref().and_then(TrafficObserver::attach_writer).unzip();
        let (mut outgoing_commands, mut incoming_commands) = if virtual_end { (commands, None) } else { (None, commands) };
        let (host_tap, _tap_task) = match (observe_host_tx, virtual_end, observer.as_ref()) {
            (true, true, Some(observer)) => (None, Some(receive_host_tx(connection.clone(), observer.clone()))),
            (true, false, Some(_)) => {
                let (sender, tap) = publish_host_tx(connection.clone());
                (Some(sender), Some(tap))
            }
            _ => (None, None),
        };
        let read_stop = stop.clone();
        let read = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut buf = [0; 8192];
            while !read_stop.load(Ordering::Acquire) {
                match reader.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        if out.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        });
        let write_observer = observer.clone();
        let write = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut controller = ControllerTraffic::default();
            let mut pins = None;
            let mut last_break = None;
            while let Some(message) = input.blocking_recv() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let bytes = match message {
                    UartWrite::Local(bytes) => {
                        self.com.write_all(&bytes)?;
                        if let Some(observer) = &write_observer {
                            observer.record(SerialDirection::Tx, &bytes);
                        }
                        if let Some(tap) = &host_tap {
                            let _ = tap.try_send(bytes);
                        }
                        continue;
                    }
                    UartWrite::Peer(bytes) if virtual_end => {
                        self.com.write_all(&bytes)?;
                        if let Some(observer) = &write_observer {
                            observer.record(SerialDirection::Rx, &bytes);
                        }
                        continue;
                    }
                    UartWrite::Peer(bytes) => bytes,
                };
                controller.feed(&bytes, write_observer.as_ref(), |event| {
                    match event {
                        Event::Data(b) => self.com.write_all(b)?,
                        Event::Baud(baud) => {
                            self.com.flush()?;
                            self.com.set_baud_rate(baud)?;
                        }
                        Event::Format(bits, parity, stops) => {
                            ensure!((5..=8).contains(&bits) && parity <= 4 && stops <= 2, MappingMessage::new("串口格式无效"));
                            self.com.flush()?;
                            let mut dcb = DCB {
                                DCBlength: std::mem::size_of::<DCB>() as u32,
                                ..Default::default()
                            };
                            // SAFETY: DCB has the Win32 layout, the handle is owned by com.
                            unsafe {
                                if GetCommState(self.com.as_raw_handle(), &mut dcb) == 0 {
                                    return Err(std::io::Error::last_os_error().into());
                                }
                                dcb.ByteSize = bits;
                                dcb.Parity = parity;
                                dcb.StopBits = stops;
                                dcb._bitfield = (dcb._bitfield & !2) | (u32::from(parity != 0) << 1);
                                if SetCommState(self.com.as_raw_handle(), &dcb) == 0 {
                                    return Err(std::io::Error::last_os_error().into());
                                }
                            }
                        }
                        Event::Pins(bits) => {
                            // com0com null modem: local CTS mirrors peer RTS, DSR mirrors peer DTR.
                            if pins.map(|p: u8| p & 0x10) != Some(bits & 0x10) {
                                self.com.write_request_to_send(bits & 0x10 != 0)?;
                            }
                            if pins.map(|p: u8| p & 0x20) != Some(bits & 0x20) {
                                self.com.write_data_terminal_ready(bits & 0x20 != 0)?;
                            }
                            pins = Some(bits);
                        }
                        Event::Break(on) => {
                            if last_break != Some(on) {
                                if on {
                                    self.com.set_break()?;
                                } else {
                                    self.com.clear_break()?;
                                }
                                last_break = Some(on);
                            }
                        }
                    }
                    Ok(())
                })?;
            }
            Ok(())
        });
        let send = async {
            tx.write_all(&self.initial).await?;
            let mut controller = ControllerTraffic::default();
            let mut escaped = Vec::new();
            loop {
                tokio::select! {
                    bytes = outgoing.recv() => {
                        let Some(bytes) = bytes else { break };
                        tx.write_all(&bytes).await?;
                        if let Some(observer) = &observer {
                            if virtual_end {
                                if controller.feed(&bytes, Some(observer), |_| Ok(())).is_err() {
                                    // Decoding here is for observation only. The host applies the actual controls.
                                    controller = ControllerTraffic::default();
                                }
                            } else {
                                observer.record(SerialDirection::Rx, &bytes);
                            }
                        }
                    }
                    bytes = local_input(&mut outgoing_commands), if controller.is_between_events() => {
                        // A partial com0com record must finish before inserting a UI write.
                        escape_controller_data(&bytes, &mut escaped);
                        tx.write_all(&escaped).await?;
                        if let Some(observer) = &observer {
                            observer.record(SerialDirection::Tx, &bytes);
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let receive = async {
            let mut buf = [0; 8192];
            loop {
                let input = tokio::select! {
                    result = rx.read(&mut buf) => {
                        let Some(n) = result? else { break };
                        UartWrite::Peer(buf[..n].to_vec())
                    }
                    bytes = local_input(&mut incoming_commands) => UartWrite::Local(bytes),
                };
                incoming.send(input).await.map_err(|error| MappingMessage::new("串口已关闭").caused_by(error))?;
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {r=read=>r?,r=write=>r?,r=send=>r,r=receive=>r}
    }
}

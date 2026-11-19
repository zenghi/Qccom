use super::line_events::{Decoder, Event, ESC};
use anyhow::{ensure, Context, Result};
use iroh::endpoint::{RecvStream, SendStream};
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
impl Port {
    pub fn open(name: &str, virtual_end: bool) -> Result<Self> {
        ensure!(super::valid_com(name), "invalid COM port");
        let com = serialport::new(name, 115200)
            .timeout(Duration::from_millis(15))
            .open_native()
            .with_context(|| format!("打不开 {name}，请检查占用和驱动"))?;
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
            ensure!(ok != 0 && used == 17, "该端口不支持 com0com 控制线同步，请选择虚拟串口对");
            initial.extend_from_slice(&out[..used as usize]);
        }
        Ok(Self { com, virtual_end, initial })
    }
    pub async fn bridge(mut self, tx: &mut SendStream, rx: &mut RecvStream) -> Result<()> {
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
        let (incoming, mut input) = mpsc::channel::<Vec<u8>>(8);
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
        let virtual_end = self.virtual_end;
        let write = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut decoder = Decoder::default();
            let mut pins = None;
            let mut last_break = None;
            while let Some(bytes) = input.blocking_recv() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                if virtual_end {
                    self.com.write_all(&bytes)?;
                    continue;
                }
                decoder.feed(&bytes, |event| {
                    match event {
                        Event::Data(b) => self.com.write_all(b)?,
                        Event::Baud(baud) => {
                            self.com.flush()?;
                            self.com.set_baud_rate(baud)?;
                        }
                        Event::Format(bits, parity, stops) => {
                            ensure!((5..=8).contains(&bits) && parity <= 4 && stops <= 2, "串口格式无效");
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
            while let Some(b) = outgoing.recv().await {
                tx.write_all(&b).await?;
            }
            Ok::<_, anyhow::Error>(())
        };
        let receive = async {
            let mut buf = [0; 8192];
            while let Some(n) = rx.read(&mut buf).await? {
                incoming.send(buf[..n].to_vec()).await.context("串口已关闭")?;
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {r=read=>r?,r=write=>r?,r=send=>r,r=receive=>r}
    }
}

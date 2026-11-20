use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

#[cfg(any(windows, test))]
const LOG_CHUNK: usize = 8 * 1024;
const LOG_QUEUE: usize = 32;
#[cfg(any(windows, test))]
const WRITE_QUEUE: usize = 8;
const WRITE_LIMIT: usize = 64 * 1024;

/// Direction as seen by the controller. It is the same on both devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialDirection {
    Tx,
    Rx,
}

#[derive(Debug)]
pub struct SerialTraffic {
    pub direction: SerialDirection,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialWriteError {
    Offline,
    Full,
    TooLarge,
}

type ActiveWriter = Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>;

/// Nonblocking input for the currently attached UART bridge.
#[derive(Clone)]
pub struct SerialWriter(ActiveWriter);

impl SerialWriter {
    pub fn try_send(&self, bytes: Vec<u8>) -> Result<(), SerialWriteError> {
        if bytes.len() > WRITE_LIMIT {
            return Err(SerialWriteError::TooLarge);
        }
        let writer = self.0.lock().map_err(|_| SerialWriteError::Offline)?;
        let sender = writer.as_ref().ok_or(SerialWriteError::Offline)?;
        sender.try_send(bytes).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SerialWriteError::Full,
            mpsc::error::TrySendError::Closed(_) => SerialWriteError::Offline,
        })
    }
}

/// Best-effort output for COM mappings. A slow observer cannot block forwarding.
#[derive(Clone)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub struct TrafficObserver {
    log: mpsc::Sender<SerialTraffic>,
    writer: ActiveWriter,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl TrafficObserver {
    pub fn channel(wake: impl Fn() + Send + Sync + 'static) -> (Self, mpsc::Receiver<SerialTraffic>, SerialWriter) {
        let (log, received) = mpsc::channel(LOG_QUEUE);
        let writer = ActiveWriter::default();
        let input = SerialWriter(writer.clone());
        (Self { log, writer, wake: Arc::new(wake) }, received, input)
    }
}

#[cfg(any(windows, test))]
mod uart_traffic {
    use iroh::endpoint::Connection;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::mapping::line_events::{Decoder, Event, ESC};

    /// The bridge owns the queue. Dropping it disconnects stale input before a retry.
    pub(in crate::mapping) struct AttachedWriter(ActiveWriter);

    impl Drop for AttachedWriter {
        fn drop(&mut self) {
            if let Ok(mut writer) = self.0.lock() {
                writer.take();
            }
        }
    }

    impl TrafficObserver {
        pub(in crate::mapping) fn attach_writer(&self) -> Option<(AttachedWriter, mpsc::Receiver<Vec<u8>>)> {
            let mut writer = self.writer.lock().ok()?;
            if writer.is_some() {
                return None;
            }
            let (tx, rx) = mpsc::channel(WRITE_QUEUE);
            *writer = Some(tx);
            Some((AttachedWriter(self.writer.clone()), rx))
        }

        pub(in crate::mapping) fn record(&self, direction: SerialDirection, bytes: &[u8]) {
            let mut queued = false;
            for bytes in bytes.chunks(LOG_CHUNK) {
                let Ok(permit) = self.log.try_reserve() else { break };
                permit.send(SerialTraffic { direction, bytes: bytes.to_vec() });
                queued = true;
            }
            if queued {
                (self.wake)();
            }
        }
    }

    /// The controller-to-device wire includes escaped data and in-band UART controls.
    #[derive(Default)]
    pub(in crate::mapping) struct ControllerTraffic {
        decoder: Decoder,
        captured: Vec<u8>,
    }

    impl ControllerTraffic {
        pub(in crate::mapping) fn is_between_events(&self) -> bool {
            self.decoder.is_between_events()
        }

        pub(in crate::mapping) fn feed(&mut self, bytes: &[u8], observer: Option<&TrafficObserver>, mut forward: impl FnMut(Event<'_>) -> anyhow::Result<()>) -> anyhow::Result<()> {
            let Some(observer) = observer else {
                return self.decoder.feed(bytes, forward);
            };
            // A full log queue must not interrupt forwarding or lose decoder state.
            let Ok(permit) = observer.log.try_reserve() else {
                return self.decoder.feed(bytes, forward);
            };
            self.captured.clear();
            let captured = &mut self.captured;
            let result = self.decoder.feed(bytes, |event| {
                let data = match &event {
                    Event::Data(bytes) => Some(*bytes),
                    _ => None,
                };
                forward(event)?;
                if let Some(bytes) = data {
                    captured.extend_from_slice(bytes);
                }
                Ok(())
            });
            let mut chunks = captured.chunks(LOG_CHUNK);
            if let Some(first) = chunks.next() {
                permit.send(SerialTraffic {
                    direction: SerialDirection::Tx,
                    bytes: first.to_vec(),
                });
                (observer.wake)();
                for chunk in chunks {
                    observer.record(SerialDirection::Tx, chunk);
                }
            }
            result
        }
    }

    pub(in crate::mapping) fn escape_controller_data(bytes: &[u8], out: &mut Vec<u8>) {
        out.clear();
        for &byte in bytes {
            out.push(byte);
            if byte == ESC {
                out.push(0);
            }
        }
    }

    pub(in crate::mapping) struct TrafficTask(JoinHandle<anyhow::Result<()>>);

    impl Drop for TrafficTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    /// The device's injected TX is observation only. It never enters virtual-port RX.
    pub(in crate::mapping) fn publish_host_tx(conn: Connection) -> (mpsc::Sender<Vec<u8>>, TrafficTask) {
        let (tx, mut received) = mpsc::channel::<Vec<u8>>(WRITE_QUEUE);
        let task = tokio::spawn(async move {
            let mut stream = conn.open_uni().await?;
            while let Some(bytes) = received.recv().await {
                stream.write_all(&bytes).await?;
            }
            stream.finish()?;
            Ok(())
        });
        (tx, TrafficTask(task))
    }

    pub(in crate::mapping) fn receive_host_tx(conn: Connection, observer: TrafficObserver) -> TrafficTask {
        TrafficTask(tokio::spawn(async move {
            let mut stream = conn.accept_uni().await?;
            let mut buffer = [0; LOG_CHUNK];
            while let Some(n) = stream.read(&mut buffer).await? {
                observer.record(SerialDirection::Tx, &buffer[..n]);
            }
            Ok(())
        }))
    }
}

#[cfg(any(windows, test))]
pub(super) use uart_traffic::*;

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn records_only_forwarded_uart_payload_and_bounds_the_log() {
        let (observer, mut log, _) = TrafficObserver::channel(|| {});
        let mut data = ControllerTraffic::default();
        data.feed(b"A\xff\x10\0\xc2", Some(&observer), |_| Ok(())).unwrap();
        assert!(!data.is_between_events());
        data.feed(b"\x01\0\xff\0B", Some(&observer), |_| Ok(())).unwrap();
        assert!(data.is_between_events());
        observer.record(SerialDirection::Rx, b"C\xff");
        for (direction, bytes) in [(SerialDirection::Tx, b"A".as_slice()), (SerialDirection::Tx, b"\xffB"), (SerialDirection::Rx, b"C\xff")] {
            let row = log.try_recv().unwrap();
            assert_eq!((row.direction, row.bytes.as_slice()), (direction, bytes));
        }
        let failed = data.feed(b"\xff\0D", Some(&observer), |event| match event {
            crate::mapping::line_events::Event::Data(b"D") => anyhow::bail!("UART write error"),
            _ => Ok(()),
        });
        assert!(failed.is_err());
        assert_eq!(log.try_recv().unwrap().bytes, b"\xff");
        for _ in 0..LOG_QUEUE + 1 {
            observer.record(SerialDirection::Rx, b"filled");
        }
        assert_eq!(log.len(), LOG_QUEUE);
    }

    #[test]
    fn writer_has_no_queue_between_bridges() {
        let (observer, _, writer) = TrafficObserver::channel(|| {});
        assert_eq!(writer.try_send(vec![1]), Err(SerialWriteError::Offline));
        let (bridge, mut received) = observer.attach_writer().unwrap();
        for _ in 0..WRITE_QUEUE {
            writer.try_send(vec![0xff]).unwrap();
        }
        assert_eq!(writer.try_send(vec![1]), Err(SerialWriteError::Full));
        assert_eq!(writer.try_send(vec![1; WRITE_LIMIT + 1]), Err(SerialWriteError::TooLarge));
        let mut wire = Vec::new();
        escape_controller_data(&received.try_recv().unwrap(), &mut wire);
        assert_eq!(wire, [0xff, 0]);
        drop(bridge);
        assert_eq!(writer.try_send(vec![1]), Err(SerialWriteError::Offline));
        let (_bridge, mut next) = observer.attach_writer().unwrap();
        assert!(next.try_recv().is_err());
    }

    #[test]
    fn legacy_com_handshake_does_not_enable_the_optional_tx_log() -> anyhow::Result<()> {
        use crate::mapping::{Hello, Reply};
        use serde_json::{from_value, json, to_value};

        let hello = json!({ "service": "Com", "number": 7001, "label": "COM21", "proof": "proof" });
        let reply = json!({ "label": "COM5", "passive": [50000, 50009] });
        let old_hello: Hello = from_value(hello.clone())?;
        let old_reply: Reply = from_value(reply.clone())?;
        assert!(!old_hello.observe_host_tx && !old_reply.observe_host_tx);
        assert_eq!(to_value(old_hello)?, hello);
        assert_eq!(to_value(old_reply)?, reply);
        Ok(())
    }

    #[tokio::test]
    async fn host_local_tx_log_stays_outside_the_uart_data_stream() -> anyhow::Result<()> {
        use anyhow::Context;
        use iroh::endpoint::presets;
        use iroh::{Endpoint, RelayMode};
        use std::net::Ipv4Addr;
        use std::time::Duration;

        const ALPN: &[u8] = b"qccom/uart-observer-test";
        let endpoint = |server| {
            let mut builder = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .clear_ip_transports()
                .bind_addr((Ipv4Addr::LOCALHOST, 0))?;
            if server {
                builder = builder.alpns(vec![ALPN.to_vec()]);
            }
            Ok::<_, anyhow::Error>(builder)
        };
        let host = endpoint(true)?.bind().await?;
        let client = endpoint(false)?.bind().await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let (client_conn, host_conn) = tokio::try_join!(async { Ok::<_, anyhow::Error>(client.connect(host.addr(), ALPN).await?) }, async {
                Ok::<_, anyhow::Error>(host.accept().await.context("host stopped accepting connections")?.await?)
            },)?;
            assert_serial_traffic(host_conn, client_conn).await
        })
        .await??;
        Ok(())
    }

    pub(in crate::mapping) async fn assert_serial_traffic(host_conn: iroh::endpoint::Connection, client_conn: iroh::endpoint::Connection) -> anyhow::Result<()> {
        use anyhow::Context;

        let (mut client_tx, mut client_rx) = client_conn.open_bi().await?;
        let original = b"user\xffwrite";
        let mut escaped = Vec::new();
        escape_controller_data(original, &mut escaped);
        client_tx.write_all(&escaped).await?;
        let (mut host_tx, mut host_rx) = host_conn.accept_bi().await?;
        let mut forwarded = vec![0; escaped.len()];
        host_rx.read_exact(&mut forwarded).await?;
        let (host_log, mut host_events, _) = TrafficObserver::channel(|| {});
        let mut actual_uart_tx = Vec::new();
        ControllerTraffic::default().feed(&forwarded, Some(&host_log), |event| {
            if let crate::mapping::line_events::Event::Data(bytes) = event {
                actual_uart_tx.extend_from_slice(bytes);
            }
            Ok(())
        })?;
        assert_eq!(&actual_uart_tx, original);
        let entry = host_events.recv().await.context("missing host TX log")?;
        assert_eq!(entry.direction, SerialDirection::Tx);
        assert_eq!(&entry.bytes, original);

        let (client_log, mut client_events, _) = TrafficObserver::channel(|| {});
        let (tap, _tap_task) = publish_host_tx(host_conn);
        let _receiver_task = receive_host_tx(client_conn, client_log);
        tap.try_send(b"local\xffwrite".to_vec())?;
        host_tx.write_all(b"device\xffresponse").await?;
        host_tx.finish()?;
        assert_eq!(client_rx.read_to_end(64).await?, b"device\xffresponse");
        let mut observed = Vec::new();
        while observed.len() < b"local\xffwrite".len() {
            let entry = client_events.recv().await.context("missing client TX log")?;
            assert_eq!(entry.direction, SerialDirection::Tx);
            observed.extend(entry.bytes);
        }
        assert_eq!(observed, b"local\xffwrite");
        Ok(())
    }
}

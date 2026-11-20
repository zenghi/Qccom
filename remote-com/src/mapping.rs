//! A single authenticated endpoint carries independent COM, MQTT and passive FTP mappings.
use crate::sharing::{now_unix, RelayVoucher, ShareInvitation, ShareService, SHARE_RELAY_TOKEN};
use anyhow::{bail, ensure, Context, Result};
use iroh::{
    endpoint::{presets, Connection, RecvStream, SendStream},
    Endpoint, EndpointAddr, RelayMap, RelayMode,
};
use iroh_base::{RelayUrl, SecretKey};
use iroh_relay::tls::CaTlsConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
    time::{sleep, timeout, Instant},
};
mod error;
mod ftp;
pub use error::MappingMessage;
#[cfg(any(windows, test))]
mod line_events;
mod relay_check;
mod relay_dns;
pub use relay_check::check_share_server;
#[cfg(all(test, feature = "server"))]
mod share_tests;
mod traffic;
#[cfg(windows)]
mod uart;
pub use traffic::{SerialDirection, SerialTraffic, SerialWriteError, SerialWriter, TrafficObserver};

const SHARE_ALPN: &[u8] = b"qccom/map/2";
const HANDSHAKE: Duration = Duration::from_secs(20);
const DIRECT_WAIT: Duration = Duration::from_secs(5);
pub const DEFAULT_SERVER: &str = "www.zenghi.com";
const DEFAULT_RELAY_PORT: u16 = 7000;
pub const DEFAULT_PASSIVE: (u16, u16) = (50000, 50009);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Service {
    Com,
    Mqtt,
    Ftp,
}
impl Service {
    pub fn port(self) -> u16 {
        match self {
            Self::Com => 7001,
            Self::Mqtt => 1883,
            Self::Ftp => 21,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Com => "COM",
            Self::Mqtt => "MQTT",
            Self::Ftp => "FTP",
        }
    }
}

impl From<ShareService> for Service {
    fn from(service: ShareService) -> Self {
        match service {
            ShareService::Com => Self::Com,
            ShareService::Mqtt => Self::Mqtt,
            ShareService::Ftp => Self::Ftp,
        }
    }
}

impl From<Service> for ShareService {
    fn from(service: Service) -> Self {
        match service {
            Service::Com => Self::Com,
            Service::Mqtt => Self::Mqtt,
            Service::Ftp => Self::Ftp,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub service: Service,
    /// Logical service number on this authorized pair, no public TCP listener.
    pub number: u16,
    /// Explicit publisher target. Consumers always listen on loopback.
    pub host: String,
    pub port: u16,
    pub passive: (u16, u16),
    /// Driver-facing port, and the label visible to the user (the opposite end on the flashing PC).
    pub serial: String,
    pub label: String,
}

impl Mapping {
    pub fn new(service: Service) -> Self {
        Self {
            service,
            number: service.port(),
            host: "127.0.0.1".into(),
            port: service.port(),
            passive: DEFAULT_PASSIVE,
            serial: String::new(),
            label: String::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.number > 0, MappingMessage::new("映射端口必须大于零"));
        if self.service == Service::Com {
            for s in [&self.serial, &self.label] {
                ensure!(valid_com(s), MappingMessage::new("请选择有效 COM 口"));
            }
        } else {
            ensure!(!self.host.trim().is_empty() && self.port > 0, MappingMessage::new("请填写服务地址和端口"));
        }
        if self.service == Service::Ftp {
            validate_range(self.passive)?;
        }
        Ok(())
    }
    fn name(&self, hosting: bool) -> String {
        if self.service == Service::Com {
            self.label.clone()
        } else if hosting {
            format!("{}:{}", self.host, self.port)
        } else {
            format!("127.0.0.1:{}", self.port)
        }
    }
}

pub fn valid_com(name: &str) -> bool {
    name.strip_prefix("COM").and_then(|n| n.parse::<u16>().ok()).is_some_and(|n| n > 0 && name == format!("COM{n}"))
}
fn validate_range((lo, hi): (u16, u16)) -> Result<()> {
    ensure!(lo >= 1024 && hi >= lo && hi - lo < 64, MappingMessage::new("FTP 被动端口范围须在 1024–65535 之间，最多 64 个端口"));
    Ok(())
}
fn share_relay(server: &str) -> Result<RelayUrl> {
    let server = server.trim();
    let server = server.strip_prefix("https://").map_or(server, |url| url.trim_end_matches('/'));
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().map_err(|error| MappingMessage::new("请输入有效的服务器端口").caused_by(error))?),
        None => (server, DEFAULT_RELAY_PORT),
    };
    ensure!(
        !host.is_empty() && host.len() <= 253 && host.bytes().all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c)) && port != 0,
        MappingMessage::new("请输入服务器域名或 IPv4 地址")
    );
    Ok(format!("https://{host}:{port}").parse()?)
}

#[derive(Debug, Clone)]
pub enum State {
    Registering,
    Waiting,
    Punching,
    Handshaking,
    Online { direct: bool, peer: String },
    ServiceError(Arc<anyhow::Error>),
    Failed(Arc<anyhow::Error>),
    Expired,
}
pub type Notify = Arc<dyn Fn(State) + Send + Sync>;

#[derive(Serialize, Deserialize)]
struct Hello {
    service: Service,
    number: u16,
    label: String,
    proof: String,
    #[serde(default, skip_serializing_if = "flag_disabled")]
    observe_host_tx: bool,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    label: String,
    passive: (u16, u16),
    #[serde(default, skip_serializing_if = "flag_disabled")]
    observe_host_tx: bool,
}
fn flag_disabled(flag: &bool) -> bool {
    !flag
}

struct Session {
    conn: Connection,
    tx: SendStream,
    rx: RecvStream,
    label: String,
    observe_host_tx: bool,
}
type Key = (Service, u16);
type Routes = Arc<Mutex<HashMap<Key, Route>>>;

#[derive(Clone)]
struct Route {
    sessions: mpsc::Sender<Session>,
    grant: Arc<RelayVoucher>,
}

impl Route {
    fn permits(&self, proof: &str) -> bool {
        now_unix().is_ok_and(|now| now < self.grant.expires_at()) && self.grant.verify_proof(proof).is_ok()
    }
}

enum Pairing {
    ShareHost(RelayUrl),
    ShareClient(ShareInvitation),
}

pub struct Node {
    endpoint: Endpoint,
    pairing: Pairing,
    routes: Routes,
    accept: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Node {
    fn drop(&mut self) {
        if let Some(accept) = &self.accept {
            accept.abort();
        }
    }
}

struct CloseMapping(Connection);
impl Drop for CloseMapping {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"mapping stopped");
    }
}

async fn send_json<T: Serialize>(tx: &mut SendStream, data: &T) -> Result<()> {
    let data = serde_json::to_vec(data)?;
    ensure!(data.len() <= 1024, "mapping header too large");
    tx.write_all(&(data.len() as u16).to_be_bytes()).await?;
    tx.write_all(&data).await?;
    Ok(())
}
async fn read_json<T: serde::de::DeserializeOwned>(rx: &mut RecvStream) -> Result<T> {
    let mut len = [0; 2];
    rx.read_exact(&mut len).await?;
    let len = u16::from_be_bytes(len) as usize;
    ensure!(len <= 1024, "mapping header too large");
    let mut data = vec![0; len];
    rx.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}
impl Node {
    pub async fn bind_share_host(server: &str, relay_only: bool) -> Result<Self> {
        let relay = share_relay(server)?;
        Self::bind_shared(Pairing::ShareHost(relay), CaTlsConfig::embedded(), relay_only).await
    }

    pub async fn bind_share_client(invite: ShareInvitation, relay_only: bool) -> Result<Self> {
        Self::bind_shared(Pairing::ShareClient(invite), CaTlsConfig::embedded(), relay_only).await
    }

    async fn bind_shared(pairing: Pairing, tls: CaTlsConfig, relay_only: bool) -> Result<Self> {
        let (relay, token) = match &pairing {
            Pairing::ShareHost(relay) => (relay, SHARE_RELAY_TOKEN),
            Pairing::ShareClient(invite) => (invite.relay(), invite.voucher().token()),
        };
        let relays = RelayMap::from(relay.clone()).with_auth_token(token);
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .ca_tls_config(tls.clone())
            .dns_resolver(relay_dns::relay_dns())
            .relay_mode(RelayMode::Custom(relays));
        if relay_only {
            builder = builder.clear_ip_transports();
        }
        let hosting = matches!(&pairing, Pairing::ShareHost(_));
        if hosting {
            builder = builder.alpns(vec![SHARE_ALPN.to_vec()]);
        }
        let endpoint = timeout(HANDSHAKE, builder.bind())
            .await
            .map_err(|error| MappingMessage::new("本机穿透端点启动超时").caused_by(error))??;
        if let Err(error) = wait_for_relay(&endpoint, relay).await {
            relay_check::probe_relay(relay.clone(), tls, token).await?;
            return Err(error);
        }
        let routes = Routes::default();
        let accept = hosting.then(|| tokio::spawn(accept_sessions(endpoint.clone(), routes.clone())));
        Ok(Self { endpoint, pairing, routes, accept })
    }

    pub async fn share(&self, map: Mapping, notify: Notify, publish: impl FnOnce(String) + Send) -> Result<()> {
        self.share_observed(map, notify, None, publish).await
    }

    pub async fn share_observed(&self, map: Mapping, notify: Notify, observer: Option<TrafficObserver>, publish: impl FnOnce(String) + Send) -> Result<()> {
        map.validate()?;
        let Pairing::ShareHost(relay) = &self.pairing else {
            bail!(MappingMessage::new("共享需要使用分享端连接"));
        };
        wait_for_relay(&self.endpoint, relay).await?;
        let invite = ShareInvitation::issue(relay.clone(), self.endpoint.secret_key(), map.service.into(), map.number)?;
        let grant = invite.voucher().clone();
        self.run_host(map, notify, Arc::new(grant), observer, || publish(invite.address())).await
    }

    pub async fn run(&self, map: Mapping, notify: Notify) -> Result<()> {
        self.run_observed(map, notify, None).await
    }

    pub async fn run_observed(&self, map: Mapping, notify: Notify, observer: Option<TrafficObserver>) -> Result<()> {
        map.validate()?;
        let Pairing::ShareClient(invite) = &self.pairing else {
            bail!(MappingMessage::new("请先生成分享地址"));
        };
        let voucher = invite.voucher();
        ensure!(
            Service::from(voucher.service()) == map.service && voucher.number() == map.number,
            MappingMessage::new("连接地址与映射不匹配")
        );
        let peer = EndpointAddr::new(voucher.host()).with_relay_url(invite.relay().clone());
        self.run_client(map, notify, peer, invite.proof().to_owned(), observer).await
    }

    async fn run_host(&self, map: Mapping, notify: Notify, grant: Arc<RelayVoucher>, observer: Option<TrafficObserver>, started: impl FnOnce()) -> Result<()> {
        let key = (map.service, map.number);
        let expires_at = grant.expires_at();
        let (tx, mut sessions) = mpsc::channel(1);
        {
            let mut routes = self.routes.lock().unwrap();
            ensure!(!routes.contains_key(&key), MappingMessage::new("映射端口已在使用"));
            routes.insert(key, Route { sessions: tx, grant });
        }
        struct Registration<'a>(&'a Routes, Key);
        impl Drop for Registration<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap().remove(&self.1);
            }
        }
        let _registration = Registration(&self.routes, key);
        started();
        notify(State::Waiting);
        loop {
            let remaining = Duration::from_secs(expires_at.saturating_sub(now_unix()?));
            let session = match timeout(remaining, sessions.recv()).await {
                Ok(session) => session.ok_or_else(|| MappingMessage::new("连接已关闭"))?,
                Err(_) => {
                    notify(State::Expired);
                    return Ok(());
                }
            };
            notify(State::Handshaking);
            let run = serve_mapping(&map, session, &notify, observer.as_ref());
            tokio::pin!(run);
            loop {
                tokio::select! {
                    result = &mut run => {
                        notify(match result {
                            Ok(()) => State::Waiting,
                            Err(error) => State::ServiceError(Arc::new(error)),
                        });
                        break;
                    }
                    Some(extra)=sessions.recv()=>extra.conn.close(1u32.into(),b"mapping busy"),
                }
            }
        }
    }

    async fn run_client(&self, map: Mapping, notify: Notify, peer: EndpointAddr, proof: String, observer: Option<TrafficObserver>) -> Result<()> {
        notify(State::Punching);
        let conn = timeout(HANDSHAKE, self.endpoint.connect(peer, SHARE_ALPN))
            .await
            .map_err(|error| MappingMessage::new("设备端未响应").caused_by(error))??;
        let _close = CloseMapping(conn.clone());
        // Iroh may use the relay for discovery. Hold application bytes until a selected
        // IP path exists, or the direct-path budget expires.
        let until = Instant::now() + DIRECT_WAIT;
        while !direct(&conn) && Instant::now() < until {
            tokio::select! { err=conn.closed()=>bail!("{err}"), _=sleep(Duration::from_millis(80))=>{} }
        }
        notify(State::Handshaking);
        #[cfg(windows)]
        let serial = if map.service == Service::Com { Some(uart::Port::open(&map.serial, true)?) } else { None };
        #[cfg(not(windows))]
        ensure!(map.service != Service::Com, MappingMessage::new("COM 映射需要 Windows"));
        #[allow(unused_mut)]
        let (mut tx, mut rx, reply) = timeout(HANDSHAKE, async {
            let (mut tx, mut rx) = conn.open_bi().await?;
            send_json(
                &mut tx,
                &Hello {
                    service: map.service,
                    number: map.number,
                    label: map.name(false),
                    proof,
                    observe_host_tx: map.service == Service::Com && observer.is_some(),
                },
            )
            .await?;
            let reply: Reply = read_json(&mut rx).await?;
            Ok::<_, anyhow::Error>((tx, rx, reply))
        })
        .await
        .map_err(|error| MappingMessage::new("映射握手超时").caused_by(error))??;
        #[cfg(windows)]
        if let Some(port) = serial {
            return monitor(&conn, &reply.label, &notify, port.bridge(&conn, &mut tx, &mut rx, observer, reply.observe_host_tx)).await;
        }
        // Keep the control stream alive for the lifetime of its mapped connection.
        let _control = (tx, rx);
        let listeners = bind_listeners(&map, reply.passive).await?;
        monitor(&conn, &reply.label, &notify, client_tcp(&conn, listeners, notify.clone(), &reply.label)).await
    }
}

async fn wait_for_relay(endpoint: &Endpoint, relay: &RelayUrl) -> Result<()> {
    // Only report the address after the relay has admitted this endpoint.
    timeout(HANDSHAKE, endpoint.online()).await.map_err(|error| {
        MappingMessage::new("连接穿透服务器 {} 超时，请检查服务器地址、TLS 证书及 TCP 端口")
            .with_value(relay)
            .caused_by(error)
            .into()
    })
}

async fn accept_sessions(endpoint: Endpoint, routes: Routes) {
    let mut pending = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = pending.join_next() => {},
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                if pending.len() >= 32 {
                    incoming.refuse();
                    continue;
                }
                let routes = routes.clone();
                pending.spawn(timeout(HANDSHAKE, async move {
                    let conn = incoming.await?;
                    let (tx, mut rx) = conn.accept_bi().await?;
                    let hello: Hello = read_json(&mut rx).await?;
                    ensure!(hello.label.len() <= 256 && !hello.label.chars().any(char::is_control), "invalid label");
                    let route = routes.lock().unwrap().get(&(hello.service, hello.number)).cloned();
                    let Some(route) = route.filter(|route| route.permits(&hello.proof)) else {
                        conn.close(1u32.into(), b"mapping unavailable");
                        return Ok(());
                    };
                    let session = Session { conn, tx, rx, label: hello.label, observe_host_tx: hello.observe_host_tx };
                    if let Err(error) = route.sessions.try_send(session) {
                        error.into_inner().conn.close(1u32.into(), b"mapping busy");
                    }
                    Ok::<_, anyhow::Error>(())
                }));
            }
        }
    }
}
fn direct(conn: &Connection) -> bool {
    conn.paths().iter().any(|p| p.is_selected() && p.is_ip())
}
async fn monitor(conn: &Connection, label: &str, notify: &Notify, run: impl Future<Output = Result<()>>) -> Result<()> {
    tokio::pin!(run);
    let mut last = None;
    let mut tick = tokio::time::interval(Duration::from_millis(300));
    loop {
        tokio::select! {
            biased;
            r=&mut run=>return r,
            err=conn.closed()=>bail!(MappingMessage::new("对端已断开").caused_by(err)),
            _=tick.tick()=>{let now=direct(conn); if last!=Some(now) {notify(State::Online{direct:now,peer:label.to_owned()});last=Some(now);}}
        }
    }
}
async fn serve_mapping(map: &Mapping, session: Session, notify: &Notify, _observer: Option<&TrafficObserver>) -> Result<()> {
    let Session {
        conn,
        mut tx,
        rx,
        label,
        observe_host_tx: _observe_host_tx,
    } = session;
    let _close = CloseMapping(conn.clone());
    if map.service == Service::Com {
        // Open the physical device before acknowledging a usable mapping.
        #[cfg(windows)]
        {
            let mut rx = rx;
            let serial = uart::Port::open(&map.serial, false).inspect_err(|_| conn.close(1u32.into(), b"physical COM unavailable or busy"))?;
            let observe_host_tx = _observe_host_tx && _observer.is_some();
            send_json(
                &mut tx,
                &Reply {
                    label: map.label.clone(),
                    passive: map.passive,
                    observe_host_tx,
                },
            )
            .await?;
            return monitor(&conn, &label, notify, serial.bridge(&conn, &mut tx, &mut rx, _observer.cloned(), observe_host_tx)).await;
        }
        #[cfg(not(windows))]
        bail!(MappingMessage::new("COM 映射需要 Windows"));
    }
    let _control_rx = rx;
    send_json(
        &mut tx,
        &Reply {
            label: map.name(true),
            passive: map.passive,
            observe_host_tx: false,
        },
    )
    .await?;
    let mut streams = JoinSet::new();
    let service = async {
        loop {
            tokio::select! {
                Some(result) = streams.join_next() => report_stream(result, &conn, &label, notify),
                stream = conn.accept_bi(), if streams.len() < 128 => {
                    let (send, recv) = stream?;
                    streams.spawn(published_tcp(map.clone(), send, recv));
                }
            }
        }
    };
    monitor(&conn, &label, notify, service).await
}
fn report_stream(result: Result<Result<()>, tokio::task::JoinError>, conn: &Connection, peer: &str, notify: &Notify) {
    match result {
        Ok(Ok(())) => notify(State::Online {
            direct: direct(conn),
            peer: peer.to_owned(),
        }),
        Ok(Err(e)) => notify(State::ServiceError(Arc::new(e))),
        Err(e) => notify(State::ServiceError(Arc::new(e.into()))),
    }
}
async fn published_tcp(map: Mapping, mut tx: SendStream, mut rx: RecvStream) -> Result<()> {
    let mut id = [0; 2];
    timeout(HANDSHAKE, rx.read_exact(&mut id)).await??;
    let stream_id = u16::from_be_bytes(id);
    let port = if stream_id == 0 {
        map.port
    } else {
        ensure!(map.service == Service::Ftp && (map.passive.0..=map.passive.1).contains(&stream_id), "unpublished port");
        stream_id
    };
    let connected = timeout(HANDSHAKE, TcpStream::connect((map.host.as_str(), port)))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result.map_err(Into::into));
    let mut tcp = match connected {
        Ok(tcp) => tcp,
        Err(error) => {
            tx.write_all(&[0]).await?;
            tx.finish()?;
            bail!(MappingMessage::new("无法连接目标服务 {}").with_value(format!("{}:{port}", map.host)).caused_by(error))
        }
    };
    tcp.set_nodelay(true)?;
    tx.write_all(&[1]).await?;
    if map.service == Service::Ftp && stream_id == 0 {
        ftp::control(tcp, tx, rx, map.passive).await
    } else {
        copy(&mut tcp, tx, rx).await
    }
}

async fn copy(tcp: &mut TcpStream, send: SendStream, recv: RecvStream) -> Result<()> {
    let mut stream = tokio::io::join(recv, send);
    tokio::io::copy_bidirectional(tcp, &mut stream).await?;
    Ok(())
}
async fn bind_listeners(map: &Mapping, passive: (u16, u16)) -> Result<Vec<(TcpListener, u16)>> {
    let mut ports = vec![(map.port, 0)];
    if map.service == Service::Ftp {
        validate_range(passive)?;
        ports.extend((passive.0..=passive.1).map(|p| (p, p)));
    }
    let mut listeners = Vec::new();
    for (port, id) in ports {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        listeners.push((
            TcpListener::bind(addr)
                .await
                .map_err(|error| MappingMessage::new("本机 {} 无法监听").with_value(addr).caused_by(error))?,
            id,
        ));
    }
    Ok(listeners)
}
async fn local_tcp(conn: Connection, mut tcp: TcpStream, stream_id: u16) -> Result<()> {
    tcp.set_nodelay(true)?;
    let (tx, rx) = timeout(HANDSHAKE, async {
        let (mut tx, mut rx) = conn.open_bi().await?;
        tx.write_all(&stream_id.to_be_bytes()).await?;
        let mut ack = [0];
        rx.read_exact(&mut ack).await?;
        ensure!(ack == [1], MappingMessage::new("设备端无法连接目标服务"));
        Ok::<_, anyhow::Error>((tx, rx))
    })
    .await??;
    copy(&mut tcp, tx, rx).await
}
async fn listen_tcp(conn: Connection, listener: TcpListener, stream_id: u16, limit: Arc<tokio::sync::Semaphore>, notify: Notify, peer: String) -> Result<()> {
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            Some(result) = clients.join_next() => report_stream(result, &conn, &peer, &notify),
            accepted = listener.accept() => {
                let (tcp, _) = accepted?;
                let Ok(permit) = limit.clone().try_acquire_owned() else { continue; };
                let conn = conn.clone();
                clients.spawn(async move {
                    let _permit = permit;
                    local_tcp(conn,tcp,stream_id).await
                });
            }
        }
    }
}
async fn client_tcp(conn: &Connection, listeners: Vec<(TcpListener, u16)>, notify: Notify, peer: &str) -> Result<()> {
    let limit = Arc::new(tokio::sync::Semaphore::new(128));
    let mut tasks = JoinSet::new();
    for (listener, stream_id) in listeners {
        tasks.spawn(listen_tcp(conn.clone(), listener, stream_id, limit.clone(), notify.clone(), peer.to_owned()));
    }
    tasks.join_next().await.context("no mapping listener")??
}

#[cfg(test)]
mod uart_loopback {
    use super::*;
    /// Device-side acceptance test with a physical TX/RX loopback or a Win32 port
    /// backed by an echoing PTY. The connection address and physical mapping are supplied
    /// explicitly by the operator. No application or firmware data is replayed.
    #[tokio::test]
    #[ignore = "needs QCCOM_LOOPBACK_ADDRESS from a Windows host sharing a loopback UART"]
    async fn windows_device_echo() -> Result<()> {
        let invite = ShareInvitation::parse(&std::env::var("QCCOM_LOOPBACK_ADDRESS")?)?;
        let voucher = invite.voucher();
        ensure!(voucher.service() == ShareService::Com, "expected a COM mapping address");
        let number = voucher.number();
        let peer = EndpointAddr::new(voucher.host()).with_relay_url(invite.relay().clone());
        let proof = invite.proof().to_owned();
        let node = Node::bind_share_client(invite, false).await?;
        let conn = timeout(HANDSHAKE, node.endpoint.connect(peer, SHARE_ALPN)).await??;
        let (mut tx, mut rx) = conn.open_bi().await?;
        send_json(
            &mut tx,
            &Hello {
                service: Service::Com,
                number,
                label: "COM21".into(),
                proof,
                observe_host_tx: false,
            },
        )
        .await?;
        let reply: Reply = timeout(HANDSHAKE, read_json(&mut rx)).await??;
        ensure!(valid_com(&reply.label), "missing device port");
        let mut stream = vec![255, 16, 0, 194, 1, 0, 255, 17, 8, 0, 0]; // 115200 8N1
        let expected: Vec<u8> = (0..=255).cycle().take(32768).collect();
        for byte in &expected {
            stream.push(*byte);
            if *byte == 255 {
                stream.push(0);
            }
        }
        tx.write_all(&stream).await?;
        let mut echoed = vec![0; expected.len()];
        timeout(Duration::from_secs(12), rx.read_exact(&mut echoed)).await??;
        ensure!(echoed == expected, "UART echo mismatch");
        tx.finish()?;
        conn.close(0u32.into(), b"loopback complete");
        node.endpoint.close().await;
        Ok(())
    }
}

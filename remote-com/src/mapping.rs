//! A single authenticated endpoint carries independent COM, MQTT and passive FTP mappings.
use crate::{
    sharing::{now_unix, RelayVoucher, ShareInvitation, ShareService, SHARE_RELAY_TOKEN},
    Invitation, Role,
};
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use iroh::{
    endpoint::{presets, Connection, RecvStream, SendStream},
    Endpoint, EndpointAddr, RelayMap, RelayMode,
};
use iroh_base::{EndpointId, RelayUrl, SecretKey};
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
mod ftp;
#[cfg(any(windows, test))]
mod line_events;
#[cfg(all(test, feature = "server"))]
mod share_tests;
#[cfg(windows)]
mod uart;

const ALPN: &[u8] = b"qccom/map/1";
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
        ensure!(self.number > 0, "映射端口必须大于零");
        if self.service == Service::Com {
            for s in [&self.serial, &self.label] {
                ensure!(valid_com(s), "请选择有效 COM 口");
            }
        } else {
            ensure!(!self.host.trim().is_empty() && self.port > 0, "请填写服务地址和端口");
        }
        if self.service == Service::Ftp {
            validate_range(self.passive)?;
        }
        Ok(())
    }
    fn name(&self, role: Role) -> String {
        if self.service == Service::Com {
            self.label.clone()
        } else if role == Role::Host {
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
    ensure!(lo >= 1024 && hi >= lo && hi - lo < 64, "FTP 被动端口范围须在 1024–65535 之间，最多 64 个端口");
    Ok(())
}
pub fn set_server(invite: &mut Invitation, addr: &str) -> Result<()> {
    let addr = addr.trim();
    ensure!(
        !addr.is_empty() && addr.len() <= 253 && addr.bytes().all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c)),
        "请输入服务器域名或 IPv4 地址"
    );
    let port = invite.relay.port().unwrap_or(443);
    invite.relay = format!("https://{addr}:{port}").parse()?;
    Ok(())
}

fn share_relay(server: &str) -> Result<RelayUrl> {
    let server = server.trim();
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().context("请输入有效的服务器端口")?),
        None => (server, DEFAULT_RELAY_PORT),
    };
    ensure!(
        !host.is_empty() && host.len() <= 253 && host.bytes().all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c)) && port != 0,
        "请输入服务器域名或 IPv4 地址"
    );
    Ok(format!("https://{host}:{port}").parse()?)
}

#[derive(Debug, Clone)]
pub enum State {
    Registering,
    Waiting,
    Punching,
    Online { direct: bool, peer: String },
    ServiceError(String),
    Failed(String),
    Expired,
}
pub type Notify = Arc<dyn Fn(State) + Send + Sync>;

#[derive(Serialize, Deserialize)]
struct Hello {
    service: Service,
    number: u16,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proof: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    label: String,
    passive: (u16, u16),
}
struct Session {
    conn: Connection,
    tx: SendStream,
    rx: RecvStream,
    label: String,
}
type Key = (Service, u16);
type Routes = Arc<Mutex<HashMap<Key, Route>>>;

#[derive(Clone)]
struct Route {
    sessions: mpsc::Sender<Session>,
    grant: Option<Arc<RelayVoucher>>,
}

impl Route {
    fn permits(&self, proof: Option<&str>) -> bool {
        match (&self.grant, proof) {
            (None, None) => true,
            (Some(grant), Some(proof)) => now_unix().is_ok_and(|now| now < grant.expires_at()) && grant.verify_proof(proof).is_ok(),
            _ => false,
        }
    }
}

enum Pairing {
    Legacy { role: Role, peer: EndpointAddr },
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
    pub async fn bind(invite: Invitation, relay_only: bool) -> Result<Self> {
        invite.validate()?;
        let tls = CaTlsConfig::custom_roots([STANDARD.decode(invite.certificate)?.into()]);
        let peer = EndpointAddr::new(invite.peer).with_relay_url(invite.relay.clone());
        Self::bind_endpoint(invite.identity, invite.relay.into(), tls, Pairing::Legacy { role: invite.role, peer }, relay_only).await
    }

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
            Pairing::Legacy { .. } => unreachable!(),
        };
        let map = RelayMap::from(relay.clone()).with_auth_token(token);
        Self::bind_endpoint(SecretKey::generate(), map, tls, pairing, relay_only).await
    }

    async fn bind_endpoint(identity: SecretKey, relays: RelayMap, tls: CaTlsConfig, pairing: Pairing, relay_only: bool) -> Result<Self> {
        let mut builder = Endpoint::builder(presets::Minimal).secret_key(identity).ca_tls_config(tls).relay_mode(RelayMode::Custom(relays));
        if relay_only {
            builder = builder.clear_ip_transports();
        }
        let incoming = match &pairing {
            Pairing::Legacy { role: Role::Host, peer } => Some((ALPN, Some(peer.id))),
            Pairing::ShareHost(_) => Some((SHARE_ALPN, None)),
            _ => None,
        };
        if let Some((alpn, _)) = incoming {
            builder = builder.alpns(vec![alpn.to_vec()]);
        }
        let endpoint = builder.bind().await?;
        // Only report the address after the relay has admitted this endpoint.
        timeout(HANDSHAKE, endpoint.online()).await.context("穿透服务器连接超时，请检查服务器地址、证书及 TCP 7000")?;
        let routes = Routes::default();
        let accept = incoming.map(|(_, peer)| tokio::spawn(accept_sessions(endpoint.clone(), routes.clone(), peer)));
        Ok(Self { endpoint, pairing, routes, accept })
    }

    pub async fn share(&self, map: Mapping, notify: Notify, publish: impl FnOnce(String) + Send) -> Result<()> {
        map.validate()?;
        let Pairing::ShareHost(relay) = &self.pairing else {
            bail!("共享需要使用分享端连接");
        };
        timeout(HANDSHAKE, self.endpoint.online()).await.context("穿透服务器连接超时")?;
        let invite = ShareInvitation::issue(relay.clone(), self.endpoint.secret_key(), map.service.into(), map.number)?;
        let grant = invite.voucher().clone();
        self.run_host(map, notify, Some(Arc::new(grant)), || publish(invite.address())).await
    }

    pub async fn run(&self, map: Mapping, notify: Notify) -> Result<()> {
        map.validate()?;
        let (peer, alpn, proof) = match &self.pairing {
            Pairing::Legacy { role: Role::Host, .. } => return self.run_host(map, notify, None, || {}).await,
            Pairing::Legacy { role: Role::Client, peer } => (peer.clone(), ALPN, None),
            Pairing::ShareHost(_) => bail!("请先生成分享地址"),
            Pairing::ShareClient(invite) => {
                let voucher = invite.voucher();
                ensure!(Service::from(voucher.service()) == map.service && voucher.number() == map.number, "连接地址与映射不匹配");
                let peer = EndpointAddr::new(voucher.host()).with_relay_url(invite.relay().clone());
                (peer, SHARE_ALPN, Some(invite.proof().to_owned()))
            }
        };
        self.run_client(map, notify, peer, alpn, proof).await
    }

    async fn run_host(&self, map: Mapping, notify: Notify, grant: Option<Arc<RelayVoucher>>, started: impl FnOnce()) -> Result<()> {
        let key = (map.service, map.number);
        let expires_at = grant.as_ref().map(|voucher| voucher.expires_at());
        let (tx, mut sessions) = mpsc::channel(1);
        {
            let mut routes = self.routes.lock().unwrap();
            ensure!(!routes.contains_key(&key), "映射端口已在使用");
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
            let session = if let Some(expires_at) = expires_at {
                let remaining = Duration::from_secs(expires_at.saturating_sub(now_unix()?));
                match timeout(remaining, sessions.recv()).await {
                    Ok(session) => session,
                    Err(_) => {
                        notify(State::Expired);
                        return Ok(());
                    }
                }
            } else {
                sessions.recv().await
            }
            .context("endpoint closed")?;
            let run = serve_mapping(&map, session, &notify);
            tokio::pin!(run);
            loop {
                tokio::select! {
                    result = &mut run => {
                        notify(match result {
                            Ok(()) => State::Waiting,
                            Err(error) => State::ServiceError(format!("{error:#}")),
                        });
                        break;
                    }
                    Some(extra)=sessions.recv()=>extra.conn.close(1u32.into(),b"mapping busy"),
                }
            }
        }
    }

    async fn run_client(&self, map: Mapping, notify: Notify, peer: EndpointAddr, alpn: &[u8], proof: Option<String>) -> Result<()> {
        notify(State::Punching);
        let conn = timeout(HANDSHAKE, self.endpoint.connect(peer, alpn)).await.context("设备端未响应")??;
        let _close = CloseMapping(conn.clone());
        // Iroh may use the relay for discovery. Hold application bytes until a selected
        // IP path exists, or the direct-path budget expires.
        let until = Instant::now() + DIRECT_WAIT;
        while !direct(&conn) && Instant::now() < until {
            tokio::select! { err=conn.closed()=>bail!("{err}"), _=sleep(Duration::from_millis(80))=>{} }
        }
        #[cfg(windows)]
        let serial = if map.service == Service::Com { Some(uart::Port::open(&map.serial, true)?) } else { None };
        #[cfg(not(windows))]
        ensure!(map.service != Service::Com, "COM 映射需要 Windows");
        #[allow(unused_mut)]
        let (mut tx, mut rx, reply) = timeout(HANDSHAKE, async {
            let (mut tx, mut rx) = conn.open_bi().await?;
            send_json(
                &mut tx,
                &Hello {
                    service: map.service,
                    number: map.number,
                    label: map.name(Role::Client),
                    proof,
                },
            )
            .await?;
            let reply: Reply = read_json(&mut rx).await?;
            Ok::<_, anyhow::Error>((tx, rx, reply))
        })
        .await
        .context("映射握手超时")??;
        #[cfg(windows)]
        if let Some(port) = serial {
            return monitor(&conn, &reply.label, &notify, port.bridge(&mut tx, &mut rx)).await;
        }
        // Keep the control stream alive for the lifetime of its mapped connection.
        let _control = (tx, rx);
        let listeners = bind_listeners(&map, reply.passive).await?;
        monitor(&conn, &reply.label, &notify, client_tcp(&conn, listeners, notify.clone(), &reply.label)).await
    }
}

async fn accept_sessions(endpoint: Endpoint, routes: Routes, peer: Option<EndpointId>) {
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
                    if peer.is_some_and(|peer| conn.remote_id() != peer) {
                        conn.close(1u32.into(), b"unknown peer");
                        return Ok(());
                    }
                    let (tx, mut rx) = conn.accept_bi().await?;
                    let hello: Hello = read_json(&mut rx).await?;
                    ensure!(hello.label.len() <= 256 && !hello.label.chars().any(char::is_control), "invalid label");
                    let route = routes.lock().unwrap().get(&(hello.service, hello.number)).cloned();
                    let Some(route) = route.filter(|route| route.permits(hello.proof.as_deref())) else {
                        conn.close(1u32.into(), b"mapping unavailable");
                        return Ok(());
                    };
                    let session = Session { conn, tx, rx, label: hello.label };
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
            err=conn.closed()=>bail!("对端已断开 · {err}"),
            _=tick.tick()=>{let now=direct(conn); if last!=Some(now) {notify(State::Online{direct:now,peer:label.to_owned()});last=Some(now);}}
        }
    }
}
async fn serve_mapping(map: &Mapping, session: Session, notify: &Notify) -> Result<()> {
    let Session { conn, mut tx, rx, label } = session;
    let _close = CloseMapping(conn.clone());
    if map.service == Service::Com {
        // Open the physical device before acknowledging a usable mapping.
        #[cfg(windows)]
        {
            let mut rx = rx;
            let serial = uart::Port::open(&map.serial, false).inspect_err(|_| conn.close(1u32.into(), b"physical COM unavailable or busy"))?;
            send_json(
                &mut tx,
                &Reply {
                    label: map.label.clone(),
                    passive: map.passive,
                },
            )
            .await?;
            return monitor(&conn, &label, notify, serial.bridge(&mut tx, &mut rx)).await;
        }
        #[cfg(not(windows))]
        bail!("COM 映射需要 Windows");
    }
    let _control_rx = rx;
    send_json(
        &mut tx,
        &Reply {
            label: map.name(Role::Host),
            passive: map.passive,
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
        Ok(Err(e)) => notify(State::ServiceError(format!("{e:#}"))),
        Err(e) => notify(State::ServiceError(e.to_string())),
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
    let connected = timeout(HANDSHAKE, TcpStream::connect((map.host.as_str(), port))).await;
    let mut tcp = match connected {
        Ok(Ok(tcp)) => tcp,
        error => {
            tx.write_all(&[0]).await?;
            tx.finish()?;
            let reason = match error {
                Ok(Err(e)) => e.to_string(),
                _ => "timed out".into(),
            };
            bail!("无法连接目标服务 {}:{port} · {reason}", map.host)
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
        listeners.push((TcpListener::bind(addr).await.with_context(|| format!("本机 {addr} 无法监听"))?, id));
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
        ensure!(ack == [1], "设备端无法连接目标服务");
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
    /// backed by an echoing PTY. The credential and physical mapping are supplied
    /// explicitly by the operator. No application or firmware data is replayed.
    #[tokio::test]
    #[ignore = "needs an authorized Windows host publishing a loopback UART on mapping 7001"]
    async fn windows_device_echo() -> Result<()> {
        let invite = serde_json::from_slice(&std::fs::read(std::env::var("QCCOM_LOOPBACK_PROFILE")?)?)?;
        let node = Node::bind(invite, false).await?;
        let Pairing::Legacy { peer, .. } = &node.pairing else {
            unreachable!("the fixture uses a legacy pairing profile");
        };
        let conn = timeout(HANDSHAKE, node.endpoint.connect(peer.clone(), ALPN)).await??;
        let (mut tx, mut rx) = conn.open_bi().await?;
        send_json(
            &mut tx,
            &Hello {
                service: Service::Com,
                number: 7001,
                label: "COM21".into(),
                proof: None,
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

use std::{io::Read, net::Ipv4Addr, path::Path, time::Duration};

use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use iroh::{
    endpoint::{presets, Connection},
    Endpoint, EndpointAddr, RelayMode,
};
use iroh_relay::tls::CaTlsConfig;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::timeout,
};

use super::{Invitation, Role};
const ALPN: &[u8] = b"qccom/serial/1";
const CONNECT: Duration = Duration::from_secs(15);

/// Called in a supervised copy of the GUI executable. No credentials in argv.
pub fn run_worker(path: &Path, port: u16, relay_only: bool) -> Result<()> {
    let file = std::fs::File::open(path)?;
    ensure!(file.metadata()?.len() <= 16384, "invalid invitation size");
    let invitation: Invitation = serde_json::from_reader(file.take(16385))?;
    invitation.validate()?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run(invitation, port, relay_only))
}

async fn run(invite: Invitation, port: u16, relay_only: bool) -> Result<()> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(invite.identity)
        .ca_tls_config(CaTlsConfig::custom_roots([STANDARD.decode(invite.certificate)?.into()]))
        .relay_mode(RelayMode::Custom(invite.relay.clone().into()));
    if relay_only {
        builder = builder.clear_ip_transports();
    }
    if invite.role == Role::Host {
        builder = builder.alpns(vec![ALPN.to_vec()]);
    }
    let ep = builder.bind().await?;
    timeout(CONNECT, ep.online()).await.context("relay login timed out; check certificate, pairing and TCP 7000")?;
    let local = (Ipv4Addr::LOCALHOST, port);
    if invite.role == Role::Host {
        println!("READY");
        let mut active = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                Some(result) = active.join_next() => {
                    if let Err(e) = result? { eprintln!("COM session: {e:#}"); }
                }
                incoming = ep.accept() => {
                    let Some(incoming) = incoming else { anyhow::bail!("endpoint closed"); };
                    if !active.is_empty() { incoming.refuse(); continue; }
                    let peer = invite.peer;
                    active.spawn(async move {
                        let (connection, tcp, send, recv) = timeout(CONNECT, async move {
                            let connection = incoming.await?;
                            ensure!(connection.remote_id() == peer, "unpaired endpoint");
                            let (mut send, mut recv) = connection.accept_bi().await?;
                            let mut hello = [0u8; 1];
                            recv.read_exact(&mut hello).await?;
                            ensure!(hello == [1], "invalid COM stream");
                            let tcp = TcpStream::connect(local).await?;
                            send.write_all(&[1]).await?;
                            Ok::<_, anyhow::Error>((connection, tcp, send, recv))
                        }).await??;
                        bridge(connection, tcp, send, recv).await
                    });
                }
            }
        }
    }
    let listener = TcpListener::bind(local).await?;
    let peer = EndpointAddr::new(invite.peer).with_relay_url(invite.relay);
    println!("READY");
    loop {
        let (tcp, _) = listener.accept().await?;
        let result = async {
            let (connection, send, recv) = timeout(CONNECT, async {
                let connection = ep.connect(peer.clone(), ALPN).await?;
                let (mut send, mut recv) = connection.open_bi().await?;
                send.write_all(&[1]).await?;
                let mut ack = [0u8; 1];
                recv.read_exact(&mut ack).await?;
                ensure!(ack == [1], "remote COM unavailable");
                Ok::<_, anyhow::Error>((connection, send, recv))
            })
            .await
            .context("paired endpoint did not answer")??;
            bridge(connection, tcp, send, recv).await
        }
        .await;
        if let Err(e) = result {
            eprintln!("COM session: {e:#}");
        }
    }
}

async fn bridge(conn: Connection, mut tcp: TcpStream, send: iroh::endpoint::SendStream, recv: iroh::endpoint::RecvStream) -> Result<()> {
    tcp.set_nodelay(true)?;
    let mut stream = tokio::io::join(recv, send);
    let copy = tokio::io::copy_bidirectional(&mut tcp, &mut stream);
    tokio::pin!(copy);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut last = None;
    loop {
        tokio::select! {
            result = &mut copy => { result?; return Ok(()); }
            _ = tick.tick() => {
                if let Some(path) = conn.paths().iter().find(|p| p.is_selected()) {
                    let direct = path.is_ip();
                    if last != Some(direct) {
                        println!("{}", if direct { "PATH direct" } else { "PATH relay" });
                        last = Some(direct);
                    }
                }
            }
        }
    }
}

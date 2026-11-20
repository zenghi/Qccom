use std::net::{Ipv4Addr, Ipv6Addr};

use iroh_relay::server::{CertConfig, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
    task::JoinHandle,
};

use super::*;
use crate::share_relay::ShareAccess;

async fn relay() -> Result<(Server, RelayUrl, CaTlsConfig)> {
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let cert = identity.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key.into())?;
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new((Ipv4Addr::LOCALHOST, 0), CertConfig::Manual { server_config: tls }));
    relay.access = Arc::new(ShareAccess::default());
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(QuicConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let server = Server::spawn(config).await?;
    let port = server.https_addr().context("missing HTTPS listener")?.port();
    let relay = share_relay(&format!("https://localhost:{port}/"))?;
    Ok((server, relay, CaTlsConfig::custom_roots([cert])))
}

async fn share(host: &Arc<Node>, mapping: &Mapping) -> Result<(ShareInvitation, JoinHandle<Result<()>>)> {
    let (published, ready) = oneshot::channel();
    let host = host.clone();
    let mapping = mapping.clone();
    let task = tokio::spawn(async move {
        host.share(mapping, Arc::new(|_| {}), |address| {
            let _ = published.send(address);
        })
        .await
    });
    let address = ready.await?;
    Ok((ShareInvitation::parse(&address)?, task))
}

async fn client(invite: ShareInvitation, tls: &CaTlsConfig, relay_only: bool) -> Result<Node> {
    Node::bind_shared(Pairing::ShareClient(invite), tls.clone(), relay_only).await
}

async fn join(invite: ShareInvitation, tls: &CaTlsConfig, relay_only: bool, mapping: Mapping) -> Result<JoinHandle<Result<()>>> {
    let joining = client(invite, tls, relay_only).await?;
    let (events, mut statuses) = mpsc::unbounded_channel();
    let client_task = tokio::spawn(async move {
        joining
            .run(
                mapping,
                Arc::new(move |state| {
                    let _ = events.send(state);
                }),
            )
            .await
    });
    loop {
        match statuses.recv().await.context("mapping stopped before connecting")? {
            State::Online { direct, .. } if direct != relay_only => break,
            State::ServiceError(error) | State::Failed(error) => bail!("mapping failed: {error}"),
            _ => {}
        }
    }
    Ok(client_task)
}

async fn verify_passive_ftp(host: &Arc<Node>, tls: &CaTlsConfig, relay_only: bool) -> Result<()> {
    // The device uses IPv6 loopback so the client can bind the same data port on IPv4.
    let server = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await?;
    let reserve_data = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let data_port = reserve_data.local_addr()?.port();
    let data = TcpListener::bind((Ipv6Addr::LOCALHOST, data_port)).await?;
    let mut published = Mapping::new(Service::Ftp);
    published.host = Ipv6Addr::LOCALHOST.to_string();
    published.port = server.local_addr()?.port();
    published.passive = (data_port, data_port);

    let mut consumed = published.clone();
    consumed.port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?.local_addr()?.port();
    drop(reserve_data);
    let local_addr = (Ipv4Addr::LOCALHOST, consumed.port);
    let (invite, host_task) = share(host, &published).await?;
    let client_task = join(invite, tls, relay_only, consumed).await?;

    let mut control = TcpStream::connect(local_addr).await?;
    let (mut server_control, _) = server.accept().await?;
    let [hi, lo] = data_port.to_be_bytes();
    let pasv = format!("227 Entering Passive Mode (192,0,2,123,{hi},{lo})\r\n");
    let local_pasv = format!("227 Entering Passive Mode (127,0,0,1,{hi},{lo})\r\n");
    let epsv = format!("229 Entering Extended Passive Mode (|||{data_port}|)\r\n");
    for (command, reply, expected) in [("PASV\r\n", &pasv, &local_pasv), ("EPSV\r\n", &epsv, &epsv)] {
        control.write_all(command.as_bytes()).await?;
        let mut received = vec![0; command.len()];
        server_control.read_exact(&mut received).await?;
        assert_eq!(received, command.as_bytes());
        server_control.write_all(reply.as_bytes()).await?;
        let mut received = vec![0; expected.len()];
        control.read_exact(&mut received).await?;
        assert_eq!(received, expected.as_bytes());
    }

    let mut consumer = TcpStream::connect((Ipv4Addr::LOCALHOST, data_port)).await?;
    let (mut producer, _) = data.accept().await?;
    let bytes: Vec<_> = (0..=255).cycle().take(64 * 1024).collect();
    let mut received = vec![0; bytes.len()];
    consumer.write_all(&bytes).await?;
    producer.read_exact(&mut received).await?;
    assert_eq!(bytes, received);
    producer.write_all(&bytes).await?;
    consumer.read_exact(&mut received).await?;
    assert_eq!(bytes, received);
    host_task.abort();
    assert!(host_task.await.unwrap_err().is_cancelled());
    assert!(client_task.await?.is_err());
    Ok(())
}

async fn verify_share(relay: RelayUrl, tls: CaTlsConfig, relay_only: bool) -> Result<()> {
    let host = Arc::new(Node::bind_shared(Pairing::ShareHost(relay), tls.clone(), relay_only).await?);
    let device = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut published = Mapping::new(Service::Mqtt);
    published.port = device.local_addr()?.port();
    let mut consumed = published.clone();
    consumed.port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?.local_addr()?.port();
    let local_addr = (Ipv4Addr::LOCALHOST, consumed.port);

    let (invite, host_task) = share(&host, &published).await?;
    let client_task = join(invite.clone(), &tls, relay_only, consumed.clone()).await?;

    let mut consumer = TcpStream::connect(local_addr).await?;
    consumer.write_all(b"subscriber\0").await?;
    let (mut broker, _) = device.accept().await?;
    let mut upstream = [0; b"subscriber\0".len()];
    broker.read_exact(&mut upstream).await?;
    assert_eq!(&upstream, b"subscriber\0");

    let binary: Vec<_> = (0..=255).cycle().take(64 * 1024).collect();
    broker.write_all(&binary).await?;
    let mut downstream = vec![0; binary.len()];
    consumer.read_exact(&mut downstream).await?;
    assert_eq!(downstream, binary);

    verify_passive_ftp(&host, &tls, relay_only).await?;
    host_task.abort();
    assert!(host_task.await.unwrap_err().is_cancelled());
    assert!(client_task.await?.is_err());
    assert!(consumer.read_u8().await.is_err());

    // Reusing the same host and mapping slot must never reactivate the old address.
    let (new_invite, new_host_task) = share(&host, &published).await?;
    assert_ne!(new_invite.address(), invite.address());
    let stale_client = client(invite, &tls, relay_only).await?;
    let error = stale_client.run(consumed, Arc::new(|_| {})).await.unwrap_err();
    assert!(format!("{error:#}").contains("mapping unavailable"), "{error:#}");
    new_host_task.abort();
    assert!(new_host_task.await.unwrap_err().is_cancelled());
    host.endpoint.close().await;
    assert!(!matches!(timeout(Duration::from_secs(2), client(new_invite, &tls, relay_only)).await, Ok(Ok(_))));
    Ok(())
}

#[tokio::test]
async fn shares_without_credentials_and_revokes_old_addresses() -> Result<()> {
    let (server, relay, tls) = relay().await?;
    super::relay_check::probe_relay(relay.clone(), tls.clone(), SHARE_RELAY_TOKEN).await?;
    for relay_only in [false, true] {
        timeout(Duration::from_secs(40), verify_share(relay.clone(), tls.clone(), relay_only))
            .await
            .with_context(|| format!("mapping timed out (relay only: {relay_only})"))??;
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn com_tx_log_and_uart_data_over_authenticated_relay() -> Result<()> {
    let (server, relay, tls) = relay().await?;
    timeout(Duration::from_secs(25), async {
        let host = Node::bind_shared(Pairing::ShareHost(relay.clone()), tls.clone(), true).await?;
        let map = Mapping::new(Service::Com);
        let invite = ShareInvitation::issue(relay.clone(), host.endpoint.secret_key(), ShareService::Com, map.number)?;
        let grant = Arc::new(invite.voucher().clone());
        // Use the real authenticated handshake, with the test handling the admitted session in place of a Windows UART.
        let (sessions, mut incoming) = mpsc::channel(1);
        host.routes.lock().unwrap().insert((Service::Com, map.number), Route { sessions, grant });
        let joining = client(invite.clone(), &tls, true).await?;
        let peer = EndpointAddr::new(host.endpoint.id()).with_relay_url(relay);
        let client_conn = joining.endpoint.connect(peer, SHARE_ALPN).await?;
        let (mut tx, mut rx) = client_conn.open_bi().await?;
        send_json(
            &mut tx,
            &Hello {
                service: Service::Com,
                number: map.number,
                label: "COM21".into(),
                proof: invite.proof().to_owned(),
                observe_host_tx: true,
            },
        )
        .await?;
        let mut accepted = incoming.recv().await.context("missing authenticated COM session")?;
        assert!(accepted.observe_host_tx);
        send_json(
            &mut accepted.tx,
            &Reply {
                label: "COM5".into(),
                passive: map.passive,
                observe_host_tx: true,
            },
        )
        .await?;
        assert!(read_json::<Reply>(&mut rx).await?.observe_host_tx);
        assert!(!direct(&client_conn) && !direct(&accepted.conn));
        super::traffic::tests::assert_serial_traffic(accepted.conn, client_conn).await
    })
    .await
    .context("COM traffic relay test timed out")??;
    server.shutdown().await?;
    Ok(())
}

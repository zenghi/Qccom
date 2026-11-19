use std::net::Ipv4Addr;

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
    let identity = rcgen::generate_simple_self_signed(vec![Ipv4Addr::LOCALHOST.to_string()])?;
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
    let relay = format!("https://{}", server.https_addr().context("missing HTTPS listener")?).parse()?;
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

async fn verify_share(relay: RelayUrl, tls: CaTlsConfig, relay_only: bool) -> Result<()> {
    let host = Arc::new(Node::bind_shared(Pairing::ShareHost(relay), tls.clone(), relay_only).await?);
    let device = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut published = Mapping::new(Service::Mqtt);
    published.port = device.local_addr()?.port();
    let mut consumed = published.clone();
    consumed.port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?.local_addr()?.port();
    let local_addr = (Ipv4Addr::LOCALHOST, consumed.port);

    let (invite, host_task) = share(&host, &published).await?;
    let joining = client(invite.clone(), &tls, relay_only).await?;
    let (events, mut statuses) = mpsc::unbounded_channel();
    let join_mapping = consumed.clone();
    let client_task = tokio::spawn(async move {
        joining
            .run(
                join_mapping,
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
    for relay_only in [false, true] {
        timeout(Duration::from_secs(40), verify_share(relay.clone(), tls.clone(), relay_only))
            .await
            .with_context(|| format!("mapping timed out (relay only: {relay_only})"))??;
    }
    server.shutdown().await?;
    Ok(())
}

//! Standalone Rust rendezvous and relay for QCCOM endpoints.
use std::{env, net::Ipv4Addr, num::NonZeroU32, path::Path, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
use iroh_relay::server::{CertConfig, ClientRateLimit, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use qccom_tunnel::share_relay::ShareAccess;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};

const TCP: u16 = 7000;
const UDP: u16 = 7842;
const DEFAULT_RELAY_RATE: NonZeroU32 = NonZeroU32::new(8 * 1024 * 1024).unwrap();
const RELAY_BURST: NonZeroU32 = NonZeroU32::new(1024 * 1024).unwrap();
const RELAY_RATE_ENV: &str = "QCCOM_RELAY_BYTES_PER_SECOND";

async fn serve(certificate: &Path, key: &Path) -> Result<()> {
    let chain = CertificateDer::pem_file_iter(certificate)
        .context("open relay TLS certificate chain")?
        .collect::<Result<Vec<_>, _>>()
        .context("read relay TLS certificate chain")?;
    let key = PrivateKeyDer::from_pem_file(key).context("read relay TLS private key")?;
    let rate = match env::var(RELAY_RATE_ENV) {
        Ok(value) => value.parse().with_context(|| format!("invalid {RELAY_RATE_ENV}"))?,
        Err(env::VarError::NotPresent) => DEFAULT_RELAY_RATE,
        Err(err) => return Err(err).context(RELAY_RATE_ENV),
    };
    // Iroh refills its token bucket every 100 ms and needs at least one byte per refill.
    ensure!(rate.get() >= 10, "{RELAY_RATE_ENV} must be at least 10 bytes per second");
    let mut limit = ClientRateLimit::new(rate);
    limit.max_burst_bytes = Some(RELAY_BURST);

    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new((Ipv4Addr::UNSPECIFIED, TCP), CertConfig::Manual { server_config: tls }));
    relay.access = Arc::new(ShareAccess::default());
    relay.limits.client_rx = Some(limit);
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(QuicConfig::new((Ipv4Addr::UNSPECIFIED, UDP)));
    let mut server = Server::spawn(config).await?;
    println!("QCCOM rendezvous ready: TCP {TCP} (TLS relay), UDP {UDP} (address discovery)");
    tokio::select! {
        done = server.join() => { done??; }
        ctrl = stop_signal() => { ctrl?; server.shutdown().await?; }
    }
    Ok(())
}

async fn stop_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        tokio::select! { r=tokio::signal::ctrl_c()=>r, _=term.recv()=>Ok(()) }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [cmd, cert_flag, cert, key_flag, key] if cmd == "serve" && cert_flag == "--cert" && key_flag == "--key" => serve(Path::new(cert), Path::new(key)).await,
        _ => bail!("Usage: qccom-server serve --cert FULLCHAIN.pem --key PRIVKEY.pem"),
    }
}

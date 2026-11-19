//! Standalone Rust rendezvous and relay for QCCOM endpoints.
use std::{env, fs, net::Ipv4Addr, num::NonZeroU32, path::Path, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use iroh_base::{EndpointId, RelayUrl, SecretKey};
use iroh_relay::server::{Access, AccessControl, CertConfig, ClientRateLimit, ClientRequest, DynAccessControl, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use qccom_tunnel::{share_relay::ShareAccess, Invitation, Role, FORMAT};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};

const DIR: &str = "qccom-pair";
const TCP: u16 = 7000;
const UDP: u16 = 7842;
const DEFAULT_SHARED_RELAY_RATE: NonZeroU32 = NonZeroU32::new(8 * 1024 * 1024).unwrap();
const SHARED_RELAY_BURST: NonZeroU32 = NonZeroU32::new(1024 * 1024).unwrap();
const RELAY_RATE_ENV: &str = "QCCOM_RELAY_BYTES_PER_SECOND";

#[derive(Serialize, Deserialize)]
struct Settings {
    certificate: String,
    tls_key: String,
    allowed: [EndpointId; 2],
}

#[derive(Debug)]
struct Pair([EndpointId; 2]);

impl AccessControl for Pair {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        if self.0.contains(&request.endpoint_id()) {
            Access::Allow
        } else {
            Access::Deny { reason: None }
        }
    }
}

fn init(host: &str, aliases: &[String]) -> Result<()> {
    for host in std::iter::once(host).chain(aliases.iter().map(String::as_str)) {
        ensure!(
            !host.is_empty()
                && host.len() <= 253
                && host
                    .split('.')
                    .all(|s| !s.is_empty() && s.len() <= 63 && !s.starts_with('-') && !s.ends_with('-') && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')),
            "expected an IPv4 address or DNS name"
        );
    }
    ensure!(!Path::new(DIR).exists(), "{DIR} exists; preserve it to keep the same identities");
    let relay: RelayUrl = format!("https://{host}:{TCP}").parse()?;
    let signed = rcgen::generate_simple_self_signed(std::iter::once(host.to_owned()).chain(aliases.iter().cloned()).collect::<Vec<_>>())?;
    let certificate = STANDARD.encode(signed.cert.der());
    let device = SecretKey::generate();
    let flasher = SecretKey::generate();
    // The private directory (0700 on Unix) is published only after all files exist.
    let mut builder = tempfile::Builder::new();
    builder.prefix(".qccom-pair-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let stage = builder.tempdir_in(".")?;
    let store = |file, bytes: Vec<u8>| -> std::io::Result<()> {
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(stage.path().join(file))?.write_all(&bytes)
    };
    store(
        "server.json",
        serde_json::to_vec(&Settings {
            certificate: certificate.clone(),
            tls_key: STANDARD.encode(signed.signing_key.serialize_der()),
            allowed: [device.public(), flasher.public()],
        })?,
    )?;
    for (role, file, identity, peer) in [(Role::Host, "host.json", &device, flasher.public()), (Role::Client, "client.json", &flasher, device.public())] {
        let invite = Invitation {
            format: FORMAT.into(),
            role,
            relay: relay.clone(),
            certificate: certificate.clone(),
            identity: identity.clone(),
            peer,
        };
        store(file, serde_json::to_vec_pretty(&invite)?)?;
    }
    fs::rename(stage.path(), DIR)?;
    println!("Created {DIR}/server.json, host.json (device PC), client.json (flashing PC). Keep all three private.");
    Ok(())
}

async fn serve_pair() -> Result<()> {
    let settings: Settings = serde_json::from_slice(&fs::read(Path::new(DIR).join("server.json")).context("run init first, then serve in the same directory")?)?;
    let certificate = CertificateDer::from(STANDARD.decode(settings.certificate)?);
    let key = PrivatePkcs8KeyDer::from(STANDARD.decode(settings.tls_key)?).into();
    serve(vec![certificate], key, Arc::new(Pair(settings.allowed)), None).await
}

async fn serve_shared(certificate: &str, key: &str) -> Result<()> {
    let chain = CertificateDer::pem_file_iter(certificate)
        .context("open relay TLS certificate chain")?
        .collect::<Result<Vec<_>, _>>()
        .context("read relay TLS certificate chain")?;
    let key = PrivateKeyDer::from_pem_file(key).context("read relay TLS private key")?;
    let rate = match env::var(RELAY_RATE_ENV) {
        Ok(value) => value.parse().with_context(|| format!("invalid {RELAY_RATE_ENV}"))?,
        Err(env::VarError::NotPresent) => DEFAULT_SHARED_RELAY_RATE,
        Err(err) => return Err(err).context(RELAY_RATE_ENV),
    };
    // Iroh refills its token bucket every 100 ms and needs at least one byte per refill.
    ensure!(rate.get() >= 10, "{RELAY_RATE_ENV} must be at least 10 bytes per second");
    let mut limit = ClientRateLimit::new(rate);
    limit.max_burst_bytes = Some(SHARED_RELAY_BURST);
    serve(chain, key, Arc::new(ShareAccess::default()), Some(limit)).await
}

async fn serve(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>, access: Arc<dyn DynAccessControl>, limit: Option<ClientRateLimit>) -> Result<()> {
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new((Ipv4Addr::UNSPECIFIED, TCP), CertConfig::Manual { server_config: tls }));
    relay.access = access;
    relay.limits.client_rx = limit;
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
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, cert_flag, cert, key_flag, key] if cmd == "serve" && cert_flag == "--cert" && key_flag == "--key" => serve_shared(cert, key).await,
        [cmd, hosts @ ..] if cmd == "init" || cmd == "run" => {
            if cmd == "init" || !Path::new(DIR).exists() {
                let host = hosts.first().map(String::as_str).unwrap_or("www.zenghi.com");
                init(host, hosts.get(1..).unwrap_or_default())?;
            }
            if cmd == "run" {
                serve_pair().await
            } else {
                Ok(())
            }
        }
        [cmd] if cmd == "serve" => serve_pair().await,
        _ => bail!("Usage: qccom-server serve --cert FULLCHAIN.pem --key PRIVKEY.pem | run [PUBLIC_DNS [PUBLIC_IP]] | init [PUBLIC_DNS [PUBLIC_IP]] | serve"),
    }
}

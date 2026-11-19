//! Standalone Rust rendezvous/relay for two QCCOM endpoints.
use std::{env, fs, net::Ipv4Addr, path::Path, sync::Arc};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use iroh_base::{EndpointId, RelayUrl, SecretKey};
use iroh_relay::server::{Access, AccessControl, CertConfig, ClientRequest, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig};
use qccom_tunnel::{Invitation, Role, FORMAT};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};

const DIR: &str = "qccom-pair";
const TCP: u16 = 7000;
const UDP: u16 = 7842;

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

fn init(host: &str) -> Result<()> {
    ensure!(
        !host.is_empty()
            && host.len() <= 253
            && host
                .split('.')
                .all(|s| !s.is_empty() && s.len() <= 63 && !s.starts_with('-') && !s.ends_with('-') && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')),
        "expected an IPv4 address or DNS name"
    );
    ensure!(!Path::new(DIR).exists(), "{DIR} exists; preserve it to keep the same identities");
    let relay: RelayUrl = format!("https://{host}:{TCP}").parse()?;
    let signed = rcgen::generate_simple_self_signed(vec![host.to_owned()])?;
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

async fn serve() -> Result<()> {
    let settings: Settings = serde_json::from_slice(&fs::read(Path::new(DIR).join("server.json")).context("run init first, then serve in the same directory")?)?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(STANDARD.decode(settings.certificate)?)],
            PrivatePkcs8KeyDer::from(STANDARD.decode(settings.tls_key)?).into(),
        )?;
    let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
    relay.tls = Some(TlsConfig::new((Ipv4Addr::UNSPECIFIED, TCP), CertConfig::Manual { server_config: tls }));
    relay.access = Arc::new(Pair(settings.allowed));
    let mut config = ServerConfig::default();
    config.relay = Some(relay);
    config.quic = Some(QuicConfig::new((Ipv4Addr::UNSPECIFIED, UDP)));
    let mut server = Server::spawn(config).await?;
    println!("QCCOM rendezvous ready: TCP {TCP} (TLS relay), UDP {UDP} (address discovery)");
    tokio::select! {
        done = server.join() => { done??; }
        ctrl = tokio::signal::ctrl_c() => { ctrl?; server.shutdown().await?; }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, host] if cmd == "init" => init(host),
        [cmd] if cmd == "serve" => serve().await,
        _ => bail!("Usage: qccom-server init PUBLIC_IPV4_OR_DNS | qccom-server serve"),
    }
}

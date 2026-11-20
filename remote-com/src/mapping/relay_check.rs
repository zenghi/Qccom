use std::time::Duration;

use anyhow::Result;
use iroh_base::{RelayUrl, SecretKey};
use iroh_relay::{
    client::{ClientBuilder, ConnectError, DialError},
    tls::{default_provider, CaTlsConfig},
};
use tokio::time::timeout;

use super::{relay_dns::relay_dns, share_relay, MappingMessage};
use crate::sharing::SHARE_RELAY_TOKEN;

/// Tests the relay from this device using the same trust roots and registration
/// as a new share. Each test uses its own temporary identity.
pub async fn check_share_server(server: &str) -> Result<()> {
    probe_relay(share_relay(server)?, CaTlsConfig::embedded(), SHARE_RELAY_TOKEN).await
}

pub(super) async fn probe_relay(relay: RelayUrl, tls: CaTlsConfig, token: &str) -> Result<()> {
    let address = relay.as_str().trim_end_matches('/').to_owned();
    let client = ClientBuilder::new(relay, SecretKey::generate(), relay_dns())
        .tls_client_config(tls.client_config(default_provider())?)
        .auth_token(token);
    timeout(Duration::from_secs(8), client.connect())
        .await
        .map_err(|error| {
            MappingMessage::new("连接穿透服务器 {} 超时，请检查服务器地址、TLS 证书及 TCP 端口")
                .with_value(&address)
                .caused_by(error)
        })?
        .map_err(|error| {
            let key = match &error {
                ConnectError::Dial { source: DialError::Dns { .. }, .. } => "无法解析穿透服务器 {} 的域名",
                ConnectError::Dial { .. } => "无法建立到穿透服务器 {} 的 TCP 连接",
                ConnectError::Tls { .. } | ConnectError::InvalidTlsServername { .. } => "穿透服务器 {} 的 TLS 握手失败",
                _ => "无法注册到穿透服务器 {}",
            };
            MappingMessage::new(key).with_value(&address).caused_by(error)
        })?;
    Ok(())
}

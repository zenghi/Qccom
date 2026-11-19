//! Shared wire configuration for a single pair of authorized endpoints.
use anyhow::{ensure, Result};
use iroh_base::{EndpointId, RelayUrl, SecretKey};
use serde::{Deserialize, Serialize};

pub const FORMAT: &str = "qccom-iroh-1";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
    Client,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub format: String,
    pub role: Role,
    pub relay: RelayUrl,
    pub certificate: String,
    pub identity: SecretKey,
    pub peer: EndpointId,
}

impl Invitation {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.format == FORMAT, "unsupported QCCOM pairing format");
        ensure!(self.relay.scheme() == "https" && self.relay.host().is_some(), "expected an HTTPS relay");
        ensure!(self.identity.public() != self.peer, "invalid peer identity");
        Ok(())
    }
}

#[cfg(feature = "endpoint")]
mod endpoint;
#[cfg(feature = "endpoint")]
pub use endpoint::run_worker;

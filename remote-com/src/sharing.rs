//! Pasteable mapping addresses and their relay authorization vouchers.
use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use iroh_base::{EndpointId, PublicKey, RelayUrl, SecretKey, Signature};

pub const SHARE_RELAY_TOKEN: &str = "qccom-share-v1";
pub const SHARE_LIFETIME_SECS: u64 = 15 * 60;

const CLOCK_TOLERANCE_SECS: u64 = 60;
const ADDRESS_PREFIX: &str = "qccom://";
const VOUCHER_CONTEXT: &[u8] = b"QCCOM mapping voucher v1\0";
const WIRE_VERSION: u8 = 1;
// Version, host key, service, mapping number, expiration, and proof key are signed together.
const VOUCHER_SIZE: usize = 76;
const SIGNED_SIZE: usize = VOUCHER_SIZE + Signature::LENGTH;
const ENCODED_SIZE: usize = (SIGNED_SIZE * 4).div_ceil(3);
const PROOF_SIZE: usize = 43;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShareService {
    Com = 1,
    Mqtt = 2,
    Ftp = 3,
}

impl TryFrom<u8> for ShareService {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            1 => Self::Com,
            2 => Self::Mqtt,
            3 => Self::Ftp,
            _ => bail!("unsupported mapping service"),
        })
    }
}

#[derive(Clone)]
pub struct RelayVoucher {
    host: EndpointId,
    service: ShareService,
    number: u16,
    expires_at: u64,
    proof_key: PublicKey,
    token: String,
}

impl RelayVoucher {
    pub fn host(&self) -> EndpointId {
        self.host
    }

    pub fn service(&self) -> ShareService {
        self.service
    }

    pub fn number(&self) -> u16 {
        self.number
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Verifies the address's separate proof, which must only travel to the peer.
    pub fn verify_proof(&self, proof: &str) -> Result<()> {
        ensure!(proof.len() == PROOF_SIZE, "invalid mapping proof");
        let bytes = URL_SAFE_NO_PAD.decode(proof).context("invalid mapping proof")?;
        let key = SecretKey::from_bytes(bytes.as_slice().try_into().context("invalid mapping proof")?);
        ensure!(key.public() == self.proof_key, "mapping proof does not match the address");
        Ok(())
    }
}

impl fmt::Debug for RelayVoucher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayVoucher")
            .field("host", &self.host)
            .field("service", &self.service)
            .field("number", &self.number)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ShareInvitation {
    relay: RelayUrl,
    voucher: RelayVoucher,
    proof: String,
}

impl ShareInvitation {
    pub fn issue(relay: RelayUrl, host: &SecretKey, service: ShareService, number: u16) -> Result<Self> {
        Self::issue_at(relay, host, service, number, now_unix()?)
    }

    pub(crate) fn issue_at(relay: RelayUrl, host: &SecretKey, service: ShareService, number: u16, now: u64) -> Result<Self> {
        relay_authority(&relay)?;
        ensure!(number != 0, "invalid mapping number");
        let expires_at = now.checked_add(SHARE_LIFETIME_SECS).context("system clock is out of range")?;
        let proof = SecretKey::generate();
        let proof_key = proof.public();
        let host_id = host.public();
        let mut signed = [0; SIGNED_SIZE];
        signed[0] = WIRE_VERSION;
        signed[1..33].copy_from_slice(host_id.as_bytes());
        signed[33] = service as u8;
        signed[34..36].copy_from_slice(&number.to_be_bytes());
        signed[36..44].copy_from_slice(&expires_at.to_be_bytes());
        signed[44..VOUCHER_SIZE].copy_from_slice(proof_key.as_bytes());
        let signature = host.sign(&voucher_message(&signed[..VOUCHER_SIZE]));
        signed[VOUCHER_SIZE..].copy_from_slice(&signature.to_bytes());
        Ok(Self {
            relay,
            voucher: RelayVoucher {
                host: host_id,
                service,
                number,
                expires_at,
                proof_key,
                token: URL_SAFE_NO_PAD.encode(signed),
            },
            proof: URL_SAFE_NO_PAD.encode(proof.to_bytes()),
        })
    }

    pub fn parse(address: &str) -> Result<Self> {
        Self::parse_at(address, now_unix()?)
    }

    fn parse_at(address: &str, now: u64) -> Result<Self> {
        let address = address.trim();
        ensure!(address.len() <= 640, "mapping address is too long");
        let address = address.strip_prefix(ADDRESS_PREFIX).context("expected a QCCOM mapping address")?;
        let (authority, payload) = address.split_once('/').context("missing mapping voucher")?;
        ensure!(
            !authority.is_empty() && authority.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-:[]".contains(&byte)),
            "invalid relay address"
        );
        let relay: RelayUrl = format!("https://{authority}/").parse()?;
        relay_authority(&relay)?;
        let (token, proof) = payload.split_once('#').context("missing mapping proof")?;
        let voucher = verify_relay_voucher_at(token, now)?;
        voucher.verify_proof(proof)?;
        Ok(Self {
            relay,
            voucher,
            proof: proof.to_owned(),
        })
    }

    pub fn address(&self) -> String {
        // Both issue and parse validate that this relay has only an HTTPS authority.
        let authority = self.relay.as_str().trim_start_matches("https://").trim_end_matches('/');
        format!("{ADDRESS_PREFIX}{authority}/{}#{}", self.voucher.token(), self.proof)
    }

    pub fn relay(&self) -> &RelayUrl {
        &self.relay
    }

    pub fn voucher(&self) -> &RelayVoucher {
        &self.voucher
    }

    pub fn proof(&self) -> &str {
        &self.proof
    }
}

impl fmt::Debug for ShareInvitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShareInvitation").field("relay", &self.relay).field("voucher", &self.voucher).finish_non_exhaustive()
    }
}

pub fn now_unix() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH).context("system clock precedes the Unix epoch")?.as_secs())
}

pub fn verify_relay_voucher(token: &str) -> Result<RelayVoucher> {
    verify_relay_voucher_at(token, now_unix()?)
}

fn verify_relay_voucher_at(token: &str, now: u64) -> Result<RelayVoucher> {
    ensure!(token.len() == ENCODED_SIZE, "invalid mapping voucher");
    let signed = URL_SAFE_NO_PAD.decode(token).context("invalid mapping voucher")?;
    let signed: [u8; SIGNED_SIZE] = signed.try_into().map_err(|_| anyhow::anyhow!("invalid mapping voucher"))?;
    ensure!(signed[0] == WIRE_VERSION, "unsupported mapping address version");
    let service = ShareService::try_from(signed[33])?;
    let number = u16::from_be_bytes([signed[34], signed[35]]);
    ensure!(number != 0, "invalid mapping number");
    let expires_at = u64::from_be_bytes(signed[36..44].try_into().expect("fixed voucher timestamp length"));
    ensure!(expires_at > now, "mapping address has expired");
    ensure!(expires_at - now <= SHARE_LIFETIME_SECS + CLOCK_TOLERANCE_SECS, "invalid mapping expiration or system clock");
    let host = EndpointId::try_from(&signed[1..33])?;
    let signature = Signature::try_from(&signed[VOUCHER_SIZE..])?;
    host.verify(&voucher_message(&signed[..VOUCHER_SIZE]), &signature).context("invalid mapping voucher signature")?;
    let proof_key = PublicKey::try_from(&signed[44..VOUCHER_SIZE])?;
    Ok(RelayVoucher {
        host,
        service,
        number,
        expires_at,
        proof_key,
        token: token.to_owned(),
    })
}

fn relay_authority(relay: &RelayUrl) -> Result<()> {
    ensure!(
        relay.scheme() == "https"
            && relay.host().is_some()
            && relay.username().is_empty()
            && relay.password().is_none()
            && relay.port() != Some(0)
            && relay.path() == "/"
            && relay.query().is_none()
            && relay.fragment().is_none(),
        "expected an HTTPS relay host and optional port"
    );
    Ok(())
}

fn voucher_message(payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(VOUCHER_CONTEXT.len() + payload.len());
    message.extend_from_slice(VOUCHER_CONTEXT);
    message.extend_from_slice(payload);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn invite() -> ShareInvitation {
        ShareInvitation::issue_at("https://203.0.113.3:7000".parse().unwrap(), &SecretKey::generate(), ShareService::Com, 7001, NOW).unwrap()
    }

    #[test]
    fn address_round_trips_without_sending_proof_to_relay() {
        let invite = invite();
        let address = invite.address();
        assert!(address.starts_with("qccom://203.0.113.3:7000/"));
        let parsed = ShareInvitation::parse_at(&format!("  {address}\n"), NOW).unwrap();
        let voucher = verify_relay_voucher_at(parsed.voucher().token(), NOW).unwrap();
        assert_eq!(parsed.relay(), invite.relay());
        assert_eq!(voucher.host(), invite.voucher().host());
        assert_eq!(voucher.service(), ShareService::Com);
        assert_eq!(voucher.number(), 7001);
        assert_eq!(voucher.expires_at(), NOW + SHARE_LIFETIME_SECS);
        assert!(voucher.verify_proof(parsed.proof()).is_ok());
        assert!(!voucher.token().contains(parsed.proof()));
        assert!(!format!("{parsed:?}").contains(parsed.proof()));
    }

    #[test]
    fn rejects_tampering_and_expired_invites() {
        let invite = invite();
        let voucher = invite.voucher();
        let mut bytes = URL_SAFE_NO_PAD.decode(voucher.token()).unwrap();
        bytes[33] = ShareService::Ftp as u8;
        assert!(verify_relay_voucher_at(&URL_SAFE_NO_PAD.encode(bytes), NOW).is_err());
        assert!(voucher.verify_proof(&URL_SAFE_NO_PAD.encode([1; 32])).is_err());
        assert!(ShareInvitation::parse_at(&invite.address(), voucher.expires_at()).is_err());
    }

    #[test]
    fn rejects_invalid_address_and_relay_shapes() {
        let invite = invite();
        let malformed = invite.address().replace("qccom://", "qccom://user@");
        assert!(ShareInvitation::parse_at(&malformed, NOW).is_err());
        assert!(ShareInvitation::parse_at("203.0.113.3", NOW).is_err());
        assert!(ShareInvitation::parse_at("qccom://203.0.113.3:7000/invalid", NOW).is_err());
        assert!(ShareInvitation::issue_at("https://example.test/prefix".parse().unwrap(), &SecretKey::generate(), ShareService::Mqtt, 7002, NOW).is_err());
    }
}

//! Authenticated address-based port mappings and relay admission.

#[cfg(feature = "endpoint")]
pub mod mapping;

pub mod sharing;

#[cfg(feature = "server")]
#[doc(hidden)]
pub mod share_relay;

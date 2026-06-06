// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The caching `IpSigner` (`specs/05` §1.6/§3.5).
//!
//! Mirrors Go `network/peer/ip_signer.go`. Returns the [`SignedIp`] for the
//! current dynamic IP, re-signing only when the IP/port changes. The cached
//! value lives in an [`arc_swap::ArcSwapOption`] so reads on the hot connect
//! path are lock-free (Go uses an `RWMutex`; the cached `SignedIP` values are
//! immutable once produced, so a lock-free swap is equivalent — `specs/05` §10).

use std::net::IpAddr;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use ava_crypto::bls::Signer;

use super::ip::{SignedIp, UnsignedIp};
use crate::error::Result;
use crate::identity::Identity;

/// A monotonic Unix-seconds clock. Abstracted so tests can inject time.
pub trait Clock: Send + Sync {
    /// The current Unix-seconds timestamp.
    fn unix(&self) -> u64;
}

/// The default wall-clock implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// Caches the [`SignedIp`] for the node's current dynamic IP, re-signing on
/// change. Cheap to share across tasks (lock-free reads).
pub struct IpSigner {
    identity: Identity,
    bls_signer: Arc<dyn Signer>,
    clock: Arc<dyn Clock>,
    cached: ArcSwapOption<SignedIp>,
}

impl IpSigner {
    /// Build an `IpSigner` for `identity`, signing BLS proofs with `bls_signer`.
    #[must_use]
    pub fn new(identity: Identity, bls_signer: Arc<dyn Signer>, clock: Arc<dyn Clock>) -> IpSigner {
        IpSigner {
            identity,
            bls_signer,
            clock,
            cached: ArcSwapOption::from(None),
        }
    }

    /// `GetSignedIP` — return the cached signed IP if it still matches
    /// `(ip, port)`, otherwise sign the current IP at the clock's timestamp and
    /// cache it.
    ///
    /// # Errors
    /// [`crate::Error::Signing`] if signing fails.
    pub fn get_signed_ip(&self, ip: IpAddr, port: u16) -> Result<Arc<SignedIp>> {
        if let Some(cached) = self.cached.load_full()
            && cached.unsigned.ip == ip
            && cached.unsigned.port == port
        {
            return Ok(cached);
        }

        let unsigned = UnsignedIp::new(ip, port, self.clock.unix());
        let tls_signer = self.identity.tls_signing_key()?;
        let signed = Arc::new(unsigned.sign(&tls_signer, self.bls_signer.as_ref())?);
        self.cached.store(Some(Arc::clone(&signed)));
        Ok(signed)
    }
}

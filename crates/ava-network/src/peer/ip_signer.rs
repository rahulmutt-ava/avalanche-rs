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
///
/// The peer actor (M2.14+) uses the same injected clock for `my_time`, the
/// clock-skew check, and the version-compatibility floor selection (`specs/26`
/// §3.1), so a test can drive the clock across `upgrade_time` deterministically.
pub trait Clock: Send + Sync {
    /// The current Unix-seconds timestamp.
    fn unix(&self) -> u64;

    /// The current time as a [`std::time::SystemTime`]. Defaults to
    /// `UNIX_EPOCH + unix()s` so an impl that only knows seconds stays
    /// consistent with [`Clock::unix`].
    fn now_system(&self) -> std::time::SystemTime {
        std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(self.unix()))
            .unwrap_or(std::time::UNIX_EPOCH)
    }
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

    fn now_system(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
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

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use ava_crypto::bls::LocalSigner;

    use super::*;
    use crate::identity::Identity;

    #[test]
    fn get_signed_ip_succeeds_with_rsa_identity() {
        // Task 8: the live validator's genesis staker slot presents an RSA
        // staking identity. `get_signed_ip` calls `Identity::tls_signing_key`
        // on the node's own connect path — it must succeed for RSA
        // identities, not just the ECDSA template `Identity::generate` uses.
        let cert = include_str!("../../tests/testdata/rsa_staker.crt");
        let key = include_str!("../../tests/testdata/rsa_staker.key");
        let identity = Identity::from_pem(cert, key).expect("rsa identity");
        let bls_signer = Arc::new(LocalSigner::generate().expect("bls signer"));
        let signer = IpSigner::new(identity, bls_signer, Arc::new(SystemClock));

        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        signer
            .get_signed_ip(ip, 9651)
            .expect("get_signed_ip must succeed for an rsa staking identity");
    }
}

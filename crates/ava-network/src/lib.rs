// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-network` — the P2P networking stack (TLS transport + node identity).
//!
//! Tier T2a (wire). Owning spec: `specs/05-networking-p2p.md` (PRIMARY).
//!
//! **Wave B scope (M2.7–M2.10):** this crate currently provides only the TLS +
//! identity foundation — the rest of the peer actor / runtime (M2.11+) lands in
//! a later wave.
//!
//! - [`peer::tls_config`] — TLS 1.3-only mutual `rustls` server/client configs.
//! - [`peer::verifier`] — the custom leaf-key cert verifiers (P-256 / RSA policy,
//!   no CA chain). Lives in a clearly-named `danger` submodule because it
//!   overrides the default certificate-chain verification (`specs/05` §1.6/§4.4).
//! - [`peer::upgrader`] — `Upgrader::upgrade` → `(NodeId, TlsStream, Certificate)`.
//! - [`peer::ip`] / [`peer::ip_signer`] — `UnsignedIp`/`SignedIp` byte layout +
//!   TLS/BLS signing + the caching `IpSigner` (`specs/05` §1.6/§3.5).
//!
//! The P2P wire protocol is a HARD byte-exact compatibility surface: a Rust node
//! must be indistinguishable from a Go peer (`specs/05` §1). The custom rustls
//! verifier replicates Go's `InsecureSkipVerify + VerifyConnection` leaf-key
//! policy exactly — it requires no `unsafe`, so this crate keeps the
//! workspace-wide `#![forbid(unsafe_code)]` (`specs/00` §7.6).

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod identity;
pub mod network;
pub mod peer;
pub mod router;

pub use error::{Error, Result};
pub use identity::Identity;

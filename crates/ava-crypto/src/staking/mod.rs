// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking TLS certificates + NodeID derivation.
//!
//! Submodules:
//! - [`tls`] — cert + key generation via `rcgen` (the exact Go template) (M0.20)
//! - [`parse`] — strict ASN.1 parse + RSA/ECDSA policy checks (M0.20)
//! - [`verify`] — `check_signature` (RSA-PKCS1v15 / ECDSA) (M0.20)
//! - [`certificate`] — the `Certificate { raw, public_key }` type (M0.20)
//!
//! `node_id_from_cert(cert_der) = NodeId::from(ripemd160(sha256(DER)))` lives
//! here (depends on `ava_types::NodeId` + hashing).
//! Owning spec: `specs/03-core-primitives.md` §3.6, `specs/25` §2.1, §8.1.

pub mod certificate;
pub mod parse;
pub mod tls;
pub mod verify;

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-side Warp / ICM surface.
//!
//! The **generic** Warp primitives now live in the standalone [`ava_warp`] crate
//! (specs 20 §1) — the envelope ([`Message`], [`UnsignedMessage`], [`Signature`],
//! [`BitSetSignature`]), the addressed-call [`payload`] layer, the ACP-77
//! [`message`] registry, the local [`signer`], and the pure bit-set/quorum
//! verification primitives. This module re-exports them so existing
//! `crate::warp::{...}` paths keep resolving, and retains the
//! **L1-lifecycle-specific glue** in [`verifier`] (the registry-payload parse +
//! the [`WarpSignatureVerifier`](verifier::WarpSignatureVerifier) executor seam,
//! both over the P-Chain [`Error`](crate::Error)).

// The ACP-77 registry payloads (`RegistryPayload`, `RegisterL1Validator`, …),
// the addressed-call payload layer, and the local signer are unchanged; re-export
// the `ava-warp` modules under their established `crate::warp::*` paths.
pub use ava_warp::{message, payload, signer};
// The Warp envelope + signature types and codec version.
pub use ava_warp::{BitSetSignature, CODEC_VERSION, Message, Signature, UnsignedMessage};

pub mod verifier;

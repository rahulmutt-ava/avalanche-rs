// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![forbid(unsafe_code)]

//! Avalanche Warp Messaging / Interchain Messaging (ICM) — the generic
//! BLS-multisignature cross-chain primitive (`vms/platformvm/warp/**`, specs 20).
//!
//! A chain produces an [`UnsignedMessage`], the source subnet's validators each
//! BLS-sign it, an aggregator collects the signatures into a [`BitSetSignature`],
//! and a verifying chain checks the aggregate against the source subnet's
//! canonical validator set at a pinned P-Chain height. It is consensus-critical
//! and **byte-exact** with the Go node: the message format is the avalanche
//! linear codec (NOT protobuf), the canonical validator ordering is fixed, and
//! the quorum rule must match bit-for-bit.
//!
//! This crate gathers machinery that is diffuse across ~10 Go files into one
//! place, reused by P-Chain (`08`), the EVM warp precompile (`10`), and SAE
//! (`11`). It owns three nested linear codecs (specs 20 §3.1), each with its own
//! [`Manager`](ava_codec::manager::Manager) and type-id numbering starting at 0:
//!
//! - The **envelope** ([`Message`] / [`UnsignedMessage`] / [`Signature`] /
//!   [`BitSetSignature`], this module).
//! - The **addressed-call** layer ([`payload`]).
//! - The **ACP-77 registry** payloads ([`message`]).
//!
//! plus the local [`signer`] and the pure bit-set/quorum [`verifier`] primitives.
//!
//! > **Module naming.** specs 20 §1 sketches a `registry.rs` module for the
//! > ACP-77 payloads; this crate keeps the original P-Chain name [`message`] to
//! > minimise churn when re-pointing `ava-platformvm` (the wire layout and type
//! > registry are identical).

pub mod error;
pub mod message;
pub mod payload;
pub mod signer;
pub mod verifier;

use std::sync::{Arc, OnceLock};

// The `AvaCodec` derive (re-exported via `ava_codec`) emits code that resolves
// `ava_codec_derive` paths; keep the crate in the dependency graph.
use ava_codec_derive as _;

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_types::id::Id;

pub use error::{Error, Result};

/// `warp.CodecVersion` — the single Warp envelope codec version (specs 20 §2).
pub const CODEC_VERSION: u16 = 0;

/// `warp.UnsignedMessage` — the standard unsigned Warp message
/// (`vms/platformvm/warp/unsigned_message.go`).
///
/// `payload` is the opaque bytes of a [`payload::WarpPayload`] (an
/// [`AddressedCall`](payload::AddressedCall) for the ACP-77 flows).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct UnsignedMessage {
    /// `NetworkID` — the network this message is bound to.
    #[codec]
    pub network_id: u32,
    /// `SourceChainID` — the chain that emitted the message.
    #[codec]
    pub source_chain_id: Id,
    /// `Payload` — the opaque inner payload bytes.
    #[codec]
    pub payload: Vec<u8>,
}

impl UnsignedMessage {
    /// `warp.ParseUnsignedMessage` — decode a standalone unsigned message from its
    /// version-prefixed wire bytes (`vms/platformvm/warp/unsigned_message.go`).
    ///
    /// Used by the C-Chain warp precompile's `handlePrecompileAccept` (specs 20
    /// §3.1/§7), which reconstructs the unsigned message from a `SendWarpMessage`
    /// log's data.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on an unknown
    /// version, trailing bytes, or a short read.
    pub fn parse(bytes: &[u8]) -> CodecResult<Self> {
        let mut m = Self::default();
        warp_codec().unmarshal(bytes, &mut m)?;
        Ok(m)
    }

    /// `UnsignedMessage.Bytes()` — the marshaled wire bytes (version-prefixed).
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure (cannot occur with a growable buffer).
    pub fn marshal(&self) -> CodecResult<Vec<u8>> {
        warp_codec().marshal(CODEC_VERSION, self)
    }

    /// `UnsignedMessage.ID()` — the message identifier, `sha256(bytes)`
    /// (single-pass; specs 20 §2.1, `vms/platformvm/warp/unsigned_message.go`).
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) if [`Self::marshal`]
    /// fails.
    pub fn id(&self) -> CodecResult<Id> {
        Ok(Id::from(ava_crypto::hashing::sha256(&self.marshal()?)))
    }
}

/// `warp.Message` — an [`UnsignedMessage`] plus its aggregate [`Signature`]
/// (`vms/platformvm/warp/message.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    /// The embedded unsigned message.
    #[codec]
    pub unsigned_message: UnsignedMessage,
    /// The aggregate BLS signature over [`UnsignedMessage::marshal`].
    #[codec]
    pub signature: Signature,
}

impl Message {
    /// `warp.ParseMessage` — decode a full Warp message from `bytes`.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on an unknown
    /// version, trailing bytes, or a short read.
    pub fn parse(bytes: &[u8]) -> CodecResult<Self> {
        let mut m = Self::default();
        warp_codec().unmarshal(bytes, &mut m)?;
        Ok(m)
    }

    /// `warp.NewMessage(...).Bytes()` — the marshaled wire bytes.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure.
    pub fn marshal(&self) -> CodecResult<Vec<u8>> {
        warp_codec().marshal(CODEC_VERSION, self)
    }
}

/// `warp.Signature` — the registered signature interface
/// (`vms/platformvm/warp/signature.go`). Only [`BitSetSignature`] (type_id 0) is
/// registered, mirroring Go's codec.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Signature {
    /// `BitSetSignature` (type_id 0) — a bit-set + aggregate BLS signature.
    #[codec(type_id = 0)]
    BitSet(BitSetSignature),
}

impl Default for Signature {
    fn default() -> Self {
        Signature::BitSet(BitSetSignature::default())
    }
}

/// `warp.BitSetSignature` — a big-endian signer bit-set plus the aggregate BLS
/// signature (`vms/platformvm/warp/signature.go`).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
pub struct BitSetSignature {
    /// `Signers` — a big-endian byte slice encoding which validators signed.
    #[codec]
    pub signers: Vec<u8>,
    /// `Signature` — the 96-byte aggregate BLS signature.
    #[codec]
    pub signature: [u8; ava_crypto::bls::SIGNATURE_LEN],
}

impl Default for BitSetSignature {
    fn default() -> Self {
        Self {
            signers: Vec::new(),
            signature: [0u8; ava_crypto::bls::SIGNATURE_LEN],
        }
    }
}

/// The Warp-envelope codec manager (`vms/platformvm/warp/codec.go`).
///
/// An `i32::MAX`-max-slice manager registering only [`BitSetSignature`] (the lone
/// `Signature` implementation). Shared, lazily built.
fn warp_codec() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(|| {
        let m = Manager::new(ava_codec::MAX_SLICE_LEN);
        // Registration cannot fail for a fresh manager; fall back to an empty
        // manager rather than panicking in library code.
        let _ = m.register(CODEC_VERSION, Arc::new(LinearCodec::new()));
        m
    })
}

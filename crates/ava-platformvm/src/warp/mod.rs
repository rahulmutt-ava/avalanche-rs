// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The minimal P-side Warp / ICM primitives the ACP-77 L1 lifecycle consumes
//! (`vms/platformvm/warp/**`, specs 20 §2–§3.1, §6).
//!
//! ## Scope (as-built, M4.19)
//!
//! There is no dedicated `ava-warp` crate yet (specs 20 names `ava-warp::{payload,
//! message}` as the eventual home). M4.19 needs to *parse and structurally verify*
//! the Warp messages embedded in [`RegisterL1ValidatorTx`](crate::txs::RegisterL1ValidatorTx)
//! and [`SetL1ValidatorWeightTx`](crate::txs::SetL1ValidatorWeightTx), so this
//! module ports the **parsing layers** locally:
//!
//! - [`Message`] / [`UnsignedMessage`] — the outer Warp envelope
//!   (`vms/platformvm/warp/{message,unsigned_message}.go`).
//! - [`Signature`] — the [`BitSetSignature`] aggregate-BLS signature
//!   (`vms/platformvm/warp/signature.go`). Parsed here; **verified** by the
//!   injected [`verifier::WarpSignatureVerifier`] seam (M4.21/M4.22).
//! - [`payload::WarpPayload`] / [`payload::AddressedCall`] — the addressed-call
//!   wrapper (`vms/platformvm/warp/payload/**`), its own codec registry.
//! - [`message::RegistryPayload`] and friends — the ACP-77 registry payloads
//!   (`vms/platformvm/warp/message/**`, specs 20 §3.1), a *third* codec registry.
//!
//! The three registries each have their own [`Manager`](ava_codec::manager::Manager)
//! and their own type-id numbering starting at 0 (specs 20 §3.1 — "do not merge").
//!
//! > **Deferred (M4.21/M4.22):** the BLS aggregate-signature / quorum check
//! > against the source subnet's canonical validator set at the pinned P-Chain
//! > height. The verifier exposes it as the [`verifier::WarpSignatureVerifier`]
//! > trait so the parsing + structural checks here are exercised independently of
//! > the not-yet-ported `WarpSet`/quorum machinery.

pub mod message;
pub mod payload;
pub mod signer;
pub mod verifier;

use std::sync::{Arc, OnceLock};

use ava_codec::AvaCodec;
use ava_codec::error::Result;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_types::id::Id;

/// `warp.CodecVersion` — the single Warp envelope codec version (== the P-Chain
/// [`CODEC_VERSION`](crate::CODEC_VERSION)).
pub const CODEC_VERSION: u16 = crate::CODEC_VERSION;

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
    /// `UnsignedMessage.Bytes()` — the marshaled wire bytes (version-prefixed).
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure (cannot occur with a growable buffer).
    pub fn marshal(&self) -> Result<Vec<u8>> {
        warp_codec().marshal(CODEC_VERSION, self)
    }

    /// `UnsignedMessage.ID()` — the message identifier, `sha256(bytes)`
    /// (single-pass; specs 20 §2.1, `vms/platformvm/warp/unsigned_message.go`).
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) if [`Self::marshal`]
    /// fails.
    pub fn id(&self) -> Result<Id> {
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
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut m = Self::default();
        warp_codec().unmarshal(bytes, &mut m)?;
        Ok(m)
    }

    /// `warp.NewMessage(...).Bytes()` — the marshaled wire bytes.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure.
    pub fn marshal(&self) -> Result<Vec<u8>> {
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

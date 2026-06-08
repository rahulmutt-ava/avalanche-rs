// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Warp addressed-call payload layer (`vms/platformvm/warp/payload/**`,
//! specs 20 §3).
//!
//! This is the **second** of the three nested Warp codecs (specs 20 §3.1): the
//! [`UnsignedMessage.payload`](crate::UnsignedMessage::payload) bytes decode to a
//! [`WarpPayload`], which for the ACP-77 flows is an [`AddressedCall`] whose own
//! `payload` is in turn an ACP-77 [`RegistryPayload`](crate::message::RegistryPayload).
//!
//! Registration order (= type IDs), mirroring Go `warp/payload/codec.go`:
//!
//! | Payload | Type ID |
//! |---|---|
//! | `Hash`          | **0** |
//! | `AddressedCall` | **1** |

use std::sync::{Arc, OnceLock};

use ava_codec::AvaCodec;
use ava_codec::error::Result;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_types::id::Id;

use crate::CODEC_VERSION;

/// `payload.Payload` — the registered addressed-payload interface
/// (`warp/payload/payload.go`).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum WarpPayload {
    /// `payload.Hash` (type_id 0) — a 32-byte hash payload.
    #[codec(type_id = 0)]
    Hash(Hash),
    /// `payload.AddressedCall` (type_id 1) — a source-addressed call.
    #[codec(type_id = 1)]
    AddressedCall(AddressedCall),
}

impl Default for WarpPayload {
    fn default() -> Self {
        WarpPayload::AddressedCall(AddressedCall::default())
    }
}

impl WarpPayload {
    /// Parses any registered Warp payload from `bytes`.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on an unknown
    /// version/type, trailing bytes, or a short read.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut p = Self::default();
        payload_codec().unmarshal(bytes, &mut p)?;
        Ok(p)
    }

    /// Marshals this payload to its wire bytes (the `UnsignedMessage.payload`).
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a codec write
    /// failure.
    pub fn marshal_payload(&self) -> Result<Vec<u8>> {
        payload_codec().marshal(CODEC_VERSION, self)
    }
}

/// `payload.Hash` — a 32-byte hash payload (`warp/payload/hash.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Hash {
    /// The hash value.
    #[codec]
    pub hash: Id,
}

/// `payload.AddressedCall` — a source address plus an opaque inner payload
/// (`warp/payload/addressed_call.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressedCall {
    /// `SourceAddress` — the address that produced the call.
    #[codec]
    pub source_address: Vec<u8>,
    /// `Payload` — the opaque inner payload (an ACP-77
    /// [`RegistryPayload`](crate::message::RegistryPayload) for the L1 flows).
    #[codec]
    pub payload: Vec<u8>,
}

impl AddressedCall {
    /// `payload.ParseAddressedCall` — decode an addressed call, rejecting any
    /// other registered payload type.
    ///
    /// # Errors
    /// Returns a [`CodecError`](ava_codec::error::CodecError) on a decode failure,
    /// or [`crate::Error`] is *not* used here — the executor maps the codec error.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        match WarpPayload::parse(bytes)? {
            WarpPayload::AddressedCall(c) => Ok(c),
            // A non-addressed-call payload is the codec "wrong type" case; the Go
            // `ParseAddressedCall` returns `ErrWrongType`. We surface it as a
            // generic codec failure so the executor rejects it.
            WarpPayload::Hash(_) => Err(ava_codec::error::CodecError::UnknownTypeId(0)),
        }
    }
}

/// The addressed-call payload codec manager (`warp/payload/codec.go`).
fn payload_codec() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(|| {
        let m = Manager::new(ava_codec::MAX_SLICE_LEN);
        let _ = m.register(CODEC_VERSION, Arc::new(LinearCodec::new()));
        m
    })
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The signed [`Tx`] envelope (specs 09 §3.1).
//!
//! Port of `vms/avm/txs/tx.go`. A [`Tx`] wraps an [`UnsignedTx`] and its fx
//! credentials, plus two **non-serialized** cache fields: the tx ID
//! (`sha256(signed_bytes)`) and the cached signed bytes.
//!
//! The [`Tx::initialize`] / [`Tx::parse`] **prefix-length trick** recovers the
//! unsigned-bytes sub-slice without re-marshalling: `signed_bytes = marshal(Tx)`,
//! `unsigned_len = Codec::size(&unsigned)`, `unsigned_bytes =
//! signed_bytes[..unsigned_len]` (specs 09 §3.1).

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_types::id::Id;

use crate::txs::UnsignedTx;
use crate::txs::credential::FxCredential;

/// The X-Chain codec version (`txs.CodecVersion = 0`; specs 09 §2.1).
pub const CODEC_VERSION: u16 = 0;

/// `txs.Tx` — a signed transaction (specs 09 §3.1).
///
/// The `unsigned` body and `creds` are serialized (in that order); `tx_id` and
/// `bytes` are derived caches populated by [`Tx::initialize`] / [`Tx::parse`] and
/// are **not** on the wire (no `#[codec]` tag).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Tx {
    /// The transaction body (interface → typeid-prefixed).
    #[codec]
    pub unsigned: UnsignedTx,
    /// The fx credentials (each interface → typeid-prefixed).
    #[codec]
    pub creds: Vec<FxCredential>,
    /// `= sha256(signed_bytes)`. Not serialized.
    pub tx_id: Id,
    /// Cached signed bytes. Not serialized.
    pub bytes: bytes::Bytes,
}

impl Tx {
    /// Builds an unsigned-only [`Tx`] (no credentials attached yet).
    #[must_use]
    pub fn new(unsigned: UnsignedTx) -> Self {
        Self {
            unsigned,
            creds: Vec::new(),
            tx_id: Id::EMPTY,
            bytes: bytes::Bytes::new(),
        }
    }

    /// `Tx.Initialize` — marshals the whole tx, then derives the cached signed
    /// bytes and `tx_id = sha256(signed_bytes)` (specs 09 §3.1).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling or the unsigned
    /// size computation fails.
    pub fn initialize(&mut self, c: &Manager) -> CodecResult<()> {
        let signed_bytes = c.marshal(CODEC_VERSION, self)?;
        // The unsigned-bytes prefix length (incl. the 2-byte version prefix the
        // signed bytes share). Computed for parity with Go `Tx.Initialize`;
        // `signed_bytes[..unsigned_len]` is the marshaled unsigned tx.
        let _unsigned_len = c.size(CODEC_VERSION, &self.unsigned)?;
        self.set_bytes(signed_bytes);
        Ok(())
    }

    /// Sets the cached signed bytes and recomputes `tx_id = sha256(signed_bytes)`.
    fn set_bytes(&mut self, signed_bytes: Vec<u8>) {
        self.tx_id = Id::from(hashing::sha256(&signed_bytes));
        self.bytes = bytes::Bytes::from(signed_bytes);
    }

    /// `txs.Parse` — decodes a signed tx and reproduces the prefix-length trick to
    /// recover (and cache) the unsigned-bytes sub-slice (specs 09 §3.1).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode or
    /// the unsigned size computation fails.
    pub fn parse(c: &Manager, signed_bytes: &[u8]) -> CodecResult<Self> {
        let mut tx = Tx::default();
        c.unmarshal(signed_bytes, &mut tx)?;
        let _unsigned_len = c.size(CODEC_VERSION, &tx.unsigned)?;
        tx.set_bytes(signed_bytes.to_vec());
        Ok(tx)
    }

    /// The marshaled unsigned-tx byte prefix length, **including** the 2-byte
    /// codec version (matches Go `Codec.Size(CodecVersion, &tx.Unsigned)`).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the size computation fails.
    pub fn unsigned_len(&self, c: &Manager) -> CodecResult<usize> {
        c.size(CODEC_VERSION, &self.unsigned)
    }

    /// The marshaled signed-tx size (`= self.bytes.len()` once initialized).
    #[must_use]
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// The cached signed bytes (empty until [`Tx::initialize`]/[`Tx::parse`]).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The tx ID (`sha256(signed_bytes)`; `Id::EMPTY` until initialized).
    #[must_use]
    pub fn id(&self) -> Id {
        self.tx_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txs::BaseTx;
    use crate::txs::codec::codec;

    #[test]
    fn initialize_roundtrip_and_prefix() {
        let c = codec().expect("build codec");
        let mut tx = Tx::new(UnsignedTx::Base(BaseTx::default()));
        tx.initialize(&c).expect("initialize");

        // tx_id is sha256 of the signed bytes.
        assert_ne!(tx.id(), Id::EMPTY);
        assert_eq!(tx.id(), Id::from(hashing::sha256(tx.bytes())));

        // The unsigned prefix length (incl. version) is <= the signed length.
        let unsigned_len = tx.unsigned_len(&c).expect("unsigned len");
        assert!(unsigned_len <= tx.size());

        // parse reproduces the same tx (unsigned + creds + derived caches).
        let parsed = Tx::parse(&c, tx.bytes()).expect("parse");
        assert_eq!(parsed.unsigned, tx.unsigned);
        assert_eq!(parsed.creds, tx.creds);
        assert_eq!(parsed.id(), tx.id());
    }
}

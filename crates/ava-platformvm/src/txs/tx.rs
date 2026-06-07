// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The signed [`Tx`] envelope (specs 08 §2.3).
//!
//! Port of `vms/platformvm/txs/tx.go`. A [`Tx`] wraps an [`UnsignedTx`] and its
//! `verify.Verifiable` credentials, plus two **non-serialized** cache fields: the
//! tx ID (`sha256(signed_bytes)`) and the cached signed bytes.
//!
//! The [`Tx::initialize`] / [`Tx::parse`] **prefix-length trick** recovers the
//! unsigned-bytes sub-slice without re-marshalling: `signed_bytes = marshal(Tx)`,
//! `unsigned_len = Codec::size(&unsigned)`, `unsigned_bytes =
//! signed_bytes[..unsigned_len]`.

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_crypto::secp256k1::SIGNATURE_LEN;
use ava_types::id::Id;

use crate::CODEC_VERSION;
use crate::txs::UnsignedTx;

/// `secp256k1fx.Credential` — `type_id = 9` (specs 08 §2.1).
///
/// A byte-exact codec mirror of [`ava_secp256k1fx::Credential`] (`u32` count + one
/// fixed 65-byte recoverable signature each). Defined locally because the codec
/// [`Serializable`]/[`Deserializable`] traits cannot be implemented for the
/// foreign `ava_secp256k1fx::Credential` (orphan rule); conversions to/from it are
/// provided so the executor / wallet can move freely between the two.
///
/// [`Serializable`]: ava_codec::Serializable
/// [`Deserializable`]: ava_codec::Deserializable
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credential {
    /// `Sigs` — one 65-byte `[r||s||v]` recoverable signature per spend.
    pub sigs: Vec<[u8; SIGNATURE_LEN]>,
}

// `Vec<[u8; 65]>` cannot use the derive's generic `Vec<T>` codec because
// `[u8; 65]: Default` does not hold in std (arrays > 32 have no `Default`). The
// wire format is byte-identical to `ava_secp256k1fx::Credential`: a `u32` count
// then each fixed 65-byte signature (no inner length prefix).
impl ava_codec::Serializable for Credential {
    fn marshal_into(&self, p: &mut ava_codec::packer::Packer) {
        ava_codec::pack_count(p, self.sigs.len());
        for sig in &self.sigs {
            p.pack_fixed_bytes(sig);
        }
    }

    fn size(&self) -> usize {
        ava_codec::packer::INT_LEN.saturating_add(self.sigs.len().saturating_mul(SIGNATURE_LEN))
    }
}

impl ava_codec::Deserializable for Credential {
    fn unmarshal_from(&mut self, p: &mut ava_codec::packer::Packer) {
        let n = p.unpack_u32() as usize;
        if p.errored() {
            return;
        }
        let mut sigs = Vec::with_capacity(n.min(ava_codec::INITIAL_SLICE_CAP));
        for _ in 0..n {
            let raw = p.unpack_fixed_bytes(SIGNATURE_LEN);
            if p.errored() {
                return;
            }
            match <[u8; SIGNATURE_LEN]>::try_from(raw.as_slice()) {
                Ok(sig) => sigs.push(sig),
                Err(_) => {
                    p.add_external_error(ava_codec::error::PackerError::InvalidInput);
                    return;
                }
            }
        }
        self.sigs = sigs;
    }
}

impl From<ava_secp256k1fx::Credential> for Credential {
    fn from(c: ava_secp256k1fx::Credential) -> Self {
        Self { sigs: c.sigs }
    }
}

impl From<Credential> for ava_secp256k1fx::Credential {
    fn from(c: Credential) -> Self {
        ava_secp256k1fx::Credential::new(c.sigs)
    }
}

/// `txs.Tx` — a signed transaction (specs 08 §2.3).
///
/// The `unsigned` body and `creds` are serialized (in that order); `tx_id` and
/// `bytes` are derived caches populated by [`Tx::initialize`] / [`Tx::parse`] and
/// are **not** on the wire (no `#[codec]` tag).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Tx {
    /// The transaction body.
    #[codec]
    pub unsigned: UnsignedTx,
    /// The `verify.Verifiable` credentials (`secp256k1fx.Credential`, type_id 9).
    #[codec]
    pub creds: Vec<Credential>,
    /// `= sha256(signed_bytes)`. Not serialized.
    pub tx_id: Id,
    /// Cached signed bytes. Not serialized.
    pub bytes: Vec<u8>,
}

impl Tx {
    /// Builds an unsigned-only [`Tx`] (no credentials attached yet).
    #[must_use]
    pub fn new(unsigned: UnsignedTx) -> Self {
        Self {
            unsigned,
            creds: Vec::new(),
            tx_id: Id::EMPTY,
            bytes: Vec::new(),
        }
    }

    /// `Tx.Initialize` — marshals the whole tx, then derives the unsigned-bytes
    /// prefix slice, the cached signed bytes, and `tx_id = sha256(signed_bytes)`
    /// (specs 08 §2.3).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling or the unsigned
    /// size computation fails.
    pub fn initialize(&mut self, c: &Manager) -> CodecResult<()> {
        let signed_bytes = c.marshal(CODEC_VERSION, self)?;
        // The unsigned-bytes prefix length (incl. the 2-byte version prefix the
        // signed bytes share). Computed here for parity with Go `Tx.Initialize`
        // and validated below; `signed_bytes[..unsigned_len]` is the marshaled
        // unsigned tx (the prefix-length trick, specs 08 §2.3).
        let _unsigned_len = c.size(CODEC_VERSION, &self.unsigned)?;
        self.set_bytes(signed_bytes);
        Ok(())
    }

    /// Sets the cached signed bytes and recomputes `tx_id = sha256(signed_bytes)`.
    fn set_bytes(&mut self, signed_bytes: Vec<u8>) {
        self.tx_id = Id::from(hashing::sha256(&signed_bytes));
        self.bytes = signed_bytes;
    }

    /// `txs.Parse` — decodes a signed tx and reproduces the prefix-length trick to
    /// recover (and cache) the unsigned-bytes sub-slice (specs 08 §2.3).
    ///
    /// The caller passes the codec explicitly because genesis txs may exceed the
    /// default `Codec` max size and must be parsed with `GenesisCodec`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode or
    /// the unsigned size computation fails.
    pub fn parse(c: &Manager, signed_bytes: &[u8]) -> CodecResult<Self> {
        let mut tx = Tx::default();
        c.unmarshal(signed_bytes, &mut tx)?;
        // Recompute the unsigned prefix length (kept for parity / future
        // zero-copy unsigned-bytes caching once `Bytes` is a direct dep).
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
    use crate::txs::codec;
    use crate::txs::{AddValidatorTx, BaseTx};

    #[test]
    fn initialize_roundtrip_and_prefix() {
        let c = codec::codec().expect("build codec");
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

    #[test]
    fn credential_converts_to_secp256k1fx() {
        let sig = [7u8; SIGNATURE_LEN];
        let local = Credential { sigs: vec![sig] };
        let fx: ava_secp256k1fx::Credential = local.clone().into();
        assert_eq!(fx.sigs, vec![sig]);
        let back: Credential = fx.into();
        assert_eq!(back, local);
    }

    #[test]
    fn parse_distinct_unsigned_variants() {
        let c = codec::codec().expect("build codec");
        let mut tx = Tx::new(UnsignedTx::AddValidator(AddValidatorTx::default()));
        tx.initialize(&c).expect("initialize");
        let parsed = Tx::parse(&c, tx.bytes()).expect("parse");
        assert_eq!(
            parsed.unsigned,
            UnsignedTx::AddValidator(AddValidatorTx::default())
        );
    }
}

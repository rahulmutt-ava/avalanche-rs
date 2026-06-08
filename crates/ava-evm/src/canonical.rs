// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `CanonicalStore` (G6, spec 10 §3/§17.7): the non-state block-metadata store
//! the [`EvmBlock`](crate::block::EvmBlock) lifecycle drives on `accept`.
//!
//! # The G6 contract
//!
//! Snowman owns fork choice: acceptance is **linear**, there are **no reorgs**,
//! and `reject` just drops an uncommitted Firewood proposal. This store keeps the
//! block tables (headers / canonical number<->hash index / bodies / receipts) and
//! a tip pointer consistent **without** reth's `TreeState` / staged-sync pipeline
//! / `forkchoiceUpdated`. The sole writer is [`CanonicalStore::append_canonical`],
//! invoked once per accept, advancing strictly by `+1` height — so the
//! number<->hash index can never disagree with Firewood's committed tip
//! (**G1 invariant:** `LAST_CANONICAL == last_accepted.height`).
//!
//! # Backend choice (as-built deviation from §17.7's reth-db MDBX sketch)
//!
//! Spec §17.7 sketches a `reth_db::DatabaseEnv` (MDBX) + `StaticFileProvider`. We
//! instead back the store with an [`ava_database`] KV ([`DynDatabase`]) per the
//! M6.9 scope note: the G6 contract is "non-state block metadata only, never
//! state/trie tables" — **not** "must be reth's MDBX schema". Reasons:
//!
//! 1. Pulling reth-db's MDBX `DatabaseEnv` + table schemas + `tx_mut`/`StaticFile`
//!    through the G0 facade is a large surface for a writer this thin.
//! 2. `ava-evm` already links Firewood with the **global ethhash compile switch**
//!    flipped on; co-loading reth's MDBX block store is an avoidable risk.
//!
//! The "tables" are realized as key-prefixed KV namespaces (one [`DynDatabase`],
//! prefixed keys), which satisfies the G6 contract and the lifecycle tests.
//! Migrating to reth-db's on-disk schema (so reth's `BlockReader` can read history
//! directly) is deferred to a later RPC/history task (M6.23/M6.24); the public API
//! here (`append_canonical` / `canonical_hash` / `height_of` / `last_canonical`)
//! is the seam that migration would re-implement.

use std::sync::Arc;

use ava_database::{DynDatabase, Error as DbError};
use ava_evm_reth::B256;

use crate::error::{Error, Result};

/// KV namespace prefixes — the analogues of reth's `Headers` / `CanonicalHeaders`
/// / `HeaderNumbers` / `BlockBodyIndices` / receipt tables and the `ChainState`
/// tip pointer (spec 10 §17.7). One byte each so keys never collide.
mod prefix {
    /// `number (BE u64) -> header commitment` (reth `Headers`).
    pub const HEADER: u8 = 0x01;
    /// `number (BE u64) -> block hash` (reth `CanonicalHeaders`).
    pub const CANONICAL: u8 = 0x02;
    /// `block hash (32) -> number (BE u64)` (reth `HeaderNumbers`).
    pub const NUMBER: u8 = 0x03;
    /// `number (BE u64) -> body bytes` (reth `BlockBodyIndices` + `Transactions`).
    pub const BODY: u8 = 0x04;
    /// `number (BE u64) -> receipts bytes` (reth receipt table / static file).
    pub const RECEIPTS: u8 = 0x05;
    /// Singleton tip pointer (reth `ChainState[LAST_CANONICAL]`).
    pub const TIP: u8 = 0x06;
}

/// The singleton key for the canonical tip height (`ChainState[LAST_CANONICAL]`).
const TIP_KEY: [u8; 1] = [prefix::TIP];

/// A `number`-keyed table key: `prefix || number_be`.
fn num_key(p: u8, number: u64) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = p;
    k[1..].copy_from_slice(&number.to_be_bytes());
    k
}

/// A `hash`-keyed table key: `prefix || hash`.
fn hash_key(p: u8, hash: &B256) -> [u8; 33] {
    let mut k = [0u8; 33];
    k[0] = p;
    k[1..].copy_from_slice(hash.as_slice());
    k
}

/// The non-state block-metadata store (G6). Writes only block headers / bodies /
/// receipts / the number<->hash index / a tip pointer — **never** state or trie
/// tables (Firewood is the EVM state-of-record, the G1 invariant).
pub struct CanonicalStore {
    /// The KV backend (`ava-database`); see the module-level backend-choice note.
    db: Arc<dyn DynDatabase>,
}

impl CanonicalStore {
    /// Builds a store over the given KV backend.
    #[must_use]
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self { db }
    }

    /// Appends an accepted block to the canonical tables and advances the tip
    /// (the only writer; called from [`EvmBlock::accept`](crate::block::EvmBlock),
    /// **after** the Firewood commit). Writes header / body / receipts +
    /// `number->hash` + `hash->number` + the tip pointer. Never touches
    /// state/trie tables (G1).
    ///
    /// Acceptance is linear, so `number` must be exactly `last_canonical + 1`
    /// (genesis ⇒ the first accepted block is height 1; the tip starts unset).
    ///
    /// # Errors
    /// Returns an error if a KV write fails, or [`Error::NilTx`] (an invariant
    /// stand-in) if `number` does not strictly follow the current tip (a linearity
    /// bug, never expected on the accept path).
    pub fn append_canonical(
        &self,
        number: u64,
        hash: B256,
        header: B256,
        body: &[u8],
        receipts: &[u8],
    ) -> Result<()> {
        // Linearity guard: strictly tip + 1 (or height 1 when unset).
        let expected = match self.last_canonical()? {
            Some(tip) => tip.checked_add(1).ok_or(Error::FeeOverflow)?,
            None => 1,
        };
        if number != expected {
            return Err(Error::NilTx);
        }

        // We store the header's 32-byte commitment rather than the full RLP (the
        // full SealedBlock/header lands when reth-db history wiring does, see the
        // module note); the lifecycle/index contract only needs the hash here.
        self.put(&num_key(prefix::HEADER, number), header.as_slice())?;
        self.put(&num_key(prefix::CANONICAL, number), hash.as_slice())?;
        self.put(&hash_key(prefix::NUMBER, &hash), &number.to_be_bytes())?;
        self.put(&num_key(prefix::BODY, number), body)?;
        self.put(&num_key(prefix::RECEIPTS, number), receipts)?;
        // Advance the tip pointer LAST (so a partial write never claims a height
        // whose index rows are missing).
        self.put(&TIP_KEY, &number.to_be_bytes())?;
        Ok(())
    }

    /// The current canonical tip height (`ChainState[LAST_CANONICAL]`), or `None`
    /// when nothing has been accepted yet.
    ///
    /// # Errors
    /// Returns an error if the KV read fails or the stored tip is malformed.
    pub fn last_canonical(&self) -> Result<Option<u64>> {
        match self.get(&TIP_KEY)? {
            Some(bytes) => Ok(Some(decode_u64(&bytes)?)),
            None => Ok(None),
        }
    }

    /// The canonical block hash at `number` (reth `CanonicalHeaders`), or `None`.
    ///
    /// # Errors
    /// Returns an error if the KV read fails or the stored value is not 32 bytes.
    pub fn canonical_hash(&self, number: u64) -> Result<Option<B256>> {
        self.read_b256(&num_key(prefix::CANONICAL, number))
    }

    /// The height of the block with `hash` (reth `HeaderNumbers`), or `None`.
    ///
    /// # Errors
    /// Returns an error if the KV read fails or the stored value is malformed.
    pub fn height_of(&self, hash: B256) -> Result<Option<u64>> {
        match self.get(&hash_key(prefix::NUMBER, &hash))? {
            Some(bytes) => Ok(Some(decode_u64(&bytes)?)),
            None => Ok(None),
        }
    }

    /// The header commitment stored at `number`, or `None`.
    ///
    /// # Errors
    /// Returns an error if the KV read fails or the stored value is not 32 bytes.
    pub fn header_at(&self, number: u64) -> Result<Option<B256>> {
        self.read_b256(&num_key(prefix::HEADER, number))
    }

    /// Reads a 32-byte value at `key`, mapping a non-32-byte value to an error.
    fn read_b256(&self, key: &[u8]) -> Result<Option<B256>> {
        match self.get(key)? {
            Some(bytes) if bytes.len() == 32 => Ok(Some(B256::from_slice(&bytes))),
            Some(_) => Err(Error::NilTx),
            None => Ok(None),
        }
    }

    /// `DynDatabase::get` mapping `NotFound` to `None`.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.db.get(key) {
            Ok(v) => Ok(Some(v)),
            Err(DbError::NotFound) => Ok(None),
            Err(e) => Err(Error::GenesisParse(e.to_string())),
        }
    }

    /// `DynDatabase::put`.
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.db
            .put(key, value)
            .map_err(|e| Error::GenesisParse(e.to_string()))
    }
}

/// Decodes a big-endian `u64` from an 8-byte KV value.
fn decode_u64(bytes: &[u8]) -> Result<u64> {
    let arr: [u8; 8] = bytes.try_into().map_err(|_| Error::NilTx)?;
    Ok(u64::from_be_bytes(arr))
}

#[cfg(test)]
mod tests {
    use ava_database::MemDb;

    use super::*;

    fn store() -> CanonicalStore {
        CanonicalStore::new(Arc::new(MemDb::new()))
    }

    #[test]
    fn empty_tip_is_none() {
        let s = store();
        assert_eq!(s.last_canonical().expect("tip"), None);
        assert_eq!(s.canonical_hash(1).expect("hash"), None);
        assert_eq!(s.height_of(B256::ZERO).expect("num"), None);
    }

    #[test]
    fn append_advances_and_indexes() {
        let s = store();
        let h1 = B256::repeat_byte(0x11);
        let hdr1 = B256::repeat_byte(0xa1);
        s.append_canonical(1, h1, hdr1, b"body1", b"rcpt1")
            .expect("append 1");
        assert_eq!(s.last_canonical().expect("tip"), Some(1));
        assert_eq!(s.canonical_hash(1).expect("hash"), Some(h1));
        assert_eq!(s.height_of(h1).expect("num"), Some(1));
        assert_eq!(s.header_at(1).expect("hdr"), Some(hdr1));

        let h2 = B256::repeat_byte(0x22);
        s.append_canonical(2, h2, B256::repeat_byte(0xa2), b"body2", b"rcpt2")
            .expect("append 2");
        assert_eq!(s.last_canonical().expect("tip"), Some(2));
        assert_eq!(s.canonical_hash(2).expect("hash"), Some(h2));
    }

    #[test]
    fn non_plus_one_append_is_rejected() {
        let s = store();
        // First accepted block must be height 1, not 0 or 2.
        assert!(
            s.append_canonical(0, B256::ZERO, B256::ZERO, &[], &[])
                .is_err()
        );
        assert!(
            s.append_canonical(2, B256::ZERO, B256::ZERO, &[], &[])
                .is_err()
        );
        s.append_canonical(1, B256::repeat_byte(1), B256::ZERO, &[], &[])
            .expect("height 1");
        // Then a gap is rejected.
        assert!(
            s.append_canonical(3, B256::ZERO, B256::ZERO, &[], &[])
                .is_err()
        );
    }
}

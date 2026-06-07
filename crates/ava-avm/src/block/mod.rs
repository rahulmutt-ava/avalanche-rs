// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) block model — the [`Block`] interface enum, byte-exact codec,
//! and `block_id = sha256(codec_bytes)` hashing (specs 09 §7).
//!
//! Port of `vms/avm/block` (`block.go`, `standard_block.go`, `parser.go`). The
//! single concrete block type, `StandardBlock`, is registered into the **same**
//! type-ID numbering space as the txs (specs 09 §2.1) at id 20 — see the
//! placeholder in [`crate::txs::codec::build_type_id_registry`]. Blocks are framed
//! by the shared [`Codec`](crate::txs::codec::Codec) /
//! [`GenesisCodec`](crate::txs::codec::GenesisCodec) managers (Go
//! `block.NewParser` wraps `txs.Parser`): ordinary blocks parse with the standard
//! codec, genesis blocks with the genesis codec.
//!
//! Go marshals a block as a pointer to the `Block` interface (`cm.Marshal(
//! CodecVersion, &blk)`), writing the `u32` type-ID then the concrete fields; the
//! [`BlockBody`] enum's `#[codec(type_registry)]` derive reproduces this exactly.

use bytes::Bytes;

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_types::id::Id;

use crate::txs::{CODEC_VERSION, Tx};

pub mod parser;
pub mod standard_block;

pub use parser::parse;
pub use standard_block::StandardBlock;

/// `block.Block` — the registered block interface; its concrete types become enum
/// variants with explicit `#[codec(type_id = N)]` over the shared registry
/// (specs 09 §7). The X-Chain has a single block type, `StandardBlock` at id 20.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum BlockBody {
    /// `StandardBlock` (type_id 20).
    #[codec(type_id = 20)]
    Standard(StandardBlock),
}

impl Default for BlockBody {
    fn default() -> Self {
        BlockBody::Standard(StandardBlock::default())
    }
}

impl BlockBody {
    /// The `parent_id` (`Parent()`) of the underlying block.
    #[must_use]
    fn parent_id(&self) -> Id {
        match self {
            BlockBody::Standard(b) => b.parent_id,
        }
    }

    /// The `height` (`Height()`) of the underlying block.
    #[must_use]
    fn height(&self) -> u64 {
        match self {
            BlockBody::Standard(b) => b.height,
        }
    }

    /// The block's Unix-seconds timestamp (`Timestamp()`).
    #[must_use]
    fn time(&self) -> u64 {
        match self {
            BlockBody::Standard(b) => b.time,
        }
    }

    /// The merkle root (`MerkleRoot()`); currently always the zero id.
    #[must_use]
    fn merkle_root(&self) -> Id {
        match self {
            BlockBody::Standard(b) => b.root,
        }
    }

    /// The block's transactions (`Txs()`).
    #[must_use]
    fn txs(&self) -> &[Tx] {
        match self {
            BlockBody::Standard(b) => &b.transactions,
        }
    }

    /// Mutable access to the block's transactions (used by [`Block::parse`] to
    /// re-derive each contained tx's `tx_id`).
    #[must_use]
    fn txs_mut(&mut self) -> &mut Vec<Tx> {
        match self {
            BlockBody::Standard(b) => &mut b.transactions,
        }
    }
}

/// An X-Chain block: a [`BlockBody`] (the wire-serialized variant) plus the
/// derived `block_id`/`bytes` caches that are not part of the encoding.
///
/// `block_id = sha256(codec_bytes)` (Go `StandardBlock.initialize` →
/// `hashing.ComputeHash256Array`). The raw codec bytes are held zero-copy in a
/// [`Bytes`] handle, mirroring the Go `bytes` cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    /// The wire-serialized block variant.
    body: BlockBody,
    /// `BlockID` — `sha256(bytes)`. Not serialized.
    block_id: Id,
    /// Cached codec bytes. Not serialized.
    bytes: Bytes,
}

impl Block {
    /// Wraps a [`BlockBody`] into an uninitialized [`Block`] (no derived caches).
    #[must_use]
    pub fn new(body: BlockBody) -> Self {
        Self {
            body,
            block_id: Id::EMPTY,
            bytes: Bytes::new(),
        }
    }

    /// `block.initialize` — marshals the body as the `Block` interface
    /// (type-id-prefix 20), sets the cached bytes and `block_id = sha256(bytes)`,
    /// then re-initializes every contained tx (specs 09 §7).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling fails.
    pub fn initialize(&mut self, c: &Manager) -> CodecResult<()> {
        let bytes = c.marshal(CODEC_VERSION, &self.body)?;
        self.set_bytes(Bytes::from(bytes));
        for tx in self.body.txs_mut() {
            tx.initialize(c)?;
        }
        Ok(())
    }

    /// Sets the cached bytes and recomputes `block_id = sha256(bytes)`.
    fn set_bytes(&mut self, bytes: Bytes) {
        self.block_id = Id::from(hashing::sha256(&bytes));
        self.bytes = bytes;
    }

    /// `block.parse` — decode a type-tagged block and initialize its derived
    /// caches (`block_id`, zero-copy raw bytes), re-deriving each contained tx's
    /// `tx_id` (Go `parse` → `blk.initialize` → `tx.Initialize` per tx; specs
    /// 09 §7).
    ///
    /// The caller passes the codec explicitly because genesis blocks parse with
    /// [`GenesisCodec`](crate::txs::codec::GenesisCodec) and ordinary blocks with
    /// [`Codec`](crate::txs::codec::Codec).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode or a
    /// contained tx fails to (re-)initialize.
    pub fn parse(c: &Manager, bytes: &[u8]) -> CodecResult<Self> {
        let mut body = BlockBody::default();
        c.unmarshal(bytes, &mut body)?;
        let mut blk = Block::new(body);
        // `block_id = sha256(bytes)`; hold the input bytes zero-copy.
        blk.set_bytes(Bytes::copy_from_slice(bytes));
        // Re-derive each contained tx's tx_id / cached bytes.
        for tx in blk.body.txs_mut() {
            tx.initialize(c)?;
        }
        Ok(blk)
    }

    /// The wire-serialized block body.
    #[must_use]
    pub fn body(&self) -> &BlockBody {
        &self.body
    }

    /// The block ID (`sha256(bytes)`; `Id::EMPTY` until initialized).
    #[must_use]
    pub fn id(&self) -> Id {
        self.block_id
    }

    /// The cached codec bytes (empty until [`Block::initialize`]/[`Block::parse`]).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The codec `type_id` of the underlying body (specs 09 §7; always 20).
    #[must_use]
    pub fn type_id(&self) -> u32 {
        self.body.codec_type_id()
    }

    /// `Parent()` — the parent block's ID.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.body.parent_id()
    }

    /// `Height()` — this block's height (genesis is `0`).
    #[must_use]
    pub fn height(&self) -> u64 {
        self.body.height()
    }

    /// `Timestamp()` — the block's proposed wall-clock time, in Unix seconds
    /// (Go `time.Unix(int64(b.Time), 0)`).
    #[must_use]
    pub fn timestamp(&self) -> u64 {
        self.body.time()
    }

    /// `MerkleRoot()` — the merkle root (currently always the zero id).
    #[must_use]
    pub fn merkle_root(&self) -> Id {
        self.body.merkle_root()
    }

    /// `Txs()` — the transactions contained in the block.
    #[must_use]
    pub fn txs(&self) -> &[Tx] {
        self.body.txs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txs::codec::codec;

    #[test]
    fn standard_block_type_id_is_20() {
        assert_eq!(
            BlockBody::Standard(StandardBlock::default()).codec_type_id(),
            20
        );
    }

    #[test]
    fn empty_block_roundtrip() {
        let c = codec().expect("build codec");
        let blk = StandardBlock::new_block(&c, Id::EMPTY, 0, 0, Vec::new()).expect("build block");
        assert_ne!(blk.id(), Id::EMPTY);
        assert_eq!(blk.id(), Id::from(hashing::sha256(blk.bytes())));
        assert_eq!(blk.type_id(), 20);

        let parsed = Block::parse(&c, blk.bytes()).expect("parse block");
        assert_eq!(parsed.id(), blk.id());
        assert_eq!(parsed.bytes(), blk.bytes());
        assert!(parsed.txs().is_empty());
    }
}

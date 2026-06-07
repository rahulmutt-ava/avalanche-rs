// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain block model — the [`Block`] interface enum, byte-exact codec, and
//! `block_id = sha256(codec_bytes)` hashing (specs 08 §4).
//!
//! Port of `vms/platformvm/block` (`block.go`, `parse.go`, plus the per-era
//! block bodies). The 9 concrete block types are registered into the **same**
//! type-ID numbering space as the txs (specs 08 §2.1): the 5 Apricot blocks at
//! IDs 0–4 and the 4 Banff blocks at 29–32. They are framed by the shared
//! [`txs::Codec`](crate::txs::Codec) / [`txs::GenesisCodec`](crate::txs::GenesisCodec)
//! managers (re-exported via [`codec`]).
//!
//! Go marshals a block as a pointer to the `Block` interface (`Codec.Marshal(
//! CodecVersion, &blk)`), writing the `u32` type-ID then the concrete fields;
//! the [`Block`] enum's `#[codec(type_registry)]` derive reproduces this exactly.

use bytes::Bytes;

use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_types::id::Id;

use crate::CODEC_VERSION;
use crate::block::apricot::{
    ApricotAbortBlock, ApricotAtomicBlock, ApricotCommitBlock, ApricotProposalBlock,
    ApricotStandardBlock,
};
use crate::block::banff::{
    BanffAbortBlock, BanffCommitBlock, BanffProposalBlock, BanffStandardBlock,
};
use crate::txs::Tx;

pub mod apricot;
pub mod banff;
pub mod codec;
pub mod executor;
pub mod parse;

pub use apricot::CommonBlock;
pub use parse::parse;

/// `block.Block` — the registered block interface; its concrete types become
/// enum variants with explicit `#[codec(type_id = N)]` over the shared registry
/// (specs 08 §4.1).
///
/// The `block_id` and raw `bytes` are derived caches populated by
/// [`Block::initialize`] / [`Block::parse`]; they are **not** on the wire.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum BlockBody {
    /// `ApricotProposalBlock` (type_id 0).
    #[codec(type_id = 0)]
    ApricotProposal(ApricotProposalBlock),
    /// `ApricotAbortBlock` (type_id 1).
    #[codec(type_id = 1)]
    ApricotAbort(ApricotAbortBlock),
    /// `ApricotCommitBlock` (type_id 2).
    #[codec(type_id = 2)]
    ApricotCommit(ApricotCommitBlock),
    /// `ApricotStandardBlock` (type_id 3).
    #[codec(type_id = 3)]
    ApricotStandard(ApricotStandardBlock),
    /// `ApricotAtomicBlock` (type_id 4).
    #[codec(type_id = 4)]
    ApricotAtomic(ApricotAtomicBlock),
    /// `BanffProposalBlock` (type_id 29).
    #[codec(type_id = 29)]
    BanffProposal(BanffProposalBlock),
    /// `BanffAbortBlock` (type_id 30).
    #[codec(type_id = 30)]
    BanffAbort(BanffAbortBlock),
    /// `BanffCommitBlock` (type_id 31).
    #[codec(type_id = 31)]
    BanffCommit(BanffCommitBlock),
    /// `BanffStandardBlock` (type_id 32).
    #[codec(type_id = 32)]
    BanffStandard(BanffStandardBlock),
}

impl Default for BlockBody {
    fn default() -> Self {
        BlockBody::ApricotCommit(ApricotCommitBlock::default())
    }
}

/// A P-Chain block: a [`BlockBody`] (the wire-serialized variant) plus the
/// derived `block_id`/`bytes` caches that are not part of the encoding.
///
/// `block_id = sha256(codec_bytes)` (Go `CommonBlock.initialize` →
/// `hashing.ComputeHash256Array`). The raw codec bytes are held zero-copy in a
/// [`Bytes`] handle, mirroring the Go `CommonBlock.bytes` cache.
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

    /// `block.initialize` — marshals the body as the `Block` interface, then sets
    /// the cached bytes and `block_id = sha256(bytes)` (specs 08 §4.1).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling fails.
    pub fn initialize(&mut self, c: &Manager) -> CodecResult<()> {
        let bytes = c.marshal(CODEC_VERSION, &self.body)?;
        self.set_bytes(Bytes::from(bytes));
        Ok(())
    }

    /// Sets the cached bytes and recomputes `block_id = sha256(bytes)`.
    fn set_bytes(&mut self, bytes: Bytes) {
        self.block_id = Id::from(hashing::sha256(&bytes));
        self.bytes = bytes;
    }

    /// `block.Parse` — decode a type-tagged block and initialize its derived
    /// caches (`block_id`, zero-copy raw bytes) (specs 08 §4.1).
    ///
    /// The caller passes the codec explicitly because genesis blocks may exceed
    /// the default [`Codec`](codec::Codec) max size and must be parsed with
    /// [`GenesisCodec`](codec::GenesisCodec).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode.
    pub fn parse(c: &Manager, bytes: &[u8]) -> CodecResult<Self> {
        let mut body = BlockBody::default();
        c.unmarshal(bytes, &mut body)?;
        let mut blk = Block::new(body);
        // Hold the input bytes zero-copy; `block_id = sha256(bytes)`.
        blk.set_bytes(Bytes::copy_from_slice(bytes));
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

    /// The codec `type_id` of the underlying body (specs 08 §4.1).
    #[must_use]
    pub fn type_id(&self) -> u32 {
        self.body.codec_type_id()
    }

    /// `Parent()` — the parent block's ID.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.body.common().parent_id
    }

    /// `Height()` — this block's height (genesis is `0`).
    #[must_use]
    pub fn height(&self) -> u64 {
        self.body.common().height
    }

    /// `Txs()` — the block's transactions.
    ///
    /// Mirrors the Go per-type `Txs()`: abort/commit blocks have none; standard
    /// blocks return their `transactions`; proposal/atomic blocks return their
    /// single `tx`; and a Banff proposal returns `transactions ++ [tx]`.
    #[must_use]
    pub fn txs(&self) -> Vec<&Tx> {
        self.body.txs()
    }

    /// `Timestamp()` — the block's proposed wall-clock time, if it carries one.
    ///
    /// Banff blocks embed a `Time` field; Apricot blocks do not (their timestamp
    /// is the parent's chain time, resolved by the executor). Returns `None` for
    /// Apricot blocks (the block manager falls back to the parent timestamp).
    #[must_use]
    pub fn banff_timestamp(&self) -> Option<u64> {
        match &self.body {
            BlockBody::BanffProposal(b) => Some(b.time),
            BlockBody::BanffAbort(b) => Some(b.time),
            BlockBody::BanffCommit(b) => Some(b.time),
            BlockBody::BanffStandard(b) => Some(b.time),
            _ => None,
        }
    }

    /// `true` iff this is a `*ProposalBlock` (the only Snowman oracle block;
    /// specs 08 §4.2). [`Block::options`] is only valid for these.
    #[must_use]
    pub fn is_proposal(&self) -> bool {
        matches!(
            &self.body,
            BlockBody::ApricotProposal(_) | BlockBody::BanffProposal(_)
        )
    }

    /// `block.NewApricotCommitBlock` — a fresh, initialized Apricot commit block
    /// over `(parent_id, height)`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if initialization fails.
    pub fn new_apricot_commit(c: &Manager, parent_id: Id, height: u64) -> CodecResult<Self> {
        let mut blk = Block::new(BlockBody::ApricotCommit(ApricotCommitBlock {
            common: CommonBlock { parent_id, height },
        }));
        blk.initialize(c)?;
        Ok(blk)
    }

    /// `block.NewApricotAbortBlock` — a fresh, initialized Apricot abort block
    /// over `(parent_id, height)`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if initialization fails.
    pub fn new_apricot_abort(c: &Manager, parent_id: Id, height: u64) -> CodecResult<Self> {
        let mut blk = Block::new(BlockBody::ApricotAbort(ApricotAbortBlock {
            common: CommonBlock { parent_id, height },
        }));
        blk.initialize(c)?;
        Ok(blk)
    }

    /// `block.NewBanffCommitBlock` — a fresh, initialized Banff commit block over
    /// `(time, parent_id, height)`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if initialization fails.
    pub fn new_banff_commit(
        c: &Manager,
        time: u64,
        parent_id: Id,
        height: u64,
    ) -> CodecResult<Self> {
        let mut blk = Block::new(BlockBody::BanffCommit(BanffCommitBlock {
            time,
            apricot: ApricotCommitBlock {
                common: CommonBlock { parent_id, height },
            },
        }));
        blk.initialize(c)?;
        Ok(blk)
    }

    /// `block.NewBanffAbortBlock` — a fresh, initialized Banff abort block over
    /// `(time, parent_id, height)`.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if initialization fails.
    pub fn new_banff_abort(
        c: &Manager,
        time: u64,
        parent_id: Id,
        height: u64,
    ) -> CodecResult<Self> {
        let mut blk = Block::new(BlockBody::BanffAbort(BanffAbortBlock {
            time,
            apricot: ApricotAbortBlock {
                common: CommonBlock { parent_id, height },
            },
        }));
        blk.initialize(c)?;
        Ok(blk)
    }
}

impl BlockBody {
    /// The embedded [`CommonBlock`] (`{ parent_id, height }`) of this variant.
    #[must_use]
    fn common(&self) -> &CommonBlock {
        match self {
            BlockBody::ApricotProposal(b) => &b.common,
            BlockBody::ApricotAbort(b) => &b.common,
            BlockBody::ApricotCommit(b) => &b.common,
            BlockBody::ApricotStandard(b) => &b.common,
            BlockBody::ApricotAtomic(b) => &b.common,
            BlockBody::BanffProposal(b) => &b.apricot.common,
            BlockBody::BanffAbort(b) => &b.apricot.common,
            BlockBody::BanffCommit(b) => &b.apricot.common,
            BlockBody::BanffStandard(b) => &b.apricot.common,
        }
    }

    /// The block's transactions (see [`Block::txs`]).
    #[must_use]
    fn txs(&self) -> Vec<&Tx> {
        match self {
            BlockBody::ApricotAbort(_)
            | BlockBody::ApricotCommit(_)
            | BlockBody::BanffAbort(_)
            | BlockBody::BanffCommit(_) => Vec::new(),
            BlockBody::ApricotProposal(b) => vec![&b.tx],
            BlockBody::ApricotAtomic(b) => vec![&b.tx],
            BlockBody::ApricotStandard(b) => b.transactions.iter().collect(),
            BlockBody::BanffStandard(b) => b.apricot.transactions.iter().collect(),
            BlockBody::BanffProposal(b) => {
                // Go `Txs()` returns decision txs ++ [proposal tx].
                let mut out: Vec<&Tx> = b.transactions.iter().collect();
                out.push(&b.apricot.tx);
                out
            }
        }
    }
}

#[cfg(test)]
mod golden {
    use serde::Deserialize;

    use super::*;
    use crate::txs::codec;

    #[derive(Deserialize)]
    struct BlockVector {
        type_id: u32,
        parent_hex: String,
        height: u64,
        bytes: String,
        id_hex: String,
    }

    fn load(name: &str) -> BlockVector {
        let path = format!(
            "{}/tests/vectors/platformvm/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(&path).expect("read golden vector");
        serde_json::from_str(&raw).expect("parse golden vector")
    }

    /// Asserts `Block::parse(bytes).id() == sha256(bytes) == expected_id` and a
    /// byte-exact re-encode for the Go-sourced Apricot commit/abort blocks and a
    /// Banff standard block with an EMPTY tx list (self-contained: no per-tx
    /// structs are exercised). Provenance: see each JSON `comment` field — the
    /// bytes/id were produced by the Go `vms/platformvm/block` codec (M4.5).
    #[test]
    fn pchain_block_hash() {
        let c = codec::codec().expect("build codec");

        for (name, want_variant) in [
            ("apricot_commit_block.json", "commit"),
            ("apricot_abort_block.json", "abort"),
            ("banff_standard_block.json", "standard"),
        ] {
            let v = load(name);
            let bytes = hex::decode(&v.bytes).expect("decode bytes hex");
            let want_id_arr: [u8; 32] = hex::decode(&v.id_hex)
                .expect("decode id hex")
                .try_into()
                .expect("id is 32 bytes");
            let want_id = Id::from(want_id_arr);
            let parent_arr: [u8; 32] = hex::decode(&v.parent_hex)
                .expect("decode parent hex")
                .try_into()
                .expect("parent is 32 bytes");
            let want_parent = Id::from(parent_arr);

            // id() == sha256(codec bytes) == expected id from Go.
            let blk = Block::parse(&c, &bytes).expect("parse block");
            assert_eq!(blk.id(), want_id, "{name}: block_id mismatch");
            assert_eq!(
                blk.id(),
                Id::from(hashing::sha256(&bytes)),
                "{name}: block_id != sha256(bytes)"
            );

            // Decoded common fields match.
            assert_eq!(blk.type_id(), v.type_id, "{name}: type_id mismatch");
            assert_eq!(blk.parent_id(), want_parent, "{name}: parent mismatch");
            assert_eq!(blk.height(), v.height, "{name}: height mismatch");
            assert!(blk.txs().is_empty(), "{name}: expected no txs");

            // Re-encode is byte-exact (round-trip == original bytes).
            let reenc = c.marshal(CODEC_VERSION, blk.body()).expect("re-marshal");
            assert_eq!(reenc, bytes, "{name}: re-encode != original bytes");

            // The parsed variant is the expected concrete block.
            match (want_variant, blk.body()) {
                ("commit", BlockBody::ApricotCommit(_))
                | ("abort", BlockBody::ApricotAbort(_))
                | ("standard", BlockBody::BanffStandard(_)) => {}
                other => panic!("{name}: unexpected variant {other:?}"),
            }
        }
    }

    /// The `Block` enum discriminants equal the `crate::txs::TYPE_ID_*` block
    /// constants (the shared numbering space; the constants are owned by
    /// `txs/mod.rs` this wave, asserted here without removing them).
    #[test]
    fn block_discriminants_match_txs_constants() {
        use crate::txs;
        let cases: &[(BlockBody, u32)] = &[
            (
                BlockBody::ApricotProposal(ApricotProposalBlock::default()),
                txs::TYPE_ID_APRICOT_PROPOSAL_BLOCK,
            ),
            (
                BlockBody::ApricotAbort(ApricotAbortBlock::default()),
                txs::TYPE_ID_APRICOT_ABORT_BLOCK,
            ),
            (
                BlockBody::ApricotCommit(ApricotCommitBlock::default()),
                txs::TYPE_ID_APRICOT_COMMIT_BLOCK,
            ),
            (
                BlockBody::ApricotStandard(ApricotStandardBlock::default()),
                txs::TYPE_ID_APRICOT_STANDARD_BLOCK,
            ),
            (
                BlockBody::ApricotAtomic(ApricotAtomicBlock::default()),
                txs::TYPE_ID_APRICOT_ATOMIC_BLOCK,
            ),
            (
                BlockBody::BanffProposal(BanffProposalBlock::default()),
                txs::TYPE_ID_BANFF_PROPOSAL_BLOCK,
            ),
            (
                BlockBody::BanffAbort(BanffAbortBlock::default()),
                txs::TYPE_ID_BANFF_ABORT_BLOCK,
            ),
            (
                BlockBody::BanffCommit(BanffCommitBlock::default()),
                txs::TYPE_ID_BANFF_COMMIT_BLOCK,
            ),
            (
                BlockBody::BanffStandard(BanffStandardBlock::default()),
                txs::TYPE_ID_BANFF_STANDARD_BLOCK,
            ),
        ];
        for (body, want) in cases {
            assert_eq!(body.codec_type_id(), *want, "discriminant mismatch");
        }
    }
}

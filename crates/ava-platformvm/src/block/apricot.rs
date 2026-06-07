// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Apricot-era block bodies (specs 08 §4.1).
//!
//! Port of `vms/platformvm/block/{common,proposal,abort,commit,standard,atomic}_block.go`.
//! Every Apricot block embeds a [`CommonBlock`] `{ parent_id, height }` as its
//! leading serialized fields; the proposal/atomic variants append a single
//! [`Tx`], and the standard variant appends a `Vec<Tx>` (`transactions`).

use ava_codec::packer::{LONG_LEN, Packer};
use ava_codec::{AvaCodec, Deserializable, Serializable};
use ava_types::id::{ID_LEN, Id};

use crate::txs::Tx;

/// `CommonBlock` — the fields common to every P-Chain block (`common_block.go`).
///
/// Serialized as `PrntID` (32 fixed bytes) then `Hght` (`u64`, big-endian). The
/// Go `BlockID`/`bytes` cache fields are **not** serialized; the block ID is
/// derived by hashing the full codec bytes (see [`crate::block::Block::id`]).
///
/// Hand-written codec (rather than `#[derive(AvaCodec)]`) because [`Id`] is a
/// foreign type without a blanket [`Serializable`] impl; the wire layout is
/// nonetheless byte-identical to the Go struct.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommonBlock {
    /// `PrntID` — the parent block's ID.
    pub parent_id: Id,
    /// `Hght` — this block's height (genesis is `0`).
    pub height: u64,
}

impl Serializable for CommonBlock {
    fn marshal_into(&self, p: &mut Packer) {
        p.pack_fixed_bytes(self.parent_id.as_bytes());
        p.pack_u64(self.height);
    }

    fn size(&self) -> usize {
        ID_LEN.saturating_add(LONG_LEN)
    }
}

impl Deserializable for CommonBlock {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        let raw = p.unpack_fixed_bytes(ID_LEN);
        if p.errored() {
            return;
        }
        match Id::from_slice(&raw) {
            Ok(id) => self.parent_id = id,
            Err(_) => {
                p.add_external_error(ava_codec::error::PackerError::InvalidInput);
                return;
            }
        }
        self.height = p.unpack_u64();
    }
}

/// `ApricotProposalBlock` — block `type_id` 0: a [`CommonBlock`] plus a single
/// proposal [`Tx`] (`proposal_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApricotProposalBlock {
    /// The embedded `{ parent_id, height }`.
    #[codec]
    pub common: CommonBlock,
    /// `Tx` — the proposal transaction.
    #[codec]
    pub tx: Tx,
}

/// `ApricotAbortBlock` — block `type_id` 1: a bare [`CommonBlock`]
/// (`abort_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApricotAbortBlock {
    /// The embedded `{ parent_id, height }`.
    #[codec]
    pub common: CommonBlock,
}

/// `ApricotCommitBlock` — block `type_id` 2: a bare [`CommonBlock`]
/// (`commit_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApricotCommitBlock {
    /// The embedded `{ parent_id, height }`.
    #[codec]
    pub common: CommonBlock,
}

/// `ApricotStandardBlock` — block `type_id` 3: a [`CommonBlock`] plus a
/// `Transactions` vector (`standard_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApricotStandardBlock {
    /// The embedded `{ parent_id, height }`.
    #[codec]
    pub common: CommonBlock,
    /// `Transactions` — the block's decision transactions.
    #[codec]
    pub transactions: Vec<Tx>,
}

/// `ApricotAtomicBlock` — block `type_id` 4: a [`CommonBlock`] plus a single
/// atomic [`Tx`] (`atomic_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct ApricotAtomicBlock {
    /// The embedded `{ parent_id, height }`.
    #[codec]
    pub common: CommonBlock,
    /// `Tx` — the atomic transaction.
    #[codec]
    pub tx: Tx,
}

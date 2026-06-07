// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Banff-era block bodies (specs 08 §4.1).
//!
//! Port of `vms/platformvm/block/{proposal,abort,commit,standard}_block.go`
//! Banff variants. Each Banff block is the Apricot body with a leading
//! `Time: u64` (Unix seconds). **Field order is byte-exact**: the Banff-specific
//! fields precede the embedded Apricot struct in the Go struct literal, so:
//!
//! - `BanffProposalBlock` = `time`, then `transactions` (the decision `Vec<Tx>`),
//!   then the embedded `ApricotProposalBlock` (`{ common, tx }`).
//! - `BanffAbortBlock` / `BanffCommitBlock` = `time`, then the embedded Apricot
//!   abort/commit body (a bare `CommonBlock`).
//! - `BanffStandardBlock` = `time`, then the embedded `ApricotStandardBlock`
//!   (`{ common, transactions }`).

use ava_codec::AvaCodec;

use crate::block::apricot::{
    ApricotAbortBlock, ApricotCommitBlock, ApricotProposalBlock, ApricotStandardBlock,
};
use crate::txs::Tx;

/// `BanffProposalBlock` — block `type_id` 29 (`proposal_block.go`).
///
/// Byte order: `time`, then `transactions` (decision txs), then the embedded
/// [`ApricotProposalBlock`] (which contributes `common` then the proposal `tx`).
/// Its `Txs()` returns `transactions ++ [tx]` (see [`crate::block::Block::txs`]).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BanffProposalBlock {
    /// `Time` — the block's wall-clock timestamp (Unix seconds).
    #[codec]
    pub time: u64,
    /// `Transactions` — the decision transactions carried alongside the proposal.
    #[codec]
    pub transactions: Vec<Tx>,
    /// The embedded Apricot proposal body (`{ common, tx }`).
    #[codec]
    pub apricot: ApricotProposalBlock,
}

/// `BanffAbortBlock` — block `type_id` 30 (`abort_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BanffAbortBlock {
    /// `Time` — the block's wall-clock timestamp (Unix seconds).
    #[codec]
    pub time: u64,
    /// The embedded Apricot abort body (a bare `CommonBlock`).
    #[codec]
    pub apricot: ApricotAbortBlock,
}

/// `BanffCommitBlock` — block `type_id` 31 (`commit_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BanffCommitBlock {
    /// `Time` — the block's wall-clock timestamp (Unix seconds).
    #[codec]
    pub time: u64,
    /// The embedded Apricot commit body (a bare `CommonBlock`).
    #[codec]
    pub apricot: ApricotCommitBlock,
}

/// `BanffStandardBlock` — block `type_id` 32 (`standard_block.go`).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct BanffStandardBlock {
    /// `Time` — the block's wall-clock timestamp (Unix seconds).
    #[codec]
    pub time: u64,
    /// The embedded Apricot standard body (`{ common, transactions }`).
    #[codec]
    pub apricot: ApricotStandardBlock,
}

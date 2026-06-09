// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The VM's consensus block handle ([`SaeBlock`]) — the bridge between the
//! lifecycle [`ava_saevm_blocks::Block`] and the Snowman
//! [`ava_saevm_adaptor::BlockProperties`] seam (specs/11 §4.1/§5).
//!
//! The orphan rule forbids `impl BlockProperties for Arc<Block>`
//! (`BlockProperties` is foreign and `Arc` is not a fundamental type), and
//! `BlockProperties::bytes() -> &[u8]` needs the VM's cached RLP wire bytes — so
//! the impl lives on this *local* newtype, which the VM trafficks in as its
//! `BP`. See the M7.11 as-built deferral note in `blocks/src/lifecycle.rs`.

use std::sync::Arc;
use std::time::SystemTime;

use ava_evm_reth::{B256, rlp_encode};
use ava_saevm_adaptor::BlockProperties;
use ava_saevm_blocks::Block;
use ava_types::id::Id;

/// Maps a block/transaction [`B256`] hash to a consensus [`Id`].
///
/// `Id` and `B256` are both 32 bytes; the SAE block id **is** its
/// keccak-256 hash (specs/11 §4.1), so this is a byte-for-byte reinterpretation.
#[must_use]
pub fn id_from_hash(hash: B256) -> Id {
    Id::from(hash.0)
}

/// The VM's consensus block handle: an `Arc<Block>` plus the cached RLP wire
/// bytes (so [`BlockProperties::bytes`] can hand out a `&[u8]` borrow) and the
/// precomputed [`Id`]s.
///
/// Cheaply `Clone` (the wire bytes are shared via `Arc`). Mirrors the role of Go
/// `*blocks.Block` once wrapped for the Snowman `adaptor`.
#[derive(Clone)]
pub struct SaeBlock {
    block: Arc<Block>,
    /// Cached RLP wire bytes (the `parse_block` input, or recomputed at build).
    bytes: Arc<Vec<u8>>,
    id: Id,
    parent: Id,
}

impl SaeBlock {
    /// Wraps `block`, computing its RLP wire bytes and ids.
    #[must_use]
    pub fn new(block: Arc<Block>) -> Self {
        let bytes = Arc::new(rlp_encode(block.eth_block().clone_block()));
        Self::with_bytes(block, bytes)
    }

    /// Wraps `block` reusing the already-known wire `bytes` (the `parse_block`
    /// path, avoiding a re-encode).
    #[must_use]
    pub fn with_bytes(block: Arc<Block>, bytes: Arc<Vec<u8>>) -> Self {
        let id = id_from_hash(block.hash());
        let parent = id_from_hash(block.parent_hash());
        Self {
            block,
            bytes,
            id,
            parent,
        }
    }

    /// The wrapped lifecycle block.
    #[must_use]
    pub fn block(&self) -> &Arc<Block> {
        &self.block
    }

    /// The cached RLP wire bytes (shared handle).
    #[must_use]
    pub fn wire_bytes(&self) -> &Arc<Vec<u8>> {
        &self.bytes
    }
}

impl BlockProperties for SaeBlock {
    fn id(&self) -> Id {
        self.id
    }

    fn parent(&self) -> Id {
        self.parent
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn height(&self) -> u64 {
        self.block.height()
    }

    fn timestamp(&self) -> SystemTime {
        self.block.timestamp()
    }
}

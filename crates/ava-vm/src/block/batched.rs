// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `block.BatchedChainVM` + the `GetAncestors`/`BatchedParseBlock` fallbacks
//! (specs 07 §2.5; Go `snow/engine/snowman/block/batched_vm.go`).
//!
//! The two free functions reproduce Go's byte-accounting fallback exactly: when
//! the VM does not implement the batched capability (or reports
//! `Err(Error::RemoteVmNotImplemented)`), they walk the chain one block at a time
//! while respecting `max_blocks_num` / `max_blocks_size` (each element counts
//! `bytes.len() + INT_LEN`) / `max_retrieval_time`, and special-case
//! `Err(Error::NotFound)` as an empty/break response.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::block::Block;
use crate::block::chain_vm::ChainVm;
use crate::error::{Error, Result};

/// `wrappers.IntLen` — the length, in bytes, of the `int` length prefix that
/// precedes each container element on the wire. Used by the [`get_ancestors`]
/// byte accounting so the fallback matches the batched VM's size budget exactly.
pub const INT_LEN: usize = 4;

/// `block.BatchedChainVM` — efficient bulk fetch/parse over the network.
#[async_trait]
pub trait BatchedChainVm: Send + Sync {
    /// `GetAncestors` — return the byte representations of `[blk_id]` and its
    /// ancestors, newest first, bounded by `max_blocks_num` /
    /// `max_blocks_size` / `max_retrieval_time`.
    async fn get_ancestors(
        &self,
        token: &CancellationToken,
        blk_id: Id,
        max_blocks_num: usize,
        max_blocks_size: usize,
        max_retrieval_time: Duration,
    ) -> Result<Vec<Vec<u8>>>;

    /// `BatchedParseBlock` — parse a batch of block byte representations.
    async fn batched_parse_block(
        &self,
        token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> Result<Vec<Arc<dyn Block>>>;
}

/// `block.GetAncestors` — the engine-side fallback (Go free function).
///
/// If the VM is batched, delegate to it; on `Err(Error::RemoteVmNotImplemented)`
/// fall back to the local walk. The local walk fetches `blk_id`, then walks
/// parents accumulating byte representations until any bound is hit. A
/// `NotFound` on the first block yields an empty response (signalling the peer
/// to stop asking this node); a `NotFound` (or any error) on a parent simply
/// breaks the walk.
pub async fn get_ancestors(
    vm: &dyn ChainVm,
    token: &CancellationToken,
    blk_id: Id,
    max_blocks_num: usize,
    max_blocks_size: usize,
    max_retrieval_time: Duration,
) -> Result<Vec<Vec<u8>>> {
    // Try the batched capability first.
    if let Some(batched) = vm.as_batched() {
        match batched
            .get_ancestors(
                token,
                blk_id,
                max_blocks_num,
                max_blocks_size,
                max_retrieval_time,
            )
            .await
        {
            Ok(blocks) => return Ok(blocks),
            Err(Error::RemoteVmNotImplemented) => {}
            Err(e) => return Err(e),
        }
    }

    // Local fallback.
    let start = Instant::now();
    let mut blk = match vm.get_block(token, blk_id).await {
        Ok(blk) => blk,
        // Special-case NotFound as an empty response: signals the peer to avoid
        // contacting this node for further ancestors (pruned / state-synced).
        Err(Error::NotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    // First element is the byte repr. of `blk`, then its parent, etc.
    // Go preallocates `maxBlocksNum`; we cap the capacity hint so an unbounded
    // `max_blocks_num` cannot trigger an allocator capacity overflow (the result
    // length is unchanged — the Vec grows as needed).
    let mut ancestors_bytes: Vec<Vec<u8>> = Vec::with_capacity(max_blocks_num.min(1024));
    ancestors_bytes.push(blk.bytes().to_vec());
    // Length, in bytes, of all elements of `ancestors` (incl. each IntLen prefix).
    let mut ancestors_bytes_len = blk.bytes().len().saturating_add(INT_LEN);

    let mut num_fetched = 1usize;
    while num_fetched < max_blocks_num && start.elapsed() < max_retrieval_time {
        let parent_id = blk.parent();
        blk = match vm.get_block(token, parent_id).await {
            Ok(blk) => blk,
            // After state sync we may not have the full chain — stop the walk.
            Err(Error::NotFound) => break,
            // Any other error also stops the walk (Go logs + breaks).
            Err(_) => break,
        };

        let blk_bytes = blk.bytes();
        // Include INT_LEN because the per-container length prefix is counted.
        let new_len = ancestors_bytes_len
            .saturating_add(blk_bytes.len())
            .saturating_add(INT_LEN);
        if new_len > max_blocks_size {
            // Reached the maximum response size.
            break;
        }
        ancestors_bytes.push(blk_bytes.to_vec());
        ancestors_bytes_len = new_len;
        num_fetched = num_fetched.saturating_add(1);
    }

    Ok(ancestors_bytes)
}

/// `block.BatchedParseBlock` — the engine-side fallback (Go free function).
///
/// If the VM is batched, delegate; on `Err(Error::RemoteVmNotImplemented)` fall
/// back to parsing one block at a time.
pub async fn batched_parse_block(
    vm: &dyn ChainVm,
    token: &CancellationToken,
    blks: &[Vec<u8>],
) -> Result<Vec<Arc<dyn Block>>> {
    if let Some(batched) = vm.as_batched() {
        match batched.batched_parse_block(token, blks).await {
            Ok(blocks) => return Ok(blocks),
            Err(Error::RemoteVmNotImplemented) => {}
            Err(e) => return Err(e),
        }
    }

    let mut blocks: Vec<Arc<dyn Block>> = Vec::with_capacity(blks.len());
    for block_bytes in blks {
        blocks.push(vm.parse_block(token, block_bytes).await?);
    }
    Ok(blocks)
}

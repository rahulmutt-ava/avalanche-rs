// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The height-ordered bootstrap executor (port of
//! `snow/engine/snowman/bootstrap/{storage.go::execute,acceptor.go}`, specs 06
//! §4.3/§4.4).
//!
//! Once the interval tree holds a continuous range above the last-accepted
//! height, [`execute`] replays the fetched blocks in **ascending height order**:
//! it parses each block via the VM, `verify`s it (when above the last-accepted
//! height), and accepts it. Accept fires the `ConsensusContext.block_acceptor`
//! **before** the block's own VM `accept` (the §2.4 ordering invariant), so
//! indexers / atomic memory see the block before the VM commits state.
//!
//! `ConsensusContext.executing` is set for the duration of the replay (Go sets it
//! while replaying txs during bootstrap), and the halt token aborts the replay
//! promptly between blocks.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;

use ava_snow::ConsensusContext;
use ava_vm::block::ChainVm;

use crate::error::{Error, Result};
use crate::snowman::bootstrap::interval::{Blocks, Tree};

/// Executes every block tracked by `tree` in ascending height order, verifying +
/// accepting those above `last_accepted_height` and removing each from the tree.
///
/// Blocks at or below `last_accepted_height` are removed without execution (they
/// are already decided). Sets [`ConsensusContext::executing`] for the duration.
///
/// # Errors
/// Returns [`Error::Halted`] if `token` fires mid-replay; propagates a fatal
/// VM/acceptor verify/accept error.
pub async fn execute<V: ChainVm>(
    token: &CancellationToken,
    ctx: &ConsensusContext,
    vm: &Arc<tokio::sync::Mutex<V>>,
    tree: &mut Tree,
    blocks: &mut Blocks,
    last_accepted_height: u64,
) -> Result<()> {
    ctx.executing.store(true, Ordering::SeqCst);
    let result = execute_inner(token, ctx, vm, tree, blocks, last_accepted_height).await;
    ctx.executing.store(false, Ordering::SeqCst);
    result
}

async fn execute_inner<V: ChainVm>(
    token: &CancellationToken,
    ctx: &ConsensusContext,
    vm: &Arc<tokio::sync::Mutex<V>>,
    tree: &mut Tree,
    blocks: &mut Blocks,
    last_accepted_height: u64,
) -> Result<()> {
    // Iterate ascending by height (the tree/blocks are height-keyed).
    for height in blocks.heights() {
        if token.is_cancelled() {
            return Err(Error::Halted);
        }

        let Some(blk_bytes) = blocks.remove(height) else {
            continue;
        };
        tree.remove(height);

        if height <= last_accepted_height {
            // Already decided; removed from the tree, not executed.
            continue;
        }

        let blk = {
            let vm = vm.lock().await;
            vm.parse_block(token, &blk_bytes).await?
        };

        blk.verify(token).await?;

        // Ordering invariant: the consensus acceptor fires before the VM accept.
        ctx.block_acceptor
            .accept(ctx, blk.id(), blk.bytes())
            .await?;
        blk.accept(token).await?;
    }
    Ok(())
}

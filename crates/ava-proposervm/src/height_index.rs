// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `height -> proposervm blockID` index logic (Go
//! `vms/proposervm/height_indexed_vm.go`).
//!
//! The proposervm maintains a height index so the engine can serve
//! `GetAncestors`/state-sync by height. Heights *below* the fork height are
//! served by the inner VM; heights at or above the fork height are served from
//! the proposervm's own index. `update_height_index` records a newly-accepted
//! post-fork block, lazily storing the fork height on the first post-fork block.

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::State;

/// The resolution of a height lookup: either the proposervm index answers it, or
/// the caller must fall through to the inner VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightLookup {
    /// The proposervm index resolved the height to this block id.
    Proposer(Id),
    /// The height precedes the fork; the inner VM must answer it.
    InnerVm,
}

/// `GetBlockIDAtHeight` (Go `height_indexed_vm.go`): resolve `height` against
/// the proposervm index, or signal that the inner VM owns it.
///
/// - If the fork height is known and `height < fork_height`: [`HeightLookup::InnerVm`].
/// - If the fork height is known and `height >= fork_height`: read the index.
/// - If the fork height is unknown (fork not reached): [`HeightLookup::InnerVm`].
///
/// # Errors
/// Propagates a database error other than `NotFound` on the fork-height read, or
/// [`Error::NotFound`] if the index has no entry at `height` (post-fork).
pub fn get_block_id_at_height(state: &State, height: u64) -> Result<HeightLookup> {
    match state.get_fork_height() {
        Ok(fork_height) => {
            if height < fork_height {
                Ok(HeightLookup::InnerVm)
            } else {
                Ok(HeightLookup::Proposer(
                    state.get_block_id_at_height(height)?,
                ))
            }
        }
        Err(Error::NotFound) => Ok(HeightLookup::InnerVm),
        Err(e) => Err(e),
    }
}

/// `updateHeightIndex` (Go `height_indexed_vm.go`): record a newly-accepted
/// post-fork block at `height`. The first post-fork block lazily stores the fork
/// height. (Historical-block pruning — `NumHistoricalBlocks` — is not yet ported;
/// see `tests/PORTING.md`.)
///
/// # Errors
/// Propagates the underlying database error.
pub fn update_height_index(state: &State, height: u64, blk_id: Id) -> Result<()> {
    match state.get_fork_height() {
        Ok(_) => {}
        Err(Error::NotFound) => {
            // First post-fork block: store the fork height.
            state.set_fork_height(height)?;
        }
        Err(e) => return Err(e),
    }
    state.set_block_id_at_height(height, blk_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ava_database::{DynDatabase, MemDb};

    use super::*;

    #[test]
    fn fork_height_lazy_set_and_lookup_split() {
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        let state = State::new(db);

        // Before any post-fork block, all heights fall through to the inner VM.
        assert_eq!(
            get_block_id_at_height(&state, 5).expect("lookup"),
            HeightLookup::InnerVm
        );

        // The first post-fork block sets the fork height to its own height.
        let id5 = Id::from([5u8; 32]);
        update_height_index(&state, 5, id5).expect("update");
        assert_eq!(state.get_fork_height().expect("fork"), 5);

        // Below the fork: inner VM. At/above: the index.
        assert_eq!(
            get_block_id_at_height(&state, 4).expect("lookup"),
            HeightLookup::InnerVm
        );
        assert_eq!(
            get_block_id_at_height(&state, 5).expect("lookup"),
            HeightLookup::Proposer(id5)
        );

        // A second post-fork block does not move the fork height.
        let id6 = Id::from([6u8; 32]);
        update_height_index(&state, 6, id6).expect("update");
        assert_eq!(state.get_fork_height().expect("fork"), 5);
        assert_eq!(
            get_block_id_at_height(&state, 6).expect("lookup"),
            HeightLookup::Proposer(id6)
        );
    }
}

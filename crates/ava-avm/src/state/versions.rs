// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `state.Versions` resolver (`vms/avm/state/versions.go`, specs 09 §5.2).
//!
//! Resolves a block id to the [`Chain`] view of the chain *after* that block has
//! been accepted. Used by [`Diff::new`](super::diff::Diff::new) to find its
//! parent state.

use std::sync::Arc;

use ava_types::id::Id;

use crate::state::chain::Chain;

/// `state.Versions` — resolves a block id to the `Chain` after it was accepted.
///
/// If the state is not known, `None` is returned (Go returns `(nil, false)`).
pub trait Versions {
    /// `GetState` — the [`Chain`] view after `blk_id` has been accepted.
    fn get_state(&self, blk_id: Id) -> Option<Arc<dyn Chain>>;
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.InitialState` — a `CreateAssetTx`'s per-fx initial UTXO outputs
//! (specs 09 §3.3).

use std::cmp::Ordering;

use ava_codec::AvaCodec;
use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_types::id::Id;

use crate::txs::components::Output;

/// `avm/txs/initial_state.go` — the initial outputs an asset starts with, scoped
/// to one fx.
///
/// Wire layout (codec v0): `fx_index u32 | outs []Output`. The `fx_id` is
/// **derived** (`serialize:"false"`) — filled in post-parse from the fx routing
/// table — so it carries no `#[codec]` tag.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct InitialState {
    /// The fx ordinal these outputs belong to (`< num_fxs`; specs 09 §2.2).
    #[codec]
    pub fx_index: u32,
    /// The fx's initial outputs (typeid-prefixed; sorted by marshaled bytes).
    #[codec]
    pub outs: Vec<Output>,
    /// `FxID` — runtime-only (`serialize:"false"`); never encoded.
    pub fx_id: Id,
}

impl InitialState {
    /// Builds an [`InitialState`] for an fx ordinal with its outputs.
    #[must_use]
    pub fn new(fx_index: u32, outs: Vec<Output>) -> Self {
        Self {
            fx_index,
            outs,
            fx_id: Id::EMPTY,
        }
    }

    /// `Compare(other)` — `InitialState`s order by `fx_index` (`Compare =
    /// cmp(fx_index)`).
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        self.fx_index.cmp(&other.fx_index)
    }

    /// `Sort()` — sort `outs` by their marshaled bytes (canonical order).
    pub fn sort(&mut self) {
        self.outs.sort_by_key(out_bytes);
    }
}

/// The canonical codec bytes of an fx output (incl. its typeID).
fn out_bytes(out: &Output) -> Vec<u8> {
    let mut p = Packer::with_max_size(usize::MAX);
    out.marshal_into(&mut p);
    p.into_bytes()
}

/// `IsSortedAndUniqueInitialStates` — true iff `states` are strictly increasing
/// by `fx_index`.
#[must_use]
pub fn is_sorted_and_unique_initial_states(states: &[InitialState]) -> bool {
    states.windows(2).all(|w| match w {
        [a, b] => a.compare(b) == Ordering::Less,
        _ => true,
    })
}

/// `SortInitialStates` — sort by `fx_index`.
pub fn sort_initial_states(states: &mut [InitialState]) {
    states.sort_by(InitialState::compare);
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/components/chain` — the block state/cache decorator (specs 07 §3.3).

pub mod state;

pub use state::{BlockWrapper, ChainState, ChainStateConfig};

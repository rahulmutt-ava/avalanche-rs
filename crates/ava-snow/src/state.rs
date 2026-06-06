// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Engine phase + engine selector (specs 06 §3.1; Go `snow/state.go`).

/// The current phase of a consensus engine.
///
/// Mirrors Go `snow.State`. Stored behind an `ArcSwap` on
/// [`crate::context::ConsensusContext`] so concurrent readers observe the phase
/// without locking.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum EngineState {
    /// The engine has not yet started (Go `Initializing`).
    Initializing,
    /// The engine is syncing state from peers (Go `StateSyncing`).
    StateSyncing,
    /// The engine is bootstrapping by fetching and replaying history
    /// (Go `Bootstrapping`).
    Bootstrapping,
    /// The engine is in steady-state operation (Go `NormalOp`).
    NormalOp,
}

/// Selects which engine handles a message for a chain.
///
/// Mirrors Go `p2p.EngineType` (Avalanche DAG vs. Snowman linear chain).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum EngineType {
    /// The Avalanche (DAG) engine.
    Avalanche,
    /// The Snowman (linear-chain) engine.
    Snowman,
}

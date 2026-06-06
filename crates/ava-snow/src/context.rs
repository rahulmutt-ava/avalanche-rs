// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Consensus context threaded into VMs and engines (specs 06 §3).
//!
//! In Rust we replace Go's single `*snow.Context` value bag with an
//! `Arc<ChainContext>` of immutable identity/handles plus explicitly-passed
//! dynamic state (specs 00 §6/§7.4: "never smuggle values through a context
//! bag", "no `Lock` field"). Go's deprecated `Context.Lock` is dropped;
//! concurrency is structured per-actor.
//!
//! ## Scope note (M3.2 scaffolding)
//!
//! The owning spec (06 §3) lists several shared-handle fields whose backing
//! traits live in crates not yet built at M3 (`Logger`/`MultiGatherer` from
//! `ava-network`, `SharedMemory`/`AliaserReader` from `ava-vm`/`ava-chains`,
//! `warp::Signer`, `ValidatorState` from `ava-validators`). To keep `ava-snow`
//! self-contained and free of forward/circular dependencies, those handles are
//! intentionally **deferred**: this scaffolding carries the immutable identity
//! fields plus the `chain_data_dir`, and the handles are added in their owning
//! milestones. `ConsensusContext` already owns the acceptor callbacks and the
//! dynamic engine phase, which is all the consensus core (`Topological`, M3.5)
//! needs.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;

use ava_crypto::bls;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::upgrade::UpgradeConfig;

use crate::acceptor::Acceptor;
use crate::state::EngineState;

use std::sync::Arc;

/// Immutable per-chain identity + shared handles. Cheaply cloneable via `Arc`.
///
/// Replaces Go `snow.Context`. Threaded into the VM at `initialize` (specs 07).
/// All fields are immutable identity/configuration; dynamic engine state lives
/// on [`ConsensusContext`].
pub struct ChainContext {
    /// The numeric network identifier (mainnet/fuji/local).
    pub network_id: u32,
    /// The subnet this chain belongs to.
    pub subnet_id: Id,
    /// This chain's identifier.
    pub chain_id: Id,
    /// This node's identifier.
    pub node_id: NodeId,
    /// This node's BLS public key (warp/uptime); `None` if not a staker.
    pub public_key: Option<bls::PublicKey>,
    /// The network upgrade (fork) schedule.
    pub network_upgrades: UpgradeConfig,

    /// The X-Chain (AVM) blockchain id.
    pub x_chain_id: Id,
    /// The C-Chain (EVM) blockchain id.
    pub c_chain_id: Id,
    /// The native AVAX asset id.
    pub avax_asset_id: Id,

    /// Chain-specific scratch directory.
    pub chain_data_dir: PathBuf,
}

/// Adds consensus-runtime handles & dynamic state. Owned by the engine/handler.
///
/// The acceptor callbacks fire on accept (specs 06 §3.1); the dynamic phase
/// (`state`/`executing`/`state_syncing`) is read concurrently by the engine and
/// VMs via atomics, never through a mutable shared bag.
pub struct ConsensusContext {
    /// The immutable per-chain identity/handles.
    pub chain: Arc<ChainContext>,
    /// The chain's human-readable primary alias.
    pub primary_alias: String,
    /// Fired when a block is accepted (before the VM block `accept`).
    pub block_acceptor: Arc<dyn Acceptor>,
    /// Fired when a transaction is accepted.
    pub tx_acceptor: Arc<dyn Acceptor>,
    /// The current engine phase, swapped atomically between phases.
    pub state: ArcSwap<EngineState>,
    /// Whether the engine is replaying txs during bootstrap.
    pub executing: AtomicBool,
    /// Whether the engine is performing state sync.
    pub state_syncing: AtomicBool,
}

impl ConsensusContext {
    /// Builds a `ConsensusContext` in the `Initializing` phase.
    #[must_use]
    pub fn new(
        chain: Arc<ChainContext>,
        primary_alias: String,
        block_acceptor: Arc<dyn Acceptor>,
        tx_acceptor: Arc<dyn Acceptor>,
    ) -> Self {
        Self {
            chain,
            primary_alias,
            block_acceptor,
            tx_acceptor,
            state: ArcSwap::from_pointee(EngineState::Initializing),
            executing: AtomicBool::new(false),
            state_syncing: AtomicBool::new(false),
        }
    }
}

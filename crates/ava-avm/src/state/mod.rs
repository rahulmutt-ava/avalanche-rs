// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) on-disk state (`vms/avm/state`, specs 09 §5).
//!
//! This wave (M5.10) lands the storage layer + the in-memory [`Diff`] overlay:
//!
//! - [`ReadOnlyChain`] / [`Chain`] / [`State`] — the trait surface mirroring
//!   `state.go`'s `ReadOnlyChain`/`Chain`/`State` (§5).
//! - [`State`] — the persisted base: a [`VersionDb`](ava_database::VersionDb)
//!   over the chain DB with five [`PrefixDb`](ava_database::PrefixDb) sub-stores
//!   (`utxo`/`tx`/`blockID`/`block`/`singleton`) (§5).
//! - [`Diff`] — the layered, in-memory overlay over a parent [`Chain`] (`diff.go`),
//!   flushed in deterministic `BTreeMap` order (00 §6.1).
//! - [`Versions`] — the block-id → `Chain` resolver (`versions.go`).
//!
//! UTXOs are stored as their opaque codec bytes ([`UtxoBytes`]) — the
//! protocol-relevant value layout (mirroring the P-Chain M4.13 as-built; the
//! typed `avax::Utxo` round-trip is layered on later). Txs are stored as their
//! cached signed bytes; the genesis codec (`txs::codec::GenesisCodec`, §5.3)
//! parses them back. UTXO ids are `UtxoId::input_id` (§5.1).
//!
//! `initialize_chain_state` / genesis block seeding (M5.11) and block
//! accept/reject (M5.16) are **not** implemented here — this is the storage
//! layer + Diff only. The block store stays byte/id-level (no `StandardBlock`
//! type, which is M5.15).

pub mod chain;
pub mod diff;
// The persisted base lives in `state.rs` (the plan-mandated filename), which
// trips `clippy::module_inception` against the parent `state` module.
#[allow(clippy::module_inception)]
pub mod state;
pub mod versions;

pub use chain::{Chain, ReadOnlyChain, UtxoBytes};
pub use diff::Diff;
pub use state::State;
pub use versions::Versions;

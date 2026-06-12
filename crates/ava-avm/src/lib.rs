// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-avm` — the X-Chain (AVM), port of Go `vms/avm` (+ `nftfx`, `propertyfx`).
//!
//! Tier T4 (VMs). Owning spec: `specs/09-avm-xchain.md` (PRIMARY), plus
//! `07` (ATOMIC-1 UTXO byte contract, fx framework, `avax`/`verify`/`secp256k1fx`
//! components, mempool, `SharedMemory`).
//!
//! The X-Chain is the asset-exchange chain: it owns a UTXO set over arbitrary
//! assets (defined by `CreateAssetTx`), three feature extensions (secp256k1fx,
//! nftfx, propertyfx) dispatched by a `TypeToFxIndex` table, and X↔P / X↔C
//! atomic import/export over shared memory. Post-linearization it is an ordinary
//! Snowman VM producing `StandardBlock`s.
//!
//! Module layout mirrors the Go subpackages (`txs/`, `nftfx/`, `propertyfx/`,
//! `state/`, `block/`, …); it is populated tier-by-tier across the M5 wave plan
//! (see `plan/M5-xchain.md`).

#![forbid(unsafe_code)]

// `ava_codec_derive` is consumed only transitively (the `#[derive(AvaCodec)]`
// macro is re-exported by `ava_codec`), so the direct dependency still reads as
// unused to `unused_crate_dependencies` (warn) — keep its silencer.
// (M5.2 drops the ava-crypto / ava-types / bytes silencers: the `txs` module now
// consumes them directly.)
use ava_codec_derive as _;

// Dev-dependencies not yet exercised by this crate's in-crate tests; each is
// wired in by a later M5 task (proptest/rstest fx & roundtrip tests, hex golden
// vectors). Silence `unused_crate_dependencies` for the lib-test unit.
#[cfg(test)]
use assert_matches as _;
#[cfg(test)]
use pretty_assertions as _;
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use rstest as _;
// M5.20 ATOMIC-1: exercised only by the `atomic_xp` integration test (the REAL
// `ava-chains` shared-memory backend + the P-Chain `Utxo` codec for cross-decode);
// silence `unused_crate_dependencies` for the lib-test unit.
#[cfg(test)]
use ava_chains as _;
#[cfg(test)]
use ava_platformvm as _;
#[cfg(test)]
use serde_json as _;

pub mod block;
pub mod config;
pub mod error;
pub mod factory;
pub mod fx;
pub mod fx_index;
pub mod jsonrpc;
pub mod mempool;
pub mod network;
pub mod nftfx;
pub mod propertyfx;
pub mod service;
pub mod state;
pub mod txs;
pub mod vm;

pub use config::Config;
pub use error::{Error, Result};
pub use factory::AvmFactory;
pub use fx_index::FxIndex;
pub use txs::{
    BaseTx, CreateAssetTx, Credential, ExportTx, FxCredential, FxOperation, ImportTx, InitialState,
    Operation, OperationTx, Tx, UnsignedTx,
};
pub use vm::AvmVm;

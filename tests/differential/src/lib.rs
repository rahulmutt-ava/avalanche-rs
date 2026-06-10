// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Differential test harness — the central cross-implementation oracle.
//!
//! Drives the Rust node (and, from M2, a live Go node) through the same
//! seed-derived program of [`Action`]s and compares a normalized
//! [`Observation`] at each finalization point. Two modes (specs/02 §11.1):
//!
//! * **recorded-oracle** (M0): replay an `arb_program()` against the Rust impl
//!   and compare each observation to a Go-recorded golden (reexecute path).
//! * **live two-binary** (M2+): boot a Go network and a Rust network via tmpnet
//!   with identical genesis/config/seed and assert observation equality; plus a
//!   mixed Go+Rust net reaching the same height with no fork.
//!
//! SCAFFOLD (tier-X task X.13): the [`Action`]/`arb_program`/observation surface
//! is sketched so each subsequent subsystem (M2 peer/handshake, M3 vm-rpc, M4
//! P-Chain, M5 X-Chain, M6 EVM state roots, M7 SAE, M8 validator/API views) can
//! plug in its `Observation` collector. The `LockstepDriver`, recorded-oracle
//! replay, seed repro, and tmpnet wiring are filled in by X.13/X.14/X.15.

#![forbid(unsafe_code)]

pub mod atomic;
pub mod driver;
pub mod network;
pub mod observation;
pub mod program;
pub mod saevm;
pub mod xchain;

pub use atomic::exported_utxo_observation;
pub use driver::LockstepDriver;
pub use network::{Binary, NetworkConfig};
pub use observation::Observation;
pub use program::{Action, Program};
pub use saevm::{FrontierObservation, VectorError, replay_recovery_vector};
pub use xchain::run_program;

// The networking deps are consumed only by the `interop_handshake` integration
// target (M2.22), and `proptest` only by the `xchain_issue_tx` target (M5.22),
// but the crate's lib-test build links every dev-dependency, so
// `unused_crate_dependencies` would flag them here. Reference them in a test-only
// block to satisfy the lint (the established idiom across the workspace).
//
// (`ava-avm` / `ava-vm` / `ava-secp256k1fx` / `ava-snow` / `ava-database` /
// `ava-types` / `ava-version` / `async-trait` / `serde_json` / `tokio` /
// `tokio-util` are genuine lib deps used by `xchain`, so they are NOT listed.)
#[cfg(test)]
mod dev_dep_uses {
    use ava_crypto as _;
    use ava_message as _;
    use ava_network as _;
    // `pretty_assertions` + `proptest` are consumed by the integration-test
    // targets (`sae_recovery`, `xchain_issue_tx`); reference them here so the
    // lib-test build does not trip `unused_crate_dependencies`.
    use pretty_assertions as _;
    use proptest as _;
}

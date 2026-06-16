// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-load` — the sustained-load test harness (specs/02 §10.3; specs/16 §5
//! perf; specs/00 §7.3 metric-name parity; M9.18).
//!
//! Issues a sustained, mixed C-Chain + X/P transaction stream at a target rate
//! against a tmpnet Rust network for `--load-timeout`, scrapes Prometheus
//! (parity metric names), and asserts throughput/latency SLOs hold with **zero**
//! errors. The crate splits, like every M9 task, into:
//!
//! * **Offline arms** (run every CI run, no feature flag) — the pure-Rust core:
//!   * [`generator`] — a deterministic, seed-derived [`generator::LoadGenerator`]
//!     stream + integer [`generator::PacingSchedule`] rate math (no floats, no
//!     panics, no RNG crate).
//!   * [`metrics`] — a Prometheus text-format [`metrics::Exposition`] parser
//!     (asserts parity metric names from specs/00 §7.3 / specs/18) + the pure
//!     [`metrics::slo_holds`] threshold verdict.
//! * **Gated live arm** (`#[cfg(feature = "live")]` + `#[ignore]`) — boots one
//!   `avalanchers` node via [`network::LoadNode`], runs the generator for the
//!   load timeout, scrapes `/ext/metrics`, and checks the SLOs. Needs a built
//!   `avalanchers` binary; never runs in CI / this sandbox. See
//!   `tests/sustained_load.rs`.

#![forbid(unsafe_code)]

pub mod generator;
pub mod metrics;
pub mod network;

pub use generator::{LoadGenerator, PacingSchedule, TxDescriptor, TxKind};
pub use metrics::{Exposition, Sample, SloMeasurement, SloThresholds, slo_holds, slo_violations};
pub use network::{LiveError, LoadNode};

/// The parity-critical Prometheus metric families a live load run scrapes and
/// asserts present, under the same `avalanche_<subsystem>_<name>` names Go uses
/// (specs/00 §7.3 naming-parity rule; specs/18 §2 catalog). A representative
/// subset spanning the node-level and per-chain namespaces the load stream
/// exercises — network I/O, the message handler, snowman consensus, the API
/// server, and the per-VM families.
pub const REQUIRED_PARITY_METRICS: &[&str] = &[
    // network (specs/18 §2.1) — peers + completed-handshake + send-fail counters.
    "avalanche_network_peers",
    "avalanche_network_times_connected",
    // per-peer message I/O (§2.2) — the on-the-wire message counters the stream
    // drives.
    "avalanche_network_msgs",
    "avalanche_network_msgs_bytes",
    // message handler (§2.4) — messages handled + queue depth per chain.
    "avalanche_handler_messages",
    "avalanche_handler_count",
    // snowman consensus (§2.8) — issuance→acceptance timing the latency SLO reads.
    "avalanche_snowman_blks_accepted_count",
    "avalanche_snowman_blks_accepted_sum",
    // API server (§2.12).
    "avalanche_api_calls",
    // per-VM (§2.11) — the C/X/P VMs the stream targets.
    "avalanche_evm_blocks",
    "avalanche_avm_txs_accepted",
    "avalanche_platformvm_txs_accepted",
];

// `pretty_assertions` is consumed only by the integration-test targets; the
// crate's lib-test build links every dev-dependency, so `unused_crate_dependencies`
// would flag it here. Reference it in a test-only block to satisfy the lint (the
// established workspace idiom; see `tests/differential/src/lib.rs`).
#[cfg(test)]
mod dev_dep_uses {
    use pretty_assertions as _;
}

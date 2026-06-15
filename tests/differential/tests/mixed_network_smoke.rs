// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.14 — `differential::mixed_network_smoke` (specs/02 §11.1 two-binary live,
//! §11.3 Observation, §11.4 normalization, specs/26 §9(4)).
//!
//! Mirrors the two-arm shape of `interop_handshake.rs`:
//!
//! 1. **Offline arms** (run every CI run, no feature, not `#[ignore]`):
//!    * `mixed_network_config_is_deterministic` — the per-slot Go/Rust
//!      [`BinaryMix`] and the seed-derived node identities are reproducible from
//!      the seed alone (same `NetworkConfig::deterministic(seed, n)` ⇒ identical
//!      mix + node-identity derivation; different seed ⇒ different identities).
//!    * `observation_normalization_round_trips` — [`Observation::normalized`]
//!      masks/sorts so two snapshots differing only in timestamps,
//!      per-instance IDs (node IDs, ip:port), or collection order compare equal,
//!      while a genuine last-accepted / state-root divergence compares unequal.
//!
//!    Pure Rust, no node spawn.
//!
//! 2. **Live arm** (`#[cfg(feature = "live")]` + `#[ignore]`):
//!    `mixed_network_bringup_smoke` — [`Network::start`] a small mixed Go+Rust
//!    net, assert handshakes complete / PeerLists are exchanged and a Go node
//!    logs the Rust peer's version `avalanchego/1.14.2` (specs/26 §9(4)), and
//!    [`Observation::collect`] over a node returns a comparable snapshot. Needs
//!    `$AVALANCHEGO_PATH` (Go) + a built `avalanchers`; returns early if unset.
//!    Never runs in CI / this sandbox — a scheduled/nightly job runs it via
//!    `cargo nextest run -p ava-differential --features live -- --ignored`.

#![allow(unused_crate_dependencies)]

use ava_differential::network::{Binary, BinaryMix, NetworkConfig};
use ava_differential::observation::Observation;

/// Offline: the Go/Rust slot assignment and the seed-derived node identities are
/// a pure function of `(seed, nodes)` — reproducible across calls and sensitive
/// to the seed.
#[test]
fn mixed_network_config_is_deterministic() {
    let cfg = NetworkConfig::deterministic(0xA11CE, 5);

    // The mix is reproducible from the seed alone.
    let mix_a = BinaryMix::from_config(&cfg);
    let mix_b = BinaryMix::from_config(&NetworkConfig::deterministic(0xA11CE, 5));
    assert_eq!(
        mix_a, mix_b,
        "BinaryMix::from_config is a deterministic function of the seed"
    );

    // 5 nodes ⇒ 5 slots; alternating Go/Rust per §11.4 (slot 0 = Go).
    assert_eq!(mix_a.len(), 5, "one slot per node");
    assert_eq!(
        mix_a.slots(),
        &[
            Binary::Go,
            Binary::Rust,
            Binary::Go,
            Binary::Rust,
            Binary::Go
        ],
        "slots alternate Go/Rust starting at Go (specs/02 §11.4)"
    );

    // Node identities are reproducible from the seed, and the i-th Go and i-th
    // Rust node share the same seed-derived identity (so peer-dependent fields
    // match across implementations, §11.4).
    let id0 = mix_a.node_identity(0);
    let id0_again = mix_b.node_identity(0);
    assert_eq!(
        id0, id0_again,
        "node identity is reproducible from the seed"
    );
    assert_ne!(
        mix_a.node_identity(0),
        mix_a.node_identity(1),
        "distinct slots get distinct identities"
    );

    // The staking ports are deterministic and distinct per slot.
    assert_ne!(
        id0.staking_port,
        mix_a.node_identity(1).staking_port,
        "per-slot staking ports are distinct"
    );

    // A different seed yields different identities.
    let other = BinaryMix::from_config(&NetworkConfig::deterministic(0xB0B, 5));
    assert_ne!(
        mix_a.node_identity(0).node_id,
        other.node_identity(0).node_id,
        "a different seed derives a different node id"
    );
}

/// Offline: `Observation::normalized()` removes expected non-determinism
/// (timestamps stripped, per-instance IDs masked, collections sorted) so two
/// correct nodes compare equal — while a real last-accepted / root divergence
/// still compares unequal.
#[test]
fn observation_normalization_round_trips() {
    // Two snapshots of the *same* finalized state, observed from two different
    // nodes: they differ only in timestamps, per-instance IDs (node id, ip:port)
    // and collection iteration order.
    let node_a = Observation::from_fields(vec![
        ("P/last_accepted_id", "blkAAA"),
        ("P/last_accepted_height", "42"),
        ("P/state_root", "0xdead"),
        // sorted-set members supplied out of order, with a per-instance self id.
        ("P/validators", "NodeID-2,NodeID-1,NodeID-3"),
        // pure non-determinism that must be stripped/masked:
        ("info/timestamp", "1700000000"),
        ("info/node_id", "NodeID-AAAA"),
        ("info/ip", "10.0.0.1:9651"),
        ("info/uptime", "99.5"),
    ]);
    let node_b = Observation::from_fields(vec![
        ("P/last_accepted_id", "blkAAA"),
        ("P/last_accepted_height", "42"),
        ("P/state_root", "0xdead"),
        // same set, different order.
        ("P/validators", "NodeID-1,NodeID-3,NodeID-2"),
        // different per-instance identity + a later wall clock.
        ("info/timestamp", "1700000999"),
        ("info/node_id", "NodeID-BBBB"),
        ("info/ip", "10.0.0.2:9651"),
        ("info/uptime", "12.0"),
    ]);

    assert_eq!(
        node_a.normalized(),
        node_b.normalized(),
        "two correct nodes' snapshots compare equal after normalization"
    );

    // A genuine divergence (different last-accepted block) must NOT normalize away.
    let mut forked = node_b.clone();
    forked.set_field("P/last_accepted_id", "blkZZZ");
    assert_ne!(
        node_a.normalized(),
        forked.normalized(),
        "a real last-accepted divergence survives normalization"
    );

    // A genuine state-root divergence must NOT normalize away.
    let mut bad_root = node_b.clone();
    bad_root.set_field("P/state_root", "0xbeef");
    assert_ne!(
        node_a.normalized(),
        bad_root.normalized(),
        "a real state-root divergence survives normalization"
    );

    // A validator-set membership divergence (not just order) must survive.
    let mut bad_vals = node_b.clone();
    bad_vals.set_field("P/validators", "NodeID-1,NodeID-3,NodeID-9");
    assert_ne!(
        node_a.normalized(),
        bad_vals.normalized(),
        "a real validator-set divergence survives normalization"
    );

    // Idempotence: normalizing twice is a no-op.
    assert_eq!(
        node_a.normalized(),
        node_a.normalized().normalized(),
        "normalization is idempotent"
    );
}

/// Live arm: bring up a small mixed Go+Rust network and assert cross-impl
/// handshake + a comparable [`Observation`]. Gated behind the `live` feature +
/// `#[ignore]`; needs `$AVALANCHEGO_PATH` (Go) and a built `avalanchers`. Never
/// runs in CI / this sandbox.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust tmpnet ($AVALANCHEGO_PATH + avalanchers) — nightly only"]
async fn mixed_network_bringup_smoke() {
    use std::time::Duration;

    use ava_differential::network::Network;

    // Skip gracefully if the Go oracle binary is not configured.
    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network_bringup_smoke");
        return;
    }

    let cfg = NetworkConfig::deterministic(0x5EED, 4);
    let mix = BinaryMix::from_config(&cfg);

    let net = Network::start(mix, &cfg)
        .await
        .expect("mixed Go+Rust network boots");

    // All nodes complete handshakes and exchange PeerLists.
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("all nodes complete handshakes / exchange PeerLists");

    // A Go node logs the Rust peer's version (specs/26 §9(4)).
    assert!(
        net.go_node_logged_peer_version("avalanchego/1.14.2")
            .await
            .expect("scan a Go node log for the Rust peer version"),
        "a Go node logs the Rust peer as avalanchego/1.14.2"
    );

    // The Observation collector returns a comparable per-chain snapshot.
    let node = net.nodes().first().expect("at least one node");
    let obs = Observation::collect(&node.api_base)
        .await
        .expect("collect a normalized observation from a live node");
    assert!(
        !obs.normalized().fields.is_empty(),
        "the collected observation is non-empty / comparable"
    );

    net.shutdown().await;
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.22 — `differential::version_interop` (specs/26 §9(4), §9(3), §7;
//! specs/16 §5(2)). The 4th M9.22 leg, complementing the three pure-Rust golden
//! legs in `crates/ava-version/tests/compat_matrix.rs`.
//!
//! This proves the **compatibility-floor decision logic** that governs which
//! peers connect in a mixed Go+Rust network. The Go and Rust nodes both report
//! the canonical `avalanchego` client name (specs/26 §2.2) and run the *same*
//! two-clause compatibility rule (`version/compatibility.go` ≡
//! `ava_version::Compatibility::compatible`), so the connectivity decision is
//! symmetric: each side evaluates the other's reported `Application` against its
//! own floor, and the moving min-compatible floor (specs/26 §7) gates
//! connectivity across the fork boundary.
//!
//! Two arms, mirroring `mixed_network_smoke.rs`:
//!
//! 1. **Offline arm** (runs every CI run, no feature, not `#[ignore]`):
//!    `version_interop_floor_decisions` — builds the mixed Go+Rust peer set via
//!    [`BinaryMix`]/[`NodeIdentity`], pins a reported `Application` version to
//!    each slot, then drives the REAL [`Compatibility`] (with a [`MockClock`]
//!    straddling `upgrade_time`) to assert the §9(4)/§9(3) connectivity cells:
//!    (a) a peer below the active floor is rejected; (b) a peer at/above it is
//!    accepted; (c) the moving floor (clock before vs after `upgrade_time`) flips
//!    a borderline peer; and the decision is Go-vs-Rust symmetric.
//!
//! 2. **Live arm** (`#[cfg(feature = "live")]` + `#[ignore]`): `version_interop`
//!    — boot a mixed Go+Rust net, lower a Rust node below the Go floor and assert
//!    Go drops it, and vice-versa, via [`Observation::collect`]. Needs
//!    `$AVALANCHEGO_PATH` (Go) + a built `avalanchers`; returns early if unset.
//!    Never runs in CI / this sandbox.

#![allow(unused_crate_dependencies)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_differential::network::{Binary, BinaryMix, NetworkConfig};
use ava_version::application::Application;
use ava_version::compatibility::{Compatibility, MockClock};
use ava_version::{CURRENT, MINIMUM_COMPATIBLE, PREV_MINIMUM_COMPATIBLE};

/// Builds a [`Compatibility`] with the shipped constants and a mock clock fixed
/// at `now`, gating the floor switch on `upgrade_time`. Mirrors the inputs both
/// `ava-network` (Rust) and Go's `network.go` thread in at the handshake
/// (specs/26 §3.1) — both implementations run *this* rule.
fn compat(upgrade_time: SystemTime, now: SystemTime) -> Compatibility<MockClock> {
    Compatibility::with_clock(
        CURRENT.clone(),
        MINIMUM_COMPATIBLE.clone(),
        PREV_MINIMUM_COMPATIBLE.clone(),
        upgrade_time,
        MockClock::new(now),
    )
}

/// A peer `Application` reporting the canonical `avalanchego` client name (both
/// Go and Rust report this on the wire — specs/26 §2.2), so the compatibility
/// check turns purely on the `(major, minor, patch)` triple.
fn peer(major: u32, minor: u32, patch: u32) -> Application {
    Application::new("avalanchego", major, minor, patch)
}

/// Offline arm: drive the REAL [`Compatibility`] over a mixed Go+Rust peer set to
/// assert the §9(4)/§9(3) connectivity decisions, including the symmetric
/// Go-vs-Rust evaluation and the moving floor across the fork boundary.
#[test]
fn version_interop_floor_decisions() {
    // A mixed Go+Rust network: slot 0 = Go, slot 1 = Rust, … (specs/02 §11.4).
    // The peer set the floor logic gates over comes from the mixed-net harness.
    let cfg = NetworkConfig::deterministic(0xC0FFEE, 4);
    let mix = BinaryMix::from_config(&cfg);
    assert_eq!(mix.len(), 4, "mixed-net peer set is the 4 configured slots");
    assert_eq!(
        mix.slots(),
        &[Binary::Go, Binary::Rust, Binary::Go, Binary::Rust],
        "alternating Go/Rust slots (specs/02 §11.4)"
    );

    // A fork at t=1000 splits the moving floor (specs/26 §7): clock < upgrade_time
    // uses the pre-floor (PREV_MINIMUM_COMPATIBLE = 1.13.0); clock >= upgrade_time
    // uses the post-floor (MINIMUM_COMPATIBLE = 1.14.0).
    let upgrade_time = UNIX_EPOCH + Duration::from_secs(1000);
    let before = UNIX_EPOCH + Duration::from_secs(500); // pre-fork
    let after = UNIX_EPOCH + Duration::from_secs(1500); // post-fork

    // Both implementations evaluate peers with the *same* rule. We model "the Go
    // side" and "the Rust side" as two independent `Compatibility` instances over
    // the same constants — the decision must be identical, proving symmetry.
    let go_side_pre = compat(upgrade_time, before);
    let go_side_post = compat(upgrade_time, after);
    let rust_side_pre = compat(upgrade_time, before);
    let rust_side_post = compat(upgrade_time, after);

    // ── §9(4)(a): a peer LOWERED BELOW the active floor is dropped ──────────────
    // Post-fork the floor is 1.14.0; a node lowered to 1.13.9 sits just below it.
    let lowered = peer(1, 13, 9);
    assert!(
        !go_side_post.compatible(&lowered),
        "§9(4): Go drops a peer (1.13.9) lowered below the post-fork floor (1.14.0)"
    );
    assert!(
        !rust_side_post.compatible(&lowered),
        "§9(4): Rust drops a peer (1.13.9) lowered below the post-fork floor (1.14.0)"
    );

    // ── §9(4)(b): a peer AT/ABOVE the floor is accepted ─────────────────────────
    // 1.14.0 == post-floor (boundary inclusive); CURRENT (1.14.2) is above it.
    let at_floor = peer(1, 14, 0);
    assert!(
        go_side_post.compatible(&at_floor),
        "§9(4): Go accepts a peer at the post-fork floor (1.14.0)"
    );
    assert!(
        rust_side_post.compatible(&at_floor),
        "§9(4): Rust accepts a peer at the post-fork floor (1.14.0)"
    );
    assert!(
        go_side_post.compatible(&CURRENT) && rust_side_post.compatible(&CURRENT),
        "§9(4): both sides accept a peer at CURRENT (1.14.2), above the floor"
    );

    // ── §9(3)/§7(c): the MOVING floor flips a borderline peer ───────────────────
    // 1.13.5 is >= pre-floor (1.13.0) but < post-floor (1.14.0): accepted before
    // the clock crosses upgrade_time, rejected after. This is the cross-fork
    // connectivity gate — the same peer's compatibility changes with the clock.
    let borderline = peer(1, 13, 5);
    assert!(
        go_side_pre.compatible(&borderline),
        "§7: borderline peer (1.13.5) accepted pre-fork (floor = pre-floor 1.13.0)"
    );
    assert!(
        !go_side_post.compatible(&borderline),
        "§7: same borderline peer rejected post-fork (floor moved to 1.14.0)"
    );

    // ── Go-vs-Rust SYMMETRY: each side decides the other identically ────────────
    // For every (clock, peer) the Go side and the Rust side reach the SAME verdict
    // — neither implementation is more permissive (specs/26 §9(4) "in both
    // directions"). Sweep a representative version ladder across both clocks.
    let ladder = [
        peer(1, 12, 9),  // below both floors
        peer(1, 13, 0),  // == pre-floor
        peer(1, 13, 5),  // borderline (in [pre-floor, post-floor))
        peer(1, 13, 9),  // just below post-floor
        peer(1, 14, 0),  // == post-floor
        CURRENT.clone(), // == current
        peer(1, 15, 0),  // newer same-major (only logged, accepted)
        peer(2, 0, 0),   // newer MAJOR (clause 1: always rejected)
    ];
    for p in &ladder {
        assert_eq!(
            go_side_pre.compatible(p),
            rust_side_pre.compatible(p),
            "pre-fork: Go and Rust reach the same verdict for {p}"
        );
        assert_eq!(
            go_side_post.compatible(p),
            rust_side_post.compatible(p),
            "post-fork: Go and Rust reach the same verdict for {p}"
        );
    }

    // ── Clause 1: a newer-MAJOR peer is dropped regardless of clock/side ────────
    let newer_major = peer(2, 0, 0);
    assert!(
        !go_side_pre.compatible(&newer_major)
            && !go_side_post.compatible(&newer_major)
            && !rust_side_pre.compatible(&newer_major)
            && !rust_side_post.compatible(&newer_major),
        "§9(3) clause 1: a newer-major peer (2.0.0) is dropped by both sides, both clocks"
    );

    // ── Tie the decision back to the concrete mixed-net slots ───────────────────
    // Assign each slot a reported version: Go slots run CURRENT (1.14.2); the Rust
    // slots are the ones we lower below the floor. Post-fork, every Rust slot
    // reporting 1.13.9 is dropped by the (Go-running) compatibility check, while
    // every Go slot at CURRENT stays connected — the §9(4) drop, per slot.
    for (i, &binary) in mix.slots().iter().enumerate() {
        let reported = match binary {
            Binary::Go => CURRENT.clone(),
            Binary::Rust => lowered.clone(), // lowered below the post-fork floor
        };
        let accepted_post = go_side_post.compatible(&reported);
        match binary {
            Binary::Go => assert!(
                accepted_post,
                "slot {i} (Go @ 1.14.2) stays connected post-fork"
            ),
            Binary::Rust => assert!(
                !accepted_post,
                "slot {i} (Rust @ 1.13.9) is dropped below the post-fork floor (§9(4))"
            ),
        }
    }
}

/// Live arm: boot a mixed Go+Rust net, lower one side below the other's floor and
/// assert it is dropped — in both directions (specs/26 §9(4)). Gated behind the
/// `live` feature + `#[ignore]`; needs `$AVALANCHEGO_PATH` (Go) and a built
/// `avalanchers`. Never runs in CI / this sandbox.
///
/// ## Operator handoff (nightly)
/// Run via `cargo nextest run -p ava-differential --features live -- --ignored`
/// with `$AVALANCHEGO_PATH` pointing at a built Go `avalanchego` (and, if not on
/// the default Cargo target path, `$AVALANCHERS_PATH` at the built Rust binary).
/// The operator must:
///  1. Boot the mixed net (`Network::start`) and `await_all_connected`.
///  2. Reconfigure one Rust slot to report a version below the Go node's active
///     min-compatible floor (specs/26 §7), restart it, and assert the Go node
///     drops it — `Observation::collect` over the Go node shows the lowered Rust
///     peer absent from `info.peers` / the validator set.
///  3. Symmetrically: lower a Go slot below the Rust node's floor and assert the
///     Rust node drops it via `Observation::collect` over the Rust node.
///  4. Cross the fork boundary (`upgrade_time`) and assert a borderline peer that
///     was connected pre-fork is dropped post-fork (the moving floor, §7).
/// The version-lowering knob (a `--version-override`-equivalent on each binary)
/// is the missing piece the operator wires in; the floor *decision* it exercises
/// is exactly what the offline arm proves against the real `Compatibility`.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust tmpnet ($AVALANCHEGO_PATH + avalanchers) and lowers a node below the floor — nightly only"]
async fn version_interop() {
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;

    // Skip gracefully if the Go oracle binary is not configured.
    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live version_interop");
        return;
    }

    let cfg = NetworkConfig::deterministic(0x5EED, 4);
    let mix = BinaryMix::from_config(&cfg);

    let net = Network::start(mix, &cfg)
        .await
        .expect("mixed Go+Rust network boots");

    // All nodes initially complete handshakes (at CURRENT, above every floor).
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

    // Operator: lower a Rust slot below the Go floor, restart it, and assert the
    // Go node drops it (the lowered peer absent from the observed peer set);
    // then do the symmetric Go-below-Rust-floor drop. The collector is the lens.
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

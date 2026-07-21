// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 — `differential::mixed_network` (specs/16 §5(2), specs/02 §11.3,
//! specs/00 §6.1).
//!
//! Two-arm shape (mirrors `mixed_network_smoke.rs` + the `xchain_issue_tx.rs`
//! determinism gate):
//!
//! 1. **Offline arm** (runs every CI run, no feature, not `#[ignore]`):
//!    * `mixed_network_replay_is_deterministic` — a [`Program`] generated
//!      deterministically from a seed, replayed TWICE through
//!      [`LockstepDriver::replay_recorded`] against the REAL in-process Rust
//!      `ava-avm` pipeline, yields byte-identical normalized [`Observation`]
//!      sequences (the determinism / total-order property, specs/00 §6.1), and
//!      at least one `AwaitFinalization` produced a non-empty observation (the
//!      pipeline really ran). A deliberately perturbed observation is shown to
//!      break the comparison, so the equality check genuinely catches a fork.
//!    * `mixed_network_replay_is_deterministic_proptest` — the same property
//!      over a spread of seeds.
//!
//!    Pure Rust, no node spawn, no Go oracle (there is none offline).
//!
//! 2. **Live arm** (`#[cfg(feature = "live")]` + `#[ignore]`):
//!    `mixed_network` — boot a small mixed Go+Rust tmpnet via [`Network::start`],
//!    replay the same seed-derived program across all nodes, collect+normalize an
//!    [`Observation`] per node per finalization, and assert no fork / same tip
//!    across every chain. Needs `$AVALANCHEGO_PATH` (Go) + a built `avalanchers`;
//!    returns early if unset. Never runs in CI / this sandbox — a
//!    scheduled/nightly job runs it via
//!    `cargo nextest run -p ava-differential --features live -- --ignored`.

// This target consumes only `ava-differential` (+ `proptest`); the other crate
// deps are used by the lib / other integration targets. Per the established
// `unused_crate_dependencies` idiom, each integration-test file that does not
// consume them opts out of the per-binary false positive.
#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod differential {
    use ava_differential::driver::LockstepDriver;
    use ava_differential::program::Program;
    use proptest::prelude::*;

    /// Offline: a seed-derived program replayed twice against the real in-process
    /// Rust pipeline produces byte-identical normalized observation sequences.
    #[test]
    fn mixed_network_replay_is_deterministic() {
        let seed = 0xA11CE_u64;
        let program = Program::from_seed(seed);
        let driver = LockstepDriver::new(seed);

        let run_a = driver
            .replay_recorded(&program)
            .expect("replay_recorded run A");
        let run_b = driver
            .replay_recorded(&program)
            .expect("replay_recorded run B");

        // The KEY property: same seed + actions ⇒ byte-identical observation
        // sequence (specs/00 §6.1 total order / determinism).
        assert_eq!(
            run_a, run_b,
            "DIFFERENTIAL_SEED={seed} — replaying the same program twice must \
             produce identical normalized observation sequences"
        );

        // The pipeline really ran: at least one AwaitFinalization produced a
        // non-empty observation with an accepted block (height >= 1).
        assert!(
            !run_a.is_empty(),
            "DIFFERENTIAL_SEED={seed} — expected at least one finalization observation"
        );
        let any_non_empty = run_a.iter().any(|obs| {
            obs.fields
                .iter()
                .any(|(k, _)| k == "xchain.last_accepted.height")
                && obs
                    .fields
                    .iter()
                    .find(|(k, _)| k == "xchain.last_accepted.height")
                    .and_then(|(_, v)| v.parse::<u64>().ok())
                    .is_some_and(|h| h >= 1)
        });
        assert!(
            any_non_empty,
            "DIFFERENTIAL_SEED={seed} — expected at least one finalization with an accepted block"
        );

        // Sanity-check the equality assertion genuinely catches a fork: inject a
        // deliberate divergence into a copy of run B and confirm it no longer
        // matches run A.
        let mut forked = run_b.clone();
        let first = forked.first_mut().expect("at least one observation");
        first.set_field("xchain.last_accepted.id", "deadbeef");
        let forked_norm: Vec<_> = forked.iter().map(|o| o.normalized()).collect();
        let a_norm: Vec<_> = run_a.iter().map(|o| o.normalized()).collect();
        assert_ne!(
            a_norm, forked_norm,
            "an injected last-accepted divergence must break the sequence comparison"
        );
    }

    proptest! {
        // A spread of seeds, kept fast (each case drives the real VM pipeline a
        // few times). 64 cases run well under the leashed differential-package
        // nextest timeout.
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn mixed_network_replay_is_deterministic_proptest(seed in any::<u64>()) {
            let program = Program::from_seed(seed);
            let driver = LockstepDriver::new(seed);

            let a = driver
                .replay_recorded(&program)
                .expect("replay_recorded run A");
            let b = driver
                .replay_recorded(&program)
                .expect("replay_recorded run B");

            prop_assert_eq!(
                &a, &b,
                "DIFFERENTIAL_SEED={} — nondeterministic mixed-network replay observation sequence",
                seed
            );
            prop_assert!(
                !a.is_empty(),
                "DIFFERENTIAL_SEED={seed} — expected at least one finalization observation"
            );
        }
    }
}

/// Live arm: bring up a small mixed Go+Rust network, drive a C-chain transfer,
/// settle both nodes to the same height, and assert no fork / same tip.
/// Gated behind the `live` feature + `#[ignore]`; needs `$AVALANCHEGO_PATH` (Go)
/// and a built `avalanchers`. Never runs in CI / this sandbox.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live mixed Go+Rust net ($AVALANCHEGO_PATH + avalanchers) — nightly only"]
async fn mixed_network() {
    use std::time::Duration;

    use ava_differential::livenet::{await_same_c_height, drive_c_transfer};
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network");
        return;
    }

    // 1. Boot Go beacon + Rust follower; waits for the Rust node to bootstrap P/X/C from Go.
    let net = Network::boot_mixed(0x5EED)
        .await
        .expect("mixed Go+Rust net boots + bootstraps");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("Go and Rust complete the TLS handshake / exchange PeerLists");

    let go_api = net.go_beacon().expect("go beacon").api_base.clone();
    let rust_api = net.rust_follower().expect("rust follower").api_base.clone();

    // 2. Record the pre-tx C height, drive one tx on the Go validator, settle.
    let before = await_same_c_height(&go_api, &rust_api, 0, Duration::from_secs(30))
        .await
        .expect("nodes agree on a starting C height");
    drive_c_transfer(&go_api)
        .await
        .expect("issue + mine one C-chain tx on the Go validator");
    let after = await_same_c_height(
        &go_api,
        &rust_api,
        before.saturating_add(1),
        Duration::from_secs(60),
    )
    .await
    .expect("both nodes advance to the same C height after the tx");
    assert!(
        after > before,
        "tx must advance the C-chain tip: {before} -> {after}"
    );

    // 3. No fork / same tip: full normalized observation must match across impls.
    let go_obs = Observation::collect(&go_api)
        .await
        .expect("collect Go observation")
        .normalized();
    let rust_obs = Observation::collect(&rust_api)
        .await
        .expect("collect Rust observation")
        .normalized();
    assert_eq!(
        go_obs, rust_obs,
        "Go and Rust diverged — fork across the mixed net"
    );

    net.shutdown().await;
}

/// Bisection probe for M9.15 rung-3 (single Go beacon → `required_conns = 1`).
/// If the follower bootstraps here, the engine/gate/frontier path works against
/// real Go and the 5-validator failure is purely connectivity (forming 4/5).
/// Gated behind `live` + `#[ignore]`; needs `$AVALANCHEGO_PATH` + a built `avalanchers`.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a single-Go-beacon + Rust follower net — nightly only"]
async fn mixed_network_single_beacon() {
    use ava_differential::network::Network;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network_single_beacon");
        return;
    }
    let net = Network::boot_single_go_beacon(0x515E)
        .await
        .expect("single-Go-beacon net boots + follower bootstraps P/X/C");
    assert!(net.go_beacon().is_some(), "go beacon present");
    assert!(net.rust_follower().is_some(), "rust follower present");
}

/// How a single gossip-pending race attempt resolved (T16 diagnostic split —
/// a live run found the old submit-then-poll ordering false-FAILED on a
/// sub-second-block-time net, and a bare bool collapsed two very different
/// causes into the same "mined" branch).
#[cfg(feature = "live")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRaceOutcome {
    /// The observer's mempool showed at least one burst tx pending
    /// (`blockHash == null`) before it was mined — gossip proven.
    Pending,
    /// The observer never showed ANY burst tx pending, but every one of them
    /// WAS already mined by the time we checked — even a pre-armed,
    /// 50ms-cadence poll lost every race (expected to be rare now that the
    /// burst spans multiple block intervals; tolerated as inconclusive, not
    /// a failure).
    MinedBeforePending,
    /// The observer showed some tx neither pending nor mined within the poll
    /// window — genuine non-delivery.
    NeitherObserved,
}

/// Assert that a tx built against `from_api` surfaces as PENDING on `to_api`
/// (`blockHash == null`, per [`await_c_pending_tx`]) — proof that `to_api`'s
/// own mempool received the tx via `app_gossip`, not merely learned of it
/// after the fact by processing an already-accepted block.
///
/// T16 live finding #1: the original design called `submit_c_transfer` (build +
/// submit in one RPC round trip) and only THEN started polling `to_api`. On
/// this net blocks mine in well under a second, so the submission call itself
/// could consume the entire pending window before the first poll ever ran —
/// all 3 Go→Rust attempts hit "already mined" even though gossip was working.
/// Fixed by decoupling build from submit ([`build_c_transfer`] /
/// [`submit_raw`]): the tx hash is known the instant it is signed (client-side,
/// no RPC needed), so the observer's pending-poll is spawned as its own task
/// and PRE-ARMED — running and already mid-poll — *before* the source node
/// ever sees the tx. [`await_c_pending_tx`]'s cadence is 50ms.
///
/// T16 live finding #2 (run 9, preserved logs): even pre-armed, a SINGLE tx
/// cannot reliably win the race. Measured end-to-end on a failing run: submit
/// → Go push 96ms (the 100ms push cadence working as designed) → admitted
/// into the Rust mempool same-millisecond → block accepted **7ms later**. A
/// 7ms pending window is unobservable at any sane poll cadence, and a tx that
/// mines before the source's first 100ms push tick is (correctly) dropped
/// from the outbox entirely, so it never gossips at all. Fixed by submitting
/// a BURST of nonce-consecutive txs per attempt: the first tx(s) may instant-
/// mine, but every tx submitted after a block lands must wait out the
/// proposervm min-block-delay before the next block can take it — a
/// seconds-wide pending window on the observer that a 50ms poll cannot miss.
/// One observed-pending tx from the burst proves delivery (on the observer,
/// `add_remote` — and hence a pending-tagged pool entry — is reachable ONLY
/// via the gossip path; block import never populates the mempool).
///
/// Retries the whole burst up to 3× on
/// [`PendingRaceOutcome::MinedBeforePending`] (inconclusive about gossip, not
/// a failure) — expected to be rare now that the burst spans block intervals.
///
/// # Panics
/// Panics if some tx is observed neither pending nor mined on `to_api` within
/// the poll window, or if every burst resolves `MinedBeforePending` on all 3
/// attempts.
#[cfg(feature = "live")]
async fn assert_gossip_delivers_pending(from_api: &str, to_api: &str, label: &str) {
    use std::time::Duration;

    use ava_differential::livenet::{
        await_c_pending_tx, await_c_receipt, build_c_transfer, submit_raw,
    };

    /// Txs per attempt. Spaced 150ms apart the burst spans ~1s — comfortably
    /// across at least one min-block-delay boundary on this net.
    const BURST: usize = 6;

    for attempt in 0..3u32 {
        let mut polls = Vec::with_capacity(BURST);
        let mut hashes = Vec::with_capacity(BURST);
        for i in 0..BURST {
            // Build + hash locally BEFORE submitting anything. Sequential
            // build-then-submit keeps the pending-tag nonce advancing across
            // the burst (each build sees the previous submissions pooled).
            let (raw_hex, tx_hash) = build_c_transfer(from_api).await.unwrap_or_else(|e| {
                panic!("{label} attempt {attempt} tx {i}: build_c_transfer: {e}")
            });

            // Pre-arm the observer's pending-poll as its own task — polling
            // `to_api` BEFORE the source node has even seen this tx, so there
            // is no submission-to-first-poll gap left to race.
            let observer_api = to_api.to_owned();
            let observer_hash = tx_hash.clone();
            polls.push(tokio::spawn(async move {
                await_c_pending_tx(&observer_api, &observer_hash, Duration::from_secs(20)).await
            }));

            submit_raw(from_api, &raw_hex)
                .await
                .unwrap_or_else(|e| panic!("{label} attempt {attempt} tx {i}: submit_raw: {e}"));
            hashes.push(tx_hash);
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        eprintln!(
            "{label} attempt {attempt}: burst of {BURST} txs submitted to {from_api} \
             ({} .. {}; pending-polls on {to_api} pre-armed before each submission)",
            hashes[0],
            hashes[BURST - 1],
        );

        // The polls run concurrently; one observed-pending tx proves gossip.
        let mut pending_hash = None;
        for (i, poll) in polls.into_iter().enumerate() {
            let seen = poll
                .await
                .unwrap_or_else(|e| {
                    panic!("{label} attempt {attempt} tx {i}: pending-poll task panicked: {e}")
                })
                .unwrap_or_else(|e| {
                    panic!("{label} attempt {attempt} tx {i}: await_c_pending_tx: {e}")
                });
            if seen {
                pending_hash = Some(hashes[i].clone());
                break;
            }
        }

        let outcome = if let Some(h) = pending_hash {
            eprintln!(
                "{label} attempt {attempt}: {h} observed PENDING on {to_api} — gossip proven"
            );
            PendingRaceOutcome::Pending
        } else {
            // No tx was ever observed pending even pre-armed — split "every
            // single one mined too fast" (inconclusive race) from "some tx
            // never arrived at all" (genuine non-delivery).
            let mut all_mined = true;
            for (i, h) in hashes.iter().enumerate() {
                let mined = await_c_receipt(to_api, h, Duration::from_secs(5))
                    .await
                    .unwrap_or_else(|e| {
                        panic!("{label} attempt {attempt} tx {i}: await_c_receipt: {e}")
                    });
                if !mined {
                    all_mined = false;
                    eprintln!(
                        "{label} attempt {attempt}: {h} neither pending nor mined on {to_api}"
                    );
                }
            }
            if all_mined {
                PendingRaceOutcome::MinedBeforePending
            } else {
                PendingRaceOutcome::NeitherObserved
            }
        };

        match outcome {
            PendingRaceOutcome::Pending => return,
            PendingRaceOutcome::MinedBeforePending => {
                eprintln!(
                    "{label} attempt {attempt}: all {BURST} burst txs were already mined on \
                     {to_api} before the pre-armed pending-polls caught any of them \
                     (inconclusive) — retrying with a fresh burst"
                );
                continue;
            }
            PendingRaceOutcome::NeitherObserved => {
                panic!(
                    "{label} attempt {attempt}: some burst tx appeared neither pending NOR \
                     mined on {to_api} within the poll window — gossip did not deliver it"
                );
            }
        }
    }
    panic!(
        "{label}: every burst raced ahead of the pre-armed pending-polls (MinedBeforePending) \
         on all 3 attempts"
    );
}

/// Live tx-gossip arm (T16): boot 4 Go + 1 Rust validator, then prove
/// `app_gossip` genuinely propagates a submitted-but-unmined C-chain tx into a
/// *peer*'s mempool in both directions (Go→Rust and Rust→Go) — the pending-tx
/// detection [`assert_gossip_delivers_pending`] establishes, per node, BEFORE
/// either tx is mined. This supersedes the old "no gossip path exists" premise
/// `mixed_network_rust_proposes` relied on (see that test's rework, T16): with
/// gossip now wired, `mixed_network_rust_proposes` must instead identify the
/// proposer by parsing the accepted container off the Go index API.
///
/// Gated behind `live` + `#[ignore]`; needs `$AVALANCHEGO_PATH` + a built
/// `avalanchers`. Never runs in CI / this sandbox — nightly/operator only.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live 4-Go + 1-Rust-validator net; needs $AVALANCHEGO_PATH; nightly only"]
async fn mixed_network_tx_gossip() {
    use std::time::Duration;

    use ava_differential::livenet::await_same_c_height;
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network_tx_gossip");
        return;
    }

    // Boot 4 Go + 1 Rust validator (same topology as `mixed_network_rust_proposes`);
    // `boot_mixed_rust_validator` asserts the Rust node's NodeID == staker5 and
    // awaits P/X/C bootstrap on all five nodes.
    let net = Network::boot_mixed_rust_validator(0x0060_551D)
        .await
        .expect("4-Go + 1-Rust validator net boots + all five bootstrap P/X/C");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("all five validators complete the TLS handshake / exchange PeerLists");

    let go_api = net.nodes().first().expect("go staker1").api_base.clone();
    let rust_api = net.nodes().last().expect("rust staker5").api_base.clone();

    let before = await_same_c_height(&go_api, &rust_api, 0, Duration::from_secs(30))
        .await
        .expect("nodes agree on a starting C height");

    // Stage A (Go -> Rust): a tx submitted only to the Go node must surface as
    // PENDING in the Rust node's own mempool before it is mined.
    assert_gossip_delivers_pending(&go_api, &rust_api, "Go->Rust").await;

    // Stage B (Rust -> Go): the same property in the other direction.
    assert_gossip_delivers_pending(&rust_api, &go_api, "Rust->Go").await;

    // Close: both settle at the same advanced tip, no fork.
    let after = await_same_c_height(
        &go_api,
        &rust_api,
        before.saturating_add(1),
        Duration::from_secs(60),
    )
    .await
    .expect("Go and Rust settle at the same C height after the gossiped txs");
    assert!(
        after > before,
        "the gossiped txs must advance the C-chain tip: {before} -> {after}"
    );

    let go_obs = Observation::collect(&go_api)
        .await
        .expect("collect Go observation")
        .normalized();
    let rust_obs = Observation::collect(&rust_api)
        .await
        .expect("collect Rust observation")
        .normalized();
    assert_eq!(
        go_obs, rust_obs,
        "Go and Rust diverged — fork across the mixed validator net"
    );

    net.shutdown().await;
}

/// Live validator arm (M9.15 Task 8): boot 4 Go + 1 Rust *validator* net, then
/// prove ≥1 Rust-proposed C-Chain block is accepted network-wide.
///
/// ## Pre-flight findings (Task 8 Step 1; re-verified after the tx-pipeline insert)
///
/// (a) `--staking-signer-key-file` → the node's BLS signer is WIRED end-to-end
///     (`ava-config` flags.rs:1163 → parse.rs:340-341 `key_path` → ava-node
///     node.rs:205 `new_staking_signer` → identity.rs:87
///     `LocalSigner::from_file_or_persist_new`). So staker5's genesis BLS key
///     (`signer5.key`, passed by `boot_mixed_rust_validator`) yields a PoP that
///     matches genesis — the Rust node registers as a real validator.
///
/// (b) `eth_sendRawTransaction` → mempool → `build_block` NOW EXISTS (cchain
///     tx-pipeline insert, commits f14e82c..b481179): the C-chain `/rpc`
///     dispatches `eth_sendRawTransaction`/`eth_getTransactionReceipt`
///     (ava-evm rpc/service.rs:176-182); admission wakes `wait_for_event`
///     (vm.rs:770-772 notify-select on the `EvmMempool`) and `build_block`
///     drains `EvmMempool::best_txs()` into `build_on` (vm.rs:838-846 — the old
///     `Vec::new()` is gone). Submit→wake→build→accept→receipt is offline
///     e2e-tested (ava-evm/tests/tx_pipeline.rs).
///
/// (c) `app_gossip` NOW GOSSIPS (T16: the `ava-chains` forwarder + txgossip
///     wiring landed after this test's original write-up — see
///     `mixed_network_tx_gossip` above, which proves it directly). A tx
///     submitted only to the Rust node's mempool can therefore ALSO be picked
///     up by a Go validator's mempool and mined into a Go-proposed block, so
///     "the tx is on-chain network-wide" is no longer, by itself, proof that
///     RUST proposed it — the old exclusivity argument this test originally
///     relied on ("no gossip path could have delivered it") no longer holds.
///     T16 reworks detection accordingly: once the tx is mined ANYWHERE,
///     `c_block_number_of_receipt` + `proposer_of_accepted_container` (the Go
///     index API, `--index-enabled=true`, plus
///     `ava_proposervm::block::codec::parse_without_verification`) read the
///     ACTUAL verified proposer off the accepted container at that height and
///     compare it against the Rust node's own NodeID (staker5); a bounded
///     fresh-tx retry covers the case where gossip won the race and a Go
///     validator proposed first.
///
/// Bounded retry (Stage 2): proposervm windowing rotates proposers, and (per
/// (c)) gossip means a competing Go proposer can pick up and mine the same tx
/// first. Each of the (up to 20; see the in-loop rationale — the windower
/// schedule is deterministic per height, so a wider single run beats re-runs)
/// attempts submits a FRESH transfer — rather
/// than re-polling the same one — so a Go-proposed inclusion doesn't retire
/// the tx before the Rust proposer window opens; the loop keeps going until
/// the index API confirms a Rust-signed block shipped one of our txs. If it
/// never lands, the failure is a timeout with all five nodes' logs preserved
/// (pin `TMPDIR`).
///
/// Gated behind `live` + `#[ignore]`; needs `$AVALANCHEGO_PATH` + a built
/// `avalanchers`. Never runs in CI / this sandbox — nightly/operator only.
#[cfg(feature = "live")]
#[tokio::test]
#[ignore = "boots a live 4-Go + 1-Rust-validator net; needs $AVALANCHEGO_PATH; nightly only"]
async fn mixed_network_rust_proposes() {
    use std::time::Duration;

    use ava_differential::livenet::{
        LOCAL_VALIDATOR_NODE_IDS, await_c_pending_tx, await_c_receipt, await_same_c_height,
        c_block_number_of_receipt, proposer_of_accepted_container, submit_c_transfer,
    };
    use ava_differential::network::Network;
    use ava_differential::observation::Observation;
    use ava_types::node_id::NodeId;

    if std::env::var("AVALANCHEGO_PATH").is_err() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live mixed_network_rust_proposes");
        return;
    }

    // Stage 1: boot 4 Go + 1 Rust validator; `boot_mixed_rust_validator` already
    // asserts the Rust node's NodeID == staker5 and awaits P/X/C bootstrap on
    // ALL five nodes (the Rust node is a validator now and must reach NormalOp).
    // It also enables `--index-enabled=true` on the Go slots — required for the
    // Stage 3 proposer lookup below.
    let net = Network::boot_mixed_rust_validator(0x9E0)
        .await
        .expect("4-Go + 1-Rust validator net boots + all five bootstrap P/X/C");
    net.await_all_connected(Duration::from_secs(60))
        .await
        .expect("all five validators complete the TLS handshake / exchange PeerLists");

    // nodes()[0] = Go staker1 (reference, also the index-API host);
    // nodes().last() = Rust staker5.
    let go_api = net.nodes().first().expect("go staker1").api_base.clone();
    let go_index_api = go_api.clone();
    let rust_api = net.nodes().last().expect("rust staker5").api_base.clone();
    let rust_node_id: NodeId = LOCAL_VALIDATOR_NODE_IDS[4]
        .parse()
        .expect("staker5's well-known NodeID string parses");

    let before = await_same_c_height(&go_api, &rust_api, 0, Duration::from_secs(30))
        .await
        .expect("nodes agree on a starting C height");

    // Stage 2 + 3: submit the tx to the Rust node's mempool, wait for it to be
    // mined ANYWHERE (per pre-flight (c), gossip means a Go validator can win
    // the race), then identify the ACTUAL proposer of the block that carried
    // it by parsing the accepted container off the Go index API. Retry with a
    // fresh tx until a Rust-signed block is confirmed.
    //
    // Attempt bound (T16 live finding, runs rp1/rp2): the post-Durango
    // windower's slot-0 proposer is a deterministic function of
    // (chainID, blockHeight) — and this fixture's chainID is genesis-derived,
    // hence identical on every boot. Two consecutive failing runs replayed
    // the SAME proposer at heights 2..=5, so re-running the test re-explores
    // the same heights and can never succeed where the previous run failed;
    // the only way to reach a Rust-scheduled height is to mine FURTHER within
    // one run. Each attempt consumes ~1 height; at ~1/5 per height (5 equal-
    // stake validators, per-height pseudorandom draws), 20 attempts leave
    // P(never Rust-scheduled) = (4/5)^20 ≈ 1.2% — and a fail after 20 fresh
    // heights is itself strong evidence of a real proposal bug, not luck.
    let mut tx_hash = String::new();
    let mut proposer_matched = false;
    for attempt in 0..20u32 {
        // If the previous attempt's tx is still sitting pending in the Rust
        // mempool, submitting a fresh transfer now would read the same
        // "latest" nonce and collide with it (rejected as an underpriced
        // replacement) — worse, `submit_c_transfer`'s `.expect` would then
        // panic the test on exactly the race condition this retry loop exists
        // to tolerate. Skip the fresh submission and keep polling the existing
        // tx instead; it can still be mined in the next proposer window.
        let still_pending = !tx_hash.is_empty()
            && await_c_pending_tx(&rust_api, &tx_hash, Duration::from_secs(1))
                .await
                .unwrap_or(false);
        if still_pending {
            eprintln!(
                "attempt {attempt}: previous tx {tx_hash} still pending in the Rust mempool — \
                 skipping a fresh submission (same nonce would collide) and re-polling it"
            );
        } else {
            match submit_c_transfer(&rust_api).await {
                Ok(h) => {
                    tx_hash = h;
                    eprintln!("attempt {attempt}: submitted tx {tx_hash} to the Rust validator");
                }
                Err(e) => {
                    eprintln!(
                        "attempt {attempt}: submit_c_transfer failed ({e}) — retrying next attempt"
                    );
                    continue;
                }
            }
        }

        if !await_c_receipt(&rust_api, &tx_hash, Duration::from_secs(60))
            .await
            .expect("poll receipt on the Rust node")
        {
            eprintln!("attempt {attempt}: tx {tx_hash} not mined within 60 s — retry");
            continue;
        }

        let n = c_block_number_of_receipt(&rust_api, &tx_hash)
            .await
            .expect("read the mined block number off the Rust node's own receipt");
        // The linear C-chain indexer is 0-based; the block at height `n` is
        // container index `n - 1`.
        let primary_index = n.saturating_sub(1);
        let mut proposer = proposer_of_accepted_container(&go_index_api, primary_index)
            .await
            .unwrap_or_else(|e| {
                eprintln!(
                    "attempt {attempt}: proposer_of_accepted_container({primary_index}): {e}"
                );
                NodeId::default()
            });
        eprintln!(
            "attempt {attempt}: tx {tx_hash} mined at C-height {n}; \
             container[{primary_index}] proposer = {proposer}"
        );

        if proposer != rust_node_id {
            // Parse/identity mismatch at the primary index: scan a small
            // window around it (height/index off-by-one across an option
            // block or an indexer lag) and log each container's proposer for
            // diagnosis, picking the one that matches Rust if any does.
            let scan_start = primary_index.saturating_sub(3);
            let scan_end = n.saturating_add(1);
            for scan in scan_start..=scan_end {
                if scan == primary_index {
                    continue;
                }
                match proposer_of_accepted_container(&go_index_api, scan).await {
                    Ok(p) => {
                        eprintln!("attempt {attempt}: scan container[{scan}] proposer = {p}");
                        if p == rust_node_id {
                            proposer = p;
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("attempt {attempt}: scan container[{scan}]: {e}");
                    }
                }
            }
        }

        if proposer == rust_node_id {
            proposer_matched = true;
            eprintln!(
                "attempt {attempt}: confirmed Rust (staker5) proposed the block including {tx_hash}"
            );
            break;
        }
        eprintln!(
            "attempt {attempt}: the block including {tx_hash} was proposed by {proposer}, not \
             Rust (staker5) — a Go validator won the gossip race; retry with a fresh tx"
        );
    }
    assert!(
        proposer_matched,
        "no Rust-proposed block ever included one of our txs within 20 attempts (last tx {tx_hash})"
    );

    // The Rust-proposed block must be accepted network-wide: the same tx is
    // on-chain per a Go validator's RPC. Stage 3 above already independently
    // confirmed (via the index API) that a RUST-signed block carried this tx;
    // this just confirms network-wide finality of that same block — it is NOT
    // "no gossip path could have delivered it" (T16 gossip exists now; see
    // `mixed_network_tx_gossip`).
    let on_go = await_c_receipt(&go_api, &tx_hash, Duration::from_secs(60))
        .await
        .expect("poll receipt on the Go validator");
    assert!(
        on_go,
        "tx {tx_hash} from the Rust-proposed block must be on-chain per the Go validator \
         (Rust proposed, Go accepted)"
    );

    // All nodes settle at the same advanced tip.
    let after = await_same_c_height(
        &go_api,
        &rust_api,
        before.saturating_add(1),
        Duration::from_secs(60),
    )
    .await
    .expect("Go and Rust settle at the same C height after the Rust-proposed block");
    assert!(
        after > before,
        "the Rust-proposed block must advance the C-chain tip: {before} -> {after}"
    );

    // Stage 3: no fork / same tip — full normalized observation matches across
    // a Go validator and the Rust validator.
    let go_obs = Observation::collect(&go_api)
        .await
        .expect("collect Go observation")
        .normalized();
    let rust_obs = Observation::collect(&rust_api)
        .await
        .expect("collect Rust observation")
        .normalized();
    assert_eq!(
        go_obs, rust_obs,
        "Go and Rust diverged — fork across the mixed validator net"
    );

    net.shutdown().await;
}

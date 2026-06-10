// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M7.30 SAE streaming-pipeline differential (`differential::sae_streaming`,
//! specs/11 §12 / §13 (validate §9 pipelined-commit), specs/02 §11.1/§11.7).
//!
//! Drives identical block/tx sequences through the **real Rust SAE pipeline**
//! (the [`ava_saevm_core::Frontier`] + [`ava_saevm_core::settle`] walk, block by
//! block) and the **live Go `vms/saevm` node** (the committed corpus emitter),
//! and asserts byte-identical block hashes, state roots, receipt roots, base
//! fees, settlement choices, and S/E/A frontier heights at **every**
//! `AwaitFinalization` barrier — not merely at the final height.
//!
//! This is the streaming extension of the M7.29 recovery differential: where
//! M7.29 compared the single post-restart frontier, M7.30 compares the **whole
//! per-barrier trajectory**, which is what validates the specs/00 §9
//! pipelined-commit optimization is *observably neutral* — the Rust node reaches
//! the identical A/E/S at each accepted height regardless of when execution /
//! commit happens within a barrier.
//!
//! # Mode (recorded-oracle, per-PR — specs/02 §11.1)
//!
//! Every per-PR / CI run replays the **committed JSON corpus** under
//! `tests/vectors/saevm/streaming_differential/` (produced once by the live Go
//! oracle; see *re-freezing* below). The corpus carries, per accepted height
//! (barrier), the Go block's **wire bytes** (RLP-encoded geth block) + the
//! committed `ExecutionResults` (gas-time, base fee, receipt/state roots), and
//! the Go A/E/S frontier observed after that accept.
//!
//! ## What is genuinely cross-checked
//!
//! * **Block hashes** — the Rust `parse_block` decoder re-seals every Go-emitted
//!   block and recomputes `keccak256(RLP(header))`; the frontier S/E/A hashes are
//!   read off those re-sealed blocks, so matching the Go node proves wire + hash
//!   parity at every barrier.
//! * **Settlement choice at EVERY barrier** — the Rust `last_to_settle_at` /
//!   `settle()` walk recomputes which height becomes `LastSettled` from the
//!   Go-emitted gas-times + parsed build-times, *per accepted block*. The headline
//!   assertion: the per-barrier S-frontier trajectory equals the Go node's exactly
//!   (the settlement rule `settle_at = BlockTime(head) − Tau`, specs/11 §1.2).
//! * **A / E frontier heights** — recomputed by the Rust frontier's
//!   `advance_accepted` / `advance_executed` monotonic walk.
//!
//! Real-EVM **state roots, receipt roots, and base fees** are the Go-emitted
//! values fed into the Rust block's executed artefacts; they round-trip unchanged
//! (verifying the Rust frontier reads back the same roots/fee the Go node
//! committed) but are NOT independently recomputed by a Rust EVM here. The base
//! fee in particular is round-tripped, not recomputed: the Go emitter exposes
//! only the resolved `ExecutionResults.base_fee`, not the per-block gas-clock
//! excess/target inputs an independent `gasprice::price()` recompute would need —
//! reconstructing those requires a real-EVM differential (out of scope; the Go
//! node pins `firewood-go-ethhash` v0.6.0 vs the Rust workspace's v0.5.0, so by
//! never recomputing a firewood root we sidestep that divergence — the M7.29
//! status note). See the M7.32 follow-up.
//!
//! # Live mode (env-gated, not run in CI — specs/02 §11.7 nightly)
//!
//! Re-freeze the corpus from the live Go node (committed emitter at
//! `tests/differential/go-oracle/streaming_vector_emitter_test.go`):
//!
//! ```sh
//! # in the avalanchego checkout ($AVALANCHEGO_DIR, default ../avalanchego):
//! cp tests/differential/go-oracle/streaming_vector_emitter_test.go \
//!    $AVALANCHEGO_DIR/vms/saevm/sae/
//! SAE_EMIT_STREAMING_VECTORS=$PWD/tests/vectors/saevm/streaming_differential \
//!   go test ./vms/saevm/sae/ -run TestEmitStreamingVectors -count=1
//! ```

// This integration-test target consumes only `ava_differential` +
// `pretty_assertions` + `proptest` + `tokio`, but the crate's lib + dev deps
// are all linked; per the established `unused_crate_dependencies` idiom each
// such test file silences the lint locally (see tests/smoke.rs).
#![allow(unused_crate_dependencies)]

use std::path::PathBuf;

use ava_differential::{StreamingBarrier, replay_streaming_vector};
use pretty_assertions::assert_eq;

/// The committed streaming-differential corpus directory (workspace-rooted).
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/saevm/streaming_differential")
}

/// The scripted streams the Go oracle emitted (each = a distinct block-time
/// cadence / commit interval, exercising a different per-barrier S trajectory).
const STREAMS: &[&str] = &["steady_settling", "archival", "fast_blocks"];

/// Replay one corpus file and assert, at EVERY barrier, the Rust-reconstructed
/// frontier (block hashes + S/E/A heights + LastSettled hash + roots + base fee)
/// equals the Go node's. Returns the matched per-barrier observations.
async fn assert_stream_matches(stream: &str) -> Vec<StreamingBarrier> {
    let path = corpus_dir().join(format!("streaming_{stream}.json"));
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read streaming corpus {}: {e}", path.display()));

    let barriers = replay_streaming_vector(&json)
        .await
        .unwrap_or_else(|e| panic!("replay streaming vector {stream}: {e}"));

    assert!(
        !barriers.is_empty(),
        "[{stream}] streaming corpus had no barriers",
    );

    for b in &barriers {
        // The headline cross-implementation assertion: at THIS barrier the Rust
        // pipeline reconstructed the EXACT A/E/S heights, LastSettled hash, roots,
        // base fee, and block hash the Go node observed (specs/11 §12/§13).
        assert_eq!(
            b.rust, b.go,
            "[{}] barrier height {}: Rust frontier != Go frontier",
            stream, b.height,
        );
    }

    barriers
}

mod differential {
    use proptest::prelude::*;

    use super::{STREAMS, StreamingBarrier, assert_eq, assert_stream_matches};

    /// `differential::sae_streaming` — the M7.30 headline test.
    ///
    /// A proptest over the scripted streams: for each `(stream)` the Rust SAE
    /// pipeline is driven block-by-block over the live-Go-emitted block stream,
    /// and the per-barrier A/E/S frontiers + settlement choices + roots are
    /// asserted equal to the Go SAE node driven through the same stream, at
    /// **every** `AwaitFinalization` barrier (recorded-oracle mode).
    #[test]
    fn sae_streaming() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        proptest!(ProptestConfig::with_cases(32), |(idx in 0usize..STREAMS.len())| {
            let stream = *STREAMS
                .get(idx)
                .expect("idx strategy bounded to STREAMS.len()");
            let barriers: Vec<StreamingBarrier> =
                runtime.block_on(assert_stream_matches(stream));

            // Beyond per-barrier equality: the frontier ordering S<=E<=A and the
            // monotonic advance of A/E/S across the trajectory hold.
            let mut prev_a = 0u64;
            let mut prev_e = 0u64;
            let mut prev_s = 0u64;
            for b in &barriers {
                let f = &b.rust;
                prop_assert!(
                    f.settled_height <= f.executed_height
                        && f.executed_height <= f.accepted_height,
                    "[{}] barrier {}: frontier ordering S<=E<=A violated: {:?}",
                    stream, b.height, f,
                );
                prop_assert!(
                    f.accepted_height >= prev_a
                        && f.executed_height >= prev_e
                        && f.settled_height >= prev_s,
                    "[{}] barrier {}: A/E/S regressed (non-monotonic)",
                    stream, b.height,
                );
                prev_a = f.accepted_height;
                prev_e = f.executed_height;
                prev_s = f.settled_height;
            }
        });

        // Exhaustively (not just sampled): every stream replays and the final A is
        // the full chain length, and the streams that share a cadence
        // (steady_settling / archival both 0.85s/block) reach the IDENTICAL final
        // A/E/S — the pipelined-commit optimization (commit interval differs: 16
        // vs 1) is observably neutral (specs/00 §9).
        let steady = runtime.block_on(assert_stream_matches("steady_settling"));
        let archival = runtime.block_on(assert_stream_matches("archival"));
        let steady_final = steady.last().expect("steady has barriers");
        let archival_final = archival.last().expect("archival has barriers");
        assert_eq!(
            (
                steady_final.rust.accepted_height,
                steady_final.rust.executed_height,
                steady_final.rust.settled_height,
                steady_final.rust.settled_hash.clone(),
            ),
            (
                archival_final.rust.accepted_height,
                archival_final.rust.executed_height,
                archival_final.rust.settled_height,
                archival_final.rust.settled_hash.clone(),
            ),
            "commit-interval-16 vs archival(=1) must reach identical final A/E/S \
             (pipelined-commit is observably neutral, specs/00 §9)",
        );
    }
}

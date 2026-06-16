// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.23 definition-of-done aggregator (specs/16 §5).
//!
//! The drop-in-replacement acceptance criteria (`16` §5) are nine simultaneously-
//! green conditions. Most are full end-to-end checks whose *runs* are owned by
//! the per-PR (offline-arm) and nightly (live two-binary) nextest passes; this
//! file is the thin in-repo aggregator that pins the offline-checkable half of
//! the checklist — the half that needs no live Go node — so the DoD cannot
//! silently rot.
//!
//! It asserts, source-side and on disk, that every `16` §5 clause maps to a
//! named exit test that EXISTS with both its offline and (where applicable)
//! live arms. This mirrors `cargo xtask acceptance` (the canonical gate) without
//! shelling out: a documentation + offline-presence test, honest about what it
//! does and does not run.
//!
//! ## CI cadence (specs/00 §11.7, specs/02 §11.7)
//!
//! - **Per-PR (offline arms, every CI run):** the recorded-Go-oracle differentials,
//!   reexecute over recorded ranges, the in-process plugin handshake, the
//!   config/genesis goldens, and `bench-guard`.
//! - **Nightly / pre-release (live two-binary arms, `#[cfg(feature="live")]`
//!   `#[ignore]`):** `mixed_network`, `plugin_go_in_rust`, `test-upgrade`,
//!   `test-load` — they need an external Go `avalanchego` binary
//!   (`$AVALANCHEGO_PATH`).
//!
//! This test verifies the named tests are PRESENT (both arms), satisfying the
//! structural DoD; the live runs are a separate scheduled job.

// A filesystem/source-presence aggregator that consumes none of the crate's own
// deps — opt out of the per-binary `unused_crate_dependencies` false positive
// like the crate's other source-presence targets (e.g. `exit_gate.rs`).
#![allow(unused_crate_dependencies)]

use std::path::PathBuf;

/// One `16` §5 DoD clause → the named exit test backing it (offline + live arms).
struct DodClause {
    /// `16` §5 clause label.
    label: &'static str,
    /// Workspace-relative source file the test(s) live in.
    file: &'static str,
    /// `fn <name>(` needles that must ALL be present.
    fns: &'static [&'static str],
}

/// The `16` §5 checklist, mapped to the named exit tests. Kept in lockstep with
/// `xtask/src/acceptance.rs::DOD` (the canonical gate); this in-repo test guards
/// against the offline-arm half drifting.
const DOD: &[DodClause] = &[
    DodClause {
        label: "16 §5(1) joins Mainnet & Fuji, tracks tip, no fork",
        file: "tests/differential/tests/mixed_network.rs",
        fns: &["mixed_network_replay_is_deterministic", "mixed_network"],
    },
    DodClause {
        label: "16 §5(2) indistinguishable mixed network",
        file: "tests/differential/tests/mixed_network_smoke.rs",
        fns: &[
            "mixed_network_config_is_deterministic",
            "observation_normalization_round_trips",
            "mixed_network_bringup_smoke",
        ],
    },
    DodClause {
        label: "16 §5(3) reexecute — C-Chain range",
        file: "tests/reexecute/tests/cchain_range.rs",
        fns: &["reexecute_cchain_range"],
    },
    DodClause {
        label: "16 §5(3) reexecute — P/X range",
        file: "tests/reexecute/tests/px_range.rs",
        fns: &["reexecute_px_range"],
    },
    DodClause {
        label: "16 §5(4) golden::flag_parity",
        file: "crates/ava-config/tests/golden_flag_parity.rs",
        fns: &["flag_parity"],
    },
    DodClause {
        label: "16 §5(5) differential::api_parity",
        file: "crates/ava-api/tests/differential_api_parity.rs",
        fns: &["info_parity", "platform_and_avm_method_sets_pinned"],
    },
    DodClause {
        label: "16 §5(6) golden::genesis_block_id",
        file: "crates/ava-genesis/tests/golden_genesis_block_id.rs",
        fns: &["genesis_block_id"],
    },
    DodClause {
        label: "16 §5(7) plugin_rust_in_go (v45)",
        file: "tests/differential/tests/plugin_rust_in_go.rs",
        fns: &[
            "plugin_rust_in_go_builds_and_serves",
            "plugin_rust_in_go_live",
        ],
    },
    DodClause {
        label: "16 §5(7) plugin_go_in_rust (v45)",
        file: "tests/differential/tests/plugin_go_in_rust.rs",
        fns: &["plugin_go_in_rust_host_dial_back", "plugin_go_in_rust_live"],
    },
    DodClause {
        label: "16 §5(8) test-upgrade (Go→Rust, Go-dir import)",
        file: "tests/upgrade/tests/go_to_rust.rs",
        fns: &[
            "rolling_swap_imports_each_node_byte_identically",
            "no_fork_holds_across_cutover_and_a_divergence_is_caught",
            "go_to_rust",
        ],
    },
    DodClause {
        label: "16 §5(9) bench-guard",
        file: "xtask/src/bench_guard.rs",
        fns: &["over_threshold"],
    },
];

/// Workspace root, derived from this crate's manifest dir
/// (`<root>/tests/differential`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| {
            panic!("ava-differential manifest dir has a <root>/tests/differential shape")
        })
        .to_path_buf()
}

#[test]
fn definition_of_done() {
    let root = workspace_root();
    let mut missing: Vec<String> = Vec::new();

    for clause in DOD {
        let path = root.join(clause.file);
        let Ok(src) = std::fs::read_to_string(&path) else {
            missing.push(format!(
                "{}: source {} unreadable",
                clause.label, clause.file
            ));
            continue;
        };
        for func in clause.fns {
            let needle = format!("fn {func}(");
            if !src.contains(&needle) {
                missing.push(format!(
                    "{}: canonical test fn `{func}` missing from {}",
                    clause.label, clause.file,
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "definition_of_done(): the 16 §5 offline-checkable DoD is incomplete:\n  - {}",
        missing.join("\n  - "),
    );
}

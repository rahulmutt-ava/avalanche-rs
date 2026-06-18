// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.13 — the four-way `proto/vm` wire-identity matrix
//! (specs/07 §10 four-way matrix; specs/02 §6 golden, §11.3).
//!
//! The matrix drives an identical block-build/verify/accept/parse sequence
//! through all four host⇄guest pairings — Rust⇄Rust, Rust-host⇄Go-guest,
//! Go-host⇄Rust-guest, Go⇄Go — and asserts the **same** block bytes / IDs /
//! last-accepted **and** the same `proto/vm` request wire bytes across every
//! pairing, diffing against committed goldens.
//!
//! ## Golden location & the `ava-differential` ↔ `ava-vm-rpc` independence
//! `ava-differential` deliberately does **not** depend on `ava-vm-rpc` (the
//! rpcchainvm host/guest crate — a verified design invariant; see the M9.3
//! AS-BUILT note in `plan/M9-interop-hardening.md`). The canonical goldens are
//! therefore produced and locked by `ava-vm-rpc`'s own test
//! (`crates/ava-vm-rpc/tests/wire_identity.rs`, the Rust⇄Rust CI-runnable arm)
//! and committed under `crates/ava-vm-rpc/tests/vectors/rpcchainvm/`. This crate
//! reads those **byte files** directly (no `ava-vm-rpc` import), via a path
//! relative to its own manifest dir. One canonical copy, two readers.
//!
//! Two arms, following the established M9.3/M9.12 cadence:
//!
//! 1. **Offline arm** ([`plugin_wire_identity_matrix_offline`], runs every CI
//!    run, no feature, not ignored): reads the committed goldens and asserts the
//!    matrix invariants that hold without any Go binary — the goldens exist, are
//!    deterministic (re-reads byte-identical), and are internally consistent
//!    (`block1_id == sha256(block1_bytes)`, the cross-language block-identity
//!    oracle). Only the Rust⇄Rust pairing runs in CI, so its goldens ARE the
//!    shared reference the other three pairings diff against.
//!
//! 2. **Live arm** ([`plugin_wire_identity_matrix`], behind the `live` feature +
//!    `#[ignore]`): drives the three Go-involving pairings against a real Go
//!    `avalanchego` and diffs their captured `proto/vm` request bytes against the
//!    same goldens. Never runs in CI / this sandbox.

#![allow(unused_crate_dependencies)]

use std::path::PathBuf;

/// The canonical golden directory, relative to this crate's manifest dir. The
/// files are owned by `ava-vm-rpc`'s `wire_identity` test; we only read them.
fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates")
        .join("ava-vm-rpc")
        .join("tests")
        .join("vectors")
        .join("rpcchainvm")
}

/// The `proto/vm` request goldens (the wire bytes the host puts on the channel
/// for each RPC in the fixed sequence). Empty files are legitimate: a request
/// whose only fields are proto3 defaults (`BuildBlockRequest{p_chain_height:
/// None}`, `SetStateRequest{state: UNSPECIFIED}`) encodes to zero bytes.
const REQUEST_GOLDENS: &[&str] = &[
    "set_state_unspecified.bin",
    "set_preference.bin",
    "build_block.bin",
    "block_verify.bin",
    "block_accept.bin",
    "parse_block.bin",
];

/// The block-identity goldens (the derived block bytes/ids the matrix asserts
/// identical across all four pairings). These are non-empty 32-byte ids / the
/// 48-byte block-1 encoding.
const IDENTITY_GOLDENS: &[&str] = &["genesis_id.bin", "block1_bytes.bin", "block1_id.bin"];

fn read_golden(name: &str) -> Vec<u8> {
    let path = golden_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "wire-identity golden {} missing ({e}); produce it by running the \
             ava-vm-rpc wire_identity test (REGEN_WIRE_GOLDENS=1 the first time)",
            path.display()
        )
    })
}

/// Offline arm: assert the committed goldens are present, deterministic, and
/// internally consistent. This is the part of the four-way matrix invariant that
/// holds with no Go binary — the Rust⇄Rust pairing's goldens ARE the shared
/// reference, so locking them locks the matrix's CI-runnable corner. Runs every
/// CI run, offline.
#[test]
fn plugin_wire_identity_matrix_offline() {
    // 1. Every golden is present and re-reads byte-identically (determinism — the
    //    same invariant that lets all four pairings share one set of bytes).
    for name in REQUEST_GOLDENS.iter().chain(IDENTITY_GOLDENS) {
        let first = read_golden(name);
        let second = read_golden(name);
        assert_eq!(first, second, "golden {name} is non-deterministic on disk");
    }

    // 2. The identity goldens have the expected fixed widths.
    let genesis_id = read_golden("genesis_id.bin");
    let block1_bytes = read_golden("block1_bytes.bin");
    let block1_id = read_golden("block1_id.bin");
    assert_eq!(genesis_id.len(), 32, "genesis_id is a 32-byte id");
    assert_eq!(block1_id.len(), 32, "block1_id is a 32-byte id");
    // block-1 = parent(32) ++ be64(height) ++ payload(be64) = 48 bytes.
    assert_eq!(block1_bytes.len(), 48, "block1 encoding width");

    // 3. Cross-language block-identity oracle: block1_id == sha256(block1_bytes)
    //    and the genesis parent embedded in block-1 == genesis_id. A Go guest
    //    that diverges on either would be caught by the live arm diffing the same
    //    files; here we verify the goldens themselves are self-consistent.
    let recomputed = ava_crypto::hashing::sha256(&block1_bytes);
    assert_eq!(
        block1_id.as_slice(),
        recomputed.as_slice(),
        "block1_id must be sha256(block1_bytes) — the matrix's id derivation"
    );
    assert_eq!(
        block1_bytes.get(..32),
        Some(genesis_id.as_slice()),
        "block-1's parent prefix must be the genesis id"
    );

    // 4. The request goldens that carry an id embed the right one. BlockAccept
    //    carries block1_id; SetPreference carries genesis_id; both are wrapped in
    //    a single proto field-1 (`bytes id = 1`) → tag 0x0a, len 0x20, then the
    //    32-byte id.
    assert_request_carries_id("block_accept.bin", &block1_id);
    assert_request_carries_id("set_preference.bin", &genesis_id);

    // 5. The two all-default requests encode to empty (proto3 omits zero fields).
    assert!(
        read_golden("build_block.bin").is_empty(),
        "BuildBlockRequest{{p_chain_height: None}} is all-default → empty wire bytes"
    );
    assert!(
        read_golden("set_state_unspecified.bin").is_empty(),
        "SetStateRequest{{UNSPECIFIED}} is all-default → empty wire bytes"
    );
}

/// Assert a `proto/vm` request golden whose single field is `bytes id = 1`
/// encodes exactly `[0x0a, 0x20, <32-byte id>]` (proto field 1, wire-type 2
/// (length-delimited), length 0x20 = 32, then the id).
fn assert_request_carries_id(name: &str, id: &[u8]) {
    let g = read_golden(name);
    let mut expected = Vec::with_capacity(34);
    expected.push(0x0a);
    expected.push(0x20);
    expected.extend_from_slice(id);
    assert_eq!(
        g, expected,
        "{name}: must encode field-1 length-delimited 32-byte id matching the identity golden"
    );
}

/// Live arm: drive the three Go-involving pairings of the four-way matrix
/// (Rust-host⇄Go-guest, Go-host⇄Rust-guest, Go⇄Go) against a real Go
/// `avalanchego`, capture each pairing's `proto/vm` request bytes, and diff them
/// against the committed goldens (same files the offline arm reads). Gated behind
/// the `live` feature + `#[ignore]` so it never runs in CI / this sandbox.
///
/// LIVE-ARM operator requirements (what the nightly job must supply — the
/// launcher cannot wire a full host+plugin+subnet blind, exactly as the M9.3 /
/// M9.12 live arms document):
///   * `$AVALANCHEGO_PATH` → a built Go `avalanchego` (protocol 45) — the Go host
///     for the Rust-host⇄Go-guest and Go⇄Go legs, and the Go guest source.
///   * `$AVALANCHEGO_PLUGIN_PATH` → a Go rpcchainvm plugin (protocol 45) — the Go
///     guest hosted under the Rust `avalanchers` node (Rust-host⇄Go-guest leg).
///   * A built `avalanchers` Rust node (resolved from `target/{debug,release}/`) —
///     the Rust host (hosts the Go guest) and the Rust guest source (a Rust
///     `testvm_plugin` hosted under the Go node, Go-host⇄Rust-guest leg).
///   * A data dir per pairing whose `plugins/` holds the right guest binary
///     renamed to its chain's VM id, plus a genesis/subnet that instantiates a
///     blockchain on that VM (so the host factory spawns the guest with
///     `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`). Pass node flags via
///     `$AVALANCHERS_EXTRA_ARGS` (Rust host) / the Go node's config.
///   * A `proto/vm` request-byte capture shim on each host's outbound channel
///     (e.g. a recording gRPC interceptor / a tcpdump-derived frame extractor),
///     since neither stock binary writes the raw request bytes to disk. The diff
///     target is the committed goldens this file reads — a Go host whose request
///     bytes differ from the Rust⇄Rust goldens is a wire-format divergence and
///     must fail the matrix.
///
/// ## Live status (2026-06-18)
/// The **Go-host⇄Rust-guest lifecycle leg** is now validated live, independently
/// of the full byte-capture matrix below: the env-gated Go harness
/// `tests/differential/go-oracle/rust_plugin_lifecycle/main.go` boots a real Go
/// `avalanchego` node hosting the Rust `testvm_plugin`, lets the chain reach
/// NormalOp, and confirms the Go host drives a full `BuildBlock → VerifyBlock →
/// AcceptBlock` sequence over the live rpcchainvm v45 channel (the Rust guest
/// emits `TESTVM-EVENT build|verify|accept` markers the harness greps from the
/// chain log). This proves the build/verify/accept **traffic** the M9.3
/// handshake-only arm left undriven. What remains gated here is the *byte-identity*
/// assertion across all four pairings, which needs the capture shim below.
///
/// Until the operator supplies the above, this arm panics with the unmet
/// requirement rather than passing vacuously (the M9.3/M9.12 precedent).
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live Go avalanchego ($AVALANCHEGO_PATH) + a Go plugin ($AVALANCHEGO_PLUGIN_PATH) + a built avalanchers + per-pairing data dirs + a proto/vm capture shim — nightly only"]
async fn plugin_wire_identity_matrix() {
    use ava_differential::plugin::{avalanchers_binary_path, go_binary_path, go_plugin_path};

    let Some(go_bin) = go_binary_path() else {
        eprintln!("AVALANCHEGO_PATH unset — skipping live plugin_wire_identity_matrix");
        return;
    };
    let Some(go_plugin) = go_plugin_path() else {
        eprintln!("AVALANCHEGO_PLUGIN_PATH unset — skipping live plugin_wire_identity_matrix");
        return;
    };
    let Some(host_bin) = avalanchers_binary_path() else {
        eprintln!("avalanchers binary not built — skipping live plugin_wire_identity_matrix");
        return;
    };

    // Sanity: the goldens the three Go legs must diff against exist offline.
    for name in REQUEST_GOLDENS.iter().chain(IDENTITY_GOLDENS) {
        let _ = read_golden(name);
    }

    panic!(
        "LIVE-ARM: the four-way wire-identity matrix's three Go legs \
         (Rust-host⇄Go-guest, Go-host⇄Rust-guest, Go⇄Go) require operator-supplied \
         per-pairing data dirs (plugins/<vm-id> + a subnet/chain on that VM) AND a \
         proto/vm request-byte capture shim on each host's outbound channel. Diff the \
         captured bytes against the committed goldens this test reads. \
         go_bin={go_bin:?} go_plugin={go_plugin:?} host_bin={host_bin:?}"
    );
}

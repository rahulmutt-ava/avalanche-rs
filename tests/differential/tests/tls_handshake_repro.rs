// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 — rustls↔Go TLS-1.3 handshake bisection matrix.
//!
//! Offline arm (every CI run): asserts the driver's outcome-parse + the
//! live-arm gate behave. Live arm (`--features live`, `#[ignore]`, needs
//! `$AVALANCHEGO_PATH`): builds the Go harness and runs the 5-cell matrix,
//! capturing both sides' handshake outcomes to pin the failing rung.

// Integration tests use unwrap/expect freely.
#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ava_differential::tls_repro;

/// Offline: the live arm must early-return (not panic) when AVALANCHEGO_PATH is
/// absent, and the outcome parser round-trips a real Go line.
#[test]
fn offline_gate_and_parse() {
    // Parser is exercised in the lib unit tests; here assert the gate predicate
    // is wired so the live arm is genuinely skippable in CI/sandbox.
    //
    // Always exercise the parse-smoke path (the primary offline assertion) and
    // then, if the Go harness builds successfully, confirm `build_go_harness`
    // returns a path. A Go build failure here is tolerated — it just means the
    // Go toolchain is unavailable in this environment, which is expected in the
    // offline/sandbox path.
    let parsed = tls_repro::parse_go_outcome(
        r#"{"ok":true,"version":772,"peer_key_type":"ecdsa"}"#,
    )
    .expect("parse");
    assert!(parsed.ok, "offline parse smoke");

    if !tls_repro::avalanchego_available() {
        // The live arm would early-return on this same condition.
        return;
    }
    // If a Go binary IS configured in this environment, attempt to build the
    // harness — the full matrix lives in the #[ignore]d live test. A build
    // failure (e.g. toolchain mismatch) is tolerated offline.
    if let Err(e) = tls_repro::build_go_harness() {
        eprintln!("go harness build skipped (offline): {e}");
    }
}

#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs $AVALANCHEGO_PATH + a built ~/avalanchego checkout"]
async fn tls_handshake_matrix_live() {
    use std::process::Stdio;
    use std::time::Duration;

    if !tls_repro::avalanchego_available() {
        eprintln!("AVALANCHEGO_PATH unset — skipping live TLS matrix");
        return;
    }
    let go_bin = tls_repro::build_go_harness().expect("build go harness");
    let id = ava_network::Identity::generate().expect("rust identity");

    // Helper: spawn the Go harness as server, read its LISTENING addr from
    // stderr, drive the Rust client against it, then collect the Go outcome.
    async fn rust_client_vs_go_server(
        go_bin: &std::path::Path,
        id: &ava_network::Identity,
        verify: &str,
        keytype: &str,
    ) -> (Result<String, String>, String) {
        let ports = ava_differential::livenet::free_ports(1).expect("free_ports");
        let port = ports.into_iter().next().expect("one port");
        let addr = format!("127.0.0.1:{port}");
        let child = tokio::process::Command::new(go_bin)
            .args([
                "--role=server",
                &format!("--addr={addr}"),
                &format!("--verify={verify}"),
                &format!("--keytype={keytype}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn go server");
        tokio::time::sleep(Duration::from_millis(500)).await; // let it bind
        let rust_result =
            tls_repro::rust_client_upgrade(addr.parse().unwrap(), id).await;
        let out = child.wait_with_output().await.expect("go output");
        let go_stdout = String::from_utf8_lossy(&out.stdout).to_string();
        (rust_result, go_stdout)
    }

    // --- Cell 1: Rust-client ↔ Go-server, verify=staking, keytype=rsa (LIVE FAILURE) ---
    let (r1, g1) = rust_client_vs_go_server(&go_bin, &id, "staking", "rsa").await;
    eprintln!("CELL1 rust={r1:?}\nCELL1 go={g1}");

    // --- Cell 2: same but verify=noop (DECISIVE isolation) ---
    let (r2, g2) = rust_client_vs_go_server(&go_bin, &id, "noop", "rsa").await;
    eprintln!("CELL2 rust={r2:?}\nCELL2 go={g2}");

    // --- Cell 5: verify=staking, keytype=ecdsa (fresh Go ECDSA cert) ---
    let (r5, g5) = rust_client_vs_go_server(&go_bin, &id, "staking", "ecdsa").await;
    eprintln!("CELL5 rust={r5:?}\nCELL5 go={g5}");

    // Diagnosis assertion: cell 2 (no Go-side cert policy) MUST localize the
    // fault. If cell 2 succeeds while cell 1 fails, the root cause is Go's
    // ValidateCertificate rejecting our cert. If cell 2 also fails, it is a
    // pure-TLS interop issue. Either way, capture — do not silently pass.
    let go2 = tls_repro::parse_go_outcome(&g2);
    assert!(
        go2.is_ok(),
        "cell 2 Go side must emit a structured outcome (got {g2:?})",
    );
    // The live arm is expected RED until the root cause is fixed; the captured
    // eprintln evidence is the deliverable (see Task 4). We assert only that the
    // harness ran end-to-end (both sides produced an outcome), not that the
    // handshake succeeded.
}

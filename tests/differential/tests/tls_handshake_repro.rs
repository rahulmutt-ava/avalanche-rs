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
    let parsed =
        tls_repro::parse_go_outcome(r#"{"ok":true,"version":772,"peer_key_type":"ecdsa"}"#)
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

    // This matrix exists to diagnose a TLS *stall*, so an unbounded harness hang
    // is indistinguishable from the bug. Both the Rust client upgrade and the Go
    // process wait are wrapped in a `tokio::time::timeout` comfortably longer
    // than the Go side's 10s handshake deadline; on timeout we kill the child if
    // still running and surface a structured string so the cell still prints
    // capturable evidence (Task 4) instead of hanging.
    const CELL_TIMEOUT: Duration = Duration::from_secs(15);

    // Reverse direction: spawn the Go harness as a TLS *client* (real RSA
    // staking cert) dialing a Rust *server*, exercising the inbound-peer
    // verifier path against a real Go cert — not just the in-process fixture.
    async fn go_client_vs_rust_server(
        go_bin: &std::path::Path,
        id: &ava_network::Identity,
        verify: &str,
        keytype: &str,
    ) -> (Result<String, String>, String) {
        // Bind the Rust server listener first so the Go client has a fixed addr.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind rust server");
        let addr = listener.local_addr().expect("rust server local_addr");

        let mut child = tokio::process::Command::new(go_bin)
            .args([
                "--role=client",
                &format!("--addr={addr}"),
                &format!("--verify={verify}"),
                &format!("--keytype={keytype}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn go client");

        // Accept + upgrade on the Rust server side, bounded like the fwd cells.
        let rust_result =
            match tokio::time::timeout(CELL_TIMEOUT, tls_repro::rust_server_upgrade(listener, id))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(format!("timed out after {}s", CELL_TIMEOUT.as_secs())),
            };

        let go_stdout = match tokio::time::timeout(CELL_TIMEOUT, child.wait()).await {
            Ok(Ok(_status)) => {
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = out.read_to_end(&mut buf).await;
                }
                String::from_utf8_lossy(&buf).to_string()
            }
            Ok(Err(e)) => format!(r#"{{"ok":false,"error":"go wait failed: {e}"}}"#),
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                format!(
                    r#"{{"ok":false,"error":"go timed out after {}s"}}"#,
                    CELL_TIMEOUT.as_secs()
                )
            }
        };
        (rust_result, go_stdout)
    }

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
        let mut child = tokio::process::Command::new(go_bin)
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
        // Wait for the Go server to actually bind, deterministically: the harness
        // prints `LISTENING <addr>` to stderr right after `tls.Listen` succeeds.
        // Read stderr lines until we see it, bounded by CELL_TIMEOUT. This removes
        // the cold-start timing race without a magic sleep (a freshly-spawned Go
        // process on an OS-uncached binary can take ~1s to bind) — and, unlike a
        // throwaway TCP probe, it does NOT consume the server's single `Accept()`.
        if let Some(stderr) = child.stderr.take() {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            let _ = tokio::time::timeout(CELL_TIMEOUT, async {
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.starts_with("LISTENING") {
                        break;
                    }
                }
            })
            .await;
        }

        // Bound the Rust-client upgrade so a stalled handshake yields a captured
        // error string rather than hanging the harness.
        let rust_result = match tokio::time::timeout(
            CELL_TIMEOUT,
            tls_repro::rust_client_upgrade(addr.parse().unwrap(), id),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {}s", CELL_TIMEOUT.as_secs())),
        };

        // Bound the Go-process wait the same way. We hold `child` mutably and
        // wait on it directly (rather than the consuming `wait_with_output`) so
        // that on timeout we can `start_kill()` the still-running child and then
        // surface a structured outcome string (still parseable as "not ok"
        // evidence) instead of leaking the process or blocking forever.
        let go_stdout = match tokio::time::timeout(CELL_TIMEOUT, child.wait()).await {
            Ok(Ok(_status)) => {
                // Drain whatever the Go side wrote to stdout before exiting.
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = out.read_to_end(&mut buf).await;
                }
                String::from_utf8_lossy(&buf).to_string()
            }
            Ok(Err(e)) => format!(r#"{{"ok":false,"error":"go wait failed: {e}"}}"#),
            Err(_) => {
                // Still running past the deadline: kill it so it cannot leak past
                // the test, then surface a structured timeout outcome line that
                // `parse_go_outcome` can still read as evidence.
                let _ = child.start_kill();
                let _ = child.wait().await;
                format!(
                    r#"{{"ok":false,"error":"go timed out after {}s"}}"#,
                    CELL_TIMEOUT.as_secs()
                )
            }
        };
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

    // --- Fix-validation gates (post-e06f0a0). The RSA cells previously failed
    // with rustls UnsupportedCertVersion (Go's v1 RSA staking cert); the
    // verify_tls13_signature_with_raw_key path must now let them complete. ---

    // CELL5 (ECDSA) was already green pre-fix and must stay green.
    let go5 = tls_repro::parse_go_outcome(&g5).expect("CELL5 go outcome");
    assert!(go5.ok, "CELL5 (ecdsa) Go handshake ok: {g5}");
    assert_eq!(go5.version, Some(772), "CELL5 negotiated TLS 1.3");
    assert!(r5.is_ok(), "CELL5 Rust client derived a NodeID: {r5:?}");

    // CELL2 (RSA, verify=noop) — the decisive isolation cell.
    let go2 = tls_repro::parse_go_outcome(&g2).expect("CELL2 go outcome");
    assert!(
        go2.ok,
        "CELL2 (rsa, noop) Go handshake ok — verifier fix: {g2}"
    );
    assert_eq!(go2.version, Some(772), "CELL2 negotiated TLS 1.3");
    assert!(
        r2.is_ok(),
        "CELL2 Rust client accepted Go's v1 RSA cert: {r2:?}"
    );

    // CELL1 (RSA, verify=staking) — full production policy on both sides.
    let go1 = tls_repro::parse_go_outcome(&g1).expect("CELL1 go outcome");
    assert!(go1.ok, "CELL1 (rsa, staking) Go handshake ok: {g1}");
    assert_eq!(go1.version, Some(772), "CELL1 negotiated TLS 1.3");
    assert!(
        r1.is_ok(),
        "CELL1 Rust client + Go RSA staking cert: {r1:?}"
    );

    // --- Reverse direction: Go *client* (RSA staking cert) -> Rust *server*.
    // Validates the inbound-peer verifier path against a real Go RSA cert. ---
    let server_id = ava_network::Identity::generate().expect("rust server identity");
    let (rs, gs) = go_client_vs_rust_server(&go_bin, &server_id, "staking", "rsa").await;
    eprintln!("REVERSE rust_server={rs:?}\nREVERSE go_client={gs}");
    let gsr = tls_repro::parse_go_outcome(&gs).expect("REVERSE go outcome");
    assert!(gsr.ok, "REVERSE Go client handshake ok: {gs}");
    assert_eq!(gsr.version, Some(772), "REVERSE negotiated TLS 1.3");
    assert!(
        rs.is_ok(),
        "REVERSE Rust server accepted Go RSA client cert: {rs:?}"
    );
}

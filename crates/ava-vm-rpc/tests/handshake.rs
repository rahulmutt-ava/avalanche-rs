// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! rpcchainvm v45 reverse-dial handshake + the in-process leg of the four-way
//! interop matrix (specs 07 §5.1, §10; plan M3.24).
//!
//! * `handshake_protocol_mismatch` — a guest reporting protocol≠45 yields
//!   [`ava_vm::Error::ProtocolVersionMismatch`].
//! * `handshake_timeout` — no `Runtime.Initialize` within
//!   [`ava_vm_rpc::DEFAULT_HANDSHAKE_TIMEOUT`] yields
//!   [`ava_vm::Error::HandshakeFailed`].
//! * `rust_host_rust_guest_roundtrip` — a Rust host hosting a Rust guest
//!   wrapping the in-memory [`ava_vm::testutil::TestVm`] drives
//!   build→verify→accept identically.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use ava_vm::Error;
use ava_vm::block::ChainVm;
use ava_vm::testutil::{TestVm, init_test_vm};
use ava_vm_rpc::host::RpcChainVm;
use ava_vm_rpc::{RPC_CHAIN_VM_PROTOCOL, guest};

// Pulled in by `tonic-build`/`tonic` transitively; referenced so the test
// binary's `unused_crate_dependencies` lint stays quiet.
use {tokio_stream as _, tonic as _};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_protocol_mismatch() {
    let token = CancellationToken::new();
    // A launcher that dials the host runtime and reports a wrong protocol
    // version. `RpcChainVm::start` must surface `ProtocolVersionMismatch`.
    let res = RpcChainVm::start(&token, DEFAULT_TIMEOUT(), |engine_addr| {
        let engine_addr = engine_addr.to_string();
        tokio::spawn(async move {
            // Bind a throwaway VM listener so we have an addr to report.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let v_addr = listener.local_addr().unwrap();
            let _ = guest::report_handshake(
                &engine_addr,
                RPC_CHAIN_VM_PROTOCOL + 1, // WRONG version
                &v_addr.to_string(),
            )
            .await;
        });
    })
    .await;
    let res = res.map(|_| ()); // RpcChainVm isn't Debug; collapse the Ok arm.
    assert!(
        matches!(res, Err(Error::ProtocolVersionMismatch)),
        "expected ProtocolVersionMismatch, got {res:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_timeout() {
    let token = CancellationToken::new();
    // A launcher that never calls Initialize. The host must time out with
    // `HandshakeFailed`.
    let res = RpcChainVm::start(&token, Duration::from_millis(150), |_engine_addr| {
        // Do nothing — never complete the handshake.
    })
    .await;
    let res = res.map(|_| ()); // RpcChainVm isn't Debug; collapse the Ok arm.
    assert!(
        matches!(res, Err(Error::HandshakeFailed)),
        "expected HandshakeFailed, got {res:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rust_host_rust_guest_roundtrip() {
    let token = CancellationToken::new();

    // The launcher spins up an in-process Rust guest wrapping a fresh TestVm.
    let host = RpcChainVm::start(&token, DEFAULT_TIMEOUT(), |engine_addr| {
        let engine_addr = engine_addr.to_string();
        let token = CancellationToken::new();
        tokio::spawn(async move {
            let vm: TestVm = init_test_vm(&token).await.expect("init test vm");
            // serve_with_addr does the guest side of the handshake (bind V,
            // dial R, Initialize(45, V.addr)) then serves the VM service.
            guest::serve_with_addr(vm, &engine_addr, &token)
                .await
                .expect("guest serve");
        });
    })
    .await
    .expect("handshake + dial VM");

    // Drive build -> verify -> accept through the host (which is itself a
    // ChainVm translating each call to a proto/vm RPC).
    let mut host = host;
    let genesis = host.last_accepted(&token).await.expect("last_accepted");
    let blk = host.build_block(&token).await.expect("build_block");
    assert_eq!(blk.parent(), genesis, "built on the last accepted block");
    assert_eq!(blk.height(), 1, "child of genesis is at height 1");

    blk.verify(&token).await.expect("verify");
    blk.accept(&token).await.expect("accept");

    let last = host.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(
        last,
        blk.id(),
        "accept advances last_accepted across the wire"
    );

    // parse_block round-trips the bytes.
    let parsed = host
        .parse_block(&token, blk.bytes())
        .await
        .expect("parse_block");
    assert_eq!(
        parsed.id(),
        blk.id(),
        "parse round-trips the id over the wire"
    );
}

#[allow(non_snake_case)]
fn DEFAULT_TIMEOUT() -> Duration {
    ava_vm_rpc::DEFAULT_HANDSHAKE_TIMEOUT
}

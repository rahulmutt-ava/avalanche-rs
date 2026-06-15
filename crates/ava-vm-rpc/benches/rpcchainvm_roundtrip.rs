// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path rpcchainvm bench (specs/02 §9; plan M9.21 follow-up): a single
//! proxied gRPC round-trip across the in-process loopback rpcchainvm boundary.
//!
//! The node serves a `memdb` over `proto/rpcdb` on an ephemeral loopback port
//! (the simplest of the M3.25 callback proxies); the plugin dials the
//! guest-side `RpcDatabase` (a synchronous [`ava_database::DynDatabase`] that
//! owns its own runtime and `block_on`s each RPC; specs 04 §1.2). The timed loop
//! does ONE `get(key)` — the proxied gRPC round-trip hot path.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a variance-prone perf-gate canary, not a precise
//! micro-benchmark.

#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use tokio_util::sync::CancellationToken;

use ava_database::{DynDatabase, MemDb};
use ava_vm_rpc::proxy;

fn bench(c: &mut Criterion) {
    // The host server runs on a multi-thread runtime that stays alive for the
    // whole bench; it is built ONCE outside the timed loop.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let shutdown = CancellationToken::new();

    // Stand up the loopback rpcdb server ONCE: serve a memdb pre-seeded with the
    // key the bench reads, over proto/rpcdb on an ephemeral loopback port.
    let addr = {
        let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        db.put(b"bench-key", b"bench-value").expect("seed memdb");
        let server = proxy::rpcdb::serve(db).into_service();
        let s2 = shutdown.clone();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let addr = listener.local_addr().expect("local_addr").to_string();
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
            tokio::spawn(async move {
                let _ = tonic::transport::Server::builder()
                    .add_service(server)
                    .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
                    .await;
            });
            addr
        })
    };

    // Guest: dial and build the synchronous RpcDatabase client ONCE. It owns its
    // own current-thread runtime and `block_on`s each RPC, so it is driven
    // directly from this (non-async) bench thread — exactly as a VM consuming the
    // `DynDatabase` would call it.
    let client = proxy::rpcdb::dial(&addr).expect("dial rpcdb");

    let key = b"bench-key".to_vec();
    c.bench_function("rpcchainvm_roundtrip", |b| {
        b.iter(|| {
            // ONE proxied gRPC round-trip across the loopback rpcchainvm
            // boundary: a single `get` on the guest-side client.
            let value = client.get(black_box(&key)).expect("proxied get");
            black_box(value);
        });
    });

    shutdown.cancel();
    drop(client);
    rt.shutdown_background();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_millis(500))
        .warm_up_time(Duration::from_millis(200));
    targets = bench
}
criterion_main!(benches);

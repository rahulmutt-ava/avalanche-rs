// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Critical-path message-framing bench (specs/02 §9): marshal a representative
//! P2P wire message to bytes (the outbound frame) and unmarshal it back (the
//! inbound parse). This is the "framing" hot path every peer hits on every
//! message — `MsgBuilder::marshal` → `MsgBuilder::unmarshal`.
//!
//! Configured for SHORT runs so `cargo xtask bench-guard` finishes well under a
//! minute; this is a perf-gate canary, not a precise micro-benchmark.

// A criterion bench is a standalone binary target: it pulls in only `criterion`
// (+ the crate under test), so the lib's `unused_crate_dependencies` lint fires
// for every *other* dev-dep, and `criterion_group!`/`criterion_main!` expand to
// undocumented items. Scope the allows to this bench target.
#![allow(unused_crate_dependencies, missing_docs)]

use std::time::Duration;

use bytes::Bytes;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

use ava_message::codec::{Compression, MsgBuilder};
use ava_message::proto::p2p;

/// Marshal `m` to wire bytes (uncompressed framing) then unmarshal it back,
/// mirroring the outbound→inbound round-trip a peer performs per message.
fn framing(mb: &MsgBuilder, m: &p2p::Message) {
    if let Ok((bytes, _saved, _op)) = mb.marshal(m, Compression::None)
        && let Ok((msg, _saved, _op)) = mb.unmarshal(&bytes)
    {
        let _ = black_box(msg);
    }
}

fn bench(c: &mut Criterion) {
    let mb = MsgBuilder::default();
    // A representative non-trivial consensus message: `Get` carries a 32-byte
    // chain id, a request id, a deadline, and a 32-byte container id (matches
    // the crate's own `marshal_unmarshal_get_preserves_deadline` test).
    let m = p2p::Message {
        message: Some(p2p::message::Message::Get(p2p::Get {
            chain_id: Bytes::from(vec![1u8; 32]),
            request_id: 3,
            deadline: 1_000_000_000,
            container_id: Bytes::from(vec![2u8; 32]),
        })),
    };
    c.bench_function("message_framing", |b| {
        b.iter(|| framing(black_box(&mb), black_box(&m)));
    });
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

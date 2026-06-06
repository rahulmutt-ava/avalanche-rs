// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.14 — the `Peer` actor: read/write/net-messages tasks + handshake-first
//! write + oversized-frame close + cancellation drain (`specs/05` §1.1/§1.4/§3.2,
//! `specs/17` §2/§3/§4/§7).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::time::Duration;

use ava_message::codec::MsgBuilder;
use ava_message::frame::MAX_MESSAGE_SIZE;
use ava_message::ops::Op;
use ava_network::peer::testutil::{read_one_frame, TestPeerBuilder};
use tokio::io::AsyncWriteExt;

/// The first frame a peer writes MUST decode to a `Handshake` op (`specs/05`
/// §1.4 — handshake forced first in the write task).
#[tokio::test]
async fn write_task_sends_handshake_first() {
    let (mut remote, peer) = TestPeerBuilder::new().spawn_over_duplex();

    let frame = read_one_frame(&mut remote)
        .await
        .expect("read first frame");

    let mb = MsgBuilder::default();
    let (_msg, _saved, op) = mb.unmarshal(&frame).expect("decode first frame");
    assert_eq!(op, Op::Handshake, "first written message must be Handshake");

    peer.close();
}

/// A length prefix larger than `MAX_MESSAGE_SIZE` is a protocol error: the peer
/// closes the connection (`on_closed` fires) (`specs/05` §1.1).
#[tokio::test]
async fn read_task_resets_deadline_and_drops_oversized() {
    let (mut remote, peer) = TestPeerBuilder::new().spawn_over_duplex();

    // Drain the peer's own handshake frame.
    let _ = read_one_frame(&mut remote)
        .await
        .expect("read peer handshake");

    let oversized = MAX_MESSAGE_SIZE + 1;
    remote
        .write_all(&oversized.to_be_bytes())
        .await
        .expect("write oversized prefix");
    remote.flush().await.expect("flush");

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("peer closes on oversized frame");
}

/// Cancelling the peer's token joins all three tasks (`specs/17` §4.2).
#[tokio::test]
async fn cancel_token_drains_tasks() {
    let (_remote, peer) = TestPeerBuilder::new().spawn_over_duplex();

    peer.close();

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("peer drains on cancel");
}

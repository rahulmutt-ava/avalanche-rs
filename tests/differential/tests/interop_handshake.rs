// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.22 — `differential::interop_handshake` (specs/05 §9.9, specs/26 §9.4,
//! specs/02 §9).
//!
//! Proves wire interop between the Rust `ava-network` peer actor + `ava-message`
//! codec and a Go node, in two arms:
//!
//! 1. **Live arm** (`differential_interop_handshake_live`, behind the `interop`
//!    Cargo feature + `#[ignore]`): a Rust [`NetworkImpl`] dials a real Go Fuji
//!    node at `$AVA_INTEROP_FUJI_ADDR`, completes the TLS 1.3 + p2p Handshake +
//!    PeerList exchange, and holds the connection `≥ $AVA_INTEROP_HOLD_SECS`
//!    (default 30s) with no disconnect. Never runs in CI / this sandbox (no live
//!    peer, no env); a scheduled/nightly job runs it.
//!
//! 2. **Recorded fallback arm** (`differential_interop_handshake`, runs every CI
//!    run, offline): replays a **Go-derived** handshake transcript
//!    (`fixtures/fuji_transcript.bin`) through the real Rust codec + peer actor
//!    and asserts the Rust side reaches `connected` (latches
//!    `finished_handshake`, fires `ExternalHandler::connected` exactly once) with
//!    no protocol error.
//!
//! ## Transcript provenance
//! `fixtures/fuji_transcript.bin` was emitted by a scratch Go program run against
//! the pinned avalanchego tree (commit `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`,
//! Go 1.25.9) that:
//!
//! - generates a staking cert+key via `staking.NewTLSCert()`;
//! - signs the loopback IP `127.0.0.1:9651 @ ts=1_700_000_000` with the staking
//!   TLS key (ECDSA-P256/SHA-256 over `SHA256(ip.As16()||port||ts)`) + a BLS PoP
//!   (`network/peer.UnsignedIP.Sign`);
//! - builds a byte-exact `Handshake` (network_id=1, version avalanchego/1.14.2,
//!   `compression.TypeNone`) and an empty `PeerList` via `message.Creator`;
//! - dumps `u32 cert_der_len | cert_der | u32 hs_frame_len | hs_frame |
//!   u32 pl_frame_len | pl_frame`, where each `*_frame` is `len_be_u32 ||
//!   proto_bytes` (the on-wire framing, specs/05 §1.1).
//!
//! The scratch program was deleted after capture. The committed `Handshake` /
//! `PeerList` proto bytes are the same byte-exact Go-derived form already golden
//! in `tests/vectors/message/{handshake,peerlist}.json`; the only delta is the
//! *real* signature/cert pair (the goldens use dummy sig bytes), which is what
//! lets the Rust peer's signed-IP verification pass and reach `connected`. A
//! true live-captured Fuji transcript is the gated/nightly follow-up (the live
//! arm here + cross-cutting tasks X.15/X.22).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::time::Duration;

use ava_message::codec::MsgBuilder;
use ava_message::ops::Op;
use ava_network::peer::peer::{Direction, Peer};
use ava_network::peer::testutil::{TestPeerBuilder, read_one_frame, write_one_frame};
use ava_network::peer::upgrader::node_id_from_cert_der;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// A decoded transcript: the peer's leaf cert DER plus the framed Handshake and
/// PeerList the Go peer sends during the handshake.
struct Transcript {
    cert_der: Vec<u8>,
    handshake_frame: Vec<u8>,
    peer_list_frame: Vec<u8>,
}

/// Parse the `u32-len || bytes` chunked transcript layout.
fn parse_transcript(bytes: &[u8]) -> Transcript {
    let mut off = 0usize;
    let mut next = || {
        assert!(off + 4 <= bytes.len(), "truncated transcript length prefix");
        let len = u32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
            as usize;
        off += 4;
        assert!(off + len <= bytes.len(), "truncated transcript chunk");
        let chunk = bytes[off..off + len].to_vec();
        off += len;
        chunk
    };
    let cert_der = next();
    let handshake_frame = next();
    let peer_list_frame = next();
    assert_eq!(off, bytes.len(), "trailing bytes in transcript");
    Transcript {
        cert_der,
        handshake_frame,
        peer_list_frame,
    }
}

/// Recorded fallback arm: replay the Go-derived transcript through the real Rust
/// peer actor + codec and assert the Rust side reaches `connected` with no
/// protocol error. Runs every CI run, offline.
#[tokio::test]
async fn differential_interop_handshake() {
    let raw = include_bytes!("fixtures/fuji_transcript.bin");
    let t = parse_transcript(raw);

    // Sanity: the recorded frames are byte-exact Go p2p messages.
    let mb = MsgBuilder::default();
    let (_m, _s, op) = mb
        .unmarshal(&t.handshake_frame[4..])
        .expect("decode handshake");
    assert_eq!(op, Op::Handshake, "recorded frame 1 is a Handshake");
    let (_m, _s, op) = mb
        .unmarshal(&t.peer_list_frame[4..])
        .expect("decode peerlist");
    assert_eq!(op, Op::PeerList, "recorded frame 2 is a PeerList");

    // Build the peer-under-test config. The transcript was emitted with
    // network_id=1, my_time=1_700_000_000, version avalanchego/1.14.2 — exactly
    // the `TestPeerBuilder` interop defaults — so the handshake validates.
    let builder = TestPeerBuilder::new();
    let cfg = builder.build_config();
    let router = builder.router();

    // The Go peer's cert is what the Rust peer verifies the signed IP against.
    let peer_cert = ava_crypto::staking::parse_certificate(&t.cert_der).expect("parse peer cert");
    let peer_id = node_id_from_cert_der(&t.cert_der).expect("node id from cert");

    let (local, mut remote) = tokio::io::duplex(1 << 20);
    let net_token = CancellationToken::new();
    let tracker = TaskTracker::new();
    let peer = Peer::spawn(
        cfg,
        peer_id,
        peer_cert,
        Direction::Inbound,
        local,
        &net_token,
        &tracker,
    );
    tracker.close();

    // The Rust peer writes its own Handshake first (forced first action,
    // specs/05 §1.4) — drain it.
    let frame = read_one_frame(&mut remote).await.expect("rust handshake");
    let (_m, _s, op) = mb.unmarshal(&frame).expect("decode rust handshake");
    assert_eq!(op, Op::Handshake, "rust peer sends Handshake first");

    // Replay the Go peer's Handshake (frame already includes the length prefix).
    write_one_frame(&mut remote, &t.handshake_frame[4..])
        .await
        .expect("replay go handshake");

    // The Rust peer must reply with its PeerList.
    let reply = read_one_frame(&mut remote).await.expect("rust reply");
    let (_m, _s, op) = mb.unmarshal(&reply).expect("decode rust reply");
    assert_eq!(
        op,
        Op::PeerList,
        "rust peer replies PeerList to the Handshake"
    );

    // Replay the Go peer's PeerList — this finishes the handshake.
    write_one_frame(&mut remote, &t.peer_list_frame[4..])
        .await
        .expect("replay go peerlist");

    tokio::time::timeout(Duration::from_secs(5), peer.finished_handshake())
        .await
        .expect("rust peer reaches `connected` (no protocol error)");

    assert_eq!(
        router.connected_count(),
        1,
        "ExternalHandler::connected fired exactly once"
    );
    assert!(
        !peer.closed_now(),
        "the connection is held (no disconnect) after the handshake"
    );

    peer.close();
}

/// Live arm: dial a real Go Fuji node and hold the connection. Gated behind the
/// `interop` feature + `#[ignore]` so it never runs in CI / this sandbox; a
/// scheduled/nightly job (or `AVA_INTEROP_FUJI_ADDR=<peer> cargo nextest run
/// --features interop -- --ignored`) runs it (specs/02 §9, specs/26 §9.4).
#[cfg(feature = "interop")]
#[tokio::test]
#[ignore = "requires a live Go Fuji peer ($AVA_INTEROP_FUJI_ADDR) — nightly only"]
async fn differential_interop_handshake_live() {
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::Arc;

    use ava_network::network::Network;
    use ava_network::network::testutil::TestNetwork;
    use ava_types::node_id::NodeId;

    let addr_str = std::env::var("AVA_INTEROP_FUJI_ADDR")
        .expect("AVA_INTEROP_FUJI_ADDR must be set for the live arm");
    let peer_addr = SocketAddr::from_str(&addr_str).expect("AVA_INTEROP_FUJI_ADDR is an ip:port");
    let peer_node_id = std::env::var("AVA_INTEROP_FUJI_NODE_ID")
        .ok()
        .and_then(|s| NodeId::from_str(&s).ok())
        .unwrap_or_default();
    let hold_secs: u64 = std::env::var("AVA_INTEROP_HOLD_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    // Bring up a Rust node and pin the live Go peer.
    let net = TestNetwork::start().await;
    net.network().manually_track(peer_node_id, peer_addr);

    let dispatch = {
        let net = Arc::clone(net.network());
        tokio::spawn(async move { net.dispatch().await })
    };

    // Dial + handshake + hold for `hold_secs` with no disconnect.
    let connected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if net.network().connected_peers().contains(&peer_node_id) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        connected,
        "Rust node should complete the handshake with the Go peer"
    );

    tokio::time::sleep(Duration::from_secs(hold_secs)).await;
    assert!(
        net.network().connected_peers().contains(&peer_node_id),
        "connection held for {hold_secs}s with no disconnect"
    );

    net.network().start_close();
    let _ = tokio::time::timeout(Duration::from_secs(10), dispatch).await;
}

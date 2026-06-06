// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.21 — `prop::handshake_reaches_connected`: property test over the peer
//! handshake state machine (`specs/05` §1.4, §9 item 5; `specs/02` §4).
//!
//! Two invariants, each over a `proptest!`-generated strategy:
//!
//!  * **Valid arm** — for any arbitrary-but-valid handshake parameters
//!    (network_id matching the peer-under-test, clock within ±60s, compatible
//!    version, ≤16 tracked subnets, disjoint supported/objected ACPs, bloom salt
//!    ≤32 bytes, valid IP signature) the in-process duplex exchange always
//!    reaches `finished_handshake` and `ExternalHandler::connected` fires
//!    exactly once on the peer-under-test.
//!  * **Violation arm** — for any single `specs/05` §1.4 violation injected
//!    (wrong network_id, clock skew >60s, incompatible version, >16 subnets,
//!    overlapping supported/objected ACPs, zero port, corrupt IP signature,
//!    bloom salt >32 bytes) the connection always closes before `connected`
//!    fires, i.e. the connected count stays 0.
//!
//! Per `specs/02` §4.1 the proptest failure-persistence corpus
//! (`proptest-regressions/`) is committed; each case drives a per-case tokio
//! runtime (`specs/17` §1.1 permits `tokio::runtime::Runtime` in tests).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::time::Duration;

use ava_message::ops::Op;
use ava_network::peer::testutil::{
    HandshakeOverrides, PeerHarness, read_one_frame, write_one_frame,
};
use ava_version::Application;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

/// The harness's fixed clock (Unix seconds). Mirrors `TestClock::new` in
/// `testutil.rs` and the default `my_time` the harness signs into a handshake.
const BASE_TIME: u64 = 1_700_000_000;

/// A fully-valid override set the peer-under-test must accept.
#[derive(Debug, Clone)]
struct ValidParams {
    /// `my_time`, within ±60s of the harness clock.
    time_skew: i64,
    /// Compatible version patch (major=1, minor=14, patch ≥ 0 ⇒ ≥ floor 1.14.0).
    version_patch: u32,
    /// Compatible version minor (≥ 14 ⇒ ≥ floor 1.14.0, with major=1).
    version_minor: u32,
    /// ≤16 tracked subnets.
    num_tracked_subnets: usize,
    /// Disjoint supported/objected ACP sets.
    supported_acps: Vec<u32>,
    objected_acps: Vec<u32>,
    /// Bloom salt ≤32 bytes.
    bloom_salt_len: usize,
}

impl ValidParams {
    fn into_overrides(self) -> HandshakeOverrides {
        let my_time = i64::try_from(BASE_TIME)
            .unwrap()
            .saturating_add(self.time_skew);
        let my_time = u64::try_from(my_time).unwrap();
        HandshakeOverrides {
            network_id: None, // matches the peer-under-test
            my_time: Some(my_time),
            version: Some(Application::new(
                "avalanchego",
                1,
                self.version_minor,
                self.version_patch,
            )),
            port: None, // valid non-zero default
            num_tracked_subnets: Some(self.num_tracked_subnets),
            supported_acps: Some(self.supported_acps),
            objected_acps: Some(self.objected_acps),
            bloom_salt_len: Some(self.bloom_salt_len),
            corrupt_ip_sig: false,
        }
    }
}

/// Strategy producing arbitrary-but-valid handshake parameters.
///
/// The supported/objected ACP sets are produced disjoint by construction: a
/// shared pool of candidate ids is partitioned by a per-id boolean.
fn valid_params() -> impl Strategy<Value = ValidParams> {
    let acps = proptest::collection::vec((0u32..64, any::<bool>()), 0..8);
    (
        -60i64..=60,
        0u32..=8,
        14u32..=14,
        0usize..=16,
        acps,
        0usize..=32,
    )
        .prop_map(
            |(
                time_skew,
                version_patch,
                version_minor,
                num_tracked_subnets,
                acps,
                bloom_salt_len,
            )| {
                let mut supported = Vec::new();
                let mut objected = Vec::new();
                let mut seen = Vec::new();
                for (id, is_supported) in acps {
                    // Dedup + disjoint: assign each id to exactly one set, and
                    // never the same id to both (the first occurrence wins).
                    if seen.contains(&id) {
                        continue;
                    }
                    seen.push(id);
                    if is_supported {
                        supported.push(id);
                    } else {
                        objected.push(id);
                    }
                }
                supported.sort_unstable();
                objected.sort_unstable();
                ValidParams {
                    time_skew,
                    version_patch,
                    version_minor,
                    num_tracked_subnets,
                    supported_acps: supported,
                    objected_acps: objected,
                    bloom_salt_len,
                }
            },
        )
}

/// The set of single `specs/05` §1.4 violations one of which is injected.
#[derive(Debug, Clone)]
enum Violation {
    WrongNetworkId(u32),
    ClockSkew(i64),
    IncompatibleVersion(Application),
    TooManySubnets(usize),
    AcpOverlap,
    ZeroPort,
    CorruptIpSig,
    BloomSaltTooLong(usize),
}

impl Violation {
    fn into_overrides(self) -> HandshakeOverrides {
        let mut o = HandshakeOverrides::default();
        match self {
            Violation::WrongNetworkId(id) => o.network_id = Some(id),
            Violation::ClockSkew(skew) => {
                let t = i64::try_from(BASE_TIME).unwrap().saturating_add(skew);
                o.my_time = Some(u64::try_from(t.max(0)).unwrap());
            }
            Violation::IncompatibleVersion(v) => o.version = Some(v),
            Violation::TooManySubnets(n) => o.num_tracked_subnets = Some(n),
            Violation::AcpOverlap => {
                // A shared id appears in both sets.
                o.supported_acps = Some(vec![1, 2, 7]);
                o.objected_acps = Some(vec![7, 9]);
            }
            Violation::ZeroPort => o.port = Some(0),
            Violation::CorruptIpSig => o.corrupt_ip_sig = true,
            Violation::BloomSaltTooLong(n) => o.bloom_salt_len = Some(n),
        }
        o
    }
}

/// Strategy producing exactly one §1.4 violation.
fn one_violation() -> impl Strategy<Value = Violation> {
    prop_oneof![
        // network_id != peer-under-test's (which is the default 1).
        (2u32..=1_000_000).prop_map(Violation::WrongNetworkId),
        // |skew| > 60s, either sign.
        prop_oneof![61i64..=86_400, -86_400i64..=-61].prop_map(Violation::ClockSkew),
        // Below the 1.14.0 floor (minor<14) or a newer major (>1).
        prop_oneof![
            (0u32..=13, 0u32..=20).prop_map(|(minor, patch)| Application::new(
                "avalanchego",
                1,
                minor,
                patch
            )),
            (2u32..=5, 0u32..=20, 0u32..=20).prop_map(|(major, minor, patch)| Application::new(
                "avalanchego",
                major,
                minor,
                patch
            )),
        ]
        .prop_map(Violation::IncompatibleVersion),
        // > 16 tracked subnets.
        (17usize..=64).prop_map(Violation::TooManySubnets),
        Just(Violation::AcpOverlap),
        Just(Violation::ZeroPort),
        Just(Violation::CorruptIpSig),
        // > 32-byte bloom salt.
        (33usize..=128).prop_map(Violation::BloomSaltTooLong),
    ]
}

/// Drive one valid in-process handshake to completion; assert `connected`
/// fires exactly once on the peer-under-test.
async fn run_valid(params: ValidParams) -> Result<(), TestCaseError> {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();

    // The peer-under-test sends its own Handshake first.
    let frame = read_one_frame(&mut remote)
        .await
        .map_err(|e| TestCaseError::fail(format!("peer handshake read: {e}")))?;
    let (_m, _s, op) = ava_message::codec::MsgBuilder::default()
        .unmarshal(&frame)
        .map_err(|e| TestCaseError::fail(format!("decode peer handshake: {e}")))?;
    prop_assert_eq!(op, Op::Handshake);

    // Our valid Handshake.
    let hs = h.build_handshake(params.into_overrides());
    write_one_frame(&mut remote, &hs)
        .await
        .map_err(|e| TestCaseError::fail(format!("send handshake: {e}")))?;

    // The peer replies PeerList.
    let reply = read_one_frame(&mut remote)
        .await
        .map_err(|e| TestCaseError::fail(format!("peer reply read: {e}")))?;
    let (_m, _s, op) = ava_message::codec::MsgBuilder::default()
        .unmarshal(&reply)
        .map_err(|e| TestCaseError::fail(format!("decode peer reply: {e}")))?;
    prop_assert_eq!(op, Op::PeerList);

    // Our PeerList finishes the handshake.
    let pl = h.build_peer_list();
    write_one_frame(&mut remote, &pl)
        .await
        .map_err(|e| TestCaseError::fail(format!("send peerlist: {e}")))?;

    tokio::time::timeout(Duration::from_secs(10), peer.finished_handshake())
        .await
        .map_err(|_| TestCaseError::fail("handshake did not finish in time"))?;

    prop_assert_eq!(h.router().connected_count(), 1);

    peer.close();
    Ok(())
}

/// Drive one single-violation handshake; assert the connection closes and
/// `connected` never fires.
async fn run_violation(violation: Violation) -> Result<(), TestCaseError> {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();

    // Drain the peer's own handshake.
    let _ = read_one_frame(&mut remote)
        .await
        .map_err(|e| TestCaseError::fail(format!("peer handshake read: {e}")))?;

    let hs = h.build_handshake(violation.into_overrides());
    // The write may race the peer's close; either outcome is fine.
    let _ = write_one_frame(&mut remote, &hs).await;

    tokio::time::timeout(Duration::from_secs(10), peer.closed())
        .await
        .map_err(|_| TestCaseError::fail("peer did not close on violation"))?;

    prop_assert_eq!(h.router().connected_count(), 0);
    Ok(())
}

/// A per-case current-thread tokio runtime. `specs/17` §1.1 permits constructing
/// a `tokio::runtime::Runtime` in tests; a fresh runtime per case keeps the
/// duplex peer actors fully isolated.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build per-case tokio runtime")
        .block_on(fut)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: Some(Box::new(FileFailurePersistence::SourceParallel(
            "proptest-regressions",
        ))),
        ..ProptestConfig::default()
    })]

    /// Valid handshakes always connect once; single §1.4 violations never do.
    #[test]
    fn handshake_reaches_connected(
        valid in valid_params(),
        violation in one_violation(),
    ) {
        block_on(run_valid(valid))?;
        block_on(run_violation(violation))?;
    }
}

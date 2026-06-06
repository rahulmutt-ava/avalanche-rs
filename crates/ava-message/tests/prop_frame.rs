// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.6 — `prop::frame_roundtrip`: for an arbitrary op/field set,
//! `build → marshal → frame → read_msg_len → unmarshal` is the identity, under
//! both compression modes (specs/05 §9, 02 §4).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

use bytes::{Bytes, BytesMut};
use proptest::prelude::*;

use ava_message::codec::{Compression, MsgBuilder};
use ava_message::frame::{MAX_MESSAGE_SIZE, read_msg_len, write_msg_len};
use ava_message::proto::p2p;

/// A strategy producing arbitrary, structurally-valid `p2p.Message`s across a
/// spread of ops (network, consensus, app).
fn arb_message() -> impl Strategy<Value = p2p::Message> {
    prop_oneof![
        any::<u32>().prop_map(|u| p2p::message::Message::Ping(p2p::Ping { uptime: u })),
        Just(p2p::message::Message::Pong(p2p::Pong {})),
        (
            prop::collection::vec(any::<u8>(), 0..64),
            any::<u32>(),
            any::<u64>(),
            prop::collection::vec(any::<u8>(), 0..64),
        )
            .prop_map(|(chain, rid, deadline, cont)| {
                p2p::message::Message::Get(p2p::Get {
                    chain_id: Bytes::from(chain),
                    request_id: rid,
                    deadline,
                    container_id: Bytes::from(cont),
                })
            }),
        (
            prop::collection::vec(any::<u8>(), 0..64),
            any::<u32>(),
            any::<u64>(),
            prop::collection::vec(any::<u8>(), 0..512),
        )
            .prop_map(|(chain, rid, deadline, app)| {
                p2p::message::Message::AppRequest(p2p::AppRequest {
                    chain_id: Bytes::from(chain),
                    request_id: rid,
                    deadline,
                    app_bytes: Bytes::from(app),
                })
            }),
        (
            prop::collection::vec(any::<u8>(), 0..64),
            any::<u32>(),
            prop::collection::vec(any::<u8>(), 0..1024),
        )
            .prop_map(|(chain, rid, container)| {
                p2p::message::Message::Put(p2p::Put {
                    chain_id: Bytes::from(chain),
                    request_id: rid,
                    container: Bytes::from(container),
                })
            }),
    ]
    .prop_map(|variant| p2p::Message {
        message: Some(variant),
    })
}

fn compression() -> impl Strategy<Value = Compression> {
    prop_oneof![Just(Compression::None), Just(Compression::Zstd)]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::SourceParallel("proptest-regressions"),
        )),
        ..ProptestConfig::default()
    })]

    #[test]
    fn frame_roundtrip(m in arb_message(), c in compression()) {
        let mb = MsgBuilder::default();
        let (bytes, _saved, op) = mb.marshal(&m, c).expect("marshal");

        // frame: len_be || payload
        let mut buf = BytesMut::new();
        write_msg_len(&mut buf, u32::try_from(bytes.len()).expect("len")).expect("cap");
        buf.extend_from_slice(&bytes);

        // read_msg_len recovers the declared length.
        let len_prefix: [u8; 4] = buf[..4].try_into().expect("4-byte prefix");
        let declared = read_msg_len(len_prefix, MAX_MESSAGE_SIZE).expect("read len");
        prop_assert_eq!(declared as usize, bytes.len());

        // unmarshal recovers the identical message + op.
        let (back, _saved2, op2) = mb.unmarshal(&buf[4..]).expect("unmarshal");
        prop_assert_eq!(op2, op);
        prop_assert_eq!(back, m);
    }
}

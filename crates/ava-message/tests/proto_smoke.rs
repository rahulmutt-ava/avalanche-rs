// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.1 — smoke test that the generated `p2p` proto module exists and that the
//! `Message.message` oneof is wired correctly (specs/05 §2.1, 15 §3.1).

use ava_message::proto::p2p;

#[test]
fn proto_module_has_message_oneof() {
    let m = p2p::Message {
        message: Some(p2p::message::Message::Ping(p2p::Ping { uptime: 0 })),
    };
    assert!(prost::Message::encoded_len(&m) > 0);
}

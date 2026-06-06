// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.4 — `MsgBuilder` marshal/unmarshal + recursive zstd packing (specs/05
//! §1.3/§2.3, 15 §4.2 R4: decode-equivalence, not byte-equality).

use bytes::Bytes;
use prost::Message as _;

use ava_message::codec::{decompress_gzip, Compression, MsgBuilder};
use ava_message::ops::Op;
use ava_message::proto::p2p;

fn ping(uptime: u32) -> p2p::Message {
    p2p::Message {
        message: Some(p2p::message::Message::Ping(p2p::Ping { uptime })),
    }
}

#[test]
fn marshal_unmarshal_uncompressed() {
    let mb = MsgBuilder::default();
    let m = ping(42);

    let (bytes, saved, op) = mb.marshal(&m, Compression::None).unwrap();
    assert_eq!(op, Op::Ping);
    assert_eq!(saved, 0);

    let (back, saved2, op2) = mb.unmarshal(&bytes).unwrap();
    assert_eq!(op2, Op::Ping);
    assert_eq!(saved2, 0);
    assert_eq!(back, m);
}

#[test]
fn marshal_unmarshal_zstd_roundtrip() {
    let mb = MsgBuilder::default();
    // A large, highly compressible Put-like payload.
    let container = Bytes::from(vec![0u8; 4096]);
    let m = p2p::Message {
        message: Some(p2p::message::Message::Put(p2p::Put {
            chain_id: Bytes::from(vec![0u8; 32]),
            request_id: 1,
            container,
        })),
    };

    let (bytes, saved, op) = mb.marshal(&m, Compression::Zstd).unwrap();
    assert_eq!(op, Op::Put);
    // bytes_saved = inner_len - outer_len; for a compressible payload it's > 0.
    assert!(saved > 0, "expected positive bytes_saved, got {saved}");

    // The outer Message's only set field is compressed_zstd.
    let outer = p2p::Message::decode(&bytes[..]).unwrap();
    assert_matches::assert_matches!(
        outer.message,
        Some(p2p::message::Message::CompressedZstd(_))
    );

    // R4: unmarshal recovers the inner message (decode-equivalence).
    let (back, saved2, op2) = mb.unmarshal(&bytes).unwrap();
    assert_eq!(op2, Op::Put);
    assert!(saved2 > 0);
    assert_eq!(back, m);
}

#[test]
fn unmarshal_rejects_corrupt_zstd() {
    let mb = MsgBuilder::default();
    // An outer Message whose compressed_zstd is not valid zstd.
    let outer = p2p::Message {
        message: Some(p2p::message::Message::CompressedZstd(Bytes::from(vec![
            1, 2, 3, 4, 5,
        ]))),
    };
    let raw = outer.encode_to_vec();
    assert!(mb.unmarshal(&raw).is_err());
}

#[test]
fn gzip_decode_tolerance() {
    // gzip is decode-only legacy tolerance; never produced by ava-message.
    let payload = b"hello legacy gzip peer";
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    use std::io::Write as _;
    enc.write_all(payload).unwrap();
    let compressed = enc.finish().unwrap();

    assert_eq!(decompress_gzip(&compressed).unwrap(), payload);
    // Garbage is rejected, not panicked on.
    assert!(decompress_gzip(&[1, 2, 3, 4]).is_err());
}

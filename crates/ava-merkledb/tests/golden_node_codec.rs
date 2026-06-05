// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::node_codec_encode` + `golden::node_codec_decode_rejects` — the
//! on-disk node codec byte-exact vs Go `x/merkledb/codec.go`.

use std::collections::BTreeMap;

use assert_matches::assert_matches;
use bytes::Bytes;

use ava_merkledb::codec::{decode_db_node, encode_db_node};
use ava_merkledb::error::Error;
use ava_merkledb::key::{Key, bytes_needed};
use ava_merkledb::maybe::Maybe;
use ava_merkledb::node::{Child, DbNode};
use ava_types::id::Id;

#[derive(serde::Deserialize)]
struct ChildJson {
    index: u8,
    compressed_key_hex: String,
    compressed_key_len: usize,
    id_hex: String,
    has_value: bool,
}

#[derive(serde::Deserialize)]
struct NodeCase {
    name: String,
    encoded_hex: String,
    has_value: bool,
    #[serde(default)]
    value_hex: String,
    #[serde(default)]
    children: Option<Vec<ChildJson>>,
}

#[derive(serde::Deserialize)]
struct Vectors {
    cases: Vec<NodeCase>,
}

fn hx(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

fn build_node(c: &NodeCase) -> DbNode {
    let value = if c.has_value {
        Maybe::Some(Bytes::from(hx(&c.value_hex)))
    } else {
        Maybe::Nothing
    };
    let mut children: BTreeMap<u8, Child> = BTreeMap::new();
    for ch in c.children.iter().flatten() {
        let key = Key::from_raw(
            Bytes::from(hx(&ch.compressed_key_hex)),
            ch.compressed_key_len,
        );
        let id = Id::from_slice(&hx(&ch.id_hex)).expect("id");
        children.insert(
            ch.index,
            Child {
                compressed_key: key,
                id,
                has_value: ch.has_value,
            },
        );
    }
    DbNode { value, children }
}

#[test]
fn node_codec_encode() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/merkledb/nodes/node_codec_encode.json"
    ))
    .expect("read node vectors");
    let v: Vectors = serde_json::from_str(&raw).expect("parse node vectors");

    for c in &v.cases {
        let node = build_node(c);
        let encoded = encode_db_node(&node);
        assert_eq!(
            hex::encode(&encoded),
            c.encoded_hex,
            "{}: encode bytes",
            c.name
        );

        // Round-trip: decoding the encoded bytes yields the same node.
        let decoded = decode_db_node(&encoded).expect("decode");
        assert_eq!(decoded, node, "{}: round-trip", c.name);
    }
}

#[test]
fn node_codec_decode_rejects() {
    // A valid baseline: value Nothing (0x00) + 1 child {index 0, key len 0,
    // id all-0x11, has_value false}.
    let mut id = [0u8; 32];
    id.iter_mut().for_each(|b| *b = 0x11);
    let base = DbNode {
        value: Maybe::Nothing,
        children: BTreeMap::from([(
            0u8,
            Child {
                compressed_key: Key::empty(),
                id: Id::from(id),
                has_value: false,
            },
        )]),
    };
    let good = encode_db_node(&base);
    assert!(decode_db_node(&good).is_ok());

    // 1) trailing bytes -> ExtraSpace.
    let mut extra = good.clone();
    extra.push(0xff);
    assert_matches!(decode_db_node(&extra), Err(Error::ExtraSpace));

    // 2) too many children: num_children = 257 > BranchFactorLargest (256).
    //    0x00 (no value) then uvarint(257) = 0x81 0x02.
    let too_many = vec![0x00, 0x81, 0x02];
    assert_matches!(decode_db_node(&too_many), Err(Error::TooManyChildren));

    // 3) child index >= 256 (out of byte range): num_children=1, index=256.
    //    0x00 (no value), 0x01 (num children), uvarint(256)=0x80 0x02, ...
    let big_index = vec![0x00, 0x01, 0x80, 0x02];
    assert_matches!(decode_db_node(&big_index), Err(Error::ChildIndexTooLarge));

    // 4) out-of-order / duplicate child indices: two children both index 0.
    let mut dup = DbNode {
        value: Maybe::Nothing,
        children: BTreeMap::new(),
    };
    dup.children.insert(
        0,
        Child {
            compressed_key: Key::empty(),
            id: Id::from(id),
            has_value: false,
        },
    );
    let mut dup_bytes = encode_db_node(&dup);
    // Hand-craft a second child entry with the same index 0 and bump count.
    // Bytes: 0x00 (value nothing) 0x01 (num=1) [child0...]. Change count to 2
    // and append a copy of the child entry (index 0 again).
    let child_entry = {
        // child entry = uvarint(index=0)=0x00, key(len 0)=0x00, id(32 0x11), bool(0)=0x00
        let mut e = vec![0x00, 0x00];
        e.extend_from_slice(&id);
        e.push(0x00);
        e
    };
    dup_bytes[1] = 0x02; // num children = 2
    dup_bytes.extend_from_slice(&child_entry);
    assert_matches!(decode_db_node(&dup_bytes), Err(Error::ChildIndexTooLarge));

    // 5) leading-zero (non-canonical) uvarint for num_children: 0x80 0x00.
    let leading = vec![0x00, 0x80, 0x00];
    assert_matches!(decode_db_node(&leading), Err(Error::LeadingZeroes));

    // 6) non-zero key padding: a 4-bit child key whose low nibble is non-zero.
    //    0x00 (value nothing), 0x01 (num=1), 0x00 (index 0),
    //    key: uvarint(bit_len=4)=0x04, byte 0x1f (low nibble non-zero) ...
    let mut bad_pad = vec![0x00, 0x01, 0x00, 0x04, 0x1f];
    bad_pad.extend_from_slice(&id);
    bad_pad.push(0x00);
    assert_matches!(decode_db_node(&bad_pad), Err(Error::NonZeroKeyPadding));

    // 7) unexpected EOF (truncated id).
    let mut truncated = vec![0x00, 0x01, 0x00, 0x00];
    truncated.extend_from_slice(&id[..10]); // only 10 of 32 id bytes
    assert_matches!(decode_db_node(&truncated), Err(Error::UnexpectedEof));

    // 8) sanity: a partial-byte key with zero padding decodes fine.
    let zero_pad_key = Key::from_raw(Bytes::from(vec![0x10]), 4); // 0x10, low nibble 0
    let mut ok_node = DbNode {
        value: Maybe::Nothing,
        children: BTreeMap::new(),
    };
    ok_node.children.insert(
        0,
        Child {
            compressed_key: zero_pad_key,
            id: Id::from(id),
            has_value: false,
        },
    );
    let ok_bytes = encode_db_node(&ok_node);
    assert!(decode_db_node(&ok_bytes).is_ok());
    // bytes_needed sanity (exercise the re-exported helper).
    assert_eq!(bytes_needed(4), 1);
}

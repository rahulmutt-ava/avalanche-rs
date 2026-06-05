// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden-vector tests for `ava-archivedb` key/value encoding, byte-matching
//! avalanchego `x/archivedb` (specs/04 §5.2, §6.5). Vectors are extracted from
//! the Go encoders — see `tests/vectors/archivedb/key_encoding.json`.

use ava_archivedb::value::new_db_value;
use ava_archivedb::{
    HEIGHT_KEY, height_key, new_db_key_from_metadata, new_db_key_from_user, parse_db_key_from_user,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct UserVec {
    key_hex: String,
    height: u64,
    db_key_hex: String,
    db_prefix_hex: String,
}

#[derive(Deserialize)]
struct MetaVec {
    key_hex: String,
    db_key_hex: String,
}

#[derive(Deserialize)]
struct ValueVec {
    value_hex: String,
    db_value_hex: String,
    tombstone_db_value_hex: String,
}

#[derive(Deserialize)]
struct Vectors {
    height_key: String,
    metadata_keys: Vec<MetaVec>,
    user_keys: Vec<UserVec>,
    value: ValueVec,
}

fn load() -> Vectors {
    let raw = include_str!("../../../tests/vectors/archivedb/key_encoding.json");
    serde_json::from_str(raw).expect("parse golden vectors")
}

#[test]
fn archivedb_key_encoding() {
    let v = load();

    // User keys: uvarint(len(key)) || key || BigEndian(^height).
    for uv in &v.user_keys {
        let key = hex::decode(&uv.key_hex).unwrap();
        let (db_key, prefix) = new_db_key_from_user(&key, uv.height);
        pretty_assertions::assert_eq!(
            hex::encode(&db_key),
            uv.db_key_hex,
            "user db_key mismatch for key={} height={}",
            uv.key_hex,
            uv.height
        );
        pretty_assertions::assert_eq!(
            hex::encode(&prefix),
            uv.db_prefix_hex,
            "user db_prefix mismatch for key={} height={}",
            uv.key_hex,
            uv.height
        );

        // Round-trip parse.
        let (parsed_key, parsed_height) = parse_db_key_from_user(&db_key).unwrap();
        pretty_assertions::assert_eq!(parsed_key, key);
        assert_eq!(parsed_height, uv.height);
    }

    // Metadata keys: uvarint(len(key)+1) || key.
    for mv in &v.metadata_keys {
        let key = hex::decode(&mv.key_hex).unwrap();
        let db_key = new_db_key_from_metadata(&key);
        pretty_assertions::assert_eq!(
            hex::encode(&db_key),
            mv.db_key_hex,
            "metadata db_key mismatch for key={}",
            mv.key_hex
        );
    }

    // heightKey == metadata encoding of the empty key == 0x01.
    assert_eq!(hex::encode(height_key()), v.height_key);
    assert_eq!(hex::encode(HEIGHT_KEY), v.height_key);

    // Stored value: 0x00 || value; tombstone is empty.
    let value = hex::decode(&v.value.value_hex).unwrap();
    pretty_assertions::assert_eq!(hex::encode(new_db_value(&value)), v.value.db_value_hex);
    assert_eq!(v.value.tombstone_db_value_hex, "");
}

#[test]
fn metadata_never_overlaps_user_prefix() {
    // The +1 length prefix guarantees a metadata key is never a prefix of any
    // user key with the same logical bytes (mirrors Go FuzzMetadataKeyInvariant).
    for bytes in [&b""[..], b"\x00", b"foo", b"hello world"] {
        let (_user_key, user_prefix) = new_db_key_from_user(bytes, 0);
        let meta_key = new_db_key_from_metadata(bytes);
        assert!(
            !meta_key.starts_with(&user_prefix),
            "metadata key {meta_key:?} must not start with user prefix {user_prefix:?}"
        );
    }
}

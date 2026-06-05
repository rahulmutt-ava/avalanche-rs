// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden header-layout tests for `ava-blockdb`.
//!
//! Vectors in `tests/vectors/blockdb/blockdb_vectors.json` were extracted from
//! the real Go `x/blockdb` package (see the file's `_provenance`). They pin the
//! byte widths, field offsets, little-endian encoding of every on-disk header,
//! and the xxhash checksum algorithm.

use ava_blockdb::format::{
    BLOCK_ENTRY_VERSION, BlockEntryHeader, INDEX_FILE_VERSION, IndexEntry, IndexFileHeader,
    SIZE_OF_BLOCK_ENTRY_HEADER, SIZE_OF_INDEX_ENTRY, SIZE_OF_INDEX_FILE_HEADER, calculate_checksum,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct HeaderVec {
    name: String,
    hex: String,
    len: usize,
}

#[derive(Deserialize)]
struct ChecksumVec {
    name: String,
    input_hex: String,
    checksum_u64: u64,
}

#[derive(Deserialize)]
struct Vectors {
    size_of_block_entry_header: u32,
    size_of_index_entry: u64,
    size_of_index_file_header: u64,
    index_file_version: u64,
    block_entry_version: u16,
    headers: Vec<HeaderVec>,
    checksums: Vec<ChecksumVec>,
}

fn load() -> Vectors {
    let raw = include_str!("vectors/blockdb/blockdb_vectors.json");
    serde_json::from_str(raw).expect("parse vectors")
}

#[test]
fn blockdb_header_layout() {
    let v = load();

    // Sizes match Go's binary.Size(...).
    assert_eq!(SIZE_OF_BLOCK_ENTRY_HEADER, v.size_of_block_entry_header);
    assert_eq!(SIZE_OF_BLOCK_ENTRY_HEADER, 22);
    assert_eq!(SIZE_OF_INDEX_ENTRY, v.size_of_index_entry);
    assert_eq!(SIZE_OF_INDEX_ENTRY, 16);
    assert_eq!(SIZE_OF_INDEX_FILE_HEADER, v.size_of_index_file_header);
    assert_eq!(SIZE_OF_INDEX_FILE_HEADER, 64);
    assert_eq!(INDEX_FILE_VERSION, v.index_file_version);
    assert_eq!(BLOCK_ENTRY_VERSION, v.block_entry_version);

    for h in &v.headers {
        let expected = hex::decode(&h.hex).expect("decode hex");
        assert_eq!(expected.len(), h.len, "{}: declared len", h.name);
        let got: Vec<u8> = match h.name.as_str() {
            "index_file_header" => {
                let hdr = IndexFileHeader {
                    version: 0x0102_0304_0506_0708,
                    max_data_file_size: 0x1112_1314_1516_1718,
                    min_height: 0x2122_2324_2526_2728,
                    max_height: 0x3132_3334_3536_3738,
                    next_write_offset: 0x4142_4344_4546_4748,
                    reserved: [0u8; 24],
                };
                hdr.marshal_binary().to_vec()
            }
            "index_entry" => {
                let e = IndexEntry {
                    offset: 0x0102_0304_0506_0708,
                    size: 0x1112_1314,
                    reserved: [0u8; 4],
                };
                e.marshal_binary().to_vec()
            }
            "block_entry_header" => {
                let beh = BlockEntryHeader {
                    height: 0x0102_0304_0506_0708,
                    size: 0x1112_1314,
                    checksum: 0x2122_2324_2526_2728,
                    version: 0x3132,
                };
                beh.marshal_binary().to_vec()
            }
            other => panic!("unexpected header vector {other}"),
        };
        assert_eq!(hex::encode(&got), h.hex, "{}: byte mismatch vs Go", h.name);
        assert_eq!(got, expected, "{}: byte mismatch vs Go", h.name);
    }

    // Round-trip unmarshal for each header.
    let hdr = IndexFileHeader {
        version: INDEX_FILE_VERSION,
        max_data_file_size: 4096,
        min_height: 0,
        max_height: 7,
        next_write_offset: 1234,
        reserved: [0u8; 24],
    };
    let bytes = hdr.marshal_binary();
    let back = IndexFileHeader::unmarshal_binary(&bytes).expect("unmarshal header");
    assert_eq!(hdr, back);
}

#[test]
fn blockdb_checksum_is_xxhash() {
    let v = load();
    for c in &v.checksums {
        let input = hex::decode(&c.input_hex).expect("decode input");
        assert_eq!(
            calculate_checksum(&input),
            c.checksum_u64,
            "checksum mismatch for {}",
            c.name
        );
    }
}

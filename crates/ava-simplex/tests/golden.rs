// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden tests asserting the Simplex canoto wire format round-trips
//! **byte-identical** to Go's generated `qc.canoto.go` / `block.canoto.go`.
//!
//! The vectors in `tests/vectors/` were captured from a scratch Go program
//! (provenance in `tests/PORTING.md`) and re-verified with the upstream canoto
//! `Reader`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use ava_simplex::block::{Block, ProtocolMetadata};
use ava_simplex::messages::{SIGNATURE_LEN, decode_qc, encode_qc};

fn hex_decode(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "odd-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// `golden::simplex_qc_roundtrip` — a canoto-encoded QC round-trips byte-exact
/// vs the captured Go vector.
#[test]
fn simplex_qc_roundtrip() {
    let want = hex_decode(include_str!("vectors/qc_canoto.hex"));

    // Reconstruct the inputs the Go vector was built from:
    //   Sig     = bytes 1..=96
    //   Signers = bitset {0,1} => big-endian big.Int 0b11 => [0x03]
    let mut sig = [0u8; SIGNATURE_LEN];
    for (i, b) in sig.iter_mut().enumerate() {
        *b = (i + 1) as u8;
    }
    let signers = vec![0x03u8];

    // Encode must equal the Go bytes exactly.
    let got = encode_qc(&sig, &signers);
    assert_eq!(got, want, "QC canoto encoding diverged from Go vector");

    // And the Go bytes must decode back to the same logical fields.
    let (sig_out, signers_out) = decode_qc(&want).expect("decode QC vector");
    assert_eq!(sig_out, sig);
    assert_eq!(signers_out, signers);

    // Encoding the decoded fields reproduces the vector (full round-trip).
    assert_eq!(encode_qc(&sig_out, &signers_out), want);
}

/// `golden::simplex_block_roundtrip` — a canoto-encoded block round-trips
/// byte-exact vs the captured Go vector.
#[test]
fn simplex_block_roundtrip() {
    let want = hex_decode(include_str!("vectors/block_canoto.hex"));

    // The Go vector's block fields:
    //   Metadata  = ProtocolMetadata{Version:1, Epoch:2, Round:3, Seq:4, Prev:0xAB*32}
    //   InnerBlock = 0xdeadbeef
    //   Blacklist  = 0x0000
    let md = ProtocolMetadata {
        version: 1,
        epoch: 2,
        round: 3,
        seq: 4,
        prev: [0xab; 32],
    };
    let block = Block::new(md.clone(), vec![0xde, 0xad, 0xbe, 0xef], vec![0x00, 0x00]);

    let got = block.to_bytes();
    assert_eq!(got, want, "block canoto encoding diverged from Go vector");

    // The Go bytes decode back to the same block (and recompute the same digest).
    let parsed = Block::from_bytes(&want).expect("decode block vector");
    assert_eq!(parsed.metadata, md);
    assert_eq!(parsed.inner_block, vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(parsed.blacklist, vec![0x00, 0x00]);
    assert_eq!(parsed.to_bytes(), want);
    assert_eq!(parsed.digest, block.digest);
}

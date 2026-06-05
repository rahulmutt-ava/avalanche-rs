// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M0.5 — `Id` operations: `from_slice`, `prefix`, `append`, `xor`, `bit`,
//! `hex`, and the bit-subset helpers (`equal_subset`/`first_difference_subset`).

use ava_types::bits::{equal_subset, first_difference_subset};
use ava_types::error::Error;
use ava_types::id::Id;

use sha2::{Digest, Sha256};

#[test]
fn id_prefix_and_bit() {
    let id = Id::from_slice(&[1u8; 32]).unwrap();

    // byte 0 == 0x01 -> LSB set, so bit(0) == 1, bit(1) == 0.
    assert_eq!(id.bit(0), 1);
    assert_eq!(id.bit(1), 0);

    let p = id.prefix(&[7]); // be_u64(7) ++ bytes -> sha256
    assert_ne!(p, id);

    // Verify prefix against an independently computed sha256.
    let mut hasher = Sha256::new();
    hasher.update(7u64.to_be_bytes());
    hasher.update([1u8; 32]);
    let expected = Id::from_slice(&hasher.finalize()).unwrap();
    assert_eq!(p, expected);
}

#[test]
fn id_append_matches_sha256() {
    let id = Id::from_slice(&[2u8; 32]).unwrap();
    let got = id.append(&[1u32, 2u32]);

    let mut hasher = Sha256::new();
    hasher.update([2u8; 32]);
    hasher.update(1u32.to_be_bytes());
    hasher.update(2u32.to_be_bytes());
    let expected = Id::from_slice(&hasher.finalize()).unwrap();
    assert_eq!(got, expected);
}

#[test]
fn id_xor() {
    let a = Id::from_slice(&[0xFFu8; 32]).unwrap();
    let b = Id::from_slice(&[0x0Fu8; 32]).unwrap();
    let x = a.xor(&b);
    assert_eq!(x, Id::from_slice(&[0xF0u8; 32]).unwrap());
    // xor with self == EMPTY.
    assert_eq!(a.xor(&a), Id::EMPTY);
}

#[test]
fn id_hex() {
    let id = Id::from_slice(&[0xABu8; 32]).unwrap();
    assert_eq!(id.hex(), "ab".repeat(32));
}

#[test]
fn from_slice_wrong_len() {
    let err = Id::from_slice(&[0u8; 31]).unwrap_err();
    assert!(matches!(err, Error::InvalidHashLen { .. }));
}

#[test]
fn bit_indexing() {
    // byte 0 = 0x80 -> only MSB (bit 7) set.
    let mut bytes = [0u8; 32];
    bytes[0] = 0x80;
    let id = Id::from_slice(&bytes).unwrap();
    assert_eq!(id.bit(7), 1);
    assert_eq!(id.bit(0), 0);
    // byte 1 = 0x01 -> bit 8 set.
    bytes[1] = 0x01;
    let id = Id::from_slice(&bytes).unwrap();
    assert_eq!(id.bit(8), 1);
}

#[test]
fn equal_subset_basic() {
    let a = Id::from_slice(&[0u8; 32]).unwrap();
    let b = Id::from_slice(&[0u8; 32]).unwrap();
    assert!(equal_subset(0, 256, &a, &b));

    let mut cb = [0u8; 32];
    cb[0] = 0x01; // differs at bit 0.
    let c = Id::from_slice(&cb).unwrap();
    // [1, 256) excludes bit 0, so still equal.
    assert!(equal_subset(1, 256, &a, &c));
    // [0, 1) includes bit 0, so not equal.
    assert!(!equal_subset(0, 1, &a, &c));
    // empty range -> true.
    assert!(equal_subset(5, 5, &a, &c));
}

#[test]
fn first_difference_subset_basic() {
    let a = Id::from_slice(&[0u8; 32]).unwrap();
    let mut cb = [0u8; 32];
    cb[0] = 0x01; // bit 0.
    let c = Id::from_slice(&cb).unwrap();
    assert_eq!(first_difference_subset(0, 256, &a, &c), Some(0));
    // Excluding bit 0 -> no difference.
    assert_eq!(first_difference_subset(1, 256, &a, &c), None);

    let mut db = [0u8; 32];
    db[1] = 0x01; // bit 8.
    let d = Id::from_slice(&db).unwrap();
    assert_eq!(first_difference_subset(0, 256, &a, &d), Some(8));
}

#[test]
fn ord_matches_bytes_compare() {
    let a = Id::from_slice(&[0u8; 32]).unwrap();
    let mut bb = [0u8; 32];
    bb[0] = 1;
    let b = Id::from_slice(&bb).unwrap();
    assert!(a < b);
}

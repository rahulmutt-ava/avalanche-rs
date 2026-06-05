// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for the big-endian [`Packer`] (M0.14).
//!
//! Mirrors `utils/wrappers/packing_test.go`: round-trip identity for every
//! primitive, golden big-endian byte layout for fixed inputs, the bad-bool
//! rejection, and the sticky-error (first-error-wins, then no-op zero) model.

use assert_matches::assert_matches;
use ava_codec::error::PackerError;
use ava_codec::packer::{MAX_STRING_LEN, Packer};
use proptest::prelude::*;

mod golden {
    use super::*;

    #[test]
    fn pack_u32_is_big_endian() {
        let mut p = Packer::new_write(64);
        p.pack_u32(1);
        assert_eq!(p.into_bytes(), vec![0, 0, 0, 1]);
    }

    #[test]
    fn pack_primitives_big_endian() {
        let mut p = Packer::new_write(64);
        p.pack_byte(0xAB);
        p.pack_u16(0x0102);
        p.pack_u32(0x0304_0506);
        p.pack_u64(0x0708_090A_0B0C_0D0E);
        p.pack_bool(true);
        p.pack_bool(false);
        assert_eq!(
            p.into_bytes(),
            vec![
                0xAB, // byte
                0x01, 0x02, // u16
                0x03, 0x04, 0x05, 0x06, // u32
                0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, // u64
                0x01, // bool true
                0x00, // bool false
            ]
        );
    }

    #[test]
    fn pack_bytes_has_u32_len_prefix() {
        let mut p = Packer::new_write(64);
        p.pack_bytes(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(p.into_bytes(), vec![0, 0, 0, 4, 0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn pack_str_has_u16_len_prefix() {
        let mut p = Packer::new_write(64);
        p.pack_str("hi");
        assert_eq!(p.into_bytes(), vec![0, 2, b'h', b'i']);
    }

    #[test]
    fn pack_fixed_bytes_has_no_prefix() {
        let mut p = Packer::new_write(64);
        p.pack_fixed_bytes(&[1, 2, 3]);
        assert_eq!(p.into_bytes(), vec![1, 2, 3]);
    }
}

mod unit {
    use super::*;

    #[test]
    fn unpack_bool_rejects_non_0_1() {
        let mut p = Packer::new_read(&[0x02]);
        let _ = p.unpack_bool();
        assert_matches!(p.error(), Some(PackerError::BadBool));
    }

    #[test]
    fn packer_bad_bool_sticky() {
        // After a bad bool, the packer is sticky: subsequent reads no-op and
        // return zero, and the FIRST error is preserved.
        let mut p = Packer::new_read(&[0x02, 0x00, 0x00, 0x00, 0x07]);
        let b = p.unpack_bool();
        assert!(!b);
        assert_matches!(p.error(), Some(PackerError::BadBool));
        // Subsequent op is a no-op returning zero; error identity unchanged.
        let v = p.unpack_u32();
        assert_eq!(v, 0);
        assert_matches!(p.error(), Some(PackerError::BadBool));
    }

    #[test]
    fn unpack_insufficient_length() {
        let mut p = Packer::new_read(&[0x00, 0x01]);
        let _ = p.unpack_u32();
        assert_matches!(p.error(), Some(PackerError::InsufficientLength));
    }

    #[test]
    fn write_overflow_max_size_sets_insufficient_length() {
        let mut p = Packer::with_max_size(4);
        p.pack_u64(1); // needs 8 bytes, max is 4
        assert_matches!(p.error(), Some(PackerError::InsufficientLength));
    }

    #[test]
    fn pack_str_rejects_oversize() {
        let mut p = Packer::new_write(8);
        let big = "a".repeat(MAX_STRING_LEN + 1);
        p.pack_str(&big);
        assert_matches!(p.error(), Some(PackerError::InvalidInput));
    }

    #[test]
    fn unpack_limited_bytes_rejects_oversize() {
        // u32 len prefix says 5, limit is 4.
        let mut p = Packer::new_read(&[0, 0, 0, 5, 1, 2, 3, 4, 5]);
        let _ = p.unpack_limited_bytes(4);
        assert_matches!(p.error(), Some(PackerError::Oversized));
    }

    #[test]
    fn unpack_limited_str_rejects_oversize() {
        let mut p = Packer::new_read(&[0, 5, b'a', b'b', b'c', b'd', b'e']);
        let _ = p.unpack_limited_str(4);
        assert_matches!(p.error(), Some(PackerError::Oversized));
    }

    #[test]
    fn bytes_roundtrip() {
        let mut w = Packer::new_write(64);
        w.pack_bytes(&[9, 8, 7]);
        assert!(w.error().is_none());
        let bytes = w.into_bytes();
        let mut r = Packer::new_read(&bytes);
        assert_eq!(r.unpack_bytes(), vec![9, 8, 7]);
        assert!(r.error().is_none());
    }

    #[test]
    fn str_roundtrip() {
        let mut w = Packer::new_write(64);
        w.pack_str("hello");
        let bytes = w.into_bytes();
        let mut r = Packer::new_read(&bytes);
        assert_eq!(r.unpack_str(), "hello");
        assert!(r.error().is_none());
    }
}

mod prop {
    use super::*;

    proptest! {
        #[test]
        fn packer_roundtrip(
            b in any::<u8>(),
            s in any::<u16>(),
            i in any::<u32>(),
            l in any::<u64>(),
            flag in any::<bool>(),
            data in proptest::collection::vec(any::<u8>(), 0..64),
            text in "\\PC{0,32}",
        ) {
            let mut w = Packer::new_write(1024);
            w.pack_byte(b);
            w.pack_u16(s);
            w.pack_u32(i);
            w.pack_u64(l);
            w.pack_bool(flag);
            w.pack_bytes(&data);
            w.pack_str(&text);
            prop_assert!(w.error().is_none());
            let bytes = w.into_bytes();

            let mut r = Packer::new_read(&bytes);
            prop_assert_eq!(r.unpack_byte(), b);
            prop_assert_eq!(r.unpack_u16(), s);
            prop_assert_eq!(r.unpack_u32(), i);
            prop_assert_eq!(r.unpack_u64(), l);
            prop_assert_eq!(r.unpack_bool(), flag);
            prop_assert_eq!(r.unpack_bytes(), data);
            prop_assert_eq!(r.unpack_str(), text);
            prop_assert!(r.error().is_none());
            prop_assert_eq!(r.offset(), bytes.len());
        }
    }
}

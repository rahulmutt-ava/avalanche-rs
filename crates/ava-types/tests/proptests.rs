// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M0.24 — Property tests for `Id`/`ShortId`/`NodeId` string and byte
//! round-trips plus purity of `Id` ops.
//!
//! Mirrors Go `ids` fuzz coverage (`FuzzEncodeDecode`): the CB58 string form
//! decode(encode(x)) == x, the byte form `from_slice(as_bytes()) == x`, and the
//! determinism/symmetry of `bit`/`xor`. See `specs/02-testing-strategy.md`
//! §4.1–§4.3 (ava-types row).

use std::str::FromStr;

use ava_types::id::{ID_LEN, Id};
use ava_types::node_id::{NODE_ID_LEN, NODE_ID_PREFIX, NodeId};
use ava_types::short_id::{SHORT_ID_LEN, ShortId};

use proptest::prelude::*;

mod prop {
    use super::*;

    proptest! {
        /// `Id::from_str(&id.to_string()) == id` for arbitrary 32-byte ids.
        /// Mirrors Go `FuzzEncodeDecode` over the CB58 string form.
        #[test]
        fn id_string_roundtrip(bytes in any::<[u8; ID_LEN]>()) {
            let id = Id::from(bytes);
            let s = id.to_string();
            let parsed = Id::from_str(&s).expect("Id CB58 string must round-trip");
            prop_assert_eq!(parsed, id);
        }

        /// `ShortId::from_str(&sid.to_string()) == sid` for arbitrary 20-byte ids.
        #[test]
        fn short_id_string_roundtrip(bytes in any::<[u8; SHORT_ID_LEN]>()) {
            let sid = ShortId::from(bytes);
            let s = sid.to_string();
            let parsed = ShortId::from_str(&s).expect("ShortId CB58 string must round-trip");
            prop_assert_eq!(parsed, sid);
        }

        /// `NodeId::from_str(&nid.to_string()) == nid`; the Display output carries
        /// the `NodeID-` prefix and bare (prefix-stripped) CB58 is rejected on parse.
        #[test]
        fn node_id_string_roundtrip(bytes in any::<[u8; NODE_ID_LEN]>()) {
            let nid = NodeId::from(bytes);
            let s = nid.to_string();

            // The Display output must carry the required prefix.
            prop_assert!(s.starts_with(NODE_ID_PREFIX), "missing NodeID- prefix: {}", s);

            let parsed = NodeId::from_str(&s).expect("NodeId string must round-trip");
            prop_assert_eq!(parsed, nid);

            // The prefix is required on parse: bare CB58 must be rejected.
            let bare = s.strip_prefix(NODE_ID_PREFIX).expect("prefix checked above");
            prop_assert!(NodeId::from_str(bare).is_err(), "bare CB58 accepted: {}", bare);
        }

        /// `Id::from_slice(id.as_bytes())` round-trips to the same 32 bytes.
        #[test]
        fn id_bytes_roundtrip(bytes in any::<[u8; ID_LEN]>()) {
            let id = Id::from(bytes);
            let parsed = Id::from_slice(id.as_bytes()).expect("32 bytes is a valid Id");
            prop_assert_eq!(parsed, id);
            prop_assert_eq!(parsed.as_bytes(), &bytes);
        }

        /// `Id` ops are pure: `bit(i)` is deterministic and yields 0/1, and `xor`
        /// is symmetric (`a.xor(b) == b.xor(a)`) with `a.xor(a) == EMPTY`.
        #[test]
        fn id_ops_pure(
            ab in any::<[u8; ID_LEN]>(),
            bb in any::<[u8; ID_LEN]>(),
            i in 0usize..(ID_LEN * 8),
        ) {
            let a = Id::from(ab);
            let b = Id::from(bb);

            // bit() is deterministic and a single bit.
            let bit = a.bit(i);
            prop_assert_eq!(a.bit(i), bit);
            prop_assert!(bit == 0 || bit == 1);

            // xor is symmetric and self-inverse.
            prop_assert_eq!(a.xor(&b), b.xor(&a));
            prop_assert_eq!(a.xor(&a), Id::EMPTY);
            // xor twice with the same operand returns the original.
            prop_assert_eq!(a.xor(&b).xor(&b), a);
        }
    }
}

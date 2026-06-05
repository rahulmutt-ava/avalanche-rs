// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests for the linear codec (M0.24, EXIT-GATE).
//!
//! Implements the `specs/02-testing-strategy.md` §4.2 row for `ava-codec`:
//!
//! - **round-trip** (`prop::codec_roundtrip`, 4096 cases): for every structurally
//!   valid typed value `x`, `unmarshal(marshal(x)) == x`. The representative type
//!   covers a tagged int, a `[u8; N]` fixed array, a `Vec<u8>`, a `String`, a
//!   `Vec<T>` of a non-`u8` element, and a nested struct.
//! - **decode never panics**: `Manager::unmarshal` over arbitrary `&[u8]` returns
//!   `Ok`/`Err` but never panics.
//! - **length-prefix bounds**: an oversize `u32` slice count is rejected (never a
//!   panic / never an unbounded allocation), surfacing through the codec error set.

use std::sync::Arc;

use ava_codec::AvaCodec;
use ava_codec::error::CodecError;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use proptest::prelude::*;

const VERSION: u16 = 0;

// ----- the representative wire-kind-covering type -----

/// A non-`u8` element type so the outer `Vec<Pair>` exercises the
/// `u32`-count + per-element codec path (distinct from `Vec<u8>`).
#[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
struct Pair {
    #[codec]
    a: u16,
    #[codec]
    b: u32,
}

/// A nested struct embedded by value inside [`Repr`].
#[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
struct Nested {
    #[codec]
    n: u64,
    #[codec]
    inner: Pair,
}

/// The representative top-level type. One field per wire kind under test.
#[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
struct Repr {
    /// tagged int
    #[codec]
    tag: u32,
    /// fixed array (`[u8; N]`, no length prefix)
    #[codec]
    id: [u8; 8],
    /// `Vec<u8>` (`u32` length prefix + raw bytes)
    #[codec]
    blob: Vec<u8>,
    /// `String` (`u16` length prefix + UTF-8)
    #[codec]
    name: String,
    /// `Vec<T>` of a non-`u8` element (`u32` count + per-element)
    #[codec]
    items: Vec<Pair>,
    /// nested struct (by value)
    #[codec]
    nested: Nested,
}

fn manager() -> Manager {
    let m = Manager::with_default_max_size();
    m.register(VERSION, Arc::new(LinearCodec::new())).unwrap();
    m
}

// ----- strategies producing structurally valid values -----

fn arb_pair() -> impl Strategy<Value = Pair> {
    (any::<u16>(), any::<u32>()).prop_map(|(a, b)| Pair { a, b })
}

fn arb_nested() -> impl Strategy<Value = Nested> {
    (any::<u64>(), arb_pair()).prop_map(|(n, inner)| Nested { n, inner })
}

fn arb_repr() -> impl Strategy<Value = Repr> {
    (
        any::<u32>(),
        any::<[u8; 8]>(),
        proptest::collection::vec(any::<u8>(), 0..256),
        // `\PC` excludes control chars; keeps strings printable UTF-8. Length is
        // bounded so the value stays well under the manager's max decode size.
        "\\PC{0,64}",
        proptest::collection::vec(arb_pair(), 0..32),
        arb_nested(),
    )
        .prop_map(|(tag, id, blob, name, items, nested)| Repr {
            tag,
            id,
            blob,
            name,
            items,
            nested,
        })
}

mod prop {
    use super::*;

    proptest! {
        // EXIT-GATE: `specs/02-testing-strategy.md` §4.3 mandates 4096 cases.
        #![proptest_config(ProptestConfig { cases: 4096, ..ProptestConfig::default() })]

        /// `unmarshal(marshal(x)) == x` for every structurally valid `x`.
        #[test]
        fn codec_roundtrip(x in arb_repr()) {
            let m = manager();
            let bytes = m.marshal(VERSION, &x).expect("marshal must succeed");

            let mut got = Repr::default();
            let ver = m
                .unmarshal(&bytes, &mut got)
                .expect("unmarshal of self-produced bytes must succeed");

            prop_assert_eq!(ver, VERSION);
            prop_assert_eq!(got, x);
        }

        /// Decoding arbitrary bytes returns `Ok`/`Err` but never panics, and a
        /// successful decode re-marshals back to the same bytes (idempotent
        /// canonical form — the manager's trailing-byte check guarantees a clean
        /// decode consumed the whole input).
        #[test]
        fn decode_never_panics(raw in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let m = manager();
            let mut dst = Repr::default();
            // The only hard contract: this returns without panicking.
            if m.unmarshal(&raw, &mut dst).is_ok() {
                let re = m.marshal(VERSION, &dst).expect("re-marshal must succeed");
                prop_assert_eq!(re, raw);
            }
        }
    }
}

mod bounds {
    use super::*;

    /// A `Vec<T>` whose `u32` count exceeds `i32::MAX` (the codec's slice-length
    /// ceiling) is rejected — never an unbounded allocation, never a panic.
    ///
    /// The crafted buffer declares an oversize count with no element body; the
    /// codec must reject it via either the explicit length guard
    /// ([`CodecError::MaxSliceLenExceeded`]) or the body shortfall surfacing as a
    /// packer error. The property is "errors, does not allocate / panic".
    #[derive(AvaCodec, Default, Debug, PartialEq, Eq, Clone)]
    struct VecStruct {
        #[codec]
        items: Vec<Pair>,
    }

    proptest! {
        #[test]
        fn oversize_count_is_rejected(count in (i32::MAX as u32 + 1)..=u32::MAX) {
            let m = manager();
            // version(2) + u32 count, with no element body.
            let mut buf = vec![0x00, 0x00];
            buf.extend_from_slice(&count.to_be_bytes());

            let mut dst = VecStruct::default();
            let res = m.unmarshal(&buf, &mut dst);
            prop_assert!(
                matches!(
                    res,
                    Err(CodecError::MaxSliceLenExceeded) | Err(CodecError::Packer(_))
                ),
                "oversize count must error, got {:?}",
                res
            );
        }

        /// A declared length far beyond the remaining body must error rather than
        /// allocate or panic — exercised through the representative type's `blob`
        /// field with a truncated body.
        #[test]
        fn declared_len_beyond_body_errors(declared in 1u32..=u32::MAX) {
            let m = manager();
            // version(2) + tag(4) + id[8] + blob count(4 = declared) with no bytes.
            let mut buf = vec![0x00, 0x00]; // version
            buf.extend_from_slice(&0u32.to_be_bytes()); // tag
            buf.extend_from_slice(&[0u8; 8]); // id
            buf.extend_from_slice(&declared.to_be_bytes()); // blob: oversize count

            let mut dst = Repr::default();
            let res = m.unmarshal(&buf, &mut dst);
            prop_assert!(res.is_err(), "truncated body must error, got {:?}", res);
        }
    }
}

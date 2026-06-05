// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! prefixdb SHA-256 namespacing golden (04 §2.3, §10.1).
//!
//! Asserts `make_prefix(p) == SHA256(p)` and
//! `join_prefixes(make_prefix(a), b) == SHA256(SHA256(a) ‖ b)` against a
//! Go-extracted vector (`tests/vectors/prefix/prefix_namespacing.json`) so the
//! on-disk `SHA256(prefix) ‖ key` layout stays byte-identical with avalanchego.
//!
//! The `unused_crate_dependencies` allow is unconditional: the package's other
//! deps are linked into every test binary but unused here (a known
//! false-positive of that lint for integration tests).

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

use ava_database::prefixdb::{join_prefixes, make_prefix};

/// The committed Go vector.
const VECTOR: &str = include_str!("vectors/prefix/prefix_namespacing.json");

mod golden {
    use super::*;

    /// `make_prefix`/`join_prefixes` reproduce the Go vector byte-for-byte.
    #[test]
    fn prefix_namespacing() {
        let v: serde_json::Value = serde_json::from_str(VECTOR).unwrap();

        let make_vm = v["make_prefix_vm"].as_str().unwrap();
        let make_a = v["make_prefix_a"].as_str().unwrap();
        let join_ab = v["join_prefix_make_a_b"].as_str().unwrap();

        assert_eq!(hex::encode(make_prefix(b"vm")), make_vm);
        assert_eq!(hex::encode(make_prefix(b"a")), make_a);

        let parent = make_prefix(b"a");
        assert_eq!(hex::encode(join_prefixes(&parent, b"b")), join_ab);
    }
}

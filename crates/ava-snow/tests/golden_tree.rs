// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::snowball_tree_vectors` — byte/behaviour-exact ports of the Go
//! `snow/consensus/snowball/tree_test.go` corpus. Each test drives a `Tree`
//! through `add`/`record_poll` and asserts `preference()`/`finalized()` and the
//! exact `Display` (Go `Tree.String()`) at each step.
//!
//! Provenance: pinned `avalanchego`, `snow/consensus/snowball/tree_test.go`.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::time::Duration;

use ava_snow::snowball::{Consensus, Parameters, SnowballFactory, Tree};
use ava_types::id::Id;
use ava_utils::bag::Bag;

/// The Go shared color ids (`consensus_test.go`): `ids.Empty.Prefix(i)`.
fn red() -> Id {
    Id::EMPTY.prefix(&[0])
}
fn blue() -> Id {
    Id::EMPTY.prefix(&[1])
}
fn green() -> Id {
    Id::EMPTY.prefix(&[2])
}

/// An id with the given first byte (matching Go `ids.ID{0xNN}`: byte 0 set, rest
/// zero). Bit 0 is the LSB of byte 0.
fn id_byte0(b: u8) -> Id {
    let mut bytes = [0u8; 32];
    bytes[0] = b;
    Id::from_slice(&bytes).unwrap()
}

fn params(k: u32, alpha_pref: u32, alpha_conf: u32, beta: u32) -> Parameters {
    Parameters {
        k,
        alpha_preference: alpha_pref,
        alpha_confidence: alpha_conf,
        beta,
        concurrent_repolls: 1,
        optimal_processing: 1,
        max_outstanding_items: 1,
        max_item_processing_time: Duration::from_nanos(1),
    }
}

fn bag_of(ids: &[Id]) -> Bag<Id> {
    Bag::of(ids.iter().copied())
}

const INITIAL_UNARY: &str =
    "SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [0, 256)";

#[test]
fn singleton() {
    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), red());
    assert!(!tree.finalized());

    let one_red = bag_of(&[red()]);
    assert!(tree.record_poll(&one_red));
    assert!(!tree.finalized());

    let empty = Bag::new();
    assert!(!tree.record_poll(&empty));
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_red));
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_red));
    assert_eq!(tree.preference(), red());
    assert!(tree.finalized());

    tree.add(blue());
    assert!(tree.finalized());

    // Already finalized: record_poll may return either value; preference stays.
    let one_blue = bag_of(&[blue()]);
    let _ = tree.record_poll(&one_blue);
    assert_eq!(tree.preference(), red());
    assert!(tree.finalized());
}

#[test]
fn record_unsuccessful_poll() {
    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 3), red());
    assert!(!tree.finalized());

    let one_red = bag_of(&[red()]);
    assert!(tree.record_poll(&one_red));

    tree.record_unsuccessful_poll();

    assert!(tree.record_poll(&one_red));
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_red));
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_red));
    assert_eq!(tree.preference(), red());
    assert!(tree.finalized());
}

#[test]
fn binary() {
    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), red());
    tree.add(blue());

    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    let one_blue = bag_of(&[blue()]);
    assert!(tree.record_poll(&one_blue));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    let one_red = bag_of(&[red()]);
    assert!(tree.record_poll(&one_red));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_blue));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&one_blue));
    assert_eq!(tree.preference(), blue());
    assert!(tree.finalized());
}

#[test]
fn last_binary() {
    let zero = Id::EMPTY;
    let mut one_bytes = [0u8; 32];
    one_bytes[31] = 0x80;
    let one = Id::from_slice(&one_bytes).unwrap();

    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), zero);
    tree.add(one);
    tree.add(one); // no-op

    let expected = "SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [0, 255)
    SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 255";
    assert_eq!(tree.to_string(), expected);
    assert_eq!(tree.preference(), zero);
    assert!(!tree.finalized());

    let one_bag = bag_of(&[one]);
    assert!(tree.record_poll(&one_bag));
    assert_eq!(tree.preference(), one);
    assert!(!tree.finalized());

    let expected = "SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [0, 255)
    SB(Preference = 1, PreferenceStrength[0] = 0, PreferenceStrength[1] = 1, SF(Confidence = [1], Finalized = false, SL(Preference = 1))) Bit = 255";
    assert_eq!(tree.to_string(), expected);

    assert!(tree.record_poll(&one_bag));
    assert_eq!(tree.preference(), one);
    assert!(tree.finalized());

    let expected = "SB(Preference = 1, PreferenceStrength[0] = 0, PreferenceStrength[1] = 2, SF(Confidence = [2], Finalized = true, SL(Preference = 1))) Bit = 255";
    assert_eq!(tree.to_string(), expected);
}

#[test]
fn first_binary() {
    let zero = Id::EMPTY;
    let one = id_byte0(0x01);

    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), zero);
    tree.add(one);

    let expected = "SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 0
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 256)
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 256)";
    assert_eq!(tree.to_string(), expected);
    assert_eq!(tree.preference(), zero);
    assert!(!tree.finalized());

    let one_bag = bag_of(&[one]);
    assert!(tree.record_poll(&one_bag));
    assert_eq!(tree.preference(), one);
    assert!(!tree.finalized());

    let expected = "SB(Preference = 1, PreferenceStrength[0] = 0, PreferenceStrength[1] = 1, SF(Confidence = [1], Finalized = false, SL(Preference = 1))) Bit = 0
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 256)
    SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [1, 256)";
    assert_eq!(tree.to_string(), expected);

    assert!(tree.record_poll(&one_bag));
    assert_eq!(tree.preference(), one);
    assert!(tree.finalized());

    let expected =
        "SB(PreferenceStrength = 2, SF(Confidence = [2], Finalized = true)) Bits = [1, 256)";
    assert_eq!(tree.to_string(), expected);
}

#[test]
fn add_decided_first_bit() {
    let zero = Id::EMPTY;
    let c1000 = id_byte0(0x01);
    let c1100 = id_byte0(0x03);
    let c0110 = id_byte0(0x06);

    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), zero);
    tree.add(c1000);
    tree.add(c1100);

    let expected = "SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 0
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 256)
    SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 1
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)";
    assert_eq!(tree.to_string(), expected);
    assert_eq!(tree.preference(), zero);
    assert!(!tree.finalized());

    assert!(tree.record_poll(&bag_of(&[c1000])));
    assert_eq!(tree.preference(), c1000);
    assert!(!tree.finalized());

    let expected = "SB(Preference = 1, PreferenceStrength[0] = 0, PreferenceStrength[1] = 1, SF(Confidence = [1], Finalized = false, SL(Preference = 1))) Bit = 0
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 256)
    SB(Preference = 0, PreferenceStrength[0] = 1, PreferenceStrength[1] = 0, SF(Confidence = [1], Finalized = false, SL(Preference = 0))) Bit = 1
        SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [2, 256)
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)";
    assert_eq!(tree.to_string(), expected);

    assert!(tree.record_poll(&bag_of(&[c1100])));
    assert_eq!(tree.preference(), c1000);
    assert!(!tree.finalized());

    let expected = "SB(Preference = 0, PreferenceStrength[0] = 1, PreferenceStrength[1] = 1, SF(Confidence = [1], Finalized = false, SL(Preference = 1))) Bit = 1
    SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [2, 256)
    SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [2, 256)";
    assert_eq!(tree.to_string(), expected);

    // Adding six should have no effect (first bit already decided).
    tree.add(c0110);
    assert_eq!(tree.to_string(), expected);
}

#[test]
fn trinary() {
    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), green());
    tree.add(red());
    tree.add(blue());

    assert_eq!(tree.preference(), green());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&bag_of(&[red()])));
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&bag_of(&[blue()])));
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    // Voting for a color can make a different color preferred (intended).
    assert!(tree.record_poll(&bag_of(&[green()])));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    // Red rejected here, so this is not a successful poll.
    assert!(!tree.record_poll(&bag_of(&[red()])));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&bag_of(&[green()])));
    assert_eq!(tree.preference(), green());
    assert!(!tree.finalized());
}

#[test]
fn transitive_reset() {
    let zero = id_byte0(0b0000_0000);
    let two = id_byte0(0b0000_0010);
    let eight = id_byte0(0b0000_1000);

    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), zero);
    tree.add(two);
    tree.add(eight);

    let expected = "SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [0, 1)
    SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 1
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 3)
            SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 3
                SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [4, 256)
                SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [4, 256)
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)";
    assert_eq!(tree.to_string(), expected);

    let zero_bag = bag_of(&[zero]);
    assert!(tree.record_poll(&zero_bag));

    let empty = Bag::new();
    assert!(!tree.record_poll(&empty));

    assert!(tree.record_poll(&zero_bag));
    assert!(tree.record_poll(&zero_bag));

    let expected =
        "SB(PreferenceStrength = 3, SF(Confidence = [2], Finalized = true)) Bits = [4, 256)";
    assert_eq!(tree.to_string(), expected);
    assert_eq!(tree.preference(), zero);
    assert!(tree.finalized());
}

#[test]
fn fine_grained() {
    let c0000 = id_byte0(0x00);
    let c1000 = id_byte0(0x01);
    let c1100 = id_byte0(0x03);
    let c0010 = id_byte0(0x04);

    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 2), c0000);
    assert_eq!(tree.to_string(), INITIAL_UNARY);
    assert_eq!(tree.preference(), c0000);
    assert!(!tree.finalized());

    tree.add(c1100);
    tree.add(c1000);
    tree.add(c0010);

    let expected = "SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 0
    SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [1, 2)
        SB(Preference = 0, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 0))) Bit = 2
            SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [3, 256)
            SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [3, 256)
    SB(Preference = 1, PreferenceStrength[0] = 0, PreferenceStrength[1] = 0, SF(Confidence = [0], Finalized = false, SL(Preference = 1))) Bit = 1
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)
        SB(PreferenceStrength = 0, SF(Confidence = [0], Finalized = false)) Bits = [2, 256)";
    assert_eq!(tree.to_string(), expected);

    assert!(tree.record_poll(&bag_of(&[c0000])));
    assert!(tree.record_poll(&bag_of(&[c0010])));

    let expected = "SB(Preference = 0, PreferenceStrength[0] = 1, PreferenceStrength[1] = 1, SF(Confidence = [1], Finalized = false, SL(Preference = 1))) Bit = 2
    SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [3, 256)
    SB(PreferenceStrength = 1, SF(Confidence = [1], Finalized = false)) Bits = [3, 256)";
    assert_eq!(tree.to_string(), expected);

    assert!(tree.record_poll(&bag_of(&[c0010])));
    let expected =
        "SB(PreferenceStrength = 2, SF(Confidence = [2], Finalized = true)) Bits = [3, 256)";
    assert_eq!(tree.to_string(), expected);
    assert_eq!(tree.preference(), c0010);
    assert!(tree.finalized());
}

#[test]
fn double_add() {
    let mut tree = Tree::new(SnowballFactory, params(1, 1, 1, 3), red());
    tree.add(red());
    assert_eq!(tree.to_string(), INITIAL_UNARY);
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());
}

#[test]
fn record_preference_poll_binary() {
    let mut tree = Tree::new(SnowballFactory, params(3, 2, 3, 2), red());
    tree.add(blue());
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    let three_blue = bag_of(&[blue(), blue(), blue()]);
    assert!(tree.record_poll(&three_blue));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    let two_red = bag_of(&[red(), red()]);
    assert!(tree.record_poll(&two_red));
    assert_eq!(tree.preference(), blue());
    assert!(!tree.finalized());

    let three_red = bag_of(&[red(), red(), red()]);
    assert!(tree.record_poll(&three_red));
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&three_red));
    assert_eq!(tree.preference(), red());
    assert!(tree.finalized());
}

#[test]
fn record_preference_poll_unary() {
    let mut tree = Tree::new(SnowballFactory, params(3, 2, 3, 2), red());
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    let two_red = bag_of(&[red(), red()]);
    assert!(tree.record_poll(&two_red));
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    tree.add(blue());

    let three_blue = bag_of(&[blue(), blue(), blue()]);
    assert!(tree.record_poll(&three_blue));
    assert_eq!(tree.preference(), red());
    assert!(!tree.finalized());

    assert!(tree.record_poll(&three_blue));
    assert_eq!(tree.preference(), blue());
    assert!(tree.finalized());
}

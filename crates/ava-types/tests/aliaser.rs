// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M0.7 — `Aliaser` bidirectional id<->alias map.

use ava_types::aliaser::Aliaser;
use ava_types::error::Error;
use ava_types::id::Id;

fn id(b: u8) -> Id {
    Id::from_slice(&[b; 32]).unwrap()
}

#[test]
fn aliaser_bidirectional() {
    let aliaser = Aliaser::new();
    let id1 = id(1);

    // An alias maps to exactly one id.
    aliaser.alias(id1, "x").unwrap();
    assert_eq!(aliaser.lookup("x").unwrap(), id1);

    // One id -> many aliases; first is primary.
    aliaser.alias(id1, "second").unwrap();
    assert_eq!(aliaser.aliases(id1), vec!["x".to_string(), "second".to_string()]);
    assert_eq!(aliaser.primary_alias(id1).unwrap(), "x");
}

#[test]
fn duplicate_alias_errors() {
    let aliaser = Aliaser::new();
    let id1 = id(1);
    let id2 = id(2);
    aliaser.alias(id1, "dup").unwrap();
    let err = aliaser.alias(id2, "dup").unwrap_err();
    assert!(matches!(err, Error::AliasAlreadyMapped(_)));
    // The original mapping is unchanged.
    assert_eq!(aliaser.lookup("dup").unwrap(), id1);
}

#[test]
fn lookup_missing_errors() {
    let aliaser = Aliaser::new();
    let err = aliaser.lookup("nope").unwrap_err();
    assert!(matches!(err, Error::NoIdWithAlias(_)));
}

#[test]
fn primary_alias_or_default_falls_back() {
    let aliaser = Aliaser::new();
    let id1 = id(1);
    // No alias yet -> falls back to the id string form.
    assert_eq!(aliaser.primary_alias_or_default(id1), id1.hex());
    aliaser.alias(id1, "p").unwrap();
    assert_eq!(aliaser.primary_alias_or_default(id1), "p");
}

#[test]
fn get_relevant_aliases_strips_self() {
    let aliaser = Aliaser::new();
    let id1 = id(1);
    // The self-alias (alias == id string form) is redundant and stripped.
    aliaser.alias(id1, &id1.hex()).unwrap();
    aliaser.alias(id1, "friendly").unwrap();

    let relevant = aliaser.get_relevant_aliases(&[id1]).unwrap();
    assert_eq!(relevant.get(&id1).unwrap(), &vec!["friendly".to_string()]);
}

#[test]
fn remove_aliases() {
    let aliaser = Aliaser::new();
    let id1 = id(1);
    aliaser.alias(id1, "a").unwrap();
    aliaser.alias(id1, "b").unwrap();
    aliaser.remove_aliases(id1);
    assert!(aliaser.lookup("a").is_err());
    assert!(aliaser.lookup("b").is_err());
    assert!(aliaser.aliases(id1).is_empty());
}

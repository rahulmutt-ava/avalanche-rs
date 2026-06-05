// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Stored-value encoding for `ava-archivedb` (port of `x/archivedb/value.go`).
//!
//! A stored database value is a single prefix byte followed by the user value:
//! `0x00 || value`. A *tombstone* (the result of a `Delete`) is stored as an
//! **empty** value (zero length). On read, a present prefix byte means the entry
//! was an insertion; an empty stored value means it was deleted.

/// Encodes a user `value` as a database value: a single `0x00` prefix byte
/// followed by the value bytes (`newDBValue`).
///
/// The prefix byte distinguishes a real (possibly empty) value from a tombstone,
/// which is stored as a zero-length value (see [`parse_db_value`]).
pub fn new_db_value(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len().saturating_add(1));
    out.push(0x00);
    out.extend_from_slice(value);
    out
}

/// Parses a stored database value, returning the user value and whether the
/// entry exists (`parseDBValue`).
///
/// An empty stored value is a tombstone: returns `(&[], false)`. Otherwise the
/// leading prefix byte is stripped and `(value, true)` is returned.
pub fn parse_db_value(db_value: &[u8]) -> (&[u8], bool) {
    match db_value.split_first() {
        Some((_prefix, rest)) => (rest, true),
        None => (&[], false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_value_roundtrip() {
        let encoded = new_db_value(b"bar");
        assert_eq!(encoded, b"\x00bar");
        let (value, exists) = parse_db_value(&encoded);
        assert!(exists);
        assert_eq!(value, b"bar");
    }

    #[test]
    fn empty_user_value_still_exists() {
        let encoded = new_db_value(b"");
        assert_eq!(encoded, b"\x00");
        let (value, exists) = parse_db_value(&encoded);
        assert!(exists);
        assert_eq!(value, b"");
    }

    #[test]
    fn tombstone_is_empty() {
        let (value, exists) = parse_db_value(&[]);
        assert!(!exists);
        assert_eq!(value, b"");
    }
}

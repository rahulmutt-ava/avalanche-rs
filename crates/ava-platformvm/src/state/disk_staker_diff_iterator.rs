// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Byte-exact key/value codecs for the staker weight-diff iterator
//! (`vms/platformvm/state/disk_staker_diff_iterator.go`, specs 08 §7.1).
//!
//! The P-Chain reconstructs historical validator sets by walking weight diffs
//! and public-key diffs **backward** from the current (last-accepted) height.
//! To make a forward, lexicographic key scan visit the **newest** height first,
//! the height is stored inverted: `inverse_height = u64::MAX - height` (Go bit-
//! flips with `^height`, which is the same value). All multi-byte integers are
//! big-endian.
//!
//! Two parallel key layouts index the same diffs:
//!
//! - **by-subnet** ([`marshal_diff_key_by_subnet_id`]):
//!   `[subnet_id(32)] ++ [inverse_height: u64 BE] ++ [node_id(20)]` — scanned
//!   with the subnet id as the iterator prefix to reconstruct a single subnet.
//! - **by-height** ([`marshal_diff_key_by_height`]):
//!   `[inverse_height: u64 BE] ++ [subnet_id(32)] ++ [node_id(20)]` — scanned
//!   without a prefix to reconstruct *all* subnets at once.
//!
//! The weight-diff **value** is `[is_negative: bool (1 byte)] ++ [weight: u64
//! BE]`. The public-key-diff value is the raw uncompressed BLS key bytes the
//! node *had before* the change (empty ⇒ the node had no key), so it has no
//! fixed-length codec here.

use ava_types::id::{ID_LEN, Id};
use ava_types::node_id::{NODE_ID_LEN, NodeId};

use crate::error::{Error, Result};

/// Size of a big-endian `u64` on the wire.
const U64_LEN: usize = 8;
/// Size of the `bool` flag prefixing a weight value.
const BOOL_LEN: usize = 1;

/// `startDiffKeyLength` — `[subnet_id] ++ [inverse_height]`, the iterator start
/// key (a prefix of [`marshal_diff_key_by_subnet_id`]).
pub const START_DIFF_KEY_LEN: usize = ID_LEN + U64_LEN;
/// `diffKeyLength` — the full by-subnet / by-height diff key length.
pub const DIFF_KEY_LEN: usize = START_DIFF_KEY_LEN + NODE_ID_LEN;
/// `weightValueLength` — `[is_negative] ++ [weight]`.
pub const WEIGHT_VALUE_LEN: usize = BOOL_LEN + U64_LEN;

/// A single validator weight change at a height (`state.ValidatorWeightDiff`).
///
/// `decrease` records the *direction* of the change at the height the diff was
/// written: `true` ⇒ the weight went down (so the prior weight was higher).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidatorWeightDiff {
    /// Whether the weight decreased at this height.
    pub decrease: bool,
    /// The magnitude of the change.
    pub amount: u64,
}

/// `packIterableHeight` — invert `height` so forward key order = decreasing
/// height. Go bit-flips with `^height`; bitwise NOT equals `u64::MAX - height`
/// for all inputs and avoids any arithmetic (so the same value round-trips
/// through [`height_from_inverse`]).
#[must_use]
pub const fn inverse_height(height: u64) -> u64 {
    !height
}

/// `unpackIterableHeight` — recover the height from its inverted form (the
/// involutive bitwise NOT).
#[must_use]
pub const fn height_from_inverse(inverse: u64) -> u64 {
    !inverse
}

/// `marshalStartDiffKeyBySubnetID` — the iterator start key for `subnet`'s diffs
/// at `height`: `[subnet_id(32)] ++ [inverse_height: u64 BE]`.
#[must_use]
pub fn marshal_start_diff_key_by_subnet_id(subnet_id: Id, height: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(START_DIFF_KEY_LEN);
    key.extend_from_slice(subnet_id.as_bytes());
    key.extend_from_slice(&inverse_height(height).to_be_bytes());
    key
}

/// `marshalStartDiffKeyByHeight` — the all-subnets iterator start key at
/// `height`: `[inverse_height: u64 BE]`.
#[must_use]
pub fn marshal_start_diff_key_by_height(height: u64) -> Vec<u8> {
    inverse_height(height).to_be_bytes().to_vec()
}

/// `marshalDiffKeyBySubnetID` — `[subnet_id(32)] ++ [inverse_height: u64 BE] ++
/// [node_id(20)]`.
#[must_use]
pub fn marshal_diff_key_by_subnet_id(subnet_id: Id, height: u64, node_id: NodeId) -> Vec<u8> {
    let mut key = Vec::with_capacity(DIFF_KEY_LEN);
    key.extend_from_slice(subnet_id.as_bytes());
    key.extend_from_slice(&inverse_height(height).to_be_bytes());
    key.extend_from_slice(node_id.as_bytes());
    key
}

/// `marshalDiffKeyByHeight` — `[inverse_height: u64 BE] ++ [subnet_id(32)] ++
/// [node_id(20)]`.
#[must_use]
pub fn marshal_diff_key_by_height(height: u64, subnet_id: Id, node_id: NodeId) -> Vec<u8> {
    let mut key = Vec::with_capacity(DIFF_KEY_LEN);
    key.extend_from_slice(&inverse_height(height).to_be_bytes());
    key.extend_from_slice(subnet_id.as_bytes());
    key.extend_from_slice(node_id.as_bytes());
    key
}

/// `unmarshalDiffKeyBySubnetID` — `(subnet_id, height, node_id)`.
///
/// # Errors
/// [`Error::Codec`] if `key` is not exactly [`DIFF_KEY_LEN`] bytes.
pub fn unmarshal_diff_key_by_subnet_id(key: &[u8]) -> Result<(Id, u64, NodeId)> {
    if key.len() != DIFF_KEY_LEN {
        return Err(bad_key_len());
    }
    let subnet_id = Id::from_slice(slice(key, 0, ID_LEN)?).map_err(|_| bad_key_len())?;
    let inverse = read_u64_be(slice(key, ID_LEN, START_DIFF_KEY_LEN)?)?;
    let node_id = NodeId::from_slice(slice(key, START_DIFF_KEY_LEN, DIFF_KEY_LEN)?)
        .map_err(|_| bad_key_len())?;
    Ok((subnet_id, height_from_inverse(inverse), node_id))
}

/// `unmarshalDiffKeyByHeight` — `(height, subnet_id, node_id)`.
///
/// # Errors
/// [`Error::Codec`] if `key` is not exactly [`DIFF_KEY_LEN`] bytes.
pub fn unmarshal_diff_key_by_height(key: &[u8]) -> Result<(u64, Id, NodeId)> {
    if key.len() != DIFF_KEY_LEN {
        return Err(bad_key_len());
    }
    let inverse = read_u64_be(slice(key, 0, U64_LEN)?)?;
    let subnet_id =
        Id::from_slice(slice(key, U64_LEN, START_DIFF_KEY_LEN)?).map_err(|_| bad_key_len())?;
    let node_id = NodeId::from_slice(slice(key, START_DIFF_KEY_LEN, DIFF_KEY_LEN)?)
        .map_err(|_| bad_key_len())?;
    Ok((height_from_inverse(inverse), subnet_id, node_id))
}

/// `marshalWeightDiff` — `[is_negative: bool (1 byte)] ++ [weight: u64 BE]`.
#[must_use]
pub fn marshal_weight_diff(diff: &ValidatorWeightDiff) -> Vec<u8> {
    let mut value = Vec::with_capacity(WEIGHT_VALUE_LEN);
    value.push(u8::from(diff.decrease));
    value.extend_from_slice(&diff.amount.to_be_bytes());
    value
}

/// `unmarshalWeightDiff` — inverse of [`marshal_weight_diff`].
///
/// # Errors
/// [`Error::Codec`] if `value` is not exactly [`WEIGHT_VALUE_LEN`] bytes.
pub fn unmarshal_weight_diff(value: &[u8]) -> Result<ValidatorWeightDiff> {
    if value.len() != WEIGHT_VALUE_LEN {
        return Err(bad_value_len());
    }
    let flag = value.first().ok_or_else(bad_value_len)?;
    let decrease = *flag != 0;
    let amount = read_u64_be(value.get(BOOL_LEN..).ok_or_else(bad_value_len)?)?;
    Ok(ValidatorWeightDiff { decrease, amount })
}

/// Borrows `data[start..end]`, mapping an out-of-range access to a key-length
/// error (callers length-check first, so this is unreachable in practice but
/// keeps the code panic-free).
fn slice(data: &[u8], start: usize, end: usize) -> Result<&[u8]> {
    data.get(start..end).ok_or_else(bad_key_len)
}

/// Reads a big-endian `u64` from an exactly-8-byte slice.
fn read_u64_be(b: &[u8]) -> Result<u64> {
    let arr: [u8; U64_LEN] = b.try_into().map_err(|_| bad_key_len())?;
    Ok(u64::from_be_bytes(arr))
}

const fn bad_key_len() -> Error {
    Error::UnexpectedDiffKeyLength
}

const fn bad_value_len() -> Error {
    Error::UnexpectedWeightValueLength
}

#[cfg(test)]
mod golden {
    //! `weight_diff_key_layout` — byte-exact key/value layouts.
    //!
    //! Oracle: Go `disk_staker_diff_iterator.go` (`marshalDiffKeyBySubnetID`,
    //! `marshalDiffKeyByHeight`, `marshalWeightDiff` + `packIterableHeight`).
    //! Hand-built expected bytes pin the protocol independent of the prefixdb
    //! hashing that wraps these keys on disk.

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn weight_diff_key_layout() {
        let subnet_id = Id::from([0x11; 32]);
        let node_id = NodeId::from([0x22; 20]);
        // height chosen so inverse_height has distinctive, asymmetric bytes:
        // u64::MAX - 0x0102_0304_0506_0708 = 0xFEFD_FCFB_FAF9_F8F7.
        let height: u64 = 0x0102_0304_0506_0708;
        let inverse: [u8; 8] = [0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0xF9, 0xF8, 0xF7];
        assert_eq!(inverse_height(height).to_be_bytes(), inverse);

        // by-subnet key: [subnet_id(32)] ++ [inverse_height(8)] ++ [node_id(20)]
        let mut expected_by_subnet = Vec::new();
        expected_by_subnet.extend_from_slice(&[0x11; 32]);
        expected_by_subnet.extend_from_slice(&inverse);
        expected_by_subnet.extend_from_slice(&[0x22; 20]);
        let by_subnet = marshal_diff_key_by_subnet_id(subnet_id, height, node_id);
        assert_eq!(by_subnet, expected_by_subnet);
        assert_eq!(by_subnet.len(), DIFF_KEY_LEN);

        // by-height key: [inverse_height(8)] ++ [subnet_id(32)] ++ [node_id(20)]
        let mut expected_by_height = Vec::new();
        expected_by_height.extend_from_slice(&inverse);
        expected_by_height.extend_from_slice(&[0x11; 32]);
        expected_by_height.extend_from_slice(&[0x22; 20]);
        let by_height = marshal_diff_key_by_height(height, subnet_id, node_id);
        assert_eq!(by_height, expected_by_height);
        assert_eq!(by_height.len(), DIFF_KEY_LEN);

        // start keys are prefixes of the full keys with the same args.
        let start_by_subnet = marshal_start_diff_key_by_subnet_id(subnet_id, height);
        assert_eq!(
            Some(start_by_subnet.as_slice()),
            by_subnet.get(..START_DIFF_KEY_LEN),
        );
        let start_by_height = marshal_start_diff_key_by_height(height);
        assert_eq!(Some(start_by_height.as_slice()), by_height.get(..U64_LEN));

        // value: [is_negative: bool (1 byte)] ++ [weight: u64 BE]
        let neg = ValidatorWeightDiff {
            decrease: true,
            amount: 0x0102_0304_0506_0708,
        };
        assert_eq!(
            marshal_weight_diff(&neg),
            vec![0x01, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        );
        let pos = ValidatorWeightDiff {
            decrease: false,
            amount: 1,
        };
        assert_eq!(
            marshal_weight_diff(&pos),
            vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
        );

        // round-trips
        let (s, h, n) = unmarshal_diff_key_by_subnet_id(&by_subnet).expect("by-subnet");
        assert_eq!((s, h, n), (subnet_id, height, node_id));
        let (h2, s2, n2) = unmarshal_diff_key_by_height(&by_height).expect("by-height");
        assert_eq!((h2, s2, n2), (height, subnet_id, node_id));
        assert_eq!(
            unmarshal_weight_diff(&marshal_weight_diff(&neg)).expect("v"),
            neg
        );
    }
}

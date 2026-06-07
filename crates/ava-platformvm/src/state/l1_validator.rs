// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ACP-77 L1 validator on-disk record (`state/l1_validator.go`, specs 08 §3.4).
//!
//! [`L1Validator`] is the `ValidationID`-keyed value persisted for every ACP-77
//! L1 validator. The `ValidationID` itself is **not** serialized (it is the DB
//! key); the value carries the constant identity fields (`SubnetID`, `NodeID`,
//! `PublicKey`, `RemainingBalanceOwner`, `DeactivationOwner`, `StartTime`) plus
//! the mutable accounting fields (`Weight`, `MinNonce`, `EndAccumulatedFee`).
//!
//! Unlike the validator-metadata record (§3.4), the L1 validator value is
//! marshalled with the **`GenesisCodec`** (an `i32::MAX`-max-slice manager), not
//! the dedicated metadata codec, mirroring Go `block.GenesisCodec`.
//!
//! The canonical [`Ord`]ering — lower [`EndAccumulatedFee`](L1Validator::end_accumulated_fee)
//! first, ties broken by [`ValidationID`](L1Validator::validation_id) — is what
//! drives the active-validator iterator that charges the ACP-77 continuous fee
//! (consumed later by M4.19). Only the ordering and [`is_active`](L1Validator::is_active)
//! live here; the iterator/store does not.

use ava_codec::AvaCodec;
use ava_codec::error::Result;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::CODEC_VERSION;

/// An ACP-77 L1 validator record (port of Go `state.L1Validator`).
///
/// For a given `ValidationID`, the fields `SubnetID`, `NodeID`, `PublicKey`,
/// `RemainingBalanceOwner`, `DeactivationOwner`, and `StartTime` are expected to
/// be constant; see [`immutable_fields_are_unmodified`](L1Validator::immutable_fields_are_unmodified).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct L1Validator {
    /// `ValidationID` — **not serialized**; it is the DB key, supplied out of
    /// band. Kept here so the in-memory record is self-describing.
    pub validation_id: Id,

    /// `SubnetID` — the subnet this validator secures (constant).
    #[codec]
    pub subnet_id: Id,
    /// `NodeID` — the validating node (constant).
    #[codec]
    pub node_id: NodeId,

    /// `PublicKey` — the uncompressed BLS public key of the validator, stored as
    /// opaque length-prefixed bytes (constant). Guaranteed to be populated.
    #[codec]
    pub public_key: Vec<u8>,

    /// `RemainingBalanceOwner` — the serialized `fx.Owner` used when returning
    /// the validator's balance after removing accrued fees, stored as opaque
    /// length-prefixed bytes (constant).
    #[codec]
    pub remaining_balance_owner: Vec<u8>,

    /// `DeactivationOwner` — the serialized `fx.Owner` that can manually
    /// deactivate the validator, stored as opaque length-prefixed bytes
    /// (constant).
    #[codec]
    pub deactivation_owner: Vec<u8>,

    /// `StartTime` — the Unix timestamp, in seconds, when this validator was
    /// added to the set (constant).
    #[codec]
    pub start_time: u64,

    /// `Weight` — this validator's weight. May be updated when `MinNonce` is
    /// increased. A weight of `0` removes the validator from the set.
    #[codec]
    pub weight: u64,

    /// `MinNonce` — the smallest nonce that can be used to modify this
    /// validator's weight. Initially `0`, set to one higher than the last nonce
    /// used. `MaxUint64` is only valid when the weight is being set to `0`.
    #[codec]
    pub min_nonce: u64,

    /// `EndAccumulatedFee` — the accumulated fee-per-validator at which this
    /// validator must be deactivated. A value of `0` means the validator is
    /// inactive.
    #[codec]
    pub end_accumulated_fee: u64,
}

impl L1Validator {
    /// Marshals the serialized fields under the [`GenesisCodec`] (Go
    /// `putL1Validator` → `block.GenesisCodec.Marshal(block.CodecVersion, v)`).
    /// The 2-byte codec-version prefix is prepended; the `ValidationID` is *not*
    /// written.
    ///
    /// [`GenesisCodec`]: crate::block::codec::GenesisCodec
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] only on a codec write failure
    /// (cannot occur with a growable buffer).
    pub fn marshal(&self) -> Result<Vec<u8>> {
        crate::block::codec::GenesisCodec().marshal(CODEC_VERSION, self)
    }

    /// Unmarshals the serialized fields from `bytes` under the [`GenesisCodec`]
    /// (Go `getL1Validator` → `block.GenesisCodec.Unmarshal`). The decoded record
    /// has [`validation_id`](L1Validator::validation_id) left as
    /// [`Id::EMPTY`](ava_types::id::Id::EMPTY); the caller supplies it from the
    /// DB key.
    ///
    /// [`GenesisCodec`]: crate::block::codec::GenesisCodec
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] on an unknown version, trailing
    /// bytes, or a short read.
    pub fn unmarshal(bytes: &[u8]) -> Result<Self> {
        let mut v = Self::default();
        crate::block::codec::GenesisCodec().unmarshal(bytes, &mut v)?;
        Ok(v)
    }

    /// Canonical ordering by `(EndAccumulatedFee, ValidationID)` — lower
    /// `EndAccumulatedFee` first, ties broken by `ValidationID` (Go
    /// `L1Validator.Compare`). This is the order the active-validator iterator
    /// walks for continuous-fee charging.
    #[must_use]
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.end_accumulated_fee
            .cmp(&other.end_accumulated_fee)
            .then_with(|| self.validation_id.cmp(&other.validation_id))
    }

    /// Whether this validator is active (Go `L1Validator.IsActive`): it has a
    /// non-zero weight **and** a non-zero `EndAccumulatedFee`. An inactive
    /// validator (`EndAccumulatedFee == 0`) does not accrue continuous fees.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.weight != 0 && self.end_accumulated_fee != 0
    }

    /// Whether this validator has been removed from the set (Go
    /// `L1Validator.isDeleted`): its weight has been set to `0`.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        self.weight == 0
    }

    /// Whether the constant identity fields are unchanged relative to `other`
    /// (Go `L1Validator.immutableFieldsAreUnmodified`).
    ///
    /// Two versions of a validator are valid either because the `ValidationID`
    /// differs (they describe different validators) or because none of the
    /// constant fields (`SubnetID`, `NodeID`, `PublicKey`,
    /// `RemainingBalanceOwner`, `DeactivationOwner`, `StartTime`) changed. The
    /// mutable fields (`Weight`, `MinNonce`, `EndAccumulatedFee`) are ignored.
    #[must_use]
    pub fn immutable_fields_are_unmodified(&self, other: &Self) -> bool {
        if self.validation_id != other.validation_id {
            return true;
        }
        self.subnet_id == other.subnet_id
            && self.node_id == other.node_id
            && self.public_key == other.public_key
            && self.remaining_balance_owner == other.remaining_balance_owner
            && self.deactivation_owner == other.deactivation_owner
            && self.start_time == other.start_time
    }
}

impl PartialOrd for L1Validator {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for L1Validator {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.compare(other)
    }
}

#[cfg(test)]
mod golden {
    //! `l1_validator_codec` — byte-exact GenesisCodec round-trip.
    //!
    //! Provenance: the Go test `vms/platformvm/state/l1_validator_test.go`
    //! (`TestPutL1Validator`) marshals `newL1Validator()` with
    //! `block.GenesisCodec.Marshal(block.CodecVersion, ...)` and asserts the
    //! stored bytes round-trip. The Go test uses random field values (no
    //! committed `expectedBytes`), so we pin a deterministic record and assert
    //! against the fully-specified linear-codec wire bytes: a 2-byte version
    //! prefix, the two 32/20-byte ids, three `u32`-length-prefixed byte slices,
    //! and four big-endian `u64`s — in serialize-tag order (ValidationID
    //! omitted).

    use pretty_assertions::assert_eq;

    use super::*;

    fn fixture() -> L1Validator {
        L1Validator {
            // Not serialized; chosen distinct from subnet_id to exercise that.
            validation_id: Id::from([0xAA; 32]),
            subnet_id: Id::from([0x11; 32]),
            node_id: NodeId::from([0x22; 20]),
            public_key: vec![0x33, 0x44, 0x55],
            remaining_balance_owner: vec![0x66, 0x77],
            deactivation_owner: vec![0x88],
            start_time: 0x0102_0304_0506_0708,
            weight: 0x1112_1314_1516_1718,
            min_nonce: 0x2122_2324_2526_2728,
            end_accumulated_fee: 0x3132_3334_3536_3738,
        }
    }

    /// The byte-exact GenesisCodec encoding of [`fixture`], built by hand from
    /// the linear-codec wire rules (the Go oracle's encoding).
    const EXPECTED: &[u8] = &[
        // codec version (CODEC_VERSION = 0)
        0x00, 0x00, //
        // subnet_id (32 bytes)
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, //
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, //
        // node_id (20 bytes)
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, //
        0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, //
        0x22, 0x22, 0x22, 0x22, //
        // public_key (u32 len = 3, then bytes)
        0x00, 0x00, 0x00, 0x03, 0x33, 0x44, 0x55, //
        // remaining_balance_owner (u32 len = 2, then bytes)
        0x00, 0x00, 0x00, 0x02, 0x66, 0x77, //
        // deactivation_owner (u32 len = 1, then bytes)
        0x00, 0x00, 0x00, 0x01, 0x88, //
        // start_time (u64 BE)
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, //
        // weight (u64 BE)
        0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, //
        // min_nonce (u64 BE)
        0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, //
        // end_accumulated_fee (u64 BE)
        0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
    ];

    #[test]
    fn l1_validator_codec() {
        let v = fixture();

        // Byte-exact: marshal matches the hand-built wire bytes.
        let bytes = v.marshal().expect("marshal");
        assert_eq!(bytes.as_slice(), EXPECTED);

        // Structural round-trip: decode discards ValidationID (DB key), so
        // compare against the fixture with validation_id cleared.
        let decoded = L1Validator::unmarshal(&bytes).expect("unmarshal");
        let mut want = fixture();
        want.validation_id = Id::EMPTY;
        assert_eq!(decoded, want);

        // encode(decode(bytes)) == bytes.
        assert_eq!(decoded.marshal().expect("re-marshal").as_slice(), EXPECTED);
    }
}

#[cfg(test)]
mod prop {
    //! `l1_validator_order` — ordering and activeness invariants.
    //!
    //! Mirrors the Go `TestL1Validator_Compare` semantics (order by
    //! `EndAccumulatedFee`, ties by `ValidationID`) and `L1Validator.IsActive`
    //! (`weight != 0 && end_accumulated_fee != 0`) as proptest invariants.

    use std::cmp::Ordering;

    use proptest::prelude::*;

    use super::*;

    prop_compose! {
        fn arb_validator()(
            validation_id in any::<[u8; 32]>(),
            weight in any::<u64>(),
            end_accumulated_fee in any::<u64>(),
        ) -> L1Validator {
            L1Validator {
                validation_id: Id::from(validation_id),
                weight,
                end_accumulated_fee,
                ..L1Validator::default()
            }
        }
    }

    proptest! {
        #[test]
        fn l1_validator_order(a in arb_validator(), b in arb_validator()) {
            // Ordering is by (end_accumulated_fee, validation_id).
            let expected = a
                .end_accumulated_fee
                .cmp(&b.end_accumulated_fee)
                .then_with(|| a.validation_id.cmp(&b.validation_id));
            prop_assert_eq!(a.cmp(&b), expected);

            // Antisymmetry of compare.
            prop_assert_eq!(a.compare(&b), b.compare(&a).reverse());

            // is_active() == (weight != 0 && end_accumulated_fee != 0).
            prop_assert_eq!(
                a.is_active(),
                a.weight != 0 && a.end_accumulated_fee != 0
            );
            // is_deleted() == (weight == 0).
            prop_assert_eq!(a.is_deleted(), a.weight == 0);
        }

        #[test]
        fn l1_validator_order_total(
            a in arb_validator(),
            b in arb_validator(),
            c in arb_validator(),
        ) {
            // Transitivity of <=.
            if a.cmp(&b) != Ordering::Greater && b.cmp(&c) != Ordering::Greater {
                prop_assert_ne!(a.cmp(&c), Ordering::Greater);
            }
        }
    }
}

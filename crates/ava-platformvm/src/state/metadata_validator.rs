// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Validator metadata (`state/metadata_validator.go`, specs 08 §3.4).
//!
//! [`ValidatorMetadata`] is the on-disk record for a (validator, subnet) pair:
//! uptime, potential/accrued rewards, and the ACP-236 auto-renew fields. It is
//! marshalled with the dedicated [`crate::state::metadata_codec`] (versions
//! v0/v1/v2 append fields), and parsed via [`parse_validator_metadata`], which
//! reproduces the length-based legacy fallbacks that pre-date the codec
//! versions.

use ava_codec::error::CodecError;
use ava_codec::packer::Packer;
use ava_types::id::Id;

use crate::state::metadata_codec::{
    self, CODEC_VERSION_1, CODEC_VERSION_2, LONG_LEN, PRE_DELEGATEE_REWARD_SIZE,
};

/// The persisted metadata for a single (validator, subnet) pair.
///
/// Port of Go `validatorMetadata`. Fields are gated by metadata codec version;
/// see the per-field docs for the version that introduced them. `tx_id` is **not
/// serialized** (it is the DB key, supplied out of band).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidatorMetadata {
    // ---- v0 ----
    /// `UpDuration` (v0) — accumulated connected time, in nanoseconds (Go
    /// `time.Duration`).
    pub up_duration: u64,
    /// `LastUpdated` (v0) — Unix time, in seconds, of the last uptime update.
    pub last_updated: u64,
    /// `PotentialReward` (v0) — the validation reward minted on a successful
    /// reward tx.
    pub potential_reward: u64,
    /// `PotentialDelegateeReward` (v0) — the accrued delegatee reward for the
    /// current cycle.
    pub potential_delegatee_reward: u64,

    // ---- v1 ----
    /// `StakerStartTime` (v1) — Unix time, in seconds, the staker's current cycle
    /// began.
    pub staker_start_time: u64,

    // ---- v2 (ACP-236 / Helicon auto-renew) ----
    /// `AccruedValidationRewards` (v2) — sum of validation rewards restaked from
    /// previous cycles.
    pub accrued_validation_rewards: u64,
    /// `AccruedDelegateeRewards` (v2) — sum of delegatee rewards restaked from
    /// previous cycles.
    pub accrued_delegatee_rewards: u64,
    /// `AutoCompoundRewardShares` (v2) — percentage of rewards to restake at
    /// cycle end.
    pub auto_compound_reward_shares: u32,
    /// `NextPeriod` (v2) — the next validation cycle duration, in seconds.
    pub next_period: u64,
    /// `StakerEndTime` (v2) — Unix time, in seconds, the current cycle ends.
    pub staker_end_time: u64,

    /// `txID` — **not serialized**; the DB key for this record.
    pub tx_id: Id,
}

/// `state.StakingInfo` — the mutable validator data the auto-renew txs update
/// (`state/metadata_validator.go`, specs 08 §3.4). A small projection of
/// [`ValidatorMetadata`]'s mutable fields, get/set as a unit via the
/// [`Chain`](crate::state::chain::Chain) `staking_info` accessors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StakingInfo {
    /// `DelegateeReward` — the delegatee reward accrued during the current cycle.
    pub delegatee_reward: u64,
    /// `AccruedValidationRewards` — sum of validation rewards restaked from
    /// previous cycles.
    pub accrued_validation_rewards: u64,
    /// `AccruedDelegateeRewards` — sum of delegatee rewards restaked from previous
    /// cycles.
    pub accrued_delegatee_rewards: u64,
    /// `AutoCompoundRewardShares` — percentage of rewards to restake at cycle end.
    pub auto_compound_reward_shares: u32,
    /// `NextPeriod` — the next validation cycle duration, in seconds.
    pub next_period: u64,
}

impl ValidatorMetadata {
    /// The auto-renewed effective weight: `tx_weight + accrued_validation_rewards
    /// + accrued_delegatee_rewards` (specs 08 §3.4).
    ///
    /// Returns `None` on `u64` overflow (the Go node treats this as unreachable;
    /// the Rust port surfaces it rather than wrapping).
    #[must_use]
    pub fn effective_weight(&self, tx_weight: u64) -> Option<u64> {
        tx_weight
            .checked_add(self.accrued_validation_rewards)?
            .checked_add(self.accrued_delegatee_rewards)
    }

    /// Marshals the metadata under `version` (one of `CODEC_VERSION_0/1/2`),
    /// prepending the 2-byte version prefix. Mirrors `MetadataCodec.Marshal`.
    ///
    /// # Errors
    /// [`CodecError::UnknownVersion`] for an unregistered version, or a wrapped
    /// [`ava_codec::error::PackerError`] on a write failure (cannot occur with a
    /// growable buffer).
    pub fn marshal(&self, version: u16) -> Result<Vec<u8>, CodecError> {
        if !metadata_codec::is_registered_version(version) {
            return Err(CodecError::UnknownVersion);
        }
        let mut p = Packer::with_max_size(usize::MAX);
        p.pack_u16(version);
        self.marshal_fields(version, &mut p);
        metadata_codec::packer_result(&p)?;
        Ok(p.into_bytes())
    }

    /// Appends the `vN:"true"` fields gated `<= version`, in declaration order.
    fn marshal_fields(&self, version: u16, p: &mut Packer) {
        // v0
        p.pack_u64(self.up_duration);
        p.pack_u64(self.last_updated);
        p.pack_u64(self.potential_reward);
        p.pack_u64(self.potential_delegatee_reward);
        if version >= CODEC_VERSION_1 {
            p.pack_u64(self.staker_start_time);
        }
        if version >= CODEC_VERSION_2 {
            p.pack_u64(self.accrued_validation_rewards);
            p.pack_u64(self.accrued_delegatee_rewards);
            p.pack_u32(self.auto_compound_reward_shares);
            p.pack_u64(self.next_period);
            p.pack_u64(self.staker_end_time);
        }
    }

    /// Unmarshals a full version-prefixed record (`MetadataCodec.Unmarshal`):
    /// reads the 2-byte version, the version-gated fields, then enforces the
    /// mandatory trailing-byte ([`CodecError::ExtraSpace`]) check.
    ///
    /// # Errors
    /// [`CodecError::UnknownVersion`] for an unregistered version,
    /// [`CodecError::ExtraSpace`] on trailing bytes, or a wrapped packer error on
    /// a short read.
    fn unmarshal(bytes: &[u8]) -> Result<Self, CodecError> {
        let mut p = Packer::new_read(bytes);
        let version = p.unpack_u16();
        metadata_codec::packer_result(&p)?;
        if !metadata_codec::is_registered_version(version) {
            return Err(CodecError::UnknownVersion);
        }
        let md = Self::unmarshal_fields(version, &mut p)?;
        metadata_codec::packer_result(&p)?;
        if p.offset() != bytes.len() {
            return Err(CodecError::ExtraSpace);
        }
        Ok(md)
    }

    /// Reads the `vN:"true"` fields gated `<= version` from `p` (no version
    /// prefix, no trailing-byte check).
    fn unmarshal_fields(version: u16, p: &mut Packer) -> Result<Self, CodecError> {
        let mut md = Self {
            up_duration: p.unpack_u64(),
            last_updated: p.unpack_u64(),
            potential_reward: p.unpack_u64(),
            potential_delegatee_reward: p.unpack_u64(),
            ..Self::default()
        };
        if version >= CODEC_VERSION_1 {
            md.staker_start_time = p.unpack_u64();
        }
        if version >= CODEC_VERSION_2 {
            md.accrued_validation_rewards = p.unpack_u64();
            md.accrued_delegatee_rewards = p.unpack_u64();
            md.auto_compound_reward_shares = p.unpack_u32();
            md.next_period = p.unpack_u64();
            md.staker_end_time = p.unpack_u64();
        }
        metadata_codec::packer_result(p)?;
        Ok(md)
    }
}

/// Parses a stored validator-metadata record, reproducing the length-based
/// legacy fallbacks that pre-date the codec versions (Go `parseValidatorMetadata`).
///
/// Permissioned validators originally wrote `nil`; Banff added the potential
/// reward; Cortina added the potential delegatee reward; the modern codec writes
/// every field. The branch is selected by the byte length **before** any codec
/// decode:
///
/// - `0` bytes → nothing was stored (all-zero metadata).
/// - [`LONG_LEN`] (8) bytes → only the potential reward (raw big-endian `u64`,
///   *no* version prefix).
/// - [`PRE_DELEGATEE_REWARD_SIZE`] (`VERSION_SIZE + 3*8`) bytes → uptime +
///   potential reward, decoded as the legacy `preDelegateeRewardMetadata`.
/// - otherwise → a full version-prefixed [`ValidatorMetadata`] record.
///
/// # Errors
/// Returns a [`CodecError`] if the full-codec branch fails (unknown version,
/// trailing bytes, or a short read).
pub fn parse_validator_metadata(bytes: &[u8]) -> Result<ValidatorMetadata, CodecError> {
    match bytes.len() {
        0 => Ok(ValidatorMetadata::default()),
        LONG_LEN => {
            // Only the potential reward was stored, as a raw u64 (no version).
            let mut p = Packer::new_read(bytes);
            let potential_reward = p.unpack_u64();
            metadata_codec::packer_result(&p)?;
            Ok(ValidatorMetadata {
                potential_reward,
                ..ValidatorMetadata::default()
            })
        }
        PRE_DELEGATEE_REWARD_SIZE => {
            // Uptime + potential reward, but no potential delegatee reward. The
            // record is version-prefixed (v0) with three u64 fields.
            let mut p = Packer::new_read(bytes);
            let version = p.unpack_u16();
            metadata_codec::packer_result(&p)?;
            if !metadata_codec::is_registered_version(version) {
                return Err(CodecError::UnknownVersion);
            }
            let up_duration = p.unpack_u64();
            let last_updated = p.unpack_u64();
            let potential_reward = p.unpack_u64();
            metadata_codec::packer_result(&p)?;
            if p.offset() != bytes.len() {
                return Err(CodecError::ExtraSpace);
            }
            Ok(ValidatorMetadata {
                up_duration,
                last_updated,
                potential_reward,
                ..ValidatorMetadata::default()
            })
        }
        _ => ValidatorMetadata::unmarshal(bytes),
    }
}

#[cfg(test)]
mod golden {
    //! `metadata_codec_v2` — round-trip + parse goldens.
    //!
    //! Provenance: the byte vectors are ported verbatim from the Go test
    //! `vms/platformvm/state/metadata_validator_test.go`
    //! (`TestParseValidatorMetadata`). The same little tables drive the
    //! length-based fallback cases and the v0/v1/v2 full-codec cases.

    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::state::metadata_codec::CODEC_VERSION_0;

    /// v0 record bytes from the Go "uptime + potential reward + potential
    /// delegatee reward" case.
    const V0_BYTES: &[u8] = &[
        // codec version
        0x00, 0x00, //
        // up duration
        0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x8D, 0x80, //
        // last updated
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0xBB, 0xA0, //
        // potential reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0, //
        // potential delegatee reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4E, 0x20,
    ];

    /// v1 record bytes (adds staker start time).
    const V1_BYTES: &[u8] = &[
        0x00, 0x01, // codec version
        0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x8D, 0x80, // up duration
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0xBB, 0xA0, // last updated
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0, // potential reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4E, 0x20, // potential delegatee reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x93, 0xE0, // staker start time
    ];

    /// v2 record bytes (adds the five auto-renew fields).
    const V2_BYTES: &[u8] = &[
        0x00, 0x02, // codec version
        0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x8D, 0x80, // up duration
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0xBB, 0xA0, // last updated
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0, // potential reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4E, 0x20, // potential delegatee reward
        0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x93, 0xE0, // staker start time
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xE8, // accrued validation rewards
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xF4, // accrued delegatee rewards
        0x00, 0x04, 0x93, 0xE0, // auto compound reward shares
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x51, 0x80, // next period
        0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x1A, 0x80, // staker end time
    ];

    fn v0_value() -> ValidatorMetadata {
        ValidatorMetadata {
            up_duration: 6_000_000,
            last_updated: 900_000,
            potential_reward: 100_000,
            potential_delegatee_reward: 20_000,
            ..ValidatorMetadata::default()
        }
    }

    fn v1_value() -> ValidatorMetadata {
        ValidatorMetadata {
            staker_start_time: 300_000,
            ..v0_value()
        }
    }

    fn v2_value() -> ValidatorMetadata {
        ValidatorMetadata {
            accrued_validation_rewards: 1_000,
            accrued_delegatee_rewards: 500,
            auto_compound_reward_shares: 300_000,
            next_period: 86_400,
            staker_end_time: 400_000,
            ..v1_value()
        }
    }

    #[test]
    fn metadata_codec_v2_roundtrip_v0() {
        let want = v0_value();
        assert_eq!(want.marshal(CODEC_VERSION_0).expect("marshal"), V0_BYTES);
        assert_eq!(parse_validator_metadata(V0_BYTES).expect("parse"), want);
    }

    #[test]
    fn metadata_codec_v2_roundtrip_v1() {
        let want = v1_value();
        assert_eq!(want.marshal(CODEC_VERSION_1).expect("marshal"), V1_BYTES);
        assert_eq!(parse_validator_metadata(V1_BYTES).expect("parse"), want);
    }

    #[test]
    fn metadata_codec_v2_roundtrip_v2() {
        let want = v2_value();
        assert_eq!(want.marshal(CODEC_VERSION_2).expect("marshal"), V2_BYTES);
        assert_eq!(parse_validator_metadata(V2_BYTES).expect("parse"), want);
    }

    #[test]
    fn metadata_codec_v2_fallback_nil() {
        // 0 bytes: nothing was stored.
        assert_eq!(
            parse_validator_metadata(&[]).expect("parse"),
            ValidatorMetadata::default()
        );
    }

    #[test]
    fn metadata_codec_v2_fallback_potential_reward_only() {
        // 8 bytes: raw u64 potential reward, no version prefix. 0x0186A0 = 100000.
        let bytes: &[u8] = &[0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0];
        assert_eq!(bytes.len(), LONG_LEN);
        assert_eq!(
            parse_validator_metadata(bytes).expect("parse"),
            ValidatorMetadata {
                potential_reward: 100_000,
                ..ValidatorMetadata::default()
            }
        );
    }

    #[test]
    fn metadata_codec_v2_fallback_pre_delegatee_reward() {
        // VERSION_SIZE + 3*8 bytes: uptime + potential reward, no delegatee reward.
        let bytes: &[u8] = &[
            0x00, 0x00, // codec version
            0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x8D, 0x80, // up duration
            0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0xBB, 0xA0, // last updated
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0, // potential reward
        ];
        assert_eq!(bytes.len(), PRE_DELEGATEE_REWARD_SIZE);
        assert_eq!(
            parse_validator_metadata(bytes).expect("parse"),
            ValidatorMetadata {
                up_duration: 6_000_000,
                last_updated: 900_000,
                potential_reward: 100_000,
                ..ValidatorMetadata::default()
            }
        );
    }

    #[test]
    fn metadata_codec_v2_invalid_version() {
        // version 0x0003 is not registered.
        let bytes: &[u8] = &[0x00, 0x03];
        assert_matches!(
            parse_validator_metadata(bytes),
            Err(CodecError::UnknownVersion)
        );
    }

    #[test]
    fn metadata_codec_v2_short_byte_len() {
        // A truncated v0 record (delegatee reward is only 6 bytes) → short read.
        let bytes: &[u8] = &[
            0x00, 0x00, // codec version
            0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x8D, 0x80, // up duration
            0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0xBB, 0xA0, // last updated
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x86, 0xA0, // potential reward
            0x00, 0x00, 0x00, 0x00, 0x4E, 0x20, // potential delegatee reward (short)
        ];
        assert_matches!(
            parse_validator_metadata(bytes),
            Err(CodecError::Packer(
                ava_codec::error::PackerError::InsufficientLength
            ))
        );
    }

    #[test]
    fn metadata_codec_v2_effective_weight() {
        let md = v2_value();
        // tx.weight + accrued_validation_rewards + accrued_delegatee_rewards.
        assert_eq!(md.effective_weight(10_000), Some(10_000 + 1_000 + 500));
        assert_eq!(md.effective_weight(u64::MAX), None);
    }
}

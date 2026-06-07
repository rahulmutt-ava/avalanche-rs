// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The validator-metadata codec (`state/metadata_codec.go`, specs 08 §3.4).
//!
//! This is a **separate** codec from the P-Chain tx/block [`Codec`] — it has its
//! own three registered versions (v0/v1/v2), selected by the value's serialized
//! 2-byte version tag. Later versions append fields to the wire layout of the
//! prior version, so a single struct ([`ValidatorMetadata`]) carries all fields
//! and the codec writes/reads only the subset gated by the active version.
//!
//! Go registers three `linearcodec`s under versions 0/1/2:
//!
//! ```text
//! c0 = linearcodec.New(["v0"])
//! c1 = linearcodec.New(["v0","v1"])
//! c2 = linearcodec.New(["v0","v1","v2"])
//! ```
//!
//! The avalanchego linear codec emits every field tagged for *any* registered
//! version of that codec, in struct-declaration order. Because the `vN:"true"`
//! tags are cumulative (v1 includes v0's fields, v2 includes v1's), encoding
//! under version `N` emits exactly the fields gated `<= N`. The Rust derive does
//! **not** support version-gated fields, so the field reads/writes are
//! hand-rolled here (the same manual `Packer` pattern `secp256k1fx` uses), keyed
//! on the codec version.
//!
//! [`Codec`]: crate::txs::codec::Codec

use ava_codec::error::CodecError;
use ava_codec::packer::Packer;

/// `codec.VersionSize` — the 2-byte version prefix every record carries.
pub const VERSION_SIZE: usize = 2;

/// `wrappers.LongLen` — the width of a marshalled `u64`.
pub const LONG_LEN: usize = 8;

/// Metadata codec version 0 (`CodecVersion0`): pre-Banff / Banff / Cortina.
///
/// Fields: `up_duration`, `last_updated`, `potential_reward`,
/// `potential_delegatee_reward`.
pub const CODEC_VERSION_0: u16 = 0;

/// Metadata codec version 1 (`CodecVersion1`): adds `staker_start_time`.
pub const CODEC_VERSION_1: u16 = 1;

/// Metadata codec version 2 (`codecVersion2`): ACP-236 / Helicon auto-renew.
///
/// Adds `accrued_validation_rewards`, `accrued_delegatee_rewards`,
/// `auto_compound_reward_shares`, `next_period`, `staker_end_time`.
pub const CODEC_VERSION_2: u16 = 2;

/// `preDelegateeRewardSize` — the marshalled size of the legacy Banff record
/// that stored uptime + potential reward but *not* the potential delegatee
/// reward: `VersionSize + 3*LongLen`.
pub const PRE_DELEGATEE_REWARD_SIZE: usize = VERSION_SIZE + 3 * LONG_LEN;

/// Returns `true` iff `version` is a registered metadata codec version.
#[must_use]
pub const fn is_registered_version(version: u16) -> bool {
    matches!(version, CODEC_VERSION_0 | CODEC_VERSION_1 | CODEC_VERSION_2)
}

/// Maps a sticky [`Packer`] error to the codec error family, returning `Ok` when
/// the packer is clean.
pub(crate) fn packer_result(p: &Packer) -> Result<(), CodecError> {
    match p.error() {
        Some(e) => Err(CodecError::Packer(e)),
        None => Ok(()),
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The serialized block bodies: `statelessUnsignedBlock`,
//! `statelessUnsignedGraniteBlock`, and `Epoch`.
//!
//! These reproduce the Go `serialize:"true"` field order **exactly** (block.go).
//! Encoding is hand-written against [`ava_codec::packer::Packer`] because the
//! fields mix `Id` (32 raw bytes, no length prefix), `i64` (the timestamp), and
//! length-prefixed `Vec<u8>` (certificate / inner block bytes) — kinds the
//! `#[derive(AvaCodec)]` macro does not currently cover for `Id`.

use ava_codec::packer::Packer;
use ava_types::id::{ID_LEN, Id};

/// Packs an [`Id`] as 32 raw bytes (no length prefix), mirroring Go's
/// fixed-array encoding.
pub(crate) fn pack_id(p: &mut Packer, id: &Id) {
    p.pack_fixed_bytes(id.as_bytes());
}

/// Unpacks a 32-byte [`Id`].
pub(crate) fn unpack_id(p: &mut Packer) -> Id {
    let raw = p.unpack_fixed_bytes(ID_LEN);
    Id::from_slice(&raw).unwrap_or(Id::EMPTY)
}

/// `statelessUnsignedBlock` — the signed body of a pre-Granite ProposerVM block.
///
/// Serialized field order (block.go): `ParentID`, `Timestamp` (i64),
/// `PChainHeight` (u64), `Certificate` (`Vec<u8>`), `Block` (`Vec<u8>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatelessUnsignedBlock {
    /// The parent ProposerVM block id.
    pub parent_id: Id,
    /// The wall-clock timestamp at which the block was proposed (Unix seconds).
    pub timestamp: i64,
    /// The P-Chain height the block is built against.
    pub p_chain_height: u64,
    /// The proposer's DER-encoded staking certificate (empty = unsigned).
    pub certificate: Vec<u8>,
    /// The inner (wrapped) block bytes.
    pub block: Vec<u8>,
}

impl StatelessUnsignedBlock {
    pub(crate) fn marshal_into(&self, p: &mut Packer) {
        pack_id(p, &self.parent_id);
        p.pack_u64(self.timestamp as u64);
        p.pack_u64(self.p_chain_height);
        p.pack_bytes(&self.certificate);
        p.pack_bytes(&self.block);
    }

    pub(crate) fn unmarshal_from(p: &mut Packer) -> Self {
        let parent_id = unpack_id(p);
        let timestamp = p.unpack_u64() as i64;
        let p_chain_height = p.unpack_u64();
        let certificate = p.unpack_bytes();
        let block = p.unpack_bytes();
        Self {
            parent_id,
            timestamp,
            p_chain_height,
            certificate,
            block,
        }
    }

    pub(crate) fn size(&self) -> usize {
        // ParentID (32) + Timestamp (8) + PChainHeight (8)
        // + u32 len + cert bytes + u32 len + block bytes.
        ID_LEN
            .saturating_add(8)
            .saturating_add(8)
            .saturating_add(4)
            .saturating_add(self.certificate.len())
            .saturating_add(4)
            .saturating_add(self.block.len())
    }
}

/// `Epoch` — the Granite per-block epoch.
///
/// Serialized field order (block.go): `PChainHeight` (u64), `Number` (u64),
/// `StartTime` (i64). The all-zero `Epoch` is the sentinel meaning "no epoch".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Epoch {
    /// The P-Chain height the epoch was sealed at.
    pub p_chain_height: u64,
    /// The monotonically-increasing epoch number.
    pub number: u64,
    /// The epoch start time (Unix seconds).
    pub start_time: i64,
}

impl Epoch {
    /// Whether this is the zero (absent) epoch (Go `epoch == (Epoch{})`).
    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Epoch::default()
    }

    pub(crate) fn marshal_into(&self, p: &mut Packer) {
        p.pack_u64(self.p_chain_height);
        p.pack_u64(self.number);
        p.pack_u64(self.start_time as u64);
    }

    pub(crate) fn unmarshal_from(p: &mut Packer) -> Self {
        let p_chain_height = p.unpack_u64();
        let number = p.unpack_u64();
        let start_time = p.unpack_u64() as i64;
        Self {
            p_chain_height,
            number,
            start_time,
        }
    }

    pub(crate) fn size(&self) -> usize {
        24
    }
}

/// `statelessUnsignedGraniteBlock` — the signed body of a Granite block.
///
/// Serialized field order (block.go): the embedded `statelessUnsignedBlock`
/// followed by the `Epoch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatelessUnsignedGraniteBlock {
    /// The embedded pre-Granite body.
    pub stateless_block: StatelessUnsignedBlock,
    /// The Granite epoch (must be non-zero on a valid Granite block).
    pub epoch: Epoch,
}

impl StatelessUnsignedGraniteBlock {
    pub(crate) fn marshal_into(&self, p: &mut Packer) {
        self.stateless_block.marshal_into(p);
        self.epoch.marshal_into(p);
    }

    pub(crate) fn unmarshal_from(p: &mut Packer) -> Self {
        let stateless_block = StatelessUnsignedBlock::unmarshal_from(p);
        let epoch = Epoch::unmarshal_from(p);
        Self {
            stateless_block,
            epoch,
        }
    }

    pub(crate) fn size(&self) -> usize {
        self.stateless_block.size().saturating_add(self.epoch.size())
    }
}

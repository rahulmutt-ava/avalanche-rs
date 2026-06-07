// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block codec — the linear-codec registration order and the interface
//! (de)serialization.
//!
//! Port of Go `vms/proposervm/block/codec.go`. Go registers, in order:
//!
//! ```text
//! lc.RegisterType(&statelessBlock{})         // typeID 0
//! lc.RegisterType(&option{})                 // typeID 1
//! lc.RegisterType(&statelessGraniteBlock{})  // typeID 2
//! ```
//!
//! Every top-level block is marshaled as the 2-byte codec version prefix, the
//! `u32` typeID, then the concrete struct fields in `serialize:"true"` order.

use ava_codec::packer::Packer;
use ava_types::id::Id;

use super::option::Option_;
use super::post_fork::{GraniteBlock, SignedBlock};
use super::stateless::{StatelessUnsignedBlock, StatelessUnsignedGraniteBlock, unpack_id};

/// The codec version (Go `CodecVersion = 0`).
pub const CODEC_VERSION: u16 = 0;

/// typeID of `statelessBlock` (the pre-Granite post-fork block).
pub const TYPE_ID_STATELESS_BLOCK: u32 = 0;
/// typeID of `option`.
pub const TYPE_ID_OPTION: u32 = 1;
/// typeID of `statelessGraniteBlock`.
pub const TYPE_ID_GRANITE_BLOCK: u32 = 2;

/// A parsed ProposerVM block (the Go `Block` interface).
#[derive(Debug, Clone)]
pub enum ParsedBlock {
    /// A `statelessBlock` (post-fork, pre-Granite; may be signed or unsigned).
    Signed(SignedBlock),
    /// An `option` block.
    Option(Option_),
    /// A `statelessGraniteBlock`.
    Granite(GraniteBlock),
}

impl ParsedBlock {
    /// The block id.
    #[must_use]
    pub fn id(&self) -> Id {
        match self {
            ParsedBlock::Signed(b) => b.id(),
            ParsedBlock::Option(b) => b.id(),
            ParsedBlock::Granite(b) => b.id(),
        }
    }

    /// The parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        match self {
            ParsedBlock::Signed(b) => b.parent_id(),
            ParsedBlock::Option(b) => b.parent_id(),
            ParsedBlock::Granite(b) => b.parent_id(),
        }
    }

    /// The inner (wrapped) block bytes.
    #[must_use]
    pub fn inner_block(&self) -> &[u8] {
        match self {
            ParsedBlock::Signed(b) => b.inner_block(),
            ParsedBlock::Option(b) => b.inner_block(),
            ParsedBlock::Granite(b) => b.inner_block(),
        }
    }

    /// The serialized bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            ParsedBlock::Signed(b) => b.bytes(),
            ParsedBlock::Option(b) => b.bytes(),
            ParsedBlock::Granite(b) => b.bytes(),
        }
    }

    /// Verifies the block against `chain_id` (Go `block.verify(chainID)`).
    ///
    /// # Errors
    /// Returns [`crate::Error`] on a bad signature, an unexpected signature, an
    /// invalid certificate, or a zero Granite epoch.
    pub fn verify(&self, chain_id: Id) -> crate::Result<()> {
        match self {
            ParsedBlock::Signed(b) => b.verify(chain_id),
            ParsedBlock::Option(_) => Ok(()),
            ParsedBlock::Granite(b) => b.verify(chain_id),
        }
    }
}

/// Marshals an interface-encoded block: version prefix + `u32` typeID + body.
pub(crate) fn marshal_typed(type_id: u32, body: &dyn Fn(&mut Packer), size_hint: usize) -> Vec<u8> {
    let mut p = Packer::with_max_size(2usize.saturating_add(4).saturating_add(size_hint));
    p.pack_u16(CODEC_VERSION);
    p.pack_u32(type_id);
    body(&mut p);
    p.into_bytes()
}

/// Parses a block from its raw bytes without verifying the signature (Go
/// `ParseWithoutVerification`).
///
/// # Errors
/// Returns [`crate::Error`] on a wrong codec version, an unknown typeID, or a
/// malformed/over-long buffer.
pub fn parse_without_verification(bytes: &[u8]) -> crate::Result<ParsedBlock> {
    let mut p = Packer::new_read(bytes);
    let version = p.unpack_u16();
    if version != CODEC_VERSION {
        return Err(crate::Error::WrongCodecVersion {
            expected: CODEC_VERSION,
            got: version,
        });
    }
    let type_id = p.unpack_u32();
    if p.errored() {
        return Err(crate::Error::Codec("truncated typeID".to_string()));
    }

    let block = match type_id {
        TYPE_ID_STATELESS_BLOCK => {
            let stateless_block = StatelessUnsignedBlock::unmarshal_from(&mut p);
            let signature = p.unpack_bytes();
            ParsedBlock::Signed(SignedBlock::initialize(
                stateless_block,
                signature,
                bytes.to_vec(),
            )?)
        }
        TYPE_ID_OPTION => {
            let parent_id = unpack_id(&mut p);
            let inner = p.unpack_bytes();
            ParsedBlock::Option(Option_::initialize(parent_id, inner, bytes.to_vec()))
        }
        TYPE_ID_GRANITE_BLOCK => {
            let body = StatelessUnsignedGraniteBlock::unmarshal_from(&mut p);
            let signature = p.unpack_bytes();
            ParsedBlock::Granite(GraniteBlock::initialize(body, signature, bytes.to_vec())?)
        }
        other => {
            return Err(crate::Error::Codec(format!("unknown typeID {other}")));
        }
    };

    if let Some(err) = p.error() {
        return Err(crate::Error::Codec(format!("{err:?}")));
    }
    if p.offset() != bytes.len() {
        return Err(crate::Error::Codec("trailing bytes".to_string()));
    }
    Ok(block)
}

/// Parses and verifies a block (Go `Parse`).
///
/// # Errors
/// Returns [`crate::Error`] on a decode failure or a failed verification.
pub fn parse(bytes: &[u8], chain_id: Id) -> crate::Result<ParsedBlock> {
    let block = parse_without_verification(bytes)?;
    block.verify(chain_id)?;
    Ok(block)
}

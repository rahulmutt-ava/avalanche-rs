// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Simplex block wrapper (`block.go` + `block.canoto.go`) and its
//! [`ProtocolMetadata`] header (the external `simplex.ProtocolMetadata`),
//! specs 06 §8.
//!
//! A Simplex block wraps an inner VM block plus a Simplex protocol header; its
//! finality is a [`QC`](crate::messages::QC) over the block, not metastable
//! confidence. On the wire it is a canoto message with three opaque byte
//! fields — `Metadata`, `InnerBlock`, `Blacklist` — encoded byte-identical to
//! Go's generated `canotoSimplexBlock.MarshalCanoto`.

use sha2::{Digest as _, Sha256};

use crate::canoto::{self, DecodeError, Reader, WIRE_LEN};
use crate::error::{Error, Result};

/// Canoto field numbers for `canotoSimplexBlock` (`block.canoto.go`).
const BLOCK_FIELD_METADATA: u32 = 1;
const BLOCK_FIELD_INNER: u32 = 2;
const BLOCK_FIELD_BLACKLIST: u32 = 3;

/// Length of a [`ProtocolMetadata`] byte encoding
/// (`simplex.ProtocolMetadataLen`): `version(1) + epoch(8) + round(8) +
/// seq(8) + prev(32)`.
pub const PROTOCOL_METADATA_LEN: usize = 1 + 8 + 8 + 8 + 32;

/// Protocol state at a point in time (`simplex.ProtocolMetadata`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolMetadata {
    /// Protocol version this block was created with.
    pub version: u8,
    /// Epoch in which the block was proposed.
    pub epoch: u64,
    /// Round number in which the block was proposed (may be an empty block).
    pub round: u64,
    /// The block's order among all blocks (never an empty block).
    pub seq: u64,
    /// Digest of the previous data block.
    pub prev: [u8; 32],
}

impl ProtocolMetadata {
    /// `ProtocolMetadata.Bytes` — `[version(1), epoch(8 BE), round(8 BE),
    /// seq(8 BE), prev(32)]`.
    pub fn to_bytes(&self) -> [u8; PROTOCOL_METADATA_LEN] {
        let mut buf = Vec::with_capacity(PROTOCOL_METADATA_LEN);
        buf.push(self.version);
        buf.extend_from_slice(&self.epoch.to_be_bytes());
        buf.extend_from_slice(&self.round.to_be_bytes());
        buf.extend_from_slice(&self.seq.to_be_bytes());
        buf.extend_from_slice(&self.prev);
        // Length is exactly PROTOCOL_METADATA_LEN by construction.
        let mut out = [0u8; PROTOCOL_METADATA_LEN];
        out.copy_from_slice(&buf);
        out
    }

    /// `ProtocolMetadataFromBytes` — parses a fixed-length metadata encoding.
    pub fn from_bytes(buf: &[u8]) -> Result<Self> {
        // Destructure the fixed-length buffer; rejects any other length without
        // any panicking index/slice.
        let buf: &[u8; PROTOCOL_METADATA_LEN] = buf
            .try_into()
            .map_err(|_| Error::Decode(DecodeError::InvalidLength))?;
        let (version, rest) = buf.split_at(1);
        let (epoch, rest) = rest.split_at(8);
        let (round, rest) = rest.split_at(8);
        let (seq, prev) = rest.split_at(8);
        let to_u64 = |s: &[u8]| -> u64 {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_be_bytes(a)
        };
        let mut prev_arr = [0u8; 32];
        prev_arr.copy_from_slice(prev);
        Ok(Self {
            version: version.first().copied().unwrap_or(0),
            epoch: to_u64(epoch),
            round: to_u64(round),
            seq: to_u64(seq),
            prev: prev_arr,
        })
    }
}

/// A parsed Simplex block (`simplex.Block`): the protocol metadata, the inner
/// VM block bytes, the blacklist bytes, and the computed digest.
///
/// The inner block and blacklist are kept as opaque bytes — parsing the inner
/// block requires a VM `block.Parser` (deferred with the feature-gated engine),
/// and the blacklist round-trips opaquely through the canoto wrapper.
pub struct Block {
    /// Protocol metadata header.
    pub metadata: ProtocolMetadata,
    /// Serialized inner VM block (`vmBlock.Bytes()`).
    pub inner_block: Vec<u8>,
    /// Serialized blacklist (`blacklist.Bytes()`).
    pub blacklist: Vec<u8>,
    /// The block digest (`hashing.ComputeHash256Array` over [`Self::to_bytes`]).
    pub digest: [u8; 32],
}

impl Block {
    /// Builds a block from its parts, computing the digest from the canoto
    /// serialization (`newBlock`).
    pub fn new(metadata: ProtocolMetadata, inner_block: Vec<u8>, blacklist: Vec<u8>) -> Self {
        let bytes = marshal_canoto_block(&metadata.to_bytes(), &inner_block, &blacklist);
        let digest = compute_digest(&bytes);
        Self {
            metadata,
            inner_block,
            blacklist,
            digest,
        }
    }

    /// `Block.Bytes` — the canoto serialization (byte-identical to Go's
    /// `canotoSimplexBlock.MarshalCanoto`).
    pub fn to_bytes(&self) -> Vec<u8> {
        marshal_canoto_block(
            &self.metadata.to_bytes(),
            &self.inner_block,
            &self.blacklist,
        )
    }

    /// `blockDeserializer.DeserializeBlock` — parses a block from its canoto
    /// bytes (without verifying or parsing the inner VM block).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let (metadata_bytes, inner_block, blacklist) = unmarshal_canoto_block(bytes)?;
        let metadata = ProtocolMetadata::from_bytes(&metadata_bytes)?;
        Ok(Self::new(metadata, inner_block, blacklist))
    }
}

/// `computeDigest` — `hashing.ComputeHash256Array` (SHA-256) over the bytes.
pub fn compute_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// canoto block encode/decode (byte-identical to block.canoto.go).
// ---------------------------------------------------------------------------

/// Marshals a `canotoSimplexBlock { Metadata, InnerBlock, Blacklist }`. Each
/// field is emitted only if non-empty (matches `MarshalCanotoInto`).
fn marshal_canoto_block(metadata: &[u8], inner: &[u8], blacklist: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if !metadata.is_empty() {
        canoto::append_tag(&mut out, BLOCK_FIELD_METADATA, WIRE_LEN);
        canoto::append_bytes(&mut out, metadata);
    }
    if !inner.is_empty() {
        canoto::append_tag(&mut out, BLOCK_FIELD_INNER, WIRE_LEN);
        canoto::append_bytes(&mut out, inner);
    }
    if !blacklist.is_empty() {
        canoto::append_tag(&mut out, BLOCK_FIELD_BLACKLIST, WIRE_LEN);
        canoto::append_bytes(&mut out, blacklist);
    }
    out
}

/// Unmarshals a `canotoSimplexBlock`, returning `(metadata, inner, blacklist)`.
/// Enforces ascending field order and the non-empty guards (matches
/// `UnmarshalCanotoFrom`).
fn unmarshal_canoto_block(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mut r = Reader::new(bytes);
    let mut metadata: Vec<u8> = Vec::new();
    let mut inner: Vec<u8> = Vec::new();
    let mut blacklist: Vec<u8> = Vec::new();
    let mut min_field: u32 = 0;
    while r.has_next() {
        let (field, wire) = r.read_tag()?;
        if field < min_field {
            return Err(Error::Decode(DecodeError::InvalidFieldOrder));
        }
        if wire != WIRE_LEN {
            return Err(Error::Decode(DecodeError::UnexpectedWireType(wire)));
        }
        let val = r.read_bytes()?;
        if val.is_empty() {
            return Err(Error::Decode(DecodeError::ZeroValue));
        }
        match field {
            BLOCK_FIELD_METADATA => metadata = val.to_vec(),
            BLOCK_FIELD_INNER => inner = val.to_vec(),
            BLOCK_FIELD_BLACKLIST => blacklist = val.to_vec(),
            other => return Err(Error::Decode(DecodeError::UnknownField(other))),
        }
        min_field = field.saturating_add(1);
    }
    Ok((metadata, inner, blacklist))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip() {
        let md = ProtocolMetadata {
            version: 1,
            epoch: 2,
            round: 3,
            seq: 4,
            prev: [0xab; 32],
        };
        let bytes = md.to_bytes();
        assert_eq!(bytes.len(), PROTOCOL_METADATA_LEN);
        assert_eq!(ProtocolMetadata::from_bytes(&bytes).unwrap(), md);
    }

    #[test]
    fn block_roundtrip() {
        let md = ProtocolMetadata {
            version: 1,
            epoch: 2,
            round: 3,
            seq: 4,
            prev: [0xab; 32],
        };
        let block = Block::new(md.clone(), vec![0xde, 0xad, 0xbe, 0xef], vec![0x00, 0x00]);
        let bytes = block.to_bytes();
        let parsed = Block::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.metadata, md);
        assert_eq!(parsed.inner_block, vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parsed.blacklist, vec![0x00, 0x00]);
        assert_eq!(parsed.digest, block.digest);
    }
}

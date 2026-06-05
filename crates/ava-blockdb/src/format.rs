// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! On-disk format primitives for `ava-blockdb`, reproduced **byte-exactly** from
//! Go `x/blockdb/database.go`.
//!
//! All multi-byte integers are **little-endian** (Go uses
//! `binary.LittleEndian`). The three headers are:
//!
//! - [`IndexFileHeader`] — 64 bytes; lives at offset 0 of the `.idx` file.
//! - [`IndexEntry`] — 16 bytes; fixed-size slot, one per height.
//! - [`BlockEntryHeader`] — 22 bytes; precedes each block's bytes in a `.dat`
//!   file.
//!
//! The checksum is `xxhash.Sum64` (Go `github.com/cespare/xxhash/v2`), i.e.
//! XXH64 with seed 0, computed over the **uncompressed** block bytes.

use std::hash::Hasher;

use crate::error::{Error, Result};

/// Version of the index file format (Go `IndexFileVersion`).
pub const INDEX_FILE_VERSION: u64 = 1;

/// Version of a block entry (Go `BlockEntryVersion`).
pub const BLOCK_ENTRY_VERSION: u16 = 1;

/// Sentinel for "no height set" (Go `unsetHeight = math.MaxUint64`).
pub const UNSET_HEIGHT: u64 = u64::MAX;

/// Size in bytes of a serialized [`BlockEntryHeader`] (Go `sizeOfBlockEntryHeader`).
pub const SIZE_OF_BLOCK_ENTRY_HEADER: u32 = 22;

/// Size in bytes of a serialized [`IndexEntry`] (Go `sizeOfIndexEntry`).
pub const SIZE_OF_INDEX_ENTRY: u64 = 16;

/// Size in bytes of a serialized [`IndexFileHeader`] (Go `sizeOfIndexFileHeader`).
pub const SIZE_OF_INDEX_FILE_HEADER: u64 = 64;

/// Computes the block checksum (Go `calculateChecksum`).
///
/// This is `xxhash.Sum64` (XXH64, seed 0) over the **uncompressed** block bytes.
#[must_use]
pub fn calculate_checksum(data: &[u8]) -> u64 {
    let mut hasher = twox_hash::XxHash64::with_seed(0);
    hasher.write(data);
    hasher.finish()
}

/// Header preceding a block's bytes in a data file (Go `blockEntryHeader`).
///
/// Note: `size` is the length of the **compressed** block bytes that follow,
/// while `checksum` is taken over the **uncompressed** bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntryHeader {
    /// Block height.
    pub height: u64,
    /// Length in bytes of the (compressed) block data that follows.
    pub size: u32,
    /// xxhash checksum of the uncompressed block data.
    pub checksum: u64,
    /// Block entry format version.
    pub version: u16,
}

impl BlockEntryHeader {
    /// Serializes to the 22-byte little-endian layout.
    ///
    /// Layout: `height(u64)@0 | size(u32)@8 | checksum(u64)@12 | version(u16)@20`.
    #[must_use]
    pub fn marshal_binary(&self) -> [u8; 22] {
        let mut buf = [0u8; 22];
        buf[0..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8..12].copy_from_slice(&self.size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.checksum.to_le_bytes());
        buf[20..22].copy_from_slice(&self.version.to_le_bytes());
        buf
    }

    /// Deserializes from exactly 22 bytes.
    ///
    /// # Errors
    /// Returns [`Error::Corrupted`] if `data` is not exactly 22 bytes.
    pub fn unmarshal_binary(data: &[u8]) -> Result<Self> {
        if data.len() != SIZE_OF_BLOCK_ENTRY_HEADER as usize {
            return Err(Error::Corrupted);
        }
        Ok(Self {
            height: u64::from_le_bytes(le8(data, 0)?),
            size: u32::from_le_bytes(le4(data, 8)?),
            checksum: u64::from_le_bytes(le8(data, 12)?),
            version: u16::from_le_bytes(le2(data, 20)?),
        })
    }
}

/// An index file slot mapping a height to its data-file location (Go `indexEntry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexEntry {
    /// Global byte offset in the (logical) data space where the block header starts.
    pub offset: u64,
    /// Length in bytes of the (compressed) block data (excluding the header).
    pub size: u32,
    /// Reserved bytes (alignment); always zero.
    pub reserved: [u8; 4],
}

impl IndexEntry {
    /// Returns true if this entry is uninitialized (Go `IsEmpty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.offset == 0 && self.size == 0
    }

    /// Serializes to the 16-byte little-endian layout.
    ///
    /// Layout: `offset(u64)@0 | size(u32)@8 | reserved[4]@12`. Note that Go's
    /// `MarshalBinary` never writes the reserved bytes, so they are always zero.
    #[must_use]
    pub fn marshal_binary(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..12].copy_from_slice(&self.size.to_le_bytes());
        buf
    }

    /// Deserializes from exactly 16 bytes.
    ///
    /// # Errors
    /// Returns [`Error::Corrupted`] if `data` is not exactly 16 bytes.
    pub fn unmarshal_binary(data: &[u8]) -> Result<Self> {
        if data.len() != SIZE_OF_INDEX_ENTRY as usize {
            return Err(Error::Corrupted);
        }
        Ok(Self {
            offset: u64::from_le_bytes(le8(data, 0)?),
            size: u32::from_le_bytes(le4(data, 8)?),
            reserved: [0u8; 4],
        })
    }
}

/// Header at offset 0 of the index file (Go `indexFileHeader`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexFileHeader {
    /// Index file format version.
    pub version: u64,
    /// Maximum size of a single data file in bytes.
    pub max_data_file_size: u64,
    /// Lowest block height tracked by the database.
    pub min_height: u64,
    /// Highest block height written (or [`UNSET_HEIGHT`]).
    pub max_height: u64,
    /// Next position to write new data in the (logical) data space.
    pub next_write_offset: u64,
    /// Reserved bytes (alignment / future use); always zero.
    pub reserved: [u8; 24],
}

impl IndexFileHeader {
    /// Serializes to the 64-byte little-endian layout.
    ///
    /// Layout: `version | max_data_file_size | min_height | max_height |
    /// next_write_offset` (all `u64`) `| reserved[24]`. Go's `MarshalBinary`
    /// never writes the reserved bytes, so they are always zero.
    #[must_use]
    pub fn marshal_binary(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..16].copy_from_slice(&self.max_data_file_size.to_le_bytes());
        buf[16..24].copy_from_slice(&self.min_height.to_le_bytes());
        buf[24..32].copy_from_slice(&self.max_height.to_le_bytes());
        buf[32..40].copy_from_slice(&self.next_write_offset.to_le_bytes());
        buf
    }

    /// Deserializes from exactly 64 bytes.
    ///
    /// # Errors
    /// Returns [`Error::Corrupted`] if `data` is not exactly 64 bytes.
    pub fn unmarshal_binary(data: &[u8]) -> Result<Self> {
        if data.len() != SIZE_OF_INDEX_FILE_HEADER as usize {
            return Err(Error::Corrupted);
        }
        Ok(Self {
            version: u64::from_le_bytes(le8(data, 0)?),
            max_data_file_size: u64::from_le_bytes(le8(data, 8)?),
            min_height: u64::from_le_bytes(le8(data, 16)?),
            max_height: u64::from_le_bytes(le8(data, 24)?),
            next_write_offset: u64::from_le_bytes(le8(data, 32)?),
            reserved: [0u8; 24],
        })
    }
}

#[inline]
fn le2(data: &[u8], off: usize) -> Result<[u8; 2]> {
    data.get(off..off.wrapping_add(2))
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Corrupted)
}

#[inline]
fn le4(data: &[u8], off: usize) -> Result<[u8; 4]> {
    data.get(off..off.wrapping_add(4))
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Corrupted)
}

#[inline]
fn le8(data: &[u8], off: usize) -> Result<[u8; 8]> {
    data.get(off..off.wrapping_add(8))
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Corrupted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_checksum_is_xxh64_seed0() {
        // Standard XXH64 of empty input with seed 0.
        assert_eq!(calculate_checksum(&[]), 0xef46_db37_51d8_e999);
    }

    #[test]
    fn header_roundtrips() {
        let beh = BlockEntryHeader {
            height: 42,
            size: 7,
            checksum: 0xdead_beef,
            version: BLOCK_ENTRY_VERSION,
        };
        assert_eq!(
            BlockEntryHeader::unmarshal_binary(&beh.marshal_binary()).unwrap(),
            beh
        );

        let e = IndexEntry {
            offset: 99,
            size: 13,
            reserved: [0; 4],
        };
        assert_eq!(
            IndexEntry::unmarshal_binary(&e.marshal_binary()).unwrap(),
            e
        );
    }
}

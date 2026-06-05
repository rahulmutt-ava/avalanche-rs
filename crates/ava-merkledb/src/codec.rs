// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! On-disk node codec — byte-exact port of Go `x/merkledb/codec.go`.
//!
//! [`encode_db_node`] writes `MaybeBytes(value)`, `Uvarint(num_children)`, then
//! per child **in ascending index order**: `Uvarint(index)`, `Key(compressed)`,
//! `ID(child_id)`, `Bool(has_value)`. [`decode_db_node`] enforces every Go
//! decode rejection (leading-zero uvarint, non-zero key padding, too-many /
//! out-of-order child indices, int overflow, trailing bytes, unexpected EOF).
//!
//! Primitives: `Bool` = 1 byte (`0x00`/`0x01`); `Uvarint` = LEB128
//! (`binary.PutUvarint`) with a canonical no-leading-zeroes decode check;
//! `Bytes`/`Key` are uvarint-length-prefixed; `Key` packs the bit length then
//! `bytes_needed(length)` bytes with the partial-byte-zero-padded rule.

use std::collections::BTreeMap;

use bytes::Bytes;

use ava_types::id::{ID_LEN, Id};

use crate::error::{Error, Result};
use crate::key::{Key, bytes_needed};
use crate::maybe::Maybe;
use crate::node::{Child, DbNode};

const TRUE_BYTE: u8 = 1;
const FALSE_BYTE: u8 = 0;

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// A byte-exact codec writer mirroring Go `codecWriter`.
struct CodecWriter {
    b: Vec<u8>,
}

impl CodecWriter {
    fn new() -> Self {
        CodecWriter { b: Vec::new() }
    }

    fn bool(&mut self, v: bool) {
        self.b.push(if v { TRUE_BYTE } else { FALSE_BYTE });
    }

    fn uvarint(&mut self, mut v: u64) {
        // Equivalent to Go binary.AppendUvarint (unsigned LEB128).
        while v >= 0x80 {
            self.b.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.b.push(v as u8);
    }

    fn id(&mut self, v: &Id) {
        self.b.extend_from_slice(v.as_bytes());
    }

    fn raw_bytes(&mut self, v: &[u8]) {
        self.uvarint(v.len() as u64);
        self.b.extend_from_slice(v);
    }

    fn maybe_bytes(&mut self, v: &Maybe<Bytes>) {
        let has_value = v.has_value();
        self.bool(has_value);
        if let Maybe::Some(bytes) = v {
            self.raw_bytes(bytes);
        }
    }

    fn key(&mut self, v: &Key) {
        self.uvarint(v.length() as u64);
        self.b.extend_from_slice(v.bytes());
    }
}

/// Encodes a [`DbNode`] to its on-disk byte representation.
/// Mirrors Go `encodeDBNode`. `children` is a [`BTreeMap`] so iteration is
/// already in ascending index order.
#[must_use]
pub fn encode_db_node(n: &DbNode) -> Vec<u8> {
    let mut w = CodecWriter::new();
    w.maybe_bytes(&n.value);
    w.uvarint(n.children.len() as u64);
    for (index, entry) in &n.children {
        w.uvarint(u64::from(*index));
        w.key(&entry.compressed_key);
        w.id(&entry.id);
        w.bool(entry.has_value);
    }
    w.b
}

/// Encodes a standalone [`Key`] (uvarint bit-length + packed bytes).
/// Mirrors Go `encodeKey`.
#[must_use]
pub fn encode_key(key: &Key) -> Vec<u8> {
    let mut w = CodecWriter::new();
    w.key(key);
    w.b
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// A byte-exact codec reader mirroring Go `codecReader` (always copies).
struct CodecReader<'a> {
    b: &'a [u8],
}

impl<'a> CodecReader<'a> {
    fn new(b: &'a [u8]) -> Self {
        CodecReader { b }
    }

    fn read_bool(&mut self) -> Result<bool> {
        let first = *self.b.first().ok_or(Error::UnexpectedEof)?;
        if first > TRUE_BYTE {
            return Err(Error::InvalidBool);
        }
        self.b = &self.b[1..];
        Ok(first == TRUE_BYTE)
    }

    fn read_uvarint(&mut self) -> Result<u64> {
        // Decode unsigned LEB128. `binary.Uvarint` returns bytesRead<=0 on EOF
        // or overflow; we surface EOF as UnexpectedEof and overflow as
        // IntOverflow (Go's `binary.Uvarint` returns 0/negative on >10 bytes).
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        let mut bytes_read = 0usize;
        loop {
            let byte = *self.b.get(bytes_read).ok_or(Error::UnexpectedEof)?;
            bytes_read += 1;
            if shift >= 64 || (shift == 63 && byte > 1) {
                // More than 10 bytes, or 10th byte sets bits above 63.
                return Err(Error::IntOverflow);
            }
            result |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                // To ensure canonical decoding, reject a final byte of 0x00 for
                // multi-byte varints (Go errLeadingZeroes).
                if bytes_read > 1 && byte == 0 {
                    return Err(Error::LeadingZeroes);
                }
                break;
            }
            shift += 7;
        }
        self.b = &self.b[bytes_read..];
        Ok(result)
    }

    fn read_id(&mut self) -> Result<Id> {
        if self.b.len() < ID_LEN {
            return Err(Error::UnexpectedEof);
        }
        let id = Id::from_slice(&self.b[..ID_LEN]).map_err(|_| Error::UnexpectedEof)?;
        self.b = &self.b[ID_LEN..];
        Ok(id)
    }

    fn read_bytes(&mut self) -> Result<Bytes> {
        let length = self.read_uvarint()?;
        let length = usize::try_from(length).map_err(|_| Error::IntOverflow)?;
        if length > self.b.len() {
            return Err(Error::UnexpectedEof);
        }
        let out = Bytes::copy_from_slice(&self.b[..length]);
        self.b = &self.b[length..];
        Ok(out)
    }

    fn read_maybe_bytes(&mut self) -> Result<Maybe<Bytes>> {
        if !self.read_bool()? {
            return Ok(Maybe::Nothing);
        }
        Ok(Maybe::Some(self.read_bytes()?))
    }

    fn read_key(&mut self) -> Result<Key> {
        let bit_len = self.read_uvarint()?;
        let bit_len = usize::try_from(bit_len).map_err(|_| Error::IntOverflow)?;
        let byte_len = bytes_needed(bit_len);
        if byte_len > self.b.len() {
            return Err(Error::UnexpectedEof);
        }
        let partial = bit_len % 8;
        if partial != 0 {
            // Confirm the padding bits in the partial final byte are zero.
            let padding_mask: u8 = 0xFF >> partial;
            if self.b[byte_len - 1] & padding_mask != 0 {
                return Err(Error::NonZeroKeyPadding);
            }
        }
        let key = Key::from_raw(Bytes::copy_from_slice(&self.b[..byte_len]), bit_len);
        self.b = &self.b[byte_len..];
        Ok(key)
    }
}

/// Decodes a [`DbNode`] from its on-disk bytes, enforcing every Go decode
/// rejection. Mirrors Go `decodeDBNode`.
pub fn decode_db_node(b: &[u8]) -> Result<DbNode> {
    let mut r = CodecReader::new(b);

    let value = r.read_maybe_bytes()?;

    let num_children = r.read_uvarint()?;
    if num_children > u64::from(crate::key::BranchFactor::LARGEST.value()) {
        return Err(Error::TooManyChildren);
    }

    let mut children: BTreeMap<u8, Child> = BTreeMap::new();
    let mut previous_child: u64 = 0;
    for i in 0..num_children {
        let index = r.read_uvarint()?;
        // Must be strictly ascending after the first entry, and fit in a byte.
        if (i != 0 && index <= previous_child) || index > u64::from(u8::MAX) {
            return Err(Error::ChildIndexTooLarge);
        }
        previous_child = index;

        let compressed_key = r.read_key()?;
        let child_id = r.read_id()?;
        let has_value = r.read_bool()?;
        children.insert(
            index as u8,
            Child {
                compressed_key,
                id: child_id,
                has_value,
            },
        );
    }

    if !r.b.is_empty() {
        return Err(Error::ExtraSpace);
    }
    Ok(DbNode { value, children })
}

/// Decodes a standalone [`Key`], rejecting trailing bytes. Mirrors Go `decodeKey`.
pub fn decode_key(b: &[u8]) -> Result<Key> {
    let mut r = CodecReader::new(b);
    let key = r.read_key()?;
    if !r.b.is_empty() {
        return Err(Error::ExtraSpace);
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvarint_roundtrip_and_canonical() {
        for v in [0u64, 1, 127, 128, 300, 16384, u64::MAX] {
            let mut w = CodecWriter::new();
            w.uvarint(v);
            let mut r = CodecReader::new(&w.b);
            assert_eq!(r.read_uvarint().unwrap(), v);
            assert!(r.b.is_empty());
        }
        // Non-canonical (leading zero) must be rejected: 0x80 0x00 == 0 padded.
        let mut r = CodecReader::new(&[0x80, 0x00]);
        assert_eq!(r.read_uvarint(), Err(Error::LeadingZeroes));
    }
}

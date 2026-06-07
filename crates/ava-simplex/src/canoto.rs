// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Minimal hand-rolled [canoto](https://github.com/StephenButtolph/canoto) wire
//! primitives.
//!
//! There is **no canoto codegen crate** in this workspace, so the Simplex
//! message types ([`crate::messages`], [`crate::block`]) encode/decode by hand
//! to round-trip byte-identical to Go's generated `qc.canoto.go` /
//! `block.canoto.go`. Canoto is a protobuf-flavoured format:
//!
//! - A **tag** is `varint(field_number << 3 | wire_type)`. For the field
//!   numbers used here (1, 2, 3) the tag is a single byte.
//! - The only wire type we use is [`WIRE_LEN`] (`2`): a length-delimited field
//!   encoded as `varint(len) ++ bytes`.
//! - Fields appear in **strictly ascending field-number order**; a field whose
//!   value is the zero value (empty bytes / all-zero fixed bytes) is **omitted**
//!   entirely (this is why Go's generated `Marshal` guards every field with a
//!   `len(..) != 0` / `!IsZero(..)` check). On decode, an empty length-delimited
//!   value for such a field is rejected (`ErrZeroValue`).

/// Canoto length-delimited wire type (`canoto.Len`).
pub const WIRE_LEN: u8 = 2;

/// Errors decoding a canoto-encoded message.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended before a complete value could be read.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// A varint was longer than 10 bytes (overflows `u64`).
    #[error("varint overflows u64")]
    VarintOverflow,
    /// A field tag referenced an unknown field number.
    #[error("unknown field {0}")]
    UnknownField(u32),
    /// Fields were not in strictly ascending field-number order.
    #[error("invalid field order")]
    InvalidFieldOrder,
    /// A field used an unexpected wire type.
    #[error("unexpected wire type {0}")]
    UnexpectedWireType(u8),
    /// A length-delimited field declared a length larger than the remaining
    /// input.
    #[error("invalid length")]
    InvalidLength,
    /// A field that must be non-zero decoded to the zero value
    /// (`canoto.ErrZeroValue`).
    #[error("zero value for required field")]
    ZeroValue,
}

/// Appends `field_number << 3 | wire_type` as a varint tag to `out`.
pub fn append_tag(out: &mut Vec<u8>, field_number: u32, wire_type: u8) {
    let tag = (u64::from(field_number) << 3) | u64::from(wire_type);
    append_varint(out, tag);
}

/// Appends an unsigned LEB128 varint to `out`.
pub fn append_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Appends a length-delimited byte field body (`varint(len) ++ bytes`) to `out`.
/// The caller is responsible for having appended the tag first.
pub fn append_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    append_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// A cursor over a canoto-encoded byte slice.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wraps `buf` in a reader positioned at the start.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Returns `true` while bytes remain (`canoto.HasNext`).
    pub fn has_next(&self) -> bool {
        self.pos < self.buf.len()
    }

    /// Reads an unsigned LEB128 varint.
    pub fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let byte = *self.buf.get(self.pos).ok_or(DecodeError::UnexpectedEof)?;
            self.pos = self.pos.saturating_add(1);
            if shift >= 64 {
                return Err(DecodeError::VarintOverflow);
            }
            // The 10th byte may only carry a single significant bit.
            if shift == 63 && (byte & 0x7e) != 0 {
                return Err(DecodeError::VarintOverflow);
            }
            result |= (u64::from(byte & 0x7f)) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift = shift.saturating_add(7);
        }
        Ok(result)
    }

    /// Reads a field tag, returning `(field_number, wire_type)`
    /// (`canoto.ReadTag`).
    pub fn read_tag(&mut self) -> Result<(u32, u8), DecodeError> {
        let tag = self.read_varint()?;
        let wire_type = (tag & 0x7) as u8;
        let field_number = (tag >> 3) as u32;
        Ok((field_number, wire_type))
    }

    /// Reads a length-delimited byte field body (`canoto.ReadBytes`),
    /// borrowing from the underlying buffer.
    pub fn read_bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = usize::try_from(self.read_varint()?).map_err(|_| DecodeError::InvalidLength)?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DecodeError::InvalidLength)?;
        let out = self
            .buf
            .get(self.pos..end)
            .ok_or(DecodeError::InvalidLength)?;
        self.pos = end;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16384, u64::MAX] {
            let mut buf = Vec::new();
            append_varint(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.read_varint().unwrap(), v);
            assert!(!r.has_next());
        }
    }

    #[test]
    fn tag_encoding() {
        // field 1, Len => 0x0a; field 2, Len => 0x12; field 3, Len => 0x1a.
        let cases = [(1u32, 0x0au8), (2, 0x12), (3, 0x1a)];
        for (field, want) in cases {
            let mut buf = Vec::new();
            append_tag(&mut buf, field, WIRE_LEN);
            assert_eq!(buf, vec![want]);
            let mut r = Reader::new(&buf);
            assert_eq!(r.read_tag().unwrap(), (field, WIRE_LEN));
        }
    }

    #[test]
    fn bytes_field() {
        let mut buf = Vec::new();
        append_bytes(&mut buf, &[0xde, 0xad, 0xbe, 0xef]);
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_bytes().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn truncated_length() {
        let buf = [0x04u8, 0x01]; // declares 4 bytes, only 1 present
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_bytes(), Err(DecodeError::InvalidLength));
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Big-endian primitive reader/writer with sticky-error semantics.
//!
//! Port of Go's `utils/wrappers/packing.go` (`Packer`). A single struct serves
//! both write (append to an owned `Vec`) and read (advance an offset over a
//! borrowed slice) directions, matching the Go API method-for-method
//! ([`specs/03-core-primitives.md`] §2.1).
//!
//! **Sticky errors.** Once an operation fails, the first error is recorded and
//! every subsequent operation is a no-op returning a zero value (Go checks
//! `p.Errored()` at the top of each method). This is consensus-critical: it
//! ensures partial reads/writes never silently produce garbage.
//!
//! Go's negative-offset and `bytes < 0` branches are unreachable with `usize`
//! arithmetic, so they are intentionally omitted (the
//! [`crate::error::PackerError::NegativeOffset`] variant is retained only for
//! parity).

use crate::error::PackerError;

/// Width of a packed byte.
pub const BYTE_LEN: usize = 1;
/// Width of a packed `u16` (big-endian).
pub const SHORT_LEN: usize = 2;
/// Width of a packed `u32` (big-endian).
pub const INT_LEN: usize = 4;
/// Width of a packed `u64` (big-endian).
pub const LONG_LEN: usize = 8;
/// Width of a packed `bool`.
pub const BOOL_LEN: usize = 1;
/// Maximum length of a packed string (`math.MaxUint16` in Go).
pub const MAX_STRING_LEN: usize = u16::MAX as usize;

/// Backing buffer for a [`Packer`]: owned on write, borrowed on read.
enum PackerBuf<'a> {
    /// Write mode: bytes are appended to this owned vector.
    Write(Vec<u8>),
    /// Read mode: bytes are consumed from this borrowed slice.
    Read(&'a [u8]),
}

impl PackerBuf<'_> {
    /// Total number of bytes currently in the buffer (written, or available to
    /// read).
    fn len(&self) -> usize {
        match self {
            PackerBuf::Write(v) => v.len(),
            PackerBuf::Read(s) => s.len(),
        }
    }
}

/// Big-endian primitive (de)serializer with sticky-error semantics.
///
/// See the module docs for the read/write duality and the sticky-error model.
pub struct Packer<'a> {
    bytes: PackerBuf<'a>,
    offset: usize,
    max_size: usize,
    err: Option<PackerError>,
}

impl<'a> Packer<'a> {
    /// Creates a write packer with capacity hint `cap` and `max_size == cap`.
    #[must_use]
    pub fn new_write(cap: usize) -> Self {
        Self {
            bytes: PackerBuf::Write(Vec::with_capacity(cap)),
            offset: 0,
            max_size: cap,
            err: None,
        }
    }

    /// Creates a write packer that rejects writes growing the buffer past
    /// `max_size` (with [`PackerError::InsufficientLength`]).
    #[must_use]
    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            bytes: PackerBuf::Write(Vec::new()),
            offset: 0,
            max_size,
            err: None,
        }
    }

    /// Creates a read packer over `src`.
    #[must_use]
    pub fn new_read(src: &'a [u8]) -> Self {
        Self {
            bytes: PackerBuf::Read(src),
            offset: 0,
            max_size: src.len(),
            err: None,
        }
    }

    /// The first sticky error, if any.
    #[must_use]
    pub fn error(&self) -> Option<PackerError> {
        self.err
    }

    /// Whether the packer has recorded an error.
    #[must_use]
    pub fn errored(&self) -> bool {
        self.err.is_some()
    }

    /// The current read offset.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// A direction-agnostic cursor: the bytes written so far (write mode) or the
    /// read offset (read mode). Used by collection codecs to detect
    /// zero-length elements regardless of direction.
    #[must_use]
    pub fn cursor(&self) -> usize {
        match &self.bytes {
            PackerBuf::Write(v) => v.len(),
            PackerBuf::Read(_) => self.offset,
        }
    }

    /// Records `err` as the sticky error if none is set yet. Public so the
    /// codec layer (derive impls, `Vec`/map codecs) can surface higher-level
    /// failures (slice-length overflow, zero-length elements) through the same
    /// first-error-wins channel.
    pub fn add_external_error(&mut self, err: PackerError) {
        self.add_error(err);
    }

    /// Total bytes in the buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.len() == 0
    }

    /// Records `err` if no error is set yet (first error wins).
    fn add_error(&mut self, err: PackerError) {
        if self.err.is_none() {
            self.err = Some(err);
        }
    }

    /// Consumes a write packer, returning the written bytes (empty if errored
    /// or if used in read mode).
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        match self.bytes {
            PackerBuf::Write(v) => v,
            PackerBuf::Read(_) => Vec::new(),
        }
    }

    /// Borrows the underlying bytes (written so far, or the full read slice).
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match &self.bytes {
            PackerBuf::Write(v) => v.as_slice(),
            PackerBuf::Read(s) => s,
        }
    }

    /// Ensures `n` more bytes can be written without exceeding `max_size`.
    fn expand_write(&mut self, n: usize) -> bool {
        if self.errored() {
            return false;
        }
        match &self.bytes {
            PackerBuf::Write(v) => match v.len().checked_add(n) {
                Some(needed) if needed <= self.max_size => true,
                _ => {
                    self.add_error(PackerError::InsufficientLength);
                    false
                }
            },
            PackerBuf::Read(_) => {
                self.add_error(PackerError::InsufficientLength);
                false
            }
        }
    }

    /// Ensures `n` more bytes can be read from the current offset.
    fn check_space(&mut self, n: usize) -> bool {
        if self.errored() {
            return false;
        }
        match self.offset.checked_add(n) {
            Some(end) if end <= self.bytes.len() => true,
            _ => {
                self.add_error(PackerError::InsufficientLength);
                false
            }
        }
    }

    /// Appends raw bytes (write mode); no-op on error.
    fn append(&mut self, data: &[u8]) {
        if let PackerBuf::Write(v) = &mut self.bytes {
            v.extend_from_slice(data);
        }
    }

    // ----- write side -----

    /// Packs a single byte.
    pub fn pack_byte(&mut self, b: u8) {
        if self.expand_write(BYTE_LEN) {
            self.append(&[b]);
        }
    }

    /// Packs a `u16` big-endian.
    pub fn pack_u16(&mut self, v: u16) {
        if self.expand_write(SHORT_LEN) {
            self.append(&v.to_be_bytes());
        }
    }

    /// Packs a `u32` big-endian.
    pub fn pack_u32(&mut self, v: u32) {
        if self.expand_write(INT_LEN) {
            self.append(&v.to_be_bytes());
        }
    }

    /// Packs a `u64` big-endian.
    pub fn pack_u64(&mut self, v: u64) {
        if self.expand_write(LONG_LEN) {
            self.append(&v.to_be_bytes());
        }
    }

    /// Packs a `bool` as a single `0`/`1` byte.
    pub fn pack_bool(&mut self, b: bool) {
        self.pack_byte(u8::from(b));
    }

    /// Packs raw bytes with no length prefix.
    pub fn pack_fixed_bytes(&mut self, data: &[u8]) {
        if self.expand_write(data.len()) {
            self.append(data);
        }
    }

    /// Packs a `u32` length prefix followed by the raw bytes.
    pub fn pack_bytes(&mut self, data: &[u8]) {
        let Ok(len) = u32::try_from(data.len()) else {
            self.add_error(PackerError::InvalidInput);
            return;
        };
        self.pack_u32(len);
        self.pack_fixed_bytes(data);
    }

    /// Packs a `u16` length prefix followed by the UTF-8 bytes. Rejects strings
    /// longer than [`MAX_STRING_LEN`] with [`PackerError::InvalidInput`].
    pub fn pack_str(&mut self, s: &str) {
        let data = s.as_bytes();
        if data.len() > MAX_STRING_LEN {
            self.add_error(PackerError::InvalidInput);
            return;
        }
        // Safe: bounded by MAX_STRING_LEN == u16::MAX above.
        let len = data.len() as u16;
        self.pack_u16(len);
        self.pack_fixed_bytes(data);
    }

    // ----- read side -----

    /// Reads `n` bytes from the current offset, advancing it. Returns an empty
    /// slice on error.
    fn read(&mut self, n: usize) -> &[u8] {
        if !self.check_space(n) {
            return &[];
        }
        let start = self.offset;
        // Safe: check_space guarantees start + n <= len.
        let end = start.saturating_add(n);
        self.offset = end;
        match &self.bytes {
            PackerBuf::Write(v) => v.get(start..end).unwrap_or(&[]),
            PackerBuf::Read(s) => s.get(start..end).unwrap_or(&[]),
        }
    }

    /// Unpacks a single byte (zero on error).
    pub fn unpack_byte(&mut self) -> u8 {
        self.read(BYTE_LEN).first().copied().unwrap_or(0)
    }

    /// Unpacks a `u16` big-endian (zero on error).
    pub fn unpack_u16(&mut self) -> u16 {
        let b = self.read(SHORT_LEN);
        match b.try_into() {
            Ok(arr) => u16::from_be_bytes(arr),
            Err(_) => 0,
        }
    }

    /// Unpacks a `u32` big-endian (zero on error).
    pub fn unpack_u32(&mut self) -> u32 {
        let b = self.read(INT_LEN);
        match b.try_into() {
            Ok(arr) => u32::from_be_bytes(arr),
            Err(_) => 0,
        }
    }

    /// Unpacks a `u64` big-endian (zero on error).
    pub fn unpack_u64(&mut self) -> u64 {
        let b = self.read(LONG_LEN);
        match b.try_into() {
            Ok(arr) => u64::from_be_bytes(arr),
            Err(_) => 0,
        }
    }

    /// Unpacks a `bool`. Rejects any byte other than `0`/`1` with
    /// [`PackerError::BadBool`] (returns `false`).
    pub fn unpack_bool(&mut self) -> bool {
        match self.unpack_byte() {
            0 => false,
            1 => true,
            _ => {
                self.add_error(PackerError::BadBool);
                false
            }
        }
    }

    /// Reads `n` raw bytes with no length prefix (empty on error).
    pub fn unpack_fixed_bytes(&mut self, n: usize) -> Vec<u8> {
        self.read(n).to_vec()
    }

    /// Unpacks a `u32`-length-prefixed byte slice.
    pub fn unpack_bytes(&mut self) -> Vec<u8> {
        let len = self.unpack_u32() as usize;
        self.unpack_fixed_bytes(len)
    }

    /// Unpacks a `u32`-length-prefixed byte slice, rejecting `len > limit` with
    /// [`PackerError::Oversized`].
    pub fn unpack_limited_bytes(&mut self, limit: u32) -> Vec<u8> {
        let len = self.unpack_u32();
        if len > limit {
            self.add_error(PackerError::Oversized);
            return Vec::new();
        }
        self.unpack_fixed_bytes(len as usize)
    }

    /// Unpacks a `u16`-length-prefixed UTF-8 string. Invalid UTF-8 yields
    /// [`PackerError::InvalidInput`] and an empty string.
    pub fn unpack_str(&mut self) -> String {
        let len = self.unpack_u16() as usize;
        self.read_str(len)
    }

    /// Unpacks a `u16`-length-prefixed UTF-8 string, rejecting `len > limit`
    /// with [`PackerError::Oversized`].
    pub fn unpack_limited_str(&mut self, limit: u16) -> String {
        let len = self.unpack_u16();
        if len > limit {
            self.add_error(PackerError::Oversized);
            return String::new();
        }
        self.read_str(len as usize)
    }

    /// Reads `len` bytes and interprets them as UTF-8.
    fn read_str(&mut self, len: usize) -> String {
        let raw = self.read(len).to_vec();
        if self.errored() {
            return String::new();
        }
        match String::from_utf8(raw) {
            Ok(s) => s,
            Err(_) => {
                self.add_error(PackerError::InvalidInput);
                String::new()
            }
        }
    }
}

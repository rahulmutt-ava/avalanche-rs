// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-codec` — the hand-written, byte-exact linear codec.
//!
//! Tier T0 (primitives). Owning spec: `specs/03-core-primitives.md` §2,
//! `specs/15-serialization-and-wire-formats.md` §4. Implemented across M0:
//!
//! - [`packer`] — big-endian primitive reader/writer with sticky errors (M0.14)
//! - object-safe [`Serializable`]/[`Deserializable`] traits + `#[derive(AvaCodec)]`
//!   (re-exported from `ava-codec-derive`) (M0.15)
//! - [`manager`] — versioned `Manager` with the `ExtraSpace` trailing-byte check (M0.16)
//! - [`linearcodec`] — sequential `u32` typeID registry (M0.16)
//! - [`codectest`] — generic conformance suite, `testutil` feature (M0.16)
//! - [`error`] — `PackerError` / `CodecError`
//!
//! # Wire format
//!
//! The traits below define a *streaming* (de)serializer over a [`packer::Packer`].
//! Per-kind encoding (transcribed from Go `reflectcodec/type_codec.go`):
//!
//! - integers: big-endian, fixed width
//! - `bool`: a single `0`/`1` byte (decode rejects other values)
//! - `String`: `u16` length prefix + UTF-8 bytes
//! - `[u8; N]`: `N` raw bytes, no prefix; `[T; N]` (`T != u8`): elements back-to-back
//! - `Vec<u8>`: `u32` length prefix + raw bytes; `Vec<T>`: `u32` count + each element
//! - struct: concatenation of `#[codec]` fields in declaration order
//! - interface enum (`#[codec(type_registry)]`): `u32` typeID + the variant value
//! - `Box<T>`: transparent (encodes as `T`)
//!
//! Slices reject `len > i32::MAX` ([`error::CodecError::MaxSliceLenExceeded`]),
//! surfaced through the packer's sticky error. `Option<T>` on a serialized field
//! is a compile error (no presence byte exists on the Avalanche wire).

#![forbid(unsafe_code)]

pub use ava_codec_derive::AvaCodec;

#[cfg(any(test, feature = "testutil"))]
pub mod codectest;
pub mod error;
pub mod linearcodec;
pub mod manager;
pub mod packer;

use crate::error::PackerError;
use crate::packer::Packer;

/// Maximum encodable slice/map length (`math.MaxInt32` in Go).
pub const MAX_SLICE_LEN: usize = i32::MAX as usize;

/// Capacity hint for decode-side slice allocation (`codec.Manager` in Go uses
/// 128). Caps speculative allocation from an attacker-controlled count.
pub const INITIAL_SLICE_CAP: usize = 128;

/// A value that can be marshaled into the linear codec stream.
///
/// Object-safe so [`manager::Manager`] can hold `&dyn Serializable`. Generated
/// by `#[derive(AvaCodec)]` for `#[codec]`-tagged structs and
/// `#[codec(type_registry)]` enums; also implemented for the primitive building
/// blocks (integers, `bool`, `String`, `Vec<T>`, `[u8; N]`, `Box<T>`, …).
pub trait Serializable {
    /// Appends this value's wire bytes to `p` (no version prefix).
    fn marshal_into(&self, p: &mut Packer);

    /// The exact marshaled byte length, excluding the 2-byte codec version that
    /// [`manager::Manager`] prepends.
    fn size(&self) -> usize;
}

/// A value that can be unmarshaled from the linear codec stream in place.
///
/// Object-safe; generated alongside [`Serializable`] by `#[derive(AvaCodec)]`.
pub trait Deserializable {
    /// Reads this value's wire bytes from `p`, advancing the offset. On error the
    /// packer becomes sticky and the destination is left partially populated.
    fn unmarshal_from(&mut self, p: &mut Packer);
}

// ----- primitive impls -----

macro_rules! impl_int_codec {
    ($t:ty, $pack:ident, $unpack:ident, $len:expr) => {
        impl Serializable for $t {
            fn marshal_into(&self, p: &mut Packer) {
                p.$pack(*self);
            }
            fn size(&self) -> usize {
                $len
            }
        }
        impl Deserializable for $t {
            fn unmarshal_from(&mut self, p: &mut Packer) {
                *self = p.$unpack();
            }
        }
    };
}

impl Serializable for u8 {
    fn marshal_into(&self, p: &mut Packer) {
        p.pack_byte(*self);
    }
    fn size(&self) -> usize {
        packer::BYTE_LEN
    }
}
impl Deserializable for u8 {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        *self = p.unpack_byte();
    }
}

impl_int_codec!(u16, pack_u16, unpack_u16, packer::SHORT_LEN);
impl_int_codec!(u32, pack_u32, unpack_u32, packer::INT_LEN);
impl_int_codec!(u64, pack_u64, unpack_u64, packer::LONG_LEN);

impl Serializable for bool {
    fn marshal_into(&self, p: &mut Packer) {
        p.pack_bool(*self);
    }
    fn size(&self) -> usize {
        packer::BOOL_LEN
    }
}
impl Deserializable for bool {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        *self = p.unpack_bool();
    }
}

impl Serializable for String {
    fn marshal_into(&self, p: &mut Packer) {
        p.pack_str(self);
    }
    fn size(&self) -> usize {
        // u16 length prefix + UTF-8 bytes.
        packer::SHORT_LEN.saturating_add(self.len())
    }
}
impl Deserializable for String {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        *self = p.unpack_str();
    }
}

impl<T: Serializable> Serializable for Box<T> {
    fn marshal_into(&self, p: &mut Packer) {
        (**self).marshal_into(p);
    }
    fn size(&self) -> usize {
        (**self).size()
    }
}
impl<T: Deserializable> Deserializable for Box<T> {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        (**self).unmarshal_from(p);
    }
}

// Fixed arrays: NO length prefix. `[u8; N]` is N raw bytes; `[T; N]` is each
// element back-to-back (`type_codec.go` array handling).

impl<const N: usize> Serializable for [u8; N] {
    fn marshal_into(&self, p: &mut Packer) {
        p.pack_fixed_bytes(self);
    }
    fn size(&self) -> usize {
        N
    }
}
impl<const N: usize> Deserializable for [u8; N] {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        let raw = p.unpack_fixed_bytes(N);
        if let Ok(arr) = <[u8; N]>::try_from(raw.as_slice()) {
            *self = arr;
        }
    }
}

/// Packs a `u32` slice/map count, rejecting `len > i32::MAX` via the packer's
/// sticky [`PackerError::Oversized`] (the [`manager::Manager`] surfaces this as
/// [`error::CodecError::MaxSliceLenExceeded`]). Shared by the collection impls
/// and the derive macro.
pub fn pack_count(p: &mut Packer, len: usize) {
    if len > MAX_SLICE_LEN {
        p.add_external_error(PackerError::Oversized);
        return;
    }
    // Safe: bounded by MAX_SLICE_LEN == i32::MAX above.
    p.pack_u32(len as u32);
}

// NOTE: there is intentionally NO dedicated `Vec<u8>` impl. A `Vec<u8>` flows
// through the generic `Vec<T>` impl below; because `u8::marshal_into` writes one
// raw byte, the bytes are identical to Go's `u32` count + raw-bytes fast path
// (`type_codec.go` slice handling), just without the bulk-copy specialization.

impl<T: Serializable> Serializable for Vec<T> {
    fn marshal_into(&self, p: &mut Packer) {
        pack_count(p, self.len());
        for elem in self {
            let before = p.cursor();
            elem.marshal_into(p);
            // Zero-length-element guard (ErrMarshalZeroLength): only triggers
            // for genuinely zero-size elements (e.g. an empty struct), never for
            // `u8` which always writes one byte.
            if !p.errored() && p.cursor() == before {
                p.add_external_error(PackerError::InvalidInput);
                return;
            }
        }
    }
    fn size(&self) -> usize {
        let mut total = packer::INT_LEN;
        for elem in self {
            total = total.saturating_add(elem.size());
        }
        total
    }
}
impl<T: Deserializable + Default> Deserializable for Vec<T> {
    fn unmarshal_from(&mut self, p: &mut Packer) {
        let count = p.unpack_u32() as usize;
        if p.errored() {
            return;
        }
        let mut out = Vec::with_capacity(count.min(INITIAL_SLICE_CAP));
        for _ in 0..count {
            let before = p.cursor();
            let mut elem = T::default();
            elem.unmarshal_from(p);
            if p.errored() {
                return;
            }
            if p.cursor() == before {
                p.add_external_error(PackerError::InvalidInput);
                return;
            }
            out.push(elem);
        }
        *self = out;
    }
}

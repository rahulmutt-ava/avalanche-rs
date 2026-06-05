// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec error enums (`thiserror`).
//!
//! Two enums mirror the two Go error families:
//! - [`PackerError`] — the `utils/wrappers/packing.go` `Errs` identities
//!   ([`specs/03-core-primitives.md`] §2.1, §7).
//! - [`CodecError`] — the `codec`/`codec/manager.go`/`codec/reflectcodec`
//!   sentinel set ([`specs/03-core-primitives.md`] §2.2).

use thiserror::Error;

/// Errors raised by the [`crate::packer::Packer`].
///
/// Mirrors the sentinel errors in Go's `utils/wrappers/packing.go`. The packer
/// is *sticky*: the first error wins and every subsequent operation is a no-op
/// returning a zero value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PackerError {
    /// Not enough bytes remaining to satisfy a read, or the write would exceed
    /// `max_size`. Go: `errBadLength` / `ErrInsufficientLength`.
    #[error("packer has insufficient length for the operation")]
    InsufficientLength,

    /// The offset went negative. Unreachable with `usize` arithmetic in Rust;
    /// retained for parity with Go's `errNegativeOffset`.
    #[error("packer offset is negative")]
    NegativeOffset,

    /// An input was too large to encode (e.g. a string longer than
    /// [`crate::packer::MAX_STRING_LEN`]). Go: `errInvalidInput`.
    #[error("packer received invalid input")]
    InvalidInput,

    /// A boolean byte was neither `0` nor `1`. Go: `errBadBool`.
    #[error("unpacked bool was neither 0 nor 1")]
    BadBool,

    /// A length-limited read exceeded its limit. Go: `errOversized`.
    #[error("packer read exceeded the supplied limit")]
    Oversized,
}

/// Result alias for codec-layer (Manager / Codec / derive) operations.
pub type Result<T> = core::result::Result<T, CodecError>;

/// Errors raised by the codec [`crate::manager::Manager`], the registry, and the
/// derive-generated (de)serializers.
///
/// Variant names mirror the Go `codec` sentinel errors so `errors.Is`-style
/// assertions port directly (see [`specs/03-core-primitives.md`] §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CodecError {
    /// A type/kind that the codec cannot encode. Go: `errUnsupportedType`.
    #[error("unsupported type for the codec")]
    UnsupportedType,

    /// A slice/map length exceeded `i32::MAX`. Go: `errMaxSliceLenExceeded`.
    #[error("slice length exceeds the maximum encodable length")]
    MaxSliceLenExceeded,

    /// A concrete type did not implement the target interface. Go:
    /// `errDoesNotImplementInterface`. Structurally impossible with enum
    /// dispatch; kept for parity.
    #[error("type does not implement the target interface")]
    DoesNotImplementInterface,

    /// A struct field was unexported. N/A in Rust; retained for parity.
    #[error("struct field is unexported")]
    UnexportedField,

    /// An element serialized to zero bytes during marshal. Go:
    /// `errMarshalZeroLength`.
    #[error("attempted to marshal a zero-length element")]
    MarshalZeroLength,

    /// An element would deserialize from zero bytes during unmarshal. Go:
    /// `errUnmarshalZeroLength`.
    #[error("attempted to unmarshal a zero-length element")]
    UnmarshalZeroLength,

    /// No codec registered for the requested version. Go: `errUnknownVersion`.
    #[error("codec version is not registered")]
    UnknownVersion,

    /// Attempted to marshal a nil pointer. Go: `errMarshalNil`.
    #[error("attempted to marshal a nil value")]
    MarshalNil,

    /// Attempted to unmarshal into a nil destination. Go: `errUnmarshalNil`.
    #[error("attempted to unmarshal into a nil value")]
    UnmarshalNil,

    /// Input exceeds the manager's `max_size`. Go: `errUnmarshalTooBig`.
    #[error("input exceeds the maximum decode size")]
    UnmarshalTooBig,

    /// Could not pack the 2-byte version prefix. Go: `errCantPackVersion`.
    #[error("could not pack the codec version")]
    CantPackVersion,

    /// Could not unpack the 2-byte version prefix. Go: `errCantUnpackVersion`.
    #[error("could not unpack the codec version")]
    CantUnpackVersion,

    /// A version was registered twice. Go: `errDuplicatedVersion`.
    #[error("codec version is already registered")]
    DuplicatedVersion,

    /// Bytes remained after a successful unmarshal. Go: `errExtraSpace`. Part of
    /// consensus validation — never skipped.
    #[error("unmarshal left trailing bytes (extra space)")]
    ExtraSpace,

    /// A concrete type was registered twice in one registry. Go:
    /// `errDuplicateType`.
    #[error("type is already registered")]
    DuplicateType,

    /// An interface field carried a typeID with no registered concrete type.
    /// Go: `errUnknownTypeID`.
    #[error("unknown type id {0}")]
    UnknownTypeId(u32),

    /// A low-level packer error surfaced through the codec layer.
    #[error("packer error: {0}")]
    Packer(#[from] PackerError),
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-codec` — the hand-written, byte-exact linear codec.
//!
//! Tier T0 (primitives). Owning spec: `specs/03-core-primitives.md` §2,
//! `specs/15-serialization-and-wire-formats.md` §4. Implemented across M0:
//!
//! - [`packer`] — big-endian primitive reader/writer with sticky errors (M0.14)
//! - object-safe `Serializable`/`Deserializable` traits + `#[derive(AvaCodec)]`
//!   (re-exported from `ava-codec-derive`) (M0.15)
//! - [`manager`] — versioned `Manager` with the `ExtraSpace` trailing-byte check (M0.16)
//! - [`linearcodec`] — sequential `u32` typeID registry (M0.16)
//! - [`codectest`] — generic conformance suite, `testutil` feature (M0.16)
//! - [`error`] — `PackerError` / `CodecError`
//!
//! Modules are scaffolded empty in M0.1 and filled in by their owning tasks.

#![forbid(unsafe_code)]

pub use ava_codec_derive::AvaCodec;

#[cfg(any(test, feature = "testutil"))]
pub mod codectest;
pub mod error;
pub mod linearcodec;
pub mod manager;
pub mod packer;

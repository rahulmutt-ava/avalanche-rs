// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 32-byte `Id` newtype + ops (`prefix`/`append`/`xor`/`bit`) and CB58 forms.
//!
//! TODO(M0.5): implement `Id([u8;32])`, constants, `from_slice`, `prefix`/
//! `append` (inline BE writer + `sha2::Sha256`), `xor`, `bit`, `hex`.
//! TODO(M0.6): `Display`/`FromStr`/serde CB58 string forms (null = no-op).
//! Owning spec: `specs/03-core-primitives.md` §1.1.

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Codec error enums (`thiserror`).
//!
//! TODO(M0.14): `PackerError { InsufficientLength, NegativeOffset, InvalidInput,
//! BadBool, Oversized }`.
//! TODO(M0.16): `CodecError` (the Manager/registry error list from
//! `specs/03-core-primitives.md` §2.2: `DuplicatedVersion`, `UnknownVersion`,
//! `UnmarshalTooBig`, `CantUnpackVersion`, `ExtraSpace`, `MaxSliceLenExceeded`,
//! unknown typeID, etc.).

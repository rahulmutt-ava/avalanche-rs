// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-types` error enum (`thiserror`).
//!
//! TODO(M0.5): define `Error` with variants `InvalidHashLen`, `NoIdWithAlias`,
//! `AliasAlreadyMapped`, `ShortNodeId`, `MissingQuotes` and a crate
//! `Result<T> = core::result::Result<T, Error>` alias.
//! Owning spec: `specs/03-core-primitives.md` §7.

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Bidirectional id<->alias mapping.
//!
//! TODO(M0.7): implement `Aliaser` with `alias->id` / `id->Vec<alias>` maps
//! behind `parking_lot::RwLock`; `primary_alias_or_default`,
//! `get_relevant_aliases` (strip the `alias == id.to_string()` self-alias),
//! duplicate alias -> `Error::AliasAlreadyMapped`.
//! Owning spec: `specs/03-core-primitives.md` §1.3.

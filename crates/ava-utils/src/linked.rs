// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `LinkedHashmap<K,V>` — insertion-ordered map; re-`put` moves key to back.
//!
//! TODO(M0.9): implement over `indexmap` + an explicit move-to-back on re-put.
//! Owning spec: `specs/03-core-primitives.md` §4.3.

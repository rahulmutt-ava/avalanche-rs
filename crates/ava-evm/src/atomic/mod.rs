// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic X<->C transactions: types/codec, mempool, backend + atomic trie,
//! `EVMStateTransfer` state hook, semantic verify (G3, spec 10 §6).
//! Populated by M6.14..M6.18.

pub mod mempool;
pub mod tx;

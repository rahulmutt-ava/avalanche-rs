// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `chains/atomic` (specs 07 §3.1) — the cross-chain atomic shared memory owner.
//!
//! [`Memory`] backs a set of per-chain [`SharedMemory`](ava_vm::components::avax::shared_memory::SharedMemory)
//! views over one shared base [`DynDatabase`](ava_database::DynDatabase). Two
//! chains communicate through a `sharedID`-namespaced sub-database; on top of
//! it, fixed inbound/outbound value/index prefixes route a chain's reads and
//! writes (the smaller/larger chain id picks the prefix pair so the two chains
//! agree on which half is inbound vs outbound — Go `chains/atomic/prefixes.go`).
//!
//! ## Port note (index layout)
//!
//! The value-DB key layout (`sharedID ‖ valuePrefix ‖ key` over the
//! prefix-hashing scheme of `database/prefixdb`) is reproduced byte-for-byte.
//! The trait **index** is stored as `indexPrefix ‖ trait ‖ key → ∅` rather than
//! Go's `linkeddb`-encoded list; `Indexed` scans that range. This is observably
//! identical for `Get`/`Indexed`/`Apply` but is **not** on-disk byte-compatible
//! with the Go index encoding — cross-impl shared-memory interop is an M9
//! concern (specs 02). The `dbElement` value encoding *is* byte-exact
//! (linear-codec `{Present, Value, Traits}` with the 2-byte version prefix).

pub mod shared_memory;

pub use shared_memory::Memory;

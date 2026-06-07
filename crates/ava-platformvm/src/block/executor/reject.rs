// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block rejector (`vms/platformvm/block/executor/rejector.go`, specs 08
//! §4.2).
//!
//! Rejecting a block discards its cached verify diff(s) — see
//! [`BlockManager::reject`](super::BlockManager::reject). Go additionally
//! re-issues the rejected block's txs to the mempool (unless partial-syncing);
//! that is the block builder's concern (M4.25) and is intentionally out of scope
//! for the read-only-sync executor, so the Rust port only frees the cache.

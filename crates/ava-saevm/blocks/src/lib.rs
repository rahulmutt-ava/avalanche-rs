// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-blocks` — the byte-exact SAE block, its lifecycle state machine,
//! the settlement range, and the in-memory GC counter (specs/11 §4).
//!
//! A SAE block **is** a standard Ethereum block (geth/`libevm`-byte-identical
//! RLP; hash = `keccak256(RLP(header))`). It is **not** the coreth-custom
//! `AvaHeader` of the synchronous C-Chain — under SAE the standard header
//! fields are merely *reinterpreted* (e.g. `Root` is the settled ancestor's
//! post-exec state root; `BaseFee`/`GasLimit` are the builder's worst-case
//! prediction). See specs/11 §4.1.
//!
//! On top of the wire block, [`Block`] tracks an async-execution lifecycle:
//! `NotExecuted` (accepted) → `Executed` → `Settled` (specs/11 §4.2). The
//! transitions follow the strict **D→M→I→X** ordering of the Go reference
//! (`blocks/execution.go`, `blocks/settlement.go`):
//!
//! * **D**isk artefacts persisted (the caller's responsibility — saedb/rawdb
//!   live in sibling crates; modelled here as a fallible `persist` step run
//!   first inside [`Block::mark_executed`]).
//! * set the execution **M**emory pointer (`execution`, once-only CAS),
//! * set the **I**nterim execution time,
//! * fire the e**X**ecuted notification.
//!
//! Settlement severs the ancestry pointers (CAS → `None`) so the linked list of
//! ancestors can be garbage-collected; a [`Drop`] impl decrements an
//! [`AtomicI64`](std::sync::atomic::AtomicI64) ([`in_memory_block_count`]) to
//! make the GC observable in tests (specs/11 §10 invariant 8).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::cast_possible_truncation)]
#![deny(clippy::cast_sign_loss)]
#![deny(clippy::cast_possible_wrap)]

mod lifecycle;
mod parse;
mod settlement;

pub use lifecycle::{
    Ancestry, Block, Error, ExecutionArtefacts, LifeCycleStage, WorstCaseBounds,
    in_memory_block_count,
};
pub use parse::{ParseError, parse_block};
pub use settlement::{Range, last_to_settle_at};

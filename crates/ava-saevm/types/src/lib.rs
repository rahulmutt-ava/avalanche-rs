// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-types` — the SAE per-block `ExecutionResults` blob plus a
//! height-indexed store for it (specs/11 §4.1/§7).
//!
//! Also the single import point for the reth/alloy block & header alias types
//! SAE crates share, re-exported from the `ava-evm-reth` facade (spec 10 §17.1)
//! so downstream SAE crates name them via `ava_saevm_types::{..}`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

mod execution;

pub use execution::{
    DbError, DecodeError, EXECUTION_RESULTS_LEN, ExecutionResults, ExecutionResultsDb,
};

// Shared reth/alloy primitives + sealed block/header aliases (specs/11 §4.1,
// §7 — re-exported through the ava-evm-reth G0 facade).
pub use ava_evm_reth::{Address, B256, Bytes, SealedBlock, SealedHeader, U256};

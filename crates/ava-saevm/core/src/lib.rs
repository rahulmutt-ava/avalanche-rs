// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-core` — the SAE core VM: three frontiers, settlement, the block
//! lifecycle, and the RPC label mapping (specs/11 §1/§5).
//!
//! M7.17 delivers the consensus-state core: the three monotonic frontiers
//! ([`Frontier`] — `LastSettled`/`LastExecuted`/`LastAccepted`, specs/11 §1.1)
//! with lock-free reads (specs/11 §13.5), the consensus-critical map (the `A..S`
//! window), and the [`settle()`] driver that marks the settlement set `Σ` in
//! increasing height on the gas-time clock (specs/11 §1.2). M7.18 added the full
//! VM lifecycle (`BuildBlock` / `VerifyBlock` / `Accept` / `SetPreference`) and
//! M7.24 the restart [`recovery`] procedure that rebuilds A/E/S from disk.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod block_handle;
pub mod frontier;
pub mod recovery;
pub mod rpc;
pub mod settle;
pub mod vm;

pub use block_handle::SaeBlock;
pub use frontier::Frontier;
pub use recovery::{RecoverError, Recovered, RecoverySource, recover};
pub use rpc::{RpcBlockLabel, RpcError, resolve_rpc_number};
pub use settle::{SettleError, settle};
pub use vm::{BlockBuilderSeam, BuildError, Error, ExecutorSeam, Vm};

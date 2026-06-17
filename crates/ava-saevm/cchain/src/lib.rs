// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-cchain` — the minimal EVM C-Chain on the SAE VM: hooks, atomic
//! import/export, the initialize harness, and the `/avax` API (specs/11 §8).
//!
//! M7.21 implements the SAE [`hook::PointsG`] surface as [`CChainHooks`]:
//! deterministic header building, the ACP-176 gas config after a block,
//! end-of-block mint/burn ops for atomic Import/Export of AVAX, and block
//! rebuild for verification. M7.22 added the atomic Import/Export tx codec +
//! [`State`] + [`AtomicTxpool`]. M7.23 ([`vm`]) supplies the VM `Initialize`
//! harness — composing [`ava_saevm_core::Vm`] (the `sae::Vm` analog) with the
//! C-Chain hooks + atomic txpool (specs/11 §5) — and the [`api`] `/avax`
//! JSON-RPC service. M7.37 ([`block_ext`]) adds the `ParseBlock` extData-hash
//! verification boundary ([`vm::Vm::parse_block`]).
//!
//! [`hook::PointsG`]: ava_saevm_hook::PointsG

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod api;
pub mod block_ext;
pub mod dynamic;
pub mod gossip;
pub mod hooks;
pub mod state;
pub mod tx;
pub mod txpool;
pub mod vm;

pub use api::{AVAX_EXTENSION_PATH, AVAX_SERVICE_NAME, AvaxService};
pub use block_ext::{EMPTY_EXT_DATA_HASH, calc_ext_data_hash, empty_ext_data_hash};
pub use gossip::{
    BloomSet, GossipMarshaller, GossipTransport, GossipTx, Gossipable, NoGossipTransport,
    PULL_GOSSIP_PERIOD, PUSH_GOSSIP_PERIOD, PullGossiper, PushGossiper,
};
pub use hooks::{
    AtomicOp, AtomicOpSource, BLACKHOLE_ADDR, CChainHooks, Error, GAS_CONFIG_AFTER_TARGET,
    Rebuilder,
};
pub use state::State;
pub use tx::{Credential, Export, Import, Input, Output, Tx, Unsigned};
pub use txpool::{AtomicTxpool, EvmPoolStub, WaitPool, WaitSource};
pub use vm::GossipConfig;
pub use vm::{CChainCoreVm, Vm};

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-vm-rpc` — the rpcchainvm out-of-process gRPC plugin host/guest
//! (`vms/rpcchainvm`, specs 07 §5).
//!
//! A VM may run as a **separate process** speaking gRPC, so the node and the VM
//! can be different binaries (and different languages). The compatibility
//! contract is byte-exact `proto/vm` + the avalanchego **v45 reverse-dial
//! handshake** (specs 07 §5.1; 00 §11.1.1 — it is **NOT** hashicorp go-plugin).
//!
//! * [`host`] — [`host::RpcChainVm`], a [`ChainVm`](ava_vm::block::ChainVm) that
//!   translates each call to a `proto/vm` RPC over the dialed channel.
//! * [`guest`] — [`guest::VmServer`], a tonic `VM` service delegating to a local
//!   `ChainVm`, plus [`guest::serve`] (the plugin `main()` entrypoint).
//! * [`runtime`] — the handshake `Runtime` service.
//! * [`proxy`] — the proxied callback services (rpcdb/appsender/sharedmemory/
//!   validatorstate/warp/aliasreader; specs 07 §5.4).
//!
//! Generated protos live in [`pb`] (built into `OUT_DIR` by `build.rs`, **not**
//! committed; specs 01 §8.1).

// This crate replicates the workspace lints rather than inheriting them via
// `[lints] workspace = true`, because it needs one audited `unsafe` (the Linux
// `PR_SET_PDEATHSIG` `pre_exec`, 00 §7.6). `deny` (not `forbid`) lets that single
// block opt out with `#[allow(unsafe_code)]`; every other `.rs` stays unsafe-free.
#![deny(unsafe_code)]
#![warn(missing_docs, unused_crate_dependencies)]
#![deny(clippy::all)]
#![deny(clippy::unwrap_used, clippy::dbg_macro)]
#![warn(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::todo
)]

use std::time::Duration;

// `anyhow`/`thiserror` are pulled in for parity with the other crates' error
// stacks but this crate surfaces `ava_vm::Error` directly, so reference them to
// keep `unused_crate_dependencies` quiet.
use {anyhow as _, thiserror as _};

pub mod guest;
pub mod host;
pub mod proxy;
pub mod runtime;

/// Generated tonic/prost types for every rpcchainvm proto package (see
/// `build.rs`). The byte-exact `.proto` sources are the shared wire contract, so
/// a Rust host/guest interoperates with a Go guest/host.
#[allow(
    missing_docs,
    dead_code,
    clippy::all,
    clippy::pedantic,
    unreachable_pub,
    clippy::doc_markdown
)]
pub mod pb {
    /// `proto/vm/vm.proto` — the `VM` service (07 §5.4).
    pub mod vm {
        tonic::include_proto!("vm");

        /// `proto/vm/runtime/runtime.proto` — the handshake `Runtime` service
        /// (07 §5.1). Nested because its proto package is `vm.runtime`.
        pub mod runtime {
            tonic::include_proto!("vm.runtime");
        }
    }
    /// `proto/appsender/appsender.proto` — the `AppSender` callback (07 §2.6).
    pub mod appsender {
        tonic::include_proto!("appsender");
    }
    /// `proto/sharedmemory/sharedmemory.proto` — the `SharedMemory` callback.
    pub mod sharedmemory {
        tonic::include_proto!("sharedmemory");
    }
    /// `proto/validatorstate/validator_state.proto` — the `ValidatorState`
    /// callback.
    pub mod validatorstate {
        tonic::include_proto!("validatorstate");
    }
    /// `proto/warp/message.proto` — the warp `Signer` callback.
    pub mod warp {
        tonic::include_proto!("warp");
    }
    /// `proto/aliasreader/aliasreader.proto` — the `AliasReader` callback.
    pub mod aliasreader {
        tonic::include_proto!("aliasreader");
    }
    /// The vendored Prometheus client-model (`io/prometheus/client`), imported
    /// by `vm.proto`'s `Gather` RPC.
    pub mod io {
        pub mod prometheus {
            pub mod client {
                tonic::include_proto!("io.prometheus.client");
            }
        }
    }
}

/// The env var the host sets to hand the runtime (handshake) server address to
/// the spawned plugin (`AVALANCHE_VM_RUNTIME_ENGINE_ADDR`; specs 07 §5.1).
pub const ENGINE_ADDRESS_KEY: &str = "AVALANCHE_VM_RUNTIME_ENGINE_ADDR";

/// The rpcchainvm protocol version (`version/constants.go RPCChainVMProtocol`).
/// Bump in lock-step with avalanchego (specs 07 §5.1).
pub const RPC_CHAIN_VM_PROTOCOL: u32 = 45;

/// How long the host waits for the plugin's `Runtime.Initialize` handshake
/// before failing with [`ava_vm::Error::HandshakeFailed`] (specs 07 §5.1).
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the host waits for the plugin to exit on graceful shutdown before
/// killing it (specs 07 §5.1).
pub const DEFAULT_GRACEFUL_TIMEOUT: Duration = Duration::from_secs(5);

/// The gRPC max recv/send message size = the p2p message limit (specs 07 §5.4).
/// avalanchego's `grpcutils` uses `math.MaxInt` for the rpcchainvm channel; we
/// pin a generous 2 GiB ceiling (the practical limit for a single block/ancestor
/// batch), matching the "no artificial cap below the p2p limit" intent.
pub const MAX_MESSAGE_SIZE: usize = 2 * 1024 * 1024 * 1024;

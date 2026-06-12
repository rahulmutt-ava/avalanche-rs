// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `eth_*` + `avax.*` RPC over Firewood + fee/accepted-tag overrides
//! (G8, spec 10 §9). Populated by M6.23/M6.24.
//!
//! All handlers are plain structs returning [`serde_json::Value`] (the M6.23
//! precedent), NOT a `jsonrpsee`/`reth-rpc` server: the jsonrpsee-vs-axum mount
//! topology is deferred to the 12-node milestone (spec §9.2).
//!
//! [`service`] (M8.22) bridges these handler bodies onto the in-process
//! [`ava_vm::VmHttpService`] seam so `EvmVm::create_handlers` returns real
//! `/rpc`, `/ws`, `/avax`, and `/admin` mounts (coreth `CreateHandlers`).

pub mod admin;
pub mod avax;
pub mod eth;
pub mod service;

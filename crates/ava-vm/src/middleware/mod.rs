// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! VM middleware decorators (specs 07 §6).
//!
//! Both [`MeterVm`] and [`TracedVm`] wrap a [`ChainVm`](crate::block::ChainVm)
//! and *are* a `ChainVm`, delegating the full `Vm`/`AppHandler`/`HealthCheck`/
//! `Connector`/`ChainVm` surface while recording metrics / opening tracing spans.
//! They compose with proposervm via the chain pipeline (07 §8) in the exact
//! wrapping order from 00 §11.1.2 (innermost first):
//!
//! ```text
//! inner VM -> metervm -> tracedvm("primaryAlias") -> proposervm
//!          -> metervm -> tracedvm("proposervm")
//! ```
//!
//! Because Go re-exposes the optional `BatchedChainVM`/`StateSyncableVM`/
//! `*WithContext` capabilities via interface embedding + type-assertion, each
//! wrapper probes the inner VM's capabilities once at construction and re-exposes
//! them **wrapped** (the `as_batched`/`as_state_syncable`/`as_*_with_context`
//! probes return `Some(self)`), so a wrapped proposervm keeps its batched /
//! state-sync surface.

pub mod meter;
pub mod traced;

pub use meter::{BlockMetrics, MeterVm};
pub use traced::TracedVm;

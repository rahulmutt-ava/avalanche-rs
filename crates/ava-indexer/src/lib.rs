// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-indexer` — the accepted-container indexer + `/ext/index` API.
//!
//! Mirrors avalanchego `indexer/` (specs 12 §5, 14 §7, 17 §2.2 #20 / §3). For
//! **Primary-Network chains only**, persists an append-only index of accepted
//! containers keyed both by container id and by monotonic acceptance index,
//! and serves it over the gorilla-parity JSON-RPC `index.*` service mounted at
//! `/ext/index/<chainAlias>/{block,tx,vtx}`.
//!
//! - [`container`] — the persisted [`Container`](container::Container) and its
//!   byte-exact linear codec (Go `container.go` + `codec.go`).
//! - [`index`] — one append-only index per (chain, container kind); `accept`
//!   writes `containerID→bytes`, `index→containerID`, `containerID→index` and
//!   advances `nextAcceptedIndex` atomically via a versioned batch (Go
//!   `index.go`, 12 §5).
//! - [`acceptor`] — the broadcast [`AcceptorGroup`](acceptor::AcceptorGroup)
//!   seam replacing Go's synchronous `snow.AcceptorGroup` fan-out (17 §3).
//! - [`indexer`] — chain registration, the incomplete-index safety rule, the
//!   `hasRun`/`incomplete` restart markers, and the per-index acceptor task
//!   that offloads writes to `spawn_blocking` (Go `indexer.go`, 17 §2.2 #20).
//! - [`service`] — the six `index.*` JSON-RPC methods (Go `service.go`, 14 §7).
//!
//! The node (M8.29) supplies the seams: the indexer's database, the three
//! acceptor groups, the [`PathAdder`] used to mount routes on the API server,
//! and the shutdown callback fired when the indexer fatals or closes.

#![forbid(unsafe_code)]

// Dev-dependency exercised only by the integration test
// (`tests/differential_indexer_parity.rs`); silence `unused_crate_dependencies`
// for the lib-test unit (per-dep, matching the ava-genesis precedent).
#[cfg(test)]
use hex as _;

pub mod acceptor;
pub mod container;
pub mod error;
pub mod index;
pub mod indexer;
pub mod service;

use async_trait::async_trait;

use ava_api::BoxedHandler;
use ava_snow::context::ConsensusContext;

pub use acceptor::{AcceptedContainer, AcceptorGroup, DEFAULT_ACCEPTOR_CAPACITY};
pub use container::{CODEC_VERSION, Container};
pub use error::{Error, Result};
pub use index::{Index, IndexReader, MAX_FETCHED_BY_RANGE};
pub use indexer::{Config, ContainerIndexer, ShutdownF};
pub use service::{Encoding, FormattedContainer, IndexService, index_handler};

/// The container topology a chain's VM exposes, deciding which indices a
/// registered chain gets (Go discriminates with a type-switch on the VM:
/// `block.ChainVM` ⇒ block only, `vertex.DAGVM` ⇒ block + vtx + tx).
///
/// Rust has no `CommonVM` super-trait to downcast (and no DAG VM port), so the
/// node passes the topology explicitly through this narrow seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmType {
    /// A Snowman chain (`block.ChainVM`): a `block` index only.
    Chain,
    /// A legacy DAG chain (`vertex.DAGVM`): `block`, `vtx`, and `tx` indices.
    Dag,
}

/// The indexer (Go `indexer.Indexer`: `chains.Registrant` + `io.Closer`;
/// spec 12 §5).
#[async_trait]
pub trait Indexer: Send + Sync {
    /// Registers `ctx`'s chain for indexing (Go `RegisterChain`). Skips
    /// non-Primary-Network subnets and already-indexed chains; enforces the
    /// incomplete-index safety rule (12 §5) — a violation is **fatal**: the
    /// indexer closes itself and fires the shutdown callback. Mirrors Go in
    /// reporting nothing to the caller.
    async fn register_chain(&self, chain_name: &str, ctx: &ConsensusContext, vm_type: VmType);

    /// Stops indexing and closes every index plus the indexer's database
    /// (node shutdown step 11). Idempotent: later calls do nothing and
    /// return `Ok`.
    ///
    /// # Errors
    /// Returns the first close error after attempting every close.
    async fn close(&self) -> Result<()>;
}

/// The API-server mounting seam (Go `server.PathAdder`): the node implements
/// this over `ava_api::ApiServer::add_route` in M8.29; the indexer calls it
/// with `base = "index/<chainAlias>"` and `endpoint = "/{block,tx,vtx}"` so the
/// route lands at `/ext/index/<chainAlias>/<kind>` (14 §7).
pub trait PathAdder: Send + Sync {
    /// Registers `handler` under `/ext/<base><endpoint>`.
    ///
    /// # Errors
    /// Propagates the server's route-reservation failure (e.g. an
    /// already-taken path).
    fn add_route(
        &self,
        handler: BoxedHandler,
        base: &str,
        endpoint: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

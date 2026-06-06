// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The base [`Vm`] trait every consensus VM implements
//! (`snow/engine/common.VM`, specs 07 §2.1), the VM→engine [`VmEvent`]
//! notification enum (`common.Message`), and the [`HttpHandler`] descriptor.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState};

use crate::app::AppHandler;
use crate::app_sender::AppSender;
use crate::connector::Connector;
use crate::error::Result;
use crate::health::HealthCheck;

/// `snow/engine/common.Message` — the VM→engine notification enum.
///
/// Discriminants match Go's `iota + 1` exactly so they round-trip over
/// `proto/vm`: `PendingTxs == 1`, `StateSyncDone == 2`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u32)]
pub enum VmEvent {
    /// The VM has pending transactions; the engine must eventually call
    /// `build_block` at least once.
    PendingTxs = 1,
    /// The VM has finished syncing the requested state summary.
    StateSyncDone = 2,
}

/// The lock semantics an HTTP handler expects, mirroring Go's
/// `common.HTTPHandler{LockOptions}`.
///
/// In Rust the VM is its own actor and does not share the engine's `ctx.Lock`,
/// so these variants carry no runtime locking behaviour here; the enum is
/// preserved for `proto/vm`/`proto/http` wire parity (specs 07 §2.1, §5).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
#[repr(u32)]
// Variant names mirror Go's `common.LockOptions` (`NoLock`/`ReadLock`/`WriteLock`)
// verbatim for wire/source parity; the shared `Lock` postfix is intentional.
#[allow(clippy::enum_variant_names)]
pub enum LockOptions {
    /// Acquire the write lock (Go default — `WriteLock == 0`).
    #[default]
    WriteLock = 0,
    /// Acquire the read lock (`ReadLock == 1`).
    ReadLock = 1,
    /// Acquire no lock (`NoLock == 2`).
    NoLock = 2,
}

/// `snow/engine/common.HTTPHandler` — an HTTP handler the VM exposes under
/// `/ext/bc/[chainID]/[extension]`, paired with its [`LockOptions`].
///
/// The root workspace pulls in no `tower`/`http`/`hyper` dependency, so (per the
/// task's design note) this is modelled as a plain descriptor rather than a
/// boxed `tower::Service`. The `handler` bytes are an opaque, transport-specific
/// reference to the registered service (e.g. a gRPC server id for the
/// rpcchainvm guest); a richer in-process service type is a follow-up once the
/// HTTP stack lands (see `tests/PORTING.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpHandler {
    /// The lock semantics the handler expects (wire-parity only here).
    pub lock_options: LockOptions,
    /// Opaque, transport-specific reference to the registered handler.
    pub handler: Vec<u8>,
}

impl HttpHandler {
    /// Builds an `HttpHandler` with the given lock options and opaque handler
    /// reference.
    #[must_use]
    pub fn new(lock_options: LockOptions, handler: Vec<u8>) -> Self {
        Self {
            lock_options,
            handler,
        }
    }
}

/// `snow/engine/common.VM` — the base every consensus VM implements
/// (specs 07 §2.1).
///
/// Supertraits give the VM its inbound app-message side ([`AppHandler`]), its
/// health surface ([`HealthCheck`]), and its peer connect/disconnect handler
/// ([`Connector`]). Go's `context.Context` is a `&CancellationToken`; the VM
/// receives only the immutable [`ChainContext`] at `initialize` (never the
/// engine's `ConsensusContext`).
#[async_trait]
pub trait Vm: AppHandler + HealthCheck + Connector + Send + Sync {
    /// `Initialize`. The VM receives immutable identity/handles
    /// (`Arc<ChainContext>`), the per-chain VM database, the genesis/upgrade/
    /// config bytes, the feature extensions, and the outbound [`AppSender`].
    ///
    /// `fxs` is `Vec<Fx>` in the spec; the concrete `Fx` type lands with the fx
    /// framework (specs 07 §6), so it is carried here as opaque
    /// `Vec<(Id, ...)>`-shaped bytes — see `tests/PORTING.md`. We pass the raw
    /// fx ids as the simplest faithful placeholder for now.
    #[allow(clippy::too_many_arguments)]
    async fn initialize(
        &mut self,
        token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        upgrade_bytes: &[u8],
        config_bytes: &[u8],
        fxs: Vec<Fx>,
        app_sender: Arc<dyn AppSender>,
    ) -> Result<()>;

    /// `SetState` — the engine tells the VM its next phase.
    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> Result<()>;

    /// `Shutdown` — called when the node is shutting down.
    async fn shutdown(&mut self, token: &CancellationToken) -> Result<()>;

    /// `Version` — the VM's version string.
    async fn version(&self, token: &CancellationToken) -> Result<String>;

    /// `CreateHandlers` — `[extension] -> HTTP handler` served under
    /// `/ext/bc/[chainID]/[extension]`.
    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> Result<HashMap<String, HttpHandler>>;

    /// `NewHTTPHandler` — single handler routed via the chain-id header.
    async fn new_http_handler(&mut self, token: &CancellationToken)
        -> Result<Option<HttpHandler>>;

    /// `WaitForEvent` — blocks until the VM has a [`VmEvent`] for the engine or
    /// the token is cancelled.
    async fn wait_for_event(&self, token: &CancellationToken) -> Result<VmEvent>;
}

/// `snow/engine/common.Fx` — a feature-extension instance bound to its id.
///
/// The full fx framework (`FxInstance`, specs 07 §6) is a follow-up; this base
/// task carries only the id so the `Vm::initialize` signature is faithful. The
/// `fx` payload is added when `ava-secp256k1fx` lands (see `tests/PORTING.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fx {
    /// The fx's id.
    pub id: ava_types::id::Id,
}

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
// Re-exported so existing `crate::vm::Fx` consumers (the metervm/tracedvm
// middleware, testutil) keep resolving after `Fx` moved to the `fx` module.
pub use crate::fx::Fx;
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

/// A buffered in-process HTTP request handed to a VM handler (the Rust
/// equivalent of Go's `http.Request` as seen by `common.VM` handlers).
///
/// `ava-vm` deliberately carries no `http`/`tower`/`hyper` dependency, so the
/// node's HTTP server (`ava-api`) adapts its transport request into this
/// buffered form before handing it to the VM (mirroring the buffered
/// `proto/http` semantics the Go rpcchainvm plugin uses for non-hijacked
/// handlers).
#[derive(Clone, Debug, Default)]
pub struct VmRequest {
    /// The HTTP method (e.g. `POST`), uppercase.
    pub method: String,
    /// The request URI (path + optional query), e.g. `/ext/bc/C/rpc`.
    pub uri: String,
    /// The request headers. Names are case-insensitive on lookup; a repeated
    /// header appears once per value (preserving multiplicity, which the
    /// proposervm header-route contract relies on — Go `vm.go:297` reads
    /// `r.Header[server.HTTPHeaderRoute]` as a `[]string`).
    pub headers: Vec<(String, String)>,
    /// The buffered request body.
    pub body: Vec<u8>,
}

impl VmRequest {
    /// All values of header `name` (case-insensitive), in order of appearance
    /// (Go `r.Header[textproto.CanonicalMIMEHeaderKey(name)]`).
    pub fn header_values<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The first value of header `name` (case-insensitive), if present.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.header_values(name).next()
    }
}

/// A buffered in-process HTTP response produced by a VM handler (the Rust
/// equivalent of what a Go handler writes to its `http.ResponseWriter`).
#[derive(Clone, Debug)]
pub struct VmResponse {
    /// The HTTP status code (e.g. `200`).
    pub status: u16,
    /// The response headers (e.g. `content-type`).
    pub headers: Vec<(String, String)>,
    /// The buffered response body.
    pub body: Vec<u8>,
}

impl VmResponse {
    /// A `200 OK` response with the given `content-type` and body.
    #[must_use]
    pub fn ok(content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_string(), content_type.to_string())],
            body,
        }
    }

    /// A bare status-code response with no headers and an empty body
    /// (Go `w.WriteHeader(code)` with nothing written).
    #[must_use]
    pub fn status_only(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
}

impl Default for VmResponse {
    fn default() -> Self {
        Self::status_only(200)
    }
}

/// The in-process HTTP service seam — the Rust mirror of Go's `http.Handler`
/// as returned by `common.VM.CreateHandlers` / `NewHTTPHandler`.
///
/// In-process VMs implement this directly; the rpcchainvm host adapts the
/// plugin's gRPC `proto/http` service onto it (follow-up; see
/// `tests/PORTING.md`).
#[async_trait]
pub trait VmHttpService: Send + Sync {
    /// Serves one buffered request (Go `handler.ServeHTTP(w, r)`).
    async fn serve_http(&self, req: VmRequest) -> VmResponse;
}

/// `snow/engine/common.HTTPHandler` — an HTTP handler the VM exposes under
/// `/ext/bc/[chainID]/[extension]`, paired with its [`LockOptions`].
///
/// Two transports share this descriptor:
/// - **in-process** VMs set [`HttpHandler::service`] (the [`VmHttpService`]
///   seam the node's HTTP server mounts directly, M8.22);
/// - the **rpcchainvm** plugin path keeps `service: None` and carries an
///   opaque, transport-specific reference in [`HttpHandler::handler`] (e.g. a
///   gRPC server id for the guest), preserving `proto/vm` wire parity.
#[derive(Clone)]
pub struct HttpHandler {
    /// The lock semantics the handler expects (wire-parity only here).
    pub lock_options: LockOptions,
    /// Opaque, transport-specific reference to the registered handler.
    pub handler: Vec<u8>,
    /// The in-process handler, when the VM runs inside the node process.
    pub service: Option<Arc<dyn VmHttpService>>,
}

impl std::fmt::Debug for HttpHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpHandler")
            .field("lock_options", &self.lock_options)
            .field("handler", &self.handler)
            .field("service", &self.service.as_ref().map(|_| "<dyn>"))
            .finish()
    }
}

impl PartialEq for HttpHandler {
    /// Wire-field equality plus in-process handler **identity** (`Arc::ptr_eq`;
    /// a `dyn` service has no structural equality).
    fn eq(&self, other: &Self) -> bool {
        self.lock_options == other.lock_options
            && self.handler == other.handler
            && match (&self.service, &other.service) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

impl Eq for HttpHandler {}

impl HttpHandler {
    /// Builds an `HttpHandler` with the given lock options and opaque handler
    /// reference (the rpcchainvm wire form; no in-process service).
    #[must_use]
    pub fn new(lock_options: LockOptions, handler: Vec<u8>) -> Self {
        Self {
            lock_options,
            handler,
            service: None,
        }
    }

    /// Builds an `HttpHandler` over an in-process [`VmHttpService`].
    #[must_use]
    pub fn in_process(lock_options: LockOptions, service: Arc<dyn VmHttpService>) -> Self {
        Self {
            lock_options,
            handler: Vec::new(),
            service: Some(service),
        }
    }
}

/// A lock-free signal that a VM has buildable work, so a forwarder can await
/// it WITHOUT holding the consensus-shared `Arc<Mutex<dyn Vm>>` (Go's model:
/// `snow/engine/common/notifier.go` calls `WaitForEvent` off the engine lock,
/// since Go VMs manage their own concurrency internally). A VM that exposes
/// this trait (via [`Vm::pending_work_waiter`]) lets a per-chain proposal
/// forwarder park on [`PendingWorkWaiter::wait`] while holding only whatever
/// the waiter itself captures — never the outer VM mutex a forwarder would
/// otherwise need to call `wait_for_event` (M7.18 lock-parking hazard: a
/// forwarder blocked inside that lock wedges verify/get/build for the whole
/// chain).
#[async_trait]
pub trait PendingWorkWaiter: Send + Sync {
    /// True iff the VM currently has work to build.
    fn has_pending(&self) -> bool;

    /// Resolves when the VM has (or gains) buildable work. Implementations
    /// must register interest (e.g. subscribe to a notify) before checking
    /// emptiness, so an admission racing the call is never lost.
    async fn wait(&self);
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
    async fn new_http_handler(&mut self, token: &CancellationToken) -> Result<Option<HttpHandler>>;

    /// `WaitForEvent` — blocks until the VM has a [`VmEvent`] for the engine or
    /// the token is cancelled.
    async fn wait_for_event(&self, token: &CancellationToken) -> Result<VmEvent>;

    /// An optional lock-free waiter for a per-chain proposal forwarder. `None`
    /// (the default) means the VM has no admission-driven build trigger (P/X/
    /// SAE today park until cancellation in `wait_for_event`); a VM that
    /// overrides this hands out a [`PendingWorkWaiter`] the forwarder can
    /// await without going through the VM's own lock.
    fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> {
        None
    }
}

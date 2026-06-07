// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/rpcdb` `Database` proxy (specs 07 §5.4, 04 §2.8).
//!
//! The wire contract, the server-side iterator handles, the batched
//! `IteratorNext`, and the `ErrEnumToError` table all live in
//! [`ava_database::rpcdb`] (M1.11); this module is the rpcchainvm wiring around
//! them: [`dial`] builds the guest-side [`RpcDatabase`] (a
//! [`DynDatabase`](ava_database::DynDatabase) over the channel), and [`serve`]
//! wraps a host [`DynDatabase`](ava_database::DynDatabase) as the node-side tonic
//! `Database` service.
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcDatabase`]); the
//! node **serves** ([`serve`] → the tonic service).

use std::sync::Arc;

use ava_database::DynDatabase;
use ava_database::rpcdb::{DatabaseClient, DatabaseServer};
use ava_vm::error::{Error, Result};

/// The guest-side `proto/rpcdb` client: a [`DynDatabase`] over the channel.
///
/// It is exactly [`ava_database::rpcdb::DatabaseClient`], which owns a
/// current-thread tokio runtime and `block_on`s each RPC (the synchronous
/// `Database` surface over an async transport; 04 §1.2). Re-exported under the
/// proxy name the spec uses (`RpcDatabase`).
pub type RpcDatabase = DatabaseClient;

/// Dials the host-served `Database` at `addr` and builds the guest-side
/// [`RpcDatabase`].
///
/// **Synchronous** by design: the returned [`RpcDatabase`] is a *blocking*
/// [`DynDatabase`](ava_database::DynDatabase) that owns the runtime used to dial,
/// so the channel's background task lives on the same runtime that later
/// `block_on`s every RPC (04 §1.2). Call this from a blocking context (e.g.
/// `spawn_blocking` / a dedicated thread), **not** from inside an async runtime
/// — a nested `block_on` panics ("Cannot start a runtime from within a
/// runtime").
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub fn dial(addr: &str) -> Result<RpcDatabase> {
    // The blocking `Database` client needs its own runtime to drive the async
    // tonic calls; the channel must be built on *that* runtime so its background
    // connection task is driven by the same runtime that `block_on`s the RPCs.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Error::HandshakeFailed)?;
    let channel = rt
        .block_on(async {
            tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
                .map_err(|_| Error::HandshakeFailed)?
                .connect()
                .await
                .map_err(|_| Error::HandshakeFailed)
        })?;
    Ok(DatabaseClient::new(rt, channel))
}

/// Wraps a host [`DynDatabase`] as the node-side `Database` service wrapper.
///
/// Call [`DatabaseServer::into_service`] on the result to get the tower service
/// for `tonic::transport::Server::add_service` (the concrete generated service
/// type is not publicly nameable, so the wrapper is returned instead).
#[must_use]
pub fn serve(db: Arc<dyn DynDatabase>) -> DatabaseServer {
    DatabaseServer::new(db)
}

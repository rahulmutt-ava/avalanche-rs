// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/aliasreader` `AliasReader` proxy (the `bc_lookup` in
//! [`ava_snow::ChainContext`]; specs 07 §5.4).
//!
//! The [`AliaserReader`] trait does **not** exist in the workspace yet — the real
//! `Aliaser` lands with `ava-chains` (M3.26). M3.25 defines a **minimal local**
//! trait here so the proxy compiles and round-trips; replace it with / re-export
//! the `ava-chains` `AliaserReader` once that crate lands. Recorded in
//! `tests/PORTING.md`.
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcAliasReader`]); the
//! node **serves** ([`serve`] → an [`AliasReaderServer`]).

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ava_types::id::Id;
use ava_vm::error::{Error, Result};

use crate::pb::aliasreader::alias_reader_client::AliasReaderClient;
use crate::pb::aliasreader::alias_reader_server::{
    AliasReader as AliasReaderService, AliasReaderServer as PbAliasReaderServer,
};
use crate::pb::aliasreader::{Alias, AliasList, Id as PbId};

/// `ids.AliaserReader` (minimal local definition — see module docs).
/// Bidirectional chain-id ↔ alias lookup.
#[async_trait]
pub trait AliaserReader: Send + Sync {
    /// Resolve a chain id from one of its aliases (`Lookup`).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the alias is unknown.
    async fn lookup(&self, alias: &str) -> Result<Id>;

    /// The canonical (primary) alias of a chain id (`PrimaryAlias`).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id has no alias.
    async fn primary_alias(&self, id: Id) -> Result<String>;

    /// All aliases of a chain id (`Aliases`).
    ///
    /// # Errors
    /// Implementation-defined.
    async fn aliases(&self, id: Id) -> Result<Vec<String>>;
}

/// The guest-side `proto/aliasreader` client: an [`AliaserReader`] over the
/// channel.
pub struct RpcAliasReader {
    client: Mutex<AliasReaderClient<Channel>>,
}

/// Dials the host-served `AliasReader` at `addr` and builds the guest-side
/// [`RpcAliasReader`].
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub async fn dial(addr: &str) -> Result<RpcAliasReader> {
    let client = AliasReaderClient::connect(format!("http://{addr}"))
        .await
        .map_err(|_| Error::HandshakeFailed)?;
    Ok(RpcAliasReader {
        client: Mutex::new(client),
    })
}

/// Maps a per-method gRPC failure to a crate [`Error`] (`NOT_FOUND` ⇒ the
/// `NotFound` sentinel).
fn rpc_err(status: tonic::Status) -> Error {
    if status.code() == tonic::Code::NotFound {
        Error::NotFound
    } else {
        Error::HandshakeFailed
    }
}

#[async_trait]
impl AliaserReader for RpcAliasReader {
    async fn lookup(&self, alias: &str) -> Result<Id> {
        let mut client = self.client.lock().clone();
        let resp = client
            .lookup(Alias {
                alias: alias.to_string(),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        Id::from_slice(&resp.id).map_err(|_| Error::NotFound)
    }

    async fn primary_alias(&self, id: Id) -> Result<String> {
        let mut client = self.client.lock().clone();
        let resp = client
            .primary_alias(PbId {
                id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        Ok(resp.alias)
    }

    async fn aliases(&self, id: Id) -> Result<Vec<String>> {
        let mut client = self.client.lock().clone();
        let resp = client
            .aliases(PbId {
                id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        Ok(resp.aliases)
    }
}

/// The node-side `AliasReader` tonic service wrapping the host's implementation.
pub struct AliasReaderServer {
    reader: Arc<dyn AliaserReader>,
}

/// Wraps a host [`AliaserReader`] as the node-side service wrapper. Call
/// [`AliasReaderServer::into_service`] for the tower service.
#[must_use]
pub fn serve(reader: Arc<dyn AliaserReader>) -> AliasReaderServer {
    AliasReaderServer { reader }
}

impl AliasReaderServer {
    /// Consumes `self` into a tower service for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbAliasReaderServer<Self> {
        PbAliasReaderServer::new(self)
    }
}

/// Maps a host-side [`Error`] to a gRPC `Status` (`NotFound` rides the status).
fn to_status(err: &Error) -> Status {
    match err {
        Error::NotFound => Status::not_found("alias not found"),
        e => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl AliasReaderService for AliasReaderServer {
    async fn lookup(&self, request: Request<Alias>) -> std::result::Result<Response<PbId>, Status> {
        let alias = request.into_inner().alias;
        let id = self
            .reader
            .lookup(&alias)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(PbId {
            id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
        }))
    }

    async fn primary_alias(
        &self,
        request: Request<PbId>,
    ) -> std::result::Result<Response<Alias>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let alias = self
            .reader
            .primary_alias(id)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(Alias { alias }))
    }

    async fn aliases(
        &self,
        request: Request<PbId>,
    ) -> std::result::Result<Response<AliasList>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let aliases = self.reader.aliases(id).await.map_err(|e| to_status(&e))?;
        Ok(Response::new(AliasList { aliases }))
    }
}

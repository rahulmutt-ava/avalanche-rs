// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/warp` `Signer` proxy (specs 07 §5.4).
//!
//! The warp message [`Signer`] trait does **not** exist in the workspace yet
//! (`ava-crypto`'s `bls::Signer` is a different, lower-level BLS signer over raw
//! bytes; warp signing binds `(network_id, source_chain_id, payload)`). M3.25
//! defines a **minimal local** trait here so the proxy compiles and round-trips;
//! the canonical warp `Signer` is expected to land with the warp/crypto
//! milestone and this trait should then be replaced by / re-exported from it.
//! Recorded in `tests/PORTING.md`.
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcWarpSigner`]); the
//! node **serves** ([`serve`] → a [`WarpSignerServer`]).

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ava_types::id::Id;
use ava_vm::error::{Error, Result};

use crate::pb::warp::signer_client::SignerClient;
use crate::pb::warp::signer_server::{Signer as SignerService, SignerServer as PbSignerServer};
use crate::pb::warp::{SignRequest, SignResponse};

/// `warp.Signer` (minimal local definition — see module docs). Signs an
/// unsigned warp message identified by `(network_id, source_chain_id, payload)`,
/// returning the raw BLS signature bytes.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Sign the unsigned warp message.
    ///
    /// # Errors
    /// Implementation-defined (e.g. a remote signer backend failure).
    async fn sign(&self, network_id: u32, source_chain_id: Id, payload: &[u8]) -> Result<Vec<u8>>;
}

/// The guest-side `proto/warp` client: a [`Signer`] over the channel.
pub struct RpcWarpSigner {
    client: Mutex<SignerClient<Channel>>,
}

/// Dials the host-served `Signer` at `addr` and builds the guest-side
/// [`RpcWarpSigner`].
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub async fn dial(addr: &str) -> Result<RpcWarpSigner> {
    let client = SignerClient::connect(format!("http://{addr}"))
        .await
        .map_err(|_| Error::HandshakeFailed)?;
    Ok(RpcWarpSigner {
        client: Mutex::new(client),
    })
}

#[async_trait]
impl Signer for RpcWarpSigner {
    async fn sign(&self, network_id: u32, source_chain_id: Id, payload: &[u8]) -> Result<Vec<u8>> {
        let mut client = self.client.lock().clone();
        let resp = client
            .sign(SignRequest {
                network_id,
                source_chain_id: bytes::Bytes::copy_from_slice(&source_chain_id.to_bytes()),
                payload: bytes::Bytes::copy_from_slice(payload),
            })
            .await
            .map_err(|_| Error::HandshakeFailed)?
            .into_inner();
        Ok(resp.signature.to_vec())
    }
}

/// The node-side `Signer` tonic service wrapping the host's implementation.
pub struct WarpSignerServer {
    signer: Arc<dyn Signer>,
}

/// Wraps a host [`Signer`] as the node-side `Signer` service wrapper. Call
/// [`WarpSignerServer::into_service`] for the tower service.
#[must_use]
pub fn serve(signer: Arc<dyn Signer>) -> WarpSignerServer {
    WarpSignerServer { signer }
}

impl WarpSignerServer {
    /// Consumes `self` into a tower service for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbSignerServer<Self> {
        PbSignerServer::new(self)
    }
}

#[tonic::async_trait]
impl SignerService for WarpSignerServer {
    async fn sign(
        &self,
        request: Request<SignRequest>,
    ) -> std::result::Result<Response<SignResponse>, Status> {
        let req = request.into_inner();
        let source_chain_id = Id::from_slice(&req.source_chain_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let sig = self
            .signer
            .sign(req.network_id, source_chain_id, &req.payload)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(SignResponse {
            signature: bytes::Bytes::from(sig),
        }))
    }
}

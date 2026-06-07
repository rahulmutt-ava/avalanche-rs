// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/appsender` `AppSender` proxy (specs 07 §2.6, §5.4).
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcAppSender`], a guest
//! [`AppSender`] over the channel); the node **serves** ([`serve`] → an
//! [`AppSenderServer`] wrapping the host's `Arc<dyn AppSender>`).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ava_types::node_id::NodeId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::error::{Error, Result};

use crate::pb::appsender::app_sender_client::AppSenderClient;
use crate::pb::appsender::app_sender_server::{
    AppSender as AppSenderService, AppSenderServer as PbAppSenderServer,
};
use crate::pb::appsender::{
    SendAppErrorMsg, SendAppGossipMsg, SendAppRequestMsg, SendAppResponseMsg,
};

/// The guest-side `proto/appsender` client: an [`AppSender`] over the channel.
pub struct RpcAppSender {
    client: Mutex<AppSenderClient<Channel>>,
}

/// Dials the host-served `AppSender` at `addr` and builds the guest-side
/// [`RpcAppSender`].
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub async fn dial(addr: &str) -> Result<RpcAppSender> {
    let client = AppSenderClient::connect(format!("http://{addr}"))
        .await
        .map_err(|_| Error::HandshakeFailed)?;
    Ok(RpcAppSender {
        client: Mutex::new(client),
    })
}

#[async_trait]
impl AppSender for RpcAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        nodes: &HashSet<NodeId>,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> Result<()> {
        // Sort node ids for a deterministic wire order (00 §6.1 — never emit an
        // unordered set onto the wire).
        let mut ids: Vec<NodeId> = nodes.iter().copied().collect();
        ids.sort();
        let node_ids = ids
            .into_iter()
            .map(|n| bytes::Bytes::copy_from_slice(n.as_bytes()))
            .collect();
        let mut client = self.client.lock().clone();
        client
            .send_app_request(SendAppRequestMsg {
                node_ids,
                request_id,
                request: bytes::Bytes::from(bytes),
            })
            .await
            .map_err(|_| Error::HandshakeFailed)?;
        Ok(())
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let mut client = self.client.lock().clone();
        client
            .send_app_response(SendAppResponseMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                request_id,
                response: bytes::Bytes::from(bytes),
            })
            .await
            .map_err(|_| Error::HandshakeFailed)?;
        Ok(())
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        code: i32,
        message: &str,
    ) -> Result<()> {
        let mut client = self.client.lock().clone();
        client
            .send_app_error(SendAppErrorMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                request_id,
                error_code: code,
                error_message: message.to_string(),
            })
            .await
            .map_err(|_| Error::HandshakeFailed)?;
        Ok(())
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        config: SendConfig,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let mut ids: Vec<NodeId> = config.node_ids.iter().copied().collect();
        ids.sort();
        let node_ids = ids
            .into_iter()
            .map(|n| bytes::Bytes::copy_from_slice(n.as_bytes()))
            .collect();
        let mut client = self.client.lock().clone();
        client
            .send_app_gossip(SendAppGossipMsg {
                node_ids,
                validators: config.validators as u64,
                non_validators: config.non_validators as u64,
                peers: config.peers as u64,
                msg: bytes::Bytes::from(bytes),
            })
            .await
            .map_err(|_| Error::HandshakeFailed)?;
        Ok(())
    }
}

/// The node-side `AppSender` tonic service wrapping the host's implementation.
pub struct AppSenderServer {
    sender: Arc<dyn AppSender>,
    token: CancellationToken,
}

/// Wraps a host [`AppSender`] as the node-side `AppSender` service wrapper. Call
/// [`AppSenderServer::into_service`] for the tower service.
#[must_use]
pub fn serve(sender: Arc<dyn AppSender>) -> AppSenderServer {
    AppSenderServer {
        sender,
        token: CancellationToken::new(),
    }
}

impl AppSenderServer {
    /// Consumes `self` into a tower service for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbAppSenderServer<Self> {
        PbAppSenderServer::new(self)
    }
}

/// Decodes a wire node-id, mapping a wrong length to a gRPC `invalid_argument`.
/// (`tonic::Status` is an unavoidably large error on every service method, so
/// the `result_large_err` lint is allowed for this gRPC-boundary helper.)
#[allow(clippy::result_large_err)]
fn node_from_bytes(b: &[u8]) -> std::result::Result<NodeId, Status> {
    NodeId::from_slice(b).map_err(|e| Status::invalid_argument(e.to_string()))
}

/// Maps a host-side [`Error`] to a gRPC `Status`.
fn to_status(err: &Error) -> Status {
    Status::internal(err.to_string())
}

#[tonic::async_trait]
impl AppSenderService for AppSenderServer {
    async fn send_app_request(
        &self,
        request: Request<SendAppRequestMsg>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = request.into_inner();
        let mut nodes = HashSet::with_capacity(req.node_ids.len());
        for id in &req.node_ids {
            nodes.insert(node_from_bytes(id)?);
        }
        self.sender
            .send_app_request(&self.token, &nodes, req.request_id, req.request.to_vec())
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(()))
    }

    async fn send_app_response(
        &self,
        request: Request<SendAppResponseMsg>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = node_from_bytes(&req.node_id)?;
        self.sender
            .send_app_response(&self.token, node, req.request_id, req.response.to_vec())
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(()))
    }

    async fn send_app_error(
        &self,
        request: Request<SendAppErrorMsg>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = node_from_bytes(&req.node_id)?;
        self.sender
            .send_app_error(
                &self.token,
                node,
                req.request_id,
                req.error_code,
                &req.error_message,
            )
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(()))
    }

    async fn send_app_gossip(
        &self,
        request: Request<SendAppGossipMsg>,
    ) -> std::result::Result<Response<()>, Status> {
        let req = request.into_inner();
        let mut node_ids = HashSet::with_capacity(req.node_ids.len());
        for id in &req.node_ids {
            node_ids.insert(node_from_bytes(id)?);
        }
        let config = SendConfig {
            node_ids,
            validators: usize::try_from(req.validators).unwrap_or(usize::MAX),
            non_validators: usize::try_from(req.non_validators).unwrap_or(usize::MAX),
            peers: usize::try_from(req.peers).unwrap_or(usize::MAX),
        };
        self.sender
            .send_app_gossip(&self.token, config, req.msg.to_vec())
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(()))
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/sharedmemory` `SharedMemory` proxy (specs 07 §3.1, §5.4).
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcSharedMemory`], a
//! guest [`SharedMemory`] over the channel); the node **serves** ([`serve`] → a
//! [`SharedMemoryServer`] wrapping the host's `Arc<dyn SharedMemory>`).
//!
//! The [`SharedMemory`] trait surface is **synchronous** (atomic cross-chain
//! KV/traits storage), so the guest client owns a current-thread tokio runtime
//! and `block_on`s each RPC — the same sync↔async bridge as `rpcdb` (04 §1.2).

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::runtime::Runtime;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ava_database::BatchOps;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, IndexedResult, Requests, SharedMemory};
use ava_vm::error::{Error, Result};

use crate::pb::sharedmemory::shared_memory_client::SharedMemoryClient;
use crate::pb::sharedmemory::shared_memory_server::{
    SharedMemory as SharedMemoryService, SharedMemoryServer as PbSharedMemoryServer,
};
use crate::pb::sharedmemory::{
    ApplyRequest, ApplyResponse, AtomicRequest, Batch, BatchDelete, BatchPut, Element as PbElement,
    GetRequest, GetResponse, IndexedRequest, IndexedResponse,
};

/// The guest-side `proto/sharedmemory` client: a [`SharedMemory`] over the
/// channel (blocking; owns a current-thread runtime).
pub struct RpcSharedMemory {
    /// The owned runtime that drives every blocking RPC. Held in an `Option`
    /// only so [`Drop`] can move it out and shut it down in the background; it
    /// is `Some` for the entire usable lifetime of the client (see
    /// [`RpcSharedMemory::rt`]).
    rt: Option<Runtime>,
    client: Mutex<SharedMemoryClient<Channel>>,
}

impl RpcSharedMemory {
    /// The owned runtime. `rt` is `Some` from construction until [`Drop`] (the
    /// only place it is taken, after which no method is reachable), so the
    /// fallback arm is unreachable in practice.
    fn rt(&self) -> &Runtime {
        match self.rt.as_ref() {
            Some(rt) => rt,
            None => unreachable!("RpcSharedMemory used after its runtime was dropped"),
        }
    }
}

impl Drop for RpcSharedMemory {
    fn drop(&mut self) {
        // The proxied client can be dropped from within an async context (the
        // rpcchainvm guest drops the proxy bundle on a tonic worker thread). The
        // default blocking [`Runtime`] drop panics there ("Cannot drop a runtime
        // in a context where blocking is not allowed"), which would abort the
        // in-flight RPC stream — the Go host then observes `RST_STREAM CANCEL`.
        // `shutdown_background` tears the runtime down without blocking, making
        // the drop safe from any context (specs 07 §5.2; matches the rpcdb
        // `DatabaseClient` fix, the M9.3 live-interop blocker).
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

/// Dials the host-served `SharedMemory` at `addr` and builds the guest-side
/// [`RpcSharedMemory`].
///
/// **Synchronous** by design (the [`SharedMemory`] trait is sync): the returned
/// client owns the runtime used to dial so the channel's background task and the
/// later `block_on`ed RPCs share one runtime (04 §1.2). Call from a blocking
/// context, not inside an async runtime.
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub fn dial(addr: &str) -> Result<RpcSharedMemory> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Error::HandshakeFailed)?;
    let client = rt
        .block_on(async { SharedMemoryClient::connect(format!("http://{addr}")).await })
        .map_err(|_| Error::HandshakeFailed)?;
    Ok(RpcSharedMemory {
        rt: Some(rt),
        client: Mutex::new(client),
    })
}

impl SharedMemory for RpcSharedMemory {
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
        let keys: Vec<bytes::Bytes> = keys
            .iter()
            .map(|k| bytes::Bytes::copy_from_slice(k))
            .collect();
        let resp = self
            .rt()
            .block_on(async {
                let mut client = self.client.lock().clone();
                client
                    .get(GetRequest {
                        peer_chain_id: bytes::Bytes::copy_from_slice(&peer_chain.to_bytes()),
                        keys,
                    })
                    .await
            })
            .map_err(|_| Error::HandshakeFailed)?
            .into_inner();
        Ok(resp.values.into_iter().map(|v| v.to_vec()).collect())
    }

    fn indexed(
        &self,
        peer_chain: Id,
        traits: &[Vec<u8>],
        start_trait: &[u8],
        start_key: &[u8],
        limit: usize,
    ) -> Result<IndexedResult> {
        let traits: Vec<bytes::Bytes> = traits
            .iter()
            .map(|t| bytes::Bytes::copy_from_slice(t))
            .collect();
        let resp = self
            .rt()
            .block_on(async {
                let mut client = self.client.lock().clone();
                client
                    .indexed(IndexedRequest {
                        peer_chain_id: bytes::Bytes::copy_from_slice(&peer_chain.to_bytes()),
                        traits,
                        start_trait: bytes::Bytes::copy_from_slice(start_trait),
                        start_key: bytes::Bytes::copy_from_slice(start_key),
                        limit: i32::try_from(limit).unwrap_or(i32::MAX),
                    })
                    .await
            })
            .map_err(|_| Error::HandshakeFailed)?
            .into_inner();
        Ok((
            resp.values.into_iter().map(|v| v.to_vec()).collect(),
            resp.last_trait.to_vec(),
            resp.last_key.to_vec(),
        ))
    }

    fn apply(&self, requests: BTreeMap<Id, Requests>, batches: &[BatchOps]) -> Result<()> {
        // BTreeMap iteration is chain-id-ascending — a deterministic wire order
        // (00 §6.1).
        let wire_requests = requests
            .into_iter()
            .map(|(chain, reqs)| AtomicRequest {
                remove_requests: reqs.remove.into_iter().map(bytes::Bytes::from).collect(),
                put_requests: reqs
                    .put
                    .into_iter()
                    .map(|e| PbElement {
                        key: bytes::Bytes::from(e.key),
                        value: bytes::Bytes::from(e.value),
                        traits: e.traits.into_iter().map(bytes::Bytes::from).collect(),
                    })
                    .collect(),
                peer_chain_id: bytes::Bytes::copy_from_slice(&chain.to_bytes()),
            })
            .collect();
        let wire_batches = batches.iter().map(batch_ops_to_proto).collect();
        self.rt()
            .block_on(async {
                let mut client = self.client.lock().clone();
                client
                    .apply(ApplyRequest {
                        requests: wire_requests,
                        batches: wire_batches,
                    })
                    .await
            })
            .map_err(|_| Error::HandshakeFailed)?;
        Ok(())
    }
}

/// Converts a [`BatchOps`] to a wire [`Batch`] (puts then deletes, in order).
fn batch_ops_to_proto(ops: &BatchOps) -> Batch {
    let mut puts = Vec::new();
    let mut deletes = Vec::new();
    for op in &ops.ops {
        if op.delete {
            deletes.push(BatchDelete {
                key: bytes::Bytes::copy_from_slice(&op.key),
            });
        } else {
            puts.push(BatchPut {
                key: bytes::Bytes::copy_from_slice(&op.key),
                value: bytes::Bytes::copy_from_slice(&op.value),
            });
        }
    }
    Batch { puts, deletes }
}

/// The node-side `SharedMemory` tonic service wrapping the host's impl.
pub struct SharedMemoryServer {
    mem: Arc<dyn SharedMemory>,
}

/// Wraps a host [`SharedMemory`] as the node-side service wrapper. Call
/// [`SharedMemoryServer::into_service`] for the tower service.
#[must_use]
pub fn serve(mem: Arc<dyn SharedMemory>) -> SharedMemoryServer {
    SharedMemoryServer { mem }
}

impl SharedMemoryServer {
    /// Consumes `self` into a tower service for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbSharedMemoryServer<Self> {
        PbSharedMemoryServer::new(self)
    }
}

#[tonic::async_trait]
impl SharedMemoryService for SharedMemoryServer {
    async fn get(
        &self,
        request: Request<GetRequest>,
    ) -> std::result::Result<Response<GetResponse>, Status> {
        let req = request.into_inner();
        let peer = Id::from_slice(&req.peer_chain_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let keys: Vec<Vec<u8>> = req.keys.iter().map(|k| k.to_vec()).collect();
        let values = self
            .mem
            .get(peer, &keys)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetResponse {
            values: values.into_iter().map(bytes::Bytes::from).collect(),
        }))
    }

    async fn indexed(
        &self,
        request: Request<IndexedRequest>,
    ) -> std::result::Result<Response<IndexedResponse>, Status> {
        let req = request.into_inner();
        let peer = Id::from_slice(&req.peer_chain_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let traits: Vec<Vec<u8>> = req.traits.iter().map(|t| t.to_vec()).collect();
        let limit = usize::try_from(req.limit).unwrap_or(0);
        let (values, last_trait, last_key) = self
            .mem
            .indexed(peer, &traits, &req.start_trait, &req.start_key, limit)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(IndexedResponse {
            values: values.into_iter().map(bytes::Bytes::from).collect(),
            last_trait: bytes::Bytes::from(last_trait),
            last_key: bytes::Bytes::from(last_key),
        }))
    }

    async fn apply(
        &self,
        request: Request<ApplyRequest>,
    ) -> std::result::Result<Response<ApplyResponse>, Status> {
        let req = request.into_inner();
        let mut requests: BTreeMap<Id, Requests> = BTreeMap::new();
        for ar in req.requests {
            let chain = Id::from_slice(&ar.peer_chain_id)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
            requests.insert(
                chain,
                Requests {
                    remove: ar.remove_requests.iter().map(|b| b.to_vec()).collect(),
                    put: ar
                        .put_requests
                        .into_iter()
                        .map(|e| Element {
                            key: e.key.to_vec(),
                            value: e.value.to_vec(),
                            traits: e.traits.iter().map(|t| t.to_vec()).collect(),
                        })
                        .collect(),
                },
            );
        }
        let batches: Vec<BatchOps> = req.batches.iter().map(proto_to_batch_ops).collect();
        self.mem
            .apply(requests, &batches)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ApplyResponse {}))
    }
}

/// Converts a wire [`Batch`] back to a [`BatchOps`] (puts then deletes).
fn proto_to_batch_ops(b: &Batch) -> BatchOps {
    let mut ops = BatchOps::new();
    for p in &b.puts {
        ops.put(&p.key, &p.value);
    }
    for d in &b.deletes {
        ops.delete(&d.key);
    }
    ops
}

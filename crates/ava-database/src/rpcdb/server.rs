// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `rpcdb` gRPC **server**: a tonic service wrapping a host
//! [`Database`](crate::Database), mirroring `database/rpcdb/db_server.go`
//! (04 §2.8).
//!
//! `Error::Closed`/`Error::NotFound` are carried in the response's `err` enum
//! (`ErrorToErrEnum`); any other error is surfaced as a gRPC [`Status`]
//! (`ErrorToRPCError`). A registry keyed by `u64` id holds server-side iterator
//! state; `IteratorNext` batches pairs up to [`ITERATION_BATCH_SIZE`] bytes per
//! RPC (port of the Go batching).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tonic::{Request, Response, Status};

use super::pb::database_server::{Database as DatabaseService, DatabaseServer as PbDatabaseServer};
use super::pb::{
    CloseRequest, CloseResponse, CompactRequest, CompactResponse, DeleteRequest, DeleteResponse,
    GetRequest, GetResponse, HasRequest, HasResponse, HealthCheckResponse, IteratorErrorRequest,
    IteratorErrorResponse, IteratorNextRequest, IteratorNextResponse, IteratorReleaseRequest,
    IteratorReleaseResponse, NewIteratorWithStartAndPrefixRequest,
    NewIteratorWithStartAndPrefixResponse, PutRequest, PutResponse, WriteBatchRequest,
    WriteBatchResponse,
};
use super::{error_to_err_enum, pb};
use crate::error::Error;
use crate::traits::{DynDatabase, WriteDelete};

/// Max bytes of key+value returned per `IteratorNext` RPC (Go's
/// `iterationBatchSize = 128 * units.KiB`). Amortizes round-trips.
const ITERATION_BATCH_SIZE: usize = 128 * 1024;

/// Server-side iterator state.
///
/// The live iterator's contents are **snapshotted into an owned `Vec` at
/// creation time** (matching memdb's point-in-time snapshot semantics, 04 §2.2
/// `TestIteratorSnapshot`) so the registry need not hold a borrowed `BoxIter<'a>`
/// (which would be self-referential against the `Arc<dyn DynDatabase>`). A
/// `pos` cursor is advanced across `IteratorNext` RPCs. The closed-detection
/// path lives on the **client** (it mirrors Go's client-side `closed` atomic),
/// so the snapshot is sufficient for conformance. `err` carries any error
/// observed while building the snapshot (e.g. iterating a closed DB).
struct IterState {
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    pos: usize,
    /// Any error captured during iteration (reported by `IteratorError`).
    err: Option<Error>,
}

/// A tonic `Database` service over a host [`DynDatabase`].
pub struct DatabaseServer {
    db: Arc<dyn DynDatabase>,
    iterators: Mutex<Iterators>,
}

#[derive(Default)]
struct Iterators {
    next_id: u64,
    map: HashMap<u64, IterState>,
}

impl DatabaseServer {
    /// Wraps `db` as a gRPC `Database` service.
    pub fn new(db: Arc<dyn DynDatabase>) -> Self {
        Self {
            db,
            iterators: Mutex::new(Iterators::default()),
        }
    }

    /// Consumes `self` into a tower service ready for `tonic::transport::Server`.
    pub fn into_service(self) -> PbDatabaseServer<Self> {
        PbDatabaseServer::new(self)
    }
}

/// `ErrorToRPCError`: sentinel errors ride the enum (return `Ok` here); any
/// other error becomes a gRPC `Status`.
fn to_status(err: &Error) -> Option<Status> {
    match err {
        Error::Closed | Error::NotFound => None,
        Error::Other(e) => Some(Status::internal(e.to_string())),
    }
}

#[tonic::async_trait]
impl DatabaseService for DatabaseServer {
    async fn has(&self, request: Request<HasRequest>) -> Result<Response<HasResponse>, Status> {
        let key = request.into_inner().key;
        match self.db.has(&key) {
            Ok(has) => Ok(Response::new(HasResponse {
                has,
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(HasResponse {
                    has: false,
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<GetResponse>, Status> {
        let key = request.into_inner().key;
        match self.db.get(&key) {
            Ok(value) => Ok(Response::new(GetResponse {
                value: value.into(),
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(GetResponse {
                    value: bytes::Bytes::new(),
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn put(&self, request: Request<PutRequest>) -> Result<Response<PutResponse>, Status> {
        let req = request.into_inner();
        match self.db.put(&req.key, &req.value) {
            Ok(()) => Ok(Response::new(PutResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(PutResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn delete(
        &self,
        request: Request<DeleteRequest>,
    ) -> Result<Response<DeleteResponse>, Status> {
        let key = request.into_inner().key;
        match self.db.delete(&key) {
            Ok(()) => Ok(Response::new(DeleteResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(DeleteResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn compact(
        &self,
        request: Request<CompactRequest>,
    ) -> Result<Response<CompactResponse>, Status> {
        let req = request.into_inner();
        // Go passes the raw byte slices through; `nil`/empty are equivalent and
        // memdb ignores the range anyway. Pass `Some(&bytes)` always (matches
        // Go, which forwards possibly-empty slices).
        match self.db.compact(Some(&req.start), Some(&req.limit)) {
            Ok(()) => Ok(Response::new(CompactResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(CompactResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn close(
        &self,
        _request: Request<CloseRequest>,
    ) -> Result<Response<CloseResponse>, Status> {
        match self.db.close() {
            Ok(()) => Ok(Response::new(CloseResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(CloseResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn health_check(
        &self,
        _request: Request<()>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        match self.db.health_check() {
            Ok(value) => {
                let details = serde_json::to_vec(&value)
                    .map_err(|e| Status::internal(format!("marshal health: {e}")))?;
                Ok(Response::new(HealthCheckResponse {
                    details: details.into(),
                }))
            }
            Err(e) => Err(to_status(&e).unwrap_or_else(|| Status::internal(e.to_string()))),
        }
    }

    async fn write_batch(
        &self,
        request: Request<WriteBatchRequest>,
    ) -> Result<Response<WriteBatchResponse>, Status> {
        let req = request.into_inner();
        let mut batch = self.db.new_batch();
        for put in &req.puts {
            if let Err(e) = WriteDelete::put(&mut *batch, &put.key, &put.value) {
                return match to_status(&e) {
                    Some(s) => Err(s),
                    None => Ok(Response::new(WriteBatchResponse {
                        err: error_to_err_enum(&e),
                    })),
                };
            }
        }
        for del in &req.deletes {
            if let Err(e) = WriteDelete::delete(&mut *batch, &del.key) {
                return match to_status(&e) {
                    Some(s) => Err(s),
                    None => Ok(Response::new(WriteBatchResponse {
                        err: error_to_err_enum(&e),
                    })),
                };
            }
        }
        match batch.write() {
            Ok(()) => Ok(Response::new(WriteBatchResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Err(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(WriteBatchResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }

    async fn new_iterator_with_start_and_prefix(
        &self,
        request: Request<NewIteratorWithStartAndPrefixRequest>,
    ) -> Result<Response<NewIteratorWithStartAndPrefixResponse>, Status> {
        let req = request.into_inner();

        // Drain the live iterator into an owned snapshot at creation time.
        let mut it = self
            .db
            .new_iterator_with_start_and_prefix(&req.start, &req.prefix);
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        while it.next() {
            let key = it.key().unwrap_or_default().to_vec();
            let value = it.value().unwrap_or_default().to_vec();
            entries.push((key, value));
        }
        let err = it.error().err();

        let mut iters = self.iterators.lock();
        let id = iters.next_id;
        iters.next_id = iters.next_id.wrapping_add(1);
        iters.map.insert(
            id,
            IterState {
                entries,
                pos: 0,
                err,
            },
        );
        Ok(Response::new(NewIteratorWithStartAndPrefixResponse { id }))
    }

    async fn iterator_next(
        &self,
        request: Request<IteratorNextRequest>,
    ) -> Result<Response<IteratorNextResponse>, Status> {
        let id = request.into_inner().id;
        let mut iters = self.iterators.lock();
        let Some(state) = iters.map.get_mut(&id) else {
            return Err(Status::not_found("unknown iterator"));
        };

        // Batch pairs from the snapshot up to ITERATION_BATCH_SIZE bytes (port of
        // the Go server's batching loop).
        let mut size: usize = 0;
        let mut data: Vec<PutRequest> = Vec::new();
        while size < ITERATION_BATCH_SIZE {
            let Some((key, value)) = state.entries.get(state.pos) else {
                break;
            };
            size = size.saturating_add(key.len()).saturating_add(value.len());
            data.push(PutRequest {
                key: bytes::Bytes::copy_from_slice(key),
                value: bytes::Bytes::copy_from_slice(value),
            });
            state.pos = state.pos.saturating_add(1);
        }

        Ok(Response::new(IteratorNextResponse { data }))
    }

    async fn iterator_error(
        &self,
        request: Request<IteratorErrorRequest>,
    ) -> Result<Response<IteratorErrorResponse>, Status> {
        let id = request.into_inner().id;
        let iters = self.iterators.lock();
        let Some(state) = iters.map.get(&id) else {
            return Err(Status::not_found("unknown iterator"));
        };
        match &state.err {
            None => Ok(Response::new(IteratorErrorResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Some(e) => match to_status(e) {
                Some(s) => Err(s),
                None => Ok(Response::new(IteratorErrorResponse {
                    err: error_to_err_enum(e),
                })),
            },
        }
    }

    async fn iterator_release(
        &self,
        request: Request<IteratorReleaseRequest>,
    ) -> Result<Response<IteratorReleaseResponse>, Status> {
        let id = request.into_inner().id;
        let mut iters = self.iterators.lock();
        let Some(state) = iters.map.remove(&id) else {
            return Ok(Response::new(IteratorReleaseResponse {
                err: pb::Error::Unspecified as i32,
            }));
        };
        match state.err {
            None => Ok(Response::new(IteratorReleaseResponse {
                err: pb::Error::Unspecified as i32,
            })),
            Some(e) => match to_status(&e) {
                Some(s) => Err(s),
                None => Ok(Response::new(IteratorReleaseResponse {
                    err: error_to_err_enum(&e),
                })),
            },
        }
    }
}

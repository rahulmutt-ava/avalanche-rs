// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `rpcdb` gRPC **client**: a [`Database`] talking over RPC, mirroring
//! `database/rpcdb/db_client.go` (04 §2.8).
//!
//! Each [`DatabaseClient`] owns a current-thread tokio runtime and `block_on`s
//! every RPC so the synchronous [`Database`] surface is satisfied without
//! leaking async (04 §1.2). The wire `Error` enum is mapped back to our
//! sentinels (`ErrEnumToError`); transport errors become [`Error::Other`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tokio::runtime::Runtime;
use tonic::transport::Channel;

use super::err_enum_to_result;
use super::pb::database_client::DatabaseClient as PbDatabaseClient;
use super::pb::{
    CloseRequest, CompactRequest, DeleteRequest, GetRequest, HasRequest, IteratorErrorRequest,
    IteratorNextRequest, IteratorReleaseRequest, NewIteratorWithStartAndPrefixRequest, PutRequest,
    WriteBatchRequest,
};
use crate::batch::BatchOps;
use crate::error::{Error, Result};
use crate::traits::{
    Batch, Batcher, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
    KeyValueReader, KeyValueWriter, WriteDelete,
};

/// Turns a tonic transport/status failure into [`Error::Other`].
fn transport_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("{e}"))
}

/// A [`Database`] implemented by calling a tonic `Database` client.
///
/// Cloneable handles share the same runtime + channel (the tonic `Channel` is
/// itself cheaply cloneable and multiplexes over one connection).
pub struct DatabaseClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    /// The owned runtime that drives every blocking RPC. Held in an `Option`
    /// only so [`Drop`] can move it out and shut it down in the background;
    /// it is `Some` for the entire usable lifetime of the client (see
    /// [`ClientInner::runtime`]).
    rt: Option<Runtime>,
    client: Mutex<PbDatabaseClient<Channel>>,
    /// Mirrors Go's client-side `closed` atomic: once the client closes the DB,
    /// every iterator short-circuits to `Err(Closed)` (db_client.go `iterator.Next`).
    closed: AtomicBool,
}

impl ClientInner {
    /// The owned runtime. `rt` is `Some` from construction until [`Drop`] (which
    /// is the only place it is taken, after which no method is reachable), so the
    /// fallback arm is unreachable in practice.
    fn runtime(&self) -> &Runtime {
        match self.rt.as_ref() {
            Some(rt) => rt,
            None => unreachable!("rpcdb DatabaseClient used after its runtime was dropped"),
        }
    }
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        // The proxied client can be dropped from within an async context (the
        // rpcchainvm guest drops it on a tonic worker thread when the inner VM
        // does not retain the proxied db). The default blocking [`Runtime`] drop
        // panics there ("Cannot drop a runtime in a context where blocking is
        // not allowed"), which aborts the in-flight `VM.Initialize` stream — the
        // Go host then observes `RST_STREAM CANCEL`. `shutdown_background` tears
        // the runtime down without blocking, making the drop safe from any
        // context (specs 07 §5.2, M9.3 live-interop blocker).
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl DatabaseClient {
    /// Builds a client over an established tonic [`Channel`]. The provided
    /// `runtime` drives all blocking RPC calls.
    pub fn new(runtime: Runtime, channel: Channel) -> Self {
        Self {
            inner: Arc::new(ClientInner {
                rt: Some(runtime),
                client: Mutex::new(PbDatabaseClient::new(channel)),
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Runs `f` (an async RPC future builder) on the owned runtime, blocking.
    fn block_on<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.inner.runtime().block_on(f)
    }
}

impl KeyValueReader for DatabaseClient {
    fn has(&self, key: &[u8]) -> Result<bool> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client
                    .has(HasRequest {
                        key: bytes::Bytes::copy_from_slice(key),
                    })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)?;
        Ok(resp.has)
    }

    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client
                    .get(GetRequest {
                        key: bytes::Bytes::copy_from_slice(key),
                    })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)?;
        Ok(resp.value.to_vec())
    }
}

impl KeyValueWriter for DatabaseClient {
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client
                    .put(PutRequest {
                        key: bytes::Bytes::copy_from_slice(key),
                        value: bytes::Bytes::copy_from_slice(value),
                    })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)
    }
}

impl KeyValueDeleter for DatabaseClient {
    fn delete(&self, key: &[u8]) -> Result<()> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client
                    .delete(DeleteRequest {
                        key: bytes::Bytes::copy_from_slice(key),
                    })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)
    }
}

impl Compacter for DatabaseClient {
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client
                    .compact(CompactRequest {
                        start: bytes::Bytes::copy_from_slice(start.unwrap_or(&[])),
                        limit: bytes::Bytes::copy_from_slice(limit.unwrap_or(&[])),
                    })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)
    }
}

impl Batcher for DatabaseClient {
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Box::new(RpcBatch {
            db: self,
            ops: BatchOps::new(),
        })
    }
}

impl Iteratee for DatabaseClient {
    type Iter<'a> = RpcIterator<'a>;

    fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> RpcIterator<'_> {
        let res = self.block_on(async {
            let mut client = self.inner.client.lock().clone();
            client
                .new_iterator_with_start_and_prefix(NewIteratorWithStartAndPrefixRequest {
                    start: bytes::Bytes::copy_from_slice(start),
                    prefix: bytes::Bytes::copy_from_slice(prefix),
                })
                .await
        });
        match res {
            Ok(resp) => RpcIterator {
                db: self,
                id: resp.into_inner().id,
                buf: Vec::new(),
                pos: 0,
                cur: None,
                exhausted: false,
                err: None,
                released: false,
            },
            Err(e) => RpcIterator {
                db: self,
                id: 0,
                buf: Vec::new(),
                pos: 0,
                cur: None,
                exhausted: true,
                err: Some(transport_err(e)),
                released: true,
            },
        }
    }
}

impl Database for DatabaseClient {
    fn close(&self) -> Result<()> {
        self.inner.closed.store(true, Ordering::SeqCst);
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client.close(CloseRequest {}).await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)
    }

    fn health_check(&self) -> Result<serde_json::Value> {
        let resp = self
            .block_on(async {
                let mut client = self.inner.client.lock().clone();
                client.health_check(()).await
            })
            .map_err(transport_err)?
            .into_inner();
        if resp.details.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&resp.details)
            .map_err(|e| Error::Other(anyhow::anyhow!("unmarshal health: {e}")))
    }
}

impl DynDatabase for DatabaseClient {
    fn has(&self, key: &[u8]) -> Result<bool> {
        KeyValueReader::has(self, key)
    }
    fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
        KeyValueReader::get(self, key)
    }
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        KeyValueWriter::put(self, key, value)
    }
    fn delete(&self, key: &[u8]) -> Result<()> {
        KeyValueDeleter::delete(self, key)
    }
    fn new_batch(&self) -> Box<dyn Batch + '_> {
        Batcher::new_batch(self)
    }
    fn new_iterator_with_start_and_prefix<'a>(
        &'a self,
        start: &[u8],
        prefix: &[u8],
    ) -> BoxIter<'a> {
        Box::new(Iteratee::new_iterator_with_start_and_prefix(
            self, start, prefix,
        ))
    }
    fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
        Compacter::compact(self, start, limit)
    }
    fn close(&self) -> Result<()> {
        Database::close(self)
    }
    fn health_check(&self) -> Result<serde_json::Value> {
        Database::health_check(self)
    }
}

/// A write-only batch buffering ops, flushed as one `WriteBatch` RPC.
struct RpcBatch<'a> {
    db: &'a DatabaseClient,
    ops: BatchOps,
}

impl WriteDelete for RpcBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.ops.put(key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.ops.delete(key);
        Ok(())
    }
}

impl Batch for RpcBatch<'_> {
    fn size(&self) -> usize {
        self.ops.size()
    }

    fn write(&mut self) -> Result<()> {
        // Port Go's de-dup: walk ops newest→oldest, keep only the last write per
        // key, splitting into puts/deletes. The host applies puts then deletes
        // in one atomic batch.
        let mut seen: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        let mut puts: Vec<PutRequest> = Vec::new();
        let mut deletes: Vec<DeleteRequest> = Vec::new();
        for op in self.ops.ops.iter().rev() {
            if !seen.insert(op.key.as_slice()) {
                continue;
            }
            if op.delete {
                deletes.push(DeleteRequest {
                    key: bytes::Bytes::copy_from_slice(&op.key),
                });
            } else {
                puts.push(PutRequest {
                    key: bytes::Bytes::copy_from_slice(&op.key),
                    value: bytes::Bytes::copy_from_slice(&op.value),
                });
            }
        }

        let resp = self
            .db
            .block_on(async {
                let mut client = self.db.inner.client.lock().clone();
                client
                    .write_batch(WriteBatchRequest { puts, deletes })
                    .await
            })
            .map_err(transport_err)?
            .into_inner();
        err_enum_to_result(resp.err)
    }

    fn reset(&mut self) {
        self.ops.reset();
    }

    fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        self.ops.replay(w)
    }

    fn inner(&mut self) -> &mut dyn Batch {
        self
    }
}

/// A client-side cursor over a server-side iterator (addressed by `id`).
///
/// Buffers each `IteratorNext` batch and serves pairs locally until empty, then
/// fetches the next batch (port of the Go `data`/`fetchedData` batching).
pub struct RpcIterator<'a> {
    db: &'a DatabaseClient,
    id: u64,
    buf: Vec<PutRequest>,
    pos: usize,
    /// The current (key, value); `None` before the first `next` / when done.
    cur: Option<(Vec<u8>, Vec<u8>)>,
    exhausted: bool,
    err: Option<Error>,
    released: bool,
}

impl RpcIterator<'_> {
    /// Fetches the next batch from the server. Returns `false` when no more data.
    fn fetch(&mut self) -> bool {
        if self.exhausted {
            return false;
        }
        let res = self.db.block_on(async {
            let mut client = self.db.inner.client.lock().clone();
            client
                .iterator_next(IteratorNextRequest { id: self.id })
                .await
        });
        match res {
            Ok(resp) => {
                let data = resp.into_inner().data;
                if data.is_empty() {
                    self.exhausted = true;
                    return false;
                }
                self.buf = data;
                self.pos = 0;
                true
            }
            Err(e) => {
                if self.err.is_none() {
                    self.err = Some(transport_err(e));
                }
                self.exhausted = true;
                false
            }
        }
    }
}

impl Iterator for RpcIterator<'_> {
    fn next(&mut self) -> bool {
        // Mirror Go's client iterator: a closed DB short-circuits to ErrClosed,
        // regardless of any buffered data (db_client.go `iterator.Next`).
        if self.db.inner.closed.load(Ordering::SeqCst) {
            self.cur = None;
            self.buf.clear();
            self.exhausted = true;
            if self.err.is_none() {
                self.err = Some(Error::Closed);
            }
            return false;
        }
        if self.err.is_some() {
            self.cur = None;
            return false;
        }
        if self.pos >= self.buf.len() && !self.fetch() {
            self.cur = None;
            return false;
        }
        let Some(item) = self.buf.get(self.pos) else {
            self.cur = None;
            return false;
        };
        self.cur = Some((item.key.to_vec(), item.value.to_vec()));
        self.pos = self.pos.saturating_add(1);
        true
    }

    fn error(&self) -> Result<()> {
        match &self.err {
            None => {}
            Some(Error::Closed) => return Err(Error::Closed),
            Some(Error::NotFound) => return Err(Error::NotFound),
            Some(Error::Other(e)) => return Err(Error::Other(anyhow::anyhow!("{e}"))),
        }
        // Query the server for any iteration error (e.g. a closed DB).
        if self.released {
            return Ok(());
        }
        let res = self.db.block_on(async {
            let mut client = self.db.inner.client.lock().clone();
            client
                .iterator_error(IteratorErrorRequest { id: self.id })
                .await
        });
        match res {
            Ok(resp) => err_enum_to_result(resp.into_inner().err),
            Err(e) => Err(transport_err(e)),
        }
    }

    fn key(&self) -> Option<&[u8]> {
        self.cur.as_ref().map(|(k, _)| k.as_slice())
    }

    fn value(&self) -> Option<&[u8]> {
        self.cur.as_ref().map(|(_, v)| v.as_slice())
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let res = self.db.block_on(async {
            let mut client = self.db.inner.client.lock().clone();
            client
                .iterator_release(IteratorReleaseRequest { id: self.id })
                .await
        });
        if let Ok(resp) = res
            && self.err.is_none()
            && let Err(e) = err_enum_to_result(resp.into_inner().err)
        {
            self.err = Some(e);
        }
        self.buf.clear();
        self.cur = None;
    }
}

impl Drop for RpcIterator<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

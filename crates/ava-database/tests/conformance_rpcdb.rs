// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! rpcdb conformance: a [`DatabaseClient`] talking to an in-process
//! [`DatabaseServer`] wrapping [`MemDb`] must pass the full shared `dbtest`
//! battery and the BTreeMap-oracle proptest (04 §2.8, 02 §6.1, §7.2).
//!
//! Transport: each pair runs the gRPC server on a dedicated multi-thread tokio
//! runtime bound to a loopback ephemeral TCP port; the client dials it over a
//! tonic `Channel`. The [`RpcDb`] wrapper owns both the client and the server's
//! shutdown handle + runtime, so dropping it tears the server down — important
//! because the proptest battery builds a fresh pair per case.
//!
//! The shared battery lives behind the `testutil` feature; the bodies are gated
//! on `testutil` + `rpcdb`. The `unused_crate_dependencies` allow is the known
//! integration-test false-positive (other package deps linked but unused here).

#![allow(clippy::unwrap_used, unused_crate_dependencies)]

#[cfg(all(feature = "testutil", feature = "rpcdb"))]
mod rpcdb_conformance {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use ava_database::dbtest::{run_database_proptests, run_database_suite};
    use ava_database::rpcdb::{DatabaseClient, DatabaseServer};
    use ava_database::traits::{
        Batch, BoxIter, Compacter, Database, DynDatabase, Iteratee, Iterator, KeyValueDeleter,
        KeyValueReader, KeyValueWriter,
    };
    use ava_database::{MemDb, Result};
    use tokio::runtime::Builder;
    use tokio::sync::oneshot;
    use tonic::transport::{Channel, Endpoint, Server};

    /// A self-contained rpcdb client+server pair over loopback TCP. Delegates the
    /// whole [`Database`] surface to the inner [`DatabaseClient`]; on drop it
    /// signals server shutdown and joins the server thread.
    struct RpcDb {
        client: DatabaseClient,
        shutdown: Option<oneshot::Sender<()>>,
        server: Option<JoinHandle<()>>,
    }

    impl RpcDb {
        fn new() -> Self {
            // std mpsc for the addr handoff (blocking recv on the sync test
            // thread); a tokio oneshot for the async shutdown signal.
            let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

            // Pick a free loopback port (bind then drop), then have tonic bind it.
            // The brief gap is fine for an in-process test.
            let addr: SocketAddr = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap()
            };
            addr_tx.send(addr).unwrap();

            let server = std::thread::spawn(move || {
                let rt = Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
                    let svc = DatabaseServer::new(db).into_service();
                    Server::builder()
                        .add_service(svc)
                        .serve_with_shutdown(addr, async {
                            let _ = shutdown_rx.await;
                        })
                        .await
                        .unwrap();
                });
            });

            let addr = addr_rx.recv().unwrap();

            // Multi-thread runtime for the client's blocking RPC calls so the
            // tonic connection driver keeps making progress between `block_on`s.
            let client_rt = Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            // Retry connect: the server thread may not have bound the port yet.
            let channel: Channel = client_rt.block_on(async move {
                let endpoint = Endpoint::from_shared(format!("http://{addr}")).unwrap();
                let mut last_err = None;
                for _ in 0..100 {
                    match endpoint.connect().await {
                        Ok(ch) => return ch,
                        Err(e) => {
                            last_err = Some(e);
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }
                }
                panic!("client failed to connect: {last_err:?}");
            });

            Self {
                client: DatabaseClient::new(client_rt, channel),
                shutdown: Some(shutdown_tx),
                server: Some(server),
            }
        }
    }

    impl Drop for RpcDb {
        fn drop(&mut self) {
            if let Some(tx) = self.shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(h) = self.server.take() {
                let _ = h.join();
            }
        }
    }

    // Delegate the full Database surface to the inner client so RpcDb satisfies
    // the `D: Database` bound of the shared battery.
    impl KeyValueReader for RpcDb {
        fn has(&self, key: &[u8]) -> Result<bool> {
            KeyValueReader::has(&self.client, key)
        }
        fn get(&self, key: &[u8]) -> Result<Vec<u8>> {
            KeyValueReader::get(&self.client, key)
        }
    }
    impl KeyValueWriter for RpcDb {
        fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
            KeyValueWriter::put(&self.client, key, value)
        }
    }
    impl KeyValueDeleter for RpcDb {
        fn delete(&self, key: &[u8]) -> Result<()> {
            KeyValueDeleter::delete(&self.client, key)
        }
    }
    impl Compacter for RpcDb {
        fn compact(&self, start: Option<&[u8]>, limit: Option<&[u8]>) -> Result<()> {
            Compacter::compact(&self.client, start, limit)
        }
    }
    impl ava_database::traits::Batcher for RpcDb {
        fn new_batch(&self) -> Box<dyn Batch + '_> {
            ava_database::traits::Batcher::new_batch(&self.client)
        }
    }
    /// Newtype so a boxed iterator satisfies the `Iter: Iterator` GAT bound.
    struct BoxedIter<'a>(BoxIter<'a>);
    impl Iterator for BoxedIter<'_> {
        fn next(&mut self) -> bool {
            self.0.next()
        }
        fn error(&self) -> Result<()> {
            self.0.error()
        }
        fn key(&self) -> Option<&[u8]> {
            self.0.key()
        }
        fn value(&self) -> Option<&[u8]> {
            self.0.value()
        }
        fn release(&mut self) {
            self.0.release();
        }
    }
    impl Iteratee for RpcDb {
        type Iter<'a> = BoxedIter<'a>;
        fn new_iterator_with_start_and_prefix(&self, start: &[u8], prefix: &[u8]) -> BoxedIter<'_> {
            BoxedIter(DynDatabase::new_iterator_with_start_and_prefix(
                &self.client,
                start,
                prefix,
            ))
        }
    }
    impl Database for RpcDb {
        fn close(&self) -> Result<()> {
            Database::close(&self.client)
        }
        fn health_check(&self) -> Result<serde_json::Value> {
            Database::health_check(&self.client)
        }
    }

    /// The full deterministic conformance battery (`dbtest.Tests`/`TestsBasic`)
    /// over a client↔server rpcdb round-trip.
    #[test]
    fn run_database_suite_rpcdb() {
        run_database_suite(RpcDb::new);
    }

    /// Any op sequence behaves like a `BTreeMap` oracle over the rpcdb round-trip.
    #[test]
    fn db_oracle_btreemap_rpcdb() {
        run_database_proptests(RpcDb::new);
    }
}

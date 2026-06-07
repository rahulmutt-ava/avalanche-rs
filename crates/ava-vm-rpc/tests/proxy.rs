// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The proxied callback services (specs 07 §5.2/§5.3/§5.4; plan M3.25).
//!
//! Symmetry: the plugin always **dials** the callback service; the node always
//! **serves** it. These tests stand up a host-served server over a loopback TCP
//! port and drive the guest-side client implementing the Rust trait.
//!
//! * `rpcdb_roundtrip` — a guest-side `RpcDatabase` (a `proto/rpcdb` client
//!   implementing [`ava_database::DynDatabase`]) put/get/delete/iterate
//!   round-trips against a host-served `memdb`, exercising the `ErrEnumToError`
//!   table and server-side iterator handles (batched `IteratorNext`).
//! * `appsender_roundtrip` — an `RpcAppSender` `send_app_request` reaches the
//!   host [`ava_vm::AppSender`].

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::{DynDatabase, MemDb};
use ava_types::node_id::NodeId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm_rpc::proxy;

/// Binds an ephemeral loopback listener and returns `(addr, incoming stream)`.
async fn bind() -> (String, tokio_stream::wrappers::TcpListenerStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    (addr, tokio_stream::wrappers::TcpListenerStream::new(listener))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpcdb_roundtrip() {
    // Host: serve a memdb over proto/rpcdb.
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let (addr, incoming) = bind().await;
    let server = proxy::rpcdb::serve(db).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // Guest: dial and build the RpcDatabase client (implements DynDatabase).
    // The client is *synchronous* (it owns a current-thread runtime and
    // `block_on`s each RPC; 04 §1.2), so it must be driven off the async test
    // runtime — exercise it on a blocking thread, exactly as a VM consuming the
    // `DynDatabase` would (the VM's blocking storage work runs at the call site).
    let addr2 = addr.clone();
    let seen = tokio::task::spawn_blocking(move || {
        let client = proxy::rpcdb::dial(&addr2).expect("dial rpcdb");

        // put/get/has/delete round-trip with the ErrEnumToError sentinel.
        client.put(b"k1", b"v1").expect("put");
        client.put(b"k2", b"v2").expect("put");
        assert!(client.has(b"k1").expect("has"));
        assert_eq!(client.get(b"k1").expect("get"), b"v1");
        assert!(matches!(
            client.get(b"missing"),
            Err(ava_database::Error::NotFound)
        ));
        client.delete(b"k1").expect("delete");
        assert!(!client.has(b"k1").expect("has after delete"));

        // Iterate (server-side iterator handle + batched IteratorNext).
        let mut it = client.new_iterator_with_start_and_prefix(b"", b"");
        let mut seen = Vec::new();
        while it.next() {
            seen.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
        }
        it.error().expect("iterator error");
        drop(it);
        seen
    })
    .await
    .expect("blocking rpcdb client task");

    assert_eq!(seen, vec![(b"k2".to_vec(), b"v2".to_vec())]);

    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appsender_roundtrip() {
    let token = CancellationToken::new();

    // Host: a recording AppSender served over proto/appsender.
    type Recorded = (Vec<NodeId>, u32, Vec<u8>);
    #[derive(Default)]
    struct Recorder {
        requests: Mutex<Vec<Recorded>>,
    }
    #[async_trait::async_trait]
    impl AppSender for Recorder {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            nodes: &HashSet<NodeId>,
            request_id: u32,
            bytes: Vec<u8>,
        ) -> ava_vm::Result<()> {
            let mut v: Vec<NodeId> = nodes.iter().copied().collect();
            v.sort();
            self.requests.lock().push((v, request_id, bytes));
            Ok(())
        }
        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> ava_vm::Result<()> {
            Ok(())
        }
        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> ava_vm::Result<()> {
            Ok(())
        }
        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> ava_vm::Result<()> {
            Ok(())
        }
    }

    let recorder = Arc::new(Recorder::default());
    let host: Arc<dyn AppSender> = recorder.clone();
    let (addr, incoming) = bind().await;
    let server = proxy::appsender::serve(host).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // Guest: dial and build the RpcAppSender (implements AppSender).
    let client = proxy::appsender::dial(&addr).await.expect("dial appsender");

    let node = NodeId::from_slice(&[7u8; 20]).unwrap();
    let nodes: HashSet<NodeId> = [node].into_iter().collect();
    client
        .send_app_request(&token, &nodes, 42, b"hello".to_vec())
        .await
        .expect("send_app_request");

    let got = recorder.requests.lock().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, vec![node]);
    assert_eq!(got[0].1, 42);
    assert_eq!(got[0].2, b"hello".to_vec());

    shutdown.cancel();
}

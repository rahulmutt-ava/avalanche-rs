// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Round-trip test for the `proto/sharedmemory` proxy (M9.6).
//!
//! Mirrors the pattern in `tests/proxy.rs`: bind an ephemeral loopback port,
//! serve a host-side impl via `tonic::transport::Server`, dial the guest-side
//! [`RpcSharedMemory`] client, and assert the three methods (`get`, `indexed`,
//! `apply`) produce the expected results end-to-end (ATOMIC-1, specs 07 §3.1).
//!
//! The [`SharedMemory`] trait is **synchronous**, so the guest client must be
//! driven from a blocking thread via `tokio::task::spawn_blocking`, exactly as
//! the `rpcdb_roundtrip` test does for [`RpcDatabase`].

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_database::BatchOps;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, IndexedResult, Requests, SharedMemory};
use ava_vm::error::Result;
use ava_vm_rpc::proxy;

/// Binds an ephemeral loopback listener and returns `(addr, incoming stream)`.
async fn bind() -> (String, tokio_stream::wrappers::TcpListenerStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr").to_string();
    (
        addr,
        tokio_stream::wrappers::TcpListenerStream::new(listener),
    )
}

// ---------------------------------------------------------------------------
// Minimal in-test `SharedMemory` implementation
// ---------------------------------------------------------------------------

/// `(peer_chain_id_bytes, key)` composite store key.
type StoreKey = (Vec<u8>, Vec<u8>);
/// `(value, traits)` composite store value.
type StoreVal = (Vec<u8>, Vec<Vec<u8>>);
/// Candidate row during `indexed` pagination: `(key, value, traits)`.
type Candidate = (Vec<u8>, Vec<u8>, Vec<Vec<u8>>);

/// A simple in-memory [`SharedMemory`] for testing.
///
/// Storage layout:
/// - `store`: `(peer_chain_id, key)` → `(value, traits)` for get/indexed
/// - After `apply` the put/remove requests are committed into `store`.
///
/// `indexed` returns all values whose traits intersect `traits`, honoring
/// pagination via `(start_trait, start_key)` and `limit`.
#[derive(Default)]
struct MockSharedMemory {
    /// Map of `(peer_chain_id_bytes, key)` → `(value, traits)`
    store: Mutex<BTreeMap<StoreKey, StoreVal>>,
}

impl SharedMemory for MockSharedMemory {
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
        let store = self.store.lock();
        let peer = peer_chain.to_bytes().to_vec();
        let values = keys
            .iter()
            .map(|k| {
                store
                    .get(&(peer.clone(), k.clone()))
                    .map(|(v, _)| v.clone())
                    .unwrap_or_default()
            })
            .collect();
        Ok(values)
    }

    fn indexed(
        &self,
        peer_chain: Id,
        traits: &[Vec<u8>],
        start_trait: &[u8],
        start_key: &[u8],
        limit: usize,
    ) -> Result<IndexedResult> {
        let store = self.store.lock();
        let peer = peer_chain.to_bytes().to_vec();

        // Collect all entries for this peer that have at least one matching trait.
        let mut candidates: Vec<Candidate> = store
            .iter()
            .filter_map(|((p, k), (v, elem_traits))| {
                if p != &peer {
                    return None;
                }
                let has_match = traits.iter().any(|t| elem_traits.contains(t));
                if !has_match {
                    return None;
                }
                Some((k.clone(), v.clone(), elem_traits.clone()))
            })
            .collect();

        // Sort deterministically: by (first matching trait, key).
        candidates.sort_by(|(ka, _, ta), (kb, _, tb)| {
            let ta_first = traits.iter().find(|t| ta.contains(t)).cloned().unwrap_or_default();
            let tb_first = traits.iter().find(|t| tb.contains(t)).cloned().unwrap_or_default();
            ta_first.cmp(&tb_first).then_with(|| ka.cmp(kb))
        });

        // Apply (start_trait, start_key) pagination: skip until we find the resume point.
        let start_idx = if start_trait.is_empty() && start_key.is_empty() {
            0
        } else {
            // Skip everything before the resume position (inclusive skip of the start item itself).
            candidates
                .iter()
                .position(|(k, _, elem_traits)| {
                    let first = traits.iter().find(|t| elem_traits.contains(t)).cloned().unwrap_or_default();
                    (first.as_slice(), k.as_slice()) >= (start_trait, start_key)
                })
                .unwrap_or(candidates.len())
        };
        let candidates = &candidates[start_idx..];

        // Apply limit.
        let take = if limit == 0 {
            candidates.len()
        } else {
            limit.min(candidates.len())
        };
        let page = &candidates[..take];

        let values: Vec<Vec<u8>> = page.iter().map(|(_, v, _)| v.clone()).collect();
        let last_trait: Vec<u8> = page.last().map(|(_, _, ts)| {
            traits.iter().find(|t| ts.contains(t)).cloned().unwrap_or_default()
        }).unwrap_or_default();
        let last_key: Vec<u8> = page.last().map(|(k, _, _)| k.clone()).unwrap_or_default();

        Ok((values, last_trait, last_key))
    }

    fn apply(&self, requests: BTreeMap<Id, Requests>, _batches: &[BatchOps]) -> Result<()> {
        let mut store = self.store.lock();
        for (chain, reqs) in requests {
            let peer = chain.to_bytes().to_vec();
            // Remove first.
            for key in reqs.remove {
                store.remove(&(peer.clone(), key));
            }
            // Then put.
            for elem in reqs.put {
                store.insert(
                    (peer.clone(), elem.key),
                    (elem.value, elem.traits),
                );
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The round-trip test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedmemory_proxy_get_indexed_apply() {
    // ---- Host: serve the mock over proto/sharedmemory -----------------------------------
    let mem: Arc<dyn SharedMemory> = Arc::new(MockSharedMemory::default());
    let (addr, incoming) = bind().await;
    let server = proxy::sharedmemory::serve(mem).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // ---- Guest: dial the synchronous RpcSharedMemory client -------------------------
    // The client is synchronous (owns a current-thread runtime); drive it from a
    // blocking thread, exactly as `rpcdb_roundtrip` does for `RpcDatabase`.
    let addr2 = addr.clone();
    tokio::task::spawn_blocking(move || {
        let client = proxy::sharedmemory::dial(&addr2).expect("dial sharedmemory");

        // Build stable peer chain IDs.
        let peer_a = Id::from_slice(&[1u8; 32]).expect("peer_a");
        let peer_b = Id::from_slice(&[2u8; 32]).expect("peer_b");

        // ---- apply: commit puts for peer_a via guest -------------------------------------------
        let trait_x = b"trait-x".to_vec();
        let trait_y = b"trait-y".to_vec();
        let put_requests: BTreeMap<Id, Requests> = {
            let mut m = BTreeMap::new();
            m.insert(
                peer_a,
                Requests {
                    remove: vec![],
                    put: vec![
                        Element {
                            key: b"k1".to_vec(),
                            value: b"v1".to_vec(),
                            traits: vec![trait_x.clone()],
                        },
                        Element {
                            key: b"k2".to_vec(),
                            value: b"v2".to_vec(),
                            traits: vec![trait_y.clone()],
                        },
                    ],
                },
            );
            m
        };
        client.apply(put_requests, &[]).expect("apply (put)");

        // ---- get: values are addressable by key after apply ------------------------------------
        let values = client.get(peer_a, &[b"k1".to_vec(), b"k2".to_vec()]).expect("get");
        assert_eq!(values.len(), 2, "get must return len == keys.len()");
        assert_eq!(values[0], b"v1".to_vec(), "get k1");
        assert_eq!(values[1], b"v2".to_vec(), "get k2");

        // ---- get: unknown key returns empty bytes (default) ------------------------------------
        let missing = client.get(peer_a, &[b"no-such-key".to_vec()]).expect("get missing");
        assert_eq!(missing.len(), 1, "get missing key must still return len==1");
        assert_eq!(missing[0], b"".as_slice(), "missing value should be empty");

        // ---- indexed: paginate over trait-matched values ---------------------------------------
        // Ask for trait_x matches — should return just k1/v1.
        let (vals, last_t, last_k) = client
            .indexed(peer_a, std::slice::from_ref(&trait_x), &[], &[], 10)
            .expect("indexed trait_x");
        assert_eq!(vals.len(), 1, "indexed trait_x should return 1 value");
        assert_eq!(vals[0], b"v1".to_vec(), "indexed trait_x value");
        assert!(!last_t.is_empty() || !last_k.is_empty(), "indexed must return pagination cursor");

        // Ask for both traits with limit=1 — should return exactly 1 value.
        let (vals_paged, _, _) = client
            .indexed(peer_a, &[trait_x.clone(), trait_y.clone()], &[], &[], 1)
            .expect("indexed limit=1");
        assert_eq!(vals_paged.len(), 1, "indexed with limit=1 should page to 1 result");

        // ---- apply: remove k1 from peer_a, then confirm get returns empty for it ---------------
        let remove_requests: BTreeMap<Id, Requests> = {
            let mut m = BTreeMap::new();
            m.insert(
                peer_a,
                Requests {
                    remove: vec![b"k1".to_vec()],
                    put: vec![],
                },
            );
            m
        };
        client.apply(remove_requests, &[]).expect("apply (remove)");

        // After removal, get k1 returns empty bytes; k2 is still present.
        let after_remove = client.get(peer_a, &[b"k1".to_vec(), b"k2".to_vec()]).expect("get after remove");
        assert_eq!(after_remove.len(), 2);
        assert_eq!(after_remove[0], b"".as_slice(), "k1 removed → empty");
        assert_eq!(after_remove[1], b"v2".to_vec(), "k2 still present");

        // ---- peer_b is a separate namespace: peer_a puts are not visible there ----------------
        let cross_chain = client.get(peer_b, &[b"k2".to_vec()]).expect("get peer_b");
        assert_eq!(cross_chain.len(), 1);
        assert_eq!(cross_chain[0], b"".as_slice(), "peer_b namespace is separate");
    })
    .await
    .expect("blocking sharedmemory client task");

    shutdown.cancel();
}

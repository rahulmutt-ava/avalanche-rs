// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Round-trip tests for the `proto/warp` Signer proxy and the
//! `proto/aliasreader` AliasReader proxy (M9.8).
//!
//! Both clients are **asynchronous** (`dial` is async, methods are async), so
//! they can be driven directly from the `#[tokio::test]` runtime — no
//! `spawn_blocking` needed, unlike the synchronous `rpcdb`/`sharedmemory`
//! proxies.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_crypto::bls;
use ava_types::id::Id;
use ava_vm::error::{Error, Result};
use ava_vm_rpc::proxy;
use ava_vm_rpc::proxy::aliasreader::AliaserReader;
use ava_vm_rpc::proxy::warp::Signer;

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

// ===========================================================================
// M9.8a — warp_signer_proxy_signs
// ===========================================================================

/// Host-side [`Signer`] backed by a BLS [`bls::LocalSigner`].
///
/// The warp proxy `Signer` trait (`proxy::warp::Signer`) takes
/// `(network_id, source_chain_id, payload)` and returns the raw signature
/// bytes. We implement it by signing the canonicalized payload bytes with the
/// SIGNATURE ciphersuite.
struct BLSWarpSigner {
    local: bls::LocalSigner,
}

impl BLSWarpSigner {
    /// Create a signer from a deterministic 32-byte IKM so tests are reproducible.
    fn from_ikm(ikm: &[u8; 32]) -> Self {
        let sk = bls::SecretKey::new(ikm).expect("SecretKey::new");
        let local = bls::LocalSigner::from_bytes(&sk.to_bytes()).expect("LocalSigner::from_bytes");
        BLSWarpSigner { local }
    }

    fn public_key(&self) -> bls::PublicKey {
        bls::Signer::public_key(&self.local).clone()
    }

    /// Encode the warp message fields deterministically for signing.
    fn encode_message(network_id: u32, source_chain_id: Id, payload: &[u8]) -> Vec<u8> {
        let mut msg = Vec::with_capacity(4 + 32 + payload.len());
        msg.extend_from_slice(&network_id.to_be_bytes());
        msg.extend_from_slice(&source_chain_id.to_bytes());
        msg.extend_from_slice(payload);
        msg
    }
}

#[async_trait]
impl Signer for BLSWarpSigner {
    async fn sign(&self, network_id: u32, source_chain_id: Id, payload: &[u8]) -> Result<Vec<u8>> {
        let msg = Self::encode_message(network_id, source_chain_id, payload);
        let sig = bls::Signer::sign(&self.local, &msg).map_err(|_| Error::HandshakeFailed)?;
        Ok(sig.compress().to_vec())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_signer_proxy_signs() {
    // ---- Host: serve the BLS-backed Signer ----------------------------------------
    let ikm: [u8; 32] = [0x42u8; 32];
    let host_signer = Arc::new(BLSWarpSigner::from_ikm(&ikm));
    let public_key = host_signer.public_key();

    let host: Arc<dyn Signer> = host_signer;
    let (addr, incoming) = bind().await;
    let server = proxy::warp::serve(host).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // ---- Guest: dial and exercise the RpcWarpSigner --------------------------------
    let client = proxy::warp::dial(&addr).await.expect("dial warp signer");

    let network_id: u32 = 1;
    let source_chain_id = Id::from_slice(&[0xABu8; 32]).expect("source_chain_id");
    let payload = b"hello warp";

    let sig_bytes = client
        .sign(network_id, source_chain_id, payload)
        .await
        .expect("sign");

    // Reconstruct the signature and verify it against the host's public key.
    let sig = bls::Signature::from_bytes(&sig_bytes).expect("Signature::from_bytes");
    let msg = BLSWarpSigner::encode_message(network_id, source_chain_id, payload);
    assert!(
        bls::verify(&public_key, &sig, &msg),
        "signature must verify against the host BLS public key"
    );

    // A different payload must NOT verify (sanity check).
    let wrong_sig = bls::verify(&public_key, &sig, b"wrong payload");
    assert!(!wrong_sig, "signature must not verify for a different message");

    shutdown.cancel();
}

// ===========================================================================
// M9.8b — aliasreader_proxy_resolves
// ===========================================================================

/// A simple in-memory [`AliaserReader`] for testing.
struct MockAliaserReader {
    /// alias → chain_id
    by_alias: HashMap<String, Id>,
    /// chain_id bytes (32-byte) → primary alias
    by_id: HashMap<Vec<u8>, String>,
    /// chain_id bytes → all aliases (primary first)
    all_aliases: HashMap<Vec<u8>, Vec<String>>,
}

impl MockAliaserReader {
    fn new(pairs: &[(&str, Id)]) -> Self {
        let mut by_alias = HashMap::new();
        let mut by_id = HashMap::new();
        let mut all_aliases: HashMap<Vec<u8>, Vec<String>> = HashMap::new();

        for (alias, id) in pairs {
            let key = id.to_bytes().to_vec();
            by_alias.insert((*alias).to_string(), *id);
            // First alias for each id becomes the primary alias.
            by_id.entry(key.clone()).or_insert_with(|| (*alias).to_string());
            all_aliases
                .entry(key)
                .or_default()
                .push((*alias).to_string());
        }
        MockAliaserReader { by_alias, by_id, all_aliases }
    }
}

#[async_trait]
impl AliaserReader for MockAliaserReader {
    async fn lookup(&self, alias: &str) -> Result<Id> {
        self.by_alias.get(alias).copied().ok_or(Error::NotFound)
    }

    async fn primary_alias(&self, id: Id) -> Result<String> {
        let key = id.to_bytes().to_vec();
        self.by_id.get(&key).cloned().ok_or(Error::NotFound)
    }

    async fn aliases(&self, id: Id) -> Result<Vec<String>> {
        let key = id.to_bytes().to_vec();
        self.all_aliases.get(&key).cloned().ok_or(Error::NotFound)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aliasreader_proxy_resolves() {
    // ---- Set up chain IDs and their aliases ----------------------------------------
    let chain_a = Id::from_slice(&[0x01u8; 32]).expect("chain_a");
    let chain_b = Id::from_slice(&[0x02u8; 32]).expect("chain_b");

    let mock = Arc::new(MockAliaserReader::new(&[
        ("Chain-A", chain_a),
        ("C", chain_a),      // second alias for chain_a
        ("Chain-B", chain_b),
    ]));

    // ---- Host: serve the mock over proto/aliasreader --------------------------------
    let host: Arc<dyn AliaserReader> = mock;
    let (addr, incoming) = bind().await;
    let server = proxy::aliasreader::serve(host).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // ---- Guest: dial and exercise the RpcAliasReader --------------------------------
    let client = proxy::aliasreader::dial(&addr).await.expect("dial aliasreader");

    // lookup: alias → chain id
    let resolved_a = client.lookup("Chain-A").await.expect("lookup Chain-A");
    assert_eq!(resolved_a, chain_a, "lookup Chain-A should resolve to chain_a");

    let resolved_c = client.lookup("C").await.expect("lookup C");
    assert_eq!(resolved_c, chain_a, "lookup C should also resolve to chain_a");

    let resolved_b = client.lookup("Chain-B").await.expect("lookup Chain-B");
    assert_eq!(resolved_b, chain_b, "lookup Chain-B should resolve to chain_b");

    // lookup: unknown alias → NotFound
    let not_found = client.lookup("no-such-alias").await;
    assert!(
        matches!(not_found, Err(Error::NotFound)),
        "lookup of unknown alias must return NotFound, got: {not_found:?}"
    );

    // primary_alias: chain id → primary alias
    let primary_a = client.primary_alias(chain_a).await.expect("primary_alias chain_a");
    assert_eq!(primary_a, "Chain-A", "primary alias of chain_a");

    let primary_b = client.primary_alias(chain_b).await.expect("primary_alias chain_b");
    assert_eq!(primary_b, "Chain-B", "primary alias of chain_b");

    // primary_alias: unknown chain → NotFound
    let unknown_chain = Id::from_slice(&[0xFFu8; 32]).expect("unknown");
    let not_found_id = client.primary_alias(unknown_chain).await;
    assert!(
        matches!(not_found_id, Err(Error::NotFound)),
        "primary_alias of unknown chain must return NotFound, got: {not_found_id:?}"
    );

    // aliases: all aliases for chain_a (Chain-A and C)
    let all_a = client.aliases(chain_a).await.expect("aliases chain_a");
    assert_eq!(all_a.len(), 2, "chain_a should have 2 aliases");
    assert!(all_a.contains(&"Chain-A".to_string()), "Chain-A in aliases");
    assert!(all_a.contains(&"C".to_string()), "C in aliases");

    shutdown.cancel();
}

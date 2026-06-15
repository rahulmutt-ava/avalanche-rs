// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Round-trip test for the `proto/validatorstate` proxy (M9.7).
//!
//! Stands up a host-served [`ValidatorState`] that returns a validator set with
//! a real BLS public key, then drives the guest [`RpcValidatorState`] client and
//! asserts the returned public key matches the source (tests the fix for the
//! uncompressed-key deserialization gap documented in the module-level comment of
//! `src/proxy/validatorstate.rs`).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_crypto::bls::{SecretKey, UNCOMPRESSED_PUBLIC_KEY_LEN};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorState;
use ava_validators::state::{GetCurrentValidatorOutput, WarpSet};
use ava_validators::validator::GetValidatorOutput;
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

/// A fixed-answer [`ValidatorState`] for test use.
struct StubValidatorState {
    min_height: u64,
    cur_height: u64,
    /// subnet_id → chain_id (reversed for look-up by chain)
    chain_subnet: HashMap<Id, Id>,
    /// (height, subnet_id) → validator set
    validator_set: BTreeMap<(u64, Id), BTreeMap<NodeId, GetValidatorOutput>>,
}

#[async_trait]
impl ValidatorState for StubValidatorState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        Ok(self.min_height)
    }

    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        Ok(self.cur_height)
    }

    async fn get_subnet_id(&self, chain: Id) -> ava_validators::Result<Id> {
        self.chain_subnet
            .get(&chain)
            .copied()
            .ok_or(ava_validators::Error::MissingValidators)
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> ava_validators::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        self.validator_set
            .get(&(height, subnet))
            .cloned()
            .ok_or(ava_validators::Error::MissingValidators)
    }

    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), self.cur_height))
    }

    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

/// Regression test for M9.7: the proxy must preserve BLS public keys across the
/// gRPC boundary.
///
/// The wire carries keys as **96-byte uncompressed** bytes (Go
/// `bls.PublicKeyToUncompressedBytes`). Before the fix, `decode_public_key` fell
/// back to `from_compressed` only and returned `None` for the 96-byte form —
/// causing every validator's public key to silently vanish in the guest view.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validatorstate_proxy_matches_source() {
    // ---------- generate a real BLS key pair ----------
    // Use a fixed IKM (32 bytes) so the test is deterministic.
    let ikm = [42u8; 32];
    let sk = SecretKey::new(&ikm).expect("SecretKey::new");
    let source_pk = sk.public_key();
    let source_bytes: [u8; UNCOMPRESSED_PUBLIC_KEY_LEN] = source_pk.serialize();

    // ---------- build the stub validator state ----------
    let subnet = Id::from_slice(&[1u8; 32]).expect("subnet id");
    let chain = Id::from_slice(&[2u8; 32]).expect("chain id");
    let node = NodeId::from_slice(&[3u8; 20]).expect("node id");

    let height: u64 = 100;
    let weight: u64 = 1_000;

    let mut validator_set_map = BTreeMap::new();
    let mut vset = BTreeMap::new();
    vset.insert(
        node,
        GetValidatorOutput {
            node_id: node,
            public_key: Some(source_pk),
            weight,
        },
    );
    validator_set_map.insert((height, subnet), vset);

    let mut chain_subnet = HashMap::new();
    chain_subnet.insert(chain, subnet);

    let host_state: Arc<dyn ValidatorState> = Arc::new(StubValidatorState {
        min_height: 10,
        cur_height: 200,
        chain_subnet,
        validator_set: validator_set_map,
    });

    // ---------- serve the host state over gRPC ----------
    let (addr, incoming) = bind().await;
    let server = proxy::validatorstate::serve(host_state).into_service();
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server)
            .serve_with_incoming_shutdown(incoming, async move { s2.cancelled().await })
            .await;
    });

    // ---------- dial the guest client ----------
    let client = proxy::validatorstate::dial(&addr)
        .await
        .expect("dial validatorstate");

    // --- height round-trips ---
    let min_h = client
        .get_minimum_height()
        .await
        .expect("get_minimum_height");
    assert_eq!(min_h, 10, "get_minimum_height");

    let cur_h = client.get_current_height().await.expect("get_current_height");
    assert_eq!(cur_h, 200, "get_current_height");

    // --- subnet id round-trips ---
    let got_subnet = client.get_subnet_id(chain).await.expect("get_subnet_id");
    assert_eq!(got_subnet, subnet, "get_subnet_id");

    // --- validator set with BLS public key round-trips ---
    let vset = client
        .get_validator_set(height, subnet)
        .await
        .expect("get_validator_set");

    let got = vset.get(&node).expect("node present in validator set");
    assert_eq!(got.weight, weight, "validator weight");

    // The public key must survive the wire: the host encodes it as 96-byte
    // uncompressed bytes; the guest must decode it back to the same key.
    let got_pk = got
        .public_key
        .as_ref()
        .expect("public key must be Some after proxy round-trip");
    let got_bytes: [u8; UNCOMPRESSED_PUBLIC_KEY_LEN] = got_pk.serialize();
    assert_eq!(
        got_bytes, source_bytes,
        "BLS public key must survive uncompressed-wire round-trip"
    );

    shutdown.cancel();
}

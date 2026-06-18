// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The host-side callback bundle multiplexed on a single `server_addr`
//! (M9.12 offline foundation; specs 07 §5.2).
//!
//! Go's `vm_client.go:newInitServer` registers sharedmemory + aliasreader +
//! appsender + validatorState + warp on one `server_addr`; a Go guest dials that
//! one address for every callback. [`serve_callback_bundle`] reproduces that
//! single-address contract. These tests act as the guest: they dial the one
//! bundle address five times (one client per service) and round-trip an op
//! against each, proving the services coexist — and that an unsupplied service is
//! answered by its no-op default.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};
use ava_vm::error::{Error as VmError, Result as VmResult};
use ava_vm_rpc::host::{CallbackBundle, serve_callback_bundle};
use ava_vm_rpc::proxy;
use ava_vm_rpc::proxy::aliasreader::AliaserReader;
use ava_vm_rpc::proxy::warp::Signer;

// Pulled in transitively; referenced so `unused_crate_dependencies` stays quiet.
use {tokio_stream as _, tonic as _};

// ── Concrete host-side impls (each returns a distinctive value) ──────────────

/// Records the request ids that reach the host over the appsender service.
#[derive(Default)]
struct RecordingAppSender {
    requests: Mutex<Vec<u32>>,
}

#[async_trait]
impl AppSender for RecordingAppSender {
    async fn send_app_request(
        &self,
        _t: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        request_id: u32,
        _bytes: Vec<u8>,
    ) -> VmResult<()> {
        self.requests.lock().push(request_id);
        Ok(())
    }
    async fn send_app_response(
        &self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _b: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _t: &CancellationToken,
        _n: NodeId,
        _r: u32,
        _c: i32,
        _m: &str,
    ) -> VmResult<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _t: &CancellationToken,
        _cfg: SendConfig,
        _b: Vec<u8>,
    ) -> VmResult<()> {
        Ok(())
    }
}

/// A `SharedMemory` whose `get` returns a fixed value for every requested key.
struct FixedSharedMemory {
    value: Vec<u8>,
}

impl SharedMemory for FixedSharedMemory {
    fn get(&self, _peer_chain: Id, keys: &[Vec<u8>]) -> VmResult<Vec<Vec<u8>>> {
        Ok(vec![self.value.clone(); keys.len()])
    }
    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> VmResult<IndexedResult> {
        Ok((Vec::new(), Vec::new(), Vec::new()))
    }
    fn apply(
        &self,
        _requests: BTreeMap<Id, Requests>,
        _batches: &[ava_database::BatchOps],
    ) -> VmResult<()> {
        Ok(())
    }
}

/// An `AliaserReader` resolving one known alias to one known id.
struct FixedAliaser {
    alias: String,
    id: Id,
}

#[async_trait]
impl AliaserReader for FixedAliaser {
    async fn lookup(&self, alias: &str) -> VmResult<Id> {
        if alias == self.alias {
            Ok(self.id)
        } else {
            Err(VmError::NotFound)
        }
    }
    async fn primary_alias(&self, _id: Id) -> VmResult<String> {
        Ok(self.alias.clone())
    }
    async fn aliases(&self, _id: Id) -> VmResult<Vec<String>> {
        Ok(vec![self.alias.clone()])
    }
}

/// A `ValidatorState` reporting one known current height.
struct FixedValidatorState {
    height: u64,
}

#[async_trait]
impl ValidatorState for FixedValidatorState {
    async fn get_minimum_height(&self) -> ava_validators::error::Result<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> ava_validators::error::Result<u64> {
        Ok(self.height)
    }
    async fn get_subnet_id(&self, _chain: Id) -> ava_validators::error::Result<Id> {
        Ok(Id::EMPTY)
    }
    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> ava_validators::error::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(BTreeMap::new())
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::error::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 0))
    }
    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::error::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

/// A `Signer` returning a fixed signature blob (the warp proxy forwards the bytes
/// verbatim, so the exact value round-trips).
struct FixedSigner {
    signature: Vec<u8>,
}

#[async_trait]
impl Signer for FixedSigner {
    async fn sign(
        &self,
        _network_id: u32,
        _source_chain_id: Id,
        _payload: &[u8],
    ) -> VmResult<Vec<u8>> {
        Ok(self.signature.clone())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// All five callback services are reachable on the single `server_addr` the
/// bundle binds — the Go single-address contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bundle_multiplexes_all_callbacks_on_one_server_addr() {
    let app_recorder = Arc::new(RecordingAppSender::default());
    let app_sender: Arc<dyn AppSender> = Arc::clone(&app_recorder) as Arc<dyn AppSender>;

    let alias_id = Id::from_slice(&[0x11u8; 32]).expect("alias id");
    let bundle = CallbackBundle {
        shared_memory: Some(Arc::new(FixedSharedMemory {
            value: b"shared-val".to_vec(),
        })),
        aliaser: Some(Arc::new(FixedAliaser {
            alias: "Chain-X".to_string(),
            id: alias_id,
        })),
        validator_state: Some(Arc::new(FixedValidatorState { height: 4242 })),
        warp_signer: Some(Arc::new(FixedSigner {
            signature: vec![0xACu8; 96],
        })),
    };

    let shutdown = CancellationToken::new();
    let server_addr = serve_callback_bundle(app_sender, bundle, shutdown.clone())
        .await
        .expect("serve bundle");

    // appsender — drive one request, assert it reached the host recorder.
    let app_client = proxy::appsender::dial(&server_addr)
        .await
        .expect("dial appsender");
    let token = CancellationToken::new();
    app_client
        .send_app_request(&token, &HashSet::new(), 77, b"hi".to_vec())
        .await
        .expect("send_app_request");
    assert_eq!(
        app_recorder.requests.lock().clone(),
        vec![77],
        "appsender request reached the host over the shared server_addr"
    );

    // sharedmemory — sync client, drive from a blocking thread (04 §1.2).
    let sm_addr = server_addr.clone();
    let sm_value = tokio::task::spawn_blocking(move || {
        let client = proxy::sharedmemory::dial(&sm_addr).expect("dial sharedmemory");
        client
            .get(Id::EMPTY, &[b"k".to_vec()])
            .expect("sharedmemory get")
    })
    .await
    .expect("blocking sharedmemory task");
    assert_eq!(
        sm_value,
        vec![b"shared-val".to_vec()],
        "sharedmemory get reached the host on the shared server_addr"
    );

    // aliasreader — resolve the known alias.
    let alias_client = proxy::aliasreader::dial(&server_addr)
        .await
        .expect("dial aliasreader");
    let resolved = alias_client.lookup("Chain-X").await.expect("lookup");
    assert_eq!(
        resolved, alias_id,
        "aliasreader lookup reached the host on the shared server_addr"
    );

    // validatorState — read the known current height.
    let vs_client = proxy::validatorstate::dial(&server_addr)
        .await
        .expect("dial validatorstate");
    let height = vs_client
        .get_current_height()
        .await
        .expect("get_current_height");
    assert_eq!(
        height, 4242,
        "validatorState reached the host on the shared server_addr"
    );

    // warp — sign and assert the forwarded bytes.
    let warp_client = proxy::warp::dial(&server_addr).await.expect("dial warp");
    let sig = warp_client
        .sign(1, Id::EMPTY, b"payload")
        .await
        .expect("warp sign");
    assert_eq!(
        sig,
        vec![0xACu8; 96],
        "warp signer reached the host on the shared server_addr"
    );

    shutdown.cancel();
}

/// An unsupplied service is served by its no-op default, so the guest's dial-back
/// still succeeds and benign queries answer (rather than the connection failing).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bundle_noop_defaults_answer_when_unsupplied() {
    let app_sender: Arc<dyn AppSender> = Arc::new(RecordingAppSender::default());
    // Default bundle: every callback is a no-op.
    let shutdown = CancellationToken::new();
    let server_addr =
        serve_callback_bundle(app_sender, CallbackBundle::default(), shutdown.clone())
            .await
            .expect("serve bundle");

    // sharedmemory no-op: get returns exactly keys.len() empty values.
    let sm_addr = server_addr.clone();
    let sm_value = tokio::task::spawn_blocking(move || {
        let client = proxy::sharedmemory::dial(&sm_addr).expect("dial sharedmemory");
        client
            .get(Id::EMPTY, &[b"a".to_vec(), b"b".to_vec()])
            .expect("sharedmemory get")
    })
    .await
    .expect("blocking sharedmemory task");
    assert_eq!(
        sm_value,
        vec![Vec::<u8>::new(), Vec::<u8>::new()],
        "no-op sharedmemory returns one empty value per key"
    );

    // validatorState no-op: current height is 0.
    let vs_client = proxy::validatorstate::dial(&server_addr)
        .await
        .expect("dial validatorstate");
    assert_eq!(
        vs_client.get_current_height().await.expect("height"),
        0,
        "no-op validatorState reports height 0"
    );

    // aliasreader no-op: every lookup is NotFound.
    let alias_client = proxy::aliasreader::dial(&server_addr)
        .await
        .expect("dial aliasreader");
    let miss = alias_client.lookup("anything").await;
    assert!(
        matches!(miss, Err(VmError::NotFound)),
        "no-op aliasreader lookup is NotFound, got: {miss:?}"
    );

    // warp no-op: signing fails (no backend supplied).
    let warp_client = proxy::warp::dial(&server_addr).await.expect("dial warp");
    let signed = warp_client.sign(1, Id::EMPTY, b"x").await;
    assert!(signed.is_err(), "no-op warp signer cannot sign");

    shutdown.cancel();
}

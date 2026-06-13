// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Differential API-parity test against the recorded Go oracle (M8.23;
//! specs 14 §14/§16.6, 12 §12.4, 02 §9/§11.4).
//!
//! `vectors/api/api_parity.json` is emitted by the in-repo Go oracle
//! (`go-oracle/api_parity_oracle_test.go`, run inside avalanchego at the
//! commit recorded in `goCommit`). For the built-in node-level services that
//! `ava-api` itself hosts — **info**, **admin**, **health** — the oracle pins
//! the REAL Go reply struct marshaled through the production `utils/json`
//! codec. This test drives the identical pinned request at the in-process Rust
//! service through the JSON-RPC registry + `ava_api::dispatch` and asserts
//! **structural-JSON-equality** after normalizing the documented
//! non-deterministic fields (02 §11.4 — health `timestamp` / `duration`).
//!
//! It also asserts:
//! - **method-set completeness** (14 §14.2): the registered Rust method set is
//!   exactly the Go method set for every service. P-Chain (`platform.*`, 31)
//!   and X-Chain (`avm.*`, 11) cannot be driven in-process from `ava-api`
//!   (the VM crates must not import `ava-api` — a dependency cycle); their
//!   reply-shape parity lives in the differential tests inside those crates
//!   (M8.23a / M8.23b). Their canonical wire-name sets are pinned here so no
//!   method goes silently missing (no-silent-caps).
//! - **error-response snapshots** (14 §16.6): bad params `-32602`, unknown
//!   method `-32601`, malformed JSON `-32700`, wrong version `-32600`, server
//!   error `-32000` — driven through the real Rust dispatch shim.
//! - **HTTP semantics** (14 §16.3): the `node-id` response header on every
//!   response (incl. the allowed-hosts `403` short-circuit), the per-chain
//!   not-bootstrapped `503`, and the health GET `200`/`503`.
//!
//! See `tests/PORTING.md` for the regeneration command + the documented
//! known-divergence list.

// An integration-test target indexes into fixtures + `serde_json::Value`
// replies and never uses every lib dep (precedent: golden_metrics_names.rs).
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use ava_api::admin::{
    Admin, AdminConfig, AliasAdder, ChainAliaser, LoggerLevels, SeamError, VmRegistry, VmReload,
};
use ava_api::health::{CheckResult, Checker, Health, handler as health_handler};
use ava_api::info::types::{PeerInfo, ProofOfPossession, UptimeResult};
use ava_api::info::{
    Benchlist, ChainManager, Info, InfoNetwork, Parameters, ValidatorSet, VmManager,
};
use ava_api::{ServiceRegistry, dispatch};
use ava_database::traits::KeyValueReader;
use ava_logging::AvaLevel;
use ava_snow::context::{ChainContext, ConsensusContext};
use ava_snow::{EngineState, NoOpAcceptor};
use ava_types::constants::MAINNET_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::CURRENT;
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::post;
use futures::future::BoxFuture;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Vector schema (mirrors the Go emitter structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    go_commit: String,
    emitter: String,
    services: BTreeMap<String, ServiceMethods>,
    errors: Vec<ErrorSnapshot>,
}

#[derive(Debug, Deserialize)]
struct ServiceMethods {
    service: String,
    methods: Vec<String>,
    // Go emits `null` (not `[]`) for a service with no recorded calls
    // (platform / avm), so accept null-or-absent as an empty list.
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    calls: Vec<MethodCall>,
}

fn null_as_empty_vec<'de, D>(deserializer: D) -> std::result::Result<Vec<MethodCall>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<MethodCall>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct MethodCall {
    method: String,
    params: Value,
    response: Value,
}

#[derive(Debug, Deserialize)]
struct ErrorSnapshot {
    name: String,
    code: i32,
}

fn vectors() -> Vectors {
    let raw = include_str!("vectors/api/api_parity.json");
    let v: Vectors = serde_json::from_str(raw).expect("parse api_parity.json");
    assert!(
        !v.go_commit.is_empty(),
        "vector provenance (goCommit) recorded"
    );
    assert!(!v.emitter.is_empty(), "vector emitter recorded");
    v
}

fn service<'a>(v: &'a Vectors, name: &str) -> &'a ServiceMethods {
    v.services
        .get(name)
        .unwrap_or_else(|| panic!("service {name} present in vectors"))
}

// ---------------------------------------------------------------------------
// Driving a registry-mounted service in-process (the indexer-test `rpc` seam)
// ---------------------------------------------------------------------------

/// POSTs a gorilla-json2 request through `ava_api::dispatch` and returns the
/// full JSON-RPC response body.
async fn post_rpc(registry: Arc<ServiceRegistry>, method: &str, params: &Value) -> Value {
    let router = Router::new()
        .route("/", post(dispatch))
        .with_state(registry);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": [params],
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
        .expect("request");
    let response = router.oneshot(request).await.expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK, "{method} HTTP status");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

/// Drives `method` and returns the `result` value (asserting no error body).
async fn rpc_result(registry: Arc<ServiceRegistry>, method: &str, params: &Value) -> Value {
    let body = post_rpc(registry, method, params).await;
    assert!(
        body.get("error").is_none(),
        "{method} returned an error: {body}"
    );
    body["result"].clone()
}

/// The gorilla first-letter-uppercasing shim: lowercase client name ->
/// registered Go wire name.
fn pascalize_first(client_name: &str) -> String {
    let mut chars = client_name.chars();
    let first = chars.next().expect("non-empty method name");
    first.to_uppercase().chain(chars).collect()
}

// ---------------------------------------------------------------------------
// Normalizer (spec 02 §11.4): blank documented volatile fields before compare.
// ---------------------------------------------------------------------------

/// Blanks the health-report volatile fields (`checks.*.timestamp` /
/// `checks.*.duration`): wall-clock + measured durations are non-deterministic
/// (02 §11.4) and are normalized out on both the Go and Rust sides.
fn normalize_health(value: &mut Value) {
    if let Some(checks) = value.get_mut("checks").and_then(Value::as_object_mut) {
        for check in checks.values_mut() {
            if let Some(obj) = check.as_object_mut() {
                obj.remove("timestamp");
                obj.remove("duration");
            }
        }
    }
}

// ===========================================================================
// info — drive every recorded call at the real Rust Info service
// ===========================================================================

#[derive(Default)]
struct InfoMocks {
    aliases: BTreeMap<String, Id>,
    primary: BTreeMap<Id, String>,
    bootstrapped: BTreeSet<Id>,
    peers: Vec<PeerInfo>,
    weights: BTreeMap<NodeId, u64>,
    total_weight: u64,
    benched: BTreeMap<NodeId, Vec<Id>>,
    versions: BTreeMap<String, String>,
    factories: Vec<Id>,
    vm_aliases: BTreeMap<Id, Vec<String>>,
}

struct InfoChainMgr(InfoMocks);
impl ChainManager for InfoChainMgr {
    fn lookup(&self, alias: &str) -> Result<Id, String> {
        self.0
            .aliases
            .get(alias)
            .copied()
            .ok_or_else(|| format!("there is no ID with alias: {alias}"))
    }
    fn primary_alias(&self, chain_id: Id) -> Result<String, String> {
        self.0
            .primary
            .get(&chain_id)
            .cloned()
            .ok_or_else(|| format!("there is no alias for ID: {chain_id}"))
    }
    fn is_bootstrapped(&self, chain_id: Id) -> bool {
        self.0.bootstrapped.contains(&chain_id)
    }
}

struct InfoNet {
    peers: Vec<PeerInfo>,
    uptime: UptimeResult,
}
impl InfoNetwork for InfoNet {
    fn peer_info(&self, node_ids: &[NodeId]) -> Vec<PeerInfo> {
        if node_ids.is_empty() {
            return self.peers.clone();
        }
        self.peers
            .iter()
            .filter(|p| node_ids.contains(&p.node_id))
            .cloned()
            .collect()
    }
    fn node_uptime(&self) -> Result<UptimeResult, String> {
        Ok(self.uptime)
    }
}

struct InfoBench(BTreeMap<NodeId, Vec<Id>>);
impl Benchlist for InfoBench {
    fn get_benched(&self, node_id: NodeId) -> Vec<Id> {
        self.0.get(&node_id).cloned().unwrap_or_default()
    }
}

struct InfoVals {
    weights: BTreeMap<NodeId, u64>,
    total: u64,
}
impl ValidatorSet for InfoVals {
    fn get_weight(&self, _subnet_id: Id, node_id: NodeId) -> u64 {
        self.weights.get(&node_id).copied().unwrap_or_default()
    }
    fn total_weight(&self, _subnet_id: Id) -> Result<u64, String> {
        Ok(self.total)
    }
}

struct InfoVms {
    versions: BTreeMap<String, String>,
    factories: Vec<Id>,
    aliases: BTreeMap<Id, Vec<String>>,
}
impl VmManager for InfoVms {
    fn versions(&self) -> Result<BTreeMap<String, String>, String> {
        Ok(self.versions.clone())
    }
    fn list_factories(&self) -> Result<Vec<Id>, String> {
        Ok(self.factories.clone())
    }
    fn aliases(&self, vm_id: Id) -> Vec<String> {
        self.aliases.get(&vm_id).cloned().unwrap_or_default()
    }
}

/// Builds the `Info` service with fixtures matching the Go oracle's pinned
/// inputs (node id `[7;20]`, BLS PoP `01..`/`02..`, mainnet, X alias =
/// `[3;32]`, avm = `[8;32]`, one peer `[9;20]` benched on `C`, etc.).
fn info_fixture() -> Arc<Info> {
    let node_id = NodeId::from([7u8; 20]);
    let x_chain = Id::from([3u8; 32]);
    let avm_id = Id::from([8u8; 32]);
    let peer_id = NodeId::from([9u8; 20]);
    let benched_chain = Id::from([4u8; 32]);
    let subnet = Id::from([5u8; 32]);

    let peer = PeerInfo {
        ip: "10.0.0.1:9651".parse().expect("static addr"),
        public_ip: None,
        node_id: peer_id,
        version: "avalanchego/1.14.2".to_string(),
        upgrade_time: 1_607_144_400,
        last_sent: chrono::DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
            .expect("rfc3339")
            .with_timezone(&chrono::Utc),
        last_received: chrono::DateTime::parse_from_rfc3339("2026-06-11T12:00:01Z")
            .expect("rfc3339")
            .with_timezone(&chrono::Utc),
        observed_uptime: 100,
        tracked_subnets: BTreeSet::from([subnet]),
        supported_acps: BTreeSet::from([23u32, 103u32, 5u32]),
        objected_acps: BTreeSet::new(),
    };

    let parameters = Parameters {
        version: CURRENT.clone(),
        git_commit: "de4da4de".to_string(),
        node_id,
        node_pop: ProofOfPossession::new([1u8; 48], [2u8; 96]),
        network_id: MAINNET_ID,
        upgrades: ava_version::upgrade::get_config(MAINNET_ID),
        tx_fee: 1_000_000,
        create_asset_tx_fee: 10_000_000,
    };

    let mocks = InfoMocks {
        aliases: BTreeMap::from([("X".to_string(), x_chain)]),
        primary: BTreeMap::from([(benched_chain, "C".to_string())]),
        bootstrapped: BTreeSet::from([x_chain]),
        peers: vec![peer.clone()],
        weights: BTreeMap::new(),
        total_weight: 0,
        benched: BTreeMap::from([(peer_id, vec![benched_chain])]),
        versions: BTreeMap::from([
            ("avm".to_string(), "v1.14.2".to_string()),
            ("platform".to_string(), "v1.14.2".to_string()),
        ]),
        factories: vec![avm_id],
        vm_aliases: BTreeMap::from([(avm_id, vec![avm_id.to_string(), "avm".to_string()])]),
    };

    // `isBootstrapped(P)` must be true (the Go vector pins P bootstrapped); the
    // service resolves `P` -> a chain id, so register that alias too.
    let p_chain = Id::from([10u8; 32]);
    let mut aliases = mocks.aliases.clone();
    aliases.insert("P".to_string(), p_chain);
    let mut bootstrapped = mocks.bootstrapped.clone();
    bootstrapped.insert(p_chain);

    let chain_mgr = InfoChainMgr(InfoMocks {
        aliases,
        primary: mocks.primary.clone(),
        bootstrapped,
        ..InfoMocks::default()
    });
    let net = InfoNet {
        peers: mocks.peers.clone(),
        uptime: UptimeResult {
            rewarding_stake_percentage: 91.5,
            weighted_average_percentage: 98.123_456,
        },
    };
    let my_ip: SocketAddr = "127.0.0.1:9651".parse().expect("static addr");
    Arc::new(Info::new(
        parameters,
        Arc::new(InfoVals {
            weights: mocks.weights.clone(),
            total: mocks.total_weight,
        }),
        Arc::new(chain_mgr),
        Arc::new(InfoVms {
            versions: mocks.versions.clone(),
            factories: mocks.factories.clone(),
            aliases: mocks.vm_aliases.clone(),
        }),
        Arc::new(parking_lot::RwLock::new(my_ip)),
        Arc::new(net),
        Arc::new(InfoBench(mocks.benched.clone())),
    ))
}

#[tokio::test]
async fn info_parity() {
    let v = vectors();
    let svc = service(&v, "info");

    // Method-set completeness (14 §14.2).
    let mut reg = ServiceRegistry::new();
    info_fixture().register_rpc(&mut reg);
    assert_eq!(reg.len(), svc.methods.len(), "info registered method count");
    for client_name in &svc.methods {
        let go_name = pascalize_first(client_name);
        assert!(
            reg.lookup("info", &go_name).is_some(),
            "info.{client_name} (Go {go_name}) registered"
        );
    }

    // Reply-shape parity for every recorded call.
    for call in &svc.calls {
        let registry = {
            let mut reg = ServiceRegistry::new();
            info_fixture().register_rpc(&mut reg);
            Arc::new(reg)
        };
        let got = rpc_result(registry, &format!("info.{}", call.method), &call.params).await;
        assert_eq!(
            got, call.response,
            "info.{} reply structural-JSON-equal vs Go",
            call.method
        );
    }
}

// ===========================================================================
// admin — drive the recorded calls at the real Rust Admin service
// ===========================================================================

struct AdminChainAliaser {
    aliases: BTreeMap<Id, Vec<String>>,
    lookup: BTreeMap<String, Id>,
}
impl ChainAliaser for AdminChainAliaser {
    fn lookup(&self, alias: &str) -> Result<Id, SeamError> {
        self.lookup
            .get(alias)
            .copied()
            .ok_or_else(|| -> SeamError { format!("no chain {alias}").into() })
    }
    fn alias(&self, _chain_id: Id, _alias: &str) -> Result<(), SeamError> {
        Ok(())
    }
    fn aliases(&self, chain_id: Id) -> Result<Vec<String>, SeamError> {
        Ok(self.aliases.get(&chain_id).cloned().unwrap_or_default())
    }
}

struct AdminLoggers {
    levels: BTreeMap<String, (AvaLevel, AvaLevel)>,
}
impl LoggerLevels for AdminLoggers {
    fn logger_names(&self) -> Vec<String> {
        self.levels.keys().cloned().collect()
    }
    fn log_level(&self, name: &str) -> Result<AvaLevel, SeamError> {
        self.levels
            .get(name)
            .map(|(l, _)| *l)
            .ok_or_else(|| -> SeamError { format!("no logger {name}").into() })
    }
    fn display_level(&self, name: &str) -> Result<AvaLevel, SeamError> {
        self.levels
            .get(name)
            .map(|(_, d)| *d)
            .ok_or_else(|| -> SeamError { format!("no logger {name}").into() })
    }
    fn set_log_level(&self, _name: &str, _level: AvaLevel) -> Result<(), SeamError> {
        Ok(())
    }
    fn set_display_level(&self, _name: &str, _level: AvaLevel) -> Result<(), SeamError> {
        Ok(())
    }
}

struct AdminAliasAdder;
impl AliasAdder for AdminAliasAdder {
    fn add_aliases(&self, _endpoint: &str, _aliases: &[String]) -> ava_api::Result<()> {
        Ok(())
    }
}

struct AdminVmRegistry {
    reload: VmReload,
}
#[async_trait::async_trait]
impl VmRegistry for AdminVmRegistry {
    async fn reload(&self) -> Result<VmReload, SeamError> {
        Ok(self.reload.clone())
    }
}

/// An empty raw DB (admin `dbGet` is not exercised here; the seam must exist).
struct EmptyDb;
impl KeyValueReader for EmptyDb {
    fn has(&self, _key: &[u8]) -> ava_database::error::Result<bool> {
        Ok(false)
    }
    fn get(&self, _key: &[u8]) -> ava_database::error::Result<Vec<u8>> {
        Err(ava_database::error::Error::NotFound)
    }
}

fn admin_fixture() -> Arc<Admin> {
    let x_chain = Id::from([3u8; 32]);
    let avm_id = Id::from([8u8; 32]);
    Admin::new(AdminConfig {
        profile_dir: std::env::temp_dir(),
        log_levels: Arc::new(AdminLoggers {
            levels: BTreeMap::from([("C".to_string(), (AvaLevel::Info, AvaLevel::Info))]),
        }),
        node_config: json!({}),
        db: Arc::new(EmptyDb),
        chain_manager: Arc::new(AdminChainAliaser {
            aliases: BTreeMap::from([(x_chain, vec!["X".to_string(), x_chain.to_string()])]),
            lookup: BTreeMap::from([(x_chain.to_string(), x_chain), ("X".to_string(), x_chain)]),
        }),
        http_server: Arc::new(AdminAliasAdder),
        vm_registry: Arc::new(AdminVmRegistry {
            reload: VmReload {
                new_vms: BTreeMap::from([(avm_id, vec!["avm".to_string()])]),
                failed_vms: BTreeMap::new(),
            },
        }),
    })
}

#[tokio::test]
async fn admin_parity() {
    let v = vectors();
    let svc = service(&v, "admin");

    let mut reg = ServiceRegistry::new();
    admin_fixture().register_rpc(&mut reg);
    assert_eq!(
        reg.len(),
        svc.methods.len(),
        "admin registered method count"
    );
    for client_name in &svc.methods {
        let go_name = pascalize_first(client_name);
        assert!(
            reg.lookup("admin", &go_name).is_some(),
            "admin.{client_name} (Go {go_name}) registered"
        );
    }

    for call in &svc.calls {
        let registry = {
            let mut reg = ServiceRegistry::new();
            admin_fixture().register_rpc(&mut reg);
            Arc::new(reg)
        };
        let got = rpc_result(registry, &format!("admin.{}", call.method), &call.params).await;
        assert_eq!(
            got, call.response,
            "admin.{} reply structural-JSON-equal vs Go",
            call.method
        );
    }
}

// ===========================================================================
// health — drive the recorded calls; normalize timestamp/duration (02 §11.4)
// ===========================================================================

/// A check that always passes with details `"ok"`.
fn passing_ok() -> Arc<dyn Checker> {
    Arc::new(|| -> BoxFuture<'static, CheckResult> { Box::pin(async move { Ok(json!("ok")) }) })
}

async fn health_fixture() -> Arc<Health> {
    let health = Arc::new(Health::new(&prometheus::Registry::new()).expect("Health::new()"));
    // All three reporters must surface check "c" => "ok" so health/readiness/
    // liveness all match the recorded healthy report.
    health
        .register_health_check("c", passing_ok(), &[])
        .expect("register health c");
    health
        .register_readiness_check("c", passing_ok(), &[])
        .expect("register readiness c");
    health
        .register_liveness_check("c", passing_ok(), &[])
        .expect("register liveness c");
    health.run_checks_now().await;
    health
}

#[tokio::test]
async fn health_parity() {
    let v = vectors();
    let svc = service(&v, "health");
    let health = health_fixture().await;

    // Method-set completeness via the mounted /ext/health router: a POST of
    // each gorilla name must resolve (not -32601).
    let router = health_handler(Arc::clone(&health));
    for client_name in &svc.methods {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": format!("health.{client_name}"),
            "params": [{}],
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("request");
        let resp = router.clone().oneshot(request).await.expect("oneshot");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let parsed: Value = serde_json::from_slice(&bytes).expect("json");
        assert!(
            parsed.get("error").is_none(),
            "health.{client_name} resolves"
        );
    }

    // Reply-shape parity (normalized).
    for call in &svc.calls {
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": format!("health.{}", call.method),
            "params": [call.params],
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("request");
        let resp = router.clone().oneshot(request).await.expect("oneshot");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let parsed: Value = serde_json::from_slice(&bytes).expect("json");
        let mut got = parsed["result"].clone();
        let mut want = call.response.clone();
        normalize_health(&mut got);
        normalize_health(&mut want);
        assert_eq!(
            got, want,
            "health.{} reply structural-JSON-equal vs Go (normalized)",
            call.method
        );
    }
}

// ===========================================================================
// platform / avm — method-set completeness only (documented divergence)
// ===========================================================================

#[test]
fn platform_and_avm_method_sets_pinned() {
    let v = vectors();
    // The reply-shape parity for these lives in ava-platformvm / ava-avm
    // (M8.23a / M8.23b); here we only confirm the canonical wire-name sets
    // are recorded and non-empty so coverage gaps are never silent.
    let platform = service(&v, "platform");
    assert_eq!(platform.service, "platform");
    assert_eq!(
        platform.methods.len(),
        31,
        "platform.* method count (14 §8)"
    );
    let avm = service(&v, "avm");
    assert_eq!(avm.service, "avm");
    assert_eq!(avm.methods.len(), 11, "avm.* method count (14 §9)");
    // No recorded calls here (driven from the VM crates' own tests).
    assert!(platform.calls.is_empty(), "platform calls driven elsewhere");
    assert!(avm.calls.is_empty(), "avm calls driven elsewhere");
}

// ===========================================================================
// error-response snapshots (14 §16.6) — driven through the real dispatch shim
// ===========================================================================

#[tokio::test]
async fn error_snapshots() {
    let v = vectors();
    let by_name: BTreeMap<&str, i32> = v.errors.iter().map(|e| (e.name.as_str(), e.code)).collect();

    let registry = {
        let mut reg = ServiceRegistry::new();
        info_fixture().register_rpc(&mut reg);
        Arc::new(reg)
    };

    // bad params -32602: `getBlockchainID` expects {alias: string}; an int fails.
    let body = post_rpc(
        Arc::clone(&registry),
        "info.getBlockchainID",
        &json!({ "alias": 123 }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], by_name["badParams"],
        "bad params -32602"
    );

    // unknown method -32601.
    let body = post_rpc(Arc::clone(&registry), "info.doesNotExist", &json!({})).await;
    assert_eq!(
        body["error"]["code"], by_name["unknownMethod"],
        "unknown method -32601"
    );

    // malformed JSON -32700 (raw body bypasses post_rpc's serializer).
    let router = Router::new()
        .route("/", post(dispatch))
        .with_state(Arc::clone(&registry));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(b"{ not json".to_vec()))
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body["error"]["code"], by_name["malformedJSON"],
        "malformed JSON -32700"
    );

    // invalid request -32600 (wrong protocol version).
    let wrong = json!({"jsonrpc": "1.0", "id": 1, "method": "info.getNodeID", "params": [{}]});
    let router = Router::new()
        .route("/", post(dispatch))
        .with_state(Arc::clone(&registry));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&wrong).unwrap()))
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(
        body["error"]["code"], by_name["invalidRequest"],
        "invalid request -32600"
    );

    // server error -32000 (a domain failure): isBootstrapped with empty chain.
    let body = post_rpc(
        Arc::clone(&registry),
        "info.isBootstrapped",
        &json!({ "chain": "" }),
    )
    .await;
    assert_eq!(
        body["error"]["code"], by_name["serverError"],
        "server error -32000"
    );

    // The EVM revert code 3 is a geth/reth wire concern (ava-evm), not the
    // gorilla json2 shim; the recorded snapshot pins the code so the Rust EVM
    // path can be checked against it in its own crate. Asserted as recorded.
    assert_eq!(by_name["evmRevert"], 3, "EVM revert code 3 (14 §16.6)");
}

// ===========================================================================
// HTTP semantics (14 §16.3) — node-id header, 403 allowed-hosts, per-chain 503
//
// `Server::build_router` is crate-private, so these drive the PUBLIC
// middleware (`ava_api::middleware::{node_id_header, allowed_hosts,
// not_bootstrapped}`) directly, composed in the same layer order
// `build_router` uses (node-id outermost so it wraps the 403 / 503
// short-circuits; server.rs:241-312). The composed-server wiring itself is
// covered by the in-crate server.rs unit tests.
// ===========================================================================

fn ctx_for(chain_id: Id, alias: &str) -> Arc<ConsensusContext> {
    let chain = Arc::new(ChainContext {
        network_id: 1,
        subnet_id: Id::EMPTY,
        chain_id,
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(1),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    });
    Arc::new(ConsensusContext::new(
        chain,
        alias.to_string(),
        Arc::new(NoOpAcceptor),
        Arc::new(NoOpAcceptor),
    ))
}

#[tokio::test]
async fn http_node_id_header_and_403() {
    use ava_api::middleware::{AllowedHosts, NODE_ID_HEADER, allowed_hosts, node_id_header};
    use axum::http::HeaderValue;
    use axum::routing::get;

    let node_id = NodeId::from([7u8; 20]);
    let node_id_value = HeaderValue::from_str(&node_id.to_string()).expect("header value");

    // node-id outermost; allowed-hosts inside it (build_router layer order).
    let make_router = |hosts: Vec<String>| {
        Router::new()
            .route("/ext/info", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                AllowedHosts::new(&hosts),
                allowed_hosts,
            ))
            .layer(axum::middleware::from_fn_with_state(
                node_id_value.clone(),
                node_id_header,
            ))
    };

    // node-id header on a normal (allowed) response.
    let router = make_router(vec!["*".to_string()]);
    let request = Request::builder()
        .uri("/ext/info")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "allowed host served");
    assert_eq!(
        resp.headers()
            .get(NODE_ID_HEADER)
            .and_then(|v| v.to_str().ok()),
        Some(node_id.to_string().as_str()),
        "node-id header present"
    );

    // 403 on a disallowed Host, with the node-id header still set (outermost).
    let router = make_router(vec!["localhost".to_string()]);
    let request = Request::builder()
        .uri("/ext/info")
        .header(header::HOST, "evil.example.com")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "403 disallowed host");
    assert!(
        resp.headers().get(NODE_ID_HEADER).is_some(),
        "node-id header on the 403 short-circuit"
    );
}

#[tokio::test]
async fn http_per_chain_503_before_bootstrap() {
    use ava_api::middleware::not_bootstrapped;
    use axum::routing::get;

    let chain_id = Id::from([9u8; 32]);
    let ctx = ctx_for(chain_id, "P"); // Initializing by default => not bootstrapped.

    // The per-chain 503 reject layer wraps the chain's route (server.rs:226).
    let router = Router::new().route("/", get(|| async { "ok" })).layer(
        axum::middleware::from_fn_with_state(Arc::clone(&ctx), not_bootstrapped),
    );

    let request = Request::builder()
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp = router.clone().oneshot(request).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "per-chain 503 before NormalOp"
    );

    // After NormalOp the 503 layer admits the request and the route serves 200.
    ctx.state.store(Arc::new(EngineState::NormalOp));
    let request = Request::builder()
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "served after NormalOp");
}

#[tokio::test]
async fn http_health_get_200_and_503() {
    // Healthy => 200.
    let health = health_fixture().await;
    let router = health_handler(health);
    let request = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "GET /ext/health healthy 200");

    // Unhealthy (a failing check) => 503.
    let health = Arc::new(Health::new(&prometheus::Registry::new()).expect("Health::new()"));
    let failing: Arc<dyn Checker> = Arc::new(|| -> BoxFuture<'static, CheckResult> {
        Box::pin(async move { Err(ava_api::health::CheckError::new("boom")) })
    });
    health
        .register_health_check("bad", failing, &[])
        .expect("register bad");
    health.run_checks_now().await;
    let router = health_handler(health);
    let request = Request::builder()
        .method(Method::GET)
        .uri("/")
        .body(Body::empty())
        .expect("request");
    let resp = router.oneshot(request).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "GET /ext/health unhealthy 503"
    );
}

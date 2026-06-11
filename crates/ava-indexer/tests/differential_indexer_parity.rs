// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Differential test against the recorded Go indexer oracle (M8.24).
//!
//! `vectors/indexer/indexer_parity.json` is emitted by the in-repo Go oracle
//! (`go-oracle/indexer_avalanche_rs_vectors_test.go`, run inside avalanchego
//! at the commit recorded in `goCommit`). It pins:
//!
//! - the persisted `Container` codec bytes (varied timestamps);
//! - the COMPLETE physical database state after a deterministic two-chain
//!   scenario (run 1) and after an indexing-disabled run that marks the chain
//!   incomplete (run 2) — covering the sha256 prefixdb namespacing, versiondb
//!   passthrough, `hasRun`/`previously-indexed`/`incomplete` marker keys, and
//!   every index record byte-for-byte;
//! - computed query replies (`FormattedContainer` JSON via the live JSON-RPC
//!   route) and every reachable index-level error string;
//! - the run-3 fatal: re-enabling indexing over an incomplete index with
//!   `index-allow-incomplete=false` closes the indexer.
//!
//! Timestamps are deterministic (the Go oracle pins `mockable.Clock`; the
//! replay pins `MockClock` to the same instant), so nothing is normalized —
//! the comparison is exact.

// Tests index into fixtures and `serde_json::Value` replies and do plain
// test-fixture arithmetic, both idiomatic in tests; an integration test
// target never uses every lib dep (precedent: ava-genesis golden tests).
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_database::memdb::MemDb;
use ava_database::{Database, Iteratee, Iterator as _, VersionDb};
use ava_indexer::{
    AcceptorGroup, Config, Container, ContainerIndexer, Index, IndexReader, Indexer as _,
    PathAdder, VmType, index_handler,
};
use ava_snow::acceptor::NoOpAcceptor;
use ava_snow::context::{ChainContext, ConsensusContext};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::MockClock;
use axum::body::Body;
use axum::http::{Request, header};
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Vector schema (mirrors the Go emitter's structs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    go_commit: String,
    clock_unix_nanos: i64,
    #[serde(rename = "chain1ID")]
    chain1_id: String,
    #[serde(rename = "chain2ID")]
    chain2_id: String,
    containers: HashMap<String, Vec<EmittedContainer>>,
    codec_vectors: Vec<CodecVector>,
    empty_index_errors: HashMap<String, String>,
    queries: Queries,
    #[serde(rename = "dbDumpAfterRun1")]
    db_dump_after_run1: Vec<EmittedKv>,
    #[serde(rename = "dbDumpAfterRun2")]
    db_dump_after_run2: Vec<EmittedKv>,
    run3_fatal_closed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmittedContainer {
    id: String,
    bytes_hex: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodecVector {
    id: String,
    bytes_hex: String,
    timestamp: i64,
    encoded_hex: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Queries {
    #[serde(rename = "lastAcceptedJSON")]
    last_accepted_json: Value,
    #[serde(rename = "containerByIndex1JSON")]
    container_by_index1_json: Value,
    #[serde(rename = "containerByID0JSON")]
    container_by_id0_json: Value,
    #[serde(rename = "range02JSON")]
    range02_json: Value,
    #[serde(rename = "rangeAllIDs")]
    range_all_ids: Vec<String>,
    range_err_too_many: String,
    range_err_zero: String,
    range_err_past_end: String,
    get_index_unknown_err: String,
    #[serde(rename = "unknownID")]
    unknown_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmittedKv {
    key_hex: String,
    value_hex: String,
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Default)]
struct NoopPathAdder;

impl PathAdder for NoopPathAdder {
    fn add_route(
        &self,
        _handler: ava_api::BoxedHandler,
        _base: &str,
        _endpoint: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}

struct Groups {
    block: Arc<AcceptorGroup>,
    tx: Arc<AcceptorGroup>,
    vertex: Arc<AcceptorGroup>,
}

fn config(
    db: Arc<VersionDb<MemDb>>,
    groups: &Groups,
    clock_nanos: i64,
    indexing_enabled: bool,
    allow_incomplete: bool,
) -> Config<VersionDb<MemDb>> {
    Config {
        db,
        indexing_enabled,
        allow_incomplete_index: allow_incomplete,
        block_acceptor_group: Arc::clone(&groups.block),
        tx_acceptor_group: Arc::clone(&groups.tx),
        vertex_acceptor_group: Arc::clone(&groups.vertex),
        path_adder: Arc::new(NoopPathAdder),
        shutdown_f: Arc::new(|| {}),
        clock: Arc::new(MockClock::at(
            UNIX_EPOCH + Duration::from_nanos(u64::try_from(clock_nanos).expect("nanos")),
        )),
    }
}

fn consensus_ctx(chain_id: Id, alias: &str) -> ConsensusContext {
    ConsensusContext::new(
        Arc::new(ChainContext {
            network_id: 1,
            subnet_id: PRIMARY_NETWORK_ID,
            chain_id,
            node_id: NodeId::default(),
            public_key: None,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        }),
        alias.to_string(),
        Arc::new(NoOpAcceptor),
        Arc::new(NoOpAcceptor),
    )
}

fn parse_id(s: &str) -> Id {
    s.parse().expect("cb58 id")
}

/// Polls until `id` is indexed (the acceptor path is async: broadcast +
/// `spawn_blocking`; 17 §2.2 #20). One consumer task per index applies
/// accepts in order, so the last id being visible implies all are.
async fn wait_indexed<D: Database + 'static>(index: &Index<D>, id: &Id) {
    for _ in 0..2500 {
        if index.get_index(id).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("container {id} was not indexed in time");
}

/// Drives the mounted JSON-RPC route end-to-end, returning `result`.
async fn rpc(index: Arc<dyn IndexReader>, method: &str, params: Value) -> Value {
    let request = Request::builder()
        .method("POST")
        .uri("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": [params],
            }))
            .expect("serialize"),
        ))
        .expect("request");
    let response = index_handler(index)
        .oneshot(request)
        .await
        .expect("oneshot");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        body.get("error").is_none(),
        "{method} returned an error: {body}"
    );
    body["result"].clone()
}

/// Dumps every physical key/value of the memdb as sorted hex pairs.
fn dump_db(db: &MemDb) -> Vec<(String, String)> {
    let mut iter = db.new_iterator_with_start_and_prefix(&[], &[]);
    let mut out = Vec::new();
    while iter.next() {
        out.push((
            hex::encode(iter.key().expect("key")),
            hex::encode(iter.value().expect("value")),
        ));
    }
    iter.error().expect("iterator error");
    out.sort();
    out
}

fn emitted_dump(dump: &[EmittedKv]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = dump
        .iter()
        .map(|kv| (kv.key_hex.clone(), kv.value_hex.clone()))
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The differential test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn indexer_parity() {
    let vectors: Vectors =
        serde_json::from_str(include_str!("vectors/indexer/indexer_parity.json"))
            .expect("parse vectors");
    assert!(!vectors.go_commit.is_empty(), "vector provenance recorded");

    // ---- codec vectors: persisted Container values are byte-exact --------
    for cv in &vectors.codec_vectors {
        let container = Container {
            id: parse_id(&cv.id),
            bytes: hex::decode(&cv.bytes_hex).expect("bytesHex"),
            timestamp: cv.timestamp,
        };
        let encoded = container.marshal().expect("Container::marshal()");
        assert_eq!(
            cv.encoded_hex,
            hex::encode(&encoded),
            "Container codec bytes (timestamp {})",
            cv.timestamp
        );
        assert_eq!(
            container,
            Container::unmarshal(&encoded).expect("Container::unmarshal()"),
            "Container codec round-trip"
        );
    }

    // ---- fixture identity: the fill-pattern chain ids round-trip cb58 ----
    let chain1 = Id::from([0xC1; 32]);
    let chain2 = Id::from([0xC2; 32]);
    assert_eq!(vectors.chain1_id, chain1.to_string(), "chain1 id cb58");
    assert_eq!(vectors.chain2_id, chain2.to_string(), "chain2 id cb58");

    let base = Arc::new(MemDb::new());
    let groups = Groups {
        block: Arc::new(AcceptorGroup::default()),
        tx: Arc::new(AcceptorGroup::default()),
        vertex: Arc::new(AcceptorGroup::default()),
    };

    // ---- run 1: index chain1 (Snowman) + chain2 (DAG) --------------------
    let db1 = Arc::new(VersionDb::new_arc(Arc::clone(&base)));
    let indexer = ContainerIndexer::new(config(
        Arc::clone(&db1),
        &groups,
        vectors.clock_unix_nanos,
        true,
        false,
    ))
    .expect("ContainerIndexer::new() run 1");

    indexer
        .register_chain("chain1", &consensus_ctx(chain1, "chain1"), VmType::Chain)
        .await;
    indexer
        .register_chain("chain2", &consensus_ctx(chain2, "chain2"), VmType::Dag)
        .await;

    // Empty-index error strings (chain2's block index, before any accept).
    let chain2_blk = indexer.block_index(&chain2).expect("chain2 block index");
    assert_eq!(
        vectors.empty_index_errors["getLastAccepted"],
        chain2_blk
            .get_last_accepted()
            .expect_err("empty")
            .to_string(),
        "empty getLastAccepted error"
    );
    assert_eq!(
        vectors.empty_index_errors["getContainerByIndex0"],
        chain2_blk
            .get_container_by_index(0)
            .expect_err("empty")
            .to_string(),
        "empty getContainerByIndex error"
    );
    assert_eq!(
        vectors.empty_index_errors["getContainerRange01"],
        chain2_blk
            .get_container_range(0, 1)
            .expect_err("empty")
            .to_string(),
        "empty getContainerRange error"
    );

    // Accepts, in the oracle's order; the per-index task applies in order.
    let accept = |group: &Arc<AcceptorGroup>, chain: &Id, key: &str| {
        let fixtures = &vectors.containers[key];
        for fixture in fixtures {
            group.accept(
                chain,
                parse_id(&fixture.id),
                &hex::decode(&fixture.bytes_hex).expect("bytesHex"),
            );
        }
        parse_id(&fixtures.last().expect("non-empty fixture").id)
    };
    let last_blk1 = accept(&groups.block, &chain1, "chain1Blocks");
    let last_blk2 = accept(&groups.block, &chain2, "chain2Blocks");
    let last_vtx2 = accept(&groups.vertex, &chain2, "chain2Vtxs");
    let last_tx2 = accept(&groups.tx, &chain2, "chain2Txs");

    let blk1 = indexer.block_index(&chain1).expect("chain1 block index");
    let vtx2 = indexer.vtx_index(&chain2).expect("chain2 vtx index");
    let tx2 = indexer.tx_index(&chain2).expect("chain2 tx index");
    wait_indexed(&blk1, &last_blk1).await;
    wait_indexed(&chain2_blk, &last_blk2).await;
    wait_indexed(&vtx2, &last_vtx2).await;
    wait_indexed(&tx2, &last_tx2).await;

    // ---- computed query replies over the live JSON-RPC route -------------
    let reader = || Arc::clone(&blk1) as Arc<dyn IndexReader>;
    assert_eq!(
        vectors.queries.last_accepted_json,
        rpc(
            reader(),
            "index.getLastAccepted",
            json!({"encoding": "hex"})
        )
        .await,
        "getLastAccepted reply JSON"
    );
    assert_eq!(
        vectors.queries.container_by_index1_json,
        rpc(
            reader(),
            "index.getContainerByIndex",
            json!({"index": "1", "encoding": "hex"})
        )
        .await,
        "getContainerByIndex(1) reply JSON"
    );
    assert_eq!(
        vectors.queries.container_by_id0_json,
        rpc(
            reader(),
            "index.getContainerByID",
            json!({"id": vectors.containers["chain1Blocks"][0].id, "encoding": "hex"})
        )
        .await,
        "getContainerByID reply JSON"
    );
    assert_eq!(
        vectors.queries.range02_json,
        rpc(
            reader(),
            "index.getContainerRange",
            json!({"startIndex": "0", "numToFetch": "2", "encoding": "hex"})
        )
        .await,
        "getContainerRange(0,2) reply JSON"
    );

    let all = blk1
        .get_container_range(0, ava_indexer::MAX_FETCHED_BY_RANGE)
        .expect("get_container_range(0, max)");
    assert_eq!(
        vectors.queries.range_all_ids,
        all.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
        "accept ordering of the full range"
    );

    assert_eq!(
        vectors.queries.range_err_too_many,
        blk1.get_container_range(0, ava_indexer::MAX_FETCHED_BY_RANGE + 1)
            .expect_err("too many")
            .to_string(),
        "range cap error string"
    );
    assert_eq!(
        vectors.queries.range_err_zero,
        blk1.get_container_range(0, 0)
            .expect_err("zero")
            .to_string(),
        "range zero error string"
    );
    assert_eq!(
        vectors.queries.range_err_past_end,
        blk1.get_container_range(9, 1)
            .expect_err("past end")
            .to_string(),
        "range start-bound error string"
    );
    assert_eq!(
        vectors.queries.get_index_unknown_err,
        blk1.get_index(&parse_id(&vectors.queries.unknown_id))
            .expect_err("unknown id")
            .to_string(),
        "getIndex unknown-id error string"
    );

    // ---- run 1 physical state is byte-identical to Go --------------------
    db1.commit().expect("VersionDb::commit() run 1");
    indexer.close().await.expect("close run 1");
    assert_eq!(
        emitted_dump(&vectors.db_dump_after_run1),
        dump_db(&base),
        "full DB dump after run 1"
    );

    // ---- run 2: indexing disabled, incomplete allowed -> marker ----------
    let db2 = Arc::new(VersionDb::new_arc(Arc::clone(&base)));
    let indexer = ContainerIndexer::new(config(
        Arc::clone(&db2),
        &groups,
        vectors.clock_unix_nanos,
        false,
        true,
    ))
    .expect("ContainerIndexer::new() run 2");
    indexer
        .register_chain("chain1", &consensus_ctx(chain1, "chain1"), VmType::Chain)
        .await;
    assert!(
        indexer.is_incomplete(&chain1).expect("is_incomplete()"),
        "chain1 marked incomplete in run 2"
    );
    db2.commit().expect("VersionDb::commit() run 2");
    indexer.close().await.expect("close run 2");
    assert_eq!(
        emitted_dump(&vectors.db_dump_after_run2),
        dump_db(&base),
        "full DB dump after run 2 (restart markers)"
    );

    // ---- run 3: re-enabling indexing over an incomplete index is fatal ---
    let db3 = Arc::new(VersionDb::new_arc(Arc::clone(&base)));
    let indexer = ContainerIndexer::new(config(
        Arc::clone(&db3),
        &groups,
        vectors.clock_unix_nanos,
        true,
        false,
    ))
    .expect("ContainerIndexer::new() run 3");
    indexer
        .register_chain("chain1", &consensus_ctx(chain1, "chain1"), VmType::Chain)
        .await;
    assert_eq!(
        vectors.run3_fatal_closed,
        indexer.is_closed(),
        "incomplete-index fatal on run 3"
    );
}

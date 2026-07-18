// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmVm` `ChainVm` adapter (M6.10, spec 10 §3; 07 ChainVm/Block).
//!
//! Exercises the engine-facing surface over a temp Firewood-ethhash provider +
//! in-memory `CanonicalStore`, reusing the committed `genesis_to_1` reexecute
//! fixture (the same Go-EXECUTED coreth oracle the M6.9 lifecycle tests use):
//!
//! * `parse_block` decodes wire bytes to an (unverified) `EvmBlock`-backed
//!   engine `Block` whose `id`/`height`/`bytes` round-trip.
//! * `get_block` returns a verified-tree block by id, reconstructs an accepted
//!   block from the `CanonicalStore`, and errors `NotFound` for an unknown id.
//! * `set_preference` records the preferred pointer with NO state mutation
//!   (record-only; Snowman owns fork choice, G6).
//! * `last_accepted` returns the seeded committed `(Id, height)` tip.

use std::str::FromStr;
use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_evm::block::{
    AvaBlockParts, AvaHeader, EvmBlock, EvmBlockContext, assemble_ava_block, decode_ava_evm_block,
};
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, AvaNextBlockCtx, NoopPreHook};
use ava_evm::feerules::{base_fee, parent_fee_state_of};
use ava_evm::precompile::rewardmanager::BLACKHOLE_ADDRESS;
use ava_evm::state::FirewoodStateProvider;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, B256, BundleState, EMPTY_OMMER_ROOT_HASH, EMPTY_ROOT_HASH, ExternalConsensusExecutor,
    Header, State, StateProviderDatabase, U256,
};
use ava_types::id::Id;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

#[derive(serde::Deserialize)]
struct AllocEntry {
    address: String,
    balance: String,
}

#[derive(serde::Deserialize)]
struct Fixture {
    chain_id: u64,
    alloc: Vec<AllocEntry>,
    genesis_state_root: String,
    genesis_base_fee: String,
    block1_rlp: String,
}

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
}

/// The synthetic AP3 genesis (height-0) coreth header the fixture's block-1 is a
/// child of — the parent [`EvmBlock::verify`] recomputes the contextual
/// `verifyHeaderGasFields` fee/gas fields against (mirrors `lifecycle.rs` /
/// `build.rs`). Also assembled into a genesis `EvmBlock` and seeded into the VM
/// (see [`EvmVm::seed_verified`]) so the full `parse → verify` path can resolve
/// the genesis parent's header.
fn genesis_header(fx: &Fixture, genesis_root: B256) -> AvaHeader {
    AvaHeader {
        parent_hash: B256::ZERO,
        uncle_hash: EMPTY_OMMER_ROOT_HASH,
        coinbase: Address::ZERO,
        state_root: genesis_root,
        tx_root: EMPTY_ROOT_HASH,
        receipt_root: EMPTY_ROOT_HASH,
        bloom: ava_evm_reth::Bytes::from(vec![0u8; 256]),
        difficulty: U256::ZERO,
        number: 0,
        gas_limit: 8_000_000,
        gas_used: 0,
        time: 0,
        extra: ava_evm_reth::Bytes::new(),
        mix_digest: B256::ZERO,
        nonce: [0u8; 8],
        ext_data_hash: ava_evm::block::empty_ext_data_hash(),
        base_fee: Some(U256::from(
            u64::from_str(&fx.genesis_base_fee).expect("genesis base fee"),
        )),
        ext_data_gas_used: None,
        block_gas_cost: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_root: None,
        time_milliseconds: None,
        min_delay_excess: None,
    }
}

/// `B256` block hash -> consensus `Id` (the block-id mapping the adapter uses).
fn id_of(hash: B256) -> Id {
    Id::from(<[u8; 32]>::from(hash))
}

/// The same AP3-from-genesis schedule the fixture was produced under.
fn ap3_chain_spec(chain_id: u64) -> AvaChainSpec {
    const FAR_FUTURE: u64 = u64::MAX;
    let upgrades = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR_FUTURE,
        apricot_phase_5: FAR_FUTURE,
        apricot_phase_pre_6: FAR_FUTURE,
        apricot_phase_6: FAR_FUTURE,
        apricot_phase_post_6: FAR_FUTURE,
        banff: FAR_FUTURE,
        cortina: FAR_FUTURE,
        durango: FAR_FUTURE,
        etna: FAR_FUTURE,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
        helicon: u64::MAX,
    };
    AvaChainSpec::from_parts(upgrades, ava_evm_reth::Chain::from_id(chain_id), false)
}

fn load_fixture() -> Fixture {
    let raw = include_str!("vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json");
    serde_json::from_str(raw).expect("parse fixture")
}

fn block1_bytes(fx: &Fixture) -> Vec<u8> {
    hex::decode(fx.block1_rlp.trim_start_matches("0x")).expect("block1 hex")
}

/// Opens a fresh Firewood db with the genesis alloc committed; returns the
/// provider, the EVM config, the canonical store, and the committed genesis
/// state root. The provider/config/store are shared (`Arc`) with the `EvmVm`.
fn setup(
    fx: &Fixture,
) -> (
    tempfile::TempDir,
    Arc<FirewoodStateProvider>,
    AvaEvmConfig,
    Arc<CanonicalStore>,
    B256,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    let mut builder = BundleState::builder(0..=0);
    for entry in &fx.alloc {
        let addr = Address::from_str(&entry.address).expect("alloc addr");
        let balance = U256::from_str_radix(&entry.balance, 10).expect("alloc balance");
        builder = builder.state_present_account_info(
            addr,
            ava_evm_reth::AccountInfo {
                balance,
                nonce: 0,
                ..Default::default()
            },
        );
    }
    let genesis_root = provider
        .propose_from_bundle(&builder.build())
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");
    assert_eq!(
        provider.root(),
        b256(&fx.genesis_state_root),
        "genesis state root parity vs coreth"
    );

    let canonical: Arc<CanonicalStore> = Arc::new(CanonicalStore::new(Arc::new(MemDb::new())));
    let config = AvaEvmConfig::new(ap3_chain_spec(fx.chain_id));
    (dir, provider, config, canonical, genesis_root)
}

/// Builds a block-1 whose header carries the post-state root our Firewood backend
/// actually produces (see `tests/lifecycle.rs` on the M6.30 parity dependency).
/// Returns the re-assembled block; the dry-run stash is dropped so a later
/// `verify` starts clean.
fn verifiable_block1(fx: &Fixture, ctx: &EvmBlockContext, parent_root: B256) -> EvmBlock {
    let decoded = decode_ava_evm_block(&block1_bytes(fx), ctx.chain_spec()).expect("decode block1");
    let txs = decoded.recover_senders().expect("recover");

    // See `lifecycle.rs::verifiable_block1`: the fixture's flat block-1 base fee
    // is a raw execution artifact, not the coreth-AP3 dynamic value the new
    // `verifyHeaderGasFields` check recomputes from the genesis parent; stamp the
    // honest value (state-neutral for the single legacy tx).
    let genesis = genesis_header(fx, parent_root);
    let h = decoded.header();
    let honest_base_fee = {
        let spec = ctx.chain_spec();
        let next = AvaNextBlockCtx {
            timestamp: h.time,
            timestamp_ms: h.time.saturating_mul(1000),
            parent_fee_state: parent_fee_state_of(spec, &genesis).expect("parent fee state"),
            ..AvaNextBlockCtx::default()
        };
        U256::from(base_fee(spec, &genesis, &next).expect("honest base fee"))
    };

    let env_header = Header {
        parent_hash: h.parent_hash,
        number: h.number,
        timestamp: h.time,
        gas_limit: h.gas_limit,
        gas_used: h.gas_used,
        base_fee_per_gas: Some(u64::try_from(honest_base_fee).expect("base fee fits")),
        // The dry-run beneficiary MUST match the coinbase the final header
        // is stamped with (BLACKHOLE_ADDRESS, below) so the fee credit lands on
        // the same account the real verify-time re-execution uses; otherwise the
        // dry-run root and the syntacticVerify-checked header disagree.
        beneficiary: BLACKHOLE_ADDRESS,
        ..Default::default()
    };
    let view = ctx
        .state()
        .history_by_state_root(parent_root)
        .expect("parent view");
    let mut state: State<StateProviderDatabase<_>> = ava_evm_reth::StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();
    let env = ctx.evm_config().evm_env_for_header(&env_header);
    let outcome = ctx
        .evm_config()
        .execute_batch(env, &mut state, &NoopPreHook, &txs)
        .expect("execute");
    let root = ctx
        .state()
        .propose_from_bundle(&outcome.bundle)
        .expect("propose");
    ctx.state().discard(root);

    // Re-assemble with the header's state root set to the produced root. The
    // fixture is a raw `core.GenerateChainWithGenesis` EVM-execution artifact
    // (state-root/fee-mechanics parity only — see `genesis_to_1.json`'s
    // `description`), not a block produced through `wrappedBlock`'s consensus
    // wrapping, so its `coinbase` is the zero address; stamp the blackhole
    // address `syntacticVerify`'s coinbase check (M9.15 task L1) now requires.
    let mut parts = decoded.into_parts();
    parts.header.state_root = root;
    parts.header.coinbase = BLACKHOLE_ADDRESS;
    parts.header.base_fee = Some(honest_base_fee);
    assemble_ava_block(parts, ctx.chain_spec()).expect("assemble")
}

/// Assembles the synthetic genesis (height-0) [`EvmBlock`] to seed into the VM's
/// processing tree, so `verify`'s parent-header read resolves the genesis parent
/// (the equivalent of what [`EvmVm::from_genesis`] seeds internally).
fn genesis_block(fx: &Fixture, ctx: &EvmBlockContext, genesis_root: B256) -> EvmBlock {
    assemble_ava_block(
        AvaBlockParts {
            header: genesis_header(fx, genesis_root),
            transactions: Vec::new(),
            atomic_txs: Vec::new(),
            ext_data: Vec::new(),
            version: 0,
        },
        ctx.chain_spec(),
    )
    .expect("assemble genesis block")
}

#[tokio::test]
async fn parse_get_setpref_lastaccepted() {
    let fx = load_fixture();
    let (_dir, provider, config, canonical, genesis_root) = setup(&fx);

    // A verifiable block-1, with the genesis block recorded as last-accepted.
    let lifecycle_ctx = EvmBlockContext::new(
        Arc::clone(&provider),
        config.clone(),
        Arc::clone(&canonical),
    );
    let block1 = verifiable_block1(&fx, &lifecycle_ctx, genesis_root);
    let block1_bytes = block1.encoded_bytes().to_vec();
    let block1_hash = block1.hash();
    let block1_id = id_of(block1_hash);
    // The genesis parent block, assembled so `verify`'s parent-header read
    // (`verifyHeaderGasFields`) can resolve block-1's parent.
    let genesis = genesis_block(&fx, &lifecycle_ctx, genesis_root);

    // Seed the VM with genesis (height 0) as the committed last-accepted tip.
    let genesis_hash = *block1.parent_hash();
    let genesis_id = id_of(genesis_hash);
    let vm = EvmVm::new(provider, config, canonical, genesis_id);
    // Seed the genesis block into the processing tree under its consensus id, as
    // `EvmVm::from_genesis` does internally — `EvmVm::new` only records the tip
    // pointer, so `verify`'s parent-header resolution would otherwise miss it.
    vm.seed_verified(genesis_id, genesis, genesis_root);
    let token = CancellationToken::new();

    // ---- last_accepted: the seeded genesis tip ----
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, genesis_id, "last_accepted seeds from the genesis tip");

    // ---- parse_block: wire bytes -> EvmBlock, round-trips id/height/bytes ----
    let parsed = vm.parse_block(&token, &block1_bytes).await.expect("parse");
    assert_eq!(parsed.id(), block1_id, "parsed id == keccak(header)");
    assert_eq!(parsed.height(), block1.number());
    assert_eq!(parsed.parent(), genesis_id, "parent id mapping");
    assert_eq!(parsed.bytes(), block1_bytes.as_slice(), "bytes round-trip");

    // ---- get_block: verified tree hit ----
    // verify the parsed block so it lands in the verified DashMap, then fetch it.
    parsed.verify(&token).await.expect("verify block1");
    let got = vm.get_block(&token, block1_id).await.expect("get verified");
    assert_eq!(got.id(), block1_id);
    assert_eq!(got.height(), block1.number());

    // ---- get_block: unknown id => NotFound ----
    let unknown = Id::from([0xAB; 32]);
    assert!(
        matches!(
            vm.get_block(&token, unknown).await,
            Err(ava_vm::error::Error::NotFound)
        ),
        "unknown id => NotFound"
    );

    // ---- set_preference: record-only, NO state mutation ----
    let mut vm = vm;
    let root_before = vm.state_root();
    vm.set_preference(&token, block1_id)
        .await
        .expect("set_preference");
    assert_eq!(vm.preferred(), block1_id, "preferred pointer recorded");
    assert_eq!(
        vm.state_root(),
        root_before,
        "set_preference does not mutate state (G6 record-only)"
    );

    // ---- accept then get_block reconstructs from the CanonicalStore ----
    got.accept(&token).await.expect("accept block1");
    let last = vm.last_accepted(&token).await.expect("last_accepted");
    assert_eq!(last, block1_id, "last_accepted advanced to block1");
    let from_store = vm
        .get_block(&token, block1_id)
        .await
        .expect("get from canonical store");
    assert_eq!(from_store.id(), block1_id);
    assert_eq!(from_store.height(), block1.number());
}

/// M8.22: `create_handlers` returns REAL in-process handlers under coreth's
/// extension set ("/rpc", "/ws", "/avax", "/admin" — coreth `vm.go:1029-1075`
/// + `atomic/vm/vm.go:337-355`) and `new_http_handler` mirrors coreth's
/// `(nil, nil)` (`vm.go:1079-1081`). Each mount answers end-to-end through
/// the buffered `VmHttpService` seam.
#[tokio::test]
async fn create_handlers_serve_real_rpc() {
    use ava_vm::vm::{Vm, VmRequest};
    use serde_json::{Value, json};

    async fn post(handler: &ava_vm::vm::HttpHandler, uri: &str, body: Value) -> Value {
        let svc = handler.service.as_ref().expect("in-process service");
        let resp = svc
            .serve_http(VmRequest {
                method: "POST".to_string(),
                uri: uri.to_string(),
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: serde_json::to_vec(&body).expect("serialize"),
            })
            .await;
        assert_eq!(resp.status, 200, "JSON-RPC replies are HTTP 200 ({uri})");
        serde_json::from_slice(&resp.body).expect("json body")
    }

    let fx = load_fixture();
    let (_dir, provider, config, canonical, _genesis_root) = setup(&fx);
    let genesis_id = Id::from([0x42; 32]);
    let mut vm = EvmVm::new(provider, config, canonical, genesis_id);
    let token = CancellationToken::new();

    let handlers = vm.create_handlers(&token).await.expect("create_handlers");
    let mut extensions: Vec<&str> = handlers.keys().map(String::as_str).collect();
    extensions.sort_unstable();
    assert_eq!(
        extensions,
        ["/admin", "/avax", "/rpc", "/ws"],
        "coreth extension set (vm.go:1029-1075 + atomic/vm/vm.go:337-355)"
    );

    // "/rpc": the Ethereum JSON-RPC envelope reaches the real EthRpc bodies.
    let rpc = handlers.get("/rpc").expect("/rpc handler");
    let body = post(
        rpc,
        "/ext/bc/C/rpc",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": [] }),
    )
    .await;
    assert_eq!(
        body["result"],
        format!("0x{:x}", fx.chain_id),
        "eth_chainId over /rpc"
    );
    let body = post(
        rpc,
        "/ext/bc/C/rpc",
        json!({ "jsonrpc": "2.0", "id": 2, "method": "eth_blockNumber", "params": [] }),
    )
    .await;
    assert_eq!(body["result"], "0x0", "eth_blockNumber over /rpc");

    // "/ws": the same dispatch (the node's WS adapter bridges frames as
    // buffered POSTs; coreth serves the same rpc.Server on both mounts).
    let ws = handlers.get("/ws").expect("/ws handler");
    let body = post(
        ws,
        "/ext/bc/C/ws",
        json!({ "jsonrpc": "2.0", "id": 3, "method": "eth_chainId", "params": [] }),
    )
    .await;
    assert_eq!(
        body["result"],
        format!("0x{:x}", fx.chain_id),
        "eth_chainId over /ws"
    );

    // "/avax": the gorilla envelope reaches the real AvaxRpc bodies.
    let avax = handlers.get("/avax").expect("/avax handler");
    let unknown_tx = Id::from([7u8; 32]);
    let body = post(
        avax,
        "/ext/bc/C/avax",
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "avax.getAtomicTxStatus",
            "params": [{ "txID": unknown_tx.to_string() }],
        }),
    )
    .await;
    assert_eq!(
        body["result"]["status"], "Unknown",
        "avax.getAtomicTxStatus over /avax"
    );

    // "/admin": the gorilla envelope reaches the real AdminRpc bodies.
    let admin = handlers.get("/admin").expect("/admin handler");
    let body = post(
        admin,
        "/ext/bc/C/admin",
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "admin.startCPUProfiler",
            "params": [{}],
        }),
    )
    .await;
    assert_eq!(
        body["result"],
        json!({}),
        "admin.startCPUProfiler over /admin"
    );

    // NewHTTPHandler: coreth returns (nil, nil) at this pin (vm.go:1079-1081).
    let header_handler = vm.new_http_handler(&token).await.expect("new_http_handler");
    assert!(
        header_handler.is_none(),
        "coreth NewHTTPHandler returns nil at this pin"
    );
}

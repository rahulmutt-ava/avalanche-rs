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
use ava_evm::block::{EvmBlock, EvmBlockContext, assemble_ava_block, decode_ava_evm_block};
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, NoopPreHook};
use ava_evm::state::FirewoodStateProvider;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, B256, BundleState, ExternalConsensusExecutor, Header, State, StateProviderDatabase,
    U256,
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
    block1_rlp: String,
}

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
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

    let h = decoded.header();
    let env_header = Header {
        parent_hash: h.parent_hash,
        number: h.number,
        timestamp: h.time,
        gas_limit: h.gas_limit,
        gas_used: h.gas_used,
        base_fee_per_gas: h
            .base_fee
            .map(|bf| u64::try_from(bf).expect("base fee fits")),
        beneficiary: h.coinbase,
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

    let mut parts = decoded.into_parts();
    parts.header.state_root = root;
    assemble_ava_block(parts, ctx.chain_spec()).expect("assemble")
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

    // Seed the VM with genesis (height 0) as the committed last-accepted tip.
    let genesis_hash = *block1.parent_hash();
    let genesis_id = id_of(genesis_hash);
    let vm = EvmVm::new(provider, config, canonical, genesis_id);
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

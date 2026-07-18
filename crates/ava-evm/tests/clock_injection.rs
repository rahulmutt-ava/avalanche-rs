// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmVm` build_block clock injection (specs/24 hazard #5).
//!
//! `build_block` previously read `SystemTime::now()` for the next-block header
//! timestamp, which becomes consensus state (the EVM block header `time`). This
//! test pins an injectable [`MockClock`] strictly above the parent header time
//! (so the `.max(parent_header.time + 1)` clamp does not mask it) and asserts the
//! produced block's header time equals the clock's `unix()` seconds — proving
//! `build_block` reads `self.clock`, not the wall clock.

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_avm::txs::components::{Input as FxInput, TransferableInput};
use ava_database::{DynDatabase, MemDb};
use ava_evm::atomic::tx::{AtomicTx, EvmOutput, Tx, UnsignedImportTx};
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
use ava_secp256k1fx::TransferInput;
use ava_types::id::Id;
use ava_utils::clock::{Clock, MockClock};
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

/// The synthetic AP3 genesis (height-0) coreth header the fixture's block-1 is a
/// child of, and the genesis `EvmBlock` seeded into the VM so `verify`'s
/// parent-header read (`verifyHeaderGasFields`) resolves it (mirrors
/// `chainvm.rs` / `lifecycle.rs`).
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

/// The synthetic genesis (height-0) [`EvmBlock`] seeded into the VM's processing
/// tree via [`EvmVm::seed_verified`] (what [`EvmVm::from_genesis`] seeds
/// internally; `EvmVm::new` only records the tip pointer).
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

fn b256(s: &str) -> B256 {
    B256::from_str(s).expect("b256")
}

fn id_of(hash: B256) -> Id {
    Id::from(<[u8; 32]>::from(hash))
}

fn load_fixture() -> Fixture {
    let raw = include_str!("vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json");
    serde_json::from_str(raw).expect("parse fixture")
}

fn block1_bytes(fx: &Fixture) -> Vec<u8> {
    hex::decode(fx.block1_rlp.trim_start_matches("0x")).expect("block1 hex")
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

/// Opens a fresh Firewood db with the genesis alloc committed; returns the
/// provider, the EVM config, the canonical store, and the committed genesis
/// state root.
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
/// actually produces (mirrors `tests/chainvm.rs::verifiable_block1`).
fn verifiable_block1(fx: &Fixture, ctx: &EvmBlockContext, parent_root: B256) -> EvmBlock {
    let decoded = decode_ava_evm_block(&block1_bytes(fx), ctx.chain_spec()).expect("decode block1");
    let txs = decoded.recover_senders().expect("recover");

    // See `lifecycle.rs::verifiable_block1`: stamp the coreth-honest base fee
    // recomputed from the genesis parent (the fixture's flat value is a raw
    // execution artifact the new `verifyHeaderGasFields` check rejects;
    // state-neutral for the single legacy tx).
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

/// A structurally-valid atomic import that credits the EVM (the credit path
/// ignores the asset id; the VM mempool keys gas by its configured asset, which
/// is `Id::EMPTY`). Seeding it gives `build_block` a non-empty batch to pack so
/// it reaches the header-stamp step instead of returning `ErrNoPendingBlock`.
fn import_tx() -> Tx {
    let unsigned = UnsignedImportTx {
        network_id: 1,
        blockchain_id: Id::from([0x11; 32]),
        source_chain: Id::from([0x22; 32]),
        imported_inputs: vec![TransferableInput {
            tx_id: Id::from([0x44; 32]),
            output_index: 1,
            asset_id: Id::EMPTY,
            r#in: FxInput::SecpTransfer(TransferInput::new(5_000, vec![0])),
        }],
        outs: vec![EvmOutput {
            address: [0x01; 20],
            amount: 4_999,
            asset_id: Id::EMPTY,
        }],
    };
    let mut tx = Tx::new(AtomicTx::Import(unsigned));
    tx.initialize().expect("initialize import");
    tx
}

/// `build_block` stamps the next header time from the injected clock, not the
/// wall clock (specs/24 hazard #5). Pin the clock strictly above
/// `parent_header.time + 1` so the clamp can't mask a wall-clock read.
#[tokio::test]
async fn build_block_uses_injected_clock() {
    let fx = load_fixture();
    let (_dir, provider, config, canonical, genesis_root) = setup(&fx);

    // A verifiable block-1 (header time == 10 in the fixture); genesis is the
    // recorded last-accepted tip.
    let ctx = EvmBlockContext::new(
        Arc::clone(&provider),
        config.clone(),
        Arc::clone(&canonical),
    );
    let block1 = verifiable_block1(&fx, &ctx, genesis_root);
    let block1_bytes = block1.encoded_bytes().to_vec();
    let block1_id = id_of(block1.hash());
    let parent_time = block1.header().time;
    // The genesis parent block, for `verify`'s parent-header resolution.
    let genesis = genesis_block(&fx, &ctx, genesis_root);

    let genesis_id = id_of(*block1.parent_hash());
    let mut vm = EvmVm::new(provider, config, canonical, genesis_id);
    // Seed genesis into the processing tree (as `EvmVm::from_genesis` does), so
    // `verify`'s `verifyHeaderGasFields` parent-header read resolves it.
    vm.seed_verified(genesis_id, genesis, genesis_root);
    let token = CancellationToken::new();

    // Parse, verify, then ACCEPT block1 so its post-state is the committed
    // Firewood tip (build_block resolves the preferred parent's state root as a
    // committed revision). Accepted blocks stay in the verified tree, so
    // build_block can build on the preferred leaf.
    let parsed = vm.parse_block(&token, &block1_bytes).await.expect("parse");
    parsed.verify(&token).await.expect("verify block1");
    parsed.accept(&token).await.expect("accept block1");
    vm.set_preference(&token, block1_id)
        .await
        .expect("set_preference");

    // Seed an atomic batch so build_block has something to pack (it contributes
    // no EVM txs itself — that integration is deferred). Without this the builder
    // returns ErrNoPendingBlock before the header is stamped.
    vm.mempool_handle()
        .lock()
        .add(import_tx())
        .expect("seed atomic import");

    // Pin the clock well above parent_time + 1 so the `.max()` clamp does not
    // mask a wall-clock read: the produced header time MUST equal pinned_secs.
    let pinned_secs = parent_time + 1_000;
    let clock = MockClock::at(UNIX_EPOCH + Duration::from_secs(pinned_secs));
    let mut vm = vm.with_clock(Arc::new(clock.clone()));
    assert!(
        pinned_secs > parent_time + 1,
        "pinned time must exceed the parent+1 clamp to be observable"
    );

    let built = vm.build_block(&token).await.expect("build_block");
    let got_secs = built
        .timestamp()
        .duration_since(UNIX_EPOCH)
        .expect("post-epoch")
        .as_secs();
    assert_eq!(
        got_secs,
        clock.unix(),
        "build_block header time comes from the injected clock"
    );
    assert_eq!(got_secs, pinned_secs, "header time == pinned clock seconds");
}

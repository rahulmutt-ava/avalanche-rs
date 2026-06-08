// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `EvmBlock` lifecycle (verify / accept / reject) + `CanonicalStore` (M6.9,
//! spec 10 §3.1/§3.2/§17.7, 06 linear acceptance).
//!
//! Driven off the committed `genesis_to_1` reexecute fixture (the same Go-EXECUTED
//! coreth oracle `cchain_state_root` uses): materialize the genesis alloc into a
//! fresh Firewood-ethhash db, build a single-transfer block-1, then exercise the
//! spec-06 lifecycle:
//!
//! * `verify` computes the Firewood **pre-commit root** (== `header.state_root`)
//!   via a stashed proposal and does NOT advance the EVM tip.
//! * `accept` commits that proposal (advances the tip), appends the block to the
//!   `CanonicalStore`, and advances `LAST_CANONICAL`.
//! * `reject` drops the proposal without committing.
//!
//! ## Why we re-assemble the block header (M6.30 dependency)
//!
//! The committed coreth block-1 header carries the **5-field libevm** post-state
//! root (`coreth_post_state_root_5field`), but our Firewood-ethhash backend does
//! not yet reproduce that exact value for block 1 (the open M6.30 state-root
//! parity gap — `cchain_state_root` asserts against our computed
//! `expected_post_state_root` as the recorded oracle, not the coreth header).
//! Since M6.9 tests the **lifecycle mechanics** (propose/stash → commit/discard →
//! canonical append), not state-root parity, we decode the real coreth block-1
//! body, execute it to learn the root our backend produces, and re-assemble the
//! block with `header.state_root` set to that value. `verify` then passes its
//! root-equality check and the commit/discard/canonical paths are exercised
//! end-to-end. When M6.30 closes the parity gap, the raw coreth bytes verify
//! directly (asserted then by `cchain_state_root` parity).

use std::str::FromStr;
use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_evm::block::{EvmBlock, EvmBlockContext, assemble_ava_block, decode_ava_evm_block};
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, NoopPreHook};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, ExternalConsensusExecutor, Header, State, StateProviderDatabase,
    U256,
};

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

/// The same AP3-from-genesis schedule the fixture was produced under
/// (`TestApricotPhase3Config`: AP1..AP3 active, AP4+ far-future ⇒ revm LONDON).
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

/// Opens a fresh Firewood db with the genesis alloc committed, plus the
/// `EvmBlockContext` (provider + config + canonical store) and the committed
/// genesis state root.
fn setup(fx: &Fixture) -> (tempfile::TempDir, EvmBlockContext, B256) {
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
    let ctx = EvmBlockContext::new(provider, config, canonical);
    (dir, ctx, genesis_root)
}

/// Builds a block-1 whose header carries the post-state root our Firewood backend
/// actually produces (see the module note on the M6.30 dependency). Returns the
/// re-assembled block; the stash created by the dry-run is dropped so `verify`
/// starts clean.
fn verifiable_block1(fx: &Fixture, ctx: &EvmBlockContext, parent_root: B256) -> EvmBlock {
    let decoded = decode_ava_evm_block(&block1_bytes(fx), ctx.chain_spec()).expect("decode block1");
    let txs = decoded.recover_senders().expect("recover");

    // Dry-run execute to learn the executor's post-state root.
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
    // Drop the dry-run stash so verify re-stashes cleanly.
    ctx.state().discard(root);

    // Re-assemble with the header's state root set to the produced root.
    let mut parts = decoded.into_parts();
    parts.header.state_root = root;
    assemble_ava_block(parts, ctx.chain_spec()).expect("assemble")
}

#[test]
fn verify_computes_precommit_root_no_commit() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);

    let tip_before = ctx.state().root();
    let precommit = block.verify(&ctx, genesis_root).expect("verify");

    // The pre-commit root is the header's state root.
    assert_eq!(precommit, *block.header_state_root());
    // verify does NOT advance the committed Firewood tip.
    assert_eq!(ctx.state().root(), tip_before);
    assert_eq!(ctx.state().root(), genesis_root);
}

#[test]
fn accept_commits_and_advances_tip() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);

    let precommit = block.verify(&ctx, genesis_root).expect("verify");
    assert_eq!(
        ctx.state().root(),
        genesis_root,
        "tip unchanged after verify"
    );

    block.accept(&ctx, precommit).expect("accept");
    // Firewood tip advanced to the post-block-1 root.
    assert_eq!(ctx.state().root(), precommit);
    // Canonical store advanced by one and points at this block.
    assert_eq!(
        ctx.canonical().last_canonical().expect("tip"),
        Some(block.number())
    );
    assert_eq!(
        ctx.canonical()
            .canonical_hash(block.number())
            .expect("hash"),
        Some(block.hash())
    );
}

#[test]
fn reject_drops_proposal_without_commit() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);

    // Two verifies of the same parent (sibling/idempotent proposals). The contract
    // is that rejecting does not disturb the committed tip (proposal-on-proposal,
    // 04 §4.2).
    let precommit = block.verify(&ctx, genesis_root).expect("verify A");
    let precommit2 = block.verify(&ctx, genesis_root).expect("verify B");
    assert_eq!(
        precommit, precommit2,
        "same parent+txs => same precommit root"
    );

    // Reject: drop the stashed proposal. Tip stays at genesis; nothing canonical.
    block.reject(&ctx, precommit).expect("reject");
    assert_eq!(ctx.state().root(), genesis_root, "reject commits nothing");
    assert_eq!(ctx.canonical().last_canonical().expect("tip"), None);

    // Accept after reject must fail: the proposal was dropped.
    assert!(block.accept(&ctx, precommit).is_err());
}

#[test]
fn canonical_store_advances_by_one() {
    let store = CanonicalStore::new(Arc::new(MemDb::new()));
    assert_eq!(store.last_canonical().expect("tip"), None);

    // Append three synthetic blocks (height 1..=3). The store only writes
    // non-state block metadata (header/body/receipt/index) + the tip pointer.
    for n in 1u64..=3 {
        let hash = B256::repeat_byte(u8::try_from(n).expect("fits"));
        store
            .append_canonical(
                n,
                hash,
                B256::repeat_byte(0x10 + u8::try_from(n).expect("fits")),
                &[1, 2, 3],
                &[4, 5],
            )
            .expect("append");
        assert_eq!(store.last_canonical().expect("tip"), Some(n));
        assert_eq!(store.canonical_hash(n).expect("hash"), Some(hash));
        assert_eq!(store.height_of(hash).expect("num"), Some(n));
    }

    // Out-of-order / gap append is rejected (linear, strictly +1).
    let bad = store.append_canonical(5, B256::repeat_byte(0x99), B256::ZERO, &[], &[]);
    assert!(bad.is_err(), "non-+1 append must be rejected");
}

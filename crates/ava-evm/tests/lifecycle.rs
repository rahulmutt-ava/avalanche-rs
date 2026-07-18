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
use ava_evm::precompile::rewardmanager::BLACKHOLE_ADDRESS;
use ava_evm::receipts::decode_block_receipts;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, EthReceipt, ExternalConsensusExecutor, Header, State,
    StateProviderDatabase, TxHashRef, TxType, U256,
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
    // Drop the dry-run stash so verify re-stashes cleanly.
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

/// cchain-tx-pipeline task 3 (verify-time stash, accept-time persist +
/// `AcceptedTxIndex`): `accept_commits_and_advances_tip` above only checks the
/// tip pointer; this asserts the receipts payload itself — persisted bytes
/// decode to the fixture's tx count, `tx_number` resolves the fixture tx to
/// this height, and the `AcceptedTxIndex` record carries real (not placeholder)
/// gas/price/sender values.
#[test]
fn accept_persists_receipts_and_indexes_txs() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);

    let txs = block.recover_senders().expect("recover senders");
    assert!(!txs.is_empty(), "fixture block1 carries >= 1 EVM tx");
    let tx_hash = *txs[0].tx_hash();
    let expected_from = txs[0].signer();

    let precommit = block.verify(&ctx, genesis_root).expect("verify");
    block.accept(&ctx, precommit).expect("accept");

    // (a) receipts bytes persisted in the canonical store, non-empty, and
    // decode back to exactly the fixture's tx count.
    let receipts_bytes = ctx
        .canonical()
        .receipts_at(block.number())
        .expect("receipts_at")
        .expect("receipts row present after accept");
    assert!(!receipts_bytes.is_empty(), "persisted receipts non-empty");
    let decoded = decode_block_receipts(&receipts_bytes).expect("decode_block_receipts");
    assert_eq!(
        decoded.len(),
        txs.len(),
        "decoded receipt count matches the block's tx count"
    );

    // (b) tx_hash -> block number row.
    assert_eq!(
        ctx.canonical().tx_number(tx_hash).expect("tx_number"),
        Some(block.number()),
        "tx_number resolves to this block's height"
    );

    // (c) AcceptedTxIndex carries a real record, not placeholder zeros.
    let record = ctx
        .accepted_tx_index()
        .get(&tx_hash)
        .expect("accepted tx index record present");
    assert_eq!(record.block_number, block.number());
    assert_eq!(record.block_hash, block.hash());
    assert_eq!(record.from, expected_from);
    assert!(
        record.gas_used > 0,
        "gas_used > 0, have {}",
        record.gas_used
    );
    assert!(record.success, "the fixture's transfer tx succeeds");
    assert!(
        record.cumulative_gas_used >= record.gas_used,
        "cumulative ({}) >= own gas_used ({})",
        record.cumulative_gas_used,
        record.gas_used
    );
    let base_fee = block
        .header()
        .base_fee
        .map(|bf| u64::try_from(bf).expect("base fee fits"))
        .expect("AP3-active block carries a base fee");
    assert!(
        record.effective_gas_price >= u128::from(base_fee),
        "effective_gas_price ({}) >= base_fee ({base_fee})",
        record.effective_gas_price
    );
    // `first_log_index` (cchain-tx-pipeline task 4 follow-up, go-ethereum
    // `DeriveFields` block-wide logIndex semantics): this fixture's block1
    // carries exactly one EVM tx, so its first_log_index is trivially 0 (no
    // earlier tx in the block to have emitted logs). The cross-tx running-sum
    // accumulation itself is unit-tested directly in
    // `receipts::tests::first_log_index_accumulates_across_txs_block_wide`
    // (no multi-tx fixture is available here).
    assert_eq!(
        record.first_log_index, 0,
        "the block's only (first) tx has no earlier tx's logs to offset past"
    );
}

/// cchain-tx-pipeline task 3, I1 review fix: a post-append indexing failure
/// (here, a corrupted verify-time stash whose receipt count disagrees with
/// the block's real tx count) must NEVER fail `accept` — the block is already
/// durably committed (state + canonical tip) by the time indexing runs.
/// `accept` must log the inconsistency and return `Ok(())`, leaving the
/// `tx_number`/`AcceptedTxIndex` rows for this block simply absent rather than
/// half-written or propagating an error to the caller.
#[test]
fn accept_never_fails_on_receipt_tx_count_mismatch() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);
    let real_txs = block.recover_senders().expect("recover senders");
    assert_eq!(
        real_txs.len(),
        1,
        "fixture block1 carries exactly one EVM tx"
    );
    let tx_hash = *real_txs[0].tx_hash();

    let precommit = block.verify(&ctx, genesis_root).expect("verify");

    // Corrupt the verify-time stash `verify` just wrote: replace it with TWO
    // receipts for a block that only has ONE tx. `stash_receipts_for_test` is
    // the `#[doc(hidden)]` test-only seam added for exactly this — production
    // code only ever reaches the stash through `EvmBlock::verify`.
    let corrupted = vec![
        EthReceipt {
            tx_type: TxType::Legacy,
            success: true,
            cumulative_gas_used: 21_000,
            logs: Vec::new(),
        },
        EthReceipt {
            tx_type: TxType::Legacy,
            success: true,
            cumulative_gas_used: 42_000,
            logs: Vec::new(),
        },
    ];
    ctx.stash_receipts_for_test(precommit, corrupted);

    // `accept` must still succeed: the length mismatch is caught inside
    // `index_accepted_receipts`, logged via `tracing::warn!`, and swallowed —
    // never propagated.
    block
        .accept(&ctx, precommit)
        .expect("accept must not fail on a corrupted/mismatched receipt stash");

    // The block IS durably accepted (tip advanced) despite the indexing
    // failure.
    assert_eq!(
        ctx.canonical().last_canonical().expect("tip"),
        Some(block.number())
    );
    // `accept` persists whatever was stashed without second-guessing it — the
    // (corrupted) 2-receipt list is what got encoded.
    let receipts_bytes = ctx
        .canonical()
        .receipts_at(block.number())
        .expect("receipts_at")
        .expect("receipts row present");
    let decoded = decode_block_receipts(&receipts_bytes).expect("decode_block_receipts");
    assert_eq!(
        decoded.len(),
        2,
        "the corrupted stash's own 2 receipts persist as-is"
    );

    // Indexing was skipped because of the mismatch — no `tx_number` row and
    // no `AcceptedTxIndex` entry for the block's real (single) tx.
    assert_eq!(
        ctx.canonical().tx_number(tx_hash).expect("tx_number"),
        None,
        "tx_number indexing was skipped on the mismatch"
    );
    assert_eq!(
        ctx.accepted_tx_index().get(&tx_hash),
        None,
        "AcceptedTxIndex indexing was skipped on the mismatch"
    );
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

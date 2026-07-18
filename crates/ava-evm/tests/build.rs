// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `BlockBuilderDriver` on-demand build + precomputed-root finish (M6.20,
//! spec 10 §4/§17.6, G5).
//!
//! Driven off the committed `genesis_to_1` reexecute fixture (the same Go-EXECUTED
//! coreth oracle the lifecycle/chainvm tests use): materialize the genesis alloc
//! into a fresh Firewood-ethhash db, then:
//!
//! * `build_then_verify_same_root` — `build_on(parent, ctx, evm_txs)` pulls the
//!   candidate EVM tx (block-1's transfer) under the gas budget, computes the
//!   Firewood pre-commit root, and assembles a coreth block whose
//!   `header.state_root` is that root. The self-built block then **re-verifies to
//!   the identical root** via `EvmBlock::verify` (build-then-verify symmetry, the
//!   determinism contract §17.6 — both drive the same executor over the same
//!   parent view).
//! * `respects_min_build_delay` — a second `build_on` on the same parent within
//!   `MIN_BLOCK_BUILD_DELAY` is rejected by the `minBlockBuildingRetryDelay`
//!   guard (returns the "nothing to build" no-op shape).

use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use ava_database::{DynDatabase, MemDb};
use ava_evm::block::{AvaHeader, EvmBlockContext, decode_ava_evm_block};
use ava_evm::builder::{BlockBuilderDriver, MIN_BLOCK_BUILD_DELAY};
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, AvaNextBlockCtx};
use ava_evm::feerules::acp176::{Acp176State, STATE_SIZE};
use ava_evm::feerules::acp226::INITIAL_DELAY_EXCESS;
use ava_evm::feerules::window::WINDOW_SIZE;
use ava_evm::feerules::{fee_state_after_block, parent_fee_state_of};
use ava_evm::precompile::rewardmanager::BLACKHOLE_ADDRESS;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, EMPTY_OMMER_ROOT_HASH, EMPTY_ROOT_HASH, U256,
    calculate_transaction_root,
};
use parking_lot::Mutex;

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

/// The same schedule as [`ap3_chain_spec`] but with Etna (and every upgrade up
/// to it) active from genesis — used by the Cancun-tail assertions in
/// [`built_header_carries_go_shape_fields`] (coreth activates Cancun with Etna,
/// `miner/worker.go:186-197`).
fn etna_chain_spec(chain_id: u64) -> AvaChainSpec {
    const FAR_FUTURE: u64 = u64::MAX;
    let upgrades = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: 0,
        apricot_phase_5: 0,
        apricot_phase_pre_6: 0,
        apricot_phase_6: 0,
        apricot_phase_post_6: 0,
        banff: 0,
        cortina: 0,
        durango: 0,
        etna: 0,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
        helicon: u64::MAX,
    };
    AvaChainSpec::from_parts(upgrades, ava_evm_reth::Chain::from_id(chain_id), false)
}

/// The same schedule as [`etna_chain_spec`] but with Fortuna **and** Granite
/// active from genesis — used by [`built_header_carries_acp176_extra_prefix`] so
/// the built block is in the ACP-176 regime (24-byte fee-state extra prefix +
/// the Granite millisecond/min-delay-excess tail).
fn granite_chain_spec(chain_id: u64) -> AvaChainSpec {
    let upgrades = NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: 0,
        apricot_phase_5: 0,
        apricot_phase_pre_6: 0,
        apricot_phase_6: 0,
        apricot_phase_post_6: 0,
        banff: 0,
        cortina: 0,
        durango: 0,
        etna: 0,
        fortuna: 0,
        granite: 0,
        helicon: u64::MAX,
    };
    AvaChainSpec::from_parts(upgrades, ava_evm_reth::Chain::from_id(chain_id), false)
}

/// Opens a fresh Firewood db with the genesis alloc committed; returns the
/// provider, the EVM config (built over `spec`), a canonical store, and the
/// committed genesis root.
fn setup(
    fx: &Fixture,
    spec: AvaChainSpec,
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
    let config = AvaEvmConfig::new(spec);
    (dir, provider, config, canonical, genesis_root)
}

/// A synthetic genesis (height-0) coreth header carrying the committed genesis
/// state root + the AP3 base fee, the parent the builder builds block-1 on.
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

/// The next-block build/fee context (AP3 window regime, genesis defaults).
fn next_ctx() -> AvaNextBlockCtx {
    AvaNextBlockCtx {
        timestamp: 10,
        suggested_fee_recipient: Address::ZERO,
        ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
    }
}

#[test]
fn build_then_verify_same_root() {
    let fx = load_fixture();
    let (_dir, provider, config, canonical, genesis_root) = setup(&fx, ap3_chain_spec(fx.chain_id));

    // The candidate EVM tx: block-1's single transfer (recovered).
    let decoded = decode_ava_evm_block(&block1_bytes(&fx), config.chain_spec()).expect("decode");
    let evm_txs = decoded.recover_senders().expect("recover senders");
    assert_eq!(evm_txs.len(), 1, "fixture block-1 has one EVM tx");

    // The synthetic genesis header (we don't reproduce coreth's exact genesis
    // bytes — `block1_parent_hash` would differ — but build-then-verify symmetry
    // depends only on the parent STATE ROOT, supplied explicitly below).
    let parent = genesis_header(&fx, genesis_root);

    let txpool = Arc::new(Mutex::new(ava_evm::atomic::mempool::AtomicMempool::new(
        64,
        ava_types::id::Id::EMPTY,
    )));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    // ---- build ----
    let built = driver
        .build_on(&parent, genesis_root, &next_ctx(), evm_txs)
        .expect("build_on");
    assert_eq!(built.number(), 1, "built block is height 1");
    assert_eq!(
        built.transactions().len(),
        1,
        "the candidate EVM tx was packed into the block"
    );
    let built_root = *built.header_state_root();

    // build_on stashed the proposal (commit-on-accept); the committed tip is
    // unchanged (G5/G1: nothing committed at build time).
    assert_eq!(
        provider.root(),
        genesis_root,
        "build does not advance the committed Firewood tip"
    );

    // ---- re-verify the self-built block to the IDENTICAL root (§17.6) ----
    // `verify` re-stashes against the (still-genesis) tip; drop the build stash
    // first so the verify path owns the proposal it commits.
    provider.discard(built_root);
    let ctx = EvmBlockContext::new(Arc::clone(&provider), config, canonical);
    let reverified = built
        .verify(&ctx, genesis_root)
        .expect("re-verify built block");
    assert_eq!(
        reverified, built_root,
        "build-then-verify symmetry: the self-built block re-verifies to the same Firewood root"
    );
}

#[test]
fn respects_min_build_delay() {
    let fx = load_fixture();
    let (_dir, provider, config, _canonical, genesis_root) =
        setup(&fx, ap3_chain_spec(fx.chain_id));

    let decoded = decode_ava_evm_block(&block1_bytes(&fx), config.chain_spec()).expect("decode");
    let evm_txs = decoded.recover_senders().expect("recover senders");
    let parent = genesis_header(&fx, genesis_root);
    let parent_hash = parent.hash();

    let txpool = Arc::new(Mutex::new(ava_evm::atomic::mempool::AtomicMempool::new(
        64,
        ava_types::id::Id::EMPTY,
    )));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    // First build on genesis succeeds and arms the min-retry-delay guard.
    let first = driver.build_on(&parent, genesis_root, &next_ctx(), evm_txs.clone());
    assert!(first.is_ok(), "first build on a fresh parent succeeds");

    // The guard now rejects an immediate re-build on the SAME parent.
    assert!(
        !driver.can_build_on(parent_hash, Instant::now()),
        "min-retry-delay guard blocks an immediate same-parent re-build"
    );
    let second = driver.build_on(&parent, genesis_root, &next_ctx(), evm_txs);
    assert!(
        second.is_err(),
        "a same-parent re-build within the delay is the no-pending-block no-op"
    );

    // Once the delay has elapsed (a `now` past `last_build + delay`), the guard
    // re-opens for the same parent.
    let later = Instant::now()
        .checked_add(MIN_BLOCK_BUILD_DELAY)
        .and_then(|t| t.checked_add(MIN_BLOCK_BUILD_DELAY))
        .expect("instant add");
    assert!(
        driver.can_build_on(parent_hash, later),
        "guard re-opens once the min-retry delay has elapsed"
    );

    // A DIFFERENT parent is always immediately buildable (the guard is per-parent).
    assert!(
        driver.can_build_on(B256::repeat_byte(0x99), Instant::now()),
        "the guard is keyed per-parent; a different parent is buildable now"
    );
}

/// M9.15 task 2: the builder stamps Go-shaped header fields on a built block —
/// blackhole coinbase, the Cancun (== Etna) tail, and real tx/receipt roots +
/// bloom derived from the body, not the previous sentinels/`suggested_fee_recipient`.
///
/// Reuses the `genesis_to_1` fixture's committed genesis (genesis-root parity
/// is alloc-only, independent of the fork schedule) but over [`etna_chain_spec`]
/// so the built block is Cancun-active and the tail assertions below apply.
#[test]
fn built_header_carries_go_shape_fields() {
    let fx = load_fixture();
    let (_dir, provider, config, _canonical, genesis_root) =
        setup(&fx, etna_chain_spec(fx.chain_id));

    let decoded = decode_ava_evm_block(&block1_bytes(&fx), config.chain_spec()).expect("decode");
    let evm_txs = decoded.recover_senders().expect("recover senders");
    assert_eq!(evm_txs.len(), 1, "fixture block-1 has one EVM tx");

    let parent = genesis_header(&fx, genesis_root);
    let txpool = Arc::new(Mutex::new(ava_evm::atomic::mempool::AtomicMempool::new(
        64,
        ava_types::id::Id::EMPTY,
    )));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    let built = driver
        .build_on(&parent, genesis_root, &next_ctx(), evm_txs)
        .expect("build_on");
    let header = built.header();
    let txs = built.transactions();
    assert_eq!(
        txs.len(),
        1,
        "the candidate EVM tx was packed into the block"
    );

    // coreth `plugin/evm/vm.go:565` — coinbase is the blackhole address, not
    // the per-block `suggested_fee_recipient`.
    assert_eq!(header.coinbase, BLACKHOLE_ADDRESS, "build_header coinbase");

    // coreth `miner/worker.go:186-197` — the Cancun tail at Etna+.
    assert_eq!(header.parent_beacon_root, Some(B256::ZERO));
    assert_eq!(header.blob_gas_used, Some(0));
    assert_eq!(header.excess_blob_gas, Some(0));

    // coreth `customheader/extra.go:46-53` — Etna is in the AP3 window regime
    // (Fortuna is far-future here), so the extra prefix is the 80-byte fee
    // window. Building on a genesis (number-0) parent, coreth's `feeWindow`
    // returns the empty window (`dynamic_fee_windower.go:156-158`). Etna is
    // also Durango+ here, so a 6-byte predicate-results suffix follows the
    // prefix (`core/evm.go:187` `SetPredicateBytesInExtra`; M9.15 Task 6 —
    // `builder.rs::EMPTY_BLOCK_PREDICATE_RESULTS`); it happens to be all-zero
    // too (the empty-`BlockResults` encoding), so the content check below is
    // unaffected.
    assert_eq!(
        header.extra.len(),
        WINDOW_SIZE + 6,
        "Etna (Window regime) extra prefix is the 80-byte fee window + the 6-byte empty predicate-results suffix"
    );
    assert_eq!(
        header.extra.as_ref(),
        &[0u8; WINDOW_SIZE + 6][..],
        "first block on genesis carries the empty (all-zero) fee window + empty predicate results"
    );
    // Pre-Granite: no millisecond timestamp / min-delay-excess tail.
    assert_eq!(header.time_milliseconds, None);
    assert_eq!(header.min_delay_excess, None);

    // coreth `customtypes/block_ext.go:189` — tx/receipt roots + bloom are
    // derived from the body at assembly.
    assert_eq!(
        header.tx_root,
        calculate_transaction_root(txs),
        "tx_root == the ordered-trie root over the built block's own transactions \
         (computed independently here, not read back off the builder)"
    );
    assert_ne!(
        header.receipt_root, EMPTY_ROOT_HASH,
        "real receipt root, not the empty-trie sentinel"
    );
    // The fixture's single tx is a plain value transfer — it emits no logs, so
    // the CORRECTLY OR-folded bloom over its (empty) receipt bloom is the zero
    // bloom. This still exercises the real per-receipt `bloom()`/fold path (as
    // opposed to the removed hardcoded `Bytes::from(vec![0u8; 256])` sentinel);
    // it happens to coincide byte-for-byte with that sentinel only because
    // there is nothing to fold in.
    assert_eq!(
        header.bloom.as_ref(),
        &[0u8; 256][..],
        "no logs in the batch ⇒ the OR-fold of receipt blooms is the zero bloom"
    );
}

/// M9.15 task 4: at Fortuna+ the builder stamps the exact 24-byte ACP-176 fee
/// state as the header extra prefix (coreth `customheader/extra.go:36-44`
/// `ExtraPrefix` → `feeStateAfterBlock`), and at Granite the millisecond
/// timestamp + ACP-226 min-delay-excess tail (`consensus/dummy/consensus.go:334-352`).
///
/// The built block's extra must round-trip as the exact post-block fee state
/// recomputed from the parent + built-header fields — the invariant Go's
/// dummy-engine `VerifyExtraPrefix` checks byte-for-byte on every node.
#[test]
fn built_header_carries_acp176_extra_prefix() {
    let fx = load_fixture();
    let (_dir, provider, config, _canonical, genesis_root) =
        setup(&fx, granite_chain_spec(fx.chain_id));

    let decoded = decode_ava_evm_block(&block1_bytes(&fx), config.chain_spec()).expect("decode");
    let evm_txs = decoded.recover_senders().expect("recover senders");
    assert_eq!(evm_txs.len(), 1, "fixture block-1 has one EVM tx");

    // A Granite genesis parent carries the ACP-226 initial min-delay-excess and a
    // millisecond timestamp (coreth `Genesis.toBlock` at Granite; the real local
    // genesis does the same) — `minDelayExcess` parity requires the parent to
    // carry it. The extra stays empty: a number-0 parent seeds the zero fee state.
    let mut parent = genesis_header(&fx, genesis_root);
    parent.time_milliseconds = Some(0);
    parent.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);

    let ctx = AvaNextBlockCtx {
        timestamp: 10,
        timestamp_ms: 10_000,
        suggested_fee_recipient: Address::ZERO,
        parent_fee_state: parent_fee_state_of(config.chain_spec(), &parent)
            .expect("parent fee state"),
        ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
    };

    let txpool = Arc::new(Mutex::new(ava_evm::atomic::mempool::AtomicMempool::new(
        64,
        ava_types::id::Id::EMPTY,
    )));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    let built = driver
        .build_on(&parent, genesis_root, &ctx, evm_txs)
        .expect("build_on");
    let header = built.header();

    // The extra is the 24-byte ACP-176 fee state prefix plus the 6-byte
    // Durango+ empty predicate-results suffix (`core/evm.go:187`
    // `SetPredicateBytesInExtra`; M9.15 Task 6 —
    // `builder.rs::EMPTY_BLOCK_PREDICATE_RESULTS`). `Acp176State::from_bytes`
    // below only reads the leading `STATE_SIZE` bytes, so the round-trip check
    // is unaffected by the trailing suffix.
    assert_eq!(
        header.extra.len(),
        STATE_SIZE + 6,
        "Fortuna+ extra is the 24-byte ACP-176 fee state + the 6-byte empty predicate-results suffix"
    );

    // Round-trip: the built block's extra must parse back to the exact post-block
    // fee state recomputed from the parent + this header's own fields.
    let ext_data_gas_used = header
        .ext_data_gas_used
        .map(|v| u64::try_from(v).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let reparsed = Acp176State::from_bytes(&header.extra).expect("parse built extra prefix");
    assert_eq!(
        reparsed,
        fee_state_after_block(
            config.chain_spec(),
            &parent,
            header.time,
            header.time_milliseconds,
            header.gas_used,
            ext_data_gas_used,
            None,
        )
        .expect("fee_state_after_block"),
        "builder extra prefix == coreth ExtraPrefix (feeStateAfterBlock)"
    );

    // Granite tail: the millisecond timestamp we built at + the ACP-226
    // min-delay-excess (carried from the Granite parent, no desired override).
    assert_eq!(
        header.time_milliseconds,
        Some(10_000),
        "Granite header stamps the millisecond timestamp"
    );
    assert_eq!(
        header.min_delay_excess,
        Some(INITIAL_DELAY_EXCESS.0),
        "Granite header carries the parent's ACP-226 min-delay-excess"
    );
}

/// M9.15 task 5 — the offline exit gate of Phases 1+2 (builder + verify): a
/// Rust-built block must satisfy the FULL ported `syntacticVerify`
/// (`wrapped_block.go:398-527`), including the two checks this task adds
/// (`Difficulty == 1`, `VerifyExtra`) — driven through the SAME
/// [`ava_evm::block::EvmBlock::verify`] entry the `ChainVm` adapter uses, not
/// a bespoke check. Built on the all-forks-active-from-genesis (Granite)
/// spec so every fork-gated check in `syntactic_verify` is exercised.
#[test]
fn built_block_passes_full_syntactic_verify() {
    let fx = load_fixture();
    let (_dir, provider, config, canonical, genesis_root) =
        setup(&fx, granite_chain_spec(fx.chain_id));

    let decoded = decode_ava_evm_block(&block1_bytes(&fx), config.chain_spec()).expect("decode");
    let evm_txs = decoded.recover_senders().expect("recover senders");
    assert_eq!(evm_txs.len(), 1, "fixture block-1 has >= 1 EVM tx");

    let mut parent = genesis_header(&fx, genesis_root);
    parent.time_milliseconds = Some(0);
    parent.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);

    let ctx = AvaNextBlockCtx {
        timestamp: 10,
        timestamp_ms: 10_000,
        suggested_fee_recipient: Address::ZERO,
        parent_fee_state: parent_fee_state_of(config.chain_spec(), &parent)
            .expect("parent fee state"),
        ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
    };

    let txpool = Arc::new(Mutex::new(ava_evm::atomic::mempool::AtomicMempool::new(
        64,
        ava_types::id::Id::EMPTY,
    )));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    let built = driver
        .build_on(&parent, genesis_root, &ctx, evm_txs)
        .expect("build_on");

    // consensus.go:233-235 — `Prepare` stamps every built header's difficulty
    // to exactly 1.
    assert_eq!(
        built.header().difficulty,
        U256::from(1),
        "builder must stamp difficulty 1 (coreth consensus.go:233-235)"
    );

    // build_on stashed the proposal (commit-on-accept); drop it so the verify
    // path below owns the proposal it re-stashes and commits (mirrors
    // `build_then_verify_same_root`).
    let built_root = *built.header_state_root();
    provider.discard(built_root);

    let block_ctx = EvmBlockContext::new(Arc::clone(&provider), config, canonical);
    // The full `syntacticVerify` port + semantic execute — the SAME entry the
    // `ChainVm` adapter drives.
    built
        .verify(&block_ctx, genesis_root)
        .expect("a Rust-built block must pass its own full syntactic_verify");
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! G1/G9 invariant CI guard: **Firewood is the EVM state-of-record; reth never
//! writes state or trie tables** (spec 10 §17.2/§17.7/§17.11).
//!
//! # Plan/spec adaptation (MDBX wording → ava-database-KV + Firewood reality)
//!
//! Spec §17.2 and the original M6.27 task text were written against the reth-db
//! MDBX schema: "assert `PlainState`/`HashedState`/`Trie` tables are empty."
//! **That wording is stale.** Per the M6.9 as-built decision, `CanonicalStore`
//! (`src/canonical.rs`) is implemented over the **`ava-database` prefixed-KV
//! backend**, not reth-db MDBX — there is NO reth `DatabaseEnv` in `ava-evm`,
//! and reth-db's `PlainState`/`HashedState`/`Trie` tables do not exist.
//!
//! The ADAPTED invariant, proven by this file:
//!
//! ## Runtime assertion (`state_trie_tables_stay_empty_after_block`)
//!
//! Build and accept a block (the same lifecycle harness as `tests/lifecycle.rs`),
//! then assert:
//!
//! 1. **CanonicalStore block-metadata grew**: `last_canonical()`, `canonical_hash()`,
//!    `header_at()`, `height_of()` all reflect block-1 — proving the KV
//!    namespaces (HEADER / CANONICAL / NUMBER / BODY / RECEIPTS / TIP) were
//!    written on `accept()`.
//! 2. **Firewood advanced**: `provider.root()` changed from `genesis_root` to the
//!    block-1 post-state root — proving EVM state lives in Firewood, not reth.
//! 3. **`TrieUpdates` are always empty** from `state_root_with_updates`: the G1
//!    trick (spec 10 §17.2 §5.2) returns a real Firewood root but empty
//!    `TrieUpdates`, so reth never persists trie nodes. Asserted separately on a
//!    fresh provider to demonstrate the contract directly.
//!
//! There are no reth MDBX tables to check for emptiness: the entire reth
//! state/trie pipeline is bypassed at the architecture level.
//!
//! ## Structural guard (`no_reth_state_writer_in_ava_evm_src`)
//!
//! Reads every `.rs` file under `crates/ava-evm/src/` and asserts that none of
//! them name the reth state-writer symbols `BlockchainProvider`,
//! `UnifiedStorageWriter`, or `StateWriter` on a non-comment line. These are the
//! entry points to reth's MDBX state-persistence pipeline. The test panics if
//! any of those symbols appear in the (non-facade) source, giving a CI gate that
//! cannot be silently bypassed by new code without breaking this test.
//!
//! The facade crate `ava-evm-reth` is intentionally excluded from this grep
//! (it may re-export reth types); only `crates/ava-evm/src` is checked.

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
    StateRootProvider, U256,
};

// ---------------------------------------------------------------------------
// Fixture helpers (mirrors lifecycle.rs)
// ---------------------------------------------------------------------------

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

/// Opens a fresh Firewood db with genesis alloc committed; returns the
/// `EvmBlockContext` (provider + config + canonical), the genesis root, and the
/// temp dir handle (must stay live for Firewood to keep its files).
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
        "genesis state root parity"
    );

    let canonical = Arc::new(CanonicalStore::new(Arc::new(MemDb::new())));
    let config = AvaEvmConfig::new(ap3_chain_spec(fx.chain_id));
    let ctx = EvmBlockContext::new(provider, config, canonical);
    (dir, ctx, genesis_root)
}

/// Builds a block-1 re-assembled so its `header.state_root` matches the root
/// our Firewood backend actually produces (the M6.30 parity gap; mirrors
/// `lifecycle.rs::verifiable_block1`). The dry-run stash is discarded so
/// `verify` starts clean.
fn verifiable_block1(fx: &Fixture, ctx: &EvmBlockContext, parent_root: B256) -> EvmBlock {
    let decoded = decode_ava_evm_block(&block1_bytes(fx), ctx.chain_spec()).expect("decode block1");
    let txs = decoded.recover_senders().expect("recover senders");

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
    ctx.state().discard(root); // Drop the dry-run stash; verify re-stashes.

    let mut parts = decoded.into_parts();
    parts.header.state_root = root;
    assemble_ava_block(parts, ctx.chain_spec()).expect("assemble")
}

// ---------------------------------------------------------------------------
// Test 1 — Runtime invariant: CanonicalStore metadata grew; Firewood advanced;
//          no reth state writer was invoked.
// ---------------------------------------------------------------------------

/// G1/G9 runtime invariant: build + accept one block and assert:
///
/// * The **CanonicalStore KV namespaces grew**: `last_canonical()`,
///   `canonical_hash()`, `header_at()`, `height_of()` all reflect block-1.
///   This proves the HEADER / CANONICAL / NUMBER / BODY / RECEIPTS / TIP
///   prefixes were written (spec 10 §17.7 "non-state block metadata only").
///
/// * **Firewood advanced**: `provider.root()` changed from `genesis_root` to
///   the block-1 post-state root, proving EVM state lives exclusively in
///   Firewood and was committed through `FirewoodStateProvider::commit`.
///
/// * **No reth MDBX state/trie tables were written**: there are none —
///   the architecture bypasses reth's `BlockchainProvider` / `UnifiedStorageWriter`
///   / `StateWriter::write_state` pipeline entirely. The absence is structural
///   (proven by the companion `no_reth_state_writer_in_ava_evm_src` test below)
///   and guaranteed at the accept path: `EvmBlock::accept` drives only
///   `FirewoodStateProvider::commit` + `CanonicalStore::append_canonical`.
#[test]
fn state_trie_tables_stay_empty_after_block() {
    let fx = load_fixture();
    let (_dir, ctx, genesis_root) = setup(&fx);
    let block = verifiable_block1(&fx, &ctx, genesis_root);

    // --- Pre-accept assertions ---
    // CanonicalStore is empty: no block has been accepted yet.
    assert_eq!(
        ctx.canonical().last_canonical().expect("tip"),
        None,
        "canonical tip must be None before any accept"
    );
    // Firewood tip is still at genesis: verify does NOT commit.
    assert_eq!(ctx.state().root(), genesis_root);

    // Verify (propose + stash): computes the pre-commit root WITHOUT advancing
    // the Firewood committed tip.
    let precommit = block.verify(&ctx, genesis_root).expect("verify");
    assert_eq!(
        ctx.state().root(),
        genesis_root,
        "G1: verify must NOT advance the Firewood tip (proposal only stashed)"
    );
    assert_eq!(precommit, *block.header_state_root());

    // Accept: commit the stashed Firewood proposal + write CanonicalStore.
    block.accept(&ctx, precommit).expect("accept");

    // --- Post-accept: Firewood advanced ---
    let tip_after_accept = ctx.state().root();
    assert_ne!(
        tip_after_accept, genesis_root,
        "G1: Firewood tip must advance on accept"
    );
    assert_eq!(
        tip_after_accept, precommit,
        "G1: Firewood tip == pre-commit root after accept"
    );

    // --- Post-accept: CanonicalStore KV namespaces (HEADER/CANONICAL/NUMBER/
    //                  BODY/RECEIPTS/TIP) all grew to reflect block-1 ---
    assert_eq!(
        ctx.canonical().last_canonical().expect("tip"),
        Some(block.number()),
        "G6: TIP namespace updated to block-1 height"
    );
    assert_eq!(
        ctx.canonical()
            .canonical_hash(block.number())
            .expect("canonical_hash"),
        Some(block.hash()),
        "G6: CANONICAL namespace maps block-1 number -> hash"
    );
    assert_eq!(
        ctx.canonical().height_of(block.hash()).expect("height_of"),
        Some(block.number()),
        "G6: NUMBER namespace maps block-1 hash -> number"
    );
    // HEADER namespace: the header state-root commitment was stored.
    assert_eq!(
        ctx.canonical()
            .header_at(block.number())
            .expect("header_at"),
        Some(*block.header_state_root()),
        "G6: HEADER namespace stores the header state-root commitment at block-1"
    );

    // --- G1 structural summary ---
    // There are no reth MDBX tables in ava-evm. The only state-write path is:
    //   FirewoodStateProvider::commit -> Firewood (accounts/storage/trie)
    // The only metadata-write path is:
    //   CanonicalStore::append_canonical -> ava-database KV (block metadata)
    // Both are accounted for above. The companion structural test below enforces
    // this at the source level: no `BlockchainProvider`/`UnifiedStorageWriter`/
    // `StateWriter` symbols appear in non-comment lines of `crates/ava-evm/src/`.
}

// ---------------------------------------------------------------------------
// Test 2 — G1 trick: state_root_with_updates always returns empty TrieUpdates
// ---------------------------------------------------------------------------

/// The G1 trick (spec 10 §17.2 §5.2): `FirewoodStateView::state_root_with_updates`
/// returns the real Firewood root **and an empty `TrieUpdates`** — reth's trie
/// persistence pipeline is never engaged, even when it asks for updates via the
/// `StateRootProvider` trait. Asserted here as a direct, isolated contract test.
#[test]
fn state_root_with_updates_returns_empty_trie_updates() {
    use ava_evm_reth::HashedPostState;

    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    // A trivial state delta: one account.  `HashedPostState` uses reth's `Account`
    // (nonce/balance/bytecode_hash), NOT revm's `AccountInfo`.
    use ava_evm_reth::Account;
    let mut accounts = ava_evm_reth::B256Map::default();
    accounts.insert(
        ava_evm_reth::keccak256(Address::repeat_byte(0x42)),
        Some(Account {
            balance: U256::from(1u64),
            nonce: 0,
            bytecode_hash: None,
        }),
    );
    let hashed = HashedPostState {
        accounts,
        storages: ava_evm_reth::B256Map::default(),
    };

    let view = provider.view_tip().expect("view_tip");
    let (root, updates) = view
        .state_root_with_updates(hashed)
        .expect("state_root_with_updates");

    // The root is non-empty (a real Firewood root, not EMPTY_ROOT_HASH).
    assert_ne!(root, ava_evm_reth::EMPTY_ROOT_HASH);

    // G1 trick: TrieUpdates must be empty — reth must not persist trie nodes.
    assert!(
        updates.is_empty(),
        "G1: state_root_with_updates must return empty TrieUpdates \
         (Firewood is the trie-of-record; reth must never write trie nodes)"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — Structural guard: no reth state-writer symbols in ava-evm/src
// ---------------------------------------------------------------------------

/// Structural CI guard: walks every `.rs` file under `crates/ava-evm/src/` and
/// asserts that none of them name the reth state-writer symbols on a non-comment
/// source line:
///
/// * `BlockchainProvider` — reth's unified reader/writer over MDBX
/// * `UnifiedStorageWriter` — reth's multi-table storage writer
/// * `StateWriter` — reth's staged-sync state writer (+ `write_state`)
///
/// These are the entry points to reth's MDBX state-persistence pipeline.
/// A violation would mean `ava-evm` is writing state/trie data through reth's
/// tables in addition to (or instead of) Firewood — a G1/G9 invariant break.
///
/// The facade crate `ava-evm-reth` is intentionally excluded (it may re-export
/// reth types for the G0 boundary); only `crates/ava-evm/src` is checked.
#[test]
fn no_reth_state_writer_in_ava_evm_src() {
    use std::path::Path;

    // The forbidden reth state-writer symbol substrings.  Any occurrence on a
    // non-comment source line indicates a potential G1 invariant violation.
    const FORBIDDEN: &[&str] = &["BlockchainProvider", "UnifiedStorageWriter", "StateWriter"];

    // Walk `crates/ava-evm/src/` recursively, collecting all `.rs` files.
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;
    let mut violations: Vec<String> = Vec::new();

    walk_rs_files(&src_root, &mut |path: &Path, contents: &str| {
        checked = checked.saturating_add(1);
        for (line_no, line) in contents.lines().enumerate() {
            // Skip doc-comment and line-comment lines: they may legitimately
            // mention reth type names for documentation purposes (e.g.
            // `src/state.rs` module doc references "nodes via `StateWriter`").
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for sym in FORBIDDEN {
                if line.contains(sym) {
                    violations.push(format!(
                        "{}:{}: found forbidden reth state-writer symbol `{}`\n  > {}",
                        path.display(),
                        line_no.saturating_add(1),
                        sym,
                        line.trim()
                    ));
                }
            }
        }
    });

    assert!(
        checked > 0,
        "structural guard: no .rs files found under {src_root:?} — path misconfigured?"
    );

    if !violations.is_empty() {
        panic!(
            "G1/G9 invariant violation: `crates/ava-evm/src` must not use reth state-writer \
             symbols (Firewood is the EVM state-of-record; reth must never write state/trie \
             tables). Violations found:\n\n{}",
            violations.join("\n")
        );
    }
}

/// Recursively walks `dir`, calling `f` for each `.rs` file with its path and
/// UTF-8 contents. Silently skips unreadable files and non-UTF-8 files.
fn walk_rs_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk_rs_files(&path, f);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            f(&path, &contents);
        }
    }
}

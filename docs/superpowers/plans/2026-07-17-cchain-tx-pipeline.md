# C-Chain EVM Transaction Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Rust C-chain accepts `eth_sendRawTransaction`, holds EVM txs in a coreth-parity mempool, includes them in Rust-proposed blocks, and serves `eth_getTransactionReceipt` — unblocking the parent plan's Task 8 live "Rust proposes" arm.

**Architecture:** A purpose-built `EvmMempool` (the `AtomicMempool` precedent — no new deps), wired RPC-side into `EthRpc` (the `AvaxRpc`/`issueTx` pattern) and build-side into `EvmVm::build_block` (whose `pack_evm_txs`/`build_on` path is already fully functional given txs). Receipts are stashed at verify time (the warp-seam pattern) and persisted + indexed at accept. Tx gossip is DEFERRED (design doc §Non-goals) — its absence IS Task 8's proposer-detection mechanism.

**Tech Stack:** Rust workspace (`ava-evm`, `ava-evm-reth` facade), alloy 2.0.x / reth rev 88505c7, Firewood, tokio. coreth ground truth at `~/avalanchego/graft/coreth`.

**Spec:** `docs/superpowers/specs/2026-07-17-cchain-tx-pipeline-design.md`

## Global Constraints

- License header on every new `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` + `// See the file LICENSE for licensing terms.`
- No `unwrap()`/`expect()`/`dbg!`/`todo!` in library code (tests may); errors via `thiserror` enums.
- Every coreth-mirroring rule carries a `// coreth <file>:<line>` citation; error messages contain the Go sentinel substrings.
- Import grouping std → external → crate; 4-space indent; fmt via `./scripts/nix_run.sh cargo fmt` if plain fmt drifts.
- Test runner: `./scripts/nix_run.sh cargo nextest run -p ava-evm` (plain cargo/nextest may be off PATH). Some ava-evm tests need `-j1` (global Firewood-ethhash switch) — retry with `-j1` on unexpected cross-test failures.
- Each task ends with: scoped nextest green, `./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings` clean, commit.
- Branch: `m9.15-rust-proposer` (already checked out — this is a nested insert; do NOT create a new branch).
- The existing ava-evm suite (224+ tests incl. the Task-5 self-gate `built_block_passes_full_syntactic_verify` and the Task-6 reader `proposer_verdicts_hold`) must stay green after every task.
- DIVERGENCES from coreth must be documented in-code where they live: (1) nonce-gap txs rejected, not queued; (2) `best_txs` orders by fee-cap not block-base-fee effective tip; (3) included-sender stale eviction is sender-local, not state-driven pool reorg.

---

### Task 1: `EvmMempool` — admission validation + storage + eviction

**Files:**
- Create: `crates/ava-evm/src/mempool.rs`
- Modify: `crates/ava-evm/src/lib.rs` (add `pub mod mempool;` beside the existing `pub mod` declarations)

**Interfaces:**
- Consumes: `RecoveredTx` (= `Recovered<TransactionSigned>`, facade re-export `ava-evm-reth/src/lib.rs:361`), `ConsensusTx` trait accessors (the crate already uses `ConsensusTx::max_fee_per_gas` at `block.rs` `check_min_gas_price` and `ConsensusTx::effective_tip_per_gas`/`gas_limit` at `builder.rs:313-330` — mirror those call shapes), `U256`/`B256`/`Address` from the facade.
- Produces (Tasks 2/4/5 rely on these exact names):
  - `pub struct EvmMempool` with `pub fn new(max_size: usize) -> Self`, `pub fn subscribe(&self) -> Arc<Notify>`, `pub fn len(&self) -> usize`, `pub fn is_empty(&self) -> bool`, `pub fn contains(&self, hash: &B256) -> bool`
  - `pub struct SenderAccount { pub nonce: u64, pub balance: U256 }`
  - `pub struct AdmissionRules { pub chain_id: u64, pub min_tip_wei: u128, pub tx_fee_cap_wei: U256, pub shanghai: bool }` with `impl Default` (min_tip_wei = 1, tx_fee_cap_wei = 1 AVAX = 10^18, shanghai = true)
  - `pub fn add_local(&mut self, tx: RecoveredTx, sender: &SenderAccount, rules: &AdmissionRules) -> Result<B256, EvmMempoolError>` (returns the tx hash on admission)
  - `pub enum EvmMempoolError` (thiserror) — variants below.

- [ ] **Step 1: Write the failing admission tests**

In `mempool.rs`'s `#[cfg(test)]` module. Build txs with the same signing helper pattern the crate's builder tests use (`crates/ava-evm/tests/build.rs` signs legacy txs with a fixed private key — copy that local helper into the test module; test-file convention is repeat-don't-import). One test per rule, RED-first:

```rust
#[test]
fn admits_a_valid_legacy_tx() {
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(/*nonce*/ 0, /*gas_price*/ 2_000_000_000, /*gas*/ 21_000, /*value*/ 1);
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let hash = pool.add_local(tx, &sender, &AdmissionRules::default()).expect("admit");
    assert!(pool.contains(&hash), "EvmMempool::add_local admits + indexes by hash");
    assert_eq!(pool.len(), 1);
}

#[test]
fn rejects_nonce_too_low() {
    // coreth core/txpool/validation.go:239 (ErrNonceTooLow, "nonce too low")
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
    let sender = SenderAccount { nonce: 5, balance: U256::from(10u128.pow(18)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("nonce too low"), "got: {err}");
}

#[test]
fn rejects_nonce_gap_documented_divergence() {
    // coreth QUEUES future-nonce txs (legacypool queued set); this pool
    // rejects them — documented divergence, design doc §Non-goals.
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(2, 2_000_000_000, 21_000, 1);
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("nonce gap"), "got: {err}");
}

#[test]
fn rejects_insufficient_funds() {
    // coreth core/txpool/validation.go:250-254 ("insufficient funds")
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
    let sender = SenderAccount { nonce: 0, balance: U256::from(1000u64) }; // « cost
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("insufficient funds"), "got: {err}");
}

#[test]
fn rejects_intrinsic_gas_too_low() {
    // coreth core/txpool/validation.go:125-130 → core.IntrinsicGas ("intrinsic gas too low")
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(0, 2_000_000_000, 20_999, 1); // < 21000 base
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("intrinsic gas too low"), "got: {err}");
}

#[test]
fn rejects_unprotected_tx() {
    // coreth internal/ethapi/api.go:1804-1807 ("only replay-protected
    // (EIP-155) transactions allowed over RPC") — default allow-unprotected = false.
    // Build a legacy tx signed WITHOUT a chain id (pre-155 v).
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx_unprotected(0, 2_000_000_000, 21_000, 1);
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("replay-protected"), "got: {err}");
}

#[test]
fn rejects_wrong_chain_id() {
    // Signature recovery + chain id agreement: a tx for chain 9999 vs rules.chain_id.
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx_for_chain(9999, 0, 2_000_000_000, 21_000, 1);
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("chain"), "got: {err}");
}

#[test]
fn rejects_fee_over_configured_cap() {
    // coreth internal/ethapi/api.go:1801 checkTxFee → "exceeds the configured cap"
    // gas_price * gas > 1 AVAX.
    let mut pool = EvmMempool::new(16);
    let tx = signed_legacy_tx(0, 100_000_000_000_000, 21_000, 1); // 2.1 AVAX fee
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(19)) };
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("exceeds the configured cap"), "got: {err}");
}

#[test]
fn rejects_already_known() {
    // coreth core/txpool/errors.go ErrAlreadyKnown ("already known")
    let mut pool = EvmMempool::new(16);
    let sender = SenderAccount { nonce: 0, balance: U256::from(10u128.pow(18)) };
    let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
    pool.add_local(tx.clone(), &sender, &AdmissionRules::default()).expect("first");
    let err = pool.add_local(tx, &sender, &AdmissionRules::default()).unwrap_err();
    assert!(err.to_string().contains("already known"), "got: {err}");
}

#[test]
fn same_nonce_replacement_requires_higher_fee_and_full_pool_evicts_cheapest() {
    // Replacement: same sender+nonce needs a strictly higher fee cap
    // (coreth legacypool price-bump rule, simplified to strict-greater);
    // capacity: at max_size, admitting a better-paying tx evicts the
    // lowest-fee-cap tx, a worse one gets "txpool is full"
    // (coreth core/txpool/errors.go ErrTxPoolOverflow "txpool is full").
    // ... assert both behaviors with a max_size=2 pool ...
}
```

The signing helpers: locate the existing signed-tx construction in `crates/ava-evm/tests/build.rs` (`grep -n "sign" crates/ava-evm/tests/build.rs`) and adapt it into module-local `signed_legacy_tx*` helpers returning `RecoveredTx`. The local chain id used by `AdmissionRules::default()` in tests: use the same constant the build.rs harness signs with.

- [ ] **Step 2: Run tests to verify they fail**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(mempool)'`
Expected: FAIL to compile (module absent) — that is the RED for a new module.

- [ ] **Step 3: Implement**

`mempool.rs` skeleton (complete the bodies; every rule cites its Go line):

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-Chain EVM mempool (design doc 2026-07-17): a purpose-built pool
//! mirroring coreth's SUBMISSION-path admission rules (internal/ethapi/api.go
//! SubmitTransaction + core/txpool/validation.go), NOT reth's pool. The
//! `AtomicMempool` (atomic/mempool.rs) is the structural precedent.
//!
//! DIVERGENCE (documented, design §Non-goals): future-nonce (gapped) txs are
//! rejected, not queued — coreth's legacypool would hold them in `queued`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tokio::sync::Notify;
// ... facade imports (RecoveredTx, ConsensusTx, U256, B256, Address) ...

/// coreth core/state_transition.go `IntrinsicGas` constants (params/protocol_params.go).
const TX_GAS: u64 = 21_000;
const TX_GAS_CONTRACT_CREATION: u64 = 53_000;
const TX_DATA_ZERO_GAS: u64 = 4;
const TX_DATA_NON_ZERO_GAS_EIP2028: u64 = 16; // Istanbul (always active ≥ AP0 here)
const ACCESS_LIST_ADDRESS_GAS: u64 = 2_400;
const ACCESS_LIST_STORAGE_KEY_GAS: u64 = 1_900;
const INIT_CODE_WORD_GAS: u64 = 2; // EIP-3860 (Shanghai == Durango)
const MAX_INIT_CODE_SIZE: usize = 49_152; // core/txpool/validation.go max-init-code check

#[derive(Debug, thiserror::Error)]
pub enum EvmMempoolError {
    /// coreth core/txpool/errors.go ErrAlreadyKnown.
    #[error("already known")]
    AlreadyKnown,
    /// coreth core/errors.go ErrNonceTooLow.
    #[error("nonce too low: address {address}, tx nonce {tx_nonce} < account nonce {account_nonce}")]
    NonceTooLow { address: Address, tx_nonce: u64, account_nonce: u64 },
    /// DIVERGENCE: coreth queues gapped txs; we reject (design §Non-goals).
    #[error("nonce gap: address {address}, tx nonce {tx_nonce} > next expected {expected}")]
    NonceGap { address: Address, tx_nonce: u64, expected: u64 },
    /// coreth core/errors.go ErrInsufficientFunds.
    #[error("insufficient funds for gas * price + value: balance {balance}, cost {cost}")]
    InsufficientFunds { balance: U256, cost: U256 },
    /// coreth core/errors.go ErrIntrinsicGas.
    #[error("intrinsic gas too low: gas {gas}, needed {needed}")]
    IntrinsicGasTooLow { gas: u64, needed: u64 },
    /// coreth internal/ethapi/api.go:1804-1807.
    #[error("only replay-protected (EIP-155) transactions allowed over RPC")]
    Unprotected,
    /// Chain-id mismatch against the node's chain.
    #[error("invalid chain id for signer: have {have}, want {want}")]
    WrongChainId { have: u64, want: u64 },
    /// coreth internal/ethapi/api.go:1801 checkTxFee.
    #[error("tx fee ({fee} wei) exceeds the configured cap ({cap} wei)")]
    FeeCapExceeded { fee: U256, cap: U256 },
    /// coreth core/txpool/errors.go ErrUnderpriced (tip floor, validation.go:133-134).
    #[error("transaction underpriced: tip {tip} < minimum {min}")]
    Underpriced { tip: u128, min: u128 },
    /// coreth core/txpool/errors.go ErrTxPoolOverflow.
    #[error("txpool is full")]
    PoolFull,
    /// Same-nonce replacement without a strictly higher fee cap
    /// (coreth legacypool ErrReplaceUnderpriced, simplified).
    #[error("replacement transaction underpriced")]
    ReplaceUnderpriced,
    /// coreth core/txpool/validation.go max-init-code-size (EIP-3860).
    #[error("max initcode size exceeded: {size} > {max}")]
    MaxInitCodeSize { size: usize, max: usize },
}

pub struct SenderAccount { pub nonce: u64, pub balance: U256 }

pub struct AdmissionRules {
    pub chain_id: u64,
    pub min_tip_wei: u128,
    pub tx_fee_cap_wei: U256,
    pub shanghai: bool,
}
// Default: chain_id from caller in prod; tests use the local harness id.
// min_tip_wei = 1 (coreth legacypool DefaultConfig.PriceLimit = 1),
// tx_fee_cap_wei = 10^18 (coreth eth/ethconfig RPCTxFeeCap default 1 unit).

struct PoolEntry { tx: RecoveredTx, arrival: u64 }

pub struct EvmMempool {
    max_size: usize,
    /// per-sender nonce-ordered txs.
    by_sender: HashMap<Address, BTreeMap<u64, PoolEntry>>,
    /// hash -> (sender, nonce) reverse index.
    by_hash: HashMap<B256, (Address, u64)>,
    arrival_seq: u64,
    notify: Arc<Notify>,
}
```

`add_local` validation ORDER (mirror the Go call sequence — api.go checks first, pool validation second): (1) already-known by hash; (2) EIP-155 protection: `ConsensusTx::chain_id(tx.inner())` — `None` on a legacy tx ⇒ `Unprotected`; `Some(id) != rules.chain_id` ⇒ `WrongChainId`; (3) `checkTxFee`: `fee = max_fee_per_gas * gas_limit` (U256 math) vs cap; (4) intrinsic gas (helper below); (5) EIP-3860 init-code size when `rules.shanghai` and the tx is a create; (6) fee-cap ≥ tip-cap (`validation.go:115` — for legacy txs both equal the gas price, vacuous); (7) tip floor: `max_priority_fee_per_gas().unwrap_or(max_fee_per_gas)` ≥ `min_tip_wei` (`validation.go:133-134`); (8) nonce vs `sender.nonce`, considering txs already pooled from that sender: too-low ⇒ `NonceTooLow`; the next expected nonce is `max(account nonce, highest pooled contiguous nonce + 1)` — same-nonce with strictly higher fee cap replaces (else `ReplaceUnderpriced`); a gap ⇒ `NonceGap`; (9) balance ≥ `value + max_fee_per_gas * gas_limit` (`validation.go:250-254`); (10) capacity: if `len() == max_size`, evict the pool-wide lowest-fee-cap entry if the incoming pays strictly more, else `PoolFull`. On admission: insert, bump `arrival_seq`, `self.notify.notify_one()` (the `AtomicMempool::add` precedent, `atomic/mempool.rs:342`), return hash.

Intrinsic gas (private helper, coreth `core/state_transition.go` `IntrinsicGas`):

```rust
fn intrinsic_gas(tx: &TransactionSigned, shanghai: bool) -> u64 {
    let input = ConsensusTx::input(tx);
    let is_create = ConsensusTx::kind(tx).is_create();
    let mut gas = if is_create { TX_GAS_CONTRACT_CREATION } else { TX_GAS };
    let nonzero = input.iter().filter(|b| **b != 0).count() as u64;
    let zero = input.len() as u64 - nonzero;
    gas = gas.saturating_add(nonzero.saturating_mul(TX_DATA_NON_ZERO_GAS_EIP2028));
    gas = gas.saturating_add(zero.saturating_mul(TX_DATA_ZERO_GAS));
    if is_create && shanghai {
        let words = (input.len() as u64).div_ceil(32);
        gas = gas.saturating_add(words.saturating_mul(INIT_CODE_WORD_GAS));
    }
    if let Some(al) = ConsensusTx::access_list(tx) {
        gas = gas.saturating_add((al.len() as u64).saturating_mul(ACCESS_LIST_ADDRESS_GAS));
        let keys: u64 = al.iter().map(|i| i.storage_keys.len() as u64).sum();
        gas = gas.saturating_add(keys.saturating_mul(ACCESS_LIST_STORAGE_KEY_GAS));
    }
    gas
}
```

(Adjust accessor names to the real `ConsensusTx`/alloy trait — the compiler holds you to it; the constants and formula are the requirement.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(mempool)'`
Expected: all admission tests PASS. Then the full suite: `./scripts/nix_run.sh cargo nextest run -p ava-evm` — 224+ green (nothing else touches the new module yet).

- [ ] **Step 5: Clippy + commit**

```bash
./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm
git commit -m "feat(M6.23): EvmMempool — coreth-parity admission validation, storage, eviction"
```

---

### Task 2: `EvmMempool` — `best_txs` ordering + accepted-block maintenance

**Files:**
- Modify: `crates/ava-evm/src/mempool.rs`

**Interfaces:**
- Produces (Task 5 relies on these exact names):
  - `pub fn best_txs(&self) -> Vec<RecoveredTx>` — contiguous-nonce runs per sender, merged across senders by descending fee cap (ties: earlier arrival first). DIVERGENCE note in-code: coreth orders by effective tip at the block base fee (`miner/ordering.go` `TransactionsByPriceAndNonce`); we order by fee cap because the base fee is computed inside `build_on`, whose `pack_evm_txs` re-filters affordability per tx (`builder.rs:313-330`) — for same-sender runs nonce order is preserved either way.
  - `pub fn on_block_accepted(&mut self, included: &[(Address, u64, B256)])` — for each `(sender, nonce, hash)`: drop the tx by hash AND drop every pooled tx from `sender` with nonce ≤ that nonce (sender-local stale eviction; DIVERGENCE note: coreth demotes via a state-driven pool reorg).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn best_txs_orders_across_senders_by_fee_cap_nonce_within_sender() {
    // sender A: nonces 0,1 at fee cap 5 gwei; sender B: nonce 0 at 10 gwei.
    // Expect [B0, A0, A1] — B first (higher cap), A's nonces in order.
}

#[test]
fn best_txs_stops_at_a_sender_nonce_gap() {
    // sender A holds nonces 0 and 2 (gap admitted impossible via add_local —
    // construct the gap by accepting nonce-1 externally? No: gaps cannot
    // exist by construction. Instead assert the contiguity INVARIANT:
    // after add_local of 0,1,2 best_txs returns all three in order.)
}

#[test]
fn on_block_accepted_drops_included_and_stale() {
    // Pool: A nonces 0,1,2. Block includes (A, 1, hash1).
    // Expect nonces 0 AND 1 gone (≤ included), nonce 2 retained.
}

#[test]
fn on_block_accepted_wakes_nothing_but_len_shrinks() {
    // Plain bookkeeping assertions: len()/is_empty()/contains() coherent after removal.
}
```

- [ ] **Step 2: Run to verify RED, implement, verify GREEN**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'test(mempool)'` (RED: methods absent → compile fail; then GREEN).

`best_txs` implementation shape: snapshot each sender's contiguous run from its lowest pooled nonce; repeatedly pick the sender whose HEAD tx has the highest `max_fee_per_gas` (tie → lower `arrival` first), emit it, advance that sender — a linear merge over a `BinaryHeap` keyed `(fee_cap, Reverse(arrival))`. Clone the `RecoveredTx`s out (the pool retains them until accept).

- [ ] **Step 3: Full suite + clippy + commit**

```bash
./scripts/nix_run.sh cargo nextest run -p ava-evm
./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm
git commit -m "feat(M6.23): EvmMempool best_txs price-and-nonce ordering + accepted-block maintenance"
```

---

### Task 3: Receipts — verify-time stash, accept-time persist + `AcceptedTxIndex`

The `RECEIPTS` column exists but `EvmBlock::accept` writes `&[]` today (`block.rs:995-1000`, "persisted once the receipt encoding is wired"). Verify HAS the receipts (`execute_batch` outcome) but drops them; accept must persist them. Use the existing warp-seam stash pattern (`block.rs:938-940` inserts verify-time artifacts keyed by pre-commit root into `ctx`; `block.rs:961-966` consumes them at accept).

**Files:**
- Create: `crates/ava-evm/src/receipts.rs` (the `AcceptedTxIndex` + receipt-record types + encoding helpers)
- Modify: `crates/ava-evm/src/block.rs` (stash at verify; persist + index at accept)
- Modify: `crates/ava-evm/src/vm.rs` (construct the index + stash in `EvmBlockContext`/`Shared`; expose `pub fn accepted_tx_index(&self)` like the existing `accepted_atomic_txs()` at `vm.rs:449`)
- Modify: `crates/ava-evm/src/canonical.rs` (a `tx_hash → block number` row: `put_tx_number`/`tx_number` using a new `prefix::TX_NUMBER`)
- Modify: `crates/ava-evm/src/lib.rs` (module decl)

**Interfaces:**
- Consumes: `EthReceipt` + `ReceiptWithBloom` facade re-exports (Task-2-of-parent added them; see `ava-evm-reth/src/lib.rs:42-44`), the warp-seam pattern in `block.rs`, `EvmBlockContext` (find its definition in `block.rs`/`vm.rs` — it already carries `canonical`, `state`, `warp`, `atomic_backend`).
- Produces (Task 4 relies on):
  - `pub struct TxReceiptRecord { pub tx_hash: B256, pub block_hash: B256, pub block_number: u64, pub tx_index: u64, pub from: Address, pub to: Option<Address>, pub contract_address: Option<Address>, pub gas_used: u64, pub cumulative_gas_used: u64, pub effective_gas_price: u128, pub success: bool, pub logs: Vec<Log>, pub tx_type: u8 }`
  - `pub struct AcceptedTxIndex` (interior-mutability like `AcceptedAtomicTxIndex`): `pub fn record(&self, records: Vec<TxReceiptRecord>)`, `pub fn get(&self, hash: &B256) -> Option<TxReceiptRecord>`
  - Persisted receipts encoding: each receipt as its EIP-2718 receipt envelope, the block's list RLP-encoded as `Vec<Bytes>`; `pub fn encode_block_receipts(receipts: &[EthReceipt]) -> Vec<u8>` + `pub fn decode_block_receipts(bytes: &[u8]) -> Result<Vec<ReceiptWithBloom>, Error>` with a round-trip unit test.

- [ ] **Step 1: Write the failing tests**

1. `receipts.rs` unit: `encode_block_receipts` → `decode_block_receipts` round-trips a 2-receipt list (one with logs, one without).
2. `receipts.rs` unit: `AcceptedTxIndex::record` + `get` round-trip; unknown hash → `None`.
3. Lifecycle test (extend the existing verify→accept test file — `grep -rln "verifiable_block1" crates/ava-evm/src/` found the fixtures in `block.rs`/`lifecycle.rs` tests during the parent run; use the same harness): after accepting a block with ≥1 EVM tx, assert (a) `canonical`'s receipts bytes at that height are non-empty and decode to the right count, (b) `canonical.tx_number(tx_hash) == Some(number)`, (c) `AcceptedTxIndex::get(tx_hash)` returns a record with correct `block_number`, `from`, `gas_used > 0`, `success == true`, `cumulative_gas_used ≥ gas_used`, `effective_gas_price ≥ base_fee`.

- [ ] **Step 2: RED, implement, GREEN**

Implementation notes:
- Verify path: where `EvmBlock::verify` stashes warp logs keyed by `precommit` (`block.rs:938-940`), also insert `outcome.result.receipts` (clone) into a new `ctx.receipts` stash: `Arc<Mutex<HashMap<B256, Vec<EthReceipt>>>>` field on `EvmBlockContext`, mirroring the warp seam's shape. Reject path (`block.rs:1005-1012`): remove the stash entry alongside the warp-log removal.
- Accept path (`block.rs:956-995`): take the stashed receipts; `encode_block_receipts` → pass to `append_canonical` instead of `&[]`; write `put_tx_number` per tx; build `TxReceiptRecord`s (per-tx `gas_used` = cumulative diff; `effective_gas_price` = `min(max_fee_per_gas, base_fee + max_priority_fee)` for dynamic txs, `gas_price` for legacy — mirror the standard effective-gas-price formula; `contract_address` = `from`+nonce derivation when `to` is create — alloy has a `create_address`/`from.create(nonce)` helper) and `ctx.accepted_tx_index.record(...)`. Missing stash (verify not run in this process) → persist `&[]` as today and skip indexing, with a `debug!` — never fail accept.
- `canonical.rs`: `prefix::TX_NUMBER` following the existing `prefix` module + `hash_key` helper (see `hash_key(prefix::NUMBER, ...)` at `canonical.rs:131`).

- [ ] **Step 3: Full suite + clippy + commit**

```bash
./scripts/nix_run.sh cargo nextest run -p ava-evm
./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm
git commit -m "feat(M6.24): receipts persisted at accept (verify-time stash) + AcceptedTxIndex + tx-hash→number rows"
```

---

### Task 4: RPC — `eth_sendRawTransaction` + `eth_getTransactionReceipt`

**Files:**
- Modify: `crates/ava-evm/src/rpc/eth.rs` (`EthRpc` gains the mempool + index handles; two new methods)
- Modify: `crates/ava-evm/src/rpc/service.rs` (dispatch rows)
- Modify: `crates/ava-evm/src/vm.rs` (`create_handlers` passes the new handles — `vm.rs:624-631` constructs `EthRpc::new(...)`)
- Test: `crates/ava-evm/tests/rpc_eth.rs` (extend the existing harness)

**Interfaces:**
- Consumes: `EvmMempool::{add_local, contains}` (Task 1), `AcceptedTxIndex::get` + `TxReceiptRecord` (Task 3), `TransactionSigned::decode_2718` + `try_into_recovered` (the exact pair `block.rs:1175` + `block.rs:421-426` use), `read_account`/`view_tip` (the pattern `eth.rs:223-227` uses for `eth_getTransactionCount`).
- Produces: `EthRpc::new(state, canonical, config, chain_id, mempool: Arc<Mutex<EvmMempool>>, tx_index: Arc<AcceptedTxIndex>)` — signature change; update ALL constructor call sites (`grep -rn "EthRpc::new" crates/`).

- [ ] **Step 1: Write the failing tests** (in `tests/rpc_eth.rs`, using its existing service harness)

```rust
#[test]
fn send_raw_transaction_admits_and_returns_hash() {
    // Serve a POST {"method":"eth_sendRawTransaction","params":["0x<rlp>"]}
    // over a service whose genesis funds the signer. Expect result == tx hash
    // and the mempool contains it.
}

#[test]
fn send_raw_transaction_maps_admission_errors() {
    // Same tx twice → second reply is a JSON-RPC error whose message
    // contains "already known". A nonce-5 tx from a nonce-0 account signed
    // sender → contains "nonce gap".
}

#[test]
fn get_transaction_receipt_null_when_unknown_then_served_after_accept() {
    // Unknown hash → result null (geth returns null, not an error).
    // Seed the AcceptedTxIndex with a TxReceiptRecord directly, then assert
    // the JSON shape: blockHash/blockNumber/transactionHash/transactionIndex/
    // from/to/gasUsed/cumulativeGasUsed/effectiveGasPrice/status/logs/
    // logsBloom/type/contractAddress — hex-quantity / hex-data encodings
    // matching the harness's existing `quantity(...)`/data helpers.
}
```

- [ ] **Step 2: RED, implement, GREEN**

`eth.rs`:

```rust
/// `eth_sendRawTransaction` — decode the EIP-2718 envelope, recover the
/// signer, admit to the EVM mempool (coreth internal/ethapi/api.go:1884-1890
/// SendRawTransaction → SubmitTransaction → txPool.Add).
pub fn send_raw_transaction(&self, raw: &[u8]) -> Result<Value> {
    let mut buf = raw;
    let tx = TransactionSigned::decode_2718(&mut buf).map_err(/* → Error decode variant */)?;
    let recovered = tx.try_into_recovered().map_err(/* invalid signature */)?;
    let sender = { // the eth_getTransactionCount read pattern (eth.rs:223-227)
        let view = self.state.view_tip()?;
        let acc = read_account(&view, &recovered.signer())?;
        SenderAccount {
            nonce: acc.as_ref().map_or(0, |a| a.nonce),
            balance: acc.as_ref().map_or(U256::ZERO, |a| a.balance),
        }
    };
    let rules = AdmissionRules { chain_id: self.chain_id, ..AdmissionRules::default() };
    let hash = self.mempool.lock().add_local(recovered, &sender, &rules)
        .map_err(/* map to crate Error carrying the sentinel message */)?;
    Ok(Value::String(format!("{hash:#x}")))
}
```

`eth_getTransactionReceipt`: `self.tx_index.get(&hash)` → `None` ⇒ `Ok(Value::Null)`; `Some(rec)` ⇒ the standard receipt object (build with the harness's quantity/data helpers; `logs` entries carry `address`/`topics`/`data`/`blockNumber`/`transactionHash`/`transactionIndex`/`blockHash`/`logIndex`/`removed:false`; `logsBloom` computed from the record's logs via the facade `Bloom` fold the parent Task 2 used, or store the bloom in the record — implementer's choice, say which). `service.rs` dispatch:

```rust
"eth_sendRawTransaction" => {
    let raw = data_param(params, 0)?; // 0x-hex bytes — add beside addr_param/b256_param if absent
    domain(self.eth.send_raw_transaction(&raw))
}
"eth_getTransactionReceipt" => {
    let hash = b256_param(params, 0)?;
    domain(self.eth.get_transaction_receipt(hash))
}
```

- [ ] **Step 3: Full suite + clippy + commit**

```bash
./scripts/nix_run.sh cargo nextest run -p ava-evm
./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm
git commit -m "feat(M6.23): eth_sendRawTransaction + eth_getTransactionReceipt over the EVM mempool + AcceptedTxIndex"
```

---

### Task 5: VM integration — pool feeds `build_block`, notify-driven wake, accept maintenance, end-to-end gate

**Files:**
- Modify: `crates/ava-evm/src/vm.rs` (`EvmVm` gains `evm_pool`; `wait_for_event`; `build_block`; `VerifiedEvmBlock::accept`)
- Test: extend the ChainVm-level lifecycle test file (locate: `grep -rln "wait_for_event\|build_block" crates/ava-evm/tests/` — the chainvm/lifecycle harness used by the M6 tests)

**Interfaces:**
- Consumes: `EvmMempool::{subscribe, is_empty, best_txs, on_block_accepted}` (Tasks 1-2), the existing `build_on` call site (`vm.rs:736-742`), `EvmBlock::parts()` transactions for included-hash extraction.
- Produces: the working pipeline the parent plan's Task 8 drives live.

- [ ] **Step 1: Pre-flight check (record in the report):** confirm the engine re-invokes `wait_for_event` after each event (read `crates/ava-engine/src/networking/engine_adapter.rs:343-344` and the loop that owns it). The current implementation parks on `token.cancelled()` ONLY (`vm.rs:674-685`) — a tx arriving after the park would never produce `PendingTxs` until shutdown. The fix in Step 3 (select on the pool notifies) is load-bearing for the live arm: submit → wake → build.

- [ ] **Step 2: Write the failing end-to-end test**

In the ChainVm harness (adapt its existing genesis/build/accept plumbing):

```rust
#[tokio::test]
async fn submitted_tx_flows_through_build_accept_receipt() {
    // 1. Boot the VM harness with a funded EOA in genesis.
    // 2. Admit a signed transfer via the vm's evm_pool handle (or the RPC
    //    service if the harness serves it — say which in the test comment).
    // 3. wait_for_event with a fresh token → returns PendingTxs WITHOUT
    //    cancellation (the notify path, bounded by tokio::time::timeout 5s).
    // 4. build_block → block whose parts().transactions contains the tx.
    // 5. The built block passes the FULL verify path (the Task-5-parent
    //    self-gate entry: verify(...) over the built bytes).
    // 6. accept → EvmMempool::is_empty() (maintenance ran), and
    //    AcceptedTxIndex::get(tx_hash) serves a record whose block_number
    //    == the accepted height.
}
```

- [ ] **Step 3: RED, implement, GREEN**

`vm.rs` changes:

1. `EvmVm` field `evm_pool: Arc<Mutex<EvmMempool>>` (size: 4096 — coreth legacypool default globalSlots order; cite loosely, exact capacity is not consensus); constructed in the same place `txpool`/`AtomicMempool` is; `Shared` gains the pool handle too (for accept maintenance) and `create_handlers` passes `Arc::clone(&self.evm_pool)` + `Arc::clone(&self.accepted_tx_index)` into `EthRpc::new` (Task 4's signature).
2. `wait_for_event` (replace `vm.rs:674-685`):

```rust
async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
    // PendingTxs when EITHER pool is non-empty; otherwise wake on the first
    // admission notify (either pool) or cancellation. The engine re-checks
    // emptiness after every event, so spurious wakes are harmless.
    let (atomic_notify, evm_notify) = {
        (self.txpool.lock().subscribe(), self.evm_pool.lock().subscribe())
    };
    let pending = !self.txpool.lock().is_empty() || !self.evm_pool.lock().is_empty();
    if pending {
        return Ok(VmEvent::PendingTxs);
    }
    tokio::select! {
        () = atomic_notify.notified() => {}
        () = evm_notify.notified() => {}
        () = token.cancelled() => {}
    }
    Ok(VmEvent::PendingTxs)
}
```

(Note `AtomicMempool::subscribe` exists at `atomic/mempool.rs:188` and admission already calls `notify_one` — this change gives the ATOMIC pool live wake too; mention that in the commit body.)

3. `build_block` (`vm.rs:736-742`): replace `Vec::new()` with the pool snapshot, and evict the failed batch on execution error:

```rust
let evm_candidates = self.evm_pool.lock().best_txs();
let had_candidates = !evm_candidates.is_empty();
match self
    .builder
    .build_on(&parent_header, parent_state_root, &ctx, evm_candidates)
{
    Ok(block) => Ok(self.wrap(block)),
    Err(Error::MissingProposal(_)) => Err(VmError::NotFound),
    Err(e) => {
        // Admission pre-validates nonce/balance/gas, so a batch-execution
        // failure is exceptional. Evict EVERY pooled candidate that was in
        // the failed batch (design §Component 3 — no per-tx bisection) so a
        // poisoned tx can never wedge block building, and say so loudly.
        if had_candidates {
            let stale: Vec<_> = /* (sender, nonce, hash) of the snapshot */;
            self.evm_pool.lock().on_block_accepted(&stale);
            tracing::warn!(error = %e, "build_on failed with EVM candidates; evicted the batch");
        }
        Err(VmError::from(e))
    }
}
```

(Snapshot `(signer, nonce, hash)` triples BEFORE moving the candidates into `build_on`.)

4. `VerifiedEvmBlock::accept` (`vm.rs:172-189`): after `self.block.accept(...)` succeeds, run pool maintenance:

```rust
let included: Vec<(Address, u64, B256)> = /* from self.block parts' recovered txs */;
self.shared.evm_pool.lock().on_block_accepted(&included);
```

(The parts hold `TransactionSigned`s; recover or carry signers — `EvmBlock::recover_senders()` at `block.rs:421-426` is the existing helper. If recovery here is awkward, extract `(sender, nonce, hash)` at verify time alongside the receipt stash and carry it in `ProcessingBlock` — implementer's choice, document it.)

- [ ] **Step 4: Full suite + clippy + commit**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm` (retry `-j1` if cross-test flakes) — ALL green including `built_block_passes_full_syntactic_verify` and `proposer_verdicts_hold`.
Also: `./scripts/nix_run.sh cargo nextest run -p ava-differential` (offline arms unaffected) and `./scripts/nix_run.sh cargo build -p ava-differential --features live --tests` (live arm still compiles).

```bash
./scripts/nix_run.sh cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm
git commit -m "feat(M6.23): build_block packs mempool txs; notify-driven PendingTxs; accept-time pool maintenance — tx pipeline end-to-end"
```

- [ ] **Step 5: Docs (same commit or a small follow-on):** update `plan/M6-cchain.md`'s M6.23 row — the "reth-txpool `best_transactions` integration" reading is retired in favor of the purpose-built `EvmMempool` (link the design doc); note receipts persistence (the old `&[]`) landed. Update the stale `vm.rs:736-737` comment (done implicitly by Step 3's edit) and the `builder.rs:165-167` doc line mentioning M6.23 (now true: the caller supplies pool candidates).

---

## Self-review notes (already applied)

- Spec coverage: Component 1 → Tasks 1-2; Component 2 → Tasks 3-4 (receipt persistence discovered to be missing at accept — spec's "already persisted" corrected to the stash-then-persist design); Component 3 → Task 5; Testing section → each task's tests + Task 5's end-to-end; Non-goals → Global Constraints DIVERGENCE list + Task 5 Step 5 docs.
- Ordering: Tasks 1-2 (pool, self-contained) → 3 (receipts, independent of pool) → 4 (RPC, needs 1+3) → 5 (VM, needs all). A reviewer can gate each independently.
- Type consistency: `add_local(tx, &SenderAccount, &AdmissionRules) -> Result<B256, EvmMempoolError>` (T1) is what T4 calls; `best_txs()` / `on_block_accepted(&[(Address, u64, B256)])` (T2) is what T5 calls; `TxReceiptRecord`/`AcceptedTxIndex::{record,get}` (T3) is what T4-5 use; `EthRpc::new` 6-arg signature (T4) is what T5's `create_handlers` passes.

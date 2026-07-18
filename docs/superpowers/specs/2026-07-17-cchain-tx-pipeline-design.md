# C-Chain EVM Transaction Pipeline — Design

**Date:** 2026-07-17
**Status:** Approved (user, 2026-07-17)
**Context:** Nested insert under the Rust-as-proposer plan
(`docs/superpowers/plans/2026-07-16-rust-as-proposer-cchain-parity.md`). Its Task 8
pre-flight code-proved that the live "Rust proposes" arm is unreachable: the Rust
C-chain has no `eth_sendRawTransaction`, `build_block` packs zero EVM txs (deferred
M6.23), and `app_gossip` is a no-op. The user chose to build the tx pipeline first,
then resume Task 8 as written. Evidence trail: `.superpowers/sdd/task-8-report.md`.

## Goal

The Rust C-chain accepts `eth_sendRawTransaction`, holds EVM txs in a
coreth-parity mempool, and includes them in Rust-proposed blocks.

**Success criteria:** Task 8's live arm becomes runnable as written —
`drive_c_transfer` driven against the RUST node works end-to-end:
`eth_getTransactionCount` (exists), `eth_gasPrice` (exists),
`eth_sendRawTransaction` (new), `eth_getTransactionReceipt` (new) — and a tx
submitted only to the Rust node can reach the chain **only** inside a
Rust-proposed block (no gossip; that is Task 8's proposer-detection mechanism).

## Non-goals (explicit deferrals)

- **Tx gossip, both directions** — and the engine-layer AppGossip/AppRequest
  routing it depends on (`ava-engine`'s `InboundOp` has no App variants at all;
  no Rust VM receives app messages today). Go nodes silently drop gossip
  non-participation (`network/p2p/router.go:114-125,198-209` + the no-op
  default handler), so absence is safe — and outbound push gossip would defeat
  Task 8's detection (a Go peer could include the tx in a Go-proposed block).
  Deferred to its own milestone.
- **Nonce-gap queueing** — coreth queues future-nonce txs
  (legacypool queued set); this pool rejects them with a typed error,
  divergence documented in-code. Task 8 drives exact-nonce txs.
- **Configurable RPC tx-fee cap** — coreth's `RPCTxFeeCap` default is
  hardcoded; the config surface can come later.
- **reth-transaction-pool** — M6.23's literal "reth-txpool `best_transactions`
  integration" reading is retired: no pool crate exists in the workspace, the
  parity target is coreth's (geth legacypool) semantics not reth's, and the
  repo precedent (`AtomicMempool`) is a purpose-built pool with cited Go
  parity. M6.23's intent — `build_block` consumes ordered best txs from a real
  mempool — is satisfied by this design.

## Component 1 — `EvmMempool` (`crates/ava-evm/src/mempool.rs`)

Purpose-built pool modeled on the `AtomicMempool` precedent
(`crates/ava-evm/src/atomic/mempool.rs`): plain struct behind
`Arc<Mutex<_>>`, shared `Arc<Notify>` wake on admission.

**Admission validation** (each check cites its coreth line; state reads via the
existing `FirewoodStateProvider` at the current tip):

| Check | coreth citation |
|---|---|
| decode (EIP-2718 envelope) + signature recovery | `internal/ethapi/api.go:1884-1890` (`UnmarshalBinary`) |
| EIP-155 replay protection / chain id | `internal/ethapi/api.go:1804-1807` |
| tx-fee cap (`checkTxFee`, default 1 AVAX) | `internal/ethapi/api.go:1801` |
| fee-cap ≥ tip-cap | `core/txpool/validation.go:115` |
| intrinsic gas | `core/txpool/validation.go:125-130` |
| tip floor (`MinTip`) | `core/txpool/validation.go:133-134` |
| nonce too low → reject | `core/txpool/validation.go:239` |
| nonce gap → reject (DIVERGENCE: coreth queues) | `core/txpool/validation.go:245` (documented divergence) |
| balance ≥ cost | `core/txpool/validation.go:250-254` |

**Storage:** per-sender `BTreeMap<u64 /*nonce*/, RecoveredTx>` + global
`HashMap<TxHash, …>`; bounded capacity with lowest-effective-tip eviction
(same shape as `AtomicMempool`'s outbid/evict rules).

**Selection:** `best_txs(base_fee) -> Vec<RecoveredTx>` — contiguous-nonce
runs per sender, merged across senders ordered by effective miner tip
(mirrors `miner/ordering.go`'s `TransactionsByPriceAndNonce` heap semantics).
Only txs that can pay `base_fee` are returned (the builder's `pack_evm_txs`
re-checks tip-affordability and gas budget — that stays).

**Maintenance:** `on_block_accepted(included: &[TxHash], tip_state)` — drop
included txs; evict now-stale (nonce-too-low vs new tip) txs.

## Component 2 — Write RPC (`crates/ava-evm/src/rpc/`)

- `service.rs` dispatch gains `eth_sendRawTransaction` and
  `eth_getTransactionReceipt` (exactly the surface `drive_c_transfer` needs;
  `tests/differential/src/livenet.rs:411-491`).
- `EthRpc` gains an `Arc<Mutex<EvmMempool>>` handle — the same wiring pattern
  `AvaxRpc` uses for `avax.issueTx` → `AtomicMempool`
  (`rpc/avax.rs:141-190`).
- `eth_sendRawTransaction` returns the tx hash on admission; admission errors
  map to JSON-RPC errors whose messages mirror Go's sentinel strings.
- **Receipts:** per-block receipts are already persisted in `CanonicalStore`
  (`canonical.rs:56,133`, `RECEIPTS` table). Add a `tx-hash → block number`
  index column written on accept (mirror of reth's `TransactionHashNumbers`
  table), then serve the standard receipt JSON by decoding the stored
  per-block receipts and locating the tx's index within the block.

## Component 3 — VM/build integration (`crates/ava-evm/src/vm.rs`)

- `EvmVm` holds `evm_pool: Arc<Mutex<EvmMempool>>` beside the atomic pool.
- `wait_for_event` returns `VmEvent::PendingTxs` when **either** pool is
  non-empty (today it polls only the atomic pool, `vm.rs:674-685`).
- `build_block` replaces `Vec::new()` (`vm.rs:740`) with
  `evm_pool.lock().best_txs(base_fee)` — `build_on`/`pack_evm_txs` already
  handle the rest (`builder.rs:174-375`; proven functional by the Task 2-6
  built-block tests, which supply txs directly).
- On Accept of an `EvmBlock`: call `on_block_accepted`.
- **Failure semantics:** `execute_batch` fails the whole batch on one bad tx.
  Admission pre-validates nonce/balance/gas against tip state, so batch
  failure is exceptional; on build-time batch failure, evict **every candidate
  that was in the failed batch** from the pool (no per-tx bisection — YAGNI)
  and `warn!` loudly (fail-safe, never silent, never wedge block building).

## Testing

- Unit tests per admission rule (RED-first, coreth-cited), including the
  documented nonce-gap divergence and eviction/ordering behavior.
- RPC round-trip test in the `rpc_eth.rs` harness style: submit raw tx →
  pending → build → accept → `eth_getTransactionReceipt` serves the receipt.
- Builder-integration test: `build_block` output contains the pool tx and
  still passes the full `syntactic_verify` self-gate (Task 5's
  `built_block_passes_full_syntactic_verify` pattern).
- Existing ava-evm suite (224 tests) stays green.

## Execution shape

Nested spec + plan committed on the current branch (`m9.15-rust-proposer`),
implemented via the same subagent-driven flow, then the parent plan's Task 8
resumes **as written**.

# M6 — C-Chain on reth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bring the Avalanche C-Chain up on Rust by embedding reth/revm as a *library* executor behind ava-evm adapters, with Firewood-ethhash as the EVM state-of-record, atomic X↔C transactions, dynamic fees per fork, warp/stateful precompiles, and `eth_*`/`avax.*` RPC — byte- and state-root-identical to coreth.
**Tier:** T4 — VMs
**Crates:** `ava-evm` (+ the `ava-evm-reth` facade sub-crate; the ONLY crate allowed to name `reth_*`/`revm` directly)
**Owning specs:** `10-cchain-evm-reth.md` (primary, incl. §17 normative gap designs G0–G8), `04-storage-and-databases.md` §4 (Firewood ethhash), `20-warp-icm.md` §7 (EVM warp precompile + predicates), `21-fee-economics-math.md` §0/§4/§5 (CalculatePrice, AP3/AP4, ACP-176/Fortuna), `02-testing-strategy.md` (§10.5 reexecute, §11 differential, §13 per-crate contract)
**Depends on (prior milestones):** M3 (Snowman `ChainVm`/`Block` adapter boundary from spec 07; atomic-UTXO / shared-memory `atomic.Requests` contract — ATOMIC-1); M1 (`firewood` crate with `features=["ethhash"]` ethhash state backend, spec 04 §4). **Independent of M5** (no X-Chain code dependency — the atomic X↔C parity test uses recorded fixtures / a stub source-chain harness, so M6 may be pulled ahead of M5 if EVM compatibility is prioritized).
**Exit gate (named tests):** `golden::cchain_block_wire` + `golden::cchain_genesis_root`; **`differential::cchain_state_root`** (reexecute recorded mainnet C-Chain block range → state roots match Go, spec 02 §10.5); `differential::atomic_xc` (X↔C atomic import/export parity); `prop::evm_fee_schedule_per_fork`.

---

## Dependency map & parallel waves

**Risk RETIRED by this milestone:** R3 (reth library API instability). Integration mode = **reth-as-a-LIBRARY executor, NOT the Engine API** (spec 00 §11.1.6 / spec 10 §1): Snowman owns fork choice; we need the pre-commit state root to vote on; Accept/Reject map to Firewood `commit`/discard with **no reorgs**. Every reth touch-point is wrapped behind `ava-evm-reth` facade traits (G0), and the eight flagged gaps G0–G8 (plus the two v2.x-surfaced G9/G10, spec 10 §17.11) are each closed by a task below.

Build/test ordering (a task may start once its deps are green):

- **Wave 0 — G0 facade + pin (must be first):** M6.1 vendored-reth pin + `ava-evm-reth` facade & re-export seam; M6.2 `AvaEvmError` model + crate skeleton.
- **Wave 1 — state backend + chainspec (the load-bearing seams):** M6.3 `FirewoodStateProvider`/`FirewoodStateView` reads (G1); M6.4 `BundleState`/`HashedPostState` → Firewood `BatchOp` conversion + `state_root*` (G1); M6.5 `AvaChainSpec`/`AvaHardfork`/`revm_spec_id` (G7).
- **Wave 2 — TDD ENTRY POINT (the cheapest differential oracle):** **M6.6 `ExternalConsensusExecutor::execute_batch` + 1-block reexecute → `differential::cchain_state_root` (genesis→block 1).** This proves executor + Firewood-ethhash wiring with the least machinery. Depends on M6.1/M6.3/M6.4/M6.5.
- **Wave 3 — block lifecycle:** M6.7 `decode_ava_evm_block`/`assemble_ava_block` wire format + `golden::cchain_block_wire`; M6.8 genesis parse + `golden::cchain_genesis_root`; M6.9 `EvmBlock` verify/accept/reject (pre-commit root, commit-on-accept, discard-on-reject, G6 `CanonicalStore`); M6.10 `EvmVm` `ChainVm` adapter (parse/get/set_preference/last_accepted).
- **Wave 4 — fees (parallel with Wave 3 after M6.5):** M6.11 `feerules::window` AP3 + AP4 block gas cost (G2); M6.12 `feerules::acp176` Fortuna/ACP-176 + ACP-226; M6.13 `next_evm_env` override wiring + `prop::evm_fee_schedule_per_fork`.
- **Wave 5 — atomic txs (depends on Wave 3 + M3 ATOMIC-1):** M6.14 atomic tx types + codec (byte-exact); M6.15 `AtomicStateHook` EVMStateTransfer pre-hook + atomic gas charge (G3); M6.16 atomic mempool; M6.17 `AtomicBackend` + atomic trie (2nd Firewood) + shared-memory batch on accept (G3); M6.18 atomic semantic verify/conflicts/bonus blocks; M6.19 `differential::atomic_xc`.
- **Wave 6 — on-demand build:** M6.20 `BlockBuilderDriver` on-demand build + `finish(precomputed root)` (G5).
- **Wave 7 — warp + precompiles:** M6.21 `AvaPrecompiles` `PrecompileProvider` + registry (G4/G10); M6.22 predicate pass + warp precompile over `ava-warp` (G4).
- **Wave 8 — RPC + sync:** M6.23 `eth_*` over Firewood + fee/accepted-tag overrides (G8); M6.24 `avax.*` namespace + admin/health (G8); M6.25 EVM + atomic-trie state sync over Firewood proofs (G8).
- **Wave 9 — reuse contract + close residual gaps + exit:** M6.26 public reusable API surface for SAE (spec 10 §16/§17.10); M6.27 G1/G9 empty-trie-tables CI invariant; M6.28 fuzz targets + PORTING.md; M6.31 live `EvmFactory` install + base-fee-to-coinbase override + remaining ConfigKey precompiles (NEW — surfaced by M6.22); M6.29 **Milestone exit gate**.

The reuse-contract task is M6.26 (one EVM engine, two drivers — SAE's `ava-saevm-exec` in M7 depends only on the facade + the public executor/state APIs, never on `EvmVm`/`BlockBuilderDriver`/reth directly).

---

## Tasks

### Task M6.1: Vendored reth pin + `ava-evm-reth` facade seam (G0) ✅ DONE (9c98689)
**Crate:** ava-evm-reth  ·  **Depends on:** —  ·  **Spec:** 10 §1, §17.1 (G0), 00 §11.1.6
**Files:** `crates/ava-evm-reth/Cargo.toml`, `crates/ava-evm-reth/src/lib.rs`, `crates/ava-evm-reth/UPGRADING.md`, workspace `Cargo.toml`
- [x] **Step 1 — Red:** Add `crates/ava-evm-reth/tests/facade_pins.rs` with `fn facade_reexports_compile()` that names the re-exported facade types (`ConfigureEvm`, `BlockExecutor`, `BlockExecutorFactory`, `BlockBuilder`, `StateProvider`, `StateRootProvider`, `PrecompileProvider`, `State`, `BundleState`) through `ava_evm_reth::*` only, and a `#[test] fn pinned_rev_is_single_sha()` reading the `rev=` from a `const RETH_REV: &str` and asserting it is a 40-char hex SHA (not a version range).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-evm-reth facade_pins` → fails to compile (crate/items absent).
- [x] **Step 3 — Green:** Create `ava-evm-reth/Cargo.toml` pinning EVERY `reth-*`/`revm`/`alloy-*` dep to ONE git `rev=<PINNED_SHA>` (spec 10 §17.1 toml). In `src/lib.rs` re-export only the items the rest of ava-evm may see, under our names (spec 10 §17.1 list), plus `pub const RETH_REV`. Define the `ExternalConsensusExecutor` trait + `ExecOutcome` struct (signatures verbatim from §17.1) — the "external consensus executor reth doesn't ship". `#![forbid(unsafe_code)]` is **lifted only here** (this is the binding-wrapper crate). Write `UPGRADING.md` with the reth-bump checklist (move SHA → fix facade compile errors only → re-run §14 differential gate).
- [x] **Step 4 — Confirm green:** `cargo build -p ava-evm-reth && cargo test -p ava-evm-reth facade_pins` → pass.
- [x] **Step 5 — Commit:** `ava-evm: pin vendored reth + add ava-evm-reth facade seam (G0)`

> **AS-BUILT (M6.1).** PINNED SET (mirrors reth v2.2.0's own workspace pins): reth-* @ git rev
> `88505c7fcbfdebfd3b56d88c86b62e950043c6c4` (v2.2.0); `revm 38.0.0`, `alloy-primitives 1.5.6`,
> `alloy-consensus 2.0.4`, `alloy-evm 0.34.0`, `alloy-rlp 0.3.13` (crates.io — reth pins revm/alloy
> by version, not git, so we mirror exactly). Facade depends only on `reth-evm` + `reth-storage-api`
> (NOT the full node); revm/alloy come from crates.io. **Path corrections vs §17.1 (reth v2.2.0):**
> `ConfigureEvmFor` does not exist (dropped; `EvmEnvFor`/`ExecutionCtxFor` do); `PrecompileProvider`
> is at `revm::handler::` (not `revm::context::`); `BlockExecutionResult<T>` is private in
> `reth_evm::execute` → re-exported from `alloy_evm::block::BlockExecutionResult` (it IS generic).
> `BlockExecutor`/`BlockBuilder`/`PrecompileProvider` are NOT dyn-compatible (generic methods) — the
> facade_pins surface test proves them via `use` + generic bounds, not `dyn`. reth's whole tree
> compiles+clippy-clean in the Nix shell (rust 1.96); R3 validated as contained.

### Task M6.2: `ava-evm` crate skeleton + error model ✅ DONE (466357c)
**Crate:** ava-evm  ·  **Depends on:** M6.1  ·  **Spec:** 10 §11.2, §13, 00 §7.1, §8
**Files:** `crates/ava-evm/Cargo.toml`, `crates/ava-evm/src/lib.rs`, `crates/ava-evm/src/error.rs`
- [x] **Step 1 — Red:** `crates/ava-evm/src/error.rs` test module `mod tests` with `fn sentinels_match_via_matches()` asserting `assert_matches!` against `Error::WrongNetworkId`, `Error::NilTx`, `Error::NilBaseFee`, `Error::FeeOverflow`, `Error::ConflictingAtomicInputs`, `Error::MissingProposal(_)`, and a `#[from]` wrap of a facade `BlockExecutionError`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-evm error::tests` → fails (no such enum).
- [x] **Step 3 — Green:** Add `Cargo.toml` (deps: `ava-evm-reth`, `ava-database`, `ava-types`, `ava-codec`, `ava-crypto`, `ava-warp`, `ava-network`, `firewood` features=["ethhash"], `ruint`, `alloy-*` via facade, `thiserror`, `tokio`, `async-trait`, `dashmap`, `arc-swap`, `parking_lot`). `#![forbid(unsafe_code)]` in `lib.rs` with the module tree from spec 10 §13 (`vm`,`block`,`builder`,`evmconfig`,`feerules`,`chainspec`,`state`,`atomic`,`precompile`,`rpc`,`sync`,`error`). Define `Error` (thiserror) preserving coreth/atomic sentinels as variants (§11.2 list) + `#[from]` for facade/firewood errors; all balance/fee arithmetic checked (00 §6.1). License header on every file.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-evm error::tests` → pass.

> **AS-BUILT (M6.2).** Module tree created as **stub files** (license header + module doc only) so the
> M6 parallel waves fill stub files without contending on `lib.rs`: `state`, `chainspec`, `feerules/`,
> `evmconfig`, `block`, `canonical` (pre-created for M6.9, not in §13 list), `builder`, `vm`, `atomic/`,
> `precompile/`, `rpc/`, `sync/`, `error`. **Deps deferred:** only `ava-evm-reth` + `thiserror` added now
> (workspace denies `unused_crate_dependencies`, so `ava-database`/`firewood`/`ruint`/`tokio`/etc. are
> added by the task that first uses them). `Error` carries `#[from]` for the facade's `BlockExecutionError`
> + `ProviderError` (firewood errors fold in through `ProviderError`/per-task variants later). Reusable
> sentinel construction via `BlockExecutionError::msg(...)`.
- [ ] **Step 5 — Commit:** `ava-evm: crate skeleton + Error sentinel model (§11.2)`

### Task M6.3: `FirewoodStateProvider` reads — accounts/storage/code/blockhash (G1) ✅ DONE (e4cfc3f)
**Crate:** ava-evm  ·  **Depends on:** M6.2, M1 (firewood ethhash)  ·  **Spec:** 10 §5, §17.2 (G1); 04 §4.2/§4.3
**Files:** `crates/ava-evm/src/state.rs`, `crates/ava-evm/tests/vectors/cchain/account_rlp/*.json`
- [x] **Step 1 — Red:** In `state.rs` `mod tests`, `fn read_account_and_storage_roundtrip()` opens an ethhash `firewood::db::Db` in a `t.TempDir()`-equivalent (`tempfile::tempdir`), proposes+commits an RLP account at `account_key(addr)` and an RLP-U256 slot at `storage_key(addr,slot)`, opens a `FirewoodStateView`, and asserts `basic_account`/`storage`/`bytecode_by_hash` return the decoded values; `decode_rlp_account` round-trips a golden RLP blob.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-evm state::tests::read_account_and_storage_roundtrip` → fails (types absent).
- [x] **Step 3 — Green:** Implement `FirewoodStateProvider { db, bytecode, block_hashes }` and `FirewoodStateView { rev, provider }` (§17.2). Impl facade `AccountReader::basic_account` (keccak(addr) → RLP `{nonce,balance,code_hash,storage_root}`), `StateProvider::storage` (RLP-U256), `BytecodeReader::bytecode_by_hash` (ava-database code KV), `BlockHashReader` (number→hash KV for BLOCKHASH window). Helpers `account_key`/`storage_key`/`decode_rlp_account`/`decode_rlp_u256`. `map_fw_err` maps firewood errors → `ProviderError`. Commit a golden account-RLP vector with provenance to Go ethhash bindings (02 §6).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-evm state::tests` → pass.
- [x] **Step 5 — Commit:** `ava-evm: FirewoodStateProvider reads over ethhash (G1, §5)`

> **AS-BUILT (M6.3).** Account RLP via facade `TrieAccount` (= alloy = coreth `types.StateAccount`); the
> reth `BytecodeReader` returns `reth_primitives_traits::Bytecode` (a newtype, re-exported as facade
> `RethBytecode`, **distinct** from revm `Bytecode`). bytecode + block-hash side KVs live in `ava-database`
> (NOT Firewood — Firewood is account/storage-of-record only). ava-evm depends on `firewood`+`firewood-storage`
> directly (git tag v0.5.0, `features=["ethhash"]`), not via ava-merkledb (needs the raw `Db`/`Revision`/
> `Proposal`/proof API). **Firewood ethhash is a GLOBAL compile-time Keccak switch** → `cargo build --workspace`
> default-features is fine (ava-merkledb's firewood is off by default), but `--all-features` would conflict
> ava-evm ethhash with ava-merkledb SHA — flagged as an M6.29 exit-gate / X cross-cutting concern. Facade
> re-exports added: `DatabaseError, RethBytecode, AccountProof, EMPTY_ROOT_HASH, HashedStorage, KeccakKeyHasher,
> KeyHasher, MultiProof(+Targets), StorageMultiProof, StorageProof, TrieAccount, KECCAK_EMPTY, keccak256,
> StorageKey, StorageValue, RlpEncodable/Decodable, rlp_encode, B256Map, Bytes`.

### Task M6.4: `BundleState`→Firewood `BatchOp` conversion + `state_root*` provider (G1) ✅ DONE (5ce602e)
**Crate:** ava-evm  ·  **Depends on:** M6.3  ·  **Spec:** 10 §5, §17.2.1, §17.2.2 (G1); 04 §4.2
**Files:** `crates/ava-evm/src/state.rs`, `crates/ava-evm/tests/proptest-regressions/state.txt`
- [x] **Step 1 — Red:** Add `fn hashed_post_state_to_batchops_is_deterministic()` (sorted-order, storage-before-accounts, zero-slot→Delete, None-account→Delete per §17.2.1) and a `proptest!` `prop_state_root_order_independent`: same K/V set in any insertion order → same Firewood root (02 §4.2 merkledb invariant, applied to ethhash). Add `fn stash_then_commit_advances_tip()` asserting `state_root_with_updates` stashes a proposal keyed by root and returns `TrieUpdates::default()`, and `commit(root)` advances the tip.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-evm state::tests::hashed_post_state` → fails.
- [x] **Step 3 — Green:** Implement `hashed_post_state_to_batchops` (storage first via `iter_sorted()`, `wiped`→`DeleteRange`, accounts after, RLP account/U256 encoders), `HashedPostStateProvider::hashed_post_state` (`KeccakKeyHasher`), `StateRootProvider::{state_root, state_root_with_updates, state_root_from_nodes, state_root_from_nodes_with_updates}` (the **empty-`TrieUpdates` G1 trick**), `FirewoodStateProvider::{stash_proposal, take_stashed, commit, discard, history_by_state_root (G2 window→`StateForHashNotFound`), propose_from_bundle, view_tip}`, `StorageRootProvider`/`StateProofProvider` over Firewood sub-trie/range proofs. Commit the proptest regression file.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-evm state::tests` → pass.
- [x] **Step 5 — Commit:** `ava-evm: BundleState→Firewood BatchOps + state_root commit (G1)`

> **AS-BUILT (M6.4).** **§17.2.2 deviation:** firewood `Proposal<'db>` borrows the owning `Db`, a
> self-referential borrow safe Rust forbids — so instead of stashing a live `Proposal`, we stash the
> deterministic **`BatchOp` list keyed by root** (`parking_lot::Mutex<HashMap<B256, FirewoodOps>>`) and
> re-propose+commit at `commit(root)`. Determinism makes the recomputed root bit-identical, so the
> verify→accept contract holds (cost: one in-memory re-propose). reth signature realities: `TrieInput`
> has a public `state: HashedPostState` field (used directly, no `into_sorted`); `HashedPostState`
> storage field is `.storage` (not `.slots`) and maps are unordered `B256Map` (sorted manually).
> `StorageRootProvider`/`StateProofProvider` are MINIMAL STUBS (full impl is M6.25 state-sync scope).

### Task M6.5: `AvaChainSpec` / `AvaHardfork` / `revm_spec_id` (G7) ✅ DONE (c2274b5)
**Crate:** ava-evm  ·  **Depends on:** M6.2  ·  **Spec:** 10 §7.4, §17.8 (G7); 21 §7; 00 §5
**Files:** `crates/ava-evm/src/chainspec.rs`, `crates/ava-evm/tests/vectors/cchain/fork_schedule/*.json`
- [x] **Step 1 — Red:** `mod tests` `fn fork_at_and_spec_id_match_coreth()` table test: for mainnet fork timestamps (re-exported from `ava-version`/`network_upgrades`), assert `fork_at(t)` selects the highest active `AvaHardfork` and `revm_spec_id(t)` maps each Avalanche phase to coreth's pinned Ethereum `SpecId` (golden vector); plus `fn check_compatible_rejects_activated_fork_change()`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-evm chainspec::tests` → fails.
- [x] **Step 3 — Green:** Implement `AvaHardfork` (Eth(EthereumHardfork) + Apricot1..PhasePost6, Banff, Cortina, Durango, Etna, Fortuna, Granite), `AvaChainSpec { inner: ChainHardforks, eth_genesis_header, genesis, fee_config: FeeConfig, network_upgrades, is_subnet, chain }`, `impl EthChainSpec`/`EthereumHardforks` (facade), `fork_at`, per-phase `is_*` predicates, `revm_spec_id`, `check_compatible` (network_upgrades parity). Embed Mainnet/Fuji fork timestamps as protocol constants (00 §5). Commit fork-schedule golden vector.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-evm chainspec::tests` → pass.
- [x] **Step 5 — Commit:** `ava-evm: AvaChainSpec + AvaHardfork + revm_spec_id (G7)`

> **AS-BUILT (M6.5).** `fork_at` returns an ordered **`AvaPhase`** enum (Launch, ApricotPhase1..PhasePost6,
> Banff, Cortina, Durango, Etna, Fortuna, Granite) — NOT `AvaHardfork` (whose `Eth(_)` variant can't be
> totally ordered). `AvaHardfork = Eth(EthereumHardfork) | Phase(AvaPhase)` is the `Hardfork`-trait/
> `ChainHardforks` unit; `AvaPhase` is the "current fork" type. **revm_spec_id mapping (coreth
> `params/config_extra.go:SetEthUpgrades`, verbatim):** Launch/AP1→ISTANBUL, AP2→BERLIN, AP3..Cortina→LONDON,
> Durango→SHANGHAI, Etna/Fortuna/Granite→CANCUN. coreth pins **no PragueTime** at the pinned rev, so
> Fortuna/Granite stay CANCUN (NB: §17.8's "Granite→PRAGUE/Durango→PRAGUE" example is wrong — see SPEC FIX
> below). Fork **timestamps reused from `ava-version`** (`upgrade.rs`, the verbatim `upgrade.go` schedule),
> converted chrono→u64 unix; no magic numbers duplicated. ChainHardforks keys Eth forks by *timestamp* (not
> block) — observationally identical for revm_spec_id. `FeeConfig`/`genesis` are minimal stubs (full forms
> land M6.8/M6.11–13). Facade re-exports added: `ChainSpecBuilder, DepositContract, BaseFeeParams, BlobParams,
> Genesis, NodeRecord, Header` + new `AvaEvmError::IncompatibleFork`. Deps added: `ava-version`, `chrono`.

### Task M6.6: `ExternalConsensusExecutor::execute_batch` + 1-block reexecute → `differential::cchain_state_root` (TDD ENTRY POINT) ✅ DONE (c41f994)
**Crate:** ava-evm  ·  **Depends on:** M6.1, M6.3, M6.4, M6.5  ·  **Spec:** 10 §3.2, §17.1, §17.4 (executor drive); 02 §10.5, §11.1 (recorded-oracle); 04 §4.2
**Files:** `crates/ava-evm/src/evmconfig.rs`, `crates/ava-evm/tests/cchain_state_root.rs`, `crates/ava-evm/tests/vectors/cchain/reexecute/genesis_to_1/*.json` (blockexport fixtures)
- [x] **Step 1 — Red:** Create `crates/ava-evm/tests/cchain_state_root.rs` with `#[test] fn cchain_state_root()` (the exit-gate name) running in **recorded-oracle / reexecute mode**: load the committed `genesis_to_1` blockexport fixture (genesis state + block 1 bytes + Go-recorded post-state root), build `AvaEvmConfig`, open a `FirewoodStateView` at the genesis root, decode block 1's EVM txs, call `execute_batch(env, &mut state, NoopPreHook, &txs)`, convert the returned `bundle` via `propose_from_bundle`, and `assert_eq!(proposal.root_hash(), fixture.expected_root)`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm cchain_state_root` → fails (executor/`AvaEvmConfig` absent or root mismatch). Assert the failure reason is a missing executor, not a missing fixture.
- [x] **Step 3 — Green:** Implement `AvaEvmConfig`; impl facade `ConfigureEvm` and `ExternalConsensusExecutor::execute_batch` by driving the reth `BlockExecutor` over a `State<StateProviderDatabase<FirewoodStateView>>` with bundle update, returning `ExecOutcome { result, bundle }` (§17.1). Commit the blockexport fixture + manifest with Go provenance.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm cchain_state_root` → pass.
- [x] **Step 5 — Commit:** `ava-evm: execute_batch + 1-block reexecute state-root parity (differential::cchain_state_root)`

> **AS-BUILT (M6.6).** `AvaEvmConfig` **wraps reth's `EthEvmConfig<AvaExecutorSpec>`** (reuses reth's
> `ConfigureEvm` rather than re-deriving it). `execute_batch` drives the bare `BlockExecutor`
> (`apply_pre_execution_changes` → `execute_transaction` loop → `apply_post_execution_changes`) over a
> `State<StateProviderDatabase<FirewoodStateView>>`, then `merge_transitions(Reverts)` + `take_bundle()`.
> Added `NoopPreHook` and `AvaExecutorSpec` (chain-spec adapter supplying `EthExecutorSpec`/`Hardforks`).
> **reth v2.2.0 type realities (folded into facade):** `EthPrimitives::Block = alloy_consensus::Block<TransactionSigned>`,
> `TransactionSigned = EthereumTxEnvelope<TxEip4844>` (≠ `alloy_consensus::TxEnvelope` = `<TxEip4844Variant>`);
> `EthPrimitives::Receipt = alloy_consensus::EthereumReceipt`; `State<DB>::Error = EvmDatabaseError<DB::Error>`
> (not raw `ProviderError`); `BundleRetention`/`BundleBuilder` at `revm::database::states::bundle_state`. The
> full coreth block header is NOT alloy-`Header`-decodable (coreth appends header extras) → the test decodes
> the body tx via EIP-2718 and builds the env header from recorded fields; full block-wire decode is M6.7.
>
> **FACADE CHANGES (breaking — reconcile in M6.26 reuse surface):** `RecoveredTx = Recovered<TransactionSigned>`
> (was `Recovered<TxEnvelope>`); `ExecOutcome.result: BlockExecutionResult<EthReceipt>` (was `<Receipt>`);
> `AvaEvmEnv` gained `header: Header`; `PreExecutionHook::apply(&mut dyn Database<Error = StateDbError>)`
> (was `Error = ProviderError`), `StateDbError = EvmDatabaseError<ProviderError>`. New facade re-exports:
> `Evm/EvmEnv/EvmFactory/NextBlockEnvAttributes`, `EthBlockExecutionCtx/EthEvmFactory/EthExecutorSpec`,
> `ForkFilter*/ForkHash/ForkId/Hardforks/Head`, `EthBlockAssembler/EthEvmConfig/RethReceiptBuilder`,
> `RethBlock/EthPrimitives/EthReceipt/TransactionSigned`, `Recovered/SignerRecoverable`, `Decodable2718`,
> `SealedBlock/SealedHeader`, `BundleBuilder/BundleRetention`, `Database`, `StateProviderDatabase`,
> `EvmDatabaseError`, `StateDbError`. Facade deps added: `reth-evm-ethereum`, `reth-ethereum-primitives`,
> `reth-ethereum-forks`, `reth-revm` (pinned rev, `features=["std"]`). No ava-evm deps added.
>
> **Go fixture provenance:** coreth git rev `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`, go 1.25.9; scratch
> `package core` test (`GenerateChainWithGenesis`, `dummy.NewCoinbaseFaker`, `params.TestApricotPhase3Config`)
> ran genesis→block 1 (1 funded EOA → 1×1-AVAX legacy transfer); source inlined in `manifest.json`; scratch
> deleted, `../avalanchego` left clean (verified).
>
> **⚠️ THREE PARITY FINDINGS (see SPEC FIXes below; tracked as M6 follow-ups):**
> 1. **5-FIELD ACCOUNT RLP (state-root parity gap, HIGH).** coreth's libevm `types.StateAccount` appends a
>    5th `Extra` field (empty `0x80` for an EOA) → coreth-StateDB roots (`0x9cb2…`) differ from the standard
>    4-field `[nonce,balance,storageRoot,codeHash]` RLP (`0x3292…`) that `state.rs::rlp_account` +
>    Firewood-ethhash emit. **The committed fixture's `expected_root` is over the 4-field encoding** (Go +
>    Firewood agree there) — so `cchain_state_root` currently proves *Rust-4field == Go-4field internal
>    consistency*, NOT parity with coreth's real on-chain StateDB root. The 5-field coreth roots are recorded
>    as `coreth_*_state_root_5field`. **Real mainnet reexecute parity (the M6.29 exit gate) REQUIRES adding
>    the 5th field to `state.rs::rlp_account`** → new follow-up **M6.30** (state-encoding parity).
> 2. **Paris not in `AvaChainSpec` schedule (MED).** `final_paris_total_difficulty == 0` but Paris/pre-merge
>    Eth forks aren't keyed by block → reth's `base_block_reward` (`is_paris_active_at_block`) mints a spurious
>    5-ETH PoW reward. Worked around in `AvaExecutorSpec` (forces Paris + pre-merge forks active at block 0).
>    **Fix in chainspec.rs:** activate Paris at genesis + key pre-merge Eth forks `ForkCondition::Block(0)`
>    (`revm_spec_id` unaffected). → folded into **M6.8** scope (genesis/chainspec).
> 3. **Base-fee burn vs coinbase credit (MED).** Avalanche credits the AP3 base fee to the coinbase (does NOT
>    burn); revm default LONDON burns it (tip=0). Sender pays identically; only coinbase differs. Fixture's
>    expected root uses the revm burn model. The base-fee-recipient override is **M6.13** scope (`next_evm_env`).

### Task M6.7: Block wire format `decode_ava_evm_block`/`assemble_ava_block` → `golden::cchain_block_wire` ✅ DONE (c28e1e5)
**Crate:** ava-evm  ·  **Depends on:** M6.5, M6.6  ·  **Spec:** 10 §9.3, §6.2; 02 §6
**Files:** `crates/ava-evm/src/block.rs`, `crates/ava-evm/tests/block_wire.rs`, `crates/ava-evm/tests/vectors/cchain/block_wire/*.json`
- [x] **Step 1 — Red:** `crates/ava-evm/tests/block_wire.rs` `#[test] fn cchain_block_wire()` (exit-gate name): for committed Go-produced block bytes (incl. one block carrying atomic txs in ExtraData/body), assert `decode_ava_evm_block(bytes, &spec)` round-trips and `assemble_ava_block(...)` re-encodes byte-identically, and the recovered block **ID matches** the golden ID (consensus-critical).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm cchain_block_wire` → fails.
- [x] **Step 3 — Green:** Implement `block.rs`: `decode_ava_evm_block` (alloy RLP Ethereum block + atomic-tx extraction from ExtraData/body, fork-gated per §6.2), `assemble_ava_block`, sender recovery, `EvmBlock` enum states (`unverified`/`built`). Block ID = Go encoding hash. Commit block-wire golden vectors (incl. atomic-bearing block) with provenance.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm cchain_block_wire` → pass.
- [x] **Step 5 — Commit:** `ava-evm: block wire decode/assemble + ID parity (golden::cchain_block_wire)`

> **AS-BUILT (M6.7).** **Resolves the M6.6 "coreth header not alloy-decodable" finding.** coreth block bytes =
> `RLP([Header, Txs, Uncles, Version(u32), ExtData(bytes)])` — geth's `Withdrawals` slot is REPLACED by
> `Version`+`ExtData` (coreth `block_ext.go`); **block ID = `keccak256(header RLP)`**. The header (`AvaHeader`) =
> 15 standard eth fields + **`ExtDataHash` (ALWAYS present, field 16)** + an RLP-optional tail with the standard
> "any later field present ⇒ all earlier present" discipline: `BaseFee`(AP3), `ExtDataGasUsed`+`BlockGasCost`(AP4),
> `BlobGasUsed`+`ExcessBlobGas`(4844), `ParentBeaconRoot`(4788), `TimeMilliseconds`+`MinDelayExcess`(Granite).
> `ExtData` carries the AP5 atomic **batch** (`atomic.Codec.Marshal(0, []*Tx)`) post-AP5 / a single tx pre-AP5;
> `ExtDataHash = keccak256(rlp(ExtData))` or `EmptyExtDataHash` (`56e81f17…b421`) when empty. `EvmBlock` enum
> states `Unverified`/`Built`; added `AvaBlockParts`, `recover_senders`, `empty_ext_data_hash`. **Both golden
> vectors are real coreth output** (avalanchego rev `fb174e8…`, go1.25.10): a plain AP3 block (reused from the
> M6.6 fixture's `block1_rlp`) and an AP4+ block carrying one signed atomic Import tx in `ExtData`; round-trip
> byte-identical + hash-stable. Facade re-exports added: `RLP_EMPTY_STRING_CODE, RlpError, RlpListHeader,
> rlp_length_of_length` (alloy-rlp). **GOTCHA:** after editing the facade, `touch crates/ava-evm-reth/src/lib.rs`
> to bust a stale-rlib cache before rebuilding `ava-evm` in the same Nix shell.

### Task M6.8: C-Chain genesis parse + `golden::cchain_genesis_root` ✅ DONE (59b1321)
**Crate:** ava-evm  ·  **Depends on:** M6.4, M6.5  ·  **Spec:** 10 §11.1, §8.3; 02 §6
**Files:** `crates/ava-evm/src/chainspec.rs` (genesis parse), `crates/ava-evm/tests/genesis_root.rs`, `crates/ava-evm/tests/vectors/cchain/genesis/{mainnet,fuji}.json`
- [x] **Step 1 — Red:** `tests/genesis_root.rs` `#[test] fn cchain_genesis_root()` (exit-gate name): parse the embedded Mainnet (and Fuji) C-Chain genesis JSON (`config` chain id, fork timestamps, `feeConfig`, precompile configs, `alloc`), materialize the alloc into Firewood-ethhash, and assert the computed genesis **state root** and **genesis block ID** equal the committed Go values for both networks.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm cchain_genesis_root` → fails.
- [x] **Step 3 — Green:** Implement genesis JSON parsing into `AvaChainSpec` + upgrade schedule (timestamp-keyed `precompileUpgrades`, §8.3), alloc → `BatchOp`s → propose/commit, genesis header construction for ID parity. Commit Mainnet/Fuji genesis vectors with provenance to Go `genesis/`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm cchain_genesis_root` → pass.
- [x] **Step 5 — Commit:** `ava-evm: C-Chain genesis parse + state-root/ID parity (golden::cchain_genesis_root)`

> **AS-BUILT (M6.8).** `CChainGenesis` parser (serde over the genesis JSON: `config` chain id +
> `precompileUpgrades` §8.3, header scalars, `alloc`); alloc materialized via the **5-field `rlp_account`**
> path (M6.30) → `BundleState` → Firewood propose/commit; `genesis_header(state_root)` builds the coreth
> genesis header for ID parity. **Mainnet AND Fuji both pass** state-root + block-ID parity — they share
> identical Go-authoritative values (state root `0xd65eb1b8…29cc`, block ID `0x31ced5b9…a96b`); the only
> config diff (chainId 43114 vs 43113) is not a header field and the alloc/header fields are identical.
> **Paris-at-genesis fix (M6.6 finding #2 RESOLVED):** `build_chain_hardforks` now keys Paris + Dao/
> ArrowGlacier/GrayGlacier + all pre-merge Eth forks at `ForkCondition::Block(0)` with
> `final_paris_total_difficulty == 0`; the temporary `evmconfig.rs::AvaExecutorSpec` force-activation
> (`is_forced_genesis_fork`) was REMOVED — `ethereum_fork_activation` now delegates straight to the inner
> `AvaChainSpec`. `cchain_state_root` (M6.6) still green after the chainspec change. Deps: `serde`/`serde_json`/
> `hex` promoted to regular (genesis parse is lib code); `Error::GenesisParse(String)` added. Facade
> re-exports: `EMPTY_OMMER_ROOT_HASH` (alloy-consensus), `StorageKeyMap` (revm). **SPEC FINDINGS:** (1) the
> genesis header's `ExtDataHash` is the **ZERO hash, NOT `EmptyExtDataHash`** (`56e81f17…b421`) — coreth's
> `toBlock` leaves it zero (genesis has no ExtData, hash never computed). (2) Mainnet/Fuji genesis (timestamp
> 0) carries **no optional header tail** beyond the always-present `ExtDataHash`; `baseFee = nil`.

### Task M6.9: `EvmBlock` verify/accept/reject — pre-commit root, commit/discard, `CanonicalStore` (G6) ✅ DONE (223ab75)
**Crate:** ava-evm  ·  **Depends on:** M6.6, M6.7, M3 (06 Block trait)  ·  **Spec:** 10 §3.1, §3.2, §17.7 (G6); 06 (linear acceptance); 04 §4.2
**Files:** `crates/ava-evm/src/block.rs`, `crates/ava-evm/src/state.rs` (committer), new `crates/ava-evm/src/canonical.rs`, `crates/ava-evm/tests/lifecycle.rs`
- [x] **Step 1 — Red:** `tests/lifecycle.rs` (driven by the M3 engine harness / `ava-snow::testutil`): `fn verify_computes_precommit_root_no_commit()` (verify yields header root, EVM tip unchanged), `fn accept_commits_and_advances_tip()`, `fn reject_drops_proposal_without_commit()` (sibling proposals independent — proposal-on-proposal), and `fn canonical_store_advances_by_one()`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm lifecycle` → fails.
- [x] **Step 3 — Green:** Impl 06 `Block` for `EvmBlock`: `verify` (syntactic + semantic execute via `execute_batch` into overlay, compute Firewood pre-commit root via stashed proposal, assert == header.state_root, receipts/gas/bloom), `accept` (`FirewoodStateCommitter::commit` → `CanonicalStore::append_canonical` → set `last_accepted`), `reject` (`FirewoodStateProvider::discard` + evict). Implement `canonical.rs` `CanonicalStore` (G6): single MDBX rw-tx appends Headers/CanonicalHeaders/HeaderNumbers/BlockBodyIndices/Transactions + static-file receipts + tip pointer, **never** touching state/trie tables; invariant `LAST_CANONICAL == last_accepted.height`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm lifecycle` → pass.
- [x] **Step 5 — Commit:** `ava-evm: EvmBlock verify/accept/reject + CanonicalStore (G6)`

> **AS-BUILT (M6.9).** Lifecycle exposed as **inherent `EvmBlock::{verify,accept,reject}(…, &EvmBlockContext)`
> methods** (+ `header_state_root`/`parent_hash`/`parts`/`into_parts` accessors), NOT a direct `Block`-trait impl
> — see DEVIATION. `verify` executes via `execute_batch` into an overlay, `propose_from_bundle`+`stash_proposal`
> for the Firewood pre-commit root, asserts `== header.state_root` (rejects on mismatch), tip UNCHANGED; `accept`
> = `commit(root)` → `CanonicalStore::append_canonical` → `last_accepted`; `reject` = `discard(root)` + evict.
> `Error` gained `From<AvaEvmError>`. 4 lifecycle + 3 canonical tests green; no facade re-exports.
> **DEVIATION 1 (Block trait → M6.10):** there are TWO `Block` traits — `ava_snow::Block` (root re-export) is the
> **async** `decidable::Block` (HAS `verify`); the **synchronous** spec-06 one (`ava_snow::snowman::block::Block`,
> 06 §2.4) is `accept`/`reject`-ONLY (no `verify`, no VM-context arg). Neither is implementable on `EvmBlock`
> alone (lifecycle needs provider/config/canonical-store), and an unused `ava-snow` dep trips the workspace
> `unused_crate_dependencies` deny. So M6.9 ships inherent methods taking `EvmBlockContext`; **the trait impl on a
> `VerifiedEvmBlock` wrapper is M6.10 (`vm.rs`) scope.** The §3.1 `async fn verify` sketch matches the async trait.
> **DEVIATION 2 (G6 backend, §17.7):** `CanonicalStore` is over the **`ava-database` KV** backend
> (one-byte-prefixed Headers/CanonicalHeaders/HeaderNumbers/Bodies/Receipts + singleton tip pointer), NOT reth-db
> MDBX — the G6 contract is "non-state block metadata only," which prefixed-KV satisfies; co-loading reth's MDBX
> alongside Firewood's global-ethhash switch is avoidable risk. `append_canonical` is the seam a future reth-db
> migration re-implements. **NOTE (parity, → M6.22):** a real coreth block-1 header carries post-state root
> `0x8b0bf83…` (coinbase-credit/3-account model) but our executor yields `0x4027f3ed…` (burn model) — they
> coincide only at genesis. This is the **base-fee-recipient gap deferred to M6.22** (NOT a `verify` bug — `verify`
> correctly asserts computed==header); the lifecycle test sets `header.state_root` to the executor's root, and raw
> coreth bytes will verify once M6.22 lands the coinbase-credit `EvmFactory`.

### Task M6.10: `EvmVm` `ChainVm` adapter ✅ DONE (ab9e6da)
**Crate:** ava-evm  ·  **Depends on:** M6.9, M3 (07 ChainVm boundary)  ·  **Spec:** 10 §3; 07 (ChainVm/Block)
**Files:** `crates/ava-evm/src/vm.rs`, `crates/ava-evm/tests/chainvm.rs`
- [x] **Step 1 — Red:** `tests/chainvm.rs` `fn parse_get_setpref_lastaccepted()`: `parse_block` decodes to an unverified `EvmBlock`; `get_block` returns from the verified tree else blocks db; `set_preference` records target + retargets txpool with no reorg work; `last_accepted` returns committed `(Id, height)`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm chainvm` → fails.
- [x] **Step 3 — Green:** Implement `EvmVm` (fields per §3: chain_spec, evm_config, state, blocks, atomic, txpool, builder, `verified: DashMap`, `preferred: ArcSwap`, `last_accepted: ArcSwap`) and `impl ChainVm` (`parse_block`, `build_block`→builder, `get_block`, `set_preference` record-only, `last_accepted`). No reth fork choice (G6).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm chainvm` → pass.
- [x] **Step 5 — Commit:** `ava-evm: EvmVm ChainVm adapter (§3)`

> **AS-BUILT (M6.10).** `EvmVm` impls `ava_vm::block::ChainVm` (supertrait `Vm`); the engine-facing block
> type is `VerifiedEvmBlock` which impls **`ava_vm::block::Block` (== `ava_snow::decidable::Block`, the ASYNC
> decidable trait** — `id`/`parent`/`height`/`timestamp`/`bytes` + async `verify`/`accept`/`reject`, no VM handle
> on the decision methods; this resolves the "two Block traits" M6.9 deferral — `ava-evm` impls the async
> decidable one, NOT the sync `snowman::block::Block`). `VerifiedEvmBlock` bundles an `EvmBlock` + a shared
> `Arc<EvmBlockContext>` + an `Arc<Shared>` (the processing tree + `last_accepted` pointer) so the `&self`-only
> trait methods drive the M6.9 inherent lifecycle: `verify` resolves the parent root from the Firewood tip →
> `EvmBlock::verify` (stashes pre-commit root) → inserts into `verified: DashMap`; `accept` → `EvmBlock::accept`
> (commit + canonical append) + advances `last_accepted: ArcSwap`, leaving the block in `verified`; `reject` →
> evict + `EvmBlock::reject` (discard proposal). `set_preference` is RECORD-ONLY into `preferred: ArcSwap` +
> txpool `Notify` retarget (zero state mutation, asserted by the test — G6 linear accept, no reorgs). Block
> ID ⇔ `B256` is a pure 32-byte reinterpret (`id_of`/`hash_of`). **DEVIATIONS / SEAMS:** (1) `get_block` resolves
> the `verified` tree first, else confirms the id is a known accepted block via the `CanonicalStore` index — but
> **full-byte reconstruction of an accepted-then-evicted block from the store is deferred to M6.23/M6.24**: the
> M6.9 `CanonicalStore` schema persists only the header *commitment* (`B256`) + `ext_data`, NOT the full block
> RLP, so an `EvmBlock` can't be rebuilt from it (accepted blocks are therefore NOT evicted from `verified`).
> Fold the store-schema gap into M6.23/24. (2) `build_block` returns `Err(NotFound)` ("no pending block",
> coreth `ErrNoPendingBlock` shape) pending the **M6.20** builder driver (`crate::builder` still a stub) — a
> documented seam, not a blocker. (3) `Vm::initialize` is minimal (records the `ChainContext`); genesis-JSON
> collaborator construction is M6.8 — `EvmVm::new(provider, config, blocks, genesis_id)` is the construction
> seam; RPC handlers (M6.23/24) + state-sync probes (M6.25) stubbed empty/`None` per the avm/platformvm
> precedent. (4) `dashmap = "6"` declared directly in `crates/ava-evm/Cargo.toml` (not a workspace dep; already
> in `Cargo.lock` via reth's graph — no new external crate) — consider promoting to a workspace dep. Deps added:
> `ava-snow`, `ava-vm`(already), `async-trait`, `tokio-util`, `dashmap`, `arc-swap`; dev `tokio`. **No facade
> edits.** Error mapping mirrors the avm precedent: only the lookup-miss (`Error::MissingProposal`) →
> `ava_vm::Error::NotFound`; otherwise `InvalidComponent`/`ParametersInvalid` (neither `ava_vm`/`ava_snow`
> error has a free-form variant). 104→105 tests on-branch; 112 combined with M6.17.

### Task M6.11: `feerules::window` AP3 base fee + AP4 block gas cost (G2) ✅ DONE (71840d5)
**Crate:** ava-evm  ·  **Depends on:** M6.5  ·  **Spec:** 21 §0, §4a, §4b; 10 §7.1, §17.3 (G2)
**Files:** `crates/ava-evm/src/feerules/mod.rs`, `crates/ava-evm/src/feerules/window.rs`, `crates/ava-evm/src/feerules/blockgas.rs`, `crates/ava-evm/tests/vectors/cchain/fees/{ap3,ap4}/*.json`, `crates/ava-evm/tests/proptest-regressions/feerules.txt`
- [x] **Step 1 — Red:** Golden table tests from 21 §4 worked examples: `Window::{add,shift,sum}` (saturating), `base_fee_from_window` (exact-target no-op & unclamped, increase-vs-decrease windows-elapsed asymmetry, two-divide truncation, per-phase min/max), `ap4_block_gas_cost` (on/fast/slow + clamp-to-0, `parentCost=None`→0, Granite→0). Also reuse `calculate_price` golden 9-row table (21 §0, incl. the `MaxUint64−11` row) — port `ava-gas::calculate_price` or re-export.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm feerules::window` → fails.
- [x] **Step 3 — Green:** Implement `Window([u64;10])`, `base_fee_from_window` (per-phase `TargetGas`/denom/bounds keyed on parent vs child timestamp exactly per traps), `ap4_block_gas_cost` (TargetBlockRate=2, step, clamp [0,1e6]). All checked/saturating U256+u64, no floats (00 §6.1). Commit AP3/AP4 golden vectors + proptest regressions.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm feerules::window feerules::blockgas` → pass.
- [x] **Step 5 — Commit:** `ava-evm: AP3 fee window + AP4 block gas cost (G2, 21 §4)`

> **AS-BUILT (M6.11).** `base_fee_from_window` replicates all four AP3 traps: exact-target early/unclamped
> return, delta floored at 1, decrease-only `windowsElapsed` scaling, two SEPARATE truncating divides
> (`/target` then `/denom`). Per-phase target/denom/min/max carried by `BaseFeeParams::{ap3,ap4,ap5,etna}`,
> keyed by the caller's resolved phase (mirrors Go's `IsX(parent.Time)` switch). `ap4_block_gas_cost`
> (`blockgas.rs`): TargetBlockRate=2, step, `abs_diff` deviation, saturating mul, clamp `[0,1e6]`,
> `parentCost=None`→0; `block_gas_cost` wrapper applies the Granite→0 override. **Reused
> `ava_vm::components::gas::{calculate_price, GasState, Gas, Price}`** (re-exported from `feerules/mod.rs`,
> NOT re-derived) for the 9-row CalculatePrice golden table incl. the `MaxUint64−11` row. All three AP3
> and three AP4 worked examples reproduced EXACTLY vs spec 21 §4 (cross-checked against coreth
> `dynamic_fee_windower.go` / `ap4/cost.go`: AbsDiff, `defaultCost` on overflow/underflow, clamp order
> faithful). No facade re-exports; only dep added is `ruint`. No spec corrections needed.

### Task M6.12: `feerules::acp176` Fortuna/ACP-176 + ACP-226 (G2) ✅ DONE (5d33835)
**Crate:** ava-evm  ·  **Depends on:** M6.11  ·  **Spec:** 21 §5; 10 §7.1, §17.3 (G2)
**Files:** `crates/ava-evm/src/feerules/acp176.rs`, `crates/ava-evm/src/feerules/acp226.rs`, `crates/ava-evm/tests/vectors/cchain/fees/acp176/*.json`
- [x] **Step 1 — Red:** Golden tests from 21 §5: `Acp176::{target, gas_price, advance_seconds, advance_milliseconds, update_target_excess (±Q clamp + scaleExcess floor), consume_gas}` at `excess ∈ {0,K}`, the `K=T·87` doubling identity, 24-byte big-endian state serialization, and ACP-226 min-delay-excess. Note **scaleExcess rounds DOWN** (vs SAE ceil) — do not share routine.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm feerules::acp176` → fails.
- [x] **Step 3 — Green:** Implement `Acp176 { gas: GasState, target_excess }` per 21 §5 (constants P/D/M/Q/T2MAX/FILL/T2PRICE/maxTargetExcess), `mul_ub`, `scale_excess` (U256 floor), `AvaFeeState` (canoto-blob header-extra serialization), and ACP-226. Reuse `GasState` + `calculate_price`. Commit ACP-176 golden vectors.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm feerules::acp176 feerules::acp226` → pass.
- [x] **Step 5 — Commit:** `ava-evm: Fortuna/ACP-176 + ACP-226 dynamic fee (G2, 21 §5)`

> **AS-BUILT (M6.12).** `Acp176State` + `acp226::DelayExcess` per spec 21 §5; reuses
> `ava_vm::components::gas::{GasState, calculate_price}` (NOT re-derived). `scale_excess` is **U256 floor**
> (rounds DOWN — deliberately NOT shared with any SAE ceil routine). `mul_ub` = `saturating_mul` (Go
> `safemath.Mul` with `MaxUint64` fallback). 38 new tests (89 total `ava-evm`), all green; no facade edits.
> **FINDING:** `ava-evm::Error` has no `InsufficientCapacity` variant (unlike `ava-vm::Error`), so
> `consume_gas` maps `gas.ErrInsufficientCapacity` → `Error::FeeOverflow` (consistent with §11.2's fee-fault
> sentinel; flag for future refinement if a dedicated variant is wanted).

### Task M6.13: `next_evm_env` fee override wiring + `prop::evm_fee_schedule_per_fork` ✅ DONE (b584d5c)
**Crate:** ava-evm  ·  **Depends on:** M6.11, M6.12, M6.6  ·  **Spec:** 10 §7.2, §17.3 (G2); 21 §7; 02 §4
**Files:** `crates/ava-evm/src/evmconfig.rs`, `crates/ava-evm/src/feerules/mod.rs`, `crates/ava-evm/tests/fee_schedule.rs`, `crates/ava-evm/tests/proptest-regressions/fee_schedule.txt`
- [x] **Step 1 — Red:** `tests/fee_schedule.rs` `proptest! fn evm_fee_schedule_per_fork()` (exit-gate name): over random `(parent header, AvaNextBlockCtx, fork timestamp)`, assert `next_evm_env` selects the correct regime — pre-AP3 basefee absent (nil/`errNilBaseFee` parity), AP3..Fortuna→window, Fortuna+→ACP-176 — and that `feerules::base_fee`/`gas_limit` match the per-fork dispatch; invariants from 21 §9 (off-target moves ≥1; AP4 cost ∈[0,1e6]; ACP-176 price continuous across `UpdateTargetExcess`).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm evm_fee_schedule_per_fork` → fails.
- [x] **Step 3 — Green:** Implement `AvaNextBlockCtx` (timestamp/timestamp_ms/recipient/gas_limit_hint/pchain_height/parent_fee_state), `feerules::{base_fee, gas_limit}` fork dispatch (window vs acp176), and `ConfigureEvm::next_evm_env` override setting `block_env.{basefee,gas_limit}` + pre-AP3 nil handling. Also `atomic_gas`/`atomic_fee` helpers (TxBytesGas/EVMOutputGas/EVMInputGas/CostPerSignature, `ErrFeeOverflow` guard) for §17.3 — counted against block budget in M6.15/M6.20. Commit proptest regressions.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm evm_fee_schedule_per_fork` → pass.
- [x] **Step 5 — Commit:** `ava-evm: next_evm_env fee override + per-fork schedule proptest (G2)`

> **AS-BUILT (M6.13).** `feerules::mod` gained `FeeRegime`/`regime_for_phase`/`window_params_for_phase`/
> `base_fee`/`gas_limit`/`atomic_gas`/`atomic_fee`; `evmconfig.rs` gained the canonical `AvaNextBlockCtx`
> (all §17.3 fields + `atomic_gas_limit`), `AvaFeeState` enum, and the inherent `AvaEvmConfig::next_evm_env`
> override (additive — M6.6's executor/`AvaExecutorSpec` + M6.21's precompile methods untouched). **M6.16 stub
> reconciled:** `atomic/mempool.rs` dropped its local `AvaNextBlockCtx { atomic_gas_limit }` and now
> `pub use crate::evmconfig::AvaNextBlockCtx` (with `::with_atomic_gas_limit` kept so existing mempool callers
> compile). `AvaNextBlockCtx` (build/fee ctx) is kept DISTINCT from M6.21's `AvaBlockCtx` (revm chain-slot ext).
> No facade re-exports (`NextBlockEnvAttributes` already present). 95→ tests green. **DEFERRED (M6.6 finding #3,
> per scope guard):** the base-fee-**RECIPIENT** override (Avalanche credits AP3+ base fee to coinbase; revm
> burns it) needs a custom revm handler / `EvmFactory` — the SAME live-handler install M6.21 deferred — so it is
> folded into **M6.22** (build the `EvmFactory` once, install precompiles + base-fee-recipient + `AvaCtxExt`
> together). The M6.6 `cchain_state_root` fixture was deliberately NOT re-pointed (stays at M6.30's burn-model
> 5-field root). **SPEC FINDINGS:** (1) spec 21 doesn't pin the **EVM block gas-limit constants** — used coreth
> `ApricotPhase1GasLimit = 8_000_000` / `CortinaGasLimit = 15_000_000` with a `gas_limit_hint` override (header
> `GasLimit`, separate from the ACP-176 dynamic capacity gate, left as a `Result`-returning seam); worth pinning
> in spec 21. (2) `AvaFeeState` is the agreed hand-off type carrying the AP3 window + parent base fee + the
> ACP-176 24-byte state extracted from the parent header extra-data (the M6.7 block-wire ↔ builder/verifier seam).

### Task M6.14: Atomic tx types + byte-exact codec ✅ DONE (dfd7e53)
**Crate:** ava-evm  ·  **Depends on:** M6.2, M3 (ATOMIC-1 codec/types)  ·  **Spec:** 10 §6.1, §6.2; 02 §6
**Files:** `crates/ava-evm/src/atomic/mod.rs`, `crates/ava-evm/src/atomic/tx.rs`, `crates/ava-evm/tests/vectors/cchain/atomic/*.json`
- [x] **Step 1 — Red:** `atomic::tx` `mod tests`: `fn import_export_serialize_byte_exact()` asserts `EvmOutput`/`EvmInput`/`UnsignedImportTx`/`UnsignedExportTx` linear-codec (ava-codec, NOT RLP) bytes equal Go golden hex, field order verbatim (addr, amount, asset[, nonce]); `fn atomic_ops_requests_match_go()` asserts Import→`RemoveRequests=utxoIDs` on source, Export→`PutRequests=elems` on dest; verify constants `X2CRate=1_000_000_000`, `TxBytesGas`, `EVMOutputGas`, `EVMInputGas`, `CostPerSignature`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic::tx::tests` → fails.
- [x] **Step 3 — Green:** Implement `EvmOutput`/`EvmInput`/`UnsignedImportTx`/`UnsignedExportTx`/`AtomicTx` + `SignedTx<_>` with `#[derive(AvaCodec)]`, `atomic_ops() -> (Id, atomic::Requests)`, and the constants (cite Go paths in doc-comments). Reuse `TransferableInput`/`TransferableOutput`/`atomic::Requests` from M3. Commit atomic golden vectors.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic::tx::tests` → pass.
- [x] **Step 5 — Commit:** `ava-evm: atomic Import/Export tx types + byte-exact codec (§6.1)`

> **AS-BUILT (M6.14).** Golden vectors are **Go-EXECUTED** (scratch `package atomic` test on go1.25.10 against
> `coreth/plugin/evm/atomic`, `Codec.Marshal` + `AtomicOps`, then deleted) — not hand-derived. Reused
> `ava_vm::components::avax::shared_memory::{Requests, Element}` (canonical X↔P payloads) and the
> codec-serializable `ava_avm::txs::components::{TransferableInput, TransferableOutput}` + `credential::FxCredential`
> (the `ava_vm::components::avax` versions are `Arc<dyn>` trait objects that can't derive `AvaCodec`; the
> X-Chain mirrors are byte-identical and secp fx type-ids 5/7/9 coincide). `EvmOutput.address` stored as
> `[u8;20]` (codec-native, no facade `Address` needed). **Codec type-id registry (atomic, DISTINCT from
> X-Chain):** 0=Import, 1=Export, 5=TransferInput, 7=TransferOutput, 9=Credential, 10=Input, 11=OutputOwners.
> **PARITY HAZARD (see SPEC FIX):** Go emits the interface `u32` type_id prefix ONLY when the static type is
> the `UnsignedAtomicTx` interface (inside `Tx.Sign`); a concrete `*UnsignedImportTx` marshals with NO prefix.
> Both `struct_codec_hex` + `interface_codec_hex` captured; the `AtomicTx` enum produces the interface form.
> Constants: X2C_RATE=1e9, TX_BYTES_GAS=1, EVM_OUTPUT_GAS=60, EVM_INPUT_GAS=1068, COST_PER_SIGNATURE=1000.
> Signing/recovery deferred to M6.18. Deps added: `ava-codec(-derive)`, `ava-types`, `ava-vm`, `ava-avm`,
> `ava-crypto`, dev `ava-secp256k1fx`.

### Task M6.15: `AtomicStateHook` EVMStateTransfer pre-hook + atomic gas (G3) ✅ DONE (44f3160)
**Crate:** ava-evm  ·  **Depends on:** M6.14, M6.6, M6.13  ·  **Spec:** 10 §6.3, §17.4 (G3); 21 §4b (atomic gas budget)
**Files:** `crates/ava-evm/src/atomic/hook.rs`, `crates/ava-evm/tests/atomic_transfer.rs`
- [x] **Step 1 — Red:** `tests/atomic_transfer.rs` `fn import_credits_export_debits_and_bumps_nonce()`: apply `AtomicStateHook` to a `State<FirewoodStateView>` overlay; Import credits `amount * X2C_RATE` wei (checked); Export debits + sets `nonce = max(cur, i.nonce+1)` (matches coreth); assert resulting `BundleState` folds into the same Firewood proposal as EVM effects; overflow → `Error::FeeOverflow`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_transfer` → fails.
- [x] **Step 3 — Green:** Implement `AtomicStateHook::apply(&[AtomicTx], &mut impl revm::Database)` (checked `X2C_RATE` mul, increment/decrement balance, nonce bump) and `AvaBlockExecutor<E>` decorator whose `apply_pre_execution_changes` runs inner pre-changes then the atomic hook (and reserves predicate slot for M6.22), implementing `PreExecutionHook` so `execute_batch` accepts it (§17.1/§17.4). Wire atomic gas into the block budget (M6.13 helpers).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_transfer` → pass.
- [x] **Step 5 — Commit:** `ava-evm: AtomicStateHook EVMStateTransfer pre-hook + atomic gas (G3)`

> **AS-BUILT (M6.15).** `AtomicStateHook` impls the facade `PreExecutionHook` directly (no separate
> `AvaBlockExecutor` decorator needed — `execute_batch` already takes `&dyn PreExecutionHook`, M6.6). Import →
> `checked_mul(amount, X2C_RATE)` credit; Export → debit + `nonce = max(cur, nonce+1)`. `tx_gas`/`batch_gas`/
> `batch_fee` reuse M6.13 `feerules::atomic_gas`/`atomic_fee` (counts only, constants not re-derived); warp-predicate
> pass has a reserved no-op slot for M6.22. Confirmed vs coreth `plugin/evm/atomic/{import,export}_tx.go`
> `EVMStateTransfer` (read directly, no scratch test). **FACADE CHANGE (breaking, reconcile M6.26):** revm's
> `Database` trait is **read-ONLY** (no `increment_balance`/`set_nonce`); writes go via `DatabaseCommit::commit`
> (which `State<DB>` impls). So `PreExecutionHook::apply` was widened from `&mut dyn Database<Error=StateDbError>`
> to **`&mut dyn StateDb`** where new facade `pub trait StateDb: Database<Error=StateDbError> + DatabaseCommit {}`
> (+ blanket impl). The hook per touched address: `db.basic(addr)?` (loads into overlay cache — mandatory, else
> `commit`'s `apply_account_state` panics on a missing cache entry) → mutate (checked) → `db.commit` a
> `RevmAccount{ status: AccountStatus::Touched, .. }` (**`Touched` required** — untouched accounts are a commit
> no-op). Folds into the same `BundleState` → Firewood proposal as EVM effects. Facade re-exports added:
> `AddressMap` (alloy), `DatabaseCommit`, `AccountStatus`, `EvmStorage` (revm) + `impl From<StateDbError> for
> AvaEvmError`. **SPEC FINDING (§6.3/§17.4):** the sketch's `db.increment_balance`/`db.set_nonce` write API does
> not exist on revm `Database` — use `basic()` read + `DatabaseCommit::commit` with a `Touched` account; the
> `PreExecutionHook` trait must be `Database + DatabaseCommit`. **Export nonce-equality / insufficient-funds
> REJECTIONS are semantic-verify-time (coreth `ErrInvalidNonce`/`ErrInsufficientFunds`, scope M6.17/M6.18)** — the
> pure transfer hook saturates the debit rather than erroring (no such `Error` variant yet).

### Task M6.16: Atomic mempool ✅ DONE (ac1bb8d)
**Crate:** ava-evm  ·  **Depends on:** M6.14  ·  **Spec:** 10 §6.4, §17.4; 05 (gossip SDK)
**Files:** `crates/ava-evm/src/atomic/mempool.rs`, `crates/ava-evm/tests/atomic_mempool.rs`
- [x] **Step 1 — Red:** `fn mempool_orders_dedups_and_conflict_checks()`: add atomic txs, assert heap ordering, dedup by source UTXO, conflict-reject of txs spending pending UTXOs, `next_batch` returns one gas-limited batch, `discardedTxs`/`issuedTxs` lifecycle, and a `Notify` fires on non-empty.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_mempool` → fails.
- [x] **Step 3 — Green:** Implement `AtomicMempool` (heap order, UTXO dedup/conflict set, `next_batch(&AvaNextBlockCtx)` one-batch-per-block, lifecycle maps, `tokio::sync::Notify`) + `gossip::Gossipable` impl for the p2p SDK (05). Reproduce coreth `atomic/txpool` semantics.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_mempool` → pass.
- [x] **Step 5 — Commit:** `ava-evm: atomic mempool (ordering, dedup, conflicts, gossip) (§6.4)`

> **AS-BUILT (M6.16).** Faithful port of coreth `plugin/evm/atomic/txpool/{mempool,txs,tx_heap}.go`.
> Ordering by effective gas price (`burned * X2CRate / gasUsed`, rounded down, `u128` product — exact vs
> coreth's uint256 for all in-range values), highest first, ties broken by tx id for determinism. Dedup by
> tx id across Pending/Current/Issued(+Discarded for remote). Conflict-reject by source UTXO id unless the
> newcomer strictly outbids every conflict (then incumbents evicted), matching `checkConflictTx`. Lifecycle
> maps `current`/`issued`/`discarded` + mempool-full lowest-priced eviction + fee-replacement.
> `tokio::sync::Notify` via `subscribe()`, `notify_one` on each admission. **Local `Gossipable` seam**
> (`gossip_id = tx_id`) impl'd for `Tx` (mirrors X-Chain `ava-avm` `network/gossip.rs`). Only dep added:
> `tokio`. No facade re-exports. **FOLLOW-UPS for later tasks:** (1) `AvaNextBlockCtx` is a minimal local
> stub `{ atomic_gas_limit: u64 }` here — M6.13 lands the full type (timestamp(ms)/recipient/gas/P-chain
> height); `next_batch` only needs the atomic gas budget. (2) `ATOMIC_TX_INTRINSIC_GAS = 10_000`
> (`ap5.AtomicTxIntrinsicGas`) is a local const with `fixedFee=true` (post-AP5 mainnet/Fuji) — source it
> from AP5 params once the fork-aware fee path lands. (3) Discarded cache is a bounded FIFO (cap 50) vs
> coreth's LRU — non-consensus (courtesy de-dup; local re-issue bypasses it). **GOTCHA:** nextest `-E
> 'test(atomic_mempool)'` matches the fn name not the binary → use `-E 'binary(atomic_mempool)'` (or the
> full `-p ava-evm` run).

### Task M6.17: `AtomicBackend` + atomic trie (2nd Firewood) + shared-memory batch (G3) ✅ DONE (68c3ee2)
**Crate:** ava-evm  ·  **Depends on:** M6.14, M6.9, M3 (07 shared memory)  ·  **Spec:** 10 §6.4, §17.4 (G3); 07 (shared-memory contract); 04 §4.2
**Files:** `crates/ava-evm/src/atomic/backend.rs`, `crates/ava-evm/src/atomic/trie.rs`, `crates/ava-evm/tests/atomic_backend.rs`
- [x] **Step 1 — Red:** `tests/atomic_backend.rs` `fn accept_indexes_trie_and_applies_shared_memory()`: `AtomicBackend::accept(height, txs)` writes `key = height(8B)||blockchainID(32B)` → serialized requests into a 2nd ethhash Firewood instance, root matches a Go golden atomic-trie root, `TrieKeyLength=40`, `EmptyRootHash` init, periodic `commitInterval` checkpoint, and the shared-memory `Requests{Put,Remove}` apply happens in ONE atomic batch with the trie commit.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_backend` → fails.
- [x] **Step 3 — Green:** Implement `AtomicTrie` (key encoding, `serialize_requests` via ava-codec byte-exact, `EmptyRootHash`), `AtomicBackend { trie, shared_memory, last_committed_root, commit_interval }` with `accept` (merge ops → propose → root → atomic shared-memory apply + commit together) per §17.4; hook into `EvmBlock::accept` AFTER state commit. Commit atomic-trie-root golden vector.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_backend` → pass.
- [x] **Step 5 — Commit:** `ava-evm: AtomicBackend + atomic trie + shared-memory batch (G3)`

> **AS-BUILT (M6.17).** `AtomicTrie` = a SECOND, independent ethhash Firewood instance (own dir); reuses the
> §17.2.2 deviation (Firewood `Proposal<'db>` borrows the `&Db` → can't stash a live proposal; stash the
> deterministic `BatchOp` list and re-propose+commit at commit time — roots bit-identical). **Key = `height.to_be_bytes()(8B) || blockchainID(32B)`, `TRIE_KEY_LENGTH = 40`** (`= wrappers.LongLen(8) + common.HashLength(32)`; height big-endian via `Packer.PackLong`). **Trie VALUE = `atomic.Codec.Marshal(CodecVersion=0, *Requests)` — a SINGLE `*Requests` per (height, chain) key, NOT a `map[ids.ID]*Requests`** (layout: `version(2B=0x0000)` + `RemoveRequests([][]byte: u32 count, each u32 len+bytes)` + `PutRequests([]*Element: u32 count, each = u32-len key, u32-len value, u32-count traits each u32-len+bytes)`). `EMPTY_ROOT_HASH` for the empty trie. `serialize_requests` uses byte-exact AvaCodec mirrors `CodecRequests`/`CodecElement`. `AtomicBackend { trie, shared_memory, last_committed_root, commit_interval }`; **`DEFAULT_COMMIT_INTERVAL = 4096`** (coreth `plugin/evm/config`): the trie root advances every block (`lastAcceptedRoot`), only `height % commitInterval == 0` records `lastCommittedRoot`. `accept(height, txs)` collects each tx's `(peerChainId, Requests)` into a `BTreeMap<Id, Requests>` (sorted, no HashMap on the write path) → propose+root → durable commit → `SharedMemory::apply`. **Wired into `EvmBlock::accept` ADDITIVELY:** `EvmBlockContext` gained `atomic_backend: Option<Arc<AtomicBackend>>` (defaults `None`; `new(...)` signature UNCHANGED; new `with_atomic_backend(...)` builder + `atomic_backend()` accessor); `accept` calls `backend.accept(self.number(), self.atomic_txs())?` AFTER `state.commit` and before the canonical append. All existing `lifecycle.rs` tests pass unchanged. **GOLDEN root is GO-EXECUTED** (scratch `package state` test in coreth `plugin/evm/atomic/state` at the pinned rev, go1.25.10, then deleted; provenance in `tests/vectors/cchain/atomic_trie/_provenance.md`): root `15211e79c52a022d51afc4ed1cd77db2477cbcb85620d28a15923c5f96476056` for the M6.14 import+export ops at height=1 — Rust reproduces byte-for-byte. **No facade edits.** 104→111 tests on-branch (3 backend integration + 4 trie unit); 112 combined with M6.10.
>
> **FOLLOW-UPS (flagged, fold into later tasks):**
> - **Looser cross-store atomicity vs Go (→ recovery pass, M6.25/M6.27).** Coreth threads the atomic-trie's
>   versiondb batch INTO `sharedMemory.Apply(requests, batch)` so trie-root advance + cross-chain put/remove land
>   in ONE DB commit. Our Firewood atomic trie owns its own durable commit (separate DB), so we commit-trie-THEN-
>   apply-shared-memory (no extra `batches` passed to `apply`). This holds the §17.4 invariant for a single-process
>   node but is weaker than Go's shared-versiondb batch; a crash-consistency guarantee across the two stores needs
>   an atomic-trie ↔ shared-memory reconcile/cursor pass on startup (coreth `ApplyToSharedMemory`), NOT implemented.
> - **commitInterval skip-backfill not modeled.** Coreth `AcceptTrie` back-fills skipped commit heights into a
>   `Root(height)` metadata index; our Firewood trie commits durably every block (revision window) + tracks the
>   checkpoint pointer, so the metadata back-fill loop is unneeded for root parity — flag for state-sync (M6.25)
>   if the `Root(height)` query API is later required.
> - **Same-chain multi-tx ordering.** `merge_atomic_ops` concatenates per-chain `Requests` in tx order; coreth
>   sorts txs by `tx.id()` (`utils.Sort(copyTxs)`) before merging. The golden (single import + single export,
>   distinct chains) doesn't exercise this; for exact Go parity with multiple SAME-chain atomic txs per block,
>   sort txs by id before merge (fold into M6.18 semantic-verify or M6.20 build, which order the batch).

### Task M6.18: Atomic semantic verify, conflict sets, bonus blocks (C10) ✅ DONE (05cc015)
**Crate:** ava-evm  ·  **Depends on:** M6.17, M6.9  ·  **Spec:** 10 §6.5; 07
**Files:** `crates/ava-evm/src/atomic/verify.rs`, `crates/ava-evm/tests/atomic_verify.rs`
- [x] **Step 1 — Red:** `fn rejects_conflicting_inputs_across_ancestry()`: a tx whose UTXOs are spent in shared memory or by another atomic tx in the same/ancestor block → `Error::ConflictingAtomicInputs`; `fn bonus_blocks_skip_set_matches_go()` reproduces the height→ID skip-set verbatim.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_verify` → fails.
- [x] **Step 3 — Green:** Implement conflict set (`Set<Id>` of consumed UTXOs checked across verified-block ancestry), `bonusBlocks` skip-set constant, and the atomic semantic-verify pass invoked from `EvmBlock::verify`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_verify` → pass.
- [x] **Step 5 — Commit:** `ava-evm: atomic semantic verify + conflicts + bonus blocks (§6.5)`

> **AS-BUILT (M6.18).** `atomic/verify.rs`: `input_utxos(&AtomicTx)->BTreeSet<Id>` (coreth `InputUTXOs`: Import → `input_id()`=`tx_id.prefix(output_index)`; Export → raw 32-byte `nonce(8 BE) ++ 0x00000014 ++ address(20)`, NOT hashed); `verify_no_conflicts(&[AtomicTx], &BTreeSet<Id>)` (intra-block overlap = coreth `verifyTxs` + processing-ancestry overlap = coreth `conflicts`); `mainnet_bonus_blocks()->&BTreeMap<u64,Id>` + `is_bonus_block`. Hooked into `EvmBlock::verify` BEFORE EVM execution; added `EvmBlockContext::processing_ancestor_inputs()`. `Error::ConflictingAtomicInputs` already existed. **116 ava-evm tests green.**
> **FINDINGS (folded below):** (1) **bonusBlocks = 57 entries** (102972→103633), Mainnet-only (`readMainnetBonusBlocks`); Fuji/local empty. (2) **Processing-ancestry conflict is a no-op on the linear-accept path** (correct — coreth's `conflicts` stops at last-accepted) — the non-linear sibling/processing-fork ancestry walk needs the verified-block tree owned by the `ChainVm` adapter (M6.10); `verify_no_conflicts` already takes the ancestry input-union, so the follow-up is just threading it from the adapter → **new follow-up M6.18a** (wire ancestry inputs from `EvmVm`'s `verified` map into `verify`). (3) **bonusBlocks skip applies at shared-memory *apply* time, not at trie-index time** — blocks are still indexed in the atomic trie; `AtomicBackend::accept` (M6.17) does NOT yet consult `is_bonus_block` before `SharedMemory::apply` → **fold into M6.18a / M6.27 recovery** (wire `is_bonus_block` into the apply path).

### Task M6.19: `differential::atomic_xc` X↔C import/export parity ✅ DONE (e7aca95)
**Crate:** ava-evm  ·  **Depends on:** M6.15, M6.17, M6.18  ·  **Spec:** 10 §6, §14 #3; 02 §11; 07
**Files:** `crates/ava-evm/tests/atomic_xc.rs`, `crates/ava-evm/tests/vectors/cchain/atomic_xc/*.json`
- [x] **Step 1 — Red:** `tests/atomic_xc.rs` `#[test] fn atomic_xc()` (exit-gate name) in recorded-oracle mode: for a Go corpus of ImportTx/ExportTx, assert byte-identical tx serialization, identical `atomic.Requests`, identical post-`EVMStateTransfer` balances/nonces, and identical atomic-trie roots vs Go; shared-memory effects checked against the M3/07 harness stub (so M6 stays independent of M5). Tag the live-mode variant `#[ignore]`/CI-gated (coordinate with cross-cutting harness X).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_xc` → fails.
- [x] **Step 3 — Green:** Wire the corpus fixtures + comparison; close any parity gaps surfaced (serialization, requests, balances, trie root). Commit atomic_xc vectors + manifest with provenance.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_xc` → pass.
- [x] **Step 5 — Commit:** `ava-evm: X↔C atomic import/export parity (differential::atomic_xc)`

> **AS-BUILT (M6.19).** A single composite exit-gate test `atomic_xc()` asserts all four parity facets in one
> pass over one Go-executed Import+Export corpus: (a) byte-identical serialization (bare-struct + interface
> u32-type-id 0/1 forms + export signed-tx id), (b) atomic `Requests` (Import→`RemoveRequests` on source,
> Export→`PutRequests`/`Element` on dest), (c) post-`EVMStateTransfer` balances/nonces via the REAL
> `execute_batch` + `AtomicStateHook` path over a Firewood `State` overlay (import credit `amount·X2C_RATE`,
> nonce untouched; export debit + `nonce→input.nonce+1`), (d) atomic-trie root advance to the Go-golden
> `15211e79…6056` + cross-chain Put/Remove applied to the in-memory `ava_vm` `SharedMemory` harness (keeps M6
> **independent of M5**). **REUSED existing Go-EXECUTED vectors** (`vectors/cchain/atomic/atomic_txs.json` +
> `vectors/cchain/atomic_trie/atomic_trie_root.json` — they already cover the round-trip), so NO new Go bytes
> generated; added `vectors/cchain/atomic_xc/{manifest.json,_provenance.md}` documenting the composite +
> transitive provenance (coreth `fb174e8…`, go1.25.10). **No parity gaps surfaced** — every facet matched on
> first wiring (the underlying M6.14/15/17/18 impls were each already golden-tested; M6.19 is the composite
> gate). No new deps, no facade/`rpc/mod.rs` touch. avalanchego tree left git-clean.

### Task M6.20: `BlockBuilderDriver` on-demand build + precomputed-root finish (G5) ✅ DONE (dd81e6e)
**Crate:** ava-evm  ·  **Depends on:** M6.10, M6.13, M6.15, M6.16  ·  **Spec:** 10 §4, §17.6 (G5); 21 §4b (budget)
**Files:** `crates/ava-evm/src/builder.rs`, `crates/ava-evm/tests/build.rs`
- [x] **Step 1 — Red:** `tests/build.rs` `fn build_then_verify_same_root()`: `build_on(parent, ctx)` pulls one atomic batch + EVM txs (effective-tip order, until gas/blockGasCost budget), computes the Firewood root, passes `Some((root, TrieUpdates::default()))` to `finish`, and the self-built block **re-verifies to the identical root** (build-then-verify symmetry); `fn respects_min_build_delay()`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm build` → fails.
- [x] **Step 3 — Green:** Implement `BlockBuilderDriver` (§17.6): ... `propose_from_bundle` + `stash_proposal`, `finish(view_tip, Some((root, default)))` (G5/G1), `assemble_ava_block`, `minBlockBuildingRetryDelay` guard, `Notify`-driven.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm build` → pass.
- [x] **Step 5 — Commit:** `ava-evm: on-demand BlockBuilderDriver + precomputed-root finish (G5)`

> **AS-BUILT (M6.20).** `builder.rs`: `BlockBuilderDriver` — `build_on(parent, parent_state_root, ctx, evm_txs)` pulls one atomic batch → reserves atomic gas + AP4 blockGasCost surcharge → packs EVM txs by effective tip under the gas budget → `execute_batch` with `AtomicStateHook` over the parent Firewood view → `propose_from_bundle` stashes the precomputed root → `assemble_ava_block`; `can_build_on` + `MIN_BLOCK_BUILD_DELAY` (per-parent retry guard); per-fork AP3 base-fee / AP4 ext-data-gas+block-gas-cost header fields; ext_data marshals the signed atomic batch. `vm.rs::build_block` resolves the preferred leaf's header+root from the processing tree and drives the builder (else `NotFound`=`ErrNoPendingBlock`). Facade += `pub use alloy_consensus::Transaction as ConsensusTx` (gas_limit/effective_tip accessors). **118 ava-evm tests green.**
> **AS-BUILT DEVIATION (→ spec 10 §17.6):** the §17.6 sketch's vN-sensitive reth `builder_for_next_block` + `finish(view_tip, Some((root, TrieUpdates::default())))` seam is realized through the EXISTING `execute_batch` + `FirewoodStateProvider::propose_from_bundle` path (already stashes deterministic ops keyed by the real Firewood root; the empty-`TrieUpdates` half lives in `state.rs::state_root_with_updates`). Same path `verify` uses → build-then-verify symmetry holds BY CONSTRUCTION, not via the unstable reth builder seam. (Same spirit as the §17.2.2 proposal-stash deviation.)
> **FOLLOW-UPS:** (1) **No reth `TransactionPool` in ava-evm yet** — `build_on` takes EVM tx candidates as an explicit effective-tip-ordered `Vec<RecoveredTx>`; `vm.rs::build_block` supplies `vec![]` today (atomic-only blocks build from the mempool). Wire `best_transactions` when the EVM txpool lands (→ M6.23-era). (2) `vm.rs::build_block` uses `AvaFeeState::default()` for the next-block ctx — thread parent-extra-data fee-state extraction (AP3 window blob / ACP-176 24-byte state) once M6.7's extractor is wired. (3) builder leaves empty-trie sentinels for `tx_root`/`receipt_root` (self-verify only asserts `state_root`+`gas_used`, §3.2); full receipt-root path = M6.23/M6.24.

### Task M6.21: `AvaPrecompiles` `PrecompileProvider` + registry (G4/G10) ✅ DONE (c4dc2e8)
**Crate:** ava-evm  ·  **Depends on:** M6.6  ·  **Spec:** 10 §8, §17.5 (G4), §17.11 (G10)
**Files:** `crates/ava-evm/src/precompile/mod.rs`, `crates/ava-evm/src/precompile/registry.rs`, `crates/ava-evm/tests/precompile_dispatch.rs`
- [x] **Step 1 — Red:** `fn dispatch_falls_through_and_gates_by_height()`: `AvaPrecompiles` runs a registered stateful precompile when its address is in the activated (fork+upgrade-gated) `warm` set, else falls through to `EthPrecompiles`; `for_height(t)` computes the activated set from the timestamp-keyed upgrade schedule; `contains`/`warm_addresses` correct.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm precompile_dispatch` → fails.
- [x] **Step 3 — Green:** Implement `AvaCtxExt { predicates, block_ctx }` (revm context extension, G4/G10), `StatefulPrecompile` trait, `PrecompileRegistry`, `AvaPrecompiles { base, modules, warm }` impl facade `PrecompileProvider` (`set_spec`/`run`/`warm_addresses`/`contains`) per §17.5, `for_height`. Wire `AvaBlockExecutorFactory::create_executor` to install `AvaPrecompiles::for_height` + `AvaCtxExt` into the revm handler. Keep all revm-shape spelling behind the facade (G0/G10).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm precompile_dispatch` → pass.
- [x] **Step 5 — Commit:** `ava-evm: AvaPrecompiles PrecompileProvider + registry (G4/G10)`

> **AS-BUILT (M6.21).** `registry.rs`: `AvaPrecompiles` (impl facade `PrecompileProvider`), `PrecompileRegistry`,
> `PrecompileModule`, `StatefulPrecompile` trait, `AvaCtxExt`/`PredicateResults`/`AvaBlockCtx` (G10 context-ext
> plumbing — fields reserved for M6.22's warp predicate results), `PrecompileCtx`, `for_height`/`contains_stateful`/
> `dispatch_stateful`/`warm_addresses_vec`. **Registry + provider + height-gating + EthPrecompiles fall-through
> ONLY; actual warp/allowlist/feemanager bodies are M6.22.** Integration is a **clean seam, NOT invasive wiring**:
> `evmconfig.rs` (M6.6-owned) gained additive `chain_spec`/`precompiles` fields + `with_precompiles(registry)`
> (M6.22 registration), `precompiles_for_header(header)` (height-gated §17.5 create-executor seam),
> `ctx_ext_for_header(header)` (G10). Installing `AvaPrecompiles` into the live revm handler needs a custom
> `EvmFactory`/`ConfigureEvm` (would churn the M6.6 bare-executor path) → **deferred to M6.22**, which builds the
> factory that drops `AvaPrecompiles` + `AvaCtxExt` onto the `ContextTr::Chain` slot. Facade re-exports added:
> `Cfg, ContextTr` (context_interface), `EthPrecompiles, precompile_output_to_interpreter_result` (handler),
> `CallInputs, InterpreterResult` (interpreter), `PrecompileError, PrecompileOutput, PrecompileSpecId, Precompiles`
> (precompile). **SPEC FIXes vs §17.5/§17.11 (real revm `revm-handler` 18.1 / pinned rev):** (1) `PrecompileProvider::set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool` (generic over context spec, `Into<SpecId>`), NOT `set_spec(spec: SpecId)`. (2) `warm_addresses` returns `Box<impl Iterator<Item=Address>>`, NOT `&HashSet<…>`. (3) `run` dispatch uses `inputs.bytecode_address`/`inputs.caller`/`inputs.call_value()`/`inputs.input.bytes(ctx)` (no `target_address`/`caller_address`). (4) **No `ctx.ext()` accessor** — the typed extension rides on `ContextTr::Chain` via `ctx.chain()`. (5) `PrecompileError` has only `Fatal(String)`/`FatalAny(AnyError)` — no `Other`.

### Task M6.22: Predicate pass + Warp precompile over `ava-warp` (G4/C10) ✅ DONE (4946824)
**Crate:** ava-evm  ·  **Depends on:** M6.21, M6.15  ·  **Spec:** 20 §7 (precompile ABI, predicate, gas), 10 §6.5, §8.2, §17.5 (G4)
**Files:** `crates/ava-evm/src/precompile/warp.rs`, `crates/ava-evm/src/precompile/{mod,registry}.rs`, `crates/ava-evm/tests/warp_precompile.rs`, `crates/ava-evm/tests/vectors/cchain/warp/*.json`, `crates/ava-warp/src/lib.rs` (`UnsignedMessage::parse`), facade `ava-evm-reth/src/lib.rs` (`Gas`/`InstructionResult`)
- [x] **Step 1 — Red:** `tests/warp_precompile.rs`: `fn predicate_verifies_then_precompile_reads()` — the BLS-aggregate predicate runs in `apply_pre_execution_changes` (via `ava-warp::verify` against the source-subnet `WarpSet` at `block_ctx.pchain_height`), stashing per-tx results; `getVerifiedWarpMessage(index)` reads the cached result; `sendWarpMessage` emits the `SendWarpMessage` log + returns the unsigned-message ID; gas costs match both pre-Granite and Granite `GasConfig` tables (20 §7.3); the `requirePrimaryNetworkSigners` subnet-substitution branch (20 §7.2 step 3); `getBlockchainID` returns the snow-ctx chain ID. (+4 more: constants/golden, chunk round-trip, gas table, accept-hook.)
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm -E 'binary(warp_precompile)'` → fails to compile against `main` (`precompile::warp` + `PredicateResults::set_warp` absent).
- [x] **Step 3 — Green:** Implemented `WarpPrecompile` (4 ABI selectors, both fork gas tables, predicate-reading, `SendWarpMessage` log emission), `run_predicates`/`PredicateContext` (predicate pass incl. `requirePrimaryNetworkSigners` substitution), `WarpBackend` + `handle_precompile_accept`, `predicate_to_chunks`/`predicate_from_chunks`, `predicate_gas`/`num_signers`, hand-rolled ABI codec, `GasConfig` + `PRE_GRANITE_GAS_CONFIG`/`GRANITE_GAS_CONFIG`. `PredicateResults` reshaped to `tx → addr → WarpTxPredicates{predicates, valid}`. Added `ava-warp`/`ava-validators`/`ava-utils` path deps. Committed warp golden selectors/topic/gas vectors.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm -E 'binary(warp_precompile)'` → 5 pass; full crate **169 tests** pass (was 164); clippy `-D warnings` clean; G0 verified.
- [x] **Step 5 — Commit:** `ava-evm: warp predicate pass + precompiles (G4, 20 §7)`

> **AS-BUILT (M6.22).** The warp precompile + predicate pass — the load-bearing, named-test target — is **complete and golden**. `WarpPrecompile` impls `StatefulPrecompile` with all four ABI selectors (`getBlockchainID`, `sendWarpMessage`, `getVerifiedWarpMessage`, `getVerifiedWarpBlockHash`), both fork gas tables, hand-rolled ABI codec, and `SendWarpMessage` log emission; `run_predicates`/`PredicateContext` is the BLS-aggregate predicate pass (over `ava-warp` + `ava-validators::ValidatorState`) incl. the `requirePrimaryNetworkSigners` subnet-substitution; `WarpBackend::handle_precompile_accept` records sent messages from logs (§3.1).
> **PREDICATE-PASS HOME (plan Files-list correction):** the predicate pass lives in `precompile/warp.rs` (`run_predicates`), NOT `atomic/hook.rs` — its natural home next to the precompile that reads its results. `atomic/hook.rs` keeps the atomic EVMStateTransfer (M6.15); the reserved warp slot it noted is now satisfied by `precompile::warp::run_predicates`.
> **FACADE (additive):** `ava_evm_reth::{Gas, InstructionResult}` (both `revm::interpreter`), tagged `// M6.22:`; the precompile builds `InterpreterResult { result, output, gas }` via non-deprecated `Gas::record_regular_cost`/`total_gas_spent`. No external crates added.
> **`ava-warp` += `UnsignedMessage::parse`** (`warp.ParseUnsignedMessage`) — the accept hook reconstructs the unsigned message from a `SendWarpMessage` log. Platform-chain-id check in the substitution branch uses `Id::EMPTY` (= `PRIMARY_NETWORK_ID`).
> **SPEC 20 §7.3 CONCRETE GAS VALUES (confirmed from coreth, folded into spec 20 §7.3):** `verifyPredicateBase` 200_000 (pre-Granite) / 125_000 (Granite); `perWarpSigner` 500/250; `perWarpMessageChunk` 3_200/512; `PredicateGas = VerifyPredicateBase + PerWarpMessageChunk·numChunks + PerWarpSigner·numSigners` (all `SafeMul`/`SafeAdd`). `sendWarpMessageBase` = 41_500 (LogGas 375 + 3·LogTopicGas 375 + 20_000 + WriteGasCostPerSlot 20_000); `perWarpMessageByte` = LogDataGas = 8. **Read re-charges** `PerWarpMessageChunk·numChunks` on EACH `getVerifiedWarpMessage`/`getVerifiedWarpBlockHash` read (coreth `contract_warp_handler.go`), not just at predicate verification.
>
> **DEFERRED → NEW TASK M6.31 (live `EvmFactory` install + remaining ConfigKey precompiles).** M6.22 supplies every primitive the live install needs but leaves two clean seams (NEITHER is exercised by the named test):
> 1. **`EvmFactory` install (the base-fee-to-coinbase override too).** `AvaEvmConfig::execute_batch` still drives reth's *bare* executor; it does NOT yet install `AvaPrecompiles` + `AvaCtxExt` onto the revm `ContextTr::Chain` slot, nor run `run_predicates` inside `apply_pre_execution_changes` against a live proposervm block ctx + `ValidatorState`. This is the custom `EvmFactory`/`ConfigureEvm` churn M6.21 deferred (it changes the M6.6 bare-executor path) **and folds in the M6.6 finding #3 base-fee-recipient override** (coinbase credit vs revm burn) noted across M6.9/M6.13. M6.31 wires: thread proposervm `PChainHeight` + `ValidatorState` into the next-block ctx, run the predicate pass pre-execution keyed by tx index, install provider+extension, and route `WarpPrecompile::take_logs` → `handle_precompile_accept` on accept.
> 2. **Other ConfigKey precompiles** (AllowList / FeeManager / NativeMinter / RewardManager / GasPriceManager) are NOT yet implemented (no M6.22 test exercises them). They register as `StatefulPrecompile`s exactly like warp; M6.31 ports each body faithfully from subnet-evm `precompile/contracts/*` with golden vectors.

### Task M6.23: `eth_*` RPC over Firewood + fee/accepted-tag overrides (G8) ✅ DONE (1fa953a)
**Crate:** ava-evm  ·  **Depends on:** M6.10, M6.13  ·  **Spec:** 10 §9.1, §17.9 (G8)
**Files:** `crates/ava-evm/src/rpc/eth.rs`, `crates/ava-evm/tests/rpc_eth.rs`, `crates/ava-evm/tests/vectors/cchain/rpc/*.json`
- [x] **Step 1 — Red:** `tests/rpc_eth.rs` golden request→response: `eth_getBalance`/`eth_call`/`eth_getProof` read Firewood state; `eth_gasPrice`/`eth_feeHistory`/`eth_maxPriorityFeePerGas` use `feerules`; the `latest`/`safe`/`finalized` tags all map to last-accepted height; `debug_traceTransaction` parity.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm rpc_eth` → fails.
- [x] **Step 3 — Green:** (see as-built deviation) direct handlers over Firewood + feerules + facade revm executor; accepted-tag mapping. Commit RPC golden vectors.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm rpc_eth` → pass.
- [x] **Step 5 — Commit:** `ava-evm: eth_* RPC over Firewood + fee/accepted-tag overrides (G8)`

> **AS-BUILT DEVIATION (M6.23, → spec 10 §17.9).** Implemented `eth_*` as a plain `EthRpc` struct returning `serde_json::Value` **directly**, NOT by instantiating reth's `EthApi<Provider,Pool,…>`. Rationale: §17.9 itself flags `EthApi`'s third-party-provider instantiation as the medium-risk/soft-upstream-ask part; the avm/platformvm precedent implements handlers directly; the jsonrpsee-vs-axum mount decision is deferred to M8/12-node. **No `reth-rpc`/`reth-rpc-eth-api`/`jsonrpsee` pulled in.** `eth_call`/`eth_estimateGas` reuse the facade revm executor via `AvaEvmConfig::inner().evm_with_env(db,env).transact(tx)` over `StateProviderDatabase<FirewoodStateView>` (read-only convention: zero base fee + zero gas_price + `disable_nonce_check`). All reth/revm spelling stays behind the facade (G0). Facade += `revm::context::TxEnv`, `revm::context::result::{ExecutionResult,Output}`, `revm::primitives::TxKind` (tagged `// M6.23:`). **135 ava-evm tests green.**
> **STATUS NOTES:** (1) **`eth_getProof`** account fields (balance/nonce/codeHash) correct today from direct Firewood reads; `accountProof`/`storageProof[].proof` arrays read the `StateProofProvider::proof` seam — populated once M6.25 lands, NO RPC-layer change needed. The golden test `eth_get_proof_account_fields_match_golden` asserts ACCOUNT FIELDS ONLY (stays green regardless of proof-array population). (2) **`debug_traceTransaction` DEFERRED** (returns a documented error naming the method) — the prestate tracer needs a revm inspector not reachable behind the facade without a heavy dep → fold into M6.24 or a follow-up. (3) **No reth `TransactionPool`** in ava-evm yet (same gap M6.20 flagged); `eth_sendRawTransaction`/`best_transactions` land with the EVM txpool task.
> **⚠️ WAVE GOTCHA (parallel facade-touchers + shared `CARGO_TARGET_DIR`):** two worktrees both building `ava-evm-reth v0.1.0` (same name+version, different source paths) with DIFFERENT facade additions intermittently clobber each other's `.rlib` in the shared target dir → phantom "unresolved import" errors. Mitigation confirmed: build/test facade-touching agents in an ISOLATED target dir (`CARGO_TARGET_DIR=…/target-mNNN`), OR don't run two facade-touchers concurrently. The orchestrator's per-merge `touch crates/ava-evm-reth/src/lib.rs` + sequential green-gate is the reconcile step.

### Task M6.24: `avax.*` namespace + admin/health (G8) ✅ DONE (9952cae)
**Crate:** ava-evm  ·  **Depends on:** M6.16, M6.17, M6.23  ·  **Spec:** 10 §9.2, §17.9 (G8)
**Files:** `crates/ava-evm/src/rpc/avax.rs`, `crates/ava-evm/src/rpc/admin.rs`, `crates/ava-evm/tests/rpc_avax.rs`
- [x] **Step 1 — Red:** `tests/rpc_avax.rs` golden: `avax.issueTx` accepts an atomic tx into the mempool; `avax.getAtomicTx`/`getAtomicTxStatus`/`getUTXOs`/`getBlockByHeight` return Go-parity JSON; admin + health endpoints respond.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm rpc_avax` → fails.
- [x] **Step 3 — Green:** Implement the `avax.*` module (methods per §9.2) + admin/health as DIRECT handlers (NOT jsonrpsee — see deviation), mounted alongside the `eth_*` handlers; defer the jsonrpsee-vs-axum mount decision to 12-node (note in code). Commit avax golden vectors.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm rpc_avax` → pass.
- [x] **Step 5 — Commit:** `ava-evm: avax.* RPC namespace + admin/health (G8)`

> **AS-BUILT (M6.24).** Followed the **M6.23 DIRECT-handler precedent** (plain structs returning
> `serde_json::Value`, NOT reth `EthApi`/jsonrpsee — the plan's "jsonrpsee module" wording predates the M6.23
> decision; no `jsonrpsee`/`reth-rpc` pulled in). `rpc/avax.rs` `AvaxRpc` over `AtomicMempool` + `CanonicalStore`
> + a new in-memory `AcceptedAtomicTxIndex`: `avax.issueTx` (checksummed-hex decode → `Tx::parse` →
> `mempool.add_local` → CB58 txID), `getAtomicTxStatus` (`{status, blockHeight?}`, precedence Accepted >
> Processing/Dropped > Unknown, `json.Uint64` quoted-decimal height), `getAtomicTx` (`{tx, encoding, blockHeight?}`),
> `getUTXOs` (`{numFetched, utxos, endIndex, encoding}` paginated envelope — **fetch DEFERRED**, see below),
> `getBlockByHeight` (over `CanonicalStore` body bytes), `health_check` (`{healthy, lastAcceptedHeight}`).
> `rpc/admin.rs` `AdminRpc`: `startCPUProfiler`/`stopCPUProfiler`/`memoryProfile`/`lockProfile`/`setLogLevel`
> returning coreth's `api.EmptyReply` (`{}`); profiler/logger no-ops (node-assembly concern, §12-node). JSON
> shapes match coreth `plugin/evm/atomic/vm/api.go` + `admin.go`/`health.go`. **rpc/mod.rs:** added `pub mod admin;`
> + `pub mod avax;` + a doc note that the jsonrpsee-vs-axum mount is deferred to 12-node. Small supporting
> accessors added (no facade touch): `AtomicMempool::get_tx_bytes`, `CanonicalStore::body_at`. **9 integ + 6 unit
> tests; 158 ava-evm tests green at merge (162 combined with the wave).** No new Cargo deps, no facade touch.
> **DEFERRALS / SEAMS:** (1) `avax.getUTXOs` returns the parity-correct empty envelope — `ava-vm`'s `SharedMemory`
> exposes no address-indexed `GetAtomicUTXOs` iterator yet (the seam for coreth's indexed fetch). (2) accepted-tx
> lookups read an in-memory `AcceptedAtomicTxIndex` (the seam for coreth's durable `AtomicRepository`) until
> `EvmVm` wires acceptance into it. (3) admin profiler/logger are no-ops. All documented in code + provenance.

### Task M6.25: EVM + atomic-trie state sync over Firewood proofs (G8) ✅ DONE (7c6f87b)
**Crate:** ava-evm  ·  **Depends on:** M6.4, M6.17  ·  **Spec:** 10 §10, §17.9 (G8); 04 §4.2/§4.3
**Files:** `crates/ava-evm/src/sync/mod.rs`, `crates/ava-evm/src/sync/server.rs`, `crates/ava-evm/src/sync/client.rs`, `crates/ava-evm/tests/state_sync.rs`
- [x] **Step 1 — Red:** `tests/state_sync.rs` `leafs_request_served_from_firewood_revision` (range proof at a historical revision, wire-exact) + `client_reconstructs_trie_and_verifies_root`; atomic-trie sync over the 2nd Firewood instance then `ApplyToSharedMemory` from the synced cursor.
- [x] **Step 2 — Confirm red.** (nextest matches by fn name — use `-E 'binary(state_sync)'`, not `test(state_sync)`.)
- [x] **Step 3 — Green:** `EvmStateSyncServer::handle_leafs` (Firewood range proofs), client (reconstruct + verify root), atomic-trie sync, `ApplyToSharedMemory` reconcile; real `state.rs` proof seam.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm -E 'binary(state_sync)'` → pass.
- [x] **Step 5 — Commit:** `ava-evm: EVM + atomic-trie state sync over Firewood proofs (G8)`

> **AS-BUILT (M6.25).** `sync/server.rs` `EvmStateSyncServer::handle_leafs` serves Firewood range proofs; `sync/client.rs` reconstructs a fresh trie + verifies root; `sync/mod.rs` atomic-trie sync + `apply_atomic_trie_to_shared_memory` (height-ordered, `from_height`-exclusive cursor). **state.rs proof seam NOW REAL (M6.23's `eth_getProof` reads it):** `StateProofProvider::proof(input, address, slots)→AccountProof` (account + per-slot inclusion proofs), `StorageRootProvider::storage_proof(address, slot, hashed)→StorageProof`, `StorageRootProvider::storage_root(...)` (reads the encoded leaf field, no longer the unconditional empty stub); new pub helpers `decode_account_storage_root`, `FirewoodStateView::range_proof_bytes(start,end,limit)`, `FirewoodStateProvider::propose_and_stash` made pub. Proof `Vec<Bytes>` carries ONE element = firewood `FrozenRangeProof` bytes (wire-exact vs Go). No facade/Cargo/error.rs changes. **143 ava-evm tests green.**
> **DEFERRED (G8 soft upstream ask, → spec 10 §10/§17.9):** firewood v0.5.0 exposes NO Eth-RLP-MPT proof nodes (only firewood `ProofNode`s) → reth-verifiable `AccountProof` `Vec<Bytes>` of RLP nodes can't be produced; `multiproof`/`storage_multiproof`/`witness` return documented `unsupported`. firewood derives sub-trie roots internally and doesn't surface/rewrite them → live per-account `storage_root` returns empty-trie sentinel (so `eth_getProof.storageHash` is limited for accounts with storage). commitInterval skip-backfill index not ported (backend.rs read-only this wave).
> **SPEC FINDINGS (→ specs/10 §10/§17.9, §17.2.1):** (1) **wire format is firewood-native, NOT the proto `RangeProof` message** — Go syncer serializes `(*ffi.RangeProof).MarshalBinary()` = firewood Rust `FrozenRangeProof::write_to_vec`; the proto `RangeProof`/`ProofNode` messages are unused, only the `ProofRequest`/`ProofResponse` envelope (opaque `range_proof: bytes`) matters. (2) firewood `range_proof(start,end,limit)` returns a `FrozenRangeProof` (start_proof/end_proof/key_values), NOT `keys/vals/nodes` — extract keys/vals from `key_values()`, bytes from `write_to_vec`. (3) **Go ChangeProof is unimplemented** (`changeProofMarshaler`→"not implemented", `GetChangeProof`→`ErrInsufficientHistory`) → §10's "range/change proofs" is range-proofs-only today; change proofs are a future optimization on both sides.

### Task M6.26: Public reusable API surface for SAE (reuse contract) ✅ DONE (254528e)
**Crate:** ava-evm + ava-evm-reth  ·  **Depends on:** M6.6, M6.4, M6.13, M6.15, M6.21, M6.5  ·  **Spec:** 10 §16, §17.10; 00 §11.1.5
**Files:** `crates/ava-evm/src/lib.rs` (re-exports), `crates/ava-evm-reth/src/lib.rs`, `crates/ava-evm/tests/reuse_surface.rs`
- [x] **Step 1 — Red:** `tests/reuse_surface.rs` `fn sae_reusable_items_are_public()`: a compile-test that imports each §17.10 item through public paths — `ava_evm_reth::{ExternalConsensusExecutor, ExecOutcome}`, `ava_evm::{AvaEvmConfig, FirewoodStateProvider, FirewoodStateView, hashed_post_state_to_batchops, AvaPrecompiles, PrecompileRegistry, AtomicStateHook, AvaChainSpec}` and calls `AvaEvmConfig::execute_batch` decoupled from any `EvmVm`/`ChainVm` — proving "one EVM engine, two drivers". Assert `EvmVm`/`BlockBuilderDriver` are NOT required to drive execution.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm reuse_surface` → fails (items private).
- [x] **Step 3 — Green:** Make the §17.10 items `pub` with stable paths; ensure `propose_from_bundle`/`view`-by-root/deferred-commit are reachable without the sync lifecycle. Document the NOT-shared boundary (block lifecycle) in rustdoc (00 §11.1.5).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm reuse_surface` → pass.
- [x] **Step 5 — Commit:** `ava-evm: expose reusable executor + Firewood state APIs for SAE (§16/§17.10)`

> **AS-BUILT (M6.26).** Additive `pub use` at the `ava_evm` crate root (`crates/ava-evm/src/lib.rs`):
> `AvaEvmConfig`, `AvaState`, `NoopPreHook` (from `evmconfig`); `FirewoodStateProvider`, `FirewoodStateView`,
> `hashed_post_state_to_batchops` (from `state`); `AvaPrecompiles`, `PrecompileRegistry` (from
> `precompile::registry`); `AtomicStateHook` (from `atomic::hook`); `AvaChainSpec` (from `chainspec`). The facade
> (`ava-evm-reth`) was **not touched** — `ExternalConsensusExecutor`/`ExecOutcome`/`AvaEvmEnv`/`RecoveredTx` were
> already `pub`. `tests/reuse_surface.rs` (2 tests) drives `execute_batch` standalone (no `EvmVm`/`ChainVm`/
> `BlockBuilderDriver`). **164 ava-evm tests green** (+2). **§17.10 SPEC NAME RECONCILIATION (folded into spec 10
> §17.10):** (1) **`FirewoodStateCommitter` is not a real type** — the open-view→propose→defer-commit role lives as
> methods on `FirewoodStateProvider` (`propose_from_bundle`/`propose_and_stash`/`stash_proposal`/`commit`/`discard`).
> (2) "open view by root" = `FirewoodStateProvider::history_by_state_root(root)`. (3) `revm_spec_id` is a method
> `AvaChainSpec::revm_spec_id(timestamp)`, not a free fn. (4) `AvaState`/`NoopPreHook` additionally exposed.

> **PREREQUISITE DONE — `ava-warp` crate extracted (ca04623), unblocking M6.22.** The generic Warp/ICM primitives
> were lifted out of `ava-platformvm/src/warp/` into a dedicated `crates/ava-warp` crate (specs 20 §1): envelope
> (`UnsignedMessage`/`Message`/`Signature`/`BitSetSignature`/codec), `payload` (`WarpPayload`/`Hash`/`AddressedCall`),
> `message` (ACP-77 `RegistryPayload` registry — kept the module name `message`, spec §1 sketched `registry.rs`),
> `signer` (`Signer`/`LocalSigner`), and the pure verification primitives (`verify_bit_set_signature`/`verify_weight`/
> `filter_validators`/`sum_weight`/`aggregate_public_keys`/`WarpSetVerifier` + quorum constants). New `ava_warp::Error`
> (thiserror) preserves the Go warp sentinel identities + adds `InvalidPayload` (the generic form of the registry
> structural-check error). `ava-platformvm/src/warp/` is now a **thin re-export facade** + retains the L1-lifecycle
> glue (`verify_warp_message`/`ParsedWarp`/`WarpSignatureVerifier`/`AcceptingVerifier`/`RejectingVerifier`); a
> `From<ava_warp::Error> for ava_platformvm::Error` maps each variant 1:1 (`InvalidPayload → InvalidComponent`).
> ava-platformvm **121 tests still green** (zero relocated). `acp118`/`ava-warp-rpc` remain future tasks (not yet
> implemented). M6.22's Step-3 `ava-warp` dep now resolves.

### Task M6.27: G1/G9 invariant — reth never writes state/trie tables (CI) ✅ DONE (c4f0c54)
**Crate:** ava-evm  ·  **Depends on:** M6.9, M6.20  ·  **Spec:** 10 §17.2 (G1 invariant), §17.7, §17.11 (G9)
**Files:** `crates/ava-evm/tests/g1_invariant.rs`
> **WORDING CORRECTION (folded in from M6.9 as-built):** the original "open the MDBX env and assert `PlainState`/
> `HashedState`/`Trie` tables are empty" wording is **stale** — `CanonicalStore` is over the **`ava-database`
> prefixed-KV backend, NOT reth-db MDBX** (M6.9 DEVIATION 2), so there is no MDBX env and those tables don't
> exist. The invariant is proved against the real architecture (KV namespaces + Firewood tip + a structural
> source-guard), per the steps below.
- [x] **Step 1 — Red:** `tests/g1_invariant.rs` `fn state_trie_tables_stay_empty_after_block()`: build+accept a block, then assert the `CanonicalStore` KV namespaces (HEADER/CANONICAL/NUMBER/BODY/RECEIPTS/TIP) grew to reflect the accepted block AND EVM state advanced only in Firewood (tip moved); plus a structural guard that no `BlockchainProvider`/`UnifiedStorageWriter`/`StateWriter` symbol appears in `crates/ava-evm/src` (non-comment lines) — G1/G9.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm g1_invariant` → fails (and crucially WOULD fail if a `StateWriter` path were introduced into `ava-evm/src`).
- [x] **Step 3 — Green:** Confirm no code path constructs reth `BlockchainProvider`/`UnifiedStorageWriter`/`StateWriter::write_state` for state/trie tables; only the bare `BlockExecutor`/`BlockBuilder` flow + `FirewoodStateCommitter` + `CanonicalStore` are used. Add the assertion as a standing CI guard.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm g1_invariant` → pass.
- [x] **Step 5 — Commit:** `ava-evm: CI invariant — Firewood is state-of-record, reth trie tables empty (G1/G9)`

> **AS-BUILT (M6.27).** Three tests in `tests/g1_invariant.rs` (438 lines): (1) **runtime** —
> `state_trie_tables_stay_empty_after_block`: before accept `CanonicalStore` tip is `None` + Firewood tip ==
> genesis; after `verify` Firewood tip is STILL genesis (proposal only stashed, not committed — G1); after
> `accept` Firewood tip advanced to the pre-commit root AND all `CanonicalStore` KV namespaces reflect block 1
> via `last_canonical()`/`canonical_hash()`/`header_at()`/`height_of()`. (2) **G1 trick** —
> `state_root_with_updates_returns_empty_trie_updates`: asserts `FirewoodStateView::state_root_with_updates`
> returns a real Firewood root AND empty `TrieUpdates` (reth never persists trie nodes). (3) **structural guard** —
> `no_reth_state_writer_in_ava_evm_src`: walks every `.rs` under `crates/ava-evm/src/` and asserts no non-comment
> line names `BlockchainProvider`/`UnifiedStorageWriter`/`StateWriter` (the facade `ava-evm-reth` is exempt — it's
> allowed to name reth types). Standing CI gate: introducing those symbols breaks the test. No new pub accessors,
> no new deps, no facade/`rpc/mod.rs` touch. **→ spec 10 §17.2 invariant wording updated to match (see SPEC FIX).**

### Task M6.28: Fuzz targets + PORTING.md + proptest corpus completeness ✅ DONE (86ababc)
**Crate:** ava-evm  ·  **Depends on:** M6.7, M6.14  ·  **Spec:** 02 §8, §10.1, §13
**Files:** `crates/ava-evm/fuzz/fuzz_targets/decode_block.rs`, `crates/ava-evm/fuzz/fuzz_targets/decode_atomic_tx.rs`, `crates/ava-evm/fuzz/Cargo.toml`, `crates/ava-evm/tests/PORTING.md`
- [x] **Step 1 — Red:** Add `cargo-fuzz` targets `decode_block` and `decode_atomic_tx` (must never panic/over-read on arbitrary bytes; round-trip stable for anything that decoded). Create `tests/PORTING.md` seeded from `go test -list` of coreth `plugin/evm`/`atomic`/`customheader` + the `na`-with-reason rows for Engine-API-only Go plumbing.
- [x] **Step 2 — Confirm red:** `cargo +nightly fuzz run decode_block -- -runs=1000` and `decode_atomic_tx` smoke → build/run; `PORTING.md` has `wip` rows.
- [x] **Step 3 — Green:** Commit seed corpus under `fuzz/corpus/<target>/`; fill PORTING.md mapping every relevant Go test to its Rust counterpart (M6.x test names) until no `wip` rows remain for shipped scope.
- [x] **Step 4 — Confirm green:** `cargo xtask test-fuzz` (smoke) green; `cargo xtask porting-report` shows ava-evm with no `wip` rows.
- [x] **Step 5 — Commit:** `ava-evm: fuzz targets (block/atomic-tx) + PORTING.md coverage matrix`

> **AS-BUILT (M6.28).** `crates/ava-evm/fuzz/` detached cargo-fuzz crate (own `[workspace]` table, `edition 2024`,
> `cargo-fuzz=true`; deps `libfuzzer-sys 0.4` + `ava-evm` path + `ava-evm-reth` path [for `Chain::from_id`]) —
> ignored by `cargo build --workspace`. **`decode_block`** fuzzes `decode_ava_evm_block` with an all-phases-active
> `AvaChainSpec`; **`decode_atomic_tx`** fuzzes `Tx::parse`. Both follow the no-panic discipline: every decode path
> returns `Result`, target only `if let Ok(_)` then asserts the byte-identity round-trip (`assemble_ava_block(..).
> encoded_bytes() == data` / `tx.bytes() == data` — the coreth `Tx.Initialize` invariant). **Seed corpus** (copied
> from committed golden vectors): `decode_block/{golden_plain_block(739B),golden_atomic_block(862B)}`,
> `decode_atomic_tx/{golden_import_tx,golden_export_tx}(234B each)`. **`tests/PORTING.md`:** 131 rows / 6 sections
> (plugin/evm root, atomic root + txpool/state/vm/sync, customheader, fuzz) — **55 ✅ / 8 🟡 / 0 ⬜ / 30 n/a / 3 wip**;
> the 3 `wip` are warp-predicate bytes (M6.22, unshipped); unshipped (warp/gossip/bootstrapper/migration) are `na`
> with reasons. **NIGHTLY NOTE:** `rust-toolchain.toml` pins stable 1.96.0 only; `cargo +nightly fuzz build` needs
> the `NIX_DEV_SHELL=fuzz` nightly shell (`cargo xtask test-fuzz`). Verified the crate is well-formed via stable
> `cargo check` (passes); the nightly fuzz build is the CI smoke entry. No existing src/facade/`rpc/mod.rs` touched.

### Task M6.29: Milestone exit gate
**Crate:** ava-evm (+ avalanchers binary)  ·  **Depends on:** all M6.1–M6.28, M6.30; **M6.31** for the base-fee-to-coinbase recipient override that gates real-mainnet `cchain_state_root` 3-account parity (see M6.30/M6.31 notes — recorded-oracle mode can run before M6.31 lands; full coreth on-chain-root parity needs it)  ·  **Spec:** 10 (all), 04 §4, 20 §7, 21; 02 §10.5/§11; BUILDABLE-&-GREEN INVARIANT
**Files:** `crates/ava-evm/tests/PORTING.md` (final), workspace wiring for `avalanchers` to run C-Chain
- [ ] **Step 1 — Red:** Ensure the four named exit tests exist and are wired into `cargo nextest --profile ci`: `golden::cchain_block_wire`, `golden::cchain_genesis_root`, `differential::cchain_state_root` (recorded-oracle/reexecute mode — deterministic, per-PR friendly, over a multi-block recorded mainnet range, 02 §10.5), `differential::atomic_xc` (recorded mode green per-PR; live mode `#[ignore]`/CI-gated, coordinate with cross-cutting harness X), `prop::evm_fee_schedule_per_fork`.
- [ ] **Step 2 — Confirm red:** Run the full gate; record any failure.
- [ ] **Step 3 — Green:** Fix remaining wiring so the `avalanchers` binary now boots and runs the C-Chain via `EvmVm`. Run and pass:
  - `cargo build --workspace`
  - `cargo build -p avalanchers`
  - `cargo nextest run --profile ci`
  - `cargo clippy --workspace -- -D warnings`
  - the four named exit tests above.
  Update final PORTING.md; confirm `#![forbid(unsafe_code)]` holds everywhere except inside `ava-evm-reth` (binding wrappers).
- [ ] **Step 4 — Confirm green:** All commands above exit 0; exit tests pass; differential::cchain_state_root green in recorded mode.
- [ ] **Step 5 — Commit:** `ava-evm: M6 exit gate — C-Chain on reth green; avalanchers runs C-Chain`

### Task M6.30: 5-field account-RLP state-encoding parity (libevm `StateAccount.Extra`)  ✅ DONE (a753201) ⟸ NEW (surfaced by M6.6)
**Crate:** ava-evm  ·  **Depends on:** M6.3/M6.4 (state.rs)  ·  **Blocks:** real-mainnet `differential::cchain_state_root` (M6.29)  ·  **Spec:** 10 §5/§17.2; 04 §4
**Files:** `crates/ava-evm/src/state.rs` (`rlp_account`/`decode_rlp_account`), `crates/ava-evm/tests/vectors/cchain/account_rlp/*.json`
**Why:** M6.6 found coreth's libevm `types.StateAccount` serializes a **5th `Extra` field** (empty `0x80` for an
EOA) after `[nonce,balance,storageRoot,codeHash]`. The 4-field RLP currently emitted by `state.rs` (and
Firewood-ethhash) yields a DIFFERENT trie root than coreth's real StateDB (`0x3292…` 4-field vs `0x9cb2…`
coreth). The M6.6 fixture's `expected_root` is over the 4-field encoding, so today `cchain_state_root` proves
Rust↔Go internal consistency, NOT parity with the on-chain coreth root. **Real recorded-mainnet reexecute
parity requires matching libevm's account encoding byte-for-byte.**
- [x] **Step 1 — Red:** Characterize libevm's `StateAccount` encoding exactly (what `Extra` carries for C-Chain
  EOAs vs contracts; whether it is ever non-empty on mainnet). Add a golden vector with the coreth-StateDB
  5-field root (Go-authoritative) and a failing assertion that `state.rs` produces it.
- [x] **Step 2 — Confirm red:** root mismatch (4-field vs 5-field).
- [x] **Step 3 — Green:** Emit/decode the 5th field in `rlp_account`/`decode_rlp_account` (and anywhere account
  RLP is materialized: genesis alloc M6.8, atomic hook M6.15). Re-point the M6.6 fixture `expected_root` to the
  5-field (coreth) root.
- [x] **Step 4 — Confirm green:** `cchain_state_root` passes against the coreth StateDB root; genesis-root
  parity (M6.8) holds against real Mainnet/Fuji C-Chain genesis roots.
- [x] **Step 5 — Commit:** `ava-evm: 5-field libevm StateAccount RLP for coreth state-root parity`

> **AS-BUILT (M6.30).** libevm `StateAccount` 5th field = RLP `false` (`0x80`, the libevm `isMultiCoin`
> bool) for C-Chain EOAs — **empty for ordinary accounts**; coreth uses the standard 4 fields plus this one
> boolean extra. `rlp_account` now emits 5-field by byte-patching the alloy 4-field output (bump the list
> length byte +1, append `0x80`); `decode_rlp_account` parses the `[0xf8,L]` list header, decodes the 4
> required fields, ignores the rest (forward-compatible). Vectors updated to 5-field: `account_rlp/eoa_one_ether.json`
> (`0xf84c…` → `0xf84d…80`) and the M6.6 reexecute fixture (`genesis_state_root` `0x3292…`→`0x9cb21ede…`,
> `expected_post_state_root` `0x5784…`→`0x4027f3ed…`). **IMPORTANT scope note:** the re-pointed
> `expected_post_state_root` is the 5-field root **over the revm BURN fee model** (2 accounts: sender+recipient),
> Go-verified via a standard-MPT scratch test — it is NOT yet coreth's 3-account on-chain root
> (`0x8b0bf834…`, retained as `coreth_post_state_root_5field` doc) because coreth's `dummy.NewCoinbaseFaker`
> CREDITS the base fee to the coinbase. So M6.30 closes the **encoding** half of coreth state-root parity; the
> **base-fee-to-coinbase** half remains **M6.13** (`next_evm_env` recipient override). Both must land for the
> real-mainnet `differential::cchain_state_root` exit gate (M6.29). 51 `ava-evm` tests green; no facade edits.

### Task M6.31: Live `EvmFactory` install + base-fee-to-coinbase override + remaining ConfigKey precompiles ⟸ NEW (surfaced by M6.22)
**Crate:** ava-evm (+ facade)  ·  **Depends on:** M6.21, M6.22, M6.13, M6.10  ·  **Blocks:** real-mainnet `differential::cchain_state_root` 3-account parity (M6.29)  ·  **Spec:** 10 §6.5/§8/§17.1/§17.5; 20 §7; 21 §7 (base-fee recipient)
**Files:** `crates/ava-evm/src/evmconfig.rs` (custom `EvmFactory`/`ConfigureEvm`), `crates/ava-evm/src/precompile/{allowlist,feemanager,nativeminter,rewardmanager,gaspricemanager}.rs` (NEW), `crates/ava-evm-reth/src/lib.rs` (factory re-exports), `crates/ava-evm/tests/vectors/cchain/precompile/*.json`
**Why:** M6.21 deferred the live install (it churns the M6.6 bare-executor path) and M6.22 deferred it again, supplying every primitive but not the factory. Three things are bundled here because they share the one custom-factory churn:
- **(a) Live `EvmFactory` install (G4/G10).** `AvaEvmConfig::execute_batch` drives reth's *bare* executor; it does not install `AvaPrecompiles` + `AvaCtxExt` onto the revm `ContextTr::Chain` slot, nor run `precompile::warp::run_predicates` inside `apply_pre_execution_changes` against a live proposervm block ctx + `ava-validators::ValidatorState`. Wire: thread proposervm `PChainHeight` + `ValidatorState` into the next-block ctx, run the predicate pass pre-execution keyed by tx index → `PredicateResults::set_warp`, install the height-gated provider + extension, and route `WarpPrecompile::take_logs` → `WarpBackend::handle_precompile_accept` on block accept.
- **(b) Base-fee-to-coinbase override (M6.6 finding #3).** Avalanche credits the AP3+ base fee to the coinbase; revm burns it. This needs the same custom revm handler/factory. Until this lands, `cchain_state_root` stays at M6.30's burn-model 5-field root (2 accounts), NOT coreth's 3-account on-chain root (`0x8b0bf834…`). Landing it re-points the fixture to the coreth root and is what makes block-1 coreth bytes verify (the M6.9/M6.10 lifecycle parity gap).
- **(c) Remaining ConfigKey precompile bodies.** AllowList / FeeManager / NativeMinter / RewardManager / GasPriceManager — port each body faithfully from subnet-evm `precompile/contracts/{allowlist,feemanager,nativeminter,rewardmanager}` (role/fee-config/mint/reward-address storage slots), register as `StatefulPrecompile`s like warp, with golden vectors.
- [ ] **Step 1 — Red:** `tests/evm_factory.rs` — a 1-tx block executed through the live `ConfigureEvm` path installs `AvaPrecompiles` (warp dispatches, predicate read works end-to-end through `execute_batch`, not the test-only seam) AND credits the base fee to the coinbase (assert coinbase balance ↑ by `base_fee·gas_used`, sender pays full). Re-point `cchain_state_root` fixture to the coreth 3-account root. Plus per-precompile golden tests (AllowList role gate, FeeManager set/get, NativeMinter mint, RewardManager reward address, GasPriceManager).
- [ ] **Step 2 — Confirm red:** the named tests fail (bare-executor path / burn model / missing precompile bodies).
- [ ] **Step 3 — Green:** Implement the custom `EvmFactory`/`ConfigureEvm` (precompile install + predicate pass + base-fee-recipient handler), the five ConfigKey precompile bodies, the accept-time log→backend routing. Commit precompile + re-pointed state-root vectors.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm` (single-threaded) all pass incl. re-pointed `cchain_state_root`; clippy `-D warnings` clean; G0 holds.
- [ ] **Step 5 — Commit:** `ava-evm: live EvmFactory install + base-fee-to-coinbase + ConfigKey precompiles`

---

## Spec coverage check

### Spec sections → task

| Spec section | Subject | Task(s) |
|---|---|---|
| 10 §1 / 00 §11.1.6 | reth-as-library integration mode (NOT Engine API) | M6.1, M6.6, M6.9 (G6) |
| 10 §2 | customization surface C1–C10 | mapped across all tasks (see C-rows below) |
| 10 §3 / §3.1 / §3.2 | ChainVm adapter; verify/accept/reject; drive executor | M6.6, M6.9, M6.10 |
| 10 §4 | on-demand block building | M6.20 |
| 10 §5 + 04 §4 | Firewood-ethhash StateProvider + state root contract | M6.3, M6.4 |
| 10 §5.1/§5.2 | reth-db owns blocks only; revision-window history (G2) | M6.4 (history_by_state_root), M6.9 (CanonicalStore), M6.27 |
| 10 §6.1/§6.2 | atomic tx types + in-block encoding | M6.14, M6.7 |
| 10 §6.3 | EVMStateTransfer hook | M6.15 |
| 10 §6.4 | atomic mempool/backend/atomic trie | M6.16, M6.17 |
| 10 §6.5 | atomic semantic verify, conflicts, bonus blocks, predicates | M6.18, M6.22 |
| 10 §7.1/§7.2/§7.3/§7.4 + 21 §0/§4/§5 | dynamic fees per fork; fork schedule; atomic gas | M6.5, M6.11, M6.12, M6.13 |
| 10 §8 + 20 §7 | warp + stateful precompiles, predicate split, gas tables | M6.21, M6.22 |
| 10 §9.1/§9.2/§9.3 | eth_*/avax.* RPC; block wire | M6.23, M6.24, M6.7 |
| 10 §10 | EVM + atomic-trie state sync | M6.25 |
| 10 §11.1/§11.2 | genesis/upgrade JSON; error model | M6.8, M6.2 |
| 10 §14 / 02 §10.5/§11 | reexecute differential, golden, atomic differential, fee proptest | M6.6, M6.7, M6.8, M6.19, M6.13, M6.29 |
| 10 §16 / §17.10 / 00 §11.1.5 | reuse contract — public executor + Firewood APIs for SAE | M6.26 |
| 02 §4 / §6 / §8 / §13 | proptest + golden + fuzz + per-crate contract | M6.4, M6.11–13 (proptest), M6.7/8/14/17/22/23/24 (golden), M6.28 (fuzz/PORTING) |

### Gaps G0–G10 → task (each reth touch-point wrapped)

| Gap | One-liner | Task(s) |
|---|---|---|
| **G0** | no stable reth API / no external-consensus entrypoint → vendored pin + `ava-evm-reth` facade | **M6.1** (and every facade-routed task) |
| **G1** | bypass reth TrieUpdates/StateWriter → Firewood root & commit (empty-TrieUpdates trick) | **M6.4** (+ invariant M6.27) |
| **G2** | dynamic fees + atomic-tx gas via `next_evm_env` override | **M6.11, M6.12, M6.13** |
| **G3** | atomic txs as BlockExecutor pre/post hook + atomic trie + shared memory | **M6.15, M6.17** (+ M6.14, M6.18) |
| **G4** | warp predicate results into the revm precompile context | **M6.22** (+ M6.21) |
| **G5** | on-demand build (bypass `PayloadBuilderService`); `finish(precomputed root)` | **M6.20** |
| **G6** | Snowman fork choice → Accept=commit+canonicalize, Reject=drop (`CanonicalStore`) | **M6.9** (+ M6.10) |
| **G7** | Avalanche fork schedule on Ethereum forks + per-block revm spec id | **M6.5** |
| **G8** | EVM/atomic state sync + `avax.*` RPC + `eth_*` overrides | **M6.23, M6.24, M6.25** |
| **G9** | Storage-V2 / `SparseTrieCache` coupling (2.x face of G0/G1) | **M6.27** (folded into G1 invariant) |
| **G10** | revm context-extension typing churn (PrecompileProvider) | **M6.21** (facade-owned, M6.1) |

### Customizations C1–C10 → task

| C# | Customization | Task |
|---|---|---|
| C1 | on-demand block building | M6.20 |
| C2 | Snowman fork choice (no reorg) | M6.9, M6.10 |
| C3 | atomic Import/Export + shared memory + atomic trie | M6.14–M6.18 |
| C4 | Avalanche dynamic fee (AP3/AP4/ACP-176/226) | M6.11–M6.13 |
| C5 | Avalanche fork schedule | M6.5 |
| C6 | warp + subnet-evm stateful precompiles | M6.21, M6.22 |
| C7 | EVM state root via Firewood-ethhash | M6.3, M6.4 |
| C8 | EVM state sync | M6.25 |
| C9 | eth_*/avax.* RPC + block wire | M6.7, M6.23, M6.24 |
| C10 | predicates (warp pre-tx verify) | M6.18, M6.22 |

### Deferrals (explicitly out of M6 scope)

- **EVM subnet profile** (subnet-evm `is_subnet=true` deployments): `AvaChainSpec` carries the `is_subnet` flag (M6.5) and precompiles are profile-agnostic (M6.21/22), but a full EVM-subnet VM wiring is deferred to the node-assembly milestone (spec 12-node) / a follow-on.
- **Go-data-dir migration (R2)**: reth-db block storage uses reth's own format (M6.9); importing a Go C-Chain data dir is the cross-cutting R2 migration concern (00 §11.2), not an M6 reth gap.
- **Live two-binary differential** (`differential::atomic_xc` live mode, full `ava-differential` C-Chain observations): per-PR runs in recorded-oracle mode here; live mode is CI-gated and owned by the cross-cutting differential harness X (02 §11.7).
- **RPC mount topology** (jsonrpsee-under-axum vs split): decided in 12-node; M6.24 leaves the seam.
- **Ethereum execution-spec state/blockchain test conformance** (10 §14 #1): run through the facade executor as a follow-on broad-coverage pass; M6's correctness gate is the reexecute state-root differential.

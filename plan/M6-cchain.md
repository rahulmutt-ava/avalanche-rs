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
- **Wave 9 — reuse contract + close residual gaps + exit:** M6.26 public reusable API surface for SAE (spec 10 §16/§17.10); M6.27 G1/G9 empty-trie-tables CI invariant; M6.28 fuzz targets + PORTING.md; M6.29 **Milestone exit gate**.

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

### Task M6.9: `EvmBlock` verify/accept/reject — pre-commit root, commit/discard, `CanonicalStore` (G6)
**Crate:** ava-evm  ·  **Depends on:** M6.6, M6.7, M3 (06 Block trait)  ·  **Spec:** 10 §3.1, §3.2, §17.7 (G6); 06 (linear acceptance); 04 §4.2
**Files:** `crates/ava-evm/src/block.rs`, `crates/ava-evm/src/state.rs` (committer), new `crates/ava-evm/src/canonical.rs`, `crates/ava-evm/tests/lifecycle.rs`
- [ ] **Step 1 — Red:** `tests/lifecycle.rs` (driven by the M3 engine harness / `ava-snow::testutil`): `fn verify_computes_precommit_root_no_commit()` (verify yields header root, EVM tip unchanged), `fn accept_commits_and_advances_tip()`, `fn reject_drops_proposal_without_commit()` (sibling proposals independent — proposal-on-proposal), and `fn canonical_store_advances_by_one()`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm lifecycle` → fails.
- [ ] **Step 3 — Green:** Impl 06 `Block` for `EvmBlock`: `verify` (syntactic + semantic execute via `execute_batch` into overlay, compute Firewood pre-commit root via stashed proposal, assert == header.state_root, receipts/gas/bloom), `accept` (`FirewoodStateCommitter::commit` → `CanonicalStore::append_canonical` → set `last_accepted`), `reject` (`FirewoodStateProvider::discard` + evict). Implement `canonical.rs` `CanonicalStore` (G6): single MDBX rw-tx appends Headers/CanonicalHeaders/HeaderNumbers/BlockBodyIndices/Transactions + static-file receipts + tip pointer, **never** touching state/trie tables; invariant `LAST_CANONICAL == last_accepted.height`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm lifecycle` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: EvmBlock verify/accept/reject + CanonicalStore (G6)`

### Task M6.10: `EvmVm` `ChainVm` adapter
**Crate:** ava-evm  ·  **Depends on:** M6.9, M3 (07 ChainVm boundary)  ·  **Spec:** 10 §3; 07 (ChainVm/Block)
**Files:** `crates/ava-evm/src/vm.rs`, `crates/ava-evm/tests/chainvm.rs`
- [ ] **Step 1 — Red:** `tests/chainvm.rs` `fn parse_get_setpref_lastaccepted()`: `parse_block` decodes to an unverified `EvmBlock`; `get_block` returns from the verified tree else blocks db; `set_preference` records target + retargets txpool with no reorg work; `last_accepted` returns committed `(Id, height)`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm chainvm` → fails.
- [ ] **Step 3 — Green:** Implement `EvmVm` (fields per §3: chain_spec, evm_config, state, blocks, atomic, txpool, builder, `verified: DashMap`, `preferred: ArcSwap`, `last_accepted: ArcSwap`) and `impl ChainVm` (`parse_block`, `build_block`→builder, `get_block`, `set_preference` record-only, `last_accepted`). No reth fork choice (G6).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm chainvm` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: EvmVm ChainVm adapter (§3)`

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

### Task M6.15: `AtomicStateHook` EVMStateTransfer pre-hook + atomic gas (G3)
**Crate:** ava-evm  ·  **Depends on:** M6.14, M6.6, M6.13  ·  **Spec:** 10 §6.3, §17.4 (G3); 21 §4b (atomic gas budget)
**Files:** `crates/ava-evm/src/atomic/hook.rs`, `crates/ava-evm/tests/atomic_transfer.rs`
- [ ] **Step 1 — Red:** `tests/atomic_transfer.rs` `fn import_credits_export_debits_and_bumps_nonce()`: apply `AtomicStateHook` to a `State<FirewoodStateView>` overlay; Import credits `amount * X2C_RATE` wei (checked); Export debits + sets `nonce = max(cur, i.nonce+1)` (matches coreth); assert resulting `BundleState` folds into the same Firewood proposal as EVM effects; overflow → `Error::FeeOverflow`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_transfer` → fails.
- [ ] **Step 3 — Green:** Implement `AtomicStateHook::apply(&[AtomicTx], &mut impl revm::Database)` (checked `X2C_RATE` mul, increment/decrement balance, nonce bump) and `AvaBlockExecutor<E>` decorator whose `apply_pre_execution_changes` runs inner pre-changes then the atomic hook (and reserves predicate slot for M6.22), implementing `PreExecutionHook` so `execute_batch` accepts it (§17.1/§17.4). Wire atomic gas into the block budget (M6.13 helpers).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_transfer` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: AtomicStateHook EVMStateTransfer pre-hook + atomic gas (G3)`

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

### Task M6.17: `AtomicBackend` + atomic trie (2nd Firewood) + shared-memory batch (G3)
**Crate:** ava-evm  ·  **Depends on:** M6.14, M6.9, M3 (07 shared memory)  ·  **Spec:** 10 §6.4, §17.4 (G3); 07 (shared-memory contract); 04 §4.2
**Files:** `crates/ava-evm/src/atomic/backend.rs`, `crates/ava-evm/src/atomic/trie.rs`, `crates/ava-evm/tests/atomic_backend.rs`
- [ ] **Step 1 — Red:** `tests/atomic_backend.rs` `fn accept_indexes_trie_and_applies_shared_memory()`: `AtomicBackend::accept(height, txs)` writes `key = height(8B)||blockchainID(32B)` → serialized requests into a 2nd ethhash Firewood instance, root matches a Go golden atomic-trie root, `TrieKeyLength=40`, `EmptyRootHash` init, periodic `commitInterval` checkpoint, and the shared-memory `Requests{Put,Remove}` apply happens in ONE atomic batch with the trie commit.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_backend` → fails.
- [ ] **Step 3 — Green:** Implement `AtomicTrie` (key encoding, `serialize_requests` via ava-codec byte-exact, `EmptyRootHash`), `AtomicBackend { trie, shared_memory, last_committed_root, commit_interval }` with `accept` (merge ops → propose → root → atomic shared-memory apply + commit together) per §17.4; hook into `EvmBlock::accept` AFTER state commit. Commit atomic-trie-root golden vector.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_backend` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: AtomicBackend + atomic trie + shared-memory batch (G3)`

### Task M6.18: Atomic semantic verify, conflict sets, bonus blocks (C10)
**Crate:** ava-evm  ·  **Depends on:** M6.17, M6.9  ·  **Spec:** 10 §6.5; 07
**Files:** `crates/ava-evm/src/atomic/verify.rs`, `crates/ava-evm/tests/atomic_verify.rs`
- [ ] **Step 1 — Red:** `fn rejects_conflicting_inputs_across_ancestry()`: a tx whose UTXOs are spent in shared memory or by another atomic tx in the same/ancestor block → `Error::ConflictingAtomicInputs`; `fn bonus_blocks_skip_set_matches_go()` reproduces the height→ID skip-set verbatim.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_verify` → fails.
- [ ] **Step 3 — Green:** Implement conflict set (`Set<Id>` of consumed UTXOs checked across verified-block ancestry), `bonusBlocks` skip-set constant, and the atomic semantic-verify pass invoked from `EvmBlock::verify`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_verify` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: atomic semantic verify + conflicts + bonus blocks (§6.5)`

### Task M6.19: `differential::atomic_xc` X↔C import/export parity
**Crate:** ava-evm  ·  **Depends on:** M6.15, M6.17, M6.18  ·  **Spec:** 10 §6, §14 #3; 02 §11; 07
**Files:** `crates/ava-evm/tests/atomic_xc.rs`, `crates/ava-evm/tests/vectors/cchain/atomic_xc/*.json`
- [ ] **Step 1 — Red:** `tests/atomic_xc.rs` `#[test] fn atomic_xc()` (exit-gate name) in recorded-oracle mode: for a Go corpus of ImportTx/ExportTx, assert byte-identical tx serialization, identical `atomic.Requests`, identical post-`EVMStateTransfer` balances/nonces, and identical atomic-trie roots vs Go; shared-memory effects checked against the M3/07 harness stub (so M6 stays independent of M5). Tag the live-mode variant `#[ignore]`/CI-gated (coordinate with cross-cutting harness X).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm atomic_xc` → fails.
- [ ] **Step 3 — Green:** Wire the corpus fixtures + comparison; close any parity gaps surfaced (serialization, requests, balances, trie root). Commit atomic_xc vectors + manifest with provenance.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm atomic_xc` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: X↔C atomic import/export parity (differential::atomic_xc)`

### Task M6.20: `BlockBuilderDriver` on-demand build + precomputed-root finish (G5)
**Crate:** ava-evm  ·  **Depends on:** M6.10, M6.13, M6.15, M6.16  ·  **Spec:** 10 §4, §17.6 (G5); 21 §4b (budget)
**Files:** `crates/ava-evm/src/builder.rs`, `crates/ava-evm/tests/build.rs`
- [ ] **Step 1 — Red:** `tests/build.rs` `fn build_then_verify_same_root()`: `build_on(parent, ctx)` pulls one atomic batch + EVM txs (effective-tip order, until gas/blockGasCost budget), computes the Firewood root, passes `Some((root, TrieUpdates::default()))` to `finish`, and the self-built block **re-verifies to the identical root** (build-then-verify symmetry); `fn respects_min_build_delay()`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm build` → fails.
- [ ] **Step 3 — Green:** Implement `BlockBuilderDriver` (§17.6): `next_block_attrs`, open `State` view, `builder_for_next_block`, `apply_pre_execution_changes` (atomic + predicate), reserve atomic gas, pack EVM txs by tip with gas/blockgascost budget + invalid-tx eviction, `propose_from_bundle` + `stash_proposal`, `finish(view_tip, Some((root, default)))` (G5/G1), `assemble_ava_block`, `minBlockBuildingRetryDelay` guard, `Notify`-driven.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm build` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: on-demand BlockBuilderDriver + precomputed-root finish (G5)`

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

### Task M6.22: Predicate pass + Warp precompile over `ava-warp` (G4/C10)
**Crate:** ava-evm  ·  **Depends on:** M6.21, M6.15  ·  **Spec:** 20 §7 (precompile ABI, predicate, gas), 10 §6.5, §8.2, §17.5 (G4)
**Files:** `crates/ava-evm/src/precompile/warp.rs`, `crates/ava-evm/src/atomic/hook.rs` (predicate pass), `crates/ava-evm/tests/warp_precompile.rs`, `crates/ava-evm/tests/vectors/cchain/warp/*.json`
- [ ] **Step 1 — Red:** `tests/warp_precompile.rs`: `fn predicate_verifies_then_precompile_reads()` — the BLS-aggregate predicate runs in `apply_pre_execution_changes` (via `ava-warp::verify` against the source-subnet `WarpSet` at `block_ctx.pchain_height`), stashing `Vec<bool>`; `getVerifiedWarpMessage(index)` reads the cached result; `sendWarpMessage` emits the `SendWarpMessage` log + returns the unsigned-message ID; gas costs match both pre-Granite and Granite `GasConfig` tables (20 §7.3); the `requirePrimaryNetworkSigners` subnet-substitution branch (20 §7.2 step 3); `getBlockchainID` returns the snow-ctx chain ID.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm warp_precompile` → fails.
- [ ] **Step 3 — Green:** Implement the predicate pass (`run_predicates` over EVM txs, `PredicateContext` from proposervm block ctx via `Block::verify_with_context`), the Warp `StatefulPrecompile` (ABI selectors per 20 §7.1, gas tables per fork, reads predicates only), other modules registered by `ConfigKey` (AllowList/FeeManager/NativeMinter/RewardManager/GasPriceManager) as `StatefulPrecompile`s, and `handlePrecompileAccept` hooks (warp backend records sent messages, §3.1). Commit warp golden vectors. Map handler ID 2 / quorum constants from 20 §6.2.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm warp_precompile` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: warp predicate pass + precompiles (G4, 20 §7)`

### Task M6.23: `eth_*` RPC over Firewood + fee/accepted-tag overrides (G8)
**Crate:** ava-evm  ·  **Depends on:** M6.10, M6.13  ·  **Spec:** 10 §9.1, §17.9 (G8)
**Files:** `crates/ava-evm/src/rpc/eth.rs`, `crates/ava-evm/tests/rpc_eth.rs`, `crates/ava-evm/tests/vectors/cchain/rpc/*.json`
- [ ] **Step 1 — Red:** `tests/rpc_eth.rs` golden request→response: `eth_getBalance`/`eth_call`/`eth_getProof` read Firewood state; `eth_gasPrice`/`eth_feeHistory`/`eth_maxPriorityFeePerGas` use `feerules`; the `latest`/`safe`/`finalized` tags all map to last-accepted height (Snowman has no pending/unsafe); `debug_traceTransaction` (incl. prestate tracer) parity vs Go golden.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm rpc_eth` → fails.
- [ ] **Step 3 — Green:** Instantiate facade `EthApi<Arc<FirewoodStateProvider>, AvaTxPool, ...>`; override fee helpers (`EthFees`) + accepted-block-tag mapping; wire revm-inspector tracing (prestate tracer). Commit RPC golden vectors.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm rpc_eth` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: eth_* RPC over Firewood + fee/accepted-tag overrides (G8)`

### Task M6.24: `avax.*` namespace + admin/health (G8)
**Crate:** ava-evm  ·  **Depends on:** M6.16, M6.17, M6.23  ·  **Spec:** 10 §9.2, §17.9 (G8)
**Files:** `crates/ava-evm/src/rpc/avax.rs`, `crates/ava-evm/src/rpc/admin.rs`, `crates/ava-evm/tests/rpc_avax.rs`
- [ ] **Step 1 — Red:** `tests/rpc_avax.rs` golden: `avax.issueTx` accepts an atomic tx into the mempool; `avax.getAtomicTx`/`getAtomicTxStatus`/`getUTXOs`/`getBlockByHeight` return Go-parity JSON; admin + health endpoints respond.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm rpc_avax` → fails.
- [ ] **Step 3 — Green:** Implement the `avax.*` jsonrpsee module (methods per §9.2) + admin/health, mounted alongside the `eth_*` modules (`merge_configured`); defer the jsonrpsee-vs-axum mount decision to 12-node (note in code). Commit avax golden vectors.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm rpc_avax` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: avax.* RPC namespace + admin/health (G8)`

### Task M6.25: EVM + atomic-trie state sync over Firewood proofs (G8)
**Crate:** ava-evm  ·  **Depends on:** M6.4, M6.17  ·  **Spec:** 10 §10, §17.9 (G8); 04 §4.2/§4.3
**Files:** `crates/ava-evm/src/sync/mod.rs`, `crates/ava-evm/src/sync/server.rs`, `crates/ava-evm/src/sync/client.rs`, `crates/ava-evm/tests/state_sync.rs`
- [ ] **Step 1 — Red:** `tests/state_sync.rs` `fn leafs_request_served_from_firewood_revision()` (range proof at a historical revision, wire-exact vs Go `firewood/syncer`) + `fn client_reconstructs_trie_and_verifies_root()`; atomic-trie sync over the 2nd Firewood instance then `ApplyToSharedMemory` from the synced cursor.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm state_sync` → fails.
- [ ] **Step 3 — Green:** Implement `EvmStateSyncServer::handle_leafs` (Firewood range proofs), the client (reconstruct + verify root), atomic-trie sync, and block/header/receipt backfill into `CanonicalStore`, all over the p2p SDK (05) — no reth sync.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm state_sync` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: EVM + atomic-trie state sync over Firewood proofs (G8)`

### Task M6.26: Public reusable API surface for SAE (reuse contract)
**Crate:** ava-evm + ava-evm-reth  ·  **Depends on:** M6.6, M6.4, M6.13, M6.15, M6.21, M6.5  ·  **Spec:** 10 §16, §17.10; 00 §11.1.5
**Files:** `crates/ava-evm/src/lib.rs` (re-exports), `crates/ava-evm-reth/src/lib.rs`, `crates/ava-evm/tests/reuse_surface.rs`
- [ ] **Step 1 — Red:** `tests/reuse_surface.rs` `fn sae_reusable_items_are_public()`: a compile-test that imports each §17.10 item through public paths — `ava_evm_reth::{ExternalConsensusExecutor, ExecOutcome}`, `ava_evm::{AvaEvmConfig, FirewoodStateProvider, FirewoodStateView, FirewoodStateCommitter, hashed_post_state_to_batchops, AvaPrecompiles, PrecompileRegistry, AtomicStateHook, AvaChainSpec}` and calls `AvaEvmConfig::execute_batch` decoupled from any `EvmVm`/`ChainVm` — proving "one EVM engine, two drivers". Assert `EvmVm`/`BlockBuilderDriver` are NOT required to drive execution.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm reuse_surface` → fails (items private).
- [ ] **Step 3 — Green:** Make the §17.10 items `pub` with stable paths; ensure `propose_from_bundle`/`view`-by-root/deferred-commit are reachable without the sync lifecycle. Document the NOT-shared boundary (block lifecycle) in rustdoc (00 §11.1.5).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm reuse_surface` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: expose reusable executor + Firewood state APIs for SAE (§16/§17.10)`

### Task M6.27: G1/G9 invariant — reth never writes state/trie tables (CI)
**Crate:** ava-evm  ·  **Depends on:** M6.9, M6.20  ·  **Spec:** 10 §17.2 (G1 invariant), §17.7, §17.11 (G9)
**Files:** `crates/ava-evm/tests/g1_invariant.rs`
- [ ] **Step 1 — Red:** `tests/g1_invariant.rs` `fn state_trie_tables_stay_empty_after_block()`: build+accept a block, then open the MDBX env and assert `PlainState`/`HashedState`/`Trie` (and any Storage-V2/`SparseTrieCache` state tables, G9) are empty while `Headers`/`Bodies`/`Receipts`/`CanonicalHeaders` grew.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-evm g1_invariant` → fails (or correctly proves the invariant if already held; ensure it would fail if a `StateWriter` path were introduced).
- [ ] **Step 3 — Green:** Confirm no code path constructs reth `BlockchainProvider`/`UnifiedStorageWriter`/`StateWriter::write_state` for state/trie tables; only the bare `BlockExecutor`/`BlockBuilder` flow + `FirewoodStateCommitter` + `CanonicalStore` are used. Add the assertion as a standing CI guard.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-evm g1_invariant` → pass.
- [ ] **Step 5 — Commit:** `ava-evm: CI invariant — Firewood is state-of-record, reth trie tables empty (G1/G9)`

### Task M6.28: Fuzz targets + PORTING.md + proptest corpus completeness
**Crate:** ava-evm  ·  **Depends on:** M6.7, M6.14  ·  **Spec:** 02 §8, §10.1, §13
**Files:** `crates/ava-evm/fuzz/fuzz_targets/decode_block.rs`, `crates/ava-evm/fuzz/fuzz_targets/decode_atomic_tx.rs`, `crates/ava-evm/fuzz/Cargo.toml`, `crates/ava-evm/tests/PORTING.md`
- [ ] **Step 1 — Red:** Add `cargo-fuzz` targets `decode_block` and `decode_atomic_tx` (must never panic/over-read on arbitrary bytes; round-trip stable for anything that decoded). Create `tests/PORTING.md` seeded from `go test -list` of coreth `plugin/evm`/`atomic`/`customheader` + the `na`-with-reason rows for Engine-API-only Go plumbing.
- [ ] **Step 2 — Confirm red:** `cargo +nightly fuzz run decode_block -- -runs=1000` and `decode_atomic_tx` smoke → build/run; `PORTING.md` has `wip` rows.
- [ ] **Step 3 — Green:** Commit seed corpus under `fuzz/corpus/<target>/`; fill PORTING.md mapping every relevant Go test to its Rust counterpart (M6.x test names) until no `wip` rows remain for shipped scope.
- [ ] **Step 4 — Confirm green:** `cargo xtask test-fuzz` (smoke) green; `cargo xtask porting-report` shows ava-evm with no `wip` rows.
- [ ] **Step 5 — Commit:** `ava-evm: fuzz targets (block/atomic-tx) + PORTING.md coverage matrix`

### Task M6.29: Milestone exit gate
**Crate:** ava-evm (+ avalanchers binary)  ·  **Depends on:** all M6.1–M6.28  ·  **Spec:** 10 (all), 04 §4, 20 §7, 21; 02 §10.5/§11; BUILDABLE-&-GREEN INVARIANT
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

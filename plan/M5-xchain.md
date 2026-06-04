# M5 — X-Chain Full Issue/Accept Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement the full X-Chain (`ava-avm`) as a post-linearization Snowman VM — complete tx model, fx dispatch (secp256k1fx + nftfx + propertyfx), UTXO state, syntactic/semantic verify + executor, linearized `StandardBlock`s, tx gossip, and X↔P atomic import/export — byte-/behavior-exact with avalanchego so a tx issued on the Rust node produces identical block IDs and UTXO sets as Go.
**Tier:** T4 — VMs
**Crates:** ava-avm (+ `ava_avm::nftfx`, `ava_avm::propertyfx` modules)
**Owning specs:** `09-avm-xchain.md` (primary), `07-vm-framework.md` (ATOMIC-1 UTXO byte contract, fx framework, `avax`/`verify`/`secp256k1fx` components, mempool, `SharedMemory`), `00-overview-and-conventions.md` (determinism §6.1, `#![forbid(unsafe_code)]`), `02-testing-strategy.md` (TDD, proptest, golden, differential)
**Depends on (prior milestones):** M3 (fx framework, `ava-secp256k1fx`, `avax` components, `verify`, chain manager, Snowman linearization boundary, generic mempool), M4 (P-Chain present for atomic X↔P import/export + `SharedMemory` shared-memory backend), M0 (codec, ids, crypto, database/versiondb/prefixdb infra)
**Exit gate (named tests):** `golden::xchain_block_hash` + `golden::xchain_tx_codec`; **`differential::xchain_issue_tx`** (10k proptest-generated X-Chain tx programs → identical block IDs + UTXO sets vs a Go node); `differential::atomic_xp` (X↔P import/export UTXO bytes decode cross-chain)

---

## Dependency map & parallel waves

The build order follows the data dependency from bytes outward: a tx cannot be verified before it can be encoded; it cannot be executed before it is verified; a block cannot be built before txs execute; atomic transfers cannot be tested before the executor emits/consumes atomic requests.

```
Wave 0  (foundation — codec & types, parallelizable internally)
  M5.1  Crate skeleton + error model + FxIndex/TypeToFxIndex scaffolding
  M5.2  Tx model types (BaseTx, CreateAssetTx, OperationTx, ImportTx, ExportTx, Tx envelope, InitialState, Operation)
  M5.3  nftfx types + codec
  M5.4  propertyfx types + codec
  M5.5  Codec & type-ID registry (the 21-entry table, standard + genesis registries) ──┐
                                                                                        │
Wave 1  (fx verification dispatch)         depends on 5.2–5.5                           │
  M5.6  secp256k1fx verify wiring into avm (verify_credentials reuse from M3)           │
  M5.7  nftfx verify_operation / verify_transfer-disallowed                             │
  M5.8  propertyfx verify_operation / verify_transfer-disallowed                        │
  M5.9  fx dispatch table (ParsedFx + TypeToFxIndex routing)                            │
                                                                                        ▼
Wave 2  (state)                            depends on 5.5            golden::xchain_tx_codec lands at end of Wave 0/1
  M5.10 UTXO state stores (utxo/tx/blockID/block/singleton) + Diff layer
  M5.11 initialize_chain_state (genesis Snowman block seed) + persistence byte-details

Wave 3  (verification + execution)         depends on 5.6–5.11
  M5.12 SyntacticVerifier (stateless, all 5 tx types)
  M5.13 SemanticVerifier (stateful, verify_fx_usage, grandfather quirk)
  M5.14 Executor (UTXO state transitions, EXEC-AVM-1 indexing, atomic requests)

Wave 4  (blocks + consensus boundary)      depends on 5.10–5.14
  M5.15 StandardBlock type + codec + block parser (golden::xchain_block_hash)
  M5.16 Block verify/accept/reject over Diff; Snowman Block trait impl
  M5.17 Mempool wiring + block Builder

Wave 5  (network)                          depends on 5.16–5.17
  M5.18 Tx gossip (push/pull over generic gossip) + Atomic app-handler switch

Wave 6  (VM assembly + atomic)             depends on 5.14, 5.16, 5.18, M4 shared memory
  M5.19 VM assembly: ChainVm impl, initialize, parse/get/build/last_accepted
  M5.20 X↔P atomic import/export end-to-end (ATOMIC-1) + differential::atomic_xp
  M5.21 JSON-RPC service (avm.* methods) — minimum needed for issueTx/getTx/getUTXOs

Wave 7  (differential + gate)
  M5.22 Differential program generator (seeded single BaseTx → scale to 10k) — differential::xchain_issue_tx
  M5.23 cargo-fuzz target for block/tx/op decoder
  M5.24 Milestone exit gate
```

Parallelism: M5.2/5.3/5.4 in parallel after 5.1. M5.6/5.7/5.8 in parallel after 5.5. M5.10/5.11 parallel with the fx work. M5.12/5.13/5.14 are sequential (each feeds the next). Wave 5/6/7 are largely sequential.

---

## Tasks

### Task M5.1: Crate skeleton, error model, FxIndex scaffolding
**Crate:** ava-avm  ·  **Depends on:** M3 (ava-vm, ava-secp256k1fx, ava-codec, ava-types), M0  ·  **Spec:** 09 §0, §2.2, §11 (Error model); 00 §7.1
**Files:** `crates/ava-avm/Cargo.toml`, `crates/ava-avm/src/lib.rs`, `crates/ava-avm/src/error.rs`, `crates/ava-avm/src/fx_index.rs`
- [ ] **Step 1 — Red:** Add `crates/ava-avm/tests/error_variants.rs` with `#[test] fn error_variants_exist_and_match_go_sentinels()` asserting `matches!(Error::AssetIdMismatch, Error::AssetIdMismatch)` for every Go sentinel named in 09 §11 (`AssetIdMismatch`, `NotAnAsset`, `IncompatibleFx`, `UnknownFx`, `WrongNumberOfCredentials`, `DoubleSpend`, `NoImportInputs`, `NoExportOutputs`, name/symbol/denomination errors) and a `#[test] fn fx_index_repr()` asserting `FxIndex::Secp256k1 as u32 == 0 && FxIndex::Nft as u32 == 1 && FxIndex::Property as u32 == 2`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-avm --test error_variants` → fails to compile (crate/types absent).
- [ ] **Step 3 — Green:** Create `Cargo.toml` (deps: `ava-vm`, `ava-secp256k1fx`, `ava-codec`, `ava-types`, `ava-database`, `ava-network`, `ava-api`, `thiserror`, `bytes`; dev: `proptest`, `rstest`, `assert_matches`, `hex`, `insta`, `pretty_assertions`). `lib.rs`: license header, `#![forbid(unsafe_code)]`, module decls + `pub mod nftfx; pub mod propertyfx;`. `error.rs`: `#[derive(thiserror::Error)] pub enum Error` with all sentinels (re-export fx errors from `ava_secp256k1fx`). `fx_index.rs`: `#[repr(u32)] pub enum FxIndex { Secp256k1 = 0, Nft = 1, Property = 2 }` per 09 §2.2.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-avm --test error_variants` passes; `cargo build -p ava-avm`.
- [ ] **Step 5 — Commit:** `avm: crate skeleton, error model, FxIndex (M5.1)`

### Task M5.2: Tx model types (BaseTx, CreateAssetTx, OperationTx, Import/ExportTx, Tx envelope, InitialState, Operation)
**Crate:** ava-avm  ·  **Depends on:** M5.1; M3 (`avax::{BaseTx, TransferableInput, TransferableOutput, Asset, UtxoId}`, `verify::State`)  ·  **Spec:** 09 §3 (all subsections), TX-AVM-1 field-order invariant; 07 §3.1 `avax` types
**Files:** `crates/ava-avm/src/txs/mod.rs`, `txs/base_tx.rs`, `txs/create_asset.rs`, `txs/operation_tx.rs`, `txs/import.rs`, `txs/export.rs`, `txs/tx.rs`, `txs/initial_state.rs`, `txs/operation.rs`, `txs/credential.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/tx_types.rs`: `#[test] fn unsigned_tx_enum_variants()` constructs each `UnsignedTx::{Base, CreateAsset, Operation, Import, Export}` with minimal fields and asserts field accessors return the embedded `BaseTx`; `#[test] fn fx_credential_fx_id_not_serialized()` asserts `FxCredential` exposes `fx_id` but the struct's `#[codec(skip)]`/serialize-false marker is present (compile-level check via a const assertion on serialized field count).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-avm --test tx_types` → fails to compile.
- [ ] **Step 3 — Green:** Define `pub enum UnsignedTx { Base(BaseTx), CreateAsset(CreateAssetTx), Operation(OperationTx), Import(ImportTx), Export(ExportTx) }` and the structs exactly per 09 §3.2–3.4 with field order = serialization order (TX-AVM-1): `BaseTx{network_id,blockchain_id,outs,ins,memo}`, `CreateAssetTx{base,name,symbol,denomination,states}`, `OperationTx{base,ops}`, `ImportTx{base,source_chain,imported_ins}`, `ExportTx{base,destination_chain,exported_outs}`. `Tx{unsigned, creds, tx_id(derived), bytes(derived)}`; `FxCredential{fx_id(serialize:false), credential}`; `InitialState{fx_index, fx_id(serialize:false), outs}`; `Operation{asset, utxo_ids, fx_id(serialize:false), op}`. Embedded `BaseTx` serializes inline (no extra prefix). Derive `ava_codec` Serialize/Deserialize with the `serialize:false` annotations on `fx_id`/`tx_id`/`bytes`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-avm --test tx_types`; `cargo build -p ava-avm`.
- [ ] **Step 5 — Commit:** `avm: tx model types — BaseTx/CreateAsset/Operation/Import/Export/Tx (M5.2)`

### Task M5.3: nftfx types + codec
**Crate:** ava-avm (`ava_avm::nftfx`)  ·  **Depends on:** M5.1; M3 (`ava_secp256k1fx::{Input, OutputOwners, Credential}`)  ·  **Spec:** 09 §4.2 (field order = serialization order)
**Files:** `crates/ava-avm/src/nftfx/mod.rs`, `nftfx/types.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/nftfx_types.rs`: round-trip a `nftfx::TransferOutput{group_id, payload, owners}` through `ava_codec` (without registry type-id prefix; raw struct) and `assert_eq!`; assert `MintOperation::outs()` synthesizes one `TransferOutput` per owner sharing `group_id`/`payload`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-avm --test nftfx_types` → fails to compile.
- [ ] **Step 3 — Green:** Define per 09 §4.2: `MintOutput{group_id:u32, owners}`, `TransferOutput{group_id:u32, payload:Vec<u8> (<=1KiB), owners}`, `MintOperation{mint_input:secp::Input, group_id:u32, payload, outputs:Vec<OutputOwners>}` with `outs()`, `TransferOperation{input:secp::Input, output:TransferOutput}` with `outs()=[output]`, `Credential(secp::Credential)` newtype. `payload` length cap (1 KiB) enforced in `verify()`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-avm --test nftfx_types`.
- [ ] **Step 5 — Commit:** `avm: nftfx types + codec (M5.3)`

### Task M5.4: propertyfx types + codec
**Crate:** ava-avm (`ava_avm::propertyfx`)  ·  **Depends on:** M5.1; M3 (`ava_secp256k1fx`)  ·  **Spec:** 09 §4.3
**Files:** `crates/ava-avm/src/propertyfx/mod.rs`, `propertyfx/types.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/propertyfx_types.rs`: round-trip `MintOutput{owners}` and `OwnedOutput{owners}` (structurally identical) and assert `MintOperation::outs() == [mint_output, owned_output]` and `BurnOperation::outs() == []`.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-avm --test propertyfx_types` → fails to compile.
- [ ] **Step 3 — Green:** Define per 09 §4.3: `MintOutput{owners}`, `OwnedOutput{owners}`, `MintOperation{mint_input:secp::Input, mint_output:MintOutput, owned_output:OwnedOutput}` with `outs()`, `BurnOperation{input:secp::Input}` with `outs()=[]`, `Credential(secp::Credential)`.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-avm --test propertyfx_types`.
- [ ] **Step 5 — Commit:** `avm: propertyfx types + codec (M5.4)`

### Task M5.5: Codec & type-ID registry (21-entry table, standard + genesis) — `golden::xchain_tx_codec`
**Crate:** ava-avm  ·  **Depends on:** M5.2, M5.3, M5.4; M3 (`ava_codec::{TypeId, CodecRegistry}`)  ·  **Spec:** 09 §2.1 (the table), §2.2, CODEC-AVM-1; 02 §6 (golden)
**Files:** `crates/ava-avm/src/codec.rs`, `crates/ava-avm/tests/golden_tx_codec.rs`, `tests/vectors/avm/typeids.json`, `tests/vectors/avm/tx_codec/*.json`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/golden_tx_codec.rs` with `mod golden { #[test] fn xchain_tx_codec() {...} }`: (a) for all 21 entries assert `registry_standard.type_id::<T>() == N` and `registry_genesis.type_id::<T>() == N` (table from 09 §2.1, ids 0..20); (b) load committed Go hex from `tests/vectors/avm/tx_codec/base_tx.json` etc., assert `hex::encode(codec.marshal(0, &tx)) == vector.expected` and `tx_id == sha256(signed_bytes)`. Seed with a single hand-constructed `BaseTx` vector first; expand to all tx types + each fx out/op/cred after green.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm golden::xchain_tx_codec` → fails (registry absent / vector mismatch). Assert on the type-id mismatch message, not a compile error, after a stub registry exists.
- [ ] **Step 3 — Green:** `codec.rs`: build two `CodecRegistry` (standard max `DefaultMaxSize`, genesis max `MaxInt32`) registering **in this exact order** (09 §2.1): tx types 0–4, then secp256k1fx 5–9, nftfx 10–14, propertyfx 15–19, `StandardBlock` 20 (block registered in M5.15 — reserve id 20 / register a placeholder then wire the real type there). Build `TypeToFxIndex: HashMap<TypeId, FxIndex>` in the **same pass** (09 §2.2). Codec version = 0 everywhere. Implement `Tx::initialize` byte derivation per 09 §3.1 (signed_bytes, unsigned_len via `codec.size`, `tx_id = sha256(signed_bytes)`).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm golden::xchain_tx_codec` passes (all 21 type-ids + tx vectors). Add `proptest-regressions/` if proptest round-trip is colocated.
- [ ] **Step 5 — Commit:** `avm: codec registry + type-ID table + tx-codec golden vectors (M5.5)`

### Task M5.6: secp256k1fx verification wiring into avm
**Crate:** ava-avm  ·  **Depends on:** M5.5; M3 (`ava_secp256k1fx::Fx::verify_credentials`, `verify_transfer`, recover-cache)  ·  **Spec:** 09 §4, §4.1; 07 §4.3
**Files:** `crates/ava-avm/src/fx/mod.rs`, `fx/secp.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/fx_secp.rs`: proptest `verify_transfer_accepts_iff_threshold_valid_sigs` — generate `OutputOwners` (locktime/threshold/addrs) + signer sets + sig-index permutations; assert accept iff exactly `threshold` valid sorted-unique sigs and locktime matured (reuse M3 `verify_credentials`); assert `utxo.amt != in.amt` → `Error::MismatchedAmounts`. Include `verify_disabled_when_not_bootstrapped` (returns `Ok` even with garbage sigs).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm fx_secp` → fails (wiring absent).
- [ ] **Step 3 — Green:** Wrap `ava_secp256k1fx::Fx` so the avm verifier can call `verify_transfer(tx,in,cred,utxo)` (checks `utxo.amt==in.amt` then `verify_credentials`) and `verify_operation` (one `MintOutput` UTXO, owners equality, then `verify_credentials` over the mint input). `bootstrapped` gate matches Go (`!bootstrapped ⇒ Ok(())`). Share one recover-cache (`dashmap`/`lru` cap 256) per 09 §4.1 / 13.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm fx_secp`; commit `proptest-regressions/fx_secp.txt`.
- [ ] **Step 5 — Commit:** `avm: secp256k1fx verify wiring + proptests (M5.6)`

### Task M5.7: nftfx verify_operation (transfer-disallowed)
**Crate:** ava-avm  ·  **Depends on:** M5.6, M5.3  ·  **Spec:** 09 §4.2, FX-AVM-1
**Files:** `crates/ava-avm/src/nftfx/fx.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/nftfx_verify.rs`: `verify_transfer_disallowed` asserts `Error::CantTransfer`; `mint_group_id_mismatch` → `Error::WrongUniqueId`; `transfer_payload_mismatch` → `Error::WrongBytes`; happy path mint + transfer accept; wrong UTXO type → `Error::WrongUtxoType`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm nftfx_verify` → fails.
- [ ] **Step 3 — Green:** Implement `nftfx::Fx::verify_operation(tx, op, cred, utxos)` per 09 §4.2 match: `Mint` requires one `MintOutput` (`try_into()?` else `WrongUtxoType`), `verify_all(&[op,cred,out])`, `group_id` equality, delegate to secp `verify_credentials`; `Transfer` requires `TransferOutput`, `group_id`+`payload` equality, delegate. `verify_transfer` returns `Err(CantTransfer)`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm nftfx_verify`.
- [ ] **Step 5 — Commit:** `avm: nftfx verify_operation (M5.7)`

### Task M5.8: propertyfx verify_operation (transfer-disallowed)
**Crate:** ava-avm  ·  **Depends on:** M5.6, M5.4  ·  **Spec:** 09 §4.3, FX-AVM-1
**Files:** `crates/ava-avm/src/propertyfx/fx.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/propertyfx_verify.rs`: `verify_transfer_disallowed`; `mint_owners_mismatch` → `Error::WrongMintOutput`; mint happy path; burn happy path; wrong UTXO type → `Error::WrongUtxoType`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm propertyfx_verify` → fails.
- [ ] **Step 3 — Green:** Implement `propertyfx::Fx::verify_operation` per 09 §4.3: `Mint` requires `MintOutput`, `out.owners == op.mint_output.owners` else `WrongMintOutput`, delegate; `Burn` requires `OwnedOutput`, delegate via secp `verify_credentials` over `op.input`. `verify_transfer` disallowed.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm propertyfx_verify`.
- [ ] **Step 5 — Commit:** `avm: propertyfx verify_operation (M5.8)`

### Task M5.9: fx dispatch table (ParsedFx + TypeToFxIndex routing)
**Crate:** ava-avm  ·  **Depends on:** M5.5, M5.6, M5.7, M5.8  ·  **Spec:** 09 §2.2, §4, FX-AVM-1
**Files:** `crates/ava-avm/src/fx/dispatch.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/fx_dispatch.rs`: given the `TypeToFxIndex` map and a value of each concrete output/credential/operation type, assert `resolve_fx_index(value)` returns the right `FxIndex` (secp out → 0, nft out → 1, property out → 2; credentials id 9/14/19 → 0/1/2); unknown type → `Error::UnknownFx`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm fx_dispatch` → fails.
- [ ] **Step 3 — Green:** `dispatch.rs`: `struct ParsedFx { id: Id, fx: Box<dyn Fx> }`; `fxs: Vec<ParsedFx>` indexed by `FxIndex`. `resolve_fx_index(value: &dyn Any) -> Result<FxIndex>` looks up the value's `TypeId` in `TypeToFxIndex` (built in M5.5's single pass). Routing helpers `route_transfer`/`route_operation`/`route_output` call the resolved fx (09 §4, FX-AVM-1).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm fx_dispatch`.
- [ ] **Step 5 — Commit:** `avm: fx dispatch table + TypeToFxIndex routing (M5.9)`

### Task M5.10: UTXO state stores + Diff layer
**Crate:** ava-avm  ·  **Depends on:** M5.5; M3 (`ava_database::{VersionDb, PrefixDb}`, `avax::UTXOState`), M0  ·  **Spec:** 09 §5, §5.1, §5.2; 00 §6.1 (BTreeMap on flush)
**Files:** `crates/ava-avm/src/state/mod.rs`, `state/state.rs`, `state/diff.rs`, `state/versions.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/state_utxo.rs`: open `State` over an in-memory base DB; add a UTXO, `commit`, reopen, `get_utxo` returns identical bytes; `add_tx` then `get_tx` parses via genesis codec; `Diff` over parent: `delete_utxo` then `apply` removes it; `abort` discards; proptest `diff_flush_is_sorted` asserts flush key order is sorted (BTreeMap), independent of insertion order.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm state_utxo` → fails.
- [ ] **Step 3 — Green:** `state.rs`: `versiondb` over base with five `prefixdb` sub-stores `"utxo"/"tx"/"blockID"/"block"/"singleton"` (09 §5). Traits `ReadOnlyChain`, `Chain`, `State` per 09 §5. UTXO `input_id = sha256(tx_id ++ be_u64(output_index))` (09 §5.1) via M3 `UtxoId::input_id`. `diff.rs`: `Diff` tracks modified UTXOs (`Some`=add / `None`=delete), added txs/blocks, pending ts/lastAccepted; `apply`/`commit`/`abort`; flush uses `BTreeMap` (00 §6.1). txs read with **genesis codec** (09 §5.3).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm state_utxo`; commit regressions.
- [ ] **Step 5 — Commit:** `avm: UTXO state stores + Diff layer (M5.10)`

### Task M5.11: initialize_chain_state (genesis Snowman block seed) + persistence byte-details
**Crate:** ava-avm  ·  **Depends on:** M5.10  ·  **Spec:** 09 §1 (stop-vertex parent, height 0), §5.3; 07 (genesis block)
**Files:** `crates/ava-avm/src/state/init.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/state_init.rs`: `initialize_chain_state(stop_vertex_id, genesis_ts)` on a fresh state seeds a genesis `StandardBlock{parent=stop_vertex_id, height=0, time=genesis_ts, txs=[]}`, sets `lastAccepted` to its id and `initialized=true`; idempotent on second call (no re-seed). Assert height key encoding = 8-byte big-endian (`database::PackUInt64`), timestamp = Unix-seconds codec-packed.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm state_init` → fails (StandardBlock not yet defined — gate this task to run its block-construction via a minimal local stub, then re-point at the real `StandardBlock` from M5.15; note the ordering in PORTING.md).
- [ ] **Step 3 — Green:** Implement `initialize_chain_state` per 09 §5 / §5.3: if no stored `lastAccepted`, build + persist the genesis block, set singleton flags; height key big-endian (04 `PackUInt64`), `PutTimestamp` Unix-seconds. `is_initialized`/`set_initialized` over `"singleton"` prefix (`0x00 initialized | 0x01 timestamp | 0x02 lastAccepted`).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm state_init`.
- [ ] **Step 5 — Commit:** `avm: initialize_chain_state genesis seed + persistence byte-details (M5.11)`

### Task M5.12: SyntacticVerifier (stateless, all 5 tx types)
**Crate:** ava-avm  ·  **Depends on:** M5.9, M5.5; M3 (`avax::verify_tx` conservation+fee, sort helpers)  ·  **Spec:** 09 §6.1, §3.3 (InitialState rules), TX-AVM-1; 07 §3.1 FlowChecker
**Files:** `crates/ava-avm/src/txs/executor/backend.rs`, `txs/executor/syntactic.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/syntactic.rs` table-driven over tx types: `base_tx_ok`; `memo_too_long` (>256) → error; `unsorted_outs` → error; `num_creds != num_inputs` → `Error::WrongNumberOfCredentials`; `create_asset_name_bad` (empty/>128/leading-ws/non-ascii) and `symbol_bad` (>4/lowercase) and `denomination_gt_32`; `states_empty`/`states_unsorted`; `operation_tx_empty_ops`; `op_utxo_collides_base_in` → `Error::DoubleSpend`; `import_no_inputs` → `Error::NoImportInputs`; `export_no_outs` → `Error::NoExportOutputs`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm syntactic` → fails.
- [ ] **Step 3 — Green:** `Backend{ctx, config, codec, fee_asset_id, fxs, type_to_fx_index, bootstrapped}`. `SyntacticVerifier` over `UnsignedTx` per 09 §6.1: verify `avax::BaseTx` (network id, memo ≤256, ins/outs sorted+typed), `avax::verify_tx(fee, fee_asset, ins, outs, codec)`, verify every credential, `num_creds == num_inputs` (inputs include op-count / imported-ins). Type-specific: CreateAsset name 1..=128 ASCII letter/digit/space no edge-ws, symbol 1..=4 ASCII upper, denom ≤32, states non-empty sorted-unique by `fx_index`, each `InitialState::verify(codec, num_fxs)` (09 §3.3); Operation ops non-empty sorted-unique-by-bytes, utxo_ids ∩ base ins = ∅; Import `imported_ins` non-empty (fee over `ins ++ imported_ins`); Export `exported_outs` non-empty (fee over `outs ++ exported_outs`). Use `CreateAssetTxFee` for CreateAsset.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm syntactic`.
- [ ] **Step 5 — Commit:** `avm: SyntacticVerifier all tx types (M5.12)`

### Task M5.13: SemanticVerifier (stateful) + verify_fx_usage + grandfather quirk
**Crate:** ava-avm  ·  **Depends on:** M5.12, M5.10; M4 (`SharedMemory` for ImportTx)  ·  **Spec:** 09 §6.2 (incl. GRANDFATHERED_OPERATION_TX, SameSubnet); 07 §3.1 (SharedMemory)
**Files:** `crates/ava-avm/src/txs/executor/semantic.rs`, `txs/executor/consts.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/semantic.rs`: `base_tx_spends_known_utxo`; `asset_id_mismatch` → `Error::AssetIdMismatch`; `incompatible_fx` (asset doesn't enable fx) → `Error::IncompatibleFx`; `not_an_asset` → `Error::NotAnAsset`; `operation_tx_cred_index` (cred index = `len(ins)+op_index`); `grandfathered_op_skips_verification` asserts the const tx-id `"MkvpJS13eCnEYeYi9B5zuWrU9goG9RBj7nr83U7BjrFV22a12"` bypasses op verification exactly as Go; `import_fetches_shared_memory` (uses a fake `SharedMemory` returning UTXO bytes); `not_bootstrapped_skips_op_verify`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm semantic` → fails.
- [ ] **Step 3 — Green:** `consts.rs`: `pub const GRANDFATHERED_OPERATION_TX: &str = "MkvpJS13eCnEYeYi9B5zuWrU9goG9RBj7nr83U7BjrFV22a12";`. `SemanticVerifier` per 09 §6.2: BaseTx fetch each input UTXO (`asset==in.asset` else `AssetIdMismatch`), resolve fx by credential type, `verify_fx_usage(fx_index, asset_id)` (load asset's CreateAssetTx; `NotAnAsset`/`IncompatibleFx`), `fx.verify_transfer`. CreateAsset = BaseTx. Operation: BaseTx then per op (skip when `!bootstrapped` **or** `tx.id == GRANDFATHERED_OPERATION_TX`) fetch input UTXOs, `verify_fx_usage`, `fx.verify_operation`, cred index `len(ins)+op_index`. Import: BaseTx, `verify.SameSubnet(source_chain)` (if bootstrapped), `SharedMemory.get(source_chain, ids)` → unmarshal `avax::UTXO` → verify_transfer, cred index `len(ins)+i`. Export: BaseTx, `SameSubnet(destination_chain)`, `verify_fx_usage` per exported out.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm semantic`.
- [ ] **Step 5 — Commit:** `avm: SemanticVerifier + verify_fx_usage + grandfather quirk (M5.13)`

### Task M5.14: Executor (UTXO state transitions, EXEC-AVM-1, atomic requests)
**Crate:** ava-avm  ·  **Depends on:** M5.13; M3 (`avax::{consume, produce}`); M4 (`Requests`/`Element`)  ·  **Spec:** 09 §6.3, EXEC-AVM-1, §9 (atomic format), ATOMIC-1
**Files:** `crates/ava-avm/src/txs/executor/executor.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/executor.rs` table-driven: `base_tx_consume_produce` asserts deleted inputs + produced UTXO ids at `output_index = i`; `create_asset_indexing` asserts asset-id == tx-id and `output_index` continues from `len(outs)` across multiple `InitialState`s in order (EXEC-AVM-1); `operation_tx_outs_indexing`; `import_builds_remove_requests` asserts `AtomicRequests{source_chain:{remove:[input_id..]}}`; `export_builds_put_requests` asserts `Element{key=utxo_input_id, value=marshal(utxo), traits=addresses}` keyed by `destination_chain`, `output_index` continuing from `len(outs)`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm executor` → fails.
- [ ] **Step 3 — Green:** `Executor` over `UnsignedTx` per 09 §6.3: BaseTx `avax::consume(state,&ins)` + `avax::produce(state, tx_id, &outs)` (index = i). CreateAsset: BaseTx then per-`InitialState` out `add_utxo` with `tx_id=self_tx_id`, `asset.id=self_tx_id`, `output_index` continuing from `len(outs)` monotonically (EXEC-AVM-1). Operation: BaseTx then per op delete input UTXOs + add `op.outs()` (asset=op.asset), index continuing. Import: BaseTx + record imported `input_id`s + build `Requests{remove}`. Export: BaseTx + per exported out build UTXO at continuing index, `marshal(utxo)` with the **avm codec v0** (ATOMIC-1), emit `Element{key=input_id, value, traits=out.addresses()}` in `Requests{put}` keyed by destination chain.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm executor`.
- [ ] **Step 5 — Commit:** `avm: Executor UTXO transitions + atomic requests (M5.14)`

### Task M5.15: StandardBlock type + codec + parser — `golden::xchain_block_hash`
**Crate:** ava-avm  ·  **Depends on:** M5.5, M5.2  ·  **Spec:** 09 §7 (field order, type id 20); 02 §6 (golden block hashes); M4/ava-genesis (stop-vertex constant)
**Files:** `crates/ava-avm/src/block/mod.rs`, `block/standard_block.rs`, `block/parser.rs`, `crates/ava-avm/tests/golden_block_hash.rs`, `tests/vectors/avm/block/*.json`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/golden_block_hash.rs` with `mod golden { #[test] fn xchain_block_hash() {...} }`: parse a committed Go-produced `StandardBlock` hex from `tests/vectors/avm/block/standard_block.json`, assert `block_id == sha256(bytes)` matches the committed id; assert the **Mainnet & Fuji X-Chain genesis block id** and the **stop-vertex parent** match the `ava-genesis` constants (09 §1, §5.3); round-trip `marshal(0, &(block as &dyn Block)) == bytes`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm golden::xchain_block_hash` → fails.
- [ ] **Step 3 — Green:** `StandardBlock{parent_id, height, time, merkle_root(zero/unused), txs}` per 09 §7 field order; register as type id 20 in M5.5's registry (replace the placeholder). Serialize as the `Block` interface (typeid-prefix 20): `cm.marshal(0, &(blk as &dyn Block))`; `block_id = sha256(bytes)`. `parser.rs`: `parse(bytes)` → recompute id + `initialize` every contained tx (re-derive each `tx_id`). `timestamp() = Unix(time)`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm golden::xchain_block_hash`.
- [ ] **Step 5 — Commit:** `avm: StandardBlock + parser + block-hash golden vectors (M5.15)`

### Task M5.16: Block verify/accept/reject over Diff; Snowman Block trait
**Crate:** ava-avm  ·  **Depends on:** M5.14, M5.15, M5.10; M4 (`SharedMemory.apply`), M3 (`ava_vm::block::Block`)  ·  **Spec:** 09 §7 (accept = commit diff + apply atomic requests), §6; 07 §2.3 Block trait
**Files:** `crates/ava-avm/src/block/executor.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/block_lifecycle.rs`: build a `StandardBlock` with one BaseTx over a seeded UTXO set; `verify()` (syntactic+semantic over a `Diff` on parent) succeeds; `accept()` commits the diff, advances `lastAccepted`+timestamp, marks txs accepted, and (for an ExportTx block) applies the atomic `put` via a fake `SharedMemory` **in the same batch** as the state commit; `reject()` discards. Conflicting-tx block → verify error.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm block_lifecycle` → fails.
- [ ] **Step 3 — Green:** Impl `ava_vm::block::Block` for the avm block wrapper: `verify` runs Syntactic+Semantic+Executor over a `Diff` on the parent; `accept` calls `state.commit_batch()` and `SharedMemory.apply(requests, &[batch])` atomically (09 §7, §9), sets txs accepted, advances `lastAccepted`/`timestamp`; `reject` aborts the diff. `parent()/height()/bytes()/id()/timestamp()` from `StandardBlock`. Wrap in `enum Block { Standard(StandardBlock) }` (09 §11) for future extensibility.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm block_lifecycle`.
- [ ] **Step 5 — Commit:** `avm: block verify/accept/reject + atomic commit (M5.16)`

### Task M5.17: Mempool wiring + block Builder
**Crate:** ava-avm  ·  **Depends on:** M5.16; M3 (`ava_vm::mempool::Mempool`)  ·  **Spec:** 09 §7.1; 07 §7 (generic mempool); 00 §6.1 (pop order = total order identical to Go)
**Files:** `crates/ava-avm/src/mempool.rs`, `crates/ava-avm/src/block/builder.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/builder.rs`: add N verified txs to the mempool; `build_block` drains in mempool order, re-verifies each against a running `Diff`, drops + records failures, and produces `StandardBlock{parent=last_accepted, height=parent.height+1, time=max(parent.time, now), txs}`; proptest `mempool_pop_order_total` asserts pop order is a stable total order independent of internal map layout (00 §6.1); byte-cap enforced (packs until `maxMempoolSize`).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm builder` → fails.
- [ ] **Step 3 — Green:** `mempool.rs`: `MempoolTx for Tx` (`id=tx_id`, `size=bytes.len()`, `inputs`), `indexmap`-backed via M3 generic `Mempool` + dropped-reason LRU (09 §7.1). `builder.rs`: drain `peek`/`remove` in order, re-verify on `Diff`, pack to size cap, build the block with clamped-monotonic `time` (09 §7.1).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm builder`; commit regressions.
- [ ] **Step 5 — Commit:** `avm: mempool + block builder (M5.17)`

### Task M5.18: Tx gossip + Atomic app-handler switch
**Crate:** ava-avm  ·  **Depends on:** M5.17; M3/M-network (`ava_network::p2p::gossip` push/pull, Bloom Set)  ·  **Spec:** 09 §8; 05 (gossip machinery)
**Files:** `crates/ava-avm/src/network/mod.rs`, `network/gossip.rs`, `network/atomic.rs`, `network/tx_verifier.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/gossip.rs`: `Tx` is `Gossipable` with `gossip_id == tx_id`; an inbound `AppGossip` of a valid tx adds it to the mempool and the Bloom set; an invalid tx is dropped with reason; `Atomic` switch (`ArcSwap<dyn AppHandler>`) forwards to the live handler.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm gossip` → fails.
- [ ] **Step 3 — Green:** `gossip.rs`: implement `Gossipable for Tx` (gossip_id = tx_id), supply marshaller + verify hook (`tx_verifier.rs` wraps semantic verify) into 05's push/pull gossip + Bloom `Set` (09 §8). `atomic.rs`: `ArcSwap<dyn AppHandler>` initialized once to the real handler (post-linearization), indirection preserved for the gRPC path (09 §8).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm gossip`.
- [ ] **Step 5 — Commit:** `avm: tx gossip + atomic app-handler switch (M5.18)`

### Task M5.19: VM assembly — ChainVm impl
**Crate:** ava-avm  ·  **Depends on:** M5.16, M5.17, M5.18, M5.11; M3 (`ava_vm::{Vm, ChainVm, block::Block}`), M4 (chain manager wiring)  ·  **Spec:** 09 §0, §5 (initialize_chain_state hook); 07 §2.1, §2.4
**Files:** `crates/ava-avm/src/vm.rs`, `crates/ava-avm/src/factory.rs`, `crates/ava-avm/src/config.rs`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/vm_conformance.rs`: run the generic `vm_conformance!(make_avm_vm)` battery (07 §10): `initialize` → genesis `last_accepted`; `build_block`→`verify`→`accept` advances last-accepted + height index; `parse_block` round-trips bytes; `get_block` of accepted/processing; `Err(NotFound)` for unknown id/height; `set_preference`; `set_state` phase transitions; `shutdown` idempotent.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm vm_conformance` → fails.
- [ ] **Step 3 — Green:** `vm.rs`: `struct Vm` holding state, fxs, mempool, builder, gossip, codec registries. Impl `ava_vm::Vm::initialize` (build registries + fxs in registration order, open state, `initialize_chain_state(stop_vertex_id, genesis_ts)`, wire `AppSender`/gossip) and `ChainVm` (`build_block`/`get_block`/`parse_block`/`set_preference`/`last_accepted`/`get_block_id_at_height`) per 07 §2.4. `factory.rs`: `Factory` returning the VM. `config.rs`: avm config (fees, gossip params).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm vm_conformance`.
- [ ] **Step 5 — Commit:** `avm: VM assembly + ChainVm impl + conformance (M5.19)`

### Task M5.20: X↔P atomic import/export end-to-end (ATOMIC-1) — `differential::atomic_xp`
**Crate:** ava-avm (+ test harness X)  ·  **Depends on:** M5.14, M5.16, M5.19; M4 (P-Chain + shared `SharedMemory`)  ·  **Spec:** 09 §9 (ATOMIC-1, byte format), 07 §3.1 (SharedMemory, canonical UTXO encoding); 00 §11.1.7
**Files:** `crates/ava-avm/tests/atomic_xp.rs`, `tests/vectors/atomic/*.json`, `tests/differential/src/atomic.rs` (harness X)
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/atomic_xp.rs` with `mod differential { #[test] fn atomic_xp() {...} }` (recorded-oracle mode default): X-Chain `ExportTx` to P emits an `Element{key=input_id, value=marshal_v0(avax::UTXO), traits=addrs}`; assert (a) `hex::encode(value)` matches the committed `tests/vectors/atomic/x_to_p_utxo.json` Go vector, and (b) the **P-Chain codec** (M4) decodes the same bytes into an identical `avax::UTXO` (cross-chain decode — ATOMIC-1), and the reverse P→X. Live two-binary mode gated behind `--features differential-live`/`DIFFERENTIAL_LIVE` env with the recorded-oracle as fallback.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm differential::atomic_xp` → fails (vectors/cross-decode missing).
- [ ] **Step 3 — Green:** Ensure the avm export marshals `avax::UTXO` with **codec v0 + the exporting VM's secp256k1fx output type IDs** (ATOMIC-1, 09 §9). Add cross-chain decode helper in the harness importing M4's P-Chain codec; commit `tests/vectors/atomic/{x_to_p_utxo,p_to_x_utxo}.json` (Go-extracted, with provenance per 02 §6.2). Wire the live-mode path in `tests/differential/src/atomic.rs` (issue export on Go X-Chain, import on Go P-Chain; mirror on Rust).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm differential::atomic_xp` (recorded mode); live mode runs under the gated feature.
- [ ] **Step 5 — Commit:** `avm: X↔P atomic import/export (ATOMIC-1) + atomic_xp differential (M5.20)`

### Task M5.21: JSON-RPC service (avm.* methods)
**Crate:** ava-avm  ·  **Depends on:** M5.19; M3 (`ava-api` JSON-RPC router)  ·  **Spec:** 09 §10; 12 (JSON-RPC serving), 14 (API reference)
**Files:** `crates/ava-avm/src/service.rs`, `crates/ava-avm/tests/service.rs`, `tests/vectors/avm/service/*.json`
- [ ] **Step 1 — Red:** `crates/ava-avm/tests/service.rs`: golden request/response JSON (vs Go `service_test.go` fixtures) for `avm.issueTx` (parse + add to mempool + gossip), `avm.getTx`, `avm.getTxStatus`, `avm.getUTXOs` (incl. cross-chain `sourceChain`), `avm.getBalance`, `avm.getHeight`, `avm.getBlockByHeight`. Assert error codes match Go.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm service` → fails.
- [ ] **Step 3 — Green:** `service.rs`: implement the `avm.*` methods per 09 §10 over `ava-api`'s JSON-RPC router, names/args/replies/error-codes mirroring `vms/avm/service.go`. Bech32 `X-` addresses with chain HRP; CB58/hex asset ids. Defer the deprecated keystore-backed `wallet.*` methods behind a feature flag (note in PORTING.md / §10).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm service`.
- [ ] **Step 5 — Commit:** `avm: JSON-RPC service (avm.* methods) + golden fixtures (M5.21)`

### Task M5.22: Differential program generator — `differential::xchain_issue_tx`
**Crate:** ava-avm (+ `tests/differential/` harness X)  ·  **Depends on:** M5.19, M5.20, M5.21; cross-cutting harness X  ·  **Spec:** 02 §11 (differential harness, proptest program), 09 §12; 00 §6.1
**Files:** `tests/differential/src/xchain.rs`, `tests/differential/tests/xchain_issue_tx.rs`, `tests/differential/proptest-regressions/xchain_issue_tx.txt`
- [ ] **Step 1 — Red (TDD ENTRY POINT — start tiny):** `tests/differential/tests/xchain_issue_tx.rs` with `mod differential { #[test] fn xchain_issue_tx() {...} }`. Begin with `cases = 1` and a generator that emits a **single BaseTx program** (one issue-tx + `AwaitFinalization`); assert identical last-accepted block ID + height **and identical UTXO set** vs the Go oracle (recorded mode). This first failing test is the milestone's entry point.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential differential::xchain_issue_tx` → fails (generator/oracle absent).
- [ ] **Step 3 — Green:** `xchain.rs`: a proptest `Strategy` producing `(seed, Vec<Action>)` where `Action::IssueTx(TxSpec)` deterministically builds X-Chain txs (BaseTx first; then CreateAsset, Operation with each fx, Import, Export) from the seed (02 §11.2); tx/key bytes derived from the seed so both nodes get identical bytes. `Observation` collects per-chain last-accepted block id+height + the full UTXO set (sorted, 00 §6.1). Compare via `prop_assert_eq!`. Grow generator coverage, then scale `cases` 1 → 100 → 1k → **10k**. Live two-binary mode gated behind feature/env with recorded-oracle fallback (coordinate with harness X); print `DIFFERENTIAL_SEED=<n>` on mismatch (02 §11.5).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential differential::xchain_issue_tx` (recorded mode, `cases = 10000`); commit `proptest-regressions/xchain_issue_tx.txt`.
- [ ] **Step 5 — Commit:** `avm: differential xchain_issue_tx generator scaled to 10k (M5.22)`

### Task M5.23: cargo-fuzz target for block/tx/op decoder
**Crate:** ava-avm  ·  **Depends on:** M5.15, M5.5  ·  **Spec:** 02 §8 (fuzzing, block parsers), 02 §13.5
**Files:** `crates/ava-avm/fuzz/Cargo.toml`, `crates/ava-avm/fuzz/fuzz_targets/decode_block.rs`, `fuzz/corpus/decode_block/` (committed seeds)
- [ ] **Step 1 — Red:** Add `fuzz/fuzz_targets/decode_block.rs`: `fuzz_target!(|data: &[u8]| { if let Ok(b) = parse_block(data) { let re = marshal(&b); let back = parse_block(&re).unwrap(); assert_eq!(b.bytes(), back.bytes()); } })`; seed corpus with the M5.15 golden block bytes + a Tx + an Operation.
- [ ] **Step 2 — Confirm red:** `cargo fuzz run decode_block -- -runs=0` → fails to build until the target compiles against the parser.
- [ ] **Step 3 — Green:** Implement the fuzz crate (`libfuzzer-sys` + `arbitrary`), targeting the block/tx/operation decoder; must never panic / over-read on arbitrary bytes (02 §8). Wire `cargo xtask test-fuzz` smoke (short run).
- [ ] **Step 4 — Confirm green:** `cargo xtask test-fuzz` (smoke) runs the target briefly with no crash; commit corpus seeds.
- [ ] **Step 5 — Commit:** `avm: cargo-fuzz block/tx/op decoder target (M5.23)`

### Task M5.24: Milestone exit gate
**Crate:** ava-avm (+ workspace)  ·  **Depends on:** all prior M5 tasks  ·  **Spec:** 09 (full), 02 §13 (per-crate contract), 00 (buildable-&-green invariant)
**Files:** `crates/ava-avm/tests/PORTING.md`, workspace `Cargo.toml` (member registration), `.config/nextest.toml` (ci profile)
- [ ] **Step 1 — Red:** Run the full gate; expect any remaining gaps (missing PORTING rows, clippy warnings, unregistered workspace member) to fail.
- [ ] **Step 2 — Confirm red:** `cargo clippy --workspace -- -D warnings` and/or the named exit tests surface remaining issues.
- [ ] **Step 3 — Green:** Run and pass all of:
  - `cargo build --workspace`
  - `cargo build -p avalanchers` (the binary now runs the X-Chain)
  - `cargo nextest run --profile ci` including the named exit tests: `golden::xchain_block_hash`, `golden::xchain_tx_codec`, `differential::xchain_issue_tx`, `differential::atomic_xp`
  - `cargo clippy --workspace -- -D warnings`
  Confirm every per-crate contract artifact (02 §13): proptest suite + committed `crates/ava-avm/proptest-regressions/`, golden vectors under `tests/vectors/avm/` + `tests/vectors/atomic/`, the cargo-fuzz target, and `crates/ava-avm/tests/PORTING.md` (every Go `vms/avm`, `vms/nftfx`, `vms/propertyfx` test mapped to a Rust counterpart or `na` with reason). Confirm `avalanchers` boots an X-Chain end-to-end. Coordinate `differential::xchain_issue_tx` live mode behind feature/env with recorded-oracle fallback (cross-cutting harness X).
- [ ] **Step 4 — Confirm green:** all four commands above pass; PORTING.md has no `wip` rows for ported surfaces.
- [ ] **Step 5 — Commit:** `avm: M5 milestone exit gate — X-Chain full issue/accept green (M5.24)`

---

## Spec coverage check

| Spec section (09 + 07 ATOMIC-1) | Task(s) | Notes |
|---|---|---|
| 09 §1 Vertex→Snowman linearization (stop-vertex parent, height 0) | M5.11, M5.15 | post-linearization only; stop-vertex constant from `ava-genesis` |
| 09 §2.1 Codec type-ID table (21 entries) / CODEC-AVM-1 | M5.5 | golden `xchain_tx_codec` asserts all 21 ids on both registries |
| 09 §2.2 Registry representation, FxIndex, TypeToFxIndex | M5.1, M5.5, M5.9 | built in one pass with the registry |
| 09 §3.1 Tx envelope + byte/ID derivation | M5.2, M5.5 | `tx_id = sha256(signed_bytes)`; unsigned slice |
| 09 §3.2 UnsignedTx enum + BaseTx + 5 tx types / TX-AVM-1 | M5.2, M5.12, M5.14 | field-order invariant |
| 09 §3.3 InitialState & asset-definition model | M5.2, M5.12, M5.14 | asset-id == CreateAssetTx id |
| 09 §3.4 Operation + FxOperation | M5.2, M5.7, M5.8 | sorted-unique-by-bytes |
| 09 §4 / §4.1 secp256k1fx verify + bootstrapped gate + recover-cache | M5.6 | reuses M3 `verify_credentials` |
| 09 §4.2 nftfx verify (transfer-disallowed) | M5.3, M5.7 | |
| 09 §4.3 propertyfx verify (transfer-disallowed) | M5.4, M5.8 | |
| 09 §4 FX-AVM-1 routing | M5.9 | concrete-type → fx index |
| 09 §5 / §5.1 State stores + UTXO model | M5.10 | five prefixdb sub-stores |
| 09 §5.2 Diff layer | M5.10 | BTreeMap flush (00 §6.1) |
| 09 §5.3 Persistence byte-details (big-endian height, genesis codec, genesis block) | M5.11 | |
| 09 §6.1 SyntacticVerify (all tx types) | M5.12 | |
| 09 §6.2 SemanticVerify + verify_fx_usage + grandfather quirk + SameSubnet | M5.13 | `GRANDFATHERED_OPERATION_TX` const |
| 09 §6.3 Executor / EXEC-AVM-1 / atomic requests | M5.14 | |
| 09 §7 StandardBlock + parser | M5.15 | golden `xchain_block_hash` |
| 09 §7 Block verify/accept/reject (Snowman Block trait) | M5.16 | atomic commit in one batch |
| 09 §7.1 Mempool + Builder | M5.17 | pop order = total order (00 §6.1) |
| 09 §8 Gossip + Atomic app-handler | M5.18 | reuses 05 gossip |
| 09 §9 Cross-chain atomic (ATOMIC-1) + 07 §3.1 SharedMemory / 00 §11.1.7 | M5.20 | `differential::atomic_xp` |
| 09 §10 JSON-RPC service (avm.*) | M5.21 | wallet.* keystore deferred (feature flag) |
| 09 §11 Go→Rust mapping + Error model | M5.1 | thiserror variants |
| 09 §12 Test plan (golden, proptest, differential, fuzz) | M5.5, M5.6, M5.15, M5.20, M5.22, M5.23 | |
| 09 §13 Performance notes (parallel sig verify, zero-copy, shared recover-cache, cached sorted-bytes) | M5.6, M5.14, M5.17 | refactor-phase; gated by differential |
| 07 ATOMIC-1 (canonical avax.UTXO + secp256k1fx output encodings, cross-VM decode) | M5.5, M5.14, M5.20 | pinned by `atomic_xp` vectors |
| 07 §7 generic mempool reuse | M5.17 | |
| 07 §10 VM conformance battery | M5.19 | |

**Deferrals (recorded in PORTING.md):**
- `wallet.*` server-side spend-building + deprecated keystore (09 §10) — behind a feature flag; the `utxo::Spender` is implemented only as far as `avm.getUTXOs`/balance need it. Defer full `wallet_service.go`/`wallet_client.go` parity to a follow-up.
- Vertex/DAG-era artifacts (09 §1 deprecated list: vertex parsing/issuance, `InputUTXOs()`, snowstorm) — **not ported** by design; only the stop-vertex constant survives in `ava-genesis`.
- `avm.getBlock`/`getAssetDescription`/`getAllBalances`/`getTxFee` JSON goldens beyond the issue/accept path may land incrementally in M5.21 follow-ups if Go fixtures are large; the issue/accept-critical subset is gated by M5.21.
- Live two-binary `differential::xchain_issue_tx` requires a Go node — gated behind `--features differential-live`/`DIFFERENTIAL_LIVE`; CI runs the recorded-oracle fallback per-PR, live mode nightly (02 §11.7), coordinated with cross-cutting harness X.

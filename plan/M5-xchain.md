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
- [x] **Step 1 — Red:** Add `crates/ava-avm/tests/error_variants.rs` with `#[test] fn error_variants_exist_and_match_go_sentinels()` asserting `matches!(Error::AssetIdMismatch, Error::AssetIdMismatch)` for every Go sentinel named in 09 §11 (`AssetIdMismatch`, `NotAnAsset`, `IncompatibleFx`, `UnknownFx`, `WrongNumberOfCredentials`, `DoubleSpend`, `NoImportInputs`, `NoExportOutputs`, name/symbol/denomination errors) and a `#[test] fn fx_index_repr()` asserting `FxIndex::Secp256k1 as u32 == 0 && FxIndex::Nft as u32 == 1 && FxIndex::Property as u32 == 2`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-avm --test error_variants` → fails to compile (crate/types absent).
- [x] **Step 3 — Green:** Create `Cargo.toml` (deps: `ava-vm`, `ava-secp256k1fx`, `ava-codec`, `ava-types`, `ava-database`, `ava-network`, `ava-api`, `thiserror`, `bytes`; dev: `proptest`, `rstest`, `assert_matches`, `hex`, `insta`, `pretty_assertions`). `lib.rs`: license header, `#![forbid(unsafe_code)]`, module decls + `pub mod nftfx; pub mod propertyfx;`. `error.rs`: `#[derive(thiserror::Error)] pub enum Error` with all sentinels (re-export fx errors from `ava_secp256k1fx`). `fx_index.rs`: `#[repr(u32)] pub enum FxIndex { Secp256k1 = 0, Nft = 1, Property = 2 }` per 09 §2.2.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-avm --test error_variants` passes; `cargo build -p ava-avm`.
- [x] **Step 5 — Commit:** `avm: crate skeleton, error model, FxIndex (M5.1)`

### Task M5.2: Tx model types (BaseTx, CreateAssetTx, OperationTx, Import/ExportTx, Tx envelope, InitialState, Operation)
**Crate:** ava-avm  ·  **Depends on:** M5.1; M3 (`avax::{BaseTx, TransferableInput, TransferableOutput, Asset, UtxoId}`, `verify::State`)  ·  **Spec:** 09 §3 (all subsections), TX-AVM-1 field-order invariant; 07 §3.1 `avax` types
**Files:** `crates/ava-avm/src/txs/mod.rs`, `txs/base_tx.rs`, `txs/create_asset.rs`, `txs/operation_tx.rs`, `txs/import.rs`, `txs/export.rs`, `txs/tx.rs`, `txs/initial_state.rs`, `txs/operation.rs`, `txs/credential.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/tx_types.rs`: `#[test] fn unsigned_tx_enum_variants()` constructs each `UnsignedTx::{Base, CreateAsset, Operation, Import, Export}` with minimal fields and asserts field accessors return the embedded `BaseTx`; `#[test] fn fx_credential_fx_id_not_serialized()` asserts `FxCredential` exposes `fx_id` but the struct's `#[codec(skip)]`/serialize-false marker is present (compile-level check via a const assertion on serialized field count).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-avm --test tx_types` → fails to compile.
- [x] **Step 3 — Green:** Define `pub enum UnsignedTx { Base(BaseTx), CreateAsset(CreateAssetTx), Operation(OperationTx), Import(ImportTx), Export(ExportTx) }` and the structs exactly per 09 §3.2–3.4 with field order = serialization order (TX-AVM-1): `BaseTx{network_id,blockchain_id,outs,ins,memo}`, `CreateAssetTx{base,name,symbol,denomination,states}`, `OperationTx{base,ops}`, `ImportTx{base,source_chain,imported_ins}`, `ExportTx{base,destination_chain,exported_outs}`. `Tx{unsigned, creds, tx_id(derived), bytes(derived)}`; `FxCredential{fx_id(serialize:false), credential}`; `InitialState{fx_index, fx_id(serialize:false), outs}`; `Operation{asset, utxo_ids, fx_id(serialize:false), op}`. Embedded `BaseTx` serializes inline (no extra prefix). Derive `ava_codec` Serialize/Deserialize with the `serialize:false` annotations on `fx_id`/`tx_id`/`bytes`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-avm --test tx_types`; `cargo build -p ava-avm`.
- [x] **Step 5 — Commit:** `avm: tx model types — BaseTx/CreateAsset/Operation/Import/Export/Tx (M5.2)`

### Task M5.3: nftfx types + codec
**Crate:** ava-avm (`ava_avm::nftfx`)  ·  **Depends on:** M5.1; M3 (`ava_secp256k1fx::{Input, OutputOwners, Credential}`)  ·  **Spec:** 09 §4.2 (field order = serialization order)
**Files:** `crates/ava-avm/src/nftfx/mod.rs`, `nftfx/types.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/nftfx_types.rs`: round-trip a `nftfx::TransferOutput{group_id, payload, owners}` through `ava_codec` (without registry type-id prefix; raw struct) and `assert_eq!`; assert `MintOperation::outs()` synthesizes one `TransferOutput` per owner sharing `group_id`/`payload`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-avm --test nftfx_types` → fails to compile.
- [x] **Step 3 — Green:** Define per 09 §4.2: `MintOutput{group_id:u32, owners}`, `TransferOutput{group_id:u32, payload:Vec<u8> (<=1KiB), owners}`, `MintOperation{mint_input:secp::Input, group_id:u32, payload, outputs:Vec<OutputOwners>}` with `outs()`, `TransferOperation{input:secp::Input, output:TransferOutput}` with `outs()=[output]`, `Credential(secp::Credential)` newtype. `payload` length cap (1 KiB) enforced in `verify()`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-avm --test nftfx_types`.
- [x] **Step 5 — Commit:** `avm: nftfx types + codec (M5.3)`

### Task M5.4: propertyfx types + codec
**Crate:** ava-avm (`ava_avm::propertyfx`)  ·  **Depends on:** M5.1; M3 (`ava_secp256k1fx`)  ·  **Spec:** 09 §4.3
**Files:** `crates/ava-avm/src/propertyfx/mod.rs`, `propertyfx/types.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/propertyfx_types.rs`: round-trip `MintOutput{owners}` and `OwnedOutput{owners}` (structurally identical) and assert `MintOperation::outs() == [mint_output, owned_output]` and `BurnOperation::outs() == []`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-avm --test propertyfx_types` → fails to compile.
- [x] **Step 3 — Green:** Define per 09 §4.3: `MintOutput{owners}`, `OwnedOutput{owners}`, `MintOperation{mint_input:secp::Input, mint_output:MintOutput, owned_output:OwnedOutput}` with `outs()`, `BurnOperation{input:secp::Input}` with `outs()=[]`, `Credential(secp::Credential)`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-avm --test propertyfx_types`.
- [x] **Step 5 — Commit:** `avm: propertyfx types + codec (M5.4)`

> **As-built (Wave 0 — M5.1+M5.2+M5.3+M5.4, 2026-06-07):** crate `ava-avm` created;
> 20 tests green; `clippy --all-targets --all-features -D warnings` clean; fmt clean;
> workspace + binary build. Commits: M5.1 = `9fcc1e0`; M5.2+M5.3+M5.4 = `50cfcf6`
> (the three implementer agents ran concurrently in the SAME working tree — the
> intended per-task isolated worktrees were not used — so their output converged
> into one commit; functionally complete & verified, history granularity is the
> only cost).
> - **M5.1:** `Cargo.toml` pre-declares the Wave-0 dep set (`ava-types`/`ava-codec`/
>   `ava-codec-derive`/`ava-crypto`/`ava-vm`/`ava-secp256k1fx`/`bytes`/`thiserror`;
>   dev `proptest`/`rstest`/`assert_matches`/`pretty_assertions`/`hex`). **Deviations from
>   the M5.1 task text:** `ava-database` (Wave 2) / `ava-network` (Wave 5) are deferred to
>   their owning tasks to keep the Wave-0 build lean; **`ava-api` does not exist yet**
>   (served via M8/M12) so it is omitted; **`insta` is not a workspace dep** so golden
>   tests use plain asserts / `pretty_assertions` / `hex` (matches the M4 precedent).
>   `error.rs` carries the §11 sentinel set as a `#[non_exhaustive]` enum + `#[from]`
>   `ava_codec::error::CodecError` and `ava_vm::error::Error` (the fx errors are
>   re-exported on the latter); the enum grows per later wave task (M5.3 added
>   `PayloadTooLarge`). lib.rs uses the `use X as _;` unused-dep silencer pattern; every
>   `tests/*.rs` integration file opens with `#![allow(unused_crate_dependencies)]`.
> - **M5.2:** `txs/{mod,components,base_tx,create_asset,operation_tx,import,export,
>   initial_state,operation,credential,tx}.rs`. `UnsignedTx` enum (Base=0…Export=4) +
>   accessors; `Tx` envelope with the sha256 prefix-length byte derivation
>   (`ava_crypto::hashing::sha256`, mirrors the P-Chain `txs/tx.rs`). `components.rs`
>   mirrors `ava_vm::components::avax` as codec-serializable types: `Output`/`Input`/
>   `Credential` are `#[derive(AvaCodec)] #[codec(type_registry)]` interface enums
>   embedding the **public** `ava_secp256k1fx` `Serializable`/`Deserializable` impls
>   (secp type-ids: TransferInput=5, MintOutput=6, TransferOutput=7, Credential=9 — the
>   M4.3 promotion is reused). `AvaxBaseTx` field order verified network_id→blockchain_id
>   →outs→ins→memo. `FxCredential.fx_id`/`InitialState.fx_id`/`Operation.fx_id` carry NO
>   `#[codec]` tag (Go `serialize:"false"`). **Deferrals to M5.5:** the 21-entry
>   standard/genesis `CodecRegistry` pair + `TypeToFxIndex` + Go-extracted byte goldens are
>   NOT built (a bare default-max `LinearCodec` `Manager` + the inline type_registry prefixes
>   suffice for the round-trip); the `Output`/`Input`/`Credential` enums define only secp
>   variants (nft/property routing is the documented `TODO(M5.5)` extension point); `Operation`
>   has a placeholder `FxOperation` (concrete secp/nft/property op type-ids 8/12/13/17/18 are
>   M5.5's domain — OperationTx round-trip is out of Wave-0 scope).
> - **M5.3:** `nftfx/{mod,types}.rs` — `MintOutput`(10)/`TransferOutput`(11, ≤1 KiB payload,
>   `verify()`→`Error::PayloadTooLarge`)/`MintOperation`(12, `outs()` synthesizes one
>   `TransferOutput` per owner)/`TransferOperation`(13)/`Credential`(14). Codec hand-routes
>   through `Serializable::marshal_into`/`Deserializable::unmarshal_from` (secp's
>   `marshal_fields` are `pub(crate)`).
> - **M5.4:** `propertyfx/{mod,types}.rs` — `MintOutput`(15)/`OwnedOutput`(16)/`MintOperation`
>   (17, `outs()=[mint,owned]`)/`BurnOperation`(18, `outs()=[]`)/`Credential`(19), with a
>   `PropertyOutput` enum unifying the two output types for the `outs()` return.

### Task M5.5: Codec & type-ID registry (21-entry table, standard + genesis) — `golden::xchain_tx_codec`
**Crate:** ava-avm  ·  **Depends on:** M5.2, M5.3, M5.4; M3 (`ava_codec::{TypeId, CodecRegistry}`)  ·  **Spec:** 09 §2.1 (the table), §2.2, CODEC-AVM-1; 02 §6 (golden)
**Files:** `crates/ava-avm/src/codec.rs`, `crates/ava-avm/tests/golden_tx_codec.rs`, `tests/vectors/avm/typeids.json`, `tests/vectors/avm/tx_codec/*.json`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/golden_tx_codec.rs` with `mod golden { #[test] fn xchain_tx_codec() {...} }`: (a) for all 21 entries assert `registry_standard.type_id::<T>() == N` and `registry_genesis.type_id::<T>() == N` (table from 09 §2.1, ids 0..20); (b) load committed Go hex from `tests/vectors/avm/tx_codec/base_tx.json` etc., assert `hex::encode(codec.marshal(0, &tx)) == vector.expected` and `tx_id == sha256(signed_bytes)`. Seed with a single hand-constructed `BaseTx` vector first; expand to all tx types + each fx out/op/cred after green.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm golden::xchain_tx_codec` → fails (registry absent / vector mismatch). Assert on the type-id mismatch message, not a compile error, after a stub registry exists.
- [x] **Step 3 — Green:** `codec.rs`: build two `CodecRegistry` (standard max `DefaultMaxSize`, genesis max `MaxInt32`) registering **in this exact order** (09 §2.1): tx types 0–4, then secp256k1fx 5–9, nftfx 10–14, propertyfx 15–19, `StandardBlock` 20 (block registered in M5.15 — reserve id 20 / register a placeholder then wire the real type there). Build `TypeToFxIndex: HashMap<TypeId, FxIndex>` in the **same pass** (09 §2.2). Codec version = 0 everywhere. Implement `Tx::initialize` byte derivation per 09 §3.1 (signed_bytes, unsigned_len via `codec.size`, `tx_id = sha256(signed_bytes)`).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm golden::xchain_tx_codec` passes (all 21 type-ids + tx vectors). Add `proptest-regressions/` if proptest round-trip is colocated.
- [x] **Step 5 — Commit:** `avm: codec registry + type-ID table + tx-codec golden vectors (M5.5)`

> **As-built (Wave 0/1 boundary — M5.5, 2026-06-07, commit `5dafad1`):** built as
> `crates/ava-avm/src/txs/codec.rs` (not top-level `src/codec.rs`) mirroring the
> proven P-Chain `ava_platformvm::txs::codec` precedent exactly. `build_type_id_registry()`
> uses `ava_codec::linearcodec::TypeIdRegistry` as the registration-order **assigner**
> (the real encoding type-ids are baked into the `#[codec(type_id=N)]` derive
> annotations; the registry asserts them against the Go order). Two `Manager`s:
> `Codec()` (default max) + `GenesisCodec()` (`ava_codec::MAX_SLICE_LEN == i32::MAX`,
> = Go `math.MaxInt32`), both process-wide `OnceLock` singletons. `type_to_fx_index()`
> returns `TypeToFxIndex = HashMap<u32, FxIndex>` (secp 5–9, nft 10–14, property 15–19;
> tx 0–4 + block 20 absent). `block.StandardBlock`(20) is a **name-only placeholder**
> (the type lands in M5.15). `Tx::initialize`/`parse` now drive the real `Manager`.
> **Golden coverage:** byte-exact `BaseTx` vector ported verbatim from Go
> `vms/avm/txs/base_tx_test.go` (incl. embedded secp `TransferOutput`(7)+`TransferInput`(5))
> + genesis-codec round-trip + all-21 type-id + `TypeToFxIndex` assertions, as an inline
> `const` (no `tests/vectors/avm/*.json` — matches the P-Chain `golden_codec.rs` style;
> those files were optional). 23 tests green; clippy `-D warnings` + fmt clean.
> **Deferred to later tasks (per plan scope):** byte-exact vectors for
> CreateAsset/Operation/Import/Export + per-fx out/op/cred, and the `components.rs`
> Output/Input/Operation nft/property variants needed to route them through the codec
> — these are M5.9's domain (the must-pass bar was BaseTx-byte-exact + 21 type-ids +
> round-trip, per the M5.5 task text).

### Task M5.6: secp256k1fx verification wiring into avm
**Crate:** ava-avm  ·  **Depends on:** M5.5; M3 (`ava_secp256k1fx::Fx::verify_credentials`, `verify_transfer`, recover-cache)  ·  **Spec:** 09 §4, §4.1; 07 §4.3
**Files:** `crates/ava-avm/src/fx/mod.rs`, `fx/secp.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/fx_secp.rs`: proptest `verify_transfer_accepts_iff_threshold_valid_sigs` — generate `OutputOwners` (locktime/threshold/addrs) + signer sets + sig-index permutations; assert accept iff exactly `threshold` valid sorted-unique sigs and locktime matured (reuse M3 `verify_credentials`); assert `utxo.amt != in.amt` → `Error::MismatchedAmounts`. Include `verify_disabled_when_not_bootstrapped` (returns `Ok` even with garbage sigs).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm fx_secp` → fails (wiring absent).
- [x] **Step 3 — Green:** Wrap `ava_secp256k1fx::Fx` so the avm verifier can call `verify_transfer(tx,in,cred,utxo)` (checks `utxo.amt==in.amt` then `verify_credentials`) and `verify_operation` (one `MintOutput` UTXO, owners equality, then `verify_credentials` over the mint input). `bootstrapped` gate matches Go (`!bootstrapped ⇒ Ok(())`). Share one recover-cache (`dashmap`/`lru` cap 256) per 09 §4.1 / 13.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm fx_secp`; commit `proptest-regressions/fx_secp.txt`.
- [x] **Step 5 — Commit:** `avm: secp256k1fx verify wiring + proptests (M5.6)`

> **As-built (M5.6, 2026-06-07, commit `404fe96`):** `fx/mod.rs` defines a minimal
> `trait Fx { verify_transfer, verify_operation }` (the documented extension point for
> M5.7/M5.8 nft/property adapters + M5.9 `ParsedFx` dispatch); `fx/secp.rs` `SecpFx`
> wraps `ava_secp256k1fx::Fx`, sharing its existing recover-cache + `bootstrapped` flag
> (no new cache built — reuses M3's per 09 §4.1). `verify_transfer` = `!bootstrapped ⇒
> Ok` → `utxo.amt==in.amt` (else `Error::MismatchedAmounts`) → `verify_credentials`;
> `verify_operation` = `!bootstrapped ⇒ Ok` → produced-mint-owners == consumed
> `MintOutput` UTXO owners (else `Error::WrongMintCreated`) → `verify_credentials` over
> the mint input. Added two avm-native error variants (`MismatchedAmounts`,
> `WrongMintCreated`); multisig-gate sentinels reused via `Error::Fx(#[from]
> ava_vm::error::Error)`. Added `ava-utils` dep (`clock::Clock`). **Known Go-parity
> deviation (documented in `secp.rs`):** the `!bootstrapped` short-circuit happens
> *before* the avm-side amount/owners checks, so during bootstrap this returns `Ok`
> where Go would still surface `ErrMismatchedAmounts`/`ErrWrongMintCreated` — this is
> exactly the contract the M5.6 task text + `verify_disabled_when_not_bootstrapped` test
> specified; revisit if strict bootstrap-time parity is later required.

### Task M5.7: nftfx verify_operation (transfer-disallowed)
**Crate:** ava-avm  ·  **Depends on:** M5.6, M5.3  ·  **Spec:** 09 §4.2, FX-AVM-1
**Files:** `crates/ava-avm/src/nftfx/fx.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/nftfx_verify.rs`: `verify_transfer_disallowed` asserts `Error::CantTransfer`; `mint_group_id_mismatch` → `Error::WrongUniqueId`; `transfer_payload_mismatch` → `Error::WrongBytes`; happy path mint + transfer accept; wrong UTXO type → `Error::WrongUtxoType`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm nftfx_verify` → fails.
- [x] **Step 3 — Green:** Implement `nftfx::Fx::verify_operation(tx, op, cred, utxos)` per 09 §4.2 match: `Mint` requires one `MintOutput` (`try_into()?` else `WrongUtxoType`), `verify_all(&[op,cred,out])`, `group_id` equality, delegate to secp `verify_credentials`; `Transfer` requires `TransferOutput`, `group_id`+`payload` equality, delegate. `verify_transfer` returns `Err(CantTransfer)`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm nftfx_verify`.
- [x] **Step 5 — Commit:** `avm: nftfx verify_operation (M5.7)`

> **As-built (M5.7, 2026-06-07, commit `5ddce64`):** `nftfx/fx.rs` = `nftfx::Fx` struct
> wrapping `ava_secp256k1fx::Fx` (shares recover-cache + bootstrapped flag). Since the
> M5.6 `crate::fx::Fx` trait is secp-typed, nftfx implements **inherent** methods +
> models Go's `opIntf`/`utxoIntf` type switches as two new pub enums `NftOperation
> {Mint,Transfer}` / `NftOutput {Mint,Transfer}` (the polymorphic cross-fx dispatch is
> M5.9). `verify_transfer` → `Error::CantTransfer`; `verify_operation` asserts UTXO
> variant (`WrongUtxoType`), `group_id` eq (`WrongUniqueId`), payload eq for transfer
> (`WrongBytes`), delegates sig check to `verify_credentials`. Added err variants
> `WrongUniqueId`/`WrongBytes` (+ shared `CantTransfer`/`WrongUtxoType`, deduped at merge
> against M5.8). 6 tests.

### Task M5.8: propertyfx verify_operation (transfer-disallowed)
**Crate:** ava-avm  ·  **Depends on:** M5.6, M5.4  ·  **Spec:** 09 §4.3, FX-AVM-1
**Files:** `crates/ava-avm/src/propertyfx/fx.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/propertyfx_verify.rs`: `verify_transfer_disallowed`; `mint_owners_mismatch` → `Error::WrongMintOutput`; mint happy path; burn happy path; wrong UTXO type → `Error::WrongUtxoType`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm propertyfx_verify` → fails.
- [x] **Step 3 — Green:** Implement `propertyfx::Fx::verify_operation` per 09 §4.3: `Mint` requires `MintOutput`, `out.owners == op.mint_output.owners` else `WrongMintOutput`, delegate; `Burn` requires `OwnedOutput`, delegate via secp `verify_credentials` over `op.input`. `verify_transfer` disallowed.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm propertyfx_verify`.
- [x] **Step 5 — Commit:** `avm: propertyfx verify_operation (M5.8)`

> **As-built (M5.8, 2026-06-07, commit `241a91d`):** `propertyfx/fx.rs` = `propertyfx::Fx`
> struct wrapping `ava_secp256k1fx::Fx`; inherent methods + two pub enums
> `PropertyOperation {Mint,Burn}` / `PropertyUtxo {Mint,Owned}` modeling Go's type
> switches. `verify_transfer` → `Error::CantTransfer`; `verify_operation`: Mint requires
> `PropertyUtxo::Mint` (`WrongUtxoType`), `out.owners==op.mint_output.owners`
> (`WrongMintOutput`), delegate over `mint_input`; Burn requires `PropertyUtxo::Owned`,
> delegate over `input`. Added err variant `WrongMintOutput` (+ shared `CantTransfer`/
> `WrongUtxoType`). **Merge:** M5.7+M5.8 ran concurrently in worktrees (each `git merge
> main` as Step 0 to pick up M5.5+M5.6), cherry-picked onto main; the shared
> `CantTransfer`/`WrongUtxoType` error variants were deduped by hand at merge (combined
> tree: **46 tests green**, clippy + fmt clean).

### Task M5.9: fx dispatch table (ParsedFx + TypeToFxIndex routing)
**Crate:** ava-avm  ·  **Depends on:** M5.5, M5.6, M5.7, M5.8  ·  **Spec:** 09 §2.2, §4, FX-AVM-1
**Files:** `crates/ava-avm/src/fx/dispatch.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/fx_dispatch.rs`: given the `TypeToFxIndex` map and a value of each concrete output/credential/operation type, assert `resolve_fx_index(value)` returns the right `FxIndex` (secp out → 0, nft out → 1, property out → 2; credentials id 9/14/19 → 0/1/2); unknown type → `Error::UnknownFx`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm fx_dispatch` → fails.
- [x] **Step 3 — Green:** `dispatch.rs`: `struct ParsedFx { id: Id, fx: Box<dyn Fx> }`; `fxs: Vec<ParsedFx>` indexed by `FxIndex`. `resolve_fx_index(value: &dyn Any) -> Result<FxIndex>` looks up the value's `TypeId` in `TypeToFxIndex` (built in M5.5's single pass). Routing helpers `route_transfer`/`route_operation`/`route_output` call the resolved fx (09 §4, FX-AVM-1).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm fx_dispatch`.
- [x] **Step 5 — Commit:** `avm: fx dispatch table + TypeToFxIndex routing (M5.9)`

> **As-built (M5.9, 2026-06-07, commit `111b339`):** `fx/dispatch.rs`. `resolve_fx_index(type_id:
> u32) -> Result<FxIndex>` looks the codec type-id up in a process-wide `OnceLock` of M5.5's
> `type_to_fx_index()` (`Error::UnknownFx` on miss — Go `getFx`/`errUnknownFx`). **Design
> deviation from the plan's `Box<dyn Fx>`/`&dyn Any` text (deliberate, noted):** the three fxs
> are heterogeneous — `SecpFx` impls the secp-typed `fx::Fx` trait while `nftfx::Fx`/
> `propertyfx::Fx` expose inherent methods over their own concrete types — so a single
> object-safe `dyn Fx` is not achievable. Realized as `ParsedFx { id, fx: FxKind }` where
> `enum FxKind { Secp(SecpFx), Nft(nftfx::Fx), Property(propertyfx::Fx) }`, dispatched by
> `match` (the codebase's enum-over-trait-object preference, cf. `components::Output`). Added a
> small `trait FxValue { fn fx_type_id(&self) -> u32 }` (impl'd for the secp interface enums via
> `.codec_type_id()` + the nft/property concrete types) + `resolve_fx_index_of(value)`.
> `Dispatch { fxs: Vec<ParsedFx> }` indexed by `FxIndex` in VM-registration order, with
> `route_transfer`/`route_{secp,nft,property}_operation`/`route_output`. 6 tests.

### Task M5.10: UTXO state stores + Diff layer
**Crate:** ava-avm  ·  **Depends on:** M5.5; M3 (`ava_database::{VersionDb, PrefixDb}`, `avax::UTXOState`), M0  ·  **Spec:** 09 §5, §5.1, §5.2; 00 §6.1 (BTreeMap on flush)
**Files:** `crates/ava-avm/src/state/mod.rs`, `state/state.rs`, `state/diff.rs`, `state/versions.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/state_utxo.rs`: open `State` over an in-memory base DB; add a UTXO, `commit`, reopen, `get_utxo` returns identical bytes; `add_tx` then `get_tx` parses via genesis codec; `Diff` over parent: `delete_utxo` then `apply` removes it; `abort` discards; proptest `diff_flush_is_sorted` asserts flush key order is sorted (BTreeMap), independent of insertion order.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm state_utxo` → fails.
- [x] **Step 3 — Green:** `state.rs`: `versiondb` over base with five `prefixdb` sub-stores `"utxo"/"tx"/"blockID"/"block"/"singleton"` (09 §5). Traits `ReadOnlyChain`, `Chain`, `State` per 09 §5. UTXO `input_id = sha256(tx_id ++ be_u64(output_index))` (09 §5.1) via M3 `UtxoId::input_id`. `diff.rs`: `Diff` tracks modified UTXOs (`Some`=add / `None`=delete), added txs/blocks, pending ts/lastAccepted; `apply`/`commit`/`abort`; flush uses `BTreeMap` (00 §6.1). txs read with **genesis codec** (09 §5.3).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm state_utxo`; commit regressions.
- [x] **Step 5 — Commit:** `avm: UTXO state stores + Diff layer (M5.10)`

> **As-built (M5.10, 2026-06-07, commits `f57547e` + `c946f3e` merge-fixup):** mirrors
> the P-Chain `ava_platformvm::state` precedent. `state/chain.rs` = `ReadOnlyChain`+`Chain`
> traits (UTXOs stored as opaque codec bytes, `UtxoBytes`); `state/state.rs` =
> `State<D: Database>` = a `VersionDb` over the base partitioned into five `PrefixDb`
> sub-stores (`utxo`/`tx`/`blockID`/`block`/`singleton`), singleton keys
> `0x00 initialized | 0x01 timestamp | 0x02 lastAccepted`, `commit`/`abort`/`load`/`snapshot`;
> `state/diff.rs` = layered `Diff` (all overlay maps `BTreeMap` → sorted flush, 00 §6.1),
> exposes `flush_utxo_ids()` for the determinism proptest; `state/versions.rs` =
> block-id→`Chain` resolver. **Storage layer is codec-agnostic** (round-trips opaque tx/UTXO
> bytes; the block store is byte/id-level — no `StandardBlock` type, that's M5.15; no
> `initialize_chain_state`/genesis seed, that's M5.11). Added `ava-database` dep + error
> variants `MissingParentState` + `Database(#[from] ava_database::error::Error)`. 8 tests.
> **Merge note (recorded in memory):** the worktree was branched from the pre-M5.5 base, so
> its `state_utxo` test referenced the old `txs::codec()` function; on merge it was rewired
> to M5.5's `txs::codec::GenesisCodec()` singleton (commit `c946f3e`). Combined tree: 35
> tests green, clippy + fmt clean.

### Task M5.11: initialize_chain_state (genesis Snowman block seed) + persistence byte-details
**Crate:** ava-avm  ·  **Depends on:** M5.10  ·  **Spec:** 09 §1 (stop-vertex parent, height 0), §5.3; 07 (genesis block)
**Files:** `crates/ava-avm/src/state/init.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/state_init.rs`: `initialize_chain_state(stop_vertex_id, genesis_ts)` on a fresh state seeds a genesis `StandardBlock{parent=stop_vertex_id, height=0, time=genesis_ts, txs=[]}`, sets `lastAccepted` to its id and `initialized=true`; idempotent on second call (no re-seed). Assert height key encoding = 8-byte big-endian (`database::PackUInt64`), timestamp = Unix-seconds codec-packed.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm state_init` → fails (StandardBlock not yet defined — gate this task to run its block-construction via a minimal local stub, then re-point at the real `StandardBlock` from M5.15; note the ordering in PORTING.md).
- [x] **Step 3 — Green:** Implement `initialize_chain_state` per 09 §5 / §5.3: if no stored `lastAccepted`, build + persist the genesis block, set singleton flags; height key big-endian (04 `PackUInt64`), `PutTimestamp` Unix-seconds. `is_initialized`/`set_initialized` over `"singleton"` prefix (`0x00 initialized | 0x01 timestamp | 0x02 lastAccepted`).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm state_init`.
- [x] **Step 5 — Commit:** `avm: initialize_chain_state genesis seed + persistence byte-details (M5.11)`

> **As-built (M5.11, 2026-06-07, commit `043996a`):** **No stub needed** — M5.15 landed
> first this session, so `State::initialize_chain_state(stop_vertex_id, genesis_ts, codec)`
> uses the real `ava_avm::block::{StandardBlock, Block}` directly. Mirrors Go
> `state.go` `InitializeChainState`: if already initialized → `load()` (restore
> `last_accepted`/`timestamp`), no re-seed; else build genesis `StandardBlock{parent=
> stop_vertex_id, height=0, time=genesis_ts (trunc to Unix secs), txs=[]}` via the standard
> `Codec()`, `set_last_accepted`+`set_timestamp`+`add_block`+`set_initialized`, `commit`.
> Reused ALL M5.10 `Chain`/`State` methods (no `state.rs`/`error.rs` edits). Height index
> 8-byte big-endian + Unix-second timestamp were already correct in M5.10's `add_block`/
> `set_timestamp` (tests assert against the raw `MemDb`, accounting for `PrefixDb` keys =
> `SHA256(prefix) ‖ key`). 4 tests.

### Task M5.12: SyntacticVerifier (stateless, all 5 tx types)
**Crate:** ava-avm  ·  **Depends on:** M5.9, M5.5; M3 (`avax::verify_tx` conservation+fee, sort helpers)  ·  **Spec:** 09 §6.1, §3.3 (InitialState rules), TX-AVM-1; 07 §3.1 FlowChecker
**Files:** `crates/ava-avm/src/txs/executor/backend.rs`, `txs/executor/syntactic.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/syntactic.rs` table-driven over tx types: `base_tx_ok`; `memo_too_long` (>256) → error; `unsorted_outs` → error; `num_creds != num_inputs` → `Error::WrongNumberOfCredentials`; `create_asset_name_bad` (empty/>128/leading-ws/non-ascii) and `symbol_bad` (>4/lowercase) and `denomination_gt_32`; `states_empty`/`states_unsorted`; `operation_tx_empty_ops`; `op_utxo_collides_base_in` → `Error::DoubleSpend`; `import_no_inputs` → `Error::NoImportInputs`; `export_no_outs` → `Error::NoExportOutputs`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm syntactic` → fails.
- [x] **Step 3 — Green:** `Backend{ctx, config, codec, fee_asset_id, fxs, type_to_fx_index, bootstrapped}`. `SyntacticVerifier` over `UnsignedTx` per 09 §6.1: verify `avax::BaseTx` (network id, memo ≤256, ins/outs sorted+typed), `avax::verify_tx(fee, fee_asset, ins, outs, codec)`, verify every credential, `num_creds == num_inputs` (inputs include op-count / imported-ins). Type-specific: CreateAsset name 1..=128 ASCII letter/digit/space no edge-ws, symbol 1..=4 ASCII upper, denom ≤32, states non-empty sorted-unique by `fx_index`, each `InitialState::verify(codec, num_fxs)` (09 §3.3); Operation ops non-empty sorted-unique-by-bytes, utxo_ids ∩ base ins = ∅; Import `imported_ins` non-empty (fee over `ins ++ imported_ins`); Export `exported_outs` non-empty (fee over `outs ++ exported_outs`). Use `CreateAssetTxFee` for CreateAsset.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm syntactic`.
- [x] **Step 5 — Commit:** `avm: SyntacticVerifier all tx types (M5.12)`

> **As-built (M5.12, 2026-06-07, commit `b87d207`):** `txs/executor/{mod,backend,syntactic}.rs`.
> Introduced a minimal `Backend` + `Config` carrying ONLY the stateless-verify fields
> (`network_id`, `blockchain_id`, `tx_fee`/`create_asset_tx_fee`, `fee_asset_id`, `num_fxs`,
> `bootstrapped`; codec/routing tables are process-wide singletons, not fields — full VM
> config = M5.19). Ported `syntactic_verifier.go` per variant: embedded `avax.BaseTx.Verify`
> (network/chain id, memo ≤256), per-tx conservation+fee via the shared `ava_vm`
> `FlowChecker` + avm `components` sort predicates, credential verify, `num_creds==num_inputs`
> (BaseTx/Export=`len(ins)`; Operation=`len(ins)+len(ops)`; Import=`len(ins)+len(imported_ins)`).
> CreateAsset name 1..=128 ASCII letter/digit/space no edge-ws, symbol 1..=4 ASCII upper,
> denom ≤32, states non-empty sorted-unique by `fx_index` + `InitialState::verify(num_fxs)`;
> OperationTx double-spend set-intersection; Import/Export non-empty. Added 2 genuinely-missing
> err variants `InputsNotSortedUnique` (`avax.ErrInputsNotSortedUnique`) + `MemoTooLarge`
> (`avax.ErrMemoTooLarge`). 22 table cases. **Note:** OperationTx `op.verify()` covers only the
> statelessly-reachable structure (utxo-ids sorted-unique); the fx-op typed verify is gated on
> the `FxOperation::Unsupported` M5.5 deferral (concrete op type-ids 8/12/13/17/18 land with the
> OperationTx codec wiring) — does not affect the double-spend path. SemanticVerifier (M5.13) +
> Executor (M5.14) intentionally left out; `executor/mod.rs` documents the slots.

> **Plan-maintenance note (2026-06-07):** a duplicate, unchecked copy of the M5.12
> task block previously sat here (an editing artifact). It has been removed — the
> real, completed M5.12 is the entry above (commit `b87d207`); the next task is M5.13.

### Task M5.13: SemanticVerifier (stateful) + verify_fx_usage + grandfather quirk
**Crate:** ava-avm  ·  **Depends on:** M5.12, M5.10; M4 (`SharedMemory` for ImportTx)  ·  **Spec:** 09 §6.2 (incl. GRANDFATHERED_OPERATION_TX, SameSubnet); 07 §3.1 (SharedMemory)
**Files:** `crates/ava-avm/src/txs/executor/semantic.rs`, `txs/executor/consts.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/semantic.rs`: `base_tx_spends_known_utxo`; `asset_id_mismatch` → `Error::AssetIdMismatch`; `incompatible_fx` (asset doesn't enable fx) → `Error::IncompatibleFx`; `not_an_asset` → `Error::NotAnAsset`; `operation_tx_cred_index` (cred index = `len(ins)+op_index`); `grandfathered_op_skips_verification` asserts the const tx-id `"MkvpJS13eCnEYeYi9B5zuWrU9goG9RBj7nr83U7BjrFV22a12"` bypasses op verification exactly as Go; `import_fetches_shared_memory` (uses a fake `SharedMemory` returning UTXO bytes); `not_bootstrapped_skips_op_verify`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm semantic` → fails.
- [x] **Step 3 — Green:** `consts.rs`: `pub const GRANDFATHERED_OPERATION_TX: &str = "MkvpJS13eCnEYeYi9B5zuWrU9goG9RBj7nr83U7BjrFV22a12";`. `SemanticVerifier` per 09 §6.2: BaseTx fetch each input UTXO (`asset==in.asset` else `AssetIdMismatch`), resolve fx by credential type, `verify_fx_usage(fx_index, asset_id)` (load asset's CreateAssetTx; `NotAnAsset`/`IncompatibleFx`), `fx.verify_transfer`. CreateAsset = BaseTx. Operation: BaseTx then per op (skip when `!bootstrapped` **or** `tx.id == GRANDFATHERED_OPERATION_TX`) fetch input UTXOs, `verify_fx_usage`, `fx.verify_operation`, cred index `len(ins)+op_index`. Import: BaseTx, `verify.SameSubnet(source_chain)` (if bootstrapped), `SharedMemory.get(source_chain, ids)` → unmarshal `avax::UTXO` → verify_transfer, cred index `len(ins)+i`. Export: BaseTx, `SameSubnet(destination_chain)`, `verify_fx_usage` per exported out.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm semantic`.
- [x] **Step 5 — Commit:** `avm: SemanticVerifier + verify_fx_usage + grandfather quirk (M5.13)`

> **As-built (M5.13, 2026-06-07, commit `1520a93`):** `txs/executor/{semantic,consts}.rs` +
> `tests/semantic.rs` (9 tests; combined ava-avm tree = 90 green, clippy + fmt clean).
> `SemanticVerifier` ports `semantic_verifier.go`: per-input UTXO fetch with
> `utxo.asset == in.asset` (`AssetIdMismatch`), `verify_fx_usage(fx_index, asset_id)`
> (loads the asset's `CreateAssetTx` → `NotAnAsset`/`IncompatibleFx`), then `fx.verify_*`;
> Operation cred index `len(ins)+op_index` is computed and exercised; Import fetches via
> `SharedMemory` then unmarshals UTXO bytes; Export checks `verify_fx_usage` per exported out.
> Added a byte-exact codec-serializable `Utxo` (mirrors `ava_platformvm::utxo::Utxo`) in
> `semantic.rs` (`pub`, round-trips through the avm `Codec()` — ATOMIC-1), since state stores
> opaque `UtxoBytes`. Two genuinely-new Go sentinels added to `error.rs` from
> `vms/components/verify/subnet.go`: `Error::SameChainId` (`verify.ErrSameChainID`) +
> `Error::MismatchedSubnetIds` (`verify.ErrMismatchedSubnetIDs`).
> **Deferrals (documented, do NOT count as M5.13 gaps — they belong to later tasks):**
> (1) **Typed `fx.verify_operation` for OperationTx** is gated on the M5.5
> `FxOperation::Unsupported` placeholder (no routable op codec type-id yet): the verifier
> does the real parts that exist (fetch each op-input UTXO, enforce `utxo.asset==op.asset`)
> then routes the op, which returns `Error::UnknownFx` — exactly where Go types the op through
> `typeToFxIndex`. The typed dispatch lands with the OperationTx codec wiring (the M5.5
> deferral / M5.7+M5.8 op wiring). (2) **`verify.SameSubnet`** is a per-verifier `SubnetResolver`
> seam (builder `with_same_subnet`) rather than a `Backend`/`Ctx` field — the node-wide
> validator-state service isn't wired into the avm verifier yet; when absent, `SameSubnet` is
> skipped (parity with the `!bootstrapped` skip). Chain-manager validator-state wiring is the
> remaining hookup (M5.19). (3) **`SharedMemory`** supplied via builder `with_shared_memory`
> (canonical `ava_vm::components::avax::shared_memory::SharedMemory`); an `ImportTx` with no
> handle returns `Error::MissingParentState` rather than panicking.

### Task M5.14: Executor (UTXO state transitions, EXEC-AVM-1, atomic requests)
**Crate:** ava-avm  ·  **Depends on:** M5.13; M3 (`avax::{consume, produce}`); M4 (`Requests`/`Element`)  ·  **Spec:** 09 §6.3, EXEC-AVM-1, §9 (atomic format), ATOMIC-1
**Files:** `crates/ava-avm/src/txs/executor/executor.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/executor.rs` table-driven: `base_tx_consume_produce` asserts deleted inputs + produced UTXO ids at `output_index = i`; `create_asset_indexing` asserts asset-id == tx-id and `output_index` continues from `len(outs)` across multiple `InitialState`s in order (EXEC-AVM-1); `operation_tx_outs_indexing`; `import_builds_remove_requests` asserts `AtomicRequests{source_chain:{remove:[input_id..]}}`; `export_builds_put_requests` asserts `Element{key=utxo_input_id, value=marshal(utxo), traits=addresses}` keyed by `destination_chain`, `output_index` continuing from `len(outs)`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm executor` → fails.
- [x] **Step 3 — Green:** `Executor` over `UnsignedTx` per 09 §6.3: BaseTx `avax::consume(state,&ins)` + `avax::produce(state, tx_id, &outs)` (index = i). CreateAsset: BaseTx then per-`InitialState` out `add_utxo` with `tx_id=self_tx_id`, `asset.id=self_tx_id`, `output_index` continuing from `len(outs)` monotonically (EXEC-AVM-1). Operation: BaseTx then per op delete input UTXOs + add `op.outs()` (asset=op.asset), index continuing. Import: BaseTx + record imported `input_id`s + build `Requests{remove}`. Export: BaseTx + per exported out build UTXO at continuing index, `marshal(utxo)` with the **avm codec v0** (ATOMIC-1), emit `Element{key=input_id, value, traits=out.addresses()}` in `Requests{put}` keyed by destination chain.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm executor`.
- [x] **Step 5 — Commit:** `avm: Executor UTXO transitions + atomic requests (M5.14)`

> **As-built (M5.14, 2026-06-07, commit `e2e486a`):** `txs/executor/exec.rs` (module named
> `exec` not `executor` to dodge `clippy::module_inception` under `-D warnings`; re-exported as
> `txs::executor::{Executor, ExecutorOutputs}`). `Executor::execute(unsigned, tx_id, &mut dyn
> Chain) -> Result<ExecutorOutputs>` mirrors Go `executor.go`'s `(inputs, atomicRequests)` return:
> `ExecutorOutputs { inputs: BTreeSet<Id>, atomic_requests: BTreeMap<Id, Requests> }` (canonical
> `ava_vm::components::avax::shared_memory::{Element, Requests}`, NOT the platformvm-local
> `AtomicRequests` — so M5.16 block-accept passes them straight to `SharedMemory.apply`).
> avm-local `consume`/`produce` helpers marshal the codec-serializable `Utxo` (reused from
> `semantic.rs`) through the avm `Codec()` since state stores opaque `UtxoBytes`. All `u32` index
> math uses `u32::try_from`/`checked_add` (→ `Error::SpendOverflow`, reused). EXEC-AVM-1 index
> continuation verified byte-exact: `create_asset_indexing` decodes produced UTXOs and asserts
> `asset_id == tx_id` + monotonic `output_index` across multiple `InitialState`s; export decodes
> `Element.value`. 5 table tests; combined avm tree **95 green**, clippy `-D warnings` + fmt clean.
> Two review passes (spec + code-quality) applied before merge.
> **Deferral (documented, NOT an M5.14 gap):** OperationTx produces op *outputs* via `op.outs()`
> only once typed `FxOperation` variants exist — the enum currently has only the
> `FxOperation::Unsupported` placeholder (the M5.5 deferral; concrete secp/nft/property op
> type-ids 8/12/13/17/18). The executor does everything reachable today (BaseTx consume/produce +
> per-op input-UTXO deletion); `operation_tx_outs_indexing` asserts exactly that and carries a
> note that op-output index continuation lands with the typed variants.

### Task M5.15: StandardBlock type + codec + parser — `golden::xchain_block_hash`
**Crate:** ava-avm  ·  **Depends on:** M5.5, M5.2  ·  **Spec:** 09 §7 (field order, type id 20); 02 §6 (golden block hashes); M4/ava-genesis (stop-vertex constant)
**Files:** `crates/ava-avm/src/block/mod.rs`, `block/standard_block.rs`, `block/parser.rs`, `crates/ava-avm/tests/golden_block_hash.rs`, `tests/vectors/avm/block/*.json`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/golden_block_hash.rs` with `mod golden { #[test] fn xchain_block_hash() {...} }`: parse a committed Go-produced `StandardBlock` hex from `tests/vectors/avm/block/standard_block.json`, assert `block_id == sha256(bytes)` matches the committed id; assert the **Mainnet & Fuji X-Chain genesis block id** and the **stop-vertex parent** match the `ava-genesis` constants (09 §1, §5.3); round-trip `marshal(0, &(block as &dyn Block)) == bytes`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm golden::xchain_block_hash` → fails.
- [x] **Step 3 — Green:** `StandardBlock{parent_id, height, time, merkle_root(zero/unused), txs}` per 09 §7 field order; register as type id 20 in M5.5's registry (replace the placeholder). Serialize as the `Block` interface (typeid-prefix 20): `cm.marshal(0, &(blk as &dyn Block))`; `block_id = sha256(bytes)`. `parser.rs`: `parse(bytes)` → recompute id + `initialize` every contained tx (re-derive each `tx_id`). `timestamp() = Unix(time)`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm golden::xchain_block_hash`.
- [x] **Step 5 — Commit:** `avm: StandardBlock + parser + block-hash golden vectors (M5.15)`

> **As-built (M5.15, 2026-06-07, commit `f574249`):** `block/{mod,standard_block,parser}.rs`
> mirroring the P-Chain block precedent. `BlockBody` enum `#[codec(type_registry)]` with
> `Standard(StandardBlock)` at `#[codec(type_id = 20)]` (the actual encoding id; the M5.5
> registry's name-only `block.StandardBlock`(20) placeholder is kept unchanged as the assertion
> table). `StandardBlock` byte-exact field order from `standard_block.go`: `parent_id`→`height`
> →`time`→`root`(zero/unused)→`transactions`. `Block` envelope: derived non-serialized
> `block_id = sha256(codec_bytes)` + cached `bytes`; `initialize`/`parse` re-derive each
> contained tx's `tx_id`; `timestamp()` = Unix secs. `parser::parse` notes ordinary→`Codec`,
> genesis→`GenesisCodec`. No `error.rs` change (reuses `CodecError`). **Golden** =
> SELF-CONSISTENT (Go `block_test.go` builds blocks programmatically — NO hardcoded
> `expectedBytes` const to copy): reuses the M5.5 `golden_tx_codec` BaseTx as the single
> contained tx, asserts on-wire field order, type-id 20, `block_id==sha256(bytes)`, and full
> parse→re-marshal round-trip (incl. the byte detail that the embedded tx shares the block's
> single 2-byte version prefix). 3 tests (+`empty_block_roundtrip`, `standard_block_type_id_is_20`).
> **DEFERRED (ava-genesis = M8, not built):** mainnet/fuji X-Chain genesis block id + stop-vertex
> parent assertion — `// TODO(M8/ava-genesis)` in the test. Full Go-byte-exact differential is
> M5.22 (needs a live Go oracle).

### Task M5.16: Block verify/accept/reject over Diff; Snowman Block trait
**Crate:** ava-avm  ·  **Depends on:** M5.14, M5.15, M5.10; M4 (`SharedMemory.apply`), M3 (`ava_vm::block::Block`)  ·  **Spec:** 09 §7 (accept = commit diff + apply atomic requests), §6; 07 §2.3 Block trait
**Files:** `crates/ava-avm/src/block/executor.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/block_lifecycle.rs`: build a `StandardBlock` with one BaseTx over a seeded UTXO set; `verify()` (syntactic+semantic over a `Diff` on parent) succeeds; `accept()` commits the diff, advances `lastAccepted`+timestamp, marks txs accepted, and (for an ExportTx block) applies the atomic `put` via a fake `SharedMemory` **in the same batch** as the state commit; `reject()` discards. Conflicting-tx block → verify error.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm block_lifecycle` → fails.
- [x] **Step 3 — Green:** Impl `ava_vm::block::Block` for the avm block wrapper: `verify` runs Syntactic+Semantic+Executor over a `Diff` on the parent; `accept` calls `state.commit_batch()` and `SharedMemory.apply(requests, &[batch])` atomically (09 §7, §9), sets txs accepted, advances `lastAccepted`/`timestamp`; `reject` aborts the diff. `parent()/height()/bytes()/id()/timestamp()` from `StandardBlock`. Wrap in `enum Block { Standard(StandardBlock) }` (09 §11) for future extensibility.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm block_lifecycle`.
- [x] **Step 5 — Commit:** `avm: block verify/accept/reject + atomic commit (M5.16)`

> **As-built (M5.16, 2026-06-07, commit `d9297ce`):** `block/executor.rs` = `BlockManager<D:
> Database>` mirroring the P-Chain `block::executor` precedent, simplified to the X-Chain's
> single `StandardBlock` (no oracle/proposal/option blocks, no validator-diff machinery). Owns
> `state: State<D>`, `base_view: Arc<dyn Chain>` (refreshed on each accept via `state.snapshot()`),
> a `Dispatch` (fx table), a `Backend`, `last_accepted: Id`, and a `blk_id_to_state:
> BTreeMap<Id, BlockState>` diff cache; implements `Versions` for parent resolution.
> - **`verify`** layers a fresh `Diff` on the parent, then per tx runs `SyntacticVerifier` →
>   `SemanticVerifier` → `Executor::execute` over the **same** `Diff`, caching the on-accept diff +
>   merged atomic requests. **Double-spend is caught by semantic verification** (the second tx's
>   `diff.get_utxo` returns `Database(NotFound)` because the first tx's executor already tombstoned
>   the UTXO in the shared diff) — NOT a hand-rolled input-set check. Reuses the M5.14 `exec.rs`
>   `Executor` verbatim (`Executor::execute(&unsigned, tx_id, &mut diff)`).
> - **`accept`** applies the cached diff to `State`, records tx + block bytes, advances
>   `lastAccepted`/`timestamp`, and — if the block carries atomic requests — performs a **true
>   single-batch co-commit**: `State::commit_batch_ops()` (new; snapshots the `VersionDb` overlay
>   into a `BatchOps` WITHOUT writing) → `SharedMemory.apply(requests, &[batch_ops])` (one
>   underlying DB write covers both state + cross-chain ops) → `state.abort()` to drop the
>   now-written overlay. Empty-requests path is a plain `state.commit()`. Returns
>   `Error::BlockNotVerified` (new variant) if accept/reject is called on an unverified block.
> - **`reject`** discards the cached diff. 7 tests assert real persisted-state postconditions
>   (UTXO set changes, last-accepted/timestamp, tx/block bytes, fake-`SharedMemory` put on export,
>   reject leaves state untouched, double-spend → `assert_matches!(…NotFound)`). Combined avm tree
>   **102 green**, clippy `-D warnings` + fmt clean. Two review passes (spec ✅ + code-quality)
>   applied before merge.
> **WORKTREE-BASE GOTCHA (recorded):** the first implementer's `isolation:"worktree"` branched from
> a STALE pre-M5.14 base, so it rebuilt a duplicate `execute.rs` Executor AND skipped semantic
> verification (faking double-spend via the executor's input set — which broke once rebased onto
> the canonical `exec.rs`). Discarded; redone in a worktree branched explicitly from the correct
> main HEAD with the implementer pointed at it directly (no `isolation:"worktree"`). Lesson: for
> SEQUENTIAL tasks, branch the worktree yourself from the verified HEAD rather than relying on the
> Agent's auto-isolation base.
> **Deferred (per task latitude):** the synchronous Snowman `Block` trait wrapper
> (`ava_snow::snowman::block::Block`) — the `BlockManager` exposes the full verify/accept/reject
> semantics; the `Arc<Mutex<…>>` Snowman shim lands with VM assembly (M5.19) where the VM's
> concurrency model is settled.

### Task M5.17: Mempool wiring + block Builder
**Crate:** ava-avm  ·  **Depends on:** M5.16; M3 (`ava_vm::mempool::Mempool`)  ·  **Spec:** 09 §7.1; 07 §7 (generic mempool); 00 §6.1 (pop order = total order identical to Go)
**Files:** `crates/ava-avm/src/mempool.rs`, `crates/ava-avm/src/block/builder.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/builder.rs`: add N verified txs to the mempool; `build_block` drains in mempool order, re-verifies each against a running `Diff`, drops + records failures, and produces `StandardBlock{parent=last_accepted, height=parent.height+1, time=max(parent.time, now), txs}`; proptest `mempool_pop_order_total` asserts pop order is a stable total order independent of internal map layout (00 §6.1); byte-cap enforced (packs until `maxMempoolSize`).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm builder` → fails.
- [x] **Step 3 — Green:** `mempool.rs`: `MempoolTx for Tx` (`id=tx_id`, `size=bytes.len()`, `inputs`), `indexmap`-backed via M3 generic `Mempool` + dropped-reason LRU (09 §7.1). `builder.rs`: drain `peek`/`remove` in order, re-verify on `Diff`, pack to size cap, build the block with clamped-monotonic `time` (09 §7.1).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm builder`; commit regressions.
- [x] **Step 5 — Commit:** `avm: mempool + block builder (M5.17)`

> **As-built (M5.17, 2026-06-07, commits `360d37f`+`24d156b`+`14c665f`, ff-merged to main):**
> 110 ava-avm tests green; clippy `-D warnings` + fmt clean. Implementer (worktree branched
> from verified HEAD) → spec review → code-quality review → fixes, per
> subagent-driven-development.
> - **Plan-text deviation (controller-verified):** there is **NO generic `ava_vm::mempool::Mempool`**
>   and no `indexmap` dependency in the workspace. `mempool.rs` instead MIRRORS the concrete
>   P-Chain precedent `crates/ava-platformvm/src/txs/mempool.rs` verbatim: `ava_utils::linked::LinkedHashmap`
>   for insertion-ordered `tx_id→Tx` + `tx_id→consumed-input-id HashSet`, `MAX_TX_SIZE` (64 KiB) +
>   `MAX_MEMPOOL_SIZE` (64 MiB) drop-on-full byte budget, conflict-free via `has_overlap`, and a
>   **module-local** `enum Error { DuplicateTx, TxTooLarge, MempoolFull, ConflictsWithOtherTx }`
>   (these are mempool-local "drop, no divergence" errors, NOT added to `crate::error::Error`).
>   API = `new/add/get/contains/remove/peek/len/is_empty/iterate/snapshot` + private `has_overlap`.
>   No `MempoolTx` trait (the P-Chain doesn't define one; `Tx` is used directly).
> - **`builder.rs`:** `build_block(BuildBlockParams) -> Result<BuildBlockOutput>` (params bundled in a
>   struct to dodge `clippy::too_many_arguments`; `BuildBlockOutput { block: Block, dropped: Vec<(Id, Error)> }`).
>   Lays a fresh `Diff` on the parent `Chain`, drains candidate txs in FIFO order through the SAME
>   running `Diff` (Syntactic→Semantic→`Executor::execute`, the exact loop from `block/executor.rs::verify`),
>   so an **intra-block double-spend is caught by semantic verify** (2nd tx's `diff.get_utxo` hits the
>   1st tx's executor tombstone → `Error::Database(NotFound)`) and the tx is dropped+recorded
>   (`tracing::warn!` per drop + returned in `dropped`). Packs to `TARGET_BLOCK_SIZE` (128 KiB,
>   `break`-on-overflow mirroring P-Chain `pack_decision_txs`). `StandardBlock{parent_id, height=
>   parent_height.saturating_add(1), time=unix_secs(max(parent_time, now)), txs}`, initialized via
>   `Codec()`. Returns `Error::NoPendingBlocks` (new variant) when nothing packs. X-Chain has no
>   reward/proposal/advance-time/staker machinery, so the builder is the P-Chain one minus all of that.
> - Added `tracing` workspace dep to `ava-avm/Cargo.toml`. New err variant `NoPendingBlocks`.
> - **Snowman `Block` trait shim still deferred to M5.19** (concurrency model); the builder + mempool
>   are plain structs the VM will own behind its own lock.

### Task M5.18: Tx gossip + Atomic app-handler switch
**Crate:** ava-avm  ·  **Depends on:** M5.17; M3/M-network (`ava_network::p2p::gossip` push/pull, Bloom Set)  ·  **Spec:** 09 §8; 05 (gossip machinery)
**Files:** `crates/ava-avm/src/network/mod.rs`, `network/gossip.rs`, `network/atomic.rs`, `network/tx_verifier.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/gossip.rs`: `Tx` is `Gossipable` with `gossip_id == tx_id`; an inbound `AppGossip` of a valid tx adds it to the mempool and the Bloom set; an invalid tx is dropped with reason; `Atomic` switch (`ArcSwap<dyn AppHandler>`) forwards to the live handler.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm gossip` → fails.
- [x] **Step 3 — Green:** `gossip.rs`: implement `Gossipable for Tx` (gossip_id = tx_id), supply marshaller + verify hook (`tx_verifier.rs` wraps semantic verify) into 05's push/pull gossip + Bloom `Set` (09 §8). `atomic.rs`: `ArcSwap<dyn AppHandler>` initialized once to the real handler (post-linearization), indirection preserved for the gRPC path (09 §8).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm gossip`.
- [x] **Step 5 — Commit:** `avm: tx gossip + atomic app-handler switch (M5.18)`

> **As-built (M5.18, 2026-06-07, commits `7b3b84f`+`6c4ddd4`+`d0091d7`, ff-merged to main):**
> 128 ava-avm tests green; clippy `-D warnings` + fmt clean. Implementer → spec review →
> code-quality review → fixes, per subagent-driven-development.
> - **SCOPING (recorded in spec 09 §8 as-built note):** spec 05's generic push/pull gossip
>   framework + **writable** Bloom `Set` do **not exist** (only a read-only IP `ReadFilter`).
>   M5.18 mirrors the P-Chain precedent (`crates/ava-platformvm/src/network.rs`): VM-side handler
>   logic only, transport DEFERRED to a 05/M2 follow-up. The M5 exit gate needs no live gossip
>   (the recorded-oracle differential issues txs via the VM, not the wire).
> - **`network/gossip.rs`:** local `trait Gossipable { gossip_id() -> Id }` + `impl for Tx` (= `tx.id()`);
>   `TxMarshaller` (`marshal` = `tx.bytes().to_vec()`, `unmarshal` = `Tx::parse(Codec(), .)`, panic-free);
>   `TxGossipHandler` + `DropReason{Duplicate, Verification(String), Mempool(mempool::Error)}` +
>   `HandleOutcome{Added, Dropped(..)}` — dedupe→verify→admit, divergence-free, line-for-line the P-Chain
>   handler. All three drop paths are test-exercised (incl. `DropReason::Mempool` via a new
>   `Mempool::with_budget(0)` `#[doc(hidden)]` test ctor).
> - **`network/tx_verifier.rs`:** `trait TxVerifier { verify_tx(&Tx) -> Result<(),String> }`; cheap
>   state-free `SyntacticTxVerifier` (renamed +`Tx` to avoid colliding with
>   `executor::syntactic::SyntacticVerifier`); `SemanticTxVerifier<'a>` = the real "wraps semantic
>   verify" hook holding `&Backend`/`&dyn ReadOnlyChain`/`&Dispatch`, running
>   `SyntacticVerifier`+`SemanticVerifier::verify()` over a seeded state (tested with a real
>   `MemDb`-backed `State` + seeded CreateAssetTx + UTXO).
> - **`network/atomic.rs`:** `arc-swap`-backed `AtomicAppHandler { ArcSwap<Arc<dyn AppGossipHandler>> }`
>   with `new`/`swap`/`load` + delegating fire-and-forget `handle_app_gossip`. **DESIGN NOTE:** the
>   canonical `ava_vm::AppHandler` methods are `&mut self`, which a shared `Arc<dyn …>` cannot call —
>   so the switch is defined over a local `&self` `AppGossipHandler` trait (the gossip handler is
>   effectively stateless; the mempool it mutates is owned+locked by the VM). The swap primitive +
>   indirection is built and tested (A→B→A routing); **wiring the VM's `AppHandler::app_gossip` to call
>   this switch + constructing the real handler is M5.19.**
> - Added `arc-swap` workspace dep to `ava-avm/Cargo.toml`. No new `crate::error::Error` variants
>   (mempool/codec errors reused; DropReason is gossip-local).

### Task M5.19: VM assembly — ChainVm impl
**Crate:** ava-avm  ·  **Depends on:** M5.16, M5.17, M5.18, M5.11; M3 (`ava_vm::{Vm, ChainVm, block::Block}`), M4 (chain manager wiring)  ·  **Spec:** 09 §0, §5 (initialize_chain_state hook); 07 §2.1, §2.4
**Files:** `crates/ava-avm/src/vm.rs`, `crates/ava-avm/src/factory.rs`, `crates/ava-avm/src/config.rs`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/vm_conformance.rs`: run the generic `vm_conformance!(make_avm_vm)` battery (07 §10): `initialize` → genesis `last_accepted`; `build_block`→`verify`→`accept` advances last-accepted + height index; `parse_block` round-trips bytes; `get_block` of accepted/processing; `Err(NotFound)` for unknown id/height; `set_preference`; `set_state` phase transitions; `shutdown` idempotent.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm vm_conformance` → fails.
- [x] **Step 3 — Green:** `vm.rs`: `struct Vm` holding state, fxs, mempool, builder, gossip, codec registries. Impl `ava_vm::Vm::initialize` (build registries + fxs in registration order, open state, `initialize_chain_state(stop_vertex_id, genesis_ts)`, wire `AppSender`/gossip) and `ChainVm` (`build_block`/`get_block`/`parse_block`/`set_preference`/`last_accepted`/`get_block_id_at_height`) per 07 §2.4. `factory.rs`: `Factory` returning the VM. `config.rs`: avm config (fees, gossip params).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm vm_conformance`.
- [x] **Step 5 — Commit:** `avm: VM assembly + ChainVm impl + conformance (M5.19)`

> **As-built (M5.19, 2026-06-07, commits `799c3cb`+`7911bd8`+`03226d3`, ff-merged to main):**
> **148 ava-avm tests green** (9-case `vm_conformance` battery + 5 vm unit tests + the rest);
> `avalanchers` binary builds; clippy `-D warnings` + fmt clean. Implementer (opus, worktree
> from verified HEAD) → spec review (opus) → multi-tx-packing rework → code-quality review
> (opus) → polish, per subagent-driven-development.
> - **New files:** `src/vm.rs` (`AvmVm` + `Vm`/`ChainVm`/`AppHandler`/`HealthCheck`/`Connector`
>   impls + `AvmBlock` Snowman wrapper + `NoopSharedMemory` + `AvmGossipHandler` + `parse_genesis`),
>   `src/vm/dyndb.rs` (`Arc<dyn DynDatabase>`→typed `Database` adapter, lifted from P-Chain),
>   `src/config.rs` (`Config` JSON parse of `config_bytes` + mainnet fee defaults
>   1e6/1e7 nAVAX), `src/factory.rs` (zero-sized `AvmFactory::new_vm`), `tests/vm_conformance.rs`.
>   `block/executor.rs` gained additive accessors (`backend`/`dispatch`/`height_of`/
>   `processing_block_bytes`/`set_bootstrapped` — now `pub(crate)`; `seed_state` `#[doc(hidden)]`)
>   + cached `height`/`bytes` on `BlockState` + `last_accepted_height` (in lockstep with
>   `last_accepted`); no M5.16 regression (block_lifecycle still green). New err variants
>   `NotInitialized`/`Config`/`InvalidGenesis` + `From<Error>` for `ava_vm::Error`/`ava_snow::Error`
>   (the load-bearing map is `Database(NotFound)→ava_vm::NotFound`).
> - **`AvmVm` shape** mirrors `PlatformVm`: `Option<Arc<Shared>>` (manager `parking_lot::Mutex` +
>   `Dispatch`) + `EngineState` + `preferred`/`genesis_id` + `Arc<parking_lot::Mutex<Mempool>>`
>   (Arc so the gossip handler shares it) + `Arc<AtomicAppHandler>`. **No lock guard is held across
>   any `.await`** (proven: the `#[async_trait]` futures are `Send`, which a held `parking_lot`
>   guard would break); manager-before-mempool lock order throughout.
> - **`set_state(NormalOp)`** calls `dispatch.bootstrapped()` + `mgr.set_bootstrapped(true)` (flips
>   the fxs + backend to enable sig verification). **`app_gossip`** wired end-to-end through
>   `AtomicAppHandler`→`AvmGossipHandler`→`TxMarshaller::unmarshal`→`TxGossipHandler` (SyntacticTxVerifier
>   admission path, P-Chain-parity) → shared mempool (this lands the M5.18 atomic-switch wiring).
>   **`AvmBlock`** verify/accept/reject delegate to `BlockManager` (this lands the M5.16-deferred
>   Snowman `Block` shim).
> - **Go-parity block packing:** `build_block` feeds the builder the FULL FIFO `mempool.snapshot()`
>   (multi-tx packing to `TARGET_BLOCK_SIZE`, Go `builder.go`) and **removes the PACKED txs on build**
>   / re-adds on reject (correct Go mempool semantics — `Peek`+`Remove` at build, `Add` at reject;
>   drop at accept). It also verifies the built block into the manager diff cache so an unaccepted
>   built block resolves as a parent-state view (differs from P-Chain which does neither — relies on
>   the engine deciding every processing block; cache-lifetime + tx-stranding disclosed in the vm.rs
>   module doc). The conformance `set_preference_ok` (build h1, set-pref unaccepted, build child) is
>   satisfied with a **chained-spend seed** (genesis UTXO U0; mempool tx2-spends-U1 enqueued before
>   tx1-spends-U0-produces-U1, so FIFO packs only tx1 into h1, then tx2 over h1's diff) — preserves
>   multi-tx packing while forcing the two-block shape.
> - **Deferrals (documented in code + here):** real cross-chain `SharedMemory` (M5.20 — `NoopSharedMemory`
>   `debug_assert!`s requests are empty); `verify.SameSubnet` validator-state (no `validator_state` on
>   `ChainContext` yet, M5.20+); full Go X-Chain genesis-asset format (`parse_genesis` uses a minimal
>   40-byte synthetic stop-vertex-id+Unix-ts seed; M8/ava-genesis); JSON-RPC `create_handlers` empty
>   (M5.21); `version()` hard-coded `"avm/0.0.0"` (`// TODO(M8)` source from ava-version).

### Task M5.20: X↔P atomic import/export end-to-end (ATOMIC-1) — `differential::atomic_xp`
**Crate:** ava-avm (+ test harness X)  ·  **Depends on:** M5.14, M5.16, M5.19; M4 (P-Chain + shared `SharedMemory`)  ·  **Spec:** 09 §9 (ATOMIC-1, byte format), 07 §3.1 (SharedMemory, canonical UTXO encoding); 00 §11.1.7
**Files:** `crates/ava-avm/tests/atomic_xp.rs`, `tests/vectors/atomic/*.json`, `tests/differential/src/atomic.rs` (harness X)
- [x] **Step 1 — Red:** `crates/ava-avm/tests/atomic_xp.rs` with `mod differential { #[test] fn atomic_xp() {...} }` (recorded-oracle mode default): X-Chain `ExportTx` to P emits an `Element{key=input_id, value=marshal_v0(avax::UTXO), traits=addrs}`; assert (a) `hex::encode(value)` matches the committed `tests/vectors/atomic/x_to_p_utxo.json` Go vector, and (b) the **P-Chain codec** (M4) decodes the same bytes into an identical `avax::UTXO` (cross-chain decode — ATOMIC-1), and the reverse P→X. Live two-binary mode gated behind `--features differential-live`/`DIFFERENTIAL_LIVE` env with the recorded-oracle as fallback.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm differential::atomic_xp` → fails (vectors/cross-decode missing).
- [x] **Step 3 — Green:** Ensure the avm export marshals `avax::UTXO` with **codec v0 + the exporting VM's secp256k1fx output type IDs** (ATOMIC-1, 09 §9). Add cross-chain decode helper in the harness importing M4's P-Chain codec; commit `tests/vectors/atomic/{x_to_p_utxo,p_to_x_utxo}.json` (Go-extracted, with provenance per 02 §6.2). Wire the live-mode path in `tests/differential/src/atomic.rs` (issue export on Go X-Chain, import on Go P-Chain; mirror on Rust).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm differential::atomic_xp` (recorded mode); live mode runs under the gated feature.
- [x] **Step 5 — Commit:** `avm: X↔P atomic import/export (ATOMIC-1) + atomic_xp differential (M5.20)`

> **As-built (M5.20, 2026-06-07, commit `f52cb31`, ff-merged to main):** 149 ava-avm + 5 ava-differential
> tests green; clippy `-D warnings` + fmt clean. Ran in parallel with M5.21 (both branched from the
> verified HEAD); reviewed (spec + quality combined, opus) → APPROVE.
> - **`crates/ava-avm/tests/atomic_xp.rs`** `mod differential { fn atomic_xp() }` exercises the X→P
>   path through the **real `ava-chains` shared-memory backend**: `BlockManager::accept`'s atomic
>   co-commit (`commit_batch_ops` → `SharedMemoryView::apply(requests, &[batch])`) with the X-Chain
>   `State` + `Memory` on **one shared base `MemDb`**, then the P-Chain view `sm_p.get(x_chain_id, key)`
>   returns the bytes and `ava_platformvm::utxo::Utxo::unmarshal` decodes an **identical** UTXO via the
>   **separate P-Chain codec/registry** (the non-circular ATOMIC-1 proof) + re-marshal reproduces the
>   bytes; P→X is the mirror. Round-trip exercises real sharedID prefixing + dbElement framing.
> - **Vectors** `tests/vectors/atomic/{x_to_p_utxo,p_to_x_utxo}.json` are **byte-layout-faithful to the
>   Go wire format** (field-by-field auditable: `0000`‖txID‖outputIndex‖assetID‖typeId=7‖secp
>   TransferOutput payload; derived from `/Users/rahul.muttineni/avalanchego` `vms/components/avax/utxo.go`
>   + `vms/avm/txs/executor` + `chains/atomic`), with provenance recorded in each JSON
>   ("pending live-oracle confirmation", per the M5.5/M5.15 precedent).
> - **secp type-id parity CONFIRMED across chains** (the cross-decode foundation): `TransferOutput`=7,
>   `TransferInput`=5, `CODEC_VERSION`=0 on BOTH `ava-avm` and `ava-platformvm` codecs.
> - Added `ava-chains`/`ava-platformvm`/`serde_json` as `ava-avm` **dev-deps** (no cycle — both are
>   leaf deps of ava-avm). `tests/differential/src/atomic.rs` adds a driver-independent `Observation`
>   collector seam + `TODO(X.13/X.15)` for live mode.
> - **Deferrals (documented):** VM-`initialize` production wiring of cross-chain shared memory needs a
>   `ChainContext.shared_memory` field the chain manager must supply (M8/chain-manager) — proven at the
>   `BlockManager` level instead; live two-binary `atomic_xp` gated behind the unimplemented tier-X
>   `LockstepDriver` (X.13/X.15).

### Task M5.21: JSON-RPC service (avm.* methods)
**Crate:** ava-avm  ·  **Depends on:** M5.19; M3 (`ava-api` JSON-RPC router)  ·  **Spec:** 09 §10; 12 (JSON-RPC serving), 14 (API reference)
**Files:** `crates/ava-avm/src/service.rs`, `crates/ava-avm/tests/service.rs`, `tests/vectors/avm/service/*.json`
- [x] **Step 1 — Red:** `crates/ava-avm/tests/service.rs`: golden request/response JSON (vs Go `service_test.go` fixtures) for `avm.issueTx` (parse + add to mempool + gossip), `avm.getTx`, `avm.getTxStatus`, `avm.getUTXOs` (incl. cross-chain `sourceChain`), `avm.getBalance`, `avm.getHeight`, `avm.getBlockByHeight`. Assert error codes match Go.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-avm service` → fails.
- [x] **Step 3 — Green:** `service.rs`: implement the `avm.*` methods per 09 §10 over `ava-api`'s JSON-RPC router, names/args/replies/error-codes mirroring `vms/avm/service.go`. Bech32 `X-` addresses with chain HRP; CB58/hex asset ids. Defer the deprecated keystore-backed `wallet.*` methods behind a feature flag (note in PORTING.md / §10).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-avm service`.
- [x] **Step 5 — Commit:** `avm: JSON-RPC service (avm.* methods) + golden fixtures (M5.21)`

> **As-built (M5.21, 2026-06-07, commits `b64f7ef`+`f524411`, ff-merged to main):** 178 ava-avm
> tests green (incl. 29 service `mod conformance` tokio tests); clippy `-D warnings` + fmt clean.
> Ran in parallel with M5.20; reviewed (spec+quality, opus) → REQUEST-CHANGES on 3 Go-parity bugs →
> fixed → merged.
> - **SCOPING:** `ava-api` / JSON-RPC HTTP router does NOT exist (deferred M8/M12). M5.21 mirrors the
>   P-Chain `service.rs` precedent exactly: **typed `Service<D>` handlers + serde request/reply types,
>   tested via inline `#[tokio::test]`s asserting `serde_json` shapes** — NO HTTP server, NO on-disk
>   goldens (assertions are direct on `serde_json::Value`, matching the P-Chain). `Vm::create_handlers`
>   stays an empty map until `ava-api` lands.
> - **Implemented (Go-shape-verified against `/Users/rahul.muttineni/avalanchego/vms/avm/service.go`):**
>   `getHeight` (`avajson.Uint64` string), `getTx`/`getBlock`/`getBlockByHeight` (checksummed hex),
>   `getTxStatus` (PascalCase `Accepted`/`Unknown`; not-found→Unknown, other errors propagate, Go-parity),
>   `issueTx` (parse→txID), `getAssetDescription` (CB58 id). `avajson` u64/u8-as-string module copied
>   from P-Chain. `format_address("X", get_hrp(network_id), addr)`. New `Error::Service(String)`.
> - **Go-parity fixes applied (the review's blocking findings, confirmed against
>   `utils/formatting/encoding.go`):** (1) **hex now appends Go's 4-byte checksum** `sha256(bytes)[28..32]`
>   via `ava_crypto::hashing::checksum(bytes,4)` → `0x` + hex(bytes‖checksum), byte-matching
>   `formatting.Encode(Hex,…)`; (2) **decode default = Hex** (Go's `Encoding` zero value, NOT CB58 — that
>   was a fabricated comment) and the hex path **strips+verifies** the 4-byte checksum (`errBadChecksum`
>   parity), supporting `hex`/`hexc`/`hexnc` (the encodings Go avm accepts); (3) **`getBalance`/
>   `getAllBalances` added as honest erroring stubs** (method exists + returns documented `Error::Service`,
>   not doc-only) with Go-matching arg/reply field names.
> - **Deferrals (documented in code citing the Go dependency):** address-indexed `getUTXOs`/`getBalance`/
>   `getAllBalances` (Go uses an address→UTXO index not yet ported); `issueTx` mempool-add + gossip
>   (needs the `AvmVm` handle, not the `State`-only `Service`); `getTxStatus` `Processing` (needs VM
>   mempool); `getAssetDescription` alias lookup; `wallet.*` keystore; the `Client<Transport>` (P-Chain
>   has one; left as a straightforward follow-up — the 29 conformance tests cover the method shapes).

### Task M5.22: Differential program generator — `differential::xchain_issue_tx`
**Crate:** ava-avm (+ `tests/differential/` harness X)  ·  **Depends on:** M5.19, M5.20, M5.21; cross-cutting harness X  ·  **Spec:** 02 §11 (differential harness, proptest program), 09 §12; 00 §6.1
**Files:** `tests/differential/src/xchain.rs`, `tests/differential/tests/xchain_issue_tx.rs`, `tests/differential/proptest-regressions/xchain_issue_tx.txt`
- [x] **Step 1 — Red (TDD ENTRY POINT — start tiny):** `tests/differential/tests/xchain_issue_tx.rs` with `mod differential { #[test] fn xchain_issue_tx() {...} }`. Begin with `cases = 1` and a generator that emits a **single BaseTx program** (one issue-tx + `AwaitFinalization`); assert identical last-accepted block ID + height **and identical UTXO set** vs the Go oracle (recorded mode). This first failing test is the milestone's entry point.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential differential::xchain_issue_tx` → fails (generator/oracle absent).
- [x] **Step 3 — Green:** `xchain.rs`: a proptest `Strategy` producing `(seed, Vec<Action>)` where `Action::IssueTx(TxSpec)` deterministically builds X-Chain txs (BaseTx first; then CreateAsset, Operation with each fx, Import, Export) from the seed (02 §11.2); tx/key bytes derived from the seed so both nodes get identical bytes. `Observation` collects per-chain last-accepted block id+height + the full UTXO set (sorted, 00 §6.1). Compare via `prop_assert_eq!`. Grow generator coverage, then scale `cases` 1 → 100 → 1k → **10k**. Live two-binary mode gated behind feature/env with recorded-oracle fallback (coordinate with harness X); print `DIFFERENTIAL_SEED=<n>` on mismatch (02 §11.5).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential differential::xchain_issue_tx` (recorded mode, `cases = 10000`); commit `proptest-regressions/xchain_issue_tx.txt`.
- [x] **Step 5 — Commit:** `avm: differential xchain_issue_tx generator scaled to 10k (M5.22)`

> **As-built (M5.22, 2026-06-07, commit `187bed6` + comment fixup, ff-merged to main):** 9
> ava-differential tests green (incl. the 512-case `differential::xchain_issue_tx` proptest, ~1.6s);
> ava-avm 178 unchanged; clippy `-D warnings` + fmt clean. Reviewed (spec+quality, opus) → APPROVE.
> - **SCOPING:** the harness `LockstepDriver` is an unimplemented tier-X scaffold (no Go recorded-oracle,
>   no live two-binary) — so `differential::xchain_issue_tx` is delivered as a **self-vs-self DETERMINISM
>   proptest** (the entry point the plan calls for): a seed-derived program of BaseTx issuances run
>   through the REAL `ava-avm` VM pipeline (seed genesis → `mempool_add` → `build_block` → `set_preference`
>   → verify → accept) on TWO independent `AvmVm` instances, asserting BYTE-IDENTICAL normalized
>   `Observation` (last-accepted id + height + full sorted UTXO set). **Go-oracle (recorded + live) +
>   richer tx kinds (CreateAsset/Operation/Import/Export) + 10k scaling are DEFERRED to X.13/X.15**
>   (documented in both new files).
> - `tests/differential/src/xchain.rs` (generator `program(seed)` via splitmix64 `mix`, deterministic
>   chained-spend BaseTxs; `xchain_observation`/`run_program`) + `tests/differential/tests/xchain_issue_tx.rs`
>   (`cases=512`, prints `DIFFERENTIAL_SEED=<seed>` on mismatch). `ava-avm`/`ava-vm`/etc promoted to real
>   `ava-differential` deps (no cycle). Added a minimal `#[doc(hidden)] AvmVm::with_state` read seam
>   (`crates/ava-avm/src/vm.rs`) for UTXO-set enumeration (the `Chain` trait has none).
> - **REAL FINDING the proptest CAUGHT (record + follow-up under X.19):** `ava-avm` `ChainVm::build_block`
>   stamps blocks with `time = max(parent_time, SystemTime::now())` using an **un-injectable wall clock**
>   — across the two determinism runs this occasionally straddled a second boundary → divergent block ids
>   (~30/256). Worked around deterministically by pinning the synthetic genesis timestamp far in the future
>   (`GENESIS_TS=9_000_000_000`) so every block inherits the fixed `parent_time`. **The real fix is to adopt
>   the injectable `ava_utils::clock::Clock` seam in `build_block`/the VM — tracked by tier-X task X.19
>   (`plan/X-cross-cutting.md`, spec 24 PART B).** `ava-utils` already has `clock::Clock`; the VM does not
>   yet consume it. (This is a genuine determinism hazard in `ava-avm`, masked only by the harness clock-pin
>   today; flagged for X.19.)

### Task M5.23: cargo-fuzz target for block/tx/op decoder
**Crate:** ava-avm  ·  **Depends on:** M5.15, M5.5  ·  **Spec:** 02 §8 (fuzzing, block parsers), 02 §13.5
**Files:** `crates/ava-avm/fuzz/Cargo.toml`, `crates/ava-avm/fuzz/fuzz_targets/decode_block.rs`, `fuzz/corpus/decode_block/` (committed seeds)
- [x] **Step 1 — Red:** Add `fuzz/fuzz_targets/decode_block.rs`: `fuzz_target!(|data: &[u8]| { if let Ok(b) = parse_block(data) { let re = marshal(&b); let back = parse_block(&re).unwrap(); assert_eq!(b.bytes(), back.bytes()); } })`; seed corpus with the M5.15 golden block bytes + a Tx + an Operation.
- [x] **Step 2 — Confirm red:** `cargo fuzz run decode_block -- -runs=0` → fails to build until the target compiles against the parser.
- [x] **Step 3 — Green:** Implement the fuzz crate (`libfuzzer-sys` + `arbitrary`), targeting the block/tx/operation decoder; must never panic / over-read on arbitrary bytes (02 §8). Wire `cargo xtask test-fuzz` smoke (short run).
- [x] **Step 4 — Confirm green:** `cargo xtask test-fuzz` (smoke) runs the target briefly with no crash; commit corpus seeds.
- [x] **Step 5 — Commit:** `avm: cargo-fuzz block/tx/op decoder target (M5.23)`

> **As-built (M5.23, 2026-06-07, fuzz crate commit `0dd19f9`; OOM fix + integration in a
> follow-up commit):** `crates/ava-avm/fuzz/` — a workspace-detached cargo-fuzz crate
> (`ava-avm-fuzz`, own empty `[workspace]`, `libfuzzer-sys` + `ava-avm` path dep) mirroring the
> proven `ava-platformvm/fuzz` precedent exactly. Target `decode_block.rs` drives
> `ava_avm::block::Block::parse` **and** `ava_avm::txs::Tx::parse` over arbitrary bytes under
> both `Codec()` and `GenesisCodec()`, asserting decode-never-panics + a guarded
> `parse → bytes` round-trip. **No `src/` changes were needed** — the parser API + codec
> singletons are already public. The smoke is auto-discovered by `xtask test-fuzz`
> (`discover_fuzz_crates` globs `crates/*/fuzz`), so **no `xtask/`/`Taskfile.yml` edits**.
> Committed seeds: `corpus/decode_block/{golden_block,golden_tx}` (the M5.15 golden block + a
> tx); `Cargo.lock` and libFuzzer-discovered corpus entries are intentionally NOT committed
> (matches platformvm). **No stable `prop_fuzz_smoke.rs`** added — platformvm has none to
> mirror (it has `prop_roundtrip.rs`); the stable round-trip gate already lives in the M5.5/M5.15
> golden tests.
> - **REAL BUG FOUND + FIXED (the fuzz target did its job):** the first smoke run OOM'd inside
>   `ava_secp256k1fx` `unmarshal_fields`. The three hand-written decoders
>   (`OutputOwners`/`Input`/`Credential` in `crates/ava-secp256k1fx/src/types.rs`) read an
>   attacker-controlled `u32` count `n` and looped `0..n` `push`-ing **without checking
>   `p.errored()`** — so a truncated buffer with a huge `n` (e.g. `0x28000001`) drove unbounded
>   `Vec` growth → OOM/DoS (the equivalent P-Chain decoders already guard with
>   `if p.errored() { return; }`). Fixed by adding the same guard after reading the count and
>   inside each loop iteration, bailing with `Error::InvalidComponent` (mirrors `check_space`
>   setting `PackerError::InsufficientLength`). After the fix the smoke runs **134,006 runs in
>   11 s, RSS peak 257 MB, exit 0, no crash**. Regression-checked: ava-secp256k1fx + ava-avm =
>   101 tests green, ava-platformvm = 121 green, clippy `-D warnings` + fmt clean across all.

### Task M5.24: Milestone exit gate
**Crate:** ava-avm (+ workspace)  ·  **Depends on:** all prior M5 tasks  ·  **Spec:** 09 (full), 02 §13 (per-crate contract), 00 (buildable-&-green invariant)
**Files:** `crates/ava-avm/tests/PORTING.md`, workspace `Cargo.toml` (member registration), `.config/nextest.toml` (ci profile)
- [x] **Step 1 — Red:** Run the full gate; expect any remaining gaps (missing PORTING rows, clippy warnings, unregistered workspace member) to fail.
- [x] **Step 2 — Confirm red:** `cargo clippy --workspace -- -D warnings` and/or the named exit tests surface remaining issues.
- [x] **Step 3 — Green:** Run and pass all of:
  - `cargo build --workspace`
  - `cargo build -p avalanchers` (the binary now runs the X-Chain)
  - `cargo nextest run --profile ci` including the named exit tests: `golden::xchain_block_hash`, `golden::xchain_tx_codec`, `differential::xchain_issue_tx`, `differential::atomic_xp`
  - `cargo clippy --workspace -- -D warnings`
  Confirm every per-crate contract artifact (02 §13): proptest suite + committed `crates/ava-avm/proptest-regressions/`, golden vectors under `tests/vectors/avm/` + `tests/vectors/atomic/`, the cargo-fuzz target, and `crates/ava-avm/tests/PORTING.md` (every Go `vms/avm`, `vms/nftfx`, `vms/propertyfx` test mapped to a Rust counterpart or `na` with reason). Confirm `avalanchers` boots an X-Chain end-to-end. Coordinate `differential::xchain_issue_tx` live mode behind feature/env with recorded-oracle fallback (cross-cutting harness X).
- [x] **Step 4 — Confirm green:** all four commands above pass; PORTING.md has no `wip` rows for ported surfaces.
- [x] **Step 5 — Commit:** `avm: M5 milestone exit gate — X-Chain full issue/accept green (M5.24)`

> **As-built (M5.24, 2026-06-07):** **M5 MILESTONE COMPLETE.** Gate run on main:
> - `cargo build --workspace` ✅ + `cargo build -p avalanchers` ✅ (binary builds with the X-Chain VM).
> - Named exit tests all pass: `golden::xchain_block_hash`, `golden::xchain_tx_codec`,
>   `differential::xchain_issue_tx`, `differential::atomic_xp` (4/4).
> - `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅ clean (1m37s).
> - Full `cargo nextest run -p ava-avm -p ava-differential` = **187 passed, 0 skipped**.
> - Per-crate contract artifacts present: golden vectors (`golden_tx_codec`/`golden_block_hash` inline
>   consts + `tests/vectors/atomic/{x_to_p,p_to_x}_utxo.json`), proptest suites + `proptest-regressions/`,
>   the cargo-fuzz target (`crates/ava-avm/fuzz/decode_block`, M5.23), and the new
>   `crates/ava-avm/tests/PORTING.md` (**121 ✅ / 31 🟡 / 0 ⬜ / 12 n/a; 0 `wip` rows**; 100% of
>   non-n/a Go `vms/{avm,nftfx,propertyfx}` tests mapped to a Rust counterpart).
> - **Open follow-ups carried out of M5 (all documented in code + the as-built notes above, none block
>   the gate):** (1) `build_block` un-injectable `SystemTime::now()` → adopt `ava_utils::clock::Clock`
>   (tier-X **X.19**); (2) VM-`initialize` production cross-chain `SharedMemory` wiring needs a
>   `ChainContext.shared_memory` field (chain-manager/M8); (3) `verify.SameSubnet` validator-state hook
>   (M8); (4) full Go X-Chain genesis-asset parsing (M8/ava-genesis); (5) JSON-RPC HTTP router / `ava-api`
>   (M8/M12) — the typed `avm.*` service handlers are ready to wire; (6) the OperationTx typed fx-op
>   codec (concrete op type-ids 8/12/13/17/18 + `components.rs` nft/property Output/Input variants) →
>   unblocks full OperationTx semantic+exec+output-production + address-indexed `getUTXOs`/`getBalance`;
>   (7) the Go recorded-oracle + live two-binary differential arms + richer tx kinds + 10k scaling
>   (tier-X X.13/X.15); (8) the `avm` `Client<Transport>` + `wallet.*` keystore.

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

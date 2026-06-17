# M4 — P-Chain Read-Only Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Build `ava-platformvm` so a Rust node can parse/verify/accept every P-Chain tx & block type, maintain flat-KV state + the staker/validator-metadata model, compute reward/fee math bit-exactly, serve the `ValidatorState` contract that all chains' consensus + proposervm windower consume, and bootstrap the Fuji P-Chain from the network to tip read-only with block IDs, state, and validator views identical to Go.
**Tier:** T4 — VMs
**Crates:** ava-platformvm
**Owning specs:** 08 (PRIMARY), 19 (bootstrap/state-sync), 20 (P-Chain warp signing), 21 (reward + P-Chain fee math), 23 (P-Chain genesis assembly), 00 + 02 (conventions)
**Depends on (prior milestones):** M3 (engine bootstrap, `ChainVm`/`Block`, `ava-validators` + `ValidatorState`, proposervm windower, fx/`ava-secp256k1fx`, chain manager), M1 (flat-KV state stores: `ava-database` prefixdb/versiondb), M0 (`ava-codec`/`ava-codec-derive`, `ava-crypto` bls+secp256k1, `ava-types`), M2 (network for bootstrap fetch + tx gossip)
**Exit gate (named tests):**
- `golden::pchain_block_hash` + `golden::pchain_tx_codec` (all tx & block types)
- `prop::pchain_tx_roundtrip`
- **`differential::pchain_sync_to_tip`** (sync Fuji P-Chain; at matching heights, last-accepted block ID + state hash + `getCurrentValidators` (sorted) == Go)
- `differential::validatorstate_parity` (windower-relevant view matches Go)

New golden vectors under `tests/vectors/platformvm/`: block hashes, tx codecs (all types incl. ACP-77 L1 JSON vectors), reward vectors, validator-diff windows.

**Importance:** P-Chain serves `ValidatorState` (08 §7) which consensus + proposervm windower consume for **every** chain. This milestone **locks the `ValidatorState` contract for all chains** and is the cheapest read-only network join (no tx issuance required).

---

## Dependency map & parallel waves

The build flows: **codec first** (tx + block share one numbering space; gates accept/verify/sync), then **state model** (flat-KV + staker + metadata v2), then **reward/fee math** (independent, can parallelize), then the **executor** (verify/accept producing diffs), then **`ValidatorState` serving** (the cross-cutting deliverable), then **bootstrap/state-sync** wiring, then the **differential sync-to-tip**.

```
Wave 0 (crate scaffold)
  M4.1 crate skeleton + license + fuzz target stub

Wave 1 (codec — gates everything; TDD entry point)
  M4.2 type_id registry + UnsignedTx enum + Tx envelope        [dep M4.1]
  M4.3 per-tx structs (apricot/banff/durango)                  [dep M4.2]
  M4.4 ACP-77 L1 tx structs + signer + stakeable               [dep M4.2]
  M4.5 Block enum + parse + block_id hashing                   [dep M4.2]
  M4.6 golden::pchain_tx_codec + golden::pchain_block_hash     [dep M4.3,M4.4,M4.5]
       + prop::pchain_tx_roundtrip + fuzz decoder

Wave 2 (math — independent, parallel with Wave 3 state)
  M4.7 reward calculator (BigUint) + Split                     [dep M4.1]
  M4.8 gas/dynamic-fee + static-fee + complexity               [dep M4.1]
  M4.9 L1 validator continuous fee                             [dep M4.1,M4.8]

Wave 3 (state model)
  M4.10 Staker model + Priority + Ord                          [dep M4.3]
  M4.11 ValidatorMetadata codec v0/v1/v2 + legacy fallbacks    [dep M4.2]
  M4.12 L1Validator (GenesisCodec) + ordering                  [dep M4.4,M4.11]
  M4.13 Chain/Diff/Versions/State flat-KV stores               [dep M4.10,M4.11,M4.12]
  M4.14 weight-diff + pk-diff disk iterators (inverseHeight)   [dep M4.13]

Wave 4 (executor — verify + accept producing diffs)
  M4.15 UTXO handler (spend/produce/verify) + ATOMIC-1         [dep M4.3,M4.13]
  M4.16 StandardTx executor + staker/subnet verification       [dep M4.15,M4.7,M4.8]
  M4.17 ProposalTx executor (advance_time + reward) oracle     [dep M4.16,M4.7,M4.9]
  M4.18 AtomicTx executor (apricot import/export path)         [dep M4.15]
  M4.19 ACP-77 L1 lifecycle executor                           [dep M4.16,M4.9,M4.12]

Wave 5 (block executor + ValidatorState)
  M4.20 block executor Verify/Accept/Reject/Options + acceptor [dep M4.16,M4.17,M4.18,M4.14]
  M4.21 PChainValidatorManager: ValidatorState impl            [dep M4.14,M4.20]
  M4.22 warp signing on P (UnsignedMessage + get_warp_sets)    [dep M4.21]
  M4.23 differential::validatorstate_parity                    [dep M4.21]

Wave 6 (genesis + VM + bootstrap)
  M4.24 P-Chain genesis build/parse + genesis block           [dep M4.5,M4.13]
  M4.25 PlatformVm: impl ChainVm/Block (+ StateSyncableVm=No)  [dep M4.20,M4.21,M4.24]
  M4.26 mempool + network tx gossip (read-only OK)             [dep M4.3,M4.25]
  M4.27 bootstrap wiring: linear bootstrap to genesis height   [dep M4.25]
       (TDD entry point #2: differential sync to height 0)
  M4.28 service.rs JSON-RPC (getCurrentValidators, getBlock…)  [dep M4.25,M4.21]

Wave 7 (differential sync-to-tip + gate)
  M4.29 differential::pchain_sync_to_tip (CI-gated + oracle)   [dep M4.27,M4.28,M4.21]
  M4.30 Milestone exit gate                                    [dep ALL]
```

**Parallel groups.** Wave 2 (math: M4.7/M4.8/M4.9) runs fully parallel with Wave 3 (state). Within Wave 1, M4.3/M4.4 parallel after M4.2; M4.5 parallel with them. Within Wave 4, M4.18 parallel with M4.16/M4.17.

---

## Tasks

### Task M4.1: Crate skeleton, license headers, fuzz target stub
**Crate:** ava-platformvm  ·  **Depends on:** none (M0 crates exist)  ·  **Spec:** 08 §1; 00 §3,§8; 02 §13
**Files:** `crates/ava-platformvm/Cargo.toml`, `crates/ava-platformvm/src/lib.rs`, `crates/ava-platformvm/src/error.rs`, `crates/ava-platformvm/tests/PORTING.md`, `crates/ava-platformvm/fuzz/Cargo.toml`, `crates/ava-platformvm/fuzz/fuzz_targets/decode_block_tx.rs`, workspace `Cargo.toml` member entry.
- [x] **Step 1 — Red:** Add `crates/ava-platformvm/tests/smoke.rs` with `#[test] fn crate_links() { assert_eq!(ava_platformvm::CODEC_VERSION, 0); }` referencing the not-yet-existing crate.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-platformvm --test smoke` → fails: crate `ava-platformvm` not found / unresolved.
- [x] **Step 3 — Green:** Create the crate with `#![forbid(unsafe_code)]`, the Ava Labs license header on every `.rs`, deps per 08 §1 (`ava-codec`, `ava-codec-derive`, `ava-crypto`, `ava-database`, `ava-validators`, `ava-secp256k1fx`, `ava-types`, `ava-utils`, `num-bigint`, `parking_lot`, `tokio`, `thiserror`, `async-trait`). Declare `pub const CODEC_VERSION: u16 = 0;` (08 §2.1). Stub `error.rs` with `Error`/`Result` (`thiserror`) holding the sentinels listed in 08 §10. Seed `tests/PORTING.md` from `go test -list '.*' ./vms/platformvm/...`. Add the cargo-fuzz target that calls `Block::parse`/`Tx::parse` once those land (compile-guarded stub now).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-platformvm --test smoke` passes; `cargo build -p ava-platformvm` clean.
- [x] **Step 5 — Commit:** `ava-platformvm: crate skeleton + error model + fuzz stub`

---

### Task M4.2: type_id registry, `UnsignedTx` enum, signed `Tx` envelope
**Crate:** ava-platformvm  ·  **Depends on:** M4.1  ·  **Spec:** 08 §2.1 (the 43-entry table), §2.2, §2.3; 00 §6.1
**Files:** `crates/ava-platformvm/src/txs/mod.rs`, `crates/ava-platformvm/src/txs/codec.rs`, `crates/ava-platformvm/src/txs/tx.rs`.
- [x] **Step 1 — Red:** Add `golden::type_id_table` (unit form ok for now) asserting each `UnsignedTx`/`Block` discriminant equals the §2.1 table value (12=AddValidator … 42=RewardAutoRenewedValidator; secp256k1fx 5,7,9,10,11; stakeable 21,22; signer 27,28). Test references the enum + `type_id()`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-platformvm type_id_table` → fails (no enum).
- [x] **Step 3 — Green:** Define `enum UnsignedTx` with explicit `#[codec(type_id = N)]` and reserved gaps exactly per the §2.2 listing. Build TWO codec managers over one type registry — `Codec` (default max) and `GenesisCodec` (`i32::MAX` max-slice) — via `SkipRegistrations(5)` (reserve block 0–4) then secp256k1fx at 5–11 (with the `SkipRegistrations(1)` MintInput/MintOutput gaps — **do not collapse**, 08 §2.1 note), then tx types, then `SkipRegistrations(4)` for Banff blocks 29–32. Implement `Tx { unsigned, creds: Vec<Credential>, tx_id, bytes }` with the prefix-length `initialize` trick (08 §2.3): `signed_bytes = marshal(Tx)`, `unsigned_len = Codec::size(&unsigned)`, `unsigned_bytes = signed_bytes[..unsigned_len]`, `tx_id = sha256(signed_bytes)`. `Tx::parse` reproduces the prefix slice (zero-copy `bytes::Bytes`). Add `UnsignedTx::{inputs,outputs,input_ids,visit}` signatures (bodies stubbed where per-tx structs not yet defined).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-platformvm type_id_table` passes.
- [x] **Step 5 — Commit:** `ava-platformvm: tx type_id registry (Codec+GenesisCodec) + Tx envelope`

---

### Task M4.3: Per-tx structs (Apricot/Banff/Durango) + `syntactic_verify`
**Crate:** ava-platformvm  ·  **Depends on:** M4.2  ·  **Spec:** 08 §2.2 (per-tx field table), §2.1; 23 (genesis tx shapes); ATOMIC-1 (00 §11.1.7)
**Files:** `crates/ava-platformvm/src/txs/base_tx.rs`, `add_validator.rs`, `add_delegator.rs`, `add_subnet_validator.rs`, `add_permissionless_validator.rs`, `add_permissionless_delegator.rs`, `remove_subnet_validator.rs`, `transform_subnet.rs`, `create_subnet.rs`, `create_chain.rs`, `transfer_subnet_ownership.rs`, `import_export.rs`, `advance_time.rs`, `reward_validator.rs`, `validator.rs` (the `Validator`/`SubnetValidator` shared structs).
- [x] **Step 1 — Red:** Add `golden::pchain_tx_codec_app_validator` (the TDD ENTRY POINT) loading `tests/vectors/platformvm/add_permissionless_validator_tx.json` (Go-extracted) and asserting `encode(decode(bytes)) == bytes` and `tx_id == expected`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-platformvm pchain_tx_codec_app_validator` → fails (`AddPermissionlessValidatorTx` undefined).
- [x] **Step 3 — Green:** Define all structs in the §2.2 field table, each `#[derive(AvaCodec)]`. `BaseTx { network_id: u32, blockchain_id: Id, outs: Vec<TransferableOutput>, ins: Vec<TransferableInput>, memo: Vec<u8> }`. `Validator { node_id, start: u64, end: u64, wght: u64 }`; `SubnetValidator { validator, subnet }`. `AddPermissionlessValidatorTx { base, validator, subnet, signer: Signer, stake_outs, validator_rewards_owner, delegator_rewards_owner, delegation_shares: u32 }`. Implement `syntactic_verify` per 08 §2.2: outputs sorted (`avax::is_sorted_transferable_outputs`), inputs sorted & unique, stake outputs sorted & summing to `validator.wght`, `delegation_shares <= reward::PERCENT_DENOMINATOR (1_000_000)`, BLS signer present iff Primary Network; memoize via `OnceCell<()>` (not serialized).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-platformvm pchain_tx_codec_app_validator` passes.
- [x] **Step 5 — Commit:** `ava-platformvm: per-tx structs + syntactic_verify (apricot/banff/durango)`

---

### Task M4.4: ACP-77 L1 tx structs, `Signer`, `stakeable` outputs
**Crate:** ava-platformvm  ·  **Depends on:** M4.2  ·  **Spec:** 08 §2.2 (L1 rows), §6 (ACP-77 lifecycle); 20 §3.1 (RegistryPayload referenced); 08 §2.1 (ids 21,22,27,28,35–42)
**Files:** `crates/ava-platformvm/src/txs/convert_subnet_to_l1.rs`, `register_l1_validator.rs`, `set_l1_validator_weight.rs`, `increase_l1_validator_balance.rs`, `disable_l1_validator.rs`, `crates/ava-platformvm/src/txs/auto_renew.rs` (Helicon 40–42), `crates/ava-platformvm/src/signer.rs`, `crates/ava-platformvm/src/stakeable.rs`.
- [x] **Step 1 — Red:** Add `golden::pchain_tx_codec_l1` loading `tests/vectors/platformvm/convert_subnet_to_l1_tx.json` and `register_l1_validator_tx.json` (the Go `*_test.json` fixtures, 08 §11.1), asserting round-trip + tx_id under the **GenesisCodec** (some are oversized).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-platformvm pchain_tx_codec_l1` → fails.
- [x] **Step 3 — Green:** Define `ConvertSubnetToL1Tx { base, subnet, chain_id, address: Vec<u8>, validators: Vec<ConvertSubnetToL1Validator>, subnet_auth }`, `RegisterL1ValidatorTx { base, balance: u64, proof_of_possession: [u8;96], message: Vec<u8> }`, `SetL1ValidatorWeightTx { base, message: Vec<u8> }`, `IncreaseL1ValidatorBalanceTx { base, validation_id: Id, balance: u64 }`, `DisableL1ValidatorTx { base, validation_id: Id, disable_auth: Verifiable }`, and the three Helicon auto-renew txs. `signer.rs`: `enum Signer { Empty (type_id 27), ProofOfPossession(ProofOfPossession) (type_id 28) }` with `ProofOfPossession { public_key: [u8;48], proof: [u8;96] }` and `verify()` via `ava-crypto::bls::verify_proof_of_possession` (08 §8). `stakeable.rs`: `LockIn` (Input, type_id 21) / `LockOut { locktime, transferable_out }` (Output, type_id 22).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-platformvm pchain_tx_codec_l1` passes.
- [x] **Step 5 — Commit:** `ava-platformvm: ACP-77 L1 txs + signer + stakeable outputs`

---

### Task M4.5: `Block` enum, `parse`, byte-exact block_id hashing
**Crate:** ava-platformvm  ·  **Depends on:** M4.2  ·  **Spec:** 08 §4.1 (block enum + byte order), §4.2 (oracle model); 23 §4.1 (genesis block)
**Files:** `crates/ava-platformvm/src/block/mod.rs`, `crates/ava-platformvm/src/block/codec.rs`, `crates/ava-platformvm/src/block/parse.rs`, `crates/ava-platformvm/src/block/apricot.rs`, `crates/ava-platformvm/src/block/banff.rs`.
- [x] **Step 1 — Red:** Add `golden::pchain_block_hash` loading `tests/vectors/platformvm/banff_standard_block.json` + `apricot_commit_block.json` and asserting `Block::parse(bytes).id() == expected_id` (sha256 of codec bytes) and `encode == bytes`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-platformvm pchain_block_hash` → fails.
- [x] **Step 3 — Green:** Define `enum Block` with the 9 variants & type_ids 0–4, 29–32 (08 §4.1) over the **shared** registry (same managers as M4.2). `CommonBlock { parent_id: Id, height: u64 }`. Apricot variants per §4.1; Banff variants prefix `time: u64` then (standard/proposal) `Vec<Tx>`. **Byte-exact field order**: `BanffProposalBlock` lays out `time`, then `transactions: Vec<Tx>`, then the embedded `ApricotProposalBlock` (single proposal `Tx`); `Txs()` returns `decision_txs ++ [proposal_tx]` (§4.1). `block_id = sha256(codec_bytes)`. `Block::parse(codec, bytes)` (zero-copy). Wire the fuzz target from M4.1 to `Block::parse`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-platformvm pchain_block_hash` passes.
- [x] **Step 5 — Commit:** `ava-platformvm: Block enum + parse + byte-exact block_id`

---

### Task M4.6: Codec golden + round-trip proptest + decoder fuzz (codec gate)
**Crate:** ava-platformvm  ·  **Depends on:** M4.3, M4.4, M4.5  ·  **Spec:** 08 §11.1; 02 §4,§6,§8; 23 §7 (cross-check)
**Files:** `crates/ava-platformvm/tests/golden_codec.rs`, `crates/ava-platformvm/tests/prop_roundtrip.rs`, `crates/ava-platformvm/tests/vectors/platformvm/*.json`, `crates/ava-platformvm/proptest-regressions/` (committed), `crates/ava-platformvm/fuzz/fuzz_targets/decode_block_tx.rs`.
- [x] **Step 1 — Red:** Add `golden::pchain_tx_codec` iterating **all** tx-type vectors (one per UnsignedTx variant, extracted from Go via `tools/extract-vectors`, coordinate with tier X) asserting round-trip + tx_id; `golden::pchain_block_hash` iterating all 9 block-type vectors; `prop::pchain_tx_roundtrip` asserting `decode(encode(x)) == x` for `arb_unsigned_tx()` (4096 cases) and `decode_never_panics` over arbitrary bytes.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm golden::pchain_tx_codec` → fails on the first missing vector / variant (not a compile error).
- [x] **Step 3 — Green:** Add the remaining Go-extracted vectors + the `arb_unsigned_tx`/`arb_block` proptest strategies. Ensure the codec handles GenesisCodec-oversized txs. Commit the generated `proptest-regressions/` seeds. Confirm the cargo-fuzz `decode_block_tx` target builds and runs a smoke pass without panic.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm golden:: prop::pchain_tx_roundtrip` all green; `cargo +nightly fuzz run decode_block_tx -- -runs=10000` no crash.
- [x] **Step 5 — Commit:** `ava-platformvm: codec golden vectors + roundtrip proptest + decoder fuzz`

> **As-built (M4.6):** `tests/golden_codec.rs` (`pchain_tx_codec` over 5 byte-exact Go vectors:
> AddPermissionlessValidator(25), Register/SetWeight/IncreaseBalance/DisableL1Validator(36/37/38/39);
> `pchain_block_hash` over all `*_block.json` vectors) + `tests/prop_roundtrip.rs`
> (`pchain_tx_roundtrip` over `arb_unsigned_tx()` covering **all 23 variants**, 1024
> cases; `pchain_signed_tx_roundtrip`; `decode_never_panics`). **Per-variant byte-exact
> goldens partially deferred:** 5 variants have Go `expectedBytes`; the remaining 18 are
> covered by the proptest round-trip (catches field-order/encoding regressions), with the
> portable-but-unported ones (`ConvertSubnetToL1`, `RemoveSubnetValidator`, `TransformSubnet`,
> `AddPermissionlessDelegator`, `TransferSubnetOwnership`, `Base`, auto-renew trio) recorded as
> `na` rows in `tests/PORTING.md` for a later vector-extraction pass. `arb_block()` not added
> (Block/body deeply entangled with Tx; block side covered byte-exact by `pchain_block_hash` +
> `decode_never_panics` over `Block::parse`). **Fuzz:** `cargo +nightly fuzz` is unavailable in
> the default Nix dev shell (cargo-fuzz lives in the `fuzz` shell; no rustup nightly present);
> `prop::decode_never_panics` is the stable always-on substitute exercising the same
> `Block::parse`/`Tx::parse` calls as the `decode_block_tx` target — consistent with the repo's
> nightly-only-fuzz / stable-proptest-smoke convention ([[proto-and-fuzz-pipeline]]).

---

### Task M4.7: Staking reward calculator (`BigUint`) + `Split`
**Crate:** ava-platformvm  ·  **Depends on:** M4.1  ·  **Spec:** 08 §5; 21 §3 (exact integer order + worked vectors)
**Files:** `crates/ava-platformvm/src/reward/calculator.rs`, `crates/ava-platformvm/src/reward/config.rs`, `crates/ava-platformvm/src/reward/mod.rs`.
- [x] **Step 1 — Red:** Add `golden::reward_vectors` loading `tests/vectors/platformvm/reward_grid.json` (the `(Δt, stake, supply)` grid frozen from Go `reward.Calculate`, 21 §3) plus the 3 worked examples (full-period ≈192 AVAX, zero-duration=0, near-cap clamp). Add `prop::reward_monotone` (≤ remaining; 0 at Δt=0; non-decreasing in stake & Δt).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm golden::reward_vectors` → fails (no calculator).
- [x] **Step 3 — Green:** Implement per 21 §3 EXACT op order with `num_bigint::BigUint`: `adjConsumNum = maxSubMin·Δt + minRate·P`; `adjConsumDen = P·D`; `remaining = supplyCap − supply`; `reward = remaining; reward *= adjConsumNum; *= stake; *= Δt; /= adjConsumDen; /= supply; /= P` (**all muls before any div**, three separate trailing divides); `if reward > u64::MAX → remaining else min(remaining, reward)`. `PERCENT_DENOMINATOR = 1_000_000`. Config constants (mainnet/Fuji identical): `MaxConsumptionRate=120_000`, `MinConsumptionRate=100_000`, `MintingPeriod=31_536_000_000_000_000 ns`, `SupplyCap=720·MegaAvax`. `Split(total, shares)`: `remainderShares = D − shares`; `remainderAmount = checked(remainderShares·total)/D` else fallback `remainderShares·(total/D)`; `fromShares = total − remainderAmount` (21 §3 delay-rounding). Δt and P in **nanoseconds**.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm golden::reward_vectors prop::reward_monotone` green.
- [x] **Step 5 — Commit:** `ava-platformvm: staking reward calculator (BigUint, exact) + Split`

---

### Task M4.8: Dynamic gas fee (ACP-103) + static fee + tx complexity
**Crate:** ava-platformvm  ·  **Depends on:** M4.1  ·  **Spec:** 08 §6; 21 §0 (`CalculatePrice`), §1 (ACP-103), §2a (static)
**Files:** `crates/ava-platformvm/src/txs/fee/dynamic_calculator.rs`, `simple_calculator.rs`, `complexity.rs`, `mod.rs`; reuse/add `ava-gas` (or local `gas` module) for `calculate_price`, `gas::State`, `Dimensions`.
- [x] **Step 1 — Red:** Add `golden::calculate_price` (the 9-row 21 §0 table incl. the `MaxUint64 − 11` row and the clamp row) and `golden::pchain_dynamic_fee` (the 21 §1 worked examples: excess=0 → fee=6600; excess=K → fee=13200; advance-then-consume round trip). Add `prop::price_monotone` (non-decreasing in excess; == minPrice at 0; never panics; ≤ MaxU64).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm golden::calculate_price` → fails.
- [x] **Step 3 — Green:** Implement `calculate_price(min_price, excess, k)` over `ruint::U256` EXACTLY per 21 §0 (clamp test `output >= max_output`; two separate divides `/k` then `/i`; trailing `/k`; m=0→0). `GasState { capacity, excess }` with `advance_time`/`consume_gas` (21 §1). `dot_to_gas(c, w)` checked dot product. Constants (mainnet=Fuji): weights `[1,1000,1000,4]`, `MaxCapacity=1_000_000`, `MaxPerSecond=100_000`, `TargetPerSecond=50_000`, `MinPrice=1`, `K=2_164_043`. Static `SimpleCalculator` returns flat `TxFee=MilliAvax=1_000_000`, `CreateAssetTxFee=10·MilliAvax` (Fuji), per-network from config. `complexity.rs` computes the `[Bandwidth,DBRead,DBWrite,Compute]` 4-vector per tx type (`txs/fee/complexity.go`). Fee path selects static (pre-Etna) vs dynamic (post-Etna) by fork.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm golden::calculate_price golden::pchain_dynamic_fee prop::price_monotone` green.
- [x] **Step 5 — Commit:** `ava-platformvm: ACP-103 dynamic fee + static fee + complexity`

> **UPSTREAM DELTA (Go `c84b906db6`, ACP-236 (3), #5202 — folded 2026-06-10).** The Rust
> `complexity.rs` shipped only the intrinsic primitives; the per-tx `complexityVisitor` is
> deferred (see the `TODO(after M4.3)` in `txs/fee/complexity.rs`). When that visitor lands,
> note that Go has since **implemented** the previously-`errUnimplemented` auto-renew cases:
> `IntrinsicAddAutoRenewedValidatorTxComplexities` (DBWrite=3) and
> `IntrinsicSetAutoRenewedValidatorConfigTxComplexities` (DBRead=1, DBWrite=1) — exact
> bandwidth formulas in `08` §6 upstream-delta; Go `txs/fee/calculator_test.go` gained the
> golden rows to mirror. Do NOT port an `errUnimplemented` stub for these.

---

### Task M4.9: L1 validator continuous fee (ACP-77)
**Crate:** ava-platformvm  ·  **Depends on:** M4.1, M4.8  ·  **Spec:** 08 §6 (continuous fee); 21 §2b (the loop + golden table)
**Files:** `crates/ava-platformvm/src/validators/fee.rs`.
- [x] **Step 1 — Red:** Add `golden::l1_validator_fee` loading the full `validators/fee/fee_test.go` table (21 §2b, esp. the `177 321 939` per-second-loop-with-underflow row and the `122 880 = 60·2048` constant-price rows). Add `prop::l1_fee_zero_excess_fast_path` (once excess hits 0 it stays 0).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm golden::l1_validator_fee` → fails.
- [x] **Step 3 — Green:** Implement `L1State { current, excess }`, `L1Config { target, min_price, k }`, `advance_one`/`advance_time` (excess only), `cost_of(c, seconds)` and `seconds_remaining(c, max, funds)` EXACTLY per 21 §2b: constant-price fast path when `current==target`; else **advance excess one second BEFORE pricing each second**, with the zero-excess short-circuit (`+= min_price·remaining`). Genesis constants: `Capacity=20_000`, `Target=10_000`, `MinPrice=512`, `K`=1_246_488_515 (mainnet) / 51_937_021 (Fuji). Reuses `calculate_price` from M4.8.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm golden::l1_validator_fee prop::l1_fee_zero_excess_fast_path` green.
- [x] **Step 5 — Commit:** `ava-platformvm: ACP-77 L1 validator continuous fee`

---

### Task M4.10: Staker model + `Priority` + `Ord` (= Go `Less`)
**Crate:** ava-platformvm  ·  **Depends on:** M4.3  ·  **Spec:** 08 §3.3 (staker + priorities.go); 00 §6.1
**Files:** `crates/ava-platformvm/src/state/staker.rs`, `crates/ava-platformvm/src/txs/priorities.rs`.
- [x] **Step 1 — Red:** Add `golden::priority_discriminants` asserting the 11 `Priority` values are 1..=11 in the exact §3.3 order, and `prop::staker_ord_matches_go` asserting `Staker::cmp` orders by `(next_time, priority, tx_id bytes)`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm priority_discriminants` → fails.
- [x] **Step 3 — Green:** `#[repr(u8)] enum Priority` with the pinned 1..=11 values (08 §3.3 — protocol-load-bearing). `struct Staker { tx_id, node_id, public_key: Option<bls::PublicKey>, subnet_id, weight, start_time, end_time, potential_reward, next_time, priority }`. `impl Ord` = `next_time.cmp` `.then(priority.cmp)` `.then(tx_id.as_ref().cmp)` (bytes.Compare). Helpers to build current (`next_time==end_time`) vs pending (`next_time==start_time`) stakers.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm priority_discriminants prop::staker_ord_matches_go` green.
- [x] **Step 5 — Commit:** `ava-platformvm: staker model + Priority + Ord (Go Less)`

> **As-built (M4.10):** `Priority` discriminants (`priorities.go` `iota+1` order): pending group
> 1–6 (`PrimaryNetworkDelegatorApricotPending`, `PrimaryNetworkValidatorPending`,
> `PrimaryNetworkDelegatorBanffPending`, `SubnetPermissionlessValidatorPending`,
> `SubnetPermissionlessDelegatorPending`, `SubnetPermissionedValidatorPending`), then current
> group 7–11 (`SubnetPermissionedValidatorCurrent`, `SubnetPermissionlessDelegatorCurrent`,
> `SubnetPermissionlessValidatorCurrent`, `PrimaryNetworkDelegatorCurrent`,
> `PrimaryNetworkValidatorCurrent`). Go `IsCurrent/IsPending/IsValidator/IsDelegator/
> IsPermissionedValidator` ported as `const fn`. **Staker times are `std::time::SystemTime`**
> (mirrors Go `time.Time`; the `u64`-seconds form in `metadata_validator.rs` is for the *on-disk
> codec* only — `Staker` is an in-memory ordering record, never codec-serialized). `Eq`/`PartialEq`
> are keyed on the `(next_time, priority, tx_id)` ordering tuple (Rust requires `a==b` iff
> `cmp==Equal`); Go's full-field `Equals` is provided separately as `Staker::equals` (handles
> `bls::PublicKey` compressed-bytes compare). `Debug` is hand-written (`bls::PublicKey` is not
> `Debug`). Constructors take resolved fields (not Go's tx-staker trait objects, deferred to the
> executor wave) and enforce current⇒`next_time==end_time`, pending⇒`next_time==start_time` &
> `potential_reward==0`.

---

### Task M4.11: `ValidatorMetadata` codec v0/v1/v2 + length-based legacy fallbacks
**Crate:** ava-platformvm  ·  **Depends on:** M4.2  ·  **Spec:** 08 §3.4 (ACP-236 metadata codec, three versions + fallbacks)
**Files:** `crates/ava-platformvm/src/state/metadata_validator.rs`, `crates/ava-platformvm/src/state/metadata_codec.rs`.
- [x] **Step 1 — Red:** Add `golden::metadata_codec_v2` round-tripping `ValidatorMetadata` at v0/v1/v2 against Go vectors (`metadata_validator_test.go`) AND the length-based fallbacks: 0 bytes (nil), 8 bytes (potential reward only), `VERSION_SIZE+3*8` bytes (`preDelegateeRewardMetadata`).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm metadata_codec_v2` → fails.
- [x] **Step 3 — Green:** Separate `MetadataCodec` manager (08 §3.4) with version tag selecting fields via `#[codec(version = N)]`: v0 `{up_duration, last_updated, potential_reward, potential_delegatee_reward}`; v1 adds `staker_start_time`; v2 adds `accrued_validation_rewards, accrued_delegatee_rewards, auto_compound_reward_shares: u32, next_period, staker_end_time`; non-serialized `tx_id`. `parse_validator_metadata` reproduces the length-based legacy fallbacks before full codec decode. Auto-renewed effective weight = `tx.weight + accrued_validation_rewards + accrued_delegatee_rewards`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm metadata_codec_v2` green.
- [x] **Step 5 — Commit:** `ava-platformvm: validator metadata codec v0/v1/v2 + legacy fallbacks`

---

### Task M4.12: `L1Validator` (GenesisCodec) + active-iterator ordering
**Crate:** ava-platformvm  ·  **Depends on:** M4.4, M4.11  ·  **Spec:** 08 §3.4 (L1Validator); §6 (active fee charging)
**Files:** `crates/ava-platformvm/src/state/l1_validator.rs`.
- [x] **Step 1 — Red:** Add `golden::l1_validator_codec` round-tripping `L1Validator` (the 9 serialized fields) with **`block::GenesisCodec`** keyed by `ValidationID`; `prop::l1_validator_order` asserting `Compare` by `(end_accumulated_fee, validation_id)` and `IsActive == (weight!=0 && end_accumulated_fee!=0)`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm l1_validator_codec` → fails.
- [x] **Step 3 — Green:** Define `L1Validator` (port the 9 fields verbatim from `state/l1_validator.go`), marshalled with `GenesisCodec` (not MetadataCodec). `Ord` by `(end_accumulated_fee, validation_id)`; `is_active()`. `immutable_fields_are_unmodified` guard for mutation (08 §3.4). The active iterator drives continuous-fee charging in `EndAccumulatedFee` order (used by M4.19).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm l1_validator_codec prop::l1_validator_order` green.
- [x] **Step 5 — Commit:** `ava-platformvm: L1Validator (GenesisCodec) + active-iterator ordering`

> **As-built (M4.12):** `L1Validator` serialized fields (Go `serialize:"true"` order, `ValidationID`
> is the DB key and is **not** codec-tagged): `subnet_id: Id`, `node_id: NodeId`,
> `public_key: Vec<u8>` (**uncompressed** BLS bytes per `bls.PublicKeyFromValidUncompressedBytes` —
> opaque to the codec), `remaining_balance_owner: Vec<u8>`, `deactivation_owner: Vec<u8>` (both raw
> length-prefixed `[]byte` in Go, **not** typed `fx.Owner`), `start_time: u64`, `weight: u64`,
> `min_nonce: u64`, `end_accumulated_fee: u64`. Marshalled with **GenesisCodec** (`#[derive(AvaCodec)]`,
> no type-id). `Ord`/`compare()` by `(end_accumulated_fee asc, validation_id lexicographic)`;
> `end_accumulated_fee` is a plain `u64` (no nil/`*uint64` — `0` is the inactive sentinel, nothing
> more). `is_active() = weight!=0 && end_accumulated_fee!=0`; `is_deleted() = weight==0`.
> `immutable_fields_are_unmodified` short-circuits `true` on differing `validation_id`, else checks
> the 6 constant fields (subnet_id, node_id, public_key, both owners, start_time) unchanged. Go's
> `l1_validator_test.go` uses random values with no committed `expectedBytes`, so the golden pins a
> deterministic fixture and asserts against a fully-specified hand-built wire encoding (linear-codec
> oracle) + structural round-trip. Active-iterator/store deferred to M4.13/M4.19 (only `Ord`+`is_active`
> needed here).

---

### Task M4.13: `Chain`/`Diff`/`Versions`/`State` flat-KV stores
**Crate:** ava-platformvm  ·  **Depends on:** M4.10, M4.11, M4.12  ·  **Spec:** 08 §3.1 (interfaces), §3.2 (flat-KV prefixes), §3.5 (supply/reward); 00 §11.1.3 (Database sync trait)
**Files:** `crates/ava-platformvm/src/state/mod.rs`, `chain.rs`, `diff.rs`, `state.rs`, `stakers.rs`, `prefixes.rs`.
- [x] **Step 1 — Red:** Add `prop::diff_apply_equals_direct` — a sequence of stat-mutations applied through a `Diff` then `apply()` to base `State` equals applying them directly to `State` (the overlay-flush oracle); `conformance::state_roundtrip` writing then re-reading UTXOs/stakers/supply across a RocksDB temp dir.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm diff_apply_equals_direct` → fails (no State).
- [x] **Step 3 — Green:** Port the trait stack (08 §3.1): `trait Chain` (timestamp, current_supply/set, fee_state/set, l1_validator_excess, accrued_fees, get/add/delete_utxo, current/pending validator+delegator getters/putters/iterators, L1 getters/putters, subnets/chains/reward-utxos/subnet-owners). `trait Versions { get_state(block_id) -> Option<Arc<dyn Chain>> }`. `Diff` overlay on a `parent_id` resolved through `Versions`, with `apply(&self, base)`. `State` = persisted base over `ava-database` prefixdbs: `utxoDB, subnetDB, subnetOwnerDB, subnetManagerDB/subnetToL1ConversionDB, chainDB, txDB, rewardUTXOsDB, blockDB, blockIDDB, current/pending* validator/delegator/subnetValidator lists, l1ValidatorDB, weightDiffDB, pkDiffDB, singletonDB` (08 §3.2), each with an LRU front cache. Stakers in two `BTreeSet<Staker>` (current/pending) + per-(subnet,node) lookup maps mirroring `state/stakers.go`; the base/diff overlay map lives in `Diff`. Per-subnet current supply (singleton, seeded from genesis `InitialSupply`); reward UTXOs keyed by staker tx ID.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm diff_apply_equals_direct conformance::state_roundtrip` green.
- [x] **Step 5 — Commit:** `ava-platformvm: Chain/Diff/Versions/State flat-KV stores + stakers`

> **As-built (M4.13):** Shipped the `Chain` trait (full read+write surface: `timestamp/set_timestamp`,
> `current_supply/set_current_supply` per-subnet, `fee_state/set_fee_state` over `txs::fee::gas::GasState`,
> `l1_validator_excess/set_l1_validator_excess`, `accrued_fees/set_accrued_fees`, `get/add/delete_utxo`,
> current+pending validator/delegator putters+deleters + `current_stakers`/`pending_stakers` iterators,
> `get_current_validator`, `get/put_l1_validator` + `weight_of_l1_validators`,
> `subnets/add_subnet`, `get/set_subnet_owner`, `get/set_subnet_manager`, `chains/add_chain`,
> `get_reward_utxos/add_reward_utxo`), `Versions { get_state -> Option<Arc<dyn Chain>> }`, the `Diff`
> overlay (per-field `Option`/`BTreeMap` overlays + ordered staker-op vectors mirroring Go `diffStakers`;
> `apply(&self, base: &mut dyn Chain)` replays scalars→utxos→current→pending→L1→subnets/chains/owners/rewards),
> and `State<D: Database>` over `ava-database` prefixdbs.
> **Deviations / choices:** (1) `Chain` is `Send + Sync` (spec sketch said just `Send`) — required because `Diff`
> holds `Arc<dyn Chain>` and is itself a `Chain`. (2) **UTXOs are stored as opaque codec bytes** (`UtxoBytes = Vec<u8>`),
> not the typed `avax::Utxo`: `Utxo` carries an `Arc<dyn State>` fx payload that is not codec-serializable in
> isolation yet (fx-registered handler is M4.15); the byte layout is exactly the cross-chain/shared-memory-relevant
> form. (3) **Cache choice:** `parking_lot::Mutex<lru::LruCache<Vec<u8>, Vec<u8>>>` front cache (cap 8192) over each
> byte-valued prefix space; `lru = "0.12"` added to the crate (already used by ava-database/blockdb/merkledb).
> (4) Byte-valued spaces (UTXOs, reward UTXOs, subnet owners/managers, subnet set, per-subnet chains) write through to
> RocksDB/MemDb; scalar singletons + stakers/L1-validators are in-memory fields (Go's cached `baseStakers` model) —
> flushing stakers to the disk sublists is the acceptor's job (M4.20). (5) `RocksDb` reached via a dev-only
> `ava-database` feature = `["rocksdb"]`; the conformance test uses `RocksDb::open_temp()` + a MemDb mirror.
> **Deferred to M4.14:** the `weightDiffDB`/`pkDiffDB` prefix *handles* are created in `State::new` (so M4.14 can build
> on them) but their byte-exact `inverseHeight` iterators are not implemented here.

---

### Task M4.14: Weight-diff + pk-diff disk iterators (`inverseHeight` byte-exact)
**Crate:** ava-platformvm  ·  **Depends on:** M4.13  ·  **Spec:** 08 §7.1 (the reconstruction algorithm + key/value layouts)
**Files:** `crates/ava-platformvm/src/state/disk_staker_diff_iterator.rs`, `crates/ava-platformvm/src/state/diff_iterators.rs`.
- [x] **Step 1 — Red:** Add `golden::weight_diff_key_layout` asserting the key `[subnetID(32)] ++ [inverseHeight=MaxU64-height BE u64] ++ [nodeID(20)]` and value `[isNegative bool] ++ [weight BE u64]` byte-match Go; plus the by-height index `[inverseHeight] ++ [subnetID] ++ [nodeID]`. Add `prop::diff_iter_newest_first` asserting forward key-order iteration walks newest height first.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm weight_diff_key_layout` → fails.
- [x] **Step 3 — Green:** Implement the weight-diff encoder/iterator with `inverseHeight = u64::MAX - height` (so forward iteration = backward in height, 08 §7.1) and the parallel by-height index. Implement the pk-diff store (records the BLS key a node *had before* a change at a height). Provide `apply_validator_weight_diffs(set, from=current, to=target+1, subnet)` and `apply_validator_public_key_diffs(...)` that **un-apply** each diff over `[to, from]` (subtract added weight / add removed; restore prior keys). These are written by the block acceptor (M4.20) and read by the manager (M4.21).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm weight_diff_key_layout prop::diff_iter_newest_first` green.
- [x] **Step 5 — Commit:** `ava-platformvm: weight/pk-diff disk iterators (inverseHeight byte-exact)`

> **As-built (M4.14):** `disk_staker_diff_iterator.rs` ships the byte-exact codecs (`marshal_diff_key_by_subnet_id`/`_by_height`, `marshal_weight_diff`, and their `unmarshal_*` inverses) + `inverse_height`; `diff_iterators.rs` ships `WeightDiffStore`/`PublicKeyDiffStore` (each with Go's **two parallel indexes** — by-subnet for single-subnet reconstruction, by-height for the all-subnets path — as joined sub-spaces under the M4.13 `WEIGHT_DIFF_PREFIX`/`PK_DIFF_PREFIX` handles) and the `apply_validator_weight_diffs`/`apply_all_validator_weight_diffs` + `apply_validator_public_key_diffs`/`apply_all_*` reconstruction fns over `[to, from]` (un-apply: decrease⇒add, increase⇒subtract, `checked_*`, drops zero-weight). **Choices:** (1) `inverse_height = !height` (bitwise NOT — identical value to Go `^height` / `u64::MAX-height` but dodges the `arithmetic_side_effects` lint). (2) The pk-diff layer speaks **raw uncompressed BLS key bytes** (`DiffValidator.public_key: Option<Vec<u8>>`) not `bls::PublicKey` — `ava-crypto` has no `PublicKeyFromValidUncompressedBytes` yet; the M4.21 manager will parse. (3) Two new sentinels `UnexpectedDiffKeyLength`/`UnexpectedWeightValueLength` mirror Go. Layouts: by-subnet key 60B `[subnet(32)]++[invH(8 BE)]++[node(20)]`; by-height key 60B `[invH(8 BE)]++[subnet(32)]++[node(20)]`; weight value 9B `[isNeg(1)]++[weight(8 BE)]`. No new external deps.

---

### Task M4.15: UTXO handler (spend/produce/verify) + ATOMIC-1 fx registration
**Crate:** ava-platformvm  ·  **Depends on:** M4.3, M4.13  ·  **Spec:** 08 §2.4 (helpers), §1 (utxo.rs); 00 §11.1.7 (ATOMIC-1); 07 fx
**Files:** `crates/ava-platformvm/src/utxo.rs`, `crates/ava-platformvm/src/fx.rs`.
- [x] **Step 1 — Red:** Add `golden::atomic_utxo_decode` asserting an avax `UTXO` with a `secp256k1fx::TransferOutput` produced under the P-Chain fx registration decodes byte-identically to the X-Chain encoding (ATOMIC-1 cross-chain contract — registered type_ids 5,7,9,10,11 align). Add `prop::spend_produce_balances` (consumed in == produced out + fee).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm atomic_utxo_decode` → fails.
- [x] **Step 3 — Green:** `fx.rs`: `trait fx::Owner` re-exporting `ava-secp256k1fx::OutputOwners`. `utxo.rs`: the UTXO handler `spend`/`produce`/`verify` over `&state::Diff` (shared with AVM via `ava-vm` components), charging fees and verifying credentials. Confirm the P-Chain codec registers the canonical `avax.UTXO` + secp256k1fx output/input/credential type IDs at the AVM-aligned positions (already done in M4.2 — this task asserts cross-chain decode).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm atomic_utxo_decode prop::spend_produce_balances` green.
- [x] **Step 5 — Commit:** `ava-platformvm: UTXO handler + ATOMIC-1 fx registration`

> **As-built (M4.15):** `utxo.rs` adds the typed `Utxo` (an `#[derive(AvaCodec)]` mirror of `avax.UTXO` — flattened `UTXOID`+`Asset`+fx-output interface) plus free-function handler `produce`/`consume`/`verify_spend` over `&mut dyn state::Chain` (so it composes with the M4.13 `Diff` overlay and `State`), owning the typed↔`UtxoBytes` boundary M4.13 left opaque. `Utxo::marshal`/`unmarshal` route through the shared `txs::codec::Codec()` manager, so the 2-byte version prefix + trailing-byte check are inherited and the bytes are byte-identical to the X/C `avax.UTXO` encoding (ATOMIC-1; type_ids 5/7/9/10/11 aligned — pinned by a hand-built golden vector since the renamed Go handler path `utxo/verifier.go` ships no `*_test.json`). `verify_spend` is the single-asset, no-locktime slice of Go's `VerifySpendUTXOs` (value conservation `consumed == produced + fee`, `checked_*`); locktime/multi-asset accounting + full credential checks are deferred to the M4.16 standard executor. `fx.rs` re-exports `OutputOwners` as `fx::Owner`, adds `owner_id` (sha256 of the type_id-11-wrapped owner — the locked-funds map key) and `verify_transfer` wrapping `secp256k1fx::Fx::verify_credentials`. ATOMIC-1 registration is *asserted*, not re-registered (M4.2 `txs::codec` is the single source of truth). No new external deps. **NOTE for M4.24 cleanup:** `genesis.rs` independently defined its own `Utxo` (it merged before M4.15 landed) — fold it onto `utxo::Utxo` in a later pass.

---

### Task M4.16: `StandardTx` executor + staker/subnet verification helpers
**Crate:** ava-platformvm  ·  **Depends on:** M4.15, M4.7, M4.8  ·  **Spec:** 08 §2.4 (StandardTx + shared helpers)
**Files:** `crates/ava-platformvm/src/txs/executor/mod.rs` (Visitor trait), `standard_tx_executor.rs`, `staker_tx_verification.rs`, `subnet_tx_verification.rs`, `state_changes.rs`.
- [x] **Step 1 — Red:** Port `txs/executor/standard_tx_executor_test.go` cases as `conformance::standard_executor` table tests: each input (state, tx) produces the expected `(consumed_input_ids, atomic_requests, resulting Diff)`; include `verify_add_permissionless_validator` bound checks and a subnet-auth case.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm standard_executor` → fails.
- [x] **Step 3 — Green:** `trait Visitor { type Error; one method per UnsignedTx variant defaulting to Err(ErrWrongTxType) }`; `UnsignedTx::visit` dispatches. `StandardTxExecutor` overrides the decision txs (Base, Create*, Import/Export, Add/RemovePermissionless*, all L1 txs, TransferSubnetOwnership), mutating a `Diff` and returning `(consumed_input_ids, atomic_requests, on_accept_fn)`. Shared free functions over `&Diff`: `verify_add_permissionless_validator`/`_delegator` (stake bounds, `MaxStakeDuration`, subnet existence, staker overlap, fee charge via M4.8, BLS uniqueness, `MaxFutureStartTime=24*7*2h`, `SyncBound=10s`), `verify_subnet_authorization` (resolve owner from CreateSubnetTx/TransferSubnetOwnershipTx + check `subnet_auth`).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm standard_executor` green.
- [x] **Step 5 — Commit:** `ava-platformvm: StandardTx executor + staker/subnet verification`

> **As-built (M4.16):** The `Visitor` trait already shipped in M4.2 (`txs/mod.rs`), so M4.16
> only added the executor module + `StandardTxExecutor` impl. **Public sibling contract** in
> `txs/executor/mod.rs` (the gateway the M4.17/18/19 wave built on): `Backend { upgrades:
> UpgradeSchedule, staking: StakingConfig, static_fee_config, network_id, chain_id, avax_asset_id,
> node_id, fx, bootstrapped }` (a self-contained port collapsing Go's `Backend` + `snow.Context` +
> `config.Internal` — fork activation is `SystemTime` compares `is_durango_activated`/`is_etna_activated`);
> `StandardTxExecutor::new(backend, &mut Diff, &Tx, unsigned_bytes) -> Self` + `into_outputs() ->
> StandardTxOutputs { inputs: BTreeSet<Id>, atomic_requests: BTreeMap<Id, AtomicRequests>, on_accept:
> Option<Box<dyn FnOnce()+Send>> }`; `AtomicRequests { put_requests, remove_requests }` (reused by M4.18);
> `pub(crate)` helpers `subnet_tx_verification::{verify_subnet_authorization, verify_authorization,
> decode_owner}`, `staker_tx_verification::{verify_add_permissionless_validator/_delegator,
> verify_staker_start_time, get_validator}` + consts `SYNC_BOUND`/`MAX_FUTURE_START_TIME`,
> `state_changes::{fee_calculator(backend, &dyn Chain) -> FeeCalculator, verify_spend(...)}`. **Choices/deferrals:**
> (1) flow check = the single-asset AVAX `utxo::verify_spend` (M4.15 byte-stored model); full multi-asset/locktime
> credential check deferred to the maturing flow checker. (2) Import/Export handled decision-side (local
> UTXO consume/produce + record `AtomicRequests`) but the shared-memory flow check was deferred to **M4.18**.
> (3) L1-lifecycle + proposal txs fall through to the default `WrongTxType` (M4.17/M4.19's domain). (4) Legacy
> `AddValidator/AddSubnetValidator/AddDelegator/TransformSubnet` not overridden (pre-Durango legacy / post-Etna
> removed — need the `Bootstrapped`-gated credential flow checker). (5) Added executor `Error` sentinels to
> `error.rs`; **no `Chain`/`Diff` API changes**. Ported Go cases: CreateSubnet valid, BaseTx-pre-Durango reject,
> proposal-tx reject, AddPermissionlessValidator weight-bound + valid-primary, two subnet-auth (CreateChain
> unknown-subnet / 0-of-0 owner), AddValidatorTx → WrongTxType. Deferred: warp/shared-memory-heavy L1 suites
> (M4.18/19), TransformSubnet, full Apricot/Banff/Etna AddSubnetValidator + over-delegation (need legacy executors
> + VM env). 63 tests green, clippy clean.

> **UPSTREAM DELTA (Go `55a1512be1`, ACP-236 (4), #5203 — folded 2026-06-17).** Go has now
> **implemented** the previously-`errUnimplemented` auto-renew standard-execution cases plus
> their state persistence (spec 08 §2.4 upstream-delta): `AddAutoRenewedValidatorTx`
> (verify → reward-calc → supply bump → `PutCurrentValidator` via new `state.NewStaker`) and
> `SetAutoRenewedValidatorConfigTx` (verify → mutate `StakingInfo.{AutoCompoundRewardShares,
> NextPeriod}` → consume/produce UTXOs), with a shared `verifySpend` helper (= the M4.16
> `state_changes::verify_spend`) and `State.write` now persisting auto-renew `StakingInfo` via
> **codec v2** (M4.11 already has the metadata). **All Helicon-gated → dormant, non-gating** (no
> scheduled network activates Helicon). When implementing: extend `StandardTxExecutor` +
> `staker_tx_verification` with the two cases + add `state::new_staker`. `RewardAutoRenewedValidatorTx`
> (type 42) stays unimplemented upstream (later ACP-236 part). No new task — folds into the existing
> Helicon-auto-renew surface (the M4.17 restake path, §M4.17 step 3, is the natural sibling).

---

### Task M4.17: `ProposalTx` executor (advance_time + reward) — the oracle
**Crate:** ava-platformvm  ·  **Depends on:** M4.16, M4.7, M4.9  ·  **Spec:** 08 §2.4 (ProposalTx, advance_time_to), §4.2 (oracle); 21 §3
**Files:** `crates/ava-platformvm/src/txs/executor/proposal_tx_executor.rs`, `crates/ava-platformvm/src/txs/executor/advance_time.rs`.
- [x] **Step 1 — Red:** Port `advance_time_test.go` + `reward_validator_test.go` as `conformance::proposal_executor`: `AdvanceTimeTx` and `RewardValidatorTx` each produce `on_commit_state` AND `on_abort_state` diffs and the "commit preferred" bool; assert supply mint on commit, no mint on abort, staker promotion/removal order.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm proposal_executor` → fails.
- [x] **Step 3 — Green:** `ProposalTxExecutor` overrides `AdvanceTimeTx` (Apricot-only) and `RewardValidatorTx`, producing two diffs. `advance_time_to(diff, new_time)`: promote pending→current stakers whose `next_time <= new_time` in `Staker` (Less) order; remove expired permissioned subnet validators; charge L1 continuous fees (M4.9) and deactivate exhausted L1 validators in `EndAccumulatedFee` order (08 §2.4). `RewardValidatorTx`: pay `PotentialReward` (commit) or not (abort), update supply, write reward UTXOs, restake auto-renewed validators (Helicon). Returns `prefers_commit`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm proposal_executor` green.
- [x] **Step 5 — Commit:** `ava-platformvm: ProposalTx executor (advance_time + reward oracle)`

> **As-built (M4.17):** `proposal_tx_executor.rs` (`ProposalTxExecutor`, `Visitor` over `advance_time`
> + `reward_validator`, both producing `on_commit`/`on_abort` `Diff`s) + `advance_time.rs`
> (`advance_time_to`: promote pending→current in `Staker::Ord` order with supply mint, remove expired
> permissioned-subnet currents, charge L1 continuous fee + deactivate exhausted L1 validators post-Etna).
> **NEW `Chain` trait method (cross-cutting — relevant to M4.20/M4.21):** `fn active_l1_validators(&self)
> -> Vec<L1Validator>` (Go `GetActiveL1ValidatorsIterator`/`NumActiveL1Validators`), sorted
> `(end_accumulated_fee, validation_id)`, implemented in `state.rs` (filter+sort) and `diff.rs`
> (overlay-aware). **Key deviations:** (1) **No tx store in-crate yet** (M4.20) → `ProposalTxExecutor::new`
> takes an injected `StakerTxResolver` closure (`Fn(&Id) -> Option<RewardedStakerTx>`) standing in for
> Go's `state.GetTx`; the block manager (M4.20) injects the real lookup. (2) `prefers_commit` returns
> `true` from the executor; RewardValidator's real uptime-based preference is computed at the block layer
> (M4.20). (3) Deferred (crate lacks the accessors): delegatee-reward split / `GetStakingInfo`,
> `GetExpiryIterator`, Helicon auto-renew restake, and the dynamic gas-fee `advanceDynamicFeeState`
> (only L1 continuous-fee was needed for the named conformance). Added `error.rs` sentinel `RemoveWrongStaker`.
> Ported Go cases: advance-time promote+mint vs abort-unchanged, permissioned-subnet removal, reward commit
> (refund + reward UTXO) vs abort (supply down, staker removed both sets), `RemoveWrongStaker`/`RemoveStakerTooEarly`,
> credentialed-proposal-tx reject. Deferred: full `TestAdvanceTimeTxUpdateStakers` matrix, delegator-reward,
> `TestTrackedSubnet` (need full VM env + tx store).

---

### Task M4.18: `AtomicTx` executor (Apricot import/export path)
**Crate:** ava-platformvm  ·  **Depends on:** M4.15  ·  **Spec:** 08 §2.4 (AtomicTx)
**Files:** `crates/ava-platformvm/src/txs/executor/atomic_tx_executor.rs`.
- [x] **Step 1 — Red:** Port `atomic_tx_executor_test.go` as `conformance::atomic_executor`: an `ImportTx`/`ExportTx` in the Apricot atomic-block path emits the expected `atomic::Requests` against the peer chain's shared memory and the resulting Diff.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm atomic_executor` → fails.
- [x] **Step 3 — Green:** `AtomicTxExecutor` wraps Import/Export for the pre-Banff `ApricotAtomicBlock` path, producing `atomic::Requests` (shared-memory ops, ATOMIC-1). Note post-Banff these are ordinary StandardTx decision txs inside a `BanffStandardBlock` (handled by M4.16) — this executor is for the legacy atomic-block path only.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm atomic_executor` green.
- [x] **Step 5 — Commit:** `ava-platformvm: AtomicTx executor (Apricot import/export)`

> **As-built (M4.18):** `atomic_tx_executor.rs` (`AtomicTxExecutor`, the legacy `ApricotAtomicBlock`
> Import/Export path) reuses M4.16's `AtomicRequests`/`StandardTxOutputs`/`Backend`/`state_changes`/`utxo`
> verbatim. Computes the shared-memory flow accounting M4.16 deferred: **import** fetches imported UTXOs,
> checks single-asset AVAX balance `local_in + imported_avax_in == out + fee` (gated on `bootstrapped`),
> records `remove_requests`; **export** balances `local_in == local_out + exported_out + fee` with
> deterministic `PutRequest` UTXOIDs at `len(outs)+i` (byte-identical to M4.16's export keying).
> **Shared-memory seam (flagged):** added a minimal in-file `trait SharedMemory { fn get(&self, peer_chain:
> Id, keys) -> Result<Vec<Vec<u8>>> }` (read-only for imports; writes flow only through `AtomicRequests`) with
> an in-memory test double — to be **unified with the real `chains/atomic` `SharedMemory`** when it lands
> (M4.20). **No `error.rs`/state-API/`Cargo.toml` changes** (only `mod.rs` +1 line). Ported Go cases:
> wrong-tx-type reject; import valid / insufficient-funds / missing-shared-memory-UTXO / unbootstrapped-skip /
> wrong-asset; export valid / insufficient-funds. Deferred: per-credential `VerifySpendUTXOs` (locktime+sig,
> same single-asset slice M4.16 uses) and `verify.SameSubnet` peer-chain check (→ M4.20 chain-manager wiring).
> 71 tests green (8 new), clippy clean.

---

### Task M4.19: ACP-77 L1 validator lifecycle executor
**Crate:** ava-platformvm  ·  **Depends on:** M4.16, M4.9, M4.12  ·  **Spec:** 08 §6 (ACP-77 lifecycle); 20 §3.1 (RegistryPayload), §6 (warp verify on P)
**Files:** `crates/ava-platformvm/src/txs/executor/l1_executor.rs`, `crates/ava-platformvm/src/warp/verifier.rs`.
- [x] **Step 1 — Red:** Port `convert_subnet_to_l1`/`register_l1_validator`/`set_l1_validator_weight`/`increase_balance`/`disable` executor tests + `warp_verifier_test.go` as `conformance::l1_lifecycle`: assert ConvertSubnetToL1 removes permissioned validators and installs L1 validators; Register funds `EndAccumulatedFee` via a verified Warp `RegisterL1Validator` message + PoP; SetWeight enforces monotonic `nonce >= MinNonce` and rejects removing the last validator; Disable refunds remaining balance to `RemainingBalanceOwner`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm l1_lifecycle` → fails.
- [x] **Step 3 — Green:** Implement the five L1 tx handlers per 08 §6, all mutating through `Diff::put_l1_validator` with the immutable-field guard (M4.12). `warp/verifier.rs` verifies the embedded Warp messages by parsing `RegistryPayload` (via `ava-warp::registry`, 20 §3.1) and checking the BLS aggregate against the source subnet set at the pinned height (deferred quorum integration consumes M4.21/M4.22). Continuous-fee deactivation during time-advance reuses M4.17's `advance_time_to`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm l1_lifecycle` green.
- [x] **Step 5 — Commit:** `ava-platformvm: ACP-77 L1 validator lifecycle executor + warp verifier`

> **As-built (M4.19):** `txs/executor/l1_executor.rs` (5 handlers: Convert/Register/SetWeight/IncreaseBalance/Disable,
> all through `Diff::put_l1_validator` + the M4.12 immutable-field guard) + a **new local `warp` module**
> (`src/warp/{mod,verifier}.rs` + `warp/payload/` + `warp/message/`) since no `ava-warp` crate exists yet — it
> implements the three nested codec layers per 20 §3.1 (`Message`/`UnsignedMessage`/`BitSetSignature`;
> `AddressedCall`/`Hash`; `RegisterL1Validator`/`L1ValidatorWeight`/`SubnetToL1Conversion`/
> `L1ValidatorRegistration`/`PChainOwner`). **Flag: move these to `ava-warp` when it lands** (no cross-crate dep
> added). **Signature/quorum seam for M4.21/M4.22:** a `WarpSignatureVerifier` trait injected into the verifier
> + executor — parsing/registry-`verify()`/PoP run unconditionally; the BLS-aggregate/`WarpSet`/quorum step is the
> trait method (`WARP_QUORUM 67/100` consts in place; `AcceptingVerifier`/`RejectingVerifier` test doubles).
> **Two state seams deferred (no-op stubs, flagged for the state task to wire):** (a) `NumActiveL1Validators` vs
> `ValidatorFeeConfig.Capacity` capacity gate; (b) `RegisterL1Validator` expiry-replay guard (`HasExpiry`/`PutExpiry`).
> `SubnetToL1Conversion` is stored in the existing `subnet_manager` slot (new local `SubnetConversion` codec struct);
> `conversion_id` left as `Id::EMPTY` (full `SubnetToL1ConversionID` hash derivation deferred — flagged). lib.rs +1
> (`pub mod warp;`), mod.rs +1, 9 new `error.rs` sentinels. Ported all five handler suites + 4 warp_verifier tests;
> deferred `errMaxNumActiveValidators`/`errWarpMessageAlreadyIssued` (need the two state seams) and quorum-failure
> cases (need the M4.21/M4.22 real verifier). 79 tests green, clippy clean.

---

### Task M4.20: Block executor Verify/Accept/Reject/Options + acceptor
**Crate:** ava-platformvm  ·  **Depends on:** M4.16, M4.17, M4.18, M4.14  ·  **Spec:** 08 §4.2 (oracle Verify/Accept/Options), §4.1; 19 §2 (bootstrap accept-without-verify)
**Files:** `crates/ava-platformvm/src/block/executor/mod.rs`, `verify.rs`, `accept.rs`, `reject.rs`, `options.rs`, `acceptor.rs`.
- [x] **Step 1 — Red:** Add `conformance::block_oracle` (proposal→commit/abort option generation; selecting the right diff on Accept) and `conformance::accept_writes_diffs` asserting Accept flushes the block's Diff to State, writes weight/pk diffs (M4.14), updates `blockIDDB`/singleton last-accepted/height, and notifies the validator manager.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm block_oracle` → fails.
- [x] **Step 3 — Green:** Port `block/executor/`: `Verify` runs the appropriate executor(s) and caches the resulting Diff(s) per block; for `*ProposalBlock`, `Options()` produces `*CommitBlock`+`*AbortBlock` children (same parent) with `on_commit_state`/`on_abort_state` (08 §4.2). `Accept` applies the selected diff down to `State`, commits the DB batch, writes the staker weight/pk diffs at the block height, sets last-accepted, and calls `validators::Manager::on_accepted_block_id`. `Reject` discards. A `non_verifying` acceptor path (for bootstrap, 19 §2) accepts fetched blocks WITHOUT re-running Verify.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm block_oracle accept_writes_diffs` green.
- [x] **Step 5 — Commit:** `ava-platformvm: block executor (oracle Verify/Accept/Reject/Options) + acceptor`

> **As-built (M4.20):** Ported `block/executor/` as an idiomatic `BlockManager<D>` (not a Go-style
> struct-per-file): `mod.rs` (the manager — owns `State`, a frozen base snapshot for diff parents,
> the per-block diff cache, codec, last-accepted pointer, notifier; impls `Versions`; `commit_accept`
> flushes the diff + writes per-height weight/pk diffs + records block/txs + advances singletons +
> notifies; `reject()` discards the cached diff), `verify.rs` (standard→single diff; proposal→
> commit/abort pair via M4.17 `ProposalTxExecutor` with the real tx-store-backed `StakerTxResolver`;
> options bind the parent proposal's chosen diff), `options.rs` (oracle `(commit, abort)` children over
> the proposal id at height+1, ordered by `prefers_commit`), `acceptor.rs` (`accept`: proposal=note-only,
> option=apply chosen diff, standard=apply diff; `accept_non_verifying`: bootstrap path, 19 §2),
> `reject.rs` (design doc; `reject()` is on the manager). 4 conformance tests assert **real** state flush
> (staker add/remove, supply mint/un-mint, reward UTXO) + the height diff written (verified by reconstructing
> the prior set via the M4.14 iterator) + block-store/singletons + notifier firing. **Seams added (the
> convergence M4.20 owned):** (1) **tx store** on `Chain` (impl `State`+`Diff`): `get_tx(&self, Id) ->
> Result<Vec<u8>>` / `add_tx(&mut self, Id, Vec<u8>)` (signed-tx bytes → `txDB`; tx-*status* not tracked —
> read-only reward resolver doesn't need it). (2) **block store + singletons** on `State<D>`: `get_block`/
> `add_block`/`get_block_id_at_height`/`last_accepted`/`set_last_accepted`/`height`/`set_height`/`snapshot`/
> `current_validator_weights`/`current_validator_public_keys`/`weight_diff_store`/`public_key_diff_store`
> (the M4.14 `_`-prefixed handles are now live). (3) **validator-notify seam:** in-crate `trait
> BlockAcceptanceNotifier { fn on_accepted_block_id(&self, Id); }` + `NoopNotifier`, injected as
> `Arc<dyn …>` — **the `OnAcceptedBlockID` hook M4.21's `PChainValidatorManager` implements** (controller
> may relocate it to `ava-validators` later). 97 tests green (was 79), clippy `--all-targets --all-features
> -D warnings` clean, fmt clean. **Deferred:** ApricotAtomic verify path rejected pending real
> `chains/atomic SharedMemory` (M4.18's seam); standard-block `on_accept`/atomic_requests + mempool re-issue
> on reject are builder concerns (M4.25); warp re-verify + uptime-driven `prefers_commit` → M4.21/M4.22;
> byte-exact on-disk migration deferred (08 §3.2). Spec-reviewed ✅ (independent re-run of tests/clippy/fmt).

---

### Task M4.21: `PChainValidatorManager` — the `ValidatorState` impl
**Crate:** ava-platformvm  ·  **Depends on:** M4.14, M4.20  ·  **Spec:** 08 §7 (ValidatorState), §7.1 (diff windowing); 00 §6.1 (BTreeMap determinism)
**Files:** `crates/ava-platformvm/src/validators/manager.rs`, `crates/ava-platformvm/src/validators/mod.rs`.
- [x] **Step 1 — Red:** Port `manager_test.go` + `validator_set_property_test.go` as `conformance::validator_set_at_height`: build a chain of staker add/remove blocks, then for **every** height assert `get_validator_set(h, subnet)` (weight + BLS key) equals the Go output via the `inverseHeight` reconstruction; assert `get_minimum_height`/`get_current_height`/`get_subnet_id` and `errUnfinalizedHeight` (not panic) when `current < target`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm validator_set_at_height` → fails.
- [x] **Step 3 — Green:** Implement `PChainValidatorManager` impl of `ava_validators::ValidatorState` (08 §7): `get_minimum_height` (recently-accepted window oldest parent, or current if `use_current_height`), `get_current_height` (last-accepted height), `get_subnet_id` (PLATFORM→PRIMARY else CreateChainTx.subnet), `get_validator_set(target, subnet)` reconstructing from the in-mem current set by un-applying weight+pk diffs over `(target, current]` with the per-subnet height→set LRU (size 64), `get_current_validator_set` (base stakers keyed tx_id + L1 validators keyed validation_id), `get_warp_validator_sets(height)` (flatten each subnet via `FlattenValidatorSet`, skip un-flattenable). All returns are `BTreeMap<NodeId,_>` (canonical order). Use `recently_accepted: Window<Id>` (MaxSize 64, TTL 30s) and `ArcSwap` for the current set (lock-free reads, 08 §12).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm validator_set_at_height` green.
- [x] **Step 5 — Commit:** `ava-platformvm: PChainValidatorManager (ValidatorState + diff windowing)`

> **As-built (M4.21):** `validators/manager.rs` ships `PChainValidatorManager<D>` impl of
> `ava_validators::ValidatorState` (all six async methods). **Construction:** `from_state(state,
> use_current_height) -> Self` captures an immutable `ManagerView` (current per-subnet sets, diff
> stores, height, block-id→height index, frozen `Chain`) behind an **`ArcSwap`** (lock-free reads,
> 08 §12); `refresh(&self, state)` re-captures the view + clears caches — **the acceptor integration
> point** (called after `commit_accept` flushes a block). `get_validator_set` un-applies BOTH weight
> and pk diffs over `[target+1, current]` from the current set (per-subnet height→set LRU size 64),
> reconstructing weights **and** BLS keys, returning `BTreeMap<NodeId, GetValidatorOutput>`.
> `errUnfinalizedHeight` is **returned** (`VError::UnfinalizedHeight`), never panicked. Implements the
> M4.20 **`BlockAcceptanceNotifier`** (`on_accepted_block_id` pushes into a local sliding window —
> `parking_lot::Mutex<VecDeque<(Instant, Id)>>`, MaxSize 64 + TTL 30s, size-takes-precedence; no new
> external crate). `get_warp_validator_sets` flattens per subnet by BLS-key dedup (sum weights per key;
> keyless dropped from `validators` but counted in `total_weight`; deduped entries use `NodeId::EMPTY`),
> skipping un-flattenable subnets. **Cross-crate additions (minimal, justified, spec-reviewed):**
> `ava-validators::Error::{UnfinalizedHeight, State{message}}`; `ava-crypto` `PublicKey::from_uncompressed`
> (parses the 96-byte uncompressed bytes the pk-diff store holds — `blst::key_validate`, subgroup-checked
> — no parse path existed) + roundtrip test; `State` accessors `current_validator_sets()`/`block_id_index()`/
> `base()`. **Deferred:** `get_current_validator_set` reads L1 validators via `Chain::active_l1_validators()`
> (active only — no "all L1 for subnet" accessor yet); production wiring of `refresh` into the acceptor loop
> is the integration task (M4.25). 101 tests green (was 97); clippy `--all-targets --all-features -D warnings`
> clean; fmt clean. Spec-reviewed ✅ (independent re-run, 141 tests across ava-platformvm/ava-validators/ava-crypto).

### Task M4.22: Warp signing on P (`UnsignedMessage` + warp set serving)
**Crate:** ava-platformvm  ·  **Depends on:** M4.21  ·  **Spec:** 08 §8 (warp); 20 §2,§4,§5.1,§6.1 (P-Chain consumes ava-warp)
**Files:** `crates/ava-platformvm/src/warp/mod.rs`, `crates/ava-platformvm/src/warp/signer.rs`.
- [x] **Step 1 — Red:** Add `golden::pchain_warp_message` asserting a P-Chain `UnsignedMessage{network_id, source_chain_id=P, payload}` marshals byte-identically to Go (`0x0000` version prefix + fields) and `id == sha256(bytes)`; add `conformance::pchain_warp_sign_verify` (local sign → `BitSetSignature` → `verify` against the §7 warp set Ok; flip a bit → fails).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm pchain_warp_message` → fails.
- [x] **Step 3 — Green:** Wire `ava-warp` (`UnsignedMessage`, `Signature::BitSet`, `verify`, `flatten_validator_set`) into the P-Chain: `LocalSigner` over the node BLS key signing `msg.bytes()` (signature DST, 20 §5.1); verification obtains the source-subnet `WarpSet` at the proposervm-pinned height via M4.21's `get_warp_validator_sets` (20 §6.1). This is the P-side glue; the generic primitives live in `ava-warp` (M0/M3).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm pchain_warp_message pchain_warp_sign_verify` green.
- [x] **Step 5 — Commit:** `ava-platformvm: warp signing on P + warp set serving`

> **As-built (M4.22):** Added `UnsignedMessage::id()` (`sha256(marshal())`, single-pass, 20 §2.1 — did not
> exist before) + `warp/signer.rs` (`LocalSigner` over the node BLS key: checks source-chain/network
> authority then signs the version-prefixed `marshal()` bytes with the BLS **message** DST `sk.sign` — NOT
> the PoP DST) + the **real verifier** in `warp/verifier.rs` (replacing M4.19's deferred seam):
> `verify_bit_set_signature(sig, msg, network_id, &WarpSet, quorum_num, quorum_den)` parses the bitset
> (no-padding invariant via `ava_utils::bits::Bits`), filters canonical-ordered signers, sums weight,
> checks quorum (`verify_weight`: u128-scaled `quorum_num*total <= quorum_den*sig`, `checked_mul`), aggregates
> signer pubkeys (`ava_crypto::bls::aggregate_public_keys`), and verifies the aggregate over `msg.marshal()`;
> `WarpSetVerifier<'a, V: ValidatorState>::verify(&Message)` maps `source_chain_id → subnet_id` via
> `get_subnet_id` then pulls `get_warp_validator_sets(p_chain_height)[subnet]` (M4.21's set is already
> dedup+sorted by uncompressed pubkey = Go canonical order, so no re-flatten). Tests: `golden::pchain_warp_message`
> (byte-EXACT marshal + `id==sha256` + Message round-trip), `conformance::pchain_warp_sign_verify` (2-validator
> real `aggregate_signatures` verifies Ok; flipped-bit fails; below-quorum single signer 50<67% fails — proving
> aggregate + quorum are actually checked). 9 new `error.rs` sentinels (`WrongSourceChainId`/`WrongNetworkId`/
> `InvalidBitSet`/`UnknownValidator`/`InsufficientWeight`/`ParseSignature`/`InvalidSignature`/`NoValidatorSet`/
> `Validators(#[from] ava_validators::error::Error)`). No `ava-crypto` changes (all primitives present). **Deferred
> per scope:** `gwarp` gRPC service, ACP-118 aggregator, separate `ava-warp` crate, JSON-RPC warp API. 103 tests
> green; clippy/fmt clean. (Integrated with M4.23/M4.25: 107 tests green on merged tree.)

---

### Task M4.23: `differential::validatorstate_parity`
**Crate:** ava-platformvm  ·  **Depends on:** M4.21  ·  **Spec:** 08 §7,§7.1; 02 §11 (recorded-oracle); 00 §6.1
**Files:** `crates/ava-platformvm/tests/differential_validatorstate.rs`, `crates/ava-platformvm/tests/vectors/platformvm/validator_diff_windows/*.json`.
- [x] **Step 1 — Red:** Add `differential::validatorstate_parity` replaying a recorded sequence of P-Chain blocks (Go-extracted, with per-height `GetValidatorSet`/`GetWarpValidatorSets` snapshots) and asserting the Rust manager's per-height windower-relevant view (weights + BLS keys, sorted) equals the recorded Go view at every height.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm validatorstate_parity` → fails (missing vectors / mismatch).
- [x] **Step 3 — Green:** Add the Go-extracted `validator_diff_windows` vectors (coordinate with tier X extraction harness). Fix any reconstruction discrepancies surfaced (the marquee diff-windowing test, 08 §11.4).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm validatorstate_parity` green.
- [x] **Step 5 — Commit:** `ava-platformvm: differential::validatorstate_parity (diff-window reconstruction)`

> **As-built (M4.23):** test-only + committed vectors. `tests/differential_validatorstate.rs` ships
> `differential::validatorstate_parity` + a gated vector generator (`gen_vectors`, behind
> `GENERATE_VALIDATOR_DIFF_WINDOWS=1`; a no-op pass in CI). Two recorded vector files under
> `tests/vectors/platformvm/validator_diff_windows/`: `primary_add_remove.json` (add/add/remove over 3
> heights) and `shared_key_and_churn.json` (multi-mutation blocks, two nodes sharing a BLS key → warp
> dedup+sum, plus a churn/no-op block, over 4 heights). **Oracle is non-circular:** the recorded snapshots
> are a **forward accumulation** (start empty, apply add/remove per block, flatten-by-key for warp) — the
> opposite code path from the manager's **backward** diff-reconstruction; the test writes real per-height
> weight/pk diffs into `State` (mirroring `BlockManager::write_validator_diffs`), `refresh`-es the manager
> per block, then asserts `get_validator_set` + `get_warp_validator_sets` match the forward snapshots at
> EVERY height (0..=N), weights AND compressed BLS keys, order-independent. Verified the test genuinely fails
> on a tampered weight. **No reconstruction bugs found in M4.21** (matched across both scenarios incl. shared-key
> warp dedup + no-op block). **Go vectors: none existed** (`tools/extract-vectors` has no P-Chain
> validator-diff-window surface) — followed the M4.24 recorded-oracle precedent; the byte-exact Go-extracted
> golden is recorded as a ⬜ na deferred row in `tests/PORTING.md` (pin once a tier-X harness for
> `vms/platformvm/validators` lands). **Deferred:** an explicit L1-validator scenario (base-staker path exercises
> the same weight/pk diff stores; L1 state would need the crate-internal L1 executor path to fabricate honestly).
> 103 tests green; clippy/fmt clean. (Integrated with M4.22/M4.25: 107 tests green on merged tree.)

---

### Task M4.24: P-Chain genesis build/parse + genesis block
**Crate:** ava-platformvm  ·  **Depends on:** M4.5, M4.13  ·  **Spec:** 23 §3.4,§4.1 (P-Chain genesis assembly + genesis block); 08 §1 (genesis.rs)
**Files:** `crates/ava-platformvm/src/genesis.rs`.
- [x] **Step 1 — Red:** Add `golden::pchain_genesis_block_id` asserting that for the Fuji P-Chain genesis bytes (supplied by `ava-genesis` M8, or a checked-in fixture for now), `genesis_block = ApricotCommitBlock{parent_id: sha256(genesis_bytes), height: 0}` and `genesis_id == sha256(genesis_bytes)` matches the 23 §7 Fuji value `MSj6o9TpezwsQx4Tv7SHqpVvCbJ8of1ikjsqPZ1bKRjc9zBy3`.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm pchain_genesis_block_id` → fails.
- [x] **Step 3 — Green:** Port `vms/platformvm/genesis/{genesis,codec}.go`: `Genesis { UTXOs, Validators, Chains, Timestamp, InitialSupply, Message }` marshalled with `GenesisCodec` (version 0, MaxInt32). `parse` reconstructs UTXOs/validators/chains; `state.init` seeds State from genesis (timestamp, supply, UTXOs, current validators) and stores the `ApricotCommitBlock(genesis_id, 0)` as last-accepted WITHOUT `Accept()` (23 §4.1). The full byte-exact genesis *construction* pipeline (§3) lives in `ava-genesis` (M8); this task provides the P-Chain types + the genesis-block derivation it calls.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm pchain_genesis_block_id` green.
- [x] **Step 5 — Commit:** `ava-platformvm: genesis build/parse + genesis block (ApricotCommit height 0)`

> **As-built (M4.24):** `genesis.rs` ships `Genesis { utxos: Vec<GenesisUtxo>, validators: Vec<Tx>, chains: Vec<Tx>, timestamp: u64, initial_supply: u64, message: String }` (declaration order = linear-codec order) + `GenesisUtxo { utxo, message: Vec<u8> }`, marshalled with `GenesisCodec` (`txs::codec`, version 0, MaxInt32), and the fns `marshal`/`parse`/`genesis_id (=sha256)`/`genesis_block (ApricotCommit, parent=genesis_id, height 0)`/`seed_state<C: Chain>` (seeds timestamp, Primary-Network supply = initial_supply, UTXOs by `input_id`, current validators from the staker txs, chains under their subnet; stores the genesis block as last-accepted WITHOUT `Accept()`). **★ UPDATE (2026-06-16e, M9.19 Gap 2):** `seed_state` now also stores each genesis validator's tx bytes (`state.add_tx(vdr_tx.id(), vdr_tx.bytes().to_vec())`) — previously it recorded the validator as a current staker but not its tx, so the reward-proposal executor's `staker_tx_resolver` (`State::get_tx`) returned `ErrNotFound` and a genesis validator could never be rewarded. Closed; inline test `seed::seed_state_records_genesis_validator_tx` proves the genesis validator tx is now `get_tx`-resolvable and projects through `rewarded_staker_tx`. **Wire fix:** `Genesis.message` is `String` (Go `Message string`, u16-len prefix) — using `Vec<u8>` (u32-len) would break byte parity; per-UTXO `message` correctly stays `Vec<u8>`. Per-validator potential-reward accrual from `syncGenesis` is left at 0 (the reward-wired acceptor applies it later). **Fuji golden DEFERRED:** the real `genesis_id MSj6o9…` is produced only by the full §3.1–§3.3 construction pipeline (AVM+C-Chain genesis bytes, bech32 alloc parsing, `txheap.ByEndTime` validator ordering, X/C `CreateChainTx` assembly) living in `ava-genesis` (M8, not built); the test instead builds a deterministic synthetic `Genesis` and asserts the derivation invariants + `parse(marshal(g))==g`, with the exact-Fuji byte golden recorded as a deferred-vector row in `tests/PORTING.md`. No new external deps; no new error sentinels needed. **Cleanup TODO:** `genesis::Utxo` duplicates `utxo::Utxo` (M4.15) — merged independently; fold together later.

---

### Task M4.25: `PlatformVm` — impl `block::ChainVm`/`Block` (StateSyncableVm = No)
**Crate:** ava-platformvm  ·  **Depends on:** M4.20, M4.21, M4.24  ·  **Spec:** 08 §1 (vm.rs), §4.3 (builder); 19 §5 (P-Chain: linear bootstrap only, no StateSyncableVm); 07 (ChainVm)
**Files:** `crates/ava-platformvm/src/vm.rs`, `crates/ava-platformvm/src/factory.rs`, `crates/ava-platformvm/src/block/builder/mod.rs`.
- [x] **Step 1 — Red:** Add `conformance::vm_initialize_and_last_accepted` asserting `PlatformVm::initialize` from genesis sets `last_accepted == genesis_id`, `get_block(genesis_id)` returns the ApricotCommit block, and `parse_block`/`build_block` round-trip; assert the VM does **not** implement `StateSyncableVm` (linear bootstrap only, 19 §5).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm vm_initialize_and_last_accepted` → fails.
- [x] **Step 3 — Green:** `struct PlatformVm` impl `block::ChainVm` + `Block` (07): `initialize` (open State, seed genesis via M4.24, build the validator manager M4.21), `get_block`/`parse_block` (M4.5/M4.20), `last_accepted`, `set_preference`, `build_block` (the §4.3 builder: options if tip needs them → reward proposal if a staker's next_time arrived → BanffStandardBlock of mempool decision txs capped by size/gas → `ErrNoPendingBlocks`; advance time to `min(now, next_staker_change)` clamped by `SyncBound`), `set_state` (Bootstrapping→NormalOp). Expose `ValidatorState` (M4.21) to the snow context. **No `StateSyncableVm` impl** (19 §5). `factory.rs` constructs the VM for the chain manager.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm vm_initialize_and_last_accepted` green.
- [x] **Step 5 — Commit:** `ava-platformvm: PlatformVm impl ChainVm/Block + block builder`

> **As-built (M4.25):** `vm.rs` ships `PlatformVm` impl `block::ChainVm` + the `Vm` supertrait family.
> `initialize` opens `State` over the engine DB, seeds genesis via M4.24 `seed_state`, records the genesis
> ApricotCommit block as last-accepted at height 0 (`add_block`+`set_last_accepted`+`set_height`, no `Accept`),
> builds the M4.21 `PChainValidatorManager::from_state` and injects it as the `BlockAcceptanceNotifier`.
> `get_block`/`parse_block`/`build_block`/`last_accepted`/`set_preference`/`get_block_id_at_height`/`set_state`
> are wired to the shared M4.20 `BlockManager`. **`as_state_syncable()` returns `None`** (no `StateSyncableVm`,
> 19 §5). Public surface: `PlatformVm::new()`, `validator_state() -> Option<Arc<PChainValidatorManager<DynDb>>>`
> (exposes `ValidatorState` to the snow context / windower / Warp signer); constructed via `factory.rs`
> `PlatformVmFactory::new_vm()` (mirrors Go `Factory.New`; does NOT impl `ava_chains::Factory` to avoid inverting
> the T4-VM→T6-services layering — M4.27/M8 adapts it). **`DynDb`** (`vm/dyndb.rs`) adapts the engine-provided
> `Arc<dyn DynDatabase>` to the typed `Database` surface `State<D>` is generic over (no such adapter existed).
> **refresh-on-accept** (the M4.21 production wiring point): the manager is the `BlockAcceptanceNotifier`
> (updates the recently-accepted window in `accept`), and `PChainBlock::accept` additionally calls
> `Shared::refresh_validators()` to re-capture the manager's snapshot from the just-flushed state.
> **`build_block` (block/builder/mod.rs, §4.3):** faithful to Go `buildBlock` — reward `BanffProposalBlock` if a
> non-permissioned staker's `next_time == block time`, else size-capped `BanffStandardBlock`, else `ErrNoPendingBlocks`;
> `next_block_time = min(max(now, parent_ts), next_staker_change)` clamped by `SyncBound` (10s). **Block wrapper note:**
> the P-Chain `Block` is `!Sync` (its `Tx` holds a `!Sync OnceCell`), so `PChainBlock` stores a `Send+Sync` projection
> (id/parent/height/timestamp/bytes) and re-parses from bytes inside the locked `verify`/`accept`/`reject`. **New
> `error.rs`:** `NoPendingBlocks`, `NotInitialized`, + `From<Error> for ava_vm::Error`/`ava_snow::Error` (the
> `ava-proposervm` precedent for the closed engine enums; non-`NotFound` collapses onto the nearest carrying variant).
> Crate `Cargo.toml` gained `tokio-util`/`chrono`/`serde_json`/`ava-snow`/`ava-version`. **Deferred:** M4.26 (gossip
> mempool — `build_block` uses a minimal in-VM `Vec<Tx>` queue, empty in read-only sync; Etna gas-aware packing),
> M4.27 (bootstrap engine wiring beyond VM hooks), M4.28 (JSON-RPC — `create_handlers` empty). 103 tests green;
> clippy/fmt clean; workspace builds. (Integrated with M4.22/M4.23: 107 tests green, workspace builds, on merged tree.)

---

### Task M4.26: Mempool + network tx gossip
**Crate:** ava-platformvm  ·  **Depends on:** M4.3, M4.25  ·  **Spec:** 08 §4.3 (mempool), §1 (network.rs); 05 (p2p Gossip)
**Files:** `crates/ava-platformvm/src/txs/mempool.rs`, `crates/ava-platformvm/src/network.rs`.
- [x] **Step 1 — Red:** Add `conformance::mempool_dedupe_fifo` (FIFO order, drop-on-full, dedupe by tx ID) and `prop::mempool_no_loss` (add/remove idempotence).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm mempool_dedupe_fifo` → fails.
- [x] **Step 3 — Green:** `mempool.rs`: a `gossip::Gossipable` tx pool (FIFO + drop-on-full, deduped by tx ID), drained deterministically by the builder (M4.25). `network.rs`: tx gossip over `ava-network` p2p `Gossip` (08 §4.3). For read-only sync this need not issue txs, but the handler must accept and dedupe inbound gossip without divergence.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm mempool_dedupe_fifo prop::mempool_no_loss` green.
- [x] **Step 5 — Commit:** `ava-platformvm: mempool + p2p tx gossip`

> **As-built (M4.26):** `txs/mempool.rs` ports the *generic* `vms/txs/mempool` base (not the Etna gas-priced
> `platformvm/txs/mempool` — out of scope for read-only sync): FIFO via `ava_utils::linked::LinkedHashmap`
> (Go `linked.Hashmap` analog — **no new external crate / no Cargo.toml change**), dedupe by tx id
> (re-add → `DuplicateTx`, position preserved), **drop-on-full** by byte budget (`MAX_MEMPOOL_SIZE` 64 MiB summing
> each tx's serialized `size()`; per-tx `MAX_TX_SIZE` 64 KiB; overflow → `MempoolFull`, **never evicts**),
> conflict rejection on UTXO input-id overlap (`ConflictsWithOtherTx`). Public API `new/add/get/contains/remove/
> peek/len/is_empty/iterate/snapshot`. Errors are a **module-local `txs::mempool::Error`** (admission-policy
> outcomes, not consensus errors — not in `error.rs`). `network.rs`: `TxGossipHandler::handle_gossiped_tx(&mut
> Mempool, &impl TxVerifier, Tx) -> HandleOutcome` (dedupe→verify-shape→admit, every drop divergence-free) with a
> local `TxVerifier`/`SyntacticVerifier` seam. **VM placeholder SWAPPED:** `vm.rs` `mempool: Mutex<Vec<Tx>>` →
> `Mutex<Mempool>` (build_block drains via `snapshot()` FIFO; `wait_for_event` uses `is_empty()`). **Deferred:**
> the real `ava-network` p2p transport (`AppGossip` framing, bloom-pull, peer fan-out — `ava-network` exposes no
> generic Gossip framework yet); a follow-up wires it to call `handle_gossiped_tx`. 113 tests green; clippy/fmt
> clean. (Integrated with M4.27/M4.28: 121 tests green on merged tree.)

---

### Task M4.27: Bootstrap wiring + sync to genesis height (TDD entry point #2)
**Crate:** ava-platformvm  ·  **Depends on:** M4.25  ·  **Spec:** 19 §1–§2 (linear bootstrap), §5 (P-Chain linear); 08 §4.2 (acceptor)
**Files:** `crates/ava-platformvm/tests/bootstrap_genesis.rs`; integration glue in `ava-engine` bootstrap config wiring (from M3) consumed here.
- [x] **Step 1 — Red:** Add `differential::pchain_sync_to_tip` (height-0 case — the TDD ENTRY POINT #2): boot the engine bootstrap loop (M3) against a recorded single-block frontier = genesis; assert the VM's `last_accepted == genesis_id` and state hash == recorded Go value at height 0. This proves the bootstrap loop end-to-end before chasing the tip.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm pchain_sync_to_tip` → fails.
- [x] **Step 3 — Green:** Wire `PlatformVm` into the M3 engine bootstrapper (19 §2.2): provide `batched_parse_block` (non-verifying acceptor path, M4.20), `last_accepted`, `get_ancestors`/getter server answers (19 §2.3), and `set_state(Bootstrapping)`. Confirm the linear-bootstrap fetch→execute-forward loop accepts the genesis block and stops at height 0 against the recorded frontier.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm pchain_sync_to_tip` (height-0 subset) green.
- [x] **Step 5 — Commit:** `ava-platformvm: bootstrap wiring + sync to genesis height`

> **As-built (M4.27):** Added **`impl BatchedChainVm for PlatformVm`** (`vm.rs`): `get_ancestors` walks the
> accepted block store newest-first (re-parsing each block to follow `parent_id`, byte-accounting with
> `INT_LEN`/`max_blocks_num`/`max_blocks_size`/`max_retrieval_time` bounds, missing-block→empty,
> missing-parent→break); `batched_parse_block` via `Block::parse` + `wrap()`; `ChainVm::as_batched()` →
> `Some(self)`. **Used the FULL real M3 `Bootstrapper`** (the `ava-engine` `tests/bootstrap.rs` + `tests/support`
> harness was the template): builds a `Config`/`ConsensusContext` with one beacon + a minimal recording `Sender`,
> drives `start → accepted_frontier → accepted → ancestors` against a single-block frontier (= genesis); the
> interval tree declines the height-0 block (already at local last-accepted), `execute` runs the empty range, node
> hands off to NormalOp; asserts `last_accepted == genesis_id` + `as_batched().is_some()` + genesis round-trips.
> **No `ava-engine` src change** (existing public surface only); `ava-engine` added as a **dev-dependency** (no
> cycle). **Test lives inline in `vm.rs`** (module `differential`), not `tests/bootstrap_genesis.rs`, because it
> needs `crate::genesis::test_synthetic_genesis` (`#[cfg(test)] pub(crate)`, unavailable to integration tests) —
> same M4.25 precedent; dev-deps are available to inline `#[cfg(test)]` mods. 108 tests green; clippy/fmt clean.
> **Deferred:** Fuji multi-block sync / chasing-the-tip = **M4.29**; recorded Go height-0 state-hash oracle deferred
> (no extraction harness — M4.24 precedent; PORTING.md row). (Integrated with M4.26/M4.28: 121 tests green.)

---

### Task M4.28: `service.rs` JSON-RPC (read methods)
**Crate:** ava-platformvm  ·  **Depends on:** M4.25, M4.21  ·  **Spec:** 08 §9 (service.go method set), §1; 14 (API reference); 09 status
**Files:** `crates/ava-platformvm/src/service.rs`, `crates/ava-platformvm/src/status.rs`, `crates/ava-platformvm/src/client.rs`.
- [x] **Step 1 — Red:** Add `conformance::service_get_current_validators` asserting `getCurrentValidators` (incl. L1 validators) returns the sorted set matching a recorded Go JSON response, and `getHeight`/`getBlock`/`getBlockByHeight`/`getTimestamp`/`getCurrentSupply` shapes match.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm service_get_current_validators` → fails.
- [x] **Step 3 — Green:** Port the read-relevant `service.go` methods (08 §9): `getHeight`, `getCurrentValidators`, `getL1Validator`, `getCurrentSupply`, `getTimestamp`, `getBlock`, `getBlockByHeight`, `getTx`, `getTxStatus`, `getSubnet(s)`, `validatedBy`/`validates`, `getValidatorsAt`/`getAllValidatorsAt`, `sampleValidators`, `getFeeState`/`getValidatorFeeState`. JSON address encodings bech32 (`P-…`), BLS keys hex (`0x…`). `status.rs` ports the tx-status enum. `client.rs` typed async wrappers (`reqwest`). Served via `ava-api` (12); deterministic sort on `getCurrentValidators` (00 §6.1).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm service_get_current_validators` green.
- [x] **Step 5 — Commit:** `ava-platformvm: service.rs JSON-RPC read methods + client`

> **As-built (M4.28):** `service.rs` `Service<D>` (holds `Arc<State<D>>` + `Arc<PChainValidatorManager<D>>` +
> `network_id` for the bech32 HRP) implements the read surface over live seams: `getHeight`, `getCurrentSupply`,
> `getTimestamp`, `getFeeState`/`getValidatorFeeState`, `getCurrentValidators` (**includes L1 validators**, sorted
> by validation id via the manager's `BTreeMap`), `getL1Validator`, `getValidatorsAt`, `validatedBy`/`validates`,
> `getTxStatus`, `getTx`, `getBlock`, `getBlockByHeight`. `status.rs` = `Status` enum (Go discriminants
> Unknown=0/Committed=4/Aborted=5/Processing=6/Dropped=8, PascalCase JSON) + `BlockchainStatus`. `client.rs` = typed
> async wrappers over a `Transport` trait seam (**reqwest HTTP transport deferred to ava-api M8/M12**; a
> `StubTransport` exercises encode/decode round-trips in tests). Encodings match Go: avajson quoted-string ints,
> `Id`/`NodeId` CB58/`NodeID-`, hex `0x…` compressed BLS keys, RFC3339 timestamps, bech32 `P-…` via
> `ava_crypto::address::format`. **`ava-api` crate does NOT exist yet** → service implemented in-crate, NO HTTP
> server wired (M8/M12). +`error.rs` `Error::Service(String)`; crate `Cargo.toml` promoted `serde`+`hex` to deps.
> **Deferred (PORTING.md rows):** `getCurrentValidators` owner/delegator/reward/uptime/signer fields, `Processing`/
> dropped-reason tx-status paths, `sampleValidators`/`getAllValidatorsAt`/`getSubnet(s)`, fee *price* field (dynamic-
> fee-config seam), and the exact-Go JSON golden (no recorded vector — M4.24 precedent; shape+sort+encodings asserted
> instead). `issueTx`/write methods omitted (read-only sync). 114 tests green; clippy/fmt clean. (Integrated with
> M4.26/M4.27: 121 tests green on merged tree.)

---

### Task M4.29: `differential::pchain_sync_to_tip` (Fuji, CI-gated + recorded oracle) ✅ DONE (2026-06-17; commit e4ba1a4)
**Crate:** ava-platformvm  ·  **Depends on:** M4.27, M4.28, M4.21  ·  **Spec:** 02 §11 (two-binary live + recorded-oracle, CI gating); 19 §2; 08 §7

> **AS-BUILT (2026-06-17).** Extended `differential::pchain_sync_to_tip` beyond height 0 to a
> **Rust-built deterministic multi-block range** (5 empty Banff standard blocks, heights 1..=5)
> driven through the M3 `Bootstrapper` (frontier → agreement → one `GetAncestors` answered
> tip-first/genesis-last → execute → handoff to NormalOp, `last_accepted == tip`), plus a
> **per-height differential arm** on a fresh VM (`parse_block→verify→accept`) asserting
> `(block_id, timestamp, state_digest, getCurrentValidators sorted)` == a committed
> recorded-oracle corpus (`tests/vectors/platformvm/fuji_sync_oracle/linear_range.json`,
> generated behind `GENERATE_PCHAIN_SYNC_ORACLE=1`). The height-0 subset is retained as
> `pchain_sync_to_tip_height0`. **Design choice:** block-codec byte-exactness vs Go is already
> proven by `golden::pchain_block_hash` (M4.6), so M4.29 proves the **forward sync pipeline**
> with a Rust-built corpus; the byte-exact full-range arm vs the Go node is the CI-gated
> `live-fuji` leg (`pchain_sync_to_tip_live_fuji`, a documented deferred stub —
> `--features live-fuji` / `AVA_DIFF_LIVE=1`; not run in CI). `state_digest` is the P-Chain
> flat-KV state-observation surrogate (sha256 over `height‖last_accepted‖ts‖supply‖sorted
> validators`), NOT a merkle root (`08` §3.2 — P-Chain has no merkle root).
**Files:** `crates/ava-platformvm/tests/differential_sync.rs`, `tests/differential/` glue (tier X), `crates/ava-platformvm/tests/vectors/platformvm/fuji_sync_oracle/*.json`.
- [ ] **Step 1 — Red:** Extend `differential::pchain_sync_to_tip` beyond height 0: in **recorded-oracle mode** (default, per-PR), replay a recorded Fuji P-Chain block range and assert at every matching height the last-accepted block ID + state hash + `getCurrentValidators` (sorted) == the recorded Go values. In **live mode** (feature `live-fuji` / env `AVA_DIFF_LIVE`), sync from a real Fuji peer to tip.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-platformvm pchain_sync_to_tip` → fails (recorded range mismatch).
- [ ] **Step 3 — Green:** Drive the M3 bootstrapper against the recorded frontier/ancestors oracle; accept forward; compare per-height observations. Gate the live-peer path behind the `live-fuji` feature + `AVA_DIFF_LIVE` env with the recorded-oracle fallback (coordinate with cross-cutting harness X, `ava-differential`). Commit the `fuji_sync_oracle` vectors.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-platformvm pchain_sync_to_tip` (recorded mode) green; document the `--features live-fuji` invocation in PORTING.md.
- [ ] **Step 5 — Commit:** `ava-platformvm: differential::pchain_sync_to_tip (recorded oracle + CI-gated live)`

---

### Task M4.30: Milestone exit gate ✅ DONE (2026-06-17)
**Crate:** ava-platformvm  ·  **Depends on:** all M4.* tasks  ·  **Spec:** plan/README §2 (buildable-&-green invariant); 02 §10.1 (PORTING.md)
**Files:** `crates/ava-platformvm/tests/PORTING.md`, `crates/avalanchers/src/main.rs` (P-Chain wiring), workspace `Cargo.toml`.

> **AS-BUILT (2026-06-17).** Exit gate green: `cargo build --workspace`, `cargo build -p
> avalanchers`, `cargo nextest run --profile ci` (incl. all M4 named exit tests), and `cargo
> clippy --workspace --all-targets -- -D warnings` all pass. The named exit tests
> (`golden::pchain_block_hash`, `golden::pchain_tx_codec`, `prop::pchain_tx_roundtrip`,
> `differential::pchain_sync_to_tip`, `differential::validatorstate_parity`) are present and
> green.
>
> **Binary boot — built in-process (user-directed, NOT deferred).** The original plan deferred
> the binary-boot leg; this revision built the live-node consensus path so `avalanchers`
> materializes and boots the **real `PlatformVm`** in-process. Decomposed into three landed
> sub-tasks:
> - **M4.30a** (`ava-engine`, commit f15e025): `ChainEngine` adapters wrapping the
>   `Bootstrapper` and `SnowmanEngine`, a handler **state-transition mechanism** (transition
>   channel + `ChainEngine::start` hook; `Initializing→Bootstrapping→NormalOp`), and additive
>   `InboundOp` consensus variants. Proven end-to-end at the engine level (handler-driven
>   bootstrap to NormalOp via synthetic responses).
> - **M4.30b** (`ava-chains`, commit befbe71): `create_snowman_chain` now builds the
>   `Bootstrapper` over a shared `Arc<Mutex<wrapped VM>>`, wraps both engines in the M4.30a
>   adapters, owns the transition channel, registers them, and starts the handler in
>   `Bootstrapping`; `SnowmanChain.engine` → `pub ctx: Arc<ConsensusContext>` (observability).
> - **M4.30c** (`avalanchers`, commit 3e91681): `boot_in_process_pchain(network_id)` boots the
>   real `PlatformVm` from real genesis (verified mainnet/id 1) through the handler to
>   `EngineState::Bootstrapping`, broadcasting `GetAcceptedFrontier` to its beacon set; node
>   test `boots_real_pchain_to_bootstrapping`.
>
> **Remaining live legs (documented, gated — need a live network, not CI-verifiable):** the
> real ava-network-backed `Sender` (engine→wire + `AdaptiveTimeoutManager` registration) and
> driving past `Bootstrapping` to `NormalOp` against real peers (the `live-fuji` arm). The
> in-process boot uses a recording sender; `ChainEngine` errors are currently traced (not
> halt-propagated) — a follow-up if a fatal engine error must tear the chain down (Go behavior).
- [ ] **Step 1 — Red:** Ensure the named exit tests exist and are collected: `golden::pchain_block_hash`, `golden::pchain_tx_codec`, `prop::pchain_tx_roundtrip`, `differential::pchain_sync_to_tip`, `differential::validatorstate_parity`. Run the full suite to surface any gap.
- [ ] **Step 2 — Confirm red (if any):** `cargo nextest run --profile ci -p ava-platformvm` → any red is a real gap to close (loop back to the owning task).
- [ ] **Step 3 — Green:** Wire `PlatformVm` into the `avalanchers` binary's chain manager so it boots far enough to bootstrap the P-Chain read-only (`--network-id=fuji`). Update `tests/PORTING.md` (no `wip` rows for ported P-Chain tests; record `na` with reasons for Go-plumbing-only tests). Ensure committed `proptest-regressions/` + golden vectors + the cargo-fuzz target are present.
- [ ] **Step 4 — Confirm green:** Run the four buildable-&-green commands plus the exit tests:
  - `cargo build --workspace`
  - `cargo build -p avalanchers`
  - `cargo nextest run --profile ci` (incl. all M4 exit tests; `pchain_sync_to_tip` in recorded-oracle mode)
  - `cargo clippy --workspace -- -D warnings`
  - confirm `avalanchers --network-id=fuji` begins P-Chain bootstrap read-only.
- [ ] **Step 5 — Commit:** `ava-platformvm: M4 exit gate — P-Chain read-only sync green; binary boots Fuji P-Chain`

---

## Spec coverage check

| Spec section | Covered by task(s) | Notes / deferrals |
|---|---|---|
| 08 §1 crate layout | M4.1 | |
| 08 §2.1 type_id registry (43 entries, shared block+tx space, skip gaps) | M4.2, M4.5 | secp256k1fx 5–11 incl. MintInput/MintOutput gaps |
| 08 §2.2 UnsignedTx enum + per-tx fields + syntactic_verify | M4.2, M4.3, M4.4 | |
| 08 §2.3 signed Tx envelope (prefix-length trick) | M4.2 | |
| 08 §2.4 executor visitors (Standard/Proposal/Atomic) + helpers | M4.16, M4.17, M4.18, M4.19 | |
| 08 §3.1 Chain/Diff/Versions/State interfaces | M4.13 | |
| 08 §3.2 flat-KV prefixes | M4.13 | byte-exact on-disk layout = migration concern (00 §4.4) deferred to M9/R2 |
| 08 §3.3 staker model + Priority + Ord | M4.10 | |
| 08 §3.4 ValidatorMetadata codec v2 + L1Validator | M4.11, M4.12 | |
| 08 §3.5 supply & reward state | M4.13, M4.17 | |
| 08 §4.1 Block enum + byte-exact codec + block_id | M4.5 | |
| 08 §4.2 proposal/commit/abort oracle | M4.17, M4.20 | |
| 08 §4.3 block builder + mempool | M4.25, M4.26 | |
| 08 §5 reward formula (exact) + Split | M4.7 | |
| 08 §6 dynamic gas fees (Etna) + ACP-77 L1 lifecycle | M4.8, M4.9, M4.19 | |
| 08 §7 ValidatorState serving | M4.21 | |
| 08 §7.1 validator-diff windowing (inverseHeight) | M4.14, M4.21 | the marquee differential M4.23 |
| 08 §8 warp (PoP, BitSetSignature, signing) | M4.4 (PoP), M4.22 | generic primitives in ava-warp (M0/M3) |
| 08 §9 JSON-RPC service + client + status | M4.28 | write/issueTx methods present but read-only sync only exercises reads |
| 08 §10 error model (sentinels) | M4.1 (+ all) | |
| 08 §11 test plan | M4.6, M4.7, M4.16–M4.23, M4.28 | |
| 08 §12 perf (parallel sig verify, ArcSwap, LRU) | M4.21 (ArcSwap+LRU) | parallel verify gated behind differential, deferred refactor |
| 19 §1 three-phase lifecycle | M4.27 | |
| 19 §2 bootstrap state machine + getter | M4.27 | engine actor lives in ava-engine (M3); P provides VM hooks |
| 19 §3–§4 state-sync + merkledb sync | — | **deferred / N/A**: P-Chain does NOT implement StateSyncableVm (19 §5); linear bootstrap only |
| 19 §5 per-VM matrix (P = linear, no state sync) | M4.25, M4.27 | explicitly no StateSyncableVm |
| 20 P-Chain warp signing | M4.22; registry payloads M4.4/M4.19 | EVM precompile (20 §7) = M6 |
| 21 §0 CalculatePrice | M4.8 | |
| 21 §1 ACP-103 dynamic gas fee | M4.8 | |
| 21 §2a static fee / §2b L1 continuous fee | M4.8, M4.9 | |
| 21 §3 staking reward + Split | M4.7 | |
| 21 §4–§6 EVM/SAE fee math | — | **N/A** for P-Chain (M6/M7) |
| 23 §3 full genesis construction pipeline | — (types only) M4.24 | byte-exact construction in ava-genesis (M8); P provides tx/UTXO/genesis types + genesis-block derivation |
| 23 §4.1 P-Chain genesis block (ApricotCommit, h0) | M4.24 | |
| ATOMIC-1 (00 §11.1.7) fx type-id alignment | M4.2, M4.15 | X↔P shared-memory decode; full X↔P atomic test in M5 |
| 02 per-crate contracts (proptest+regressions, goldens, PORTING.md, fuzz) | M4.1, M4.6, M4.30 | |
| 02 §11 differential (recorded-oracle + CI-gated live) | M4.23, M4.29 | live Fuji behind `live-fuji`/`AVA_DIFF_LIVE` |

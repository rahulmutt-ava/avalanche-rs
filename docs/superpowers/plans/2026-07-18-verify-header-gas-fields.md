# verifyHeaderGasFields Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port coreth's `verifyHeaderGasFields` onto the C-Chain verify path (fee/gas header **equality** checks against the parent) and fix the coupled `feerules::base_fee` time-advance, closing the flagged Byzantine-proposer fail-open without introducing false rejects of honest Go blocks under load.

**Architecture:** Three new Go-line-cited verify-side functions in `crates/ava-evm/src/feerules/mod.rs` (`verify_gas_limit`, `verify_extra_prefix`, `verify_header_gas_fields`), reusing the already-ported primitives (`fee_state_before_block`, `fee_state_after_block`, `fee_window`, `blockgas::block_gas_cost`). `base_fee` is repaired at the source (signature refactor from reth `Header` to `AvaHeader` + `feeStateBeforeBlock` time-advance) so builder, RPC, and verifier share the one fixed function. The parent `AvaHeader` is threaded into `EvmBlock::verify_with_predicates` via a new `Shared::parent_header`.

**Tech Stack:** Rust (cargo nextest, clippy `-D warnings`), Go oracle at `~/avalanchego` (coreth `graft/coreth/consensus/dummy/consensus.go` + `plugin/evm/customheader/`), recorded-oracle corpus pattern (`crates/ava-evm/tests/proposer_candidates.rs` + `tests/differential/go-oracle/`).

**Spec:** `docs/superpowers/specs/2026-07-18-verify-header-gas-fields-design.md`

## Global Constraints

- License header on every new `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` + `// See the file LICENSE for licensing terms.`
- No `unwrap()`/`expect()`/`todo!` in library code (clippy denies); tests may use them.
- Every ported check cites the exact Go file:line it mirrors, in a comment, and runs in Go's check order.
- Error messages mirror coreth sentinel strings verbatim (rejection-class parity).
- Imports grouped std → external → crate; 4-space indent; run `cargo fmt` via `./scripts/nix_run.sh cargo fmt` before each commit.
- Per-task gate: `cargo nextest run -p ava-evm` + `cargo clippy -p ava-evm --all-targets -- -D warnings` + `cargo fmt --check`.
- Arithmetic: `saturating_*`/`checked_*`/`try_from` — no raw `as` casts on consensus values.
- Go source of truth: `~/avalanchego/graft/coreth/` (verify `./scripts/check_oracle_binary.sh` prints OK before any oracle-recording step).
- Commit after every task with a scope-prefixed message.

---

### Task 1: `base_fee` time-advance fix (AvaHeader signature refactor)

Closes the deferral documented at `crates/ava-evm/src/feerules/mod.rs:122-138`: the ACP-176 arm reads `gas_price()` off the RAW parent state; coreth computes `feeStateBeforeBlock(parent, childTimeMS).GasPrice()` (advance first, then read). Requires the parent as `AvaHeader` (the reth `Header` has no `time_milliseconds`).

**Files:**
- Modify: `crates/ava-evm/src/feerules/mod.rs` (`base_fee` ~:106, `gas_limit` ~:161)
- Modify: `crates/ava-evm/src/block.rs` (add `Default` to `AvaHeader`'s derive at :85)
- Modify: `crates/ava-evm/src/evmconfig.rs` (`next_evm_env` :503)
- Modify: `crates/ava-evm/src/builder.rs` (`build_on` :175-199; `parent_eth_header` visibility)
- Modify: `crates/ava-evm/src/rpc/eth.rs` (`suggested_base_fee` ~:597)
- Modify: `crates/ava-evm/tests/fee_schedule.rs` (parent construction + new test)

**Interfaces:**
- Produces: `pub fn base_fee(cs: &AvaChainSpec, parent: &AvaHeader, ctx: &AvaNextBlockCtx) -> Result<u64, Error>` and `pub fn gas_limit(cs: &AvaChainSpec, parent: &AvaHeader, ctx: &AvaNextBlockCtx) -> Result<u64, Error>` (parent type changed); `AvaEvmConfig::next_evm_env(&self, parent: &AvaHeader, ctx: &AvaNextBlockCtx) -> Result<AvaEvmEnv, Error>`; `AvaHeader: Default`. Tasks 2–4 call `base_fee` with this signature.

- [ ] **Step 1: Write the failing test**

Append to `crates/ava-evm/tests/fee_schedule.rs` (reuse the file's existing imports/spec constructor — read its header first; the chain-spec construction pattern also appears at `crates/ava-evm/tests/feerules.rs::fee_state_after_block_matches_live_go_block_extra`, which parses `vectors/cchain/genesis/local.json` where every fork is active at genesis):

```rust
/// coreth `customheader/base_fee.go:27-33` — the ACP-176 child base fee is
/// `feeStateBeforeBlock(parent, childTimeMS).GasPrice()`: the parent state is
/// FIRST advanced by the elapsed time (draining `gas.excess`, lowering the
/// price) and only then read. The old code read the raw parent state.
#[test]
fn base_fee_acp176_advances_parent_state_by_elapsed_time() {
    let cs = local_all_active_spec(); // all forks (incl. Fortuna+Granite) at genesis

    // 24-byte ACP-176 prefix: capacity(8) | excess(8) | target_excess(8), BE.
    // A large excess makes the price strictly > MinGasPrice so the advance is
    // observable. Tune constants until the strict inequality below holds.
    let mut extra = Vec::with_capacity(24);
    extra.extend_from_slice(&2_000_000u64.to_be_bytes()); // capacity
    extra.extend_from_slice(&200_000_000u64.to_be_bytes()); // excess (nonzero)
    extra.extend_from_slice(&1_500_000u64.to_be_bytes()); // target_excess
    let raw = Acp176State::from_bytes(&extra).expect("parse fixture state");

    let parent = AvaHeader {
        number: 1,
        time: 1_000,
        extra: extra.clone().into(),
        ..AvaHeader::default()
    };
    let child_ts = 1_060; // 60s elapsed
    let ctx = AvaNextBlockCtx {
        timestamp: child_ts,
        timestamp_ms: child_ts * 1000,
        parent_fee_state: feerules::parent_fee_state_of(&cs, &parent).expect("parent fee state"),
        ..AvaNextBlockCtx::default()
    };

    let advanced = feerules::fee_state_before_block(&cs, &parent, child_ts * 1000)
        .expect("advance parent state");
    assert!(
        advanced.gas_price().0 < raw.gas_price().0,
        "fixture must make the advance observable: advanced {} !< raw {}",
        advanced.gas_price().0,
        raw.gas_price().0
    );
    assert_eq!(
        feerules::base_fee(&cs, &parent, &ctx).expect("base_fee"),
        advanced.gas_price().0,
        "base_fee must return the TIME-ADVANCED price (coreth base_fee.go:27-33)"
    );
}
```

If `fee_schedule.rs` has no all-active spec helper, add `fn local_all_active_spec() -> AvaChainSpec` copying the construction from `feerules.rs::fee_state_after_block_matches_live_go_block_extra` (genesis parse → chain spec).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ava-evm --test fee_schedule base_fee_acp176_advances -- --exact 2>&1 | tail -20`
Expected: compile error (`base_fee` takes `&Header`, test passes `&AvaHeader`) — that IS the red signal; or, if you adapt the test to the old signature first, assertion failure `base_fee must return the TIME-ADVANCED price`.

- [ ] **Step 3: Refactor `base_fee`/`gas_limit` signatures + the ACP-176 arm**

In `crates/ava-evm/src/feerules/mod.rs`:

```rust
pub fn base_fee(cs: &AvaChainSpec, parent: &AvaHeader, ctx: &AvaNextBlockCtx) -> Result<u64, Error> {
    let phase = cs.fork_at(ctx.timestamp);
    match regime_for_phase(phase) {
        FeeRegime::Legacy => Err(Error::NilBaseFee),
        FeeRegime::Window => {
            let params = window_params_for_phase(phase);
            let (window, parent_base) = match &ctx.parent_fee_state {
                AvaFeeState::Window { window, base_fee } => (*window, *base_fee),
                AvaFeeState::Acp176(_) => return Err(Error::NilBaseFee),
            };
            // `time_elapsed = child.Time - parent.Time` (seconds), floored at 0.
            let time_elapsed = ctx.timestamp.saturating_sub(parent.time);
            let bf = window::base_fee_from_window(params, &window, parent_base, time_elapsed);
            Ok(u64::try_from(bf).unwrap_or(u64::MAX))
        }
        // coreth `customheader/base_fee.go:27-33` — the child base fee is
        // `feeStateBeforeBlock(parent, childTimeMS).GasPrice()`: advance the
        // parent state by the elapsed time FIRST (ms at Granite, s at Fortuna),
        // then read the price. Re-derives from `parent.extra` (Go-exact);
        // `ctx.parent_fee_state` is not consulted on this arm.
        FeeRegime::Acp176 => Ok(fee_state_before_block(cs, parent, ctx.timestamp_ms)?
            .gas_price()
            .0),
    }
}
```

Update the doc comment: delete the whole `DEFERRED (tracked follow-up)` block (it is now implemented). Change `gas_limit`'s signature the same way (`parent: &AvaHeader`; its body only touches `ctx`, so nothing else changes — keep its "no elapsed-time-advance nuance" comment, still true).

Remove the now-unused `use ava_evm_reth::Header` import if nothing else in the module needs it (check with `cargo build -p ava-evm`).

- [ ] **Step 4: Add `Default` to `AvaHeader` and sweep the call sites**

`crates/ava-evm/src/block.rs:85`: `#[derive(Clone, Debug, PartialEq, Eq, Default)]` (every field type — `B256`, `U256`, `u64`, `Bytes`, `Address`, `[u8; 8]`, `Option<_>` — implements `Default`).

`crates/ava-evm/src/builder.rs`: make `parent_eth_header` `pub(crate)` (it currently projects the coreth parent onto the fee-bearing reth fields for `next_evm_env`; find its definition with `grep -n "fn parent_eth_header" crates/ava-evm/src/builder.rs`).

`crates/ava-evm/src/evmconfig.rs:503`: change `next_evm_env` to take `parent: &AvaHeader` and derive the reth header internally:

```rust
pub fn next_evm_env(&self, parent: &AvaHeader, ctx: &AvaNextBlockCtx) -> Result<AvaEvmEnv, Error> {
    let parent_eth = crate::builder::parent_eth_header(parent)?;
    // ... existing body, with the reth `ConfigureEvm::next_evm_env(&self.inner,
    // &parent_eth, &attrs)` call using `&parent_eth`, and the
    // `base_fee(&self.chain_spec, parent, ctx)` / `gas_limit(...)` calls using
    // the AvaHeader `parent` directly.
}
```

`crates/ava-evm/src/builder.rs:195-196` (`build_on`): delete the local `let parent_eth = parent_eth_header(parent)?;` projection and pass `parent` straight to `self.evm_config.next_evm_env(parent, ctx)?`.

`crates/ava-evm/src/rpc/eth.rs` (`suggested_base_fee` ~:597): replace `let parent = ava_evm_reth::Header::default();` with `let parent = AvaHeader::default();` (import from `crate::block::AvaHeader` following the file's existing import style). Semantics unchanged: a default parent is genesis-shaped (`number == 0`), so `fee_state_before_block` seeds the zero state and the suggestion path still resolves `MinGasPrice`/`NilBaseFee` exactly as before.

Then compile-sweep: `cargo build -p ava-evm --tests 2>&1 | grep -E "^error" | head`. Fix every remaining caller (known: `crates/ava-evm/tests/fee_schedule.rs` — its `parent` reth `Header` constructions become `AvaHeader` constructions with the same `number`/`time`/`base_fee`/`extra` fields; the `next_evm_env(&parent, ...)` calls take the same `AvaHeader`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm 2>&1 | tail -5`
Expected: PASS, including `base_fee_acp176_advances_parent_state_by_elapsed_time` and every pre-existing `fee_schedule`/`feerules`/`build`/`lifecycle` test (the quiet-net paths have `excess ≈ 0`, where the advance is a no-op — byte-parity with existing goldens must hold).

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p ava-evm --all-targets -- -D warnings && ./scripts/nix_run.sh cargo fmt && cargo fmt --check`

```bash
git add crates/ava-evm
git commit -m "fix(ava-evm): base_fee ACP-176 arm applies the feeStateBeforeBlock time-advance (coreth base_fee.go:27-33); parent threads as AvaHeader"
```

---

### Task 2: `feerules::verify_gas_limit`

**Files:**
- Modify: `crates/ava-evm/src/feerules/mod.rs`
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/tests/feerules.rs`

**Interfaces:**
- Consumes: `fee_state_before_block(spec, parent, time_ms)` (existing), `header_time_ms(h)` (existing private fn, same module), `base_fee`/`gas_limit` signatures from Task 1.
- Produces: `pub fn verify_gas_limit(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader) -> Result<(), Error>`; `Error::GasLimitMismatch { have: u64, want: u64 }`, `Error::GasLimitOutOfRange { have: u64, min: u64, max: u64 }`; module consts `APRICOT_PHASE1_GAS_LIMIT`, `CORTINA_GAS_LIMIT`, `AP0_MIN_GAS_LIMIT`, `AP0_MAX_GAS_LIMIT`. Task 4 calls `verify_gas_limit` first.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ava-evm/tests/feerules.rs` (all-active spec as in Task 1; for the pre-Fortuna arms use the phase-staggered spec pattern from `fee_schedule.rs` — one spec with distinct fork activation times, arms selected by varying `header.time`):

```rust
/// coreth `customheader/gas_limit.go:101-145` — `VerifyGasLimit` per-fork arms.
#[test]
fn verify_gas_limit_fortuna_equality() {
    let cs = local_all_active_spec();
    let parent = AvaHeader { number: 1, time: 1_000, extra: acp176_extra(2_000_000, 0, 1_500_000), ..AvaHeader::default() };
    let want = feerules::fee_state_before_block(&cs, &parent, 1_002_000)
        .expect("pre-block state")
        .max_capacity()
        .0;
    let ok = AvaHeader { number: 2, time: 1_002, gas_limit: want, ..AvaHeader::default() };
    feerules::verify_gas_limit(&cs, &parent, &ok).expect("exact MaxCapacity accepted");

    let bad = AvaHeader { gas_limit: want + 1, ..ok };
    let err = feerules::verify_gas_limit(&cs, &parent, &bad).expect_err("off-by-one rejected");
    assert!(err.to_string().contains("invalid gas limit"), "sentinel parity: {err}");
}
```

Add the analogous three tests (same shape, different spec/arm): `verify_gas_limit_cortina_is_15m` (`want == 15_000_000`), `verify_gas_limit_ap1_is_8m` (`want == 8_000_000`), `verify_gas_limit_ap0_range` (accepts `5_000` and `0x7fff_ffff_ffff_ffff`, rejects `4_999`). Add a small local helper for the fixture prefix:

```rust
fn acp176_extra(capacity: u64, excess: u64, target_excess: u64) -> Bytes {
    let mut e = Vec::with_capacity(24);
    e.extend_from_slice(&capacity.to_be_bytes());
    e.extend_from_slice(&excess.to_be_bytes());
    e.extend_from_slice(&target_excess.to_be_bytes());
    e.into()
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ava-evm --test feerules verify_gas_limit 2>&1 | tail -10`
Expected: compile error — `verify_gas_limit` not found in `feerules`.

- [ ] **Step 3: Implement**

In `crates/ava-evm/src/error.rs` (follow the enum's existing thiserror style):

```rust
/// coreth `customheader/gas_limit.go:24` `errInvalidGasLimit` — equality arms
/// ("%w: have %d, want %d" / "%w: expected to be %d in ..., but found %d").
#[error("invalid gas limit: have {have}, want {want}")]
GasLimitMismatch { have: u64, want: u64 },
/// coreth `gas_limit.go:138-144` — the pre-AP1 range arm
/// ("%w: %d not in range [%d, %d]").
#[error("invalid gas limit: {have} not in range [{min}, {max}]")]
GasLimitOutOfRange { have: u64, min: u64, max: u64 },
```

In `crates/ava-evm/src/feerules/mod.rs`: hoist the two consts currently local to `gas_limit()` to module level and add the AP0 pair (coreth `plugin/evm/upgrade/ap0/params.go:27-28`):

```rust
/// coreth `params/avalanche_params.go` — static pre-Fortuna gas limits.
pub const APRICOT_PHASE1_GAS_LIMIT: u64 = 8_000_000;
pub const CORTINA_GAS_LIMIT: u64 = 15_000_000;
/// coreth `plugin/evm/upgrade/ap0/params.go:27-28` — the pre-AP1 launch range.
pub const AP0_MIN_GAS_LIMIT: u64 = 5_000;
pub const AP0_MAX_GAS_LIMIT: u64 = 0x7fff_ffff_ffff_ffff;
```

```rust
/// coreth `customheader/gas_limit.go:101-145` — `VerifyGasLimit`.
///
/// The verify-side complement of [`gas_limit`]: recomputes the expected gas
/// limit from the parent and equality-checks the header's claim (range-checks
/// pre-AP1). At Fortuna+ the expectation is the ACP-176 `MaxCapacity()` off the
/// time-advanced pre-block state (`gas_limit.go:107-120`).
///
/// # Errors
/// [`Error::GasLimitMismatch`] / [`Error::GasLimitOutOfRange`] on a wrong
/// claim; propagates [`Error::InvalidFeeState`] from the fee-state recompute.
pub fn verify_gas_limit(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let phase = spec.fork_at(header.time);
    if phase >= AvaPhase::Fortuna {
        // gas_limit.go:107-120
        let state = fee_state_before_block(spec, parent, header_time_ms(header))?;
        let want = state.max_capacity().0;
        if header.gas_limit != want {
            return Err(Error::GasLimitMismatch { have: header.gas_limit, want });
        }
    } else if phase >= AvaPhase::Cortina {
        // gas_limit.go:121-128
        if header.gas_limit != CORTINA_GAS_LIMIT {
            return Err(Error::GasLimitMismatch { have: header.gas_limit, want: CORTINA_GAS_LIMIT });
        }
    } else if phase >= AvaPhase::ApricotPhase1 {
        // gas_limit.go:129-136
        if header.gas_limit != APRICOT_PHASE1_GAS_LIMIT {
            return Err(Error::GasLimitMismatch { have: header.gas_limit, want: APRICOT_PHASE1_GAS_LIMIT });
        }
    } else if header.gas_limit < AP0_MIN_GAS_LIMIT || header.gas_limit > AP0_MAX_GAS_LIMIT {
        // gas_limit.go:137-144
        return Err(Error::GasLimitOutOfRange {
            have: header.gas_limit,
            min: AP0_MIN_GAS_LIMIT,
            max: AP0_MAX_GAS_LIMIT,
        });
    }
    Ok(())
}
```

Update `gas_limit()`'s body to use the hoisted consts (delete its local `const` items).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(verify_gas_limit)' 2>&1 | tail -5`
Expected: 4 PASS.

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p ava-evm --all-targets -- -D warnings && ./scripts/nix_run.sh cargo fmt && cargo fmt --check`

```bash
git add crates/ava-evm
git commit -m "feat(ava-evm): port customheader.VerifyGasLimit (gas_limit.go:101-145) as feerules::verify_gas_limit"
```

---

### Task 3: `feerules::verify_extra_prefix`

**Files:**
- Modify: `crates/ava-evm/src/feerules/mod.rs`
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/tests/feerules.rs`

**Interfaces:**
- Consumes: `fee_state_after_block(spec, parent, time, time_ms, gas_used, ext_data_gas_used, desired_target_excess)` (mod.rs:278), `fee_window(spec, parent, timestamp)` (mod.rs:311), `Acp176State::from_bytes` / `PartialEq` / `.target_excess: Gas`, `opt_u256_to_u64` (private, same module).
- Produces: `pub fn verify_extra_prefix(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader) -> Result<(), Error>`; `Error::IncorrectFeeState { expected: String, found: String }`, `Error::InvalidExtraPrefix { expected: String, found: String }`. Task 4 calls it second.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ava-evm/tests/feerules.rs`:

```rust
/// coreth `customheader/extra.go:62-110` — `VerifyExtraPrefix`, Fortuna arm:
/// the claimed ACP-176 state must equal `feeStateAfterBlock(parent, header,
/// claimed.TargetExcess)` — passing the CLAIMED target excess means the
/// expectation clamps toward the claim, so any one-step-reachable claim is
/// accepted and anything else mismatches (extra.go:74-84).
#[test]
fn verify_extra_prefix_fortuna_honest_and_tampered() {
    let cs = local_all_active_spec();
    let parent = AvaHeader { number: 1, time: 1_000, extra: acp176_extra(2_000_000, 0, 1_500_000), ..AvaHeader::default() };

    // Honest child: extra prefix = fee_state_after_block with its own target.
    let honest_state = feerules::fee_state_after_block(&cs, &parent, 1_002, Some(1_002_000), 21_000, 0, None)
        .expect("after-block state");
    let child = AvaHeader {
        number: 2,
        time: 1_002,
        time_milliseconds: Some(1_002_000),
        gas_used: 21_000,
        ext_data_gas_used: Some(U256::ZERO),
        extra: honest_state.to_bytes().to_vec().into(),
        ..AvaHeader::default()
    };
    feerules::verify_extra_prefix(&cs, &parent, &child).expect("honest prefix accepted");

    // Tampered: flip a byte inside the excess field (bytes 8..16 of the prefix).
    let mut tampered_extra = child.extra.to_vec();
    tampered_extra[9] ^= 0x01;
    let tampered = AvaHeader { extra: tampered_extra.into(), ..child };
    let err = feerules::verify_extra_prefix(&cs, &parent, &tampered).expect_err("tampered prefix rejected");
    assert!(err.to_string().contains("incorrect fee state"), "sentinel parity: {err}");
}

/// A claimed target excess reachable in one step is accepted (the clamp makes
/// expected == claimed); a claim beyond the per-block step mismatches.
#[test]
fn verify_extra_prefix_target_excess_clamp() {
    let cs = local_all_active_spec();
    let parent = AvaHeader { number: 1, time: 1_000, extra: acp176_extra(2_000_000, 0, 1_500_000), ..AvaHeader::default() };

    // Reachable claim: recompute with a slightly-moved desired target.
    let near = feerules::fee_state_after_block(&cs, &parent, 1_002, Some(1_002_000), 0, 0, Some(1_500_001))
        .expect("near-claim state");
    assert_eq!(near.target_excess.0, 1_500_001, "one-step-reachable claim");
    let child_near = AvaHeader {
        number: 2, time: 1_002, time_milliseconds: Some(1_002_000),
        extra: near.to_bytes().to_vec().into(),
        ..AvaHeader::default()
    };
    feerules::verify_extra_prefix(&cs, &parent, &child_near).expect("reachable claim accepted");

    // Unreachable claim: hand-craft a prefix whose target_excess jumped far
    // beyond one step; the clamped expectation cannot equal it.
    let far = acp176_extra(near.to_bytes()[..8].try_into().map(u64::from_be_bytes).unwrap(), 0, u64::MAX / 2);
    let child_far = AvaHeader { extra: far, ..child_near };
    let err = feerules::verify_extra_prefix(&cs, &parent, &child_far).expect_err("unreachable claim rejected");
    assert!(err.to_string().contains("incorrect fee state"));
}
```

(If the `far` capacity extraction reads awkwardly, just reuse the parent's capacity value `2_000_000` — the mismatch fires on `target_excess` regardless.) Add a third test `verify_extra_prefix_window_arm` using the phase-staggered spec at an AP3-but-pre-Fortuna `header.time`: honest `extra = fee_window(..).to_bytes() ++ trailing bytes` accepted (prefix semantics), first byte flipped → error contains `"invalid header.Extra prefix"`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ava-evm --test feerules verify_extra_prefix 2>&1 | tail -10`
Expected: compile error — `verify_extra_prefix` not found.

- [ ] **Step 3: Implement**

In `crates/ava-evm/src/error.rs`:

```rust
/// coreth `customheader/extra.go:22` `errIncorrectFeeState`
/// ("%w: expected %+v, found %+v") — Fortuna full-struct mismatch.
#[error("incorrect fee state: expected {expected}, found {found}")]
IncorrectFeeState { expected: String, found: String },
/// coreth `customheader/extra.go:21` `errInvalidExtraPrefix`
/// ("%w: expected %x as prefix, found %x") — AP3 window-prefix mismatch.
#[error("invalid header.Extra prefix: expected {expected} as prefix, found {found}")]
InvalidExtraPrefix { expected: String, found: String },
```

In `crates/ava-evm/src/feerules/mod.rs`:

```rust
/// coreth `customheader/extra.go:62-110` — `VerifyExtraPrefix`.
///
/// Fortuna+: the header's claimed ACP-176 fee state (first 24 bytes of
/// `Extra`) must equal `feeStateAfterBlock(parent, header, claimed.
/// TargetExcess)` — the claimed target excess is passed as the desired value
/// so the expectation clamps toward the claim (`extra.go:74-84`); a claim
/// reachable in one step therefore matches exactly, anything else mismatches.
/// `[AP3, Fortuna)`: `Extra` must start with the recomputed fee window's
/// bytes. Pre-AP3: no expected prefix.
///
/// upstream-delta: coreth's `IsHelicon` arm short-circuits Fortuna's check
/// (the ACP-176 state leaves `header.Extra` under Helicon); `AvaPhase` has no
/// Helicon variant yet — fold the arm in when it grows one (same callout as
/// `EvmBlock::syntactic_verify`'s `VerifyExtra` port).
///
/// # Errors
/// [`Error::IncorrectFeeState`] / [`Error::InvalidExtraPrefix`] on mismatch;
/// [`Error::InvalidFeeState`] if the claimed or parent state is unparsable.
pub fn verify_extra_prefix(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let phase = spec.fork_at(header.time);
    if phase >= AvaPhase::Fortuna {
        // extra.go:69-72 — parse the CLAIMED fee state off the header.
        let claimed = Acp176State::from_bytes(&header.extra)
            .map_err(|e| Error::InvalidFeeState(format!("parsing remote fee state: {e}")))?;
        // extra.go:74-87
        let expected = fee_state_after_block(
            spec,
            parent,
            header.time,
            header.time_milliseconds,
            header.gas_used,
            opt_u256_to_u64(header.ext_data_gas_used),
            Some(claimed.target_excess.0),
        )?;
        // extra.go:89-95
        if claimed != expected {
            return Err(Error::IncorrectFeeState {
                expected: format!("{expected:?}"),
                found: format!("{claimed:?}"),
            });
        }
    } else if phase >= AvaPhase::ApricotPhase3 {
        // extra.go:96-108
        let window = fee_window(spec, parent, header.time)?;
        let want = window.to_bytes();
        if !header.extra.starts_with(want.as_slice()) {
            return Err(Error::InvalidExtraPrefix {
                expected: hex::encode(want),
                found: hex::encode(&header.extra),
            });
        }
    }
    Ok(())
}
```

If `hex` is not already a dependency of `ava-evm`'s lib (check `Cargo.toml`; it appears in tests), use `format!("{:x}", ...)`-style encoding via the existing pattern in `error.rs` for byte fields instead — do NOT add a new dependency.

If `Gas`'s inner field is not `.0`-accessible, use its public accessor (check `crates/ava-evm/src/feerules/acp176.rs` — `target_excess` is a `pub` field of a `Copy` struct).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(verify_extra_prefix)' 2>&1 | tail -5`
Expected: 3 PASS.

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p ava-evm --all-targets -- -D warnings && ./scripts/nix_run.sh cargo fmt && cargo fmt --check`

```bash
git add crates/ava-evm
git commit -m "feat(ava-evm): port customheader.VerifyExtraPrefix (extra.go:62-110) as feerules::verify_extra_prefix"
```

---

### Task 4: `feerules::verify_header_gas_fields` orchestrator

**Files:**
- Modify: `crates/ava-evm/src/feerules/mod.rs`
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/tests/feerules.rs`

**Interfaces:**
- Consumes: `verify_gas_limit` (Task 2), `verify_extra_prefix` (Task 3), `base_fee` (Task 1), `parent_fee_state_of`, `blockgas::{block_gas_cost, BLOCK_GAS_COST_STEP_AP4, BLOCK_GAS_COST_STEP_AP5}`, `header_time_ms`.
- Produces: `pub fn verify_header_gas_fields(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader) -> Result<(), Error>` and `pub fn expected_block_gas_cost(spec: &AvaChainSpec, parent: &AvaHeader, timestamp: u64) -> Option<u64>`; `Error::{BaseFeeMismatch, BlockGasCostMismatch, ExtDataGasUsedBeforeFork, NilExtDataGasUsed, ExtDataGasUsedTooLarge}`. Task 5 calls `verify_header_gas_fields` from the verify path.

- [ ] **Step 1: Write the failing tests**

Append to `crates/ava-evm/tests/feerules.rs`:

```rust
/// coreth `consensus/dummy/consensus.go:136-144` — header.BaseFee must equal
/// the recompute; nil-vs-non-nil is unequal BOTH ways (`utils.BigEqual`).
#[test]
fn verify_header_gas_fields_rejects_wrong_base_fee() {
    let cs = local_all_active_spec();
    let (parent, honest) = honest_pair(&cs); // helper below
    feerules::verify_header_gas_fields(&cs, &parent, &honest).expect("honest header accepted");

    let bad = AvaHeader {
        base_fee: honest.base_fee.map(|bf| bf + U256::from(1)),
        ..honest.clone()
    };
    let err = feerules::verify_header_gas_fields(&cs, &parent, &bad).expect_err("wrong base fee rejected");
    assert!(err.to_string().contains("expected base fee"), "sentinel parity: {err}");
}

/// consensus.go:146-156 — BlockGasCost equality (Granite expectation is 0).
#[test]
fn verify_header_gas_fields_rejects_wrong_block_gas_cost() {
    let cs = local_all_active_spec();
    let (parent, honest) = honest_pair(&cs);
    let bad = AvaHeader { block_gas_cost: Some(U256::from(123u64)), ..honest };
    let err = feerules::verify_header_gas_fields(&cs, &parent, &bad).expect_err("wrong block gas cost rejected");
    assert!(err.to_string().contains("invalid block gas cost"), "sentinel parity: {err}");
}

/// consensus.go:158-175 — ExtDataGasUsed fork gating: AP4+ nil and oversize
/// both reject; and (pre-AP3/pre-AP4 arms via the staggered spec) a base fee
/// or block gas cost PRESENT before its fork rejects — nil expectation vs
/// non-nil claim (the Option-equality fail-opens the spec calls out).
#[test]
fn verify_header_gas_fields_ext_data_gas_used_gating() {
    let cs = local_all_active_spec();
    let (parent, honest) = honest_pair(&cs);

    let nil = AvaHeader { ext_data_gas_used: None, ..honest.clone() };
    let err = feerules::verify_header_gas_fields(&cs, &parent, &nil).expect_err("nil at AP4+ rejected");
    assert!(err.to_string().contains("extDataGasUsed is nil"));

    let oversize = AvaHeader {
        ext_data_gas_used: Some(U256::from(u64::MAX) + U256::from(1)),
        ..honest
    };
    let err = feerules::verify_header_gas_fields(&cs, &parent, &oversize).expect_err("oversize rejected");
    assert!(err.to_string().contains("extDataGasUsed is not uint64"));
}

/// Go check order: a header wrong in BOTH gas limit and base fee reports the
/// gas-limit error (the FIRST check), matching Go's first-rejection class.
#[test]
fn verify_header_gas_fields_check_order_matches_go() {
    let cs = local_all_active_spec();
    let (parent, honest) = honest_pair(&cs);
    let doubly_bad = AvaHeader {
        gas_limit: honest.gas_limit + 1,
        base_fee: honest.base_fee.map(|bf| bf + U256::from(1)),
        ..honest
    };
    let err = feerules::verify_header_gas_fields(&cs, &parent, &doubly_bad).expect_err("rejected");
    assert!(err.to_string().contains("invalid gas limit"), "gas limit checks FIRST: {err}");
}
```

`honest_pair` builds a self-consistent (parent, child) at the all-active spec — child fields computed through the SAME feerules functions the builder uses:

```rust
fn honest_pair(cs: &AvaChainSpec) -> (AvaHeader, AvaHeader) {
    let parent = AvaHeader { number: 1, time: 1_000, extra: acp176_extra(2_000_000, 0, 1_500_000), ..AvaHeader::default() };
    let (time, time_ms, gas_used) = (1_002u64, 1_002_000u64, 0u64);
    let state = feerules::fee_state_before_block(cs, &parent, time_ms).expect("pre-block state");
    let after = feerules::fee_state_after_block(cs, &parent, time, Some(time_ms), gas_used, 0, None).expect("after-block state");
    let child = AvaHeader {
        number: 2,
        time,
        time_milliseconds: Some(time_ms),
        gas_used,
        gas_limit: state.max_capacity().0,
        base_fee: Some(U256::from(state.gas_price().0)),
        block_gas_cost: Some(U256::ZERO), // Granite retires the mechanism => 0
        ext_data_gas_used: Some(U256::ZERO),
        extra: after.to_bytes().to_vec().into(),
        ..AvaHeader::default()
    };
    (parent, child)
}
```

Also add pre-fork Option-equality tests using the phase-staggered spec: `pre_ap3_base_fee_present_is_rejected` (header at a pre-AP3 `time` carrying `base_fee: Some(..)` → error contains `"expected base fee"`) and `pre_ap4_block_gas_cost_present_is_rejected` (pre-AP4 `time`, `block_gas_cost: Some(..)` → `"invalid block gas cost"`), plus `pre_ap4_ext_data_gas_used_present_is_rejected` (→ `"invalid extDataGasUsed before fork"`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ava-evm --test feerules verify_header_gas_fields 2>&1 | tail -10`
Expected: compile error — `verify_header_gas_fields` not found.

- [ ] **Step 3: Implement**

In `crates/ava-evm/src/error.rs`:

```rust
/// coreth `consensus/dummy/consensus.go:142-144`
/// ("expected base fee %d, found %d"; nil renders as "<nil>").
#[error("expected base fee {expected:?}, found {found:?}")]
BaseFeeMismatch { expected: Option<U256>, found: Option<U256> },
/// coreth `consensus/dummy/consensus.go:153-155`
/// ("invalid block gas cost: have %d, want %d").
#[error("invalid block gas cost: have {have:?}, want {want:?}")]
BlockGasCostMismatch { have: Option<U256>, want: Option<U256> },
/// coreth `consensus/dummy/consensus.go:160-162`
/// ("invalid extDataGasUsed before fork: have %d, want <nil>").
#[error("invalid extDataGasUsed before fork: have {0}, want <nil>")]
ExtDataGasUsedBeforeFork(U256),
/// coreth `consensus/dummy/consensus.go:28` `errExtDataGasUsedNil`.
#[error("extDataGasUsed is nil")]
NilExtDataGasUsed,
/// coreth `consensus/dummy/consensus.go:29` `errExtDataGasUsedTooLarge`.
#[error("extDataGasUsed is not uint64")]
ExtDataGasUsedTooLarge(U256),
```

In `crates/ava-evm/src/feerules/mod.rs`:

```rust
/// coreth `customheader/block_gas_cost.go:31-59` — `BlockGasCost`, the
/// fork-gated wrapper over [`blockgas::block_gas_cost`]: `None` pre-AP4,
/// `Some(0)` at Granite, else the AP4/AP5-stepped cost off the parent.
#[must_use]
pub fn expected_block_gas_cost(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    timestamp: u64,
) -> Option<u64> {
    // block_gas_cost.go:36-38
    if !spec.is_apricot_phase4(timestamp) {
        return None;
    }
    // block_gas_cost.go:42-45
    let step = if spec.is_apricot_phase5(timestamp) {
        BLOCK_GAS_COST_STEP_AP5
    } else {
        BLOCK_GAS_COST_STEP_AP4
    };
    // block_gas_cost.go:46-53 — an invalid parent/current time combination
    // counts as 0 elapsed time.
    let time_elapsed = timestamp.saturating_sub(parent.time);
    Some(blockgas::block_gas_cost(
        parent
            .block_gas_cost
            .map(|c| u64::try_from(c).unwrap_or(u64::MAX)),
        step,
        time_elapsed,
        spec.is_granite(timestamp),
    ))
}

/// coreth `consensus/dummy/consensus.go:125-176` — `verifyHeaderGasFields`.
///
/// The contextual (parent-dependent) fee/gas equality checks that complement
/// the parent-less structural checks in `EvmBlock::syntactic_verify` — coreth
/// keeps both layers, and so do we. Checks run in Go's order so a multi-fault
/// header reports Go's first rejection class. Go's `VerifyGasUsed` is NOT
/// called here (same comment as consensus.go:126-127): gas-used correctness
/// is checked by execution (`EvmBlock::verify` asserts executed gas ==
/// `header.gas_used`).
///
/// # Errors
/// The first failing check's error (see the per-check variants); recompute
/// failures propagate as [`Error::InvalidFeeState`] / [`Error::NilBaseFee`].
pub fn verify_header_gas_fields(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    // consensus.go:128-131
    verify_gas_limit(spec, parent, header)?;
    // consensus.go:132-134
    verify_extra_prefix(spec, parent, header)?;

    // consensus.go:136-144 — expected base fee via the SAME `base_fee` the
    // builder stamps with (nil pre-AP3; `utils.BigEqual` treats nil-vs-non-nil
    // as unequal in both directions). The dispatch timestamp is
    // `timeMS / 1000` in Go; `header.time == header_time_ms(header) / 1000`
    // by construction, so `header.time` is the same value.
    let phase = spec.fork_at(header.time);
    let expected_base_fee = if phase >= AvaPhase::ApricotPhase3 {
        let ctx = AvaNextBlockCtx {
            timestamp: header.time,
            timestamp_ms: header_time_ms(header),
            parent_fee_state: parent_fee_state_of(spec, parent)?,
            ..AvaNextBlockCtx::default()
        };
        Some(U256::from(base_fee(spec, parent, &ctx)?))
    } else {
        None
    };
    if header.base_fee != expected_base_fee {
        return Err(Error::BaseFeeMismatch {
            expected: expected_base_fee,
            found: header.base_fee,
        });
    }

    // consensus.go:146-156 — BlockGasCost equality (BigEqual: nil==nil).
    let want = expected_block_gas_cost(spec, parent, header.time).map(U256::from);
    if header.block_gas_cost != want {
        return Err(Error::BlockGasCostMismatch { have: header.block_gas_cost, want });
    }

    // consensus.go:158-175 — ExtDataGasUsed fork gating.
    if phase < AvaPhase::ApricotPhase4 {
        if let Some(v) = header.ext_data_gas_used {
            return Err(Error::ExtDataGasUsedBeforeFork(v));
        }
        return Ok(());
    }
    match header.ext_data_gas_used {
        None => Err(Error::NilExtDataGasUsed),
        Some(v) if v > U256::from(u64::MAX) => Err(Error::ExtDataGasUsedTooLarge(v)),
        Some(_) => Ok(()),
    }
}
```

Import `AvaNextBlockCtx` from `crate::evmconfig` if not already in scope (check the module's imports; `base_fee` already takes it, so it is).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(verify_header_gas_fields) or test(pre_ap3_base_fee) or test(pre_ap4)' 2>&1 | tail -5`
Expected: all PASS.

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p ava-evm --all-targets -- -D warnings && ./scripts/nix_run.sh cargo fmt && cargo fmt --check`

```bash
git add crates/ava-evm
git commit -m "feat(ava-evm): port verifyHeaderGasFields (consensus/dummy/consensus.go:125-176) — fee/gas header equality checks"
```

---

### Task 5: Thread the parent header into the verify path

**Files:**
- Modify: `crates/ava-evm/src/vm.rs` (`Shared` impl ~:259-285; `VerifiedEvmBlock::verify` :154-160)
- Modify: `crates/ava-evm/src/block.rs` (`verify` :678, `verify_with_predicates` :913)
- Modify (compile-driven sweep): `crates/ava-evm/tests/{build.rs,lifecycle.rs,g1_invariant.rs,proposer_candidates.rs,cchain_state_root.rs,...}` — every direct `EvmBlock::verify(&ctx, root)` caller
- Create: `crates/ava-evm/tests/verify_gas_fields.rs`

**Interfaces:**
- Consumes: `feerules::verify_header_gas_fields` (Task 4).
- Produces: `EvmBlock::verify(&self, ctx: &EvmBlockContext, parent_state_root: B256, parent: &AvaHeader) -> Result<B256>`; `EvmBlock::verify_with_predicates(&self, ctx, parent_state_root: B256, parent: &AvaHeader, exec_ctx: &AvaExecCtx) -> Result<B256>`; `Shared::parent_header(&self, parent: Id) -> ava_snow::Result<AvaHeader>`. Task 6's corpus reader exercises this path through `EvmVm::parse_block → Block::verify`.

- [ ] **Step 1: Write the failing test**

Create `crates/ava-evm/tests/verify_gas_fields.rs` (license header; model the harness on `crates/ava-evm/tests/cancun_clamp.rs` — read it first; it builds an `EvmVm::from_genesis`, decodes/mutates/re-encodes a block via `AvaBlockParts`, and drives `vm.parse_block → blk.verify(&token)`):

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end guard for the `verifyHeaderGasFields` port (coreth
//! `consensus/dummy/consensus.go:125-176`): the FULL `ChainVm` verify entry
//! (`EvmVm::parse_block` → `Block::verify`) must reject a block whose fee/gas
//! header fields disagree with the parent-derived recompute, even though its
//! state root is valid — the Byzantine-proposer fail-open this port closes.

// (imports + parse_and_verify + mutate/re-encode helpers: copy the exact
// pattern from cancun_clamp.rs — same genesis, same helper shapes.)

#[tokio::test]
async fn wrong_base_fee_is_rejected_by_full_verify() {
    // 1. Build an honest block on genesis (the cancun_clamp.rs build recipe).
    // 2. Mutate: header.base_fee = Some(base_fee + 1); re-encode (hash recomputed).
    // 3. parse_and_verify(honest)  => Ok
    // 4. parse_and_verify(mutated) => Err containing "expected base fee"
}

#[tokio::test]
async fn wrong_gas_limit_is_rejected_by_full_verify() {
    // Same shape; header.gas_limit += 1; expect Err containing "invalid gas limit".
}
```

Fill the bodies concretely from the `cancun_clamp.rs` helpers — the two tests are the same mutate+re-encode pattern with different fields and sentinels. Do not leave the comments as the implementation; the comments above describe what the copied helper calls do.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ava-evm --test verify_gas_fields 2>&1 | tail -10`
Expected: `wrong_base_fee_is_rejected_by_full_verify` FAILS — the mutated block is ACCEPTED (verify has no fee-field equality checks yet). This is the fail-open, demonstrated.

- [ ] **Step 3: Implement the threading**

`crates/ava-evm/src/vm.rs` — add to `impl Shared` (after `parent_state_root`):

```rust
/// The parent block's coreth header (for the contextual
/// `verifyHeaderGasFields` checks). Same resolution set as
/// [`Shared::parent_state_root`]: accepted blocks are retained in `verified`
/// for resolvability (see `build_block`), so both the committed tip and
/// processing parents resolve here; a parent evicted from the tree falls back
/// to the canonical store's block bytes (restart path).
fn parent_header(&self, parent: Id) -> ava_snow::Result<AvaHeader> {
    if let Some(pb) = self.verified.get(&parent) {
        return Ok(pb.block.header().clone());
    }
    // Fallback: decode the accepted block's bytes out of the blocks db
    // (mirror the exact decode `EvmVm::parse_block` uses — read that fn).
    let number = self
        .blocks
        .height_of(hash_of(parent))
        .map_err(Error::from)?
        .ok_or(Error::MissingProposal(hash_of(parent)))?;
    let bytes = self
        .blocks
        .body_at(number)
        .map_err(Error::from)?
        .ok_or(Error::MissingProposal(hash_of(parent)))?;
    let block = /* same decode parse_block uses */;
    Ok(block.header().clone())
}
```

(Adjust the fallback to the real `CanonicalStore` + decode API — `height_of`/`body_at` exist (`canonical.rs:169/:189`); the decode entry is whatever `EvmVm::parse_block` calls. If the stored body bytes are not the full block encoding, keep ONLY the `verified`-map branch and return `MissingProposal` otherwise — that matches `build_block`'s resolution contract and every current caller; note it in the doc comment.)

`VerifiedEvmBlock::verify` (vm.rs:154-160):

```rust
let parent_root = self.shared.parent_state_root(self.parent)?;
let parent_header = self.shared.parent_header(self.parent)?;
let precommit = self
    .block
    .verify(&self.ctx, parent_root, &parent_header)
    .map_err(ava_snow::Error::from)?;
```

`crates/ava-evm/src/block.rs`:

```rust
pub fn verify(&self, ctx: &EvmBlockContext, parent_state_root: B256, parent: &AvaHeader) -> Result<B256> {
    self.verify_with_predicates(ctx, parent_state_root, parent, &AvaExecCtx::default())
}

pub fn verify_with_predicates(
    &self,
    ctx: &EvmBlockContext,
    parent_state_root: B256,
    parent: &AvaHeader,
    exec_ctx: &AvaExecCtx,
) -> Result<B256> {
    self.syntactic_verify(ctx.chain_spec())?;

    // Contextual fee/gas header equality checks (coreth dummy-engine
    // `verifyHeaderGasFields`, consensus/dummy/consensus.go:125-176) — the
    // parent-dependent complement to the parent-less `syntactic_verify`;
    // runs before any execution work, like Go's header verification.
    crate::feerules::verify_header_gas_fields(ctx.chain_spec(), parent, self.header())?;

    // ... existing body unchanged ...
}
```

Check whether `verify_with_predicates` has other direct callers (the `ChainVm` adapter's predicate path): `grep -rn "verify_with_predicates" crates/ | grep -v block.rs` — thread the parent header the same way at each (the adapter resolves it via `Shared::parent_header`).

- [ ] **Step 4: Compile-driven caller sweep**

Run: `cargo build -p ava-evm --tests 2>&1 | grep -E "^error" | head -30`

Fix every `EvmBlock::verify(&ctx, root)` call site by passing the parent header the test already holds — in the builder-driven tests that is `&genesis_header` (e.g. `build.rs:282/:584`, `g1_invariant.rs:252`, `lifecycle.rs:204/:219/:251`, `proposer_candidates.rs:279`); chained-block tests pass the previous block's `.header()`. Repeat the build until clean, then check other workspace crates: `cargo build --workspace --tests 2>&1 | grep -E "^error" | head` (the `ChainVm` adapter and `ava-chains` reach `verify` only through `VmBlock::verify`, which changed internally, so no ripple is expected — verify that).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm 2>&1 | tail -5`
Expected: full crate PASS, including both new `verify_gas_fields` tests (the mutated blocks now reject with the ported sentinels) and every pre-existing lifecycle/build/clamp/syntactic test (honest blocks stamp exactly what verify recomputes — builder and verifier share the same functions).

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p ava-evm --all-targets -- -D warnings && ./scripts/nix_run.sh cargo fmt && cargo fmt --check`

```bash
git add crates/ava-evm
git commit -m "feat(ava-evm): run verifyHeaderGasFields on the verify path — parent header threaded via Shared::parent_header"
```

---

### Task 6: Go-oracle verdict corpus — fee-field mutations

Extends the recorded proposer-verdict corpus (`crates/ava-evm/tests/proposer_candidates.rs` + `tests/vectors/proposer_verdict/`) with five fee-field mutations: Go REJECTS each and Rust now REJECTS the identical bytes with the matched class; the honest block still ACCEPTs both sides. Requires the live Go oracle (`~/avalanchego`).

**Files:**
- Modify: `crates/ava-evm/tests/proposer_candidates.rs` (`MUTATIONS` :109, `REJECTION_CLASSES` :143)
- Modify (regenerated): `crates/ava-evm/tests/vectors/proposer_verdict/` (new `.rlp.hex` files + `verdicts.json`)

**Interfaces:**
- Consumes: the Task 5 verify path (Rust-side verdicts run through `parse_and_verify` → `EvmVm::parse_block` → `Block::verify`); the existing `mutate_candidate` + emit/judge/read pipeline (module doc lines 15-46 documents the 3-step flow).
- Produces: committed corpus files `wrong_base_fee.rlp.hex`, `wrong_block_gas_cost.rlp.hex`, `tampered_fee_state_prefix.rlp.hex`, `wrong_gas_limit.rlp.hex`, `oversized_ext_data_gas_used.rlp.hex` + their `verdicts.json` entries.

**Scope note (spec deviation, intentional):** the spec lists an "ext-data-gas-used present pre-AP4" corpus mutation; the corpus genesis has every fork active, so pre-fork arms are unreachable here — they are covered by Task 4's unit tests (`pre_ap3_base_fee_present_is_rejected` etc.). The AP4+ `oversized_ext_data_gas_used` mutation stands in as the corpus-level ExtDataGasUsed leg.

- [ ] **Step 1: Extend the mutation + rejection-class tables**

In `crates/ava-evm/tests/proposer_candidates.rs`, grow `MUTATIONS` from `[Mutation; 5]` to `[Mutation; 10]`:

```rust
    // ── verifyHeaderGasFields legs (consensus/dummy/consensus.go:125-176):
    // each corrupts exactly ONE fee/gas equality check, leaving every
    // earlier-checked field untouched so the first rejection is the intended
    // one (order: GasLimit → ExtraPrefix → BaseFee → BlockGasCost → ExtData).
    ("wrong_gas_limit", |p| {
        p.header.gas_limit = p.header.gas_limit.saturating_add(1)
    }),
    ("tampered_fee_state_prefix", |p| {
        let mut extra = p.header.extra.to_vec();
        extra[9] ^= 0x01; // flip a bit inside the ACP-176 `excess` field
        p.header.extra = extra.into();
    }),
    ("wrong_base_fee", |p| {
        p.header.base_fee = p.header.base_fee.map(|bf| bf + U256::from(1))
    }),
    ("wrong_block_gas_cost", |p| {
        p.header.block_gas_cost = Some(U256::from(123u64))
    }),
    ("oversized_ext_data_gas_used", |p| {
        p.header.ext_data_gas_used = Some(U256::from(u64::MAX) + U256::from(1))
    }),
```

Grow `REJECTION_CLASSES` to `[(&str, &str, &str); 10]` (Go substring from the coreth sentinels, Rust substring from the Task 2-4 `error.rs` messages):

```rust
    ("wrong_gas_limit", "invalid gas limit", "invalid gas limit"),
    ("tampered_fee_state_prefix", "incorrect fee state", "incorrect fee state"),
    ("wrong_base_fee", "expected base fee", "expected base fee"),
    ("wrong_block_gas_cost", "invalid block gas cost", "invalid block gas cost"),
    (
        "oversized_ext_data_gas_used",
        "extDataGasUsed is not uint64",
        "extDataGasUsed is not uint64",
    ),
```

- [ ] **Step 2: Re-record the corpus (Rust emit → Go judge)**

Pre-gate: `./scripts/check_oracle_binary.sh` must print `OK`.

Follow the module's documented 3-step flow exactly (module doc, `proposer_candidates.rs` lines ~15-46, and `tests/differential/go-oracle/README.md`):

```bash
# 1. Emit candidates (honest + 10 mutations) into the corpus dir:
EMIT_PROPOSER_CANDIDATES=$PWD/crates/ava-evm/tests/vectors/proposer_verdict \
  cargo test -p ava-evm --test proposer_candidates -- --exact emit_proposer_candidates --nocapture

# 2. Judge with real coreth (per go-oracle/README.md — the judge test file is
#    tests/differential/go-oracle/rust_built_block_verdict_test.go):
#    copy it to its documented location under ~/avalanchego, then:
cd ~/avalanchego && RUST_BLOCK_VERDICT_DIR=<repo>/crates/ava-evm/tests/vectors/proposer_verdict \
  go test ./graft/coreth/... -run TestRustBuiltBlockVerdict -count=1 -v
# (exact package path + test name per the README — follow it, do not guess)
```

Expected `verdicts.json` after the judge run: `honest` accepted; all 10 mutations rejected; each new rejection's `error` contains its Go substring. If a Go error text differs from the table's substring, update the SUBSTRING to match the recorded Go text (the Go source sentinel is authoritative: `errInvalidGasLimit`="invalid gas limit", `errIncorrectFeeState`="incorrect fee state", base fee="expected base fee %d, found %d", block gas cost="invalid block gas cost: have %d, want %d", `errExtDataGasUsedTooLarge`="extDataGasUsed is not uint64") — do NOT weaken a substring to paper over an unexpected class; if Go rejects at a different check than intended, re-examine the mutation.

- [ ] **Step 3: Run the per-PR reader to verify both sides hold**

Run: `cargo nextest run -p ava-evm -E 'test(proposer_verdicts_hold)' 2>&1 | tail -5`
Expected: PASS — Go verdict and Rust verdict agree on all 11 candidates, matched classes.

- [ ] **Step 4: Commit**

```bash
git add crates/ava-evm/tests/proposer_candidates.rs crates/ava-evm/tests/vectors/proposer_verdict/
git commit -m "test(ava-evm): verdict corpus grows 5 verifyHeaderGasFields mutations — Go and Rust reject identically"
```

---

### Task 7: Go-recorded base-fee time-advance vectors (false-reject guard)

The regression proof for Task 1: honest Go-computed base fees under **nonzero excess** must match Rust's recompute exactly — the case where the old raw-parent read diverges (and where verify would otherwise falsely reject honest Go blocks under load). Function-level recorded vectors (the M6.11 `fees/` vector pattern); the live re-run in Task 8 covers the block-level end.

**Files:**
- Create: `tests/differential/go-oracle/base_fee_advance_emitter_test.go`
- Create (recorded): `crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json`
- Modify: `crates/ava-evm/tests/feerules.rs` (reader test)

**Interfaces:**
- Consumes: `feerules::base_fee` (Task 1 signature), Go `customheader.BaseFee(config, parent, timeMS)`.
- Produces: the committed vector file; test `acp176_base_fee_advance_matches_go_vectors`.

- [ ] **Step 1: Write the Go emitter**

`tests/differential/go-oracle/base_fee_advance_emitter_test.go` — follow the build/env conventions of the sibling emitters (read `streaming_vector_emitter_test.go` for the env-gate + JSON-write shape and the README for where it runs). Core logic:

```go
// Env-gated: BASE_FEE_ADVANCE_OUT=<abs path to base_fee_advance.json>.
// Uses a ChainConfig with Fortuna+Granite active at genesis (reuse the config
// the rust_built_block_verdict_test.go judge builds from the committed
// proposer_verdict/genesis.json — same fork shape as the Rust local spec).
func TestEmitBaseFeeAdvanceVectors(t *testing.T) {
    out := os.Getenv("BASE_FEE_ADVANCE_OUT")
    if out == "" { t.Skip("BASE_FEE_ADVANCE_OUT not set") }
    config := ... // as above

    type row struct {
        ParentNumber  uint64 `json:"parent_number"`
        ParentTime    uint64 `json:"parent_time"`
        ParentExtra   string `json:"parent_extra_hex"` // 24-byte ACP-176 prefix
        ChildTimeMS   uint64 `json:"child_time_ms"`
        ExpectedBaseFee uint64 `json:"expected_base_fee"`
    }
    var rows []row
    parentTime := uint64(1_000)
    for _, excess := range []uint64{0, 1_000_000, 50_000_000, 200_000_000, 2_000_000_000} {
        for _, deltaMS := range []uint64{0, 500, 1_000, 10_000, 60_000, 600_000} {
            extra := acp176.State{ /* capacity 2_000_000, excess, targetExcess 1_500_000 — use the real field/ctor names from graft/coreth's acp176 package */ }.Bytes()
            parent := &types.Header{Number: big.NewInt(1), Time: parentTime, Extra: extra}
            childMS := parentTime*1000 + deltaMS
            bf, err := customheader.BaseFee(config, parent, childMS)
            require.NoError(t, err)
            rows = append(rows, row{1, parentTime, hex.EncodeToString(extra), childMS, bf.Uint64()})
        }
    }
    // write {"_comment": ..., "go_commit": <HEAD>, "rows": rows} to out
}
```

The sub-second `deltaMS` values (500) exercise the Granite millisecond-granularity advance; the 60s/600s rows exercise deep drains. Resolve the exact `acp176.State` construction + `Bytes()` API from `~/avalanchego/graft/coreth` source before writing (grep `package acp176`).

- [ ] **Step 2: Record the vectors**

Pre-gate: `./scripts/check_oracle_binary.sh` prints `OK`. Copy the emitter to its documented run location (README), then:

```bash
cd ~/avalanchego && BASE_FEE_ADVANCE_OUT=<repo>/crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json \
  go test <package per README> -run TestEmitBaseFeeAdvanceVectors -count=1 -v
```

Expected: the JSON lands with 30 rows; spot-check that rows with `excess > 0` and `delta_ms > 0` have a LOWER `expected_base_fee` than their `delta_ms == 0` sibling (the advance drains excess).

- [ ] **Step 3: Write the failing Rust reader test**

Append to `crates/ava-evm/tests/feerules.rs`:

```rust
/// Go-recorded false-reject guard for the Task-1 time-advance: for every
/// recorded (parent fee state, elapsed) — including NONZERO excess, where the
/// old raw-parent read diverged — Rust's `base_fee` must equal what coreth's
/// `customheader.BaseFee` computed. A mismatch here means the verify path
/// would falsely reject an honest Go block under load.
#[test]
fn acp176_base_fee_advance_matches_go_vectors() {
    let raw = include_str!("vectors/cchain/fees/acp176/base_fee_advance.json");
    let doc: Value = serde_json::from_str(raw).expect("parse advance vectors");
    let cs = local_all_active_spec();
    let rows = doc["rows"].as_array().expect("rows array");
    assert!(rows.len() >= 30, "corpus must not silently shrink");
    for row in rows {
        let parent = AvaHeader {
            number: row["parent_number"].as_u64().expect("parent_number"),
            time: row["parent_time"].as_u64().expect("parent_time"),
            extra: hex::decode(row["parent_extra_hex"].as_str().expect("extra"))
                .expect("decode extra")
                .into(),
            ..AvaHeader::default()
        };
        let child_ms = row["child_time_ms"].as_u64().expect("child_time_ms");
        let ctx = AvaNextBlockCtx {
            timestamp: child_ms / 1000,
            timestamp_ms: child_ms,
            parent_fee_state: feerules::parent_fee_state_of(&cs, &parent).expect("fee state"),
            ..AvaNextBlockCtx::default()
        };
        assert_eq!(
            feerules::base_fee(&cs, &parent, &ctx).expect("base_fee"),
            row["expected_base_fee"].as_u64().expect("expected_base_fee"),
            "row {row:?}"
        );
    }
}
```

- [ ] **Step 4: Run the reader**

Run: `cargo nextest run -p ava-evm -E 'test(acp176_base_fee_advance_matches_go_vectors)' 2>&1 | tail -5`
Expected: PASS on the Task-1 code. Sanity-check the guard is real: temporarily revert `base_fee`'s ACP-176 arm to the raw read (`AvaFeeState::Acp176(state) => Ok(state.gas_price().0)` off `ctx.parent_fee_state`) and re-run — it must FAIL on the nonzero-excess rows; restore.

- [ ] **Step 5: Commit**

```bash
git add tests/differential/go-oracle/base_fee_advance_emitter_test.go \
        crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json \
        crates/ava-evm/tests/feerules.rs
git commit -m "test(ava-evm): Go-recorded ACP-176 base-fee time-advance vectors — nonzero-excess false-reject guard"
```

---

### Task 8: Docs, PORTING rows, workspace closeout, live operator gate

**Files:**
- Modify: `crates/ava-evm/tests/PORTING.md`
- Modify: `plan/M9-interop-hardening.md` (AS-BUILT addendum)
- Modify: `specs/10-cchain-evm-reth.md` (as-built callout)
- Modify: `docs/superpowers/specs/2026-07-18-verify-header-gas-fields-design.md` (status line → implemented)

**Interfaces:** none new — documentation + gates over Tasks 1-7.

- [ ] **Step 1: PORTING.md + docs**

In `crates/ava-evm/tests/PORTING.md`, add/flip rows (follow the table's existing format) for: `verifyHeaderGasFields` (consensus/dummy/consensus.go:125-176) → ✅, `VerifyGasLimit` (customheader/gas_limit.go:101-145) → ✅, `VerifyExtraPrefix` (customheader/extra.go:62-110) → ✅, `BaseFee` time-advance (customheader/base_fee.go:27-33) → ✅ noting the AvaHeader signature.

In `plan/M9-interop-hardening.md`, append an AS-BUILT note under the 2026-07-18 addendum: the flagged `verifyHeaderGasFields` fail-open is CLOSED (this plan); the Helicon arm of `VerifyExtraPrefix` remains a documented upstream-delta (no `AvaPhase::Helicon` yet).

In `specs/10-cchain-evm-reth.md`, add the as-built callout to the block-verification section: verify now runs the contextual `verify_header_gas_fields` after `syntactic_verify`; `base_fee` is time-advance-correct (byte-parity guarded by the recorded advance vectors + verdict corpus).

Flip the design spec's `**Status:**` line to `Implemented (see docs/superpowers/plans/2026-07-18-verify-header-gas-fields.md)`.

- [ ] **Step 2: Full-workspace closeout gates**

```bash
./scripts/run_task.sh lint-all
./scripts/run_task.sh test-unit
```

Expected: both green (the SDD lesson: per-task scoped clippy misses repo-wide rustfmt drift and `--profile ci` parallel races — this catches them). Fix anything surfaced before committing.

- [ ] **Step 3: Commit docs**

```bash
git add crates/ava-evm/tests/PORTING.md plan/M9-interop-hardening.md specs/10-cchain-evm-reth.md docs/superpowers/specs/2026-07-18-verify-header-gas-fields-design.md
git commit -m "docs: verifyHeaderGasFields port AS-BUILT — PORTING rows, plan + spec 10 callouts"
```

- [ ] **Step 4: Live operator gate (needs $AVALANCHEGO_PATH + built Go node + relinked release binary)**

```bash
./scripts/check_oracle_binary.sh                      # must print OK
touch crates/ava-evm/src/lib.rs                       # stale-binary gotcha: force relink
cargo build -p avalanchers --release
./target/release/avalanchers --version                # PREWARM (macOS first-exec ~40s stall)
export TMPDIR=/tmp/live-vhgf && mkdir -p $TMPDIR      # stable log dir (nix temp_dir is ephemeral)
# Follower arm (no regression), then proposer arm (builder still stamps what verify accepts):
cargo test --test mixed_network --features live -- --ignored --exact --nocapture mixed_network
cargo test --test mixed_network --features live -- --ignored --exact --nocapture mixed_network_rust_proposes
```

(Run from `tests/differential/`'s owning package as the M9.15 runs did — `cargo test`, NOT nextest: nextest's 120s slow-timeout kills live arms. Both PASS = the gate. If the operator defers this, record it as the pending nightly leg in the plan AS-BUILT note — do not claim it ran.)

---

## Self-Review (performed at write time)

- **Spec coverage:** base_fee time-advance → Task 1 + Task 7 (recorded guard); verify_gas_limit → Task 2; verify_extra_prefix (incl. clamp trick + Helicon callout) → Task 3; orchestrator + Option-equality fail-opens + ExtDataGasUsed gating + check order → Task 4; parent threading + Shared::parent_header + Go ordering → Task 5; verdict-corpus mutations → Task 6 (pre-AP4 corpus mutation intentionally replaced by unit tests — deviation documented in-task); live re-run + PORTING/specs/plan updates + workspace gates → Task 8.
- **Placeholder scan:** Task 5 Step 1 and Task 7 Step 1 intentionally point at existing in-repo patterns (`cancun_clamp.rs`, sibling emitters) for harness boilerplate rather than inlining ~100 lines of copied setup — the novel logic (mutations, assertions, sentinels, row schema) is fully specified. Task 5 Step 3's fallback branch gives the concrete API names and an explicit simplification rule if the decode entry doesn't fit.
- **Type consistency:** `verify_*` all take `(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader)`; `base_fee`/`gas_limit` take `(cs, parent: &AvaHeader, ctx)` after Task 1 and every later task uses that; error variants named identically in Tasks 2-4 definitions and Task 5/6 assertions.

# C-Chain Semantic-Verify Family Port — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port Go's C-Chain `semanticVerify` stage (`~/avalanchego/graft/coreth/plugin/evm/wrapped_block.go:335-391`) — `VerifyMinDelayExcess`, `VerifyTime`, `verifyIntrinsicGas`/`VerifyGasUsed`, and the atomic-extension `ExtDataGasUsed` value check — closing the four Byzantine-proposer fail-opens enumerated in the `verifyHeaderGasFields` final review (`d41cee0`).

**Architecture:** New pure functions in `crates/ava-evm/src/feerules/mod.rs` file-mapped to `customheader/{time,min_delay_excess,gas_limit}.go`, an `EvmBlock::verify_intrinsic_gas` method, and an `atomic::verify::verify_ext_data_gas_used`, all called from a new semantic stage inside `EvmBlock::verify_with_predicates` in Go's exact order. `now_ms` and a live `bootstrapped` flag thread from `Shared` (clock moves into `Shared` behind a `parking_lot::Mutex<Arc<dyn Clock>>`; `bootstrapped` is an `AtomicBool` that `set_state` writes). The SAE-only header fields (`TargetExponent`/`MinPriceExponent`/`Settled*`) need no port — the RLP decoder fail-closes on trailing tail fields (`block.rs:250-252`); they get an equivalence test + PORTING rows.

**Tech Stack:** Rust workspace crate `ava-evm`; recorded Go-oracle corpus at `crates/ava-evm/tests/vectors/proposer_verdict/` (emitter `tests/proposer_candidates.rs` + Go judge `tests/differential/go-oracle/rust_built_block_verdict_test.go` run against `~/avalanchego`).

**Design spec:** `docs/superpowers/specs/2026-07-19-cchain-semantic-verify-family-design.md`

## Global Constraints

- License header on every touched `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` + `// See the file LICENSE for licensing terms.`
- No `unwrap()`/`expect()`/`todo!` in library code (clippy denies). `unwrap_or`/`ok_or` are fine.
- All arithmetic on gas/time quantities `checked_*`/`saturating_*` (`arithmetic_side_effects`).
- Error sentinels carry **verbatim Go message fragments**; every new function's doc comment cites its exact Go source file:line.
- Preserve **Go check order** — the first sentinel a doubly-invalid block hits should match Go wherever it costs nothing.
- TDD: write the failing test, see it fail, implement, see it pass, commit.
- Run tests: `cargo nextest run -p ava-evm -E 'test(<name>)'`; full crate: `cargo nextest run -p ava-evm`. Format/lint at task end: `./scripts/nix_run.sh cargo fmt` and `cargo clippy -p ava-evm --all-targets -- -D warnings`.
- Before any oracle recording: `./scripts/check_oracle_binary.sh` must print `OK`.
- Commit after every green step; message style `feat(ava-evm): …` / `test(ava-evm): …` / `docs: …`.

## Go source ↔ Rust target map

| Go | Rust |
|---|---|
| `customheader/time.go:55-124` `VerifyTime` | `feerules::verify_time` (new) |
| `customheader/min_delay_excess.go:45-81` `VerifyMinDelayExcess` | `feerules::verify_min_delay_excess` (new) |
| `customheader/gas_limit.go:63-98` `VerifyGasUsed`, `:164-180` `GasCapacity` | `feerules::verify_gas_used`, `feerules::gas_capacity` (new) |
| `wrapped_block.go:287-332` `verifyIntrinsicGas` | `EvmBlock::verify_intrinsic_gas` (new) |
| libevm `core.IntrinsicGas` | `mempool::intrinsic_gas` (**exists**, promote to `pub(crate)`) |
| `atomic/{import,export}_tx.go` `GasUsed(fixedFee)` | `atomic::tx::Tx::gas_used` (new) |
| `atomic/vm/block_extension.go:142-177` `SemanticVerify` ExtData check | `atomic::verify::verify_ext_data_gas_used` (new) |
| `customheader/{target_exponent,min_price_exponent,settled}.go` | no port — parse fail-close equivalence (Task 7) |

Existing building blocks (verified present): `feerules::{header_time_ms, min_delay_excess_of, fee_state_before_block, fee_state_after_block, verify_header_gas_fields}`, `feerules::acp226::{DelayExcess, INITIAL_DELAY_EXCESS}` (`DelayExcess(u64)` newtype with `.delay()` and `.update()`), `AvaPhase` ordering + `spec.{fork_at,is_fortuna,is_granite}`, `Acp176State { gas: GasState { capacity: Gas, .. }, .. }` (capacity as `state.gas.capacity.0`), atomic constants `atomic::tx::{TX_BYTES_GAS, COST_PER_SIGNATURE}`, `ava_utils::clock::{Clock, RealClock}`.

---

### Task 1: `feerules::verify_time`

**Files:**
- Modify: `crates/ava-evm/src/error.rs` (new variants)
- Modify: `crates/ava-evm/src/feerules/mod.rs` (const + function + tests; make `header_time_ms` `pub(crate)`)

**Interfaces:**
- Produces: `pub fn verify_time(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader, now_ms: u64) -> Result<(), Error>`; `pub const MAX_FUTURE_BLOCK_TIME_MS: u64 = 10_000;`; `pub(crate) fn header_time_ms(&AvaHeader) -> u64` (visibility bump only). Task 4 wires the call; Task 4's `verify()` wrapper also calls `header_time_ms`.

- [ ] **Step 1: Write the failing unit tests**

Append a new test module at the end of `crates/ava-evm/src/feerules/mod.rs` (below `fee_state_tests`). Copy the `spec_from` + `hdr` helpers from `fee_state_tests` verbatim (test convention is repeat-don't-import):

```rust
#[cfg(test)]
mod semantic_verify_tests {
    use ava_evm_reth::{Address, B256, Bytes, U256, keccak256, Chain};

    use super::{MAX_FUTURE_BLOCK_TIME_MS, verify_time};
    use crate::block::AvaHeader;
    use crate::chainspec::{AvaChainSpec, NetworkUpgrades};
    use crate::error::Error;
    use crate::feerules::acp226::{DelayExcess, INITIAL_DELAY_EXCESS};

    // Repeat of fee_state_tests::spec_from (test convention: repeat-don't-import).
    fn spec_from(fortuna: u64, granite: u64, ap3: u64) -> AvaChainSpec {
        const FF: u64 = u64::MAX;
        let upgrades = NetworkUpgrades {
            apricot_phase_1: 0,
            apricot_phase_2: 0,
            apricot_phase_3: ap3,
            apricot_phase_4: ap3,
            apricot_phase_5: ap3,
            apricot_phase_pre_6: ap3,
            apricot_phase_6: ap3,
            apricot_phase_post_6: ap3,
            banff: ap3,
            cortina: ap3,
            durango: ap3,
            etna: fortuna.min(granite),
            fortuna,
            granite,
            helicon: FF,
        };
        AvaChainSpec::from_parts(upgrades, Chain::from_id(43112), false)
    }

    // Repeat of fee_state_tests::hdr, plus a min_delay_excess parameter.
    fn hdr(number: u64, time: u64, time_ms: Option<u64>, mde: Option<u64>) -> AvaHeader {
        AvaHeader {
            parent_hash: B256::ZERO,
            uncle_hash: B256::ZERO,
            coinbase: Address::ZERO,
            state_root: B256::ZERO,
            tx_root: B256::ZERO,
            receipt_root: B256::ZERO,
            bloom: Bytes::from(vec![0u8; 256]),
            difficulty: U256::ZERO,
            number,
            gas_limit: 15_000_000,
            gas_used: 0,
            time,
            extra: Bytes::new(),
            mix_digest: B256::ZERO,
            nonce: [0u8; 8],
            ext_data_hash: keccak256([]),
            base_fee: Some(U256::from(25_000_000_000u64)),
            ext_data_gas_used: None,
            block_gas_cost: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_root: None,
            time_milliseconds: time_ms,
            min_delay_excess: mde,
        }
    }

    const T: u64 = 1_700_000_000; // an arbitrary base timestamp (seconds)

    #[test]
    fn verify_time_pre_granite_equal_timestamp_ok() {
        // time.go:65-70 — equality allowed (multiple blocks per second pre-Granite).
        let spec = spec_from(0, u64::MAX, 0); // Granite never active
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T, None, None);
        assert!(
            verify_time(&spec, &parent, &header, T.checked_mul(1000).unwrap()).is_ok(),
            "verify_time(equal pre-Granite timestamps)"
        );
    }

    #[test]
    fn verify_time_rejects_block_older_than_parent() {
        // time.go:68-70 — errBlockTooOld.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T - 1, None, None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, T * 1000),
            Err(Error::BlockTooOld { .. })
        ));
    }

    #[test]
    fn verify_time_future_bound_is_inclusive() {
        // time.go:72-79 — exactly now+10s is allowed; one ms over rejects.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T + 10, None, None); // header_ms = (T+10)*1000
        let now_ms = T * 1000; // max allowed = now_ms + 10_000 == header_ms
        assert!(verify_time(&spec, &parent, &header, now_ms).is_ok());
        assert!(matches!(
            verify_time(&spec, &parent, &header, now_ms - 1),
            Err(Error::BlockTooFarInFuture { .. })
        ));
        // Sanity on the constant itself (time.go:20).
        assert_eq!(MAX_FUTURE_BLOCK_TIME_MS, 10_000);
    }

    #[test]
    fn verify_time_rejects_time_milliseconds_before_granite() {
        // time.go:81-86 — ErrTimeMillisecondsBeforeGranite.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T, Some(T * 1000), None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, T * 1000),
            Err(Error::TimeMillisecondsBeforeGranite)
        ));
    }

    #[test]
    fn verify_time_requires_time_milliseconds_at_granite() {
        // time.go:89-92 — ErrTimeMillisecondsRequired.
        let spec = spec_from(0, 0, 0); // Granite from genesis
        let parent = hdr(1, T, Some(T * 1000), None);
        let header = hdr(2, T + 2, None, None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, (T + 2) * 1000),
            Err(Error::TimeMillisecondsRequired)
        ));
    }

    #[test]
    fn verify_time_rejects_mismatched_time_milliseconds() {
        // time.go:94-101 — ErrTimeMillisecondsMismatched.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), None);
        let header = hdr(2, T + 2, Some((T + 3) * 1000), None); // 	ime != ms/1000
        assert!(matches!(
            verify_time(&spec, &parent, &header, (T + 3) * 1000),
            Err(Error::TimeMillisecondsMismatched { .. })
        ));
    }

    #[test]
    fn verify_time_first_granite_block_skips_min_delay() {
        // time.go:103-108 — parent without MinDelayExcess is exempt.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), None); // no excess
        let header = hdr(2, T, Some(T * 1000 + 1), None); // 1ms delay
        assert!(verify_time(&spec, &parent, &header, T * 1000 + 1).is_ok());
    }

    #[test]
    fn verify_time_enforces_min_delay_boundary() {
        // time.go:110-121 — actual delay < required rejects; == passes. Each
        // header derives `time` as `ms / 1000` so the Mismatched arm cannot
        // fire first, whatever value `required` happens to be.
        let spec = spec_from(0, 0, 0);
        let required = INITIAL_DELAY_EXCESS.delay();
        assert!(required > 0, "test premise: initial excess demands a delay");
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let exact_ms = T * 1000 + required;
        let exact = hdr(2, exact_ms / 1000, Some(exact_ms), None);
        let short = hdr(2, (exact_ms - 1) / 1000, Some(exact_ms - 1), None);
        let now = exact_ms;
        assert!(verify_time(&spec, &parent, &exact, now).is_ok());
        assert!(matches!(
            verify_time(&spec, &parent, &short, now),
            Err(Error::MinDelayNotMet { .. })
        ));
    }
}
```

(Drop `DelayExcess` from the test module's `use` list if the compiler flags it unused — only `INITIAL_DELAY_EXCESS` is exercised directly.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p ava-evm -E 'test(verify_time)'`
Expected: compile FAIL — `verify_time` and the `Error` variants do not exist.

- [ ] **Step 3: Add the Error variants**

In `crates/ava-evm/src/error.rs`, inside the existing `Error` enum (next to the `verify_header_gas_fields` variants like `BaseFeeMismatch`), add:

```rust
    /// coreth `customheader/time.go:22` (`errBlockTooOld`,
    /// `"block timestamp is too old: %d < parent %d"`).
    #[error("block timestamp is too old: {have} < parent {parent}")]
    BlockTooOld { have: u64, parent: u64 },
    /// coreth `customheader/time.go:23` (`ErrBlockTooFarInFuture`,
    /// `"block timestamp is too far in the future: %d > allowed %d"`).
    #[error("block timestamp is too far in the future: {have} > allowed {allowed}")]
    BlockTooFarInFuture { have: u64, allowed: u64 },
    /// coreth `customheader/time.go:24` (`ErrTimeMillisecondsRequired`).
    #[error("TimeMilliseconds is required after Granite activation")]
    TimeMillisecondsRequired,
    /// coreth `customheader/time.go:25` (`ErrTimeMillisecondsMismatched`,
    /// `"…: header.Time (%d) != TimeMilliseconds/1000 = (%d)"`).
    #[error(
        "TimeMilliseconds does not match header.Time: header.Time ({time}) != TimeMilliseconds/1000 = ({expected})"
    )]
    TimeMillisecondsMismatched { time: u64, expected: u64 },
    /// coreth `customheader/time.go:26` (`ErrTimeMillisecondsBeforeGranite`).
    #[error("TimeMilliseconds should be nil before Granite activation")]
    TimeMillisecondsBeforeGranite,
    /// coreth `customheader/time.go:27` (`ErrMinDelayNotMet`,
    /// `"…: actual delay %dms < required %dms"`).
    #[error("minimum block delay not met: actual delay {actual}ms < required {required}ms")]
    MinDelayNotMet { actual: u64, required: u64 },
```

- [ ] **Step 4: Implement `verify_time`**

In `crates/ava-evm/src/feerules/mod.rs`, first change `fn header_time_ms` to `pub(crate) fn header_time_ms` (Task 4's `verify()` wrapper needs it from `block.rs`). Then add, next to `verify_header_gas_fields`:

```rust
/// coreth `customheader/time.go:20` — `MaxFutureBlockTime` (10 s), in ms.
pub const MAX_FUTURE_BLOCK_TIME_MS: u64 = 10_000;

/// coreth `customheader/time.go:55-124` — `VerifyTime`.
///
/// Verifies the header's `Time`/`TimeMilliseconds` against the parent, the
/// rules, and the current time `now_ms` (Go passes `b.vm.clock.Time()`):
/// non-decreasing vs parent (equality allowed), not beyond `now + 10s`,
/// `TimeMilliseconds` nil pre-Granite / required + consistent at Granite, and
/// the ACP-226 minimum block delay demanded by the PARENT's `MinDelayExcess`.
///
/// # Errors
/// [`Error::BlockTooOld`] / [`Error::BlockTooFarInFuture`] /
/// [`Error::TimeMillisecondsBeforeGranite`] / [`Error::TimeMillisecondsRequired`] /
/// [`Error::TimeMillisecondsMismatched`] / [`Error::MinDelayNotMet`].
pub fn verify_time(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
    now_ms: u64,
) -> Result<(), Error> {
    // time.go:62-63 — both sides through the HeaderTimeMilliseconds fallback.
    let header_ms = header_time_ms(header);
    let parent_ms = header_time_ms(parent);

    // time.go:65-70 — non-decreasing; equality allowed.
    if header_ms < parent_ms {
        return Err(Error::BlockTooOld {
            have: header_ms,
            parent: parent_ms,
        });
    }

    // time.go:72-79 — future bound.
    let max_ms = now_ms.saturating_add(MAX_FUTURE_BLOCK_TIME_MS);
    if header_ms > max_ms {
        return Err(Error::BlockTooFarInFuture {
            have: header_ms,
            allowed: max_ms,
        });
    }

    // time.go:81-87 — pre-Granite: the field must be absent.
    if !spec.is_granite(header.time) {
        if header.time_milliseconds.is_some() {
            return Err(Error::TimeMillisecondsBeforeGranite);
        }
        return Ok(());
    }

    // time.go:89-92 — Granite: required.
    let Some(ms) = header.time_milliseconds else {
        return Err(Error::TimeMillisecondsRequired);
    };

    // time.go:94-101 — Time == TimeMilliseconds/1000.
    let expected_time = ms / 1000;
    if header.time != expected_time {
        return Err(Error::TimeMillisecondsMismatched {
            time: header.time,
            expected: expected_time,
        });
    }

    // time.go:103-108 — a parent without an excess (the first Granite block)
    // cannot demand a delay.
    let Some(parent_excess) = parent.min_delay_excess else {
        return Ok(());
    };

    // time.go:110-121 — the ordering check above proved header_ms >= parent_ms,
    // so the subtraction cannot underflow (Go carries the same comment).
    let actual = header_ms.saturating_sub(parent_ms);
    let required = DelayExcess(parent_excess).delay();
    if actual < required {
        return Err(Error::MinDelayNotMet { actual, required });
    }
    Ok(())
}
```

`DelayExcess` is already imported into `mod.rs` scope via `use crate::feerules::acp226::…` — check the existing `min_delay_excess_of` imports and reuse them.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(verify_time)'`
Expected: all 8 PASS.

- [ ] **Step 6: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt
cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/error.rs crates/ava-evm/src/feerules/mod.rs
git commit -m "feat(ava-evm): port customheader.VerifyTime (time.go:55-124) as feerules::verify_time"
```

---

### Task 2: `feerules::verify_min_delay_excess`

**Files:**
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/src/feerules/mod.rs`

**Interfaces:**
- Consumes: existing `min_delay_excess_of(spec, parent, timestamp, desired) -> Result<Option<u64>, Error>`; Task 1's test module.
- Produces: `pub fn verify_min_delay_excess(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader) -> Result<(), Error>`.

- [ ] **Step 1: Write the failing unit tests**

Append to `semantic_verify_tests` in `feerules/mod.rs` (add `verify_min_delay_excess` to the `use super::…` list):

```rust
    #[test]
    fn verify_min_delay_excess_pre_granite_is_noop() {
        // min_delay_excess.go:50-52.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T + 2, None, None);
        assert!(verify_min_delay_excess(&spec, &parent, &header).is_ok());
    }

    #[test]
    fn verify_min_delay_excess_requires_field_at_granite() {
        // min_delay_excess.go:54-57 — errRemoteMinDelayExcessNil.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), None);
        assert!(matches!(
            verify_min_delay_excess(&spec, &parent, &header),
            Err(Error::RemoteMinDelayExcessNil)
        ));
    }

    #[test]
    fn verify_min_delay_excess_accepts_reachable_claim() {
        // min_delay_excess.go:59-71 — claimed-as-desired: an unchanged claim
        // is always reachable (update toward itself is a no-op).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), Some(INITIAL_DELAY_EXCESS.0));
        assert!(verify_min_delay_excess(&spec, &parent, &header).is_ok());
    }

    #[test]
    fn verify_min_delay_excess_rejects_unreachable_claim() {
        // min_delay_excess.go:73-79 — errIncorrectMinDelayExcess: a claim the
        // one-step update from the parent cannot reach recomputes lower.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), Some(u64::MAX));
        assert!(matches!(
            verify_min_delay_excess(&spec, &parent, &header),
            Err(Error::IncorrectMinDelayExcess { .. })
        ));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p ava-evm -E 'test(verify_min_delay_excess)'`
Expected: compile FAIL — function/variants missing.

- [ ] **Step 3: Add the Error variants**

In `error.rs`:

```rust
    /// coreth `customheader/min_delay_excess.go:18` (`errRemoteMinDelayExcessNil`).
    #[error("remote min delay excess should not be nil")]
    RemoteMinDelayExcessNil,
    /// coreth `customheader/min_delay_excess.go:19` (`errIncorrectMinDelayExcess`,
    /// `"…: expected %d, found %d"`).
    #[error("incorrect min delay excess: expected {expected}, found {found}")]
    IncorrectMinDelayExcess { expected: u64, found: u64 },
```

- [ ] **Step 4: Implement**

In `feerules/mod.rs`, next to `min_delay_excess_of`:

```rust
/// coreth `customheader/min_delay_excess.go:45-81` — `VerifyMinDelayExcess`.
///
/// Granite-only: the header's ACP-226 `MinDelayExcess` must be present and
/// equal the recompute from the parent with the CLAIMED value as the desired
/// target — Go's claimed-as-desired trick (`min_delay_excess.go:59-63`): if the
/// claim was reachable in one update step, the recompute lands exactly on it;
/// otherwise the recompute stops short and the equality fails.
///
/// # Errors
/// [`Error::RemoteMinDelayExcessNil`] / [`Error::IncorrectMinDelayExcess`];
/// propagates [`Error::InvalidFeeState`] from [`min_delay_excess_of`].
pub fn verify_min_delay_excess(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    // min_delay_excess.go:50-52.
    if !spec.is_granite(header.time) {
        return Ok(());
    }
    // min_delay_excess.go:54-57.
    let Some(found) = header.min_delay_excess else {
        return Err(Error::RemoteMinDelayExcessNil);
    };
    // min_delay_excess.go:59-71.
    let Some(expected) =
        min_delay_excess_of(spec, parent, header.time, Some(DelayExcess(found)))?
    else {
        // Unreachable: min_delay_excess_of returns Some whenever the child
        // timestamp is in Granite, which the guard above established.
        return Err(Error::InvalidFeeState(
            "expected min delay excess absent at Granite".to_string(),
        ));
    };
    // min_delay_excess.go:73-79.
    if found != expected {
        return Err(Error::IncorrectMinDelayExcess { expected, found });
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(verify_min_delay_excess)'`
Expected: 4 PASS.

- [ ] **Step 6: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/error.rs crates/ava-evm/src/feerules/mod.rs
git commit -m "feat(ava-evm): port customheader.VerifyMinDelayExcess (min_delay_excess.go:45-81)"
```

---

### Task 3: `feerules::gas_capacity` + `feerules::verify_gas_used`

**Files:**
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/src/feerules/mod.rs`

**Interfaces:**
- Consumes: `fee_state_before_block`, `header_time_ms`, constants `CORTINA_GAS_LIMIT` / `APRICOT_PHASE1_GAS_LIMIT`, existing `Error::{ExtDataGasUsedTooLarge, FeeOverflow}`.
- Produces: `pub fn gas_capacity(spec, parent: &AvaHeader, time_ms: u64) -> Result<u64, Error>`; `pub fn verify_gas_used(spec, parent: &AvaHeader, header: &AvaHeader) -> Result<(), Error>`. Task 5 calls `verify_gas_used`.

- [ ] **Step 1: Write the failing unit tests**

Append to `semantic_verify_tests` (extend imports with `gas_capacity, verify_gas_used` and `super::CORTINA_GAS_LIMIT`):

```rust
    #[test]
    fn gas_capacity_pre_fortuna_static_limits() {
        // gas_limit.go:170-173 → GasLimit: Cortina 15M / AP1 8M / pre-AP1
        // parent.gas_limit (gas_limit.go:52-57).
        let cortina = spec_from(u64::MAX, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        assert_eq!(gas_capacity(&cortina, &parent, T * 1000).unwrap(), CORTINA_GAS_LIMIT);
    }

    #[test]
    fn gas_capacity_fortuna_uses_pre_block_fee_state() {
        // gas_limit.go:175-179 — the ACP-176 capacity. A genesis parent whose
        // elapsed time saturates the fill gives capacity == MaxCapacity (10M
        // at the default target excess — the live-block-1 golden numbers in
        // fee_state_tests::after_block_matches_live_block1_numbers).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(0, T, Some(T * 1000), None);
        let cap = gas_capacity(&spec, &parent, (T + 1_000_000) * 1000).unwrap();
        assert_eq!(cap, 10_000_000, "saturated ACP-176 capacity at default target");
    }

    #[test]
    fn verify_gas_used_boundary() {
        // gas_limit.go:90-96 — errInvalidGasUsed: > capacity rejects, == passes.
        let spec = spec_from(u64::MAX, u64::MAX, 0); // Cortina static 15M
        let parent = hdr(1, T, None, None);
        let mut ok_hdr = hdr(2, T + 2, None, None);
        ok_hdr.gas_used = CORTINA_GAS_LIMIT;
        assert!(verify_gas_used(&spec, &parent, &ok_hdr).is_ok());
        let mut bad = hdr(2, T + 2, None, None);
        bad.gas_used = CORTINA_GAS_LIMIT + 1;
        assert!(matches!(
            verify_gas_used(&spec, &parent, &bad),
            Err(Error::GasUsedOverCapacity { .. })
        ));
    }

    #[test]
    fn verify_gas_used_folds_ext_data_gas_at_fortuna() {
        // gas_limit.go:69-82 — Fortuna+: gasUsed + extDataGasUsed vs capacity;
        // a non-u64 claim errors (errInvalidExtraDataGasUsed).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(0, T, Some(T * 1000), None);
        let ms = (T + 1_000_000) * 1000;
        let mut h = hdr(2, T + 1_000_000, Some(ms), None);
        h.gas_used = 9_999_999;
        h.ext_data_gas_used = Some(U256::from(2u64)); // 10_000_001 > 10M
        assert!(matches!(
            verify_gas_used(&spec, &parent, &h),
            Err(Error::GasUsedOverCapacity { .. })
        ));
        h.ext_data_gas_used = Some(U256::from(u128::from(u64::MAX)) + U256::from(1u64));
        assert!(matches!(
            verify_gas_used(&spec, &parent, &h),
            Err(Error::ExtDataGasUsedTooLarge(_))
        ));
    }
```

(Test code may use `unwrap()` — the no-unwrap rule is library-code only; `gosec`-analog lints skip tests.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p ava-evm -E 'test(gas_capacity) or test(verify_gas_used)'`
Expected: compile FAIL.

- [ ] **Step 3: Add the Error variant**

```rust
    /// coreth `customheader/gas_limit.go:90-96` (`errInvalidGasUsed`,
    /// `"invalid gas used: have %d, capacity %d"`).
    #[error("invalid gas used: have {have}, capacity {capacity}")]
    GasUsedOverCapacity { have: u64, capacity: u64 },
```

(Confirm the exact `errInvalidGasUsed` message text in `gas_limit.go`'s `var` block at implementation time and mirror it verbatim; adjust the `#[error]` string if it differs.)

- [ ] **Step 4: Implement**

In `feerules/mod.rs`, next to `verify_gas_limit`:

```rust
/// coreth `customheader/gas_limit.go:164-180` — `GasCapacity`.
///
/// Pre-Fortuna the capacity IS the gas limit (`GasLimit`, gas_limit.go:30-58:
/// Cortina 15M / AP1 8M / pre-AP1 the parent's own limit); Fortuna+ it is the
/// ACP-176 pre-block state's capacity (`feeStateBeforeBlock`).
///
/// # Errors
/// Propagates [`Error::InvalidFeeState`] from the fee-state recompute.
pub fn gas_capacity(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    time_ms: u64,
) -> Result<u64, Error> {
    // gas_limit.go:169.
    let timestamp = time_ms / 1000;
    let phase = spec.fork_at(timestamp);
    if phase >= AvaPhase::Fortuna {
        // gas_limit.go:175-179.
        let state = fee_state_before_block(spec, parent, time_ms)?;
        return Ok(state.gas.capacity.0);
    }
    // gas_limit.go:170-173 → GasLimit's static arms.
    if phase >= AvaPhase::Cortina {
        Ok(CORTINA_GAS_LIMIT)
    } else if phase >= AvaPhase::ApricotPhase1 {
        Ok(APRICOT_PHASE1_GAS_LIMIT)
    } else {
        // gas_limit.go:52-57 — pre-AP1 falls back to the parent's limit.
        Ok(parent.gas_limit)
    }
}

/// coreth `customheader/gas_limit.go:61-98` — `VerifyGasUsed`.
///
/// The claimed `GasUsed` (plus `ExtDataGasUsed` at Fortuna+, when present)
/// must fit within the block's gas capacity. This is the pre-execution
/// capacity bound Go runs inside `verifyIntrinsicGas` (`wrapped_block.go:302`).
///
/// # Errors
/// [`Error::ExtDataGasUsedTooLarge`] (non-u64 claim) / [`Error::FeeOverflow`]
/// (u64 overflow of the sum) / [`Error::GasUsedOverCapacity`]; propagates
/// [`gas_capacity`]'s errors.
pub fn verify_gas_used(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let mut gas_used = header.gas_used;
    // gas_limit.go:69-82 — fold ExtDataGasUsed in at Fortuna+.
    if spec.is_fortuna(header.time) {
        if let Some(ext) = header.ext_data_gas_used {
            let ext_u64 =
                u64::try_from(ext).map_err(|_| Error::ExtDataGasUsedTooLarge(ext))?;
            gas_used = gas_used.checked_add(ext_u64).ok_or(Error::FeeOverflow)?;
        }
    }
    // gas_limit.go:84-96.
    let capacity = gas_capacity(spec, parent, header_time_ms(header))?;
    if gas_used > capacity {
        return Err(Error::GasUsedOverCapacity {
            have: gas_used,
            capacity,
        });
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(gas_capacity) or test(verify_gas_used)'`
Expected: 4 PASS.

- [ ] **Step 6: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/error.rs crates/ava-evm/src/feerules/mod.rs
git commit -m "feat(ava-evm): port customheader.VerifyGasUsed + GasCapacity (gas_limit.go:61-98,164-180)"
```

---

### Task 4: Thread `now_ms` + `bootstrapped`; wire the semantic stage

**Files:**
- Modify: `crates/ava-evm/src/vm.rs` (Shared fields, `set_state`, `with_clock`, `build_block` clock read, the `VerifiedEvmBlock::verify` call at ~line 161-164)
- Modify: `crates/ava-evm/src/block.rs` (`verify` wrapper, `verify_with_predicates` signature + semantic-stage calls)
- Test: `crates/ava-evm/tests/semantic_verify.rs` (new)

**Interfaces:**
- Consumes: Task 1 `verify_time` + `MAX_FUTURE_BLOCK_TIME_MS`, Task 2 `verify_min_delay_excess`, `pub(crate) header_time_ms`.
- Produces: `EvmBlock::verify_with_predicates(ctx, parent_state_root, parent, exec_ctx, now_ms: u64, bootstrapped: bool)` (two appended params); `EvmBlock::verify(ctx, parent_state_root, parent)` **signature unchanged** (bootstrap-shape defaults, see Step 3); `Shared { clock: parking_lot::Mutex<Arc<dyn Clock>>, bootstrapped: AtomicBool, .. }`. Tasks 5/6 insert further calls into the same semantic stage.

- [ ] **Step 1: Write the failing integration test**

Create `crates/ava-evm/tests/semantic_verify.rs`. Mirror `verify_gas_fields.rs`'s harness **exactly** (copy its `inner_block_of`, `local_spec`, `mutated_live_block`, and vm-boot/verify-driver helper — read that file first; test convention is repeat-don't-import). The four mutants below need **no fee-field restamp**: the live local block 1 sits years after its genesis parent, so the ACP-176 pre-block advance is saturated (capacity pinned at max) and the fee/gas expectations are invariant under these time-field edits (each mutant's comment carries the argument).

```rust
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end guard for the semantic-verify family port (coreth
//! `wrapped_block.go:335-391` — `VerifyMinDelayExcess` + `VerifyTime`): the
//! full `parse_block → verify` entry must reject a block whose time /
//! min-delay-excess fields disagree with the rules, even though its state
//! root (and, for these mutants, every fee/gas field expectation) is valid.
//!
//! Restamp-free mutants: block 1's elapsed-time-from-genesis is ~5.6 years,
//! so `fee_state_before_block`'s advance saturates capacity at MaxCapacity
//! regardless of ±seconds-scale (or even +decades) shifts to the header time
//! fields — base_fee / extra-prefix / gas-limit / block-gas-cost recomputes
//! are unchanged and `verify_header_gas_fields` still passes, isolating the
//! NEW checks' sentinels.

// [copy of verify_gas_fields.rs's inner_block_of / local_spec /
//  mutated_live_block / verify-driver helpers goes here]

#[tokio::test]
async fn strip_time_milliseconds_is_rejected() {
    // VerifyTime time.go:89-92 — Granite requires TimeMilliseconds. The ms
    // fallback (time*1000) differs from the stamped ms by <1s; the saturated
    // advance makes the fee recomputes identical either way.
    let bytes = mutated_live_block(|parts| {
        parts.header.time_milliseconds = None;
        // The header also carries min_delay_excess (t8); dropping t7 while
        // keeping t8 is a legal wire shape (presence rule is any-later ⇒
        // encode-earlier as nil scalar).
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("TimeMilliseconds is required"),
        "want ErrTimeMillisecondsRequired, got: {err}"
    );
}

#[tokio::test]
async fn mismatched_time_milliseconds_is_rejected() {
    // VerifyTime time.go:94-101 — Time != TimeMilliseconds/1000.
    let bytes = mutated_live_block(|parts| {
        let ms = parts.header.time_milliseconds.expect("live block has ms");
        parts.header.time_milliseconds = Some(ms + 5_000); // +5s in ms only
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("TimeMilliseconds does not match header.Time"),
        "want ErrTimeMillisecondsMismatched, got: {err}"
    );
}

#[tokio::test]
async fn far_future_block_is_rejected() {
    // VerifyTime time.go:72-79 — beyond now+10s (prod path reads RealClock;
    // year-4000 is deterministically far-future for any test run).
    let bytes = mutated_live_block(|parts| {
        let t = 64_060_588_800u64; // 4000-01-01
        parts.header.time = t;
        parts.header.time_milliseconds = Some(t * 1000);
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("too far in the future"),
        "want ErrBlockTooFarInFuture, got: {err}"
    );
}

#[tokio::test]
async fn wrong_min_delay_excess_is_rejected() {
    // VerifyMinDelayExcess min_delay_excess.go:73-79 — an unreachable claim.
    // min_delay_excess is a bare header-tail field, not a fee-prefix input,
    // so no other expectation shifts.
    let bytes = mutated_live_block(|parts| {
        parts.header.min_delay_excess = Some(u64::MAX);
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("incorrect min delay excess"),
        "want errIncorrectMinDelayExcess, got: {err}"
    );
}
```

Name the copied verify-driver helper `parse_and_verify(bytes) -> Result<(), String>` (same shape as `verify_gas_fields.rs`'s driver that returns the first error string from parse or verify).

**Caveat while copying:** if `verify_gas_fields.rs`'s existing mutants pass through a genesis whose stamped `min_delay_excess`/`time_milliseconds` interplay differs from the assumptions above, run the honest (unmutated) block through `parse_and_verify` first in a fifth test `honest_block_still_verifies` and assert `Ok` — this pins that the new checks don't false-reject the honest arm:

```rust
#[tokio::test]
async fn honest_block_still_verifies() {
    let bytes = mutated_live_block(|_| {}); // identity re-assemble
    parse_and_verify(&bytes).await.expect("honest live block verifies");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-evm -E 'test(semantic_verify)'`
Expected: the four mutant tests FAIL (blocks are ACCEPTED — the fail-open this task closes); `honest_block_still_verifies` PASSES.

- [ ] **Step 3: Implement the threading + wiring**

**(a) `vm.rs` — `Shared` gains the live-read seams.** Add fields (with `use std::sync::atomic::{AtomicBool, Ordering};` and the existing `Clock` import):

```rust
    /// Injectable wall clock (specs/24 hazard #5) — the single live-read
    /// source for build_block's timestamp AND verify's `VerifyTime` future
    /// bound (Go `vm.clock`). Behind a mutex so the builder-style
    /// [`EvmVm::with_clock`] can swap it after `Shared` is assembled; reads
    /// clone the `Arc` (cheap) and never hold the lock across work.
    clock: parking_lot::Mutex<Arc<dyn Clock>>,
    /// Go `vm.bootstrapped` (`utils.Atomic[bool]`): true once `set_state`
    /// enters `NormalOp`. Read live at verify time to gate
    /// `verifyIntrinsicGas` (wrapped_block.go:376) — a wrap-time copy would
    /// be stale for blocks re-verified after bootstrap completes.
    bootstrapped: AtomicBool,
```

- Seed both where `Shared` is constructed (`clock: parking_lot::Mutex::new(Arc::new(RealClock))`, `bootstrapped: AtomicBool::new(false)`).
- **Delete** the `clock` field from `EvmVm` (vm.rs:364) and its `EvmVm::new` seeding; `with_clock` becomes:

```rust
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        *self.shared.clock.lock() = clock;
        self
    }
```

- `build_block`'s read (vm.rs:918) becomes `let now_secs = self.shared.clock.lock().unix().max(parent_header.time.saturating_add(1));` (keep the surrounding logic identical).
- `set_state` (vm.rs:766):

```rust
    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> VmResult<()> {
        self.engine_state = state;
        // Go coreth vm.go SetState → bootstrapped.Set(state == snow.NormalOp):
        // onNormalOperationsStarted flips true; re-entering bootstrap flips false.
        self.shared
            .bootstrapped
            .store(matches!(state, EngineState::NormalOp), Ordering::Release);
        Ok(())
    }
```

**(b) `vm.rs` — the prod verify call** (`VerifiedEvmBlock::verify`, ~line 154-175). Replace the `self.block.verify(&self.ctx, parent_root, &parent_header)` call with:

```rust
        // Live reads, mirroring Go's b.vm.clock.Time() / b.vm.bootstrapped.Get()
        // at verify time (wrapped_block.go:359,376).
        let now_ms = {
            let clock = Arc::clone(&self.shared.clock.lock());
            clock
                .now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        };
        let bootstrapped = self.shared.bootstrapped.load(Ordering::Acquire);
        let precommit = self
            .block
            .verify_with_predicates(
                &self.ctx,
                parent_root,
                &parent_header,
                &AvaExecCtx::default(),
                now_ms,
                bootstrapped,
            )
            .map_err(ava_snow::Error::from)?;
```

(`AvaExecCtx` may need importing in vm.rs; `verify(ctx, root, parent)` was exactly this call with default exec ctx, so behavior is unchanged apart from the two new args.)

**(c) `block.rs` — signatures + the semantic stage.**

`verify` keeps its 3-arg signature as the bootstrap-shape wrapper (zero churn across the ~10 existing test call sites):

```rust
    pub fn verify(
        &self,
        ctx: &EvmBlockContext,
        parent_state_root: B256,
        parent: &AvaHeader,
    ) -> Result<B256> {
        // Bootstrap-shape verify: `now` pinned to the block's own timestamp
        // (the future bound is vacuous for canonically-accepted history —
        // the same effective behavior as Go verifying old blocks against a
        // real clock) and bootstrapped=false (intrinsic-gas + predicate
        // checks deferred, wrapped_block.go:372-386). The production path
        // (`VerifiedEvmBlock::verify`) supplies live values instead.
        let now_ms = crate::feerules::header_time_ms(self.header());
        self.verify_with_predicates(
            ctx,
            parent_state_root,
            parent,
            &AvaExecCtx::default(),
            now_ms,
            false,
        )
    }
```

`verify_with_predicates` appends the two params and inserts the semantic stage directly after the `verify_header_gas_fields` call (block.rs:941):

```rust
    pub fn verify_with_predicates(
        &self,
        ctx: &EvmBlockContext,
        parent_state_root: B256,
        parent: &AvaHeader,
        exec_ctx: &AvaExecCtx,
        now_ms: u64,
        bootstrapped: bool,
    ) -> Result<B256> {
        …existing syntactic_verify + verify_header_gas_fields calls…

        // ── coreth `wrappedBlock.semanticVerify` (wrapped_block.go:335-391),
        // in Go's call order. VerifyTargetExponent / VerifyMinPriceExponent /
        // VerifySettled (wrapped_block.go:350-366) are structurally covered:
        // `AvaHeader::decode_rlp` fail-closes on the SAE-only trailing tail
        // fields (block.rs:250-252), so a violating block never parses — Go
        // rejects the same block at verify; same verdict, different stage.
        // The errIsHeliconBlock guard (wrapped_block.go:368) is n/a — Helicon
        // is unscheduled and `AvaPhase` carries no Helicon variant (see the
        // verify_extra_prefix Helicon callout).
        // wrapped_block.go:345.
        crate::feerules::verify_min_delay_excess(ctx.chain_spec(), parent, self.header())?;
        // wrapped_block.go:359.
        crate::feerules::verify_time(ctx.chain_spec(), parent, self.header(), now_ms)?;
        // wrapped_block.go:372-379 — bootstrapped-gated (during bootstrap the
        // block is canonically accepted; required indices may be absent).
        // Task 5 inserts verify_intrinsic_gas here.
        let _ = bootstrapped; // consumed by Task 5's insertion
        // Task 6 inserts atomic verify_ext_data_gas_used here.

        …existing atomic-conflict verify + execution…
    }
```

(The `let _ = bootstrapped;` placeholder exists ONLY until Task 5 lands in the same branch — if executing tasks strictly in order it is removed within two commits; keep it to compile warning-free.)

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p ava-evm -E 'test(semantic_verify)'`
Expected: all 5 PASS (4 mutants now rejected with the asserted sentinels; honest still accepted).

Run the full crate to prove zero regression across the ~10 unchanged `verify()` call sites and the vm-path suites:
`cargo nextest run -p ava-evm`
Expected: all PASS. If `clock_injection.rs` fails, its `with_clock` expectations must be checked against the moved clock (the seam's behavior is identical; a failure means the swap missed a read path).

- [ ] **Step 5: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/vm.rs crates/ava-evm/src/block.rs crates/ava-evm/tests/semantic_verify.rs
git commit -m "feat(ava-evm): run VerifyMinDelayExcess + VerifyTime on the verify path — clock + bootstrapped threaded live via Shared"
```

---

### Task 5: `verify_intrinsic_gas` (bootstrapped-gated)

**Files:**
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/src/mempool.rs` (`fn intrinsic_gas` → `pub(crate) fn intrinsic_gas`)
- Modify: `crates/ava-evm/src/block.rs` (new method + gate wiring)
- Test: `crates/ava-evm/tests/semantic_verify.rs` (extend)

**Interfaces:**
- Consumes: Task 3 `verify_gas_used`; existing `mempool::intrinsic_gas(tx: &TransactionSigned, shanghai: bool) -> u64`; Task 4's `bootstrapped` param.
- Produces: `EvmBlock::verify_intrinsic_gas(&self, spec: &AvaChainSpec, parent: &AvaHeader) -> Result<()>` called under `if bootstrapped`.

- [ ] **Step 1: Write the failing integration test**

Append to `tests/semantic_verify.rs`. This mutant lowers the claimed `gas_used` below the tx's intrinsic floor, which shifts the ACP-176 extra prefix (the prefix consumes `gas_used`), so the mutation **restamps the prefix** to stay self-consistent — the exact shape a Byzantine proposer would use:

```rust
#[tokio::test]
async fn understated_gas_used_is_rejected_when_bootstrapped() {
    // wrapped_block.go:321-329 — Σ intrinsic gas (21_000 for the single
    // legacy transfer) > claimed gas_used (0). Gated on bootstrapped, so the
    // driver must SetState(NormalOp) first. The extra prefix is restamped for
    // the new gas_used (fee_state_after_block consumes it); base_fee /
    // block_gas_cost / gas_limit are gas_used-independent recomputes.
    let spec = local_spec();
    let bytes = mutated_live_block(|parts| {
        parts.header.gas_used = 0;
        // Restamp the ACP-176 prefix: recompute the post-block fee state at
        // the mutated gas_used and splice it over the first STATE_SIZE bytes
        // of `extra` (the Durango predicate-results suffix is preserved).
        let genesis = genesis_header_of(&spec); // the same parent the honest
                                                // vector was built on — copy
                                                // the helper verify_gas_fields.rs
                                                // (or cancun_clamp.rs) uses to
                                                // obtain it.
        let after = ava_evm::feerules::fee_state_after_block(
            &spec,
            &genesis,
            parts.header.time,
            parts.header.time_milliseconds,
            0,                                   // the mutated gas_used
            0,                                   // no atomic gas in this block
            None,
        )
        .expect("restamp fee state");
        let mut extra = after.to_bytes().to_vec();
        extra.extend_from_slice(&parts.header.extra[ava_evm::feerules::acp176::STATE_SIZE..]);
        parts.header.extra = extra.into();
    });
    let err = parse_and_verify_bootstrapped(&bytes)
        .await
        .expect_err("must reject");
    assert!(
        err.contains("intrinsic gas"),
        "want errTotalIntrinsicGasCostExceedsClaimed, got: {err}"
    );
}

#[tokio::test]
async fn understated_gas_used_skipped_while_bootstrapping() {
    // wrapped_block.go:376 — the SAME bytes verify while NOT bootstrapped
    // (the gate must skip, not reject) … and then fail the execution-layer
    // gas_used equality instead, which is the pre-existing backstop. Assert
    // the error does NOT name intrinsic gas.
    let bytes = understated_gas_used_block();
    let err = parse_and_verify(&bytes).await.expect_err("execution backstop");
    assert!(
        !err.contains("intrinsic gas"),
        "bootstrapping node must skip verifyIntrinsicGas, got: {err}"
    );
}
```

Extract the whole `mutated_live_block(…)` call from the first test into `fn understated_gas_used_block() -> Vec<u8>` so both tests share the identical mutant bytes (the first test becomes `let bytes = understated_gas_used_block();` too).

Adaptation notes for the implementer:
- `parse_and_verify_bootstrapped` = copy of `parse_and_verify` that calls `vm.set_state(&token, EngineState::NormalOp).await` (import `ava_snow::EngineState`) after boot, before `parsed.verify(...)`.
- `genesis_header_of`: `verify_gas_fields.rs` / `cancun_clamp.rs` already derive the genesis parent header for their drivers — reuse that exact mechanism (whatever it is in that harness: decoding the genesis block or reading the fixture); do not invent a new one.
- Check `feerules::acp176::STATE_SIZE` and `Acp176State::to_bytes` visibility from an integration test (`ava_evm::feerules::acp176` must be a `pub` path — it is used by `fee_state_tests` internally; if not re-exported, add `pub use` of `STATE_SIZE` alongside the existing `pub use ava_vm::components::gas::…` re-exports in `feerules/mod.rs`).
- If the second test's execution backstop produces an `Ok` (execution recomputes and overwrites rather than equality-checks), change its assertion to whatever the pre-existing behavior is and document it — the load-bearing assertion is only that **no intrinsic-gas sentinel fires while bootstrapping**.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p ava-evm -E 'test(understated_gas_used)'`
Expected: first test FAILS (no intrinsic check exists → no such sentinel).

- [ ] **Step 3: Implement**

**(a) Error variants** (`error.rs`) — mirror `wrapped_block.go`'s error vars (confirm exact strings in the Go `var` block near the top of `wrapped_block.go` and mirror verbatim):

```rust
    /// coreth `wrapped_block.go` (`errInvalidGasUsedRelativeToCapacity`).
    #[error("invalid gas used relative to capacity: {0}")]
    GasUsedRelativeToCapacity(Box<Error>),
    /// coreth `wrapped_block.go` (`errTotalIntrinsicGasCostExceedsClaimed`,
    /// `"…: intrinsic gas (%d) > claimed gas used (%d)"`).
    #[error("total intrinsic gas cost exceeds claimed: intrinsic gas ({intrinsic}) > claimed gas used ({claimed})")]
    TotalIntrinsicGasExceedsClaimed { intrinsic: u64, claimed: u64 },
```

**(b) `mempool.rs`:** change `fn intrinsic_gas(` to `pub(crate) fn intrinsic_gas(` (line ~631). No body change — it is the already-reviewed libevm `core.IntrinsicGas` port the mempool admission uses; the recorded-oracle corpus (Task 8) arbitrates its parity on the verify path.

**(c) `block.rs`:** add the method next to `check_min_gas_price`:

```rust
    /// coreth `wrapped_block.go:287-332` — `verifyIntrinsicGas`. Runs only on
    /// a bootstrapped node (wrapped_block.go:376): (1) the claimed GasUsed
    /// (+ExtDataGasUsed at Fortuna+) must fit the block's gas capacity
    /// ([`crate::feerules::verify_gas_used`]); (2) the summed per-tx intrinsic
    /// gas (libevm `core.IntrinsicGas` — the same port the mempool admission
    /// uses) must not exceed the claimed GasUsed.
    fn verify_intrinsic_gas(&self, spec: &AvaChainSpec, parent: &AvaHeader) -> Result<()> {
        // wrapped_block.go:301-304.
        crate::feerules::verify_gas_used(spec, parent, self.header())
            .map_err(|e| Error::GasUsedRelativeToCapacity(Box::new(e)))?;

        // wrapped_block.go:306-319 — Σ intrinsic. Shanghai ← Durango
        // (config_extra.go:83). The Rust port saturates where Go returns
        // ErrGasUintOverflow; a saturated u64::MAX total still exceeds any
        // claimable gas_used, so the verdict is identical.
        let shanghai = spec.fork_at(self.header().time) >= AvaPhase::Durango;
        let mut total: u64 = 0;
        for tx in &self.parts().transactions {
            let gas = crate::mempool::intrinsic_gas(tx, shanghai);
            total = total.saturating_add(gas);
        }
        // wrapped_block.go:321-329.
        if total > self.header().gas_used {
            return Err(Error::TotalIntrinsicGasExceedsClaimed {
                intrinsic: total,
                claimed: self.header().gas_used,
            });
        }
        Ok(())
    }
```

(Adjust `AvaPhase`/`Error` imports; `parts()` accessor exists — it backs `atomic_txs()`.)

Replace Task 4's `let _ = bootstrapped;` placeholder in `verify_with_predicates` with:

```rust
        if bootstrapped {
            self.verify_intrinsic_gas(ctx.chain_spec(), parent)?;
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(understated_gas_used) or test(semantic_verify)'`
Expected: PASS. Then full crate: `cargo nextest run -p ava-evm` — all PASS (existing suites run un-bootstrapped, so the gate keeps them green).

- [ ] **Step 5: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/error.rs crates/ava-evm/src/mempool.rs crates/ava-evm/src/block.rs crates/ava-evm/tests/semantic_verify.rs
git commit -m "feat(ava-evm): port verifyIntrinsicGas (wrapped_block.go:287-332) — bootstrapped-gated capacity + intrinsic-sum checks"
```

---

### Task 6: Atomic `Tx::gas_used` + `verify_ext_data_gas_used`

**Files:**
- Modify: `crates/ava-evm/src/atomic/tx.rs` (new constants + method + unit tests)
- Modify: `crates/ava-evm/src/atomic/verify.rs` (new function + unit tests)
- Modify: `crates/ava-evm/src/error.rs`
- Modify: `crates/ava-evm/src/block.rs` (wire the call)

**Interfaces:**
- Consumes: `atomic::tx::Tx { unsigned: AtomicTx, unsigned_bytes, .. }`, `TX_BYTES_GAS`, `COST_PER_SIGNATURE`, `TransferableInput.r#in.cost()` (`ava_vm::components::avax::TransferableIn::cost`), `AvaPhase`, `spec.is_fortuna`.
- Produces: `Tx::gas_used(&self, fixed_fee: bool) -> Result<u64, Error>`; `pub const AP5_ATOMIC_TX_INTRINSIC_GAS: u64 = 10_000;` / `pub const AP5_ATOMIC_GAS_LIMIT: u64 = 100_000;` (in `atomic/tx.rs`); `atomic::verify::verify_ext_data_gas_used(spec: &AvaChainSpec, header: &AvaHeader, atomic_txs: &[Tx]) -> Result<(), Error>`.

- [ ] **Step 1: Write the failing unit tests**

In `atomic/tx.rs`'s existing `#[cfg(test)]` module (it already has `golden_import()` / `golden_export()` builders):

```rust
    #[test]
    fn gas_used_matches_go_formula() {
        // coreth import_tx.go:136-160 / export_tx.go:134-153.
        let import = Tx::new(AtomicTx::Import(golden_import()));
        // Tx::new must populate unsigned_bytes (if it does not, call the same
        // initialize/parse path the golden tests use to fill the caches).
        let base = import.gas_used(false).expect("import gas");
        // import = len(unsigned_bytes)*TxBytesGas + Σ in.cost(); the golden
        // import has one input with one sig index ⇒ + COST_PER_SIGNATURE.
        assert_eq!(
            base,
            import.unsigned_bytes.len() as u64 * TX_BYTES_GAS + COST_PER_SIGNATURE,
            "Tx::gas_used(import, fixed_fee=false)"
        );
        // fixedFee (AP5+) adds exactly ap5.AtomicTxIntrinsicGas.
        assert_eq!(
            import.gas_used(true).expect("import gas fixed"),
            base + AP5_ATOMIC_TX_INTRINSIC_GAS
        );

        let export = Tx::new(AtomicTx::Export(golden_export()));
        let ins = match &export.unsigned {
            AtomicTx::Export(t) => t.ins.len() as u64,
            AtomicTx::Import(_) => unreachable!(),
        };
        assert_eq!(
            export.gas_used(false).expect("export gas"),
            export.unsigned_bytes.len() as u64 * TX_BYTES_GAS + ins * COST_PER_SIGNATURE,
            "Tx::gas_used(export, fixed_fee=false)"
        );
    }

    #[test]
    fn ap5_constants_match_go() {
        // coreth plugin/evm/upgrade/ap5/params.go:33,38.
        assert_eq!(AP5_ATOMIC_GAS_LIMIT, 100_000);
        assert_eq!(AP5_ATOMIC_TX_INTRINSIC_GAS, 10_000);
    }
```

If the golden import's input carries a different sig-index count, read the fixture and adjust the expected `Σ in.cost()` term — the assertion must be computed from the FIXTURE's shape, not tuned until green.

In `atomic/verify.rs`'s test module (create one if absent, following `verify_no_conflicts`' tests):

```rust
    #[test]
    fn verify_ext_data_gas_used_arms() {
        // block_extension.go:142-177. `spec_from` and `hdr` are local repeats
        // of the helpers shown IN FULL in Task 1 Step 1 (feerules/mod.rs
        // semantic_verify_tests — copy those two fn bodies verbatim into this
        // module); the golden import fixture repeats `atomic/tx.rs`'s
        // `golden_import()` test builder.
        let spec = spec_from(u64::MAX, u64::MAX, 0); // AP4+AP5 on, Fortuna off
        let tx = Tx::new(AtomicTx::Import(golden_import()));
        let want = tx.gas_used(true).expect("gas");
        let mut header = hdr(2, T + 2, None, None);

        // (1) equality: claimed == recomputed ⇒ Ok.
        header.ext_data_gas_used = Some(U256::from(want));
        assert!(verify_ext_data_gas_used(&spec, &header, std::slice::from_ref(&tx)).is_ok());

        // (2) inflated claim ⇒ "invalid extDataGasUsed".
        header.ext_data_gas_used = Some(U256::from(want + 1));
        assert!(matches!(
            verify_ext_data_gas_used(&spec, &header, std::slice::from_ref(&tx)),
            Err(Error::InvalidExtDataGasUsed { .. })
        ));

        // (3) AP5 pre-Fortuna bound: claim > AtomicGasLimit ⇒ "too large".
        header.ext_data_gas_used = Some(U256::from(AP5_ATOMIC_GAS_LIMIT + 1));
        assert!(matches!(
            verify_ext_data_gas_used(&spec, &header, std::slice::from_ref(&tx)),
            Err(Error::TooLargeExtDataGasUsed(_))
        ));

        // (4) nil claim at AP4+ ⇒ reject (Go BigEqualUint64(nil, x) == false).
        header.ext_data_gas_used = None;
        assert!(verify_ext_data_gas_used(&spec, &header, std::slice::from_ref(&tx)).is_err());

        // (5) pre-AP4 ⇒ no-op even with a garbage claim: spec_from's third
        // parameter is the AP3..post-6 activation time, so pushing it
        // far-future turns AP4 off.
        let pre_ap4 = spec_from(u64::MAX, u64::MAX, u64::MAX);
        header.ext_data_gas_used = Some(U256::MAX);
        assert!(verify_ext_data_gas_used(&pre_ap4, &header, std::slice::from_ref(&tx)).is_ok());
    }
```

(Note `spec_from(…, ap3: u64::MAX)` also leaves the header's `time` in the `Launch` phase — that is exactly the pre-AP4 arm being tested. Every numbered arm above must land as a real assertion; split into separate `#[test]` fns if that reads better, keeping the arm comments.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p ava-evm -E 'test(gas_used_matches_go_formula) or test(ap5_constants) or test(verify_ext_data_gas_used)'`
Expected: compile FAIL.

- [ ] **Step 3: Implement**

**(a) Constants + method in `atomic/tx.rs`:**

```rust
/// coreth `plugin/evm/upgrade/ap5/params.go:33` — `AtomicGasLimit`: the
/// AP5..pre-Fortuna cap on a block's total atomic gas.
pub const AP5_ATOMIC_GAS_LIMIT: u64 = 100_000;
/// coreth `plugin/evm/upgrade/ap5/params.go:38` — `AtomicTxIntrinsicGas`: the
/// fixed per-atomic-tx gas charged from AP5 (`GasUsed(fixedFee=true)`).
pub const AP5_ATOMIC_TX_INTRINSIC_GAS: u64 = 10_000;

impl Tx {
    /// coreth `atomic/{import_tx.go:136-160, export_tx.go:134-153}` —
    /// `GasUsed(fixedFee)`. Priced over the UNSIGNED bytes (Go
    /// `Metadata.Bytes()`, `metadata.go:30` — see the `unsigned_bytes` field
    /// doc): `len·TxBytesGas` + per-input signature costs (+ the AP5 fixed
    /// fee). NOTE this is deliberately NOT [`crate::feerules::atomic_gas`]
    /// (the EVMInput/EVMOutput complexity accumulator) — the verify-side
    /// equality (`block_extension.go:174`) compares against THIS formula.
    ///
    /// # Errors
    /// [`Error::FeeOverflow`] on u64 overflow (Go `math.Add`).
    pub fn gas_used(&self, fixed_fee: bool) -> Result<u64, Error> {
        // tx.go:340-342 — calcBytesCost over unsigned bytes.
        let len = u64::try_from(self.unsigned_bytes.len()).map_err(|_| Error::FeeOverflow)?;
        let mut cost = TX_BYTES_GAS.checked_mul(len).ok_or(Error::FeeOverflow)?;
        match &self.unsigned {
            // import_tx.go:141-150 — Σ in.In.Cost() (secp: sigIndices·1000).
            AtomicTx::Import(tx) => {
                for input in &tx.imported_inputs {
                    let c = input.r#in.cost().map_err(|_| Error::FeeOverflow)?;
                    cost = cost.checked_add(c).ok_or(Error::FeeOverflow)?;
                }
            }
            // export_tx.go:135-143 — len(Ins) · CostPerSignature.
            AtomicTx::Export(tx) => {
                let ins = u64::try_from(tx.ins.len()).map_err(|_| Error::FeeOverflow)?;
                let sig_cost = ins.checked_mul(COST_PER_SIGNATURE).ok_or(Error::FeeOverflow)?;
                cost = cost.checked_add(sig_cost).ok_or(Error::FeeOverflow)?;
            }
        }
        // import_tx.go:151-156 / export_tx.go:145-150.
        if fixed_fee {
            cost = cost
                .checked_add(AP5_ATOMIC_TX_INTRINSIC_GAS)
                .ok_or(Error::FeeOverflow)?;
        }
        Ok(cost)
    }
}
```

(Adjust the `Error` import to the crate error type `atomic/tx.rs` already uses; if `input.r#in.cost()` returns a distinct `ava_vm` error, map it with a comment — its only failure is arithmetic overflow, `secp256k1fx types.rs:145`.)

**(b) Error variants** (`error.rs`):

```rust
    /// coreth `atomic/vm/block_extension.go:156` (`"too large extDataGasUsed: %d"`).
    /// Carries the claim as decoded (None mirrors Go's nil, which also fails
    /// `BigLessOrEqualUint64`).
    #[error("too large extDataGasUsed: {0:?}")]
    TooLargeExtDataGasUsed(Option<U256>),
    /// coreth `atomic/vm/block_extension.go:175`
    /// (`"invalid extDataGasUsed: have %d, want %d"`).
    #[error("invalid extDataGasUsed: have {have:?}, want {want}")]
    InvalidExtDataGasUsed { have: Option<U256>, want: u64 },
```

**(c) `atomic/verify.rs`:**

```rust
/// coreth `atomic/vm/block_extension.go:142-177` — the extension
/// `SemanticVerify`'s `ExtDataGasUsed` value check (AP4+): the claimed header
/// value must equal the recomputed atomic-batch gas
/// ([`Tx::gas_used`], fixed fee from AP5), bounded by
/// [`AP5_ATOMIC_GAS_LIMIT`] in the AP5..pre-Fortuna window (Fortuna+ the
/// bound is enforced by `VerifyGasUsed`'s capacity instead —
/// block_extension.go:152-154). Go's nil-claim semantics carry over: both
/// `BigLessOrEqualUint64(nil, …)` and `BigEqualUint64(nil, …)` are false
/// (`graft/evm/utils/numbers.go:42-54`), so an absent claim rejects.
///
/// # Errors
/// [`Error::TooLargeExtDataGasUsed`] / [`Error::InvalidExtDataGasUsed`] /
/// [`Error::FeeOverflow`].
pub fn verify_ext_data_gas_used(
    spec: &AvaChainSpec,
    header: &AvaHeader,
    atomic_txs: &[Tx],
) -> Result<(), Error> {
    let phase = spec.fork_at(header.time);
    // block_extension.go:148 — pre-AP4: nothing to check.
    if phase < AvaPhase::ApricotPhase4 {
        return Ok(());
    }
    let claimed = header.ext_data_gas_used;
    // block_extension.go:154-158 — AP5..pre-Fortuna bound.
    if phase >= AvaPhase::ApricotPhase5 && !spec.is_fortuna(header.time) {
        let within = matches!(claimed, Some(c) if c <= U256::from(AP5_ATOMIC_GAS_LIMIT));
        if !within {
            return Err(Error::TooLargeExtDataGasUsed(claimed));
        }
    }
    // block_extension.go:159-172 — Σ GasUsed(fixedFee = AP5+).
    let fixed_fee = phase >= AvaPhase::ApricotPhase5;
    let mut total: u64 = 0;
    for tx in atomic_txs {
        total = total
            .checked_add(tx.gas_used(fixed_fee)?)
            .ok_or(Error::FeeOverflow)?;
    }
    // block_extension.go:174-176 — equality (nil claim fails).
    if claimed != Some(U256::from(total)) {
        return Err(Error::InvalidExtDataGasUsed {
            have: claimed,
            want: total,
        });
    }
    Ok(())
}
```

**(d) Wire in `block.rs`** — in `verify_with_predicates`, directly after the Task 5 `if bootstrapped { … }` block:

```rust
        // coreth atomic extension SemanticVerify (block_extension.go:142-177)
        // — the ExtDataGasUsed value check. Unconditional at AP4+ (only the
        // shared-memory UTXO-presence half of the Go extension is
        // bootstrapped-gated; see Task 7's equivalence finding).
        crate::atomic::verify::verify_ext_data_gas_used(
            ctx.chain_spec(),
            self.header(),
            self.atomic_txs(),
        )?;
```

**Wired-order note:** on this path a `None` claim at AP4+ is already rejected earlier by `verify_header_gas_fields` (`NilExtDataGasUsed`) — the pure function still implements Go's nil-shape so its unit tests and any future caller match Go exactly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p ava-evm -E 'test(gas_used_matches_go_formula) or test(ap5_constants) or test(verify_ext_data_gas_used)'`
Expected: PASS. Then the full crate: `cargo nextest run -p ava-evm` — the pre-existing atomic suites (`cchain_atomic_tx.rs`, `atomic_*`) must stay green: any failure means an honest builder-stamped `ext_data_gas_used` disagrees with `Tx::gas_used` — STOP and reconcile against coreth (the builder stamps `CalcExtDataGasUsed` = the same `GasUsed(fixedFee)` sum) rather than loosening the check.

- [ ] **Step 5: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/atomic/tx.rs crates/ava-evm/src/atomic/verify.rs crates/ava-evm/src/error.rs crates/ava-evm/src/block.rs
git commit -m "feat(ava-evm): port atomic GasUsed(fixedFee) + block-extension ExtDataGasUsed value check (block_extension.go:142-177)"
```

---

### Task 7: SAE-field parse fail-close equivalence + the two check items

**Files:**
- Modify: `crates/ava-evm/src/block.rs` (unit test in the existing `#[cfg(test)]` module — `encode_rlp` is `pub(crate)`, unreachable from `tests/`)
- Modify: `crates/ava-evm/tests/PORTING.md` (rows)
- Modify: `docs/superpowers/specs/2026-07-19-cchain-semantic-verify-family-design.md` (check-item findings recorded as an AS-BUILT note)

**Interfaces:**
- Consumes: `AvaHeader::{encode_rlp, decode_rlp}` internals; the two plan-time check items from the design spec.
- Produces: documented findings; no new API.

- [ ] **Step 1: Write the failing parse fail-close test**

In `block.rs`'s existing test module (near the `syntactic_verify` test helpers, which already build self-consistent headers):

```rust
    #[test]
    fn trailing_sae_tail_field_fails_decode() {
        // Go's HeaderExtra carries six SAE-only optional tail fields beyond
        // MinDelayExcess (TargetExponent, MinPriceExponent, Settled{Height,
        // GasUnix,GasNumerator,Excess} — customtypes/header_ext.go:47-53)
        // that AvaHeader deliberately does not model. A coreth block carrying
        // any of them is rejected by Go at semanticVerify
        // (VerifyTargetExponent / VerifyMinPriceExponent / VerifySettled);
        // Rust rejects the same bytes at PARSE (decode_rlp's trailing-bytes
        // fail-close, block.rs:250-252). Same verdict, different stage — this
        // test pins the fail-close so a future codec change cannot silently
        // reopen it.
        let mut h = test_header(&[], None, None, Bytes::new()); // the module's
        // existing self-consistent builder (block.rs ~1566) — decode round-trip
        // only cares about the tail-field shape, not verify validity.
        h.time_milliseconds = Some(1_000);
        h.min_delay_excess = Some(1); // t7+t8 present so the tail is emitted
        let mut bytes = Vec::new();
        h.encode_rlp(&mut bytes);

        // Splice one extra RLP u64 (a would-be t9 = TargetExponent) into the
        // list payload and fix up the outer list header.
        let extended = {
            let header = RlpListHeader::decode(&mut &bytes[..]).expect("outer list");
            let payload_start = bytes.len() - header.payload_length;
            let mut payload = bytes[payload_start..].to_vec();
            1u64.encode(&mut payload); // the trailing SAE field
            let mut out = Vec::new();
            RlpListHeader { list: true, payload_length: payload.len() }.encode(&mut out);
            out.extend_from_slice(&payload);
            out
        };

        let mut cursor = &extended[..];
        assert!(
            AvaHeader::decode_rlp(&mut cursor).is_err(),
            "a header with an SAE tail field must fail decode (fail-close)"
        );
        // Control: the unspliced bytes still decode.
        let mut ok_cursor = &bytes[..];
        AvaHeader::decode_rlp(&mut ok_cursor).expect("honest header decodes");
    }
```

(Use the same `RlpListHeader`/`Encodable` imports the encode/decode impls use in this file; the header-builder to reuse is whatever `block.rs`'s existing tests construct — see the "self-consistent, minimal test header" helper documented around block.rs:1558.)

- [ ] **Step 2: Run test to verify it passes immediately**

Run: `cargo nextest run -p ava-evm -E 'test(trailing_sae_tail_field)'`
Expected: PASS on first run — this pins EXISTING fail-closed behavior (it is an equivalence guard, not a fix; the TDD fail-first rule does not apply to a pin of already-correct behavior — verify it can fail by temporarily commenting the `!body.is_empty()` check locally if in doubt, then restore).

- [ ] **Step 3: Investigate check item 1 (predicate-pass gating)**

Read `crates/ava-evm/src/precompile/warp.rs::build_block_predicates` and every caller (grep `build_block_predicates` across `crates/`). Determine where the warp predicate pass runs relative to `EvmVm`'s verify path and whether it is `bootstrapped`-gated as Go's is (`wrapped_block.go:376-386`).

Expected finding (from plan-time reconnaissance — verify, don't assume): the pass exists but is NOT invoked from `VerifiedEvmBlock::verify`'s prod path (`AvaExecCtx::default()` is passed; the doc comment on `verify_with_predicates` describes a `ChainVm` adapter step that grep does not find as a caller). If confirmed: this is a pre-existing M6.31-scoped deferral, NOT a one-line gate — record it (Step 5) as a named follow-up ("warp predicate pass not wired on the prod verify path; when it is wired, gate it on the same `bootstrapped` flag Task 4 added") and do NOT attempt the wiring in this branch. If instead a real caller exists, add the one-line `bootstrapped` gate there and a matching test.

- [ ] **Step 4: Investigate check item 2 (atomic UTXO presence)**

Go's `verifyUTXOsPresent` (`block_extension.go:179-190`, bootstrapped-gated) checks shared memory contains every import-tx UTXO at verify time. Trace the Rust equivalence: `block.rs:967` passes `NoopPreHook` at verify — find where import-tx UTXOs are actually resolved against shared memory on the verify path (candidates: `atomic/backend.rs`'s fetch during execution, the `ChainVm` adapter, or nowhere). Deliverable, one of:
- **Equivalence confirmed:** absent shared-memory UTXOs make verify fail via execution (fail-closed) → record the mechanism with file:line in PORTING.md + the design spec AS-BUILT note.
- **Gap confirmed:** Rust verify accepts a block whose import UTXOs are absent (Go rejects when bootstrapped) → a fifth fail-open; add `verify_utxos_present` to `atomic/verify.rs` mirroring `block_extension.go:179-190` under the Task 4 `bootstrapped` flag, with a unit test (shared-memory mock returning missing), in THIS task.

- [ ] **Step 5: Record findings + PORTING rows**

In `crates/ava-evm/tests/PORTING.md`, add/update rows (statuses per actual findings):

```markdown
| VerifyTime (customheader/time.go) | ✅ | feerules::verify_time + semantic_verify.rs |
| VerifyMinDelayExcess (min_delay_excess.go:45) | ✅ | feerules::verify_min_delay_excess |
| VerifyGasUsed / GasCapacity (gas_limit.go:61,164) | ✅ | feerules::{verify_gas_used,gas_capacity} |
| verifyIntrinsicGas (wrapped_block.go:287) | ✅ | EvmBlock::verify_intrinsic_gas (bootstrapped-gated) |
| blockExtension.SemanticVerify ExtDataGasUsed (block_extension.go:142) | ✅ | atomic::verify::verify_ext_data_gas_used + Tx::gas_used |
| VerifyTargetExponent / VerifyMinPriceExponent / VerifySettled | n/a | parse fail-close equivalence — AvaHeader::decode_rlp rejects the SAE tail fields (trailing_sae_tail_field_fails_decode) |
| verifyUTXOsPresent (block_extension.go:179) | <per Step 4> | <mechanism or new port> |
```

Append the Step 3/4 findings to the design spec under a new `## AS-BUILT notes` heading (2-6 lines each, file:line cited).

- [ ] **Step 6: Lint + commit**

```bash
./scripts/nix_run.sh cargo fmt && cargo clippy -p ava-evm --all-targets -- -D warnings
git add crates/ava-evm/src/block.rs crates/ava-evm/tests/PORTING.md docs/superpowers/specs/2026-07-19-cchain-semantic-verify-family-design.md
git commit -m "test(ava-evm): pin SAE-tail parse fail-close; record predicate-gating + UTXO-presence equivalence findings"
```

(If Step 4 found a gap, this becomes two commits: the `verify_utxos_present` port with its test, then the docs.)

---

### Task 8: Recorded Go-oracle verdict corpus extension

**Files:**
- Modify: `crates/ava-evm/tests/proposer_candidates.rs` (new mutation entries in the emitter's mutation table + matching sentinel expectations in `proposer_verdicts_hold`)
- Modify: `tests/differential/go-oracle/rust_built_block_verdict_test.go` (only if the judge needs `SetState(NormalOp)` — check first)
- Regenerate: `crates/ava-evm/tests/vectors/proposer_verdict/*` (new `.rlp.hex` + `verdicts.json`)

**Interfaces:**
- Consumes: Tasks 1-6 sentinels; the two-step recorded-oracle workflow documented at the top of `proposer_candidates.rs` (emit → Go judge → committed verdicts).
- Produces: an extended committed corpus proving Go and Rust reject each new mutant for the same reason.

- [ ] **Step 1: Read the existing emitter + judge**

Read `proposer_candidates.rs` fully (the mutation-table shape, how each mutant restamps to stay self-consistent, how `proposer_verdicts_hold` maps candidate name → (Go sentinel substring, Rust sentinel substring)). Read `tests/differential/go-oracle/rust_built_block_verdict_test.go` and confirm whether the coreth judge VM is bootstrapped (`SetState(snow.NormalOp)`) before `Verify` — the intrinsic-gas mutant needs it on BOTH sides. If the Go judge is not bootstrapped, add the `SetState` call there (mirroring how coreth's own tests do it) — Go-side change stays inside the drop-in test file.

- [ ] **Step 2: Add the new mutation entries**

Extend the emitter's mutation list with six candidates (names → mutation → expected sentinels). Mutants marked *restamp* recompute the ACP-176 extra prefix exactly as Task 5's integration test does (`fee_state_after_block(...).to_bytes()` spliced over the first `STATE_SIZE` bytes):

| Candidate | Mutation | Go sentinel (substring) | Rust sentinel (substring) |
|---|---|---|---|
| `missing_time_milliseconds` | `time_milliseconds = None` | `TimeMilliseconds is required` | `TimeMilliseconds is required` |
| `mismatched_time_milliseconds` | `time_milliseconds += 5_000` (ms only) | `TimeMilliseconds does not match` | `TimeMilliseconds does not match` |
| `far_future_time` | `time = 64_060_588_800; time_milliseconds = Some(time*1000)` | `too far in the future` | `too far in the future` |
| `wrong_min_delay_excess` | `min_delay_excess = Some(u64::MAX)` | `incorrect min delay excess` | `incorrect min delay excess` |
| `understated_gas_used` (*restamp*) | `gas_used = 0` | `intrinsic gas` | `intrinsic gas` |
| `trailing_sae_tail_field` | splice one extra RLP u64 into the header list payload (the Task 7 technique — emit raw bytes rather than going through `assemble_ava_block`, which cannot represent the field) | `remote target exponent` / whichever `VerifyTargetExponent` message Go produces — capture from the recorded verdict, don't guess | Rust rejects at PARSE: assert `parse_block` errors (the verdict checker needs a small extension to accept parse-stage rejection for this candidate) |

The same saturated-advance argument as Task 4 makes the first four restamp-free (the honest candidate is a child of the local genesis, years elapsed). Follow the existing per-mutation comment convention: each entry states WHY the mutant is self-consistent.

- [ ] **Step 3: Extend `proposer_verdicts_hold`**

Add the six names to the expected-verdict map with both sentinel substrings; for `understated_gas_used`, the Rust-side re-verification must run bootstrapped (`vm.set_state(&token, EngineState::NormalOp).await` in the checker's vm-boot helper — mirror Task 5's `parse_and_verify_bootstrapped`); for `trailing_sae_tail_field`, assert the Rust rejection comes from `parse_block` (verify is never reached).

- [ ] **Step 4: Re-record the corpus**

```bash
./scripts/check_oracle_binary.sh                    # must print OK
EMIT_PROPOSER_CANDIDATES=$PWD/crates/ava-evm/tests/vectors/proposer_verdict \
  cargo test -p ava-evm --test proposer_candidates -- --exact emit_proposer_candidates
cp tests/differential/go-oracle/rust_built_block_verdict_test.go ~/avalanchego/graft/coreth/plugin/evm/
cd ~/avalanchego && AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
RUST_BLOCK_VERDICT_DIR=$OLDPWD/crates/ava-evm/tests/vectors/proposer_verdict \
  go test -run TestRustBuiltBlockVerdicts ./graft/coreth/plugin/evm/ && \
  rm graft/coreth/plugin/evm/rust_built_block_verdict_test.go
cd $OLDPWD
```

Expected: the Go judge accepts the honest candidate and rejects all mutants (old + new). Inspect `verdicts.json` — each new mutant's recorded Go error must contain the table's sentinel. **If a Go verdict disagrees with the expectation (e.g. Go rejects with a different earlier sentinel), that is a finding, not a test bug: trace the Go rejection order, fix the Rust port's ordering to match, and re-record.**

- [ ] **Step 5: Run the verdict parity test**

Run: `cargo nextest run -p ava-evm -E 'test(proposer_verdicts_hold)'`
Expected: PASS — Go and Rust reject every mutant for the same reason.

- [ ] **Step 6: Atomic-mutant leg (export candidate)**

Attempt an export-tx-bearing candidate so the `ExtDataGasUsed` equality gets a cross-binary check: the emitter builds a signed `ExportTx` spending the funded genesis EVM account (crib the construction + signing from `crates/ava-evm/tests/cchain_atomic_tx.rs`, which drives exactly this through `AtomicMempool` — the emitter already imports `AtomicMempool`), lets `build_block` pack it, then adds one mutant `inflated_ext_data_gas_used` (*restamp*, claim+1 → Go `invalid extDataGasUsed` / Rust `invalid extDataGasUsed`). Export txs need no shared-memory UTXOs, so the Go judge verifies them without extra fixtures (`verifyUTXOsPresent` checks imports only).

**Fallback (use only if the export candidate cannot pass the Go judge within this task — e.g. the judge VM rejects the atomic tx for a fixture reason unrelated to this branch):** document the blocked mechanism in the emitter with a `// DEFERRED:` comment naming the exact Go error, mark the PORTING.md `verify_ext_data_gas_used` row `🟡` with "unit + golden-constant coverage; oracle leg deferred: <reason>", and note it in the plan AS-BUILT. Do not silently skip.

- [ ] **Step 7: Commit**

```bash
git add crates/ava-evm/tests/proposer_candidates.rs crates/ava-evm/tests/vectors/proposer_verdict/ tests/differential/go-oracle/rust_built_block_verdict_test.go
git commit -m "test(ava-evm): verdict corpus grows semantic-verify mutations — Go and Rust reject identically (recorded oracle re-run)"
```

---

### Task 9: Closeout — docs, workspace gates, live gates

**Files:**
- Modify: `plan/M9-interop-hardening.md` (AS-BUILT callout under the verifyHeaderGasFields section: the d41cee0 residual list is now closed / re-scoped per Task 7 findings)
- Modify: `specs/10-*.md` (the C-Chain spec's verify-surface section — same callout style as the verifyHeaderGasFields entry; add the "Upstream delta"-style note only if `~/avalanchego` HEAD moved)
- No code.

- [ ] **Step 1: Docs**

Add the AS-BUILT callout to `plan/M9-interop-hardening.md` mirroring the existing verifyHeaderGasFields AS-BUILT block: what landed (5 ports + equivalence pin), the Task 7 findings verbatim, the corpus growth, and any deferral from Task 8's fallback. Update the spec 10 callout that enumerated the residual gaps (the one d41cee0 added) to point at the new functions.

- [ ] **Step 2: Full workspace gates** (the per-task scoped lint missed rustfmt drift last branch — run repo-wide)

```bash
./scripts/run_task.sh lint-all
./scripts/run_task.sh test-unit
```

Expected: both green. Fix anything surfaced (fmt drift, cross-crate breakage from the `with_clock` move) before proceeding.

- [ ] **Step 3: Live gates**

Prewarm a freshly-relinked release binary (macOS first-exec stall), then rerun both live arms:

```bash
./scripts/check_oracle_binary.sh
cargo build -p avalanchers --release && ./target/release/avalanchers --version
cargo test --test mixed_network -- --ignored --exact --nocapture mixed_network
cargo test --test mixed_network -- --ignored --exact --nocapture mixed_network_rust_proposes
```

(Exact invocation per the M9.15 gotcha notes: run via `cargo test`, NOT nextest — the 120s slow-timeout kills live arms. `$AVALANCHEGO_PATH` must point at the oracle binary.)
Expected: both PASS — the honest arm must survive the new checks (the builder already stamps `time_milliseconds` + `min_delay_excess`). A failure here is a false-reject bug in a port — debug with the Go source, do not weaken the check.

- [ ] **Step 4: Final commit**

```bash
git add plan/M9-interop-hardening.md specs/
git commit -m "docs: semantic-verify family AS-BUILT — d41cee0 residual verify-surface gaps closed (VerifyTime/MinDelayExcess/GasUsed/intrinsic/atomic ExtDataGasUsed)"
```

Then invoke **superpowers:finishing-a-development-branch**.

---

## Self-review notes (spec coverage)

- Spec "Problem" gaps 1-4 → Tasks 1-6. SAE trio + Helicon → Task 7 + wiring comments (Task 4 Step 3c). Threading section → Task 4. Testing layer 1 → Tasks 1-7; layer 2 → Task 8; layer 3 → Task 9. Plan-time check items 1-2 → Task 7 Steps 3-4. Intrinsic-gas source decision → resolved: reuse the existing `mempool::intrinsic_gas` libevm port (Task 5), oracle-arbitrated (Task 8).
- Known intentional deviations from a literal Go transliteration, each documented inline: intrinsic-gas overflow saturates (verdict-identical), `verify()`'s bootstrap-shape defaults (Go has no clock-free entry), atomic check placement before execution (order-independent; corpus asserts verdicts).

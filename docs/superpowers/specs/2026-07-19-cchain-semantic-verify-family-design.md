# C-Chain semantic-verify family port — VerifyTime + siblings

**Date:** 2026-07-19
**Status:** Approved (brainstorming session, section-by-section)
**Predecessor:** `verifyHeaderGasFields` port (merged `5c4ae3b`); this closes the residual
verify-surface gaps enumerated in its final review (`d41cee0`).

## Problem

Go's C-Chain block verification runs a `semanticVerify` stage
(`graft/coreth/plugin/evm/wrapped_block.go:335-391`) after `syntacticVerify`. Rust ports the
syntactic stage fully (including last branch's `feerules::verify_header_gas_fields`) but has
**no semantic stage at all**. Four Go rejection surfaces therefore have no Rust mirror — each
a Byzantine-proposer fail-open (Go rejects, Rust accepts → silent split). All are
honest-arm-safe today (the Rust builder stamps correct values; every live gate is green), but
they must close before any BFT-exposed deployment claim:

1. **`customheader.VerifyTime`** (`customheader/time.go:55-124`) — Rust does no timestamp
   verification. `header.time_milliseconds` is consumed as a *trusted input* by the fee checks
   (via the `HeaderTimeMilliseconds` fallback). A Granite header with missing/inconsistent
   `TimeMilliseconds` that Go rejects can pass Rust if the fee fields are stamped
   self-consistently at the fallback milliseconds.
2. **`customheader.VerifyMinDelayExcess`** (`customheader/min_delay_excess.go:45-81`) — the
   ACP-226 excess equality recompute. Rust has the builder-side calculation
   (`feerules::min_delay_excess_of`) but never verifies a remote header's claim.
3. **`verifyIntrinsicGas`** (`wrapped_block.go:287-332`) = `customheader.VerifyGasUsed`
   (claimed `GasUsed` + `ExtDataGasUsed` must fit the block's gas capacity) + the summed
   per-tx intrinsic gas must not exceed claimed `GasUsed`. Rust's executor independently
   recomputes `gas_used` at execution — a different, currently-sufficient check — but Go's
   pre-execution capacity/intrinsic rejection surface has no direct mirror.
4. **Atomic `ExtDataGasUsed` value check** (`atomic/vm/block_extension.go:142-177`) — claimed
   `ExtDataGasUsed` must equal the recomputed atomic-batch gas (AP4+), bounded by the AP5
   `AtomicGasLimit` pre-Fortuna. Rust never compares the claim; at Fortuna+ an inflated claim
   self-consistently stamped into the ACP-176 extra prefix passes `verify_extra_prefix`
   (the claim feeds the recompute) — Go rejects, Rust accepts.

Three further Go checks in the same stage — `VerifyTargetExponent`, `VerifyMinPriceExponent`,
`VerifySettled` — reject coreth blocks carrying SAE-only header fields. These need **no port**:
Go's `HeaderExtra` carries six optional tail fields Rust's `AvaHeader` does not
(`TargetExponent`, `MinPriceExponent`, `Settled{Height,GasUnix,GasNumerator,Excess}`), and the
Rust RLP decoder **fail-closes on trailing tail bytes** (`block.rs:250-252`,
`RlpError::UnexpectedLength`), so any block carrying them is rejected at *parse* where Go
rejects at *verify*. Same verdict, different stage, no consensus split. They get equivalence
tests + PORTING rows only.

The `errIsHeliconBlock` guard (`wrapped_block.go:368`) is n/a: Helicon is unscheduled on every
network and `AvaPhase` carries no `Helicon` variant — folded into the existing
"when Helicon lands" callout from the predecessor branch.

## Architecture

Add the semantic stage inside `EvmBlock::verify_with_predicates`
(`crates/ava-evm/src/block.rs:918`), in **Go's exact call order**:

```
syntactic_verify                       (exists — coreth syntacticVerify port)
feerules::verify_header_gas_fields     (exists — dummy-engine header checks)
── new semantic stage (wrapped_block.go:335-391 order) ──
feerules::verify_min_delay_excess      (wrapped_block.go:345)
[VerifyTargetExponent]                 — parse fail-close; equivalence test only
[VerifyMinPriceExponent]               — parse fail-close; equivalence test only
feerules::verify_time                  (wrapped_block.go:359)
[VerifySettled]                        — parse fail-close; equivalence test only
[errIsHeliconBlock]                    — n/a (no Helicon variant; existing callout)
if bootstrapped:
    EvmBlock::verify_intrinsic_gas     (wrapped_block.go:377)
atomic::verify::verify_ext_data_gas_used  (extension.SemanticVerify)
── end new stage ──
atomic conflict verify, EVM execution…  (exists)
```

Note on placement: Go runs `extension.SemanticVerify` after the predicate pass; the atomic
`ExtDataGasUsed` check is order-independent of predicates (pure header/tx arithmetic), so the
stage groups it with the other new checks. Any observable ordering difference in error
*selection* for doubly-invalid blocks is acceptable — the recorded-oracle corpus asserts
verdicts, not error precedence — but implementation should keep Go's relative order wherever
it costs nothing.

## Components

Four new functions, each file-mapped to its Go source, with **verbatim Go sentinel strings**
carried in new `ava-evm` `Error` variants (the same pattern as `verify_header_gas_fields`):

### `feerules::verify_time(spec, parent, header, now_ms) -> Result<(), Error>`
Port of `customheader/time.go:55-124`. Checks, in Go order:
1. `header_time_ms(header) >= header_time_ms(parent)` — both via the `HeaderTimeMilliseconds`
   fallback (`time_milliseconds` if present, else `time * 1000`); equality allowed
   (pre-Granite multiple blocks per timestamp). Sentinel `errBlockTooOld`
   ("block timestamp is too old").
2. `header_time_ms <= now_ms + MAX_FUTURE_BLOCK_TIME_MS` where
   `MAX_FUTURE_BLOCK_TIME_MS = 10_000` (Go `MaxFutureBlockTime = 10 * time.Second`).
   Sentinel `ErrBlockTooFarInFuture`.
3. Pre-Granite (`!spec.is_granite(header.time)`): `time_milliseconds` must be `None`
   (`ErrTimeMillisecondsBeforeGranite`), then return Ok.
4. Granite: `time_milliseconds` required (`ErrTimeMillisecondsRequired`).
5. Granite: `header.time == time_milliseconds / 1000` (`ErrTimeMillisecondsMismatched`).
6. Min-delay: if `parent.min_delay_excess` is `Some` (first-Granite-block parent exempt, as
   in Go), `header_time_ms − parent_time_ms >= DelayExcess(parent_excess).delay()`
   (`ErrMinDelayNotMet`). The subtraction cannot underflow — check 1 established ordering.

### `feerules::verify_min_delay_excess(spec, parent, header) -> Result<(), Error>`
Port of `customheader/min_delay_excess.go:45-81`. Granite-only (else Ok). Header's
`min_delay_excess` must be `Some` (`errRemoteMinDelayExcessNil`) and must equal the recompute
from the **existing** `min_delay_excess_of(spec, parent, timestamp, desired)` called with the
*claimed* excess as `desired` — Go's claimed-as-desired trick: the expected value equals the
claim iff the claim was reachable from the parent (`errIncorrectMinDelayExcess`).

### `feerules::verify_gas_used(spec, parent, header) -> Result<(), Error>`
Port of `customheader/gas_limit.go:63-98` + `GasCapacity` (`gas_limit.go:164-180`).
- Fortuna+ with `ext_data_gas_used` present: claim must fit `u64`
  (`errInvalidExtraDataGasUsed`), then checked-add onto `gas_used` (overflow → error, Go
  `math.Add`).
- Capacity composes existing ports: pre-Fortuna → `gas_limit()` (capacity == gas limit);
  Fortuna+ → `fee_state_before_block()` capacity (the ACP-176 state at
  `header_time_ms(header)`).
- `gas_used_total > capacity` → `errInvalidGasUsed` ("invalid gas used" / "have %d,
  capacity %d" shape).

### `EvmBlock::verify_intrinsic_gas(spec, parent) -> Result<(), Error>`
Port of `wrapped_block.go:287-332` (lives in `block.rs` — needs transaction access):
1. `feerules::verify_gas_used(spec, parent, header)` (wrapped in Go's
   `errInvalidGasUsedRelativeToCapacity` context).
2. Σ per-tx intrinsic gas over the block's txs, checked-add
   (`errTotalIntrinsicGasCostExceedsClaimed` on overflow), then
   `total_intrinsic > claimed gas_used` → `errTotalIntrinsicGasCostExceedsClaimed`.

**Intrinsic-gas source (plan-time decision):** use reth/revm's intrinsic-gas function iff its
fork-rule shape matches coreth's `ethcore.IntrinsicGas(data, accessList, isCreate, rules)`
1:1 for the C-Chain's active rule set; otherwise port coreth's function (~40 lines of pure
arithmetic) into `feerules`. The recorded-oracle corpus arbitrates either way.

**Gating:** runs only when `bootstrapped == true`, exactly mirroring Go
(`wrapped_block.go:376`) — during bootstrap, canonically-accepted blocks are guaranteed to
pass and required indices may be absent.

### `atomic::verify::verify_ext_data_gas_used(rules, header, atomic_txs) -> Result<(), Error>`
Port of `atomic/vm/block_extension.go:142-177`. AP4+ only (else Ok):
- AP5-and-not-Fortuna: claimed `ext_data_gas_used <= AtomicGasLimit` (100_000) — sentinel
  `"too large extDataGasUsed"`. (Fortuna+ the bound is enforced by `verify_gas_used` capacity,
  as Go's comment notes.)
- Σ atomic-tx gas via the **existing** `feerules::atomic_gas` accumulator (fixed fee charged
  at AP5+, matching `atomicTx.GasUsed(fixedFee)`), checked-add.
- Claimed `ext_data_gas_used` must **equal** the recomputed total — sentinel
  `"invalid extDataGasUsed: have %d, want %d"`. Note Go compares a `*big.Int` claim (nil claim
  fails equality against any total, incl. 0 with nil — `BigEqualUint64(nil, x)` is false);
  Rust must mirror: AP4+ with `None` claim → reject.

### Error handling
New `Error` variants in `crates/ava-evm/src/error.rs` with Go-verbatim sentinel message
fragments (the recorded-oracle corpus and unit tests assert on them):
`errBlockTooOld`, `ErrBlockTooFarInFuture`, `ErrTimeMillisecondsRequired`,
`ErrTimeMillisecondsMismatched`, `ErrTimeMillisecondsBeforeGranite`, `ErrMinDelayNotMet`,
`errRemoteMinDelayExcessNil`, `errIncorrectMinDelayExcess`, `errInvalidGasUsed`,
`errInvalidExtraDataGasUsed`, `errInvalidGasUsedRelativeToCapacity`,
`errTotalIntrinsicGasCostExceedsClaimed`, `"too large extDataGasUsed"`,
`"invalid extDataGasUsed"`. All arithmetic `checked_*`/`saturating_*` per repo convention
(no raw casts; `arithmetic_side_effects` clean).

## Data flow — threading `now` and `bootstrapped`

`verify_time` needs a wall clock; `verify_intrinsic_gas` needs the engine phase. Both live on
`EvmVm` (`clock: Arc<dyn Clock>`, `engine_state: EngineState`) but verification runs on
`VerifiedEvmBlock`, which holds `Arc<Shared>` + `EvmBlockContext`. Go reads both **live at
verify time** (`b.vm.clock.Time()`, `b.vm.bootstrapped.Get()`) — wrap-time copies would be
wrong (a block wrapped during bootstrap can be re-verified after bootstrap completes and must
see the post-bootstrap rules).

Mirror Go via `Shared` (the same seam last branch used for `parent_header`):
- `Shared.clock: Arc<dyn Clock>` — a clone of the VM's injected clock, so `EvmVm::with_clock`
  still governs tests and the determinism gate is satisfied (the only wall-clock read goes
  through the injected `Arc<dyn Clock>`; specs/24 hazard #5).
- `Shared` gains an atomic bootstrapped flag (e.g. `AtomicBool`) that `EvmVm::set_state`
  updates — the Go `utils.Atomic[bool] vm.bootstrapped` analog.

`VerifiedEvmBlock::verify` reads both and passes `now_ms: u64` + `bootstrapped: bool` as
plain arguments into `verify_with_predicates`. `EvmBlockContext` is untouched; all new
feerules functions stay pure (spec + headers + scalars in, Result out).

## Testing

Three-layer gate, identical in shape to the `verifyHeaderGasFields` branch:

1. **Unit (TDD, per function).** Table tests covering the accept arm, every reject arm, and
   boundaries: equal timestamps allowed; future bound exactly at `now+10s` (accept) vs one ms
   over (reject); min-delay exact boundary; Granite on/off for every time check;
   first-Granite-block parent (no `min_delay_excess`) exemption; AP4/AP5/Fortuna phase matrix
   for both gas checks; `None`-claim rejection at AP4+; intrinsic-sum overflow; parse-reject
   equivalence tests feeding Go-shaped header RLP carrying each of the six SAE tail fields
   (assert `RlpError::UnexpectedLength`-mapped parse failure) — pinning the
   fail-closed-at-parse equivalence for `VerifyTargetExponent`/`VerifyMinPriceExponent`/
   `VerifySettled`.
2. **Recorded Go-oracle verdict corpus.** Extend the env-gated emitter copied into
   `~/avalanchego` + the committed corpus with ~8-10 new mutations of an honest block:
   timeMS/Time mismatch, missing timeMS at Granite, timeMS set pre-Granite, too-far-future
   timestamp, min-delay violation, wrong `min_delay_excess`, `gas_used` over capacity,
   intrinsic total > claimed `gas_used`, inflated atomic `ExtDataGasUsed`, over-`AtomicGasLimit`
   claim (AP5 arm), trailing SAE tail field. Assert Go and Rust reject identically and both
   accept the honest block. Run `./scripts/check_oracle_binary.sh` before recording
   (mandatory pre-gate; oracle pin `~/avalanchego` HEAD, rpcchainvm=45).
3. **Live gates.** Rerun `mixed_network` + `mixed_network_rust_proposes` against the live Go
   cluster (prewarm a freshly-relinked release binary — macOS first-exec stall). The honest
   arm must stay green: the Rust builder already stamps `time_milliseconds` +
   `min_delay_excess` (builder.rs:484-485) and correct gas fields. Closeout: full-workspace
   `lint-all` + `test-unit` (the per-task-scoped-lint gap from last branch's process notes).

**Plan-time check items:**
1. Go gates the *predicate pass* on `bootstrapped` too (`wrapped_block.go:376-386`). Verify
   the existing Rust predicate pass matches; if not, gate it on the same new flag in this
   branch (one line).
2. Go's `extension.SemanticVerify` also runs a bootstrapped-gated `verifyUTXOsPresent`
   (shared-memory presence of import-tx UTXOs, `block_extension.go:179-190`). Rust has no
   direct analog in `atomic/`; confirm the atomic Import pre-hook fail-closes at verify-time
   execution when shared-memory UTXOs are absent (the expected equivalence). If it does,
   record the equivalence in PORTING.md; if it does not, that is a fifth fail-open — add the
   check to this branch's atomic verify component.

## Risks / notes

- **Intrinsic-gas parity** is the main divergence risk (reth vs coreth fork-rule shapes);
  mitigated by the plan-time 1:1 comparison and the oracle mutation corpus.
- **False-reject risk on the honest arm** (the inverse failure): `verify_time`'s future bound
  introduces the verify path's first wall-clock dependency — live-gate reruns cover it; the
  10s bound is generous against real cluster skew.
- **Helicon:** when Helicon lands upstream, `verify_time`'s Granite arm, the
  `errIsHeliconBlock` guard, `VerifyExtra`, and `VerifyExtraPrefix` all need arms together —
  extend the existing single callout (predecessor branch, plan AS-BUILT).
- Out of scope: tx gossip, `VerifyTime`'s SAE/saevm analog, the M9.15 follower follow-up
  sweep, nightly live-arm cadence.

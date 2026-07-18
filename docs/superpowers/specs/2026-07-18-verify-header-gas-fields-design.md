# verifyHeaderGasFields port — C-Chain verify-path fee/gas equality checks

**Date:** 2026-07-18
**Status:** Approved (brainstorming complete; awaiting implementation plan)
**Predecessor:** M9.15 rust-as-proposer arc (merge `74625f5`); this closes the
whole-branch review's flagged fail-open (plan/M9-interop-hardening.md AS-BUILT
addendum, item (2)).

## Problem

coreth's complete block verification runs `verifyHeaderGasFields`
(`consensus/dummy/consensus.go:125-176`), which **recomputes and
equality-checks** four fee/gas header fields against the parent. The Rust
C-Chain verify path (`EvmBlock::syntactic_verify`) checks these fields only
**structurally** (nil-ness / length):

| Field | Go check | Rust today |
|---|---|---|
| `GasLimit` | `VerifyGasLimit` — per-fork equality/range vs recompute | no check |
| ACP-176 extra prefix | `VerifyExtraPrefix` — full fee-state struct equality | length ≥ 24/80 only |
| `BaseFee` | `BigEqual(header.BaseFee, BaseFee(parent, timeMS))` | non-nil at AP3+ only |
| `BlockGasCost` | `BigEqual(extra.BlockGasCost, BlockGasCost(parent, time))` | non-nil + u64 at AP4+ only |
| `ExtDataGasUsed` | pre-AP4 nil; AP4+ non-nil + `IsUint64` | no check |

None of these affects an empty block's EVM state root, so a **Byzantine
proposer** can craft a block with a valid state root but wrong fee metadata
that Go rejects and Rust accepts — a silent Rust-vs-Go consensus split (the
same class the Cancun clamp closed). Not triggered on the honest arm (the Rust
builder stamps these fields correctly; live gates pass), but it must be ported
before any adversarial/BFT-exposed deployment.

**Coupled prerequisite:** `feerules::base_fee`'s ACP-176 arm reads
`gas_price()` off the **raw** parent fee state, skipping coreth's
`feeStateBeforeBlock` time-advance (`customheader/base_fee.go:27-33`). It is
byte-exact only at `excess ≈ 0`. If verify recomputed the expected base fee
through today's `base_fee`, Rust would falsely REJECT honest Go blocks under
sustained load — trading fail-open for fail-closed. The time-advance fix is
therefore in scope (user decision).

## Scope

**In scope**
1. Verify-side `feerules` functions: `verify_gas_limit`, `verify_extra_prefix`,
   and the `verify_header_gas_fields` orchestrator (Go check order).
2. `base_fee` time-advance fix at the source (signature refactor to the parent
   `AvaHeader`), shared by builder, RPC, and verifier.
3. Threading the parent `AvaHeader` into `EvmBlock::verify_with_predicates`.
4. Tests: per-arm units, recorded Go-oracle mutation-corpus extension (incl. a
   nonzero-excess false-reject guard), live follower + proposer re-run.

**Out of scope (explicit)**
- The rest of Go `verifyHeader`'s contextual checks: `number == parent+1` and
  timestamp ordering are enforced at the consensus/snowman layer;
  gas-used-vs-limit (`verifyIntrinsicGas`) is covered by execution's gas
  assertion in `EvmBlock::verify`.
- Helicon semantics — `AvaPhase` has no Helicon variant (unscheduled on every
  network). Documented as an upstream-delta callout, mirroring the existing
  `VerifyExtra` one: under Helicon, `VerifyExtraPrefix`'s Fortuna arm changes
  (the ACP-176 state-space floor leaves `header.Extra`).
- Tx gossip and every other deferred M9.15 follow-up.

## Design

### Approach (chosen: faithful customheader-module port)

Mirror Go one-to-one in `crates/ava-evm/src/feerules/` — the repo's established
porting convention (exact Go line citations, Go check order, sentinel parity;
same as the `wrappedBlock.syntacticVerify` port). Rejected alternatives:
inline checks in `block.rs` (duplicates expectation logic; lets builder and
verifier compute base fee two different ways — the exact drift class this port
kills) and a unified builder+verifier `ExpectedGasFields` computation (max DRY
but diverges from coreth's structure, weakening line-cited parity review — the
safety argument here).

### New functions (`crates/ava-evm/src/feerules/mod.rs`)

All three take `(spec: &AvaChainSpec, parent: &AvaHeader, header: &AvaHeader)`.

**`verify_gas_limit`** — port of `customheader.VerifyGasLimit`
(`plugin/evm/customheader/gas_limit.go:101-145`):
- Fortuna+: `header.gas_limit ==
  fee_state_before_block(spec, parent, header_time_ms(header))?.max_capacity()`.
- Cortina+: `== 15_000_000`; AP1+: `== 8_000_000`.
- Pre-AP1: range `[ap0::MinGasLimit, ap0::MaxGasLimit]` (constants from
  `graft/coreth/plugin/evm/upgrade/ap0`).

**`verify_extra_prefix`** — port of `customheader.VerifyExtraPrefix`
(`plugin/evm/customheader/extra.go:62-108`):
- Fortuna+: parse the **claimed** ACP-176 state from `header.extra`
  (`Acp176State::from_bytes`); recompute the expected state via the existing
  `fee_state_after_block`, passing `Some(claimed.target_excess)` as its
  `desired_target_excess` — Go's clamp trick (`extra.go:74-84`): the expected
  value equals the claim iff the claim was reachable in one step; full-struct
  equality against the claimed state.
- AP3+ (pre-Fortuna): recompute the fee window via the existing `fee_window`
  and require `header.extra` starts with its bytes.
- Pre-AP3 / (Helicon): no check (documented arm).

**`verify_header_gas_fields`** — port of `verifyHeaderGasFields`
(`consensus/dummy/consensus.go:125-176`), in Go's exact order:
1. `verify_gas_limit(spec, parent, header)?`
2. `verify_extra_prefix(spec, parent, header)?`
3. Expected base fee: at AP3+, `base_fee(spec, parent, ctx)` with a ctx the
   orchestrator builds itself — `timestamp`/`timestamp_ms` from the header
   under verification, `parent_fee_state` via the existing
   `parent_fee_state_of(spec, parent)` (needed by the window arm; the ACP-176
   arm re-derives from `parent.extra` after the fix); **exact `Option`
   equality** with
   `header.base_fee` — pre-AP3 the expectation is `None`, so a block
   *carrying* a base fee pre-AP3 is rejected (Go `BigEqual(nil, x)` = false;
   Rust today accepts — a second fail-open this closes).
4. Expected block gas cost: mirror `customheader.BlockGasCost(config, parent,
   header.Time)` (pre-AP4 `None`; AP4+ via the existing
   `blockgas::block_gas_cost` with the same parent-cost/step/elapsed/granite
   wiring `builder.rs` uses); exact `Option` equality with
   `header.block_gas_cost` (same both-directions nil semantics — a third
   fail-open closed).
5. `ExtDataGasUsed` gating: pre-AP4 must be `None`
   (`invalid extDataGasUsed before fork`); AP4+ must be `Some` and fit `u64`
   (`errExtDataGasUsedNil` / `errExtDataGasUsedTooLarge`). Rust has no check
   today.

### `base_fee` time-advance fix

`feerules::base_fee` signature changes from `(cs, parent: &reth Header, ctx)`
to `(cs, parent: &AvaHeader, ctx)` — the refactor the module's deferral note
(`mod.rs:122-138`) prescribes (the reth `Header` carries no
`time_milliseconds`, blocking a faithful Granite ms-advance).

- **Window arm:** unchanged semantics; `time_elapsed` reads `parent.time` off
  the `AvaHeader`.
- **ACP-176 arm:** becomes
  `fee_state_before_block(cs, parent, ctx.timestamp_ms)?.gas_price().0` —
  exactly Go `BaseFee = feeStateBeforeBlock(parent, timeMS).GasPrice()`. The
  arm stops reading `ctx.parent_fee_state` (it re-derives from
  `parent.extra`, as Go does); `ctx.parent_fee_state` remains used by the
  window arm and by `gas_limit` (whose Fortuna arm is already byte-exact —
  `MaxCapacity` depends only on `TargetExcess`, which the time-advance never
  touches).
- `AvaNextBlockCtx.timestamp_ms` already exists; no ctx change.
- **Call sites to update** (each has, or can resolve, the parent
  `AvaHeader`): the builder/next-block-env path (`evmconfig.rs` /
  `builder.rs`), `rpc/eth.rs:603` (`eth_gasPrice` suggestion), and
  `tests/fee_schedule.rs`. Implementation must sweep all callers
  (`grep feerules::base_fee`).

Effect: builder, RPC, and verifier share the one fixed function — builder and
verify cannot drift, and the builder now stamps the time-advanced (Go-exact)
base fee under load too.

### Verify-path threading

- New `Shared::parent_header(parent_id: Id) -> Result<AvaHeader>` in
  `crates/ava-evm/src/vm.rs`: resolves the parent from the processing tree
  (`verified` map) or the canonical store — covers a processing parent, the
  last-accepted parent, and genesis (seeded into the store at boot by
  `from_genesis`).
- `EvmBlock::verify_with_predicates` gains `parent: &AvaHeader` and calls
  `feerules::verify_header_gas_fields(spec, parent, self.header())`
  immediately after `syntactic_verify`, before sender recovery/execution —
  Go's ordering (header verification precedes the state transition).
  `EvmBlock::verify` (the no-predicates wrapper) threads it through.
- `syntactic_verify` is untouched: Go keeps both layers (structural checks in
  `wrappedBlock.syntacticVerify`, equality checks in `verifyHeaderGasFields`),
  and so do we. The new checks are purely additive.

### Error handling

New `ava-evm::Error` variants with Go sentinel parity (the proposer-arc
rejection-class convention): `GasLimitMismatch { have, want }`,
`IncorrectFeeState { expected, found }` (Fortuna full-struct arm),
`FeeWindowMismatch` (AP3 prefix arm), `BaseFeeMismatch { expected, found }`,
`BlockGasCostMismatch { have, want }`, `ExtDataGasUsedBeforeFork`,
`NilExtDataGasUsed`, `ExtDataGasUsedTooLarge`. Fee-state computation failures
propagate through the existing `Error::InvalidFeeState` (Go wraps identically:
`"failed to calculate base fee: %w"`, `"calculating initial fee state: %w"`).
Checks run in Go's order so a multi-fault block hits the same **first** error
class as Go.

## Testing

**Unit (per function, Go-cited):**
- `verify_gas_limit`: all four fork arms, pass + fail each.
- `verify_extra_prefix`: honest Fortuna prefix accepted; tampered fee-state
  bytes rejected; claimed-target-excess clamp — a one-step-reachable claim
  accepted, an unreachable claim rejected; AP3 window arm both ways.
- `base_fee` time-advance: nonzero parent `gas.excess` + elapsed time →
  expected price drops vs the raw-parent read; Granite ms vs Fortuna s
  granularity; `excess ≈ 0` unchanged (guards byte-parity with every existing
  golden — the quiet-net corpus must not shift).
- Orchestrator: check-order (multi-fault block → Go's first error);
  ExtDataGasUsed gating; pre-AP3-base-fee and pre-AP4-block-gas-cost
  `Option`-equality rejections.

**Recorded Go-oracle verdict leg** (proposer-arc precedent: env-gated emitter
copied into `~/avalanchego`, corpus committed; pre-gate
`./scripts/check_oracle_binary.sh`):
- Extend the mutation corpus with per-field mutations of an otherwise-valid
  block — wrong base fee, wrong block gas cost, tampered extra-prefix bytes,
  wrong gas limit, ext-data-gas-used present pre-AP4 — Go REJECTS and Rust now
  REJECTS with matched rejection classes; the honest block still ACCEPTs on
  both sides.
- **False-reject guard (new corpus shape):** an honest Go-built block recorded
  under **nonzero excess / sustained load** must PASS Rust verify — the
  time-advance's regression proof (existing recordings are quiet-net,
  `excess ≈ 0`, and cannot catch it).

**Live operator gate:** `check_oracle_binary.sh` → prewarm a freshly-relinked
release binary (`--version` once; macOS first-exec stall) → re-run
`mixed_network` (follower, no regression) + `mixed_network_rust_proposes`
(proposer arm — proves the builder still stamps fields the new verify
accepts, i.e. both really share the fixed `base_fee`).

**Closeout:** per-task `-p ava-evm` nextest + clippy `-D warnings` + fmt; at
branch closeout the full-workspace `lint-all` + `test-unit` (the SDD lesson:
scoped per-task checks missed rustfmt drift and a `--profile ci` parallel race
last round). PORTING.md rows for `verifyHeaderGasFields` / `VerifyGasLimit` /
`VerifyExtraPrefix` flip to ✅; plan/M9-interop-hardening.md and
`specs/10-cchain-evm-reth.md` get AS-BUILT callouts (including the Helicon
delta note).

## Risks / notes

- **False-reject is the main hazard**: an incorrect recompute rejects honest
  Go blocks — worse operationally than the fail-open it replaces. Mitigated by
  the nonzero-excess recorded corpus, the exactness of the already-ported
  `fee_state_before_block`/`fee_state_after_block` primitives, and the live
  re-run.
- The `base_fee` signature sweep touches the RPC gas-price path; its parent
  resolution must produce an `AvaHeader` (canonical store), not a reth
  `Header`.
- `blockgas::block_gas_cost` is a low-level helper (caller supplies
  parent-cost/step/elapsed/granite); the orchestrator must wire it exactly as
  `builder.rs` does — extract a tiny shared wrapper mirroring
  `customheader.BlockGasCost(config, parent, time)` if duplication threatens.
- Once Helicon lands in `AvaPhase`, `verify_extra_prefix`'s Fortuna arm and
  the existing `VerifyExtra` port must both grow the Helicon arm (single
  callout covers both).

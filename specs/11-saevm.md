# 11 — SAE VM (Streaming Asynchronous Execution, ACP-194)

> **Status:** Frontier subsystem. Mirrors the Go note in `vms/saevm/README.md`
> ("`strevm` … under active development … no guarantees about the stability of
> its … APIs"). Per `00` §7.7 this crate family is held to the **stricter lint
> bar**: `clippy::pedantic`, `#![deny(clippy::arithmetic_side_effects)]` on the
> gas-time crates, `overflow-checks = true` in *all* profiles (release too),
> analogous to the Go `lint-saevm` / gosec G115 pass. This spec conforms to
> `00-overview-and-conventions.md` and cross-references `04` (storage/Firewood),
> `06` (consensus / Snowman `block.ChainVM`), `07` (VM framework), and `10`
> (C-Chain on reth). Where it deviates it says so and justifies it.

## Go source covered

| Go path | Rust crate |
|---|---|
| `vms/saevm/sae/` (+ `sae/rpc/`) | `ava-saevm-core` |
| `vms/saevm/blocks/` (+ `blockstest`) | `ava-saevm-blocks` |
| `vms/saevm/saexec/` | `ava-saevm-exec` |
| `vms/saevm/saedb/` | `ava-saevm-db` |
| `vms/saevm/gastime/` | `ava-saevm-gastime` |
| `vms/saevm/gasprice/` | `ava-saevm-gasprice` |
| `vms/saevm/proxytime/` | `ava-saevm-proxytime` |
| `vms/saevm/hook/` | `ava-saevm-hook` |
| `vms/saevm/adaptor/` | `ava-saevm-adaptor` |
| `vms/saevm/cchain/` (+ `state`, `tx`, `txpool`) | `ava-saevm-cchain` |
| `vms/saevm/txgossip/` | `ava-saevm-txgossip` |
| `vms/saevm/worstcase/` | `ava-saevm-worstcase` |
| `vms/saevm/{intmath,cmputils,types,params}` | `ava-saevm-{intmath,cmputils,types,params}` |
| `vms/saevm/saetest/` | `ava-saevm-testutil` (dev-dependency) |
| `vms/saevm/docs/invariants.md` | §10 of this spec (test-enforced) |

> **`libevm` mapping.** The Go code is built on `github.com/ava-labs/libevm`
> (`core.ApplyTransaction`, `core/rawdb`, `core/state`, `triedb`, `core/txpool`).
> Per `00` §4.5 and `10`, our EVM execution layer is **reth/revm + Firewood**, so
> we do **not** port `libevm`; we re-derive the same *behaviour* on reth. The SAE
> machinery (gas-time, settlement, the streaming pipeline, recovery) is
> EVM-engine-agnostic and is what this spec specifies precisely. See §8 for the
> reth/`ava-evm` reuse decision and the exact mapping of `libevm` calls.

---

## 1. The SAE model

Classic Avalanche VMs are **synchronous**: a block is *verified* (executed) before
it can be voted on, so consensus latency includes execution latency and the chain
throughput is bounded by single-threaded execution that must finish inside the
consensus round. ACP-194 **decouples ordering from execution**:

1. **Consensus orders blocks first.** A block carries transactions plus a small
   amount of *lagged* execution metadata (see below). Verification is cheap — it
   does **not** execute the transactions. Snowman votes purely on ordering.
2. **Execution streams asynchronously behind the accepted frontier.** On
   `Accept`, the block is pushed onto a FIFO queue. A single executor task drains
   the queue, executes each block deterministically against the post-state of its
   parent, commits to the state DB, and advances an **execution frontier** that
   *lags* the **consensus (accepted) frontier**.
3. **Results are referenced with a delay (settlement).** A new block does not
   embed its *own* post-execution state root (it isn't known yet). It embeds the
   post-execution state root of the **last settled block** — an ancestor that
   finished executing at least `Tau` (gas-)seconds ago. It also carries the
   execution artefacts (receipts root, etc.) of the ancestors it newly settles.

### 1.1 Three frontiers (not two)

The Go code (`blocks/access.go::Frontier`) tracks **three** monotonic pointers,
all derivable from disk after restart (invariants §10):

```
height ─────────────────────────────────────────────────▶
   … b_{s}        …        b_{e}        …        b_{a}
     ▲                       ▲                     ▲
 LastSettled            LastExecuted          LastAccepted
 (S frontier)            (E frontier)          (A frontier)

  Guarantees (temporal "happens-before", invariants doc):
    b ∈ S  ⟹  b ∈ E  ⟹  b ∈ A          (settle after exec after accept)
    S frontier ≤ E frontier ≤ A frontier  (heights, always)
```

- **Accepted (A):** consensus has committed the ordering. `rawdb` *canonical*.
- **Executed (E):** the executor has produced and committed the post-state. This
  is the EVM "head"/"latest". Lags A by the queue depth.
- **Settled (S):** an executed block whose results have been *referenced by a
  later accepted block* — the point at which results are demonstrably agreed and
  treated as "safe"/"finalized" (SAE has no reorgs, so settlement is the disk-
  corruption-safety analogue, not a finality delay). `rawdb` *finalized*.

The RPC label mapping (`blocks/access.go::ResolveRPCNumber`, invariants §"Height
mapping"):

| RPC label | SAE frontier |
|---|---|
| `pending` | LastAccepted (A) |
| `latest` | LastExecuted (E) |
| `safe`, `finalized` | LastSettled (S) |

### 1.2 Settlement rule (the heart of ACP-194)

A block `b` *settles* the contiguous half-open range of ancestors
`(parent.LastSettled, b.LastSettled]` (`blocks/settlement.go::Settles`,
`Range`). `b.LastSettled` is chosen at build time as **the last ancestor that
finished executing no later than `BlockTime(b) − Tau`**, measured on the
**gas-time clock** (§2), via `blocks::last_to_settle_at`
(`LastToSettleAt`). The "can we settle yet?" predicate returns `(block, known)`:
settlement is only permitted when the executor has progressed far enough that the
*child* of the candidate is provably **not** finished by `settleAt` — otherwise
`known = false` and the builder reports `ErrExecutionLagging` and tries later.

> **Why gas-time, not wall-time.** "Finished executing `Tau` ago" is measured in
> the *gas clock*: a block's execution-completion instant is the gas-time after
> its last tx ticks the clock (§2). This makes the settlement delay a function of
> *work done*, deterministic and replay-stable, independent of how fast/slow real
> hardware executed. This is the protocol-defining novelty.

### 1.3 What a block carries about execution

A SAE block is an Ethereum block (RLP, byte-identical hashing) where the header's
**`Root` field is repurposed**: it is the **post-execution state root of the
block's last settled ancestor** (`blocks/export.go::SettledStateRoot` returns
`types.Block.Root()`), **not** this block's own post-state (which is unknown at
build time). The newly-settled ancestors' receipts are carried so peers can
reconstruct settled receipt roots. A small `hook.Settled` struct
(`{Height, GasUnix, GasNumerator, Excess}`) records the settled block's
post-execution **gas-time and excess** so a freshly synced/recovered node can
reconstruct the gas clock without re-deriving it (`hook.go::Settled`,
`SettledGasTime`). The block builder populates `GasLimit`, `BaseFee`, `GasUsed`
from the **worst-case** prediction (§7), since true values aren't known until
execution.

### 1.4 Recovery after restart (`sae/recovery.go`)

On startup the VM rebuilds all three frontiers from disk with **no trust in
in-memory state**:
1. `last_committed_block` = highest height whose **post-execution state root was
   committed** to the trie DB (`saedb::last_height_with_execution_root_committed`,
   driven by the commit interval / archival flag).
2. Re-enqueue every *accepted-but-not-executed* canonical block
   (`execute_all_accepted`) and `wait_until_executed` the tip → rebuilds E.
3. Walk back from the executed tip reconstructing the *consensus-critical* set
   (accepted blocks from LastExecuted back through LastSettled), calling
   `mark_settled` on those whose gas-time ≤ `BlockTime − Tau`
   (`consensus_critical_blocks`) → rebuilds S.

Determinism guarantee: re-execution from the last committed root reproduces the
exact same post-state roots, because execution is a pure function of the ordered
blocks + parent state (no wall-clock, no map-iteration nondeterminism — §6.1).

### 1.5 Data-flow diagram

```
        ┌── network: tx gossip ──┐
        ▼                        │
   txgossip::Set (mempool)       │
        │ TransactionsByPriority │
        ▼                        │
  ┌─────────────┐  BuildBlock    │      Snowman engine (06)
  │ blockBuilder │◀──────────────┼──────  Verify / Accept / SetPreference
  └─────┬───────┘                │            │
        │ worstcase::State       │            │ AcceptBlock
        │ (predict GasLimit/Fee) │            ▼
        ▼                        │     ┌──────────────────┐
   blocks::Block (eth block) ────┘     │  mark settled Σ   │ (D→M→I→X order)
        │                              │  enqueue(block)    │
        │                              └─────────┬─────────┘
        │                                        │ mpsc (FIFO, bounded)
        ▼                                        ▼
   consensusCritical map               ┌───────────────────────┐
   (hash → Block, A..S)                │ saexec::Executor task  │
                                       │  execute(b) on reth    │
                                       │  Firewood propose/commit│ (spawn_blocking, 04)
                                       │  mark_executed → E++    │
                                       └───────────┬───────────┘
                                                   ▼
                                       events: ChainHead / receipts / WaitUntilExecuted
```

---

## 2. Gas-as-time (`proxytime` + `gastime` + `gasprice`)

This is the protocol-defining accounting model. Three layers:

### 2.1 `proxytime` — time measured by a proxy unit

`proxytime::Time<D>` (Go `proxytime.Time[D Duration]`) represents an instant whose
*passage* is measured in some unit `D: ~u64` (for SAE, `D = Gas`). It stores
`seconds: u64`, `fraction: D` (with invariant `fraction < hertz`), and `hertz: D`
— the number of proxy units equivalent to **one wall second** (the "rate", `R`).
`Tick(d)` advances by `d` proxy units (carrying fraction → seconds);
`FastForwardTo` jumps forward to a `(unix, frac)` if it is in the future and
returns how far it advanced; `SetRate` rescales the fraction (rounding **up** for
monotonicity). It is `canoto`-serialized.

```rust
// ava-saevm-proxytime — newtype-parameterised, no floats, checked math.
pub trait ProxyUnit: Copy + Ord + Into<u128> { /* ~u64 */ }

#[derive(Clone, Debug)]
pub struct Time<D: ProxyUnit> {
    seconds: u64,
    fraction: D,   // invariant: fraction < hertz
    hertz: D,      // proxy units per wall-second (the rate R)
}

#[derive(Clone, Copy)]
pub struct FractionalSecond<D> { pub numerator: D, pub denominator: D }

impl<D: ProxyUnit> Time<D> {
    pub fn tick(&mut self, d: D);                        // advance by d proxy units
    pub fn rate(&self) -> D { self.hertz }
    pub fn fraction(&self) -> FractionalSecond<D>;
    pub fn fast_forward_to(&mut self, to: u64, to_frac: D) -> (u64, FractionalSecond<D>);
    pub fn set_rate(&mut self, hertz: D);                // rescales fraction, rounds UP
    pub fn compare(&self, other: &Self) -> Ordering;     // rates MAY differ
    pub fn as_time(&self) -> SystemTime;                 // for metrics only
}
```

> **Cross-multiplication compare** (`FractionalSecond::compare`) uses 128-bit
> widening (`u64::widening_mul` / `u128`) exactly like Go's `bits.Mul64` so two
> instants at different rates compare identically to Go.
>
> **AS-BUILT (M7.4):** this lives in `ava-saevm-cmputils::compare_fractions(n1,d1,
> n2,d2) -> Ordering` (cross-multiply `n1*d2` vs `n2*d1`, each via
> `u128::from(n).wrapping_mul(...)` — exact for u64×u64 in u128, panic-free, since
> `u64::widening_mul` is still nightly-only). It is a **runtime** helper (proxytime
> calls it), **not** dev/test-only — see the §3 crate-layout note.

### 2.2 `gastime` — the SAE gas clock (Tau-discipline newtype)

`gastime::Time` (Go `gastime.Time`) wraps a `proxytime::Time<Gas>` and adds
**ACP-176/194 dynamic-fee state**: `target: Gas` (the `T` parameter), `excess:
Gas` (the `x` variable), and a `GasPriceConfig`. The rate is pinned to
`rate = target * TargetToRate` where `TargetToRate = 2`, i.e. consuming
`target * 2` gas == one wall-second of gas-time. Constants:
`MinTarget = 1`, `MaxTarget = u64::MAX / 2`.

Two block-boundary operations (cited from `gastime/acp176.go`,
`gastime/gastime.go`):

- **`before_block(t)`** (`BeforeBlock`/`FastForwardToTime`): fast-forward the gas
  clock to be **no earlier than the block's wall timestamp** `t` (converting the
  sub-second ns to gas units at the current rate, rounding up). This is what makes
  an *idle* chain still advance its clock so price can decay.
- **`tick(used)`** during execution: advances gas-time by `used` and grows excess
  by `used·(R−T)/R` (only the over-target portion accrues excess).
- **`after_block(used, target, cfg)`** (`AfterBlock`): final `tick(used)`, then
  **rescale excess** to the new `(target, scaling)` and **re-pin the rate** to the
  new target. `excess' = excess · (newT·newScale) / (oldT·oldScale)` rounded up,
  capped at `u64::MAX` (256-bit intermediate via `U256` — `scaleExcess`).

```rust
// ava-saevm-gastime
#[derive(Clone)]
pub struct GasTime {
    inner: proxytime::Time<Gas>,
    target: Gas,
    excess: Gas,
    config: GasPriceConfig,
}

impl GasTime {
    /// `target * TargetToRate` gas == 1 wall-second.
    pub const TARGET_TO_RATE: Gas = Gas(2);
    pub const MIN_TARGET: Gas = Gas(1);
    pub const MAX_TARGET: Gas = Gas(u64::MAX / 2);

    pub fn new(at: SystemTime, target: Gas, starting_excess: Gas, c: GasPriceConfig)
        -> Result<Self, Error>;

    pub fn target(&self) -> Gas { self.target }
    pub fn excess(&self) -> Gas { self.excess }
    pub fn rate(&self)  -> Gas { self.inner.rate() }

    /// "base fee" = price of one gas unit (ACP-176 exponential).
    pub fn price(&self) -> GasPrice {
        self.config.min_price.max(calculate_price(self.excess, self.excess_scaling_factor()))
    }

    pub fn before_block(&mut self, t: SystemTime);               // fast-forward to >= t
    pub fn tick(&mut self, used: Gas);                            // advance + accrue excess
    pub fn after_block(&mut self, used: Gas, target: Gas, c: GasPriceConfig) -> Result<(), Error>;
    pub fn compare(&self, other: &Self) -> Ordering;
}
```

> **Upstream delta (avalanchego `3a5cba4a61`, #5485 — folded 2026-06-15).**
> `gastime.New` now takes the **starting price** (`gas.Price`/`base fee`), not the
> starting excess: `New(at, target, startingPrice, cfg)`. Internally it converts
> price→excess via `excessForPrice(startingPrice, K)` and delegates to the new
> private `newFromExcess`. The old `starting_excess` arg (and its
> "difficult-for-a-caller-to-provide" `TODO`) are gone. Two knock-ons: (1)
> `Block.MarkSynchronous` dropped its `excessAfter` parameter — it now derives the
> base fee from the eth block's `BaseFee` (nil→0, overflow→`MaxUint64`) and feeds
> it as the starting price; (2) `sae.Config.ExcessAfterLastSynchronous` was removed
> entirely. The Rust `GasTime::new` (sketch below) and the `Block::mark_synchronous`
> placeholder must mirror the new signature — see `plan/M7` M7.36.

The **excess scaling factor** `K = TargetToExcessScaling · T` (capped to `u64`),
and `price = max(min_price, e^(x/K))` computed by the **integer** exponential
`gas::calculate_price` ported from `vms/components/gas` (`gasprice/...`). Defaults:
`TargetToExcessScaling = 87`, `MinPrice = 1`. `StaticPricing = true` pins `excess
= 0` (constant min price). `enforce_min_excess` binary-searches `excessForPrice`
to keep `excess` consistent with `min_price`.

### 2.3 The Tau discipline — making the lint impossible by construction

The Go repo forbids `time.Add(...TauSeconds)` (the `tausecondslint` CI grep): gas
is measured **in time**, and you must add a `time.Duration` (`params.Tau`), never
a raw second count. We reproduce this **structurally** so the mistake cannot
compile:

```rust
// ava-saevm-params
use std::time::Duration;

/// Tau: minimum (gas-)time between a block finishing execution and being
/// settled by a later block. C-Chain value: 5 s.
pub const TAU: Duration = Duration::from_secs(TAU_SECONDS);
pub const TAU_SECONDS: u64 = 5;

/// Lambda: minimum-gas-per-tx denominator (min charge = ceil(gas_limit/λ)).
pub const LAMBDA: u64 = 2;

/// A wall instant on the SAE timeline. Constructed only from `SystemTime`; the
/// ONLY way to shift it is by a `Duration`. There is deliberately **no**
/// `impl Add<u64>` and **no** way to add a bare second count — the Go
/// `tausecondslint` rule is enforced by the type system here.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockInstant(SystemTime);

impl BlockInstant {
    pub fn from_unix(secs: u64) -> Self { /* ... */ }
    /// Subtract a Duration (e.g. `instant.minus(params::TAU)`), saturating.
    pub fn minus(self, d: Duration) -> Self { /* saturating_sub */ }
    pub fn plus(self, d: Duration) -> Self { /* saturating_add */ }
}

// ❌ Will not compile — no Add<u64>/Sub<u64> exists, so `instant - TAU_SECONDS`
//    is a type error, exactly mirroring the Go forbidden pattern.
```

`last_to_settle` (`block_builder.go::lastToSettle`) thus reads
`block_time.minus(params::TAU)` — a `Duration` op only. The
`MaxQueueWallTime = MaxFullBlocksInClosedQueue · Tau · Lambda` constant
(`params.go`) is likewise a `Duration · u64` multiply, never a second add.

### 2.4 Derived block/queue limits (ACP-194 §)

From `worstcase/state.go` + `params`:
- **Max block gas** `Ω_B = R · Tau · Lambda` (`safeMaxBlockSize`, capped so a full
  *closed* queue fits in `u64`). With `Tau=5, Lambda=2`: `Ω_B = 10·R` gas.
- **Open-queue threshold** `MaxFullBlocksInOpenQueue = 2` (`Ω_Q = 2·Ω_B`).
- **Closed-queue cap** `MaxFullBlocksInClosedQueue = 3`.
- **Min gas charged per tx** `max(gas_used, ceil(gas_limit/Lambda))`
  (`hook::MinimumGasConsumption = ceil(txLimit / Lambda)`) — prevents
  high-limit/low-usage queue-stuffing attacks.

> **Upstream delta (avalanchego `0b0b57143c`, #5424 — folded 2026-06-15).** The
> minimum-gas floor is now **enforced inside the EVM**, not just defined as a
> param. `RulesExtra.MinimumGasConsumption` (coreth `params/hooks_libevm.go`) was
> a no-op (`ethparams.NOOPHooks`); it now returns `hook.MinimumGasConsumption(limit)
> = ceil(limit/Lambda)` **when `IsHelicon`** (the SAE fork — currently unscheduled
> on all networks), falling back to the no-op pre-Helicon. The libevm gas-charge
> path consults this hook so a tx with a high `gas_limit` but low actual usage is
> still charged the floor. In the Rust port the `hook::minimum_gas_consumption`
> function already exists (M7.9); the gap is **wiring it into the reth/ava-evm
> `RulesHooks` gas-charge path gated on the Helicon fork** — see `plan/M7` M7.35
> (touches the M6 `ava-evm-reth` EVM layer).

---

## 3. Crate layout (`ava-saevm` sub-workspace)

`ava-saevm` (per `00` §3) is an internal sub-workspace; dependency direction is
strictly downward:

```
ava-saevm-params      (Tau, Lambda, BlockInstant, queue limits — leaf)
ava-saevm-intmath     (mul_div, mul_div_ceil, ceil_div, bounded_{add,sub,mul} — leaf, no_std-able)
ava-saevm-cmputils    (compare_fractions — 128-bit cross-mul; RUNTIME, used by proxytime — NOT dev-only)
ava-saevm-proxytime   → intmath
ava-saevm-gastime     → proxytime, intmath, ava-types(Gas/GasPrice), gas (price fn)
ava-saevm-gasprice    → gastime, blocks  (eth_gasPrice/feeHistory estimator)
ava-saevm-types       → ava-database (HeightIndex), ava-evm (reth block/header types)
ava-saevm-hook        → gastime, proxytime, intmath, params, types, ava-evm
ava-saevm-worstcase   → blocks, gastime, hook, db, params, ava-evm
ava-saevm-blocks      → gastime, proxytime, params, types, hook, adaptor, ava-evm
ava-saevm-adaptor     → ava-snow, ava-engine (block.ChainVM bridge; no SAE deps)
ava-saevm-db          → hook, ava-evm/firewood (04), ava-types
ava-saevm-exec        → blocks, gastime, hook, db, types, ava-evm/reth
ava-saevm-txgossip    → ava-network(gossip), blocks, exec, ava-evm
ava-saevm-core        → all of the above (the sae.VM) + sae-rpc
ava-saevm-cchain      → core, ava-evm, ava-secp256k1fx-equivalent (avax import/export)
ava-saevm-testutil    (dev): saetest, blockstest, txtest, escrow
```

> **Upstream delta (`ab442aa244`).** Go's `saetest` gained **`network.go`** — an
> in-memory multi-node harness: `saetest.Sender` (an `common.AppSender` that
> routes AppRequest/AppResponse/AppError/AppGossip between registered peers on
> goroutines, with validator-aware gossip sampling), `Connect`/`ConnectTo`
> (full-mesh or star wiring of `Peer`s), and `SetValidators`. The
> multi-node tests formerly inlined in `sae/networked_test.go` now build on it,
> and `cchain`'s gossip tests reuse it. Port into `ava-saevm-testutil` when
> wiring live gossip (plan/M7.33) — it is also the natural substrate for the
> M7.29/M7.30 differential harnesses.

Public surface highlights:

- **`ava-saevm-core::Vm`** — the SAE VM (everything except `Initialize`,
  provided by a *harness*; §5). Implements the generic `adaptor::ChainVm`.
- **`ava-saevm-blocks::Block`** — eth block + SAE lifecycle (§4).
- **`ava-saevm-exec::Executor`** — the async streaming engine (§6).
- **`ava-saevm-db::Tracker`** — state-DB / Firewood-revision tracker (§7).
- **`ava-saevm-hook::{Points, BlockBuilder, Op, Settled}`** — lifecycle hooks
  (§8) — the seam the C-Chain and future subnets plug into.
- **`ava-saevm-gastime::GasTime`**, **`ava-saevm-params::{TAU, LAMBDA}`** (§2).
- **`ava-saevm-cchain::Vm`** — the minimal EVM C-Chain (§8).

---

## 4. Blocks (`ava-saevm-blocks`) — format, codec, lifecycle

### 4.1 Byte-exact format

A SAE block **is** an Ethereum block. Wire encoding is **RLP of the eth block**
(`blocks/snow.go::Bytes` = `rlp.EncodeToBytes`), hashing is Keccak of the header —
**byte-for-byte identical to a geth/`libevm` block** so peers and tooling
interoperate. Field semantics that differ under SAE (all *interpretation*, not
layout):

| Header field | Standard eth | SAE meaning |
|---|---|---|
| `Root` | this block's post-state | **settled ancestor's** post-exec state root (`SettledStateRoot`) |
| `ReceiptHash`/`Bloom`/`GasUsed` | this block's exec results | results of the **newly settled ancestors** carried for them |
| `BaseFee`,`GasLimit` | this block's | **worst-case prediction** by the builder; real values emerge at execution |
| `Time` | block time | inclusion time of txs; **execution** time is the separate gas-time |

A side `hook.Settled {Height, GasUnix, GasNumerator, Excess}` is embedded via the
hook's block-extras mechanism (libevm "extras" → reth header extension in `10`),
recording the settled block's gas-clock so recovery/state-sync can rebuild it.
**`canoto`** is used for the *persisted* execution-results blob
(`blocks/execution.canoto.go`, §7), **not** for the block on the wire.

> **Upstream delta (avalanchego `dbf0f71dc1`, #5573 — folded 2026-06-24).** The
> "block-extras mechanism" above is now concrete for the SAE C-Chain: the
> `hook.Settled {Height, GasUnix, GasNumerator, Excess}` quad is carried as **four
> new optional coreth header fields** — `SettledHeight`, `SettledGasUnix`,
> `SettledGasNumerator`, `SettledExcess` (all `*uint64`, RLP-`optional` tail +
> JSON, see the header-tail callout in `10`). `cchain.builder.BuildBlock` writes
> them from the chosen `settled` (replacing the prior `_ = settled` TODO), and
> `cchain.hooks.SettledBy(h)` reconstructs the quad by reading them back (returning
> the zero `hook.Settled{}` if **any** of the four is absent). Pure coreth blocks
> must *not* carry the marker — coreth's `customheader.VerifySettled` rejects a
> coreth header with any `Settled*` field set, since `semanticVerify` only runs on
> coreth's own `block.Verify` and the fields belong to SAE. In the Rust port these
> are the reth header-extension fields backing the side `hook.Settled` (the
> `[seconds, fraction, hertz]` proxy-clock quad of the M7.8 AS-BUILT note); landing
> the encode/decode is `plan/M7` M7.45. **Non-gating** (Helicon-dormant SAE
> C-Chain), but a wire/format parity constraint worth matching now.

> **AS-BUILT (M7.8).** There is no canoto codec in the Rust workspace, so
> `ava-saevm-types::ExecutionResults` persists via a deterministic **fixed-layout
> 96-byte big-endian encoding** (`gas_time` 24 ++ `base_fee` 8 ++ `receipt_root`
> 32 ++ `post_state_root` 32) rather than a canoto/`ava-codec` derive. This is
> sufficient for the persist-and-reload round-trip (recovery/state-sync); exact
> Go-canoto **byte** parity, if needed, is a differential-test concern (M7.29).
> `gas_time` is stored as the proxy-clock scalars (`proxytime::Time<u64>`'s
> `[seconds, fraction, hertz]`), echoing the side `hook.Settled` quad.

Parsing (`sae/blocks.go::ParseBlock`) RLP-decodes, checks height fits `u64`,
rejects blocks > `now + 10s` (`maxFutureBlockSeconds`), and verifies tx/uncle/
withdrawals hashes match the header. Ancestry is **not** populated at parse — only
on successful `VerifyBlock`.

### 4.2 Lifecycle state machine

`blocks::Block` (Go `blocks.Block`) tracks stage with channels + atomics:

```rust
// ava-saevm-blocks — stricter-lint crate.
pub struct Block {
    eth: reth_primitives::SealedBlock,         // the wire block (10)
    // Invariant: Some(ancestry) iff NOT yet settled (severed for GC after settle).
    ancestry: ArcSwapOption<Ancestry>,         // {parent, last_settled}
    synchronous: bool,                          // genesis / last pre-SAE block
    bounds: OnceLock<WorstCaseBounds>,          // set pre-execution
    execution: ArcSwapOption<ExecutionResults>, // Some iff executed
    interim_execution_time: ArcSwapOption<proxytime::Time<Gas>>, // monotonic, during exec
    executed: Notify,                            // fired after `execution` set
    settled:  Notify,                            // fired after `ancestry` cleared
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum LifeCycleStage { NotExecuted /*=Accepted*/, Executed, Settled }
```

Key methods (cite `blocks/execution.go`, `settlement.go`):

- `mark_executed(...)`: writes receipts + canoto execution blob to disk **first**,
  sets `execution`, advances the E pointer, fires `executed` — strict **D→M→I→X**
  ordering (§10). Once only.
- `mark_settled(last_settled_ptr)`: CAS `ancestry` to `None` (severs parent links
  → GC), updates the S pointer, fires `settled`. Once only.
- `mark_synchronous(...)`: combined exec+settle for the genesis / last pre-SAE
  block (self-settling — impossible under normal SAE rules).
- `settles()` → `Range(parent.last_settled, self.last_settled)` — disjoint per
  block, contiguous overall.
- `executed_by_gas_time()`, `post_execution_state_root()`, `receipts()`, etc.
  **block** on the `executed` notify (with a `MaxQueueWallTime` warn) then read.
- `restore_execution_artefacts(...)` / `restore_settled_block(...)`: rebuild from
  disk (recovery / `GetBlock` of a settled block).

> **Upstream delta (avalanchego `84533ec5b1`, #5547 — folded 2026-06-18).**
> `VM.GetBlock` (`sae/blocks.go`) previously swallowed any unexpected error from
> `blocks.FromHash(...)`: it returned `(b, nil)` after only special-casing
> `ErrNotFound` → `database.ErrNotFound`, so a failed/corrupt height-index read
> (e.g. an underlying `xdb.Get` I/O error) was silently dropped and the caller
> got a possibly-incomplete block with no error. The fix returns `(b, err)` —
> the not-found sentinel is still translated, but every other error now
> propagates. Companion change: `RestoreSettledBlock` (`blocks/block.go`) switches
> its two wrap sites from `%v` to `%w` so the underlying error survives in the
> chain (`errors.Is`/`Unwrap` now see the read failure). Rust analog:
> `ava_saevm` VM `get_block` / `restore_settled_block` — `?`-propagation plus
> `thiserror` `#[from]`/`#[source]` make the `%w` chain idiomatic by default, so
> the substantive parity check is that `get_block` maps **only** the
> not-found case to the DB-not-found sentinel and otherwise returns the
> underlying error rather than `Ok(block)`. Tracked as `plan/M7` task **M7.42**.

The Go `runtime.AddCleanup` GC-leak counter (`InMemoryBlockCount`) maps to a
`Drop` impl decrementing an `AtomicI64` (test observability — §10).

> **AS-BUILT (M7.11).** `interim_execution_time` is `proxytime::Time<u64>` (the
> canonical proxy gas unit — `Time<Gas>` is not used; `Gas`/`Price` appear only
> at boundaries), and `synchronous` is a `OnceLock<()>`. The block is the **stock
> reth/alloy `SealedBlock<RethBlock>`** (hash = `keccak256(RLP(header))`),
> reached via the `ava-evm-reth` facade — **not** the coreth `AvaHeader`. The
> once-only transitions use lock-free atomics: `mark_settled` is an atomic
> `ancestry.swap(None)` (a `None` prior value ⇒ `ReSettled`); `mark_executed` is
> a load-check-then-`store` on the single execution thread (an API-misuse guard,
> not a data-race guard — matching Go). `last_to_settle_at` treats a block's
> **build time as a lower bound on its execution-completion instant** (skip
> `build_time > settle_at` without consulting the exec result), then decides on
> the gas clock; when execution lags it returns the nearest synchronous floor
> with `known = false` (`ErrExecutionLagging`).
> **`adaptor::BlockProperties` is deferred to the VM layer (M7.18)**: the orphan
> rule forbids `impl BlockProperties for Arc<Block>` (`Arc` is not fundamental),
> and `bytes() -> &[u8]` needs the VM's cached wire bytes — both live naturally
> on the VM's local block-handle. The `hook.Settled` header-extras embedding
> (§4.1) is likewise deferred to M7.21 (reth header-extension). `WorstCaseBounds`
> is a minimal placeholder until M7.13. The GC-counter test relies on nextest's
> process-per-test isolation (the `static AtomicI64` is shared under plain
> `cargo test`).

---

## 5. Adaptor — exposing SAE as a Snowman `ChainVM` (cross-ref 06/07)

> **AS-BUILT (M7.10) — crate-name mapping.** The M3 VM framework is entirely in
> the **`ava-vm`** crate (not `ava-engine`). The sketch below names
> `ava_vm::CommonVm`, `ava_engine::block::ChainVm`, and `block::Context`; the
> implementation maps these to **`ava_vm::Vm`** (the base VM trait),
> **`ava_vm::ChainVm`** (`= ava_vm::block::ChainVm`), and **`ava_vm::BlockContext`**
> respectively, and the wrapper `Block` implements **`ava_vm::Block`** (re-export
> of `ava_snow::Block`). `ava_vm::Error` is mapped to
> `ava_snow::Error::ParametersInvalid(..)` at the block verify/accept/reject
> boundary (no built-in conversion exists).

`ava-saevm-adaptor` (Go `adaptor/`) is a **generic bridge**: it converts a
`ChainVm<BP>` (a VM that returns *plain block-property* objects and takes the
state-changing methods on the VM itself) into the `block::ChainVm` +
`BuildBlockWithContext` + `SetPreferenceWithContext` traits Snowman expects
(`07` §"ChainVm/Block traits"). The key design choice (kept from Go): **the
block does not know about the VM** — `Verify/Accept/Reject` live on the VM
(`VerifyBlock(ctx, block_ctx, b)`, `AcceptBlock(ctx, b)`, …) and the adaptor's
wrapper `Block` forwards `snowman::Block` calls back to its owning VM.

```rust
// ava-saevm-adaptor
#[async_trait]
pub trait ChainVm<BP: BlockProperties>: ava_vm::CommonVm {
    async fn get_block(&self, id: Id) -> Result<BP, VmError>;
    async fn parse_block(&self, bytes: &[u8]) -> Result<BP, VmError>;
    async fn build_block(&self, ctx: Option<&block::Context>) -> Result<BP, VmError>;

    async fn verify_block(&self, ctx: Option<&block::Context>, b: &BP) -> Result<(), VmError>;
    async fn accept_block(&self, b: &BP) -> Result<(), VmError>;
    async fn reject_block(&self, b: &BP) -> Result<(), VmError>;

    async fn set_preference(&self, id: Id, ctx: Option<&block::Context>) -> Result<(), VmError>;
    async fn last_accepted(&self) -> Result<Id, VmError>;
    async fn get_block_id_at_height(&self, h: u64) -> Result<Id, VmError>;
}

pub trait BlockProperties: Clone {
    fn id(&self) -> Id;
    fn parent(&self) -> Id;
    fn bytes(&self) -> &[u8];
    fn height(&self) -> u64;
    fn timestamp(&self) -> SystemTime;
}

/// Wraps the generic VM into the concrete Snowman traits (07).
pub fn convert<BP, V: ChainVm<BP>>(vm: Arc<V>) -> impl ava_engine::block::ChainVm { /* Adaptor */ }
```

> **Upstream delta (avalanchego `b1393ecb06`, #5480 — folded 2026-06-17).** The
> Go `adaptor/` package gains a **second generic bridge alongside the block-VM
> one** — a *syncable*-VM wrapper (`adaptor/sync.go`): `ConvertStateSync[SP](vm)`
> turns a `SyncableVM[SP]` (the SAE-friendly shape — `StateSyncEnabled`,
> `Get{Last,OngoingSync}StateSummary`, `GetStateSummary`, `ParseStateSummary`,
> `AcceptSummary`, all returning a *plain* `SP: SummaryProperties` =
> `{ID, Bytes, Height}`) into Snowman's `block.StateSyncableVM`, wrapping each
> `SP` in a `Summary[SP]` whose `Accept` forwards back to the VM's
> `AcceptSummary` — the same "block doesn't know about the VM" inversion this
> adaptor already applies to `ChainVm`/`Block`, now for state-sync summaries.
> (The commit also renames `adaptor.go` → `vm.go`, a no-op.) The Rust analog is a
> `ConvertStateSync` + `SyncableVm`/`SummaryProperties` traits in
> `ava-saevm-adaptor`, paralleling the existing `convert`/`ChainVm`/`BlockProperties`.
> **Dormant:** SAE state sync itself (§10 / `10` §10, C8) is unported — this is
> the bridge those summaries will flow through once a syncable SAE VM exists.
> Tracked as `plan/M7` M7.40 (non-gating; Helicon unscheduled).

`ava-saevm-blocks::Block` implements `BlockProperties` (Go `blocks/snow.go`).
`ava-saevm-core::Vm` implements `ChainVm<Block>` — its `verify_block` *rebuilds*
the block from its parent + the hook builder and compares hashes (cheap, no
execution; `sae/blocks.go::VerifyBlock`); during **bootstrapping** it skips the
rebuild (peers verify by hash) and blocks on `wait_until_executed` so the engine's
accept-in-a-loop cannot outrun the executor and FATAL (`AcceptBlock` +
`verifyWhenBootstrapping`).

> **`block.Context`** (proposervm P-Chain height) threads through unchanged (06).
> SAE blocks return `ShouldVerifyWithContext = true`.

---

## 6. `saexec` — the streaming execution engine (tokio pipeline)

The Go `saexec.Executor` is a goroutine draining a buffered channel. Rust port:

```rust
// ava-saevm-exec
pub struct Executor {
    tracker: Arc<saedb::Tracker>,          // state DB + Firewood revisions (§7)
    queue_tx: mpsc::Sender<Arc<Block>>,    // bounded FIFO, cap = 2 * commit_interval
    last_executed: arc_swap::ArcSwap<Block>,
    head_events: broadcast::Sender<ChainHeadEvent>,
    receipts: DashMap<TxHash, eventual::Value<Receipt>>,
    chain_ctx: ChainContext,               // recent-headers LRU for BLOCKHASH
    config: Arc<reth_chainspec::ChainSpec>,
    hooks: Arc<dyn hook::Points>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl Executor {
    pub fn new(last_executed: Arc<Block>, /* sources, chainspec, db, hooks */) -> Result<Arc<Self>, Error>;
    pub async fn enqueue(&self, b: Arc<Block>) -> Result<(), Error>; // backpressure here
    pub fn last_executed(&self) -> Arc<Block>;
    pub fn subscribe_chain_head(&self) -> broadcast::Receiver<ChainHeadEvent>;
}
```

### 6.1 The execute step (`saexec/execution.go::Execute`)

Per dequeued block, on a **dedicated execution thread** (not the async reactor —
`00` §7.2; this thread owns the Firewood handle so calls it synchronously per
`04` §4.2):

1. Sanity: `last_executed.hash() == b.parent_hash()` (else error → fatal log).
2. Clone the parent's `executed_by_gas_time` → `gas_clock`; `before_block(block_time)`.
3. Open `state_db` at `parent.post_execution_state_root()` (Firewood `view`/
   reth `StateProvider`, §7/§8).
4. `hooks.before_executing_block(rules, state, eth)`.
5. `base_fee = gas_clock.price()`; **check it against the worst-case bound**
   (`CheckBaseFeeBound`); set `header.base_fee`.
6. For each tx: check worst-case sender balance, run `reth`/`revm`
   `ApplyTransaction` (`10`), `per_tx_clock.tick(receipt.gas_used)`,
   `set_interim_execution_time` (lets `LastToSettleAt` settle mid-block), fix up
   receipt `BlockHash`/`EffectiveGasPrice`, publish to the `eventual::Value`
   receipt buffer.
7. `hooks.end_of_block_ops` → apply mint/burn `Op`s, ticking the clock.
8. `hooks.after_executing_block`.
9. `gas_clock.after_block(block_gas_consumed, target, cfg)` (§2.2) → the final
   execution gas-time.
10. **Commit:** `state.commit()` → new root; `tracker.maybe_commit(settled_root,
    exec_root, height)` (§7); `tracker.track(root)`; then `mark_executed(...)` in
    strict **D→M→I→X** order; emit chain-head/receipt events.

Errors are classified: `Fatal` (consensus-critical, e.g. a tx *errored* rather
than reverted — points at the emergency playbook and **stops the executor**, which
is correct: continuing would corrupt the head) vs recoverable. A reverted tx is
**normal** (it still consumes gas). Determinism: the entire function is a pure
function of `(ordered block, parent state, chain config, hooks)` — no wall-clock
in any consensus output (`FinishBy.Wall` is metrics-only), no map iteration over
unsorted collections.

### 6.2 Backpressure, ordering, recovery

- **FIFO + bounded queue:** `mpsc` with capacity `2 * commit_interval`; `enqueue`
  `await`s when full → natural backpressure onto `AcceptBlock`. The *builder* also
  refuses to build when the worst-case queue exceeds `MaxFullBlocksInOpenQueue ·
  Ω_B` (`worstcase::ErrQueueFull`) — so consensus paces itself to execution.
- **Single executor task** guarantees total order == accept order; no parallel
  execution across blocks (state dependencies are serial). *Within* a block,
  txs are serial (EVM semantics).
- **Shutdown:** `CancellationToken` cancels; the task finishes the in-flight block
  then `tracker.close(last_root)` flushes the Firewood/snapshot layer.
- **Recovery** re-drives the queue from disk (§1.4); because execution is pure,
  the replayed roots match and `maybe_commit` lands on the same heights.

---

## 7. `saedb` — storage model (consensus-state vs execution-state, Firewood)

SAE keeps **two logically distinct kinds of state** (invariants doc):

- **Consensus state** (ordering): canonical hashes, head/finalized pointers,
  block bodies — the `rawdb`-style KV (our `ava-evm` rawdb-equivalent over the KV
  `Database`, `04`). Written on `AcceptBlock`.
- **Execution state** (the EVM trie): account/storage state, keyed by **state
  root**, in **Firewood** (`04` §4.2/§4.3). Written on execution-commit, **behind**
  the consensus state.
- Plus a **height-indexed `ExecutionResults`** DB (`types::ExecutionResults`
  wrapping `database::HeightIndex`) holding the per-block canoto blob
  `{gas_time, base_fee, receipt_root, post_state_root}` so executed artefacts
  survive restart independently of the trie commit cadence.

### 7.1 Tracker + Firewood revision contract

```rust
// ava-saevm-db
pub struct Tracker {
    state: Arc<ava_evm::FirewoodStateProvider>,  // Firewood Db (04)
    config: Config,                               // commit_interval, archival
    // reth-side caches/snapshot equivalent
}

impl Tracker {
    /// Increment the retain-count for a root (consensus needs it).
    pub fn track(&self, root: B256);
    /// Decrement; when zero and not on disk, the in-memory revision may drop.
    pub fn untrack(&self, root: B256);
    /// Commit policy:
    ///  - archival      → commit `execution_root` (every block)
    ///  - height%N == 0  → commit `settled_root`
    ///  - else           → nothing (pipelined; root still readable in-memory)
    pub fn maybe_commit(&self, settled_root: B256, execution_root: B256, height: u64) -> Result<(), Error>;
    pub fn state_db(&self, root: B256) -> Result<StateDb, Error>; // open at any retained revision
}
```

This is the **Firewood-pipelining contract with `04`**: `propose` yields the EVM
state root *before* `commit`, so the executor knows `post_execution_state_root`
(which a *future* block will embed once settled) without durably committing every
block. `commit` is deferred to the **commit interval** (default 4096) — or every
block when archival — and runs on the execution thread (`spawn_blocking`-free per
`04` §4.2). Retained revisions are bounded by the consensus-critical window
(LastExecuted back to LastSettled) via `track`/`untrack` ref-counting, mapping Go's
`triedb.Reference`/`Dereference` onto Firewood's `RevisionManager`. SAE has **no
reorgs**, so on close we flatten the snapshot to the last root unconditionally.

`last_height_with_execution_root_committed` (used by recovery, §1.4) reads the
head header and rounds down to the last committed interval (or returns the
settled-by height for that block), or the head if archival.

> **Deviation from `04` default (RocksDB) noted:** the *consensus* KV uses the
> standard `ava-database` backend; the *execution* trie is Firewood directly,
> exactly as `04` §4.2/§4.3 prescribe for the EVM. No new storage engine.

> **AS-BUILT (M7.12).** `ava_evm::FirewoodStateProvider` exposes **no public
> `track`/`untrack`/`RevisionManager`** API — revision lifecycle is encapsulated.
> So `Tracker::track`/`untrack` is a **Tracker-owned ref-count layer**
> (`Mutex<BTreeMap<B256,u64>>`) that *records* the consensus-critical window
> (Go's `triedb.Reference`/`Dereference`); actual in-memory eviction is governed
> by the provider's own retained window (a real `Dereference`→drop hook is a
> future provider extension). `state_db(root)` = `provider.history_by_state_root`
> (so `StateDb` is a type alias for `ava_evm::FirewoodStateView`); `commit`s go
> through `provider.commit(root)`. **CC-ORDER (`27` §2.4)** is split: this module
> owns the *durability half* (the commit), the executor owns the *call-order
> half* (`maybe_commit`→`track`→`mark_executed`, all before any consensus pointer
> advance). `close` is idempotent (already-tip or `MissingProposal` ⇒ no-op). The
> height-indexed `ExecutionResults` DB (the third storage kind above) is the
> M7.8 `ava-saevm-types::ExecutionResultsDb`, **re-exported** from `ava-saevm-db`
> rather than re-implemented — so `saedb` presents the full §7 storage surface.
> This task does **not** depend on `ava-saevm-blocks` (keyed on roots/heights,
> not the `Block` type) and was built in parallel with M7.11.

---

## 8. `cchain` — the minimal EVM C-Chain on `sae.VM` (reuse decision vs 10)

`ava-saevm-cchain` (Go `cchain/`, the "sae: Implement minimal C-Chain VM" commit)
is a thin VM that **composes** `sae::Vm` with the C-Chain-specific pieces:

- **`hooks` (`cchain/hooks.go`)** — the `hook::PointsG<Tx>` implementation:
  header building, gas config (`GasConfigAfter`), end-of-block mint/burn ops for
  **atomic Import/Export** of AVAX, block rebuild for verification.
- **`state` (`cchain/state`)** — cross-chain shared-memory / atomic-tx state
  (codec'd; an avalanchego `Database`).
- **`tx` (`cchain/tx`)** — Import/Export tx types, fx, codec (avalanchego linear
  codec, `03`).
- **`txpool` (`cchain/txpool`)** — pool for the *atomic* (cross-chain) txs,
  separate from the EVM `txgossip` mempool; `WaitForEvent` selects across both.
- **`api` (`cchain/api.go`)** — the `avax` JSON-RPC service (import/export),
  mounted at `/avax` alongside the SAE EVM RPC.

`Initialize` (`cchain/vm.go`) sets up genesis, builds the hooks, constructs
`sae::NewVM`, then the atomic txpool. It is the *harness* that supplies the
`Initialize` method `sae::Vm` deliberately omits (§5).

> **Upstream delta (avalanchego `ff8f0e5020`, #5536 — folded 2026-06-19).**
> `cchain.Initialize` replaces the placeholder `json.Unmarshal` into a bare
> `core.Genesis` + `core.SetupGenesisBlock` with a dedicated **coreth-compatible
> genesis path** (new `cchain/genesis.go`). `parseGenesis(ctx, bytes)` unmarshals
> only the **chain-specific** config — the JSON supplies `ChainID` + alloc + the
> SAE-allowed `BaseFee`, while almost every `core.Genesis` testing-only field is
> now *rejected* (`errNonZeroGenesisNumber`/`GasUsed`/`ParentHash`,
> `errNonNilGenesisExcessBlobGas`/`BlobGasUsed`) — then **synthesizes the full
> `ChainConfig`** from `ctx.NetworkUpgrades`: all eth forks at block 0 except
> **`BerlinBlock`/`LondonBlock` pinned to the historical AP2/AP3 activation
> heights per chainID** (mainnet `1_640_340`/`3_308_552`, fuji `184_985`/`805_078`,
> else 0), `ShanghaiTime = DurangoTime`, `CancunTime = EtnaTime`, the coreth
> `extras.NetworkUpgrades` (AP1…Helicon) timestamps, and the Durango-gated Warp
> precompile upgrade. `genesis.setup(db, trieConfig)` then writes the genesis
> block + canonical/head/finalized pointers + (nil) receipts, checks
> `GenesisMismatchError` and `CheckCompatible` against any stored config, and
> writes genesis **state** only when the trie is not already initialized —
> returning the genesis `*types.Block` that seeds `sae.NewVM` (replacing the old
> `genesis.ToBlock()`). **Rust seam:** this is the canonical C-Chain genesis the
> M6.8 `EvmVm::from_genesis` wiring (parse → materialize → commit → seed) must
> reproduce for the SAE C-Chain; the per-chainID Berlin/London pins and the
> `ChainConfig`-from-`NetworkUpgrades` synthesis are the parity-critical bits.
> Tracked as `plan/M7` M7.43.

> **Upstream delta (avalanchego `484daf4593`, #5524 — folded 2026-06-19).**
> The SAE C-Chain now **preserves millisecond block timestamps** end-to-end.
> `builder.BuildHeader` stamps `now := b.now().UnixMilli()`, sets the seconds
> field `Time = now/1000` and the Granite-gated **`TimeMilliseconds = &now`**
> (previously a `new(uint64)` placeholder), and the `block_time` hook
> (`hooks.BlockTime(h)`) reconstructs the instant as
> `time.Unix(h.Time, (HeaderTimeMilliseconds(h) % 1000)·1ms)` — **anchoring the
> seconds to `h.Time`** so the invariant `BlockTime(h).Unix() == h.Time` holds
> even when a malicious peer's `TimeMilliseconds` disagrees with `Time`. The
> clock is now **injected**: `cchain.VM` carries a `now func() time.Time` threaded
> into both `newHooks(..., now)` and `sae.Config.Now`, replacing the direct
> `time.Now` in the builder (consistent with `00` §6.1 / `24` clock-injection).
> **Rust seam:** the `ava-saevm-cchain` builder / `block_time` hook must fill the
> `TimeMilliseconds` header field from the injected clock's millis and read the
> sub-second component back while anchoring `.timestamp() == header.time`; the
> clock must come from the determinism-gated injected source (§1 `BlockTime`,
> `gastime` §2.2), not wall time. Tracked as `plan/M7` M7.44.

> **Upstream delta (avalanchego `fb174e8` → `cc3b103b9`, folded 2026-06-10).**
> Three post-snapshot Go commits extend `cchain`/`sae`:
>
> 1. **Cross-chain (atomic) tx gossip** (`ab442aa244`, #5408). `cchain.VM`
>    now wires the *atomic* txpool into the generic p2p gossip framework in
>    `Initialize`: a `gossip.BloomSet` over the txpool (`gossipTx` newtype
>    implements `Gossipable` via the linear-codec tx bytes; `GossipID` = txID),
>    `gossip.NewSystem(...)` registered on **`p2p.AtomicTxGossipHandlerID`**,
>    and push + pull `gossip.Every` loops with **configurable
>    `pullGossipPeriod`/`pushGossipPeriod`** (tests use 100 ms) cancelled via
>    `onClose`. Crucially, the `/avax` service constructor changed from
>    `newService(ctx, txpool, state)` to **`newService(ctx, gossipSet,
>    pushGossiper, state)`** — `avax.issueTx` now admits through the bloom set
>    and push-gossips, instead of poking the txpool directly. Metrics register
>    under a **`cchain`** namespace (`gossip_bloom_*`, `gossip_*`). Rust
>    follow-up tracked as plan/M7.33 (the M7.23 AS-BUILT already deferred this
>    wiring; there is now a concrete Go reference).
> 2. **`cchain/dynamic` package** (`2750cc9e42`, #5481): exponential
>    integrators for dynamic C-Chain consensus parameters — see `21` §6
>    upstream-delta (ACP-176 target / ACP-226 min block delay / **ACP-283 min
>    gas price**, a spec-new ACP). **Unconsumed at Go HEAD** (preparatory);
>    plan/M7.34.
> 3. **Frontier-height metrics** (`844535b313`, #5362): `sae` registers gauge
>    **`last_settled_height`** (set in `AcceptBlock` when a block settles and
>    once at startup from the recovered S frontier) and `saexec` registers
>    **`last_executed_height`** (set in `sendPostExecutionEvents` and at
>    `Executor` construction) — both on the `"sae"`-namespaced registry from
>    `snowCtx.Metrics`. See `18` §2.11.

> **Upstream delta (avalanchego `553742045d` #5500 → `72adc639e6` #5535, folded 2026-06-17).**
> SAE execution-pressure metrics. The bulk is metric-name parity (full table in
> `18` §2.11 upstream-delta: `execution_queue_duration_seconds`,
> `execute_block_duration_seconds`, `execution_queue_blocks/gas_limit`,
> `accepted_gas_limit_total`, `executed_gas_{charged,limit}_total`, and the
> `sae` `in_memory_blocks` GaugeFunc). One **structural knock-on** touches this
> spec's execution path (`saexec/execution.go::Execute`, the M7.14 `execute_step`
> analog): `ExecutionResults` gains a **`GasConsumed gas.Gas`** field (= the
> block's charged gas: tx gas used + end-of-block op gas, distinct from eth gas
> used), and `afterExecution` now passes the **whole `*ExecutionResults`** to
> `sendPostExecutionEvents` (previously just `EthBlock()` + `Receipts`) so the
> charged-gas total can be metered at `markExecuted`. The Rust `StepOutput`/event
> seam should carry the charged-gas total alongside the receipts when the
> prometheus registration lands (M7.33 → M8); the value is the per-block
> `block_gas_consumed` the M7.14 driver already computes, so this is an
> additive field, not a behavior change. The queue-timing wrapper (`queuedBlock`
> pairs each enqueued block with `enqueuedAt`) is a Go-side instrumentation
> detail with no consensus surface.
>
> **As-built (2026-06-17).** The M7-side is folded: the executor already computes the charged
> total (Go `blockGasConsumed` = floored per-tx charge + end-of-block op gas) and feeds it to
> `GasTime::after_block`; `StepOutput` now carries it as **`gas_consumed`** (renamed from the
> misleading `gas_used` — the eth quantity it is explicitly *distinct from*). It is **not**
> persisted (Go's `GasConsumed` is metered only, never written to disk), so the `ExecutionResults`
> blob is unchanged. The sole remainder is the **M8** prometheus `executed_gas_charged` counter
> that meters `StepOutput::gas_consumed` at `markExecuted` — gated on the SAE metrics-registry
> seam (`snowCtx.Metrics`), which M7 deferred to node-assembly (see `18` §2.11).

> **Upstream delta (avalanchego `5896c92fee`, #5447 — folded 2026-06-15).**
> `cchain.VM` now **overrides `ParseBlock`** to verify the block's `extData`
> hashes to the `ExtDataHash` committed in its header, rejecting tampered blocks
> *before* they are accepted/persisted/executed. The block ID is the header hash
> (which commits `ExtDataHash`), so a block whose `extData` body was swapped keeps
> the same ID — the SAE VM's own `ParseBlock` is unaware of the C-Chain `extData`
> concept, making this override the boundary that catches the mismatch. The check:
> decode via `vm.VM.ParseBlock`, then compare `GetHeaderExtra(eth.Header()).ExtDataHash`
> against `CalcExtDataHash(BlockExtData(eth))` (= `keccak256(RLP(extData))`, see
> `10` §9), erroring `extData hash does not match header` on mismatch. (A
> `TODO` notes pre-AP1/pre-Helicon blocks that incorrectly left `ExtDataHash`
> unset still need handling to fully retire coreth.) In the Rust port the cchain
> `extData` marshaling itself is still a `TODO(M7.22)` (no `parse_block` override
> exists yet) — so this verification rides on first landing extData
> marshaling/commit; tracked as `plan/M7` M7.37.

> **Upstream delta (avalanchego `4772ab3c97`, #5543 — folded 2026-06-17).**
> `cchain.VM.ParseBlock` gains a **second C-Chain syntactic check beside the
> extData-hash verify above**: the block's `BlockBodyExtra.Version` must be `0`,
> the only supported version, else it rejects with `errInvalidBlockVersion`
> (`"invalid block version: <n>"`). Rationale mirrors the extData case — the
> header (and thus the block ID) commits *neither* the body's `Version` nor its
> `extData` bytes (only `ExtDataHash`), so a block with a tampered `Version` keeps
> the same ID; `ParseBlock` is the boundary that rejects it before
> accept/persist/execute. Go also reworked `cchaintest.NewTestBlock` to build the
> body via `types.NewBlock` + `SetBlockExtra(&BlockBodyExtra{Version, ExtData})`
> (replacing the old `NewBlockWithExtData`, which Go marks for deletion) and added
> a `WithBlockVersion` option. **In the Rust port there is no `BlockBodyExtra`
> wire struct**: approach (B) (M7.37) carries `extData` as a trailing RLP item
> after a stock SAE eth block. **Resolved (M7.39, as-built):** Go's full block
> RLP is `[Header, Txs, Uncles, Version, ExtData]`, so the faithful place for the
> C-Chain `Version` is the trailing RLP item *before* `extData` — the carrier
> becomes the pair `[Version: u32, extData: bytes]` after the bare eth block. A
> bare block (no trailing items) decodes to `Version = 0` + empty `extData`,
> matching Go's `BlockVersion`/`BlockExtData` defaults. `parse_block` checks
> `version != 0` unconditionally and first (Go's order), rejecting with
> `Error::InvalidBlockVersion(u32)`, then runs the existing extData-hash check.
> This extends (does not fork) M7.37's one-item carrier and stays dormant on the
> empty build path. Done in `plan/M7` M7.39. Non-gating (Helicon unscheduled;
> same dormancy as M7.37).

> **Upstream delta (avalanchego `08ae32b741`, #5565 — folded 2026-06-24).**
> `cchain.VM.ParseBlock` now **handles genesis and pre-ApricotPhase1 blocks**,
> resolving the `TODO` the M7.37 delta above flagged (those blocks incorrectly
> left `ExtDataHash` unset). The extData-hash check splits: for `height == 0` **or**
> a block whose `Time` predates `IsApricotPhase1`, the *expected* hash is
> `EmptyExtDataHash` **unless** the `(networkID, height)` pair is listed in a
> hardcoded table (`errExtDataUnexpectedHash` on mismatch); otherwise the hash must
> equal the header's committed `ExtDataHash` (`errExtDataHashMismatch`, the existing
> path). The tables are embedded JSON corpora (`extdata-fuji.json`,
> `extdata-mainnet.json` — the mainnet file is ~63 k entries) decoded at `init()`
> into `map[uint32]map[uint64]ethcommon.Hash` keyed by network then height. `VM`
> now also retains the parsed `chainConfig` (`*ethparams.ChainConfig`) so
> `IsApricotPhase1(time)` is available at parse. This is the work needed to fully
> retire coreth (replay historical chain history through the SAE C-Chain). In the
> Rust port it rides on first landing extData marshaling (M7.37/M7.43); tracked as
> `plan/M7` M7.47. Carrying the mainnet/fuji extData-hash corpora into the Rust tree
> is the bulk of the work. **Non-gating** beyond historical replay, but a
> consensus-parse parity constraint. See `10` §9 header-tail callout.

> **Upstream delta (avalanchego `9b48abd852`, #5523 — folded 2026-06-17).**
> SAE C-Chain gains a dedicated **`cchain/warp` package** consolidating the
> Avalanche Warp (ICM) message lifecycle that `10` §8 documents for the
> *synchronous* C-Chain — now re-homed for the *asynchronous* (ACP-194) C-Chain.
> Four seams, all in `vms/saevm/cchain/warp/`:
> - **`FromReceipts(receipts)`** (`warp.go`) — scans block receipts for logs at
>   the Warp precompile `ContractAddress`, unpacking each into an
>   `UnsignedMessage` (`corethwarp.UnpackSendWarpEventDataToMessage`). This is the
>   *outbound* capture step that, under SAE, runs **after** the block executes
>   (execution follows acceptance) — the data that feeds the message store.
> - **`Storage`** (`storage.go`) — persist/cache warp messages; the SAE analog of
>   coreth's `warp/backend.go` message store. Notably it **must keep coreth's
>   `"warp"` `prefixdb.New` key** (not `NewNested`) so the underlying DB structure
>   stays byte-compatible during the coreth→SAE VM transition — a parity
>   constraint the Rust `Storage` port has to honor. Off-chain operator messages
>   are held in an in-memory `overrides` map. (A `TODO` notes the bytes are never
>   actually read back, only the ID — a possible space optimization, not a wire
>   concern.)
> - **`Verifier`** (`verifier.go`) — the **ACP-118** sign-decision (`acp118.Verifier`):
>   a node signs a message iff it is in `Storage` (precompile-emitted or off-chain),
>   *or* its payload is a `payload.Hash` block-attestation whose block the
>   `Backend.IsAccepted` reports accepted. Refusals carry one of four codes —
>   `StorageErrCode`/`ParseErrCode`/`UnknownMessageErrCode`/`NotAcceptedErrCode`
>   (`iota+1`) — which the Rust port must reproduce for p2p `AppError` parity.
> - **`VerifyBlock(snowCtx, blockCtx, rules, txs)`** (`warp.go`) — the *inbound*
>   predicate pass (`10` §6.5/§8): for each tx with warp predicates in its access
>   list it fans `VerifyPredicate` out over an `errgroup` (one goroutine per
>   predicate, capped at `GOMAXPROCS`), collecting per-precompile failure
>   `set.Bits` into `predicate.BlockResults`. Requires a non-nil proposervm
>   `block.Context` (pins the P-Chain height for the validator set) **only when
>   predicates are present** — `errNoBlockContext` otherwise. The Rust analog is
>   the M6 synchronous-C-Chain predicate pass (`precompile/warp.rs`, `BlockExecutor`
>   pre-execution pass), reused under the SAE driver via `rayon` rather than
>   `errgroup`.
> The Rust SAE C-Chain (`ava-saevm-cchain`, M7.23) has the `/avax` API and
> `Initialize` harness but **no warp lifecycle yet**; this is staged as
> `plan/M7` **M7.38**. **Non-gating:** like M7.37 this is correct-but-dormant
> parity — Helicon is unscheduled on all networks and SAE C-Chain Warp interop is
> not yet exercised.
>
> **As-built (2026-06-17, merge `e675de7`).** M7.38 landed the `warp` lifecycle
> *package* in `ava-saevm-cchain::warp` (`from_receipts` / `Storage` flat-`"warp"`-prefix
> / ACP-118 `Verifier` with the four `iota+1` codes / async-then-`rayon` `verify_block`),
> 22 tests. The *VM-`Initialize` wiring* (feeding `Storage` post-execution, mounting the
> `Verifier` on the p2p handler, calling `verify_block` inbound) is a deferred integration
> step — still "no warp lifecycle wired into the VM" until then. See `plan/M7` M7.38 as-built.

> **Upstream delta (avalanchego `5e040de53e`, #5514 — folded 2026-06-26).** Go
> *wires* the M7.38 `cchain/warp` package into the C-Chain VM lifecycle, resolving
> the three TODOs the M7.38 as-built flagged as deferred. Four seams, all in
> `vms/saevm/cchain/`: (1) `vm.go Initialize` now parses `configBytes` into a
> `config` carrying `WarpOffChainMessages` (`[]hexutil.Bytes` of off-chain
> messages the node will sign), constructs `warp.NewStorage(avaDB, msgs...)`, and
> calls `registerWarpHandler(vm.VM, warpStorage, snowCtx.WarpSigner)` — an
> `acp118.NewCachedHandler` (512-entry LRU) over a `warpBackend` whose `IsAccepted`
> guards against non-canonical `GetBlock` results by re-checking
> `GetBlockIDAtHeight`. (2) `hooks.AfterExecutingBlock` replaces `_ = receipts`
> with `warp.FromReceipts(receipts)` → `warpStorage.Add(...)` (persist produced
> messages). (3) `hooks.BuildBlock` replaces `_ = blockCtx` with the inbound
> predicate pass `warp.VerifyBlock(ctx, blockCtx, rulesExtra, ethTxs)`, serializing
> the resulting `warpValidity.Bytes()` into `header.Extra` via
> `customheader.SetPredicateBytesInExtra` (Go flags the current predicate-bytes
> format as inefficient — a `// TODO` to repack as canoto). (4) `hooks`/`builder`
> now thread `*params.ChainConfig` so `BuildBlock` can compute `rulesExtra`. The
> Rust analog (`ava-saevm-cchain`) already has the `warp` *package* (M7.38); this
> is the VM-`Initialize` integration the M7.38 as-built explicitly deferred. Staged
> as `plan/M7` **M7.50**. **Non-gating** (Helicon unscheduled, SAE C-Chain Warp
> interop not yet exercised).

> **Upstream delta (avalanchego `f72fee1347`, #5441 — folded 2026-06-26).** The
> ACP-283 `MinPriceExponent` (M7.34 `dynamic.PriceExponent` integrator, M7.46 wire
> home) is now *consumed* in the C-Chain block lifecycle, replacing the hardcoded
> `MinPrice: 1`. In `vms/saevm/cchain/`: `dynamic.InitialPriceExponent = 0` (the
> 1-wei minimum) is added; `hooks.GasConfigAfter(header)` now returns
> `MinPrice: priceExponent(header).Price()` where `priceExponent` reads
> `GetHeaderExtra(h).MinPriceExponent` (defaulting to `InitialPriceExponent` when
> absent); `builder.BuildHeader` computes the child exponent as
> `priceExponent(parent).Toward(b.desired.priceExponent)`, where the node's
> *desired* exponent comes from the operator config's `min-price-target`
> (`config.desired()` → `dynamic.DesiredPriceExponent(PriceTarget)`); and
> `BlockRebuilderFrom` re-derives `desired` from the existing header's exponent.
> Genesis seeds `InitialPriceExponent` (was a zero-valued `new(PriceExponent)`).
> This is the integrator→pricing wiring the M7.34/M7.46 callouts anticipated — no
> new formula (see `21` §6.x). Staged as `plan/M7` **M7.51**. **Non-gating**
> (Helicon unscheduled).

> **Upstream delta (avalanchego `cbea62895c`, #5574 — folded 2026-06-26).**
> `vms/saevm/cchain` gains a real operator-config surface: `Initialize` decodes
> `configBytes` into a `config` (new `config.md` documents the JSON keys) with a
> `defaultConfig()` (pruning on, default tx-pool slots) so unset fields populate
> sane defaults. Active keys: `min-price-target` (the ACP-283 `PriceTarget`,
> above), `pruning-enabled` + `commit-interval` (→ `saedb.Config{Archival,
> TrieCommitInterval}`), and `local-txs-enabled` / `tx-pool-account-slots` /
> `tx-pool-global-slots` (→ `legacypool.Config`). A `config.saeConfig(now)` helper
> folds these into the `sae.Config` previously hardcoded in `Initialize` (the
> `mempoolConfig.NoLocals = true` line moves behind `!LocalTxsEnabled`). Many
> coreth keys remain commented-out stubs (trie caches, state-sync, API limits) —
> a `// TODO(JonathanOppenheimer) enable and wire all remaining configs`. The Rust
> analog is the `ava-saevm-cchain` VM config decode (cross-ref `13`/`14` chain
> config). Staged as `plan/M7` **M7.52**. **Non-gating** (Helicon unscheduled).

### Reuse decision (binding cross-ref to `10`)

**Decision: SAE's `cchain` reuses `ava-evm` (reth/revm + Firewood), not an
independent EVM.** Rationale and mapping:

- The Go `cchain` runs EVM execution through `libevm` `core.ApplyTransaction` /
  `core/state` / `triedb`. In Rust those become **reth/revm** + the Firewood
  `StateProvider` defined in `10`/`04`. There is exactly one EVM engine in the
  workspace (`00` §4.5). `saexec::Execute` calls `ava-evm`'s revm executor; it
  does **not** re-implement opcodes.
- The **C-Chain block-building/fee/atomic-tx semantics** that Go's `cchain` adds
  on top of `libevm` are re-expressed as: (a) SAE `hook::Points` for the SAE
  lifecycle, and (b) reth `ConfigureEvm` / custom `PayloadBuilder` inspector hooks
  where the customization is *EVM-internal* (precompiles, fee recipient) — per
  `10`'s extension model. The two coexist: SAE hooks own the *streaming/settlement*
  concerns; reth config owns the *EVM-execution* concerns.
- What is **distinct** from `10`'s C-Chain: the *consensus/ordering* layer. The
  reth-based C-Chain in `10` is the *synchronous* (current) C-Chain; SAE's
  `cchain` is the *asynchronous* (ACP-194) C-Chain. They **share the EVM engine and
  Firewood state layout** but differ entirely in the block lifecycle (synchronous
  verify-then-vote vs. order-then-stream-execute). `10` and `11` therefore share
  `ava-evm` as a dependency; `11` adds the SAE machinery around it.

> **Net:** one `revm`, one Firewood state format, two block-lifecycle drivers.
> `10` MUST expose its revm executor + Firewood `StateProvider` as reusable APIs
> (not bury them in a synchronous-only VM) so `ava-saevm-exec` can call them.
> This is recorded as a cross-spec requirement on `10`.

---

## 9. Hooks, txgossip, worst-case analysis

### 9.1 Hooks (`ava-saevm-hook`)

`hook::Points` / `PointsG<Tx>` are the **only** seam between the SAE core and a
concrete chain — they make the core EVM-policy-agnostic. The trait set (cited
`hook/hook.go`): `execution_results_db`, `gas_config_after`, `block_time`,
`settled_by`, `end_of_block_ops`, `can_execute_transaction`,
`before/after_executing_block`, and the generic `BlockBuilder<Tx>`
(`build_header`, `potential_end_of_block_ops`, `build_block`) +
`block_rebuilder_from`. `Op` (mint/burn with nonce-authorized debits + min-balance
guard) and `Settled` are the data types. Ported as object-safe
traits behind `Arc<dyn …>` where dynamic, generic over `Tx: Transaction` for the
builder.

> **AS-BUILT (M7.9).** These hook points are **synchronous** — Go is sync at all
> of `BeforeExecutingBlock`/`AfterExecutingBlock`/`EndOfBlockOps`/`ApplyTo`, so
> `Points`/`BlockBuilder`/`PointsG` carry **no `#[async_trait]`**. The trait
> *seam* + `Op`/`Settled` land in M7.9; the method **bodies** land in M7.21
> (C-Chain hooks). State mutation in `Op::apply_to` goes through a minimal
> object-safe `StateMut` trait (the revm-backed impl is M7.14), and `Op` uses
> `BTreeMap` (deterministic iteration, not `HashMap`). Trait methods that take
> the libevm `params.Rules` / Snowman `block.Context` / `*types.Transaction` /
> `Receipt` / `saetypes.BlockSource` use **placeholder associated types** until
> those are wired (M7.14/M7.21), since the concrete reth `Block` is not yet
> re-exported from `ava-saevm-types`. `gasprice` (§3) decouples from blocks (§4)
> via the same kind of `Backend`/`BlockSource` trait seam (the worst-case-bounds
> next-block base-fee is a `Backend` hook, default `None`, wired in M7.13).

### 9.2 Tx gossip (`ava-saevm-txgossip`)

An EVM mempool (`txpool` over the reth pool, `10`) wrapped in avalanchego's
push/pull gossip (`05`): `Transaction` is `gossip::Gossipable` (RLP), `Set`
couples a `gossip::BloomSet` with the pool, `TransactionsByPriority` feeds the
builder, and push (100 ms)/pull (1 s) gossipers run as tokio tasks
(`sae/vm.go::NewVM` P2P section). `priority.go` orders by effective tip.

### 9.3 Worst-case analysis (`ava-saevm-worstcase`)

Because a block is built **before** its txs execute and its `BaseFee`/`GasLimit`
are *predictions*, the builder must guarantee every included tx will *still be
valid and affordable* whenever it eventually executes — under the **worst case**
that the entire queue ahead of it consumed maximum gas (pushing base fee up). Go
`worstcase.State` replays settled→parent history then the new block on a state DB,
tracking the worst-case gas clock, base fee, per-op min-balances, and gas limit
`Ω_B = R·Tau·Lambda`. A tx is includable iff it passes intrinsic validation,
EOA/nonce checks, the `can_execute_transaction` hook, and worst-case affordability
(`mulAdd(gas, fee_cap, value)` with `U256`). The resulting `WorstCaseBounds`
(`MaxBaseFee`, `LatestEndTime`, `MinOpBurnerBalances`) are attached to the block;
the executor **asserts** actual ≤/≥ bound (`CheckBaseFeeBound`,
`CheckSenderBalanceBound`) and logs (test-fatal) on violation — an early-warning
that the prediction model is wrong. Ported with `U256` (`ruint`) and checked math.

> **AS-BUILT (M7.13).** `WorstCaseBounds` is defined in **`ava-saevm-blocks`** (not
> `ava-saevm-worstcase`): the dependency direction is `worstcase → blocks`, and the
> `Block` stores `bounds: OnceLock<WorstCaseBounds>`, so the type must live at or
> below `blocks`. Expanding it (`max_base_fee: Price`, `latest_end_time: GasTime`,
> `min_op_burner_balances: Vec<BTreeMap<Address, U256>>`) re-added the `gastime`
> dep that M7.11 had dropped. `worstcase::State<H: Points>` replays over the
> **object-safe `hook::StateMut` seam passed per-call** (`apply(&mut self, &Op,
> &mut dyn StateMut)`) rather than owning a concrete `state.StateDB` like Go — so
> the replay is unit-testable with an in-memory fake, matching the M7.9/M7.12
> seam-with-deferred-revm-impl precedent. The reth-tx half of `ApplyTx` (libevm
> `txpool.ValidateTransaction` intrinsic-gas validation, `types.Sender` signer
> recovery, `GetCodeHash` EOA check) and the saedb-opener-driven `State::new`
> (open state at the settled root, rebuild the `GasTime` from the settled block's
> `executed_by_gas_time()` `Time<u64>` via the hook gas config, raise
> `SettledBlockNotExecuted`) are **deferred to M7.14**, where the concrete reth
> signed-transaction + a `code_hash` state accessor first exist.

---

## 10. Invariants (from `docs/invariants.md`) — test-enforced

Each is a property/integration test (`02`). `D`=disk, `M`=memory, `I`=internal
pointer, `X`=external signal; `→` = "happens-before" (the prerequisite occurs
*before* the guarantor).

1. **Frontier ordering:** at all times `height(S) ≤ height(E) ≤ height(A)`.
2. **Stage causality:** `b∈S → b∈E → b∈A` (a block is settled only if executed,
   executed only if accepted).
3. **Persistence ordering on execute:** `mark_executed` writes receipts + canoto
   blob + head pointers to disk (D), *then* sets the `execution` cell (M), *then*
   advances `last_executed` (I), *then* fires `executed` (X). Test asserts a
   reader observing X can always read D.
4. **Persistence ordering on accept:** finalized-hash (settled) persisted **before**
   canonical-hash (accepted): `D(σ∈S) → D(b∈A)`.
5. **Settle-in-order:** `mark_settled` called on `Σ_n` in increasing height;
   `M(b_n∈S) → M(b_{n-1}∈S)`.
6. **Atomics-before-broadcast:** the internal pointer is updated before the
   `WaitUntil{Executed,Settled}` notify fires (poll sees ≥ what broadcast saw).
7. **Recovery equivalence:** a node restarted from disk reconstructs identical A/E/S
   frontiers and identical post-state roots (differential vs. pre-restart).
8. **GC of settled ancestry:** after `mark_settled`, `parent()`/`last_settled()`
   return `None`; `InMemoryBlockCount` returns to baseline (no leak).
9. **No reorg:** acceptance is final; the snapshot layer may be flattened freely.
10. **Receipt-root match:** stored `receipt_root == derive_sha(receipts)`
    (`CheckInvariants`).
11. **Determinism:** execution output independent of wall-clock and map order
    (§6.1); base-fee/gas-time are pure functions of the ordered chain.

---

## 11. Go → Rust mapping (non-obvious)

| Go | Rust |
|---|---|
| `proxytime.Time[D Duration]` (`~uint64`) | `proxytime::Time<D: ProxyUnit>` |
| `gas.Gas`, `gas.Price` | `ava_types::{Gas, GasPrice}` (`u64` newtypes) |
| `time.Add(-params.Tau)` (forbidden raw `TauSeconds`) | `BlockInstant::minus(params::TAU)` — no `Add<u64>` exists (§2.3) |
| `holiman/uint256`, `big.Int` | `ruint::U256` / `alloy_primitives::U256` |
| `atomic.Pointer[Block]` | `arc_swap::ArcSwap<Block>` / `ArcSwapOption` |
| `chan struct{}` close as broadcast | `tokio::sync::Notify` / `oneshot` |
| `event.FeedOf[T]` | `tokio::sync::broadcast` |
| `eventual.Value[*Receipt]` | a `OnceCell`/`watch`-backed `Eventual<T>` |
| `syncMap[K,V]` with on-store/on-delete | `DashMap` + explicit track/untrack calls |
| `io.Closer` `toClose` reverse order | `Vec<Box<dyn FnOnce>>` drained in reverse on shutdown (or `Drop`) |
| `runtime.AddCleanup` leak counter | `Drop` + `AtomicI64` (test only) |
| `canoto` codec | `ava-codec`-derived canoto-compatible blob (persisted only) |
| `rlp.EncodeToBytes(ethBlock)` | reth `alloy_rlp` encode (byte-identical) |
| `core.ApplyTransaction` (libevm) | `ava-evm` revm executor (`10`) |
| `triedb`/`snapshot.Tree` | Firewood `Db` revisions + reth state caches (`04`) |
| goroutine executor + buffered chan | tokio task + bounded `mpsc` (dedicated exec thread for Firewood) |
| `context.Context` | `&CancellationToken` + deadlines (`00` §6) |

### Error model

Per crate, `thiserror` enum + `pub type Result<T>`. Sentinels preserved as
variants and matched (mirroring Go `errors.Is`): `ErrExecutionLagging`,
`ErrQueueFull`, `ErrBlockTime{UnderMinimum,BeforeParent,AfterMaximum}`,
`ErrParentHashMismatch`, `ErrHashMismatch`, `ErrBlockTooFarInFuture`,
`ErrSettled{Root,Height}Mismatch`, `ErrNotFound`/`ErrFutureBlockNotResolved`/
`ErrNonCanonicalBlock`. The executor distinguishes a **`Fatal`** variant
(stops the stream, points at the playbook) from recoverable errors. `anyhow` only
in the binary/tests (`00` §7.1).

---

## 12. Test plan (cross-ref `02`)

- **Gas-time accounting (proptest):** `proxytime`/`gastime` are pure integer math
  — property tests for monotonicity (`tick`/`set_rate`/`fast_forward_to` never go
  backwards), `compare` consistency across differing rates, `before_block`/
  `after_block` excess scaling round-trips, `price` ≥ `min_price`, and
  `excess_for_price ∘ calculate_price` inverse bounds. Differential against the Go
  `gastime`/`proxytime` (run Go as oracle, `02`).
- **Tau-discipline compile-fail test:** a `trybuild` case asserting
  `block_instant - TAU_SECONDS` (a `u64`) does **not** compile — the structural
  analogue of the Go `tausecondslint`.
- **Settlement logic:** table tests for `Range`/`Settles`/`last_to_settle_at`
  including the `known=false` (execution-lagging) path and the synchronous-block
  edge cases.
- **Execution determinism:** execute the same ordered block set on N runs / N
  thread counts → identical roots, receipts, gas-times. Map-order fuzzing of any
  intermediate collections must not change output (§6.1).
- **Recovery / restart:** build→accept→execute→settle a chain, snapshot disk, drop
  the VM, reconstruct → assert identical A/E/S frontiers and roots (invariant 7);
  fuzz the crash point (mid-execute, between commit interval and head).
- **Worst-case as property tests:** for random tx sets, assert the executor's
  actual base fee ≤ `MaxBaseFee` and sender balances ≥ `MinOpBurnerBalances`
  (the bounds must *never* be violated) and that `ErrQueueFull` paces the builder.
- **Backpressure:** flood `accept` faster than execution; assert the queue stays
  bounded and the builder refuses via `ErrQueueFull` rather than unbounded growth.
- **Adaptor/Snowman conformance (`06`/`07`):** the converted VM satisfies the
  `block::ChainVm` contract; bootstrap accept-in-a-loop blocks on execution.
- **Differential vs Go `saevm` (`02`):** drive identical block/tx sequences
  through both; assert byte-identical block hashes, state roots, receipt roots,
  base fees, settlement choices, and frontier heights at every step.
- **Invariants §10:** each numbered invariant has a dedicated assertion harness.

> **Implementation note (M7.29/M7.30, 2026-06-10) — the live-Go-oracle pattern
> as built.** The differential is a *recorded-oracle backed by a live Go run*:
> a Go emitter committed in **this** repo (`tests/differential/go-oracle/*.go`,
> an in-package `sae` `_test.go` so it can use the unexported Go test harness)
> is copied into the avalanchego checkout and run env-gated
> (`SAE_EMIT_{RECOVERY,STREAMING}_VECTORS=<dir>`); the committed corpora
> (`tests/vectors/saevm/{recovery,streaming}_differential/`) carry each block's
> **wire bytes**, so the Rust side gets input byte-parity *by construction* and
> the comparison lands on what Rust **recomputes**: re-sealed block hashes, the
> per-barrier settlement choice (`last_to_settle_at`/`settle()` walk), and the
> A/E/S trajectories. EVM state/receipt roots and base fees **round-trip** the
> Go-emitted values (an independent root recompute is a real-EVM differential
> follow-up; it would also re-open the firewood ffi v0.5-vs-v0.6 pin question,
> `04` §4.2 delta). Two findings: (1) the settlement boundary can land on a
> **sub-second gas-time tie**, so streaming vectors must carry the full-precision
> fraction `{seconds, frac_num, frac_denom}` — whole-second gas-times are NOT
> sufficient for settlement parity; (2) Go's *test-stub* `BlockTime` injects a
> sub-second header component that neither the production cchain hook nor the
> Rust `Block::timestamp()` models — scripted streams therefore advance
> wall-clock by whole seconds. The `00` §9 pipelined-commit neutrality claim is
> validated cross-stream (archival vs interval-16 reach identical final A/E/S).

---

## 13. Performance notes / improvements over Go

SAE is precisely where Rust + the Firewood-pipelining contract (`04`) shine; all
gains are **observably-neutral** (same hashes/roots/ordering — §6.1, validated by
the differential suite §12):

1. **Execution fully off the consensus latency path.** This is ACP-194's whole
   premise; we keep it. Consensus votes on cheap (no-execution) verification, so
   round latency is independent of EVM throughput. Quantify in load tests as
   *blocks accepted/s decoupled from gas executed/s* up to the `Ω_Q` queue cap.
2. **Firewood `propose`/`commit` pipelining (`04` §4.2, `00` §9).** The state root
   is available at `propose` time (pre-commit), so the executor advances E and
   reports roots while `commit` lands durably on the **commit interval** in the
   background — Go already batches via `triedb` commit-interval, but Firewood's
   stacked proposals let us *overlap* hashing of block N+1 with commit of block N
   on the dedicated exec thread. Safe because commit cadence does not affect any
   consensus output, only durability/recovery start point.
3. **Parallel signature recovery / worst-case validation at build time.** Sender
   recovery for the candidate tx set and worst-case affordability checks are
   independent across txs → `rayon` batch (Go does `SenderCacher.Recover`
   async already; we widen it). Ordering of *inclusion* is unchanged.
4. **Zero-copy block bytes / receipt buffers** on the gossip + RPC paths
   (`bytes::Bytes`, `00` §9) — RLP encode once, share.
5. **Lock-free frontiers.** `arc_swap` reads for A/E/S pointers and the
   consensus-critical map (`DashMap`) replace Go's `RWMutex`-guarded `syncMap`,
   removing reader contention on hot RPC paths (`latest`/`safe` label resolution).
6. **`Notify`/`broadcast` instead of channel-close fan-out** for
   `WaitUntil{Executed,Settled}` and chain-head events — fewer allocations,
   bounded broadcast lag with explicit handling.

Each is gated behind a differential test proving identical external behaviour vs.
the Go `saevm` reference before it is enabled.

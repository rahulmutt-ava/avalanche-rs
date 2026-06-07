# ava-vm — porting notes

Tracks the Go → Rust port of the VM-framework base traits (`specs/07-vm-framework.md`
§2.1, §2.2, §2.6, §9). This crate (M3.14) lands only the base trait family + error
model; the items below are deliberate follow-ups, recorded so later milestones close
them rather than re-deriving the gaps.

## Go source

- `snow/engine/common/vm.go` — `VM` → [`crate::vm::Vm`].
- `snow/engine/common/engine.go` (`AppHandler` + `AppRequestHandler` /
  `AppResponseHandler` / `AppGossipHandler`) → [`crate::app::AppHandler`].
- `snow/engine/common/error.go` (`AppError`, `ErrUndefined`, `ErrTimeout`) →
  [`crate::app::AppError`].
- `snow/engine/common/message.go` (`Message`) → [`crate::vm::VmEvent`].
- `snow/engine/common/sender.go` (`AppSender`, `SendConfig`) →
  [`crate::app_sender::{AppSender, SendConfig}`].
- `snow/engine/common/fx.go` (`Fx`) → [`crate::fx::Fx`] (`{ id, fx: Arc<dyn FxInstance> }`).
- `vms/fx/fx.go` + `vms/secp256k1fx` verification surface →
  [`crate::fx::{FxInstance, FxVm, CodecRegistry, UnsignedTx}`] (M3.20).
- `snow/validators/connector.go` (`Connector`) → [`crate::connector::Connector`].
- `api/health/checker.go` (`Checker`) → [`crate::health::HealthCheck`].

## Faithful placeholders / deferred surface

1. **`Fx` payload (M3.20 — DONE).** Go's `common.Fx{ ID, Fx interface{} }` is now
   [`crate::fx::Fx`] `{ id, fx: Arc<dyn FxInstance> }`. The fx framework
   ([`crate::fx`], specs 07 §4.1) is built: `FxInstance` (the `&dyn Any`
   verification surface), `FxVm` (host `codec_registry()`/`clock()`),
   `CodecRegistry`, and the `UnsignedTx` tx-bytes trait. `ava-secp256k1fx`
   implements `FxInstance` as `Secp256k1Fx` (`tests/fx.rs` exercises the
   wrong-type downcast sentinels + the 5-typeID codec registration). `FxVm`
   omits Go's `Logger()` (no workspace `Logger` type yet — see the
   `ava-secp256k1fx` PORTING note + the recommended 07 §4.1 spec tweak).

2. **`HttpHandler` body.** The root workspace pulls in no `tower`/`http`/`hyper`
   dependency, so [`crate::vm::HttpHandler`] is a descriptor (`LockOptions` +
   opaque `handler: Vec<u8>`) rather than a boxed `tower::Service` as the spec
   sketches (§2.1). `LockOptions` is preserved verbatim (`WriteLock=0`,
   `ReadLock=1`, `NoLock=2`) for `proto/vm`/`proto/http` wire parity even though
   the Rust VM, being its own actor, applies no lock here. Swap in a real
   in-process service type when the HTTP stack (specs 07 §5, specs 12) lands.

3. **genesis / upgrade / config bytes.** Passed as `&[u8]` per the spec note —
   no typed genesis/upgrade/config structs are pulled in.

4. **`AppError` vs crate `Error`.** `AppError` is intentionally a *separate*
   `thiserror` type (matched by integer `code` via `AppError::is`, mirroring Go's
   `(*AppError).Is`), not a variant of [`crate::error::Error`] (which is matched
   structurally with `matches!`). The fx wrong-type set lives on `Error` and is
   re-exported by `ava-secp256k1fx`.

## Dep choices

- `tokio-util` (`CancellationToken`) replaces `context.Context` on every method.
- `ava-snow` re-export supplies `ChainContext`/`ConsensusContext`/`EngineState`.
- `ava-database` supplies `Arc<dyn DynDatabase>` for `Vm::initialize`.
- `ava-version` supplies `Application` for `Connector::connected`.
- `SendConfig` is defined locally (not re-exported from a `Sender` crate) to keep
  `ava-vm` free of a networking dependency; its fields mirror Go exactly.

## M3.15 — Snowman VM trait family (`block/`)

Go source → Rust:

- `snow/engine/snowman/block/vm.go` (`ChainVM`/`Getter`/`Parser`) →
  [`crate::block::chain_vm::ChainVm`]. `&mut self` only on the mutating ops
  (`build_block`/`set_preference`); read ops take `&self` (Go relies on
  `ctx.Lock`, which we drop — specs 07 §2.4 mutability note).
- `block.BuildBlockWithContextChainVM` / `SetPreferenceWithContextChainVM` →
  [`BuildBlockWithContext`]/[`SetPreferenceWithContext`], probed via the
  `as_build_with_context`/`as_set_preference_with_context` accessors (default
  `None`, mirroring Go's interface type-assertions).
- `block.WithVerifyContext` + `block.Context` → `block/with_context.rs`.
- `batched_vm.go` (`BatchedChainVM` + the `GetAncestors`/`BatchedParseBlock`
  free-function fallbacks) → `block/batched.rs`. `wrappers.IntLen == 4` is
  reproduced as [`crate::block::INT_LEN`]; the byte accounting (each element
  costs `len + INT_LEN`), the `Err(NotFound)`-on-head ⇒ empty special-case, and
  the break-on-parent-error are byte-for-byte faithful.
- `state_syncable_vm.go` / `state_summary.go` →
  `block/state_sync.rs` (`StateSyncableVm`/`StateSummary`/`StateSyncMode` with
  Go's `1/2/3` discriminants).
- `Block` is owned by `06` and **re-exported** from `ava_snow::Block` (specs 07
  §2.3) — note `Block::{verify,accept,reject}` return `ava_snow::Result`, while
  the `ChainVm` surface returns `ava_vm::Result`.

### Findings / deltas

1. **`get_ancestors` capacity.** Go does `make([][]byte, 1, maxBlocksNum)`; we
   cap the *capacity hint* at `min(max_blocks_num, 1024)` so an unbounded
   `max_blocks_num` cannot trigger an allocator capacity overflow. The result
   *length* is identical to Go.
2. **`vm_conformance!` closure takes an owned `CancellationToken`.** The async
   make-VM closure receives the token **by value** (cheap clone), not by
   reference, to sidestep the higher-ranked-lifetime borrow on the returned
   future. Each generated `#[tokio::test]` owns its own token. Macro lives in
   `testutil.rs` (gated on the `testutil` feature) so downstream VMs (`08`–`11`)
   and the rpcchainvm host/guest can reuse the battery.
3. **`testutil` feature.** `TestVm`/`TestBlock`/`NoopAppSender`/`init_test_vm` +
   the macro are gated behind `feature = "testutil"` (pulls `tokio`+`sha2`); the
   `conformance_vm` integration test is `#![cfg(feature = "testutil")]`.

## M3.18 — shared components (`components/`)

Go source → Rust:

- `vms/components/avax/{utxo_id,asset,utxo,transferables,base_tx,flow_checker}.go`
  → `components/avax/mod.rs`. `serialize:"true"` fields are encoded in
  registration order; `fx_id` (`serialize:"false"`) is runtime-only. `UtxoId.id`
  is a lazy `OnceLock` (`input_id() == tx_id.prefix(&[output_index as u64])`).
  `sort_transferable_outputs` (assetID, then `out.codec_bytes()`),
  `sort_transferable_inputs[_with_signers]` (UTXOID), and the `is_sorted*`
  predicates reproduce Go's comparators exactly (consensus-affecting).
  `FlowChecker` uses a `BTreeMap` + `safemath` checked add; `verify_tx` burns the
  fee and requires `consumed >= produced` per asset + all-sorted.
- `chains/atomic/shared_memory.go` (`SharedMemory`/`Requests`/`Element`) →
  `components/avax/shared_memory.rs`. `apply` is keyed by `BTreeMap<Id, Requests>`
  (no `HashMap` on a write path) and takes `&[BatchOps]`.
- `vms/components/verify/verification.go` → `components/verify.rs`
  (`Verifiable`/`State`/`all`). The `IsState`/`IsNotState` marker split is encoded
  at the type level (a type is `State` iff it `impl`s `State`).
- `vms/components/chain/{state,block}.go` → `components/chain/state.rs`
  (`ChainState`/`ChainStateConfig`/`BlockWrapper`).
- `vms/components/gas/{gas,state,dimensions}.go` → `components/gas.rs`. Integer
  only — **no floats**. `calculate_price` reproduces Go's `fakeExponential`
  fixed-point loop bit-for-bit (golden-tested against Go's vectors).

### Findings / deltas

1. **`calculate_price` uses `num_bigint::BigUint`** in place of Go's
   `uint256.Int` (intermediate values reach ~`MaxUint192`, so `u128` is
   insufficient and a `U256` type is not in the workspace). The result is
   bit-identical to Go for every `gas_test.go` vector. No floats anywhere.
2. **`chain::State` caches are count-bounded, not byte-sized.** Go uses
   `lru.NewSizedCache` (eviction by cumulative byte size); this crate uses a
   small internal count-bounded `CountLru` (`ava-utils` has no LRU yet). The
   observable behaviour (tiering verified/decided/unverified/missing, idempotent
   get/parse, `last_accepted` tracking via the `BlockWrapper` lifecycle) is
   identical — only the eviction *metric* differs. Swap to a byte-sized LRU when
   `ava-utils` grows one.
3. **`TransferableOut::codec_bytes()`** is an explicit trait method standing in
   for Go's `codec.Marshal(out)` as the secondary output sort key, because the
   codec↔fx wiring (`ava-secp256k1fx`, later milestone) is not yet built. The
   concrete fx output must return its canonical encoded bytes here.
4. **New `Error` variants:** `Overflow`/`Underflow`/`InsufficientFunds`/
   `InsufficientCapacity`/`OutputsNotSorted`/`InputsNotSortedUnique`/
   `InvalidComponent(&'static str)` were added for the avax/gas paths. The
   structural-validation messages (`BaseTx::verify`, `Asset::verify`) are carried
   verbatim from Go via `InvalidComponent`.
5. **`SharedMemory`/`Metadata`/`BaseTx` are scaffolding** for the P/X-Chain VMs
   (`08`/`09`): the trait + serializable payloads + cached-bytes `OnceLock` shape
   are in place; the concrete impls (atomic DB, codec-driven `initialize`) land
   with those VMs.

## M3.16 — `middleware` (MeterVm + TracedVm)

Go source: `vms/metervm/{block_vm.go,block_metrics.go,metrics.go,batched_vm.go,
state_syncable_vm.go,build_block_with_context_vm.go,set_preference_with_context_vm.go}`
and `vms/tracedvm/{block_vm.go,batched_vm.go,state_syncable_vm.go,…}`.

1. **`MeterVm` uses an `Averager` (count + sum), not a Prometheus `Histogram`.**
   The task spec says "Prometheus histogram"; the faithful Go port is
   `metric.Averager` = a `<name>_count` counter + a `<name>_sum` gauge (summing
   observed nanoseconds — `utils/metric/averager.go`). Metric **names** are
   byte-identical to Go (`build_block_count`/`build_block_sum`/…), so dashboards
   port unchanged. If a true `_bucket` histogram is wanted later it can swap in
   under the same `<name>` prefix without changing call sites.
2. **Capability forwarding = `Some(self)`, not a re-wrapped trait object.** Go
   re-exposes `BatchedChainVM`/`StateSyncableVM`/`*WithContext` via interface
   embedding + type-assertion. In Rust each wrapper probes the inner VM's
   capabilities **once at construction** (storing a bool) and has
   `as_batched()`/`as_state_syncable()`/`as_*_with_context()` return `Some(self)`;
   the wrapper itself implements those traits, delegating to
   `self.inner.as_batched()` etc. So the forwarded calls are themselves
   metered/traced — a wrapped proposervm keeps its batched/state-sync surface.
3. **`should_verify_with_context`/`verify_with_context`(`_err`) averagers are
   registered (name-parity with Go) but not yet observed.** Go observes them from
   the per-block `meterBlock` wrapper's `ShouldVerifyWithContext`/
   `VerifyWithContext`. This port does not wrap individual blocks (the `Block`
   trait is returned as `Arc<dyn Block>` straight through), so they are
   `#[allow(dead_code)]` until a `MeterBlock`/`TracedBlock` wrapper lands.
4. **`TracedVm` uses `tracing::Span` + `Instrument`, not OTel directly.** Each
   async method runs `future.instrument(span)`, which guarantees the span ends
   when the future resolves/drops — the Rust analogue of Go's `defer span.End()`
   (00 §4.6). Span tag shape is `tracedvm{vm=<name>, method=<method>}`; the
   `<name>.<method>` Go tag is reconstructible from the fields. A real OTel
   exporter wires in at the node-assembly layer.
5. **New deps pinned directly in `crates/ava-vm/Cargo.toml`** (not workspace
   deps yet): `prometheus = { version = "0.13", default-features = false }`
   (mirrors `crates/ava-network/Cargo.toml`) and `tracing = { workspace = true }`.
   Promote `prometheus` to `[workspace.dependencies]` when a third crate needs it.
6. **New `Error::FailedRegistering`** variant (= Go `metric.ErrFailedRegistering`)
   for a metric name collision in the supplied registry.

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
- `snow/engine/common/fx.go` (`Fx`) → [`crate::vm::Fx`] (id-only placeholder).
- `snow/validators/connector.go` (`Connector`) → [`crate::connector::Connector`].
- `api/health/checker.go` (`Checker`) → [`crate::health::HealthCheck`].

## Faithful placeholders / deferred surface

1. **`Fx` payload.** Go's `common.Fx{ ID, Fx interface{} }` carries an arbitrary fx
   instance. The fx framework (`FxInstance`, specs 07 §6) is not built yet, so
   [`crate::vm::Fx`] carries only `id: ava_types::id::Id`. Add the `fx:
   Arc<dyn FxInstance>` field when `ava-secp256k1fx` lands; `Vm::initialize` already
   takes `Vec<Fx>` so the signature does not change.

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

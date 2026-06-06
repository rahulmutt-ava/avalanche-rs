# ava-engine — porting notes (Task M3.9)

Port of the `snow/engine/common` op state machine: the inbound-op `Handler`
traits, `Engine`, the log-and-drop `NoOpHandler`, the typed `AppError`, and the
engine-facing `Sender` + `SendConfig`.

Go reference (pinned tree at `../avalanchego`):
- `snow/engine/common/engine.go` — `Engine`/`Handler`/`AllGetsServer` + every
  per-op handler interface (the full op set).
- `snow/engine/common/error.go` — `AppError` + `ErrUndefined`/`ErrTimeout`.
- `snow/engine/common/no_ops_handlers.go` — the `noOp*Handler` family.
- `snow/networking/sender/sender.go` — the outbound `Sender` surface (specs 06
  §5.3 is the trimmed engine-facing view).

## Op-group → trait mapping

| Go interface | Rust trait (`common::handler`) |
|---|---|
| `GetStateSummaryFrontierHandler` + `StateSummaryFrontierHandler` + `GetAcceptedStateSummaryHandler` + `AcceptedStateSummaryHandler` | `StateSyncHandler` |
| `GetAcceptedFrontierHandler` + `AcceptedFrontierHandler` | `FrontierHandler` |
| `GetAcceptedHandler` + `AcceptedHandler` | `AcceptedHandler` |
| `GetAncestorsHandler` + `AncestorsHandler` | `AncestorsHandler` |
| `GetHandler` + `PutHandler` | `PutHandler` |
| `QueryHandler` (`PullQuery`/`PushQuery`) | `QueryHandler` |
| `ChitsHandler` (`Chits`/`QueryFailed`) | `ChitsHandler` |
| `AppHandler` (`AppRequest`/`AppResponse`/`AppRequestFailed`/`AppGossip`) | `AppHandler` |
| `InternalHandler` (`validators.Connector` + `Gossip`/`Shutdown`/`Notify`) | `InternalHandler: Connector` |
| `SimplexHandler` (`Simplex`) | `SimplexHandler` |
| `AllGetsServer` | `AllGetsServer` (blanket-impl marker super-trait) |
| `Handler` | `Handler` (blanket-impl over the union) |
| `Engine` | `Engine: Handler` (`start`/`health_check`) |

Every request op keeps its `*_failed` callback (`get_failed`, `query_failed`,
`get_accepted_failed`, `get_ancestors_failed`, `get_accepted_frontier_failed`,
`get_state_summary_frontier_failed`, `get_accepted_state_summary_failed`,
`app_request_failed`) on the handler that owns the corresponding response, exactly
as Go places them.

## Deliberate deviations from Go (record for spec/plan)

1. **`AllGetsServer` is a blanket-implemented marker super-trait**, not a leaf
   trait with its own methods. Go composes `AllGetsServer` out of the six `Get*`
   interfaces; we already place each `Get*` method on its owning op-group trait
   (`StateSyncHandler`/`FrontierHandler`/`AcceptedHandler`/`AncestorsHandler`/
   `PutHandler`), so `AllGetsServer` is just their conjunction. This avoids
   duplicating method signatures and keeps `Handler` object-safe.

2. **`SimplexHandler::simplex` takes `&[u8]`, not a decoded `p2p.Simplex`.** Go
   passes `*p2p.Simplex`. We take raw bytes to keep `ava-engine` decoupled from
   the generated proto type (the Simplex engine decodes them). If protocol parity
   later needs the decoded form, swap the param to
   `&ava_message::proto::p2p::Simplex` and add an `ava-message` dep. The op-set
   requirement (06 §4.1) is satisfied either way — the method exists.

3. **`NoOpHandler` is a concrete log-and-drop struct, not a default-method
   mixin.** Rust default trait methods cannot be selectively overridden across a
   trait *family* (an engine that implements `QueryHandler` but wants the
   state-sync no-op cannot mix defaults from two different traits without
   conflicts). So `NoOpHandler` is a zero-sized type implementing **all** op
   traits with `debug!`+`Ok(())`; an engine embeds it and delegates the op groups
   it does not handle. This mirrors Go's `noOp*Handler` structs (which are
   likewise embedded), just unified into one type.

4. **Bootstrap/query/fetch sends on `Sender` are infallible `fn` (no `Result`),
   matching Go**, which swallows enqueue errors and surfaces failures through the
   `*Failed` handler callbacks. Only the App sends (`send_app_*`) are `async` +
   fallible, matching `ava-vm`'s `AppSender`.

5. **Two distinct `Result`/error types.** `crate::error::Error` (the crate
   `thiserror` enum) is a *fatal* engine error returned by handler/sender methods
   (Go returns a non-nil `error`). `common::error::AppError` is the *application*
   error value carried inside a successful `AppRequestFailed`/`SendAppError`
   flow — matched by `i32` code via `AppError::is` (mirrors `(*AppError).Is`,
   which compares only `Code`). `AppError` intentionally matches the shape in
   `ava-vm/src/app.rs`.

6. **`Connector::{connected,disconnected}` return `ava_validators::Result`, not
   the `ava-engine` `Result`.** `InternalHandler` super-traits `Connector`
   (re-used from `ava-validators` per the plan); its own `gossip`/`shutdown`/
   `notify` return the `ava-engine` `Result`. This is a cross-crate `Result`
   split — callers of the `Connector` part see `ava_validators::Error`.

7. **`Notify` carries `ava_vm::VmEvent`** (the `common.Message` enum:
   `PendingTxs`/`StateSyncDone`), re-used from `ava-vm` rather than redefined,
   per the plan's "M2 `ava-message` ops / `VmToEngineMessage`" pointer — the
   actual VM→engine notification enum lives in `ava-vm` as `VmEvent`, not in
   `ava-message`. **Plan/spec correction:** spec 06 §4.1 sketches
   `Notify(msg: VmToEngineMessage)`; the realized type is `ava_vm::VmEvent`.

## TDD

- Red→green tests live in `src/lib.rs` `#[cfg(test)]`: `app_error_codes`,
  `handler_is_object_safe` (`fn _o(_: &dyn Handler){}` static-assert + boxed
  form), `noop_handler_drops_statesync`.

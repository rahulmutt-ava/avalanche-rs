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

## TDD (M3.9)

- Red→green tests live in `src/lib.rs` `#[cfg(test)]`: `app_error_codes`,
  `handler_is_object_safe` (`fn _o(_: &dyn Handler){}` static-assert + boxed
  form), `noop_handler_drops_statesync`.

---

# ava-engine — networking glue (Task M3.10)

Port of the `snow/networking/{router,handler,timeout,benchlist,tracker}`
subsystems into `crate::networking` (specs 06 §5.1–5.6, 24 §B.2).

Go reference (pinned `../avalanchego`):
- `snow/networking/router/chain_router.go` — `ChainRouter`.
- `snow/networking/handler/handler.go` + `message_queue.go` — the per-chain actor.
- `utils/timer/adaptive_timeout_manager.go` + `utils/math/continuous_averager.go`
  — `AdaptiveTimeoutManager` + the EWMA averager.
- `snow/networking/benchlist/benchlist.go` — `Benchlist`.
- `snow/networking/tracker/{tracker,targeter}.go` — `ResourceTracker`/`Targeter`.

## Module → Go mapping

| Rust (`networking::`) | Go |
|---|---|
| `timeout::AdaptiveTimeoutManager` + `ContinuousAverager` | `adaptiveTimeoutManager` + `continuousAverager` |
| `timeout::AdaptiveTimeoutConfig` + `verify` | `AdaptiveTimeoutConfig` + `NewAdaptiveTimeoutManager` switch |
| `router::ChainRouter` (`Router` trait) | `chainRouter` (`Router`) |
| `handler::ChainHandler` (one tokio task) + `EngineManager` | `handler` goroutine + `EngineManager` |
| `message_queue::{MessageQueue,MessageClass}` | `messageQueue` sync/async split |
| `benchlist::Benchlist` | `benchlist` |
| `tracker::{CumulativeTracker,Targeter}` | `resourceTracker` / `targeter` |

## Deliberate deviations / findings (record for spec/plan)

1. **The router operates on an engine-internal `InboundOp`/`InboundMessage`, not
   the `ava-message` wire type.** `ava-engine` deliberately carries no
   `ava-message`/proto dep (same stance as M3.9's `SimplexHandler: &[u8]`). The
   network layer (05) decodes + tags a message, then calls
   `Router::handle_inbound(InboundMessage{node, chain, op})`. The `*Failed`
   synthesis maps a numeric op tag → the matching `InboundOp::*Failed` variant
   (`router::op` constants). When the full sender/engine wiring lands (M3.11), the
   decode boundary moves into the `OutboundSender`/network adaptor; `InboundOp`
   stays the engine-facing projection.

2. **Timer fires over `tokio::time`, time read via `clock.monotonic()`** (specs
   24 §B.4). The Go manager uses a min-heap keyed by `time.Time` deadlines + a
   single `Timer`. We keep a `HashMap<RequestId, PendingTimeout>` and a single
   dispatch task that `sleep_until`s the earliest `tokio::time::Instant` deadline,
   woken by an unbounded `mpsc` on every `put`/`remove` to recompute. This honors
   `start_paused` + `tokio::time::advance` with no branching (the §B.2 test
   `deadline_fires_after_timeout` advances `MockClock` + tokio time in lock-step).

3. **Averager uses `Instant`-relative nanoseconds (float), not Go's
   `time.Time`.** Behavior-equivalent: `halflife = halflife_ns / ln2`,
   `weight = exp(-Δ/halflife)`. Float is explicitly acceptable here (06 §5.4 /
   24 §B.4: timeouts affect liveness only, never which block is accepted). No
   prometheus metrics ported (no registry in-crate yet) — `current_timeout` /
   `avg_latency` are internal state, asserted via `timeout_duration()`.

4. **Benchlist is the simpler consecutive-failure model the spec describes
   (06 §5.5), not Go's EWMA `failureProbability` averager.** Go's `benchlist.go`
   actually benches on an EWMA of success(0)/failure(1) crossing
   `benchProbability`, with a single consumer goroutine + event queue. The spec
   text says "consecutive request failures per peer; once a peer exceeds a failure
   threshold … benched … for a randomized cooldown" — we implement exactly that
   (threshold + reset-on-success + randomized `[min,max)` cooldown). The randomized
   duration is drawn from a **seedable gonum `Mt19937_64`** (off the consensus
   path) so tests are reproducible; Go uses `rand`. **Finding: spec 06 §5.5
   under-describes Go** — flag for a follow-up if exact Go bench parity is needed.

5. **`Benchlist` cooldown / `ResourceTracker` use `SystemTime`/`f64`.** Both are
   off the consensus path (06 §5.5/§5.6). The targeter mirrors Go
   `targeter.TargetUsage` exactly: `min(max(0, maxNonVdrUsage - totalUsage),
   maxNonVdrNodeUsage)` + `vdrAlloc * weight/totalWeight`. The single
   `cast_precision_loss` (weight ratio) is `#[allow]`ed at the site.

6. **The handler actor is one tokio task; async (`App*`) work runs on a bounded
   `JoinSet` tracked by a `tokio_util::task::TaskTracker`.** Shutdown cancels the
   `halt` `CancellationToken`, closes the tracker, `shutdown().await`s the pool,
   and `wait()`s the tracker — the tests (`handler.rs`) assert
   `tracker.is_empty()` after join (no leaked tasks). `ChainEngine` is a minimal
   object-safe dispatch trait (`handle`/`gossip`/`notify`); the full `Engine`/
   `Handler` op family (06 §4.1) composes onto it in M3.11. The `App*` body is a
   tracked placeholder until the VM `AppHandler` is wired (M3.11).
   `SYNC_PROCESSING_TIME_WARN_LIMIT = 30s` ported (Go `syncProcessingTimeWarnLimit`).

## Deps / features added (report for workspace promotion)

- `tokio` gains the `time` feature (already pinned with rt-multi-thread/macros/
  sync/net/time at workspace level — additive at the crate dep line for clarity).
- **`tokio-util` needs the `time` feature for `DelayQueue`/time utilities** — the
  workspace pins `tokio-util = ["rt"]`, so the crate dep line adds
  `features = ["time", "rt"]`. **Promotion candidate:** add `time` to the
  workspace `tokio-util` features if another crate needs it.
- dev-dep `assert_matches` (workspace) added for the config-verify sentinel
  assertions; dev `tokio` gains `test-util` for `start_paused`.

## TDD (M3.10)

- `tests/timeout.rs`: `deadline_fires_after_timeout` (§B.2 lock-step virtual
  time), `response_shortens_timeout`, `config_verify_rejections` (all four
  `verify` branches via `assert_matches!`).
- `tests/router.rs`: `router_routes_to_chain_handler` (route + unknown-chain
  drop), `timeout_synthesizes_failed` (register → timeout → `GetFailed`).
- `tests/handler.rs`: `handler_dispatches_and_shuts_down_clean` (sync dispatch +
  VM notify + gossip tick + clean halt, **no leaked tasks**),
  `async_op_runs_on_pool_and_drains`.
- Unit: `benchlist::tests` (threshold/cooldown/reset), `tracker::tests`
  (accumulation + stake-weighted target).

---

# ava-engine — Snowman engine (Task M3.11)

Port of `snow/engine/snowman/` (engine/issuer/voter) + `snow/consensus/snowman/poll/`
+ `snow/engine/snowman/getter/` into `crate::snowman` (specs 06 §4.2/§4.3).

Go reference (pinned `../avalanchego`):
- `snow/engine/snowman/engine.go` — the normal-op engine (Put/PullQuery/PushQuery/
  Chits/QueryFailed/Notify, issueFrom/deliver/sendChits/sendQuery/repoll/
  getProcessingAncestor).
- `snow/engine/snowman/{issuer,voter}.go` — the parked-job machinery.
- `snow/consensus/snowman/poll/{set,early_term_traversal}.go` — the poll set +
  early-termination predicate.
- `snow/engine/snowman/getter/getter.go` — the read-only `AllGetsServer`.

## Module → Go mapping

| Rust (`snowman::`) | Go |
|---|---|
| `engine::SnowmanEngine` + `Config` | `engine.Engine` + `engine.Config` |
| `poll::{PollSet,Poll,EarlyTermFactory}` | `poll.{set,earlyTermPoll,earlyTermTraversalFactory}` |
| `getter::Getter` | `getter.getter` (`common.AllGetsServer`) |
| `adaptor::BlockAdaptor` | the async-VM-block → sync-consensus-block bridge |
| `issuer` / `voter` (doc modules) | `issuer.go` / `voter.go` |

## Deliberate deviations / findings (record for spec/plan)

1. **Job scheduler folded into inline ancestry resolution.** Go parks
   `issuer`/`voter` jobs in a `job.Scheduler` keyed by block-id dependencies and
   drains them in `executeDeferredWork`. Because the engine task is single-owner
   and the VM/consensus calls are `await`ed inline here, `issue_from` walks the
   ancestry eagerly and chits are applied once their referenced blocks are issued.
   The observable behaviour (outbound messages, accept/reject order) is identical
   for the connected-ancestry cases the tests exercise. The `issuer`/`voter`
   modules are doc-only maps onto the inline `engine.rs` flow. **Finding:** if a
   later differential test needs the exact parked-job interleaving (e.g. an
   out-of-order multi-Put ancestry fill that abandons mid-chain), a real
   `JobScheduler<Id>` should be reintroduced.

2. **Early-term predicate is the per-id cases 1–4, not the full transitive-vote
   graph.** Go's `early_term_traversal.go` builds a transitive vote graph with
   shared-prefix bifurcations on every `Finished()` call to finish polls as early
   as theoretically possible. We require the engine to bubble each chit to the
   nearest *processing ancestor* before `PollSet::vote` (so the votes bag already
   holds ancestor ids), then apply Go's per-id `shouldTerminate` (cases 1–4)
   incrementally (O(1) per chit). This only changes *when* a poll completes, never
   the resulting decision (the safety argument of spec 06 §11). **Deferred:** the
   shared-prefix bifurcation short-circuit (can only ever finish a poll *earlier*).
   **Recommended spec note:** 06 §4.2/§11 should state the prefix-graph
   short-circuit is an optional optimization, not a correctness requirement.

3. **Two `Block` traits bridged by `BlockAdaptor`.** `ava-snow` exposes a
   synchronous consensus `Block` (`snowman::block::Block`, accept/reject are
   sync+fallible, matching Go's `RecordPoll` threading them directly) and an async
   engine-facing `Block` (`decidable::Block` = `ava_vm::Block`). The engine gets
   `Arc<dyn ava_vm::Block>` from the VM and must hand `Arc<dyn snowman::Block>` to
   `Consensus::add`. `BlockAdaptor` bridges them, driving the async accept/reject
   with `futures::executor::block_on` (a standalone executor — does **not**
   re-enter the engine's tokio runtime, so it works on the current-thread test
   runtime). **Sound** for VMs whose accept/reject perform no tokio-driven I/O
   (the in-memory test VM). **M3.14+ caveat:** production VMs that await tokio I/O
   inside accept must move acceptance off the synchronous `record_poll` path.

4. **`getProcessingAncestor` walks the consensus parent map**, not a separate
   `unverifiedIDToAncestor` tree. Since unverified blocks are not retained in a
   side cache in this port, the bubble walks `Consensus::get_parent` until it hits
   a processing block (or runs out). Equivalent for the issued-ancestry case;
   votes for never-issued descendants are dropped (same end state as Go's
   "ancestor isn't cached" drop).

5. **Getter limits copied from Go config defaults:** `MaxContainersLen =
   4*2MiB/5 = 1_677_721`, `maxContainersGetAncestors = 2000`,
   `bootstrap-max-time-get-ancestors = 50ms`. `get_ancestors` reuses the
   `ava_vm::block::get_ancestors` helper (batched-capability + local fallback).

## Deps / features added (report for workspace promotion)

- `futures` (workspace) added as a **non-dev** dep for `BlockAdaptor`'s
  `block_on` bridge.
- dev-deps: `ava-vm`/`ava-snow` with `testutil`, `ava-database`, `ava-version`,
  `proptest`, `sha2`, `tokio-util` for the integration + property harness
  (`tests/support/mod.rs`).

## TDD (M3.11)

- `src/snowman/poll.rs` unit: `early_term_case4`, `early_term_case2_unreachable`,
  `early_term_drop_global_unreachable`, `polls_drain_in_order`.
- `src/snowman/getter.rs` unit: `limit_constants`.
- `tests/engine_flows.rs`: `engine_requests_missing_block` (exactly one Get to
  the providing node for an unknown parent), `engine_records_poll_on_chits`
  (completed poll → record_poll → set_preference → child accepted),
  `early_term_completes_poll` (3/4 unanimous chits complete the poll early).

---

# ava-engine — Snowman bootstrapper + state-sync (Task M3.12)

Port of `snow/engine/snowman/bootstrap/` (bootstrapper + interval tree +
acceptor) and `snow/engine/snowman/syncer/` (no-op state-summary handlers) into
`crate::snowman::{bootstrap,syncer}` (specs 06 §4.3/§4.4).

Go reference (pinned `../avalanchego`):
- `bootstrap/bootstrapper.go` — frontier discovery → agreement → fetch → execute
  → handoff.
- `bootstrap/storage.go` (`process`/`execute`) — chain processing + height-order
  replay.
- `bootstrap/acceptor.go` — the `blockAcceptor` (consensus acceptor fires before
  the block's VM accept).
- `bootstrap/interval/{interval,tree,blocks}.go` — the interval tree.
- `syncer/state_syncer.go` — the state-sync engine (no-op path).

## Module → Go mapping

| Rust (`snowman::bootstrap::`) | Go |
|---|---|
| `Bootstrapper` + `Config` + `Phase` | `bootstrap.Bootstrapper` + `Config` |
| `interval::{Tree,Interval,Blocks,add_block}` | `interval.{Tree,Interval,Add}` + `PutBlock` |
| `acceptor::execute` | `storage.go::execute` + `acceptor.go::blockAcceptor.Accept` |
| `syncer::StateSyncer` | `syncer.stateSyncer` (no-op handlers + capability probe) |

## Deliberate deviations / findings (record for spec/plan)

1. **In-memory interval tree (no DB persistence).** Go's `interval.Tree` writes
   each interval to a `database.KeyValueWriterDeleter` so a restarted node
   resumes bootstrap. This port keeps the tree + fetched-block-bytes purely
   in-memory (`interval::{Tree,Blocks}`) — the engine drives a single
   uninterrupted pass. `Add`/`Remove`/`Contains`/`Flatten`/`Len` are faithful to
   the Go btree logic (keyed by `upper_bound`, merge/extend/split identical).
   The btree-custom-comparator is replaced by a `BTreeMap<upper_bound, Interval>`.

2. **Frontier agreement is a `> total_weight/2` quorum.** Go uses an `Alpha`
   weight majority from the config. The in-memory beacon model uses a simple
   strict-majority weight threshold (`Config::weight_threshold`). **Recommended
   spec note:** 06 §4.3 step 2 says "≥ a weight threshold of peers"; the exact
   threshold constant should be pulled from `Parameters`/config when the full
   beacon/StartupTracker wiring lands.

3. **Bootstrapper is driven by handler callbacks, not the Go StartupTracker /
   PeerTracker / ETA / batch-DB machinery.** The five-step state machine
   (`Phase` enum) + a round-robin fetch over the beacon set is the essential
   port; bandwidth sampling, ETA, genesis checkpoints, and DB batching are
   dropped. `on_finished` is folded into `finish()` (sets `EngineState::NormalOp`).
   **Finding:** the StartupTracker "should I start yet" gating and peer health
   sampling belong with the network/validator wiring (M3.10 / a later task).

4. **`acceptor::execute` toggles `ConsensusContext.executing`** for the replay
   duration and fires `ctx.block_acceptor.accept(...)` **before** `blk.accept()`
   (the §2.4 ordering invariant), replaying in ascending height order. The halt
   token is checked between blocks (the test `halt_aborts_bootstrap` cancels
   mid-stream and asserts no block is accepted + no handoff).

5. **State-sync is the no-op skeleton only (06 §4.4).** `syncer::StateSyncer`
   probes `StateSyncableVm::state_sync_enabled` (a non-syncable VM reports
   `false`) and drops the inbound `StateSummaryFrontier`/`AcceptedStateSummary`
   (+ `*Failed`) ops. The active out-of-band summary-fetch flow is **deferred**.

## TDD (M3.12)

- `src/snowman/bootstrap/interval.rs` unit: `add_merge_extend`,
  `contains_and_remove_interior`, `add_block_signals_parent_fetch`.
- `src/snowman/syncer.rs` unit: `state_sync_disabled_and_handlers_noop`.
- `tests/bootstrap.rs`: `bootstrap_fetches_and_executes_range` (frontier →
  agreement → fetch → height-ordered execute with acceptor-before-accept +
  executing flag → Bootstrapping→NormalOp handoff), `halt_aborts_bootstrap`
  (token cancel aborts the execute pass; no accepts, no handoff).

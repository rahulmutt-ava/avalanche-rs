# `ava-chains` — porting notes

Tracks deviations from the Go source (`chains/manager.go`, `chains/atomic`,
`vms/manager.go`, `vms/registry`, `ids.Aliaser`, `subnets`) so the central specs
(`specs/07-vm-framework.md`, `specs/00-overview-and-conventions.md`) can fold
them in. Nothing here changes observable behaviour the tests assert.

## M3.26 — VmManager / Factory / VmRegistry / Aliaser / Subnet / atomic SharedMemory

### `Factory` / `VmManager` (`src/manager.rs`)

- **Go `Factory.New(logging.Logger) (interface{}, error)`** → `Factory::new_vm()
  -> Result<Box<dyn Any + Send>>`. There is no `Logger` facade in the workspace
  yet (same deferral as `ava-vm`'s `FxVm::logger()`, M3.20), so `new_vm` takes no
  logger. Re-add it when a logging facade lands.
- **Version probing.** Go `manager.RegisterFactory` creates the VM, type-asserts
  it to `common.VM`, logs `Version()`, then `Shutdown()`s it. Rust cannot
  downcast a `Box<dyn Any>` to a *trait object*, so a probeable factory boxes its
  product as `manager::DynProbe(Box<dyn ProbeableVm>)`; the manager downcasts to
  `DynProbe` and probes. A factory whose product is not a `DynProbe` records the
  version as `"unknown"` (Go's same fallback string). `ProbeableVm` is the
  `{version, shutdown}` slice of the VM surface the manager needs — it avoids a
  dependency on a fully-initialized `ChainVm` just to read a version.
- **`versions()`** returns `primaryAlias -> version` exactly as Go
  (`manager.Versions`), resolving each VM id through the embedded `Aliaser`.

### `Aliaser` (`src/aliaser.rs`)

- Faithful port of `ids.Aliaser`: `dealias: alias→id`, `aliases: id→[alias]`
  (registration order; index 0 is the primary). `primary_alias_or_default`
  falls back to the id's CB58 string (Go `PrimaryAliasOrDefault`).

### `Subnet` (`src/subnet.rs`)

- `should_handle` is the exact `subnets.subnet.IsAllowed(nodeID, isValidator)`
  predicate: `node == me || !validator_only || is_validator ||
  allowed_nodes.contains(node)`. The bootstrapped-chains tracking
  (`AllBootstrapped`/`Bootstrapped`) is reduced to the `add_chain` set; the
  bootstrap-completion channel lands with the node-assembly milestone (M3.28+).

### atomic `SharedMemory` (`src/atomic/`)

- **Value encoding is byte-exact.** The `dbElement` (`{Present: bool, Value:
  []byte, Traits: [][]byte}`) is encoded with the linear codec layout + the
  2-byte version prefix (`atomic.CodecVersion = 0`), matching
  `chains/atomic/state.go`.
- **`sharedID`** = `sha256(version(2) ‖ min(id) ‖ max(id))`, reproducing
  `Codec.Marshal([2]ids.ID{min,max})` (a fixed-size array → no length prefix).
- **Value-DB key layout is byte-exact:** `JoinPrefixes(MakePrefix(sharedID),
  valuePrefix) ‖ key`, matching the `prefixdb` nesting Go builds
  (`GetSharedDatabase` → inbound/outbound value DB). Inbound/outbound prefix
  selection (smaller/larger swap) matches `prefixes.go`.
- **Index layout is NOT byte-exact (deviation).** Go stores the trait→key index
  as a `linkeddb`-encoded list per trait. We store it as flat keys
  `indexNs ‖ len(trait) ‖ trait ‖ key → ∅` and range-scan them in `Indexed`.
  This is observably identical for `Get`/`Indexed`/`Apply` but not on-disk
  compatible with a Go node's shared-memory DB. Cross-impl shared-memory interop
  is an **M9** concern (`specs/02`); revisit then if a Go-compatible on-disk
  index is required.
- **Atomicity.** Go opens a `versiondb` over the base, applies all value/index
  ops, then `WriteAll`s its `CommitBatch` together with the caller's side
  batches. Because `versiondb` requires a concrete `Database` and the shared base
  is an `Arc<dyn DynDatabase>`, we instead accumulate every value/index op AND
  the caller's side batches into a single base `Batch` and `write()` it once —
  the same single-atomic-write guarantee, without the intermediate overlay.
- **Per-channel locking** uses a `sharedID → Mutex` map locked in sorted order
  (Go `rcLock` + sorted `sharedIDs`), preventing apply/apply deadlocks.

## M3.27 — `create_snowman_chain` pipeline + `differential::testvm_finalizes`

### VM wrapping order (`src/create_chain.rs`)

- **Exact, ratified order (00 §11.1.2, cross-checked against Go
  `chains/manager.go::createSnowmanChain` lines ~1190-1235):**
  `inner → tracedvm(primaryAlias) → proposervm → metervm → tracedvm("proposervm")
   → change-notifier`, then `vm.initialize(...)`.
  Reproduced verbatim. `wrap_snowman_vm` builds the **maximal** stack (tracing +
  metering both enabled); the `WrappedVm<V, S>` type alias *is* the compile-time
  proof of the order and `pipeline_wrapping_order` walks it with `.inner()`.
- **Optional layers.** Go gates the two `tracedvm` layers on `TracingEnabled`
  and the `metervm` layer on `MeterVMEnabled`. Because dropping a layer changes
  the concrete Rust type and `Box<dyn ChainVm>` has no blanket `ChainVm` impl,
  the realized `wrap_snowman_vm` builds the maximal stack unconditionally (a
  `tracedvm`/`metervm` is cheap and faithful). Making the layers runtime-optional
  needs either a local `BoxedChainVm` newtype (delegating `ChainVm`) or per-flag
  monomorphized builders; deferred — flag-gating is a node-config concern (M3.28).
- **`ChangeNotifier`** (`block.ChangeNotifier`) is the outermost wrapper: fires
  `on_change` on `build_block` / `set_state` / a *changed* `set_preference`
  (lastPref tracking matches Go). It forwards every `ChainVm`/optional-capability
  method to the inner VM.

### DB stack (`build_db_stack`)

- Exactly `base → meterdb → prefixdb(chainID) → {prefix(VMDBPrefix="vm"),
  prefix(ChainBootstrappingDBPrefix="bs")}`. The Go bootstrapping prefix is
  `bs`; the VM prefix is `vm`. `PrefixDb<D: Database>` needs a concrete base, so
  `build_db_stack` (and `create_snowman_chain`) are generic over `D: Database`;
  the VM/bootstrapping DBs are returned type-erased as `Arc<dyn DynDatabase>`
  (what `Vm::initialize` and the bootstrapper consume).

### Sender / OutboundSender (simplification)

- Go builds an `OutboundSender` (+ optional `TracedSender`) that translates
  engine `Sender` calls into `proto`/network messages, registering outstanding
  requests with the timeout manager. The realized pipeline is generic over
  `Snd: Sender` (the caller supplies the concrete sender). A concrete
  `OutboundSender` over `ava-network` + the timeout-manager request registration
  is **deferred to M3.28** (node assembly), where the real network handles exist.
  The router-side request registration already lives on `ChainRouter`
  (M3.10); `create_snowman_chain` registers the chain's handler sink with the
  `Router` (which owns the `AdaptiveTimeoutManager`).

### Engine / handler wiring (simplification)

- `create_snowman_chain` builds the `Topological` consensus core + the
  `SnowmanEngine` over the fully-wrapped VM, creates the per-chain
  `ChainHandler` (engine-type Snowman), and registers its sink with the router.
  The `ChainHandler`'s `EngineManager` is created empty here: adapting the
  `SnowmanEngine`/`Bootstrapper`/`StateSyncer` to the handler's minimal
  `ChainEngine` (`handle`/`gossip`/`notify`) dispatch trait — so the actor loop
  drives them by `(EngineState, EngineType)` — is **deferred to M3.28**. The
  differential test therefore drives the `SnowmanEngine` directly (the same way
  `ava-engine`'s `prop::consensus_liveness` does), exercising the *full wrapped
  VM pipeline* rather than the handler actor's routing.

### `differential::testvm_finalizes` (`tests/differential_testvm.rs`)

- Boots an N-node in-memory cluster where **each node's VM is the maximal
  pipeline stack** (`wrap_snowman_vm` over `TestVm` + a `FixedState`
  `ValidatorState` + a generated staking identity + a `MockClock` at epoch 0),
  loops outbound queries back as inbound `pull_query`/`chits`, and asserts every
  node finalizes the *same* block at the *same* height (no fork) within the poll
  bound. The proposervm runs in its **pre-fork (passthrough)** regime (genesis
  timestamp = `UNIX_EPOCH`, before any ApricotPhase4 activation), so the test
  block flows through the wrappers unchanged.
- The cluster harness in `tests/support/mod.rs` is a local copy of the
  `ava-engine` cluster mechanics adapted to drive the wrapped VM (the engine
  harness is a test-only module, not importable across crates).

### Cross-crate change

- Added `pub fn inner(&self) -> &V` to `ava_proposervm::ProposerVm` (mirroring
  the existing `MeterVm::inner`/`TracedVm::inner`) so `pipeline_wrapping_order`
  can walk the stack. Non-behavioural accessor only.

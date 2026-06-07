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

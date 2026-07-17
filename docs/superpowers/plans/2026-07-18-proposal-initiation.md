# Production Block-Proposal Initiation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pending EVM tx on the live Rust node triggers `build_block` through the real engine path, and the proposervm windower computes proposer slots the Go net agrees with — unblocking the parent plan's live "Rust proposes" arm.

**Architecture:** A lock-free `PendingWorkWaiter` seam on the `Vm` trait (captured from the VM before it is wrapped in the consensus-shared `Arc<tokio::Mutex<dyn Vm>>`), a per-chain forwarder task that awaits the waiter and feeds `VmEvent::PendingTxs` into the existing `msg_from_vm` channel (no VM lock held), and a `GenesisValidatorState` that feeds the proposervm windower the real genesis 5-validator set instead of the `FixedState` self+beacons.

**Tech Stack:** Rust workspace (`ava-vm`, `ava-evm`, `ava-chains`, `ava-genesis`, `avalanchers`), tokio, `Arc<Notify>`. Go parity: `snow/engine/common/notifier.go` (`NotificationForwarder`), `vms/proposervm/proposer/windower.go`.

**Spec:** `docs/superpowers/specs/2026-07-18-proposal-initiation-design.md`

## Global Constraints

- License header on every new `.rs` file: `// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.` + `// See the file LICENSE for licensing terms.`
- No `unwrap()`/`expect()`/`dbg!`/`todo!` in library code (tests may); errors via `thiserror`.
- `// coreth`/`// Go <file>:<line>` citation on parity-mirroring logic. Import grouping std → external → crate; 4-space indent.
- Test runner: `./scripts/nix_run.sh cargo nextest run -p <crate>` (plain cargo/nextest is off PATH); `-j1` fallback on the known Firewood-lock cross-test flake.
- Each task ends with: scoped nextest green, `./scripts/nix_run.sh cargo clippy -p <crate> --all-targets -- -D warnings` clean, commit.
- Branch `m9.15-rust-proposer` (already checked out — nested insert #2; do NOT branch).
- The GREEN follower arm (`tests/differential`, `mixed_network`) and the whole existing workspace suite must stay green. The forwarder must NOT change any test that drives `vm_tx` manually (`boot_chain_with_loopback`, `engine_issuance.rs`).
- After any `ava-network`/`ava-evm`/`ava-vm` dep-crate source change, `touch crates/<dep>/src/lib.rs` before rebuilding `avalanchers`.

---

### Task 1: `PendingWorkWaiter` trait seam + `EvmVm` impl

**Files:**
- Modify: `crates/ava-vm/src/vm.rs` (new trait + `Vm` default method)
- Modify: `crates/ava-evm/src/vm.rs` (`EvmVm::pending_work_waiter` impl)
- Modify: `crates/ava-evm/src/mempool.rs` and `crates/ava-evm/src/atomic/mempool.rs` only if a `subscribe()`/`is_empty()` accessor is missing (both already expose `subscribe() -> Arc<Notify>` + `is_empty()` — verify, don't duplicate)
- Test: `crates/ava-evm/tests/pending_work_waiter.rs` (new)

**Interfaces:**
- Consumes: `EvmMempool::{subscribe() -> Arc<Notify>, is_empty()}` (`mempool.rs:280-284`), `AtomicMempool::{subscribe, is_empty}` (`atomic/mempool.rs:188,201`), `tokio::sync::Notify`.
- Produces (Task 2 consumes these EXACT names):
  - In `ava-vm`: `pub trait PendingWorkWaiter: Send + Sync { fn has_pending(&self) -> bool; async fn wait(&self); }` (use `async_trait`, matching the crate's other async traits) and a `Vm` trait default method `fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> { None }`.
  - In `ava-evm`: `EvmVm::pending_work_waiter` returns `Some` of an `EvmPendingWorkWaiter { atomic: Arc<Mutex<AtomicMempool>>, evm: Arc<Mutex<EvmMempool>> }` (holds the pool `Arc`s the VM already owns — NOT the outer VM mutex).

- [ ] **Step 1: Write the failing test**

`crates/ava-evm/tests/pending_work_waiter.rs`. Reuse the VM-construction harness from `crates/ava-evm/tests/tx_pipeline.rs` (copy its genesis/`EvmVm` setup helper — test-file convention is repeat-don't-import) plus its signed-tx helper. Assert real behavior:

```rust
#[tokio::test]
async fn waiter_fires_on_evm_pool_admission_without_vm_lock() {
    let vm = /* build EvmVm on the local genesis, funded EOA (tx_pipeline.rs setup) */;
    let waiter = vm.pending_work_waiter().expect("EvmVm exposes a waiter");
    assert!(!waiter.has_pending(), "empty pools => nothing pending");

    // Park a wait() on another task; it must resolve when a tx is admitted.
    let w2 = Arc::clone(&waiter);
    let parked = tokio::spawn(async move { w2.wait().await });
    // Admit a tx via the VM's public mempool handle (the #[doc(hidden)] accessor
    // tx_pipeline.rs already uses) — NOT via the outer Vm mutex.
    vm.evm_mempool_handle().lock().add_local(/* signed tx, sender, rules */).expect("admit");
    tokio::time::timeout(Duration::from_secs(5), parked).await
        .expect("wait() must resolve within 5s of admission").unwrap();
    assert!(waiter.has_pending(), "has_pending true after admission");
}

#[tokio::test]
async fn wait_returns_immediately_when_already_pending() {
    let vm = /* ... */;
    vm.evm_mempool_handle().lock().add_local(/* tx */).expect("admit");
    let waiter = vm.pending_work_waiter().unwrap();
    tokio::time::timeout(Duration::from_secs(1), waiter.wait()).await
        .expect("wait() returns at once when work already present");
}

#[tokio::test]
async fn no_lost_wake_between_check_and_wait() {
    // Admit AFTER cloning the Notify subscription but conceptually before wait():
    // the impl must register interest (subscribe) BEFORE checking emptiness, so a
    // tx admitted in that window still wakes. Drive it by admitting from a task
    // spawned immediately before the wait() call and asserting resolution.
    let vm = /* ... */;
    let waiter = vm.pending_work_waiter().unwrap();
    let h = vm.evm_mempool_handle();
    let admit = tokio::spawn(async move { h.lock().add_local(/* tx */).ok(); });
    tokio::time::timeout(Duration::from_secs(5), waiter.wait()).await
        .expect("no lost wake").ok();
    admit.await.ok();
}
```

If `EvmVm` lacks a public `evm_mempool_handle()`/`atomic pool` test accessor, add a `#[doc(hidden)] pub fn` mirroring the existing `mempool_handle` seam (`vm.rs:484`) — note it in the report.

- [ ] **Step 2: Run test to verify it fails**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm -E 'binary(pending_work_waiter)'`
Expected: FAIL to compile — `pending_work_waiter` / `PendingWorkWaiter` absent.

- [ ] **Step 3: Implement the trait + default**

In `crates/ava-vm/src/vm.rs` (beside the `Vm` trait; the crate already uses `async_trait` + `Arc`):

```rust
/// A lock-free signal that a VM has buildable work, so a forwarder can await
/// it WITHOUT holding the consensus-shared `Arc<Mutex<dyn Vm>>` (Go's model:
/// `snow/engine/common/notifier.go` calls `WaitForEvent` off the engine lock).
#[async_trait]
pub trait PendingWorkWaiter: Send + Sync {
    /// True iff the VM currently has work to build.
    fn has_pending(&self) -> bool;
    /// Resolves when the VM has (or gains) buildable work. Must register
    /// interest before checking emptiness so an admission racing the call is
    /// not lost.
    async fn wait(&self);
}
```

Add to the `Vm` trait, with a default so no other VM changes:

```rust
/// An optional lock-free waiter for a per-chain proposal forwarder. `None`
/// (default) means the VM has no admission-driven build trigger (P/X/SAE
/// today park until cancellation in `wait_for_event`).
fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> {
    None
}
```

- [ ] **Step 4: Implement `EvmVm::pending_work_waiter`**

In `crates/ava-evm/src/vm.rs`. Model `wait()` on the existing `EvmVm::wait_for_event` (`vm.rs:755-779`) — subscribe to BOTH pools' `Notify` **before** the emptiness check, then `select!` on the two `notified()` futures; return as soon as either pool is non-empty:

```rust
struct EvmPendingWorkWaiter {
    atomic: Arc<Mutex<AtomicMempool>>,
    evm: Arc<Mutex<EvmMempool>>,
}

#[async_trait]
impl PendingWorkWaiter for EvmPendingWorkWaiter {
    fn has_pending(&self) -> bool {
        !self.atomic.lock().is_empty() || !self.evm.lock().is_empty()
    }
    async fn wait(&self) {
        loop {
            let (an, en) = (self.atomic.lock().subscribe(), self.evm.lock().subscribe());
            if self.has_pending() { return; }
            tokio::select! {
                () = an.notified() => {}
                () = en.notified() => {}
            }
            if self.has_pending() { return; }
            // Spurious wake (e.g. an admission immediately drained): re-arm.
        }
    }
}

// on EvmVm:
fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> {
    Some(Arc::new(EvmPendingWorkWaiter {
        atomic: Arc::clone(&self.txpool),
        evm: Arc::clone(&self.evm_mempool),
    }))
}
```

(Adjust field names to the real ones — `self.txpool` is the `AtomicMempool`, `self.evm_mempool` the `EvmMempool`; confirm at `vm.rs`.)

- [ ] **Step 5: Run tests + full suite**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-evm` (retry `-j1` on flake)
Expected: the 3 new tests PASS; existing ava-evm suite (258+) green. Also `./scripts/nix_run.sh cargo nextest run -p ava-vm` green (default method compiles, no other VM affected).

- [ ] **Step 6: Clippy + commit**

```bash
./scripts/nix_run.sh cargo clippy -p ava-vm -p ava-evm --all-targets -- -D warnings
git add crates/ava-vm crates/ava-evm
git commit -m "feat(M9.15): PendingWorkWaiter seam — lock-free admission signal for a proposal forwarder (EvmVm impl)"
```

---

### Task 2: The forwarder task (production `NotificationForwarder`)

**Files:**
- Modify: `crates/ava-chains/src/create_chain.rs` (`create_snowman_chain` — capture the waiter from `inner_vm` before wrapping; spawn the forwarder after the handler is built)
- Test: `crates/avalanchers/tests/proposal_forwarder.rs` (new) — or extend `engine_issuance.rs` if its harness fits; prefer a new file to keep the manual-`vm_tx` test untouched

**Interfaces:**
- Consumes: `Vm::pending_work_waiter()` + `PendingWorkWaiter` (Task 1), the existing `vm_tx: mpsc::Sender<VmEvent>` and `token: CancellationToken` in `create_snowman_chain` (`create_chain.rs:752,689`), `VmEvent::PendingTxs`.
- Produces: no new public API — a spawned task. The observable contract (Task 2 test + parent Task 8): submitting a tx to the VM's mempool, with NO manual `vm_tx.send`, drives an accepted block.

- [ ] **Step 1: Capture the waiter before wrapping**

In `create_snowman_chain`, BEFORE `wrap_snowman_vm(inner_vm, ...)` moves `inner_vm` (`create_chain.rs:646`):

```rust
// Capture the lock-free proposal waiter from the inner VM before it is wrapped
// and moved behind the consensus-shared mutex. Go: snow/engine/common/notifier.go
// (NotificationForwarder) polls WaitForEvent off the engine lock; the shared
// `Arc<Mutex<dyn Vm>>` here forbids that, so we drive a lock-free waiter instead.
let pending_waiter = inner_vm.pending_work_waiter();
```

- [ ] **Step 2: Write the failing integration test**

`crates/avalanchers/tests/proposal_forwarder.rs`. Model boot on `engine_issuance.rs` BUT for the C-chain/EVM VM and WITHOUT the manual `vm_tx.send` — the forwarder must do it. (If `engine_issuance.rs` boots platformvm, adapt to the EVM boot helper used by `ava-evm`/`avalanchers` C-chain tests; if no in-`avalanchers` EVM boot helper exists, this test may instead live in `ava-evm` driving `EvmVm` + a minimal engine — implementer picks the lightest real harness and documents it.) Shape:

```rust
#[tokio::test]
async fn forwarder_drives_submitted_tx_to_accepted_block() {
    // Boot a C-chain (EvmVm) via the normal create_snowman_chain path (NOT
    // boot_chain_with_loopback's manual vm_tx), reach NormalOp.
    // Submit a tx directly to the VM's EVM mempool (no vm_tx.send).
    // Assert an accepted height-1 block appears (the forwarder woke the engine).
    // Bound with a timeout so a missing forwarder FAILS (RED) rather than hangs.
}
```

- [ ] **Step 3: Run to verify it fails (RED)**

Run: `./scripts/nix_run.sh cargo nextest run -p avalanchers -E 'binary(proposal_forwarder)'`
Expected: FAIL (times out / no block) — no forwarder spawned yet.

- [ ] **Step 4: Spawn the forwarder**

In `create_snowman_chain`, after the handler + `vm_tx` are built (`create_chain.rs:752-763`, before `Ok(SnowmanChain{..})`):

```rust
// Production NotificationForwarder (Go snow/engine/common/notifier.go:31-134):
// a pending-work signal becomes an engine PendingTxs build trigger. Holds NO VM
// lock (Task-1 waiter), so it never blocks verify/get/build on the shared mutex.
if let Some(waiter) = pending_waiter {
    let vm_tx = vm_tx.clone();
    let token = token.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = waiter.wait() => {}
                () = token.cancelled() => return,
            }
            // Signal once; spurious signals are harmless (engine build returns
            // NotFound when there is nothing to build — engine.rs:719-737).
            if vm_tx.send(VmEvent::PendingTxs).await.is_err() { return; }
            // Re-arm while work remains so a build rejected by the proposervm
            // windower ("not my slot yet") is retried when the next window opens
            // (Go's CheckForEvent re-arm). Bounded sleep, cancellation-aware.
            while waiter.has_pending() {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(2)) => {}
                    () = token.cancelled() => return,
                }
                if vm_tx.send(VmEvent::PendingTxs).await.is_err() { return; }
            }
        }
    });
}
```

- [ ] **Step 5: Run tests (GREEN) + regression**

Run: `./scripts/nix_run.sh cargo nextest run -p avalanchers` (retry `-j1`)
Expected: the new test PASSES; `engine_issuance.rs` and all `boot_chain_with_loopback` tests still green (the manual `vm_tx` path is unchanged — the forwarder is additive and only fires for VMs returning `Some` from `pending_work_waiter`, i.e. the EVM VM; platformvm/avm return `None` so their loopback tests see no extra sends). Also `./scripts/nix_run.sh cargo nextest run -p ava-chains` green.

- [ ] **Step 6: Clippy + commit**

```bash
./scripts/nix_run.sh cargo clippy -p ava-chains -p avalanchers --all-targets -- -D warnings
git add crates/ava-chains crates/avalanchers
git commit -m "feat(M9.15): per-chain proposal forwarder — pending work drives engine build_block off the VM lock (Go NotificationForwarder parity)"
```

---

### Task 3: `GenesisValidatorState` for the network proposervm windower

**Files:**
- Create: `crates/ava-genesis/src/validators.rs` (a `genesis_validator_set(config) -> Vec<GenesisValidatorEntry>` helper deriving NodeId + BLS key + weight from `initial_stakers` + the same allocation split `build.rs` stakes)
- Modify: `crates/ava-genesis/src/lib.rs` (module decl + re-export)
- Create: `crates/avalanchers/src/wiring/genesis_validator_state.rs` (the `ValidatorState` impl)
- Modify: `crates/avalanchers/src/wiring/chains.rs` (`boot_chain_with_sender` ~:1290 — replace `FixedState` with `GenesisValidatorState` on the NETWORK path; leave the loopback/self path's `FixedState` at :404 intact)

**Interfaces:**
- Consumes: `ava_genesis` config (`UNMODIFIED_LOCAL_CONFIG`, `Staker { node_id, signer: Option<ProofOfPossession> }`, `config.rs:66-75`), the `split_allocations` weight logic (`build.rs:448-482`), `ava_validators::{ValidatorState, GetValidatorOutput, PublicKey}` (`validator.rs:43`, `state.rs:31`).
- Produces:
  - `ava-genesis`: `pub struct GenesisValidatorEntry { pub node_id: NodeId, pub public_key: Option<PublicKey>, pub weight: u64 }` and `pub fn genesis_validator_set(config: &Config) -> Result<Vec<GenesisValidatorEntry>, GenesisError>`. (If `PublicKey` would create an `ava-genesis → ava-validators` dep that doesn't already exist, return the raw 48-byte compressed BLS bytes `pub public_key: Option<[u8; 48]>` instead and let the `avalanchers` adapter parse them — pick whichever keeps the dep graph clean and document it.)
  - `avalanchers`: `GenesisValidatorState { set: BTreeMap<NodeId, GetValidatorOutput> }` implementing `ValidatorState` with `get_current_height() -> 1`, `get_minimum_height() -> 0`, `get_validator_set(_, _) -> set.clone()` (same shape as `FixedState`, chains.rs:152-181), built from `genesis_validator_set`.

- [ ] **Step 1: Write the failing genesis-helper test**

In `crates/ava-genesis/src/validators.rs` `#[cfg(test)]`:

```rust
#[test]
fn genesis_validator_set_matches_local_genesis_stakers() {
    let cfg = ava_genesis::config::UNMODIFIED_LOCAL_CONFIG.clone(); // adjust to real accessor
    let set = genesis_validator_set(&cfg).expect("genesis validator set");
    // The local genesis has exactly 5 initial stakers.
    assert_eq!(set.len(), 5, "local genesis stakes 5 validators");
    // Every entry carries the staker's NodeID and a BLS public key.
    for e in &set {
        assert!(e.public_key.is_some(), "genesis staker {} has a BLS key", e.node_id);
        assert!(e.weight > 0, "genesis staker {} has nonzero weight", e.node_id);
    }
    // The NodeIDs equal the genesis initial_stakers' NodeIDs (order-independent).
    let got: std::collections::BTreeSet<_> = set.iter().map(|e| e.node_id).collect();
    let want: std::collections::BTreeSet<_> = cfg.initial_stakers.iter().map(|s| s.node_id).collect();
    assert_eq!(got, want, "node ids match genesis stakers");
    // Weight parity: each weight equals the sum this staker's allocations stake
    // (the build.rs split), NOT a flat 1.
    // ... assert the per-staker weight equals the split_allocations-derived stake ...
}
```

Read `build.rs:448-482` and reuse its exact `split_allocations` call so the weights are derived identically (the plan MUST NOT re-derive weights by hand — call the same helper `build.rs` uses; if it's private, make it `pub(crate)` and reuse it).

- [ ] **Step 2: Run to verify failure**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-genesis -E 'test(genesis_validator_set)'`
Expected: FAIL to compile (`genesis_validator_set` absent).

- [ ] **Step 3: Implement the genesis helper**

`crates/ava-genesis/src/validators.rs`: iterate `config.initial_stakers`, compute each staker's weight as the sum of its staked allocation amounts (the same `split_allocations(&skipped_allocations, n)` + per-node unlock-schedule sum that `build.rs:448-482` uses to build `PermissionlessValidator.staked`), parse the BLS public key from `staker.signer` (`ProofOfPossession.public_key`, 48 bytes). No floats, `checked_add` for the weight sum. Cite `// Go: genesis validator weights = staked amount (platformvm genesis)`.

- [ ] **Step 4: Write the failing `GenesisValidatorState` test + implement**

`crates/avalanchers/src/wiring/genesis_validator_state.rs` `#[cfg(test)]`: assert `get_validator_set(0, EMPTY)` and `get_validator_set(999, EMPTY)` both return the 5-entry set (height-invariant on a local net), `get_current_height() == 1`, `get_minimum_height() == 0`. Implement by building `set: BTreeMap<NodeId, GetValidatorOutput>` from `genesis_validator_set(config)` (parsing the 48-byte key into `PublicKey` here if the helper returned raw bytes). Mirror `FixedState`'s trait impl (chains.rs:152-181) for the other methods (`get_current_validator_set` → `(empty, 1)`, `get_warp_validator_sets` → empty, `get_subnet_id` → `EMPTY`).

- [ ] **Step 5: Wire into the network path**

In `boot_chain_with_sender` (`chains.rs:~1290`), replace `let validator_state = FixedState { set };` with `GenesisValidatorState` built from the node's genesis config. The genesis config is available via the boot spec / node config the network path already threads (find where `spec.network_id` → genesis is resolved; `ava_genesis` config for `network_id` is the source). Keep `validators` (`DefaultManager`) registration for the self + beacons as-is (that feeds connectedness/gossip, a separate concern), but the **windower's `ValidatorState`** becomes `GenesisValidatorState`. Leave the loopback/self path (`chains.rs:404`) on `FixedState` — loopback tests have no genesis staker set.

- [ ] **Step 6: Run tests + regression**

Run: `./scripts/nix_run.sh cargo nextest run -p ava-genesis -p avalanchers` (retry `-j1`)
Expected: new tests PASS; existing suites green. The follower-arm offline gates in `ava-differential` are unaffected (that arm uses its own harness), but verify `./scripts/nix_run.sh cargo build -p ava-differential --features live --tests` still compiles (the network wiring it calls changed).

- [ ] **Step 7: Clippy + commit**

```bash
./scripts/nix_run.sh cargo clippy -p ava-genesis -p avalanchers --all-targets -- -D warnings
git add crates/ava-genesis crates/avalanchers
git commit -m "feat(M9.15): GenesisValidatorState — proposervm windower uses the real genesis validator set on the network path"
```

---

### Task 4: Workspace gates + docs

**Files:**
- Modify: `plan/M9-interop-hardening.md` (note the proposal-initiation gap from the 2026-07-15 AS-BUILT is now closed offline: forwarder + genesis windower landed; the live proof is the parent plan's Task 8)
- Modify: `docs/superpowers/specs/2026-07-18-proposal-initiation-design.md` (mark the deferred `PChainValidatorManager` wiring + P/X/SAE forwarder opt-in as the remaining follow-ups if not already crisp)

- [ ] **Step 1: Full workspace verification**

```bash
./scripts/nix_run.sh cargo nextest run --workspace
./scripts/nix_run.sh cargo clippy --workspace --all-targets -- -D warnings
./scripts/nix_run.sh cargo fmt --check
```
Expected: ALL green. Fix anything before docs.

- [ ] **Step 2: Docs + commit**

Update the M9 plan note. Then:

```bash
git add plan docs/superpowers/specs
git commit -m "docs(M9.15): proposal-initiation offline-complete — forwarder + genesis windower; live proof deferred to parent Task 8"
```

- [ ] **Step 3: Hand back to the parent plan**

Report that the pipeline is offline-green; the controller resumes parent-plan Task 8 (Step 5 live run: oracle gate OK, touch+rebuild+verify-compile+PRE-WARM `avalanchers` release, re-run `mixed_network_rust_proposes`, then the follower arm for regression).

---

## Self-review notes (already applied)

- Spec coverage: Component 1 → Task 1; Component 2 → Task 2; Component 3 → Task 3; testing/live handoff → Task 4 + parent Task 8. Non-goals (PChainValidatorManager wiring, P/X/SAE opt-in, slot-wait-under-lock) explicitly deferred in Global Constraints + Task 4.
- Ordering: Task 1 (seam, self-contained) → Task 2 (forwarder, needs the seam) → Task 3 (windower state, independent of 1-2) → Task 4 (gates/docs). A reviewer can gate each independently.
- Type consistency: `PendingWorkWaiter::{has_pending, wait}` + `Vm::pending_work_waiter() -> Option<Arc<dyn PendingWorkWaiter>>` (T1) is exactly what T2 captures and spawns on; `GenesisValidatorState` mirrors the `FixedState` `ValidatorState` surface (T3) the windower already consumes.
- Deadlock avoidance is the load-bearing property: the waiter holds only the pools' own sync locks (never the outer `Arc<tokio::Mutex<dyn Vm>>`), and the forwarder task holds no lock at all — encoded in T1 Step 4 and T2 Step 4 with citations.

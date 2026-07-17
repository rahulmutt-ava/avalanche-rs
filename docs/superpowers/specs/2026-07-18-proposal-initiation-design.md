# Production Block-Proposal Initiation — Design

**Date:** 2026-07-18
**Status:** Approved (user, 2026-07-18)
**Context:** Nested insert #2 under the Rust-as-proposer plan
(`docs/superpowers/plans/2026-07-16-rust-as-proposer-cchain-parity.md`). Task 8's
live run 2 proved the Rust node boots as staker5, votes, and admits the driven tx
into its EVM mempool — but **never initiates a proposal**: `vm.wait_for_event()`
has no production caller (the `msg_from_vm` sender is a test-only handle), and the
live proposervm windower runs on a `FixedState` of self+beacons at weight 1
(`wiring/chains.rs:145-178,1250-1290`). Evidence:
`.superpowers/sdd/task-8-report.md` §"RESUMED", live log
`/private/tmp/claude-502/live-rust-proposer/rust_proposes_run2.log`.

## Goal

A pending EVM tx on the live Rust node triggers `build_block` through the real
engine path, the proposervm windower computes proposer slots the Go net agrees
with, and `mixed_network_rust_proposes` goes GREEN live — with the GREEN
follower arm (`mixed_network`) unregressed.

## Non-goals (explicit deferrals)

- **Full `PChainValidatorManager` node wiring.** The real P-chain-backed
  `ValidatorState` (`ava-platformvm/src/validators/manager.rs:468`) stays
  unwired; it becomes necessary only when validator sets change (Fuji/mainnet).
  The local net's set is static — the genesis-backed adapter below is exactly
  what Go's P-chain answers there.
- **P/X/SAE forwarder opt-in.** Their `wait_for_event` parks until cancellation
  (no admission wake); they keep the trait default (`None`) and today's
  behavior. Wiring them is a follow-up per VM.
- **The slot-wait-under-lock hazard.** `ProposerVm`'s `wait_for_slot_and_decide`
  runs inside `build_block` under the shared `Arc<tokio::Mutex<dyn Vm>>` —
  pre-existing, bounded by the slot duration, same family as the documented
  M7.18 SAE-adaptor limitation (`ava-saevm/adaptor/src/lib.rs:286-299`).
  Documented, not fixed here.

## Component 1 — lock-free pending-work waiter seam

**Why:** the VM is shared as `Arc<tokio::sync::Mutex<dyn Vm>>`
(`create_chain.rs:688`) with every consensus call (verify/get/build/preference —
full locker list in the task-8 exploration). `wait_for_event` parks; a forwarder
holding that lock while parked wedges the chain. Go has no outer mutex
(`snow/engine/common/notifier.go` calls `vm.WaitForEvent` freely; Go VMs manage
internal concurrency). The seam restores that shape.

**What:** a new optional method on the `Vm` trait (`crates/ava-vm`):

```rust
fn pending_work_waiter(&self) -> Option<Arc<dyn PendingWorkWaiter>> { None }
```

`PendingWorkWaiter` (same crate): `async fn wait(&self)` — resolves when the VM
has buildable work (must be race-free against admission: register interest
before checking emptiness, the `EvmVm::wait_for_event` pattern) — plus
`fn has_pending(&self) -> bool`.

`EvmVm` implements it over what already exists: its two pools'
(`AtomicMempool`, `EvmMempool`) `Arc<Notify>` subscribe handles + emptiness
checks, behind the pools' own cheap sync locks — never the outer VM mutex. The
waiter is captured at chain-creation time **before** the mutex wrap. All other
VMs inherit the `None` default — zero behavior change.

## Component 2 — the forwarder task

Go parity anchor: `common.NotificationForwarder`
(`~/avalanchego/snow/engine/common/notifier.go:31-134`), started at handler
start (`handler.go:254-255`).

Spawned per chain in `create_chain` when `pending_work_waiter()` is `Some`
(taken from the VM before wrapping):

```
loop {
    select { waiter.wait() | shutdown_token.cancelled() -> return }
    send vm_tx <- VmEvent::PendingTxs           // existing msg_from_vm channel
    while waiter.has_pending() {
        select { sleep(REARM_INTERVAL ~2s) | shutdown -> return }
        send vm_tx <- VmEvent::PendingTxs       // re-arm: covers "not my slot yet"
    }
}
```

- Spurious `PendingTxs` are already tolerated: the engine's build path returns
  `NotFound` harmlessly (`engine.rs:719-737`).
- The re-arm loop is the simpler equivalent of Go's `CheckForEvent` re-arm: a
  build attempt rejected by the windower ("not my slot") gets retried when the
  next proposer window opens.
- The task holds no locks — shutdown via the chain's existing cancellation
  token is trivially clean.
- The test-only `vm_tx` manual seam stays; `boot_chain_with_loopback` and
  existing tests are untouched.

## Component 3 — `GenesisValidatorState`

A small `ValidatorState` adapter built once at boot for the **network path
only** (replacing `FixedState` at `wiring/chains.rs:1250-1290`; `FixedState`
remains for loopback test seams):

- Source: the embedded genesis `initialStakers`
  (`ava-genesis/src/config.rs:27,149` — 5 NodeIDs + BLS keys) with the **exact
  weights the P-chain genesis builder stakes** — reuse the allocation-split
  logic in `ava-genesis/src/build.rs:448-482`; never re-derive weights by hand.
- `get_current_height() = 1`, `get_minimum_height() = 0`,
  `get_validator_set(any_height) = the genesis set` — exactly what Go's
  P-chain answers on a local net with no staking txs (same set at every
  height).
- Placement: beside `FixedState` in `avalanchers/src/wiring/` (keeps
  `ava-genesis` free of `ava-validators` types; the weight-derivation helper it
  calls may live in `ava-genesis` where the allocation logic already is).
- Side benefit: follower-side proposervm **verification** now consults the true
  validator set (strictly more correct than self+beacons-at-weight-1).

## What already works (verified during exploration — no work here)

- proposervm wrapping is unconditional in `create_chain` (`create_chain.rs:640-655`);
  signed-block building with the staking identity exists end-to-end
  (`ava-proposervm/src/vm.rs:367-390`: `wait_for_slot_and_decide` →
  `SignedBlock::build_signed` with the node's certificate + signer).
- The C-chain builder stamps full Go-shape headers (this branch's Tasks 2/4/5;
  coreth ACCEPTed the honest built block in Task 6). The M9 AS-BUILT note about
  `difficulty:0` Rust-built blocks is stale — fixed on this branch.

## Testing

- **Unit:** waiter semantics (fires on admission to either pool; `has_pending`
  truthful; no lost wake between check and wait). `GenesisValidatorState`
  golden-pins the exact 5-validator set + weights against the genesis builder's
  own output.
- **Integration:** an `engine_issuance.rs`-style test where the spawned
  forwarder — not a manual `vm_tx.send` — drives a submitted tx to an accepted
  block (submit → wake → build with zero manual sends).
- **Live (operator):** re-run `mixed_network_rust_proposes`; then the follower
  arm `mixed_network` for regression.

## Execution shape

Nested spec + plan committed on `m9.15-rust-proposer`, implemented via the same
subagent-driven flow, then parent Task 8's live run resumes.

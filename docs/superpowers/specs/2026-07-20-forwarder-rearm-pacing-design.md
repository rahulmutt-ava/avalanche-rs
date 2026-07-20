# Forwarder re-arm pacing (ACP-226 residual) — design

**Date:** 2026-07-20
**Status:** implemented
**Follow-up from:** builder min-delay pacing merge `56a95a3` (final-review Important
finding: unpaced 2 s re-arm under sustained load).

## Problem

The per-chain NotificationForwarder task in `ava-chains/src/create_chain.rs`
(step 7b, ~lines 777-807) signals the engine (`VmEvent::PendingTxs`) in two
ways: the outer loop awaits the paced `PendingWorkWaiter::wait()`, but the
inner re-arm loop (`while has_pending() { sleep(2s); send }`) bypasses pacing
entirely. Its 2 s cadence is phase-anchored to the *first* send, not to each
newly built parent, so under sustained tx load consecutive builds can still
fire before the ACP-226 minimum delay and die locally at `MinDelayNotMet` —
the exact cost the min-delay-pacing branch was built to remove. Coreth has no
such gap: its NotificationForwarder re-enters `WaitForEvent` (which re-paces
against the current header) before *every* signal
(`snow/engine/common/notifier.go`).

The forwarder is also an inline spawned closure today — the behavior under
fix has no direct unit test; coverage is only indirect (integration/live
arms).

## Design

### Shape: one paced loop (coreth parity)

Collapse the two loops into one, so every send — first and re-arm alike —
goes through the paced `wait()`:

```rust
loop {
    tokio::select! {
        () = waiter.wait() => {}
        () = token.cancelled() => return,
    }
    if vm_tx.send(VmEvent::PendingTxs).await.is_err() {
        return;
    }
    tokio::select! {
        () = tokio::time::sleep(FORWARDER_RETRY_FLOOR) => {}
        () = token.cancelled() => return,
    }
}
```

- `wait()` parking on an empty pool *is* the old outer-loop park; the
  `has_pending()` guard disappears from the forwarder (the method stays on
  the `PendingWorkWaiter` trait — SAE's txpool and the ava-evm waiter tests
  still use it).
- The post-send floor sleep is **load-bearing**: after a send that produces
  no block (proposervm windower "not my slot"), work still exists and pacing
  has already elapsed, so `wait()` would return instantly — without the floor
  the loop busy-spins sends. Named const:
  `FORWARDER_RETRY_FLOOR: Duration = Duration::from_secs(2)`.
- Effective cadence under sustained load: `max(floor, pacing)` — the floor
  when the parent's min delay has already passed, the paced target when it
  has not (delays can exceed 2 s under ACP-226 excess dynamics; building
  earlier would only die at `MinDelayNotMet`).
- Send backpressure unchanged: a full `vm_tx` parks the forwarder with no
  cancellation select around the send (same as today; channel depth 1024,
  engine drains).

### Extraction: a testable unit

The closure body becomes a private free function in `create_chain.rs`:

```rust
async fn forward_pending_work(
    waiter: Arc<dyn PendingWorkWaiter>,
    vm_tx: mpsc::Sender<VmEvent>,
    token: CancellationToken,
)
```

Spawn site: `tokio::spawn(forward_pending_work(waiter, vm_tx.clone(),
token.clone()))`. No new public API.

### Error handling

No new failure modes. Exits are the existing ones: cancellation at either
await, or channel-closed on send. Pacing's own failure modes (unresolvable
preferred id, pre-Granite parent) live inside `wait()` and are fail-open,
already reviewed on the min-delay-pacing branch.

## Testing

In-module `#[cfg(test)]` tests with a hand-rolled mock waiter (narrow local
mock): `wait()` gated on a `tokio::sync::Notify` released by the test, with a
release/call counter. `tokio::time::pause()` + manual `advance()` is safe
here — there is no `MockClock` interplay (the mock waiter's gating is
test-controlled), unlike the min-delay-pacing integration tests which had to
avoid paused time.

1. **Paced-send ordering (the regression):** no `PendingTxs` lands on the
   channel before the test releases `wait()`; one send per release.
2. **Retry floor:** after a send, the next send requires BOTH the floor to
   elapse and the next `wait()` release.
3. **Cancellation:** cancelling while parked in `wait()` and while in the
   floor sleep both terminate the task.
4. **Closed channel:** dropping the receiver terminates the task after the
   failed send.

System-level gate: rerun both live arms (`mixed_network`,
`mixed_network_rust_proposes`) with the oracle-gate + prewarm protocol.

## Docs

- `plan/M9-interop-hardening.md`: flip the min-delay AS-BUILT **Residual
  (follow-up)** sentence to closed, pointing at this change.
- `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md`
  AS-BUILT note: same flip for the re-arm clause (the plugin-path
  `wait_for_event` asymmetry note stays — out of scope here).
- This spec's Status → implemented at closeout.

## Out of scope

- Plugin-path `EvmVm::wait_for_event` pacing (latent, documented asymmetry).
- Porting coreth's `RetryDelay`(100 ms) arm — the 2 s floor keeps the
  established cadence; revisit only if the 20× coarser retry proves costly.
- Any change to `PendingWorkWaiter`'s trait surface, `EvmPendingWorkWaiter`,
  or `build_block`.

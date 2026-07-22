# Live real-quorum finalization — follow-up workstream (from T16 live debugging)

Status: OPEN (created 2026-07-22 at the close of the cchain-tx-gossip branch's T16).
Owner: next planning cycle. Evidence: this doc + `.superpowers/sdd/progress.md`
(T16 RUN rp4/rp5/11 entries) + `task-16-report.md`.

## What T16 proved and shipped

- The C-chain tx-gossip system (ava-p2p SDK, push/pull, EvmVm wiring) is
  correct: byte-exact Go-oracle wire goldens, offline two-node e2e, and LIVE
  bidirectional delivery proven on a 4-Go + 1-Rust validator net (runs 8/10:
  Go→Rust and Rust→Go pending-before-mined, measured push latency 96ms/54ms,
  same-millisecond admission).
- Two production bugs fixed on the way, each previously masked by the other
  (both landed, offline-tested, Go-parity-verified):
  1. `8571c0b` — networked chains ran a throwaway staking identity: the
     proposervm could never match its own windower slot; the Rust node was
     structurally unable to propose.
  2. `3f300ce` — networked chains ran `single_node_params()` (k=1 α=1 β=1):
     unilateral instant finality. Live-proven fork in 9ms (run rp4) the moment
     bug 1 was fixed. Includes two Go-parity sub-fixes real params require:
     `ava-validators` weight-unit sampling (k may exceed validator count,
     duplicates expected) and `ava-engine` poll vote-multiplicity
     (`bag.Of(vdrIDs...)` semantics, not stake-weighted votes).

## The consequence every prior live result must be re-read under

With k=1 + no-propose, every earlier live "consensus" green (including the
whole M9.15 mixed-network arc) was a degenerate zero-contention run: the Rust
node instantly self-finalized the only candidate block in town. Genuine
multi-validator participation had never been exercised live until run rp5.

## The gap (live-observed, run rp5 logs preserved)

With both fixes active on a 4-Go + 1-Rust net, staker5 (Rust):
1. **Never finalizes**: h1 stuck `processing` forever; 1219 polls issued;
   chit application `applied=true` 1965 vs `applied=false` 3848; beta(20)
   never approached. Suspects: chit→processing-block mapping (votes for ids
   the node hasn't issued / transitive vote application), poll completion
   semantics under duplicate-multiplicity, alpha accounting.
2. **Cannot build on a processing parent**: once-per-minute
   `WARN build_on failed with EVM candidates present; evicted the batch
   error="no state found for block 0x1afe…"` — the preferred (processing,
   unaccepted) parent's precommit state revision is absent when build_on
   resolves it, and the failure EVICTS the candidate batch (tx loss from the
   pool — separate sub-bug worth its own test).
3. Downstream of 1+2 the whole net stalls (Go can't take every height when
   the deterministic windower schedules staker5 first and staker5 can't
   build; and staker5's ~4/20 poll weight degrades Go's alpha margins).

## Acceptance gate (already written, red today)

`tests/differential/tests/mixed_network.rs::mixed_network_rust_proposes`
(20-attempt, index-API-verified) and `::mixed_network_tx_gossip` (burst
pending assertion) + `::mixed_network` follower leg — all three under REAL
params (the tree's default since `3f300ce`). No new tests needed to define
done; the workstream is done when these are green live.

## Where to start

- Rust engine vote application: `crates/ava-engine/src/snowman/engine.rs`
  (chits handling; the `applied=false` log line) vs Go
  `snow/engine/snowman/engine.go` + `snow/consensus/snowman/` vote/record
  paths — especially votes naming un-issued blocks (Go issues a `Get` and
  BLOCKS the poll on the fetch — check our parity there).
- Build-on-processing-parent state: `crates/ava-evm/src/vm.rs` `build_block`
  precommit-root resolution vs when verify/execution actually commits the
  revision; plus the batch-eviction-on-failure behavior in `build_on`.
- Preserved live logs: scratchpad `txg-rp5` (both stalls), `txg-rp4` (the
  k=1 fork, for contrast). Note scratchpad dirs are session-lifetime — copy
  anything needed into the repo's docs before relying on them long-term.

## Adjacent follow-up (final branch review, 2026-07-23)

- T7 trade-off, now load-bearing: `App*` ops are delivered INLINE on the
  per-chain consensus handler task (`ava-engine/src/networking/handler.rs`
  Async arm) with no processing-time warn — a slow VM AppHandler (e.g. the
  pull-answer marshal pass) head-of-line-blocks consensus. Fix alongside this
  workstream (extend the sync warn-limit to the Async arm, or land the real
  dispatch pool).

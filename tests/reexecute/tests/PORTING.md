# ava-reexecute — porting notes

Tracks the Go → Rust port of the **reexecute** suite (M9.19): replay a recorded
range of mainnet blocks through the Rust VMs from a fixed starting state and
assert the resulting state/merkle roots match the Go-recorded expected roots
byte-for-byte (specs/02 §10.5/§11.1 recorded-oracle mode, specs/16 §5(3),
specs/00 §11.7). Because the expected roots come from the Go node, this is a
*differential test on recorded data* — the cheapest per-PR oracle.

## Go source

- `tests/reexecute/{vm.go,c,blockexport,db.go}` (specs/02 §11.1) — replay a
  recorded block range through a VM and assert state. The Rust analogue is the
  [`ava_reexecute`] lib harness ([`ReexecuteCase`] + [`replay_cchain`]) driven by
  the integration targets under `tests/`.

## Layout

- `src/lib.rs` — the harness. [`ReexecuteCase`] deserializes a committed
  `blockexport`-style fixture; [`replay_cchain`] materializes the genesis alloc
  into a fresh Firewood-ethhash db, decodes the recorded block's EIP-2718 txs,
  drives `ExternalConsensusExecutor::execute_batch`, converts the returned
  `BundleState` into a Firewood proposal, and returns the genesis + post-state
  roots ([`ReexecuteRoots`]) for the caller to assert.
- `src/xchain.rs` — the X-Chain leg. [`replay_xchain`] drives the REAL `ava-avm`
  VM/block pipeline over a seed-derived synthetic case and returns the
  deterministic [`XchainReexecuteRoots`] (chain-tip block id + height + `sha256`
  post-state digest over the sorted final UTXO set).
- `tests/cchain_range.rs::reexecute_cchain_range` — the C-Chain leg. GREEN
  against the committed `genesis_to_1` fixture.
- `tests/px_range.rs::reexecute_px_range` — the P/X leg. GREEN (X-Chain
  determinism; see below). No longer `#[ignore]`d.
- `vectors/cchain/genesis_to_1/` — the committed fixture (`genesis_to_1.json` +
  `manifest.json`), copied from `crates/ava-evm/tests/vectors/cchain/reexecute/`
  (M6.6) so this crate is self-contained.

## C-Chain leg (`reexecute_cchain_range`) — DONE

Ports the M6.6 recorded-oracle logic from
`crates/ava-evm/tests/cchain_state_root.rs` into a reusable lib harness. The
`genesis_to_1` fixture was Go-EXECUTED against coreth (avalanchego @fb174e8,
go1.25.9, `TestApricotPhase3Config` ⇒ revm `LONDON`); roots are over the
STANDARD 4-field Ethereum account RLP matching the Firewood-ethhash backend. The
test asserts both the genesis root and the post-block-1 root equal the recorded
Go values. See the adjacent `vectors/.../manifest.json` for the M6.6 SPEC
FINDINGs (4-field vs 5-field account RLP; base-fee burn vs coinbase credit;
chainspec Paris activation).

## P/X leg (`reexecute_px_range`) — X-Chain DONE; P-Chain + Go-oracle DEFERRED

### What's PROVEN (X-Chain — `replay_xchain`, GREEN)

No Go-recorded mainnet P/X `blockexport`-style fixture exists in the repo, so —
**exactly as the C-Chain leg's `genesis_to_1` is a synthetic fixture run through
the REAL EVM pipeline** — the X-Chain sub-leg builds a synthetic-but-real
reexecute case: a seed-derived chain of `BaseTx` issuances driven through the
genuine `ava-avm` VM/block pipeline (`initialize` → seed genesis state → admit
txs → `build` → `set_preference` → `verify` → `accept`, one tx per block). This
is the SAME real-pipeline driver the `ava-differential` `xchain` collector
(M5.22) uses; ported into `src/xchain.rs` as lib code that propagates VM/codec
errors via `Error::Xchain` (no `unwrap`/`expect` in lib).

The X-Chain keys its UTXOs by id and maintains **no merkle state trie** (a
`StandardBlock`'s `MerkleRoot()` is always the zero id), so the reexecute "root"
is the deterministic **post-state digest**: `sha256` over the canonically-sorted
`(utxo_id ++ utxo_bytes)` of the final UTXO set, alongside the chain-tip block id
+ height (`XchainReexecuteRoots`).

**Property asserted:** the recorded-oracle property available WITHOUT a live Go
oracle — **determinism / reproducibility**. `reexecute_px_range` replays the same
synthetic case on two INDEPENDENT VM instances and asserts byte-identical roots
(specs/00 §6.1, specs/02 §11), checks the roots are non-trivial (height >= 1,
a real non-zero 32-byte sha256), and that a DIFFERENT case yields a DIFFERENT
root (so the assertion genuinely catches divergence — not a constant). This is
GENUINE VM execution, **NOT a fabricated/hardcoded root** — mirroring how the
`ava-differential` `xchain` collector proves determinism when no live oracle
exists.

### What's DEFERRED (and why)

1. **P-Chain sub-leg.** The P-Chain (`ava-platformvm`) reexecute sub-leg is not
   wired here. The X-Chain leg is shipped solidly first (one solid real-pipeline
   leg over two shallow ones, per the M9.19 P/X-leg plan). The same `replay_*`
   shape extends to P-Chain once a P-Chain seed-driven block-accept driver +
   post-state digest accessor are factored out (the platformvm state-diff/commit
   path).
2. **Go recorded-oracle parity.** We do NOT fabricate a "Go-recorded" root, so
   the parity arm (compare the computed root against a Go-EXECUTED P/X
   `blockexport` root) waits on a recorded Go P/X fixture — mirroring how
   `ava-differential` defers its absent-oracle live `interop` arm. When such a
   fixture lands, the X-Chain post-state-digest convention here is the comparison
   surface (or a Go-recorded `MerkleRoot` if the recorded blocks carry one). The
   reserved `px` cargo feature gates that future live arm.

## Dep choices

- `ava-evm` / `ava-evm-reth` / `ava-database` supply the reth `BlockExecutor` +
  Firewood-ethhash state-root pipeline + the `MemDb` K/V backend Firewood needs.
- `serde`/`serde_json`/`hex`/`tempfile`/`thiserror` are lib-only; the integration
  targets opt out of the per-binary `unused_crate_dependencies` false positive
  via `#![allow(unused_crate_dependencies)]` (the established repo idiom — see the
  `ava-differential` integration targets).

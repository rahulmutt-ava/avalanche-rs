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
- `tests/cchain_range.rs::reexecute_cchain_range` — the C-Chain leg. GREEN
  against the committed `genesis_to_1` fixture.
- `tests/px_range.rs::reexecute_px_range` — the P/X leg. `#[ignore]`d (see below).
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

## P/X leg (`reexecute_px_range`) — DEFERRED (M9.19 follow-up)

1. **No recorded P/X `blockexport` fixtures exist in the repo yet.** The P/X
   reexecute leg replays a recorded range of mainnet P-Chain / X-Chain blocks
   from a fixed start state and asserts each block's merkle root == the
   Go-recorded root. We do NOT fabricate roots, so `reexecute_px_range` is
   `#[ignore]`d (panics if forced) until a Go-recorded P/X `blockexport` fixture
   lands — mirroring how `ava-differential` defers its absent-oracle live
   `interop` arm. The harness's [`replay_cchain`] shape is the template; a
   `replay_px` fn + a P/X `ReexecuteCase`-equivalent are the follow-up. A `px`
   cargo feature is reserved on the crate for the live arm.

## Dep choices

- `ava-evm` / `ava-evm-reth` / `ava-database` supply the reth `BlockExecutor` +
  Firewood-ethhash state-root pipeline + the `MemDb` K/V backend Firewood needs.
- `serde`/`serde_json`/`hex`/`tempfile`/`thiserror` are lib-only; the integration
  targets opt out of the per-binary `unused_crate_dependencies` false positive
  via `#![allow(unused_crate_dependencies)]` (the established repo idiom — see the
  `ava-differential` integration targets).

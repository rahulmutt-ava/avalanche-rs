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
- `src/pchain.rs` — the P-Chain leg. [`replay_pchain`] drives the REAL
  `ava-platformvm` VM/block pipeline over a seed-derived genesis to its honestly
  reachable floor (`initialize` → `seed_state` → genesis block → `build_block`
  declines) and returns the deterministic [`PchainReexecuteRoots`] (chain-tip block
  id + height + `sha256` post-state digest over the sorted final UTXO set +
  Primary-Network supply + chain timestamp).
- `tests/cchain_range.rs::reexecute_cchain_range` — the C-Chain leg. GREEN
  against the committed `genesis_to_1` fixture.
- `tests/px_range.rs::reexecute_px_range` — the X-Chain determinism leg. GREEN
  (see below). No longer `#[ignore]`d.
- `tests/pchain_range.rs::reexecute_pchain_range` — the P-Chain determinism leg.
  GREEN (see below).
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

## P/X leg — X-Chain DONE; P-Chain DONE (genesis floor); Go-oracle DEFERRED

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

### What's PROVEN (P-Chain — `replay_pchain`, GREEN)

The P-Chain sub-leg now drives the REAL `ava-platformvm` VM/block pipeline over a
seed-derived synthetic genesis (two genesis UTXOs + one Primary-Network
permissionless validator, every field a pure function of the seed): `initialize`
→ `genesis::parse`/`seed_state` (genesis UTXOs, current staker, supply, timestamp)
→ genesis block → `build_block`. The genesis time + the validator period are
pinned FAR in the FUTURE (the X-Chain leg's clock-pinning trick), so
`builder::next_block_time` resolves to the fixed genesis time with no wall-clock
leak and no staker-change cap — and the builder honestly **declines**
(`ErrNoPendingBlocks`). The chain stays at the accepted genesis block (height 0).

The P-Chain keeps **FLAT KV state** (no merkledb — `state/`), so the reexecute
"root" is the deterministic **post-state digest**: `sha256` over the
canonically-sorted final UTXO set (enumerated by the seed-derived genesis owner
address via `State::utxo_ids`) + the Primary-Network current supply + the chain
timestamp, alongside the chain-tip block id + height
([`PchainReexecuteRoots`]). The post-state is read back via a minimal additive
read-only seam on `PlatformVm`, `with_state` (the P-Chain mirror of
`ava_avm::vm::AvmVm::with_state` — `#[doc(hidden)]`, acquires the block-manager
lock, runs the closure against the persisted `State`; the only change to
`ava-platformvm`). `reexecute_pchain_range` asserts byte-identical roots across two
INDEPENDENT VM instances (determinism, specs/00 §6.1), a non-zero 32-byte digest +
chain-tip id, `height == 0` (the honestly-reached floor), and that a DIFFERENT seed
yields a DIFFERENT root. GENUINE VM execution, **NOT a fabricated/hardcoded root**.

#### Why the floor is genesis (height 0), and what advances it

Two independent gaps block a height >= 1 accepted block today, and the leg refuses
to paper over either:

1. **The decision-tx mempool is un-shared on `PlatformVm`.** There is no public
   tx-admission seam (the X-Chain `AvmVm::mempool_add` analogue is absent;
   `vm.rs` ~line 166 documents "RPC issuance not yet wired"), so no decision tx
   (`CreateChainTx`/`CreateSubnetTx`/…) can be packed into a standard block. The
   M8 shared-mempool + gossip wiring lifts this.
2. **The genesis ⇄ staker-reward resolver wiring is incomplete.** The only
   height-advancing path that needs no decision tx is the reward-proposal block
   (`getNextStakerToReward` → `BanffProposalBlock`/`RewardValidatorTx`). But
   `genesis::seed_state` records the genesis validator as a current *staker*
   (`put_current_validator`) without storing its tx in the tx store, so the reward
   executor's `staker_tx_resolver` (`state.GetTx`,
   `block/executor/mod.rs:187`) returns `database: ErrNotFound` on verify.
   (Confirmed empirically: a past-pinned validator made `build_block` emit a
   reward block that failed verify with exactly that error.) This is the M4.24
   reward-wiring follow-up; patching production `seed_state` to store the tx was
   deliberately avoided (out of this test's scope, would change genesis behaviour).

Once **either** gap closes, the existing `replay_pchain` loop (already written
generally + `MAX_BLOCKS`-capped) advances height with **no change to the harness** —
only the future-pinned validator period (or the genesis time) needs revisiting to
drive the desired control flow.

### What's DEFERRED (and why)

1. **P-Chain height >= 1 accepted-block arm.** Blocked on the two gaps above
   (M8 shared mempool / M4.24 reward-resolver wiring), not on this harness.
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
- `ava-avm` supplies the X-Chain VM + tx/state/fx types the X-Chain leg drives.
- `ava-platformvm` supplies the P-Chain VM + genesis/tx/state/fx types the P-Chain
  leg drives; `ava-vm`/`ava-snow`/`ava-types`/`ava-version`/`ava-secp256k1fx`/
  `ava-crypto`/`async-trait`/`tokio`/`tokio-util` are the shared VM-framework /
  runtime deps both VM legs need (the sha256 helper is `ava-crypto`).
- `serde`/`serde_json`/`hex`/`tempfile`/`thiserror` are lib-only; the integration
  targets opt out of the per-binary `unused_crate_dependencies` false positive
  via `#![allow(unused_crate_dependencies)]` (the established repo idiom — see the
  `ava-differential` integration targets).

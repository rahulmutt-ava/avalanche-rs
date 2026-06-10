# SAE differentials — live Go oracles

This directory holds the **live Go oracles** for the avalanche-rs SAE
differentials. Each emitter is a `package sae` test dropped into the avalanchego
checkout (it needs the unexported `vms/saevm/sae` test harness — `newSUT`,
`rawVM`, `runConsensusLoop`, `canonicalBlock`, …) and gated behind an env var so
a normal `go test` never runs it:

| Emitter | Env gate | Rust test | Corpus |
|---------|----------|-----------|--------|
| `recovery_vector_emitter_test.go` | `SAE_EMIT_RECOVERY_VECTORS` | `sae_recovery.rs::differential::sae_recovery` (M7.29) | `tests/vectors/saevm/recovery_differential/` |
| `streaming_vector_emitter_test.go` | `SAE_EMIT_STREAMING_VECTORS` | `sae_streaming.rs::differential::sae_streaming` (M7.30) | `tests/vectors/saevm/streaming_differential/` |

> The two emitters redeclare a few shared helper names (`observe*Frontier`,
> `*HexBytes`) under distinct prefixes, but to be safe drop **one emitter at a
> time** into the checkout when re-freezing.

## Recovery emitter (M7.29)

`recovery_vector_emitter_test.go` is the **live Go oracle** for the avalanche-rs
M7.29 differential (`tests/differential/tests/sae_recovery.rs ::
differential::sae_recovery`).

It drives the real Go `vms/saevm` SAE node (`github.com/ava-labs/avalanchego`)
through a scripted block stream, crashes (snapshots the durable DB) at three
crash points, restarts via Go `recover()`, and writes a JSON `Observation`
corpus that the Rust differential replays against `ava_saevm_core::recover`.

The corpus records, per canonical height, the Go block's **wire bytes**
(RLP-encoded geth block) + the committed `ExecutionResults` (gas-time, base fee,
receipt/state roots), and the Go **source** (pre-crash) + **recovered**
(post-restart) A/E/S frontier observations.

## Provenance

This file is the source-of-truth copy. It is dropped into the avalanchego
checkout to run, because it needs the unexported `vms/saevm/sae` test harness
(`newSUT`, `rawVM`, `runConsensusLoop`, `canonicalBlock`, …). It contains **no
absolute paths**: the output directory is supplied at runtime via
`SAE_EMIT_RECOVERY_VECTORS`, and the avalanchego commit is read from
`$AVALANCHEGO_COMMIT` (else `"unknown"`).

The committed corpus was emitted from avalanchego @
`cc3b103b91173f5e8b89b1b31aea0816766c8ada` with Go 1.25.10.

## Re-freezing the corpus (live mode)

```sh
# AVALANCHEGO_DIR defaults to ../avalanchego relative to this repo root.
AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}

cp tests/differential/go-oracle/recovery_vector_emitter_test.go \
   "$AVALANCHEGO_DIR/vms/saevm/sae/"

cd "$AVALANCHEGO_DIR"
AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
SAE_EMIT_RECOVERY_VECTORS="$OLDPWD/tests/vectors/saevm/recovery_differential" \
  go test ./vms/saevm/sae/ -run TestEmitRecoveryVectors -count=1
```

Then re-run the Rust per-PR test to confirm parity:

```sh
cargo nextest run -p ava-differential -E 'test(sae_recovery)'
```

Without `SAE_EMIT_RECOVERY_VECTORS` set, `TestEmitRecoveryVectors` is skipped, so
the emitter never runs during a normal `go test`.

## Streaming emitter (M7.30)

`streaming_vector_emitter_test.go` is the **live Go oracle** for the M7.30
differential (`tests/differential/tests/sae_streaming.rs ::
differential::sae_streaming`).

It drives the real Go `vms/saevm` SAE node through three scripted block streams
and, **after every accepted block** (each `AwaitFinalization` barrier),
snapshots the A/E/S frontier observation plus that height's canonical block
**wire bytes** + committed `ExecutionResults`. The emitted corpus is a per-barrier
transcript (an ordered `barriers[]` array). The Rust differential drives its own
`Frontier` + `settle()` walk block-by-block over the same stream and asserts the
reconstructed S/E/A + settlement choice + roots match at **every** barrier index
— which validates the specs/00 §9 pipelined-commit optimization is observably
neutral.

The gas-time is emitted at **full precision** (`seconds` + fractional-second
`num`/`denom`): the per-barrier settlement boundary lands on a sub-second tie, so
the fraction is consensus-critical (unlike the recovery emitter, which needed
only whole seconds). All streams advance the wall clock by **whole seconds** per
block, so the test stub's sub-second block-time component stays zero — matching
both the Go production cchain hook (whole-second `BlockTime`) and the Rust
whole-second `Block::timestamp()` model.

### Re-freezing the corpus (live mode)

```sh
# AVALANCHEGO_DIR defaults to ../avalanchego relative to this repo root.
AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}

cp tests/differential/go-oracle/streaming_vector_emitter_test.go \
   "$AVALANCHEGO_DIR/vms/saevm/sae/"

cd "$AVALANCHEGO_DIR"
AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
SAE_EMIT_STREAMING_VECTORS="$OLDPWD/tests/vectors/saevm/streaming_differential" \
  go test ./vms/saevm/sae/ -run TestEmitStreamingVectors -count=1
```

Then re-run the Rust per-PR test to confirm parity:

```sh
cargo nextest run -p ava-differential -E 'test(sae_streaming)'
```

Without `SAE_EMIT_STREAMING_VECTORS` set, `TestEmitStreamingVectors` is skipped,
so the emitter never runs during a normal `go test`.

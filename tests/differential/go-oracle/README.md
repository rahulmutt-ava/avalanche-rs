# SAE recovery differential — live Go oracle

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

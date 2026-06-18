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
| `precompile_configkey_golden_emitter_test.go` | (none — plain `go test -run TestM631EmitGoldens`) | `ava-evm precompile_golden.rs` (M6.31) | `crates/ava-evm/tests/vectors/cchain/precompile/configkey_golden.json` |
| `precompile_selectors_emitter_test.go` + `precompile_nativeminter_selectors_emitter_test.go` | (none — plain `go test`) | constants pinned in `ava-evm src/precompile/{feemanager,rewardmanager,gaspricemanager,nativeminter}.rs` (M6.31) | (stdout only) |
| `atomic_tx_gas_emitter_test.go` | `AVAX_RS_EMIT_ATOMIC_GAS` | `ava-evm atomic_mempool.rs::gas_used_matches_coreth_oracle` (M6.29) | `gas_used` block in `crates/ava-evm/tests/vectors/cchain/atomic/atomic_txs.json` |

> The two SAE emitters redeclare a few shared helper names (`observe*Frontier`,
> `*HexBytes`) under distinct prefixes, but to be safe drop **one emitter at a
> time** into the checkout when re-freezing.
>
> The M6.31 precompile emitters are `package feemanager` /
> `package nativeminter` tests — drop them into
> `graft/subnet-evm/precompile/contracts/{feemanager,nativeminter}/` in the
> avalanchego checkout (they use the contracts' own exported `Pack*` ABI
> helpers, so the emitted bytes ARE the Go encoding).
>
> The M6.29 atomic-gas emitter is a `package atomic_test` test — drop it into
> `graft/coreth/plugin/evm/atomic/` in the avalanchego checkout and run
> `AVAX_RS_EMIT_ATOMIC_GAS=1 go test -run TestEmitAtomicTxGasUsed -v` from
> `graft/coreth/`. It parses the already-committed unsigned-tx interface bytes
> from `atomic_txs.json`, signs each with one key, and prints `GasUsed` for both
> `fixedFee` modes (frozen into the vector's `gas_used` block).

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

## Rust-plugin-in-Go-host live harness (M9.3 live arm)

`rust_plugin_handshake/main.go` is the **live two-binary arm** of M9.3
(`differential::plugin_rust_in_go`). Unlike the SAE emitters above (which emit
recorded corpora), this is a `package main` program that boots a real Go
`avalanchego` single-node `tmpnet`, creates a subnet + blockchain whose VM is the
Rust `testvm_plugin` rpcchainvm guest binary, and asserts the Go node spawns the
Rust plugin and completes the rpcchainvm **v45 reverse-dial handshake** (the
chain manager only reaches a successful "creating chain" for our VM once the
factory resolves, the plugin spawns + handshakes, and `Initialize` returns).

It is the source-of-truth copy; drop it into the avalanchego checkout to compile
against the `tests/fixture/tmpnet` fixture, then run:

```sh
# 1. build the Rust plugin (from the avalanche-rs repo root)
cargo build -p ava-vm-rpc --example testvm_plugin

# 2. copy the harness into the checkout and run it
AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}
mkdir -p "$AVALANCHEGO_DIR/tests/rustplugin"
cp tests/differential/go-oracle/rust_plugin_handshake/main.go \
   "$AVALANCHEGO_DIR/tests/rustplugin/main.go"

cd "$AVALANCHEGO_DIR"
# HOME override: tmpnet writes prometheus SD config under $HOME/.tmpnet; point it
# at a writable dir. The node inherits AVALANCHEGO_PLUGIN_DIR (set by the harness).
HOME=$(mktemp -d) \
AVALANCHEGO_PATH="$HOME/avalanchego/build/avalanchego" \
RUST_PLUGIN_PATH="$OLDPWD/target/debug/examples/testvm_plugin" \
  go run ./tests/rustplugin
```

Exit 0 + `PASS` = the Go host spawned the Rust plugin and the v45 handshake was
observed. This arm is nightly/manual only (it needs the live Go binary + a built
Rust plugin); the per-PR offline arm (`plugin_rust_in_go_builds_and_serves`)
black-box-drives the same plugin subprocess without a Go node.

### Gotchas (load-bearing — learned the hard way)

- **plugin-dir is env-only here.** avalanchego's `getPluginDir` only honors a
  config-file `plugin-dir` when `viper.IsSet("plugin-dir")` is true, which it is
  NOT for tmpnet's `--config-file` path — the node silently falls back to
  `$AVALANCHEGO_DATA_DIR/plugins`. The harness therefore sets
  `AVALANCHEGO_PLUGIN_DIR` (a viper env source that DOES set `IsSet`); the
  spawned node inherits it. Setting `ProcessRuntimeConfig.PluginDir` or
  `node.Flags["plugin-dir"]` is NOT sufficient.
- **PASS criterion counts, not greps.** The pre-restart bootstrap node logs a
  transient `error creating chain ... vmFactory ... not found` (it doesn't yet
  track the subnet); a naive grep for the VM id, "creating chain", or "rpcchainvm"
  false-PASSes. The harness compares successful vs errored "creating chain"
  counts for the VM id instead.

## Rust-plugin lifecycle live harness (M9.13 Go-host⇄Rust-guest leg)

`rust_plugin_lifecycle/main.go` is the live two-binary arm of the M9.13 four-way
wire-identity matrix's **Go-host⇄Rust-guest leg**. Where the M9.3 handshake
harness above proves only the v45 reverse-dial + first `VM.Initialize`, this one
proves the subsequent **build/verify/accept traffic** over the live channel.

It boots the same single-node Go node hosting the Rust `testvm_plugin`, but then
lets the chain reach NormalOp and drives a real `BuildBlock → VerifyBlock →
AcceptBlock` lifecycle: the Rust `FixedGenesisVm` returns `PendingTxs` from
`WaitForEvent` (bounded to 16 events) so the snowman engine's notifier triggers
`buildBlocks`, and a single-validator subnet immediately accepts each block. The
Rust guest emits a `TESTVM-EVENT build|verify|accept` marker to **stderr** on each
op; the node copies plugin stderr verbatim into the chain log
(`utils/logging.(*log).Write` bypasses the level filter and zap encoder), so the
harness greps those markers and PASSes once it has seen ≥1 build, ≥1 verify, and
≥1 accept.

```sh
# 1. build the Rust plugin (from the avalanche-rs repo root)
cargo build -p ava-vm-rpc --example testvm_plugin

# 2. copy the harness into the checkout and run it
AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}
mkdir -p "$AVALANCHEGO_DIR/tests/rustpluginlifecycle"
cp tests/differential/go-oracle/rust_plugin_lifecycle/main.go \
   "$AVALANCHEGO_DIR/tests/rustpluginlifecycle/main.go"

cd "$AVALANCHEGO_DIR"
# HOME override (tmpnet writes prometheus SD under $HOME/.tmpnet); preserve the
# Go module/build caches across the override so `go run` doesn't re-download or
# recompile, and pin the matching toolchain (go.mod pins 1.25.10).
HOME=$(mktemp -d) \
GOTOOLCHAIN=local \
PATH="$HOME_REAL/.local/share/mise/installs/go/1.25.10/bin:$PATH" \
GOPATH="$HOME_REAL/go" GOMODCACHE="$HOME_REAL/go/pkg/mod" \
GOCACHE="$HOME_REAL/Library/Caches/go-build" \
AVALANCHEGO_PATH="$HOME_REAL/avalanchego/build/avalanchego" \
RUST_PLUGIN_PATH="$OLDPWD/target/debug/examples/testvm_plugin" \
  go run ./tests/rustpluginlifecycle
```

(`$HOME_REAL` = your real home before the override.) Exit 0 + `PASS` = the Go host
drove the full build/verify/accept lifecycle against the Rust guest. Verified live
2026-06-18 (`build=15 verify=15 accept=15`). Nightly/manual only.

### Gotchas (in addition to the handshake harness's)

- **Plugin signals reach the harness via stderr → chain log, NOT an env var.**
  The plugin subprocess only inherits `GRPC_*`/`GODEBUG` from the node
  (`vms/rpcchainvm/runtime/subprocess/runtime.go` filters `os.Environ()`), so a
  custom `TESTVM_*` env var does NOT propagate. The reliable channel is the
  plugin's stderr, which the factory wires to the chain logger (`config.Stderr =
  log`), written verbatim.
- **Bound the build loop in the plugin, not the harness.** `WaitForEvent`
  returning `PendingTxs` unconditionally produces an unbounded build loop (tight
  CPU + huge logs). The `testvm_plugin` caps it at 16 events then long-polls
  (blocks until cancel) — the correct "no pending event" semantics.

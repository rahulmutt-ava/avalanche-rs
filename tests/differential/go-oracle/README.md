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
| `rust_built_block_verdict_test.go` | `RUST_BLOCK_VERDICT_DIR` (judge; the Rust side emits via `EMIT_PROPOSER_CANDIDATES`) | `ava-evm proposer_candidates.rs::proposer_verdicts_hold` (M9.15 Task 6) | `crates/ava-evm/tests/vectors/proposer_verdict/` |
| `base_fee_advance_emitter_test.go` | `BASE_FEE_ADVANCE_OUT` | `ava-evm feerules.rs::acp176_base_fee_advance_matches_go_vectors` (verifyHeaderGasFields-port Task 7) | `crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json` |
| `p2p_sdk_wire_emitter_test.go` | `P2P_SDK_EMIT_WIRE_GOLDENS` | `ava-p2p wire_goldens.rs` (cchain-tx-gossip Task 15) | `tests/vectors/p2p_sdk/*.bin` |

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
>
> The verifyHeaderGasFields-port Task 7 base-fee-advance emitter is a
> `package customheader` test — drop it into
> `graft/coreth/plugin/evm/customheader/` in the avalanchego checkout and run
> `BASE_FEE_ADVANCE_OUT=<abs path>/crates/ava-evm/tests/vectors/cchain/fees/acp176/base_fee_advance.json
> go test ./graft/coreth/plugin/evm/customheader/ -run TestEmitBaseFeeAdvanceVectors -count=1 -v` from
> the avalanchego repo root (per the file's own header comment). It needs no unexported test harness —
> only the package's own exported `BaseFee`, swept over a Cartesian (excess, elapsed-ms) grid under a
> Fortuna+Granite-active-at-genesis `ChainConfig`.

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

## Rust-built-block verdict judge (M9.15 Task 6)

`rust_built_block_verdict_test.go` is the **reverse shape** of the SAE emitters
above: instead of a Go emitter feeding a Rust reader, the **Rust side emits**
candidate C-Chain block RLPs and this Go test **judges** them — REAL coreth
code (`vm.ParseBlock` + `blk.Verify`) decides whether a Rust-**built** block is
accepted, closing the loop the M9.15 live differentials opened (which only
proved Go blocks parse/verify in Rust; this proves the reverse).

Two-step recording:

1. `ava-evm/tests/proposer_candidates.rs::emit_proposer_candidates`
   (env-gated on `EMIT_PROPOSER_CANDIDATES=<dir>`) builds the "honest"
   candidate — a real block the Task 2-5 `BlockBuilderDriver` produces on the
   committed `vectors/cchain/genesis/local.json` C-Chain genesis, carrying one
   signed EVM tx — plus five adversarial header mutations
   (`zero_difficulty`, `missing_cancun_tail`, `wrong_tx_root`, `bad_coinbase`,
   `nonzero_nonce`), and writes each as `<name>.rlp.hex` plus a copy of the
   genesis JSON into the output directory.
2. `rust_built_block_verdict_test.go` (env-gated on `RUST_BLOCK_VERDICT_DIR`,
   dropped into `graft/coreth/plugin/evm/` to run — it needs the package's own
   unexported `newDefaultTestVM` helper) boots a real coreth test VM
   (`vmtest.SetupTestVM`) over that SAME genesis JSON, `ParseBlock`s + `Verify`s
   each `*.rlp.hex` candidate, and writes `verdicts.json` (with
   `$AVALANCHEGO_COMMIT` provenance) back into the directory.

The per-PR reader (`proposer_verdicts_hold`) loads the committed
`verdicts.json`, asserts the honest verdict is `accepted == true`, and for each
adversarial candidate asserts BOTH the recorded Go verdict is a rejection
naming the expected sentinel AND that Rust's own `EvmVm::parse_block` →
`Block::verify` entry rejects the identical bytes with the matching sentinel —
Go and Rust reject the SAME candidate for the SAME reason (see
`proposer_candidates.rs::REJECTION_CLASSES` for the name -> (go, rust)
substring table, including the one asymmetric pair: `missing_cancun_tail` fails
at Go's wire decoder — a shorter RLP shape, not the semantic check — while
Rust's decoder tolerates it and rejects at the semantic `syntactic_verify`
check instead; both are correct rejections of the same malformed candidate).

### Re-recording (operator, live mode)

```sh
./scripts/check_oracle_binary.sh   # must print OK before recording

EMIT_PROPOSER_CANDIDATES=$PWD/crates/ava-evm/tests/vectors/proposer_verdict \
  cargo test -p ava-evm --test proposer_candidates -- --exact emit_proposer_candidates

cp tests/differential/go-oracle/rust_built_block_verdict_test.go \
   ~/avalanchego/graft/coreth/plugin/evm/

cd ~/avalanchego && AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
RUST_BLOCK_VERDICT_DIR=$OLDPWD/crates/ava-evm/tests/vectors/proposer_verdict \
  go test -run TestRustBuiltBlockVerdicts ./graft/coreth/plugin/evm/ -v -count=1 && \
  rm graft/coreth/plugin/evm/rust_built_block_verdict_test.go
```

Then re-run the Rust per-PR test to confirm parity:

```sh
cargo nextest run -p ava-evm -E 'test(proposer_verdicts_hold)'
```

Expected: `honest` `accepted=true`. If Go rejects the honest candidate, that IS
the differential working — the error names the failed check; M9.15 Task 6's
own recording session hit this twice against a completely honest builder and
fixed two real Go-shape gaps this way: the builder was not appending the
Durango+ empty-predicate-results suffix to `header.Extra`
(`builder.rs::EMPTY_BLOCK_PREDICATE_RESULTS`), and `feerules::gas_limit` had no
Fortuna+ (ACP-176 `MaxCapacity`) branch at all (it fell through to the stale
Cortina constant). Neither fix required implementing new consensus logic — both
were small, previously-incomplete Go-shape ports the live judge surfaced.

`go vet ./graft/coreth/plugin/evm/` is a fast standalone syntax/type check for
the copied file before running the real judge (it cannot compile as part of
the avalanche-rs CI — it needs the coreth package's own unexported test
helpers, so it lives only as this source-of-truth copy plus the note that
`go vet`/`go build ./graft/coreth/plugin/evm/...` is how to sanity-check it
after editing, from an avalanchego checkout).

Without `RUST_BLOCK_VERDICT_DIR` set, `TestRustBuiltBlockVerdicts` is skipped,
so the judge never runs during a normal `go test`.

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

## p2p SDK wire-frame goldens (cchain-tx-gossip Task 15)

`p2p_sdk_wire_emitter_test.go` is the **live Go oracle** for
`crates/ava-p2p/tests/wire_goldens.rs`. Unlike the SAE emitters above, it needs
no unexported test harness — it's a same-package (`network/p2p`) test that
calls only that package's own exported `ProtocolPrefix`/`PrefixMessage`, plus
`proto/pb/sdk` + `google.golang.org/protobuf/proto`.

For three fixed `sdk` messages (`PushGossip`, `PullGossipRequest`,
`PullGossipResponse`) it writes `PrefixMessage(ProtocolPrefix(0),
proto.Marshal(msg))` to its own `.bin` file — exactly the framing
`network/p2p.Network`'s gossip/request dispatch uses on the wire (mirrored by
the Rust `network::protocol_prefix`/`parse_prefix` pair). The Rust reader
builds the identical frame from the same fixed inputs and byte-compares
against the committed golden (encode leg), then parses + prost-decodes the
golden back and asserts the fields round-trip (decode leg). See
`tests/vectors/p2p_sdk/MANIFEST.md` for the fixed-input table and provenance.

### Re-freezing the corpus (live mode)

```sh
./scripts/check_oracle_binary.sh   # must print OK before capture

AVALANCHEGO_DIR=${AVALANCHEGO_DIR:-../avalanchego}
cp tests/differential/go-oracle/p2p_sdk_wire_emitter_test.go \
   "$AVALANCHEGO_DIR/network/p2p/"

cd "$AVALANCHEGO_DIR"
P2P_SDK_EMIT_WIRE_GOLDENS="$OLDPWD/tests/vectors/p2p_sdk" \
  go test ./network/p2p/ -run TestEmitP2pSdkWireGoldens -count=1 -v

rm network/p2p/p2p_sdk_wire_emitter_test.go
```

Then re-run the Rust per-PR test to confirm parity:

```sh
cargo nextest run -p ava-p2p --test wire_goldens
```

Without `P2P_SDK_EMIT_WIRE_GOLDENS` set, `TestEmitP2pSdkWireGoldens` is
skipped, so the emitter never runs during a normal `go test`.

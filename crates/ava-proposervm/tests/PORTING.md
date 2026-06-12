# ava-proposervm — PORTING.md

Provenance and parity notes for the Go → Rust port of `vms/proposervm`.

## Source (pinned)

All ports are against the pinned `../avalanchego` tree:

- `vms/proposervm/block/` — `block.go`, `codec.go`, `header.go`, `option.go`,
  `parse.go`, `build.go` → `src/block/`.
- `vms/proposervm/proposer/windower.go` → `src/proposer/windower.rs`.

## Golden vectors

### `tests/vectors/proposervm/blocks/blocks.json` (M3.21)

Produced by a scratch Go program (run in `/tmp`, **not** committed to this repo)
against `vms/proposervm/block`:

- `block.BuildOption(parent, inner)` → the `option` vector.
- `block.BuildUnsigned(parent, ts, pChainHeight, Epoch{}, inner)` →
  `post_fork_unsigned`.
- `staking.NewTLSCert()` + `block.Build(..., Epoch{}, cert, ..., chainID, key)`
  → `post_fork_signed` (a real Go-signed block; the cert + header bytes are
  captured so the Rust test re-verifies the signature via
  `staking::check_signature`).
- `block.Build(..., epoch{777,3,...}, cert, ...)` → `granite_signed`.
- `block.BuildUnsigned(..., epoch{777,3,...}, inner)` → `granite_unsigned`.

Each record captures the full serialized `bytes`, the Go `ID()`, `ParentID()`,
inner `Block()`, timestamp/pChainHeight, the proposer NodeID, the certificate
DER, and the `BuildHeader(...)` bytes.

The test (`golden_block.rs`) asserts:

1. **byte-exact re-encode** — `parse_without_verification(bytes).bytes() == bytes`;
2. **block-ID rule** — option `id == sha256(bytes)`; signed/Granite
   `id == sha256(bytes[.. len - 4 - len(sig)])` (strip the u32-length-prefixed
   signature suffix), bit-identical to Go's `hashing.ComputeHash256Array`;
3. **signature verification** — `parse(bytes, chainID)` runs `verify()` which
   builds `Header{chain, parent, id}` and calls `staking::check_signature` over
   `header.bytes()`; a Go-signed block passes, and the rebuilt header bytes
   equal Go's `BuildHeader(...)` output;
4. **zero-epoch rejection** — a Granite block with `Epoch{}` fails `verify()`
   with `Error::ZeroEpoch`.

### `tests/vectors/proposervm/windower/windower.json` (M3.22 — CONFIRMS R1)

Produced by a scratch Go program (run in `/tmp`) against
`vms/proposervm/proposer`, driving `proposer.New(state, subnetID, chainID, NoLog)`
over a fixed 5-validator set (plus an empty-NodeID validator that the windower
must drop). Captured:

- `chain_source` (BE `u64` of the chain id's first 8 bytes);
- `ExpectedProposer(height, pChainHeight=1, slot)` over heights
  `{0,1,2,100,12345,99999}` × slots `{0,1,2,5,50,719,720}` (42 cases);
- `Proposers(height, 1, maxWindows)` over the heights × `{1,6,60}` (18 cases);
- `Delay(height, 1, nodeID, 60)` for a few node ids incl. the empty NodeID
  (12 cases, as nanoseconds).

`golden_windower.rs` reproduces every ordering **bit-exactly** by driving the
pure-sync cores (`expected_proposer_from`/`proposers_from`/`delay_for`) and,
separately, the async `Windower` over a fixed `ValidatorState`. **R1 (gonum
MT19937/-64 compatibility) is CONFIRMED on the windower** — the vendored
`ava_utils::rng::{Mt19937, Mt19937_64}` + `WeightedWithoutReplacementGeneric`
match Go's `gonum prng` + `DeterministicWeightedWithoutReplacement`.

## Findings / deviations

- **Manual codec, not `#[derive(AvaCodec)]`.** The block bodies mix `Id`
  (32 raw bytes, no length prefix), `i64` (the timestamp — Go `int64`), and
  length-prefixed `Vec<u8>` (cert / inner / signature). `ava_types::Id` does not
  implement `ava_codec::Serializable`, and the derive does not cover `i64`, so
  the block (de)serialization is hand-written against `ava_codec::packer::Packer`
  (`src/block/stateless.rs`, `src/block/codec.rs`). The wire bytes are
  byte-identical to Go's linear codec (verified by the golden vectors). The
  registration order (`statelessBlock(0)`/`option(1)`/`statelessGraniteBlock(2)`)
  is encoded as the `u32` typeID in `codec.rs`.
- **`check_signature(cert, header.bytes(), sig)`** matches Go
  `staking.CheckSignature(cert, headerBytes, sig)`: `ring` hashes the message
  internally with SHA-256, exactly as Go signs `key.Sign(rand, sha256(headerBytes), crypto.SHA256)`.
- **Pre-fork blocks** (`src/block/pre_fork.rs`) are a thin pass-through of the
  inner-VM bytes/identity; the full fork-regime selection lands with the VM
  wrapper (M3.23).
- **Windower MT re-seeding (M3.22).** Go reuses one `prng` `source` object and
  calls `source.Seed(...)` before each `sampler.Sample(...)`. The Rust
  `WeightedWithoutReplacementGeneric` owns its `Box<dyn Source>` and exposes no
  re-seed, so the windower constructs a **fresh** sampler from a freshly-seeded
  MT per sample. This is bit-identical to Go: the weighted-heap `Initialize` is
  RNG-free and `Sample` resets the uniform, so only the seed determines the
  draw stream. Verified by `golden::windower_schedule` (no deviations).
- **Windower async surface.** Go's `Windower` methods take a `context.Context`;
  the Rust `Windower<S: ValidatorState>` methods are `async` (the
  `ValidatorState` lookup is async). The determinism-critical math is factored
  into pure-sync free functions (`*_from`) that the golden test drives directly
  — that is the actual R1 gate; the async wrappers only add the set fetch +
  empty-NodeID drop.

## VM wrapper (M3.23)

`src/vm.rs` (`ProposerVm<V: ChainVm, S: ValidatorState>`), `src/state.rs`,
`src/height_index.rs`, `tests/vm.rs`. Ports `vm.go`, `pre_fork_block.go`,
`post_fork_block.go`, `block.go::buildChild`, `batched_vm.go`,
`state_syncable_vm.go`, `height_indexed_vm.go`, `state/`.

### What landed

- **Fork-regime selection** by the preferred block's timestamp vs
  `UpgradeConfig`: pre-fork (bare inner block) / post-fork transition (child of a
  pre-fork block ⇒ always **unsigned**, no proposer, Go `preForkBlock.buildChild`)
  / post-fork pre-Durango (proposer windows via `Windower::delay`) / post-Durango
  (per-slot proposer via `Windower::expected_proposer`). Granite epoch wrapping is
  deferred (see below).
- **Height index** (`state.rs` + `height_index.rs`) over a `DynDatabase`: chain
  state (`lastAccepted`), block-by-id, `height -> blockID`, and the lazily-set
  fork height. `GetBlockIDAtHeight` serves heights `>= forkHeight` from the
  proposervm index and delegates `< forkHeight` to the inner VM.
- **Sign/build with slot wait**: `build_block` computes the child timestamp
  (`max(clock.unix_time(), parentTimestamp)`), waits for this node's slot via
  `Arc<dyn Clock>` + `tokio::time::sleep` (virtual-time-friendly, `start_paused`
  in tests; `maxSkew = 10s` constant carried as `vm::MAX_SKEW`), and on its slot
  signs the `Header` with the staking cert. Signing is pluggable via
  `StakingIdentity { certificate, signer: BlockSigner }` because `ava-crypto`
  exposes only cert *verification* (`check_signature`), not signing — the signer
  closure (ECDSA P-256 / RSA over `header.bytes()`, hashed internally by `ring`)
  is supplied by the caller, mirroring Go's `block.Build(..., key crypto.Signer)`.
  Added `block::SignedBlock::build_signed` / `block::GraniteBlock::build_signed`
  (Go `block.Build`) alongside the existing `build_unsigned`.
- **Inner-VM delegation**: `Vm`/`ChainVm` ops (`initialize`/`set_state`/
  `shutdown`/`version`/handlers/`wait_for_event`/`app_*`/`connected`/health/
  `parse_block`/`get_block`/`set_preference`/`last_accepted`) all delegate, and
  `as_batched`/`as_state_syncable` return `Some(self)` **iff** the inner VM
  implements them (then forward each method). The wrapper passes the generic
  `vm_conformance!` battery in the pre-fork regime.

### Findings / deviations / deferrals

- **`ProposerVm` is generic over `S: ValidatorState`** (owns `Windower<S>`),
  whereas Go reads `chainCtx.ValidatorState`. The Rust `ChainContext` (06 §3)
  intentionally has no `validator_state` handle, so it is injected at
  construction. Added `Windower::validator_state()` to let the VM read the
  recommended minimum P-Chain height (`selectChildPChainHeight`).
- **Error mapping is lossy across crate boundaries.** Neither `ava_vm::Error` nor
  `ava_snow::Error` exposes a free-form `Other(String)`, and this task may only
  edit `ava-proposervm`. So non-`NotFound` proposervm errors collapse onto
  `ava_vm::Error::InvalidComponent(&'static str)` (engine side) and
  `ava_snow::Error::ParametersInvalid(String)` (block accept/verify side, message
  preserved). `NotFound` round-trips exactly. **Recommended spec/plan follow-up:**
  add a generic `Other(String)`/`Internal(String)` variant to `ava_vm::Error`
  (and ideally `ava_snow::Error`) so middleware VMs can surface dynamic errors
  faithfully (Go uses `fmt.Errorf`/`errors.Join` freely here).
- **In-memory `verified` cache.** Go keeps freshly-built (verified, not yet
  accepted) post-fork blocks in `vm.verifiedBlocks`; the Rust wrapper keeps a
  `Shared.verified: HashMap<Id, Vec<u8>>` so `set_preference`/`get_block` resolve
  a block before it is accepted (the accept path then persists it). No sized-LRU
  inner-block cache (Go `innerBlkCache`, 64 MiB) yet — deferred.
- **Slot-wait modeled in `build_block`** (per the task), not in `WaitForEvent` as
  in Go (`WaitForEvent` here just forwards the inner VM's events). The wait polls
  the injected `Clock`; under `#[tokio::test(start_paused)]` a single-validator
  node resolves to a zero delay, so no wall-clock sleep occurs. A defensive
  short-circuit avoids a busy-spin if a non-advancing mock clock is used.
- **Deferrals** (recorded for later milestones): Granite epoch (ACP-181)
  selection on build (`acp181.NewEpoch`) — only zero-epoch `SignedBlock`s are
  built; oracle/option block wrapping (`postForkOption`); the full post-fork
  verify graph (parent timestamp monotonicity, proposer/pchain-height bounds,
  `verifyAndRecordInnerBlk` + the inner-block `tree.Tree`); height-repair
  (`repairAcceptedChainByHeight`) and pruning (`NumHistoricalBlocks`); state-sync
  *summary* re-wrapping (`buildStateSummary`/`summary.Build`) — the wrapper
  forwards `StateSyncableVm` to the inner VM verbatim rather than building
  proposervm summaries; `WithVerifyContext`/`BuildBlockWithContext` use; the
  Fuji P-Chain-height override (`vm.go:898` time-boxed
  `fujiOverridePChainHeight*` branch — skipped both in the build path and in
  the API's `proposed_height`).
- **API service (M8.22, Go `service.go` + `vm.go:255-311`).** `CreateHandlers`
  adds the gorilla-parity JSON-RPC mount at `/proposervm`
  (`getProposedHeight`/`getCurrentEpoch`, `json.Uint64` string replies);
  `NewHTTPHandler` composes the inner VM's header handler with the
  `proposervm.ProposerVM` Connect-unary mux on the 2-value
  `Avalanche-Api-Route` header. The Connect transport is hand-rolled over the
  buffered `VmHttpService` seam (`src/connect.rs`) with protojson semantics
  (camelCase, 64-bit ints as strings, zero fields omitted); messages come from
  `proto/proposervm/service.proto` via `build.rs` prost codegen. The API reads
  a `PreferredMeta` snapshot refreshed on `set_preference`/`initialize_wrapper`
  (blocks are immutable, so this equals Go's request-time
  `getBlock(preferred)`); clock + `GetMinimumHeight` are read live. Deferrals:
  gRPC reflection (`grpcreflect`, `vm.go:292`); the
  `metric.NewAPIInterceptor` HTTP metrics wrap (`vm.go:261`); the
  "routing request"/"API called" debug logs; option blocks as the preferred
  block (unsupported crate-wide) surface as "not found".

## Status

| Go file | Rust | Status |
|---------|------|--------|
| `block/codec.go` | `src/block/codec.rs` | done |
| `block/block.go` | `src/block/{stateless,post_fork}.rs` | done |
| `block/header.go` + `BuildHeader` | `src/block/header.rs` | done |
| `block/option.go` + `BuildOption` | `src/block/option.rs` | done |
| `block/parse.go` | `src/block/codec.rs` (`parse`/`parse_without_verification`) | done |
| `block/build.go` | `src/block/{post_fork,option,header}.rs` | done (signed build added M3.23) |
| `proposer/windower.go` | `src/proposer/windower.rs` | done (M3.22) |
| `vm.go` | `src/vm.rs` | core done (M3.23); deferrals above |
| `pre_fork_block.go` / `post_fork_block.go` / `block.go::buildChild` | `src/vm.rs` (`build_child`) | core done (M3.23) |
| `state/` | `src/state.rs` | done (M3.23) |
| `height_indexed_vm.go` | `src/height_index.rs` | done (no pruning) (M3.23) |
| `service.go` (`jsonrpcService` + `connectrpcService`) | `src/service.rs` + `src/connect.rs` | done (M8.22; no metrics interceptor / reflection) |
| `vm.go:255-311` (`CreateHandlers`/`NewHTTPHandler`) | `src/vm.rs` | done (M8.22) |
| `acp181/epoch.go` | `src/acp181.rs` | done (M8.22; used by the API only — build-path epoch still deferred) |
| `connectproto/proposervm/service.proto` | `proto/proposervm/service.proto` + `src/pb.rs` | done (M8.22) |
| `batched_vm.go` / `state_syncable_vm.go` | `src/vm.rs` (delegation) | delegate-to-inner (M3.23) |

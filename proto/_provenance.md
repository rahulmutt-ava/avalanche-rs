# Proto provenance

The `.proto` sources under `proto/` are **copied verbatim** from the
`avalanchego` Go tree — they are the shared, byte-exact wire contract so a Rust
plugin/host interoperates with a Go host/plugin (overview §1 gRPC-plugin
requirement; `specs/15-serialization-and-wire-formats.md` §2, §3).

| Field | Value |
|-------|-------|
| Source repo | `github.com/ava-labs/avalanchego` |
| Source rev | `fb174e8925` |
| Source path | `proto/` (e.g. `proto/rpcdb/rpcdb.proto`) |

## Codegen (not committed)

Rust bindings are generated at build time via `tonic-build` / `prost-build`
inside each consuming crate's `build.rs`, into `OUT_DIR` — **never committed**
(`specs/01-development-environment.md` §8.1, `00` decision 8). Consumers reach
the generated types via `tonic::include_proto!("<pkg>")` or
`include!(concat!(env!("OUT_DIR"), "/<pkg>.rs"))`. `protoc` and `buf` come from
the Nix dev shell. Bazel uses `rust_prost_library` for the hermetic path.

## Files present

- `rpcdb/rpcdb.proto` — `Database` over gRPC (`specs/04` §2.8, `15` §3.4).
  Consumed by `crates/ava-database` (M1.11). Imports only
  `google/protobuf/empty.proto` (a well-known type `protoc` resolves
  automatically).
- `sync/sync.proto` — merkledb state-sync range/change-proof request/response
  wire (`specs/04` §3.7, `19` §4, `15` §3.10). Consumed by `crates/ava-merkledb`
  (M1.19). No external imports.
- `vm/vm.proto` — the `VM` service (`block.ChainVM` + batched/statesync/
  withcontext RPCs, `specs/07` §5.4). Consumed by `crates/ava-vm-rpc` (M3.24).
  Imports `google/protobuf/{duration,empty,timestamp}.proto` (well-known types)
  and `io/prometheus/client/metrics.proto` (the `Gather` RPC).
- `vm/runtime/runtime.proto` — the `Runtime` service (the v45 reverse-dial
  handshake `Initialize`, `specs/07` §5.1). Consumed by `crates/ava-vm-rpc`
  (M3.24). Imports `google/protobuf/empty.proto`.
- `io/prometheus/client/metrics.proto` — the Prometheus client-model
  (`buf.build/prometheus/client-model`), vendored here because the Go tree
  resolves it as a `buf` dependency (its `buf.yaml` excludes `io/prometheus`
  from generate, noting "this proto file is required by languages such as
  rust"). proto2 syntax; `protoc`/`prost` handle it. Only the `MetricFamily`
  message is referenced (by `vm.proto`'s `GatherResponse`). Vendored from
  `github.com/prometheus/client_model@v0.6.2`.
- `appsender/appsender.proto` — the `AppSender` callback service (`specs/07`
  §2.6, §5.4). Consumed by `crates/ava-vm-rpc` (M3.25). Imports
  `google/protobuf/empty.proto`.
- `sharedmemory/sharedmemory.proto` — the `SharedMemory` callback service
  (`specs/07` §3.1, §5.4). Consumed by `crates/ava-vm-rpc` (M3.25). No external
  imports.
- `validatorstate/validator_state.proto` — the `ValidatorState` callback
  service (`specs/06` §6.1, `07` §5.4). Consumed by `crates/ava-vm-rpc`
  (M3.25). Imports `google/protobuf/empty.proto`.
- `warp/message.proto` — the warp `Signer` callback service (`specs/07` §5.4).
  Consumed by `crates/ava-vm-rpc` (M3.25). No external imports.
- `aliasreader/aliasreader.proto` — the `AliasReader` (`bc_lookup`) callback
  service (`specs/07` §5.4). Consumed by `crates/ava-vm-rpc` (M3.25). No
  external imports.

More `.proto` files (`p2p`, `messenger`, `http`, `net`, `io`, …) land here as
their consuming crates do (one per backend/wave), each recorded against the same
Go rev.

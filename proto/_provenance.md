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

More `.proto` files (`sync`, `p2p`, `vm`, `appsender`, `sharedmemory`, …) land
here as their consuming crates do (one per backend/wave), each recorded against
the same Go rev.

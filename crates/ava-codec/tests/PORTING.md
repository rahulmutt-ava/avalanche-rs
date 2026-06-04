# PORTING.md — `ava-codec` (+ `ava-codec-derive`)

Parity against avalanchego `codec/`, `codec/reflectcodec/`,
`codec/linearcodec/`, `utils/wrappers/` (Packer). One row per upstream Go test;
status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25 exit gate.
See `specs/02-testing-strategy.md` §10.1.

Owning tasks: M0.14 (Packer), M0.15 (derive + traits), M0.16 (Manager +
linearcodec + codectest).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `utils/wrappers/packing_test.go` | `tests/packer.rs` | todo |
| `codec/reflectcodec/type_codec_test.go` | `tests/derive.rs` | todo |
| `codec/codec_test.go` | `tests/golden_codec.rs` | todo |
| `codec/linearcodec/codec_test.go` | `tests/golden_codec.rs` | todo |
| `codec/test_codec.go` (RunAll) | `tests/conformance.rs` | todo |

# PORTING.md — `ava-types`

Tracks parity of this crate against its avalanchego source packages
(`ids/`, `utils/constants/`). One row per upstream Go test; status is one of
`todo` / `wip` / `ported` / `na`. The milestone exit gate (M0.25) requires no
`wip` rows for shipped surfaces. See `specs/02-testing-strategy.md` §10.1.

Owning tasks: M0.5 (ids + bits + error), M0.6 (CB58 string forms),
M0.7 (RequestId + Aliaser), M0.8 (constants).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `ids/id_test.go` | `tests/id_ops.rs` | todo |
| `ids/bits_test.go` | `tests/id_ops.rs::bits` | todo |
| `ids/id_test.go` (CB58 string) | `tests/golden_cb58.rs` | todo |
| `ids/node_id_test.go` | `tests/golden_cb58.rs` | todo |
| `ids/aliases_test.go` | `tests/aliaser.rs` | todo |
| `utils/constants/network_ids_test.go` | `tests/constants.rs` | todo |

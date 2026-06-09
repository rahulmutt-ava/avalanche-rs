# `ava-saevm` — Go → Rust porting matrix

Tracks coverage of the Go `vms/saevm` reference tree (spec 11 §0 "Go source
covered"). Rows are seeded from reading the `*_test.go` files in the
avalanchego reference tree at `../avalanchego/vms/saevm/{sae,cchain,blocks,
saexec,saedb,adaptor,hook,gastime,gasprice,proxytime,txgossip,intmath,cmputils,
worstcase,params,saetest}` (the tree is read-only; it is never modified here).

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable · wip in-progress

**Summary:** 0 ported ✅ / 0 partial 🟡 / 0 not-ported ⬜ / 0 n/a — M7.1 lands the
sub-workspace scaffold only; rows are added per task as each crate is implemented.

---

## params / intmath / cmputils (M7.2–M7.4)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.2–M7.4)_ | ⬜ | — |

## proxytime / gastime / gasprice (M7.5–M7.7)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.5–M7.7)_ | ⬜ | — |

## types / hook / adaptor / blocks (M7.8–M7.11)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.8–M7.11)_ | ⬜ | — |

## saedb / worstcase / saexec (M7.12–M7.16)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.12–M7.16)_ | ⬜ | — |

## sae core / txgossip (M7.17–M7.20)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.17–M7.20)_ | ⬜ | — |

## cchain (M7.21–M7.23)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.21–M7.23)_ | ⬜ | — |

## recovery / invariants / differentials (M7.24–M7.32)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| _(seeded in M7.24–M7.30, M7.32)_ | ⬜ | — |
| `blocks/parse_block` fuzz (no Go counterpart) | ✅ | `crates/ava-saevm/blocks/fuzz/fuzz_targets/decode_block.rs` (nightly cargo-fuzz) + `blocks/tests/parse_block_fuzz_smoke.rs` (stable proptest, M7.31) |

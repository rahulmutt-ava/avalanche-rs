# PORTING.md — `ava-codec` (+ `ava-codec-derive`)

Parity against avalanchego `codec/`, `codec/reflectcodec/`,
`codec/linearcodec/`, `utils/wrappers/` (Packer). One row per upstream Go test;
status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25 exit gate.
See `specs/02-testing-strategy.md` §10.1.

Owning tasks: M0.14 (Packer), M0.15 (derive + traits), M0.16 (Manager +
linearcodec + codectest), M0.24 (proptests + fuzz).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `utils/wrappers/packing_test.go` | `tests/packer.rs` | ported |
| `codec/reflectcodec/type_codec_test.go` | `tests/derive.rs` | ported |
| `codec/codec_test.go` | `tests/golden_codec.rs` | ported |
| `codec/linearcodec/codec_test.go` | `tests/golden_codec.rs` (`typeid::typeid_table_matches`) | ported |
| `codec/test_codec.go` (RunAll) | `tests/conformance.rs` + `src/codectest.rs` | ported |
| `codec/codec_test.go` (`FuzzStructUnmarshal`/round-trip) | `tests/proptests.rs` (`prop::codec_roundtrip` @ 4096 cases, `prop::decode_never_panics`, `bounds::*`) | ported |
| `codec` fuzz harness (decode-never-panics + round-trip) | `fuzz/fuzz_targets/codec_roundtrip.rs` | ported |

## Notes / deviations

- **Golden vectors are hand-derived, not Go-extracted.** `tests/vectors/codec/`
  shipped only `MANIFEST.md`; the M0.2 extractor produced no `.json`. The
  `codec.json` + `typeid_table.json` here were computed directly from the
  wire-format rules (`specs/03` §2.4, `specs/15` §4.1/§6) and carry a
  `_provenance` note. A differential cross-check against a Go `Manager.Marshal`
  dump is deferred to the X-cross-cutting milestone. The
  `conformance::run_codec_suite` test is the primary correctness anchor (no Go
  vectors needed).
- **`Vec<u8>`** flows through the generic `Vec<T>` codec (each `u8` writes one
  raw byte) rather than a bulk-copy specialization; the produced bytes are
  byte-identical to Go's `u32`-count + raw-bytes path. (Rust trait coherence
  precludes a `Vec<u8>` override without `specialization`.)
- **Maps** are supported for `BTreeMap<K, V>` via the serialize-then-sort path
  with strictly-increasing decode enforcement (`type_codec.go:423`).
- The `unused_crate_dependencies` workspace lint is **not** opted into by these
  crates (matching the already-landed `ava-utils`/`ava-types`/etc., none of which
  carry `[lints] workspace = true`). See the final report's discrepancy list.

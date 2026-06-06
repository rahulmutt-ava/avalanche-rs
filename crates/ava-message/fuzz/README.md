# ava-message fuzzing (M2.6)

Canonical [`cargo-fuzz`](https://rust-fuzz.github.io/book/cargo-fuzz.html) crate
for `ava-message`. Spec refs: `specs/02-testing-strategy.md` §8 (fuzzing —
cargo-fuzz + `arbitrary`; the mandated `ava-message` "parse arbitrary wire
frames; must never panic or over-read" target) and §4.1 (the
proptest-regressions corpus is non-negotiable); cross-cutting pattern in
`plan/X-cross-cutting.md` task X.16.

## Targets

| Target | Input | Invariant |
|--------|-------|-----------|
| `decode_never_overreads` | `&[u8]` | `MsgBuilder::unmarshal(data)` never panics, never reads past the buffer, and never allocates more than `MAX_MESSAGE_SIZE` (the zstd decode path is bounded to 2 MiB). `Ok`/`Err` both fine. |

The target is a thin wrapper over `ava_message::fuzz_support::decode_never_overreads`
(gated behind the crate's `fuzzing` feature), so the fuzz logic is defined
exactly **once** and shared with the stable smoke test below.

## Running

Requires a **nightly** toolchain + LLVM sanitizers (cargo-fuzz injects
`-Zsanitizer=address` + sancov). Under rustup the pinned `rust-toolchain.toml`
auto-selects nightly:

```sh
cargo fuzz run decode_never_overreads -- -runs=100000
```

On the repo's pinned **stable** nix dev shell `cargo fuzz` is not runnable; the
equivalent per-PR coverage is the stable proptest smoke harness
`crates/ava-message/tests/prop_fuzz_smoke.rs` (run by `cargo nextest`).

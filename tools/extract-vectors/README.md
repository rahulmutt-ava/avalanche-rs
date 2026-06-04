# extract-vectors (M0 stub)

A scratch Go program that dumps **golden test vectors** from a pinned
`avalanchego` tree, as `{input, expected_hex | expected}` JSON, for the Rust
parity tests to load. This is the mechanism behind the `golden::*` exit-gate
tests in milestone M0 (`plan/M0-foundations.md`, task M0.2; `specs/02` §6.2).

> **Status: vectors committed for the M0 surfaces below** (extracted from
> avalanchego `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`; see
> `../../tests/vectors/manifest.json`). The `codec` goldens and the
> `large_rsa_key` reject cert remain TODO (they need the Rust type registry /
> M0.16 and a crafted RSA-3072 cert / M0.20). The CI integration, the drift job,
> and the full corpus (`specs/22`) are owned by `plan/X-cross-cutting.md`.

## Not part of the Cargo workspace

`extract.go` is a standalone Go program. It is gated out of the Rust workspace
(no `Cargo.toml` here) and is built against the pinned avalanchego source tree,
not this repo.

## Surfaces dumped (M0)

Each surface writes one or more files under `../../tests/vectors/<surface>/`.
See each directory's `MANIFEST.md` for the exact expected files and schema.

| Surface dir | Contents | Owning spec |
|---|---|---|
| `rng/` | MT19937 / MT19937-64 raw `Uint64` streams (seeds incl. 0) | `03` §10.4 item 1 |
| `sampler/` | `Uint64Inclusive` triples + uniform/weighted/wwr outputs | `03` §10.4 items 2–3 |
| `codec/` | linear-codec golden bytes per registered-type family | `03` §8 item 1 |
| `ids/` | CB58 + bech32 + hex address strings (Mainnet/Fuji) | `03` §8 item 5 |
| `crypto/` | secp256k1 RFC6979 + recover; BLS sign/agg/PoP + DSTs; NodeID-from-cert | `03` §8 items 6–7, `25` §8.1 |
| `upgrade/` | fork activation booleans at `{forkTime-1ns, forkTime, +1ns}` | `03` §11.3 |

## Running (from inside the avalanchego module)

`extract.go` imports avalanchego packages, so it runs from within the pinned
avalanchego checkout (it carries a `//go:build ignore` tag and is not part of
this Cargo workspace):

```sh
cp tools/extract-vectors/extract.go ~/avalanchego/cmd_extract_vectors/main.go
cd ~/avalanchego && go run ./cmd_extract_vectors \
    --out /path/to/avalanche-rs/tests/vectors
rm -rf ~/avalanchego/cmd_extract_vectors   # leave avalanchego clean
```

Each multi-case file carries a `_provenance` block (revision + extractor) per
`specs/02` §6.1. The pinned revision is recorded in
`tests/vectors/manifest.json` (`avalanchego_revision`).

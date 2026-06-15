# criterion bench-guard baselines

These JSON files are the **committed advisory baselines** consumed by
`cargo xtask bench-guard` (specs/02 §9, 16 §5(9), 00 §9). Each names a
critical-path criterion bench and its baseline mean (nanoseconds):

| file | crate | bench target / id | measures |
|------|-------|-------------------|----------|
| `codec_roundtrip.json` | `ava-codec` | `codec` / `codec_roundtrip` | `Packer` encode→decode round-trip (codec encode/decode, §9) |
| `secp256k1_verify.json` | `ava-crypto` | `signature` / `secp256k1_verify` | secp256k1 recoverable-signature verify over a digest (signature verify, §9) |

## How the gate works

`cargo xtask bench-guard` runs each bench (`cargo bench -p <crate> --bench
<target>`), reads criterion's mean point estimate from
`target/criterion/<id>/new/estimates.json`, and compares it to the `mean_ns`
here. It **fails if any bench is more than the threshold (default 10%,
`--threshold <fraction>`) slower than its baseline.** The pure comparison logic
(`over_threshold`) has unit tests in `xtask/src/bench_guard.rs`, including a
synthetic 2x-regression case that proves the gate trips.

## These numbers are machine-specific and advisory

Absolute criterion timings depend heavily on the host CPU, load, and toolchain.
The committed values are **padded above the locally measured mean** so a clean
run on a comparable machine passes the default 10% gate without flapping. They
are NOT a portable performance contract.

**Real CI baselines should be regenerated per-runner.** A CI runner should run
the benches on a quiet baseline commit and snapshot its own
`target/criterion/<id>/new/estimates.json` means into these files (the loader
also accepts criterion's native `{"mean":{"point_estimate":...}}` shape), then
compare subsequent PRs against that runner-local baseline.

## Regenerating

```sh
cargo bench -p ava-codec  --bench codec
cargo bench -p ava-crypto --bench signature
# then read target/criterion/<id>/new/estimates.json mean.point_estimate
# and write it (optionally padded) into the matching *.json mean_ns field.
```

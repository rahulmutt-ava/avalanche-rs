# `compatibility.json` — provenance

`crates/ava-version/compatibility.json` is copied **byte-identical** from the
upstream Go `avalanchego` tree:

- **Go source path:** `version/compatibility.json`
- **Upstream commit:** `0b0b57143c286b0653850a41839f4668f8e0b66e`
- **What it is:** `version.RPCChainVMProtocolCompatibility` — the map from each
  rpcchainvm **protocol version** (decimal string key) to the set of
  `avalanchego` releases that shipped that protocol version. Loaded in Go via
  `//go:embed compatibility.json` (`version/compatibility.go`).
- **What it is NOT:** it is *not* consulted in the peer connect/reject path — that
  is the numeric rule in `version.Compatibility` (mirrored by
  `crates/ava-version/src/compatibility.rs`, `specs/26` §3). The Go comment is
  explicit: *"This is not used by avalanchego, but is useful for downstream
  libraries."* It is a lookup table for VM authors / tooling.

It is **data, not a generated artifact**, so it is checked in (distinct from
generated artifacts under `specs/00` §11.1(8)).

## Updating

Bump this file together with the version constants in
`crates/ava-version/src/application.rs` (`CURRENT`, `RPC_CHAIN_VM_PROTOCOL`,
`CURRENT_DATABASE`) in the same change — exactly as Go bumps `version/constants.go`
+ `version/compatibility.json` together. Re-copy from the new upstream pin:

```sh
cp ~/avalanchego/version/compatibility.json crates/ava-version/compatibility.json
```

The `golden::compatibility_json_byte_parity` test (in `tests/compat_matrix.rs`)
guards byte parity against the embedded copy and asserts it parses to the same
table the code loads.

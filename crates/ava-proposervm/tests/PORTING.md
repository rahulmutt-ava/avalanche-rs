# ava-proposervm — PORTING.md

Provenance and parity notes for the Go → Rust port of `vms/proposervm`.

## Source (pinned)

All ports are against the pinned `../avalanchego` tree:

- `vms/proposervm/block/` — `block.go`, `codec.go`, `header.go`, `option.go`,
  `parse.go`, `build.go` → `src/block/`.
- `vms/proposervm/proposer/windower.go` → `src/proposer/windower.rs`.

## Golden vectors

### `tests/vectors/proposervm/blocks/blocks.json` (M3.21)

Produced by a scratch Go program (run in `/tmp`, **not** committed to this repo)
against `vms/proposervm/block`:

- `block.BuildOption(parent, inner)` → the `option` vector.
- `block.BuildUnsigned(parent, ts, pChainHeight, Epoch{}, inner)` →
  `post_fork_unsigned`.
- `staking.NewTLSCert()` + `block.Build(..., Epoch{}, cert, ..., chainID, key)`
  → `post_fork_signed` (a real Go-signed block; the cert + header bytes are
  captured so the Rust test re-verifies the signature via
  `staking::check_signature`).
- `block.Build(..., epoch{777,3,...}, cert, ...)` → `granite_signed`.
- `block.BuildUnsigned(..., epoch{777,3,...}, inner)` → `granite_unsigned`.

Each record captures the full serialized `bytes`, the Go `ID()`, `ParentID()`,
inner `Block()`, timestamp/pChainHeight, the proposer NodeID, the certificate
DER, and the `BuildHeader(...)` bytes.

The test (`golden_block.rs`) asserts:

1. **byte-exact re-encode** — `parse_without_verification(bytes).bytes() == bytes`;
2. **block-ID rule** — option `id == sha256(bytes)`; signed/Granite
   `id == sha256(bytes[.. len - 4 - len(sig)])` (strip the u32-length-prefixed
   signature suffix), bit-identical to Go's `hashing.ComputeHash256Array`;
3. **signature verification** — `parse(bytes, chainID)` runs `verify()` which
   builds `Header{chain, parent, id}` and calls `staking::check_signature` over
   `header.bytes()`; a Go-signed block passes, and the rebuilt header bytes
   equal Go's `BuildHeader(...)` output;
4. **zero-epoch rejection** — a Granite block with `Epoch{}` fails `verify()`
   with `Error::ZeroEpoch`.

## Findings / deviations

- **Manual codec, not `#[derive(AvaCodec)]`.** The block bodies mix `Id`
  (32 raw bytes, no length prefix), `i64` (the timestamp — Go `int64`), and
  length-prefixed `Vec<u8>` (cert / inner / signature). `ava_types::Id` does not
  implement `ava_codec::Serializable`, and the derive does not cover `i64`, so
  the block (de)serialization is hand-written against `ava_codec::packer::Packer`
  (`src/block/stateless.rs`, `src/block/codec.rs`). The wire bytes are
  byte-identical to Go's linear codec (verified by the golden vectors). The
  registration order (`statelessBlock(0)`/`option(1)`/`statelessGraniteBlock(2)`)
  is encoded as the `u32` typeID in `codec.rs`.
- **`check_signature(cert, header.bytes(), sig)`** matches Go
  `staking.CheckSignature(cert, headerBytes, sig)`: `ring` hashes the message
  internally with SHA-256, exactly as Go signs `key.Sign(rand, sha256(headerBytes), crypto.SHA256)`.
- **Pre-fork blocks** (`src/block/pre_fork.rs`) are a thin pass-through of the
  inner-VM bytes/identity; the full fork-regime selection lands with the VM
  wrapper (M3.23).

## Status

| Go file | Rust | Status |
|---------|------|--------|
| `block/codec.go` | `src/block/codec.rs` | done |
| `block/block.go` | `src/block/{stateless,post_fork}.rs` | done |
| `block/header.go` + `BuildHeader` | `src/block/header.rs` | done |
| `block/option.go` + `BuildOption` | `src/block/option.rs` | done |
| `block/parse.go` | `src/block/codec.rs` (`parse`/`parse_without_verification`) | done |
| `block/build.go` | `src/block/{post_fork,option,header}.rs` | done |
| `proposer/windower.go` | `src/proposer/windower.rs` | done (M3.22) |

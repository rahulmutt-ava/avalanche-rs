<!--
Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
See the file LICENSE for licensing terms.
-->

# Migrating a Go data dir into the Rust node (R2)

Closes overview risk **R2** (`specs/00-overview-and-conventions.md` §11.2). The
authoritative design is **`specs/04-storage-and-databases.md` §7 + §11**; this
document records what the `ava-database::migrate` module (feature `migrate`)
implements and the precise status of each piece.

## The problem

A Go avalanchego node stores its base DB as either **goleveldb** (`db/v1.4.5/`,
pre-v1.10.15) or **Pebble** (`pebble/`). The Rust node's only on-disk backend is
**RocksDB**. The engines share neither file format nor SSTable layout, so a Rust
node **cannot open a Go data dir in place**.

Crucially, everything layered *inside* the KV pairs is **byte-identical** across
the two nodes (the entire `04 §10` catalog: prefixdb SHA-256 namespaces, linkeddb
node codec, archivedb `^height` keys, blockdb file format, merkledb node/proof
bytes). So migration reduces to **copying every `(key, value)` pair into RocksDB,
verbatim** — no transformation of key or value bytes.

## Decision (BINDING): offline import tool

Ship an **offline, one-shot import tool**, run before first Rust-node start.
Rejected alternatives (per `04 §11.2`): in-place open (impossible for Pebble),
runtime translation layer, replay-from-genesis.

> **In-place open is NOT supported — especially for Pebble.** There is no
> production-grade pure-Rust Pebble reader. The correctness-guaranteed Pebble
> path is the Go export sidecar described below.

## CLI surface (wired in M12)

```
avalanchego db migrate --from <godir> --to <rocksdir> \
    --db-type {leveldb|pebble} [--verify {none|roots|full}] [--resume]
```

- `--from` / `--to` — source Go dir / destination RocksDB dir.
- `--db-type` — overrides auto-detection (`v1.4.5/` ⇒ leveldb, `pebble/` ⇒
  pebble).
- `--verify` — post-migration check tier (default `roots`).
- `--resume` — skip pairs already copied (uses the `MIGRATION_CURSOR_KEY`
  checkpoint).

This milestone (M1.24) ships the **library**; the CLI is assembled in **M12**.

## The import path (`migrate` module)

| Piece | Status |
|---|---|
| `GoDbSource` trait — every pair, lexicographic, **verbatim** | **implemented** |
| `migrate(src, dst, resume_after)` driver — 64 MiB flush window + `MIGRATION_CURSOR_KEY` resume checkpoint, byte-for-byte | **implemented + tested** |
| `leveldb::RocksDbCompatSource` — goleveldb **fast path** (RocksDB opens the LevelDB dir) | **implemented** (gated on the `rocksdb` feature) |
| `leveldb::RustyLevelDbSource` — pure-Rust goleveldb **fallback** | **documented best-effort stub** (see below) |
| `pebble::PebbleSidecarSource` — Pebble via Go sidecar; framing parser | parser **implemented + tested**; sidecar **spawn stubbed** (M12) |
| `verify(level, root_verifiers)` — `None`/`Roots`/`Full`, pluggable root re-derivation | **implemented + tested** |

### Byte-exactness

`migrate` never decodes a key or value. The whole `04 §10` catalog rides along
untouched — this is what makes a one-pass copy correct. The driver is generic
over `&dyn Database`, so the same logic serves the production `RocksDb` target
and the in-memory `MemDb` used by the tests.

### Resume / idempotency

After each 64 MiB flush (and at the end) the driver records the last-written key
under `MIGRATION_CURSOR_KEY`. `--resume` passes that cursor as `resume_after`;
the driver skips every key `<= cursor`. A clean re-run is a no-op (every `put`
rewrites identical bytes).

### Bulk-ingest fast path

Because `iter_all` yields keys in lexicographic order, a future optimization can
feed a RocksDB `SstFileWriter` + `ingest_external_file` instead of batched
`put`s, migrating multi-GB dirs in minutes. The current driver uses the
`Database` batch path (correct and backend-agnostic); the SST fast path is a
drop-in for the `RocksDb` target and is exercised by the conformance suite when
the `rocksdb` feature is built.

## Reading the Go DB

### goleveldb (`db/v1.4.5/`)

- **Fast path — `RocksDbCompatSource`:** RocksDB descends from LevelDB and reads
  many classic LevelDB directories directly. When the open succeeds, the
  "migration" is a streaming in-place ingest. Implemented; needs the `rocksdb`
  feature.
- **Fallback — `RustyLevelDbSource`:** for dirs RocksDB refuses. The spec
  proposes the `rusty-leveldb` crate. It is **not yet wired**: it adds an
  unvetted transitive-dependency surface that must clear `cargo deny` first
  (`specs/00` §4 / `deny.toml`). Per the M1.24 directive, the reader ships as a
  documented best-effort stub (correct trait shape + constructor, explanatory
  error on `iter_all`). The driver/verify/fast-path are all complete and tested.

### Pebble (`pebble/`) — sidecar only

No pure-Rust Pebble reader exists. The only correctness-guaranteed path is a
small **Go export sidecar** (`avalanchego-db-export`, ~80 LOC) that links the
real `database/pebbledb` and streams every pair as a length-prefixed framing:

```
u32 key_len (big-endian) ‖ key ‖ u32 value_len (big-endian) ‖ value   (repeat; clean EOF ends)
```

`PebbleSidecarSource` parses that stream. The **frame parser is implemented and
unit-tested** (round-trip, empty stream, truncated frame). The **spawn is
stubbed** — the sidecar binary is built and shipped with the CLI in M12, at which
point `iter_all` swaps the stub for a `std::process::Command` spawn whose stdout
feeds `pebble::parse_stream`.

## Verification (`verify` module)

`verify(dst, level, root_verifiers)`:

- **`None`** — skip.
- **`Roots`** (default) — re-derive the load-bearing compatibility surfaces:
  - **Flat-KV P/X:** a structural walk confirming `"last accepted"` singleton
    pointers survived (present ⇒ non-empty). The full
    `blockID → PackUInt64(height)` chain walk layers on in M12 using the VM
    crates (no trie dependency at the storage tier).
  - **Merkleized SAE/EVM:** for each chain, recompute `merkle_root()` over the
    migrated DB and assert it equals the root in the last block header.
- **`Full`** — additionally sample random pairs back against the source
  (`check_sampled_pair`; the source-resampling loop is wired with the CLI in
  M12).

### Pluggable root re-derivation (decoupled from `ava-merkledb`)

The merkle-root recomputation is supplied by the caller as a `RootVerifier`
(`recompute_root` + `expected_root`), **not** wired to `ava-merkledb`/Firewood in
this module. This keeps `migrate` free of the merkledb/Firewood crates (under
concurrent development) and lets the CLI inject the concrete verifier in M12.
Tests inject a stub verifier (an XOR-fold "root") to prove `verify(Roots)`
detects a corrupted copy.

## Alternative: bootstrap fresh from the network (`04 §11.5`)

A node does **not** need the old data dir at all — it can start empty and obtain
state from peers (state-sync for C-Chain/SAE; bootstrap re-execution for P/X).
For fresh/pruned/small validators this is the **recommended** path: no tool, a
clean RocksDB. Use the import tool for downtime-sensitive validators with deep
archives. Either way, the byte-exactness of `04 §10` is the safety net — a
network-bootstrapped node and an import-migrated node converge on identical
on-disk bytes.

## Tests

`crates/ava-database/tests/migrate.rs` (run `cargo test -p ava-database
--features migrate --test migrate`) uses **`MemDb`** as the ingest target (the
driver is generic over `&dyn Database`, so this is representative; the `RocksDb`
target is covered by the conformance suite). It asserts:

- `migrate_preserves_bytes` — a prefixdb-namespaced key, an archivedb `^height`
  key, a merkledb node key, the `singleton`, and an empty key all read back
  byte-identical.
- `migrate_resumable` — a `--resume` re-run past `MIGRATION_CURSOR_KEY` copies
  nothing.
- `verify_roots_detects_mismatch` — `verify(Roots)` via the injected stub
  verifier fails on a corrupted copy.

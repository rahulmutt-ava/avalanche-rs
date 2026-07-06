# M1 — Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Deliver the byte-exact storage tier — the synchronous `Database` trait + every KV backend, the path-based `ava-merkledb` trie (root/proof/range-proof/sync protocol), `ava-blockdb`, `ava-archivedb`, Firewood wiring (SHA + ethhash), and the Go-data-dir import tool — proven against Go via golden roots/proofs and the shared conformance + proptest batteries.
**Tier:** T1 — Storage
**Crates:** `ava-database` (backends: rocksdb, mem, prefix, version, meter, corruptable, rpc, linkeddb, heightindex), `ava-merkledb` (+ `firewood` wiring with SHA + ethhash features), `ava-blockdb`, `ava-archivedb`
**Owning specs:** `04-storage-and-databases.md` (primary), `27-crash-consistency-and-recovery.md`, `19-state-sync-and-bootstrap.md`, `15-serialization-and-wire-formats.md` (§3.10 sync proto, §3.4 rpcdb proto)
**Depends on (prior milestones):** M0 (ava-codec for linearcodec node/proof encodings, ava-crypto for SHA-256/Keccak hashing, ava-types for `Id`/`Maybe`/fixed-byte newtypes), plus the M0 proto build wiring (`prost`/`tonic` via `build.rs`).
**Exit gate (named tests):** `conformance::run_database_suite` + `prop::db_oracle_btreemap` (every backend); **`golden::merkledb_root`** (fixed K/V sets → Go root); `golden::merkledb_proof` + `golden::range_proof` (wire-critical); `prop::merkle_order_independent_root`; `golden::firewood_ethhash_root` (vs Go EVM root); `prop::blockdb_roundtrip`.

---

## Progress

> **Status (2026-06-05, M1 IN PROGRESS — Wave 1 landed):** First parallel wave merged to
> `main` via three isolated worktree agents (distinct crates → conflict-free merges):
> - **`ava-database` contract + reference backend (M1.1, M1.2, M1.3):** `Database` trait family +
>   sentinel `Error` + byte-exact helpers; the `dbtest` conformance battery + BTreeMap-oracle
>   proptest behind the `testutil` feature; `MemDb`. Green: `conformance::run_database_suite`,
>   `prop::db_oracle_btreemap`, `unit::helpers_byte_exact`, `unit::error_variants`.
> - **`ava-merkledb` core (M1.12, M1.13, M1.14):** `Key`/`BranchFactor` bit-path, `DbNode`
>   model + `encodeDBNode` byte-exact (+ all decode rejections), SHA-256 `HashNode`. Green:
>   `golden::key_pack`, `golden::node_codec_encode`, `golden::node_codec_decode_rejects`,
>   **`golden::merkledb_root`** (empty→single→multi, 18 cases over BranchFactor 256/16/2).
> - **`ava-blockdb` (M1.22):** append-optimized height-indexed store + torn-write recovery scan.
>   Green: **`prop::blockdb_roundtrip`**, `golden::blockdb_header_layout`, `unit::recovery_rebuilds_index`.
>
> **All Wave-1 golden vectors are REAL Go-extracted** (scratch programs run against the
> `../avalanchego` tree at rev `fb174e8…`, then removed): merkledb keys/nodes/roots from
> `x/merkledb`, blockdb header/checksum from `x/blockdb`, timestamp from `time.Time.MarshalBinary`.
> Workspace after merge: **152 tests pass** (`--all-features`, was 124 at M0 exit), `cargo build
> --workspace --all-features`, `cargo build -p avalanchers`, `cargo clippy --workspace
> --all-targets --all-features -- -D warnings`, `cargo fmt --all --check` — all clean.
>
> **Findings recorded during Wave 1 (specs updated where noted):**
> - **`Batch` supertrait correction (spec 04 §1.3):** the spec's `Batch: KeyValueWriter +
>   KeyValueDeleter` (both `&self`) is internally inconsistent — a batch accumulates ops (`&mut
>   self`) and serves as a `replay` target. Implemented as **`Batch: WriteDelete`** (the `&mut`
>   Put/Delete trait). Spec 04 §1.3 updated.
> - **merkledb codec decode-rejections (spec 04 §3.3):** Go `decodeDBNode` does **not** validate
>   child index against the configured branch factor (that happens at trie-construction time). The
>   codec rejects `index > 255`, out-of-order/duplicate index (`errChildIndexTooLarge`), and
>   `num_children > 256` (`errTooManyChildren`). Spec 04 §3.3 clarified.
> - **blockdb on-disk format (spec 04 §5.1):** all headers are **little-endian**; the index-entry
>   and block-entry `size` fields store the **compressed** length while the checksum
>   (`xxhash.Sum64` = **XXH64 seed 0**, `github.com/cespare/xxhash/v2`) is over the **uncompressed**
>   bytes; `MarshalBinary` leaves reserved bytes zero. Cross-implementation byte-replay of
>   *compressed* `.dat` payloads is not asserted (each side owns its zstd encoder; checksum is over
>   uncompressed data). Spec 04 §5.1 updated.
> - **`Maybe<T>`** is defined locally in `ava-merkledb` (`src/maybe.rs`), not added to `ava-types`
>   (kept M0 crates untouched for parallel-merge safety). The M1 header above still lists
>   `ava-types/Maybe`; revisit if a second consumer appears (then promote to `ava-types`).
> - **`[lints] workspace = true` not opted into by the new crates** (matches the M0-crate
>   majority). With it on, `unused_crate_dependencies` fires on every integration-test binary, and
>   the workspace `arithmetic_side_effects`/`indexing_slicing` warn-lints + `-D warnings` reject
>   byte-manipulation ports wholesale. Crates still pass `cargo clippy -- -D warnings` (clippy::all).
>   This remains the **open X-cross-cutting decision** carried over from M0.
> - **`unused_crate_dependencies` test-binary gotcha (for M1.4–M1.11 backend agents):**
>   `ava-database` opts into `[lints] workspace = true`, so each integration test needs
>   `#![allow(unused_crate_dependencies)]` at its crate root, and `testutil`-gated test files must
>   guard items with `#[cfg(feature = "testutil")]` (not a crate-level `#![cfg]`).
> - **New third-party deps introduced (owed to X-cross-cutting for workspace-dep promotion):**
>   `ava-blockdb` pins `twox-hash`, `zstd`, `lru` (+ dev `tempfile`) directly in its crate
>   `Cargo.toml` (root `Cargo.toml` left untouched to avoid parallel-merge conflicts).
> - **In-memory trie builder (M1.14)** is a faithful port of `view.insert` sufficient for fixed-K/V
>   roots; full DB-backed View/history is M1.15 (not yet done).
>
> **Status (2026-06-06, M1 Wave 2 landed):** second parallel wave merged to `main` via three
> isolated worktree agents (distinct crates → conflict-free):
> - **`ava-database` wrapper backends (M1.5, M1.6, M1.8):** `PrefixDb` (SHA-256 namespacing),
>   `VersionDb` (overlay + merge iterator + `commit_batch`), `CorruptableDb` (latch on `Other`
>   only). Each passes `conformance::run_database_suite` + `prop::db_oracle_btreemap`. Green:
>   `golden::prefix_namespacing` (Go-extracted SHA-256 vector).
> - **`ava-merkledb` View/history (M1.15, M1.16):** `View`/`TrieView` over a base `Database`
>   (Arc-linked validity + arc_swap committed-root + sibling/descendant invalidation), bounded
>   `history` ring, `intermediate_node_db`/`value_node_db` over §10.8 prefixes, cleanShutdown
>   rebuild. Green: `unit::{commit_invalidates_siblings, view_layering_equals_direct,
>   clean_shutdown_rebuild, commit_requires_db_parent}`, **`prop::merkle_order_independent_root`**
>   (+ delete-all→EMPTY, layering==direct, BTreeMap oracle).
> - **`ava-archivedb` (M1.23):** height-versioned KV with `^height` encoding. Green:
>   `golden::archivedb_key_encoding` (Go-extracted), `unit::reads_newest_at_or_below`.
>
> Workspace after Wave 2: **190 tests pass** (`--all-features`; 152 after Wave 1), `cargo build
> --workspace --all-features` / `-p avalanchers`, `cargo clippy --workspace --all-targets
> --all-features -- -D warnings`, `cargo fmt --all --check` — all clean.
>
> **Findings recorded during Wave 2:**
> - **Order-independent-root proptest earned its keep:** it caught a real commit bug — a node
>   transitioning value→intermediate (a key losing its value as a sibling is added/removed) was
>   left in the value-node store, so `get` returned `Some(b"")` instead of `None`. Fixed
>   `commit_view` to compute add/delete of value vs intermediate nodes independently (mirrors Go
>   `applyChanges`); triggering seed committed to `proptest-regressions/`.
> - **merkledb View architecture (faithful, simplified):** builds the merged K/V set up the parent
>   chain and diffs the full node set against the parent, rather than Go's incremental
>   node-change tracking. Behavior-identical (verified vs `merkle_root` + oracle); single-pass
>   deterministic walk (rayon pulled in but parallel subtrie hashing deferred as a pure
>   optimization). `close()` writes the clean-shutdown marker; no `Drop` auto-close (a missing
>   marker just triggers the idempotent rebuild on next open — matches Go §27 §4.1).
> - **`VersionDb`:** merge iterator snapshots BOTH overlay and base at creation (avoids a
>   self-referential lifetime over the lock guard; equivalent because the base yields snapshot
>   iterators). `commit_batch()` returns an owned `VersionCommitBatch<D>` (Arc<D> + buffered ops);
>   unwritten-until-`write()` semantics (§27 §2.2/§2.3) hold.
> - **`PrefixDb`:** the prefix-JOIN path is an explicit `PrefixDb::join(&self, prefix)` method
>   (Rust generics can't runtime-detect "base is a PrefixDb" like Go's `New`); namespacing bytes
>   are byte-identical to Go regardless.
> - **`ava-archivedb` tombstone precision (spec 04 §5.2 clarified):** a *delete* stores an
>   **empty (zero-length)** DB value; a real value stores `0x00 ‖ value`; `parseDBValue` treats
>   `len==0` as the tombstone. An explicitly-stored empty *user* value still "exists" and is
>   distinct from a delete (regression-tested). `get_height` on a tombstone returns `NotFound`;
>   the lower-level `get_entry` still exposes the delete height for strict Go `GetEntry` parity.
>   No shared `uvarint` helper exists yet — archivedb has a local LEB128 `put_uvarint`/`read_uvarint`
>   (candidate for promotion to a shared helper).
>
> **Status (2026-06-06, M1 Wave 3 landed):** third parallel wave merged to `main` via two
> isolated worktree agents (distinct crates → conflict-free):
> - **`ava-database` remaining backends (M1.7, M1.9, M1.10, M1.4):** `MeterDb` (Prometheus
>   `calls`/`duration`/`size` keyed on the 21-value `method` label), `LinkedDb` (linearcodec nodes,
>   LIFO iterator, LRU head/node caches), `HeightIndex` trait + `HeightIndexMemDb`/`HeightIndexMeterDb`
>   + `run_heightindex_suite`, and **`RocksDb`** (the FFI on-disk default — `rust-rocksdb` 0.22 /
>   librocksdb-sys 8.10.0 builds cleanly in the Nix toolchain; snapshot iterators, atomic
>   `WriteBatch`, `AtomicBool` close-gate). Green: `golden::meterdb_metric_names`,
>   `golden::linkeddb_node_codec`, `conformance::run_heightindex_suite`, and
>   `conformance::run_database_suite` + `prop::db_oracle_btreemap` over meterdb/heightindex-meterdb/rocksdb.
> - **`ava-merkledb` proofs (M1.17, M1.18):** `Proof` (inclusion/exclusion), `RangeProof`,
>   `ChangeProof` with a hand-rolled minimal protobuf wire encoder (no proto build infra added),
>   generation + verification ports of Go `getProof`/`verifyProofPath`/`addPathInfo`. Green:
>   **`golden::merkledb_proof`**, **`golden::range_proof`** (REAL Go-extracted, deterministic
>   marshaler), `prop::proof_verify_accepts_valid_rejects_tampered` (+ 2 more proptests).
>
> Workspace after Wave 3: **208 tests pass** (`--all-features`; 190 after Wave 2), `cargo build
> --workspace --all-features` / `-p avalanchers`, `cargo clippy --workspace --all-targets
> --all-features -- -D warnings`, `cargo fmt --all --check`, doctests — all clean.
>
> **Findings recorded during Wave 3 (specs updated where noted):**
> - **meterdb method-label set (spec 04 §2.5 updated):** the full driven set is **21** values
>   (adds `batch_size`, `iterator_key`, `iterator_value` beyond the §2.5 enumeration; iterator
>   ctor label is `new_iterator`). Go-extracted vector committed.
> - **linkeddb node codec (spec 04 §10.6 updated):** node bytes = `0x0000` (codec.Manager version)
>   ‖ `u32-len Value` ‖ `bool HasNext` ‖ `u32-len Next` ‖ `bool HasPrevious` ‖ `u32-len Previous`;
>   empty node = 16 zero bytes; `node_key(k)=0x00‖k`, head at `0x01`. `LinkedDb` is NOT a full
>   `Database` (reader/writer/deleter + LIFO only), so it doesn't run the shared suite.
> - **proof proto determinism (spec 15 §3.10 updated):** `ProofNode.children` is the one proto map
>   on the wire; byte-parity requires Go's `Deterministic:true` marshaler (sorts map keys). There is
>   no single-`Proof` message — a `Proof.path` is a bare `repeated ProofNode`. Used a hand-rolled
>   protobuf encoder (BTreeMap ascending children) → byte-equal to Go.
> - **proof port simplifications:** in-range value tampering isn't always detectable by range-proof
>   verification (Go behaves identically — boundary nodes injected by ID mask sub-values);
>   `ChangeProof` generation here takes before/after states directly (history-backed generation is
>   M1.19). Verification semantics match Go.
> - **rocksdb wrapper keeps `#![forbid(unsafe_code)]`** — all FFI is inside `librocksdb-sys`; snapshot
>   iterators collect the range into an owned `Vec` at creation (avoids self-referential lifetime),
>   point-in-time-correct, mirroring memdb.
> - **New deps (owed to X-cross-cutting for workspace-dep promotion):** `ava-database` now pins
>   `prometheus 0.13`, `lru 0.12` (matches ava-blockdb), `ava-codec` (path; first ava-database→ava-codec
>   edge), and behind a new optional `rocksdb` feature: `rocksdb 0.22` + `tempfile 3`. `ava-merkledb`
>   added **no** new deps.
>
> **Status (2026-06-06, M1 Wave 4 landed):** fourth parallel wave merged to `main` via two
> isolated worktree agents (disjoint paths — only the proto agent touched root `Cargo.toml`):
> - **proto/tonic/prost build pipeline + M1.11 rpcdb (`ava-database`, X-cross-cutting):** stood up
>   the repo's first protobuf/gRPC codegen path — `proto/rpcdb/rpcdb.proto` (copied from avalanchego,
>   `_provenance.md`), `prost 0.13`/`tonic 0.12`/`tonic-build 0.12`/`tokio 1` in
>   `[workspace.dependencies]`, a `build.rs` in `ava-database` that generates into `OUT_DIR`
>   (**not committed**, gated by `CARGO_FEATURE_RPCDB` so non-rpcdb builds never invoke `protoc`),
>   types reached via `tonic::include_proto!("rpcdb")`. `DatabaseClient`/`DatabaseServer` behind a
>   new `rpcdb` feature pass the full `conformance::run_database_suite` + `prop::db_oracle_btreemap`
>   over an in-process loopback tonic channel. **This is the pattern every future proto crate
>   reuses** (sync M1.19, p2p M2, vm M3, …).
> - **M1.25 fuzz (`ava-merkledb`):** standalone `crates/ava-merkledb/fuzz/` cargo-fuzz crate
>   (`op_stream` + `node_codec` targets, committed corpus, nightly `rust-toolchain.toml`), shared
>   `#[cfg(feature="fuzzing")] fuzz_support` module (one source of truth for `DbOp`/op-apply/decoder),
>   and a **stable** proptest smoke harness `tests/prop_fuzz_smoke.rs` (`prop::fuzz_op_stream_smoke`,
>   `prop::node_codec_never_panics`) that runs in `cargo nextest` TODAY.
>
> Workspace after Wave 4: **212 tests pass** (`--all-features`; 208 after Wave 3), `cargo build
> --workspace --all-features` / `-p avalanchers`, `cargo clippy --workspace --all-targets
> --all-features -- -D warnings`, `cargo fmt --all --check`, doctests, and the standalone fuzz crate
> (`cargo build --manifest-path crates/ava-merkledb/fuzz/Cargo.toml`) — all clean.
>
> **Findings recorded during Wave 4 (specs updated where noted):**
> - **Proto codegen path established (spec 01 §8.1 / 15 §2 confirmed):** per-crate `build.rs` +
>   `tonic-build`, generated into `OUT_DIR`, NOT committed; `protoc` 32.1 + `buf` 1.59 from the Nix
>   shell (set `PROTOC` to the nix path if PATH lookup is unreliable). `.bytes(["."])` maps all proto
>   `bytes` fields → `bytes::Bytes` (15 §5). build.rs codegen gated on `CARGO_FEATURE_RPCDB`.
> - **rpcdb server iterator is a point-in-time snapshot (spec 04 §2.8 clarified), not Go's live
>   iterator:** a live server-side `BoxIter<'a>` is self-referential over the `Arc<dyn DynDatabase>`
>   and inexpressible under `#![forbid(unsafe_code)]` without a helper (`ouroboros`). Snapshotting at
>   `NewIterator` matches memdb's own `TestIteratorSnapshot` semantics; the client-side `closed`
>   `AtomicBool` (Go parity, `db_client.go`) covers `iterator_closed`/`iterator_error`. Passes the
>   full battery; server laziness is unobservable through the `Database` contract.
> - **Sync↔async bridge:** `DatabaseClient` owns a per-client tokio `Runtime` and `block_on`s every
>   RPC (keeps the sync `Database`/`DynDatabase` surface; consistent with 04 §1.2 call-site blocking).
> - **rpcdb proto has only `NewIteratorWithStartAndPrefix`** (no plain `NewIterator` RPC) — matches Go.
> - **Fuzz needs nightly (spec 02 §8 confirmed):** `cargo fuzz build` fails on the pinned stable
>   toolchain with `error: the option 'Z' is only accepted on the nightly compiler` (it injects
>   `-Zsanitizer=address` + sancov). The fuzz crate itself COMPILES on stable (libfuzzer-sys builds;
>   only the instrumented run needs nightly). **Local gate = the stable `prop_fuzz_smoke` proptest;
>   nightly = a dedicated CI `test-fuzz` job.** Recommended X.2/X.16 follow-up: add a
>   `rust-bin.nightly` `fuzzToolchain` to `flake.nix` (or rustup nightly in the CI fuzz job) + a
>   `test-fuzz`/`test-fuzz-long` Task target running `cargo fuzz run <target> -- -max_total_time=<n>`.
> - **New deps (owed to X-cross-cutting for promotion):** root `[workspace.dependencies]` now has
>   `prost 0.13`, `tonic 0.12`, `tonic-build 0.12`, `tokio 1` (added by the proto agent — the
>   sanctioned root edit this wave). `ava-merkledb` added crate-local `arbitrary 1` (optional, under a
>   `fuzzing` feature) — candidate for workspace-dep promotion alongside the X.16 fuzz wiring.
>
> **Status (2026-06-06, M1 Wave 5 landed):** fifth parallel wave merged to `main` via two
> isolated worktree agents (distinct crates → conflict-free):
> - **`ava-merkledb` state-sync (M1.19):** copied `proto/sync/sync.proto` (no external imports;
>   provenance recorded) + a second `build.rs` mirroring the Wave-4 rpcdb pipeline (gated on
>   `CARGO_FEATURE_SYNC`); `SyncDb` trait (04 §3.7) + `SyncableTrie` impl; `WorkHeap`
>   (`Priority{Low,Med,High,Retry}` + range-coalescing) faithfully ported from Go `workheap.go`;
>   `Syncer` (`ArcSwap<Id>` target + tokio `Notify` for `sync.Cond` + bounded task set + rayon
>   verify pool + `update_sync_target` re-queue) + `ProofServer` (port of `network_server.go` with
>   key/bytes caps + change→range fallback on insufficient history) + a `SyncClient` transport trait
>   (`LocalClient` in-process impl; real p2p client is M2). Green: **`golden::sync_proof_wire`**
>   (REAL Go-extracted frames, rev `fb174e8925`), `prop::sync_proof_roundtrip` (+ mid-sync
>   `update_sync_target` → final root == new target), `prop::workheap_invariants`.
> - **`ava-database` R2 import tool (M1.24):** `migrate` feature — `GoDbSource` trait (lexicographic,
>   no byte transformation), `migrate(src, dst: &dyn DynDatabase, resume_after)` driver (64 MiB flush
>   window + `MIGRATION_CURSOR_KEY` resume), `RocksDbCompatSource` (real leveldb-dir reader, gated on
>   `rocksdb` feature), `RustyLevelDbSource` + Pebble-sidecar **spawn** as documented stubs (the
>   Pebble length-prefixed frame **parser** is real + tested), `verify(level)` with `VerifyLevel`
>   and a **pluggable `RootVerifier`** trait (decoupled from `ava-merkledb`), `docs/migration.md`.
>   Green: `unit::{migrate_preserves_bytes, migrate_resumable, verify_roots_detects_mismatch,
>   verify_none_is_noop}`.
>
> Workspace after Wave 5: **231 tests pass** (`--all-features`; 212 after Wave 4), `cargo build
> --workspace --all-features` / `-p avalanchers`, `cargo clippy --workspace --all-targets
> --all-features -- -D warnings`, `cargo fmt --all --check`, doctests — all clean.
>
> **Findings recorded during Wave 5 (specs updated where noted):**
> - **WorkHeap single-canonical-store (spec 19 §4.2 updated):** Go's two synced containers
>   (`BinaryHeap` + `BTreeMap` over shared pointers) aren't expressible under `#![forbid(unsafe_code)]`
>   without `Rc<RefCell>`; the Rust impl keeps one canonical `BTreeMap<RangeStart, WorkItem>` (None
>   sorts smallest) and derives the highest-priority pop by a bounded scan — behavior-identical
>   (non-overlap, same coalescing, FIFO tie-break).
> - **`SyncDb` history (spec 04 §3.7 updated):** `SyncableTrie` serves change/range proofs at a past
>   root from a bounded root-keyed snapshot ring (reusing M1.18's before/after-state proof gen), not
>   the full merkledb change-history ring; verification semantics + `InsufficientHistory`/`NoEndRoot`
>   are identical. Encode stays the byte-exact hand-rolled `encode_proto` (M1.17/M1.18); prost
>   generated types are used only to decode peer responses + frame request/response.
> - **`MaybeBytes` present-but-empty (spec 15 §3.10 updated):** marshals to empty `bytes` (proto3
>   omits the empty scalar); presence is carried by the parent oneof/field, not the inner bytes.
> - **migrate signature (spec 04 §11.4 updated):** the driver takes `dst: &dyn DynDatabase` (not
>   `&RocksDb`) because the typed `Database` trait carries a GAT iterator and isn't dyn-compatible;
>   `DynDatabase` is the object-safe facade every backend implements (lets tests use `MemDb`,
>   production pass `RocksDb`). `verify` root re-derivation is injected via a caller-supplied
>   `RootVerifier` so the storage tier stays free of merkledb/Firewood (concrete wiring → M12 CLI).
> - **No new third-party deps:** M1.19 referenced only already-workspace deps (`prost`/`tonic`/`tokio`
>   `workspace = true`; `arc-swap`/`rayon` already present). M1.24's `migrate` feature is `[]` (empty)
>   — `Cargo.lock` unchanged, zero new `cargo deny` surface; `rusty-leveldb` deliberately deferred
>   until it clears `deny.toml`.
>
> **Status (2026-06-06, M1 Wave 6 landed):** sixth wave — a single agent did the M1.20→M1.21
> Firewood chain (sequentially dependent, same crate) in `ava-merkledb`:
> - **M1.20 Firewood SHA wiring + `SyncDb` impl:** wraps the `firewood` crate (git tag `v0.5.0`, rev
>   `0695b91f` — the exact rev Go's `firewood-go-ethhash/ffi v0.5.0` wraps; crates.io's `firewood
>   0.2.0` is too old) behind a safe sync module (`#![forbid(unsafe_code)]` held; call sites wrap in
>   `spawn_blocking`). `SyncDb for FirewoodDb` delegates to firewood's native
>   `FrozenRangeProof`/`FrozenChangeProof` (proves the §3.7 protocol is backend-agnostic). Green:
>   `unit::firewood_propose_commit_roundtrip` (pre-commit root, commit advances tip, historical
>   `revision(old_root)` read). **R3 firewood-build risk effectively RETIRED — built clean in ~18s,
>   pure Rust, no slow cmake/native step in this env.**
> - **M1.21 Firewood ethhash + `golden::firewood_ethhash_root`:** REAL Go-extracted vector — scratch
>   Go program against `firewood-go-ethhash/ffi v0.5.0` (prebuilt static lib) on a fixed 3-account +
>   2-storage-slot RLP batch → root `eb8b07d6…`; the Rust `firewood` crate in ethhash mode matches
>   byte-for-byte. Empty-trie case == `types.EmptyRootHash` (`0x56e81f17…`). Vector + `_provenance`
>   committed; scratch deleted.
>
> Workspace after Wave 6: **235 tests pass** (`--all-features`; 231 after Wave 5), `cargo build
> --workspace --all-features` / `-p avalanchers`, `cargo clippy --workspace --all-targets
> --all-features -- -D warnings`, `cargo fmt --all --check` — all clean.
>
> **Findings recorded during Wave 6 (specs updated where noted):**
> - **Firewood API path (spec 04 §4.2 updated):** the real crate uses `firewood::api` (not the
>   sketched `firewood::v2::api`); `propose`/`root_hash`/`revision` are on the `api::Db` *trait*,
>   `commit(self)`/`root_hash`/`val` on `api::Proposal: DbView`. `DbConfig::builder()` requires a
>   `node_hash_algorithm: firewood_storage::NodeHashAlgorithm` (NOT re-exported from `firewood`), so a
>   second optional dep `firewood-storage` (same git tag) is pulled in solely for
>   `NodeHashAlgorithm::compile_option()`.
> - **Hashing mode is a GLOBAL compile-time switch (spec 04 §4.1 clarified):** `firewood/ethhash` →
>   `firewood-storage/ethhash` → Keccak. So the crate's `firewood` (SHA, merkledb-compatible) and
>   `firewood-ethhash` (Keccak, EVM) features map to the *same* dep with `ethhash` toggled — they are
>   **mutually exclusive per build**, not per-instance runtime modes. `HashKey::default_root_hash()`
>   returns `None` (SHA → `Id::EMPTY`) or `Some(0x56e81f17…)` (ethhash). Revision retention via
>   `RevisionManagerConfig::builder().max_revisions(n)` (default 256).
> - **New deps (owed to X-cross-cutting):** `crates/ava-merkledb/Cargo.toml` pins `firewood` +
>   `firewood-storage` (git tag v0.5.0, optional, default-features off) behind `firewood`/
>   `firewood-ethhash` features (`firewood = [..., "sync"]` since the firewood path impls `SyncDb`);
>   dev-dep `tempfile`. `Cargo.lock` updated (pulls firewood + sha3/keccak under ethhash). These are
>   git deps — `cargo deny`/Bazel `crate_universe` may need a git-source allowance (flag for X).
>
> **Status (2026-06-06, M1 Wave 7 — MILESTONE COMPLETE):** single agent did the **M1.26 exit gate**:
> created all 4 `tests/PORTING.md` matrices (seeded from `go test -list` at avalanchego rev
> `fb174e8925`) and re-confirmed the buildable-&-green invariant. Row counts (all `wip` = **0**):
> ava-database 100 ported / 18 na; ava-merkledb 165 ported / 7 na; ava-blockdb 27 / 0; ava-archivedb
> 10 / 0. `na` reasons: leveldb/pebbledb backends → rocksdb (00 §4.4); `Benchmark*` perf-only;
> versiondb `SetDatabase` (typed base, not runtime-swappable); linkeddb not a full `Database` (04
> §10.6); generated mock pkg → `mockall`; `onEvictCache` → `lru` crate; internal Prometheus wiring →
> `ava-node`. **Exit gate GREEN: 235 tests pass, `avalanchers --version`=`avalanchers/1.14.2`,
> clippy/fmt clean.** All named exit tests confirmed: `run_database_suite`+`db_oracle_btreemap` (every
> backend), `run_heightindex_suite_{memdb,meterdb}`, `golden::{merkledb_root,merkledb_proof,range_proof,
> firewood_ethhash_root}`, `prop::{merkle_order_independent_root,blockdb_roundtrip}`.
>
> **🎉 M1 STORAGE COMPLETE** — all tasks M1.1–M1.26 ✅. Storage tier (T1) done: `ava-database` (all 9
> backend families + migrate), `ava-merkledb` (trie/proofs/sync/firewood SHA+ethhash), `ava-blockdb`,
> `ava-archivedb`. **R2** (Go-dir import, scoped) and **R3** (firewood link/build) retired for the
> storage tier. Next milestone: **M2 networking** (`ava-message`, `ava-network` — T2a wire; depends on
> M0 + the proto pipeline, both done).
>
> **Findings recorded during Wave 7 (specs/plan corrections):**
> - **`x/merkledb` path correction:** the classic path-based trie lives at Go `x/merkledb` (rev
>   `fb174e8925`), NOT `database/merkle/` (which is the *firewood-backed* merkle home; merkle sync =
>   `database/merkle/sync`, firewood-backed sync = `database/merkle/firewood/syncer`). The ava-merkledb
>   PORTING matrix was seeded from `x/merkledb` + `database/merkle/sync` + `database/merkle/firewood/syncer`.
>   (The M1 header and a few task lines say `x/merkledb` already; the M1.26 task body's `database/merkle`
>   wording is the imprecise one.)
> - **`cargo xtask porting-report` is still a stub** (defers matrix aggregation to tier-X task **X.20**);
>   it does NOT parse the matrices or fail on `wip` rows yet, so the gate's wip-detection is currently a
>   manual grep. Implement (or formally defer) under X.20.
> - **Final-gate worktrees should branch from the tip of `main`**, not an intermediate wave commit (the
>   M1.26 agent branched from Wave-5 `25ebd3e` and had to merge `main` in to pick up Wave-6 firewood;
>   it was a clean fast-forward, but branch-from-tip avoids the foot-gun).

---

## Dependency map & parallel waves

The `Database` trait + sentinel `Error` (M1.1) is the chokepoint; the dbtest/proptest battery (M1.2) defines the contract every backend must pass. Once both land, **all KV backends parallelize**; once the trait lands, `ava-merkledb`/`ava-blockdb`/`ava-archivedb` parallelize against any backend (they only need `memdb`/`rocksdb` to test).

| Wave | Tasks | Notes |
|---|---|---|
| **W0 — contract** | M1.1 (trait + errors + helpers), M1.2 (dbtest battery + proptest oracle skeleton, `testutil` feature) | Sequential; everything depends on these. |
| **W1 — backends (parallel)** | M1.3 memdb, M1.4 rocksdb, M1.5 prefixdb, M1.6 versiondb, M1.7 meterdb, M1.8 corruptabledb, M1.9 linkeddb, M1.10 heightindexdb, M1.11 rpcdb | Each implements `Database`+`DynDatabase` and passes M1.2's suites. memdb (M1.3) lands first as the reference for the proptest oracle; the rest fan out. |
| **W2 — merkledb core (TDD anchor)** | M1.12 Key/Path, M1.13 node+codec, M1.14 hashing + `golden::merkledb_root` (EMPTY→single-key→multi), M1.15 view/trie/history, M1.16 `prop::merkle_order_independent_root` | M1.14 is the first failing test of the milestone (empty trie root). M1.12→M1.13→M1.14 strictly sequential; M1.15/M1.16 follow. |
| **W3 — proofs + sync (parallel after W2)** | M1.17 single proof (`golden::merkledb_proof`), M1.18 range/change proof (`golden::range_proof`), M1.19 sync proto + `SyncDb` trait + Syncer | Depend on M1.14/M1.15. M1.18 depends on M1.17. M1.19 depends on M1.18 + the M0 sync proto build. |
| **W4 — Firewood + append stores (parallel)** | M1.20 firewood wiring (SHA feature, `SyncDb`), M1.21 firewood ethhash + `golden::firewood_ethhash_root`, M1.22 blockdb + `prop::blockdb_roundtrip`, M1.23 archivedb | M1.22/M1.23 depend only on M1.1. M1.21 depends on M1.20. |
| **W5 — migration (R2)** | M1.24 R2 Go-data-dir import tool (scope + leveldb/pebble readers + verify) | Depends on M1.4 (rocksdb), M1.14 (merkledb roots for `--verify roots`). |
| **W6 — gate** | M1.25 fuzz target (merkledb op-stream), M1.26 milestone exit gate | M1.26 last; runs all named exit tests + workspace build/clippy + updates PORTING.md. |

---

## Tasks

### Task M1.1: `Database` trait family, sentinel errors, shared helpers ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M0 (ava-types `Id`/`Maybe`, ava-codec, ava-crypto SHA-256)  ·  **Spec:** 04 §1.1–§1.4, 00 §11.1.3 (sentinels), 15 §3.4 (Error enum maps to rpcdb), 27 §6.1 (Closed/NotFound are control flow, not poison)
**Files:**
- Create: `crates/ava-database/Cargo.toml`, `crates/ava-database/src/lib.rs`, `crates/ava-database/src/error.rs`, `crates/ava-database/src/traits.rs`, `crates/ava-database/src/batch.rs`, `crates/ava-database/src/helpers.rs`
- Test: `crates/ava-database/src/helpers.rs` (`#[cfg(test)]` module), `crates/ava-database/tests/sentinels.rs`

- [ ] **Step 1 — Red:** Write `unit::helpers_byte_exact` asserting `put_u64(0x0102030405060708)` == `[1,2,3,4,5,6,7,8]` (big-endian, Go `PackUInt64`), `put_bool(true) == [0x01]`, `get_bool(&[0x01]) == Ok(true)`, and `unit::error_variants` asserting `Error::NotFound.to_string() == "not found"` and `Error::Closed.to_string() == "closed"` (04 §1.3 `#[error(...)]` strings, byte-exact with `database/errors.go`).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --lib helpers_byte_exact error_variants` → expect compile error (`error.rs`/`helpers.rs` missing).
- [ ] **Step 3 — Green:** Implement per 04 §1.3: `pub enum Error { #[error("closed")] Closed, #[error("not found")] NotFound, #[error(transparent)] Other(#[from] anyhow::Error) }` + `pub type Result<T>`. Implement the trait set verbatim from 04 §1.3: `KeyValueReader`, `KeyValueWriter`, `KeyValueDeleter`, `Compacter`, `Iterator` (with `release()` default), `Iteratee` (GAT `Iter<'a>`), `Batch: KeyValueWriter+KeyValueDeleter`, `WriteDelete`, `Batcher`, the full `Database: ... + Send + Sync { fn close; fn health_check -> Result<serde_json::Value> }`, the object-safe `DynDatabase` + `pub type BoxIter<'a>` (04 §1.3 object-safety note). Add `BatchOps` recorder (`Vec<BatchOp{key,value,delete}>` + `size` accounting + `MaxExcessCapacityFactor=4`/`CapacityReductionFactor=2` reset shrink, 04 §1.4) in `batch.rs`. Implement `helpers.rs` free fns byte-exact: `put_id/get_id`, `put_u64/get_u64` (8-byte BE), `put_u32/get_u32`, `put_bool/get_bool` (`0x00`/`0x01`), `put_timestamp/get_timestamp` (Go `time.Time.MarshalBinary`), `with_default`, `count`, `size` (`kvPairOverhead=8`), `clear[_prefix]`/`atomic_clear[_prefix]`. License header on every file; `#![forbid(unsafe_code)]` in `lib.rs`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --lib helpers_byte_exact error_variants` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: Database trait family, sentinel errors, byte-exact helpers (04 §1)"`

### Task M1.2: dbtest conformance battery + proptest BTreeMap oracle (`testutil` feature) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1  ·  **Spec:** 04 §6.1, 02 §7.2, 02 §13.3 (every backend MUST pass)
**Files:**
- Create: `crates/ava-database/src/dbtest.rs` (gated `#[cfg(feature = "testutil")]`), `crates/ava-database/Cargo.toml` (`testutil` feature + dev `proptest`, `tempfile`)
- Test: a temporary inline `#[cfg(test)]` self-check that runs the suite against a stub in-vec map (replaced by memdb in M1.3)

- [ ] **Step 1 — Red:** Define the public battery signatures from 02 §7.2: `pub fn run_database_suite<D: Database, F: Fn() -> D>(new: F)` and `pub fn run_database_proptests<D: Database, F: Fn() -> D + Clone>(new: F)`. Inside, port each Go `dbtest` test as a private fn: `simple_key_value`, `overwrite`, `empty_key`, `key_empty_value` (nil⇔empty: `get` of empty-valued key returns `Ok(vec![])`, 04 §1.1), `*_closed` (post-close ops return `Err(Error::Closed)`), `memory_safety_get`/`memory_safety_put` (mutate args after call), `batch_put_delete`/`batch_inner`/`batch_replay`/`batch_large_size`, `iterator`/`iterator_start`/`iterator_prefix`/`iterator_snapshot`/`iterator_error_after_release`, `compact`, `clear`/`clear_prefix`, `modify_value_after_put`, `concurrent_batches`, `many_small_concurrent_kv_batches`, `put_get_empty` (04 §6.1 full list). The proptest body builds `arb_db_op()` (Put/Delete/Get/Has/Iterate) and asserts a full-scan `dump(&db)` equals a `BTreeMap<Vec<u8>,Vec<u8>>` oracle (02 §7.2 sketch). Write `prop::db_oracle_btreemap` as the public proptest entry name the exit gate calls.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --lib dbtest` → expect failure: no concrete backend yet to instantiate (compiles, asserts unimplemented stub).
- [ ] **Step 3 — Green:** Implement all battery bodies against the trait surface only (backend-agnostic). Commit the `proptest-regressions/` dir (empty placeholder) per 02 §4.1. The suite must be a *library of helpers*, not a test binary (02 §3.3).
- [ ] **Step 4 — Confirm green:** Run `cargo build -p ava-database --features testutil` → PASS (full green proven by M1.3 onward).
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: dbtest conformance battery + BTreeMap oracle proptest (testutil, 02 §7.2)"`

### Task M1.3: `memdb` backend (BTreeMap) — reference backend ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2  ·  **Spec:** 04 §2.2
**Files:**
- Create: `crates/ava-database/src/memdb.rs`
- Test: `crates/ava-database/tests/conformance_memdb.rs`

- [ ] **Step 1 — Red:** Write `conformance::run_database_suite` (test name) in `conformance_memdb.rs` calling `ava_database::dbtest::run_database_suite(MemDb::new)` and `prop::db_oracle_btreemap` calling `run_database_proptests(MemDb::new)`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_memdb` → expect compile error (`MemDb` missing).
- [ ] **Step 3 — Green:** Implement `MemDb` per 04 §2.2: `parking_lot::RwLock<Option<BTreeMap<Vec<u8>, Vec<u8>>>>` (the `Option` models Go `db == nil` after `Close`; post-close ops → `Error::Closed`). `get` clones the value (memory-safety contract). Iterators snapshot the relevant range into a `Vec` so they are independent of later mutation (`TestIteratorSnapshot`). Implement both `Database` (typed GAT iterator) and `DynDatabase` (blanket/thin impl). nil⇔empty rules from 04 §1.1.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_memdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: memdb backend passing dbtest + oracle (04 §2.2)"`

### Task M1.4: `rocksdb` backend (on-disk default, replaces leveldb + pebble) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2  ·  **Spec:** 04 §2.1, 00 §4.4
**Files:**
- Create: `crates/ava-database/src/rocksdb.rs`, audited `unsafe` wrapper module note
- Test: `crates/ava-database/tests/conformance_rocksdb.rs`

- [ ] **Step 1 — Red:** Write `conformance::run_database_suite` + `prop::db_oracle_btreemap` in `conformance_rocksdb.rs` using `|| RocksDb::open(tempfile::tempdir()?.path())` and a `RocksDb::open_temp()` helper (02 §7.2 example).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_rocksdb` → expect compile error (`RocksDb` missing).
- [ ] **Step 3 — Green:** Wrap `rust-rocksdb` per 04 §2.1. `get` → `Error::NotFound` on `Ok(None)`; `has` via `get_pinned().is_some()` (zero-copy). Iterators: `DBRawIterator` + `set_mode(From(start, Forward))` AND a wrapper-applied `start ≥` + `HasPrefix` predicate (RocksDB prefix-seek alone insufficient, 04 §2.1); hold a RocksDB **snapshot** for point-in-time semantics (`TestIteratorSnapshot`). Batch = `WriteBatch`, atomic `write()`. `compact` = `compact_range`. Close gated by `AtomicBool` so post-close ops return `Error::Closed` (not a panic). Open options (block cache, write buffer, max open files, bloom, LZ4/Snappy, level compaction) exposed via a `RocksDbConfig` mirroring Go JSON DB-config keys (perf knobs, not protocol). `health_check` → JSON blob of `rocksdb.estimate-live-data-size`. Since this is the one `unsafe`-permitted backend (FFI), isolate `rust-rocksdb` calls and document `// SAFETY:` per 00 §7.6.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_rocksdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: rocksdb backend (snapshot iterators, atomic batch) passing dbtest (04 §2.1)"`

### Task M1.5: `prefixdb` backend (SHA-256 namespacing, byte-exact) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3, M0 (ava-crypto SHA-256)  ·  **Spec:** 04 §2.3, 04 §10.1, 04 §6.5 (encoding golden)
**Files:**
- Create: `crates/ava-database/src/prefixdb.rs`
- Test: `crates/ava-database/tests/conformance_prefixdb.rs`, `crates/ava-database/tests/golden_prefix.rs`, `tests/vectors/prefix/*.json`

- [ ] **Step 1 — Red:** Write `golden::prefix_namespacing` asserting `make_prefix(b"vm") == SHA256(b"vm")` and `join_prefixes(make_prefix(b"a"), b"b") == SHA256(SHA256(b"a") ‖ b"b")` against a committed Go vector (04 §10.1). Add `conformance::run_database_suite` over `PrefixDb::new(b"test", MemDb::new())`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test golden_prefix` → expect compile error (`PrefixDb`/`make_prefix` missing).
- [ ] **Step 3 — Green:** Implement per 04 §2.3 + §10.1: `MakePrefix(prefix) = SHA256(prefix)` (32-byte hashed prefix); `New` on an already-`PrefixDb` *joins* via `JoinPrefixes(parent32, child) = SHA256(parent32 ‖ child)`; `NewNested` always `SHA256(prefix)` (no compression). On-disk key = `prefix32 ‖ key`. `dbLimit = increment(prefix)` for range-bounded `compact`. Iterators strip the prefix from returned keys. Reuse a byte-buffer pool (mirror Go `utils.BytesPool`). Provide a Go-extracted vector under `tests/vectors/prefix/`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test golden_prefix --test conformance_prefixdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: prefixdb SHA-256 namespacing (MakePrefix/JoinPrefixes byte-exact) (04 §2.3)"`

### Task M1.6: `versiondb` backend (in-memory overlay + merge iterator + commit batch) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3  ·  **Spec:** 04 §2.4, 27 §2.2/§2.3 (CommitBatch returns unwritten batch)
**Files:**
- Create: `crates/ava-database/src/versiondb.rs`
- Test: `crates/ava-database/tests/conformance_versiondb.rs`, inline `#[cfg(test)]` merge-iterator unit tests

- [ ] **Step 1 — Red:** Write `unit::merge_iterator_ties` asserting the merge walks sorted `mem` snapshot + base iterator preferring `mem` on key ties and skipping tombstones (04 §2.4 `Next()` cases), and `conformance::run_database_suite` over `VersionDb::new(MemDb::new())`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_versiondb` → expect compile error (`VersionDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §2.4: `mem: HashMap<Vec<u8>, ValueOrDelete>` overlay; reads consult `mem` first (tombstone ⇒ `Error::NotFound`) else base; `put`/`delete` touch only `mem`. `commit()` flushes `mem` into one base batch, writes atomically, clears `mem`; `abort()` clears `mem`. Expose `commit_batch()` (return unwritten batch — used by the CC-ATOMIC merge, 27 §2.2/§2.3) and `set_database`/`get_database`. Port the exact merge-iterator state machine (exhausted-mem, exhausted-base, `memKey<dbKey`, `dbKey<memKey`, equal). Note: keys are passthrough (no key rewrite — 04 §10.1).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_versiondb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: versiondb overlay + merge iterator + commit_batch (04 §2.4, 27 §2)"`

### Task M1.7: `meterdb` backend (Prometheus wrapper) + metrics-name golden ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3  ·  **Spec:** 04 §2.5, 02 §6 (metrics-name golden), 00 §7.3
**Files:**
- Create: `crates/ava-database/src/meterdb.rs`
- Test: `crates/ava-database/tests/conformance_meterdb.rs`, `crates/ava-database/tests/golden_meterdb_metrics.rs`, `tests/vectors/meterdb/metric_names.json`

- [ ] **Step 1 — Red:** Write `golden::meterdb_metric_names` asserting the registered metric + label set (`method` ∈ `{has,get,put,delete,new_batch,new_iterator,compact,close,health_check,batch_*,iterator_*}`) matches the committed Go metric-name vector (04 §2.5). Add `conformance::run_database_suite` over `MeterDb::new(MemDb::new(), &registry)`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test golden_meterdb_metrics` → expect compile error (`MeterDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §2.5: wraps any `Database`, times+counts every method with `prometheus` histograms/counters under the exact Go metric names/labels. Key-passthrough (04 §10.1). Provide the Go-extracted metric-name vector.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test golden_meterdb_metrics --test conformance_meterdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: meterdb prometheus wrapper + metric-name golden (04 §2.5)"`

### Task M1.8: `corruptabledb` backend (poison-on-error) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3  ·  **Spec:** 04 §2.6, 27 §6.1
**Files:**
- Create: `crates/ava-database/src/corruptabledb.rs`
- Test: `crates/ava-database/tests/conformance_corruptabledb.rs`, inline `#[cfg(test)]` poison test

- [ ] **Step 1 — Red:** Write `unit::poison_latches_on_other` asserting that after one `Error::Other(_)` from the inner DB, every subsequent op returns the latched error, while `Error::Closed`/`Error::NotFound` do NOT latch (27 §6.1). Add `conformance::run_database_suite` over `CorruptableDb::new(MemDb::new())`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_corruptabledb` → expect compile error (`CorruptableDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §2.6 / 27 §6.1: `parking_lot::RwLock<Option<Error>>`; `handle_error` latches only on `Error::Other(_)`; `check()` returns the latched error before every op. Use a test-only failpoint inner DB to inject `Error::Other`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_corruptabledb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: corruptabledb poison-on-error (latch Other only) (04 §2.6, 27 §6.1)"`

### Task M1.9: `linkeddb` backend (in-DB doubly-linked list, linearcodec nodes) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3, M0 (ava-codec linearcodec)  ·  **Spec:** 04 §2.7, 04 §10.6 (node codec byte-exact)
**Files:**
- Create: `crates/ava-database/src/linkeddb.rs`
- Test: `crates/ava-database/tests/golden_linkeddb.rs`, inline LIFO iteration unit tests, `tests/vectors/linkeddb/*.json`

- [ ] **Step 1 — Red:** Write `golden::linkeddb_node_codec` asserting a node `{value,hasNext,next,hasPrevious,previous}` serializes (via linearcodec) to the committed Go bytes, and that the head pointer lives at key `0x01` (04 §2.7, §10.6).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test golden_linkeddb` → expect compile error (`LinkedDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §2.7: `headKey = 0x01`; nodes carry `value/hasNext/next/hasPrevious/previous` serialized with the M0 linearcodec (these bytes are persisted — byte-exact, 04 §10.6). LRU caches (`lru` crate) for head-key + nodes, an `updatedNodes` staging map, and the batch write of head + touched nodes. Provide LIFO `Iterator`. Provide a Go-extracted node-codec vector.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test golden_linkeddb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: linkeddb (linearcodec nodes, LIFO iterator, LRU caches) (04 §2.7)"`

### Task M1.10: `heightindexdb` (`HeightIndex` trait + memdb/meterdb backends) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.7  ·  **Spec:** 04 §2.9, 04 §6.1 (own dbtest battery)
**Files:**
- Create: `crates/ava-database/src/heightindex.rs`
- Test: `crates/ava-database/tests/conformance_heightindex.rs`

- [ ] **Step 1 — Red:** Write `conformance::run_heightindex_suite` (battery name) over a memdb-backed `HeightIndex` asserting `put(h,v)`/`get(h)`→`Ok(v)`, `get(missing)`→`Err(NotFound)`, `has`, `sync(start,end)`, and `*_closed`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_heightindex` → expect compile error (`HeightIndex` missing).
- [ ] **Step 3 — Green:** Implement `pub trait HeightIndex { fn put(&self,height:u64,value:&[u8]); fn get(&self,height:u64)->Result<Vec<u8>>; fn has; fn sync(start,end); fn close }` (04 §2.9 verbatim). Backends: `HeightIndexMemDb` (`HashMap<u64,Vec<u8>>`) and `HeightIndexMeterDb` (Prometheus wrapper). Add `run_heightindex_suite` to `dbtest.rs` (its own battery, 04 §6.1 / `heightindexdb/dbtest`).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_heightindex` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: HeightIndex trait + memdb/meterdb backends + battery (04 §2.9)"`

### Task M1.11: `rpcdb` client/server (Database over gRPC, tonic) ✅ COMPLETED
**Crate:** `ava-database`  ·  **Depends on:** M1.1, M1.2, M1.3, M0 (proto build)  ·  **Spec:** 04 §2.8, 15 §3.4 (rpcdb.proto), 02 §6.1
**Files:**
- Create: `crates/ava-database/src/rpcdb/mod.rs`, `crates/ava-database/src/rpcdb/client.rs`, `crates/ava-database/src/rpcdb/server.rs`, `crates/ava-database/build.rs` (or shared proto build per M0)
- Test: `crates/ava-database/tests/conformance_rpcdb.rs`

- [ ] **Step 1 — Red:** Write `conformance::run_database_suite` over a `DatabaseClient` talking to an in-process `DatabaseServer` wrapping `MemDb` over an in-process tonic channel (02 §6.1 / 04 §6.1 "rpcdb client↔server").
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features testutil --test conformance_rpcdb` → expect compile/link error (rpcdb types missing).
- [ ] **Step 3 — Green:** Generate from `proto/rpcdb/rpcdb.proto` (15 §3.4). `DatabaseClient` implements `Database`/`DynDatabase` by calling the tonic client; map the `Error` enum (`ERROR_CLOSED`/`ERROR_NOT_FOUND`) back to `Error::Closed`/`Error::NotFound` (`ErrEnumToError`); transport errors → `Error::Other`. Iterators are server-side handles addressed by id; `IteratorNext` batches multiple pairs per RPC (port the Go batching). Client batch buffers `BatchOp`s → one `WriteBatch`. `DatabaseServer` wraps a host `Database`; maps `Error::Closed/NotFound` → the enum (`ErrorToErrEnum`), other errors → gRPC errors; holds an iterator registry keyed by id. Map `bytes` proto fields to `bytes::Bytes` (15 §5).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features testutil --test conformance_rpcdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: rpcdb tonic client/server over rpcdb.proto, passing dbtest (04 §2.8, 15 §3.4)"`

### Task M1.12: `ava-merkledb` Key / Path (bit-path over branch factor) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.1, M0 (ava-types, ava-codec)  ·  **Spec:** 04 §3.2
**Files:**
- Create: `crates/ava-merkledb/Cargo.toml`, `crates/ava-merkledb/src/lib.rs`, `crates/ava-merkledb/src/key.rs`
- Test: `crates/ava-merkledb/src/key.rs` (`#[cfg(test)]`), `crates/ava-merkledb/tests/golden_key.rs`, `tests/vectors/merkledb/keys/*.json`

- [ ] **Step 1 — Red:** Write `unit::token_extraction` + `golden::key_pack` asserting `token(bit_index, token_size)` bit-extraction, `has_prefix`/`iterated_has_prefix`, `skip`/`take`, longest-common-prefix, and the partial-byte zero-padding rule produce the committed Go `key.go` vectors for `BranchFactor256` (production default) and at least one of 2/4/16.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test golden_key` → expect compile error (`Key`/`BranchFactor` missing).
- [ ] **Step 3 — Green:** Port `x/merkledb/key.go` per 04 §3.2: `pub struct Key { value: Bytes, length: usize /* bits */ }`, `pub enum BranchFactor { Two, Four, Sixteen, TwoFiftySix }` (token sizes 1/2/4/8 bits). Implement `token`, `to_token`, `has_prefix`, `iterated_has_prefix`, `skip`, `take`, `longest_common_prefix`, and partial-byte zero-padding. Use `Bytes`/`&[u8]` instead of Go's unsafe string aliasing.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test golden_key` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: Key/Path bit-path over BranchFactor (byte-exact key.go) (04 §3.2)"`

### Task M1.13: node model + on-disk codec (`encodeDBNode`, byte-exact) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.12  ·  **Spec:** 04 §3.3, 04 §10.8 (on-disk node key spaces), 02 §6
**Files:**
- Create: `crates/ava-merkledb/src/node.rs`, `crates/ava-merkledb/src/codec.rs`
- Test: `crates/ava-merkledb/tests/golden_node_codec.rs`, `tests/vectors/merkledb/nodes/*.json`

- [ ] **Step 1 — Red:** Write `golden::node_codec_encode` (encode a hand-built `DbNode` → committed Go bytes) and `golden::node_codec_decode_rejects` asserting decode rejects: child index ≥ branch factor, too-many-children, int overflow, trailing bytes (`errExtraSpace`), non-zero key padding (`errNonZeroKeyPadding`), leading-zero uvarint (`errLeadingZeroes`) (04 §3.3 — these error conditions are part of the conformance surface).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test golden_node_codec` → expect compile error (`DbNode`/`encode_db_node` missing).
- [ ] **Step 3 — Green:** Port per 04 §3.3: `struct DbNode { value: Maybe<Bytes>, children: BTreeMap<u8, Child> }`, `struct Child { compressed_key: Key, id: Id, has_value: bool }`, `struct Node { db_node, key, value_digest }`. `value_digest` = value if `len < 32` else `HashValue(value)` (`setValueDigest`). `encode_db_node`: `MaybeBytes(value)`, `Uvarint(num_children)`, then per child **in ascending index order** `Uvarint(index)`, `Key(compressed_key)`, `ID(child_id)`, `Bool(has_value)`. Primitives byte-exact: `Bool`=1 byte, `Uvarint`=`binary.PutUvarint` with no-leading-zeroes decode check, `Bytes`/`Key` uvarint-length-prefixed, `Key` packs bit-length + `bytesNeeded(length)` with partial-byte-zero-padded rule. Enforce all decode rejections. Document the three on-disk node key spaces (`metadataPrefix=0x00`, `valueNodePrefix=0x01`, `intermediateNodePrefix=0x02`, 04 §10.8) for later DB wiring.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test golden_node_codec` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: node model + encodeDBNode byte-exact + decode rejections (04 §3.3)"`

### Task M1.14: hashing + `golden::merkledb_root` (TDD ANCHOR — empty → single-key → multi) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.13  ·  **Spec:** 04 §3.4 (HashNode), 00 §11.1.4 (SHA-256 default), 02 §6.3
**Files:**
- Create: `crates/ava-merkledb/src/hashing.rs`
- Test: `crates/ava-merkledb/tests/golden_root.rs`, `tests/vectors/merkledb/roots/{empty,single_key,multi}.json`

- [ ] **Step 1 — Red (FIRST FAILING TEST OF THE MILESTONE):** Write `golden::merkledb_root` with the smallest cases first — (1) the EMPTY trie hashes to `ids::EMPTY` (04 §3.4); (2) a SINGLE-KEY trie `{b"k" => b"v"}` hashes to the committed Go root ID; (3) a small multi-key set → Go root. Assert `hex::encode(merkle_root(kvs)) == vector.expected` per case (02 §6.1).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test golden_root` → expect failure: assertion mismatch / `Hasher`/`hash_node` unimplemented (the *right* failure — not a compile error once the test struct is in place).
- [ ] **Step 3 — Green:** Implement `pub trait Hasher` + the protocol-fixed SHA-256 `DefaultHasher` per 04 §3.4. `hash_node` feeds the hasher in this exact order: (1) `Uvarint(num_children)`; (2) per child **ascending byte-index** `Uvarint(index)` then the child's 32-byte `id`; (3) value digest present ⇒ `0x01`, `Uvarint(len(digest))`, digest bytes, else `0x00`; (4) `Uvarint(key.length)` then `key.bytes()`. `HashValue(v)=SHA-256(v)`, `HashLength=32`. Root ID = `hash_node(root)`; empty trie ⇒ `ids::EMPTY`. Provide a minimal in-memory trie builder sufficient to compute roots for fixed K/V sets (full DB-backed view comes in M1.15). Extract the three Go root vectors.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test golden_root` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: HashNode SHA-256 byte-exact; golden::merkledb_root empty/single/multi (04 §3.4)"`

### Task M1.15: View/TrieView, history, node stores ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.14, M1.3/M1.4 (a base `Database`)  ·  **Spec:** 04 §3.5, 27 §4.1 (cleanShutdown rebuild), 04 §10.8
**Files:**
- Create: `crates/ava-merkledb/src/view.rs`, `crates/ava-merkledb/src/history.rs`, `crates/ava-merkledb/src/db.rs`
- Test: `crates/ava-merkledb/tests/view.rs`, inline `#[cfg(test)]` validity/commit tests

- [ ] **Step 1 — Red:** Write `unit::commit_invalidates_siblings` (committing a view invalidates sibling views + descendants → `ErrInvalid`; a view commits only if parent is the DB and only once), `unit::view_layering_equals_direct` (applying changes through a view equals direct application), and `unit::clean_shutdown_rebuild` (open with `metadataPrefix→cleanShutdown` missing/false ⇒ rebuild intermediate nodes from value nodes, 27 §4.1).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test view` → expect compile error (`View`/`MerkleDb` missing).
- [ ] **Step 3 — Green:** Port per 04 §3.5: `View`/`TrieView` = immutable proposal layering changes over a parent (DB or another view); lazily compute node IDs/root. Validity model: `Arc`-linked parent pointers + `AtomicBool` validity + `arc_swap` committed-root swap; committing invalidates siblings + descendants. `history` = bounded ring of recent change-sets keyed by root ID (trim/size bound). Two node stores: `intermediate_node_db` (LRU-cached, hashed-key) + `value_node_db` over a base `Database`, distinguished by the 1-byte prefixes of 04 §10.8; `bytes` pool + `lru` cache. On open, honor the `cleanShutdown` flag (rebuild intermediate from value nodes if unclean — 27 §4.1). Parallelize independent-subtrie hashing via a rayon scope (safe: hashing is pure, 02 §4.2).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test view` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: View/TrieView + history + node stores + cleanShutdown rebuild (04 §3.5, 27 §4.1)"`

### Task M1.16: `prop::merkle_order_independent_root` (proptest invariants) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.14, M1.15  ·  **Spec:** 02 §4.2 (merkledb properties)
**Files:**
- Create: `crates/ava-merkledb/tests/prop_merkle.rs`, `crates/ava-merkledb/proptest-regressions/` (committed)
- Test: same

- [ ] **Step 1 — Red:** Write `prop::merkle_order_independent_root`: for a random K/V set, inserting in any permutation yields the **same** root; `root(after delete-all) == ids::EMPTY`; `view_layering == direct application`. Also a BTreeMap-oracle property: `get` after a random op sequence equals the oracle (02 §4.2).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test prop_merkle` → expect failure if any order-dependence/HashMap-on-serialization leaks (00 §6.1); otherwise confirm by temporarily shuffling child iteration to prove the test catches it, then revert.
- [ ] **Step 3 — Green:** Ensure determinism: child maps are `BTreeMap` (ascending index), no `HashMap` on the hashing path (00 §6.1), checked arithmetic. Commit the `proptest-regressions/` corpus (02 §4.1).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test prop_merkle` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: prop::merkle_order_independent_root + oracle invariants (02 §4.2)"`

### Task M1.17: single proof (`golden::merkledb_proof`, inclusion/exclusion) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.14, M1.15  ·  **Spec:** 04 §3.6 (single proof), 15 §3.10 (proto envelope), 02 §6.3
**Files:**
- Create: `crates/ava-merkledb/src/proof.rs`
- Test: `crates/ava-merkledb/tests/golden_proof.rs`, `tests/vectors/merkledb/proofs/*.json`, `tests/vectors/sync/proof/*.json`

- [ ] **Step 1 — Red:** Write `golden::merkledb_proof`: build the trie for a fixed K/V set, generate an inclusion proof and an exclusion proof, assert the proto-encoded `Proof` bytes equal the committed Go vector (15 §3.10 envelope), and that `verify(proof, expected_root)` accepts valid and rejects a tampered proof.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test golden_proof` → expect compile error (`Proof`/`ProofNode` missing).
- [ ] **Step 3 — Green:** Port per 04 §3.6: `pub struct ProofNode { key: Key, value_or_hash: Maybe<Bytes>, children: BTreeMap<u8, Id> }`, `pub struct Proof { path: Vec<ProofNode>, key: Key, value: Maybe<Bytes> }`. `ProofNode.value_or_hash` = value if `len < 32` else its hash. Verify by rebuilding a trie from `path` and checking recomputed root == expected root. Proto marshalers per `proto/sync` (15 §3.10): `ProofNode{key:Key, value_or_hash:MaybeBytes, children:map<uint32,bytes>}`, `Key{length:uint64,value:bytes}`, `MaybeBytes{value:bytes}` (presence = "something"). Extract Go vectors into both `tests/vectors/merkledb/proofs/` and `tests/vectors/sync/proof/`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test golden_proof` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: single Proof (inclusion/exclusion) + sync proto envelope golden (04 §3.6, 15 §3.10)"`

### Task M1.18: range proof + change proof (`golden::range_proof`) ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.17  ·  **Spec:** 04 §3.6 (RangeProof/ChangeProof), 15 §3.10, 02 §6.3
**Files:**
- Modify: `crates/ava-merkledb/src/proof.rs`
- Test: `crates/ava-merkledb/tests/golden_range_proof.rs`, `crates/ava-merkledb/tests/prop_proof.rs`, `tests/vectors/merkledb/range-proofs/*.json`

- [ ] **Step 1 — Red:** Write `golden::range_proof`: build a trie, produce a `RangeProof` for `[start,end]`, assert proto bytes equal the committed Go vector and that verification (build trie from `key_values` + insert boundary nodes + check root) accepts valid and rejects tampered. Add `prop::proof_verify_accepts_valid_rejects_tampered` (proptest random tries + random ranges; also exercises `ChangeProof`).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test golden_range_proof` → expect compile error (`RangeProof`/`ChangeProof` missing).
- [ ] **Step 3 — Green:** Port per 04 §3.6: `pub struct KeyValue`, `pub struct RangeProof { start_proof, end_proof, key_values }`, `pub struct KeyChange { key, value: Maybe<Bytes> }`, `pub struct ChangeProof { start_proof, end_proof, key_changes }`. RangeProof verify: build trie from `key_values`, insert `start_proof`/`end_proof` boundary nodes, check root; invariants (sorted, no gaps, omit-from-end-on-truncation) ported verbatim. ChangeProof: `KeyChange.value = Nothing` ⇒ deletion; verify by applying changes to a local (partial) trie and checking root == `expected_end_root`; all ordering/subset invariants enforced. Proto per 15 §3.10. Commit `proptest-regressions/`. Verification fans out over rayon (independent subtries — 19 §9, safe per 02).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test golden_range_proof --test prop_proof` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: RangeProof + ChangeProof verify + golden::range_proof + proptest (04 §3.6)"`

### Task M1.19: state-sync protocol — `SyncDb` trait, proto, Syncer + work-heap ✅ COMPLETED
**Crate:** `ava-merkledb`  ·  **Depends on:** M1.18, M0 (proto build for `proto/sync`)  ·  **Spec:** 04 §3.7, 19 §4, 15 §3.10
**Files:**
- Create: `crates/ava-merkledb/src/sync/mod.rs`, `crates/ava-merkledb/src/sync/db.rs` (`SyncDb` trait), `crates/ava-merkledb/src/sync/workheap.rs`, `crates/ava-merkledb/src/sync/syncer.rs`, `crates/ava-merkledb/src/sync/proto.rs`
- Test: `crates/ava-merkledb/tests/sync_roundtrip.rs`, `crates/ava-merkledb/tests/prop_workheap.rs`, `tests/vectors/sync/wire/*.json`

- [ ] **Step 1 — Red:** Write `golden::sync_proof_wire` asserting `ProofRequest`/`ProofResponse` proto frames (15 §3.10: `ChangeProofRequest`/`RangeProofRequest` field tags + `MaybeBytes`) match Go-extracted vectors; `prop::sync_proof_roundtrip` (server `range_proof`/`change_proof` verifies against client and committed root equals the byte-exact target root, incl. an `UpdateSyncTarget` mid-sync that advances the root → final root is the *new* target, 19 §8); `prop::workheap_invariants` (fuzz `merge_insert`/split for range non-overlap + full coverage, 19 §8).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --test sync_roundtrip --test prop_workheap` → expect compile error (`SyncDb`/`Syncer`/`WorkHeap` missing).
- [ ] **Step 3 — Green:** Define `pub trait SyncDb` verbatim from 04 §3.7 (`merkle_root`, `change_proof`, `range_proof`, `verify_change_proof`, `commit_range_proof`, `commit_change_proof`, `clear`, assoc `RangeProof`/`ChangeProof`); impl for `ava-merkledb`. Port `Priority { Low, Med, High, Retry }`, `WorkItem`, `WorkHeap { merge_insert, get_work, keyspace_percent }` (BinaryHeap by priority + BTree by start; `merge_insert` coalesces adjacent same-root; split when spare capacity) per 19 §4.1/§4.2. `Syncer` uses `ArcSwap<Id>` target root, `Notify` (replaces `sync.Cond`), a tokio task set bounded by `SimultaneousWorkLimit`, rayon verify pool; `network_server` answers range/change requests capped by `key_limit`/`bytes_limit`. Errors: `ErrFinishedWithUnexpectedRoot`, `ErrInsufficientHistory`, `ErrNoEndRoot`, `errInvalidRangeProof`, `errInvalidChangeProof`, `errTooManyBytes` (19 §4). Generate proto from `proto/sync/sync.proto` (15 §3.10), `bytes`→`Bytes`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --test sync_roundtrip --test prop_workheap` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: SyncDb trait + Syncer/work-heap + sync proto wire golden (04 §3.7, 19 §4, 15 §3.10)"`

### Task M1.20: Firewood wiring (SHA feature, `SyncDb` impl, safe wrapper) ✅ COMPLETED
**Crate:** `ava-merkledb` (firewood binding)  ·  **Depends on:** M1.19, M0 (R3 firewood pin)  ·  **Spec:** 04 §4.1, §4.2, §4.4, 00 §11.1.4, 00 §11.2 R3
**Files:**
- Create: `crates/ava-merkledb/src/firewood/mod.rs`, `crates/ava-merkledb/src/firewood/sha.rs`, `crates/ava-merkledb/Cargo.toml` (firewood dep + `sha`/`ethhash` features)
- Test: `crates/ava-merkledb/tests/firewood_sha.rs`

- [ ] **Step 1 — Red:** Write `unit::firewood_propose_commit_roundtrip` asserting `propose([Put,Delete]).root_hash()` is available *pre-commit* (consensus votes on it, 04 §4.2), `commit()` advances the tip, and read-after-commit via `db.view().val(&key)` returns the value; and that historical `db.revision(old_root)` reads the prior value (within the retained window). Confirms the firewood crate links with SHA feature.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --features firewood --test firewood_sha` → expect link/compile error (firewood not wired).
- [ ] **Step 3 — Green:** Add the `firewood` dep with `sha` (default) feature; scope/document the R3 pinned revision in `Cargo.toml` (00 §11.2). Wrap `firewood::db::{Db, DbConfig, BatchOp}` + `v2::api` traits per 04 §4.2 sketch behind a safe module (firewood is pure Rust — no CGO; its internal `unsafe` is its own concern, 04 §4.4; keep `#![forbid(unsafe_code)]` in our wrapper). Calls run under `spawn_blocking`/a dedicated thread at the call site (04 §1.2). Implement `SyncDb` for the Firewood path (04 §3.7 — proves protocol reuse; `EmptyRoot` = firewood default empty root). Configure bounded revision retention to cover the reorg/sync window.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --features firewood --test firewood_sha` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: firewood wiring (SHA feature) + SyncDb impl + safe wrapper (04 §4, R3)"`

### Task M1.21: Firewood `ethhash` feature + `golden::firewood_ethhash_root` ✅ COMPLETED
**Crate:** `ava-merkledb` (firewood binding)  ·  **Depends on:** M1.20  ·  **Spec:** 04 §4.1 (ethhash = Keccak/Eth-MPT), 00 §11.1.4, 15 §6 (EVM state root YES), 02 §6
**Files:**
- Create: `crates/ava-merkledb/src/firewood/ethhash.rs`
- Test: `crates/ava-merkledb/tests/golden_firewood_ethhash.rs`, `tests/vectors/firewood/ethhash/*.json`

- [ ] **Step 1 — Red:** Write `golden::firewood_ethhash_root`: feed a fixed batch (RLP-encoded accounts at the account depth + storage slots) into a Firewood instance with `features=["ethhash"]`, assert the resulting state root equals the committed Go EVM root vector (from `firewood-go-ethhash` bindings on the identical batch — 04 §6.6); include the empty-trie case == `types.EmptyRootHash` (04 §4.1).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-merkledb --features firewood-ethhash --test golden_firewood_ethhash` → expect failure: feature/root mismatch.
- [ ] **Step 3 — Green:** Enable the firewood `ethhash` feature (Keccak-256 + Ethereum-MPT/RLP, account = node at fixed depth, 04 §4.1). Expose an `EthHashDb` view that takes RLP-account/storage `BatchOp`s and yields the EVM state root via `propose().root_hash()`. Extract Go ethhash root vectors. (Full reth/revm `StateProvider` adaptation is M-EVM scope per 04 §4.3 — here we only prove root parity.)
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-merkledb --features firewood-ethhash --test golden_firewood_ethhash` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: firewood ethhash feature + golden::firewood_ethhash_root vs Go EVM root (04 §4.1)"`

### Task M1.22: `ava-blockdb` (append-optimized height-indexed block store) + `prop::blockdb_roundtrip` ✅ COMPLETED
**Crate:** `ava-blockdb`  ·  **Depends on:** M1.1  ·  **Spec:** 04 §5.1, 27 §4.1/§5.1 (torn-write recovery scan), 02 §6
**Files:**
- Create: `crates/ava-blockdb/Cargo.toml`, `crates/ava-blockdb/src/lib.rs`, `crates/ava-blockdb/src/index.rs`, `crates/ava-blockdb/src/data.rs`, `crates/ava-blockdb/src/recovery.rs`
- Test: `crates/ava-blockdb/tests/prop_roundtrip.rs`, `crates/ava-blockdb/tests/recovery.rs`, `crates/ava-blockdb/tests/golden_format.rs`, `tests/vectors/blockdb/*.json`

- [ ] **Step 1 — Red:** Write `prop::blockdb_roundtrip`: write N blocks at arbitrary (incl. out-of-order) heights, read each back byte-identical; `golden::blockdb_header_layout` asserting the 64-byte index header `{version,max_data_file_size,min_height,max_height,next_write_offset,reserved[24]}` + 16-byte entries `{data_offset:u64,block_size:u32,reserved:u32}` and the 22-byte data entry header `{height:u64,size:u32,checksum:u64,version:u16}` match Go byte widths/endianness (04 §5.1); `unit::recovery_rebuilds_index` simulating `data_file_size > indexed_size` (torn write) and asserting the scan from `next_write_offset` validating header+checksum rebuilds the index identically to Go (27 §5.1).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-blockdb` → expect compile error (`BlockDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §5.1: one `.idx` file (64-byte header + fixed 16-byte entries → O(1) seek by height) and multiple `.dat` files (22-byte entry header + raw bytes, split at `max_data_file_size`). Recovery scan on open (27 §4.1/§5.1). Durability: `sync_to_disk` fsync after each write + index fsync every `checkpoint_interval`, else OS buffering. Use positioned `pread`/`pwrite` (`std::os::unix::fs::FileExt`) under an atomic `next_write_offset` (RwLock-free), `lru` block cache, `zstd` compression, and the Go-matching checksum (`crc`/`xxhash` — verify which against a vector). Per-file handles so reads don't block writes.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-blockdb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-blockdb: append-optimized block store + recovery scan + prop::blockdb_roundtrip (04 §5.1, 27 §5.1)"`

### Task M1.23: `ava-archivedb` (height-versioned KV, `^height` encoding) ✅ COMPLETED
**Crate:** `ava-archivedb`  ·  **Depends on:** M1.1, M1.3/M1.4  ·  **Spec:** 04 §5.2, 04 §6.5 (encoding golden), 04 §10.8/§10.3 (`^height` trick)
**Files:**
- Create: `crates/ava-archivedb/Cargo.toml`, `crates/ava-archivedb/src/lib.rs`, `crates/ava-archivedb/src/value.rs`
- Test: `crates/ava-archivedb/tests/golden_encoding.rs`, `crates/ava-archivedb/tests/versioned.rs`, `tests/vectors/archivedb/*.json`

- [ ] **Step 1 — Red:** Write `golden::archivedb_key_encoding` asserting a user key encodes to `uvarint(len(key)) ‖ key ‖ BigEndian(^height)` and a metadata key to `uvarint(len(key)+1) ‖ key`, byte-matching Go (04 §5.2, §6.5); `unit::reads_newest_at_or_below` asserting `open(height).get(key)` seeks `prefix ‖ ^height` forward and returns the newest version ≤ height, a tombstone ⇒ `Error::NotFound`, and `get_height` returns the set-at height.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-archivedb` → expect compile error (`ArchiveDb` missing).
- [ ] **Step 3 — Green:** Implement per 04 §5.2: `pub struct ArchiveDb<D: Database> { db: D }` with `height()`, `new_batch(height) -> ArchiveBatch`, `open(height) -> Reader`. User key = `uvarint(len)‖key‖BE(^height)` (negated height ⇒ ascending RocksDB order yields descending height, so a forward seek lands on newest ≤ target); metadata key = `uvarint(len+1)‖key` (`+1` prevents overlap; `heightKey` stores last written height). Writes buffer Put/Delete (delete = tombstone value per `value.go`) stamped with height, committed atomically. Extract Go encoding vectors. (Note 04 §5.2: EVM/SAE archival uses Firewood historical revisions; this is the generic-KV reference model.)
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-archivedb` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-archivedb: height-versioned KV (^height encoding byte-exact) (04 §5.2)"`

### Task M1.24: R2 — Go-data-dir import tool (scope + leveldb/pebble readers + verify) ✅ COMPLETED
**Crate:** `ava-database` (`migrate` module)  ·  **Depends on:** M1.4 (rocksdb), M1.14 (merkledb roots for verify)  ·  **Spec:** 04 §7, 04 §11 (full tool design), 00 §11.2 R2, 02 §10.4 (doubles as upgrade/migration test)
**Files:**
- Create: `crates/ava-database/src/migrate/mod.rs`, `crates/ava-database/src/migrate/leveldb.rs`, `crates/ava-database/src/migrate/pebble.rs` (sidecar driver), `crates/ava-database/src/migrate/verify.rs`, `crates/ava-database/docs/migration.md` (scope + document the import path; **in-place open NOT supported**)
- Test: `crates/ava-database/tests/migrate.rs`

- [ ] **Step 1 — Red:** Write `unit::migrate_preserves_bytes`: seed a `GoDbSource` stub yielding fixed `(key,value)` pairs (incl. a prefixdb-namespaced key, a `^height` archivedb key, a merkledb node key), run `migrate(src, &rocks, None)`, assert every pair reads back byte-identical from RocksDB (04 §11.4 — bytes never transformed); `unit::migrate_resumable` asserting a re-run with `--resume` past `MIGRATION_CURSOR_KEY` is a no-op; `unit::verify_roots_detects_mismatch` asserting `verify(VerifyLevel::Roots)` re-derives merkledb `merkle_root()` and fails on a corrupted copy.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-database --features migrate --test migrate` → expect compile error (`migrate`/`GoDbSource` missing).
- [ ] **Step 3 — Green:** Implement per 04 §11: `trait GoDbSource { fn iter_all() -> Box<dyn Iterator<Item=(Vec<u8>,Vec<u8>)>> }` (lexicographic, NO transformation). Impls: `RocksDbCompatSource` (rust-rocksdb opening a classic LevelDB dir — fast in-place ingest path), `RustyLevelDbSource` (`rusty-leveldb` fallback reader), `PebbleSidecarSource` (spawns a small Go `avalanchego-db-export` sidecar streaming length-prefixed pairs — only correctness-guaranteed Pebble reader; **document that in-place Pebble open is NOT supported**, 04 §7/§11.3). `migrate()` batches/`SstFileWriter` bulk-ingest with a 64 MiB flush window + `MIGRATION_CURSOR_KEY` resume checkpoint (04 §11.4). `verify(level)`: `Roots` re-reads P/X `"singleton"→"last accepted"` chain and re-opens merkledb/Firewood to check `merkle_root()` against the header root; `Full` samples random pairs back. CLI surface: `avalanchego db migrate --from --to --db-type {leveldb|pebble} [--verify {none|roots|full}] [--resume]` (wired in M12; here expose the library + document). Note §11.5 alternative (network bootstrap) in the doc. Gate behind a `migrate` feature.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-database --features migrate --test migrate` → PASS.
- [ ] **Step 5 — Commit:** `git commit -m "ava-database: R2 Go-data-dir import tool (leveldb/pebble readers, verify roots, resume) (04 §11)"`

### Task M1.25: cargo-fuzz target — merkledb op-stream parser ✅ COMPLETED
**Crate:** `ava-merkledb` (fuzz sub-crate)  ·  **Depends on:** M1.13, M1.16  ·  **Spec:** 02 §8, 02 §13.5
**Files:**
- Create: `crates/ava-merkledb/fuzz/Cargo.toml`, `crates/ava-merkledb/fuzz/fuzz_targets/op_stream.rs`, `crates/ava-merkledb/fuzz/corpus/op_stream/` (committed seeds)
- Test: the fuzz target itself (smoke run)

- [ ] **Step 1 — Red:** Write `fuzz_target!(|ops: Vec<DbOp>| ...)` (structure-aware via `#[derive(arbitrary::Arbitrary)]`) applying the op stream against the trie + a `BTreeMap` oracle, asserting no panic/over-read and that decode of arbitrary node bytes (`decode_db_node(&data)`) never panics (02 §8 merkledb target). Commit seed corpus.
- [ ] **Step 2 — Confirm red:** Run `cargo fuzz build -p ava-merkledb` → expect failure if the fuzz crate isn't wired; then the smoke run reveals any panic.
- [ ] **Step 3 — Green:** Wire the `fuzz/` sub-crate (`libfuzzer-sys` + `arbitrary`), implement the target, commit corpus seeds. Fix any panic the smoke run surfaces (the point of the target).
- [ ] **Step 4 — Confirm green:** Run `cargo xtask test-fuzz` (smoke, brief per 02 §8) → PASS (no crash).
- [ ] **Step 5 — Commit:** `git commit -m "ava-merkledb: cargo-fuzz op-stream + node-codec target + corpus (02 §8)"`

### Task M1.26: Milestone exit gate ✅ COMPLETED
**Crate:** all M1 crates  ·  **Depends on:** M1.1–M1.25  ·  **Spec:** 02 §13, 00 §1 (BUILDABLE-&-GREEN invariant)
**Files:**
- Modify: `crates/ava-database/tests/PORTING.md`, `crates/ava-merkledb/tests/PORTING.md`, `crates/ava-blockdb/tests/PORTING.md`, `crates/ava-archivedb/tests/PORTING.md`
- Create: any missing `PORTING.md` matrix (Go test → Rust counterpart → status)

- [ ] **Step 1 — Red:** Add/refresh each crate's `tests/PORTING.md` matrix (seed via `go test -list '.*'` over `database/`, `x/merkledb` (the classic path-based trie — NOT `database/merkle/`, which is firewood-backed), `database/merkle/sync` + `database/merkle/firewood/syncer`, `x/blockdb`, `x/archivedb`, 02 §10.1) and mark any remaining `wip` rows — these are the red items.
- [ ] **Step 2 — Confirm red:** Run `cargo xtask porting-report` → expect remaining `wip` rows listed (or none if fully ported).
- [ ] **Step 3 — Green:** Drive every named exit test green and resolve `wip` rows to `ported`/`na` (with reason). Confirm the full exit gate:
  - `cargo build --workspace`
  - `cargo build -p avalanchers` (binary still builds)
  - `cargo nextest run --profile ci` (incl. `conformance::run_database_suite` + `prop::db_oracle_btreemap` for **every** backend: memdb, rocksdb, prefixdb, versiondb, meterdb, corruptabledb, rpcdb, heightindex; `golden::merkledb_root`; `golden::merkledb_proof`; `golden::range_proof`; `prop::merkle_order_independent_root`; `golden::firewood_ethhash_root`; `prop::blockdb_roundtrip`)
  - `cargo clippy --workspace -- -D warnings`
  - `./target/debug/avalanchers --version` and `--help` answer correctly
- [ ] **Step 4 — Confirm green:** Run the five commands above → all PASS; `cargo xtask porting-report` shows no `wip` rows for M1 crates.
- [ ] **Step 5 — Commit:** `git commit -m "M1 storage: exit gate green (all backends + merkledb roots/proofs/sync + firewood ethhash + blockdb), PORTING.md updated"`

---

### Task M1.27: `ava-blockdb` single-process advisory file lock **[UPSTREAM DELTA — added 2026-07-06]** ⬜ TODO
**Crate:** `ava-blockdb`  ·  **Depends on:** M1.22  ·  **Spec:** 04 §5.1 upstream-delta (Go `dc350727b7` #5420)
> **Upstream parity (Go `dc350727b7`, #5420).** BlockDB now prevents multi-process access. On `New`, **before** any DB file is opened or recovery runs, it `MkdirAll`s `IndexDir` (and `DataDir` when distinct) and acquires an exclusive non-blocking `flock` (`LOCK_EX|LOCK_NB`) on a `LOCK` file in each; a second opener fails immediately with `errDatabaseInUse`. The lock releases after all files close on `Close`, and the OS reclaims it on unclean exit. Ordering invariant: **locks first, then recovery** (so two processes can't race the recovery scan and corrupt the index). The per-dir `MkdirAll` moved out of `openAndInitializeIndex`/`initializeDataFiles` into lock acquisition. Advisory only (non-cooperating tools not blocked); both dirs must be dedicated to BlockDB.
> **Rust task:** acquire the per-directory `LOCK` file lock in `Database::new` (via `fs2`/`rustix` `flock` or `File::try_lock` on Rust ≥ 1.89), hold it in the `Database` handle, drop on close; add a distinct `Error::DirectoryInUse` sentinel; move dir creation to the lock step. This is real implementation work — **not** fork-gated.
**Files (anticipated):** `crates/ava-blockdb/src/lib.rs` (+ `lock.rs` new), `crates/ava-blockdb/tests/lock.rs`.

---

## Spec coverage check

| Spec section | Subject | Task(s) | Notes |
|---|---|---|---|
| 04 §1.1–§1.3 | Database trait family, sentinel errors, iterator/batch contracts | M1.1 | |
| 04 §1.2 | sync trait + spawn_blocking at call sites | M1.1 (decision), M1.4/M1.20 (call-site blocking) | |
| 04 §1.4 | helpers (PackUInt64, timestamp, BatchOps shrink) | M1.1 | |
| 04 §2.1 | rocksdb backend | M1.4 | |
| 04 §2.2 | memdb | M1.3 | |
| 04 §2.3 + §10.1 | prefixdb SHA-256 namespacing (MakePrefix/JoinPrefixes) | M1.5 | golden vector |
| 04 §2.4 | versiondb overlay + merge iterator + commit_batch | M1.6 | underpins 27 §2 CC-ATOMIC |
| 04 §2.5 | meterdb prometheus | M1.7 | metric-name golden |
| 04 §2.6 | corruptabledb poison | M1.8 | + 27 §6.1 |
| 04 §2.7 + §10.6 | linkeddb node codec | M1.9 | golden vector |
| 04 §2.8 + 15 §3.4 | rpcdb tonic client/server | M1.11 | |
| 04 §2.9 | heightindexdb + own battery | M1.10 | |
| 04 §3.2 | merkledb Key/Path | M1.12 | golden vector |
| 04 §3.3 + §10.8 | node model + encodeDBNode + decode rejections | M1.13 | golden vector |
| 04 §3.4 | HashNode SHA-256 byte-exact | M1.14 | **golden::merkledb_root** (TDD anchor) |
| 04 §3.5 | View/TrieView, history, node stores | M1.15 | + 27 §4.1 cleanShutdown rebuild |
| 04 §3.6 | single/range/change proofs | M1.17, M1.18 | golden::merkledb_proof, golden::range_proof |
| 04 §3.7 + 19 §4 | SyncDb trait, Syncer, work-heap | M1.19 | sync proto wire golden |
| 04 §4.1/§4.2/§4.4 | Firewood SHA mode + propose/commit/revision | M1.20 | R3 |
| 04 §4.1 (ethhash) + §4.3 | Firewood ethhash EVM root | M1.21 | golden::firewood_ethhash_root; full reth `StateProvider` deferred to M-EVM (10) |
| 04 §5.1 | blockdb file format + recovery | M1.22 | prop::blockdb_roundtrip; + 27 §5.1 |
| 04 §5.1 upstream-delta | blockdb single-process advisory file lock (Go `dc350727b7` #5420) | **M1.27** | not fork-gated; `LOCK` file `flock` before recovery |
| 04 §5.2 | archivedb ^height encoding | M1.23 | golden vector |
| 04 §6 | test plan (dbtest, goldens, proofs, proptest invariants, rpcdb interop) | M1.2, M1.3–M1.11, M1.14, M1.16, M1.17–M1.19 | live Go↔Rust differential interop deferred to M-differential (02 §11) |
| 04 §7 + §11 + 00 R2 | Go-data-dir migration tool | M1.24 | CLI wiring in M12; in-place open documented NOT supported |
| 04 §8/§9/§10 | Go→Rust mapping / perf notes / on-disk key catalog | informs M1.5/M1.9/M1.13/M1.23/M1.24 | §10 full VM key catalog reproduced by VM milestones (08/09/10) |
| 27 §1/§2 | "committed" per layer; CC-ATOMIC one-batch | M1.6 (versiondb commit_batch primitive) | full accept-boundary merge (write_all/SharedMemory.apply) lives in VM/chains milestones (08/09/10/chains) |
| 27 §3/§5 | crash-point matrix, per-VM recovery | M1.15 (merkledb cleanShutdown), M1.22 (blockdb torn-write) | per-VM recovery procedures deferred to VM milestones; crash-injection suite to 27's test plan |
| 27 §4 | ungracefulShutdown marker | — | deferred to M-node (ava-node) |
| 27 §6 | corruptabledb fatal-vs-recoverable | M1.8 | classification wiring in M-node supervision |
| 19 §1–§3 | three-phase lifecycle, bootstrap/state-sync actors | — | deferred to M-consensus/engine (ava-engine, spec 19) |
| 19 §4 | merkledb sync work-heap + Syncer | M1.19 | |
| 19 §5/§6 | per-VM matrix, EVM snap sync | — | deferred to M-EVM (10) |
| 19 §8 | sync proof round-trip + work-heap fuzz | M1.19 | live differential (Rust↔Go server) deferred to M-differential |
| 15 §3.4 | rpcdb proto | M1.11 | |
| 15 §3.10 | sync proto (ProofRequest/Response, ProofNode, MaybeBytes, Key) | M1.17, M1.18, M1.19 | only proto map on the wire (`ProofNode.children`); never on a hashed path (15 §6) |
| 15 §6 | byte-exactness matrix (merkledb root/proof YES, EVM state root YES) | M1.14, M1.17, M1.18, M1.21 | |
| 02 §4.2 | merkledb/database proptest properties | M1.2, M1.16, M1.18 | |
| 02 §6 | golden vectors per surface | M1.5, M1.7, M1.9, M1.12, M1.13, M1.14, M1.17, M1.18, M1.19, M1.21, M1.22, M1.23 | committed under tests/vectors/ |
| 02 §7.2 + §13.3 | run_database_suite/proptests; every backend passes | M1.2 + all backends | |
| 02 §8 + §13.5 | cargo-fuzz merkledb op-stream | M1.25 | |
| 02 §10.1 + §13.4 | PORTING.md matrix | M1.26 | |
| 00 §6.1 | determinism (order-independent root, no HashMap on serialization, checked arith) | M1.13, M1.16 | |
| 00 §7.6 | unsafe only behind audited wrappers (rocksdb, firewood) | M1.4, M1.20 | |

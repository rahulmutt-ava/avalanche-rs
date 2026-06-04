# M0 — Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bootstrap the Cargo workspace and deliver the T0 primitive crates (`ava-types`, `ava-codec` + `ava-codec-derive`, `ava-crypto`, `ava-utils`, `ava-version`) byte-/behavior-exact with avalanchego, retiring R1 (gonum MT19937/-64 RNG parity) first.
**Tier:** T0 — Primitives
**Crates:** `ava-types`, `ava-codec`, `ava-codec-derive`, `ava-crypto`, `ava-utils`, `ava-version`, `avalanchers` (skeleton binary)
**Owning specs:** `03-core-primitives.md` (primary), `15-serialization-and-wire-formats.md` §4, `24-determinism-and-clock.md`, `25-key-management-and-signing.md`, `21-fee-economics-math.md` (constants only), `00`/`02` (conventions)
**Depends on (prior milestones):** none (root + workspace bootstrap)
**Exit gate (named tests):** `golden::codec_all_types`, `golden::cb58_addr_bech32`, `golden::bls_sign_pop`, `golden::secp_recover`, `golden::nodeid_from_cert`, **`golden::sampler_mt19937_stream`** (R1 gate), `prop::codec_roundtrip` (cases=4096), `conformance::run_codec_suite`

---

## Dependency map & parallel waves

Crate dep direction (spec 03 §0): `ava-types` → `ava-codec` (+derive); `ava-crypto` → `ava-types`; `ava-utils` standalone; `ava-version` → `ava-types`. `ava-codec` needs only `ava-types` primitive newtypes; `ava-types::Id::prefix/append` use an inline BE writer to avoid a cycle (spec 03 §0 "Packer placement decision").

| Wave | Tasks | Notes / parallelism |
|---|---|---|
| **W0 — bootstrap** | M0.1 (workspace), M0.2 (extract-vectors stub) | Must land first. M0.2 produces golden vectors consumed by later tasks; X-cross-cutting owns its maturation. |
| **W1 — R1 gate (RNG)** | M0.3 (`Source` + MT19937/-64), M0.4 (`Uint64Inclusive`) | **TDD entry point.** Highest risk; pin before any consensus-affecting code. Lives in `ava-utils`. M0.4 depends on M0.3. |
| **W2 — independent primitives** | `ava-types`: M0.5, M0.6, M0.7, M0.8 · `ava-utils`: M0.9, M0.10, M0.11, M0.12 (clock) · `ava-crypto` hashing: M0.13 | After W0. `ava-types`, `ava-utils`, and `ava-crypto` hashing proceed **in parallel** (independent crates). M0.4 (RNG) feeds M0.10 (samplers). |
| **W3 — codec** | M0.14 (Packer), M0.15 (derive macro + traits), M0.16 (Manager + linearcodec typeID registry) | Depends on `ava-types` (M0.5). M0.15 depends on M0.14; M0.16 depends on M0.15. `ava-codec` ⫫ `ava-crypto`/`ava-version` (parallel tracks). |
| **W4 — crypto surfaces** | M0.17 (CB58/formatting/bech32), M0.18 (secp256k1), M0.19 (BLS sign/agg/PoP), M0.20 (staking cert parse + NodeID), M0.21 (BLS LocalSigner lifecycle) | Depend on M0.13 (hashing) + M0.5 (ids). Largely parallel with each other and with the codec track. |
| **W5 — version/upgrade** | M0.22 (`Application`/`Compatibility`), M0.23 (`UpgradeConfig`/`Fork`/activation) | Depends on M0.5 (`Id`). Parallel with W3/W4. |
| **W6 — per-crate contracts** | M0.24 (proptest suites + regressions + fuzz targets + PORTING.md scaffold) | Depends on the crate it covers being green. Can be folded incrementally but is tracked as one task for completeness. |
| **W7 — exit gate** | M0.25 (Milestone exit gate) | Last. Builds workspace + binary, runs nextest CI profile + clippy + all named exit tests; updates every `tests/PORTING.md`. |

`ava-crypto` and `ava-codec` can proceed fully in parallel after the workspace bootstrap (W0) and `ava-types` ids (M0.5); `ava-utils` (incl. the R1 RNG) and `ava-version` are independent of both.

---

## Tasks

### Task M0.1: Bootstrap the Cargo workspace + skeleton `avalanchers` binary
**Crate:** workspace · **Depends on:** none · **Spec:** `00` §3 (layout), §4 (canonical deps), §8 (lints, license header); `02` §1 (nextest)
**Files:**
- Create: `/Users/rahul.muttineni/avalanche-rs/Cargo.toml` (`[workspace]` + `[workspace.dependencies]`)
- Create: `/Users/rahul.muttineni/avalanche-rs/rust-toolchain.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/rustfmt.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/.config/nextest.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/avalanchers/Cargo.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/avalanchers/src/main.rs`
- Test: `/Users/rahul.muttineni/avalanche-rs/crates/avalanchers/tests/cli_version_help.rs`

- [ ] **Step 1 — Red:** Write `unit::binary_answers_version_and_help` in `tests/cli_version_help.rs`:
  ```rust
  // Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
  // See the file LICENSE for licensing terms.
  use std::process::Command;
  #[test]
  fn binary_answers_version_and_help() {
      let exe = env!("CARGO_BIN_EXE_avalanchers");
      let v = Command::new(exe).arg("--version").output().unwrap();
      assert!(v.status.success());
      assert!(String::from_utf8_lossy(&v.stdout).contains("avalanchers/"));
      let h = Command::new(exe).arg("--help").output().unwrap();
      assert!(h.status.success());
  }
  ```
- [ ] **Step 2 — Confirm red:** Run `cd /Users/rahul.muttineni/avalanche-rs && cargo test -p avalanchers --test cli_version_help` → expect failure: no workspace / crate `avalanchers` not found (compile/manifest error).
- [ ] **Step 3 — Green:**
  - Root `Cargo.toml`: `[workspace] resolver = "2"`, `members = ["crates/*"]`. Add `[workspace.package]` (edition 2021, license, authors). Add `[workspace.dependencies]` pinning the canonical crates from `00` §4 used in M0: `thiserror`, `anyhow`, `serde`, `serde_json`, `hex`, `sha2`, `sha3`, `ripemd`, `bs58`, `bech32 = "0.11"`, `secp256k1 = { version = "0.31", features = ["recovery","global-context"] }`, `blst = "0.3"`, `rcgen`, `x509-parser`, `rustls-pemfile`, `zeroize`, `num-bigint`, `parking_lot`, `arc-swap`, `indexmap`, `smallvec`, `bytes`, `chrono`, `clap = { version = "4", features=["derive"] }`, plus dev-deps `proptest`, `rstest`, `assert_matches`, `pretty_assertions`, `libfuzzer-sys`, `arbitrary`.
  - `rust-toolchain.toml`: pin a recent stable channel + components `rustfmt`, `clippy`.
  - `.config/nextest.toml`: a `[profile.ci]` with `slow-timeout = { period = "120s" }` and `leak-timeout = "120s"` (mirrors Go `-timeout=120s`, `02` §1).
  - `crates/avalanchers/src/main.rs`: a `clap`-derive `Args` with global `--version`/`--help` wired (clap auto-handles `--help`; set `version` from a constant). Print `format!("avalanchers/{}", env!("CARGO_PKG_VERSION"))` for `--version` — this is the **local CLI identity** (`avalanchers/<ver>`); the **wire/P2P client string** stays `avalanchego` (`ava_version::CLIENT`, `03` §5.1 / `26`). Until `ava-version` exists, hardcode the M0 placeholder and add a `// TODO(M0.22)` to source the numeric version from `ava_version::CURRENT` while keeping the `avalanchers/` prefix. License header on every `.rs` file; `#![forbid(unsafe_code)]`.
  - Note in a top comment: deeper dev-env (Nix, Bazel, cargo-deny, CI, run_task.sh/xtask, differential harness) is owned by **X-cross-cutting.md** — do not add here.
- [ ] **Step 4 — Confirm green:** Run `cd /Users/rahul.muttineni/avalanche-rs && cargo build -p avalanchers && cargo test -p avalanchers --test cli_version_help` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.1: bootstrap cargo workspace + skeleton avalanchers binary"`

---

### Task M0.2: Stub `tools/extract-vectors` to produce M0 golden vectors
**Crate:** tooling (stub) · **Depends on:** M0.1 · **Spec:** `02` §6.2 (extraction procedure); `22` (corpus). X-cross-cutting owns maturation.
**Files:**
- Create: `/Users/rahul.muttineni/avalanche-rs/tools/extract-vectors/README.md`
- Create: `/Users/rahul.muttineni/avalanche-rs/tools/extract-vectors/extract.go` (scratch Go program, gated, not in the Cargo workspace)
- Create vector dirs (with committed `.json` placeholders + provenance manifests): `tests/vectors/{codec,ids,crypto,sampler,rng,upgrade}/`

- [ ] **Step 1 — Red:** Add `unit::vectors_present` to `crates/ava-utils/tests/...` later; for this task the "red" is operational: the golden vector files the W1–W5 tests load do not yet exist. Write a one-line shell check (in the task notes) `test -f tests/vectors/rng/mt19937_64.json` → fails.
- [ ] **Step 2 — Confirm red:** Run `cd /Users/rahul.muttineni/avalanche-rs && test -f tests/vectors/rng/mt19937_64.json` → expect non-zero exit (file missing).
- [ ] **Step 3 — Green:**
  - `extract.go`: a minimal Go program (built against the pinned avalanchego tree out-of-band) that dumps, as `{input, expected_hex|expected}` JSON, exactly the M0 surfaces (`02` §6.2): RNG raw streams for `Mt19937_64` and `Mt19937` (seeds `{0,1,5489,0xDEADBEEF,u64::MAX,1700000000000000000}`, first 320 `Uint64` each — 320 > NN forces a refill — per `03` §10.4 item 1); `Uint64Inclusive` triples for the three branches (`03` §10.4 item 2); deterministic uniform/weighted/weighted-without-replacement sampler outputs (item 3); codec golden bytes per registered-type family (`03` §8 item 1); cb58 + bech32 + hex address strings for Mainnet/Fuji (`03` §8 item 5); secp256k1 RFC6979 + recover vectors (item 6); BLS sign/agg/PoP + DST vectors (item 7); NodeID-from-cert vectors incl. the `large_rsa_key` reject case (`25` §8.1); upgrade activation booleans at `{forkTime-1ns, forkTime, forkTime+1ns}` for Mainnet/Fuji (`03` §11.3).
  - Commit the produced `.json` vectors under the dirs above. Each file carries a provenance note (Go source path + commit) per `02` §6.1.
  - `README.md`: document that this is an M0 stub; **X-cross-cutting.md owns** its CI integration, drift job, and full corpus (`22`).
- [ ] **Step 4 — Confirm green:** Run `cd /Users/rahul.muttineni/avalanche-rs && test -f tests/vectors/rng/mt19937_64.json && test -f tests/vectors/codec && test -f tests/vectors/upgrade` (dirs/files exist) → expect exit 0 for the files; vectors load as valid JSON (`python3 -c "import json,glob;[json.load(open(f)) for f in glob.glob('tests/vectors/**/*.json',recursive=True)]"`).
- [ ] **Step 5 — Commit:** `git commit -m "M0.2: stub tools/extract-vectors + commit M0 golden vectors"`

---

### Task M0.3: R1 GATE — `Source` trait + hand-ported gonum MT19937 / MT19937-64
**Crate:** `ava-utils` · **Depends on:** M0.1, M0.2 · **Spec:** `03` §10.1–§10.3 (R1 resolution); `00` §11.2 R1; `24` hazard #4
**Files:**
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/Cargo.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/src/lib.rs`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/src/rng.rs`
- Test: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/tests/golden_rng.rs`

- [ ] **Step 1 — Red (THE TDD ENTRY POINT):** Write `golden::sampler_mt19937_stream` asserting the Rust MT19937-64 stream equals the committed gonum stream **for seed 0** (then extend to the full seed set). Real test:
  ```rust
  // Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
  // See the file LICENSE for licensing terms.
  use ava_utils::rng::{Mt19937_64, Mt19937, Source};
  #[derive(serde::Deserialize)]
  struct Vec64 { seed: u64, stream: Vec<u64> }
  #[test]
  fn sampler_mt19937_stream() {
      let raw = std::fs::read_to_string(
          concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vectors/rng/mt19937_64.json")).unwrap();
      let cases: Vec<Vec64> = serde_json::from_str(&raw).unwrap();
      // R1 gate: assert seed 0 first, then every committed seed.
      assert!(cases.iter().any(|c| c.seed == 0), "seed-0 vector required");
      for c in &cases {
          let mut g = Mt19937_64::new();
          g.seed(c.seed);
          let got: Vec<u64> = (0..c.stream.len()).map(|_| g.uint64()).collect();
          assert_eq!(got, c.stream, "MT19937-64 stream diverged for seed {}", c.seed);
      }
      // 32-bit variant: high-word-first Uint64 composition.
      let raw32 = std::fs::read_to_string(
          concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/vectors/rng/mt19937_32.json")).unwrap();
      let cases32: Vec<Vec64> = serde_json::from_str(&raw32).unwrap();
      for c in &cases32 {
          let mut g = Mt19937::new(); g.seed(c.seed);
          let got: Vec<u64> = (0..c.stream.len()).map(|_| g.uint64()).collect();
          assert_eq!(got, c.stream, "MT19937(32) Uint64 stream diverged for seed {}", c.seed);
      }
  }
  ```
- [ ] **Step 2 — Confirm red:** Run `cd /Users/rahul.muttineni/avalanche-rs && cargo test -p ava-utils --test golden_rng sampler_mt19937_stream` → expect failure: `ava_utils::rng` module / `Mt19937_64` does not exist (compile error), then once stubbed, an `assert_eq!` stream mismatch.
- [ ] **Step 3 — Green:** Implement `rng.rs` verbatim from `03` §10.3: the `Source` trait (`uint64(&mut self) -> u64`); `Mt19937_64 { mt: [u64;312], mti }` with `NN=312, MM=156, MATRIX_A=0xB5026F5AA96619E9, UPPER=0xFFFFFFFF80000000, LOWER=0x7FFFFFFF`, the `seed` schedule (`6364136223846793005.wrapping_mul(prev ^ (prev>>62)).wrapping_add(i)`), the `refill` twist (two ranges + wrap word, `MAG01=[0,MATRIX_A]`), and the 4-line tempering; `Mt19937 { mt: [u32;624], mti }` with `N=624, M=397, MATRIX_A=0x9908b0df` and `seed as u32` truncation, the 32-bit tempering, and `uint64() = (h<<32)|l` (high word drawn first). Use `wrapping_*` throughout (Go integer overflow). Lazy default-seed `5489` on the `mti==N+1` sentinel. `#![forbid(unsafe_code)]`; license header; this module is the only place the consensus RNG lives (hazard #4).
- [ ] **Step 4 — Confirm green:** Run `cd /Users/rahul.muttineni/avalanche-rs && cargo test -p ava-utils --test golden_rng sampler_mt19937_stream` → expect PASS (R1 retired). Also `cargo nextest run -p ava-utils`.
- [ ] **Step 5 — Commit:** `git commit -m "M0.3: R1 GATE — hand-port gonum MT19937/-64, golden::sampler_mt19937_stream green"`

---

### Task M0.4: `Uint64Inclusive` rejection-sampling wrapper (exact draw count)
**Crate:** `ava-utils` · **Depends on:** M0.3 · **Spec:** `03` §4.1 (`uint64_inclusive` three branches), §10.3 (draw-count parity)
**Files:**
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/src/sampler/rng.rs`
- Test: `/Users/rahul.muttineni/avalanche-rs/crates/ava-utils/tests/golden_uint64_inclusive.rs`

- [ ] **Step 1 — Red:** Write `golden::uint64_inclusive_branches` loading `tests/vectors/sampler/uint64_inclusive.json` ( `{seed, n, outputs}` covering the three branches: `n=255` power-of-two, `n > MaxInt64`, `n=10` rejection). Assert `uint64_inclusive(&mut src, n)` produces the committed `outputs` in order (proving identical draw counts through rejections).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-utils --test golden_uint64_inclusive` → expect failure: `uint64_inclusive` undefined.
- [ ] **Step 3 — Green:** Port `uint64_inclusive(src: &mut impl Source, n: u64) -> u64` verbatim from `03` §4.1: branch 1 `n & n.wrapping_add(1) == 0` → `src.uint64() & n`; branch 2 `n > i64::MAX as u64` → loop `while v > n`; branch 3 `max = (1<<63)-1 - (1<<63)%(n+1)`, uint63 mask `& i64::MAX as u64`, reject loop, `% (n+1)`. No floats; the loop must consume the same RNG draws as Go.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-utils --test golden_uint64_inclusive` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.4: port Uint64Inclusive rejection sampler (draw-count parity)"`

---

### Task M0.5: `ava-types` fixed IDs (`Id`/`ShortId`/`NodeId`) + CB58-free helpers
**Crate:** `ava-types` · **Depends on:** M0.1 · **Spec:** `03` §1.1 (IDs, `prefix`/`append`/`xor`/`bit`), §1.2 (bits), §7 (error model)
**Files:**
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-types/Cargo.toml`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-types/src/lib.rs`
- Create: `/Users/rahul.muttineni/avalanche-rs/crates/ava-types/src/id.rs`, `short_id.rs`, `node_id.rs`, `bits.rs`, `error.rs`
- Test: `/Users/rahul.muttineni/avalanche-rs/crates/ava-types/tests/id_ops.rs`

- [ ] **Step 1 — Red:** Write `unit::id_prefix_and_bit` asserting `Id::prefix(&[u64])` = `sha256(be_u64(p)…  ++ id_bytes)` against a committed value, `append`, `xor`, and `bit(i)` (`(byte[i/8] >> (i%8)) & 1`). Use a small known vector (or compute `sha256` inline in the test for `prefix`).
  ```rust
  use ava_types::Id;
  #[test]
  fn id_prefix_and_bit() {
      let id = Id::from_slice(&[1u8;32]).unwrap();
      assert_eq!(id.bit(0), 1);
      let p = id.prefix(&[7]); // be_u64(7) ++ bytes -> sha256
      assert_ne!(p, id);
  }
  ```
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-types --test id_ops` → expect failure: types undefined.
- [ ] **Step 3 — Green:** Implement the three `Copy` newtypes per `03` §1.1 (`Id([u8;32])`, `ShortId([u8;20])`, `NodeId([u8;20])`), `derive(Clone,Copy,PartialEq,Eq,Hash,PartialOrd,Ord,Default)` (Ord == `bytes.Compare`). Constants `ID_LEN/SHORT_ID_LEN/NODE_ID_LEN/NODE_ID_PREFIX`, `EMPTY`. `from_slice` → `Error::InvalidHashLen` on wrong len. `prefix`/`append` use an inline BE writer + `sha2::Sha256` single-pass (spec note: avoid the `ava-codec` cycle); `xor`, `bit`, `hex()`. Port `bits.rs` `equal_subset`/`first_difference_subset` verbatim (`03` §1.2, consensus-affecting masking). `error.rs`: `thiserror` enum with `InvalidHashLen, NoIdWithAlias, AliasAlreadyMapped, ShortNodeId, MissingQuotes` (`03` §7). `#![forbid(unsafe_code)]`; license headers.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-types --test id_ops` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.5: ava-types fixed IDs + bit helpers + error model"`

---

### Task M0.6: ID string/JSON forms (CB58 Display/FromStr, NodeID prefix)
**Crate:** `ava-types` (+ depends on crypto cb58) · **Depends on:** M0.5, M0.17 · **Spec:** `03` §1.1 (Display/FromStr/JSON), §3.2 (CB58); `15` §4.4
**Files:**
- Modify: `crates/ava-types/src/{id.rs,short_id.rs,node_id.rs}`
- Test: `/Users/rahul.muttineni/avalanche-rs/crates/ava-types/tests/golden_cb58.rs`

> **Cycle note:** CB58 lives in `ava-crypto` (`03` §3.2) but `ava-types` Display needs it. To avoid a `types→crypto` dep cycle, place the raw CB58 codec in `ava-utils` or a tiny `ava-types` internal module (hand-rolled `bs58` + `checksum4`); spec 03 §0 keeps `ava-types` minimal. **Decision for M0:** put `cb58_encode/decode` in `ava-utils` (standalone) and have both `ava-types` and `ava-crypto::cb58` re-export it. Update M0.17 to define CB58 in `ava-utils`. (Record this in `ava-types` docs.)

- [ ] **Step 1 — Red:** Write `golden::id_nodeid_cb58_strings` loading `tests/vectors/ids/cb58.json` (known Mainnet/Fuji `Id`/`NodeId` strings). Assert `Id::from_str(s)?.to_string() == s`, `NodeId` requires/emits the `NodeID-` prefix, and JSON `null` is a no-op (leaves `Default`).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-types --test golden_cb58` → expect failure: no `Display`/`FromStr`/serde impls (or vector mismatch).
- [ ] **Step 3 — Green:** Implement `Display`/`FromStr` using `cb58_encode/decode` (no prefix for `Id`/`ShortId`; `"NodeID-" + cb58` for `NodeId`, parse requires prefix → `Error::ShortNodeId`/`MissingQuotes`). serde `Serialize` as quoted Display string; custom `Deserialize` where literal `null` keeps `Default` (Go behavior, `03` §1.1). `Id::hex()` lowercase no-`0x`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-types --test golden_cb58` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.6: ID/NodeID CB58 string + JSON forms (golden)"`

---

### Task M0.7: `RequestId` + `Aliaser`
**Crate:** `ava-types` · **Depends on:** M0.5 · **Spec:** `03` §1.3
**Files:**
- Create: `crates/ava-types/src/request_id.rs`, `crates/ava-types/src/aliaser.rs`
- Test: `crates/ava-types/tests/aliaser.rs`

- [ ] **Step 1 — Red:** Write `unit::aliaser_bidirectional` asserting an alias maps to exactly one id, one id → many aliases (first is primary), `primary_alias_or_default` falls back to `id.to_string()`, `get_relevant_aliases` strips the `alias == id.String()` self-alias, and a duplicate alias errors `AliasAlreadyMapped`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-types --test aliaser` → expect failure: `Aliaser` undefined.
- [ ] **Step 3 — Green:** `RequestId { node_id, chain_id, request_id: u32, op: u8 }` (plain value type). `Aliaser`: bidirectional `alias→id` / `id→Vec<alias>` behind `parking_lot::RwLock`, methods per `03` §1.3, errors from `ava-types::Error`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-types --test aliaser` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.7: ava-types RequestId + Aliaser"`

---

### Task M0.8: Network constants (`ava-types::constants`)
**Crate:** `ava-types` · **Depends on:** M0.5 · **Spec:** `03` §1.4 (`utils/constants/network_ids.go`)
**Files:**
- Create: `crates/ava-types/src/constants.rs`
- Test: `crates/ava-types/tests/constants.rs`

- [ ] **Step 1 — Red:** Write `unit::network_hrp_and_ids` asserting `MAINNET_ID==1`, `FUJI_ID==5`, `LOCAL_ID==12345`, `get_hrp(MAINNET_ID)=="avax"`, `get_hrp(FUJI_ID)=="fuji"`, `get_hrp(9999)=="custom"` (fallback), `PRIMARY_NETWORK_ID == Id::EMPTY`, and `network_id(network_name(MAINNET_ID)) == Some(MAINNET_ID)`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-types --test constants` → expect failure: constants undefined.
- [ ] **Step 3 — Green:** Copy the constants verbatim with Go-path doc-comments (`03` §1.4): network IDs, HRPs (incl. cascade/denali/everest/testing historical HRPs), `FALLBACK_HRP="custom"`, `PRIMARY_NETWORK_ID`. Implement `get_hrp`, `network_name`, `network_id` over the bidirectional maps.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-types --test constants` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.8: ava-types network constants + get_hrp"`

---

### Task M0.9: `ava-utils` set / bag / Bits / linked map / safemath / units
**Crate:** `ava-utils` · **Depends on:** M0.1 · **Spec:** `03` §4.2 (set/bag/Bits), §4.3 (linked, safemath, units); `24` hazard #3
**Files:**
- Create: `crates/ava-utils/src/{set.rs,bits.rs,bag.rs,linked.rs,math.rs,units.rs}`
- Test: `crates/ava-utils/tests/{bits.rs,safemath.rs,linked.rs}`

- [ ] **Step 1 — Red:** Write `unit::safemath_checked` (table test: `add(u64::MAX,1)==Err(Overflow)`, `sub(0,1)==Err(Underflow)`), `unit::bits_set_algebra` (big-int-backed `Bits` union/intersection/difference/len(popcount)/`from_bytes`/`Bytes` big-endian round-trip), and `unit::linked_move_to_back` (re-`Put` of an existing key moves it to back).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-utils --test safemath --test bits --test linked` → expect failure: modules undefined.
- [ ] **Step 3 — Green:**
  - `math.rs` (`safemath` alias): generic checked `add/sub/mul` → `Error::{Overflow,Underflow}`, `abs_diff`, `max_uint::<T>()` (`03` §4.3, `00` §6.1; checked arithmetic per hazard #3).
  - `bits.rs`: `Bits` over `num_bigint::BigUint` (`Add/Remove/Contains/Union/Intersection/Difference/Len/BitLen/Bytes/from_bytes` big-endian, `String`=hex) + `Bits64` u64 fast-path (`03` §4.2).
  - `set.rs`: `Set<T>` (`Of/Add/Contains/Overlaps/List/SortedList`); callers sort before serializing.
  - `bag.rs`: `Bag<T>` multiset with `threshold`/`met_threshold` bookkeeping + `UniqueBag` (`HashMap<T, Bits>`).
  - `linked.rs`: `LinkedHashmap<K,V>` preserving insertion order, `Put` on existing key moves to back (`indexmap` + explicit move, `03` §4.3).
  - `units.rs`: `KiB/MiB/GiB`, `NanoAvax…Avax` constants.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-utils --test safemath --test bits --test linked` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.9: ava-utils set/bag/Bits/linked/safemath/units"`

---

### Task M0.10: Samplers (uniform / weighted / weighted-without-replacement)
**Crate:** `ava-utils` · **Depends on:** M0.4, M0.9 · **Spec:** `03` §4.1 (uniform_replacer, weighted_heap, weighted-without-replacement)
**Files:**
- Create: `crates/ava-utils/src/sampler/{mod.rs,uniform.rs,weighted.rs,weighted_without_replacement.rs}`
- Test: `crates/ava-utils/tests/golden_samplers.rs`

- [ ] **Step 1 — Red:** Write `golden::deterministic_samplers` loading `tests/vectors/sampler/samplers.json` (`{seed, weights, count, sampled_indices}` for uniform, weighted, weighted-without-replacement over an `Mt19937_64` source). Assert each `Sample(count)` matches the committed indices.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-utils --test golden_samplers` → expect failure: sampler constructors undefined.
- [ ] **Step 3 — Green:** Port the three samplers from `03` §4.1:
  - Uniform (`uniform_replacer.go`): lazy partial Fisher–Yates with the `drawn` defaultMap (`get(k, default=k)`) and the exact draw formula; `Sample(count)` resets then `next` count times.
  - Weighted (`weighted_heap.go`): heap of `{weight, cumulative_weight, index}`, stable-sort `(weight desc, index asc)`, accumulate with **checked add** (`parent=(i-1)>>1`), traversal exactly as Go.
  - Weighted-without-replacement (generic): `Initialize` sums with checked add; `Sample(count)` = reset uniform, then `weighted.sample(uniform.next())` per draw. `new_deterministic_weighted_without_replacement(src)`.
  - Define `Source`/`Uniform`/`Weighted`/`WeightedWithoutReplacement` traits per spec. Single-threaded by contract (no parallelism — `03` §9).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-utils --test golden_samplers` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.10: deterministic samplers (uniform/weighted/wwr) golden-gated"`

---

### Task M0.11: CB58 codec in `ava-utils` (shared by types & crypto)
**Crate:** `ava-utils` · **Depends on:** M0.1, M0.13 (sha256) · **Spec:** `03` §3.2 (CB58); `15` §4.4
**Files:**
- Create: `crates/ava-utils/src/cb58.rs`
- Test: `crates/ava-utils/tests/golden_cb58_codec.rs`, fuzz target `crates/ava-utils/fuzz/fuzz_targets/cb58_roundtrip.rs`

> Placed here (not `ava-crypto`) to break the `ava-types → ava-crypto` cycle (see M0.6 note). `ava-crypto::cb58` re-exports it. Hashing (`sha256`) used by the checksum lives in `ava-crypto` (M0.13); to keep `ava-utils` cycle-free, CB58's checksum uses `sha2::Sha256` **directly** in `ava-utils` rather than depending on `ava-crypto`.

- [ ] **Step 1 — Red:** Write `golden::cb58_roundtrip` loading `tests/vectors/ids/cb58_raw.json` (`{bytes_hex, cb58}` pairs). Assert `cb58_encode(bytes)==cb58` and `cb58_decode(cb58)==bytes`; plus a bad-checksum case → `Error::BadChecksum`, a too-short case → `MissingChecksum`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-utils --test golden_cb58_codec` → expect failure: `cb58_encode` undefined.
- [ ] **Step 3 — Green:** Implement per `03` §3.2: `cb58_encode(b)` = `bs58_encode(b ++ last4(sha256(b)))` (reject `len > i32::MAX - 4` → `EncodingOverflow`); `cb58_decode(s)` = `bs58_decode`, split off 4-byte checksum, verify `== last4(sha256(raw))`. Use `bs58` (Bitcoin alphabet) raw — **not** `with_check`. Define `ava_utils::Error` variants `{Overflow, Underflow, Base58Decoding, BadChecksum, MissingChecksum, EncodingOverflow}` (extends M0.9's `Error`). `#![forbid(unsafe_code)]`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-utils --test golden_cb58_codec` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.11: CB58 codec in ava-utils (shared, golden + fuzz target)"`

---

### Task M0.12: Injectable `Clock` (RealClock / MockClock)
**Crate:** `ava-utils` · **Depends on:** M0.1 · **Spec:** `24` Part B (clock trait, MockClock, MAX_UNIX_SECS); `24` hazard #5
**Files:**
- Create: `crates/ava-utils/src/clock.rs`
- Test: `crates/ava-utils/tests/clock_parity.rs`

- [ ] **Step 1 — Red:** Write `unit::mock_clock_parity` (port Go `clock_test.go`, `24` §B.6): `set` ⇒ faked, `sync` ⇒ wall, `unix` clamps pre-epoch to 0, `unix_time` truncates sub-second, `advance(d)` moves faked + monotonic by `d`, `MAX_UNIX_SECS == (1<<63) - 62_135_596_801`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-utils --test clock_parity` → expect failure: `Clock`/`MockClock` undefined.
- [ ] **Step 3 — Green:** Implement the `Clock` trait + `RealClock` + `MockClock` verbatim from `24` §B.1 (`now/unix/unix_time/since/monotonic`; `MockClock::{at,set,sync,advance}`; `MAX_UNIX_SECS`). This module is the **only** place `SystemTime::now`/`tokio::time::Instant::now` may be called (hazard #5 allowlist; add the `// determinism-allow: ava-utils::clock` markers). `monotonic()` returns `tokio::time::Instant` so tests compose with `start_paused`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-utils --test clock_parity` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.12: injectable Clock (Real/Mock) per spec 24"`

---

### Task M0.13: `ava-crypto` hashing (sha256 / ripemd160 / keccak / checksum / address)
**Crate:** `ava-crypto` · **Depends on:** M0.5 · **Spec:** `03` §3.1 (`utils/hashing`)
**Files:**
- Create: `crates/ava-crypto/Cargo.toml`, `crates/ava-crypto/src/lib.rs`, `crates/ava-crypto/src/hashing.rs`, `crates/ava-crypto/src/error.rs`
- Test: `crates/ava-crypto/tests/hashing.rs`

- [ ] **Step 1 — Red:** Write `golden::address_from_pubkey` loading `tests/vectors/crypto/addr.json` asserting `pubkey_bytes_to_address(key) == ripemd160(sha256(key))` for committed pubkey/addr pairs, plus `checksum(b,4) == last 4 bytes of sha256(b)`.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test hashing` → expect failure: functions undefined.
- [ ] **Step 3 — Green:** Implement per `03` §3.1: `HASH_LEN=32`, `ADDR_LEN=20`, `sha256` (`sha2`), `ripemd160` (`ripemd`), `keccak256` (`sha3`), `checksum(b,n)` = last n bytes of sha256 (panic `n>32`), `pubkey_bytes_to_address` = `ripemd160(sha256(key))`. `error.rs`: `thiserror` `ava_crypto::Error` with the `03` §7 / `25` §7.1 variants seeded (extend in later tasks). `#![forbid(unsafe_code)]` (no FFI here yet).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test hashing` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.13: ava-crypto hashing + address derivation"`

---

### Task M0.14: `ava-codec` Packer (BE primitive reader/writer, sticky errors)
**Crate:** `ava-codec` · **Depends on:** M0.5 · **Spec:** `03` §2.1 (Packer), §7 (PackerError); `15` §4.1
**Files:**
- Create: `crates/ava-codec/Cargo.toml`, `crates/ava-codec/src/lib.rs`, `crates/ava-codec/src/packer.rs`, `crates/ava-codec/src/error.rs`
- Test: `crates/ava-codec/tests/packer.rs`

- [ ] **Step 1 — Red:** Write `prop::packer_roundtrip` + `unit::packer_bad_bool_sticky`: pack→unpack identity for each primitive; golden BE bytes for fixed inputs (`pack_u32(1) == [0,0,0,1]`); `unpack_bool` rejects values ≠ 0/1 (`Error::BadBool`); once errored, ops are no-ops returning zero (sticky); `unpack_str` over `MAX_STRING_LEN`/limited variants.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-codec --test packer` → expect failure: `Packer` undefined.
- [ ] **Step 3 — Green:** Implement `Packer` per `03` §2.1: constants (`BYTE_LEN…LONG_LEN`, `MAX_STRING_LEN=u16::MAX`), `PackerBuf` (owned `Vec` on write / borrowed `&[u8]` on read), `offset`, `max_size`, sticky `Option<PackerError>`. Methods `pack_byte/u16/u32/u64/bool/fixed_bytes/bytes/str` + `unpack_*` + `unpack_limited_bytes/str`. Sticky-error semantics (first error wins; subsequent ops no-op zero); `check_space` → `InsufficientLength`; `pack_bool`/`unpack_bool` 0/1 only. `error.rs`: `PackerError` enum `{InsufficientLength, NegativeOffset, InvalidInput, BadBool, Oversized}` (`03` §2.1/§7). Document the unreachable negative-offset branches.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-codec --test packer` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.14: ava-codec Packer (BE primitives, sticky errors)"`

---

### Task M0.15: `ava-codec-derive` macro + `Serializable`/`Deserializable` traits
**Crate:** `ava-codec-derive` (+ `ava-codec` traits) · **Depends on:** M0.14 · **Spec:** `03` §2.4 (reflectcodec rules), §2.5 (derive surface), §9 (field order/size)
**Files:**
- Create: `crates/ava-codec-derive/Cargo.toml` (`proc-macro = true`), `crates/ava-codec-derive/src/lib.rs`
- Modify: `crates/ava-codec/src/lib.rs` (define object-safe `Serializable`/`Deserializable` traits)
- Test: `crates/ava-codec/tests/derive.rs`

- [ ] **Step 1 — Red:** Write `unit::derive_field_order_and_kinds`: a `#[derive(AvaCodec)]` struct with a tagged `u32`, a `[u8;4]` (no length prefix), a `Vec<u8>` (u32 len prefix), a `String` (u16 len), a `Vec<NonU8>` (u32 count, per-element), and an **untagged** cache field (skipped). Assert `marshal_into` bytes match a hand-computed expected, `unmarshal_from` round-trips, declaration order preserved, and `size()` matches the byte length (excl. version). Add an interface enum `#[codec(type_registry)]` with `#[codec(type_id=7)]` asserting the `u32` typeID prefix.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-codec --test derive` → expect failure: `AvaCodec` derive undefined.
- [ ] **Step 3 — Green:**
  - In `ava-codec`: define object-safe `Serializable { marshal_into(&self, &mut Packer); size(&self)->usize }` and `Deserializable { unmarshal_from(&mut self, &mut Packer) }`.
  - In `ava-codec-derive`: `#[derive(AvaCodec)]` using `syn`/`quote`. Attributes `#[codec]` (include field, declaration order), `#[codec(type_id=N)]`, `#[codec(type_registry)]`, `#[codec(skip_ids=N)]`, `#[codec(version=N)]`. Emit per-kind wire encoding from the `03` §2.4 table: ints BE, bool 0/1, `String` u16+UTF-8, `[u8;N]` raw, `[T;N]` back-to-back, `Vec<u8>` u32+bytes, `Vec<T>` u32 count + elements, struct = concatenated fields, interface enum = `u32` typeID + value, `Box<T>` transparent, **reject `Option<T>` on serialized fields** (compile error). Slices: reject `len > i32::MAX` (`MaxSliceLenExceeded`); zero-length-element guard (`Marshal/UnmarshalZeroLength`); nil slice = u32 count 0. Maps: serialize-then-sort by serialized key bytes; decode enforces strictly increasing keys. Generate exact `size()`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-codec --test derive` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.15: ava-codec-derive macro + Serializable/Deserializable traits"`

---

### Task M0.16: Codec `Manager` + linearcodec typeID registry + version framing
**Crate:** `ava-codec` · **Depends on:** M0.15 · **Spec:** `03` §2.2 (Manager/Codec), §2.3 (typeID registry); `15` §4.1, §6 (version bytes)
**Files:**
- Create: `crates/ava-codec/src/manager.rs`, `crates/ava-codec/src/linearcodec.rs`, `crates/ava-codec/src/codectest.rs` (feature `testutil`)
- Test: `crates/ava-codec/tests/{golden_codec.rs,conformance.rs}`

- [ ] **Step 1 — Red:** Write two tests:
  - `golden::codec_all_types` (an EXIT-GATE test) loading `tests/vectors/codec/*.json` — for every registered-type family (fixed array, `Vec<u8>`, `Vec<struct>`, interface/typeID, map, nested) assert `manager.marshal(version, &v)` == committed bytes (incl. 2-byte version prefix) and `unmarshal` round-trips. Negative cases: trailing bytes → `ExtraSpace`; oversize slice → `MaxSliceLenExceeded`; bad bool; unsorted map keys; unknown typeID; unknown version.
  - `conformance::run_codec_suite` (EXIT-GATE) invoking the generic `ava_codec::codectest::run_codec_suite()` (mirrors Go `codectest.RunAll`).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-codec --test golden_codec --test conformance` → expect failure: `Manager` undefined / `run_codec_suite` undefined.
- [ ] **Step 3 — Green:**
  - `manager.rs`: `Manager { max_size, codecs: RwLock<HashMap<u16, Arc<dyn Codec>>> }`, `VERSION_SIZE=2`, `DEFAULT_MAX_SIZE=256*1024`, `INITIAL_SLICE_CAP=128`. `register` (→ `DuplicatedVersion`), `marshal` (new Packer, `pack_u16(version)`, `marshal_into`), `unmarshal` (reject `> max_size` → `UnmarshalTooBig`; read version → `CantUnpackVersion`/`UnknownVersion`; dispatch; **require `offset==len` → `ExtraSpace`**), `size`. `Codec` trait per `03` §2.2.
  - `linearcodec.rs`: sequential `u32` typeIDs in registration order from 0; `SkipRegistrations(n)`; interface = `pack_u32(typeID)` + value; modeled via the derive `#[codec(type_registry)]` enums (`03` §2.3). A golden test (in M0.16's vectors) asserts typeIDs against a Go-dumped table.
  - `error.rs`: extend `CodecError` enum with the `03` §2.2 list.
  - `codectest.rs` (`testutil` feature): `run_codec_suite()` exercising round-trip + the negative cases (the `02` §7 contract analog of Go `codectest`).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-codec --test golden_codec --test conformance` → expect PASS (`golden::codec_all_types`, `conformance::run_codec_suite` green).
- [ ] **Step 5 — Commit:** `git commit -m "M0.16: codec Manager + linearcodec typeID registry + codectest suite"`

---

### Task M0.17: CB58 re-export + `formatting` (Hex/HexC/HexNC) + bech32 addresses
**Crate:** `ava-crypto` · **Depends on:** M0.11, M0.13, M0.8 · **Spec:** `03` §3.2 (formatting), §3.3 (bech32); `15` §4.4
**Files:**
- Create: `crates/ava-crypto/src/cb58.rs` (re-export `ava_utils::cb58`), `crates/ava-crypto/src/formatting.rs`, `crates/ava-crypto/src/address.rs`
- Test: `crates/ava-crypto/tests/golden_encodings.rs`

- [ ] **Step 1 — Red:** Write `golden::cb58_addr_bech32` (EXIT-GATE) loading `tests/vectors/crypto/encodings.json` with Mainnet/Fuji known addresses. Assert: `formatting::encode(Hex, payload)` = `"0x"+hex(payload++ck4)`, `HexNC` = `"0x"+hex(payload)`, decode verifies checksum / requires `0x`, `Json` path errors; `address::format(alias, hrp, addr)` = `"alias-bech32(hrp,addr)"` round-trips (e.g. `X-avax1…`, `P-fuji1…`); `parse` splits on first `-` (≤2 parts; `ErrNoSeparator`).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test golden_encodings` → expect failure: functions undefined.
- [ ] **Step 3 — Green:** `cb58.rs` re-exports `ava_utils::cb58::{cb58_encode,cb58_decode}`. `formatting.rs`: `Encoding {Hex,HexNC,HexC,Json}` + `encode/decode` per `03` §3.2 (default `Hex`, `Json` unsupported in this path → error). `address.rs`: `format_bech32`/`parse_bech32` (8↔5-bit, pad=true, standard bech32 not bech32m), `format`/`parse` chain-prefixed; HRP from `ava_types::constants::get_hrp`. Use `bech32 = "0.11"` (`Hrp::parse` path) — verify `ConvertBits(8,5,pad)` parity via the golden vectors. Add crypto error variants `{MissingHexPrefix, BadChecksum, Base58Decoding, NoSeparator}`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test golden_encodings` → expect PASS (`golden::cb58_addr_bech32` green).
- [ ] **Step 5 — Commit:** `git commit -m "M0.17: formatting (Hex/HexNC/HexC) + bech32 addresses (golden)"`

---

### Task M0.18: secp256k1 (recoverable, low-S enforce, recover→address)
**Crate:** `ava-crypto` · **Depends on:** M0.13, M0.17 · **Spec:** `03` §3.4 (secp256k1); `00` §7.6 (FFI boundary)
**Files:**
- Create: `crates/ava-crypto/src/secp256k1.rs`
- Test: `crates/ava-crypto/tests/golden_secp.rs`

- [ ] **Step 1 — Red:** Write `golden::secp_recover` (EXIT-GATE) loading `tests/vectors/crypto/secp.json`: RFC6979 deterministic-signature vectors (reuse Go `rfc6979_test.go` inputs), recover pubkey from `[r||s||v]` over a hash and assert `recovered.address()` == expected; a hand-mutated **high-S** sig is rejected (`Error::MutatedSig`); `PrivateKey-` CB58 string round-trip.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test golden_secp` → expect failure: secp module undefined.
- [ ] **Step 3 — Green:** Implement per `03` §3.4 with `secp256k1 = "0.31"` (feature `recovery`). Constants `SIGNATURE_LEN=65 [r||s||v]`, `PRIVATE_KEY_LEN=32`, `PUBLIC_KEY_LEN=33`, `PRIVATE_KEY_PREFIX="PrivateKey-"`. `ava_sig_to_recoverable`/`recoverable_to_ava_sig` reorder vs decred `[v'||r||s]` (`v'=v+27`). `sign_hash` (RFC6979 + low-S). `verify_sig_format` rejects high-S (`is_high_s` on the 32-byte S scalar; consensus-critical) before recovery; reject compressed recids (`Compressed`). `PublicKey::{bytes()→[u8;33] compressed, address()→ShortId = ripemd160(sha256(compressed)), eth_address()→keccak256(uncompressed[1..])[12..]}`. `VerifyHash` recovers + compares addresses. `PrivateKey` string `"PrivateKey-"+cb58`. **This file uses the `secp256k1` FFI binding** — isolate behind safe wrappers with `// SAFETY:` (`00` §7.6); do not blanket-`forbid(unsafe_code)` if a binding shim needs it, else keep the crate forbidding and rely on the crate's safe API.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test golden_secp` → expect PASS (`golden::secp_recover` green).
- [ ] **Step 5 — Commit:** `git commit -m "M0.18: secp256k1 recoverable + low-S enforce + recover→address (golden)"`

---

### Task M0.19: BLS12-381 (`min_pk`): sign / aggregate / PoP / verify
**Crate:** `ava-crypto` · **Depends on:** M0.13 · **Spec:** `03` §3.5 (BLS); `25` §3.1 (Signer trait shape — trait deferred to M0.21)
**Files:**
- Create: `crates/ava-crypto/src/bls/mod.rs`, `crates/ava-crypto/src/bls/{keys.rs,sign.rs,ciphersuite.rs}`
- Test: `crates/ava-crypto/tests/golden_bls.rs`

- [ ] **Step 1 — Red:** Write `golden::bls_sign_pop` (EXIT-GATE) loading `tests/vectors/crypto/bls.json`: given a fixed 32-byte secret key, assert `(pk_compressed[48], pop[96])` equals committed Go bytes; `verify_pop(pk, pop, pk.compress())` accepts; a plain `sign`+`verify` over a fixed message accepts; aggregate(verify) of N sigs equals individual verifies; compress/uncompress round-trip; **DST byte-equality** asserts for `CIPHERSUITE_SIGNATURE`/`CIPHERSUITE_POP`. Cross-verify a Go-produced signature/pubkey from the vector.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test golden_bls` → expect failure: bls module undefined.
- [ ] **Step 3 — Green:** Implement per `03` §3.5 with `blst = "0.3"`, `use blst::min_pk::*`. Constants `PUBLIC_KEY_LEN=48`, `SIGNATURE_LEN=96`, `SECRET_KEY_LEN=32`; DST strings verbatim (`ciphersuite.rs`). API: `SecretKey::{new,from_bytes (zeroize on drop)}`, `sign`/`sign_pop`, `PublicKey::{compress→48, from_compressed (uncompress + key_validate subgroup check), serialize→96}`, `Signature::{compress→96, from_bytes (uncompress + sig_validate)}`, `aggregate_public_keys`/`aggregate_signatures` (error on empty), `verify`/`verify_pop` (pass `false` validation flags since validated on parse). Isolate `blst` FFI behind safe wrappers with `// SAFETY:` (`00` §7.6).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test golden_bls` → expect PASS (`golden::bls_sign_pop` green).
- [ ] **Step 5 — Commit:** `git commit -m "M0.19: BLS12-381 min_pk sign/agg/PoP/verify (golden)"`

---

### Task M0.20: Staking cert generation + strict parse + NodeID-from-cert
**Crate:** `ava-crypto` (+ `ava-types::NodeId`) · **Depends on:** M0.13, M0.5 · **Spec:** `03` §3.6 (staking certs, NodeID); `25` §2.1, §8.1
**Files:**
- Create: `crates/ava-crypto/src/staking/{mod.rs,tls.rs,parse.rs,verify.rs,certificate.rs}`
- Modify: `crates/ava-types/src/node_id.rs` (add `node_id_from_cert`) OR place in `ava-crypto` and `From<[u8;20]>` for `NodeId` (cycle-free: `ava-crypto → ava-types`)
- Test: `crates/ava-crypto/tests/golden_nodeid.rs`

- [ ] **Step 1 — Red:** Write `golden::nodeid_from_cert` (EXIT-GATE) loading `tests/vectors/crypto/nodeid.json` (Go-generated staking certs + expected NodeID). Assert `node_id_from_cert(cert_der)` == expected `NodeId` for each; assert the strict parser **rejects** RSA-3072 / exponent≠65537 / oversize (>2 KiB) certs exactly as Go (`large_rsa_key` case), and accepts P-256 ECDSA + RSA-2048/4096(exp 65537).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test golden_nodeid` → expect failure: `node_id_from_cert`/`staking::parse` undefined.
- [ ] **Step 3 — Green:**
  - `node_id_from_cert(cert_der) = NodeId::from(pubkey_bytes_to_address(cert_der))` = ripemd160(sha256(whole DER)) (`03` §3.6 / `25` §2.1). Lives in `ava-crypto` (depends on `ava-types::NodeId` + hashing).
  - `tls.rs`: `new_cert_and_key_bytes` via `rcgen` with the exact template (ECDSA P-256, `SerialNumber=0`, `NotBefore` = the Go `January,0` instant (Dec 31 1999), `NotAfter = now+100y`, `KeyUsage=DigitalSignature`, no SAN); PEM `CERTIFICATE` + PKCS#8 `PRIVATE KEY`; files `0o400`, dir `0o700`.
  - `parse.rs`: strict ASN.1 SPKI walk via `x509-parser` **plus** explicit checks: `MAX_CERTIFICATE_LEN = 2*1024` (`CertificateTooLarge`); RSA modulus exactly 2048/4096 + exponent 65537 + positive odd modulus (`UnsupportedRSAModulusBitLen`/`UnsupportedRSAPublicExponent`); ECDSA P-256 only (`FailedUnmarshallingEllipticCurvePoint`); unknown alg → `UnknownPublicKeyAlgorithm`. `Certificate { raw, public_key }`.
  - `verify.rs`: `check_signature(cert, msg, sig)` = `sha256(msg)` then RSA-PKCS1v15/SHA-256 or ECDSA `VerifyASN1`.
  - Extend crypto `Error` with the cert family (`25` §7.1).
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test golden_nodeid` → expect PASS (`golden::nodeid_from_cert` green).
- [ ] **Step 5 — Commit:** `git commit -m "M0.20: staking cert gen + strict parse + NodeID-from-cert (golden)"`

---

### Task M0.21: BLS `Signer` trait + `LocalSigner` lifecycle (file/zeroize)
**Crate:** `ava-crypto` · **Depends on:** M0.19 · **Spec:** `25` §3.1–§3.2 (Signer trait, LocalSigner); `25` §6 (zeroize, perms)
**Files:**
- Create: `crates/ava-crypto/src/bls/signer.rs`, `crates/ava-crypto/src/bls/local_signer.rs`
- Test: `crates/ava-crypto/tests/local_signer.rs`

> `RpcSigner` (tonic over `proto/signer`) is **deferred** to the milestone owning proto codegen / `ava-vm-rpc` (M0 has no proto build per M0.1 note). Record as N/A here.

- [ ] **Step 1 — Red:** Write `unit::local_signer_roundtrip`: `LocalSigner::generate()`; `to_file(tmp)` writes 32 bytes with mode `0o400` (dir `0o700`) on Unix; `from_file` reloads the same public key; `from_file_or_persist_new` creates-then-reuses; a Go-written 32-byte `signer.key` (committed fixture) loads to the expected pubkey; signing via the `Signer` trait (`sign`/`sign_proof_of_possession`) verifies.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-crypto --test local_signer` → expect failure: `LocalSigner`/`Signer` undefined.
- [ ] **Step 3 — Green:** `signer.rs`: object-safe `#[async_trait] Signer { public_key(&self)->&PublicKey; sign; sign_proof_of_possession; shutdown }` (`25` §3.1). `local_signer.rs`: `LocalSigner { sk: Zeroizing<SecretKeyBytes>, pk }` with `generate/from_bytes/from_file/to_file/from_file_or_persist_new` (`25` §3.2): 32-byte big-endian `SecretKey::serialize` file format (NOT PEM), `0o400`/`0o700` perms, IKM + key zeroized. `Signer` impl routes `sign`→SIG DST, `sign_proof_of_possession`→POP DST. Add `Error::FailedSecretKeyDeserialize`.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-crypto --test local_signer` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.21: BLS Signer trait + LocalSigner lifecycle (zeroize, 0o400)"`

---

### Task M0.22: `ava-version` `Application` + `Compatibility`
**Crate:** `ava-version` · **Depends on:** M0.5 · **Spec:** `03` §5.1 (`version/`); `26` (version taxonomy)
**Files:**
- Create: `crates/ava-version/Cargo.toml`, `crates/ava-version/src/lib.rs`, `crates/ava-version/src/application.rs`, `crates/ava-version/src/compatibility.rs`
- Test: `crates/ava-version/tests/version.rs`

- [ ] **Step 1 — Red:** Write `unit::application_display_compare`: `CURRENT.display() == "avalanchego/<maj>.<min>.<patch>"`, `semantic() == "v…"`, `compare` orders major→minor→patch; constants `RPC_CHAIN_VM_PROTOCOL == 45`, `CLIENT == "avalanchego"`; `Compatibility` accepts a peer `>= MINIMUM_COMPATIBLE` and applies `MinCompatibleAfterUpgrade` once `UpgradeTime` passed.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-version --test version` → expect failure: types undefined.
- [ ] **Step 3 — Green:** `application.rs`: `Application { name, major, minor, patch }` + `display/semantic/compare`; constants `CLIENT`, `RPC_CHAIN_VM_PROTOCOL=45`, `CURRENT_DATABASE`, `CURRENT`, `MINIMUM_COMPATIBLE`, `PREV_MINIMUM_COMPATIBLE` (pin to the Go tree at port time, doc-comment the Go path; `03` §5.1). `compatibility.rs`: port the version-vs-upgrade-time comparison. Wire `avalanchers --version` (M0.1) to use `ava_version::CURRENT`'s numeric version while keeping the local `avalanchers/` prefix (the wire client string `CLIENT` stays `avalanchego`); resolve the M0.1 TODO.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-version --test version && cargo run -p avalanchers -- --version` → expect PASS + correct version string.
- [ ] **Step 5 — Commit:** `git commit -m "M0.22: ava-version Application + Compatibility; wire --version"`

---

### Task M0.23: `UpgradeConfig` + `Fork` + activation schedule (protocol constants)
**Crate:** `ava-version` · **Depends on:** M0.22, M0.5 · **Spec:** `03` §5.2 + §11 (upgrade gating, verbatim tables); `15` (constants)
**Files:**
- Create: `crates/ava-version/src/upgrade.rs`
- Test: `crates/ava-version/tests/golden_upgrade.rs`

- [ ] **Step 1 — Red:** Write `golden::upgrade_activation` loading `tests/vectors/upgrade/activation.json` (per `03` §11.3): for `networkID ∈ {Mainnet,Fuji}` and each fork, the activation boolean at `{forkTime-1ns, forkTime, forkTime+1ns}` and `fork_at(t)`. Assert `is_active(fork, t)` (== `t >= fork_time`, inclusive at boundary) matches every committed row; assert `validate()` accepts each shipped config and rejects a hand-swapped out-of-order one.
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-version --test golden_upgrade` → expect failure: `UpgradeConfig`/`Fork` undefined.
- [ ] **Step 3 — Green:** Implement per `03` §5.2 + §11.2: `UpgradeConfig` with all phase times (`DateTime<Utc>`) + the three non-time side params (`apricot_phase_4_min_p_chain_height: u64`, `cortina_x_chain_stop_vertex_id: Id`, `granite_epoch_duration: Duration`). `Fork` enum (15 time-gated phases, `Ord` == chronological) + `Fork::ALL`. `fork_time`, `is_active` (`t >= fork_time`), `fork_at` (rev scan), `validate` (15 time fields monotonic non-decreasing; side params excluded → `Error::InvalidUpgradeTimes`). Per-phase `is_*_activated` thin forwarders. `get_config(network_id)` → Mainnet/Fuji/Default. **Verbatim constants** from the §5.2/§11.2 tables (Mainnet/Fuji instants, `InitiallyActiveTime=2020-12-05 05:00 UTC`, `UnscheduledActivationTime=9999-12-01`, the side-param values per network) with Go-path doc-comments.
- [ ] **Step 4 — Confirm green:** Run `cargo test -p ava-version --test golden_upgrade` → expect PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.23: UpgradeConfig + Fork + activation schedule (golden)"`

---

### Task M0.24: Per-crate contracts — proptest suites, regressions, fuzz targets, PORTING.md
**Crate:** all M0 crates · **Depends on:** the crate each covers (M0.5–M0.23) · **Spec:** `02` §4 (proptest mandatory), §8 (fuzz), §10.1 (PORTING.md), §13 (cross-spec contract)
**Files:**
- Create: `crates/ava-codec/tests/proptests.rs`, `crates/ava-types/tests/proptests.rs`, `crates/ava-crypto/tests/proptests.rs`, `crates/ava-utils/tests/proptests.rs`, `crates/ava-version/tests/proptests.rs`
- Create committed `crates/<crate>/proptest-regressions/` dirs
- Create fuzz crates: `crates/ava-codec/fuzz/`, `crates/ava-utils/fuzz/` (cb58) — `libfuzzer-sys` + `arbitrary`
- Create: `crates/<crate>/tests/PORTING.md` for each M0 crate

- [ ] **Step 1 — Red:** Write `prop::codec_roundtrip` (EXIT-GATE, `cases = 4096`) in `ava-codec/tests/proptests.rs`: `decode(encode(x)) == x` for an `arb`-generated derived type, plus `decode_never_panics` over arbitrary `&[u8]`. Add per-crate properties from `02` §4.2 (ids/cb58 round-trip; safemath checked-ops vs `u128` reference; `Bits` set algebra; secp sign→verify / low-S; BLS aggregate==individual; sampler without-replacement never repeats).
- [ ] **Step 2 — Confirm red:** Run `cargo test -p ava-codec --test proptests prop::codec_roundtrip` → expect failure: strategy/`arb_*` undefined.
- [ ] **Step 3 — Green:** Implement `arb_*` strategies + the property bodies per crate. Set `ProptestConfig { cases: 4096, .. }` for `ava-codec`. Add `[[bin]]` fuzz targets: `ava-codec` decode-every-type + round-trip-differential (`02` §8 sketch); `ava-utils` cb58 round-trip. Commit empty/seeded `proptest-regressions/` so the corpus persists (`02` §4.1). Seed each `tests/PORTING.md` with the Go package's test list (rows: Go test → Rust counterpart/`na` → status) per `02` §10.1.
- [ ] **Step 4 — Confirm green:** Run `cargo nextest run --workspace` (proptests run) and `cargo +nightly fuzz build` (fuzz crates compile) → expect PASS / build OK.
- [ ] **Step 5 — Commit:** `git commit -m "M0.24: per-crate proptest suites + fuzz targets + PORTING.md scaffolds"`

---

### Task M0.25: Milestone exit gate
**Crate:** workspace · **Depends on:** M0.1–M0.24 · **Spec:** all M0 specs; `02` §13 (contract); BUILDABLE-&-GREEN invariant
**Files:**
- Modify: every `crates/<crate>/tests/PORTING.md` (mark ported rows)
- Modify: `.config/nextest.toml` if needed for the exit suite

- [ ] **Step 1 — Red:** Add (or confirm) a workspace exit-suite assertion: a `tests/` or per-crate check that every named exit test exists and is collected. The "red" is any of the named tests failing or the binary not answering `--version`/`--help`.
- [ ] **Step 2 — Confirm red:** Run the full gate (below) before final fixes → expect at least one failure if any prior task regressed.
- [ ] **Step 3 — Green:** Resolve any failures. Update each `tests/PORTING.md` to mark M0 rows `ported` (no `wip`). Ensure `#![forbid(unsafe_code)]` everywhere except the audited `secp256k1`/`blst` wrapper modules; ensure license headers on all `.rs`.
- [ ] **Step 4 — Confirm green:** Run the complete gate:
  ```sh
  cd /Users/rahul.muttineni/avalanche-rs
  cargo build --workspace
  cargo build -p avalanchers
  cargo run -p avalanchers -- --version   # prints avalanchers/<ver>
  cargo run -p avalanchers -- --help      # exits 0
  cargo nextest run --profile ci --workspace
  cargo clippy --workspace -- -D warnings
  # named exit tests:
  cargo nextest run --profile ci \
    golden::codec_all_types golden::cb58_addr_bech32 golden::bls_sign_pop \
    golden::secp_recover golden::nodeid_from_cert golden::sampler_mt19937_stream \
    prop::codec_roundtrip conformance::run_codec_suite
  ```
  → expect ALL PASS.
- [ ] **Step 5 — Commit:** `git commit -m "M0.25: milestone exit gate — workspace green, R1 retired, PORTING.md updated"`

---

## Spec coverage check

| Spec section / requirement | Task(s) | Notes / deferrals |
|---|---|---|
| `03` §0 crate dep order, Packer placement, cycle avoidance | M0.1, M0.5, M0.11, M0.20 | CB58 placed in `ava-utils` to break `types→crypto` cycle (M0.6/M0.11 note) |
| `03` §1.1 fixed IDs (`Id`/`ShortId`/`NodeId`, prefix/append/xor/bit) | M0.5 | |
| `03` §1.1 CB58 Display/FromStr/JSON (null no-op) | M0.6 | |
| `03` §1.2 bit helpers (`equal_subset`/`first_difference_subset`) | M0.5 | consensus-affecting masking |
| `03` §1.3 RequestId + Aliaser | M0.7 | |
| `03` §1.4 network constants + get_hrp | M0.8 | |
| `03` §2.1 Packer (BE, sticky errors) | M0.14 | |
| `03` §2.2 Codec/Manager/Registry + version framing + ExtraSpace | M0.16 | |
| `03` §2.3 linearcodec typeID registry (SkipRegistrations) | M0.16 | typeID golden table dumped from Go |
| `03` §2.4 reflectcodec rules → derive (kinds, slices, maps, zero-len guard) | M0.15 | |
| `03` §2.5 derive surface (`#[codec(...)]` attrs, traits, size()) | M0.15 | |
| `03` §3.1 hashing (sha256/ripemd160/keccak/checksum/address) | M0.13 | |
| `03` §3.2 CB58 + formatting (Hex/HexC/HexNC) | M0.11 (CB58), M0.17 (formatting) | |
| `03` §3.3 bech32 chain-prefixed addresses | M0.17 | |
| `03` §3.4 secp256k1 (recoverable, low-S, recover→address, eth_address) | M0.18 | |
| `03` §3.5 BLS12-381 min_pk (sign/agg/PoP/verify, DSTs) | M0.19 | |
| `03` §3.6 staking cert gen + strict parse + NodeID-from-cert | M0.20 | |
| `03` §4.1 sampler determinism (Source, Uint64Inclusive, 3 samplers) | M0.4 (Uint64Inclusive), M0.10 (samplers) | |
| `03` §4.2 set/bag/Bits/UniqueBag | M0.9 | |
| `03` §4.3 LinkedHashmap/safemath/units; window/timer/bloom | M0.9 | window/timer/bloom = stubs only; matured in consumer specs (`05`/`06`) — deferred |
| `03` §5.1 version Application + Compatibility | M0.22 | numbers pinned to Go tree at port time |
| `03` §5.2 + §11 UpgradeConfig/Fork/activation (verbatim constants) | M0.23 | |
| `03` §7 error model (per-crate `thiserror` enums) | M0.5, M0.11, M0.13, M0.14, M0.16, M0.17–M0.21, M0.23 | variants seeded per crate as surfaces land |
| `03` §10 Deterministic RNG (R1) — MT19937/-64 hand-port | **M0.3** | **R1 retired; TDD entry point** |
| `03` §10.4 RNG/sampler golden-vector plan | M0.2, M0.3, M0.4, M0.10 | |
| `15` §4.1 linear codec wire rules | M0.14, M0.15, M0.16 | |
| `15` §4.4 CB58/NodeID/bech32/hex string encodings | M0.6, M0.11, M0.17 | |
| `15` §6 codec version bytes + ExtraSpace mandatory | M0.16 | |
| `24` Part A determinism hazards (#1 no-HashMap-serialize, #2 no floats, #3 checked arith, #4 RNG, #5 clock) | M0.3 (#4), M0.9 (#3), M0.12 (#5), M0.15 (#1/#9 map sort), throughout | xtask/clippy enforcement (#2,#5,#8) owned by X-cross-cutting |
| `24` Part B injectable Clock (Real/Mock, MAX_UNIX_SECS, virtual time) | M0.12 | timeout-mgr/skew tests deferred to `05`/`06` consumers |
| `25` §2 NodeID + PoP derivation | M0.20 (NodeID), M0.19 (PoP math) | `ProofOfPossession` registry type → `ava-platformvm` (deferred, `08`) |
| `25` §3.1–§3.2 BLS Signer trait + LocalSigner lifecycle | M0.21 | |
| `25` §3.3 RpcSigner (tonic / proto/signer) | — | **Deferred** — needs proto codegen (`ava-vm-rpc`); recorded N/A in M0.21 |
| `25` §4 signed-IP construction | — | **Deferred** to `ava-network` (`05`) |
| `25` §5 config loading precedence | — | **Deferred** to `ava-config` (`12`/`13`) |
| `21` protocol constants (units, fee skim) | M0.9 (units) | full fee/gas math deferred to `21` consumers (SAE/EVM) |
| `02` §4 proptest per crate + committed regressions | M0.24 | |
| `02` §6 golden vectors per surface (codec/ids/crypto/sampler/rng/upgrade) | M0.2 + each golden task | extract-vectors maturation owned by X-cross-cutting |
| `02` §7 generic `run_codec_suite` (codectest) | M0.16 | |
| `02` §8 cargo-fuzz targets (codec, cb58) | M0.24 | |
| `02` §10.1 tests/PORTING.md per crate | M0.24, M0.25 | |
| `00` §3 workspace bootstrap + `avalanchers` binary | M0.1 | |
| Dev-env (Nix/Bazel/cargo-deny/CI/xtask/run_task.sh/differential harness/extract-vectors maturation) | — | **Owned by X-cross-cutting.md** — not duplicated here |

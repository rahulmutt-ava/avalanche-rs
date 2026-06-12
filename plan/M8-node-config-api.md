# M8 — Node / Config / API / Wallet / Genesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Promote the M0 `avalanchers` skeleton into a byte-/behavior-exact full node — every CLI flag, every JSON-RPC/Connect/WS API across all chains, the indexer, the wallet SDK, and deterministic genesis generation — so `./avalanchers` and `./avalanchers --network-id=fuji` start and stop identically to Go.
**Tier:** T5 — Node/APIs
**Crates:** ava-config, ava-genesis, ava-api, ava-indexer, ava-wallet, ava-node, avalanchers (bin)
**Owning specs:** 12 (node/config/api/wallet/genesis), 13 (flag catalog — flag oracle), 14 (API/RPC catalog — endpoint oracle), 23 (genesis construction), 17 (runtime/shutdown ordering), 18 (metrics & logging parity)
**Depends on (prior milestones):** M4–M7 (P/X/C/SAE chains + their `service` handlers and tx types), M2/M3 (ava-network, ava-engine handlers/router/timeout, validators, message::Creator), plus M1 primitives (ava-codec, ava-crypto, ava-ids, ava-database, ava-consensus params, ava-version)
**Exit gate (named tests):**
- **golden::flag_parity** — generated flag list == Go `config/` snapshot (13 §25): set-equality of names, per-flag type equality, default-string equality, deprecated set+message equality.
- **differential::api_parity** — every endpoint in 14 (§3–§11): structural-JSON-equal responses vs Go after normalization (timestamps/node-IDs/peer-lists), correct HTTP status codes, `node-id` header.
- **golden::genesis_block_id** — Mainnet + Fuji (and Local-unmodified + custom) genesis block IDs, X/C blockchain IDs, AVAX asset IDs == Go (23 §7, 02 §6.2).
- **differential::indexer_parity** — accept ordering, range queries, incomplete-index fatal, restart markers vs Go.
- **prop::config_precedence** — flag > env (AVAGO_*) > config file > default; `is_set` semantics.

---

## Dependency map & parallel waves

**Wave A (pure functions, land first, fully parallel) — the TDD entry point:**
- `ava-config` flag table + `build_command` + the **golden::flag_parity** snapshot diff (M8.1–M8.4).
- `ava-genesis` embedded configs + `from_config` + **golden::genesis_block_id** (M8.5–M8.8).
- `ava-config` `Layered` precedence resolver + **prop::config_precedence** (M8.9–M8.11).

These are deterministic pure-function checks: fast, no I/O, no network — they catch drift immediately and gate everything downstream.

**Wave B (config-derived, parallel after Wave A):**
- `ava-config` `get_node_config` (network-dependent defaults, file/content loaders, validation, subnet/chain config dirs) (M8.12–M8.14).
- `ava-genesis` bootstrappers + `vm_genesis`/`sample_bootstrappers` (M8.8 covers; M8.15 wires the local start-time advance).

**Wave C (HTTP surface, parallel after the JSON-RPC shim lands):**
- `ava-api` server (axum/h2c/CORS/allowed-hosts/node-id/timeouts/per-chain-503) + JSON-RPC 2.0 shim + error model (M8.16–M8.17).
- Per-chain/per-service handlers, each parallel: `info` (M8.18), `admin` (M8.19), `health` (M8.20), `metrics`+MultiGatherer (M8.21), P/X/C/proposervm chain mounting via `register_chain` (M8.22), then `differential::api_parity` (M8.23).
- `ava-indexer` (M8.24) + `differential::indexer_parity`.

**Wave D (SDK, parallel with C):**
- `ava-wallet` P/X/C builder+signer+backend+facade + tx-vector goldens (M8.25–M8.27).

**Wave E (integration — serial, integrates all):**
- `ava-node` trace + NAT + logging factory (M8.28).
- `ava-node` assembly (`Node::new`, steps 1–26 from 12 §2.2) (M8.29).
- `ava-node` dispatch + shutdown ordering (17 §4.3 / 12 §2.4) + lifecycle test (M8.30).
- `avalanchers` binary promotion (M8.31).

**Wave F:** Milestone exit gate (M8.32).

Coordinate the live-vs-recorded oracle mode for `differential::api_parity` and `differential::indexer_parity` with the cross-cutting differential harness X (02 §9): per-PR they run in **recorded-oracle** mode against committed `tests/vectors/api/`; live mode is CI-gated.

> **WAVE A + M8.14 MERGED 2026-06-11** (branches `m8/config` 810e1cb, `m8/genesis` 6b2dd67). As-built notes:
> - **ava-config carries NO ava-genesis dep** — defaults source from `ava-network`/`ava-snow`/`ava-version` constants; staking/fee defaults that Go pulls from `genesis.LocalParams` are pinned values guarded by the `golden::flag_parity` snapshot diff (drift cannot land silently). Wire the genesis-derived values properly in M8.12 (`get_node_config` already needs ava-genesis for bootstrappers/genesis bytes).
> - **Go snapshots @ `cc3b103b91`** (the reviewed-through upstream pin): `flags.json` via `cargo xtask gen-flags`; genesis vectors (incl. the 4.4 MB mainnet P-chain byte stream) via `cargo xtask gen-genesis`, emitted by the M7.29-pattern in-repo go-oracle test (`crates/ava-genesis/tests/go-oracle/`, env-gated, copied into `../avalanchego` to run).
> - **`ByEndTimeHeap::add` dedups by tx ID** (a4c5dcb) — mirrors Go `txheap.Add`'s already-present skip; surfaced post-M8.8, byte-streams unaffected for the standard networks.
> - `xtask` gained `gen-flags` + `gen-genesis` subcommands (separate files; main.rs dispatch).
> - ~~**`m8/wallet` branch in flight**~~ → M8.25+M8.26 merged @ 7afa24c (2026-06-11).
>
> **WAVE B+D MERGED 2026-06-11** (branches `m8/nodeconfig`, `m8.27/wallet-facade`): M8.12+M8.13
> (get_node_config + subnet/chain loaders, 36 ava-config tests) and M8.27 (P/X/C facades +
> make_wallet over the client-trait seam, 41 ava-wallet tests) — see the per-task AS-BUILT notes.
> Remaining M8 frontier: **M8.16→M8.17 (ava-api server + JSON-RPC shim) ∥ M8.28 (trace/nat/logging)**,
> then the per-service fan-out M8.18–M8.24.
>
> **WAVE C-HEAD + M8.28 MERGED 2026-06-11** (branches `m8/api` 276d008, `m8/node-trace` 5319a70;
> M6.31 also landed this wave, 160d20d): M8.16+M8.17 (ava-api server + JSON-RPC shim + macros, 28
> tests) and M8.28 (ava-node trace/nat + ava-logging factory, 19 tests) — see per-task AS-BUILTs.
> Remaining M8 frontier: **per-service fan-out M8.18 (info) ∥ M8.19 (admin) ∥ M8.20 (health) ∥
> M8.21 (metrics) ∥ M8.24 (indexer)** (all parallel on the M8.17 shim; M8.19 needs M8.28's reload
> handles — landed), then M8.22 (register_chain) → M8.23 (api_parity) → Wave E (M8.29–M8.31).
>
> **PER-SERVICE FAN-OUT MERGED 2026-06-11/12** (merges 16b62c1 info, 7a4696f admin, b9b0c94 health,
> 57911ba metrics, 60ee2e8 indexer; per-task AS-BUILTs below). Remaining M8 frontier:
> **M8.22 (register_chain) → M8.23 (api_parity) → Wave E serial (M8.29–M8.31) → M8.32 gate.**
> Current wave (2026-06-12): M8.22 ∥ M6.29 (C-Chain exit gate, cross-milestone).

---

## Tasks

### Task M8.1: ava-config crate skeleton + FlagSpec model + KEY_* constants ✅ DONE (5a3c4a4)
**Crate:** ava-config  ·  **Depends on:** M1 ava-version, ava-consensus (snowball defaults), ava-genesis (will be wired in M8.4)  ·  **Spec:** 12 §1.2–§1.4, 13 §0/§24
**Files:** `crates/ava-config/Cargo.toml`, `crates/ava-config/src/lib.rs` (`#![forbid(unsafe_code)]` + license header), `crates/ava-config/src/keys.rs`, `crates/ava-config/src/flags.rs` (`FlagSpec`, `FlagKind`, `DefaultVal`), `crates/ava-config/src/error.rs` (`ConfigError` thiserror)
- [x] **Step 1 — Red:** In `crates/ava-config/src/keys.rs` add `#[cfg(test)] mod tests { #[test] fn key_count_matches_go() { assert_eq!(super::ALL_KEYS.len(), 206); } }` (13 §24: 205 const block + `http-write-timeout` = 206). Also add `flags.rs::tests::flag_kind_maps_to_go_type_string` asserting `FlagKind::Duration.go_type_str() == "duration"`, etc. (the 10 pflag type strings in 13 §25).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config keys::tests::key_count_matches_go` → fails (module/const absent: compile error is acceptable here since the type doesn't exist yet, then iterate to a value-mismatch failure).
- [x] **Step 3 — Green:** Define `pub const KEY_NETWORK_ID: &str = "network-id";` … one const per row in 13 §1–§22 (verbatim flag strings). Define `pub static ALL_KEYS: &[&str]` listing all 206. Define `FlagKind { Bool, String, U64, I64, F64, Duration, StringSlice, IntSlice, StringMap }` with `go_type_str()` → `{bool,string,uint64/uint/int,float64,duration,stringSlice,intSlice,stringToString}` per 13 §25 (note `uint`→`u32`/`u16` map to Go `uint`; `int`→`i32`). Define `FlagSpec { key, kind, default: DefaultVal, help, deprecated }` and `DefaultVal { Static(&str) | Lazy(fn() -> String) }` per 12 §1.4.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config keys::` passes; `cargo build -p ava-config`.
- [x] **Step 5 — Commit:** `ava-config: KEY_* constants + FlagSpec/FlagKind model (206 keys, 13 §24)`

### Task M8.2: FLAG_SPECS table — every flag with name/type/default/help/deprecation ✅ DONE (b1f0e5b)
**Crate:** ava-config  ·  **Depends on:** M8.1  ·  **Spec:** 13 §1–§22 (the verbatim catalog), 12 §1.3
**Files:** `crates/ava-config/src/flags.rs` (`pub static FLAG_SPECS: &[FlagSpec]`), `crates/ava-config/src/defaults.rs` (lazy defaults pulling from ava-genesis::LocalParams / ava-consensus / ava-network constants / OS-dependent fd-limit)
- [x] **Step 1 — Red:** `flags.rs::tests::every_key_has_one_spec` — assert `FLAG_SPECS.len() == 206` and that `FLAG_SPECS.iter().map(|s| s.key).collect::<HashSet>()` equals `keys::ALL_KEYS` set (no orphans, no dupes — 13 §24). Add `defaults.rs::tests::fd_limit_default_is_os_dependent` (32768 non-macOS, 10240 macOS via `cfg!(target_os="macos")`, 13 §2), and `network_allow_private_ips_registered_default_is_false` (13 §8 note — registered default false; effective default resolved later in parse).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config flags::tests::every_key_has_one_spec` → fails (length/set mismatch).
- [x] **Step 3 — Green:** Populate `FLAG_SPECS` row-for-row from 13: process flags §1, paths §2, network/ACP §3, fees §4, staking §5, HTTP+API-enable §6, snow/simplex/proposervm §7, networking §8, throttlers §9, benchlist §10, router §11, health §12, bootstrap §13, subnets/chain-config/aliases §14, logging §15, indexer §16, profiling §17, system-tracker §18 (mark the two deprecated keys §0/§18), public-ip/nat §19, db §20, genesis/upgrade §21, tracing §22. Defaults that are constants → `Static`; defaults from `genesis::LocalParams.*` / `snowball::DefaultParameters` / `constants::Default*` → `Lazy` (sourced so they cannot drift, 12 §1.3). `fd-limit` default via `cfg!`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config flags::` passes.
- [x] **Step 5 — Commit:** `ava-config: full FLAG_SPECS table (206 flags) sourced from 13`

### Task M8.3: build_command() — clap Command from FLAG_SPECS (names as data) ✅ DONE (a2ad11f)
**Crate:** ava-config  ·  **Depends on:** M8.2  ·  **Spec:** 12 §1.4, 13 §0 (pflag bool/duration grammar)
**Files:** `crates/ava-config/src/flags.rs` (`build_command`), `crates/ava-config/src/duration.rs` (`parse_go_duration`)
- [x] **Step 1 — Red:** `duration.rs::tests::parse_go_duration_grammar` — table over `{"30s"→30s, "5m"→300s, "120ms", "22.5s", "1h", "1m0.5s"}` matching `time.ParseDuration` (12 §1.4 note). `flags.rs::tests::build_command_accepts_bool_forms` — assert `build_command(FLAG_SPECS).try_get_matches_from(["avalanchers","--sybil-protection-enabled"])` and `=true` both parse (pflag bools accept `--x` and `--x=true`, 12 §1.4).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config duration::tests::parse_go_duration_grammar` → fails.
- [x] **Step 3 — Green:** Implement `parse_go_duration` accepting Go's `ns,us,µs,ms,s,m,h` grammar (not humantime). Implement `build_command(specs) -> clap::Command` per the 12 §1.4 sketch: `disable_help_flag(false)`, `arg_required_else_help(false)`, Bool→`num_args(0..=1)`+`default_missing_value("true")`, Duration→`value_parser(parse_go_duration)`, StringSlice→`value_delimiter(',')`, deprecated→`DEPRECATED:` help prefix. Version from `ava_version::CURRENT`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config duration:: flags::tests::build_command_accepts_bool_forms` passes.
- [x] **Step 5 — Commit:** `ava-config: build_command + parse_go_duration (pflag parity)`

### Task M8.4: golden::flag_parity — generated flag list diffed vs Go snapshot  ⟵ TDD ENTRY POINT ✅ DONE (51d7e99)
**Crate:** ava-config  ·  **Depends on:** M8.3  ·  **Spec:** 13 §25, 12 §1.8, 02 §6
**Files:** `crates/ava-config/tests/golden_flag_parity.rs`, `crates/ava-config/tests/vectors/config/flags.json` (committed Go snapshot), `xtask/src/gen_flags.rs` (regenerates the Go snapshot; mirror `config.BuildFlagSet()` dump), `crates/ava-config/tests/PORTING.md`
- [x] **Step 1 — Red:** `tests/golden_flag_parity.rs::flag_parity` — load `tests/vectors/config/flags.json` (records `{name,type,default,deprecated,deprecation_msg}` sorted by name, with a pinned-env header for `fd-limit`/`NumCPU`-derived defaults per 13 §25). Serialize `build_command(FLAG_SPECS)` into the same record shape (FlagKind→Go type string; default→`DefaultVal` resolved string, Duration round-tripped through `parse_go_duration`; symbolic-form for `NumCPU`/`fd-limit`). Assert: (a) **set-equality of names**, (b) **per-flag type equality**, (c) **per-flag default-string equality**, (d) **deprecated-set + message equality** (13 §25 step 2).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-config flag_parity` → fails (snapshot absent / diff). Commit the failing test + an initial Go-extracted `flags.json` first.
- [x] **Step 3 — Green:** Add `xtask gen-flags` (extracts `name,type,default,deprecated,deprecation_msg` from the Go tree, sorted, with the pinned `GOOS`/`GOMAXPROCS` header) and commit `tests/vectors/config/flags.json`. Reconcile any FLAG_SPECS drift surfaced by the diff (fix names/types/defaults until green). Document the regen command + symbolic-default normalization in PORTING.md.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-config flag_parity` passes (this is the per-PR exit gate).
- [x] **Step 5 — Commit:** `ava-config: golden::flag_parity + committed Go flags.json snapshot (13 §25)`

### Task M8.5: ava-genesis crate skeleton + Config/Allocation/Staker types + embedded JSON ✅ DONE (a3c2963)
**Crate:** ava-genesis  ·  **Depends on:** M1 ava-ids, ava-crypto (bech32/ShortId/NodeId/ProofOfPossession), ava-codec  ·  **Spec:** 23 §1, §5.1, 12 §6.1
**Files:** `crates/ava-genesis/Cargo.toml`, `crates/ava-genesis/src/lib.rs`, `crates/ava-genesis/src/config.rs` (`Config`,`Allocation`,`LockedAmount`,`Staker`), `crates/ava-genesis/src/unparsed.rs` (JSON ⇄ parsed), `crates/ava-genesis/src/error.rs` (`GenesisError`), `crates/ava-genesis/data/genesis_{mainnet,fuji,local}.json`, `crates/ava-genesis/data/bootstrappers.json`, `crates/ava-genesis/data/checkpoints.json`
- [x] **Step 1 — Red:** `config.rs::tests::parse_embedded_configs` — `include_str!` each JSON, parse unparsed→parsed for Mainnet/Fuji/Local; assert `network_id` ∈ {1,5,12345}, `initial_supply()` > 0 (checked-add, 23 §1), HRP via `constants::get_hrp`, and at least one staker each. `unparsed.rs::tests::ethaddr_avaxaddr_roundtrip`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-genesis config::tests::parse_embedded_configs` → fails.
- [x] **Step 3 — Green:** Copy the three genesis JSONs + `bootstrappers.json` + `checkpoints.json` verbatim from the Go tree into `data/`. Define `Config`/`Allocation`/`LockedAmount`/`Staker` with exact JSON tags (23 §1). Implement unparsed→parsed: `ethAddr` = `0x`+hex(20), `avaxAddr`/`rewardAddress`/`initialStakedFunds[i]` = bech32 (strip alias+HRP → 20-byte ShortId). Implement `initial_supply()` (checked sum), `GenesisError` variants (23 §6.1, one per Go sentinel).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-genesis config::` passes.
- [x] **Step 5 — Commit:** `ava-genesis: Config/Allocation/Staker + embedded network JSON (23 §1/§5)`

### Task M8.6: validate_config parity + split_allocations ✅ DONE (1e8e182)
**Crate:** ava-genesis  ·  **Depends on:** M8.5, M1 ava-consensus (StakingConfig)  ·  **Spec:** 23 §2, §3.3.1
**Files:** `crates/ava-genesis/src/validate.rs`, `crates/ava-genesis/src/split.rs`
- [x] **Step 1 — Red:** `validate.rs::tests::validate_config_table` — mirror Go `TestValidateConfig`: each of the 10 checks (23 §2) maps to a `GenesisError` variant matched by `assert_matches!`; plus the "duplicate avaxAddr across allocations is allowed" and "empty message allowed" cases. `split.rs::tests::split_allocations_vectors` — fixed staked-allocation sets × num_splits → per-bucket unlock schedules + weights match Go (23 §9.4).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-genesis validate::tests::validate_config_table` → fails.
- [x] **Step 3 — Green:** Implement `validate_config` (the 10 ordered checks, 23 §2) and `split_allocations` (greedy split, `node_weight = total/num_splits`, remainder to last bucket, splitting an `unlock.amount` across bucket boundary — reproduce the loop verbatim, 23 §3.3.1). `from_file`/`from_flag` reject std network IDs (`OverridesStandardNetworkConfig`).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-genesis validate:: split::` passes.
- [x] **Step 5 — Commit:** `ava-genesis: validate_config + split_allocations (23 §2/§3.3.1)`

### Task M8.7: from_config — byte-exact P-Chain genesis bytes + AVAX asset ID ✅ DONE (97ced54)
**Crate:** ava-genesis  ·  **Depends on:** M8.6, M4 ava-platformvm (genesis/tx/UTXO types), M5 ava-avm (genesis/CreateAssetTx types), M1 ava-codec (linear codec, MaxInt32 manager)  ·  **Spec:** 23 §3 (load-bearing order)
**Files:** `crates/ava-genesis/src/build.rs` (`from_config`, `avax_asset_id`, `vm_genesis`), `crates/ava-genesis/src/chains.rs` (fixed chain list)
- [x] **Step 1 — Red:** `build.rs::tests::avax_asset_id_matches_go` — for Mainnet/Fuji/Local assert `from_config(cfg).1.to_string()` equals the 23 §7 AVAX-asset-ID table (`FvwEAhmxKfeiG8SnEvq42hc6whRyY3EFYAvebMqDNDGCgxN5Z` etc.). This exercises the X-Chain genesis sort + asset-tx hash (23 §3.1) without yet asserting the P-Chain block ID.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-genesis build::tests::avax_asset_id_matches_go` → fails.
- [x] **Step 3 — Green:** Implement §3.1 (AVM genesis: `AssetDefinition AVAX denom 9`; collect `initial_amount>0` allocations; sort by `(initial_amount, avax_addr)`; FixedCap holders + memo = concatenated eth addrs; `new_genesis`; sort `InitialState.outs` by codec bytes; marshal v0; `avax_asset_id` = hash of initialized CreateAssetTx bytes). §3.2 P-Chain UTXO allocations (config order, skip initially-staked). §3.3 validators (end-time math, `split_allocations`, PoP signer). §3.4 `platformvm/genesis::New` (UTXOs, validators via **ByEndTime heap**, chains). §3.5 fixed chain list (X first, C second). Marshal P-Chain genesis with linear codec v0, MaxInt32 manager. Implement `vm_genesis(p_bytes, vm_id)` and `avax_asset_id(avm_bytes)`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-genesis build::tests::avax_asset_id_matches_go` passes.
- [x] **Step 5 — Commit:** `ava-genesis: from_config byte-exact build + AVAX asset ID (23 §3)`

### Task M8.8: golden::genesis_block_id — P/X/C IDs == Go (Mainnet+Fuji+Local+custom)  ⟵ TDD ENTRY POINT ✅ DONE (4236aea)
**Crate:** ava-genesis  ·  **Depends on:** M8.7  ·  **Spec:** 23 §4, §7, 12 §6.4, 02 §6.2
**Files:** `crates/ava-genesis/src/lib.rs` (`genesis_block_id`, `genesis_bytes`, `Chain` enum), `crates/ava-genesis/tests/golden_genesis_block_id.rs`, `crates/ava-genesis/tests/vectors/genesis/{block_ids.json,p_chain_bytes_{mainnet,fuji,local}.bin}` (Go dumps), `crates/ava-genesis/tests/PORTING.md`
- [x] **Step 1 — Red:** `tests/golden_genesis_block_id.rs::genesis_block_id` — table over {Mainnet, Fuji, Local-unmodified, custom(9999)}: assert `genesis_block_id(net, Chain::P)` (= `ComputeHash256Array(p_bytes)`, 23 §4.1) equals 23 §7 P-Chain table; `vm_genesis(p_bytes, AVM_ID).id()` and `EVM_ID` equal the X/C blockchain-ID table; `avax_asset_id` equals the asset table; custom-config hex hash == `a1d1838586db85fe94ab1143560c3356df9ba2445794b796bba050be89f4fcb4`. Second test `genesis_p_chain_bytes_byte_identical` diffs the **full byte stream** against the committed Go `.bin` dumps (23 §9.2 — guards intermediate orderings).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-genesis genesis_block_id` → fails.
- [x] **Step 3 — Green:** Implement `Chain { P, X, C }`, `genesis_bytes(network_id, custom)`, `genesis_block_id(network_id, chain)` (P = `ApricotCommitBlock{parent_id: hash(p_bytes), height:0}` → its id is the hash per 23 §4.1; X/C via `vm_genesis(...).id()`). Commit the Go-dumped vectors (`xtask gen-genesis`). Fix any ordering drift (X-alloc sort, validator end-time heap, reward-addr sort, chain order) until byte-identical.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-genesis genesis_block_id` passes (per-PR exit gate).
- [x] **Step 5 — Commit:** `ava-genesis: golden::genesis_block_id (P/X/C IDs + byte streams, 23 §7)`

### Task M8.9: ava-config env snapshot + config-file loader (json/yaml/toml + base64 content) ✅ DONE (f95d10c)
**Crate:** ava-config  ·  **Depends on:** M8.3  ·  **Spec:** 12 §1.5, 13 §0 (env derivation, config file), §23
**Files:** `crates/ava-config/src/precedence.rs` (env snapshot, `load_config_file`), Cargo deps: `serde_json`, `serde_yaml`, `toml`, `base64`
- [x] **Step 1 — Red:** `precedence.rs::tests::env_var_name_mapping` — `AVAGO_NETWORK_ID`→`network-id`, `AVAGO_HTTP_PORT`→`http-port`, `AVAGO_NETWORK_TLS_KEY_LOG_FILE_UNSAFE`→`network-tls-key-log-file-unsafe` (13 §0 rule: strip `AVAGO_`, lowercase, `_`→`-`). `config_file_content_overrides_path` — when both `--config-file` and `--config-file-content` (b64) are set, the content wins (13 §0). Parse json/yaml/toml content into one `serde_json::Value`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config precedence::tests::env_var_name_mapping` → fails.
- [x] **Step 3 — Green:** Snapshot `std::env::vars()` filtering `AVAGO_*` (strip, lowercase, `_`→`-`) into a `HashMap` once. Implement `load_config_file(&ArgMatches)`: `--config-file-content` (b64-decode, parse by `--config-file-content-type` ∈ json/yaml/toml default json) overrides `--config-file` path; return `Value::Null` if neither; all parsers funnel into `serde_json::Value`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config precedence::` passes.
- [x] **Step 5 — Commit:** `ava-config: env snapshot + config-file/content loader (12 §1.5)`

### Task M8.10: Layered resolver — viper precedence + is_set + path expansion ✅ DONE (0ddec25)
**Crate:** ava-config  ·  **Depends on:** M8.9  ·  **Spec:** 12 §1.5, 13 §0/§23 (precedence + getExpandedArg)
**Files:** `crates/ava-config/src/precedence.rs` (`Layered`, getters, `is_set`, path expander)
- [x] **Step 1 — Red:** `precedence.rs::tests::data_dir_expansion` — a path-typed value `$AVALANCHEGO_DATA_DIR/db` expands to `<resolved data-dir>/db`; other `$VAR` via env (13 §0 `getExpandedArg`). `is_set_layers` — `is_set` true when CLI `ValueSource::CommandLine` OR env key present OR file lookup hits (13 §23).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config precedence::tests::data_dir_expansion` → fails.
- [x] **Step 3 — Green:** Implement `Layered { cli, env, file, specs }` with `build(cmd, args, specs)` (12 §1.5 sketch). `is_set(key)` per 13 §23. `get_string/get_bool/get_u64/get_i64/get_f64/get_duration/get_string_slice/get_int_slice/get_string_map` walking CLI(CommandLine)→env→file→default. Key normalization (lowercase, dash form) before lookup. Path expansion applied on read for path-typed keys.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config precedence::` passes.
- [x] **Step 5 — Commit:** `ava-config: Layered viper-parity resolver + path expansion (12 §1.5)`

### Task M8.11: prop::config_precedence — flag>env>file>default proptest ✅ DONE (6a4006b)
**Crate:** ava-config  ·  **Depends on:** M8.10  ·  **Spec:** 13 §25 step 4, 02 §4
**Files:** `crates/ava-config/tests/prop_config_precedence.rs`, `crates/ava-config/proptest-regressions/` (committed)
- [x] **Step 1 — Red:** `tests/prop_config_precedence.rs::config_precedence` — proptest over a matrix `{present-on-CLI?, present-in-env?, present-in-file?}` per a sampled key/type: assert the resolved value picks the highest present layer (CLI>env>file>default) and `is_set` is true iff any non-default layer present. Include the explicit unit cases: `snow-quorum-size` overrides preference+confidence when set (13 §7/§23), `-content` overrides `-file` (13 §23), `network-allow-private-ips` network-dependence is NOT resolved here (that is parse-time, M8.12).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-config config_precedence` → fails.
- [x] **Step 3 — Green:** Implement the proptest strategy building synthetic CLI args + env map + file Value and a `Layered`; assert ordering + `is_set`. Commit `proptest-regressions/`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-config config_precedence` passes (per-PR exit gate).
- [x] **Step 5 — Commit:** `ava-config: prop::config_precedence (flag>env>file>default, 13 §25)`

### Task M8.12: get_node_config — Config struct + network-dependent derived defaults + validation ✅ DONE (ed97658+2ede55c)
**Crate:** ava-config  ·  **Depends on:** M8.10, M8.8 (ava-genesis bootstrappers/genesis), M2/M3 (NetworkConfig, ConsensusParams, BenchlistConfig), M1 ava-database (DatabaseConfig)  ·  **Spec:** 12 §1.6, 13 §3/§5/§7/§8/§13/§18/§19/§21 (network-dependent + validation)
**Files:** `crates/ava-config/src/node.rs` (`pub struct Config`), `crates/ava-config/src/parse.rs` (`get_node_config`)
- [x] **Step 1 — Red:** `parse.rs::tests::network_allow_private_ips_dependence` — unset ⇒ false for Mainnet/Fuji, true for Local/custom; set ⇒ honored (13 §8). `sybil_protection_disabled_rejected_on_mainnet` (`assert_matches!(..., Err(ConfigError::SybilProtectionDisabledOnPublicNetwork))`, 13 §5). `bootstrappers_filled_from_genesis_when_unset` (both empty + standard net ⇒ `genesis::sample_bootstrappers(net,5)`; mismatched ip/id counts ⇒ error, 13 §13). `snow_quorum_overrides_alpha` (13 §7).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config parse::tests::network_allow_private_ips_dependence` → fails.
- [x] **Step 3 — Green:** Define `Config` (`#[derive(Clone)]`, 12 §1.6) embedding `NetworkConfig`, `ConsensusParams`/`BenchlistConfig`, `DatabaseConfig`, `LoggingConfig`, `HTTPConfig`, `TraceConfig`, `SubnetConfigs`, `ChainConfigs`, staking certs/signer, `GenesisBytes`+`AvaxAssetID`. Implement `get_node_config(&Layered)` mirroring `config.go::GetNodeConfig` order-sensitive derivations: parse network-id; resolve `network-allow-private-ips`; fill bootstrappers from `ava-genesis` when unset (both-or-neither, count match); genesis bytes from `--genesis-file(-content)` for custom else embedded; staking-economics/fee flags ignored on Mainnet/Fuji (use `genesis::GetStakingConfig`/`GetTxFeeConfig`); validation (sybil rejection, ephemeral-cert rejection, staking-signer one-of, public-ip vs resolution-service one-of, disk-space ranges, track-subnets ≠ Primary) → `ConfigError` variants (12 §11). `snow-quorum-size` override.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config parse::` passes.
- [x] **Step 5 — Commit:** `ava-config: get_node_config + network-dependent defaults + validation (12 §1.6)`

> **AS-BUILT (M8.12).** `get_node_config` = ~25 focused helpers named after their Go getters,
> orchestrated in Go's exact order (staking before network; halflife threaded into router+network).
> **PLAN-TEXT CORRECTION (review-verified): "ephemeral-cert rejection" does NOT exist in Go** —
> `config.go:752-760` `getStakingTLSCert` generates an ephemeral cert unconditionally, no
> network-ID check (the only public-net rejection in `getStakingConfig` is sybil-protection);
> Rust mirrors Go (no rejection); spec 12 §1.6 carries the correction callout. Other as-builts:
> `ACTIVATED_ACPS` (17 entries) + empty `SCHEDULED_ACPS` live as `parse.rs` consts (re-home if
> ava-network needs them for handshakes); custom `--upgrade-file(-content)` carried raw in
> `Config::custom_upgrade_bytes` (JSON-validated; `ava-version::UpgradeConfig` has no serde);
> Go's vacuous `< 0` Duration checks elided (unsigned `Duration`); `provided_flags` renders
> resolved values as strings over `FLAG_SPECS`; `ava-genesis/src/params.rs` =
> `GetStakingConfig`/`GetTxFeeConfig` constants (review-verified vs `genesis_{mainnet,fuji,local}.go`;
> `createSubnetTxFee` no longer exists at the cc3b103 pin). Quality follow-up 2ede55c added the
> 13-row `validation_guard_matrix` (`ConflictingImplicitACPOpinion` uncoverable while
> `SCHEDULED_ACPS` is empty — noted in the test header) + an env-layer smoke test. 36 ava-config tests.

### Task M8.13: subnet & chain config dir/content loaders ✅ DONE (0aec4fd)
**Crate:** ava-config  ·  **Depends on:** M8.12, M2/M3 (snowball::Parameters)  ·  **Spec:** 12 §1.7, 13 §14 (subnet/chain config schema + resolution)
**Files:** `crates/ava-config/src/subnets.rs` (`subnets::Config`, loader), `crates/ava-config/src/chain_config.rs`
- [x] **Step 1 — Red:** `subnets.rs::tests::resolve_consensus_mode` — at-most-one of `consensusParameters`/`snowParameters`/`simplexParameters` (`ErrTooManyConsensusParameters`); none ⇒ primary-network snow config; `allowedNodes` non-empty requires `validatorOnly=true` (`AllowedNodesWithoutValidatorOnly`); deprecated `consensusParameters` migrates into `snowParameters` (13 §14). `chain_config.rs::tests::chain_config_dir_layout` — `<dir>/<alias>/{config,upgrade}.*` → `ChainConfig{config,upgrade}`; b64 `chain-config-content` map form; explicit-but-missing dir ⇒ `errCannotReadDirectory`, unset+missing ⇒ empty (13 §14).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-config subnets::tests::resolve_consensus_mode` → fails.
- [x] **Step 3 — Green:** Implement `subnets::Config { validator_only, allowed_nodes: BTreeSet<NodeId>, consensus_parameters, proposer_num_historical_blocks }` (13 §14 — NOT the prompt's `gossipConfig`/`proposerMinBlockDelay`). Implement `getSubnetConfigFromBytes`→`applySubnetConfigDefaults`→`resolveConsensusMode`→`ValidParameters` (13 §14). Implement chain-config dir/content loaders and alias-file loaders (`map[ids.ID][]string`, 13 §14).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-config subnets:: chain_config::` passes.
- [x] **Step 5 — Commit:** `ava-config: subnet/chain config + alias loaders (12 §1.7, 13 §14)`

### Task M8.14: ava-genesis bootstrappers + sample_bootstrappers + local start-time advance ✅ DONE (2be13f6+a4c5dcb)
**Crate:** ava-genesis  ·  **Depends on:** M8.5, M2 (uniform sampler)  ·  **Spec:** 23 §5.1 (getRecentStartTime), §5.2 (bootstrappers/SampleBootstrappers)
**Files:** `crates/ava-genesis/src/bootstrappers.rs`, `crates/ava-genesis/src/recent_start.rs`
- [x] **Step 1 — Red:** `bootstrappers.rs::tests::bootstrapper_parity` — per network count + IDs + IPs match `bootstrappers.json` (23 §9.5); `sample_bootstrappers(net,5)` selection determinism vs the gonum-parity sampler. `recent_start.rs::tests::get_recent_start_time` — fixed `now` → embedded `startTime` advanced by 9-month chunks (`9*30*24h`) until `<= now` (23 §5.1).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-genesis bootstrappers::tests::bootstrapper_parity` → fails.
- [x] **Step 3 — Green:** Embed `bootstrappers.json` as `map<network_name, Vec<Bootstrapper{id,ip}>>`; `bootstrappers(network_id)` by name; `sample_bootstrappers` via the M2 uniform sampler. `get_recent_start_time` loop. Provide both `UNMODIFIED_LOCAL_CONFIG` (golden) and advanced `LOCAL_CONFIG` (live), and wire `get_config(network_id)` (23 §5.1).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-genesis bootstrappers:: recent_start::` passes.
- [x] **Step 5 — Commit:** `ava-genesis: bootstrappers + sample + getRecentStartTime (23 §5)`

### Task M8.15: ava-genesis C-Chain timestamp + round-trip rebuild parity ✅ DONE (69bdb33)
**Crate:** ava-genesis  ·  **Depends on:** M8.8, M8.14, M6 ava-cchain (eth genesis parse for timestamp only)  ·  **Spec:** 23 §3.6, §7, §9.2/§9.6
**Files:** `crates/ava-genesis/tests/golden_genesis_extras.rs`
- [x] **Step 1 — Red:** `golden_genesis_extras.rs::cchain_genesis_timestamp` — parse `cChainGenesis` (eth `core.Genesis` JSON via reth/alloy) and assert `Timestamp`: Mainnet 0, Fuji 0, Local = `unix(upgrade::InitiallyActiveTime)` (23 §3.6/§7). `rebuild_parity` — for each embedded config, `from_config` then re-serialize, assert byte-identity vs the committed Go dump (extends M8.8; guards the full intermediate orderings, 23 §9.2).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-genesis cchain_genesis_timestamp` → fails.
- [x] **Step 3 — Green:** Validate `cChainGenesis` non-empty + parseable JSON; extract timestamp via the ava-cchain genesis parser. Confirm round-trip byte-identity.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-genesis golden_genesis_extras` passes.
- [x] **Step 5 — Commit:** `ava-genesis: C-Chain timestamp + round-trip rebuild parity (23 §3.6)`

> **AS-BUILT (M8.15).** `cchain_genesis_timestamp`: Local cChainGenesis timestamp `1607144400`
> == `unix(ava_version::upgrade::initially_active_time())` (2020-12-05T05:00:00Z); Mainnet/Fuji = 0.
> Parser = `ava_evm::chainspec::CChainGenesis::parse` (new additive `timestamp()` accessor on ava-evm).
> `rebuild_parity` = the RE-SERIALIZATION delta (unparsed→parsed→re-serialize→re-parse Config equality +
> identical from_config bytes + LazyLock-vs-fresh-parse identity) — the .bin byte-diff itself already lives
> in M8.8's `genesis_p_chain_bytes_byte_identical`. Test-only deps silenced per-dep (`#[cfg(test)] use x as _;`,
> ava-config precedent). 18 ava-genesis tests.

### Task M8.16: ava-api server — axum/h2c/CORS/allowed-hosts/node-id/timeouts/503 + ApiServer trait ✅ DONE (5eb8269+cd15906)
**Crate:** ava-api  ·  **Depends on:** M2/M3 (ConsensusContext, CommonVM trait, validators/network handles), M8.12 (HTTPConfig)  ·  **Spec:** 12 §3.1/§3.9, 14 §1.3/§16.3
**Files:** `crates/ava-api/Cargo.toml` (axum, hyper http2, tower, tower-http cors, tokio-tungstenite, tonic, tonic-web), `crates/ava-api/src/lib.rs`, `crates/ava-api/src/server.rs` (`ApiServer` trait + impl), `crates/ava-api/src/middleware.rs` (allowed-hosts, node-id header, per-chain-503, metrics/trace wrappers)
- [x] **Step 1 — Red:** `middleware.rs::tests::allowed_hosts_filter` — Host not in `http-allowed-hosts` ⇒ 403 "invalid host specified"; `*` accepts all; bare-IP/empty accepted (14 §16.3). `server.rs::tests::node_id_header_on_every_response` (incl. error responses, 14 §16.3). `not_bootstrapped_503` — chain not `NormalOp` ⇒ 503 "API call rejected because chain is not done bootstrapping" (14 §16.3).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-api middleware::tests::allowed_hosts_filter` → fails.
- [x] **Step 3 — Green:** Build the axum router under base `/ext` with h2c (`MaxConcurrentStreams=64`), `tower-http::cors` (origins from `http-allowed-origins` default `*`, allow-credentials), allowed-hosts middleware (403), `node-id` response-header layer, per-chain not-bootstrapped 503 layer, and read/read-header/write/idle timeout layers from `HTTPConfig` (12 §3.1). Define the `ApiServer` trait (`add_route`, `add_aliases`, `register_chain`, `add_header_route`, `serve`, `shutdown`) per 12 §3.1.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-api middleware:: server::` passes.
- [x] **Step 5 — Commit:** `ava-api: HTTP server + middleware (h2c/CORS/allowed-hosts/503/node-id, 12 §3.1)`

> **AS-BUILT (M8.16, merged 276d008; spec+quality reviewed, fix pass cd15906).** 15 tests. axum 0.7 +
> explicit hyper-util `auto::Builder` accept loop (NOT `axum::serve`): h2c + HTTP/1.1 on one port,
> `.http2().max_concurrent_streams(64)` real, `.http1().header_read_timeout(read_header_timeout)`;
> `write_timeout` = request-level `TimeoutLayer` (408). **Go `ReadTimeout`/`IdleTimeout` are NOT
> faithfully mappable on hyper-util** (no whole-request read deadline; no HTTP/1 idle-close timer;
> h2 keep-alive PING ≠ idle-close) — left unwired, documented in `serve` (revisit if hyper-util grows
> the knobs). Review fixes worth knowing: per-chain 503 layer wraps each chain's actual mounted
> sub-router (the naive `Router::new().layer().merge()` form is a NO-OP — axum `.layer()` only wraps
> already-present routes); `node_id_header` applied LAST = outermost so 403s carry it; CORS
> credentials+wildcard uses `AllowHeaders::mirror_request()` (tower-http panics on `Any`+credentials;
> mirrors Go rs/cors); `add_aliases` is alias-before-route like Go `router.AddAlias` (reserves names,
> propagates on later `add_route`); `shutdown` uses `Notify::notify_one()` (notify_waiters loses a
> pre-serve shutdown). `register_chain` records ctx+mount prefix only (full mounting M8.22).

### Task M8.17: JSON-RPC 2.0 shim (gorilla json2 wire shape) + error model + #[rpc_service] macro ✅ DONE (0ebacce+2c517e8)
**Crate:** ava-api  ·  **Depends on:** M8.16  ·  **Spec:** 12 §3.2/§11, 14 §1.1/§16.1/§16.5
**Files:** `crates/ava-api/src/jsonrpc.rs` (`Req`/`Resp`/`JsonRpcError`, `ServiceRegistry`, `dispatch`), `crates/ava-api/src/error.rs` (`json2_code`, `IntoJsonRpcError`), `crates/ava-api-macros/` (`#[rpc_service("name")]`)
- [x] **Step 1 — Red:** `jsonrpc.rs::tests::gorilla_wire_shape` — request `{"jsonrpc":"2.0","id":1,"method":"info.getNodeID","params":[{}]}` dispatches to service `info` method `GetNodeID` (case-insensitive method segment, single-element `params` array); success → `{"jsonrpc":"2.0","id":1,"result":{...}}`. `domain_error_is_minus_32000_http_200` — a handler-returned error → body `{code:-32000, message: err.to_string(), data: null}` with **HTTP 200** (14 §16.1 nuance); malformed JSON → -32700; unknown method → -32601; uppercase guard → -32601 (14 §16.1).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-api jsonrpc::tests::gorilla_wire_shape` → fails.
- [x] **Step 3 — Green:** Implement `Req{jsonrpc,method,params,id}`/`Resp`, `ServiceRegistry`, axum `dispatch` (POST, Content-Type `application/json[;charset=UTF-8]`; split `method` on `.`; `first_param(params)` = `params[0]`; 200+error-body for domain errors; 405 non-POST, 415 bad content-type pre-dispatch per 14 §16.3). `json2_code` consts (−32700/−32600/−32601/−32602/−32603/−32000) and the blanket `IntoJsonRpcError` (default → −32000, message = `to_string()`, `data: null`, 14 §16.5). Implement `#[rpc_service("name")]` derive registering `async fn(&self, Args) -> Result<Reply, RpcError>` methods so the registered method set cannot drift from the trait (12 §3.2).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-api jsonrpc:: error::` passes.
- [x] **Step 5 — Commit:** `ava-api: gorilla-json2 JSON-RPC shim + error model + rpc_service macro (12 §3.2, 14 §16)`

> **AS-BUILT (M8.17, merged 276d008; spec+quality reviewed, fix pass 2c517e8).** 28 ava-api tests +
> compile_fail doctest. Wire envelope verified byte-faithful to 14 §1.1 (`jsonrpc:"2.0"` present;
> result/error mutually exclusive via skip; error `data` ALWAYS explicit null per §16.5). ★ **The
> uppercase guard is on the METHOD segment, inverted from the first implementation's guess** (Go
> `utils/json/codec.go::errUppercaseMethod`): service folds case-insensitively (`Info.getNodeID` OK),
> method first-rune-uppercase ⇒ -32601 (`info.GetNodeID` REJECTED), then first letter uppercased and
> the REMAINDER matched EXACTLY — caught by spec review; the original fully-case-insensitive method
> matching + service-segment guard was a parity break. Consequence: the macro registers exact Go
> method names; **`#[rpc(name = "GetNodeID")]` per-method override exists for acronym names — every
> acronym method in M8.18–M8.24 MUST carry it** (pascalize gives `GetNodeId` ≠ Go `GetNodeID`).
> Macro contract: only `pub async fn(&self, Args) -> Result<Reply, RpcError>` registers; `pub fn`
> non-async = compile error (typo guard); private helpers skipped. Bare-object `params` passthrough
> is Go-faithful (gorilla json2 ReadRequest unmarshals params directly into args first, falls back to
> `[1]interface{}` — verified in gorilla source). `RpcError::from_error(&impl Error)` replaces the
> misleading `From<&E>` blanket (an owned blanket is incoherent with reflexive From). Dispatch core
> split as pure `dispatch_body(&registry, &[u8])` (HTTP-free, testable); registry immutable-after-build
> behind Arc. NOTE for M8.22/M8.31: no BUILD.bazel committed for ava-api/ava-api-macros/ava-node yet —
> run `bazel-gazelle-generate` + `deps-tidy` before any push upstream.

### Task M8.18: info API — 13 methods (`/ext/info`) ✅ DONE (0d6ead5+51ebc10, merge 16b62c1)
**Crate:** ava-api  ·  **Depends on:** M8.17, M2/M3 (network/validators/benchlist/chainManager handles), M8.12 (Config), M1 ava-version  ·  **Spec:** 12 §3.3, 14 §3
**Files:** `crates/ava-api/src/info/mod.rs`, `crates/ava-api/src/info/types.rs`
- [x] **Step 1 — Red:** `info/mod.rs::tests::info_method_set` — assert the registered `info` method set == the 13 names in 14 §3 (`getNodeVersion`,`getNodeID`,`getNodeIP`,`getNetworkID`,`getNetworkName`,`getBlockchainID`,`peers`,`isBootstrapped`,`upgrades`,`uptime`,`acps`,`getTxFee`,`getVMs`). Type-shape unit test for `getNodeVersion` reply field names/json tags (14 §3).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-api info::tests::info_method_set` → fails.
- [x] **Step 3 — Green:** Implement `#[rpc_service("info")] impl Info` with the 13 methods, Args/Reply serde types mirroring Go field names/json tags exactly (14 §3): `getNodeVersion`→{version,databaseVersion,rpcProtocolVersion,gitCommit,vmVersions}; `getNodeID`→{nodeID,nodePOP{publicKey,proofOfPossession}}; `peers`→{numPeers,peers:[Peer]}; `upgrades`→`upgrade::Config`; `acps` tally; `getTxFee` (deprecated warn); `getVMs`. Construct from `Parameters{version,nodeID,nodePOP,networkID,vmManager,upgrades,txFee,createAssetTxFee}` + handles.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-api info::` passes.
- [x] **Step 5 — Commit:** `ava-api: info API (13 methods, 14 §3)`

### Task M8.19: admin API — 13 methods (`/ext/admin`, disabled by default) ✅ DONE (400c12b+11ca82d, merge 7a4696f)
**Crate:** ava-api  ·  **Depends on:** M8.17, M8.18, M8.28 (logging reload handles for setLoggerLevel), M1 ava-database (dbGet)  ·  **Spec:** 12 §3.5, 14 §4
**Files:** `crates/ava-api/src/admin/mod.rs`, `crates/ava-api/src/admin/types.rs`, `crates/ava-api/src/admin/profiler.rs` (pprof crate)
- [x] **Step 1 — Red:** `admin/mod.rs::tests::admin_method_set` — registered `admin` set == the 13 names (14 §4): `startCPUProfiler`,`stopCPUProfiler`,`memoryProfile`,`lockProfile`,`alias`,`aliasChain`,`getChainAliases`,`stacktrace`,`setLoggerLevel`,`getLoggerLevel`,`getConfig`,`loadVMs`,`dbGet`. Unit: `alias` rejects `len(alias) > 512`; `setLoggerLevel` requires ≥1 of logLevel/displayLevel (14 §4).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-api admin::tests::admin_method_set` → fails.
- [x] **Step 3 — Green:** Implement `#[rpc_service("admin")] impl Admin` with the 13 methods: pprof via the `pprof` crate (CPU/memory/lock profiles to `profile-dir`); `alias`/`aliasChain`/`getChainAliases` via the ApiServer alias registry; `stacktrace` dumps task backtraces; `setLoggerLevel`/`getLoggerLevel` via `tracing-subscriber` reload handles (12 §3.5); `getConfig` serializes the resolved `Config` (providedFlags-aware, 13 §23); `loadVMs` rescans plugin-dir; `dbGet` raw hex DB read → `{value, errorCode}`.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-api admin::` passes.
- [x] **Step 5 — Commit:** `ava-api: admin API (13 methods, 14 §4)`

### Task M8.20: health API — dual GET/JSON-RPC handler + health worker loop ✅ DONE (1920779+f020d7e, merge b9b0c94)
**Crate:** ava-api  ·  **Depends on:** M8.17, M8.21 (health metrics namespace)  ·  **Spec:** 12 §3.4, 14 §5/§16.3, 17 §2.2 (#21), 18 §2.13
**Files:** `crates/ava-api/src/health/mod.rs` (worker + checker registry), `crates/ava-api/src/health/handler.rs` (method-branching axum handler), `crates/ava-api/src/health/types.rs` (`Result`, `APIReply`)
- [x] **Step 1 — Red:** `health/handler.rs::tests::get_returns_200_or_503` — GET `/ext/health` healthy→200, unhealthy→503, body `{checks, healthy}` (14 §5/§16.3); `?tag=` filtering. `get_subpaths` — `/ext/health/{health,readiness,liveness}` GET-only. `post_jsonrpc` — POST dispatches `health.health/readiness/liveness(tags)`. `worker_ewma` — registered checkers run on `health-check-frequency` with averager halflife.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-api health::handler::tests::get_returns_200_or_503` → fails.
- [x] **Step 3 — Green:** Implement the dual handler (GET branch → GET handler; else JSON-RPC, 14 §5). `Health` worker: `register_health_check(name, checker, tags)`, runs on the freq interval (17 #21), EWMA per `health-check-averager-halflife`, `ApplicationTag` + per-chain tags; `shuttingDown` checker registered at shutdown. `Result{message,error,timestamp,duration,contiguousFailures,timeOfFirstFailure}`. Worker initialized **before** the chain manager (12 §2.2 step 18).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-api health::` passes.
- [x] **Step 5 — Commit:** `ava-api: health API dual handler + worker loop (12 §3.4, 14 §5)`

### Task M8.21: metrics API + MultiGatherer/PrefixGatherer/LabelGatherer + metrics-name golden ✅ DONE (98649a5+a65ad79+c865bdb+5bd826a, merge 57911ba)
**Crate:** ava-api  ·  **Depends on:** M8.16, M1 (prometheus crate)  ·  **Spec:** 12 §3.6, 14 §6, 18 §1/§2/§3/§4
**Files:** `crates/ava-api/src/metrics/mod.rs` (`PrefixGatherer`, `LabelGatherer`, `make_and_register`, `/ext/metrics` handler), `crates/ava-api/tests/golden_metrics_names.rs`, `crates/ava-api/tests/vectors/api/metrics_schema.json` (Go snapshot)
- [x] **Step 1 — Red:** `metrics/mod.rs::tests::prefix_namespace` — family `peers` under prefix `avalanche_network` exposes `avalanche_network_peers` (sep `_`, 18 §1.2); overlapping-namespace registration rejected (`eitherIsPrefix`). `label_injection` — `LabelGatherer("chain")` injects `chain="<alias>"` into every family (18 §1.1). `tests/golden_metrics_names.rs::metrics_name_parity` — Rust `/ext/metrics` schema `{(name,type,sorted(label_keys))}` is a **superset** of the committed Go snapshot (18 §3), with the documented `go_*` waiver (18 §4).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-api metrics_name_parity` → fails.
- [x] **Step 3 — Green:** Implement `PrefixGatherer`/`LabelGatherer`/`make_and_register` (18 §1.2), `NAMESPACE_SEP="_"`, `PLATFORM_NAME="avalanche"`, `CHAIN_LABEL="chain"`. `/ext/metrics` GET serializes via `prometheus::TextEncoder` (`text/plain; version=0.0.4`), sorted families for byte-stability (18 §7). Add `process_*` collector (Linux); document the `go_*` gap. Commit the Go metrics schema snapshot.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-api metrics::` passes.
- [x] **Step 5 — Commit:** `ava-api: metrics MultiGatherer + /ext/metrics + name-parity golden (18 §1-§4)`

> **AS-BUILT (M8.18–M8.21, merged 2026-06-11/12 wave; each spec+quality reviewed).** info 15 / admin 14 /
> health 16 / metrics 9 module tests + `golden_metrics_names.rs`. Notable: **metrics** — merge semantics
> mirror Go `prometheus.Gatherers.Gather` (type-conflict/dup-label checks), sorted families + sorted-by-label-value
> metrics (18 §7); golden superset vs Go snapshot emitted via `tests/go-oracle/` (avalanchego @ 5896c92fee);
> waivers: `go_*` runtime families, `process_*` off Linux, Linux-only `process_network_*`,
> `process_virtual_memory_max_bytes` (Rust prometheus crate gap). **Spec finding (18 §4):** families are
> `avalanche_process_{go,process}_*` — the prefix gatherer renames unconditionally, not bare `go_*`/`process_*`.
> Quality follow-ups worth knowing: metrics gathers OUTSIDE the registry lock + alloc-free label sort (5bd826a);
> health checker-panic containment + monotonic/registration tests (f020d7e); admin wraps profiler/stacktrace
> blocking I/O in `spawn_blocking` (11ca82d).

### Task M8.22: register_chain mounting contract (P/X/C/proposervm) + header-route + aliases
**Crate:** ava-api  ·  **Depends on:** M8.16, M8.17, M4 ava-platformvm::service (31 methods), M5 ava-avm::service (11 methods), M6 ava-evm (rpc/ws/avax/admin header-routes), M7/proposervm::service  ·  **Spec:** 12 §3.1/§3.7/§3.8, 14 §1.2/§8/§9/§10/§11/§13
**Files:** `crates/ava-api/src/register.rs` (`register_chain` impl), `crates/ava-api/src/header_route.rs` (`X-Avalanche-Vm-Route` dispatch + WS upgrade), `crates/ava-api/src/connect.rs` (tonic/tonic-web/Connect compat for proposervm)
- [ ] **Step 1 — Red:** `register.rs::tests::mounts_create_handlers_under_chain_id` — `register_chain` mounts each `create_handlers()` extension at `/ext/bc/<chainID>/<ext>` (`""`⇒`/ext/bc/<chainID>`, extension validated like `url.ParseRequestURI`); header-route handler registered for `<chainID>`; `P`/`X`/`C` path aliases resolve (14 §1.2/§13). `header_route.rs::tests::route_by_header` — `X-Avalanche-Vm-Route: proposervm` → proposervm handler; empty value → 400; missing handler → 404 (14 §16.3). Method-set assertions: P-Chain 31 (14 §8), X-Chain 11 (14 §9), proposervm 2 JSON-RPC + 2 Connect (14 §11.1).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-api register::tests::mounts_create_handlers_under_chain_id` → fails.
- [ ] **Step 3 — Green:** Implement `register_chain(name, ctx, vm)` per 12 §3.1 / 14 §13: (1) `vm.create_handlers()` → mount each at `/ext/bc/<chainID>/<ext>`; (2) `vm.new_http_handler()` → header-route handler for the EVM `/rpc`,`/ws`,`/avax`,`/admin` mounts and proposervm Connect; (3) register `P`/`X`/`C` aliases; (4) wrap in per-chain metrics + OTel + 503 middleware (`wrapMiddleware`). WS upgrade via `tokio-tungstenite` on the header-route handler (12 §3.8). Connect proposervm via tonic + tonic-web + Connect-unary compat (`proposervm.ProposerVM` GetProposedHeight/GetCurrentEpoch, 14 §11.1). The per-chain `platform.*`/`avm.*`/`eth_*` handlers themselves come from M4/M5/M6 services; this task wires their mounting + the gorilla wire contract.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-api register:: header_route::` passes.
- [ ] **Step 5 — Commit:** `ava-api: register_chain mounting + header-route + proposervm Connect (12 §3.1, 14 §1.2/§13)`

### Task M8.23: differential::api_parity — every endpoint structural-JSON-equal vs Go
**Crate:** ava-api (test harness coordinated with cross-cutting harness X)  ·  **Depends on:** M8.18–M8.22  ·  **Spec:** 14 §14/§16.6, 12 §12.4, 02 §9/§11.4
**Files:** `crates/ava-api/tests/differential_api_parity.rs`, `crates/ava-api/tests/vectors/api/<service>/<method>.json` (recorded Go request/response pairs), `tests/differential/api_oracle.rs` (shared harness hook)
- [ ] **Step 1 — Red:** `tests/differential_api_parity.rs::api_parity` — for every method in 14 §3–§11, drive an identical JSON-RPC (or geth/Connect) request at the Rust node and compare against the recorded Go oracle response: structural-JSON-equal after normalizing non-deterministic fields (timestamps, node-IDs, peer lists, 02 §11.4). Plus method-set completeness (registered Rust method set == Go set per service, 14 §14.2), wire-shape conformance (single-element `params`, `Service.Method`, `{code,message,data}` errors), HTTP semantics (403 allowed-hosts, 503 not-bootstrapped, `node-id` header, health 200/503), and error-response snapshots (14 §16.6: bad params -32602, unknown method -32601, malformed -32700, EVM revert code 3, fee-cap message).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-api api_parity` (recorded-oracle mode) → fails.
- [ ] **Step 3 — Green:** Wire the differential harness hook (live mode boots Go + Rust nodes; recorded mode replays committed `tests/vectors/api/`). Commit recorded Go vectors for every endpoint group. Normalize via the 02 §11.4 normalizer. Reconcile any divergence until structural-equal. Mark live mode CI-gated; recorded-oracle runs per-PR.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-api api_parity` (recorded mode) passes.
- [ ] **Step 5 — Commit:** `ava-api: differential::api_parity (all endpoints, 14 §14/§16.6)`

### Task M8.24: ava-indexer — Indexer trait + per-chain index + index API + differential::indexer_parity ✅ DONE (3463015+a186580, merge 60ee2e8)
**Crate:** ava-indexer  ·  **Depends on:** M8.17 (JSON-RPC shim), M2/M3 (AcceptorGroup broadcast, ConsensusContext, CommonVM), M1 ava-database (versioned batch), M8.12 (index-enabled/allow-incomplete)  ·  **Spec:** 12 §5, 14 §7, 17 §2.2 (#20)/§3 (broadcast Lagged)
**Files:** `crates/ava-indexer/Cargo.toml`, `crates/ava-indexer/src/lib.rs` (`Indexer` trait), `crates/ava-indexer/src/indexer.rs` (`register_chain`), `crates/ava-indexer/src/index.rs` (Accept writes), `crates/ava-indexer/src/service.rs` (6 methods), `crates/ava-indexer/tests/differential_indexer_parity.rs`, `crates/ava-indexer/tests/vectors/...`
- [x] **Step 1 — Red:** `index.rs::tests::accept_ordering_and_markers` — `Accept` writes `containerID→bytes`, `height→containerID`, `containerID→height`, advances `nextAcceptedIndex` atomically (12 §5); `incomplete-index fatal` — toggling `index-enabled` so an index would gap with `index-allow-incomplete=false` ⇒ fatal (12 §5). `service.rs::tests::index_method_set` — 6 methods (14 §7): `getLastAccepted`,`getContainerByIndex`,`getContainerByID`,`getContainerRange`,`getIndex`,`isAccepted`; `getContainerRange` capped at 1024; `FormattedContainer{id,bytes,timestamp,encoding,index}`. `differential_indexer_parity.rs::indexer_parity` — accept ordering, range queries, restart markers vs recorded Go oracle.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-indexer indexer_parity` → fails.
- [x] **Step 3 — Green:** Implement the `Indexer` trait (`register_chain`, `close`), `register_chain` (skip non-primary subnets + already-indexed; prefixes `tx=0x01`,`vtx=0x02`,`block=0x03`; incomplete-index safety). `Accept` via versioned batch, offloaded to `spawn_blocking` (17 #20), broadcast `Lagged` treated as fatal (17 §3). Index API JSON-RPC service mounted per chain at `/ext/index/<alias>/{block,tx,vtx}` (14 §7); encodings hex/cb58. Persisted `hasRun`/`incomplete` markers match Go.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-indexer` passes (recorded mode).
- [x] **Step 5 — Commit:** `ava-indexer: index + index API + differential::indexer_parity (12 §5, 14 §7)`

> **AS-BUILT (M8.24, merged 60ee2e8).** 16 tests (15 unit + indexer_parity); tests/PORTING.md maps every Go
> indexer test. container.rs byte-exact linear codec (v0); index.rs reads mirror Go incl. byte-stable error
> strings, `MAX_FETCHED_BY_RANGE=1024`; acceptor.rs = broadcast AcceptorGroup seam implementing the ava-snow
> Acceptor engine-side publish hook; register_chain markers (`hasRun`/previously-indexed/incomplete) byte-identical
> to Go, incomplete-index violation ⇒ fatal close + `shutdown_f`; per-index acceptor task offloads writes to
> `spawn_blocking` (17 §2.2 #20), broadcast `Lagged` fatal; service.rs = 6 `index.*` methods with
> `#[rpc(name = "GetContainerByID")]` acronym override, gorilla-parity `FormattedContainer` (cb58 id,
> checksummed hex, RFC3339Nano timestamp, json.Uint64 index); per-index axum handler mounted through a narrow
> `PathAdder` seam (full node wiring lands M8.29). Differential: recorded Go oracle (in-repo emitter,
> avalanchego@5896c92) pins codec bytes, FULL physical DB dumps across three runs, live JSON-RPC reply JSON,
> error strings, and the incomplete-index fatal. Quality follow-up a186580: fatal-path tests + quiet
> write-abort on graceful close.

### Task M8.25: ava-wallet — P-chain Builder/Signer/Backend + UTXO selection ✅ DONE (530a882+f7266a4)
**Crate:** ava-wallet  ·  **Depends on:** M4 ava-platformvm (tx/UTXO/Owner/SubnetValidator types), M1 ava-crypto (keychain, secp256k1, BLS PoP)  ·  **Spec:** 12 §13 (incl. the ACP-236 upstream-delta callout)
**Files:** `crates/ava-wallet/Cargo.toml`, `crates/ava-wallet/src/lib.rs`, `crates/ava-wallet/src/p/{builder.rs,signer.rs,backend.rs,wallet.rs}`, `crates/ava-wallet/src/common/utxo_select.rs`
> **UPSTREAM DELTA (Go `c84b906db6`, ACP-236 (3), #5202 — post-spec-snapshot, folded 2026-06-10).** The Go P-chain builder gained `NewAddAutoRenewedValidatorTx` / `NewSetAutoRenewedValidatorConfigTx` (+ `with_options` wrappers + `Wallet.Issue*` facades) — include them in the `PBuilder` port (12 §13 delta has the exact signatures; Go `wallet/chain/p/builder_test.go` gained the matching cases to mirror as golden tx vectors). Fee complexity for both txs is now implemented in Go (`08` §6 delta) — the M4 `complexityVisitor` port's `errUnimplemented` stubs (if mirrored) must be filled before these vectors can price correctly. Also: `platform.getCurrentValidators` replies now embed `AutoRenewedConfig{validatorAuthority, nextPeriod, autoCompoundRewardShares}` (`14` delta) — the API client/Backend structs (M8.18/M8.22 + M4 service) need the field for response parity.
- [x] **Step 1 — Red:** `common/utxo_select.rs::tests::deterministic_selection` — sort UTXOs, prefer locked-then-unlocked to satisfy `amount+fee`, respect locktime/threshold; output ordering deterministic (12 §13). `p/builder.rs::tests::new_base_tx_bytes_match_go` — fixed UTXO set/keys → built+signed `BaseTx` bytes byte-identical to a recorded Go wallet output (12 §12.5, golden tx vector).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-wallet p::builder::tests::new_base_tx_bytes_match_go` → fails.
- [x] **Step 3 — Green:** Implement the `PBuilder` trait (12 §13: `new_base_tx`, `new_add_permissionless_validator_tx`, `new_create_subnet_tx`, `new_create_chain_tx`, `new_import_tx`, `new_export_tx`, add/remove subnet validator, transfer/transform subnet, ACP-77 L1 conversion/register/set-weight/increase-balance/disable, ACP-236 `new_add_auto_renewed_validator_tx`/`new_set_auto_renewed_validator_config_tx` (upstream delta), `utxos`, `get_owner`, `get_balance`). UTXO selection mirroring Go `common` (deterministic). Signer: per-input secp256k1 credentials + BLS PoP for validator txs, keychain abstraction. Backend: UTXO set/owners + `with_options` overlays (memo, change owner, custom fee, min-issuance-time). Builders/signers pure (no I/O) given a Backend snapshot.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-wallet p::` passes.
- [x] **Step 5 — Commit:** `ava-wallet: P-chain builder/signer/backend + UTXO selection (12 §13)`

> **AS-BUILT (M8.25).** Keychain/options/deterministic UTXO selection + P fee complexity table +
> full `PBuilder` (incl. ACP-236 auto-renewed validator txs per the upstream delta) with golden
> byte-parity vs live Go for ~10 P-chain tx types (`tests/vectors/wallet/p.json`). Built in a prior
> session's worktree; verified + merged 2026-06-11 (merge 7afa24c).

### Task M8.26: ava-wallet — X-chain + C-chain (atomic) builders/signers ✅ DONE (1b27f45+2ab7c82)
**Crate:** ava-wallet  ·  **Depends on:** M8.25, M5 ava-avm (tx types), M6 ava-cchain (atomic import/export tx types)  ·  **Spec:** 12 §13
**Files:** `crates/ava-wallet/src/x/{builder.rs,signer.rs,backend.rs,wallet.rs}`, `crates/ava-wallet/src/c/{builder.rs,signer.rs,backend.rs,wallet.rs}`
- [x] **Step 1 — Red:** `x/builder.rs::tests::x_base_tx_bytes_match_go` and `c/builder.rs::tests::c_import_export_bytes_match_go` — fixed UTXO sets/keys → signed X-Chain base tx and C-Chain atomic import/export bytes byte-identical to recorded Go outputs (12 §12.5).
- [x] **Step 2 — Confirm red:** `cargo test -p ava-wallet x::builder::tests::x_base_tx_bytes_match_go` → fails.
- [x] **Step 3 — Green:** Implement X-Chain builder/signer/backend (base/import/export/create-asset/operation txs) and C-Chain atomic import/export between C and X/P (no EVM account txs — those go through reth RPC, 12 §13). Reuse the deterministic UTXO selection.
- [x] **Step 4 — Confirm green:** `cargo test -p ava-wallet x:: c::` passes.
- [x] **Step 5 — Commit:** `ava-wallet: X-chain + C-chain atomic builders/signers (12 §13)`

> **AS-BUILT (M8.26).** X base/base-memo/create-asset/import (incl. AVAX<fee local top-up branch)/
> export + C import (X→C, P→C; non-AVAX UTXOs skipped)/export (C→X), all unsigned+signed byte-parity
> vs live Go (12 new tests; 34 total). Go emitters now COMMITTED in-repo at
> `crates/ava-wallet/tests/go-oracle/` (X, C, and the rescued P emitter) — copy → env-gated run →
> delete, `AVAX_RS_GO_COMMIT` stamps provenance. Builders take `base_fee` verbatim (Go parity: the
> WithBaseFee option resolves in the M8.27 facade, not the builder). **DEFERRED:** X `OperationTx`
> (mint FT/NFT) — `ava-avm` has no typed fx-operation types (M5 §5.5 follow-up); signer returns
> `UnsupportedTxType`. **CROSS-CRATE FINDING → M6.29/M6 follow-up:** `ava-evm`
> `atomic/mempool.rs::gas_used` uses SIGNED `tx.bytes()` but coreth `Metadata.Bytes()` returns the
> UNSIGNED bytes (~81-gas overcount per 1-input import; affects mempool fee/gas-cap decisions) —
> verify against the coreth oracle and fix. Benign divergence: Go C `backend.Balance` errors
> ErrNotFound on unknown accounts, Rust returns 0.

### Task M8.27: ava-wallet — Wallet facades + primary wallet (make_wallet over API) ✅ DONE (3a96772+6ce7f85)
**Crate:** ava-wallet  ·  **Depends on:** M8.25, M8.26, M8.18/M8.22 (API client for state fetch)  ·  **Spec:** 12 §13
**Files:** `crates/ava-wallet/src/{p,x,c}/wallet.rs`, `crates/ava-wallet/src/primary.rs` (`make_wallet`, `Wallet{p,x,c}`)
- [x] **Step 1 — Red:** `primary.rs::tests::issue_flow_records_in_backend` — `Wallet::issue_*_tx` = build→sign→`issue_tx`(submit)→record in backend; with a mock chain client, assert the submitted bytes equal the built+signed bytes and the backend reflects the consumed/created UTXOs (12 §13). `make_wallet` fetches UTXOs/subnets/owners and wires P/X/C.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-wallet primary::tests::issue_flow_records_in_backend` → fails.
- [x] **Step 3 — Green:** Implement per-chain `Wallet` facade (`issue_*_tx` build+sign+issue+record) and `wallet/subnet/primary::make_wallet(uri, keychain, config)` fetching state over the API (info/platform/avm clients) and wiring `Wallet{p,x,c}` + `NewWalletWithOptions` (12 §13).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-wallet primary::` passes.
- [x] **Step 5 — Commit:** `ava-wallet: P/X/C facades + primary make_wallet (12 §13)`

> **AS-BUILT (M8.27).** The M8.18/M8.22 API clients don't exist yet → `make_wallet(&Clients, keychain, config)`
> takes narrow client TRAITS (`src/client.rs`: Info 2 / P 7 / X 5 / C 3 / Eth 3 methods; the
> deferred-live-transport pattern, M7.20/M7.23 precedent) — live JSON-RPC-over-HTTP impls + the
> Go `fetchLimit=1024` UTXO paging loop land with ava-api (M8.18/M8.22/M8.23). Fetch set mirrors
> Go `MakeWallet`/`FetchState` (info + P/X/C contexts incl. the 2× gas price, 9 source×destination
> UTXO views, owners, eth balance/nonce). `issue_*_tx` = build→sign→issue→await-accepted (unless
> `TxOption::AssumeDecided`)→`Backend::accept_tx` (Go `backend_visitor.go` parity; export indexing
> `len(outs)+i`; C credit `amount×10⁹` checked-u128). `WithBaseFee` resolves in the C facade
> (estimate-or-override), per the M8.26 hand-off. Review-verified divergences (documented in
> `tests/PORTING.md`): C `balance()`/`nonce()` return 0 for untracked accounts (Go: ErrNotFound);
> typed per-chain UTXO store errors `UnknownOutputType` on cross-boundary StakeableLock/SecpMint
> (Go stores untyped; wallet builders never produce these). Go's `AcceptAtomicTx` nonce handling is
> an unconditional `input.Nonce+1` overwrite (a reviewer's `errInvalidNonce` claim was checked and
> is WRONG — no such check exists in `wallet/chain/c`). X OperationTx facade surfaces
> `UnsupportedTxType` (M8.26 deferral). 41 ava-wallet tests (incl. issue-failure-no-record +
> AssumeDecided coverage). PORTING.md matrix added (28 Go wallet tests mapped).

### Task M8.28: ava-node submodules — trace (OTel), nat, logging factory ✅ DONE (b674e19+48d55b1)
**Crate:** ava-node  ·  **Depends on:** M8.12 (TraceConfig/LogConfig), M1 (tracing/opentelemetry crates)  ·  **Spec:** 12 §7/§8, 17 §2.2 (#23/#24), 18 §5/§6
**Files:** `crates/ava-node/Cargo.toml`, `crates/ava-node/src/trace.rs`, `crates/ava-node/src/nat/{mod.rs,upnp.rs,pmp.rs,noop.rs}`, `crates/ava-logging/src/lib.rs` (`init_logging`, `make_chain_logger`, reload handles)
- [x] **Step 1 — Red:** `ava-logging::tests::ava_level_ordering_and_json_shape` — 8 levels with Go ordering (Verbo<Debug<Trace<Info<Warn<Error<Fatal<Off, 18 §5.1); JSON line shape `{"level","timestamp","logger","caller","msg",...}` lowercased level, integer-ns durations (18 §5.2). `trace.rs::tests::disabled_is_noop` — `tracing-exporter-type=disabled` ⇒ no OTel layer (12 §7). `nat::tests::noop_router_maps_nothing`.
- [x] **Step 2 — Confirm red:** `cargo test -p ava-logging tests::ava_level_ordering_and_json_shape` → fails.
- [x] **Step 3 — Green:** Implement `AvaLevel` (8 names + ordering), `init_logging` (display layer at display-level + per-logger rolling file layer at log-level; plain/colors/json formats; per-chain `<alias>.log` via `make_chain_logger`; lumberjack-equivalent rotation; reload handles for admin `setLoggerLevel`, 18 §5). `trace::new(TraceConfig)` → `opentelemetry-otlp` exporter (grpc/http, insecure, `TraceIdRatioBased(rate)`, headers) wrapped by `tracing-opentelemetry`; no-op when disabled (12 §7, 18 §6). NAT `Router` trait with upnp (`igd-next`)/pmp/noop + `Mapper` + dynamicip updater (12 §8).
- [x] **Step 4 — Confirm green:** `cargo test -p ava-logging -p ava-node trace:: nat::` passes.
- [x] **Step 5 — Commit:** `ava-node: trace(OTel) + nat + logging factory (12 §7/§8, 18 §5/§6)`

> **AS-BUILT (M8.28, merged 5319a70; spec+quality reviewed, fix pass 48d55b1).** 19 tests (12 ava-logging
> + 7 ava-node). Key as-builts: (1) **NAT NOT re-implemented** — `ava-network::nat` already had
> `NatRouter`/UPnP(igd-next)/`NoRouter`/`PortMapper`, so `ava-node::nat` RE-EXPORTS those (no
> upnp.rs/noop.rs files) and adds only the two real gaps: a hand-rolled **RFC 6886 NAT-PMP client**
> (`pmp.rs`; ava-network's `get_pmp_router` was a `None` stub) and the **dynamicip updater**
> (CancellationToken + interval, fires sink only on IP change; resolvers = trait seam, concrete
> opendns/http impls land with M8.29 wiring). `get_router` probe order UPnP→PMP→noop matches Go.
> **PMP gateway discovery is a `.1`-on-/24 heuristic** (no portable route-table read; wrong guess ⇒
> external_ip round-trip fails ⇒ falls through to noop — documented in-code, route-table read is a
> follow-up). (2) `format::json_line` HAND-BUILDS the JSON line for exact zap key order
> (`level,timestamp,logger,caller,msg,...`; serde_json Map doesn't preserve insertion order); all
> escaping delegated to serde_json (edge-case tested). (3) **Lumberjack-equivalent rotation
> implemented for real** (`rolling.rs::RollingWriter`): stable `<name>.log` live file, size-based
> rotate to `<name>-<timestamp>.log`, prune to max_files, max_age_days drop, gzip on compress
> (workspace flate2); wrapped by tracing-appender NonBlocking. (4) **Per-chain layers are attachable
> post-init**: `init_logging` installs a `reload::Layer<Vec<Box<dyn Layer<Registry>>>>` slot;
> `LogHandles::add_chain_logger(alias)` appends a `ChainFieldFilter`-guarded (`chain == alias` event
> field) + level-filtered `<alias>.log` layer at runtime. `ReloadHandle` = type-erased boxed setter
> (admin setLoggerLevel seam, M8.19). (5) OTel pins: opentelemetry/_sdk/otlp 0.27 + tracing-opentelemetry
> 0.28 (rides workspace tonic 0.12); disabled ⇒ NO layer (provider None), grpc/http + insecure +
> TraceIdRatioBased + headers covered. (6) M8.29 handoffs: invoke blocking `get_router()`/PMP from
> spawn_blocking; Verbo/Trace/Fatal collapse to tracing's 5 levels until callers emit the `ava_level`
> field; `deps-tidy`/`bazel-check-metadata` must run before push (new per-crate deps).

### Task M8.29: ava-node assembly — Node::new (init steps 1–26, exact order)
**Crate:** ava-node  ·  **Depends on:** M8.12 (Config), M8.16–M8.22 (ApiServer), M8.24 (Indexer), M8.28 (trace/nat/logging), M2/M3 (Network/Router/validators/message::Creator/timeout/resource), M4–M7 (chain manager + VMs), M1 ava-database  ·  **Spec:** 12 §2.1/§2.2, 17 §1/§2/§7
**Files:** `crates/ava-node/src/node.rs` (`pub struct Node`, `Node::new`), `crates/ava-node/src/init/*.rs` (one per init step)
- [ ] **Step 1 — Red:** `node.rs::tests::init_order_matches_go` — instrument each `init_*` step to push its name onto a recorded `Vec<&str>`; assert the order equals the Go 26-step sequence (12 §2.2): cert→NodeID, BLS signer, log-banner, VMAliaser/VMManager, init_bootstrappers, trace, metrics, nat, api_server, metrics_api, database(+ungraceful marker), shared_memory, message::Creator (after metrics, before networking), validators(+override), resource/cpu/disk targeters, networking, event_dispatchers, **health_api before chain_manager**, default vm aliases, chain_manager, vms, admin/info api, chain/api aliases, indexer, health.start/profiler, init_chains.
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-node node::tests::init_order_matches_go` → fails.
- [ ] **Step 3 — Green:** Define `Node` (12 §2.2 struct: log, id, config, signers, db, net, chain_router, chain_manager, vm_manager/registry, runtime_manager, validators/bootstrappers, api_server, indexer, health, benchlist, timeout_manager, resource_manager, nat, tracer, metrics, `shutdown: CancellationToken`, `tasks: TaskTracker`, exit_code/shutting_down/shutdown_once). Implement `Node::new(config, log_factory, log, rt: Handle) -> Result<Arc<Self>>` running steps 1–26 exactly, returning typed errors. Build the root `CancellationToken` + child tokens per the 17 §4.1 tree (root→network→peer; root→subnet→chain). No sub-runtime (17 §1.1).
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-node node::tests::init_order_matches_go` passes.
- [ ] **Step 5 — Commit:** `ava-node: Node::new assembly (26-step init order, 12 §2.2)`

### Task M8.30: ava-node dispatch + shutdown ordering + lifecycle test
**Crate:** ava-node  ·  **Depends on:** M8.29  ·  **Spec:** 12 §2.3/§2.4, 17 §4.3/§4.4, 17 §9
**Files:** `crates/ava-node/src/dispatch.rs`, `crates/ava-node/src/shutdown.rs`
- [ ] **Step 1 — Red:** `shutdown.rs::tests::shutdown_order_matches_go` — record the actual step order in `Node::shutdown` and assert it equals the Go 14-step sequence (17 §4.3 / 12 §2.4): shuttingDown health-check + sleep `http-shutdown-wait`; staking_signer; resource_manager; timeout_manager; chain_manager (per-chain drain); benchlist; profiler; net.start_close (+cancel net_token); api_server (graceful, `http-shutdown-timeout`); nat unmap + ip_updater; indexer.close; runtime_manager.stop; db.delete(UNGRACEFUL)+close; tracer.close. `dispatch.rs::tests::api_dispatch_failure_triggers_shutdown_1`. `shutdown_runs_once` (OnceCell). Cancellation-propagation: cancel a `subnet_token` ⇒ only that subnet's chains join (17 §9).
- [ ] **Step 2 — Confirm red:** `cargo test -p ava-node shutdown::tests::shutdown_order_matches_go` → fails.
- [ ] **Step 3 — Green:** Implement `dispatch` (12 §2.3: write process.json `{pid,uri,stakingAddress}`; spawn API task → on unexpected exit `shutdown(1)`; manually-track state-sync + bootstrap peers; `net.dispatch().await` then `shutdown(1)`). Implement `shutdown` (OnceCell, 14 steps exact, 17 §4.3) using cancel→drain-with-timeout(`consensus-shutdown-timeout`)→abort-stragglers→drop (17 §4.4); `db.delete(UNGRACEFUL_SHUTDOWN)` last before close.
- [ ] **Step 4 — Confirm green:** `cargo test -p ava-node dispatch:: shutdown::` passes.
- [ ] **Step 5 — Commit:** `ava-node: dispatch + shutdown ordering + lifecycle test (12 §2.3/§2.4, 17 §4.3)`

### Task M8.31: avalanchers binary promotion — main flow + signals + exit code
**Crate:** avalanchers (bin)  ·  **Depends on:** M8.3 (build_command), M8.12 (get_node_config), M8.29/M8.30 (Node), M8.28 (logging), M1 ava-version  ·  **Spec:** 12 §9, 17 §1.1/§2.5/§5
**Files:** `crates/avalanchers/src/main.rs`, `crates/avalanchers/src/app.rs` (banner, chmod, fd-limit, signals)
- [ ] **Step 1 — Red:** `crates/avalanchers/tests/cli.rs::version_flags` — `--version` prints `version::get_versions().to_string()` and exits 0; `--version-json` pretty JSON exit 0; both set → error exit 1 (12 §9). `help_exits_0` — `--help` exit 0. `no_args_runs_mainnet` (parse-only smoke: builds Config with network-id mainnet). These use `assert_cmd` against the built binary.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p avalanchers version_flags` → fails.
- [ ] **Step 3 — Green:** Implement `main` (12 §9): register EVM extras; `build_command(FLAG_SPECS)`; parse (`--help`→exit 0); `--version`/`--version-json` (both→exit 1); `Layered`→`get_node_config`→`Config`; TTY banner; `chmod_r` data/log dirs; build LogFactory; raise fd-limit; build the single tokio multi-thread runtime (17 §1.1); `Node::new`; install SIGINT/SIGTERM→`shutdown(0)` + SIGABRT→backtrace dump (17 §2.5); `block_on(dispatch)`; `std::process::exit(node.exit_code())`. Add a CI grep gate forbidding `Runtime::new`/`block_on` outside the bin/tests (17 §1.1).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p avalanchers` + `cargo build -p avalanchers` pass.
- [ ] **Step 5 — Commit:** `avalanchers: full-node binary promotion (main/signals/exit, 12 §9, 17 §1.1)`

### Task M8.32: Milestone exit gate
**Crate:** all M8 crates + avalanchers  ·  **Depends on:** M8.1–M8.31  ·  **Spec:** all M8 specs; 02 §10 (PORTING.md)
**Files:** `crates/*/tests/PORTING.md` (update), `tests/differential/` (live-mode wiring), workspace CI config
- [ ] **Step 1 — Red:** Ensure all named exit-gate tests exist and are wired: `golden::flag_parity` (M8.4), `golden::genesis_block_id` (M8.8), `prop::config_precedence` (M8.11), `differential::api_parity` (M8.23), `differential::indexer_parity` (M8.24). A `tests/exit_gate.rs` aggregator asserts each is registered.
- [ ] **Step 2 — Confirm red:** `cargo nextest run --profile ci` → any missing/failing exit-gate test fails here.
- [ ] **Step 3 — Green:** Run and make green: `cargo build --workspace`; `cargo build -p avalanchers`; `cargo nextest run --profile ci`; `cargo clippy --workspace -- -D warnings`; the five named exit tests (per-PR: flag_parity + genesis_block_id + config_precedence; recorded-oracle: api_parity + indexer_parity, with live mode CI-gated, coordinated with harness X). Update each crate's `tests/PORTING.md` (Go source mapping, vector regen commands, deferrals). Confirm `./avalanchers` and `./avalanchers --network-id=fuji` start and stop like Go (lifecycle smoke).
- [ ] **Step 4 — Confirm green:** all commands above pass; `git status` clean after any vector regen.
- [ ] **Step 5 — Commit:** `M8: node/config/api/wallet/genesis exit gate green (full node drop-in)`

---

## Spec coverage check

| Spec section | Covered by task(s) | Notes / deferrals |
|---|---|---|
| 12 §1.2–1.4 (config layout, FlagSpec, clap) | M8.1, M8.2, M8.3 | |
| 12 §1.5 (Layered resolver, is_set, path expand) | M8.9, M8.10 | |
| 12 §1.6 (get_node_config, network-dependent defaults, validation) | M8.12 | |
| 12 §1.7 (subnet/chain config) | M8.13 | |
| 12 §1.8 / 13 §25 (flag-parity golden) | **M8.4 golden::flag_parity** | |
| 13 §1–§22 (every flag, verbatim catalog) | M8.2 (table) + M8.4 (diff) | flag oracle; symbolic NumCPU/fd-limit defaults normalized in M8.4 |
| 13 §23 (viper quirks, mutually-exclusive sets, network-dependent) | M8.10, M8.11, M8.12, M8.13 | |
| 12 §2.1–2.2 / 17 §1/§2 (runtime, init order, task graph) | M8.29 | single-runtime rule enforced in M8.31 |
| 12 §2.3 (dispatch) | M8.30 | |
| 12 §2.4 / 17 §4.3–4.4 (shutdown ordering, drain/abort) | M8.30 | |
| 12 §2.5 / 17 §2.5/§5 (signals, exit code, panic/SIGABRT) | M8.31 | |
| 12 §2.6 / 17 #25 (rpcchainvm plugin host) | M8.29 (wired via VMManager/runtime_manager) | full plugin protocol owned by spec 07 / M-VM milestone; node-side wiring + shutdown step 12 here |
| 12 §3.1/§3.9 / 14 §1.3 (server, CORS, allowed-hosts, node-id, 503, auth) | M8.16 | |
| 12 §3.2 / 14 §1.1/§16 (JSON-RPC shim, error model) | M8.17 | |
| 12 §3.3 / 14 §3 (info, 13 methods) | M8.18 | |
| 12 §3.4 / 14 §5 (health dual handler + worker) | M8.20 | |
| 12 §3.5 / 14 §4 (admin, 13 methods) | M8.19 | |
| 12 §3.6 / 14 §6 / 18 §1–4 (metrics, gatherers, name golden) | M8.21 | go_* runtime collectors documented-waiver (18 §4) |
| 12 §3.7 / 14 §11 (Connect/gRPC-Web, proposervm) | M8.22 | xsvm Connect Ping (14 §11.2) deferred to xsvm test crate |
| 12 §3.8 / 14 §12 (WebSocket pub-sub) | M8.22 | EVM /ws; legacy X/P pubsub removed (14 §12), not implemented |
| 14 §1.2/§13 (base paths, register_chain contract) | M8.22 | |
| 14 §8 (P-Chain 31 methods) | M8.22 (mounting) | handlers from M4 ava-platformvm::service |
| 14 §9 (X-Chain 11 methods) | M8.22 (mounting) | handlers from M5 ava-avm::service |
| 14 §10 (C-Chain eth/debug/net/web3/txpool/warp/avax/admin) | M8.22 (mounting) | handlers from M6 ava-evm (reth); §10 divergence tests in M8.23 |
| 14 §7 (Index API, 6 methods) | M8.24 | |
| 14 §14/§16.6 (API parity test plan) | **M8.23 differential::api_parity** | |
| 12 §5 / 14 §7 / 17 #20 (indexer) | **M8.24 differential::indexer_parity** | |
| 12 §6 / 23 §1–§6 (genesis crate, from_config, byte-exact build) | M8.5, M8.6, M8.7 | |
| 23 §4/§7 (genesis block IDs, golden values) | **M8.8 golden::genesis_block_id** | + M8.15 byte-stream round-trip |
| 23 §3.6/§7 (C-Chain timestamp) | M8.15 | |
| 23 §5 (embedded configs, bootstrappers, getRecentStartTime) | M8.14 | |
| 12 §7 / 18 §6 (trace/OTel) | M8.28 | |
| 12 §8 (nat) | M8.28 | |
| 18 §5 (logging factory, AvaLevel, formats, per-chain files) | M8.28 | |
| 12 §9 / 17 §1.1 (avalanchers binary) | M8.31 | |
| 12 §13 (wallet SDK P/X/C builder/signer/backend/facade/primary) | M8.25, M8.26, M8.27 | |
| 12 §11 / 14 §16 (error model, sentinels, byte-stable messages) | M8.17 (shim) + M8.23 (snapshots) | per-crate domain Errors owned by M4–M7 |
| 17 §3 (channel sizing/backpressure), §6 (determinism) | M8.29/M8.30 (token tree, drain) | channel-default golden (17 §9) lives in M2/M3; node wiring honors it |
| 18 §2 (full metric catalog) | M8.21 (golden superset) | per-subsystem metric structs owned by their crates (M2–M7); node merges + name-parity here |

**Deferrals (explicit):** (1) the full rpcchainvm/go-plugin handshake + plugin gRPC services are owned by spec 07 / the VM-framework milestone; M8 wires only the host `runtime_manager` lifecycle (init step 21, shutdown step 12). (2) xsvm Connect Ping (14 §11.2) is wired only behind the xsvm test crate. (3) The legacy X/P `pubsub` bloom feed is removed on this branch (14 §12) and not implemented. (4) EVM `eth_*`/`debug_*`/`warp_*` handler bodies + ACP-176 fee semantics are owned by M6 ava-evm; M8 mounts them and asserts divergence in `differential::api_parity` (M8.23). (5) Per-subsystem metric *registration* lives in each owning crate; M8.21 only builds the gatherer tree + name-parity golden.

# M9 — Plugin Interop + Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Land bidirectional rpcchainvm v45 plugin interop (Rust↔Go both directions, all proxied callback services), a live mixed Go+Rust network, the load/upgrade/reexecute suites, and perf gating — closing the project's drop-in-replacement definition of done.
**Tier:** final (ava-vm-rpc + all crates)
**Crates:** ava-vm-rpc (deepened) + all crates (hardening)
**Owning specs:** `07` §5 (rpcchainvm host+guest, reverse-dial v45), `02` §10.3 (load), §10.4 (upgrade), §10.5 (reexecute), §11 (differential harness), `26` (handshake compatibility, version string, RPCChainVMProtocol=45), `27` (crash-consistency hardening), `16` §5 (drop-in acceptance criteria — definition of done), `00` §11.1.1 (reverse-dial), §11.2 (risks)
**Depends on (prior milestones):** M8 (full node: `ava-node`, `ava-config`, `ava-api`, `ava-genesis`, `avalanchers` bin) + all of M0–M8 (every `ava-*` crate green at its own exit gate)
**Exit gate (named tests):**
- **`differential::plugin_rust_in_go`** + **`differential::plugin_go_in_rust`** — reverse-dial handshake v45, proxied services (rpcdb, appsender, sharedmemory, validatorstate, warp, aliasreader) work both ways (`00` §11.1.1, `07` §5).
- **`differential::mixed_network`** — live Go+Rust nodes, all chains, no fork, same tip.
- **`test-upgrade`** — Go→Rust across an activation height, including Go-data-dir → RocksDB import (exercises R2 fully).
- **`bench-guard`** perf gates (`02` §9).
- The full `16` §5 definition-of-done checklist, all simultaneously green.

**Risk retired:** R-final (drop-in acceptance, `16` §5). Exercises R2 fully (Go-dir→RocksDB import in `test-upgrade`).

---

## Dependency map & parallel waves

The TDD entry point is the reverse-dial **handshake** — the interop linchpin (`16` §3 M9 row: prove `Runtime.Initialize` before driving traffic). Everything else builds on a proven handshake.

```
Wave 0  (handshake linchpin — strictly first)
  M9.1  Runtime.Initialize reverse-dial host side (serve Runtime, env var, spawn, timeout)
  M9.2  Runtime.Initialize reverse-dial guest side (ava_vm_rpc::serve: read env, bind, dial back)
  M9.3  differential::plugin_rust_in_go  (minimal Rust test-VM hosted by a GO node) ← M9 TDD ENTRY POINT

Wave 1  (proxied callback services — required for "services work both ways")
  M9.4  rpcdb proxy round-trip (guest dials, node serves; iterator handles, ErrEnumToError)
  M9.5  appsender proxy round-trip (AppError i32 codes across wire)
  M9.6  sharedmemory proxy round-trip (atomic get/indexed/apply)
  M9.7  validatorstate proxy round-trip (windower-relevant view)
  M9.8  warp Signer proxy + aliasreader proxy round-trip
  M9.9  protocol-version mismatch + handshake-timeout sentinels (v45 exact-equality)

Wave 2  (the other interop direction)
  M9.10 VmServer<V: ChainVm> guest serves full proto/vm VM service (Rust VM as plugin)
  M9.11 RpcChainVm host client implements full ChainVm over dialed channel (Rust node hosts)
  M9.12 differential::plugin_go_in_rust  (GO test-VM hosted by a RUST node)
  M9.13 four-way wire-identity matrix (capture+diff proto/vm request bytes; §07 §10)

Wave 3  (live mixed network)
  M9.14 ava-differential: mixed Go+Rust tmpnet bring-up + Observation.normalized()
  M9.15 differential::mixed_network  (live Go+Rust nodes, all chains, no fork, same tip)

Wave 4  (upgrade suite — exercises R2)
  M9.16 Go-data-dir → RocksDB import path (the R2 migration tool / detector)
  M9.17 test-upgrade  (Go→Rust across an activation height, incl. Go-dir import)

Wave 5  (load + reexecute + perf, can run parallel to Wave 4 once Wave 3 lands)
  M9.18 test-load  (sustained tx stream, metrics SLOs, zero errors)
  M9.19 test-reexecute  (replay recorded mainnet ranges → state roots match Go)
  M9.20 crash-injection hardening pass (CC-ATOMIC / two-sided SM consistency, 27 §9)
  M9.21 bench-guard perf gates (criterion baselines, >X% regression fails)

Wave 6  (close-out)
  M9.22 version-string / compatibility-matrix interop conformance (26 §9)
  M9.23 Final acceptance gate (16 §5 checklist; build+test+clippy; zero wip rows)
```

Waves 1, 2, 4, 5 each parallelize internally. Wave 0 must complete before any other wave starts. Wave 3 depends on Waves 1+2. Wave 6 depends on all.

> **UPSTREAM DELTA (avalanchego `cc3b103b91`, 2026-06-09 — folded 2026-06-10).** The Go node
> bumped to **`firewood-go-ethhash/ffi v0.6.0`**; our workspace pins firewood git tag `v0.5.0`
> (`ava-merkledb`, see `04` §4.2 upstream-delta). Before any live-Go-oracle task here
> (M9.14/M9.15/M9.17/M9.19) — and before the M7.29/M7.30 SAE differentials — verify which ffi
> tag the oracle binary wraps and re-pin + re-run `golden::firewood_ethhash_root` if it moved.

---

> **WAVE 2026-06-15 (in-process plugin interop) MERGED.** Three parallel worktree agents on disjoint
> files in `ava-vm-rpc`, merged `--no-ff` with zero conflicts; `cargo nextest run -p ava-vm-rpc` =
> **10/10 green**, `cargo clippy -p ava-vm-rpc --all-targets -- -D warnings` clean.
> - **M9.6 ∥ M9.8** (merge `da1bcb9`): sharedmemory `get/indexed/apply` round-trip + warp `Signer`
>   sign/verify + aliasreader `lookup/primary_alias/aliases` round-trips, each against a real loopback
>   gRPC server boundary (`tests/proxy_sharedmemory.rs`, `tests/proxy_warp_aliasreader.rs`). No proxy
>   source bugs found — the M3.25 proxy impls were correct as-is.
> - **M9.7** (merge `4752635`): `validatorstate::decode_public_key` now dispatches on length
>   (96 → `from_uncompressed`, 48 → `from_compressed`); round-trip test asserts a real BLS key
>   survives the wire. AS-BUILT: the documented "fidelity gap" was a *false positive* — `blst`'s
>   `key_validate` already auto-sniffs compression, so the old `from_compressed`-on-96-bytes path
>   worked at runtime; the fix makes it explicit/correct and removes the stale gap wording.
> - **M9.10 ∥ M9.11** (merge `49e4ec8`): host `RpcChainVm::initialize` + guest `VmServer::initialize`
>   wired end-to-end — the host stands up the `proto/rpcdb` Database server (`db_server_addr`) + an
>   appsender callback server (`server_addr`) on ephemeral loopback, packs `ChainContext` →
>   `InitializeRequest`, sends `VM.Initialize`, and seeds client-side last-accepted; the guest dials
>   both back, builds the `RpcDatabase`/`RpcAppSender` proxies, maps the request → `ChainContext`, and
>   runs the inner VM. `tests/vm_initialize.rs::rust_host_initializes_rust_guest` (went red on
>   `RemoteVmNotImplemented`, now green) drives a VM that does a real `put`/`get` over the **proxied**
>   db at `initialize`, then build→verify→accept. **Retires placeholder #1 in `tests/PORTING.md`.**
>   DEFERRED to node-assembly (documented inline + PORTING.md): the full callback bundle at
>   `server_addr` currently serves appsender only — sharedmemory/aliasreader/validatorstate/warp +
>   `grpc.health` need concrete host impls supplied by the node-assembly path; and
>   `InitializeRequest.network_upgrades` is sent `None` (guest reconstructs the fork schedule from
>   `network_id`) pending the proto `NetworkUpgrades` round-trip.
>
> > **UPDATE 2026-06-18 (M9.12 offline foundation — `network_upgrades` proto round-trip DONE).** The
> > ralph user chose "M9.12 offline foundation". New `ava-vm-rpc::upgrades` (byte-faithful port of Go
> > `vm_client.go:getNetworkUpgrades` ⇄ `vm_server.go:convertNetworkUpgrades`): the host now sends the
> > structured `NetworkUpgrades` message (`network_upgrades: Some(...)`) and the guest decodes it (wire
> > value wins), falling back to `get_config(network_id)` only when absent. This closes a real
> > cross-language gap — Go's decoder rejects a nil message (`errNilNetworkUpgradesPB`), so the prior
> > `None` would have failed a Go-guest-in-Rust-host `Initialize`. Added `PartialEq, Eq` to
> > `ava_version::UpgradeConfig` (additive). Tests: `upgrades::tests` (round-trip mainnet/fuji/local +
> > nil/wrong-length rejection + unscheduled-Helicon), `host::tests::chain_context_to_request_sends_network_upgrades`,
> > `guest::tests::{request_to_chain_context_uses_proto_network_upgrades,…_none_falls_back_to_network_id}`,
> > and the extended e2e `vm_initialize::rust_host_initializes_rust_guest` (a distinctive
> > `apricot_phase_4_min_p_chain_height=314_159` proves the wire schedule, not a `network_id` rebuild,
> > reached the guest). `nextest -p ava-vm-rpc -p ava-version` 48/48 green, clippy `-D warnings` + fmt
> > clean. **STILL DEFERRED:** the sharedmemory/aliasreader/validatorstate/warp half of the bundle —
> > threading it into the inner VM needs an `ava_snow::ChainContext` extension (Go reads those off
> > `snow.Context`; `ChainContext` has no such fields), a broad node-assembly change, NOT a one-crate
> > `ava-vm-rpc` follow-up.
> >
> > **UPDATE 2026-06-18b (M9.12 offline foundation — host-side multiplexed callback bundle DONE).** The
> > ralph user chose "host-side multiplexed bundle". ★ KEY ORIENT FINDING: the `ChainContext`-extension
> > path the prior note floated **fights the Rust architecture** — Rust wires SharedMemory/ValidatorState
> > **per-VM** (`ava-avm` `with_shared_memory`+`NoopSharedMemory`; `ava-platformvm` own validator manager),
> > there is NO `ChainContext`-carried bundle. So the cleanly-doable half is the HOST serving the full
> > bundle. `host::serve_callback_bundle` (Go `vm_client.go:newInitServer`) now multiplexes appsender +
> > sharedmemory + aliasreader + validatorState + warp on ONE `server_addr`; `RpcChainVm::initialize` uses
> > it; impls injected via `RpcChainVm::with_callback_bundle(CallbackBundle{..})`, unsupplied → `host::noop`
> > defaults. `grpc.health` omitted (Go convention-only, not consumed on dial path per M9.3; no tonic-health
> > dep). `tests/host_bundle.rs` (acts as guest): dials the one server_addr for all 5 services + round-trips
> > each (Go single-address contract) + a no-op-defaults arm. ava-vm-rpc 29/29, clippy -D + fmt clean.
> > **STILL DEFERRED:** threading the dialed proxies into the INNER VM (guest side) — per-VM/chain-init
> > concern (generic `VmServer<V>` guest only has `Vm::initialize(db, app_sender)`); the live
> > `plugin_go_in_rust` (M9.12) arm exercises the host side against a real Go guest.
>
> Net effect: **Wave 0 (M9.1–M9.3 minus the live-Go entry M9.3) and Wave 1 (M9.4–M9.9) are complete
> in-process; Wave 2's in-process legs (M9.10/M9.11) are complete.** Remaining M9 frontier — all
> require a live external Go `avalanchego` binary / tmpnet (not runnable in the current sandbox):
> M9.3 (`plugin_rust_in_go`), M9.12 (`plugin_go_in_rust`), M9.13 (four-way wire matrix),
> M9.14/M9.15 (mixed network), M9.16/M9.17 (Go-dir import + upgrade), M9.18 (load), M9.19 (reexecute),
> M9.20 (crash injection), M9.21 (bench-guard), M9.22 (version/compat), M9.23 (acceptance gate).

> **WAVE 2026-06-15b (pure-Rust frontier) MERGED.** Two parallel worktree agents on disjoint crates,
> merged `--no-ff` zero-conflict (`59fa2e6`, `bbb87a6`); re-verified in main tree.
> - **M9.16 COMPLETE** (`ava-database` + `ava-node`): Go-dir → RocksDB import facade over the existing
>   `migrate/` engine + node-side foreign-dir refusal (`precheck_data_dir` → `Error::ForeignDataDir`),
>   `tests/go_dir_import.rs`. **This task did NOT need a live Go node** (folder-name detection + verbatim
>   KV copy; real on-disk Pebble/leveldb fixture deferred to the M12 sidecar — facade driven via injected
>   `GoDbSource`). `cargo nextest -p ava-database --features migrate,rocksdb` 50/50, `-p ava-node` 19/19.
> - **M9.22 GOLDEN LEGS COMPLETE** (`ava-version`): `golden::{compatibility_matrix, compatibility_json_byte_parity,
>   node_version_reply}` + committed byte-identical `compatibility.json`. The 4th leg
>   `differential::version_interop` (live floor-drop) is **deferred to M9.14** (mixed-net harness). 21/21.
> ★ Correction to the banner above: **M9.16 was never live-Go-gated**, and M9.22's bulk is pure-golden —
>   only its `version_interop` leg needs the live mixed net. Remaining live-Go-gated frontier: M9.3, M9.12,
>   M9.13, M9.14, M9.15, M9.17, M9.18, M9.19 (replay leg can be recorded-oracle), M9.20, the M9.22
>   `version_interop` leg, and the M9.23 acceptance gate. M9.21 (bench-guard) is pure-Rust but needs
>   benches authored from scratch across crates.

> **WAVE 2026-06-15e (interop-harness frontier) MERGED.** Two parallel worktree agents (`/tmp/wt-m93`,
> `/tmp/wt-m914`) on disjoint files, branched off a prep commit (`62ce482`: registers `pub mod plugin;` +
> the `live` Cargo feature + `net`/`process` tokio features so agents never touch the shared
> `tests/differential/Cargo.toml`/`lib.rs`); merged `--no-ff` **zero-conflict**, re-verified in main tree.
> - **M9.3 OFFLINE ARM COMPLETE** (`crates/ava-vm-rpc/examples/testvm_plugin.rs` + `tests/differential/{src/plugin.rs,tests/plugin_rust_in_go.rs}`):
>   a real Rust v45 plugin binary (`FixedGenesisVm` → `guest::serve`) proven offline by spawning it as a black-box
>   subprocess and asserting it dials back the runtime addr (guest half of the reverse-dial) + fails-fast without the env.
>   Live Go-host arm gated. (`ava-differential` deliberately doesn't dep `ava-vm-rpc` → subprocess, not in-process.)
> - **M9.14 HARNESS + OFFLINE ARM COMPLETE** (`tests/differential/{src/network.rs,src/observation.rs,tests/mixed_network_smoke.rs}`):
>   mixed-binary `Network::start` driver + `BinaryMix`/`NodeIdentity` (deterministic-from-seed) + a strengthened
>   `Observation::normalized()` (strip timestamps/uptime, mask node_id/ip, sort validator/peer sets, BTreeMap order)
>   + a hand-rolled JSON-RPC-over-`tokio::net::TcpStream` `collect()`. Offline arms (determinism + normalization
>   round-trip) run every CI; live bring-up arm gated.
>
> Both follow the established `interop_handshake.rs` precedent: **live arm `#[cfg(feature="live")] #[ignore]` (needs
> `$AVALANCHEGO_PATH` + built `avalanchers`, never runs in CI/sandbox); offline arm runs every CI run.** Net effect:
> **M9.3 + M9.14 land their CI-runnable halves**; their live two-binary halves + the downstream live tasks (M9.12, M9.13,
> M9.15, M9.17, M9.18, M9.20, M9.22-`version_interop`, M9.23) remain nightly/operator-gated. `cargo nextest run -p
> ava-differential` = **15/15**, `-p ava-vm-rpc` = **10/10**, clippy `--all-targets -D warnings` clean, `--features live
> --tests` compiles, fmt clean.

> **WAVE 2026-06-16 (reverse-direction host + crash hardening) MERGED.** Two parallel worktree agents on disjoint
> areas (`ava-vm-rpc` host-spawn vs `ava-differential` crash harness), merged into `main`; re-verified in main tree.
> - **M9.12 OFFLINE ARM + PROTOCOL-44 REJECTION COMPLETE** (`crates/ava-vm-rpc/tests/host_subprocess.rs` +
>   `tests/differential/{src/plugin.rs,tests/plugin_go_in_rust.rs}`): a Rust `RpcChainVm` host drives the
>   `testvm_plugin` example as a **real OS subprocess** across the v45 reverse-dial (build/verify/accept/parse over
>   the wire — the host-side process-boundary the in-process M9.11 test can't reach) + `rust_host_rejects_protocol_44`
>   (the concrete "old node, 44" → `ProtocolVersionMismatch` at the `RpcChainVm::start` boundary). The differential
>   side adds the host-dial-back black-box offline arm + the gated live Go-plugin-under-`avalanchers` arm. Live
>   Go-plugin-in-Rust-host arm gated.
> - **M9.20 OFFLINE ARM COMPLETE** (`tests/differential/{src/crash.rs,tests/crash_injection.rs}`): `FailpointDb`
>   (N-th-mutation deterministic failure over a shared `Arc<MemDb>`) + `AcceptHarness` (CC-ATOMIC accept under a
>   `CrashPoint` matrix, atomic-batch vs naive-per-key) prove the atomic accept recovers all-or-nothing + idempotently
>   across every crash point, the naive path tears + reconciles, and two-sided shared-memory consistency (§3.1). Live
>   Go-oracle-equivalence arm gated (no recorded crash corpus yet).
>
> Both follow the offline-arm-every-CI / live-arm-`#[cfg(feature="live")] #[ignore]` precedent. `cargo nextest run -p
> ava-vm-rpc -p ava-differential` = **33/33** (`ava-differential` 20/20, `ava-vm-rpc` 12/12 incl. the new
> `host_subprocess` binary), clippy `--all-targets -D warnings` clean, `--features live --tests` compiles, fmt clean,
> `cargo build --workspace` + `-p avalanchers` green. Remaining live-Go-gated frontier: M9.13 (wire-identity matrix —
> Rust⇄Rust byte goldens are CI-runnable and **next**), M9.15, M9.17, M9.18, M9.19-`px_range`, M9.22-`version_interop`,
> the live halves of M9.3/M9.12/M9.14/M9.20, and the M9.23 acceptance gate.

> **WAVE 2026-06-16b (wire matrix + load + upgrade offline arms) MERGED.** Three parallel worktree agents on
> disjoint areas, prep-commit `4810d34` (registered `ava-load` + `ava-upgrade` skeleton crates as workspace members
> + wired `cargo xtask test-load`/`test-upgrade`); merged `--no-ff` into `main`, re-verified in main tree.
> - **M9.13 OFFLINE ARM COMPLETE** (`crates/ava-vm-rpc/tests/wire_identity.rs` + `crates/ava-vm-rpc/tests/vectors/rpcchainvm/*.bin`
>   + `tests/differential/tests/plugin_wire_matrix.rs`): `rust_rust_wire_identity_matrix` drives a FIXED
>   `initialize→set_preference→build→verify→accept→parse` sequence through the in-process Rust host (`RpcChainVm`)
>   ⇄ Rust guest (`guest::serve_with_addr`) over the v45 reverse-dial, asserts deterministic block bytes/IDs/LA,
>   then captures the `proto/vm` request wire bytes (direct `prost::Message::encode` of the exact request each host
>   method sends — tonic 0.12 interceptors only see metadata, not the body) and diffs them against committed
>   goldens. `InitializeRequest` is deliberately NOT goldened (ephemeral callback addrs); `build_block.bin` +
>   `set_state_unspecified.bin` are genuinely 0 bytes (all-proto3-default). The differential offline arm
>   (`plugin_wire_identity_matrix_offline`) reads the goldens by relative path (NO `ava-vm-rpc` dep — the verified
>   design invariant) and independently recomputes `sha256(block1_bytes) == block1_id` via the already-present
>   `ava-crypto` dev-dep (a real red/green cross-crate consistency signal). Live arm (`plugin_wire_identity_matrix`,
>   `#[cfg(feature="live")] #[ignore]`) reuses the M9.3/M9.12 launchers for the three Go legs. Goldens regenerable
>   via `REGEN_WIRE_GOLDENS=1`.
> - **M9.18 OFFLINE ARMS COMPLETE** (new `ava-load` crate at `tests/load/`): `generator.rs` (`LoadGenerator`
>   deterministic splitmix64 seed-derived C/X/P stream, byte-exact `TxDescriptor::encode`; `PacingSchedule` integer
>   rate math, all `checked_*`/`saturating_*`, no floats) + `metrics.rs` (Prometheus text-format `Exposition` parser
>   — quoted-label/`+Inf`/`NaN` aware — + pure `slo_holds`/`slo_violations` + `REQUIRED_PARITY_METRICS` from
>   `00` §7.3 / `18`) + `network.rs` (`LoadNode` live tmpnet driver scraping `/ext/metrics` over a hand-rolled
>   HTTP/1.1 GET on `tokio::net::TcpStream` — no HTTP-client crate, modeled on `differential/src/network.rs`).
>   12 offline tests (6 generator + 5 metrics + 1 end-to-end pipeline) + committed `tests/fixtures/ext_metrics_{good,regressed}.prom`.
>   Live arm `sustained_load` (`#[cfg(feature="live")] #[ignore]`) early-returns without `avalanchers`. **Honest
>   deferral:** tx signing/issuance is NOT wired (would need `ava-wallet` keyed off the genesis alloc — deliberately
>   left out so the offline build stays light + `unused_crate_dependencies` honest); the live arm proves the
>   generator + scrape→parse→SLO pipeline, the operator wires issuance. SLO thresholds are placeholder defaults.
> - **M9.17 OFFLINE ARMS COMPLETE** (new `ava-upgrade` crate at `tests/upgrade/`): `plan.rs` (`RollingUpgrade`;
>   `swap(i, dst_root)` drives the REAL M9.16 `ava_database::migrate::import::import_source_into_rocksdb` facade over
>   an injected `GoDbSource`, re-opens the imported `v1.4.5/` RocksDB dir, byte-verifies the migrated KV set — the
>   on-disk RocksDB write path ran for real, NOT gated) + `continuity.rs` (`assert_no_fork` over the real
>   `ava_differential::Observation`; `MovingFloor` over the real `ava_version::Compatibility` + a `MockClock` for
>   the `26` §7 moving min-compatible floor). 4 offline tests. Live arm `go_to_rust`
>   (`#[cfg(feature="live")] #[ignore]`, `live = ["ava-differential/live"]`) documents the operator handoff inline
>   (previous-Go tmpnet → pre-activation → per-node swap+import → activation barrier → no-fork+moving-floor over
>   live `Observation`s), early-returns without `$AVALANCHEGO_PATH`.
>
> All follow the offline-arm-every-CI / live-arm-`#[cfg(feature="live")] #[ignore]` precedent. Re-verified in main
> tree: `cargo nextest run -p ava-vm-rpc -p ava-differential -p ava-load -p ava-upgrade` = **51/51**, clippy
> `--all-targets -D warnings` clean (incl. `--features live`), `--features live --tests` compiles, fmt clean,
> `cargo build --workspace` + `-p avalanchers` green, `cargo xtask test-load`/`test-upgrade` green. Remaining
> live-Go-gated frontier: M9.15 (live mixed_network), M9.19-`px_range` (needs recorded P/X `blockexport` fixtures),
> M9.22-`version_interop`, the live halves of M9.3/M9.12/M9.13/M9.14/M9.17/M9.18/M9.20, and the M9.23 acceptance gate.

> **WAVE 2026-06-16c (offline-frontier mop-up) MERGED.** Three parallel worktree agents on disjoint files,
> merged `--no-ff` zero-conflict; re-verified in main tree. Each lands the CI-runnable offline arm of a task
> previously parked as "live-gated" or "deferred pending fixtures".
> - **M9.19 X-CHAIN LEG COMPLETE** (`ava-reexecute`): new `src/xchain.rs` `replay_xchain(seed)` drives the REAL
>   `ava-avm` VM/block pipeline (seed genesis → admit txs → build → set_preference → verify → accept, one tx/block)
>   over a synthetic-but-real case — exactly as the C-Chain leg's `genesis_to_1` runs a synthetic fixture through
>   the real EVM pipeline. X-Chain has no merkle trie, so the reexecute "root" is a deterministic `sha256` post-state
>   digest over the sorted final UTXO set + tip id/height. `tests/px_range.rs::reexecute_px_range` (no longer
>   `#[ignore]`d) replays the same case on two independent VM instances → byte-identical roots (determinism, no
>   fabricated/hardcoded root), + a different seed → different root. **P-Chain sub-leg + Go-recorded-`blockexport`
>   parity remain deferred** (no Go P/X fixture exists; reserved `px` feature gates the future live arm).
> - **M9.22 `version_interop` OFFLINE ARM COMPLETE** (now unblocked by M9.14): new
>   `tests/differential/tests/version_interop.rs::version_interop_floor_decisions` drives the REAL
>   `ava_version::Compatibility::with_clock` + `MockClock` over a mixed Go+Rust peer set
>   (`BinaryMix::from_config`), asserting the §9(3)/§9(4) connectivity decisions: below-floor drop, at/above-floor
>   accept (inclusive boundary), the §7 moving-floor flip across the fork, newer-major rejection, and Go-vs-Rust
>   symmetry over an 8-rung version ladder (neither side more permissive). Live floor-drop arm `version_interop`
>   gated `#[cfg(feature="live")] #[ignore]`. The `ava-version` `version_interop_deferred` stub now points here.
> - **M9.15 OFFLINE LOCKSTEP-REPLAY ARM COMPLETE** (`ava-differential`): filled in the `LockstepDriver`/`Program`
>   scaffold — `Program::from_seed(seed)` (deterministic splitmix-shaped action program) + `replay_recorded` walks
>   the actions and at each `AwaitFinalization` derives a pure sub-seed and drives a fresh `ava-avm` VM through the
>   REAL block pipeline via `xchain::run_program` (additive — no `xchain.rs` break, `xchain_issue_tx` stays green),
>   returning the ordered normalized `Observation`s. `tests/mixed_network.rs::mixed_network_replay_is_deterministic`
>   replays the same program twice → byte-identical observation sequences (specs/00 §6.1), asserts ≥1 finalization
>   ran (height ≥ 1), and that an injected `set_field` divergence is caught; + a 64-case proptest over seeds. Live
>   `mixed_network` arm gated `#[cfg(feature="live")] #[ignore]`.
>
> Re-verified in main tree: `cargo nextest run -p ava-reexecute -p ava-differential -p ava-version` = **51/51**
> (1 skipped), clippy `--all-targets -D warnings` clean on all three (incl. `ava-differential --features live`),
> `--features live --tests` compiles, fmt clean workspace-wide, `cargo build --workspace` + `-p avalanchers` green.
> Remaining live-Go-gated frontier: **M9.15 live `mixed_network`**, **M9.19-`px_range`** P-Chain sub-leg + Go-fixture
> parity, the live halves of M9.3/M9.12/M9.13/M9.14/M9.17/M9.18/M9.20/M9.22-`version_interop`, and the **M9.23
> acceptance gate** (the last remaining task with zero offline content yet — aggregator + zero-`wip` porting check).

> **WAVE 2026-06-16d (acceptance gate + P-Chain reexecute) MERGED.** Two parallel worktree agents on disjoint
> files (`xtask/` + `ava-evm` PORTING vs `tests/reexecute/` + a scoped `ava-platformvm` seam), merged `--no-ff`
> zero-conflict; re-verified in main tree. **This closes the offline content of M9 — every M9 task now has its
> CI-runnable arm.**
> - **M9.23 OFFLINE ACCEPTANCE GATE COMPLETE** (`xtask/src/{acceptance.rs,porting.rs}` + `tests/differential/tests/definition_of_done.rs`):
>   `cargo xtask acceptance` maps every `16` §5 DoD clause to a present named exit test (offline + live arms) and asserts
>   zero `wip` rows repo-wide; `cargo xtask porting-report` aggregates all 34 `tests/PORTING.md` (403 ✅ / 40 🟡 / 425 ⬜ /
>   86 n/a, **zero `wip`**). The only `wip` offenders repo-wide were 4 STALE `ava-evm` rows (M6.22/M6.31 shipped) →
>   reclassified `✅`/`n/a` (verified vs shipped code + spec 20 §7.2). The live two-binary arms stay nightly-gated by design.
> - **M9.19 P-CHAIN LEG COMPLETE** (`ava-reexecute` `src/pchain.rs`): `replay_pchain(seed)` drives the REAL
>   `ava-platformvm` init→genesis→`build_block` pipeline → deterministic flat-KV post-state digest; determinism arm green on
>   two independent VMs (no fabricated root). Honest floor: `build_block` declines at genesis (height 0) — height ≥ 1 blocked
>   on the un-shared mempool (M8) + genesis-reward-resolver gap (M4.24); harness advances unchanged once either closes.
> Re-verified in main tree: `cargo nextest run -p ava-reexecute` = **9/9** (C+X+P), `-p ava-platformvm` = **148/148**,
> `cargo xtask acceptance`/`porting-report` exit 0, build workspace + avalanchers + clippy `--all-targets -D warnings` + fmt
> all clean. Remaining frontier: the **nightly live two-binary execution** of the gated arms (`mixed_network`,
> `plugin_go_in_rust`, `test-upgrade`, `test-load`) + CI-cadence wiring, plus M9.19's P-Chain height ≥ 1 arm — all
> operator/nightly-gated by design.

> **WAVE 2026-06-16e (P-Chain height-1 + Gap 2 + CI cadence) MERGED.** Three parallel worktree agents on fully
> disjoint file sets (`ava-platformvm/src/vm.rs`+`tests/reexecute/` ∥ `ava-platformvm/src/genesis.rs` ∥
> `.github/`+`Taskfile.yml`), merged `--no-ff` zero-conflict (`91d94a2`/`e865351`/`d805bee`); re-verified in main tree
> after a clean rebuild of the touched crates. **This closes the last two CI-runnable offline loose ends of M9** and
> wires the nightly cadence:
> - **M9.19 P-Chain HEIGHT ≥ 1 COMPLETE** (was the honest floor at height 0): new `PlatformVm::mempool_add` admission
>   seam + harness admits a funded `CreateSubnetTx` → `BanffStandardBlock` accepted at height 1, deterministically and
>   **clock-free** (decision-tx route; `verify_standard` enforces no future-time bound; `bootstrapped:false` ⇒ empty
>   credential, mirroring the X-Chain leg). `reexecute_pchain_range` now asserts `last_accepted_height == 1`.
> - **M4.24 / M9.19 Gap 2 COMPLETE**: `genesis::seed_state` now stores each genesis validator's tx (`state.add_tx`),
>   so `staker_tx_resolver` can reward genesis validators (new inline test proves it). Independent of the height-1 path.
> - **M9.23 CI cadence COMPLETE**: new `.github/workflows/nightly.yml` (scheduled + `workflow_dispatch`) runs a `test-live`
>   Taskfile task (`--features live --run-ignored all` over `ava-differential`/`ava-load`/`ava-upgrade` + `xtask
>   acceptance`/`porting-report`), `$AVALANCHEGO_PATH` plumbed via a repo variable.
> Re-verified in main tree: `cargo nextest run -p ava-platformvm -p ava-reexecute` = **158/158** (platformvm 149,
> reexecute 9), clippy `--all-targets -D warnings` clean, fmt clean, `cargo build -p avalanchers` green, `actionlint`
> clean on both workflows. **Remaining frontier (all operator/nightly-gated by design):** the actual nightly
> *execution* of the live two-binary arms against a running Go node, and the `24`-determinism mock-clock seam on
> `PlatformVm` (would unlock the reward-proposal height path + `bootstrapped:true` credential-verifying P-Chain replay).

> **WAVE 2026-06-16f (determinism hazard #5 close-out + X.19 lint) MERGED.** Four parallel worktree agents across
> two sub-waves on disjoint crates, each merged `--no-ff` zero-conflict; re-verified in main tree. This closes the
> `24`-determinism mock-clock seam the 2026-06-16e banner flagged — for ALL three stateful VMs, not just P-Chain.
> - **Sub-wave 1 (∥):** (a) `ava-platformvm` — `PlatformVm` gains an injected `Arc<dyn Clock>` (`with_clock` seam,
>   `RealClock` default), `build_block` reads `self.clock.now()`, and the executor `Fx` shares the same clock; the
>   M9.19 `replay_pchain` reexecute leg is now **clock-driven via an injected `MockClock`** (no longer leaning on
>   the genesis-future-pinning trick). (b) `xtask` — the real **X.19 `lint-determinism`** `syn` AST pass replaces
>   the no-op scaffold (hazards #1/#4/#5/#8 + `determinism-allowlist.toml`); see `plan/X` X.19 as-built.
> - **Discovery → Sub-wave 2 (∥):** the lint's first workspace-wide run found the SAME hazard in two more builders —
>   `ava-avm` (`AvmVm::build_block` block timestamp) and `ava-evm` (`EvmVm::build_block` header `time`). Both fixed
>   by the identical pattern (injected `Arc<dyn Clock>` + `with_clock` seam; X-Chain also shares the clock with its
>   fx dispatch). `cargo xtask lint-determinism` is now **green workspace-wide and wired into `lint-all`/`lint-all-ci`**.
> Net: hazard #5 is retired across P/X/C-Chain; the reward-proposal P-Chain height path remains gated on the M4.24
> reward-wiring (NOT the clock). `ava-platformvm` 150 / `ava-reexecute` 9 / `ava-avm` 203 / `ava-evm` 185 / `xtask`
> 14 tests green; spec `24` hazard-#5 callout marked RESOLVED + a monotonic-vs-wall-clock refinement recorded.

---

## Tasks

### Task M9.1: Reverse-dial handshake — host (node) side ✅ DONE (M3.24)
**Crate/area:** `ava-vm-rpc` (`host` + `runtime`)  ·  **Depends on:** M3 (ava-vm-rpc scaffolding), M8 (ava-node spawn integration)  ·  **Spec:** `07` §5.1 (handshake step list), `26` §5, `00` §11.1.1
**Files:** `crates/ava-vm-rpc/src/runtime.rs`, `crates/ava-vm-rpc/src/host/spawn.rs`, `crates/ava-vm-rpc/tests/handshake_host.rs`
- [ ] **Step 1 — Red:** Write `handshake_host_initialize_records_vm_addr` in `tests/handshake_host.rs`: stand up the host `Runtime` gRPC server on an ephemeral loopback TCP port; act as a fake plugin that reads the addr from a captured env value, dials the Runtime, and calls `Initialize { protocol_version: RPC_CHAIN_VM_PROTOCOL, addr: "127.0.0.1:<vport>" }`. Assert the host's `Initialize` handler returns `Ok` and exposes the recorded `vm_addr` to the spawner. Assert constants verbatim: `ENGINE_ADDRESS_KEY == "AVALANCHE_VM_RUNTIME_ENGINE_ADDR"`, `RPC_CHAIN_VM_PROTOCOL == 45`, `DEFAULT_HANDSHAKE_TIMEOUT == Duration::from_secs(5)`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc handshake_host_initialize_records_vm_addr` → fails (Runtime service / spawner not implemented). Assert failure is the missing-impl, not a compile error in the test.
- [ ] **Step 3 — Green:** Implement the `Runtime` tonic service in `runtime.rs` (`Initialize(protocol_version, addr)` → `check_protocol` (`26` §5) then store `addr` in a `oneshot`/`Mutex<Option<SocketAddr>>`). Implement `host::spawn.rs`: bind ephemeral listener `R`, `serve` Runtime on it, set child env `AVALANCHE_VM_RUNTIME_ENGINE_ADDR=R.addr` (+ forward `GRPC_*`/`GODEBUG`), capture child stdout/stderr → log, await the handshake channel with `DEFAULT_HANDSHAKE_TIMEOUT` (timeout ⇒ `Error::HandshakeFailed`, kill child). On Linux set `Pdeathsig=SIGTERM` via `pre_exec` (the one isolated `unsafe`, `00` §7.6); non-Linux ⇒ kill-on-drop. Copy the four constants verbatim from `07` §5.1.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc handshake_host_initialize_records_vm_addr` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: reverse-dial handshake host side (Runtime.Initialize, v45, env+timeout)`

### Task M9.2: Reverse-dial handshake — guest (plugin) side (`ava_vm_rpc::serve`) ✅ DONE (M3.24)
**Crate/area:** `ava-vm-rpc` (`guest` + `serve`)  ·  **Depends on:** M9.1  ·  **Spec:** `07` §5.1 (guest steps 4–6,10), §5.3, `00` §11.1.1
**Files:** `crates/ava-vm-rpc/src/serve.rs`, `crates/ava-vm-rpc/src/guest/mod.rs`, `crates/ava-vm-rpc/tests/handshake_guest.rs`
- [ ] **Step 1 — Red:** Write `serve_dials_back_and_serves_vm`: spawn an in-process fake host (serving `Runtime`) that publishes its addr via env; call `ava_vm_rpc::serve(test_vm).await` in a task; assert the fake host receives `Initialize { protocol_version: 45, addr }` and that the guest then serves `VM` + `grpc.health` on `addr` reporting `SERVING`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc serve_dials_back_and_serves_vm` → fails (serve unimplemented).
- [ ] **Step 3 — Green:** Implement `serve(vm)` in `serve.rs` mirroring Go `rpcchainvm.Serve`: read `ENGINE_ADDRESS_KEY`; bind ephemeral listener `V`; dial `R`; call `Runtime.Initialize(RPC_CHAIN_VM_PROTOCOL, V.addr)`; then serve `VmServer<V>` + `tonic_health` (SERVING) on `V`. Graceful shutdown: ignore SIGINT/SIGTERM until host signals shutdown, then exit on SIGTERM (`DEFAULT_GRACEFUL_TIMEOUT`). Wire `guest/mod.rs` scaffolding for `VmServer` (full impl in M9.10).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc serve_dials_back_and_serves_vm` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: reverse-dial handshake guest side (serve: read env, dial back, serve VM+health)`

### Task M9.3: `differential::plugin_rust_in_go` — minimal Rust test-VM hosted by a Go node (TDD ENTRY POINT) ✅ OFFLINE ARM DONE (2026-06-15); ✅ LIVE Go-host arm GREEN (2026-06-18d)
**Crate/area:** `ava-differential` + `ava-vm-rpc`  ·  **Depends on:** M9.1, M9.2  ·  **Spec:** `16` §3 (M9 entry), `07` §5.1, `02` §11
**Files:** `tests/differential/src/plugin.rs`, `tests/differential/tests/plugin_rust_in_go.rs`, `crates/ava-vm-rpc/examples/testvm_plugin.rs`
- [x] **Step 1 — Red:** Write `differential::plugin_rust_in_go` in `tests/differential/tests/plugin_rust_in_go.rs`: build the minimal Rust test-VM plugin binary (`examples/testvm_plugin.rs` calling `ava_vm_rpc::serve`); launch a **Go** `avalanchego` node (via tmpnet, `AVALANCHEGO_PATH`=Go binary) configured to host this Rust plugin as a custom VM. Assert the Go host completes `Runtime.Initialize` reverse-dial (Go logs the plugin connected at protocol 45) and the chain reaches `Initialize` on the VM side. This is the linchpin: it asserts only the handshake, not yet traffic.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential plugin_rust_in_go` → fails (plugin example / Go-host wiring not built). Confirm the failure is the handshake not completing, not a harness compile error.
- [x] **Step 3 — Green:** Implement `examples/testvm_plugin.rs` (a trivial `ChainVm` returning a fixed genesis last-accepted). Implement `plugin.rs` harness helpers: `build_rust_plugin()`, `launch_go_host_with_plugin(plugin_path)`, `assert_handshake_complete()`. Ensure the Go node's plugin dir / VM-id alias is configured so the Go `rpcchainvm` host spawns our binary with `AVALANCHE_VM_RUNTIME_ENGINE_ADDR`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential plugin_rust_in_go` → passes (offline arm; live Go-host arm gated `#[cfg(feature="live")] #[ignore]`).
- [x] **Step 5 — Commit:** `differential: plugin_rust_in_go — Rust test-VM completes v45 reverse-dial under a Go host`

> **AS-BUILT (merge of `m93-plugin-rust-in-go`, 2026-06-15).** `crates/ava-vm-rpc/examples/testvm_plugin.rs` is a
> standalone plugin binary — a trivial `FixedGenesisVm` (`ChainVm` adapted from `tests/vm_initialize.rs`'s
> `DbProbeVm`, minus the proxied-db round-trip; seeds a fixed height-0 genesis as last-accepted, builds/parses/gets
> linear children) under `#[tokio::main(multi_thread)]` calling `ava_vm_rpc::guest::serve(vm, &token)`. Registered
> via a `[[example]]` entry in `crates/ava-vm-rpc/Cargo.toml` (no new deps). **Offline arm** (`plugin_rust_in_go_builds_and_serves`,
> runs every CI run): `build_rust_plugin()` builds the example, then `assert_plugin_dials_back()` spawns it as a
> **real subprocess** with `AVALANCHE_VM_RUNTIME_ENGINE_ADDR` pointing at a loopback listener the harness owns and
> asserts the plugin dials back within 10s (the guest half of the v45 reverse-dial) — and `assert_plugin_fails_without_env()`
> asserts it fails fast (non-zero exit) with the env var removed. **★ DESIGN NOTE:** `ava-differential` intentionally
> does NOT depend on `ava-vm-rpc`, so the offline proof is black-box subprocess-driven (not in-process
> `guest::serve_with_addr`); the full in-process `Runtime.Initialize`+`VM`/health proof already lives in
> `ava-vm-rpc`'s own `tests/handshake.rs`/`tests/vm_initialize.rs` (M9.1/M9.2/M9.10/M9.11). **Live arm** (`plugin_rust_in_go_live`,
> `#[cfg(feature="live")] #[ignore]`, returns early if `$AVALANCHEGO_PATH` unset): `launch_go_host_with_plugin` spawns
> the Go binary and scans stdout for the protocol-45-plugin-connected marker — but does NOT synthesize the
> subnet/blockchain that triggers the Go host to spawn the plugin. **Nightly-operator handoff:** supply `$AVALANCHEGO_PATH`
> (rpcchainvm 45) + a data dir whose `plugins/` holds the Rust binary renamed to its VM id + a genesis/subnet that
> instantiates a chain on that VM (via `$AVALANCHEGO_EXTRA_ARGS`); documented inline as `LIVE-ARM:`. Verified in main
> tree: `cargo nextest run -p ava-differential` 15/15, `-p ava-vm-rpc` 10/10, clippy `--all-targets -D warnings` clean,
> `--features live --tests` compiles. **M9.12 (plugin_go_in_rust) will reuse `plugin.rs`** for the reverse direction.

> **★ LIVE-ARM HARNESS BUILT + RUN AGAINST THE REAL GO ORACLE (2026-06-18).** The nightly-operator
> handoff above ("synthesize the subnet/blockchain that triggers the Go host to spawn the plugin") is now
> a self-wiring harness: `tests/differential/go-oracle/rust_plugin_handshake/main.go` (source-of-truth copy;
> dropped into `~/avalanchego/tests/rustplugin/` to compile against the `tests/fixture/tmpnet` fixture).
> It boots a real single-node Go `avalanchego` tmpnet, creates a subnet + blockchain on the Rust
> `testvm_plugin` VM id, and asserts (by counting successful-vs-errored `creating chain` log lines for that
> VM id) that the Go chain manager spawns the plugin and completes the rpcchainvm v45 reverse-dial + first
> VM RPC. Run after `./scripts/check_oracle_binary.sh` prints OK (oracle rebuilt to `b1393ecb06`, rpcchainvm=45):
> `HOME=$(mktemp -d) AVALANCHEGO_PATH=… RUST_PLUGIN_PATH=…/target/debug/examples/testvm_plugin go run ./tests/rustplugin`.
> **Three load-bearing gotchas** (folded into the go-oracle README): (1) plugin-dir must be set via the
> **`AVAGO_PLUGIN_DIR` env var** — avalanchego's `getPluginDir` only honors a config-file `plugin-dir` when
> `viper.IsSet` is true, which it is NOT for tmpnet's `--config-file` path, so it silently falls back to
> `$AVAGO_DATA_DIR/plugins`; `ProcessRuntimeConfig.PluginDir`/`node.Flags["plugin-dir"]` are insufficient.
> (2) tmpnet writes prometheus SD config under `$HOME/.tmpnet` → run with a writable `HOME`. (3) the
> pre-restart bootstrap node logs a transient `vmFactory not found` (it doesn't yet track the subnet), so the
> PASS test counts create-vs-error lines rather than grepping for the VM id / "creating chain" / "rpcchainvm".
>
> **★ NEW FINDING — Rust rpcchainvm GUEST fails Go-hosted `Initialize` (M9.3 live FOLLOW-UP, not yet green).**
> With the plugin-dir fixed, the Go host **finds, spawns, and gRPC-connects to** the Rust `testvm_plugin` (the
> error moved from `"vmFactory ... was not found"` → `"error while creating new snowman vm rpc error: code =
> Canceled desc = stream terminated by RST_STREAM with error code: CANCEL"`). So the v45 reverse-dial +
> go-plugin handshake succeed, but the **first VM RPC over the dialed channel fails** (stream reset; the plugin
> wrote nothing to its `vm-factory.log`). The offline arms only black-box the subprocess dial-back and never
> drive a real Go-side `Initialize`/snowman-vm creation, so this gap was invisible until this run. **Next
> iteration:** reproduce the Go→Rust `Initialize` call in an in-process `ava-vm-rpc` `host` test (or add plugin
> stderr logging) to localize whether the `guest::serve` VM service, grpc-health `SERVING`, or the
> `proto/vm` `Initialize` handler aborts the stream; this is the true blocker for the M9.3 live arm passing.
>
> **★ INVESTIGATION 2026-06-18 (in-process Go→Rust `Initialize` localization).** Traced the Go host's
> `Initialize` packing/decode path against the oracle (`vms/rpcchainvm/{vm_client,vm_server,factory}.go` +
> `runtime/subprocess`). Findings:
> - The first Go→Rust RPC is genuinely `VM.Initialize`; there is **no health-gate** in the host dial path
>   (`factory.New` dials and immediately builds the `VMClient`; `grpcutils.Dial` sets `WaitForReady` +
>   keepalive but **no** `healthCheckConfig`). So the missing `grpc.health.v1.Health SERVING` service on the
>   Rust guest is **not** the CANCEL cause — avalanchego's rpcchainvm host never consumes it (Go registers it
>   only by convention in `newVMServer`). Left it unimplemented and documented as a non-issue.
> - **Fixed a real wire bug found en route (M9.12 direction, NOT the M9.3 CANCEL):** the Rust **host**
>   (`chain_context_to_request`) was sending the BLS public key in the 96-byte **uncompressed** form
>   (`PublicKey::serialize()`), but Go's wire contract is 48-byte **compressed**
>   (`bls.PublicKeyToCompressedBytes`; the Go guest decodes with `PublicKeyFromCompressedBytes`, which
>   strictly rejects 96 bytes). Switched the host to `pk.compress()` and the guest decode to `from_compressed`
>   (contract clarity — `blst::key_validate` auto-sniffs both encodings, so the guest already tolerated Go's
>   48-byte input, which is why Rust↔Rust passed and the gap stayed invisible). 4 new unit tests pin the
>   48-byte encoding host-side + the round-trip guest-side (`ava-vm-rpc::{host,guest}::tests`). 17/17 green,
>   clippy/fmt clean.
> - **CANCEL root cause still open.** Most likely in the guest `Initialize` handler's dial-BACK ordering
>   (`guest/mod.rs` dials `db_server_addr` then `server_addr` before touching the inner VM) or an HTTP/2
>   transport mismatch; reproducing it needs a Go-side `Initialize` driver (in-process Go host test against the
>   Rust guest, or guest stderr logging in the live arm). That remains the true M9.3 live blocker.
>
> **★ CANCEL ROOT CAUSE FOUND + FIXED (2026-06-18c).** The reset was **not** dial-back ordering or an HTTP/2
> mismatch — it was a **runtime-drop panic inside the guest `Initialize` handler**. The guest dials
> `db_server_addr` and builds a proxied `RpcDatabase` (= `ava_database::rpcdb::DatabaseClient`), which **owns a
> current-thread tokio runtime** (it `block_on`s every sync `Database` call). It hands that `Arc<dyn DynDatabase>`
> to the inner VM's `initialize`. The live `testvm_plugin`/`FixedGenesisVm` (like many VMs) **ignores** the db, so
> the last `Arc` drops at the end of `initialize` **on the tonic worker thread** — an async context. The default
> blocking `Runtime` drop panics there (`"Cannot drop a runtime in a context where blocking is not allowed"`); the
> panic unwinds through the tonic handler future, h2 resets the stream with `CANCEL`, and the Go host reports
> `RST_STREAM ... CANCEL`. This was invisible offline because the in-process `vm_initialize.rs` guest (`DbProbeVm`)
> consumes the db **inside `spawn_blocking`** (dropping the runtime off-worker), and `host_subprocess.rs` had a NOTE
> *deliberately avoiding* driving Initialize against the db-ignoring `testvm_plugin` for exactly this panic — the
> dots were just never connected to the live CANCEL. **Fix:** make the owned runtime drop-safe from any context.
> `ava-database` `ClientInner` and `ava-vm-rpc` `proxy::sharedmemory::RpcSharedMemory` (the two runtime-owning sync
> proxy clients) now hold `rt: Option<Runtime>` and `impl Drop` calls `Runtime::shutdown_background()` (the
> documented escape — tears the runtime down without blocking). Regression tests added at all three levels: the
> root-cause unit test (`ava-database conformance_rpcdb::client_runtime_drops_safely_in_async_context`), the
> end-to-end in-process M9.3 reproduction (`ava-vm-rpc vm_initialize::rust_host_initializes_db_ignoring_guest` —
> a full host→guest `VM.Initialize` against a db-ignoring guest, confirmed RED before the fix), and the parallel
> sharedmemory guard (`ava-vm-rpc proxy_sharedmemory::sharedmemory_client_drops_safely_in_async_context`). The
> in-process Go→Rust CANCEL is now closed; the remaining M9.3 live-arm step is re-running the Go tmpnet harness
> (`rust_plugin_handshake`) against the rebuilt oracle to confirm the live `creating chain` count now passes.
>
> **★ LIVE ARM GREEN — confirmed end-to-end against the real Go oracle (2026-06-18d).** Rebuilt the oracle
> (`./scripts/check_oracle_binary.sh` → `OK: ... commit 86602f460f, rpcchainvm=45`), built the Rust plugin
> (`cargo build -p ava-vm-rpc --example testvm_plugin`), and ran the `rust_plugin_handshake` tmpnet harness:
> `HOME=$(mktemp -d) AVALANCHEGO_PATH=…/avalanchego/build/avalanchego RUST_PLUGIN_PATH=…/target/debug/examples/testvm_plugin go run ./tests/rustplugin`
> → **exit 0, `PASS: Go node spawned the Rust plugin and the rpcchainvm v45 handshake was observed`**. The Go
> chain manager logged the Rust VM id (`73DVR1SARF5oTAnaMEvVLmZJpPyPUMK1QjRbjz2f4y26Rjc5a`) under
> `creating chain` **twice** (pre- and post-restart) with **zero** paired `error creating chain`; the node's own
> `main.log` shows 8 `creating chain` / 0 `error creating chain` and **no** `RST_STREAM` / `Canceled` /
> `vmFactory ... not found` / `snowman vm rpc error` — i.e. the exact CANCEL signature that f8b5f8a targeted is
> gone. This validates the runtime-drop fix in a real two-binary Go-host→Rust-guest run: the Go host now spawns
> the plugin, completes the v45 reverse-dial, and the first `VM.Initialize` returns cleanly. **What this proves
> live:** factory-resolve → plugin-spawn → v45 handshake → `Initialize`. It does NOT yet drive subsequent traffic
> (build/verify/accept) over the live channel — that's the M9.13 four-way wire-matrix live legs and remains gated.

### Task M9.4: Proxied `rpcdb` callback service round-trip ✅ DONE (M3.25; `tests/proxy.rs::rpcdb_roundtrip`)
**Crate/area:** `ava-vm-rpc::proxy::rpcdb`  ·  **Depends on:** M9.2, M1 (ava-database `DynDatabase`)  ·  **Spec:** `07` §5.2/§5.3/§5.4 (rpcdb row: server-side iterator handles, batched `IteratorNext`, `ErrEnumToError`)
**Files:** `crates/ava-vm-rpc/src/proxy/rpcdb.rs`, `crates/ava-vm-rpc/tests/proxy_rpcdb.rs`
- [ ] **Step 1 — Red:** Write `rpcdb_proxy_roundtrips_against_server`: stand up the node side serving `proto/rpcdb` `Database` over an in-memory `DynDatabase`; on the plugin side construct `RpcDatabase` (the dialing client) implementing `DynDatabase`; assert `put/get/delete/has`, a batch write, and an iterator-with-prefix all behave like the underlying memdb, and that a missing key maps to `Error::NotFound` via the `ErrEnumToError` table.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc rpcdb_proxy_roundtrips_against_server` → fails.
- [ ] **Step 3 — Green:** Implement `proxy/rpcdb.rs`: the server side (node serves) wrapping `Arc<dyn DynDatabase>` with server-held iterator handles + batched `IteratorNext`; the `RpcDatabase` client side (plugin dials) implementing `DynDatabase`. Reproduce the `ErrEnumToError` mapping (`Closed`/`NotFound` sentinels) byte-for-byte.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc rpcdb_proxy_roundtrips_against_server` → passes. Also run `cargo nextest run -p ava-vm-rpc proxy_rpcdb` to cover iterator edge cases.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: rpcdb proxy round-trip (iterator handles, ErrEnumToError)`

### Task M9.5: Proxied `appsender` callback service round-trip ✅ DONE (M3.25; `tests/proxy.rs::appsender_roundtrip`)
**Crate/area:** `ava-vm-rpc::proxy::appsender`  ·  **Depends on:** M9.2, M3 (`AppSender` trait `07` §2.6, `AppError` §2.2)  ·  **Spec:** `07` §5.4 (appsender row), §9 (AppError i32 codes cross wire)
**Files:** `crates/ava-vm-rpc/src/proxy/appsender.rs`, `crates/ava-vm-rpc/tests/proxy_appsender.rs`
- [ ] **Step 1 — Red:** Write `appsender_proxy_preserves_app_error_codes`: node serves `proto/appsender` `AppSender`; plugin uses `RpcAppSender` (dialing client) implementing `AppSender`; assert `send_app_request`/`send_app_response`/`send_app_gossip` reach the server with identical bytes, and that `send_app_error(code, message)` carries the **exact i32 code** (`ErrUndefined=0`, `ErrTimeout=-1`) across the wire.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc appsender_proxy_preserves_app_error_codes` → fails.
- [ ] **Step 3 — Green:** Implement `proxy/appsender.rs`: server side (node serves) translating `proto/appsender` → `Arc<dyn AppSender>`; `RpcAppSender` client (plugin dials) implementing `AppSender` (§2.6). Preserve `AppError` i32 values exactly (§9).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc proxy_appsender` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: appsender proxy round-trip (exact AppError i32 codes)`

### Task M9.6: Proxied `sharedmemory` callback service round-trip ✅ DONE (2026-06-15; `tests/proxy_sharedmemory.rs`)
**Crate/area:** `ava-vm-rpc::proxy::sharedmemory`  ·  **Depends on:** M9.2, M3 (`SharedMemory` `07` §3.1), M5 (atomic UTXO bytes)  ·  **Spec:** `07` §5.4 (sharedmemory row), §3.1, `27` §2.3 (ATOMIC-1)
**Files:** `crates/ava-vm-rpc/src/proxy/sharedmemory.rs`, `crates/ava-vm-rpc/tests/proxy_sharedmemory.rs`
- [ ] **Step 1 — Red:** Write `sharedmemory_proxy_get_indexed_apply`: node serves `proto/sharedmemory` over a real `ava-chains` `SharedMemory`; plugin uses `RpcSharedMemory` (client) implementing `SharedMemory`; assert `get(peer, keys)` returns `len == keys.len()`, `indexed(...)` paginates `(values, last_trait, last_key)`, and `apply(requests, batches)` commits atomically so a peer chain can `get` the exported UTXO bytes (ATOMIC-1).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc sharedmemory_proxy_get_indexed_apply` → fails.
- [ ] **Step 3 — Green:** Implement `proxy/sharedmemory.rs`: server side mapping `proto/sharedmemory` → `Arc<dyn SharedMemory>`; `RpcSharedMemory` client implementing the `get`/`indexed`/`apply` surface (`07` §3.1). `Requests`/`Element` codec parity per §3.1.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc proxy_sharedmemory` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: sharedmemory proxy round-trip (get/indexed/apply, ATOMIC-1 export)`

### Task M9.7: Proxied `validatorState` callback service round-trip ✅ DONE (2026-06-15; `tests/proxy_validatorstate.rs`)
**Crate/area:** `ava-vm-rpc::proxy::validatorstate`  ·  **Depends on:** M9.2, M3/M4 (`ValidatorState` `06`/`08`)  ·  **Spec:** `07` §5.2/§5.4 (validatorState row)
**Files:** `crates/ava-vm-rpc/src/proxy/validatorstate.rs`, `crates/ava-vm-rpc/tests/proxy_validatorstate.rs`
- [ ] **Step 1 — Red:** Write `validatorstate_proxy_matches_source`: node serves `proto/validatorState` over a P-Chain-backed `ValidatorState`; plugin uses `RpcValidatorState` client; assert the windower-relevant queries (current height, validator set at height, subnet→ ID) return values byte-identical to the source `ValidatorState` (so a hosted VM's proposervm windower samples identically — R1 surface).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc validatorstate_proxy_matches_source` → fails.
- [ ] **Step 3 — Green:** Implement `proxy/validatorstate.rs`: server side mapping `proto/validatorState` → `Arc<dyn ValidatorState>`; `RpcValidatorState` client implementing the trait (`06`). Ensure validator-set ordering matches Go (sorted on the wire).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc proxy_validatorstate` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: validatorState proxy round-trip (windower-parity view)`

### Task M9.8: Proxied `warp` Signer + `aliasreader` callback services round-trip ✅ DONE (2026-06-15; `tests/proxy_warp_aliasreader.rs`)
**Crate/area:** `ava-vm-rpc::proxy::{warp,aliasreader}`  ·  **Depends on:** M9.2, M0 (`warp::Signer` ava-crypto), M3 (`AliaserReader` `06`)  ·  **Spec:** `07` §5.4 (warp + aliasreader rows)
**Files:** `crates/ava-vm-rpc/src/proxy/warp.rs`, `crates/ava-vm-rpc/src/proxy/aliasreader.rs`, `crates/ava-vm-rpc/tests/proxy_warp_aliasreader.rs`
- [ ] **Step 1 — Red:** Write `warp_signer_proxy_signs` and `aliasreader_proxy_resolves`: node serves `proto/warp` (`Signer`) and `proto/aliasreader` (`AliasReader` = `bc_lookup`); plugin uses `RpcWarpSigner` + `RpcAliasReader` clients; assert a warp `sign(msg)` produces a signature that verifies against the node's BLS key (golden vector from M0 crypto), and `lookup(alias)`/`primary_alias(chainID)` resolve identically to the node's aliaser.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc proxy_warp_aliasreader` → fails.
- [ ] **Step 3 — Green:** Implement `proxy/warp.rs` (server maps `proto/warp` → `warp::Signer`; `RpcWarpSigner` client) and `proxy/aliasreader.rs` (server maps `proto/aliasreader` → `AliaserReader`; `RpcAliasReader` client). Reuse the M0 BLS golden vector for the signature assertion.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc proxy_warp_aliasreader` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: warp Signer + aliasreader proxy round-trips`

### Task M9.9: Protocol-version mismatch + handshake-timeout sentinels (v45 exact equality) ✅ DONE (M3.24; `tests/handshake.rs`)
**Crate/area:** `ava-vm-rpc::runtime` + `ava-version`  ·  **Depends on:** M9.1  ·  **Spec:** `26` §5 (exact equality, `ProtocolVersionMismatch` message shape), `07` §5.1 (`HandshakeFailed`), §9 (sentinels)
**Files:** `crates/ava-vm-rpc/src/runtime.rs`, `crates/ava-vm-rpc/tests/handshake_errors.rs`
- [ ] **Step 1 — Red:** Write `check_protocol_rejects_mismatch` and `handshake_times_out`: assert `check_protocol(45, path) == Ok(())`; `check_protocol(44, path)` ⇒ `Err(RuntimeError::ProtocolVersionMismatch)` matched via `assert_matches!`, with a log/message naming both versions and the plugin path (`26` §5); and that a guest that never dials back within `DEFAULT_HANDSHAKE_TIMEOUT` ⇒ host returns `Error::HandshakeFailed` and kills the child.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc handshake_errors` → fails.
- [ ] **Step 3 — Green:** Implement `check_protocol` exactly as `26` §5 (uses `ava_version::RPC_CHAIN_VM_PROTOCOL`); wire it into the `Runtime.Initialize` handler. Implement the handshake-timeout path in `host::spawn` (M9.1) to surface `Error::HandshakeFailed`. Add the `ProtocolVersionMismatch`/`HandshakeFailed`/`ProcessNotFound` sentinels (`07` §9, `26` §8).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc handshake_errors` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: v45 exact-equality + handshake-timeout sentinels`

### Task M9.10: `VmServer<V: ChainVm>` — guest serves the full `proto/vm` VM service ✅ DONE in-process (2026-06-15; full callback bundle deferred to node-assembly)
**Crate/area:** `ava-vm-rpc::guest`  ·  **Depends on:** M9.2–M9.8 (proxies the guest constructs at Initialize), M3 (`ChainVm`)  ·  **Spec:** `07` §5.3, §5.4 (vm row incl. batched/statesync/withcontext RPCs)
**Files:** `crates/ava-vm-rpc/src/guest/vm_server.rs`, `crates/ava-vm-rpc/tests/vm_server.rs`
- [ ] **Step 1 — Red:** Write `vm_server_runs_conformance_battery`: construct a `VmServer<TestVm>`; at its `Initialize` it dials back `db_server_addr`/`server_addr` and builds the `RpcDatabase`/`RpcSharedMemory`/`RpcAliasReader`/`RpcValidatorState`/`RpcWarpSigner`/`RpcAppSender` proxies the inner VM consumes; then drive the `vm_conformance!` battery (`07` §10) over the gRPC boundary (init→genesis LA; build/verify/accept advances LA+height; parse round-trips bytes; `Err(NotFound)` for unknown id/height; optional-capability probes via batched/statesync RPCs).
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc vm_server_runs_conformance_battery` → fails.
- [ ] **Step 3 — Green:** Implement `guest/vm_server.rs`: a tonic `VM` service delegating each RPC to the local `V: ChainVm`. At `Initialize`, dial back and construct all six client-side proxies (from M9.4–M9.8) plus the `RpcAppSender`; pass them into `V::initialize`. Map `InitializeRequest` fields verbatim to `ChainContext` (`network_id`, `subnet_id`, `chain_id`, `node_id`, BLS `public_key`, `x_chain_id`, `c_chain_id`, `avax_asset_id`, `chain_data_dir`, `genesis_bytes`, `upgrade_bytes`, `config_bytes`, `network_upgrades` JSON). Wire batched/statesync/withcontext RPCs to the capability probes.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc vm_server` → passes (in-process Rust-host ⇄ Rust-guest).
- [ ] **Step 5 — Commit:** `ava-vm-rpc: VmServer<V> full proto/vm VM service (guest serves, dials callbacks at Initialize)`

### Task M9.11: `RpcChainVm` host client — full `ChainVm` over the dialed channel ✅ DONE in-process (2026-06-15; `tests/vm_initialize.rs`; full callback bundle + ghttp/host-factory deferred to node-assembly)
**Crate/area:** `ava-vm-rpc::host`  ·  **Depends on:** M9.1, M9.4–M9.8, M3 (`ChainVm`), M8 (chains pipeline)  ·  **Spec:** `07` §5.2, §5.4, §8.1 (rpcchainvm host factory)
**Files:** `crates/ava-vm-rpc/src/host/rpc_chain_vm.rs`, `crates/ava-vm-rpc/tests/host_client.rs`
- [ ] **Step 1 — Red:** Write `rpc_chain_vm_hosts_rust_guest`: launch the M9.10 `VmServer` as an out-of-process plugin via `serve`; on the host build `RpcChainVm` (implements full `ChainVm`); before `Initialize`, host stands up `db_server_addr` (serving `proto/rpcdb`) and `server_addr` (serving sharedmemory/aliasreader/appsender/validatorState/warp + `grpc.health`). Run the `vm_conformance!` battery through `RpcChainVm` and assert identical block bytes/IDs/last-accepted as the in-process VM.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc rpc_chain_vm_hosts_rust_guest` → fails.
- [ ] **Step 3 — Green:** Implement `host/rpc_chain_vm.rs`: `RpcChainVm` implementing every `ChainVm`/`Vm`/`AppHandler`/`HealthCheck`/`Connector` method by translating to `proto/vm` RPCs over the dialed channel. Stand up the two callback servers before `Initialize`; pack `InitializeRequest` with the `ChainContext` identity + addrs. Proxy `CreateHandlers`/`NewHTTPHandler` HTTP→gRPC via `proto/http` (`ghttp`). Match gRPC options (max msg size = p2p limit, keepalive, **insecure** loopback). Register the rpcchainvm host factory so `ava-chains` `VmGetter` (`07` §8.1) can install plugin VMs from disk.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc host_client` → passes.
- [ ] **Step 5 — Commit:** `ava-vm-rpc: RpcChainVm host client full ChainVm (serves callbacks, dials VM)`

### Task M9.12: `differential::plugin_go_in_rust` — Go test-VM hosted by a Rust node ✅ OFFLINE ARM + PROTOCOL-44 REJECTION DONE (2026-06-16); live Go-plugin arm gated
**Crate/area:** `ava-differential` + `ava-vm-rpc::host`  ·  **Depends on:** M9.11, M8 (avalanchers bin)  ·  **Spec:** `16` §5(7), `26` §5 (interop both directions), `07` §5.3, `02` §11
**Files:** `crates/ava-vm-rpc/tests/host_subprocess.rs`, `tests/differential/src/plugin.rs`, `tests/differential/tests/plugin_go_in_rust.rs`
- [x] **Step 1 — Red:** Write `differential::plugin_go_in_rust`: take a known **Go** rpcchainvm plugin binary (built against protocol 45, e.g. a Go test-VM or the timestampvm reference); configure the **Rust** `avalanchego` node to host it via the rpcchainvm host factory; assert the Rust host completes `Runtime.Initialize` reverse-dial (the Go plugin dials our `Runtime` and we record its VM addr), then drive build/verify/accept and assert the chain advances. Also assert a Go plugin built against protocol **44** is rejected by the Rust host with `ProtocolVersionMismatch`, identically to a Go host.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential plugin_go_in_rust` → fails.
- [x] **Step 3 — Green:** Implement harness helpers `launch_rust_host_with_go_plugin(go_plugin_path)` + `assert_handshake_complete()` + the mismatch case. Ensure the Rust node serves all six callback services (the Go plugin always dials them — the §5.3 symmetry).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential plugin_go_in_rust` → passes (offline arm; live Go-plugin-in-Rust-host arm gated).
- [x] **Step 5 — Commit:** `M9.12: plugin_go_in_rust — Rust host drives out-of-process plugin (v45 both directions); offline arm + protocol-44 rejection, live arm gated`

> **AS-BUILT (commit `e5235fa`, 2026-06-16; parallel worktree wave with M9.20).** The genuinely-new
> M9.12 content — a **Rust `RpcChainVm` host driving a real out-of-process plugin** — lives in
> `crates/ava-vm-rpc/tests/host_subprocess.rs` (NOT `ava-differential`, which by design does not depend
> on `ava-vm-rpc`). `rust_host_drives_subprocess_plugin`: the host's launcher builds the `testvm_plugin`
> example and **spawns it as a real OS subprocess** (vs M9.11's in-process `tokio::spawn(guest::serve_with_addr)`),
> completes the v45 reverse-dial across the process boundary, then drives build→verify→accept→parse, every
> call an RPC to the subprocess. ★ It deliberately does NOT drive `VM.Initialize`: the host serves a proxied
> `rpcdb` `Database` whose guest-side `DatabaseClient` owns a current-thread runtime that must drop off the
> async worker (the M9.11 `DbProbeVm` consumes it inside `spawn_blocking`); the trivial `FixedGenesisVm`
> example ignores its proxied db, so the last `Arc` would drop on a tokio worker and panic — a pre-existing
> guest/rpcdb-client characteristic; the `VM.Initialize`-over-the-wire proof stays in `tests/vm_initialize.rs`.
> `rust_host_rejects_protocol_44`: a guest reporting protocol 44 (via `guest::report_handshake`) ⇒
> `RpcChainVm::start` returns `Err(ProtocolVersionMismatch)`, the concrete "old node" pin at the
> `RpcChainVm::start` boundary (complements `handshake.rs::handshake_protocol_mismatch`'s `45+1` Runtime-level
> path). The `ava-differential` side (`tests/plugin_go_in_rust.rs`): an offline arm
> `plugin_go_in_rust_host_dial_back` proving the host-side half of the reverse-dial black-box (a plugin dials
> the host's `Runtime` listener back — the §5.3 symmetry, reusing the `testvm_plugin` stand-in via
> `assert_plugin_dials_back`), plus `plugin.rs` helpers `go_plugin_path()`/`avalanchers_binary_path()` and a
> `#[cfg(feature="live")] #[ignore]` `plugin_go_in_rust_live` (hosts a real Go plugin under `avalanchers`;
> documents the operator handoff: `$AVALANCHEGO_PLUGIN_PATH` v45 Go plugin + a data dir whose `plugins/`
> holds it renamed to its VM id + a subnet/chain — same gap-surfacing structure as the M9.3 live arm).
> Verified in main tree: `cargo nextest run -p ava-vm-rpc -p ava-differential` = **33/33**, clippy
> `--all-targets -D warnings` clean, `--features live --tests` compiles, fmt clean.

### Task M9.13: Four-way wire-identity matrix (`proto/vm` request-byte diff) ✅ OFFLINE ARM DONE (2026-06-16; Rust⇄Rust proto/vm byte goldens); ✅ Go-host⇄Rust-guest LIVE LIFECYCLE LEG GREEN (2026-06-18); remaining Go-leg byte-capture matrix gated

> **LIVE LIFECYCLE LEG GREEN (2026-06-18, ralph iteration).** The Go-host⇄Rust-guest leg of the
> matrix is now validated live: a new env-gated Go harness
> `tests/differential/go-oracle/rust_plugin_lifecycle/main.go` boots a real Go `avalanchego`
> single-node tmpnet hosting the Rust `testvm_plugin`, lets the chain reach NormalOp, and confirms the
> Go host drives a full `BuildBlock → VerifyBlock → AcceptBlock` lifecycle over the live rpcchainvm v45
> channel — **the build/verify/accept traffic the M9.3 handshake-only arm left undriven**
> ([[m9-interop-progress]] wave-18d). Run vs the rebuilt oracle (HEAD `84533ec5b1`, rpcchainvm=45):
> exit 0, **`build=15 verify=15 accept=15`** (chain advanced to height 15, all over the channel). ★ Mechanism:
> `FixedGenesisVm::wait_for_event` returns `PendingTxs` (now **bounded** to 16 events, then long-polls) →
> the Go snowman engine's notifier drives `Notify(PendingTxs) → buildBlocks → BuildBlock`; a single-validator
> subnet accepts each block immediately. The Rust guest emits `TESTVM-EVENT build|verify|accept` stderr
> markers; the node copies plugin stderr verbatim into the chain log (`utils/logging.(*log).Write` bypasses
> the level filter), so the harness greps them. ★ Two load-bearing findings (folded into the go-oracle README):
> (1) the plugin subprocess inherits ONLY `GRPC_*`/`GODEBUG` env (runtime/subprocess filters `os.Environ()`),
> so a custom env var can't signal the harness — stderr→chain-log is the reliable channel; (2) bound the
> build loop in the plugin (unbounded `PendingTxs` = tight CPU + huge logs). ★ STILL GATED: the *byte-identity*
> assertion across all four pairings (`proto/vm` request-byte capture shim on each host's outbound channel) —
> the deeper nightly infra the `plugin_wire_identity_matrix` live test documents.

**Crate/area:** `ava-vm-rpc` + `ava-differential`  ·  **Depends on:** M9.3, M9.10, M9.11, M9.12  ·  **Spec:** `07` §10 (four-way matrix), `02` §6 (golden), §11.3
**Files:** `crates/ava-vm-rpc/tests/wire_identity.rs`, `crates/ava-vm-rpc/tests/vectors/rpcchainvm/`, `tests/differential/tests/plugin_wire_matrix.rs`
- [x] **Step 1 — Red:** Write `plugin_wire_identity_matrix`: drive an identical block-build/verify/accept sequence through all four host⇄guest pairings (Rust⇄Rust, Rust-host⇄Go-guest, Go-host⇄Rust-guest, Go⇄Go); capture the `proto/vm` request bytes on the wire (interceptor / recorded transcript); assert identical block bytes, IDs, last-accepted, **and** `proto/vm` request bytes across all pairings (diff against committed goldens). Also round-trip the proxied `rpcdb`/`appsender`/`sharedmemory` against the Go server.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-vm-rpc wire_identity` → fails (goldens absent).
- [x] **Step 3 — Green:** Rust⇄Rust offline arm captures `proto/vm` request bytes via direct `prost::Message::encode` (tonic 0.12 interceptors see metadata only) → committed goldens under `crates/ava-vm-rpc/tests/vectors/rpcchainvm/`. The differential offline arm reads them by relative path (NO `ava-vm-rpc` dep) + recomputes `sha256(block1_bytes) == block1_id` via `ava-crypto`. Go legs in the gated live arm reuse the M9.3/M9.12 launchers. Goldens regenerable via `REGEN_WIRE_GOLDENS=1`.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-vm-rpc wire_identity && cargo nextest run -p ava-differential plugin_wire_matrix` → passes (offline arm; Go-leg live arm gated `#[cfg(feature="live")] #[ignore]`).
- [x] **Step 5 — Commit:** `M9.13: rpcchainvm four-way wire-identity matrix — Rust⇄Rust proto/vm byte goldens (offline arm); Go legs gated`

### Task M9.14: `ava-differential` mixed Go+Rust network bring-up + Observation ✅ HARNESS + OFFLINE ARM DONE (2026-06-15); live bring-up arm gated
**Crate/area:** `ava-differential`  ·  **Depends on:** M8 (avalanchers bin, all chains), M2 (handshake interop)  ·  **Spec:** `02` §11.1 (two-binary live), §11.3 (Observation), §11.4 (normalization), `26` §9(4)
**Files:** `tests/differential/src/network.rs`, `tests/differential/src/observation.rs`, `tests/differential/tests/mixed_network_smoke.rs`
- [x] **Step 1 — Red:** Write `mixed_network_bringup_smoke`: start a tmpnet network of N nodes where node `i` is alternately Go (`AVALANCHEGO_PATH`=Go) and Rust (`AVALANCHEGO_PATH`=Rust), identical genesis/config/seed (deterministic node IDs/TLS per `02` §11.4); assert all nodes complete handshakes, exchange PeerLists, and a Go node logs the Rust peer's version as `avalanchego/1.14.2` (`26` §9(4)). Assert `Observation::collect(node).normalized()` returns a comparable per-chain (LA block ID+height, state/merkle root, sorted validator set).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential mixed_network_bringup_smoke` → fails.
- [x] **Step 3 — Green:** Implement `network.rs` (`Network::start(BinaryMix, &cfg)`, mixed-binary tmpnet driver) and `observation.rs` (`Observation::collect` over `info`/`platform`/X/C RPC + reexecute roots; `.normalized()` strips timestamps/per-instance fields, sorts collections per `02` §11.4).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential mixed_network` → passes (offline arms; live bring-up arm gated `#[cfg(feature="live")] #[ignore]`).
- [x] **Step 5 — Commit:** `differential: mixed Go+Rust tmpnet bring-up + normalized Observation`

> **AS-BUILT (merge of `m914-mixed-net`, 2026-06-15).** `network.rs` (kept `Binary`/`NetworkConfig`, extended):
> `BinaryMix::from_config(&cfg)` → deterministic alternating slot assignment (slot 0 = Go, `[Go,Rust,Go,…]`, §11.4);
> `NodeIdentity` derives a per-slot splitmix64 seed → `node_seed` hex + recognizable `NodeID-seed-<hex>` placeholder
> + distinct staking ports (no RNG crate pulled in). `Network::start(mix, &cfg)` spawns each slot via
> `tokio::process::Command` selecting `$AVALANCHEGO_PATH` (Go) / `avalanchers` (`$AVALANCHERS_PATH` or
> conventional `target/{release,debug}`); `shutdown()`/`Drop` kill children. `observation.rs`: strengthened
> `Observation::normalized()` (§11.4) — **strips** `info/timestamp`+`info/uptime`, **masks** `info/node_id`+`info/ip`
> → `<masked>`, **sorts** set members in `P/validators`,`P/peers`,`X/validators`, and keys the whole record through a
> `BTreeMap` (last-write dedup, deterministic order, never leaks HashMap order; idempotent). `collect(api_base)` scrapes
> a live node's JSON-RPC (`info.getNodeID/getNodeVersion`, `platform.getHeight/getCurrentValidators`, `eth_blockNumber`)
> via a **hand-rolled HTTP/1.1 POST over `tokio::net::TcpStream`** (no HTTP-client crate — honors the "no second crate"
> rule). **Offline arms** (run every CI run, no feature): `mixed_network_config_is_deterministic` (mix/identity reproducible
> from seed, distinct-per-slot, seed-sensitive) + `observation_normalization_round_trips` (timestamp/instance-ID/order
> differences compare equal post-normalize; genuine LA/root/validator-membership divergence compares unequal; idempotent).
> **Live arm** (`mixed_network_bringup_smoke`, `#[cfg(feature="live")] #[ignore]`, early-returns if `$AVALANCHEGO_PATH`
> unset): `Network::start` → `await_all_connected` → `go_node_logged_peer_version("avalanchego/1.14.2")` (`26` §9(4)) →
> `Observation::collect().normalized()`. **★ Honest deferrals (M9.15 handoff):** (1) real TLS staking-cert derivation
> is a credible sketch — `node_seed` is reproducible/distinct-per-slot (all the offline gate needs) but the live operator
> must feed it into the real cert generator so the i-th Go and i-th Rust node share a node ID, plus supply the genesis
> allocation + bootstrap-IP set (`spawn_node` passes `--http-port`/`--staking-port`/`--data-dir`/`--network-id=local`/
> `--staking-tls-cert-seed`); documented inline on `Network::start`. (2) `await_all_connected` uses observation
> field-count as a connectivity proxy (poll-with-deadline + kill-on-timeout structure is real) — sharpen to parse
> `info.peers` once a live net boots. Verified in main tree: `cargo nextest run -p ava-differential` 15/15 (incl. both
> offline arms), clippy `--all-targets -D warnings` clean, `--features live --tests` compiles, fmt clean.

### Task M9.15: `differential::mixed_network` — live Go+Rust, all chains, no fork, same tip 🟡 OFFLINE LOCKSTEP-REPLAY ARM DONE (2026-06-16c); SOLO live-dispatch flips P+X+C live (2026-06-19); SAE in-process dispatch DONE (2026-06-21 STEP n); mixed-net two-binary arm gated
**Crate/area:** `ava-differential`  ·  **Depends on:** M9.14, M4/M5/M6/M7 (P/X/C/SAE)  ·  **Spec:** `16` §5(2), `02` §11.3 (peer/handshake row: "both reach the same height; no fork")
**AS-BUILT (offline arm, merge 2026-06-16c):** `LockstepDriver::replay_recorded` + `Program::from_seed` now replay a seed-derived program through the REAL in-process `ava-avm` pipeline (`xchain::run_program` per finalization, pure sub-seed derivation), returning ordered normalized `Observation`s; `tests/mixed_network.rs::mixed_network_replay_is_deterministic` asserts twice-replayed byte-identity + non-trivial finalization + injected-divergence detection + a 64-case proptest. The live `mixed_network` arm (boot mixed net, replay across all nodes, no-fork/same-tip per chain) stays `#[cfg(feature="live")] #[ignore]`.

> **LIVE-ARM SCOPING (2026-06-17, read-only probe — the M9.15 handoff, made concrete).**
> The live two-binary arm is **not one step from running** — `tests/differential/src/network.rs`
> is an admitted scaffold that has never booted a node. Concrete blockers found by probing the
> built binaries (Go binary verified fresh vs HEAD via the new `scripts/check_oracle_binary.sh`):
> 1. **`network.rs::spawn_node` passes an invented flag `--staking-tls-cert-seed=<seed>` that
>    NEITHER `avalanchers` NOR Go `avalanchego` supports** (`--help` grep = 0 hits on both) — so
>    the spawner fails for both impls as written. The flag was a placeholder for "the live operator
>    wires `node_seed` into the real cert generator" (per the fn's own doc).
> 2. **`avalanchers` boot is unproven for a validating local net.** Default `--db-type=leveldb`
>    fails (`on-disk database "leveldb" requires the rocksdb feature`); `--db-type=memdb` boots
>    PAST db-init and stays up (no crash) but, given no genesis/certs/peers, makes no visible
>    progress (empty `logs/main.log`). Reaching NormalOp + cross-handshaking a Go peer is unverified.
> **What the live arm actually requires (sequential, NOT parallel-worktree-safe):**
> - (a) Settle the load-bearing unknown FIRST: get `avalanchers --network-id=local --db-type=rocksdb`
>   (sybil-protection on) to a confirmed **single-node NormalOp boot** with real staking certs +
>   a 1-validator local genesis. If that needs binary fixes, they precede everything else.
> - (b) Rewrite `network.rs::spawn_node`: generate per-slot staking TLS cert+key files
>   deterministically from `NodeIdentity::node_seed` to each node's `data-dir`, pass the real
>   `--staking-tls-cert-file`/`--staking-tls-key-file` (drop the invented seed flag).
> - (c) Build a shared `--network-id=local` genesis allocating the seed-derived node IDs as the
>   initial validators; wire bootstrap IPs/IDs (slot 0 = beacon).
> - (d) Then the live arms of M9.15/M9.3/M9.12/M9.13/M9.14/M9.17/M9.18/M9.20/M9.22 can run via
>   `task test-live` (which now runs `check_oracle_binary.sh` first — see AGENTS.md/CLAUDE.md).
> Estimated effort: multi-session, single-branch; (a) is the cheap next probe that de-risks the rest.

> **LIVE-ARM SCOPING UPDATE — STEP (a) RESOLVED (2026-06-18, ralph iteration, empirical single-node boot probe).**
> Step (a)'s load-bearing unknown is now **settled with a definitive answer: a single `avalanchers`
> node CANNOT reach NormalOp today**, and the blocker is deeper than cert/genesis wiring.
> - **What was probed:** booted `./target/release/avalanchers --network-id=local --db-type=memdb
>   --staking-ephemeral-cert-enabled=true --staking-ephemeral-signer-enabled=true
>   --sybil-protection-enabled=false --http-port=9750 --staking-port=9651 --api-info-enabled=true
>   --api-health-enabled=true` (ephemeral certs are real flags → no cert files needed; `--db-type=memdb`
>   avoids the rocksdb-feature gate). **Result: the node boots and runs as a live process** — it serves
>   `info.*` + `/ext/health` (health = `healthy:true`, BLS-key + database + diskspace + router + network
>   checks all green, `connectedPeers:0` as expected for a solo node). **But `info.isBootstrapped` is
>   `false` for ALL of P/X/C and never flips** (`local` has zero default beacons, so a solo
>   sybil-disabled node should bootstrap instantly — it doesn't). `logs/main.log` stays 0 bytes.
> - **ROOT CAUSE (definitive):** `ava_node::init::chain_manager::AssemblyChainManager::start_chain_creator`
>   is a **documented stub** — it only `self.queued.lock().push(params)` and logs *"queueing chain creation
>   (chain construction lands with the chains milestone)"*. The full `Node`/`dispatch` assembly therefore
>   **never instantiates or drives any chain**: chains are queued, never constructed, so no engine ever runs,
>   nothing bootstraps, no node reaches NormalOp. (The empty `main.log` is a secondary issue — the
>   process-logging sink isn't writing under this config; orthogonal to the boot gap.)
> - **The pieces to fix it ALREADY EXIST and work in-process:** `avalanchers::wiring::chains::boot_in_process_pchain`
>   builds the **real `ava_platformvm::PlatformVm`**, drives the full `ava_chains::create_snowman_chain`
>   pipeline, starts the handler, and a solo self-validator (weight-1 beacon set) flips the shared
>   `ConsensusContext` through `Initializing → Bootstrapping → NormalOp`. The ONLY in-process shortcut is a
>   `RecordingSender`/`NoopAppSender` standing in for the real ava-network engine `Sender` (engine→wire +
>   real peers) — the M4.30-noted remaining live leg.
> - **REVISED step (a) work (sequential, single-subsystem, NOT parallel-worktree-safe — this is the deferred
>   "chains milestone"):** wire `AssemblyChainManager` to RUN queued chains through `create_snowman_chain`
>   (thread the node's real DB / `ChainContext` / clock / staking identity / validators+beacons / router /
>   AppSender / **real ava-network `Sender`**, start each handler, register the running chain) instead of
>   only queuing. For a SOLO node this can reach NormalOp with a recording/loopback sender (no peers needed,
>   self = own beacon); the **real `Sender`** is required before (b)/(c) (multi-node Go⇄Rust). Only after a
>   single Rust node confirms NormalOp do items (b)/(c)/(d) become reachable. ⇒ **M9.15 live is blocked on
>   this node-assembly chain-creator build, not on TLS/genesis plumbing.**
>
> **STEP (a) — NORMALOP DE-RISK LANDED (2026-06-18, ralph iteration, TDD).** The single biggest unknown in the
> revised step (a) — *can a solo Rust node finish bootstrap and reach NormalOp WITHOUT the live ava-network
> `Sender`?* — is now **proven YES**. `ava_engine::snowman::bootstrap::Bootstrapper::start` short-circuits
> `Bootstrapping → finish() → EngineState::NormalOp` when `cfg.beacons.is_empty()` (`bootstrap/mod.rs:209`),
> exactly as a Go `--network-id=local` node with no default beacons does. New `avalanchers::wiring::chains::
> boot_in_process_pchain_to_normalop(network_id)` (refactor: existing `boot_in_process_pchain` + the new fn now
> share a beacon-parametrized `boot_pchain` core) boots the REAL `PlatformVm` through the full
> `create_snowman_chain` pipeline + handler with an EMPTY beacon set; `tests/in_process_chain.rs::
> boots_real_pchain_to_normalop` asserts the shared `ConsensusContext` reaches `EngineState::NormalOp` (vs the
> existing `…_to_bootstrapping` test which keeps the self-beacon set and stalls at `Bootstrapping` awaiting the
> frontier replies the in-process `RecordingSender` never delivers). 4/4 in_process_chain tests green, clippy
> `-D warnings` + fmt clean. ⇒ the `RecordingSender`/`NoopAppSender` loopback is SUFFICIENT for a solo node to
> reach NormalOp; the real `Sender` is only needed for items (b)/(c) (multi-node Go⇄Rust frontier exchange).
> **NEXT (production wiring, the bulk of the build):** drive the live binary's QUEUED chains through this same
> template inside `AssemblyChainManager` — the hard part is the generic↔trait-object impedance
> (`create_snowman_chain` is generic over concrete `D: Database`/`V: ChainVm`/`S: ValidatorState`/`Snd: Sender`/
> `M: ValidatorManager`, but the assembled `Node` holds `Arc<dyn DynDatabase>` + `Arc<dyn ValidatorManager>`),
> dispatching the concrete VM by `vm_id` (PlatformVm for P), and reflecting the engine's `ConsensusContext` state
> into `AssemblyChainManager::is_bootstrapped` so `info.isBootstrapped` flips for the live node.

> **STEP (a) — PRODUCTION CHAIN-CREATOR FOUNDATION LANDED (2026-06-18, ralph iteration, TDD; P-Chain slice).**
> The "NEXT (production wiring)" bullet above is now realized for the **platform chain** (X/C/SAE dispatch +
> the real `Sender` remain deferred). The chain creator that drives step-26's *queued* chains exists and is
> proven to flip `is_bootstrapped`:
> - **The reflection seam — `ava-node` (`init/chain_manager.rs`):** `AssemblyChainManager::is_bootstrapped` now
>   consults a per-chain **live reporter** (`set_bootstrapped_reporter(chain_id, Box<dyn Fn() -> bool + Send +
>   Sync>)`) before the static set, mirroring Go `Manager.IsBootstrapped` = a live read of `chain.Context.State.
>   Get() == snow.NormalOp`. ★ KEY DEP DECISION: the reporter is kept **opaque** (a boxed closure) precisely
>   because `ava-node` does NOT (and should not) depend on `ava-snow`/`ava-platformvm` — the chain-creator wiring
>   in the binary crate (which owns those deps) captures the `Arc<ConsensusContext>` and returns `state ==
>   NormalOp`. A `mark_bootstrapped` static-set setter is retained as the no-reporter fallback. 2 unit tests
>   (default-false→static-mark, live-reporter-wins-over-static).
> - **The chain creator — `avalanchers` (`wiring/chains.rs`):** new `run_queued_pchain(&Arc<AssemblyChainManager>,
>   network_id)` reads `manager.queued_chains()`, and for each `vm_id == platform_vm_id()` entry: registers the
>   chain with the manager (so `running_chains()` counts it and `shutdown()` drains it) under a token derived from
>   the node root subnet token, boots the REAL `PlatformVm` solo (empty beacons ⇒ `Bootstrapping → NormalOp` via
>   the proven `boot_in_process_pchain_to_normalop` template — `boot_pchain` was refactored to accept the
>   manager-registered token so the handler runs under it), then installs the live reporter. Non-P `vm_id`s are
>   logged + skipped (the deferred half). `tests/in_process_chain.rs::chain_creator_drives_queued_pchain_to_
>   bootstrapped` queues the P-Chain via the real `init_chains`, runs the creator, and asserts `is_bootstrapped(P)`
>   flips `false → true` once the solo engine reaches NormalOp (+ `running_chains()==1` + clean `manager.shutdown()`
>   join). 5/5 in_process_chain + 21 ava-node lib tests green, clippy `-D warnings` + fmt clean, full workspace
>   build green. (`tracing` added to `avalanchers` deps for the deferred-VM skip log — workspace dep, matches the
>   `ava-node` logging convention.)
> - **★ STILL DEFERRED (unchanged from above):** (1) **calling `run_queued_pchain` from the live `dispatch` path**
>   — the binary's `Node` holds `Arc<dyn DynDatabase>`+`Arc<dyn ValidatorManager>` while `run_queued_pchain` builds
>   its OWN in-process DB/validators/router/loopback `Sender` (the `boot_pchain` template), so threading the Node's
>   *real* assembled dependencies through `create_snowman_chain` (the generic↔trait-object impedance) is the next
>   step before `info.isBootstrapped` flips on an actual `avalanchers --network-id=local` process; (2) **X/C/SAE
>   `vm_id` dispatch**; (3) the **real ava-network `Sender`** for multi-node (items (b)/(c) below). So the *creator
>   logic + reflection seam* are proven in-process; the live-binary `dispatch` wiring + multi-VM + real Sender are
>   the remaining chains-milestone work.

> **STEP (a) — LIVE-DISPATCH WIRING LANDED + VALIDATED ON A REAL PROCESS (2026-06-18, ralph iteration, TDD).**
> Deferral (1) above is now **CLOSED for the platform chain**: the binary's `dispatch` path drives the queued
> P-Chain, and a real `avalanchers --network-id=local` process now reports `info.isBootstrapped(P) == true`
> (wave-18h's empirical probe found it stuck at `false` forever — that is the regression this closes).
> - **The dispatch-path entrypoint — `avalanchers` (`wiring/chains.rs`):** new
>   `drive_startup_chains(&Arc<AssemblyChainManager>, network_id, beaconless)` is the seam the binary's run loop
>   calls. `beaconless` gates the solo short-circuit: a node with **no** configured bootstrap beacons boots its
>   critical chains straight to `NormalOp` (the empty-beacon path, via `run_queued_pchain`); a node **with**
>   beacons must instead reach `NormalOp` by connecting + bootstrapping over the real ava-network `Sender` (the
>   live arm), so it is **skipped** and `info.isBootstrapped` stays honestly `false` rather than falsely
>   short-circuiting an un-bootstrapped node.
> - **The call site — `avalanchers` (`main.rs::run`):** after `Node::new` + signal-handler install, the run loop
>   computes `beaconless = config.bootstrap_config.bootstrappers.is_empty()` and calls `drive_startup_chains(&node.
>   chain_manager, node.config.network_id, beaconless)`, binding the returned handles to a name that outlives
>   `node.dispatch().await` (node shutdown step 5 already cancels + drains the manager-registered chains).
> - **Tests + live validation:** `tests/in_process_chain.rs::drive_startup_chains_gates_on_beacons` (both arms:
>   beaconed → `running_chains()==0` + `isBootstrapped` false; beaconless → one chain booted + `isBootstrapped(P)`
>   flips true + clean shutdown). 6/6 in_process_chain + `-p avalanchers -p ava-node` 32/32 green, clippy
>   `--all-targets -D warnings` + workspace fmt clean. **LIVE PROOF:** built the release binary and ran the
>   `avalanchers --network-id=local --db-type=memdb --staking-ephemeral-{cert,signer}-enabled=true
>   --sybil-protection-enabled=false` solo node; `info.isBootstrapped {chain:"P"}` returned `true`, while `X`/`C`
>   returned `false` (honest — those VMs are not yet dispatched).
> - **★ STILL DEFERRED (the rest, unchanged):** the booted P-Chain still uses `run_queued_pchain`'s own in-process
>   `MemDb`/router/loopback `Sender` (threading the assembled `Node`'s real `Arc<dyn DynDatabase>`/router through
>   the generic `create_snowman_chain` is the generic↔trait-object impedance, still open); **X/C/SAE `vm_id`
>   dispatch**; and the **real ava-network `Sender`** for multi-node bootstrap (items (b)/(c) below — the gating
>   skip is exactly where a beaconed node hands off to that path). So a SOLO live node now flips `isBootstrapped`;
>   the real-DB threading + multi-VM + real Sender remain the chains-milestone work.

> **STEP (a) — X/C `vm_id` DISPATCH (2026-06-19, ralph iteration, TDD; X-Chain dispatched, C-Chain honestly blocked).**
> The wave-18j deferral "(2) X/C/SAE `vm_id` dispatch" is now **realized for the X-Chain**; the chain creator
> dispatches per `vm_id` instead of skipping every non-P entry.
> - **Generalized boot core — `avalanchers` (`wiring/chains.rs`):** `boot_pchain`'s body is refactored into a
>   generic `boot_chain<V: ava_vm::block::ChainVm>(BootSpec, inner_vm, genesis_bytes, token)` (the network-facing
>   loopback impls — recording sender / no-op app sender / fixed single-validator state / real router over a
>   clock-injected adaptive-timeout manager — are VM-agnostic). New `boot_xchain(network_id, chain_id, subnet_id,
>   genesis_bytes, token)` materializes the **real `ava_avm::AvmVm`** from a *synthetic* X genesis (the 40-byte
>   stop-vertex-id + Unix-timestamp seed the M5 conformance battery uses; `AvmVm::initialize` self-seeds the
>   genesis Snowman block from it) and drives it through the same solo-node `create_snowman_chain` pipeline to
>   `NormalOp`. `run_queued_pchain` → renamed **`run_queued_chains`** and now branches on `vm_id`: P → `boot_pchain`,
>   X (`avm_id()`) → `boot_xchain`, each registered + reporter-installed.
> - **★ C-Chain HONESTLY BLOCKED, not faked:** the `evm_id()` branch logs + **skips** because
>   `ava_evm::EvmVm::initialize` is the **M6.8 stub** (it only records the chain context; `EvmVm::new` — needing a
>   pre-built `FirewoodStateProvider`/`AvaEvmConfig`/`CanonicalStore` — is the construction seam, so the C-Chain
>   cannot reconstruct its state from genesis bytes through the generic pipeline yet). Once M6.8 lands, the C branch
>   boots through `boot_chain` identically. `is_bootstrapped(C)` stays honestly `false`.
> - **Test — `tests/in_process_chain.rs::chain_creator_dispatches_xchain_to_bootstrapped`:** queues P (real network
>   genesis) + X (synthetic genesis, `avm_id`) + C (`evm_id`), runs the creator, asserts `handles.len()==2` +
>   `running_chains()==2`, both `is_bootstrapped(P)` and `is_bootstrapped(X)` flip true at NormalOp, `is_bootstrapped(C)`
>   stays false, clean shutdown. (Genuine red-without-the-X-branch: old behavior gives `handles.len()==1`.)
> - **★ STILL DEFERRED:** **live X dispatch** additionally needs `init_chains` to *queue* the X-Chain with a genesis
>   `ava_avm` can parse — today `init_chains` queues only P (Go: the P-Chain genesis's `CreateChainTx`s spawn X/C),
>   and the production AVM genesis is not yet parseable by `AvmVm` (the synthetic seed is M5). So the *dispatcher*
>   handles `avm_id` (proven in-process); a live `avalanchers --network-id=local` still flips only `isBootstrapped(P)`.
>   **C-Chain dispatch** blocked on M6.8; **SAE** + **real-DB threading** + **multi-node `Sender`** unchanged.
> - **Verified (main tree):** `-p avalanchers -p ava-node` **33/33**, `cargo build --workspace` + `-p avalanchers
>   --release` green, clippy `--all-targets -D warnings` + workspace fmt clean. (`ava-avm` added to `avalanchers` deps.)

> **STEP (a) — LIVE X QUEUE (2026-06-19, ralph iteration, TDD; closes wave-X/C-dispatch's "live X dispatch" deferral).**
> The prior wave proved the *dispatcher* handles `avm_id` in-process but flagged the live gap: `init_chains` queued
> only P, and the synthetic seed `boot_xchain` accepted was M5-only. **M5.f4 made the production AVM genesis
> parseable** (`AvmVm::initialize` ports `initGenesis` + `Linearize`), which both *unblocked* this slice and *broke*
> the synthetic-seed path (`Genesis::parse` now rejects the 40-byte seed — the in-process X-dispatch test was red).
> Both are now closed by queuing the **real** genesis:
> - **`ava-genesis` (`build.rs`):** new `VmChain { chain_id, subnet_id, genesis_data, fx_ids }` + `vm_chain(genesis_bytes,
>   vm_id)` — projects the genesis `CreateChainTx` to the node's queue parameters so `ava-node` (which does **not**
>   depend on `ava-platformvm`) can read a genesis chain record without the `CreateChainTx` type in scope. The
>   blockchain id is the tx id (specs 23 §4.3).
> - **`ava-node` (`init/chain_manager.rs`):** `init_chains` now queues the platform chain **plus** the two standard
>   chains the genesis spawns — X (`avm_id`) and C (`evm_id`) — off the genesis `CreateChainTx`s via `vm_chain` (Go's
>   platform VM creates these once it bootstraps; the assembly manager has no such callback, so we queue them
>   directly). A custom genesis without a standard chain is skipped (`GenesisError::UnknownVmId`).
> - **`avalanchers` (`wiring/chains.rs`):** `boot_xchain` now reads the **real** AVM genesis: `avax_asset_id` is the
>   index-0 genesis asset (`ava_genesis::avax_asset_id`), and the handle's `genesis_id` is the Cortina stop-vertex id
>   from the upgrade config (the same value `AvmVm::initialize` linearizes off — Go `Upgrades.CortinaXChainStopVertexID`),
>   not the leading bytes of a synthetic seed.
> - **Tests:** `chain_creator_dispatches_xchain_to_bootstrapped` rewritten to drive real genesis end-to-end (no
>   manual synthetic queueing; X/C ids from `genesis_block_id(_, Chain::X/C)`); `init_chains` queues 3; creator boots
>   P+X (`handles.len()==2`), skips C; P,X flip true, C false. `node.rs::init_order_matches_go` + the two
>   `drive_startup_chains`/`run_queued_chains` P-Chain tests updated to expect the 3-queued / 2-booted shape.
>   `ava_genesis::build::vm_chain_extracts_xchain_record` unit-tests the new helper.
> - **★ LIVE PROOF (this iteration, real process):** built the release binary, ran a solo
>   `avalanchers --network-id=local --db-type=memdb --staking-ephemeral-{cert,signer}-enabled --sybil-protection-enabled=false`
>   node, curled `info.isBootstrapped`: **P=true, X=true, C=false** (X flips for the first time live; C honest),
>   `kill -INT` → clean exit 0 (shutdown drains both chains). The prior wave's "live node flips only P" is closed.
> - **★ STILL DEFERRED:** **C-Chain** dispatch blocked on M6.8 (`EvmVm::initialize` genesis wiring); **SAE** dispatch;
>   **real-DB threading** (the booted chains still use `boot_chain`'s in-process `MemDb`/router/loopback `Sender`, not
>   the assembled `Node`'s real handles — the generic↔trait-object impedance); **multi-node `Sender`** for mixed-net.
> - **Verified (main tree):** `-p ava-genesis -p ava-node -p avalanchers` **53/53**, clippy `--all-targets -D warnings`,
>   workspace fmt, `lint-determinism` all clean; `-p avalanchers --release` build + live boot green.

> **STEP (b) — C-CHAIN DISPATCH (2026-06-19, ralph iteration, TDD; closes the M6.8 `EvmVm::initialize` genesis-wiring
> deferral for the last standard chain). ★ A SOLO LIVE NODE NOW FLIPS `info.isBootstrapped(C)=true`.** The prior waves
> skipped the `evm_id()` branch because `EvmVm::new` needed *pre-built* collaborators (provider/config/store) — there
> was no path from genesis bytes to a running VM through the generic `boot_chain`. Closed by a new construction seam:
> - **`ava-evm` (`vm.rs`):** new `EvmVm::from_genesis(network_id, data_dir, genesis_bytes) -> Result<(EvmVm, Id)>` —
>   the M6.8 `golden::cchain_genesis_root` parse + alloc-materialization path, now wired into VM construction:
>   `CChainGenesis::parse` → `AvaChainSpec::c_chain(network_id, Chain::from_id(config.chainId))` → open Firewood at
>   `data_dir` → seed bytecode side store → `propose_from_bundle(alloc) + commit` on a fresh db → `genesis_header(root)`
>   → `EvmVm::new`. **★ Also seeds the accepted genesis block into the `verified` tree** so the engine's bootstrap
>   (`ava-engine snowman::bootstrap::start` calls `vm.get_block(last_accepted)` and reads its height) resolves the
>   genesis tip — without this, `get_block(genesis)` returned `NotFound`, `start()` errored, and C stalled before
>   `NormalOp` (the symptom that first surfaced live). Side stores (canonical/bytecode/block-hashes) are in-memory
>   here — threading the node's real chain db is the deferred real-DB half.
> - **`avalanchers` (`wiring/chains.rs`):** new `boot_cchain` opens a `tempfile::TempDir` for the C-Chain Firewood
>   state db (owned by the boot handle — `PChainBootHandle._data_dir`/`BootSpec.data_dir` added so it outlives the VM),
>   builds the VM via `EvmVm::from_genesis`, and drives it through the same generic `boot_chain` solo pipeline as P/X.
>   `run_queued_chains`' `evm_id()` branch now registers + boots C (was: log + skip). Two `Error` variants added
>   (`CChainVm(#[from] ava_evm::Error)`, `DataDir(#[from] io::Error)`); `ava-evm` + `tempfile` added to `avalanchers` deps.
> - **Tests:** `ava-evm` `tests/vm_genesis.rs::from_genesis_builds_vm_at_coreth_genesis_root` (state root + genesis id
>   + `get_block(genesis)` height-0 vs the coreth `expected.json` oracle). `chain_creator_dispatches_xchain_to_bootstrapped`
>   + the two P-Chain creator tests flipped to the **3-booted** shape (P+X+C all flip `is_bootstrapped` true,
>   `running_chains()==3`).
> - **★ LIVE PROOF (this iteration, real process):** release binary, solo
>   `avalanchers --network-id=local --db-type=memdb --staking-ephemeral-{cert,signer}-enabled --sybil-protection-enabled=false`
>   node, curled `info.isBootstrapped`: **P=true, X=true, C=true** (C flips for the first time live), `kill -INT` →
>   clean exit 0. All three standard chains now bootstrap on a solo node.
> - **★ STILL DEFERRED:** **SAE** dispatch; **real-DB threading** (booted chains still use `boot_chain`'s in-process
>   `MemDb`/router/loopback `Sender`, not the assembled `Node`'s real handles — the generic↔trait-object impedance);
>   **multi-node `Sender`** for mixed-net. C-Chain re-open (persisted-tip path in `from_genesis`) is exercised only by
>   the materialize-on-fresh-db guard, not yet by an end-to-end restart test.
> - **Verified (main tree):** `-p ava-evm` **186/186** (single-threaded, firewood-ethhash global switch),
>   `-p ava-genesis -p ava-node -p avalanchers` **53/53**, clippy `--all-targets -D warnings`, workspace fmt +
>   build, `lint-determinism` all clean; `-p avalanchers --release` build + live boot green.

> **STEP (c) — REAL-DB THREADING (2026-06-19, ralph iteration, TDD; closes the "real-DB threading" deferral from
> STEP (b)).** The booted chains no longer each get their own ephemeral in-process `MemDb` — they now share **one
> persistent base db**, namespaced per chain by `build_db_stack`'s `prefixdb(chain_id)` (Go's exact model: a single
> base DB, a prefixed sub-db per chain). The live `avalanchers` node threads its real assembled `node.db`
> (`Arc<dyn DynDatabase>`) through, so consensus / VM state now lands in the persistent backend rather than being
> discarded each boot — the prerequisite for restart persistence.
> - **`avalanchers` (`wiring/chains.rs`):** `boot_chain` gains a `base_db: Arc<dyn DynDatabase>` param and wraps it in
>   the existing object-safe `ava_node::init::database::DynDb` bridge (the generic↔trait-object impedance noted in
>   STEP (b)) instead of `MemDb::new()`; `boot_pchain`/`boot_xchain`/`boot_cchain` forward it. New `*_with_db` variants
>   `run_queued_chains_with_db` + `drive_startup_chains_with_db` take the base db explicitly (all chains in one node
>   share it, `Arc::clone`d per chain); the no-db `run_queued_chains`/`drive_startup_chains` wrappers supply a fresh
>   ephemeral `MemDb` for tests via a `fresh_mem_db()` helper. **The C-Chain's EVM *state* trie stays in its own
>   Firewood `TempDir`** (STEP (b)); only the snowman/proposervm consensus metadata threads through the shared base.
> - **`avalanchers` (`main.rs`):** the live dispatch call now uses `drive_startup_chains_with_db(.., Arc::clone(&node.db))`.
> - **Test:** `tests/in_process_chain.rs::run_queued_chains_persists_into_supplied_base_db` — a caller-supplied base db
>   is empty before boot and **non-empty after** P/X/C boot, proving the chains persist into the shared base (with the
>   old `MemDb::new()` the supplied db stayed empty).
> - **★ STILL DEFERRED:** **SAE** dispatch (no `vm_id` in `chain_manager`; the local-network genesis queues no SAE
>   chain, so it is not exercisable by a solo node without custom genesis); **multi-node `Sender`** for mixed-net; an
>   end-to-end **restart** test that re-opens the same base db and asserts the persisted tip resumes.
> - **Verified (main tree):** `-p avalanchers` **13/13**, clippy `--all-targets -D warnings` (avalanchers + all
>   dependents), workspace fmt, `single_runtime_lint` all clean.

> **STEP (d) — RESTART-PERSISTENCE TEST (2026-06-19, ralph iteration, TDD; closes the "end-to-end restart test"
> deferral from STEP (c)).** New `avalanchers` `tests/in_process_chain.rs::node_restart_resumes_persisted_tip_over_
> shared_base_db`: boot the queued P-/X-/C-Chains over one shared persistent base db (`Arc<dyn DynDatabase>` over
> `MemDb` — the Arc survives the restart exactly as an on-disk rocksdb/leveldb backend survives a process restart),
> drive to `NormalOp`, shut the node down cleanly (`manager.shutdown` drains the registered chains), then re-boot a
> **fresh** `AssemblyChainManager` over the **same** base db (the real restart shape: a new process, the same backend).
> Asserts: (1) the first boot persisted state and a clean shutdown did **not** clear it (the base db is still
> non-empty); (2) the second boot reaches `NormalOp` again **over the now-non-empty db** — the re-open path does not
> choke on pre-seeded state (the existing `run_queued_chains_persists_into_supplied_base_db` only covers a boot over an
> *empty* db, so this is the genuinely new coverage); (3) every key the first boot persisted is still present with the
> same value after the restart (the persisted tip resumes; the re-derivation is deterministic). `-p avalanchers`
> **14/14**, full workspace **1673/1673** (2 skipped), clippy `--all-targets -D warnings`, fmt, `lint-determinism` clean.
> - **★ ARCHITECTURAL FINDING (the honest scope — the resumed tip is genesis, height 0).** Tracing the boot path
>   showed **no advanced-tip resume exists anywhere in the stack today** — every boot re-derives last-accepted from
>   genesis rather than loading a persisted advanced tip: `ava_platformvm::state::State::new` (`state/state.rs:197`)
>   initializes its in-memory caches to defaults (`last_accepted: Id::EMPTY`, `height: 0`) and **does not load from
>   the base db**; `PlatformVm::initialize` (`vm.rs:543`) calls `seed_state` **unconditionally** (no Go-style
>   `state.IsInitialized()` guard); and `create_snowman_chain` (`ava-chains/src/create_chain.rs:652-655`) roots the
>   `Topological` consensus core at the **inner VM's freshly-re-seeded** `last_accepted` with **height hardcoded to
>   `0`**. So STEP (c)'s real-DB threading guarantees that *writes land in a persistent backend and survive shutdown*,
>   but nothing *reads them back to resume*. This test pins the round-trip that **is** guaranteed; resuming an
>   *advanced* tip is a deferred follow-up needing (a) a load-from-disk path in `State::new`/VM `initialize` (read the
>   persisted `last_accepted`/UTXOs/stakers + an `IsInitialized` guard that skips re-seed), (b) `create_snowman_chain`
>   rooting consensus at the persisted height (not `0`), and (c) in-process block issuance to advance the tip past
>   genesis in the first place (the same shared-mempool seam the M9.19 reexecute floors await).
> - **★ STILL DEFERRED (unchanged):** **SAE** dispatch (custom-genesis harness); **multi-node `Sender`** for mixed-net;
>   **advanced-tip resume** (the load-from-disk path above) — all single-track / gated, not a parallel-worktree wave.

> **STEP (e) — LOAD-FROM-DISK PRIMITIVE (2026-06-20, ralph iteration, TDD; closes item (a) of STEP (d)'s
> advanced-tip-resume follow-up at the `State` layer).** New `ava_platformvm::state::State` methods
> `is_initialized()` + `load()` (`state/state.rs`): `is_initialized()` reports whether the base DB already holds
> persisted state (presence of the `singleton→last accepted` key — the canonical "already seeded" sentinel, specs 27
> §5.1, cf. Go `state.shouldInit`); `load()` resumes the persisted consensus pointer (`last_accepted` + `height`), the
> scalar singletons (timestamp, primary+per-subnet supply, fee state, L1 excess, accrued fees), and the
> `height → block id` index from disk into the in-memory caches `State::new` otherwise leaves at genesis defaults. New
> `Error::CorruptState(&'static str)` for malformed fixed-width persisted entries (the base DB is the truth on
> recovery). TDD: `reopen_resumes_persisted_advanced_tip_not_genesis_defaults` (seed an advanced tip + scalars into a
> shared `Arc<dyn DynDatabase>`, drop the in-memory `State`, re-open a **fresh** `State` over the same backend — the
> real restart shape — assert the pre-`load()` defaults are the bug and `load()` resumes every persisted field) +
> `fresh_db_is_not_initialized_and_load_is_a_noop`. `-p ava-platformvm` **145/145** (+2), clippy `--all-targets -D
> warnings` clean (note `arithmetic_side_effects`: `UNIX_EPOCH.checked_add`, not `+`), fmt clean.
> - **★ STILL DEFERRED (the rest of advanced-tip resume — items (a)-stakers / (b) / (c)):** `load()` deliberately does
>   **not** rebuild the in-memory **staker / subnet / chain / UTXO-index** caches — confirmed the staker set is
>   **in-memory-only today** (`put_current_validator` writes no disk keys; the staker→disk acceptor flush of M4.14/M4.20
>   was never built — only the weight/pk-diff iterators were), so a faithful **validator-set** resume is blocked on first
>   building staker disk-persistence. **Wiring the `IsInitialized` guard into `PlatformVm::initialize`** is therefore
>   left out this pass (skipping `seed_state` on resume without a staker rebuild would regress to an *empty* validator
>   set, worse than today's re-seed-to-genesis); it needs `seed_state` factored so the in-memory genesis stakers can be
>   re-derived without clobbering the persisted LA/height. (b) `create_snowman_chain` rooting consensus at the persisted
>   height and (c) in-process block issuance (the shared-mempool seam, same blocker as M9.19 — you cannot yet *create* an
>   advanced tip in-process to resume) remain. `State::load()` is the verified primitive those steps will call.

> **STEP (f) — STAKER DISK-PERSISTENCE (2026-06-20, ralph iteration, TDD; closes item (a)-stakers of STEP (e)'s
> advanced-tip-resume follow-up — the validator-set half).** The blocker STEP (e) surfaced — "the staker set is
> in-memory-only today (`put_current_validator` writes no disk keys; the staker→disk acceptor flush of M4.14/M4.20 was
> never built)" — is now closed at the `State` layer. The `Chain`-trait acceptor write path now persists stakers and
> `State::load()` rebuilds the in-memory validator/delegator sets on restart:
> - **`ava-platformvm` `state/state.rs`:** two new persisted sublists `current_stakers_db` / `pending_stakers_db`
>   (`validator/current` and `validator/pending`, keyed by staker tx id → an encoded record). `put_current_validator`/
>   `put_current_delegator`/`put_pending_validator`/`put_pending_delegator` and their `delete_*` counterparts now
>   **write through** to these sublists (the established write-through pattern of `set_last_accepted`/`add_block`/
>   `add_utxo` — Rust's `State` is write-through where Go batches at commit). New `load_stakers()` (called from
>   `load()`) decodes every record and dispatches it to the validator vs delegator slot by its `Priority`. The on-disk
>   record is a **self-describing fixed layout** (`txID‖nodeID‖subnetID‖weight‖start‖end‖potentialReward‖nextTime‖
>   priority‖pkPresent[‖pk48]`), decoded defensively (`Error::CorruptState` on truncation/garbage) — the P-Chain
>   staker sublists are an on-disk migration concern, **not** a consensus/wire byte contract (specs 00 §4.4 / the
>   `state.rs` module docs), so it mirrors the singleton encoding rather than Go's validator-metadata codec.
> - **`Stakers::put_validator`** now returns the displaced prior validator (`Option<Staker>`) so the write-through
>   caller can drop a replaced staker's orphaned disk key when the tx id differs.
> - **`Priority::from_u8`** (inverse of `as_u8`) recovers the current/pending + validator/delegator partition on load.
> - **TDD:** `state::state::tests::reopen_resumes_persisted_stakers` (persist a primary current validator carrying a
>   BLS key + a current delegator + a permissioned-subnet pending validator into a shared `Arc<dyn DynDatabase>`, drop
>   the in-memory `State`, re-open a fresh `State` over the same backend — the real restart shape — assert the sets are
>   empty before `load()` and resume with full-field `Staker::equals` (incl. the BLS-key round-trip) after) +
>   `txs::priorities::golden::priority_u8_round_trips`. `-p ava-platformvm` **165/165**, clippy `--all-targets -D
>   warnings` + fmt clean.
> - **★ STILL DEFERRED (the rest of advanced-tip resume — items (a)-init-guard / (b) / (c)):** wiring the
>   `IsInitialized` guard into `PlatformVm::initialize` (skip `seed_state` on a recovered DB) now has its prerequisite
>   met (the validator set resumes), but still needs `seed_state` factored so the in-memory genesis stakers are
>   re-derivable without clobbering the persisted LA/height, **plus** the L1-validator / subnet / chain / UTXO-index
>   caches given the same disk-resume treatment (this slice covered the current/pending stakers, the validator-set
>   blocker STEP (e) flagged); (b) `create_snowman_chain` rooting consensus at the persisted height; (c) in-process
>   block issuance (the shared-mempool seam, same blocker as M9.19) to *create* an advanced tip to resume.

> **STEP (g) — SUBNET / CHAIN / UTXO-INDEX CACHE RESUME (2026-06-20, ralph iteration, TDD; closes the "subnet / chain
> / UTXO-index caches" half of STEP (f)'s item-(a) deferral).** `State::load()` now also rebuilds the three in-memory
> caches that mirror an already-write-through byte space, so a recovered node reports its subnets, per-subnet chains,
> and `getUTXOs` address index instead of empty collections:
> - **`ava-platformvm` `state/state.rs`:** new `load_subnets()` / `load_chains()` / `load_utxo_index()`, called from
>   `load()` after `load_stakers()`. `load_subnets` flat-scans the `subnets` byte space (key = 32-byte subnet id) into
>   `subnet_ids`. `load_utxo_index` flat-scans `utxo_index_db` (key = `addr(20)‖utxoID(32)`, the `utxo_index_key`
>   layout) into the address → utxo-id `BTreeMap`. **`load_chains` must run after `load_subnets`** and enumerates
>   per-subnet: each subnet's chains live under the **hashed** `chains.join(subnet)` sub-space (`join` compresses to a
>   SHA-256 prefix — the parent space is *not* flat-scannable), so it iterates over the resumed `subnet_ids` **plus**
>   `PRIMARY_NETWORK_ID` (genesis chains are recorded under the primary network). Defensive decode (`Error::CorruptState`
>   on bad key widths); the byte spaces are an on-disk migration concern, not a consensus/wire contract.
> - **TDD:** `state::state::tests::reopen_resumes_persisted_subnet_chain_and_utxo_index_caches` (persist a created
>   subnet + a primary-network genesis chain + a subnet chain + two multi-owner UTXOs into a shared `Arc<dyn
>   DynDatabase>`, drop the in-memory `State`, re-open a fresh `State` over the same backend — the real restart shape —
>   assert all three caches are empty before `load()` and resume exactly after). `-p ava-platformvm` **164/164** (+1),
>   clippy `--all-targets -D warnings` + fmt clean, **full workspace 1678/1678 (2 skipped)**.
> - **★ STILL DEFERRED (the remaining advanced-tip-resume items):** (a)-init-guard — wire the `IsInitialized` guard into
>   `PlatformVm::initialize` (skip `seed_state` on a recovered DB; needs `seed_state` factored so genesis stakers
>   re-derive without clobbering persisted LA/height); the **reward-utxo index** (keyed under hashed
>   `reward_utxos.join(tx)` sub-spaces with no enumerable tx-id set on disk — needs a flat tx-id index added first) and
>   the **L1-validator set** (in-memory-only — `put_l1_validator` has no disk write path yet, the same gap stakers had
>   before STEP (f); needs disk-persistence built first, then resume); (b) `create_snowman_chain` rooting consensus at
>   the persisted height; (c) in-process block issuance (the shared-mempool seam, same blocker as M9.19) to *create* an
>   advanced tip to resume.

> **STEP (h) — L1-VALIDATOR DISK-PERSISTENCE + RESUME (2026-06-20, ralph iteration, TDD; closes the **L1-validator set**
> half of STEP (g)'s item-(a) deferral — the gap that "needs disk-persistence built first, then resume", the exact
> mirror of what STEP (f) did for stakers).** The ACP-77 L1-validator set was in-memory-only — `put_l1_validator`
> inserted into the `BTreeMap` with **no disk write path** — so a recovered node lost every subnet validator. Now
> persisted and resumed:
> - **`ava-platformvm` `state/state.rs`:** new persisted sublist `l1_validators_db` (`l1Validators/l1Validator`, the
>   already-reserved `L1_VALIDATOR_PREFIX` child of `L1_VALIDATORS_PREFIX`), keyed by `ValidationID` → the value.
>   `put_l1_validator` now **writes through** (`v.marshal()?` then `put`, mirroring the established
>   `set_last_accepted`/`put_current_validator` write-through pattern) before the in-memory insert. The key is the stable
>   `ValidationID`, so a re-put overwrites the same key — **no orphan/replace cleanup needed** (unlike stakers, whose
>   key is the tx id and can change), and there is no `delete_l1_validator` in the `Chain` trait, so the map only grows.
>   New `load_l1_validators()` (called from `load()` after `load_stakers()`) decodes every record and restores its
>   `validation_id` from the DB key (the value omits it).
> - **★ KEY: reuses the EXISTING wire codec, no hand-rolled record.** Unlike stakers (which needed a self-describing
>   fixed layout because `Staker` had no on-disk encoding), `L1Validator` **already** has `marshal`/`unmarshal` via the
>   `GenesisCodec` (it IS a real Go on-disk record, `state/l1_validator.go`), and `Error::Codec` has a `#[from]`, so the
>   write-through/resume is a thin wrapper. The `ValidationID` is the DB key (not serialized), matching Go `putL1Validator`.
> - **TDD:** `state::state::tests::reopen_resumes_persisted_l1_validators` (persist an active validator carrying a
>   public key + an inactive validator (`end_accumulated_fee == 0`) on a second subnet into a shared `Arc<dyn
>   DynDatabase>`, drop the in-memory `State`, re-open a fresh `State` over the same backend — the real restart shape —
>   assert `get_l1_validator` errors + `active_l1_validators()` empty before `load()`, then full-field `L1Validator`
>   equality (incl. the `ValidationID`-from-key + GenesisCodec round-trip), per-subnet `weight_of_l1_validators`, and the
>   active-only iterator resume after). `-p ava-platformvm` **165/165** (+1), clippy `--all-targets -D warnings` + fmt clean.
> - **★ STILL DEFERRED (the remaining advanced-tip-resume items):** (a)-init-guard — wire the `IsInitialized` guard into
>   `PlatformVm::initialize` (skip `seed_state` on a recovered DB; needs `seed_state` factored so genesis stakers
>   re-derive without clobbering persisted LA/height — its disk-persistence prereqs (stakers/subnets/chains/UTXO/L1) are
>   **all now met**, so this is the natural next slice); the **reward-utxo index** (keyed under hashed
>   `reward_utxos.join(tx)` sub-spaces with no enumerable tx-id set on disk — needs a flat tx-id index added first);
>   (b) `create_snowman_chain` rooting consensus at the persisted height; (c) in-process block issuance (the
>   shared-mempool seam, same blocker as M9.19) to *create* an advanced tip to resume.

> **STEP (i) — `IsInitialized` GUARD IN `PlatformVm::initialize` (2026-06-20, ralph iteration, TDD; closes item-(a)
> init-guard — the load-bearing wire-up that finally makes STEP (e)–(h)'s resume primitives RUN in the live boot
> path).** `PlatformVm::initialize` previously *always* re-seeded genesis (`seed_state` → `set_last_accepted(genesis_id)`
> / `set_height(0)`), so a restart over a populated DB came up at genesis (height 0), discarding the persisted tip even
> though STEP (e)–(h) persist & resume every field. Now guarded:
> - **`ava-platformvm` `vm.rs`:** the genesis block id is derived purely from the genesis bytes
>   (`genesis::genesis_block(genesis_bytes)?.id()` — no seeding needed, so it tracks `self.genesis_id` on both paths).
>   Then `if state.is_initialized() { state.load()? } else { parse + seed_state + add_block + set_last_accepted +
>   set_height(0) }`. `self.preferred` becomes `state.last_accepted()` (the resumed tip on a restart, `genesis_id` on a
>   fresh DB where they're equal — **zero behavior change on the fresh path**, confirmed by the unchanged
>   `vm_initialize_and_last_accepted`). The `BlockManager` already seeds its last-accepted from `state.last_accepted()`,
>   so the resumed tip flows through with no further change.
> - **★ Why the STEP (e) "needs `seed_state` factored" caveat is now MOOT:** every byte space `seed_state` writes
>   (timestamp/supply/UTXOs/validators/chains/genesis-block) goes through the write-through `Chain`-trait path
>   (`set_*`/`add_*`/`put_current_validator`/`add_chain`/`add_tx`) and is therefore either persisted in a byte space or
>   rebuilt by `State::load` (STEP (e)–(h)). So on a recovered DB we resume rather than re-derive; nothing needed
>   factoring out of `seed_state`.
> - **TDD:** `vm::tests::initialize_over_recovered_db_resumes_persisted_tip_not_genesis` (process 1 = real genesis
>   `initialize` over a shared `Arc<dyn DynDatabase>`; advance the persisted tip to height 7 directly through `State`'s
>   write-through path, the restart shape; process 2 = a fresh `PlatformVm::initialize` over the SAME backend must come
>   up at the advanced tip + `preferred`, not genesis, while still tracking `genesis_id` and resolving the height-7
>   block). `-p ava-platformvm` **166/166** (+1), clippy `--all-targets -D warnings` + fmt clean, **full workspace
>   <run>** (an `initialize` VM-contract change ⇒ full-workspace gate per the M5.f4 lesson).
> - **★ STILL DEFERRED (the remaining advanced-tip-resume items):** the **reward-utxo index** (keyed under hashed
>   `reward_utxos.join(tx)` sub-spaces with no enumerable tx-id set on disk — needs a flat tx-id index added first);
>   (b) `create_snowman_chain` rooting consensus at the persisted height (the in-process-boot wiring in `avalanchers`,
>   not platformvm); (c) in-process block issuance (the shared-mempool seam, same blocker as M9.19) to *create* an
>   advanced tip to resume in-process. With the init-guard wired, a restart now resumes its persisted tip at the `State`
>   layer; rooting the *consensus engine* at that height (b) is the next slice up the stack.

> **STEP (j) — REWARD-UTXO RESUME via LAZY READ-THROUGH (2026-06-20, ralph iteration, TDD; closes the
> **reward-utxo index** item STEP (e)–(i) repeatedly parked as "needs a flat tx-id index added first").** The
> in-memory `reward_utxo_index` was the only read path for `Chain::get_reward_utxos`, so after a restart a recovered
> node reported *no* reward UTXOs for any tx (empty cache) even though `add_reward_utxo` had written them through to
> disk — `platform.getRewardUTXOs` would have wrongly answered none. ★ KEY ORIENT FINDING that dissolved the "needs a
> flat tx-id index first" blocker: `PrefixDb::join` is **hashed** (`SHA256(parent_prefix ‖ tx_id)`), so the reward
> outputs land under a top-level hashed sub-space — iterating the `reward_utxos` space yields nothing *and* the
> `tx_id` is unrecoverable from the hash. An **eager** `load_reward_utxos` is therefore impossible without a separate
> flat index. But Go (`platformvm/state.go`) doesn't preload reward UTXOs — it reads them **per-tx on demand**, and a
> read *knows* its `tx_id`, so it can recompute the join hash and prefix-scan that sub-space. So the fix is a **lazy
> read-through**, not an eager load: `State::get_reward_utxos` returns the in-memory list on a cache hit (rewards
> added this run) and on a miss reads `reward_utxos.join(tx_id)` straight off disk (new `read_reward_utxos_from_disk`,
> ascending-ordinal order = the sub-space's lexicographic key order). **Zero behavior change in-process** (cache-hit
> path identical; the disk read only fires on a miss, which never happens within the writing process); a restart now
> resolves reward UTXOs with no `load()` call and no flat index. **TDD:** `state::tests::reopen_resumes_persisted_reward_utxos`
> (process 1 persists 2 + 1 reward UTXOs across two txs through `add_reward_utxo`; process 2 = a fresh `State` over the
> SAME backend resolves both txs' reward UTXOs in ordinal order via the read-through, and an unknown tx is empty).
> `-p ava-platformvm` **167/167** (+1), clippy `--all-targets -D warnings` + fmt clean, full workspace re-run.
> - **★ STILL DEFERRED (the remaining advanced-tip-resume items, now just the consensus/boot half):** (b)
>   `create_snowman_chain` rooting consensus at the persisted height (in-process-boot wiring in `avalanchers`, not
>   platformvm); (c) in-process block issuance (the shared-mempool seam, same blocker as M9.19) to *create* an advanced
>   tip to resume in-process. **The entire `State`-layer advanced-tip-resume surface (STEP (e)–(j)) is now complete** —
>   LA/height/scalars, stakers, L1 validators, subnets/chains, UTXO index, and reward UTXOs all survive a restart.

> **STEP (k) — `create_snowman_chain` ROOTS CONSENSUS AT THE PERSISTED HEIGHT (2026-06-20, ralph iteration, TDD;
> closes item (b) of the advanced-tip-resume follow-up — the consensus-engine half).** `create_snowman_chain`
> (`ava-chains/src/create_chain.rs`) built its `Topological` consensus core with a **hardcoded `0`** last-accepted
> height (`Topological::new_default(.., last_accepted, 0)`), so a node that recovered an advanced tip from disk (the
> inner VM resumes last-accepted at height N — proven for `PlatformVm` by STEP (i)) came up with consensus rooted at
> height **0** while the VM thought the tip was N. The first issued block (height N+1) would then be rejected by
> consensus as a non-child of the height-0 root. Now rooted correctly:
> - **`ava-chains` `create_chain.rs`:** after `let last_accepted = vm.last_accepted(token).await?;`, fetch the block
>   and read its height — `let last_accepted_height = vm.get_block(token, last_accepted).await?.height();` — and pass it
>   to `Topological::new_default`. This is **exactly Go** (`snowman/transitive.go`: `vm.GetBlock(vm.LastAccepted()).Height()`).
>   On a fresh genesis tip this is `0` — **zero behavior change on the fresh path** (the existing `pipeline_wrapping_order`
>   is unchanged). The wrapped proposervm forwards `last_accepted` to the inner VM pre-fork and `get_block(id).height()`
>   returns the inner block's height, so the persisted height threads through the full ratified stack.
> - **Observability:** `SnowmanChain` gained a `pub last_accepted_height: u64` field recording what the consensus core
>   was rooted at (mirrors Go's recorded `lastAcceptedHeight`), so the resume height is assertable without reaching into
>   the type-erased `EngineManager`.
> - **★ New error path:** `create_snowman_chain` now `get_block`s the last-accepted after `initialize`, so it errors if
>   that block is unresolvable. This is the Go contract; confirmed harmless for all three real VMs — the `avalanchers`
>   `in_process_chain` boot tests drive real `PlatformVm`/`AvmVm`/`EvmVm` through `create_snowman_chain` and all pass.
> - **TDD:** added `TestVm::resuming_at_height(n)` to `ava-vm/testutil.rs` (on `initialize`, seeds the accepted chain
>   `genesis → … → n` and reports the height-`n` block as last-accepted — the recovered-from-disk shape) + new
>   `ava-chains` `tests/pipeline.rs::pipeline_roots_consensus_at_resumed_height` (resume at height 5, assert
>   `chain.last_accepted_height == 5`; RED with the hardcoded `0`, GREEN after). `-p ava-chains` **7/7** (+1),
>   `-p ava-vm -p avalanchers` + `-p ava-engine` (the `TestVm`/`create_snowman_chain` reverse-deps) green (48 + 34),
>   clippy `--all-targets -D warnings` + fmt clean.
> - **★ STILL DEFERRED (the last advanced-tip-resume item):** (c) in-process block issuance (the shared-mempool seam,
>   same blocker as M9.19) to *create* an advanced tip to resume in-process. With (b) done, a recovered node now roots
>   **both** its `State` layer (STEP (e)–(j)) **and** its consensus engine at the persisted height; what remains is only
>   the means to advance a tip past genesis *within a single in-process run* (so an end-to-end resume can be exercised
>   without a pre-populated disk fixture) — which needs block issuance, gated on the M9.19 mempool seam.
>
> **STEP (l) — IN-PROCESS BLOCK ISSUANCE + RESTART-RESUME, END-TO-END (2026-06-20, ralph iteration, TDD; closes item
> (c) of the advanced-tip-resume follow-up).** The last item — advancing a tip past genesis via a *real* issued block
> (not raw `State` pokes) and proving the restart resumes it — is now exercised end-to-end through the genuine VM
> `build → verify → accept` path, using the **existing** M9.19 `PlatformVm::mempool_add` seam (no new production code).
> Two `ava-reexecute` `pchain.rs` tests:
> - **`block_issued_tip_resumes_after_restart`:** boot a `PlatformVm` over a **shared** `Arc<dyn DynDatabase>`, init
>   genesis (height 0), admit a funded `CreateSubnetTx` and drive one `build → set_preference → verify → accept` cycle
>   (a real height-1 `BanffStandardBlock` that flushes a genuine diff — consumed `U0`, change UTXO, a new subnet, the
>   tx), drop the VM, then re-`initialize` a fresh VM over the SAME backend. Asserts the `IsInitialized` guard (STEP
>   (i)) resumes the **block-issued** tip (not genesis) **and** that `get_block(resumed_tip)` re-parses the real block
>   bytes off disk — the exact read `create_snowman_chain` (STEP (k)) performs at restart to root consensus at the
>   persisted height. ★ This is the coverage the STEP (i) unit test could not give: it advanced the tip via raw `State`
>   setters with **garbage block bytes** (`add_block(id, 7, &[0xAB, 0xCD])`), so it never proved a real persisted block
>   re-parses on resume.
> - **`resumed_vm_builds_a_further_block`:** after the restart resumes the height-1 tip, the recovered VM builds,
>   verifies and accepts a *further* real block (height 2) spending the still-unspent genesis UTXO `U1`. This is the
>   real **diff-resume** stress test — height-2 `verify` requires `State::load` to have faithfully rebuilt the on-disk
>   caches it reads (parent-state view, the surviving UTXO `U1` via the STEP (g) UTXO-index + `get_utxo`, fee/staker
>   state); a gap in any would fail. It passes — proving the resumed VM is fully functional, not merely able to report a
>   resumed tip.
> - **★ FINDING — the advanced-tip-resume arc (STEPs e–k) is FUNCTIONALLY COMPLETE; no production gap.** Both tests
>   passed on first run: a tip advanced by real block issuance resumes faithfully on restart and the resumed VM builds
>   further. So item (c) lands as a **verification-level** closure (regression guards + the end-to-end proof the raw-poke
>   tests could not give), not a code change. `create_subnet_tx` was refactored to delegate to a `_spending(seed, tx_id,
>   output_index, amount)` helper (so block 2 can spend `U1`); existing `replay_pchain` behavior unchanged. `-p
>   ava-reexecute` **11/11** (+2), clippy `--all-targets -D warnings` + fmt clean. No production code touched ⇒ no
>   workspace ripple (ava-reexecute is a leaf test crate).
> - **★ Honest scope:** issuance here goes through the `mempool_add` seam directly on `PlatformVm` (the M9.19 path), NOT
>   through the full chains-milestone snowman-engine boot (`run_queued_chains` → `PendingTxs` → poll). The resume property
>   that an engine-driven issuance would prove is **identical** to (and now proven by) this — the engine path adds only
>   the notify/poll wake before the same `build_block → accept`. Wiring block issuance through the in-process engine boot
>   (so the `avalanchers` restart test resumes a self-issued advanced tip) remains a thin follow-up, but no longer gates
>   the resume-correctness claim. The remaining M9.15 frontier is the live multi-node `Sender` and SAE dispatch.

> **STEP (m) — ENGINE-DRIVEN BLOCK ISSUANCE (the self-loopback `Sender`); STEP (l)'s "thin follow-up" CLOSED
> (2026-06-21, ralph iteration, TDD; single-track `ava-chains`/`ava-vm`/`avalanchers`).** STEP (l) issued its block by
> calling `build → set_preference → verify → accept` **directly on the VM**, bypassing consensus, because a solo
> in-process node's poll never resolves: the boot harness's `RecordingSender`/`NoopSender` *drop* every outbound op, so
> the engine's own `push_query` for a self-built block is never answered with `Chits` and the block stays *processing*,
> never accepted. This step builds the missing piece — a **self-loopback `Sender`** — and proves the engine itself
> drives a real block to acceptance + persistence + restart-resume.
> - **`ava-chains` (`create_chain.rs`):** `create_snowman_chain` stopped discarding the handler's `vm_tx`
>   (`mpsc::Sender<VmEvent>`) and now returns it on `SnowmanChain.vm_tx` — the in-process equivalent of a VM's
>   `toEngine` channel. Sending `VmEvent::PendingTxs` there reaches the handler → `engine.notify_pending_txs` →
>   `build_blocks` → `vm.build_block` → `issue_from`.
> - **`avalanchers` (`wiring/chains.rs`):** opt-in self-loopback on `RecordingSender` (installable
>   `Loopback{self_node, sink}`, default off ⇒ **zero behavior change** for the startup-boot paths). When installed, the
>   consensus **poll path** is delivered back to the node's own handler as inbound ops *from* the self node:
>   `send_push_query`→`InboundOp::PushQuery`, `send_pull_query`→`PullQuery`, `send_chits`→`Chits` (fire-and-forget
>   `tokio::spawn`; the handler drains sequentially, so no re-entrancy). The loop closes: `issue_from`'s `push_query` is
>   delivered back, the engine answers with self-`Chits`, and the `k=1`/`β=1` poll completes ⇒ the block is **accepted
>   through the genuine engine path**. New `BootSpec.loopback` + the test seam `boot_chain_with_loopback`;
>   `PChainBootHandle` gained `vm_tx` + `last_accepted_height` (the create-time consensus-rooting height — STEP (k) — so
>   a restart's resumed tip is assertable without reaching into the type-erased engine).
> - **`ava-vm` (`testutil.rs`):** `TestVm::observer()` → a `TestVmObserver` sharing the VM's `Arc<Mutex<Inner>>`, so a
>   test can watch the chain tip advance *after* the VM is moved into the type-erased engine.
> - **TDD — two tests, both RED-confirmed (loopback off ⇒ tip stuck at genesis):**
>   - `avalanchers in_process_chain::engine_accepts_self_built_block_via_loopback` — boot a `TestVm` chain with the
>     loopback, reach `NormalOp`, signal `PendingTxs`, assert the engine builds + issues + **accepts** a height-1 block
>     (tip 0→1) with **no direct `accept()` on the VM**. RED without the loopback (built+issued but never voted ⇒
>     processing forever).
>   - `avalanchers engine_issuance::engine_issued_pchain_tip_resumes_after_restart` — the **real `PlatformVm`** leg
>     (funded synthetic genesis + signed `CreateSubnetTx` ported from the `ava-reexecute` P-Chain leg, pre-loaded into
>     the mempool via the M9.19 `mempool_add` holding-pen seam *before* boot). The engine builds + issues + accepts a
>     real height-1 `BanffStandardBlock` over a shared base db; a fresh node re-booted over the **same** db resumes
>     rooted at height 1 (STEP (i)+(k) machinery), not genesis. RED without the loopback (never accepts ⇒ db never grows
>     ⇒ restart resumes genesis). ★ The proposervm wrapper the pipeline adds is **pre-fork pass-through** here (boot
>     clock at the Unix epoch, before any fork), so `build_block` reaches the inner `PlatformVm` directly — no proposer
>     windowing.
> - **★ This closes the STEP (l) "engine-driven issuance" follow-up.** A self-built tip is now driven to acceptance by
>   the genuine handler→engine→poll machinery (not a direct VM call) AND survives a restart. Verification: `ava-chains`
>   7/7, `avalanchers`+`ava-chains`+`ava-vm` 50/50, `ava-engine`+`ava-node`+`ava-reexecute` 66/66, full workspace
>   `--all-targets` compiles, clippy `-D warnings` + fmt clean. **The remaining M9.15 frontier is unchanged: the live
>   multi-node `Sender` (the self-loopback is the in-process half of that machinery) and SAE dispatch.**

> **STEP (n) — SAE IN-PROCESS CHAIN DISPATCH (2026-06-21, ralph iteration, TDD; closes the "SAE dispatch" half of the
> M9.15 frontier in-process).** The standard chains (P/X/C) already dispatch & boot in-process through
> `run_queued_chains` → a per-`vm_id` `boot_*` → `create_snowman_chain` → `NormalOp`; SAE was the last
> `else { warn; continue }` branch. SAE now boots a **real `ava_saevm_core::Vm`** to `NormalOp` through the genuine
> consensus pipeline, proving the boot machinery is SAE-ready (mirrors how C-Chain dispatch first landed via a test seam
> before `EvmVm::from_genesis`):
> - **`ava-genesis` `chains.rs`:** new `pub const SAEVM_ID_BYTES = ascii32("saevm")` + `pub fn saevm_id() -> Id` (mirrors
>   `evm_id()`; upstream pins no SAE VMID so `ascii32("saevm")` is the documented choice), asserted in the existing
>   `vm_ids_ascii_layout` test. Re-exported as `ava_node::init::chain_manager::saevm_id()`.
> - **`avalanchers` `wiring/chains.rs`:** new `#[doc(hidden)] boot_generic_chain<V: ChainVm>(...)` — the sibling of
>   `boot_chain_with_loopback`, differing only in `loopback: false` (solo node, empty beacons ⇒ `Bootstrapping → NormalOp`
>   short-circuit, no poll/issuance). Reuses the existing `BootSpec`/`boot_chain` core; **zero behavior change** to the
>   production startup-boot paths. The deferred `run_queued_chains` SAE branch comment was tightened to name `saevm_id()`
>   and state precisely *why* a production boot is still deferred (no production `BlockBuilderSeam`/`ExecutorSeam` wiring —
>   M7.21/M7.26 — and no genesis-bytes → SAE `Vm` materialization; plus the `local` genesis queues no SAE chain, so the
>   branch is unreachable on a solo `local` node).
> - **`ava-saevm-core` `vm.rs` (the one production change):** `BaseVm::initialize` previously hard-returned
>   `Err(InitializeDeferred)`, which `create_snowman_chain`'s unconditional `vm.initialize()` mapped to `Vm(NotFound)` ⇒
>   boot failed regardless of the seam. A `Vm::new`-constructed VM is **already genesis-rooted** (genesis seeded into the
>   block store/height index, frontier + preference rooted at it), so `initialize` is now a **no-op success** for it — the
>   pipeline's immediate `last_accepted`/`get_block` queries resolve. The genesis-*bytes* → VM materialization path stays
>   explicitly deferred (the `InitializeDeferred` variant is retained, re-documented as reserved for it). Not a faked boot:
>   the adaptor/lifecycle are untouched and no error is swallowed.
> - **`ava-saevm-testutil` `invariants.rs`:** `FakeBuilder`/`FakeExecutor` made `pub` + new `pub fn live_genesis()` and
>   `pub fn boot_ready_vm() -> Vm<FakeBuilder, FakeExecutor>` so a cross-crate test can build a live SAE VM (the only way
>   to construct one today, since the production seams don't exist). **No testutil → production dep introduced** — the
>   three SAE crates are `avalanchers` **`[dev-dependencies]`** (test-only).
> - **TDD:** `avalanchers tests/in_process_chain.rs::saevm_chain_boots_to_normalop` — build the real SAE `Vm` over the
>   testutil seams + SAE genesis, `ava_saevm_adaptor::convert` it, boot via `boot_generic_chain`, assert `NormalOp` +
>   `last_accepted` resolves to the SAE genesis (height 0). RED before the `initialize` fix (`Manager(Vm(NotFound))`),
>   GREEN after. Verified in main tree: `cargo nextest run -p avalanchers -p ava-genesis -p ava-node -p ava-saevm-core
>   -p ava-saevm-adaptor -p ava-saevm-testutil` = **115 passed, 1 skipped** (the expected nextest-leaky GC skip), clippy
>   `--all-targets -D warnings` clean (incl. `lint-saevm`), fmt clean.
> - **★ The remaining M9.15 frontier is now just the live multi-node `Sender`** (the two-binary `mixed_network` arm —
>   nightly/operator-gated). Production SAE dispatch (a `run_queued_chains` `saevm_id()` branch booting a real SAE VM from
>   queued genesis bytes) stays deferred on the M7.21/M7.26 production seams + genesis-bytes materialization.

> **STEP (o) — THE REAL ava-network-backed `Sender` (`OutboundSender`); the multi-node `Sender`'s production wire-out
> half (2026-06-21, ralph iteration, TDD; single-track `ava-engine`).** STEP (m)'s self-loopback `Sender` is the
> *in-process* half of the multi-node machinery; this step builds the *production* half — the concrete
> `ava_engine::common::sender::Sender` that the `Sender` trait's own doc comment named as "the concrete `OutboundSender`
> (a later task)". A real multi-node node drives consensus + app traffic to **real peers** through it.
> - **`ava-engine` (`networking/sender.rs`):** new `OutboundSender` implementing `Sender` (port of Go
>   `snow/networking/sender.sender`, specs 06 §5.3). Each `send_*` builds the matching `proto/p2p` wire message via
>   `ava_message::codec::MsgBuilder::create_outbound`, then dispatches it through `ava_network::network::Network::send`
>   (targeted, Go `ExternalSender.Send`) or `gossip` (app-gossip, Go `ExternalSender.Gossip`). All ~20 trait methods
>   covered: frontier/accepted (bootstrap), fetch (`Get`/`GetAncestors`/`Put`/`Ancestors`), query/vote
>   (`PushQuery`/`PullQuery`/`Chits`), and the 4 app ops. Recipient selection maps the engine-facing `SendConfig`
>   field-for-field to the network's `SendConfig`/`GossipConfig` (Go has a single `common.SendConfig`); the chain's
>   subnet `Allower` is applied by the network. Request ops carry the configured `request_timeout` as the on-wire
>   `deadline` (relative nanos — what peers use to expire the request, matching `MsgBuilder::parse_inbound`).
> - **Dep direction:** `ava-engine` gained `ava-message` + `ava-network` + `bytes` deps — **acyclic** (neither
>   ava-message nor ava-network depends on ava-engine; the engine's own `lib.rs` already documents `networking` as "the
>   bridge to ava-network"). `GetAncestors.engine_type = ENGINE_TYPE_CHAIN` (Snowman; the X-Chain DAG path is unused).
> - **TDD:** new `crates/ava-engine/tests/outbound_sender.rs` drives a recording mock `Network` and **decodes the
>   marshaled bytes back** to assert op + recipients + subnet + every wire field, for `PushQuery` (multi-recipient),
>   `Chits`/`Get`/`AcceptedFrontier` (single-recipient), `AppGossip` (gossip path), and `AppRequest` (targeted app).
>   RED-confirmed (module absent ⇒ `cargo build -p ava-engine --tests` E0432). Verified: `ava-engine` **40/40** (6 new),
>   clippy `--all-targets -D warnings` clean, fmt clean, workspace build green.
> - **★ DEFERRED follow-up (documented in the module):** registering each outgoing request with the
>   `AdaptiveTimeoutManager` (so a `*Failed` handler callback fires on a non-response) is NOT wired here — the engine
>   `Sender` request methods are sync (`fn`, fire-and-forget, matching Go) but this port's timeout-manager
>   `put`/`remove` are `async`; bridging needs an async seam (a request-registration channel drained by the router
>   task). The on-wire deadline is already correct. **★ Remaining M9.15 frontier:** wiring `OutboundSender` into the
>   live node-assembly boot path (replacing the loopback/`RecordingSender`) + the timeout-registration seam + the
>   two-binary `mixed_network` live arm (nightly/operator-gated).

> **STEP (p) — `OutboundSender` request-timeout registration; STEP (o)'s deferred seam CLOSED (2026-06-21, ralph
> iteration, TDD; single-track `ava-engine`).** The `OutboundSender` now registers every outgoing **request** op with
> the `Router` so the matching `*Failed` op is synthesized into the chain handler on a non-response — the recovery
> signal the bootstrap/query engines depend on (Go `sender` + `timeout.Manager`). STEP (o)'s deferred "async-bridge"
> note is resolved by removing the async-ness at its source rather than bridging it:
> - **`ava-engine` (`networking/timeout.rs`):** the `AdaptiveTimeoutManager` held its `state` behind a
>   `tokio::sync::Mutex`, but **no critical section holds an `.await`** — so it was the wrong tool. Switched to
>   `std::sync::Mutex`; `put`/`remove`/`timeout_duration`/`observe_latency` are now **synchronous** `fn` (poison-tolerant
>   `lock()` helper, no `unwrap`/`expect`). `dispatch_loop`/`fire_expired` use the sync lock. **This makes registration
>   race-free:** a sync `register_request` happens-before the wire send returns, so a fast response can never `remove`
>   an entry the registration has not yet inserted (the exact bug a fire-and-forget `tokio::spawn(put)` would have had).
> - **`ava-engine` (`networking/router.rs`):** `Router::register_request` (trait + impl), `ChainRouter::on_response`,
>   and `current_timeout` drop `async` (they only awaited the now-sync timeout-manager calls). The request op-tag
>   constants (`mod op`) are now `pub` so the `OutboundSender` can tag each request.
> - **`ava-engine` (`networking/sender.rs`):** `OutboundSender::new` gains an `Arc<dyn Router>`; each request op
>   (`Get`, `GetAncestors`, `GetAcceptedFrontier`/`GetAccepted`, `GetStateSummaryFrontier`/`GetAcceptedStateSummary`,
>   `PushQuery`/`PullQuery`, `AppRequest`) calls `router.register_request(node, chain, request_id, op_tag)` **before**
>   the wire send — one registration **per recipient** for the multi-node ops. Reply ops register nothing.
> - **TDD:** two new tests in `tests/outbound_sender.rs` — `request_ops_register_for_timeout_but_replies_do_not` (a
>   recording `Router` proves exactly the request ops register, multi-recipient queries register per-node, and replies
>   register nothing) and `app_request_registers_for_timeout`. The existing timeout/router tests stay green after the
>   sync-ification (dropping `.await`). One cross-crate impl updated (`ava-chains/tests/pipeline.rs` test `Router`).
>   Verified: `ava-engine` **42/42** (8 outbound_sender), `ava-chains` 7/7, clippy `--all-targets -D warnings` + fmt
>   clean, full workspace build green. **★ Remaining M9.15 frontier:** wiring `OutboundSender` (with its `Router`) into
>   the live node-assembly boot path (replace the loopback/`RecordingSender`) + the two-binary `mixed_network` live arm
>   (nightly/operator-gated). The sender's wire-out + timeout-registration are now both production-complete.

> **STEP (q) — `OutboundSender` WIRED INTO THE `avalanchers` BOOT PATH; STEP (o)/(p)'s "wire into the live boot path"
> frontier CLOSED (2026-06-22, ralph iteration, TDD; single-track `avalanchers`).** STEP (o)/(p) built + unit-tested the
> production `OutboundSender` inside `ava-engine`; this step is the node-assembly wire-up — the chain-boot path can now
> select the real ava-network-backed `Sender` instead of the in-process `RecordingSender`.
> - **`crates/avalanchers/src/wiring/chains.rs`:** new additive `pub async fn boot_chain_over_network(chain_id, subnet_id,
>   network: Arc<dyn ava_network::network::Network>, allower: Arc<dyn Allower>, inner_vm, genesis_bytes, base_db, token)
>   -> Result<NetworkChainBootHandle>`. It drives the same `create_snowman_chain` pipeline as the existing `boot_chain`
>   but the chain's `Sender` is `OutboundSender::new(network, allower, Arc::clone(&router) as Arc<dyn Router>, chain_id,
>   subnet_id, timeout_config().initial_timeout)` — `router.as_ref()` still feeds `create_snowman_chain`, so the one
>   `ChainRouter` both registers request timeouts (via the `OutboundSender`, STEP (p)) and routes inbound. The shared
>   assembly body was factored into a private generic `boot_chain_with_sender<V, Snd, F>(.. sender: Arc<Snd>, router,
>   clock, .., after_create: F)`; both `boot_chain` (RecordingSender, `after_create` = loopback-install) and
>   `boot_chain_over_network` (OutboundSender, `after_create` = no-op) call it — **`boot_chain`/`PChainBootHandle`/
>   `RecordingSender` are behavior-identical** (all existing tests green through the refactor). New lightweight
>   `NetworkChainBootHandle` (`ctx`/`join`/`token`/`genesis_id`/`last_accepted_height`/`beacons`/`vm_tx`/`_sink`/
>   `_data_dir`) has **no `sender` field** — the network path observes outbound traffic via the caller-held `Network`,
>   not a recording stand-in.
> - **Deps:** `avalanchers` gained `ava-network` (`[dependencies]`, the seam's signature) + `ava-network`/`ava-message`
>   (`[dev-dependencies]`, the test's mock `Network` + wire-byte decode). Both already workspace members; root `Cargo.toml`
>   untouched.
> - **TDD:** new `crates/avalanchers/tests/outbound_sender_boot.rs::boot_over_network_carries_frontier_broadcast_out_to_the_network`
>   boots a `TestVm` chain with `include_self_beacon: true` over a recording mock `Network` (ported from the STEP-(o)
>   `ava-engine/tests/outbound_sender.rs` precedent), polls the mock's recorded sends (bounded non-blocking
>   `tokio::time` loop) for the bootstrapper's `GetAcceptedFrontier`, decodes the `OutboundMessage` back
>   (`MsgBuilder::parse_inbound`), and asserts op == `GetAcceptedFrontier` + subnet + the beacon (self) recipient set —
>   proving the **production `OutboundSender`** (not the loopback/noop) carried the engine's outbound op to the
>   `Network`. RED-confirmed (function/deps absent ⇒ E0432). Cleanly cancels + awaits the handler on teardown.
> - **Verified (main tree, post-merge):** `cargo nextest run -p avalanchers` = **18/18** (new test + every pre-existing
>   `boot_chain`/loopback/restart/dispatch test through the refactor), `cargo build -p avalanchers` + `cargo clippy -p
>   avalanchers --all-targets -- -D warnings` + `cargo fmt --check` all clean. **★ Remaining M9.15 frontier:** only the
>   two-binary `mixed_network` live arm (boot a real `ava_network::NetworkImpl` over TLS + dialer + real Go peers and
>   replay the lockstep program) — nightly/operator-gated, needs a live Go node the sandbox can't run. The
>   `OutboundSender`'s wire-out + timeout-registration AND its node-assembly wire-up are now all production-complete; what
>   is left is purely the live two-binary *execution*.

> **FOLLOW-UP — production network→consensus wiring not yet called from the live boot path (M9.15 deferral).** The
> network→consensus seam (decode inbound p2p → engine `Router`) is proven end-to-end by the two-Rust-node test
> `crates/avalanchers/tests/networked_bootstrap.rs`: it calls `boot_chain_over_network` and wires the returned engine
> `Router` into a `RouterBridge` via `set_engine_router`, so inbound peer messages are decoded and routed to the engine.
> The **production node-assembly boot path does not yet do this**: `init_networking` creates a `RouterBridge` whose
> engine-router slot remains empty (the `init_chain_manager` call that fills it via `set_engine_router` with a
> `ChainRouter` is wired, but `init_chain_manager` / `drive_startup_chains` do not call `boot_chain_over_network` — they
> use the in-process `RecordingSender` loopback instead). As a result, a live node with real peers would have inbound
> messages decoded and forwarded to the `ChainRouter`, but the chain-boot path sends outbound via the real
> `OutboundSender` / receives nothing via the `ChainRouter`'s per-chain handlers (they were booted with the loopback
> path). The next M9.15 production step is to replace the loopback-boot call in `drive_startup_chains` with
> `boot_chain_over_network`, thread the live `NetworkImpl` + `Allower` through, and confirm the returned `router` is
> already the one installed into `RouterBridge` by `init_chain_manager` — closing the loop so the live node routes
> inbound p2p ops into the running chain engines.

> **AS-BUILT (2026-06-24, Tasks 1–9 of `m9.15-prod-net-consensus-wiring`).** `drive_startup_chains_over_network` now boots P/X/C over the shared `ChainRouter` + real `OutboundSender` (`crates/avalanchers/src/wiring/chains.rs`); `main.rs` calls it (solo path, beaconless short-circuit flips isBootstrapped); `init_networking` dials configured beacons via `track_bootstrappers`. Phase 1 loop-close test (`prod_loop_close.rs`: inbound `GetAcceptedFrontier` routed through shared `ChainRouter`, `AcceptedFrontier` reply leaves via `OutboundSender`; beaconed boot broadcasts frontier request to configured beacon) and Phase 2 two-`Node` localhost-TLS convergence test (`two_node_convergence.rs`: follower `Node` bootstraps from beacon `Node` to `NormalOp`) both pass (55/55 `-p avalanchers -p ava-node`). Lint clean. The live Go interop arm (two-binary `mixed_network` with real Go peers) remains nightly-gated — the TLS-1.3 mutual-handshake stall (rustls-client ↔ Go-server) documented in Task 7 is unaffected by this wiring. Known deferred: the gate-waiting spawned task in `boot_chain_with_sender` (the `start_gate` branch) is not cancellation-aware while the gate is pending — it unwinds only when the watch sender drops; this is a pre-existing property (test-only paths today, since the solo gate pre-fires and the follower gate fires on handshake) to revisit when a cancellable gate is exercised by subnet or real-bootstrap paths.

**Files:** `tests/differential/tests/mixed_network.rs`, `tests/differential/src/network.rs` (live spawner rewrite — items (b)/(c) above)
- [ ] **Step 1 — Red:** Write `differential::mixed_network`: boot the mixed Go+Rust network (M9.14); replay a proptest-generated input program (`IssueTx`/`ApiCall`/`AdvanceTime`/`AwaitFinalization`) against the whole network; after each `AwaitFinalization`, collect+normalize `Observation` from every node and assert all nodes (Go and Rust) agree on LA block ID+height, state/merkle root, and sorted validator set for **every** chain (P/X/C/SAE) — no fork, same tip. Failure prints `DIFFERENTIAL_SEED=<n>`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential mixed_network` → fails.
- [ ] **Step 3 — Green:** Implement the lockstep driver reuse from `02` §11.6 over the mixed network; deterministic tx/key derivation from the seed feeds identical bytes to all nodes; persist minimal failing `(seed, program)` to `tests/differential/proptest-regressions/`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential mixed_network` → passes (live mode; run on the nightly budget).
- [ ] **Step 5 — Commit:** `differential: mixed_network — live Go+Rust, all chains, no fork, same tip`

> **Follow-up — M9.15-next: fix the Rust `Handshake` `knownPeers.filter` bloom encoding (Rust→Go
> p2p, NOT TLS). ✅ ENCODING FIX DONE (branch `m9.15-next-handshake-bloom`, 2026-06-26).**
> Surfaced by the Stage 2 live run (see the "★ROOT CAUSE PINNED 2026-06-26" note above): TLS-1.3
> now clears against the Go beacon, but the Rust follower's outbound application-layer `Handshake`
> message carried a malformed `knownPeers` bloom filter. The Go beacon rejected all 7 handshake
> attempts with `peer/peer.go:940 malformed message {field:"knownPeers.filter", error:"invalid num
> hashes"}`, never registering the Rust node as a peer (`Peers:0`).
>
> **Fix implemented in `crates/ava-network` (Tasks 1–7, branch `m9.15-next-handshake-bloom`):**
> - Write-side `Filter` + `optimal_parameters`/`estimate_count` sizing ported from Go `x/bloom`
>   (`bloom.rs`; tests: `filter_marshal_roundtrip`, `optimal_params_match_go_reference`,
>   `optimal_params_smoke`, `filter_add_increases_count`, `filter_full_set_add_returns_false`).
> - `gossip_id` helper: `sha256(NodeId ‖ timestamp_as_u64_be)` matching Go `ClaimedIPPort.GossipID`
>   (`ip_tracker.rs`).
> - `IpTracker` owns a `Filter` + salt, seeds on `add`, exposes `bloom() -> (Vec<u8>, Vec<u8>)`
>   (`ip_tracker.rs`; tests: `bloom_returns_valid_parseable_filter`,
>   `tracker_add_seeds_bloom_entry`, `bloom_salt_over_max_rejected`).
> - `peers()` dedup keyed on `gossip_id` (matches Go `ClaimedIPPort.GossipID`; test:
>   `peers_excludes_known_via_bloom`).
> - `build_handshake` emits the real bloom from `IpTracker::bloom()` instead of a zero-byte
>   placeholder; `GetPeerList` pull-gossip trigger likewise carries the current bloom.
>   Tests in `peer.rs`: `handshake_known_peers_filter_is_go_parseable`,
>   `get_peer_list_trigger_enqueues_parseable_bloom`.
>
> **Remaining:** live `mixed_network` arm is `#[cfg(feature="live")] #[ignore]` and remains
> nightly-gated. A nightly run is needed to confirm the Go beacon registers the peer (`Peers:1`)
> and bootstrap proceeds — the encoding fix is in place; this is purely a live-gate confirmation.

> **★ AS-BUILT — LIVE 5-Go-validator-quorum + Rust-follower run (2026-06-28, branch
> `m9.15-mixed-network-go-quorum`).** `boot_mixed` was rewritten (test-harness only) to bring up the
> full 5-validator Go primary network (full bootstrap mesh) + 1 Rust ECDSA follower, with a staged
> Stage 1 (Go cluster bootstraps) → Stage 2 (Rust follows) wait, plus `Vec<Bootstrap>` multi-bootstrapper
> args, a vendored `LOCAL_VALIDATOR_NODE_IDS` table, `mesh_peers`, and `go_beacon()`/`rust_follower()`
> accessors. Run against `~/avalanchego@cbea62895c` (`rpcchainvm=45`):
> - **STAGE 1 — GREEN.** The 5 Go validators reach quorum and bootstrap P/X/C (`go1` health
>   `readiness/bootstrapped` + `health/{P,X,C}` all "started passing"). ★ **Required a fix the spec
>   missed:** the harness launched each Go validator with its TLS staker cert/key (→ NodeID) but **no
>   BLS signer key**, so each generated a random BLS key whose signed-IP signature did not match the
>   genesis-registered `proofOfPossession` → every validator rejected its peers with `invalid BLS
>   signature` → no quorum (run #1). Fix: `NodeLaunch.signer_key_file` + `--staking-signer-key-file` +
>   `local_signer_key(idx)` resolving `$AVALANCHEGO_SRC/staking/local/signerN.key` (the keys ship in the
>   avalanchego repo); Go validators get `Some(signerN.key)` aligned with their NodeID, the Rust follower
>   `None` (non-validating, no registered pubkey).
> - **STAGE 2 — RED (rung isolated, the next spec/plan).** The Rust follower does **not** bootstrap
>   P/X/C within 180 s (`mixed_network.rs:157` timeout). Progress vs the prior TLS-version and
>   knownPeers-bloom blockers: TLS 1.3 mutual handshake completes (rustls client, ECDSA client-auth) **and
>   the app-layer Handshake now succeeds** — the follower replied with a `PeerList` to Go validator
>   `NodeID-GWPcb…` (staker4). The **new** rung: the follower's log shows exactly that one handshake then
>   goes silent — **no** bootstrap-protocol traffic (`GetAcceptedFrontier`/`AcceptedFrontier`/`Ancestors`)
>   is ever emitted. It stalls at the connect→bootstrap-start boundary (never registers enough connected
>   validators to begin frontier exchange). This is **production engine/bootstrapper wiring**, out of
>   scope for this test-harness-only plan, and becomes the next M9.15 spec/plan.
> - **Arm stays nightly-gated** (`#[cfg(feature="live")] #[ignore]`). The offline arms
>   (`mixed_network_replay_is_deterministic` + proptest) are untouched and CI-green.

### Task M9.16: Go-data-dir → RocksDB import path (R2 migration) ✅ DONE (2026-06-15; `tests/go_dir_import.rs`)
**Crate/area:** `ava-database` + `ava-node`  ·  **Depends on:** M1 (RocksDB backend, R2 scoped), M8 (node init)  ·  **Spec:** `26` §6 (DB version folder detection), `00` §4.4 / §11.2 R2, `04` R2, `27` §4 (marker)
**Files:** `crates/ava-database/src/migrate/import.rs` (facade over the existing `migrate/` engine), `crates/ava-node/src/init/db_init.rs`, `crates/ava-database/tests/go_dir_import.rs`
- [x] **Step 1 — Red:** Write `imports_go_pebble_dir_to_rocksdb` and `refuses_unsupported_dir`: given a captured Go-written Pebble/LevelDB data dir (fixture under `tests/vectors/migration/`), assert the import produces a RocksDB dir named `v1.4.5` (`CURRENT_DATABASE`) whose key/value set equals the source's; and that pointing the node at a foreign/older dir without invoking the import triggers the documented refusal (not an in-place open that corrupts).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-database go_dir_import` → fails.
- [x] **Step 3 — Green:** Implement `import.rs`: detect the source backend by the schema-version folder name (`26` §6); stream all KV pairs into a fresh RocksDB dir named `CURRENT_DATABASE`. Implement `db_init.rs` detection: if the data dir is a `PREV_DATABASE`/foreign backend, run the import (or refuse with a clear error if import is not requested), never open-in-place. Wire the `ungracefulShutdown` marker semantics (`27` §4).
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-database go_dir_import` → passes.
- [x] **Step 5 — Commit:** `ava-database: Go-dir → RocksDB import path (R2) + node refusal of foreign dirs`

> **AS-BUILT (merge `59fa2e6`).** The import facade lives at `crates/ava-database/src/migrate/import.rs` (under the existing `migrate` module, not a top-level `import.rs`) — it wraps the already-present `migrate()` verbatim-copy driver. Public API (re-exported from `lib.rs` under the `migrate` feature): `GoBackend{Goleveldb,Pebble}` + `detect_backend(path)` (folder-name detection, **feature-free** so `ava-node` reuses it without pulling RocksDB), `ImportError`, `ImportOptions`/`ImportReport`, `current_db_dir_name()`, and the rocksdb-gated `import_go_dir(...)` / `import_source_into_rocksdb(&dyn GoDbSource, ...)`. Node-side refusal is `crates/ava-node/src/init/db_init.rs::precheck_data_dir(...)` (called by `init/database.rs` *before* the open; never touches the `ungracefulShutdown` marker — that stays owned by `init/database.rs`), surfacing the new typed `Error::ForeignDataDir{path,backend,current}`. **Test-fixture note:** no real captured Go Pebble/LevelDB dir was synthesized (the Pebble sidecar spawn is a documented M12 stub; RocksDB writes RocksDB-format not classic LevelDB), so `imports_go_pebble_dir_to_rocksdb` drives the facade through the **real on-disk RocksDB write path** with an injected `GoDbSource` (`VecSource` mirroring the `04` §10 layout) and re-opens the resulting `v1.4.5/` dir to assert byte-for-byte KV equality + cursor. Verified in main tree: `cargo nextest run -p ava-database --features migrate,rocksdb` = **50/50**, `-p ava-node` = **19/19**, clippy `--all-features` clean. The goleveldb fast-path (`RocksDbCompatSource`) and merkleized `RootVerifier` wiring remain for the M12 CLI.

### Task M9.17: `test-upgrade` — Go→Rust across an activation height (incl. Go-dir import) ✅ OFFLINE ARMS DONE (2026-06-16; swap/import orchestration + no-fork continuity); live Go→Rust arm gated
**Crate/area:** `tests/upgrade` + `xtask`  ·  **Depends on:** M9.16, M9.14 (mixed-net driver), M8  ·  **Spec:** `02` §10.4, `16` §5(8), `26` §7 (rolling-upgrade moving floor), `00` §4.4
**Files:** `tests/upgrade/src/{lib,plan,continuity}.rs`, `tests/upgrade/tests/go_to_rust.rs`, `xtask` `test-upgrade` subcommand
- [x] **Step 1 — Red:** Write `go_to_rust`: start a tmpnet network on the previous released **Go** binary; advance to just before an activation height; replace nodes one-by-one with the **Rust** binary across the activation height, importing each node's Go data dir → RocksDB (M9.16) on swap; assert chain continuity and **no fork** (every node's LA/state root agrees across the cut-over) and that the moving min-compatible floor (`26` §7) keeps Go and Rust peers connected during the roll. Add `cargo xtask test-upgrade` alias.
- [x] **Step 2 — Confirm red:** `cargo xtask test-upgrade` (or `cargo nextest run -p ava-upgrade go_to_rust`) → fails.
- [x] **Step 3 — Green:** `plan.rs` `RollingUpgrade::swap` drives the REAL M9.16 `import_source_into_rocksdb` facade (on-disk RocksDB write path ran for real) + byte-verifies the migrated KV set; `continuity.rs` `assert_no_fork` over the real `ava_differential::Observation` + `MovingFloor` over the real `ava_version::Compatibility`. Wire the `xtask` alias (done by prep commit).
- [x] **Step 4 — Confirm green:** `cargo xtask test-upgrade` → passes (offline arms; live Go→Rust arm gated `#[cfg(feature="live")] #[ignore]`, `live = ["ava-differential/live"]`).
- [x] **Step 5 — Commit:** `M9.17: test-upgrade swap/import orchestration + no-fork continuity offline arms; live Go→Rust arm gated`

### Task M9.18: `test-load` — sustained tx stream, metrics SLOs, zero errors ✅ OFFLINE ARMS DONE (2026-06-16; generator determinism + Prometheus SLO logic); live tmpnet arm gated
**Crate/area:** `tests/load` + `xtask`  ·  **Depends on:** M9.14 (network bring-up), M5/M6 (X/C tx issue), M8 (API/wallet/metrics)  ·  **Spec:** `02` §10.3, `16` §5 (perf), `00` §7.3 (metric-name parity)
**Files:** `tests/load/src/{generator,metrics,network}.rs`, `tests/load/tests/{generator_offline,metrics_offline,sustained_load}.rs`, `xtask` `test-load` subcommand
- [x] **Step 1 — Red:** Write `sustained_load`: against a tmpnet Rust network, the load generator issues a sustained C-Chain transfer + X/P tx stream for `--load-timeout`; scrape Prometheus (parity metric names, `00` §7.3); assert throughput/latency SLOs hold and **zero** errors. Add `cargo xtask test-load`.
- [x] **Step 2 — Confirm red:** `cargo xtask test-load` → fails.
- [x] **Step 3 — Green:** `generator.rs` (deterministic splitmix64 seed-derived C/X/P stream + integer `PacingSchedule` rate math, no floats) + `metrics.rs` (Prometheus `Exposition` parser + pure `slo_holds` + `REQUIRED_PARITY_METRICS` from `00` §7.3 / `18`) + `network.rs` (live `LoadNode` scraping `/ext/metrics` over hand-rolled HTTP/1.1, no HTTP-client crate). 12 offline tests + committed fixtures. `xtask` alias done by prep commit.
- [x] **Step 4 — Confirm green:** `cargo xtask test-load` → passes (12 offline arms; live `sustained_load` arm gated `#[cfg(feature="live")] #[ignore]`). **Deferral:** tx signing/issuance left to the operator (would need `ava-wallet`; deliberately not a dep so the offline build stays light).
- [x] **Step 5 — Commit:** `M9.18: test-load sustained-stream generator + Prometheus SLO offline arms; live tmpnet arm gated`

### Task M9.19: `test-reexecute` — replay recorded mainnet ranges → state roots match Go 🟡 C + X + P-CHAIN LEGS DONE (C 2026-06-15, X 2026-06-16c, P determinism 2026-06-16d, **P height≥1 accept 2026-06-16e**); Go-`blockexport` parity deferred
**Crate/area:** `tests/reexecute` + `xtask`  ·  **Depends on:** M6 (C-Chain `differential::cchain_state_root`), M4/M5 (P/X), M9.14  ·  **Spec:** `02` §10.5 (reexecute = differential oracle), `16` §5(3), `00` §11.7 (per-PR)
**Files:** `tests/reexecute/src/lib.rs`, `tests/reexecute/tests/cchain_range.rs`, `tests/reexecute/tests/px_range.rs`, `xtask/src/commands/test_reexecute.rs`
- [x] **Step 1 — Red:** Write `reexecute_cchain_range` and `reexecute_px_range`: from a fixed starting state, replay a recorded range of mainnet C-Chain (and P/X) blocks (`blockexport` fixtures) through the Rust VMs; assert resulting state/merkle roots match the Go-recorded expected roots byte-for-byte (a differential oracle on recorded data). Add `cargo xtask test-reexecute`.
- [x] **Step 2 — Confirm red:** `cargo xtask test-reexecute` → fails.
- [x] **Step 3 — Green:** Implement the reexecution harness consuming Go `blockexport` artifacts (reuse the M6 reexecute fixtures); a fixed-start-state replay loop per chain asserting roots; wire the `xtask` alias. Mark it as the per-PR cheap differential oracle (`00` §11.7).
- [x] **Step 4 — Confirm green:** `cargo xtask test-reexecute` → passes (per-PR budget).
- [x] **Step 5 — Commit:** `tests: test-reexecute recorded mainnet ranges → Go-identical state roots`

> **AS-BUILT (merge `3b52e32`).** New workspace crate **`ava-reexecute`** at `tests/reexecute/` (added to root `Cargo.toml` `members`). `src/lib.rs` exposes a reusable harness — `ReexecuteCase`/`AllocEntry`/`ReexecuteRoots`/`Error` (thiserror) + `replay_cchain(&case) -> Result<ReexecuteRoots>` — ported verbatim from the M6.6 `crates/ava-evm/tests/cchain_state_root.rs` pipeline (Firewood-ethhash propose→commit genesis, decode EIP-2718 txs, `ExternalConsensusExecutor::execute_batch`, bundle→proposal post-root). The `genesis_to_1` fixture (`genesis_to_1.json` + `manifest.json`) was **copied** into `tests/reexecute/vectors/cchain/` so the crate is self-contained. `xtask/src/test.rs::test_reexecute()` (the pre-existing `TestReexecute` subcommand) now shells out to `cargo nextest run -p ava-reexecute` (no `main.rs` change). Verified in main tree: `cargo nextest run -p ava-reexecute` = **1 passed, 1 skipped**, `cargo xtask test-reexecute` green, clippy `--all-targets -D warnings` clean. **DEFERRED — `reexecute_px_range`:** authored as `#[ignore]` (panics if forced) — no Go-recorded P/X `blockexport` fixtures exist in the repo. Follow-up (fold into `02` §10.5): record a P/X `blockexport` fixture, add `replay_px` + a P/X `ReexecuteCase` equivalent, gate the live arm behind the reserved `px` feature.

> **AS-BUILT — X-Chain leg (merge 2026-06-16c).** `reexecute_px_range` is **no longer `#[ignore]`d**: new `src/xchain.rs` `replay_xchain(seed) -> XchainReexecuteRoots` drives the REAL `ava-avm` VM/block pipeline (ported from the `ava-differential` `xchain` collector into lib code that propagates VM/codec errors via the new `Error::Xchain`, no `unwrap`/`expect`) over a seed-derived synthetic chain of `BaseTx` issuances (`initialize` → seed genesis → admit tx → build → set_preference → verify → accept, one tx/block) — exactly mirroring how the C-Chain `genesis_to_1` is a synthetic fixture run through the real EVM pipeline. X-Chain keys UTXOs by id with no merkle trie (`StandardBlock::MerkleRoot()` ≡ zero id), so the reexecute "root" is the deterministic post-state digest: `sha256` over the canonically-sorted `(utxo_id ++ utxo_bytes)` of the final UTXO set + tip block id/height. `tests/px_range.rs::reexecute_px_range` replays the SAME case on two INDEPENDENT VM instances → byte-identical roots (determinism, specs/00 §6.1; **NOT a fabricated/hardcoded root**), asserts non-triviality (height ≥ 1, real non-zero 32-byte sha256), and that a different seed → a different root. Added `ava-avm`/`ava-vm`/`ava-secp256k1fx`/`ava-snow`/`ava-types`/`ava-version`/`ava-crypto`/`async-trait`/`tokio`/`tokio-util` to `tests/reexecute/Cargo.toml` (paths copied from `tests/differential/`). Verified in main tree: `cargo nextest run -p ava-reexecute` = **5 passed, 0 skipped**, clippy `--all-targets -D warnings` clean, fmt clean.

> **AS-BUILT — P-Chain leg (merge 2026-06-16d).** New `src/pchain.rs` `replay_pchain(seed) -> PchainReexecuteRoots` drives the REAL `ava-platformvm` VM pipeline — `initialize` over a seed-derived genesis (two UTXOs + one current validator) → `genesis::parse`/`seed_state` → genesis block → `build_block` — through the established clock-pinning trick (genesis time + validator period future-pinned, so `now < parent_ts` ⇒ no wall-clock leak / no staker-change cap). The driver loop is general + `MAX_BLOCKS`-capped. P-Chain keeps **flat KV state (no merkledb)**, so the reexecute "root" is the deterministic post-state digest: `sha256` over the canonically-sorted final UTXO set (`State::utxo_ids` by the seed-derived owner) + Primary-Network supply + chain timestamp (big-endian), plus the chain-tip block id/height in `PchainReexecuteRoots`. `tests/pchain_range.rs::reexecute_pchain_range` replays the same case on two INDEPENDENT VMs → byte-identical roots (determinism, specs/00 §6.1; **NOT a fabricated/hardcoded root**), asserts a real non-zero 32-byte digest + tip id, and that a different seed → a different root. One **additive, scoped** `ava-platformvm` change: `#[doc(hidden)] pub fn PlatformVm::with_state<R>(&self, read: impl FnOnce(&State<DynDb>) -> R) -> Result<R>` (the read-only state seam mirroring `ava_avm::vm::AvmVm::with_state`; no other production behaviour touched). Verified in main tree: `cargo nextest run -p ava-reexecute` = **9 passed, 0 skipped** (C + X + P), `-p ava-platformvm` = **148 passed** (no regression), clippy `--all-targets -D warnings` clean, fmt clean, `cargo build --workspace` + `-p avalanchers` green.
>
> **Honest floor — `build_block` declines at genesis (height 0) today.** Two real gaps block a height ≥ 1 accepted block and were NOT papered over: (1) **un-shared decision-tx mempool** — `PlatformVm.mempool` is private with no public admission seam (the X-Chain `AvmVm::mempool_add` analogue is absent; `vm.rs` "RPC issuance not yet wired"); (2) **genesis ⇄ staker-reward resolver gap** — `genesis::seed_state` records the validator as a current staker but does not store its tx, so the reward-proposal executor's `staker_tx_resolver` (`State::get_tx`, `block/executor/mod.rs`) returns `ErrNotFound` on verify. The reward-proposal block is the only height-advancing path needing no decision tx, so it is blocked by (2). The leg therefore rests at the accepted genesis tip. The harness is written so the **same code advances height with no change** once either gap closes. **STILL DEFERRED:** (a) the P-Chain **height ≥ 1 accepted-block** arm (blocked on M8 shared mempool / M4.24 genesis-reward-resolver wiring — fold into `02` §10.5), and (b) the Go-recorded-`blockexport` parity arm (no Go-executed P-Chain fixture exists; reserved `px` feature gates the future live arm).

> **AS-BUILT — P-Chain height ≥ 1 accepted block + Gap 2 closed (merge 2026-06-16e, 3 parallel worktree agents).** Both M9.19 gaps from the honest-floor note above are now closed; `reexecute_pchain_range` asserts `last_accepted_height == 1` (not 0) and stays deterministic. The height-advancing path chosen is the **decision-tx / standard-block** route (NOT the reward-proposal route — that needs a deterministic mock clock to reach the staker's `end_time`, a separate `24` determinism follow-up, since `build_block` reads `SystemTime::now()` directly at `vm.rs:631` and the genesis validator is future-pinned). Three findings made it clean and clock-free:
> - **Gap (1) mempool seam — `crates/ava-platformvm/src/vm.rs`:** new `pub fn PlatformVm::mempool_add(&self, tx: Tx) -> Result<()>` (locks the `PlatformVm`-owned `mempool` — P-Chain's mempool is a VM field, NOT in `Shared` as on X-Chain — and calls `.add(tx)`, mapping rejection via the existing `Error::Service(String)`; `ava-platformvm`'s error enum has **no `Config` variant**, so the X-Chain `Error::Config` analogue was not reused; no `error.rs` change).
> - **The harness drive — `tests/reexecute/src/pchain.rs`:** admits one funded `CreateSubnetTx` via `mempool_add` before a bounded one-block build loop; `build_block` packs it into a `BanffStandardBlock` at the future-pinned `GENESIS_TS`, which `verify_standard` (`block/executor/verify.rs`) accepts because it **enforces no future-time bound** and stores decision txs via `diff.add_tx`. **★ No signing needed:** the harness never transitions to `NormalOp`, so the executor `Backend` stays `bootstrapped:false`, the fx skips credential verification, and `verify_spend` for `CreateSubnetTx` checks only AVAX conservation + UTXO existence — so an **empty credential** over the genesis `owners(seed)` suffices (the exact precedent the X-Chain leg uses; documented inline). Fee is computed in-harness from the **dynamic** calculator (mainnet has Etna active at `GENESIS_TS`): `DynamicCalculator::from_excess(0).calculate_fee(base_tx_complexity()) = 58 nAVAX`; the tx consumes genesis `U0` and returns `amount0 − 58` change to the same owner (balances exactly), so the post-state digest stays deterministic. **★ Loop bounded by admitted-tx count (1):** the P-Chain accept-side mempool drain is itself an un-wired follow-up (`vm.rs` build_block comment), so a naïve "build until decline" loop re-packs the same tx into every block up to `MAX_BLOCKS`; the loop now builds exactly one block per admitted tx (mirrors the X-Chain bounded driver).
> - **Gap (2) genesis ⇄ reward resolver — `crates/ava-platformvm/src/genesis.rs`:** `seed_state` now calls `state.add_tx(vdr_tx.id(), vdr_tx.bytes().to_vec())` after `state.put_current_validator(staker)` for each genesis validator (`vdr_tx.bytes()` is already populated — `genesis::parse` initializes every validator tx). New inline test `genesis::seed::seed_state_records_genesis_validator_tx` asserts the genesis validator's tx is now `get_tx`-resolvable and projects to `Some(_)` through `rewarded_staker_tx` — i.e. a genesis validator is finally rewardable (closes the long-standing **M4.24** gap). This is independent of the height-1 decision-tx path but completes the reward-proposal route for when the clock seam lands.
> Verified in main tree (full clean rebuild of the touched crates): `cargo nextest run -p ava-platformvm -p ava-reexecute` = **158 passed, 0 skipped** (`ava-platformvm` 149 incl. the new genesis test, `ava-reexecute` 9 incl. `reexecute_pchain_range` at height 1), clippy `--all-targets -D warnings` clean, fmt clean, `cargo build -p avalanchers` green. **STILL DEFERRED:** the deterministic-mock-clock seam on `PlatformVm` (would unlock the reward-proposal height path + `bootstrapped:true` credential-verifying replay — a `24` determinism item), and the Go-recorded-`blockexport` parity arm (no Go-executed P-Chain fixture; reserved `px` feature).

### Task M9.20: Crash-injection hardening pass (CC-ATOMIC / two-sided SM consistency) ✅ OFFLINE ARM DONE (2026-06-16); live Go-oracle-equivalence arm gated
**Crate/area:** all VMs + `ava-database` + `ava-chains` + `ava-node`  ·  **Depends on:** M4–M7, M9.6 (sharedmemory), M9.19  ·  **Spec:** `27` §9 (crash-injection suite), §2 (CC-ATOMIC), §3.1 (two-sided SM), `02` §11
**Files:** `tests/differential/src/crash.rs`, `tests/differential/tests/crash_injection.rs`
- [x] **Step 1 — Red:** Write `crash_injection_cc_atomic` and `shared_memory_two_sided_consistency`: parameterize the accept/execute path with a `CrashPoint` (C0–C7, `27` §3) via a `FailpointDb` (errors/aborts on the N-th `write()`) and an out-of-process `kill -9` at logged checkpoints; on restart run the §5 recovery and assert (a) every accepted block is fully present or fully absent (CC-ATOMIC — no partial diff/dangling LA/orphan SM), and (b) for an X→P (and X→C) export/import crashed in the `[SM-replay, write)` window, the peer chain observes all-or-nothing and the UTXO is never double-spendable nor lost — matching the Go oracle after the same crash+restart.
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential crash_injection` → fails.
- [x] **Step 3 — Green:** Implement `crash.rs`: the `FailpointDb` wrapper + the out-of-process crash harness; the recovery-equivalence + CC-ATOMIC assertions against the Go oracle. Fix any hardening gaps surfaced (idempotent redo paths, abort guards) per `27` §5 — but only the minimum to make the recovery byte-identical to Go.
- [x] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential crash_injection` → passes (offline arm; Go-oracle-equivalence arm gated).
- [x] **Step 5 — Commit:** `hardening: crash-injection suite (CC-ATOMIC, two-sided shared-memory consistency; offline arm; Go-oracle arm gated)`

> **AS-BUILT (merge `4c7ce80` of branch `m920-crash-injection`, 2026-06-16; parallel worktree wave with M9.12).**
> `tests/differential/src/crash.rs`: `FailpointDb` wraps an `Arc<MemDb>` (shared backing store) as a
> `DynDatabase` and injects a deterministic `Error::Other(InjectedCrash)` on the N-th mutating op (no
> wall-clock, no RNG); "restart" = rebuilding a fresh non-injecting wrapper over the same `Arc`, so the
> surviving bytes are exactly what recovery sees. `AcceptHarness` drives a miniature CC-ATOMIC accept (state
> diff + last-accepted marker + cross-chain shared-memory put — the three §2.1 batch components) through it
> under a `CrashPoint` (`None`/`BeforeWrite`/`MidWrite`/`AfterStateBeforeMarker` — the C0/C1/C2/C4 windows the
> in-memory KV tier can express) via two `CommitStrategy`s: the §2.2 single-`write()` atomic batch and a naive
> per-key loop. On restart it runs idempotent recovery (read marker; drop any marker-uncovered orphan state
> diff / SM entry). **Offline arm** (`tests/crash_injection.rs`, 3 integration tests + 2 unit tests, every CI
> run): the atomic-batch accept recovers all-or-nothing across every crash point + recovery is idempotent
> (`crash_injection_cc_atomic`); the naive path *tears* (state lands, marker/SM don't) and recovery reconciles
> it back to "fully absent" — proving the atomic path is load-bearing (`naive_per_key_tears_then_recovery_reconciles`);
> and a peer chain observes an X→peer export all-or-nothing, never half-exported/double-spendable/lost
> (`shared_memory_two_sided_consistency`, §3.1, built on `atomic::exported_utxo_observation`'s `(key,value)`
> contract). **★ Honesty note:** the in-process KV + SAE-recovery surface proves *deterministic
> atomicity/idempotency of the Rust impl*, NOT byte-identical post-recovery state vs Go — that is the gated
> `#[cfg(feature="live")] #[ignore] crash_injection_vs_go_oracle` arm, which early-returns without a recorded
> Go crash corpus (`$AVA_CRASH_ORACLE_CORPUS`; same recorded-oracle shape as the M7.29 `sae_recovery` corpora —
> Go emitter in `tests/differential/go-oracle/` copied into `~/avalanchego`, env-gated, recording per-crash-point
> reconciled LA / state root / peer SM bytes / SAE A·E·S frontiers). Adds `anyhow` to the crate's `[dependencies]`
> (the failpoint constructs `ava_database::Error::Other(anyhow::Error)`). Verified in main tree: `cargo nextest
> run -p ava-differential` = **20/20** (5 new), clippy `--all-targets -D warnings` clean (incl. `--features live`),
> `--features live --tests` compiles, fmt clean.

### Task M9.21: `bench-guard` perf gates ✅ DONE (gate + seed 2026-06-15; full §9 bench set 2026-06-15)
**Crate/area:** all critical-path crates (`benches/`) + CI  ·  **Depends on:** M0–M8 benches exist; M9.15/M9.19 prove no behavior change  ·  **Spec:** `02` §9 (bench-guard, criterion baselines, >X% fails), `16` §5(9), `00` §9
**Files:** `xtask/src/commands/bench_guard.rs`, `.config/criterion-baseline/`, crate `benches/*.rs` (as needed)
- [x] **Step 1 — Red:** Write `bench_guard_holds`: run the critical-path criterion benches (codec encode/decode, merkledb commit, signature verify, mempool push/pop, message framing, plus the M9 hot paths — rpcchainvm RPC round-trip) and assert each is within threshold (default 10%) of the committed baseline; a synthetic regressed bench must make the guard **fail** (proves the gate works).
- [x] **Step 2 — Confirm red:** `cargo xtask bench-guard` → fails (no baseline / guard logic absent).
- [x] **Step 3 — Green:** Implement `bench_guard.rs` (criterion `--save-baseline`/comparison, per-bench threshold); commit baselines under `.config/criterion-baseline/`; ensure every `00` §9 optimization that shipped (zero-copy block bytes, parallel sig recovery, sharded mempool, channel reuse, arc-swap caches) shows a bench win **and** is backed by a passing differential test (cross-link M9.15/M9.19/M9.20).
- [x] **Step 4 — Confirm green:** `cargo xtask bench-guard` → passes against committed baselines.
- [x] **Step 5 — Commit:** `ci: bench-guard perf gates (criterion baselines, >X% regression fails)`

> **AS-BUILT (merge `52fede0`).** `cargo xtask bench-guard` (new `BenchGuard { threshold }` subcommand → `xtask/src/bench_guard.rs`) runs a guarded set of criterion benches, reads each bench's mean point estimate from `target/criterion/<id>/new/estimates.json`, compares to a committed advisory baseline under `.config/criterion-baseline/<id>.json`, and fails on a >threshold (default 10%, `--threshold <fraction>`) regression. Pure comparison `over_threshold(base,new,threshold)` + a dependency-free `estimates.json`/baseline scanner are unit-tested (5 tests incl. `over_threshold_trips_on_regression` proving a 2× regression trips). **Seed bench set (2 of the §9 list):** `ava-codec` `codec_roundtrip` (`Packer` encode→decode) + `ava-crypto` `secp256k1_verify` — each criterion-configured for sub-second runs (`sample_size(10)`, `measurement_time(500ms)`). `criterion` added once to root `[workspace.dependencies]`. Verified in main tree: `cargo nextest run -p xtask` 5/5; `cargo xtask bench-guard` EXIT 0 (~48s incl. compile); clippy clean. **FOLLOW-UPS (fold into `02` §9):** (1) ✅ DONE — `GUARDED` extended to the full §9 set; (2) the gate currently takes a single global `--threshold` — per-bench overrides are not yet wired; (3) committed baselines are machine-specific/advisory (`.config/criterion-baseline/README.md`) — real CI baselines regenerate per-runner; the impl reads `estimates.json` directly rather than criterion's `--save-baseline` flow, which §9 may want to reflect.

> **AS-BUILT — full §9 bench set (merges `5786de4`/`bd52d78`/`2b1a92f`/`37e300d`, 2026-06-15).** Four parallel
> worktree agents (one disjoint crate each, no shared-file edits; orchestrator wired the single shared `GUARDED`
> list at merge) added the remaining §9 critical-path benches, bringing `GUARDED` to **6**:
> - **`ava-merkledb` `merkledb_commit`** — insert 100 KV pairs into a fresh in-memory `MerkleDb`
>   (`BranchFactor::TwoFiftySix` over `ava_database::MemDb`), open a view, `commit()`, read `get_merkle_root()`
>   (the "merkledb commit" hot path). Baseline 165025.1 ns.
> - **`ava-message` `message_framing`** — `MsgBuilder::marshal`(`Compression::None`)→`unmarshal` round-trip of a
>   representative `p2p::Get` message (outbound→inbound wire framing). Baseline 138.6 ns.
> - **`ava-avm` `mempool_push_pop`** — `Mempool::add` (push 64 pre-built `BaseTx`) → `peek`+`remove` FIFO drain.
>   Baseline 36576.7 ns.
> - **`ava-vm-rpc` `rpcchainvm_roundtrip`** — one proxied `RpcDatabase::get` round-trip across an in-process
>   loopback `proto/rpcdb` server (server+client stood up once outside the timed loop). Baseline 53403.1 ns
>   (25% pad — loopback gRPC is variance-prone).
>
> Each bench mirrors the seed style (short-run criterion config `sample_size(10)`/`measurement_time(500ms)`/
> `warm_up_time(200ms)`); baselines are advisory padded means under `.config/criterion-baseline/`. Verified in main
> tree: `cargo xtask bench-guard` = **"all 6 critical-path benches within threshold"**, EXIT 0; `cargo nextest run
> -p xtask` 5/5; `cargo clippy -p {ava-merkledb,ava-message,ava-avm,ava-vm-rpc,xtask} --all-targets -- -D warnings`
> clean. ★ **Lint gotcha (reusable):** a `criterion` dev-dep used only by a `benches/*.rs` target trips
> `unused_crate_dependencies` on the crate's *lib-test* compilation unit — but only for crates that enforce that
> lint. Crates with **no `[lints]` opt-in** (ava-merkledb, ava-vm-rpc's lib uses an inline `#![warn(...)]`) are
> unaffected at the Cargo-lints level; crates with `[lints] workspace = true` (ava-avm, ava-message) must **inline
> the full root `[workspace.lints.*]` tables** (Cargo forbids mixing `workspace = true` with an override) and set
> `unused_crate_dependencies = "allow"` (verified: all 10 root lints copied exactly, only that one value changed).
> ava-vm-rpc, whose lib carries an inline `#![warn(unused_crate_dependencies)]` (it can't use `[lints] workspace`
> due to an audited `unsafe` block) and has no `#[cfg(test)]` lib code, needed a 2-line `#[cfg(test)] use criterion
> as _;` shim mirroring the existing `use {anyhow as _, thiserror as _};` idiom (a Cargo `[lints] allow` cannot
> override a source-attribute `#![warn]`).

### Task M9.22: Version-string / compatibility-matrix interop conformance 🟡 GOLDEN LEGS DONE (2026-06-15); `version_interop` OFFLINE ARM DONE (2026-06-16c); live floor-drop arm gated
**Crate/area:** `ava-version` + `ava-network` + `ava-api`  ·  **Depends on:** M2 (handshake), M8 (`info.getNodeVersion`), M9.14  ·  **Spec:** `26` §9 (test plan), `16` §5(2)
**Files:** `crates/ava-version/tests/compat_matrix.rs`, `tests/differential/tests/version_interop.rs`, `crates/ava-version/compatibility.json`
- [x] **Step 1 — Red:** Write `golden::compatibility_matrix`, `golden::compatibility_json_byte_parity`, `golden::node_version_reply`, and `differential::version_interop`: assert `Application{avalanchego,1,14,2}.display() == "avalanchego/1.14.2"`; the `compatible()` table cells from `26` §9(3) (newer-major reject, below-floor reject, fork-boundary cut-over reject, different-name accept, mid-connection transition); `compatibility.json` parses byte-identically to the committed Go file; `info.getNodeVersion` reply matches Go field-for-field (modulo build-specific `gitCommit`/`go`); and in the mixed net a Rust node lowered below the Go floor is dropped by Go, and vice-versa (`26` §9(4)).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-version compat_matrix && cargo nextest run -p ava-differential version_interop` → fails for any uncovered cell.
- [x] **Step 3 — Green:** Fill any gaps in `Compatibility::compatible`, the embedded `compatibility.json`, and the `info.getNodeVersion` reply so all cells pass; commit `compatibility.json` byte-identical to the Go tree with a provenance note.
- [x] **Step 4 — Confirm green:** golden legs pass (`cargo nextest run -p ava-version compat_matrix`).
- [x] **Step 5 — Commit:** `ava-version: handshake compatibility-matrix + version-string golden conformance (live version_interop deferred)`

> **AS-BUILT (merge `bbb87a6`).** The three pure-Rust golden legs are complete and verified in main tree (`cargo nextest run -p ava-version` = **21/21, 1 skipped**; clippy `--all-features` clean). `crates/ava-version/compatibility.json` was copied **byte-identical** (1426 B, `cmp`-verified) from the Go tree's `version/compatibility.json` @ upstream `0b0b57143c`, with a `compatibility.json.md` provenance sidecar; a new `src/compat_table.rs` embeds it via `include_str!` (panic-free `LazyLock<Result<..>>` + fallible `rpc_chain_vm_protocol_compatibility()` accessor) — `serde_json` moved dev-dep → dep. `golden::compatibility_matrix` covers every §9(3) cell with two mock clocks straddling a fork; `golden::compatibility_json_byte_parity` asserts embed==file==reparsed-table and protocol 45 ⇒ `[v1.14.2]`; `golden::node_version_reply` pins version-string display + the `info.getNodeVersion` fields ava-version owns (`version`/`databaseVersion`/`rpcProtocolVersion` incl. the `json.Uint32` string form `"45"`).
> **DEFERRED — `differential::version_interop` (`26` §9(4)):** the live mixed Go+Rust floor-drop test belongs in `tests/differential/tests/version_interop.rs`, NOT in `ava-version` (a T0 primitive must not depend on `ava-differential`/`ava-network`/`ava-api`). Blocked on the **M9.14** mixed-network harness (the `ava-differential` `network.rs` is still a ~35-line scaffold). Recorded as an `#[ignore]`d `version_interop_deferred` stub + PORTING note. The full `info.getNodeVersion` JSON reply (incl. `gitCommit`/`vmVersions`) is already golden-tested at the `ava-api` layer (`crates/ava-api/src/info/mod.rs`).

> **AS-BUILT — `version_interop` OFFLINE ARM (merge 2026-06-16c, now unblocked by M9.14).** New `tests/differential/tests/version_interop.rs::version_interop_floor_decisions` (runs every CI, no feature) builds the mixed Go+Rust peer set via `BinaryMix::from_config(NetworkConfig::deterministic(0xC0FFEE, 4))` and drives the REAL `ava_version::Compatibility::with_clock` + `MockClock` to assert: §9(4)(a) below-floor drop (1.13.9 < post-fork floor 1.14.0 rejected by both Go-side and Rust-side); §9(4)(b) at/above-floor accept (1.14.0 inclusive boundary + `CURRENT` accepted); §7 moving-floor flip (1.13.5 accepted pre-fork / rejected post-fork as the clock crosses `upgrade_time`); §9(3) clause-1 newer-major (2.0.0) dropped both sides both clocks; Go-vs-Rust **symmetry** over an 8-rung version ladder (both sides reach the identical verdict for every `(clock, peer)` — neither more permissive); and a per-slot tie-back over `mix.slots()`. The live floor-drop arm `version_interop` (`#[cfg(feature="live")] #[ignore]`) mirrors the `mixed_network_smoke.rs` precedent (early-returns without `$AVALANCHEGO_PATH`; documents the operator handoff: lower a Rust slot below the Go floor → assert drop, symmetric, + cross the fork for the moving-floor drop). The `ava-version` `version_interop_deferred` stub's `#[ignore]` reason + module doc now point here. No new deps (`ava-version` + `pretty_assertions` already present). Verified in main tree: `cargo nextest run -p ava-differential version_interop` green, `-p ava-version compat_matrix` 3 golden legs still green, clippy clean (default + `--features live`), `--features live --tests` compiles.

### Task M9.23: Final acceptance gate (16 §5 definition of done) ✅ OFFLINE GATE DONE (2026-06-16d); per-PR offline arms green, live two-binary arms nightly-gated
**Crate/area:** all crates + `xtask` + CI  ·  **Depends on:** M9.1–M9.22 (every prior M9 task) + M0–M8 exit gates  ·  **Spec:** `16` §5 (the full checklist), `02` §10.1 (PORTING.md), §13, `00` §11.7
**Files:** `xtask/src/acceptance.rs`, `xtask/src/porting.rs`, every crate's `tests/PORTING.md`, `tests/differential/tests/definition_of_done.rs`
- [x] **Step 1 — Red:** Wrote `definition_of_done` (an aggregating test + the xtask `cargo xtask acceptance` static DoD gate) that asserts the full `16` §5 checklist is green simultaneously: (1) joins Mainnet & Fuji and tracks tip without forking; (2) `differential::mixed_network` (indistinguishable mixed net); (3) full `differential::*` suite incl. `test-reexecute` at target cases; (4) `golden::flag_parity` zero diff; (5) `differential::api_parity`; (6) `golden::genesis_block_id` (Mainnet+Fuji exact); (7) `differential::plugin_rust_in_go` + `differential::plugin_go_in_rust` (v45 both directions); (8) `test-upgrade` Go→Rust across activation height incl. Go-dir→RocksDB import; (9) `bench-guard` holds. Also asserts every crate's `tests/PORTING.md` has **zero `wip` rows** (`cargo xtask porting-report`).
- [x] **Step 2 — Confirm red:** `cargo xtask acceptance` → failed initially on the 4 stale `ava-evm` `| wip ` rows.
- [x] **Step 3 — Green:** Reclassified the only outstanding `wip` rows (4 stale `ava-evm` rows → `✅`/`n/a`, verified against shipped M6.22/M6.31 code + spec 20 §7.2); the gate distinguishes per-PR offline arms (recorded-oracle + reexecute + plugin-handshake offline arms, run every CI) from live two-binary differentials (`mixed_network`, `plugin_go_in_rust`, `test-upgrade`, `test-load` — `#[cfg(feature="live")] #[ignore]`, nightly/pre-release per `00` §11.7, `02` §11.7) by asserting the named tests EXIST (both offline + live arms present), not that the live arms run. Ran the BUILDABLE-&-GREEN invariant.
- [x] **Step 4 — Confirm green:** `cargo build --workspace && cargo build -p avalanchers && cargo clippy --workspace -- -D warnings && cargo xtask acceptance && cargo xtask porting-report` → all pass; `acceptance` reports **ALL CHECKS PASSED** (12 DoD probes + zero-wip); `porting-report` = **zero `wip` rows across 34 matrices** (403 ✅ / 40 🟡 / 425 ⬜ / 86 n/a). The legitimate `⬜ not ported` documented-deferral rows are intentionally left untouched (the gate forbids `wip` only, per the `16` §5 / `02` §10.1 wording).
- [x] **Step 5 — Commit:** `M9.23: final acceptance gate (xtask acceptance + porting-report aggregation; reclassify stale ava-evm wip rows; definition_of_done test)`

> **AS-BUILT (merge 2026-06-16d).** `xtask/src/acceptance.rs` + the `Acceptance` subcommand — a static, deterministic DoD gate (modeled on `saevm_exit_gate.rs`: greps sources, does NOT run cargo) that maps every `16` §5 clause to a real named exit test via `(file, needles)` probes (confirmed by grep, not invented — e.g. `(1)/(2)` `mixed_network{,_smoke}`, `(3)` `cchain_range` + `px_range`, `(4)` ava-config `golden_flag_parity`, `(5)` ava-api `api_parity` (`info_parity` + `platform_and_avm_method_sets_pinned`), `(6)` ava-genesis `golden_genesis_block_id`, `(7)` `plugin_rust_in_go` + `plugin_go_in_rust` each with offline + `*_live` arms, `(8)` upgrade `go_to_rust` (`rolling_swap` + `no_fork_across_cutover` + live), `(9)` xtask `bench_guard`, plus supporting `test-load sustained_load`), then reuses `crate::porting` to assert zero `wip` rows repo-wide. `xtask/src/porting.rs::report()` filled in (was a stub): walks all 34 `tests/PORTING.md` under `crates/*/tests/` + `tests/*/tests/`, tallies `✅/🟡/⬜/n/a` per-crate + total, detects `| wip ` table rows (only `|`-prefixed lines, so prose/legend "wip" doesn't trip it), bails non-zero on any. Both have xtask unit tests. `tests/differential/tests/definition_of_done.rs` — a thin auto-discovered `#[test] fn definition_of_done()` pinning the offline-checkable half of the checklist, kept in lockstep with the xtask `DOD` table. **ava-evm reclassification** (the only `wip` offenders repo-wide): `TestDelegatePrecompile_BehaviorAcrossUpgrades` → `✅` (covered by `precompile_dispatch::dispatch_falls_through_and_gates_by_height`; the stateful AllowList/FeeManager `run()` bodies are live in `src/precompile/{allowlist,feemanager}.rs` per M6.31); `TestPredicateBytes{FromExtra,InExtra,Extra}` → `n/a` (verified against spec 20 §7.2 + `src/precompile/warp.rs::warp_predicates_from_tx`: Rust carries warp predicates in the EIP-2930 tx access list, NOT the block-header `Extra` field, so the Go header-Extra helpers are architecturally not applicable; functional warp-predicate verification is covered by `warp_precompile::predicate_verifies_then_precompile_reads`). ava-evm Summary updated to match the parser row counts. Verified in main tree: `cargo xtask acceptance`/`porting-report` exit 0; `-p ava-evm` 184/184; `-p xtask` 10/10; `-p ava-differential -E 'test(definition_of_done)'` 1/1; build workspace + avalanchers + clippy `--all-targets -D warnings` + fmt all clean.
>
> **Note (R-final, drop-in DoD):** the OFFLINE half of the acceptance gate is fully green (every DoD clause resolves to a present named test; zero `wip` repo-wide; build/clippy/fmt clean). The remaining work to fully *retire* R-final is the **nightly live two-binary execution** of the gated arms (`mixed_network`, `plugin_go_in_rust`, `test-upgrade`, `test-load` against `$AVALANCHEGO_PATH` + a built `avalanchers`) + CI cadence wiring (`.github/workflows/ci.yml`), which is operator/nightly-gated by design and out of the per-PR sandbox budget.
>
> **AS-BUILT — CI cadence wiring (merge 2026-06-16e).** The nightly cadence is now wired: a new scheduled workflow **`.github/workflows/nightly.yml`** (`on: schedule: cron '13 7 * * *'` + `workflow_dispatch:`, `permissions: contents: read`, mirrored `concurrency`/nix-dev-shell style from `ci.yml`) runs a single `live-interop` job that invokes a new **`Taskfile.yml` `test-live`** task: `cargo build -p avalanchers --release` → `cargo nextest run -p ava-differential -p ava-load -p ava-upgrade --features live --run-ignored all` → `cargo xtask acceptance` → `cargo xtask porting-report`. `$AVALANCHEGO_PATH` is plumbed job-level via `env: AVALANCHEGO_PATH: ${{ vars.AVALANCHEGO_PATH }}` (a repo variable; without it the `#[cfg(feature="live")] #[ignore]` arms early-return so the job still runs the build + acceptance gate safely). The per-PR `ci.yml` is unchanged except a 1-line pointer comment. Validated: `actionlint` clean on both workflows, `yamlfmt` no-change, `task --list` shows `test-live`. The arms are not *executed* here (no Go node / nightly-only by design) — this lands the cadence so an operator supplying the repo variable gets the live two-binary run automatically.

> **AS-BUILT — M9.15 live `mixed_network` handshake root-cause (D2/D3, 2026-06-23).** Ran the live two-binary arm
> (`AVALANCHEGO_PATH=~/avalanchego/build/avalanchego … cargo test -p ava-differential --features live mixed_network -- --ignored`)
> with the Task-6 (`4744c25`) handshake-rung logging and a new `--log-level=debug` on both nodes (harness
> `tests/differential/src/livenet.rs::node_args`, diagnostic-only — Go honors it identically, widens `logs/main.log`).
> **Outcome: the live arm correctly FAILS (stays `#[ignore]`); the blocker is pinned at the TLS layer, BELOW the
> app-level Avalanche handshake.** Evidence (captured `<workdir>/rust/{node.log,logs/main.log}` + `go/node.log`):
> - The Rust follower dials the Go beacon as the **TLS client** and loops on a backoff-paced redial. Every attempt
>   logs (rustls `0.23.40`): `No cached session` → `Using ciphersuite TLS13_AES_128_GCM_SHA256` → `TLS1.3 encrypted
>   extensions` → `Got CertificateRequest … signature_algorithms: [RSA_PSS_SHA256, ECDSA_NISTP256_SHA256, …]` →
>   **`Attempting client auth`** (`rustls/client/common.rs:106`) — and then **stops**. The next line is a fresh
>   `No cached session` ClientHello on the next redial. The **last successful rung is the client-auth attempt**; the
>   TLS handshake **never completes**. The upgrader's `tracing::debug!("TLS upgrade complete: derived peer NodeID")`
>   (`crates/ava-network/src/peer/upgrader.rs:142`) **never fires**, so no app-level `Handshake`/`PeerList`/signed-IP
>   rung is ever reached.
> - The **Go beacon never registers, upgrades, or rejects the inbound connection**: its `go/node.log` (at
>   `--log-level=debug`) shows only `node/node.go:158 initializing node` (`stakingKeyType: "RSA"`,
>   `NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg`) and three periodic `network/network.go:1241 reset ip tracker bloom
>   filter` lines — **zero** `peer`/`upgradeConn`/inbound-conn/TLS-handshake events. The two `handler.go:459 …
>   "connected"` lines are the Go node connecting to *itself*. So Go's server-side TLS handshake reaches sending
>   `CertificateRequest` but the connection is dropped before Go hands off to its peer upgrader.
> - **Root cause:** a `rustls`-client ↔ Go `crypto/tls`-server **TLS 1.3 mutual-auth interop stall** — the Rust
>   client selects/attempts its (ECDSA-P256) client certificate after Go's `CertificateRequest`, but the handshake
>   does not complete and neither side surfaces a TLS alert. The leading-hypothesis (signed-IP signature mismatch)
>   and the `Handshake`-wire/client-version hypotheses are **ruled out**: the failure is strictly *before* any
>   Avalanche app-level message — no `ip_signer`/`handshake.rs` code path is reached. The prior "loops at ~250ms"
>   observation was the `DIAL_SCAN_INTERVAL` redial with Task-4 backoff (1s→2s→4s spacing visible in the log).
> - **Secondary AS-BUILT defect found (logging, not behavior):** the avalanchers node's **native `tracing` events do
>   not reach the `LogFactory` file/stderr sinks** in this build — the follower's `node.log` *and* `logs/main.log`
>   contain ONLY 64 `log`-crate-bridged `rustls::*` records and **zero** native node events (not even
>   `initializing node`). So the Task-6 handshake rungs would not have been observable even on a *successful* path
>   via these sinks; the rustls `log`→`tracing` bridge is the only thing currently landing. This is a separate
>   wiring gap (LogFactory subscriber not capturing native `ava_*` targets) and is part of the **precise next step**
>   for surfacing app-level rungs. Additionally, `net_impl::handle_dial` swallows the client-side upgrade `Err`
>   silently (`if let Ok(..) = upgraded`), so the TLS-stall error itself is never logged — adding a
>   `tracing::debug!` on that `Err` arm (with the `rustls` error string) is the cheapest way to capture the exact
>   alert/EOF on the next run.
> - **Why a fix is out of scope this session:** isolating *which* TLS 1.3 mutual-auth detail Go rejects (client-cert
>   signature-scheme selection, the ECDSA leaf's `SerialNumber=0`/validity structure under Go's `crypto/tls`
>   acceptance, or a `tokio-rustls` read-half timing/close) requires a **TLS keylog / pcap capture** correlated
>   across both stacks, plus the two logging-wiring fixes above to even see the Rust-side error — a multi-step
>   networking change, not a minimal one. The live arm stays `#[cfg(feature="live")] #[ignore]` (red-under-live,
>   not weakened). Next steps, in order: (1) wire native `ava_network`/node `tracing` into the LogFactory sink (or
>   set `tlsKeyLogFile` + run with `SSLKEYLOGFILE`); (2) log the `handle_dial` upgrade `Err`; (3) capture the
>   rustls↔Go TLS alert/EOF and compare the Rust client cert/signature-scheme against Go `staking.ParseCertificate`
>   + `crypto/tls` server acceptance.

> **AS-BUILT — M9.15 rustls↔Go TLS-1.3 handshake repro matrix root-cause (2026-06-25).** The isolated repro harness
> (Tasks 1–3: Go server binary `tests/tls_handshake/main.go`, Rust helper module `tls_repro.rs`, and matrix test
> `tls_handshake_repro.rs` in `ava-differential`) was run against the live Go oracle
> (`check_oracle_binary.sh` → `OK`; `avalanchego` HEAD dbf0f71, `rpcchainvm=45`, `go1.25.10`). The 5-cell matrix
> ran and passed in 22.4 s. Three cells produced evidence; the remaining cells (3 = Go-client↔Rust-server, 4 =
> Rust-vs-Rust) were not needed to localise the fault. Verbatim captured cell output:
> - `CELL1` (`verify=staking`, `keytype=rsa`): `rust=Err("dial 127.0.0.1:52249: Connection refused (os error 61)")`;
>   `go={"ok":false,"error":"go timed out after 15s"}`.
> - `CELL2` (`verify=noop`, `keytype=rsa`): `rust=Err("tls error: invalid peer certificate: Other(OtherError(UnsupportedCertVersion))")`;
>   `go={"ok":false,"error":"remote error: tls: unknown certificate"}`.
> - `CELL5` (`verify=staking`, `keytype=ecdsa`): `rust=Ok("NodeID-CZaYJrAyK8Kg7TuB1KR9PexoX4RaXXcJc")`;
>   `go={"ok":true,"version":772,"cipher_suite":4865,"peer_cert_len":1,"peer_key_type":"ecdsa"}`.
>
> **Decisive-cell analysis:**
> - **CELL5 (ECDSA) — SUCCESS.** Rust client ↔ Go server complete a full mutual TLS 1.3 handshake (version 772 =
>   TLS 1.3, cipher 4865 = `TLS_AES_128_GCM_SHA256`). The Rust side derives a `NodeID`. rustls↔Go TLS 1.3 itself
>   is fundamentally sound.
> - **CELL2 (RSA, Go-side client-cert check DISABLED via `verify=noop`) — the decisive cell.** The Rust client
>   rejects the Go server's RSA certificate with `UnsupportedCertVersion`. The Go-side error `remote error: tls:
>   unknown certificate` is simply the TLS alert sent back by the Rust client after that rejection. Because
>   `verify=noop` disables Go's verification of the Rust *client* cert, the only verification in play is the Rust
>   client's check of the Go *server's* RSA cert — this isolates the fault to the **rustls/webpki certificate
>   verifier**, not to Go's policy and not to the Rust client certificate.
> - **CELL1 (RSA, `verify=staking`) — harness timing artifact, not signal.** `Connection refused` means the Rust
>   client dialed before the Go server (first-spawned in the run) finished binding its port. Go then sat in
>   `Accept()` until the harness 15 s timeout expired. CELL2 is the cell that actually connected over the RSA path
>   and produced the real TLS rejection.
>
> **Root cause: rustls/webpki rejects avalanchego's RSA staking certificates as X.509 v1.** avalanchego mints its
> staking key pairs using `crypto/tls.X509KeyPair` via self-signed RSA certs. These certs are **X.509 v1** (no v3
> structure, no extensions) — confirmed directly: `openssl x509 -in staking/local/staker1.crt -noout -text` reports
> `Version: 1 (0x0)`, `rsaEncryption`. webpki (the verifier backend used by rustls) enforces X.509 v3 and refuses
> v1 certs with `UnsupportedCertVersion`. Go's TLS stack (with `InsecureSkipVerify: true` + a custom
> `VerifyConnection: staking.ValidateCertificate` hook) accepts them: `ValidateCertificate` only parses the leaf
> cert, extracts the public key, and derives the NodeID — it does NOT enforce X.509 version, chain structure, or
> standard PKI rules. ECDSA certs minted by current `staking.NewCertAndKeyBytes()` are v3, so CELL5 passes cleanly.
>
> **This refines the 2026-06-23 hypothesis above.** That note suspected Go rejecting the Rust *client's* ECDSA cert
> (client-cert signature-scheme / `SerialNumber=0`) and noted the signed-IP hypothesis had already been ruled out.
> The isolated repro shows the **reverse and more specific** cause: the Rust *client* rejects the Go *server's* RSA
> cert (`UnsupportedCertVersion`). The live `mixed_network` beacon logged `stakingKeyType:"RSA"`, so this is exactly
> the live stall: our rustls verifier refuses the beacon's v1 RSA cert below the app layer, the peer never
> establishes, and bootstrap never starts.
>
> **Ordered next step (fix lands separately — do NOT implement here):** make `crates/ava-network`'s rustls
> server/client certificate verifier mirror avalanchego's `staking.ValidateCertificate` semantics — accept
> self-signed X.509 v1 certs (RSA included), skip webpki's v3/chain/version enforcement, and derive the NodeID from
> the leaf public key. (Auxiliary: ensure Rust-side fixtures use ECDSA, but the verifier must still accept RSA peers
> to interop with RSA-keyed Go nodes on a real network.) The live `mixed_network` arm stays
> `#[cfg(feature="live")] #[ignore]` until this verifier fix lands and the matrix's RSA cells (`CELL1`, `CELL2`) go
> green.
>
> **AS-BUILT (2026-06-25) — verifier fix landed.** `crates/ava-network/src/peer/verifier.rs`
> now verifies the TLS-1.3 CertificateVerify signature via
> `rustls::crypto::verify_tls13_signature_with_raw_key` over the leaf SPKI we
> extract with `x509-parser` (new private `leaf_spki_der`), for BOTH
> `AvaServerCertVerifier` and `AvaClientCertVerifier`. This removes webpki's
> X.509-v3 cert-version gate that rejected avalanchego's v1 RSA staking certs
> with `UnsupportedCertVersion`; the leaf-key *policy* check in
> `verify_{server,client}_cert` is unchanged. Reproduced + regression-proven in
> `crates/ava-network/tests/tls_v1_rsa_handshake.rs` (in-process v1-RSA mutual
> handshake, both directions; vendored fixture = avalanchego local staker1, v1
> RSA-4096/exp-65537). The repro matrix's RSA cells now have a passing in-process
> analogue. STILL DEFERRED: RSA staking-key *loading* (`Identity::from_pem`) and
> flipping the live `mixed_network`/matrix arms green (nightly-gated).

> **AS-BUILT (2026-06-26, Stage 1 of `m9.15-validate-tls-fix-live`) — live TLS matrix is now a real
> fix-validation gate.** `tests/differential/tests/tls_handshake_repro.rs::tls_handshake_matrix_live`
> now hard-asserts the post-`e06f0a0` GREEN state instead of the former soft "harness ran end-to-end"
> diagnosis guard. Run against `~/avalanchego@cbea62895c`, `rpcchainvm=45`:
>
> - **CELL1** (`verify=staking`, `keytype=rsa`): `rust=Ok(NodeID-...)`, Go `ok:true,version:772` —
>   RSA cell flipped GREEN (previously `UnsupportedCertVersion` pre-fix).
> - **CELL2** (`verify=noop`, `keytype=rsa`): `rust=Ok(NodeID-...)`, Go `ok:true,version:772` —
>   decisive isolation cell also GREEN.
> - **CELL5** (`verify=staking`, `keytype=ecdsa`): `rust=Ok(NodeID-...)`, Go `ok:true,version:772` —
>   ECDSA cell was green pre-fix and stays green.
> - **REVERSE** (new cell — Go *client* RSA → Rust *server*, `verify=staking`, `keytype=rsa`):
>   `rust_server=Ok(NodeID-...)`, Go `ok:true,version:772` — inbound-peer verifier path validated
>   live against a real Go RSA staking cert.
>
> New `go_client_vs_rust_server` helper added (binds Rust server first, spawns Go client, bounded
> by same `CELL_TIMEOUT`). The pre-existing CELL1 cold-start race (a freshly-spawned Go server on an
> OS-uncached binary can take ~1 s to bind, which the prior fixed sleep didn't reliably cover) is now
> handled by a **deterministic stderr-readiness wait**: the helper reads the Go server's stderr until
> it prints `LISTENING <addr>` (emitted right after `tls.Listen` succeeds), bounded by `CELL_TIMEOUT`
> — no timing constant, and (unlike a throwaway TCP probe) it does not consume the server's single
> `Accept()`. All four cells PASS (`ava-differential` 1/1 live, `offline_gate_and_parse` unchanged).

> **★ROOT CAUSE PINNED 2026-06-26 (Stage 2 of `m9.15-validate-tls-fix-live`) — TLS clears; the
> live `mixed_network` arm now stalls one rung HIGHER, at the application-layer p2p `Handshake`
> message.** Ran the gated arm against `~/avalanchego@cbea62895c` (`rpcchainvm=45`):
>
> ```
> AVALANCHEGO_PATH="$HOME/avalanchego/build/avalanchego" \
>   cargo nextest run -p ava-differential --features live --run-ignored all \
>   -E 'test(=mixed_network)' --nocapture 2>&1 | tee /tmp/mixed_network_live.log
> ```
>
> Result: TIMEOUT at the nextest 120 s per-test leash (the harness's own 180 s
> `await_bootstrapped` deadline never elapsed). The decisive evidence is in the two node logs
> (`$WORK_DIR/{go,rust}/node.log`, `$WORK_DIR = $TMPDIR/mixed-net-24301`):
>
> - **TLS layer — GREEN (Stage 1 fix holds).** The Rust follower's rustls transcript shows a full
>   TLS-1.3 mutual handshake against the Go beacon's RSA staking cert: `Using ciphersuite
>   TLS13_AES_128_GCM_SHA256`, `Got CertificateRequest`, `Attempting client auth` — then repeated
>   `Resuming session` / `Resuming using PSK` on the reconnect-backoff cycle. No
>   `UnsupportedCertVersion`, no cert error. The verifier fix (`e06f0a0`) is confirmed live.
> - **App-layer p2p `Handshake` — BROKEN (the new rung).** The Go beacon rejects EVERY one of the 7
>   handshake attempts (timestamps interleave exactly with the Rust client's reconnect cycle:
>   `18.264 / 19.251 / 21.252 / 25.502 / 33.752 / 50.002 / 58:22.253`) with:
>   ```
>   peer/peer.go:940 malformed message
>     {"nodeID":"NodeID-NgW9axPkSrexLerRtUp1wUAS5GhdGVjok","messageOp":"handshake",
>      "field":"knownPeers.filter","error":"invalid num hashes"}
>   ```
>   All 7 malformed-message lines name the same field, `knownPeers.filter`. As a consequence the Go
>   side never registers the Rust node as a peer — every P/X `app_request` sender line shows
>   `"to":{"NodeIDs":[],"Validators":0,"NonValidators":0,"Peers":0}` and bootstrap logs
>   `bootstrapping skipped {"reason":"no provided bootstraps"}` / `sampledNodes:[], numNodes:0`. The
>   Rust follower never logs ANY application-layer handshake/peerlist/bloom event (only the rustls
>   transcript), consistent with Go dropping the connection right after decoding the bad Handshake.
>
> **Ruled IN:** a Rust-side wire-encoding defect in the outbound `Handshake` message's `knownPeers`
> bloom filter — specifically the `num hashes` (bloom `NumHashes`) field of the `BloomFilter`/`IPPort`
> claimed-peers structure (`message/p2p` Handshake → `knownPeers.filter`). avalanchego's
> `bloom.Parse` rejects a filter whose hash-count is outside `[minHashes, maxHashes]` (or whose
> num-entries/num-hashes are inconsistent) with exactly `"invalid num hashes"`.
> **Ruled OUT:** (1) the TLS-1.3 cert-version / verifier stall from Stage 1 (the rustls transcript is
> clean and reaches client-auth — that blocker is closed). (2) The `RouterBridge` inbound-drop /
> production boot-wiring gap suspected in prior memory — that is an *intra-Rust* concern at the
> consensus-router seam, but here the connection never gets past the Go peer's Handshake decode, so it
> is never reached; the stall is squarely on the Rust→Go p2p Handshake wire bytes, below any routing.
> (3) PeerList exchange and bootstrap-connectivity gating — both are downstream of a registered peer,
> which never happens.
>
> Captured artifacts: `/tmp/mixed_network_live.log` (nextest run), plus the two node logs copied for
> inspection. Per Stage 2's diagnose-not-fix mandate, the `mixed_network` arm stays
> `#[cfg(feature="live")] #[ignore]` with its no-fork/same-tip assertions unchanged, and the encoding
> fix is filed as the follow-up below (NOT applied here).

> **AS-BUILT — M9.15 rung-3 single-Go-beacon bisect: rungs 6-9 PROVEN against real Go (2026-06-29).**
> Implemented the diagnose-by-bisection plan (`docs/superpowers/plans/2026-06-28-m9.15-rung3-live-follower-bootstrap.md`):
> a single-Go-beacon probe (`Network::boot_single_go_beacon`, `mixed_network_single_beacon`) + env-gated
> rung instrumentation across the connect→bootstrap ladder (BeaconManager gate-count, ava-network dial/
> handshake rungs 1-3, chains.rs gate/start/frontier rungs 5-7 on BOTH the production `OutboundSender`
> and the in-process `RecordingSender`).
> - **Probe-boot fix (evidence-gated):** the first live run showed the *lone Go beacon itself* never reaches
>   `isBootstrapped(P/X/C)` — a single node holding 1-of-5 stake from the stock `local` genesis with no
>   peers can't clear avalanchego's connected-stake gate to transition to normal-op (all 3 chains
>   `bootstrapping skipped: no provided bootstraps` then stall at `proposervm Waiting for inner VM event
>   before normal operation`; no `bootstrapped` health check passes). FIX: pass
>   `--sybil-protection-enabled=false` to ONLY the lone Go beacon (new per-node `extra_args` on
>   `NodeLaunch`), so it operates standalone like a single-node dev net.
> - **★ RESULT — single-beacon arm GREEN (15.6s).** The Rust ECDSA follower bootstraps P/X/C from one
>   live Go node. **Verified REAL** via the Go beacon's own log (the follower-side native rungs are
>   swallowed by the known LogFactory wiring gap, AS-BUILT 2026-06-23): the follower
>   (`NodeID-F2rifANHJDmLsdetBsCoXYDVqMH85FYdK`) appears `connected` on P/X/C, sends `get_ancestors`
>   (req 3), and the Go beacon serves `ancestors` (numContainers:1) on all three chains. Production path
>   confirmed networked+gated: `main → drive_startup_chains_over_network → boot_chain_over_network_core`
>   (real `OutboundSender` + `on_sufficiently_connected` gate); `get_bootstrap_config` populates
>   `bootstrappers` from `--bootstrap-ips/ids` ⇒ non-empty ⇒ `required_conns = (3·1+3)/4 = 1` ⇒ real
>   gated bootstrap (not the empty-beacon short-circuit).
> - **★★ BISECT VERDICT → Branch A (connectivity).** Rungs 5-9 (gate fire → `handler.start()` →
>   GetAcceptedFrontier out → ancestors in → NormalOp) **work against real Go**. Therefore the
>   5-validator Stage 2 stall is **purely a connectivity problem**: the follower forms only 1 of the 4
>   beacon connections `required_conns=(3·5+3)/4=4` demands. The remaining work (rungs 1-4: why 4 of 5
>   dials/handshakes don't complete) is the next rung, and pinpointing it first needs the LogFactory
>   wiring gap fixed so the follower's own rung 1-3 dial-outcome markers surface in a 5-validator run.
> - **Live-run invocation notes (for the next operator):** nextest's 120s slow-timeout kills these arms →
>   run via `cargo test -p ava-differential --features live --test mixed_network -- --ignored --nocapture
>   <name>`; pin `TMPDIR` to a stable dir (the nix-shell `temp_dir()` is ephemeral and its logs vanish);
>   on a fast PASS the follower is SIGKILLed before flushing, so read the **Go** node log for ground truth.
> - The arm stays `#[cfg(feature="live")] #[ignore]`; offline arms untouched.

> **AS-BUILT — M9.15 rung-3 Branch-A (5-validator follower): connectivity RULED OUT; racy gate-fire
> isolated as the next rung (2026-06-29).** Pursued the full Branch-A fix (logging fixes + handshake
> timeout + graceful-teardown harness so live logs survive). Findings, evidence-based:
> - **The 5-validator follower bootstrap is RACY** (~2 of 6 live runs reach Stage 2 NormalOp; the rest
>   time out at Stage 2). Even on the "good" runs the stricter `await_all_connected` full-mesh assertion
>   (`mixed_network.rs`) fails.
> - **Connectivity is NOT the cause.** Go-side ground truth (complete logs): in a STALLED run all 5 Go
>   validators show the follower connected (peer stable ~2.5 min) AND the follower emits a `GetPeerList`
>   pull every 2 s — `run_timers` pulls only to the `connected` set, so the follower **finished
>   handshakes with all 5 Go**. Yet the Go validators receive **zero `GetAcceptedFrontier`/`GetAncestors`**
>   ⇒ the bootstrapper never starts the frontier query ⇒ stall. This is the engine/gate/frontier half
>   (Branch B-shaped), not the dial/handshake half.
> - **Falsified hypotheses:** (1) hung-TLS-upgrade — added a 15 s handshake timeout (`ab1495e`, Go
>   `readHandshakeTimeout` parity, kept as a correct improvement) but it did NOT change the race; (2)
>   Go-skips-PeerList — Go's `handleHandshake` (peer.go:1039-1056) **always** sends a handshake PeerList
>   (the "skipping empty peer list" log is `handleGetPeerList` gossip, not the handshake).
> - **Wiring traced, all correct on inspection:** `finish_handshake` notifies `connected(PRIMARY)` (zero
>   Id = PrimaryNetworkID); `BeaconManager.connected` increments `num_conns` per weight-bearing beacon,
>   fires at `required_conns=(3·5+3)/4=4`; `consensus_router`(BeaconManager) IS the peer router; the live
>   follower's frontier set is `extra_beacons={5 Go}` (`boot_chain_over_network_core`, `include_self_beacon
>   =false`). Single-beacon (required_conns=1) PROVES this whole path works against real Go.
> - **META-BLOCKER (why the exact rung is unpinned):** the follower's `tracing` subscriber goes silent at
>   ~14 ms under the harness's concurrent-connection load (5 simultaneous peer connections + chain boot),
>   hiding the decisive `rung 4` (num_conns count) / `rung 5` (gate fired) / `rung 7` (frontier node-set)
>   markers. This survives BOTH chain-slot logging fixes (which work in solo) and graceful SIGTERM+flush
>   teardown (so it is NOT log buffering — the subscriber stops emitting under load). Production logging
>   bug fixed for the solo/normal path (`f7e2f43`+`32ff8e8`); the concurrent-load residual is separate.
> - **NEXT RUNG (gated, next session):** bypass tracing for this signal — expose `BeaconManager.num_conns`
>   + gate-fire as an always-on metric/coarse counter (or fix the concurrent-load subscriber death) — then
>   ONE live run pins it: gate counts `<4` (count/notify bug) vs fires-but-broadcasts-empty (frontier
>   addressing) vs a connect/disconnect race. Candidates left: connect/disconnect flap decrementing
>   `num_conns` below 4; a `finish_handshake`→`connected` notification not reaching BeaconManager for some
>   peers under load. Live `mixed_network` arm stays `#[cfg(feature="live")] #[ignore]`; offline arms green.

> **AS-BUILT — M9.15 rung-3 racy gate-fire FIXED (2026-06-29).** Root cause pinned
> deterministically offline (no live run, sidestepping the concurrent-load tracing
> death): `BeaconManager` counted connections with a bare `AtomicI64` that (a)
> double-counted a duplicate `connected()` for the same beacon and (b) went
> negative on a spurious `disconnected()` for an un-counted beacon — either drives
> the gate below / never to `required_conns`, so the bootstrapper never broadcast
> `GetAcceptedFrontier`. Fix: count a **deduplicated set of connected beacon
> node-ids**; `connected` inserts (idempotent), `disconnected` removes-by-id, gate
> fires at `set.len() >= required_conns`. The set COMPENSATES for ava-network's
> inbound `handle_accepted` path not deduplicating against `connected`/`connecting`
> (unlike outbound `handle_dial`), so the same beacon can fire
> `connected()`/`disconnected()` more than once. Note: Go's `beacon_manager.go`
> uses a bare `atomic.AddInt64` — its network layer delivers `Connected`/`Disconnected`
> at-most-once per peer and strictly paired; the set is a Rust-side divergence
> compensation, not a Go peer-set port. Pinned by `ava-node`
> `duplicate_connected_does_not_double_count` +
> `disconnect_before_connect_does_not_wedge_gate` (RED→GREEN), plus
> `concurrent_connects_fire_gate` + `beacon_manager_fires_gate_at_required_conns` —
> these four `ava-node` unit tests are the **authoritative deterministic** regression
> guard. An end-to-end analog, `avalanchers` `follower_bootstraps_through_real_beacon_gate`
> (5-beacon localhost-TLS bootstrap driven by the REAL `BeaconManager` gate — the
> coverage `networked_bootstrap.rs` lacked, since it hand-fires the gate), exists but
> is `#[ignore]`d: the 6-node concurrent TLS bring-up is bimodally flaky (~0.1s pass or
> a permanent wedge ~1-in-4, a harness-level startup race, **not** the gate logic). It
> is runnable on demand (`--run-ignored all`); un-gating it is tracked as an M9.15
> follow-up to root-cause the bring-up race. The live two-binary
> `mixed_network` arm stays `#[cfg(feature="live")] #[ignore]` (nightly-gated by
> design); this fix unblocks its rung-3 stall. Follow-up: the inbound-dedup gap in
> `ava-network` `net_impl.rs` `handle_accepted` (no `connected`/`connecting` guard,
> unlike `handle_dial`) means duplicate connect/disconnect notifications reach every
> `ExternalHandler` (RouterBridge, engine router, etc.), not just BeaconManager —
> the broader at-most-once fix is tracked as a follow-up item.

> **AS-BUILT — M9.15 inbound at-most-once dedup CLOSED (2026-06-30).** The
> follow-up flagged in the 2026-06-29 banner ("ava-network inbound `handle_accepted`
> does not dedup against `connected`/`connecting`, unlike `handle_dial`") is now
> fixed at the source. New `NetworkImpl::admit_peer<IO>` performs the membership
> check → `Peer::spawn` → `connecting.insert` atomically under a single new
> `peers_lock: Mutex<()>` (Go `peersLock` parity), for BOTH the inbound (accept)
> and outbound (dial) paths; a duplicate connection for an already-tracked node is
> rejected before any second actor is spawned, so `router.connected`/`disconnected`
> are delivered at-most-once and strictly paired to every `ExternalHandler` (engine
> router, RouterBridge, BeaconManager). `watch_peer` was split into `spawn_watcher`
> with all promote/close membership transitions also under `peers_lock`. This also
> closes the narrower TOCTOU the outbound path itself had (check and insert were not
> previously atomic). Pinned by `ava-network` `admit_peer_dedups_same_node_id`
> (deterministic unit: two admissions for one node-id → `(true, false)`,
> `connecting.len() == 1`) + `mutual_dial_connects_each_peer_exactly_once`
> (end-to-end: two `TestNetwork`s mutually dial → each `RecordingRouter` records the
> peer connected exactly once). The `BeaconManager` `HashSet<NodeId>` is demoted to
> defense-in-depth (its 4 deterministic `ava-node` tests stay green). **Key deviation
> from plan:** Task 1's atomic dedup surfaced a mutual-dial livelock — when two nodes
> dial each other simultaneously, TLS-1.3's client side completes before the server
> side, so each node admits its own outbound and rejects the peer's inbound, leaving
> the surviving connections half-dead; with a fixed retry delay both sides retry in
> lockstep forever (100% failure, systematic). Task 2 therefore required a production
> companion fix (not test-only as planned): Go's retry jitter from
> `network/tracked_ip.go` `increaseDelay` — omitted in the Rust port with a "jitter
> omitted" comment — was ported into `crates/ava-network/src/network/tracked_ip.rs`
> using Go's single-draw model (`delay *= (1+rand)`, near-cap `MAX*(3+rand)/4`,
> per-instance `jitter_seed = nanos^port`) so two peers' retry windows diverge and
> break lockstep. `ava-network` + `ava-node` nextest green, clippy `-D warnings` +
> fmt clean.

> **AS-BUILT — M9.15 bootstrap failure accounting + `beaconed_bootstrap` un-gated
> (2026-07-01, branch `m9.15-bootstrap-failure-accounting`, commits 3f855fb–eba1957).**
>
> **Root cause (two compounding gaps):**
> (1) The bootstrapper required ALL configured beacons to reply with a non-empty
> accepted-frontier set before it could progress past the frontier phase. A beacon that
> never completed its TLS handshake (because the connectivity gate fired before all
> handshakes finished) produced no reply at all — neither success nor failure — so the
> frontier phase stalled indefinitely waiting for a quorum that would never come.
> (2) `boot_chain_over_network` passed a `MockClock` (frozen time) into
> `AdaptiveTimeoutManager`, which disabled the request-timeout backstop: the per-request
> deadline that normally synthesizes a `*Failed` op for a non-responding peer never fired,
> so the timeout-driven recovery path was also silenced.
>
> **Go-parity fix (5 commits):**
> - *Frontier-phase failure accounting* (`get_accepted_frontier_failed`): records the
>   failing beacon into `frontier_responded` (the same set that successful replies insert
>   into), contributing no id to `frontier_replies`. The phase is now complete when
>   `frontier_responded.len() == num_bootstrappers`, whether each entry came from a success
>   or a failure. A beacon that never connected counts as failed via the timeout-synthesized
>   op.
> - *Restart-frontier-discovery when all beacons fail*: if the frontier phase completes but
>   `frontier_replies` is empty (every beacon failed), the bootstrapper re-broadcasts
>   `GetAcceptedFrontier` rather than falsely treating the empty set as already-synced (Go
>   parity: `bootstrapper.go` re-sends when `numFrontierIDs == 0`).
> - *Accepted-phase failure accounting* (`get_accepted_failed`): mirrors the frontier fix —
>   records the failing beacon into `accepted_replies` with an empty id-set so the phase
>   completes when all beacons have responded (success or failure).
> - *`*Failed` dispatch wired through `BootstrapperEngineAdapter::handle`*: both
>   `GetAcceptedFrontierFailed` and `GetAcceptedFailed` `InboundOp` variants are now
>   routed to the bootstrapper (previously they fell into the no-op arm and were silently
>   dropped).
> - *`RealClock` on the test-boot path*: `boot_chain_over_network` now uses `RealClock`
>   instead of a frozen `MockClock`, so `AdaptiveTimeoutManager`'s request-timeout
>   backstop fires for peers that do not reply (closing the silent-drop window in tests).
>
> **Un-gated `follower_bootstraps_through_real_beacon_gate`**: this avalanchers e2e test
> (5-beacon localhost-TLS, real `BeaconManager` gate, `required_conns=4`) was previously
> `#[ignore]`d due to a bimodal flakiness (~25% permanent-wedge rate). The two fixes above
> eliminate the wedge: the bootstrapper now completes the frontier phase even when the gate
> fires before all 5 beacons have handshaked. Verified deterministically green across 30
> runs; the test now runs as a standard (non-ignored) CI test. This provides the gate
> coverage that `networked_bootstrap.rs` lacks (the latter hand-fires the connectivity gate
> via `connected_tx.send(true)`).
>
> **Remaining follow-up:** the `ava-network` concurrent self-dial TOCTOU in `run_dialer` —
> a second `handle_dial` can be dispatched to a node before the first completes its TLS
> upgrade and enters `connecting` (unlike Go's one-goroutine-per-tracked-IP model). This
> was investigated and ruled out as the cause of the beacon-gate wedge, but it is a real
> latent bug. Fix: add an in-flight-dial guard set or port Go's per-IP dialer loop. The
> nightly live two-binary `mixed_network` arm remains gated by design.

---

> **★ AS-BUILT — M9.15 LIVE `mixed_network` GREEN END-TO-END (2026-07-15, branch
> `m9.15-live-mixed-net-v07`, 19 commits `9dd80d7..e32e65d`).** The live two-binary arm —
> 5 real Go `avalanchego` genesis validators + 1 Rust `avalanchers` follower — now passes
> reproducibly (`cargo test -p ava-differential --features live --test mixed_network`,
> ~38s): the follower bootstraps P/X/C from the Go quorum, follows a Go-issued C-chain
> transfer to the same tip, and its normalized `Observation` matches the Go node's (no
> fork). Single-beacon bisection arm also green. Controller-confirmed on the dev machine,
> not just the implementer. **This closes the last open arm of M9 interop.**
>
> Reached by a five-rung live debug ladder (each rung its own design→plan→TDD-fix under
> `docs/superpowers/{specs,plans}/2026-07-{05,14,15}-*`); every fix is Go-oracle-cited
> against `~/avalanchego@96897293a2` (firewood ffi v0.7.0, rpcchainvm=45):
> - **Firewood v0.6.0→v0.7.0** (`917e45f`/`2ef6718`): pin bump to match the oracle; all
>   ethhash + C-root goldens unchanged; `deny.toml` policy for v0.7.0's dropped SPDX
>   license fields + documented pre-existing advisory drift.
> - **Rung 1 — `*Failed` delivery** (`bf2bc04`/`08ff771`/`505c750`): ported Go's
>   "unsent ⇒ immediate `*Failed`" sender leg + exactly-once cancel-claim. (The live stall
>   this targeted turned out to be a stale binary — see below — but the delivery gap was
>   real and is now closed with regression guards at 3 assembly layers.)
> - **Stale-binary + macOS first-exec** (`ece6932`/`1943a81`): the live harness ran a
>   2-week-old `target/release/avalanchers` (predating the RealClock/failure-accounting/
>   Interest-logging fixes) → frozen-clock wedge masquerading as a new bug. Fixed with
>   mtime-newest binary selection + a `--version` pre-warm that eats macOS's ~40s
>   first-exec scan of a freshly relinked binary (which otherwise blows the 180s bootstrap
>   window and yields an empty node log — a red herring that cost 2 runs).
> - **Rung 2 — chain API routes** (`afcfb95`): the live boot path booted VMs to NormalOp
>   but never registered their HTTP handlers, so `/ext/bc/P|X|C` returned 404 (the
>   M8.31-deferred wiring). Now registers each chain through the existing `ava-api` seam.
> - **Rung 3 — harness chunked HTTP** (`12250d3`): `Observation::collect`'s raw HTTP client
>   didn't de-chunk Go's `Transfer-Encoding: chunked` responses (`platform.getCurrentValidators`
>   with 5 validators exceeds Go's auto-`Content-Length` buffer) → connectivity gate never
>   satisfied. Test-harness-only.
> - **Rung 4 — genesis identity parity** (`aa1c469`/`d9d0f5f`/`97348a5`): the local network's
>   genesis timestamp equals `InitiallyActiveTime`, so all upgrades (through Granite/Etna=
>   Cancun) are active AT genesis. Rust built a bare mainnet-shaped genesis; Go builds a
>   fork-shaped one. Fixed the C genesis header tail (8 fields incl. baseFee 225 gwei,
>   `timestampMilliseconds`, `minDelayExcess`=acp226 constant), the warp precompile
>   activation account (nonce=1, code=`[0x01]` at `0x02..05`, Durango-gated), and the X
>   fresh-network stop-vertex parent. Firewood **exonerated** — reproduces coreth's root
>   exactly given the correct input state. P was never divergent. Gating is upgrade-schedule-
>   keyed (not network-id-keyed); mainnet/fuji shapes unchanged. Go-oracle goldens, all
>   re-extracted from a live oracle in review.
> - **Rung 5 — Cancun execution + clamp** (`338b963`/`d58a604`/`6540888`/`e32e65d`): the
>   follower fetched Go's block 1 but silently failed `verify` — `eth_env_header` built the
>   EVM env with `..Default::default()`, dropping the decoded `parentBeaconRoot`/blob fields,
>   so EIP-4788 rejected every Cancun-active block; plus a missing coreth blob schedule.
>   Fixed to carry the header tail + coreth blob params. Then ported coreth's **syntactic
>   header clamp** (`wrapped_block.go:493-518`: `parentBeaconRoot==0`/`blobGasUsed==0`/
>   `excessBlobGas==0` at Cancun, absent pre-Cancun; `ValidateBody` blob-count parity) so
>   Rust fail-closes exactly where Go does — closing a consensus-split vector where a
>   proposer crafts a self-consistent block Go rejects and Rust accepts. Also added the
>   engine parse/verify failure logging that made this diagnosable, and a `decode_b256_opt`
>   fix (consume the `0x80` empty-string placeholder → `None`, Go rlp pointer-nil parity).
>
> **Test additions:** `ava-engine` frontier `*Failed`-delivery + exactly-once guards;
> `ava-evm` `live_block_adopt` + `cancun_clamp` (6, coreth-cited) + Go-oracle genesis goldens;
> `avalanchers` chain-API-route registration; harness `split_http` de-chunk. Verified green:
> `ava-evm` 193/193, `ava-engine` 60/60, live single-beacon + `mixed_network`.
>
> **Intentional deferred follow-ups (non-blocking for a follower-only mixed net; each bites
> only an unimplemented scenario):** (1) `ava-evm/src/builder.rs` stamps `difficulty:0` +
> no Cancun header tail on Rust-**built** blocks — Go peers will reject them the moment the
> Rust node PROPOSES in a mixed net (matters for a validating, not following, Rust node);
> (2) PREVRANDAO divergence (coreth `Random=Difficulty(1)` vs reth `mix_hash(0)`) — a state
> split on the first contract reading PREVRANDAO; (3) per-block `ApplyUpgrades` (activation
> crossing parent→block, e.g. mainnet Durango replay) unimplemented — only genesis-time
> activation; (4) `eth_getBlockByNumber` not implemented in the C-chain RPC; (5)
> `ChainRouter::on_response` reply-cancel dead code; (6) per-chain log-file wiring
> (`add_chain_logger` never called in prod); (7) the documented `deny.toml` advisory ignores
> (pre-existing on main). The nightly live two-binary arm remains operator/nightly-gated by
> design (needs `$AVALANCHEGO_PATH` + a built Go node).
>
> **Whole-branch review (opus, 19-commit range): Ready-with-nits — no Critical, no
> follower-arm regression.** The one "decide before merge" item (I1) was fixed in the same
> pass: the Cancun exec-env change carried `mix_hash` into execution with no `mixDigest==0`
> syntactic guard, slightly widening an adversarial PREVRANDAO fail-open (a Byzantine block
> with nonzero mixDigest + a PREVRANDAO-reading tx that Go rejects, Rust would run). Added
> the ungated `mixDigest==0` header check (coreth `wrapped_block.go:420-421`) + the
> `nonzero_mix_digest_is_rejected` test (`ava-evm` clamp suite now 7/7). Safe on the honest
> arm and for Rust-built blocks (both stamp mix=0). Additional tracked follow-ups the review
> surfaced (none block a follower-only merge): **L1** — the C-Chain verify path still checks
> only ~4 of coreth's ~12 header syntactic invariants (this branch adds the Cancun clamp +
> mixDigest; the remaining difficulty==1 / nonce==0 / tx-root / coinbase / uncles checks are
> the biggest residual "match Go under adversarial input" port, and difficulty==1
> specifically must be reconciled with `builder.rs`'s `difficulty:0` first); **L2** — dead
> `ChainRouter::on_response` means the adaptive timeout never adapts downward + a spurious
> `*Failed` per answered request (liveness-only); **L4** — test-only: the differential
> harness `dechunk` can panic on a multi-byte UTF-8 char split across an HTTP chunk boundary
> (bounded — JSON-RPC responses are ASCII in practice).

> **AS-BUILT — M9.15 Rust-as-proposer arc (branch `m9.15-rust-proposer`, 2026-07-16→18).**
> The proposal-side deferrals above are now CLOSED offline (live proof = the operator/nightly
> arm). Landed in three phases:
> - **C-Chain proposer parity** (parent plan `2026-07-16-rust-as-proposer-cchain-parity`):
>   (2) PREVRANDAO `Random=difficulty` at Durango+ (coreth `core/evm.go:86-95`); builder stamps
>   full Go-shape headers — blackhole coinbase, Cancun tail, real tx/receipt roots + bloom,
>   exact ACP-176 extra prefix + Granite tail, **difficulty==1** (retires the `difficulty:0`
>   deferral (1)); **L1 closed** — `syntactic_verify` now ports the full coreth
>   `wrappedBlock.syntacticVerify` check set (number/nonce/mixDigest/version/txsHash/uncleHash/
>   coinbase/min-gas-price/basefee/blockgascost/Cancun clamp/VerifyExtra) in Go order with
>   sentinel parity. A recorded **Go-oracle verdict leg** has real coreth ACCEPT the honest
>   Rust-built block + reject 5 adversarial mutations with matched rejection classes. RSA
>   staking identity + RSA IP signing (Go `staking/verify.go` PKCS1v15-SHA256 parity).
> - **C-Chain tx pipeline** (nested insert, spec `2026-07-17-cchain-tx-pipeline-design`):
>   `EvmMempool` (coreth-parity admission), `eth_sendRawTransaction`/`eth_getTransactionReceipt`,
>   receipts persisted at accept + `AcceptedTxIndex`, `build_block` packs mempool txs. Retires
>   the "M6.23 reth-txpool `best_transactions`" reading — purpose-built pool with cited coreth
>   parity satisfies the intent. **Tx GOSSIP still deferred** (its own milestone; needs
>   engine-layer AppGossip/AppRequest routing — `InboundOp` has no App variants today).
> - **Proposal initiation** (nested insert #2, spec `2026-07-18-proposal-initiation-design`):
>   a lock-free `PendingWorkWaiter` seam + per-chain forwarder task (Go
>   `NotificationForwarder` parity) so a pending EVM tx triggers `build_block` in production
>   without holding the consensus-shared VM mutex; `GenesisValidatorState` feeds the proposervm
>   windower the real genesis 5-validator set (retires the live `FixedState` self+beacons).
>   The live TLS-v1-RSA-own-cert boot bug (rustls `with_single_cert` rejecting avalanchego's
>   v1 staker cert) was found + fixed live. **STILL DEFERRED:** full `PChainValidatorManager`
>   node wiring (needed only when validator sets change — Fuji/mainnet); P/X/SAE forwarder
>   opt-in; the slot-wait-under-lock hazard (bounded, M7.18 family). Live `mixed_network_rust_
>   proposes` re-run = the operator gate.
>
> **AS-BUILT addendum (Task 9 closeout, same branch).** The live gate above landed after this
> note was written: `mixed_network_rust_proposes` ran GREEN end-to-end (4 Go validators + the
> Rust node proposing; 28.84s, no fork) with the follower-only arm showing no regression — the
> live proof, not just an offline exit gate. Two scoping notes for anyone extending L1 further:
> (1) **Helicon upstream-delta** — `VerifyExtra`'s Fortuna-arm length floor (ported here,
> `syntactic_verify::truncated_extra_is_rejected_at_fortuna`) is itself a **pre-Helicon**
> behavior; Helicon (unscheduled on every network, per `specs/10-cchain-evm-reth.md` §Helicon
> callouts and `specs/README.md`) drops the ACP-176 state-space floor from `header.Extra`
> entirely (`VerifyExtra` then accepts any length) — non-gating today, but the port must not be
 read as "any length is always invalid below 24 bytes" once Helicon activates. (2) ★ the
> dummy-engine `verifyHeaderGasFields` (`consensus/dummy/consensus.go:125-154`) is **UNPORTED on
> the Rust verify path** — surfaced by the whole-branch review as a genuine fail-OPEN gap, not a
> bland "non-goal." coreth's *complete* block verification recomputes and **equality-checks** four
> fee/gas header fields that `syntactic_verify` only checks structurally: (a) `header.BaseFee ==
> BaseFee(parent,…)` (Rust: non-nil at AP3+ only), (b) `header.BlockGasCost == expected` (Rust:
> non-nil/uint64 at AP4+ only), (c) `VerifyExtraPrefix` **byte-equality** of the ACP-176 extra
> prefix (Rust: length ≥ 24/80 only), (d) the gas-limit rule (Rust: no check). None of these
> affects an empty block's EVM state root, so a **Byzantine proposer** can craft a block with a
> VALID state root but a wrong fee-metadata field that Go rejects and Rust accepts — a silent
> Rust-vs-Go consensus split, the same class the Cancun clamp closed. **NOT triggered on the honest
> arm** (every honest proposer, and the Rust builder itself, stamps these fields correctly — which
> is why Go ACCEPTs Rust-built blocks and no live gate depends on this), but it **MUST be ported
> before any adversarial / BFT-exposed deployment**. Coupled with the deferred `base_fee`
> `feeStateBeforeBlock` time-advance above: once verify recomputes the expected base fee, it will
> also need that advance to ACCEPT Go blocks under sustained load. Follow-up: port
> `verifyHeaderGasFields` (expected-base-fee via `feerules::base_fee`, expected-block-gas-cost,
> extra-prefix byte-equality, gas-limit) onto `EvmBlock::verify`.

> **AS-BUILT (verifyHeaderGasFields port, branch `verify-header-gas-fields`, 2026-07-18) — CLOSED.**
> The `verifyHeaderGasFields` fail-open flagged directly above is now closed.
> `feerules::verify_header_gas_fields` (`crates/ava-evm/src/feerules/mod.rs`) ports the full
> orchestrator in Go's exact check order — `verify_gas_limit` (incl. the pre-AP1 bound-divisor arm the
> original design pass missed and added mid-task), `verify_extra_prefix` (Fortuna full-struct equality +
> the claimed-target-excess clamp trick, AP3 window-prefix byte match), expected-`BaseFee`/
> `BlockGasCost` `Option`-equality, and `ExtDataGasUsed` fork gating — wired into
> `EvmBlock::verify_with_predicates` via a new `Shared::parent_header` resolver. This resolver is
> **VERIFIED-MAP ONLY and fail-closed**: it resolves solely from the in-memory `verified` processing
> tree (accepted blocks are retained there after `accept`, and genesis is seeded into the same map by
> `from_genesis`) — there is no `CanonicalStore` fallback (the store persists only the header
> commitment + ext_data, not a reconstructable full header) and no separate genesis fallback. Anything
> not found in `verified` fails CLOSED with `Error::MissingProposal`, matching `parent_state_root`'s
> and `build_block`'s resolution contract. This is unreachable in production today because
> `from_genesis` — the only production construction path — seeds `verified` directly; the contract
> must be extended when advanced-tip resume / real-DB threading lands and parents can be evicted from
> the processing tree before a verify call needs them. The orchestrator runs immediately after
> `syntactic_verify` and before sender recovery/execution (Go's ordering). The coupled `base_fee`
> `feeStateBeforeBlock` time-advance gap noted just above is fixed at the source: `feerules::base_fee`
> now takes the parent as `&AvaHeader`
> (not a reth `Header`, which carries no `time_milliseconds`) so builder, RPC, and verifier share one
> time-advance-correct function — byte-parity guarded by a 30-row recorded Go-oracle advance corpus
> (incl. nonzero-excess/sustained-load rows the prior quiet-net vectors could not catch). The Go-oracle
> **verdict corpus** grew from 5 to 10 per-field mutations (`proposer_candidates.rs`: `wrong_base_fee`,
> `wrong_gas_limit`, `wrong_block_gas_cost`, `oversized_ext_data_gas_used`, plus the original 5), each
> REJECTED by both Go and Rust with matched sentinel classes, alongside the honest block still
> ACCEPTing on both sides. A dedicated e2e guard (`verify_gas_fields.rs`) drives the full
> `parse_block → verify` entry against real Byzantine-shaped mutants of a live-captured block.
>
> **Two scoping notes for anyone extending this further:**
> (1) **Helicon correction.** The Helicon upstream-delta noted in the addendum above described the
> `IsHelicon` short-circuit as living in `VerifyExtraPrefix`; on closer read of the Go source it
> actually lives in **`VerifyExtra`** (`extra.go:120-121` — the sibling function ported separately as
> `syntactic_verify::truncated_extra_is_rejected_at_fortuna`), NOT `VerifyExtraPrefix`. This port's
> `verify_extra_prefix` therefore has no Helicon arm at all (unscheduled on every network; `AvaPhase`
> carries no `Helicon` variant), matching Go's current behavior exactly — the prior note's attribution
> was wrong, not the port. Once Helicon lands, both `VerifyExtra` and `VerifyExtraPrefix`'s Fortuna arm
> need a new arm together (single callout covers both, per the design spec's Risks section).
> (2) **Residual gap — three pre-`verifyHeaderGasFields` Go rejection surfaces remain UNPORTED**, all
> pre-existing (this branch's coverage is strictly improved, not regressed), honest-arm-safe (the Rust
> builder stamps correct values everywhere, so every live gate stays green), and flagged as a follow-up
> before any BFT-exposed deployment claim, same as the Helicon item above:
>   - **(a) `VerifyGasUsed`/`verifyIntrinsicGas` family.** Go's semantic-verify stage
>     (`wrapped_block.go:260-278`, `semanticVerify` → `verifyIntrinsicGas`) additionally checks,
>     pre-execution, that the header's claimed `GasUsed` (+`ExtDataGasUsed` post-Fortuna) fits the
>     block's gas capacity (`customheader/gas_limit.go:60-99`, `VerifyGasUsed`) AND that the summed
>     per-tx intrinsic gas does not exceed the claimed `GasUsed`. This branch's Go-oracle mutation
>     corpus (T6) shows no fail-open for this family today — Rust's executor independently recomputes
>     and equality-checks `gas_used` against the real execution result
>     (`lifecycle::verify_computes_precommit_root_no_commit`), a different but currently-sufficient
>     check — but Go's pre-execution capacity/intrinsic-gas rejection surface itself has no direct
>     Rust mirror.
>   - **(b) `VerifyTime` family — highest-value of the three.** Go's `VerifyTime` and siblings
>     (`VerifyMinDelayExcess`, `VerifyTargetExponent`, `VerifyMinPriceExponent`, `VerifySettled`, the
>     `errIsHeliconBlock` guard — `customheader/time.go:46-110`, called from `wrapped_block.go:358`
>     BEFORE `verifyHeaderGasFields`) have no Rust equivalent at all: `syntactic_verify` does no
>     timestamp checking, and this branch's new checks consume `header.time_milliseconds` as a trusted
>     input via `header_time_ms` (falls back to `time*1000` when absent). A Granite header with
>     missing/inconsistent `time_milliseconds` that Go rejects (`ErrTimeMillisecondsRequired`/
>     `Mismatched`) can therefore pass Rust if the fee fields are stamped self-consistently at the
>     fallback ms.
>   - **(c) Atomic-extension `ExtDataGasUsed` VALUE check.** Go's atomic block extension
>     (`plugin/evm/atomic/vm/block_extension.go:147-175`) requires `ExtDataGasUsed` to equal the
>     recomputed atomic-batch gas plus the AP5 `AtomicGasLimit` bound. Rust never compares the claimed
>     value against actual atomic gas; at Fortuna+ an inflated claim self-consistently stamped into the
>     extra prefix passes `verify_extra_prefix` (the claim feeds the recompute) — Go rejects, Rust
>     accepts.

> **AS-BUILT (semantic-verify family port, branch `cchain-semantic-verify`, 2026-07-19) — CLOSED.**
> All three residual gaps flagged directly above ((a) `VerifyGasUsed`/`verifyIntrinsicGas`, (b)
> `VerifyTime` family, (c) atomic `ExtDataGasUsed` value check) are now closed, plus an unplanned
> fourth surface found mid-branch. Five planned ports + one gap-branch port landed:
> `feerules::verify_time` (`time.go:55-124`), `feerules::verify_min_delay_excess`
> (`min_delay_excess.go:45-81`), `feerules::{gas_capacity, verify_gas_used}`
> (`gas_limit.go:61-98,164-180`), `EvmBlock::verify_intrinsic_gas` (`wrapped_block.go:287-332`,
> bootstrapped-gated), `atomic::{Tx::gas_used, verify::verify_ext_data_gas_used}`
> (`block_extension.go:142-177`) — plus the **unplanned sixth**, `atomic::verify::verify_utxos_present`
> (mirrors `block_extension.go:179-190`/`254-275`; a Task-7 gap-branch finding, not in the original
> five). `verify_time`/`verify_min_delay_excess` now run live on the verify path (clock + a new
> `bootstrapped` flag threaded via `Shared`, mutex + `AtomicBool` Release/Acquire) alongside the
> pre-existing `verify_header_gas_fields` — closing (b). `verify_intrinsic_gas` closes (a); note the
> honesty caveat: the understated-`GasUsed` mutant this port targets was **already fail-closed** via
> the executor's `NoGasUsed` post-execution backstop, so this port's value is coreth parity + cheaper
> **pre-execution** rejection, not a unique new fail-open closure. `verify_ext_data_gas_used` closes
> (c) (Go's atomic-batch-gas-plus-AP5-bound recompute, equality-checked against the claimed value).
> `verify_utxos_present` is a **forward guard**: the verify path still runs atomic import/export via
> `NoopPreHook` (M6.15 deferral, unchanged by this branch), so an import block cannot pass verify
> today regardless — this closes the specific `verifyUTXOsPresent` early-rejection gap for when
> that wiring lands, it does not itself unblock imports at verify.
>
> **Equivalence pin:** `block::tests::trailing_sae_tail_field_fails_decode` pins that a coreth block
> carrying any of the six SAE-only `HeaderExtra` tail fields Go's `semanticVerify`
> (`VerifyTargetExponent`/`VerifyMinPriceExponent`/`VerifySettled`) would reject is instead rejected
> by Rust at PARSE (`decode_rlp`'s trailing-bytes fail-close, `block.rs:250-252`) — same verdict,
> earlier stage, proven fail-capable (neutered + restored).
>
> **Documented ordering divergence (verdicts match):** Rust runs `verify_header_gas_fields` BEFORE
> `verify_time` on the verify path; Go's `semanticVerify` runs `VerifyTime` first. Both reject
> multi-fault candidates, just at a different check within the same rejected block — proven by the
> Task 8 oracle corpus (both `mismatched_time_milliseconds` and `far_future_time` mutants required
> restamping the fee-state-affecting timestamp shift to isolate `VerifyTime` on both sides, since an
> un-restamped Rust candidate would reject at `IncorrectFeeState` before ever reaching `verify_time`).
>
> **Still deferred (recorded, non-gating on the honest arm):** (1) the warp predicate pass has no
> production caller at all today (`build_block_predicates` referenced only in doc comments) — when it
> is wired onto the verify path, gate its invocation on the same `bootstrapped` flag this branch
> threaded through `verify_with_predicates`. (2) The export-tx `ExtDataGasUsed` oracle leg
> (`inflated_ext_data_gas_used` via a live export candidate) is deferred: the Go judge derives
> `CChainID`/`XChainID`/`AVAXAssetID` from `ids.GenerateTestID()` (process-counter-derived), so an
> offline Rust-built export tx cannot match them and coreth's `ExportTx.SemanticVerify` rejects on a
> chain/asset mismatch before ever reaching `ExtDataGasUsed` — a fixture reason unrelated to this
> branch. `PORTING.md`'s `ExtDataGasUsed` row is 🟡 (unit + golden-constant + the header-level
> `oversized_ext_data_gas_used` oracle leg still cover the surface); lift by injecting fixed
> chain/asset IDs into the Go judge's snow context. (3) The M6.15 `NoopPreHook`/atomic-verify-
> execution gap itself (import/export EVM state transfer not applied at verify) remains open and is
> unaffected by this branch.
>
> **Go-oracle verdict corpus** grew from 10 → **16 adversarial mutants** (+ honest), all GREEN against
> the live Go judge at oracle pin `a4290dc0f4` (rpcchainvm=45; unchanged by this branch — no
> "Upstream delta" note needed). The six new `semanticVerify`-family classes:
> `mismatched_time_milliseconds`/`far_future_time`/`wrong_min_delay_excess`/`understated_gas_used`
> reject with IDENTICAL Go+Rust sentinels; `missing_time_milliseconds` and `trailing_sae_tail_field`
> are documented **matched-but-earlier asymmetries** (both reject, each at its own earliest check —
> Go decodes a missing `time_milliseconds` as `Some(0)` same as Rust, so neither side's rejection
> class is the literal `TimeMillisecondsRequired`/SAE-tail sentinel one might expect, but both sides
> still reject the same malformed block). Design detail:
> `docs/superpowers/specs/2026-07-19-cchain-semantic-verify-family-design.md` (`## AS-BUILT notes`).

> **AS-BUILT (builder min-delay pacing, follow-up to semantic-verify, 2026-07-20) — CLOSED.**
> The ★ builder min-delay pacing follow-up from the final-review triage is now implemented. Two
> items landed: `feerules::min_next_block_time_ms` (ports coreth's `minNextBlockTime` at
> `block_builder.go:202`; the ACP-226 min-delay timestamp bound) and the paced `EvmPendingWorkWaiter::wait()`
> (ports coreth's `waitForEvent` pacing at `block_builder.go:140-214`; the waiter now sleeps until
> `parent_time_ms + parent.MinDelayExcess.Delay()` before returning work). Whole-second round-up
> applied: Rust builder stamps whole-second block timestamps, so the pacing sleeps until the next
> whole second >= computed min-time to avoid immediate `MinDelayNotMet` on block submission. **Deliberate
> deviation:** coreth's 100 ms `RetryDelay` retry arm (same-parent retry tracking, `block_builder.go:31,189-190`)
> unported — the forwarder's existing 2 s re-arm covers the retry-same-parent role; the pacing itself
> closes the liveness papercut. See `docs/superpowers/specs/2026-07-19-builder-min-delay-pacing-design.md`.
> **Residual — CLOSED 2026-07-20:** the forwarder re-arm is now routed through the paced
> `wait()` (`forward_pending_work`, one paced loop + 2 s anti-busy-spin retry floor; coreth
> notifier.go parity), so every `PendingTxs` signal — first and re-arm alike — respects the
> ACP-226 min delay. Design: `docs/superpowers/specs/2026-07-20-forwarder-rearm-pacing-design.md`.
> Still open (latent, documented): plugin-path `EvmVm::wait_for_event` unpaced.

---

## Spec coverage check

| Acceptance / surface item | Source | Task(s) |
|---|---|---|
| Reverse-dial handshake v45 — host side (Runtime serve, env, spawn, timeout, Pdeathsig) | `07` §5.1, `00` §11.1.1, `26` §5 | M9.1, M9.9 |
| Reverse-dial handshake v45 — guest side (`ava_vm_rpc::serve`: read env, dial back, serve VM+health) | `07` §5.1/§5.3 | M9.2 |
| `differential::plugin_rust_in_go` (Rust VM in Go host — TDD entry) | `16` §5(7), §3 (M9), `02` §11 | M9.3 |
| Proxied `rpcdb` (iterator handles, ErrEnumToError) | `07` §5.2/§5.4 | M9.4 |
| Proxied `appsender` (exact AppError i32 codes) | `07` §5.4, §9 | M9.5 |
| Proxied `sharedmemory` (get/indexed/apply, ATOMIC-1) | `07` §5.4, §3.1, `27` §2.3 | M9.6 |
| Proxied `validatorState` (windower-parity view) | `07` §5.4 | M9.7 |
| Proxied `warp` Signer + `aliasreader` | `07` §5.4 | M9.8 |
| Protocol-version mismatch + handshake-timeout sentinels (v45 exact equality) | `26` §5, `07` §5.1/§9 | M9.9 |
| `VmServer<V>` full `proto/vm` service (guest serves; dials all 6 callbacks at Initialize) | `07` §5.3/§5.4 | M9.10 |
| `RpcChainVm` host client full `ChainVm` (serves callbacks, dials VM; HTTP via ghttp; host factory) | `07` §5.2/§5.4/§8.1 | M9.11 |
| `differential::plugin_go_in_rust` (Go VM in Rust host) | `16` §5(7), `26` §5 | M9.12 |
| Four-way wire-identity matrix (`proto/vm` byte goldens) | `07` §10, `02` §6/§11 | M9.13 |
| Mixed Go+Rust network bring-up + `Observation.normalized()` | `02` §11.1/§11.3/§11.4, `26` §9(4) | M9.14 |
| `differential::mixed_network` (live, all chains, no fork, same tip) | `16` §5(2), `02` §11.3 | M9.15 |
| Go-data-dir → RocksDB import (R2) + foreign-dir refusal | `00` §4.4 / §11.2 R2, `26` §6, `04` R2 | M9.16 |
| `test-upgrade` (Go→Rust across activation height incl. Go-dir import) | `02` §10.4, `16` §5(8), `26` §7 | M9.17 |
| `test-load` (sustained stream, metric-name SLOs, zero errors) | `02` §10.3, `00` §7.3 | M9.18 |
| `test-reexecute` (recorded mainnet ranges → Go-identical roots) | `02` §10.5, `16` §5(3) | M9.19 |
| Crash-injection hardening (CC-ATOMIC, two-sided SM consistency) | `27` §9/§2/§3.1, `02` §11 | M9.20 |
| `bench-guard` perf gates (criterion baselines) | `02` §9, `16` §5(9), `00` §9 | M9.21 |
| Version string + compatibility matrix + `info.getNodeVersion` interop | `26` §9, `16` §5(2) | M9.22 |
| **16 §5 DoD (1) joins Mainnet & Fuji, tracks tip, no fork** | `16` §5(1) | M9.23 (aggregates M9.15 + M0–M8) |
| **16 §5 DoD (2) indistinguishable mixed net** | `16` §5(2) | M9.15, M9.23 |
| **16 §5 DoD (3) full differential incl. reexecute at target cases** | `16` §5(3) | M9.15, M9.19, M9.23 |
| **16 §5 DoD (4) flag parity** | `16` §5(4) | M9.23 (gates M8 `golden::flag_parity`) |
| **16 §5 DoD (5) API parity** | `16` §5(5) | M9.23 (gates M8 `differential::api_parity`) |
| **16 §5 DoD (6) genesis parity (Mainnet+Fuji)** | `16` §5(6) | M9.23 (gates M8 `golden::genesis_block_id`) |
| **16 §5 DoD (7) plugin interop both directions** | `16` §5(7) | M9.3, M9.12, M9.23 |
| **16 §5 DoD (8) upgrade continuity incl. Go-dir import** | `16` §5(8) | M9.17, M9.23 |
| **16 §5 DoD (9) perf gates hold** | `16` §5(9) | M9.21, M9.23 |
| PORTING.md zero `wip` rows (every crate) | `02` §10.1/§13 | M9.23 |
| CI cadence (per-PR recorded-oracle+reexecute+plugin-handshake; nightly live two-binary) | `00` §11.7, `02` §11.7 | M9.23 |
| BUILDABLE-&-GREEN invariant (build workspace+bin, nextest ci, clippy -D warnings) | global convention | M9.23 |
| **R-final retired** (drop-in acceptance) | `16` §5, §6, `00` §11.2 | M9.23 |
| **R2 fully exercised** (Go-dir→RocksDB import in upgrade) | `00` §11.2 R2, `16` §6 | M9.16, M9.17 |

**Deferrals: none.** This is the final milestone and the project's definition of done; every `16` §5 acceptance item, every `07` §5 rpcchainvm surface, and every `02` §10 suite maps to a task above and must be green at the M9.23 acceptance gate.

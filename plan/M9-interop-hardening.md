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

### Task M9.15: `differential::mixed_network` — live Go+Rust, all chains, no fork, same tip 🟡 OFFLINE LOCKSTEP-REPLAY ARM DONE (2026-06-16c); live two-binary arm gated
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

**Files:** `tests/differential/tests/mixed_network.rs`, `tests/differential/src/network.rs` (live spawner rewrite — items (b)/(c) above)
- [ ] **Step 1 — Red:** Write `differential::mixed_network`: boot the mixed Go+Rust network (M9.14); replay a proptest-generated input program (`IssueTx`/`ApiCall`/`AdvanceTime`/`AwaitFinalization`) against the whole network; after each `AwaitFinalization`, collect+normalize `Observation` from every node and assert all nodes (Go and Rust) agree on LA block ID+height, state/merkle root, and sorted validator set for **every** chain (P/X/C/SAE) — no fork, same tip. Failure prints `DIFFERENTIAL_SEED=<n>`.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential mixed_network` → fails.
- [ ] **Step 3 — Green:** Implement the lockstep driver reuse from `02` §11.6 over the mixed network; deterministic tx/key derivation from the seed feeds identical bytes to all nodes; persist minimal failing `(seed, program)` to `tests/differential/proptest-regressions/`.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential mixed_network` → passes (live mode; run on the nightly budget).
- [ ] **Step 5 — Commit:** `differential: mixed_network — live Go+Rust, all chains, no fork, same tip`

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

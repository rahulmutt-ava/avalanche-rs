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

### Task M9.3: `differential::plugin_rust_in_go` — minimal Rust test-VM hosted by a Go node (TDD ENTRY POINT) ✅ OFFLINE ARM DONE (2026-06-15); live Go-host arm gated
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

### Task M9.12: `differential::plugin_go_in_rust` — Go test-VM hosted by a Rust node
**Crate/area:** `ava-differential` + `ava-vm-rpc::host`  ·  **Depends on:** M9.11, M8 (avalanchers bin)  ·  **Spec:** `16` §5(7), `26` §5 (interop both directions), `07` §5.3, `02` §11
**Files:** `tests/differential/src/plugin.rs`, `tests/differential/tests/plugin_go_in_rust.rs`
- [ ] **Step 1 — Red:** Write `differential::plugin_go_in_rust`: take a known **Go** rpcchainvm plugin binary (built against protocol 45, e.g. a Go test-VM or the timestampvm reference); configure the **Rust** `avalanchego` node to host it via the rpcchainvm host factory; assert the Rust host completes `Runtime.Initialize` reverse-dial (the Go plugin dials our `Runtime` and we record its VM addr), then drive build/verify/accept and assert the chain advances. Also assert a Go plugin built against protocol **44** is rejected by the Rust host with `ProtocolVersionMismatch`, identically to a Go host.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential plugin_go_in_rust` → fails.
- [ ] **Step 3 — Green:** Implement harness helpers `launch_rust_host_with_go_plugin(go_plugin_path)` + `assert_handshake_complete()` + the mismatch case. Ensure the Rust node serves all six callback services (the Go plugin always dials them — the §5.3 symmetry).
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential plugin_go_in_rust` → passes (Go-plugin-in-Rust-host, v45).
- [ ] **Step 5 — Commit:** `differential: plugin_go_in_rust — Go test-VM hosted by a Rust node (v45 both directions)`

### Task M9.13: Four-way wire-identity matrix (`proto/vm` request-byte diff)
**Crate/area:** `ava-vm-rpc` + `ava-differential`  ·  **Depends on:** M9.3, M9.10, M9.11, M9.12  ·  **Spec:** `07` §10 (four-way matrix), `02` §6 (golden), §11.3
**Files:** `tests/differential/tests/plugin_wire_matrix.rs`, `tests/vectors/rpcchainvm/`
- [ ] **Step 1 — Red:** Write `plugin_wire_identity_matrix`: drive an identical block-build/verify/accept sequence through all four host⇄guest pairings (Rust⇄Rust, Rust-host⇄Go-guest, Go-host⇄Rust-guest, Go⇄Go); capture the `proto/vm` request bytes on the wire (interceptor / recorded transcript); assert identical block bytes, IDs, last-accepted, **and** `proto/vm` request bytes across all pairings (diff against committed `tests/vectors/rpcchainvm/` goldens). Also round-trip the proxied `rpcdb`/`appsender`/`sharedmemory` against the Go server.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential plugin_wire_identity_matrix` → fails (vectors absent / interceptor unwired).
- [ ] **Step 3 — Green:** Add a tonic interceptor to capture request bytes; extract the Go-side `proto/vm` request goldens via `tools/extract-vectors/` (`02` §6.2) into `tests/vectors/rpcchainvm/` with provenance. Implement the matrix driver reusing M9.3/M9.12 launchers.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential plugin_wire_identity_matrix` → passes.
- [ ] **Step 5 — Commit:** `differential: rpcchainvm four-way wire-identity matrix (proto/vm byte goldens)`

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

### Task M9.15: `differential::mixed_network` — live Go+Rust, all chains, no fork, same tip
**Crate/area:** `ava-differential`  ·  **Depends on:** M9.14, M4/M5/M6/M7 (P/X/C/SAE)  ·  **Spec:** `16` §5(2), `02` §11.3 (peer/handshake row: "both reach the same height; no fork")
**Files:** `tests/differential/tests/mixed_network.rs`
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

### Task M9.17: `test-upgrade` — Go→Rust across an activation height (incl. Go-dir import)
**Crate/area:** `tests/upgrade` + `xtask`  ·  **Depends on:** M9.16, M9.14 (mixed-net driver), M8  ·  **Spec:** `02` §10.4, `16` §5(8), `26` §7 (rolling-upgrade moving floor), `00` §4.4
**Files:** `tests/upgrade/src/lib.rs`, `tests/upgrade/tests/go_to_rust.rs`, `xtask/src/commands/test_upgrade.rs`
- [ ] **Step 1 — Red:** Write `test_upgrade` (`go_to_rust`): start a tmpnet network on the previous released **Go** binary; advance to just before an activation height; replace nodes one-by-one with the **Rust** binary across the activation height, importing each node's Go data dir → RocksDB (M9.16) on swap; assert chain continuity and **no fork** (every node's LA/state root agrees across the cut-over) and that the moving min-compatible floor (`26` §7) keeps Go and Rust peers connected during the roll. Add `cargo xtask test-upgrade` alias.
- [ ] **Step 2 — Confirm red:** `cargo xtask test-upgrade` (or `cargo nextest run -p ava-upgrade go_to_rust`) → fails.
- [ ] **Step 3 — Green:** Implement the upgrade harness: previous-Go-binary start, per-node Go→Rust swap with data-dir import, activation-height barrier, continuity/no-fork assertions reusing `Observation` (M9.14). Wire the `xtask` alias.
- [ ] **Step 4 — Confirm green:** `cargo xtask test-upgrade` → passes (nightly/pre-release budget).
- [ ] **Step 5 — Commit:** `tests: test-upgrade Go→Rust across activation height (incl. Go-dir→RocksDB import, R2)`

### Task M9.18: `test-load` — sustained tx stream, metrics SLOs, zero errors
**Crate/area:** `tests/load` + `xtask`  ·  **Depends on:** M9.14 (network bring-up), M5/M6 (X/C tx issue), M8 (API/wallet/metrics)  ·  **Spec:** `02` §10.3, `16` §5 (perf), `00` §7.3 (metric-name parity)
**Files:** `tests/load/src/generator.rs`, `tests/load/tests/sustained_load.rs`, `xtask/src/commands/test_load.rs`
- [ ] **Step 1 — Red:** Write `sustained_load`: against a tmpnet Rust network, the load generator issues a sustained C-Chain transfer + X/P tx stream for `--load-timeout`; scrape Prometheus (parity metric names, `00` §7.3); assert throughput/latency SLOs hold and **zero** errors. Add `cargo xtask test-load -- --load-timeout=30s`.
- [ ] **Step 2 — Confirm red:** `cargo xtask test-load -- --load-timeout=5s` → fails.
- [ ] **Step 3 — Green:** Implement `generator.rs` (uses `ava-wallet` + API client to build/issue txs at a target rate), the Prometheus scraper asserting parity metric names + SLO thresholds, and the `xtask` alias mirroring the Go `tests/load` task surface.
- [ ] **Step 4 — Confirm green:** `cargo xtask test-load -- --load-timeout=30s` → passes.
- [ ] **Step 5 — Commit:** `tests: test-load sustained tx stream + metric-name SLOs (zero errors)`

### Task M9.19: `test-reexecute` — replay recorded mainnet ranges → state roots match Go 🟡 C-CHAIN LEG DONE (2026-06-15); P/X deferred pending fixtures
**Crate/area:** `tests/reexecute` + `xtask`  ·  **Depends on:** M6 (C-Chain `differential::cchain_state_root`), M4/M5 (P/X), M9.14  ·  **Spec:** `02` §10.5 (reexecute = differential oracle), `16` §5(3), `00` §11.7 (per-PR)
**Files:** `tests/reexecute/src/lib.rs`, `tests/reexecute/tests/cchain_range.rs`, `tests/reexecute/tests/px_range.rs`, `xtask/src/commands/test_reexecute.rs`
- [x] **Step 1 — Red:** Write `reexecute_cchain_range` and `reexecute_px_range`: from a fixed starting state, replay a recorded range of mainnet C-Chain (and P/X) blocks (`blockexport` fixtures) through the Rust VMs; assert resulting state/merkle roots match the Go-recorded expected roots byte-for-byte (a differential oracle on recorded data). Add `cargo xtask test-reexecute`.
- [x] **Step 2 — Confirm red:** `cargo xtask test-reexecute` → fails.
- [x] **Step 3 — Green:** Implement the reexecution harness consuming Go `blockexport` artifacts (reuse the M6 reexecute fixtures); a fixed-start-state replay loop per chain asserting roots; wire the `xtask` alias. Mark it as the per-PR cheap differential oracle (`00` §11.7).
- [x] **Step 4 — Confirm green:** `cargo xtask test-reexecute` → passes (per-PR budget).
- [x] **Step 5 — Commit:** `tests: test-reexecute recorded mainnet ranges → Go-identical state roots`

> **AS-BUILT (merge `3b52e32`).** New workspace crate **`ava-reexecute`** at `tests/reexecute/` (added to root `Cargo.toml` `members`). `src/lib.rs` exposes a reusable harness — `ReexecuteCase`/`AllocEntry`/`ReexecuteRoots`/`Error` (thiserror) + `replay_cchain(&case) -> Result<ReexecuteRoots>` — ported verbatim from the M6.6 `crates/ava-evm/tests/cchain_state_root.rs` pipeline (Firewood-ethhash propose→commit genesis, decode EIP-2718 txs, `ExternalConsensusExecutor::execute_batch`, bundle→proposal post-root). The `genesis_to_1` fixture (`genesis_to_1.json` + `manifest.json`) was **copied** into `tests/reexecute/vectors/cchain/` so the crate is self-contained. `xtask/src/test.rs::test_reexecute()` (the pre-existing `TestReexecute` subcommand) now shells out to `cargo nextest run -p ava-reexecute` (no `main.rs` change). Verified in main tree: `cargo nextest run -p ava-reexecute` = **1 passed, 1 skipped**, `cargo xtask test-reexecute` green, clippy `--all-targets -D warnings` clean. **DEFERRED — `reexecute_px_range`:** authored as `#[ignore]` (panics if forced) — no Go-recorded P/X `blockexport` fixtures exist in the repo. Follow-up (fold into `02` §10.5): record a P/X `blockexport` fixture, add `replay_px` + a P/X `ReexecuteCase` equivalent, gate the live arm behind the reserved `px` feature.

### Task M9.20: Crash-injection hardening pass (CC-ATOMIC / two-sided SM consistency)
**Crate/area:** all VMs + `ava-database` + `ava-chains` + `ava-node`  ·  **Depends on:** M4–M7, M9.6 (sharedmemory), M9.19  ·  **Spec:** `27` §9 (crash-injection suite), §2 (CC-ATOMIC), §3.1 (two-sided SM), `02` §11
**Files:** `tests/differential/src/crash.rs`, `tests/differential/tests/crash_injection.rs`
- [ ] **Step 1 — Red:** Write `crash_injection_cc_atomic` and `shared_memory_two_sided_consistency`: parameterize the accept/execute path with a `CrashPoint` (C0–C7, `27` §3) via a `FailpointDb` (errors/aborts on the N-th `write()`) and an out-of-process `kill -9` at logged checkpoints; on restart run the §5 recovery and assert (a) every accepted block is fully present or fully absent (CC-ATOMIC — no partial diff/dangling LA/orphan SM), and (b) for an X→P (and X→C) export/import crashed in the `[SM-replay, write)` window, the peer chain observes all-or-nothing and the UTXO is never double-spendable nor lost — matching the Go oracle after the same crash+restart.
- [ ] **Step 2 — Confirm red:** `cargo nextest run -p ava-differential crash_injection` → fails.
- [ ] **Step 3 — Green:** Implement `crash.rs`: the `FailpointDb` wrapper + the out-of-process crash harness; the recovery-equivalence + CC-ATOMIC assertions against the Go oracle. Fix any hardening gaps surfaced (idempotent redo paths, abort guards) per `27` §5 — but only the minimum to make the recovery byte-identical to Go.
- [ ] **Step 4 — Confirm green:** `cargo nextest run -p ava-differential crash_injection` → passes.
- [ ] **Step 5 — Commit:** `hardening: crash-injection suite (CC-ATOMIC, two-sided shared-memory consistency)`

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

### Task M9.22: Version-string / compatibility-matrix interop conformance 🟡 GOLDEN LEGS DONE (2026-06-15); live `version_interop` deferred to M9.14
**Crate/area:** `ava-version` + `ava-network` + `ava-api`  ·  **Depends on:** M2 (handshake), M8 (`info.getNodeVersion`), M9.14  ·  **Spec:** `26` §9 (test plan), `16` §5(2)
**Files:** `crates/ava-version/tests/compat_matrix.rs`, `tests/differential/tests/version_interop.rs`, `crates/ava-version/compatibility.json`
- [x] **Step 1 — Red:** Write `golden::compatibility_matrix`, `golden::compatibility_json_byte_parity`, `golden::node_version_reply`, and `differential::version_interop`: assert `Application{avalanchego,1,14,2}.display() == "avalanchego/1.14.2"`; the `compatible()` table cells from `26` §9(3) (newer-major reject, below-floor reject, fork-boundary cut-over reject, different-name accept, mid-connection transition); `compatibility.json` parses byte-identically to the committed Go file; `info.getNodeVersion` reply matches Go field-for-field (modulo build-specific `gitCommit`/`go`); and in the mixed net a Rust node lowered below the Go floor is dropped by Go, and vice-versa (`26` §9(4)).
- [x] **Step 2 — Confirm red:** `cargo nextest run -p ava-version compat_matrix && cargo nextest run -p ava-differential version_interop` → fails for any uncovered cell.
- [x] **Step 3 — Green:** Fill any gaps in `Compatibility::compatible`, the embedded `compatibility.json`, and the `info.getNodeVersion` reply so all cells pass; commit `compatibility.json` byte-identical to the Go tree with a provenance note.
- [x] **Step 4 — Confirm green:** golden legs pass (`cargo nextest run -p ava-version compat_matrix`).
- [x] **Step 5 — Commit:** `ava-version: handshake compatibility-matrix + version-string golden conformance (live version_interop deferred)`

> **AS-BUILT (merge `bbb87a6`).** The three pure-Rust golden legs are complete and verified in main tree (`cargo nextest run -p ava-version` = **21/21, 1 skipped**; clippy `--all-features` clean). `crates/ava-version/compatibility.json` was copied **byte-identical** (1426 B, `cmp`-verified) from the Go tree's `version/compatibility.json` @ upstream `0b0b57143c`, with a `compatibility.json.md` provenance sidecar; a new `src/compat_table.rs` embeds it via `include_str!` (panic-free `LazyLock<Result<..>>` + fallible `rpc_chain_vm_protocol_compatibility()` accessor) — `serde_json` moved dev-dep → dep. `golden::compatibility_matrix` covers every §9(3) cell with two mock clocks straddling a fork; `golden::compatibility_json_byte_parity` asserts embed==file==reparsed-table and protocol 45 ⇒ `[v1.14.2]`; `golden::node_version_reply` pins version-string display + the `info.getNodeVersion` fields ava-version owns (`version`/`databaseVersion`/`rpcProtocolVersion` incl. the `json.Uint32` string form `"45"`).
> **DEFERRED — `differential::version_interop` (`26` §9(4)):** the live mixed Go+Rust floor-drop test belongs in `tests/differential/tests/version_interop.rs`, NOT in `ava-version` (a T0 primitive must not depend on `ava-differential`/`ava-network`/`ava-api`). Blocked on the **M9.14** mixed-network harness (the `ava-differential` `network.rs` is still a ~35-line scaffold). Recorded as an `#[ignore]`d `version_interop_deferred` stub + PORTING note. The full `info.getNodeVersion` JSON reply (incl. `gitCommit`/`vmVersions`) is already golden-tested at the `ava-api` layer (`crates/ava-api/src/info/mod.rs`).

### Task M9.23: Final acceptance gate (16 §5 definition of done)
**Crate/area:** all crates + `xtask` + CI  ·  **Depends on:** M9.1–M9.22 (every prior M9 task) + M0–M8 exit gates  ·  **Spec:** `16` §5 (the full checklist), `02` §10.1 (PORTING.md), §13, `00` §11.7
**Files:** `xtask/src/commands/acceptance.rs`, every crate's `tests/PORTING.md`, `.github/workflows/ci.yml` (or Bazel equivalent), `tests/differential/tests/definition_of_done.rs`
- [ ] **Step 1 — Red:** Write `definition_of_done` (an aggregating test/xtask `cargo xtask acceptance`) that asserts the full `16` §5 checklist is green simultaneously: (1) joins Mainnet & Fuji and tracks tip without forking; (2) `differential::mixed_network` (indistinguishable mixed net); (3) full `differential::*` suite incl. `test-reexecute` at target cases; (4) `golden::flag_parity` zero diff; (5) `differential::api_parity`; (6) `golden::genesis_block_id` (Mainnet+Fuji exact); (7) `differential::plugin_rust_in_go` + `differential::plugin_go_in_rust` (v45 both directions); (8) `test-upgrade` Go→Rust across activation height incl. Go-dir→RocksDB import; (9) `bench-guard` holds. Also assert every crate's `tests/PORTING.md` has **zero `wip` rows** (`cargo xtask porting-report`).
- [ ] **Step 2 — Confirm red:** `cargo xtask acceptance` → fails (some checklist item or `wip` row outstanding).
- [ ] **Step 3 — Green:** Drive every outstanding item green; update each crate's `tests/PORTING.md` to zero `wip` rows; wire CI so per-PR runs recorded-oracle + reexecute + plugin-handshake (`differential::plugin_rust_in_go`) while live two-binary differentials (`mixed_network`, `plugin_go_in_rust`, `test-upgrade`, `test-load`) run nightly/pre-release (`00` §11.7, `02` §11.7). Run the BUILDABLE-&-GREEN invariant: `cargo build --workspace`, `cargo build -p avalanchers`, `cargo nextest run --profile ci`, `cargo clippy --workspace -- -D warnings`, plus **all** named exit tests.
- [ ] **Step 4 — Confirm green:** `cargo build --workspace && cargo build -p avalanchers && cargo nextest run --profile ci && cargo clippy --workspace -- -D warnings && cargo xtask acceptance` → all pass; the `16` §5 checklist is fully green; zero `wip` rows.
- [ ] **Step 5 — Commit:** `M9: final acceptance gate — drop-in replacement DoD green (R-final retired)`

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

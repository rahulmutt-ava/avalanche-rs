# `ava-node` — Go → Rust porting matrix

Tracks the node-assembly port (`node/node.go`, `node/insecure_validator_manager.go`,
`node/beacon_manager.go`, `node/overridden_manager.go`) against the
`../avalanchego` reference tree, plus every **narrow seam** and **documented
deferral** M8.29 introduced (per-item, never silently skipped).

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

---

## `Node::new` (M8.29 — specs 12 §2.1/§2.2, 17 §1/§2/§4)

The 26-step Go init order is reproduced step-for-step in
`src/node.rs::Node::new` (one `src/init/*.rs` module per concern) and pinned
by `node::tests::init_order_matches_go`. Go's load-bearing ordering comments
are preserved: the message creator runs **after metrics, before networking**;
the health API runs **before the chain manager**.

Runtime ownership (17 §1.1): `Node::new` takes a `tokio::runtime::Handle`;
no `Runtime::new` / `block_on` anywhere in the crate. Blocking work (NAT
probe/PMP exchange, DNS lookups, NAT external-IP query) goes through
`spawn_blocking`. The 17 §4.1 token tree exists: root `shutdown` token →
`network_token` (peer tokens are its children inside `ava-network`) and →
`subnet_token` (chain tokens land with chain creation). `TaskTracker`,
`exit_code`, `shutting_down`, `shutdown_once` are assembled for M8.30;
the M8.29 `ShutdownTrigger` records the exit code and cancels the root token
(the full 14-step sequence is M8.30).

### Narrow seams (defined in `ava-node`, no cross-crate refactors)

| Seam | File | What it stands in for | Filled by |
|---|---|---|---|
| `RouterBridge` | `src/init/networking.rs` | the network→consensus `ExternalHandler`; `handle_inbound` debug-drops messages (no wire-op → engine-op conversion yet); the engine-router slot is filled by init step 20 | M8.30 dispatch |
| `SystemResourceManager` (+ `NoopSystemResources`) | `src/init/resource.rs` | Go `resource.Manager` (process CPU/disk poller + available disk space); the noop reports unbounded disk so the `diskspace` health check stays green | future system-poller task |
| `RuntimeManager` (+ `NoopRuntimeManager`) | `src/init/vms.rs` | Go `runtime.Manager` (rpcchainvm plugin subprocess tracking; shutdown step 12 kills them) | plugin-host milestone |
| `EmptyVmGetter` | `src/init/vms.rs` | the `plugin-dir` scanner (filename → VM id + rpcchainvm probe); the registry starts empty and `reload` discovers nothing | plugin-host milestone |
| `AssemblyChainManager` | `src/init/chain_manager.rs` | Go `chains.Manager`: owns the aliaser, bootstrapped set, registrants and **queues** `ChainParameters` (`start_chain_creator` records, does not build) — the P/X/C `ava_chains::Factory` impls don't exist yet | chains milestone |
| `ShutdownTrigger` | `src/init/mod.rs` | `n.Shutdown(code)` for subsystems that can demand shutdown before `Node` exists (disk-space check → exit 1, indexer fatal close → exit 0 like Go's `TODO put exit code here`) | M8.30 |
| admin/info seam adapters | `src/init/api_services.rs` | live impls of the `ava-api` admin (`LoggerLevels`, `ChainAliaser`, `VmRegistry`) and info (`ChainManager`, `ValidatorSet`, `VmManager`, `InfoNetwork`, `Benchlist`) trait seams | this task (done) |
| `ServerPathAdder` | `src/init/api_services.rs` | the indexer's `PathAdder` over `ava_api::ApiServer::add_route` | this task (done) |
| `DynDb` | `src/init/database.rs` | object-safe `Arc<dyn DynDatabase>` re-implementing the typed `Database` GAT so `PrefixDb`/the indexer run over the dynamically-chosen backend without making `Node` generic | n/a (permanent adapter) |

### Documented deferrals / divergences (Go ↔ Rust)

| # | Step | Go behavior | Rust behavior (M8.29) |
|---|---|---|---|
| 1 | 2 — staking signer | `rpcsigner` over `--staking-rpc-signer-endpoint` | `Error::RpcSignerUnsupported` (typed, byte-recognizable message) |
| 2 | 9 — API server | binds the HTTP listener in `initAPIServer`, maps the **bound** port | the Rust `Server` binds in `serve()` (M8.30); the configured port is mapped when non-zero; a `--http-port=0` URI is re-resolved once the listener binds (M8.30) |
| 3 | 10 — metrics API | registers Go process + go-runtime collectors under `avalanche_process` | namespace registered for layout parity; the `prometheus` process collector is Linux-only — collectors deferred |
| 4 | 11 — database | leveldb / pebbledb backends; `--db-read-only` | every on-disk name opens the single RocksDB engine (04 §2.1) under the Go-compatible folder (`v1.4.5` / `pebble` / `db`); `memdb` in-memory; read-only mode warns and opens read-write |
| 4a | 11 — database (M9.16) | (Go opens a `PrevDatabase`/foreign dir via its migration logic) | `init/db_init::precheck_data_dir` refuses a foreign/older Go dir (`pebble/` or `v1.0.0/`) with `Error::ForeignDataDir` *before* the RocksDB open — never opens-in-place (26 §6 / 04 §11.2); offline import is the M12 CLI tool. Marker (`ungracefulShutdown`) ownership stays in `init/database.rs`. Tests: `db_init::tests::refuses_pebble_dir` / `refuses_prev_database_dir` / `allows_{fresh,current_database,memdb}_dir` |
| 5 | 16 — networking | `--public-ip-resolution-service` (opendns/ifconfig…) | `Error::UnsupportedResolver` — the M8.28 `Resolver` trait seam exists, concrete resolvers need an HTTP-client dep decision; `Networking::ip_updater` is always `None` until then |
| 6 | 16 — networking | `--network-tls-key-log-file` | warns "not supported yet" (rustls keylog wiring deferred) |
| 7 | 16/18 — uptime | per-peer observed uptime, `lastSent`/`lastReceived`, tracked subnets/ACPs in `peer.Info` | the M2 `ava-network` `PeerInfo` is trimmed (node id / ip / version / ingress); info-API `peers` reply zero-fills the missing fields |
| 8 | 18 — health | `futureupgrade` check + live `avalanche_upgrade_time_until` gauge | check deferred; gauge registered at `+Inf` for metrics-layout parity |
| 9 | 20 — benchlist | full Go `benchlist.Manager` config block (threshold, min failing duration, duration, max portion) | the simplified M3 `ava-engine` benchlist: only `--benchlist-duration` maps (bench-duration cap); seeded from the first 8 NodeID bytes |
| 10 | 21 — VMs | registers built-in platformvm/avm/coreth factories | no `ava_chains::Factory` impls for P/X/C yet — registration deferred to the chains milestone; `initVMs`'s reload still runs (discovers nothing) |
| 11 | 22 — admin `getConfig` | marshals the whole resolved config | `ava_config::node::Config` has no serde derives (kept additive-minimal); the reply is the resolved `providedFlags` map |
| 12 | 22 — info `getNodeVersion` | `version.GitCommit` build global | empty string until the `avalanchers` bin wires build-info (M8.30/bin task) |
| 13 | 22 — info `peers` benched list | `benchlist.Manager.GetBenched(nodeID)` chain IDs | empty (the M3 benchlist has no per-chain bench registry) |
| 14 | 25 — profiler | `profiler.NewContinuous` dispatcher | warns when `--profile-continuous-enabled`; the on-demand admin profiler (M8.19) is the only profiler |
| 15 | 26 — chains | `StartChainCreator` boots the platform chain | the creation request is queued on `AssemblyChainManager` (asserted by the init test); chain construction is the chains milestone |
| 16 | logging | per-logger display cores | one shared stdout core: the display level is name-independent (18 §5 divergence, `LogFactory::display_handle`) |
| 17 | 17 — dispatchers | synchronous unbounded `AcceptorGroup` fan-out | `tokio::broadcast` with capacity 1024; a lagging indexer subscriber is fatal (M8.24 semantics) |

### Go test parity (`node/`)

| Go test | Status | Where / why |
|---|---|---|
| `node.go` init order (no Go unit test; ordering is comment-enforced) | ✅ | `src/node.rs::tests::init_order_matches_go` (stronger than Go: asserted) |
| `overridden_manager_test.go` (`TestOverriddenManager`) | 🟡 | port covered by `OverriddenManager` impl (`src/init/validators.rs`); dedicated unit test deferred to M8.30 wave |
| `beacon_manager_test.go` (`TestBeaconManager_DataRace`) | 🟡 | `BeaconManager` ported with atomics (`src/init/networking.rs`); the 100-goroutine race test relies on Go's mock router — covered by `-race`-equivalent loom-free atomics review, dedicated test deferred |
| `insecure_validator_manager.go` (no Go test) | n/a | ported (`src/init/networking.rs`) |
| `config.go` / `config_test.go` | n/a | lives in `ava-config` (M8.1–M8.15) |

## Dispatch + 14-step shutdown (M8.30 — specs 12 §2.3/§2.4, 17 §4.3/§4.4/§9)

`Node::dispatch` (`src/dispatch.rs`) and `Node::shutdown` (`src/shutdown.rs`)
are the run loop + teardown. Dispatch writes `process.json`
(`{pid, uri, stakingAddress}`), spawns the API task + the
bootstrap-beacon-connection-timeout warn task (both on `Node.tasks`),
manually-tracks state-sync + bootstrap peers, runs `net.dispatch().await`, then
`shutdown(1)` and removes `process.json`. Shutdown sets exit-code/`shuttingDown`
(first demand wins), cancels the root token, and runs the 14 steps **exactly
once** via `shutdown_once: OnceCell`. Pinned by
`src/shutdown.rs::tests::shutdown_order_matches_go` (+ `shutdown_runs_once`,
`subnet_cancellation_is_scoped`) and `src/dispatch.rs::tests::*`.

### Shutdown steps: real vs instrumented-seam

| # | Step (recorded name) | Status | Notes |
|---|---|---|---|
| 1 | `shuttingDown` | ✅ real | registers an always-failing `Checker`; sleeps `http-shutdown-wait` |
| 2 | `staking_signer` | ✅ real | `Signer::shutdown()` (default `Ok` for the ephemeral/file signer) |
| 3 | `resource_manager` | 🟡 seam | calls `SystemResourceManager::shutdown()`; the noop has no poller (real poller → future system-poller task) |
| 4 | `timeout_manager` | ✅ real | `AdaptiveTimeoutManager::stop()` cancels the dispatch loop |
| 5 | `chain_manager` | 🟡 partial | `AssemblyChainManager::shutdown(consensus_shutdown_timeout)` drains every registered chain (cancel token → close+`wait()` w/ timeout → abandon); **no chains are created yet** so the live node drains zero chains. `register_chain`/`subnet_token` exist + are exercised by the cancellation-propagation test; real per-chain executor/gossip/engine drop lands with the chains milestone |
| 6 | `benchlist` | 🟡 noop | the M3 `Benchlist` has no background task (no `shutdown` method); Go-parity placeholder |
| 7 | `profiler` | 🟡 noop | continuous profiler is a documented deferral; on-demand admin profiler holds no state |
| 8 | `net_start_close` | ✅ real | `Network::start_close()` + cancel `network_token` |
| 9 | `api_server` | ✅ real | `ApiServer::shutdown()` bounded by `http-shutdown-timeout` |
| 10 | `nat` | 🟡 partial | aborts the staking-port keep-alive task (per-mapping unmap fires on the cancelled token, Go `UnmapAllPorts`); `ip_updater` is always `None` (resolver deferral) |
| 11 | `indexer` | ✅ real | `Indexer::close()` flushes the final batch |
| 12 | `runtime_manager` | 🟡 seam | `RuntimeManager::stop()`; the noop tracks no subprocesses (real kill → plugin-host milestone) |
| 13 | `database` | ✅ real | `db.delete(UNGRACEFUL_SHUTDOWN_KEY)` then `db.close()` — persistence last |
| 14 | `tracer` | ✅ real | `Tracer::shutdown()` flushes spans (no-op when tracing disabled) |

### Dispatch divergences (Go ↔ Rust)

| Go | Rust (M8.30) |
|---|---|
| `RecoverAndPanic` around the API goroutine | a `tokio` task on `Node.tasks`; an exit while not `shutting_down` logs + demands `shutdown(1)` |
| `tlsKeyLogWriterCloser.Close()` in `Dispatch` | n/a — TLS key logging is a networking deferral (no closer) |
| `apiURI` re-resolution for `--http-port=0` | **DEFERRED**: `process.json` still records the *configured* URI. The `Server` binds inside `serve()` and does not expose the bound `SocketAddr` post-bind, so a `--http-port=0` URI cannot yet be re-resolved. Handoff: add a bound-addr watch/oneshot to `ava_api::Server` (cross-crate) so dispatch can rewrite `process.json` after `serve()` binds. `Server::bind_addr()` only returns the *configured* addr. |
| `RouterBridge::handle_inbound` → engine router | **DEFERRED**: `handle_inbound` still debug-drops (wire-op → engine-op conversion + chain dispatch is the chains milestone). The engine-router slot is filled at init step 20, but no decoded message is routed yet. |

### Go test parity (`node/`) — M8.30 additions

| Go test | Status | Where / why |
|---|---|---|
| `node.go` shutdown order (no Go unit test; ordering is comment-enforced) | ✅ | `src/shutdown.rs::tests::shutdown_order_matches_go` (stronger than Go: asserted) + `shutdown_runs_once` (the `OnceCell` guard) |
| `node.go` `Dispatch` (no Go unit test) | ✅ | `src/dispatch.rs::tests::api_dispatch_failure_triggers_shutdown_1` + `write_process_context_writes_pid_uri_staking` |
| cancellation propagation (17 §9; no Go unit test) | ✅ | `src/shutdown.rs::tests::subnet_cancellation_is_scoped` |

### Notes for M8.31 (the `avalanchers` bin)

- `Node::dispatch(self: Arc<Self>) -> i32` is the bin's run loop; it owns the
  `Arc<Node>` and returns the process exit code.
- `apiURI` re-resolution + `RouterBridge` engine routing remain deferred (see
  the divergence table above) — the bin does not need them, but the chains
  milestone does.
- The `ShutdownTrigger` tail (`src/init/mod.rs`) still only records the exit
  code + cancels the root token. The real 14-step sequence runs in
  `Node::shutdown`; subsystems that hold a `ShutdownTrigger` (disk-space check,
  indexer fatal close) therefore demand a shutdown that the **dispatch loop**
  observes (root token cancel → `net.dispatch()` returns → `shutdown(1)`). A
  trigger fired before `dispatch` starts is observed once `net.dispatch()` is
  entered.

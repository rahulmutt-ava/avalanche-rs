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

### Notes for M8.30 (dispatch + shutdown)

- `Networking::on_sufficiently_connected` (watch receiver) feeds the
  bootstrap-beacon-connection-timeout warn task.
- `RouterBridge::engine_router()` is `Some` after step 20 — wire
  `handle_inbound` to it.
- `Node.api_uri` uses the configured port; re-resolve via
  `Server::bind_addr()` after `serve()` binds when `--http-port=0`, then write
  `process.json`.
- The `ShutdownTrigger` tail must become the 14-step sequence; delete
  `UNGRACEFUL_SHUTDOWN_KEY` (step 13) before `db.close()`.
- `Node.tasks` (`TaskTracker`) is constructed but nothing registers on it yet;
  dispatch-spawned tasks must.

# `ava-api` — Go → Rust porting matrix

Tracks coverage of Go `api/...` tests against the `../avalanchego` reference
tree, plus any **documented behavioral divergences** where the Go runtime
surface has no 1:1 Rust equivalent (per-method, never silently skipped).

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

---

## admin API (`api/admin`, M8.19 — specs 12 §3.5, 14 §4)

All 13 methods are implemented in `src/admin/` behind narrow trait seams
(`AliasAdder`, `ChainAliaser`, `LoggerLevels`, `VmRegistry`,
`ava_database::traits::KeyValueReader`); live node handles are wired in
M8.22/M8.29.

> Spec note: 14 §4's prose says "14 methods" but its table — and Go
> `api/admin/service.go` — has exactly **13**; the count in the spec prose is
> a typo (do not add a method).

### Documented divergences (Go runtime surface ↔ Rust)

| Method | Go behavior | Rust behavior | Why |
|---|---|---|---|
| `admin.startCPUProfiler` / `stopCPUProfiler` | `runtime/pprof.StartCPUProfile` streams samples into `<profile-dir>/cpu.profile` | **Real** CPU profile via the `pprof` crate (100 Hz, the Go default), pprof-protobuf encoded — `go tool pprof` reads it. Divergence: `cpu.profile` is created at start but its contents are written at stop (samples buffer in memory, not streamed to the fd) | no stable streaming sampler in Rust; output format and file name are parity |
| `admin.memoryProfile` | dumps the Go heap profile to `mem.profile` | returns the domain error `memory profiling is not supported by this node implementation` (`-32000` on the wire); **no fabricated file** | Rust has no allocator-level heap profile without replacing the global allocator (jemalloc etc.), which 00 §4 dep policy doesn't admit for this |
| `admin.lockProfile` | `runtime.SetMutexProfileFraction` + mutex profile to `lock.profile` | returns the domain error `lock profiling is not supported by this node implementation`; **no fabricated file** | no `SetMutexProfileFraction` equivalent for std/parking_lot/tokio mutexes on stable (12 §3.5 floats "tokio metrics", but tokio's task dump is unstable/`tokio_unstable`-only and is not a lock profile) |
| `admin.stacktrace` | dumps **all goroutine** stacks to `stacktrace.txt` (cwd-relative, like Go) | writes `stacktrace.txt` (same cwd-relative path) containing the **calling thread's** backtrace plus a header naming the limitation — real but partial | Rust/tokio have no stable all-thread/all-task stack dump (`tokio::runtime::Handle::dump` needs `tokio_unstable`); revisit if/when task dumps stabilize |

Everything else (method set & exact wire casing incl. `StartCPUProfiler` /
`StopCPUProfiler` / `LoadVMs` / `DbGet`, 512-byte alias cap, byte-exact error
strings `alias length is too long` / `need to specify either displayLevel or
logLevel` / `cpu profiler already running` / `cpu profiler doesn't exist` /
`missing 0x prefix to hex encoding`, UPPERCASE `logging.Level` JSON,
`failedVMs` omitempty, `dbGet` HexNC codec + `rpcdbpb.Error` numeric
`errorCode` with mapped errors as SUCCESS replies) is Go-parity.

### Go test parity

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `service_test.go::TestServiceDBGet` | ✅ ported | `admin::tests::db_get_error_code_mapping` (found / not-found=2 / closed=1 / missing-0x-prefix) |
| `service_test.go::TestLoadVMsSuccess` | ✅ ported | `admin::tests::load_vms_reply_shape` (newVMs + failedVMs, id-string keys, omitempty) |
| `service_test.go::TestLoadVMsReloadFails` | ✅ ported | `admin::tests` mock seam propagates reload errors as `-32000` (covered by the seam-error path in `load_vms`) |
| `service_test.go::TestLoadVMsGetAliasesFails` | n/a | alias resolution is folded into the `VmRegistry::reload` seam (node assembly composes registry+aliaser, M8.29); failure surfaces identically |
| `client_test.go::Test{StartCPUProfiler,StopCPUProfiler,MemoryProfile,LockProfile}` | ✅ ported | `admin::tests::profiler_lifecycle_and_unsupported_profiles` (lifecycle + byte-exact errors; client crate itself is out of scope here) |
| `client_test.go::Test{Alias,AliasChain,GetChainAliases}` | ✅ ported | `admin::tests::{alias_rejects_too_long_alias, alias_chain_registers_chain_and_route_aliases, get_chain_aliases_parses_id}` |
| `client_test.go::TestStacktrace` | 🟡 partial | `admin::tests::stacktrace_writes_file_in_cwd` — file written, content is the partial dump (divergence above) |
| `client_test.go::Test{SetLoggerLevel,GetLoggerLevel}` | ✅ ported | `admin::tests::{set_logger_level_requires_a_level, set_logger_level_empty_name_sets_all, get_logger_level_named_and_all, level_json_casing_matches_go}` |
| `client_test.go::TestGetConfig` | ✅ ported | `admin::tests::get_config_returns_node_config` |
| `client_test.go::TestReloadInstalledVMs` | ✅ ported | `admin::tests::load_vms_reply_shape` + `wire_dispatch_uses_go_method_names` (`admin.loadVMs` exact casing) |
| (no Go counterpart) method-set drift guard | ✅ added | `admin::tests::admin_method_set` — exactly the 13 wire names; wrong pascalizations (`StartCpuProfiler`, `LoadVms`, `DBGet`) must NOT resolve |
| (no Go counterpart) wire dispatch | ✅ added | `admin::tests::wire_dispatch_uses_go_method_names` — end-to-end gorilla-shim POSTs with client casing |

**Summary:** 10 ported ✅ / 1 partial 🟡 (stacktrace content) / 1 n/a / 0 ⬜.

---

## `vectors/api/metrics_schema.json` — metrics-name golden (M8.21, specs 18 §3/§4)

**What it is.** The Go `/ext/metrics` schema snapshot
`{(name, type, sorted(label_keys))}` — values dropped, schema only — emitted by
the **real** Go `api/metrics` gatherer tree via the in-repo oracle
`go-oracle/metrics_schema_oracle_test.go`. `golden_metrics_names.rs::
metrics_name_parity` rebuilds the identical tree with `ava_api::metrics` and
asserts the Rust schema is a **superset** of every non-waived Go family.

**Scope decision.** Spec 18 §3 prescribes snapshotting a fully booted node's
`/ext/metrics`. That full-node differential run is the `02`-harness's job
(post-M8.29, when `avalanchers` can boot all chains); it is not feasible — and
not honest — from an `ava-api` unit test, because the per-subsystem families
(18 §2.1–§2.15) are registered by their owning crates (M2–M7), not by
`ava-api`. This per-PR golden is therefore scoped to what the gatherer/naming
machinery itself produces, built from real Go code (never hand-fabricated
names):

- `avalanche_process` — node.go `initMetricsAPI`'s collectors
  (`collectors.NewProcessCollector` + `collectors.NewGoCollector`) under
  `MakeAndRegister`. This captures the real (and spec-correcting) names:
  the prefix gatherer renames unconditionally, so the families are
  `avalanche_process_process_*` / `avalanche_process_go_*` — **not** bare
  `process_*`/`go_*` as 18 §4's parenthetical suggests.
- `avalanche_network` — a representative subsystem registry
  (`peers` gauge, `peers_subnet{subnetID}` gauge vec; 18 §2.1).
- `avalanche_snowman` — the chains/manager.go per-chain wiring: a
  `LabelGatherer("chain")` registered into the root prefix gatherer, with a
  chain registry under alias `P` (`polls_successful`/`polls_failed`; 18 §2.8).

**Waivers (documented in the test, 18 §4):**

- `avalanche_process_go_*` — Go-runtime collector; no Rust equivalent, never
  faked.
- `avalanche_process_process_*` off Linux — the Rust `prometheus` crate's
  process collector is Linux-only; full `process_*` parity is asserted on
  Linux (the production target).
- `avalanche_process_process_virtual_memory_max_bytes` — not emitted by the
  Rust `prometheus` 0.13 process collector (crate gap), on any platform.
- `avalanche_process_process_network_{receive,transmit}_bytes_total` — emitted
  only by client_golang v1.23.0's **Linux** procfs collector (not on darwin);
  not emitted by the Rust `prometheus` 0.13 collector on any platform (crate
  gap).

**Regenerate** (avalanchego checkout required; leaves the Go tree clean):

```sh
AG=/path/to/avalanchego
RS=/path/to/avalanche-rs
cp $RS/crates/ava-api/tests/go-oracle/metrics_schema_oracle_test.go $AG/api/metrics/
cd $AG
AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
AVAX_RS_METRICS_SCHEMA_OUT=$RS/crates/ava-api/tests/vectors/api/metrics_schema.json \
  go test ./api/metrics/ -run TestEmitAvalancheRsMetricsSchema -count=1 -v
rm $AG/api/metrics/metrics_schema_oracle_test.go
```

Current snapshot provenance: avalanchego `5896c92fee23c2eff53d557dceeb89f1a6218224`,
emitted on `darwin`. Note the collectors are **not** platform-identical:
client_golang v1.23.0's Linux (procfs) process collector emits 2 extra
families — `process_network_receive_bytes_total` and
`process_network_transmit_bytes_total` — that the darwin collector does not;
the Rust `prometheus` 0.13.4 process collector emits neither, on any platform.
Both are therefore explicitly waived in the test, so a snapshot regenerated on
Linux stays green (`go_*` families are waived regardless).

Keep the Rust tree in `golden_metrics_names.rs::rust_schema()` and the Go tree
in the oracle **in sync** — they must build the same namespaces/families.

**Known Go-observable divergences (error paths only):**

- Gather-error message strings differ from client_golang's (Rust error text is
  not a transliteration of `prometheus.Gatherers.Gather`'s).
- Non-GET `/ext/metrics` returns 405 (Go's promhttp serves any method; spec 14
  §6 prescribes GET).
- No gzip content-negotiation (Go's promhttp gzips on `Accept-Encoding`; the
  plain text output is spec-compliant either way).
- Empty metric families are not filtered from the merged output (Go's
  `NormalizeMetricFamilies` drops them).

---

## `vectors/api/api_parity.json` — differential API parity (M8.23, specs 14 §14/§16.6, 02 §11.4)

**What it is.** Recorded REAL Go responses for the built-in node-level services
`ava-api` hosts (info / admin / health), emitted by the in-repo Go oracle
`go-oracle/api_parity_oracle_test.go`. Each recorded call's reply struct is
marshaled through the production `utils/json` codec (the same `json.Uint32` /
`json.Uint64` / `json.Float64` / `signer.ProofOfPossession` marshalers the live
service uses). `differential_api_parity.rs` drives the identical pinned request
at the in-process Rust service through the JSON-RPC registry + `ava_api::dispatch`
and asserts **structural-JSON-equality** after the 02 §11.4 normalizer.

**Recorded-oracle is the per-PR mode** (not `#[ignore]`, not env-gated): the
committed vector is `include_str!`-loaded and the test always runs under
`cargo nextest run -p ava-api`. Live mode (booting both a Go and a Rust node and
diffing live `/ext/*` responses) is the cross-cutting differential harness X's
job — see the seam comment at the top of `differential_api_parity.rs`; this task
does not build the two-binary live path.

### Coverage

| Service | Reply-shape parity | Method-set completeness (14 §14.2) | Notes |
|---|---|---|---|
| `info` (13) | ✅ 9 recorded calls (getNodeVersion, getNodeID, getNetworkID, getNetworkName, getBlockchainID, isBootstrapped, uptime, getTxFee, getVMs, peers) | ✅ exact set | `acps` / `getNodeIP` shape covered by the in-crate `info::tests`; not re-recorded (no new wire-type) |
| `admin` (13) | ✅ 3 recorded calls (getChainAliases, getLoggerLevel UPPERCASE level, loadVMs omitempty) | ✅ exact set | profiler/stacktrace divergences are the existing admin matrix above |
| `health` (3 JSON-RPC) | ✅ 3 recorded calls (health/readiness/liveness), `timestamp`+`duration` normalized | ✅ via mounted router | dual GET handler covered by the HTTP-semantics tests below |
| `platform` (31) | n/a — driven in `ava-platformvm` (M8.23a) | ✅ canonical wire-name set pinned | see divergences below |
| `avm` (11) | n/a — driven in `ava-avm` (M8.23b) | ✅ canonical wire-name set pinned | see divergences below |

Plus: **error-response snapshots** (14 §16.6) driven through the real Rust
dispatch shim — bad params `-32602`, unknown method `-32601`, malformed JSON
`-32700`, wrong version `-32600`, server error `-32000` (+ the EVM-revert code
`3`, asserted-as-recorded; it is a reth wire concern, checked in `ava-evm`).
And **HTTP semantics** (14 §16.3): the `node-id` response header on a normal
response and on the allowed-hosts `403` short-circuit, the per-chain
not-bootstrapped `503`, and the health GET `200`/`503` — driven through the
PUBLIC `ava_api::middleware` functions in `build_router`'s layer order (the
composed-server wiring is covered by the in-crate `server.rs` unit tests; its
`build_router` is crate-private).

### Why P-Chain / X-Chain are method-set-only here (no silent caps)

The `platform.*` / `avm.*` services live in `ava-platformvm` / `ava-avm`, which
**must not import `ava-api`** (the dep chain is `ava-api → ava-config →
ava-genesis → ava-{platformvm,avm}`; importing back would cycle). So they cannot
be driven in-process from this crate. Their reply-shape differential parity is
covered by the tests INSIDE those crates (M8.23a / M8.23b). This harness pins
their canonical gorilla wire-name sets (31 / 11) so a dropped or renamed method
is caught here even though the bodies are diffed elsewhere.

### Documented known-divergences (do not force byte-match)

- **`gas.Gas` / `gas.Price` have no Go marshaler** ⇒ `platform.getFeeState` /
  `getFeeConfig` / `getValidatorFeeState` emit **bare JSON numbers**, not
  `json.Uint64` strings (asserted in the ava-platformvm differential test).
- **P-Chain**: `getCurrentValidators` / `getL1Validator` attribute subset;
  `getTx` / `getBlock` `json` encoding; `getUTXOs` cross-chain `sourceChain`
  atomic UTXOs; elastic-subnet transform fields; `issueTx` runtime admission
  (un-shared mempool until M8.30 node wiring).
- **X-Chain**: `getTxJSON` shape goldens; typed `FxOperation` outputs;
  address→UTXO pagination order (Rust UTXO-ID sort vs Go linkeddb insertion
  order — node-local).
- **`Avalanche-Api-Route`** is the header-route header (NOT
  `X-Avalanche-Vm-Route`).
- **health `timestamp` / `duration`** are normalized out (02 §11.4: wall-clock +
  measured durations are non-deterministic).

### Regenerate (avalanchego checkout required; leaves the Go tree clean)

```sh
AG=/path/to/avalanchego
RS=/path/to/avalanche-rs
cp $RS/crates/ava-api/tests/go-oracle/api_parity_oracle_test.go $AG/api/info/
cd $AG
AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
AVAX_RS_API_PARITY_OUT=$RS/crates/ava-api/tests/vectors/api/api_parity.json \
  go test -tags test -run TestEmitAvalancheRsAPIParity ./api/info/ -count=1 -v
rm $AG/api/info/api_parity_oracle_test.go
```

Current snapshot provenance: avalanchego `3d434bacaee972c37ae899f7e49566e09d04f915`.

Keep the Rust replay fixtures in `differential_api_parity.rs` and the pinned
inputs in the oracle **in sync** — the fixtures (node id `[7;20]`, BLS PoP
`01../02..`, mainnet, X alias = `[3;32]`, avm = `[8;32]`, one peer `[9;20]`
benched on `C`) must match the oracle's inputs exactly.

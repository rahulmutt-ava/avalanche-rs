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

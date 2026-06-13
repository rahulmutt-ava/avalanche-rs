# `avalanchers` — Go → Rust porting matrix

Tracks the node binary port — Go `main/main.go` + `app/app.go` (`App`, `New`,
`Run`, `Header`) and `version/string.go` (`Versions`, `GetVersions`) — against
the `../avalanchego` reference tree, plus every documented deferral.

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

The binary is a thin shell (`src/main.rs`) over the library (`src/app.rs`); the
26-step node assembly + 14-step shutdown live in `ava-node` (M8.29/M8.30).

---

## `main/main.go` → `src/main.rs`

| Go (`main/main.go`) | Rust (`src/main.rs`) | Status | Notes |
|---|---|---|---|
| `evm.RegisterAllLibEVMExtras()` | (documented no-op comment, step 1) | n/a | reth registers precompiles/hooks at chain creation inside `ava-evm`; no process-global init exists. |
| `config.BuildFlagSet()` | `flags::build_command(FLAG_SPECS)` | ✅ | M8.3. |
| `config.BuildViper(fs, os.Args[1:])` + `pflag.ErrHelp` → exit 0 | `cmd.try_get_matches_from` → `ErrorKind::DisplayHelp` → `ExitCode::SUCCESS` | ✅ | clap prints help to stdout and carries the kind; other parse errors → exit 1. |
| `v.GetBool(VersionJSONKey) && v.GetBool(VersionKey)` → exit 1 | both `get_bool` true → `eprintln!` + `ExitCode::FAILURE` | ✅ | byte-identical message `can't print both JSON and human readable versions`. |
| `version.GetVersions()` + `json.MarshalIndent` → exit 0 | `serde_json::to_string_pretty(&app::versions())` → exit 0 | ✅ | see `version/string.go` row. |
| `version.GetVersions().String()` → exit 0 | `app::versions().line()` → exit 0 | ✅ | reconciled with the M0 `avalanchers/` invariant (below). |
| `config.GetNodeConfig(v)` | `ava_config::parse::get_node_config(&layered)` | ✅ | M8.12. |
| `term.IsTerminal(os.Stdout.Fd())` → `fmt.Println(app.Header)` | `app::stdout_is_tty()` (`std::io::IsTerminal`) → `app::HEADER` | ✅ | banner byte-identical to Go `app.Header`. |
| `app.New(nodeConfig)` | `app::chmod_r` + `app::build_log_factory` + `app::set_fd_limit` + `Node::new` | ✅ | see `app/app.go` rows. |
| `app.Run(nodeApp)` → `os.Exit(exitCode)` | `run(config)` → `ExitCode::from(node.exit_code())` | ✅ | single runtime owned here (17 §1.1). |

## `app/app.go` → `src/app.rs`

| Go (`app/app.go`) | Rust (`src/app.rs`) | Status | Notes |
|---|---|---|---|
| `const Header` | `pub const HEADER` | ✅ | byte-identical ASCII art (raw string). |
| `perms.ChmodR(DatabaseConfig.Path, true, ReadWriteExecute)` | `chmod_r(&config.database_config.path)` | ✅ | unix: recursive `set_permissions(0o700)`; missing dir = Ok; no-op on non-unix. |
| `perms.ChmodR(LoggingConfig.Directory, …)` | `chmod_r(&config.logging_config.directory)` | ✅ | same. |
| `logging.NewFactory(config.LoggingConfig)` | `build_log_factory` → `logging::init` + `LogFactory::new` | ✅ | M8.28 logging; init installs the global subscriber once. |
| `ulimit.Set(config.FdLimit, log)` | `set_fd_limit(config.fd_limit)` | 🟡 | unix `getrlimit`/`setrlimit(RLIMIT_NOFILE)`; clamps to hard limit, never lowers. The single isolated `unsafe` FFI (see below). No-op on non-unix. |
| `node.New(&config, logFactory, log)` | `ava_node::node::Node::new(Arc<config>, log_factory, handle)` | ✅ | M8.29 (takes the runtime `Handle`, 17 §1.1). |
| `app.Run`: `app.Start()` (spawn `node.Dispatch`) | `Node::dispatch().await` under `rt.block_on` | ✅ | M8.30; returns the exit code. |
| `signal.Notify(SIGINT, SIGTERM)` → `app.Stop()` | `install_signal_handlers`: `tokio::signal::unix` SIGINT/SIGTERM → `node.shutdown(0)` | ✅ | spawned task on the ambient runtime; non-unix falls back to `ctrl_c`. |
| `signal.Notify(SIGABRT)` → `utils.GetStacktrace(true)` to stderr | `install_signal_handlers`: SIGABRT → `dump_backtrace()` | 🟡 | dumps the handler thread's `std::backtrace::Backtrace` to stderr; Rust cannot enumerate every task's stack (Go reads all goroutines), so this is best-effort (17 §5). |
| `app.ExitCode()` (blocks on `exitWG`) | `Node::exit_code()` after `dispatch` returns | ✅ | first fatal `shutdown(code)` wins (17 §5). |

## `version/string.go` → `app::versions()`

| Go | Rust | Status | Notes |
|---|---|---|---|
| `type Versions {Application, Database, RPCChainVM, Commit, Go}` | `pub struct Versions` (serde) | ✅ | same JSON field names → `--version-json` is drop-in unmarshalable. |
| `GetVersions()` | `versions()` | ✅ | `application`=`CURRENT.display()`; `database`=`CURRENT_DATABASE`; `rpcchainvm`=`RPC_CHAIN_VM_PROTOCOL` (45). |
| `GitCommit` (ldflags `-X version.GitCommit`) | `commit` from `option_env!("AVALANCHERS_GIT_COMMIT")` | 🟡 | empty unless a build script injects it (Go's is also empty by default). |
| `runtime.Version()` (trimmed) | `go` from `option_env!("AVALANCHERS_RUSTC_VERSION")` | 🟡 | empty unless injected; field kept for shape/interop parity. |
| `Versions.String()` | `Versions::line()` | ✅* | *reconciled — prefixes `avalanchers/<semver>` (M0 invariant) before the Go-style `[application=…, database=…, rpcchainvm=…, (commit=…,) go=…]` detail. |

---

## Reconciliations

- **`--version` string.** Go prints `version.GetVersions().String()`
  (`avalanchego/1.14.2 [database=…, rpcchainvm=45, go=…]`). The existing M0 test
  (`tests/cli_version_help.rs`) requires the substring `avalanchers/`. `line()`
  satisfies both: `avalanchers/1.14.2 [application=avalanchego/1.14.2,
  database=v1.4.5, rpcchainvm=45, go=…]`. The JSON `application` field stays the
  pure Go value (`avalanchego/1.14.2`).

## `unsafe`

- `app::set_fd_limit` (unix only) wraps one `getrlimit`/`setrlimit` libc call in
  an `#[allow(unsafe_code)]` block with a `// SAFETY:` note — scoped exactly like
  `ava-vm-rpc/src/host/subprocess.rs`'s `prctl`. `lib.rs` `deny`s `unsafe` on
  unix (so the scoped `allow` is honored) and `forbid`s it elsewhere;
  `main.rs` is `#![forbid(unsafe_code)]`.

## CI gate

- `scripts/single_runtime_lint.sh` (CI job `single_runtime_lint` in
  `.github/workflows/ci.yml`; Task `lint-single-runtime`, wired into
  `lint-all`/`lint-all-ci`) forbids `Runtime::new` / `new_multi_thread` /
  `new_current_thread` / `block_on` outside `crates/avalanchers/src/main.rs` and
  test/bench files (specs/17 §1.1, 00 §7.2). Allowlisted: the rpcchainvm-plugin
  *client* bridges (`ava-database/src/rpcdb/client.rs`,
  `ava-vm-rpc/src/proxy/{rpcdb,sharedmemory}.rs`) — these own a tiny
  current-thread runtime to drive a *blocking* `Database`/`SharedMemory` trait
  over gRPC in a VM **plugin subprocess** (17 §1.2), not a second runtime in the
  node process.

## Deferrals

- **`apiURI` re-resolution for `--http-port=0`** — inherited from M8.30. When the
  HTTP listener binds an ephemeral port, `process.json`'s `uri` should be
  re-resolved from the bound address before being written. This needs a
  cross-crate bound-address accessor on `ava_api::Server`; **DEFERRED** (not
  attempted here).
- **`set_fd_limit` / SIGABRT** are best-effort (see rows above); both are unix
  shaped and no-op on non-unix, matching Go's unix-only `ulimit`/`GetStacktrace`.
- **Lifecycle smoke** (`./avalanchers` and `--network-id=fuji` actually start +
  stop like Go) is owned by the M8.32 milestone exit gate; M8.31 proves the
  config builds (`app::tests::build_config_for_mainnet_and_fuji`) without
  spawning the blocking node.

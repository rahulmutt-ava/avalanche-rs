// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `admin` API service — `/ext/admin`, **disabled by default**
//! (mirror Go `api/admin/service.go`; specs 12 §3.5, 14 §4).
//!
//! Thirteen methods, registered through `#[rpc_service("admin")]` so the
//! method set cannot drift (12 §3.2): `startCPUProfiler`, `stopCPUProfiler`,
//! `memoryProfile`, `lockProfile`, `alias`, `aliasChain`, `getChainAliases`,
//! `stacktrace`, `setLoggerLevel`, `getLoggerLevel`, `getConfig`, `loadVMs`,
//! `dbGet`.
//!
//! The node subsystems admin drives are taken as **narrow trait seams**
//! (precedent: `ava-wallet`'s client traits), mirroring exactly the interfaces
//! Go's `admin.Config` carries: [`AliasAdder`] (Go `server.PathAdderWithReadLock`),
//! [`ChainAliaser`] (Go `chains.Manager` alias surface), [`LoggerLevels`]
//! (Go `logging.Factory` level surface), [`VmRegistry`] (Go
//! `registry.VMRegistry.Reload` + `ids.GetRelevantAliases`), and the raw DB as
//! `ava_database::traits::KeyValueReader`. Node assembly (M8.29) provides the
//! live implementations.

pub mod profiler;
pub mod types;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ava_api_macros::rpc_service;
use ava_database::error::Error as DbError;
use ava_database::traits::KeyValueReader;
use ava_logging::AvaLevel;
use ava_types::id::Id;
use axum::Router;
use serde_json::Value;

use self::types::{
    AliasArgs, AliasChainArgs, DbGetArgs, DbGetReply, EmptyArgs, EmptyReply, GetChainAliasesArgs,
    GetChainAliasesReply, GetLoggerLevelArgs, LevelJson, LoadVmsReply, LogAndDisplayLevels,
    LoggerLevelReply, SetLoggerLevelArgs, db_error_code,
};
use crate::jsonrpc::{RpcError, ServiceRegistry};
use crate::server::ApiServer;

/// Go `maxAliasLength` — the longest accepted alias, in bytes.
const MAX_ALIAS_LENGTH: usize = 512;

/// Go `stacktraceFile` — written to the process working directory (Go uses the
/// bare relative path).
const STACKTRACE_FILE: &str = "stacktrace.txt";

/// Go `errAliasTooLong` (byte-exact message).
const ERR_ALIAS_TOO_LONG: &str = "alias length is too long";

/// Go `errNoLogLevel` (byte-exact message).
const ERR_NO_LOG_LEVEL: &str = "need to specify either displayLevel or logLevel";

/// The boxed error a seam implementation surfaces; its `to_string()` becomes
/// the `-32000` JSON-RPC message, exactly like a Go handler-returned `error`
/// (14 §16.1).
pub type SeamError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Go `server.PathAdderWithReadLock` — the only HTTP-server capability admin
/// needs (`admin.alias` / `admin.aliasChain` register endpoint aliases).
pub trait AliasAdder: Send + Sync {
    /// Registers `aliases` for an already-known `endpoint` (mirror Go
    /// `AddAliasesWithReadLock`).
    ///
    /// # Errors
    /// Propagates the server's alias-registration error (`-32000` on the wire).
    fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> crate::error::Result<()>;
}

/// Every [`ApiServer`] is an [`AliasAdder`]: `ApiServer::add_aliases` is
/// exactly the Go `PathAdderWithReadLock` surface admin consumes.
impl<T: ApiServer + ?Sized> AliasAdder for T {
    fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> crate::error::Result<()> {
        ApiServer::add_aliases(self, endpoint, aliases)
    }
}

/// The `chains.Manager` alias surface admin consumes (Go `admin.Config
/// .ChainManager`): `Lookup`, `Alias`, and `Aliases`.
pub trait ChainAliaser: Send + Sync {
    /// Resolves a chain alias (or stringified id) to the chain id (Go
    /// `Manager.Lookup`).
    ///
    /// # Errors
    /// An unknown alias (Go's `errUnknownChain`-style lookup failure).
    fn lookup(&self, alias: &str) -> Result<Id, SeamError>;

    /// Registers `alias` as a name for `chain_id` (Go `Manager.Alias`).
    ///
    /// # Errors
    /// An alias conflict or unknown chain.
    fn alias(&self, chain_id: Id, alias: &str) -> Result<(), SeamError>;

    /// All aliases registered for `chain_id` (Go `Manager.Aliases`).
    ///
    /// # Errors
    /// Propagates the manager's lookup failure.
    fn aliases(&self, chain_id: Id) -> Result<Vec<String>, SeamError>;
}

/// The `logging.Factory` per-logger level surface (Go `admin.Config
/// .LogFactory`): names, plus get/set of the file ("log") and display levels.
pub trait LoggerLevels: Send + Sync {
    /// The names of every registered logger (Go `GetLoggerNames`).
    fn logger_names(&self) -> Vec<String>;

    /// The file ("log") level of logger `name` (Go `GetLogLevel`).
    ///
    /// # Errors
    /// If no logger with that name exists.
    fn log_level(&self, name: &str) -> Result<AvaLevel, SeamError>;

    /// The display level of logger `name` (Go `GetDisplayLevel`).
    ///
    /// # Errors
    /// If no logger with that name exists.
    fn display_level(&self, name: &str) -> Result<AvaLevel, SeamError>;

    /// Sets the file ("log") level of logger `name` (Go `SetLogLevel`).
    ///
    /// # Errors
    /// If no logger with that name exists or the reload handle is dead.
    fn set_log_level(&self, name: &str, level: AvaLevel) -> Result<(), SeamError>;

    /// Sets the display level of logger `name` (Go `SetDisplayLevel`).
    ///
    /// # Errors
    /// If no logger with that name exists or the reload handle is dead.
    fn set_display_level(&self, name: &str, level: AvaLevel) -> Result<(), SeamError>;
}

/// The outcome of a plugin-dir rescan (Go `VMRegistry.Reload` composed with
/// `ids.GetRelevantAliases`, which the node-assembly implementation performs).
#[derive(Debug, Clone, Default)]
pub struct VmReload {
    /// Newly registered VMs and their aliases (Go `LoadVMsReply.NewVMs`).
    pub new_vms: BTreeMap<Id, Vec<String>>,
    /// VMs that failed to register, with the error message (Go `FailedVMs`).
    pub failed_vms: BTreeMap<Id, String>,
}

/// The VM-registry surface admin consumes for `loadVMs` (Go `admin.Config
/// .VMRegistry` + `.VMManager`).
#[async_trait::async_trait]
pub trait VmRegistry: Send + Sync {
    /// Rescans `plugin-dir`, registering any new VMs (Go `VMRegistry.Reload`),
    /// and resolves the aliases of the newly loaded VMs.
    ///
    /// # Errors
    /// A registry-level failure (individual VM failures are reported in
    /// [`VmReload::failed_vms`], not as an error).
    async fn reload(&self) -> Result<VmReload, SeamError>;
}

/// The dependencies of the admin service (mirror Go `admin.Config`).
pub struct AdminConfig {
    /// `--profile-dir`: where cpu/mem/lock profiles are written.
    pub profile_dir: PathBuf,
    /// Per-logger level get/set (Go `LogFactory`).
    pub log_levels: Arc<dyn LoggerLevels>,
    /// The resolved node config, pre-serialized to JSON (Go stores it as
    /// `interface{}`; node assembly serializes the `ava_config` `Config`,
    /// providedFlags-aware — 13 §23).
    pub node_config: Value,
    /// The raw node database (`dbGet`).
    pub db: Arc<dyn KeyValueReader + Send + Sync>,
    /// The chain manager's alias surface (`aliasChain` / `getChainAliases`).
    pub chain_manager: Arc<dyn ChainAliaser>,
    /// The HTTP server's alias registry (`alias` / `aliasChain`).
    pub http_server: Arc<dyn AliasAdder>,
    /// The VM registry (`loadVMs`).
    pub vm_registry: Arc<dyn VmRegistry>,
}

/// The admin API service (mirror Go `admin.Admin`).
pub struct Admin {
    cfg: AdminConfig,
    profiler: profiler::Profiler,
}

impl Admin {
    /// Builds the service from its dependencies (mirror Go `admin.NewService`).
    #[must_use]
    pub fn new(cfg: AdminConfig) -> Arc<Self> {
        let profiler = profiler::Profiler::new(&cfg.profile_dir);
        Arc::new(Self { cfg, profiler })
    }

    /// The mountable `/ext/admin` handler: a router dispatching JSON-RPC POSTs
    /// to this service's registered methods (node assembly mounts it only when
    /// `api-admin-enabled`, 12 §3.5).
    pub fn into_handler(self: Arc<Self>) -> Router {
        let mut registry = ServiceRegistry::new();
        self.register_rpc(&mut registry);
        Router::new()
            .route("/", axum::routing::any(crate::jsonrpc::dispatch))
            .with_state(Arc::new(registry))
    }

    /// Go `getLoggerNames`: the empty name means **all** loggers.
    fn logger_names(&self, logger_name: &str) -> Vec<String> {
        if logger_name.is_empty() {
            self.cfg.log_levels.logger_names()
        } else {
            vec![logger_name.to_string()]
        }
    }

    /// Go `getLogLevels`: the {log, display} level of each named logger.
    fn log_levels(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, LogAndDisplayLevels>, RpcError> {
        let mut levels = BTreeMap::new();
        for name in names {
            let log_level = self.cfg.log_levels.log_level(name).map_err(seam_err)?;
            let display_level = self.cfg.log_levels.display_level(name).map_err(seam_err)?;
            levels.insert(
                name.clone(),
                LogAndDisplayLevels {
                    log_level: LevelJson(log_level),
                    display_level: LevelJson(display_level),
                },
            );
        }
        Ok(levels)
    }
}

/// A seam failure surfaces exactly like a Go handler-returned `error`:
/// `-32000` with the message set to `to_string()` (14 §16.1).
fn seam_err(e: SeamError) -> RpcError {
    RpcError::server(e.to_string())
}

/// Go `formatting.Decode(formatting.HexNC, …)`: the empty string is the empty
/// key (nil bytes); otherwise a `0x` prefix is required (byte-exact
/// `errMissingHexPrefix` message) and the remainder hex-decoded.
fn hex_nc_decode(s: &str) -> Result<Vec<u8>, RpcError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let Some(stripped) = s.strip_prefix("0x") else {
        return Err(RpcError::server("missing 0x prefix to hex encoding"));
    };
    hex::decode(stripped).map_err(|e| RpcError::from_error(&e))
}

/// Go `formatting.Encode(formatting.HexNC, …)`: `0x` + hex, no checksum.
fn hex_nc_encode(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

/// The 13 `admin.*` methods (Go `api/admin/service.go`; 14 §4). Names with
/// inner acronyms carry `#[rpc(name = …)]` because dispatch matches the
/// remainder of the method segment EXACTLY (`StartCPUProfiler`,
/// `StopCPUProfiler`, `LoadVMs`); Go deliberately named the dbGet handler
/// `DbGet` (renaming it `DBGet` would change the API to `dBGet`), which is
/// exactly what `pascalize(db_get)` produces.
#[rpc_service("admin")]
impl Admin {
    /// `admin.startCPUProfiler` — starts a CPU profile writing to
    /// `<profile-dir>/cpu.profile`.
    #[rpc(name = "StartCPUProfiler")]
    pub async fn start_cpu_profiler(&self, _args: EmptyArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(service = "admin", method = "startCPUProfiler", "API called");
        self.profiler
            .start_cpu_profiler()
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.stopCPUProfiler` — stops the CPU profile and writes it out.
    #[rpc(name = "StopCPUProfiler")]
    pub async fn stop_cpu_profiler(&self, _args: EmptyArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(service = "admin", method = "stopCPUProfiler", "API called");
        self.profiler
            .stop_cpu_profiler()
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.memoryProfile` — Go dumps the heap profile; unsupported here
    /// (see [`profiler`] module docs).
    pub async fn memory_profile(&self, _args: EmptyArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(service = "admin", method = "memoryProfile", "API called");
        self.profiler
            .memory_profile()
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.lockProfile` — Go dumps the mutex profile; unsupported here
    /// (see [`profiler`] module docs).
    pub async fn lock_profile(&self, _args: EmptyArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(service = "admin", method = "lockProfile", "API called");
        self.profiler
            .lock_profile()
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.alias` — aliases an HTTP endpoint to a new name.
    pub async fn alias(&self, args: AliasArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(
            service = "admin",
            method = "alias",
            endpoint = %args.endpoint,
            alias = %args.alias,
            "API called"
        );
        if args.alias.len() > MAX_ALIAS_LENGTH {
            return Err(RpcError::server(ERR_ALIAS_TOO_LONG));
        }
        self.cfg
            .http_server
            .add_aliases(&args.endpoint, std::slice::from_ref(&args.alias))
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.aliasChain` — aliases a chain (chain manager + the
    /// `/ext/bc/<alias>` HTTP route).
    pub async fn alias_chain(&self, args: AliasChainArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(
            service = "admin",
            method = "aliasChain",
            chain = %args.chain,
            alias = %args.alias,
            "API called"
        );
        if args.alias.len() > MAX_ALIAS_LENGTH {
            return Err(RpcError::server(ERR_ALIAS_TOO_LONG));
        }
        let chain_id = self
            .cfg
            .chain_manager
            .lookup(&args.chain)
            .map_err(seam_err)?;
        self.cfg
            .chain_manager
            .alias(chain_id, &args.alias)
            .map_err(seam_err)?;
        // Go: path.Join(constants.ChainAliasPrefix, …) on both sides.
        let endpoint = format!("bc/{chain_id}");
        let alias = format!("bc/{}", args.alias);
        self.cfg
            .http_server
            .add_aliases(&endpoint, &[alias])
            .map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.getChainAliases` — the aliases of the chain (the argument is a
    /// stringified chain id, NOT an alias — mirror Go `ids.FromString`).
    pub async fn get_chain_aliases(
        &self,
        args: GetChainAliasesArgs,
    ) -> Result<GetChainAliasesReply, RpcError> {
        tracing::debug!(
            service = "admin",
            method = "getChainAliases",
            chain = %args.chain,
            "API called"
        );
        let id: Id = args.chain.parse().map_err(|e| RpcError::from_error(&e))?;
        let aliases = self.cfg.chain_manager.aliases(id).map_err(seam_err)?;
        Ok(GetChainAliasesReply { aliases })
    }

    /// `admin.stacktrace` — Go dumps all goroutine stacks to `stacktrace.txt`
    /// in the working directory. Rust has no all-task/all-thread dump on
    /// stable (tokio's task dump is unstable-only), so this writes the
    /// **calling thread's** backtrace — real, but partial — with a header
    /// naming the limitation.
    pub async fn stacktrace(&self, _args: EmptyArgs) -> Result<EmptyReply, RpcError> {
        tracing::debug!(service = "admin", method = "stacktrace", "API called");
        let backtrace = std::backtrace::Backtrace::force_capture();
        let content = format!(
            "avalanche-rs best-effort stacktrace: Rust has no Go-style \
             all-goroutine dump; this is the backtrace of the thread serving \
             admin.stacktrace.\n{backtrace}\n"
        );
        std::fs::write(STACKTRACE_FILE, content).map_err(|e| RpcError::from_error(&e))?;
        Ok(EmptyReply {})
    }

    /// `admin.setLoggerLevel` — sets the log and/or display level of one
    /// logger (or all, when `loggerName` is empty) and returns the resulting
    /// levels. At least one of `logLevel` / `displayLevel` is required.
    pub async fn set_logger_level(
        &self,
        args: SetLoggerLevelArgs,
    ) -> Result<LoggerLevelReply, RpcError> {
        tracing::debug!(
            service = "admin",
            method = "setLoggerLevel",
            logger_name = %args.logger_name,
            "API called"
        );
        if args.log_level.is_none() && args.display_level.is_none() {
            return Err(RpcError::server(ERR_NO_LOG_LEVEL));
        }
        let names = self.logger_names(&args.logger_name);
        for name in &names {
            if let Some(LevelJson(level)) = args.log_level {
                self.cfg
                    .log_levels
                    .set_log_level(name, level)
                    .map_err(seam_err)?;
            }
            if let Some(LevelJson(level)) = args.display_level {
                self.cfg
                    .log_levels
                    .set_display_level(name, level)
                    .map_err(seam_err)?;
            }
        }
        Ok(LoggerLevelReply {
            logger_levels: self.log_levels(&names)?,
        })
    }

    /// `admin.getLoggerLevel` — the log and display levels of one logger (or
    /// all, when `loggerName` is empty).
    pub async fn get_logger_level(
        &self,
        args: GetLoggerLevelArgs,
    ) -> Result<LoggerLevelReply, RpcError> {
        tracing::debug!(
            service = "admin",
            method = "getLoggerLevel",
            logger_name = %args.logger_name,
            "API called"
        );
        let names = self.logger_names(&args.logger_name);
        Ok(LoggerLevelReply {
            logger_levels: self.log_levels(&names)?,
        })
    }

    /// `admin.getConfig` — the resolved node config as JSON (Go returns the
    /// `interface{}` it was constructed with).
    pub async fn get_config(&self, _args: EmptyArgs) -> Result<Value, RpcError> {
        tracing::debug!(service = "admin", method = "getConfig", "API called");
        Ok(self.cfg.node_config.clone())
    }

    /// `admin.loadVMs` — rescans `plugin-dir` and reports the newly loaded
    /// VMs (with aliases) and any per-VM failures.
    #[rpc(name = "LoadVMs")]
    pub async fn load_vms(&self, _args: EmptyArgs) -> Result<LoadVmsReply, RpcError> {
        tracing::debug!(service = "admin", method = "loadVMs", "API called");
        let outcome = self.cfg.vm_registry.reload().await.map_err(seam_err)?;
        Ok(LoadVmsReply {
            new_vms: outcome.new_vms,
            failed_vms: outcome.failed_vms,
        })
    }

    /// `admin.dbGet` — raw DB read by HexNC key. A mapped database error
    /// (`closed` / `not found`) is reported via `errorCode` with a SUCCESS
    /// response (Go `rpcdb.ErrorToRPCError` returns nil for mapped errors);
    /// anything else is a JSON-RPC error.
    pub async fn db_get(&self, args: DbGetArgs) -> Result<DbGetReply, RpcError> {
        tracing::debug!(service = "admin", method = "dbGet", key = %args.key, "API called");
        let key = hex_nc_decode(&args.key)?;
        match self.cfg.db.get(&key) {
            Ok(value) => Ok(DbGetReply {
                value: hex_nc_encode(&value),
                error_code: db_error_code::UNSPECIFIED,
            }),
            Err(DbError::Closed) => Ok(DbGetReply {
                value: String::new(),
                error_code: db_error_code::CLOSED,
            }),
            Err(DbError::NotFound) => Ok(DbGetReply {
                value: String::new(),
                error_code: db_error_code::NOT_FOUND,
            }),
            Err(e) => Err(RpcError::from_error(&e)),
        }
    }
}

#[cfg(test)]
// `serde_json::Value` indexing is the idiomatic way to assert on JSON-RPC
// bodies (missing keys yield `Value::Null`, not a panic) — same allowance as
// `jsonrpc::tests`.
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::collections::BTreeMap;

    use ava_database::error::Error as DbError;
    use pretty_assertions::assert_eq;

    use super::types::*;
    use super::*;

    // ------------------------------------------------------------------
    // Narrow local mocks for the seams (the project prefers hand-rolled,
    // test-local mocks over generated ones).
    // ------------------------------------------------------------------

    /// Records `add_aliases` calls; fails when `fail` is set.
    #[derive(Default)]
    struct MockAliasAdder {
        calls: parking_lot::Mutex<Vec<(String, Vec<String>)>>,
    }

    impl AliasAdder for MockAliasAdder {
        fn add_aliases(&self, endpoint: &str, aliases: &[String]) -> crate::error::Result<()> {
            self.calls
                .lock()
                .push((endpoint.to_string(), aliases.to_vec()));
            Ok(())
        }
    }

    /// A single-chain manager: knows `chain_id` under alias "C".
    struct MockChainAliaser {
        chain_id: Id,
        aliased: parking_lot::Mutex<Vec<(Id, String)>>,
    }

    impl MockChainAliaser {
        fn new(chain_id: Id) -> Self {
            Self {
                chain_id,
                aliased: parking_lot::Mutex::new(Vec::new()),
            }
        }
    }

    impl ChainAliaser for MockChainAliaser {
        fn lookup(&self, alias: &str) -> Result<Id, SeamError> {
            if alias == "C" || alias == self.chain_id.to_string() {
                Ok(self.chain_id)
            } else {
                Err(format!("there is no chain with alias/ID '{alias}'").into())
            }
        }

        fn alias(&self, chain_id: Id, alias: &str) -> Result<(), SeamError> {
            self.aliased.lock().push((chain_id, alias.to_string()));
            Ok(())
        }

        fn aliases(&self, chain_id: Id) -> Result<Vec<String>, SeamError> {
            if chain_id == self.chain_id {
                Ok(vec!["C".to_string(), chain_id.to_string()])
            } else {
                Ok(Vec::new())
            }
        }
    }

    /// Two loggers ("main", "C") with independently settable levels.
    struct MockLoggerLevels {
        levels: parking_lot::Mutex<BTreeMap<String, (AvaLevel, AvaLevel)>>,
    }

    impl MockLoggerLevels {
        fn new() -> Self {
            let mut levels = BTreeMap::new();
            levels.insert("main".to_string(), (AvaLevel::Info, AvaLevel::Info));
            levels.insert("C".to_string(), (AvaLevel::Debug, AvaLevel::Info));
            Self {
                levels: parking_lot::Mutex::new(levels),
            }
        }
    }

    impl LoggerLevels for MockLoggerLevels {
        fn logger_names(&self) -> Vec<String> {
            self.levels.lock().keys().cloned().collect()
        }

        fn log_level(&self, name: &str) -> Result<AvaLevel, SeamError> {
            self.levels
                .lock()
                .get(name)
                .map(|(log, _)| *log)
                .ok_or_else(|| format!("logger {name} does not exist").into())
        }

        fn display_level(&self, name: &str) -> Result<AvaLevel, SeamError> {
            self.levels
                .lock()
                .get(name)
                .map(|(_, display)| *display)
                .ok_or_else(|| format!("logger {name} does not exist").into())
        }

        fn set_log_level(&self, name: &str, level: AvaLevel) -> Result<(), SeamError> {
            match self.levels.lock().get_mut(name) {
                Some(entry) => {
                    entry.0 = level;
                    Ok(())
                }
                None => Err(format!("logger {name} does not exist").into()),
            }
        }

        fn set_display_level(&self, name: &str, level: AvaLevel) -> Result<(), SeamError> {
            match self.levels.lock().get_mut(name) {
                Some(entry) => {
                    entry.1 = level;
                    Ok(())
                }
                None => Err(format!("logger {name} does not exist").into()),
            }
        }
    }

    /// A fixed-outcome VM registry.
    struct MockVmRegistry {
        outcome: VmReload,
    }

    #[async_trait::async_trait]
    impl VmRegistry for MockVmRegistry {
        async fn reload(&self) -> Result<VmReload, SeamError> {
            Ok(self.outcome.clone())
        }
    }

    /// A KV reader over a fixed map; `closed` simulates `database.ErrClosed`.
    #[derive(Default)]
    struct MockDb {
        entries: BTreeMap<Vec<u8>, Vec<u8>>,
        closed: bool,
    }

    impl KeyValueReader for MockDb {
        fn has(&self, key: &[u8]) -> ava_database::error::Result<bool> {
            if self.closed {
                return Err(DbError::Closed);
            }
            Ok(self.entries.contains_key(key))
        }

        fn get(&self, key: &[u8]) -> ava_database::error::Result<Vec<u8>> {
            if self.closed {
                return Err(DbError::Closed);
            }
            self.entries.get(key).cloned().ok_or(DbError::NotFound)
        }
    }

    /// An Admin over fresh mocks, with the profile dir in `dir`.
    fn test_admin_in(dir: &std::path::Path, db: MockDb, vms: VmReload) -> Arc<Admin> {
        let chain_id = Id::from_slice(&[7u8; 32]).expect("32-byte id");
        Admin::new(AdminConfig {
            profile_dir: dir.to_path_buf(),
            log_levels: Arc::new(MockLoggerLevels::new()),
            node_config: serde_json::json!({ "networkID": 1, "httpPort": 9650 }),
            db: Arc::new(db),
            chain_manager: Arc::new(MockChainAliaser::new(chain_id)),
            http_server: Arc::new(MockAliasAdder::default()),
            vm_registry: Arc::new(MockVmRegistry { outcome: vms }),
        })
    }

    fn test_admin() -> (tempfile::TempDir, Arc<Admin>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let admin = test_admin_in(dir.path(), MockDb::default(), VmReload::default());
        (dir, admin)
    }

    // ------------------------------------------------------------------
    // Step 1 (Red): the registered `admin` method set is exactly the 13 Go
    // wire names (14 §4) — including the acronym overrides StartCPUProfiler /
    // StopCPUProfiler / LoadVMs and the deliberate Go `DbGet` casing.
    // ------------------------------------------------------------------
    #[test]
    fn admin_method_set() {
        let (_dir, admin) = test_admin();
        let mut reg = ServiceRegistry::new();
        admin.register_rpc(&mut reg);

        // Registered method names as gorilla matches them (first letter of the
        // client's `admin.<method>` segment uppercased, remainder EXACT).
        let want = [
            "StartCPUProfiler",
            "StopCPUProfiler",
            "MemoryProfile",
            "LockProfile",
            "Alias",
            "AliasChain",
            "GetChainAliases",
            "Stacktrace",
            "SetLoggerLevel",
            "GetLoggerLevel",
            "GetConfig",
            "LoadVMs",
            "DbGet",
        ];
        for name in want {
            assert!(
                reg.lookup("admin", name).is_some(),
                "admin.{name} must be registered"
            );
        }
        assert_eq!(reg.len(), want.len(), "exactly the 13 admin methods");

        // Drift guards: the snake_case-pascalized names that would be WRONG on
        // the wire must NOT be registered.
        assert!(reg.lookup("admin", "StartCpuProfiler").is_none());
        assert!(reg.lookup("admin", "StopCpuProfiler").is_none());
        assert!(reg.lookup("admin", "LoadVms").is_none());
        assert!(reg.lookup("admin", "DBGet").is_none());
    }

    // ------------------------------------------------------------------
    // alias / aliasChain
    // ------------------------------------------------------------------

    /// `alias` rejects `len(alias) > 512` with the byte-exact Go message and
    /// accepts exactly 512; an accepted alias reaches the HTTP server seam.
    #[tokio::test]
    async fn alias_rejects_too_long_alias() {
        let dir = tempfile::tempdir().expect("tempdir");
        let http_server = Arc::new(MockAliasAdder::default());
        let chain_id = Id::from_slice(&[7u8; 32]).expect("32-byte id");
        let admin = Admin::new(AdminConfig {
            profile_dir: dir.path().to_path_buf(),
            log_levels: Arc::new(MockLoggerLevels::new()),
            node_config: serde_json::json!({}),
            db: Arc::new(MockDb::default()),
            chain_manager: Arc::new(MockChainAliaser::new(chain_id)),
            http_server: http_server.clone(),
            vm_registry: Arc::new(MockVmRegistry {
                outcome: VmReload::default(),
            }),
        });

        // 513 bytes: rejected, seam never called.
        let err = admin
            .alias(AliasArgs {
                endpoint: "bc/X".to_string(),
                alias: "a".repeat(513),
            })
            .await
            .expect_err("alias longer than 512 must be rejected");
        assert_eq!(err.message, "alias length is too long", "admin.alias");
        assert_eq!(err.code, crate::error::json2_code::SERVER);
        assert!(http_server.calls.lock().is_empty());

        // Exactly 512 bytes: accepted and forwarded to the alias registry.
        let alias512 = "a".repeat(512);
        admin
            .alias(AliasArgs {
                endpoint: "bc/X".to_string(),
                alias: alias512.clone(),
            })
            .await
            .expect("512-byte alias is accepted");
        assert_eq!(
            http_server.calls.lock().as_slice(),
            &[("bc/X".to_string(), vec![alias512])]
        );
    }

    /// `aliasChain` length-checks, registers the chain alias with the chain
    /// manager, and aliases the `bc/<chainID>` route to `bc/<alias>` (Go
    /// `path.Join(constants.ChainAliasPrefix, …)` on both sides).
    #[tokio::test]
    async fn alias_chain_registers_chain_and_route_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let chain_id = Id::from_slice(&[7u8; 32]).expect("32-byte id");
        let chain_manager = Arc::new(MockChainAliaser::new(chain_id));
        let http_server = Arc::new(MockAliasAdder::default());
        let admin = Admin::new(AdminConfig {
            profile_dir: dir.path().to_path_buf(),
            log_levels: Arc::new(MockLoggerLevels::new()),
            node_config: serde_json::json!({}),
            db: Arc::new(MockDb::default()),
            chain_manager: chain_manager.clone(),
            http_server: http_server.clone(),
            vm_registry: Arc::new(MockVmRegistry {
                outcome: VmReload::default(),
            }),
        });

        let err = admin
            .alias_chain(AliasChainArgs {
                chain: "C".to_string(),
                alias: "a".repeat(513),
            })
            .await
            .expect_err("aliasChain alias longer than 512 must be rejected");
        assert_eq!(err.message, "alias length is too long", "admin.aliasChain");

        let err = admin
            .alias_chain(AliasChainArgs {
                chain: "unknown".to_string(),
                alias: "X".to_string(),
            })
            .await
            .expect_err("unknown chain must be rejected");
        assert_eq!(
            err.message, "there is no chain with alias/ID 'unknown'",
            "admin.aliasChain lookup"
        );

        admin
            .alias_chain(AliasChainArgs {
                chain: "C".to_string(),
                alias: "mychain".to_string(),
            })
            .await
            .expect("aliasChain");
        assert_eq!(
            chain_manager.aliased.lock().as_slice(),
            &[(chain_id, "mychain".to_string())]
        );
        assert_eq!(
            http_server.calls.lock().as_slice(),
            &[(format!("bc/{chain_id}"), vec!["bc/mychain".to_string()])]
        );
    }

    /// `getChainAliases` parses the chain as an id (NOT an alias) and returns
    /// the chain manager's aliases.
    #[tokio::test]
    async fn get_chain_aliases_parses_id() {
        let (_dir, admin) = test_admin();
        let chain_id = Id::from_slice(&[7u8; 32]).expect("32-byte id");

        let reply = admin
            .get_chain_aliases(GetChainAliasesArgs {
                chain: chain_id.to_string(),
            })
            .await
            .expect("getChainAliases");
        assert_eq!(
            reply.aliases,
            vec!["C".to_string(), chain_id.to_string()],
            "admin.getChainAliases"
        );

        // A non-id string (an alias) is a parse error, mirroring Go
        // `ids.FromString`.
        let err = admin
            .get_chain_aliases(GetChainAliasesArgs {
                chain: "C".to_string(),
            })
            .await
            .expect_err("an alias is not a valid chain id");
        assert_eq!(err.code, crate::error::json2_code::SERVER);
    }

    // ------------------------------------------------------------------
    // setLoggerLevel / getLoggerLevel
    // ------------------------------------------------------------------

    /// `setLoggerLevel` requires at least one of logLevel / displayLevel,
    /// with the byte-exact Go `errNoLogLevel` message.
    #[tokio::test]
    async fn set_logger_level_requires_a_level() {
        let (_dir, admin) = test_admin();
        let err = admin
            .set_logger_level(SetLoggerLevelArgs {
                logger_name: "main".to_string(),
                log_level: None,
                display_level: None,
            })
            .await
            .expect_err("setLoggerLevel with neither level must fail");
        assert_eq!(
            err.message, "need to specify either displayLevel or logLevel",
            "admin.setLoggerLevel"
        );
        assert_eq!(err.code, crate::error::json2_code::SERVER);
    }

    /// `setLoggerLevel` with an empty name sets ALL loggers; the reply carries
    /// the resulting per-logger levels.
    #[tokio::test]
    async fn set_logger_level_empty_name_sets_all() {
        let (_dir, admin) = test_admin();
        let reply = admin
            .set_logger_level(SetLoggerLevelArgs {
                logger_name: String::new(),
                log_level: Some(LevelJson(AvaLevel::Verbo)),
                display_level: None,
            })
            .await
            .expect("setLoggerLevel all loggers");
        assert_eq!(reply.logger_levels.len(), 2, "both loggers updated");
        for (name, levels) in &reply.logger_levels {
            assert_eq!(
                levels.log_level,
                LevelJson(AvaLevel::Verbo),
                "log level of {name}"
            );
        }
        // Display levels untouched ("main" started at Info).
        assert_eq!(
            reply.logger_levels["main"].display_level,
            LevelJson(AvaLevel::Info)
        );
    }

    /// `getLoggerLevel` returns one logger when named, all when empty; an
    /// unknown name surfaces the seam error.
    #[tokio::test]
    async fn get_logger_level_named_and_all() {
        let (_dir, admin) = test_admin();

        let reply = admin
            .get_logger_level(GetLoggerLevelArgs {
                logger_name: "C".to_string(),
            })
            .await
            .expect("getLoggerLevel C");
        assert_eq!(reply.logger_levels.len(), 1);
        assert_eq!(
            reply.logger_levels["C"].log_level,
            LevelJson(AvaLevel::Debug)
        );

        let reply = admin
            .get_logger_level(GetLoggerLevelArgs {
                logger_name: String::new(),
            })
            .await
            .expect("getLoggerLevel all");
        assert_eq!(reply.logger_levels.len(), 2);

        let err = admin
            .get_logger_level(GetLoggerLevelArgs {
                logger_name: "nope".to_string(),
            })
            .await
            .expect_err("unknown logger");
        assert_eq!(err.message, "logger nope does not exist");
    }

    /// Go `logging.Level` JSON parity: levels marshal UPPERCASE and unmarshal
    /// case-insensitively; an unknown level is rejected.
    #[test]
    fn level_json_casing_matches_go() {
        let reply = LoggerLevelReply {
            logger_levels: BTreeMap::from([(
                "main".to_string(),
                LogAndDisplayLevels {
                    log_level: LevelJson(AvaLevel::Verbo),
                    display_level: LevelJson(AvaLevel::Info),
                },
            )]),
        };
        let json = serde_json::to_value(&reply).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "loggerLevels": { "main": { "logLevel": "VERBO", "displayLevel": "INFO" } }
            })
        );

        // Unmarshal accepts any case (Go ToLevel upper-cases its input).
        let args: SetLoggerLevelArgs = serde_json::from_value(serde_json::json!({
            "loggerName": "main", "logLevel": "verbo", "displayLevel": "INFO"
        }))
        .expect("deserialize");
        assert_eq!(args.log_level, Some(LevelJson(AvaLevel::Verbo)));
        assert_eq!(args.display_level, Some(LevelJson(AvaLevel::Info)));

        let err = serde_json::from_value::<SetLoggerLevelArgs>(serde_json::json!({
            "logLevel": "loud"
        }))
        .expect_err("unknown level");
        assert!(
            err.to_string().contains("unknown log level: \"loud\""),
            "got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // getConfig / loadVMs
    // ------------------------------------------------------------------

    /// `getConfig` echoes the node config JSON it was constructed with.
    #[tokio::test]
    async fn get_config_returns_node_config() {
        let (_dir, admin) = test_admin();
        let config = admin.get_config(EmptyArgs {}).await.expect("getConfig");
        assert_eq!(
            config,
            serde_json::json!({ "networkID": 1, "httpPort": 9650 })
        );
    }

    /// `loadVMs` reply shape: `newVMs` always present (id-keyed), `failedVMs`
    /// omitted when empty (Go `omitempty`).
    #[tokio::test]
    async fn load_vms_reply_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vm_id = Id::from_slice(&[9u8; 32]).expect("32-byte id");

        // Empty outcome: newVMs == {}, failedVMs omitted.
        let admin = test_admin_in(dir.path(), MockDb::default(), VmReload::default());
        let reply = admin.load_vms(EmptyArgs {}).await.expect("loadVMs");
        let json = serde_json::to_value(&reply).expect("serialize");
        assert_eq!(json, serde_json::json!({ "newVMs": {} }), "empty loadVMs");

        // Non-empty: both maps present, keyed by the VM id string.
        let admin = test_admin_in(
            dir.path(),
            MockDb::default(),
            VmReload {
                new_vms: BTreeMap::from([(vm_id, vec!["myvm".to_string()])]),
                failed_vms: BTreeMap::from([(vm_id, "oops".to_string())]),
            },
        );
        let reply = admin.load_vms(EmptyArgs {}).await.expect("loadVMs");
        let json = serde_json::to_value(&reply).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "newVMs": { (vm_id.to_string()): ["myvm"] },
                "failedVMs": { (vm_id.to_string()): "oops" },
            }),
            "loadVMs reply keys are ids.ID strings"
        );
    }

    // ------------------------------------------------------------------
    // dbGet
    // ------------------------------------------------------------------

    /// `dbGet` parity: HexNC key in / value out, the Go `errorCode` mapping
    /// (mapped DB errors are a SUCCESS reply with the enum set), and the
    /// byte-exact missing-prefix message.
    #[tokio::test]
    async fn db_get_error_code_mapping() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = MockDb::default();
        db.entries.insert(b"hi".to_vec(), b"world".to_vec());
        let admin = test_admin_in(dir.path(), db, VmReload::default());

        // Found: HexNC-encoded value, errorCode ERROR_UNSPECIFIED (0).
        let reply = admin
            .db_get(DbGetArgs {
                key: format!("0x{}", hex::encode(b"hi")),
            })
            .await
            .expect("dbGet found");
        assert_eq!(reply.value, format!("0x{}", hex::encode(b"world")));
        assert_eq!(reply.error_code, db_error_code::UNSPECIFIED);

        // Not found: SUCCESS reply, empty value, errorCode ERROR_NOT_FOUND (2).
        let reply = admin
            .db_get(DbGetArgs {
                key: "0xdeadbeef".to_string(),
            })
            .await
            .expect("dbGet not-found is NOT a JSON-RPC error");
        assert_eq!(reply.value, "");
        assert_eq!(reply.error_code, db_error_code::NOT_FOUND);

        // The wire field names are value/errorCode and errorCode is a NUMBER.
        let json = serde_json::to_value(&reply).expect("serialize");
        assert_eq!(json, serde_json::json!({ "value": "", "errorCode": 2 }));

        // Missing 0x prefix: byte-exact Go errMissingHexPrefix.
        let err = admin
            .db_get(DbGetArgs {
                key: "abcd".to_string(),
            })
            .await
            .expect_err("missing 0x prefix");
        assert_eq!(err.message, "missing 0x prefix to hex encoding");

        // Closed DB: SUCCESS reply with errorCode ERROR_CLOSED (1).
        let admin = test_admin_in(
            dir.path(),
            MockDb {
                entries: BTreeMap::new(),
                closed: true,
            },
            VmReload::default(),
        );
        let reply = admin
            .db_get(DbGetArgs {
                key: "0x00".to_string(),
            })
            .await
            .expect("dbGet on closed DB is NOT a JSON-RPC error");
        assert_eq!(reply.error_code, db_error_code::CLOSED);
    }

    // ------------------------------------------------------------------
    // profiler + stacktrace
    // ------------------------------------------------------------------

    /// CPU profiler round-trip: start writes nothing yet, stop produces a
    /// non-empty `cpu.profile`; double-start / stop-without-start use the
    /// byte-exact Go error messages; memory/lock profiles are documented
    /// unsupported.
    #[tokio::test]
    async fn profiler_lifecycle_and_unsupported_profiles() {
        let (dir, admin) = test_admin();

        // Stop before start: Go errCPUProfilerNotRunning.
        let err = admin
            .stop_cpu_profiler(EmptyArgs {})
            .await
            .expect_err("stop without start");
        assert_eq!(err.message, "cpu profiler doesn't exist");

        admin
            .start_cpu_profiler(EmptyArgs {})
            .await
            .expect("startCPUProfiler");

        // Double start: Go errCPUProfilerRunning.
        let err = admin
            .start_cpu_profiler(EmptyArgs {})
            .await
            .expect_err("double start");
        assert_eq!(err.message, "cpu profiler already running");

        // Burn a little CPU so the profile has samples to encode.
        let mut acc: u64 = 0;
        for i in 0..2_000_000u64 {
            acc = acc.wrapping_mul(31).wrapping_add(i);
        }
        assert!(acc != 1, "keep the loop observable");

        admin
            .stop_cpu_profiler(EmptyArgs {})
            .await
            .expect("stopCPUProfiler");
        let profile = std::fs::read(dir.path().join(profiler::CPU_PROFILE_FILE))
            .expect("cpu.profile written");
        assert!(!profile.is_empty(), "cpu.profile has content");

        // Memory / lock profiles: clean unsupported errors (no fabricated
        // files) — see the profiler module docs.
        let err = admin
            .memory_profile(EmptyArgs {})
            .await
            .expect_err("memoryProfile unsupported");
        assert!(
            err.message.contains("memory profiling is not supported"),
            "got: {}",
            err.message
        );
        let err = admin
            .lock_profile(EmptyArgs {})
            .await
            .expect_err("lockProfile unsupported");
        assert!(
            err.message.contains("lock profiling is not supported"),
            "got: {}",
            err.message
        );
        assert!(!dir.path().join(profiler::MEM_PROFILE_FILE).exists());
        assert!(!dir.path().join(profiler::LOCK_PROFILE_FILE).exists());
    }

    /// `stacktrace` writes `stacktrace.txt` to the working directory (the Go
    /// relative-path behavior). nextest runs each test in its own process, so
    /// changing the cwd here cannot race other tests.
    #[tokio::test]
    async fn stacktrace_writes_file_in_cwd() {
        let (_dir, admin) = test_admin();
        let cwd = tempfile::tempdir().expect("tempdir");
        std::env::set_current_dir(cwd.path()).expect("chdir");

        admin.stacktrace(EmptyArgs {}).await.expect("stacktrace");
        let content =
            std::fs::read_to_string(cwd.path().join("stacktrace.txt")).expect("stacktrace.txt");
        assert!(
            content.contains("best-effort stacktrace"),
            "got: {content:?}"
        );
    }

    // ------------------------------------------------------------------
    // Wire-level dispatch through the gorilla shim (client-cased method
    // names resolve; reply JSON is Go-shaped).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn wire_dispatch_uses_go_method_names() {
        use axum::body::Body;
        use axum::http::{Method, Request, header};
        use tower::ServiceExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = MockDb::default();
        db.entries.insert(vec![0x01], vec![0xab]);
        let admin = test_admin_in(dir.path(), db, VmReload::default());
        let router = admin.into_handler();

        let post = |body: serde_json::Value| {
            let router = router.clone();
            async move {
                let request = Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).expect("serialize")))
                    .expect("request");
                let response = router.oneshot(request).await.expect("oneshot");
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body");
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
            }
        };

        // The exact client wire name `admin.dbGet` (→ `DbGet`) resolves.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "admin.dbGet",
            "params": [{ "key": "0x01" }],
        }))
        .await;
        assert_eq!(
            body["result"],
            serde_json::json!({ "value": "0xab", "errorCode": 0 })
        );

        // `admin.loadVMs` (→ override `LoadVMs`) resolves; the snake-case
        // pascalization `loadVms` does NOT.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "admin.loadVMs", "params": [],
        }))
        .await;
        assert_eq!(body["result"], serde_json::json!({ "newVMs": {} }));
        let body = post(serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "admin.loadVms", "params": [],
        }))
        .await;
        assert_eq!(
            body["error"]["code"],
            crate::error::json2_code::NO_METHOD,
            "wrong acronym casing must not resolve"
        );

        // `admin.memoryProfile` resolves and surfaces the documented
        // unsupported error as a -32000 domain error (HTTP 200 body).
        let body = post(serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "admin.memoryProfile", "params": [],
        }))
        .await;
        assert_eq!(body["error"]["code"], crate::error::json2_code::SERVER);
        assert_eq!(
            body["error"]["message"],
            "memory profiling is not supported by this node implementation"
        );

        // `admin.setLoggerLevel` end-to-end: UPPERCASE level strings on the
        // wire, both directions.
        let body = post(serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "admin.setLoggerLevel",
            "params": [{ "loggerName": "main", "logLevel": "DEBUG" }],
        }))
        .await;
        assert_eq!(body["result"]["loggerLevels"]["main"]["logLevel"], "DEBUG");
    }
}

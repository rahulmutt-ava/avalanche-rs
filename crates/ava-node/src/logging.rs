// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Bridge from the resolved node logging config to the `ava-logging` factory
//! (specs/18 §5).
//!
//! `ava_config` resolves the flag block into [`LoggingConfig`]; `ava-logging`
//! owns the subscriber factory ([`init_logging`] / [`make_chain_logger`]). This
//! module is the thin seam that maps one onto the other so `Node::new` (M8.29)
//! and the chain manager (`MakeChain`) call a single function each.

use ava_config::node::LoggingConfig;
use ava_logging::{ChainLogger, LogConfig, LogHandles, Result, Rotation, init_logging};

/// Map the resolved [`LoggingConfig`] onto the [`LogConfig`] consumed by
/// [`ava_logging::init_logging`].
///
/// `--log-display-level` inherits `--log-level` when unset; `ava_config` has
/// already resolved that, so [`LoggingConfig::display_level`] is taken verbatim.
#[must_use]
pub fn to_log_config(cfg: &LoggingConfig) -> LogConfig {
    LogConfig {
        directory: cfg.directory.clone().into(),
        file_level: cfg.log_level,
        display_level: cfg.display_level,
        format: cfg.log_format,
        rotation: Rotation {
            max_size_mib: cfg.max_size,
            max_files: cfg.max_files,
            max_age_days: cfg.max_age,
            compress: cfg.compress,
        },
    }
}

/// Install the global logging subscriber from the resolved node config.
///
/// Mirrors `app.New` building the `LogFactory` and the `main`-logger cores
/// (specs/18 §5.4). Returns the [`LogHandles`] (reload handles for the admin
/// `setLoggerLevel` endpoint + appender worker guards to keep alive until
/// shutdown).
///
/// # Errors
/// Propagates [`ava_logging::LogError`] when the log directory cannot be opened
/// or a global subscriber is already installed.
pub fn init(cfg: &LoggingConfig) -> Result<LogHandles> {
    init_logging(&to_log_config(cfg))
}

/// Build a per-chain rolling-file logger (`<log-dir>/<alias>.log`), mirroring
/// `LogFactory.MakeChain(primaryAlias)` (specs/18 §5.3).
///
/// The returned [`ChainLogger`] carries a chain-field-filtered layer (only
/// events tagged `chain = "<alias>"` reach it), the reload handle for that
/// chain's logger, and the appender worker guard (keep alive for the chain's
/// lifetime). The usual runtime path is [`LogHandles::add_chain_logger`], which
/// appends the layer to the global reloadable chain slot installed by [`init`];
/// this lower-level builder is for callers assembling their own subscriber.
///
/// [`LogHandles::add_chain_logger`]: ava_logging::LogHandles::add_chain_logger
///
/// # Errors
/// Propagates [`ava_logging::LogError`] when the log directory cannot be opened.
pub fn make_chain_logger<S>(alias: &str, cfg: &LoggingConfig) -> Result<ChainLogger<S>>
where
    S: tracing::Subscriber + for<'a> ava_logging::LookupSpan<'a>,
{
    ava_logging::make_chain_logger(alias, &to_log_config(cfg))
}

/// The node's logger factory: the [`LogHandles`] returned by [`init`] plus a
/// name → reload-handle registry for every logger created since (the Rust
/// shape of Go `logging.Factory`, consumed by `Node::new` and the admin
/// `setLoggerLevel` seam — specs/18 §5.4, 12 §3.5).
///
/// The factory tracks two well-known loggers up front — `main` (the node's
/// file core) and the display core — and grows a per-chain entry each time
/// [`LogFactory::add_chain_logger`] is called.
pub struct LogFactory {
    /// The logging block the factory was built from (per-chain layers reuse
    /// its directory/format/rotation).
    cfg: LoggingConfig,
    inner: parking_lot::Mutex<FactoryInner>,
}

/// The name of the node-wide file logger (Go's `"main"` logger).
pub const MAIN_LOGGER: &str = "main";

struct FactoryInner {
    handles: LogHandles,
    /// Per-logger file-level reload handles, keyed by logger name. `main` maps
    /// to the node-wide `main.log` core; chain aliases map to their chain
    /// layer.
    loggers: std::collections::BTreeMap<String, ava_logging::ReloadHandle>,
}

impl LogFactory {
    /// Wrap already-installed [`LogHandles`] (from [`init`]) into a factory.
    #[must_use]
    pub fn new(cfg: LoggingConfig, handles: LogHandles) -> Self {
        Self {
            cfg,
            inner: parking_lot::Mutex::new(FactoryInner {
                handles,
                loggers: std::collections::BTreeMap::new(),
            }),
        }
    }

    /// The logging block this factory was built from.
    #[must_use]
    pub fn config(&self) -> &LoggingConfig {
        &self.cfg
    }

    /// Add a per-chain logger (Go `LogFactory.MakeChain`), registering its
    /// reload handle under `alias`.
    ///
    /// # Errors
    /// Propagates [`ava_logging::LogError`] when the chain layer cannot be
    /// built or appended.
    pub fn add_chain_logger(&self, alias: &str) -> Result<ava_logging::ReloadHandle> {
        let mut inner = self.inner.lock();
        let handle = inner.handles.add_chain_logger(alias)?;
        inner.loggers.insert(alias.to_owned(), handle.clone());
        Ok(handle)
    }

    /// The names of every registered logger, `main` first (Go
    /// `GetLoggerNames`).
    #[must_use]
    pub fn logger_names(&self) -> Vec<String> {
        let inner = self.inner.lock();
        let mut names = vec![MAIN_LOGGER.to_owned()];
        names.extend(inner.loggers.keys().cloned());
        names
    }

    /// The file ("log") level reload handle for `name` (`main` or a chain
    /// alias).
    #[must_use]
    pub fn log_handle(&self, name: &str) -> Option<ava_logging::ReloadHandle> {
        let inner = self.inner.lock();
        if name == MAIN_LOGGER {
            return Some(inner.handles.main_file.clone());
        }
        inner.loggers.get(name).cloned()
    }

    /// The display-level reload handle. The Rust subscriber has a single
    /// stdout core shared by every logger, so this is name-independent
    /// (divergence noted in `tests/PORTING.md`).
    #[must_use]
    pub fn display_handle(&self) -> ava_logging::ReloadHandle {
        self.inner.lock().handles.display.clone()
    }
}

impl std::fmt::Debug for LogFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogFactory")
            .field("loggers", &self.logger_names())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use ava_logging::{AvaLevel, Format};

    use super::*;

    fn sample_config() -> LoggingConfig {
        LoggingConfig {
            directory: "logs".to_owned(),
            log_level: AvaLevel::Debug,
            display_level: AvaLevel::Info,
            log_format: Format::Json,
            disable_writer_displaying: false,
            max_size: 16,
            max_files: 5,
            max_age: 3,
            compress: true,
        }
    }

    #[test]
    fn maps_logging_config_fields() {
        let resolved = to_log_config(&sample_config());
        assert_eq!(resolved.directory.to_str(), Some("logs"));
        assert_eq!(resolved.file_level, AvaLevel::Debug);
        assert_eq!(resolved.display_level, AvaLevel::Info);
        assert_eq!(resolved.format, Format::Json);
        assert_eq!(resolved.rotation.max_size_mib, 16);
        assert_eq!(resolved.rotation.max_files, 5);
        assert_eq!(resolved.rotation.max_age_days, 3);
        assert!(resolved.rotation.compress);
    }
}

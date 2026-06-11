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

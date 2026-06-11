// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Logging level/format model + tracing wiring (specs/18 §5–§6).
//!
//! Mirrors avalanchego's `utils/logging`: the eight named levels (with Go's
//! distinctive `Trace`-above-`Debug` ordering), the plain/colors/json output
//! formats with byte-exact key order, per-chain rolling files, and reloadable
//! per-logger levels. Span/field names mirror Go log messages so operator greps
//! keep working (specs/00 §7.3).
//!
//! The [`init_logging`] factory builds the global subscriber from a
//! [`LogConfig`]: a display layer (stdout, at the display level), a `main.log`
//! rolling file layer (file, at the file level), plus a *reloadable per-chain
//! slot*. After init, [`LogHandles::add_chain_logger`] appends a per-chain
//! layer (writing `<alias>.log`, filtered to events carrying `chain =
//! "<alias>"`) into that slot at runtime — mirroring Go `LogFactory.MakeChain`,
//! which adds a chain's logger after the node is up. Every per-logger level is a
//! [`ReloadHandle`] so the admin `setLoggerLevel` endpoint (specs/12 §3.5) can
//! flip it at runtime.
//!
//! File rotation is the lumberjack-equivalent size-aware writer in the internal
//! `rolling` module: a stable `<name>.log` live file, timestamped backups on
//! size overflow, retention by count/age, and gzip when configured.

#![forbid(unsafe_code)]

mod format;
mod level;
mod rolling;

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Filter, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry, reload};

pub use format::{AvaFormat, Format};
pub use level::{AvaLevel, ParseLevelError};
/// Re-export of the `tracing-subscriber` registry trait that
/// [`make_chain_logger`] is generic over, so downstream crates can name the
/// bound without a direct `tracing-subscriber` dependency.
pub use tracing_subscriber::registry::LookupSpan;

/// The subscriber the global per-chain slot is parameterized over.
///
/// The reloadable chain-layer slot ([`init_logging`]) is the *first* layer
/// added to the registry, so each boxed chain layer is a `Layer<Registry>`.
type ChainLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// Errors raised while building or mutating the logging subscriber.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The global subscriber was already installed.
    #[error("global tracing subscriber already installed")]
    AlreadyInitialized,
    /// Failed to create or open the log directory / file.
    #[error("log directory I/O error: {0}")]
    Io(String),
    /// A reload handle could no longer reach its layer.
    #[error("failed to reload logger level: {0}")]
    Reload(String),
}

/// Convenience result alias for this crate.
pub type Result<T> = std::result::Result<T, LogError>;

/// Lumberjack-equivalent rotation policy (specs/18 §5.3).
///
/// Mirrors Go's `lumberjack.Logger` knobs and is honored by the size-aware
/// rolling writer: the live file keeps the stable name `<name>.log`; once a
/// write would exceed `max_size_mib`, it is renamed to a timestamped backup,
/// pruned to at most `max_files` (and dropping anything older than
/// `max_age_days`), and gzip-compressed when `compress` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rotation {
    /// `--log-rotater-max-size` (MiB, default 8).
    pub max_size_mib: u32,
    /// `--log-rotater-max-files` (default 7).
    pub max_files: u32,
    /// `--log-rotater-max-age` (days, default 0 = keep all).
    pub max_age_days: u32,
    /// `--log-rotater-compress-enabled` (gzip).
    pub compress: bool,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            max_size_mib: 8,
            max_files: 7,
            max_age_days: 0,
            compress: false,
        }
    }
}

/// The node-level logging configuration consumed by [`init_logging`].
///
/// This is the narrow shape `ava-node` maps its resolved `ava_config`
/// `LoggingConfig` into. It is defined here (not in `ava-config`) because
/// `ava-config` depends on `ava-logging`, not the reverse.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Expanded `--log-dir`.
    pub directory: PathBuf,
    /// `--log-level` (the file core level).
    pub file_level: AvaLevel,
    /// `--log-display-level` (the stdout core level).
    pub display_level: AvaLevel,
    /// `--log-format` (`auto` resolved against the tty at parse time).
    pub format: Format,
    /// Lumberjack-equivalent rotation knobs.
    pub rotation: Rotation,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("logs"),
            file_level: AvaLevel::Info,
            display_level: AvaLevel::Info,
            format: Format::Plain,
            rotation: Rotation::default(),
        }
    }
}

/// A type-erased setter that flips a single layer's `LevelFilter`.
///
/// Each layer's `reload::Handle` is parameterized by the subscriber it is
/// attached to, so the typed handles are heterogeneous; we erase them behind a
/// boxed closure to keep [`ReloadHandle`] uniform across the display layer, the
/// `main.log` layer, and per-chain layers.
type FilterSetter = Box<dyn Fn(LevelFilter) -> Result<()> + Send + Sync>;

/// A reloadable per-logger level, exposed to the admin `setLoggerLevel`
/// endpoint (specs/12 §3.5).
///
/// Wraps a `tracing_subscriber::reload` handle (type-erased) plus the current
/// [`AvaLevel`] so the admin API can both read and flip the level.
#[derive(Clone)]
pub struct ReloadHandle {
    setter: std::sync::Arc<FilterSetter>,
    current: std::sync::Arc<Mutex<AvaLevel>>,
}

impl ReloadHandle {
    fn new<S>(handle: reload::Handle<LevelFilter, S>, initial: AvaLevel) -> Self
    where
        S: 'static,
    {
        let setter: FilterSetter = Box::new(move |filter: LevelFilter| {
            handle
                .modify(|f| *f = filter)
                .map_err(|e| LogError::Reload(e.to_string()))
        });
        Self {
            setter: std::sync::Arc::new(setter),
            current: std::sync::Arc::new(Mutex::new(initial)),
        }
    }

    /// The level this logger is currently filtering at.
    #[must_use]
    pub fn level(&self) -> AvaLevel {
        *self.current.lock()
    }

    /// Flip the logger to a new level at runtime (admin `setLoggerLevel`).
    ///
    /// # Errors
    /// Returns [`LogError::Reload`] if the underlying layer has been dropped.
    pub fn set_level(&self, level: AvaLevel) -> Result<()> {
        (self.setter)(ava_to_level_filter(level))?;
        *self.current.lock() = level;
        Ok(())
    }
}

impl std::fmt::Debug for ReloadHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadHandle")
            .field("level", &self.level())
            .finish()
    }
}

/// A type-erased handle to the global reloadable per-chain layer slot.
///
/// `reload::Handle` is parameterized by the slot's layer type and the
/// subscriber; we erase the `modify`/append operation behind a boxed closure so
/// callers can append chain layers without naming `tracing-subscriber` types.
type ChainSlotAppender = Box<dyn Fn(ChainLayer) -> Result<()> + Send + Sync>;

/// The handles + writer guards returned by [`init_logging`].
///
/// The guards must be kept alive for the lifetime of the process — dropping a
/// [`WorkerGuard`] flushes and stops the corresponding non-blocking appender.
/// New per-chain loggers are added at runtime via [`Self::add_chain_logger`].
pub struct LogHandles {
    /// The reloadable level for the stdout/display core.
    pub display: ReloadHandle,
    /// The reloadable level for the `main.log` file core.
    pub main_file: ReloadHandle,
    /// Worker guards for every non-blocking appender; keep until shutdown.
    pub guards: Vec<WorkerGuard>,
    /// Appender into the reloadable per-chain layer slot.
    chain_slot: ChainSlotAppender,
    /// The directory/format/level context for chain loggers added later.
    cfg: LogConfig,
}

impl LogHandles {
    /// Add a per-chain logger after init (mirrors Go `LogFactory.MakeChain`).
    ///
    /// Builds a rolling-file layer writing `<log-dir>/<alias>.log`, filtered so
    /// only events carrying a `chain = "<alias>"` field reach it, and appends it
    /// to the global reloadable chain slot. The returned [`ReloadHandle`] is the
    /// per-chain logger level for the admin `setLoggerLevel` endpoint; the
    /// appender's [`WorkerGuard`] is parked on `self.guards` for the process
    /// lifetime.
    ///
    /// # Errors
    /// - [`LogError::Io`] if the log directory/file cannot be created or opened.
    /// - [`LogError::Reload`] if the chain slot is no longer reachable.
    pub fn add_chain_logger(&mut self, alias: &str) -> Result<ReloadHandle> {
        let ChainLogger {
            layer,
            handle,
            guard,
        } = make_chain_logger::<Registry>(alias, &self.cfg)?;
        (self.chain_slot)(layer)?;
        self.guards.push(guard);
        Ok(handle)
    }
}

impl std::fmt::Debug for LogHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogHandles")
            .field("display", &self.display)
            .field("main_file", &self.main_file)
            .field("guards", &self.guards.len())
            .finish()
    }
}

/// Map an [`AvaLevel`] onto a `tracing` `LevelFilter`.
///
/// `tracing` has five levels to avalanchego's eight; `Verbo`/`Trace` collapse
/// onto `TRACE`/`DEBUG` and `Fatal` onto `ERROR` (specs/18 §5.1). `Off` disables
/// the layer entirely.
#[must_use]
pub fn ava_to_level_filter(level: AvaLevel) -> LevelFilter {
    match level {
        AvaLevel::Off => LevelFilter::OFF,
        AvaLevel::Fatal | AvaLevel::Error => LevelFilter::ERROR,
        AvaLevel::Warn => LevelFilter::WARN,
        AvaLevel::Info => LevelFilter::INFO,
        // Go's Trace sits above Debug; both map below INFO. Verbo is the most
        // verbose and maps onto TRACE.
        AvaLevel::Trace => LevelFilter::DEBUG,
        AvaLevel::Debug => LevelFilter::DEBUG,
        AvaLevel::Verbo => LevelFilter::TRACE,
    }
}

fn ensure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| LogError::Io(e.to_string()))
}

/// Build a non-blocking lumberjack-equivalent file appender for `<dir>/<name>.log`.
///
/// Wraps the size-aware [`rolling::RollingWriter`] (honoring all four
/// [`Rotation`] knobs) in `tracing-appender`'s `NonBlocking` so writes — and the
/// synchronous rotate/prune/compress that happens inside them — run off the hot
/// path.
fn rolling_appender(
    dir: &Path,
    name: &str,
    rotation: Rotation,
) -> Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    ensure_dir(dir)?;
    let writer = rolling::RollingWriter::new(dir, name, rotation)
        .map_err(|e| LogError::Io(e.to_string()))?;
    Ok(tracing_appender::non_blocking(writer))
}

/// A `Filter` that admits only events carrying a `chain = "<alias>"` field.
///
/// `filter_fn` only sees `&Metadata`, which does not carry per-event field
/// *values*; so this custom filter visits the event's fields and matches the
/// `chain` value against the layer's alias. Events without a matching `chain`
/// field are routed elsewhere (the main file / display layers), never to this
/// chain's file — mirroring Go's per-chain logger.
#[derive(Clone)]
struct ChainFieldFilter {
    alias: String,
}

/// Visitor that captures the value of a `chain` field, if present.
#[derive(Default)]
struct ChainFieldVisitor {
    chain: Option<String>,
}

impl Visit for ChainFieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "chain" {
            self.chain = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "chain" && self.chain.is_none() {
            // A `chain` field recorded via Debug (e.g. `chain = %alias`) renders
            // with surrounding quotes/escapes only for non-string types; for the
            // common `&str`/`String` case `record_str` already handled it.
            self.chain = Some(format!("{value:?}"));
        }
    }
}

impl<S> Filter<S> for ChainFieldFilter {
    fn enabled(
        &self,
        _meta: &tracing::Metadata<'_>,
        _cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        // Field values are not available at `enabled` time (callsite-level
        // check); defer the real decision to `event_enabled`, which sees the
        // event. Returning `true` keeps the callsite live so `event_enabled` is
        // consulted per event.
        true
    }

    fn event_enabled(
        &self,
        event: &tracing::Event<'_>,
        _cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        let mut visitor = ChainFieldVisitor::default();
        event.record(&mut visitor);
        visitor.chain.as_deref() == Some(self.alias.as_str())
    }
}

/// Install the global logging subscriber (specs/18 §5.4).
///
/// Builds a stdout display layer at `cfg.display_level`, a `main.log` rolling
/// file layer at `cfg.file_level` (both using the configured [`Format`]), and a
/// reloadable per-chain layer slot that starts empty. Returns [`LogHandles`]
/// carrying the reload handles (admin `setLoggerLevel`), the appender worker
/// guards (keep alive until shutdown), and [`LogHandles::add_chain_logger`] for
/// adding chain loggers at runtime.
///
/// # Errors
/// - [`LogError::Io`] if the log directory cannot be created or opened.
/// - [`LogError::AlreadyInitialized`] if a global subscriber is already set.
pub fn init_logging(cfg: &LogConfig) -> Result<LogHandles> {
    let (display_filter, display_handle) =
        reload::Layer::new(ava_to_level_filter(cfg.display_level));
    let (file_filter, file_handle) = reload::Layer::new(ava_to_level_filter(cfg.file_level));

    // The reloadable per-chain slot: a `Vec` of boxed layers (empty at init).
    // It is the *first* layer on the registry so each chain layer is a
    // `Layer<Registry>`, which `add_chain_logger` can build without naming the
    // outer `Layered<...>` subscriber type.
    let (chain_slot_layer, chain_slot_handle) = reload::Layer::new(Vec::<ChainLayer>::new());

    let display_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(cfg.format.with_ansi())
        .event_format(AvaFormat::new(cfg.format))
        .with_filter(display_filter);

    let (main_writer, main_guard) = rolling_appender(&cfg.directory, "main", cfg.rotation)?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(main_writer)
        .with_ansi(false)
        .event_format(AvaFormat::new(cfg.format))
        .with_filter(file_filter);

    Registry::default()
        .with(chain_slot_layer)
        .with(display_layer)
        .with(file_layer)
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;

    let chain_slot: ChainSlotAppender = Box::new(move |layer: ChainLayer| {
        chain_slot_handle
            .modify(|layers| layers.push(layer))
            .map_err(|e| LogError::Reload(e.to_string()))
    });

    Ok(LogHandles {
        display: ReloadHandle::new(display_handle, cfg.display_level),
        main_file: ReloadHandle::new(file_handle, cfg.file_level),
        guards: vec![main_guard],
        chain_slot,
        cfg: cfg.clone(),
    })
}

/// A per-chain file logger layer plus its reload handle and worker guard.
///
/// Produced by [`make_chain_logger`]. The `layer` is filtered so only events
/// carrying a `chain = "<alias>"` field route to it; it is added to a layered
/// subscriber (the usual path is [`LogHandles::add_chain_logger`], which appends
/// it to the global reloadable chain slot). Keep `guard` alive for the chain's
/// lifetime.
pub struct ChainLogger<S> {
    /// The rolling-file layer writing `<alias>.log`, level- and chain-filtered.
    pub layer: Box<dyn Layer<S> + Send + Sync>,
    /// The reloadable level for this chain logger.
    pub handle: ReloadHandle,
    /// The appender worker guard; keep alive for the chain's lifetime.
    pub guard: WorkerGuard,
}

/// Build a per-chain rolling file logger writing `<log-dir>/<alias>.log`
/// (specs/18 §5.3).
///
/// The returned `layer` carries two stacked filters: a reloadable
/// [`LevelFilter`] (flipped by the returned [`ReloadHandle`] for the admin
/// `setLoggerLevel` endpoint) and a chain-field filter so only events tagged
/// `chain = "<alias>"` reach this chain's file. Most callers go through
/// [`LogHandles::add_chain_logger`] instead of calling this directly.
///
/// # Errors
/// [`LogError::Io`] if the log directory cannot be created or opened.
pub fn make_chain_logger<S>(alias: &str, cfg: &LogConfig) -> Result<ChainLogger<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let (level_filter, handle) = reload::Layer::new(ava_to_level_filter(cfg.file_level));
    let (writer, guard) = rolling_appender(&cfg.directory, alias, cfg.rotation)?;
    let chain_filter = ChainFieldFilter {
        alias: alias.to_owned(),
    };
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .event_format(AvaFormat::new(cfg.format))
        .with_filter(chain_filter)
        .with_filter(level_filter)
        .boxed();
    Ok(ChainLogger {
        layer,
        handle: ReloadHandle::new(handle, cfg.file_level),
        guard,
    })
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use assert_matches::assert_matches;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn ava_level_ordering_and_json_shape() {
        // 8 levels with Go's severity ordering (specs/18 §5.1):
        // Verbo < Debug < Trace < Info < Warn < Error < Fatal < Off.
        let ordered = [
            AvaLevel::Verbo,
            AvaLevel::Debug,
            AvaLevel::Trace,
            AvaLevel::Info,
            AvaLevel::Warn,
            AvaLevel::Error,
            AvaLevel::Fatal,
            AvaLevel::Off,
        ];
        for pair in ordered.windows(2) {
            if let [lo, hi] = pair {
                assert_eq!(
                    lo.cmp(hi),
                    Ordering::Less,
                    "{lo:?} should order below {hi:?}"
                );
            }
        }

        // Lowercased level strings (specs/18 §5.2).
        assert_eq!(AvaLevel::Verbo.as_str(), "verbo");
        assert_eq!(AvaLevel::Trace.as_str(), "trace");
        assert_eq!(AvaLevel::Fatal.as_str(), "fatal");
        assert_eq!(AvaLevel::Off.as_str(), "off");

        // JSON line shape: keys in exact zap order, lowercased level, and
        // integer-nanosecond durations — exercised through the real formatter
        // helper (`format::json_line`) that the JSON layer emits.
        let mut extra = vec![
            ("height".to_owned(), serde_json::Value::from(1234_u64)),
            // A duration field is rendered as integer nanoseconds.
            (
                "elapsed".to_owned(),
                serde_json::Value::from(1_500_000_000_u64),
            ),
        ];
        let line = format::json_line(
            AvaLevel::Info,
            "2026-06-04T12:00:00.000Z",
            "C",
            "chain/foo.go:42",
            "accepted block",
            &mut extra,
        )
        .expect("json_line");
        assert_eq!(
            line,
            r#"{"level":"info","timestamp":"2026-06-04T12:00:00.000Z","logger":"C","caller":"chain/foo.go:42","msg":"accepted block","height":1234,"elapsed":1500000000}"#
        );
    }

    #[test]
    fn off_disables_the_layer() {
        assert_eq!(ava_to_level_filter(AvaLevel::Off), LevelFilter::OFF);
        assert_eq!(ava_to_level_filter(AvaLevel::Verbo), LevelFilter::TRACE);
        assert_eq!(ava_to_level_filter(AvaLevel::Fatal), LevelFilter::ERROR);
    }

    #[test]
    fn rotation_defaults_match_go() {
        let r = Rotation::default();
        assert_eq!(r.max_size_mib, 8);
        assert_eq!(r.max_files, 7);
        assert_eq!(r.max_age_days, 0);
        assert!(!r.compress);
    }

    #[test]
    fn unknown_level_string_is_rejected() {
        assert_matches!("nope".parse::<AvaLevel>(), Err(_));
    }

    /// A chain logger added to a (scoped, non-global) subscriber routes only the
    /// events tagged with its own `chain` field to its `<alias>.log`, and never
    /// another chain's events. Uses `set_default` so the test does not collide
    /// with the global subscriber other tests may install.
    #[test]
    fn chain_logger_routes_only_matching_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogConfig {
            directory: dir.path().to_path_buf(),
            format: Format::Plain,
            ..LogConfig::default()
        };

        // Build two chain layers over a plain `Registry` directly (the same `S`
        // `add_chain_logger` uses) and collect them into a single `Vec<ChainLayer>`
        // layer — exactly the reloadable slot `init_logging` installs.
        let c = make_chain_logger::<Registry>("C", &cfg).expect("C logger");
        let p = make_chain_logger::<Registry>("P", &cfg).expect("P logger");

        let chain_slot: Vec<ChainLayer> = vec![c.layer, p.layer];
        let subscriber = Registry::default().with(chain_slot);
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(chain = "C", "hello from C");
            tracing::info!(chain = "P", "hello from P");
            tracing::info!("no chain field at all");
        });

        // Flush the non-blocking appenders.
        drop(c.guard);
        drop(p.guard);

        let c_log = std::fs::read_to_string(dir.path().join("C.log")).expect("C.log");
        let p_log = std::fs::read_to_string(dir.path().join("P.log")).expect("P.log");

        assert!(c_log.contains("hello from C"), "C.log: {c_log:?}");
        assert!(!c_log.contains("hello from P"), "C.log leaked P: {c_log:?}");
        assert!(
            !c_log.contains("no chain field"),
            "C.log leaked untagged: {c_log:?}"
        );

        assert!(p_log.contains("hello from P"), "P.log: {p_log:?}");
        assert!(!p_log.contains("hello from C"), "P.log leaked C: {p_log:?}");
    }
}

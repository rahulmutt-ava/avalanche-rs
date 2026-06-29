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
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry, reload};

pub use format::{AvaFormat, Format};
pub use level::{AvaLevel, ParseLevelError};
/// Re-export of the `tracing-subscriber` registry trait that
/// [`make_chain_logger`] is generic over, so downstream crates can name the
/// bound without a direct `tracing-subscriber` dependency.
pub use tracing_subscriber::registry::LookupSpan;

/// A per-chain entry stored inside [`ChainSlotVec`].
///
/// Chain layers must NOT use `with_filter` / `Filtered` wrappers because
/// `Filtered` requires `on_layer` to be called to register a `FilterId` with
/// the subscriber.  Layers added dynamically via `reload::modify` never have
/// `on_layer` called — so any `Filtered` wrapper would panic the moment an
/// event is processed.
///
/// Instead, the routing (chain-field check) and level gate are performed
/// directly by [`ChainSlotVec::on_event`], and the inner layer is a plain
/// `fmt` layer with no filter wrappers.
struct ChainEntry {
    /// The alias this entry routes, e.g. `"C"`, `"P"`.
    alias: String,
    /// The current level gate for this chain logger.  Updated atomically by
    /// the per-chain [`ReloadHandle`] returned to the admin `setLoggerLevel`
    /// endpoint.
    level: std::sync::Arc<Mutex<LevelFilter>>,
    /// The plain rolling-file layer (no `Filtered` wrapper).  Only
    /// [`ChainSlotVec::on_event`] calls into it, after the alias and level
    /// gates have been checked.
    layer: Box<dyn Layer<Registry> + Send + Sync>,
}

/// A growable collection of per-chain layers that is transparent to the
/// display and file layers.
///
/// ## Why not `Vec<L>`?
///
/// `Vec<L>` delegates `register_callsite` to its elements and returns
/// `Interest::never()` when empty, which causes `Layered::pick_interest` to
/// short-circuit and cache the callsite as permanently disabled for the
/// **whole** subscriber — silencing the display and file layers too.
///
/// Even when non-empty the problem persists: all chain layers return
/// `Interest::never()` for callsites that lack a `chain` field, so the
/// combined result is still `never()`.  Because `reload::modify` calls
/// `callsite::rebuild_interest_cache()` after every push, every untagged
/// callsite gets re-cached as disabled the moment the first chain logger is
/// added — the node goes silent ~15 ms after boot.
///
/// ## Fix
///
/// `ChainSlotVec` returns `Interest::sometimes()` and `max_level_hint = None`
/// unconditionally.  This defers interest evaluation to per-event time and
/// never lets the chain slot impose a global level ceiling on the display/file
/// layers.
///
/// Per-chain routing (`chain = "C"` events → C.log only) is done in
/// [`Layer::on_event`] by inspecting the event's fields directly, without
/// going through `Filtered` wrappers that would require `FilterId`
/// registration (which is impossible for dynamically-added layers).
struct ChainSlotVec(Vec<ChainEntry>);

impl Layer<Registry> for ChainSlotVec {
    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        // Always defer to per-event evaluation.  The chain layers route via
        // on_event (not via Filtered/callsite interest), so this slot has no
        // static opinion about any callsite.  Returning `sometimes()` ensures
        // the callsite is never cached disabled — which would also silence the
        // display and file layers stacked above this one.
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>, _ctx: Context<'_, Registry>) -> bool {
        // The chain slot has no metadata-level opinion.  Routing is done per
        // event in on_event.  Returning `true` passes the decision up to the
        // outer layers (display + file).
        true
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        // The chain layers express no global level ceiling — returning `None`
        // lets the display and file layers' own `LevelFilter`s decide the
        // effective maximum.
        None
    }

    fn event_enabled(&self, _event: &tracing::Event<'_>, _ctx: Context<'_, Registry>) -> bool {
        // Routing is handled in on_event; never veto events here so the outer
        // (display + file) layers are not suppressed.
        true
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, Registry>) {
        // Extract the `chain` field value from the event (if any).
        let mut visitor = ChainFieldVisitor::default();
        event.record(&mut visitor);
        let chain_alias = visitor.chain.as_deref();

        // Route to the matching chain entry; untagged events are intentionally
        // not forwarded to any chain layer.
        for entry in &self.0 {
            if chain_alias != Some(entry.alias.as_str()) {
                continue;
            }
            // Level gate: skip if the event's level is below the chain's filter.
            let level_filter = *entry.level.lock();
            if event.metadata().level() > &level_filter {
                break;
            }
            entry.layer.on_event(event, ctx.clone());
            break;
        }
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, Registry>,
    ) {
        for entry in &self.0 {
            entry.layer.on_new_span(attrs, id, ctx.clone());
        }
    }

    fn on_record(
        &self,
        span: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, Registry>,
    ) {
        for entry in &self.0 {
            entry.layer.on_record(span, values, ctx.clone());
        }
    }

    fn on_enter(&self, id: &tracing::span::Id, ctx: Context<'_, Registry>) {
        for entry in &self.0 {
            entry.layer.on_enter(id, ctx.clone());
        }
    }

    fn on_exit(&self, id: &tracing::span::Id, ctx: Context<'_, Registry>) {
        for entry in &self.0 {
            entry.layer.on_exit(id, ctx.clone());
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, Registry>) {
        for entry in &self.0 {
            entry.layer.on_close(id.clone(), ctx.clone());
        }
    }
}

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

    /// Build a [`ReloadHandle`] backed by a shared [`LevelFilter`] mutex.
    ///
    /// Used for per-chain loggers whose level is stored in [`ChainEntry`]
    /// rather than via a `tracing_subscriber::reload` handle (chain layers are
    /// appended dynamically and cannot go through the `reload` machinery).
    fn from_arc(level: std::sync::Arc<Mutex<LevelFilter>>, initial: AvaLevel) -> Self {
        let level_arc = level.clone();
        let setter: FilterSetter = Box::new(move |filter: LevelFilter| {
            *level_arc.lock() = filter;
            Ok(())
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
/// callers can append chain entries without naming `tracing-subscriber` types.
type ChainSlotAppender = Box<dyn Fn(ChainEntry) -> Result<()> + Send + Sync>;

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
            alias: alias_string,
            level,
            layer,
            handle,
            guard,
        } = make_chain_logger::<Registry>(alias, &self.cfg)?;
        (self.chain_slot)(ChainEntry {
            alias: alias_string,
            level,
            layer,
        })?;
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

/// Visitor that captures the value of a `chain` field, if present.
///
/// Used by [`ChainSlotVec::on_event`] to extract the chain alias and route
/// the event to the matching per-chain layer.
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

    // The reloadable per-chain slot: a `ChainSlotVec` of boxed layers (empty at
    // init).  It is the *first* layer on the registry so each chain layer is a
    // `Layer<Registry>`, which `add_chain_logger` can build without naming the
    // outer `Layered<...>` subscriber type.
    //
    // `ChainSlotVec` (not a bare `Vec`) is used here because `Vec::register_callsite`
    // returns `Interest::never()` when empty, which causes `Layered::pick_interest`
    // to short-circuit and cache every callsite as permanently disabled for the
    // *whole* subscriber stack — silencing the display and file layers.
    // `ChainSlotVec` returns `Interest::sometimes()` and `max_level_hint = None`
    // when empty so the outer layers are never suppressed.
    let (chain_slot_layer, chain_slot_handle) = reload::Layer::new(ChainSlotVec(Vec::new()));

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

    let chain_slot: ChainSlotAppender = Box::new(move |entry: ChainEntry| {
        chain_slot_handle
            .modify(|slot| slot.0.push(entry))
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
/// Produced by [`make_chain_logger`].  The plain `layer` (no `Filtered`
/// wrappers) is intended for the [`ChainSlotVec`] which handles chain-field
/// routing and level gating itself.  Most callers use
/// [`LogHandles::add_chain_logger`] instead of calling this directly.
///
/// Keep `guard` alive for the chain's lifetime.
pub struct ChainLogger<S> {
    /// The chain alias, e.g. `"C"`, `"P"`.  Consumed by [`ChainEntry`].
    pub alias: String,
    /// The shared level filter for this chain logger.  Updated by the
    /// returned [`ReloadHandle`] and read by [`ChainSlotVec::on_event`].
    pub level: std::sync::Arc<Mutex<LevelFilter>>,
    /// The plain rolling-file layer writing `<alias>.log`, with NO `Filtered`
    /// wrappers — routing and level checks are done by [`ChainSlotVec`].
    pub layer: Box<dyn Layer<S> + Send + Sync>,
    /// The reloadable level for this chain logger.
    pub handle: ReloadHandle,
    /// The appender worker guard; keep alive for the chain's lifetime.
    pub guard: WorkerGuard,
}

/// Build a per-chain rolling file logger writing `<log-dir>/<alias>.log`
/// (specs/18 §5.3).
///
/// The returned `layer` is a **plain** `fmt` layer with no `Filtered`
/// wrappers.  Chain-field routing and level gating are handled by
/// [`ChainSlotVec`] so that the layer itself needs no `FilterId`
/// registration — which is impossible for layers appended dynamically via
/// `reload::modify`.  The caller is responsible for constructing a
/// [`ChainEntry`] and passing it to the chain slot.
///
/// Most callers go through [`LogHandles::add_chain_logger`] instead of
/// calling this directly.
///
/// # Errors
/// [`LogError::Io`] if the log directory cannot be created or opened.
pub fn make_chain_logger<S>(alias: &str, cfg: &LogConfig) -> Result<ChainLogger<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let initial_level = ava_to_level_filter(cfg.file_level);
    let level = std::sync::Arc::new(Mutex::new(initial_level));
    let (writer, guard) = rolling_appender(&cfg.directory, alias, cfg.rotation)?;
    // No with_filter wrappers — ChainSlotVec performs the routing and level gate.
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .event_format(AvaFormat::new(cfg.format))
        .boxed();
    Ok(ChainLogger {
        alias: alias.to_owned(),
        level: level.clone(),
        layer,
        handle: ReloadHandle::from_arc(level, cfg.file_level),
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

        // Build two chain loggers and assemble them into a `ChainSlotVec` —
        // the same structure that `init_logging` installs as a reloadable slot.
        let c = make_chain_logger::<Registry>("C", &cfg).expect("C logger");
        let p = make_chain_logger::<Registry>("P", &cfg).expect("P logger");

        let chain_slot = ChainSlotVec(vec![
            ChainEntry {
                alias: c.alias,
                level: c.level,
                layer: c.layer,
            },
            ChainEntry {
                alias: p.alias,
                level: p.level,
                layer: p.layer,
            },
        ]);
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

    /// Regression: once at least one chain logger is added to the chain-slot
    /// (`ChainSlotVec` becomes non-empty via `reload::modify`), the chain-slot
    /// layer must NOT suppress untagged (no `chain` field) events from reaching
    /// the main display/file layers.
    ///
    /// Before this fix, `ChainSlotVec::register_callsite` in the non-empty branch
    /// seeded the combine loop with `Interest::never()`.  `reload::modify` calls
    /// `callsite::rebuild_interest_cache()` after every change, so the moment
    /// the first chain logger was pushed, every callsite lacking a `chain` field
    /// was re-registered as `never()` and cached as permanently disabled for the
    /// **whole** subscriber stack — silencing the display and file layers ~15 ms
    /// after node boot.
    ///
    /// The fix: `register_callsite` must always return `Interest::sometimes()` so
    /// callsite interest is never cached disabled by the chain slot, regardless of
    /// whether the slot is empty or non-empty.
    #[test]
    fn untagged_events_reach_main_sink_when_chain_slot_nonempty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = LogConfig {
            directory: dir.path().to_path_buf(),
            format: Format::Plain,
            ..LogConfig::default()
        };

        // Build the "main" sink — a separate plain file layer, mirroring the
        // display/file layers in init_logging.
        let main_log_path = dir.path().join("main.log");
        let main_file = std::fs::File::create(&main_log_path).expect("create main.log");
        let (file_filter, _file_handle) = reload::Layer::new(LevelFilter::INFO);
        let main_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(main_file))
            .with_ansi(false)
            .with_filter(file_filter);

        // Start with an EMPTY chain-slot wrapped in a reload handle — exactly
        // as init_logging builds it.
        let (chain_slot_layer, chain_slot_handle) = reload::Layer::new(ChainSlotVec(Vec::new()));
        let subscriber = Registry::default().with(chain_slot_layer).with(main_layer);
        let dispatch = tracing::Dispatch::new(subscriber);

        tracing::dispatcher::with_default(&dispatch, || {
            // Emit BEFORE adding any chain logger — callsite is registered here.
            tracing::info!("untagged before chains added");

            // Now add a chain logger via reload::modify.  This triggers
            // callsite::rebuild_interest_cache(), which re-invokes
            // register_callsite on the now-non-empty ChainSlotVec.  Before the
            // fix, this re-cached the callsite as Interest::never(), silencing
            // the subscriber.
            let ChainLogger {
                alias,
                level,
                layer,
                guard: c_guard,
                handle: _,
            } = make_chain_logger::<Registry>("C", &cfg).expect("C logger");
            let entry = ChainEntry {
                alias,
                level,
                layer,
            };
            chain_slot_handle
                .modify(|slot| slot.0.push(entry))
                .expect("modify chain slot");

            // Emit AFTER the chain logger was added — this is the regression
            // case.  With the bug, this and all subsequent events are silent.
            tracing::info!("untagged after chains added");
            drop(c_guard);
        });

        let main_contents = std::fs::read_to_string(&main_log_path).expect("read main.log");

        // Both untagged events must appear in the main sink.
        assert!(
            main_contents.contains("untagged before chains added"),
            "untagged event (before) was suppressed; main.log: {main_contents:?}",
        );
        assert!(
            main_contents.contains("untagged after chains added"),
            "untagged event was suppressed by non-empty chain-slot; main.log: {main_contents:?}",
        );
    }

    /// Regression: the chain-slot layer installed as the first layer on the
    /// registry must NOT suppress untagged (no `chain` field) events from
    /// reaching the main display/file layers.
    ///
    /// Before the fix, `Vec::register_callsite` returned `Interest::never()` when
    /// the vec was empty, and `Layered::pick_interest` short-circuited on
    /// `Interest::never()` from a non-filter layer, caching the callsite as
    /// permanently disabled for the whole subscriber stack.
    #[test]
    fn untagged_events_reach_the_main_sink() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Build the SAME layer stack that init_logging builds:
        //   chain_slot_layer (empty ChainSlotVec, reloadable) → file_layer (plain fmt).
        let (chain_slot_layer, _chain_slot_handle) = reload::Layer::new(ChainSlotVec(Vec::new()));

        let log_path = dir.path().join("main.log");
        let file = std::fs::File::create(&log_path).expect("create main.log");
        let (file_filter, _file_handle) = reload::Layer::new(LevelFilter::INFO);
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_filter(file_filter);

        let subscriber = Registry::default().with(chain_slot_layer).with(file_layer);
        let dispatch = tracing::Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!("untagged node event");
        });

        let contents = std::fs::read_to_string(&log_path).expect("read main.log");
        assert!(
            contents.contains("untagged node event"),
            "untagged event was suppressed by the empty chain-slot layer; main.log: {contents:?}",
        );
    }
}

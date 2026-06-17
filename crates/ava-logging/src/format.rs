// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Output formats for a logging sink (specs/18 §5.2).
//!
//! Mirrors avalanchego `utils/logging/format.go`. Three concrete encoders are
//! supported: `plain` and `colors` (console, time layout `[01-02|15:04:05.000]`,
//! level rendered via the uppercase level string, colorized only for `colors`)
//! and `json` (one object per line with zap's exact key order — `level`,
//! `timestamp`, `logger`, `caller`, `msg`, then structured fields — a lowercased
//! level string, an ISO8601 timestamp and integer-nanosecond durations).
//!
//! The encoders are implemented as [`tracing_subscriber::fmt::FormatEvent`]
//! hooks so the byte shape is frozen independently of `tracing`'s default
//! formatter.

use std::collections::BTreeMap;
use std::fmt;

use chrono::Utc;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use crate::level::AvaLevel;

/// Output format for a logging sink (specs/18 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Human-readable, no ANSI colors.
    Plain,
    /// Human-readable with ANSI colors (TTY console).
    Colors,
    /// One JSON object per line (machine-ingestible; exact Go key order).
    Json,
}

impl Format {
    /// Whether this format colorizes the level token.
    #[must_use]
    pub fn with_ansi(self) -> bool {
        matches!(self, Format::Colors)
    }
}

/// Maps a `tracing::Level` onto the avalanchego level taxonomy.
///
/// `tracing` has only five levels; the avalanchego `Verbo`/`Trace` distinction
/// is carried by an explicit `verbo`/`trace` marker field on the event (set by
/// the logging macros), falling back to the nearest `tracing` level otherwise.
fn event_level(meta_level: &tracing::Level, visited: &VisitedFields) -> AvaLevel {
    if let Some(explicit) = visited.ava_level {
        return explicit;
    }
    match *meta_level {
        tracing::Level::ERROR => AvaLevel::Error,
        tracing::Level::WARN => AvaLevel::Warn,
        tracing::Level::INFO => AvaLevel::Info,
        tracing::Level::DEBUG => AvaLevel::Debug,
        tracing::Level::TRACE => AvaLevel::Verbo,
    }
}

/// ANSI color code for a level token (mirrors `utils/logging/color.go`).
fn level_color(level: AvaLevel) -> &'static str {
    match level {
        AvaLevel::Fatal | AvaLevel::Error => "\u{1b}[31m", // red
        AvaLevel::Warn => "\u{1b}[33m",                    // yellow
        AvaLevel::Info => "\u{1b}[32m",                    // green
        AvaLevel::Trace | AvaLevel::Debug => "\u{1b}[36m", // cyan
        AvaLevel::Verbo => "\u{1b}[37m",                   // light gray
        AvaLevel::Off => "",
    }
}

const ANSI_RESET: &str = "\u{1b}[0m";

/// Render one JSON log line with zap's exact key order (specs/18 §5.2).
///
/// Keys are emitted in the frozen order `level`, `timestamp`, `logger`,
/// `caller`, `msg`, followed by the structured fields in their (already sorted)
/// order. The line is built by hand rather than via a `serde_json::Map` because
/// the default `serde_json` object preserves *no* insertion order, and zap's
/// byte shape (00 §7.3) must be reproduced exactly.
pub(crate) fn json_line(
    level: AvaLevel,
    timestamp: &str,
    logger: &str,
    caller: &str,
    message: &str,
    extra: &mut [(String, serde_json::Value)],
) -> Result<String, fmt::Error> {
    let mut out = String::from("{");
    push_kv_str(&mut out, "level", level.as_str());
    out.push(',');
    push_kv_str(&mut out, "timestamp", timestamp);
    out.push(',');
    push_kv_str(&mut out, "logger", logger);
    out.push(',');
    push_kv_str(&mut out, "caller", caller);
    out.push(',');
    push_kv_str(&mut out, "msg", message);
    for (key, value) in extra.iter() {
        out.push(',');
        let encoded_key = serde_json::to_string(key).map_err(|_| fmt::Error)?;
        let encoded_value = serde_json::to_string(value).map_err(|_| fmt::Error)?;
        out.push_str(&encoded_key);
        out.push(':');
        out.push_str(&encoded_value);
    }
    out.push('}');
    Ok(out)
}

/// Append `"key":"value"` with JSON-escaped key and value to `out`.
fn push_kv_str(out: &mut String, key: &str, value: &str) {
    // `serde_json::to_string` on a `&str` cannot fail; fall back to an empty
    // quoted string rather than panicking if it ever did.
    let encoded_key = serde_json::to_string(key).unwrap_or_else(|_| String::from("\"\""));
    let encoded_value = serde_json::to_string(value).unwrap_or_else(|_| String::from("\"\""));
    out.push_str(&encoded_key);
    out.push(':');
    out.push_str(&encoded_value);
}

/// A captured set of structured event fields.
///
/// `message` is the event's `msg`; `ava_level`/`caller`/`logger` shadow the
/// reserved keys; everything else lands in `fields` in stable (sorted) order so
/// JSON output is byte-deterministic (specs/00 forbids `HashMap` ordering on
/// serialized paths).
#[derive(Default)]
struct VisitedFields {
    message: Option<String>,
    ava_level: Option<AvaLevel>,
    caller: Option<String>,
    logger: Option<String>,
    fields: BTreeMap<String, serde_json::Value>,
}

impl Visit for VisitedFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(value.to_owned()),
            "caller" => self.caller = Some(value.to_owned()),
            "logger" | "chain" => self.logger = Some(value.to_owned()),
            "ava_level" => {
                if let Ok(level) = value.parse() {
                    self.ava_level = Some(level);
                }
            }
            other => {
                self.fields.insert(
                    other.to_owned(),
                    serde_json::Value::String(value.to_owned()),
                );
            }
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), serde_json::Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), serde_json::Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), serde_json::Value::Bool(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        match field.name() {
            "message" => self.message = Some(rendered),
            other => {
                self.fields
                    .insert(other.to_owned(), serde_json::Value::String(rendered));
            }
        }
    }
}

/// The avalanchego console/JSON event formatter (specs/18 §5.2).
#[derive(Debug, Clone, Copy)]
pub struct AvaFormat {
    format: Format,
}

impl AvaFormat {
    /// Build a formatter for the given output [`Format`].
    #[must_use]
    pub fn new(format: Format) -> Self {
        Self { format }
    }
}

impl<S, N> FormatEvent<S, N> for AvaFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut visited = VisitedFields::default();
        event.record(&mut visited);

        let meta = event.metadata();
        let level = event_level(meta.level(), &visited);
        let caller = visited
            .caller
            .clone()
            .or_else(|| match (meta.file(), meta.line()) {
                (Some(file), Some(line)) => Some(format!("{file}:{line}")),
                _ => None,
            })
            .unwrap_or_default();
        let logger = visited.logger.clone().unwrap_or_default();
        let message = visited.message.clone().unwrap_or_default();

        match self.format {
            Format::Json => {
                let ts = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
                let mut extra: Vec<(String, serde_json::Value)> =
                    visited.fields.into_iter().collect();
                let line = json_line(level, &ts, &logger, &caller, &message, &mut extra)?;
                writeln!(writer, "{line}")
            }
            Format::Plain | Format::Colors => {
                let ts = Utc::now().format("[%m-%d|%H:%M:%S%.3f]");
                if self.format == Format::Colors {
                    write!(
                        writer,
                        "{ts} {color}{level}{reset} ",
                        level = level.as_upper_str(),
                        color = level_color(level),
                        reset = ANSI_RESET,
                    )?;
                } else {
                    write!(writer, "{ts} {level} ", level = level.as_upper_str())?;
                }
                if !logger.is_empty() {
                    write!(writer, "<{logger}> ")?;
                }
                write!(writer, "{message}")?;
                for (k, v) in &visited.fields {
                    write!(writer, " {k}={v}")?;
                }
                writeln!(writer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_for_level_is_stable() {
        assert_eq!(level_color(AvaLevel::Error), "\u{1b}[31m");
        assert_eq!(level_color(AvaLevel::Info), "\u{1b}[32m");
        assert_eq!(level_color(AvaLevel::Off), "");
    }

    /// The JSON line must escape control/quote/non-ASCII content in both the
    /// message and field values so the result is valid JSON that round-trips
    /// through `serde_json` with the values preserved exactly.
    #[test]
    fn json_line_escapes_quotes_newlines_and_non_ascii() {
        let message = "say \"hi\"\nover µ lines";
        let field_value = "value \"q\"\nµ";
        let mut extra = vec![(
            "note".to_owned(),
            serde_json::Value::String(field_value.to_owned()),
        )];

        let line = json_line(
            AvaLevel::Info,
            "2026-06-04T12:00:00.000Z",
            "C",
            "chain/foo.go:42",
            message,
            &mut extra,
        )
        .expect("json_line");

        // The emitted line contains no raw newline (it was escaped).
        assert!(
            !line.contains('\n'),
            "newline must be escaped, got {line:?}"
        );

        // It parses as valid JSON and preserves every value verbatim.
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        let get = |k: &str| parsed.get(k).cloned().expect("key present");
        assert_eq!(get("level"), serde_json::json!("info"));
        assert_eq!(get("logger"), serde_json::json!("C"));
        assert_eq!(get("caller"), serde_json::json!("chain/foo.go:42"));
        assert_eq!(get("msg"), serde_json::Value::String(message.to_owned()));
        assert_eq!(
            get("note"),
            serde_json::Value::String(field_value.to_owned())
        );
    }
}

/// Golden byte-shape test for the JSON log line (specs/18 §5.2).
///
/// Drives the *real* [`AvaFormat`] (`Format::Json`) formatter through a
/// `tracing_subscriber` registry whose writer is an in-memory buffer, emits one
/// event with a fixed logger/caller/message and three structured fields chosen
/// so the sorted (BTreeMap) field order is observable, then freezes the exact
/// emitted byte shape against a committed golden. This is the byte-shape
/// guarantee the unit-level `json_line` test cannot give: it exercises the full
/// `FormatEvent` path (field capture → sort → encode → `writeln!`).
#[cfg(test)]
mod golden {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;

    use super::{AvaFormat, Format};

    /// The committed golden: the normalized JSON line (timestamp replaced by the
    /// `<TS>` placeholder). Loaded via `include_str!` so the on-disk golden file
    /// is the single source of truth.
    const GOLDEN: &str = include_str!("../tests/vectors/log_json_shape.golden");

    /// A `MakeWriter` backed by a shared in-memory buffer so the test can read
    /// back exactly the bytes the formatter produced.
    #[derive(Clone, Default)]
    struct BufWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl io::Write for BufWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let mut guard = self.buf.lock().expect("buffer lock");
            guard.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufWriter {
        type Writer = BufWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Capture the byte index of `needle` in `haystack`, failing the test if it
    /// is absent.
    fn index_of(haystack: &str, needle: &str) -> usize {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("expected {needle:?} in {haystack:?}"))
    }

    /// True iff `s` matches the ISO8601 shape
    /// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$` (the `Utc::now()` layout
    /// `%Y-%m-%dT%H:%M:%S%.3fZ`), checked without a regex dependency.
    ///
    /// The pattern string uses `d` for an ASCII digit and any other byte as a
    /// literal separator; matching iterates in lockstep so no slice indexing or
    /// fallible arithmetic is needed.
    fn is_iso8601_millis_z(s: &str) -> bool {
        // YYYY-MM-DDTHH:MM:SS.mmmZ
        const PATTERN: &[u8] = b"dddd-dd-ddTdd:dd:dd.dddZ";
        let bytes = s.as_bytes();
        if bytes.len() != PATTERN.len() {
            return false;
        }
        bytes.iter().zip(PATTERN.iter()).all(|(&got, &pat)| {
            if pat == b'd' {
                got.is_ascii_digit()
            } else {
                got == pat
            }
        })
    }

    /// Replace the JSON `"timestamp":"<value>"` value with the `<TS>` placeholder
    /// and return both the placeholder-normalized line and the raw timestamp
    /// value, so the test can compare the normalized shape against the golden and
    /// separately assert the raw timestamp's ISO8601 shape.
    fn normalize_timestamp(line: &str) -> (String, String) {
        const KEY: &str = "\"timestamp\":\"";
        let key_at = index_of(line, KEY);
        let value_start = key_at
            .checked_add(KEY.len())
            .expect("timestamp key offset overflow");
        let rest = line
            .get(value_start..)
            .unwrap_or_else(|| panic!("timestamp value start out of bounds in {line:?}"));
        let value_len = rest
            .find('"')
            .unwrap_or_else(|| panic!("unterminated timestamp value in {line:?}"));
        let raw_ts = rest
            .get(..value_len)
            .unwrap_or_else(|| panic!("timestamp value out of bounds in {line:?}"))
            .to_owned();
        let prefix = line
            .get(..value_start)
            .unwrap_or_else(|| panic!("timestamp prefix out of bounds in {line:?}"));
        let suffix = rest
            .get(value_len..)
            .unwrap_or_else(|| panic!("timestamp suffix out of bounds in {line:?}"));
        let mut normalized = String::with_capacity(line.len());
        normalized.push_str(prefix);
        normalized.push_str("<TS>");
        normalized.push_str(suffix);
        (normalized, raw_ts)
    }

    #[test]
    fn json_line_shape_is_frozen() {
        let writer = BufWriter::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer.clone())
                .with_ansi(false)
                .event_format(AvaFormat::new(Format::Json)),
        );

        // Emit one event through the real formatter. Fields are listed in a
        // deliberately *unsorted* insertion order (height, flavor, awake) so the
        // BTreeMap sort in the formatter is observable: the output must reorder
        // them to awake < flavor < height.
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                logger = "C",
                caller = "chain/foo.go:42",
                height = 1234_i64,
                flavor = "vanilla",
                awake = true,
                "accepted block"
            );
        });

        let bytes = writer.buf.lock().expect("buffer lock").clone();
        let raw = String::from_utf8(bytes).expect("utf8 log output");
        // Exactly one line, newline-terminated.
        assert!(
            raw.ends_with('\n'),
            "line must be newline-terminated: {raw:?}"
        );
        let line = raw.trim_end_matches('\n');
        assert!(
            !line.contains('\n'),
            "exactly one event line expected, got {raw:?}"
        );

        // (1) Frozen reserved-key ORDER on the raw bytes: level < timestamp <
        // logger < caller < msg.
        let i_level = index_of(line, "\"level\":");
        let i_ts = index_of(line, "\"timestamp\":");
        let i_logger = index_of(line, "\"logger\":");
        let i_caller = index_of(line, "\"caller\":");
        let i_msg = index_of(line, "\"msg\":");
        assert!(
            i_level < i_ts && i_ts < i_logger && i_logger < i_caller && i_caller < i_msg,
            "reserved keys out of frozen zap order in {line:?}"
        );

        // Structured fields follow `msg` in sorted (BTreeMap) order:
        // awake < flavor < height — all after `msg`.
        let i_awake = index_of(line, "\"awake\":");
        let i_flavor = index_of(line, "\"flavor\":");
        let i_height = index_of(line, "\"height\":");
        assert!(
            i_msg < i_awake && i_awake < i_flavor && i_flavor < i_height,
            "structured fields not in sorted order after msg in {line:?}"
        );

        // (2) Normalize the non-deterministic timestamp, compare against golden.
        let (normalized, raw_ts) = normalize_timestamp(line);
        let golden = GOLDEN.trim_end_matches('\n');
        assert_eq!(normalized, golden, "normalized JSON line vs golden");

        // The raw (un-normalized) timestamp matches the ISO8601 millis-Z shape.
        assert!(
            is_iso8601_millis_z(&raw_ts),
            "timestamp {raw_ts:?} is not ISO8601 %Y-%m-%dT%H:%M:%S%.3fZ"
        );
    }
}

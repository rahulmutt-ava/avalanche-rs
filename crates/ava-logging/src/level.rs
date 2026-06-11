// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Avalanche log levels (specs/18 §5.1).
//!
//! Ported from avalanchego `utils/logging/level.go`. Go assigns the levels
//! ascending numeric values via `iota - 9`, giving the severity-ordered
//! sequence
//!
//! ```text
//! Verbo < Debug < Trace < Info < Warn < Error < Fatal < Off
//! ```
//!
//! Note Go's two deliberate quirks we preserve for parity: `Trace` sits
//! **above** `Debug` (i.e. is *less* verbose), and `Verbo` is the most verbose
//! (lowest numeric value). A sink configured at level `L` emits every record
//! whose level is `>= L` numerically — so `--log-level=verbo` shows everything
//! and `--log-level=trace` hides `Debug`/`Verbo`.

use std::fmt;
use std::str::FromStr;

/// The eight named log levels, ordered by Go's severity ordering.
///
/// The numeric discriminants reproduce Go's ascending `iota` ordering so
/// `PartialOrd`/`Ord` yield `Verbo < Debug < Trace < Info < Warn < Error <
/// Fatal < Off` (specs/18 §5.1). A more-verbose level has a *lower* value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum AvaLevel {
    /// Verbose (most verbose; below `Debug`).
    Verbo = 0,
    /// Debug.
    Debug = 1,
    /// Trace (Go orders this *above* `Debug`).
    Trace = 2,
    /// Informational.
    Info = 3,
    /// A warning.
    Warn = 4,
    /// A recoverable error.
    Error = 5,
    /// Unrecoverable error; the process is going down.
    Fatal = 6,
    /// Logging disabled.
    Off = 7,
}

impl AvaLevel {
    /// The lowercase wire string for this level (used by the JSON format).
    ///
    /// Mirrors Go `Level.LowerString()`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AvaLevel::Off => "off",
            AvaLevel::Fatal => "fatal",
            AvaLevel::Error => "error",
            AvaLevel::Warn => "warn",
            AvaLevel::Info => "info",
            AvaLevel::Trace => "trace",
            AvaLevel::Debug => "debug",
            AvaLevel::Verbo => "verbo",
        }
    }

    /// The uppercase string for this level (used by the plain/colors formats).
    ///
    /// Mirrors Go `Level.String()`.
    #[must_use]
    pub fn as_upper_str(self) -> &'static str {
        match self {
            AvaLevel::Off => "OFF",
            AvaLevel::Fatal => "FATAL",
            AvaLevel::Error => "ERROR",
            AvaLevel::Warn => "WARN",
            AvaLevel::Info => "INFO",
            AvaLevel::Trace => "TRACE",
            AvaLevel::Debug => "DEBUG",
            AvaLevel::Verbo => "VERBO",
        }
    }
}

impl fmt::Display for AvaLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string does not name a known [`AvaLevel`].
#[derive(Debug, thiserror::Error)]
#[error("unknown log level: {0}")]
pub struct ParseLevelError(String);

impl FromStr for AvaLevel {
    type Err = ParseLevelError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(AvaLevel::Off),
            "fatal" => Ok(AvaLevel::Fatal),
            "error" => Ok(AvaLevel::Error),
            "warn" => Ok(AvaLevel::Warn),
            "info" => Ok(AvaLevel::Info),
            "trace" => Ok(AvaLevel::Trace),
            "debug" => Ok(AvaLevel::Debug),
            "verbo" => Ok(AvaLevel::Verbo),
            other => Err(ParseLevelError(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    #[test]
    fn trace_is_above_debug_in_go_ordering() {
        // The deliberate Go quirk: Trace is *less* verbose than Debug, so under
        // the severity ordering Debug < Trace.
        assert_eq!(AvaLevel::Debug.cmp(&AvaLevel::Trace), Ordering::Less);
        assert_eq!(AvaLevel::Verbo.cmp(&AvaLevel::Debug), Ordering::Less);
    }

    #[test]
    fn round_trips_through_string() {
        for level in [
            AvaLevel::Off,
            AvaLevel::Fatal,
            AvaLevel::Error,
            AvaLevel::Warn,
            AvaLevel::Info,
            AvaLevel::Trace,
            AvaLevel::Debug,
            AvaLevel::Verbo,
        ] {
            assert_eq!(level.as_str().parse::<AvaLevel>().ok(), Some(level));
        }
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Avalanche log levels (specs/18 §5.1).
//!
//! Ported from avalanchego `utils/logging/level.go`. Note the Go-specific
//! ordering: `Trace` sits **above** `Debug` in verbosity (a deliberate quirk we
//! preserve for parity), and `Verbo` is the most verbose. Higher numeric value
//! = more verbose; a sink at level L emits records whose level is `<= L`.

use std::fmt;
use std::str::FromStr;

/// The eight named log levels, in increasing verbosity.
///
/// SCAFFOLD: the numeric values + ordering mirror Go; the JSON/console string
/// rendering and the reloadable per-logger wiring land in tier-X task X.18.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum AvaLevel {
    /// Logging disabled.
    Off = 0,
    /// Unrecoverable error; the process is going down.
    Fatal = 1,
    /// A recoverable error.
    Error = 2,
    /// A warning.
    Warn = 3,
    /// Informational.
    Info = 4,
    /// Trace (Go orders this *above* Debug).
    Trace = 5,
    /// Debug.
    Debug = 6,
    /// Verbose (most verbose).
    Verbo = 7,
}

impl AvaLevel {
    /// The lowercase wire string for this level (used by the JSON format).
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
    use super::*;

    #[test]
    fn trace_is_above_debug_in_go_ordering() {
        // The deliberate Go quirk: Trace is less verbose than Debug.
        assert!(AvaLevel::Trace < AvaLevel::Debug);
        assert!(AvaLevel::Debug < AvaLevel::Verbo);
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
            assert_eq!(level.as_str().parse::<AvaLevel>().unwrap(), level);
        }
    }
}

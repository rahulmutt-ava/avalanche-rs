// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Logging level/format model + tracing wiring (specs/18 §5–§6).
//!
//! Mirrors avalanchego's `utils/logging`: the eight named levels (with Go's
//! distinctive `Trace`-above-`Debug` ordering), the plain/colors/json output
//! formats with byte-exact key order, per-chain rolling files, and reloadable
//! per-logger levels. Span names mirror Go log messages so operator greps keep
//! working (specs/00 §7.3).
//!
//! SCAFFOLD (tier-X task X.18): the level taxonomy + format enum are defined
//! here so the model is pinned from M0. The custom `tracing` JSON format layer
//! (exact key order, lowercased level, integer-nanosecond durations), the
//! per-chain rolling file layer, the reload handles, and the OTLP exporter
//! (deferred to M8) are filled in by X.18.

#![forbid(unsafe_code)]

mod level;

pub use level::{AvaLevel, ParseLevelError};

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

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Health API wire types (mirror Go `api/health/{result,service}.go`; specs 12
//! §3.4, 14 §5).
//!
//! The JSON field set and omission rules are byte-faithful to the Go structs'
//! `json` tags:
//!
//! - [`Result`] mirrors `health.Result`: `message` / `error` /
//!   `contiguousFailures` / `timeOfFirstFailure` are omitted when empty,
//!   `timestamp` is **always** present (Go's `omitempty` is a no-op on a
//!   `time.Time` struct — the zero value renders as `0001-01-01T00:00:00Z`),
//!   and `duration` is always present as integer nanoseconds.
//! - [`APIReply`] mirrors `health.APIReply` (`{checks, healthy}`); the checks
//!   map is a `BTreeMap` so keys serialize sorted, matching `encoding/json`'s
//!   sorted map-key output.
//! - [`APIArgs`] mirrors `health.APIArgs` (`{tags}`).

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize, Serializer};

/// The result of a single health check (mirror Go `health.Result`,
/// `api/health/result.go`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Result {
    /// Details reported by the check (Go `Details interface{}
    /// json:"message,omitempty"`). `None` (Go `nil`) is omitted.
    #[serde(rename = "message", skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,

    /// The string form of the error returned by a failing check; `None` if the
    /// check passed (Go `Error *string json:"error,omitempty"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Timestamp of the last run of this check. Always serialized (see module
    /// docs); a never-run check carries [`go_zero_time`].
    #[serde(serialize_with = "serialize_go_time")]
    pub timestamp: DateTime<Utc>,

    /// How long the last run took. Serialized as integer nanoseconds (Go
    /// `time.Duration`).
    #[serde(serialize_with = "serialize_go_duration")]
    pub duration: Duration,

    /// The number of contiguous failures (Go `int64
    /// json:"contiguousFailures,omitempty"`); omitted when zero.
    #[serde(rename = "contiguousFailures", skip_serializing_if = "is_zero")]
    pub contiguous_failures: i64,

    /// When the current failure streak began (Go `*time.Time
    /// json:"timeOfFirstFailure,omitempty"`); omitted when `None`.
    #[serde(
        rename = "timeOfFirstFailure",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_opt_go_time"
    )]
    pub time_of_first_failure: Option<DateTime<Utc>>,
}

impl Result {
    /// The placeholder stored for a check that has not run yet (Go
    /// `notYetRunResult`): failing with error `"not yet run"`.
    #[must_use]
    pub fn not_yet_run() -> Self {
        Self {
            error: Some("not yet run".to_string()),
            ..Self::default()
        }
    }
}

impl Default for Result {
    /// The Go zero-value `Result` (all fields empty, timestamp = the Go zero
    /// time).
    fn default() -> Self {
        Self {
            details: None,
            error: None,
            timestamp: go_zero_time(),
            duration: Duration::ZERO,
            contiguous_failures: 0,
            time_of_first_failure: None,
        }
    }
}

/// The reply for `health.health` / `health.readiness` / `health.liveness` and
/// the GET endpoints (mirror Go `health.APIReply`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct APIReply {
    /// Per-check results, keyed by check name (sorted; see module docs).
    pub checks: BTreeMap<String, Result>,
    /// Whether every reported check is passing.
    pub healthy: bool,
}

/// The arguments for `health.health` / `health.readiness` / `health.liveness`
/// (mirror Go `health.APIArgs`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct APIArgs {
    /// The tags to filter checks by (empty = all checks).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Go's zero `time.Time` (`0001-01-01T00:00:00Z`), the timestamp a never-run
/// check serializes with.
#[must_use]
pub fn go_zero_time() -> DateTime<Utc> {
    // Year 1 is well inside chrono's range; the fallback is unreachable defence
    // (`unwrap` is denied in lib code).
    Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0)
        .single()
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Whether an `i64` is zero (`omitempty` helper for serde).
#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Formats a UTC time the way Go's `time.Time.MarshalJSON` does
/// (`time.RFC3339Nano`): seconds, then the fractional part with trailing zeros
/// trimmed (absent entirely when zero), then `Z`.
fn format_go_rfc3339_nano(t: &DateTime<Utc>) -> String {
    let mut s = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    // `timestamp_subsec_nanos` can exceed 999 999 999 only on a leap second,
    // which `Utc::now` never produces; clamp for safety.
    let nanos = t.timestamp_subsec_nanos().min(999_999_999);
    if nanos > 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s.push('Z');
    s
}

/// Serde adapter for [`format_go_rfc3339_nano`].
fn serialize_go_time<S: Serializer>(
    t: &DateTime<Utc>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&format_go_rfc3339_nano(t))
}

/// Serde adapter for an optional Go time; only invoked when `Some` (the `None`
/// case is skipped via `skip_serializing_if`).
fn serialize_opt_go_time<S: Serializer>(
    t: &Option<DateTime<Utc>>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match t {
        Some(t) => serialize_go_time(t, s),
        // Unreachable behind skip_serializing_if; serialize an explicit null to
        // stay total.
        None => s.serialize_none(),
    }
}

/// Serializes a [`Duration`] as Go `time.Duration` JSON: integer nanoseconds
/// (saturating at `i64::MAX`, mirroring Go's int64 domain).
fn serialize_go_duration<S: Serializer>(
    d: &Duration,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_i64(i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    // The not-yet-run placeholder serializes exactly like Go's
    // `notYetRunResult`: error + always-present zero timestamp + duration 0;
    // message / contiguousFailures / timeOfFirstFailure omitted.
    #[test]
    fn not_yet_run_result_json_shape() {
        let got = serde_json::to_value(Result::not_yet_run()).expect("serialize Result");
        assert_eq!(
            got,
            json!({
                "error": "not yet run",
                "timestamp": "0001-01-01T00:00:00Z",
                "duration": 0,
            }),
            "not_yet_run() JSON shape"
        );
    }

    // A populated failing result carries every field with Go's names; the
    // fractional timestamp is RFC3339Nano-trimmed (".5", not ".500").
    #[test]
    fn full_result_json_shape() {
        let ts = Utc
            .with_ymd_and_hms(2026, 6, 11, 12, 0, 0)
            .single()
            .expect("valid time")
            + chrono::Duration::milliseconds(500);
        let first = Utc
            .with_ymd_and_hms(2026, 6, 11, 11, 59, 0)
            .single()
            .expect("valid time")
            + chrono::Duration::nanoseconds(123_456_789);
        let result = Result {
            details: Some(json!({"consecutive": 3})),
            error: Some("boom".to_string()),
            timestamp: ts,
            duration: Duration::from_millis(2),
            contiguous_failures: 4,
            time_of_first_failure: Some(first),
        };
        let got = serde_json::to_value(&result).expect("serialize Result");
        assert_eq!(
            got,
            json!({
                "message": {"consecutive": 3},
                "error": "boom",
                "timestamp": "2026-06-11T12:00:00.5Z",
                "duration": 2_000_000,
                "contiguousFailures": 4,
                "timeOfFirstFailure": "2026-06-11T11:59:00.123456789Z",
            }),
            "full Result JSON shape"
        );
    }

    // A passing result omits error / contiguousFailures / timeOfFirstFailure.
    #[test]
    fn passing_result_omits_failure_fields() {
        let result = Result {
            details: None,
            error: None,
            timestamp: go_zero_time(),
            duration: Duration::from_nanos(7),
            contiguous_failures: 0,
            time_of_first_failure: None,
        };
        let got = serde_json::to_value(&result).expect("serialize Result");
        assert_eq!(
            got,
            json!({
                "timestamp": "0001-01-01T00:00:00Z",
                "duration": 7,
            }),
            "passing Result omits failure fields"
        );
    }

    // APIArgs deserializes Go-style ({"tags": [...]}) and tolerates {} (the
    // gorilla `*struct{}`-style empty params).
    #[test]
    fn api_args_deserializes() {
        let args: APIArgs =
            serde_json::from_value(json!({"tags": ["s1", "s2"]})).expect("APIArgs with tags");
        assert_eq!(args.tags, vec!["s1".to_string(), "s2".to_string()]);

        let args: APIArgs = serde_json::from_value(json!({})).expect("APIArgs empty object");
        assert!(args.tags.is_empty(), "default tags must be empty");
    }
}

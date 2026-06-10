// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Go `time.Duration` grammar: formatting (and, from M8.3, parsing) that is
//! byte-identical to Go's `Duration.String()` / `time.ParseDuration`
//! (specs 12 §1.4 — pflag duration defaults are `DefValue == d.String()`).

use std::time::Duration;

use crate::ConfigError;

/// Go's duration-overflow boundary (`1 << 63`, i.e. `i64::MIN` magnitude).
const GO_OVERFLOW: u64 = 1 << 63;
/// `leadingInt` overflow guard: `( 1<<63 - 1 ) / 10`.
const LEADING_INT_MAX: u64 = (GO_OVERFLOW - 1) / 10;

/// Splits `v` into the integer part above `10^prec` and the Go-style decimal
/// fraction string below it (trailing zeros — and a fully-zero fraction's
/// period — omitted; Go `time.fmtFrac`).
// `pow = 10^prec` with `prec <= 9`, so it is never zero and never overflows.
#[allow(clippy::arithmetic_side_effects)]
fn frac(v: u128, prec: u32) -> (u128, String) {
    let pow = 10u128.pow(prec);
    let int = v / pow;
    let rem = v % pow;
    if rem == 0 {
        return (int, String::new());
    }
    let mut digits = format!("{rem:0width$}", width = prec as usize);
    while digits.ends_with('0') {
        digits.pop();
    }
    (int, format!(".{digits}"))
}

/// Formats a duration exactly as Go's `time.Duration.String()` does:
/// `"0s"`, `"100ms"`, `"22.5s"`, `"5m0s"`, `"8760h0m0s"`, `"1.5µs"`, ….
///
/// Sub-second durations use the largest fitting unit of `ns`/`µs`/`ms`;
/// otherwise the form is `[Nh][Nm]N[.frac]s`.
#[must_use]
pub fn format_go_duration(d: Duration) -> String {
    let u = d.as_nanos();
    if u == 0 {
        return "0s".to_string();
    }
    if u < 1_000_000_000 {
        // Sub-second: pick ns / µs / ms (Go uses U+00B5 MICRO SIGN).
        let (prec, unit) = if u < 1_000 {
            (0, "ns")
        } else if u < 1_000_000 {
            (3, "\u{b5}s")
        } else {
            (6, "ms")
        };
        let (int, f) = frac(u, prec);
        return format!("{int}{f}{unit}");
    }
    let (secs, f) = frac(u, 9);
    let mut out = format!("{}{f}s", secs % 60);
    let mins = secs / 60;
    if mins > 0 {
        out = format!("{}m{out}", mins % 60);
        let hours = mins / 60;
        if hours > 0 {
            out = format!("{hours}h{out}");
        }
    }
    out
}

/// Nanoseconds per unit token (Go `time/format.go::unitMap`). Both the micro
/// sign (U+00B5) and the Greek small mu (U+03BC) spell microseconds.
fn unit_nanos(unit: &str) -> Option<u64> {
    Some(match unit {
        "ns" => 1,
        "us" | "\u{b5}s" | "\u{3bc}s" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        _ => return None,
    })
}

/// Go `time/format.go::leadingInt`: consumes leading ASCII digits; `None` on
/// overflow past `1<<63`.
fn leading_int(s: &str) -> Option<(u64, &str)> {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    let (digits, rest) = s.split_at_checked(end)?;
    let mut x: u64 = 0;
    for b in digits.bytes() {
        if x > LEADING_INT_MAX {
            return None;
        }
        x = x
            .checked_mul(10)?
            .checked_add(u64::from(b.checked_sub(b'0')?))?;
        if x > GO_OVERFLOW {
            return None;
        }
    }
    Some((x, rest))
}

/// Go `time/format.go::leadingFraction`: consumes leading digits as the
/// fraction value + scale; on overflow keeps consuming digits but stops
/// accumulating (precision is simply lost, exactly as in Go).
fn leading_fraction(s: &str) -> (u64, f64, &str) {
    let mut f: u64 = 0;
    let mut scale: f64 = 1.0;
    let mut overflow = false;
    let mut end = 0usize;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            break;
        }
        end = end.saturating_add(1);
        if overflow {
            continue;
        }
        if f > LEADING_INT_MAX {
            overflow = true;
            continue;
        }
        let digit = u64::from(b.saturating_sub(b'0'));
        let Some(y) = f.checked_mul(10).and_then(|v| v.checked_add(digit)) else {
            overflow = true;
            continue;
        };
        if y > GO_OVERFLOW {
            overflow = true;
            continue;
        }
        f = y;
        scale *= 10.0;
    }
    let rest = s.get(end..).unwrap_or("");
    (f, scale, rest)
}

/// Parses a duration with exactly Go's `time.ParseDuration` grammar
/// (`[-+]?([0-9]*(\.[0-9]*)?(ns|us|µs|μs|ms|s|m|h))+`), e.g. `"30s"`, `"5m"`,
/// `"120ms"`, `"22.5s"`, `"1m0.5s"` (12 §1.4 — NOT humantime).
///
/// # Errors
///
/// Mirrors Go's three error shapes ([`ConfigError::InvalidDuration`],
/// [`ConfigError::MissingDurationUnit`], [`ConfigError::UnknownDurationUnit`]),
/// plus [`ConfigError::NegativeDurationUnsupported`] for negative inputs
/// (`std::time::Duration` is unsigned).
pub fn parse_go_duration(input: &str) -> crate::Result<Duration> {
    let invalid = || ConfigError::InvalidDuration {
        input: input.to_string(),
    };
    let mut s = input;
    let mut neg = false;
    if let Some(rest) = s.strip_prefix('-') {
        neg = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }
    // Special case: a bare zero needs no unit.
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    if s.is_empty() {
        return Err(invalid());
    }
    let mut d: u64 = 0; // total, in nanoseconds
    while !s.is_empty() {
        // The next character must be [0-9.].
        match s.bytes().next() {
            Some(b) if b == b'.' || b.is_ascii_digit() => {}
            _ => return Err(invalid()),
        }
        // Integer part.
        let pre_len = s.len();
        let (mut v, rest) = leading_int(s).ok_or_else(invalid)?;
        s = rest;
        let pre = pre_len != s.len();
        // Fraction part.
        let mut f: u64 = 0;
        let mut scale: f64 = 1.0;
        let mut post = false;
        if let Some(rest) = s.strip_prefix('.') {
            let frac_len = rest.len();
            let (ff, sc, rest) = leading_fraction(rest);
            f = ff;
            scale = sc;
            s = rest;
            post = frac_len != s.len();
        }
        if !pre && !post {
            // No digits at all (e.g. ".s").
            return Err(invalid());
        }
        // Unit token: everything up to the next [0-9.].
        let unit_end = s
            .bytes()
            .position(|b| b == b'.' || b.is_ascii_digit())
            .unwrap_or(s.len());
        if unit_end == 0 {
            return Err(ConfigError::MissingDurationUnit {
                input: input.to_string(),
            });
        }
        let (unit_str, rest) = s.split_at_checked(unit_end).ok_or_else(invalid)?;
        s = rest;
        let unit = unit_nanos(unit_str).ok_or_else(|| ConfigError::UnknownDurationUnit {
            unit: unit_str.to_string(),
            input: input.to_string(),
        })?;
        if v > GO_OVERFLOW.checked_div(unit).ok_or_else(invalid)? {
            return Err(invalid());
        }
        v = v.checked_mul(unit).ok_or_else(invalid)?;
        if f > 0 {
            // Float math mirrors Go: v += uint64(float64(f) * (float64(unit) / scale)).
            let frac_nanos = (f as f64 * (unit as f64 / scale)) as u64;
            v = v.checked_add(frac_nanos).ok_or_else(invalid)?;
            if v > GO_OVERFLOW {
                return Err(invalid());
            }
        }
        d = d.checked_add(v).ok_or_else(invalid)?;
        if d > GO_OVERFLOW {
            return Err(invalid());
        }
    }
    if neg && d > 0 {
        return Err(ConfigError::NegativeDurationUnsupported {
            input: input.to_string(),
        });
    }
    if d > GO_OVERFLOW.saturating_sub(1) {
        return Err(invalid());
    }
    Ok(Duration::from_nanos(d))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn format_go_duration_matches_go_string() {
        // Expected strings are Go `Duration.String()` outputs (the pflag
        // DefValue forms observed in the committed flags.json snapshot).
        let cases: &[(Duration, &str)] = &[
            (Duration::ZERO, "0s"),
            (Duration::from_nanos(1), "1ns"),
            (Duration::from_nanos(1_500), "1.5\u{b5}s"),
            (Duration::from_millis(100), "100ms"),
            (Duration::from_millis(500), "500ms"),
            (Duration::from_secs(30), "30s"),
            (Duration::from_millis(22_500), "22.5s"),
            (Duration::from_secs(300), "5m0s"),
            (Duration::from_secs(60), "1m0s"),
            (Duration::from_secs(2 * 60), "2m0s"),
            (Duration::from_secs(10 * 60), "10m0s"),
            (Duration::from_secs(24 * 60 * 60), "24h0m0s"),
            (Duration::from_secs(8760 * 60 * 60), "8760h0m0s"),
            (Duration::from_millis(60_500), "1m0.5s"),
        ];
        for (d, want) in cases {
            assert_eq!(format_go_duration(*d), *want, "{d:?}");
        }
    }

    #[test]
    fn parse_go_duration_grammar() {
        // Inputs/outputs match Go `time.ParseDuration` (12 §1.4 note).
        let cases: &[(&str, Duration)] = &[
            ("30s", Duration::from_secs(30)),
            ("5m", Duration::from_secs(300)),
            ("120ms", Duration::from_millis(120)),
            ("22.5s", Duration::from_millis(22_500)),
            ("1h", Duration::from_secs(3600)),
            ("1m0.5s", Duration::from_millis(60_500)),
            ("0", Duration::ZERO),
            ("0s", Duration::ZERO),
            ("1h30m", Duration::from_secs(5400)),
            ("1.5h", Duration::from_secs(5400)),
            (".5s", Duration::from_millis(500)),
            ("2.s", Duration::from_secs(2)),
            ("100ns", Duration::from_nanos(100)),
            ("1us", Duration::from_micros(1)),
            ("1\u{b5}s", Duration::from_micros(1)),
            ("1\u{3bc}s", Duration::from_micros(1)),
            ("+5s", Duration::from_secs(5)),
            ("8760h0m0s", Duration::from_secs(8760 * 60 * 60)),
        ];
        for (input, want) in cases {
            let got = parse_go_duration(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(got, *want, "{input}");
        }
    }

    #[test]
    fn parse_go_duration_errors_match_go() {
        use assert_matches::assert_matches;

        use crate::ConfigError;

        // `time: invalid duration` shapes.
        for input in ["", "x", "s", ".", "-", "+", ".s", "+.s", "3000000h"] {
            assert_matches!(
                parse_go_duration(input),
                Err(ConfigError::InvalidDuration { .. }),
                "{input}"
            );
        }
        // `time: missing unit in duration`.
        for input in ["10", "1m10", "22.5"] {
            assert_matches!(
                parse_go_duration(input),
                Err(ConfigError::MissingDurationUnit { .. }),
                "{input}"
            );
        }
        // `time: unknown unit ... in duration ...`.
        assert_matches!(
            parse_go_duration("10x"),
            Err(ConfigError::UnknownDurationUnit { unit, .. }) if unit == "x"
        );
        assert_matches!(
            parse_go_duration("1d"),
            Err(ConfigError::UnknownDurationUnit { unit, .. }) if unit == "d"
        );
        // Deviation: `std::time::Duration` is unsigned, so negative durations
        // (valid in Go) are rejected with a dedicated variant.
        assert_matches!(
            parse_go_duration("-5s"),
            Err(ConfigError::NegativeDurationUnsupported { .. })
        );
    }
}

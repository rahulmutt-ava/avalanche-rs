// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Go `time.Duration` grammar: formatting (and, from M8.3, parsing) that is
//! byte-identical to Go's `Duration.String()` / `time.ParseDuration`
//! (specs 12 §1.4 — pflag duration defaults are `DefValue == d.String()`).

use std::time::Duration;

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
}

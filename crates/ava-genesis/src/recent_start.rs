// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `config.go::getRecentStartTime` — the local-network start-time advance
//! (specs 23 §5.1).
//!
//! The embedded local config's `startTime` is advanced in 9-month chunks until
//! the latest value `<= now`, so a freshly started local network has a recent
//! genesis time. The **unmodified** config remains the golden identity.

/// `localNetworkUpdateStartTimePeriod` = 9 months (`9*30*24h`), in seconds.
pub const LOCAL_NETWORK_UPDATE_START_TIME_PERIOD_SECS: u64 = 9 * 30 * 24 * 60 * 60;

/// Advances `defined_start_time` (unix seconds) in chunks of `period` seconds;
/// returns the latest start time that isn't after `now`.
///
/// Mirrors Go's loop: `next = start + period; if now < next { break }`.
/// Saturates at `u64::MAX` instead of wrapping (unreachable for sane inputs).
#[must_use]
pub fn get_recent_start_time(defined_start_time: u64, now: u64, period: u64) -> u64 {
    let mut start_time = defined_start_time;
    if period == 0 {
        return start_time;
    }
    loop {
        let Some(next_start_time) = start_time.checked_add(period) else {
            return start_time;
        };
        if now < next_start_time {
            return start_time;
        }
        start_time = next_start_time;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8.14 red test: mirror Go `TestGetRecentStartTime` (fixed `now` inputs →
    /// expected advanced start, 23 §5.1). `DEFINED` is
    /// 2024-07-15T04:00:00Z.
    #[test]
    fn get_recent_start_time_table() {
        const DEFINED: u64 = 1_721_016_000;
        const PERIOD: u64 = LOCAL_NETWORK_UPDATE_START_TIME_PERIOD_SECS;
        let cases: [(&str, u64, u64); 8] = [
            // (name, now, expected)
            (
                "before 1 period and 1 second",
                DEFINED - PERIOD - 1,
                DEFINED,
            ),
            ("before 1 second", DEFINED - 1, DEFINED),
            ("equal", DEFINED, DEFINED),
            ("after 1 second", DEFINED + 1, DEFINED),
            ("after 1 period", DEFINED + PERIOD, DEFINED + PERIOD),
            (
                "after 1 period and 1 second",
                DEFINED + PERIOD + 1,
                DEFINED + PERIOD,
            ),
            (
                "after 2 periods",
                DEFINED + 2 * PERIOD,
                DEFINED + 2 * PERIOD,
            ),
            (
                "after 2 periods and 1 second",
                DEFINED + 2 * PERIOD + 1,
                DEFINED + 2 * PERIOD,
            ),
        ];
        for (name, now, expected) in cases {
            assert_eq!(
                get_recent_start_time(DEFINED, now, PERIOD),
                expected,
                "{name}"
            );
        }
        // 9 months == 23_328_000 seconds (9*30*24h) — the Go constant.
        assert_eq!(PERIOD, 23_328_000);
    }
}

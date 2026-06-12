// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ACP-181 epoch selection (Go `vms/proposervm/acp181/epoch.go`).
//!
//! <https://github.com/avalanche-foundation/ACPs/blob/main/ACPs/181-p-chain-epoched-views/README.md>

use chrono::{TimeZone, Utc};

use ava_version::upgrade::UpgradeConfig;

use crate::block::Epoch;

/// Returns a child block's epoch based on its parent (Go `acp181.NewEpoch`).
///
/// Timestamps are Unix seconds (the proposervm block encoding).
#[must_use]
pub fn new_epoch(
    upgrades: &UpgradeConfig,
    parent_p_chain_height: u64,
    parent_epoch: Epoch,
    parent_timestamp: i64,
    child_timestamp: i64,
) -> Epoch {
    let child_time = Utc
        .timestamp_opt(child_timestamp, 0)
        .single()
        .unwrap_or_default();
    if !upgrades.is_granite_activated(child_time) {
        return Epoch::default();
    }

    if parent_epoch.is_zero() {
        // If the parent was not assigned an epoch, then the child is the first
        // block of the initial epoch.
        return Epoch {
            p_chain_height: parent_p_chain_height,
            number: 1,
            start_time: parent_timestamp,
        };
    }

    // `parentEpoch.StartTime + GraniteEpochDuration` (Go `time.Add`); the
    // duration is seconds-granular (5 minutes on every network).
    let duration_secs = i64::try_from(upgrades.granite_epoch_duration.as_secs()).unwrap_or(0);
    let epoch_end_time = parent_epoch.start_time.saturating_add(duration_secs);
    if parent_timestamp < epoch_end_time {
        // If the parent was issued before the end of its epoch, then it did
        // not seal the epoch.
        return parent_epoch;
    }

    // The parent sealed the epoch, so the child is the first block of the new
    // epoch.
    Epoch {
        p_chain_height: parent_p_chain_height,
        number: parent_epoch.number.saturating_add(1),
        start_time: parent_timestamp,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::*;

    /// An upgrade schedule with Granite active from `granite_secs` onward and a
    /// 5-minute epoch duration (the Go default).
    fn upgrades(granite_secs: i64) -> UpgradeConfig {
        let mut config = ava_version::upgrade::get_config(1);
        config.granite_time = Utc
            .timestamp_opt(granite_secs, 0)
            .single()
            .unwrap_or_default();
        config.granite_epoch_duration = Duration::from_secs(5 * 60);
        config
    }

    // Mirrors Go acp181.NewEpoch case-by-case (epoch.go:27-62).
    #[test]
    fn new_epoch_go_parity() {
        let cfg = upgrades(1_000);
        let parent_epoch = Epoch {
            p_chain_height: 7,
            number: 3,
            start_time: 2_000,
        };

        // Granite not yet active at the CHILD timestamp -> zero epoch.
        assert_eq!(
            new_epoch(&cfg, 7, parent_epoch, 500, 999),
            Epoch::default(),
            "pre-Granite child gets the zero epoch"
        );

        // Zero parent epoch -> the child opens epoch 1 at the PARENT timestamp
        // with the PARENT P-Chain height.
        assert_eq!(
            new_epoch(&cfg, 11, Epoch::default(), 1_500, 1_600),
            Epoch {
                p_chain_height: 11,
                number: 1,
                start_time: 1_500,
            },
            "first block of the initial epoch"
        );

        // Parent issued BEFORE its epoch end (start + 300s) -> epoch unchanged.
        assert_eq!(
            new_epoch(&cfg, 99, parent_epoch, 2_299, 2_300),
            parent_epoch,
            "parent did not seal the epoch"
        );

        // Parent issued AT/after the epoch end -> the child opens the next
        // epoch at the parent timestamp / parent P-Chain height.
        assert_eq!(
            new_epoch(&cfg, 99, parent_epoch, 2_300, 2_400),
            Epoch {
                p_chain_height: 99,
                number: 4,
                start_time: 2_300,
            },
            "parent sealed the epoch"
        );
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests for M0.24: `ava-version` invariants.
//!
//! Covers the testing-strategy (`specs/02` §4) mandate for a proptest suite
//! per crate. The key invariants exercised here:
//!
//! - **Version compare is a total order** — `Application::cmp` agrees with the
//!   natural `(major, minor, patch)` tuple ordering (the oracle) and is
//!   antisymmetric. (`name` is excluded from the ordering by design.)
//! - **Upgrade activation monotonicity** — for the shipped Mainnet/Fuji
//!   configs, `is_active(fork, t)` is monotone in `t` and inclusive at the
//!   boundary; `fork_at(t)` is consistent with `is_active`.
//! - **`Fork` chronological `Ord` matches `fork_time` ordering** for the
//!   shipped configs.

use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;

use ava_types::constants::{FUJI_ID, MAINNET_ID};
use ava_version::application::Application;
use ava_version::upgrade::{Fork, UpgradeConfig, get_config};

mod prop {
    use super::*;

    /// Builds an `Application` from a version triple, with a fixed name. The
    /// `name` is intentionally constant because the ordering excludes it.
    fn app(major: u32, minor: u32, patch: u32) -> Application {
        Application::new("avalanchego", major, minor, patch)
    }

    /// Strategy selecting one shipped network ID (Mainnet or Fuji).
    ///
    /// `UpgradeConfig` does not implement `Debug`, so proptest cases pick a
    /// network *id* (which is `Debug`) and build the config inside the test
    /// body.
    fn shipped_network() -> impl Strategy<Value = u32> {
        prop_oneof![Just(MAINNET_ID), Just(FUJI_ID)]
    }

    /// A plausible timestamp range spanning before the first fork through well
    /// after the last scheduled fork (forks run 2021..=2025; Helicon is
    /// unscheduled at year 9999). The range here is in whole seconds since the
    /// Unix epoch, deliberately covering 2018-01-01 .. ~2035.
    fn timestamp() -> impl Strategy<Value = DateTime<Utc>> {
        // 2018-01-01T00:00:00Z .. 2035-01-01T00:00:00Z (seconds).
        (1_514_764_800_i64..2_051_222_400_i64).prop_map(|secs| {
            Utc.timestamp_opt(secs, 0)
                .single()
                .expect("static range: always valid")
        })
    }

    proptest! {
        /// `Application::cmp` agrees with the `(major, minor, patch)` tuple
        /// oracle, and the ordering is antisymmetric.
        #[test]
        fn version_compare_total_order(
            a in (any::<u32>(), any::<u32>(), any::<u32>()),
            b in (any::<u32>(), any::<u32>(), any::<u32>()),
        ) {
            let va = app(a.0, a.1, a.2);
            let vb = app(b.0, b.1, b.2);

            // Oracle: natural tuple ordering.
            let oracle = a.cmp(&b);
            prop_assert_eq!(va.cmp(&vb), oracle);

            // Antisymmetry: cmp(a,b) == reverse(cmp(b,a)).
            prop_assert_eq!(va.cmp(&vb), vb.cmp(&va).reverse());

            // PartialOrd consistency with Ord.
            prop_assert_eq!(va.partial_cmp(&vb), Some(va.cmp(&vb)));

            // Equality reflexivity for the ordering.
            prop_assert_eq!(va.cmp(&va), std::cmp::Ordering::Equal);
        }

        /// `Application::cmp` is transitive over arbitrary triples.
        #[test]
        fn version_compare_transitive(
            a in (any::<u32>(), any::<u32>(), any::<u32>()),
            b in (any::<u32>(), any::<u32>(), any::<u32>()),
            c in (any::<u32>(), any::<u32>(), any::<u32>()),
        ) {
            let va = app(a.0, a.1, a.2);
            let vb = app(b.0, b.1, b.2);
            let vc = app(c.0, c.1, c.2);

            // If a <= b and b <= c then a <= c.
            if va <= vb && vb <= vc {
                prop_assert!(va <= vc);
            }
        }

        /// `name` is excluded from the ordering: two `Application`s with the
        /// same triple but different names compare `Equal`.
        #[test]
        fn version_compare_ignores_name(
            v in (any::<u32>(), any::<u32>(), any::<u32>()),
            name in "[a-z][a-z0-9-]{0,15}",
        ) {
            let canonical = app(v.0, v.1, v.2);
            let renamed = Application::new(name, v.0, v.1, v.2);
            prop_assert_eq!(canonical.cmp(&renamed), std::cmp::Ordering::Equal);
        }

        /// For the shipped Mainnet/Fuji configs, `is_active(fork, t)` is monotone
        /// in `t` (once active, never deactivates) and inclusive at the boundary;
        /// `fork_at(t)` is consistent with `is_active`.
        #[test]
        fn upgrade_activation_monotone(
            network_id in shipped_network(),
            t in timestamp(),
        ) {
            let config: UpgradeConfig = get_config(network_id);
            for fork in Fork::ALL {
                let fork_time = config.fork_time(fork);
                let active = config.is_active(fork, t);

                // is_active iff t >= fork_time (the canonical gate).
                prop_assert_eq!(active, t >= fork_time);

                // Boundary inclusive: active exactly at fork_time.
                prop_assert!(config.is_active(fork, fork_time));

                // One nanosecond before the boundary: inactive.
                if let Some(just_before) =
                    fork_time.checked_sub_signed(chrono::Duration::nanoseconds(1))
                {
                    prop_assert!(!config.is_active(fork, just_before));
                }

                // Monotone in t: if active at t, active at any later time.
                if active
                    && let Some(later) = t.checked_add_signed(chrono::Duration::seconds(3600))
                {
                    prop_assert!(config.is_active(fork, later));
                }
            }

            // fork_at(t) is the latest fork with fork_time <= t, consistent with
            // is_active.
            let at = config.fork_at(t);
            match at {
                Some(fork) => {
                    // The returned fork must itself be active at t.
                    prop_assert!(config.is_active(fork, t));
                    // No strictly-later fork (by chronological Ord) may be active.
                    for later in Fork::ALL.iter().copied().filter(|&f| f > fork) {
                        prop_assert!(!config.is_active(later, t));
                    }
                }
                None => {
                    // No fork active → t precedes the earliest fork.
                    for fork in Fork::ALL {
                        prop_assert!(!config.is_active(fork, t));
                    }
                }
            }
        }

        /// For all pairs of `Fork` variants, the chronological `Ord` agrees with
        /// the `fork_time` ordering of the shipped configs. The `network_id`
        /// input drives shrinking; the pair scan is exhaustive per case.
        #[test]
        fn fork_times_match_ord(network_id in shipped_network()) {
            let config: UpgradeConfig = get_config(network_id);
            for a in Fork::ALL {
                for b in Fork::ALL {
                    if a <= b {
                        prop_assert!(
                            config.fork_time(a) <= config.fork_time(b),
                            "Fork::Ord {:?} <= {:?} but fork_time {} > {}",
                            a, b, config.fork_time(a), config.fork_time(b)
                        );
                    }
                }
            }
        }
    }
}

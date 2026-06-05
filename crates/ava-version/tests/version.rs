// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tests for M0.22: `Application` + `Compatibility`.
//! Mirrors `version/application_test.go` and `version/compatibility_test.go`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_version::{
    APPLICATION_NAME,
    CLIENT, CURRENT, CURRENT_DATABASE, MINIMUM_COMPATIBLE, PREV_MINIMUM_COMPATIBLE,
    RPC_CHAIN_VM_PROTOCOL,
    application::Application,
    compatibility::{Compatibility, MockClock},
};

// ─── Application display ──────────────────────────────────────────────────────

#[test]
fn application_display_compare() {
    // Constants sanity
    assert_eq!(CLIENT, "avalanchego");
    assert_eq!(RPC_CHAIN_VM_PROTOCOL, 45_u32);
    assert_eq!(CURRENT_DATABASE, "v1.4.5");

    // CURRENT should be avalanchego 1.14.2
    assert_eq!(CURRENT.name, "avalanchego");
    assert_eq!(CURRENT.major, 1);
    assert_eq!(CURRENT.minor, 14);
    assert_eq!(CURRENT.patch, 2);

    // display() == "avalanchego/1.14.2"
    assert_eq!(CURRENT.display(), "avalanchego/1.14.2");
    // semantic() == "v1.14.2"
    assert_eq!(CURRENT.semantic(), "v1.14.2");
    // Display trait
    assert_eq!(format!("{}", *CURRENT), "avalanchego/1.14.2");
    // semantic_with_commit
    assert_eq!(CURRENT.semantic_with_commit(""), "v1.14.2");
    assert_eq!(CURRENT.semantic_with_commit("abc123"), "v1.14.2@abc123");

    // Ordering: major → minor → patch; name excluded.
    let v1_13_0 = Application { name: "avalanchego".into(), major: 1, minor: 13, patch: 0 };
    let v1_14_0 = Application { name: "avalanchego".into(), major: 1, minor: 14, patch: 0 };
    let v1_14_2 = Application { name: "avalanchego".into(), major: 1, minor: 14, patch: 2 };
    let v2_0_0 = Application { name: "avalanchego".into(), major: 2, minor: 0, patch: 0 };
    assert!(v1_13_0 < v1_14_0);
    assert!(v1_14_0 < v1_14_2);
    assert!(v1_14_2 < v2_0_0);
    assert_eq!(v1_14_2, v1_14_2.clone());

    // Name does NOT affect ordering
    let other_name = Application { name: "avalanche-rs".into(), major: 1, minor: 14, patch: 2 };
    use std::cmp::Ordering;
    assert_eq!(v1_14_2.cmp(&other_name), Ordering::Equal);

    // MINIMUM_COMPATIBLE == 1.14.0
    assert_eq!(MINIMUM_COMPATIBLE.major, 1);
    assert_eq!(MINIMUM_COMPATIBLE.minor, 14);
    assert_eq!(MINIMUM_COMPATIBLE.patch, 0);

    // PREV_MINIMUM_COMPATIBLE == 1.13.0
    assert_eq!(PREV_MINIMUM_COMPATIBLE.major, 1);
    assert_eq!(PREV_MINIMUM_COMPATIBLE.minor, 13);
    assert_eq!(PREV_MINIMUM_COMPATIBLE.patch, 0);

    // APPLICATION_NAME constant
    assert_eq!(APPLICATION_NAME, "avalanchego");
}

// ─── Compatibility ────────────────────────────────────────────────────────────

fn make_compat_with_clock(upgrade_time: SystemTime, now: SystemTime) -> Compatibility<MockClock> {
    Compatibility::with_clock(
        CURRENT.clone(),
        MINIMUM_COMPATIBLE.clone(),
        PREV_MINIMUM_COMPATIBLE.clone(),
        upgrade_time,
        MockClock::new(now),
    )
}

#[test]
fn compatibility_peer_newer_major_rejected() {
    // Clause 1: peer on a newer major → reject.
    let upgrade_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1);
    let compat = make_compat_with_clock(upgrade_time, now);
    let peer = Application { name: "avalanchego".into(), major: 2, minor: 0, patch: 0 };
    assert!(!compat.compatible(&peer));
}

#[test]
fn compatibility_pre_upgrade_floor() {
    // Before upgrade_time: floor = PREV_MINIMUM_COMPATIBLE (1.13.0)
    let upgrade_time = UNIX_EPOCH + Duration::from_secs(1000);
    let now = UNIX_EPOCH + Duration::from_secs(500); // before upgrade

    let compat = make_compat_with_clock(upgrade_time, now);

    // 1.14.0 >= 1.13.0 → accept
    let peer_140 = Application { name: "avalanchego".into(), major: 1, minor: 14, patch: 0 };
    assert!(compat.compatible(&peer_140));

    // 1.13.0 >= 1.13.0 → accept
    let peer_130 = Application { name: "avalanchego".into(), major: 1, minor: 13, patch: 0 };
    assert!(compat.compatible(&peer_130));

    // 1.12.9 < 1.13.0 → reject
    let peer_129 = Application { name: "avalanchego".into(), major: 1, minor: 12, patch: 9 };
    assert!(!compat.compatible(&peer_129));
}

#[test]
fn compatibility_post_upgrade_floor() {
    // After upgrade_time: floor = MINIMUM_COMPATIBLE (1.14.0)
    let upgrade_time = UNIX_EPOCH + Duration::from_secs(500);
    let now = UNIX_EPOCH + Duration::from_secs(1000); // after upgrade

    let compat = make_compat_with_clock(upgrade_time, now);

    // 1.14.0 >= 1.14.0 → accept
    let peer_140 = Application { name: "avalanchego".into(), major: 1, minor: 14, patch: 0 };
    assert!(compat.compatible(&peer_140));

    // 1.14.2 >= 1.14.0 → accept
    let peer_142 = Application { name: "avalanchego".into(), major: 1, minor: 14, patch: 2 };
    assert!(compat.compatible(&peer_142));

    // 1.13.9 < 1.14.0 → reject
    let peer_139 = Application { name: "avalanchego".into(), major: 1, minor: 13, patch: 9 };
    assert!(!compat.compatible(&peer_139));
}

#[test]
fn compatibility_same_version_accepted() {
    let upgrade_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1);
    let compat = make_compat_with_clock(upgrade_time, now);
    assert!(compat.compatible(&CURRENT));
}

#[test]
fn compatibility_different_name_compatible_version_accepted() {
    // Name is not compared in the compatibility check
    let upgrade_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1);
    let compat = make_compat_with_clock(upgrade_time, now);
    let peer = Application { name: "some-other-client".into(), major: 1, minor: 14, patch: 2 };
    assert!(compat.compatible(&peer));
}

#[test]
fn compatibility_mid_connection_transition() {
    // A peer that was acceptable pre-upgrade is rejected after the clock crosses upgrade_time.
    // peer = 1.13.5 (>= 1.13.0 pre-upgrade floor, < 1.14.0 post-upgrade floor)
    let upgrade_time = UNIX_EPOCH + Duration::from_secs(1000);
    let peer = Application { name: "avalanchego".into(), major: 1, minor: 13, patch: 5 };

    // Before upgrade: floor=1.13.0 → accept
    let pre = make_compat_with_clock(upgrade_time, UNIX_EPOCH + Duration::from_secs(500));
    assert!(pre.compatible(&peer));

    // After upgrade: floor=1.14.0 → reject
    let post = make_compat_with_clock(upgrade_time, UNIX_EPOCH + Duration::from_secs(1001));
    assert!(!post.compatible(&peer));
}

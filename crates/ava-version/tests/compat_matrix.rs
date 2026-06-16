// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.22 — Version-string / compatibility-matrix interop conformance (golden legs).
//!
//! Implements the three pure-Rust golden tests named by `specs/26` §9 and the
//! M9.22 task in `plan/M9-interop-hardening.md`:
//!
//! - `golden::compatibility_matrix` — every mandatory `compatible()` cell from
//!   `specs/26` §9(3).
//! - `golden::compatibility_json_byte_parity` — the embedded
//!   `compatibility.json` is byte-identical to the Go tree's and parses to the
//!   table the code loads (`specs/26` §9(6), §4).
//! - `golden::node_version_reply` — the `info.getNodeVersion` fields that
//!   `ava-version` owns match Go field-for-field, plus the version-string
//!   display goldens (`specs/26` §9(1)/(2)).
//!
//! The FOURTH M9.22 test, `differential::version_interop`, lives in
//! `tests/differential/tests/version_interop.rs` (it must not live here: a T0
//! primitive crate like `ava-version` may not depend on
//! `ava-differential`/`ava-network`/`ava-api`). Its OFFLINE arm — driving this
//! crate's real `Compatibility` over the mixed-net peer set — now exists; only
//! the LIVE two-binary drop arm remains gated. See the `version_interop_deferred`
//! note at the bottom of this file and `tests/PORTING.md`.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_version::{
    CURRENT, CURRENT_DATABASE, MINIMUM_COMPATIBLE, PREV_MINIMUM_COMPATIBLE, RPC_CHAIN_VM_PROTOCOL,
    application::Application,
    compat_table::COMPATIBILITY_JSON,
    compatibility::{Compatibility, MockClock},
    rpc_chain_vm_protocol_compatibility,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Builds a `Compatibility` with the shipped constants and a mock clock fixed at
/// `now`, gating the floor switch on `upgrade_time`. Mirrors the inputs
/// `ava-network` threads in at the handshake (`specs/26` §3.1).
fn compat(upgrade_time: SystemTime, now: SystemTime) -> Compatibility<MockClock> {
    Compatibility::with_clock(
        CURRENT.clone(),
        MINIMUM_COMPATIBLE.clone(),
        PREV_MINIMUM_COMPATIBLE.clone(),
        upgrade_time,
        MockClock::new(now),
    )
}

/// Builds a peer `Application` triple with the canonical `avalanchego` name.
fn peer(major: u32, minor: u32, patch: u32) -> Application {
    Application::new("avalanchego", major, minor, patch)
}

// ─── golden::compatibility_matrix ───────────────────────────────────────────────
//
// Asserts every mandatory cell from `specs/26` §9(3): the `(our_current,
// floor_pre, floor_post, upgrade_time, clock, peer_version) → accept/reject`
// table. Our constants: current=1.14.2, post-floor (MINIMUM_COMPATIBLE)=1.14.0,
// pre-floor (PREV_MINIMUM_COMPATIBLE)=1.13.0.

mod golden {
    use super::*;

    #[test]
    fn compatibility_matrix() {
        // Two clocks straddling a fork at t=1000.
        let upgrade_time = UNIX_EPOCH + Duration::from_secs(1000);
        let before = UNIX_EPOCH + Duration::from_secs(500); // clock < upgrade_time
        let after = UNIX_EPOCH + Duration::from_secs(1500); // clock >= upgrade_time

        let pre = compat(upgrade_time, before);
        let post = compat(upgrade_time, after);

        // §9(3) cell: peer on a NEWER MAJOR than ours → reject (clause 1).
        // current major is 1; a major-2 peer is rejected regardless of clock.
        assert!(
            !pre.compatible(&peer(2, 0, 0)),
            "newer-major peer must be rejected (clause 1, pre-upgrade clock)"
        );
        assert!(
            !post.compatible(&peer(2, 0, 0)),
            "newer-major peer must be rejected (clause 1, post-upgrade clock)"
        );

        // §9(3) cell: peer BELOW THE PRE-UPGRADE FLOOR with clock < upgrade_time
        // → reject. Pre-floor is 1.13.0, so 1.12.9 is below it.
        assert!(
            !pre.compatible(&peer(1, 12, 9)),
            "peer below pre-upgrade floor (1.13.0) must be rejected when clock < upgrade_time"
        );

        // §9(3) cell: same peer with clock < upgrade_time but >= pre-floor → accept.
        // 1.13.0 == pre-floor (boundary inclusive); 1.13.5 > pre-floor.
        assert!(
            pre.compatible(&peer(1, 13, 0)),
            "peer == pre-upgrade floor (1.13.0) must be accepted when clock < upgrade_time"
        );
        assert!(
            pre.compatible(&peer(1, 13, 5)),
            "peer >= pre-upgrade floor must be accepted when clock < upgrade_time"
        );

        // §9(3) cell: peer >= pre-floor but < post-floor, with clock >= upgrade_time
        // → REJECT (the fork-boundary cut-over). 1.13.5 is in [1.13.0, 1.14.0).
        assert!(
            !post.compatible(&peer(1, 13, 5)),
            "fork-boundary cut-over: peer in [pre-floor, post-floor) rejected when clock >= upgrade_time"
        );
        // Boundary: 1.13.0 (== pre-floor) is also < post-floor → reject post-upgrade.
        assert!(
            !post.compatible(&peer(1, 13, 0)),
            "fork-boundary cut-over: peer == pre-floor still < post-floor (1.14.0), rejected post-upgrade"
        );

        // §9(3) cell: peer == our CURRENT → accept (both clocks).
        assert!(
            pre.compatible(&CURRENT),
            "peer == current (1.14.2) must be accepted (pre-upgrade clock)"
        );
        assert!(
            post.compatible(&CURRENT),
            "peer == current (1.14.2) must be accepted (post-upgrade clock)"
        );

        // §9(3) cell: peer NEWER (same major) → accept (we only log, never reject).
        // 1.14.9 and 1.15.0 are > current but same major 1.
        assert!(
            post.compatible(&peer(1, 14, 9)),
            "newer same-major peer (1.14.9) must be accepted (we only log)"
        );
        assert!(
            post.compatible(&peer(1, 15, 0)),
            "newer same-major peer (1.15.0) must be accepted (we only log)"
        );

        // §9(3) cell: peer with a DIFFERENT name but compatible triple → accept
        // (name is not part of the compatibility check).
        let differently_named = Application::new("some-other-client", 1, 14, 2);
        assert!(
            post.compatible(&differently_named),
            "differently-named peer with compatible triple must be accepted (name not compared)"
        );

        // §9(3) cell: the MID-CONNECTION transition. A peer accepted before
        // `upgrade_time` is rejected after the clock crosses it. peer=1.13.5 is
        // >= pre-floor (accept) but < post-floor (reject). Modeled with two
        // clocks on the same upgrade_time (the Ping re-check, `specs/26` §3.1).
        let transition_peer = peer(1, 13, 5);
        assert!(
            pre.compatible(&transition_peer),
            "mid-connection: peer compatible before the clock crosses upgrade_time"
        );
        assert!(
            !post.compatible(&transition_peer),
            "mid-connection: same peer rejected after the clock crosses upgrade_time"
        );

        // Sanity on the post-upgrade floor itself: 1.14.0 == post-floor → accept;
        // 1.13.9 (just below) → reject.
        assert!(
            post.compatible(&peer(1, 14, 0)),
            "peer == post-upgrade floor (1.14.0) accepted when clock >= upgrade_time"
        );
        assert!(
            !post.compatible(&peer(1, 13, 9)),
            "peer just below post-upgrade floor (1.13.9) rejected when clock >= upgrade_time"
        );
    }
}

// ─── golden::compatibility_json_byte_parity ─────────────────────────────────────

mod golden_json {
    use pretty_assertions::assert_eq;

    use super::*;

    /// `specs/26` §9(6): the embedded `compatibility.json` is byte-identical to
    /// the Go tree's `version/compatibility.json` and parses to the same table
    /// the code loads. We assert byte parity against the committed file (the
    /// provenance + a re-copy command live in `compatibility.json.md`), and that
    /// the parsed table is well-formed and pins the protocol-45 → v1.14.2 row.
    #[test]
    fn compatibility_json_byte_parity() {
        // The crate-relative committed file, read independently of the
        // `include_str!`-embedded copy.
        let committed_path = concat!(env!("CARGO_MANIFEST_DIR"), "/compatibility.json");
        let committed = std::fs::read_to_string(committed_path)
            .expect("read crates/ava-version/compatibility.json");

        // (1) Byte parity: the embedded `include_str!` copy IS the committed file.
        assert_eq!(
            COMPATIBILITY_JSON, committed,
            "embedded compatibility.json must be byte-identical to the committed file"
        );

        // (2) The committed file parses (byte-faithful) to the table the code
        //     loads via `rpc_chain_vm_protocol_compatibility()`.
        let from_code =
            rpc_chain_vm_protocol_compatibility().expect("embedded compatibility.json parses");
        let from_file: BTreeMap<String, Vec<String>> =
            serde_json::from_str(&committed).expect("committed compatibility.json parses");
        let from_file_rekeyed: BTreeMap<u32, Vec<String>> = from_file
            .into_iter()
            .map(|(k, v)| (k.parse::<u32>().expect("decimal protocol key"), v))
            .collect();
        assert_eq!(
            from_code, from_file_rekeyed,
            "code-loaded table must equal the freshly-parsed committed file"
        );

        // (3) The table pins the current protocol → release mapping (the row that
        //     moves on every node-version bump). Protocol 45 ⇒ exactly [v1.14.2].
        assert_eq!(
            from_code.get(&(RPC_CHAIN_VM_PROTOCOL)),
            Some(&vec!["v1.14.2".to_string()]),
            "protocol {RPC_CHAIN_VM_PROTOCOL} must map to exactly [v1.14.2]"
        );

        // The current node version must appear under the current protocol key.
        let current_semantic = CURRENT.semantic(); // "v1.14.2"
        assert!(
            from_code
                .get(&RPC_CHAIN_VM_PROTOCOL)
                .is_some_and(|vers| vers.contains(&current_semantic)),
            "CURRENT.semantic() ({current_semantic}) must appear under protocol {RPC_CHAIN_VM_PROTOCOL}"
        );

        // (4) A couple of stable historical rows from `specs/26` §4, guarding the
        //     full table is present (not just the head row).
        assert_eq!(
            from_code.get(&44),
            Some(&vec!["v1.14.0".to_string(), "v1.14.1".to_string()]),
            "protocol 44 ⇒ [v1.14.0, v1.14.1]"
        );
        assert_eq!(
            from_code.get(&16),
            Some(&vec![
                "v1.8.0".to_string(),
                "v1.8.1".to_string(),
                "v1.8.2".to_string(),
                "v1.8.3".to_string(),
                "v1.8.4".to_string(),
                "v1.8.5".to_string(),
                "v1.8.6".to_string(),
            ]),
            "protocol 16 ⇒ the v1.8.x series"
        );
    }
}

// ─── golden::node_version_reply ─────────────────────────────────────────────────

mod golden_reply {
    use pretty_assertions::assert_eq;

    use super::*;

    /// `specs/26` §9(1)/(2): the version-string display goldens and the
    /// `info.getNodeVersion` fields that `ava-version` owns.
    ///
    /// The FULL `info.getNodeVersion` JSON reply (incl. `gitCommit` and
    /// `vmVersions`) is golden-tested at the `ava-api` layer
    /// (`crates/ava-api/src/info/mod.rs`) — `ava-api` depends on `ava-version`,
    /// not vice-versa, so asserting the full reply here would invert the
    /// dependency edge. Here we pin the fields `ava-version` is the source of
    /// truth for: `version` (== `Application.String()`), `databaseVersion`
    /// (== `CURRENT_DATABASE`), and `rpcProtocolVersion` (== `RPC_CHAIN_VM_PROTOCOL`,
    /// serialized as a STRING per Go's `json.Uint32`).
    #[test]
    fn node_version_reply() {
        // §9(1) version-string display goldens. Bytes pinned against Go's
        // `Application.String()` / `Semantic()` / `SemanticWithCommit()`.
        let app = Application::new("avalanchego", 1, 14, 2);
        assert_eq!(
            app.display(),
            "avalanchego/1.14.2",
            "Application.display() == Go Application.String()"
        );
        assert_eq!(
            app.semantic(),
            "v1.14.2",
            "Application.semantic() == Go Application.Semantic()"
        );
        assert_eq!(
            app.semantic_with_commit("abc"),
            "v1.14.2@abc",
            "semantic_with_commit(\"abc\") == Go SemanticWithCommit(\"abc\")"
        );
        assert_eq!(
            app.semantic_with_commit(""),
            "v1.14.2",
            "empty-commit case == Semantic()"
        );
        // The shipped CURRENT renders the same way.
        assert_eq!(
            CURRENT.display(),
            "avalanchego/1.14.2",
            "CURRENT.display() golden"
        );

        // §9(2) `info.getNodeVersion` — the ava-version-owned fields, matching Go
        // `GetNodeVersionReply` field-for-field (modulo build-specific gitCommit/go).
        //   reply.Version          = i.Version.String()             -> "avalanchego/1.14.2"
        //   reply.DatabaseVersion  = version.CurrentDatabase        -> "v1.4.5"
        //   reply.RPCProtocolVersion = json.Uint32(RPCChainVMProtocol) -> "45" (STRING)
        assert_eq!(
            CURRENT.display(),
            "avalanchego/1.14.2",
            "getNodeVersion.version == Application.String()"
        );
        assert_eq!(
            CURRENT_DATABASE, "v1.4.5",
            "getNodeVersion.databaseVersion == CURRENT_DATABASE"
        );
        assert_eq!(
            RPC_CHAIN_VM_PROTOCOL, 45,
            "getNodeVersion.rpcProtocolVersion numeric value"
        );
        // Go encodes `json.Uint32` as a JSON STRING; assert the string form.
        assert_eq!(
            RPC_CHAIN_VM_PROTOCOL.to_string(),
            "45",
            "getNodeVersion.rpcProtocolVersion serializes as the string \"45\" (Go json.Uint32)"
        );
    }
}

// ─── differential::version_interop — moved out of this crate ────────────────────
//
// The fourth M9.22 test, `differential::version_interop` (`specs/26` §9(4)), now
// lives in `tests/differential/tests/version_interop.rs` — it must NOT live here
// (a T0 primitive crate like `ava-version` may not depend on
// `ava-differential`/`ava-network`/`ava-api`).
//
// Its OFFLINE arm (`version_interop_floor_decisions`) is COMPLETE: it drives this
// crate's real `Compatibility` over the M9.14 mixed-net `BinaryMix`/`NodeIdentity`
// peer set with a `MockClock` straddling the fork boundary, asserting the
// §9(4)/§9(3) connectivity decisions (below-floor drop, at/above-floor accept,
// the moving floor flipping a borderline peer, and Go-vs-Rust symmetry).
//
// The LIVE two-binary drop arm (boot a mixed Go+Rust net, lower a node below the
// other's floor, assert the drop in both directions) remains gated behind that
// crate's `live` feature + `#[ignore]`. Tracked in `tests/PORTING.md`.

#[test]
#[ignore = "MOVED: differential::version_interop offline arm now lives in \
            tests/differential/tests/version_interop.rs (version_interop_floor_decisions); \
            the live two-binary drop arm there stays #[cfg(feature=\"live\")] + #[ignore]"]
fn version_interop_deferred() {
    // Intentionally empty: documents (in `cargo nextest run --ignored` inventory)
    // that the real test moved to tests/differential, off this T0 crate.
}

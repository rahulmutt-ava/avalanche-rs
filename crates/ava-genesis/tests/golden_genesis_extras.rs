// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M8.15 genesis extras (specs 23 §3.6 / §9.2 / §9.6):
//!
//! 1. `cchain_genesis_timestamp` — the `cChainGenesis` embedded JSON carries the
//!    expected genesis header timestamp for each network (0 for Mainnet/Fuji; the
//!    `InitiallyActiveTime` unix second for Local).
//!
//! 2. `rebuild_parity` — unparsed→parsed→re-serialize round-trip stability: parsing
//!    the embedded JSON then re-serializing the `UnparsedConfig` back to a string and
//!    re-parsing it produces a `Config` that is identical to the first parse, AND
//!    `from_config` on both configs produces identical P-chain genesis bytes.  This
//!    guards against any re-serialization ambiguity in the JSON layer.
//!
//!    The existing `golden_genesis_block_id::genesis_p_chain_bytes_byte_identical`
//!    already pins from_config byte-streams against Go `.bin` dumps; this test
//!    adds the orthogonal *re-serialize-then-rebuild* stability variant.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use ava_evm::chainspec::CChainGenesis;
use ava_genesis::config::{
    FUJI_GENESIS_CONFIG_JSON, LOCAL_GENESIS_CONFIG_JSON, MAINNET_GENESIS_CONFIG_JSON,
    UNMODIFIED_LOCAL_CONFIG,
};
use ava_genesis::{from_config, unparsed::UnparsedConfig};
use ava_version::upgrade::initially_active_time;

// ── tests ──────────────────────────────────────────────────────────────────────

/// M8.15 §1: the `cChainGenesis` timestamp embedded in each network genesis.
///
/// Mainnet and Fuji embed `0x0` (genesis at the Unix epoch).
/// Local embeds `unix(InitiallyActiveTime)` = `1607144400`
/// (2020-12-05 05:00:00 UTC, per specs 23 §3.6 and `upgrade.go`).
///
/// The embedded JSON data is authoritative; if it ever diverges from the
/// ava-version constant, the JSON must be updated — not this golden.
#[test]
fn cchain_genesis_timestamp() {
    // Compute the expected Local timestamp from ava-version.
    let initially_active_unix = u64::try_from(initially_active_time().timestamp())
        .expect("InitiallyActiveTime must be post-epoch");

    struct Case {
        name: &'static str,
        json: &'static str,
        expected_chain_id: u64,
        expected_timestamp: u64,
    }

    let cases = [
        Case {
            name: "mainnet",
            json: MAINNET_GENESIS_CONFIG_JSON,
            expected_chain_id: 43114,
            expected_timestamp: 0,
        },
        Case {
            name: "fuji",
            json: FUJI_GENESIS_CONFIG_JSON,
            expected_chain_id: 43113,
            expected_timestamp: 0,
        },
        Case {
            name: "local",
            json: LOCAL_GENESIS_CONFIG_JSON,
            expected_chain_id: 43112,
            expected_timestamp: initially_active_unix,
        },
    ];

    for case in &cases {
        // The cChainGenesis string must be non-empty (embedded at compile time).
        let unparsed: UnparsedConfig =
            serde_json::from_str(case.json).expect("parse embedding JSON");
        let config = unparsed.parse().expect("parse Config");
        assert!(
            !config.c_chain_genesis.is_empty(),
            "{}: cChainGenesis must be non-empty",
            case.name
        );

        let cchain = CChainGenesis::parse(&config.c_chain_genesis)
            .unwrap_or_else(|e| panic!("{}: CChainGenesis::parse failed: {e}", case.name));

        assert_eq!(
            cchain.chain_id(),
            case.expected_chain_id,
            "{}: chain_id",
            case.name
        );
        assert_eq!(
            cchain.timestamp(),
            case.expected_timestamp,
            "{}: timestamp (expected {}; embedded JSON is authoritative)",
            case.name,
            case.expected_timestamp
        );
    }

    // Confirm the local golden matches the ava-version constant exactly.
    assert_eq!(
        initially_active_unix, 1_607_144_400,
        "InitiallyActiveTime unix constant must be 1607144400 (2020-12-05T05:00:00Z)"
    );
}

/// M8.15 §2: unparsed→parsed→re-serialize→re-parse round-trip stability
/// (specs 23 §9.2 / §9.6).
///
/// For Mainnet, Fuji, and Local:
///   1. parse the embedded JSON to `UnparsedConfig` → `Config` (first parse).
///   2. serialize the `UnparsedConfig` back to JSON (`serde_json::to_string`).
///   3. parse the re-serialized JSON → `Config` (second parse).
///   4. assert the two `Config` values are equal (round-trip stability).
///   5. run `from_config` on both and assert byte-identical P-chain output.
///
/// This is the genuine delta over `genesis_p_chain_bytes_byte_identical` in
/// `golden_genesis_block_id.rs`, which only pins against the committed Go .bin
/// dumps but does not exercise the re-serialization path.
#[test]
fn rebuild_parity() {
    struct Case {
        name: &'static str,
        json: &'static str,
    }

    let cases = [
        Case {
            name: "mainnet",
            json: MAINNET_GENESIS_CONFIG_JSON,
        },
        Case {
            name: "fuji",
            json: FUJI_GENESIS_CONFIG_JSON,
        },
        Case {
            name: "local_unmodified",
            json: LOCAL_GENESIS_CONFIG_JSON,
        },
    ];

    for case in &cases {
        // Step 1: first parse.
        let unparsed_1: UnparsedConfig =
            serde_json::from_str(case.json).expect("first parse of embedding JSON");
        let config_1 = unparsed_1.parse().expect("first parse to Config");

        // Step 2: re-serialize the UnparsedConfig.
        let reserialized = serde_json::to_string(&unparsed_1).expect("re-serialize UnparsedConfig");

        // Step 3: second parse from the re-serialized form.
        let unparsed_2: UnparsedConfig =
            serde_json::from_str(&reserialized).expect("parse re-serialized JSON");
        let config_2 = unparsed_2.parse().expect("second parse to Config");

        // Step 4: the two parsed Configs must be equal.
        assert_eq!(
            config_1, config_2,
            "{}: re-serialized Config must equal the original parse",
            case.name
        );

        // Step 5: from_config bytes must be identical.
        let (bytes_1, asset_id_1) = from_config(&config_1)
            .unwrap_or_else(|e| panic!("{}: from_config (first) failed: {e}", case.name));
        let (bytes_2, asset_id_2) = from_config(&config_2)
            .unwrap_or_else(|e| panic!("{}: from_config (second) failed: {e}", case.name));

        assert_eq!(
            bytes_1.len(),
            bytes_2.len(),
            "{}: P-chain genesis byte length must match after re-serialization",
            case.name
        );
        assert_eq!(
            bytes_1, bytes_2,
            "{}: P-chain genesis bytes must be byte-identical after re-serialization",
            case.name
        );
        assert_eq!(
            asset_id_1, asset_id_2,
            "{}: AVAX asset id must match after re-serialization",
            case.name
        );
    }

    // Local: additionally confirm the UNMODIFIED_LOCAL_CONFIG (parsed from the
    // same embedded JSON via LazyLock) produces the same bytes as a fresh parse —
    // the two paths must agree.
    let fresh_local: UnparsedConfig =
        serde_json::from_str(LOCAL_GENESIS_CONFIG_JSON).expect("fresh local parse");
    let fresh_config = fresh_local.parse().expect("fresh local Config");
    let (bytes_static, _) =
        from_config(&UNMODIFIED_LOCAL_CONFIG).expect("from_config UNMODIFIED_LOCAL_CONFIG");
    let (bytes_fresh, _) = from_config(&fresh_config).expect("from_config fresh local");
    assert_eq!(
        bytes_static, bytes_fresh,
        "UNMODIFIED_LOCAL_CONFIG bytes must equal fresh-parsed local bytes"
    );
}

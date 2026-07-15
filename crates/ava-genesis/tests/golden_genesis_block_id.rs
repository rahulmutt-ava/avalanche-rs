// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! THE genesis-block-ID golden test (specs 23 §7/§9.1, 02 §6.2/§6.3) — the
//! per-PR exit gate for M8.8 and the strongest single Go-compatibility check:
//! a node whose genesis identity drifts from this table cannot join Mainnet,
//! Fuji, or Local.
//!
//! Golden values come from two independent sources, both asserted:
//! 1. the §7 tables (copied verbatim from Go `genesis/genesis_test.go`), and
//! 2. the committed Go byte dumps under `tests/vectors/genesis/` (emitted by
//!    `xtask gen-genesis` from `genesis.FromConfig` — see `vectors/genesis/README.md`).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use ava_genesis::config::{FUJI_CONFIG, MAINNET_CONFIG, UNMODIFIED_LOCAL_CONFIG};
use ava_genesis::{Chain, Config, chains, from_config, genesis_bytes, vm_genesis};

/// One row of the specs 23 §7 golden table.
struct GoldenRow {
    name: &'static str,
    network_id: u32,
    p_genesis_block_id: &'static str,
    x_blockchain_id: &'static str,
    c_blockchain_id: &'static str,
    avax_asset_id: &'static str,
}

const GOLDEN: [GoldenRow; 3] = [
    GoldenRow {
        name: "mainnet",
        network_id: 1,
        p_genesis_block_id: "UUvXi6j7QhVvgpbKM89MP5HdrxKm9CaJeHc187TsDNf8nZdLk",
        x_blockchain_id: "2oYMBNV4eNHyqk2fjjV5nVQLDbtmNJzq5s3qs3Lo6ftnC6FByM",
        c_blockchain_id: "2q9e4r6Mu3U68nU1fYjgbR6JvwrRx36CohpAX5UQxse55x1Q5",
        avax_asset_id: "FvwEAhmxKfeiG8SnEvq42hc6whRyY3EFYAvebMqDNDGCgxN5Z",
    },
    GoldenRow {
        name: "fuji",
        network_id: 5,
        p_genesis_block_id: "MSj6o9TpezwsQx4Tv7SHqpVvCbJ8of1ikjsqPZ1bKRjc9zBy3",
        x_blockchain_id: "2JVSBoinj9C2J33VntvzYtVJNZdN2NKiwwKjcumHUWEb5DbBrm",
        c_blockchain_id: "yH8D7ThNJkxmtkuv2jgBa4P1Rn3Qpr4pPr7QYNfcdoS6k6HWp",
        avax_asset_id: "U8iRqJoiJm8xZHAacmvYyZVwqQx6uDNtQeP3CQ6fcgQk3JqnK",
    },
    GoldenRow {
        name: "local_unmodified",
        network_id: 12345,
        p_genesis_block_id: "2nRRoR76HuEk1JjDpRdN8FKvZFvUXWxY3b9C5rZRPFjcgEh7S7",
        x_blockchain_id: "2eNy1mUFdmaxXNj1eQHUe7Np4gju9sJsEtWQ4MX3ToiNKuADed",
        c_blockchain_id: "2owdGqyG6FFzTHy5qhenDXQcEghvr571KZE3gSfRJERSJinuwC",
        avax_asset_id: "2fombhL7aGPwj3KH4bfrmJwW6PVnMobf9Y2fn9GwxiAAJyFDbe",
    },
];

/// The custom `genesis_test.json` (networkID 9999) — committed verbatim from
/// the Go tree; its expected P-chain bytes hash is the `TestGenesisFromFile`
/// golden.
const CUSTOM_EXPECTED_HASH_HEX: &str =
    "a1d1838586db85fe94ab1143560c3356df9ba2445794b796bba050be89f4fcb4";

fn custom_config() -> Config {
    let json = include_str!("vectors/genesis/genesis_test.json");
    let unparsed: ava_genesis::unparsed::UnparsedConfig =
        serde_json::from_str(json).expect("parse genesis_test.json");
    let config = unparsed.parse().expect("parse custom config");
    assert_eq!(config.network_id, 9999, "genesis_test.json network id");
    config
}

fn go_dump(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/vectors/genesis/p_chain_bytes_{name}.bin",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing Go dump {path}: {e}"))
}

/// M8.8 per-PR exit gate (`golden::genesis_block_id`): the P-chain genesis
/// block id, the X/C blockchain ids, and the AVAX asset id for Mainnet, Fuji,
/// Local-unmodified, and custom(9999) byte-match the Go golden tables
/// (specs 23 §7).
#[test]
fn genesis_block_id() {
    for row in &GOLDEN {
        // Mainnet/Fuji resolve through the public genesis_bytes/get_config
        // path. Local's P id is computed over the **unmodified** local config
        // (specs 23 §5.1 quirk: the live local config advances startTime, so
        // its P genesis id is time-dependent — exactly as in Go, whose
        // TestGenesis pins unmodifiedLocalConfig).
        let p_id = if row.network_id == 12345 {
            let (p_bytes, _) = from_config(&UNMODIFIED_LOCAL_CONFIG).expect("local build");
            ava_platformvm::genesis::genesis_id(&p_bytes)
        } else {
            ava_genesis::genesis_block_id(row.network_id, Chain::P).expect(row.name)
        };
        assert_eq!(p_id.to_string(), row.p_genesis_block_id, "{} P", row.name);

        assert_eq!(
            ava_genesis::genesis_block_id(row.network_id, Chain::X)
                .expect(row.name)
                .to_string(),
            row.x_blockchain_id,
            "{} X",
            row.name
        );
        assert_eq!(
            ava_genesis::genesis_block_id(row.network_id, Chain::C)
                .expect(row.name)
                .to_string(),
            row.c_blockchain_id,
            "{} C",
            row.name
        );

        let (_p_bytes, asset_id) = genesis_bytes(row.network_id, None).expect(row.name);
        assert_eq!(asset_id.to_string(), row.avax_asset_id, "{} AVAX", row.name);
    }

    // The genesis_block_id(P) identity: hash of the P-chain genesis bytes.
    let (p_bytes, _) = genesis_bytes(1, None).expect("mainnet bytes");
    assert_eq!(
        ava_genesis::genesis_block_id(1, Chain::P)
            .expect("mainnet P")
            .to_string(),
        ava_platformvm::genesis::genesis_id(&p_bytes).to_string()
    );

    // Custom (networkID 9999): hex(sha256(p_bytes)) matches TestGenesisFromFile.
    let custom = custom_config();
    let (custom_bytes, _asset_id) = from_config(&custom).expect("custom build");
    assert_eq!(
        hex::encode(ava_crypto::hashing::sha256(&custom_bytes)),
        CUSTOM_EXPECTED_HASH_HEX,
        "custom-config hash"
    );
}

/// The **full byte stream** parity check (specs 23 §9.2): `from_config` output
/// is byte-identical to the committed Go `genesis.FromConfig` dumps — this
/// guards every intermediate ordering (X-alloc sort, validator end-time heap,
/// reward-addr sort, chain order), not just the final hashes.
#[test]
fn genesis_p_chain_bytes_byte_identical() {
    let custom = custom_config();
    let cases: [(&str, &Config); 4] = [
        ("mainnet", &MAINNET_CONFIG),
        ("fuji", &FUJI_CONFIG),
        ("local_unmodified", &UNMODIFIED_LOCAL_CONFIG),
        ("custom_9999", &custom),
    ];
    for (name, config) in cases {
        let (got, _) = from_config(config).expect(name);
        let want = go_dump(name);
        assert_eq!(got.len(), want.len(), "{name}: length mismatch");
        if got != want {
            let first_diff = got
                .iter()
                .zip(want.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(got.len());
            panic!("{name}: byte stream diverges at offset {first_diff}");
        }
    }
}

/// M9.15 rung-4 live-parity pin: the **advanced** local config (startTime
/// moved to the most recent 9-month chunk, as the live node computes it via
/// `get_recent_start_time`) produces exactly the P-chain genesis the Go oracle
/// (`avalanchego@96897293a2`) ran with in mixed-net run 7 (2026-07-15):
/// every Go validator's P `initializing last accepted` blkID is
/// `2Z95KpsdZCEfwFHYybU6wyn9s3fbYnn92ZtnBnPWZEAtQMjqnq` (go1/logs/P.log), and
/// the live Rust follower's P preference matched it on the wire.
///
/// `NOW` is frozen inside the chunk `[2026-01-06, 2026-10-03)` so the test is
/// deterministic forever; when Go's live chunk advances past 2026-10-03 the
/// live id changes with it on BOTH sides (same `getRecentStartTime`), and this
/// pin keeps guarding the frozen chunk.
#[test]
fn genesis_p_chain_advanced_local_matches_live_go() {
    /// 2026-07-15 00:00:00 UTC — inside the run-7 chunk.
    const NOW: u64 = 1_784_073_600;
    /// The chunk start `getRecentStartTime` selects for `NOW`
    /// (2026-01-06 04:00:00 UTC).
    const CHUNK_START: u64 = 1_767_672_000;

    let mut config = UNMODIFIED_LOCAL_CONFIG.clone();
    config.start_time = ava_genesis::recent_start::get_recent_start_time(
        config.start_time,
        NOW,
        ava_genesis::recent_start::LOCAL_NETWORK_UPDATE_START_TIME_PERIOD_SECS,
    );
    assert_eq!(config.start_time, CHUNK_START, "advanced startTime chunk");

    let (p_bytes, _) = from_config(&config).expect("advanced local build");
    // sha256(p_bytes) — Go probe `genesis.FromConfig(GetConfig(local))` prints
    // the identical hash for this chunk.
    assert_eq!(
        hex::encode(ava_crypto::hashing::sha256(&p_bytes)),
        "5c23e336aa6dc99a5e5424d005a439193f25b434fb62d815dc3c224e132c4310",
        "live-local P genesis bytes hash vs Go oracle (96897293a2)"
    );
    // The height-0 ApricotCommit genesis **block** id (the network's live P
    // last-accepted id — what peers gossip on the wire).
    assert_eq!(
        ava_platformvm::genesis::genesis_block(&p_bytes)
            .expect("genesis block")
            .id()
            .to_string(),
        "2Z95KpsdZCEfwFHYybU6wyn9s3fbYnn92ZtnBnPWZEAtQMjqnq",
        "live-local P genesis block id vs Go oracle (96897293a2)"
    );
}

/// `vm_genesis` finds the CreateChainTx per VM id and errors on unknown ids.
#[test]
fn vm_genesis_unknown_vm() {
    let (p_bytes, _) = genesis_bytes(12345, None).expect("local bytes");
    let x = vm_genesis(&p_bytes, chains::avm_id()).expect("X chain tx");
    assert_eq!(
        x.id().to_string(),
        "2eNy1mUFdmaxXNj1eQHUe7Np4gju9sJsEtWQ4MX3ToiNKuADed"
    );
    assert!(vm_genesis(&p_bytes, chains::platform_vm_id()).is_err());
}

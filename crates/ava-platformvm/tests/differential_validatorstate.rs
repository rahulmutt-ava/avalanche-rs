// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M4.23 `differential::validatorstate_parity` — the diff-window reconstruction
//! parity gate (specs 08 §7, §7.1, §11.4; 02 §11 recorded-oracle; 00 §6.1).
//!
//! Replays a recorded sequence of P-Chain blocks (each carrying staker
//! add/remove mutations) and, for **every** height, asserts the M4.21
//! [`PChainValidatorManager`]'s reconstructed validator-set view (weights + BLS
//! keys, `NodeId`-ascending) and warp-set view equal a recorded oracle snapshot.
//!
//! ## Why this is a real cross-check (non-circular)
//!
//! The recorded vectors are produced by a **forward** oracle: start from an
//! empty set and accumulate per-block add/remove weights+keys as each block is
//! applied (the `record_forward_oracle` generator, run behind
//! `GENERATE_VALIDATOR_DIFF_WINDOWS=1`). The manager answers the same queries by
//! the **opposite** code path — it holds the *current* (tip) set and un-applies
//! the persisted per-height weight/pk diffs **backward** over `(target, current]`
//! (`makeValidatorSet`). Forward-accumulation vs backward-reconstruction
//! agreement at every historical height is therefore a genuine bug catcher, not a
//! tautology.
//!
//! ## Deferred Go-extracted golden
//!
//! There is currently no Go extraction harness for P-Chain validator diff windows
//! (`tools/extract-vectors` does not dump this surface). Following the M4.24
//! genesis precedent, the byte-exact Go golden is recorded as a deferred row in
//! `tests/PORTING.md`; the recorded-oracle vectors here deliver the §11.4 parity
//! guarantee honestly today and leave a clean seam for Go vectors later.

// This suite exercises only the validator-manager reconstruction path; the
// dev-deps declared for the codec/reward/state suites are unused here.
#![allow(unused_crate_dependencies)]
// Test-fixture arithmetic on known-small bounds is clearer than checked math.
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation)]

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use ava_crypto::bls::{PublicKey, SecretKey};
use ava_database::MemDb;
use ava_platformvm::state::chain::Chain;
use ava_platformvm::state::disk_staker_diff_iterator::ValidatorWeightDiff;
use ava_platformvm::state::staker::Staker;
use ava_platformvm::state::state::State;
use ava_platformvm::txs::Priority;
use ava_platformvm::validators::manager::PChainValidatorManager;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::ValidatorState;
use serde::{Deserialize, Serialize};

const AVAX: u64 = 1_000_000_000;

/// The recorded validator-diff-window vectors for one scenario.
#[derive(Debug, Serialize, Deserialize)]
struct Vectors {
    /// Human description of the scenario (and how the oracle is built).
    description: String,
    /// The subnet whose validator set is reconstructed (hex `Id`, 32 bytes). The
    /// all-zero id is the Primary Network.
    subnet: String,
    /// The recorded blocks, in acceptance order (height 1..=N).
    blocks: Vec<BlockVec>,
    /// The forward-oracle snapshot at every height 0..=N (index == height).
    snapshots: Vec<Snapshot>,
}

/// One recorded block: the staker mutations its on-accept diff applies.
#[derive(Debug, Serialize, Deserialize)]
struct BlockVec {
    height: u64,
    mutations: Vec<MutationVec>,
}

/// A single staker mutation in a block.
#[derive(Debug, Serialize, Deserialize)]
struct MutationVec {
    /// `"add"` or `"remove"`.
    op: String,
    /// The staker's transaction id seed byte (`Id::from([tx; 32])`).
    tx: u8,
    /// The node id (hex, 20 bytes).
    node_id: String,
    /// The compressed BLS key (hex, 48 bytes), or empty for no key.
    bls_key: String,
    /// The staker weight.
    weight: u64,
}

/// The recorded validator-set + warp-set view at one height.
#[derive(Debug, Serialize, Deserialize)]
struct Snapshot {
    height: u64,
    /// The validator set: node id (hex) → (weight, compressed BLS key hex or ""),
    /// `NodeId`-ascending.
    validators: Vec<ValidatorVec>,
    /// The flattened warp-set total weight for `subnet`.
    warp_total_weight: u64,
    /// The flattened warp-set per-key entries (compressed BLS key hex), sorted by
    /// key bytes.
    warp_entries: Vec<WarpEntryVec>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ValidatorVec {
    node_id: String,
    weight: u64,
    /// Compressed BLS key hex, or "" if the node has no key.
    bls_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WarpEntryVec {
    bls_key: String,
    weight: u64,
}

fn unix(secs: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// Derives a deterministic BLS public key from a seed byte.
fn pk(seed: u8) -> PublicKey {
    SecretKey::from_bytes(&[seed; 32]).expect("sk").public_key()
}

fn genesis_state() -> State<MemDb> {
    let mut s = State::new(MemDb::new()).expect("state");
    s.set_timestamp(unix(1_000));
    s.set_current_supply(Id::EMPTY, 100_000_000 * AVAX);
    s.set_last_accepted(Id::from([0xAB; 32]));
    s.set_height(0);
    s
}

/// Builds a current primary-network validator staker (BLS key optional).
fn make_staker(tx: u8, node: NodeId, key: Option<PublicKey>, weight: u64) -> Staker {
    Staker::new_current(
        Id::from([tx; 32]),
        node,
        key,
        Id::EMPTY,
        weight,
        unix(1_000),
        unix(9_000),
        0,
        Priority::PrimaryNetworkValidatorCurrent,
    )
}

/// Writes the per-height staker weight + public-key diffs into `state`'s diff
/// stores, exactly as `BlockManager::write_validator_diffs` does on accept (Go
/// `writeValidatorDiffs`): a weight increase is `decrease = false`, a decrease
/// `decrease = true`; a pk diff stores the key the node *had before*.
///
/// This is the production write path the manager reads back; the test feeds it
/// the `before`/`after` snapshots it takes around applying each block's
/// mutations, so the persisted diffs match what an accepted block would write.
fn write_validator_diffs(
    state: &State<MemDb>,
    height: u64,
    weights_before: &BTreeMap<(Id, NodeId), u64>,
    weights_after: &BTreeMap<(Id, NodeId), u64>,
    keys_before: &BTreeMap<(Id, NodeId), Vec<u8>>,
    keys_after: &BTreeMap<(Id, NodeId), Vec<u8>>,
) {
    let weight_store = state.weight_diff_store();
    let pk_store = state.public_key_diff_store();

    let mut touched: std::collections::BTreeSet<(Id, NodeId)> = std::collections::BTreeSet::new();
    touched.extend(weights_before.keys().copied());
    touched.extend(weights_after.keys().copied());
    touched.extend(keys_before.keys().copied());
    touched.extend(keys_after.keys().copied());

    for (subnet, node) in touched {
        let before = weights_before.get(&(subnet, node)).copied().unwrap_or(0);
        let after = weights_after.get(&(subnet, node)).copied().unwrap_or(0);
        if before != after {
            let diff = if after > before {
                ValidatorWeightDiff {
                    decrease: false,
                    amount: after - before,
                }
            } else {
                ValidatorWeightDiff {
                    decrease: true,
                    amount: before - after,
                }
            };
            weight_store
                .put(subnet, node, height, &diff)
                .expect("weight diff");
        }
        let prev = keys_before.get(&(subnet, node));
        let new = keys_after.get(&(subnet, node));
        if prev != new {
            let prev_bytes = prev.map_or(&[][..], Vec::as_slice);
            pk_store
                .put(subnet, node, height, prev_bytes)
                .expect("pk diff");
        }
    }
}

/// Applies one recorded block's mutations to `state`, snapshotting the
/// validator-set weights/keys before and after to persist the per-height diffs,
/// then advances the height / last-accepted singletons (the accept tail).
fn apply_block(state: &mut State<MemDb>, blk: &BlockVec) {
    let weights_before = state.current_validator_weights();
    let keys_before = state.current_validator_public_keys();

    for m in &blk.mutations {
        let node = node_from_hex(&m.node_id);
        let key = pk_from_hex(&m.bls_key);
        let staker = make_staker(m.tx, node, key, m.weight);
        match m.op.as_str() {
            "add" => state.put_current_validator(staker).expect("add"),
            "remove" => state.delete_current_validator(&staker),
            other => panic!("unknown op {other}"),
        }
    }

    let weights_after = state.current_validator_weights();
    let keys_after = state.current_validator_public_keys();
    write_validator_diffs(
        state,
        blk.height,
        &weights_before,
        &weights_after,
        &keys_before,
        &keys_after,
    );

    let block_id = Id::from([(blk.height as u8) ^ 0xC0; 32]);
    state.add_block(block_id, blk.height, &[blk.height as u8]);
    state.set_last_accepted(block_id);
    state.set_height(blk.height);
}

fn node_from_hex(s: &str) -> NodeId {
    NodeId::from_slice(&hex_to_bytes(s)).expect("node id")
}

fn pk_from_hex(s: &str) -> Option<PublicKey> {
    if s.is_empty() {
        return None;
    }
    Some(PublicKey::from_compressed(&hex_to_bytes(s)).expect("compressed pk"))
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn bytes_to_hex(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 2);
    for byte in b {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

mod differential {
    use super::*;

    /// Loads the recorded scenarios and asserts the M4.21 manager's backward
    /// diff-window reconstruction matches the forward-oracle snapshots at every
    /// height (validators: weights + BLS keys, `NodeId`-ascending) and that the
    /// warp set matches at every height.
    #[tokio::test]
    async fn validatorstate_parity() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/platformvm/validator_diff_windows"
        );
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("validator_diff_windows vectors dir must exist")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        entries.sort();
        assert!(
            !entries.is_empty(),
            "no validator_diff_windows vectors found"
        );

        for path in entries {
            let raw = std::fs::read_to_string(&path).expect("read vector");
            let vec: Vectors = serde_json::from_str(&raw).expect("parse vector");
            replay_and_assert(&vec, &path.display().to_string()).await;
        }
    }

    async fn replay_and_assert(vectors: &Vectors, name: &str) {
        assert!(!vectors.description.is_empty(), "{name}: description");
        let subnet = Id::from_slice(&hex_to_bytes(&vectors.subnet)).expect("subnet id");

        let mut state = genesis_state();
        let mgr = PChainValidatorManager::from_state(&state, false);

        for (i, blk) in vectors.blocks.iter().enumerate() {
            assert_eq!(blk.height, (i + 1) as u64, "{name}: block height order");
            apply_block(&mut state, blk);
            mgr.refresh(&state);
        }

        let n = vectors.blocks.len() as u64;
        assert_eq!(
            mgr.get_current_height().await.unwrap(),
            n,
            "{name}: current height"
        );
        assert_eq!(
            vectors.snapshots.len() as u64,
            n + 1,
            "{name}: one snapshot per height 0..=N"
        );

        for snap in &vectors.snapshots {
            assert!(snap.height <= n, "{name}: snapshot height in range");

            // Validator-set parity (weights + BLS keys), order-independent.
            let got = mgr
                .get_validator_set(snap.height, subnet)
                .await
                .unwrap_or_else(|e| panic!("{name}: get_validator_set({}) {e:?}", snap.height));
            let got_proj: Vec<ValidatorVec> = got
                .iter()
                .map(|(node, out)| ValidatorVec {
                    node_id: bytes_to_hex(node.as_bytes()),
                    weight: out.weight,
                    bls_key: out
                        .public_key
                        .as_ref()
                        .map(|k| bytes_to_hex(&k.compress()))
                        .unwrap_or_default(),
                })
                .collect();
            assert_eq!(
                projection_pairs(&got_proj),
                projection_pairs(&snap.validators),
                "{name}: validator-set mismatch at height {}",
                snap.height
            );

            // Warp-set parity for `subnet`.
            let warp = mgr
                .get_warp_validator_sets(snap.height)
                .await
                .unwrap_or_else(|e| {
                    panic!("{name}: get_warp_validator_sets({}) {e:?}", snap.height)
                });
            let (got_total, got_entries) = match warp.get(&subnet) {
                Some(ws) => {
                    let mut entries: Vec<(String, u64)> = ws
                        .validators
                        .iter()
                        .map(|v| {
                            (
                                v.public_key
                                    .as_ref()
                                    .map(|k| bytes_to_hex(&k.compress()))
                                    .unwrap_or_default(),
                                v.weight,
                            )
                        })
                        .collect();
                    entries.sort();
                    (ws.total_weight, entries)
                }
                None => (0, Vec::new()),
            };
            let mut exp_entries: Vec<(String, u64)> = snap
                .warp_entries
                .iter()
                .map(|e| (e.bls_key.clone(), e.weight))
                .collect();
            exp_entries.sort();
            assert_eq!(
                got_total, snap.warp_total_weight,
                "{name}: warp total weight mismatch at height {}",
                snap.height
            );
            assert_eq!(
                got_entries, exp_entries,
                "{name}: warp entries mismatch at height {}",
                snap.height
            );
        }
    }

    /// Normalizes a validator projection to a sorted `(node_id, weight, key)`
    /// tuple list for order-independent comparison (the manager already returns
    /// `NodeId`-ascending; this guards the vectors regardless of their order).
    fn projection_pairs(v: &[ValidatorVec]) -> Vec<(String, u64, String)> {
        let mut out: Vec<(String, u64, String)> = v
            .iter()
            .map(|e| (e.node_id.clone(), e.weight, e.bls_key.clone()))
            .collect();
        out.sort();
        out
    }
}

/// The forward-oracle vector generator (the INDEPENDENT code path).
///
/// Gated behind `GENERATE_VALIDATOR_DIFF_WINDOWS=1` so it never runs in CI; when
/// set it builds each scenario's block list, forward-accumulates the validator
/// set (start empty; add/remove weights + keys per block), and writes the
/// per-height snapshots + warp sets to the `validator_diff_windows/*.json`
/// vectors. The committed vectors are the output of this generator.
#[cfg(test)]
mod gen_vectors {
    use super::*;

    /// A scenario: a name and its ordered block list.
    struct Scenario {
        file: &'static str,
        description: &'static str,
        blocks: Vec<BlockVec>,
    }

    fn mutation(
        op: &str,
        tx: u8,
        node: NodeId,
        key: Option<PublicKey>,
        weight: u64,
    ) -> MutationVec {
        MutationVec {
            op: op.to_string(),
            tx,
            node_id: bytes_to_hex(node.as_bytes()),
            bls_key: key.map(|k| bytes_to_hex(&k.compress())).unwrap_or_default(),
            weight,
        }
    }

    /// Forward-accumulates the validator set across `blocks` and produces the
    /// per-height snapshots (including height 0 = empty) using a flatten-by-key
    /// warp computation independent of the manager.
    fn forward_oracle(blocks: &[BlockVec]) -> Vec<Snapshot> {
        // node -> (weight, compressed-key-hex or "").
        let mut set: BTreeMap<String, (u64, String)> = BTreeMap::new();
        let mut snaps = vec![snapshot_of(0, &set)];
        for blk in blocks {
            for m in &blk.mutations {
                match m.op.as_str() {
                    "add" => {
                        set.insert(m.node_id.clone(), (m.weight, m.bls_key.clone()));
                    }
                    "remove" => {
                        set.remove(&m.node_id);
                    }
                    other => panic!("unknown op {other}"),
                }
            }
            snaps.push(snapshot_of(blk.height, &set));
        }
        snaps
    }

    /// Builds a `Snapshot` from a forward-accumulated set (validators sorted by
    /// node-id hex; warp set deduped + summed by key, sorted by key hex).
    fn snapshot_of(height: u64, set: &BTreeMap<String, (u64, String)>) -> Snapshot {
        let mut validators: Vec<ValidatorVec> = set
            .iter()
            .map(|(node, (w, k))| ValidatorVec {
                node_id: node.clone(),
                weight: *w,
                bls_key: k.clone(),
            })
            .collect();
        validators.sort_by(|a, b| a.node_id.cmp(&b.node_id));

        // Flatten by BLS key: sum weights of nodes sharing a key; nodes with no
        // key are dropped from warp entries but still count toward total weight.
        let mut by_key: BTreeMap<String, u64> = BTreeMap::new();
        let mut total: u64 = 0;
        for (w, k) in set.values() {
            total += *w;
            if k.is_empty() {
                continue;
            }
            *by_key.entry(k.clone()).or_insert(0) += *w;
        }
        let mut warp_entries: Vec<WarpEntryVec> = by_key
            .into_iter()
            .map(|(bls_key, weight)| WarpEntryVec { bls_key, weight })
            .collect();
        warp_entries.sort_by(|a, b| a.bls_key.cmp(&b.bls_key));

        Snapshot {
            height,
            validators,
            warp_total_weight: total,
            warp_entries,
        }
    }

    fn scenarios() -> Vec<Scenario> {
        let node_a = NodeId::from([0x0A; 20]);
        let node_b = NodeId::from([0x0B; 20]);
        let node_c = NodeId::from([0x0C; 20]);
        let key_a = pk(0x11);
        let key_b = pk(0x22);
        let key_c = pk(0x33);
        let shared = pk(0x44);
        let wa = 1_000 * AVAX;
        let wb = 2_000 * AVAX;
        let wc = 3_000 * AVAX;

        vec![
            // Basic add/add/remove across three heights.
            Scenario {
                file: "primary_add_remove.json",
                description: "Primary-network add A, add B, remove A across three \
                              heights; forward oracle vs backward diff reconstruction.",
                blocks: vec![
                    BlockVec {
                        height: 1,
                        mutations: vec![mutation("add", 1, node_a, Some(key_a.clone()), wa)],
                    },
                    BlockVec {
                        height: 2,
                        mutations: vec![mutation("add", 2, node_b, Some(key_b.clone()), wb)],
                    },
                    BlockVec {
                        height: 3,
                        mutations: vec![mutation("remove", 1, node_a, Some(key_a.clone()), wa)],
                    },
                ],
            },
            // Multiple mutations per block + a shared BLS key (warp dedup).
            Scenario {
                file: "shared_key_and_churn.json",
                description: "Multi-mutation blocks with two nodes sharing a BLS key \
                              (warp dedup + summed weight), then churn removing one.",
                blocks: vec![
                    BlockVec {
                        height: 1,
                        mutations: vec![
                            mutation("add", 1, node_a, Some(shared.clone()), wa),
                            mutation("add", 2, node_b, Some(shared.clone()), wb),
                            mutation("add", 3, node_c, Some(key_c.clone()), wc),
                        ],
                    },
                    BlockVec {
                        height: 2,
                        mutations: vec![mutation("remove", 2, node_b, Some(shared.clone()), wb)],
                    },
                    BlockVec {
                        height: 3,
                        mutations: vec![
                            mutation("remove", 3, node_c, Some(key_c.clone()), wc),
                            mutation("add", 4, node_b, Some(key_b.clone()), wb),
                        ],
                    },
                    BlockVec {
                        height: 4,
                        mutations: vec![],
                    },
                ],
            },
        ]
    }

    #[test]
    fn generate() {
        if std::env::var("GENERATE_VALIDATOR_DIFF_WINDOWS").is_err() {
            return;
        }
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/vectors/platformvm/validator_diff_windows"
        );
        std::fs::create_dir_all(dir).expect("mkdir vectors");
        for sc in scenarios() {
            let snapshots = forward_oracle(&sc.blocks);
            let vectors = Vectors {
                description: sc.description.to_string(),
                subnet: bytes_to_hex(Id::EMPTY.as_bytes()),
                blocks: sc.blocks,
                snapshots,
            };
            let json = serde_json::to_string_pretty(&vectors).expect("serialize");
            let path = format!("{dir}/{}", sc.file);
            std::fs::write(&path, json + "\n").expect("write vector");
            eprintln!("wrote {path}");
        }
    }
}

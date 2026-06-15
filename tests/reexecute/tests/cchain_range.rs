// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `reexecute_cchain_range` — the M9.19 C-Chain reexecute oracle (specs/02
//! §10.5/§11.1, specs/16 §5(3), specs/00 §11.7).
//!
//! Replays the committed `genesis_to_1` `blockexport`-style fixture (Go-EXECUTED
//! against coreth) through the Rust reth `BlockExecutor` + Firewood-ethhash
//! pipeline via [`ava_reexecute::replay_cchain`] and asserts the computed genesis
//! and post-block-1 state roots match the Go-recorded values byte-for-byte. One
//! block proves the recorded-oracle / reexecute path end-to-end (the cheapest
//! per-PR differential oracle).

// The reexecute deps (ava-evm/evm-reth/database, hex/serde*/tempfile/thiserror)
// are consumed by the `ava_reexecute` lib, not directly by this integration
// target; per the established `unused_crate_dependencies` idiom each integration
// test that does not name them directly opts out of the per-binary false positive.
#![allow(unused_crate_dependencies)]

use ava_reexecute::{ReexecuteCase, replay_cchain};

/// The committed C-Chain reexecute fixture (Go-EXECUTED against coreth; see the
/// adjacent `manifest.json` for provenance — avalanchego @fb174e8, go1.25.9).
const GENESIS_TO_1: &str = include_str!("../vectors/cchain/genesis_to_1/genesis_to_1.json");

#[test]
fn reexecute_cchain_range() {
    let case = ReexecuteCase::from_json(GENESIS_TO_1).expect("parse genesis_to_1 fixture");

    let roots = replay_cchain(&case).expect("replay cchain genesis_to_1 range");

    assert_eq!(
        roots.genesis,
        case.expected_genesis_root().expect("expected genesis root"),
        "genesis state root parity vs coreth (recorded oracle)"
    );
    assert_eq!(
        roots.post,
        case.expected_post_root().expect("expected post root"),
        "post-block-1 state root parity vs coreth (recorded oracle)"
    );
}

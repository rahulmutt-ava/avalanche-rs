// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fresh-network **stop vertex** identity (M9.15 rung 4; Go
//! `snow/engine/avalanche/vertex/builder.go::BuildStopVertex` +
//! `snow/engine/avalanche/bootstrap/bootstrapper.go`).
//!
//! When a network's `Upgrades.CortinaXChainStopVertexID` is **empty**
//! (local/custom networks created after the Cortina linearization), Go does not
//! linearize the X-Chain off the empty id. Its avalanche bootstrapper treats
//! the current DAG state as final and builds a stop vertex over the DAG edge —
//! which on a freshly-created chain is **empty** — then linearizes off that
//! vertex's id:
//!
//! ```text
//! // If a stop vertex isn't well known, treat the current state as the final
//! // DAG state.  (bootstrapper.go)
//! edge := b.Manager.Edge(ctx)             // [] on a fresh chain
//! stopVertex := b.Manager.BuildStopVtx(ctx, edge)
//! ```
//!
//! `BuildStopVertex(chainID, height=0, parentIDs=[])` marshals the stateless
//! vertex with codec version 1 (`CodecVersionWithStopVtx`); the `serializeV1`
//! field set is `ChainID`, `Height`, `ParentIDs` (no `Epoch`/`Txs`), so the
//! byte layout is exactly:
//!
//! ```text
//! [codec version u16 = 1] [chainID 32B] [height u64 = 0] [parentIDs len u32 = 0]
//! ```
//!
//! and the vertex id is `sha256` of those 46 bytes. The height-0 X genesis
//! `StandardBlock` then uses this id as its parent
//! (`state.InitializeChainState`), which fixes the genesis Snowman block id a
//! Rust node must byte-match to follow a Go local network.
//!
//! Mainnet/Fuji pin a **non-empty** well-known stop vertex in the upgrade
//! config, which is used as-is — this module only serves the empty-id branch.

use ava_types::id::Id;

/// `vertex.BuildStopVertex(chain_id, 0, [])` — the stop vertex id Go computes
/// for a fresh (post-linearization-created) X-Chain, e.g. every
/// `network-id=local` network. See the module docs for the byte layout.
#[must_use]
pub fn fresh_stop_vertex_id(chain_id: Id) -> Id {
    // [codec version u16 = 1] [chainID] [height u64 = 0] [parentIDs len u32 = 0]
    let mut bytes = Vec::with_capacity(2 + 32 + 8 + 4);
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(chain_id.as_bytes());
    bytes.extend_from_slice(&0u64.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    Id::from(ava_crypto::hashing::sha256(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden: the local network's fresh stop vertex over the pinned local X
    /// blockchain id `2eNy1mUFd…` (specs 23 §7). Value verified against the Go
    /// oracle `avalanchego@96897293a2`: linearizing off this id yields the
    /// live network's X genesis Snowman block
    /// `2R2UY2pZMQr8nR9ywCdqn97Lp5a6hceqtLXkag6vH7KQSVvmst` (mixed-net run-7
    /// go1/logs/X.log `starting bootstrapper`; re-confirmed on a solo node) —
    /// the end-to-end pin lives in `tests/golden_x_genesis_block.rs`.
    #[test]
    fn local_fresh_stop_vertex_id_golden() {
        let x_chain_id: Id = "2eNy1mUFdmaxXNj1eQHUe7Np4gju9sJsEtWQ4MX3ToiNKuADed"
            .parse()
            .expect("cb58");
        assert_eq!(
            fresh_stop_vertex_id(x_chain_id).to_string(),
            "3LdUD428gni3AFYAooqcTiN3xPGiLE7XdHowi9CsbNGH1i7QY",
            "vertex.BuildStopVertex(localXChainID, 0, []) id"
        );
    }
}

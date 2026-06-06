// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PeerSet` — the connecting / connected peer bookkeeping (`specs/05` §3.1).
//!
//! Mirrors Go's `peerData` maps in `network/network.go`. The network holds two
//! sets: peers mid-handshake (`connecting`) and peers that finished it
//! (`connected`). Lock-free reads are not strictly required for the handshake
//! milestone, so a single `parking_lot::Mutex<HashMap>` per set is used (the
//! documented lock order — `peers` before `manually_tracked_ids` — is preserved
//! by the network never holding both at once across an `.await`).

use std::collections::HashMap;

use ava_types::node_id::NodeId;
use parking_lot::Mutex;

use crate::peer::handle::PeerHandle;

/// A set of peer handles keyed by NodeID.
#[derive(Default)]
pub struct PeerSet {
    peers: Mutex<HashMap<NodeId, PeerHandle>>,
}

impl PeerSet {
    /// A fresh, empty set.
    #[must_use]
    pub fn new() -> PeerSet {
        PeerSet::default()
    }

    /// Insert (or replace) a peer handle.
    pub fn insert(&self, handle: PeerHandle) {
        self.peers.lock().insert(handle.node_id(), handle);
    }

    /// Remove a peer by NodeID, returning its handle if present.
    pub fn remove(&self, node: &NodeId) -> Option<PeerHandle> {
        self.peers.lock().remove(node)
    }

    /// Whether the set contains `node`.
    #[must_use]
    pub fn contains(&self, node: &NodeId) -> bool {
        self.peers.lock().contains_key(node)
    }

    /// The NodeIDs currently in the set.
    #[must_use]
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.peers.lock().keys().copied().collect()
    }

    /// A cloned handle for `node`, if present.
    #[must_use]
    pub fn get(&self, node: &NodeId) -> Option<PeerHandle> {
        self.peers.lock().get(node).cloned()
    }

    /// All current handles (cloned).
    #[must_use]
    pub fn handles(&self) -> Vec<PeerHandle> {
        self.peers.lock().values().cloned().collect()
    }

    /// Number of peers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.lock().len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.lock().is_empty()
    }

    /// Close every peer (used during shutdown).
    pub fn close_all(&self) {
        for handle in self.peers.lock().values() {
            handle.close();
        }
    }
}

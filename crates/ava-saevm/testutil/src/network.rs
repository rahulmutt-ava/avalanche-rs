// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! An in-memory multi-node gossip network harness (specs/11 §3 testutil delta;
//! Go `vms/saevm/saetest/network.go`).
//!
//! Go's `saetest.Sender` is an in-memory `common.AppSender` that routes
//! `AppRequest`/`AppResponse`/`AppGossip` between registered peers (each on its
//! own goroutine), with `Connect`/`ConnectTo` wiring full-mesh / star topologies
//! and validator-aware gossip sampling.
//!
//! # AS-BUILT deviations
//!
//! `ava-network` has no live `AppSender` / generic gossip framework yet (M8), so
//! this harness routes at the **gossip-set level** rather than over the P2P wire:
//! a [`Network`] connects [`GossipNode`]s and delivers push/pull gossip between
//! the topology's connected pairs. This reproduces Go's *observable* multi-node
//! behaviour (a tx issued at node A reaches a connected node B's pool via push;
//! a tx seeded at B reaches A via pull) without a live network. Validator-aware
//! sampling is out of scope (the seen-filter + topology fully determine
//! delivery); document as a `// TODO(M8)` once the real framework lands.
//!
//! The router is deterministic and synchronous (no background tasks), so tests
//! observe delivery immediately after [`Network::push`] / [`Network::pull`].

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_types::id::Id;

/// A node participating in the gossip network — the seam a C-Chain `BloomSet`
/// (+ marshaller) satisfies (Go's `Peer`/`gossip.Set`).
///
/// The harness routes **payloads** (marshalled atomic-tx bytes), so a node need
/// only: snapshot the gossip ids it currently has, snapshot the wire payloads of
/// those txs, report whether it has *seen* a gossip id (the pull filter), and
/// admit an inbound payload. This keeps the harness independent of the concrete
/// tx type.
pub trait GossipNode: Send + Sync {
    /// The gossip ids of the txs currently in this node's pool.
    fn have_ids(&self) -> Vec<Id>;

    /// The wire payloads of every tx currently in this node's pool, paired with
    /// its gossip id.
    fn payloads(&self) -> Vec<(Id, Vec<u8>)>;

    /// Whether this node has ever seen gossip id `id` (the pull membership
    /// filter — Go's bloom containment).
    fn seen(&self, id: Id) -> bool;

    /// Admit an inbound gossip payload (push received, or a pull response).
    /// Malformed payloads are ignored. Returns `true` if a new tx was admitted.
    fn admit(&self, payload: &[u8]) -> bool;
}

/// An in-memory gossip network: a set of [`GossipNode`]s plus the symmetric
/// connection topology between them (Go `saetest.Sender` + `Connect`).
#[derive(Default)]
pub struct Network {
    nodes: BTreeMap<Id, Arc<dyn GossipNode>>,
    /// Symmetric adjacency: `peers[a]` contains `b` iff `a` and `b` are
    /// connected. Held as a sorted map/vec so routing order is deterministic.
    edges: BTreeMap<Id, Vec<Id>>,
}

impl Network {
    /// An empty network.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `node` under `id` (Go `Sender.AddPeer` self-registration).
    pub fn add_node(&mut self, id: Id, node: Arc<dyn GossipNode>) {
        self.nodes.insert(id, node);
        self.edges.entry(id).or_default();
    }

    /// Symmetrically connects `a` and `b` (Go `ConnectTo`). Idempotent.
    pub fn connect(&mut self, a: Id, b: Id) {
        if a == b {
            return;
        }
        Self::add_edge(&mut self.edges, a, b);
        Self::add_edge(&mut self.edges, b, a);
    }

    /// Connects every pair of the given nodes into a full mesh (Go `Connect`).
    pub fn connect_all(&mut self, ids: &[Id]) {
        for (i, &a) in ids.iter().enumerate() {
            for &b in &ids[i.saturating_add(1)..] {
                self.connect(a, b);
            }
        }
    }

    fn add_edge(edges: &mut BTreeMap<Id, Vec<Id>>, from: Id, to: Id) {
        let adj = edges.entry(from).or_default();
        if !adj.contains(&to) {
            adj.push(to);
            adj.sort_unstable();
        }
    }

    /// The peers connected to `id` (deterministic order).
    #[must_use]
    pub fn peers(&self, id: Id) -> &[Id] {
        self.edges.get(&id).map_or(&[], Vec::as_slice)
    }

    /// **Push gossip** from `from`: deliver every payload in `from`'s pool to
    /// each connected peer (Go `pushGossiper` over `AppGossip`). Returns the
    /// number of newly-admitted (peer, tx) deliveries.
    #[must_use]
    pub fn push(&self, from: Id) -> usize {
        let Some(node) = self.nodes.get(&from) else {
            return 0;
        };
        let payloads = node.payloads();
        let mut admitted = 0usize;
        for &peer in self.peers(from) {
            if let Some(peer_node) = self.nodes.get(&peer) {
                for (_id, payload) in &payloads {
                    if peer_node.admit(payload) {
                        admitted = admitted.saturating_add(1);
                    }
                }
            }
        }
        admitted
    }

    /// **Pull gossip** by `puller`: ask each connected peer for txs `puller` has
    /// not seen, and admit them (Go `pullGossiper` over `AppRequest`/
    /// `AppResponse`). Returns the number of newly-admitted txs.
    #[must_use]
    pub fn pull(&self, puller: Id) -> usize {
        let Some(node) = self.nodes.get(&puller) else {
            return 0;
        };
        let mut admitted = 0usize;
        for &peer in self.peers(puller) {
            let Some(peer_node) = self.nodes.get(&peer) else {
                continue;
            };
            for (id, payload) in peer_node.payloads() {
                if !node.seen(id) && node.admit(&payload) {
                    admitted = admitted.saturating_add(1);
                }
            }
        }
        admitted
    }

    /// Runs `rounds` of full network gossip: every node pushes to its peers,
    /// then every node pulls from its peers (Go's `gossip.Every` loops let to
    /// converge). Returns the total newly-admitted deliveries across all rounds.
    /// Repeated rounds let a tx propagate across multi-hop topologies (A→vdrA→
    /// vdrB).
    #[must_use]
    pub fn gossip_rounds(&self, rounds: usize) -> usize {
        let mut total = 0usize;
        let ids: Vec<Id> = self.nodes.keys().copied().collect();
        for _ in 0..rounds {
            for &id in &ids {
                total = total.saturating_add(self.push(id));
            }
            for &id in &ids {
                total = total.saturating_add(self.pull(id));
            }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use parking_lot::Mutex;

    use super::*;

    /// A minimal in-memory node: a `BTreeMap<Id, payload>` pool, where the
    /// payload is the gossip id's bytes (so unmarshal is trivial).
    #[derive(Default)]
    struct FakeNode {
        pool: Mutex<BTreeMap<Id, Vec<u8>>>,
        seen: Mutex<std::collections::BTreeSet<Id>>,
    }

    impl FakeNode {
        fn seed(&self, id: Id) {
            self.pool.lock().insert(id, id.to_bytes().to_vec());
            self.seen.lock().insert(id);
        }
    }

    impl GossipNode for FakeNode {
        fn have_ids(&self) -> Vec<Id> {
            self.pool.lock().keys().copied().collect()
        }
        fn payloads(&self) -> Vec<(Id, Vec<u8>)> {
            self.pool
                .lock()
                .iter()
                .map(|(id, p)| (*id, p.clone()))
                .collect()
        }
        fn seen(&self, id: Id) -> bool {
            self.seen.lock().contains(&id)
        }
        fn admit(&self, payload: &[u8]) -> bool {
            let id = Id::from_slice(payload).expect("32-byte id payload");
            self.seen.lock().insert(id);
            self.pool.lock().insert(id, payload.to_vec()).is_none()
        }
    }

    fn nid(b: u8) -> Id {
        Id::from([b; 32])
    }

    #[test]
    fn push_delivers_to_connected_peer_only() {
        let a = Arc::new(FakeNode::default());
        let b = Arc::new(FakeNode::default());
        let c = Arc::new(FakeNode::default());
        let tx = nid(0x33);
        a.seed(tx);

        let mut net = Network::new();
        net.add_node(nid(0xa1), Arc::clone(&a) as Arc<dyn GossipNode>);
        net.add_node(nid(0xb2), Arc::clone(&b) as Arc<dyn GossipNode>);
        net.add_node(nid(0xc3), Arc::clone(&c) as Arc<dyn GossipNode>);
        net.connect(nid(0xa1), nid(0xb2)); // c is NOT connected to a

        assert_eq!(net.push(nid(0xa1)), 1, "delivered to b only");
        assert!(b.seen(tx), "b received via push");
        assert!(!c.seen(tx), "c (unconnected) did not");
    }

    #[test]
    fn pull_fetches_only_unseen_txs() {
        let a = Arc::new(FakeNode::default());
        let b = Arc::new(FakeNode::default());
        let tx = nid(0x44);
        b.seed(tx);

        let mut net = Network::new();
        net.add_node(nid(0xa1), Arc::clone(&a) as Arc<dyn GossipNode>);
        net.add_node(nid(0xb2), Arc::clone(&b) as Arc<dyn GossipNode>);
        net.connect(nid(0xa1), nid(0xb2));

        assert_eq!(net.pull(nid(0xa1)), 1, "a pulls the unseen tx from b");
        assert!(a.seen(tx));
        // A second pull is a no-op — a has now seen it.
        assert_eq!(net.pull(nid(0xa1)), 0, "already-seen tx is not re-pulled");
    }

    #[test]
    fn gossip_rounds_propagate_multi_hop() {
        // a — vdr_a — vdr_b (a not connected to vdr_b); the tx must hop twice.
        let a = Arc::new(FakeNode::default());
        let vdr_a = Arc::new(FakeNode::default());
        let vdr_b = Arc::new(FakeNode::default());
        let tx = nid(0x55);
        a.seed(tx);

        let mut net = Network::new();
        net.add_node(nid(0x01), Arc::clone(&a) as Arc<dyn GossipNode>);
        net.add_node(nid(0x02), Arc::clone(&vdr_a) as Arc<dyn GossipNode>);
        net.add_node(nid(0x03), Arc::clone(&vdr_b) as Arc<dyn GossipNode>);
        net.connect(nid(0x01), nid(0x02));
        net.connect(nid(0x02), nid(0x03));

        let _ = net.gossip_rounds(2);
        assert!(vdr_b.seen(tx), "tx propagated a -> vdr_a -> vdr_b");
    }

    #[test]
    fn connect_all_builds_full_mesh() {
        let mut net = Network::new();
        let ids = [nid(1), nid(2), nid(3)];
        for &i in &ids {
            net.add_node(i, Arc::new(FakeNode::default()) as Arc<dyn GossipNode>);
        }
        net.connect_all(&ids);
        for &i in &ids {
            assert_eq!(
                net.peers(i).len(),
                2,
                "each node connected to the other two"
            );
        }
    }
}

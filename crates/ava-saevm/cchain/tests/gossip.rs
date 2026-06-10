// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Multi-node cross-chain (atomic) tx gossip over the in-memory
//! `ava-saevm-testutil` network harness (specs/11 §8 upstream-delta; Go
//! `vms/saevm/cchain/gossip_test.go`, `ab442aa244` #5408).
//!
//! Two in-memory-connected C-Chain VMs:
//! * `issued_tx_reaches_peer_pool_via_push_gossip` — `avax.issueTx` on node A
//!   ⇒ the tx appears in node B's atomic txpool via push gossip (Go
//!   `TestPushGossip`).
//! * `seeded_tx_reaches_peer_pool_via_pull_gossip` — a tx pre-seeded on B
//!   reaches A via pull gossip (Go `TestPullGossip`).

#![allow(clippy::arithmetic_side_effects)]

use std::sync::Arc;

use ava_chains::atomic::Memory;
use ava_database::{DynDatabase, MemDb};
use ava_saevm_cchain::gossip::{BloomSet, GossipMarshaller, Gossipable};
use ava_saevm_cchain::tx::components::Input as FxInput;
use ava_saevm_cchain::tx::components::{TransferInput, TransferableInput};
use ava_saevm_cchain::tx::{Credential as TxCredential, Import, Output, Tx, Unsigned};
use ava_saevm_cchain::vm::Vm;
use ava_saevm_testutil::network::{GossipNode, Network};
use ava_secp256k1fx::Credential as SecpCredential;
use ava_types::id::Id;

fn avax_asset_id() -> Id {
    Id::from([0x0a; 32])
}

fn c_chain_id() -> Id {
    Id::from([0xc0; 32])
}

fn id(b: u8) -> Id {
    Id::from([b; 32])
}

fn addr(b: u8) -> [u8; 20] {
    let mut a = [0u8; 20];
    a[0] = b;
    a
}

/// A distinct import tx keyed by `seed` (so different nodes can seed different
/// txs).
fn import_tx(seed: u8) -> Tx {
    let unsigned = Unsigned::Import(Import {
        network_id: 1,
        blockchain_id: c_chain_id(),
        source_chain: id(0x0b),
        imported_ins: vec![TransferableInput {
            tx_id: id(seed),
            output_index: 0,
            asset_id: avax_asset_id(),
            r#in: FxInput::SecpTransfer(TransferInput::new(1_000, vec![0])),
        }],
        outs: vec![Output {
            address: addr(seed),
            amount: 1_000,
            asset_id: avax_asset_id(),
        }],
    });
    Tx {
        unsigned,
        creds: vec![TxCredential::Secp256k1(SecpCredential::new(vec![
            [0u8; 65],
        ]))],
    }
}

fn new_vm() -> Vm {
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let memory = Memory::new(Arc::clone(&base));
    let sm = memory.new_shared_memory(c_chain_id());
    Vm::initialize(&base, Arc::new(sm), c_chain_id(), avax_asset_id()).expect("initialize")
}

/// Adapts a VM's [`BloomSet`] onto the harness [`GossipNode`] seam: marshal /
/// unmarshal atomic txs and admit inbound payloads.
struct NodeAdapter {
    set: Arc<BloomSet>,
    marshaller: GossipMarshaller,
}

impl NodeAdapter {
    fn new(set: Arc<BloomSet>) -> Self {
        Self {
            set,
            marshaller: GossipMarshaller::new(),
        }
    }
}

impl GossipNode for NodeAdapter {
    fn have_ids(&self) -> Vec<Id> {
        self.set
            .snapshot()
            .iter()
            .map(Gossipable::gossip_id)
            .collect()
    }

    fn payloads(&self) -> Vec<(Id, Vec<u8>)> {
        self.set
            .snapshot()
            .iter()
            .filter_map(|tx| {
                self.marshaller
                    .marshal(tx)
                    .ok()
                    .map(|b| (tx.gossip_id(), b))
            })
            .collect()
    }

    fn seen(&self, id: Id) -> bool {
        self.set.seen(id)
    }

    fn admit(&self, payload: &[u8]) -> bool {
        let Ok(tx) = self.marshaller.unmarshal(payload) else {
            return false;
        };
        let gid = tx.gossip_id();
        let was_new = !self.set.pool().has(gid);
        let _ = self.set.add(tx);
        was_new
    }
}

/// A transport that pushes marshalled txs straight into a peer VM's gossip set
/// (the in-process stand-in for the live `Network::gossip` wiring, M8).
#[derive(Clone)]
struct DirectTransport {
    peer: Arc<BloomSet>,
    marshaller: GossipMarshaller,
}

impl ava_saevm_cchain::gossip::GossipTransport for DirectTransport {
    fn push(&self, payloads: &[Vec<u8>]) -> usize {
        let mut n = 0;
        for p in payloads {
            if let Ok(tx) = self.marshaller.unmarshal(p) {
                let _ = self.peer.add(tx);
                n += 1;
            }
        }
        n
    }
    fn pull(&self, _have: &[Id]) -> usize {
        0
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawned_push_loop_gossips_then_shutdown_stops_it() {
    let node_a = new_vm();
    let node_b = new_vm();

    let transport = DirectTransport {
        peer: Arc::clone(node_b.gossip_set()),
        marshaller: GossipMarshaller::new(),
    };
    // Re-initialize node A with a live transport + fast gossip period.
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let memory = Memory::new(Arc::clone(&base));
    let sm = memory.new_shared_memory(c_chain_id());
    let cfg = ava_saevm_cchain::vm::GossipConfig {
        push_period: std::time::Duration::from_millis(10),
        pull_period: std::time::Duration::from_millis(10),
    };
    let node_a_live = Vm::initialize_with_gossip(
        &base,
        Arc::new(sm),
        c_chain_id(),
        avax_asset_id(),
        Some((transport, cfg)),
    )
    .expect("initialize with gossip");
    let _ = &node_a; // keep the helper VM construction exercised

    let tx = import_tx(0x66);
    let tx_id = tx.id();
    node_a_live.avax_service().issue_tx(&tx).expect("issue");

    // The spawned push loop re-broadcasts the pool each tick; wait for B to get it.
    for _ in 0..100 {
        if node_b.atomic_txpool().has(tx_id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        node_b.atomic_txpool().has(tx_id),
        "spawned push loop gossiped the tx to B"
    );

    // Shutdown cancels the loops and awaits them.
    node_a_live.shutdown().await;
}

#[test]
fn issued_tx_reaches_peer_pool_via_push_gossip() {
    let node_a = new_vm();
    let node_b = new_vm();

    let a_id = id(0xa1);
    let b_id = id(0xb2);

    let mut net = Network::new();
    net.add_node(
        a_id,
        Arc::new(NodeAdapter::new(Arc::clone(node_a.gossip_set()))),
    );
    net.add_node(
        b_id,
        Arc::new(NodeAdapter::new(Arc::clone(node_b.gossip_set()))),
    );
    net.connect(a_id, b_id);

    // issueTx on node A admits the tx (Go `api.IssueTx` → gossipSet.Add).
    let tx = import_tx(0x33);
    let tx_id = tx.id();
    let issued = node_a.avax_service().issue_tx(&tx).expect("issue tx on A");
    assert_eq!(issued, tx_id, "issueTx returns the tx id");
    assert!(node_a.atomic_txpool().has(tx_id), "tx pooled on A");
    assert!(
        !node_b.atomic_txpool().has(tx_id),
        "tx not yet on B before gossip"
    );

    // Push gossip from A delivers the tx to B's pool.
    let admitted = net.push(a_id);
    assert_eq!(admitted, 1, "exactly one tx pushed to B");
    assert!(
        node_b.atomic_txpool().has(tx_id),
        "tx reached B's pool via push gossip"
    );
}

#[test]
fn seeded_tx_reaches_peer_pool_via_pull_gossip() {
    let node_a = new_vm();
    let node_b = new_vm();

    let a_id = id(0xa1);
    let b_id = id(0xb2);

    let mut net = Network::new();
    net.add_node(
        a_id,
        Arc::new(NodeAdapter::new(Arc::clone(node_a.gossip_set()))),
    );
    net.add_node(
        b_id,
        Arc::new(NodeAdapter::new(Arc::clone(node_b.gossip_set()))),
    );
    net.connect(a_id, b_id);

    // A tx pre-seeded on B (e.g. issued there earlier).
    let tx = import_tx(0x44);
    let tx_id = tx.id();
    node_b.avax_service().issue_tx(&tx).expect("seed tx on B");
    assert!(node_b.atomic_txpool().has(tx_id), "tx seeded on B");
    assert!(!node_a.atomic_txpool().has(tx_id), "tx not yet on A");

    // A pulls from its peers; B serves the tx A is missing.
    let admitted = net.pull(a_id);
    assert_eq!(admitted, 1, "exactly one tx pulled to A");
    assert!(
        node_a.atomic_txpool().has(tx_id),
        "tx reached A's pool via pull gossip"
    );
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M6.17 — `AtomicBackend` + atomic trie (2nd Firewood) + shared-memory batch
//! (G3, spec 10 §6.4/§17.4).
//!
//! `accept(height, txs)` indexes the block's atomic `Requests` into a second
//! ethhash Firewood trie (key = `height(8B)||blockchainID(32B)`, value =
//! byte-exact serialized `Requests`), advancing the trie root to a **Go-executed
//! golden** root, and applies the cross-chain Put/Remove to shared memory in the
//! same atomic accept (Import → Remove on source, Export → Put on dest).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ava_avm::txs::components::{
    Input as FxInput, Output as FxOutput, TransferableInput, TransferableOutput,
};
use ava_evm::atomic::backend::{AtomicBackend, DEFAULT_COMMIT_INTERVAL};
use ava_evm::atomic::trie::{AtomicTrie, TRIE_KEY_LENGTH, serialize_requests, trie_key};
use ava_evm::atomic::tx::{AtomicTx, EvmInput, EvmOutput, Tx, UnsignedExportTx, UnsignedImportTx};
use ava_evm_reth::{B256, EMPTY_ROOT_HASH};
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_vm::components::avax::shared_memory::{Element, IndexedResult, Requests, SharedMemory};
use ava_vm::error::Error as VmError;

/// 32-byte id with every byte = `b`.
fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

/// The deterministic AVAX asset id used by the Go golden dump (0xAA × 32).
fn avax_asset() -> Id {
    id32(0xAA)
}

/// The Go-golden import tx (matches `tests/vectors/cchain/atomic/atomic_txs.json`).
fn golden_import_tx() -> Tx {
    let unsigned = UnsignedImportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        source_chain: id32(0x22),
        imported_inputs: vec![TransferableInput {
            tx_id: id32(0x44),
            output_index: 1,
            asset_id: avax_asset(),
            r#in: FxInput::SecpTransfer(TransferInput::new(5000, vec![0])),
        }],
        outs: vec![EvmOutput {
            address: [0x01; 20],
            amount: 4999,
            asset_id: avax_asset(),
        }],
    };
    let mut tx = Tx::new(AtomicTx::Import(unsigned));
    tx.initialize().expect("initialize import");
    tx
}

/// The Go-golden export tx; `initialize()` derives its signed-tx id which must
/// equal the golden `export_tx_id` so the put `Element` key/value match Go.
fn golden_export_tx() -> Tx {
    let unsigned = UnsignedExportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        destination_chain: id32(0x33),
        ins: vec![EvmInput {
            address: [0x02; 20],
            amount: 3000,
            asset_id: avax_asset(),
            nonce: 7,
        }],
        exported_outputs: vec![TransferableOutput {
            asset_id: avax_asset(),
            out: FxOutput::SecpTransfer(TransferOutput {
                amt: 3000,
                owners: OutputOwners {
                    locktime: 0,
                    threshold: 1,
                    addrs: vec![ShortId::from([0x05; 20])],
                },
            }),
        }],
    };
    let mut tx = Tx::new(AtomicTx::Export(unsigned));
    tx.initialize().expect("initialize export");
    tx
}

/// One peer chain's view: `key -> (value, traits)`.
type ChainView = BTreeMap<Vec<u8>, (Vec<u8>, Vec<Vec<u8>>)>;

/// A narrow in-memory `SharedMemory` recording every `apply`. Keyed per peer
/// chain so we can assert Put landed and Remove deleted.
#[derive(Default)]
struct InMemorySharedMemory {
    // chain -> key -> (value, traits)
    state: Mutex<BTreeMap<Id, ChainView>>,
}

impl SharedMemory for InMemorySharedMemory {
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, VmError> {
        let state = self.state.lock().expect("lock");
        let chain = state.get(&peer_chain);
        Ok(keys
            .iter()
            .map(|k| {
                chain
                    .and_then(|c| c.get(k))
                    .map(|(v, _)| v.clone())
                    .unwrap_or_default()
            })
            .collect())
    }

    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> Result<IndexedResult, VmError> {
        Ok((Vec::new(), Vec::new(), Vec::new()))
    }

    fn apply(
        &self,
        requests: BTreeMap<Id, Requests>,
        _batches: &[ava_database::BatchOps],
    ) -> Result<(), VmError> {
        let mut state = self.state.lock().expect("lock");
        for (chain, reqs) in requests {
            let entry = state.entry(chain).or_default();
            for key in reqs.remove {
                entry.remove(&key);
            }
            for el in reqs.put {
                entry.insert(el.key, (el.value, el.traits));
            }
        }
        Ok(())
    }
}

impl InMemorySharedMemory {
    fn has_key(&self, chain: Id, key: &[u8]) -> bool {
        self.state
            .lock()
            .expect("lock")
            .get(&chain)
            .is_some_and(|c| c.contains_key(key))
    }

    fn value(&self, chain: Id, key: &[u8]) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("lock")
            .get(&chain)
            .and_then(|c| c.get(key).map(|(v, _)| v.clone()))
    }
}

/// Reads `tests/vectors/cchain/atomic_trie/atomic_trie_root.json`.
fn golden() -> serde_json::Value {
    let raw = include_str!("vectors/cchain/atomic_trie/atomic_trie_root.json");
    serde_json::from_str(raw).expect("golden json")
}

fn b256_hex(s: &str) -> B256 {
    B256::from_slice(&hex::decode(s).expect("hex"))
}

#[test]
fn accept_indexes_trie_and_applies_shared_memory() {
    let g = golden();

    // (d) An empty atomic trie starts at EmptyRootHash.
    let trie_dir = tempfile::tempdir().expect("tempdir");
    let trie = AtomicTrie::open(trie_dir.path()).expect("open trie");
    assert_eq!(trie.root(), EMPTY_ROOT_HASH);
    assert_eq!(trie.root(), b256_hex(g["empty_root"].as_str().unwrap()));

    // (b) TrieKeyLength == 40 and the key encoding matches Go.
    assert_eq!(TRIE_KEY_LENGTH, 40);
    assert_eq!(
        hex::encode(trie_key(1, &id32(0x22))),
        g["source_chain"]["key"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(trie_key(1, &id32(0x33))),
        g["dest_chain"]["key"].as_str().unwrap()
    );

    // The serialized Requests (trie VALUE) is byte-exact vs Go for both chains.
    let import_tx = golden_import_tx();
    let export_tx = golden_export_tx();
    let (src_chain, import_reqs) = import_tx
        .unsigned
        .atomic_ops(import_tx.id())
        .expect("import");
    let (dst_chain, export_reqs) = export_tx
        .unsigned
        .atomic_ops(export_tx.id())
        .expect("export");
    assert_eq!(src_chain, id32(0x22));
    assert_eq!(dst_chain, id32(0x33));
    assert_eq!(
        hex::encode(serialize_requests(&import_reqs).expect("ser import")),
        g["source_chain"]["value"].as_str().unwrap()
    );
    assert_eq!(
        hex::encode(serialize_requests(&export_reqs).expect("ser export")),
        g["dest_chain"]["value"].as_str().unwrap()
    );

    // Wire the backend over the trie + an in-memory shared memory.
    let shared = Arc::new(InMemorySharedMemory::default());
    let backend = AtomicBackend::new(
        trie,
        Arc::clone(&shared) as Arc<dyn SharedMemory>,
        DEFAULT_COMMIT_INTERVAL,
    );

    // (a) accept(height=1, [import, export]) advances the trie root to the Go
    //     golden root.
    let root = backend
        .accept(1, &[import_tx.clone(), export_tx.clone()])
        .expect("accept");
    assert_eq!(root, b256_hex(g["root"].as_str().unwrap()));
    assert_eq!(backend.root(), root);

    // (c) shared memory now reflects the cross-chain effects:
    //   - Export → Put on the destination chain (key present, value matches).
    let export_key = export_reqs.put[0].key.clone();
    assert!(shared.has_key(id32(0x33), &export_key));
    assert_eq!(
        shared.value(id32(0x33), &export_key),
        Some(export_reqs.put[0].value.clone())
    );
    //   - Import → Remove on the source chain. We pre-seed the source key, accept
    //     again at a new height, then assert it was removed.
    shared
        .apply(
            BTreeMap::from([(
                id32(0x22),
                Requests {
                    remove: Vec::new(),
                    put: vec![Element {
                        key: import_reqs.remove[0].clone(),
                        value: vec![0xde, 0xad],
                        traits: Vec::new(),
                    }],
                },
            )]),
            &[],
        )
        .expect("seed");
    assert!(shared.has_key(id32(0x22), &import_reqs.remove[0]));
    backend
        .accept(2, std::slice::from_ref(&import_tx))
        .expect("accept import remove");
    assert!(!shared.has_key(id32(0x22), &import_reqs.remove[0]));
}

#[test]
fn accept_with_no_txs_does_not_advance_root() {
    let trie_dir = tempfile::tempdir().expect("tempdir");
    let trie = AtomicTrie::open(trie_dir.path()).expect("open trie");
    let shared = Arc::new(InMemorySharedMemory::default());
    let backend = AtomicBackend::new(
        trie,
        shared as Arc<dyn SharedMemory>,
        DEFAULT_COMMIT_INTERVAL,
    );

    let root = backend.accept(1, &[]).expect("accept empty");
    assert_eq!(root, EMPTY_ROOT_HASH);
    assert_eq!(backend.last_committed_root(), EMPTY_ROOT_HASH);
}

#[test]
fn commit_interval_checkpoints_durable_root() {
    let trie_dir = tempfile::tempdir().expect("tempdir");
    let trie = AtomicTrie::open(trie_dir.path()).expect("open trie");
    let shared = Arc::new(InMemorySharedMemory::default());
    // commit_interval = 2: only even heights checkpoint last_committed_root.
    let backend = AtomicBackend::new(trie, shared as Arc<dyn SharedMemory>, 2);

    let import_tx = golden_import_tx();

    // height 1 (odd): root advances but last_committed_root stays empty.
    let r1 = backend
        .accept(1, std::slice::from_ref(&import_tx))
        .expect("accept h1");
    assert_ne!(r1, EMPTY_ROOT_HASH);
    assert_eq!(backend.last_committed_root(), EMPTY_ROOT_HASH);

    // height 2 (even): checkpoint to the current root.
    let r2 = backend
        .accept(2, std::slice::from_ref(&import_tx))
        .expect("accept h2");
    assert_eq!(backend.last_committed_root(), r2);
}

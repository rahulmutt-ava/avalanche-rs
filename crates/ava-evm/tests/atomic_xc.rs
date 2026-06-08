// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M6.19 — `differential::atomic_xc`: X↔C atomic import/export parity
//! (spec 10 §6, §14 #3; 02 §11; 07).
//!
//! End-to-end **recorded-oracle** parity test over the Go-executed atomic corpus
//! (`tests/vectors/cchain/atomic/atomic_txs.json` +
//! `tests/vectors/cchain/atomic_trie/atomic_trie_root.json`, both Go-EXECUTED
//! against coreth `plugin/evm/atomic{,/state}` — see the sibling `_provenance.md`
//! files). For a Go corpus of one ImportTx + one ExportTx it asserts, in one
//! pass, the four facets that define X↔C atomic parity:
//!
//! - **(a) serialization** — the unsigned tx bytes (struct + interface form) and
//!   each component (`EvmOutput`/`EvmInput`) are byte-identical to Go.
//! - **(b) atomic `Requests`** — Import → `RemoveRequests` on the SOURCE chain,
//!   Export → `PutRequests`/`Element`s on the DESTINATION chain, byte-identical to
//!   Go (chain id, remove ids, element key/value/traits).
//! - **(c) post-`EVMStateTransfer` balances/nonces** — applying [`AtomicStateHook`]
//!   to a Firewood-backed `State` overlay credits `amount * X2C_RATE` wei to the
//!   import recipient and debits `amount * X2C_RATE` + bumps `nonce → nonce+1` on
//!   the export EOA, exactly as coreth `(*UnsignedImportTx).EVMStateTransfer` /
//!   `(*UnsignedExportTx).EVMStateTransfer`.
//! - **(d) atomic-trie root** — `AtomicBackend::accept` indexes the merged batch
//!   `Requests` into the 2nd ethhash Firewood trie, advancing it to the Go-golden
//!   root, and applies the cross-chain Put/Remove to shared memory in the same
//!   accept (checked against the in-memory `ava_vm` `SharedMemory` harness, 07, so
//!   M6 stays independent of M5).
//!
//! `atomic_xc` (this exact fn name is the milestone exit-gate) runs in recorded
//! mode — the per-PR gate. A true live-two-binary X↔C variant would be tagged
//! `#[ignore]`; none is required here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ava_avm::txs::components::{
    Input as FxInput, Output as FxOutput, TransferableInput, TransferableOutput,
};
use ava_codec::Serializable;
use ava_database::MemDb;
use ava_evm::atomic::backend::{AtomicBackend, DEFAULT_COMMIT_INTERVAL};
use ava_evm::atomic::hook::AtomicStateHook;
use ava_evm::atomic::trie::{AtomicTrie, TRIE_KEY_LENGTH, serialize_requests, trie_key};
use ava_evm::atomic::tx::{
    AtomicTx, CODEC_VERSION, EvmInput, EvmOutput, Tx, UnsignedExportTx, UnsignedImportTx, X2C_RATE,
    codec,
};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    AccountInfo, AccountReader, Address, B256, BundleState, Chain, EMPTY_ROOT_HASH,
    ExternalConsensusExecutor, Header, State, StateBuilder, StateProviderDatabase, U256,
};
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_vm::components::avax::shared_memory::{Element, IndexedResult, Requests, SharedMemory};
use ava_vm::error::Error as VmError;
use pretty_assertions::assert_eq;
use serde_json::Value;

// --- golden corpus ----------------------------------------------------------

fn tx_vectors() -> Value {
    let raw = include_str!("vectors/cchain/atomic/atomic_txs.json");
    serde_json::from_str(raw).expect("parse atomic_txs golden vectors")
}

fn trie_vectors() -> Value {
    let raw = include_str!("vectors/cchain/atomic_trie/atomic_trie_root.json");
    serde_json::from_str(raw).expect("parse atomic_trie golden vectors")
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing {ptr}"))
        .to_string()
}

fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

fn avax_asset() -> Id {
    id32(0xAA)
}

fn marshal<T: Serializable>(v: &T) -> String {
    hex::encode(codec().marshal(CODEC_VERSION, v).expect("marshal"))
}

fn b256_hex(s: &str) -> B256 {
    B256::from_slice(&hex::decode(s).expect("hex"))
}

/// Inserts a big-endian `u32` `type_id` (8 hex chars) right after the 2-byte
/// codec version prefix (4 hex chars) of a bare-struct hex encoding, yielding the
/// interface-framed encoding the signed `Tx` envelope carries.
fn splice_type_id(struct_hex: &str, type_id: u32) -> String {
    let (version, body) = struct_hex.split_at(4);
    format!("{version}{type_id:08x}{body}")
}

// The deterministic golden corpus (matches the Go-executed `atomic_txs.json`).

fn golden_import_unsigned() -> UnsignedImportTx {
    UnsignedImportTx {
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
    }
}

fn golden_export_unsigned() -> UnsignedExportTx {
    UnsignedExportTx {
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
    }
}

fn golden_import_tx() -> Tx {
    let mut tx = Tx::new(AtomicTx::Import(golden_import_unsigned()));
    tx.initialize().expect("initialize import");
    tx
}

fn golden_export_tx() -> Tx {
    let mut tx = Tx::new(AtomicTx::Export(golden_export_unsigned()));
    tx.initialize().expect("initialize export");
    tx
}

// --- in-memory SharedMemory harness (07; keeps M6 independent of M5) --------

type ChainView = BTreeMap<Vec<u8>, (Vec<u8>, Vec<Vec<u8>>)>;

#[derive(Default)]
struct InMemorySharedMemory {
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

fn far_future_upgrades() -> NetworkUpgrades {
    const FAR: u64 = u64::MAX;
    NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR,
        apricot_phase_5: FAR,
        apricot_phase_pre_6: FAR,
        apricot_phase_6: FAR,
        apricot_phase_post_6: FAR,
        banff: FAR,
        cortina: FAR,
        durango: FAR,
        etna: FAR,
        fortuna: FAR,
        granite: FAR,
    }
}

#[test]
fn atomic_xc() {
    let v = tx_vectors();
    let g = trie_vectors();

    let import_tx = golden_import_tx();
    let export_tx = golden_export_tx();

    // ====================================================================
    // (a) serialization — byte-identical tx + component encodings vs Go.
    // ====================================================================

    // Components.
    let out = EvmOutput {
        address: [0x01; 20],
        amount: 1000,
        asset_id: avax_asset(),
    };
    assert_eq!(marshal(&out), s(&v, "/evm_output/codec_hex"), "EvmOutput");
    let evm_in = EvmInput {
        address: [0x02; 20],
        amount: 2000,
        asset_id: avax_asset(),
        nonce: 7,
    };
    assert_eq!(marshal(&evm_in), s(&v, "/evm_input/codec_hex"), "EvmInput");

    // Unsigned bare-struct form.
    let import_struct = s(&v, "/unsigned_import_tx/struct_codec_hex");
    let export_struct = s(&v, "/unsigned_export_tx/struct_codec_hex");
    assert_eq!(
        marshal(&golden_import_unsigned()),
        import_struct,
        "import struct codec"
    );
    assert_eq!(
        marshal(&golden_export_unsigned()),
        export_struct,
        "export struct codec"
    );

    // Interface form (signed-tx envelope carries the u32 type_id: 0/1).
    assert_eq!(
        marshal(&AtomicTx::Import(golden_import_unsigned())),
        splice_type_id(&import_struct, 0),
        "import interface codec"
    );
    assert_eq!(
        marshal(&AtomicTx::Export(golden_export_unsigned())),
        splice_type_id(&export_struct, 1),
        "export interface codec"
    );

    // The export's signed-tx id is the Go-golden id (drives the put Element key).
    assert_eq!(
        hex::encode(export_tx.id().to_bytes()),
        s(&v, "/export_tx_id"),
        "export signed-tx id"
    );

    // ====================================================================
    // (b) atomic Requests — Import→Remove on source, Export→Put on dest.
    // ====================================================================

    let (src_chain, import_reqs) = golden_import_unsigned().atomic_ops();
    assert_eq!(
        hex::encode(src_chain.to_bytes()),
        s(&v, "/import_atomic_ops/chain"),
        "import ops chain (source)"
    );
    assert!(import_reqs.put.is_empty(), "import has no puts");
    assert_eq!(import_reqs.remove.len(), 1, "import removes one utxo");
    assert_eq!(
        hex::encode(&import_reqs.remove[0]),
        s(&v, "/import_atomic_ops/remove_requests/0"),
        "import remove id"
    );

    let (dst_chain, export_reqs) = golden_export_unsigned()
        .atomic_ops(export_tx.id())
        .expect("export atomic ops");
    assert_eq!(
        hex::encode(dst_chain.to_bytes()),
        s(&v, "/export_atomic_ops/chain"),
        "export ops chain (dest)"
    );
    assert!(export_reqs.remove.is_empty(), "export has no removes");
    assert_eq!(export_reqs.put.len(), 1, "export puts one element");
    let elem = &export_reqs.put[0];
    assert_eq!(
        hex::encode(&elem.key),
        s(&v, "/export_atomic_ops/put_requests/0/key"),
        "export element key"
    );
    assert_eq!(
        hex::encode(&elem.value),
        s(&v, "/export_atomic_ops/put_requests/0/value"),
        "export element value"
    );
    assert_eq!(elem.traits.len(), 1, "export element has one trait");
    assert_eq!(
        hex::encode(&elem.traits[0]),
        s(&v, "/export_atomic_ops/put_requests/0/traits/0"),
        "export element trait"
    );

    // ====================================================================
    // (c) post-EVMStateTransfer balances/nonces, via AtomicStateHook on a
    //     Firewood-backed State overlay (same execute_batch path as the EVM
    //     tx loop). Uses the golden tx amounts; the hook arithmetic
    //     (amount * X2C_RATE, nonce = max(cur, nonce+1)) mirrors coreth
    //     (*UnsignedImport/Export).EVMStateTransfer verbatim.
    // ====================================================================

    let import_to = Address::from([0x01; 20]); // golden import out address
    let export_from = Address::from([0x02; 20]); // golden export in address
    let import_amount: u64 = 4999; // golden import out amount
    let export_amount: u64 = 3000; // golden export in amount
    let export_nonce: u64 = 7; // golden export in nonce
    // Genesis the export EOA at the exact nonce coreth's EVMStateTransfer requires
    // (it rejects unless cur == input.nonce) and enough balance to cover the debit.
    let from_genesis_wei = U256::from(export_amount) * U256::from(X2C_RATE) * U256::from(4u64);

    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");

    let genesis_bundle = BundleState::builder(0..=0)
        .state_present_account_info(
            export_from,
            AccountInfo {
                balance: from_genesis_wei,
                nonce: export_nonce,
                ..Default::default()
            },
        )
        .build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    // The hook carries the SAME unsigned atomic txs as the golden corpus.
    let hook = AtomicStateHook::new(vec![
        AtomicTx::Import(golden_import_unsigned()),
        AtomicTx::Export(golden_export_unsigned()),
    ]);

    let chain_spec = AvaChainSpec::from_parts(far_future_upgrades(), Chain::from_id(43114), false);
    let config = AvaEvmConfig::new(chain_spec);
    let header = Header {
        number: 1,
        timestamp: 1,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(25_000_000_000),
        ..Default::default()
    };
    let view = provider
        .history_by_state_root(genesis_root)
        .expect("genesis view");
    let mut state: State<StateProviderDatabase<_>> = StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();
    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch(env, &mut state, &hook, &[])
        .expect("execute_batch with atomic hook");
    let post_root = provider
        .propose_from_bundle(&outcome.bundle)
        .expect("propose post-state");
    provider.commit(post_root).expect("commit post-state");
    let post_view = provider
        .history_by_state_root(post_root)
        .expect("post view");

    // Import credits amount * X2C_RATE wei to the recipient; nonce untouched.
    let to_acct = post_view
        .basic_account(&import_to)
        .expect("read to")
        .expect("recipient credited");
    assert_eq!(
        to_acct.balance,
        U256::from(import_amount) * U256::from(X2C_RATE),
        "import credit"
    );
    assert_eq!(to_acct.nonce, 0, "import does not touch recipient nonce");

    // Export debits amount * X2C_RATE wei and bumps nonce to input.nonce + 1.
    let from_acct = post_view
        .basic_account(&export_from)
        .expect("read from")
        .expect("export EOA present");
    assert_eq!(
        from_acct.balance,
        from_genesis_wei - U256::from(export_amount) * U256::from(X2C_RATE),
        "export debit"
    );
    assert_eq!(from_acct.nonce, export_nonce + 1, "export bumps nonce");

    // ====================================================================
    // (d) atomic-trie root — AtomicBackend::accept advances the 2nd Firewood
    //     trie to the Go-golden root and applies cross-chain Put/Remove to
    //     shared memory in the same accept.
    // ====================================================================

    let trie_dir = tempfile::tempdir().expect("tempdir");
    let trie = AtomicTrie::open(trie_dir.path()).expect("open trie");
    assert_eq!(trie.root(), EMPTY_ROOT_HASH, "fresh trie root");
    assert_eq!(
        trie.root(),
        b256_hex(g["empty_root"].as_str().unwrap()),
        "empty root vs Go"
    );

    // Trie key encoding + length vs Go.
    assert_eq!(TRIE_KEY_LENGTH, 40, "trie key length");
    assert_eq!(
        hex::encode(trie_key(1, &id32(0x22))),
        g["source_chain"]["key"].as_str().unwrap(),
        "source trie key"
    );
    assert_eq!(
        hex::encode(trie_key(1, &id32(0x33))),
        g["dest_chain"]["key"].as_str().unwrap(),
        "dest trie key"
    );

    // Serialized Requests (the trie VALUE) byte-exact vs Go for both chains.
    assert_eq!(
        hex::encode(serialize_requests(&import_reqs).expect("ser import")),
        g["source_chain"]["value"].as_str().unwrap(),
        "source chain Requests value"
    );
    assert_eq!(
        hex::encode(serialize_requests(&export_reqs).expect("ser export")),
        g["dest_chain"]["value"].as_str().unwrap(),
        "dest chain Requests value"
    );

    let shared = Arc::new(InMemorySharedMemory::default());
    let backend = AtomicBackend::new(
        trie,
        Arc::clone(&shared) as Arc<dyn SharedMemory>,
        DEFAULT_COMMIT_INTERVAL,
    );

    // accept(height=1, [import, export]) → Go-golden root.
    let root = backend
        .accept(1, &[import_tx.clone(), export_tx.clone()])
        .expect("accept");
    assert_eq!(
        root,
        b256_hex(g["root"].as_str().unwrap()),
        "atomic-trie root"
    );
    assert_eq!(backend.root(), root, "backend root tracks trie");

    // Cross-chain shared-memory effects landed in the same accept:
    //   Export → Put on the destination chain.
    let export_key = export_reqs.put[0].key.clone();
    assert!(
        shared.has_key(id32(0x33), &export_key),
        "export Put present on dest"
    );
    assert_eq!(
        shared.value(id32(0x33), &export_key),
        Some(export_reqs.put[0].value.clone()),
        "export Put value on dest"
    );
    //   Import → Remove on the source chain: seed the key, accept again, assert gone.
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
        .expect("seed source key");
    assert!(
        shared.has_key(id32(0x22), &import_reqs.remove[0]),
        "seeded source key present"
    );
    backend
        .accept(2, std::slice::from_ref(&import_tx))
        .expect("accept import remove");
    assert!(
        !shared.has_key(id32(0x22), &import_reqs.remove[0]),
        "import Remove deleted source key"
    );
}

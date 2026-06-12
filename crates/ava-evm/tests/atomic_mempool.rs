// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Behavioral tests for the atomic mempool (M6.16; spec 10 §6.4, §17.4).
//!
//! Port of coreth `plugin/evm/atomic/txpool` semantics: heap ordering by
//! effective gas price, dedup by source UTXO, conflict-reject of txs spending a
//! UTXO already pending, one gas-limited batch per block, the
//! `discardedTxs`/`issuedTxs` lifecycle, and a `Notify` that fires on non-empty.

use std::time::Duration;

use ava_avm::txs::components::{Input as FxInput, TransferableInput};
use ava_avm::txs::credential::FxCredential;
use ava_evm::atomic::mempool::{AtomicMempool, AvaNextBlockCtx, Gossipable, MempoolError};
use ava_evm::atomic::tx::{AtomicTx, CODEC_VERSION, EvmOutput, Tx, UnsignedImportTx, codec};
use ava_secp256k1fx::{Credential as SecpCredential, TransferInput};
use ava_types::id::Id;

/// 32-byte id with every byte = `b`.
fn id32(b: u8) -> Id {
    Id::from([b; 32])
}

/// The deterministic AVAX asset id (0xAA × 32).
fn avax_asset() -> Id {
    id32(0xAA)
}

/// Builds an initialized import `Tx` spending a single source UTXO identified by
/// `(utxo_tx, utxo_index)`, importing `imported` nAVAX and crediting `credited`
/// to the EVM (so the burn = imported - credited). `sigs` sets the number of
/// signature indices (affects `GasUsed`). The differing `imported`/`credited`
/// and source UTXO make distinct tx ids + gas prices.
fn import_tx(utxo_tx: u8, utxo_index: u32, imported: u64, credited: u64, sigs: usize) -> Tx {
    let unsigned = UnsignedImportTx {
        network_id: 1,
        blockchain_id: id32(0x11),
        source_chain: id32(0x22),
        imported_inputs: vec![TransferableInput {
            tx_id: id32(utxo_tx),
            output_index: utxo_index,
            asset_id: avax_asset(),
            r#in: FxInput::SecpTransfer(TransferInput::new(
                imported,
                (0..u32::try_from(sigs).expect("sigs fits u32")).collect(),
            )),
        }],
        outs: vec![EvmOutput {
            address: [0x01; 20],
            amount: credited,
            asset_id: avax_asset(),
        }],
    };
    let mut tx = Tx::new(AtomicTx::Import(unsigned));
    tx.initialize().expect("initialize");
    tx
}

#[test]
fn mempool_orders_dedups_and_conflict_checks() {
    let mut m = AtomicMempool::new(1024, avax_asset());

    // Two distinct (different source UTXO) txs with different burns => different
    // effective gas prices. tx_hi burns more per gas than tx_lo.
    let tx_hi = import_tx(0x44, 1, 5_000, 1_000, 1); // burn 4000
    let tx_lo = import_tx(0x55, 1, 5_000, 4_000, 1); // burn 1000

    let notify = m.subscribe();
    assert!(m.is_empty());

    // Insert low first, then high — heap must still surface high first.
    m.add(tx_lo.clone()).expect("add lo");
    m.add(tx_hi.clone()).expect("add hi");
    assert_eq!(m.pending_len(), 2);

    // --- Notify fired on non-empty (a permit was stored on add) --------------
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("rt");
    rt.block_on(async {
        // Resolves immediately because add() stored a permit; timeout guards the
        // failure case so the test can never hang.
        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .expect("notify should have fired on non-empty");
    });

    // --- Dedup by tx id -------------------------------------------------------
    assert_eq!(m.add(tx_hi.clone()), Err(MempoolError::AlreadyKnown));
    assert_eq!(m.pending_len(), 2);

    // --- Conflict reject: same source UTXO, not higher fee -> rejected --------
    // tx_dup_lo spends the SAME UTXO as tx_hi but burns less => lower gas price.
    let tx_dup_lo = import_tx(0x44, 1, 5_000, 4_900, 1); // burn 100, same UTXO as tx_hi
    assert_eq!(m.add(tx_dup_lo), Err(MempoolError::Conflict));
    assert_eq!(m.pending_len(), 2);

    // --- Heap ordering: next_batch surfaces tx_hi before tx_lo ----------------
    let ctx = AvaNextBlockCtx::with_atomic_gas_limit(u64::MAX);
    let batch = m.next_batch(&ctx);
    assert_eq!(batch.len(), 2, "both fit under the budget");
    assert_eq!(batch[0].id(), tx_hi.id(), "highest gas price first");
    assert_eq!(batch[1].id(), tx_lo.id());

    // next_batch marked both Current — pending is now empty.
    assert_eq!(m.pending_len(), 0);

    // --- issuedTxs lifecycle --------------------------------------------------
    m.issue_current_txs();
    // Re-adding an issued tx is AlreadyKnown.
    assert_eq!(m.add(tx_hi.clone()), Err(MempoolError::AlreadyKnown));

    // --- Gossipable id is tx id ----------------------------------------------
    assert_eq!(tx_hi.gossip_id(), tx_hi.id());
    assert_ne!(tx_hi.gossip_id(), Id::EMPTY);
}

#[test]
fn next_batch_is_one_gas_limited_batch() {
    let mut m = AtomicMempool::new(1024, avax_asset());

    // Three txs, all distinct source UTXOs. With a tight gas budget only the
    // highest-priced one fits.
    let a = import_tx(0x01, 0, 5_000, 1_000, 1); // burn 4000 (highest price)
    let b = import_tx(0x02, 0, 5_000, 2_000, 1); // burn 3000
    let c = import_tx(0x03, 0, 5_000, 3_000, 1); // burn 2000
    m.add(a.clone()).expect("a");
    m.add(b.clone()).expect("b");
    m.add(c.clone()).expect("c");

    // Each import tx uses the SAME GasUsed (same shape); a budget of one tx's gas
    // admits exactly one. Pull the per-tx gas from the mempool's own accounting.
    let one_gas = m.tx_gas_used(&a).expect("gas");
    let ctx = AvaNextBlockCtx::with_atomic_gas_limit(one_gas);
    let batch = m.next_batch(&ctx);
    assert_eq!(batch.len(), 1, "exactly one tx fits the one-tx budget");
    assert_eq!(batch[0].id(), a.id(), "the highest-priced tx is chosen");

    // The unchosen txs were cancelled back to Pending (not lost).
    assert_eq!(m.pending_len(), 2);

    // A second build with a fat budget now takes the remaining two.
    let ctx2 = AvaNextBlockCtx::with_atomic_gas_limit(u64::MAX);
    // First issue the current (a) so it's not re-pulled.
    m.issue_current_txs();
    let batch2 = m.next_batch(&ctx2);
    assert_eq!(batch2.len(), 2);
    assert_eq!(batch2[0].id(), b.id(), "next highest price");
    assert_eq!(batch2[1].id(), c.id());
}

#[test]
fn discarded_tx_lifecycle() {
    let mut m = AtomicMempool::new(1024, avax_asset());
    let a = import_tx(0x07, 0, 5_000, 1_000, 1);
    m.add(a.clone()).expect("add");

    let ctx = AvaNextBlockCtx::with_atomic_gas_limit(u64::MAX);
    let batch = m.next_batch(&ctx);
    assert_eq!(batch.len(), 1);

    // Discard the current tx (e.g. it produced a conflict with an ancestor).
    m.discard_current_tx(&a.id());

    // A discarded tx is not pending and its source UTXO spender is cleared.
    assert_eq!(m.pending_len(), 0);
    assert!(m.is_discarded(&a.id()));

    // A *remote* re-add of a discarded tx is rejected as AlreadyKnown.
    assert_eq!(m.add_remote(a.clone()), Err(MempoolError::AlreadyKnown));

    // A *local* re-add bypasses the discarded check (UTXO may now be present).
    m.add_local(a.clone()).expect("local re-add");
    assert_eq!(m.pending_len(), 1);
    assert!(!m.is_discarded(&a.id()));
}

#[test]
fn mempool_full_evicts_lowest_priced() {
    // maxSize = 1: adding a higher-priced tx evicts the lower-priced one.
    let mut m = AtomicMempool::new(1, avax_asset());
    let lo = import_tx(0x10, 0, 5_000, 4_000, 1); // burn 1000 (low)
    let hi = import_tx(0x11, 0, 5_000, 1_000, 1); // burn 4000 (high)

    m.add(lo.clone()).expect("lo");
    assert_eq!(m.pending_len(), 1);

    // hi outbids lo => lo evicted, hi admitted.
    m.add(hi.clone()).expect("hi outbids");
    assert_eq!(m.pending_len(), 1);
    assert!(m.has(&hi.id()));
    assert!(!m.has(&lo.id()));

    // A second low-priced tx can't displace hi => insufficient fee.
    let lo2 = import_tx(0x12, 0, 5_000, 4_500, 1); // burn 500 (lower)
    assert_eq!(m.add(lo2), Err(MempoolError::InsufficientFee));
}

/// M6.29 fold-in (found by the M8.26 wallet differential): `gas_used` must be
/// computed over the **unsigned** tx bytes — coreth `Metadata.Bytes()`
/// (`metadata.go:30`) returns `unsignedBytes` despite the misleading name, and
/// `GasUsed` calls `calcBytesCost(len(utx.Bytes()))`
/// (`import_tx.go:136-138`, `export_tx.go:134-135`). Computing over the signed
/// envelope overcounts by 77 gas for a 1-credential/1-sig tx (4B creds len +
/// 4B cred type_id + 4B sigs len + 65B sig).
///
/// Pins the Go-EXECUTED values from `vectors/cchain/atomic/atomic_txs.json`
/// `gas_used` (emitter: `tests/differential/go-oracle/
/// atomic_tx_gas_emitter_test.go`, avalanchego@5896c92f, go1.25.10).
#[test]
fn gas_used_matches_coreth_oracle() {
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("vectors/cchain/atomic/atomic_txs.json"))
            .expect("parse golden vectors");

    for (kind, iface_ptr) in [
        ("import", "/unsigned_import_tx/interface_codec_hex"),
        ("export", "/unsigned_export_tx/interface_codec_hex"),
    ] {
        let iface_hex = vectors
            .pointer(iface_ptr)
            .and_then(serde_json::Value::as_str)
            .expect("interface_codec_hex");
        let unsigned_bytes = hex::decode(iface_hex).expect("hex");

        let golden = &vectors["gas_used"][kind];
        let want_unsigned_len = golden["unsigned_bytes_len"].as_u64().expect("unsigned len");
        let want_signed_len = golden["signed_bytes_len"].as_u64().expect("signed len");
        let want_gas = golden["gas_used_fixed_fee"].as_u64().expect("gas");

        // Parse the Go-golden unsigned tx and re-create the exact signed
        // envelope the Go emitter built: one secp credential with one 65-byte
        // signature (signature content does not affect GasUsed).
        let mut unsigned = AtomicTx::default();
        codec()
            .unmarshal(&unsigned_bytes, &mut unsigned)
            .expect("unmarshal unsigned interface bytes");
        assert_eq!(
            unsigned_bytes.len() as u64,
            want_unsigned_len,
            "{kind}: vector unsigned length"
        );

        let mut tx = Tx::new(unsigned);
        tx.creds = vec![FxCredential::new(
            Id::EMPTY,
            SecpCredential::new(vec![[0u8; 65]]),
        )];
        tx.initialize().expect("initialize");
        assert_eq!(
            tx.bytes().len() as u64,
            want_signed_len,
            "{kind}: signed envelope length must match the Go emitter's"
        );

        // Round-trip sanity: re-marshalling the unsigned body reproduces the
        // Go unsigned bytes GasUsed is priced over.
        let remarshal = codec()
            .marshal(CODEC_VERSION, &tx.unsigned)
            .expect("marshal unsigned");
        assert_eq!(
            remarshal, unsigned_bytes,
            "{kind}: unsigned bytes round-trip"
        );

        let m = AtomicMempool::new(1024, avax_asset());
        assert_eq!(
            m.tx_gas_used(&tx).expect("tx_gas_used"),
            want_gas,
            "{kind}: GasUsed(fixedFee=true) must equal the Go oracle (priced \
             over UNSIGNED bytes, coreth metadata.go:30)"
        );
    }
}

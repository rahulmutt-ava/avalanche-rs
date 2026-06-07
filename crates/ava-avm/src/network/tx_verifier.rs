// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain tx verifier seam for the gossip handler (specs 09 §8;
//! `vms/avm/network/tx_verifier.go`).
//!
//! Provides the [`TxVerifier`] trait and two implementations:
//!
//! * [`SyntacticTxVerifier`] — cheap, state-free check (initialized envelope).
//! * [`SemanticTxVerifier`] — full syntactic + semantic verification against a
//!   borrowed state view (for use when the chain is past bootstrapping).
//!
//! Mirrors the P-Chain precedent in `crates/ava-platformvm/src/network.rs`.

use ava_types::id::Id;

use crate::fx::dispatch::Dispatch;
use crate::state::chain::ReadOnlyChain;
use crate::txs::Tx;
use crate::txs::executor::backend::Backend;
use crate::txs::executor::semantic::SemanticVerifier;
use crate::txs::executor::syntactic::SyntacticVerifier;

/// `tx_verifier.VerifyTx` — the shape/semantic gate a gossiped tx must pass
/// before it is admitted to the mempool (Go `network.txVerifier`).
///
/// A minimal local seam: the real verifier runs the executor's syntactic +
/// semantic checks against the preferred state. During read-only sync the VM
/// supplies [`SyntacticTxVerifier`], which only enforces the cheap, state-free
/// shape checks. The trait keeps the handler decoupled from the executor
/// wiring.
///
/// Mirrors `crates/ava-platformvm/src/network.rs` `TxVerifier`.
pub trait TxVerifier {
    /// Returns `Ok(())` if `tx` is acceptable, or a human-readable reason it
    /// was rejected.
    ///
    /// # Errors
    /// Returns a descriptive reason string when the tx fails verification.
    fn verify_tx(&self, tx: &Tx) -> Result<(), String>;
}

/// The state-free verifier used during read-only sync: rejects uninitialized
/// txs (no id / no bytes) that are malformed gossip, without requiring a state
/// view.
///
/// Mirrors `crates/ava-platformvm/src/network.rs` `SyntacticVerifier`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyntacticTxVerifier;

impl TxVerifier for SyntacticTxVerifier {
    fn verify_tx(&self, tx: &Tx) -> Result<(), String> {
        // A gossiped tx must be initialized (have a non-empty ID / cached bytes);
        // an uninitialized envelope is malformed gossip.
        if tx.id() == Id::EMPTY || tx.bytes().is_empty() {
            return Err("tx is not initialized".to_string());
        }
        Ok(())
    }
}

/// The state-aware verifier used when the chain has finished bootstrapping:
/// runs the full [`SyntacticVerifier`] then [`SemanticVerifier`] against a
/// borrowed state view.
///
/// Holds borrowed references to the verification context, chain state, and fx
/// dispatch; the caller (the VM) constructs this for each gossip call when it
/// can supply a read-locked state view.
///
/// Mirrors `vms/avm/network/tx_verifier.go` `txVerifier`.
pub struct SemanticTxVerifier<'a> {
    backend: &'a Backend,
    state: &'a dyn ReadOnlyChain,
    fxs: &'a Dispatch,
}

impl<'a> SemanticTxVerifier<'a> {
    /// Builds a [`SemanticTxVerifier`] over the given context, state, and fx table.
    #[must_use]
    pub fn new(backend: &'a Backend, state: &'a dyn ReadOnlyChain, fxs: &'a Dispatch) -> Self {
        Self {
            backend,
            state,
            fxs,
        }
    }
}

impl<'a> TxVerifier for SemanticTxVerifier<'a> {
    fn verify_tx(&self, tx: &Tx) -> Result<(), String> {
        // Cheap init check first (same as SyntacticTxVerifier).
        if tx.id() == Id::EMPTY || tx.bytes().is_empty() {
            return Err("tx is not initialized".to_string());
        }

        // Run the stateless syntactic verifier.
        SyntacticVerifier::new(self.backend, tx)
            .verify()
            .map_err(|e| e.to_string())?;

        // Run the stateful semantic verifier.
        SemanticVerifier::new(self.backend, self.state, tx, self.fxs, Id::EMPTY) // Id::EMPTY: the asset_id arg is unused per SemanticVerifier::new doc
            .verify()
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::State;
    use crate::state::chain::Chain;
    use crate::txs::codec::{Codec, codec};
    use crate::txs::components::AvaxBaseTx;
    use crate::txs::components::{Input, Output, TransferableInput};
    use crate::txs::executor::backend::Config;
    use crate::txs::executor::semantic::Utxo;
    use crate::txs::{BaseTx, UnsignedTx};
    use crate::txs::{CreateAssetTx, FxCredential, InitialState};
    use ava_database::MemDb;
    use ava_secp256k1fx::{
        Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput,
    };
    use ava_types::short_id::ShortId;
    use ava_utils::clock::MockClock;

    const NETWORK_ID: u32 = 1;
    const TX_FEE: u64 = 0;
    const CREATE_ASSET_TX_FEE: u64 = 0;

    fn chain_id() -> Id {
        Id::from([0xcc; 32])
    }

    fn addr() -> ShortId {
        ShortId::from([0xab; 20])
    }

    fn owners() -> OutputOwners {
        OutputOwners::new(0, 1, vec![addr()])
    }

    fn make_backend(bootstrapped: bool) -> Backend {
        Backend::new(
            NETWORK_ID,
            chain_id(),
            Config::new(TX_FEE, CREATE_ASSET_TX_FEE),
            Id::EMPTY,
            3,
            bootstrapped,
        )
    }

    fn make_dispatch() -> Dispatch {
        Dispatch::new(
            Id::EMPTY,
            Id::from([1u8; 32]),
            Id::from([2u8; 32]),
            Arc::new(MockClock::default()),
        )
    }

    fn create_asset_tx_for_secp() -> crate::txs::Tx {
        let ca = CreateAssetTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: NETWORK_ID,
                blockchain_id: chain_id(),
                outs: vec![],
                ins: vec![],
                memo: vec![],
            }),
            name: "TestAsset".to_string(),
            symbol: "TST".to_string(),
            denomination: 8,
            states: vec![InitialState::new(
                0, // secp256k1fx
                vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
            )],
        };
        let mut tx = crate::txs::Tx::new(UnsignedTx::CreateAsset(ca));
        tx.initialize(Codec()).expect("initialize create-asset");
        tx
    }

    fn utxo_bytes_for(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
        let mut tx_id = [0u8; 32];
        tx_id[0] = tx_byte;
        let utxo = Utxo {
            tx_id: Id::from(tx_id),
            output_index: idx,
            asset_id,
            out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
        };
        (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
    }

    fn secp_cred() -> FxCredential {
        FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
    }

    #[test]
    fn syntactic_rejects_uninit() {
        let v = SyntacticTxVerifier;
        let uninit = crate::txs::Tx::new(UnsignedTx::default());
        assert!(v.verify_tx(&uninit).is_err());
    }

    #[test]
    fn syntactic_accepts_initialized() {
        let c = codec().expect("codec");
        let v = SyntacticTxVerifier;
        let base = BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![],
            ins: vec![],
            memo: vec![1, 2, 3],
        });
        let mut tx = crate::txs::Tx::new(UnsignedTx::Base(base));
        tx.initialize(&c).expect("initialize");
        assert!(v.verify_tx(&tx).is_ok());
    }

    /// Tests UTXO-lookup semantics: the verifier finds the UTXO in state,
    /// resolves the asset type, and passes fee + balance checks.
    ///
    /// The credential is an all-zeros fake (`[0u8; 65]` × 1); this is
    /// intentional. The dispatch is constructed with `bootstrapped = true` in
    /// the `Backend` but the `Dispatch` fx table is **not** bootstrapped, so
    /// `secp256k1fx::Fx::VerifySpend` skips signature recovery entirely
    /// (Go parity: `secp256k1fx.Fx` skips `VerifySpend` signatures until
    /// `Bootstrapped()` is called). The test therefore targets UTXO-lookup
    /// and amount-matching semantics, not credential validity.
    #[test]
    fn semantic_verifier_accepts_known_utxo() {
        let ca = create_asset_tx_for_secp();
        let asset_id = ca.id();

        let base_db = Arc::new(MemDb::new());
        let mut state = State::new(Arc::clone(&base_db)).expect("state");
        state.add_tx(asset_id, ca.bytes().to_vec());

        let (utxo_id, utxo_bytes) = utxo_bytes_for(0xaa, 0, asset_id, 1000);
        state.add_utxo(utxo_id, utxo_bytes);

        // Build a spending tx.
        let mut tx_id = [0u8; 32];
        tx_id[0] = 0xaa;
        let spending_tx_body = AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![],
            ins: vec![TransferableInput {
                tx_id: Id::from(tx_id),
                output_index: 0,
                asset_id,
                r#in: Input::SecpTransfer(TransferInput::new(1000, vec![0])),
            }],
            memo: vec![],
        };
        let mut spending_tx = crate::txs::Tx::new(UnsignedTx::Base(BaseTx::new(spending_tx_body)));
        // All-zeros fake secp credential: signature recovery is skipped because
        // the fx Dispatch is not bootstrapped (see doc above).
        spending_tx.creds = vec![secp_cred()];
        spending_tx
            .initialize(Codec())
            .expect("initialize spending tx");

        let backend = make_backend(true);
        let fxs = make_dispatch();
        let v = SemanticTxVerifier::new(&backend, &state, &fxs);
        assert!(v.verify_tx(&spending_tx).is_ok());
    }

    #[test]
    fn semantic_verifier_rejects_missing_utxo() {
        let ca = create_asset_tx_for_secp();
        let asset_id = ca.id();

        let base_db = Arc::new(MemDb::new());
        let mut state = State::new(Arc::clone(&base_db)).expect("state");
        state.add_tx(asset_id, ca.bytes().to_vec());
        // Note: UTXO NOT seeded — should fail.

        let mut tx_id = [0u8; 32];
        tx_id[0] = 0xaa;
        let spending_tx_body = AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![],
            ins: vec![TransferableInput {
                tx_id: Id::from(tx_id),
                output_index: 0,
                asset_id,
                r#in: Input::SecpTransfer(TransferInput::new(1000, vec![0])),
            }],
            memo: vec![],
        };
        let mut spending_tx = crate::txs::Tx::new(UnsignedTx::Base(BaseTx::new(spending_tx_body)));
        // All-zeros fake credential; sig recovery skipped (fx not bootstrapped — see
        // `semantic_verifier_accepts_known_utxo` doc for the full rationale).
        spending_tx.creds = vec![secp_cred()];
        spending_tx
            .initialize(Codec())
            .expect("initialize spending tx");

        let backend = make_backend(true);
        let fxs = make_dispatch();
        let v = SemanticTxVerifier::new(&backend, &state, &fxs);
        // Missing UTXO → database error (ErrNotFound variant).
        assert!(v.verify_tx(&spending_tx).is_err());
    }
}

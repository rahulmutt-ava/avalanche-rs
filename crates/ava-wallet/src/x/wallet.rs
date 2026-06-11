// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-chain wallet facade — port of `wallet/chain/x/wallet.go` +
//! `backend.go` + `backend_visitor.go`.
//!
//! [`Wallet::issue_base_tx`] (and friends) = build → sign → submit over the
//! [`XChainClient`] seam → poll for acceptance (unless
//! [`TxOption::AssumeDecided`]) → record in the [`Backend`]
//! (`backend.AcceptTx`).
//!
//! `IssueOperationTx*` (mint FT/NFT/property) is deferred with the typed fx
//! operations (see [`crate::x::builder`]); issuing an
//! [`UnsignedTx::Operation`] through [`Wallet::issue_unsigned_tx`] surfaces
//! the signer's [`Error::UnsupportedTxType`](crate::error::Error).

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_avm::txs::UnsignedTx;
use ava_avm::txs::components::{Output as FxOutput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo;
use ava_secp256k1fx::OutputOwners;
use ava_types::id::Id;

use super::Context;
use super::backend::Backend as StateBackend;
use super::builder::{Builder, XBuilder};
use super::signer::{SignedTx, Signer};
use crate::client::XChainClient;
use crate::common::utxos::{UtxoStore, avm_output_to_p};
use crate::error::{Error, Result};
use crate::keychain::Keychain;
use crate::options::{Options, TxOption, union_options};
use crate::p::PLATFORM_CHAIN_ID;

/// `x.Backend` — the X wallet's view of the (shared) cross-chain UTXO store,
/// updated on every accepted tx (`backend.AcceptTx`).
pub struct Backend {
    chain_id: Id,
    utxos: Arc<UtxoStore>,
}

impl Backend {
    /// `x.NewBackend(context, chainUTXOs)`.
    #[must_use]
    pub fn new(chain_id: Id, utxos: Arc<UtxoStore>) -> Self {
        Self { chain_id, utxos }
    }

    /// `Backend.AcceptTx` — removes the consumed UTXOs (local + imported) and
    /// adds the produced ones (exported UTXOs land in the destination chain's
    /// view of the shared store).
    ///
    /// # Errors
    /// [`Error::UnknownOutputType`] if an output exported to the P-chain is
    /// not a transfer output; [`Error::Overflow`] on an output-index overflow.
    pub fn accept_tx(&self, tx: &SignedTx) -> Result<()> {
        let tx_id = tx.tx_id;
        match &tx.unsigned {
            UnsignedTx::Import(utx) => {
                for input in &utx.imported_ins {
                    self.utxos
                        .remove_xc(utx.source_chain, self.chain_id, input.input_id());
                }
            }
            UnsignedTx::Export(utx) => {
                let base_outs = tx.unsigned.outputs().len();
                for (i, out) in utx.exported_outs.iter().enumerate() {
                    let output_index = base_outs
                        .checked_add(i)
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or(Error::Overflow)?;
                    self.add_outbound(utx.destination_chain, tx_id, output_index, out)?;
                }
            }
            UnsignedTx::Base(_) | UnsignedTx::CreateAsset(_) | UnsignedTx::Operation(_) => {}
        }

        // Remove the consumed local inputs and add every produced output
        // (`tx.UTXOs()`); Go also re-attempts the imported inputs against the
        // local view, a no-op.
        for input in tx.unsigned.inputs() {
            self.utxos
                .remove_xc(self.chain_id, self.chain_id, input.input_id());
        }
        for (i, out) in tx.unsigned.outputs().iter().enumerate() {
            let output_index = u32::try_from(i).map_err(|_| Error::Overflow)?;
            self.utxos.add_xc(
                self.chain_id,
                self.chain_id,
                Utxo {
                    tx_id,
                    output_index,
                    asset_id: out.asset_id,
                    out: out.out.clone(),
                },
            );
        }
        Ok(())
    }

    /// Adds an exported UTXO under `(source = X, destination)` — converting to
    /// the platformvm shape when the destination is the P-chain.
    fn add_outbound(
        &self,
        destination_chain_id: Id,
        tx_id: Id,
        output_index: u32,
        out: &TransferableOutput,
    ) -> Result<()> {
        if destination_chain_id == PLATFORM_CHAIN_ID {
            self.utxos.add_p(
                self.chain_id,
                ava_platformvm::utxo::Utxo {
                    tx_id,
                    output_index,
                    asset_id: out.asset_id,
                    out: avm_output_to_p(&out.out)?,
                },
            );
        } else {
            self.utxos.add_xc(
                self.chain_id,
                destination_chain_id,
                Utxo {
                    tx_id,
                    output_index,
                    asset_id: out.asset_id,
                    out: out.out.clone(),
                },
            );
        }
        Ok(())
    }
}

impl StateBackend for Backend {
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo> {
        self.utxos.xc_utxos(source_chain_id, self.chain_id)
    }

    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo> {
        self.utxos.get_xc(source_chain_id, self.chain_id, utxo_id)
    }
}

/// `x.Wallet` — build + sign + issue + record (`wallet.go`).
#[derive(Clone)]
pub struct Wallet {
    client: Arc<dyn XChainClient>,
    backend: Arc<Backend>,
    keychain: Arc<Keychain>,
    context: Context,
    default_options: Vec<TxOption>,
}

impl Wallet {
    /// `x.NewWallet(builder, signer, client, backend)` — the builder/signer
    /// are derived from the keychain + backend on demand.
    #[must_use]
    pub fn new(
        client: Arc<dyn XChainClient>,
        backend: Arc<Backend>,
        keychain: Arc<Keychain>,
        context: Context,
    ) -> Self {
        Self {
            client,
            backend,
            keychain,
            context,
            default_options: Vec::new(),
        }
    }

    /// `x.NewWalletWithOptions` — a wallet that applies `options` before the
    /// per-call options on every operation.
    #[must_use]
    pub fn with_options(mut self, options: Vec<TxOption>) -> Self {
        self.default_options = union_options(&self.default_options, &options);
        self
    }

    /// The wallet's mutable backend (shared UTXO store view).
    #[must_use]
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// `Wallet.Builder()`.
    #[must_use]
    pub fn builder(&self) -> Builder<'_> {
        Builder::new(
            self.keychain.addresses(),
            self.context,
            self.backend.as_ref(),
        )
    }

    /// `Wallet.Signer()`.
    #[must_use]
    pub fn signer(&self) -> Signer<'_> {
        Signer::new(&self.keychain, self.backend.as_ref())
    }

    fn merged(&self, options: &[TxOption]) -> Vec<TxOption> {
        union_options(&self.default_options, options)
    }

    /// `IssueBaseTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_base_tx(
        &self,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_base_tx(outputs, &options)?;
        self.issue_unsigned(UnsignedTx::Base(utx), &options).await
    }

    /// `IssueCreateAssetTx` — `initial_state` maps fx index → initial outputs.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_create_asset_tx(
        &self,
        name: String,
        symbol: String,
        denomination: u8,
        initial_state: BTreeMap<u32, Vec<FxOutput>>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_create_asset_tx(
            name,
            symbol,
            denomination,
            initial_state,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::CreateAsset(utx), &options)
            .await
    }

    /// `IssueImportTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_import_tx(source_chain_id, to, &options)?;
        self.issue_unsigned(UnsignedTx::Import(utx), &options).await
    }

    /// `IssueExportTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_export_tx(destination_chain_id, outputs, &options)?;
        self.issue_unsigned(UnsignedTx::Export(utx), &options).await
    }

    /// `IssueUnsignedTx` — sign then issue.
    ///
    /// # Errors
    /// Sign failures (incl. [`Error::UnsupportedTxType`] for operation txs);
    /// [`Error::Client`] on submit/poll failure.
    pub async fn issue_unsigned_tx(
        &self,
        unsigned: UnsignedTx,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        self.issue_unsigned(unsigned, &options).await
    }

    /// `IssueTx` — submit the signed tx, await acceptance (unless
    /// [`TxOption::AssumeDecided`]) and record it in the backend.
    ///
    /// # Errors
    /// [`Error::Client`] on submit/poll failure; backend recording failures.
    pub async fn issue_tx(&self, tx: &SignedTx, options: &[TxOption]) -> Result<()> {
        let options = self.merged(options);
        self.issue_signed(tx, &options).await
    }

    async fn issue_unsigned(
        &self,
        unsigned: UnsignedTx,
        merged_options: &[TxOption],
    ) -> Result<SignedTx> {
        let tx = self.signer().sign_unsigned(unsigned)?;
        self.issue_signed(&tx, merged_options).await?;
        Ok(tx)
    }

    async fn issue_signed(&self, tx: &SignedTx, merged_options: &[TxOption]) -> Result<()> {
        let ops = Options::new(merged_options);
        let tx_id = self.client.issue_tx(&tx.bytes).await?;
        if !ops.assume_decided() {
            self.client.await_tx_accepted(tx_id).await?;
        }
        self.backend.accept_tx(tx)
    }
}

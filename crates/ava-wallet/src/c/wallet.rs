// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-chain wallet facade — port of `wallet/chain/c/wallet.go` +
//! `backend.go`.
//!
//! [`Wallet::issue_import_tx`] / [`Wallet::issue_export_tx`] = resolve the
//! base fee (the `WithBaseFee` override, else `EstimateBaseFee` over the
//! [`EthClient`] — Go `wallet.baseFee`; the builders take the fee verbatim,
//! see [`crate::c::builder`]) → build → sign → submit over the
//! [`CChainClient`] seam → poll for acceptance (unless
//! [`TxOption::AssumeDecided`]) → record in the [`Backend`]
//! (`backend.AcceptAtomicTx`: consume/produce the shared-store UTXOs and
//! update the tracked EVM accounts).

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use ava_avm::txs::executor::semantic::Utxo;
use ava_evm::atomic::tx::{AtomicTx, X2C_RATE};
use ava_secp256k1fx::TransferOutput;
use ava_types::id::Id;

use super::Context;
use super::backend::Backend as StateBackend;
use super::builder::{Builder, CBuilder};
use super::signer::{SignedTx, Signer};
use crate::client::{CChainClient, EthClient};
use crate::common::utxos::{UtxoStore, avm_output_to_p};
use crate::error::{Error, Result};
use crate::keychain::Keychain;
use crate::options::{Options, TxOption, union_options};
use crate::p::PLATFORM_CHAIN_ID;

/// `c.Account` — the tracked EVM account state (`backend.go`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// The balance in wei.
    pub balance: u128,
    /// The nonce.
    pub nonce: u64,
}

/// `c.Backend` — the C wallet's view of the (shared) cross-chain UTXO store
/// plus the tracked EVM accounts, updated on every accepted atomic tx
/// (`backend.AcceptAtomicTx`).
pub struct Backend {
    chain_id: Id,
    utxos: Arc<UtxoStore>,
    accounts: RwLock<BTreeMap<[u8; 20], Account>>,
}

impl Backend {
    /// `c.NewBackend(chainUTXOs, accounts)`.
    #[must_use]
    pub fn new(chain_id: Id, utxos: Arc<UtxoStore>, accounts: BTreeMap<[u8; 20], Account>) -> Self {
        Self {
            chain_id,
            utxos,
            accounts: RwLock::new(accounts),
        }
    }

    /// `Backend.AcceptAtomicTx` — an import removes the consumed shared-store
    /// UTXOs and credits the tracked accounts (`amount × 10⁹` wei); an export
    /// adds the exported UTXOs into the destination chain's view, debits the
    /// tracked accounts and advances their nonces.
    ///
    /// # Errors
    /// [`Error::Overflow`] on balance/nonce arithmetic overflow;
    /// [`Error::InsufficientEthBalance`] if an export debit exceeds a tracked
    /// balance; [`Error::UnknownOutputType`] if an output exported to the
    /// P-chain is not a transfer output.
    pub fn accept_atomic_tx(&self, tx: &SignedTx) -> Result<()> {
        match &tx.unsigned {
            AtomicTx::Import(utx) => {
                for input in &utx.imported_inputs {
                    self.utxos
                        .remove_xc(utx.source_chain, self.chain_id, input.input_id());
                }
                let mut accounts = self
                    .accounts
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                for output in &utx.outs {
                    let Some(account) = accounts.get_mut(&output.address) else {
                        continue;
                    };
                    let credit = u128::from(output.amount)
                        .checked_mul(u128::from(X2C_RATE))
                        .ok_or(Error::Overflow)?;
                    account.balance = account.balance.checked_add(credit).ok_or(Error::Overflow)?;
                }
            }
            AtomicTx::Export(utx) => {
                for (i, out) in utx.exported_outputs.iter().enumerate() {
                    let output_index = u32::try_from(i).map_err(|_| Error::Overflow)?;
                    if utx.destination_chain == PLATFORM_CHAIN_ID {
                        self.utxos.add_p(
                            self.chain_id,
                            ava_platformvm::utxo::Utxo {
                                tx_id: tx.tx_id,
                                output_index,
                                asset_id: out.asset_id,
                                out: avm_output_to_p(&out.out)?,
                            },
                        );
                    } else {
                        self.utxos.add_xc(
                            self.chain_id,
                            utx.destination_chain,
                            Utxo {
                                tx_id: tx.tx_id,
                                output_index,
                                asset_id: out.asset_id,
                                out: out.out.clone(),
                            },
                        );
                    }
                }
                let mut accounts = self
                    .accounts
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                for input in &utx.ins {
                    let Some(account) = accounts.get_mut(&input.address) else {
                        continue;
                    };
                    let debit = u128::from(input.amount)
                        .checked_mul(u128::from(X2C_RATE))
                        .ok_or(Error::Overflow)?;
                    account.balance = account
                        .balance
                        .checked_sub(debit)
                        .ok_or(Error::InsufficientEthBalance)?;
                    account.nonce = input.nonce.checked_add(1).ok_or(Error::Overflow)?;
                }
            }
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

    fn balance(&self, addr: &[u8; 20]) -> u128 {
        self.accounts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(addr)
            .map(|a| a.balance)
            .unwrap_or_default()
    }

    fn nonce(&self, addr: &[u8; 20]) -> u64 {
        self.accounts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(addr)
            .map(|a| a.nonce)
            .unwrap_or_default()
    }
}

/// `c.Wallet` — build + sign + issue + record (`wallet.go`).
#[derive(Clone)]
pub struct Wallet {
    avax_client: Arc<dyn CChainClient>,
    eth_client: Arc<dyn EthClient>,
    backend: Arc<Backend>,
    keychain: Arc<Keychain>,
    context: Context,
    default_options: Vec<TxOption>,
}

impl Wallet {
    /// `c.NewWallet(builder, signer, avaxClient, ethClient, backend)` — the
    /// builder/signer are derived from the keychain + backend on demand.
    #[must_use]
    pub fn new(
        avax_client: Arc<dyn CChainClient>,
        eth_client: Arc<dyn EthClient>,
        backend: Arc<Backend>,
        keychain: Arc<Keychain>,
        context: Context,
    ) -> Self {
        Self {
            avax_client,
            eth_client,
            backend,
            keychain,
            context,
            default_options: Vec::new(),
        }
    }

    /// `c.NewWalletWithOptions` — a wallet that applies `options` before the
    /// per-call options on every operation.
    #[must_use]
    pub fn with_options(mut self, options: Vec<TxOption>) -> Self {
        self.default_options = union_options(&self.default_options, &options);
        self
    }

    /// The wallet's mutable backend (shared UTXO store view + accounts).
    #[must_use]
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// `Wallet.Builder()`.
    #[must_use]
    pub fn builder(&self) -> Builder<'_> {
        Builder::new(
            self.keychain.addresses(),
            self.keychain.eth_addresses(),
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

    /// `IssueImportTx` — resolves the base fee, then builds, signs, issues
    /// and records.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on fee-estimation/submit/poll
    /// failure.
    pub async fn issue_import_tx(
        &self,
        source_chain_id: Id,
        to: [u8; 20],
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let base_fee = self.base_fee(&options).await?;
        let utx = self
            .builder()
            .new_import_tx(source_chain_id, to, base_fee, &options)?;
        self.issue_unsigned(AtomicTx::Import(utx), &options).await
    }

    /// `IssueExportTx` — resolves the base fee, then builds, signs, issues
    /// and records.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on fee-estimation/submit/poll
    /// failure.
    pub async fn issue_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferOutput>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let base_fee = self.base_fee(&options).await?;
        let utx =
            self.builder()
                .new_export_tx(destination_chain_id, outputs, base_fee, &options)?;
        self.issue_unsigned(AtomicTx::Export(utx), &options).await
    }

    /// `IssueUnsignedAtomicTx` — sign then issue.
    ///
    /// # Errors
    /// Sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_unsigned_atomic_tx(
        &self,
        unsigned: AtomicTx,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        self.issue_unsigned(unsigned, &options).await
    }

    /// `IssueAtomicTx` — submit the signed tx, await acceptance (unless
    /// [`TxOption::AssumeDecided`]) and record it in the backend.
    ///
    /// # Errors
    /// [`Error::Client`] on submit/poll failure; backend recording failures.
    pub async fn issue_atomic_tx(&self, tx: &SignedTx, options: &[TxOption]) -> Result<()> {
        let options = self.merged(options);
        self.issue_signed(tx, &options).await
    }

    /// `wallet.baseFee` — the `WithBaseFee` override, else `EstimateBaseFee`
    /// over the eth client.
    async fn base_fee(&self, merged_options: &[TxOption]) -> Result<u128> {
        let ops = Options::new(merged_options);
        match ops.base_fee_override() {
            Some(base_fee) => Ok(base_fee),
            None => self.eth_client.estimate_base_fee().await,
        }
    }

    async fn issue_unsigned(
        &self,
        unsigned: AtomicTx,
        merged_options: &[TxOption],
    ) -> Result<SignedTx> {
        let tx = self.signer().sign_unsigned_atomic(unsigned)?;
        self.issue_signed(&tx, merged_options).await?;
        Ok(tx)
    }

    async fn issue_signed(&self, tx: &SignedTx, merged_options: &[TxOption]) -> Result<()> {
        let ops = Options::new(merged_options);
        let tx_id = self.avax_client.issue_tx(&tx.bytes).await?;
        if !ops.assume_decided() {
            self.avax_client.await_tx_accepted(tx_id).await?;
        }
        self.backend.accept_atomic_tx(tx)
    }
}

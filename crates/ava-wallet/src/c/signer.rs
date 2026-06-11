// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-chain atomic tx signer — port of `wallet/chain/c/signer.go`.
//!
//! Imports are signed with the AVAX keychain against the source chain's UTXOs
//! (one credential per imported input); exports with the eth keychain (one
//! single-signature credential per EVM input). Signatures are 65-byte
//! recoverable secp256k1 over `sha256(unsigned_bytes)`; missing keys/UTXOs
//! leave their slots zeroed (partial signing).

use std::collections::BTreeMap;

use ava_avm::txs::components::{Input as FxInput, Output as FxOutput};
use ava_avm::txs::credential::{Credential, FxCredential};
use ava_crypto::hashing;
use ava_crypto::secp256k1::SIGNATURE_LEN;
use ava_evm::atomic::tx::{AtomicTx, CODEC_VERSION, Tx, codec};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use super::backend::Backend;
use crate::error::{Error, Result};
use crate::keychain::Keychain;

const EMPTY_SIG: [u8; SIGNATURE_LEN] = [0u8; SIGNATURE_LEN];

/// A signed C-chain atomic tx (`atomic.Tx` after `Initialize`).
pub type SignedTx = Tx;

/// The C-chain signer (Go `c.NewSigner(avaxKC, ethKC, backend)`); the single
/// [`Keychain`] serves both the AVAX- and eth-address lookups.
pub struct Signer<'a> {
    keychain: &'a Keychain,
    backend: &'a dyn Backend,
}

enum Slot {
    Avax(Option<ShortId>),
    Eth(Option<[u8; 20]>),
}

impl<'a> Signer<'a> {
    /// Builds a signer over a keychain + backend snapshot.
    #[must_use]
    pub fn new(keychain: &'a Keychain, backend: &'a dyn Backend) -> Self {
        Self { keychain, backend }
    }

    /// `SignUnsignedAtomic` — signs every input the keychain has keys for and
    /// initializes the canonical signed bytes.
    ///
    /// # Errors
    /// [`Error::InvalidUtxoSigIndex`] on a malformed sig index; codec/crypto
    /// failures.
    pub fn sign_unsigned_atomic(&self, unsigned: AtomicTx) -> Result<SignedTx> {
        let slots = match &unsigned {
            AtomicTx::Import(tx) => self.import_slots(tx)?,
            AtomicTx::Export(tx) => tx
                .ins
                .iter()
                .map(|input| {
                    vec![Slot::Eth(
                        self.keychain.get_eth(&input.address).map(|_| input.address),
                    )]
                })
                .collect(),
        };
        self.sign(unsigned, slots)
    }

    /// `getImportSigners`.
    fn import_slots(&self, tx: &ava_evm::atomic::tx::UnsignedImportTx) -> Result<Vec<Vec<Slot>>> {
        let mut slots = Vec::with_capacity(tx.imported_inputs.len());
        for input in &tx.imported_inputs {
            let FxInput::SecpTransfer(secp_in) = &input.r#in;
            let mut input_slots: Vec<Slot> = (0..secp_in.input.sig_indices.len())
                .map(|_| Slot::Avax(None))
                .collect();

            let utxo_id = input.input_id();
            let Some(utxo) = self.backend.get_utxo(tx.source_chain, utxo_id) else {
                slots.push(input_slots);
                continue;
            };

            let FxOutput::SecpTransfer(out) = &utxo.out else {
                return Err(Error::UnknownOutputType);
            };

            for (slot, &addr_index) in input_slots.iter_mut().zip(&secp_in.input.sig_indices) {
                let addr = out
                    .owners
                    .addrs
                    .get(addr_index as usize)
                    .ok_or(Error::InvalidUtxoSigIndex)?;
                if self.keychain.get(addr).is_some() {
                    *slot = Slot::Avax(Some(*addr));
                }
            }
            slots.push(input_slots);
        }
        Ok(slots)
    }

    fn sign(&self, unsigned: AtomicTx, slots: Vec<Vec<Slot>>) -> Result<SignedTx> {
        let unsigned_bytes = codec().marshal(CODEC_VERSION, &unsigned)?;
        let hash = hashing::sha256(&unsigned_bytes);

        let mut sig_cache: BTreeMap<ShortId, [u8; SIGNATURE_LEN]> = BTreeMap::new();
        let mut creds = Vec::with_capacity(slots.len());
        for input_slots in &slots {
            let mut sigs = vec![EMPTY_SIG; input_slots.len()];
            for (sig, slot) in sigs.iter_mut().zip(input_slots) {
                let key = match slot {
                    Slot::Avax(Some(addr)) => {
                        if let Some(cached) = sig_cache.get(addr) {
                            *sig = *cached;
                            continue;
                        }
                        let Some(key) = self.keychain.get(addr) else {
                            continue;
                        };
                        *sig = key.sign_hash(&hash)?;
                        sig_cache.insert(*addr, *sig);
                        continue;
                    }
                    Slot::Eth(Some(addr)) => self.keychain.get_eth(addr),
                    Slot::Avax(None) | Slot::Eth(None) => None,
                };
                if let Some(key) = key {
                    *sig = key.sign_hash(&hash)?;
                }
            }
            creds.push(FxCredential {
                credential: Credential::Secp256k1(ava_secp256k1fx::Credential::new(sigs)),
                fx_id: Id::EMPTY, // runtime-only; never encoded
            });
        }

        let mut tx = Tx::new(unsigned);
        tx.creds = creds;
        tx.initialize()?;
        Ok(tx)
    }
}

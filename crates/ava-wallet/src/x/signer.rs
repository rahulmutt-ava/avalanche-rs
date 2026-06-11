// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-chain tx signer — port of `wallet/chain/x/signer` (`visitor.go`).
//!
//! One `secp256k1fx.Credential` per transferable input (imports also sign the
//! imported inputs against the source chain), each holding 65-byte recoverable
//! signatures over `sha256(unsigned_bytes)`. Missing keys/UTXOs leave their
//! slots zeroed (partial signing). `OperationTx` signing is deferred with the
//! typed fx-operation types (see [`crate::x::builder`]).

use std::collections::BTreeMap;

use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{Input as FxInput, Output as FxOutput, TransferableInput};
use ava_avm::txs::credential::{Credential, FxCredential};
use ava_avm::txs::{CODEC_VERSION, Tx, UnsignedTx};
use ava_crypto::hashing;
use ava_crypto::secp256k1::SIGNATURE_LEN;
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use super::backend::Backend;
use crate::error::{Error, Result};
use crate::keychain::Keychain;

const EMPTY_SIG: [u8; SIGNATURE_LEN] = [0u8; SIGNATURE_LEN];

/// A signed X-chain tx (`txs.Tx` after `Initialize`).
pub type SignedTx = Tx;

/// The X-chain signer (Go `signer.New(kc, backend)`).
pub struct Signer<'a> {
    keychain: &'a Keychain,
    backend: &'a dyn Backend,
}

impl<'a> Signer<'a> {
    /// Builds a signer over a keychain + backend snapshot.
    #[must_use]
    pub fn new(keychain: &'a Keychain, backend: &'a dyn Backend) -> Self {
        Self { keychain, backend }
    }

    /// `signer.SignUnsigned` — signs every input the keychain has keys for and
    /// initializes the canonical signed bytes.
    ///
    /// # Errors
    /// [`Error::InvalidUtxoSigIndex`] on a malformed sig index,
    /// [`Error::UnsupportedTxType`] for operation txs (typed fx operations are
    /// an M5 follow-up), and codec/crypto failures.
    pub fn sign_unsigned(&self, unsigned: UnsignedTx) -> Result<SignedTx> {
        let slots = match &unsigned {
            UnsignedTx::Base(tx) => self.input_slots(tx.base.blockchain_id, &tx.base.ins)?,
            UnsignedTx::CreateAsset(tx) => {
                self.input_slots(tx.base.base.blockchain_id, &tx.base.base.ins)?
            }
            UnsignedTx::Export(tx) => {
                self.input_slots(tx.base.base.blockchain_id, &tx.base.base.ins)?
            }
            UnsignedTx::Import(tx) => {
                let mut slots = self.input_slots(tx.base.base.blockchain_id, &tx.base.base.ins)?;
                slots.extend(self.input_slots(tx.source_chain, &tx.imported_ins)?);
                slots
            }
            // Typed fx operations (mint/burn) are an M5 §5.5 follow-up; an
            // opaque `FxOperation::Unsupported` cannot expose its sig indices.
            UnsignedTx::Operation(_) => return Err(Error::UnsupportedTxType),
        };
        self.sign(unsigned, slots)
    }

    /// `visitor.getSigners` — the owner address (if known) for each signature
    /// slot of each input.
    fn input_slots(
        &self,
        source_chain_id: Id,
        ins: &[TransferableInput],
    ) -> Result<Vec<Vec<Option<ShortId>>>> {
        let mut slots = Vec::with_capacity(ins.len());
        for input in ins {
            let FxInput::SecpTransfer(secp_in) = &input.r#in;
            let mut input_slots = vec![None; secp_in.input.sig_indices.len()];

            let utxo_id = input.input_id();
            let Some(utxo) = self.backend.get_utxo(source_chain_id, utxo_id) else {
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
                    *slot = Some(*addr);
                }
            }
            slots.push(input_slots);
        }
        Ok(slots)
    }

    fn sign(&self, unsigned: UnsignedTx, slots: Vec<Vec<Option<ShortId>>>) -> Result<SignedTx> {
        let unsigned_bytes = Codec().marshal(CODEC_VERSION, &unsigned)?;
        let hash = hashing::sha256(&unsigned_bytes);

        let mut sig_cache: BTreeMap<ShortId, [u8; SIGNATURE_LEN]> = BTreeMap::new();
        let mut creds = Vec::with_capacity(slots.len());
        for input_slots in &slots {
            let mut sigs = vec![EMPTY_SIG; input_slots.len()];
            for (sig, addr) in sigs.iter_mut().zip(input_slots) {
                let Some(addr) = addr else {
                    continue;
                };
                if let Some(cached) = sig_cache.get(addr) {
                    *sig = *cached;
                    continue;
                }
                let Some(key) = self.keychain.get(addr) else {
                    continue;
                };
                *sig = key.sign_hash(&hash)?;
                sig_cache.insert(*addr, *sig);
            }
            creds.push(FxCredential {
                credential: Credential::Secp256k1(ava_secp256k1fx::Credential::new(sigs)),
                fx_id: Id::EMPTY, // runtime-only; never encoded
            });
        }

        let mut tx = Tx::new(unsigned);
        tx.creds = creds;
        tx.initialize(Codec())?;
        Ok(tx)
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-chain tx signer — port of `wallet/chain/p/signer` (`visitor.go`).
//!
//! Produces one `secp256k1fx.Credential` per transferable input (and one per
//! subnet/validator authorization, appended after the input credentials), each
//! holding a 65-byte recoverable signature over `sha256(unsigned_bytes)`.
//! Missing keys/UTXOs leave their signature slots zeroed (partial signing),
//! exactly like Go.
//!
//! The signed envelope is assembled manually: `unsigned_bytes` (codec version +
//! typeID + body) followed by the credential array, each credential prefixed
//! with its registered `type_id = 9` — byte-identical to Go's
//! `Codec.Marshal(CodecVersion, &tx)`. (`ava_platformvm::txs::tx::Tx` omits the
//! per-credential typeID, so it is not used here; see the M8 porting notes.)

use std::collections::BTreeMap;

use ava_codec::packer::Packer;
use ava_crypto::hashing;
use ava_crypto::secp256k1::SIGNATURE_LEN;
use ava_platformvm::CODEC_VERSION;
use ava_platformvm::txs::components::{
    Auth, Input as FxInput, Output as FxOutput, TransferableInput,
};
use ava_platformvm::txs::{Codec, UnsignedTx};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use super::PLATFORM_CHAIN_ID;
use super::backend::Backend;
use crate::error::{Error, Result};
use crate::keychain::Keychain;

/// `secp256k1fx.Credential`'s registered codec type id.
const CREDENTIAL_TYPE_ID: u32 = 9;

const EMPTY_SIG: [u8; SIGNATURE_LEN] = [0u8; SIGNATURE_LEN];

/// A signed P-chain tx: the unsigned body, its credentials, and the derived
/// caches (`txs.Tx` after `Initialize`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedTx {
    /// The transaction body.
    pub unsigned: UnsignedTx,
    /// One credential (signature list) per input + authorization.
    pub creds: Vec<ava_secp256k1fx::Credential>,
    /// `sha256(signed_bytes)`.
    pub tx_id: Id,
    /// The canonical signed wire bytes.
    pub signed_bytes: Vec<u8>,
    /// The unsigned prefix of [`Self::signed_bytes`] (the signed message).
    pub unsigned_bytes: Vec<u8>,
}

/// The P-chain signer (Go `signer.New(kc, backend)`).
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

    /// `signer.SignUnsigned` — signs every input (and authorization) the
    /// keychain has keys for and assembles the canonical signed bytes.
    ///
    /// # Errors
    /// [`Error::InvalidUtxoSigIndex`] on a malformed sig index,
    /// [`Error::MissingOwner`] on an unknown authorization owner,
    /// [`Error::UnsupportedTxType`] for non-wallet txs, and codec/crypto
    /// failures.
    pub fn sign_unsigned(&self, unsigned: UnsignedTx) -> Result<SignedTx> {
        let signer_slots = self.signer_slots(&unsigned)?;
        self.sign(unsigned, signer_slots)
    }

    /// The per-credential signer addresses for the tx (Go `visitor`): one slot
    /// list per input credential, then the auth credential (if any).
    fn signer_slots(&self, unsigned: &UnsignedTx) -> Result<Vec<Vec<Option<ShortId>>>> {
        let ins =
            |tx: &ava_platformvm::txs::components::BaseTx| -> Result<Vec<Vec<Option<ShortId>>>> {
                self.input_slots(PLATFORM_CHAIN_ID, &tx.ins)
            };

        match unsigned {
            UnsignedTx::Base(tx) => ins(&tx.base),
            UnsignedTx::AddValidator(tx) => ins(&tx.base.base),
            UnsignedTx::AddDelegator(tx) => ins(&tx.base.base),
            UnsignedTx::CreateSubnet(tx) => ins(&tx.base.base),
            UnsignedTx::Export(tx) => ins(&tx.base.base),
            UnsignedTx::AddPermissionlessValidator(tx) => ins(&tx.base.base),
            UnsignedTx::AddPermissionlessDelegator(tx) => ins(&tx.base.base),
            UnsignedTx::RegisterL1Validator(tx) => ins(&tx.base.base),
            UnsignedTx::SetL1ValidatorWeight(tx) => ins(&tx.base.base),
            UnsignedTx::IncreaseL1ValidatorBalance(tx) => ins(&tx.base.base),
            UnsignedTx::AddAutoRenewedValidator(tx) => ins(&tx.base.base),
            UnsignedTx::AddSubnetValidator(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet_validator.subnet, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::CreateChain(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet_id, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::RemoveSubnetValidator(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::TransformSubnet(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::TransferSubnetOwnership(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::ConvertSubnetToL1(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.subnet, &tx.subnet_auth)?);
                Ok(slots)
            }
            UnsignedTx::DisableL1Validator(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.validation_id, &tx.disable_auth)?);
                Ok(slots)
            }
            UnsignedTx::SetAutoRenewedValidatorConfig(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.push(self.auth_slots(tx.tx_id, &tx.auth)?);
                Ok(slots)
            }
            UnsignedTx::Import(tx) => {
                let mut slots = ins(&tx.base.base)?;
                slots.extend(self.input_slots(tx.source_chain, &tx.imported_inputs)?);
                Ok(slots)
            }
            UnsignedTx::AdvanceTime(_)
            | UnsignedTx::RewardValidator(_)
            | UnsignedTx::RewardAutoRenewedValidator(_) => Err(Error::UnsupportedTxType),
        }
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
            let secp_in = match &input.r#in {
                FxInput::Transfer(i) => i,
                FxInput::StakeableLock(lock) => match lock.transferable_in.as_ref() {
                    FxInput::Transfer(i) => i,
                    FxInput::StakeableLock(_) => return Err(Error::UnknownOutputType),
                },
            };

            let mut input_slots = vec![None; secp_in.input.sig_indices.len()];

            let utxo_id = input.input_id();
            let Some(utxo) = self.backend.get_utxo(source_chain_id, utxo_id) else {
                // We can't sign this input, but may partially sign the tx.
                slots.push(input_slots);
                continue;
            };

            let out = match &utxo.out {
                FxOutput::Transfer(o) => o,
                FxOutput::StakeableLock(lock) => match lock.transferable_out.as_ref() {
                    FxOutput::Transfer(o) => o,
                    FxOutput::StakeableLock(_) => return Err(Error::UnknownOutputType),
                },
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

    /// `visitor.getAuthSigners`.
    fn auth_slots(&self, owner_id: Id, auth: &Auth) -> Result<Vec<Option<ShortId>>> {
        let Auth::Secp256k1(input) = auth;
        let owner = self
            .backend
            .get_owner(owner_id)
            .ok_or(Error::MissingOwner(owner_id))?;

        let mut slots = vec![None; input.sig_indices.len()];
        for (slot, &addr_index) in slots.iter_mut().zip(&input.sig_indices) {
            let addr = owner
                .addrs
                .get(addr_index as usize)
                .ok_or(Error::InvalidUtxoSigIndex)?;
            if self.keychain.get(addr).is_some() {
                *slot = Some(*addr);
            }
        }
        Ok(slots)
    }

    /// `visitor sign()` — produce the credentials and assemble the signed
    /// bytes.
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
            creds.push(ava_secp256k1fx::Credential::new(sigs));
        }

        Ok(assemble_signed(unsigned, unsigned_bytes, creds))
    }
}

/// Appends the credential array (each with its `type_id = 9` prefix) to the
/// unsigned bytes — Go `Codec.Marshal(CodecVersion, &tx)` + `tx.SetBytes`.
fn assemble_signed(
    unsigned: UnsignedTx,
    unsigned_bytes: Vec<u8>,
    creds: Vec<ava_secp256k1fx::Credential>,
) -> SignedTx {
    let mut p = Packer::with_max_size(usize::MAX);
    p.pack_fixed_bytes(&unsigned_bytes);
    ava_codec::pack_count(&mut p, creds.len());
    for cred in &creds {
        p.pack_u32(CREDENTIAL_TYPE_ID);
        ava_codec::pack_count(&mut p, cred.sigs.len());
        for sig in &cred.sigs {
            p.pack_fixed_bytes(sig);
        }
    }
    let signed_bytes = p.into_bytes();
    let tx_id = Id::from(hashing::sha256(&signed_bytes));
    SignedTx {
        unsigned,
        creds,
        tx_id,
        signed_bytes,
        unsigned_bytes,
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-Chain `fx.Owner` interface + the spend gate glue (`vms/platformvm/fx`,
//! specs 08 §1, 07 §4).
//!
//! Go's `fx.Owner` is `verify.Verifiable + IsNotState + ContextInitializable`;
//! on the P-Chain the only concrete owner is [`ava_secp256k1fx::OutputOwners`]
//! (registered type_id 11). [`Owner`] re-exports the concrete type and exposes
//! the byte-exact owner-id derivation used for the locked-funds accounting in the
//! UTXO handler. [`verify_transfer`] wraps `secp256k1fx.Fx.VerifyTransfer` (the
//! multisig spend gate, [`ava_secp256k1fx::Fx::verify_credentials`]) so the
//! handler can authorize an input's credential against the consumed output's
//! owners.

use ava_secp256k1fx::{Credential, Fx, Input as Secp256k1Input, OutputOwners};
use ava_vm::fx::UnsignedTx;

use crate::error::{Error, Result};

/// `fx.Owner` — the P-Chain reward/subnet owner interface.
///
/// The only concrete owner on the P-Chain is `secp256k1fx.OutputOwners`
/// (type_id 11); this re-export documents that binding and the canonical owner
/// id used for locked-funds accounting.
pub type Owner = OutputOwners;

/// The canonical owner id — `sha256(codec.Marshal(0, owner))` — keying the
/// locked-funds maps in the spend handler (Go `hashing.ComputeHash256Array` over
/// `txs.Codec.Marshal(txs.CodecVersion, owner)`).
///
/// The owner is marshaled through the P-Chain [`Owner`](crate::txs::components::Owner)
/// interface enum (type_id 11 + the `OutputOwners` fields) under the shared
/// codec, so the id is identical across P/X/C (ATOMIC-1).
///
/// # Errors
/// Returns [`Error::Codec`] if the owner cannot be marshaled.
pub fn owner_id(owner: &OutputOwners) -> Result<ava_types::id::Id> {
    let wrapped = crate::txs::components::Owner::Secp256k1(owner.clone());
    let bytes = crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &wrapped)
        .map_err(Error::Codec)?;
    Ok(ava_types::id::Id::from(ava_crypto::hashing::sha256(&bytes)))
}

/// `secp256k1fx.Fx.VerifyTransfer(tx, in, cred, out)` — `Ok(())` iff `cred`
/// proves the consumed output's `owners` assent to spending under `input`
/// (specs 07 §4.3).
///
/// # Errors
/// Propagates the fx spend-gate sentinel, mapped into [`Error::InvalidComponent`]
/// (the P-Chain error model surfaces fx failures as a single component error).
pub fn verify_transfer(
    fx: &Fx,
    tx: &dyn UnsignedTx,
    input: &Secp256k1Input,
    cred: &Credential,
    owners: &OutputOwners,
) -> Result<()> {
    fx.verify_credentials(tx, input, cred, owners)
        .map_err(|_| Error::InvalidComponent)
}

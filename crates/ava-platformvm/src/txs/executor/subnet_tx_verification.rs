// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Subnet/owner authorization verification for the tx executors
//! (`vms/platformvm/txs/executor/subnet_tx_verification.go`, specs 08 §2.4).
//!
//! A subnet-modifying tx (`CreateChainTx`, `AddSubnetValidatorTx`,
//! `RemoveSubnetValidatorTx`, `TransferSubnetOwnershipTx`, `TransformSubnetTx`,
//! `ConvertSubnetToL1Tx`) carries a `subnet_auth` credential proving the issuer
//! controls the subnet's current owner. The last credential in the signed tx is
//! consumed as the authorization; the rest authorize the base-tx inputs.
//!
//! These helpers are `pub(crate)` so the M4.17/M4.18/M4.19 sibling executors can
//! reuse them without editing the standard executor.

use ava_secp256k1fx::{Credential as FxCredential, Input as FxInput, OutputOwners};
use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::fx;
use crate::state::chain::Chain;
use crate::txs::components::Auth;
use crate::txs::tx::Tx;

use super::backend::Backend;

/// `verifyAuthorization` — verifies that the **last** credential in `tx`
/// authorizes spending under `owner`, using `auth` as the sig-index set.
/// Returns the remaining (base-tx) credentials.
///
/// The owner is the decoded `secp256k1fx.OutputOwners`; `auth` is the
/// `secp256k1fx.Input` sig-index set from the tx's `subnet_auth`/`disable_auth`
/// field. The fx hashes the **unsigned** tx bytes (Go `fx.VerifyPermission(
/// tx.Unsigned, …)`), so `unsigned_bytes` must be the marshaled unsigned tx.
///
/// # Errors
/// - [`Error::WrongNumberOfCredentials`] if `tx` has no credentials.
/// - [`Error::UnauthorizedModification`] if the credential fails to prove
///   control of `owner`.
pub(crate) fn verify_authorization<'a>(
    backend: &Backend,
    tx: &'a Tx,
    unsigned_bytes: &[u8],
    owner: &OutputOwners,
    auth: &Auth,
) -> Result<&'a [crate::txs::tx::Credential]> {
    if tx.creds.is_empty() {
        // Ensure there is at least one credential for the authorization.
        return Err(Error::WrongNumberOfCredentials);
    }

    let base_creds_len = tx.creds.len().saturating_sub(1);
    let auth_cred = tx
        .creds
        .get(base_creds_len)
        .ok_or(Error::WrongNumberOfCredentials)?;

    let Auth::Secp256k1(input) = auth;
    let fx_input: FxInput = input.clone();
    let fx_cred: FxCredential = auth_cred.clone().into();

    // The fx tx-bytes boundary is `&[u8]` (an `ava_vm::fx::UnsignedTx` blanket
    // impl); pass the unsigned-tx bytes so the recovered signer hashes match Go.
    fx::verify_transfer(&backend.fx, &unsigned_bytes, &fx_input, &fx_cred, owner)
        .map_err(|_| Error::UnauthorizedModification)?;

    Ok(tx.creds.get(..base_creds_len).unwrap_or(&[]))
}

/// `verifySubnetAuthorization` — resolves `subnet_id`'s current owner from
/// `chain` and verifies `auth` against it (via [`verify_authorization`]).
/// Returns the remaining (base-tx) credentials.
///
/// # Errors
/// - [`Error::Database`] if the subnet owner is not recorded.
/// - [`Error::Codec`] if the stored owner bytes are malformed.
/// - Propagates [`verify_authorization`]'s failures.
pub(crate) fn verify_subnet_authorization<'a>(
    backend: &Backend,
    chain: &dyn Chain,
    tx: &'a Tx,
    unsigned_bytes: &[u8],
    subnet_id: Id,
    auth: &Auth,
) -> Result<&'a [crate::txs::tx::Credential]> {
    let owner_bytes = chain.get_subnet_owner(subnet_id)?;
    let owner = decode_owner(&owner_bytes)?;
    verify_authorization(backend, tx, unsigned_bytes, &owner, auth)
}

/// Decodes the persisted subnet-owner bytes (a codec-marshaled
/// `fx.Owner` / `secp256k1fx.OutputOwners`, type_id 11) into the concrete owner.
///
/// # Errors
/// Returns [`Error::Codec`] on malformed owner bytes.
pub(crate) fn decode_owner(bytes: &[u8]) -> Result<OutputOwners> {
    let mut owner = crate::txs::components::Owner::default();
    crate::txs::codec::Codec()
        .unmarshal(bytes, &mut owner)
        .map_err(Error::Codec)?;
    let crate::txs::components::Owner::Secp256k1(o) = owner;
    Ok(o)
}

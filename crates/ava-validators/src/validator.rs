// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Validator` and its public projection `GetValidatorOutput`.
//!
//! Port of `snow/validators/validator.go` and the `GetValidatorOutput` struct in
//! `snow/validators/state.go`. The BLS public key comes from
//! [`ava_crypto::bls::PublicKey`]; it derives only `Clone` (no `PartialEq`), so
//! these structs intentionally do not derive `Eq`.

use ava_crypto::bls::PublicKey;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

/// A single validator's full record within a subnet (Go `validators.Validator`).
#[derive(Clone)]
pub struct Validator {
    /// The validating node's id.
    pub node_id: NodeId,
    /// The node's BLS public key, or `None` if it registered without one.
    pub public_key: Option<PublicKey>,
    /// The transaction id that added this validator (the staking tx).
    pub tx_id: Id,
    /// The validator's voting weight (stake).
    pub weight: u64,
}

impl Validator {
    /// Returns the public projection of this validator.
    #[must_use]
    pub fn output(&self) -> GetValidatorOutput {
        GetValidatorOutput {
            node_id: self.node_id,
            public_key: self.public_key.clone(),
            weight: self.weight,
        }
    }
}

/// The public projection of a [`Validator`] exposed by the validator state
/// (Go `validators.GetValidatorOutput`). Drops the staking `tx_id`.
#[derive(Clone)]
pub struct GetValidatorOutput {
    /// The validating node's id.
    pub node_id: NodeId,
    /// The node's BLS public key, or `None`.
    pub public_key: Option<PublicKey>,
    /// The validator's voting weight (stake).
    pub weight: u64,
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.Validator` / `txs.SubnetValidator` — the staker descriptor embedded in
//! staking txs (specs 08 §2.2).

use ava_codec::AvaCodec;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::Error;

/// `txs.Validator` — node id + staking window + weight.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Validator {
    /// Node ID of the validator.
    #[codec]
    pub node_id: NodeId,
    /// Unix time this validator starts validating.
    #[codec]
    pub start: u64,
    /// Unix time this validator stops validating.
    #[codec]
    pub end: u64,
    /// Weight of this validator used when sampling.
    #[codec]
    pub wght: u64,
}

impl Validator {
    /// `Validator.Verify` — the weight must be non-zero.
    ///
    /// # Errors
    /// Returns [`Error::WeightTooSmall`] if the weight is zero.
    pub fn verify(&self) -> Result<(), Error> {
        if self.wght == 0 {
            return Err(Error::WeightTooSmall);
        }
        Ok(())
    }
}

/// `txs.SubnetValidator` — a [`Validator`] bound to a subnet.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SubnetValidator {
    /// The embedded validator descriptor.
    #[codec]
    pub validator: Validator,
    /// ID of the subnet this validator is validating.
    #[codec]
    pub subnet: Id,
}

impl SubnetValidator {
    /// `SubnetValidator.Verify` — the subnet must not be the Primary Network,
    /// then the embedded validator verifies.
    ///
    /// # Errors
    /// Returns [`Error::BadSubnetId`] for the Primary Network subnet, else
    /// propagates [`Validator::verify`].
    pub fn verify(&self) -> Result<(), Error> {
        if self.subnet == Id::EMPTY {
            return Err(Error::BadSubnetId);
        }
        self.validator.verify()
    }
}

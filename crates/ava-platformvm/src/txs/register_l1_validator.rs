// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.RegisterL1ValidatorTx` (type_id 36) — register an L1 validator from a
//! signed Warp `RegisterL1Validator` message (specs 08 §2.2, §6).

use ava_codec::AvaCodec;

use crate::signer::SIGNATURE_LEN;
use crate::txs::base_tx::BaseTx;

/// `txs.RegisterL1ValidatorTx`.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
pub struct RegisterL1ValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Balance funding this validator's continuous fee.
    #[codec]
    pub balance: u64,
    /// Proof of possession of the BLS key included in `message`.
    #[codec]
    pub proof_of_possession: [u8; SIGNATURE_LEN],
    /// The signed Warp `RegisterL1Validator` message (raw bytes).
    #[codec]
    pub message: Vec<u8>,
}

impl Default for RegisterL1ValidatorTx {
    fn default() -> Self {
        Self {
            base: BaseTx::default(),
            balance: 0,
            proof_of_possession: [0u8; SIGNATURE_LEN],
            message: Vec::new(),
        }
    }
}

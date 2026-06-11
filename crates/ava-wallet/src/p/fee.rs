// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Post-Etna (ACP-103) tx complexity — port of `vms/platformvm/txs/fee/complexity.go`.
//!
//! The P-chain builder prices a tx incrementally while selecting UTXOs: it
//! starts from the per-tx-type *intrinsic* dimensions, adds the memo /
//! owner / auth / signer / warp dimensions of the concrete arguments, then adds
//! input/output dimensions per consumed UTXO and produced output
//! (`spendHelper`). The constants here are protocol values; the M4 fee port
//! (`ava_platformvm::txs::fee::complexity`) only carries the base-tx intrinsic,
//! so the full per-component table lives here until the executor needs it.

use ava_platformvm::signer::Signer;
use ava_platformvm::txs::ConvertSubnetToL1Validator;
use ava_platformvm::txs::components::{Input, Output, TransferableInput, TransferableOutput};
use ava_platformvm::txs::fee::gas::{self, Dimensions};
use ava_secp256k1fx::{Input as SecpInput, OutputOwners};

use crate::error::{Error, Result};

const INT_LEN: u64 = 4;
const SHORT_LEN: u64 = 2;
const LONG_LEN: u64 = 8;
const ID_LEN: u64 = 32;
const NODE_ID_LEN: u64 = 20;
const SHORT_ID_LEN: u64 = 20;
const CODEC_VERSION_LEN: u64 = 2;
const BLS_PUBLIC_KEY_LEN: u64 = 48;
const BLS_SIGNATURE_LEN: u64 = 96;
const SECP_SIGNATURE_LEN: u64 = 65;

const INTRINSIC_VALIDATOR_BANDWIDTH: u64 = NODE_ID_LEN + 3 * LONG_LEN; // nodeID + start + end + weight
const INTRINSIC_SUBNET_VALIDATOR_BANDWIDTH: u64 = INTRINSIC_VALIDATOR_BANDWIDTH + ID_LEN;
const INTRINSIC_OUTPUT_BANDWIDTH: u64 = ID_LEN + INT_LEN; // assetID + output typeID
const INTRINSIC_STAKEABLE_LOCKED_OUTPUT_BANDWIDTH: u64 = LONG_LEN + INT_LEN; // locktime + typeID
const INTRINSIC_SECP_OUTPUT_OWNERS_BANDWIDTH: u64 = LONG_LEN + INT_LEN + INT_LEN; // locktime + threshold + num addrs
const INTRINSIC_SECP_OUTPUT_BANDWIDTH: u64 = LONG_LEN + INTRINSIC_SECP_OUTPUT_OWNERS_BANDWIDTH;
const INTRINSIC_INPUT_BANDWIDTH: u64 = ID_LEN + INT_LEN + ID_LEN + INT_LEN + INT_LEN; // txID + index + assetID + input typeID + cred typeID
const INTRINSIC_STAKEABLE_LOCKED_INPUT_BANDWIDTH: u64 = LONG_LEN + INT_LEN; // locktime + typeID
const INTRINSIC_SECP_INPUT_BANDWIDTH: u64 = INT_LEN + INT_LEN; // num indices + num signatures
const INTRINSIC_SECP_TRANSFERABLE_INPUT_BANDWIDTH: u64 = LONG_LEN + INTRINSIC_SECP_INPUT_BANDWIDTH;
const INTRINSIC_SECP_SIGNATURE_BANDWIDTH: u64 = INT_LEN + SECP_SIGNATURE_LEN; // sig index + sig
const INTRINSIC_SECP_SIGNATURE_COMPUTE: u64 = 200;

const INTRINSIC_CONVERT_SUBNET_TO_L1_VALIDATOR_BANDWIDTH: u64 =
    INT_LEN + LONG_LEN + LONG_LEN + INT_LEN + INT_LEN + INT_LEN + INT_LEN;

const INTRINSIC_BLS_AGGREGATE_COMPUTE: u64 = 5;
const INTRINSIC_BLS_VERIFY_COMPUTE: u64 = 1_000;
const INTRINSIC_BLS_PUBLIC_KEY_VALIDATION_COMPUTE: u64 = 50;
const INTRINSIC_BLS_POP_VERIFY_COMPUTE: u64 =
    INTRINSIC_BLS_PUBLIC_KEY_VALIDATION_COMPUTE + INTRINSIC_BLS_VERIFY_COMPUTE;

const INTRINSIC_WARP_DB_READS: u64 = 3 + 20;
const INTRINSIC_POP_BANDWIDTH: u64 = BLS_PUBLIC_KEY_LEN + BLS_SIGNATURE_LEN;

const INTRINSIC_INPUT_DB_READ: u64 = 1;
const INTRINSIC_INPUT_DB_WRITE: u64 = 1;
const INTRINSIC_OUTPUT_DB_WRITE: u64 = 1;
const INTRINSIC_CONVERT_SUBNET_TO_L1_VALIDATOR_DB_WRITE: u64 = 4;

/// `IntrinsicBaseTxComplexities[Bandwidth]`.
const BASE_TX_BANDWIDTH: u64 =
    CODEC_VERSION_LEN + INT_LEN + INT_LEN + ID_LEN + INT_LEN + INT_LEN + INT_LEN + INT_LEN;

/// `IntrinsicBaseTxComplexities`.
pub const INTRINSIC_BASE_TX: Dimensions = [BASE_TX_BANDWIDTH, 0, 0, 0];

/// `IntrinsicAddSubnetValidatorTxComplexities`.
pub const INTRINSIC_ADD_SUBNET_VALIDATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH + INTRINSIC_SUBNET_VALIDATOR_BANDWIDTH + INT_LEN + INT_LEN,
    3,
    3,
    0,
];

/// `IntrinsicCreateChainTxComplexities`.
pub const INTRINSIC_CREATE_CHAIN_TX: Dimensions = [
    BASE_TX_BANDWIDTH + ID_LEN + SHORT_LEN + ID_LEN + INT_LEN + INT_LEN + INT_LEN + INT_LEN,
    3,
    1,
    0,
];

/// `IntrinsicCreateSubnetTxComplexities`.
pub const INTRINSIC_CREATE_SUBNET_TX: Dimensions = [BASE_TX_BANDWIDTH + INT_LEN, 0, 1, 0];

/// `IntrinsicImportTxComplexities`.
pub const INTRINSIC_IMPORT_TX: Dimensions = [BASE_TX_BANDWIDTH + ID_LEN + INT_LEN, 0, 0, 0];

/// `IntrinsicExportTxComplexities`.
pub const INTRINSIC_EXPORT_TX: Dimensions = [BASE_TX_BANDWIDTH + ID_LEN + INT_LEN, 0, 0, 0];

/// `IntrinsicRemoveSubnetValidatorTxComplexities`.
pub const INTRINSIC_REMOVE_SUBNET_VALIDATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH + NODE_ID_LEN + ID_LEN + INT_LEN + INT_LEN,
    1,
    3,
    0,
];

/// `IntrinsicAddPermissionlessValidatorTxComplexities`.
pub const INTRINSIC_ADD_PERMISSIONLESS_VALIDATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH
        + INTRINSIC_VALIDATOR_BANDWIDTH
        + ID_LEN
        + INT_LEN
        + INT_LEN
        + INT_LEN
        + INT_LEN
        + INT_LEN,
    1,
    3,
    0,
];

/// `IntrinsicAddPermissionlessDelegatorTxComplexities`.
pub const INTRINSIC_ADD_PERMISSIONLESS_DELEGATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH + INTRINSIC_VALIDATOR_BANDWIDTH + ID_LEN + INT_LEN + INT_LEN,
    1,
    2,
    0,
];

/// `IntrinsicTransferSubnetOwnershipTxComplexities`.
pub const INTRINSIC_TRANSFER_SUBNET_OWNERSHIP_TX: Dimensions = [
    BASE_TX_BANDWIDTH + ID_LEN + INT_LEN + INT_LEN + INT_LEN,
    1,
    1,
    0,
];

/// `IntrinsicConvertSubnetToL1TxComplexities`.
pub const INTRINSIC_CONVERT_SUBNET_TO_L1_TX: Dimensions = [
    BASE_TX_BANDWIDTH + ID_LEN + ID_LEN + INT_LEN + INT_LEN + INT_LEN + INT_LEN,
    3,
    2,
    0,
];

/// `IntrinsicRegisterL1ValidatorTxComplexities`.
pub const INTRINSIC_REGISTER_L1_VALIDATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH + LONG_LEN + BLS_SIGNATURE_LEN + INT_LEN,
    5,
    6,
    INTRINSIC_BLS_POP_VERIFY_COMPUTE,
];

/// `IntrinsicSetL1ValidatorWeightTxComplexities`.
pub const INTRINSIC_SET_L1_VALIDATOR_WEIGHT_TX: Dimensions = [BASE_TX_BANDWIDTH + INT_LEN, 3, 5, 0];

/// `IntrinsicIncreaseL1ValidatorBalanceTxComplexities`.
pub const INTRINSIC_INCREASE_L1_VALIDATOR_BALANCE_TX: Dimensions =
    [BASE_TX_BANDWIDTH + ID_LEN + LONG_LEN, 1, 5, 0];

/// `IntrinsicDisableL1ValidatorTxComplexities`.
pub const INTRINSIC_DISABLE_L1_VALIDATOR_TX: Dimensions =
    [BASE_TX_BANDWIDTH + ID_LEN + INT_LEN + INT_LEN, 1, 6, 0];

/// `IntrinsicAddAutoRenewedValidatorTxComplexities` (ACP-236 upstream delta).
pub const INTRINSIC_ADD_AUTO_RENEWED_VALIDATOR_TX: Dimensions = [
    BASE_TX_BANDWIDTH
        + INT_LEN // nodeID length
        + NODE_ID_LEN
        + INT_LEN // signer typeID
        + INT_LEN // num stake outs
        + INT_LEN // validator rewards typeID
        + INT_LEN // delegator rewards typeID
        + INT_LEN // validator authority typeID
        + INT_LEN // delegation shares
        + INT_LEN // auto compound reward shares
        + LONG_LEN, // period
    0,
    3,
    0,
];

/// `IntrinsicSetAutoRenewedValidatorConfigTxComplexities` (ACP-236 upstream
/// delta).
pub const INTRINSIC_SET_AUTO_RENEWED_VALIDATOR_CONFIG_TX: Dimensions = [
    BASE_TX_BANDWIDTH + ID_LEN + INT_LEN + INT_LEN + INT_LEN + LONG_LEN,
    1,
    1,
    0,
];

/// `gas.Dimensions.Add` — element-wise checked sum.
///
/// # Errors
/// [`Error::Overflow`] if any element overflows `u64`.
pub fn add(mut acc: Dimensions, terms: &[Dimensions]) -> Result<Dimensions> {
    for term in terms {
        for (a, t) in acc.iter_mut().zip(term.iter()) {
            *a = a.checked_add(*t).ok_or(Error::Overflow)?;
        }
    }
    Ok(acc)
}

/// The bandwidth-only dimensions of `n` extra bytes (memo, genesis, …).
#[must_use]
pub fn bandwidth(n: usize) -> Dimensions {
    [n as u64, 0, 0, 0]
}

/// `fee.OutputComplexity` — the dimensions outputs add to a tx.
///
/// # Errors
/// [`Error::Overflow`] on arithmetic overflow.
pub fn output_complexity(outs: &[TransferableOutput]) -> Result<Dimensions> {
    let mut complexity = [0u64; 4];
    for out in outs {
        complexity = add(complexity, &[single_output_complexity(out)?])?;
    }
    Ok(complexity)
}

fn single_output_complexity(out: &TransferableOutput) -> Result<Dimensions> {
    let mut bandwidth = INTRINSIC_OUTPUT_BANDWIDTH
        .checked_add(INTRINSIC_SECP_OUTPUT_BANDWIDTH)
        .ok_or(Error::Overflow)?;

    let secp_out = match &out.out {
        Output::Transfer(o) => o,
        Output::StakeableLock(lock) => {
            bandwidth = bandwidth
                .checked_add(INTRINSIC_STAKEABLE_LOCKED_OUTPUT_BANDWIDTH)
                .ok_or(Error::Overflow)?;
            match lock.transferable_out.as_ref() {
                Output::Transfer(o) => o,
                Output::StakeableLock(_) => return Err(Error::UnknownOutputType),
            }
        }
    };

    let addr_bandwidth = (secp_out.owners.addrs.len() as u64)
        .checked_mul(SHORT_ID_LEN)
        .ok_or(Error::Overflow)?;
    bandwidth = bandwidth
        .checked_add(addr_bandwidth)
        .ok_or(Error::Overflow)?;
    Ok([bandwidth, 0, INTRINSIC_OUTPUT_DB_WRITE, 0])
}

/// `fee.InputComplexity` — the dimensions inputs (and their future
/// credentials) add to a tx.
///
/// # Errors
/// [`Error::Overflow`] on arithmetic overflow.
pub fn input_complexity(ins: &[TransferableInput]) -> Result<Dimensions> {
    let mut complexity = [0u64; 4];
    for input in ins {
        complexity = add(complexity, &[single_input_complexity(input)?])?;
    }
    Ok(complexity)
}

fn single_input_complexity(input: &TransferableInput) -> Result<Dimensions> {
    let mut bandwidth = INTRINSIC_INPUT_BANDWIDTH
        .checked_add(INTRINSIC_SECP_TRANSFERABLE_INPUT_BANDWIDTH)
        .ok_or(Error::Overflow)?;

    let secp_in = match &input.r#in {
        Input::Transfer(i) => i,
        Input::StakeableLock(lock) => {
            bandwidth = bandwidth
                .checked_add(INTRINSIC_STAKEABLE_LOCKED_INPUT_BANDWIDTH)
                .ok_or(Error::Overflow)?;
            match lock.transferable_in.as_ref() {
                Input::Transfer(i) => i,
                Input::StakeableLock(_) => return Err(Error::UnknownOutputType),
            }
        }
    };

    let num_signatures = secp_in.input.sig_indices.len() as u64;
    let signature_bandwidth = num_signatures
        .checked_mul(INTRINSIC_SECP_SIGNATURE_BANDWIDTH)
        .ok_or(Error::Overflow)?;
    bandwidth = bandwidth
        .checked_add(signature_bandwidth)
        .ok_or(Error::Overflow)?;
    let compute = num_signatures
        .checked_mul(INTRINSIC_SECP_SIGNATURE_COMPUTE)
        .ok_or(Error::Overflow)?;
    Ok([
        bandwidth,
        INTRINSIC_INPUT_DB_READ,
        INTRINSIC_INPUT_DB_WRITE,
        compute,
    ])
}

/// `fee.OwnerComplexity` — the dimensions an owner adds (excl. its typeID).
///
/// # Errors
/// [`Error::Overflow`] on arithmetic overflow.
pub fn owner_complexity(owner: &OutputOwners) -> Result<Dimensions> {
    let addr_bandwidth = (owner.addrs.len() as u64)
        .checked_mul(SHORT_ID_LEN)
        .ok_or(Error::Overflow)?;
    let bandwidth = addr_bandwidth
        .checked_add(INTRINSIC_SECP_OUTPUT_OWNERS_BANDWIDTH)
        .ok_or(Error::Overflow)?;
    Ok([bandwidth, 0, 0, 0])
}

/// `fee.AuthComplexity` — the dimensions an authorization (and its future
/// credential) adds (excl. typeIDs).
///
/// # Errors
/// [`Error::Overflow`] on arithmetic overflow.
pub fn auth_complexity(auth: &SecpInput) -> Result<Dimensions> {
    let num_signatures = auth.sig_indices.len() as u64;
    let signature_bandwidth = num_signatures
        .checked_mul(INTRINSIC_SECP_SIGNATURE_BANDWIDTH)
        .ok_or(Error::Overflow)?;
    let bandwidth = signature_bandwidth
        .checked_add(INTRINSIC_SECP_INPUT_BANDWIDTH)
        .ok_or(Error::Overflow)?;
    let compute = num_signatures
        .checked_mul(INTRINSIC_SECP_SIGNATURE_COMPUTE)
        .ok_or(Error::Overflow)?;
    Ok([bandwidth, 0, 0, compute])
}

/// `fee.SignerComplexity` — the dimensions a BLS signer adds (excl. typeID).
#[must_use]
pub fn signer_complexity(signer: &Signer) -> Dimensions {
    match signer {
        Signer::Empty(_) => [0, 0, 0, 0],
        Signer::ProofOfPossession(_) => [
            INTRINSIC_POP_BANDWIDTH,
            0,
            0,
            INTRINSIC_BLS_POP_VERIFY_COMPUTE,
        ],
    }
}

/// `fee.ConvertSubnetToL1ValidatorComplexity`.
///
/// # Errors
/// [`Error::Overflow`] on arithmetic overflow.
pub fn convert_subnet_to_l1_validator_complexity(
    validators: &[ConvertSubnetToL1Validator],
) -> Result<Dimensions> {
    let mut complexity = [0u64; 4];
    for v in validators {
        let num_addresses = (v.remaining_balance_owner.addresses.len() as u64)
            .checked_add(v.deactivation_owner.addresses.len() as u64)
            .ok_or(Error::Overflow)?;
        let address_bandwidth = num_addresses
            .checked_mul(SHORT_ID_LEN)
            .ok_or(Error::Overflow)?;
        // PoP signer (always present on a ConvertSubnetToL1Validator).
        let signer = [
            INTRINSIC_POP_BANDWIDTH,
            0,
            0,
            INTRINSIC_BLS_POP_VERIFY_COMPUTE,
        ];
        complexity = add(
            complexity,
            &[
                [
                    INTRINSIC_CONVERT_SUBNET_TO_L1_VALIDATOR_BANDWIDTH,
                    0,
                    INTRINSIC_CONVERT_SUBNET_TO_L1_VALIDATOR_DB_WRITE,
                    0,
                ],
                bandwidth(v.node_id.len()),
                signer,
                [address_bandwidth, 0, 0, 0],
            ],
        )?;
    }
    Ok(complexity)
}

/// `fee.WarpComplexity` — the dimensions a warp message adds.
///
/// # Errors
/// [`Error::Codec`] if the message fails to parse; [`Error::Overflow`] on
/// arithmetic overflow.
pub fn warp_complexity(message: &[u8]) -> Result<Dimensions> {
    let msg = ava_warp::Message::parse(message)?;
    let num_signers: u64 = match &msg.signature {
        ava_warp::Signature::BitSet(sig) => sig
            .signers
            .iter()
            .map(|b| u64::from(b.count_ones() as u8))
            .sum(),
    };
    let aggregation_compute = num_signers
        .checked_mul(INTRINSIC_BLS_AGGREGATE_COMPUTE)
        .ok_or(Error::Overflow)?;
    let compute = aggregation_compute
        .checked_add(INTRINSIC_BLS_VERIFY_COMPUTE)
        .ok_or(Error::Overflow)?;
    Ok([message.len() as u64, INTRINSIC_WARP_DB_READS, 0, compute])
}

/// `spendHelper.calculateFee` — `(complexity · weights) · gas_price`.
///
/// # Errors
/// [`Error::Overflow`] if the dot product or the price multiplication
/// overflows.
pub fn calculate_fee(complexity: Dimensions, weights: Dimensions, gas_price: u64) -> Result<u64> {
    let g = gas::dot_to_gas(complexity, weights).map_err(|_| Error::Overflow)?;
    g.checked_mul(gas_price).ok_or(Error::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The intrinsic tables are protocol constants — pin the Go values.
    #[test]
    fn intrinsic_dimension_goldens() {
        assert_eq!(INTRINSIC_BASE_TX, [58, 0, 0, 0]);
        assert_eq!(INTRINSIC_ADD_SUBNET_VALIDATOR_TX, [142, 3, 3, 0]);
        assert_eq!(INTRINSIC_CREATE_CHAIN_TX, [140, 3, 1, 0]);
        assert_eq!(INTRINSIC_CREATE_SUBNET_TX, [62, 0, 1, 0]);
        assert_eq!(INTRINSIC_IMPORT_TX, [94, 0, 0, 0]);
        assert_eq!(INTRINSIC_EXPORT_TX, [94, 0, 0, 0]);
        assert_eq!(INTRINSIC_REMOVE_SUBNET_VALIDATOR_TX, [118, 1, 3, 0]);
        assert_eq!(INTRINSIC_ADD_PERMISSIONLESS_VALIDATOR_TX, [154, 1, 3, 0]);
        assert_eq!(INTRINSIC_ADD_PERMISSIONLESS_DELEGATOR_TX, [142, 1, 2, 0]);
        assert_eq!(INTRINSIC_TRANSFER_SUBNET_OWNERSHIP_TX, [102, 1, 1, 0]);
        assert_eq!(INTRINSIC_CONVERT_SUBNET_TO_L1_TX, [138, 3, 2, 0]);
        assert_eq!(INTRINSIC_REGISTER_L1_VALIDATOR_TX, [166, 5, 6, 1050]);
        assert_eq!(INTRINSIC_SET_L1_VALIDATOR_WEIGHT_TX, [62, 3, 5, 0]);
        assert_eq!(INTRINSIC_INCREASE_L1_VALIDATOR_BALANCE_TX, [98, 1, 5, 0]);
        assert_eq!(INTRINSIC_DISABLE_L1_VALIDATOR_TX, [98, 1, 6, 0]);
        assert_eq!(INTRINSIC_ADD_AUTO_RENEWED_VALIDATOR_TX, [118, 0, 3, 0]);
        assert_eq!(
            INTRINSIC_SET_AUTO_RENEWED_VALIDATOR_CONFIG_TX,
            [110, 1, 1, 0]
        );
    }
}

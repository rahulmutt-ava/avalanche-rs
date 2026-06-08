// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The pure Warp signature / quorum verification primitives
//! (`vms/platformvm/warp/{signature,validator}.go`, specs 20 §4–§6).
//!
//! These are the **generic** bit-set / quorum checks, reused by every verifying
//! chain (P-Chain, the C-Chain EVM warp precompile, SAE). The L1-lifecycle glue
//! that parses ACP-77 registry payloads stays in the P-Chain
//! (`ava_platformvm::warp::verifier`).
//!
//! - [`verify_bit_set_signature`] is the pure [`BitSetSignature`].Verify
//!   (`vms/platformvm/warp/signature.go`): parse the signer bit-set (enforcing
//!   the no-padding invariant), select the canonical-ordered signers, check the
//!   signing weight meets the quorum of the set's total weight, aggregate the
//!   signers' public keys, and BLS-verify the aggregate over `msg.bytes()`.
//! - [`WarpSetVerifier`] resolves the source chain's subnet and the
//!   height-pinned [`WarpSet`] from a [`ValidatorState`] (specs 20 §6.1), then
//!   calls [`verify_bit_set_signature`].

use ava_crypto::bls;
use ava_utils::bits::Bits;
use ava_validators::state::{ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;

use crate::error::{Error, Result};
use crate::{BitSetSignature, Message, Signature, UnsignedMessage};

/// `WarpQuorumNumerator` — the `67/100` quorum numerator (specs 20 §6).
pub const WARP_QUORUM_NUMERATOR: u64 = 67;
/// `WarpQuorumDenominator` — the `67/100` quorum denominator (specs 20 §6).
pub const WARP_QUORUM_DENOMINATOR: u64 = 100;

/// `warp.FilterValidators` — the canonical-ordered subset of `validators` whose
/// bit is set in `indices` (`vms/platformvm/warp/validator.go`).
///
/// # Errors
/// [`Error::UnknownValidator`] if `indices` references a canonical index past the
/// end of `validators` (`BitLen > len(validators)`).
pub fn filter_validators<'a>(
    indices: &Bits,
    validators: &'a [GetValidatorOutput],
) -> Result<Vec<&'a GetValidatorOutput>> {
    // The highest set bit must be within range (Go: BitLen() > len(vdrs)).
    if indices.bit_len() > validators.len() as u64 {
        return Err(Error::UnknownValidator);
    }
    let mut filtered = Vec::with_capacity(validators.len());
    for (i, vdr) in validators.iter().enumerate() {
        if indices.contains(i as u64) {
            filtered.push(vdr);
        }
    }
    Ok(filtered)
}

/// `warp.SumWeight` — the total weight of `validators`.
///
/// # Errors
/// [`Error::Overflow`] if the summed weight overflows `u64`.
pub fn sum_weight(validators: &[&GetValidatorOutput]) -> Result<u64> {
    let mut weight: u64 = 0;
    for vdr in validators {
        weight = weight.checked_add(vdr.weight).ok_or(Error::Overflow)?;
    }
    Ok(weight)
}

/// `warp.AggregatePublicKeys` — aggregate the (uncompressed) public keys of
/// `validators`. The warp set only contains validators that have a key, so every
/// entry's `public_key` is `Some`.
///
/// # Errors
/// [`Error::InvalidSignature`] if a validator is missing a key or the BLS
/// aggregation fails.
pub fn aggregate_public_keys(validators: &[&GetValidatorOutput]) -> Result<bls::PublicKey> {
    let mut pks = Vec::with_capacity(validators.len());
    for vdr in validators {
        let Some(pk) = &vdr.public_key else {
            return Err(Error::InvalidSignature);
        };
        pks.push(pk);
    }
    bls::aggregate_public_keys(&pks).map_err(|_| Error::InvalidSignature)
}

/// `warp.VerifyWeight` — `Ok` iff `quorum_num * total_weight <= quorum_den *
/// sig_weight` (`vms/platformvm/warp/signature.go`). Uses `u128` math (Go uses
/// `big.Int`; all inputs are `u64` so `u128` cannot overflow).
///
/// # Errors
/// [`Error::InsufficientWeight`] if the signing weight is below quorum.
pub fn verify_weight(
    sig_weight: u64,
    total_weight: u64,
    quorum_num: u64,
    quorum_den: u64,
) -> Result<()> {
    // The product of two `u64` always fits in `u128`, so these multiplications
    // cannot overflow; `checked_mul` keeps the crate's `arithmetic_side_effects`
    // lint satisfied without a raw `*`.
    let scaled_total = u128::from(total_weight)
        .checked_mul(u128::from(quorum_num))
        .ok_or(Error::Overflow)?;
    let scaled_sig = u128::from(sig_weight)
        .checked_mul(u128::from(quorum_den))
        .ok_or(Error::Overflow)?;
    if scaled_total > scaled_sig {
        return Err(Error::InsufficientWeight);
    }
    Ok(())
}

/// `BitSetSignature.Verify` — verify that `msg` was signed by at least
/// `quorum_num`/`quorum_den` of `validators` (the source subnet's canonical warp
/// set at the pinned P-Chain height) (`vms/platformvm/warp/signature.go`).
///
/// # Errors
/// - [`Error::WrongNetworkId`] if `msg.network_id != network_id`.
/// - [`Error::InvalidBitSet`] if the signer bit-set has unnecessary zero-padding.
/// - [`Error::UnknownValidator`] if a bit references a missing canonical index.
/// - [`Error::InsufficientWeight`] if the signing weight is below quorum.
/// - [`Error::ParseSignature`] if the aggregate signature bytes do not parse.
/// - [`Error::InvalidSignature`] if the aggregate BLS check fails.
pub fn verify_bit_set_signature(
    sig: &BitSetSignature,
    msg: &UnsignedMessage,
    network_id: u32,
    validators: &WarpSet,
    quorum_num: u64,
    quorum_den: u64,
) -> Result<()> {
    if msg.network_id != network_id {
        return Err(Error::WrongNetworkId);
    }

    // Parse the signer bit vector, enforcing the no-padding invariant: the
    // big-endian round-trip must reproduce exactly the stored bytes.
    let indices = Bits::from_bytes(&sig.signers);
    if indices.bytes() != sig.signers {
        return Err(Error::InvalidBitSet);
    }

    // Select the (allegedly) signing validators in canonical order.
    let signers = filter_validators(&indices, &validators.validators)?;

    // Quorum check against TOTAL weight (incl. keyless validators).
    let sig_weight = sum_weight(&signers)?;
    verify_weight(sig_weight, validators.total_weight, quorum_num, quorum_den)?;

    // Aggregate pubkey of exactly the selected signers + parse the aggregate sig.
    let agg_pk = aggregate_public_keys(&signers)?;
    let agg_sig = bls::Signature::from_bytes(&sig.signature).map_err(|_| Error::ParseSignature)?;

    // Verify the aggregate BLS signature over the version-prefixed message bytes
    // (signature ciphersuite DST).
    let unsigned_bytes = msg.marshal()?;
    if !bls::verify(&agg_pk, &agg_sig, &unsigned_bytes) {
        return Err(Error::InvalidSignature);
    }
    Ok(())
}

/// Resolves the source subnet's canonical [`WarpSet`] at a pinned P-Chain height
/// from a [`ValidatorState`] (specs 20 §6.1) and runs
/// [`verify_bit_set_signature`].
///
/// The pinned `p_chain_height` comes from the verifying block's proposervm
/// context (specs 20 §6.1); the caller supplies it.
pub struct WarpSetVerifier<'a, V: ValidatorState> {
    /// The validator-state provider.
    state: &'a V,
    /// The verifying node's network id.
    network_id: u32,
    /// The proposervm-pinned P-Chain height that fixes the validator set.
    p_chain_height: u64,
    /// The quorum numerator (typically [`WARP_QUORUM_NUMERATOR`]).
    quorum_num: u64,
    /// The quorum denominator (typically [`WARP_QUORUM_DENOMINATOR`]).
    quorum_den: u64,
}

impl<'a, V: ValidatorState> WarpSetVerifier<'a, V> {
    /// Construct a verifier with the default `67/100` quorum
    /// ([`WARP_QUORUM_NUMERATOR`]/[`WARP_QUORUM_DENOMINATOR`]).
    pub fn new(state: &'a V, network_id: u32, p_chain_height: u64) -> Self {
        Self {
            state,
            network_id,
            p_chain_height,
            quorum_num: WARP_QUORUM_NUMERATOR,
            quorum_den: WARP_QUORUM_DENOMINATOR,
        }
    }

    /// Resolve the source subnet's warp set at the pinned height and verify the
    /// message's [`BitSetSignature`].
    ///
    /// `get_warp_validator_sets` returns a set per *subnet*, so the source chain
    /// id is first mapped to its subnet (specs 20 §6.1).
    ///
    /// # Errors
    /// Propagates [`verify_bit_set_signature`] failures, [`Error::Validators`] on
    /// a state lookup failure, and [`Error::NoValidatorSet`] if the source
    /// subnet has no set at the pinned height.
    pub async fn verify(&self, message: &Message) -> Result<()> {
        let source_chain_id = message.unsigned_message.source_chain_id;
        let subnet_id = self.state.get_subnet_id(source_chain_id).await?;
        let sets = self
            .state
            .get_warp_validator_sets(self.p_chain_height)
            .await?;
        let warp_set = sets.get(&subnet_id).ok_or(Error::NoValidatorSet)?;

        let Signature::BitSet(sig) = &message.signature;
        verify_bit_set_signature(
            sig,
            &message.unsigned_message,
            self.network_id,
            warp_set,
            self.quorum_num,
            self.quorum_den,
        )
    }
}

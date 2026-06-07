// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Warp message verification for the L1 lifecycle txs
//! (`vms/platformvm/txs/executor/warp_verifier.go`, specs 20 §6).
//!
//! Go's `VerifyWarpMessages` parses each embedded Warp message
//! ([`Message`](super::Message)) and verifies its [`BitSetSignature`](super::BitSetSignature)
//! against the canonical validator set of the source chain at the pinned P-Chain
//! height, requiring a `67/100` weight quorum.
//!
//! ## The real signature/quorum check (M4.22)
//!
//! The canonical-validator-set lookup (`warp.GetCanonicalValidatorSetFromChainID`,
//! the `WarpSet`/`FlattenValidatorSet` machinery of specs 20 §4) is served by
//! M4.21's [`PChainValidatorManager`](crate::validators::manager::PChainValidatorManager)
//! via [`get_warp_validator_sets`](ava_validators::state::ValidatorState::get_warp_validator_sets),
//! and the aggregate-BLS quorum check is now implemented here:
//!
//! - [`verify_bit_set_signature`] is the pure
//!   [`BitSetSignature`](super::BitSetSignature)`.Verify`
//!   (`vms/platformvm/warp/signature.go`): parse the signer bit-set (enforcing the
//!   no-padding invariant), select the canonical-ordered signers, check the
//!   signing weight meets the `67/100` quorum of the set's total weight, aggregate
//!   the signers' public keys, and BLS-verify the aggregate over `msg.bytes()`.
//! - [`WarpSetVerifier`] is the P-side glue: it resolves the source chain's subnet
//!   and the height-pinned [`WarpSet`](ava_validators::state::WarpSet) from a
//!   [`ValidatorState`](ava_validators::state::ValidatorState) (specs 20 §6.1),
//!   then calls [`verify_bit_set_signature`].
//!
//! The structural / parsing path below is unchanged: [`verify_warp_message`]
//! performs the layered parse ([`Message`](super::Message) →
//! [`AddressedCall`](super::payload::AddressedCall) →
//! [`RegistryPayload`](super::message::RegistryPayload)) and the registry payload's
//! structural `verify()`, then delegates the signature/quorum decision to the
//! injected [`WarpSignatureVerifier`] (the L1-lifecycle executor seam; tests use
//! [`AcceptingVerifier`] / [`RejectingVerifier`]).

use ava_crypto::bls;
use ava_utils::bits::Bits;
use ava_validators::state::{ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;

use crate::error::{Error, Result};

use super::message::RegistryPayload;
use super::payload::AddressedCall;
use super::{BitSetSignature, Message, Signature, UnsignedMessage};

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
fn filter_validators<'a>(
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
fn sum_weight(validators: &[&GetValidatorOutput]) -> Result<u64> {
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
fn aggregate_public_keys(validators: &[&GetValidatorOutput]) -> Result<bls::PublicKey> {
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

/// The real [`WarpSignatureVerifier`] (M4.22): obtains the source subnet's
/// canonical [`WarpSet`] at a pinned P-Chain height from a [`ValidatorState`]
/// (M4.21's [`PChainValidatorManager`](crate::validators::manager::PChainValidatorManager))
/// and runs [`verify_bit_set_signature`].
///
/// The pinned `p_chain_height` comes from the verifying block's proposervm
/// context (specs 20 §6.1); the caller supplies it.
pub struct WarpSetVerifier<'a, V: ValidatorState> {
    /// The validator-state provider (M4.21).
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

/// The injected BLS aggregate-signature / quorum check (the M4.21/M4.22 seam).
///
/// An implementor verifies that `message` was signed by at least the
/// [`WARP_QUORUM_NUMERATOR`]/[`WARP_QUORUM_DENOMINATOR`] weight of the canonical
/// validator set of `message.unsigned_message.source_chain_id` at the pinned
/// P-Chain height. The real implementation (M4.21/M4.22) resolves the warp set
/// from the validator-set provider; this trait lets the executor and the parsing
/// tests run without it.
pub trait WarpSignatureVerifier {
    /// Verify the aggregate signature + quorum for `message`.
    ///
    /// # Errors
    /// Returns an [`Error`] when the signature is malformed or the signing weight
    /// is below quorum.
    fn verify_signature(&self, message: &Message) -> Result<()>;
}

/// A verifier that accepts every (well-formed) signature.
///
/// Stands in for the real quorum check while M4.21/M4.22 are pending, and is the
/// fixture the L1-lifecycle conformance tests use to drive the parsing + PoP
/// path. **Not** for production use.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptingVerifier;

impl WarpSignatureVerifier for AcceptingVerifier {
    fn verify_signature(&self, _message: &Message) -> Result<()> {
        Ok(())
    }
}

/// A verifier that rejects every signature with [`Error::FlowCheckFailed`]'s
/// sibling [`Error::InvalidComponent`]. Used to assert the executor surfaces a
/// failed quorum.
#[derive(Clone, Copy, Debug, Default)]
pub struct RejectingVerifier;

impl WarpSignatureVerifier for RejectingVerifier {
    fn verify_signature(&self, _message: &Message) -> Result<()> {
        Err(Error::InvalidComponent)
    }
}

/// The fully-parsed contents of a verified ACP-77 Warp message: the outer
/// [`Message`], the [`AddressedCall`] wrapper, and the decoded registry payload
/// (plus the exact registry-payload bytes, needed for the `ValidationID` hash).
#[derive(Clone, Debug)]
pub struct ParsedWarp {
    /// The outer Warp message (carries `source_chain_id`).
    pub message: Message,
    /// The addressed-call wrapper (carries `source_address`).
    pub addressed_call: AddressedCall,
    /// The decoded ACP-77 registry payload.
    pub payload: RegistryPayload,
    /// The exact registry-payload wire bytes (the `AddressedCall.payload`).
    pub payload_bytes: Vec<u8>,
}

/// `warpVerifier.verify` — parse `message_bytes` through the three Warp codec
/// layers, run the registry payload's structural `verify()`, then delegate the
/// signature/quorum check to `verifier`.
///
/// Returns the parsed contents so the executor can read the source chain/address
/// and the registry payload without re-parsing.
///
/// # Errors
/// - [`Error::Codec`] if any of the three layers fails to decode.
/// - [`Error::InvalidComponent`] if the registry payload's structural check
///   fails.
/// - Propagates the injected [`WarpSignatureVerifier`]'s failure (the quorum
///   seam).
pub fn verify_warp_message<V: WarpSignatureVerifier>(
    verifier: &V,
    message_bytes: &[u8],
) -> Result<ParsedWarp> {
    // Layer 1: the Warp envelope.
    let message = Message::parse(message_bytes).map_err(Error::Codec)?;

    // Layer 2: the addressed call inside the unsigned message's payload.
    let addressed_call =
        AddressedCall::parse(&message.unsigned_message.payload).map_err(Error::Codec)?;

    // Layer 3: the ACP-77 registry payload inside the addressed call.
    let payload_bytes = addressed_call.payload.clone();
    let payload = RegistryPayload::parse(&payload_bytes).map_err(Error::Codec)?;

    // Structural verification of the registry payload (Go `msg.Verify()`).
    match &payload {
        RegistryPayload::RegisterL1Validator(m) => m.verify()?,
        RegistryPayload::L1ValidatorWeight(m) => m.verify()?,
        // The P-Chain only *receives* these two as commands; the other two are
        // outbound. Accept them structurally (Go has no inbound Verify for them).
        RegistryPayload::SubnetToL1Conversion(_) | RegistryPayload::L1ValidatorRegistration(_) => {}
    }

    // The signature/quorum step — the M4.21/M4.22 seam.
    verifier.verify_signature(&message)?;

    Ok(ParsedWarp {
        message,
        addressed_call,
        payload,
        payload_bytes,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! `warp_verifier` — the parsing + structural-check + seam path.
    //!
    //! Mirrors the buildable parts of Go `warp_verifier_test.go`
    //! (`TestVerifyWarpMessages`): a well-formed message parses & passes; a
    //! wrong-network / malformed message is rejected before the seam; the
    //! injected verifier governs the quorum decision.

    use ava_types::id::Id;
    use ava_types::short_id::ShortId;

    use super::*;
    use crate::warp::message::{PChainOwner, RegisterL1Validator};
    use crate::warp::payload::AddressedCall;
    use crate::warp::{BitSetSignature, Message, Signature, UnsignedMessage};

    /// Wraps `payload` (a marshaled [`RegistryPayload`]) in the three Warp layers,
    /// returning the full message bytes.
    fn wrap(network_id: u32, source_chain: Id, registry_payload: &RegistryPayload) -> Vec<u8> {
        let inner = registry_payload
            .marshal()
            .expect("marshal registry payload");
        let call = AddressedCall {
            source_address: vec![0xAB; 20],
            payload: inner,
        };
        let call_bytes = crate::warp::payload::WarpPayload::AddressedCall(call)
            .marshal_payload()
            .expect("marshal addressed call");
        let unsigned = UnsignedMessage {
            network_id,
            source_chain_id: source_chain,
            payload: call_bytes,
        };
        let msg = Message {
            unsigned_message: unsigned,
            signature: Signature::BitSet(BitSetSignature::default()),
        };
        msg.marshal().expect("marshal message")
    }

    fn valid_register() -> RegistryPayload {
        RegistryPayload::RegisterL1Validator(RegisterL1Validator {
            subnet_id: Id::from([0x11; 32]),
            node_id: vec![0x22; 20],
            bls_public_key: [0x33; ava_crypto::bls::PUBLIC_KEY_LEN],
            expiry: 100,
            remaining_balance_owner: PChainOwner {
                threshold: 1,
                addresses: vec![ShortId::from([0x44; 20])],
            },
            disable_owner: PChainOwner {
                threshold: 0,
                addresses: vec![],
            },
            weight: 1,
        })
    }

    #[test]
    fn warp_verifier_round_trip_and_accept() {
        let bytes = wrap(1, Id::from([0x55; 32]), &valid_register());
        let parsed = verify_warp_message(&AcceptingVerifier, &bytes).expect("verify");
        assert_eq!(
            parsed.message.unsigned_message.source_chain_id,
            Id::from([0x55; 32])
        );
        assert_eq!(parsed.addressed_call.source_address, vec![0xAB; 20]);
        match parsed.payload {
            RegistryPayload::RegisterL1Validator(m) => assert_eq!(m.weight, 1),
            _ => panic!("wrong payload type"),
        }
    }

    #[test]
    fn warp_verifier_rejects_invalid_registry_payload() {
        // A zero-weight RegisterL1Validator fails the structural check before the
        // seam is consulted.
        let bad = RegistryPayload::RegisterL1Validator(RegisterL1Validator {
            weight: 0,
            ..match valid_register() {
                RegistryPayload::RegisterL1Validator(m) => m,
                _ => unreachable!(),
            }
        });
        let bytes = wrap(1, Id::from([0x55; 32]), &bad);
        let err = verify_warp_message(&AcceptingVerifier, &bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidComponent));
    }

    #[test]
    fn warp_verifier_seam_governs_quorum() {
        let bytes = wrap(1, Id::from([0x55; 32]), &valid_register());
        // The structural checks pass, but the injected verifier rejects.
        let err = verify_warp_message(&RejectingVerifier, &bytes).unwrap_err();
        assert!(matches!(err, Error::InvalidComponent));
    }

    #[test]
    fn warp_verifier_rejects_malformed_message_bytes() {
        let err = verify_warp_message(&AcceptingVerifier, &[0xFF, 0xFF, 0xFF]).unwrap_err();
        assert!(matches!(err, Error::Codec(_)));
    }
}

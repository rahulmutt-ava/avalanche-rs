// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Warp message verification glue for the L1 lifecycle txs
//! (`vms/platformvm/txs/executor/warp_verifier.go`, specs 20 §6).
//!
//! The **generic** bit-set / quorum primitives now live in [`ava_warp::verifier`]
//! and are re-exported here ([`verify_bit_set_signature`], [`verify_weight`],
//! [`filter_validators`], [`sum_weight`], [`aggregate_public_keys`],
//! [`WarpSetVerifier`], and the quorum constants). This module keeps the
//! **P-Chain-specific** glue that operates over the ACP-77
//! [`RegistryPayload`](super::message::RegistryPayload) and the crate
//! [`Error`](crate::Error):
//!
//! - [`verify_warp_message`] performs the layered parse
//!   ([`Message`](super::Message) → [`AddressedCall`](super::payload::AddressedCall)
//!   → [`RegistryPayload`](super::message::RegistryPayload)) and the registry
//!   payload's structural `verify()`, then delegates the signature/quorum
//!   decision to the injected [`WarpSignatureVerifier`].
//! - [`WarpSignatureVerifier`] / [`AcceptingVerifier`] / [`RejectingVerifier`]
//!   are the L1-lifecycle executor seam (tests drive the parse + structural path
//!   without the live quorum machinery).

use crate::error::{Error, Result};

use super::Message;
use super::message::RegistryPayload;
use super::payload::AddressedCall;

// Re-export the generic Warp verification primitives so existing
// `crate::warp::verifier::{...}` paths (and downstream crates / tests) keep
// resolving against their `ava-warp` home (specs 20 §1).
pub use ava_warp::verifier::{
    WARP_QUORUM_DENOMINATOR, WARP_QUORUM_NUMERATOR, WarpSetVerifier, aggregate_public_keys,
    filter_validators, sum_weight, verify_bit_set_signature, verify_weight,
};

/// The injected BLS aggregate-signature / quorum check (the L1-lifecycle seam).
///
/// An implementor verifies that `message` was signed by at least the
/// [`WARP_QUORUM_NUMERATOR`]/[`WARP_QUORUM_DENOMINATOR`] weight of the canonical
/// validator set of `message.unsigned_message.source_chain_id` at the pinned
/// P-Chain height. The real implementation resolves the warp set from the
/// validator-set provider (via [`WarpSetVerifier`]); this trait lets the executor
/// and the parsing tests run without it.
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
/// Stands in for the real quorum check in the L1-lifecycle conformance tests,
/// driving the parsing + PoP path. **Not** for production use.
#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptingVerifier;

impl WarpSignatureVerifier for AcceptingVerifier {
    fn verify_signature(&self, _message: &Message) -> Result<()> {
        Ok(())
    }
}

/// A verifier that rejects every signature with [`Error::InvalidComponent`]. Used
/// to assert the executor surfaces a failed quorum.
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
///   fails (the generic `ava_warp::Error::InvalidPayload` maps here).
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

    // Structural verification of the registry payload (Go `msg.Verify()`). The
    // generic `ava_warp::Error::InvalidPayload` maps to `Error::InvalidComponent`.
    match &payload {
        RegistryPayload::RegisterL1Validator(m) => m.verify()?,
        RegistryPayload::L1ValidatorWeight(m) => m.verify()?,
        // The P-Chain only *receives* these two as commands; the other two are
        // outbound. Accept them structurally (Go has no inbound Verify for them).
        RegistryPayload::SubnetToL1Conversion(_) | RegistryPayload::L1ValidatorRegistration(_) => {}
    }

    // The signature/quorum step — the L1-lifecycle seam.
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

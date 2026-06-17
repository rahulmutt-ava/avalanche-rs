// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Simplex wire messages: the **quorum certificate** (`qc.go` +
//! `qc.canoto.go`) and the BLS verifier/aggregator backing it
//! (`bls.go`), specs 06 §8.
//!
//! The proposal / vote / finalization messages are, on the Go side, `p2p.Simplex`
//! protobuf envelopes that carry a [`QC`] (notarizations/finalizations) or a
//! per-node signature plus a [`BlockHeader`]. Since `ava-message`'s proto types
//! for `p2p.Simplex` are not yet wired here, this module ports the
//! **consensus-affecting** wire pieces — the canoto QC and the BLS quorum rules
//! — verbatim, and exposes [`BlockHeader`] / [`Vote`] / [`Finalization`] as the
//! engine-facing payloads. The QC canoto bytes are byte-identical to Go's
//! generated `canotoQC.MarshalCanoto`.

use std::collections::BTreeMap;

use ava_crypto::bls::{PublicKey, Signature, aggregate_public_keys, aggregate_signatures, verify};
use ava_types::id::{ID_LEN, Id};
use ava_types::node_id::NodeId;

use crate::canoto::{self, DecodeError, Reader, WIRE_LEN};
use crate::error::{Error, Result};

/// `bls.SignatureLen` — length of a compressed BLS signature (G2).
pub const SIGNATURE_LEN: usize = ava_crypto::bls::SIGNATURE_LEN;

/// Canoto field numbers for `canotoQC` (`qc.canoto.go`).
const QC_FIELD_SIG: u32 = 1;
const QC_FIELD_SIGNERS: u32 = 2;

/// The BLS membership set backing QC verification (`simplex.BLSVerifier`).
///
/// It holds the per-node public keys plus the **canonical** node-ID ordering
/// (Go sorts node IDs ascending) used to map signers to bitset indices.
#[derive(Clone)]
pub struct BlsVerifier {
    /// Map from node ID to its BLS public key.
    node_id_to_pk: BTreeMap<NodeId, PublicKey>,
    /// Node IDs sorted ascending (`canonicalNodeIDs`).
    canonical_node_ids: Vec<NodeId>,
    /// The network ID mixed into every signed message.
    network_id: u32,
    /// The chain ID mixed into every signed message.
    chain_id: Id,
}

impl BlsVerifier {
    /// Builds a verifier from `(node_id, public_key)` pairs (`createVerifier`).
    /// Node IDs are sorted ascending to form the canonical index order.
    pub fn new(
        validators: impl IntoIterator<Item = (NodeId, PublicKey)>,
        network_id: u32,
        chain_id: Id,
    ) -> Self {
        let node_id_to_pk: BTreeMap<NodeId, PublicKey> = validators.into_iter().collect();
        // BTreeMap iterates ascending, matching Go's `utils.Sort(nodeIDs)`.
        let canonical_node_ids: Vec<NodeId> = node_id_to_pk.keys().copied().collect();
        Self {
            node_id_to_pk,
            canonical_node_ids,
            network_id,
            chain_id,
        }
    }

    /// `simplex.Quorum(n)` — the ⅔+1 quorum size for `n` validators.
    ///
    /// Go's `simplex.Quorum` is `floor((2n)/3) + 1` (a strict supermajority of
    /// one node = one vote).
    pub fn quorum(&self) -> usize {
        quorum(self.node_id_to_pk.len())
    }

    /// Number of validators in the membership set.
    pub fn len(&self) -> usize {
        self.node_id_to_pk.len()
    }

    /// Whether the membership set is empty.
    pub fn is_empty(&self) -> bool {
        self.node_id_to_pk.is_empty()
    }
}

/// `simplex.Quorum(n) = floor(2n/3) + 1`.
pub fn quorum(n: usize) -> usize {
    n.saturating_mul(2)
        .checked_div(3)
        .unwrap_or(0)
        .saturating_add(1)
}

/// A quorum certificate: an aggregated BLS signature over a set of signers
/// (`simplex.QC`).
#[derive(Clone)]
pub struct QC {
    sig: Signature,
    signers: Vec<NodeId>,
    verifier: BlsVerifier,
}

impl QC {
    /// Constructs a QC from an aggregated signature, its signers, and the
    /// verifier they were drawn from.
    pub fn new(sig: Signature, signers: Vec<NodeId>, verifier: BlsVerifier) -> Self {
        Self {
            sig,
            signers,
            verifier,
        }
    }

    /// The signers that contributed to this certificate (`QC.Signers`).
    pub fn signers(&self) -> &[NodeId] {
        &self.signers
    }

    /// `QC.Bytes` — the canoto serialization (byte-identical to Go's
    /// `canotoQC.MarshalCanoto`): field 1 = the 96-byte compressed signature,
    /// field 2 = the signers bitset (canonical-index big-endian bigint bytes).
    pub fn bytes(&self) -> Vec<u8> {
        let sig = self.sig.compress();
        let signers = self.create_signers_bitset();
        marshal_canoto_qc(&sig, &signers)
    }

    /// Builds the signers bitset over `canonicalNodeIDIndices`
    /// (`QC.createSignersBitSet`): a big-endian big-integer with one bit set per
    /// signer's canonical index.
    fn create_signers_bitset(&self) -> Vec<u8> {
        let mut indices = Vec::with_capacity(self.signers.len());
        for signer in &self.signers {
            if let Ok(idx) = self.verifier.canonical_node_ids.binary_search(signer) {
                indices.push(idx);
            }
        }
        bits_to_bytes(&indices)
    }

    /// `QC.Verify` — checks the certificate against `msg`.
    ///
    /// 1. exactly `quorum` signers,
    /// 2. no duplicate signer, all in the membership set,
    /// 3. the aggregated public key verifies the aggregated signature over the
    ///    chain/network-tagged message.
    pub fn verify(&self, msg: &[u8]) -> Result<()> {
        let quorum = self.verifier.quorum();
        if self.signers.len() != quorum {
            return Err(Error::UnexpectedSigners {
                expected: quorum,
                got: self.signers.len(),
            });
        }

        let mut seen = std::collections::BTreeSet::new();
        let mut pks: Vec<&PublicKey> = Vec::with_capacity(self.signers.len());
        for signer in &self.signers {
            if !seen.insert(*signer) {
                return Err(Error::DuplicateSigner);
            }
            let pk = self
                .verifier
                .node_id_to_pk
                .get(signer)
                .ok_or(Error::SignerNotFound)?;
            pks.push(pk);
        }

        let agg_pk = aggregate_public_keys(&pks).map_err(Error::SignatureAggregation)?;
        let message = encode_message_to_sign(msg, self.verifier.chain_id, self.verifier.network_id);

        if !verify(&agg_pk, &self.sig, &message) {
            return Err(Error::SignatureVerificationFailed);
        }
        Ok(())
    }

    /// `QCDeserializer.DeserializeQuorumCertificate` — parses a QC from its
    /// canoto bytes against the supplied membership set.
    pub fn from_bytes(bytes: &[u8], verifier: BlsVerifier) -> Result<Self> {
        let (sig_bytes, signers_bytes) = unmarshal_canoto_qc(bytes)?;
        let sig = Signature::from_bytes(&sig_bytes).map_err(Error::InvalidSignature)?;
        let signers = signers_from_bytes(&signers_bytes, &verifier.canonical_node_ids)?;
        Ok(Self {
            sig,
            signers,
            verifier,
        })
    }
}

/// Aggregates `signatures` (one per signer) into a [`QC`] over `verifier`
/// (`SignatureAggregator.Aggregate`). Requires at least a quorum; takes the
/// first `quorum` signatures in the order supplied (matching Go's
/// `signatures = signatures[:quorumSize]`).
pub fn aggregate(verifier: &BlsVerifier, signatures: &[(NodeId, Signature)]) -> Result<QC> {
    let quorum = verifier.quorum();
    if signatures.len() < quorum {
        return Err(Error::UnexpectedSigners {
            expected: quorum,
            got: signatures.len(),
        });
    }
    // `signatures.len() >= quorum` was just checked, so this slice exists.
    let chosen = signatures.get(..quorum).ok_or(Error::UnexpectedSigners {
        expected: quorum,
        got: signatures.len(),
    })?;

    let mut signers = Vec::with_capacity(quorum);
    let mut sigs: Vec<&Signature> = Vec::with_capacity(quorum);
    for (node_id, sig) in chosen {
        if !verifier.node_id_to_pk.contains_key(node_id) {
            return Err(Error::SignerNotFound);
        }
        signers.push(*node_id);
        sigs.push(sig);
    }

    let agg = aggregate_signatures(&sigs).map_err(Error::SignatureAggregation)?;
    Ok(QC::new(agg, signers, verifier.clone()))
}

// ---------------------------------------------------------------------------
// canoto QC encode/decode (byte-identical to qc.canoto.go).
// ---------------------------------------------------------------------------

/// Encodes a raw `(sig, signers)` pair as a `canotoQC` — the public entry point
/// for the canoto wire layer (used by [`QC::bytes`] and the golden vector test).
/// `sig` is the 96-byte compressed BLS signature; `signers` is the canonical
/// big-endian bitset.
pub fn encode_qc(sig: &[u8; SIGNATURE_LEN], signers: &[u8]) -> Vec<u8> {
    marshal_canoto_qc(sig, signers)
}

/// Decodes a `canotoQC` into its raw `(sig, signers)` byte pair, enforcing the
/// same wire-format invariants as Go's generated `UnmarshalCanotoFrom`.
pub fn decode_qc(bytes: &[u8]) -> Result<([u8; SIGNATURE_LEN], Vec<u8>)> {
    unmarshal_canoto_qc(bytes)
}

/// Marshals a `canotoQC { Sig [96]byte, Signers []byte }`. The fixed-bytes
/// `Sig` is only emitted if non-zero; `Signers` only if non-empty
/// (matches the generated `MarshalCanotoInto`).
fn marshal_canoto_qc(sig: &[u8; SIGNATURE_LEN], signers: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    if sig.iter().any(|&b| b != 0) {
        canoto::append_tag(&mut out, QC_FIELD_SIG, WIRE_LEN);
        canoto::append_bytes(&mut out, sig);
    }
    if !signers.is_empty() {
        canoto::append_tag(&mut out, QC_FIELD_SIGNERS, WIRE_LEN);
        canoto::append_bytes(&mut out, signers);
    }
    out
}

/// Unmarshals a `canotoQC`, returning `(sig_bytes, signers_bytes)`. Enforces
/// ascending field order, the fixed 96-byte `Sig` length, the non-zero `Sig`
/// guard, and the non-empty `Signers` guard (matches `UnmarshalCanotoFrom`).
fn unmarshal_canoto_qc(bytes: &[u8]) -> Result<([u8; SIGNATURE_LEN], Vec<u8>)> {
    let mut r = Reader::new(bytes);
    let mut sig = [0u8; SIGNATURE_LEN];
    let mut signers: Vec<u8> = Vec::new();
    let mut min_field: u32 = 0;
    while r.has_next() {
        let (field, wire) = r.read_tag()?;
        if field < min_field {
            return Err(Error::Decode(DecodeError::InvalidFieldOrder));
        }
        match field {
            QC_FIELD_SIG => {
                if wire != WIRE_LEN {
                    return Err(Error::Decode(DecodeError::UnexpectedWireType(wire)));
                }
                let val = r.read_bytes()?;
                if val.len() != SIGNATURE_LEN {
                    return Err(Error::Decode(DecodeError::InvalidLength));
                }
                sig.copy_from_slice(val);
                if sig.iter().all(|&b| b == 0) {
                    return Err(Error::Decode(DecodeError::ZeroValue));
                }
            }
            QC_FIELD_SIGNERS => {
                if wire != WIRE_LEN {
                    return Err(Error::Decode(DecodeError::UnexpectedWireType(wire)));
                }
                let val = r.read_bytes()?;
                if val.is_empty() {
                    return Err(Error::Decode(DecodeError::ZeroValue));
                }
                signers = val.to_vec();
            }
            other => return Err(Error::Decode(DecodeError::UnknownField(other))),
        }
        min_field = field.saturating_add(1);
    }
    Ok((sig, signers))
}

// ---------------------------------------------------------------------------
// Signers bitset (set.Bits — a big-endian big.Int).
// ---------------------------------------------------------------------------

/// Encodes a set of bit indices as Go's `set.Bits.Bytes()` does: a `big.Int`
/// holding the bits, serialized big-endian with **no leading zero bytes** (the
/// empty set serializes to an empty slice).
fn bits_to_bytes(indices: &[usize]) -> Vec<u8> {
    let Some(&max) = indices.iter().max() else {
        return Vec::new();
    };
    let num_bytes = max.checked_div(8).unwrap_or(0).saturating_add(1);
    // Little-endian scratch, byte 0 = bits 0..7.
    let mut le = vec![0u8; num_bytes];
    for &i in indices {
        let (byte_idx, bit) = (i.checked_div(8).unwrap_or(0), i % 8);
        if let Some(slot) = le.get_mut(byte_idx) {
            // `bit` is in `[0, 8)`, a valid shift amount for `u8`.
            *slot |= 1u8 << bit;
        }
    }
    // big.Int.Bytes() is big-endian and trims leading zero bytes.
    let mut be: Vec<u8> = le.into_iter().rev().collect();
    let first_nonzero = be.iter().position(|&b| b != 0).unwrap_or(be.len());
    be.drain(..first_nonzero);
    be
}

/// Decodes a `set.Bits` big-endian bitset into the set bit indices (ascending).
fn bytes_to_bits(bytes: &[u8]) -> Vec<usize> {
    let mut indices = Vec::new();
    // byte[n-1] holds bits 0..7, byte[n-2] bits 8..15, ... (big-endian).
    for (rev, &byte) in bytes.iter().rev().enumerate() {
        for bit in 0..8usize {
            // `bit` is in `[0, 8)`, a valid shift amount for `u8`.
            if byte & (1u8 << bit) != 0 {
                indices.push(rev.saturating_mul(8).saturating_add(bit));
            }
        }
    }
    indices.sort_unstable();
    indices
}

/// `QCDeserializer.signersFromBytes` — decodes the signers bitset and maps the
/// set indices back to node IDs, enforcing the canonical-form round-trip
/// (`errInvalidBitSet`) and bound (`errNodeNotFound`).
fn signers_from_bytes(signer_bytes: &[u8], canonical: &[NodeId]) -> Result<Vec<NodeId>> {
    let indices = bytes_to_bits(signer_bytes);
    // Re-encode and compare to reject non-canonical encodings (leading zeros).
    if bits_to_bytes(&indices) != signer_bytes {
        return Err(Error::InvalidBitSet);
    }
    let mut signers = Vec::with_capacity(indices.len());
    for i in indices {
        let node = canonical.get(i).ok_or(Error::SignerNotFound)?;
        signers.push(*node);
    }
    Ok(signers)
}

// ---------------------------------------------------------------------------
// Message-to-sign encoding (bls.go encodeMessageToSign / encodedSimplexSignedPayload).
// ---------------------------------------------------------------------------

/// `simplex.encodeMessageToSign` — the linearcodec serialization of
/// `encodedSimplexSignedPayload { NetworkID uint32, ChainID [32]byte, Message
/// []byte }` with the 2-byte codec-version prefix.
///
/// Layout (all integers big-endian, matching the avalanchego linear codec):
/// `version(2) ++ network_id(4) ++ chain_id(32) ++ message_len(4) ++ message`.
pub fn encode_message_to_sign(message: &[u8], chain_id: Id, network_id: u32) -> Vec<u8> {
    // CodecVersion = warp.CodecVersion(0) + 1 = 1.
    const CODEC_VERSION: u16 = 1;
    // version(2) + network_id(4) + chain_id(32) + len(4) = 42-byte header.
    let header_len = 2 + 4 + ID_LEN + 4;
    let mut out = Vec::with_capacity(header_len.saturating_add(message.len()));
    out.extend_from_slice(&CODEC_VERSION.to_be_bytes());
    out.extend_from_slice(&network_id.to_be_bytes());
    out.extend_from_slice(chain_id.as_bytes());
    let msg_len = u32::try_from(message.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&msg_len.to_be_bytes());
    out.extend_from_slice(message);
    out
}

// ---------------------------------------------------------------------------
// Engine-facing payloads (the non-QC p2p.Simplex pieces). Defined here so the
// stubbed engine has a typed surface; full p2p.Simplex proto wiring is deferred.
// ---------------------------------------------------------------------------

/// A succinct, collision-free reference to a block (`simplex.BlockHeader`):
/// the [`crate::block::ProtocolMetadata`] plus the block digest.
#[derive(Clone, PartialEq, Eq)]
pub struct BlockHeader {
    /// Protocol metadata (version/epoch/round/seq/prev).
    pub metadata: crate::block::ProtocolMetadata,
    /// The block's 32-byte digest.
    pub digest: [u8; 32],
}

/// A per-node signature over a [`BlockHeader`] (`simplex.Vote`).
#[derive(Clone)]
pub struct Vote {
    /// The voted-on block header.
    pub block_header: BlockHeader,
    /// The signer's node ID.
    pub signer: NodeId,
    /// The signer's BLS signature bytes.
    pub signature: Vec<u8>,
}

/// A finalization message — a [`QC`] over a finalized [`BlockHeader`]
/// (`simplex.Finalization`).
pub struct Finalization {
    /// The finalized block header.
    pub block_header: BlockHeader,
    /// The aggregated quorum certificate.
    pub qc: QC,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quorum_sizes() {
        // floor(2n/3)+1: n=1→1, n=3→3, n=4→3, n=7→5, n=10→7.
        assert_eq!(quorum(1), 1);
        assert_eq!(quorum(3), 3);
        assert_eq!(quorum(4), 3);
        assert_eq!(quorum(7), 5);
        assert_eq!(quorum(10), 7);
    }

    #[test]
    fn bitset_roundtrip() {
        // {0,1} => big.Int 0b11 = 0x03.
        assert_eq!(bits_to_bytes(&[0, 1]), vec![0x03]);
        assert_eq!(bytes_to_bits(&[0x03]), vec![0, 1]);
        // {8} => bit 8 set => big-endian [0x01, 0x00].
        assert_eq!(bits_to_bytes(&[8]), vec![0x01, 0x00]);
        assert_eq!(bytes_to_bits(&[0x01, 0x00]), vec![8]);
        // empty set => empty bytes.
        assert!(bits_to_bytes(&[]).is_empty());
    }

    #[test]
    fn non_canonical_bitset_rejected() {
        // [0x00, 0x03] is non-canonical (leading zero) for {0,1}.
        let canonical = vec![NodeId::from([0u8; 20]), NodeId::from([1u8; 20])];
        assert!(signers_from_bytes(&[0x00, 0x03], &canonical).is_err());
        assert!(signers_from_bytes(&[0x03], &canonical).is_ok());
    }
}

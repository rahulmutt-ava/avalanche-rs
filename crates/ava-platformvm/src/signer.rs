// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/platformvm/signer` — the `Signer` interface (specs 08 §2.2, §8).
//!
//! A staker either declares **no** BLS key ([`Signer::Empty`], type_id 27) or a
//! BLS key with a [`ProofOfPossession`] (type_id 28). The two concrete types are
//! registered into the shared P-Chain codec, so the enum marshals the typeID
//! prefix then the payload (`AddPermissionlessValidatorTx.Signer`, etc.).

use ava_codec::AvaCodec;
use ava_crypto::bls;

use crate::Error;

/// Length of a compressed BLS public key (`bls.PublicKeyLen` = 48).
pub const PUBLIC_KEY_LEN: usize = bls::PUBLIC_KEY_LEN;
/// Length of a BLS signature (`bls.SignatureLen` = 96).
pub const SIGNATURE_LEN: usize = bls::SIGNATURE_LEN;

/// `signer.ProofOfPossession` (type_id 28).
///
/// A compressed BLS public key plus a signature proving ownership of it (the
/// signed message is the public key itself, under the proof-of-possession
/// ciphersuite — `bls.VerifyProofOfPossession`).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
pub struct ProofOfPossession {
    /// `PublicKey` — the compressed (G1) BLS public key.
    #[codec]
    pub public_key: [u8; PUBLIC_KEY_LEN],
    /// `ProofOfPossession` — the BLS signature over `public_key`.
    #[codec]
    pub proof: [u8; SIGNATURE_LEN],
}

impl Default for ProofOfPossession {
    fn default() -> Self {
        Self {
            public_key: [0u8; PUBLIC_KEY_LEN],
            proof: [0u8; SIGNATURE_LEN],
        }
    }
}

impl ProofOfPossession {
    /// Builds a [`ProofOfPossession`] from a compressed key and signature.
    #[must_use]
    pub fn new(public_key: [u8; PUBLIC_KEY_LEN], proof: [u8; SIGNATURE_LEN]) -> Self {
        Self { public_key, proof }
    }

    /// `ProofOfPossession.Verify` — parse the key & signature, then verify the
    /// proof of possession over the public-key bytes (specs 08 §8).
    ///
    /// # Errors
    /// Returns [`Error::InvalidProofOfPossession`] if the key or signature is
    /// malformed, or the proof fails to verify.
    pub fn verify(&self) -> Result<(), Error> {
        let pk = bls::PublicKey::from_compressed(&self.public_key)
            .map_err(|_| Error::InvalidProofOfPossession)?;
        let sig =
            bls::Signature::from_bytes(&self.proof).map_err(|_| Error::InvalidProofOfPossession)?;
        if bls::verify_pop(&pk, &sig, &self.public_key) {
            Ok(())
        } else {
            Err(Error::InvalidProofOfPossession)
        }
    }

    /// The parsed BLS public key, if it deserializes.
    ///
    /// # Errors
    /// Returns [`Error::InvalidProofOfPossession`] if the compressed key bytes
    /// are not a valid G1 point.
    pub fn key(&self) -> Result<bls::PublicKey, Error> {
        bls::PublicKey::from_compressed(&self.public_key)
            .map_err(|_| Error::InvalidProofOfPossession)
    }
}

/// `signer.Signer` — the registered BLS-signer interface.
///
/// Marshals as `u32 typeID` + payload: [`Signer::Empty`] is type_id 27 (empty
/// body); [`Signer::ProofOfPossession`] is type_id 28.
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum Signer {
    /// `signer.Empty` — no BLS key (type_id 27).
    #[codec(type_id = 27)]
    Empty(Empty),
    /// `signer.ProofOfPossession` — a BLS key + proof (type_id 28).
    #[codec(type_id = 28)]
    ProofOfPossession(ProofOfPossession),
}

/// `signer.Empty` — the empty (no-BLS-key) signer (type_id 27).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Empty;

impl Default for Signer {
    fn default() -> Self {
        Signer::Empty(Empty)
    }
}

impl Signer {
    /// `Signer.Verify` — `Empty` always verifies; `ProofOfPossession` verifies
    /// its proof.
    ///
    /// # Errors
    /// Propagates [`ProofOfPossession::verify`].
    pub fn verify(&self) -> Result<(), Error> {
        match self {
            Signer::Empty(_) => Ok(()),
            Signer::ProofOfPossession(pop) => pop.verify(),
        }
    }

    /// `Signer.Key` — the BLS public key, or `None` for the empty signer.
    ///
    /// # Errors
    /// Returns [`Error::InvalidProofOfPossession`] if a present key is malformed.
    pub fn key(&self) -> Result<Option<bls::PublicKey>, Error> {
        match self {
            Signer::Empty(_) => Ok(None),
            Signer::ProofOfPossession(pop) => pop.key().map(Some),
        }
    }

    /// `true` iff this signer carries a BLS key (used by the "BLS key present
    /// iff Primary Network" syntactic check).
    #[must_use]
    pub fn has_key(&self) -> bool {
        matches!(self, Signer::ProofOfPossession(_))
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use ava_codec::packer::Packer;
    use ava_codec::{Deserializable, Serializable};

    use super::*;

    #[test]
    fn empty_signer_marshals_type_id_27() {
        let s = Signer::Empty(Empty);
        let mut p = Packer::with_max_size(64);
        s.marshal_into(&mut p);
        let bytes = p.into_bytes();
        // Just the u32 typeID 27, empty body.
        assert_eq!(bytes, vec![0x00, 0x00, 0x00, 0x1b]);
        assert_eq!(s.codec_type_id(), 27);
    }

    #[test]
    fn pop_roundtrip_type_id_28() {
        let pop = ProofOfPossession::new([1u8; PUBLIC_KEY_LEN], [2u8; SIGNATURE_LEN]);
        let s = Signer::ProofOfPossession(pop);
        assert_eq!(s.codec_type_id(), 28);

        let mut p = Packer::with_max_size(256);
        s.marshal_into(&mut p);
        let bytes = p.into_bytes();
        assert_eq!(bytes.len(), 4 + PUBLIC_KEY_LEN + SIGNATURE_LEN);
        assert_eq!(&bytes[..4], &[0x00, 0x00, 0x00, 0x1c]);

        let mut decoded = Signer::default();
        let mut rp = Packer::new_read(&bytes);
        decoded.unmarshal_from(&mut rp);
        assert!(!rp.errored());
        assert_eq!(decoded, s);
    }
}

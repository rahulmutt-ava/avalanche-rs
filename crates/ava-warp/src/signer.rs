// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Local Warp signing (`vms/platformvm/warp/signer.go`, specs 20 §5.1).
//!
//! A [`LocalSigner`] is a node's authority to produce a BLS signature over an
//! [`UnsignedMessage`] *for its own chain at its own network*. It wraps a BLS
//! [`bls::Signer`](ava_crypto::bls::Signer) (the node key) plus the
//! `(network_id, chain_id)` it is authorized to sign for; signing first checks
//! the message is bound to that chain/network and then signs the
//! version-prefixed [`UnsignedMessage::marshal`] bytes with the **signature**
//! ciphersuite (NOT the proof-of-possession DST; specs 20 §5.1).

use std::sync::Arc;

use ava_crypto::bls;
use ava_types::id::Id;

use crate::UnsignedMessage;
use crate::error::{Error, Result};

/// `warp.Signer` — produce this node's BLS signature over an unsigned message.
///
/// Mirrors Go `warp.Signer.Sign`: the implementor errors if it lacks the
/// authority to sign the message (wrong source chain / network).
pub trait Signer {
    /// The 96-byte compressed BLS signature over [`UnsignedMessage::marshal`].
    ///
    /// # Errors
    /// - [`Error::WrongSourceChainId`] / [`Error::WrongNetworkId`] if the message
    ///   is not bound to this signer's chain/network.
    /// - [`Error::Codec`] if the message fails to marshal.
    fn sign(&self, msg: &UnsignedMessage) -> Result<[u8; bls::SIGNATURE_LEN]>;
}

/// `warp.signer` — the in-process [`Signer`] backed by the node's BLS key
/// (`vms/platformvm/warp/signer.go::signer`).
pub struct LocalSigner {
    /// The node's BLS signer (local or remote).
    sk: Arc<dyn bls::Signer>,
    /// The network this signer is authorized for.
    network_id: u32,
    /// The chain this signer is authorized for (its own chain id).
    chain_id: Id,
}

impl LocalSigner {
    /// `warp.NewSigner(sk, networkID, chainID)`.
    #[must_use]
    pub fn new(sk: Arc<dyn bls::Signer>, network_id: u32, chain_id: Id) -> Self {
        Self {
            sk,
            network_id,
            chain_id,
        }
    }
}

impl Signer for LocalSigner {
    fn sign(&self, msg: &UnsignedMessage) -> Result<[u8; bls::SIGNATURE_LEN]> {
        if msg.source_chain_id != self.chain_id {
            return Err(Error::WrongSourceChainId);
        }
        if msg.network_id != self.network_id {
            return Err(Error::WrongNetworkId);
        }
        let bytes = msg.marshal()?;
        // Signature ciphersuite DST (specs 20 §5.1), NOT the PoP DST.
        let sig = self.sk.sign(&bytes).map_err(|_| Error::InvalidSignature)?;
        Ok(sig.compress())
    }
}

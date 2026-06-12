// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 1–3 (specs/12 §2.2): staking certificate → NodeID, BLS staking
//! signer + proof of possession, and the "initializing node" banner.

use std::sync::Arc;

use base64::Engine;

use ava_config::node::{Config, StakingSignerConfig};
use ava_crypto::bls::{LocalSigner, Signer};
use ava_network::identity::Identity;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};

/// The node's BLS proof of possession: the compressed public key and the PoP
/// signature over it (Go `signer.ProofOfPossession`).
#[derive(Clone, Copy)]
pub struct ProofOfPossession {
    /// Compressed (G1) BLS public key, 48 bytes.
    pub public_key: [u8; 48],
    /// PoP signature over the compressed public key, 96 bytes.
    pub proof_of_possession: [u8; 96],
}

impl std::fmt::Debug for ProofOfPossession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProofOfPossession")
            .field("public_key", &hex(&self.public_key))
            .field("proof_of_possession", &hex(&self.proof_of_possession))
            .finish()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2).saturating_add(2));
    out.push_str("0x");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Step 1: parse the staking TLS certificate strictly and derive the NodeID
/// (Go `staking.ParseCertificate` + `ids.NodeIDFromCert`).
///
/// # Errors
/// [`Error::StakingCert`] when the certificate fails the strict ASN.1 parse.
pub fn node_id_from_identity(identity: &Identity) -> Result<NodeId> {
    ava_crypto::staking::parse_certificate(identity.cert_der())
        .map_err(|e| Error::StakingCert(e.to_string()))?;
    Ok(ava_crypto::staking::node_id_from_cert(identity.cert_der()))
}

/// Step 2: build the BLS staking signer from the resolved signer config
/// (mirror Go `newStakingSigner`).
///
/// Branch order matches Go: ephemeral → key content → RPC endpoint → key file
/// (creating + persisting a fresh key when the file does not exist).
///
/// # Errors
/// - [`Error::StakingSigner`] when key material is invalid or the key file
///   cannot be read/written.
/// - [`Error::RpcSignerUnsupported`] when `--staking-rpc-signer-endpoint` is
///   set (the Rust RPC signer is deferred — `tests/PORTING.md`).
pub fn new_staking_signer(cfg: &StakingSignerConfig) -> Result<Arc<dyn Signer>> {
    if cfg.ephemeral_signer_enabled {
        let signer = LocalSigner::generate().map_err(|e| Error::StakingSigner(e.to_string()))?;
        return Ok(Arc::new(signer));
    }

    if !cfg.key_content.is_empty() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(cfg.key_content.trim())
            .map_err(|e| Error::StakingSigner(format!("unable to decode base64 content: {e}")))?;
        let signer = LocalSigner::from_bytes(&bytes)
            .map_err(|e| Error::StakingSigner(format!("could not parse signing key: {e}")))?;
        return Ok(Arc::new(signer));
    }

    if !cfg.rpc_endpoint.is_empty() {
        return Err(Error::RpcSignerUnsupported(cfg.rpc_endpoint.clone()));
    }

    let signer = LocalSigner::from_file_or_persist_new(std::path::Path::new(&cfg.key_path))
        .map_err(|e| Error::StakingSigner(e.to_string()))?;
    Ok(Arc::new(signer))
}

/// Build the proof of possession for `signer` (Go
/// `signer.NewProofOfPossession`): sign the compressed public key with the PoP
/// ciphersuite.
///
/// # Errors
/// [`Error::StakingSigner`] when the signer refuses to sign (a remote signer
/// may fail; the local signer never does).
pub fn proof_of_possession(signer: &dyn Signer) -> Result<ProofOfPossession> {
    let public_key = signer.public_key().compress();
    let sig = signer
        .sign_proof_of_possession(&public_key)
        .map_err(|e| Error::StakingSigner(format!("problem creating proof of possession: {e}")))?;
    Ok(ProofOfPossession {
        public_key,
        proof_of_possession: sig.compress(),
    })
}

/// Step 3: the "initializing node" banner (Go logs version / commit / nodeID /
/// POP / config).
pub fn log_banner(config: &Config, node_id: NodeId, pop: &ProofOfPossession) {
    tracing::info!(
        version = %*ava_version::CURRENT,
        node_id = %node_id,
        node_pop = ?pop,
        network_id = config.network_id,
        provided_flags = ?config.provided_flags,
        "initializing node"
    );
}

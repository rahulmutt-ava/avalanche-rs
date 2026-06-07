// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Post-fork blocks: `statelessBlock` (signed/unsigned) and
//! `statelessGraniteBlock`.
//!
//! Port of Go `vms/proposervm/block/block.go` + `build.go`. Both share the
//! `statelessBlockMetadata` initialization/verification logic:
//!
//! - `id = sha256(bytes[.. len - IntLen - len(sig)])` — the serialized form is
//!   the unsigned bytes followed by the `u32`-length-prefixed signature, so the
//!   unsigned preimage strips both the signature and its 4-byte length prefix.
//! - `verify`: empty cert + present sig ⇒ error; empty cert ⇒ ok; otherwise
//!   build the `Header{chain, parent, id}` and `staking::check_signature` over
//!   `header.bytes()`.

use ava_codec::packer::INT_LEN;
use ava_crypto::staking::{self, Certificate};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use super::codec::{TYPE_ID_GRANITE_BLOCK, TYPE_ID_STATELESS_BLOCK, marshal_typed};
use super::hash::sha256_id;
use super::header::Header;
use super::stateless::{Epoch, StatelessUnsignedBlock, StatelessUnsignedGraniteBlock};
use ava_codec::packer::Packer;

/// A signing function over a block's `Header` bytes (Go `crypto.Signer.Sign`;
/// the signer hashes the message internally with SHA-256). Returns the raw
/// signature or a human-readable error.
pub type SignFn<'a> = dyn Fn(&[u8]) -> std::result::Result<Vec<u8>, String> + 'a;

/// The cached metadata shared by signed and Granite blocks.
#[derive(Debug, Clone)]
struct Metadata {
    id: Id,
    cert: Option<Certificate>,
    proposer: NodeId,
    bytes: Vec<u8>,
}

impl Metadata {
    /// Go `statelessBlockMetadata.initialize`.
    fn initialize(
        body: &StatelessUnsignedBlock,
        sig: &[u8],
        bytes: Vec<u8>,
    ) -> crate::Result<Self> {
        // Strip the signature and its u32 length prefix to recover the unsigned
        // preimage that the id hashes.
        let len_unsigned = bytes
            .len()
            .saturating_sub(INT_LEN)
            .saturating_sub(sig.len());
        let preimage = bytes.get(..len_unsigned).unwrap_or(&bytes);
        let id = sha256_id(preimage);

        let (cert, proposer) = if body.certificate.is_empty() {
            (None, NodeId::EMPTY)
        } else {
            let cert = staking::parse_certificate(&body.certificate)
                .map_err(|e| crate::Error::InvalidCertificate(format!("{e:?}")))?;
            let proposer = staking::node_id_from_cert(&body.certificate);
            (Some(cert), proposer)
        };

        Ok(Self {
            id,
            cert,
            proposer,
            bytes,
        })
    }

    /// Go `statelessBlockMetadata.verify`.
    fn verify(&self, body: &StatelessUnsignedBlock, sig: &[u8], chain_id: Id) -> crate::Result<()> {
        let Some(cert) = &self.cert else {
            if !sig.is_empty() {
                return Err(crate::Error::UnexpectedSignature);
            }
            return Ok(());
        };

        let header = Header::build(chain_id, body.parent_id, self.id);
        staking::check_signature(cert, header.bytes(), sig)
            .map_err(|_| crate::Error::SignatureVerifyFailed)
    }
}

/// `statelessBlock` — a post-fork, pre-Granite block.
#[derive(Debug, Clone)]
pub struct SignedBlock {
    stateless_block: StatelessUnsignedBlock,
    signature: Vec<u8>,
    metadata: Metadata,
}

impl SignedBlock {
    /// Builds an **unsigned** post-fork block (Go `BuildUnsigned`, non-epoch).
    ///
    /// # Errors
    /// Propagates a metadata-initialization failure (none expected here).
    pub fn build_unsigned(
        parent_id: Id,
        timestamp: i64,
        p_chain_height: u64,
        inner: Vec<u8>,
    ) -> crate::Result<Self> {
        let stateless_block = StatelessUnsignedBlock {
            parent_id,
            timestamp,
            p_chain_height,
            certificate: Vec::new(),
            block: inner,
        };
        let signature = Vec::new();
        let bytes = marshal_signed(&stateless_block, &signature);
        Self::initialize(stateless_block, signature, bytes)
    }

    /// Builds a **signed** post-fork block (Go `block.Build`, non-epoch).
    ///
    /// The signature is produced by `sign` over `Header{chain, parent, id}`'s
    /// serialized bytes — `id = sha256(unsigned bytes)`, exactly as Go signs
    /// `sha256(header.Bytes())`. The signer hashes the message internally
    /// (matching `staking::check_signature`).
    ///
    /// # Errors
    /// Returns [`crate::Error::InvalidCertificate`] if `certificate` is
    /// unparsable, or [`crate::Error::SignFailed`] if `sign` fails.
    pub fn build_signed(
        parent_id: Id,
        timestamp: i64,
        p_chain_height: u64,
        certificate: Vec<u8>,
        inner: Vec<u8>,
        chain_id: Id,
        sign: &SignFn<'_>,
    ) -> crate::Result<Self> {
        let stateless_block = StatelessUnsignedBlock {
            parent_id,
            timestamp,
            p_chain_height,
            certificate,
            block: inner,
        };
        // Marshal with an empty signature to recover the unsigned preimage.
        let empty_sig: Vec<u8> = Vec::new();
        let unsigned_with_empty = marshal_signed(&stateless_block, &empty_sig);
        let len_unsigned = unsigned_with_empty.len().saturating_sub(INT_LEN);
        let preimage = unsigned_with_empty.get(..len_unsigned).unwrap_or_default();
        let id = sha256_id(preimage);
        let header = Header::build(chain_id, parent_id, id);
        let signature = sign(header.bytes()).map_err(crate::Error::SignFailed)?;
        let bytes = marshal_signed(&stateless_block, &signature);
        Self::initialize(stateless_block, signature, bytes)
    }

    /// Finalizes a decoded block from its body + signature + raw bytes.
    ///
    /// # Errors
    /// Returns [`crate::Error::InvalidCertificate`] if the embedded cert is
    /// present but unparsable.
    pub fn initialize(
        stateless_block: StatelessUnsignedBlock,
        signature: Vec<u8>,
        bytes: Vec<u8>,
    ) -> crate::Result<Self> {
        let metadata = Metadata::initialize(&stateless_block, &signature, bytes)?;
        Ok(Self {
            stateless_block,
            signature,
            metadata,
        })
    }

    /// The block id.
    #[must_use]
    pub fn id(&self) -> Id {
        self.metadata.id
    }

    /// The parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.stateless_block.parent_id
    }

    /// The inner block bytes.
    #[must_use]
    pub fn inner_block(&self) -> &[u8] {
        &self.stateless_block.block
    }

    /// The serialized bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.metadata.bytes
    }

    /// The block timestamp (Unix seconds).
    #[must_use]
    pub fn timestamp(&self) -> i64 {
        self.stateless_block.timestamp
    }

    /// The P-Chain height.
    #[must_use]
    pub fn p_chain_height(&self) -> u64 {
        self.stateless_block.p_chain_height
    }

    /// The proposer node id (`EMPTY` if unsigned).
    #[must_use]
    pub fn proposer(&self) -> NodeId {
        self.metadata.proposer
    }

    /// The block signature.
    #[must_use]
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// Verifies the block against `chain_id`.
    ///
    /// # Errors
    /// See [`Metadata::verify`].
    pub fn verify(&self, chain_id: Id) -> crate::Result<()> {
        self.metadata
            .verify(&self.stateless_block, &self.signature, chain_id)
    }
}

/// `statelessGraniteBlock` — a post-Granite block carrying a non-zero `Epoch`.
#[derive(Debug, Clone)]
pub struct GraniteBlock {
    body: StatelessUnsignedGraniteBlock,
    signature: Vec<u8>,
    metadata: Metadata,
}

impl GraniteBlock {
    /// Builds an **unsigned** Granite block (Go `BuildUnsigned`, epoch set).
    ///
    /// # Errors
    /// Propagates a metadata-initialization failure (none expected here).
    pub fn build_unsigned(
        parent_id: Id,
        timestamp: i64,
        p_chain_height: u64,
        epoch: Epoch,
        inner: Vec<u8>,
    ) -> crate::Result<Self> {
        let body = StatelessUnsignedGraniteBlock {
            stateless_block: StatelessUnsignedBlock {
                parent_id,
                timestamp,
                p_chain_height,
                certificate: Vec::new(),
                block: inner,
            },
            epoch,
        };
        let signature = Vec::new();
        let bytes = marshal_granite(&body, &signature);
        Self::initialize(body, signature, bytes)
    }

    /// Builds a **signed** Granite block (Go `block.Build`, epoch set).
    ///
    /// # Errors
    /// Returns [`crate::Error::InvalidCertificate`] if `certificate` is
    /// unparsable, or [`crate::Error::SignFailed`] if `sign` fails.
    #[allow(clippy::too_many_arguments)]
    pub fn build_signed(
        parent_id: Id,
        timestamp: i64,
        p_chain_height: u64,
        epoch: Epoch,
        certificate: Vec<u8>,
        inner: Vec<u8>,
        chain_id: Id,
        sign: &SignFn<'_>,
    ) -> crate::Result<Self> {
        let body = StatelessUnsignedGraniteBlock {
            stateless_block: StatelessUnsignedBlock {
                parent_id,
                timestamp,
                p_chain_height,
                certificate,
                block: inner,
            },
            epoch,
        };
        let empty_sig: Vec<u8> = Vec::new();
        let unsigned_with_empty = marshal_granite(&body, &empty_sig);
        let len_unsigned = unsigned_with_empty.len().saturating_sub(INT_LEN);
        let preimage = unsigned_with_empty.get(..len_unsigned).unwrap_or_default();
        let id = sha256_id(preimage);
        let header = Header::build(chain_id, parent_id, id);
        let signature = sign(header.bytes()).map_err(crate::Error::SignFailed)?;
        let bytes = marshal_granite(&body, &signature);
        Self::initialize(body, signature, bytes)
    }

    /// Finalizes a decoded Granite block.
    ///
    /// # Errors
    /// Returns [`crate::Error::InvalidCertificate`] if the embedded cert is
    /// present but unparsable.
    pub fn initialize(
        body: StatelessUnsignedGraniteBlock,
        signature: Vec<u8>,
        bytes: Vec<u8>,
    ) -> crate::Result<Self> {
        let metadata = Metadata::initialize(&body.stateless_block, &signature, bytes)?;
        Ok(Self {
            body,
            signature,
            metadata,
        })
    }

    /// The block id.
    #[must_use]
    pub fn id(&self) -> Id {
        self.metadata.id
    }

    /// The parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.body.stateless_block.parent_id
    }

    /// The inner block bytes.
    #[must_use]
    pub fn inner_block(&self) -> &[u8] {
        &self.body.stateless_block.block
    }

    /// The serialized bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.metadata.bytes
    }

    /// The block timestamp (Unix seconds).
    #[must_use]
    pub fn timestamp(&self) -> i64 {
        self.body.stateless_block.timestamp
    }

    /// The P-Chain height.
    #[must_use]
    pub fn p_chain_height(&self) -> u64 {
        self.body.stateless_block.p_chain_height
    }

    /// The Granite epoch.
    #[must_use]
    pub fn epoch(&self) -> Epoch {
        self.body.epoch
    }

    /// The proposer node id (`EMPTY` if unsigned).
    #[must_use]
    pub fn proposer(&self) -> NodeId {
        self.metadata.proposer
    }

    /// The block signature.
    #[must_use]
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// Verifies the block against `chain_id`, rejecting a zero `Epoch` first
    /// (Go `errZeroEpoch`).
    ///
    /// # Errors
    /// Returns [`crate::Error::ZeroEpoch`] on a zero epoch, otherwise see
    /// [`Metadata::verify`].
    pub fn verify(&self, chain_id: Id) -> crate::Result<()> {
        if self.body.epoch.is_zero() {
            return Err(crate::Error::ZeroEpoch);
        }
        self.metadata
            .verify(&self.body.stateless_block, &self.signature, chain_id)
    }
}

/// Marshals a `statelessBlock` body + signature into interface-encoded bytes.
fn marshal_signed(body: &StatelessUnsignedBlock, signature: &[u8]) -> Vec<u8> {
    marshal_typed(
        TYPE_ID_STATELESS_BLOCK,
        &|p: &mut Packer| {
            body.marshal_into(p);
            p.pack_bytes(signature);
        },
        body.size()
            .saturating_add(4)
            .saturating_add(signature.len()),
    )
}

/// Marshals a `statelessGraniteBlock` body + signature into interface bytes.
fn marshal_granite(body: &StatelessUnsignedGraniteBlock, signature: &[u8]) -> Vec<u8> {
    marshal_typed(
        TYPE_ID_GRANITE_BLOCK,
        &|p: &mut Packer| {
            body.marshal_into(p);
            p.pack_bytes(signature);
        },
        body.size()
            .saturating_add(4)
            .saturating_add(signature.len()),
    )
}

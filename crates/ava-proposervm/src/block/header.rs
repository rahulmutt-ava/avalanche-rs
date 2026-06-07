// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Header` — the message that the proposer signs.
//!
//! Port of Go `vms/proposervm/block/header.go` + `BuildHeader` (build.go). The
//! header is `{chain, parent, body}` (three 32-byte ids in `serialize:"true"`
//! order), codec-marshaled with the 2-byte version prefix. The proposer signs
//! `header.bytes()` (the staking-cert signature scheme hashes it internally).

use ava_codec::packer::Packer;
use ava_types::id::{ID_LEN, Id};

use super::codec::CODEC_VERSION;
use super::stateless::{pack_id, unpack_id};

/// `statelessHeader` — the signed-over preimage `{Chain, Parent, Body}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// The chain id this block belongs to.
    pub chain: Id,
    /// The parent ProposerVM block id.
    pub parent: Id,
    /// The body id (= the block's own id, `sha256(unsigned bytes)`).
    pub body: Id,
    bytes: Vec<u8>,
}

impl Header {
    /// Builds and codec-marshals a header (Go `BuildHeader`). The returned
    /// header caches its serialized form (the bytes the proposer signs).
    #[must_use]
    pub fn build(chain: Id, parent: Id, body: Id) -> Self {
        let mut h = Self {
            chain,
            parent,
            body,
            bytes: Vec::new(),
        };
        h.bytes = h.marshal();
        h
    }

    /// The serialized header (with the 2-byte codec version prefix).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The chain id.
    #[must_use]
    pub fn chain_id(&self) -> Id {
        self.chain
    }

    /// The parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.parent
    }

    /// The body id.
    #[must_use]
    pub fn body_id(&self) -> Id {
        self.body
    }

    fn marshal(&self) -> Vec<u8> {
        // version prefix (2) + 3 raw ids (96).
        let mut p = Packer::with_max_size(2 + 3 * ID_LEN);
        p.pack_u16(CODEC_VERSION);
        pack_id(&mut p, &self.chain);
        pack_id(&mut p, &self.parent);
        pack_id(&mut p, &self.body);
        p.into_bytes()
    }

    /// Parses a header from its serialized bytes (Go `ParseHeader`).
    ///
    /// # Errors
    /// Returns [`crate::Error`] if the codec version is wrong or the buffer is
    /// malformed/has trailing bytes.
    pub fn parse(bytes: &[u8]) -> crate::Result<Self> {
        let mut p = Packer::new_read(bytes);
        let version = p.unpack_u16();
        if version != CODEC_VERSION {
            return Err(crate::Error::WrongCodecVersion {
                expected: CODEC_VERSION,
                got: version,
            });
        }
        let chain = unpack_id(&mut p);
        let parent = unpack_id(&mut p);
        let body = unpack_id(&mut p);
        if let Some(err) = p.error() {
            return Err(crate::Error::Codec(format!("{err:?}")));
        }
        if p.offset() != bytes.len() {
            return Err(crate::Error::Codec("trailing bytes".to_string()));
        }
        Ok(Self {
            chain,
            parent,
            body,
            bytes: bytes.to_vec(),
        })
    }
}

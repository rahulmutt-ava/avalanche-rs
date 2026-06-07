// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `option` block (Go `vms/proposervm/block/option.go`).
//!
//! An option wraps a child option's inner bytes. Its id is simply
//! `sha256(bytes)` over the **full** serialized form (there is no signature to
//! strip), and `verify` is always a no-op.

use ava_codec::packer::Packer;
use ava_types::id::Id;

use super::codec::{TYPE_ID_OPTION, marshal_typed};
use super::stateless::pack_id;
use crate::block::hash::sha256_id;

/// An `option` block (named `Option_` to avoid clashing with `core::option`).
#[derive(Debug, Clone)]
pub struct Option_ {
    parent_id: Id,
    inner_bytes: Vec<u8>,
    id: Id,
    bytes: Vec<u8>,
}

impl Option_ {
    /// Builds an option block (Go `BuildOption`).
    #[must_use]
    pub fn build(parent_id: Id, inner_bytes: Vec<u8>) -> Self {
        let bytes = marshal_typed(
            TYPE_ID_OPTION,
            &|p: &mut Packer| {
                pack_id(p, &parent_id);
                p.pack_bytes(&inner_bytes);
            },
            32usize.saturating_add(4).saturating_add(inner_bytes.len()),
        );
        Self::initialize(parent_id, inner_bytes, bytes)
    }

    /// Finalizes an option block from its decoded fields + raw bytes
    /// (Go `option.initialize`: `id = sha256(bytes)`).
    #[must_use]
    pub fn initialize(parent_id: Id, inner_bytes: Vec<u8>, bytes: Vec<u8>) -> Self {
        let id = sha256_id(&bytes);
        Self {
            parent_id,
            inner_bytes,
            id,
            bytes,
        }
    }

    /// The block id (`sha256` of the full bytes).
    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }

    /// The parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.parent_id
    }

    /// The inner option bytes.
    #[must_use]
    pub fn inner_block(&self) -> &[u8] {
        &self.inner_bytes
    }

    /// The serialized bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

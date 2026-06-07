// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Pre-fork blocks — a thin pass-through of the inner VM bytes.
//!
//! Before the ProposerVM activates, blocks are the bare inner-VM bytes with no
//! ProposerVM wrapping (Go `vms/proposervm/pre_fork_block.go`). The id and
//! parent of a pre-fork block are those of the inner block; there is no
//! ProposerVM-level codec framing. This type records the inner identity so the
//! VM wrapper (M3.23) can present a uniform `Block` surface across the fork.

use ava_types::id::Id;

/// A pre-fork block: the inner block bytes are returned verbatim, with the
/// inner id/parent surfaced directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreForkBlock {
    id: Id,
    parent_id: Id,
    inner_bytes: Vec<u8>,
}

impl PreForkBlock {
    /// Wraps the inner block's identity + bytes.
    #[must_use]
    pub fn new(id: Id, parent_id: Id, inner_bytes: Vec<u8>) -> Self {
        Self {
            id,
            parent_id,
            inner_bytes,
        }
    }

    /// The (inner) block id.
    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }

    /// The (inner) parent id.
    #[must_use]
    pub fn parent_id(&self) -> Id {
        self.parent_id
    }

    /// The inner block bytes (returned verbatim — pre-fork has no wrapping).
    #[must_use]
    pub fn inner_block(&self) -> &[u8] {
        &self.inner_bytes
    }

    /// The serialized bytes (identical to the inner bytes pre-fork).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.inner_bytes
    }
}

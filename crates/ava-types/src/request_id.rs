// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `RequestId` — the (node, chain, request, op) identity for p2p requests.
//!
//! Mirrors Go `ids.RequestID`. Plain value type, never serialized (field order
//! is irrelevant). Owning spec: `specs/03-core-primitives.md` §1.3.

use crate::id::Id;
use crate::node_id::NodeId;

/// Identifies an outstanding p2p request. Mirrors `ids.RequestID`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RequestId {
    /// The peer the request is associated with.
    pub node_id: NodeId,
    /// The chain the request is scoped to.
    pub chain_id: Id,
    /// The per-(node, chain) request counter.
    pub request_id: u32,
    /// The message op-code.
    pub op: u8,
}

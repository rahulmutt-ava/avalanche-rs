// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `RequestId` — the (node, chain, request, op) identity for p2p requests.
//!
//! TODO(M0.7): implement `RequestId { node_id, chain_id, request_id: u32, op: u8 }`
//! as a plain value type.
//! Owning spec: `specs/03-core-primitives.md` §1.3.

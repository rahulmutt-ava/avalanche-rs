// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 20-byte `NodeId` newtype with the `NodeID-` string prefix.
//!
//! TODO(M0.5): implement `NodeId([u8;20])`, `NODE_ID_PREFIX`, `from_slice`.
//! TODO(M0.6): `Display`/`FromStr` requiring the `NodeID-` prefix; serde forms.
//! TODO(M0.20): `From<[u8;20]>` consumed by `ava-crypto::node_id_from_cert`.
//! Owning spec: `specs/03-core-primitives.md` §1.1, §3.6.

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `proto/sync` frame <-> Rust type conversion (spec 15 §3.10).
//!
//! The generated prost/tonic types are reached via
//! [`tonic::include_proto!("sync")`] (build.rs, gated on the `sync` feature) and
//! re-exported here. All `bytes` fields are `bytes::Bytes` (`.bytes(["."])`), so
//! key/value/proof payloads are zero-copy.
//!
//! The only stateful conversion is the optional `MaybeBytes` field: a *present*
//! message (even with empty bytes) means [`Maybe::Some`]; an *absent* field
//! (`None`) means [`Maybe::Nothing`] — exactly Go `protoutils.MaybeToProto` /
//! `ProtoToMaybe`.

use std::collections::BTreeMap;

use bytes::Bytes;

use ava_types::id::{ID_LEN, Id};

use crate::key::Key;
use crate::maybe::Maybe;
use crate::proof::{ChangeProof, KeyChange, KeyValue, ProofNode, RangeProof, key_from_proto};
use crate::sync::error::{SyncError, SyncResult};

#[allow(
    missing_docs,
    dead_code,
    clippy::all,
    clippy::pedantic,
    unreachable_pub,
    clippy::doc_markdown
)]
mod pb {
    //! Generated tonic/prost types for the `sync` package (see `build.rs`).
    tonic::include_proto!("sync");
}

// Re-export the generated wire types so callers don't need to reach into `pb`.
pub use pb::proof_request::Request as ProofRequestKind;
pub use pb::proof_response::Response as ProofResponseKind;
pub use pb::{
    ChangeProofRequest, Key as ProtoKey, KeyChange as ProtoKeyChange, KeyValue as ProtoKeyValue,
    MaybeBytes, ProofNode as ProtoProofNode, ProofRequest, ProofResponse, RangeProofRequest,
};

// Re-export the generated proof message containers too (used by the marshalers).
pub use pb::{ChangeProof as ProtoChangeProof, RangeProof as ProtoRangeProof};

/// Converts a [`Maybe<Bytes>`] bound into the optional `MaybeBytes` field.
/// `Nothing` -> `None` (absent field); `Some(b)` -> `Some(MaybeBytes{value:b})`.
/// Mirrors Go `protoutils.MaybeToProto`.
#[must_use]
pub fn maybe_to_proto(m: &Maybe<Bytes>) -> Option<MaybeBytes> {
    match m {
        Maybe::Nothing => None,
        Maybe::Some(b) => Some(MaybeBytes { value: b.clone() }),
    }
}

/// Inverse of [`maybe_to_proto`]: an absent field is `Nothing`, a present field
/// (even empty) is `Some`. Mirrors Go `protoutils.ProtoToMaybe`.
#[must_use]
pub fn proto_to_maybe(mb: &Option<MaybeBytes>) -> Maybe<Bytes> {
    match mb {
        None => Maybe::Nothing,
        Some(b) => Maybe::Some(b.value.clone()),
    }
}

/// Converts an [`Option<&[u8]>`] bound into the optional `MaybeBytes` field.
#[must_use]
pub fn opt_to_proto(b: Option<&[u8]>) -> Option<MaybeBytes> {
    b.map(|v| MaybeBytes {
        value: Bytes::copy_from_slice(v),
    })
}

/// Extracts an `Option<Vec<u8>>` bound from an optional `MaybeBytes` field.
#[must_use]
pub fn proto_to_opt(mb: &Option<MaybeBytes>) -> Option<Vec<u8>> {
    mb.as_ref().map(|b| b.value.to_vec())
}

/// Builds a [`ProofRequest`] wrapping a [`RangeProofRequest`].
#[must_use]
pub fn range_proof_request(
    root: Id,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    key_limit: u32,
    bytes_limit: u32,
) -> ProofRequest {
    ProofRequest {
        request: Some(ProofRequestKind::RangeProof(RangeProofRequest {
            root_hash: Bytes::copy_from_slice(root.as_bytes()),
            start_key: opt_to_proto(start),
            end_key: opt_to_proto(end),
            key_limit,
            bytes_limit,
        })),
    }
}

/// Builds a [`ProofRequest`] wrapping a [`ChangeProofRequest`].
#[must_use]
pub fn change_proof_request(
    start_root: Id,
    end_root: Id,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    key_limit: u32,
    bytes_limit: u32,
) -> ProofRequest {
    ProofRequest {
        request: Some(ProofRequestKind::ChangeProof(ChangeProofRequest {
            start_root_hash: Bytes::copy_from_slice(start_root.as_bytes()),
            end_root_hash: Bytes::copy_from_slice(end_root.as_bytes()),
            start_key: opt_to_proto(start),
            end_key: opt_to_proto(end),
            key_limit,
            bytes_limit,
        })),
    }
}

/// Decodes a 32-byte root hash from a wire `bytes` field.
///
/// # Errors
/// Returns [`SyncError::InvalidRootHash`] if the length isn't [`ID_LEN`].
pub fn root_from_bytes(b: &[u8]) -> SyncResult<Id> {
    if b.len() != ID_LEN {
        return Err(SyncError::InvalidRootHash);
    }
    Id::from_slice(b).map_err(|_| SyncError::InvalidRootHash)
}

/// Serializes a prost message to its proto wire bytes.
#[must_use]
pub fn encode<M: prost::Message>(msg: &M) -> Vec<u8> {
    msg.encode_to_vec()
}

/// Deserializes a prost message from proto wire bytes.
///
/// # Errors
/// Returns [`SyncError::Decode`] if the bytes are not a valid frame.
pub fn decode<M: prost::Message + Default>(bytes: &[u8]) -> SyncResult<M> {
    M::decode(bytes).map_err(|e| SyncError::Decode(e.to_string()))
}

// ---------------------------------------------------------------------------
// proof message <-> Rust proof type conversions
//
// The crate's `Proof`/`RangeProof`/`ChangeProof` are *encoded* by their own
// byte-exact hand-rolled marshalers (`encode_proto`, proven against Go golden
// vectors in M1.17/M1.18). For *decoding* a peer's response we go through the
// generated prost types (same schema), then convert into the crate types so the
// existing byte-exact verification machinery runs unchanged.
// ---------------------------------------------------------------------------

/// Converts a generated `ProofNode` into the crate [`ProofNode`].
fn proof_node_from_proto(pn: &ProtoProofNode) -> SyncResult<ProofNode> {
    let key = match &pn.key {
        Some(k) => key_from_proto(&k.value, usize::try_from(k.length).unwrap_or(usize::MAX))
            .map_err(|_| SyncError::Decode("invalid proof node key".to_string()))?,
        None => Key::empty(),
    };
    let value_or_hash = proto_to_maybe(&pn.value_or_hash);
    let mut children: BTreeMap<u8, Id> = BTreeMap::new();
    for (index, id_bytes) in &pn.children {
        let index = u8::try_from(*index)
            .map_err(|_| SyncError::Decode("child index out of range".to_string()))?;
        let id = Id::from_slice(id_bytes)
            .map_err(|_| SyncError::Decode("invalid child id".to_string()))?;
        children.insert(index, id);
    }
    Ok(ProofNode {
        key,
        value_or_hash,
        children,
    })
}

fn proof_nodes_from_proto(nodes: &[ProtoProofNode]) -> SyncResult<Vec<ProofNode>> {
    nodes.iter().map(proof_node_from_proto).collect()
}

/// Decodes a `sync.RangeProof` wire frame into the crate [`RangeProof`].
///
/// # Errors
/// Returns [`SyncError::Decode`] if the frame is malformed.
pub fn range_proof_from_bytes(bytes: &[u8]) -> SyncResult<RangeProof> {
    let p: ProtoRangeProof = decode(bytes)?;
    Ok(RangeProof {
        start_proof: proof_nodes_from_proto(&p.start_proof)?,
        end_proof: proof_nodes_from_proto(&p.end_proof)?,
        key_values: p
            .key_values
            .iter()
            .map(|kv| KeyValue {
                key: kv.key.to_vec(),
                value: kv.value.to_vec(),
            })
            .collect(),
    })
}

/// Decodes a `sync.ChangeProof` wire frame into the crate [`ChangeProof`].
///
/// # Errors
/// Returns [`SyncError::Decode`] if the frame is malformed.
pub fn change_proof_from_bytes(bytes: &[u8]) -> SyncResult<ChangeProof> {
    let p: ProtoChangeProof = decode(bytes)?;
    Ok(ChangeProof {
        start_proof: proof_nodes_from_proto(&p.start_proof)?,
        end_proof: proof_nodes_from_proto(&p.end_proof)?,
        key_changes: p
            .key_changes
            .iter()
            .map(|kc| KeyChange {
                key: kc.key.to_vec(),
                value: proto_to_maybe(&kc.value),
            })
            .collect(),
    })
}

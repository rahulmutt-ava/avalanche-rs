// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Op` opcodes + the `UNREQUESTED_OPS` / `FAILED_TO_RESPONSE_OPS`
//! classification sets — a byte-exact port of `message/ops.go` (specs/05
//! §1.2/§2.2).
//!
//! `Op` is an **internal routing/metrics tag, not on the wire** — the wire
//! identity of a message is its protobuf `oneof` field number. We still
//! reproduce the exact `iota` ordering and `String()` names because other crates
//! (consensus, VM framework) match on `Op` and metrics labels use the names.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use crate::error::Error;
use crate::proto::p2p;

/// Opcode — internal routing/metrics tag (NOT on the wire). Values mirror
/// `message/ops.go` `iota` exactly; **do not reorder**.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Op {
    // Handshake:
    /// `ping` (proto tag 11).
    Ping = 0,
    /// `pong` (proto tag 12).
    Pong,
    /// `handshake` (proto tag 13).
    Handshake,
    /// `get_peerlist` (proto tag 35).
    GetPeerList,
    /// `peerlist` (proto tag 14).
    PeerList,
    // State sync:
    /// `get_state_summary_frontier` (proto tag 15).
    GetStateSummaryFrontier,
    /// `get_state_summary_frontier_failed` (internal).
    GetStateSummaryFrontierFailed,
    /// `state_summary_frontier` (proto tag 16).
    StateSummaryFrontier,
    /// `get_accepted_state_summary` (proto tag 17).
    GetAcceptedStateSummary,
    /// `get_accepted_state_summary_failed` (internal).
    GetAcceptedStateSummaryFailed,
    /// `accepted_state_summary` (proto tag 18).
    AcceptedStateSummary,
    // Bootstrapping:
    /// `get_accepted_frontier` (proto tag 19).
    GetAcceptedFrontier,
    /// `get_accepted_frontier_failed` (internal).
    GetAcceptedFrontierFailed,
    /// `accepted_frontier` (proto tag 20).
    AcceptedFrontier,
    /// `get_accepted` (proto tag 21).
    GetAccepted,
    /// `get_accepted_failed` (internal).
    GetAcceptedFailed,
    /// `accepted` (proto tag 22).
    Accepted,
    /// `get_ancestors` (proto tag 23).
    GetAncestors,
    /// `get_ancestors_failed` (internal).
    GetAncestorsFailed,
    /// `ancestors` (proto tag 24).
    Ancestors,
    // Consensus:
    /// `get` (proto tag 25).
    Get,
    /// `get_failed` (internal).
    GetFailed,
    /// `put` (proto tag 26).
    Put,
    /// `push_query` (proto tag 27).
    PushQuery,
    /// `pull_query` (proto tag 28).
    PullQuery,
    /// `query_failed` (internal).
    QueryFailed,
    /// `chits` (proto tag 29).
    Chits,
    // Application:
    /// `app_request` (proto tag 30).
    AppRequest,
    /// `app_error` (proto tag 34).
    AppError,
    /// `app_response` (proto tag 31).
    AppResponse,
    /// `app_gossip` (proto tag 32).
    AppGossip,
    // Internal:
    /// `connected` (internal).
    Connected,
    /// `disconnected` (internal).
    Disconnected,
    /// `notify` (internal).
    Notify,
    /// `gossip_request` (internal).
    GossipRequest,
    // Simplex:
    /// `simplex` (proto tag 36).
    Simplex,
}

impl Op {
    /// The exact `String()` name from `message/ops.go` (used by metrics labels).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Op::Ping => "ping",
            Op::Pong => "pong",
            Op::Handshake => "handshake",
            Op::GetPeerList => "get_peerlist",
            Op::PeerList => "peerlist",
            Op::GetStateSummaryFrontier => "get_state_summary_frontier",
            Op::GetStateSummaryFrontierFailed => "get_state_summary_frontier_failed",
            Op::StateSummaryFrontier => "state_summary_frontier",
            Op::GetAcceptedStateSummary => "get_accepted_state_summary",
            Op::GetAcceptedStateSummaryFailed => "get_accepted_state_summary_failed",
            Op::AcceptedStateSummary => "accepted_state_summary",
            Op::GetAcceptedFrontier => "get_accepted_frontier",
            Op::GetAcceptedFrontierFailed => "get_accepted_frontier_failed",
            Op::AcceptedFrontier => "accepted_frontier",
            Op::GetAccepted => "get_accepted",
            Op::GetAcceptedFailed => "get_accepted_failed",
            Op::Accepted => "accepted",
            Op::GetAncestors => "get_ancestors",
            Op::GetAncestorsFailed => "get_ancestors_failed",
            Op::Ancestors => "ancestors",
            Op::Get => "get",
            Op::GetFailed => "get_failed",
            Op::Put => "put",
            Op::PushQuery => "push_query",
            Op::PullQuery => "pull_query",
            Op::QueryFailed => "query_failed",
            Op::Chits => "chits",
            Op::AppRequest => "app_request",
            Op::AppError => "app_error",
            Op::AppResponse => "app_response",
            Op::AppGossip => "app_gossip",
            Op::Connected => "connected",
            Op::Disconnected => "disconnected",
            Op::Notify => "notify",
            Op::GossipRequest => "gossip_request",
            Op::Simplex => "simplex",
        }
    }

    /// Maps a `Message.message` oneof variant to its `Op` (mirrors Go `ToOp`).
    /// Returns [`Error::UnknownOp`] for the compressed wrapper or any variant
    /// without an op (none currently).
    ///
    /// # Errors
    /// Returns [`Error::UnknownOp`] if the variant carries no parseable op
    /// (e.g. the `compressed_zstd` wrapper).
    pub fn of(m: &p2p::message::Message) -> Result<Op, Error> {
        use p2p::message::Message as M;
        let op = match m {
            M::Ping(_) => Op::Ping,
            M::Pong(_) => Op::Pong,
            M::Handshake(_) => Op::Handshake,
            M::GetPeerList(_) => Op::GetPeerList,
            M::PeerList(_) => Op::PeerList,
            M::GetStateSummaryFrontier(_) => Op::GetStateSummaryFrontier,
            M::StateSummaryFrontier(_) => Op::StateSummaryFrontier,
            M::GetAcceptedStateSummary(_) => Op::GetAcceptedStateSummary,
            M::AcceptedStateSummary(_) => Op::AcceptedStateSummary,
            M::GetAcceptedFrontier(_) => Op::GetAcceptedFrontier,
            M::AcceptedFrontier(_) => Op::AcceptedFrontier,
            M::GetAccepted(_) => Op::GetAccepted,
            M::Accepted(_) => Op::Accepted,
            M::GetAncestors(_) => Op::GetAncestors,
            M::Ancestors(_) => Op::Ancestors,
            M::Get(_) => Op::Get,
            M::Put(_) => Op::Put,
            M::PushQuery(_) => Op::PushQuery,
            M::PullQuery(_) => Op::PullQuery,
            M::Chits(_) => Op::Chits,
            M::AppRequest(_) => Op::AppRequest,
            M::AppResponse(_) => Op::AppResponse,
            M::AppError(_) => Op::AppError,
            M::AppGossip(_) => Op::AppGossip,
            M::Simplex(_) => Op::Simplex,
            // The compressed wrapper has no op (mirrors Go `ToOp` default).
            M::CompressedZstd(_) => return Err(Error::UnknownOp),
        };
        Ok(op)
    }
}

/// Operations expected to be seen without having been requested
/// (`message/ops.go` `UnrequestedOps`). Used by the timeout/response bookkeeping
/// in the consensus router.
#[must_use]
pub fn unrequested_ops() -> &'static HashSet<Op> {
    static SET: LazyLock<HashSet<Op>> = LazyLock::new(|| {
        HashSet::from([
            Op::GetAcceptedFrontier,
            Op::GetAccepted,
            Op::GetAncestors,
            Op::Get,
            Op::PushQuery,
            Op::PullQuery,
            Op::AppRequest,
            Op::AppGossip,
            Op::GetStateSummaryFrontier,
            Op::GetAcceptedStateSummary,
            Op::Simplex,
        ])
    });
    &SET
}

/// Maps each `*Failed` internal op to its successful counterpart
/// (`message/ops.go` `FailedToResponseOps`).
#[must_use]
pub fn failed_to_response_ops() -> &'static HashMap<Op, Op> {
    static MAP: LazyLock<HashMap<Op, Op>> = LazyLock::new(|| {
        HashMap::from([
            (Op::GetStateSummaryFrontierFailed, Op::StateSummaryFrontier),
            (Op::GetAcceptedStateSummaryFailed, Op::AcceptedStateSummary),
            (Op::GetAcceptedFrontierFailed, Op::AcceptedFrontier),
            (Op::GetAcceptedFailed, Op::Accepted),
            (Op::GetAncestorsFailed, Op::Ancestors),
            (Op::GetFailed, Op::Put),
            (Op::QueryFailed, Op::Chits),
            (Op::AppError, Op::AppResponse),
        ])
    });
    &MAP
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;
    use crate::error::Error;

    #[test]
    fn of_rejects_compressed_wrapper() {
        let m = p2p::message::Message::CompressedZstd(bytes::Bytes::from_static(&[1, 2, 3]));
        assert_matches!(Op::of(&m), Err(Error::UnknownOp));
    }

    #[test]
    fn iota_is_contiguous() {
        // The simplex op is the last and highest value (35).
        assert_eq!(Op::Simplex as u8, 35);
        assert_eq!(Op::Connected as u8, 31);
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Wire → engine op decoding: the inverse of [`OutboundSender`].
//!
//! [`decode_inbound`] maps an [`ava_message::codec::InboundMessage`] (from the
//! network layer) to an [`EngineInboundMessage`] (for the engine router). It
//! returns `None` for ops the engine does not consume (Ping/Pong/Handshake/
//! PeerList/StateSummary/* and any op with no matching [`InboundOp`] variant),
//! so the router can silently drop them. `AppRequest`/`AppResponse`/
//! `AppGossip`/`AppError` decode to `InboundOp::AppRequest`/`AppResponse`/
//! `AppGossip`/`AppRequestFailed` (Task 7: engine inbound App routing).
//!
//! This is the decode half of the network→consensus boundary (specs/06 §5.1).
//! The encode half is [`ava_engine::networking::sender::OutboundSender`].
//!
//! [`OutboundSender`]: ava_engine::networking::sender::OutboundSender
//! [`EngineInboundMessage`]: ava_engine::networking::router::InboundMessage

use ava_engine::networking::router::{InboundMessage as EngineInboundMessage, InboundOp};
use ava_message::codec::InboundMessage;
use ava_message::proto::p2p::message::Message as M;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

/// Decode a network-layer [`InboundMessage`] into an [`EngineInboundMessage`]
/// for the engine router.
///
/// Returns `None` for ops the engine does not handle (Ping/Pong/Handshake/
/// PeerList/GetPeerList and StateSummary ops), or when a required field
/// (chain id, container id) is malformed/missing (wrong byte length).
///
/// # Design note
///
/// `node` is passed as an argument rather than read from `msg.sender` so the
/// function remains agnostic to how the message was produced — tests can build
/// synthetic [`InboundMessage`] values via [`ava_message::codec::MsgBuilder`]
/// (which sets `sender` to `NodeId::default()`) and supply the sender
/// explicitly.
pub fn decode_inbound(node: NodeId, msg: &InboundMessage) -> Option<EngineInboundMessage> {
    let (chain, op) = match &msg.message {
        // --- Bootstrap: frontier -------------------------------------------------
        M::GetAcceptedFrontier(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::GetAcceptedFrontier {
                request_id: m.request_id,
            };
            (chain, op)
        }
        M::AcceptedFrontier(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_id = parse_id(m.container_id.as_ref())?;
            let op = InboundOp::AcceptedFrontier {
                request_id: m.request_id,
                container_id,
            };
            (chain, op)
        }

        // --- Bootstrap: accepted -------------------------------------------------
        M::GetAccepted(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_ids = parse_ids(&m.container_ids)?;
            let op = InboundOp::GetAccepted {
                request_id: m.request_id,
                container_ids,
            };
            (chain, op)
        }
        M::Accepted(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_ids = parse_ids(&m.container_ids)?;
            let op = InboundOp::Accepted {
                request_id: m.request_id,
                container_ids,
            };
            (chain, op)
        }

        // --- Bootstrap: ancestors ------------------------------------------------
        M::GetAncestors(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_id = parse_id(m.container_id.as_ref())?;
            // `engine_type` (set to `ENGINE_TYPE_CHAIN` by `OutboundSender::send_get_ancestors`)
            // is intentionally dropped: only linear Chain engine routing is supported.
            let op = InboundOp::GetAncestors {
                request_id: m.request_id,
                container_id,
            };
            (chain, op)
        }
        M::Ancestors(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let containers: Vec<Vec<u8>> = m.containers.iter().map(|b| b.to_vec()).collect();
            let op = InboundOp::Ancestors {
                request_id: m.request_id,
                containers,
            };
            (chain, op)
        }

        // --- Consensus: fetch ----------------------------------------------------
        M::Get(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_id = parse_id(m.container_id.as_ref())?;
            let op = InboundOp::Get {
                request_id: m.request_id,
                container_id,
            };
            (chain, op)
        }
        M::Put(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::Put {
                request_id: m.request_id,
                container: m.container.to_vec(),
            };
            (chain, op)
        }

        // --- Consensus: query/vote -----------------------------------------------
        M::PushQuery(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::PushQuery {
                request_id: m.request_id,
                container: m.container.to_vec(),
                requested_height: m.requested_height,
            };
            (chain, op)
        }
        M::PullQuery(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let container_id = parse_id(m.container_id.as_ref())?;
            let op = InboundOp::PullQuery {
                request_id: m.request_id,
                container_id,
                requested_height: m.requested_height,
            };
            (chain, op)
        }
        M::Chits(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let preferred_id = parse_id(m.preferred_id.as_ref())?;
            let preferred_id_at_height = parse_id(m.preferred_id_at_height.as_ref())?;
            let accepted_id = parse_id(m.accepted_id.as_ref())?;
            let op = InboundOp::Chits {
                request_id: m.request_id,
                preferred_id,
                preferred_id_at_height,
                accepted_id,
                accepted_height: m.accepted_height,
            };
            (chain, op)
        }

        // --- App messages (Task 7: engine inbound App routing) ------------------
        M::AppRequest(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::AppRequest {
                request_id: m.request_id,
                deadline_nanos: m.deadline,
                bytes: m.app_bytes.to_vec(),
            };
            (chain, op)
        }
        M::AppResponse(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::AppResponse {
                request_id: m.request_id,
                bytes: m.app_bytes.to_vec(),
            };
            (chain, op)
        }
        M::AppGossip(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::AppGossip {
                bytes: m.app_bytes.to_vec(),
            };
            (chain, op)
        }
        M::AppError(m) => {
            let chain = parse_id(m.chain_id.as_ref())?;
            let op = InboundOp::AppRequestFailed {
                request_id: m.request_id,
                code: m.error_code,
                message: m.error_message.clone(),
            };
            (chain, op)
        }

        // --- Non-consensus ops (peer layer / not yet in InboundOp) ---------------
        // Ping/Pong/Handshake/GetPeerList/PeerList are handled by the peer actor
        // inline and never reach the router — but guard here defensively.
        M::Ping(_)
        | M::Pong(_)
        | M::Handshake(_)
        | M::GetPeerList(_)
        | M::PeerList(_)
        // StateSummary bootstrap has no InboundOp variants yet.
        | M::GetStateSummaryFrontier(_)
        | M::StateSummaryFrontier(_)
        | M::GetAcceptedStateSummary(_)
        | M::AcceptedStateSummary(_)
        // Simplex is a separate consensus path.
        | M::Simplex(_)
        // Compressed wrapper should never arrive here (already decoded).
        | M::CompressedZstd(_) => {
            tracing::trace!(op = ?msg.op, "non-consensus inbound op; ignored by decode_inbound");
            return None;
        }
    };

    Some(EngineInboundMessage { node, chain, op })
}

/// Parse a 32-byte byte slice into an [`Id`].
///
/// Returns `None` if the slice is not exactly 32 bytes, so a malformed
/// peer message is silently dropped rather than panicking.
fn parse_id(b: &[u8]) -> Option<Id> {
    let arr: [u8; 32] = b.try_into().ok()?;
    Some(Id::from(arr))
}

/// Parse a slice of byte slices into a `Vec<Id>`.
///
/// Returns `None` if any element is not exactly 32 bytes.
fn parse_ids<B: AsRef<[u8]>>(ids: &[B]) -> Option<Vec<Id>> {
    ids.iter().map(|b| parse_id(b.as_ref())).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use ava_engine::networking::router::InboundOp;
    use ava_message::codec::{Compression, MsgBuilder};
    use ava_message::proto::p2p;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use bytes::Bytes;

    use super::decode_inbound;

    fn chain() -> Id {
        Id::from([0xABu8; 32])
    }

    fn chain_bytes() -> Bytes {
        Bytes::copy_from_slice(chain().as_bytes())
    }

    fn parse(inner: p2p::message::Message) -> ava_message::codec::InboundMessage {
        let m = p2p::Message {
            message: Some(inner),
        };
        let mb = MsgBuilder::default();
        let (bytes, _, _) = mb.marshal(&m, Compression::None).expect("marshal");
        mb.parse_inbound(&bytes).expect("parse_inbound")
    }

    #[test]
    fn decodes_get_accepted_frontier() {
        // Build the wire bytes for GetAcceptedFrontier{chain, request_id: 9}
        // using the ava-message builder; parse into codec::InboundMessage.
        let msg = parse(p2p::message::Message::GetAcceptedFrontier(
            p2p::GetAcceptedFrontier {
                chain_id: chain_bytes(),
                request_id: 9,
                deadline: 1_000_000_000,
            },
        ));
        let node = NodeId::from([1u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(got.op, InboundOp::GetAcceptedFrontier { request_id: 9 });
    }

    #[test]
    fn drops_non_consensus_ops() {
        // A Ping message decodes to None.
        let ping = parse(p2p::message::Message::Ping(p2p::Ping { uptime: 42 }));
        assert!(decode_inbound(NodeId::from([2u8; 20]), &ping).is_none());
    }

    #[test]
    fn decodes_accepted_frontier() {
        let container_id = Id::from([0xCDu8; 32]);
        let msg = parse(p2p::message::Message::AcceptedFrontier(
            p2p::AcceptedFrontier {
                chain_id: chain_bytes(),
                request_id: 7,
                container_id: Bytes::copy_from_slice(container_id.as_bytes()),
            },
        ));
        let node = NodeId::from([3u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(
            got.op,
            InboundOp::AcceptedFrontier {
                request_id: 7,
                container_id,
            }
        );
    }

    #[test]
    fn decodes_get_accepted() {
        let id1 = Id::from([0x11u8; 32]);
        let id2 = Id::from([0x22u8; 32]);
        let msg = parse(p2p::message::Message::GetAccepted(p2p::GetAccepted {
            chain_id: chain_bytes(),
            request_id: 5,
            deadline: 1_000_000_000,
            container_ids: vec![
                Bytes::copy_from_slice(id1.as_bytes()),
                Bytes::copy_from_slice(id2.as_bytes()),
            ],
        }));
        let node = NodeId::from([4u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(
            got.op,
            InboundOp::GetAccepted {
                request_id: 5,
                container_ids: vec![id1, id2],
            }
        );
    }

    #[test]
    fn decodes_app_request() {
        let msg = parse(p2p::message::Message::AppRequest(p2p::AppRequest {
            chain_id: chain_bytes(),
            request_id: 1,
            deadline: 1_000_000_000,
            app_bytes: Bytes::from_static(&[0x01, 0x02]),
        }));
        let node = NodeId::from([5u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(
            got.op,
            InboundOp::AppRequest {
                request_id: 1,
                deadline_nanos: 1_000_000_000,
                bytes: vec![0x01, 0x02],
            }
        );
    }

    #[test]
    fn decodes_app_response() {
        let msg = parse(p2p::message::Message::AppResponse(p2p::AppResponse {
            chain_id: chain_bytes(),
            request_id: 2,
            app_bytes: Bytes::from_static(&[0x03, 0x04]),
        }));
        let node = NodeId::from([15u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(
            got.op,
            InboundOp::AppResponse {
                request_id: 2,
                bytes: vec![0x03, 0x04],
            }
        );
    }

    #[test]
    fn decodes_app_gossip() {
        let msg = parse(p2p::message::Message::AppGossip(p2p::AppGossip {
            chain_id: chain_bytes(),
            app_bytes: Bytes::from_static(&[0x05, 0x06]),
        }));
        let node = NodeId::from([16u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(
            got.op,
            InboundOp::AppGossip {
                bytes: vec![0x05, 0x06],
            }
        );
    }

    #[test]
    fn decodes_app_error() {
        let msg = parse(p2p::message::Message::AppError(p2p::AppError {
            chain_id: chain_bytes(),
            request_id: 3,
            error_code: 7,
            error_message: "boom".to_string(),
        }));
        let node = NodeId::from([17u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(got.chain, chain());
        assert_eq!(got.node, node);
        assert_eq!(
            got.op,
            InboundOp::AppRequestFailed {
                request_id: 3,
                code: 7,
                message: "boom".to_string(),
            }
        );
    }

    #[test]
    fn drops_state_summary_frontier() {
        let msg = parse(p2p::message::Message::StateSummaryFrontier(
            p2p::StateSummaryFrontier {
                chain_id: chain_bytes(),
                request_id: 2,
                summary: Bytes::from_static(&[0xDE, 0xAD]),
            },
        ));
        assert!(decode_inbound(NodeId::from([6u8; 20]), &msg).is_none());
    }

    #[test]
    fn decodes_chits() {
        let preferred = Id::from([0x11u8; 32]);
        let preferred_at_height = Id::from([0x22u8; 32]);
        let accepted = Id::from([0x33u8; 32]);
        let msg = parse(p2p::message::Message::Chits(p2p::Chits {
            chain_id: chain_bytes(),
            request_id: 42,
            preferred_id: Bytes::copy_from_slice(preferred.as_bytes()),
            preferred_id_at_height: Bytes::copy_from_slice(preferred_at_height.as_bytes()),
            accepted_id: Bytes::copy_from_slice(accepted.as_bytes()),
            accepted_height: 100,
        }));
        let node = NodeId::from([7u8; 20]);
        let got = decode_inbound(node, &msg).expect("decode");
        assert_eq!(
            got.op,
            InboundOp::Chits {
                request_id: 42,
                preferred_id: preferred,
                preferred_id_at_height: preferred_at_height,
                accepted_id: accepted,
                accepted_height: 100,
            }
        );
    }

    #[test]
    fn malformed_chain_id_returns_none() {
        // A chain_id of wrong length (< 32 bytes) should return None.
        let msg = parse(p2p::message::Message::GetAcceptedFrontier(
            p2p::GetAcceptedFrontier {
                chain_id: Bytes::from_static(&[0x01, 0x02]),
                request_id: 1,
                deadline: 1_000_000_000,
            },
        ));
        assert!(decode_inbound(NodeId::from([8u8; 20]), &msg).is_none());
    }
}

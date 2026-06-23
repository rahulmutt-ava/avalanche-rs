// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `MsgBuilder` — the inbound/outbound codec: proto3 marshal/unmarshal plus the
//! **recursive zstd packing** trick from `message/messages.go` (specs/05 §1.3,
//! 15 §4.2).
//!
//! avalanchego avoids a compression flag by nesting a `Message` inside a
//! `Message`: a compressed message is marshaled, zstd-compressed, then wrapped
//! in `Message{ compressed_zstd: c }` and marshaled again. The receiver decodes
//! the outer `Message`; if `compressed_zstd` is non-empty it decompresses
//! (bounded to `MAX_MESSAGE_SIZE` to prevent decompression-bomb over-reads) and
//! decodes the inner `Message`.
//!
//! Only **zstd** is produced (R4): byte-equality of compressed output is **not**
//! required for interop — only mutual decodability, which is deterministic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use prost::Message as _;

use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::frame::MAX_MESSAGE_SIZE;
use crate::ops::Op;
use crate::proto::p2p;

/// Maximum message timeout used to clamp inbound deadlines
/// (`network-maximum-inbound-message-timeout`, default 10s; specs/05 §7).
pub const DEFAULT_MAX_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// The compression algorithm applied to an outbound message. Mirrors Go
/// `compression.Type` for the two live variants (`TypeNone` / `TypeZstd`);
/// gzip/snappy are not produced (specs/05 §1.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Compression {
    /// No compression — the inner `Message` is sent as-is.
    #[default]
    None,
    /// zstd recursive packing.
    Zstd,
}

/// A decoded inbound message handed to the router. Owns its proto payload.
///
/// `expiration` is `min(deadline, max_message_timeout)` from now for ops that
/// carry a deadline; `None` means "no deadline" (Go's `mockable.MaxTime`).
///
/// `sender` is the NodeId of the peer this message arrived from. The
/// [`MsgBuilder`] sets it to `NodeId::default()`; the peer actor overwrites it
/// with `self.id` before forwarding to the router (the codec cannot know the
/// sender — only the peer actor does).
#[derive(Debug)]
pub struct InboundMessage {
    /// The NodeId of the peer this message arrived from.
    pub sender: NodeId,
    /// The opcode of the unwrapped message.
    pub op: Op,
    /// The unwrapped oneof variant.
    pub message: p2p::message::Message,
    /// Expiration instant, if the op carries a deadline.
    pub expiration: Option<Instant>,
    /// Bytes saved by compression (decompressed_len - compressed_len); `0` when
    /// the message was not compressed.
    pub bytes_saved_compression: i64,
}

/// Ready-to-send framed bytes (the 4-byte length prefix is added at write time).
#[derive(Clone, Debug)]
pub struct OutboundMessage {
    /// If true, this message bypasses outbound throttling (handshake / PeerList
    /// replies). Mirrors Go `OutboundMessage.BypassThrottling`.
    pub bypass_throttling: bool,
    /// The opcode of the message.
    pub op: Op,
    /// The marshaled proto bytes (possibly zstd-wrapped).
    pub bytes: Bytes,
    /// Bytes saved by compression (negative/zero when uncompressed, mirroring
    /// Go's `inner_len - outer_len`).
    pub bytes_saved_compression: i64,
}

/// The proto marshal/unmarshal + zstd recursive packer (`message/messages.go`
/// `msgBuilder`).
#[derive(Clone)]
pub struct MsgBuilder {
    max_message_timeout: Duration,
}

impl Default for MsgBuilder {
    fn default() -> Self {
        Self {
            max_message_timeout: DEFAULT_MAX_MESSAGE_TIMEOUT,
        }
    }
}

impl MsgBuilder {
    /// Builds a `MsgBuilder` with an explicit max inbound message timeout.
    #[must_use]
    pub fn new(max_message_timeout: Duration) -> Self {
        Self {
            max_message_timeout,
        }
    }

    /// The configured max inbound message timeout.
    #[must_use]
    pub fn max_message_timeout(&self) -> Duration {
        self.max_message_timeout
    }

    /// Marshals `m` and optionally zstd-packs it. Returns
    /// `(bytes, bytes_saved, op)`.
    ///
    /// Mirrors Go `msgBuilder.marshal`: when zstd, the inner bytes are
    /// compressed and re-wrapped in `Message{compressed_zstd}`;
    /// `bytes_saved = inner_len - outer_len`.
    ///
    /// # Errors
    /// Returns [`Error::UnknownOp`] if `m`'s oneof is unset/has no op,
    /// [`Error::Compression`] on a zstd failure.
    pub fn marshal(&self, m: &p2p::Message, c: Compression) -> Result<(Bytes, i64, Op)> {
        let inner = m.encode_to_vec();
        let op = match &m.message {
            Some(variant) => Op::of(variant)?,
            None => return Err(Error::UnknownOp),
        };

        match c {
            Compression::None => Ok((Bytes::from(inner), 0, op)),
            Compression::Zstd => {
                let compressed = zstd::bulk::compress(&inner, zstd::DEFAULT_COMPRESSION_LEVEL)
                    .map_err(|e| Error::Compression(e.to_string()))?;
                let outer = p2p::Message {
                    message: Some(p2p::message::Message::CompressedZstd(Bytes::from(
                        compressed,
                    ))),
                };
                let outer_bytes = outer.encode_to_vec();
                // bytes_saved = inner_len - outer_len (mirrors Go).
                let saved = i64::try_from(inner.len())
                    .unwrap_or(i64::MAX)
                    .saturating_sub(i64::try_from(outer_bytes.len()).unwrap_or(i64::MAX));
                Ok((Bytes::from(outer_bytes), saved, op))
            }
        }
    }

    /// Decodes the outer `Message`; if `compressed_zstd` is set, decompresses
    /// (bounded to `MAX_MESSAGE_SIZE`) and decodes the inner `Message`. Returns
    /// `(message, bytes_saved, op)`.
    ///
    /// Mirrors Go `msgBuilder.unmarshal`.
    ///
    /// # Errors
    /// Returns [`Error::ProtoDecode`] on a malformed frame,
    /// [`Error::Compression`] on a zstd failure or over-long payload, and
    /// [`Error::UnknownOp`] if the (inner) oneof has no parseable op.
    pub fn unmarshal(&self, b: &[u8]) -> Result<(p2p::Message, i64, Op)> {
        let outer = p2p::Message::decode(b)?;

        let compressed = match &outer.message {
            Some(p2p::message::Message::CompressedZstd(c)) if !c.is_empty() => c.clone(),
            // Not compressed (or an empty wrapper): treat as the final message.
            _ => {
                let op = match &outer.message {
                    Some(variant) => Op::of(variant)?,
                    None => return Err(Error::UnknownOp),
                };
                return Ok((outer, 0, op));
            }
        };

        // Bound the decompressed size to MAX_MESSAGE_SIZE so a hostile peer
        // cannot force an unbounded allocation (decompression-bomb guard).
        let capacity = MAX_MESSAGE_SIZE as usize;
        let decompressed = zstd::bulk::decompress(&compressed, capacity)
            .map_err(|e| Error::Compression(e.to_string()))?;
        let bytes_saved = i64::try_from(decompressed.len())
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(compressed.len()).unwrap_or(i64::MAX));

        let inner = p2p::Message::decode(&decompressed[..])?;
        let op = match &inner.message {
            Some(variant) => Op::of(variant)?,
            None => return Err(Error::UnknownOp),
        };
        Ok((inner, bytes_saved, op))
    }

    /// Marshals `m` into a ready-to-send [`OutboundMessage`] (mirrors Go
    /// `createOutbound`).
    ///
    /// # Errors
    /// Propagates [`Self::marshal`] errors.
    pub fn create_outbound(
        &self,
        m: &p2p::Message,
        c: Compression,
        bypass_throttling: bool,
    ) -> Result<OutboundMessage> {
        let (bytes, saved, op) = self.marshal(m, c)?;
        Ok(OutboundMessage {
            bypass_throttling,
            op,
            bytes,
            bytes_saved_compression: saved,
        })
    }

    /// Parses inbound frame bytes into an [`InboundMessage`] (mirrors Go
    /// `parseInbound`), computing the deadline-based expiration.
    ///
    /// # Errors
    /// Propagates [`Self::unmarshal`] errors.
    pub fn parse_inbound(&self, b: &[u8]) -> Result<InboundMessage> {
        let (m, bytes_saved_compression, op) = self.unmarshal(b)?;
        let message = m.message.ok_or(Error::UnknownOp)?;
        let expiration = deadline_of(&message)
            .map(|d| d.min(self.max_message_timeout))
            .and_then(|clamped| Instant::now().checked_add(clamped));
        Ok(InboundMessage {
            sender: NodeId::default(),
            op,
            message,
            expiration,
            bytes_saved_compression,
        })
    }
}

/// Decodes a legacy gzip-compressed payload, bounded to `MAX_MESSAGE_SIZE`.
///
/// The modern p2p wire only ever carries zstd (specs/05 §1.3); this gzip
/// **decode-only** tolerance path exists so a frame produced by a legacy peer
/// can still be read. `ava-message` never *produces* gzip. It is not wired into
/// [`MsgBuilder::unmarshal`] (the live wire has no gzip branch), but is exposed
/// for callers that need to tolerate a legacy frame.
///
/// # Errors
/// Returns [`Error::Compression`] on a malformed stream or a payload that would
/// exceed `MAX_MESSAGE_SIZE`.
pub fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read as _;

    let mut decoder = flate2::read::GzDecoder::new(data);
    // Read at most MAX_MESSAGE_SIZE + 1 so we can detect over-long payloads.
    let max = u64::from(MAX_MESSAGE_SIZE);
    let cap = max.saturating_add(1);
    let mut out = Vec::new();
    decoder
        .by_ref()
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|e| Error::Compression(e.to_string()))?;
    if out.len() as u64 > max {
        return Err(Error::Compression(
            "gzip payload exceeds MAX_MESSAGE_SIZE".to_string(),
        ));
    }
    Ok(out)
}

/// Returns the deadline (as a `Duration`) carried by ops that have one. The
/// proto encodes deadlines as nanoseconds (`u64`), matching Go (specs/05 §2.4).
fn deadline_of(m: &p2p::message::Message) -> Option<Duration> {
    use p2p::message::Message as M;
    let ns = match m {
        M::GetStateSummaryFrontier(x) => x.deadline,
        M::GetAcceptedStateSummary(x) => x.deadline,
        M::GetAcceptedFrontier(x) => x.deadline,
        M::GetAccepted(x) => x.deadline,
        M::GetAncestors(x) => x.deadline,
        M::Get(x) => x.deadline,
        M::PushQuery(x) => x.deadline,
        M::PullQuery(x) => x.deadline,
        M::AppRequest(x) => x.deadline,
        _ => return None,
    };
    Some(Duration::from_nanos(ns))
}

/// A shared, clonable handle to a [`MsgBuilder`] (the `Creator` holds one).
pub type SharedMsgBuilder = Arc<MsgBuilder>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marshal_unmarshal_get_preserves_deadline() {
        let mb = MsgBuilder::default();
        let m = p2p::Message {
            message: Some(p2p::message::Message::Get(p2p::Get {
                chain_id: Bytes::from(vec![1u8; 32]),
                request_id: 3,
                deadline: 1_000_000_000,
                container_id: Bytes::from(vec![2u8; 32]),
            })),
        };
        let (bytes, _saved, op) = mb.marshal(&m, Compression::None).unwrap();
        assert_eq!(op, Op::Get);
        let parsed = mb.parse_inbound(&bytes).unwrap();
        assert_eq!(parsed.op, Op::Get);
        // Get carries a deadline, so expiration is Some.
        assert!(parsed.expiration.is_some());
    }

    #[test]
    fn deadline_units_are_nanoseconds() {
        // 1s deadline -> Duration::from_nanos(1e9) == 1s.
        let m = p2p::message::Message::Get(p2p::Get {
            deadline: 1_000_000_000,
            ..Default::default()
        });
        assert_eq!(deadline_of(&m), Some(Duration::from_secs(1)));
    }

    #[test]
    fn vector_json_shape_parses() {
        // Sanity: the golden-vector JSON shape (input_fields + hex_frame) parses.
        let raw = r#"{"input_fields":{"uptime":7},"hex_frame":"000000045a020807"}"#;
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            v.get("hex_frame").and_then(|x| x.as_str()),
            Some("000000045a020807")
        );
        // serde derive smoke (uses the `serde` dev-dep).
        #[derive(serde::Deserialize)]
        struct Tiny {
            hex_frame: String,
        }
        let t: Tiny = serde_json::from_str(raw).unwrap();
        assert_eq!(t.hex_frame, "000000045a020807");
    }
}

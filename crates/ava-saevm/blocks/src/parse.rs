// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! SAE block parsing (specs/11 §4.1, Go `sae/blocks.go::ParseBlock`).
//!
//! A SAE block is a standard Ethereum block, so parsing is RLP-decoding the eth
//! block and sealing it (computing `keccak256(RLP(header))`). Parsing performs
//! the *stateless* checks: the height fits `u64` (inherent — the header `number`
//! is a `u64`) and the block is not too far in the future
//! (`now + MAX_FUTURE_BLOCK`). Ancestry is **not** populated here — it is set on
//! a successful `VerifyBlock` (M7.18).

use std::time::{SystemTime, UNIX_EPOCH};

use ava_evm_reth::{RethBlock, RlpDecodable, SealedBlock};
use ava_saevm_params::MAX_FUTURE_BLOCK;

use crate::Block;

/// Failure decoding or validating an encoded SAE block.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The bytes did not RLP-decode to a well-formed Ethereum block.
    #[error("decoding block RLP: {0}")]
    Rlp(String),
    /// The block's timestamp is beyond `now + MAX_FUTURE_BLOCK` (specs/11 §4.1,
    /// Go `maxFutureBlockSeconds`).
    #[error("block timestamp {timestamp}s exceeds now + MAX_FUTURE_BLOCK ({limit}s)")]
    FutureBlock {
        /// The block header's timestamp (Unix seconds).
        timestamp: u64,
        /// The accepted ceiling (`now + MAX_FUTURE_BLOCK`, Unix seconds).
        limit: u64,
    },
    /// Constructing the [`Block`] from the sealed eth block failed (header
    /// invariants); see [`crate::Error`].
    #[error(transparent)]
    Lifecycle(#[from] crate::Error),
}

/// Parses an RLP-encoded SAE (Ethereum) block, sealing it and rejecting blocks
/// too far in the future. Ancestry is left unset (populated on `VerifyBlock`).
///
/// `now` is the current wall-clock instant used for the future-block bound
/// (injected for determinism/testability rather than read from the system
/// clock — specs/24).
///
/// # Errors
/// [`ParseError::Rlp`] on malformed input; [`ParseError::FutureBlock`] when the
/// header timestamp exceeds `now + MAX_FUTURE_BLOCK`; [`ParseError::Lifecycle`]
/// if [`Block::new`] rejects the sealed block.
pub fn parse_block(bytes: &[u8], now: SystemTime) -> Result<Block, ParseError> {
    let mut slice = bytes;
    let eth: RethBlock =
        RethBlock::decode(&mut slice).map_err(|e| ParseError::Rlp(e.to_string()))?;
    let sealed: SealedBlock<RethBlock> = SealedBlock::seal_slow(eth);

    let timestamp = sealed.header().timestamp;
    // limit = now + MAX_FUTURE_BLOCK, as Unix seconds (saturating).
    let limit = now
        .checked_add(MAX_FUTURE_BLOCK)
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(u64::MAX, |d| d.as_secs());
    if timestamp > limit {
        return Err(ParseError::FutureBlock { timestamp, limit });
    }

    // Ancestry is NOT set at parse time (specs/11 §4.1) — populated on verify.
    Ok(Block::new(sealed, None, None)?)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ava_evm_reth::{EMPTY_OMMER_ROOT_HASH, EMPTY_ROOT_HASH, Header, rlp_encode};

    use super::*;

    fn encoded_block(number: u64, timestamp: u64) -> Vec<u8> {
        let header = Header {
            number,
            timestamp,
            transactions_root: EMPTY_ROOT_HASH,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            ..Header::default()
        };
        rlp_encode(RethBlock::uncle(header))
    }

    #[test]
    fn parse_round_trips_a_sealed_block() {
        let bytes = encoded_block(5, 100);
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let block = parse_block(&bytes, now).expect("parse");
        assert_eq!(block.height(), 5);
        assert!(block.parent_block().is_none(), "ancestry unset at parse");
    }

    #[test]
    fn parse_rejects_future_block() {
        // Block at t = 1000, but now = 10 (so now + MAX_FUTURE_BLOCK << 1000).
        let bytes = encoded_block(1, 1_000);
        let now = UNIX_EPOCH + Duration::from_secs(10);
        // Map away the non-Debug `Block` in the Ok arm so we can assert on the error.
        let outcome = parse_block(&bytes, now).map(|_| ());
        assert!(
            matches!(outcome, Err(ParseError::FutureBlock { .. })),
            "expected FutureBlock, got {outcome:?}"
        );
    }
}

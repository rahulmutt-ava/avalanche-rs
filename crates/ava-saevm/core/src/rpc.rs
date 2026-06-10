// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! SAE RPC frontier label mapping (specs/11 §1.1, §3).
//!
//! Maps eth-JSON-RPC block-number tags to the three SAE frontiers.
//! Port of `vms/saevm/blocks/access.go::ResolveRPCNumber`.
//!
//! ## Label → Frontier table (specs/11 §1.1)
//!
//! | RPC label | SAE frontier |
//! |---|---|
//! | `pending` | `LastAccepted` (A) — SAE has no pending block distinct from A |
//! | `latest` | `LastExecuted` (E) — the EVM head with known post-state |
//! | `safe`, `finalized` | `LastSettled` (S) — demonstrably agreed |
//! | `earliest` | genesis (height 0) |
//! | `Number(n)` | height `n`, must be ≤ `A.height` and canonical |
//!
//! ## HTTP handler seam
//!
//! The actual HTTP handler mounting (wiring `block`/`receipt`/`gasPrice`
//! endpoints onto `create_handlers`) is deferred to the cchain harness.
//!
//! TODO(M7.23): wire `create_handlers` to mount the eth-RPC surface via the
//! resolver here once the cchain harness supplies the `Initialize` context.

use std::sync::Arc;

use ava_saevm_blocks::Block;

use crate::frontier::Frontier;

// ---------------------------------------------------------------------------
// Label enum
// ---------------------------------------------------------------------------

/// An eth-JSON-RPC block-number specifier.
///
/// Richer than `ava_saevm_gasprice::BlockNumberRef` (which omits `Safe` /
/// `Finalized`); defined here to avoid adding those variants to the gasprice
/// crate. The two types intentionally mirror each other for the shared subset
/// (`Earliest`, `Latest`, `Pending`, `Number`).
///
/// Go reference: `go-ethereum/rpc.BlockNumber` + `rpc.BlockNumberOrHash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcBlockLabel {
    /// `earliest` — genesis (height 0).
    Earliest,
    /// `latest` — `LastExecuted` (E): the EVM head with known post-state.
    Latest,
    /// `pending` — `LastAccepted` (A): SAE has no pending block distinct from A.
    Pending,
    /// `safe` — `LastSettled` (S): demonstrably agreed.
    Safe,
    /// `finalized` — `LastSettled` (S): same as `safe` in the SAE model.
    Finalized,
    /// An absolute block height.
    Number(u64),
}

// ---------------------------------------------------------------------------
// Sentinel errors
// ---------------------------------------------------------------------------

/// Errors returned by [`resolve_rpc_number`].
///
/// Port of Go `ErrFutureBlockNotResolved` / `ErrNonCanonicalBlock` from
/// `vms/saevm/blocks/access.go`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// The requested height is beyond `LastAccepted` (A) — execution has not
    /// yet resolved that block (Go `ErrFutureBlockNotResolved`).
    #[error("block not yet resolved: height exceeds LastAccepted (A)")]
    FutureBlockNotResolved,
    /// The requested height is ≤ A but is absent from the canonical height
    /// index — the block is on a rejected fork (Go `ErrNonCanonicalBlock`).
    #[error("non-canonical block: height is not in the canonical chain index")]
    NonCanonicalBlock,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Maps an eth-JSON-RPC [`RpcBlockLabel`] to the canonical accepted height.
///
/// `canonical` is a closure `|height: u64| -> Option<Arc<Block>>` that returns
/// the accepted block at that height (or `None` if the height is not indexed).
/// Callers typically back this with the VM's `height_index`.
///
/// # Errors
///
/// - [`RpcError::FutureBlockNotResolved`] — `Number(n)` with `n > A.height`.
/// - [`RpcError::NonCanonicalBlock`] — `Number(n)` with `n ≤ A.height` but
///   absent from the canonical index.
///
/// # Frontier choices
///
/// | Label | Resolved to |
/// |---|---|
/// | `Pending` | `LastAccepted` (A) |
/// | `Latest` | `LastExecuted` (E), falling back to `LastSettled` (S) if E is
///   not yet populated (i.e. execution is still catching up from genesis) |
/// | `Safe` / `Finalized` | `LastSettled` (S) |
/// | `Earliest` | height 0 |
/// | `Number(n)` | `n` after bounds + canonical checks |
///
/// Go reference: `vms/saevm/blocks/access.go::ResolveRPCNumber`.
pub fn resolve_rpc_number<F>(
    label: RpcBlockLabel,
    frontier: &Frontier,
    canonical: F,
) -> Result<u64, RpcError>
where
    F: Fn(u64) -> Option<Arc<Block>>,
{
    match label {
        RpcBlockLabel::Pending => Ok(frontier.last_accepted().height()),

        RpcBlockLabel::Latest => {
            // E lags A; fall back to S if E is not yet populated.
            Ok(frontier
                .last_executed()
                .as_ref()
                .map_or_else(|| frontier.last_settled().height(), |e| e.height()))
        }

        RpcBlockLabel::Safe | RpcBlockLabel::Finalized => Ok(frontier.last_settled().height()),

        RpcBlockLabel::Earliest => Ok(0),

        RpcBlockLabel::Number(n) => {
            let a_height = frontier.last_accepted().height();
            if n > a_height {
                return Err(RpcError::FutureBlockNotResolved);
            }
            // Check the canonical index.
            if canonical(n).is_none() {
                return Err(RpcError::NonCanonicalBlock);
            }
            Ok(n)
        }
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `block.WithVerifyContext` + `block.Context` (specs 07 §2.3).
//!
//! The optional per-block extension a block implements when its validity depends
//! on the P-Chain height (proposervm-driven). The engine probes for it via a
//! downcast helper on the wrapper block and only calls it when proposervm is
//! activated.

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// `block.Context` — the proposervm-supplied context a block may verify against.
///
/// Modelled as a canoto type in Go (`p_chain_height` is field 1). Carried here
/// as a plain struct; the field id is preserved for `proto`/`canoto` parity when
/// the wire layer lands.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BlockContext {
    /// The P-Chain height the block must be verified against.
    pub p_chain_height: u64,
}

impl BlockContext {
    /// Builds a [`BlockContext`] at the given P-Chain height.
    #[must_use]
    pub fn new(p_chain_height: u64) -> Self {
        Self { p_chain_height }
    }
}

/// `block.WithVerifyContext` — implemented when a block's validity depends on the
/// P-Chain height. Only invoked when proposervm is active.
#[async_trait]
pub trait WithVerifyContext: Send + Sync {
    /// `ShouldVerifyWithContext` — whether this block must be verified against a
    /// [`BlockContext`] rather than via the plain `verify`.
    async fn should_verify_with_context(&self, token: &CancellationToken) -> Result<bool>;

    /// `VerifyWithContext` — verify the block against the supplied
    /// [`BlockContext`].
    async fn verify_with_context(
        &self,
        token: &CancellationToken,
        ctx: &BlockContext,
    ) -> Result<()>;
}

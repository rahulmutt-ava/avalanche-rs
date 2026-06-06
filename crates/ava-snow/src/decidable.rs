// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Block` trait (specs 06 §2.4; Go `snowman/block.go` + `snow/decidable.go`).

use std::time::SystemTime;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::error::Result;

/// A Snowman block: a linearly-ordered, decidable container.
///
/// Go's `Decidable` collapses into this trait's `id`/`accept`/`reject`; the
/// container `Status` is queried from the VM where needed rather than carried on
/// the block object (specs 06 §3.1).
///
/// `verify`/`accept`/`reject` take a [`CancellationToken`] in place of Go's
/// `context.Context` cancellation (specs 06 §9 Go mapping). Implementations must
/// observe the token for prompt shutdown.
#[async_trait]
pub trait Block: Send + Sync {
    /// The unique identifier of this block.
    fn id(&self) -> Id;

    /// The identifier of this block's parent.
    fn parent(&self) -> Id;

    /// The height of this block in the chain (genesis is height 0).
    fn height(&self) -> u64;

    /// The block's timestamp.
    fn timestamp(&self) -> SystemTime;

    /// The canonical serialized bytes of this block.
    fn bytes(&self) -> &[u8];

    /// Verifies the block is valid, to be called before `accept`.
    async fn verify(&self, token: &CancellationToken) -> Result<()>;

    /// Accepts this block, committing it to the chain.
    async fn accept(&self, token: &CancellationToken) -> Result<()>;

    /// Rejects this block, discarding it from the chain.
    async fn reject(&self, token: &CancellationToken) -> Result<()>;
}

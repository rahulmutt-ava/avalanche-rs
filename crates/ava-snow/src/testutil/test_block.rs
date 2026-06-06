// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! A hand-written no-op [`Block`] for the in-memory consensus cluster.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::decidable::Block;
use crate::error::Result;

/// A minimal in-memory block: identity + parent + height + bytes, with no-op
/// `verify`/`accept`/`reject` (acceptance recording is done by the cluster's
/// shared oracle, not the block).
#[derive(Clone, Debug)]
pub struct TestBlock {
    id: Id,
    parent: Id,
    height: u64,
    timestamp: SystemTime,
    bytes: Vec<u8>,
}

impl TestBlock {
    /// Builds a test block with the given identity, parent, and height. The
    /// timestamp is derived deterministically from the height (no wall clock).
    #[must_use]
    pub fn new(id: Id, parent: Id, height: u64) -> Self {
        Self {
            id,
            parent,
            height,
            timestamp: UNIX_EPOCH + Duration::from_secs(height),
            bytes: id.as_bytes().to_vec(),
        }
    }
}

#[async_trait]
impl Block for TestBlock {
    fn id(&self) -> Id {
        self.id
    }

    fn parent(&self) -> Id {
        self.parent
    }

    fn height(&self) -> u64 {
        self.height
    }

    fn timestamp(&self) -> SystemTime {
        self.timestamp
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    async fn verify(&self, _token: &CancellationToken) -> Result<()> {
        Ok(())
    }

    async fn accept(&self, _token: &CancellationToken) -> Result<()> {
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> Result<()> {
        Ok(())
    }
}

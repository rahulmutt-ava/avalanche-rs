// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The host-side block handle ([`RpcBlock`]): an immutable snapshot of a remote
//! block's identity/bytes whose `verify`/`accept`/`reject` translate to
//! `proto/vm` `BlockVerify`/`BlockAccept`/`BlockReject` RPCs (specs 07 §5.2).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use ava_snow::{Block, Error as SnowError, Result as SnowResult};
use ava_types::id::Id;

use crate::pb::vm::vm_client::VmClient;
use crate::pb::vm::{BlockAcceptRequest, BlockRejectRequest, BlockVerifyRequest};

/// Maps a wire `google.protobuf.Timestamp` (optional) to a [`SystemTime`].
pub(crate) fn timestamp_to_system_time(ts: Option<prost_types::Timestamp>) -> SystemTime {
    let Some(ts) = ts else {
        return UNIX_EPOCH;
    };
    // seconds may be negative in proto; clamp before the epoch to UNIX_EPOCH.
    if ts.seconds < 0 {
        return UNIX_EPOCH;
    }
    let secs = u64::try_from(ts.seconds).unwrap_or(0);
    let nanos = u32::try_from(ts.nanos).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::new(secs, nanos))
        .unwrap_or(UNIX_EPOCH)
}

/// A remote block fronted by the host VM client.
///
/// `bytes`/`id`/`parent`/`height`/`timestamp` are captured at parse/build time
/// (zero re-marshalling on the host; specs 07 §11); the decide ops round-trip to
/// the guest. A transport failure during a decide op has no dedicated
/// `ava_snow::Error` variant, so it is surfaced as [`SnowError::Multiple`] with
/// an empty cause list (a "critical remote error" — the engine halts the chain,
/// matching Go's treatment of a decide error). See `tests/PORTING.md`.
pub struct RpcBlock {
    id: Id,
    parent: Id,
    height: u64,
    timestamp: SystemTime,
    bytes: Vec<u8>,
    /// Whether the guest cast this block to `WithVerifyContext` (`BlockVerify`
    /// then takes a `p_chain_height`). Captured from the build/parse response.
    verify_with_context: bool,
    client: Mutex<VmClient<Channel>>,
    /// Shared host-side last-accepted cache, advanced to this block's id on a
    /// successful `accept` (the `chain.State` decorator's job in Go).
    last_accepted: Arc<Mutex<Id>>,
}

impl RpcBlock {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: Id,
        parent: Id,
        height: u64,
        timestamp: SystemTime,
        bytes: Vec<u8>,
        verify_with_context: bool,
        client: VmClient<Channel>,
        last_accepted: Arc<Mutex<Id>>,
    ) -> Self {
        Self {
            id,
            parent,
            height,
            timestamp,
            bytes,
            verify_with_context,
            client: Mutex::new(client),
            last_accepted,
        }
    }

    /// Whether this block opts into `verify_with_context`.
    #[must_use]
    pub fn verify_with_context(&self) -> bool {
        self.verify_with_context
    }
}

/// A transport failure during a decide op is a critical error (the chain halts).
fn decide_err() -> SnowError {
    SnowError::Multiple(Vec::new())
}

#[async_trait]
impl Block for RpcBlock {
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

    async fn verify(&self, _token: &CancellationToken) -> SnowResult<()> {
        let mut client = self.client.lock().clone();
        client
            .block_verify(BlockVerifyRequest {
                bytes: bytes::Bytes::copy_from_slice(&self.bytes),
                // proposervm passes the p-chain height for WithVerifyContext
                // blocks; the plain Snowman path leaves it unset.
                p_chain_height: None,
            })
            .await
            .map_err(|_| decide_err())?;
        Ok(())
    }

    async fn accept(&self, _token: &CancellationToken) -> SnowResult<()> {
        let mut client = self.client.lock().clone();
        client
            .block_accept(BlockAcceptRequest {
                id: bytes::Bytes::copy_from_slice(&self.id.to_bytes()),
            })
            .await
            .map_err(|_| decide_err())?;
        // Advance the host-side last-accepted cache (Go: `chain.State` does this).
        *self.last_accepted.lock() = self.id;
        Ok(())
    }

    async fn reject(&self, _token: &CancellationToken) -> SnowResult<()> {
        let mut client = self.client.lock().clone();
        client
            .block_reject(BlockRejectRequest {
                id: bytes::Bytes::copy_from_slice(&self.id.to_bytes()),
            })
            .await
            .map_err(|_| decide_err())?;
        Ok(())
    }
}

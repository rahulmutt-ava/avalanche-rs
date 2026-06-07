// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The handshake `Runtime` service (`proto/vm/runtime`, specs 07 §5.1).
//!
//! The **host** binds an ephemeral listener `R`, serves this `Runtime` service
//! on it, and hands `R`'s address to the spawned plugin via the
//! [`ENGINE_ADDRESS_KEY`](crate::ENGINE_ADDRESS_KEY) env var. The **guest** dials
//! `R` and calls `Initialize(protocol_version, addr)` reporting its own VM
//! listener address `V`. The host's `Initialize` handler validates the protocol
//! version (must equal [`RPC_CHAIN_VM_PROTOCOL`](crate::RPC_CHAIN_VM_PROTOCOL))
//! and records `V`, unblocking the host so it can dial `V`.

use parking_lot::Mutex;
use tonic::{Request, Response, Status};

use crate::RPC_CHAIN_VM_PROTOCOL;
use crate::pb::vm::runtime::InitializeRequest;
use crate::pb::vm::runtime::runtime_server::{Runtime as RuntimeService, RuntimeServer};

/// The outcome of a single handshake, delivered from the `Runtime.Initialize`
/// handler to the host that is awaiting it.
#[derive(Debug)]
pub(crate) enum Handshake {
    /// The plugin reported a compatible protocol version and its VM address.
    Ok {
        /// The plugin's VM listener address (`V`).
        vm_addr: String,
    },
    /// The plugin reported an incompatible protocol version.
    VersionMismatch {
        /// The version the plugin reported (kept for the host's diagnostic log;
        /// the surfaced error is the version-agnostic
        /// [`ProtocolVersionMismatch`](ava_vm::Error::ProtocolVersionMismatch)).
        #[allow(dead_code)]
        got: u32,
    },
}

/// The host-side `Runtime` service. On `Initialize` it validates the protocol
/// version and forwards the [`Handshake`] to the waiting host via a oneshot
/// channel (taken once; subsequent calls are ignored as the host has moved on).
pub(crate) struct RuntimeServiceImpl {
    tx: Mutex<Option<tokio::sync::oneshot::Sender<Handshake>>>,
}

impl RuntimeServiceImpl {
    /// Builds the service paired with the oneshot receiver the host awaits.
    pub(crate) fn new() -> (Self, tokio::sync::oneshot::Receiver<Handshake>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Self {
                tx: Mutex::new(Some(tx)),
            },
            rx,
        )
    }

    /// Wraps `self` as a tower service for `tonic::transport::Server`.
    pub(crate) fn into_service(self) -> RuntimeServer<Self> {
        RuntimeServer::new(self)
    }
}

#[tonic::async_trait]
impl RuntimeService for RuntimeServiceImpl {
    async fn initialize(
        &self,
        request: Request<InitializeRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let handshake = if req.protocol_version == RPC_CHAIN_VM_PROTOCOL {
            Handshake::Ok {
                vm_addr: req.addr,
            }
        } else {
            Handshake::VersionMismatch {
                got: req.protocol_version,
            }
        };

        // Deliver to the waiting host (first call wins).
        if let Some(tx) = self.tx.lock().take() {
            let _ = tx.send(handshake);
        }
        Ok(Response::new(()))
    }
}

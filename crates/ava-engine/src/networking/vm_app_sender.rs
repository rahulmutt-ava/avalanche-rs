// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`VmAppSender`] bridges `ava-vm`'s [`AppSender`] (the VM-facing app-message
//! handle, specs 07 §2.6) onto the engine-facing [`Sender`]'s app surface
//! (`common/sender.rs:112-127`), so a booted chain's VM can request/respond/
//! error/gossip over the very same `Sender` the consensus engine drives — the
//! production [`OutboundSender`](crate::networking::sender::OutboundSender)
//! for a networked node, or an in-process loopback/recording `Sender` for a
//! solo boot — instead of a no-op stand-in.
//!
//! The two traits' four app methods line up 1:1 (same node/request-id/bytes
//! shapes); this bridge absorbs the two differences between them:
//!
//! * `AppSender` takes a leading `&CancellationToken` per call (the Rust
//!   analogue of Go's `context.Context`); the engine [`Sender`] has no such
//!   parameter, so it is accepted here and **intentionally unused** —
//!   cancelling an in-flight send is not a capability the engine `Sender`
//!   exposes.
//! * The two crates each define their own `SendConfig` (mirroring Go's single
//!   `common.SendConfig`, kept as two identical-shaped types so `ava-vm` stays
//!   free of a networking dependency); [`to_engine_send_config`] maps one onto
//!   the other field-for-field.
//!
//! Errors: `ava-vm` cannot depend on `ava-engine` (the dependency runs the
//! other way — `ava-engine` depends on `ava-vm`), so an engine [`Sender`]
//! failure is mapped into [`ava_vm::error::Error::AppSendFailed`] by its
//! `Display` message.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;
use ava_vm::app_sender::{AppSender, SendConfig as VmSendConfig};
use ava_vm::error::{Error as VmError, Result as VmResult};

use crate::common::sender::{SendConfig, Sender};

/// Bridges a generic engine [`Sender`] onto `ava-vm`'s [`AppSender`] via
/// direct delegation of the four app methods.
pub struct VmAppSender<S: Sender> {
    /// The engine `Sender` every call delegates to.
    sender: Arc<S>,
}

impl<S: Sender> VmAppSender<S> {
    /// Wraps `sender` as an `ava-vm`-facing [`AppSender`].
    #[must_use]
    pub fn new(sender: Arc<S>) -> Self {
        Self { sender }
    }
}

/// Maps an engine [`Sender`] failure into `ava-vm`'s error type, preserving
/// the failure's message (see the module docs on the crate-dependency
/// direction that rules out a `From` impl here).
fn to_vm_result(r: crate::error::Result<()>) -> VmResult<()> {
    r.map_err(|e| VmError::AppSendFailed(e.to_string()))
}

/// Maps the `ava-vm`-facing [`VmSendConfig`] onto the engine's [`SendConfig`]
/// (the two mirror Go's single `common.SendConfig` field-for-field).
fn to_engine_send_config(c: VmSendConfig) -> SendConfig {
    SendConfig {
        node_ids: c.node_ids,
        validators: c.validators,
        non_validators: c.non_validators,
        peers: c.peers,
    }
}

#[async_trait]
impl<S: Sender> AppSender for VmAppSender<S> {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        nodes: &HashSet<NodeId>,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> VmResult<()> {
        to_vm_result(self.sender.send_app_request(nodes, request_id, bytes).await)
    }

    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        bytes: Vec<u8>,
    ) -> VmResult<()> {
        to_vm_result(self.sender.send_app_response(node, request_id, bytes).await)
    }

    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        code: i32,
        message: &str,
    ) -> VmResult<()> {
        to_vm_result(
            self.sender
                .send_app_error(node, request_id, code, message)
                .await,
        )
    }

    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        config: VmSendConfig,
        bytes: Vec<u8>,
    ) -> VmResult<()> {
        to_vm_result(
            self.sender
                .send_app_gossip(to_engine_send_config(config), bytes)
                .await,
        )
    }
}

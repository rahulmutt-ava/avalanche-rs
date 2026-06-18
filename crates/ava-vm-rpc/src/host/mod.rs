// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The host (node, VM **client**) side: [`RpcChainVm`] implements the full
//! [`ChainVm`] trait by translating each method to a `proto/vm` RPC over the
//! dialed channel (`vms/rpcchainvm/vm_client.go`, specs 07 §5.2).
//!
//! [`RpcChainVm::start`] performs the **v45 reverse-dial handshake** (specs 07
//! §5.1): bind an ephemeral runtime listener `R`, serve the [`Runtime`](crate::runtime)
//! service on it, run the caller's `launcher` (which spawns the plugin with
//! [`ENGINE_ADDRESS_KEY`](crate::ENGINE_ADDRESS_KEY) = `R.addr`), await the
//! plugin's `Runtime.Initialize(version, V.addr)` within
//! [`DEFAULT_HANDSHAKE_TIMEOUT`](crate::DEFAULT_HANDSHAKE_TIMEOUT), assert the
//! version, then dial `V` and build the VM client.

pub mod block;
pub mod subprocess;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Endpoint};

use ava_database::DynDatabase;
use ava_snow::{ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::application::Application;

use crate::MAX_MESSAGE_SIZE;
use crate::pb::vm::vm_client::VmClient;
use crate::pb::vm::{
    self, AppGossipMsg, AppRequestFailedMsg, AppRequestMsg, AppResponseMsg, BuildBlockRequest,
    ConnectedRequest, DisconnectedRequest, GetBlockIdAtHeightRequest, GetBlockRequest,
    InitializeRequest, ParseBlockRequest, SetPreferenceRequest, SetStateRequest,
};
use crate::runtime::{Handshake, RuntimeServiceImpl};
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{Block, ChainVm};
use ava_vm::connector::Connector;
use ava_vm::error::{Error, Result};
use ava_vm::fx::Fx;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{HttpHandler, Vm, VmEvent};

use self::block::{RpcBlock, timestamp_to_system_time};

/// Maps a tonic transport/dial error to [`Error::HandshakeFailed`] (only used on
/// the dial/handshake path; per-method RPC failures map to richer errors).
fn dial_err<E: std::fmt::Display>(_e: E) -> Error {
    Error::HandshakeFailed
}

/// Maps a per-method gRPC failure to a crate [`Error`]. Transport faults become
/// [`Error::HandshakeFailed`] (the channel is dead); a `NOT_FOUND` status is the
/// `database.ErrNotFound` sentinel.
fn rpc_err(status: tonic::Status) -> Error {
    if status.code() == tonic::Code::NotFound {
        Error::NotFound
    } else {
        Error::HandshakeFailed
    }
}

/// Maps the wire [`vm::Error`] enum to the crate sentinel model.
fn err_enum_to_result(err: i32) -> Result<()> {
    match vm::Error::try_from(err) {
        Ok(vm::Error::NotFound) => Err(Error::NotFound),
        Ok(vm::Error::StateSyncNotImplemented) => Err(Error::StateSyncableVmNotImplemented),
        Ok(vm::Error::Closed) => Err(Error::HandshakeFailed),
        Ok(vm::Error::Unspecified) | Err(_) => Ok(()),
    }
}

/// Maps an [`EngineState`] to the wire [`vm::State`] enum.
fn engine_state_to_proto(state: EngineState) -> i32 {
    let s = match state {
        EngineState::StateSyncing => vm::State::StateSyncing,
        EngineState::Bootstrapping => vm::State::Bootstrapping,
        EngineState::NormalOp => vm::State::NormalOp,
        // `Initializing` has no wire value; the VM is told its first *operational*
        // phase. Map to the unspecified sentinel (the guest treats it as a no-op).
        EngineState::Initializing => vm::State::Unspecified,
    };
    s as i32
}

/// Builds a 32-byte [`Id`] from a wire byte slice, defaulting to `EMPTY` on a
/// wrong length (a malformed peer; mirrors Go's `ids.ToID` error handling at the
/// call sites, which log and treat as empty/unknown).
fn id_from_bytes(b: &[u8]) -> Id {
    Id::from_slice(b).unwrap_or(Id::EMPTY)
}

/// Maps an [`Id`] to its wire bytes.
fn id_bytes(id: Id) -> bytes::Bytes {
    bytes::Bytes::copy_from_slice(&id.to_bytes())
}

/// Encodes the [`ChainContext`] identity + the genesis/upgrade/config bytes +
/// the two callback addresses into an [`InitializeRequest`] (07 §5.2).
///
/// `network_upgrades` carries the full fork-activation schedule as the proto
/// [`NetworkUpgrades`](vm::NetworkUpgrades) message (Go
/// `vm_client.go:getNetworkUpgrades`). A Go guest's `convertNetworkUpgrades`
/// rejects a nil message (`errNilNetworkUpgradesPB`), so this field MUST be sent
/// — the guest no longer reconstructs the schedule from `network_id`. See
/// [`crate::upgrades`].
fn chain_context_to_request(
    chain_ctx: &ChainContext,
    genesis_bytes: &[u8],
    upgrade_bytes: &[u8],
    config_bytes: &[u8],
    db_server_addr: String,
    server_addr: String,
) -> InitializeRequest {
    // The wire encoding is the 48-byte COMPRESSED form, matching Go's
    // `bls.PublicKeyToCompressedBytes` (vms/rpcchainvm/vm_client.go) — NOT the
    // 96-byte uncompressed `serialize()`. This was a real bug for the
    // Rust-host→Go-guest direction (M9.12): Go's `PublicKeyFromCompressedBytes`
    // strictly expects 48 bytes and rejects the uncompressed form. It went
    // unnoticed Rust↔Rust only because `blst`'s `key_validate` auto-sniffs both
    // encodings on decode (see the guest's `request_to_chain_context`).
    let public_key = chain_ctx
        .public_key
        .as_ref()
        .map(|pk| bytes::Bytes::copy_from_slice(&pk.compress()))
        .unwrap_or_default();
    InitializeRequest {
        network_id: chain_ctx.network_id,
        subnet_id: id_bytes(chain_ctx.subnet_id),
        chain_id: id_bytes(chain_ctx.chain_id),
        node_id: bytes::Bytes::copy_from_slice(chain_ctx.node_id.as_bytes()),
        public_key,
        x_chain_id: id_bytes(chain_ctx.x_chain_id),
        c_chain_id: id_bytes(chain_ctx.c_chain_id),
        avax_asset_id: id_bytes(chain_ctx.avax_asset_id),
        chain_data_dir: chain_ctx.chain_data_dir.to_string_lossy().into_owned(),
        genesis_bytes: bytes::Bytes::copy_from_slice(genesis_bytes),
        upgrade_bytes: bytes::Bytes::copy_from_slice(upgrade_bytes),
        config_bytes: bytes::Bytes::copy_from_slice(config_bytes),
        db_server_addr,
        server_addr,
        network_upgrades: Some(crate::upgrades::upgrades_to_proto(
            &chain_ctx.network_upgrades,
        )),
    }
}

/// The host-side rpcchainvm client: a [`ChainVm`] backed by a dialed VM channel.
///
/// The last-accepted id is tracked **client-side** (a faithful port of Go, where
/// `VMClient` wraps the block state in a `chain.State` decorator that holds
/// `LastAcceptedBlock`): it is seeded from the `Initialize`/`SetState` response
/// and advanced whenever an [`RpcBlock`] is accepted. The id is shared with each
/// [`RpcBlock`] (via `Arc<Mutex<Id>>`) so `accept` can update it.
pub struct RpcChainVm {
    client: VmClient<Channel>,
    last_accepted: Arc<Mutex<Id>>,
    /// Cancels the spawned runtime-server task on drop.
    runtime_shutdown: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Cancels the callback-bundle servers (rpcdb at `db_server_addr` +
    /// appsender at `server_addr`) stood up at [`initialize`](Vm::initialize).
    /// Fired on `shutdown` and on drop so the gRPC servers stop with the VM.
    callback_shutdown: CancellationToken,
}

impl RpcChainVm {
    /// Performs the v45 reverse-dial handshake and returns a connected host VM.
    ///
    /// `launcher` is invoked with the runtime server's address (`R.addr`) once
    /// the server is listening; it must spawn (or otherwise start) the plugin so
    /// that the plugin dials back and calls `Runtime.Initialize`.
    ///
    /// # Errors
    /// * [`Error::HandshakeFailed`] — the handshake did not complete within
    ///   `timeout`, or a transport error occurred binding/serving/dialing.
    /// * [`Error::ProtocolVersionMismatch`] — the plugin reported an
    ///   incompatible protocol version.
    pub async fn start<F>(
        _token: &CancellationToken,
        timeout: std::time::Duration,
        launcher: F,
    ) -> Result<Self>
    where
        F: FnOnce(&str),
    {
        // 1. Bind the ephemeral runtime listener R.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(dial_err)?;
        let r_addr = listener.local_addr().map_err(dial_err)?;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

        // 2. Serve the Runtime service on R; rx receives the handshake outcome.
        let (svc, rx) = RuntimeServiceImpl::new();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(svc.into_service())
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        // 3. Spawn the plugin via the caller's launcher, handing it R.addr.
        launcher(&r_addr.to_string());

        // 4. Await the handshake within `timeout`.
        let handshake = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(h)) => h,
            // Timed out, or the sender dropped without completing the handshake.
            Ok(Err(_)) | Err(_) => {
                let _ = shutdown_tx.send(());
                return Err(Error::HandshakeFailed);
            }
        };

        let vm_addr = match handshake {
            Handshake::Ok { vm_addr } => vm_addr,
            Handshake::VersionMismatch { .. } => {
                let _ = shutdown_tx.send(());
                return Err(Error::ProtocolVersionMismatch);
            }
        };

        // 5. Dial the plugin's VM listener V and build the client.
        let endpoint = Endpoint::from_shared(format!("http://{vm_addr}"))
            .map_err(dial_err)?
            .connect_timeout(timeout);
        let channel = endpoint.connect().await.map_err(dial_err)?;
        let mut client = VmClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_MESSAGE_SIZE);

        // Seed the client-side last-accepted snapshot. `SetState(Unspecified)` is
        // a benign probe the guest answers with its current last-accepted id
        // without changing phase (the engine drives real phase transitions via
        // `set_state`). This mirrors Go seeding `chain.State` from the
        // Initialize/SetState response.
        let last_accepted = match client
            .set_state(SetStateRequest {
                state: vm::State::Unspecified as i32,
            })
            .await
        {
            Ok(resp) => id_from_bytes(&resp.into_inner().last_accepted_id),
            Err(_) => Id::EMPTY,
        };

        Ok(Self {
            client,
            last_accepted: Arc::new(Mutex::new(last_accepted)),
            runtime_shutdown: Mutex::new(Some(shutdown_tx)),
            callback_shutdown: CancellationToken::new(),
        })
    }

    fn client(&self) -> VmClient<Channel> {
        self.client.clone()
    }

    /// Builds an [`RpcBlock`] from a build/parse/get response shape.
    fn make_block(
        &self,
        id: Id,
        parent: Id,
        height: u64,
        timestamp: Option<prost_types::Timestamp>,
        bytes: bytes::Bytes,
        verify_with_context: bool,
    ) -> Arc<dyn Block> {
        Arc::new(RpcBlock::new(
            id,
            parent,
            height,
            timestamp_to_system_time(timestamp),
            bytes.to_vec(),
            verify_with_context,
            self.client(),
            Arc::clone(&self.last_accepted),
        ))
    }
}

impl Drop for RpcChainVm {
    fn drop(&mut self) {
        if let Some(tx) = self.runtime_shutdown.lock().take() {
            let _ = tx.send(());
        }
        // Stop the rpcdb / appsender callback servers stood up at `initialize`.
        self.callback_shutdown.cancel();
    }
}

/// Binds an ephemeral loopback listener; returns `(addr, incoming)` so the
/// caller can learn the bound address before spawning the server on it. The
/// guest dials `addr` back at `VM.Initialize` (the callback-bundle servers).
async fn bind_callback_listener() -> Result<(String, tokio_stream::wrappers::TcpListenerStream)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(dial_err)?;
    let addr = listener.local_addr().map_err(dial_err)?.to_string();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    Ok((addr, incoming))
}

#[async_trait]
impl AppHandler for RpcChainVm {
    async fn app_request(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> Result<()> {
        // Convert the monotonic deadline to a wall-clock proto Timestamp by
        // anchoring "now" (the host has no shared clock with the guest; the
        // remaining duration is what matters and is preserved).
        let remaining = deadline.saturating_duration_since(Instant::now());
        let ts = prost_types::Timestamp {
            seconds: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
            nanos: i32::try_from(remaining.subsec_nanos()).unwrap_or(0),
        };
        self.client()
            .app_request(AppRequestMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                request_id,
                deadline: Some(ts),
                request: bytes::Bytes::copy_from_slice(request),
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    async fn app_request_failed(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> Result<()> {
        self.client()
            .app_request_failed(AppRequestFailedMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                request_id,
                error_code: err.code,
                error_message: err.message,
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    async fn app_response(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> Result<()> {
        self.client()
            .app_response(AppResponseMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                request_id,
                response: bytes::Bytes::copy_from_slice(response),
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    async fn app_gossip(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> Result<()> {
        self.client()
            .app_gossip(AppGossipMsg {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                msg: bytes::Bytes::copy_from_slice(msg),
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }
}

#[async_trait]
impl HealthCheck for RpcChainVm {
    async fn health_check(&self, _token: &CancellationToken) -> Result<serde_json::Value> {
        let resp = self
            .client()
            .health(())
            .await
            .map_err(rpc_err)?
            .into_inner();
        if resp.details.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_slice(&resp.details)
            .map_err(|_| Error::InvalidComponent("failed to unmarshal health details"))
    }
}

#[async_trait]
impl Connector for RpcChainVm {
    async fn connected(
        &mut self,
        _token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> Result<()> {
        self.client()
            .connected(ConnectedRequest {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
                name: version.name,
                major: version.major,
                minor: version.minor,
                patch: version.patch,
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    async fn disconnected(&mut self, _token: &CancellationToken, node: NodeId) -> Result<()> {
        self.client()
            .disconnected(DisconnectedRequest {
                node_id: bytes::Bytes::copy_from_slice(node.as_bytes()),
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }
}

#[async_trait]
impl Vm for RpcChainVm {
    async fn initialize(
        &mut self,
        _token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        upgrade_bytes: &[u8],
        config_bytes: &[u8],
        _fxs: Vec<Fx>,
        app_sender: Arc<dyn AppSender>,
    ) -> Result<()> {
        // 1. Stand up the `proto/rpcdb` Database server over the host's db at an
        //    ephemeral loopback port (`db_server_addr`). The guest dials it back
        //    and builds the `RpcDatabase` the inner VM consumes (07 §5.2/§5.4).
        let (db_server_addr, db_incoming) = bind_callback_listener().await?;
        let db_service = crate::proxy::rpcdb::serve(db).into_service();
        {
            let token = self.callback_shutdown.clone();
            tokio::spawn(async move {
                let _ = tonic::transport::Server::builder()
                    .add_service(db_service)
                    .serve_with_incoming_shutdown(db_incoming, async move {
                        token.cancelled().await;
                    })
                    .await;
            });
        }

        // 2. Stand up the callback-bundle server (`server_addr`). The full
        //    avalanchego bundle serves sharedmemory + aliasreader + appsender +
        //    validatorstate + warp + grpc.health; the in-process Rust↔Rust path
        //    exercises the appsender service (the others are stood up by the node
        //    assembly with concrete impls — see tests/PORTING.md). Sharing one
        //    ephemeral listener across the bundle matches Go's single
        //    `server_addr` for all callback services.
        let (server_addr, cb_incoming) = bind_callback_listener().await?;
        let app_service = crate::proxy::appsender::serve(app_sender).into_service();
        {
            let token = self.callback_shutdown.clone();
            tokio::spawn(async move {
                let _ = tonic::transport::Server::builder()
                    .add_service(app_service)
                    .serve_with_incoming_shutdown(cb_incoming, async move {
                        token.cancelled().await;
                    })
                    .await;
            });
        }

        // 3. Encode the ChainContext identity + the two callback addrs into the
        //    InitializeRequest and send `VM.Initialize` over the dialed channel.
        let resp = self
            .client()
            .initialize(chain_context_to_request(
                &chain_ctx,
                genesis_bytes,
                upgrade_bytes,
                config_bytes,
                db_server_addr,
                server_addr,
            ))
            .await
            .map_err(rpc_err)?
            .into_inner();

        // 4. Seed the client-side last-accepted snapshot from the response
        //    (mirrors Go seeding `chain.State` from the Initialize response).
        *self.last_accepted.lock() = id_from_bytes(&resp.last_accepted_id);
        Ok(())
    }

    async fn set_state(&mut self, _token: &CancellationToken, state: EngineState) -> Result<()> {
        let resp = self
            .client()
            .set_state(SetStateRequest {
                state: engine_state_to_proto(state),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        // Re-seed the last-accepted cache from the response (Go does the same).
        *self.last_accepted.lock() = id_from_bytes(&resp.last_accepted_id);
        Ok(())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> Result<()> {
        self.client().shutdown(()).await.map_err(rpc_err)?;
        // Stop the rpcdb / appsender callback servers stood up at `initialize`.
        self.callback_shutdown.cancel();
        Ok(())
    }

    async fn version(&self, _token: &CancellationToken) -> Result<String> {
        let resp = self
            .client()
            .version(())
            .await
            .map_err(rpc_err)?
            .into_inner();
        Ok(resp.version)
    }

    async fn create_handlers(
        &mut self,
        _token: &CancellationToken,
    ) -> Result<HashMap<String, HttpHandler>> {
        // HTTP handler proxying (proto/http ghttp) is deferred (no HTTP stack in
        // the workspace yet); return an empty set. See tests/PORTING.md.
        let _ = self.client().create_handlers(()).await.map_err(rpc_err)?;
        Ok(HashMap::new())
    }

    async fn new_http_handler(
        &mut self,
        _token: &CancellationToken,
    ) -> Result<Option<HttpHandler>> {
        let _ = self.client().new_http_handler(()).await.map_err(rpc_err)?;
        Ok(None)
    }

    async fn wait_for_event(&self, _token: &CancellationToken) -> Result<VmEvent> {
        let resp = self
            .client()
            .wait_for_event(())
            .await
            .map_err(rpc_err)?
            .into_inner();
        match vm::Message::try_from(resp.message) {
            Ok(vm::Message::BuildBlock) => Ok(VmEvent::PendingTxs),
            Ok(vm::Message::StateSyncFinished) => Ok(VmEvent::StateSyncDone),
            Ok(vm::Message::Unspecified) | Err(_) => Ok(VmEvent::PendingTxs),
        }
    }
}

#[async_trait]
impl ChainVm for RpcChainVm {
    async fn build_block(&mut self, _token: &CancellationToken) -> Result<Arc<dyn Block>> {
        let resp = self
            .client()
            .build_block(BuildBlockRequest {
                p_chain_height: None,
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        Ok(self.make_block(
            id_from_bytes(&resp.id),
            id_from_bytes(&resp.parent_id),
            resp.height,
            resp.timestamp,
            resp.bytes,
            resp.verify_with_context,
        ))
    }

    async fn get_block(&self, _token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>> {
        let resp = self
            .client()
            .get_block(GetBlockRequest {
                id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        err_enum_to_result(resp.err)?;
        Ok(self.make_block(
            id,
            id_from_bytes(&resp.parent_id),
            resp.height,
            resp.timestamp,
            resp.bytes,
            resp.verify_with_context,
        ))
    }

    async fn parse_block(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn Block>> {
        let resp = self
            .client()
            .parse_block(ParseBlockRequest {
                bytes: bytes::Bytes::copy_from_slice(bytes),
            })
            .await
            .map_err(rpc_err)?
            .into_inner();
        Ok(self.make_block(
            id_from_bytes(&resp.id),
            id_from_bytes(&resp.parent_id),
            resp.height,
            resp.timestamp,
            bytes::Bytes::copy_from_slice(bytes),
            resp.verify_with_context,
        ))
    }

    async fn set_preference(&mut self, _token: &CancellationToken, id: Id) -> Result<()> {
        self.client()
            .set_preference(SetPreferenceRequest {
                id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
            })
            .await
            .map_err(rpc_err)?;
        Ok(())
    }

    async fn last_accepted(&self, _token: &CancellationToken) -> Result<Id> {
        // `LastAccepted` is not a standalone proto/vm RPC; Go tracks it
        // client-side in the `chain.State` decorator (seeded at
        // Initialize/SetState, advanced on block accept). We mirror that with a
        // shared cache.
        Ok(*self.last_accepted.lock())
    }

    async fn get_block_id_at_height(&self, _token: &CancellationToken, height: u64) -> Result<Id> {
        let resp = self
            .client()
            .get_block_id_at_height(GetBlockIdAtHeightRequest { height })
            .await
            .map_err(rpc_err)?
            .into_inner();
        err_enum_to_result(resp.err)?;
        Ok(id_from_bytes(&resp.blk_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava_crypto::bls::{PUBLIC_KEY_LEN, PublicKey, SecretKey};

    fn ctx_with_key(public_key: Option<PublicKey>) -> ChainContext {
        ChainContext {
            network_id: 1,
            subnet_id: Id::EMPTY,
            chain_id: Id::EMPTY,
            node_id: NodeId::default(),
            public_key,
            network_upgrades: ava_version::upgrade::get_config(1),
            x_chain_id: Id::EMPTY,
            c_chain_id: Id::EMPTY,
            avax_asset_id: Id::EMPTY,
            chain_data_dir: std::path::PathBuf::new(),
        }
    }

    // Regression for the M9.3 live-arm finding: the host must put the BLS
    // public key on the wire in the 48-byte COMPRESSED form (Go
    // `bls.PublicKeyToCompressedBytes`, vms/rpcchainvm/vm_client.go), not the
    // 96-byte uncompressed `serialize()`. A Go guest decodes with
    // `PublicKeyFromCompressedBytes`, so an uncompressed field is unreadable.
    #[test]
    fn chain_context_to_request_encodes_compressed_bls_key() {
        let sk = SecretKey::new(&[7u8; 32]).expect("bls secret key");
        let pk = sk.public_key();
        let ctx = ctx_with_key(Some(pk.clone()));
        let req = chain_context_to_request(&ctx, b"g", b"u", b"c", String::new(), String::new());
        assert_eq!(
            req.public_key.len(),
            PUBLIC_KEY_LEN,
            "BLS pubkey on the wire is 48-byte compressed"
        );
        let decoded =
            PublicKey::from_compressed(&req.public_key).expect("wire bytes decode as compressed");
        assert_eq!(
            decoded.compress(),
            pk.compress(),
            "compressed wire bytes round-trip the host BLS key"
        );
    }

    #[test]
    fn chain_context_to_request_empty_key_when_none() {
        let ctx = ctx_with_key(None);
        let req = chain_context_to_request(&ctx, b"", b"", b"", String::new(), String::new());
        assert!(
            req.public_key.is_empty(),
            "absent BLS key maps to an empty wire field"
        );
    }

    // The host must send the fork schedule as a populated `NetworkUpgrades`
    // message: a Go guest's `convertNetworkUpgrades` rejects a nil message
    // (`errNilNetworkUpgradesPB`). The wire bytes must decode back to the source
    // schedule (Go `getNetworkUpgrades`/`convertNetworkUpgrades` round trip).
    #[test]
    fn chain_context_to_request_sends_network_upgrades() {
        let mut ctx = ctx_with_key(None);
        // A height a real config would never carry, to prove the wire value (not
        // a reconstruction from network_id) is what travels.
        ctx.network_upgrades.apricot_phase_4_min_p_chain_height = 987_654;
        let req = chain_context_to_request(&ctx, b"", b"", b"", String::new(), String::new());
        let pb = req
            .network_upgrades
            .as_ref()
            .expect("network_upgrades must be sent, never nil");
        let decoded = crate::upgrades::upgrades_from_proto(pb).expect("wire upgrades decode back");
        assert_eq!(
            decoded, ctx.network_upgrades,
            "the wire NetworkUpgrades round-trips the host schedule"
        );
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The guest (plugin, VM **server**) side: [`VmServer`] is a tonic `proto/vm`
//! `VM` service delegating to a local [`ChainVm`], and [`serve`] is the plugin
//! `main()` entrypoint (`vms/rpcchainvm/vm_server.go` + `Serve`, specs 07 §5.3).
//!
//! The guest does the guest half of the v45 reverse-dial handshake (specs 07
//! §5.1): bind an ephemeral VM listener `V`, dial the host's runtime address `R`
//! (read from [`ENGINE_ADDRESS_KEY`](crate::ENGINE_ADDRESS_KEY)), call
//! `Runtime.Initialize(RPC_CHAIN_VM_PROTOCOL, V.addr)`, then serve the `VM`
//! service on `V`.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};

use ava_types::id::Id;

use crate::pb::vm::vm_server::{Vm as VmService, VmServer as PbVmServer};
use crate::pb::vm::{
    self, BlockAcceptRequest, BlockRejectRequest, BlockVerifyRequest, BlockVerifyResponse,
    BuildBlockRequest, BuildBlockResponse, CreateHandlersResponse, GetBlockIdAtHeightRequest,
    GetBlockIdAtHeightResponse, GetBlockRequest, GetBlockResponse, HealthResponse,
    NewHttpHandlerResponse, ParseBlockRequest, ParseBlockResponse, SetPreferenceRequest,
    SetStateRequest, SetStateResponse, VersionResponse, WaitForEventResponse,
};
use crate::{ENGINE_ADDRESS_KEY, MAX_MESSAGE_SIZE, RPC_CHAIN_VM_PROTOCOL};
use ava_vm::block::{Block, ChainVm};
use ava_vm::error::Error as VmError;
use ava_vm::vm::VmEvent;

/// Maps a crate [`VmError`] to the wire [`vm::Error`] enum value (mirrors Go's
/// `errorToErrEnum`); `NotFound` ⇒ `ERROR_NOT_FOUND`, etc.
fn error_to_enum(err: &VmError) -> i32 {
    let e = match err {
        VmError::NotFound => vm::Error::NotFound,
        VmError::StateSyncableVmNotImplemented => vm::Error::StateSyncNotImplemented,
        _ => vm::Error::Unspecified,
    };
    e as i32
}

/// Builds a wire `google.protobuf.Timestamp` from a [`std::time::SystemTime`].
fn system_time_to_proto(t: std::time::SystemTime) -> Option<prost_types::Timestamp> {
    let dur = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    Some(prost_types::Timestamp {
        seconds: i64::try_from(dur.as_secs()).unwrap_or(0),
        nanos: i32::try_from(dur.subsec_nanos()).unwrap_or(0),
    })
}

/// A tonic `VM` service delegating to a local [`ChainVm`].
///
/// The VM is held behind a `tokio::sync::Mutex` because several `VM` RPCs are
/// `&mut self` on the trait (`build_block`, `set_preference`, `set_state`); the
/// guest serializes them, matching Go where the rpcchainvm server holds the
/// chain lock per call. Built/parsed blocks are cached by id so
/// `BlockVerify`/`BlockAccept`/`BlockReject` (addressed by id/bytes) can resolve
/// the live [`Block`] handle.
pub struct VmServer<V: ChainVm> {
    vm: Arc<tokio::sync::Mutex<V>>,
    blocks: Arc<Mutex<HashMap<Id, Arc<dyn Block>>>>,
    token: CancellationToken,
}

impl<V: ChainVm + 'static> VmServer<V> {
    /// Wraps `vm` as a `VM` service. `token` cancels in-flight VM calls on
    /// shutdown.
    #[must_use]
    pub fn new(vm: V, token: CancellationToken) -> Self {
        Self {
            vm: Arc::new(tokio::sync::Mutex::new(vm)),
            blocks: Arc::new(Mutex::new(HashMap::new())),
            token,
        }
    }

    /// Consumes `self` into a tower service ready for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbVmServer<Self> {
        PbVmServer::new(self)
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_MESSAGE_SIZE)
    }

    fn remember(&self, blk: &Arc<dyn Block>) {
        self.blocks.lock().insert(blk.id(), Arc::clone(blk));
    }

    fn lookup(&self, id: Id) -> Option<Arc<dyn Block>> {
        self.blocks.lock().get(&id).map(Arc::clone)
    }
}

#[tonic::async_trait]
impl<V: ChainVm + 'static> VmService for VmServer<V> {
    async fn initialize(
        &self,
        request: Request<vm::InitializeRequest>,
    ) -> Result<Response<vm::InitializeResponse>, Status> {
        let req = request.into_inner();

        // 1. Dial the `proto/rpcdb` Database server (`db_server_addr`) and build
        //    the guest-side `RpcDatabase` the inner VM consumes (07 §5.2/§5.4).
        //    `rpcdb::dial` is synchronous (it owns a current-thread runtime and
        //    `block_on`s each RPC), so it must be driven off the async runtime
        //    context — dial it on a blocking thread (04 §1.2).
        let db_addr = req.db_server_addr.clone();
        let db_client = tokio::task::spawn_blocking(move || crate::proxy::rpcdb::dial(&db_addr))
            .await
            .map_err(|e| Status::internal(format!("db dial task: {e}")))?
            .map_err(|e| Status::internal(format!("dial db_server_addr: {e}")))?;
        let db: Arc<dyn ava_database::DynDatabase> = Arc::new(db_client);

        // 2. Dial the callback-bundle server (`server_addr`) for the appsender.
        //    The full bundle also serves sharedmemory/aliasreader/validatorstate/
        //    warp; the inner VM here consumes only the db + appsender (the other
        //    proxies are constructed by the node-assembly path — see
        //    tests/PORTING.md).
        let app_sender: Arc<dyn ava_vm::app_sender::AppSender> = Arc::new(
            crate::proxy::appsender::dial(&req.server_addr)
                .await
                .map_err(|e| Status::internal(format!("dial server_addr: {e}")))?,
        );

        // 3. Map the InitializeRequest identity fields onto a ChainContext.
        let chain_ctx = request_to_chain_context(&req)
            .map_err(|e| Status::invalid_argument(format!("InitializeRequest: {e}")))?;

        // 4. Run the inner VM's `initialize` with the proxied handles.
        {
            let mut vm = self.vm.lock().await;
            vm.initialize(
                &self.token,
                chain_ctx,
                db,
                &req.genesis_bytes,
                &req.upgrade_bytes,
                &req.config_bytes,
                Vec::new(),
                app_sender,
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        }

        // 5. Report the post-initialize last-accepted snapshot so the host can
        //    seed its client-side `chain.State` cache.
        let vm = self.vm.lock().await;
        let last = vm
            .last_accepted(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let blk = vm
            .get_block(&self.token, last)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(vm::InitializeResponse {
            last_accepted_id: bytes::Bytes::copy_from_slice(&last.to_bytes()),
            last_accepted_parent_id: bytes::Bytes::copy_from_slice(&blk.parent().to_bytes()),
            height: blk.height(),
            bytes: bytes::Bytes::copy_from_slice(blk.bytes()),
            timestamp: system_time_to_proto(blk.timestamp()),
        }))
    }

    async fn set_state(
        &self,
        request: Request<SetStateRequest>,
    ) -> Result<Response<SetStateResponse>, Status> {
        let state = request.into_inner().state;
        let mut vm = self.vm.lock().await;
        // Only a recognized operational phase advances the VM; UNSPECIFIED is a
        // benign last-accepted probe (host seeding) that does not change phase.
        if let Some(engine_state) = proto_to_engine_state(state) {
            vm.set_state(&self.token, engine_state)
                .await
                .map_err(|e| Status::internal(e.to_string()))?;
        }
        let last = vm
            .last_accepted(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let blk = vm
            .get_block(&self.token, last)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(SetStateResponse {
            last_accepted_id: bytes::Bytes::copy_from_slice(&last.to_bytes()),
            last_accepted_parent_id: bytes::Bytes::copy_from_slice(&blk.parent().to_bytes()),
            height: blk.height(),
            bytes: bytes::Bytes::copy_from_slice(blk.bytes()),
            timestamp: system_time_to_proto(blk.timestamp()),
        }))
    }

    async fn shutdown(&self, _request: Request<()>) -> Result<Response<()>, Status> {
        let mut vm = self.vm.lock().await;
        vm.shutdown(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn create_handlers(
        &self,
        _request: Request<()>,
    ) -> Result<Response<CreateHandlersResponse>, Status> {
        // HTTP handler proxying (ghttp) is deferred; report none.
        Ok(Response::new(CreateHandlersResponse {
            handlers: Vec::new(),
        }))
    }

    async fn new_http_handler(
        &self,
        _request: Request<()>,
    ) -> Result<Response<NewHttpHandlerResponse>, Status> {
        Ok(Response::new(NewHttpHandlerResponse {
            server_addr: String::new(),
        }))
    }

    async fn wait_for_event(
        &self,
        _request: Request<()>,
    ) -> Result<Response<WaitForEventResponse>, Status> {
        let vm = self.vm.lock().await;
        let event = vm
            .wait_for_event(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let message = match event {
            VmEvent::PendingTxs => vm::Message::BuildBlock,
            VmEvent::StateSyncDone => vm::Message::StateSyncFinished,
        };
        Ok(Response::new(WaitForEventResponse {
            message: message as i32,
        }))
    }

    async fn connected(
        &self,
        request: Request<vm::ConnectedRequest>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = ava_types::node_id::NodeId::from_slice(&req.node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let version =
            ava_version::application::Application::new(req.name, req.major, req.minor, req.patch);
        let mut vm = self.vm.lock().await;
        vm.connected(&self.token, node, version)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn disconnected(
        &self,
        request: Request<vm::DisconnectedRequest>,
    ) -> Result<Response<()>, Status> {
        let node = ava_types::node_id::NodeId::from_slice(&request.into_inner().node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let mut vm = self.vm.lock().await;
        vm.disconnected(&self.token, node)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn build_block(
        &self,
        _request: Request<BuildBlockRequest>,
    ) -> Result<Response<BuildBlockResponse>, Status> {
        let mut vm = self.vm.lock().await;
        let blk = vm
            .build_block(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        drop(vm);
        let vwc = block_verify_with_context(&blk);
        self.remember(&blk);
        Ok(Response::new(BuildBlockResponse {
            id: bytes::Bytes::copy_from_slice(&blk.id().to_bytes()),
            parent_id: bytes::Bytes::copy_from_slice(&blk.parent().to_bytes()),
            bytes: bytes::Bytes::copy_from_slice(blk.bytes()),
            height: blk.height(),
            timestamp: system_time_to_proto(blk.timestamp()),
            verify_with_context: vwc,
        }))
    }

    async fn parse_block(
        &self,
        request: Request<ParseBlockRequest>,
    ) -> Result<Response<ParseBlockResponse>, Status> {
        let bytes = request.into_inner().bytes;
        let vm = self.vm.lock().await;
        let blk = vm
            .parse_block(&self.token, &bytes)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        drop(vm);
        let vwc = block_verify_with_context(&blk);
        self.remember(&blk);
        Ok(Response::new(ParseBlockResponse {
            id: bytes::Bytes::copy_from_slice(&blk.id().to_bytes()),
            parent_id: bytes::Bytes::copy_from_slice(&blk.parent().to_bytes()),
            height: blk.height(),
            timestamp: system_time_to_proto(blk.timestamp()),
            verify_with_context: vwc,
        }))
    }

    async fn get_block(
        &self,
        request: Request<GetBlockRequest>,
    ) -> Result<Response<GetBlockResponse>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let vm = self.vm.lock().await;
        match vm.get_block(&self.token, id).await {
            Ok(blk) => {
                drop(vm);
                let vwc = block_verify_with_context(&blk);
                self.remember(&blk);
                Ok(Response::new(GetBlockResponse {
                    parent_id: bytes::Bytes::copy_from_slice(&blk.parent().to_bytes()),
                    bytes: bytes::Bytes::copy_from_slice(blk.bytes()),
                    height: blk.height(),
                    timestamp: system_time_to_proto(blk.timestamp()),
                    err: vm::Error::Unspecified as i32,
                    verify_with_context: vwc,
                }))
            }
            Err(e) => Ok(Response::new(GetBlockResponse {
                parent_id: bytes::Bytes::new(),
                bytes: bytes::Bytes::new(),
                height: 0,
                timestamp: None,
                err: error_to_enum(&e),
                verify_with_context: false,
            })),
        }
    }

    async fn set_preference(
        &self,
        request: Request<SetPreferenceRequest>,
    ) -> Result<Response<()>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let mut vm = self.vm.lock().await;
        vm.set_preference(&self.token, id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn health(&self, _request: Request<()>) -> Result<Response<HealthResponse>, Status> {
        let vm = self.vm.lock().await;
        let value = vm
            .health_check(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        let details = if value.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&value).map_err(|e| Status::internal(e.to_string()))?
        };
        Ok(Response::new(HealthResponse {
            details: details.into(),
        }))
    }

    async fn version(&self, _request: Request<()>) -> Result<Response<VersionResponse>, Status> {
        let vm = self.vm.lock().await;
        let version = vm
            .version(&self.token)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(VersionResponse { version }))
    }

    async fn app_request(
        &self,
        request: Request<vm::AppRequestMsg>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = ava_types::node_id::NodeId::from_slice(&req.node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // The wire deadline carries the remaining duration anchored at the host;
        // reconstruct an Instant relative to the guest's clock (saturating so a
        // pathological duration can never panic — `arithmetic_side_effects`).
        let remaining = req
            .deadline
            .map(|d| {
                std::time::Duration::new(
                    u64::try_from(d.seconds).unwrap_or(0),
                    u32::try_from(d.nanos).unwrap_or(0),
                )
            })
            .unwrap_or_default();
        let deadline = std::time::Instant::now()
            .checked_add(remaining)
            .unwrap_or_else(std::time::Instant::now);
        let mut vm = self.vm.lock().await;
        vm.app_request(&self.token, node, req.request_id, deadline, &req.request)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn app_request_failed(
        &self,
        request: Request<vm::AppRequestFailedMsg>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = ava_types::node_id::NodeId::from_slice(&req.node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let err = ava_vm::app::AppError::new(req.error_code, req.error_message);
        let mut vm = self.vm.lock().await;
        vm.app_request_failed(&self.token, node, req.request_id, err)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn app_response(
        &self,
        request: Request<vm::AppResponseMsg>,
    ) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = ava_types::node_id::NodeId::from_slice(&req.node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let mut vm = self.vm.lock().await;
        vm.app_response(&self.token, node, req.request_id, &req.response)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn app_gossip(&self, request: Request<vm::AppGossipMsg>) -> Result<Response<()>, Status> {
        let req = request.into_inner();
        let node = ava_types::node_id::NodeId::from_slice(&req.node_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let mut vm = self.vm.lock().await;
        vm.app_gossip(&self.token, node, &req.msg)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn gather(&self, _request: Request<()>) -> Result<Response<vm::GatherResponse>, Status> {
        // Metric gathering is deferred (no Prometheus registry plumbed through
        // the VM trait yet); report no families.
        Ok(Response::new(vm::GatherResponse {
            metric_families: Vec::new(),
        }))
    }

    async fn get_ancestors(
        &self,
        _request: Request<vm::GetAncestorsRequest>,
    ) -> Result<Response<vm::GetAncestorsResponse>, Status> {
        Err(Status::unimplemented(
            "GetAncestors (batched) — M3.25 follow-up",
        ))
    }

    async fn batched_parse_block(
        &self,
        _request: Request<vm::BatchedParseBlockRequest>,
    ) -> Result<Response<vm::BatchedParseBlockResponse>, Status> {
        Err(Status::unimplemented("BatchedParseBlock — M3.25 follow-up"))
    }

    async fn get_block_id_at_height(
        &self,
        request: Request<GetBlockIdAtHeightRequest>,
    ) -> Result<Response<GetBlockIdAtHeightResponse>, Status> {
        let height = request.into_inner().height;
        let vm = self.vm.lock().await;
        match vm.get_block_id_at_height(&self.token, height).await {
            Ok(id) => Ok(Response::new(GetBlockIdAtHeightResponse {
                blk_id: bytes::Bytes::copy_from_slice(&id.to_bytes()),
                err: vm::Error::Unspecified as i32,
            })),
            Err(e) => Ok(Response::new(GetBlockIdAtHeightResponse {
                blk_id: bytes::Bytes::new(),
                err: error_to_enum(&e),
            })),
        }
    }

    async fn state_sync_enabled(
        &self,
        _request: Request<()>,
    ) -> Result<Response<vm::StateSyncEnabledResponse>, Status> {
        Ok(Response::new(vm::StateSyncEnabledResponse {
            enabled: false,
            err: vm::Error::Unspecified as i32,
        }))
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        _request: Request<()>,
    ) -> Result<Response<vm::GetOngoingSyncStateSummaryResponse>, Status> {
        Ok(Response::new(vm::GetOngoingSyncStateSummaryResponse {
            id: bytes::Bytes::new(),
            height: 0,
            bytes: bytes::Bytes::new(),
            err: vm::Error::NotFound as i32,
        }))
    }

    async fn get_last_state_summary(
        &self,
        _request: Request<()>,
    ) -> Result<Response<vm::GetLastStateSummaryResponse>, Status> {
        Ok(Response::new(vm::GetLastStateSummaryResponse {
            id: bytes::Bytes::new(),
            height: 0,
            bytes: bytes::Bytes::new(),
            err: vm::Error::StateSyncNotImplemented as i32,
        }))
    }

    async fn parse_state_summary(
        &self,
        _request: Request<vm::ParseStateSummaryRequest>,
    ) -> Result<Response<vm::ParseStateSummaryResponse>, Status> {
        Ok(Response::new(vm::ParseStateSummaryResponse {
            id: bytes::Bytes::new(),
            height: 0,
            err: vm::Error::StateSyncNotImplemented as i32,
        }))
    }

    async fn get_state_summary(
        &self,
        _request: Request<vm::GetStateSummaryRequest>,
    ) -> Result<Response<vm::GetStateSummaryResponse>, Status> {
        Ok(Response::new(vm::GetStateSummaryResponse {
            id: bytes::Bytes::new(),
            bytes: bytes::Bytes::new(),
            err: vm::Error::StateSyncNotImplemented as i32,
        }))
    }

    async fn block_verify(
        &self,
        request: Request<BlockVerifyRequest>,
    ) -> Result<Response<BlockVerifyResponse>, Status> {
        let req = request.into_inner();
        // The host sends the block bytes; resolve via parse (idempotent) so the
        // call works for blocks the guest may not have built locally.
        let vm = self.vm.lock().await;
        let blk = vm
            .parse_block(&self.token, &req.bytes)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        blk.verify(&self.token)
            .await
            .map_err(|e| Status::internal(format!("{e:?}")))?;
        let ts = blk.timestamp();
        drop(vm);
        self.remember(&blk);
        Ok(Response::new(BlockVerifyResponse {
            timestamp: system_time_to_proto(ts),
        }))
    }

    async fn block_accept(
        &self,
        request: Request<BlockAcceptRequest>,
    ) -> Result<Response<()>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let blk = self
            .lookup(id)
            .ok_or_else(|| Status::not_found("unknown block id for accept"))?;
        blk.accept(&self.token)
            .await
            .map_err(|e| Status::internal(format!("{e:?}")))?;
        Ok(Response::new(()))
    }

    async fn block_reject(
        &self,
        request: Request<BlockRejectRequest>,
    ) -> Result<Response<()>, Status> {
        let id = Id::from_slice(&request.into_inner().id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let blk = self
            .lookup(id)
            .ok_or_else(|| Status::not_found("unknown block id for reject"))?;
        blk.reject(&self.token)
            .await
            .map_err(|e| Status::internal(format!("{e:?}")))?;
        Ok(Response::new(()))
    }

    async fn state_summary_accept(
        &self,
        _request: Request<vm::StateSummaryAcceptRequest>,
    ) -> Result<Response<vm::StateSummaryAcceptResponse>, Status> {
        Ok(Response::new(vm::StateSummaryAcceptResponse {
            mode: vm::state_summary_accept_response::Mode::Skipped as i32,
            err: vm::Error::StateSyncNotImplemented as i32,
        }))
    }
}

/// Maps an [`InitializeRequest`](vm::InitializeRequest) onto an
/// [`Arc<ChainContext>`](ava_snow::ChainContext) (07 §5.2).
///
/// Identity fields map verbatim. Empty id/node-id byte fields decode to the
/// zero id (Go's `ids.Empty`). The BLS `public_key` is the 96-byte uncompressed
/// form (`bls.PublicKeyToUncompressedBytes`); an empty field means no key. The
/// fork schedule is reconstructed from `network_id` (the host sends
/// `network_upgrades = None` for the in-process path — see `tests/PORTING.md`).
fn request_to_chain_context(
    req: &vm::InitializeRequest,
) -> std::result::Result<Arc<ava_snow::ChainContext>, String> {
    let id_or_empty = |b: &[u8]| -> std::result::Result<Id, String> {
        if b.is_empty() {
            Ok(Id::EMPTY)
        } else {
            Id::from_slice(b).map_err(|e| e.to_string())
        }
    };
    let node_id = if req.node_id.is_empty() {
        ava_types::node_id::NodeId::default()
    } else {
        ava_types::node_id::NodeId::from_slice(&req.node_id).map_err(|e| e.to_string())?
    };
    let public_key = if req.public_key.is_empty() {
        None
    } else {
        Some(
            ava_crypto::bls::PublicKey::from_uncompressed(&req.public_key)
                .map_err(|e| e.to_string())?,
        )
    };
    Ok(Arc::new(ava_snow::ChainContext {
        network_id: req.network_id,
        subnet_id: id_or_empty(&req.subnet_id)?,
        chain_id: id_or_empty(&req.chain_id)?,
        node_id,
        public_key,
        network_upgrades: ava_version::upgrade::get_config(req.network_id),
        x_chain_id: id_or_empty(&req.x_chain_id)?,
        c_chain_id: id_or_empty(&req.c_chain_id)?,
        avax_asset_id: id_or_empty(&req.avax_asset_id)?,
        chain_data_dir: std::path::PathBuf::from(&req.chain_data_dir),
    }))
}

/// Maps a wire [`vm::State`] enum value to an [`EngineState`], or `None` for the
/// unspecified sentinel (a benign probe that does not change phase).
fn proto_to_engine_state(state: i32) -> Option<ava_snow::EngineState> {
    match vm::State::try_from(state) {
        Ok(vm::State::StateSyncing) => Some(ava_snow::EngineState::StateSyncing),
        Ok(vm::State::Bootstrapping) => Some(ava_snow::EngineState::Bootstrapping),
        Ok(vm::State::NormalOp) => Some(ava_snow::EngineState::NormalOp),
        Ok(vm::State::Unspecified) | Err(_) => None,
    }
}

/// Whether a block opts into `WithVerifyContext` (proposervm-driven). The plain
/// `Block` trait does not expose this; resolved downstream once per-block
/// `WithVerifyContext` probing lands on the wrapper (M3.25/M5). For now `false`.
fn block_verify_with_context(_blk: &Arc<dyn Block>) -> bool {
    false
}

/// Dials the host runtime at `engine_addr` and reports the handshake
/// (`Runtime.Initialize(protocol_version, vm_addr)`). Exposed so tests can drive
/// a mismatched protocol version (specs 07 §5.1).
///
/// # Errors
/// Returns [`ava_vm::Error::HandshakeFailed`] if the runtime cannot be dialed or
/// the `Initialize` RPC fails.
pub async fn report_handshake(
    engine_addr: &str,
    protocol_version: u32,
    vm_addr: &str,
) -> Result<(), VmError> {
    use crate::pb::vm::runtime::InitializeRequest;
    use crate::pb::vm::runtime::runtime_client::RuntimeClient;

    let mut client = RuntimeClient::connect(format!("http://{engine_addr}"))
        .await
        .map_err(|_| VmError::HandshakeFailed)?;
    client
        .initialize(InitializeRequest {
            protocol_version,
            addr: vm_addr.to_string(),
        })
        .await
        .map_err(|_| VmError::HandshakeFailed)?;
    Ok(())
}

/// Serves `vm` against an already-known host runtime address `engine_addr`
/// (the in-process / test entrypoint): bind `V`, report the handshake with the
/// real [`RPC_CHAIN_VM_PROTOCOL`], then serve the `VM` service on `V` until
/// `token` is cancelled.
///
/// # Errors
/// Returns [`ava_vm::Error::HandshakeFailed`] on a bind/dial/serve failure.
pub async fn serve_with_addr<V: ChainVm + 'static>(
    vm: V,
    engine_addr: &str,
    token: &CancellationToken,
) -> Result<(), VmError> {
    // 1. Bind the ephemeral VM listener V.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| VmError::HandshakeFailed)?;
    let v_addr = listener
        .local_addr()
        .map_err(|_| VmError::HandshakeFailed)?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    // 2. Stand up the VM service; spawn the server before reporting the
    //    handshake so the host can dial V the instant it learns the address.
    let server = VmServer::new(vm, token.clone());
    let serve_token = token.clone();
    let serve = tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(server.into_service())
            .serve_with_incoming_shutdown(incoming, async move {
                serve_token.cancelled().await;
            })
            .await;
    });

    // 3. Dial R and report the handshake with the real protocol version.
    report_handshake(engine_addr, RPC_CHAIN_VM_PROTOCOL, &v_addr.to_string()).await?;

    // 4. Block until the server task ends (token cancelled).
    let _ = serve.await;
    Ok(())
}

/// The plugin `main()` entrypoint (`rpcchainvm.Serve`, specs 07 §5.3): read the
/// host runtime address from [`ENGINE_ADDRESS_KEY`], then [`serve_with_addr`].
///
/// # Errors
/// Returns [`ava_vm::Error::ProcessNotFound`] if the env var is unset, else any
/// error from [`serve_with_addr`].
pub async fn serve<V: ChainVm + 'static>(vm: V, token: &CancellationToken) -> Result<(), VmError> {
    let engine_addr = std::env::var(ENGINE_ADDRESS_KEY).map_err(|_| VmError::ProcessNotFound)?;
    serve_with_addr(vm, &engine_addr, token).await
}

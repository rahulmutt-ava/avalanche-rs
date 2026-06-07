// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `proto/validatorstate` `ValidatorState` proxy (specs 06 §6.1, 07 §5.4).
//!
//! Symmetry (07 §5.3): the plugin **dials** ([`dial`] → [`RpcValidatorState`], a
//! guest [`ValidatorState`] over the channel); the node **serves** ([`serve`] →
//! a [`ValidatorStateServer`] wrapping the host's `Arc<dyn ValidatorState>`).
//!
//! **Public-key deserialization gap.** The wire carries BLS public keys as
//! *uncompressed* 96-byte bytes (`bls.PublicKeyToUncompressedBytes`).
//! `ava-crypto` exposes [`PublicKey::serialize`] (host → wire, used by [`serve`])
//! but no `from_uncompressed` (wire → key). The guest-side decode therefore
//! tries [`PublicKey::from_compressed`] and, failing that (the common
//! uncompressed case), yields `None`. **This is a real fidelity gap** — add
//! `PublicKey::from_uncompressed` to `ava-crypto` and use it here. Recorded in
//! `tests/PORTING.md`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tonic::transport::Channel;
use tonic::{Request, Response, Status};

use ava_crypto::bls::PublicKey;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorState;
use ava_validators::state::{GetCurrentValidatorOutput, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_vm::error::{Error, Result};

use crate::pb::validatorstate::validator_state_client::ValidatorStateClient;
use crate::pb::validatorstate::validator_state_server::{
    ValidatorState as ValidatorStateService, ValidatorStateServer as PbValidatorStateServer,
};
use crate::pb::validatorstate::{
    GetCurrentHeightResponse, GetCurrentValidatorSetRequest, GetCurrentValidatorSetResponse,
    GetMinimumHeightResponse, GetSubnetIdRequest, GetSubnetIdResponse, GetValidatorSetRequest,
    GetValidatorSetResponse, GetWarpValidatorSetsRequest, GetWarpValidatorSetsResponse, Validator,
    WarpValidator, WarpValidatorSet,
};

/// Decodes an optional BLS public key from its wire (uncompressed) bytes.
///
/// See the module-level gap note: only the compressed form round-trips today;
/// an uncompressed key yields `None`.
fn decode_public_key(b: &[u8]) -> Option<PublicKey> {
    if b.is_empty() {
        return None;
    }
    PublicKey::from_compressed(b).ok()
}

/// Encodes an optional public key to its wire (uncompressed) bytes.
fn encode_public_key(pk: Option<&PublicKey>) -> bytes::Bytes {
    pk.map_or_else(bytes::Bytes::new, |k| {
        bytes::Bytes::copy_from_slice(&k.serialize())
    })
}

/// The guest-side `proto/validatorstate` client: a [`ValidatorState`] over the
/// channel.
pub struct RpcValidatorState {
    client: Mutex<ValidatorStateClient<Channel>>,
}

/// Dials the host-served `ValidatorState` at `addr` and builds the guest-side
/// [`RpcValidatorState`].
///
/// # Errors
/// Returns [`Error::HandshakeFailed`] if the channel cannot be established.
pub async fn dial(addr: &str) -> Result<RpcValidatorState> {
    let client = ValidatorStateClient::connect(format!("http://{addr}"))
        .await
        .map_err(|_| Error::HandshakeFailed)?;
    Ok(RpcValidatorState {
        client: Mutex::new(client),
    })
}

/// Maps an `ava_validators` result error to the crate error (transport only;
/// the validator-state RPCs do not use the `database.ErrNotFound` enum).
fn val_err(_e: ava_validators::Error) -> Error {
    Error::HandshakeFailed
}

#[async_trait]
impl ValidatorState for RpcValidatorState {
    async fn get_minimum_height(&self) -> ava_validators::Result<u64> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_minimum_height(())
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        Ok(resp.height)
    }

    async fn get_current_height(&self) -> ava_validators::Result<u64> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_current_height(())
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        Ok(resp.height)
    }

    async fn get_subnet_id(&self, chain: Id) -> ava_validators::Result<Id> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_subnet_id(GetSubnetIdRequest {
                chain_id: bytes::Bytes::copy_from_slice(&chain.to_bytes()),
            })
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        Id::from_slice(&resp.subnet_id).map_err(|_| ava_validators::Error::MissingValidators)
    }

    async fn get_validator_set(
        &self,
        height: u64,
        subnet: Id,
    ) -> ava_validators::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_validator_set(GetValidatorSetRequest {
                height,
                subnet_id: bytes::Bytes::copy_from_slice(&subnet.to_bytes()),
            })
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        let mut out = BTreeMap::new();
        for v in resp.validators {
            let node = NodeId::from_slice(&v.node_id)
                .map_err(|_| ava_validators::Error::MissingValidators)?;
            out.insert(
                node,
                GetValidatorOutput {
                    node_id: node,
                    public_key: decode_public_key(&v.public_key),
                    weight: v.weight,
                },
            );
        }
        Ok(out)
    }

    async fn get_current_validator_set(
        &self,
        subnet: Id,
    ) -> ava_validators::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_current_validator_set(GetCurrentValidatorSetRequest {
                subnet_id: bytes::Bytes::copy_from_slice(&subnet.to_bytes()),
            })
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        let mut out = BTreeMap::new();
        for v in resp.validators {
            let node = NodeId::from_slice(&v.node_id)
                .map_err(|_| ava_validators::Error::MissingValidators)?;
            let validation_id = Id::from_slice(&v.validation_id)
                .map_err(|_| ava_validators::Error::MissingValidators)?;
            out.insert(
                validation_id,
                GetCurrentValidatorOutput {
                    validation_id,
                    node_id: node,
                    public_key: decode_public_key(&v.public_key),
                    weight: v.weight,
                    start_time: v.start_time,
                    min_nonce: v.min_nonce,
                    is_active: v.is_active,
                    is_l1_validator: v.is_l1_validator,
                },
            );
        }
        Ok((out, resp.current_height))
    }

    async fn get_warp_validator_sets(
        &self,
        height: u64,
    ) -> ava_validators::Result<HashMap<Id, WarpSet>> {
        let mut client = self.client.lock().clone();
        let resp = client
            .get_warp_validator_sets(GetWarpValidatorSetsRequest { height })
            .await
            .map_err(|_| ava_validators::Error::MissingValidators)?
            .into_inner();
        let mut out = HashMap::new();
        for ws in resp.validator_sets {
            let subnet = Id::from_slice(&ws.subnet_id)
                .map_err(|_| ava_validators::Error::MissingValidators)?;
            let mut validators = Vec::with_capacity(ws.validators.len());
            for wv in ws.validators {
                // A WarpValidator may carry multiple node ids sharing one key;
                // expand each into a GetValidatorOutput (NodeId-keyed downstream).
                let public_key = decode_public_key(&wv.public_key);
                for nid in &wv.node_ids {
                    let node = NodeId::from_slice(nid)
                        .map_err(|_| ava_validators::Error::MissingValidators)?;
                    validators.push(GetValidatorOutput {
                        node_id: node,
                        public_key: public_key.clone(),
                        weight: wv.weight,
                    });
                }
            }
            out.insert(
                subnet,
                WarpSet {
                    validators,
                    total_weight: ws.total_weight,
                },
            );
        }
        Ok(out)
    }
}

/// The node-side `ValidatorState` tonic service wrapping the host's impl.
pub struct ValidatorStateServer {
    state: Arc<dyn ValidatorState>,
}

/// Wraps a host [`ValidatorState`] as the node-side service wrapper. Call
/// [`ValidatorStateServer::into_service`] for the tower service.
#[must_use]
pub fn serve(state: Arc<dyn ValidatorState>) -> ValidatorStateServer {
    ValidatorStateServer { state }
}

impl ValidatorStateServer {
    /// Consumes `self` into a tower service for `tonic::transport::Server`.
    #[must_use]
    pub fn into_service(self) -> PbValidatorStateServer<Self> {
        PbValidatorStateServer::new(self)
    }
}

#[tonic::async_trait]
impl ValidatorStateService for ValidatorStateServer {
    async fn get_minimum_height(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<GetMinimumHeightResponse>, Status> {
        let height = self
            .state
            .get_minimum_height()
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetMinimumHeightResponse { height }))
    }

    async fn get_current_height(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<GetCurrentHeightResponse>, Status> {
        let height = self
            .state
            .get_current_height()
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetCurrentHeightResponse { height }))
    }

    async fn get_subnet_id(
        &self,
        request: Request<GetSubnetIdRequest>,
    ) -> std::result::Result<Response<GetSubnetIdResponse>, Status> {
        let chain = Id::from_slice(&request.into_inner().chain_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let subnet = self
            .state
            .get_subnet_id(chain)
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(GetSubnetIdResponse {
            subnet_id: bytes::Bytes::copy_from_slice(&subnet.to_bytes()),
        }))
    }

    async fn get_warp_validator_sets(
        &self,
        request: Request<GetWarpValidatorSetsRequest>,
    ) -> std::result::Result<Response<GetWarpValidatorSetsResponse>, Status> {
        let height = request.into_inner().height;
        let sets = self
            .state
            .get_warp_validator_sets(height)
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        // Sort by subnet id for a deterministic wire order (00 §6.1).
        let mut entries: Vec<_> = sets.into_iter().collect();
        entries.sort_by_key(|(id, _)| *id);
        let validator_sets = entries
            .into_iter()
            .map(|(subnet, ws)| WarpValidatorSet {
                subnet_id: bytes::Bytes::copy_from_slice(&subnet.to_bytes()),
                total_weight: ws.total_weight,
                validators: ws
                    .validators
                    .into_iter()
                    .map(|v| WarpValidator {
                        public_key: encode_public_key(v.public_key.as_ref()),
                        weight: v.weight,
                        node_ids: vec![bytes::Bytes::copy_from_slice(v.node_id.as_bytes())],
                    })
                    .collect(),
            })
            .collect();
        Ok(Response::new(GetWarpValidatorSetsResponse {
            validator_sets,
        }))
    }

    async fn get_validator_set(
        &self,
        request: Request<GetValidatorSetRequest>,
    ) -> std::result::Result<Response<GetValidatorSetResponse>, Status> {
        let req = request.into_inner();
        let subnet =
            Id::from_slice(&req.subnet_id).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let set = self
            .state
            .get_validator_set(req.height, subnet)
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        // BTreeMap iteration is already NodeId-ascending (deterministic).
        let validators = set
            .into_values()
            .map(|v| Validator {
                node_id: bytes::Bytes::copy_from_slice(v.node_id.as_bytes()),
                weight: v.weight,
                public_key: encode_public_key(v.public_key.as_ref()),
                start_time: 0,
                min_nonce: 0,
                is_active: false,
                validation_id: bytes::Bytes::new(),
                is_l1_validator: false,
            })
            .collect();
        Ok(Response::new(GetValidatorSetResponse { validators }))
    }

    async fn get_current_validator_set(
        &self,
        request: Request<GetCurrentValidatorSetRequest>,
    ) -> std::result::Result<Response<GetCurrentValidatorSetResponse>, Status> {
        let subnet = Id::from_slice(&request.into_inner().subnet_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let (set, current_height) = self
            .state
            .get_current_validator_set(subnet)
            .await
            .map_err(val_err)
            .map_err(|e| Status::internal(e.to_string()))?;
        let validators = set
            .into_values()
            .map(|v| Validator {
                node_id: bytes::Bytes::copy_from_slice(v.node_id.as_bytes()),
                weight: v.weight,
                public_key: encode_public_key(v.public_key.as_ref()),
                start_time: v.start_time,
                min_nonce: v.min_nonce,
                is_active: v.is_active,
                validation_id: bytes::Bytes::copy_from_slice(&v.validation_id.to_bytes()),
                is_l1_validator: v.is_l1_validator,
            })
            .collect();
        Ok(Response::new(GetCurrentValidatorSetResponse {
            validators,
            current_height,
        }))
    }
}

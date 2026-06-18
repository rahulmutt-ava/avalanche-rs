// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! No-op default implementations of the `server_addr` callback-bundle traits
//! (specs 07 §5.2).
//!
//! Go's `vm_client.go:newInitServer` registers sharedmemory + aliasreader +
//! appsender + validatorState + warp on a single `server_addr`, each backed by a
//! concrete handle off the `snow.Context`. The Rust port wires those handles
//! per-VM (there is no `ChainContext`-carried bundle), so when the node assembly
//! does not supply a concrete impl the corresponding service is still registered
//! with a benign no-op — the guest dials the bundle back at `VM.Initialize` and
//! constructs its proxy clients lazily, so an unsupplied service only matters if a
//! hosted VM actually calls it.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;

use ava_database::BatchOps;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};
use ava_vm::error::{Error as VmError, Result as VmResult};

use crate::proxy::aliasreader::AliaserReader;
use crate::proxy::warp::Signer;

/// A `SharedMemory` that holds nothing: reads return empties, `apply` is a no-op.
/// `get` returns exactly `keys.len()` empty values (Go's length contract).
pub(crate) struct NoopSharedMemory;

impl SharedMemory for NoopSharedMemory {
    fn get(&self, _peer_chain: Id, keys: &[Vec<u8>]) -> VmResult<Vec<Vec<u8>>> {
        Ok(vec![Vec::new(); keys.len()])
    }

    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> VmResult<IndexedResult> {
        Ok((Vec::new(), Vec::new(), Vec::new()))
    }

    fn apply(&self, _requests: BTreeMap<Id, Requests>, _batches: &[BatchOps]) -> VmResult<()> {
        Ok(())
    }
}

/// An `AliaserReader` that resolves nothing.
pub(crate) struct NoopAliaser;

#[async_trait]
impl AliaserReader for NoopAliaser {
    async fn lookup(&self, _alias: &str) -> VmResult<Id> {
        Err(VmError::NotFound)
    }

    async fn primary_alias(&self, _id: Id) -> VmResult<String> {
        Err(VmError::NotFound)
    }

    async fn aliases(&self, _id: Id) -> VmResult<Vec<String>> {
        Ok(Vec::new())
    }
}

/// A `ValidatorState` with an empty validator set at height 0.
pub(crate) struct NoopValidatorState;

#[async_trait]
impl ValidatorState for NoopValidatorState {
    async fn get_minimum_height(&self) -> ava_validators::error::Result<u64> {
        Ok(0)
    }

    async fn get_current_height(&self) -> ava_validators::error::Result<u64> {
        Ok(0)
    }

    async fn get_subnet_id(&self, _chain: Id) -> ava_validators::error::Result<Id> {
        Ok(Id::EMPTY)
    }

    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> ava_validators::error::Result<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(BTreeMap::new())
    }

    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> ava_validators::error::Result<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 0))
    }

    async fn get_warp_validator_sets(
        &self,
        _height: u64,
    ) -> ava_validators::error::Result<HashMap<Id, WarpSet>> {
        Ok(HashMap::new())
    }
}

/// A warp `Signer` that cannot sign (the node supplied no signing backend).
pub(crate) struct NoopSigner;

#[async_trait]
impl Signer for NoopSigner {
    async fn sign(
        &self,
        _network_id: u32,
        _source_chain_id: Id,
        _payload: &[u8],
    ) -> VmResult<Vec<u8>> {
        Err(VmError::InvalidComponent(
            "no warp signer supplied to the host callback bundle",
        ))
    }
}

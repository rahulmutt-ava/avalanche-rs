// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `subnets.Subnet` (specs 07 §8.3) — the consensus configuration + allowed-node
//! ACL shared by every chain in a subnet.
//!
//! A subnet owns the consensus [`Parameters`] (Snowball K/alpha/beta), an
//! allowed-nodes ACL (`should_handle`, the handler-level filter, specs 06
//! §5.1), and per-subnet config. The special primary network is
//! `PRIMARY_NETWORK_ID = Id::EMPTY`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ava_snow::snowball::Parameters;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

/// `PRIMARY_NETWORK_ID` — the canonical primary network subnet id (`Id::EMPTY`,
/// re-exported from `ava_types::constants` for ergonomics, specs 03).
pub const PRIMARY_NETWORK_ID: Id = ava_types::constants::PRIMARY_NETWORK_ID;

/// `subnets.Config` — per-subnet configuration.
#[derive(Clone, Debug)]
pub struct SubnetConfig {
    /// Whether this subnet's chains are available only to subnet validators.
    pub validator_only: bool,
    /// Node ids explicitly allowed to connect when `validator_only` is set.
    pub allowed_nodes: HashSet<NodeId>,
    /// The Snowball consensus parameters for this subnet's chains.
    pub consensus_parameters: Parameters,
}

impl SubnetConfig {
    /// Builds a subnet config with the given consensus parameters and an open
    /// (non-validator-only) ACL.
    #[must_use]
    pub fn new(consensus_parameters: Parameters) -> Self {
        Self {
            validator_only: false,
            allowed_nodes: HashSet::new(),
            consensus_parameters,
        }
    }
}

/// `subnets.Subnet` — the runtime subnet handle owning the config + the set of
/// chains it has bootstrapped.
pub struct Subnet {
    subnet_id: Id,
    my_node_id: NodeId,
    config: SubnetConfig,
    chains: Mutex<HashSet<Id>>,
}

impl Subnet {
    /// `subnets.New(myNodeID, config)` — builds a subnet handle.
    #[must_use]
    pub fn new(subnet_id: Id, my_node_id: NodeId, config: SubnetConfig) -> Arc<Self> {
        Arc::new(Self {
            subnet_id,
            my_node_id,
            config,
            chains: Mutex::new(HashSet::new()),
        })
    }

    /// This subnet's id.
    #[must_use]
    pub fn id(&self) -> Id {
        self.subnet_id
    }

    /// The subnet's consensus parameters.
    #[must_use]
    pub fn consensus_parameters(&self) -> Parameters {
        self.config.consensus_parameters
    }

    /// The subnet's config.
    #[must_use]
    pub fn config(&self) -> &SubnetConfig {
        &self.config
    }

    /// `AddChain(chainID)` — records that `chain_id` belongs to this subnet.
    /// Returns `false` if it was already present.
    pub fn add_chain(&self, chain_id: Id) -> bool {
        self.chains
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(chain_id)
    }

    /// `IsAllowed(nodeID, isValidator)` — whether a node may connect to this
    /// subnet. The handler-level ACL (`Handler::should_handle`, specs 06 §5.1):
    /// allow if the node is us, the subnet is not validator-only, the node is a
    /// validator, or the node is explicitly allow-listed.
    #[must_use]
    pub fn should_handle(&self, node: NodeId, is_validator: bool) -> bool {
        node == self.my_node_id
            || !self.config.validator_only
            || is_validator
            || self.config.allowed_nodes.contains(&node)
    }
}

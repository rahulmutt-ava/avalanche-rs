// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `genesis.go::FromConfig` — the byte-exact genesis build pipeline
//! (specs 23 §3; the order is **load-bearing** for byte parity).
//!
//! §3.1 builds the X-Chain (AVM) genesis bytes and derives the AVAX asset ID;
//! §3.2 builds the P-Chain genesis UTXO allocations (config order, skipping the
//! initially-staked funds); §3.3 splits the staked funds across the initial
//! stakers ([`split_allocations`]) and builds the validators (staggered end
//! times); §3.4 assembles the P-Chain genesis (`platformvm/genesis.New`) with
//! the validators ordered by the **by-end-time heap**; §3.5 appends the fixed
//! chain list (X first, C second).
//!
//! Address plumbing note: Go round-trips every address through
//! `address.FormatBech32` and back (`bech32ToID`). bech32 is bijective on the
//! 20-byte payload, so this module carries [`ShortId`]s directly; orderings and
//! bytes are unchanged.

use std::collections::HashSet;

use ava_avm::genesis::{Genesis as AvmGenesis, GenesisAsset as AvmGenesisAsset};
use ava_avm::txs::codec::GenesisCodec as AvmGenesisCodec;
use ava_avm::txs::components::{AvaxBaseTx as AvmAvaxBaseTx, Output as AvmOutput};
use ava_avm::txs::initial_state::InitialState;
use ava_avm::txs::{
    BaseTx as AvmBaseTx, CODEC_VERSION as AVM_CODEC_VERSION, CreateAssetTx, Tx as AvmTx,
    UnsignedTx as AvmUnsignedTx,
};
use ava_platformvm::genesis::{Genesis as PGenesis, GenesisUtxo, Utxo};
use ava_platformvm::signer::{ProofOfPossession, Signer};
use ava_platformvm::stakeable::LockOut;
use ava_platformvm::txs::base_tx::BaseTx as PBaseTx;
use ava_platformvm::txs::components::{
    Auth, BaseTx as PAvaxBaseTx, Output as POutput, Owner as POwner, TransferableOutput,
};
use ava_platformvm::txs::create_chain::CreateChainTx;
use ava_platformvm::txs::validator::Validator as PValidator;
use ava_platformvm::txs::{
    AddPermissionlessValidatorTx, AddValidatorTx, GenesisCodec as PGenesisCodec, Tx as PTx,
    UnsignedTx as PUnsignedTx,
};
use ava_secp256k1fx::{OutputOwners, TransferOutput};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

use crate::chains::{ChainSpec, genesis_chains};
use crate::config::{Allocation, Config};
use crate::error::{GenesisError, Result};
use crate::split::split_allocations;

/// `avm.Holder` — how much asset an address owns at genesis (the address is
/// carried as its 20-byte payload; Go uses the bech32 string).
#[derive(Clone, Debug)]
struct Holder {
    amount: u64,
    address: ShortId,
}

/// `avm.NewGenesis` (FixedCap subset — `FromConfig` only emits FixedCap
/// holders): one `GenesisAsset` with the sorted secp256k1fx `TransferOutput`
/// initial state.
fn new_avm_genesis(
    network_id: u32,
    alias: &str,
    name: &str,
    symbol: &str,
    denomination: u8,
    memo: Vec<u8>,
    fixed_cap: &[Holder],
) -> AvmGenesis {
    let mut outs: Vec<AvmOutput> = fixed_cap
        .iter()
        .map(|holder| {
            AvmOutput::SecpTransfer(TransferOutput::new(
                holder.amount,
                OutputOwners::new(0, 1, vec![holder.address]),
            ))
        })
        .collect();
    // `initialState.Sort(codec)` — canonical codec-byte order of the outputs.
    let mut initial_state = InitialState::new(0, Vec::new());
    initial_state.outs = std::mem::take(&mut outs);
    initial_state.sort();

    let mut asset = AvmGenesisAsset {
        alias: alias.to_string(),
        tx: CreateAssetTx {
            base: AvmBaseTx::new(AvmAvaxBaseTx {
                network_id,
                blockchain_id: Id::EMPTY,
                outs: Vec::new(),
                ins: Vec::new(),
                memo,
            }),
            name: name.to_string(),
            symbol: symbol.to_string(),
            denomination,
            states: Vec::new(),
        },
    };
    if !initial_state.outs.is_empty() {
        asset.tx.states.push(initial_state);
    }
    // `utils.Sort(asset.States)` (by fx_index) and `utils.Sort(g.Txs)` (by
    // alias) — single-element here, kept for contract parity.
    asset.tx.states.sort_by(InitialState::compare);
    let mut genesis = AvmGenesis { txs: vec![asset] };
    genesis.txs.sort_by(|a, b| a.alias.cmp(&b.alias));
    genesis
}

/// `AVAXAssetID(avmGenesisBytes)` — re-parse the AVM genesis with the genesis
/// codec, wrap the first asset's `CreateAssetTx` in a `Tx`, initialize it and
/// return its id (specs 23 §3.1 step 8 — do **not** shortcut the re-parse).
///
/// # Errors
/// [`GenesisError::NoTxs`] for an empty genesis, else the codec error.
pub fn avax_asset_id(avm_genesis_bytes: &[u8]) -> Result<Id> {
    let mut genesis = AvmGenesis::default();
    AvmGenesisCodec().unmarshal(avm_genesis_bytes, &mut genesis)?;
    let genesis_tx = genesis.txs.into_iter().next().ok_or(GenesisError::NoTxs)?;
    let mut tx = AvmTx::new(AvmUnsignedTx::CreateAsset(genesis_tx.tx));
    tx.initialize(AvmGenesisCodec())?;
    Ok(tx.id())
}

// ---------------------------------------------------------------------------
// P-Chain genesis shapes (`vms/platformvm/genesis/genesis.go`)
// ---------------------------------------------------------------------------

/// `platformvm/genesis.Allocation` — a P-Chain genesis UTXO-to-be (the address
/// is carried as its 20-byte payload; Go uses the bech32 string).
#[derive(Clone, Debug)]
pub struct PlatformAllocation {
    /// `Locktime` (unix seconds; wrapped in a `stakeable.LockOut` when
    /// `> start_time`).
    pub locktime: u64,
    /// `Amount` (nAVAX).
    pub amount: u64,
    /// `Address` — the owner.
    pub address: ShortId,
    /// `Message` — the claiming eth address bytes.
    pub message: Vec<u8>,
}

impl PlatformAllocation {
    /// `Allocation.Compare` — locktime, then amount, then address.
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.locktime
            .cmp(&other.locktime)
            .then_with(|| self.amount.cmp(&other.amount))
            .then_with(|| self.address.cmp(&other.address))
    }
}

/// `platformvm/genesis.Owner` — a genesis reward owner.
#[derive(Clone, Debug, Default)]
pub struct PlatformOwner {
    /// `Locktime`.
    pub locktime: u64,
    /// `Threshold`.
    pub threshold: u32,
    /// `Addresses` (sorted before building the fx owner).
    pub addresses: Vec<ShortId>,
}

/// `platformvm/genesis.PermissionlessValidator` — a genesis validator.
#[derive(Clone, Debug)]
pub struct PermissionlessValidator {
    /// `StartTime` (unix seconds).
    pub start_time: u64,
    /// `EndTime` (unix seconds).
    pub end_time: u64,
    /// `NodeID`.
    pub node_id: NodeId,
    /// `RewardOwner`.
    pub reward_owner: PlatformOwner,
    /// `Staked` — the stake allocations (sorted by `Allocation.Compare`).
    pub staked: Vec<PlatformAllocation>,
    /// `ExactDelegationFee` (millionths).
    pub exact_delegation_fee: u32,
    /// `Signer` — `None` ⇒ legacy `AddValidatorTx`.
    pub signer: Option<ProofOfPossession>,
}

/// Go `container/heap` + `txheap.NewByEndTime`: a binary min-heap on the
/// validator end time (strict `Less`, no tie-break — equal keys never swap on
/// sift-up). `list()` returns the **heap array order**, which is what Go's
/// `heap.MapValues` exposes and what the genesis bytes encode.
#[derive(Default)]
struct ByEndTimeHeap {
    entries: Vec<PTx>,
}

impl ByEndTimeHeap {
    /// The validator tx's end time (only validator txs are pushed).
    fn end_time(tx: &PTx) -> u64 {
        match &tx.unsigned {
            PUnsignedTx::AddValidator(v) => v.validator.end,
            PUnsignedTx::AddPermissionlessValidator(v) => v.validator.end,
            _ => 0,
        }
    }

    /// `heap.Push` — append then sift up (mirrors Go `container/heap.up`).
    /// Go's `txHeap.Add` skips a tx whose ID is already in the heap.
    fn add(&mut self, tx: PTx) {
        if self.entries.iter().any(|e| e.id() == tx.id()) {
            return;
        }
        self.entries.push(tx);
        let mut j = self.entries.len().saturating_sub(1);
        while j > 0 {
            let i = (j.saturating_sub(1)) / 2; // parent
            let (Some(child), Some(parent)) = (self.entries.get(j), self.entries.get(i)) else {
                break;
            };
            if Self::end_time(child) >= Self::end_time(parent) {
                break;
            }
            self.entries.swap(i, j);
            j = i;
        }
    }

    /// `txHeap.List()` — the heap's internal array order.
    fn list(self) -> Vec<PTx> {
        self.entries
    }
}

/// `platformvm/genesis.New` — assembles the P-Chain genesis state
/// (specs 23 §3.4).
#[allow(clippy::too_many_arguments)] // mirrors the Go signature
fn new_platform_genesis(
    avax_asset_id: Id,
    network_id: u32,
    allocations: &[PlatformAllocation],
    validators: &[PermissionlessValidator],
    chains: &[ChainSpec],
    time: u64,
    initial_supply: u64,
    message: &str,
) -> Result<PGenesis> {
    // UTXOs — one per allocation, in input order; index == output_index.
    let mut utxos = Vec::with_capacity(allocations.len());
    let mut output_index: u32 = 0;
    for allocation in allocations {
        if allocation.amount == 0 {
            return Err(GenesisError::UtxoHasNoValue);
        }
        let mut out = POutput::Transfer(TransferOutput::new(
            allocation.amount,
            OutputOwners::new(0, 1, vec![allocation.address]),
        ));
        if allocation.locktime > time {
            out = POutput::StakeableLock(LockOut::new(allocation.locktime, out));
        }
        utxos.push(GenesisUtxo {
            utxo: Utxo {
                tx_id: Id::EMPTY,
                output_index,
                asset_id: avax_asset_id,
                out,
            },
            message: allocation.message.clone(),
        });
        output_index = output_index
            .checked_add(1)
            .ok_or(GenesisError::StakeOverflow)?;
    }

    // Validators — each becomes a tx, ordered by the by-end-time heap.
    let mut vdrs = ByEndTimeHeap::default();
    for vdr in validators {
        let mut weight: u64 = 0;
        let mut staked = vdr.staked.clone();
        staked.sort_by(PlatformAllocation::compare);
        let mut stake = Vec::with_capacity(staked.len());
        for allocation in &staked {
            let mut out = POutput::Transfer(TransferOutput::new(
                allocation.amount,
                OutputOwners::new(0, 1, vec![allocation.address]),
            ));
            if allocation.locktime > time {
                out = POutput::StakeableLock(LockOut::new(allocation.locktime, out));
            }
            stake.push(TransferableOutput {
                asset_id: avax_asset_id,
                out,
            });
            weight = weight
                .checked_add(allocation.amount)
                .ok_or(GenesisError::StakeOverflow)?;
        }

        if weight == 0 {
            return Err(GenesisError::ValidatorHasNoWeight);
        }
        if vdr.end_time <= time {
            return Err(GenesisError::ValidatorAlreadyExited);
        }

        let mut owner_addrs = vdr.reward_owner.addresses.clone();
        owner_addrs.sort();
        let owner = OutputOwners::new(
            vdr.reward_owner.locktime,
            vdr.reward_owner.threshold,
            owner_addrs,
        );

        let base = PBaseTx::new(PAvaxBaseTx {
            network_id,
            blockchain_id: Id::EMPTY,
            outs: Vec::new(),
            ins: Vec::new(),
            memo: Vec::new(),
        });
        let validator = PValidator {
            node_id: vdr.node_id,
            start: time,
            end: vdr.end_time,
            wght: weight,
        };
        let mut tx = match &vdr.signer {
            None => PTx::new(PUnsignedTx::AddValidator(AddValidatorTx {
                base,
                validator,
                stake_outs: stake,
                rewards_owner: POwner::Secp256k1(owner),
                delegation_shares: vdr.exact_delegation_fee,
            })),
            Some(pop) => PTx::new(PUnsignedTx::AddPermissionlessValidator(
                AddPermissionlessValidatorTx {
                    base,
                    validator,
                    subnet: Id::EMPTY,
                    signer: Signer::ProofOfPossession(pop.clone()),
                    stake_outs: stake,
                    validator_rewards_owner: POwner::Secp256k1(owner.clone()),
                    delegator_rewards_owner: POwner::Secp256k1(owner),
                    delegation_shares: vdr.exact_delegation_fee,
                    verified: std::cell::OnceCell::new(),
                },
            )),
        };
        tx.initialize(PGenesisCodec())?;
        vdrs.add(tx);
    }

    // Chains — each a CreateChainTx, in the fixed list order.
    let mut chain_txs = Vec::with_capacity(chains.len());
    for chain in chains {
        let mut tx = PTx::new(PUnsignedTx::CreateChain(CreateChainTx {
            base: PBaseTx::new(PAvaxBaseTx {
                network_id,
                blockchain_id: Id::EMPTY,
                outs: Vec::new(),
                ins: Vec::new(),
                memo: Vec::new(),
            }),
            subnet_id: chain.subnet_id,
            chain_name: chain.name.clone(),
            vm_id: chain.vm_id,
            fx_ids: chain.fx_ids.clone(),
            genesis_data: chain.genesis_data.clone(),
            subnet_auth: Auth::default(),
        }));
        tx.initialize(PGenesisCodec())?;
        chain_txs.push(tx);
    }

    Ok(PGenesis {
        utxos,
        validators: vdrs.list(),
        chains: chain_txs,
        timestamp: time,
        initial_supply,
        message: message.to_string(),
    })
}

// ---------------------------------------------------------------------------
// FromConfig (`genesis/genesis.go`)
// ---------------------------------------------------------------------------

/// `genesis.FromConfig` — builds `(p_chain_genesis_bytes, avax_asset_id)`
/// (specs 23 §3; the construction order is load-bearing).
///
/// # Errors
/// Any [`GenesisError`] surfaced by the §3 pipeline (supply overflow, zero-value
/// UTXO, exited/weightless validator, codec failure, …).
pub fn from_config(config: &Config) -> Result<(Vec<u8>, Id)> {
    // §3.1 — the X-Chain (AVM) genesis → bytes → AVAX asset id.
    let mut x_allocations: Vec<&Allocation> = config
        .allocations
        .iter()
        .filter(|allocation| allocation.initial_amount > 0)
        .collect();
    x_allocations.sort_by(|a, b| a.compare(b));

    let mut memo = Vec::with_capacity(x_allocations.len().saturating_mul(20));
    let mut fixed_cap = Vec::with_capacity(x_allocations.len());
    for allocation in &x_allocations {
        fixed_cap.push(Holder {
            amount: allocation.initial_amount,
            address: allocation.avax_addr,
        });
        memo.extend_from_slice(allocation.eth_addr.as_bytes());
    }
    let avm_genesis = new_avm_genesis(
        config.network_id,
        "AVAX",
        "Avalanche",
        "AVAX",
        9,
        memo,
        &fixed_cap,
    );
    let avm_genesis_bytes = AvmGenesisCodec().marshal(AVM_CODEC_VERSION, &avm_genesis)?;
    let avax_asset_id = avax_asset_id(&avm_genesis_bytes)?;

    let initial_supply = config.initial_supply()?;

    // §3.2 — P-Chain genesis UTXO allocations (config order, skipping the
    // initially-staked addresses).
    let initially_staked: HashSet<ShortId> = config.initial_staked_funds.iter().copied().collect();
    let mut platform_allocations = Vec::new();
    let mut skipped_allocations = Vec::new();
    for allocation in &config.allocations {
        if initially_staked.contains(&allocation.avax_addr) {
            skipped_allocations.push(allocation.clone());
            continue;
        }
        for unlock in &allocation.unlock_schedule {
            if unlock.amount > 0 {
                platform_allocations.push(PlatformAllocation {
                    locktime: unlock.locktime,
                    amount: unlock.amount,
                    address: allocation.avax_addr,
                    message: allocation.eth_addr.as_bytes().to_vec(),
                });
            }
        }
    }

    // §3.3 — initial validators (staggered end times; post-incremented offset).
    let all_node_allocations =
        split_allocations(&skipped_allocations, config.initial_stakers.len());
    let end_staking_time = config
        .start_time
        .checked_add(config.initial_stake_duration)
        .ok_or(GenesisError::TimeOverflow)?;
    let mut staking_offset: u64 = 0;
    let mut validators = Vec::with_capacity(config.initial_stakers.len());
    for (i, staker) in config.initial_stakers.iter().enumerate() {
        let node_allocations = all_node_allocations.get(i).map_or(&[][..], Vec::as_slice);
        let this_end_staking_time = end_staking_time
            .checked_sub(staking_offset)
            .ok_or(GenesisError::TimeOverflow)?;
        staking_offset = staking_offset
            .checked_add(config.initial_stake_duration_offset)
            .ok_or(GenesisError::TimeOverflow)?;

        let mut staked = Vec::new();
        for allocation in node_allocations {
            for unlock in &allocation.unlock_schedule {
                staked.push(PlatformAllocation {
                    locktime: unlock.locktime,
                    amount: unlock.amount,
                    address: allocation.avax_addr,
                    message: allocation.eth_addr.as_bytes().to_vec(),
                });
            }
        }

        validators.push(PermissionlessValidator {
            start_time: config.start_time,
            end_time: this_end_staking_time,
            node_id: staker.node_id,
            reward_owner: PlatformOwner {
                locktime: 0,
                threshold: 1,
                addresses: vec![staker.reward_address],
            },
            staked,
            exact_delegation_fee: staker.delegation_fee,
            signer: staker.signer.clone(),
        });
    }

    // §3.5 — the fixed chain list (X first, C second).
    let chains = genesis_chains(avm_genesis_bytes, &config.c_chain_genesis);

    // §3.4 — assemble + marshal the P-Chain genesis.
    let genesis = new_platform_genesis(
        avax_asset_id,
        config.network_id,
        &platform_allocations,
        &validators,
        &chains,
        config.start_time,
        initial_supply,
        &config.message,
    )?;
    let p_chain_genesis_bytes = ava_platformvm::genesis::marshal(&genesis)?;
    Ok((p_chain_genesis_bytes, avax_asset_id))
}

/// `genesis.VMGenesis` — parse the P-Chain genesis and return the
/// `CreateChainTx` whose VM id matches; its tx id is the blockchain id
/// (specs 23 §4.3).
///
/// # Errors
/// [`GenesisError::UnknownVmId`] when no chain runs `vm_id`, else the parse
/// error.
pub fn vm_genesis(genesis_bytes: &[u8], vm_id: Id) -> Result<PTx> {
    let genesis = ava_platformvm::genesis::parse(genesis_bytes)?;
    genesis
        .chains
        .into_iter()
        .find(|chain| matches!(&chain.unsigned, PUnsignedTx::CreateChain(c) if c.vm_id == vm_id))
        .ok_or(GenesisError::UnknownVmId(vm_id))
}

#[cfg(test)]
mod tests {
    use crate::config::{FUJI_CONFIG, MAINNET_CONFIG, UNMODIFIED_LOCAL_CONFIG};

    use super::*;

    /// M8.7 red test (TDD): the AVAX asset IDs for Mainnet/Fuji/Local match the
    /// Go golden table (specs 23 §7) — this exercises the §3.1 X-allocation
    /// sort + asset-tx hash end-to-end.
    #[test]
    fn avax_asset_id_matches_go() {
        let cases = [
            (
                &*MAINNET_CONFIG,
                "FvwEAhmxKfeiG8SnEvq42hc6whRyY3EFYAvebMqDNDGCgxN5Z",
            ),
            (
                &*FUJI_CONFIG,
                "U8iRqJoiJm8xZHAacmvYyZVwqQx6uDNtQeP3CQ6fcgQk3JqnK",
            ),
            (
                &*UNMODIFIED_LOCAL_CONFIG,
                "2fombhL7aGPwj3KH4bfrmJwW6PVnMobf9Y2fn9GwxiAAJyFDbe",
            ),
        ];
        for (config, expected) in cases {
            let (_p_bytes, asset_id) = from_config(config).expect("from_config");
            assert_eq!(
                asset_id.to_string(),
                expected,
                "network {}",
                config.network_id
            );
        }
    }
}

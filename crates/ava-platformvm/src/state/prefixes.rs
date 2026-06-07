// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Flat-KV prefix spaces for the persisted P-Chain [`State`](super::state::State)
//! (`vms/platformvm/state/state.go`, specs 08 §3.2).
//!
//! The P-Chain state lives in flat, prefixed key/value spaces over the base
//! [`Database`](ava_database::Database) — **not** a merkledb. Each prefix is a
//! distinct [`PrefixDb`](ava_database::PrefixDb) keyspace. The byte values of
//! the prefixes are an on-disk migration concern (specs 00 §4.4); only the UTXO
//! *value* bytes are cross-chain / protocol-relevant, so the prefix bytes here
//! mirror Go's `state.go` byte constants for parity but are not asserted
//! byte-exact by consensus.

/// `validatorsPrefix` parent space for the staker sublists.
pub const VALIDATORS_PREFIX: &[u8] = b"validator";
/// `blockPrefix` — height/blockID → block bytes.
pub const BLOCK_PREFIX: &[u8] = b"block";
/// `blockIDPrefix` — height → blockID.
pub const BLOCK_ID_PREFIX: &[u8] = b"blockID";
/// `txPrefix` — txID → {tx, status}.
pub const TX_PREFIX: &[u8] = b"tx";
/// `rewardUTXOsPrefix` — txID → reward UTXOs.
pub const REWARD_UTXOS_PREFIX: &[u8] = b"rewardUTXOs";
/// `utxoPrefix` — utxoID → UTXO bytes (the cross-chain-relevant value layout).
pub const UTXO_PREFIX: &[u8] = b"utxo";
/// `subnetPrefix` — the set of created subnets.
pub const SUBNET_PREFIX: &[u8] = b"subnet";
/// `subnetOwnerPrefix` — subnetID → owner bytes.
pub const SUBNET_OWNER_PREFIX: &[u8] = b"subnetOwner";
/// `subnetToL1ConversionPrefix` — subnetID → L1-conversion (manager) bytes.
pub const SUBNET_MANAGER_PREFIX: &[u8] = b"subnetManager";
/// `transformedSubnetPrefix` — subnetID → transform tx (legacy elastic subnets).
pub const TRANSFORMED_SUBNET_PREFIX: &[u8] = b"transformedSubnet";
/// `chainPrefix` — subnetID → chains created under it.
pub const CHAIN_PREFIX: &[u8] = b"chain";
/// `singletonPrefix` — scalar singletons (timestamp, supply, last accepted, …).
pub const SINGLETON_PREFIX: &[u8] = b"singleton";
/// `l1ValidatorsPrefix` parent space for the ACP-77 L1-validator sublists.
pub const L1_VALIDATORS_PREFIX: &[u8] = b"l1Validators";

// ----- staker sublist prefixes (children of `VALIDATORS_PREFIX`) -----

/// `currentPrefix` — the current staker sublists.
pub const CURRENT_PREFIX: &[u8] = b"current";
/// `pendingPrefix` — the pending staker sublists.
pub const PENDING_PREFIX: &[u8] = b"pending";
/// `validatorPrefix` — the validator sub-sublist within current/pending.
pub const VALIDATOR_PREFIX: &[u8] = b"validator";
/// `delegatorPrefix` — the delegator sub-sublist within current/pending.
pub const DELEGATOR_PREFIX: &[u8] = b"delegator";
/// `subnetValidatorPrefix` — the subnet-validator sub-sublist.
pub const SUBNET_VALIDATOR_PREFIX: &[u8] = b"subnetValidator";
/// `subnetDelegatorPrefix` — the subnet-delegator sub-sublist.
pub const SUBNET_DELEGATOR_PREFIX: &[u8] = b"subnetDelegator";

// ----- L1-validator sublist prefixes (children of `L1_VALIDATORS_PREFIX`) -----

/// `l1ValidatorPrefix` — validationID → L1 validator bytes.
pub const L1_VALIDATOR_PREFIX: &[u8] = b"l1Validator";
/// `weightDiffPrefix` — the staker weight-diff iterator space (written by
/// M4.14; the handle exists here so the disk iterator can build on it).
pub const WEIGHT_DIFF_PREFIX: &[u8] = b"flatWeightDiff";
/// `pkDiffPrefix` — the BLS public-key diff space (M4.14, handle only here).
pub const PK_DIFF_PREFIX: &[u8] = b"flatPublicKeyDiff";

// ----- singleton keys (within `SINGLETON_PREFIX`) -----

/// `timestampKey` — the chain time, big-endian unix seconds.
pub const TIMESTAMP_KEY: &[u8] = b"timestamp";
/// `feeStateKey` — the ACP-103 gas `(capacity, excess)` meter.
pub const FEE_STATE_KEY: &[u8] = b"fee state";
/// `l1ValidatorExcessKey` — the ACP-77 validator-fee excess accumulator.
pub const L1_VALIDATOR_EXCESS_KEY: &[u8] = b"l1 validator excess";
/// `accruedFeesKey` — the accrued continuous-fee total.
pub const ACCRUED_FEES_KEY: &[u8] = b"accrued fees";
/// `currentSupplyKey` — the Primary Network current supply.
pub const CURRENT_SUPPLY_KEY: &[u8] = b"current supply";
/// `lastAcceptedKey` — the last-accepted block ID.
pub const LAST_ACCEPTED_KEY: &[u8] = b"last accepted";
/// `heightKey` — the last-accepted block height.
pub const HEIGHT_KEY: &[u8] = b"height";

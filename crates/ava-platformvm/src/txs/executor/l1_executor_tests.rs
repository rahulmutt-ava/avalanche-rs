// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M4.19 conformance tests — the ACP-77 L1 validator lifecycle.
//!
//! Ported decision cases from Go `standard_tx_executor_test.go` (the L1 handler
//! cases buildable without the not-yet-ported quorum / expiry-set fixtures) plus
//! the buildable parts of `warp_verifier_test.go`. Each test builds a
//! `State`-backed `Diff`, runs the [`L1TxExecutor`] (with the
//! [`AcceptingVerifier`](crate::warp::verifier::AcceptingVerifier) quorum seam),
//! and asserts the resulting `Diff` mutations.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ava_database::MemDb;
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;

use super::*;
use crate::signer::ProofOfPossession;
use crate::state::chain::{Chain, Versions};
use crate::state::state::State;
use crate::txs::components::{
    Auth, BaseTx as AvaxBaseTx, Input, Output as ComponentOutput, Owner, PChainOwner,
    TransferableInput, TransferableOutput,
};
use crate::txs::convert_subnet_to_l1::ConvertSubnetToL1Validator;
use crate::txs::executor::backend::{StakingConfig, UpgradeSchedule};
use crate::txs::{Tx, UnsignedTx};
use crate::utxo::Utxo;
use crate::warp::message::{
    L1ValidatorWeight, PChainOwner as MsgOwner, RegisterL1Validator, RegistryPayload,
};
use crate::warp::payload::{AddressedCall, WarpPayload};
use crate::warp::verifier::AcceptingVerifier;
use crate::warp::{BitSetSignature, Message, Signature, UnsignedMessage};

const AVAX_ASSET: [u8; 32] = [0x42; 32];
const AVAX: u64 = 1_000_000_000;
/// The manager chain id recorded by a conversion.
const MANAGER_CHAIN: [u8; 32] = [0x99; 32];
/// The manager (source) address recorded by a conversion.
const MANAGER_ADDR: [u8; 20] = [0xAB; 20];

/// The known-good BLS PoP from the Go vectors (`localsigner.FromBytes`).
const BLS_PUBKEY: [u8; crate::signer::PUBLIC_KEY_LEN] = [
    0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3, 0xa4,
    0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b, 0x90, 0xcf,
    0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e, 0xd0, 0xd4, 0xd1,
];
const BLS_SIG: [u8; crate::signer::SIGNATURE_LEN] = [
    0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20, 0x7b,
    0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83, 0x46, 0xd9,
    0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b, 0x2a, 0x05, 0xe0,
    0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a, 0x18, 0xa0, 0x83, 0xe4,
    0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d, 0xa5, 0xc4, 0x89, 0x87, 0x2e,
    0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33, 0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
];

/// A `Versions` resolving exactly one parent block id.
struct SingleParent {
    id: Id,
    chain: Arc<dyn Chain>,
}
impl Versions for SingleParent {
    fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
        (block_id == self.id).then(|| Arc::clone(&self.chain))
    }
}

fn owners(addr: u8) -> OutputOwners {
    OutputOwners::new(0, 1, vec![ShortId::from([addr; 20])])
}

/// Builds a `Diff` over a fresh `State` whose chain time is `ts` and which holds
/// the given AVAX UTXOs (keyed by `(tx_id, index)`).
fn diff_with_utxos(ts: SystemTime, utxos: &[(Id, u32, u64)]) -> Diff {
    let mut state = State::new(MemDb::new()).expect("state");
    state.set_timestamp(ts);
    state.set_current_supply(Id::EMPTY, 100_000_000 * AVAX);
    for &(tx_id, index, amt) in utxos {
        let utxo = Utxo {
            tx_id,
            output_index: index,
            asset_id: Id::from(AVAX_ASSET),
            out: ComponentOutput::Transfer(TransferOutput::new(amt, owners(1))),
        };
        state.add_utxo(utxo.input_id(), utxo.marshal().expect("marshal utxo"));
    }
    let parent_id = Id::from([0xAB; 32]);
    let base: Arc<dyn Chain> = Arc::new(state);
    let versions = SingleParent {
        id: parent_id,
        chain: base,
    };
    Diff::new(parent_id, &versions).expect("diff")
}

/// A test backend with the given fork schedule, mainnet staking params, static
/// fees, and an un-bootstrapped fx (structural auth checks only).
fn backend(upgrades: UpgradeSchedule, bootstrapped: bool) -> Backend {
    Backend {
        upgrades,
        staking: StakingConfig::mainnet(),
        static_fee_config: crate::txs::fee::simple_calculator::StaticFeeConfig::MAINNET,
        network_id: 1,
        chain_id: Id::EMPTY,
        avax_asset_id: Id::from(AVAX_ASSET),
        node_id: NodeId::EMPTY,
        fx: ava_secp256k1fx::Fx::new(Arc::new(MockClock::at(SystemTime::UNIX_EPOCH))),
        bootstrapped,
    }
}

fn avax_input(tx_id: Id, index: u32, amt: u64) -> TransferableInput {
    TransferableInput {
        tx_id,
        output_index: index,
        asset_id: Id::from(AVAX_ASSET),
        r#in: Input::Transfer(TransferInput::new(amt, vec![0])),
    }
}

fn avax_output(amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id: Id::from(AVAX_ASSET),
        out: ComponentOutput::Transfer(TransferOutput::new(amt, owners(1))),
    }
}

/// The fee in force for a base tx under `backend`'s fork at `diff`'s gas excess
/// (the same fork-selected calculator the executor uses).
fn fee_for(backend: &Backend, diff: &Diff) -> u64 {
    state_changes::fee_calculator(backend, diff)
        .calculate_fee(crate::txs::fee::complexity::base_tx_complexity())
        .expect("fee")
}

/// A funded `BaseTx` consuming `(fund, 0, amt)` and changing back `amt - fee`,
/// where `fee` is the fork-selected fee for `backend`/`diff`.
fn base_tx(backend: &Backend, diff: &Diff, fund: Id, amt: u64) -> crate::txs::BaseTx {
    let fee = fee_for(backend, diff);
    crate::txs::BaseTx::new(AvaxBaseTx {
        network_id: 1,
        blockchain_id: Id::EMPTY,
        outs: vec![avax_output(amt - fee)],
        ins: vec![avax_input(fund, 0, amt)],
        memo: vec![],
    })
}

/// Runs `unsigned` with `creds`, dispatching through the L1 executor with the
/// accepting quorum seam.
fn run_creds(
    backend: &Backend,
    diff: &mut Diff,
    unsigned: UnsignedTx,
    creds: Vec<crate::txs::tx::Credential>,
) -> Result<Id> {
    let mut tx = Tx::new(unsigned);
    tx.creds = creds;
    tx.initialize(crate::txs::codec::Codec()).expect("init");
    let tx_id = tx.id();
    let unsigned_bytes = crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &tx.unsigned)
        .expect("marshal unsigned");
    let verifier = AcceptingVerifier;
    let mut exec = L1TxExecutor::new(backend, diff, &tx, unsigned_bytes, &verifier);
    tx.unsigned.visit(&mut exec).map(|()| tx_id)
}

fn run(backend: &Backend, diff: &mut Diff, unsigned: UnsignedTx) -> Result<Id> {
    run_creds(backend, diff, unsigned, vec![])
}

fn run_err(backend: &Backend, diff: &mut Diff, unsigned: UnsignedTx) -> Error {
    match run(backend, diff, unsigned) {
        Ok(_) => panic!("expected the tx to fail execution"),
        Err(e) => e,
    }
}

/// Wraps a [`RegistryPayload`] in the three Warp codec layers, returning the full
/// message bytes (source chain = [`MANAGER_CHAIN`], source address =
/// [`MANAGER_ADDR`]).
fn wrap_warp(payload: &RegistryPayload) -> Vec<u8> {
    let inner = payload.marshal().expect("marshal registry payload");
    let call = AddressedCall {
        source_address: MANAGER_ADDR.to_vec(),
        payload: inner,
    };
    let call_bytes = WarpPayload::AddressedCall(call)
        .marshal_payload()
        .expect("marshal addressed call");
    let unsigned = UnsignedMessage {
        network_id: 1,
        source_chain_id: Id::from(MANAGER_CHAIN),
        payload: call_bytes,
    };
    Message {
        unsigned_message: unsigned,
        signature: Signature::BitSet(BitSetSignature::default()),
    }
    .marshal()
    .expect("marshal message")
}

/// Installs an L1 conversion (manager chain + address) on `subnet` in `diff` so
/// `verify_l1_conversion` accepts a message originating from [`MANAGER_CHAIN`] /
/// [`MANAGER_ADDR`].
fn install_conversion(diff: &mut Diff, subnet: Id) {
    let conversion = SubnetConversion {
        conversion_id: Id::EMPTY,
        chain_id: Id::from(MANAGER_CHAIN),
        addr: MANAGER_ADDR.to_vec(),
    };
    diff.set_subnet_manager(subnet, conversion.marshal().expect("marshal conversion"));
}

/// A `ConvertSubnetToL1Tx` over a 0-of-0-owned `subnet`, funded by `(fund, 0,
/// amt)`, with one initial validator of `weight`/`balance`.
fn convert_tx(
    backend: &Backend,
    diff: &Diff,
    subnet: Id,
    fund: Id,
    amt: u64,
    weight: u64,
    balance: u64,
) -> ConvertSubnetToL1Tx {
    ConvertSubnetToL1Tx {
        base: base_tx(backend, diff, fund, amt),
        subnet,
        chain_id: Id::from(MANAGER_CHAIN),
        address: MANAGER_ADDR.to_vec(),
        validators: vec![ConvertSubnetToL1Validator {
            node_id: vec![0x07; 20],
            weight,
            balance,
            signer: ProofOfPossession::new(BLS_PUBKEY, BLS_SIG),
            remaining_balance_owner: PChainOwner {
                threshold: 1,
                addresses: vec![ShortId::from([0x44; 20])],
            },
            deactivation_owner: PChainOwner {
                threshold: 1,
                addresses: vec![ShortId::from([0x55; 20])],
            },
        }],
        // 0-of-0 owner authorizes with an empty sig-index set.
        subnet_auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
    }
}

/// Creates a permissioned subnet owned by a 0-of-0 owner so subnet auth passes
/// with a single empty credential.
fn create_permissioned_subnet(diff: &mut Diff, subnet: Id) {
    diff.add_subnet(subnet);
    let owner = Owner::Secp256k1(OutputOwners::new(0, 0, vec![]));
    let owner_bytes = crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &owner)
        .expect("marshal owner");
    diff.set_subnet_owner(subnet, owner_bytes);
}

/// `ConvertSubnetToL1Tx` is rejected before Etna activates.
#[test]
fn l1_lifecycle_convert_pre_etna_rejected() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut diff = diff_with_utxos(ts, &[]);
    let b = backend(UpgradeSchedule::durango_only(), true);
    let tx = convert_tx(
        &b,
        &diff,
        Id::from([0x33; 32]),
        Id::from([1; 32]),
        100 * AVAX,
        100,
        0,
    );
    let err = run_err(&b, &mut diff, UnsignedTx::ConvertSubnetToL1(tx));
    assert!(matches!(err, Error::EtnaUpgradeNotActive), "got {err:?}");
}

/// `ConvertSubnetToL1Tx` installs the initial L1 validator and records the
/// conversion manager; the funding UTXO is consumed.
#[test]
fn l1_lifecycle_convert_installs_validators() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([1; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    create_permissioned_subnet(&mut diff, subnet);

    // An active validator (balance > 0) accrues an EndAccumulatedFee.
    let balance = 5 * AVAX;
    let tx = convert_tx(&b, &diff, subnet, fund, 100 * AVAX, 100, balance);
    let creds = vec![crate::txs::tx::Credential { sigs: vec![] }];
    run_creds(&b, &mut diff, UnsignedTx::ConvertSubnetToL1(tx), creds).expect("convert");

    // The L1 validator is keyed by `subnet.append(0)`.
    let validation_id = subnet.append(&[0]);
    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.subnet_id, subnet);
    assert_eq!(v.weight, 100);
    assert_eq!(v.end_accumulated_fee, balance); // accrued_fees starts at 0
    assert!(v.is_active());
    // Total subnet weight reflects the new validator.
    assert_eq!(diff.weight_of_l1_validators(subnet).unwrap(), 100);
    // The conversion manager was recorded and decodes.
    let manager = diff.get_subnet_manager(subnet).expect("manager");
    let conversion = SubnetConversion::unmarshal(&manager).expect("decode conversion");
    assert_eq!(conversion.chain_id, Id::from(MANAGER_CHAIN));
    // The funding UTXO is consumed.
    assert!(diff.get_utxo(avax_input(fund, 0, 0).input_id()).is_err());
}

/// A `RegisterL1ValidatorTx` funds `EndAccumulatedFee` via a verified Warp
/// `RegisterL1Validator` message + PoP.
#[test]
fn l1_lifecycle_register_funds_end_accumulated_fee() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let now = ts.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let fund = Id::from([2; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    install_conversion(&mut diff, subnet);

    let payload = RegistryPayload::RegisterL1Validator(RegisterL1Validator {
        subnet_id: subnet,
        node_id: vec![0x07; 20],
        bls_public_key: BLS_PUBKEY,
        expiry: now + 60 * 60, // 1h into the future (inside the 1-day window)
        remaining_balance_owner: MsgOwner {
            threshold: 1,
            addresses: vec![ShortId::from([0x44; 20])],
        },
        disable_owner: MsgOwner {
            threshold: 0,
            addresses: vec![],
        },
        weight: 100,
    });
    let validation_id =
        RegisterL1Validator::validation_id(&payload.marshal().expect("marshal payload"));

    let balance = 7 * AVAX;
    let tx = RegisterL1ValidatorTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        balance,
        proof_of_possession: BLS_SIG,
        message: wrap_warp(&payload),
    };
    run(&b, &mut diff, UnsignedTx::RegisterL1Validator(tx)).expect("register");

    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.subnet_id, subnet);
    assert_eq!(v.weight, 100);
    assert_eq!(v.start_time, now);
    assert_eq!(v.end_accumulated_fee, balance);
    assert!(v.is_active());
}

/// A `RegisterL1ValidatorTx` whose message has already expired is rejected.
#[test]
fn l1_lifecycle_register_expired_rejected() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let now = ts.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let fund = Id::from([2; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    install_conversion(&mut diff, subnet);

    let payload = RegistryPayload::RegisterL1Validator(RegisterL1Validator {
        subnet_id: subnet,
        node_id: vec![0x07; 20],
        bls_public_key: BLS_PUBKEY,
        expiry: now, // == current time → expired (Go: expiry <= now)
        remaining_balance_owner: MsgOwner {
            threshold: 0,
            addresses: vec![],
        },
        disable_owner: MsgOwner {
            threshold: 0,
            addresses: vec![],
        },
        weight: 100,
    });
    let tx = RegisterL1ValidatorTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        balance: 0,
        proof_of_possession: BLS_SIG,
        message: wrap_warp(&payload),
    };
    let err = run_err(&b, &mut diff, UnsignedTx::RegisterL1Validator(tx));
    assert!(matches!(err, Error::WarpMessageExpired), "got {err:?}");
}

/// Seeds an active L1 validator directly into `diff` and returns its
/// validation id. `min_nonce`/`end_accumulated_fee` are configurable.
fn seed_validator(
    diff: &mut Diff,
    subnet: Id,
    validation_id: Id,
    weight: u64,
    min_nonce: u64,
    end_accumulated_fee: u64,
) {
    let owner = Owner::Secp256k1(OutputOwners::new(0, 1, vec![ShortId::from([0x44; 20])]));
    let owner_bytes = crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &owner)
        .expect("marshal owner");
    let disable = Owner::Secp256k1(OutputOwners::new(0, 0, vec![]));
    let disable_bytes = crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &disable)
        .expect("marshal disable owner");
    let v = L1Validator {
        validation_id,
        subnet_id: subnet,
        node_id: NodeId::from([0x07; 20]),
        public_key: vec![0x01; ava_crypto::bls::UNCOMPRESSED_PUBLIC_KEY_LEN],
        remaining_balance_owner: owner_bytes,
        deactivation_owner: disable_bytes,
        start_time: 0,
        weight,
        min_nonce,
        end_accumulated_fee,
    };
    diff.put_l1_validator(v).expect("seed validator");
}

/// A `SetL1ValidatorWeightTx` enforces a monotonic nonce (`nonce >= MinNonce`).
#[test]
fn l1_lifecycle_set_weight_rejects_stale_nonce() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([3; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    install_conversion(&mut diff, subnet);
    let validation_id = Id::from([0xCC; 32]);
    // MinNonce = 5; the message uses a stale nonce of 4.
    seed_validator(&mut diff, subnet, validation_id, 100, 5, 0);

    let payload = RegistryPayload::L1ValidatorWeight(L1ValidatorWeight {
        validation_id,
        nonce: 4,
        weight: 50,
    });
    let tx = SetL1ValidatorWeightTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        message: wrap_warp(&payload),
    };
    let err = run_err(&b, &mut diff, UnsignedTx::SetL1ValidatorWeight(tx));
    assert!(
        matches!(err, Error::WarpMessageContainsStaleNonce),
        "got {err:?}"
    );
}

/// A `SetL1ValidatorWeightTx` updating the weight bumps `MinNonce` to
/// `nonce + 1` and sets the new weight.
#[test]
fn l1_lifecycle_set_weight_updates_nonce_and_weight() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([3; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    install_conversion(&mut diff, subnet);
    let validation_id = Id::from([0xCC; 32]);
    seed_validator(&mut diff, subnet, validation_id, 100, 0, 0);

    let payload = RegistryPayload::L1ValidatorWeight(L1ValidatorWeight {
        validation_id,
        nonce: 3,
        weight: 250,
    });
    let tx = SetL1ValidatorWeightTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        message: wrap_warp(&payload),
    };
    run(&b, &mut diff, UnsignedTx::SetL1ValidatorWeight(tx)).expect("set weight");

    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.weight, 250);
    assert_eq!(v.min_nonce, 4);
}

/// A `SetL1ValidatorWeightTx` with weight 0 that would remove the only validator
/// of the subnet is rejected.
#[test]
fn l1_lifecycle_set_weight_rejects_removing_last_validator() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([3; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    install_conversion(&mut diff, subnet);
    let validation_id = Id::from([0xCC; 32]);
    // The only validator of the subnet (total weight == this validator's weight).
    seed_validator(&mut diff, subnet, validation_id, 100, 0, 0);

    let payload = RegistryPayload::L1ValidatorWeight(L1ValidatorWeight {
        validation_id,
        nonce: 1,
        weight: 0,
    });
    let tx = SetL1ValidatorWeightTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        message: wrap_warp(&payload),
    };
    let err = run_err(&b, &mut diff, UnsignedTx::SetL1ValidatorWeight(tx));
    assert!(matches!(err, Error::RemovingLastValidator), "got {err:?}");
}

/// An `IncreaseL1ValidatorBalanceTx` tops up an active validator's
/// `EndAccumulatedFee`.
#[test]
fn l1_lifecycle_increase_balance_tops_up() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([4; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    let validation_id = Id::from([0xCC; 32]);
    // Active validator with EndAccumulatedFee = 3 AVAX.
    seed_validator(&mut diff, subnet, validation_id, 100, 0, 3 * AVAX);

    let add = 4 * AVAX;
    let tx = IncreaseL1ValidatorBalanceTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        validation_id,
        balance: add,
    };
    run(&b, &mut diff, UnsignedTx::IncreaseL1ValidatorBalance(tx)).expect("increase");

    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.end_accumulated_fee, 3 * AVAX + add);
}

/// An `IncreaseL1ValidatorBalanceTx` reactivates an inactive validator (sets
/// `EndAccumulatedFee` from `accruedFees` then adds the balance).
#[test]
fn l1_lifecycle_increase_balance_reactivates() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([4; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    let validation_id = Id::from([0xCC; 32]);
    // Inactive validator (EndAccumulatedFee == 0); accrued fees are 0 here.
    seed_validator(&mut diff, subnet, validation_id, 100, 0, 0);

    let add = 2 * AVAX;
    let tx = IncreaseL1ValidatorBalanceTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        validation_id,
        balance: add,
    };
    run(&b, &mut diff, UnsignedTx::IncreaseL1ValidatorBalance(tx)).expect("increase");

    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.end_accumulated_fee, add);
    assert!(v.is_active());
}

/// A `DisableL1ValidatorTx` disables an active validator and refunds the
/// remaining balance to the `RemainingBalanceOwner`.
#[test]
fn l1_lifecycle_disable_refunds_remaining_balance() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([5; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    let validation_id = Id::from([0xCC; 32]);
    // Active validator with a 6-AVAX remaining balance; deactivation owner is
    // 0-of-0 so the empty disable-auth credential authorizes.
    let remaining = 6 * AVAX;
    seed_validator(&mut diff, subnet, validation_id, 100, 0, remaining);

    let tx = DisableL1ValidatorTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        validation_id,
        disable_auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
    };
    // One empty credential serves as the 0-of-0 disable authorization.
    let creds = vec![crate::txs::tx::Credential { sigs: vec![] }];
    let tx_id =
        run_creds(&b, &mut diff, UnsignedTx::DisableL1Validator(tx), creds).expect("disable");

    // The validator is now disabled (EndAccumulatedFee == 0).
    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.end_accumulated_fee, 0);
    assert!(!v.is_active());

    // A refund UTXO of `remaining` was produced to the remaining-balance owner,
    // keyed by `(tx_id, len(outs))`. The single change output occupies index 0,
    // so the refund is at index 1.
    let refund = Utxo {
        tx_id,
        output_index: 1,
        asset_id: Id::from(AVAX_ASSET),
        out: ComponentOutput::Transfer(TransferOutput::new(remaining, owners(0x44))),
    };
    let stored = diff
        .get_utxo(refund.input_id())
        .expect("refund utxo present");
    assert_eq!(stored, refund.marshal().expect("marshal refund"));
}

/// A `DisableL1ValidatorTx` against an already-disabled validator is a no-op
/// (no refund, EndAccumulatedFee stays 0).
#[test]
fn l1_lifecycle_disable_already_disabled_noop() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fund = Id::from([5; 32]);
    let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
    let b = backend(UpgradeSchedule::all_active(), true);

    let subnet = Id::from([0x33; 32]);
    let validation_id = Id::from([0xCC; 32]);
    // Already-disabled validator (EndAccumulatedFee == 0).
    seed_validator(&mut diff, subnet, validation_id, 100, 0, 0);

    let tx = DisableL1ValidatorTx {
        base: base_tx(&b, &diff, fund, 100 * AVAX),
        validation_id,
        disable_auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
    };
    let creds = vec![crate::txs::tx::Credential { sigs: vec![] }];
    run_creds(&b, &mut diff, UnsignedTx::DisableL1Validator(tx), creds).expect("disable");

    let v = diff.get_l1_validator(validation_id).expect("l1 validator");
    assert_eq!(v.end_accumulated_fee, 0);
}

/// The L1 executor rejects a non-L1 tx (e.g. a `BaseTx`) as wrong-type.
#[test]
fn l1_lifecycle_rejects_non_l1_tx() {
    let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut diff = diff_with_utxos(ts, &[]);
    let b = backend(UpgradeSchedule::all_active(), true);
    let err = run_err(
        &b,
        &mut diff,
        UnsignedTx::Base(crate::txs::BaseTx::default()),
    );
    assert!(matches!(err, Error::WrongTxType), "got {err:?}");
}

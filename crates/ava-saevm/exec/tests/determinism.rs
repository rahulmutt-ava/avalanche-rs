// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The headline SAE determinism gate (specs/11 §10 invariant 11):
//! `prop::sae_execution_determinism`.
//!
//! The streaming executor must settle to **identical state regardless of
//! pipeline scheduling**. This property test builds a random 2-transaction block
//! on a funded genesis, runs it through the production [`AvaEvmDriver`] over a
//! real `FirewoodStateProvider` (tempdir) under **two forced commit schedules**
//! ([`PipelineSchedule`]: commit-every-block vs deferred-wide-interval), and
//! asserts the settled outputs are byte-identical across schedules:
//!
//! - the committed **post-state root**,
//! - the **derived receipt root** (`derive_sha(receipts)`, specs/11 §10 inv 10),
//! - the final **gas-time** (compared via its `Time<u64>` encoding),
//! - the realised **base fee**, and
//! - the **executed-frontier height** (`executor.last_executed().height()`).
//!
//! The schedule changes only *when* the durable Firewood commit happens, never
//! the execution inputs — so the invariant must hold. The full A/E/S frontier
//! pointers live in `ava-saevm-core` (M7.17) and are re-asserted in the M7.25
//! invariants harness; here we assert the `StepOutput` fields + executed height.

#![allow(clippy::arithmetic_side_effects)] // readable reference arithmetic in tests.

use std::str::FromStr;
use std::sync::Arc;

use alloy_consensus::{SignableTransaction, TxLegacy};
use alloy_primitives::Signature;
use arc_swap::ArcSwapOption;
use ava_database::{DynDatabase, MemDb};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::AvaEvmConfig;
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, B256, BundleState, Bytes, EMPTY_ROOT_HASH, Header, RethBlock, SealedBlock,
    TransactionSigned, TxKind, U256, keccak256,
};
use ava_saevm_blocks::{Block, WorstCaseBounds};
use ava_saevm_db::Tracker;
use ava_saevm_exec::{AvaEvmDriver, NoopExecHooks};
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_testutil::schedule::PipelineSchedule;
use ava_vm::components::gas::Price;
use proptest::prelude::*;
use secp256k1::{Message, SECP256K1, SecretKey};

// ---------------------------------------------------------------------------
// Fixed funded test key + chain parameters
// ---------------------------------------------------------------------------

/// The chain id the txs are signed for (EIP-155); arbitrary but fixed.
const CHAIN_ID: u64 = 43_112;
/// The pinned base fee for every case (a static-priced gas clock holds the price
/// at this minimum, so the realised base fee is deterministic).
const BASE_FEE: u64 = 25;
/// The funded sender's genesis balance (ample for two transfers).
const FUNDED_BALANCE: u128 = 1_000_000_000_000_000_000;
/// Gas a plain value transfer costs.
const TRANSFER_GAS: u64 = 21_000;

/// The fixed funded secret key (32 bytes of 0x11). Its address is funded in
/// genesis so both signed txs recover to a funded sender.
fn funded_key() -> SecretKey {
    SecretKey::from_byte_array([0x11u8; 32]).expect("valid secp256k1 secret key")
}

/// The EVM address of `funded_key` (`keccak256(uncompressed_pubkey[1..])[12..]`).
fn funded_address() -> Address {
    let pk = funded_key().public_key(SECP256K1);
    let uncompressed = pk.serialize_uncompressed();
    let hash = keccak256(&uncompressed[1..]);
    Address::from_slice(&hash[12..])
}

/// Signs `tx` (a legacy tx with `chain_id` set) with `funded_key`, producing a
/// `TransactionSigned` whose sender recovers to [`funded_address`].
fn sign_legacy(tx: TxLegacy) -> TransactionSigned {
    let sig_hash = tx.signature_hash();
    let msg = Message::from_digest(sig_hash.0);
    let recoverable = SECP256K1.sign_ecdsa_recoverable(msg, &funded_key());
    let (recid, bytes) = recoverable.serialize_compact();
    let r = U256::from_be_slice(&bytes[..32]);
    let s = U256::from_be_slice(&bytes[32..]);
    // For the EVM, y-parity is the low bit of the recovery id (0 or 1).
    let y_parity = i32::from(recid) == 1;
    let sig = Signature::new(r, s, y_parity);
    TransactionSigned::Legacy(tx.into_signed(sig))
}

// ---------------------------------------------------------------------------
// Genesis materialization + block building (mirrors execute_step.rs test (4))
// ---------------------------------------------------------------------------

/// Opens a fresh `FirewoodStateProvider` over in-memory side stores; holds the
/// tempdir alive in the returned tuple.
fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
    (dir, provider)
}

/// Materializes a genesis alloc funding [`funded_address`], commits it, and
/// returns the genesis state root.
fn materialize_genesis(provider: &FirewoodStateProvider) -> B256 {
    let mut builder = BundleState::builder(0..=0);
    builder = builder.state_present_account_info(
        funded_address(),
        ava_evm_reth::AccountInfo {
            balance: U256::from(FUNDED_BALANCE),
            nonce: 0,
            ..Default::default()
        },
    );
    let root = provider
        .propose_from_bundle(&builder.build())
        .expect("propose genesis");
    provider.commit(root).expect("commit genesis");
    root
}

/// AP1..AP3 active from genesis (London-era / revm LONDON), later forks far in
/// the future — mirrors the `genesis_to_1` fixture's coreth schedule.
fn ap3_london_upgrades() -> NetworkUpgrades {
    const FAR_FUTURE: u64 = u64::MAX;
    NetworkUpgrades {
        apricot_phase_1: 0,
        apricot_phase_2: 0,
        apricot_phase_3: 0,
        apricot_phase_4: FAR_FUTURE,
        apricot_phase_5: FAR_FUTURE,
        apricot_phase_pre_6: FAR_FUTURE,
        apricot_phase_6: FAR_FUTURE,
        apricot_phase_post_6: FAR_FUTURE,
        banff: FAR_FUTURE,
        cortina: FAR_FUTURE,
        durango: FAR_FUTURE,
        etna: FAR_FUTURE,
        fortuna: FAR_FUTURE,
        granite: FAR_FUTURE,
    }
}

/// A genesis SAE block (synchronous) a child can be built against.
fn sae_genesis() -> Arc<Block> {
    let header = Header {
        parent_hash: B256::ZERO,
        number: 0,
        timestamp: 0,
        transactions_root: EMPTY_ROOT_HASH,
        ..Header::default()
    };
    let g = Arc::new(
        Block::new(SealedBlock::seal_slow(RethBlock::uncle(header)), None, None).expect("genesis"),
    );
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

/// A static-priced gas clock at `unix` whose `price()` equals `BASE_FEE` (static
/// pricing pins the excess at its minimum so `price() == min_price`).
fn static_clock(unix: u64) -> GasTime {
    GasTime::new(
        unix,
        1,
        ava_vm::components::gas::Price(0),
        GasPriceConfig::new(BASE_FEE, 87, true),
    )
}

/// A worst-case bound permitting any base fee (the bound check is exercised by
/// the execute-step tests; here we hold the inputs fixed across schedules).
fn permissive_bounds() -> WorstCaseBounds {
    WorstCaseBounds {
        max_base_fee: Price(u64::MAX),
        latest_end_time: GasTime::new(
            0,
            1,
            ava_vm::components::gas::Price(0),
            GasPriceConfig::default(),
        ),
        min_op_burner_balances: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// One random 2-tx program
// ---------------------------------------------------------------------------

/// A randomized but always-affordable 2-tx program from the funded sender.
#[derive(Clone, Debug)]
struct TwoTxProgram {
    nonce0: u64,
    value0: u128,
    gas_price0: u128,
    recipient0: u8,
    value1: u128,
    gas_price1: u128,
    recipient1: u8,
}

impl TwoTxProgram {
    /// Builds the two signed legacy txs in execution order (sequential nonces
    /// from the same sender, so both are valid and apply in order).
    fn signed_txs(&self) -> Vec<TransactionSigned> {
        let tx0 = TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce: self.nonce0,
            gas_price: self.gas_price0,
            gas_limit: TRANSFER_GAS,
            to: TxKind::Call(Address::repeat_byte(self.recipient0)),
            value: U256::from(self.value0),
            input: Bytes::default(),
        };
        let tx1 = TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce: self.nonce0 + 1,
            gas_price: self.gas_price1,
            gas_limit: TRANSFER_GAS,
            to: TxKind::Call(Address::repeat_byte(self.recipient1)),
            value: U256::from(self.value1),
            input: Bytes::default(),
        };
        vec![sign_legacy(tx0), sign_legacy(tx1)]
    }
}

/// A proptest strategy over affordable 2-tx programs. Values are small, gas
/// prices are `>= BASE_FEE` (legacy-tx London validity), recipients are distinct
/// non-sender addresses, and the starting nonce is 0 (the funded account's).
fn program_strategy() -> impl Strategy<Value = TwoTxProgram> {
    let min_gas_price = u128::from(BASE_FEE);
    (
        0u128..1_000_000u128,
        min_gas_price..(min_gas_price + 1_000),
        1u8..120u8,
        0u128..1_000_000u128,
        min_gas_price..(min_gas_price + 1_000),
        120u8..240u8,
    )
        .prop_map(
            |(value0, gas_price0, recipient0, value1, gas_price1, recipient1)| TwoTxProgram {
                nonce0: 0,
                value0,
                gas_price0,
                recipient0,
                value1,
                gas_price1,
                recipient1,
            },
        )
}

// ---------------------------------------------------------------------------
// The settled outputs we compare across schedules
// ---------------------------------------------------------------------------

/// The schedule-invariant settled outputs of executing one 2-tx block.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Settled {
    post_state_root: B256,
    receipt_root: B256,
    base_fee: u64,
    /// The final gas clock's `Time<u64>` instant, compared via its byte encoding
    /// (`Time` is not `PartialEq`).
    gas_time_encoded: [u8; 24],
    executed_height: Option<u64>,
}

/// Runs the 2-tx `program` on a freshly-funded genesis under `schedule`, and
/// returns the settled outputs. Each call is fully independent (its own tempdir,
/// provider, executor) so the only thing that varies between two runs of the
/// same program is the injected schedule.
fn run_under(schedule: PipelineSchedule, program: &TwoTxProgram) -> Settled {
    let (_dir, provider) = open_provider();
    let genesis_root = materialize_genesis(&provider);

    let g = sae_genesis();
    let header = Header {
        parent_hash: g.hash(),
        number: 1,
        timestamp: 10,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(BASE_FEE),
        beneficiary: Address::ZERO,
        ..Header::default()
    };
    let mut eth = RethBlock::uncle(header);
    eth.body.transactions = program.signed_txs();
    let block = Arc::new(
        Block::new(
            SealedBlock::seal_slow(eth),
            Some(Arc::clone(&g)),
            Some(Arc::clone(&g)),
        )
        .expect("block"),
    );

    let chain_spec = AvaChainSpec::from_parts(
        ap3_london_upgrades(),
        ava_evm_reth::Chain::from_id(CHAIN_ID),
        false,
    );
    let config = AvaEvmConfig::new(chain_spec);
    let driver = AvaEvmDriver::new(config, Arc::clone(&provider));

    let clock = static_clock(9);
    // The injected schedule controls ONLY the saedb commit cadence.
    let tracker = Tracker::new(Arc::clone(&provider), schedule.db_config());
    let last_executed_ptr = ArcSwapOption::from(Some(Arc::clone(&g)));

    let out = ava_saevm_exec::execute_step(
        &block,
        &g,
        &clock,
        genesis_root,
        &permissive_bounds(),
        &driver,
        &NoopExecHooks::default(),
        &tracker,
        &last_executed_ptr,
    )
    .expect("execute the 2-tx block");

    let receipt_root = block
        .execution_results()
        .expect("execution results recorded")
        .receipt_root;

    Settled {
        post_state_root: out.post_state_root,
        receipt_root,
        base_fee: out.base_fee.0,
        gas_time_encoded: out.gas_time.time().encode(),
        executed_height: last_executed_ptr.load_full().map(|b| b.height()),
    }
}

/// Sanity: the same program under the same `archival` schedule is reproducible
/// (the random program actually exercises two distinct signed txs that apply).
fn run_archival(program: &TwoTxProgram) -> Settled {
    run_under(PipelineSchedule::CommitEveryBlock, program)
}

// ---------------------------------------------------------------------------
// The property test
// ---------------------------------------------------------------------------

mod prop {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// specs/11 §10 invariant 11: the executor settles to identical state
        /// regardless of pipeline scheduling. Across 64 random 2-tx programs, the
        /// settled outputs are byte-identical under commit-every-block vs
        /// deferred-wide-interval commit schedules.
        #[test]
        fn sae_execution_determinism(program in program_strategy()) {
            let [tight, deferred] = PipelineSchedule::contrasting_pair();
            let settled_tight = run_under(tight, &program);
            let settled_deferred = run_under(deferred, &program);

            prop_assert_eq!(
                &settled_tight, &settled_deferred,
                "settled state must be identical across pipeline schedules (specs/11 §10 inv 11)"
            );

            // The executed-frontier height advanced to the (only) block, height 1.
            prop_assert_eq!(settled_tight.executed_height, Some(1));
            // The realised base fee is the pinned static price.
            prop_assert_eq!(settled_tight.base_fee, BASE_FEE);
            // A real state transition occurred (non-empty post-state root).
            prop_assert_ne!(settled_tight.post_state_root, B256::ZERO);

            // Re-running the same program under the same schedule reproduces it
            // (no per-run nondeterminism even before contrasting schedules).
            let again = run_archival(&program);
            prop_assert_eq!(&settled_tight, &again, "same schedule reproduces the same settled state");
        }
    }
}

// A compile-time witness that the funded address is stable (used by the genesis
// alloc and the recovered sender); not a proptest, just a cheap guard.
#[test]
fn funded_address_is_stable() {
    let a = funded_address();
    let b = funded_address();
    assert_eq!(a, b, "funded address derivation is deterministic");
    assert_eq!(
        a,
        Address::from_str("0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a").expect("addr"),
        "funded address is the well-known EVM address of the 0x11..11 secp256k1 key",
    );
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M7.35 — ACP-194 minimum-gas-consumption floor in the EVM gas-charge path.
//!
//! Go reference (`avalanchego` HEAD `0b0b57143c`):
//!   * `graft/coreth/params/hooks_libevm.go` — `RulesExtra.MinimumGasConsumption`
//!     returns `hook.MinimumGasConsumption(limit) = ceil(limit/Lambda)` gated on
//!     `IsHelicon`, else the libevm `NOOPHooks` zero.
//!   * libevm `core/state_transition.go` `refundGas` → `consumeMinimumGas`:
//!     after the EIP refund is applied, `gasRemaining` is clamped so that
//!     `gasUsed = limit - gasRemaining >= MinimumGasConsumption(limit)`.
//!
//! Rust seam: the `AvaHandler::refund` override (the revm analog of libevm's
//! `refundGas`) clamps the refund so `gas.used() >= ceil(limit/Lambda)` when
//! Helicon is active. Pre-Helicon it is a no-op (charge actual gas).
//!
//! A high-`gas_limit` / low-actual-usage tx (a 21_000-gas value transfer with a
//! 1_000_000 limit) must still be charged `ceil(1_000_000/2) = 500_000` once
//! Helicon is active — closing the queue-stuffing vector. Pre-Helicon it is
//! charged the actual 21_000.

use std::sync::Arc;

use ava_crypto::secp256k1::PrivateKey;
use ava_database::MemDb;
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, AvaExecCtx, NoopPreHook};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    Address, BundleState, Bytes, EvmSignature, Header, SignableTransaction, SignerRecoverable,
    State, StateProviderDatabase, TransactionSigned, TxKind, TxLegacy, U256,
};

const CHAIN_ID: u64 = 43_112;
const BASE_FEE: u128 = 25_000_000_000;
const FUNDED: u128 = 1_000_000_000_000_000_000;

/// The high tx gas limit (≫ the 21_000 a plain transfer actually consumes).
const TX_GAS_LIMIT: u64 = 1_000_000;
/// A plain value transfer's intrinsic gas (no calldata).
const TRANSFER_GAS: u64 = 21_000;
/// `ceil(TX_GAS_LIMIT / Lambda)` with `Lambda == 2` — the post-Helicon floor.
const HELICON_FLOOR: u64 = 500_000;

fn funded_key() -> PrivateKey {
    PrivateKey::from_bytes(&[0x11u8; 32]).expect("key")
}

fn funded_address() -> Address {
    Address::from(funded_key().public_key().eth_address())
}

fn sign_legacy(tx: TxLegacy) -> TransactionSigned {
    let sig_hash = tx.signature_hash();
    let rsv = funded_key().sign_hash(&sig_hash.0).expect("sign");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    TransactionSigned::Legacy(tx.into_signed(sig))
}

/// Builds an all-active-pre-Etna schedule with Helicon at `helicon` (unix secs).
/// Etna onward is parked in the far future so the tx runs in a London-era spec
/// (the same window `evm_factory_live_path` exercises).
fn upgrades_with_helicon(helicon: u64) -> NetworkUpgrades {
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
        helicon,
    }
}

/// Executes a single 21_000-gas value transfer with `gas_limit = TX_GAS_LIMIT`
/// at block timestamp 10, returning the receipt's `gas_used`.
fn gas_used_for_transfer(helicon: u64) -> u64 {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");
    let genesis_bundle = BundleState::builder(0..=0)
        .state_present_account_info(
            funded_address(),
            ava_evm_reth::AccountInfo {
                balance: U256::from(FUNDED),
                nonce: 0,
                ..Default::default()
            },
        )
        .build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");

    let chain_spec = AvaChainSpec::from_parts(
        upgrades_with_helicon(helicon),
        ava_evm_reth::Chain::from_id(CHAIN_ID),
        false,
    );
    let config = AvaEvmConfig::new(chain_spec);

    let coinbase = Address::from([0x44u8; 20]);
    let tx = sign_legacy(TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 0,
        gas_price: BASE_FEE,
        gas_limit: TX_GAS_LIMIT,
        to: TxKind::Call(Address::from([0x99u8; 20])),
        value: U256::from(1u64),
        input: Bytes::new(),
    });
    let txs: Vec<_> = [tx]
        .into_iter()
        .map(|t| t.try_into_recovered().expect("recover"))
        .collect();

    let header = Header {
        number: 1,
        timestamp: 10,
        gas_limit: 8_000_000,
        base_fee_per_gas: Some(BASE_FEE as u64),
        beneficiary: coinbase,
        extra_data: Bytes::new(),
        ..Default::default()
    };

    let view = provider
        .history_by_state_root(genesis_root)
        .expect("genesis view");
    let mut state: State<StateProviderDatabase<_>> = ava_evm_reth::StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();
    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch_with_ctx(env, &mut state, &NoopPreHook, &txs, &AvaExecCtx::default())
        .expect("execute_batch_with_ctx");

    let receipts = &outcome.result.receipts;
    assert_eq!(receipts.len(), 1, "single tx");
    assert!(receipts[0].success, "value transfer must succeed");
    receipts[0].cumulative_gas_used
}

/// Pre-Helicon (Helicon unscheduled): the floor is a no-op — a high-limit
/// transfer is charged exactly the intrinsic 21_000 gas it actually uses.
#[test]
fn min_gas_pre_helicon_is_noop() {
    let used = gas_used_for_transfer(u64::MAX);
    assert_eq!(
        used, TRANSFER_GAS,
        "pre-Helicon: charge actual gas (no ACP-194 floor)"
    );
}

/// Post-Helicon: the same tx is charged `ceil(gas_limit/Lambda) = 500_000`,
/// the ACP-194 minimum-gas-consumption floor that closes the queue-stuffing
/// vector. (Helicon active at timestamp 0 ≤ block timestamp 10.)
#[test]
fn min_gas_post_helicon_charges_floor() {
    let used = gas_used_for_transfer(0);
    assert_eq!(
        used, HELICON_FLOOR,
        "post-Helicon: charge ceil(gas_limit/Lambda) floor"
    );
}

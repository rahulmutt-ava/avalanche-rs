// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-reexecute` — the recorded-oracle / reexecute harness (specs/02 §10.5,
//! §11.1; specs/16 §5(3); specs/00 §11.7).
//!
//! Replays a recorded range of mainnet blocks through the Rust VMs from a fixed
//! starting state and asserts the resulting state/merkle roots match the
//! Go-recorded expected roots byte-for-byte. Because the expected roots come
//! from the Go node, this is a *differential test on recorded data* — the
//! cheapest per-PR oracle (§11.1 recorded-oracle mode).
//!
//! The C-Chain leg ([`replay_cchain`]) consumes a committed `blockexport`-style
//! fixture (Go-EXECUTED against coreth), materializes the genesis alloc into a
//! fresh Firewood-ethhash db, decodes the recorded block's EVM txs, drives
//! `ExternalConsensusExecutor::execute_batch`, converts the returned
//! `BundleState` into a Firewood proposal, and asserts both the genesis and the
//! post-state roots equal the Go-recorded values.
//!
//! The P/X leg ([`replay_xchain`]) has no Go-recorded `blockexport` fixture in the
//! repo yet, so — exactly as the C-Chain leg's `genesis_to_1` is a synthetic
//! fixture run through the real EVM pipeline — it builds a synthetic-but-real
//! case: a seed-derived chain of X-Chain `BaseTx` issuances driven through the
//! REAL `ava-avm` VM/block pipeline, capturing the deterministic post-state digest
//! and chain-tip id. The property proven is determinism / reproducibility (two
//! replays of the same case yield identical roots), not Go-oracle parity; see
//! `tests/PORTING.md` for the precise as-built / deferred boundary.

pub mod xchain;

use std::str::FromStr;
use std::sync::Arc;

use ava_database::{DynDatabase, MemDb};
use ava_evm::chainspec::{AvaChainSpec, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, NoopPreHook};
use ava_evm::state::FirewoodStateProvider;
use ava_evm_reth::{
    AccountInfo, Address, B256, BundleState, Bytes, Chain, Decodable2718,
    ExternalConsensusExecutor, Header, SignerRecoverable, State, StateBuilder,
    StateProviderDatabase, TransactionSigned, U256,
};

/// Harness errors. A reexecute *root mismatch* is surfaced via [`ReexecuteRoots`]
/// (the caller asserts), not as an `Err` — these variants cover the mechanical
/// failure modes of loading + replaying the fixture.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The fixture JSON failed to parse.
    #[error("parse fixture: {0}")]
    Parse(#[from] serde_json::Error),
    /// A hex-encoded field (address, tx envelope, …) failed to decode.
    #[error("decode hex: {0}")]
    Hex(String),
    /// Opening / proposing / committing through the Firewood backend failed.
    #[error("firewood: {0}")]
    Firewood(String),
    /// Decoding a recorded EIP-2718 tx envelope or recovering its sender failed.
    #[error("decode tx: {0}")]
    DecodeTx(String),
    /// Driving `execute_batch` over the recorded block failed.
    #[error("execute: {0}")]
    Execute(String),
    /// Driving the X-Chain (`ava-avm`) VM/block reexecute pipeline failed.
    #[error("xchain: {0}")]
    Xchain(String),
}

/// Result alias for the harness.
pub type Result<T> = std::result::Result<T, Error>;

/// One recorded C-Chain reexecute case: the genesis alloc + a single recorded
/// block (its EVM txs + header fields) + the Go-recorded expected roots.
///
/// Deserialized straight from a committed `blockexport`-style fixture (see
/// `tests/vectors/cchain/<name>/genesis_to_1.json`).
#[derive(Debug, serde::Deserialize)]
pub struct ReexecuteCase {
    /// EVM chain id of the recorded network.
    pub chain_id: u64,
    /// Genesis allocation (funded EOAs).
    pub alloc: Vec<AllocEntry>,
    /// Go-recorded genesis state root (4-field account RLP, Firewood-ethhash).
    pub genesis_state_root: String,
    /// EIP-2718 typed-envelope encodings of the recorded block's txs.
    pub block1_txs: Vec<String>,
    /// Recorded block timestamp.
    pub block1_timestamp: u64,
    /// Recorded block base fee (decimal string).
    pub block1_base_fee: String,
    /// Recorded block gas limit.
    pub block1_gas_limit: u64,
    /// Recorded block coinbase / beneficiary.
    pub block1_coinbase: String,
    /// Recorded block parent hash.
    pub block1_parent_hash: String,
    /// Recorded block number.
    pub block1_number: u64,
    /// Go-recorded post-state root after executing the recorded block.
    pub expected_post_state_root: String,
}

/// One funded genesis EOA.
#[derive(Debug, serde::Deserialize)]
pub struct AllocEntry {
    /// Account address (`0x`-prefixed hex).
    pub address: String,
    /// Account balance (decimal string, wei).
    pub balance: String,
}

/// The two roots a C-Chain reexecute produces — the genesis root the harness
/// computes after materializing the alloc, and the post-state root after
/// replaying the recorded block. The caller asserts both against the
/// Go-recorded values carried on the [`ReexecuteCase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReexecuteRoots {
    /// Computed genesis state root.
    pub genesis: B256,
    /// Computed post-block state root.
    pub post: B256,
}

impl ReexecuteCase {
    /// Parse a `ReexecuteCase` from a `blockexport`-style fixture's JSON bytes.
    pub fn from_json(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }

    /// The Go-recorded genesis root as a [`B256`].
    pub fn expected_genesis_root(&self) -> Result<B256> {
        b256(&self.genesis_state_root)
    }

    /// The Go-recorded post-state root as a [`B256`].
    pub fn expected_post_root(&self) -> Result<B256> {
        b256(&self.expected_post_state_root)
    }
}

fn b256(s: &str) -> Result<B256> {
    B256::from_str(s).map_err(|e| Error::Hex(format!("b256 {s}: {e}")))
}

/// Far-future activation height for forks that are inactive in the recorded
/// `genesis_to_1` fixture (coreth `TestApricotPhase3Config`: AP1..AP3 active
/// from genesis, AP4+ inactive ⇒ London-era / revm `LONDON`).
const FAR_FUTURE: u64 = u64::MAX;

/// Replay a recorded C-Chain reexecute case from a fixed genesis state and
/// return the computed genesis + post-state roots.
///
/// Ported from `crates/ava-evm/tests/cchain_state_root.rs` (M6.6): materialize
/// the genesis alloc into a fresh Firewood-ethhash db (propose → commit), decode
/// the recorded block's EIP-2718 txs + recover senders, drive
/// `execute_batch` over a `State<FirewoodStateView>` at genesis, convert the
/// returned `BundleState` into a Firewood proposal, and return both roots. The
/// caller asserts them against the Go-recorded values on the [`ReexecuteCase`].
pub fn replay_cchain(case: &ReexecuteCase) -> Result<ReexecuteRoots> {
    // --- Materialize the genesis alloc into a fresh Firewood-ethhash db. ---
    let dir = tempfile::tempdir().map_err(|e| Error::Firewood(format!("tempdir: {e}")))?;
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider = FirewoodStateProvider::open(dir.path(), bytecode, block_hashes)
        .map_err(|e| Error::Firewood(format!("open: {e}")))?;

    // Build a genesis bundle of funded EOAs and commit it through the provider's
    // propose → stash → commit lifecycle (the same path accept() uses).
    let mut builder = BundleState::builder(0..=0);
    for entry in &case.alloc {
        let addr = Address::from_str(&entry.address)
            .map_err(|e| Error::Hex(format!("alloc addr {}: {e}", entry.address)))?;
        let balance = U256::from_str_radix(&entry.balance, 10)
            .map_err(|e| Error::Hex(format!("alloc balance {}: {e}", entry.balance)))?;
        builder = builder.state_present_account_info(
            addr,
            AccountInfo {
                balance,
                nonce: 0,
                ..Default::default()
            },
        );
    }
    let genesis_bundle = builder.build();
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .map_err(|e| Error::Firewood(format!("propose genesis: {e}")))?;
    provider
        .commit(genesis_root)
        .map_err(|e| Error::Firewood(format!("commit genesis: {e}")))?;

    // --- Decode the recorded block's txs (EIP-2718) + recover senders. ---
    let mut txs = Vec::with_capacity(case.block1_txs.len());
    for hex_tx in &case.block1_txs {
        let bytes = hex::decode(hex_tx.trim_start_matches("0x"))
            .map_err(|e| Error::DecodeTx(format!("tx hex: {e}")))?;
        let signed = TransactionSigned::decode_2718(&mut bytes.as_slice())
            .map_err(|e| Error::DecodeTx(format!("decode 2718: {e}")))?;
        let recovered = signed
            .try_into_recovered()
            .map_err(|_| Error::DecodeTx("recover sender".to_string()))?;
        txs.push(recovered);
    }

    // The env header: built from the recorded block fields (Go-computed basefee /
    // gas_limit / timestamp / coinbase). On the reexecute path the env is taken
    // straight from the header.
    let base_fee_per_gas = case
        .block1_base_fee
        .parse()
        .map_err(|e| Error::Hex(format!("base fee {}: {e}", case.block1_base_fee)))?;
    let header = Header {
        parent_hash: b256(&case.block1_parent_hash)?,
        number: case.block1_number,
        timestamp: case.block1_timestamp,
        gas_limit: case.block1_gas_limit,
        base_fee_per_gas: Some(base_fee_per_gas),
        beneficiary: Address::from_str(&case.block1_coinbase)
            .map_err(|e| Error::Hex(format!("coinbase {}: {e}", case.block1_coinbase)))?,
        extra_data: Bytes::new(),
        ..Default::default()
    };

    // Mirror the fixture's coreth `TestApricotPhase3Config` schedule so
    // `EvmEnv::for_eth_block` resolves `SpecId::LONDON` at the block timestamp
    // (AP1..AP3 active at genesis, later forks far in the future).
    let upgrades = NetworkUpgrades {
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
        helicon: FAR_FUTURE,
    };
    let chain_spec = AvaChainSpec::from_parts(upgrades, Chain::from_id(case.chain_id), false);
    let config = AvaEvmConfig::new(chain_spec);

    // --- Drive execute_batch over a State<FirewoodStateView> at genesis. ---
    let view = provider
        .history_by_state_root(genesis_root)
        .map_err(|e| Error::Firewood(format!("genesis view: {e}")))?;
    let mut state: State<StateProviderDatabase<_>> = StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();

    let env = config.evm_env_for_header(&header);
    let outcome = config
        .execute_batch(env, &mut state, &NoopPreHook, &txs)
        .map_err(|e| Error::Execute(format!("execute_batch: {e}")))?;

    // --- Convert the bundle to a Firewood proposal → post-state root. ---
    let post_root = provider
        .propose_from_bundle(&outcome.bundle)
        .map_err(|e| Error::Firewood(format!("propose post-state: {e}")))?;

    Ok(ReexecuteRoots {
        genesis: genesis_root,
        post: post_root,
    })
}

pub use xchain::{XchainReexecuteRoots, replay_xchain};

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-evm-reth` — the **G0 facade** (spec 10 §17.1, 00 §11.1.6).
//!
//! This is the ONLY crate in the workspace allowed to name `reth_*` / `revm` /
//! `alloy_*` directly. It pins ONE reth revision (see `RETH_REV` /
//! `Cargo.toml`) and re-exports a *minimal, stable internal API surface* under
//! our own names. The rest of `ava-evm` (and, in M7, `ava-saevm-exec`) depend
//! ONLY on this crate, never on reth directly — so an upstream rename or
//! reshuffle is a one-line edit here (the blast radius is one crate).
//!
//! `#![forbid(unsafe_code)]` is intentionally **lifted only here**: this is the
//! audited binding-wrapper crate (00 §8). It contains no `unsafe` of its own;
//! the exemption exists because the re-exported reth/revm/alloy types do.
//!
//! See `UPGRADING.md` for the reth-bump checklist.

// --- reth execution layer (reth-evm) -------------------------------------
pub use reth_evm::execute::{
    BlockBuilder, BlockBuilderOutcome, BlockExecutionError, BlockExecutor, BlockExecutorFactory,
};
pub use reth_evm::{ConfigureEvm, EvmEnvFor, ExecutionCtxFor};
// `BlockExecutionResult` is re-exported privately inside `reth_evm::execute`;
// its canonical public home is `alloy-evm` (reth re-exports from there). Pin it
// here so callers name only `ava_evm_reth::BlockExecutionResult`.
pub use alloy_evm::block::BlockExecutionResult;

// --- reth storage/provider traits (reth-storage-api) ---------------------
pub use reth_storage_api::errors::provider::{ProviderError, ProviderResult};
// The low-level reth DB error wrapped by `ProviderError::Database` — used to
// surface Firewood/side-KV read failures across the facade (G1, §11.2).
pub use reth_storage_api::errors::db::DatabaseError;
pub use reth_storage_api::{
    AccountReader, BlockHashReader, BytecodeReader, HashedPostStateProvider, StateProofProvider,
    StateProvider, StateProviderFactory, StateRootProvider, StorageRootProvider,
};

// --- reth state/trie value types crossing the facade (G1, §17.2) ---------
// `Account` is the value `AccountReader::basic_account` returns (nonce,
// balance, bytecode_hash). The trie types are the inputs/outputs of the
// `StateRootProvider`/`HashedPostStateProvider` methods FirewoodStateProvider
// implements (we hand reth EMPTY `TrieUpdates` — the G1 trick).
pub use reth_primitives_traits::Account;
// The bytecode type the `BytecodeReader` trait returns — a reth newtype wrapper
// around `revm::state::Bytecode` (NOT the same type as `Bytecode` below, which
// is the revm one used on the execution/`AccountInfo` path). Exposed for
// `FirewoodStateView`'s `bytecode_by_hash` impl (G1, §17.2).
pub use reth_primitives_traits::Bytecode as RethBytecode;
pub use reth_trie_common::updates::TrieUpdates;
pub use reth_trie_common::{
    AccountProof, EMPTY_ROOT_HASH, ExecutionWitnessMode, HashedPostState, HashedStorage,
    KeccakKeyHasher, KeyHasher, MultiProof, MultiProofTargets, StorageMultiProof, StorageProof,
    TrieInput,
};
// The standard Ethereum account-leaf RLP `[nonce, balance, storage_root,
// code_hash]` (G1, §17.2.1) — the value FirewoodStateView reads/encodes at an
// ethhash account node. `TrieAccount` carries the RLP en/decode derives.
pub use alloy_consensus::TrieAccount;
// Account/code-hash sentinels and the keccak hasher used by `account_key` /
// `storage_key` / `hashed_post_state` (must match Firewood-ethhash derivation).
pub use alloy_consensus::constants::KECCAK_EMPTY;
pub use alloy_primitives::{StorageKey, StorageValue, keccak256};
// Minimal `alloy-rlp` surface for encoding/decoding account-leaf and slot RLP
// values crossing into Firewood (G1, §17.2.1).
pub use alloy_rlp::{Decodable as RlpDecodable, Encodable as RlpEncodable, encode as rlp_encode};

// --- reth chain spec + fork schedule (G7, §17.8) -------------------------
// `reth_chainspec` re-exports the whole `reth_ethereum_forks` hardfork set
// (EthereumHardfork / EthereumHardforks / ChainHardforks / ForkCondition /
// the `Hardfork` trait). `AvaChainSpec`/`AvaHardfork` are built on top.
pub use reth_chainspec::{
    Chain, ChainHardforks, ChainSpec, EthChainSpec, EthereumHardfork, EthereumHardforks,
    ForkCondition, Hardfork,
};

// --- revm fork/spec id (G7) ----------------------------------------------
// The Ethereum `SpecId` each Avalanche phase maps onto (revm_spec_id, §17.8).
pub use revm::primitives::hardfork::SpecId;

// --- revm (state overlay + precompile dispatch) --------------------------
pub use revm::database::{BundleState, State, StateBuilder};
pub use revm::handler::PrecompileProvider;
pub use revm::state::{Account as RevmAccount, AccountInfo, Bytecode};

// --- alloy primitives / consensus types crossing the facade boundary -----
pub use alloy_consensus::{Receipt, TxEnvelope};
pub use alloy_primitives::map::B256Map;
pub use alloy_primitives::{Address, B256, Bytes, U256};

/// The pinned reth git revision (G0 / R3, spec 10 §17.1). A single 40-char hex
/// commit SHA — never a version range. Bumping it is the one-line edit in the
/// `UPGRADING.md` checklist.
pub const RETH_REV: &str = "88505c7fcbfdebfd3b56d88c86b62e950043c6c4";

/// Recovered (sender-attached) transaction, the unit the executor consumes.
pub type RecoveredTx = alloy_consensus::transaction::Recovered<TxEnvelope>;

/// The execution environment handed to a single batch execution: block env +
/// chain/fork context. Kept opaque at the facade boundary; `ava-evm`'s
/// `AvaEvmConfig` populates it from `AvaNextBlockCtx` (spec 10 §7.2).
#[derive(Clone, Debug, Default)]
pub struct AvaEvmEnv {
    /// reth/revm block environment (basefee, gas limit, timestamp, …),
    /// fork-overridden by `ava-evm::feerules` before execution.
    pub evm_env: reth_evm::EvmEnv,
}

/// Receipts + the unflushed revm state delta produced by one batch. The caller
/// (`ava-evm`) turns `bundle` into a Firewood proposal (spec 10 §17.2);
/// decoupled from any block lifecycle so SAE can reuse it (§16).
pub struct ExecOutcome {
    /// Receipts, cumulative gas, and requests for the batch.
    pub result: BlockExecutionResult<Receipt>,
    /// revm state delta to convert into a Firewood proposal.
    pub bundle: BundleState,
}

/// A pre-execution hook run before the EVM tx loop — used by ava-evm to apply
/// atomic Import/Export `EVMStateTransfer` and the warp predicate pass
/// (spec 10 §17.4). Defined here so the executor trait can name it.
pub trait PreExecutionHook {
    /// Apply state effects to the journaled overlay before tx execution.
    /// `db` is the revm `State<_>` overlay the batch executes against.
    fn apply(&self, db: &mut dyn revm::Database<Error = ProviderError>) -> Result<(), AvaEvmError>;
}

/// The ONE entrypoint SAE (spec 11) and the sync `ChainVm` (spec 10 §3) both
/// call. This is the "external consensus executor" reth does not ship: execute
/// an ordered batch of txs (+ an atomic pre-hook) against a parent state view
/// and return receipts + a revm `BundleState` — with NO node, NO engine, NO
/// fork choice attached (spec 10 §17.1).
pub trait ExternalConsensusExecutor: Send + Sync {
    /// A reth `StateProvider`-backed `State<DB>` view (spec 10 §17.2).
    type State;

    /// Pure function of `(env, parent view, pre-hook, ordered txs)`. Used by
    /// BOTH the sync verify path (spec 10 §3.2) and SAE's streaming executor
    /// (spec 11 §6.1).
    fn execute_batch(
        &self,
        env: AvaEvmEnv,
        parent: &mut Self::State,
        pre_hook: &dyn PreExecutionHook,
        txs: &[RecoveredTx],
    ) -> Result<ExecOutcome, AvaEvmError>;
}

/// Errors crossing the facade boundary (spec 10 §11.2). `ava-evm::Error` wraps
/// this via `#[from]`. Kept small: reth/revm error types are re-spelled here so
/// callers never name a reth type directly.
#[derive(Debug, thiserror::Error)]
pub enum AvaEvmError {
    /// A reth block-execution error (invalid tx, state, gas, …).
    #[error("block execution: {0}")]
    BlockExecution(#[from] BlockExecutionError),

    /// A reth provider/state-read error.
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),

    /// Checked-arithmetic / fee overflow on the atomic or fee path.
    #[error("fee overflow")]
    FeeOverflow,
}

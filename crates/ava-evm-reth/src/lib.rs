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
pub use reth_evm::{
    ConfigureEvm, Evm, EvmEnv, EvmEnvFor, EvmFactory, ExecutionCtxFor, NextBlockEnvAttributes,
};
// `EthEvmConfig`/`EthEvmFactory`/`EthBlockAssembler`/`RethReceiptBuilder` are
// reth's ready-made Ethereum `ConfigureEvm` implementation; `AvaEvmConfig`
// reuses it (parameterised on `AvaChainSpec`) to DRIVE the bare reth
// `BlockExecutor` rather than re-deriving the whole `ConfigureEvm` trait by hand
// (spec 10 §17.1 — "reth as a library", G6). `EthBlockExecutionCtx` is the
// per-block context the Ethereum block executor consumes.
pub use alloy_evm::eth::{EthBlockExecutionCtx, EthEvmFactory};
// The `EthExecutorSpec` + `Hardforks` chain-spec super-traits `EthEvmConfig`'s
// `ConfigureEvm` bound requires (alloy-evm / reth-ethereum-forks). `ava-evm`
// implements them for `AvaChainSpec` so it can serve as the `EthEvmConfig`
// chain spec (spec 10 §17.1/§17.8).
pub use alloy_evm::eth::spec::EthExecutorSpec;
pub use reth_ethereum_forks::{ForkFilter, ForkFilterKey, ForkHash, ForkId, Hardforks, Head};
pub use reth_evm_ethereum::{EthBlockAssembler, EthEvmConfig, RethReceiptBuilder};
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
// Low-level `alloy-rlp` list primitives `ava-evm::block` needs to hand-roll the
// coreth C-Chain header/block RLP — the libevm header-extra layout (`ExtDataHash`
// + the AP3/AP4/EIP-4844/Granite optional tail) that alloy's stock `Header`
// decoder rejects (spec 10 §9.3 / §6.2). `RlpListHeader` is the RLP
// `{list, payload_length}` framing struct (decode/encode a list prefix);
// `RlpError` its decode error; `RLP_EMPTY_STRING_CODE` (=0x80) the empty-string
// byte; `rlp_length_of_length` sizes a length-prefix when hand-encoding the
// outer block list.
pub use alloy_rlp::{
    EMPTY_STRING_CODE as RLP_EMPTY_STRING_CODE, Error as RlpError, Header as RlpListHeader,
    length_of_length as rlp_length_of_length,
};

// --- reth chain spec + fork schedule (G7, §17.8) -------------------------
// `reth_chainspec` re-exports the whole `reth_ethereum_forks` hardfork set
// (EthereumHardfork / EthereumHardforks / ChainHardforks / ForkCondition /
// the `Hardfork` trait). `AvaChainSpec`/`AvaHardfork` are built on top.
pub use reth_chainspec::{
    Chain, ChainHardforks, ChainSpec, ChainSpecBuilder, DepositContract, EthChainSpec,
    EthereumHardfork, EthereumHardforks, ForkCondition, Hardfork,
};
// `BaseFeeParams` is the EIP-1559 base-fee tuple `EthChainSpec::base_fee_params_at_timestamp`
// returns; reth-chainspec re-exports it from `alloy_eips::eip1559`.
pub use reth_chainspec::BaseFeeParams;
// The remaining `EthChainSpec` return types reth-chainspec does NOT re-export
// publicly, so `AvaChainSpec` names them from their source crates (G7, §17.8):
// the genesis block spec, the blob-fee schedule, and the (empty for Avalanche)
// bootnode list.
pub use alloy_eips::eip7840::BlobParams;
pub use alloy_genesis::Genesis;
pub use reth_network_peers::NodeRecord;

// --- revm fork/spec id (G7) ----------------------------------------------
// The Ethereum `SpecId` each Avalanche phase maps onto (revm_spec_id, §17.8).
pub use revm::primitives::hardfork::SpecId;

// --- revm (state overlay + precompile dispatch) --------------------------
pub use revm::database::states::bundle_state::{BundleBuilder, BundleRetention};
pub use revm::database::{BundleState, State, StateBuilder};
pub use revm::handler::PrecompileProvider;
pub use revm::state::{Account as RevmAccount, AccountInfo, Bytecode};
// `Database` (revm's state-data trait) — the bound `State<DB>` requires and the
// type `PreExecutionHook::apply` operates on. `StateProviderDatabase<DB>` is
// reth's adapter turning a reth `StateProvider` (our `FirewoodStateView`) into a
// revm `Database<Error = ProviderError>` so the bare reth executor can run over
// Firewood-ethhash (spec 10 §17.1/§17.2).
pub use revm::Database;
// The error type a revm `State<DB>` overlay surfaces — `EvmDatabaseError<E>`
// wrapping the inner db error (here `ProviderError`). `PreExecutionHook` operates
// on the `State` overlay, so its `dyn Database` bound names this error.
pub use reth_revm::database::StateProviderDatabase;
pub use revm::database_interface::bal::EvmDatabaseError;

/// The error a revm `State<StateProviderDatabase<…>>` overlay surfaces: the
/// Firewood/provider read error (`ProviderError`) wrapped by revm's
/// `EvmDatabaseError`. This is the `Database::Error` of the overlay
/// `PreExecutionHook::apply` (and the block executor) operate on.
pub type StateDbError = EvmDatabaseError<ProviderError>;

// --- alloy primitives / consensus types crossing the facade boundary -----
pub use alloy_consensus::{Header, Receipt, TxEnvelope};
pub use alloy_primitives::map::B256Map;
pub use alloy_primitives::{Address, B256, Bytes, U256};
// The block + signed-tx types reth's Ethereum executor operates on
// (`EthPrimitives`): `RethBlock = alloy_consensus::Block<TransactionSigned>` and
// `TransactionSigned = EthereumTxEnvelope<TxEip4844>`. These are the wire types
// `ava-evm::block` decodes (alloy RLP) and the executor consumes — distinct from
// `TxEnvelope` (= `EthereumTxEnvelope<TxEip4844Variant>`), so they are named
// explicitly here (spec 10 §9.3 / §17.1).
pub use reth_ethereum_primitives::{
    Block as RethBlock, EthPrimitives, Receipt as EthReceipt, TransactionSigned,
};
// `Recovered<T>` (sender-attached tx) + the `SignerRecoverable` recovery trait
// used by `ava-evm::block` to recover senders before execution (spec 10 §9.3).
pub use alloy_consensus::transaction::{Recovered, SignerRecoverable};
// EIP-2718 typed-envelope decode for a single signed tx (`TransactionSigned`)
// — used by `ava-evm::block` to decode the txs out of a block body (spec 10
// §9.3); reused by the M6.6 reexecute test to decode the recorded tx.
pub use alloy_eips::Decodable2718;
// `SealedBlock`/`SealedHeader` are the sealed (hash-cached) wrappers reth's
// `ConfigureEvm::context_for_*` consumes; exposed for the block lifecycle.
pub use reth_primitives_traits::{SealedBlock, SealedHeader};

// --- revm precompile surface (G4/G10, §17.5) -----------------------------
// The `PrecompileProvider` trait itself is re-exported above (revm::handler).
// `ava-evm::precompile` implements it for `AvaPrecompiles`, overlaying the
// Avalanche stateful precompiles (warp/allowlist/feemanager/…) on revm's
// standard set and gating them by the fork+upgrade-activated `warm` set.
//
// `EthPrecompiles` is revm's standard Ethereum `PrecompileProvider` (the
// fall-through `base` set, keyed on the active `SpecId`). `CallInputs` is the
// per-call input the provider's `run` receives; `InterpreterResult` is the
// provider `Output` type. `ContextTr` is the revm execution-context trait the
// provider is generic over (the G10 churn point — the typed context extension
// `AvaCtxExt` rides on `ContextTr::Chain`, threaded by M6.22's predicate pass).
// `Cfg` is the context-config super-trait whose `Spec` (= `SpecId` for the Eth
// context) `set_spec` consumes. `Precompiles`/`PrecompileSpecId` build the
// standard set for a spec; `PrecompileError`/`PrecompileOutput` are the
// stateful-precompile result types; `precompile_output_to_interpreter_result`
// converts a precompile output into the `InterpreterResult` revm expects.
pub use revm::context_interface::{Cfg, ContextTr};
pub use revm::handler::{EthPrecompiles, precompile_output_to_interpreter_result};
pub use revm::interpreter::{CallInputs, InterpreterResult};
pub use revm::precompile::{PrecompileError, PrecompileOutput, PrecompileSpecId, Precompiles};
// revm's `SpecId` is already re-exported above (revm fork/spec id, G7).

/// The pinned reth git revision (G0 / R3, spec 10 §17.1). A single 40-char hex
/// commit SHA — never a version range. Bumping it is the one-line edit in the
/// `UPGRADING.md` checklist.
pub const RETH_REV: &str = "88505c7fcbfdebfd3b56d88c86b62e950043c6c4";

/// Recovered (sender-attached) transaction, the unit the executor consumes.
///
/// This is `Recovered<TransactionSigned>` — `TransactionSigned` (=
/// `EthereumTxEnvelope<TxEip4844>`) is reth's `EthPrimitives::SignedTx`, the
/// `BlockExecutor::Transaction` the Ethereum executor's `execute_transaction`
/// accepts. (NOT `Recovered<TxEnvelope>`: `TxEnvelope` is the *variant*-typed
/// `EthereumTxEnvelope<TxEip4844Variant>`, a distinct type — spec 10 §17.1.)
pub type RecoveredTx = Recovered<TransactionSigned>;

/// The execution environment handed to a single batch execution: block env +
/// chain/fork context + the block header providing the per-block execution
/// context (parent hash, extra data, withdrawals). `ava-evm`'s `AvaEvmConfig`
/// populates `evm_env` from `AvaNextBlockCtx` on the build path (spec 10 §7.2)
/// or from the decoded header on the reexecute/verify path (spec 10 §3.2).
#[derive(Clone, Debug, Default)]
pub struct AvaEvmEnv {
    /// reth/revm block environment (basefee, gas limit, timestamp, …),
    /// fork-overridden by `ava-evm::feerules` before execution.
    pub evm_env: reth_evm::EvmEnv,
    /// The block header this batch executes as. Supplies the
    /// `EthBlockExecutionCtx` fields (parent hash / beacon root / withdrawals /
    /// extra data) the reth block executor's pre/post-execution changes need.
    pub header: Header,
}

/// Receipts + the unflushed revm state delta produced by one batch. The caller
/// (`ava-evm`) turns `bundle` into a Firewood proposal (spec 10 §17.2);
/// decoupled from any block lifecycle so SAE can reuse it (§16).
pub struct ExecOutcome {
    /// Receipts, cumulative gas, and requests for the batch. `EthReceipt` is
    /// reth's `EthPrimitives::Receipt` (= `alloy_consensus::EthereumReceipt`),
    /// the type the Ethereum block executor produces.
    pub result: BlockExecutionResult<EthReceipt>,
    /// revm state delta to convert into a Firewood proposal.
    pub bundle: BundleState,
}

/// A pre-execution hook run before the EVM tx loop — used by ava-evm to apply
/// atomic Import/Export `EVMStateTransfer` and the warp predicate pass
/// (spec 10 §17.4). Defined here so the executor trait can name it.
pub trait PreExecutionHook {
    /// Apply state effects to the journaled overlay before tx execution.
    /// `db` is the revm `State<_>` overlay the batch executes against; its
    /// `Database::Error` is [`StateDbError`] (the Firewood `ProviderError`
    /// wrapped by revm's `EvmDatabaseError`).
    fn apply(&self, db: &mut dyn revm::Database<Error = StateDbError>) -> Result<(), AvaEvmError>;
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

    /// A network-upgrade fork that has already activated was rescheduled to a
    /// different timestamp (coreth `network_upgrades.go:checkCompatible` parity,
    /// spec 10 §7.4 / §17.8, G7).
    #[error("incompatible fork: {fork} has already activated and cannot be rescheduled")]
    IncompatibleFork {
        /// The stable name of the offending fork.
        fork: &'static str,
    },
}

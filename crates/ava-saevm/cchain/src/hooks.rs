// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain SAE hooks: the [`hook::PointsG`] implementation the C-Chain VM
//! composes (specs/11 §8).
//!
//! Faithful port of `vms/saevm/cchain/hooks.go`'s `hooks`/`builder` types. This
//! implements the SAE hook surface: deterministic header building, the
//! ACP-176-style gas config after a block, end-of-block mint/burn [`Op`]s for
//! atomic Import/Export of AVAX, and block rebuild for verification.
//!
//! # EVM-config reuse ("one revm, two drivers", specs/11 §8 reuse decision)
//!
//! EVM-internal execution (opcodes, precompiles, fee recipient) is owned by
//! `ava-evm`'s `ConfigureEvm` — these hooks own only the SAE
//! streaming/settlement concerns (headers, gas clock, atomic mint/burn,
//! rebuild). This task therefore needs only the SAE [`hook`] surface plus the
//! reth header/block types re-exported through [`ava_saevm_types`]; it does not
//! re-derive any EVM-execution behaviour.
//!
//! # Atomic-tx seam (deferred to M7.22)
//!
//! The real avalanchego linear-codec atomic Import/Export tx types, the fx, and
//! the txpool are M7.22. To avoid colliding with that work, this module defines
//! a minimal local [`AtomicOp`]/[`AtomicOpSource`] seam describing the *input*
//! the hook needs to produce mint/burn [`Op`]s. M7.22's `tx::Tx`/txpool will
//! later implement/feed this seam. See the `// TODO(M7.22)` markers.

use std::collections::{BTreeMap, BTreeSet};

use ava_evm_reth::{EthReceipt, Header, RethBlock, SealedBlock, TransactionSigned};
use ava_saevm_gastime::GasPriceConfig;
use ava_saevm_hook::op::{AccountDebit, Op};
use ava_saevm_hook::{BlockBuilder, Points, PointsG, Settled, StateRead, Transaction};
use ava_saevm_types::{Address, B256, Bytes, SealedHeader, U256};
use ava_vm::components::gas::Gas;

/// The blackhole address used as the C-Chain block coinbase.
///
/// Mirrors Go's `constants.BlackholeAddr` (`0x0100…0000`): the first byte is
/// `0x01`, the rest zero.
pub const BLACKHOLE_ADDR: Address = {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x01;
    Address::new(bytes)
};

/// The gas target returned by [`CChainHooks::gas_config_after`].
///
/// Mirrors the hard-coded `1_000_000` in Go's `hooks.GasConfigAfter`
/// (TODO upstream: extract from the header).
pub const GAS_CONFIG_AFTER_TARGET: Gas = Gas(1_000_000);

/// Errors returned by the C-Chain hooks.
///
/// Port of the error paths in Go's `cchain/hooks.go`. Atomic-tx parsing errors
/// land here once M7.22 wires the real tx codec.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A transaction was rejected by [`Points::can_execute_transaction`].
    ///
    /// Mirrors the libevm `RulesAllowlistHooks.CanExecuteTransaction` rejection
    /// path (Go returns `nil` today; the allowlist gate is wired here).
    #[error("transaction from {0:#x} not permitted")]
    NotPermitted(Address),
    /// An atomic op could not be converted to an [`Op`]. Mirrors Go's
    /// `tx.AsOp`/`parseBlockTxs` error wrapping (full impl M7.22).
    #[error("converting atomic op: {0}")]
    AtomicOp(String),
}

/// A pending atomic operation crossing into or out of the C-Chain — the
/// *input* the hook turns into a mint/burn [`Op`].
///
/// This is the seam M7.22's atomic Import/Export tx codec + txpool will produce
/// (one `AtomicOp` per `tx.Tx`). It carries exactly the fields
/// [`CChainHooks::end_of_block_ops`] needs to build the [`Op`]:
///
/// * an **import** credits a recipient (mint), consuming no on-chain balance;
/// * an **export** debits a sender (burn), authorised by a nonce + min-balance.
///
/// Port-of note: in Go this information is carried by `tx.Tx` and surfaced via
/// `tx.AsOp(avaxAssetID)`; here it is a flat enum so the hook need not depend on
/// the (not-yet-ported) tx codec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomicOp {
    /// Importing AVAX from another chain: mint `amount` to `to`.
    Import {
        /// The atomic op / tx ID (used as [`Op::id`]).
        id: ava_types::id::Id,
        /// The recipient credited the imported AVAX.
        to: Address,
        /// The amount of AVAX minted to `to`.
        amount: U256,
        /// Gas consumed by the op.
        gas: Gas,
        /// Maximum gas price the op will pay.
        gas_fee_cap: U256,
    },
    /// Exporting AVAX to another chain: burn `amount` from `from`.
    Export {
        /// The atomic op / tx ID (used as [`Op::id`]).
        id: ava_types::id::Id,
        /// The sender debited the exported AVAX.
        from: Address,
        /// The amount of AVAX burned from `from`.
        amount: U256,
        /// The minimum balance `from` must hold for the burn to be valid (MUST
        /// be at least `amount`).
        min_balance: U256,
        /// The nonce authorising the debit.
        nonce: u64,
        /// Gas consumed by the op.
        gas: Gas,
        /// Maximum gas price the op will pay.
        gas_fee_cap: U256,
    },
}

impl AtomicOp {
    /// Converts this atomic op to the SAE [`Op`] applied during block execution.
    ///
    /// Mirrors Go's `tx.Tx.AsOp`: an import becomes a single-entry `mint`; an
    /// export becomes a single-entry `burn`.
    #[must_use]
    pub fn as_op(&self) -> Op {
        match self {
            AtomicOp::Import {
                id,
                to,
                amount,
                gas,
                gas_fee_cap,
            } => Op {
                id: *id,
                gas: *gas,
                gas_fee_cap: *gas_fee_cap,
                burn: BTreeMap::new(),
                mint: BTreeMap::from([(*to, *amount)]),
            },
            AtomicOp::Export {
                id,
                from,
                amount,
                min_balance,
                nonce,
                gas,
                gas_fee_cap,
            } => Op {
                id: *id,
                gas: *gas,
                gas_fee_cap: *gas_fee_cap,
                burn: BTreeMap::from([(
                    *from,
                    AccountDebit {
                        nonce: *nonce,
                        amount: *amount,
                        min_balance: *min_balance,
                    },
                )]),
                mint: BTreeMap::new(),
            },
        }
    }
}

impl Transaction for AtomicOp {
    fn as_op(&self) -> Op {
        AtomicOp::as_op(self)
    }
}

/// A source of pending atomic ops for a block.
///
/// This is the seam M7.22 implements: given the block being executed (or built),
/// return the atomic Import/Export ops it carries (decoded from the block's
/// extData in Go's `parseBlockTxs`). The fake used in tests returns a fixed set.
pub trait AtomicOpSource {
    /// Returns the atomic ops carried by `block`, in inclusion order.
    ///
    /// Mirrors Go's `parseBlockTxs(b)` followed by `tx.AsOp` per tx.
    fn atomic_ops(&self, block: &SealedBlock<RethBlock>) -> Vec<AtomicOp>;
}

/// The C-Chain SAE hooks.
///
/// Port of Go's `cchain.hooks` (which embeds `builder`). Holds the atomic-op
/// source seam and an optional sender allowlist for
/// [`Points::can_execute_transaction`].
pub struct CChainHooks<S: AtomicOpSource> {
    source: S,
    /// Senders disallowed from executing transactions. Empty by default (Go's
    /// `CanExecuteTransaction` returns `nil` unconditionally today; the libevm
    /// `RulesAllowlistHooks` gate is modelled here as a deny-set).
    blocked_senders: BTreeSet<Address>,
}

impl<S: AtomicOpSource> CChainHooks<S> {
    /// Constructs hooks over the given atomic-op `source` with an empty
    /// sender allowlist.
    pub fn new(source: S) -> Self {
        Self {
            source,
            blocked_senders: BTreeSet::new(),
        }
    }

    /// Sets the set of senders that [`Points::can_execute_transaction`] rejects.
    #[must_use]
    pub fn with_blocked_senders(mut self, blocked: BTreeSet<Address>) -> Self {
        self.blocked_senders = blocked;
        self
    }

    /// Builds the deterministic C-Chain header for `parent` at the given
    /// `time_unix` (Unix seconds).
    ///
    /// Mirrors Go's `builder.BuildHeader`: parent-hash, blackhole coinbase,
    /// difficulty 1, number = parent + 1, and the supplied block time. Root,
    /// gas-limit, base-fee and gas-used are left at their defaults — the SAE
    /// execution path overwrites them (see [`BlockBuilder::build_header`] doc).
    fn header_for(parent: &SealedHeader, time_unix: u64) -> SealedHeader {
        let number = parent.number.saturating_add(1);
        let h = Header {
            parent_hash: parent.hash(),
            beneficiary: BLACKHOLE_ADDR,
            difficulty: U256::from(1u64),
            number,
            timestamp: time_unix,
            ..Header::default()
        };
        SealedHeader::seal_slow(h)
    }
}

impl<S: AtomicOpSource> Points for CChainHooks<S> {
    type Error = Error;
    type Block = SealedBlock<RethBlock>;
    type Receipts = Vec<EthReceipt>;
    // Rules are owned by the reth `ConfigureEvm` driver (specs/11 §8 reuse
    // decision); the SAE hooks do not consume libevm `params.Rules`, so the
    // seam type is unit here. TODO(M7.23): thread the concrete reth rules handle
    // if the VM lifecycle needs it.
    type Rules = ();
    // The height-indexed execution-results DB is opened by the M7.14/M7.23 VM
    // harness (Go's `blockdb.New`); unit seam here. TODO(M7.23).
    type ExecutionResultsDb = ();

    fn execution_results_db(&self, _data_dir: &str) -> Result<(), Error> {
        // TODO(M7.23): open the height-indexed execution-results DB
        // (Go's `blockdb.New(...).WithDir(dataDir)`).
        Ok(())
    }

    fn gas_config_after(&self, _header: &SealedHeader) -> (Gas, GasPriceConfig) {
        // Mirrors Go's `hooks.GasConfigAfter`: a fixed 1_000_000 gas target and
        // the default ACP-176 config (TargetToExcessScaling 87, MinPrice 1).
        // TODO(M7.23): extract the ACP-176 target excess from the header.
        (GAS_CONFIG_AFTER_TARGET, GasPriceConfig::default())
    }

    fn block_time(&self, header: &SealedHeader) -> (u64, u32) {
        // Mirrors Go's `hooks.BlockTime`: `time.Unix(header.Time, 0)`.
        // TODO(M7.23): extract sub-second milliseconds from the header.
        (header.timestamp, 0)
    }

    fn settled_by(&self, _header: &SealedHeader) -> Settled {
        // Mirrors Go's `hooks.SettledBy`: a zero-valued `hook.Settled`.
        // TODO(M7.23): extract the settled info from the header.
        Settled {
            height: 0,
            gas_unix: 0,
            gas_numerator: Gas(0),
            excess: Gas(0),
        }
    }

    fn end_of_block_ops(&self, block: &SealedBlock<RethBlock>) -> Result<Vec<Op>, Error> {
        // Mirrors Go's `hooks.EndOfBlockOps`: parse the block's atomic txs and
        // map each to its `Op`. Here the atomic ops come from the seam source.
        Ok(self
            .source
            .atomic_ops(block)
            .iter()
            .map(AtomicOp::as_op)
            .collect())
    }

    fn can_execute_transaction(
        &self,
        from: Address,
        _to: Option<Address>,
        _state: &dyn StateRead,
    ) -> Result<(), Error> {
        // Go's `CanExecuteTransaction` returns nil; the libevm
        // `RulesAllowlistHooks` gate is modelled as a deny-set here.
        if self.blocked_senders.contains(&from) {
            return Err(Error::NotPermitted(from));
        }
        Ok(())
    }

    fn before_executing_block(
        &self,
        _rules: &(),
        _state: &mut dyn ava_saevm_hook::op::StateMut,
        _block: &SealedBlock<RethBlock>,
    ) -> Result<(), Error> {
        // Mirrors Go's `hooks.BeforeExecutingBlock`: no-op.
        Ok(())
    }

    fn after_executing_block(
        &self,
        _state: &mut dyn ava_saevm_hook::op::StateMut,
        _block: &SealedBlock<RethBlock>,
        _receipts: Vec<EthReceipt>,
    ) -> Result<(), Error> {
        // Go's `AfterExecutingBlock` transfers non-AVAX assets and applies
        // cross-chain state via `state.Apply`. Both depend on the M7.22 atomic
        // tx codec + `cchain/state.State`, so this is a seam for now.
        // TODO(M7.22): transfer non-AVAX assets + apply cross-chain state.
        Ok(())
    }
}

impl<S: AtomicOpSource> BlockBuilder<AtomicOp> for CChainHooks<S> {
    type Error = Error;
    type Block = SealedBlock<RethBlock>;
    // The Snowman `block.Context` (warp predicate results) is consumed by the
    // M7.23 VM lifecycle; unit seam here. Go ignores it in `BuildBlock` today.
    type BlockContext = ();
    type EvmTransaction = TransactionSigned;
    type Receipt = EthReceipt;
    // `saetypes.BlockSource` (worst-case-queue lookup) is wired by the M7.23 VM
    // build path; unit seam here. TODO(M7.23).
    type BlockSource = ();

    fn build_header(&self, parent: &SealedHeader) -> Result<SealedHeader, Error> {
        // Build at the parent's timestamp + 1 so the result is deterministic
        // (Go uses `time.Now()`, but the rebuild path freezes the header time;
        // we make the build path deterministic too so build == rebuild without
        // a wall clock). TODO(M7.23): enforce the minimum block time / inject a
        // clock.
        let time_unix = parent.timestamp.saturating_add(1);
        Ok(Self::header_for(parent, time_unix))
    }

    fn potential_end_of_block_ops(
        &self,
        _header: &SealedHeader,
        _last_settled_block: B256,
        _source: &(),
    ) -> Vec<AtomicOp> {
        // Go's `PotentialEndOfBlockOps` filters txpool txs against the
        // ancestor-input conflict set + shared-memory credential checks. Those
        // depend on the M7.22 txpool/shared-memory; empty seam for now.
        // TODO(M7.22): return non-conflicting, credential-verified atomic ops.
        Vec::new()
    }

    fn build_block(
        &self,
        header: SealedHeader,
        _block_ctx: &(),
        _txs: &[TransactionSigned],
        _receipts: &[EthReceipt],
        _end_of_block_ops: &[AtomicOp],
        _settled: Settled,
    ) -> Result<SealedBlock<RethBlock>, Error> {
        // Go's `builder.BuildBlock` marshals the atomic txs into the block's
        // extData and commits its hash to the header (`NewBlockWithExtData`).
        // The extData codec is M7.22; here we build a header-only block so the
        // rebuild path stays byte-identical. The end_of_block_ops are recorded
        // by the caller; encoding them into extData is M7.22.
        // TODO(M7.22): marshal atomic txs into extData + commit ExtDataHash.
        let _ = Bytes::new();
        Ok(SealedBlock::seal_slow(RethBlock::uncle(
            header.into_header(),
        )))
    }
}

impl<S: AtomicOpSource> PointsG<AtomicOp> for CChainHooks<S> {
    type Rebuilder = Rebuilder;

    fn block_rebuilder_from(&self, block: &SealedBlock<RethBlock>) -> Result<Rebuilder, Error> {
        // Mirrors Go's `hooks.BlockRebuilderFrom`: freeze the block's time so
        // the rebuilt header is reconstructed identically.
        Ok(Rebuilder {
            frozen_time: block.header().timestamp,
        })
    }
}

/// A [`BlockBuilder`] that reconstructs a block built elsewhere, during
/// verification.
///
/// Port of the `builder` returned by Go's `hooks.BlockRebuilderFrom`, with the
/// block's time frozen so [`BlockBuilder::build_header`] is deterministic and
/// byte-identical to the originally-built header.
pub struct Rebuilder {
    /// The block time captured from the block being rebuilt (Unix seconds).
    frozen_time: u64,
}

impl BlockBuilder<AtomicOp> for Rebuilder {
    type Error = Error;
    type Block = SealedBlock<RethBlock>;
    type BlockContext = ();
    type EvmTransaction = TransactionSigned;
    type Receipt = EthReceipt;
    type BlockSource = ();

    fn build_header(&self, parent: &SealedHeader) -> Result<SealedHeader, Error> {
        Ok(CChainHooks::<NoopSource>::header_for(
            parent,
            self.frozen_time,
        ))
    }

    fn potential_end_of_block_ops(
        &self,
        _header: &SealedHeader,
        _last_settled_block: B256,
        _source: &(),
    ) -> Vec<AtomicOp> {
        Vec::new()
    }

    fn build_block(
        &self,
        header: SealedHeader,
        _block_ctx: &(),
        _txs: &[TransactionSigned],
        _receipts: &[EthReceipt],
        _end_of_block_ops: &[AtomicOp],
        _settled: Settled,
    ) -> Result<SealedBlock<RethBlock>, Error> {
        // TODO(M7.22): marshal atomic txs into extData + commit ExtDataHash.
        Ok(SealedBlock::seal_slow(RethBlock::uncle(
            header.into_header(),
        )))
    }
}

/// A zero-sized [`AtomicOpSource`] used only to name [`CChainHooks::header_for`]
/// from [`Rebuilder`] (which is `S`-agnostic). It is never invoked.
struct NoopSource;

impl AtomicOpSource for NoopSource {
    fn atomic_ops(&self, _block: &SealedBlock<RethBlock>) -> Vec<AtomicOp> {
        Vec::new()
    }
}

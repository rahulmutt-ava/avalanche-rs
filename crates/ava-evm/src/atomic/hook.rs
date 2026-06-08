// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AtomicStateHook` — the atomic `EVMStateTransfer` pre-execution hook (G3,
//! spec 10 §6.3/§17.4).
//!
//! revm/reth have **no** notion of crediting/debiting an account from *outside*
//! the EVM. coreth does this with `EVMStateTransfer(stateDB)`: an `ImportTx`
//! **credits** balance to its `Outs` addresses; an `ExportTx` **debits** its
//! `Ins` addresses and **bumps their nonces**. We implement it as a pre-execution
//! state mutation on the reth `BlockExecutor`'s journaled `State` overlay,
//! applied via the facade [`PreExecutionHook`] **before** any EVM transaction
//! runs — so the atomic effects land in the **same** `BundleState` → Firewood
//! proposal as the EVM effects, and the post-state root *includes* them, exactly
//! as coreth (spec 10 §6.3, §3.2/§5).
//!
//! ## Mechanics (how the hook reads/writes through revm)
//!
//! The hook receives a `&mut dyn StateDb` — the `State<StateProviderDatabase<…>>`
//! overlay exposed as both a `Database` (read) and a `DatabaseCommit`
//! (write). revm has no "increment balance" primitive, so per touched address we:
//!
//! 1. `db.basic(addr)?` — read the current [`AccountInfo`] (this also **loads the
//!    account into the overlay cache**, which `commit` requires).
//! 2. compute the new `balance`/`nonce` with **checked** arithmetic (overflow →
//!    [`Error::FeeOverflow`], 00 §6.1).
//! 3. build a [`RevmAccount`] carrying the updated `AccountInfo`, mark it
//!    `Touched` (the commit path skips untouched accounts), and `db.commit(...)`
//!    it — which folds the delta into the overlay's transition set.
//!
//! ## Atomic gas (spec 10 §7.3/§17.3, spec 21 §4b)
//!
//! The block gas budget must also charge the atomic txs in the batch.
//! [`AtomicStateHook::batch_gas`] / [`AtomicStateHook::batch_fee`] reuse
//! [`crate::feerules::atomic_gas`] / [`crate::feerules::atomic_fee`] (M6.13) — they
//! only wire the per-tx input/output/signature **counts**; they do **not**
//! re-derive the gas constants (those live in [`crate::atomic::tx`], M6.14).

use ava_evm_reth::{
    AccountInfo, AccountStatus, Address, AddressMap, AvaEvmError, EvmStorage, PreExecutionHook,
    RevmAccount, StateDb, U256,
};

use crate::atomic::tx::{AtomicTx, X2C_RATE};
use crate::error::Error;
use crate::feerules::{atomic_fee, atomic_gas};

/// The atomic `EVMStateTransfer` pre-execution hook (spec 10 §6.3/§17.4).
///
/// Carries the batch's atomic transactions (the `AtomicTx` unsigned bodies
/// attached to the block, §6.2); [`PreExecutionHook::apply`] applies their
/// EVMState transfer to the journaled overlay before the EVM tx loop.
///
/// > **Warp predicate pass (M6.22):** the predicate verification pass (warp BLS
/// > aggregate-signature verify over the EVM txs, §6.5/§17.5) also runs
/// > pre-execution. It is **not** implemented here — this hook covers only the
/// > atomic Import/Export state transfer. M6.22 adds the predicate pass (writing
/// > results onto the revm context `Chain` slot via `AvaCtxExt`); see the
/// > reserved no-op slot in [`AtomicStateHook::apply`].
#[derive(Clone, Debug, Default)]
pub struct AtomicStateHook {
    txs: Vec<AtomicTx>,
}

impl AtomicStateHook {
    /// Builds a hook over the given atomic transactions (owned).
    #[must_use]
    pub fn new(txs: Vec<AtomicTx>) -> Self {
        Self { txs }
    }

    /// The atomic transactions this hook applies.
    #[must_use]
    pub fn txs(&self) -> &[AtomicTx] {
        &self.txs
    }

    /// `tx.GasUsed` for one atomic tx (coreth `atomic/tx.go`, spec 10 §7.3) —
    /// wires the per-tx input/output/signature counts into the shared
    /// [`crate::feerules::atomic_gas`] accumulator (M6.13). `tx_len` is the length
    /// of the **signed** tx bytes; `num_signatures` the total credential
    /// signatures (the per-input `CostPerSignature` is folded into `EVMInputGas`,
    /// so the additional signature charge is the credential-signature total).
    ///
    /// # Errors
    /// Returns [`Error::FeeOverflow`] if the gas accumulation overflows `u64`.
    pub fn tx_gas(
        tx_len: u64,
        num_outputs: u64,
        num_inputs: u64,
        num_signatures: u64,
    ) -> Result<u64, Error> {
        atomic_gas(tx_len, num_outputs, num_inputs, num_signatures)
    }

    /// Total atomic gas charged against the block budget for this hook's batch
    /// (spec 10 §7.3/§17.3, spec 21 §4b). Sums [`AtomicStateHook::tx_gas`] over
    /// each tx, deriving each tx's counts from its Import `outs` / Export `ins` +
    /// the signed-byte length supplied alongside.
    ///
    /// `tx_lens` parallels `self.txs()` (the signed-byte length of each tx); a
    /// tx's signature count is taken as one per debited input (the minimum the
    /// `EVMInput` cost already folds in — extra credential signatures, if any, are
    /// charged by the caller that holds the signed `Tx`). All arithmetic checked.
    ///
    /// # Errors
    /// Returns [`Error::FeeOverflow`] on `u64` overflow of the accumulation.
    pub fn batch_gas(&self, tx_lens: &[u64]) -> Result<u64, Error> {
        let mut total: u64 = 0;
        for (tx, &tx_len) in self.txs.iter().zip(tx_lens.iter()) {
            let (num_outputs, num_inputs) = match tx {
                AtomicTx::Import(t) => {
                    let outs = u64::try_from(t.outs.len()).map_err(|_| Error::FeeOverflow)?;
                    (outs, 0u64)
                }
                AtomicTx::Export(t) => {
                    let ins = u64::try_from(t.ins.len()).map_err(|_| Error::FeeOverflow)?;
                    (0u64, ins)
                }
            };
            let gas = atomic_gas(tx_len, num_outputs, num_inputs, num_inputs)?;
            total = total.checked_add(gas).ok_or(Error::FeeOverflow)?;
        }
        Ok(total)
    }

    /// The AVAX fee the batch must pay at `base_fee` (coreth `dynamicFee`): the
    /// product of [`AtomicStateHook::batch_gas`] and the active base fee, checked
    /// (`ErrFeeOverflow` parity, spec 10 §17.3).
    ///
    /// # Errors
    /// Returns [`Error::FeeOverflow`] if `batch_gas * base_fee` overflows.
    pub fn batch_fee(&self, tx_lens: &[u64], base_fee: U256) -> Result<U256, Error> {
        atomic_fee(self.batch_gas(tx_lens)?, base_fee)
    }
}

/// Reads `addr`'s current [`AccountInfo`] from the overlay (loading it into the
/// cache, which the subsequent `commit` requires). A missing account reads as the
/// default (zero balance/nonce, empty code) — coreth's `state.AddBalance` on a
/// fresh address behaves identically.
fn read_info(db: &mut dyn StateDb, addr: Address) -> Result<AccountInfo, AvaEvmError> {
    Ok(db.basic(addr)?.unwrap_or_default())
}

/// Commits a single `(addr, info)` mutation into the overlay's transition set.
/// The account is marked `Touched` (commit skips untouched accounts) with empty
/// storage (atomic transfers never touch storage slots).
fn commit_info(db: &mut dyn StateDb, addr: Address, info: AccountInfo) {
    let account = RevmAccount {
        info,
        status: AccountStatus::Touched,
        storage: EvmStorage::default(),
        ..Default::default()
    };
    let mut changes: AddressMap<RevmAccount> = AddressMap::default();
    changes.insert(addr, account);
    db.commit(changes);
}

impl PreExecutionHook for AtomicStateHook {
    fn apply(&self, db: &mut dyn StateDb) -> Result<(), AvaEvmError> {
        // RESERVED (M6.22): the warp predicate verification pass runs here too,
        // before the atomic state transfer. Not implemented in M6.15 — this hook
        // covers only the atomic Import/Export EVMStateTransfer.

        for tx in &self.txs {
            match tx {
                // ImportTx: credit `amount * X2CRate` wei to each EVM output
                // address (coreth `import_tx.go::EVMStateTransfer`, AVAX asset).
                AtomicTx::Import(import) => {
                    for out in &import.outs {
                        let wei = wei_amount(out.amount)?;
                        let addr = Address::from(out.address);
                        let mut info = read_info(db, addr)?;
                        info.balance = info
                            .balance
                            .checked_add(wei)
                            .ok_or(AvaEvmError::FeeOverflow)?;
                        commit_info(db, addr, info);
                    }
                }
                // ExportTx: debit `amount * X2CRate` wei from each EVM input
                // address and bump its nonce to `max(cur, input.nonce + 1)`
                // (coreth `export_tx.go::EVMStateTransfer`). coreth additionally
                // requires `cur == input.nonce` and sufficient funds (rejected at
                // semantic verify, M6.17/M6.18); on a valid input
                // `max(cur, nonce+1) == nonce+1`, matching coreth's `SetNonce`.
                AtomicTx::Export(export) => {
                    for input in &export.ins {
                        let wei = wei_amount(input.amount)?;
                        let addr = Address::from(input.address);
                        let mut info = read_info(db, addr)?;
                        // Debit. Underflow (insufficient funds) is a verify-time
                        // rejection (coreth `ErrInsufficientFunds`, M6.17); saturate
                        // here so the pure state transfer never panics.
                        info.balance = info.balance.saturating_sub(wei);
                        // Nonce bump: `max(cur, input.nonce + 1)`.
                        let bumped = input.nonce.checked_add(1).ok_or(AvaEvmError::FeeOverflow)?;
                        info.nonce = info.nonce.max(bumped);
                        commit_info(db, addr, info);
                    }
                }
            }
        }
        Ok(())
    }
}

/// `amount (nAVAX) * X2CRate` → wei, checked (coreth multiplies a `u64` amount by
/// the `1e9` x2c rate in `uint256`; overflow → [`AvaEvmError::FeeOverflow`]).
fn wei_amount(amount: u64) -> Result<U256, AvaEvmError> {
    U256::from(amount)
        .checked_mul(U256::from(X2C_RATE))
        .ok_or(AvaEvmError::FeeOverflow)
}

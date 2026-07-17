// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-Chain EVM mempool (design doc 2026-07-17): a purpose-built pool
//! mirroring coreth's SUBMISSION-path admission rules
//! (`internal/ethapi/api.go` `SubmitTransaction` + `core/txpool/validation.go`
//! / `ValidateTransactionWithState`), **not** reth's pool. The
//! [`crate::atomic::mempool::AtomicMempool`] (atomic X<->C mempool) is the
//! structural precedent this module mirrors (struct + `Arc<Notify>` +
//! `thiserror` enum in one file).
//!
//! DIVERGENCE (documented, design §Non-goals): future-nonce (gapped) txs are
//! rejected, not queued — coreth's legacypool would hold them in `queued`.
//!
//! ## Admission order
//!
//! [`EvmMempool::add_local`] mirrors the Go call sequence: the stateless
//! `internal/ethapi/api.go` `SubmitTransaction` checks run first (already
//! known, EIP-155 protection, `checkTxFee`), then `core/txpool/validation.go`
//! `ValidateTransaction`/`ValidateTransactionWithState` (intrinsic gas,
//! init-code size, fee-cap-vs-tip-cap, tip floor, nonce, balance), then pool
//! capacity/replacement (coreth `legacypool`).
//!
//! ## Wake-on-nonempty
//!
//! [`EvmMempool::subscribe`] hands out a [`tokio::sync::Notify`]; every
//! admission calls `notify_one` (the [`crate::atomic::mempool::AtomicMempool::add_local`]
//! precedent), so a block-builder driver can park on `notified()` and wake
//! when the pool gains work.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ava_evm_reth::{Address, B256, ConsensusTx, RecoveredTx, TransactionSigned, U256};
use tokio::sync::Notify;

/// coreth `core/state_transition.go` `IntrinsicGas` constants
/// (`params/protocol_params.go`, verified against the vendored libevm
/// `v1.13.15-0.20260629092640-7d62036142ff` `params/protocol_params.go`).
const TX_GAS: u64 = 21_000;
/// `params.TxGasContractCreation`.
const TX_GAS_CONTRACT_CREATION: u64 = 53_000;
/// `params.TxDataZeroGas`.
const TX_DATA_ZERO_GAS: u64 = 4;
/// `params.TxDataNonZeroGasEIP2028` — Istanbul (always active >= AP0 here).
const TX_DATA_NON_ZERO_GAS_EIP2028: u64 = 16;
/// `params.TxAccessListAddressGas`.
const ACCESS_LIST_ADDRESS_GAS: u64 = 2_400;
/// `params.TxAccessListStorageKeyGas`.
const ACCESS_LIST_STORAGE_KEY_GAS: u64 = 1_900;
/// `params.InitCodeWordGas` — EIP-3860 (Shanghai == Durango).
const INIT_CODE_WORD_GAS: u64 = 2;
/// `params.MaxInitCodeSize` (`= 2 * MaxCodeSize`) — `core/txpool/validation.go`
/// max-init-code check.
const MAX_INIT_CODE_SIZE: usize = 49_152;

/// Why an EVM tx was not admitted to the mempool.
///
/// Mirrors coreth's sentinels from `internal/ethapi/api.go`,
/// `core/txpool/errors.go`, `core/txpool/legacypool/legacypool.go`, and
/// `core/error.go`; see each variant.
#[derive(Debug, thiserror::Error)]
pub enum EvmMempoolError {
    /// coreth `core/txpool/errors.go` `ErrAlreadyKnown`.
    #[error("already known")]
    AlreadyKnown,
    /// coreth `core/error.go` `ErrNonceTooLow`.
    #[error(
        "nonce too low: address {address}, tx nonce {tx_nonce} < account nonce {account_nonce}"
    )]
    NonceTooLow {
        /// The tx's sender.
        address: Address,
        /// The tx's declared nonce.
        tx_nonce: u64,
        /// The sender's current on-chain nonce.
        account_nonce: u64,
    },
    /// DIVERGENCE: coreth queues gapped txs; we reject (design §Non-goals).
    #[error("nonce gap: address {address}, tx nonce {tx_nonce} > next expected {expected}")]
    NonceGap {
        /// The tx's sender.
        address: Address,
        /// The tx's declared nonce.
        tx_nonce: u64,
        /// The next nonce this sender may submit (account nonce plus any
        /// contiguous run already pooled).
        expected: u64,
    },
    /// coreth `core/error.go` `ErrInsufficientFunds`.
    #[error("insufficient funds for gas * price + value: balance {balance}, cost {cost}")]
    InsufficientFunds {
        /// The sender's current balance.
        balance: U256,
        /// `value + max_fee_per_gas * gas_limit`.
        cost: U256,
    },
    /// coreth `core/error.go` `ErrIntrinsicGas`.
    #[error("intrinsic gas too low: gas {gas}, needed {needed}")]
    IntrinsicGasTooLow {
        /// The tx's declared gas limit.
        gas: u64,
        /// The computed intrinsic gas floor.
        needed: u64,
    },
    /// coreth `internal/ethapi/api.go` `SubmitTransaction` (~line 1804).
    #[error("only replay-protected (EIP-155) transactions allowed over RPC")]
    Unprotected,
    /// Chain-id mismatch against the node's configured chain (coreth recovers
    /// the signer via `types.Sender(signer, tx)` under a chain-id-bound
    /// `Signer`, which fails closed on a foreign chain id).
    #[error("invalid chain id for signer: have {have}, want {want}")]
    WrongChainId {
        /// The chain id encoded in the tx's signature.
        have: u64,
        /// The node's configured chain id.
        want: u64,
    },
    /// coreth `internal/ethapi/api.go` `checkTxFee` (~line 2171).
    #[error("tx fee ({fee} wei) exceeds the configured cap ({cap} wei)")]
    FeeCapExceeded {
        /// `max_fee_per_gas * gas_limit`.
        fee: U256,
        /// The configured fee cap.
        cap: U256,
    },
    /// coreth `core/error.go` `ErrTipAboveFeeCap` ("max priority fee per gas
    /// higher than max fee per gas"), enforced by
    /// `core/txpool/validation.go:114-117`. Vacuous for legacy txs (fee cap ==
    /// tip cap == gas price); kept for completeness / future dynamic-fee txs.
    #[error("max priority fee per gas higher than max fee per gas")]
    TipAboveFeeCap,
    /// coreth `core/txpool/errors.go` `ErrUnderpriced` (tip floor,
    /// `validation.go:132-135`).
    #[error("transaction underpriced: tip {tip} < minimum {min}")]
    Underpriced {
        /// The tx's effective tip (`max_priority_fee_per_gas` or, for a
        /// legacy tx, its gas price).
        tip: u128,
        /// The pool's configured tip floor.
        min: u128,
    },
    /// coreth `core/txpool/legacypool/legacypool.go:75-77` `ErrTxPoolOverflow`.
    #[error("txpool is full")]
    PoolFull,
    /// Same-nonce replacement without a strictly higher fee cap (coreth
    /// `legacypool` `ErrReplaceUnderpriced`, simplified to strict-greater —
    /// coreth's real price-bump percentage is not modeled here).
    #[error("replacement transaction underpriced")]
    ReplaceUnderpriced,
    /// coreth `core/txpool/validation.go` max-init-code-size (EIP-3860).
    #[error("max initcode size exceeded: {size} > {max}")]
    MaxInitCodeSize {
        /// The tx's init-code length.
        size: usize,
        /// `MAX_INIT_CODE_SIZE`.
        max: usize,
    },
}

/// The sender-side world state `add_local` checks a tx's nonce/balance
/// against (the caller looks this up from the current EVM state, coreth
/// `opts.State.Get{Nonce,Balance}`).
#[derive(Debug, Clone, Copy)]
pub struct SenderAccount {
    /// The sender's current on-chain nonce.
    pub nonce: u64,
    /// The sender's current on-chain balance.
    pub balance: U256,
}

/// Admission-time policy knobs (coreth `ValidationOptions`/`ethconfig`
/// defaults + the node's configured chain id).
#[derive(Debug, Clone)]
pub struct AdmissionRules {
    /// The node's configured chain id; a tx's EIP-155 chain id must match.
    pub chain_id: u64,
    /// The pool's minimum tip floor, in wei (coreth `legacypool`
    /// `PriceLimit`).
    pub min_tip_wei: u128,
    /// The RPC submission fee cap, in wei (coreth `ethconfig.RPCTxFeeCap`).
    pub tx_fee_cap_wei: U256,
    /// Whether Shanghai (== Durango) EIP-3860 init-code-size checking is
    /// active.
    pub shanghai: bool,
}

impl Default for AdmissionRules {
    /// coreth defaults: `min_tip_wei = 1` (`legacypool.DefaultConfig.PriceLimit`,
    /// `legacypool.go:173`), `tx_fee_cap_wei = 1` AVAX = 10^18 wei
    /// (`ethconfig.Defaults.RPCTxFeeCap = 1`, `eth/ethconfig/config.go:74`),
    /// `shanghai = true`. `chain_id` defaults to 43112, the local C-Chain id
    /// this crate's tests sign against (`prevrandao.rs`/`min_gas.rs`
    /// `CHAIN_ID`); production callers override it with the node's
    /// configured chain id.
    fn default() -> Self {
        Self {
            chain_id: 43_112,
            min_tip_wei: 1,
            tx_fee_cap_wei: U256::from(1_000_000_000_000_000_000u128),
            shanghai: true,
        }
    }
}

/// A pooled tx plus its admission order (used only to break fee-cap ties
/// deterministically during capacity eviction).
struct PoolEntry {
    tx: RecoveredTx,
    arrival: u64,
}

/// The C-Chain EVM mempool. Single-threaded by design (mirrors
/// [`crate::atomic::mempool::AtomicMempool`]): the VM holds it behind its own
/// lock, so [`Self::add_local`] takes `&mut self`.
pub struct EvmMempool {
    /// Capacity in txs.
    max_size: usize,
    /// Per-sender, nonce-ordered pooled txs.
    by_sender: HashMap<Address, BTreeMap<u64, PoolEntry>>,
    /// tx hash -> `(sender, nonce)` reverse index.
    by_hash: HashMap<B256, (Address, u64)>,
    /// Monotonic admission counter (eviction tie-break).
    arrival_seq: u64,
    /// Wakes a builder driver when the pool gains a tx.
    notify: Arc<Notify>,
}

impl EvmMempool {
    /// Builds an empty mempool with capacity `max_size` txs.
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            by_sender: HashMap::new(),
            by_hash: HashMap::new(),
            arrival_seq: 0,
            notify: Arc::new(Notify::new()),
        }
    }

    /// A [`Notify`] handle that fires (`notify_one`) whenever a tx is
    /// admitted. A builder driver parks on `notified()` to wake on new work
    /// (the [`crate::atomic::mempool::AtomicMempool::subscribe`] precedent).
    #[must_use]
    pub fn subscribe(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// The number of txs currently pooled.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// Whether the pool holds no txs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }

    /// Whether `hash` is pooled.
    #[must_use]
    pub fn contains(&self, hash: &B256) -> bool {
        self.by_hash.contains_key(hash)
    }

    /// The next nonce this sender may submit without gapping: the sender's
    /// on-chain `account_nonce` plus the length of any contiguous run of
    /// pooled nonces starting there.
    fn next_expected_nonce(&self, address: &Address, account_nonce: u64) -> u64 {
        let Some(map) = self.by_sender.get(address) else {
            return account_nonce;
        };
        let mut next = account_nonce;
        while map.contains_key(&next) {
            match next.checked_add(1) {
                Some(bumped) => next = bumped,
                None => break,
            }
        }
        next
    }

    /// The pool-wide lowest-fee-cap `(sender, nonce, fee_cap)`, ties broken by
    /// earliest arrival (oldest evicted first). `None` iff the pool is empty.
    fn peek_min(&self) -> Option<(Address, u64, u128)> {
        let mut best: Option<(Address, u64, u128, u64)> = None;
        for (address, pooled) in &self.by_sender {
            for (nonce, entry) in pooled {
                let fee = ConsensusTx::max_fee_per_gas(entry.tx.inner());
                let is_better = match best {
                    None => true,
                    Some((_, _, best_fee, best_arrival)) => {
                        fee < best_fee || (fee == best_fee && entry.arrival < best_arrival)
                    }
                };
                if is_better {
                    best = Some((*address, *nonce, fee, entry.arrival));
                }
            }
        }
        best.map(|(address, nonce, fee, _)| (address, nonce, fee))
    }

    /// Removes the pooled tx at `(address, nonce)` from both indexes, if
    /// present.
    fn remove_entry(&mut self, address: &Address, nonce: u64) {
        if let Some(pooled) = self.by_sender.get_mut(address) {
            if let Some(entry) = pooled.remove(&nonce) {
                self.by_hash.remove(entry.tx.hash());
            }
            if pooled.is_empty() {
                self.by_sender.remove(address);
            }
        }
    }

    /// Validates and admits a locally-submitted tx (coreth
    /// `internal/ethapi/api.go` `SubmitTransaction` + `core/txpool/validation.go`
    /// `ValidateTransaction`/`ValidateTransactionWithState`; see the module
    /// docs for the exact check order). Returns the tx's hash on admission.
    ///
    /// # Errors
    /// See [`EvmMempoolError`].
    pub fn add_local(
        &mut self,
        tx: RecoveredTx,
        sender: &SenderAccount,
        rules: &AdmissionRules,
    ) -> Result<B256, EvmMempoolError> {
        let hash = *tx.hash();

        // (1) Already known (coreth `core/txpool/errors.go` `ErrAlreadyKnown`).
        if self.by_hash.contains_key(&hash) {
            return Err(EvmMempoolError::AlreadyKnown);
        }

        let address = tx.signer();

        // (2) EIP-155 replay protection (coreth `internal/ethapi/api.go`
        // `SubmitTransaction`, ~line 1804): a legacy tx with no chain id is
        // unprotected; a protected tx must target this node's chain.
        match ConsensusTx::chain_id(tx.inner()) {
            None => return Err(EvmMempoolError::Unprotected),
            Some(have) if have != rules.chain_id => {
                return Err(EvmMempoolError::WrongChainId {
                    have,
                    want: rules.chain_id,
                });
            }
            Some(_) => {}
        }

        // (3) `checkTxFee` (coreth `internal/ethapi/api.go` ~line 2171):
        // fee = max_fee_per_gas * gas_limit vs the configured RPC fee cap.
        let max_fee = ConsensusTx::max_fee_per_gas(tx.inner());
        let gas_limit = ConsensusTx::gas_limit(tx.inner());
        let fee = U256::from(max_fee)
            .checked_mul(U256::from(gas_limit))
            .unwrap_or(U256::MAX);
        if fee > rules.tx_fee_cap_wei {
            return Err(EvmMempoolError::FeeCapExceeded {
                fee,
                cap: rules.tx_fee_cap_wei,
            });
        }

        // (4) Intrinsic gas floor (coreth `core/txpool/validation.go:123-131`
        // -> libevm `core/state_transition.go` `IntrinsicGas`).
        let needed = intrinsic_gas(tx.inner(), rules.shanghai);
        if gas_limit < needed {
            return Err(EvmMempoolError::IntrinsicGasTooLow {
                gas: gas_limit,
                needed,
            });
        }

        // (5) EIP-3860 max-init-code-size (coreth
        // `core/txpool/validation.go:89-92`).
        let is_create = ConsensusTx::kind(tx.inner()).is_create();
        let input_len = ConsensusTx::input(tx.inner()).len();
        if rules.shanghai && is_create && input_len > MAX_INIT_CODE_SIZE {
            return Err(EvmMempoolError::MaxInitCodeSize {
                size: input_len,
                max: MAX_INIT_CODE_SIZE,
            });
        }

        // (6) fee-cap >= tip-cap (coreth `core/txpool/validation.go:114-117`
        // `ErrTipAboveFeeCap`); vacuous for legacy txs.
        let tip = ConsensusTx::max_priority_fee_per_gas(tx.inner()).unwrap_or(max_fee);
        if max_fee < tip {
            return Err(EvmMempoolError::TipAboveFeeCap);
        }

        // (7) Tip floor (coreth `core/txpool/validation.go:132-135`
        // `ErrUnderpriced`).
        if tip < rules.min_tip_wei {
            return Err(EvmMempoolError::Underpriced {
                tip,
                min: rules.min_tip_wei,
            });
        }

        // (8) Nonce vs account state + already-pooled txs from this sender
        // (coreth `core/txpool/validation.go:236-246`
        // `ValidateTransactionWithState`).
        let tx_nonce = ConsensusTx::nonce(tx.inner());
        if tx_nonce < sender.nonce {
            return Err(EvmMempoolError::NonceTooLow {
                address,
                tx_nonce,
                account_nonce: sender.nonce,
            });
        }
        let expected = self.next_expected_nonce(&address, sender.nonce);
        let mut is_replacement = false;
        if tx_nonce > expected {
            return Err(EvmMempoolError::NonceGap {
                address,
                tx_nonce,
                expected,
            });
        }
        if tx_nonce < expected {
            // Within the sender's already-pooled contiguous run: a same-nonce
            // replacement, which requires a strictly higher fee cap (coreth
            // `legacypool` price-bump rule, simplified to strict-greater).
            let existing_fee = self
                .by_sender
                .get(&address)
                .and_then(|pooled| pooled.get(&tx_nonce))
                .map_or(0, |entry| ConsensusTx::max_fee_per_gas(entry.tx.inner()));
            if max_fee <= existing_fee {
                return Err(EvmMempoolError::ReplaceUnderpriced);
            }
            is_replacement = true;
        }

        // (9) Balance >= value + max_fee_per_gas * gas_limit (coreth
        // `core/txpool/validation.go:248-254` `ErrInsufficientFunds`).
        let value = ConsensusTx::value(tx.inner());
        let cost = fee.checked_add(value).unwrap_or(U256::MAX);
        if sender.balance < cost {
            return Err(EvmMempoolError::InsufficientFunds {
                balance: sender.balance,
                cost,
            });
        }

        // (10) Capacity: a same-nonce replacement never changes pool size, so
        // only a brand-new (sender, nonce) slot is subject to eviction
        // (coreth `core/txpool/legacypool/legacypool.go:75-77`
        // `ErrTxPoolOverflow`).
        if !is_replacement && self.len() >= self.max_size {
            let Some((min_address, min_nonce, min_fee)) = self.peek_min() else {
                return Err(EvmMempoolError::PoolFull);
            };
            if max_fee <= min_fee {
                return Err(EvmMempoolError::PoolFull);
            }
            self.remove_entry(&min_address, min_nonce);
        }

        // Admit: drop the old hash mapping on a replacement, then index the
        // new tx by both hash and (sender, nonce).
        if let Some(old_hash) = self
            .by_sender
            .get(&address)
            .and_then(|pooled| pooled.get(&tx_nonce))
            .map(|entry| *entry.tx.hash())
        {
            self.by_hash.remove(&old_hash);
        }
        self.arrival_seq = self.arrival_seq.saturating_add(1);
        self.by_sender.entry(address).or_default().insert(
            tx_nonce,
            PoolEntry {
                tx,
                arrival: self.arrival_seq,
            },
        );
        self.by_hash.insert(hash, (address, tx_nonce));

        // Signal a parked builder driver there is work (the `AtomicMempool`
        // `add_tx` precedent, `atomic/mempool.rs:342`).
        self.notify.notify_one();
        Ok(hash)
    }
}

/// coreth `core/state_transition.go` `IntrinsicGas` (via the vendored libevm
/// `core/state_transition.go:70-123`).
fn intrinsic_gas(tx: &TransactionSigned, shanghai: bool) -> u64 {
    let input = ConsensusTx::input(tx);
    let is_create = ConsensusTx::kind(tx).is_create();
    let mut gas = if is_create {
        TX_GAS_CONTRACT_CREATION
    } else {
        TX_GAS
    };
    let nonzero = input.iter().filter(|byte| **byte != 0).count() as u64;
    let zero = (input.len() as u64).saturating_sub(nonzero);
    gas = gas.saturating_add(nonzero.saturating_mul(TX_DATA_NON_ZERO_GAS_EIP2028));
    gas = gas.saturating_add(zero.saturating_mul(TX_DATA_ZERO_GAS));
    if is_create && shanghai {
        let words = (input.len() as u64).div_ceil(32);
        gas = gas.saturating_add(words.saturating_mul(INIT_CODE_WORD_GAS));
    }
    if let Some(access_list) = ConsensusTx::access_list(tx) {
        gas =
            gas.saturating_add((access_list.len() as u64).saturating_mul(ACCESS_LIST_ADDRESS_GAS));
        let keys: u64 = access_list
            .iter()
            .map(|item| item.storage_keys.len() as u64)
            .sum();
        gas = gas.saturating_add(keys.saturating_mul(ACCESS_LIST_STORAGE_KEY_GAS));
    }
    gas
}

#[cfg(test)]
mod tests {
    use ava_crypto::secp256k1::PrivateKey;
    use ava_evm_reth::{
        Address, Bytes, EvmSignature, RecoveredTx, SignableTransaction, SignerRecoverable,
        TransactionSigned, TxKind, TxLegacy, U256,
    };

    use super::{AdmissionRules, EvmMempool, SenderAccount};

    /// The local C-Chain id this test module signs against, matching
    /// `AdmissionRules::default()`'s `chain_id` and the constant used by
    /// `prevrandao.rs`/`min_gas.rs` (`CHAIN_ID = 43_112`).
    const CHAIN_ID: u64 = 43_112;

    fn key(byte: u8) -> PrivateKey {
        PrivateKey::from_bytes(&[byte; 32]).expect("PrivateKey::from_bytes")
    }

    /// Signs `tx` with sender key `byte` (test-local repeat of the
    /// `prevrandao.rs`/`min_gas.rs` `sign_legacy` helper — test-file
    /// convention is repeat-don't-import).
    fn sign_legacy(byte: u8, tx: TxLegacy) -> TransactionSigned {
        let sig_hash = tx.signature_hash();
        let rsv = key(byte).sign_hash(&sig_hash.0).expect("sign_hash");
        let r = U256::from_be_slice(&rsv[..32]);
        let s = U256::from_be_slice(&rsv[32..64]);
        let sig = EvmSignature::new(r, s, rsv[64] == 1);
        TransactionSigned::Legacy(tx.into_signed(sig))
    }

    fn recipient() -> Address {
        Address::repeat_byte(0xEE)
    }

    /// A protected (EIP-155, `CHAIN_ID`) legacy tx signed by sender key
    /// `byte`.
    fn signed_legacy_tx_from(
        byte: u8,
        nonce: u64,
        gas_price: u128,
        gas: u64,
        value: u128,
    ) -> RecoveredTx {
        let tx = TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce,
            gas_price,
            gas_limit: gas,
            to: TxKind::Call(recipient()),
            value: U256::from(value),
            input: Bytes::new(),
        };
        sign_legacy(byte, tx)
            .try_into_recovered()
            .expect("try_into_recovered")
    }

    /// A protected legacy tx signed by the default sender key (`0x11`).
    fn signed_legacy_tx(nonce: u64, gas_price: u128, gas: u64, value: u128) -> RecoveredTx {
        signed_legacy_tx_from(0x11, nonce, gas_price, gas, value)
    }

    /// An UNPROTECTED (pre-EIP-155, no chain id) legacy tx — `v` carries no
    /// chain id, so `ConsensusTx::chain_id` reads `None`.
    fn signed_legacy_tx_unprotected(
        nonce: u64,
        gas_price: u128,
        gas: u64,
        value: u128,
    ) -> RecoveredTx {
        let tx = TxLegacy {
            chain_id: None,
            nonce,
            gas_price,
            gas_limit: gas,
            to: TxKind::Call(recipient()),
            value: U256::from(value),
            input: Bytes::new(),
        };
        sign_legacy(0x11, tx)
            .try_into_recovered()
            .expect("try_into_recovered")
    }

    /// A legacy tx protected for a DIFFERENT chain id than the node's.
    fn signed_legacy_tx_for_chain(
        chain_id: u64,
        nonce: u64,
        gas_price: u128,
        gas: u64,
        value: u128,
    ) -> RecoveredTx {
        let tx = TxLegacy {
            chain_id: Some(chain_id),
            nonce,
            gas_price,
            gas_limit: gas,
            to: TxKind::Call(recipient()),
            value: U256::from(value),
            input: Bytes::new(),
        };
        sign_legacy(0x11, tx)
            .try_into_recovered()
            .expect("try_into_recovered")
    }

    #[test]
    fn admits_a_valid_legacy_tx() {
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let hash = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .expect("admit");
        assert!(
            pool.contains(&hash),
            "EvmMempool::add_local admits + indexes by hash"
        );
        assert_eq!(pool.len(), 1, "EvmMempool::len()");
    }

    #[test]
    fn rejects_nonce_too_low() {
        // coreth core/txpool/validation.go:239 (ErrNonceTooLow, "nonce too low")
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 5,
            balance: U256::from(10u128.pow(18)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("nonce too low"), "got: {err}");
    }

    #[test]
    fn rejects_nonce_gap_documented_divergence() {
        // coreth QUEUES future-nonce txs (legacypool queued set); this pool
        // rejects them — documented divergence, design doc §Non-goals.
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(2, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("nonce gap"), "got: {err}");
    }

    #[test]
    fn rejects_insufficient_funds() {
        // coreth core/txpool/validation.go:250-254 ("insufficient funds")
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(1000u64), // << cost
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("insufficient funds"), "got: {err}");
    }

    #[test]
    fn rejects_intrinsic_gas_too_low() {
        // coreth core/txpool/validation.go:125-130 -> core.IntrinsicGas ("intrinsic gas too low")
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(0, 2_000_000_000, 20_999, 1); // < 21000 base
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("intrinsic gas too low"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_unprotected_tx() {
        // coreth internal/ethapi/api.go:1804-1807 ("only replay-protected
        // (EIP-155) transactions allowed over RPC") — default allow-unprotected = false.
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx_unprotected(0, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("replay-protected"), "got: {err}");
    }

    #[test]
    fn rejects_wrong_chain_id() {
        // Signature recovery + chain id agreement: a tx for chain 9999 vs rules.chain_id.
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx_for_chain(9999, 0, 2_000_000_000, 21_000, 1);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("chain"), "got: {err}");
    }

    #[test]
    fn rejects_fee_over_configured_cap() {
        // coreth internal/ethapi/api.go:1801 checkTxFee -> "exceeds the configured cap"
        // gas_price * gas > 1 AVAX.
        let mut pool = EvmMempool::new(16);
        let tx = signed_legacy_tx(0, 100_000_000_000_000, 21_000, 1); // 2.1 AVAX fee
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(19)),
        };
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("exceeds the configured cap"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_already_known() {
        // coreth core/txpool/errors.go ErrAlreadyKnown ("already known")
        let mut pool = EvmMempool::new(16);
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(18)),
        };
        let tx = signed_legacy_tx(0, 2_000_000_000, 21_000, 1);
        pool.add_local(tx.clone(), &sender, &AdmissionRules::default())
            .expect("first");
        let err = pool
            .add_local(tx, &sender, &AdmissionRules::default())
            .unwrap_err();
        assert!(err.to_string().contains("already known"), "got: {err}");
    }

    #[test]
    fn same_nonce_replacement_requires_higher_fee_and_full_pool_evicts_cheapest() {
        // Replacement: same sender+nonce needs a strictly higher fee cap
        // (coreth legacypool price-bump rule, simplified to strict-greater);
        // capacity: at max_size, admitting a better-paying tx evicts the
        // lowest-fee-cap tx, a worse one gets "txpool is full"
        // (coreth core/txpool/errors.go ErrTxPoolOverflow "txpool is full").
        let mut pool = EvmMempool::new(2);
        let rules = AdmissionRules::default();
        let sender = SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(19)),
        };

        // Two distinct senders fill the pool's two slots.
        let tx_a = signed_legacy_tx_from(0x11, 0, 2_000_000_000, 21_000, 1);
        let hash_a = pool.add_local(tx_a, &sender, &rules).expect("admit a");
        let tx_b = signed_legacy_tx_from(0x22, 0, 3_000_000_000, 21_000, 1);
        let hash_b = pool.add_local(tx_b, &sender, &rules).expect("admit b");
        assert_eq!(pool.len(), 2, "pool full at max_size");

        // Same sender + nonce, but NOT a strictly higher fee cap -> rejected;
        // the incumbent A stays pooled.
        let tx_a_same_fee = signed_legacy_tx_from(0x11, 0, 2_000_000_000, 21_000, 2);
        let err = pool.add_local(tx_a_same_fee, &sender, &rules).unwrap_err();
        assert!(
            err.to_string()
                .contains("replacement transaction underpriced"),
            "got: {err}"
        );
        assert!(
            pool.contains(&hash_a),
            "non-outbidding replacement must not evict A"
        );
        assert_eq!(pool.len(), 2);

        // Same sender + nonce, strictly higher fee cap -> replaces A in place
        // (pool size unchanged, old hash gone, new hash present).
        let tx_a_replace = signed_legacy_tx_from(0x11, 0, 4_000_000_000, 21_000, 3);
        let hash_a2 = pool
            .add_local(tx_a_replace, &sender, &rules)
            .expect("replace a");
        assert!(
            !pool.contains(&hash_a),
            "old A hash must be gone after replacement"
        );
        assert!(pool.contains(&hash_a2));
        assert_eq!(pool.len(), 2, "replacement does not grow the pool");

        // Capacity: pool is full (A@4gwei, B@3gwei). A NEW sender+nonce
        // paying less than the pool-wide cheapest (B@3gwei) is rejected.
        let tx_cheap = signed_legacy_tx_from(0x33, 0, 1_000_000_000, 21_000, 1);
        let err = pool.add_local(tx_cheap, &sender, &rules).unwrap_err();
        assert!(err.to_string().contains("txpool is full"), "got: {err}");
        assert_eq!(pool.len(), 2, "rejected newcomer must not be admitted");

        // A NEW sender+nonce paying more than the pool-wide cheapest (B)
        // evicts B and is admitted.
        let tx_rich = signed_legacy_tx_from(0x44, 0, 5_000_000_000, 21_000, 1);
        let hash_rich = pool.add_local(tx_rich, &sender, &rules).expect("evicts B");
        assert!(pool.contains(&hash_rich));
        assert!(
            !pool.contains(&hash_b),
            "B must be evicted (pool-wide cheapest)"
        );
        assert_eq!(pool.len(), 2, "eviction keeps the pool at max_size");
    }
}

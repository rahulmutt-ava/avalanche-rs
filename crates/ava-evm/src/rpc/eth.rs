// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `eth_*` JSON-RPC handlers over Firewood + `feerules`/accepted-tag overrides
//! (G8, spec 10 §9.1/§17.9, M6.23).
//!
//! # Scoping deviation from §9.1/§17.9 (folded into the spec by the M6.23 report)
//!
//! The spec sketch instantiates reth's `EthApi<Provider, Pool, …>` and overrides
//! its fee/tag helper traits. We do **not** pull in `reth-rpc` /
//! `reth-rpc-eth-api` / `jsonrpsee`: the spec itself flags `EthApi`'s generic
//! instantiation over a *third-party* provider as the medium-risk part
//! ("reth keeps refactoring it", §17.9), and the avm/platformvm precedent in this
//! repo implements RPC handlers directly. The jsonrpsee-vs-axum mount decision is
//! deferred to the 12-node milestone anyway (§9.2).
//!
//! So [`EthRpc`] is a plain handler struct over:
//! - [`FirewoodStateProvider`] — read-only account/storage/code/proof reads
//!   (spec 10 §5/§17.2). We never mutate it here.
//! - [`CanonicalStore`] — the accepted-block tip; `latest`/`safe`/`finalized` all
//!   map to the last-accepted height (Snowman has no pending/unsafe head —
//!   coreth `rpc_accepted`).
//! - [`feerules`](crate::feerules) — `eth_gasPrice` / `eth_feeHistory` /
//!   `eth_maxPriorityFeePerGas` (spec 10 §7).
//! - [`AvaEvmConfig`] — the facade revm executor for `eth_call`/`eth_estimateGas`
//!   (a single read-only `Evm::transact` against a Firewood-backed db).
//!
//! Each method returns a [`serde_json::Value`] encoded with the Ethereum
//! JSON-RPC conventions coreth's `eth/` server emits: quantities are minimal
//! `0x`-hex (`0x0` for zero), data/hashes are full-width `0x`-hex.
//!
//! ## `eth_getProof` status (M6.23)
//!
//! The account fields (`balance`/`nonce`/`codeHash`/`storageHash`) come from
//! direct Firewood reads and are correct **today**. The merkle-proof array
//! (`accountProof`/`storageProof[].proof`) depends on Firewood range/inclusion
//! proofs owned by M6.25 ([`StateProofProvider::proof`] still returns
//! `unsupported`); until that lands we return an **empty** proof array. See the
//! golden vector `_provenance.md` and the M6.23 report.
//!
//! ## `debug_traceTransaction` status (M6.23)
//!
//! Deferred. The prestate tracer needs a revm inspector that is not reachable
//! behind the facade without a heavy dep; the handler returns a documented
//! [`Error`] until a follow-up wires it.

use std::sync::Arc;

use ava_evm_reth::{
    Address, B256, Bytes, ConfigureEvm, Decodable2718, EMPTY_ROOT_HASH, Evm, ExecutionResult,
    KECCAK_EMPTY, Output, ProviderError, SignerRecoverable, StateProviderDatabase,
    TransactionSigned, TxEnv, TxKind, U256, logs_bloom,
};
use parking_lot::Mutex;
use ruint::aliases::U256 as RuintU256;
use serde_json::{Value, json};

use crate::canonical::CanonicalStore;
use crate::error::{Error, Result};
use crate::evmconfig::{AvaEvmConfig, AvaFeeState, AvaNextBlockCtx};
use crate::feerules;
use crate::mempool::{AdmissionRules, EvmMempool, SenderAccount};
use crate::receipts::AcceptedTxIndex;
use crate::state::FirewoodStateProvider;

// ─── Block tags ──────────────────────────────────────────────────────────────

/// A JSON-RPC block selector. The accepted-block tags `latest`/`safe`/`finalized`
/// all resolve to the **last-accepted height** — Snowman acceptance is final and
/// there is no pending/unsafe head (coreth `rpc_accepted`, spec 10 §17.9).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockTag {
    /// `latest` — the last-accepted block.
    Latest,
    /// `safe` — same as `latest` on Snowman (acceptance is final).
    Safe,
    /// `finalized` — same as `latest` on Snowman (acceptance is final).
    Finalized,
    /// `earliest` — the genesis block (height 0).
    Earliest,
    /// An explicit block number.
    Number(u64),
}

impl BlockTag {
    /// Parses the JSON-RPC block-tag string forms (`latest`/`safe`/`finalized`/
    /// `earliest`/`pending`) or a `0x`-hex / decimal number. `pending` maps to
    /// `latest` (Snowman has no pending head).
    ///
    /// # Errors
    /// Returns [`Error::GenesisParse`] (the crate's string-carrying variant) if
    /// the value is neither a known tag nor a parseable number.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "latest" | "pending" => Ok(BlockTag::Latest),
            "safe" => Ok(BlockTag::Safe),
            "finalized" => Ok(BlockTag::Finalized),
            "earliest" => Ok(BlockTag::Earliest),
            _ => {
                let n = if let Some(hex) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16)
                } else {
                    s.parse::<u64>()
                };
                n.map(BlockTag::Number)
                    .map_err(|e| Error::GenesisParse(format!("invalid block tag {s:?}: {e}")))
            }
        }
    }
}

// ─── Call request ────────────────────────────────────────────────────────────

/// The `eth_call` / `eth_estimateGas` request object (a minimal subset of the
/// JSON-RPC `TransactionCall` — the fields the read-only call path needs).
#[derive(Clone, Debug, Default)]
pub struct CallRequest {
    /// `from` — the caller. Defaults to the zero address.
    pub from: Option<Address>,
    /// `to` — the callee. `None` is a contract creation.
    pub to: Option<Address>,
    /// `gas` — the call gas limit. Defaults to the block gas limit.
    pub gas: Option<u64>,
    /// `value` — wei sent with the call.
    pub value: Option<RuintU256>,
    /// `data` — the call input (calldata or init code).
    pub data: Option<Bytes>,
}

// ─── Fee-history args ────────────────────────────────────────────────────────

/// The `eth_feeHistory` request args.
#[derive(Clone, Debug)]
pub struct FeeHistoryArgs {
    /// Number of blocks in the requested range (ending at `newest_block`).
    pub block_count: u64,
    /// The newest block of the range (an accepted-block tag or number).
    pub newest_block: BlockTag,
    /// Reward percentiles (we report empty `reward` rows; the C-Chain tip is 0).
    pub reward_percentiles: Vec<f64>,
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// The `eth_*` RPC handler set (M6.23). Read-only over Firewood + the canonical
/// store + the fee rules + the facade revm executor. Cheaply cloneable
/// (`Arc`-backed).
pub struct EthRpc {
    /// Firewood state-of-record (read-only here).
    state: Arc<FirewoodStateProvider>,
    /// The accepted-block tip / number↔hash index (the tag mapping).
    canonical: Arc<CanonicalStore>,
    /// The EVM config (fee rules + the revm executor for `eth_call`).
    config: AvaEvmConfig,
    /// The EIP-155 chain id reported by `eth_chainId`.
    chain_id: u64,
    /// The EVM mempool `eth_sendRawTransaction` admits into (cchain-tx-pipeline
    /// task 4). Held behind a [`parking_lot::Mutex`] — the same convention
    /// [`crate::vm::EvmVm`]'s atomic `txpool` uses (`EvmMempool::add_local`
    /// takes `&mut self`).
    mempool: Arc<Mutex<EvmMempool>>,
    /// The accepted-tx receipt index `eth_getTransactionReceipt` reads
    /// (cchain-tx-pipeline task 3/4).
    tx_index: Arc<AcceptedTxIndex>,
}

impl EthRpc {
    /// Builds the handler over the given state/canonical/config + chain id +
    /// the mempool/receipt-index handles (cchain-tx-pipeline task 4).
    #[must_use]
    pub fn new(
        state: Arc<FirewoodStateProvider>,
        canonical: Arc<CanonicalStore>,
        config: AvaEvmConfig,
        chain_id: u64,
        mempool: Arc<Mutex<EvmMempool>>,
        tx_index: Arc<AcceptedTxIndex>,
    ) -> Self {
        Self {
            state,
            canonical,
            config,
            chain_id,
            mempool,
            tx_index,
        }
    }

    // ─── Tag resolution ───────────────────────────────────────────────────────

    /// Resolves a [`BlockTag`] to a concrete block number against the canonical
    /// tip: the accepted tags collapse to the last-accepted height (`None` when
    /// nothing has been accepted), `earliest` → 0, `number(n)` → `n`.
    ///
    /// # Errors
    /// Returns an error if the canonical-tip read fails.
    pub fn resolve_tag(&self, tag: BlockTag) -> Result<Option<u64>> {
        match tag {
            BlockTag::Latest | BlockTag::Safe | BlockTag::Finalized => {
                self.canonical.last_canonical()
            }
            BlockTag::Earliest => Ok(Some(0)),
            BlockTag::Number(n) => Ok(Some(n)),
        }
    }

    // ─── Chain / head ───────────────────────────────────────────────────────

    /// `eth_chainId` — the EIP-155 chain id as a `0x`-quantity.
    #[must_use]
    pub fn chain_id(&self) -> Value {
        quantity(self.chain_id)
    }

    /// `eth_blockNumber` — the last-accepted height as a `0x`-quantity (`0x0`
    /// before genesis is accepted).
    ///
    /// # Errors
    /// Returns an error if the canonical-tip read fails.
    pub fn block_number(&self) -> Result<Value> {
        Ok(quantity(self.canonical.last_canonical()?.unwrap_or(0)))
    }

    // ─── Account reads ─────────────────────────────────────────────────────────

    /// `eth_getBalance` — the account balance (wei) as a `0x`-quantity.
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails.
    pub fn get_balance(&self, addr: Address, _tag: BlockTag) -> Result<Value> {
        let view = self.state.view_tip()?;
        let bal = read_account(&view, &addr)?.map_or(U256::ZERO, |a| a.balance);
        Ok(quantity_u256(bal))
    }

    /// `eth_getTransactionCount` — the account nonce as a `0x`-quantity.
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails.
    pub fn get_transaction_count(&self, addr: Address, _tag: BlockTag) -> Result<Value> {
        let view = self.state.view_tip()?;
        let nonce = read_account(&view, &addr)?.map_or(0, |a| a.nonce);
        Ok(quantity(nonce))
    }

    /// `eth_getCode` — the account's deployed bytecode as `0x`-data (`0x` for an
    /// EOA / absent account).
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails.
    pub fn get_code(&self, addr: Address, _tag: BlockTag) -> Result<Value> {
        use ava_evm_reth::BytecodeReader;

        let view = self.state.view_tip()?;
        let code = match read_account(&view, &addr)? {
            Some(acc) => match acc.bytecode_hash {
                Some(hash) => view
                    .bytecode_by_hash(&hash)?
                    .map(|b| b.original_bytes().to_vec())
                    .unwrap_or_default(),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        Ok(data(&code))
    }

    /// `eth_getStorageAt` — the 32-byte storage word at `slot` as `0x`-data.
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails.
    pub fn get_storage_at(&self, addr: Address, slot: B256, _tag: BlockTag) -> Result<Value> {
        use ava_evm_reth::StateProvider;

        let view = self.state.view_tip()?;
        let value = view.storage(addr, slot)?.unwrap_or(U256::ZERO);
        Ok(data(&value.to_be_bytes::<32>()))
    }

    // ─── eth_sendRawTransaction / eth_getTransactionReceipt ─────────────────────
    // (cchain-tx-pipeline task 4, over Task 1's EvmMempool + Task 3's
    // AcceptedTxIndex.)

    /// `eth_sendRawTransaction` — decode the EIP-2718 envelope, recover the
    /// signer, and admit to the EVM mempool (coreth
    /// `internal/ethapi/api.go:1884-1890` `SendRawTransaction` ->
    /// `SubmitTransaction` -> `txPool.Add`). Returns the tx hash on admission.
    ///
    /// # Errors
    /// Returns [`Error::TxDecode`] if `raw` is not a valid EIP-2718 envelope,
    /// [`Error::InvalidTxSignature`] if signature recovery fails,
    /// [`Error::Mempool`] (carrying the coreth-parity sentinel text) if
    /// [`EvmMempool::add_local`] rejects the tx, or an error if the Firewood
    /// sender-account read fails.
    pub fn send_raw_transaction(&self, raw: &[u8]) -> Result<Value> {
        let mut buf = raw;
        let tx =
            TransactionSigned::decode_2718(&mut buf).map_err(|e| Error::TxDecode(e.to_string()))?;
        let recovered = tx
            .try_into_recovered()
            .map_err(|e| Error::InvalidTxSignature(e.to_string()))?;

        // The eth_getTransactionCount read pattern (this module, above): a
        // Firewood snapshot of the sender's current nonce/balance.
        let sender = {
            let view = self.state.view_tip()?;
            let acc = read_account(&view, &recovered.signer())?;
            SenderAccount {
                nonce: acc.as_ref().map_or(0, |a| a.nonce),
                balance: acc.as_ref().map_or(U256::ZERO, |a| a.balance),
            }
        };
        let rules = AdmissionRules {
            chain_id: self.chain_id,
            ..AdmissionRules::default()
        };
        let hash = self.mempool.lock().add_local(recovered, &sender, &rules)?;
        Ok(data(hash.as_slice()))
    }

    /// `eth_getTransactionReceipt` — the accepted receipt for `hash`, or
    /// `null` if unknown (geth returns `null`, not an error, for an unknown
    /// hash — coreth `internal/ethapi/api.go` `GetTransactionReceipt`).
    ///
    /// `logsBloom` is folded from the record's own logs (not stored on
    /// [`crate::receipts::TxReceiptRecord`]): `ava_evm_reth::logs_bloom`, the
    /// same `Bloom::accrue_log` fold [`crate::builder`] uses for the
    /// block-level bloom, scoped to this one tx's logs.
    ///
    /// Each `logs[]` entry's `logIndex` is the true block-wide index
    /// (go-ethereum `core/types.Receipts.DeriveFields` semantics):
    /// [`crate::receipts::TxReceiptRecord::first_log_index`] (the running
    /// count of logs emitted by every earlier tx in the block) plus this
    /// log's position within [`crate::receipts::TxReceiptRecord::logs`].
    ///
    /// # Errors
    /// Currently infallible (a lookup miss returns `Ok(Value::Null)`), but
    /// returns [`Result`] for API symmetry with the other handlers.
    pub fn get_transaction_receipt(&self, hash: B256) -> Result<Value> {
        let Some(rec) = self.tx_index.get(&hash) else {
            return Ok(Value::Null);
        };

        let logs: Vec<Value> = rec
            .logs
            .iter()
            .enumerate()
            .map(|(i, log)| {
                let local_index = u64::try_from(i).unwrap_or(u64::MAX);
                // Block-wide logIndex (go-ethereum DeriveFields semantics):
                // the running count of every earlier tx's logs, plus this
                // log's position within its own tx.
                let log_index = rec.first_log_index.saturating_add(local_index);
                let topics: Vec<Value> = log.topics().iter().map(|t| data(t.as_slice())).collect();
                json!({
                    "address": data(log.address.as_slice()),
                    "topics": topics,
                    "data": data(log.data.data.as_ref()),
                    "blockNumber": quantity(rec.block_number),
                    "transactionHash": data(rec.tx_hash.as_slice()),
                    "transactionIndex": quantity(rec.tx_index),
                    "blockHash": data(rec.block_hash.as_slice()),
                    "logIndex": quantity(log_index),
                    "removed": false,
                })
            })
            .collect();
        let bloom = logs_bloom(rec.logs.iter());

        Ok(json!({
            "transactionHash": data(rec.tx_hash.as_slice()),
            "transactionIndex": quantity(rec.tx_index),
            "blockHash": data(rec.block_hash.as_slice()),
            "blockNumber": quantity(rec.block_number),
            "from": data(rec.from.as_slice()),
            "to": rec.to.map_or(Value::Null, |a| data(a.as_slice())),
            "cumulativeGasUsed": quantity(rec.cumulative_gas_used),
            "gasUsed": quantity(rec.gas_used),
            "contractAddress": rec.contract_address.map_or(Value::Null, |a| data(a.as_slice())),
            "logs": logs,
            "logsBloom": data(bloom.as_slice()),
            "status": quantity(u64::from(rec.success)),
            "type": quantity(u64::from(rec.tx_type)),
            "effectiveGasPrice": quantity_u128(rec.effective_gas_price),
        }))
    }

    // ─── eth_call / eth_estimateGas ────────────────────────────────────────────

    /// `eth_call` — execute a read-only call against the latest accepted state and
    /// return its output as `0x`-data (revert data on a revert).
    ///
    /// Runs a single revm transaction through the facade executor
    /// ([`AvaEvmConfig::inner`]'s `Evm::transact`) over a Firewood-backed db, with
    /// the fee/nonce/balance checks disabled (the read-only-call convention).
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails or the revm transaction errors
    /// at the database/EVM boundary.
    pub fn call(&self, req: CallRequest, _tag: BlockTag) -> Result<Value> {
        let result = self.transact(&req)?;
        match result {
            ExecutionResult::Success { output, .. } => Ok(data(call_output(&output))),
            // A revert returns its revert data (matching coreth/geth `eth_call`,
            // which surfaces the revert payload rather than erroring here).
            ExecutionResult::Revert { output, .. } => Ok(data(&output)),
            ExecutionResult::Halt { reason, .. } => Err(Error::Provider(ProviderError::Database(
                ava_evm_reth::DatabaseError::Other(format!("eth_call halted: {reason:?}")),
            ))),
        }
    }

    /// `eth_estimateGas` — the gas consumed by the call (a `0x`-quantity). We run
    /// the call once and report its `gas_used` (coreth's estimator binary-searches
    /// for the minimal limit; a single execution at a generous limit is the
    /// faithful lower-bound the differential test pins, with the search refinement
    /// a documented follow-up).
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails or the call halts.
    pub fn estimate_gas(&self, req: CallRequest, _tag: BlockTag) -> Result<Value> {
        let result = self.transact(&req)?;
        match result {
            ExecutionResult::Success { gas, .. } | ExecutionResult::Revert { gas, .. } => {
                Ok(quantity(gas.total_gas_spent()))
            }
            ExecutionResult::Halt { reason, .. } => Err(Error::Provider(ProviderError::Database(
                ava_evm_reth::DatabaseError::Other(format!("eth_estimateGas halted: {reason:?}")),
            ))),
        }
    }

    /// Shared `eth_call`/`eth_estimateGas` execution: build a read-only `TxEnv`
    /// from the request and `transact` it against the latest accepted state.
    fn transact(&self, req: &CallRequest) -> Result<ExecutionResult> {
        // A synthetic header at the last-accepted height drives the env. The
        // fee-bearing fields are zeroed and the fee/balance/nonce checks disabled
        // below, so the call is purely a state read (no funds needed).
        let header = ava_evm_reth::Header {
            number: self.canonical.last_canonical()?.unwrap_or(0),
            gas_limit: req.gas.unwrap_or(u64::MAX),
            ..Default::default()
        };
        let mut env = self.config.evm_env_for_header(&header);
        // Read-only call conventions (spec 10 §9.1): a zero base fee + zero
        // gas_price means no fee is charged (so the caller needs no funds), and
        // `disable_nonce_check` lets a call run regardless of the caller's nonce
        // (the default-build `CfgEnv` exposes only this gate; the base-fee /
        // balance / eip-3607 gates are feature-gated off in this revm build, so we
        // neutralize them via the zero base fee / zero gas_price above).
        env.evm_env.block_env.basefee = 0;
        env.evm_env.cfg_env.disable_nonce_check = true;

        let caller = req.from.unwrap_or(Address::ZERO);
        let view = self.state.view_tip()?;
        // Use the caller's current nonce so the tx is valid even with the nonce
        // check on (a belt-and-braces complement to `disable_nonce_check`).
        let nonce = read_account(&view, &caller)?.map_or(0, |a| a.nonce);

        let tx = TxEnv {
            caller,
            kind: req.to.map_or(TxKind::Create, TxKind::Call),
            gas_limit: req.gas.unwrap_or(env.evm_env.block_env.gas_limit),
            gas_price: 0,
            nonce,
            value: req.value.unwrap_or(U256::ZERO),
            data: req.data.clone().unwrap_or_default(),
            chain_id: Some(self.chain_id),
            ..Default::default()
        };

        let db = StateProviderDatabase::new(view);
        let mut evm = self.config.inner().evm_with_env(db, env.evm_env);
        evm.transact(tx).map(|out| out.result).map_err(|e| {
            Error::Provider(ProviderError::Database(ava_evm_reth::DatabaseError::Other(
                format!("eth_call execution: {e}"),
            )))
        })
    }

    // ─── eth_getProof ──────────────────────────────────────────────────────────

    /// `eth_getProof` — the account proof object. The account fields
    /// (`balance`/`nonce`/`codeHash`/`storageHash`) are read directly from
    /// Firewood (correct today); the merkle-proof arrays are **empty** until M6.25
    /// wires Firewood proofs into [`StateProofProvider`](ava_evm_reth::StateProofProvider)
    /// (see the module note + golden `_provenance.md`).
    ///
    /// # Errors
    /// Returns an error if the Firewood read fails.
    pub fn get_proof(&self, addr: Address, slots: &[B256], _tag: BlockTag) -> Result<Value> {
        use ava_evm_reth::StateProvider;

        let view = self.state.view_tip()?;
        let acc = read_account(&view, &addr)?;
        let (balance, nonce, code_hash) = match acc {
            Some(a) => (a.balance, a.nonce, a.bytecode_hash.unwrap_or(KECCAK_EMPTY)),
            None => (U256::ZERO, 0, KECCAK_EMPTY),
        };

        // Storage values are read directly; the per-slot proof array is empty
        // until M6.25 (Firewood proofs). `storageHash` is the empty-trie sentinel
        // until the sub-trie root is exposed (M6.25 owns StorageRootProvider too).
        let mut storage_proof = Vec::with_capacity(slots.len());
        for slot in slots {
            let value = view.storage(addr, *slot)?.unwrap_or(U256::ZERO);
            storage_proof.push(json!({
                "key": data(slot.as_slice()),
                "value": quantity_u256(value),
                // Empty until M6.25 wires Firewood storage proofs.
                "proof": Value::Array(Vec::new()),
            }));
        }

        Ok(json!({
            "address": data(addr.as_slice()),
            "balance": quantity_u256(balance),
            "nonce": quantity(nonce),
            "codeHash": data(code_hash.as_slice()),
            "storageHash": data(EMPTY_ROOT_HASH.as_slice()),
            // Empty until M6.25 wires Firewood account proofs (StateProofProvider).
            "accountProof": Value::Array(Vec::new()),
            "storageProof": Value::Array(storage_proof),
        }))
    }

    // ─── Fee helpers (feerules overrides, spec 10 §7/§9.1) ──────────────────────

    /// `eth_gasPrice` — the suggested gas price as a `0x`-quantity, from the active
    /// fork's dynamic-fee rules ([`feerules::base_fee`]). The C-Chain has a zero
    /// priority tip, so the suggested price is the next-block base fee.
    ///
    /// # Errors
    /// Returns an error if the canonical/state read fails.
    pub fn gas_price(&self) -> Result<Value> {
        Ok(quantity(self.suggested_base_fee()?))
    }

    /// `eth_maxPriorityFeePerGas` — the C-Chain priority tip, which is **always
    /// zero**: the dynamic base fee (AP3 window / ACP-176) fully prices
    /// congestion, so there is no separate miner tip (coreth
    /// `SuggestTipCap` → 0). Returned as `0x0`.
    ///
    /// # Errors
    /// Currently infallible, but returns `Result` for API symmetry with the other
    /// fee helpers.
    pub fn max_priority_fee_per_gas(&self) -> Result<Value> {
        Ok(quantity(0))
    }

    /// `eth_feeHistory` — the base-fee history over the requested range. We report
    /// the same suggested base fee for each block (the per-block historical base
    /// fee from stored headers lands with the reth-db history wiring, M6.24); the
    /// `reward` rows are empty (zero C-Chain tip) and `gasUsedRatio` is zeroed
    /// until receipts carry cumulative gas (M6.24).
    ///
    /// # Errors
    /// Returns an error if the canonical/state read fails.
    pub fn fee_history(&self, args: FeeHistoryArgs) -> Result<Value> {
        let newest = self.resolve_tag(args.newest_block)?.unwrap_or(0);
        let count = args.block_count.min(newest.saturating_add(1));
        let oldest = newest.saturating_sub(count.saturating_sub(1));

        let base_fee = self.suggested_base_fee()?;
        // baseFeePerGas has count+1 entries (one extra for the "next" block).
        let base_fees: Vec<Value> = (0..count.saturating_add(1))
            .map(|_| quantity(base_fee))
            .collect();
        let gas_used_ratio: Vec<Value> = (0..count).map(|_| json!(0.0)).collect();

        let mut out = json!({
            "oldestBlock": quantity(oldest),
            "baseFeePerGas": base_fees,
            "gasUsedRatio": gas_used_ratio,
        });
        // A non-empty percentile request gets empty per-block reward rows (zero
        // C-Chain tip), matching geth's shape.
        if !args.reward_percentiles.is_empty() {
            let reward: Vec<Value> = (0..count)
                .map(|_| {
                    Value::Array(
                        args.reward_percentiles
                            .iter()
                            .map(|_| quantity(0))
                            .collect(),
                    )
                })
                .collect();
            out["reward"] = Value::Array(reward);
        }
        Ok(out)
    }

    /// The suggested next-block base fee from the active fork's rules. Genesis /
    /// pre-AP3 (legacy, no base fee) and the empty default fee-state both resolve
    /// to 0 (coreth `errNilBaseFee` → "absent" → 0 on the suggestion path).
    fn suggested_base_fee(&self) -> Result<u64> {
        let parent = ava_evm_reth::Header::default();
        let ctx = AvaNextBlockCtx {
            parent_fee_state: AvaFeeState::default(),
            ..AvaNextBlockCtx::default()
        };
        match feerules::base_fee(self.config.chain_spec(), &parent, &ctx) {
            Ok(bf) => Ok(bf),
            // Legacy / nil-base-fee fork → suggested price is 0 (absent base fee).
            Err(Error::NilBaseFee) => Ok(0),
            Err(e) => Err(e),
        }
    }

    // ─── debug_traceTransaction (deferred) ──────────────────────────────────────

    /// `debug_traceTransaction` — **deferred** (M6.23). The prestate tracer needs
    /// a revm inspector not reachable behind the facade without a heavy dep; this
    /// returns a documented error until a follow-up wires the revm tracing stack
    /// (spec 10 §9.1, the prestate-tracer parity ask).
    ///
    /// # Errors
    /// Always returns [`Error::GenesisParse`] (the string-carrying variant) with a
    /// "deferred" message naming `debug_traceTransaction`.
    pub fn debug_trace_transaction(&self, _tx_hash: B256) -> Result<Value> {
        Err(Error::GenesisParse(
            "debug_traceTransaction (prestate tracer) is deferred: needs a revm \
             inspector behind the facade (M6.23 scoping note, spec 10 §9.1)"
                .to_string(),
        ))
    }
}

// ─── Read helpers ─────────────────────────────────────────────────────────────

/// Reads an account from a Firewood view via the reth `AccountReader` trait.
fn read_account(
    view: &crate::state::FirewoodStateView,
    addr: &Address,
) -> Result<Option<ava_evm_reth::Account>> {
    use ava_evm_reth::AccountReader;
    Ok(view.basic_account(addr)?)
}

/// Extracts the call output bytes from a revm [`Output`] (`Call` data, or the
/// empty slice for a `Create` whose deployed code is reported elsewhere).
fn call_output(output: &Output) -> &[u8] {
    match output {
        Output::Call(bytes) => bytes,
        Output::Create(bytes, _) => bytes,
    }
}

// ─── JSON encoding (Ethereum JSON-RPC conventions) ──────────────────────────────

/// Encodes a `u64` as a minimal `0x`-hex quantity (`0x0` for zero — geth/coreth
/// `hexutil.Uint64`).
fn quantity(n: u64) -> Value {
    Value::String(format!("0x{n:x}"))
}

/// Encodes a [`U256`] as a minimal `0x`-hex quantity (`0x0` for zero —
/// `hexutil.Big`).
fn quantity_u256(n: U256) -> Value {
    if n.is_zero() {
        Value::String("0x0".to_string())
    } else {
        Value::String(format!("0x{n:x}"))
    }
}

/// Encodes a byte slice as full-width `0x`-hex data (`0x` for empty —
/// `hexutil.Bytes`).
fn data(bytes: &[u8]) -> Value {
    Value::String(format!("0x{}", hex::encode(bytes)))
}

/// Encodes a `u128` as a minimal `0x`-hex quantity (`0x0` for zero —
/// `effectiveGasPrice`'s wei amount, which fits `u128` but not `u64`).
fn quantity_u128(n: u128) -> Value {
    Value::String(format!("0x{n:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_tag_parse_forms() {
        assert_eq!(BlockTag::parse("latest").expect("t"), BlockTag::Latest);
        assert_eq!(BlockTag::parse("pending").expect("t"), BlockTag::Latest);
        assert_eq!(BlockTag::parse("safe").expect("t"), BlockTag::Safe);
        assert_eq!(
            BlockTag::parse("finalized").expect("t"),
            BlockTag::Finalized
        );
        assert_eq!(BlockTag::parse("earliest").expect("t"), BlockTag::Earliest);
        assert_eq!(BlockTag::parse("0x10").expect("t"), BlockTag::Number(16));
        assert_eq!(BlockTag::parse("42").expect("t"), BlockTag::Number(42));
        assert!(BlockTag::parse("nope").is_err());
    }

    #[test]
    fn quantity_encoding_is_minimal() {
        assert_eq!(quantity(0), Value::String("0x0".to_string()));
        assert_eq!(quantity(255), Value::String("0xff".to_string()));
        assert_eq!(quantity_u256(U256::ZERO), Value::String("0x0".to_string()));
        assert_eq!(
            quantity_u256(U256::from(0x1234u64)),
            Value::String("0x1234".to_string())
        );
    }

    #[test]
    fn data_encoding_is_full_width() {
        assert_eq!(data(&[]), Value::String("0x".to_string()));
        assert_eq!(data(&[0x00, 0x2a]), Value::String("0x002a".to_string()));
    }
}

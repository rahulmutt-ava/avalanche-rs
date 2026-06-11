// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-chain atomic tx builder — port of `wallet/chain/c/builder.go`.
//!
//! Builds `atomic.UnsignedImportTx` / `atomic.UnsignedExportTx` between the
//! C-chain and X/P, pricing them with the Apricot-5 fixed-fee gas model
//! (`tx.GasUsed(fixedFee=true)` + `CalculateDynamicFee`), byte-identical to
//! the Go wallet (specs 12 §13 / §12.5).

use std::collections::BTreeSet;

use ava_avm::txs::components::{Input as FxInput, Output as FxOutput, TransferableInput};
use ava_evm::atomic::mempool::ATOMIC_TX_INTRINSIC_GAS;
use ava_evm::atomic::tx::{
    AtomicTx, CODEC_VERSION, COST_PER_SIGNATURE, EVM_INPUT_GAS, EVM_OUTPUT_GAS, EvmInput,
    EvmOutput, TX_BYTES_GAS, UnsignedExportTx, UnsignedImportTx, X2C_RATE, codec,
};
use ava_secp256k1fx::{TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use super::Context;
use super::backend::Backend;
use crate::common::match_owners;
use crate::error::{Error, Result};
use crate::options::{Options, TxOption};

/// The Go `c.Builder` interface (specs 12 §13). All methods are pure over the
/// [`Backend`] snapshot.
pub trait CBuilder {
    /// The chain configuration used to price/stamp txs.
    fn context(&self) -> &Context;

    /// `GetBalance` — the total EVM balance (wei) over the eth address set.
    fn get_balance(&self, options: &[TxOption]) -> u128;

    /// `GetImportableBalance` — the AVAX (nAVAX) importable from
    /// `source_chain_id`.
    ///
    /// # Errors
    /// [`Error::Overflow`].
    fn get_importable_balance(&self, source_chain_id: Id, options: &[TxOption]) -> Result<u64>;

    /// `NewImportTx` — consume every importable AVAX UTXO from
    /// `source_chain_id` and credit `to` with the total minus the dynamic fee.
    ///
    /// `base_fee` is taken verbatim (wei), exactly like the Go builder; the
    /// `WithBaseFee` option is resolved by the wallet facade (M8.27), not here.
    ///
    /// # Errors
    /// [`Error::InsufficientFunds`] if the imported amount cannot cover the
    /// fee; codec failures.
    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: [u8; 20],
        base_fee: u128,
        options: &[TxOption],
    ) -> Result<UnsignedImportTx>;

    /// `NewExportTx` — debit the EVM accounts to export `outputs` (AVAX only)
    /// to `destination_chain_id`.
    ///
    /// `base_fee` is taken verbatim (wei), exactly like the Go builder; the
    /// `WithBaseFee` option is resolved by the wallet facade (M8.27), not here.
    ///
    /// # Errors
    /// [`Error::InsufficientFunds`]; codec failures.
    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferOutput>,
        base_fee: u128,
        options: &[TxOption],
    ) -> Result<UnsignedExportTx>;
}

/// The concrete builder over a [`Backend`] snapshot (Go `c.NewBuilder`).
pub struct Builder<'a> {
    avax_addrs: BTreeSet<ShortId>,
    eth_addrs: BTreeSet<[u8; 20]>,
    context: Context,
    backend: &'a dyn Backend,
}

impl<'a> Builder<'a> {
    /// Go `c.NewBuilder(avaxAddrs, ethAddrs, context, backend)`.
    #[must_use]
    pub fn new(
        avax_addrs: BTreeSet<ShortId>,
        eth_addrs: BTreeSet<[u8; 20]>,
        context: Context,
        backend: &'a dyn Backend,
    ) -> Self {
        Self {
            avax_addrs,
            eth_addrs,
            context,
            backend,
        }
    }
}

/// `atomic.CalculateDynamicFee` — `(cost · base_fee + X2CRate − 1) / X2CRate`,
/// in nAVAX.
///
/// # Errors
/// [`Error::Overflow`] if the result exceeds `u64`.
pub fn calculate_dynamic_fee(cost: u64, base_fee: u128) -> Result<u64> {
    let x2c = u128::from(X2C_RATE);
    let fee = u128::from(cost)
        .checked_mul(base_fee)
        .and_then(|f| f.checked_add(x2c.checked_sub(1)?))
        .and_then(|f| f.checked_div(x2c))
        .ok_or(Error::Overflow)?;
    u64::try_from(fee).map_err(|_| Error::Overflow)
}

/// `tx.GasUsed(fixedFee=true)` for an unsigned atomic tx: the unsigned wire
/// bytes (`calcBytesCost`) + per-signature costs + `AtomicTxIntrinsicGas`.
fn gas_used(unsigned: &AtomicTx) -> Result<u64> {
    let unsigned_bytes = codec().marshal(CODEC_VERSION, unsigned)?;
    let byte_len = u64::try_from(unsigned_bytes.len()).map_err(|_| Error::Overflow)?;
    let mut cost = byte_len.checked_mul(TX_BYTES_GAS).ok_or(Error::Overflow)?;

    match unsigned {
        AtomicTx::Import(tx) => {
            for input in &tx.imported_inputs {
                let FxInput::SecpTransfer(secp_in) = &input.r#in;
                let in_cost = (secp_in.input.sig_indices.len() as u64)
                    .checked_mul(COST_PER_SIGNATURE)
                    .ok_or(Error::Overflow)?;
                cost = cost.checked_add(in_cost).ok_or(Error::Overflow)?;
            }
        }
        AtomicTx::Export(tx) => {
            let sig_cost = (tx.ins.len() as u64)
                .checked_mul(COST_PER_SIGNATURE)
                .ok_or(Error::Overflow)?;
            cost = cost.checked_add(sig_cost).ok_or(Error::Overflow)?;
        }
    }
    cost.checked_add(ATOMIC_TX_INTRINSIC_GAS)
        .ok_or(Error::Overflow)
}

/// `getSpendableAmount` — the AVAX amount + sig indices if the UTXO is an
/// importable AVAX transfer output owned by `addrs`.
fn spendable_amount(
    utxo: &ava_avm::txs::executor::semantic::Utxo,
    addrs: &BTreeSet<ShortId>,
    min_issuance_time: u64,
    avax_asset_id: Id,
) -> Option<(u64, Vec<u32>)> {
    if utxo.asset_id != avax_asset_id {
        // Only AVAX can be imported.
        return None;
    }
    let FxOutput::SecpTransfer(out) = &utxo.out else {
        return None;
    };
    let sig_indices = match_owners(&out.owners, addrs, min_issuance_time)?;
    Some((out.amt, sig_indices))
}

impl CBuilder for Builder<'_> {
    fn context(&self) -> &Context {
        &self.context
    }

    fn get_balance(&self, options: &[TxOption]) -> u128 {
        let ops = Options::new(options);
        ops.eth_addresses(&self.eth_addrs)
            .iter()
            .map(|addr| self.backend.balance(addr))
            .sum()
    }

    fn get_importable_balance(&self, source_chain_id: Id, options: &[TxOption]) -> Result<u64> {
        let ops = Options::new(options);
        let addrs = ops.addresses(&self.avax_addrs);
        let min_issuance_time = ops.min_issuance_time();

        let mut balance = 0u64;
        for utxo in self.backend.utxos(source_chain_id) {
            let Some((amount, _)) =
                spendable_amount(&utxo, &addrs, min_issuance_time, self.context.avax_asset_id)
            else {
                continue;
            };
            balance = balance.checked_add(amount).ok_or(Error::Overflow)?;
        }
        Ok(balance)
    }

    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: [u8; 20],
        base_fee: u128,
        options: &[TxOption],
    ) -> Result<UnsignedImportTx> {
        let ops = Options::new(options);
        let utxos = self.backend.utxos(source_chain_id);

        let addrs = ops.addresses(&self.avax_addrs);
        let min_issuance_time = ops.min_issuance_time();
        let avax_asset_id = self.context.avax_asset_id;

        let mut imported_inputs = Vec::with_capacity(utxos.len());
        let mut imported_amount = 0u64;
        for utxo in &utxos {
            let Some((amount, sig_indices)) =
                spendable_amount(utxo, &addrs, min_issuance_time, avax_asset_id)
            else {
                continue;
            };
            imported_inputs.push(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id: utxo.asset_id,
                r#in: FxInput::SecpTransfer(TransferInput::new(amount, sig_indices)),
            });
            imported_amount = imported_amount.checked_add(amount).ok_or(Error::Overflow)?;
        }
        imported_inputs.sort_by(TransferableInput::compare);

        let mut tx = UnsignedImportTx {
            network_id: self.context.network_id,
            blockchain_id: self.context.blockchain_id,
            source_chain: source_chain_id,
            imported_inputs,
            outs: Vec::new(),
        };

        let gas_used_without_output = gas_used(&AtomicTx::Import(tx.clone()))?;
        let gas_used_with_output = gas_used_without_output
            .checked_add(EVM_OUTPUT_GAS)
            .ok_or(Error::Overflow)?;
        let tx_fee = calculate_dynamic_fee(gas_used_with_output, base_fee)?;

        if imported_amount <= tx_fee {
            return Err(Error::InsufficientFunds {
                amount: tx_fee.saturating_sub(imported_amount).max(1),
                asset_id: avax_asset_id,
            });
        }

        tx.outs = vec![EvmOutput {
            address: to,
            amount: imported_amount.saturating_sub(tx_fee),
            asset_id: avax_asset_id,
        }];
        Ok(tx)
    }

    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferOutput>,
        base_fee: u128,
        options: &[TxOption],
    ) -> Result<UnsignedExportTx> {
        let avax_asset_id = self.context.avax_asset_id;
        let mut exported_outputs = Vec::with_capacity(outputs.len());
        let mut exported_amount = 0u64;
        for output in outputs {
            exported_amount = exported_amount
                .checked_add(output.amt)
                .ok_or(Error::Overflow)?;
            exported_outputs.push(ava_avm::txs::components::TransferableOutput {
                asset_id: avax_asset_id,
                out: FxOutput::SecpTransfer(output),
            });
        }
        ava_avm::txs::components::sort_transferable_outputs(&mut exported_outputs);

        let mut tx = UnsignedExportTx {
            network_id: self.context.network_id,
            blockchain_id: self.context.blockchain_id,
            destination_chain: destination_chain_id,
            ins: Vec::new(),
            exported_outputs,
        };

        let mut cost = gas_used(&AtomicTx::Export(tx.clone()))?;
        let initial_fee = calculate_dynamic_fee(cost, base_fee)?;
        let mut amount_to_consume = exported_amount
            .checked_add(initial_fee)
            .ok_or(Error::Overflow)?;

        let ops = Options::new(options);
        let eth_addrs = ops.eth_addresses(&self.eth_addrs);
        let mut inputs = Vec::with_capacity(eth_addrs.len());
        for addr in &eth_addrs {
            if amount_to_consume == 0 {
                break;
            }

            let prev_fee = calculate_dynamic_fee(cost, base_fee)?;
            let new_cost = cost.checked_add(EVM_INPUT_GAS).ok_or(Error::Overflow)?;
            let new_fee = calculate_dynamic_fee(new_cost, base_fee)?;
            let additional_fee = new_fee.saturating_sub(prev_fee);

            let balance_wei = self.backend.balance(addr);
            // 1 nAVAX == 1 gWei: divide by the conversion rate to get the
            // exportable AVAX denomination.
            let avax_balance = balance_wei
                .checked_div(u128::from(X2C_RATE))
                .and_then(|b| u64::try_from(b).ok())
                .ok_or(Error::Overflow)?;

            // Skip accounts that can't even cover their own input's fee.
            if avax_balance <= additional_fee {
                continue;
            }

            cost = new_cost;
            amount_to_consume = amount_to_consume
                .checked_add(additional_fee)
                .ok_or(Error::Overflow)?;

            let input_amount = amount_to_consume.min(avax_balance);
            inputs.push(EvmInput {
                address: *addr,
                amount: input_amount,
                asset_id: avax_asset_id,
                nonce: self.backend.nonce(addr),
            });
            amount_to_consume = amount_to_consume.saturating_sub(input_amount);
        }

        if amount_to_consume > 0 {
            return Err(Error::InsufficientFunds {
                amount: amount_to_consume,
                asset_id: avax_asset_id,
            });
        }

        // `SortEVMInputsAndSigners` — by (address, asset_id).
        inputs.sort_by(|a, b| {
            a.address
                .cmp(&b.address)
                .then_with(|| a.asset_id.to_bytes().cmp(&b.asset_id.to_bytes()))
        });
        tx.ins = inputs;
        Ok(tx)
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects
)]
mod tests {
    use std::collections::BTreeMap;

    use ava_avm::txs::executor::semantic::Utxo;
    use ava_secp256k1fx::OutputOwners;

    use super::*;
    use crate::c::backend::WalletBackend;
    use crate::c::signer::Signer;
    use crate::keychain::Keychain;

    // --- the Go-side fixture (wallet_avalanche_rs_vectors_c_test.go) ---

    const MIN_ISSUANCE_TIME: u64 = 1_700_000_000;
    const NONCE: u64 = 7;
    /// 25 gWei.
    const BASE_FEE: u128 = 25_000_000_000;
    /// 5 AVAX in wei.
    const BALANCE_WEI: u128 = 5_000_000_000_000_000_000;

    const MILLI_AVAX: u64 = 1_000_000;
    const AVAX: u64 = 1_000_000_000;

    fn test_keys() -> Vec<ava_crypto::secp256k1::PrivateKey> {
        // Go `secp256k1.TestKeys()`.
        [
            "24jUJ9vZexUM6expyMcT48LBx27k1m7xpraoV62oSQAHdziao5",
            "2MMvUMsxx6zsHSNXJdFD8yc5XkancvwyKPwpw4xUK3TCGDuNBY",
            "cxb7KpGWhDMALTjNNSJ7UQkkomPesyWAPUaWRGdyeBNzR6f35",
            "ewoqjP7PxY4yr3iLTpLisriqt94hdyDFNgchSxGGztUrTXtNN",
            "2RWLv6YVEXDiWLpaCbXhhqxtLbnFaKQsWPSSMSPhpWo47uJAeV",
        ]
        .iter()
        .map(|s| {
            let b = ava_crypto::cb58::cb58_decode(s).expect("decode");
            ava_crypto::secp256k1::PrivateKey::from_bytes(&b).expect("key")
        })
        .collect()
    }

    fn avax_asset_id() -> Id {
        Id::EMPTY.prefix(&[1789])
    }

    fn other_asset_id() -> Id {
        Id::EMPTY.prefix(&[2024])
    }

    fn c_chain_id() -> Id {
        Id::EMPTY.prefix(&[2025])
    }

    fn x_chain_id() -> Id {
        Id::EMPTY.prefix(&[2021])
    }

    fn p_chain_id() -> Id {
        Id::EMPTY
    }

    fn test_context() -> Context {
        Context {
            network_id: ava_types::constants::UNIT_TEST_ID,
            blockchain_id: c_chain_id(),
            avax_asset_id: avax_asset_id(),
        }
    }

    fn secp_utxo(prefix: u64, asset_id: Id, amt: u64, addr: ShortId) -> Utxo {
        Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id,
            out: FxOutput::SecpTransfer(ava_secp256k1fx::TransferOutput::new(
                amt,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        }
    }

    struct Env {
        keychain: Keychain,
        backend: WalletBackend,
        context: Context,
        avax_addrs: BTreeSet<ShortId>,
        eth_addrs: BTreeSet<[u8; 20]>,
        recipient_eth_addr: [u8; 20],
    }

    impl Env {
        fn new() -> Self {
            let keys = test_keys();
            let recipient_eth_addr = keys[0].public_key().eth_address();
            let utxo_addr = keys[1].public_key().address();
            let utxo_eth_addr = keys[1].public_key().eth_address();

            // Two AVAX UTXOs + one non-AVAX (must be skipped) on the X-chain;
            // one AVAX UTXO on the P-chain.
            let x_utxos = vec![
                secp_utxo(3024, avax_asset_id(), 2 * MILLI_AVAX, utxo_addr),
                secp_utxo(3025, other_asset_id(), 5 * AVAX, utxo_addr),
                secp_utxo(3026, avax_asset_id(), 9 * AVAX, utxo_addr),
            ];
            let p_utxos = vec![secp_utxo(5024, avax_asset_id(), 3 * MILLI_AVAX, utxo_addr)];

            let keychain = Keychain::new(keys);
            let avax_addrs = keychain.addresses();
            Self {
                keychain,
                backend: WalletBackend::new(
                    BTreeMap::from([(x_chain_id(), x_utxos), (p_chain_id(), p_utxos)]),
                    BTreeMap::from([(utxo_eth_addr, BALANCE_WEI)]),
                    BTreeMap::from([(utxo_eth_addr, NONCE)]),
                ),
                context: test_context(),
                avax_addrs,
                eth_addrs: BTreeSet::from([utxo_eth_addr]),
                recipient_eth_addr,
            }
        }

        fn builder(&self) -> Builder<'_> {
            Builder::new(
                self.avax_addrs.clone(),
                self.eth_addrs.clone(),
                self.context,
                &self.backend,
            )
        }

        fn opts(&self) -> Vec<TxOption> {
            vec![TxOption::MinIssuanceTime(MIN_ISSUANCE_TIME)]
        }

        /// Builds the unsigned + signed bytes and compares against the Go
        /// vector.
        fn check(&self, name: &str, unsigned: AtomicTx) {
            let vector = load_vector(name);
            assert_eq!(
                hex::encode(self.eth_addrs.iter().next().expect("eth addr")),
                vector.inputs["utxo_eth_addr"],
                "eth address derivation mismatch for {name}"
            );
            assert_eq!(
                hex::encode(self.recipient_eth_addr),
                vector.inputs["recipient_eth_addr"],
                "recipient eth address derivation mismatch for {name}"
            );

            let unsigned_bytes = codec()
                .marshal(CODEC_VERSION, &unsigned)
                .expect("marshal unsigned");
            assert_eq!(
                hex::encode(&unsigned_bytes),
                vector.unsigned_hex,
                "unsigned bytes mismatch for {name}"
            );

            let signer = Signer::new(&self.keychain, &self.backend);
            let signed = signer.sign_unsigned_atomic(unsigned).expect("sign");
            assert_eq!(
                hex::encode(signed.bytes()),
                vector.signed_hex,
                "signed bytes mismatch for {name}"
            );
        }
    }

    #[derive(serde::Deserialize)]
    struct Vector {
        name: String,
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        unsigned_hex: String,
        signed_hex: String,
    }

    #[derive(serde::Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    fn load_vector(name: &str) -> Vector {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/wallet/c.json");
        let data = std::fs::read_to_string(path).expect("read vectors");
        let file: VectorFile = serde_json::from_str(&data).expect("parse vectors");
        file.vectors
            .into_iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("missing vector {name}"))
    }

    #[test]
    fn c_import_x_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_import_tx(x_chain_id(), env.recipient_eth_addr, BASE_FEE, &env.opts())
            .expect("build");
        env.check("c_import_x", AtomicTx::Import(tx));
    }

    #[test]
    fn c_import_p_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_import_tx(p_chain_id(), env.recipient_eth_addr, BASE_FEE, &env.opts())
            .expect("build");
        env.check("c_import_p", AtomicTx::Import(tx));
    }

    #[test]
    fn c_export_x_bytes_match_go() {
        let env = Env::new();
        let keys = test_keys();
        let recipient_owner = OutputOwners::new(0, 1, vec![keys[0].public_key().address()]);
        let tx = env
            .builder()
            .new_export_tx(
                x_chain_id(),
                vec![TransferOutput::new(AVAX, recipient_owner)],
                BASE_FEE,
                &env.opts(),
            )
            .expect("build");
        env.check("c_export_x", AtomicTx::Export(tx));
    }

    #[test]
    fn c_import_insufficient_funds_is_reported() {
        let env = Env::new();
        // An absurd base fee makes the dynamic fee exceed the imported amount.
        let err = env
            .builder()
            .new_import_tx(
                p_chain_id(),
                env.recipient_eth_addr,
                BASE_FEE * 1_000_000,
                &env.opts(),
            )
            .expect_err("must fail");
        assert_matches::assert_matches!(err, Error::InsufficientFunds { .. });
    }

    #[test]
    fn c_export_insufficient_funds_is_reported() {
        let env = Env::new();
        let keys = test_keys();
        let recipient_owner = OutputOwners::new(0, 1, vec![keys[0].public_key().address()]);
        // More than the 5 AVAX the only funded account holds.
        let err = env
            .builder()
            .new_export_tx(
                x_chain_id(),
                vec![TransferOutput::new(100 * AVAX, recipient_owner)],
                BASE_FEE,
                &env.opts(),
            )
            .expect_err("must fail");
        assert_matches::assert_matches!(err, Error::InsufficientFunds { .. });
    }
}

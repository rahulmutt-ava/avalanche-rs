// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-chain tx builder — port of `wallet/chain/x/builder/builder.go`.
//!
//! Static-fee UTXO selection: iterate the canonical-order UTXO snapshot, burn
//! until every requested amount (incl. the flat fee) is covered, returning a
//! change output per partially-consumed UTXO — exactly the Go loop, so the
//! produced unsigned txs are byte-identical (specs 12 §13 / §12.5).
//!
//! `NewOperationTx*` (mint FT/NFT/property) is deferred: the typed
//! `secp256k1fx.MintOperation` / `nftfx` / `propertyfx` operation types do not
//! exist yet in `ava-avm` (`FxOperation::Unsupported` placeholder, M5 §5.5
//! follow-up), so operations can be neither built nor signed faithfully.

use std::collections::{BTreeMap, BTreeSet};

use ava_avm::txs::components::{
    AvaxBaseTx, Input as FxInput, Output as FxOutput, TransferableInput, TransferableOutput,
    sort_transferable_inputs, sort_transferable_outputs,
};
use ava_avm::txs::initial_state::{InitialState, sort_initial_states};
use ava_avm::txs::{BaseTx, CreateAssetTx, ExportTx, ImportTx};
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use super::Context;
use super::backend::{Backend, sort_utxos};
use crate::common::match_owners;
use crate::error::{Error, Result};
use crate::options::{Options, TxOption};

/// The Go `builder.Builder` interface for the X-chain (specs 12 §13). All
/// methods are pure over the [`Backend`] snapshot.
pub trait XBuilder {
    /// The chain configuration used to price/stamp txs.
    fn context(&self) -> &Context;

    /// `GetFTBalance` — the spendable amount of each fungible asset.
    ///
    /// # Errors
    /// [`Error::Overflow`].
    fn get_ft_balance(&self, options: &[TxOption]) -> Result<BTreeMap<Id, u64>>;

    /// `GetImportableBalance`.
    ///
    /// # Errors
    /// [`Error::Overflow`].
    fn get_importable_balance(
        &self,
        source_chain_id: Id,
        options: &[TxOption],
    ) -> Result<BTreeMap<Id, u64>>;

    /// `NewBaseTx` — a simple value transfer.
    ///
    /// # Errors
    /// [`Error::InsufficientFunds`] / [`Error::NoChangeAddress`].
    fn new_base_tx(&self, outputs: Vec<TransferableOutput>, options: &[TxOption])
    -> Result<BaseTx>;

    /// `NewCreateAssetTx` — `initial_state` maps fx index → initial outputs.
    ///
    /// # Errors
    /// [`Error::InsufficientFunds`] / [`Error::NoChangeAddress`].
    fn new_create_asset_tx(
        &self,
        name: String,
        symbol: String,
        denomination: u8,
        initial_state: BTreeMap<u32, Vec<FxOutput>>,
        options: &[TxOption],
    ) -> Result<CreateAssetTx>;

    /// `NewImportTx` — consume every importable UTXO from `source_chain_id`.
    ///
    /// # Errors
    /// [`Error::NoImportableFunds`] / selection failures.
    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<ImportTx>;

    /// `NewExportTx`.
    ///
    /// # Errors
    /// [`Error::InsufficientFunds`] / [`Error::NoChangeAddress`].
    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<ExportTx>;
}

/// The concrete builder over a [`Backend`] snapshot (Go `builder.New`).
pub struct Builder<'a> {
    addrs: BTreeSet<ShortId>,
    context: Context,
    backend: &'a dyn Backend,
}

impl<'a> Builder<'a> {
    /// Go `builder.New(addrs, context, backend)`.
    #[must_use]
    pub fn new(addrs: BTreeSet<ShortId>, context: Context, backend: &'a dyn Backend) -> Self {
        Self {
            addrs,
            context,
            backend,
        }
    }

    fn get_balance_for(&self, chain_id: Id, ops: &Options) -> Result<BTreeMap<Id, u64>> {
        let utxos = self.backend.utxos(chain_id);
        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();
        let mut balance = BTreeMap::new();

        for utxo in &utxos {
            // Only secp transfer outputs are spendable by the wallet.
            let FxOutput::SecpTransfer(out) = &utxo.out else {
                continue;
            };
            if match_owners(&out.owners, &addrs, min_issuance_time).is_none() {
                continue;
            }
            let entry = balance.entry(utxo.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amt).ok_or(Error::Overflow)?;
        }
        Ok(balance)
    }

    /// `builder.spend` — burn `amounts_to_burn` from the canonical-order UTXO
    /// snapshot, producing a change output per partially-consumed UTXO.
    fn spend(
        &self,
        mut amounts_to_burn: BTreeMap<Id, u64>,
        ops: &Options,
    ) -> Result<(Vec<TransferableInput>, Vec<TransferableOutput>)> {
        let mut utxos = self.backend.utxos(self.context.blockchain_id);
        sort_utxos(&mut utxos);

        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();

        let first_addr = addrs.iter().next().ok_or(Error::NoChangeAddress)?;
        let change_owner = ops.change_owner(OutputOwners::new(0, 1, vec![*first_addr]));

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();

        for utxo in &utxos {
            let asset_id = utxo.asset_id;
            let remaining = amounts_to_burn.get(&asset_id).copied().unwrap_or_default();
            if remaining == 0 {
                continue;
            }
            let FxOutput::SecpTransfer(out) = &utxo.out else {
                continue;
            };
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };

            inputs.push(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id,
                r#in: FxInput::SecpTransfer(TransferInput::new(out.amt, sig_indices)),
            });

            let burned = remaining.min(out.amt);
            if let Some(entry) = amounts_to_burn.get_mut(&asset_id) {
                *entry = entry.saturating_sub(burned);
            }
            let remaining_amount = out.amt.saturating_sub(burned);
            if remaining_amount > 0 {
                outputs.push(TransferableOutput {
                    asset_id,
                    out: FxOutput::SecpTransfer(TransferOutput::new(
                        remaining_amount,
                        change_owner.clone(),
                    )),
                });
            }
        }

        for (&asset_id, &amount) in &amounts_to_burn {
            if amount != 0 {
                return Err(Error::InsufficientFunds { amount, asset_id });
            }
        }

        sort_transferable_inputs(&mut inputs);
        sort_transferable_outputs(&mut outputs);
        Ok((inputs, outputs))
    }

    fn base_tx(
        &self,
        inputs: Vec<TransferableInput>,
        outputs: Vec<TransferableOutput>,
        memo: &[u8],
    ) -> AvaxBaseTx {
        AvaxBaseTx {
            network_id: self.context.network_id,
            blockchain_id: self.context.blockchain_id,
            outs: outputs,
            ins: inputs,
            memo: memo.to_vec(),
        }
    }
}

impl XBuilder for Builder<'_> {
    fn context(&self) -> &Context {
        &self.context
    }

    fn get_ft_balance(&self, options: &[TxOption]) -> Result<BTreeMap<Id, u64>> {
        let ops = Options::new(options);
        self.get_balance_for(self.context.blockchain_id, &ops)
    }

    fn get_importable_balance(
        &self,
        source_chain_id: Id,
        options: &[TxOption],
    ) -> Result<BTreeMap<Id, u64>> {
        let ops = Options::new(options);
        self.get_balance_for(source_chain_id, &ops)
    }

    fn new_base_tx(
        &self,
        mut outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<BaseTx> {
        let mut to_burn = BTreeMap::from([(self.context.avax_asset_id, self.context.base_tx_fee)]);
        for out in &outputs {
            let entry = to_burn.entry(out.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amount()).ok_or(Error::Overflow)?;
        }

        let ops = Options::new(options);
        let (inputs, change_outputs) = self.spend(to_burn, &ops)?;
        outputs.extend(change_outputs);
        sort_transferable_outputs(&mut outputs);

        Ok(BaseTx::new(self.base_tx(inputs, outputs, ops.memo())))
    }

    fn new_create_asset_tx(
        &self,
        name: String,
        symbol: String,
        denomination: u8,
        initial_state: BTreeMap<u32, Vec<FxOutput>>,
        options: &[TxOption],
    ) -> Result<CreateAssetTx> {
        let to_burn =
            BTreeMap::from([(self.context.avax_asset_id, self.context.create_asset_tx_fee)]);
        let ops = Options::new(options);
        let (inputs, outputs) = self.spend(to_burn, &ops)?;

        let mut states = Vec::with_capacity(initial_state.len());
        for (fx_index, outs) in initial_state {
            let mut state = InitialState {
                fx_index,
                outs,
                fx_id: Id::EMPTY, // runtime-only; never encoded
            };
            state.sort();
            states.push(state);
        }
        sort_initial_states(&mut states);

        Ok(CreateAssetTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            name,
            symbol,
            denomination,
            states,
        })
    }

    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<ImportTx> {
        let ops = Options::new(options);
        let mut utxos = self.backend.utxos(source_chain_id);
        sort_utxos(&mut utxos);

        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();
        let avax_asset_id = self.context.avax_asset_id;
        let tx_fee = self.context.base_tx_fee;

        let mut imported_inputs = Vec::with_capacity(utxos.len());
        let mut imported_amounts: BTreeMap<Id, u64> = BTreeMap::new();
        for utxo in &utxos {
            let FxOutput::SecpTransfer(out) = &utxo.out else {
                continue;
            };
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };
            imported_inputs.push(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id: utxo.asset_id,
                r#in: FxInput::SecpTransfer(TransferInput::new(out.amt, sig_indices)),
            });
            let entry = imported_amounts.entry(utxo.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amt).ok_or(Error::Overflow)?;
        }
        sort_transferable_inputs(&mut imported_inputs);

        if imported_amounts.is_empty() {
            return Err(Error::NoImportableFunds);
        }

        let mut inputs = Vec::new();
        let mut outputs = Vec::with_capacity(imported_amounts.len());
        let imported_avax = imported_amounts
            .get(&avax_asset_id)
            .copied()
            .unwrap_or_default();
        if imported_avax > tx_fee {
            if let Some(entry) = imported_amounts.get_mut(&avax_asset_id) {
                *entry = entry.saturating_sub(tx_fee);
            }
        } else {
            if imported_avax < tx_fee {
                // The imported amount only covers part of the fee.
                let to_burn =
                    BTreeMap::from([(avax_asset_id, tx_fee.saturating_sub(imported_avax))]);
                (inputs, outputs) = self.spend(to_burn, &ops)?;
            }
            imported_amounts.remove(&avax_asset_id);
        }

        for (&asset_id, &amount) in &imported_amounts {
            outputs.push(TransferableOutput {
                asset_id,
                out: FxOutput::SecpTransfer(TransferOutput::new(amount, to.clone())),
            });
        }
        sort_transferable_outputs(&mut outputs);

        Ok(ImportTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            source_chain: source_chain_id,
            imported_ins: imported_inputs,
        })
    }

    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        mut outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<ExportTx> {
        let mut to_burn = BTreeMap::from([(self.context.avax_asset_id, self.context.base_tx_fee)]);
        for out in &outputs {
            let entry = to_burn.entry(out.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amount()).ok_or(Error::Overflow)?;
        }

        let ops = Options::new(options);
        let (inputs, change_outputs) = self.spend(to_burn, &ops)?;

        sort_transferable_outputs(&mut outputs);
        Ok(ExportTx {
            base: BaseTx::new(self.base_tx(inputs, change_outputs, ops.memo())),
            destination_chain: destination_chain_id,
            exported_outs: outputs,
        })
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
    use ava_avm::txs::executor::semantic::Utxo;
    use ava_avm::txs::{CODEC_VERSION, UnsignedTx};
    use ava_secp256k1fx::MintOutput;
    use ava_types::constants::UNIT_TEST_ID;

    use super::*;
    use crate::keychain::Keychain;
    use crate::x::backend::WalletBackend;
    use crate::x::signer::Signer;

    // --- the Go-side fixture (wallet_avalanche_rs_vectors_x_test.go) ---

    const MIN_ISSUANCE_TIME: u64 = 1_700_000_000;

    const NANO_AVAX: u64 = 1;
    const MICRO_AVAX: u64 = 1_000;
    const MILLI_AVAX: u64 = 1_000_000;
    const AVAX: u64 = 1_000_000_000;
    const MEGA_AVAX: u64 = 1_000_000_000_000_000;

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

    fn x_chain_id() -> Id {
        Id::EMPTY.prefix(&[2021])
    }

    fn other_chain_id() -> Id {
        Id::EMPTY.prefix(&[6161])
    }

    fn small_chain_id() -> Id {
        Id::EMPTY.prefix(&[6262])
    }

    fn test_context() -> Context {
        Context {
            network_id: UNIT_TEST_ID,
            blockchain_id: x_chain_id(),
            avax_asset_id: avax_asset_id(),
            base_tx_fee: MICRO_AVAX,
            create_asset_tx_fee: 99 * MILLI_AVAX,
        }
    }

    fn secp_utxo(prefix: u64, asset_id: Id, amt: u64, addr: ShortId) -> Utxo {
        Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id,
            out: FxOutput::SecpTransfer(TransferOutput::new(
                amt,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        }
    }

    struct Env {
        keychain: Keychain,
        backend: WalletBackend,
        context: Context,
        addrs: BTreeSet<ShortId>,
        utxo_owner: OutputOwners,
        recipient_owner: OutputOwners,
    }

    impl Env {
        fn new() -> Self {
            let keys = test_keys();
            let recipient_addr = keys[0].public_key().address();
            let utxo_addr = keys[1].public_key().address();

            let chain_utxos = vec![
                secp_utxo(2024, avax_asset_id(), 2 * MILLI_AVAX, utxo_addr),
                secp_utxo(2025, other_asset_id(), 99 * MEGA_AVAX, utxo_addr),
                secp_utxo(2026, avax_asset_id(), 9 * AVAX, utxo_addr),
            ];
            let import_utxos = vec![
                secp_utxo(3024, avax_asset_id(), 2 * MILLI_AVAX, utxo_addr),
                secp_utxo(3025, other_asset_id(), 5 * AVAX, utxo_addr),
            ];
            let small_import_utxos =
                vec![secp_utxo(4024, avax_asset_id(), 600 * NANO_AVAX, utxo_addr)];

            let utxo_sets = BTreeMap::from([
                (x_chain_id(), chain_utxos),
                (other_chain_id(), import_utxos),
                (small_chain_id(), small_import_utxos),
            ]);

            Self {
                keychain: Keychain::new(keys),
                backend: WalletBackend::new(utxo_sets),
                context: test_context(),
                addrs: BTreeSet::from([utxo_addr]),
                utxo_owner: OutputOwners::new(0, 1, vec![utxo_addr]),
                recipient_owner: OutputOwners::new(0, 1, vec![recipient_addr]),
            }
        }

        fn builder(&self) -> Builder<'_> {
            Builder::new(self.addrs.clone(), self.context, &self.backend)
        }

        fn opts(&self) -> Vec<TxOption> {
            vec![
                TxOption::MinIssuanceTime(MIN_ISSUANCE_TIME),
                TxOption::ChangeOwner(self.utxo_owner.clone()),
            ]
        }

        /// Builds the unsigned + signed bytes and compares against the Go
        /// vector.
        fn check(&self, name: &str, unsigned: UnsignedTx) {
            let vector = load_vector(name);
            let unsigned_bytes = ava_avm::txs::codec::Codec()
                .marshal(CODEC_VERSION, &unsigned)
                .expect("marshal unsigned");
            assert_eq!(
                hex::encode(&unsigned_bytes),
                vector.unsigned_hex,
                "unsigned bytes mismatch for {name}"
            );

            let signer = Signer::new(&self.keychain, &self.backend);
            let signed = signer.sign_unsigned(unsigned).expect("sign");
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
        unsigned_hex: String,
        signed_hex: String,
    }

    #[derive(serde::Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    fn load_vector(name: &str) -> Vector {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/wallet/x.json");
        let data = std::fs::read_to_string(path).expect("read vectors");
        let file: VectorFile = serde_json::from_str(&data).expect("parse vectors");
        file.vectors
            .into_iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("missing vector {name}"))
    }

    fn avax_output(env: &Env) -> TransferableOutput {
        TransferableOutput {
            asset_id: avax_asset_id(),
            out: FxOutput::SecpTransfer(TransferOutput::new(7 * AVAX, env.utxo_owner.clone())),
        }
    }

    #[test]
    fn insufficient_funds_is_reported() {
        let env = Env::new();
        let too_much = TransferableOutput {
            asset_id: avax_asset_id(),
            out: FxOutput::SecpTransfer(TransferOutput::new(1_000 * AVAX, env.utxo_owner.clone())),
        };
        let err = env
            .builder()
            .new_base_tx(vec![too_much], &env.opts())
            .expect_err("must fail");
        assert_matches::assert_matches!(err, Error::InsufficientFunds { .. });
    }

    #[test]
    fn x_base_tx_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_base_tx(vec![avax_output(&env)], &env.opts())
            .expect("build");
        env.check("x_base", UnsignedTx::Base(tx));
    }

    #[test]
    fn x_base_tx_with_memo_bytes_match_go() {
        let env = Env::new();
        let mut opts = env.opts();
        opts.push(TxOption::Memo(b"memo".to_vec()));
        let tx = env
            .builder()
            .new_base_tx(vec![avax_output(&env)], &opts)
            .expect("build");
        env.check("x_base_memo", UnsignedTx::Base(tx));
    }

    #[test]
    fn x_create_asset_tx_bytes_match_go() {
        let env = Env::new();
        let initial_state = BTreeMap::from([(
            super::super::SECP256K1_FX_INDEX,
            vec![
                FxOutput::SecpMint(MintOutput::new(env.recipient_owner.clone())),
                FxOutput::SecpTransfer(TransferOutput::new(1234, env.utxo_owner.clone())),
            ],
        )]);
        let tx = env
            .builder()
            .new_create_asset_tx(
                "Team Rocket".to_string(),
                "TR".to_string(),
                0,
                initial_state,
                &env.opts(),
            )
            .expect("build");
        env.check("x_create_asset", UnsignedTx::CreateAsset(tx));
    }

    #[test]
    fn x_import_tx_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_import_tx(other_chain_id(), env.recipient_owner.clone(), &env.opts())
            .expect("build");
        env.check("x_import", UnsignedTx::Import(tx));
    }

    #[test]
    fn x_import_tx_avax_lt_fee_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_import_tx(small_chain_id(), env.recipient_owner.clone(), &env.opts())
            .expect("build");
        env.check("x_import_avax_lt_fee", UnsignedTx::Import(tx));
    }

    #[test]
    fn x_export_tx_bytes_match_go() {
        let env = Env::new();
        let tx = env
            .builder()
            .new_export_tx(other_chain_id(), vec![avax_output(&env)], &env.opts())
            .expect("build");
        env.check("x_export", UnsignedTx::Export(tx));
    }
}

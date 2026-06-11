// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The primary-network wallet — port of `wallet/subnet/primary`
//! (`wallet.go` + `api.go`; specs 12 §13).
//!
//! [`make_wallet`] fetches all wallet state (UTXOs for every source→destination
//! chain pair, P-chain owners, C-chain account balances/nonces and the chain
//! contexts) over the [`crate::client`] trait seam, then wires the three chain
//! wallet facades around one shared [`UtxoStore`] — exactly Go's
//! `primary.MakeWallet` over `FetchState`/`FetchEthState`.
//!
//! The live JSON-RPC-over-HTTP clients are deferred to the `ava-api` milestone
//! tasks (M8.18/M8.22); until then callers construct [`Clients`] from their own
//! transport (tests use in-memory mocks). See `tests/PORTING.md`.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_types::id::Id;
use ava_types::short_id::ShortId;

use crate::client::{CChainClient, EthClient, InfoClient, PChainClient, XChainClient};
use crate::common::utxos::UtxoStore;
use crate::error::{Error, Result};
use crate::keychain::Keychain;
use crate::options::TxOption;
use crate::p::PLATFORM_CHAIN_ID;
use crate::{c, p, x};

/// `gasPriceMultiplier` — Go inflates the fetched P-chain gas price to support
/// issuing multiple transactions (`wallet/chain/p/context.go`).
const GAS_PRICE_MULTIPLIER: u64 = 2;

/// `primary.WalletConfig` — the owner ids the wallet pre-fetches so it can
/// authorize subnet / L1-validator / auto-renew transactions.
#[derive(Clone, Debug, Default)]
pub struct WalletConfig {
    /// `SubnetIDs` — subnets the wallet should know the owners of.
    pub subnet_ids: Vec<Id>,
    /// `ValidationIDs` — L1 validations the wallet should know the
    /// (deactivation) owners of.
    pub validation_ids: Vec<Id>,
    /// `AutoRenewedValidatorTxIDs` — auto-renewed validator txs the wallet
    /// should know the validator-authority owners of (ACP-236).
    pub auto_renewed_validator_tx_ids: Vec<Id>,
}

/// The API clients [`make_wallet`] fetches state over and the chain wallets
/// issue through — Go's `info.Client` / `platformvm.Client` / `avm.Client` /
/// `evm client.Client` / `ethclient.Client` bundle (`primary/api.go`).
#[derive(Clone)]
pub struct Clients {
    /// The info API (`info.getNetworkID` / `info.getBlockchainID`).
    pub info: Arc<dyn InfoClient>,
    /// The P-chain platform API client.
    pub p: Arc<dyn PChainClient>,
    /// The X-chain avm API client.
    pub x: Arc<dyn XChainClient>,
    /// The C-chain avax (atomic) API client.
    pub c: Arc<dyn CChainClient>,
    /// The C-chain eth JSON-RPC client.
    pub eth: Arc<dyn EthClient>,
}

/// `primary.Wallet` — the P/X/C chain wallets of the primary network.
#[derive(Clone)]
pub struct Wallet {
    p: p::wallet::Wallet,
    x: x::wallet::Wallet,
    c: c::wallet::Wallet,
}

impl Wallet {
    /// `primary.NewWallet`.
    #[must_use]
    pub fn new(p: p::wallet::Wallet, x: x::wallet::Wallet, c: c::wallet::Wallet) -> Self {
        Self { p, x, c }
    }

    /// `Wallet.P()`.
    #[must_use]
    pub fn p(&self) -> &p::wallet::Wallet {
        &self.p
    }

    /// `Wallet.X()`.
    #[must_use]
    pub fn x(&self) -> &x::wallet::Wallet {
        &self.x
    }

    /// `Wallet.C()`.
    #[must_use]
    pub fn c(&self) -> &c::wallet::Wallet {
        &self.c
    }

    /// `primary.NewWalletWithOptions` — a wallet that applies `options` before
    /// the per-call options on every operation, on all three chains.
    #[must_use]
    pub fn with_options(self, options: Vec<TxOption>) -> Self {
        Self {
            p: self.p.with_options(options.clone()),
            x: self.x.with_options(options.clone()),
            c: self.c.with_options(options),
        }
    }
}

/// `primary.MakeWallet` — fetches all UTXOs referencing the keychain's
/// addresses (every source→destination chain pair), the requested P-chain
/// owners, the C-chain accounts and the chain contexts, and wires the P/X/C
/// wallets over one shared UTXO store.
///
/// Go's signature takes a `uri` and dials the clients itself; the live HTTP
/// transport is deferred to the `ava-api` milestone (M8.18/M8.22), so the Rust
/// port takes the already-constructed [`Clients`].
///
/// # Errors
/// Propagates client failures ([`Error::Client`]) and codec failures decoding
/// the fetched UTXOs; [`Error::Overflow`] if the gas price multiplication
/// overflows.
pub async fn make_wallet(
    clients: &Clients,
    keychain: Keychain,
    config: &WalletConfig,
) -> Result<Wallet> {
    let keychain = Arc::new(keychain);
    let avax_addrs: Vec<ShortId> = keychain.addresses().into_iter().collect();

    // --- contexts (Go p/x/c `NewContextFromClients`) ---
    let network_id = clients.info.get_network_id().await?;
    let x_chain_id = clients.info.get_blockchain_id(x::ALIAS).await?;
    let c_chain_id = clients.info.get_blockchain_id(c::ALIAS).await?;

    let p_context = p::Context {
        network_id,
        avax_asset_id: clients.p.get_staking_asset_id().await?,
        complexity_weights: clients.p.get_dynamic_fee_weights().await?,
        gas_price: clients
            .p
            .get_gas_price()
            .await?
            .checked_mul(GAS_PRICE_MULTIPLIER)
            .ok_or(Error::Overflow)?,
    };

    let xc_avax_asset_id = clients.x.get_avax_asset_id().await?;
    let (base_tx_fee, create_asset_tx_fee) = clients.x.get_tx_fees().await?;
    let x_context = x::Context {
        network_id,
        blockchain_id: x_chain_id,
        avax_asset_id: xc_avax_asset_id,
        base_tx_fee,
        create_asset_tx_fee,
    };
    let c_context = c::Context {
        network_id,
        blockchain_id: c_chain_id,
        avax_asset_id: xc_avax_asset_id,
    };

    // --- UTXOs (Go `FetchState`: AddAllUTXOs over every chain pair) ---
    let store = Arc::new(UtxoStore::default());
    let chain_ids = [PLATFORM_CHAIN_ID, x_chain_id, c_chain_id];
    for source_chain_id in chain_ids {
        for bytes in clients
            .p
            .get_atomic_utxos(&avax_addrs, source_chain_id)
            .await?
        {
            let mut utxo = ava_platformvm::utxo::Utxo::default();
            ava_platformvm::txs::Codec().unmarshal(&bytes, &mut utxo)?;
            store.add_p(source_chain_id, utxo);
        }
        for bytes in clients
            .x
            .get_atomic_utxos(&avax_addrs, source_chain_id)
            .await?
        {
            let mut utxo = ava_avm::txs::executor::semantic::Utxo::default();
            ava_avm::txs::codec::Codec().unmarshal(&bytes, &mut utxo)?;
            store.add_xc(source_chain_id, x_chain_id, utxo);
        }
        for bytes in clients
            .c
            .get_atomic_utxos(&avax_addrs, source_chain_id)
            .await?
        {
            let mut utxo = ava_avm::txs::executor::semantic::Utxo::default();
            ava_evm::atomic::tx::codec().unmarshal(&bytes, &mut utxo)?;
            store.add_xc(source_chain_id, c_chain_id, utxo);
        }
    }

    // --- P-chain owners (Go `PClient.GetOwners`) ---
    let owners = clients
        .p
        .get_owners(
            &config.subnet_ids,
            &config.validation_ids,
            &config.auto_renewed_validator_tx_ids,
        )
        .await?;

    // --- C-chain accounts (Go `FetchEthState`) ---
    let mut accounts = BTreeMap::new();
    for addr in keychain.eth_addresses() {
        accounts.insert(
            addr,
            c::wallet::Account {
                balance: clients.eth.balance(&addr).await?,
                nonce: clients.eth.nonce(&addr).await?,
            },
        );
    }

    // --- wire the chain wallets (Go `primary.MakeWallet` tail) ---
    let p_backend = Arc::new(p::wallet::Backend::new(Arc::clone(&store), owners));
    let x_backend = Arc::new(x::wallet::Backend::new(x_chain_id, Arc::clone(&store)));
    let c_backend = Arc::new(c::wallet::Backend::new(c_chain_id, store, accounts));

    Ok(Wallet::new(
        p::wallet::Wallet::new(
            Arc::clone(&clients.p),
            p_backend,
            Arc::clone(&keychain),
            p_context,
        ),
        x::wallet::Wallet::new(
            Arc::clone(&clients.x),
            x_backend,
            Arc::clone(&keychain),
            x_context,
        ),
        c::wallet::Wallet::new(
            Arc::clone(&clients.c),
            Arc::clone(&clients.eth),
            c_backend,
            keychain,
            c_context,
        ),
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::sync::Mutex;

    use assert_matches::assert_matches;
    use async_trait::async_trait;
    use ava_platformvm::txs::fee::gas::Dimensions;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_types::constants::UNIT_TEST_ID;

    use super::*;
    use crate::c::CBuilder;
    use crate::c::backend::Backend as _;
    use crate::p::PBuilder;
    use crate::p::backend::Backend as _;
    use crate::x::XBuilder;
    use crate::x::backend::Backend as _;

    const MIN_ISSUANCE_TIME: u64 = 1_700_000_000;
    const AVAX: u64 = 1_000_000_000;
    const WEIGHTS: Dimensions = [1, 10, 100, 1000];
    const GAS_PRICE: u64 = 1;
    const BASE_TX_FEE: u64 = 1_000_000;
    const CREATE_ASSET_TX_FEE: u64 = 10_000_000;
    /// 25 gWei.
    const BASE_FEE_ESTIMATE: u128 = 25_000_000_000;
    /// 5 AVAX in wei.
    const ETH_BALANCE_WEI: u128 = 5_000_000_000_000_000_000;
    const ETH_NONCE: u64 = 7;

    fn test_key() -> ava_crypto::secp256k1::PrivateKey {
        // Go `secp256k1.TestKeys()[0]`.
        let b = ava_crypto::cb58::cb58_decode("24jUJ9vZexUM6expyMcT48LBx27k1m7xpraoV62oSQAHdziao5")
            .expect("decode");
        ava_crypto::secp256k1::PrivateKey::from_bytes(&b).expect("key")
    }

    fn avax_asset_id() -> Id {
        Id::EMPTY.prefix(&[1789])
    }

    fn x_chain_id() -> Id {
        Id::EMPTY.prefix(&[2021])
    }

    fn c_chain_id() -> Id {
        Id::EMPTY.prefix(&[2025])
    }

    fn subnet_id() -> Id {
        Id::EMPTY.prefix(&[7777])
    }

    fn p_utxo_bytes(prefix: u64, amt: u64, addr: ShortId) -> Vec<u8> {
        let utxo = ava_platformvm::utxo::Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id: avax_asset_id(),
            out: ava_platformvm::txs::components::Output::Transfer(TransferOutput::new(
                amt,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        };
        utxo.marshal().expect("marshal")
    }

    fn avm_utxo_bytes(prefix: u64, amt: u64, addr: ShortId) -> Vec<u8> {
        let utxo = ava_avm::txs::executor::semantic::Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id: avax_asset_id(),
            out: ava_avm::txs::components::Output::SecpTransfer(TransferOutput::new(
                amt,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        };
        utxo.marshal().expect("marshal")
    }

    /// Fetched UTXO bytes keyed by `(destination alias, source chain id)`.
    type UtxoPages = BTreeMap<(char, Id), Vec<Vec<u8>>>;

    /// One in-memory node: implements all five client traits over shared
    /// state.
    #[derive(Default)]
    struct Mock {
        utxos: Mutex<UtxoPages>,
        issued: Mutex<BTreeMap<char, Vec<Vec<u8>>>>,
        awaited: Mutex<Vec<Id>>,
        owners: BTreeMap<Id, OutputOwners>,
        estimate_calls: Mutex<u32>,
    }

    impl Mock {
        fn issue(&self, chain: char, tx_bytes: &[u8]) -> Id {
            self.issued
                .lock()
                .expect("lock")
                .entry(chain)
                .or_default()
                .push(tx_bytes.to_vec());
            Id::from(ava_crypto::hashing::sha256(tx_bytes))
        }

        fn fetch(&self, chain: char, source_chain_id: Id) -> Vec<Vec<u8>> {
            self.utxos
                .lock()
                .expect("lock")
                .get(&(chain, source_chain_id))
                .cloned()
                .unwrap_or_default()
        }

        fn issued(&self, chain: char) -> Vec<Vec<u8>> {
            self.issued
                .lock()
                .expect("lock")
                .get(&chain)
                .cloned()
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl InfoClient for Mock {
        async fn get_network_id(&self) -> Result<u32> {
            Ok(UNIT_TEST_ID)
        }

        async fn get_blockchain_id(&self, alias: &str) -> Result<Id> {
            match alias {
                "X" => Ok(x_chain_id()),
                "C" => Ok(c_chain_id()),
                _ => Err(Error::Client("unknown alias".into())),
            }
        }
    }

    #[async_trait]
    impl PChainClient for Mock {
        async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id> {
            Ok(self.issue('P', tx_bytes))
        }

        async fn await_tx_accepted(&self, tx_id: Id) -> Result<()> {
            self.awaited.lock().expect("lock").push(tx_id);
            Ok(())
        }

        async fn get_atomic_utxos(
            &self,
            _addrs: &[ShortId],
            source_chain_id: Id,
        ) -> Result<Vec<Vec<u8>>> {
            Ok(self.fetch('P', source_chain_id))
        }

        async fn get_owners(
            &self,
            subnet_ids: &[Id],
            validation_ids: &[Id],
            auto_renewed_validator_tx_ids: &[Id],
        ) -> Result<BTreeMap<Id, OutputOwners>> {
            let mut owners = BTreeMap::new();
            for id in subnet_ids
                .iter()
                .chain(validation_ids)
                .chain(auto_renewed_validator_tx_ids)
            {
                if let Some(owner) = self.owners.get(id) {
                    owners.insert(*id, owner.clone());
                }
            }
            Ok(owners)
        }

        async fn get_staking_asset_id(&self) -> Result<Id> {
            Ok(avax_asset_id())
        }

        async fn get_dynamic_fee_weights(&self) -> Result<Dimensions> {
            Ok(WEIGHTS)
        }

        async fn get_gas_price(&self) -> Result<u64> {
            Ok(GAS_PRICE)
        }
    }

    #[async_trait]
    impl XChainClient for Mock {
        async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id> {
            Ok(self.issue('X', tx_bytes))
        }

        async fn await_tx_accepted(&self, tx_id: Id) -> Result<()> {
            self.awaited.lock().expect("lock").push(tx_id);
            Ok(())
        }

        async fn get_atomic_utxos(
            &self,
            _addrs: &[ShortId],
            source_chain_id: Id,
        ) -> Result<Vec<Vec<u8>>> {
            Ok(self.fetch('X', source_chain_id))
        }

        async fn get_avax_asset_id(&self) -> Result<Id> {
            Ok(avax_asset_id())
        }

        async fn get_tx_fees(&self) -> Result<(u64, u64)> {
            Ok((BASE_TX_FEE, CREATE_ASSET_TX_FEE))
        }
    }

    #[async_trait]
    impl CChainClient for Mock {
        async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id> {
            Ok(self.issue('C', tx_bytes))
        }

        async fn await_tx_accepted(&self, tx_id: Id) -> Result<()> {
            self.awaited.lock().expect("lock").push(tx_id);
            Ok(())
        }

        async fn get_atomic_utxos(
            &self,
            _addrs: &[ShortId],
            source_chain_id: Id,
        ) -> Result<Vec<Vec<u8>>> {
            Ok(self.fetch('C', source_chain_id))
        }
    }

    #[async_trait]
    impl EthClient for Mock {
        async fn balance(&self, _addr: &[u8; 20]) -> Result<u128> {
            Ok(ETH_BALANCE_WEI)
        }

        async fn nonce(&self, _addr: &[u8; 20]) -> Result<u64> {
            Ok(ETH_NONCE)
        }

        async fn estimate_base_fee(&self) -> Result<u128> {
            let mut calls = self.estimate_calls.lock().expect("lock");
            *calls = calls.checked_add(1).expect("count");
            Ok(BASE_FEE_ESTIMATE)
        }
    }

    fn clients(mock: &Arc<Mock>) -> Clients {
        Clients {
            info: Arc::clone(mock) as Arc<dyn InfoClient>,
            p: Arc::clone(mock) as Arc<dyn PChainClient>,
            x: Arc::clone(mock) as Arc<dyn XChainClient>,
            c: Arc::clone(mock) as Arc<dyn CChainClient>,
            eth: Arc::clone(mock) as Arc<dyn EthClient>,
        }
    }

    fn opts() -> Vec<TxOption> {
        vec![TxOption::MinIssuanceTime(MIN_ISSUANCE_TIME)]
    }

    /// `Wallet::issue_*_tx` = build → sign → submit → record: the submitted
    /// bytes equal the built+signed bytes, the consumed UTXO leaves the
    /// backend and the created UTXOs appear in it (12 §13).
    #[tokio::test]
    async fn issue_flow_records_in_backend() {
        let key = test_key();
        let addr = key.public_key().address();
        let mock = Arc::new(Mock::default());
        mock.utxos.lock().expect("lock").insert(
            ('P', PLATFORM_CHAIN_ID),
            vec![p_utxo_bytes(1, 9 * AVAX, addr)],
        );

        let wallet = make_wallet(
            &clients(&mock),
            Keychain::new(vec![key]),
            &WalletConfig::default(),
        )
        .await
        .expect("make_wallet");

        let spent_id = Id::EMPTY.prefix(&[1]).prefix(&[1]);
        assert_eq!(
            wallet
                .p()
                .backend()
                .get_utxo(PLATFORM_CHAIN_ID, spent_id)
                .map(|u| u.input_id()),
            Some(spent_id),
        );

        let outputs = vec![ava_platformvm::txs::components::TransferableOutput {
            asset_id: avax_asset_id(),
            out: ava_platformvm::txs::components::Output::Transfer(TransferOutput::new(
                AVAX,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        }];
        let tx = wallet
            .p()
            .issue_base_tx(outputs, &opts())
            .await
            .expect("issue");

        // The submitted bytes are exactly the built+signed bytes.
        assert_eq!(mock.issued('P'), vec![tx.signed_bytes.clone()]);
        // Issuance polled for acceptance (no `AssumeDecided`).
        assert_eq!(
            mock.awaited.lock().expect("lock").as_slice(),
            &[Id::from(ava_crypto::hashing::sha256(&tx.signed_bytes))],
        );

        // The consumed UTXO left the backend...
        let backend = wallet.p().backend();
        assert_eq!(backend.get_utxo(PLATFORM_CHAIN_ID, spent_id), None);
        // ...and every produced output was recorded under the new tx id.
        let outs = tx.unsigned.outputs();
        assert!(!outs.is_empty());
        let utxos = backend.utxos(PLATFORM_CHAIN_ID);
        assert_eq!(utxos.len(), outs.len());
        for (i, out) in outs.iter().enumerate() {
            let utxo_id = tx.tx_id.prefix(&[u64::try_from(i).expect("index")]);
            let utxo = backend
                .get_utxo(PLATFORM_CHAIN_ID, utxo_id)
                .expect("produced UTXO recorded");
            assert_eq!(utxo.asset_id, out.asset_id);
            assert_eq!(utxo.out, out.out);
        }
    }

    /// `make_wallet` fetches the chain contexts (network id, blockchain ids,
    /// AVAX asset, fees, 2× gas price), the UTXOs and the requested owners.
    #[tokio::test]
    async fn make_wallet_fetches_state() {
        let key = test_key();
        let addr = key.public_key().address();
        let eth_addr = key.public_key().eth_address();
        let owner = OutputOwners::new(0, 1, vec![addr]);

        let mock = Arc::new(Mock {
            owners: BTreeMap::from([(subnet_id(), owner.clone())]),
            ..Mock::default()
        });
        {
            let mut utxos = mock.utxos.lock().expect("lock");
            utxos.insert(
                ('P', PLATFORM_CHAIN_ID),
                vec![p_utxo_bytes(1, 9 * AVAX, addr)],
            );
            utxos.insert(('X', x_chain_id()), vec![avm_utxo_bytes(2, 5 * AVAX, addr)]);
            utxos.insert(('C', x_chain_id()), vec![avm_utxo_bytes(3, 2 * AVAX, addr)]);
        }

        let wallet = make_wallet(
            &clients(&mock),
            Keychain::new(vec![key]),
            &WalletConfig {
                subnet_ids: vec![subnet_id()],
                ..WalletConfig::default()
            },
        )
        .await
        .expect("make_wallet");

        // Contexts (Go p/x/c NewContextFromClients).
        let p_ctx = *wallet.p().builder().context();
        assert_eq!(p_ctx.network_id, UNIT_TEST_ID);
        assert_eq!(p_ctx.avax_asset_id, avax_asset_id());
        assert_eq!(p_ctx.complexity_weights, WEIGHTS);
        assert_eq!(p_ctx.gas_price, 2 * GAS_PRICE);
        let x_ctx = *wallet.x().builder().context();
        assert_eq!(x_ctx.blockchain_id, x_chain_id());
        assert_eq!(x_ctx.base_tx_fee, BASE_TX_FEE);
        assert_eq!(x_ctx.create_asset_tx_fee, CREATE_ASSET_TX_FEE);
        let c_ctx = *wallet.c().builder().context();
        assert_eq!(c_ctx.blockchain_id, c_chain_id());
        assert_eq!(c_ctx.avax_asset_id, avax_asset_id());

        // Balances reflect the fetched UTXO / account state.
        let p_balance = wallet
            .p()
            .builder()
            .get_balance(&opts())
            .expect("p balance");
        assert_eq!(p_balance.get(&avax_asset_id()), Some(&(9 * AVAX)));
        let x_balance = wallet
            .x()
            .builder()
            .get_ft_balance(&opts())
            .expect("x balance");
        assert_eq!(x_balance.get(&avax_asset_id()), Some(&(5 * AVAX)));
        assert_eq!(wallet.c().builder().get_balance(&opts()), ETH_BALANCE_WEI);
        assert_eq!(
            wallet
                .c()
                .builder()
                .get_importable_balance(x_chain_id(), &opts())
                .expect("c importable"),
            2 * AVAX,
        );
        assert_eq!(wallet.c().backend().nonce(&eth_addr), ETH_NONCE);

        // The requested owners were fetched into the P backend.
        assert_eq!(wallet.p().backend().get_owner(subnet_id()), Some(owner));
    }

    /// A P→X export records the exported UTXO into the X wallet's view of the
    /// shared store (Go: one `common.UTXOs` across all chain wallets), and the
    /// follow-up X import consumes it.
    #[tokio::test]
    async fn cross_chain_export_records_into_destination_backend() {
        let key = test_key();
        let addr = key.public_key().address();
        let owner = OutputOwners::new(0, 1, vec![addr]);
        let mock = Arc::new(Mock::default());
        mock.utxos.lock().expect("lock").insert(
            ('P', PLATFORM_CHAIN_ID),
            vec![p_utxo_bytes(1, 9 * AVAX, addr)],
        );

        let wallet = make_wallet(
            &clients(&mock),
            Keychain::new(vec![key]),
            &WalletConfig::default(),
        )
        .await
        .expect("make_wallet");

        let exported = vec![ava_platformvm::txs::components::TransferableOutput {
            asset_id: avax_asset_id(),
            out: ava_platformvm::txs::components::Output::Transfer(TransferOutput::new(
                AVAX,
                owner.clone(),
            )),
        }];
        let export = wallet
            .p()
            .issue_export_tx(x_chain_id(), exported, &opts())
            .await
            .expect("export");

        // The exported UTXO is visible to the X wallet (source = P).
        let x_backend = wallet.x().backend();
        let imported = x_backend.utxos(PLATFORM_CHAIN_ID);
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].tx_id, export.tx_id);
        assert_eq!(imported[0].asset_id, avax_asset_id());

        // The X import consumes it and records the produced output locally.
        let import = wallet
            .x()
            .issue_import_tx(PLATFORM_CHAIN_ID, owner, &opts())
            .await
            .expect("import");
        assert_eq!(mock.issued('X'), vec![import.bytes.to_vec()]);
        assert!(x_backend.utxos(PLATFORM_CHAIN_ID).is_empty());
        let local = x_backend.utxos(x_chain_id());
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].tx_id, import.tx_id);
    }

    /// The C facade resolves `WithBaseFee` (override) or estimates via the eth
    /// client (Go `wallet.baseFee`), and `AcceptAtomicTx` updates accounts +
    /// the shared store.
    #[tokio::test]
    async fn c_facade_resolves_base_fee_and_records() {
        let key = test_key();
        let addr = key.public_key().address();
        let eth_addr = key.public_key().eth_address();
        let mock = Arc::new(Mock::default());
        mock.utxos
            .lock()
            .expect("lock")
            .insert(('C', x_chain_id()), vec![avm_utxo_bytes(2, 2 * AVAX, addr)]);

        let wallet = make_wallet(
            &clients(&mock),
            Keychain::new(vec![key]),
            &WalletConfig::default(),
        )
        .await
        .expect("make_wallet");

        // Import: no WithBaseFee -> the facade estimates over the eth client.
        let import = wallet
            .c()
            .issue_import_tx(x_chain_id(), eth_addr, &opts())
            .await
            .expect("import");
        assert_eq!(*mock.estimate_calls.lock().expect("lock"), 1);
        assert_eq!(mock.issued('C'), vec![import.bytes.clone()]);
        // The imported UTXO left the shared store; the account was credited
        // with amount × 10^9 wei.
        let backend = wallet.c().backend();
        assert!(backend.utxos(x_chain_id()).is_empty());
        let credited = backend.balance(&eth_addr);
        let ava_evm::atomic::tx::AtomicTx::Import(ref utx) = import.unsigned else {
            panic!("expected import");
        };
        let out_amount = u128::from(utx.outs[0].amount);
        assert_eq!(credited, ETH_BALANCE_WEI + out_amount * 1_000_000_000);

        // Export: WithBaseFee overrides -> no further estimate calls.
        let export = wallet
            .c()
            .issue_export_tx(
                x_chain_id(),
                vec![TransferOutput::new(
                    AVAX,
                    OutputOwners::new(0, 1, vec![addr]),
                )],
                &[
                    TxOption::MinIssuanceTime(MIN_ISSUANCE_TIME),
                    TxOption::BaseFee(BASE_FEE_ESTIMATE),
                ],
            )
            .await
            .expect("export");
        assert_eq!(*mock.estimate_calls.lock().expect("lock"), 1);
        // The exported UTXO is visible to the X wallet; the nonce advanced.
        let x_utxos = wallet.x().backend().utxos(c_chain_id());
        assert_eq!(x_utxos.len(), 1);
        assert_eq!(x_utxos[0].tx_id, export.tx_id);
        assert_eq!(backend.nonce(&eth_addr), ETH_NONCE + 1);
    }

    /// X `OperationTx` is still unsupported (no typed fx operations in
    /// `ava-avm`): the facade surfaces the signer's `UnsupportedTxType`.
    #[tokio::test]
    async fn x_operation_tx_unsupported() {
        let key = test_key();
        let mock = Arc::new(Mock::default());
        let wallet = make_wallet(
            &clients(&mock),
            Keychain::new(vec![key]),
            &WalletConfig::default(),
        )
        .await
        .expect("make_wallet");

        let result = wallet
            .x()
            .issue_unsigned_tx(
                ava_avm::txs::UnsignedTx::Operation(ava_avm::txs::OperationTx::default()),
                &opts(),
            )
            .await;
        assert_matches!(result, Err(Error::UnsupportedTxType));
    }
}

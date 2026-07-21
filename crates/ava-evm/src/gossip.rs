// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain tx gossip: `GossipEthTx` + its marshaller + a bloom-backed
//! `ava_p2p::gossip::Set` over [`crate::mempool::EvmMempool`] (cchain-tx-gossip
//! design doc, task 11).
//!
//! Ports coreth `plugin/evm/eth_gossiper.go` (NOTE: the task brief names this
//! file `gossip.go`; as of the pinned `~/avalanchego` checkout the C-Chain eth
//! tx gossip lives in `eth_gossiper.go` — `gossip.go` does not exist in
//! `plugin/evm`, only `gossip_test.go` and `plugin/evm/atomic/gossip.go`, the
//! unrelated atomic-tx gossip. All line cites below are against
//! `eth_gossiper.go`):
//!
//! - [`GossipEthTx`] = Go `GossipEthTx` (`eth_gossiper.go:157-163`): wraps one
//!   tx; `GossipID` = `ids.ID(tx.Tx.Hash())`.
//! - [`EthTxMarshaller`] = Go `GossipEthTxMarshaller` (`eth_gossiper.go:143-155`):
//!   `MarshalGossip` = `tx.Tx.MarshalBinary()` (EIP-2718 encode); `UnmarshalGossip`
//!   = `tx.Tx.UnmarshalBinary(bytes)` (EIP-2718 decode).
//! - [`EthTxGossipSet`] = Go `GossipEthTxPool` (`eth_gossiper.go:58-141`):
//!   `Add`/`Has`/`Iterate`/`BloomFilter` backed by the mempool + a
//!   `gossip.BloomFilter`.
//!
//! ## Bloom parameters (coreth exact values, cited)
//!
//! `NewGossipEthTxPool` (`eth_gossiper.go:39-46`) builds its bloom filter with:
//! ```go
//! bloom, err := gossip.NewBloomFilter(
//!     registerer,
//!     "eth_tx_bloom_filter",
//!     config.TxGossipBloomMinTargetElements,
//!     config.TxGossipBloomTargetFalsePositiveRate,
//!     config.TxGossipBloomResetFalsePositiveRate,
//! )
//! ```
//! and those constants are (`plugin/evm/config/constants.go:6-10`):
//! ```go
//! const (
//!     TxGossipBloomMinTargetElements       = 8 * 1024
//!     TxGossipBloomTargetFalsePositiveRate = 0.01
//!     TxGossipBloomResetFalsePositiveRate  = 0.05
//!     TxGossipBloomChurnMultiplier         = 3
//! )
//! ```
//! `TxGossipBloomChurnMultiplier` is not a bloom-constructor argument; it
//! scales the reset size hint (`eth_gossiper.go:94`):
//! ```go
//! optimalElements := (g.mempool.PendingSize(txpool.PendingFilter{}) + len(pendingTxs.Txs)) * config.TxGossipBloomChurnMultiplier
//! ```
//! — [`EthTxGossipSet::add`] mirrors this: `count_hint = mempool.len() *
//! TX_GOSSIP_BLOOM_CHURN_MULTIPLIER`.
//!
//! ## Lock order (documented, per the task-6 `PushGossiper::drain_queue`
//! deadlock note: `parking_lot` is non-reentrant)
//!
//! [`EthTxGossipSet`] holds two locks: the shared `mempool` and its own
//! `bloom`. They are **never held simultaneously** — every method takes at
//! most one of the two locks at a time, dropping it before (if needed) taking
//! the other. [`EthTxGossipSet::add`] in particular: (1) lock `mempool`,
//! admit, unlock; (2) lock `mempool` again just to snapshot `len()` +
//! `iterate()` into a local `Vec<Id>`, unlock; (3) lock `bloom`, add + maybe
//! reset (refilled from the already-collected `Vec`, no nested mempool call),
//! unlock. This also means `Set::iterate`/`Set::has`/`Set::add` never call
//! back into a `PushGossiper` synchronously while holding either lock.
//!
//! **Bound the `mempool` lock's hold time, not just its nesting (task 16
//! live-debugging fix).** [`EthTxGossipSet::iterate`] follows the same
//! snapshot-then-release shape as `add`'s step (2): the pull-request handler
//! (the only production caller) marshals every visited tx into the response,
//! work whose cost scales with pool size — holding `mempool` for that whole
//! pass would hold it for exactly as long as the push loop's own
//! `EvmMempool::take_gossip_outbox`/`PushGossiper::drain_push` need the SAME
//! lock every cycle (`ava_p2p::gossip::push`'s module doc), so a busy pull
//! responder under sustained concurrent load directly lengthens the push
//! loop's worst-case wait. `iterate` now clones every pooled tx into a `Vec`
//! under the lock, then runs the visitor over that snapshot with the lock
//! already released.

use std::sync::Arc;

use ava_evm_reth::{
    Address, B256, Decodable2718, Encodable2718, RecoveredTx, SignerRecoverable, TransactionSigned,
    U256,
};
use ava_p2p::gossip::bloom::BloomSet;
use ava_p2p::gossip::{Gossipable, Marshaller, Set};
use ava_types::id::Id;
use parking_lot::Mutex;

use crate::error::Error;
use crate::mempool::{AdmissionRules, EvmMempool, SenderAccount};
use crate::state::FirewoodStateProvider;

/// Minimum target elements a bloom filter is sized for (coreth
/// `plugin/evm/config/constants.go:7` `TxGossipBloomMinTargetElements = 8 *
/// 1024`).
pub const TX_GOSSIP_BLOOM_MIN_TARGET_ELEMENTS: usize = 8 * 1024;
/// Target false-positive probability (coreth
/// `plugin/evm/config/constants.go:8` `TxGossipBloomTargetFalsePositiveRate =
/// 0.01`).
pub const TX_GOSSIP_BLOOM_TARGET_FPP: f64 = 0.01;
/// False-positive probability that triggers a bloom reset (coreth
/// `plugin/evm/config/constants.go:9` `TxGossipBloomResetFalsePositiveRate =
/// 0.05`).
pub const TX_GOSSIP_BLOOM_RESET_FPP: f64 = 0.05;
/// Multiplier applied to the current pending count when sizing a bloom reset
/// (coreth `plugin/evm/config/constants.go:10` `TxGossipBloomChurnMultiplier
/// = 3`, consumed at `eth_gossiper.go:94`).
pub const TX_GOSSIP_BLOOM_CHURN_MULTIPLIER: usize = 3;

/// A gossipable Eth tx (Go `GossipEthTx`, `eth_gossiper.go:157-163`).
pub struct GossipEthTx(pub RecoveredTx);

impl Gossipable for GossipEthTx {
    /// `ids.ID(tx.Tx.Hash())` (`eth_gossiper.go:161-163`): the tx hash,
    /// reinterpreted byte-for-byte as an [`Id`] (both are 32 bytes).
    fn gossip_id(&self) -> Id {
        Id::from(self.0.hash().0)
    }
}

/// Marshals a [`GossipEthTx`] to/from its EIP-2718 envelope bytes (Go
/// `GossipEthTxMarshaller`, `eth_gossiper.go:143-155`).
pub struct EthTxMarshaller;

impl Marshaller<GossipEthTx> for EthTxMarshaller {
    /// `MarshalGossip` = `tx.Tx.MarshalBinary()` (`eth_gossiper.go:145-147`) —
    /// the EIP-2718 typed-envelope encoding.
    fn marshal(&self, t: &GossipEthTx) -> ava_p2p::error::Result<Vec<u8>> {
        Ok(t.0.encoded_2718())
    }

    /// `UnmarshalGossip` = `tx.Tx.UnmarshalBinary(bytes)`
    /// (`eth_gossiper.go:149-155`) — decode the EIP-2718 envelope, then
    /// recover the signer (mirroring `rpc::eth::send_raw_transaction`'s
    /// decode + recover pair).
    fn unmarshal(&self, bytes: &[u8]) -> ava_p2p::error::Result<GossipEthTx> {
        let mut buf = bytes;
        let tx = TransactionSigned::decode_2718(&mut buf)
            .map_err(|e| ava_p2p::Error::Decode(e.to_string()))?;
        let recovered = tx
            .try_into_recovered()
            .map_err(|e| ava_p2p::Error::Decode(e.to_string()))?;
        Ok(GossipEthTx(recovered))
    }
}

/// Resolves a sender's current on-chain nonce/balance for gossip-tx
/// admission (the `eth_sendRawTransaction` `view_tip` + `read_account`
/// pattern, `rpc/eth.rs:296-320`). Implemented in Task 12 by the VM's state
/// handle; a fixed-answer test double stands in here.
pub trait SenderAccountReader: Send + Sync {
    /// Looks up `addr`'s current [`SenderAccount`].
    ///
    /// # Errors
    /// Returns an error if the account lookup fails (e.g. a Firewood read
    /// error in the real implementation).
    fn sender_account(&self, addr: &Address) -> crate::error::Result<SenderAccount>;
}

/// The Task 12 [`SenderAccountReader`] implementation: resolves a sender's
/// current nonce/balance from the VM's shared committed Firewood state,
/// reusing the exact `view_tip()` + `basic_account()` read pattern
/// `rpc/eth.rs`'s `send_raw_transaction`/`read_account` use
/// (`rpc/eth.rs:296-320`) — gossip-received tx admission checks the SAME
/// on-chain account state the local RPC submission path does.
pub struct VmSenderAccountReader(Arc<FirewoodStateProvider>);

impl VmSenderAccountReader {
    /// Builds a reader over the VM's shared Firewood state handle.
    #[must_use]
    pub fn new(state: Arc<FirewoodStateProvider>) -> Self {
        Self(state)
    }
}

impl SenderAccountReader for VmSenderAccountReader {
    /// # Errors
    /// Returns an error if the Firewood tip view or the account read fails.
    fn sender_account(&self, addr: &Address) -> crate::error::Result<SenderAccount> {
        use ava_evm_reth::AccountReader;

        let view = self.0.view_tip()?;
        let account = view.basic_account(addr)?;
        Ok(SenderAccount {
            nonce: account.as_ref().map_or(0, |a| a.nonce),
            balance: account.as_ref().map_or(U256::ZERO, |a| a.balance),
        })
    }
}

/// A bloom-backed [`Set<GossipEthTx>`] over [`EvmMempool`] (Go
/// `GossipEthTxPool`, `eth_gossiper.go:58-141`). See the module docs for the
/// bloom constants (cited) and the lock-order rule between `mempool` and
/// `bloom`.
pub struct EthTxGossipSet {
    mempool: Arc<Mutex<EvmMempool>>,
    accounts: Arc<dyn SenderAccountReader>,
    rules: AdmissionRules,
    bloom: Mutex<BloomSet>,
}

impl EthTxGossipSet {
    /// Builds a new set over `mempool`, using `accounts` to resolve sender
    /// state and `rules` for admission policy. The bloom filter is sized per
    /// the coreth constants (see module docs).
    ///
    /// # Errors
    /// Returns an error if the initial bloom filter could not be constructed.
    pub fn new(
        mempool: Arc<Mutex<EvmMempool>>,
        accounts: Arc<dyn SenderAccountReader>,
        rules: AdmissionRules,
    ) -> crate::error::Result<Self> {
        let bloom = BloomSet::new(
            TX_GOSSIP_BLOOM_MIN_TARGET_ELEMENTS,
            TX_GOSSIP_BLOOM_TARGET_FPP,
            TX_GOSSIP_BLOOM_RESET_FPP,
        )
        .map_err(|e| Error::GossipBloomInit(e.to_string()))?;
        Ok(Self {
            mempool,
            accounts,
            rules,
            bloom: Mutex::new(bloom),
        })
    }
}

impl Set<GossipEthTx> for EthTxGossipSet {
    /// Resolves the sender account, then admits via
    /// [`EvmMempool::add_remote`] (Go `GossipEthTxPool.Add`,
    /// `eth_gossiper.go:118-122`), then folds the id into the bloom filter
    /// and (per the coreth churn-multiplier rule) maybe resets it
    /// (`eth_gossiper.go:93-112`). See the module docs for the lock-order
    /// rule this follows (mempool and bloom locks are never held together).
    fn add(&self, t: GossipEthTx) -> ava_p2p::error::Result<()> {
        let GossipEthTx(tx) = t;
        let hash = *tx.hash();
        let address = tx.signer();

        let account = self
            .accounts
            .sender_account(&address)
            .map_err(|e| ava_p2p::Error::Set(e.to_string()))?;

        // (1) Mempool admission. The mempool lock is dropped at the end of
        // this block, before the bloom lock is ever taken.
        {
            let mut mempool = self.mempool.lock();
            mempool
                .add_remote(tx, &account, &self.rules)
                .map_err(|e| ava_p2p::Error::Set(e.to_string()))?;
        }
        // Permanent live-forensics hook (T16): `add_remote` is the ONLY path
        // by which a tx enters this pool from the network — block import
        // never populates the mempool — so this line is per-tx proof that
        // gossip (push or pull) delivered it, orderable against the sender's
        // own send log and the block-accept timestamp. T16's live runs
        // measured a 7ms pending window on this net; this log is what made
        // that diagnosable. Bounded: one line per gossip-admitted tx.
        tracing::debug!(%hash, "gossip: admitted remote tx into mempool");

        // (2) Snapshot the pool's current ids (a second, brief mempool lock,
        // dropped before the bloom lock below is taken) so the refill
        // callback below never needs the mempool lock while the bloom lock
        // is held.
        let id = Id::from(hash.0);
        let (count_hint, known_ids) = {
            let mempool = self.mempool.lock();
            let len = mempool.len();
            let mut ids = Vec::with_capacity(len);
            mempool.iterate(&mut |pooled| {
                ids.push(Id::from(pooled.hash().0));
                true
            });
            (len.saturating_mul(TX_GOSSIP_BLOOM_CHURN_MULTIPLIER), ids)
        };

        // (3) Bloom add + reset-if-needed, using only the local snapshot —
        // no mempool call while `bloom` is locked.
        //
        // A `reset_if_needed` failure is logged, not propagated: the mempool
        // admission above already succeeded, so this `add` must report
        // success regardless (bloom's own doc: on error, the existing filter
        // — which already has `id` added, just above — is left unchanged).
        // Mirrors Go's `Subscribe` event loop (`eth_gossiper.go:98-102`),
        // which logs `ResetBloomFilterIfNeeded`'s error and `continue`s
        // rather than failing the (separate, synchronous) `Add` call.
        let mut bloom = self.bloom.lock();
        bloom.add(&id);
        if let Err(err) = bloom.reset_if_needed(count_hint, &mut |add| {
            for known in &known_ids {
                add(known);
            }
        }) {
            tracing::warn!(error = %err, "failed to reset gossip bloom filter");
        }

        Ok(())
    }

    /// `Has` (`eth_gossiper.go:126-128`): whether `id` (reinterpreted as a
    /// tx hash) is still pooled.
    fn has(&self, id: &Id) -> bool {
        let hash = B256::new(*id.as_bytes());
        self.mempool.lock().contains(&hash)
    }

    /// `Iterate` (`eth_gossiper.go:130-134`): visits every pooled tx, wrapped
    /// as a [`GossipEthTx`].
    ///
    /// Snapshot-then-release (task 16 live-debugging fix): the only
    /// production caller is [`ava_p2p::gossip::handler::GossipHandler::app_request`],
    /// which calls `f` once per visited tx to marshal it (EIP-2718 encode)
    /// into the pull-request response — work whose cost scales with the
    /// pool's size, run for every inbound pull `AppRequest` a peer sends. An
    /// earlier version held `self.mempool`'s lock for the ENTIRE call
    /// (`self.mempool.lock().iterate(&mut |tx| f(...))`), i.e. for the whole
    /// visit-and-marshal pass, directly lengthening how long a busy pull
    /// responder holds the SAME lock [`Self::add`]/[`Self::has`] and — most
    /// importantly — the push loop's own `EvmMempool::take_gossip_outbox`/
    /// `drain_push` need every cycle (see the module doc's lock-order note
    /// and `ava_p2p::gossip::push`'s module doc). Under sustained concurrent
    /// pull load this directly extends the push loop's worst-case wait for
    /// the mempool lock. This now takes the lock only long enough to clone
    /// every pooled tx into a local `Vec` (mirrors [`Self::add`]'s own
    /// pre-collect-then-release pattern), then runs `f` over the snapshot
    /// with the lock already released.
    fn iterate(&self, f: &mut dyn FnMut(&GossipEthTx) -> bool) {
        let snapshot: Vec<GossipEthTx> = {
            let mempool = self.mempool.lock();
            let mut items = Vec::with_capacity(mempool.len());
            mempool.iterate(&mut |tx| {
                items.push(GossipEthTx(tx.clone()));
                true
            });
            items
        };
        for item in &snapshot {
            if !f(item) {
                break;
            }
        }
    }

    /// `BloomFilter` (`eth_gossiper.go:136-141`): the current
    /// `(bloom_bytes, salt)`.
    fn get_filter(&self) -> (Vec<u8>, Vec<u8>) {
        self.bloom.lock().marshal()
    }
}

#[cfg(test)]
mod tests {
    use ava_crypto::secp256k1::PrivateKey;
    use ava_evm_reth::{
        Bytes, EvmSignature, SignableTransaction, SignerRecoverable, TxKind, TxLegacy, U256,
    };
    use ava_utils::bloom::ReadFilter;

    use super::*;

    /// Matches `AdmissionRules::default()`'s `chain_id` (the mempool test
    /// module's own `CHAIN_ID`, `mempool.rs`).
    const CHAIN_ID: u64 = 43_112;

    fn key(byte: u8) -> PrivateKey {
        PrivateKey::from_bytes(&[byte; 32]).expect("PrivateKey::from_bytes")
    }

    fn recipient() -> Address {
        Address::repeat_byte(0xEE)
    }

    /// A protected (EIP-155, `CHAIN_ID`) legacy tx signed by sender key
    /// `byte` (repeat of the `mempool.rs` tx-builder helper — test-file
    /// convention is repeat-don't-import).
    fn signed_legacy_tx_from(byte: u8, nonce: u64, gas_price: u128, gas: u64) -> RecoveredTx {
        let tx = TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce,
            gas_price,
            gas_limit: gas,
            to: TxKind::Call(recipient()),
            value: U256::from(1u64),
            input: Bytes::new(),
        };
        let sig_hash = tx.signature_hash();
        let rsv = key(byte).sign_hash(&sig_hash.0).expect("sign_hash");
        let r = U256::from_be_slice(&rsv[..32]);
        let s = U256::from_be_slice(&rsv[32..64]);
        let sig = EvmSignature::new(r, s, rsv[64] == 1);
        TransactionSigned::Legacy(tx.into_signed(sig))
            .try_into_recovered()
            .expect("try_into_recovered")
    }

    /// A tx protected for a chain id other than [`CHAIN_ID`] — rejected by
    /// [`EvmMempool::add_remote`]'s chain-id check without touching nonce
    /// or balance.
    fn signed_legacy_tx_wrong_chain(byte: u8, nonce: u64) -> RecoveredTx {
        let tx = TxLegacy {
            chain_id: Some(CHAIN_ID + 1),
            nonce,
            gas_price: 2_000_000_000,
            gas_limit: 21_000,
            to: TxKind::Call(recipient()),
            value: U256::from(1u64),
            input: Bytes::new(),
        };
        let sig_hash = tx.signature_hash();
        let rsv = key(byte).sign_hash(&sig_hash.0).expect("sign_hash");
        let r = U256::from_be_slice(&rsv[..32]);
        let s = U256::from_be_slice(&rsv[32..64]);
        let sig = EvmSignature::new(r, s, rsv[64] == 1);
        TransactionSigned::Legacy(tx.into_signed(sig))
            .try_into_recovered()
            .expect("try_into_recovered")
    }

    /// A [`SenderAccountReader`] test double returning a fixed account for
    /// every address (ample nonce headroom + balance).
    struct FixedAccountReader(SenderAccount);

    impl SenderAccountReader for FixedAccountReader {
        fn sender_account(&self, _addr: &Address) -> crate::error::Result<SenderAccount> {
            Ok(self.0)
        }
    }

    fn rich_account() -> SenderAccount {
        SenderAccount {
            nonce: 0,
            balance: U256::from(10u128.pow(19)),
        }
    }

    fn new_set() -> EthTxGossipSet {
        EthTxGossipSet::new(
            Arc::new(Mutex::new(EvmMempool::new(16))),
            Arc::new(FixedAccountReader(rich_account())),
            AdmissionRules {
                chain_id: CHAIN_ID,
                ..AdmissionRules::default()
            },
        )
        .expect("EthTxGossipSet::new")
    }

    #[test]
    fn set_add_admits_valid_remote_tx() {
        let set = new_set();
        let tx = signed_legacy_tx_from(0x11, 0, 2_000_000_000, 21_000);
        let hash = *tx.hash();
        let id = Id::from(hash.0);

        set.add(GossipEthTx(tx)).expect("Set::add");

        assert!(
            set.has(&id),
            "Set::has() must report the admitted tx's gossip id as known"
        );
        let mut seen = Vec::new();
        set.iterate(&mut |g| {
            seen.push(g.0.hash().0);
            true
        });
        assert_eq!(
            seen,
            vec![hash.0],
            "Set::iterate() must yield exactly the admitted tx"
        );
    }

    #[test]
    fn set_add_rejects_wrong_chain_id_without_poisoning() {
        let set = new_set();

        let bad = signed_legacy_tx_wrong_chain(0x11, 0);
        let err = set.add(GossipEthTx(bad)).unwrap_err();
        assert!(err.to_string().contains("chain"), "got: {err}");

        // The failed add must not have locked up the mempool or bloom lock
        // (no poisoning/deadlock) — a subsequent valid tx from a fresh
        // sender must still be admitted normally.
        let good = signed_legacy_tx_from(0x22, 0, 2_000_000_000, 21_000);
        let good_hash = *good.hash();
        set.add(GossipEthTx(good)).expect("good tx must admit");
        assert!(set.has(&Id::from(good_hash.0)));
    }

    #[test]
    fn get_filter_readable_and_contains_added() {
        let set = new_set();
        let tx = signed_legacy_tx_from(0x11, 0, 2_000_000_000, 21_000);
        let id = Id::from(tx.hash().0);

        set.add(GossipEthTx(tx)).expect("Set::add");

        let (bloom_bytes, salt) = set.get_filter();
        let read_filter =
            ReadFilter::parse(&bloom_bytes).expect("ReadFilter::parse of get_filter() bytes");
        assert!(
            read_filter.contains_key(id.as_bytes(), &salt),
            "the marshaled filter must be readable and contain the added id"
        );
    }

    #[test]
    fn marshaller_round_trips_2718() {
        let tx = signed_legacy_tx_from(0x11, 3, 5_000_000_000, 21_000);
        let hash = *tx.hash();
        let signer = tx.signer();

        let marshaller = EthTxMarshaller;
        let bytes = marshaller
            .marshal(&GossipEthTx(tx))
            .expect("EthTxMarshaller::marshal");
        let GossipEthTx(round_tripped) = marshaller
            .unmarshal(&bytes)
            .expect("EthTxMarshaller::unmarshal");

        assert_eq!(
            *round_tripped.hash(),
            hash,
            "round trip must preserve the tx hash"
        );
        assert_eq!(
            round_tripped.signer(),
            signer,
            "round trip must recover the same signer"
        );
    }
}

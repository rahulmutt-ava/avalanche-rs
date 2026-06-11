// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-call build options — port of `wallet/subnet/primary/common/options.go`.
//!
//! Go threads a variadic `...common.Option` (function options); the Rust port
//! uses a [`TxOption`] enum collected into [`Options`] (the spec's `Option_`).
//! The build-affecting options plus the issuance option `WithAssumeDecided`
//! are ported; `WithContext` / `WithPollFrequency` and the issuance /
//! confirmation handlers are transport concerns deferred with the live
//! `ava-api` clients (see `tests/PORTING.md`).

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use ava_secp256k1fx::OutputOwners;
use ava_types::short_id::ShortId;

/// One build option (Go `common.Option`).
#[derive(Clone, Debug)]
pub enum TxOption {
    /// `WithCustomAddresses` — override the builder's spendable address set.
    CustomAddresses(BTreeSet<ShortId>),
    /// `WithCustomEthAddresses` — override the C-Chain eth address set.
    CustomEthAddresses(BTreeSet<[u8; 20]>),
    /// `WithBaseFee` — override the C-Chain base fee (wei).
    BaseFee(u128),
    /// `WithMinIssuanceTime` — the unix time the tx will be issued no earlier
    /// than (locktime comparisons).
    MinIssuanceTime(u64),
    /// `WithStakeableLocked` — allow burning stakeable-locked outputs.
    AllowStakeableLocked,
    /// `WithChangeOwner` — the owner of unlocked AVAX change.
    ChangeOwner(OutputOwners),
    /// `WithMemo` — the tx memo bytes.
    Memo(Vec<u8>),
    /// `WithAssumeDecided` — skip polling for acceptance after issuance and
    /// record the tx in the backend immediately.
    AssumeDecided,
}

/// The collected options (Go `common.Options`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    custom_addresses: Option<BTreeSet<ShortId>>,
    custom_eth_addresses: Option<BTreeSet<[u8; 20]>>,
    base_fee: Option<u128>,
    min_issuance_time: Option<u64>,
    allow_stakeable_locked: bool,
    change_owner: Option<OutputOwners>,
    memo: Vec<u8>,
    assume_decided: bool,
}

impl Options {
    /// `common.NewOptions` — folds the option list.
    #[must_use]
    pub fn new(options: &[TxOption]) -> Self {
        let mut o = Options::default();
        for op in options {
            match op {
                TxOption::CustomAddresses(addrs) => o.custom_addresses = Some(addrs.clone()),
                TxOption::CustomEthAddresses(addrs) => {
                    o.custom_eth_addresses = Some(addrs.clone());
                }
                TxOption::BaseFee(fee) => o.base_fee = Some(*fee),
                TxOption::MinIssuanceTime(t) => o.min_issuance_time = Some(*t),
                TxOption::AllowStakeableLocked => o.allow_stakeable_locked = true,
                TxOption::ChangeOwner(owner) => o.change_owner = Some(owner.clone()),
                TxOption::Memo(memo) => o.memo = memo.clone(),
                TxOption::AssumeDecided => o.assume_decided = true,
            }
        }
        o
    }

    /// `Options.Addresses` — the custom set, or `default_addresses`.
    #[must_use]
    pub fn addresses(&self, default_addresses: &BTreeSet<ShortId>) -> BTreeSet<ShortId> {
        self.custom_addresses
            .clone()
            .unwrap_or_else(|| default_addresses.clone())
    }

    /// `Options.EthAddresses` — the custom set, or `default_addresses`.
    #[must_use]
    pub fn eth_addresses(&self, default_addresses: &BTreeSet<[u8; 20]>) -> BTreeSet<[u8; 20]> {
        self.custom_eth_addresses
            .clone()
            .unwrap_or_else(|| default_addresses.clone())
    }

    /// `Options.BaseFee` — the custom base fee, or `default_base_fee`.
    #[must_use]
    pub fn base_fee(&self, default_base_fee: u128) -> u128 {
        self.base_fee.unwrap_or(default_base_fee)
    }

    /// `Options.BaseFee(nil)` — the custom base fee if one was set. The C
    /// wallet facade estimates over the eth client when absent (Go
    /// `wallet.baseFee`).
    #[must_use]
    pub fn base_fee_override(&self) -> Option<u128> {
        self.base_fee
    }

    /// `Options.MinIssuanceTime` — defaults to the current unix time (matches
    /// Go; pass [`TxOption::MinIssuanceTime`] for deterministic builds).
    #[must_use]
    pub fn min_issuance_time(&self) -> u64 {
        self.min_issuance_time.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default()
        })
    }

    /// `Options.AllowStakeableLocked`.
    #[must_use]
    pub fn allow_stakeable_locked(&self) -> bool {
        self.allow_stakeable_locked
    }

    /// `Options.ChangeOwner` — the custom change owner, or `default_owner`.
    #[must_use]
    pub fn change_owner(&self, default_owner: OutputOwners) -> OutputOwners {
        self.change_owner.clone().unwrap_or(default_owner)
    }

    /// `Options.Memo`.
    #[must_use]
    pub fn memo(&self) -> &[u8] {
        &self.memo
    }

    /// `Options.AssumeDecided`.
    #[must_use]
    pub fn assume_decided(&self) -> bool {
        self.assume_decided
    }
}

/// `common.UnionOptions` — `first` then `second` (later options win when
/// folded by [`Options::new`]).
#[must_use]
pub fn union_options(first: &[TxOption], second: &[TxOption]) -> Vec<TxOption> {
    let mut union = Vec::with_capacity(first.len().saturating_add(second.len()));
    union.extend_from_slice(first);
    union.extend_from_slice(second);
    union
}

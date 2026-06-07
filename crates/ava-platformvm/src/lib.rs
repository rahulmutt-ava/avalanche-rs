// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-platformvm` — the P-Chain (PlatformVM), port of Go `vms/platformvm`.
//!
//! Tier T4 (VMs). Owning specs: `specs/08-platformvm-pchain.md` (PRIMARY), plus
//! `19` (bootstrap/state-sync), `20` (P-Chain Warp signing), `21` (reward +
//! P-Chain fee math), `23` (genesis assembly).
//!
//! The P-Chain is the staking & metadata chain: it owns the validator/delegator
//! sets of every subnet (incl. the Primary Network), subnets & their
//! blockchains, the platform-chain AVAX UTXO set, import/export, the ACP-77 L1
//! validator lifecycle, BLS-signed Warp messages, and — critically — serves the
//! [`ava_validators::ValidatorState`] contract that all of consensus,
//! proposervm, uptime, and Warp depend on (specs 08 §7).
//!
//! Module layout mirrors the Go subpackages (`txs/`, `block/`, `state/`,
//! `reward/`, `validators/`, `warp/`, …); it is populated tier-by-tier across the
//! M4 wave plan (see `plan/M4-pchain.md`).

#![forbid(unsafe_code)]

// Dependencies declared up front per specs/08 §1 but not yet consumed by the
// skeleton; each is wired in by a later M4 wave task, which drops its silencer.
// `unused_crate_dependencies` (warn) would otherwise fire on the bare crate.
use arc_swap as _;
use async_trait as _;
use ava_codec_derive as _;
use ava_database as _;
use ava_utils as _;
use ava_validators as _;
use num_bigint as _;
use parking_lot as _;
use ruint as _;
use tokio as _;

// Dev-dependencies not yet exercised by this crate's in-crate tests; each is
// wired in by a later M4 task (proptest/rstest table tests, hex/serde_json
// golden vectors). Silence `unused_crate_dependencies` for the lib-test unit.
#[cfg(test)]
use assert_matches as _;
#[cfg(test)]
use hex as _;
#[cfg(test)]
use pretty_assertions as _;
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use rstest as _;
#[cfg(test)]
use serde as _;
#[cfg(test)]
use serde_json as _;

pub mod error;
pub mod txs;

pub use error::{Error, Result};

/// The single P-Chain codec version (`txs.CodecVersion`, specs 08 §2.1).
///
/// `0` is the only codec version that has ever existed; both the `Codec` and
/// `GenesisCodec` managers register their (identical) type IDs under it.
pub const CODEC_VERSION: u16 = 0;

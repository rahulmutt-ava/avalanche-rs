// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain feature-extension verification surface (`vms/avm` fx wiring,
//! specs 09 §4, §4.1).
//!
//! Each fx (secp256k1fx, nftfx, propertyfx) authorizes spends of the UTXOs it
//! owns. The avm verifier routes a parsed input/operation to the right fx (via
//! the `TypeToFxIndex` table built in M5.5) and calls into it to check the
//! supplied credential. This module defines that verification trait and houses
//! the concrete fx adapters; [`secp`] is the base (secp256k1fx) fx wired here in
//! M5.6.
//!
//! The heavy lifting — signature recovery, the threshold/locktime spend gate —
//! lives in [`ava_secp256k1fx::Fx::verify_credentials`] (specs 07 §4.3); the
//! adapters are thin and share one recover-cache per fx instance (09 §4.1).
//!
//! The nftfx / propertyfx adapters (M5.7/M5.8) expose their own inherent verify
//! methods over their concrete operation/utxo types. The [`dispatch`] module
//! (M5.9) holds the three heterogeneous fxs in a [`dispatch::FxKind`] enum and
//! routes a parsed value to its fx by codec type-id via the `TypeToFxIndex`
//! table (specs 09 §2.2, §4; FX-AVM-1).

pub mod dispatch;
pub mod secp;

use ava_secp256k1fx::{Credential, Input, MintOutput, TransferInput, TransferOutput};
use ava_vm::fx::UnsignedTx;

use crate::error::Result;

/// `fxs.Fx` — the avm-side spend-authorization surface a feature extension
/// exposes to the verifier (specs 09 §4).
///
/// Kept minimal and gated by a `bootstrapped` flag matching Go (verification is
/// skipped while replaying historical blocks). The secp256k1fx adapter
/// ([`secp::SecpFx`]) is the only implementor in M5.6; the nft/property adapters
/// (M5.7/M5.8) satisfy the same trait.
pub trait Fx {
    /// `VerifyTransfer(tx, in, cred, utxo)` — `Ok(())` iff `cred` authorizes
    /// spending the transfer-output `utxo` under `input` (amount equality + the
    /// multisig spend gate).
    ///
    /// # Errors
    /// Returns [`crate::Error::MismatchedAmounts`] when `utxo.amt != in.amt`, or
    /// the wrapped fx spend-gate sentinel ([`crate::Error::Fx`]) on a failed
    /// credential check.
    fn verify_transfer(
        &self,
        tx: &dyn UnsignedTx,
        input: &TransferInput,
        cred: &Credential,
        utxo: &TransferOutput,
    ) -> Result<()>;

    /// `VerifyOperation(tx, op, cred, utxo)` — `Ok(())` iff the produced mint
    /// output's owners equal the consumed `MintOutput` UTXO's owners and `cred`
    /// authorizes the mint input.
    ///
    /// # Errors
    /// Returns [`crate::Error::WrongMintCreated`] when the produced mint owners
    /// differ from the consumed UTXO's, or the wrapped fx spend-gate sentinel
    /// ([`crate::Error::Fx`]) on a failed credential check.
    fn verify_operation(
        &self,
        tx: &dyn UnsignedTx,
        mint_input: &Input,
        mint_output: &MintOutput,
        cred: &Credential,
        utxo: &MintOutput,
    ) -> Result<()>;
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `secp256k1fx.Fx.VerifyCredentials` — the multisig spend gate (specs 07 §4.3).
//!
//! Reproduced **bit-for-bit** with Go because it gates spending: locktime vs the
//! fx clock, `threshold == num_sigs` exactly, one signature per index, the
//! bootstrap skip, then `sha256(unsigned_tx)` recover → `ripemd160(sha256(pk))`
//! address compare at `addrs[index]`.

use std::sync::Arc;

use ava_crypto::secp256k1::PublicKey;
use ava_utils::clock::Clock;
use ava_vm::error::{Error, Result};
// The `UnsignedTx` boundary is owned by the fx framework (`ava-vm::fx`) so the
// secp256k1fx `FxInstance` and `verify_credentials` share one tx-bytes trait
// (specs 07 §4.1); re-exported for source-compatibility (incl. the blanket
// `Vec<u8>`/`&[u8]` impls used by the multisig proptests).
pub use ava_vm::fx::UnsignedTx;

use crate::types::{Credential, Input, OutputOwners};

/// `secp256k1fx.Fx` — the verification state (clock + bootstrapped flag).
///
/// Signature verification is disabled until [`Fx::bootstrapped`] is called,
/// matching Go (the node skips sig checks while replaying historical blocks).
pub struct Fx {
    clock: Arc<dyn Clock>,
    bootstrapped: bool,
}

impl Fx {
    /// Builds an [`Fx`] reading time through `clock`, signature verification
    /// **disabled** (still bootstrapping).
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            bootstrapped: false,
        }
    }

    /// `Bootstrapping()` — no-op (verification already disabled).
    pub fn bootstrapping(&mut self) {}

    /// `Bootstrapped()` — enables signature verification.
    pub fn bootstrapped(&mut self) {
        self.bootstrapped = true;
    }

    /// Whether signature verification is enabled.
    #[must_use]
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrapped
    }

    /// `VerifyCredentials(tx, in, cred, owner)` — `Ok(())` iff `cred` proves the
    /// owners assent to spending under `in` (specs 07 §4.3).
    ///
    /// # Errors
    /// Returns the matching fx sentinel ([`Error::Timelocked`],
    /// [`Error::TooManySigners`], [`Error::TooFewSigners`],
    /// [`Error::InputCredentialSignersMismatch`],
    /// [`Error::InputOutputIndexOutOfBounds`], [`Error::WrongSig`]) or a
    /// signature-recovery failure.
    pub fn verify_credentials(
        &self,
        unsigned_tx: &dyn UnsignedTx,
        input: &Input,
        cred: &Credential,
        owner: &OutputOwners,
    ) -> Result<()> {
        let num_sigs = input.sig_indices.len();

        // 1. locktime — must be matured against the fx clock (Go: `> Unix()`).
        if owner.locktime > self.clock.unix() {
            return Err(Error::Timelocked);
        }
        // 2. threshold must equal the number of supplied sig indices, exactly.
        //    The two-branch order matches Go (`<` ⇒ TooMany, `>` ⇒ TooFew).
        if owner.threshold < num_sigs as u32 {
            return Err(Error::TooManySigners);
        }
        if owner.threshold > num_sigs as u32 {
            return Err(Error::TooFewSigners);
        }
        // 3. one signature per sig index.
        if num_sigs != cred.sigs.len() {
            return Err(Error::InputCredentialSignersMismatch);
        }
        // 4. during bootstrap, skip signature verification (Go parity).
        if !self.bootstrapped {
            return Ok(());
        }

        let tx_hash = ava_crypto::hashing::sha256(unsigned_tx.bytes());
        // `cred.sigs[i]` pairs positionally with `sig_indices[i]`; the length
        // equality was checked above, so `zip` consumes both fully.
        for (&index, sig) in input.sig_indices.iter().zip(cred.sigs.iter()) {
            // 5. index must reference an existing owner address.
            let idx = index as usize;
            let Some(&expected) = owner.addrs.get(idx) else {
                return Err(Error::InputOutputIndexOutOfBounds);
            };
            // 6. recover the signer and compare its address to `addrs[index]`.
            let pk = PublicKey::recover_from_hash(&tx_hash, sig).map_err(|_| Error::WrongSig)?;
            if expected != pk.address() {
                return Err(Error::WrongSig);
            }
        }
        Ok(())
    }
}

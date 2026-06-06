// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/components/verify` — the `Verifiable` / `State` traits + `verify::all`
//! (specs 07 §3.2).
//!
//! Go's `IsState` / `IsNotState` marker split (which prevents an `OutputOwners`,
//! marked `IsNotState`, from being used where a `State` output is expected) is
//! encoded at the Rust type level: a type is a `State` iff it implements the
//! [`State`] trait. `init_ctx` carries the `ContextInitializable` hook used only
//! for JSON address formatting — it is **not** on the codec/consensus path.

use ava_snow::ChainContext;

use crate::error::Result;

/// `verify.Verifiable` — anything that can be structurally validated.
pub trait Verifiable {
    /// `Verify()` — structural validation. `Err` describes the first failure.
    ///
    /// # Errors
    /// Returns a [`crate::error::Error`] describing the validation failure.
    fn verify(&self) -> Result<()>;
}

/// `verify.State` = `ContextInitializable` + `Verifiable` + the `IsState` marker.
///
/// Only output types that may legally appear as a UTXO `Out` implement this.
/// `init_ctx` mirrors Go's `InitCtx`, used solely to seed JSON address
/// formatting; it must not influence codec bytes or consensus.
pub trait State: Verifiable + Send + Sync {
    /// `InitCtx(ctx)` — seed any context the type needs for JSON formatting.
    fn init_ctx(&self, ctx: &ChainContext);
}

/// `verify.All(..)` — verify each item, short-circuiting on the first error.
///
/// # Errors
/// Returns the first item's verification error.
pub fn all(items: &[&dyn Verifiable]) -> Result<()> {
    for v in items {
        v.verify()?;
    }
    Ok(())
}

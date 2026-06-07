// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.BaseTx` (type_id 34) — a plain fee-burn / transfer tx and the common
//! preamble every other tx embeds (specs 08 §2.2).

use std::cell::OnceCell;

use ava_codec::AvaCodec;

use crate::Error;
use crate::txs::components::{
    self, BaseTx as AvaxBaseTx, MAX_MEMO_SIZE, TransferableInput, TransferableOutput,
};

/// `txs.BaseTx` — the embedded `avax.BaseTx` plus a memoized syntactic-verify
/// flag (not serialized).
#[derive(AvaCodec, Clone, Debug, Default)]
pub struct BaseTx {
    /// The embedded `avax.BaseTx` (network/chain id, ins/outs, memo).
    #[codec]
    pub base: AvaxBaseTx,
    /// `SyntacticallyVerified` — a non-serialized memo of a successful
    /// [`BaseTx::syntactic_verify`].
    pub verified: OnceCell<()>,
}

impl PartialEq for BaseTx {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
    }
}

impl Eq for BaseTx {}

impl BaseTx {
    /// Builds a [`BaseTx`] over an `avax.BaseTx`.
    #[must_use]
    pub fn new(base: AvaxBaseTx) -> Self {
        Self {
            base,
            verified: OnceCell::new(),
        }
    }

    /// The tx outputs.
    #[must_use]
    pub fn outputs(&self) -> &[TransferableOutput] {
        &self.base.outs
    }

    /// The tx inputs.
    #[must_use]
    pub fn inputs(&self) -> &[TransferableInput] {
        &self.base.ins
    }

    /// `BaseTx.SyntacticVerify` — memo bound, each in/out verifies, outputs
    /// sorted, inputs sorted & unique (specs 08 §2.2). The network/chain id
    /// checks are deferred to semantic verification (they need a chain context).
    ///
    /// # Errors
    /// Returns [`Error::InvalidComponent`] (oversized memo / failed component),
    /// [`Error::OutputsNotSorted`], or [`Error::InputsNotSortedUnique`].
    pub fn syntactic_verify(&self) -> Result<(), Error> {
        if self.verified.get().is_some() {
            return Ok(());
        }
        if self.base.memo.len() > MAX_MEMO_SIZE {
            return Err(Error::InvalidComponent);
        }
        for out in &self.base.outs {
            out.verify()?;
        }
        for input in &self.base.ins {
            input.verify()?;
        }
        if !components::is_sorted_transferable_outputs(&self.base.outs) {
            return Err(Error::OutputsNotSorted);
        }
        if !components::is_sorted_and_unique_transferable_inputs(&self.base.ins) {
            return Err(Error::InputsNotSortedUnique);
        }
        let _ = self.verified.set(());
        Ok(())
    }
}

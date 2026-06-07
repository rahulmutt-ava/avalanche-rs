// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `platformvm.Factory` — constructs [`PlatformVm`] instances for the chain
//! manager (`vms/platformvm/factory.go`, specs 08 §1).
//!
//! Go's `Factory` embeds the chain `config.Internal` and `New()` returns a fresh
//! `&VM{Internal: ...}`. The Rust factory mirrors this: a zero-sized constructor
//! producing an uninitialized [`PlatformVm`] (the engine then drives
//! [`Vm::initialize`](crate::vm::PlatformVm), which seeds genesis and builds the
//! block + validator managers).
//!
//! ## Scope (M4.25)
//!
//! The factory does **not** implement the [`ava_chains::Factory`] trait (that
//! would invert the T4-VM → T6-services layering); the chain-creation pipeline
//! (M4.27 / M8) adapts this constructor. Per-network staking/fee `config.Internal`
//! plumbing lands with `ava-genesis` (M8) — until then the VM derives a
//! mainnet-staking [`Backend`](crate::txs::executor::Backend) from the chain
//! context at `initialize`.

use crate::vm::PlatformVm;

/// `platformvm.Factory` — creates fresh [`PlatformVm`] instances.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformVmFactory;

impl PlatformVmFactory {
    /// Builds a new factory.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// `Factory.New` — constructs a fresh, uninitialized [`PlatformVm`]. The
    /// caller (chain manager / engine) then calls
    /// [`Vm::initialize`](crate::vm::PlatformVm).
    #[must_use]
    pub fn new_vm(&self) -> PlatformVm {
        PlatformVm::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_constructs_a_vm() {
        let factory = PlatformVmFactory::new();
        // The freshly-built VM is uninitialized: it has no validator state yet.
        let vm = factory.new_vm();
        assert!(vm.validator_state().is_none());
    }
}

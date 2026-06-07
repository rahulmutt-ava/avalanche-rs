// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `avm.Factory` — constructs [`AvmVm`] instances for the chain manager
//! (`vms/avm/factory.go`, specs 09 §1).
//!
//! Go's `Factory` embeds the chain `config.Config` and `New()` returns a fresh
//! `&VM{}`. The Rust factory mirrors this: a zero-sized constructor producing an
//! uninitialized [`AvmVm`] (the engine then drives
//! [`Vm::initialize`](crate::vm::AvmVm), which seeds the genesis Snowman block
//! and builds the block manager + mempool + gossip handler). The per-chain
//! `config.Config` is parsed from the engine-supplied `config_bytes` inside
//! `initialize` (see [`crate::config::Config`]), so the factory itself carries no
//! state.
//!
//! ## Scope (M5.19)
//!
//! The factory does **not** implement the [`ava_chains`](ava_chains) `Factory`
//! trait (that would invert the T4-VM → T6-services layering); the chain-creation
//! pipeline (M8) adapts this constructor.

use crate::vm::AvmVm;

/// `avm.Factory` — creates fresh [`AvmVm`] instances.
#[derive(Clone, Copy, Debug, Default)]
pub struct AvmFactory;

impl AvmFactory {
    /// Builds a new factory.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// `Factory.New` — constructs a fresh, uninitialized [`AvmVm`]. The caller
    /// (chain manager / engine) then calls [`Vm::initialize`](crate::vm::AvmVm).
    #[must_use]
    pub fn new_vm(&self) -> AvmVm {
        AvmVm::new()
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::vm::AvmVm;
    use ava_vm::block::ChainVm;

    #[tokio::test]
    async fn factory_constructs_an_uninitialized_vm() {
        let factory = AvmFactory::new();
        let vm: AvmVm = factory.new_vm();
        // Uninitialized: a read op errors until `initialize` runs.
        let token = CancellationToken::new();
        assert!(vm.last_accepted(&token).await.is_err());
    }
}

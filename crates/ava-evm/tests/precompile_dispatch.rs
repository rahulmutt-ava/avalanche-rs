// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Dispatch + height-gating tests for [`AvaPrecompiles`] / [`PrecompileRegistry`]
//! (M6.21, spec 10 §8/§17.5/§17.11, G4/G10).
//!
//! These exercise the registry + provider plumbing ONLY — registration, the
//! fork+upgrade-gated `warm` set computed by `for_height`, `contains` /
//! `warm_addresses`, and the "dispatch to a registered stateful precompile when
//! warm, else fall through" decision. The actual warp/allowlist/feemanager
//! precompile bodies + the revm-context predicate threading are M6.22; here we
//! register a trivial dummy `StatefulPrecompile` to drive the dispatch path.

use std::sync::Arc;

use ava_evm::precompile::registry::{
    AvaPrecompiles, PrecompileCtx, PrecompileModule, PrecompileRegistry, PrecompileStateOps,
    StatefulPrecompile,
};
use ava_evm_reth::{Address, InterpreterResult, PrecompileError};

/// A test-only stateful precompile: records nothing, returns a fixed marker so a
/// dispatch can be observed. Body is irrelevant to M6.21 (M6.22 ports the real
/// ones); this only needs to be a registrable `StatefulPrecompile`.
struct DummyPrecompile;

impl StatefulPrecompile for DummyPrecompile {
    fn run(
        &self,
        _input: &[u8],
        _gas_limit: u64,
        _ctx: &PrecompileCtx,
        _state: &mut dyn PrecompileStateOps,
    ) -> Result<InterpreterResult, PrecompileError> {
        // Not exercised by M6.21 (no live revm context in this unit test).
        Err(PrecompileError::Fatal("dummy".into()))
    }
}

/// Address of our test module (an arbitrary high address well clear of the
/// standard Ethereum precompile range 0x01..=0x0a).
fn dummy_addr() -> Address {
    Address::from([0x42u8; 20])
}

/// A module gated on a timestamp `t >= activation`.
fn dummy_module(activation: u64) -> PrecompileModule {
    PrecompileModule {
        address: dummy_addr(),
        activation,
        precompile: Arc::new(DummyPrecompile),
    }
}

#[test]
fn dispatch_falls_through_and_gates_by_height() {
    // A registry with one module activated at timestamp 1_000.
    let mut registry = PrecompileRegistry::new();
    registry.register(dummy_module(1_000));
    let registry = Arc::new(registry);

    // Before activation: the module is NOT in the warm set, so a call to its
    // address must fall through (no stateful dispatch).
    let before = AvaPrecompiles::for_height(registry.clone(), 999);
    assert!(
        !before.contains_stateful(&dummy_addr()),
        "module must not be activated before its upgrade timestamp"
    );
    assert!(
        before.dispatch_stateful(&dummy_addr()).is_none(),
        "pre-activation: dispatch must fall through to the base set"
    );
    assert!(
        !before.warm_addresses_vec().contains(&dummy_addr()),
        "pre-activation address must not be warm"
    );

    // At/after activation: the module IS warm; a call to its address dispatches
    // to the registered stateful precompile rather than falling through.
    let after = AvaPrecompiles::for_height(registry.clone(), 1_000);
    assert!(
        after.contains_stateful(&dummy_addr()),
        "module must be activated at exactly its upgrade timestamp (inclusive)"
    );
    assert!(
        after.dispatch_stateful(&dummy_addr()).is_some(),
        "post-activation: dispatch must resolve the registered stateful precompile"
    );
    assert!(
        after.warm_addresses_vec().contains(&dummy_addr()),
        "post-activation address must be warm"
    );

    // An unregistered address never dispatches as a stateful precompile, at any
    // height (it falls through to the base Ethereum set).
    let unknown = Address::from([0x99u8; 20]);
    assert!(!after.contains_stateful(&unknown));
    assert!(after.dispatch_stateful(&unknown).is_none());
    assert!(!after.warm_addresses_vec().contains(&unknown));
}

#[test]
fn empty_registry_warms_nothing() {
    let registry = Arc::new(PrecompileRegistry::new());
    let p = AvaPrecompiles::for_height(registry, u64::MAX);
    assert!(p.warm_addresses_vec().is_empty());
    assert!(!p.contains_stateful(&dummy_addr()));
    assert!(p.dispatch_stateful(&dummy_addr()).is_none());
}

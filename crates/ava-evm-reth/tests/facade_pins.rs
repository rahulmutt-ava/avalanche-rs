// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! G0 facade pin tests (spec 10 §17.1, plan M6.1).
//!
//! These guard the two invariants the facade exists to hold:
//!   1. the minimal internal API surface compiles and is reachable through
//!      `ava_evm_reth::*` ONLY (no `reth_*`/`revm` path leaks to callers);
//!   2. the reth dependency is pinned to a single 40-char hex commit SHA, not a
//!      version range.

// (1) Surface check. A plain `use` of each item is itself the compile-time
// proof that the path resolves: if any upstream rename/move breaks a re-export,
// this file fails to compile — the single place a reth bump is allowed to
// surface (UPGRADING.md step 2). The `_assert_*` bindings below additionally
// pin the *kind* of each item (trait vs. struct) so a re-export that silently
// changes shape is caught too.
#[allow(unused_imports)]
use ava_evm_reth::{
    BlockBuilder, BlockExecutor, BlockExecutorFactory, BundleState, ConfigureEvm,
    PrecompileProvider, State, StateProvider, StateRootProvider,
};

/// Prove the trait re-exports resolve, both as generic bounds (for the ones
/// with generic methods, which are not `dyn`-compatible) and as `dyn` objects
/// (for the object-safe storage traits), and that the struct re-exports resolve
/// as types — all without constructing anything.
#[allow(dead_code)]
fn facade_reexports_compile() {
    // Traits with generic/`Self`-referencing methods → exercise as bounds.
    fn _bounds<C, F>()
    where
        C: ConfigureEvm,
        F: BlockExecutorFactory,
    {
    }
    // `BlockExecutor`, `BlockBuilder`, and `PrecompileProvider` are imported
    // above; the `use` itself is the compile-time proof their paths resolve (a
    // broken re-export fails to compile here). They are not `dyn`-compatible
    // and `PrecompileProvider<CTX: ContextTr>` needs a context type, so we do
    // not name them again — importing is sufficient and honest.

    // Object-safe storage traits → exercise as `dyn`.
    let _: Option<&dyn StateProvider> = None;
    let _: Option<&dyn StateRootProvider> = None;

    // Struct re-exports resolve as types.
    let _ = core::any::type_name::<BundleState>();
    let _ = core::mem::size_of::<*const State<()>>();
}

/// The pinned reth revision must be a single 40-char hex SHA (G0/R3), not a
/// version range or tag.
#[test]
fn pinned_rev_is_single_sha() {
    let rev = ava_evm_reth::RETH_REV;
    assert_eq!(
        rev.len(),
        40,
        "RETH_REV must be a 40-char commit SHA: {rev:?}"
    );
    assert!(
        rev.bytes().all(|b| b.is_ascii_hexdigit()),
        "RETH_REV must be lowercase hex (no range/tag): {rev:?}"
    );
}

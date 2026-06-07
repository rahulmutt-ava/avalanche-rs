// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Executor constants — the grandfathered operation-tx id (specs 09 §6.2).
//!
//! Port of the inline string literal in
//! `vms/avm/txs/executor/semantic_verifier.go`'s `OperationTx` visitor.

/// `GRANDFATHERED_OPERATION_TX` — a mainnet `OperationTx` whose operation
/// verification is **skipped** for backwards compatibility (specs 09 §6.2).
///
/// Go's `SemanticVerifier.OperationTx` returns early (skipping per-op fx
/// verification) when `!Bootstrapped` **or** when `v.Tx.ID().String()` equals
/// this exact CB58 id. This tx is part of the accepted chain history, so the
/// check must be reproduced byte-for-byte; the value is the literal string Go
/// compares against, parsed into an [`Id`](ava_types::id::Id) at the call site.
pub const GRANDFATHERED_OPERATION_TX: &str = "MkvpJS13eCnEYeYi9B5zuWrU9goG9RBj7nr83U7BjrFV22a12";

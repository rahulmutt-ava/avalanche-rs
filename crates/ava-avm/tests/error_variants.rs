// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.1 — assert the `ava_avm::Error` sentinel set mirrors the Go `vms/avm`
//! sentinels named in specs/09 §11, and that `FxIndex` carries the
//! VM-registration-order discriminants (specs/09 §2.2).

// This integration test exercises only the error/fx-index surface; the dev-deps
// (and crate deps) declared for the richer Wave-0 suites are unused here.
#![allow(unused_crate_dependencies)]

use ava_avm::Error;
use ava_avm::fx_index::FxIndex;

/// Every Go sentinel named in specs/09 §11 (plus the name/symbol/denomination
/// family) exists as an `Error` variant and is `matches!`-assertable, mirroring
/// the `errors.Is` posture Go uses.
#[test]
fn error_variants_exist_and_match_go_sentinels() {
    // The core §11 set.
    assert!(matches!(Error::AssetIdMismatch, Error::AssetIdMismatch));
    assert!(matches!(Error::NotAnAsset, Error::NotAnAsset));
    assert!(matches!(Error::IncompatibleFx, Error::IncompatibleFx));
    assert!(matches!(Error::UnknownFx, Error::UnknownFx));
    assert!(matches!(
        Error::WrongNumberOfCredentials,
        Error::WrongNumberOfCredentials
    ));
    assert!(matches!(Error::DoubleSpend, Error::DoubleSpend));
    assert!(matches!(Error::NoImportInputs, Error::NoImportInputs));
    assert!(matches!(Error::NoExportOutputs, Error::NoExportOutputs));

    // Name / symbol / denomination family (CreateAssetTx syntactic verify).
    assert!(matches!(Error::NameTooLong, Error::NameTooLong));
    assert!(matches!(Error::SymbolTooLong, Error::SymbolTooLong));
    assert!(matches!(
        Error::DenominationTooLarge,
        Error::DenominationTooLarge
    ));
    assert!(matches!(
        Error::IllegalNameCharacter,
        Error::IllegalNameCharacter
    ));
    assert!(matches!(
        Error::IllegalSymbolCharacter,
        Error::IllegalSymbolCharacter
    ));
    assert!(matches!(
        Error::UnexpectedWhitespace,
        Error::UnexpectedWhitespace
    ));

    // Nil / empty-collection family.
    assert!(matches!(Error::NilInitialState, Error::NilInitialState));
    assert!(matches!(Error::NilOperation, Error::NilOperation));
    assert!(matches!(Error::NoOperations, Error::NoOperations));
    assert!(matches!(Error::NoFxs, Error::NoFxs));

    // Sort / uniqueness family.
    assert!(matches!(Error::OutputsNotSorted, Error::OutputsNotSorted));
    assert!(matches!(
        Error::InitialStatesNotSortedUnique,
        Error::InitialStatesNotSortedUnique
    ));
    assert!(matches!(
        Error::OperationsNotSortedUnique,
        Error::OperationsNotSortedUnique
    ));
}

/// `FxIndex` is assigned in VM-registration order and is on-disk-meaningful
/// (it appears in `CreateAssetTx::InitialState.fx_index`), so the discriminants
/// are protocol constants (specs/09 §2.2).
#[test]
fn fx_index_repr() {
    assert_eq!(FxIndex::Secp256k1 as u32, 0);
    assert_eq!(FxIndex::Nft as u32, 1);
    assert_eq!(FxIndex::Property as u32, 2);
}

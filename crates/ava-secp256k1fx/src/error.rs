// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-secp256k1fx` error model (specs 07 §9).
//!
//! Most fx sentinels (`ErrTimelocked`, `ErrTooManySigners`, `ErrWrongSig`, the
//! `OutputOwners::Verify` set, the wrong-type set, …) are already defined on the
//! shared [`ava_vm::Error`] enum and are re-exported here so the fx framework
//! (M3.20) and downstream VMs see one error type. The syntactic-validation
//! sentinels that have no `ava-vm` analog (`ErrNilInput`, `ErrNoValueInput`,
//! `ErrNoValueOutput`, `ErrInputIndicesNotSortedUnique`, `ErrNilCredential`) are
//! carried as a small local enum and folded back into the shared error via
//! [`Error::InvalidComponent`] so a single `Result` flows through `verify::all`.

/// Re-exported shared error enum — the canonical fx error type (specs 07 §9).
pub use ava_vm::error::Error;
/// Re-exported shared `Result` alias.
pub use ava_vm::error::Result;

/// `Input.Verify` — `ErrNilInput` has no Rust analog (a `&Input` is never nil),
/// so the syntactic-validation messages are surfaced as
/// [`Error::InvalidComponent`] with the Go sentinel text.
pub(crate) const ERR_INPUT_INDICES_NOT_SORTED_UNIQUE: &str =
    "address indices not sorted and unique";
/// `TransferInput.Verify` — `ErrNoValueInput`.
pub(crate) const ERR_NO_VALUE_INPUT: &str = "input has no value";
/// `TransferOutput.Verify` — `ErrNoValueOutput`.
pub(crate) const ERR_NO_VALUE_OUTPUT: &str = "output has no value";

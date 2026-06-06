// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Decidable status (specs 06 §3.1; Go `snow/choices/status.go`).

/// The wire/persisted status of a decidable container.
///
/// The discriminant values are part of the wire/persisted encoding and **must**
/// match Go's `choices.Status` constants exactly: `Unknown=0`, `Processing=1`,
/// `Rejected=2`, `Accepted=3`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Status {
    /// The container's status is unknown (Go `Unknown`).
    Unknown = 0,
    /// The container is being processed (Go `Processing`).
    Processing = 1,
    /// The container has been rejected (Go `Rejected`).
    Rejected = 2,
    /// The container has been accepted (Go `Accepted`).
    Accepted = 3,
}

impl Status {
    /// Reports whether this status is one of the terminal decided states
    /// (`Accepted` or `Rejected`), matching Go `Status.Decided`.
    #[must_use]
    pub fn decided(self) -> bool {
        matches!(self, Status::Accepted | Status::Rejected)
    }

    /// Reports whether this status is a valid (known) value, matching Go
    /// `Status.Valid` (every value except `Unknown`).
    #[must_use]
    pub fn valid(self) -> bool {
        !matches!(self, Status::Unknown)
    }
}

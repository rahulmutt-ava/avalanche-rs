// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.Priority` — the staker tie-break ordering (`txs/priorities.go`, specs
//! 08 §3.3).
//!
//! [`Priority`] is a `#[repr(u8)]` enum whose discriminants are
//! **protocol-load-bearing**: they break ties between stakers that share a
//! `NextTime` and pin the time-advancement promotion/removal order. The values
//! `1..=11` reproduce the exact order of the Go `iota`-based constants — the
//! pending group first (`1..=6`), then the current group (`7..=11`).
//!
//! Invariant (from `priorities.go`): all permissioned subnet stakers are removed
//! first (priority `7`) because they are removed by the advancement of time;
//! permissionless stakers are removed by a `RewardValidatorTx` after time has
//! advanced.

/// `txs.Priority` — orders stakers that share a `NextTime`.
///
/// The discriminants match the Go `iota + 1` constants in `priorities.go`
/// (`1..=11`) and are wire/ordering-significant. Lower values sort first under
/// the [`Ord`] derive, mirroring Go `Priority < than.Priority`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// `PrimaryNetworkDelegatorApricotPendingPriority` (1).
    PrimaryNetworkDelegatorApricotPending = 1,
    /// `PrimaryNetworkValidatorPendingPriority` (2).
    PrimaryNetworkValidatorPending = 2,
    /// `PrimaryNetworkDelegatorBanffPendingPriority` (3).
    PrimaryNetworkDelegatorBanffPending = 3,
    /// `SubnetPermissionlessValidatorPendingPriority` (4).
    SubnetPermissionlessValidatorPending = 4,
    /// `SubnetPermissionlessDelegatorPendingPriority` (5).
    SubnetPermissionlessDelegatorPending = 5,
    /// `SubnetPermissionedValidatorPendingPriority` (6).
    SubnetPermissionedValidatorPending = 6,
    /// `SubnetPermissionedValidatorCurrentPriority` (7).
    SubnetPermissionedValidatorCurrent = 7,
    /// `SubnetPermissionlessDelegatorCurrentPriority` (8).
    SubnetPermissionlessDelegatorCurrent = 8,
    /// `SubnetPermissionlessValidatorCurrentPriority` (9).
    SubnetPermissionlessValidatorCurrent = 9,
    /// `PrimaryNetworkDelegatorCurrentPriority` (10).
    PrimaryNetworkDelegatorCurrent = 10,
    /// `PrimaryNetworkValidatorCurrentPriority` (11).
    PrimaryNetworkValidatorCurrent = 11,
}

impl Priority {
    /// The raw `u8` discriminant (the protocol byte value).
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// `Priority.IsCurrent` — in the current validator set.
    #[must_use]
    pub const fn is_current(self) -> bool {
        self.is_current_validator() || self.is_current_delegator()
    }

    /// `Priority.IsPending` — in the pending validator set.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        self.is_pending_validator() || self.is_pending_delegator()
    }

    /// `Priority.IsValidator` — a (current or pending) validator.
    #[must_use]
    pub const fn is_validator(self) -> bool {
        self.is_current_validator() || self.is_pending_validator()
    }

    /// `Priority.IsDelegator` — a (current or pending) delegator.
    #[must_use]
    pub const fn is_delegator(self) -> bool {
        self.is_current_delegator() || self.is_pending_delegator()
    }

    /// `Priority.IsPermissionedValidator` — a permissioned subnet validator.
    #[must_use]
    pub const fn is_permissioned_validator(self) -> bool {
        matches!(
            self,
            Priority::SubnetPermissionedValidatorCurrent
                | Priority::SubnetPermissionedValidatorPending
        )
    }

    /// `Priority.IsCurrentValidator`.
    #[must_use]
    pub const fn is_current_validator(self) -> bool {
        matches!(
            self,
            Priority::PrimaryNetworkValidatorCurrent
                | Priority::SubnetPermissionedValidatorCurrent
                | Priority::SubnetPermissionlessValidatorCurrent
        )
    }

    /// `Priority.IsCurrentDelegator`.
    #[must_use]
    pub const fn is_current_delegator(self) -> bool {
        matches!(
            self,
            Priority::PrimaryNetworkDelegatorCurrent
                | Priority::SubnetPermissionlessDelegatorCurrent
        )
    }

    /// `Priority.IsPendingValidator`.
    #[must_use]
    pub const fn is_pending_validator(self) -> bool {
        matches!(
            self,
            Priority::PrimaryNetworkValidatorPending
                | Priority::SubnetPermissionedValidatorPending
                | Priority::SubnetPermissionlessValidatorPending
        )
    }

    /// `Priority.IsPendingDelegator`.
    #[must_use]
    pub const fn is_pending_delegator(self) -> bool {
        matches!(
            self,
            Priority::PrimaryNetworkDelegatorBanffPending
                | Priority::PrimaryNetworkDelegatorApricotPending
                | Priority::SubnetPermissionlessDelegatorPending
        )
    }
}

#[cfg(test)]
mod golden {
    //! `priority_discriminants` — pins the 11 protocol byte values.
    //!
    //! Provenance: the discriminant order is ported verbatim from the Go
    //! `iota`-based constants in `vms/platformvm/txs/priorities.go` (pending
    //! group `1..=6`, then current group `7..=11`). The values are wire/ordering
    //! significant, so this golden locks them down.

    use super::*;

    #[test]
    fn priority_discriminants() {
        // Pending group, in `iota + 1` order.
        assert_eq!(Priority::PrimaryNetworkDelegatorApricotPending.as_u8(), 1);
        assert_eq!(Priority::PrimaryNetworkValidatorPending.as_u8(), 2);
        assert_eq!(Priority::PrimaryNetworkDelegatorBanffPending.as_u8(), 3);
        assert_eq!(Priority::SubnetPermissionlessValidatorPending.as_u8(), 4);
        assert_eq!(Priority::SubnetPermissionlessDelegatorPending.as_u8(), 5);
        assert_eq!(Priority::SubnetPermissionedValidatorPending.as_u8(), 6);
        // Current group, continuing the `iota`.
        assert_eq!(Priority::SubnetPermissionedValidatorCurrent.as_u8(), 7);
        assert_eq!(Priority::SubnetPermissionlessDelegatorCurrent.as_u8(), 8);
        assert_eq!(Priority::SubnetPermissionlessValidatorCurrent.as_u8(), 9);
        assert_eq!(Priority::PrimaryNetworkDelegatorCurrent.as_u8(), 10);
        assert_eq!(Priority::PrimaryNetworkValidatorCurrent.as_u8(), 11);
    }

    #[test]
    fn priority_predicates() {
        // Pending vs current partition.
        assert!(Priority::PrimaryNetworkValidatorPending.is_pending());
        assert!(!Priority::PrimaryNetworkValidatorPending.is_current());
        assert!(Priority::PrimaryNetworkValidatorCurrent.is_current());
        assert!(!Priority::PrimaryNetworkValidatorCurrent.is_pending());

        // Validator vs delegator partition.
        assert!(Priority::SubnetPermissionlessValidatorCurrent.is_validator());
        assert!(!Priority::SubnetPermissionlessValidatorCurrent.is_delegator());
        assert!(Priority::PrimaryNetworkDelegatorBanffPending.is_delegator());
        assert!(!Priority::PrimaryNetworkDelegatorBanffPending.is_validator());

        // Permissioned validators (the time-removed group).
        assert!(Priority::SubnetPermissionedValidatorCurrent.is_permissioned_validator());
        assert!(Priority::SubnetPermissionedValidatorPending.is_permissioned_validator());
        assert!(!Priority::SubnetPermissionlessValidatorCurrent.is_permissioned_validator());
    }
}

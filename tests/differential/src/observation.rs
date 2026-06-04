// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Normalized cross-implementation observation (specs/02 §11.3/§11.4).

/// A normalized snapshot of node state, comparable across implementations.
///
/// SCAFFOLD: per-subsystem collectors (block IDs/heights, state/merkle roots,
/// normalized API JSON, metrics schema) are added as each subsystem lands
/// (specs/02 §11.3 table). [`Observation::normalized`] strips timestamps, sorts
/// collections, and masks per-instance IDs so two correct implementations
/// compare equal. Filled in by tier-X task X.13.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    /// Finalized block IDs/heights, state roots, normalized API responses, etc.
    /// Represented opaquely until the collectors land.
    pub fields: Vec<(String, String)>,
}

impl Observation {
    /// Return a normalized copy: collections sorted, timestamps stripped,
    /// per-instance IDs masked (specs/02 §11.4).
    #[must_use]
    pub fn normalized(&self) -> Observation {
        let mut fields = self.fields.clone();
        fields.sort();
        Observation { fields }
    }
}

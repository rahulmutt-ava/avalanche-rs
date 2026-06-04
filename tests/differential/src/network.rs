// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Two-binary / mixed-net wiring (specs/02 §11.6, §10.2).

/// Which node implementation a network slot runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    /// The reference avalanchego binary.
    Go,
    /// The avalanchers binary under test.
    Rust,
}

/// Deterministic network configuration: identical genesis/config/seed across
/// implementations, with the i-th Go and i-th Rust node assigned the same
/// seed-derived node IDs / TLS certs (specs/02 §11.4).
///
/// SCAFFOLD: tmpnet integration + the mixed Go↔Rust interop scenario land in
/// tier-X task X.15 (first lands M2).
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Seed driving all deterministic node identity derivation.
    pub seed: u64,
    /// Number of nodes per implementation.
    pub nodes: u32,
}

impl NetworkConfig {
    /// Build a deterministic config for `nodes` validators from `seed`.
    #[must_use]
    pub fn deterministic(seed: u64, nodes: u32) -> Self {
        Self { seed, nodes }
    }
}

// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Continuity / **no-fork** assertions across the cut-over (specs/02 §10.4,
//! §11.3) and the **moving min-compatible floor** (specs/26 §7).
//!
//! Two pure surfaces, both exercised by the offline CI arm:
//!
//! 1. [`assert_no_fork`] — given the per-node, per-cut-over sequence of
//!    [`Observation`]s (each a node's normalized finalized state), assert that
//!    every node — Go (pre-swap) and Rust (post-swap) alike — agrees on the
//!    last-accepted block ID + height and the state/merkle root for every chain
//!    at each step. A genuine divergence (a fork) is detected and reported.
//!
//! 2. [`MovingFloor`] — the specs/26 §7 rolling-upgrade compatibility model:
//!    while nodes are rolled from the previous Go release onto the new Rust
//!    build, the network's minimum-compatible floor moves once the activation
//!    time passes, but Go and Rust peers MUST stay mutually compatible
//!    throughout the roll. Backed by the REAL [`ava_version::Compatibility`]
//!    checker.

use std::time::SystemTime;

use ava_differential::Observation;
use ava_version::application::Application;
use ava_version::compatibility::{Compatibility, MockClock};

/// A no-fork violation: two nodes disagree on a per-chain finalized field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "fork detected at step {step}: nodes {reference_node} and {divergent_node} \
     disagree on `{field}` ({reference_value:?} != {divergent_value:?})"
)]
pub struct ForkError {
    /// The cut-over step (0 = all-Go, increasing as nodes are rolled to Rust).
    pub step: usize,
    /// The node index taken as the reference for this step.
    pub reference_node: usize,
    /// The node index whose observation diverged.
    pub divergent_node: usize,
    /// The normalized field name that diverged (e.g. `P/last_accepted_id`).
    pub field: String,
    /// The reference node's value for that field.
    pub reference_value: String,
    /// The divergent node's value for that field.
    pub divergent_value: String,
}

/// A single step of the roll: every node's [`Observation`] of the same finalized
/// state, taken after `swapped` of the N nodes have been rolled onto Rust.
///
/// All observations at one step must agree (no fork). Across steps the height
/// advances; within a step the nodes are compared field-by-field.
#[derive(Debug, Clone)]
pub struct CutoverStep {
    /// How many nodes have been rolled onto Rust by this step.
    pub swapped: usize,
    /// One observation per node, in node-index order.
    pub observations: Vec<Observation>,
}

impl CutoverStep {
    /// Build a cut-over step from `swapped` count and per-node observations.
    #[must_use]
    pub fn new(swapped: usize, observations: Vec<Observation>) -> Self {
        Self {
            swapped,
            observations,
        }
    }
}

/// Assert the **no-fork** invariant across an entire roll: at every step, every
/// node's normalized observation agrees with node 0's on every shared field.
///
/// This is the load-bearing continuity check (specs/02 §10.4): the chain must be
/// continuous and unforked across the Go→Rust cut-over — a Rust node that has
/// just imported a Go data dir must observe exactly the same last-accepted block
/// ID/height and state/merkle root as the Go nodes it joined.
///
/// Comparison is over [`Observation::normalized`], so expected non-determinism
/// (timestamps, per-instance IDs, collection order) never masquerades as a fork
/// and never hides one.
///
/// # Errors
///
/// Returns the first [`ForkError`] found: a step where two nodes disagree on a
/// shared normalized field (different last-accepted ID/height or root = a fork).
pub fn assert_no_fork(steps: &[CutoverStep]) -> Result<(), ForkError> {
    for (step_idx, step) in steps.iter().enumerate() {
        let Some((first, rest)) = step.observations.split_first() else {
            continue; // no nodes at this step — vacuously consistent.
        };
        let reference = first.normalized();
        for (offset, other) in rest.iter().enumerate() {
            let divergent_node = offset.saturating_add(1);
            let other_norm = other.normalized();
            // Compare every field the reference carries; a missing or differing
            // value on a shared field is a fork.
            for (key, ref_val) in &reference.fields {
                let other_val = other_norm
                    .fields
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone());
                if other_val.as_deref() != Some(ref_val.as_str()) {
                    return Err(ForkError {
                        step: step_idx,
                        reference_node: 0,
                        divergent_node,
                        field: key.clone(),
                        reference_value: ref_val.clone(),
                        divergent_value: other_val.unwrap_or_default(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// The rolling-upgrade **moving min-compatible floor** (specs/26 §7).
///
/// During a roll the network has two peer versions in flight: the previous Go
/// release (`previous`) and the new Rust build (`current`, the version Rust
/// reports — `ava-version`'s pinned `CURRENT`). The minimum-compatible floor a
/// node accepts moves from `min_compatible` (before the activation time) to
/// `min_compatible_after_upgrade` (after it). The §7 invariant is: throughout
/// the roll, a node of either implementation accepts a peer of the other —
/// otherwise the partially-rolled network would split.
///
/// This wraps the REAL [`ava_version::Compatibility`] checker with a mock clock
/// so the offline arm can drive the floor across the activation boundary
/// deterministically.
#[derive(Debug, Clone)]
pub struct MovingFloor {
    /// The new build's reported version (Rust `avalanchers`, == Go `CURRENT`).
    pub current: Application,
    /// The previous released peer version still on the network during the roll.
    pub previous: Application,
    /// The floor before the activation time (`PREV_MINIMUM_COMPATIBLE`).
    pub min_compatible: Application,
    /// The floor after the activation time (`MINIMUM_COMPATIBLE`).
    pub min_compatible_after_upgrade: Application,
    /// The activation time that moves the floor.
    pub activation: SystemTime,
}

impl MovingFloor {
    /// Build the moving floor from `ava-version`'s pinned constants
    /// (`CURRENT` / `MINIMUM_COMPATIBLE` / `PREV_MINIMUM_COMPATIBLE`) and the
    /// previous released peer version still on the wire during the roll.
    #[must_use]
    pub fn from_constants(previous: Application, activation: SystemTime) -> Self {
        Self {
            current: ava_version::application::CURRENT.clone(),
            previous,
            min_compatible: ava_version::application::PREV_MINIMUM_COMPATIBLE.clone(),
            min_compatible_after_upgrade: ava_version::application::MINIMUM_COMPATIBLE.clone(),
            activation,
        }
    }

    /// A [`Compatibility`] checker (real Go-mirrored logic) fixed at `at`.
    fn checker_at(&self, at: SystemTime) -> Compatibility<MockClock> {
        Compatibility::with_clock(
            self.current.clone(),
            self.min_compatible_after_upgrade.clone(),
            self.min_compatible.clone(),
            self.activation,
            MockClock::new(at),
        )
    }

    /// Whether a node accepts the `previous` (Go) peer at time `at` — i.e. the
    /// rolled-to-Rust node still talks to a not-yet-rolled Go node.
    #[must_use]
    pub fn accepts_previous(&self, at: SystemTime) -> bool {
        self.checker_at(at).compatible(&self.previous)
    }

    /// Whether a node accepts the `current` (Rust) peer at time `at` — i.e. a
    /// still-Go node still talks to an already-rolled Rust node.
    #[must_use]
    pub fn accepts_current(&self, at: SystemTime) -> bool {
        self.checker_at(at).compatible(&self.current)
    }

    /// The specs/26 §7 invariant: at time `at` (before OR after the activation),
    /// Go and Rust peers stay **mutually** compatible, so a partially-rolled
    /// network never splits.
    #[must_use]
    pub fn peers_stay_connected(&self, at: SystemTime) -> bool {
        self.accepts_previous(at) && self.accepts_current(at)
    }
}

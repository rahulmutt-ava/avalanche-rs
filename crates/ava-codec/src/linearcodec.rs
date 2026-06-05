// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Linear codec — sequential `u32` typeID registry + the [`Codec`] adapter.
//!
//! Port of Go's `codec/linearcodec/`. Go assigns sequential `u32` typeIDs in
//! **registration order** from `0`, with `SkipRegistrations(n)` reserving gaps.
//! Interface fields encode as `pack_u32(typeID)` + the concrete value.
//!
//! In Rust we model each Go interface as an enum whose `#[codec(type_id = N)]`
//! variants pin those typeIDs at the wire level (the encoding lives in the
//! derive-generated [`Serializable`] impl, see `specs/03` §2.3). This module
//! provides:
//!
//! - [`LinearCodec`] — the [`Codec`] adapter the [`crate::manager::Manager`]
//!   stores per version. It delegates encoding to the value's streaming impls
//!   and translates packer errors into [`CodecError`]s.
//! - [`TypeIdRegistry`] — a registration-order typeID assigner used purely to
//!   **assert** that an enum's `#[codec(type_id = N)]` annotations match the Go
//!   registration order (the golden typeID-table test).

use crate::error::{CodecError, Result};
use crate::manager::{Codec, map_packer_error};
use crate::packer::Packer;
use crate::{Deserializable, Serializable};

/// The [`Codec`] adapter for the linear codec.
///
/// Stateless: the per-type typeID wiring is baked into the derived enums. A
/// single instance can be shared (via `Arc`) across all versions that use the
/// same registry, or one per version for clarity.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinearCodec;

impl LinearCodec {
    /// Creates a new linear codec.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Codec for LinearCodec {
    fn marshal_into(&self, value: &dyn Serializable, p: &mut Packer) -> Result<()> {
        value.marshal_into(p);
        match p.error() {
            Some(err) => Err(map_packer_error(err)),
            None => Ok(()),
        }
    }

    fn unmarshal_from(&self, p: &mut Packer, dst: &mut dyn Deserializable) -> Result<()> {
        dst.unmarshal_from(p);
        match p.error() {
            Some(err) => Err(map_packer_error(err)),
            None => Ok(()),
        }
    }

    fn size(&self, value: &dyn Serializable) -> Result<usize> {
        Ok(value.size())
    }
}

/// A registration-order typeID assigner mirroring Go's `linearCodec` counter.
///
/// Used in tests to reproduce the Go typeID assignment and assert it against an
/// enum's `#[codec(type_id = N)]` annotations (the golden typeID table). It does
/// **not** participate in encoding — that is fixed by the derive macro.
#[derive(Debug, Default, Clone)]
pub struct TypeIdRegistry {
    next: u32,
    assigned: Vec<(String, u32)>,
}

impl TypeIdRegistry {
    /// Creates an empty registry (next typeID = 0).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a named type, assigning it the next sequential typeID. Errors
    /// with [`CodecError::DuplicateType`] on a repeated name.
    pub fn register(&mut self, name: &str) -> Result<u32> {
        if self.assigned.iter().any(|(n, _)| n == name) {
            return Err(CodecError::DuplicateType);
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(CodecError::MaxSliceLenExceeded)?;
        self.assigned.push((name.to_string(), id));
        Ok(id)
    }

    /// Reserves `n` typeIDs (Go `SkipRegistrations(n)`), bumping the counter.
    pub fn skip_registrations(&mut self, n: u32) -> Result<()> {
        self.next = self
            .next
            .checked_add(n)
            .ok_or(CodecError::MaxSliceLenExceeded)?;
        Ok(())
    }

    /// The typeID that would be assigned to the next registration.
    #[must_use]
    pub fn next_id(&self) -> u32 {
        self.next
    }

    /// The full `(name, type_id)` table in registration order.
    #[must_use]
    pub fn table(&self) -> &[(String, u32)] {
        &self.assigned
    }
}

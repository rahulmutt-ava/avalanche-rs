// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Linear codec — sequential `u32` typeID registry.
//!
//! TODO(M0.16): implement the linearcodec assigning sequential `u32` typeIDs in
//! registration order from 0, with `skip_registrations(n)`; interfaces encode
//! as `pack_u32(typeID)` + value, modeled via the derive `#[codec(type_registry)]`
//! enums. A golden test asserts typeIDs against a Go-dumped table.
//! Owning spec: `specs/03-core-primitives.md` §2.3.

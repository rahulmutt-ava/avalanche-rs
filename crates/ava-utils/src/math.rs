// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Checked arithmetic (`safemath`) — never silent-wrap (determinism hazard #3).
//!
//! TODO(M0.9): generic checked `add`/`sub`/`mul` -> `Error::{Overflow,Underflow}`,
//! `abs_diff`, `max_uint::<T>()`.
//! Owning spec: `specs/03-core-primitives.md` §4.3, `specs/00` §6.1.

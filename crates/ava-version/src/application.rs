// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Application` version + the pinned client version constants.
//!
//! TODO(M0.22): `Application { name, major, minor, patch }` + `display`
//! (`"avalanchego/<maj>.<min>.<patch>"`) / `semantic` (`"v..."`) / `compare`
//! (major->minor->patch). Constants `CLIENT="avalanchego"`,
//! `RPC_CHAIN_VM_PROTOCOL=45`, `CURRENT_DATABASE`, `CURRENT`,
//! `MINIMUM_COMPATIBLE`, `PREV_MINIMUM_COMPATIBLE` (pin to the Go tree at port
//! time; doc-comment the Go path).
//! Owning spec: `specs/03-core-primitives.md` §5.1.

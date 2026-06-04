// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generic codec conformance suite (the Go `codectest.RunAll` analogue).
//!
//! Gated behind `cfg(test)` or the `testutil` feature so downstream crates can
//! re-run the contract against their own registered types.
//!
//! TODO(M0.16): implement `run_codec_suite()` exercising round-trip + the
//! negative cases (`ExtraSpace`, `MaxSliceLenExceeded`, bad bool, unsorted map
//! keys, unknown typeID, unknown version).
//! Owning spec: `specs/02-testing-strategy.md` §7.
